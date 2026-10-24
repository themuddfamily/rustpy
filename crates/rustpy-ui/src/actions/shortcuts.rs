//! Extended shortcut grammar. The serialized strings live in the same overrides
//! as ordinary chords, so copying, resetting and importing never lose them.
use super::{Action, KeyChord, Keymap};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shortcut {
    Sequence(Vec<KeyChord>),
    Shared(KeyChord),
    PhysicalControl(KeyChord),
    ControlSequence(Vec<KeyChord>),
    Mouse {
        button: String,
        ctrl: bool,
        alt: bool,
        shift: bool,
    },
    Abbreviation(String),
}
impl Shortcut {
    pub fn parse(text: &str) -> Option<Self> {
        if let Some(chord) = text.strip_prefix("Shared:") {
            return KeyChord::parse(chord).map(Self::Shared);
        }
        if text.split('+').any(|p| p == "Control") && !text.contains(',') {
            return KeyChord::parse(text).map(Self::PhysicalControl);
        }
        if let Some(word) = text.strip_prefix("Abbreviation:") {
            let word = word.trim();
            return (!word.is_empty()
                && word.len() <= 80
                && !word.chars().any(char::is_whitespace))
            .then(|| Self::Abbreviation(word.to_owned()));
        }
        if let Some(mouse) = text.strip_prefix("Mouse:") {
            let mut ctrl = false;
            let mut alt = false;
            let mut shift = false;
            let mut button = None;
            for part in mouse.split('+') {
                match part {
                    "Ctrl" | "Cmd" => ctrl = true,
                    "Alt" => alt = true,
                    "Shift" => shift = true,
                    "Primary" | "PrimaryDouble" | "Middle" | "Secondary" | "Extra1" | "Extra2"
                    | "WheelUp" | "WheelDown"
                        if button.is_none() =>
                    {
                        button = Some(part.to_owned())
                    }
                    _ => return None,
                }
            }
            return Some(Self::Mouse {
                button: button?,
                ctrl,
                alt,
                shift,
            });
        }
        let strokes: Option<Vec<_>> = text.split(',').map(|s| KeyChord::parse(s.trim())).collect();
        let strokes = strokes?;
        if strokes.len() != 2 {
            return None;
        }
        if text
            .split(',')
            .all(|s| s.trim().split('+').any(|p| p == "Control"))
        {
            Some(Self::ControlSequence(strokes))
        } else if text.contains("Control+") {
            None
        } else {
            Some(Self::Sequence(strokes))
        }
    }
    pub fn text(&self) -> String {
        match self {
            Self::Shared(chord) => format!("Shared:{chord}"),
            Self::PhysicalControl(chord) => chord.to_string().replacen("Ctrl+", "Control+", 1),
            Self::ControlSequence(keys) => keys
                .iter()
                .map(|c| c.to_string().replacen("Ctrl+", "Control+", 1))
                .collect::<Vec<_>>()
                .join(", "),
            Self::Sequence(keys) => keys
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            Self::Mouse {
                button,
                ctrl,
                alt,
                shift,
            } => format!(
                "Mouse:{}{}{}{button}",
                if *ctrl { "Ctrl+" } else { "" },
                if *alt { "Alt+" } else { "" },
                if *shift { "Shift+" } else { "" }
            ),
            Self::Abbreviation(word) => format!("Abbreviation:{word}"),
        }
    }
}
impl Keymap {
    pub fn primary_modifiers(mods: egui::Modifiers) -> bool {
        !cfg!(target_os = "macos") || !mods.ctrl || mods.mac_cmd
    }
    pub fn prefix_actions(&self, chord: KeyChord, physical: bool) -> Vec<Action> {
        self.extended
            .iter()
            .filter_map(|(a, s)| match s {
                Shortcut::Sequence(keys) if !physical && keys.first() == Some(&chord) => Some(*a),
                Shortcut::ControlSequence(keys) if physical && keys.first() == Some(&chord) => {
                    Some(*a)
                }
                _ => None,
            })
            .collect()
    }
    pub fn linux_global_conflict(text: &str) -> bool {
        text.split(',').any(|s| {
            let Some(chord) = KeyChord::parse(s.trim()) else {
                return s.contains("Super+");
            };
            (chord.alt
                && !chord.ctrl
                && matches!(chord.key, egui::Key::F1 | egui::Key::F2 | egui::Key::F4))
                || (chord.ctrl
                    && chord.alt
                    && matches!(
                        chord.key,
                        egui::Key::ArrowLeft
                            | egui::Key::ArrowRight
                            | egui::Key::ArrowUp
                            | egui::Key::ArrowDown
                            | egui::Key::Delete
                    ))
        })
    }
    pub fn shortcut_definitions(&self, action: Action) -> Vec<String> {
        self.shortcuts(action)
            .iter()
            .map(ToString::to_string)
            .chain(
                self.extended
                    .iter()
                    .filter(|(a, _)| *a == action)
                    .map(|(_, s)| s.text()),
            )
            .collect()
    }
    /// Includes sequence prefixes, so extension and macro bindings cannot steal
    /// a first stroke that the IDE's dispatcher owns.
    pub fn chord_owner(&self, chord: KeyChord) -> Option<Action> {
        self.lookup(chord).or_else(|| {
            self.extended.iter().find_map(|(a, s)| match s {
                Shortcut::Sequence(keys) if keys.first() == Some(&chord) => Some(*a),
                Shortcut::ControlSequence(keys)
                    if !cfg!(target_os = "macos") && keys.first() == Some(&chord) =>
                {
                    Some(*a)
                }
                Shortcut::Shared(c) if *c == chord => Some(*a),
                Shortcut::PhysicalControl(c) if !cfg!(target_os = "macos") && *c == chord => {
                    Some(*a)
                }
                _ => None,
            })
        })
    }
    pub fn remove_definition(&mut self, action: Action, text: &str) {
        if let Some(shortcut) = Shortcut::parse(text) {
            self.extended
                .retain(|(a, s)| *a != action || *s != shortcut);
        } else if let Some(chord) = KeyChord::parse(text) {
            if self.lookup(chord) == Some(action) {
                self.bindings.remove(&chord);
            }
            if let Some(keys) = self.by_action.get_mut(&action) {
                keys.retain(|c| *c != chord);
            }
        }
    }
    pub fn bind_definition(
        &mut self,
        action: Action,
        text: &str,
        leave_conflicts: bool,
    ) -> Result<(), String> {
        if let Some(shortcut) = Shortcut::parse(text) {
            let prefix = match &shortcut {
                Shortcut::Sequence(keys) => keys.first().copied(),
                Shortcut::ControlSequence(keys) if !cfg!(target_os = "macos") => {
                    keys.first().copied()
                }
                Shortcut::PhysicalControl(chord) if !cfg!(target_os = "macos") => Some(*chord),
                _ => None,
            };
            if !leave_conflicts {
                let control_prefix = match &shortcut {
                    Shortcut::ControlSequence(keys) => keys.first().copied(),
                    Shortcut::Sequence(keys) if !cfg!(target_os = "macos") => keys.first().copied(),
                    _ => None,
                };
                if let Some(chord) = control_prefix {
                    self.extended
                        .retain(|(_, s)| !matches!(s,Shortcut::PhysicalControl(c) if *c == chord));
                }
                if let Some(prefix) = prefix {
                    if let Some(owner) = self.lookup(prefix) {
                        self.remove_definition(owner, &prefix.to_string());
                    }
                    self.extended
                        .retain(|(_, s)| !matches!(s,Shortcut::Shared(c) if *c == prefix));
                }
            }
            if !leave_conflicts && !matches!(shortcut, Shortcut::Shared(_)) {
                self.extended.retain(|(_, s)| *s != shortcut);
            }
            if !self.extended.contains(&(action, shortcut.clone())) {
                self.extended.push((action, shortcut));
            }
            return Ok(());
        }
        let chord = KeyChord::parse(text).ok_or_else(|| format!("Invalid shortcut: {text}"))?;
        if leave_conflicts && self.conflict(chord, action).is_some() {
            let shortcut = Shortcut::Shared(chord);
            if !self.extended.contains(&(action, shortcut.clone())) {
                self.extended.push((action, shortcut));
            }
            return Ok(());
        }
        if !leave_conflicts {
            self.extended
                .retain(|(_, s)| !matches!(s,Shortcut::Shared(c) if *c == chord));
        }
        if !self.shortcuts(action).contains(&chord) {
            self.bind(chord, action);
        }
        Ok(())
    }
    pub fn definition_conflicts(&self, action: Action, text: &str) -> Vec<Action> {
        let shortcut = Shortcut::parse(text);
        let mut owners: Vec<_> = self
            .extended
            .iter()
            .filter_map(|(a, s)| {
                if shortcut.as_ref() == Some(s) {
                    Some(*a)
                } else {
                    None
                }
            })
            .collect();
        let control_prefix = match &shortcut {
            Some(Shortcut::ControlSequence(keys)) => keys.first().copied(),
            Some(Shortcut::Sequence(keys)) if !cfg!(target_os = "macos") => keys.first().copied(),
            _ => None,
        };
        if let Some(chord) = control_prefix {
            owners.extend(self.extended.iter().filter_map(|(a, s)| {
                matches!(s,Shortcut::PhysicalControl(c) if *c == chord).then_some(*a)
            }));
        }
        let prefix = match &shortcut {
            Some(Shortcut::Sequence(keys)) => keys.first().copied(),
            Some(Shortcut::ControlSequence(keys)) if !cfg!(target_os = "macos") => {
                keys.first().copied()
            }
            Some(Shortcut::PhysicalControl(chord)) if !cfg!(target_os = "macos") => Some(*chord),
            None => KeyChord::parse(text),
            _ => None,
        };
        if let Some(chord) = prefix {
            if let Some(owner) = self.lookup(chord) {
                owners.push(owner);
            }
            if shortcut.is_none() {
                for (owner, s) in &self.extended {
                    if matches!(s,Shortcut::Shared(c) if *c == chord)
                        || matches!(s,Shortcut::Sequence(keys) if keys.first() == Some(&chord))
                        || (!cfg!(target_os = "macos")
                            && matches!(s,Shortcut::ControlSequence(keys) if keys.first() == Some(&chord)))
                        || (!cfg!(target_os = "macos")
                            && matches!(s,Shortcut::PhysicalControl(c) if *c == chord))
                    {
                        owners.push(*owner);
                    }
                }
            }
        }
        owners.retain(|owner| *owner != action);
        owners.sort();
        owners.dedup();
        owners
    }
    pub fn shared_actions(&self, chord: KeyChord) -> Vec<Action> {
        let mut actions: Vec<_> = self
            .extended
            .iter()
            .filter_map(|(a, s)| matches!(s,Shortcut::Shared(c) if *c == chord).then_some(*a))
            .collect();
        if !actions.is_empty() {
            if let Some(a) = self.lookup(chord) {
                actions.push(a);
            }
        }
        actions.sort();
        actions.dedup();
        actions
    }
    pub fn physical_control_candidates(
        &self,
        key: egui::Key,
        mods: egui::Modifiers,
    ) -> Vec<Action> {
        if !mods.ctrl || mods.mac_cmd {
            return Vec::new();
        }
        let chord = KeyChord {
            key,
            ctrl: true,
            alt: mods.alt,
            shift: mods.shift,
        };
        let mut candidates: Vec<_> = self
            .extended
            .iter()
            .filter_map(|(a, s)| {
                matches!(s,Shortcut::PhysicalControl(c) if *c == chord).then_some(*a)
            })
            .collect();
        if !candidates.is_empty() && !cfg!(target_os = "macos") {
            if let Some(owner) = self.lookup(chord) {
                candidates.push(owner);
            }
        }
        candidates.sort();
        candidates.dedup();
        candidates
    }
    pub fn physical_control_action(&self, key: egui::Key, mods: egui::Modifiers) -> Option<Action> {
        if !mods.ctrl || mods.mac_cmd {
            return None;
        }
        let chord = KeyChord {
            key,
            ctrl: true,
            alt: mods.alt,
            shift: mods.shift,
        };
        self.extended.iter().find_map(|(a, s)| {
            matches!(s,Shortcut::PhysicalControl(c) if *c == chord).then_some(*a)
        })
    }
    pub fn control_sequence_prefix(&self, chord: KeyChord) -> bool {
        self.extended.iter().any(
            |(_, s)| matches!(s,Shortcut::ControlSequence(keys) if keys.first() == Some(&chord)),
        )
    }
    pub fn control_sequence_action(&self, first: KeyChord, second: KeyChord) -> Option<Action> {
        self.extended.iter().find_map(|(a, s)| {
            matches!(s,Shortcut::ControlSequence(keys) if keys.as_slice() == [first,second])
                .then_some(*a)
        })
    }
    pub fn sequence_candidates(
        &self,
        first: KeyChord,
        second: KeyChord,
        physical: bool,
    ) -> Vec<Action> {
        let mut actions: Vec<_> = self
            .extended
            .iter()
            .filter_map(|(a, s)| match s {
                Shortcut::Sequence(keys) if !physical && keys.as_slice() == [first, second] => {
                    Some(*a)
                }
                Shortcut::ControlSequence(keys)
                    if physical && keys.as_slice() == [first, second] =>
                {
                    Some(*a)
                }
                _ => None,
            })
            .collect();
        actions.sort();
        actions.dedup();
        actions
    }
    pub fn mouse_candidates(&self, button: &str, mods: egui::Modifiers) -> Vec<Action> {
        let mut actions: Vec<_> = self
            .extended
            .iter()
            .filter_map(|(a, s)| match s {
                Shortcut::Mouse {
                    button: b,
                    ctrl,
                    alt,
                    shift,
                } if b == button
                    && *ctrl == (mods.ctrl || mods.command || mods.mac_cmd)
                    && *alt == mods.alt
                    && *shift == mods.shift =>
                {
                    Some(*a)
                }
                _ => None,
            })
            .collect();
        actions.sort();
        actions.dedup();
        actions
    }
    pub fn sequence_prefix(&self, chord: KeyChord) -> bool {
        self.extended
            .iter()
            .any(|(_, s)| matches!(s, Shortcut::Sequence(keys) if keys.first() == Some(&chord)))
    }
    pub fn sequence_action(&self, first: KeyChord, second: KeyChord) -> Option<Action> {
        self.extended.iter().find_map(|(a, s)| {
            matches!(s, Shortcut::Sequence(keys) if keys.as_slice() == [first, second])
                .then_some(*a)
        })
    }
    pub fn mouse_action(&self, button: &str, mods: egui::Modifiers) -> Option<Action> {
        self.extended.iter().find_map(|(a, s)| match s {
            Shortcut::Mouse {
                button: b,
                ctrl,
                alt,
                shift,
            } if b == button
                && *ctrl == (mods.ctrl || mods.command || mods.mac_cmd)
                && *alt == mods.alt
                && *shift == mods.shift =>
            {
                Some(*a)
            }
            _ => None,
        })
    }
    pub fn abbreviation_score(&self, action: Action, query: &str) -> Option<i32> {
        self.extended
            .iter()
            .filter_map(|(a, s)| match s {
                Shortcut::Abbreviation(word)
                    if *a == action && word.eq_ignore_ascii_case(query.trim()) =>
                {
                    Some(10000)
                }
                _ => None,
            })
            .max()
    }
    pub fn export_xml(preset: &str, overrides: &[(String, Vec<String>)]) -> String {
        fn escape(s: &str) -> String {
            s.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('"', "&quot;")
                .replace('>', "&gt;")
        }
        let mut xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<keymap name=\"RustPy\" parent=\"{}\">\n",
            escape(preset)
        );
        for (id, shortcuts) in overrides {
            xml.push_str(&format!("  <action id=\"{}\">\n", escape(id)));
            for shortcut in shortcuts {
                xml.push_str(&format!("    <shortcut value=\"{}\"/>\n", escape(shortcut)));
            }
            xml.push_str("  </action>\n");
        }
        xml.push_str("</keymap>\n");
        xml
    }
    pub fn import_file(
        path: &std::path::Path,
    ) -> Result<(String, rustpy_settings::ShortcutOverrides, Vec<String>), String> {
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mut reader = std::io::Read::take(file, 4 * 1024 * 1024 + 1);
        let mut text = String::new();
        std::io::Read::read_to_string(&mut reader, &mut text).map_err(|e| e.to_string())?;
        Self::import_document(&text)
    }
    pub fn import_document(
        text: &str,
    ) -> Result<(String, rustpy_settings::ShortcutOverrides, Vec<String>), String> {
        if text.len() > 4 * 1024 * 1024 {
            return Err("Keymap exceeds 4 MiB".into());
        }
        if !text.trim_start().starts_with('<') {
            return Self::import_json(text);
        }
        let document = roxmltree::Document::parse(text).map_err(|e| e.to_string())?;
        let root = document.root_element();
        if root.tag_name().name() != "keymap" {
            return Err("Expected a keymap element".into());
        }
        let mut bindings = serde_json::Map::new();
        for node in root.children().filter(|n| n.has_tag_name("action")) {
            let Some(id) = node.attribute("id") else {
                continue;
            };
            if bindings.contains_key(id) {
                return Err(format!("Duplicate action {id}"));
            }
            let mut shortcuts = Vec::new();
            for s in node.children().filter(|n| n.is_element()) {
                let value = match s.tag_name().name() {
                    "shortcut" => s.attribute("value").map(str::to_owned),
                    "abbreviation" => s.attribute("value").map(|v| format!("Abbreviation:{v}")),
                    "mouse-shortcut" => s.attribute("keystroke").map(|v| {
                        let mut parts = Vec::new();
                        for part in v.split_whitespace() {
                            parts.push(match part.to_ascii_lowercase().as_str() {
                                "control" | "ctrl" | "meta" => "Ctrl",
                                "alt" => "Alt",
                                "shift" => "Shift",
                                "button1" => "Primary",
                                "button2" => "Middle",
                                "button3" => "Secondary",
                                "doubleclick" => "Double",
                                _ => part,
                            });
                        }
                        let mut text = format!("Mouse:{}", parts.join("+"));
                        text = text.replace("Primary+Double", "PrimaryDouble");
                        text
                    }),
                    "keyboard-shortcut" => s.attribute("first-keystroke").map(|first| {
                        let convert = |v: &str| {
                            v.split_whitespace()
                                .map(|p| match p.to_ascii_lowercase().as_str() {
                                    "ctrl" | "control" => "Control".into(),
                                    "meta" => "Ctrl".into(),
                                    "alt" => "Alt".into(),
                                    "shift" => "Shift".into(),
                                    _ => p.to_owned(),
                                })
                                .collect::<Vec<_>>()
                                .join("+")
                        };
                        let mut v = convert(first);
                        if let Some(second) = s.attribute("second-keystroke") {
                            v.push_str(", ");
                            v.push_str(&convert(second));
                        }
                        v
                    }),
                    _ => None,
                };
                if let Some(v) = value {
                    shortcuts.push(v);
                }
            }
            bindings.insert(id.to_owned(), serde_json::json!(shortcuts));
        }
        Self::import_json(&serde_json::json!({"parent":root.attribute("parent").unwrap_or("Default"),"bindings":bindings}).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sequence_is_not_a_single_stroke_and_roundtrips_xml() {
        assert!(KeyChord::parse("Ctrl+K, Ctrl+C").is_none());
        let mut keymap = Keymap::default_keymap();
        keymap
            .bind_definition(Action::Save, "Ctrl+K, Ctrl+S", false)
            .unwrap();
        assert_eq!(
            keymap.sequence_action(
                KeyChord::parse("Ctrl+K").unwrap(),
                KeyChord::parse("Ctrl+S").unwrap()
            ),
            Some(Action::Save)
        );
        let overrides = keymap.overrides_from(&Keymap::default_keymap());
        let (_, imported, skipped) =
            Keymap::import_document(&Keymap::export_xml("Default", &overrides)).unwrap();
        assert!(skipped.is_empty());
        assert_eq!(imported, overrides);
    }
    #[test]
    fn abbreviation_and_mouse_conflicts_keep_or_remove_owners() {
        let mut km = Keymap::default_keymap();
        km.bind_definition(Action::Save, "Abbreviation:write", false)
            .unwrap();
        assert_eq!(km.abbreviation_score(Action::Save, "WRITE"), Some(10000));
        km.bind_definition(Action::Save, "Mouse:Alt+Middle", false)
            .unwrap();
        assert_eq!(
            km.definition_conflicts(Action::Run, "Mouse:Alt+Middle"),
            vec![Action::Save]
        );
        km.bind_definition(Action::Run, "Mouse:Alt+Middle", false)
            .unwrap();
        assert_eq!(
            km.mouse_action("Middle", egui::Modifiers::ALT),
            Some(Action::Run)
        );
    }
}

#[cfg(test)]
mod persistence_fixtures {
    use super::*;
    #[test]
    fn retained_conflict_survives_every_serialization_and_remove_is_exact() {
        let mut map = Keymap::default_keymap();
        map.bind_definition(Action::Run, "Ctrl+S", true).unwrap();
        assert_eq!(
            map.shared_actions(KeyChord::parse("Ctrl+S").unwrap()).len(),
            2
        );
        let overrides = map.overrides_from(&Keymap::default_keymap());
        for document in [
            Keymap::export_json("Default", &overrides),
            Keymap::export_xml("Default", &overrides),
        ] {
            let (parent, overrides, skipped) = Keymap::import_document(&document).unwrap();
            assert!(skipped.is_empty());
            let mut loaded = Keymap::preset(&parent);
            loaded.apply_overrides(&overrides);
            assert_eq!(
                loaded
                    .shared_actions(KeyChord::parse("Ctrl+S").unwrap())
                    .len(),
                2
            );
            loaded
                .bind_definition(Action::Debug, "Ctrl+S", false)
                .unwrap();
            assert!(loaded
                .shared_actions(KeyChord::parse("Ctrl+S").unwrap())
                .is_empty());
            assert_eq!(
                loaded.lookup(KeyChord::parse("Ctrl+S").unwrap()),
                Some(Action::Debug)
            );
        }
    }
    #[test]
    fn keymap_xml_second_stroke_and_untrusted_inputs() {
        let (_,overrides,skipped)=Keymap::import_document(r#"<keymap parent="Default"><action id="SaveDocument"><keyboard-shortcut first-keystroke="ctrl X" second-keystroke="ctrl S"/><mouse-shortcut keystroke="alt button2"/><abbreviation value="write"/></action></keymap>"#).unwrap();
        assert!(skipped.is_empty());
        let mut map = Keymap::default_keymap();
        map.apply_overrides(&overrides);
        assert_eq!(
            map.control_sequence_action(
                KeyChord::parse("Ctrl+X").unwrap(),
                KeyChord::parse("Ctrl+S").unwrap()
            ),
            Some(Action::Save)
        );
        assert_eq!(
            map.mouse_action("Middle", egui::Modifiers::ALT),
            Some(Action::Save)
        );
        assert_eq!(map.abbreviation_score(Action::Save, "write"), Some(10000));
        assert!(Keymap::import_document(
            "<!DOCTYPE keymap [<!ENTITY x SYSTEM 'file:///etc/passwd'>]><keymap>&x;</keymap>"
        )
        .is_err());
        assert!(Keymap::import_document(&"x".repeat(4 * 1024 * 1024 + 1)).is_err());
        assert!(KeyChord::parse("Bogus+Ctrl+S").is_none());
        assert!(KeyChord::parse("Ctrl+X, Ctrl+S").is_none());
    }
    #[test]
    fn reset_removes_extended_edits_and_restores_mouse_defaults() {
        let mut map = Keymap::default_keymap();
        map.bind_definition(Action::Save, "Abbreviation:write", false)
            .unwrap();
        assert!(!map.is_default(Action::Save));
        map.reset_action(Action::Save);
        assert!(map.is_default(Action::Save));
        map.clear_action(Action::GoToDeclaration);
        assert_eq!(map.mouse_action("Middle", egui::Modifiers::NONE), None);
        map.reset_action(Action::GoToDeclaration);
        assert_eq!(
            map.mouse_action("Middle", egui::Modifiers::NONE),
            Some(Action::GoToDeclaration)
        );
    }
    #[test]
    fn physical_control_is_distinct_from_command() {
        let mut map = Keymap::default_keymap();
        map.bind_definition(Action::CaretLeft, "Control+B", false)
            .unwrap();
        assert_eq!(
            map.physical_control_action(
                egui::Key::B,
                egui::Modifiers {
                    ctrl: true,
                    ..Default::default()
                }
            ),
            Some(Action::CaretLeft)
        );
        assert_eq!(
            map.physical_control_action(
                egui::Key::B,
                egui::Modifiers {
                    mac_cmd: true,
                    command: true,
                    ..Default::default()
                }
            ),
            None
        );
    }
}

#[cfg(test)]
mod ownership_fixtures {
    use super::*;
    #[test]
    fn sequence_prefixes_are_reserved_from_extensions_and_reassignment_removes_them() {
        let mut map = Keymap::default_keymap();
        map.bind_definition(Action::Save, "Ctrl+K, Ctrl+S", false)
            .unwrap();
        let first = KeyChord::parse("Ctrl+K").unwrap();
        assert_eq!(map.chord_owner(first), Some(Action::Save));
        assert!(map.lookup(first).is_none());
        assert!(map
            .definition_conflicts(Action::Run, "Ctrl+K")
            .contains(&Action::Save));
        map.bind_definition(Action::Run, "Ctrl+K", false).unwrap();
        assert!(!map.sequence_prefix(first));
        assert_eq!(map.lookup(first), Some(Action::Run));
    }
    #[test]
    fn retained_sequences_and_mouse_bindings_expose_every_chooser_candidate() {
        let mut map = Keymap::default_keymap();
        for action in [Action::Run, Action::Save] {
            map.bind_definition(action, "Ctrl+K, Ctrl+S", true).unwrap();
            map.bind_definition(action, "Mouse:Alt+Middle", true)
                .unwrap();
        }
        assert_eq!(
            map.sequence_candidates(
                KeyChord::parse("Ctrl+K").unwrap(),
                KeyChord::parse("Ctrl+S").unwrap(),
                false
            )
            .len(),
            2
        );
        assert_eq!(
            map.mouse_candidates("Middle", egui::Modifiers::ALT).len(),
            2
        );
        map.bind_definition(Action::Debug, "Mouse:Alt+Middle", false)
            .unwrap();
        assert_eq!(
            map.mouse_candidates("Middle", egui::Modifiers::ALT),
            vec![Action::Debug]
        );
    }
    #[test]
    fn linux_desktop_warnings_are_specific_to_reserved_combinations() {
        assert!(Keymap::linux_global_conflict("Alt+F4"));
        assert!(Keymap::linux_global_conflict("Ctrl+Alt+ArrowRight"));
        assert!(!Keymap::linux_global_conflict("Ctrl+Shift+S"));
    }
}
