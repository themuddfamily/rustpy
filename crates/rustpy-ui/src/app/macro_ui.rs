//! Saved macro step editor and named, editor-owned shortcuts.
use super::*;
use crate::a11y::DialogWindow;
use rustpy_settings::macro_shortcuts::MacroShortcut;

#[derive(Clone)]
struct Draft {
    selected: usize,
    original: String,
    name: String,
    steps: String,
    shortcut: String,
    error: Option<String>,
}
fn draft_id() -> egui::Id {
    egui::Id::new("macro-editor-draft")
}

pub(super) fn validate_steps(steps: &[String]) -> Result<(), String> {
    for (index, step) in steps.iter().enumerate() {
        if step.starts_with("text:") {
            continue;
        }
        let action = step.strip_prefix("action:").and_then(Keymap::action_by_id);
        if action.is_none()
            || matches!(
                action,
                Some(
                    Action::StartStopMacroRecording
                        | Action::PlaybackLastMacro
                        | Action::EditMacros
                )
            )
        {
            return Err(format!(
                "Step {} must be text:<text> or a non-macro action:<action id>",
                index + 1
            ));
        }
    }
    Ok(())
}
fn parse_steps(text: &str) -> Result<Vec<String>, String> {
    if text.len() > 2 * 1024 * 1024 {
        return Err("Macro steps exceed the 2 MiB editor limit".into());
    }
    let steps = serde_json::from_str::<Vec<String>>(text)
        .map_err(|error| format!("Steps must be a JSON list of strings: {error}"))?;
    if steps.len() > 5000 {
        return Err("The step editor supports at most 5000 steps".into());
    }
    validate_steps(&steps)?;
    Ok(steps)
}

#[cfg(test)]
thread_local! { static SAVE_PATH: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) }; }
fn persist(settings: &Settings) -> Result<(), String> {
    #[cfg(test)]
    if let Some(path) = SAVE_PATH.with(|path| path.borrow().clone()) {
        let (_, mut baseline, error) = rustpy_settings::persist::load_from(&path);
        if let Some(error) = error {
            return Err(error.to_string());
        }
        return rustpy_settings::persist::save_to(&path, settings, &mut baseline)
            .map_err(|error| error.to_string());
    }
    settings.save().map_err(|error| error.to_string())
}

impl IdeApp {
    pub(super) fn macro_chord(
        &self,
        name: &str,
        shortcut: &str,
    ) -> Result<Option<KeyChord>, String> {
        let chord = crate::external_tools::tool_chord(shortcut, &self.keymap)?;
        if let Some(chord) = chord {
            if self
                .settings
                .keymap_quick_lists
                .iter()
                .any(|(_, _, shortcut)| KeyChord::parse(shortcut) == Some(chord))
            {
                return Err(format!("{chord} is assigned to a Quick List"));
            }
            if self
                .settings
                .external_tools
                .iter()
                .any(|tool| KeyChord::parse(&tool.shortcut) == Some(chord))
            {
                return Err(format!("{chord} is assigned to an external tool"));
            }
            if self.settings.macro_shortcuts.iter().any(|binding| {
                binding.name != name && KeyChord::parse(&binding.shortcut) == Some(chord)
            }) {
                return Err(format!("{chord} is assigned to another macro"));
            }
        }
        Ok(chord)
    }
    pub(super) fn finish_macro_recording(&mut self, steps: Vec<String>) {
        let mut number = self.settings.macros.len() + 1;
        while self
            .settings
            .macros
            .iter()
            .any(|entry| entry.0 == format!("Macro {number}"))
        {
            number += 1;
        }
        let name = format!("Macro {number}");
        let mut next = self.settings.clone();
        next.macros.push((name.clone(), steps.clone()));
        match persist(&next) {
            Ok(()) => {
                self.settings = next;
                self.set_status(format!("Recorded {name} — Edit → Macros → Edit Macros to change steps or bind a shortcut"));
            }
            Err(error) => {
                self.macro_recording = Some(steps);
                self.set_status(format!(
                    "Cannot save macro: {error}. Recording retained; stop again to retry."
                ));
            }
        }
    }
    pub(super) fn begin_macro_editor(&mut self, ctx: &egui::Context) {
        ctx.data_mut(|data| data.remove::<Draft>(draft_id()));
    }
    fn macro_draft(&self, selected: usize) -> Option<Draft> {
        let (name, steps) = self.settings.macros.get(selected)?;
        Some(Draft {
            selected,
            original: name.clone(),
            name: name.clone(),
            steps: serde_json::to_string_pretty(steps).ok()?,
            shortcut: self
                .settings
                .macro_shortcuts
                .iter()
                .find(|binding| &binding.name == name)
                .map(|binding| binding.shortcut.clone())
                .unwrap_or_default(),
            error: None,
        })
    }
    fn save_macro_draft(&mut self, draft: &Draft) -> Result<(), String> {
        let name = draft.name.trim();
        if name.is_empty() {
            return Err("Macro name cannot be empty".into());
        }
        if self
            .settings
            .macros
            .get(draft.selected)
            .is_none_or(|entry| entry.0 != draft.original)
        {
            return Err("This macro changed while it was being edited; reopen the editor".into());
        }
        if self
            .settings
            .macros
            .iter()
            .enumerate()
            .any(|(index, entry)| index != draft.selected && entry.0 == name)
        {
            return Err("Another macro already has that name".into());
        }
        let steps = parse_steps(&draft.steps)?;
        let shortcut = self
            .macro_chord(&draft.original, &draft.shortcut)?
            .map(|chord| chord.to_string());
        let mut next = self.settings.clone();
        next.macros[draft.selected] = (name.into(), steps);
        next.macro_shortcuts
            .retain(|binding| binding.name != draft.original);
        if let Some(shortcut) = shortcut {
            next.macro_shortcuts.push(MacroShortcut {
                name: name.into(),
                shortcut,
            });
        }
        persist(&next)?;
        self.settings = next;
        self.set_status(format!("Saved macro {name}"));
        Ok(())
    }
    fn delete_macro(&mut self, selected: usize) -> Result<(), String> {
        let Some((name, _)) = self.settings.macros.get(selected) else {
            return Err("Macro no longer exists".into());
        };
        let mut next = self.settings.clone();
        next.macros.remove(selected);
        next.macro_shortcuts.retain(|binding| &binding.name != name);
        persist(&next)?;
        self.settings = next;
        Ok(())
    }
    pub(super) fn macro_editor(
        &mut self,
        ctx: &egui::Context,
        mut selected: usize,
        new_name: String,
    ) {
        let mut draft = ctx.data(|data| data.get_temp::<Draft>(draft_id()));
        if draft
            .as_ref()
            .is_none_or(|draft| draft.selected != selected)
        {
            draft = self.macro_draft(selected);
        }
        let mut open = true;
        let (mut save, mut delete, mut play, mut cancel) = (false, false, false, false);
        egui::Window::new("Edit Macros").collapsible(false).resizable(true).default_size([680.0, 500.0]).anchor(Align2::CENTER_CENTER, Vec2::ZERO).open(&mut open).show_modal(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (index, (name, _)) in self.settings.macros.iter().enumerate() {
                    if ui.selectable_label(index == selected, name).clicked() { selected = index; }
                }
            });
            if let Some(draft) = &mut draft {
                ui.horizontal(|ui| { ui.label("Name"); ui.text_edit_singleline(&mut draft.name); });
                ui.horizontal(|ui| { ui.label("Shortcut"); ui.text_edit_singleline(&mut draft.shortcut); });
                ui.small("Shortcuts run this named macro while the editor has focus. Leave empty to remove the binding.");
                if let Err(error) = self.macro_chord(&draft.original, &draft.shortcut) { ui.colored_label(self.theme.warning, error); }
                ui.label("Steps (JSON strings, in playback order)");
                ui.small("Use text: for literal Unicode/newlines and action: for an IDE action ID. Move, add or remove entries in this list.");
                ui.add(egui::TextEdit::multiline(&mut draft.steps).code_editor().desired_rows(12).desired_width(f32::INFINITY));
                if let Some(error) = &draft.error { ui.colored_label(self.theme.warning, error); }
                ui.horizontal(|ui| {
                    save = ui.button("Save Macro").clicked();
                    play = ui.button("Play Saved").clicked();
                    delete = ui.button("Delete").clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            } else { ui.label("No macros recorded. Use Edit → Macros → Start/Stop Macro Recording."); }
        });
        if let Some(mut current) = draft.take() {
            if current.selected != selected {
                draft = self.macro_draft(selected);
            } else if save {
                match self.save_macro_draft(&current) {
                    Ok(()) => draft = self.macro_draft(selected),
                    Err(error) => {
                        current.error = Some(error);
                        draft = Some(current);
                    }
                }
            } else if delete {
                match self.delete_macro(selected) {
                    Ok(()) => {
                        selected = 0;
                        draft = self.macro_draft(selected);
                    }
                    Err(error) => {
                        current.error = Some(error);
                        draft = Some(current);
                    }
                }
            } else {
                if play {
                    if let Some((_, steps)) = self.settings.macros.get(selected).cloned() {
                        self.play_macro(steps, ctx);
                        open = false;
                    }
                }
                draft = Some(current);
            }
        }
        if selected
            != draft
                .as_ref()
                .map(|draft| draft.selected)
                .unwrap_or(selected)
        {
            draft = self.macro_draft(selected);
        }
        if open && !cancel && !ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            if let Some(draft) = draft {
                ctx.data_mut(|data| data.insert_temp(draft_id(), draft));
            }
            self.dialog = Dialog::Macros { selected, new_name };
        } else {
            self.begin_macro_editor(ctx);
            self.focus_editor = true;
        }
    }
    pub(super) fn macro_shortcut(&mut self, ui: &mut egui::Ui) -> bool {
        if !self.editor_keyboard()
            || self.find.visible
            || self.find_in_files.visible
            || self.nav.popup_visible()
            || self.intentions.is_some()
            || self.inplace_rename.is_some()
            || self.completion.visible
            || self.quick_doc.is_some()
            || self.run_panel.launcher.is_open()
            || self.coordinated_quit_active()
            || egui::Popup::is_any_open(ui.ctx())
        {
            return false;
        }
        let shortcuts = self
            .settings
            .macro_shortcuts
            .iter()
            .filter_map(|binding| {
                let chord = self.macro_chord(&binding.name, &binding.shortcut).ok()??;
                let matches = self
                    .settings
                    .macros
                    .iter()
                    .filter(|entry| entry.0 == binding.name)
                    .collect::<Vec<_>>();
                (matches.len() == 1).then(|| (chord, matches[0].1.clone()))
            })
            .collect::<Vec<_>>();
        let steps = ui.input_mut(|input| {
            let (index, steps) =
                input
                    .events
                    .iter()
                    .enumerate()
                    .find_map(|(index, event)| match event {
                        egui::Event::Key {
                            key,
                            physical_key,
                            pressed: true,
                            repeat: false,
                            modifiers,
                            ..
                        } if Keymap::primary_modifiers(*modifiers)
                            && self
                                .keymap
                                .lookup_key(*key, *physical_key, *modifiers)
                                .is_none() =>
                        {
                            let chord = KeyChord::new(*key, *modifiers);
                            shortcuts
                                .iter()
                                .find(|entry| entry.0 == chord)
                                .map(|entry| (index, entry.1.clone()))
                        }
                        _ => None,
                    })?;
            input.events.remove(index);
            Some(steps)
        });
        if let Some(steps) = steps {
            self.play_macro(steps, ui.ctx());
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::Caret;
    struct Sink;
    impl Sink {
        fn new(path: PathBuf) -> Self {
            SAVE_PATH.with(|slot| *slot.borrow_mut() = Some(path));
            Self
        }
    }
    impl Drop for Sink {
        fn drop(&mut self) {
            SAVE_PATH.with(|slot| *slot.borrow_mut() = None);
        }
    }
    fn fixture() -> (tempfile::TempDir, Sink, IdeApp, egui::Context) {
        let root = tempfile::tempdir().unwrap();
        let sink = Sink::new(root.path().join("settings.json"));
        let path = root.path().join("unicode.txt");
        std::fs::write(&path, "αβ").unwrap();
        let mut app = IdeApp::new_welcome(Settings::default());
        app.settings.completion.show_as_you_type = false;
        app.open_file(&path);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        app.focus_editor = true;
        frame(&mut app, &ctx, 0.0, vec![], false).drop_without_applying_deltas();
        frame(&mut app, &ctx, 0.02, vec![], false).drop_without_applying_deltas();
        (root, sink, app, ctx)
    }
    fn frame(
        app: &mut IdeApp,
        ctx: &egui::Context,
        time: f64,
        events: Vec<egui::Event>,
        dialog: bool,
    ) -> egui::FullOutput {
        ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                events,
                focused: true,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 750.0))),
                ..Default::default()
            },
            |ui| {
                app.global_shortcuts(ui);
                egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
                if dialog {
                    app.dialogs(ui.ctx());
                }
            },
        )
    }
    fn key(modifiers: egui::Modifiers) -> Vec<egui::Event> {
        vec![
            egui::Event::ModifiersChanged(modifiers),
            egui::Event::Key {
                key: egui::Key::F12,
                physical_key: Some(egui::Key::F12),
                pressed: true,
                repeat: false,
                modifiers,
            },
        ]
    }
    fn binding(name: &str) -> MacroShortcut {
        MacroShortcut {
            name: name.into(),
            shortcut: "Ctrl+Alt+Shift+F12".into(),
        }
    }
    fn label_rect(output: &egui::FullOutput, label: &str) -> Rect {
        output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == label => {
                    Some(text.galley.rect.translate(text.pos.to_vec2()))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing actual painted {label}"))
    }
    fn click(app: &mut IdeApp, ctx: &egui::Context, time: f64, rect: Rect) {
        let point = rect.center();
        frame(
            app,
            ctx,
            time,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            true,
        )
        .drop_without_applying_deltas();
        frame(
            app,
            ctx,
            time + 0.02,
            vec![egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            true,
        )
        .drop_without_applying_deltas();
    }
    #[test]
    fn actual_record_typing_action_stop_persists_and_last_playback_replays_unicode() {
        let (root, _sink, mut app, ctx) = fixture();
        app.handle_action(Action::StartStopMacroRecording, &ctx);
        frame(
            &mut app,
            &ctx,
            0.1,
            vec![egui::Event::Text("π".into())],
            false,
        )
        .drop_without_applying_deltas();
        frame(
            &mut app,
            &ctx,
            0.15,
            vec![egui::Event::Key {
                key: egui::Key::ArrowLeft,
                physical_key: Some(egui::Key::ArrowLeft),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            false,
        )
        .drop_without_applying_deltas();
        app.handle_action(Action::StartStopMacroRecording, &ctx);
        assert!(app.macro_recording.is_none());
        assert_eq!(app.settings.macros.len(), 1);
        assert_eq!(
            app.settings.macros[0].1,
            vec![
                "text:π".to_owned(),
                format!("action:{}", Action::CaretLeft.id())
            ]
        );
        let (loaded, _, error) =
            rustpy_settings::persist::load_from(&root.path().join("settings.json"));
        assert!(error.is_none());
        assert_eq!(loaded.macros, app.settings.macros);
        let i = app.active.unwrap();
        app.tabs[i].state.doc.reset("αβ");
        app.tabs[i]
            .state
            .set_carets(vec![Caret::at(TextSize::new(0))]);
        app.handle_action(Action::PlaybackLastMacro, &ctx);
        assert_eq!(app.tabs[i].state.doc.text(), "παβ");
        assert_eq!(app.tabs[i].state.primary().head(), TextSize::new(0));
        assert!(!app.macro_playing);
    }
    #[test]
    fn actual_step_editor_save_button_persists_rename_reordered_steps_and_named_binding() {
        let (root, _sink, mut app, ctx) = fixture();
        app.settings.macros = vec![("Original".into(), vec!["text:old".into()])];
        app.settings.macro_shortcuts = vec![binding("Original")];
        app.handle_action(Action::EditMacros, &ctx);
        frame(&mut app, &ctx, 0.1, vec![], true).drop_without_applying_deltas();
        // Draft storage belongs to the actual multiline editor; Save exercises
        // the same validation, persistence and publication as keyboard editing.
        ctx.data_mut(|data| {
            let mut draft = data.get_temp::<Draft>(draft_id()).unwrap();
            draft.name = "Renamed".into();
            draft.steps = serde_json::to_string(&vec![
                format!("action:{}", Action::CaretRight.id()),
                "text:😀\n".into(),
            ])
            .unwrap();
            data.insert_temp(draft_id(), draft);
        });
        let output = frame(&mut app, &ctx, 0.2, vec![], true);
        let save = label_rect(&output, "Save Macro");
        output.drop_without_applying_deltas();
        click(&mut app, &ctx, 0.3, save);
        assert_eq!(app.settings.macros[0].0, "Renamed");
        assert_eq!(app.settings.macros[0].1[1], "text:😀\n");
        assert_eq!(app.settings.macro_shortcuts, vec![binding("Renamed")]);
        let (loaded, _, error) =
            rustpy_settings::persist::load_from(&root.path().join("settings.json"));
        assert!(error.is_none());
        assert_eq!(loaded.macros, app.settings.macros);
        assert_eq!(loaded.macro_shortcuts, app.settings.macro_shortcuts);
        assert_eq!(app.tabs[app.active.unwrap()].state.doc.text(), "αβ");
    }
    #[test]
    fn actual_named_shortcut_selects_saved_macro_not_last_and_respects_modal_repeat_readonly() {
        let (_root, _sink, mut app, ctx) = fixture();
        app.settings.macros = vec![
            ("First".into(), vec!["text:π".into()]),
            ("Last".into(), vec!["text:WRONG".into()]),
        ];
        app.settings.macro_shortcuts = vec![binding("First")];
        let modifiers = egui::Modifiers {
            ctrl: true,
            alt: true,
            shift: true,
            ..Default::default()
        };
        frame(&mut app, &ctx, 0.1, key(modifiers), false).drop_without_applying_deltas();
        let i = app.active.unwrap();
        assert_eq!(app.tabs[i].state.doc.text(), "παβ");
        app.dialog = Dialog::GoToLine(String::new());
        frame(&mut app, &ctx, 0.2, key(modifiers), false).drop_without_applying_deltas();
        assert_eq!(app.tabs[i].state.doc.text(), "παβ");
        app.dialog = Dialog::None;
        // A modal render really surrenders editor focus. Reacquire through the
        // actual view before dispatch, whose global shortcut phase runs first.
        app.focus_editor = true;
        frame(&mut app, &ctx, 0.25, vec![], false).drop_without_applying_deltas();
        assert!(ctx.memory(|memory| memory.has_focus(app.tabs[i].view.id)));
        let mut repeated = key(modifiers);
        if let egui::Event::Key { repeat, .. } = &mut repeated[1] {
            *repeat = true;
        }
        frame(&mut app, &ctx, 0.3, repeated, false).drop_without_applying_deltas();
        assert_eq!(app.tabs[i].state.doc.text(), "παβ");
        frame(
            &mut app,
            &ctx,
            0.35,
            vec![egui::Event::Key {
                key: egui::Key::F12,
                physical_key: Some(egui::Key::F12),
                pressed: false,
                repeat: false,
                modifiers,
            }],
            false,
        )
        .drop_without_applying_deltas();
        let path = app.tabs[i].state.path.clone().unwrap();
        let permissions = std::fs::metadata(&path).unwrap().permissions();
        let mut read_only = permissions.clone();
        read_only.set_readonly(true);
        std::fs::set_permissions(&path, read_only).unwrap();
        app.refresh_file_permissions(i);
        assert!(app.tabs[i].state.doc.is_read_only());
        assert!(ctx.memory(|memory| memory.has_focus(app.tabs[i].view.id)));
        app.focus_editor = true;
        frame(&mut app, &ctx, 0.4, key(modifiers), false).drop_without_applying_deltas();
        std::fs::set_permissions(&path, permissions).unwrap();
        assert_eq!(app.tabs[i].state.doc.text(), "παβ");
        assert!(app.tabs[i].state.doc.is_read_only());
        assert!(matches!(app.dialog, Dialog::MakeWritable { .. }));
    }
    #[test]
    fn invalid_steps_collisions_and_failed_write_preserve_settings_and_draft() {
        let (root, _sink, mut app, _ctx) = fixture();
        app.settings.macros = vec![("Kept".into(), vec!["text:kept".into()])];
        let before = app.settings.clone();
        let mut draft = app.macro_draft(0).unwrap();
        draft.steps = r#"["text:partial","action:NotAnAction"]"#.into();
        assert!(app.save_macro_draft(&draft).is_err());
        assert_eq!(app.settings, before);
        draft.steps = r#"["text:next"]"#.into();
        draft.shortcut = "Ctrl+C".into();
        assert!(app.save_macro_draft(&draft).is_err());
        assert_eq!(app.settings, before);
        draft.shortcut = String::new();
        std::fs::create_dir(root.path().join("settings.json")).unwrap();
        assert!(app.save_macro_draft(&draft).is_err());
        assert_eq!(app.settings, before);
        assert_eq!(draft.steps, r#"["text:next"]"#);
    }
    #[test]
    fn cancellation_keeps_saved_steps_and_delete_removes_binding_without_resurrection() {
        let (_root, _sink, mut app, ctx) = fixture();
        app.settings.macros = vec![("Kept".into(), vec!["text:kept".into()])];
        app.settings.macro_shortcuts = vec![binding("Kept")];
        app.handle_action(Action::EditMacros, &ctx);
        frame(&mut app, &ctx, 0.1, vec![], true).drop_without_applying_deltas();
        ctx.data_mut(|data| {
            let mut draft = data.get_temp::<Draft>(draft_id()).unwrap();
            draft.steps = r#"["text:discard"]"#.into();
            data.insert_temp(draft_id(), draft);
        });
        let output = frame(&mut app, &ctx, 0.2, vec![], true);
        let cancel = label_rect(&output, "Cancel");
        output.drop_without_applying_deltas();
        click(&mut app, &ctx, 0.3, cancel);
        assert_eq!(app.settings.macros[0].1, vec!["text:kept"]);
        app.delete_macro(0).unwrap();
        assert!(app.settings.macros.is_empty());
        assert!(app.settings.macro_shortcuts.is_empty());
        assert!(validate_steps(&[format!("action:{}", Action::PlaybackLastMacro.id())]).is_err());
    }
    #[test]
    fn recording_save_failure_keeps_pending_steps_and_successful_retry_publishes_once() {
        let (root, _sink, mut app, ctx) = fixture();
        std::fs::create_dir(root.path().join("settings.json")).unwrap();
        app.handle_action(Action::StartStopMacroRecording, &ctx);
        frame(
            &mut app,
            &ctx,
            0.1,
            vec![egui::Event::Text("π".into())],
            false,
        )
        .drop_without_applying_deltas();
        app.handle_action(Action::StartStopMacroRecording, &ctx);
        assert!(app.settings.macros.is_empty());
        assert_eq!(
            app.macro_recording.as_ref().unwrap(),
            &vec!["text:π".to_owned()]
        );
        assert!(app
            .status
            .as_ref()
            .is_some_and(|(text, _)| text.contains("Cannot save macro")));
        std::fs::remove_dir(root.path().join("settings.json")).unwrap();
        app.handle_action(Action::StartStopMacroRecording, &ctx);
        assert!(app.macro_recording.is_none());
        assert_eq!(app.settings.macros.len(), 1);
        assert_eq!(app.settings.macros[0].1, vec!["text:π"]);
    }
}
