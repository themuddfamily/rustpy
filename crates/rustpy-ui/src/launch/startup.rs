//! Actual launcher startup. Restoration starts from an empty app, retains its
//! checked project until acceptance, and confirms only after endpoint publication.
use super::window_owner::WindowOwnerLifetime;
use crate::app::IdeApp;
use rustpy_cli::{
    args::{GlobalOptions, OpenArgs},
    ipc::Server,
    window_admission::{AdmissionLease, WindowAdmission},
    window_sessions::{
        self, PendingRestore, PrimaryPlan, RestoreProgress, RestoreRound, StartupRestore,
    },
};
use rustpy_settings::Settings;
use std::{
    path::{Path, PathBuf},
    thread::JoinHandle,
    time::{Duration, Instant},
};

pub(super) struct StartupWindow {
    pub app: IdeApp,
    pub owner: WindowOwnerLifetime,
    pub restore: StartupState,
}
type RestoreWork = (
    Box<RestoreRound>,
    Option<Server>,
    Result<RestoreProgress, String>,
);
pub(super) struct StartupState {
    confirmation: Option<PrimaryPlan>,
    round: Option<Box<RestoreRound>>,
    worker: Option<JoinHandle<RestoreWork>>,
    // Keep the OS restore claim through uncertainty. Never automatically respawn.
    uncertain: Option<PendingRestore>,
    started: bool,
    progress: Option<RestoreProgress>,
    global: GlobalOptions,
    program: PathBuf,
    cwd: PathBuf,
    next_poll: Instant,
    warning: Option<String>,
}
fn describe_issues(issues: &[window_sessions::RestoreIssue]) -> String {
    use window_sessions::RestoreIssue;
    let project = |path: &Option<PathBuf>| {
        path.as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "Welcome".into())
    };
    issues
        .iter()
        .map(|issue| match issue {
            RestoreIssue::MissingProject(path) => format!("Missing project: {}", path.display()),
            RestoreIssue::InvalidProject { path, reason } => {
                format!("Cannot restore {}: {reason}", path.display())
            }
            RestoreIssue::DuplicateProject(path) => {
                format!("Duplicate previous project: {}", project(path))
            }
            RestoreIssue::SpawnFailed {
                project: path,
                reason,
            } => format!("Cannot start {}: {reason}", project(path)),
            RestoreIssue::ChildExited {
                project: path,
                status,
            } => format!("{} exited during startup: {status}", project(path)),
            RestoreIssue::Unknown { project: path, .. } => format!(
                "{} has not confirmed startup; it will not be launched twice",
                project(path)
            ),
            RestoreIssue::AlreadyOpen(path) => format!("Already open: {}", project(path)),
            RestoreIssue::Deferred(path) => format!("Startup is deferred for {}", project(path)),
            RestoreIssue::RegistryUncertain(_) => {
                "Another window has not confirmed its identity".into()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
impl StartupState {
    pub(super) fn empty(global: &GlobalOptions) -> Self {
        Self {
            confirmation: None,
            round: None,
            worker: None,
            uncertain: None,
            started: false,
            progress: None,
            global: global.clone(),
            program: std::env::current_exe().unwrap_or_default(),
            cwd: std::env::current_dir().unwrap_or_default(),
            next_poll: Instant::now(),
            warning: None,
        }
    }
    pub(super) fn awaiting_confirmation(&self) -> bool {
        self.confirmation.is_some()
    }
    pub(super) fn unresolved(&self) -> bool {
        self.confirmation.is_some()
            || self.uncertain.is_some()
            || self.worker.is_some()
            || (self.round.is_some()
                && self.progress.as_ref().is_none_or(|progress| {
                    progress.pending != 0
                        || progress.unsubmitted != 0
                        || progress.issues.iter().any(|issue| {
                            matches!(
                                issue,
                                window_sessions::RestoreIssue::Unknown { .. }
                                    | window_sessions::RestoreIssue::Deferred(_)
                                    | window_sessions::RestoreIssue::RegistryUncertain(_)
                            )
                        })
                }))
    }
    pub(super) fn report_warning(&mut self, error: String) {
        self.warning = Some(error);
    }
    pub(super) fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }
    pub(super) fn confirm(
        &mut self,
        server: &Server,
        owner: &WindowOwnerLifetime,
        lease: &AdmissionLease,
    ) -> Result<(), String> {
        let Some(plan) = &self.confirmation else {
            return Ok(());
        };
        lease
            .validate_identity()
            .map_err(|error| error.to_string())?;
        plan.confirm_after_publish(server, owner.token(), Duration::from_millis(100))
            .map_err(|error| error.to_string())?;
        lease
            .validate_identity()
            .map_err(|error| error.to_string())?;
        self.confirmation = None;
        self.warning = None;
        Ok(())
    }
    pub(super) fn drive(&mut self, config: &Path, server: &mut Option<Server>) {
        if self.confirmation.is_some() || self.uncertain.is_some() {
            return;
        }
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            let work = self.worker.take().unwrap();
            match work.join() {
                Ok((round, returned_server, result)) => {
                    if let Some(returned_server) = returned_server {
                        *server = Some(returned_server);
                    }
                    self.round = Some(round);
                    match result {
                        Ok(progress) => {
                            self.warning = (!progress.issues.is_empty()).then(|| describe_issues(&progress.issues));
                            self.progress = Some(progress);
                        }
                        Err(error) => self.warning = Some(error),
                    }
                }
                Err(_) => self.warning = Some("Previous-window restoration stopped unexpectedly; no windows will be resubmitted.".into()),
            }
            self.next_poll = Instant::now() + Duration::from_millis(100);
        }
        if self.worker.is_some() || Instant::now() < self.next_poll {
            return;
        }
        if self.started
            && self
                .progress
                .as_ref()
                .is_some_and(|progress| progress.pending == 0)
            && self
                .round
                .as_ref()
                .is_some_and(|round| round.unsubmitted_count() == 0)
        {
            return;
        }
        let submitting = !self.started
            || self
                .round
                .as_ref()
                .is_some_and(|round| round.unsubmitted_count() != 0 && round.pending_count() == 0);
        if self.round.is_none() || (submitting && server.is_none()) {
            return;
        }
        let Ok(admission) = WindowAdmission::new(config)
            .and_then(|gate| gate.acquire_shared(Instant::now() + Duration::from_millis(5)))
        else {
            return;
        };
        let mut round = self.round.take().unwrap();
        let started = self.started;
        self.started = true;
        let global = self.global.clone();
        let program = self.program.clone();
        let cwd = self.cwd.clone();
        // Each bounded submission pass briefly owns the real Server; its listener
        // and waker remain live. Poll-only passes leave it in the GUI.
        let parent = if submitting { server.take() } else { None };
        self.worker = Some(std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                admission.validate_identity().map_err(|error| error.to_string())?;
                let progress = if let Some(parent) = &parent {
                    if started {
                        round.resume_children(parent, &program, &global, &cwd, Duration::from_millis(500))
                    } else {
                        round.restore_children(parent, &program, &global, &cwd, Duration::from_millis(500))
                    }
                } else {
                    round.poll_children(Duration::from_millis(50))
                }.map_err(|error| error.to_string())?;
                admission.validate_identity().map_err(|error| error.to_string())?;
                Ok(progress)
            })).unwrap_or_else(|_| Err("Previous-window restoration stopped unexpectedly; no windows will be resubmitted.".into()));
            (round, parent, result)
        }));
    }
}

pub(super) fn initialize(
    settings: Settings,
    args: &OpenArgs,
    project: Option<PathBuf>,
    config: &Path,
    server: &Server,
    lease: &AdmissionLease,
    global: &GlobalOptions,
) -> Result<StartupWindow, String> {
    lease
        .validate_identity()
        .map_err(|error| error.to_string())?;
    let startup = window_sessions::prepare_startup(
        config,
        args,
        settings.reopen_last_project,
        server,
        Duration::from_secs(2),
    )
    .map_err(|error| error.to_string())?;
    let mut restore = StartupState::empty(global);
    let plan = match startup {
        StartupRestore::Child(plan) => Some(plan),
        StartupRestore::Restore(round) => {
            let plan = round.primary().clone();
            restore.round = Some(round);
            Some(plan)
        }
        StartupRestore::Uncertain(pending) => {
            restore.warning = Some("Previous-window startup is uncertain. RustPy has kept the restore claim and will not launch duplicate windows.".into());
            restore.uncertain = Some(pending);
            None
        }
        StartupRestore::Skipped(_) => None,
    };
    if let Some(plan) = plan {
        let prepared = plan
            .project()
            .map(IdeApp::prepare_startup_project)
            .transpose()?;
        let mut app = IdeApp::new_welcome(settings);
        lease
            .validate_identity()
            .map_err(|error| error.to_string())?;
        let mutation = plan
            .accept_before_publish(server, Duration::from_secs(2))
            .map_err(|error| error.to_string())?;
        let token = mutation.token;
        let installed = prepared
            .map(|prepared| app.install_startup_project(prepared))
            .transpose();
        if let Err(error) = installed {
            let retirement = lease
                .validate_identity()
                .map_err(|error| error.to_string())
                .and_then(|_| {
                    plan.store()
                        .close_window(&token)
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                });
            return Err(match retirement {
                Ok(()) => error,
                Err(retire) => {
                    format!("{error}; accepted startup owner retirement is uncertain: {retire}")
                }
            });
        }
        let owner = match WindowOwnerLifetime::from_accepted(config, token.clone(), plan.project())
        {
            Ok(owner) => owner,
            Err(error) => {
                let retirement = lease
                    .validate_identity()
                    .map_err(|error| error.to_string())
                    .and_then(|_| {
                        plan.store()
                            .close_window(&token)
                            .map(|_| ())
                            .map_err(|error| error.to_string())
                    });
                return Err(format!(
                    "{error}; accepted startup owner retirement: {retirement:?}"
                ));
            }
        };
        restore.confirmation = Some(plan);
        return Ok(StartupWindow {
            app,
            owner,
            restore,
        });
    }
    let reopen = settings.reopen_last_project;
    let quiet = args.dont_reopen_projects && project.is_none();
    let mut initial = settings;
    if quiet {
        initial.reopen_last_project = false;
    }
    let mut app = if quiet || args.welcome || restore.uncertain.is_some() {
        IdeApp::new_welcome(initial)
    } else {
        IdeApp::new(initial, project.clone())
    };
    if quiet {
        app.set_reopen_last_project(reopen);
    }
    if restore.uncertain.is_none() {
        app.open_cli_targets(&args.targets, true);
        if let Some(file) = &args.open_file {
            let file = match &project {
                Some(root) if file.is_relative() && !file.exists() => root.join(file),
                _ => file.clone(),
            };
            app.open_file(&file);
        }
    }
    let owner =
        WindowOwnerLifetime::accept(config, server, app.actual_current_project_root(), lease)
            .map_err(|error| error.to_string())?;
    Ok(StartupWindow {
        app,
        owner,
        restore,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_cli::{ipc::query_window_owner, window_registry};
    use rustpy_settings::window_session_store::{WindowOwnerId, WindowSessionStore};
    fn checkpoint(config: &Path, project: &Path) -> WindowOwnerId {
        // This fixture publishes input checkpoint data through the legacy Store
        // API. It is not evidence that any UI guard approved application Quit.
        let store = WindowSessionStore::new(config);
        let owner = WindowOwnerId::new();
        let accepted = store
            .accept_window(&store.load().unwrap(), owner.clone(), Some(project))
            .unwrap();
        let plan = store.prepare_quit(&accepted.snapshot).unwrap();
        store.quit_application(plan).unwrap();
        owner
    }
    fn lease(config: &Path) -> AdmissionLease {
        WindowAdmission::new(config)
            .unwrap()
            .acquire_shared(Instant::now() + Duration::from_secs(2))
            .unwrap()
    }
    #[test]
    fn actual_restored_primary_installs_checked_project_and_confirms_exact_owner_after_publication()
    {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let project = dir.path().join("checkpoint");
        let recent = dir.path().join("unrelated-recent");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&recent).unwrap();
        let original_owner = checkpoint(&config, &project);
        let store = WindowSessionStore::new(&config);
        let original_checkpoint = store.load().unwrap().checkpoint().unwrap().clone();
        let lease = lease(&config);
        let server = Server::start_secondary(&config).unwrap();
        let settings = Settings {
            reopen_last_project: true,
            recent_projects: vec![recent.clone()],
            ..Default::default()
        };
        let mut startup = initialize(
            settings,
            &OpenArgs::default(),
            None,
            &config,
            &server,
            &lease,
            &GlobalOptions::default(),
        )
        .unwrap();
        assert_eq!(
            startup.app.actual_current_project_root(),
            Some(dunce::canonicalize(&project).unwrap().as_path())
        );
        assert_eq!(startup.owner.token().owner(), &original_owner);
        assert!(startup.restore.awaiting_confirmation());
        assert!(!recent.join(".rustpy/workspace.json").exists());
        let window = window_registry::live_windows(&config)
            .unwrap()
            .pop()
            .unwrap();
        assert!(window.project.is_none());
        assert!(query_window_owner(&window, Duration::from_secs(1)).is_err());
        startup
            .owner
            .sync(&server, startup.app.actual_current_project_root(), &lease)
            .unwrap();
        startup
            .restore
            .confirm(&server, &startup.owner, &lease)
            .unwrap();
        assert!(!startup.restore.awaiting_confirmation());
        let window = window_registry::live_windows(&config)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(
            query_window_owner(&window, Duration::from_secs(1)).unwrap(),
            startup.owner.state_stamp()
        );
        assert_eq!(
            store.load().unwrap().checkpoint(),
            Some(&original_checkpoint)
        );
    }
    #[test]
    fn malformed_restored_model_refuses_before_logical_acceptance_or_project_publication() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let project = dir.path().join("broken");
        std::fs::create_dir_all(project.join(".rustpy")).unwrap();
        let model = rustpy_project::ProjectModel::path_for(&project);
        std::fs::write(&model, "{broken").unwrap();
        checkpoint(&config, &project);
        let store = WindowSessionStore::new(&config);
        let original_checkpoint = store.load().unwrap().checkpoint().unwrap().clone();
        let lease = lease(&config);
        let server = Server::start_secondary(&config).unwrap();
        let settings = Settings {
            reopen_last_project: true,
            ..Default::default()
        };
        assert!(initialize(
            settings,
            &OpenArgs::default(),
            None,
            &config,
            &server,
            &lease,
            &GlobalOptions::default()
        )
        .is_err());
        assert_eq!(std::fs::read_to_string(&model).unwrap(), "{broken");
        assert!(store.load().unwrap().live_windows().is_empty());
        assert_eq!(
            store.load().unwrap().checkpoint(),
            Some(&original_checkpoint)
        );
        let window = window_registry::live_windows(&config)
            .unwrap()
            .pop()
            .unwrap();
        assert!(window.project.is_none());
        assert!(query_window_owner(&window, Duration::from_secs(1)).is_err());
    }
    #[test]
    fn dont_reopen_builds_real_welcome_owner_and_preserves_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let project = dir.path().join("recent");
        std::fs::create_dir_all(&project).unwrap();
        checkpoint(&config, &project);
        let store = WindowSessionStore::new(&config);
        let original_checkpoint = store.load().unwrap().checkpoint().unwrap().clone();
        let lease = lease(&config);
        let server = Server::start_secondary(&config).unwrap();
        let settings = Settings {
            reopen_last_project: true,
            recent_projects: vec![project.clone()],
            ..Default::default()
        };
        let args = OpenArgs {
            dont_reopen_projects: true,
            ..Default::default()
        };
        let startup = initialize(
            settings,
            &args,
            None,
            &config,
            &server,
            &lease,
            &GlobalOptions::default(),
        )
        .unwrap();
        assert!(startup.app.actual_current_project_root().is_none());
        assert!(startup.owner.project().is_none());
        assert!(!project.join(".rustpy/workspace.json").exists());
        assert_eq!(
            store.load().unwrap().checkpoint(),
            Some(&original_checkpoint)
        );
        assert_eq!(store.load().unwrap().live_windows().len(), 1);
    }
}
