//! A captured clipboard choice is tied to one document incarnation and selection.
use super::*;
use crate::a11y::DialogWindow;
use crate::editor::{string_paste, Caret};

pub(super) struct Prompt {
    document: rustpy_text::DocumentId,
    version: u64,
    carets: Vec<Caret>,
    text: String,
    payload: Option<crate::editor::commands::clipboard_payload::ClipboardPayload>,
    plan: string_paste::StringPastePlan,
}
#[cfg(test)]
thread_local! {
    static TEST_CLIPBOARD: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}
#[derive(Copy, Clone)]
pub(super) enum Choice {
    Normal,
    Escaped,
    Raw,
    Cancel,
}
impl IdeApp {
    pub(super) fn open_string_paste(&mut self, index: usize, text: &str) -> bool {
        if !matches!(self.dialog, Dialog::None) {
            return false;
        }
        // Harmless literal text keeps the ordinary paste path without prompting.
        if !text
            .chars()
            .any(|c| c == '\\' || c == '\'' || c == '"' || c.is_control())
        {
            return false;
        }
        let Some(tab) = self.tabs.get(index) else {
            return false;
        };
        let Some(plan) = string_paste::prepare(&tab.state, text) else {
            return false;
        };
        self.dialog = Dialog::StringPaste(Prompt {
            document: tab.state.doc.identity(),
            version: tab.state.doc.version(),
            carets: tab.state.carets.clone(),
            text: text.to_owned(),
            payload: self.clipboard_payload.clone(),
            plan,
        });
        true
    }
    pub(super) fn string_paste_events(&mut self, index: usize, events: &[ViewEvent]) {
        for event in events {
            match event {
                ViewEvent::StringPaste(text) => {
                    self.open_string_paste(index, text);
                }
                ViewEvent::Action(Action::Paste) => self.paste_for_rendered_tab(index),
                _ => {}
            }
        }
    }
    pub(super) fn paste_for_rendered_tab(&mut self, index: usize) {
        #[cfg(test)]
        let captured = TEST_CLIPBOARD
            .with(|text| text.borrow().clone())
            .or_else(|| self.clipboard_text());
        #[cfg(not(test))]
        let captured = self.clipboard_text();
        let Some(text) = captured else {
            self.set_status("The clipboard is empty");
            return;
        };
        self.paste_captured_for_tab(index, &text);
    }
    fn paste_captured_for_tab(&mut self, index: usize, text: &str) {
        if index >= self.tabs.len() || !matches!(self.dialog, Dialog::None) {
            return;
        }
        if text.len() > view::LARGE_PASTE_BYTES {
            // The existing large-paste dialog applies to the active editor.
            self.active = Some(index);
            self.dialog = Dialog::ConfirmPaste(text.to_owned());
            return;
        }
        if self.open_string_paste(index, text) {
            return;
        }
        let opts = self.edit_options_for(index);
        self.tabs[index]
            .state
            .paste_payload(text, self.clipboard_payload.as_ref(), &opts);
        self.focus_editor = true;
    }
    fn finish_string_paste(&mut self, prompt: Prompt, choice: Choice) {
        if matches!(choice, Choice::Cancel) {
            self.focus_editor = true;
            return;
        }
        let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.state.doc.identity() == prompt.document)
        else {
            self.set_status("Paste target is no longer open");
            return;
        };
        let state = &self.tabs[index].state;
        if state.doc.version() != prompt.version
            || state.carets != prompt.carets
            || string_paste::prepare(state, &prompt.text).is_none()
        {
            self.set_status("Paste target changed; paste again");
            self.focus_editor = true;
            return;
        }
        let opts = self.edit_options_for(index);
        match choice {
            Choice::Normal => {
                self.tabs[index]
                    .state
                    .paste_payload(&prompt.text, prompt.payload.as_ref(), &opts)
            }
            Choice::Escaped | Choice::Raw => {
                let choice = if matches!(choice, Choice::Raw) {
                    prompt.plan.raw
                } else {
                    Some(prompt.plan.escaped)
                };
                if let Some(choice) = choice {
                    self.tabs[index].state.apply(
                        vec![choice.edit],
                        vec![Caret::at(choice.caret)],
                        rustpy_text::EditGroup::Separate,
                        Some("Paste"),
                    );
                }
            }
            Choice::Cancel => unreachable!(),
        }
        self.focus_editor = true;
    }
    pub(super) fn draw_string_paste(&mut self, ctx: &egui::Context, prompt: Prompt) {
        let mut selected = None;
        let mut open = true;
        let raw_available = prompt.plan.raw.is_some();
        ctx.input_mut(|input| {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                selected = Some(Choice::Cancel);
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::N) {
                selected = Some(Choice::Normal);
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::R) && raw_available {
                selected = Some(Choice::Raw);
            } else if input.consume_key(egui::Modifiers::NONE, egui::Key::E)
                || input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
            {
                selected = Some(Choice::Escaped);
            }
        });
        egui::Window::new("Paste into Python String")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .show_modal(ctx, |ui| {
                ui.label("Treat clipboard text as literal string content?");
                ui.label(format!(
                    "Value preview: {}",
                    prompt.plan.value.chars().take(80).collect::<String>()
                ));
                ui.label(
                    "Escaped preserves the pasted value; Raw converts the whole string when safe.",
                );
                ui.horizontal(|ui| {
                    if ui.button("Normal (N)").clicked() {
                        selected = Some(Choice::Normal);
                    }
                    if ui.button("Escaped (Enter / E)").clicked() {
                        selected = Some(Choice::Escaped);
                    }
                    if ui
                        .add_enabled(raw_available, egui::Button::new("Raw (R)"))
                        .clicked()
                    {
                        selected = Some(Choice::Raw);
                    }
                    if ui.button("Cancel (Esc)").clicked() {
                        selected = Some(Choice::Cancel);
                    }
                });
            });
        if !open {
            selected = Some(Choice::Cancel);
        }
        match selected {
            Some(choice) => self.finish_string_paste(prompt, choice),
            None => self.dialog = Dialog::StringPaste(prompt),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::view::{self, ViewConfig};
    fn frame(app: &mut IdeApp, ctx: &egui::Context, time: f64, events: Vec<egui::Event>) {
        ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 720.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                // Production ordering: keyboard owners run before editor input;
                // the real chooser paints after the editor and consumes its keys.
                app.global_shortcuts(ui);
                egui::CentralPanel::default().show(ui, |ui| {
                    let opts = app.edit_options();
                    let request_focus = std::mem::take(&mut app.focus_editor);
                    let cfg = ViewConfig {
                        scheme: &app.scheme,
                        theme: &app.theme,
                        settings: &app.settings.editor,
                        opts: &opts,
                        keymap: &app.keymap,
                        font_size: 14.0,
                        search_highlights: &[],
                        request_focus,
                        run_lines: &[],
                        line_changes: &[],
                        breakpoints: &[],
                        exec_line: None,
                        inline_values: &[],
                        bookmarks: &[],
                        gutter_markers: &[],
                        gutter_quick_fixes: &[],
                        code_vision: None,
                        annotations: None,
                        coverage: &[],
                        keyboard: app.editor_keyboard(),
                    };
                    let tab = &mut app.tabs[app.active.unwrap()];
                    let actions = view::show(ui, &mut tab.state, &mut tab.view, &cfg);
                    app.string_paste_events(app.active.unwrap(), &actions);
                    for action in actions {
                        if let view::ViewEvent::Action(action) = action {
                            app.handle_action(action, ui.ctx());
                        }
                    }
                });
                app.dialogs(ui.ctx());
            },
        )
        .drop_without_applying_deltas();
    }

    fn key(key: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
        vec![
            egui::Event::ModifiersChanged(modifiers),
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: true,
                repeat: false,
                modifiers,
            },
            egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed: false,
                repeat: false,
                modifiers,
            },
        ]
    }
    fn plain(keycode: egui::Key) -> Vec<egui::Event> {
        key(keycode, egui::Modifiers::NONE)
    }

    fn fixture() -> (IdeApp, egui::Context) {
        let mut app = IdeApp::new(Settings::default(), None);
        app.push_tab(EditorState::from_text("x = 'πEND'", Language::Python));
        app.tabs[0]
            .state
            .set_carets(vec![Caret::at(TextSize::new(7))]);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        frame(&mut app, &ctx, 0.0, vec![]);
        assert!(app.tabs[0].view.focused);
        (app, ctx)
    }
    fn paste(app: &mut IdeApp, ctx: &egui::Context) {
        frame(app, ctx, 0.1, vec![egui::Event::Paste("C:\temp".into())]);
        assert!(matches!(app.dialog, Dialog::StringPaste(_)));
        assert_eq!(app.tabs[0].state.doc.text(), "x = 'πEND'");
        assert!(!app.tabs[0].state.doc.can_undo());
    }
    #[test]
    fn real_paste_frame_escape_choice_is_one_undo_and_preserves_literal_value() {
        let (mut app, ctx) = fixture();
        paste(&mut app, &ctx);
        let selections = app.tabs[0].state.selections();
        frame(&mut app, &ctx, 0.2, plain(egui::Key::Enter));
        assert!(matches!(app.dialog, Dialog::None));
        assert_eq!(app.tabs[0].state.doc.text(), "x = 'πC:\\tempEND'");
        assert!(app.tabs[0].state.undo());
        assert_eq!(app.tabs[0].state.doc.text(), "x = 'πEND'");
        assert_eq!(app.tabs[0].state.selections(), selections);
        assert!(!app.tabs[0].state.doc.can_undo());
        assert!(app.tabs[0].state.redo());
        assert_eq!(app.tabs[0].state.doc.text(), "x = 'πC:\\tempEND'");
    }
    #[test]
    fn real_raw_choice_converts_whole_string_and_normal_choice_is_explicit() {
        let (mut app, ctx) = fixture();
        paste(&mut app, &ctx);
        frame(&mut app, &ctx, 0.2, plain(egui::Key::R));
        assert_eq!(app.tabs[0].state.doc.text(), "x = r'πC:\tempEND'");
        let (mut app, ctx) = fixture();
        paste(&mut app, &ctx);
        frame(&mut app, &ctx, 0.2, plain(egui::Key::N));
        assert_eq!(app.tabs[0].state.doc.text(), "x = 'πC:\tempEND'");
    }
    #[test]
    fn real_cancel_owns_keyboard_and_preserves_document_selection_history() {
        let (mut app, ctx) = fixture();
        paste(&mut app, &ctx);
        let selections = app.tabs[0].state.selections();
        frame(
            &mut app,
            &ctx,
            0.2,
            vec![egui::Event::Text("blocked".into())],
        );
        assert_eq!(app.tabs[0].state.doc.text(), "x = 'πEND'");
        frame(&mut app, &ctx, 0.3, plain(egui::Key::Escape));
        assert!(matches!(app.dialog, Dialog::None));
        assert_eq!(app.tabs[0].state.doc.text(), "x = 'πEND'");
        assert_eq!(app.tabs[0].state.selections(), selections);
        assert!(!app.tabs[0].state.doc.can_undo());
    }
    #[test]
    fn stale_version_or_selection_refuses_prepared_choice() {
        for text_mutation in [false, true] {
            let (mut app, ctx) = fixture();
            paste(&mut app, &ctx);
            if text_mutation {
                let opts = app.edit_options();
                app.tabs[0].state.insert_text("changed", &opts);
            } else {
                app.tabs[0]
                    .state
                    .set_carets(vec![Caret::at(TextSize::new(5))]);
            }
            let before = app.tabs[0].state.doc.text();
            let selections = app.tabs[0].state.selections();
            frame(&mut app, &ctx, 0.2, plain(egui::Key::Enter));
            assert!(matches!(app.dialog, Dialog::None));
            assert_eq!(app.tabs[0].state.doc.text(), before);
            assert_eq!(app.tabs[0].state.selections(), selections);
            assert!(app
                .status
                .as_ref()
                .is_some_and(|(message, _)| message.contains("changed")));
        }
    }
    #[test]
    fn replaced_document_or_new_readonly_guard_refuses_choice() {
        for replacement in [false, true] {
            let (mut app, ctx) = fixture();
            paste(&mut app, &ctx);
            if replacement {
                let carets = app.tabs[0].state.carets.clone();
                app.tabs[0].state = EditorState::from_text("x = 'πEND'", Language::Python);
                app.tabs[0].state.set_carets(carets);
            } else {
                app.tabs[0].state.doc.set_read_only(true);
            }
            frame(&mut app, &ctx, 0.2, plain(egui::Key::R));
            assert!(matches!(app.dialog, Dialog::None));
            assert_eq!(app.tabs[0].state.doc.text(), "x = 'πEND'");
            assert!(!app.tabs[0].state.doc.can_undo());
        }
    }
    fn context_frame(
        app: &mut IdeApp,
        ctx: &egui::Context,
        index: usize,
        time: f64,
        events: Vec<egui::Event>,
    ) -> Vec<egui::epaint::ClippedShape> {
        let output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 720.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                // Production ordering: keyboard owners run before editor input;
                // the real chooser paints after the editor and consumes its keys.
                app.global_shortcuts(ui);
                egui::CentralPanel::default().show(ui, |ui| {
                    let opts = app.edit_options();
                    let request_focus = true;
                    let cfg = ViewConfig {
                        scheme: &app.scheme,
                        theme: &app.theme,
                        settings: &app.settings.editor,
                        opts: &opts,
                        keymap: &app.keymap,
                        font_size: 14.0,
                        search_highlights: &[],
                        request_focus,
                        run_lines: &[],
                        line_changes: &[],
                        breakpoints: &[],
                        exec_line: None,
                        inline_values: &[],
                        bookmarks: &[],
                        gutter_markers: &[],
                        gutter_quick_fixes: &[],
                        code_vision: None,
                        annotations: None,
                        coverage: &[],
                        keyboard: app.editor_keyboard(),
                    };
                    let tab = &mut app.tabs[index];
                    let actions = view::show(ui, &mut tab.state, &mut tab.view, &cfg);
                    app.record_auxiliary_editor_events(index, &actions);
                    app.string_paste_events(index, &actions);
                    for action in actions {
                        if let view::ViewEvent::Action(action) = action {
                            assert_eq!(
                                action,
                                Action::Paste,
                                "fixture clicked actual Paste action"
                            );
                        }
                    }
                });
                app.dialogs(ui.ctx());
            },
        );
        let shapes = output.shapes.clone();
        output.drop_without_applying_deltas();
        shapes
    }

    #[test]
    fn actual_inactive_editor_context_menu_paste_targets_rendered_document() {
        let (mut app, ctx) = fixture();
        app.push_tab(EditorState::from_text("other = 'πEND'", Language::Python));
        app.tabs[1]
            .state
            .set_carets(vec![Caret::at(TextSize::new(11))]);
        app.active = Some(0);
        let active_before = app.tabs[0].state.doc.text();
        let _ = context_frame(&mut app, &ctx, 1, 0.1, vec![]);
        // Right-click moves the caret: use its rendered position inside the literal.
        let pointer = app.tabs[1].view.caret_screen_rect.unwrap().center();
        let _ = context_frame(
            &mut app,
            &ctx,
            1,
            0.2,
            vec![
                egui::Event::PointerMoved(pointer),
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Secondary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: pointer,
                    button: egui::PointerButton::Secondary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert_eq!(app.tabs[1].state.primary().head(), TextSize::new(11));
        let shapes = context_frame(&mut app, &ctx, 1, 0.25, vec![]);
        let paste = shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == Action::Paste.text() => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            })
            .expect("actual editor context menu must paint Paste");
        struct ClipboardGuard;
        impl Drop for ClipboardGuard {
            fn drop(&mut self) {
                TEST_CLIPBOARD.with(|text| *text.borrow_mut() = None);
            }
        }
        let _clipboard = ClipboardGuard;
        TEST_CLIPBOARD.with(|text| *text.borrow_mut() = Some("C:\\temp".into()));
        let _ = context_frame(
            &mut app,
            &ctx,
            1,
            0.3,
            vec![
                egui::Event::PointerMoved(paste),
                egui::Event::PointerButton {
                    pos: paste,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos: paste,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        TEST_CLIPBOARD.with(|text| *text.borrow_mut() = None);
        assert!(matches!(app.dialog, Dialog::StringPaste(_)));
        assert_eq!(app.tabs[0].state.doc.text(), active_before);
        assert_eq!(app.tabs[1].state.doc.text(), "other = 'πEND'");
        frame(&mut app, &ctx, 0.4, plain(egui::Key::R));
        assert_eq!(app.tabs[0].state.doc.text(), active_before);
        assert_eq!(app.tabs[1].state.doc.text(), "other = r'πC:\\tempEND'");
        assert!(app.tabs[1].state.undo());
        assert_eq!(app.tabs[1].state.doc.text(), "other = 'πEND'");
    }
}
