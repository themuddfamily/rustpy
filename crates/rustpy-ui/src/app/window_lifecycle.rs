//! Staged local participant in a coordinated Quit. Durable cohort verification
//! belongs to the shell's commit callback, never to transport enqueueing.
use super::*;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuitGuard {
    pub nonce: String,
    pub state: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuitProgress {
    PendingPersistence {
        message: String,
    },
    PendingProcesses {
        processes: Vec<String>,
    },
    PendingDocument {
        view_id: egui::Id,
        title: String,
        error: Option<String>,
    },
    Prepared {
        guard: QuitGuard,
    },
    Rejected {
        message: String,
    },
    Aborted,
    Committed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuitAnswer {
    Continue,
    Save,
    Discard,
    Cancel,
}
pub(super) struct WindowQuit {
    transaction: String,
    managed: bool,
    cancel_requested: bool,
    published: bool,
    processes: Vec<String>,
    consent: bool,
    consent_owners: String,
    discarded: HashMap<egui::Id, String>,
    guard: Option<QuitGuard>,
    error: Option<String>,
}
fn digest(bytes: &[u8]) -> String {
    rustpy_core::vfs::ContentRevision::of(bytes)
        .0
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
static NEXT_PROCESS_OWNER: AtomicU64 = AtomicU64::new(1);
pub(crate) fn next_process_owner() -> u64 {
    NEXT_PROCESS_OWNER.fetch_add(1, Ordering::Relaxed)
}
static NEXT_GUARD: AtomicU64 = AtomicU64::new(1);
/// Tags the screenshot taken for the Quit backdrop.
struct QuitBackdropShot;
pub(crate) fn is_quit_backdrop(user_data: &egui::UserData) -> bool {
    user_data
        .data
        .as_ref()
        .is_some_and(|data| data.is::<QuitBackdropShot>())
}

impl IdeApp {
    #[cfg(test)]
    pub(crate) fn testing_coordinated_quit_transaction(&self) -> Option<String> {
        self.coordinated_quit
            .as_ref()
            .map(|quit| quit.transaction.clone())
    }
    pub fn coordinated_quit_active(&self) -> bool {
        self.coordinated_quit.is_some()
    }
    pub fn prepare_coordinated_quit(&mut self, transaction: &str) -> QuitProgress {
        self.prepare_quit_participant(transaction, false)
    }
    /// Coordinator-managed Cancel retains the frozen participant until explicit Abort.
    pub fn prepare_managed_coordinated_quit(&mut self, transaction: &str) -> QuitProgress {
        self.prepare_quit_participant(transaction, true)
    }
    pub fn coordinated_quit_cancel_requested(&self, transaction: &str) -> bool {
        self.coordinated_quit.as_ref().is_some_and(|quit| {
            quit.transaction == transaction
                && quit.managed
                && quit.cancel_requested
                && !quit.published
        })
    }
    pub fn coordinated_quit_published(&self, transaction: &str) -> bool {
        self.coordinated_quit
            .as_ref()
            .is_some_and(|quit| quit.transaction == transaction && quit.published)
    }
    fn prepare_quit_participant(&mut self, transaction: &str, managed: bool) -> QuitProgress {
        if self.coordinated_quit_committed {
            return if self
                .coordinated_quit
                .as_ref()
                .is_some_and(|quit| quit.transaction == transaction)
            {
                QuitProgress::Committed
            } else {
                QuitProgress::Rejected {
                    message: "Committed quit transaction mismatch".into(),
                }
            };
        }
        if let Some(quit) = &self.coordinated_quit {
            if quit.transaction != transaction || quit.managed != managed {
                return QuitProgress::Rejected {
                    message: "Another quit transaction is active".into(),
                };
            }
            return self.poll_coordinated_quit(transaction);
        }
        if transaction.is_empty()
            || !matches!(self.dialog, Dialog::None | Dialog::DirectoryComparison)
            || self.layout.exit_prompt.is_some()
        {
            return QuitProgress::Rejected {
                message: "Finish the current dialog before quitting all windows".into(),
            };
        }
        let processes = self.quit_process_labels();
        let consent_owners = self.quit_process_stamp();
        self.coordinated_quit = Some(WindowQuit {
            transaction: transaction.into(),
            managed,
            cancel_requested: false,
            published: false,
            consent: !self.settings.confirm_exit && processes.is_empty(),
            processes,
            consent_owners,
            discarded: HashMap::new(),
            guard: None,
            error: None,
        });
        self.poll_coordinated_quit(transaction)
    }
    pub fn poll_coordinated_quit(&mut self, transaction: &str) -> QuitProgress {
        if self.coordinated_quit_committed {
            return if self
                .coordinated_quit
                .as_ref()
                .is_some_and(|quit| quit.transaction == transaction)
            {
                QuitProgress::Committed
            } else {
                QuitProgress::Rejected {
                    message: "Committed quit transaction mismatch".into(),
                }
            };
        }
        if self
            .coordinated_quit
            .as_ref()
            .is_some_and(|quit| quit.transaction == transaction)
        {
            if self.dialog_workspace_pending().is_some() {
                return QuitProgress::PendingPersistence {
                    message: "Finishing the cancelled settings or maintenance operation…".into(),
                };
            }
            if self.settings_sync_busy() {
                return QuitProgress::PendingPersistence {
                    message: "Finishing the cancelled settings operation…".into(),
                };
            }
            let retirement = self.network_security_retire_poll().and_then(|pending| {
                if !pending
                    && self
                        .coordinated_quit
                        .as_ref()
                        .is_some_and(|quit| quit.guard.is_none())
                {
                    self.finish_network_security_persistence()?;
                }
                Ok(pending)
            });
            match retirement {
                Ok(true) => {
                    return QuitProgress::PendingPersistence {
                        message: "Finishing pending password-store operations…".into(),
                    };
                }
                Ok(false) => {}
                Err(error) => {
                    // Managed participants reject readiness so the coordinator can
                    // abort its entire unpublished cohort. Local Quit resumes UI.
                    if self
                        .coordinated_quit
                        .as_ref()
                        .is_some_and(|quit| !quit.managed)
                    {
                        self.abort_coordinated_quit(transaction);
                        self.set_status(format!("Quit cancelled: {error}"));
                    }
                    return QuitProgress::Rejected {
                        message: format!("Password storage failed: {error}; cancel Quit and retry"),
                    };
                }
            }
        }
        if self
            .coordinated_quit
            .as_ref()
            .is_some_and(|quit| quit.transaction == transaction && quit.guard.is_none())
        {
            let owners = self.quit_process_stamp();
            let labels = self.quit_process_labels();
            let quit = self.coordinated_quit.as_mut().unwrap();
            if quit.consent_owners != owners {
                quit.consent_owners = owners;
                quit.processes = labels;
                if !quit.processes.is_empty() {
                    quit.consent = false;
                }
            }
        }
        let Some(quit) = &self.coordinated_quit else {
            return QuitProgress::Aborted;
        };
        if quit.transaction != transaction {
            return QuitProgress::Rejected {
                message: "Quit transaction mismatch".into(),
            };
        }
        if quit.managed && quit.cancel_requested && !quit.published {
            return QuitProgress::Rejected {
                message: "Quit cancellation requested".into(),
            };
        }
        // The shell freezes normal UI/callback publication while prepared.
        // Expensive observational state is rechecked explicitly at checkpoint/commit,
        // never copied merely to paint the modal.
        if let Some(guard) = &quit.guard {
            return QuitProgress::Prepared {
                guard: guard.clone(),
            };
        }
        if !quit.consent {
            return QuitProgress::PendingProcesses {
                processes: quit.processes.clone(),
            };
        }
        for tab in &self.tabs {
            if !quit.discarded.contains_key(&tab.view.id) && tab.has_unsaved_changes() {
                return QuitProgress::PendingDocument {
                    view_id: tab.view.id,
                    title: tab.state.title(),
                    error: quit.error.clone(),
                };
            }
        }
        // Check staged decisions only at readiness, before any workspace write.
        // Waiting for another document must not serialize discarded notebooks
        // on every modal paint, but a changed or missing editor still rejects Quit.
        for (view_id, stamp) in &quit.discarded {
            if !self
                .tabs
                .iter()
                .any(|tab| tab.view.id == *view_id && stamp == &Self::quit_tab_stamp(tab))
            {
                return QuitProgress::Rejected {
                    message: "An editor changed after its discard decision".into(),
                };
            }
        }
        if self.setup_workspace_choice_pending() {
            return QuitProgress::Rejected {
                message: "Resolve the pending interpreter choice before quitting".into(),
            };
        }
        self.persist_terminal_layout();
        if !self.checkpoint_task_context() {
            return QuitProgress::Rejected {
                message: "Task context persistence failed; cancel Quit and retry".into(),
            };
        }
        let persisted = self.save_workspace();
        let provisional = self
            .workspace_client
            .as_ref()
            .is_some_and(|client| client.quit_clean_provisional());
        if !persisted && !provisional {
            return QuitProgress::Rejected { message: "Workspace persistence failed; cancel Quit and retry or explicitly discard the pending choice".into() };
        }
        let guard = QuitGuard {
            nonce: digest(
                format!(
                    "{}:{}",
                    transaction,
                    NEXT_GUARD.fetch_add(1, Ordering::Relaxed)
                )
                .as_bytes(),
            ),
            state: self.coordinated_quit_stamp(),
        };
        self.coordinated_quit.as_mut().unwrap().guard = Some(guard.clone());
        QuitProgress::Prepared { guard }
    }
    pub fn answer_coordinated_quit(
        &mut self,
        transaction: &str,
        answer: QuitAnswer,
    ) -> QuitProgress {
        if answer == QuitAnswer::Cancel {
            if let Some(quit) = &mut self.coordinated_quit {
                if quit.transaction == transaction && quit.managed {
                    if quit.published {
                        return QuitProgress::Rejected {
                            message: "Quit checkpoint has already been published".into(),
                        };
                    }
                    quit.cancel_requested = true;
                    return QuitProgress::Rejected {
                        message: "Quit cancellation requested".into(),
                    };
                }
            }
            return self.abort_coordinated_quit(transaction);
        }
        match self.poll_coordinated_quit(transaction) {
            QuitProgress::PendingProcesses { .. } if answer == QuitAnswer::Continue => {
                self.coordinated_quit.as_mut().unwrap().consent = true;
            }
            QuitProgress::PendingDocument { view_id, .. }
                if matches!(answer, QuitAnswer::Save | QuitAnswer::Discard) =>
            {
                let Some(index) = self.tabs.iter().position(|tab| tab.view.id == view_id) else {
                    return QuitProgress::Rejected {
                        message: "Editor disappeared".into(),
                    };
                };
                if answer == QuitAnswer::Discard {
                    let stamps = self
                        .tabs
                        .iter()
                        .filter(|tab| tab.state.doc.shares_authority(&self.tabs[index].state.doc))
                        .map(|tab| (tab.view.id, Self::quit_tab_stamp(tab)))
                        .collect::<Vec<_>>();
                    self.coordinated_quit
                        .as_mut()
                        .unwrap()
                        .discarded
                        .extend(stamps);
                } else if !self.save_tab(index) {
                    self.coordinated_quit.as_mut().unwrap().error = Some(
                        self.status
                            .as_ref()
                            .map(|(message, _)| message.clone())
                            .unwrap_or_else(|| "Save failed".into()),
                    );
                } else {
                    self.coordinated_quit.as_mut().unwrap().error = None;
                }
            }
            progress => return progress,
        }
        self.poll_coordinated_quit(transaction)
    }
    /// A managed coordinator must privately establish that no durable checkpoint
    /// was published before calling Abort. An unknown CAS outcome is not proof.
    pub fn abort_coordinated_quit(&mut self, transaction: &str) -> QuitProgress {
        if self.coordinated_quit_committed {
            return if self
                .coordinated_quit
                .as_ref()
                .is_some_and(|quit| quit.transaction == transaction)
            {
                QuitProgress::Committed
            } else {
                QuitProgress::Rejected {
                    message: "Committed quit transaction mismatch".into(),
                }
            };
        }
        if self
            .coordinated_quit
            .as_ref()
            .is_some_and(|quit| quit.transaction != transaction)
        {
            return QuitProgress::Rejected {
                message: "Quit transaction mismatch".into(),
            };
        }
        if self
            .coordinated_quit
            .as_ref()
            .is_some_and(|quit| quit.published)
        {
            return QuitProgress::Rejected {
                message: "Published quit cannot be aborted".into(),
            };
        }
        // The coordinator must privately prove no durable publication before Abort.
        self.coordinated_quit = None;
        self.cancel_pending_update_restart();
        self.cancel_dialog_restart();
        self.restore_directory_comparison();
        QuitProgress::Aborted
    }
    pub fn validate_coordinated_quit(
        &self,
        transaction: &str,
        guard: &QuitGuard,
    ) -> Result<(), String> {
        let quit = self.coordinated_quit.as_ref().ok_or("No prepared quit")?;
        if quit.transaction != transaction
            || quit.guard.as_ref() != Some(guard)
            || guard.state != self.coordinated_quit_stamp()
        {
            return Err("Prepared window guard no longer matches".into());
        }
        Ok(())
    }
    /// The callback must privately verify the exact durable transaction, complete
    /// cohort and this guard. Unknown publication outcomes must not authorize Abort.
    /// A publication that wins a late Cancel remains eligible for verified Commit.
    pub fn seal_coordinated_quit(
        &mut self,
        transaction: &str,
        guard: &QuitGuard,
        verify_checkpoint: impl FnOnce(&str, &QuitGuard) -> Result<(), String>,
    ) -> Result<(), String> {
        self.validate_coordinated_quit(transaction, guard)?;
        if !self.coordinated_quit.as_ref().unwrap().managed {
            return Err("Only managed quit can be sealed".into());
        }
        verify_checkpoint(transaction, guard)?;
        self.validate_coordinated_quit(transaction, guard)?;
        self.coordinated_quit.as_mut().unwrap().published = true;
        Ok(())
    }
    /// The shell must verify the durable, transaction-bound complete cohort
    /// certificate inside `verify_checkpoint`; transport acceptance is insufficient.
    pub fn commit_coordinated_quit(
        &mut self,
        transaction: &str,
        guard: &QuitGuard,
        verify_checkpoint: impl FnOnce(&str, &QuitGuard) -> Result<(), String>,
    ) -> Result<(), String> {
        if self.coordinated_quit_committed {
            return if self.coordinated_quit.as_ref().is_some_and(|quit| {
                quit.transaction == transaction && quit.guard.as_ref() == Some(guard)
            }) {
                Ok(())
            } else {
                Err("Committed quit transaction mismatch".into())
            };
        }
        self.validate_coordinated_quit(transaction, guard)?;
        if self.network_security_pending() {
            return Err("Password-store operations must finish before Quit commits".into());
        }
        if self.settings_sync_busy() {
            return Err("Settings synchronization must finish before Quit commits".into());
        }
        if self.dialog_workspace_pending().is_some() {
            return Err(
                "Settings and maintenance operations must finish before Quit commits".into(),
            );
        }
        verify_checkpoint(transaction, guard)?;
        self.validate_coordinated_quit(transaction, guard)?;
        self.coordinated_quit_committed = true;
        self.stop_task_operation();
        self.terminate_processes();
        self.test_panel.stop_quit_process();
        self.py_console.stop_quit_process();
        self.packages_panel.stop_quit_process();
        for tab in &mut self.tabs {
            if let Some(notebook) = &mut tab.notebook {
                notebook.stop_quit_process();
            }
        }
        self.cancel_startup_terminal();
        self.cancel_project_tasks();
        self.data_files.cancel();
        self.cancel_interpreter_management();
        self.git_panel.branch_workspace.cancel_jobs();
        self.github.cancel();
        if let Some(setup) = &mut self.pending_venv {
            setup.stop_quit_process();
        }
        self.pending_venv = None;
        if let Some((_, mut setup, task)) = self.settings_ui.venv_rx.take() {
            setup.cancel();
            task.cancel();
        }
        self.exit_requested = true;
        Ok(())
    }
    /// Physical close waits for every live tab and every dropped terminal owner.
    /// Stop is a request; reader drain and child retirement remain observable.
    pub(super) fn terminal_retirement_pending(&mut self, ctx: &egui::Context) -> bool {
        let live = self
            .terminal_panel
            .tabs
            .iter_mut()
            .any(|tab| tab.terminal.is_alive());
        let retained = !rustpy_terminal::poll_retiring_terminals().is_empty();
        if live || retained {
            ctx.request_repaint_after(std::time::Duration::from_millis(25));
        }
        live || retained
    }
    /// Before this window freezes for a Quit: capture the next ordinary frame,
    /// so the prompt is shown over the IDE instead of an empty window.
    pub(crate) fn request_quit_backdrop(&mut self) {
        self.quit_backdrop_wanted = true;
    }
    pub(crate) fn has_quit_backdrop(&self) -> bool {
        self.quit_backdrop.is_some()
    }
    /// Captures the frame being painted (eframe reads it back after painting).
    pub(super) fn send_quit_backdrop_capture(ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(
            QuitBackdropShot,
        )));
    }
    /// Ordinary frames: send a requested capture, keep an arriving one, and
    /// otherwise drop a stale backdrop.
    pub(super) fn capture_quit_backdrop(&mut self, ctx: &egui::Context) {
        if std::mem::take(&mut self.quit_backdrop_wanted) {
            Self::send_quit_backdrop_capture(ctx);
        }
        if !self.take_quit_backdrop_shot(ctx) {
            self.quit_backdrop = None;
        }
    }
    fn take_quit_backdrop_shot(&mut self, ctx: &egui::Context) -> bool {
        let shot = ctx.input(|input| {
            input.events.iter().find_map(|event| match event {
                egui::Event::Screenshot {
                    user_data, image, ..
                } if is_quit_backdrop(user_data) => Some(image.clone()),
                _ => None,
            })
        });
        let Some(image) = shot else {
            return false;
        };
        self.quit_backdrop = Some(ctx.load_texture(
            "coordinated-quit-backdrop",
            (*image).clone(),
            egui::TextureOptions::LINEAR,
        ));
        true
    }
    /// The frozen IDE behind the Quit prompt (the modal dims it), or at least
    /// the panel colour while no capture is available.
    pub(crate) fn paint_quit_backdrop(&mut self, ctx: &egui::Context) {
        self.take_quit_backdrop_shot(ctx);
        let rect = ctx.content_rect();
        let painter = ctx.layer_painter(egui::LayerId::background());
        painter.rect_filled(rect, 0.0, self.theme.panel_bg);
        if let Some(texture) = &self.quit_backdrop {
            painter.image(
                texture.id(),
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
    }
    pub fn coordinated_quit_guard_ui(&mut self, ctx: &egui::Context) {
        self.paint_quit_backdrop(ctx);
        let terminals_pending = self.terminal_retirement_pending(ctx);
        self.cancel_dialog_workspace_job();
        self.poll_dialog_workspace_retirement(ctx);
        let native_ui_pending = self.retire_native_ui(ctx);
        let project_windows_pending = self.retire_project_windows(ctx);
        let appearance_pending = self.retire_appearance_asset(ctx);
        let plugins_pending = self.retire_plugin_runtime(ctx);
        self.poll_docker_services_retirement(ctx);
        // Ordinary shortcut dispatch is bypassed while this modal owns input.
        // Cancel must not revive a Shift tap from before the consent prompt.
        self.search_everywhere_gesture.reset();
        if self.settings_sync_busy() {
            self.stop_all_settings_sync();
            self.poll_settings_sync_retirement(ctx);
        }
        if !self.coordinated_quit_committed
            && self
                .coordinated_quit
                .as_ref()
                .is_some_and(|quit| quit.guard.is_none())
        {
            self.poll_remote_development_quit_io(ctx);
        }
        if self.coordinated_quit_committed {
            self.poll_remote_development_retirement(ctx);
            self.poll_branch_workspace_jobs(ctx);
            self.poll_data_files(ctx);
            self.poll_web_languages(ctx);
            self.db_panel.poll_database();
            self.poll_update_retirement(ctx);
            self.deploy.poll_retirement(ctx);
            self.poll_github(ctx);
            self.poll_svg_previews(ctx);
            self.poll_directory_comparison(ctx);
            self.poll_native_skeletons(ctx);
            self.poll_run_workflows(ctx);
            let notebooks_pending = self.poll_notebook_retirement(ctx);
            self.diagram_panel.cancel_capture();
            let diagram_pending = self.diagram_panel.poll_capture();
            let diagnostic_capture_pending = self.poll_ide_capture(ctx, true);
            let diagnostic_export_pending = self.diagnostic_export.retire();
            if self.shell_defer_committed_close
                || terminals_pending
                || self.update_transfer_state().is_some()
                || self.deploy.is_busy()
                || notebooks_pending
                || diagnostic_capture_pending
                || diagram_pending
                || diagnostic_export_pending
                || self.web_languages.is_running()
                || plugins_pending
                || native_ui_pending
                || project_windows_pending
                || appearance_pending
                || self.docker_services_pending().is_some()
                || self.dialog_workspace_pending().is_some()
                || self.poll_debug_transport_retirement(ctx)
                || !project_dialogs::abandoned_clone_cleanup_pending().is_empty()
                || !self.remote_development_pending().is_empty()
                || self.collaboration_running()
                || !self.ai_worker_ids().is_empty()
                || self.settings_sync_busy()
                || self.data_files.busy()
                || self.db_panel.database_has_owners()
                || self.github.busy()
                || !self.git_panel.branch_workspace.pending.is_empty()
                || self.svg_jobs.is_active()
                || self.web_preview_pending()
                || (self.directory_comparisons_pending()
                    || self.native_skeletons_pending()
                    || self.run_workflows_pending())
            {
                egui::Modal::new(egui::Id::new("coordinated-quit-retiring")).show(ctx, |ui| {
                    ui.heading("Closing RustPy…");
                    ui.label("Waiting for owned processes and background requests to finish");
                });
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.request_repaint_after(std::time::Duration::from_millis(25));
            } else {
                if self.native_project_close(ctx) {
                    return;
                }
                crate::netsec::shutdown_prompts();
                self.finish_requested_restart();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }
        if self.coordinated_quit_active() && ctx.input(|input| input.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        let Some(transaction) = self
            .coordinated_quit
            .as_ref()
            .map(|quit| quit.transaction.clone())
        else {
            return;
        };
        let progress = self.poll_coordinated_quit(&transaction);
        let cancellation_enabled = !self.coordinated_quit_published(&transaction);
        let mut answer = None;
        egui::Modal::new(egui::Id::new("coordinated_quit")).show(ctx, |ui| {
            ui.heading("Quit all RustPy windows");
            match &progress {
                QuitProgress::PendingPersistence { message } => {
                    ui.label(message);
                    ctx.request_repaint_after(std::time::Duration::from_millis(25));
                }
                QuitProgress::PendingProcesses { processes } => {
                    ui.label(if processes.len() == 1 {
                        "A process is still running:".to_string()
                    } else {
                        format!("{} processes are still running:", processes.len())
                    });
                    for process in processes {
                        ui.label(format!("  • {process}"));
                    }
                    ui.label("Terminate them and exit?");
                    if ui.button("Terminate").clicked() {
                        answer = Some(QuitAnswer::Continue);
                    }
                }
                QuitProgress::PendingDocument { title, error, .. } => {
                    ui.label(format!("Save changes to {title}?"));
                    if let Some(error) = error {
                        ui.colored_label(egui::Color32::RED, error);
                    }
                    if ui.button("Save").clicked() {
                        answer = Some(QuitAnswer::Save);
                    }
                    if ui.button("Don't Save").clicked() {
                        answer = Some(QuitAnswer::Discard);
                    }
                }
                QuitProgress::Prepared { .. } => {
                    ui.label("Waiting for the other windows and the saved checkpoint…");
                }
                QuitProgress::Rejected { message } => {
                    ui.label(message);
                }
                _ => {}
            }
            if ui
                .add_enabled(cancellation_enabled, egui::Button::new("Cancel"))
                .clicked()
            {
                answer = Some(QuitAnswer::Cancel);
            }
        });
        if cancellation_enabled && ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            answer = Some(QuitAnswer::Cancel);
        }
        if let Some(answer) = answer {
            self.answer_coordinated_quit(&transaction, answer);
        }
    }
    fn quit_tab_stamp(tab: &Tab) -> String {
        format!(
            "{:?}",
            (
                tab.view.id,
                &tab.state.path,
                tab.group,
                tab.pinned,
                tab.preview,
                tab.state.doc.quit_guard_state(),
                &tab.state.carets,
                tab.view.session_position(&tab.state),
                tab.notebook.as_ref().map(|notebook| (
                    notebook.to_json(),
                    notebook.is_dirty(),
                    notebook.synced_version()
                ))
            )
        )
    }
    fn quit_process_labels(&self) -> Vec<String> {
        let mut labels: Vec<_> = self
            .run_panel
            .sessions
            .iter()
            .filter(|session| session.owns_process())
            .map(|session| format!("Run '{}'", session.name))
            .collect();
        labels.extend(
            rustpy_terminal::poll_retiring_terminals()
                .into_iter()
                .map(|stamp| format!("Terminal cleanup: {stamp}")),
        );
        labels.extend(
            self.debug_transport_pending()
                .into_iter()
                .map(|(_, name)| name),
        );
        if let Some(job) = self.git_panel.log_job_label() {
            labels.push(format!("Git Log: {job}"));
        }
        if !self.git_panel.branch_workspace.pending.is_empty() {
            labels.push("Branch workspace operation".into());
        }
        if self.data_files.busy() {
            labels.push("Data-file operation".into());
        }
        if self.github.busy() {
            labels.push("Code hosting request".into());
        }
        if self.task_operation_pending() {
            labels.push("Task tracker operation".into());
        }
        if self.svg_jobs.is_active() {
            labels.push("SVG preview".into());
        }
        if self.directory_comparisons_pending() {
            labels.push("Directory comparison".into());
        }
        if self.native_skeletons_pending() {
            labels.push("Native stub generation".into());
        }
        if !self.run_workflow_stamps().is_empty() {
            labels.push("Before-launch tasks".into());
        }
        if self
            .debug
            .as_ref()
            .is_some_and(|session| session.is_active())
        {
            labels.push("Debug session".into());
        }
        labels.extend(self.retained_measurement_labels());
        labels.extend(self.file_watcher_pending_labels());
        labels.extend(self.other_vcs_process_labels());
        labels.extend(self.web_preview_labels());
        if let Some((_, label, _)) = self.update_transfer_state() {
            labels.push(label);
        }
        labels.extend(
            self.deploy
                .pending_transfers()
                .into_iter()
                .map(|(_, label, _)| label),
        );
        if let Some((_, host)) = self.shared_index_download_owner() {
            labels.push(format!("Shared index from {host}"));
        }
        labels.extend(
            self.http_panel
                .running()
                .into_iter()
                .map(|request| format!("HTTP '{}'", request.configuration)),
        );
        if self
            .debug_sessions
            .iter()
            .any(|(session, _)| session.is_active())
        {
            labels.push("Additional debug sessions".into());
        }
        if self.debug_panel.setup_task.is_some()
            || self
                .debug_sessions
                .iter()
                .any(|(_, panel)| panel.setup_task.is_some())
        {
            labels.push("Debugger setup".into());
        }
        if self.test_panel.is_running() {
            labels.push("Tests".into());
        }
        if self.coverage_panel.is_running() {
            labels.push("Coverage run".into());
        }
        if self.profiler_panel.is_running() {
            labels.push("Profiler run".into());
        }
        if self.concurrency_panel.is_running() {
            labels.push("Concurrency capture".into());
        }
        if self.py_console.any_running() {
            labels.push("Python Console".into());
        }
        // Retained terminal ownership is observed without polling/reaping the child.
        for tab in &self.terminal_panel.tabs {
            labels.push(format!("Terminal '{}'", tab.title));
        }
        if self.pending_venv.is_some() {
            labels.push("Project setup".into());
        }
        if self.settings_ui.venv_rx.is_some() {
            labels.push("Interpreter environment setup".into());
        }
        if self.packages_panel.is_running() {
            labels.push("Package operation".into());
        }
        labels.extend(self.notebook_process_labels());
        labels.extend(self.database_process_labels());
        let ai = self.ai_worker_ids();
        if !ai.is_empty() {
            labels.push(format!("{} AI request readers", ai.len()));
        }
        labels.extend(self.web_languages.stamps());
        if self.collaboration_running() {
            labels.push("Collaboration session".into());
        }
        labels.extend(
            self.settings_sync_pending()
                .into_iter()
                .map(|(_, name, _)| name),
        );
        labels.extend(
            self.remote_development_pending()
                .into_iter()
                .map(|(_, name, _)| name),
        );
        labels.extend(
            project_dialogs::abandoned_clone_cleanup_pending()
                .into_iter()
                .map(|(_, name)| name),
        );
        labels.extend(self.plugin_jobs().into_iter().map(|(_, name)| name));
        labels.extend(
            self.window_operation_jobs()
                .into_iter()
                .map(|(_, name)| name),
        );
        labels.extend(
            self.docker_services_pending()
                .into_iter()
                .map(|(_, name, _)| name),
        );
        if self.diagram_panel.capture_owner().is_some() {
            labels.push("Diagram model capture".into());
        }
        if let Some((_, title)) = self.ide_capture_owner() {
            labels.push(title);
        }
        if self.diagnostic_export.stamp().is_some() {
            labels.push("Collecting diagnostic data (must finish before exit)".into());
        }
        if self.network_security_pending() {
            labels.push("Password-store operations (must finish before exit)".into());
        }
        labels
    }
    fn quit_process_stamp(&self) -> String {
        let state = format!(
            "{:?}",
            (
                self.run_panel.process_stamps(),
                (
                    self.debug_sessions_quit_state(),
                    self.task_operation_stamp()
                ),
                self.coverage_panel.run.as_ref().map(|run| run.id),
                (
                    self.profiler_panel.run.as_ref().map(|run| run.id),
                    self.concurrency_panel
                        .run
                        .as_ref()
                        .map(|run| run.lifecycle_stamp())
                ),
                (
                    self.terminal_panel
                        .tabs
                        .iter()
                        .map(|tab| (tab.id, tab.terminal.lifecycle_stamp()))
                        .collect::<Vec<_>>(),
                    rustpy_terminal::poll_retiring_terminals()
                ),
                self.pending_venv
                    .as_ref()
                    .map(|setup| setup.quit_guard_state()),
                self.settings_ui
                    .venv_rx
                    .as_ref()
                    .map(|(target, setup, task)| (
                        target,
                        setup.quit_guard_state(),
                        task.quit_guard_state()
                    )),
                (
                    self.test_panel.quit_process_state(),
                    self.retained_measurement_stamp(),
                    self.http_panel.quit_stamp()
                ),
                self.py_console.quit_process_state(),
                self.packages_panel.quit_process_state(),
                (
                    self.svg_jobs.lifecycle_stamp(),
                    (
                        self.directory_comparison_stamps(),
                        self.native_skeleton_stamps(),
                        (
                            self.interpreter_management_stamps(),
                            self.run_workflow_stamps(),
                            (
                                self.git_panel.log_lifecycle_stamp(),
                                self.file_watcher_lifecycle_stamp(),
                                (
                                    self.other_vcs_lifecycle_stamp(),
                                    (
                                        (
                                            self.shared_index_download_stamp(),
                                            self.update_transfer_state(),
                                            self.update_restart_stamp()
                                        ),
                                        self.web_preview_stamps(),
                                        self.deploy.pending_transfers()
                                    )
                                )
                            ),
                            (
                                self.git_panel.branch_workspace.job_stamp(),
                                (
                                    self.github.stamp(),
                                    self.data_files.stamp(),
                                    self.network_security_pending(),
                                    (
                                        self.database_process_stamp(),
                                        self.ide_capture_owner(),
                                        self.diagnostic_export.stamp(),
                                        self.diagram_panel.capture_owner(),
                                        self.settings_sync_pending(),
                                        (
                                            self.web_languages.stamps(),
                                            self.collaboration_stamp(),
                                            self.ai_worker_ids(),
                                            self.remote_development_pending(),
                                            self.plugin_jobs(),
                                            self.window_operation_jobs(),
                                            self.docker_services_pending(),
                                            (
                                                self.dialog_restart_stamp(),
                                                (project_dialogs::abandoned_clone_cleanup_pending(), self.debug_transport_pending())
                                            )
                                        )
                                    )
                                )
                            )
                        )
                    )
                ),
                self.tabs
                    .iter()
                    .filter_map(|tab| tab
                        .notebook
                        .as_ref()
                        .map(|notebook| (tab.view.id, notebook.quit_process_state())))
                    .collect::<Vec<_>>()
            )
        );
        digest(state.as_bytes())
    }
    /// Current retained UI domains, independently of their acknowledged/pending
    /// workspace snapshots. This observes state without persistence or polling.
    fn quit_current_workspace_stamp(&self) -> String {
        let mut current = Workspace {
            tool_windows: self.tool_window_layout(),
            recent_files: self.recent_files.clone(),
            bookmark_lists: self.bookmark_lists.persisted_names(),
            bookmarks: self
                .bookmarks
                .iter()
                .map(|bookmark| rustpy_settings::BookmarkState {
                    kind: bookmark.kind,
                    list: bookmark.list.clone(),
                    path: bookmark.path.clone(),
                    line: bookmark.line,
                    mnemonic: bookmark.mnemonic,
                    description: bookmark.description.clone(),
                })
                .collect(),
            watches: self
                .debug
                .as_ref()
                .map(|session| {
                    session
                        .watches
                        .iter()
                        .map(|watch| watch.expression.clone())
                        .collect()
                })
                .unwrap_or_default(),
            ..Default::default()
        };
        self.breakpoints.write_to(&mut current);
        let project = self.project.as_ref().map(|project| {
            let mut expanded: Vec<_> = project.tree.expanded.iter().collect();
            expanded.sort();
            (
                &project.root,
                &project.tree.root,
                project.tree.model(),
                &project.interpreter,
                project.interpreter_rx.is_some(),
                expanded,
            )
        });
        let disk: std::collections::BTreeMap<_, _> = self.disk_revisions.iter().collect();
        let observed: std::collections::BTreeMap<_, _> = self.observed_revisions.iter().collect();
        let state = format!(
            "{:?}",
            (
                current,
                project,
                disk,
                observed,
                &self.external_changes,
                self.safe_mode,
                &self.settings.terminal,
                &self.settings.trusted_projects,
                (
                    &self.terminal_on_start,
                    self.startup_terminal_session,
                    &self.pending_tool,
                    self.startup_tasks_done,
                    self.pending_action,
                )
            )
        );
        digest(state.as_bytes())
    }
    fn coordinated_quit_stamp(&self) -> String {
        let tabs = self
            .tabs
            .iter()
            .map(Self::quit_tab_stamp)
            .collect::<Vec<_>>();
        let workspace = self
            .workspace_client
            .as_ref()
            .map(|client| client.quit_guard_state());
        let setup = self
            .pending_venv
            .as_ref()
            .map(|setup| setup.quit_guard_state());
        let processes = self.quit_process_stamp();
        let state = format!(
            "{:?}",
            (
                tabs,
                self.active,
                self.group_active,
                self.capture_editor_layout(),
                self.file_session,
                self.encoding_generation,
                self.encoding_overrides
                    .iter()
                    .collect::<std::collections::BTreeMap<_, _>>(),
                self.actual_current_project_root(),
                workspace,
                setup,
                processes,
                self.quit_current_workspace_stamp()
            )
        );
        digest(state.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, IdeApp) {
        let dir = tempfile::tempdir().unwrap();
        for name in ["first.py", "second.py"] {
            std::fs::write(dir.path().join(name), "value = 1\n").unwrap();
        }
        let mut settings = Settings {
            confirm_exit: false,
            ..Default::default()
        };
        settings.trust_project(dir.path());
        let mut app = IdeApp::new(settings, Some(dir.path().to_path_buf()));
        for name in ["first.py", "second.py"] {
            app.open_file(&dir.path().join(name));
        }
        assert_eq!(app.tabs.len(), 2);
        for tab in &mut app.tabs {
            tab.state.doc.set_text("value = 2\n", &[], &[]).unwrap();
        }
        (dir, app)
    }
    #[cfg(unix)]
    #[test]
    fn committed_close_waits_for_live_and_dropped_terminal_reader_retirement() {
        if isolated_terminal_retirement_fixture(
            "committed_close_waits_for_live_and_dropped_terminal_reader_retirement",
        ) {
            return;
        }
        let (root, mut app) = fixture();
        app.terminal_panel
            .open_command(
                "retirement barrier",
                "/bin/sh",
                &["-c".into(), "tty; exec sleep 30".into()],
                root.path(),
                &[],
            )
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let slave = loop {
            if let Some(path) = app.terminal_panel.tabs[0]
                .terminal
                .contents()
                .lines()
                .map(str::trim)
                .find(|line| line.starts_with("/dev/"))
            {
                break std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(path)
                    .unwrap();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "terminal did not publish its slave device"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        // This separately owned slave descriptor is a real deterministic EOF
        // barrier. Killing the owned child cannot retire the reader until released.
        let owner = app.terminal_panel.tabs[0]
            .terminal
            .lifecycle_stamp()
            .split(':')
            .take(3)
            .collect::<Vec<_>>()
            .join(":");
        let context = egui::Context::default();
        assert!(matches!(
            app.prepare_coordinated_quit("terminal-drain"),
            QuitProgress::PendingProcesses { .. }
        ));
        app.answer_coordinated_quit("terminal-drain", QuitAnswer::Continue);
        app.answer_coordinated_quit("terminal-drain", QuitAnswer::Discard);
        let QuitProgress::Prepared { guard } =
            app.answer_coordinated_quit("terminal-drain", QuitAnswer::Discard)
        else {
            panic!("terminal close was not prepared with consent")
        };
        app.commit_coordinated_quit("terminal-drain", &guard, |transaction, actual| {
            assert_eq!(transaction, "terminal-drain");
            assert_eq!(actual, &guard);
            Ok(())
        })
        .unwrap();
        assert!(app.exit_requested);
        assert!(
            app.terminal_retirement_pending(&context),
            "Stop request alone cannot authorize close"
        );
        for dropped in [false, true] {
            if dropped {
                app.terminal_panel.close(0);
            }
            assert!(app.terminal_retirement_pending(&context));
            let mut output = context.run_ui(Default::default(), |ui| {
                app.coordinated_quit_guard_ui(ui.ctx())
            });
            output.textures_delta.clear(); // clear before assertions can unwind
            let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
            assert!(commands
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::CancelClose)));
            assert!(!commands
                .iter()
                .any(|command| matches!(command, egui::ViewportCommand::Close)));
            output.drop_without_applying_deltas();
        }
        drop(slave);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while rustpy_terminal::poll_retiring_terminals()
            .iter()
            .any(|stamp| stamp.starts_with(&owner))
        {
            assert!(
                std::time::Instant::now() < deadline,
                "captured terminal did not physically retire"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    #[test]
    fn managed_cancel_retains_notebook_decisions_and_guard_until_central_abort() {
        let (dir, mut app) = fixture();
        let path = dir.path().join("managed.ipynb");
        let bytes = serde_json::to_vec(&rustpy_notebook::new_notebook().to_json()).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        app.open_file(&path);
        let notebook = app.tabs.last_mut().unwrap().notebook.as_mut().unwrap();
        notebook.set_cell_source(0, "retained intent\n".into());
        notebook.set_dirty(true);
        let model = notebook.to_json();
        assert!(matches!(
            app.prepare_managed_coordinated_quit("managed"),
            QuitProgress::PendingDocument { .. }
        ));
        app.answer_coordinated_quit("managed", QuitAnswer::Discard);
        app.answer_coordinated_quit("managed", QuitAnswer::Discard);
        let QuitProgress::Prepared { guard } =
            app.answer_coordinated_quit("managed", QuitAnswer::Discard)
        else {
            panic!("not prepared")
        };
        assert!(matches!(
            app.answer_coordinated_quit("managed", QuitAnswer::Cancel),
            QuitProgress::Rejected { .. }
        ));
        assert!(app.coordinated_quit_active());
        assert!(app.coordinated_quit_cancel_requested("managed"));
        app.validate_coordinated_quit("managed", &guard).unwrap();
        assert!(matches!(
            app.poll_coordinated_quit("managed"),
            QuitProgress::Rejected { .. }
        ));
        assert_eq!(
            app.abort_coordinated_quit("other"),
            QuitProgress::Rejected {
                message: "Quit transaction mismatch".into()
            }
        );
        assert_eq!(app.abort_coordinated_quit("managed"), QuitProgress::Aborted);
        assert!(!app.coordinated_quit_active());
        assert!(app.tabs[0].state.doc.is_modified());
        assert!(app.tabs[1].state.doc.is_modified());
        let notebook = app.tabs[2].notebook.as_ref().unwrap();
        assert!(notebook.is_dirty());
        assert_eq!(notebook.to_json(), model);
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
    #[test]
    fn managed_cancel_keeps_actual_environment_owner_until_explicit_abort() {
        let Some(python) = ["python3", "python"].into_iter().find_map(|name| {
            let path = which::which(name).ok()?;
            let interpreter = rustpy_project::interpreter::probe(&path, "system")?;
            interpreter.version.starts_with("3.").then_some(path)
        }) else {
            return;
        };
        let (dir, mut app) = fixture();
        let setup =
            rustpy_project::setup::Setup::new(dir.path().into(), python.clone(), Vec::new());
        let token = setup.command_token();
        let task = rustpy_project::setup::CommandTask::spawn(
            rustpy_project::setup::CommandSpec::Process {
                program: python,
                args: vec!["-c".into(), "import time; time.sleep(60)".into()],
                cwd: dir.path().into(),
            },
            token.clone(),
        );
        app.settings_ui.venv_rx = Some((dir.path().into(), setup, task));
        let owners = app.quit_process_stamp();
        assert!(matches!(
            app.prepare_managed_coordinated_quit("owner"),
            QuitProgress::PendingProcesses { .. }
        ));
        app.answer_coordinated_quit("owner", QuitAnswer::Cancel);
        assert_eq!(app.quit_process_stamp(), owners);
        assert!(app.coordinated_quit_active());
        assert!(!token.is_cancelled());
        assert_eq!(app.abort_coordinated_quit("owner"), QuitProgress::Aborted);
        assert_eq!(app.quit_process_stamp(), owners);
        assert!(!token.is_cancelled());
        assert!(app.tabs.iter().all(|tab| tab.state.doc.is_modified()));
        // Explicit fixture cleanup, never a side effect of Cancel or Abort.
        let (_, mut setup, task) = app.settings_ui.venv_rx.take().unwrap();
        setup.cancel();
        task.cancel();
    }
    #[test]
    fn managed_verified_seal_survives_late_cancel_and_rejects_abort_until_commit() {
        let (_dir, mut app) = fixture();
        app.prepare_managed_coordinated_quit("sealed");
        app.answer_coordinated_quit("sealed", QuitAnswer::Discard);
        let QuitProgress::Prepared { guard } =
            app.answer_coordinated_quit("sealed", QuitAnswer::Discard)
        else {
            panic!("not prepared")
        };
        assert!(app
            .seal_coordinated_quit("sealed", &guard, |_, _| Err(
                "checkpoint not verified".into()
            ))
            .is_err());
        assert!(!app.coordinated_quit_published("sealed"));
        app.answer_coordinated_quit("sealed", QuitAnswer::Cancel);
        app.validate_coordinated_quit("sealed", &guard).unwrap();
        app.seal_coordinated_quit("sealed", &guard, |tx, actual| {
            assert_eq!(tx, "sealed");
            assert_eq!(actual, &guard);
            Ok(())
        })
        .unwrap();
        assert!(app.coordinated_quit_published("sealed"));
        assert!(matches!(
            app.answer_coordinated_quit("sealed", QuitAnswer::Cancel),
            QuitProgress::Rejected { .. }
        ));
        assert!(matches!(
            app.abort_coordinated_quit("sealed"),
            QuitProgress::Rejected { .. }
        ));
        assert!(app
            .seal_coordinated_quit("other", &guard, |_, _| panic!("mismatched verification"))
            .is_err());
        app.seal_coordinated_quit("sealed", &guard, |_, _| Ok(()))
            .unwrap();
        assert!(
            matches!(app.poll_coordinated_quit("sealed"), QuitProgress::Prepared { guard: actual } if actual == guard)
        );
        assert!(app
            .commit_coordinated_quit("sealed", &guard, |_, _| Err("retry verification".into()))
            .is_err());
        assert!(app.coordinated_quit_active());
        assert!(app.tabs.iter().all(|tab| tab.state.doc.is_modified()));
        app.commit_coordinated_quit("sealed", &guard, |_, _| Ok(()))
            .unwrap();
        assert!(app.exit_requested);
        app.commit_coordinated_quit("sealed", &guard, |_, _| panic!("duplicate verification"))
            .unwrap();
    }
    // The PTY EOF-barrier fixture deliberately publishes process-global terminal
    // retirement. Isolate that publisher so unrelated document/notebook Quit
    // contracts cannot inherit its pending process-consent prompt. The child
    // still exercises the original real PTY, reader drain and retirement checks.
    #[cfg(unix)]
    fn isolated_terminal_retirement_fixture(name: &str) -> bool {
        const MARKER: &str = "RUSTPY_TERMINAL_RETIREMENT_FIXTURE";
        let selector = format!("app::window_lifecycle::tests::{name}");
        if std::env::var(MARKER).as_deref() == Ok(selector.as_str()) {
            assert!(rustpy_terminal::poll_retiring_terminals().is_empty());
            return false;
        }
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &selector, "--nocapture", "--test-threads=1"])
            .env(MARKER, &selector)
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                let _ = child.wait();
                panic!("isolated terminal retirement fixture timed out: {selector}");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        true
    }

    #[test]
    fn discard_then_cancel_retains_both_dirty_documents_and_lease() {
        let (_dir, mut app) = fixture();
        let before = app.coordinated_quit_stamp();
        assert!(matches!(
            app.prepare_coordinated_quit("discard"),
            QuitProgress::PendingDocument { .. }
        ));
        assert!(matches!(
            app.answer_coordinated_quit("discard", QuitAnswer::Discard),
            QuitProgress::PendingDocument { .. }
        ));
        assert_eq!(
            app.answer_coordinated_quit("discard", QuitAnswer::Cancel),
            QuitProgress::Aborted
        );
        assert_eq!(app.coordinated_quit_stamp(), before);
        assert!(app.tabs.iter().all(Tab::has_unsaved_changes));
    }
    #[test]
    fn authorized_save_then_cancel_keeps_saved_tab_and_remaining_dirty_tab() {
        let (dir, mut app) = fixture();
        app.prepare_coordinated_quit("save");
        assert!(matches!(
            app.answer_coordinated_quit("save", QuitAnswer::Save),
            QuitProgress::PendingDocument { .. }
        ));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("first.py")).unwrap(),
            "value = 2\n"
        );
        assert_eq!(
            app.answer_coordinated_quit("save", QuitAnswer::Cancel),
            QuitProgress::Aborted
        );
        assert_eq!(app.tabs.len(), 2);
        assert!(!app.tabs[0].has_unsaved_changes());
        assert!(app.tabs[1].has_unsaved_changes());
    }
    #[test]
    fn prepared_guard_rejects_changed_state_and_failed_certificate_without_side_effects() {
        let (_dir, mut app) = fixture();
        app.prepare_coordinated_quit("guard");
        app.answer_coordinated_quit("guard", QuitAnswer::Discard);
        let QuitProgress::Prepared { guard } =
            app.answer_coordinated_quit("guard", QuitAnswer::Discard)
        else {
            panic!("not prepared")
        };
        let before = app.coordinated_quit_stamp();
        assert!(app
            .commit_coordinated_quit("guard", &guard, |_, _| Err("checkpoint unavailable".into()))
            .is_err());
        assert_eq!(app.coordinated_quit_stamp(), before);
        assert!(!app.exit_requested);
        app.tabs[0].pinned = !app.tabs[0].pinned;
        assert!(app.validate_coordinated_quit("guard", &guard).is_err());
        assert_eq!(app.abort_coordinated_quit("guard"), QuitProgress::Aborted);
        assert_eq!(app.tabs.len(), 2);
    }
    #[test]
    fn failed_save_retains_document_disk_and_abortability() {
        let (dir, mut app) = fixture();
        app.tabs[0]
            .state
            .doc
            .set_encoding(rustpy_text::Encoding::Latin1);
        app.tabs[0]
            .state
            .doc
            .set_text("value = '😀'\n", &[], &[])
            .unwrap();
        let before = app.tabs[0].state.doc.quit_guard_state();
        app.prepare_coordinated_quit("failure");
        assert!(matches!(
            app.answer_coordinated_quit("failure", QuitAnswer::Save),
            QuitProgress::PendingDocument { error: Some(_), .. }
        ));
        assert_eq!(app.tabs[0].state.doc.quit_guard_state(), before);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("first.py")).unwrap(),
            "value = 1\n"
        );
        assert_eq!(app.abort_coordinated_quit("failure"), QuitProgress::Aborted);
        assert_eq!(app.tabs.len(), 2);
    }
    #[test]
    fn only_verified_commit_sets_exit_and_duplicate_is_transaction_bound() {
        let (_dir, mut app) = fixture();
        app.prepare_coordinated_quit("commit");
        app.answer_coordinated_quit("commit", QuitAnswer::Discard);
        let QuitProgress::Prepared { guard } =
            app.answer_coordinated_quit("commit", QuitAnswer::Discard)
        else {
            panic!("not prepared")
        };
        assert!(!app.exit_requested);
        app.commit_coordinated_quit("commit", &guard, |tx, passed| {
            assert_eq!(tx, "commit");
            assert_eq!(passed, &guard);
            Ok(())
        })
        .unwrap();
        assert!(app.exit_requested);
        assert_eq!(app.tabs.len(), 2);
        assert!(app.tabs.iter().all(Tab::has_unsaved_changes));
        app.commit_coordinated_quit("commit", &guard, |_, _| {
            panic!("duplicate must not verify again")
        })
        .unwrap();
        assert!(app
            .commit_coordinated_quit("other", &guard, |_, _| Ok(()))
            .is_err());
    }

    #[test]
    fn dirty_notebook_discard_cancel_preserves_model_and_clean_document() {
        let (dir, mut app) = fixture();
        let path = dir.path().join("model.ipynb");
        let notebook = rustpy_notebook::new_notebook();
        let original_bytes = serde_json::to_vec(&notebook.to_json()).unwrap();
        std::fs::write(&path, &original_bytes).unwrap();
        app.open_file(&path);
        for tab in &mut app.tabs {
            tab.state.doc.mark_saved();
        }
        let tab = app
            .tabs
            .iter_mut()
            .find(|tab| tab.state.path.as_ref() == Some(&path))
            .unwrap();
        let model = tab.notebook.as_mut().unwrap();
        model.set_cell_source(0, "unsaved = 7\n".into());
        model.set_dirty(true);
        let before = model.to_json();
        let id = tab.view.id;
        assert!(
            matches!(app.prepare_coordinated_quit("notebook"), QuitProgress::PendingDocument { view_id, .. } if view_id == id)
        );
        assert!(matches!(
            app.answer_coordinated_quit("notebook", QuitAnswer::Discard),
            QuitProgress::Prepared { .. }
        ));
        assert_eq!(
            app.abort_coordinated_quit("notebook"),
            QuitProgress::Aborted
        );
        let tab = app.tabs.iter().find(|tab| tab.view.id == id).unwrap();
        assert!(!tab.state.doc.is_modified());
        assert!(tab.notebook.as_ref().unwrap().is_dirty());
        assert_eq!(tab.notebook.as_ref().unwrap().to_json(), before);
        assert_eq!(std::fs::read(path).unwrap(), original_bytes);
    }
    #[test]
    fn staged_notebook_change_is_rejected_at_readiness_before_workspace_save() {
        let (dir, mut app) = fixture();
        let path = dir.path().join("staged.ipynb");
        let bytes = serde_json::to_vec(&rustpy_notebook::new_notebook().to_json()).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        app.open_file(&path);
        let index = app
            .tabs
            .iter()
            .position(|tab| tab.state.path.as_ref() == Some(&path))
            .unwrap();
        let tab = app.tabs.remove(index);
        app.tabs.insert(0, tab);
        let id = app.tabs[0].view.id;
        let notebook = app.tabs[0].notebook.as_mut().unwrap();
        notebook.set_cell_source(0, "first intent\n".into());
        notebook.set_dirty(true);
        assert!(
            matches!(app.prepare_coordinated_quit("staged"), QuitProgress::PendingDocument { view_id, .. } if view_id == id)
        );
        assert!(matches!(
            app.answer_coordinated_quit("staged", QuitAnswer::Discard),
            QuitProgress::PendingDocument { .. }
        ));
        app.tabs[0]
            .notebook
            .as_mut()
            .unwrap()
            .set_cell_source(0, "changed intent\n".into());
        let changed = app.tabs[0].notebook.as_ref().unwrap().to_json();
        assert!(matches!(
            app.poll_coordinated_quit("staged"),
            QuitProgress::PendingDocument { .. }
        ));
        assert!(matches!(
            app.answer_coordinated_quit("staged", QuitAnswer::Discard),
            QuitProgress::PendingDocument { .. }
        ));
        let workspace_path = Workspace::path_for(dir.path());
        let workspace_before = std::fs::read(&workspace_path).ok();
        assert!(matches!(
            app.answer_coordinated_quit("staged", QuitAnswer::Discard),
            QuitProgress::Rejected { .. }
        ));
        assert_eq!(std::fs::read(workspace_path).ok(), workspace_before);
        assert_eq!(app.abort_coordinated_quit("staged"), QuitProgress::Aborted);
        assert_eq!(app.tabs.len(), 3);
        assert_eq!(app.tabs[0].notebook.as_ref().unwrap().to_json(), changed);
        assert!(app.tabs[0].notebook.as_ref().unwrap().is_dirty());
        assert!(!app.tabs[0].state.doc.is_modified());
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
    #[test]
    fn damaged_workspace_prepares_only_without_explicit_pending_intent() {
        let dir = tempfile::tempdir().unwrap();
        let path = Workspace::path_for(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let corrupt = b"damaged workspace retained";
        std::fs::write(&path, corrupt).unwrap();
        let mut settings = Settings {
            confirm_exit: false,
            ..Default::default()
        };
        settings.trust_project(dir.path());
        let mut app = IdeApp::new(settings, Some(dir.path().to_path_buf()));
        assert!(matches!(
            app.prepare_coordinated_quit("provisional"),
            QuitProgress::Prepared { .. }
        ));
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);
        app.abort_coordinated_quit("provisional");
        app.workspace_client.as_mut().unwrap().pending.interpreter =
            Some(Some(dir.path().join("selected-python")));
        assert!(matches!(
            app.prepare_coordinated_quit("intent"),
            QuitProgress::Rejected { .. }
        ));
        assert!(app
            .workspace_client
            .as_ref()
            .unwrap()
            .pending
            .interpreter
            .is_some());
        assert_eq!(std::fs::read(path).unwrap(), corrupt);
        assert_eq!(app.abort_coordinated_quit("intent"), QuitProgress::Aborted);
    }
    #[test]
    fn save_started_watcher_requires_fresh_process_consent_and_cancel_keeps_it_owned() {
        // Use the same PATH candidates and actual probe as interpreter discovery.
        // Hosts without Python retain the repository's optional-provider test policy.
        let Some(python) = ["python3", "python"].into_iter().find_map(|name| {
            let path = which::which(name).ok()?;
            let interpreter = rustpy_project::interpreter::probe(&path, "system")?;
            interpreter.version.starts_with("3.").then_some(path)
        }) else {
            return;
        };
        let (dir, mut app) = fixture();
        let provider = dir.path().join("owned_watcher.py");
        let started = dir.path().join("watcher_started");
        std::fs::write(&provider, "import pathlib, sys, time\npathlib.Path(sys.argv[1]).write_text('started')\ntime.sleep(60)\n").unwrap();
        app.settings
            .file_watchers
            .push(rustpy_settings::FileWatcher {
                name: "owned watcher".into(),
                pattern: "*.py".into(),
                program: python.to_string_lossy().into_owned(),
                arguments: format!("\"{}\" \"{}\"", provider.display(), started.display()),
                enabled: true,
                ..Default::default()
            });
        assert!(app.run_panel.sessions.is_empty());
        assert!(matches!(
            app.prepare_coordinated_quit("watcher"),
            QuitProgress::PendingDocument { .. }
        ));
        let QuitProgress::PendingProcesses { processes } =
            app.answer_coordinated_quit("watcher", QuitAnswer::Save)
        else {
            panic!("new watcher did not require consent")
        };
        assert!(processes
            .iter()
            .any(|label| label.contains("owned watcher")));
        assert_eq!(app.tabs.len(), 2);
        assert!(
            app.run_panel.sessions.is_empty(),
            "saved watchers wait for debounce"
        );
        assert!(!app.file_watcher_pending_labels().is_empty());
        let ctx = egui::Context::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !started.exists() && std::time::Instant::now() < deadline {
            app.file_watchers_tick(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(started.exists(), "actual watcher never started");
        assert_eq!(app.run_panel.sessions.len(), 1);
        let first_owner = app.run_panel.sessions[0].id;
        let first_stamp = app.file_watcher_lifecycle_stamp();
        assert!(app.run_panel.sessions[0].is_running());
        assert!(matches!(
            app.answer_coordinated_quit("watcher", QuitAnswer::Continue),
            QuitProgress::PendingDocument { .. }
        ));
        assert!(matches!(
            app.answer_coordinated_quit("watcher", QuitAnswer::Save),
            QuitProgress::PendingProcesses { .. }
        ));
        assert_eq!(
            app.run_panel.sessions.len(),
            1,
            "watchers serialize their processes"
        );
        assert_eq!(app.run_panel.sessions[0].id, first_owner);
        assert_ne!(
            app.file_watcher_lifecycle_stamp(),
            first_stamp,
            "same-label queued work must invalidate the earlier consent"
        );
        assert!(!app.file_watcher_pending_labels().is_empty());
        assert_eq!(
            app.answer_coordinated_quit("watcher", QuitAnswer::Cancel),
            QuitProgress::Aborted
        );
        assert_eq!(app.tabs.len(), 2);
        assert!(app
            .run_panel
            .sessions
            .iter()
            .all(|session| session.is_running()));
        assert!(
            !app.file_watcher_pending_labels().is_empty(),
            "Cancel retains queued ownership too"
        );
        // Explicit test cleanup after proving Cancel retains both running and queued owners.
        app.terminate_processes();
        assert!(app.file_watcher_pending_labels().is_empty());
    }
    #[test]
    fn quit_prompt_paints_the_captured_ide_frame_behind_it() {
        let (_dir, mut app) = fixture();
        app.prepare_coordinated_quit("backdrop");
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let backdrop = |output: &egui::FullOutput| {
            output.shapes.iter().any(|clipped| match &clipped.shape {
                egui::Shape::Mesh(mesh) => {
                    mesh.texture_id != egui::TextureId::default()
                        && mesh.calc_bounds().contains_rect(screen)
                }
                _ => false,
            })
        };
        let filled = |output: &egui::FullOutput| {
            output.shapes.iter().any(|clipped| match &clipped.shape {
                egui::Shape::Rect(rect) => {
                    rect.rect.contains_rect(screen) && rect.fill != egui::Color32::TRANSPARENT
                }
                _ => false,
            })
        };
        let input = || egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        };
        // Before the capture arrives: the panel colour, never an empty window.
        let output = ctx.run_ui(input(), |ui| app.coordinated_quit_guard_ui(ui.ctx()));
        assert!(filled(&output) && !backdrop(&output));
        output.drop_without_applying_deltas();
        let mut shot = input();
        shot.events.push(egui::Event::Screenshot {
            viewport_id: egui::ViewportId::ROOT,
            user_data: egui::UserData::new(QuitBackdropShot),
            image: std::sync::Arc::new(egui::ColorImage::new(
                [16, 12],
                vec![egui::Color32::from_rgb(40, 80, 120); 16 * 12],
            )),
        });
        let output = ctx.run_ui(shot, |ui| app.coordinated_quit_guard_ui(ui.ctx()));
        assert!(backdrop(&output), "captured IDE frame behind the prompt");
        output.drop_without_applying_deltas();
        // It stays for the following frames of the frozen window.
        let output = ctx.run_ui(input(), |ui| app.coordinated_quit_guard_ui(ui.ctx()));
        assert!(backdrop(&output));
        output.drop_without_applying_deltas();
        // An ordinary frame (Quit over) drops it; Exit requests a new capture.
        let output = ctx.run_ui(input(), |ui| app.capture_quit_backdrop(ui.ctx()));
        output.drop_without_applying_deltas();
        assert!(!app.has_quit_backdrop());
        app.request_quit_backdrop();
        let output = ctx.run_ui(input(), |ui| app.capture_quit_backdrop(ui.ctx()));
        assert!(output.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .iter()
            .any(|command| matches!(
                command,
                egui::ViewportCommand::Screenshot(data) if is_quit_backdrop(data)
            )));
        output.drop_without_applying_deltas();
    }
    #[test]
    fn native_close_is_cancelled_until_verified_commit_then_only_close_is_emitted() {
        let (_dir, mut app) = fixture();
        app.prepare_coordinated_quit("native-close");
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .events
            .push(egui::ViewportEvent::Close);
        let output = ctx.run_ui(input, |ui| app.coordinated_quit_guard_ui(ui.ctx()));
        let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
        assert!(commands
            .iter()
            .any(|command| matches!(command, egui::ViewportCommand::CancelClose)));
        assert!(!commands
            .iter()
            .any(|command| matches!(command, egui::ViewportCommand::Close)));
        output.drop_without_applying_deltas();
        assert_eq!(app.tabs.len(), 2);
        app.answer_coordinated_quit("native-close", QuitAnswer::Discard);
        let QuitProgress::Prepared { guard } =
            app.answer_coordinated_quit("native-close", QuitAnswer::Discard)
        else {
            panic!("not prepared")
        };
        let before = app.coordinated_quit_stamp();
        let output = ctx.run_ui(Default::default(), |ui| {
            app.coordinated_quit_guard_ui(ui.ctx())
        });
        assert_eq!(
            app.coordinated_quit_stamp(),
            before,
            "modal paint must not publish state"
        );
        output.drop_without_applying_deltas();
        app.commit_coordinated_quit("native-close", &guard, |_, _| Ok(()))
            .unwrap();
        let output = ctx.run_ui(Default::default(), |ui| {
            app.coordinated_quit_guard_ui(ui.ctx())
        });
        let commands = &output.viewport_output[&egui::ViewportId::ROOT].commands;
        assert_eq!(commands.len(), 1);
        assert!(matches!(commands[0], egui::ViewportCommand::Close));
        output.drop_without_applying_deltas();
        assert_eq!(app.tabs.len(), 2);
    }
    fn prepare_existing(app: &mut IdeApp, transaction: &str) -> QuitGuard {
        app.prepare_coordinated_quit(transaction);
        app.answer_coordinated_quit(transaction, QuitAnswer::Discard);
        let QuitProgress::Prepared { guard } =
            app.answer_coordinated_quit(transaction, QuitAnswer::Discard)
        else {
            panic!("not prepared")
        };
        guard
    }
    #[test]
    fn prepared_guard_rechecks_current_workspace_domains() {
        type Change = fn(&mut IdeApp, &Path);
        let changes: &[(&str, Change)] = &[
            ("deferred terminal command", |app, _| {
                app.terminal_on_start = Some("echo pending".into());
            }),
            ("deferred terminal session", |app, _| {
                app.startup_terminal_session = Some(987);
            }),
            ("pending tool", |app, _| {
                app.pending_tool = Some(rustpy_settings::ExternalTool {
                    name: "Pending tool".into(),
                    ..Default::default()
                });
            }),
            ("startup task completion", |app, _| {
                app.startup_tasks_done = !app.startup_tasks_done;
            }),
            ("pending action", |app, _| {
                app.pending_action = Some(Action::Save);
            }),
            ("disk baseline", |app, root| {
                app.disk_revisions.insert(
                    root.join("first.py"),
                    Some(rustpy_core::vfs::ContentRevision::of(b"changed baseline")),
                );
            }),
            ("observed disk revision", |app, root| {
                app.observed_revisions.insert(
                    root.join("first.py"),
                    Some(rustpy_core::vfs::ContentRevision::of(b"observed change")),
                );
            }),
            ("tool layout", |app, _| app.layout.left_width += 31.0),
            ("recent files", |app, root| {
                app.recent_files.push(root.join("recent.py"))
            }),
            ("expanded directories", |app, root| {
                app.project
                    .as_mut()
                    .unwrap()
                    .tree
                    .expanded
                    .insert(root.join("expanded"));
            }),
            ("source roots", |app, root| {
                app.project
                    .as_mut()
                    .unwrap()
                    .tree
                    .source_roots
                    .insert(root.join("source"));
            }),
            ("test roots", |app, root| {
                app.project
                    .as_mut()
                    .unwrap()
                    .tree
                    .test_roots
                    .insert(root.join("tests"));
            }),
            ("resource roots", |app, root| {
                app.project
                    .as_mut()
                    .unwrap()
                    .tree
                    .resource_roots
                    .insert(root.join("resources"));
            }),
            ("excluded roots", |app, root| {
                app.project
                    .as_mut()
                    .unwrap()
                    .tree
                    .excluded
                    .insert(root.join("excluded"));
            }),
            ("attached roots", |app, root| {
                app.project
                    .as_mut()
                    .unwrap()
                    .tree
                    .content_roots
                    .push(root.join("attached"))
            }),
            ("bookmarks", |app, root| {
                app.bookmarks.push(Bookmark {
                    kind: rustpy_settings::BookmarkKind::Line,
                    list: String::new(),
                    path: root.join("first.py"),
                    line: 1,
                    mnemonic: Some('Q'),
                    description: "changed after preparation".into(),
                })
            }),
            ("breakpoints", |app, root| {
                app.breakpoints.toggle(&root.join("first.py"), 1);
            }),
            ("breakpoint preferences", |app, _| {
                app.breakpoints.muted = !app.breakpoints.muted
            }),
            ("interpreter", |app, root| {
                app.project.as_mut().unwrap().interpreter = Some(Interpreter {
                    path: root.join("recorded-python"),
                    version: "3.12".into(),
                    prefix: root.into(),
                    sys_path: vec![root.join("lib")],
                    is_venv: true,
                    platform: "fixture".into(),
                    kind: "venv".into(),
                })
            }),
        ];
        let mut missed = Vec::new();
        for &(name, change) in changes {
            let (dir, mut app) = fixture();
            let guard = prepare_existing(&mut app, "domains");
            app.validate_coordinated_quit("domains", &guard)
                .expect("unchanged actual app must match before domain mutation");
            change(&mut app, dir.path());
            if app.validate_coordinated_quit("domains", &guard).is_ok() {
                missed.push(name);
            }
            assert_eq!(app.abort_coordinated_quit("domains"), QuitProgress::Aborted);
            assert_eq!(app.tabs.len(), 2);
        }
        assert!(
            missed.is_empty(),
            "prepared guard missed current domains: {missed:?}"
        );
    }
    #[test]
    fn settings_environment_owner_requires_consent_and_is_retained_until_verified_commit() {
        let Some(python) = ["python3", "python"].into_iter().find_map(|name| {
            let path = which::which(name).ok()?;
            let interpreter = rustpy_project::interpreter::probe(&path, "system")?;
            interpreter.version.starts_with("3.").then_some(path)
        }) else {
            return;
        };
        let (dir, mut app) = fixture();
        assert!(matches!(
            app.prepare_coordinated_quit("settings-owner"),
            QuitProgress::PendingDocument { .. }
        ));
        let marker = dir.path().join("environment_started");
        let setup =
            rustpy_project::setup::Setup::new(dir.path().into(), python.clone(), Vec::new());
        let token = setup.command_token();
        let command = rustpy_project::setup::CommandSpec::Process { program: python, args: vec!["-c".into(), "import pathlib,sys,time; pathlib.Path(sys.argv[1]).write_text('started'); time.sleep(60)".into(), marker.to_string_lossy().into_owned()], cwd: dir.path().into() };
        let task = rustpy_project::setup::CommandTask::spawn(command, token.clone());
        app.settings_ui.venv_rx = Some((dir.path().join(".venv"), setup, task));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !marker.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            marker.exists(),
            "actual Settings environment task never started"
        );
        assert!(
            matches!(
                app.poll_coordinated_quit("settings-owner"),
                QuitProgress::PendingProcesses { .. }
            ),
            "new Settings owner did not require consent"
        );
        assert_eq!(
            app.abort_coordinated_quit("settings-owner"),
            QuitProgress::Aborted
        );
        assert!(app.settings_ui.venv_rx.is_some());
        assert!(
            !token.is_cancelled(),
            "Abort cancelled retained environment task"
        );
        assert!(matches!(
            app.prepare_coordinated_quit("settings-commit"),
            QuitProgress::PendingProcesses { .. }
        ));
        app.answer_coordinated_quit("settings-commit", QuitAnswer::Continue);
        app.answer_coordinated_quit("settings-commit", QuitAnswer::Discard);
        let QuitProgress::Prepared { guard } =
            app.answer_coordinated_quit("settings-commit", QuitAnswer::Discard)
        else {
            panic!("not prepared")
        };
        assert!(app
            .commit_coordinated_quit("settings-commit", &guard, |_, _| Err(
                "checkpoint failure".into()
            ))
            .is_err());
        assert!(app.settings_ui.venv_rx.is_some());
        assert!(!token.is_cancelled());
        app.commit_coordinated_quit("settings-commit", &guard, |_, _| Ok(()))
            .unwrap();
        assert!(app.settings_ui.venv_rx.is_none());
        assert!(
            token.is_cancelled(),
            "verified commit did not cancel actual owner"
        );
        assert_eq!(app.tabs.len(), 2);
    }
    #[test]
    fn prepared_guard_rechecks_current_debug_watch_expressions() {
        let Some(python) = ["python3", "python"]
            .into_iter()
            .find_map(|name| which::which(name).ok())
        else {
            return;
        };
        let available = std::process::Command::new(&python)
            .args(["-c", "import debugpy"])
            .stdin(std::process::Stdio::piped())
            .output()
            .unwrap();
        if !available.status.success() {
            return;
        }
        let (dir, mut app) = fixture();
        let script = dir.path().join("watch_owner.py");
        std::fs::write(&script, "import time\ntime.sleep(60)\n").unwrap();
        let mut cfg = rustpy_project::RunConfig::for_script(&script);
        cfg.interpreter = Some(python);
        app.debug = Some(
            rustpy_debug::DebugSession::launch(777, &cfg, None, dir.path(), Default::default())
                .unwrap(),
        );
        assert!(matches!(
            app.prepare_coordinated_quit("watches"),
            QuitProgress::PendingProcesses { .. }
        ));
        app.answer_coordinated_quit("watches", QuitAnswer::Continue);
        app.answer_coordinated_quit("watches", QuitAnswer::Discard);
        let QuitProgress::Prepared { guard } =
            app.answer_coordinated_quit("watches", QuitAnswer::Discard)
        else {
            panic!("not prepared")
        };
        app.validate_coordinated_quit("watches", &guard)
            .expect("unchanged retained debug owner matches");
        app.debug
            .as_mut()
            .unwrap()
            .watches
            .push(rustpy_debug::Watch {
                expression: "changed_watch".into(),
                result: None,
                error: None,
            });
        let rejected = app.validate_coordinated_quit("watches", &guard).is_err();
        app.abort_coordinated_quit("watches");
        app.debug.as_mut().unwrap().stop();
        assert!(rejected, "prepared guard missed current watch expressions");
        assert_eq!(app.tabs.len(), 2);
    }
    #[test]
    fn captured_quit_workspace_stamp_includes_empty_lists_and_bookmark_membership() {
        let (_root, mut app) = fixture();
        let before = app.quit_current_workspace_stamp();
        app.bookmark_lists
            .restore(vec!["Default".into(), "Empty review".into()]);
        let with_empty = app.quit_current_workspace_stamp();
        assert_ne!(before, with_empty);
        app.bookmarks.push(Bookmark {
            path: PathBuf::from("/bookmark-only.py"),
            line: 0,
            mnemonic: None,
            description: String::new(),
            kind: rustpy_settings::BookmarkKind::File,
            list: "Empty review".into(),
        });
        let with_file = app.quit_current_workspace_stamp();
        assert_ne!(with_empty, with_file);
        app.bookmarks.last_mut().unwrap().list = "Default".into();
        assert_ne!(with_file, app.quit_current_workspace_stamp());
    }
}
