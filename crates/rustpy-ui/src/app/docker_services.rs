//! Host admission and hidden polling for the Services Docker snapshot owner.
use super::*;
impl IdeApp {
    /// Root calls this during ordinary updates, including while Services is hidden.
    /// It admits a context but never starts a CLI command; only the visible Docker
    /// node may start a refresh after the same authority has been checked.
    pub(super) fn poll_docker_services(&mut self, ctx: &egui::Context) {
        self.services_panel.docker.program = self.settings.docker_path.clone();
        let allowed = self.project.is_some()
            && self.builtin_plugin_enabled("docker")
            && !self.project_safe_mode();
        self.services_panel.docker_enabled = allowed;
        let root = self.project_root();
        self.services_panel.docker.set_admission(allowed, &root);
        self.poll_docker_services_retirement(ctx);
    }
    pub(super) fn docker_services_pending(&self) -> Option<(u64, String, String)> {
        self.services_panel.docker.pending()
    }
    pub(super) fn cancel_docker_services_project(&mut self) {
        self.services_panel.docker_enabled = false;
        self.services_panel.docker.cancel();
        let root = self.project_root();
        self.services_panel.docker.set_admission(false, &root);
    }
    pub(super) fn stop_docker_services(&mut self, id: u64, stamp: &str) {
        if !self.services_panel.docker.stop(id, stamp) {
            self.set_status("Docker snapshot changed; open Stop again");
        }
    }
    /// Frozen quit may revoke live authority and drains existing owners. It
    /// never grants a different context or starts a refresh.
    pub(super) fn poll_docker_services_retirement(&mut self, ctx: &egui::Context) {
        let root = self.project_root();
        if self.project.is_none()
            || !self.builtin_plugin_enabled("docker")
            || self.project_safe_mode()
            || !self
                .services_panel
                .docker
                .matches_admission(&root, &self.settings.docker_path)
        {
            self.services_panel.docker_enabled = false;
            self.services_panel.docker.set_admission(false, &root);
        }
        let now = ctx.input(|input| input.time);
        self.services_panel.docker.poll_retirement(now, ctx);
    }
}
