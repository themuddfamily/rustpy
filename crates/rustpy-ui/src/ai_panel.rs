//! AI Assistant tool window: chat with editor context, one-shot code actions
//! (explain, refactor, tests, docstring, fix error) and code-block insertion.

use rustpy_ai::{AiConfig, AiEvent, ChatRequest, Message, Role, StreamHandle};

use crate::theme::UiTheme;

pub struct ChatTurn {
    pub role: Role,
    pub text: String,
    /// Short label for one-shot actions ("Explain Code", …) shown instead of the full prompt.
    pub label: Option<String>,
}

pub struct AiPanel {
    worker_owner: u64,
    pub visible: bool,
    pub turns: Vec<ChatTurn>,
    input: String,
    stream: Option<StreamHandle>,
    pub include_context: bool,
    status: Option<String>,
    focus_input: bool,
    /// Where finished replies go instead of the chat (e.g. the commit message box).
    pub redirect: Option<Redirect>,
    redirect_buf: String,
}

impl Default for AiPanel {
    fn default() -> Self {
        Self {
            worker_owner: rustpy_ai::new_worker_owner(),
            visible: false,
            turns: Vec::new(),
            input: String::new(),
            stream: None,
            include_context: true,
            status: None,
            focus_input: false,
            redirect: None,
            redirect_buf: String::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Redirect {
    CommitMessage,
}

pub enum AiPanelEvent {
    /// Send the typed message; the app adds editor context and calls [`AiPanel::start`].
    Send(String),
    InsertAtCaret(String),
    ReplaceSelection(String),
    OpenSettings,
}

impl AiPanel {
    pub fn worker_owner(&self) -> u64 {
        self.worker_owner
    }
    pub fn is_busy(&self) -> bool {
        self.stream.as_ref().is_some_and(|s| !s.is_finished())
    }

    /// Conversation so far (for follow-up questions).
    pub fn history(&self) -> Vec<Message> {
        self.turns
            .iter()
            .filter(|t| !t.text.is_empty())
            .map(|t| {
                if t.role == Role::User {
                    Message::user(t.text.clone())
                } else {
                    Message::assistant(t.text.clone())
                }
            })
            .collect()
    }

    /// Starts streaming `req`; `label` names one-shot actions in the transcript.
    pub fn start(
        &mut self,
        cfg: &AiConfig,
        req: ChatRequest,
        shown_prompt: String,
        label: Option<String>,
    ) {
        if let Some(s) = self.stream.take() {
            s.cancel();
        }
        self.visible = true;
        self.status = None;
        if self.redirect.is_none() {
            self.turns.push(ChatTurn {
                role: Role::User,
                text: shown_prompt,
                label,
            });
            self.turns.push(ChatTurn {
                role: Role::Assistant,
                text: String::new(),
                label: None,
            });
        }
        self.redirect_buf.clear();
        self.stream = Some(rustpy_ai::start_stream(cfg, req));
    }

    pub fn stop(&mut self) {
        if let Some(s) = self.stream.take() {
            s.cancel();
            self.status = Some("Stopped".into());
        }
    }

    /// Drains stream events; returns `(redirect, text)` when a redirected reply finished.
    pub fn poll(&mut self, ctx: &egui::Context) -> Option<(Redirect, String)> {
        let stream = self.stream.as_ref()?;
        let mut finished = None;
        for ev in stream.poll() {
            match ev {
                AiEvent::Delta(t) => {
                    if self.redirect.is_some() {
                        self.redirect_buf.push_str(&t);
                    } else if let Some(last) = self.turns.last_mut() {
                        last.text.push_str(&t);
                    }
                }
                AiEvent::Done {
                    output_tokens,
                    input_tokens,
                    stop_reason,
                } => {
                    let reason = stop_reason
                        .filter(|r| r != "end_turn")
                        .map(|r| format!(" · stopped: {r}"))
                        .unwrap_or_default();
                    self.status = Some(format!(
                        "{} in / {} out tokens{reason}",
                        input_tokens.unwrap_or(0),
                        output_tokens.unwrap_or(0)
                    ));
                    finished = Some(true);
                }
                AiEvent::Error(e) => {
                    self.status = Some(e);
                    finished = Some(false);
                }
            }
        }
        if finished.is_some() || stream.is_finished() {
            self.stream = None;
            if let Some(r) = self.redirect.take() {
                let text = rustpy_ai::strip_code_fence(&self.redirect_buf);
                if finished == Some(true) && !text.trim().is_empty() {
                    return Some((r, text.trim().to_string()));
                }
            }
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        None
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        cfg: &AiConfig,
        has_key: bool,
    ) -> Vec<AiPanelEvent> {
        let mut events = Vec::new();
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("AI Assistant").strong());
            ui.label(
                egui::RichText::new(rustpy_ai::model_label(cfg.model.as_str()))
                    .color(theme.text_dim)
                    .small(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("×").on_hover_text("Hide").clicked() {
                    self.visible = false;
                }
                if ui.small_button("New chat").clicked() {
                    self.stop();
                    self.turns.clear();
                    self.status = None;
                }
                if ui.small_button("Settings").clicked() {
                    events.push(AiPanelEvent::OpenSettings);
                }
            });
        });
        ui.separator();
        if !has_key {
            ui.label(egui::RichText::new("No API key for the selected provider. Configure its key in Settings › Tools › AI Assistant or its provider environment variable.").color(theme.warning));
            ui.separator();
        }
        let input_h = 96.0;
        let avail = ui.available_height() - input_h - 40.0;
        egui::ScrollArea::vertical().id_salt("ai_chat").max_height(avail.max(80.0)).stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
            if self.turns.is_empty() {
                ui.label(egui::RichText::new("Ask about your code. The current file or selection is attached when \"Use editor context\" is on. Editor › AI Actions offers Explain, Refactor, Write Tests, Docstring and Fix Error.").color(theme.text_dim));
            }
            let n = self.turns.len();
            for (i, t) in self.turns.iter().enumerate() {
                if t.role == Role::User {
                    egui::Frame::new().fill(theme.panel_bg_darker).corner_radius(6.0).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        match &t.label {
                            Some(l) => ui.label(egui::RichText::new(l).strong()),
                            None => ui.label(&t.text),
                        };
                    });
                } else {
                    let streaming = i + 1 == n && self.stream.is_some();
                    if t.text.is_empty() && streaming {
                        ui.label(egui::RichText::new("Thinking…").color(theme.text_dim));
                    } else {
                        crate::markdown_view::render_markdown(ui, theme, &t.text, 13.0);
                        if !streaming {
                            for (k, (lang, code)) in rustpy_ai::extract_code_blocks(&t.text).into_iter().enumerate() {
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new(format!("Code block {}{}", k + 1, lang.map(|l| format!(" ({l})")).unwrap_or_default())).color(theme.text_dim).small());
                                    if ui.small_button("Insert at caret").clicked() {
                                        events.push(AiPanelEvent::InsertAtCaret(code.clone()));
                                    }
                                    if ui.small_button("Replace selection").clicked() {
                                        events.push(AiPanelEvent::ReplaceSelection(code.clone()));
                                    }
                                    if ui.small_button("Copy").clicked() {
                                        ui.ctx().copy_text(code.clone());
                                    }
                                });
                            }
                        }
                    }
                }
                ui.add_space(6.0);
            }
        });
        if let Some(s) = &self.status {
            let is_err = !s.contains("tokens") && s != "Stopped";
            ui.label(
                egui::RichText::new(s)
                    .color(if is_err { theme.error } else { theme.text_dim })
                    .small(),
            );
        }
        ui.separator();
        let resp = ui.add(
            egui::TextEdit::multiline(&mut self.input)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text("Ask AI Assistant (Enter sends, Shift+Enter adds a line)"),
        );
        if self.focus_input {
            resp.request_focus();
            self.focus_input = false;
        }
        let enter =
            resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.include_context, "Use editor context");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.is_busy() {
                    if ui.button("Stop").clicked() {
                        self.stop();
                    }
                } else if (ui.button("Send").clicked() || enter) && !self.input.trim().is_empty() {
                    let text = self.input.trim().to_string();
                    self.input.clear();
                    events.push(AiPanelEvent::Send(text));
                }
            });
        });
        if enter && self.input.ends_with('\n') && !self.is_busy() {
            // Enter inserted a newline before we saw it; it was sent above.
            self.input = self.input.trim_end_matches('\n').to_string();
        }
        events
    }

    pub fn focus(&mut self) {
        self.visible = true;
        self.focus_input = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_skips_empty_turns() {
        let mut p = AiPanel::default();
        p.turns.push(ChatTurn {
            role: Role::User,
            text: "hi".into(),
            label: None,
        });
        p.turns.push(ChatTurn {
            role: Role::Assistant,
            text: String::new(),
            label: None,
        });
        let h = p.history();
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].role, Role::User);
    }
}
