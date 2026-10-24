//! Recover modified Copy chords swallowed by the native clipboard transport,
//! and drop the text X11 reports for an Alt or Ctrl chord (Alt+1 also sends
//! "1"). A quick chord is released within one frame, so widgets that check
//! `InputState::modifiers` see no Alt and typed the digit (Alt+1 started a
//! Project speed search for "1", and arrows then went nowhere).

#[derive(Default)]
pub(super) struct ClipboardShortcuts {
    modifiers: egui::Modifiers,
}

impl ClipboardShortcuts {
    pub(super) fn normalize(&mut self, raw: &mut egui::RawInput) {
        let mut chord_text = Vec::new();
        for (index, event) in raw.events.iter_mut().enumerate() {
            match event {
                egui::Event::ModifiersChanged(modifiers) | egui::Event::Key { modifiers, .. } => {
                    self.modifiers = *modifiers
                }
                egui::Event::WindowFocused(false) => self.modifiers = egui::Modifiers::NONE,
                egui::Event::Copy
                    if self.modifiers.command && (self.modifiers.alt || self.modifiers.shift) =>
                {
                    *event = egui::Event::Key {
                        key: egui::Key::C,
                        physical_key: Some(egui::Key::C),
                        pressed: true,
                        repeat: false,
                        modifiers: self.modifiers,
                    };
                }
                egui::Event::Text(_) if is_chord(self.modifiers) => chord_text.push(index),
                _ => {}
            }
        }
        for index in chord_text.into_iter().rev() {
            raw.events.remove(index);
        }
    }
}

/// Alt or Ctrl alone. Ctrl+Alt is AltGr on Windows and Option composes
/// characters on macOS: their text is real typing.
fn is_chord(modifiers: egui::Modifiers) -> bool {
    !crate::actions::shortcuts::mac_keymap() && (modifiers.alt != modifiers.ctrl)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_chord_text_is_dropped_but_typing_and_altgr_remain() {
        let key = |key, modifiers| egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        };
        let mut state = ClipboardShortcuts::default();
        let mut raw = egui::RawInput {
            events: vec![
                key(egui::Key::Num1, egui::Modifiers::ALT),
                egui::Event::Text("1".into()),
                egui::Event::ModifiersChanged(egui::Modifiers::NONE),
                key(egui::Key::A, egui::Modifiers::NONE),
                egui::Event::Text("a".into()),
                key(egui::Key::Q, egui::Modifiers::CTRL | egui::Modifiers::ALT),
                egui::Event::Text("@".into()),
            ],
            ..Default::default()
        };
        state.normalize(&mut raw);
        let texts: Vec<_> = raw
            .events
            .iter()
            .filter_map(|e| match e {
                egui::Event::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        if crate::actions::shortcuts::mac_keymap() {
            assert_eq!(texts, ["1", "a", "@"]);
        } else {
            assert_eq!(texts, ["a", "@"]);
        }
    }

    #[test]
    fn ordinary_copy_remains_a_clipboard_event() {
        let mut state = ClipboardShortcuts::default();
        let mut raw = egui::RawInput {
            events: vec![
                egui::Event::ModifiersChanged(egui::Modifiers::CTRL),
                egui::Event::Copy,
            ],
            ..Default::default()
        };
        state.normalize(&mut raw);
        assert!(matches!(raw.events[1], egui::Event::Copy));
    }

    #[test]
    fn modifiers_survive_frames_but_focus_loss_clears_them() {
        let mut state = ClipboardShortcuts::default();
        let modifiers = egui::Modifiers::CTRL | egui::Modifiers::SHIFT | egui::Modifiers::COMMAND;
        let mut raw = egui::RawInput {
            events: vec![egui::Event::ModifiersChanged(modifiers)],
            ..Default::default()
        };
        state.normalize(&mut raw);
        raw.events = vec![egui::Event::Copy];
        state.normalize(&mut raw);
        assert!(
            matches!(raw.events[0], egui::Event::Key { key: egui::Key::C, pressed: true, modifiers: actual, .. } if actual == modifiers)
        );
        raw.events = vec![egui::Event::WindowFocused(false), egui::Event::Copy];
        state.normalize(&mut raw);
        assert!(matches!(raw.events[1], egui::Event::Copy));
    }
}
