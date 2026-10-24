//! Each process retains its session, panel state, transport and stop ownership.
use super::*;
impl IdeApp {
    pub(super) fn attach_notebook_debugger(&mut self, host: String, port: u16) {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode {
            return;
        }
        let mut setup = self.debug_launch_setup();
        setup.breakpoints.extend(self.notebook_debug_breakpoints());
        match DebugSession::attach_endpoint(&host, port, setup.options, setup.breakpoints) {
            Ok(mut session) => {
                session.id = self.next_session_id;
                self.next_session_id += 1;
                session.name = "Notebook cell".into();
                self.retain_debug_session();
                self.debug = Some(session);
                self.bottom = Some(BottomTool::Debug);
            }
            Err(error) => self.set_status(format!("Cannot attach notebook debugger: {error}")),
        }
    }
    pub(super) fn attach_failed_test_debugger(&mut self, host: String, port: u16) {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode {
            return;
        }
        let setup = self.debug_launch_setup();
        match DebugSession::attach_endpoint(&host, port, setup.options, setup.breakpoints) {
            Ok(mut session) => {
                session.id = self.next_session_id;
                self.next_session_id += 1;
                session.name = "Failed test (postmortem)".into();
                self.retain_debug_session();
                self.debug = Some(session);
                self.bottom = Some(BottomTool::Debug);
            }
            Err(error) => self.set_status(format!("Cannot attach failed-test debugger: {error}")),
        }
    }
    pub(super) fn debug_sessions_quit_state(&self) -> String {
        format!(
            "{:?}",
            (
                self.debug
                    .as_ref()
                    .map(|session| (session.id, &session.state)),
                self.debug_sessions
                    .iter()
                    .map(|(session, panel)| (
                        session.id,
                        &session.state,
                        panel.setup_task.as_ref().map(|task| task.owner_id)
                    ))
                    .collect::<Vec<_>>(),
                self.debug_panel
                    .setup_task
                    .as_ref()
                    .map(|task| task.owner_id)
            )
        )
    }
    pub(super) fn debug_session_mut(&mut self, id: u64) -> Option<&mut DebugSession> {
        if self.debug.as_ref().is_some_and(|session| session.id == id) {
            return self.debug.as_mut();
        }
        self.debug_sessions
            .iter_mut()
            .find(|(session, _)| session.id == id)
            .map(|(session, _)| session)
    }
    pub(super) fn select_debug_session(&mut self, id: u64) -> bool {
        if self.debug.as_ref().is_some_and(|session| session.id == id) {
            return true;
        }
        let Some(index) = self
            .debug_sessions
            .iter()
            .position(|(session, _)| session.id == id)
        else {
            return false;
        };
        let (session, panel) = self.debug_sessions.remove(index);
        self.retain_debug_session();
        self.debug = Some(session);
        self.debug_panel = panel;
        self.exec_point = None;
        self.exec_point_column = None;
        self.debug_was_stopped = false;
        true
    }
    pub(super) fn retain_debug_session(&mut self) {
        if let Some(session) = self.debug.take() {
            let panel = std::mem::take(&mut self.debug_panel);
            self.debug_sessions.push((session, panel));
        }
    }
    pub(super) fn has_running_debug_configuration(&self, name: &str) -> bool {
        self.debug
            .as_ref()
            .is_some_and(|session| session.is_active() && session.config.name == name)
            || self
                .debug_sessions
                .iter()
                .any(|(session, _)| session.is_active() && session.config.name == name)
    }
    pub(super) fn poll_debug_sessions(&mut self, ctx: &egui::Context) {
        let restart = self
            .debug
            .as_ref()
            .zip(self.debug_panel.restart_requested)
            .filter(|(session, since)| {
                session.disconnect_completed() || since.elapsed().as_secs() >= 3
            })
            .map(|(session, _)| session.id)
            .or_else(|| {
                self.debug_sessions.iter().find_map(|(session, panel)| {
                    panel
                        .restart_requested
                        .filter(|since| {
                            session.disconnect_completed() || since.elapsed().as_secs() >= 3
                        })
                        .map(|_| session.id)
                })
            });
        if let Some(id) = restart {
            let old = if self.debug.as_ref().is_some_and(|session| session.id == id) {
                self.debug_panel.restart_requested = None;
                self.debug.take()
            } else {
                self.debug_sessions
                    .iter()
                    .position(|(session, _)| session.id == id)
                    .map(|index| self.debug_sessions.remove(index).0)
            };
            if let Some(old) = old {
                let cfg = old.config.clone();
                drop(old); // Release the captured session's endpoint lease before typed launch.
                self.launch_config(cfg, crate::run_context::RunMode::Debug);
            }
        }
        if self.debug_panel.restart_requested.is_some()
            || self
                .debug_sessions
                .iter()
                .any(|(_, panel)| panel.restart_requested.is_some())
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }

        if self.debug_breakpoints_snapshot != self.breakpoints {
            let paths = self
                .debug_breakpoints_snapshot
                .paths_differing(&self.breakpoints);
            for (session, _) in &mut self.debug_sessions {
                for path in &paths {
                    self.breakpoints.sync(path, session);
                }
                self.breakpoints.sync_global(session);
                session.options.backend.breakpoints = self
                    .breakpoints
                    .all()
                    .into_iter()
                    .map(|(path, line, bp)| {
                        rustpy_settings::debugger_workspace::AdvancedBreakpoint {
                            path,
                            line,
                            metadata: bp.metadata,
                        }
                    })
                    .collect();
            }
            if let Some(session) = &mut self.debug {
                session.options.backend.breakpoints = self
                    .breakpoints
                    .all()
                    .into_iter()
                    .map(|(path, line, bp)| {
                        rustpy_settings::debugger_workspace::AdvancedBreakpoint {
                            path,
                            line,
                            metadata: bp.metadata,
                        }
                    })
                    .collect();
            }
            self.debug_breakpoints_snapshot = self.breakpoints.clone();
        }
        let mut children = Vec::new();
        if let Some(parent) = &mut self.debug {
            for body in std::mem::take(&mut parent.backend.subprocesses) {
                children.push(parent.attach_subprocess(&body));
            }
        }
        let mut newly_stopped = None;
        for (index, (session, _)) in self.debug_sessions.iter_mut().enumerate() {
            let was_stopped = session.is_stopped();
            session.poll();
            if !was_stopped && session.is_stopped() {
                newly_stopped = Some(index);
            }

            for body in std::mem::take(&mut session.backend.subprocesses) {
                children.push(session.attach_subprocess(&body));
            }
            if session.is_active() {
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
        }
        if let Some(index) = newly_stopped {
            if self.settings.debugger.workspace.jump_to_source {
                let (session, panel) = self.debug_sessions.remove(index);
                self.retain_debug_session();
                self.debug = Some(session);
                self.debug_panel = panel;
                self.debug_was_stopped = false;
                self.exec_point = None;
                self.exec_point_column = None;
                self.bottom = Some(BottomTool::Debug);
            } else {
                self.set_status(format!("{} suspended", self.debug_sessions[index].0.name));
            }
        }
        for child in children {
            match child {
                Ok(mut session) => {
                    session.id = self.next_session_id;
                    self.next_session_id += 1;
                    self.debug_sessions.push((session, DebugPanel::default()));
                }
                Err(error) => self.set_status(format!("Cannot attach subprocess: {error}")),
            }
        }
    }
    pub(super) fn debug_session_tabs(&mut self, ui: &mut egui::Ui) {
        self.debug_tool_content_tabs(ui);
    }
}
