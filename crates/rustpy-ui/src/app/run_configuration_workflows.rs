//! Ordered prerequisites finish successfully before a configuration starts.
use super::*;
use crate::run_context::RunMode;
use rustpy_core::{TaskError, TaskHandle, TaskManager};
use rustpy_project::run_workflow::{self, Planned, Task};

#[derive(Default)]
pub(super) struct State {
    pending: Vec<Pending>,
    approved: Option<(PathBuf, RunConfig, RunMode)>,
    replacements: Vec<Replacement>,
}
#[derive(Debug)]
pub(super) enum ReplacementLaunch {
    Configuration(Box<RunConfig>, RunMode),
    Command(String, crate::run_anything::CommandSpec),
}
struct Replacement {
    root: PathBuf,
    analysis: Arc<Analysis>,
    file_session: u64,
    old: u64,
    launch: ReplacementLaunch,
    configuration_identity: Option<RunConfig>,
}
struct Pending {
    root: PathBuf,
    analysis: Arc<Analysis>,
    file_session: u64,
    cfg: RunConfig,
    mode: RunMode,
    replaces: Option<u64>,
    task: TaskHandle<()>,
    ssh_auth: Option<u64>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.task.cancel();
    }
}
enum Action {
    Command {
        command: std::process::Command,
        input: Option<PathBuf>,
    },
    Upload {
        server: Box<rustpy_deploy::Server>,
        path: PathBuf,
        root: PathBuf,
    },
}
struct Step {
    label: String,
    action: Action,
}

impl IdeApp {
    /// Stop is nonblocking. A replacement must await the process owner's
    /// physical retirement before executing (servers may reuse its socket).
    pub(super) fn defer_run_replacement(
        &mut self,
        old: Option<u64>,
        launch: ReplacementLaunch,
    ) -> bool {
        let Some(old) = old else { return false };
        let Some(session) = self.run_panel.sessions.iter_mut().find(|s| s.id == old) else {
            return false;
        };
        session.poll();
        if !session.owns_process() {
            return false;
        }
        let Some(project) = &self.project else {
            return false;
        };
        session.stop();
        if !self
            .run_workflows
            .replacements
            .iter()
            .any(|pending| pending.old == old)
        {
            self.run_workflows.replacements.push(Replacement {
                root: project.root.clone(),
                analysis: project.analysis.clone(),
                file_session: self.file_session,
                old,
                configuration_identity: match &launch {
                    ReplacementLaunch::Configuration(cfg, _) => project
                        .run_configs
                        .configs
                        .iter()
                        .find(|stored| stored.name == cfg.name)
                        .cloned(),
                    ReplacementLaunch::Command(_, _) => None,
                },
                launch,
            });
        }
        true
    }

    pub(super) fn approve_run_configuration(&mut self, cfg: &RunConfig, mode: RunMode) {
        if let Some(project) = &self.project {
            self.run_workflows.approved = Some((project.root.clone(), cfg.clone(), mode));
        }
    }
    pub(super) fn before_launch_requires_prompt(&self, cfg: &RunConfig) -> bool {
        let Some(project) = &self.project else {
            return false;
        };
        let Ok(plan) = run_workflow::before_plan(cfg, &project.run_configs.configs) else {
            return false;
        };
        let context = self.macro_context(None);
        plan.iter().any(|entry| {
            let Planned::Task(Task::ExternalTool { name }) = entry else {
                return false;
            };
            self.settings
                .external_tools
                .iter()
                .find(|tool| tool.name == *name)
                .is_some_and(|tool| {
                    matches!(
                        crate::external_tool_workspace::plan(tool, &context),
                        Err(crate::external_tool_workspace::PlanError::Invocation(
                            crate::external_tools::InvocationError::NeedsPrompt
                        ))
                    )
                })
        })
    }
    /// A true result consumes this launch (dialog, pending work or refusal).
    pub(super) fn prepare_run_workflow(
        &mut self,
        cfg: &RunConfig,
        mode: RunMode,
        replaces: Option<u64>,
        prompt: Option<&str>,
    ) -> bool {
        if self.project_safe_mode() {
            self.set_status("Trust the project before starting before-launch tasks");
            return true;
        }
        let Some(project) = &self.project else {
            return false;
        };
        let approved =
            self.run_workflows
                .approved
                .take()
                .is_some_and(|(root, approved, approved_mode)| {
                    root == project.root && approved == *cfg && approved_mode == mode
                });
        if cfg.workflow.show_settings && !approved {
            self.dialog = Dialog::RunConfigurations;
            self.set_status("Review the selected configuration and choose Run or Debug");
            return true;
        }
        if let Some(problem) = self.parallel_configuration_problem(cfg, replaces) {
            self.set_status(problem);
            return true;
        }
        let Some(project) = &self.project else {
            return false;
        };
        let plan = match run_workflow::before_plan(cfg, &project.run_configs.configs) {
            Ok(plan) => plan,
            Err(error) => {
                self.set_status(error);
                return true;
            }
        };
        if plan.is_empty() {
            return false;
        }
        if !self.save_all() {
            return true;
        }
        let needs_ssh = plan.iter().any(|entry| match entry {
            Planned::Task(Task::ExternalTool { name }) => self
                .settings
                .external_tools
                .iter()
                .any(|tool| tool.name == *name && tool.options.ssh.is_some()),
            _ => false,
        });
        let ssh_auth = needs_ssh.then(|| self.git_panel.git_jobs.new_job());
        let planned = match &ssh_auth {
            Some((_, environment)) => self.prepare_before_steps_authorized(
                cfg,
                plan.clone(),
                prompt,
                Some(&environment.vars),
            ),
            None => self.prepare_before_steps(cfg, plan.clone(), prompt),
        };
        let steps = match planned {
            Ok(steps) => steps,
            Err(error) => {
                if let Some((id, _)) = &ssh_auth {
                    self.git_panel
                        .git_jobs
                        .job_finished(*id, false, "Before-launch plan failed");
                }
                self.set_status(error);
                return true;
            }
        };
        if let Err(error) = self.prepare_watcher_prerequisites(&plan) {
            if let Some((id, _)) = &ssh_auth {
                self.git_panel.git_jobs.job_finished(
                    *id,
                    false,
                    "Before-launch watcher preparation failed",
                );
            }
            self.set_status(error);
            return true;
        }
        let Some(project) = &self.project else {
            return true;
        };
        let task = TaskManager::global()
            .task(format!("Before launch: {}", cfg.name))
            .spawn(move |progress| {
                let count = steps.len();
                for (index, step) in steps.into_iter().enumerate() {
                    progress.check()?;
                    progress.set_text(&step.label);
                    progress.set_fraction(index as f64 / count.max(1) as f64);
                    match step.action {
                        Action::Command { mut command, input } => {
                            if let Some(path) = input {
                                command.stdin(std::fs::File::open(path)?);
                            } else {
                                command.stdin(std::process::Stdio::null());
                            }
                            let (status, stdout, stderr) =
                                rustpy_project::native_skeleton::supervise_tool_status(
                                    &mut command,
                                    progress.token(),
                                    std::time::Duration::from_secs(15 * 60),
                                    4 * 1024 * 1024,
                                )
                                .map_err(|error| {
                                    if progress.is_cancelled() {
                                        TaskError::Cancelled
                                    } else {
                                        TaskError::Failed(format!("{}: {error}", step.label))
                                    }
                                })?;
                            if !status.success() {
                                let output = if stderr.is_empty() { &stdout } else { &stderr };
                                return Err(TaskError::Failed(format!(
                                    "{} failed ({status}): {}",
                                    step.label,
                                    String::from_utf8_lossy(output)
                                        .chars()
                                        .take(4096)
                                        .collect::<String>()
                                )));
                            }
                        }
                        Action::Upload { server, path, root } => {
                            let transport = crate::deploy_ui::transport(&server)
                                .map_err(|error| TaskError::Failed(error.to_string()))?;
                            let results = rustpy_deploy::sync::upload_paths_while(
                                transport.as_ref(),
                                &server,
                                &root,
                                &[path],
                                |_, _, path| {
                                    progress.set_details(path);
                                    !progress.is_cancelled()
                                },
                            );
                            progress.check()?;
                            for (path, result) in results {
                                result.map_err(|error| {
                                    TaskError::Failed(format!("Upload {}: {error}", path.display()))
                                })?;
                            }
                        }
                    }
                }
                progress.set_fraction(1.0);
                Ok(())
            });
        self.run_workflows.pending.push(Pending {
            root: project.root.clone(),
            analysis: project.analysis.clone(),
            file_session: self.file_session,
            cfg: cfg.clone(),
            mode,
            replaces,
            task,
            ssh_auth: ssh_auth.map(|(id, _)| id),
        });
        self.set_status(format!("Running before-launch tasks for {}", cfg.name));
        true
    }
    fn parallel_configuration_problem(
        &self,
        cfg: &RunConfig,
        replaces: Option<u64>,
    ) -> Option<String> {
        if cfg.workflow.allow_parallel {
            return None;
        }
        let pending = self
            .run_workflows
            .pending
            .iter()
            .any(|pending| pending.cfg.name == cfg.name);
        let run = self.run_panel.sessions.iter().any(|session| {
            Some(session.id) != replaces
                && session.owns_process()
                && session
                    .config
                    .as_ref()
                    .is_some_and(|launched| launched.name == cfg.name)
        });
        let debug = self.has_running_debug_configuration(&cfg.name);
        let measured = self.measured_configuration_running(&cfg.name);
        let concurrency = self.concurrency_panel.is_running()
            && self
                .concurrency_panel
                .last_config
                .as_ref()
                .is_some_and(|config| config.name == cfg.name);
        let console = self.py_console.running_configuration(&cfg.name);
        let http = self.project.as_ref().is_some_and(|project| {
            self.http_panel
                .running()
                .iter()
                .any(|request| request.project == project.root && request.configuration == cfg.name)
        });
        (pending||run||debug||measured||concurrency||console||http).then(||format!("'{}' is already running. Enable Allow parallel run in Edit Configurations to start another instance.",cfg.name))
    }
    fn prepare_before_steps(
        &self,
        parent: &RunConfig,
        plan: Vec<Planned>,
        prompt: Option<&str>,
    ) -> Result<Vec<Step>, String> {
        self.prepare_before_steps_authorized(parent, plan, prompt, None)
    }
    fn prepare_before_steps_authorized(
        &self,
        parent: &RunConfig,
        plan: Vec<Planned>,
        prompt: Option<&str>,
        ssh_environment: Option<&[(String, String)]>,
    ) -> Result<Vec<Step>, String> {
        let project = self.project.as_ref().ok_or("Open a project first")?;
        let root = &project.root;
        let file = self
            .active
            .and_then(|index| self.tabs.get(index))
            .and_then(|tab| tab.state.path.as_deref());
        let expand = |text: &str| self.expand_macros(text, file);
        let directory = |path: &Path| {
            let path = PathBuf::from(expand(&path.to_string_lossy()));
            if path.as_os_str().is_empty() {
                root.clone()
            } else if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        };
        let (environment, errors) = parent.load_env(root);
        if let Some(error) = errors.first() {
            return Err(error.clone());
        }
        let make_command =
            |program: &Path,
             args: Vec<String>,
             cwd: PathBuf,
             inherit: bool,
             env: &std::collections::BTreeMap<String, String>| {
                let mut command = rustpy_core::interpreter_launch::interpreter_command(program);
                if !inherit {
                    command.env_clear();
                }
                command.args(args).current_dir(cwd).envs(env);
                command
            };
        let mut steps = Vec::new();
        for entry in plan {
            let (label, action) = match entry {
                Planned::Configuration(cfg) => {
                    let cfg = *cfg;
                    if cfg
                        .type_options
                        .as_ref()
                        .is_some_and(|spec| spec.kind.special())
                    {
                        return Err(format!(
                            "Before-launch configuration '{}' requires an interactive or compound session; select its executable member instead",
                            cfg.name
                        ));
                    }
                    let cfg = if let Some(spec) = &cfg.type_options {
                        spec.materialize_at(&cfg, root)?
                    } else {
                        cfg
                    };
                    let launch = self.launch_cfg(&cfg);
                    let (_, errors) = launch.load_env(root);
                    if let Some(error) = errors.first() {
                        return Err(error.clone());
                    }
                    let (program, args, cwd, env) =
                        RunSession::command_for(&launch, project.interpreter.as_ref(), root);
                    let command = make_command(
                        &program,
                        args,
                        cwd,
                        launch.workflow.inherit_environment,
                        &env,
                    );
                    (
                        format!("Run {}", cfg.name),
                        Action::Command {
                            command,
                            input: launch.input_file(root),
                        },
                    )
                }
                Planned::Task(task) => match task {
                    Task::Upload { server, path } => {
                        let config = rustpy_deploy::DeploymentConfig::load(root)
                            .map_err(|error| error.to_string())?;
                        let server = if server.trim().is_empty() {
                            config.default_server_entry()
                        } else {
                            config.server(&server)
                        }
                        .cloned()
                        .ok_or_else(|| format!("Deployment server '{server}' is not configured"))?;
                        (
                            format!("Upload to {}", server.name),
                            Action::Upload {
                                server: Box::new(server),
                                path: directory(&path),
                                root: root.clone(),
                            },
                        )
                    }
                    Task::Configuration { .. } => {
                        return Err("Unexpanded configuration prerequisite".into());
                    }
                    Task::ExternalTool { name } => {
                        let tool = self
                            .settings
                            .external_tools
                            .iter()
                            .find(|tool| tool.name == name)
                            .ok_or_else(|| format!("External tool '{name}' does not exist"))?;
                        let mut context = self.macro_context(file);
                        context.prompt = prompt.map(str::to_owned);
                        if [&tool.program, &tool.arguments, &tool.working_dir]
                            .into_iter()
                            .chain(tool.options.environment.values())
                            .any(|value| value.contains("$ClipboardContent$"))
                        {
                            context.clipboard = self.clipboard_text();
                        }
                        let planned = if let Some(environment) = ssh_environment {
                            crate::external_tool_workspace::plan_with_ssh_confirmation(
                                tool,
                                &context,
                                environment,
                            )
                        } else {
                            crate::external_tool_workspace::plan(tool, &context)
                        };
                        let launch = planned
                            .map_err(|error| format!("External tool {name}: {error}"))?
                            .config;
                        let (tool_environment, errors) = launch.load_env(root);
                        if let Some(error) = errors.first() {
                            return Err(error.clone());
                        }
                        let mut env = environment.clone();
                        env.extend(tool_environment);
                        let RunConfigKind::Program { program, args } = launch.kind else {
                            unreachable!()
                        };
                        let cwd = launch.working_dir.unwrap_or_else(|| root.clone());
                        (
                            format!("External tool {name}"),
                            Action::Command {
                                command: make_command(
                                    &program,
                                    args,
                                    cwd,
                                    launch.workflow.inherit_environment,
                                    &env,
                                ),
                                input: None,
                            },
                        )
                    }
                    task => {
                        let (label, program, args, cwd) = match task {
                            Task::FileWatcher { name } => {
                                let invocation = self.named_watcher_invocation(&name, file)?;
                                (
                                    format!("File watcher {name}"),
                                    invocation.program,
                                    invocation.args,
                                    invocation
                                        .working_dir
                                        .map(|path| directory(&path))
                                        .unwrap_or_else(|| root.clone()),
                                )
                            }
                            Task::Npm {
                                directory: path,
                                script,
                            } => {
                                if script.trim().is_empty() {
                                    return Err("Specify the before-launch npm script".into());
                                }
                                (
                                    format!("npm run {script}"),
                                    PathBuf::from(if cfg!(windows) { "npm.cmd" } else { "npm" }),
                                    vec!["run".into(), expand(&script)],
                                    directory(&path),
                                )
                            }
                            Task::Grunt {
                                directory: path,
                                task,
                            } => {
                                let mut args = vec!["--no-install".into(), "grunt".into()];
                                args.extend(RunConfig::split_args(&expand(&task)));
                                (
                                    "Grunt task".into(),
                                    PathBuf::from(if cfg!(windows) { "npx.cmd" } else { "npx" }),
                                    args,
                                    directory(&path),
                                )
                            }
                            Task::Command {
                                program,
                                arguments,
                                directory: path,
                            } => (
                                "Program".into(),
                                PathBuf::from(expand(&program.to_string_lossy())),
                                RunConfig::split_args(&expand(&arguments)),
                                directory(&path),
                            ),
                            Task::Configuration { .. }
                            | Task::Upload { .. }
                            | Task::ExternalTool { .. } => unreachable!(),
                        };
                        if program.as_os_str().is_empty() {
                            return Err(format!("{label}: specify an executable"));
                        }
                        let command = make_command(
                            &program,
                            args,
                            cwd,
                            parent.workflow.inherit_environment,
                            &environment,
                        );
                        (
                            label,
                            Action::Command {
                                command,
                                input: None,
                            },
                        )
                    }
                },
            };
            steps.push(Step { label, action });
        }
        Ok(steps)
    }
    pub(super) fn poll_run_workflows(&mut self, ctx: &egui::Context) {
        let replacements = std::mem::take(&mut self.run_workflows.replacements);
        for pending in replacements {
            let valid = !self.project_safe_mode()
                && pending.file_session == self.file_session
                && self.project.as_ref().is_some_and(|project| {
                    project.root == pending.root
                        && Arc::ptr_eq(&project.analysis, &pending.analysis)
                        && match &pending.launch {
                            ReplacementLaunch::Configuration(cfg, _) => {
                                let current = project
                                    .run_configs
                                    .configs
                                    .iter()
                                    .find(|stored| stored.name == cfg.name);
                                current == pending.configuration_identity.as_ref()
                            }
                            ReplacementLaunch::Command(_, _) => true,
                        }
                });
            if !valid {
                self.set_status("Rerun cancelled because the project or configuration changed");
                continue;
            }
            if let Some(old) = self
                .run_panel
                .sessions
                .iter_mut()
                .find(|s| s.id == pending.old)
            {
                old.poll();
                if old.owns_process() {
                    self.run_workflows.replacements.push(pending);
                    ctx.request_repaint_after(std::time::Duration::from_millis(20));
                    continue;
                }
            }
            match pending.launch {
                ReplacementLaunch::Configuration(cfg, mode) => {
                    self.launch_after_workflow(*cfg, mode, Some(pending.old))
                }
                ReplacementLaunch::Command(name, spec) => {
                    self.spawn_command_session_replacing(&name, spec, Some(pending.old))
                }
            }
        }
        let project_safe_mode = self.project_safe_mode();
        let pending = std::mem::take(&mut self.run_workflows.pending);
        for mut job in pending {
            let valid = !project_safe_mode
                && self.file_session == job.file_session
                && self.project.as_ref().is_some_and(|project| {
                    project.root == job.root
                        && Arc::ptr_eq(&project.analysis, &job.analysis)
                        && project
                            .run_configs
                            .configs
                            .iter()
                            .any(|cfg| cfg == &job.cfg)
                });
            if !valid {
                job.task.cancel();
            }
            let result = job.task.try_result();
            if let Some(result) = &result {
                if let Some(auth) = job.ssh_auth.take() {
                    self.git_panel.git_jobs.job_finished(
                        auth,
                        result.is_ok() && !job.task.is_cancelled(),
                        &result
                            .as_ref()
                            .err()
                            .map(ToString::to_string)
                            .unwrap_or_default(),
                    );
                }
            }
            match result {
                Some(Ok(())) if valid && !job.task.is_cancelled() => {
                    if let Some(problem) =
                        self.parallel_configuration_problem(&job.cfg, job.replaces)
                    {
                        self.set_status(problem);
                    } else {
                        self.launch_after_workflow(job.cfg.clone(), job.mode, job.replaces);
                    }
                }
                Some(Err(error)) => self.set_status(format!("Before launch: {error}")),
                Some(Ok(())) => self.set_status(
                    "Before-launch tasks cancelled because the project or configuration changed",
                ),
                None => {
                    self.run_workflows.pending.push(job);
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
        }
    }
    pub(super) fn record_workflow_launch(&mut self, cfg: &RunConfig, _mode: RunMode) {
        if let Some(project) = &mut self.project {
            if let Some(stored) = project
                .run_configs
                .configs
                .iter_mut()
                .find(|stored| stored.name == cfg.name)
            {
                stored.workflow.last_used = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
            }
            if let Err(error) = project.run_configs.save(&project.root) {
                self.set_status(format!("Cannot save recent configurations: {error}"));
            }
        }
    }
    pub(super) fn finish_run_workflow(
        &mut self,
        cfg: &RunConfig,
        mode: RunMode,
        previous: Option<BottomTool>,
    ) {
        self.record_workflow_launch(cfg, mode);
        if !cfg.workflow.activate_window && !cfg.workflow.focus_window {
            self.bottom = previous;
        }
        if cfg.workflow.activate_window || cfg.workflow.focus_window {
            if let Some(tool) = self.bottom {
                self.reveal_tool_window_content(layout::ToolRef::Bottom(tool));
            }
        }
        if cfg.workflow.focus_window {
            if self.bottom == Some(BottomTool::Run) {
                self.run_panel.request_console_focus();
            }
            if let Some(tool) = self.bottom {
                self.queue_tool_controls_focus(tool);
            }
        }
    }
    pub(super) fn before_launch_stop_items(&self) -> Vec<crate::run_anything::StopItem> {
        self.run_workflows
            .pending
            .iter()
            .filter(|job| !job.task.is_cancelled())
            .map(|job| {
                crate::run_anything::StopItem::BeforeLaunch(job.task.id(), job.cfg.name.clone())
            })
            .collect()
    }
    pub(super) fn stop_before_launch(&mut self, id: u64) {
        if let Some(job) = self
            .run_workflows
            .pending
            .iter()
            .find(|job| job.task.id() == id)
        {
            job.task.cancel();
        }
    }
    pub(super) fn run_workflow_stamps(&self) -> Vec<String> {
        self.run_workflows
            .pending
            .iter()
            .map(|job| {
                format!(
                    "{}:{}:{}",
                    job.task.id(),
                    job.cfg.name,
                    job.task.is_cancelled()
                )
            })
            .chain(self.run_workflows.replacements.iter().map(|pending| {
                format!(
                    "replacement:{}:{}:{:?}",
                    pending.old, pending.file_session, pending.launch
                )
            }))
            .collect()
    }
    pub(super) fn run_workflows_pending(&self) -> bool {
        !self.run_workflows.pending.is_empty() || !self.run_workflows.replacements.is_empty()
    }
    pub(super) fn cancel_run_workflows(&mut self) {
        for pending in &self.run_workflows.pending {
            pending.task.cancel();
        }
        self.run_workflows.approved = None;
        self.run_workflows.replacements.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn external_prerequisite_captures_prompt_as_one_argument_and_tool_environment() {
        let directory = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        settings.trust_project(directory.path());
        let mut app = IdeApp::new(settings, Some(directory.path().into()));
        let mut tool = rustpy_settings::ExternalTool {
            name: "Prepare".into(),
            program: "provider".into(),
            arguments: "--answer $Prompt$".into(),
            ..Default::default()
        };
        tool.options.inherit_environment = false;
        tool.options
            .environment
            .insert("TOOL_ONLY".into(), "yes".into());
        app.settings.external_tools.push(tool);
        let mut cfg = RunConfig::default();
        cfg.workflow.before_launch.push(run_workflow::BeforeLaunch {
            enabled: true,
            task: Task::ExternalTool {
                name: "Prepare".into(),
            },
        });
        assert!(app.before_launch_requires_prompt(&cfg));
        let plan = run_workflow::before_plan(&cfg, &[]).unwrap();
        assert!(app
            .prepare_before_steps(&cfg, plan.clone(), None)
            .err()
            .unwrap()
            .contains("prompt"));
        let answer = "two words 'literal'";
        let mut steps = app.prepare_before_steps(&cfg, plan, Some(answer)).unwrap();
        let Action::Command { command, .. } = steps.remove(0).action else {
            panic!("not a command")
        };
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["--answer", answer]);
        assert_eq!(
            command
                .get_envs()
                .find(|(key, _)| *key == "TOOL_ONLY")
                .unwrap()
                .1,
            Some(std::ffi::OsStr::new("yes"))
        );
        assert!(app.settings.external_tools[0]
            .arguments
            .contains("$Prompt$"));
    }
    #[test]
    fn prerequisite_preflight_keeps_quoted_arguments_and_environment_without_spawning() {
        let directory = tempfile::tempdir().unwrap();
        let app = IdeApp::new(Settings::default(), Some(directory.path().into()));
        let mut cfg = RunConfig::default();
        cfg.workflow.inherit_environment = false;
        cfg.env.insert("FLAG".into(), "yes".into());
        let plan = vec![Planned::Task(Task::Command {
            program: "tool".into(),
            arguments: "--name 'two words'".into(),
            directory: PathBuf::new(),
        })];
        let mut steps = app.prepare_before_steps(&cfg, plan, None).unwrap();
        let Action::Command { command, .. } = steps.remove(0).action else {
            panic!("expected command")
        };
        assert_eq!(command.get_program(), "tool");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["--name", "two words"]
        );
        assert_eq!(
            command.get_current_dir(),
            app.project.as_ref().map(|project| project.root.as_path())
        );
        assert!(command
            .get_envs()
            .any(|(key, value)| key == "FLAG" && value.is_some_and(|value| value == "yes")));
        assert!(app.run_workflows.pending.is_empty());
    }
    #[test]
    fn missing_prerequisite_and_unapproved_settings_do_not_start_a_task() {
        let directory = tempfile::tempdir().unwrap();
        let mut settings = Settings::default();
        settings.trust_project(directory.path());
        let mut app = IdeApp::new(settings, Some(directory.path().into()));
        let mut cfg = RunConfig {
            name: "checked".into(),
            ..Default::default()
        };
        cfg.workflow.before_launch.push(run_workflow::BeforeLaunch {
            enabled: true,
            task: Task::Configuration {
                name: "missing".into(),
            },
        });
        assert!(app.prepare_run_workflow(&cfg, RunMode::Run, None, None));
        assert!(app.run_workflows.pending.is_empty());
        cfg.workflow.show_settings = true;
        assert!(app.prepare_run_workflow(&cfg, RunMode::Run, None, None));
        assert!(matches!(app.dialog, Dialog::RunConfigurations));
        assert!(app.run_workflows.pending.is_empty());
    }
}
