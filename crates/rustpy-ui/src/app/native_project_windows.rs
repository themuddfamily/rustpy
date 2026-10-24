//! The native shell owns viewport closure; each app still approves its own
//! exact documents/processes through the ordinary local and coordinated guards.
use super::*;
impl IdeApp {
    #[cfg(target_os = "macos")]
    pub(crate) fn bind_native_project_window(
        &mut self,
        binding: crate::launch::native_project_windows::Binding,
    ) {
        self.native_project_binding = Some(binding);
    }
    pub(crate) fn native_project_is_closing(&self) -> bool {
        self.native_project_binding
            .as_ref()
            .is_some_and(|binding| binding.is_closing())
    }
    #[cfg(target_os = "macos")]
    pub(crate) fn native_project_retirement_ui(&mut self, ui: &mut egui::Ui) {
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::CancelClose);
        ui.vertical_centered(|ui| {
            crate::a11y::spinner(ui);
            ui.label("Closing project window…");
            if let Some((message, _)) = &self.status {
                ui.label(message);
            }
        });
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(50));
    }
    pub(crate) fn native_project_close(&mut self, ctx: &egui::Context) -> bool {
        let Some(binding) = &self.native_project_binding else {
            return false;
        };
        binding.close(ctx);
        true
    }
    #[cfg(target_os = "macos")]
    pub(crate) fn native_project_viewport_begin(&self, ctx: &egui::Context) {
        self.theme.apply(ctx);
    }
    #[cfg(target_os = "macos")]
    pub(crate) fn request_native_host_quit(&mut self) {
        self.shell_global_quit_requested = true;
    }
    #[cfg(target_os = "macos")]
    pub(crate) fn native_project_finish_restart(&mut self) {
        self.finish_requested_restart();
    }
    #[cfg(target_os = "macos")]
    pub(crate) fn native_project_restart_pending(&self) -> bool {
        self.pending_update_restart() || self.pending_dialog_restart()
    }
    #[cfg(target_os = "macos")]
    pub(crate) fn native_project_open_paths(&mut self, paths: Vec<PathBuf>) {
        for path in paths {
            if path.is_dir() {
                self.request_open_project(&path);
            } else {
                self.open_file(&path);
            }
        }
    }
    pub(crate) fn take_macos_window_paths(&self, ctx: &egui::Context) -> Vec<PathBuf> {
        if self.native_project_binding.is_some() {
            Vec::new()
        } else {
            crate::macos_open::take_opened_paths(ctx)
        }
    }
    #[cfg(target_os = "macos")]
    pub(crate) fn native_project_report(&mut self, message: String) {
        self.set_status(message);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod retirement_fixtures {
    use super::*;
    #[test]
    fn retirement_ui_preserves_accepted_documents_without_reopening_editors() {
        let mut app = IdeApp::new(
            Settings {
                reopen_last_project: false,
                ..Default::default()
            },
            None,
        );
        app.push_tab(EditorState::from_text("unsaved = 'λ'\n", Language::Python));
        let before = app.tabs[0].state.doc.text();
        let version = app.tabs[0].state.doc.version();
        let ctx = egui::Context::default();
        let output = ctx.run_ui(Default::default(), |ui| {
            app.native_project_retirement_ui(ui)
        });
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "Closing project window…")));
        assert_eq!(app.tabs[0].state.doc.text(), before);
        assert_eq!(app.tabs[0].state.doc.version(), version);
        output.drop_without_applying_deltas();
    }
}
