//! Right-side tools use the same host and placement authority as every dock.
use super::*;
impl IdeApp {
    pub(super) fn ai_tool_contents(&mut self, ui: &mut egui::Ui) {
        self.tool_window_gear(ui, layout::ToolRef::Ai);
        let theme = self.theme.clone();
        let cfg = self.ai_config();
        let has_key = cfg.resolved_key().is_some();
        let events = self.ai_panel.ui(ui, &theme, &cfg, has_key);
        for ev in events {
            match ev {
                crate::ai_panel::AiPanelEvent::Send(text) => {
                    let context = if self.ai_panel.include_context {
                        self.ai_editor_context().map(|(code, file, language, _)| {
                            rustpy_ai::ContextSnippet {
                                file,
                                selection: code,
                                language,
                            }
                        })
                    } else {
                        None
                    };
                    let history = self.ai_panel.history();
                    let req = rustpy_ai::chat(&history, &text, context);
                    self.ai_panel.redirect = None;
                    self.ai_panel.start(&cfg, req, text, None);
                }
                crate::ai_panel::AiPanelEvent::InsertAtCaret(code) => {
                    if self.non_text_source_hidden() {
                        self.set_status("Choose Source to insert AI text");
                    } else if let Some(i) = self.active {
                        let opts = self.edit_options();
                        self.tabs[i].state.paste(&code, &opts);
                        self.focus_editor = true;
                    }
                }
                crate::ai_panel::AiPanelEvent::ReplaceSelection(code) => {
                    if self.non_text_source_hidden() {
                        self.set_status("Choose Source to replace text");
                    } else if let Some(i) = self.active {
                        let st = &mut self.tabs[i].state;
                        let r = st.primary().range();
                        st.apply_text_edits(vec![(r, code)], "AI: replace selection");
                        self.focus_editor = true;
                    }
                }
                crate::ai_panel::AiPanelEvent::OpenSettings => {
                    self.settings_ui.page = settings_ui::SettingsPage::AiAssistant;
                    self.dialog = Dialog::Settings;
                }
            }
        }
    }
    pub(super) fn learn_tool_contents(&mut self, ui: &mut egui::Ui) {
        self.tool_window_gear(ui, layout::ToolRef::Learn);
        let theme = self.theme.clone();
        let keymap = self.keymap.clone();
        let ev = self.learn.ui(ui, &theme, &keymap);
        match ev {
            Some(crate::learn_panel::LearnEvent::Start(i)) => self.start_lesson(i),
            Some(crate::learn_panel::LearnEvent::Close) => self.learn.visible = false,
            None => {}
        }
        // Document-state expectations of the active lesson.
        if let Some(i) = self.active {
            let tab = &self.tabs[i];
            if tab
                .state
                .path
                .as_deref()
                .is_some_and(|p| self.learn.is_lesson_file(p))
                && self.learn_seen_version != Some(tab.state.doc.version())
            {
                self.learn_seen_version = Some(tab.state.doc.version());
                let text = tab.state.doc.text();
                if self.learn.observe_text(&text) {
                    self.lesson_completed();
                }
            }
        }
    }
}
