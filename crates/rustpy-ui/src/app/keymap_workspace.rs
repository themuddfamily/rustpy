//! Custom profile management and context-bound two-stroke dispatch.
use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;

#[derive(Default)]
pub(super) struct KeymapWorkspace {
    name: String,
    target: String,
    definition: String,
    conflict: Option<(Action, String)>,
    choices: Vec<Action>,
    choices_session: u64,
    choices_editor: bool,
    choices_tab: Option<egui::Id>,
    recorder: Option<(usize, bool, Vec<String>)>,
    quick_name: String,
    quick_actions: String,
    quick_shortcut: String,
    prefix: Option<(KeyChord, f64, u64, Option<egui::Id>, bool)>,
    pub(super) reference_file: String,
}
impl IdeApp {
    pub(super) fn offer_keymap_choices(&mut self, actions: Vec<Action>, tab: Option<egui::Id>) {
        self.keymap_workspace.choices = actions;
        self.keymap_workspace.choices_session = self.file_session;
        self.keymap_workspace.choices_editor = tab.is_some();
        self.keymap_workspace.choices_tab = tab;
    }
    pub(super) fn reset_keymap_input(&mut self) {
        self.keymap_workspace.prefix = None;
        self.keymap_workspace.choices.clear();
        self.keymap_workspace.recorder = None;
        self.keymap_workspace.conflict = None;
    }
    pub(super) fn keymap_parent(&self) -> &str {
        self.settings
            .custom_keymaps
            .iter()
            .find(|(name, _)| name == &self.settings.keymap)
            .map(|(_, parent)| parent.as_str())
            .unwrap_or(self.settings.keymap_name())
    }
    pub(super) fn keymap_workspace_ui(&mut self, ui: &mut egui::Ui) {
        self.named_quick_lists_ui(ui);
        ui.horizontal(|ui| {
            ui.label(crate::i18n::tr_static("Custom keymap name:"));
            ui.text_edit_singleline(&mut self.keymap_workspace.name);
            let name = self.keymap_workspace.name.trim().to_owned();
            let valid = !name.is_empty()
                && name.len() <= 120
                && !name.chars().any(char::is_control)
                && !crate::plugin_host::keymap_names().contains(&name)
                && !self.settings.custom_keymaps.iter().any(|(n, _)| *n == name);
            if ui
                .add_enabled(valid, egui::Button::new(crate::i18n::tr_static("Copy")))
                .clicked()
            {
                let parent = self.keymap_parent().to_owned();
                let overrides = self.settings.custom_shortcuts.clone();
                self.settings.switch_keymap(&name);
                self.settings.custom_keymaps.push((name.clone(), parent));
                self.settings.custom_shortcuts = overrides;
                self.apply_custom_shortcuts();
            }
            let custom = self
                .settings
                .custom_keymaps
                .iter()
                .position(|(n, _)| n == &self.settings.keymap);
            if ui
                .add_enabled(
                    valid && custom.is_some(),
                    egui::Button::new(crate::i18n::tr_static("Rename")),
                )
                .clicked()
            {
                let index = custom.unwrap();
                let old = self.settings.keymap.clone();
                self.settings.custom_keymaps[index].0 = name.clone();
                self.settings.keymap = name;
                for (n, _) in &mut self.settings.keymap_overrides {
                    if *n == old {
                        *n = self.settings.keymap.clone();
                    }
                }
                self.apply_custom_shortcuts();
            }
            if ui
                .add_enabled(
                    custom.is_some(),
                    egui::Button::new(crate::i18n::tr_static("Delete")),
                )
                .clicked()
            {
                let (name, parent) = self.settings.custom_keymaps.remove(custom.unwrap());
                self.settings.switch_keymap(&parent);
                self.settings.keymap_overrides.retain(|(n, _)| *n != name);
                self.apply_custom_shortcuts();
            }
        });
        ui.collapsing(crate::i18n::tr_static("Add keyboard, mouse or abbreviation shortcut"), |ui| {
            if self.keymap_workspace.target.is_empty() { self.keymap_workspace.target = Action::Save.id().into(); }
            egui::ComboBox::from_id_salt("extended_shortcut_action").selected_text(&self.keymap_workspace.target).show_ui(ui, |ui| {
                for &a in Action::ALL { ui.selectable_value(&mut self.keymap_workspace.target, a.id().into(), a.text()); }
            }).combo_named(crate::i18n::tr_static("Action"));
            if let Some(action) = Keymap::action_by_id(&self.keymap_workspace.target) {
                for definition in self.keymap.shortcut_definitions(action) {
                    ui.horizontal(|ui| {
                        ui.label(&definition);
                        if ui.small_button(crate::i18n::tr_static("Remove")).clicked() {
                            self.keymap.remove_definition(action,&definition);
                            self.settings.custom_shortcuts = self.keymap_overrides();
                        }
                    });
                }
            }
            ui.text_edit_singleline(&mut self.keymap_workspace.definition);
            ui.horizontal(|ui| {
                for (label,count,mouse) in [("Record key",1,false),("Record two strokes",2,false),("Record mouse",1,true)] {
                    let response = ui.button(crate::i18n::tr(label));
                    if response.clicked() { self.keymap_workspace.recorder = Some((count,mouse,Vec::new())); response.surrender_focus(); }
                }
                if self.keymap_workspace.recorder.is_some() { ui.label(crate::i18n::tr_static("Recording… Esc cancels")); }
            });
            if ui.button(crate::i18n::tr_static("Find actions using this shortcut")).clicked() {
                let query = self.keymap_workspace.definition.trim();
                self.settings_ui.message = Some(Action::ALL.iter().filter(|a| self.keymap.shortcut_definitions(**a).iter().any(|s| s.eq_ignore_ascii_case(query))).map(|a| a.text()).collect::<Vec<_>>().join(", "));
            }

            ui.label(crate::i18n::tr_static("Examples: Ctrl+K, Ctrl+C · Mouse:Alt+Middle · Mouse:Ctrl+WheelUp · Abbreviation:format"));
            ui.label(crate::i18n::tr_static("On macOS Ctrl denotes the Command key (⌘); use Control explicitly for the physical Control key (⌃)."));
            if cfg!(target_os="linux") && Keymap::linux_global_conflict(&self.keymap_workspace.definition) {
                ui.colored_label(self.theme.warning,crate::i18n::tr_static("This combination is commonly reserved by the Linux desktop/window manager. Reassign the OS binding if RustPy does not receive it."));
            }
            if ui.button(crate::i18n::tr_static("Add shortcut")).clicked() {
                if let Some(action) = Keymap::action_by_id(&self.keymap_workspace.target) {
                    let value = self.keymap_workspace.definition.trim().to_owned();
                    if self.keymap.definition_conflicts(action, &value).is_empty() {
                        match self.keymap.bind_definition(action, &value, false) {
                            Ok(()) => self.settings.custom_shortcuts = self.keymap_overrides(),
                            Err(e) => self.settings_ui.message = Some(e),
                        }
                    } else { self.keymap_workspace.conflict = Some((action,value)); }
                }
            }
            if let Some((action, value)) = self.keymap_workspace.conflict.clone() {
                let owners = self.keymap.definition_conflicts(action, &value).iter().map(|a| a.text()).collect::<Vec<_>>().join(", ");
                ui.colored_label(self.theme.warning, crate::i18n::tr_fmt("{value} conflicts with {owners}",&[("value",&value),("owners",&owners)]));
                ui.horizontal(|ui| {
                    for (label,leave) in [("Remove other assignments",false),("Leave other assignments",true)] {
                        if ui.button(crate::i18n::tr(label)).clicked() {
                            match self.keymap.bind_definition(action, &value, leave) {
                                Ok(()) => self.settings.custom_shortcuts = self.keymap_overrides(),
                                Err(e) => self.settings_ui.message = Some(e),
                            }
                            self.keymap_workspace.conflict = None;
                        }
                    }
                    if ui.button(crate::i18n::tr_static("Cancel")).clicked() { self.keymap_workspace.conflict = None; }
                });
            }
            if cfg!(target_os="linux") {
                ui.label(crate::i18n::tr_static("Linux desktop shortcuts may intercept Alt+F1/F2/F4, Ctrl+Alt+arrows and Super combinations. Change the desktop binding if RustPy never receives the event."));
            }
        });
    }
    fn named_quick_lists_ui(&mut self, ui: &mut egui::Ui) {
        ui.collapsing(crate::i18n::tr_static("Named Quick Lists and macro shortcuts"), |ui| {
            let mut remove = None;
            for (i,(name,actions,shortcut)) in self.settings.keymap_quick_lists.iter_mut().enumerate() {
                ui.horizontal(|ui| { ui.label(name.as_str()); ui.text_edit_singleline(shortcut); if ui.small_button(crate::i18n::tr_static("Delete")).clicked() { remove = Some(i); } });
                ui.label(actions.join(", "));
            }
            if let Some(i) = remove { self.settings.keymap_quick_lists.remove(i); }
            ui.horizontal(|ui| { ui.label(crate::i18n::tr_static("Name")); ui.text_edit_singleline(&mut self.keymap_workspace.quick_name); });
            ui.horizontal(|ui| { ui.label(crate::i18n::tr_static("Action IDs")); ui.text_edit_singleline(&mut self.keymap_workspace.quick_actions); });
            ui.horizontal(|ui| { ui.label(crate::i18n::tr_static("Shortcut")); ui.text_edit_singleline(&mut self.keymap_workspace.quick_shortcut); });
            if ui.button(crate::i18n::tr_static("Add Quick List")).clicked() {
                let name = self.keymap_workspace.quick_name.trim().to_owned();
                let actions: Vec<_> = self.keymap_workspace.quick_actions.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect();
                if name.is_empty() || self.settings.keymap_quick_lists.iter().any(|(n,_,_)| *n == name) || actions.is_empty() || actions.iter().any(|s| Keymap::action_by_id(s).is_none()) {
                    self.settings_ui.message = Some("Use a unique name and known action IDs".into());
                } else if let Some(chord) = KeyChord::parse(&self.keymap_workspace.quick_shortcut) {
                    if self.keymap.chord_owner(chord).is_some()
                        || self.settings.external_tools.iter().any(|t| KeyChord::parse(&t.shortcut) == Some(chord))
                        || self.settings.macro_shortcuts.iter().any(|t| KeyChord::parse(&t.shortcut) == Some(chord))
                        || self.settings.keymap_quick_lists.iter().any(|(_,_,s)| KeyChord::parse(s) == Some(chord)) {
                        self.settings_ui.message = Some("The Quick List shortcut is already assigned".into());
                    } else { self.settings.keymap_quick_lists.push((name,actions,chord.to_string())); }
                } else { self.settings_ui.message = Some("Enter a keyboard shortcut such as Ctrl+Alt+Q".into()); }
            }
            let names: Vec<_> = self.settings.macros.iter().map(|(name,_)| name.clone()).collect();
            for name in names {
                let current = self.settings.macro_shortcuts.iter().find(|binding| binding.name == name).map(|binding| binding.shortcut.clone()).unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label(crate::i18n::tr_fmt("Macro: {name}",&[("name",&name)]));
                    let draft = crate::parsed_field::singleline(ui,egui::Id::new(("keymap_macro",&name)),&current,|t| t.hint_text("Ctrl+Alt+M"));
                    if draft.changed {
                        match self.macro_chord(&name,&draft.text) {
                            Ok(_) => {
                                self.settings.macro_shortcuts.retain(|binding| binding.name != name);
                                if !draft.text.trim().is_empty() { self.settings.macro_shortcuts.push(rustpy_settings::macro_shortcuts::MacroShortcut { name:name.clone(),shortcut:draft.text }); }
                            }
                            Err(error) => { ui.colored_label(self.theme.warning,error); }
                        }
                    }
                });
            }
            ui.label(crate::i18n::tr_static("Create or record macros in Edit → Macros; their named shortcuts run in the editor."));
        });
    }
    pub(super) fn capture_extended_shortcut(&mut self, ctx: &egui::Context) -> bool {
        if !matches!(self.dialog, Dialog::Settings) {
            self.keymap_workspace.recorder = None;
            return false;
        }
        let Some((limit, mouse, mut strokes)) = self.keymap_workspace.recorder.take() else {
            return false;
        };
        let events = ctx.input(|i| i.events.clone());
        let mut result = None;
        let mut cancelled = false;
        for event in &events {
            match event {
                egui::Event::Key {
                    key: egui::Key::Escape,
                    pressed: true,
                    ..
                } => {
                    cancelled = true;
                    break;
                }
                egui::Event::Key {
                    key,
                    modifiers,
                    pressed: true,
                    repeat: false,
                    ..
                } if !mouse && !crate::actions::is_modifier_key(*key) => {
                    let mut text = KeyChord::new(*key, *modifiers).to_string();
                    if crate::actions::shortcuts::mac_keymap()
                        && modifiers.ctrl
                        && !modifiers.mac_cmd
                    {
                        text = text.replacen("Ctrl+", "Control+", 1);
                    }
                    strokes.push(text);
                    if strokes.len() == limit {
                        result = Some(strokes.join(", "));
                        break;
                    }
                }
                egui::Event::PointerButton {
                    button,
                    modifiers,
                    pressed: true,
                    ..
                } if mouse => {
                    let name = match button {
                        egui::PointerButton::Primary => "Primary",
                        egui::PointerButton::Secondary => "Secondary",
                        egui::PointerButton::Middle => "Middle",
                        egui::PointerButton::Extra1 => "Extra1",
                        egui::PointerButton::Extra2 => "Extra2",
                    };
                    result = Some(format!(
                        "Mouse:{}{}{}{name}",
                        if modifiers.ctrl || modifiers.command {
                            "Ctrl+"
                        } else {
                            ""
                        },
                        if modifiers.alt { "Alt+" } else { "" },
                        if modifiers.shift { "Shift+" } else { "" }
                    ));
                    break;
                }
                egui::Event::MouseWheel {
                    delta, modifiers, ..
                } if mouse && delta.y != 0.0 => {
                    result = Some(format!(
                        "Mouse:{}{}{}{}",
                        if modifiers.ctrl || modifiers.command {
                            "Ctrl+"
                        } else {
                            ""
                        },
                        if modifiers.alt { "Alt+" } else { "" },
                        if modifiers.shift { "Shift+" } else { "" },
                        if delta.y > 0.0 {
                            "WheelUp"
                        } else {
                            "WheelDown"
                        }
                    ));
                    break;
                }
                _ => {}
            }
        }
        ctx.input_mut(|i| {
            i.events.retain(|e| {
                !matches!(e, egui::Event::Key { .. } | egui::Event::Text(_))
                    && !(mouse
                        && matches!(
                            e,
                            egui::Event::PointerButton { .. } | egui::Event::MouseWheel { .. }
                        ))
            })
        });
        if let Some(text) = result {
            self.keymap_workspace.definition = text;
        } else if !cancelled {
            self.keymap_workspace.recorder = Some((limit, mouse, strokes));
        }
        true
    }
    pub(super) fn keymap_reference_export(&mut self, ui: &mut egui::Ui) {
        if self.keymap_workspace.reference_file.is_empty() {
            self.keymap_workspace.reference_file = dirs::home_dir()
                .unwrap_or_default()
                .join("rustpy-keyboard-shortcuts.pdf")
                .display()
                .to_string();
        }
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut self.keymap_workspace.reference_file);
            if ui
                .button(crate::i18n::tr_static("Export printable PDF"))
                .clicked()
            {
                let path = PathBuf::from(self.keymap_workspace.reference_file.trim());
                self.settings_ui.message =
                    Some(match std::fs::write(&path, self.keymap.reference_pdf()) {
                        Ok(()) => format!("Keyboard reference saved to {}", path.display()),
                        Err(e) => format!("Cannot write {}: {e}", path.display()),
                    });
            }
        });
        if let Some(message) = &self.settings_ui.message {
            ui.label(message);
        }
    }
    pub(super) fn keymap_overrides(&self) -> rustpy_settings::ShortcutOverrides {
        Action::ALL
            .iter()
            .copied()
            .filter(|a| !self.keymap.is_default(*a))
            .map(|a| (a.id().into(), self.keymap.shortcut_definitions(a)))
            .collect()
    }
    pub(super) fn keymap_sequence(&mut self, ui: &mut egui::Ui, editor_focused: bool) -> bool {
        if self.keymap_workspace.choices_session != self.file_session {
            self.keymap_workspace.choices.clear();
        }
        if !self.keymap_workspace.choices.is_empty() {
            let mut selected = None;
            let mut open = true;
            egui::Window::new(crate::i18n::tr_static("Choose shortcut action"))
                .id(egui::Id::new("shared_keymap_action"))
                .open(&mut open)
                .collapsible(false)
                .show_modal(ui.ctx(), |ui| {
                    for &action in &self.keymap_workspace.choices {
                        if ui.button(action.text()).clicked() {
                            selected = Some(action);
                        }
                    }
                });
            if let Some(action) = selected {
                self.keymap_workspace.choices.clear();
                if let Some(id) = self.keymap_workspace.choices_tab.take() {
                    let Some(index) = self.tabs.iter().position(|tab| tab.view.id == id) else {
                        return true;
                    };
                    self.active = Some(index);
                    self.group_active[self.tabs[index].group as usize] = Some(id);
                    self.focus_editor = true;
                }
                if !self.keymap_workspace.choices_editor || !self.dispatch_editor_action(action) {
                    self.handle_action(action, ui.ctx());
                }
            } else if !open || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.keymap_workspace.choices.clear();
            }
            return true;
        }
        let now = ui.input(|i| i.time);
        let focus = ui.memory(|m| m.focused());
        if let Some((first, started, session, id, physical)) = self.keymap_workspace.prefix {
            if session != self.file_session || id != focus {
                self.keymap_workspace.prefix = None;
            } else if now - started > 1.5 {
                self.keymap_workspace.prefix = None;
                let mut candidates = if physical {
                    self.keymap.physical_control_candidates(
                        first.key,
                        egui::Modifiers {
                            ctrl: true,
                            alt: first.alt,
                            shift: first.shift,
                            ..Default::default()
                        },
                    )
                } else {
                    self.keymap.shared_actions(first)
                };
                if candidates.is_empty() && !physical {
                    candidates.extend(self.keymap.lookup(first));
                }
                candidates.retain(|a| {
                    editor_focused || (!view::is_editor_action(*a) && !Self::is_emacs_action(*a))
                });
                if candidates.len() > 1 {
                    let tab = if editor_focused {
                        self.active.map(|i| self.tabs[i].view.id)
                    } else {
                        None
                    };
                    self.offer_keymap_choices(candidates, tab);
                    return true;
                }
                if let Some(action) = candidates.first().copied() {
                    if !editor_focused || !self.dispatch_editor_action(action) {
                        self.handle_action(action, ui.ctx());
                    }
                    return true;
                }
            }
        }
        let keys: Vec<_> = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key {
                        key,
                        modifiers,
                        pressed: true,
                        repeat: false,
                        ..
                    } if !crate::actions::is_modifier_key(*key) => Some((*key, *modifiers)),
                    _ => None,
                })
                .collect()
        });
        for (key, mods) in keys {
            // The terminal owns its explicit overrides, including potential prefixes.
            if ((self.bottom == Some(BottomTool::Terminal) && self.terminal_panel.focused)
                || (self.bottom == Some(BottomTool::PythonConsole) && self.py_console.focused))
                && self.terminal_panel.override_shortcut(key, mods)
            {
                self.keymap_workspace.prefix = None;
                continue;
            }
            let chord = KeyChord::new(key, mods);
            if let Some((_, actions, _)) =
                self.settings.keymap_quick_lists.iter().find(|(_, _, s)| {
                    Keymap::primary_modifiers(mods) && KeyChord::parse(s) == Some(chord)
                })
            {
                self.keymap_workspace.choices_session = self.file_session;
                self.keymap_workspace.choices_editor = editor_focused;
                self.keymap_workspace.choices_tab = if editor_focused {
                    self.active.map(|i| self.tabs[i].view.id)
                } else {
                    None
                };
                self.keymap_workspace.choices = actions
                    .iter()
                    .filter_map(|id| Keymap::action_by_id(id))
                    .collect();
                ui.input_mut(|i| {
                    i.consume_key(mods, key);
                    if !mods.ctrl && !mods.command && !mods.alt {
                        i.events.retain(|e| !matches!(e, egui::Event::Text(_)));
                    }
                });
                return true;
            }

            if let Some((first, _, _, _, physical)) = self.keymap_workspace.prefix.take() {
                if key == egui::Key::Escape {
                    ui.input_mut(|i| {
                        i.consume_key(mods, key);
                        if !mods.ctrl && !mods.command && !mods.alt {
                            i.events.retain(|e| !matches!(e, egui::Event::Text(_)));
                        }
                    });
                    return true;
                }
                let action = if physical {
                    (mods.ctrl && !mods.mac_cmd)
                        .then(|| self.keymap.control_sequence_action(first, chord))
                        .flatten()
                } else if Keymap::primary_modifiers(mods) {
                    self.keymap.sequence_action(first, chord)
                } else {
                    None
                };
                let modifiers_match = if physical {
                    mods.ctrl && !mods.mac_cmd
                } else {
                    Keymap::primary_modifiers(mods)
                };
                let candidates = if modifiers_match {
                    self.keymap.sequence_candidates(first, chord, physical)
                } else {
                    Vec::new()
                };
                if candidates.len() > 1 {
                    let tab = if editor_focused {
                        self.active.map(|i| self.tabs[i].view.id)
                    } else {
                        None
                    };
                    self.offer_keymap_choices(candidates, tab);
                    ui.input_mut(|i| i.consume_key(mods, key));
                    return true;
                }
                if let Some(action) = action {
                    ui.input_mut(|i| {
                        i.consume_key(mods, key);
                        if !mods.ctrl && !mods.command && !mods.alt {
                            i.events.retain(|e| !matches!(e, egui::Event::Text(_)));
                        }
                    });
                    if !editor_focused || !self.dispatch_editor_action(action) {
                        self.handle_action(action, ui.ctx());
                    }
                    return true;
                }
                // A mismatching second stroke remains available to the ordinary dispatcher.
            }
            let mut physical_candidates = self.keymap.physical_control_candidates(key, mods);
            physical_candidates.retain(|a| {
                editor_focused || (!view::is_editor_action(*a) && !Self::is_emacs_action(*a))
            });
            if physical_candidates.len() > 1 && !self.keymap.control_sequence_prefix(chord) {
                let tab = if editor_focused {
                    self.active.map(|i| self.tabs[i].view.id)
                } else {
                    None
                };
                self.offer_keymap_choices(physical_candidates, tab);
                ui.input_mut(|i| i.consume_key(mods, key));
                return true;
            }
            if let Some(action) = self
                .keymap
                .physical_control_action(key, mods)
                .filter(|_| !self.keymap.control_sequence_prefix(chord))
            {
                if !editor_focused
                    && (view::is_editor_action(action) || Self::is_emacs_action(action))
                {
                    continue;
                }
                ui.input_mut(|i| {
                    i.consume_key(mods, key);
                    if !mods.ctrl && !mods.command && !mods.alt {
                        i.events.retain(|e| !matches!(e, egui::Event::Text(_)));
                    }
                });
                if !editor_focused || !self.dispatch_editor_action(action) {
                    self.handle_action(action, ui.ctx());
                }
                return true;
            }
            let shared = self.keymap.shared_actions(chord);
            if Keymap::primary_modifiers(mods)
                && !shared.is_empty()
                && !self.keymap.sequence_prefix(chord)
                && !self.keymap.control_sequence_prefix(chord)
            {
                ui.input_mut(|i| {
                    i.consume_key(mods, key);
                    if !mods.ctrl && !mods.command && !mods.alt {
                        i.events.retain(|e| !matches!(e, egui::Event::Text(_)));
                    }
                });
                self.keymap_workspace.choices = shared;
                self.keymap_workspace.choices_session = self.file_session;
                self.keymap_workspace.choices_editor = editor_focused;
                self.keymap_workspace.choices_tab = if editor_focused {
                    self.active.map(|i| self.tabs[i].view.id)
                } else {
                    None
                };
                return true;
            }

            let eligible = |a: &Action| {
                editor_focused || (!view::is_editor_action(*a) && !Self::is_emacs_action(*a))
            };
            let physical = mods.ctrl
                && !mods.mac_cmd
                && self.keymap.prefix_actions(chord, true).iter().any(eligible);
            if crate::actions::shortcuts::mac_keymap() && mods.ctrl && !mods.mac_cmd && !physical {
                continue;
            }
            if physical
                || self
                    .keymap
                    .prefix_actions(chord, false)
                    .iter()
                    .any(eligible)
            {
                ui.input_mut(|i| {
                    i.consume_key(mods, key);
                    if !mods.ctrl && !mods.command && !mods.alt {
                        i.events.retain(|e| !matches!(e, egui::Event::Text(_)));
                    }
                });
                self.keymap_workspace.prefix =
                    Some((chord, now, self.file_session, focus, physical));
                self.set_status(format!("{chord}, … (Esc cancels)"));
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(1500));
                return true;
            }
        }
        false
    }
}
