//! Editor actions that need the application (roadmap §1.2, §1.3, §1.6):
//! Cut / Copy / Paste run as actions (Edit menu, editor context menu, Find
//! Action and the Shift+Delete / Ctrl+Insert / Shift+Insert chords, which
//! egui only turns into clipboard events on Windows), macro recording of
//! actions the editor performs itself, and the docstring stub that Enter
//! inside `"""|"""` under a `def` inserts.

use super::*;

/// Text on the system clipboard (`None` when it is empty or unavailable).
fn read_system_clipboard() -> Option<String> {
    let mut cb = crate::system_clipboard::open().ok()?;
    cb.get_text()
        .ok()
        .map(|t| t.replace("\r\n", "\n"))
        .filter(|t| !t.is_empty())
}

impl IdeApp {
    /// Adds copied text to *Paste from History*.
    pub(super) fn remember_copied(&mut self, text: String) {
        self.clipboard_payload = None;
        if !text.is_empty() {
            self.clipboard_history.retain(|t| t != &text);
            self.clipboard_history.insert(0, text);
            self.clipboard_history.truncate(30);
        }
    }

    pub(super) fn record_clipboard_events(&mut self, events: &[ViewEvent]) {
        for event in events {
            match event {
                ViewEvent::Copied(text) => self.remember_copied(text.clone()),
                ViewEvent::ClipboardPayload(payload) => {
                    self.clipboard_payload = Some(payload.clone())
                }
                _ => {}
            }
        }
    }

    /// Text to paste: the system clipboard, else the latest copy inside the IDE.
    pub(super) fn clipboard_text(&self) -> Option<String> {
        read_system_clipboard().or_else(|| self.clipboard_history.first().cloned())
    }

    /// Cut / Copy / Paste on the active editor.
    pub(super) fn clipboard_action(&mut self, action: Action, ctx: &egui::Context) {
        let Some(i) = self.active.filter(|&i| i < self.tabs.len()) else {
            return;
        };
        match action {
            Action::Cut | Action::Copy => {
                let payload = self.tabs[i].state.copy_payload();
                let text = if action == Action::Cut {
                    self.tabs[i].state.cut()
                } else {
                    self.tabs[i].state.copy_text()
                };
                if text.is_empty() {
                    return;
                }
                ctx.copy_text(text.clone());
                self.remember_copied(text);
                self.clipboard_payload = Some(payload);
            }
            _ => {
                let Some(text) = self.clipboard_text() else {
                    self.set_status("The clipboard is empty");
                    return;
                };
                let text = if self.tabs[i].state.language == Language::Http
                    && text.trim_start().starts_with("curl ")
                {
                    rustpy_http::workspace::from_curl(text.trim())
                        .map(|request| rustpy_http::workspace::to_http(&request))
                        .unwrap_or(text)
                } else {
                    text
                };
                if text.len() > view::LARGE_PASTE_BYTES {
                    self.dialog = Dialog::ConfirmPaste(text);
                    return;
                }
                if action == Action::Paste && self.open_string_paste(i, &text) {
                    return;
                }
                let opts = self.edit_options();
                self.tabs[i]
                    .state
                    .paste_payload(&text, self.clipboard_payload.as_ref(), &opts);
            }
        }
        self.focus_editor = true;
    }

    /// A clipboard chord pressed while another widget (a text field) has the
    /// keyboard focus becomes egui's own clipboard event for that widget.
    pub(super) fn forward_clipboard_chord(
        ui: &mut egui::Ui,
        action: Action,
        key: egui::Key,
        mods: egui::Modifiers,
    ) {
        let event = match action {
            Action::Cut => Some(egui::Event::Cut),
            Action::Copy => Some(egui::Event::Copy),
            _ => read_system_clipboard().map(egui::Event::Paste),
        };
        ui.input_mut(|i| {
            if let Some(pos) = i.events.iter().position(|e| matches!(e, egui::Event::Key { key: k, pressed: true, modifiers: m, .. } if *k == key && *m == mods)) {
                match event {
                    Some(ev) => i.events[pos] = ev,
                    None => {
                        i.events.remove(pos);
                    }
                }
            }
        });
    }

    /// An action the editor view ran itself (keyboard or context menu).
    pub(super) fn editor_action_performed(&mut self, i: usize, action: Action) {
        self.learn_observe(action);
        self.macro_record_action(action);
        // Escape in the editor closes the find bar too (and still drops extra
        // carets and the selection).
        if action == Action::Escape && self.find.visible {
            self.close_find_bar();
        }
        if action == Action::Enter
            && self.settings.editor.insert_docstring_stub
            && self.active == Some(i)
            && self
                .tabs
                .get_mut(i)
                .is_some_and(|t| t.state.prepare_docstring_stub())
        {
            let version = self.tabs[i].state.doc.version();
            self.generate_docstring();
            if self.tabs[i].state.doc.version() != version {
                // Enter was pressed: continue on the summary line under the quotes.
                self.tabs[i].state.caret_to_docstring_summary();
            }
        }
    }
}

#[cfg(test)]
mod clipboard_payload_tests {
    use super::*;
    use crate::editor::Caret;
    fn actual_frame(app: &mut IdeApp, ctx: &egui::Context, time: f64, event: egui::Event) {
        let mut events = Vec::new();
        let output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                events: vec![event],
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(600.0, 300.0),
                )),
                ..Default::default()
            },
            |ui| {
                let opts = app.edit_options();
                let cfg = ViewConfig {
                    scheme: &app.scheme,
                    theme: &app.theme,
                    settings: &app.settings.editor,
                    opts: &opts,
                    keymap: &app.keymap,
                    font_size: 14.0,
                    search_highlights: &[],
                    request_focus: true,
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
                    keyboard: true,
                };
                let tab = &mut app.tabs[app.active.unwrap()];
                tab.view.clipboard_payload = app.clipboard_payload.clone();
                events = view::show(ui, &mut tab.state, &mut tab.view, &cfg);
            },
        );
        output.drop_without_applying_deltas();
        app.record_clipboard_events(&events);
    }
    #[test]
    fn actual_view_copy_and_cut_share_exact_payload_across_app_documents() {
        for cut in [false, true] {
            let mut app = IdeApp::new(Settings::default(), None);
            let mut source = EditorState::from_text("é\nβ--tail", Language::PlainText);
            source.set_carets(vec![
                Caret::new(TextSize::new(0), TextSize::new(5)),
                Caret::at(TextSize::new(9)),
            ]);
            app.push_tab(source);
            let ctx = egui::Context::default();
            crate::theme::install_fonts(&ctx);
            actual_frame(
                &mut app,
                &ctx,
                0.0,
                if cut {
                    egui::Event::Cut
                } else {
                    egui::Event::Copy
                },
            );
            let payload = app
                .clipboard_payload
                .clone()
                .expect("actual view copy event reaches shared App provenance");
            assert_eq!(payload.pieces.as_ref().unwrap(), &["é\nβ", ""]);
            let mut dest = EditorState::from_text("ab", Language::PlainText);
            dest.set_carets(vec![
                Caret::at(TextSize::new(0)),
                Caret::at(TextSize::new(2)),
            ]);
            app.push_tab(dest);
            app.active = Some(1);
            app.tabs[1].state.doc.set_read_only(true);
            actual_frame(
                &mut app,
                &ctx,
                0.1,
                egui::Event::Paste(payload.text.clone()),
            );
            assert_eq!(app.tabs[1].state.doc.text(), "ab");
            assert!(app.tabs[1].state.take_read_only_refusal());
            app.tabs[1].state.doc.set_read_only(false);
            actual_frame(&mut app, &ctx, 0.2, egui::Event::Paste(payload.text));
            assert_eq!(app.tabs[1].state.doc.text(), "é\nβab");
            assert!(app.tabs[1].state.undo());
            assert_eq!(app.tabs[1].state.doc.text(), "ab");
            assert!(!app.tabs[1].state.doc.can_undo());
            assert!(app.tabs[1].state.redo());
            assert_eq!(app.tabs[1].state.doc.text(), "é\nβab");
            app.remember_copied("generic external copy".into());
            assert!(app.clipboard_payload.is_none());
        }
    }
}
