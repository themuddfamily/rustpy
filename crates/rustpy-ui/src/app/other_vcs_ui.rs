//! Provider tool-window adapter. Jobs, follow-ups and file-lifecycle requests keep
//! the provider/root captured at submission rather than reading the active Git root.
use super::*;
use crate::a11y::DialogWindow;
use crate::other_vcs_panel::{Event, OtherVcsPanel, Request, ResultView};
use rustpy_vcs::{
    other_vcs::{
        self,
        runner::{Event as JobEvent, ProviderJob},
        Arguments, Credentials, Operation,
    },
    provider::Provider,
};
use std::collections::VecDeque;
struct Running {
    job: ProviderJob,
    id: u64,
    request: Request,
    format: other_vcs::Format,
}
struct FileChange {
    root: PathBuf,
    provider: Provider,
    path: PathBuf,
    removed: bool,
}
#[derive(Default)]
pub(super) struct OtherVcsWorkspace {
    pub panel: OtherVcsPanel,
    running: Option<Running>,
    queue: VecDeque<Request>,
    file_changes: VecDeque<FileChange>,
    project: PathBuf,
    last_file: Option<PathBuf>,
    export: Option<(String, String)>,
}
impl IdeApp {
    pub(super) fn enable_tfs_integration(
        &mut self,
        integration: rustpy_vcs::provider::Integration,
    ) {
        if self.project_safe_mode() || !self.builtin_plugin_enabled("tfs") {
            self.set_status("Enable the TFS bundled adapter in Plugins and trust this project before creating a workspace");
            return;
        }
        if self.other_vcs_workspace.running.is_some() || !self.other_vcs_workspace.queue.is_empty()
        {
            self.set_status("Wait for the current provider operation before enabling TFVC");
            return;
        }
        if let Err(error) = integration.commands() {
            self.set_status(error.to_string());
            return;
        }
        if !integration.root.is_dir() {
            self.set_status("Select an existing TFVC workspace directory");
            return;
        }
        self.other_vcs_workspace.project = self
            .project
            .as_ref()
            .map(|project| project.root.clone())
            .unwrap_or_default();
        let settings = rustpy_settings::other_vcs::RootSettings {
            root: integration.root.clone(),
            endpoint: integration.endpoint,
            client: integration.workspace,
            username: integration.user,
            ..Default::default()
        };
        self.settings.other_vcs.put(settings.clone());
        self.vcs_workspace
            .mappings
            .register_provider(settings.root.clone(), Provider::Tfs);
        let _ = self
            .vcs_workspace
            .mappings
            .save(&self.other_vcs_workspace.project);
        let _ = self.settings.save();
        self.enqueue_provider_request(Request {
            root: settings.root.clone(),
            provider: Provider::Tfs,
            operation: Operation::Initialize,
            args: Arguments {
                source: integration.depot.clone(),
                ..Default::default()
            },
            settings: settings.clone(),
            credentials: Credentials::default(),
        });
        self.other_vcs_workspace
            .panel
            .select(settings.root.clone(), Provider::Tfs, settings);
        self.bottom = Some(BottomTool::OtherVcs);
    }
    pub(super) fn other_vcs_process_labels(&self) -> Vec<String> {
        let mut labels: Vec<_> = other_vcs::runner::poll_retiring()
            .into_iter()
            .map(|label| format!("VCS cleanup: {label}"))
            .collect();
        if let Some(running) = &self.other_vcs_workspace.running {
            labels.push(format!(
                "{} {} ({})",
                running.request.provider.label(),
                running.request.operation.label(),
                running.request.root.display()
            ));
        }
        labels.extend(self.other_vcs_workspace.queue.iter().map(|request| {
            format!(
                "Queued {} {}",
                request.provider.label(),
                request.operation.label()
            )
        }));
        if !self.other_vcs_workspace.file_changes.is_empty() {
            labels.push("Queued VCS file changes".into());
        }
        labels
    }
    pub(super) fn other_vcs_lifecycle_stamp(&self) -> String {
        format!(
            "{:?}",
            (
                self.other_vcs_workspace
                    .running
                    .as_ref()
                    .map(|running| (running.id, &running.request.root)),
                self.other_vcs_workspace
                    .queue
                    .iter()
                    .map(|request| (&request.root, request.provider, request.operation))
                    .collect::<Vec<_>>(),
                self.other_vcs_workspace
                    .file_changes
                    .iter()
                    .map(|change| (&change.root, &change.path, change.removed))
                    .collect::<Vec<_>>(),
                other_vcs::runner::poll_retiring()
            )
        )
    }
    pub(super) fn stop_other_vcs_operations(&mut self) {
        if let Some(running) = &self.other_vcs_workspace.running {
            running.job.stop();
        }
        self.other_vcs_workspace.queue.clear();
        self.other_vcs_workspace.file_changes.clear();
    }
    pub(super) fn route_other_vcs_action(&mut self, action: Action) -> bool {
        if !matches!(
            action,
            Action::ToggleGitView
                | Action::VcsPopup
                | Action::ToggleCommitView
                | Action::Commit
                | Action::Push
                | Action::GitFetch
                | Action::UpdateProject
                | Action::ShowDiff
                | Action::ShowFileHistory
                | Action::Annotate
                | Action::GitBranches
                | Action::StashChanges
        ) {
            return false;
        }
        let path = self
            .active
            .and_then(|index| self.tabs.get(index))
            .and_then(|tab| tab.state.path.clone())
            .or_else(|| self.project.as_ref().map(|p| p.root.clone()));
        let mapped = if self.bottom == Some(BottomTool::OtherVcs) {
            self.other_vcs_workspace.panel.selected.clone()
        } else {
            None
        }
        .or_else(|| {
            path.as_ref()
                .and_then(|path| self.vcs_workspace.mappings.provider_for(path))
                .filter(|(_, provider)| *provider != Provider::Git)
                .map(|(root, provider)| (root.to_path_buf(), provider))
        });
        let Some((root, provider)) = mapped else {
            return false;
        };
        let preferences = self.settings.other_vcs.root(&root);
        self.other_vcs_workspace
            .panel
            .select(root.clone(), provider, preferences);
        if let Some(path) = path.filter(|path| path.starts_with(&root)) {
            self.other_vcs_workspace.panel.set_target(path);
        }
        self.bottom = Some(BottomTool::OtherVcs);
        let operation = match action {
            Action::ToggleGitView | Action::VcsPopup => return true,
            Action::ToggleCommitView | Action::Commit => {
                if provider == Provider::Perforce {
                    Operation::Submit
                } else {
                    Operation::Commit
                }
            }
            Action::Push => match provider {
                Provider::Mercurial => Operation::Push,
                Provider::Subversion => Operation::Commit,
                Provider::Perforce => Operation::Submit,
                Provider::Tfs => Operation::Commit,
                Provider::Git => unreachable!(),
            },
            Action::GitFetch => match provider {
                Provider::Mercurial => Operation::Pull,
                Provider::Subversion => Operation::Update,
                Provider::Perforce => Operation::Sync,
                Provider::Tfs => Operation::Update,
                Provider::Git => unreachable!(),
            },
            Action::UpdateProject => {
                if provider == Provider::Perforce {
                    Operation::Sync
                } else {
                    Operation::Update
                }
            }
            Action::ShowDiff => Operation::Diff,
            Action::ShowFileHistory => {
                if provider == Provider::Perforce {
                    Operation::RevisionGraph
                } else {
                    Operation::Log
                }
            }
            Action::Annotate => Operation::Annotate,
            Action::GitBranches => Operation::Branches,
            Action::StashChanges => {
                if provider == Provider::Subversion {
                    self.other_vcs_workspace.panel.message="Use SVN changelists to group local changes; shelving is provided by Mercurial and Perforce".into();
                    return true;
                } else {
                    Operation::Shelve
                }
            }
            _ => return false,
        };
        if !Operation::available(provider).contains(&operation) {
            self.set_status("This action is unavailable for the selected provider; use its tool window operations");
            return true;
        }
        self.other_vcs_workspace.panel.operation = Some(operation);
        if matches!(
            action,
            Action::ShowDiff | Action::ShowFileHistory | Action::Annotate
        ) {
            if let Some(request) = self.other_vcs_workspace.panel.request(operation) {
                self.enqueue_provider_request(request);
            }
        }
        true
    }
    pub(super) fn poll_other_vcs(&mut self, ctx: &egui::Context) {
        if !self.builtin_plugin_enabled("tfs") || self.project_safe_mode() {
            if let Some(running) = &self.other_vcs_workspace.running {
                if running.request.provider == Provider::Tfs {
                    running.job.stop();
                }
            }
            self.other_vcs_workspace
                .queue
                .retain(|request| request.provider != Provider::Tfs);
            self.other_vcs_workspace
                .file_changes
                .retain(|request| request.provider != Provider::Tfs);
        }
        if !other_vcs::runner::poll_retiring().is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(25));
        }
        let project = self
            .project
            .as_ref()
            .map(|project| project.root.clone())
            .unwrap_or_default();
        if self.other_vcs_workspace.project != project {
            if let Some(running) = self.other_vcs_workspace.running.take() {
                running.job.stop();
                self.git_panel.git_jobs.job_finished(
                    running.id,
                    false,
                    "Operation cancelled on project change",
                );
            }
            self.other_vcs_workspace = OtherVcsWorkspace {
                project: project.clone(),
                ..Default::default()
            };
        }
        let active = self
            .active
            .and_then(|i| self.tabs.get(i))
            .and_then(|tab| tab.state.path.clone());
        if active != self.other_vcs_workspace.last_file {
            if let Some(path) = &active {
                if let Some((root, provider)) = self
                    .vcs_workspace
                    .mappings
                    .provider_for(path)
                    .filter(|(_, p)| *p != Provider::Git)
                {
                    let root = root.to_path_buf();
                    let preferences = self.settings.other_vcs.root(&root);
                    self.other_vcs_workspace
                        .panel
                        .select(root, provider, preferences);
                    self.other_vcs_workspace.panel.set_target(path.clone());
                }
            }
            self.other_vcs_workspace.last_file = active;
        }
        let mut completed = None;
        if let Some(running) = &mut self.other_vcs_workspace.running {
            for event in running.job.poll() {
                match event {
                    JobEvent::Output { stderr: _, text } => {
                        self.other_vcs_workspace.panel.append_console(&text)
                    }
                    JobEvent::Done(done) => completed = Some(done),
                }
            }
            self.other_vcs_workspace.panel.busy = Some(format!(
                "{} · {} · {:.1}s",
                running.request.provider.label(),
                running.request.operation.label(),
                running.job.started.elapsed().as_secs_f32()
            ));
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        if let Some(done) = completed {
            let mut running = self.other_vcs_workspace.running.take().unwrap();
            self.other_vcs_workspace.panel.busy = None;
            self.git_panel
                .git_jobs
                .job_finished(running.id, done.success, &done.stderr);
            running.request.credentials.password.clear();
            let decoded = other_vcs::results::decode(
                done.provider,
                &done.root,
                running.format,
                done.operation,
                &done.stdout,
            );
            let (data, error) = match decoded {
                Ok(data) => (data, done.stderr.clone()),
                Err(error) => (
                    other_vcs::results::Data {
                        raw: done.stdout.clone(),
                        ..Default::default()
                    },
                    format!("{}\nCannot decode provider output: {error}", done.stderr),
                ),
            };
            let success = done.success;
            if success
                && done.provider == Provider::Tfs
                && !self.project_safe_mode()
                && self.builtin_plugin_enabled("tfs")
                && self.vcs_workspace.mappings.providers.get(&done.root) == Some(&Provider::Tfs)
            {
                let next = match done.operation {
                    Operation::Initialize => Some(Operation::MapWorkspace),
                    Operation::MapWorkspace => Some(Operation::Update),
                    _ => None,
                };
                if let Some(operation) = next {
                    let mut request = running.request.clone();
                    request.operation = operation;
                    self.other_vcs_workspace.queue.push_front(request);
                }
            }
            if done.provider == Provider::Tfs
                && !success
                && matches!(
                    done.operation,
                    Operation::Initialize | Operation::MapWorkspace
                )
            {
                self.other_vcs_workspace
                    .queue
                    .retain(|request| request.root != done.root);
            }
            if done.operation == Operation::RevisionGraph
                && !success
                && self.other_vcs_workspace.panel.queued_time_lapse
            {
                self.other_vcs_workspace.queue.clear();
                self.other_vcs_workspace.panel.queued_time_lapse = false;
            }
            self.other_vcs_workspace.panel.accept(ResultView {
                request: running.request.clone(),
                data,
                format: running.format,
                success,
                error,
                truncated: done.truncated,
            });
            self.set_status(format!(
                "{} {} {} at {} ({:.1}s)",
                done.provider.label(),
                done.operation.label(),
                if done.cancelled {
                    "cancelled"
                } else if success {
                    "finished"
                } else {
                    "failed"
                },
                done.root.display(),
                done.elapsed.as_secs_f32()
            ));
            if success && done.operation.mutates() {
                self.reload_changed_files();
                self.reload_analysis_from_disk();
            }
            if success && done.operation == Operation::Checkout {
                let target = PathBuf::from(&running.request.args.destination);
                let target = if target.is_absolute() {
                    target
                } else {
                    done.root.join(target)
                };
                if done.provider.detect(&target) {
                    self.vcs_workspace
                        .mappings
                        .register_provider(target, done.provider);
                    let _ = self.vcs_workspace.mappings.save(&project);
                }
            }
            if done.operation == Operation::TimeLapse {
                self.other_vcs_workspace.panel.queued_time_lapse = false;
            }
        }
        if self.other_vcs_workspace.running.is_none() {
            if let Some(request) = self.other_vcs_workspace.queue.pop_front() {
                self.start_provider_request(request);
            }
        }
        let mut file_decision = None;
        if let Some(change) = self.other_vcs_workspace.file_changes.front() {
            egui::Window::new(format!(
                "{} {} file",
                change.provider.label(),
                if change.removed { "remove" } else { "add" }
            ))
            .id(egui::Id::new("provider_file_policy"))
            .collapsible(false)
            .show_modal(ctx, |ui| {
                ui.label(format!(
                    "{}\n{}",
                    change.root.display(),
                    change.path.display()
                ));
                ui.horizontal(|ui| {
                    if ui
                        .button(if change.removed {
                            "Mark removed"
                        } else {
                            "Add to version control"
                        })
                        .clicked()
                    {
                        file_decision = Some(true);
                    }
                    if ui.button("Leave unversioned / unchanged").clicked() {
                        file_decision = Some(false);
                    }
                });
            });
        }
        if let Some(add) = file_decision {
            if let Some(change) = self.other_vcs_workspace.file_changes.pop_front() {
                if add {
                    let settings = self.settings.other_vcs.root(&change.root);
                    let request = Request {
                        root: change.root,
                        provider: change.provider,
                        operation: if change.removed {
                            Operation::Remove
                        } else {
                            Operation::Add
                        },
                        args: Arguments {
                            paths: vec![change.path],
                            ..Default::default()
                        },
                        settings,
                        credentials: Credentials::default(),
                    };
                    self.enqueue_provider_request(request);
                }
            }
        }
        self.provider_export_window(ctx);
    }
    pub(super) fn other_vcs_tool_window(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if let Some(running) = &self.other_vcs_workspace.running {
                if ui.button("Cancel provider operation").clicked() {
                    running.job.stop();
                    self.other_vcs_workspace.queue.clear();
                    self.other_vcs_workspace.panel.queued_time_lapse = false;
                }
            }
        });
        let roots = self
            .vcs_workspace
            .mappings
            .providers
            .iter()
            .map(|(root, provider)| (root.clone(), *provider))
            .collect::<Vec<_>>();
        let events =
            self.other_vcs_workspace
                .panel
                .ui(ui, &self.theme, &roots, &self.settings.other_vcs);
        for event in events {
            match event {
                Event::Run(request) => self.enqueue_provider_request(*request),
                Event::Mappings => {
                    self.dialog = Dialog::Settings;
                    self.settings_ui.page = settings_ui::SettingsPage::VersionControl;
                }
                Event::Open(path, line) => {
                    self.open_file(&path);
                    if let (Some(line), Some(index)) = (line, self.active) {
                        if let Some(tab) = self.tabs.get_mut(index) {
                            tab.state.go_to_line(line, None);
                        }
                    }
                }
                Event::SaveSettings(settings) => {
                    let valid = other_vcs::validate_endpoint(&settings.endpoint).is_ok();
                    if !valid {
                        self.set_status(
                            "Store credentials in the password field, not in a repository URL",
                        );
                    } else {
                        self.settings.other_vcs.put(settings);
                        match self.settings.save() {
                            Ok(()) => self.set_status("Provider preferences saved"),
                            Err(error) => self.set_status(error.to_string()),
                        }
                    }
                }
                Event::Export(text) => {
                    let path = self
                        .project
                        .as_ref()
                        .map(|p| p.root.join("vcs-output.txt"))
                        .unwrap_or_else(|| PathBuf::from("vcs-output.txt"));
                    self.other_vcs_workspace.export =
                        Some((path.to_string_lossy().into_owned(), text));
                }
            }
        }
    }
    fn enqueue_provider_request(&mut self, request: Request) {
        if request.provider == Provider::Tfs && !self.builtin_plugin_enabled("tfs") {
            self.set_status("The TFS / TFVC optional plugin is disabled");
            return;
        }
        if self.project_safe_mode() {
            self.notify_safe_mode("Version control commands");
            return;
        }
        if self.other_vcs_workspace.queue.len() >= 128 {
            self.set_status(
                "The provider queue is full; wait for its current operations to finish",
            );
            return;
        }
        if self.vcs_workspace.mappings.providers.get(&request.root) != Some(&request.provider) {
            self.set_status("The captured provider root is no longer mapped");
            return;
        }
        if request.operation == Operation::TimeLapse
            && request.provider == Provider::Perforce
            && !self
                .other_vcs_workspace
                .panel
                .graph_matches(&request.root, &request.args.paths)
            && !self.other_vcs_workspace.panel.queued_time_lapse
        {
            let mut history = request.clone();
            history.operation = Operation::RevisionGraph;
            self.other_vcs_workspace.queue.push_back(history);
            self.other_vcs_workspace.panel.queued_time_lapse = true;
        }
        self.other_vcs_workspace.queue.push_back(request);
    }
    fn start_provider_request(&mut self, request: Request) {
        if request.provider == Provider::Tfs && !self.builtin_plugin_enabled("tfs") {
            return;
        }
        if self.project_safe_mode() {
            return;
        }
        let mut history = None;
        if self.vcs_workspace.mappings.providers.get(&request.root) != Some(&request.provider) {
            self.set_status("The queued provider root is no longer mapped");
            return;
        }
        if request.operation.mutates() {
            let unsaved = self.save_for_vcs();
            if unsaved
                .failed
                .iter()
                .chain(unsaved.changed_on_disk.iter())
                .any(|path| path.starts_with(&request.root))
            {
                self.set_status(
                    "Provider operation cancelled because a file in its root could not be saved",
                );
                return;
            }
            let mut paths = request.args.paths.clone();
            if paths.is_empty() {
                paths.push(request.root.clone());
            }
            if let Some(project) = &self.project {
                history = Some((project.root.clone(), paths));
            }
        }
        let preferences = &self.settings.vcs.preferences;
        let executable = match request.provider {
            Provider::Mercurial => &preferences.hg_executable,
            Provider::Subversion => &preferences.svn_executable,
            Provider::Perforce => &preferences.p4_executable,
            Provider::Tfs => &preferences.tf_executable,
            Provider::Git => return,
        };
        let plan = match other_vcs::plan(
            request.provider,
            &request.root,
            request.operation,
            &request.args,
            &request.settings,
            executable,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                self.other_vcs_workspace.panel.message = error.to_string();
                self.set_status(error.to_string());
                return;
            }
        };
        self.other_vcs_workspace.panel.append_console(&format!(
            "\n{} · {}\n",
            request.root.display(),
            plan.display()
        ));
        let format = plan.format;
        let (id, env) = self.git_panel.git_jobs.new_job();
        match ProviderJob::start_with_history(
            plan,
            request.credentials.clone(),
            env,
            &request.settings.endpoint,
            history,
        ) {
            Ok(job) => {
                self.other_vcs_workspace.running = Some(Running {
                    job,
                    id,
                    request,
                    format,
                });
            }
            Err(error) => {
                self.git_panel
                    .git_jobs
                    .job_finished(id, false, &error.to_string());
                self.set_status(error.to_string());
            }
        }
    }
    /// Existing file-created/deleted policies delegate here before considering Git.
    /// None asks, Some(true) adds/removes, Some(false) consumes without a mutation.
    pub(super) fn queue_other_vcs_file_change(
        &mut self,
        path: &Path,
        removed: bool,
        decision: Option<bool>,
    ) -> bool {
        let Some((root, provider)) = self
            .vcs_workspace
            .mappings
            .provider_for(path)
            .filter(|(_, p)| *p != Provider::Git)
        else {
            return false;
        };
        let root = root.to_path_buf();
        match decision {
            Some(false) => {}
            None => {
                if self.other_vcs_workspace.file_changes.len() < 128 {
                    self.other_vcs_workspace.file_changes.push_back(FileChange {
                        root,
                        provider,
                        path: path.to_path_buf(),
                        removed,
                    });
                }
            }
            Some(true) => {
                let settings = self.settings.other_vcs.root(&root);
                self.enqueue_provider_request(Request {
                    root,
                    provider,
                    operation: if removed {
                        Operation::Remove
                    } else {
                        Operation::Add
                    },
                    args: Arguments {
                        paths: vec![path.to_path_buf()],
                        ..Default::default()
                    },
                    settings,
                    credentials: Credentials::default(),
                });
            }
        }
        true
    }
    fn provider_export_window(&mut self, ctx: &egui::Context) {
        let Some((mut path, text)) = self.other_vcs_workspace.export.take() else {
            return;
        };
        let mut open = true;
        let mut save = false;
        egui::Window::new("Export provider result")
            .id(egui::Id::new("provider_export"))
            .open(&mut open)
            .show_modal(ctx, |ui| {
                ui.text_edit_singleline(&mut path);
                save = ui
                    .button(if Path::new(&path).exists() {
                        "Overwrite file"
                    } else {
                        "Save"
                    })
                    .clicked();
            });
        if save {
            match std::fs::write(&path, text) {
                Ok(()) => self.set_status(format!("Exported provider result to {path}")),
                Err(error) => self.set_status(error.to_string()),
            }
        } else if open {
            self.other_vcs_workspace.export = Some((path, text));
        }
    }
}

#[cfg(test)]
mod tfs_tests {
    use super::*;
    #[test]
    fn tfvc_setup_prelaunch_failure_has_no_dependent_map_or_get_worker() {
        let directory = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        settings.trust_project(directory.path());
        settings.vcs.preferences.tf_executable.clear();
        let mut app = IdeApp::new(settings, None);
        app.vcs_workspace
            .mappings
            .register_provider(directory.path().into(), Provider::Tfs);
        let settings = rustpy_settings::other_vcs::RootSettings {
            root: directory.path().into(),
            endpoint: "https://dev.azure.com/owned".into(),
            client: "owned".into(),
            ..Default::default()
        };
        app.enqueue_provider_request(Request {
            root: settings.root.clone(),
            provider: Provider::Tfs,
            operation: Operation::Initialize,
            args: Arguments {
                source: "$/Project/Main".into(),
                ..Default::default()
            },
            settings,
            credentials: Credentials::default(),
        });
        assert_eq!(app.other_vcs_workspace.queue.len(), 1);
        let first = app.other_vcs_workspace.queue.pop_front().unwrap();
        app.start_provider_request(first);
        assert!(app.other_vcs_workspace.running.is_none());
        assert!(
            app.other_vcs_workspace.queue.is_empty(),
            "Map/Get must be created only by successful completion, never prequeued"
        );
        assert!(!directory.path().join("$tf").exists());
    }
    #[test]
    fn disabled_tfvc_plugin_refuses_queue_and_enable_before_any_worker() {
        let directory = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        settings.trust_project(directory.path());
        settings.plugins.disabled_builtins.insert("tfs".into());
        let mut app = IdeApp::new(settings, None);
        app.vcs_workspace
            .mappings
            .register_provider(directory.path().into(), Provider::Tfs);
        let settings = rustpy_settings::other_vcs::RootSettings {
            root: directory.path().into(),
            endpoint: "https://dev.azure.com/owned".into(),
            client: "owned".into(),
            ..Default::default()
        };
        app.enqueue_provider_request(Request {
            root: settings.root.clone(),
            provider: Provider::Tfs,
            operation: Operation::Status,
            args: Arguments::default(),
            credentials: Credentials::default(),
            settings,
        });
        assert!(app.other_vcs_workspace.queue.is_empty());
        assert!(app.other_vcs_workspace.running.is_none());
        app.enable_tfs_integration(rustpy_vcs::provider::Integration {
            provider: Provider::Tfs,
            root: directory.path().into(),
            endpoint: "https://dev.azure.com/owned".into(),
            workspace: "owned".into(),
            depot: "$/Project/Main".into(),
            executable: "/never-executed/tf.exe".into(),
            ..Default::default()
        });
        assert!(app.other_vcs_workspace.queue.is_empty());
        assert!(app.other_vcs_workspace.running.is_none());
    }
}
