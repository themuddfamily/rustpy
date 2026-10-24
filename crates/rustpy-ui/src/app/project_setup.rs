//! UI adapter for project-owned setup planning and progression. Package output,
//! notifications, editor opening and selecting the current project remain here.

use super::*;
pub(super) use rustpy_project::setup::{django_project_name, find_on_path, EnvTool, Step};
use rustpy_project::setup::{CommandTask, Setup, SetupAction, SetupError, SetupEvent};

enum Running {
    Command(CommandTask),
    Pip,
    WorkspaceChoice(PathBuf),
}

pub(super) struct ProjectSetup {
    service: Setup,
    running: Option<Running>,
    open_when_done: Vec<PathBuf>,
    workspace: Option<workspace_ui::WorkspaceClient>,
    managed: Option<rustpy_project::environment_manager::Task>,
    shared_interpreter: bool,
}

impl ProjectSetup {
    pub fn new(
        root: PathBuf,
        base: PathBuf,
        steps: Vec<Step>,
        open_when_done: Vec<PathBuf>,
    ) -> Self {
        Self {
            service: Setup::new(root, base, steps),
            running: None,
            open_when_done,
            workspace: None,
            managed: None,
            shared_interpreter: false,
        }
    }
    pub fn managed(
        root: PathBuf,
        request: Option<rustpy_project::environment_manager::Request>,
        steps: Vec<Step>,
        open_when_done: Vec<PathBuf>,
        shared: bool,
    ) -> Result<Self, String> {
        let base = request
            .as_ref()
            .map(|request| request.base.clone())
            .unwrap_or_else(|| PathBuf::from("python3"));
        let managed = request
            .map(|request| rustpy_project::environment_manager::Task::start(request, true))
            .transpose()?;
        let mut setup = Self::new(root, base, steps, open_when_done);
        setup.managed = managed;
        setup.shared_interpreter = shared;
        Ok(setup)
    }
    pub(super) fn quit_guard_state(&self) -> String {
        let running = match &self.running {
            Some(Running::Command(task)) => format!("command:{}", task.quit_guard_state()),
            Some(Running::Pip) => "pip".into(),
            Some(Running::WorkspaceChoice(path)) => format!("choice:{path:?}"),
            None => "idle".into(),
        };
        format!(
            "{:?}",
            (
                self.service.quit_guard_state(),
                running,
                &self.open_when_done,
                self.managed.as_ref().map(|task| task.quit_guard_state()),
                self.shared_interpreter,
                self.workspace
                    .as_ref()
                    .map(|workspace| workspace.quit_guard_state())
            )
        )
    }
    pub(super) fn stop_quit_process(&mut self) {
        self.service.cancel();
        self.running = None;
        self.managed = None;
    }
    pub fn waits_for_pip(&self) -> bool {
        self.service.waits_for_pip()
    }
}

/// Notification presentation stays in the UI; failure policy belongs to Setup.
fn setup_problem_actions(
    root: &Path,
    env_ok: bool,
    failed: &[Step],
) -> Vec<(String, crate::notifications::NotificationAction)> {
    use crate::notifications::NotificationAction;
    let mut actions = Vec::new();
    if failed.iter().any(|s| matches!(s, Step::CreateEnv { .. })) {
        actions.push((
            "Configure Python Interpreter…".to_string(),
            NotificationAction::OpenSettings("interpreter"),
        ));
    }
    if env_ok && failed.contains(&Step::InstallRequirements) {
        actions.push((
            "Install Requirements".to_string(),
            NotificationAction::InstallRequirements(root.join("requirements.txt")),
        ));
        actions.push((
            "Show Python Packages".to_string(),
            NotificationAction::ShowToolWindow("Python Packages"),
        ));
    }
    actions
}

impl IdeApp {
    pub(super) fn poll_pending_venv(&mut self, ctx: &egui::Context) {
        let Some(mut setup) = self.pending_venv.take() else {
            return;
        };
        if let Some(task) = setup.managed.as_ref() {
            use rustpy_project::environment_manager::Event;
            let mut complete = None;
            while let Some(event) = task.poll() {
                match event {
                    Event::Progress(message) => self.set_status(message),
                    Event::Finished(result) => {
                        complete = Some(result);
                        break;
                    }
                }
            }
            match complete {
                None => {
                    self.pending_venv = Some(setup);
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                    return;
                }
                Some(Ok(interpreter)) => {
                    setup.managed = None;
                    let registry_path = rustpy_project::environment_manager::registry_path();
                    let mut registry =
                        rustpy_project::environment_manager::Registry::load(&registry_path);
                    registry.remember(
                        interpreter.clone(),
                        &setup.service.root,
                        "",
                        setup.shared_interpreter,
                    );
                    if let Err(error) = registry.save(&registry_path) {
                        setup
                            .service
                            .problems
                            .push(format!("Cannot save the interpreter registry: {error}"));
                    }
                    setup.service.set_environment(interpreter.path);
                }
                Some(Err(error)) => {
                    setup.managed = None;
                    setup.service.problems.push(error);
                    setup.service.cancel();
                }
            }
        }
        loop {
            match setup.running.take() {
                Some(Running::WorkspaceChoice(interpreter)) => {
                    setup.running = Some(Running::WorkspaceChoice(interpreter));
                    break;
                }
                Some(Running::Command(task)) => match task.poll() {
                    Some(result) => setup.service.complete(result),
                    None => {
                        setup.running = Some(Running::Command(task));
                        break;
                    }
                },
                Some(Running::Pip) => match self.packages_panel.take_finished() {
                    Some(code) => {
                        let detail = self
                            .packages_panel
                            .take_failure()
                            .map(|(_, d)| d)
                            .filter(|d| !d.trim().is_empty());
                        let result = match (code, detail) {
                            (Some(0), _) => Ok(()),
                            (_, Some(d)) => Err(SetupError::Failed(format!("Installing the requirements failed:\n{d}"))),
                            (_, None) => Err(SetupError::Failed("Installing the requirements failed (see the Python Packages tool window)".into())),
                        };
                        setup.service.complete(result);
                    }
                    None => {
                        setup.running = Some(Running::Pip);
                        break;
                    }
                },
                None => {
                    let uv = find_on_path("uv");
                    let Some(action) = setup
                        .service
                        .next_action(self.packages_panel.is_running(), uv.as_deref())
                    else {
                        break;
                    };
                    match action {
                        SetupAction::Command { step, command } => {
                            match step {
                                Step::Command { ref label, .. } => self.set_status(label.clone()),
                                Step::CreateEnv { tool, .. } => self.set_status(format!(
                                    "Creating virtual environment ({})…",
                                    if tool == EnvTool::Uv && uv.is_some() {
                                        "uv"
                                    } else {
                                        "venv"
                                    }
                                )),
                                Step::DjangoProject { .. } => {
                                    self.set_status("Creating the Django project…")
                                }
                                _ => {}
                            }
                            setup.running = Some(Running::Command(CommandTask::spawn(
                                command,
                                setup.service.command_token(),
                            )));
                        }
                        SetupAction::InstallRequirements {
                            interpreter,
                            requirements,
                        } => {
                            match rustpy_packages::install_requirements_with(
                                &interpreter,
                                &requirements,
                                &[],
                            ) {
                                Ok(task) => {
                                    self.packages_panel.adopt_task(task);
                                    self.bottom = Some(BottomTool::Packages);
                                    self.set_status("Installing requirements into .venv…");
                                    setup.running = Some(Running::Pip);
                                }
                                Err(error) => setup.service.complete(Err(SetupError::Failed(
                                    format!("Cannot install requirements: {error}"),
                                ))),
                            }
                        }
                        SetupAction::SelectEnvironment { interpreter } => {
                            let root = setup.service.root.clone();
                            if let Err(error) =
                                self.save_setup_workspace_choice(&mut setup, &interpreter)
                            {
                                setup.running = Some(Running::WorkspaceChoice(interpreter));
                                self.notify(crate::notifications::NotificationKind::Error, "Project Setup", "Cannot save the interpreter choice", error, vec![("Retry".into(), crate::notifications::NotificationAction::RetrySetupWorkspace)]);
                                break;
                            }
                            if self.project.as_ref().is_some_and(|p| p.root == root) {
                                self.rediscover_interpreters();
                            }
                            setup.service.complete(Ok(()));
                        }
                        SetupAction::Finished => {
                            self.finish_setup(&setup);
                            return;
                        }
                    }
                }
            }
            for event in setup.service.take_events() {
                if matches!(
                    event,
                    SetupEvent::StepFinished {
                        step: Step::CreateEnv { .. },
                        result: Ok(())
                    }
                ) {
                    self.set_status(format!(
                        "Virtual environment created in {}",
                        setup.service.root.join(".venv").display()
                    ));
                }
            }
        }
        self.pending_venv = Some(setup);
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }

    fn save_setup_workspace_choice(
        &mut self,
        setup: &mut ProjectSetup,
        interpreter: &Path,
    ) -> Result<(), String> {
        if self
            .project
            .as_ref()
            .is_some_and(|p| p.root == setup.service.root)
        {
            self.record_interpreter_choice(interpreter);
            self.workspace_client
                .as_ref()
                .and_then(|client| client.error.clone())
                .map_or(Ok(()), Err)
        } else {
            let client = setup
                .workspace
                .get_or_insert_with(|| workspace_ui::WorkspaceClient::open(&setup.service.root));
            client.pending.interpreter = Some(Some(interpreter.to_path_buf()));
            let result = client.commit(&client.effective(), false);
            if let Err(error) = &result {
                client.error = Some(error.clone());
            }
            result
        }
    }
    pub(super) fn setup_workspace_choice_error(&self) -> Option<String> {
        let setup = self.pending_venv.as_ref()?;
        setup
            .workspace
            .as_ref()
            .and_then(|client| client.error.clone())
    }
    pub(super) fn setup_workspace_choice_pending(&self) -> bool {
        self.pending_venv
            .as_ref()
            .is_some_and(|setup| matches!(setup.running, Some(Running::WorkspaceChoice(_))))
    }
    pub(super) fn discard_setup_workspace_choice(&mut self) {
        if self.setup_workspace_choice_pending() {
            self.pending_venv = None;
        }
    }
    pub(super) fn retry_setup_workspace_choice(&mut self) -> bool {
        let Some(mut setup) = self.pending_venv.take() else {
            return true;
        };
        let Some(Running::WorkspaceChoice(interpreter)) = setup.running.take() else {
            self.pending_venv = Some(setup);
            return true;
        };
        let recovery = if let Some(client) = &mut setup.workspace {
            client.recover_baseline()
        } else {
            self.recover_workspace_baseline()
        };
        let result =
            recovery.and_then(|()| self.save_setup_workspace_choice(&mut setup, &interpreter));
        match result {
            Ok(()) => {
                setup.service.complete(Ok(()));
                self.pending_venv = Some(setup);
                true
            }
            Err(error) => {
                setup.running = Some(Running::WorkspaceChoice(interpreter));
                self.pending_venv = Some(setup);
                self.notify_error("Project Setup", "Cannot save the interpreter choice", error);
                false
            }
        }
    }

    fn finish_setup(&mut self, setup: &ProjectSetup) {
        let service = &setup.service;
        self.seed_new_project_dialog_defaults(&service.root);
        if self
            .project
            .as_ref()
            .is_some_and(|p| p.root == service.root)
        {
            if let Err(error) =
                rustpy_settings::dialogs::load_project(&service.root, &mut self.settings)
            {
                self.set_status(format!("Project preferences: {error}"));
            }
            if let Some(p) = &mut self.project {
                p.tree.invalidate_all();
            }
            for file in &setup.open_when_done {
                if file.is_file() {
                    self.open_file(file);
                }
            }
        }
        if service.problems.is_empty() {
            self.set_status(format!("Project {} is ready", service.root.display()));
        } else {
            let serious = service
                .failed
                .iter()
                .any(|s| matches!(s, Step::CreateEnv { .. } | Step::InstallRequirements));
            let kind = if serious {
                crate::notifications::NotificationKind::Error
            } else {
                crate::notifications::NotificationKind::Warning
            };
            let name = service
                .root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| service.root.display().to_string());
            self.notify(
                kind,
                "Project Setup",
                format!("Project {name} was set up with problems"),
                service.problems.join("\n"),
                setup_problem_actions(&service.root, service.env_ok, &service.failed),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use rustpy_project::setup::plan;

    #[test]
    fn setup_problems_offer_the_fitting_links() {
        use crate::notifications::NotificationAction;
        let root = Path::new("/p");
        let env = Step::CreateEnv {
            tool: EnvTool::Venv,
            inherit_site_packages: false,
        };
        let labels = |actions: Vec<(String, NotificationAction)>| {
            actions.into_iter().map(|(l, _)| l).collect::<Vec<_>>()
        };
        assert_eq!(
            labels(setup_problem_actions(
                root,
                false,
                std::slice::from_ref(&env)
            )),
            vec!["Configure Python Interpreter…"]
        );
        let pip = setup_problem_actions(root, true, &[Step::InstallRequirements]);
        assert_eq!(
            pip[0].1,
            NotificationAction::InstallRequirements(root.join("requirements.txt")),
            "retry installing into the new environment"
        );
        assert_eq!(
            labels(pip),
            vec!["Install Requirements", "Show Python Packages"]
        );
        assert!(
            setup_problem_actions(root, true, &[Step::DjangoProject { name: "s".into() }])
                .is_empty()
        );
        let mut setup = ProjectSetup::new(
            root.to_path_buf(),
            PathBuf::from("python3"),
            vec![Step::InstallRequirements],
            Vec::new(),
        );
        assert!(!setup.waits_for_pip());
        setup.service.env_ok = true;
        assert!(matches!(
            setup.service.next_action(false, None),
            Some(SetupAction::InstallRequirements { .. })
        ));
        assert!(
            setup.waits_for_pip(),
            "a failing pip is reported by the setup, not twice"
        );
    }
    #[cfg(unix)]
    #[test]
    fn settings_environment_completion_does_not_select_another_projects_interpreter() {
        let original = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        assert!(settings.trust_project(other.path()));
        let mut app = IdeApp::new(settings, None);
        let root = other.path().to_path_buf();
        app.project_trust_identity =
            Some(rustpy_settings::trust::DirectoryIdentity::capture(&root).unwrap());
        app.project = Some(Project {
            root: root.clone(),
            tree: ProjectTree::new(root.clone()),
            analysis: Arc::new(Analysis::for_project(&root)),
            run_configs: RunConfigStore::default(),
            interpreters: Vec::new(),
            interpreter: None,
            interpreter_rx: None,
            git: None,
            git_refreshed: -100.0,
            git_branch: None,
        });
        assert!(!app.project_safe_mode());
        let target = original.path().join(".venv");
        let mut service = Setup::new(
            original.path().into(),
            PathBuf::from("/bin/true"),
            plan(
                rustpy_project::setup::ProjectKind::Pure,
                true,
                EnvTool::Venv,
                false,
                false,
                "",
            ),
        );
        let Some(SetupAction::Command { command, .. }) = service.next_action(false, None) else {
            panic!("environment command");
        };
        let task = CommandTask::spawn(command, service.command_token());
        app.settings_ui.venv_rx = Some((target.clone(), service, task));
        let context = egui::Context::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.settings_ui.venv_rx.is_some() && std::time::Instant::now() < deadline {
            app.poll_interpreter_venv(&context);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(app.settings_ui.venv_rx.is_none());
        assert!(app.project.as_ref().unwrap().interpreter.is_none());
        assert!(app.project.as_ref().unwrap().interpreters.is_empty());
        assert_eq!(
            app.settings_ui.venv_status.as_deref(),
            Some("Virtual environment created")
        );
        assert!(app
            .status
            .as_ref()
            .unwrap()
            .0
            .contains(&target.display().to_string()));
    }
}

#[cfg(test)]
mod workspace_choice_tests {
    use super::*;
    #[test]
    fn actual_setup_poll_retains_off_project_choice_until_explicit_retry() {
        let current = tempfile::tempdir().unwrap();
        let setup_root = tempfile::tempdir().unwrap();
        let path = Workspace::path_for(setup_root.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"damaged setup workspace").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(current.path());
        let mut app = IdeApp::new(settings, Some(current.path().to_path_buf()));
        let mut setup = ProjectSetup::new(
            setup_root.path().to_path_buf(),
            PathBuf::from("/usr/bin/python3"),
            vec![Step::SelectEnv],
            Vec::new(),
        );
        setup.service.env_ok = true;
        app.pending_venv = Some(Box::new(setup));
        let ctx = egui::Context::default();
        app.poll_pending_venv(&ctx);
        assert!(app.setup_workspace_choice_pending());
        assert_eq!(std::fs::read(&path).unwrap(), b"damaged setup workspace");
        assert!(app
            .notifications
            .items()
            .iter()
            .any(|n| n.title == "Cannot save the interpreter choice"));
        std::fs::remove_file(&path).unwrap();
        assert!(app.retry_setup_workspace_choice());
        app.poll_pending_venv(&ctx);
        assert!(!app.setup_workspace_choice_pending());
        assert_eq!(
            Workspace::try_load(setup_root.path()).unwrap().interpreter,
            Some(rustpy_project::setup::venv_python(setup_root.path()))
        );
        assert_eq!(app.project.as_ref().unwrap().root, current.path());
    }
}
