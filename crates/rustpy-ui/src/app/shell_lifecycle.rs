//! Shell hooks distinguish File Exit from native Close Window. The shell owns
//! global orchestration; standalone apps keep their existing local close guards.
use super::IdeApp;
impl IdeApp {
    #[cfg(test)]
    pub(crate) fn testing_edit_active_document(&mut self, text: &str) {
        let active = self.active.expect("test requires an active editor");
        let document = &mut self.tabs[active].state.doc;
        let content = format!("{text}{}", document.text());
        document.set_text(&content, &[], &[]).unwrap();
    }
    #[cfg(test)]
    pub(crate) fn testing_handle_action(&mut self, action: crate::actions::Action) {
        self.handle_action(action, &egui::Context::default());
        self.testing_skip_quit_capture();
    }
    /// Closes the project-trust (or any other) question opened at startup.
    #[cfg(test)]
    pub(crate) fn testing_dismiss_dialog(&mut self) {
        self.dialog = super::Dialog::None;
    }
    #[cfg(test)]
    pub(crate) fn testing_run_sessions(&mut self) -> &mut Vec<rustpy_run::RunSession> {
        &mut self.run_panel.sessions
    }
    pub(crate) fn request_startup_window_close(&mut self) {
        self.request_exit();
    }
    /// A pending startup may close locally without enabling project mutations.
    /// The shell holds shared admission while these existing local guards run.
    pub(crate) fn startup_close_ui(&mut self, ctx: &egui::Context) {
        if ctx.input(|input| input.viewport().close_requested()) && !self.intercept_close(ctx) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.dialogs(ctx);
        self.draw_exit_prompt(ctx);
        if self.exit_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
    pub(crate) fn lifecycle_error(&mut self, title: &str, message: String) {
        self.notify_error("Project", title, message);
    }
    pub(crate) fn manage_shell_lifecycle(&mut self) {
        self.shell_managed_window = true;
    }
    pub(crate) fn take_global_quit_request(&mut self) -> bool {
        if self
            .shell_quit_after_frame
            .is_some_and(|frame| self.frame_count <= frame)
        {
            return false;
        }
        self.shell_quit_after_frame = None;
        std::mem::take(&mut self.shell_global_quit_requested)
    }
    /// Unit tests run no frames: skip waiting for the backdrop frame.
    #[cfg(test)]
    pub(crate) fn testing_skip_quit_capture(&mut self) {
        self.shell_quit_after_frame = None;
    }
    pub(crate) fn defer_coordinated_close(&mut self, defer: bool) {
        self.shell_defer_committed_close = defer;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::Action;
    use crate::app::Dialog;
    #[test]
    fn managed_file_exit_preserves_dirty_editor_for_global_guards_and_native_close_stays_local() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dirty.txt");
        std::fs::write(&path, "original").unwrap();
        let mut app = IdeApp::new_welcome(rustpy_settings::Settings::default());
        app.open_file(&path);
        app.testing_edit_active_document("dirty ");
        app.manage_shell_lifecycle();
        let ctx = egui::Context::default();
        app.handle_action(Action::Exit, &ctx);
        assert!(
            !app.take_global_quit_request(),
            "waits for the backdrop frame"
        );
        app.frame_count += 2;
        assert!(app.take_global_quit_request());
        assert!(!app.take_global_quit_request());
        assert!(!app.exit_requested);
        assert!(app.tabs[0].has_unsaved_changes());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
        assert!(matches!(app.dialog, Dialog::None));
        app.request_exit();
        assert!(!app.take_global_quit_request());
        assert!(!app.exit_requested);
        assert!(
            app.layout.exit_prompt.is_some()
                || matches!(
                    app.dialog,
                    Dialog::ConfirmClose {
                        then_exit: true,
                        ..
                    }
                )
        );
    }
}
