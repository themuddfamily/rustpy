//! Window-owned dialogs and settings jobs use captured TaskManager identities.
use super::*;

impl IdeApp {
    pub(super) fn window_operation_jobs(&self) -> Vec<(u64, String)> {
        let mut jobs = self.native_ui_jobs();
        jobs.extend(self.project_window_jobs());
        jobs.extend(self.appearance_asset_jobs());
        jobs.extend(self.dialog_workspace_pending());
        jobs
    }
    pub(super) fn stop_window_operation(&mut self, id: u64) {
        self.stop_project_window_job(id);
        self.stop_appearance_asset(id);
        if self
            .dialog_workspace_pending()
            .is_some_and(|job| job.0 == id)
        {
            self.cancel_dialog_workspace_job();
        }
        self.stop_native_ui_job(id);
    }
    pub(super) fn cancel_window_operations(&mut self) {
        self.cancel_project_windows();
        self.cancel_appearance_asset();
        self.cancel_native_ui();
        self.cancel_dialog_workspace_job();
    }
    pub(super) fn finish_requested_restart(&mut self) {
        if self.pending_update_restart() {
            self.cancel_dialog_restart();
            self.finish_update_restart();
        } else if self.pending_dialog_restart() {
            self.finish_dialog_restart();
        }
    }
    pub(crate) fn cancel_requested_restart(&mut self) {
        self.cancel_pending_update_restart();
        self.cancel_dialog_restart();
    }
}
