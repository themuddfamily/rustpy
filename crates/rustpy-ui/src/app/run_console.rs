//! Shared Run/Services console actions and retained-output exports.
use super::*;
use crate::a11y::DialogWindow;
#[derive(Default)]
pub(super) struct State {
    export: Option<Export>,
}
impl State {
    pub(super) fn modal_active(&self) -> bool {
        self.export.is_some()
    }
}
struct Export {
    text: String,
    path: String,
    error: Option<String>,
}
impl IdeApp {
    pub(super) fn run_console_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut export) = self.run_console_state.export.take() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        egui::Window::new("Export Run Console")
            .id(egui::Id::new("export-run-console"))
            .collapsible(false)
            .open(&mut open)
            .show_modal(ctx, |ui| {
                ui.label("Save console output to:");
                ui.add(egui::TextEdit::singleline(&mut export.path).desired_width(480.0));
                if let Some(error) = &export.error {
                    ui.colored_label(self.theme.warning, error);
                }
                if ui
                    .add_enabled(
                        !export.path.trim().is_empty(),
                        egui::Button::new("Save output"),
                    )
                    .clicked()
                {
                    save = true;
                }
            });
        if save {
            let path = PathBuf::from(export.path.trim());
            match std::fs::write(&path, &export.text) {
                Ok(()) => {
                    self.set_status(format!("Console exported to {}", path.display()));
                    open = false;
                }
                Err(error) => export.error = Some(error.to_string()),
            }
        }
        if open {
            self.run_console_state.export = Some(export);
        }
    }
    pub(super) fn handle_run_panel_event(&mut self, event: RunPanelEvent) {
        match event {
            RunPanelEvent::MenuAction(id, action) => self.custom_run_menu_action(id, action),
            RunPanelEvent::Rerun(id) => self.rerun_session(id),
            RunPanelEvent::Stop(id) => self.run_panel.request_stop(id),
            RunPanelEvent::Close(id) => self.run_panel.request_close(id),
            RunPanelEvent::OpenFile(path, line) => {
                let full = crate::run_panel::resolve_output_path(
                    &self
                        .project
                        .as_ref()
                        .map(|p| p.root.clone())
                        .unwrap_or_default(),
                    &path,
                );
                if full.is_file() {
                    self.mark_nav_jump();
                    self.open_file(&full);
                    if let Some(tab) = self
                        .opened_editor_index(&full)
                        .and_then(|i| self.tabs.get_mut(i))
                    {
                        tab.view.cancel_session_restore();
                        tab.state.go_to_line(line, None);
                    }
                }
            }
            RunPanelEvent::OpenUrl(url) => {
                if url.starts_with("https://") || url.starts_with("http://") {
                    self.open_url(&url);
                }
            }
            RunPanelEvent::SelectInProject(path) => {
                if let Some(project) = &mut self.project {
                    let full = crate::run_panel::resolve_output_path(&project.root, &path);
                    project.tree.selected = Some(full);
                }
            }
            RunPanelEvent::Attach(id) => {
                let target = self
                    .run_panel
                    .sessions
                    .iter()
                    .find(|s| s.id == id && s.is_running())
                    .and_then(|session| {
                        session.process_id().map(|pid| {
                            (
                                pid,
                                session
                                    .config
                                    .as_ref()
                                    .and_then(|cfg| cfg.interpreter.clone()),
                            )
                        })
                    });
                if let Some((pid, interpreter)) = target {
                    self.attach_run_process(
                        pid,
                        interpreter.or_else(|| {
                            self.project
                                .as_ref()
                                .and_then(|p| p.interpreter.as_ref().map(|i| i.path.clone()))
                        }),
                    );
                }
            }
            RunPanelEvent::Services(id) => self.open_process_service(id),
            RunPanelEvent::Hide => self.bottom = None,
            RunPanelEvent::Export(id) => {
                if let Some(text) = self.run_panel.retained_text(id) {
                    let path = self
                        .project
                        .as_ref()
                        .map(|project| project.root.join(format!("console-{id}.txt")))
                        .unwrap_or_else(|| PathBuf::from(format!("console-{id}.txt")));
                    self.run_console_state.export = Some(Export {
                        text,
                        path: path.display().to_string(),
                        error: None,
                    });
                }
            }
            RunPanelEvent::Gist(id) => {
                if let Some(text) = self.run_panel.retained_text(id) {
                    self.console_create_gist(text);
                }
            }
            RunPanelEvent::Print(id) => {
                let Some(text) = self.run_panel.retained_text(id) else {
                    return;
                };
                let directory = Settings::system_dir().join("console-prints");
                let path = directory.join(format!("run-{id}.html"));
                let escaped = escape_html(&text);
                let html = format!(
                    "<!doctype html><meta charset=utf-8><title>Run console</title><style>pre{{white-space:pre-wrap;font:12px monospace}}@media print{{button{{display:none}}}}</style><button onclick='window.print()'>Print</button><pre>{escaped}</pre><script>window.addEventListener('load',()=>window.print())</script>"
                );
                match std::fs::create_dir_all(directory).and_then(|_| std::fs::write(&path, html)) {
                    Ok(()) => self.open_url(&file_url(&path)),
                    Err(error) => self.set_status(format!("Cannot prepare console print: {error}")),
                }
            }
        }
    }
}
fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut encoded = String::from(if cfg!(windows) { "file:///" } else { "file://" });
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b':' | b'-' | b'_' | b'.') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn printing_escapes_output_and_file_url_encodes_paths() {
        assert_eq!(
            escape_html("<script>&\"'"),
            "&lt;script&gt;&amp;&quot;&#39;"
        );
        assert!(file_url(Path::new("/console logs/μ.html")).contains("%20"));
    }
}
