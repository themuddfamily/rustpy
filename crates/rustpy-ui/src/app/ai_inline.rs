//! Explicit AI completion, anchored at the captured caret. Suggestions never
//! execute code and cannot apply after a document/caret/project change.
use super::*;
#[derive(Default)]
pub(super) struct State {
    request: Option<rustpy_ai::StreamHandle>,
    view: Option<egui::Id>,
    identity: Option<rustpy_text::DocumentId>,
    version: u64,
    caret: rustpy_text::TextSize,
    epoch: u64,
    text: String,
    ready: bool,
}
impl IdeApp {
    pub(super) fn stop_ai_workers(&mut self) {
        self.cancel_ai_inline();
        self.ai_panel.stop();
        rustpy_ai::cancel_workers(&self.ai_worker_ids());
    }
    pub(super) fn ai_worker_ids(&self) -> Vec<u64> {
        rustpy_ai::worker_ids_for_owner(self.ai_panel.worker_owner())
    }
    pub(super) fn cancel_ai_inline(&mut self) {
        self.ai_inline = State::default();
    }
    pub(super) fn request_ai_inline(&mut self) {
        if self.project_safe_mode() {
            self.set_status("Trust this project before sending AI context");
            return;
        }
        let Some(index) = self.active else {
            return;
        };
        let tab = &self.tabs[index];
        if tab.state.doc.is_read_only()
            || tab.notebook.is_some()
            || tab.image.is_some()
            || tab.state.is_large_file()
        {
            return;
        }
        let text = tab.state.doc.text();
        let caret = tab.state.primary().head();
        let offset: usize = caret.into();
        let before = &text[..offset];
        let after = &text[offset..];
        let prefix = rustpy_ai::truncate_middle(before, 12_000);
        let suffix = rustpy_ai::truncate_middle(after, 4_000);
        let request=rustpy_ai::ChatRequest{system:"Complete code at <CURSOR>. Return only the new text to insert, without Markdown, explanations or duplicated surrounding code. Do not issue commands or tool calls.".into(),messages:vec![rustpy_ai::Message::user(format!("Language: {}\n{prefix}<CURSOR>{suffix}",tab.state.language_label()))]};
        let mut cfg = self.ai_config();
        cfg.max_tokens = 2048;
        self.ai_inline = State {
            request: Some(rustpy_ai::start_stream(&cfg, request)),
            view: Some(tab.view.id),
            identity: Some(tab.state.doc.identity()),
            version: tab.state.doc.version(),
            caret,
            epoch: self.file_session,
            text: String::new(),
            ready: false,
        };
    }
    pub(super) fn ai_inline_ui(&mut self, ctx: &egui::Context) {
        if self.ai_inline.view.is_none() {
            return;
        }
        let valid = self
            .active
            .and_then(|i| self.tabs.get(i))
            .is_some_and(|tab| {
                Some(tab.view.id) == self.ai_inline.view
                    && Some(tab.state.doc.identity()) == self.ai_inline.identity
                    && tab.state.doc.version() == self.ai_inline.version
                    && tab.state.primary().head() == self.ai_inline.caret
                    && !tab.state.doc.is_read_only()
            })
            && self.file_session == self.ai_inline.epoch
            && !self.project_safe_mode();
        if !valid {
            self.cancel_ai_inline();
            return;
        }
        let events = self
            .ai_inline
            .request
            .as_ref()
            .map(|request| request.poll())
            .unwrap_or_default();
        for event in events {
            match event {
                rustpy_ai::AiEvent::Delta(text) => {
                    if self.ai_inline.text.len() + text.len() > 32_000 {
                        self.cancel_ai_inline();
                        self.set_status("AI completion exceeded size limit");
                        return;
                    }
                    self.ai_inline.text.push_str(&text);
                }
                rustpy_ai::AiEvent::Done { .. } => {
                    self.ai_inline.ready = true;
                    self.ai_inline.request = None;
                    self.ai_inline.text =
                        rustpy_ai::strip_code_fence(&self.ai_inline.text).to_string();
                }
                rustpy_ai::AiEvent::Error(error) => {
                    self.cancel_ai_inline();
                    self.set_status(error);
                    return;
                }
            }
        }
        if self.ai_inline.request.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        let Some(index) = self.active else {
            return;
        };
        let Some(rect) = self.tabs[index].view.caret_screen_rect else {
            return;
        };
        let mut accept = false;
        let mut dismiss = false;
        egui::Area::new(egui::Id::new("ai-inline-completion"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.right_bottom())
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(500.0);
                    if self.ai_inline.text.is_empty() {
                        ui.label("AI completion…");
                    } else {
                        ui.label(
                            egui::RichText::new(&self.ai_inline.text)
                                .monospace()
                                .color(self.theme.text_dim),
                        );
                    }
                    ui.horizontal(|ui| {
                        accept = ui
                            .add_enabled(
                                self.ai_inline.ready && !self.ai_inline.text.is_empty(),
                                egui::Button::new("Accept completion"),
                            )
                            .clicked();
                        dismiss = ui.button("Dismiss / Cancel").clicked();
                    });
                });
            });
        if accept {
            let text = self.ai_inline.text.clone();
            let caret = self.ai_inline.caret;
            self.tabs[index].state.apply_text_edits(
                vec![(rustpy_text::TextRange::empty(caret), text)],
                "AI inline completion",
            );
            self.cancel_ai_inline();
            self.focus_editor = true;
        } else if dismiss {
            self.cancel_ai_inline();
        }
    }
}

#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn completion_cannot_apply_after_document_revision() {
        let mut app = IdeApp::new(Settings::default(), None);
        app.push_tab(EditorState::from_text("code", Language::PlainText));
        let index = app.active.unwrap();
        let tab = &app.tabs[index];
        app.ai_inline = State {
            view: Some(tab.view.id),
            identity: Some(tab.state.doc.identity()),
            version: tab.state.doc.version(),
            caret: tab.state.primary().head(),
            epoch: app.file_session,
            text: "suggestion".into(),
            ready: true,
            request: None,
        };
        app.tabs[index].state.apply_text_edits(
            vec![(rustpy_text::TextRange::empty(0.into()), "changed".into())],
            "local edit",
        );
        let context = egui::Context::default();
        app.ai_inline_ui(&context);
        assert!(app.ai_inline.view.is_none());
        assert!(!app.tabs[index].state.doc.text().contains("suggestion"));
    }
}
