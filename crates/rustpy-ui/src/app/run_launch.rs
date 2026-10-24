//! Launching run configurations (roadmap §3.1–3.4): what a launch puts on
//! `PYTHONPATH`, pytest / unittest configurations in the Test Runner with
//! their options, Run / Debug / Coverage / Profile of gutter targets and
//! context configurations, Emulate terminal, Run with Python Console (see
//! `console_exec.rs`), `$Prompt$`, Run Anything commands, Rerun, the Stop
//! picker and the Run/Debug Configurations dialog.

use super::*;
use crate::run_anything::{
    quote_args, CommandPlan, CommandSpec, LauncherEvent, LauncherInput, StopItem,
};
use crate::run_config_dialog::{DialogInput, RunCfgEvent};
use crate::run_context::{self, RunMode};
use crate::run_panel::RerunPlan;
use rustpy_test::{Framework, TestRunOptions};

const SAFE_MODE: &str =
    "Safe mode: this project is not trusted yet (File → Trust Project to allow running code)";

fn metadata_test_framework(cfg: &RunConfig) -> Option<Framework> {
    use rustpy_project::configuration_types::ConfigurationType as Kind;
    cfg.type_options.as_ref().and_then(|spec| match spec.kind {
        Kind::Nose => Some(Framework::Nose),
        Kind::Nose2 => Some(Framework::Nose2),
        Kind::Trial => Some(Framework::Trial),
        Kind::Tox => Some(Framework::Tox),
        Kind::Behave => Some(Framework::Behave),
        Kind::Lettuce => Some(Framework::Lettuce),
        _ => None,
    })
}
fn is_test_kind(cfg: &RunConfig) -> bool {
    metadata_test_framework(cfg).is_some()
        || matches!(
            cfg.kind,
            RunConfigKind::Pytest { .. } | RunConfigKind::Unittest { .. }
        )
        || matches!(&cfg.kind,RunConfigKind::PythonModule{module}if module=="doctest")
}

/// Run overrides win; otherwise the target file's module supplies its SDK
/// and default working directory. Resolve targets before changing that directory.
fn materialize_project_run_config(
    cfg: &RunConfig,
    root: &Path,
    model: &rustpy_project::ProjectModel,
) -> RunConfig {
    let mut launch = cfg.clone();
    let script = cfg.script_path(root);
    let target = match &cfg.kind {
        RunConfigKind::Pytest { target, .. } | RunConfigKind::Unittest { target } => {
            Some(target.split("::").next().unwrap_or(target))
        }
        _ => None,
    };
    let anchor = script
        .clone()
        .or_else(|| {
            target.filter(|target| !target.is_empty()).map(|target| {
                let path = PathBuf::from(target);
                if path.is_absolute() {
                    path
                } else {
                    cfg.working_dir_in(root).join(path)
                }
            })
        })
        .unwrap_or_else(|| cfg.working_dir_in(root));
    let anchor = rustpy_core::vfs::normalize_path(&anchor);
    if launch.interpreter.is_none() {
        launch.interpreter = model.sdk_for(&anchor).map(Path::to_path_buf);
    }
    if let Some(module) = model.module_for(&anchor) {
        if cfg.working_dir.is_none() || (cfg.temporary && cfg.working_dir_in(root) == root) {
            launch.working_dir = Some(module.content_root.clone());
            match &mut launch.kind {
                RunConfigKind::Python { script: path }
                | RunConfigKind::ShellScript { script: path } => {
                    if let Some(script) = script {
                        *path = script;
                    }
                }
                RunConfigKind::Pytest { target, .. } | RunConfigKind::Unittest { target }
                    if !target.is_empty() =>
                {
                    let suffix = target
                        .find("::")
                        .map(|index| target[index..].to_owned())
                        .unwrap_or_default();
                    *target = format!("{}{suffix}", anchor.display());
                }
                _ => {}
            }
        }
    }
    launch
}

impl IdeApp {
    pub(super) fn project_root(&self) -> PathBuf {
        self.project
            .as_ref()
            .map(|p| p.root.clone())
            .unwrap_or_default()
    }

    /// The configuration as launched: macros expanded in parameters and the
    /// working directory, and `PYTHONPATH` set to the content and Sources
    /// Roots (plus the inherited value) unless the configuration or one of
    /// its `.env` files sets it. The result is never persisted.
    pub(super) fn launch_cfg(&self, cfg: &RunConfig) -> RunConfig {
        let mut launch = cfg.clone();
        let root = self.project_root();
        if let Some(project) = &self.project {
            launch = materialize_project_run_config(&launch, &root, &project.tree.model());
        }
        let file = self
            .active
            .and_then(|i| self.tabs.get(i))
            .and_then(|t| t.state.path.clone())
            .filter(|p| !p.starts_with("<typeshed>"));
        let mut values = self.macro_values(file.as_deref());
        values.push((
            "ModuleSdkPath",
            launch
                .interpreter
                .as_ref()
                .or_else(|| {
                    self.project
                        .as_ref()
                        .and_then(|p| p.interpreter.as_ref().map(|i| &i.path))
                })
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
        ));
        let expand = |text: &str| crate::plugin_host::expand_macros_verbatim(text, &values);
        if let Some(spec) = &mut launch.type_options {
            spec.target = expand(&spec.target);
            spec.environment = expand(&spec.environment);
        }
        launch.parameters = expand(&launch.parameters);
        launch.interpreter_options = expand(&launch.interpreter_options);
        for value in launch.env.values_mut() {
            *value = expand(value);
        }
        if let Some(wd) = launch.working_dir.clone() {
            let text = wd.to_string_lossy();
            if text.contains('$') {
                launch.working_dir = Some(PathBuf::from(expand(&text)));
            }
        }
        match &mut launch.kind {
            RunConfigKind::Python { script } | RunConfigKind::ShellScript { script } => {
                *script = PathBuf::from(expand(&script.to_string_lossy()))
            }
            RunConfigKind::PythonModule { module } => *module = expand(module),
            RunConfigKind::Pytest { target, keywords } => {
                *target = expand(target);
                *keywords = expand(keywords);
            }
            RunConfigKind::Unittest { target } => *target = expand(target),
            RunConfigKind::Program { program, args } => {
                *program = PathBuf::from(expand(&program.to_string_lossy()));
                for arg in args {
                    *arg = expand(arg);
                }
            }
        }
        for file in &mut launch.env_files {
            *file = PathBuf::from(expand(&file.to_string_lossy()));
        }
        if let Some(input) = &mut launch.redirect_input {
            *input = PathBuf::from(expand(&input.to_string_lossy()));
        }
        if !launch.env.contains_key("PYTHONPATH")
            && !launch.load_env(&root).0.contains_key("PYTHONPATH")
        {
            if let Some(pp) = self.pythonpath_for(cfg) {
                launch.env.insert("PYTHONPATH".into(), pp);
            }
        }
        launch
    }

    /// Test Runner options for runs without a configuration (Run All Tests,
    /// tests of the current file, behave scenarios): Sources Roots on `PYTHONPATH`.
    pub(super) fn default_test_options(&self) -> TestRunOptions {
        let mut env = self
            .pythonpath_for(&RunConfig::default())
            .map(|pp| vec![("PYTHONPATH".to_string(), pp)])
            .unwrap_or_default();
        if self.settings.debugger.workspace.drop_failed_tests {
            env.push(("RUSTPY_DEBUG_FAILED_TESTS".into(), "1".into()));
        }
        TestRunOptions {
            env,
            collect_runtime_types: self.settings.collect_runtime_types,
            ..Default::default()
        }
    }

    /// Saves `cfg` as the selected configuration and launches it in `mode`
    /// (asking for `$Prompt$` values first). A temporary configuration never
    /// replaces another one (see `RunConfigStore::resolve_temporary`).
    pub(super) fn launch_config(&mut self, cfg: RunConfig, mode: RunMode) {
        self.launch_resolved(cfg, mode, false, None);
    }

    /// Launches a configuration made from the editor or Project view context:
    /// an existing configuration with the same target runs instead (with its
    /// parameters and environment).
    pub(super) fn launch_context(&mut self, cfg: RunConfig, mode: RunMode) {
        if self.maybe_choose_test_or_main(&cfg, mode) {
            return;
        }
        self.launch_resolved(cfg, mode, true, None);
    }

    /// Project view Run / Debug: a script, a test file or the tests of a directory.
    pub(super) fn launch_path(&mut self, path: &Path, mode: RunMode) {
        if self.remote_development_document_run(path, mode) {
            return;
        }
        let cfg = run_context::config_for_path(&self.project_root(), path);
        self.launch_context(cfg, mode);
    }

    /// `replaces`: the Run session a Rerun replaces. It is stopped and its
    /// tab closed only when the launch starts ([`Self::launch_now`]), so it
    /// keeps running when the launch cannot start or its `$Prompt$` is cancelled.
    fn launch_resolved(
        &mut self,
        cfg: RunConfig,
        mode: RunMode,
        reuse: bool,
        replaces: Option<u64>,
    ) {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode {
            self.set_status(SAFE_MODE);
            return;
        }
        let mut persist_error = None;
        let cfg = match &mut self.project {
            Some(p) => {
                let cfg = if reuse
                    && cfg.temporary
                    && !p
                        .run_configs
                        .configs
                        .iter()
                        .any(|stored| stored.same_target(&cfg, &p.root))
                {
                    rustpy_project::run_workflow::Templates::load(&p.root).apply(&cfg)
                } else {
                    cfg
                };
                let mut cfg = p.run_configs.resolve_temporary(cfg, reuse, &p.root);
                // Scratch files run from the project root (with the project
                // interpreter) so they can import its modules.
                if cfg.temporary
                    && matches!(&cfg.kind, RunConfigKind::Python { script } if crate::scratches::is_scratch(script))
                {
                    cfg.working_dir = Some(p.root.clone());
                }
                p.run_configs.upsert(cfg.clone());
                let templates = rustpy_project::run_workflow::Templates::load(&p.root);
                templates.trim_temporary(
                    &mut p.run_configs.configs,
                    p.run_configs.selected.as_deref(),
                );
                if let Err(error) = p.run_configs.save(&p.root) {
                    persist_error = Some(error.to_string());
                }
                cfg
            }
            None => cfg,
        };
        if let Some(error) = persist_error {
            self.set_status(format!("Cannot save run configurations: {error}"));
            return;
        }
        let mut cfg = cfg;
        self.framework_run_policy(&mut cfg);
        let prompt = self.before_launch_requires_prompt(&cfg)
            || cfg.parameters.contains("$Prompt$")
            || cfg
                .working_dir
                .as_ref()
                .is_some_and(|w| w.to_string_lossy().contains("$Prompt$"));
        if prompt {
            self.run_panel.launcher.open_prompt(cfg, mode, replaces);
            return;
        }
        self.launch_now(cfg, mode, replaces);
    }

    /// Why `cfg` cannot be launched in `mode`: safe mode, or a
    /// missing "Redirect input from" file, which an emulated terminal does not use.
    fn launch_problem(&self, cfg: &RunConfig, _mode: RunMode) -> Option<String> {
        let project_safe_mode = self.project_safe_mode();
        let codec = cfg
            .env
            .get("PYTHONIOENCODING")
            .map(|value| value.split(':').next().unwrap_or(value))
            .unwrap_or(&cfg.console_encoding);
        if rustpy_run::output_encoding::Encoding::parse(codec).is_none() {
            return Some(format!("Unsupported console encoding: {codec}"));
        }
        if project_safe_mode {
            return Some(SAFE_MODE.into());
        }
        if is_test_kind(cfg) {
            return None;
        }
        cfg.input_problem(&self.project_root())
    }

    fn launch_now(&mut self, cfg: RunConfig, mode: RunMode, replaces: Option<u64>) {
        self.launch_now_with_prompt(cfg, mode, replaces, None);
    }
    fn launch_now_with_prompt(
        &mut self,
        cfg: RunConfig,
        mode: RunMode,
        replaces: Option<u64>,
        prompt: Option<&str>,
    ) {
        if let Some(problem) = self.launch_problem(&cfg, mode) {
            self.set_status(problem);
            return;
        }
        if self.prepare_run_workflow(&cfg, mode, replaces, prompt) {
            return;
        }
        self.launch_after_workflow(cfg, mode, replaces);
    }

    pub(super) fn launch_after_workflow(
        &mut self,
        cfg: RunConfig,
        mode: RunMode,
        replaces: Option<u64>,
    ) {
        if let Some(problem) = self.launch_problem(&cfg, mode) {
            self.set_status(problem);
            return;
        }
        if self.defer_run_replacement(
            replaces,
            super::run_configuration_workflows::ReplacementLaunch::Configuration(
                Box::new(cfg.clone()),
                mode,
            ),
        ) {
            return;
        }
        let previous_bottom = self.bottom;
        self.checkpoint_execution_history(if is_test_kind(&cfg) {
            "Before test run"
        } else {
            "Before run"
        });
        let original = cfg.clone();
        if self.launch_configuration_type(&cfg, mode) {
            self.finish_run_workflow(&original, mode, previous_bottom);
            return;
        }
        let cfg = if let Some(spec) = &cfg.type_options {
            match spec.materialize_at(&cfg, &self.project_root()) {
                Ok(config) => config,
                Err(error) => {
                    self.set_status(error);
                    return;
                }
            }
        } else {
            cfg
        };
        if let Some(id) = replaces {
            if let Some(old) = self
                .run_panel
                .sessions
                .iter_mut()
                .find(|s| s.id == id && s.owns_process())
            {
                old.stop();
            }
        }
        let new_session = self.next_session_id;
        match mode {
            RunMode::Run => self.start_run(cfg, replaces),
            RunMode::Debug => self.start_debug(cfg),
            RunMode::Coverage | RunMode::Profile => self.start_measured(cfg, mode),
            RunMode::Concurrency => self.start_concurrency_config(cfg),
        }
        // Retain the editable source config for restart/rerun. Lowering adds
        // type arguments, so persisting that result would append them twice.
        if let Some(session) = self
            .run_panel
            .sessions
            .iter_mut()
            .find(|session| session.id == new_session && session.name == original.name)
        {
            session.config = Some(original.clone());
            self.run_panel.record_launch(new_session, original.clone());
        }
        if let Some(session) = self
            .debug
            .as_mut()
            .filter(|session| session.id == new_session && session.config.name == original.name)
        {
            session.config = original.clone();
        }
        if self
            .concurrency_panel
            .run
            .as_ref()
            .is_some_and(|run| run.id == new_session)
        {
            self.concurrency_panel.last_config = Some(original.clone());
        }
        self.finish_run_workflow(&original, mode, previous_bottom);
    }

    pub(super) fn report_env_errors(&mut self, launch: &RunConfig) {
        let (_, errors) = launch.load_env(&self.project_root());
        if let Some(e) = errors.into_iter().next() {
            self.set_status(e);
        }
    }

    fn start_run(&mut self, cfg: RunConfig, replaces: Option<u64>) {
        if self.project_safe_mode() {
            self.set_status("Trust the project before launching or probing execution backends");
            return;
        }
        if is_test_kind(&cfg) {
            self.run_test_config(&cfg);
            return;
        }
        if !self.save_all() {
            return;
        }
        let launch = self.launch_cfg(&cfg);
        self.report_env_errors(&launch);
        if launch.run_with_console && matches!(launch.kind, RunConfigKind::Python { .. }) {
            // `runfile` in the Python Console (console_exec.rs): expanded parameters,
            // the resolved working directory and the configuration's own
            // environment (.env files included; the console has the roots already).
            let root = self.project_root();
            let mut console = launch.clone();
            console.working_dir = Some(launch.working_dir_in(&root));
            console.env = launch.load_env(&root).0;
            self.run_config_in_console(&console);
            return;
        }
        let Some(p) = &self.project else { return };
        let id = self.next_session_id;
        self.next_session_id += 1;
        let session = if self.settings.collect_runtime_types {
            rustpy_run::typecollect::spawn_collecting(id, &launch, p.interpreter.as_ref(), &p.root)
        } else {
            RunSession::spawn(id, &launch, p.interpreter.as_ref(), &p.root)
        };
        self.run_panel.settings = self.settings.run_console.clone();
        // Preserve the old tab and its output if spawning the replacement failed.
        if !matches!(session.status, rustpy_run::SessionStatus::Failed(_)) {
            if let Some(old) = replaces.filter(|id| self.run_panel.can_replace(*id)) {
                self.run_panel.sessions.retain(|s| s.id != old);
            }
        }
        self.run_panel.add_session(session);
        self.run_panel.record_launch(id, cfg);
        self.bottom = Some(BottomTool::Run);
    }

    /// Runs a pytest / unittest configuration in the Test Runner with its
    /// parameters, environment (`.env` files included), interpreter,
    /// interpreter options and working directory. A temporary (context)
    /// pytest configuration runs with the detected framework.
    fn run_test_config(&mut self, cfg: &RunConfig) {
        if self.project_safe_mode() {
            self.set_status("Trust the project before launching or probing execution backends");
            return;
        }
        if !self.save_all() {
            return;
        }
        let Some(p) = &self.project else { return };
        let root = p.root.clone();
        let launch = self.launch_cfg(cfg);
        let Some(interp) = launch
            .interpreter
            .clone()
            .or_else(|| p.interpreter.as_ref().map(|i| i.path.clone()))
        else {
            self.set_status("No Python interpreter configured");
            return;
        };
        let target_list = |t: &str| {
            if t.trim().is_empty() {
                Vec::new()
            } else {
                vec![t.trim().to_string()]
            }
        };
        let (mut targets, keywords, framework) =
            if let Some(framework) = metadata_test_framework(&launch) {
                let spec = launch.type_options.as_ref().unwrap();
                let targets = if matches!(framework, Framework::Trial | Framework::Behave) {
                    target_list(&spec.target_argument())
                } else {
                    Vec::new()
                };
                let keywords = if framework == Framework::Trial {
                    spec.keywords.clone()
                } else {
                    String::new()
                };
                (targets, keywords, framework)
            } else {
                match &launch.kind {
                    RunConfigKind::Pytest { target, keywords } => {
                        let targets = target_list(target);
                        let framework = if cfg.temporary {
                            self.configured_test_framework().unwrap_or_else(|| {
                                rustpy_test::detect_framework_for(&root, &targets, Some(&interp))
                            })
                        } else {
                            Framework::Pytest
                        };
                        (targets, keywords.clone(), framework)
                    }
                    RunConfigKind::Unittest { target } => {
                        (target_list(target), String::new(), Framework::Unittest)
                    }
                    RunConfigKind::PythonModule { module } if module == "doctest" => {
                        (Vec::new(), String::new(), Framework::Doctest)
                    }
                    _ => return,
                }
            };
        let mut args = Vec::new();
        if !keywords.trim().is_empty() {
            args.push("-k".to_string());
            args.push(keywords.trim().to_string());
        }
        args.extend(RunConfig::split_args(&launch.parameters));
        if matches!(framework, Framework::Trial | Framework::Behave) {
            if let Some(spec) = &launch.type_options {
                if let Some(index) = args
                    .iter()
                    .position(|argument| argument == &spec.target_argument())
                {
                    args.remove(index);
                }
            }
        }
        if framework == Framework::Doctest {
            let mut options = Vec::new();
            let mut takes_value = false;
            for argument in args {
                if takes_value {
                    options.push(argument);
                    takes_value = false;
                } else if matches!(argument.as_str(), "-o" | "--option") {
                    options.push(argument);
                    takes_value = true;
                } else if argument.starts_with('-') {
                    options.push(argument);
                } else {
                    targets.push(argument);
                }
            }
            args = options;
        }
        let (env, errors) = launch.load_env(&root);
        if let Some(e) = errors.into_iter().next() {
            self.set_status(e);
        }
        let mut env = env;
        if self.settings.debugger.workspace.drop_failed_tests {
            env.insert("RUSTPY_DEBUG_FAILED_TESTS".into(), "1".into());
        }
        let mut options = TestRunOptions {
            inherit_environment: cfg.workflow.inherit_environment,
            env: env.into_iter().collect(),
            cwd: launch.working_dir.clone(),
            interpreter_options: RunConfig::split_args(&launch.interpreter_options),
            collect_runtime_types: self.settings.collect_runtime_types,
        };
        self.configure_test_tools(framework, &targets, &mut args, &mut options);
        self.apply_test_preferences(framework, &mut args, &mut options);
        self.bottom = Some(BottomTool::Tests);
        self.begin_measurement(super::run_configuration_sessions::Kind::Tests, &cfg.name);
        if let Err(e) = self
            .test_panel
            .start_with(framework, &interp, &root, targets, args, options)
        {
            self.set_status(e);
        }
    }

    /// The debugger, coverage and profiler run test configurations as plain
    /// processes (`python -m pytest …` / `python -m unittest …`); a context
    /// (temporary) pytest configuration follows the detected framework.
    pub(super) fn process_test_cfg(&mut self, cfg: RunConfig) -> Option<RunConfig> {
        if let Some(framework) = metadata_test_framework(&cfg).filter(|framework| {
            matches!(
                framework,
                Framework::Nose | Framework::Nose2 | Framework::Lettuce | Framework::Tox
            )
        }) {
            let directory = Settings::system_dir().join("test-reporters");
            let script = match rustpy_test::provision_reporter(&directory) {
                Ok(script) => script,
                Err(error) => {
                    self.set_status(error.to_string());
                    return None;
                }
            };
            let mut adapted = cfg.clone();
            adapted.kind = RunConfigKind::Python { script };
            let mut arguments = vec![
                "--framework".into(),
                framework.label().into(),
                "--root".into(),
                self.project_root().to_string_lossy().into_owned(),
                "--".into(),
            ];
            arguments.extend(RunConfig::split_args(&cfg.parameters));
            adapted.parameters = quote_args(&arguments);
            return Some(adapted);
        }
        let RunConfigKind::Pytest { target, .. } = &cfg.kind else {
            return Some(cfg);
        };
        if !cfg.temporary {
            return Some(self.process_test_preferences(cfg, Framework::Pytest));
        }
        let root = self.project_root();
        let routed = self.launch_cfg(&cfg);
        let interp = routed.interpreter.clone().or_else(|| {
            self.project
                .as_ref()
                .and_then(|p| p.interpreter.as_ref().map(|i| i.path.clone()))
        });
        let targets = if target.trim().is_empty() {
            Vec::new()
        } else {
            vec![target.trim().to_string()]
        };
        match self.preferred_test_framework(&root).unwrap_or_else(|| {
            rustpy_test::detect_framework_for(&root, &targets, interp.as_deref())
        }) {
            Framework::Pytest => Some(self.process_test_preferences(cfg, Framework::Pytest)),
            Framework::Unittest => {
                let mut c = cfg.clone();
                c.kind = RunConfigKind::Unittest {
                    target: target.clone(),
                };
                Some(c)
            }
            other => {
                use rustpy_project::configuration_types::{ConfigurationSpec, ConfigurationType};
                let kind = match other {
                    Framework::Nose => ConfigurationType::Nose,
                    Framework::Nose2 => ConfigurationType::Nose2,
                    Framework::Doctest => ConfigurationType::Doctest,
                    Framework::Trial => ConfigurationType::Trial,
                    Framework::Behave => ConfigurationType::Behave,
                    Framework::Lettuce => ConfigurationType::Lettuce,
                    Framework::Tox => ConfigurationType::Tox,
                    _ => unreachable!(),
                };
                let mut spec = ConfigurationSpec::new(kind);
                spec.target = target.clone();
                match spec.materialize_at(&cfg, &root) {
                    Ok(mut adapted) => {
                        adapted.type_options = Some(spec);
                        self.process_test_cfg(adapted)
                    }
                    Err(error) => {
                        self.set_status(error);
                        None
                    }
                }
            }
        }
    }

    /// Starts a debug session for a configuration (without saving it).
    pub(super) fn start_debug(&mut self, cfg: RunConfig) {
        if self.project_safe_mode() {
            self.set_status("Trust the project before launching or probing execution backends");
            return;
        }
        if !self.save_all() {
            return;
        }
        let requested = cfg.clone();
        let Some(cfg) = self.process_test_cfg(cfg) else {
            return;
        };
        let mut launch = self.launch_cfg(&cfg);
        self.prepare_scientific_debug(&mut launch);
        self.report_env_errors(&launch);
        let Some(p) = &self.project else { return };
        let root = p.root.clone();
        let interp = p.interpreter.clone();
        let setup = self.debug_launch_setup();
        self.retain_debug_session();
        let id = self.next_session_id;
        self.next_session_id += 1;
        match DebugSession::launch(id, &launch, interp.as_ref(), &root, setup) {
            Ok(mut s) => {
                let ws = self.effective_workspace();
                for w in ws.watches {
                    s.watches.push(rustpy_debug::session::Watch {
                        expression: w,
                        result: None,
                        error: None,
                    });
                }
                self.debug = Some(s);
                self.debug_panel = DebugPanel::default();
                self.bottom = Some(BottomTool::Debug);
                self.run_panel.launcher.debug_launched = Some(requested);
            }
            Err(e) => self.set_status(format!("Cannot start debugger: {e}")),
        }
    }

    /// Run with Coverage / Profile.
    fn start_measured(&mut self, cfg: RunConfig, mode: RunMode) {
        if self.project_safe_mode() {
            self.set_status("Trust the project before launching or probing execution backends");
            return;
        }
        if !self.save_all() {
            return;
        }
        let Some(cfg) = self.process_test_cfg(cfg) else {
            return;
        };
        let launch = self.launch_cfg(&cfg);
        self.report_env_errors(&launch);
        let Some(p) = &self.project else { return };
        let root = p.root.clone();
        let interp = p.interpreter.clone();
        let id = self.next_session_id;
        self.next_session_id += 1;
        if mode == RunMode::Coverage {
            self.configure_coverage();
            match rustpy_run::coverage::run_with_coverage_opts(
                id,
                &launch,
                interp.as_ref(),
                &root,
                &self.coverage_panel.options(),
            ) {
                Ok(run) => {
                    self.begin_measurement(
                        super::run_configuration_sessions::Kind::Coverage,
                        &cfg.name,
                    );
                    self.configure_coverage();
                    self.coverage_panel.start(run);
                    if self.settings.coverage.activate_view {
                        self.bottom = Some(BottomTool::Coverage);
                    }
                }
                Err(e) => self.set_status(format!("Cannot run with coverage: {e}")),
            }
        } else {
            let options = self.profiler_options();
            let mut launch = launch;
            if self.settings.profiler.disable_server_reload {
                let django = matches!(&launch.kind,RunConfigKind::Python{script}if script.file_name().is_some_and(|name|name=="manage.py"))
                    && launch
                        .parameters
                        .split_whitespace()
                        .any(|arg| arg == "runserver");
                let flask = matches!(&launch.kind,RunConfigKind::PythonModule{module}if module=="flask")
                    && launch.parameters.split_whitespace().any(|arg| arg == "run");
                if django && !launch.parameters.contains("--noreload") {
                    launch.parameters.push_str(" --noreload");
                }
                if flask && !launch.parameters.contains("--no-reload") {
                    launch.parameters.push_str(" --no-reload");
                }
            }
            match rustpy_run::profile::run_with_profiler_opts(
                id,
                &launch,
                interp.as_ref(),
                &root,
                &options,
            ) {
                Ok(run) => {
                    self.begin_measurement(
                        super::run_configuration_sessions::Kind::Profile,
                        &cfg.name,
                    );
                    self.profiler_options();
                    self.profiler_panel.start(run);
                    if self.settings.profiler.activate_view {
                        self.bottom = Some(BottomTool::Profiler);
                    }
                }
                Err(e) => self.set_status(format!("Cannot profile: {e}")),
            }
        }
    }

    /// ▶ lines of a Python editor (tests, main guard, doctests, `#%%` cells),
    /// cached per tab and document version.
    pub(super) fn python_run_lines(&mut self, i: usize) -> Vec<usize> {
        let tab = &self.tabs[i];
        let (id, version, path) = (
            tab.view.id,
            tab.state.doc.version(),
            tab.state.path.clone().unwrap_or_default(),
        );
        let cache = &mut self.run_panel.launcher.gutter_cache;
        if let Some((v, p, lines)) = cache.get(&id) {
            if *v == version && *p == path {
                return lines.clone();
            }
        }
        let root = self
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .unwrap_or_default();
        let large = usize::from(tab.state.doc.len()) >= super::analysis_jobs::BACKGROUND_SYNC_BYTES;
        let text = tab.state.doc.text();
        let lines = if large {
            // A large file's targets are found off the UI thread (walking its
            // whole tree per keystroke cost a frame); the markers of the
            // previous version stay until they are in.
            // What a new run reparses from (needed when none of ours runs).
            let base = self
                .bg
                .run_lines
                .as_ref()
                .is_none_or(|(owner, ..)| *owner != id)
                .then(|| self.tabs[i].state.reparse_base());
            let job = &mut self.bg.run_lines;
            // Another editor's run is of no use any more.
            if job.as_ref().is_some_and(|(owner, ..)| *owner != id) {
                if let Some((.., handle)) = job.take() {
                    handle.cancel();
                }
            }
            let finished = job
                .as_mut()
                .filter(|(owner, ..)| *owner == id)
                .and_then(|(.., handle)| handle.try_result());
            match finished {
                Some(result) => {
                    let (_, done, done_path, _) = job.take().unwrap();
                    match result {
                        Ok(lines) if done == version && done_path == path => lines,
                        _ => return self.stale_run_lines(id),
                    }
                }
                None => {
                    if let (None, Some(base)) = (&job, base) {
                        // Reparsed there too, from the editor's latest tree.
                        let handle = rustpy_core::TaskManager::global()
                            .task("Run markers")
                            .hidden()
                            .spawn({
                                let path = path.clone();
                                move |_| {
                                    let (parse, text) = crate::editor::state::reparsed(base, text);
                                    Ok(python_run_lines_in(&root, &path, &text, &parse))
                                }
                            });
                        *job = Some((id, version, path, handle));
                    }
                    return self.stale_run_lines(id);
                }
            }
        } else {
            // The editor's syntax tree for this version: typing does not parse the file again.
            let parse = self.tabs[i].state.syntax().clone();
            python_run_lines_in(&root, &path, &text, &parse)
        };
        let open: Vec<egui::Id> = self.tabs.iter().map(|t| t.view.id).collect();
        let cache = &mut self.run_panel.launcher.gutter_cache;
        cache.retain(|k, _| open.contains(k));
        cache.insert(id, (version, path, lines.clone()));
        lines
    }

    /// The run markers last computed for editor `id`, of any version.
    fn stale_run_lines(&self, id: egui::Id) -> Vec<usize> {
        self.run_panel
            .launcher
            .gutter_cache
            .get(&id)
            .map(|(_, _, lines)| lines.clone())
            .unwrap_or_default()
    }

    /// The ▶ of `line` in tab `i` was used: run it in `mode`.
    pub(super) fn gutter_run(&mut self, i: usize, line: usize, mode: RunMode) {
        let language = self.tabs[i].state.language;
        if language == Language::Shell {
            self.data_run_file(i);
            return;
        }
        if language == Language::Requirements {
            self.data_requirement_gutter(i, line);
            return;
        }
        if language == Language::Http {
            self.run_http_at(i, line);
            return;
        }
        if language == Language::Python
            && crate::editor::console_code::is_cell_marker(&self.tabs[i].state.doc.line_text(line))
        {
            // A `#%%` cell runs in the Python Console (console_exec.rs).
            if mode == RunMode::Debug {
                self.scientific_debug_cell(i, line);
            } else {
                self.run_cell(i, line, false);
            }
            return;
        }
        let Some(path) = self.tabs[i].state.path.clone() else {
            return;
        };
        let root = self.project_root();
        if language == Language::Gherkin {
            if mode != RunMode::Run {
                self.set_status(format!(
                    "{} is not available for behave scenarios yet",
                    mode.label()
                ));
                return;
            }
            // behave: whole feature from its Feature line, else feature:LINE for one scenario.
            let rel = path
                .strip_prefix(&root)
                .map(|p| p.to_path_buf())
                .unwrap_or(path.clone())
                .to_string_lossy()
                .into_owned();
            let is_feature = rustpy_gherkin::parse(&self.tabs[i].state.doc.text()).line == line + 1;
            let target = if is_feature {
                rel
            } else {
                format!("{rel}:{}", line + 1)
            };
            self.run_tests(vec![target], "");
            return;
        }
        if self.remote_development_document_run(&path, mode) {
            return;
        }
        if self.deployment_document_run(&path, mode) {
            return;
        }
        let text = self.tabs[i].state.doc.text();
        let Some(target) = run_context::target_at(&root, &path, &text, line) else {
            self.set_status("Run target changed; reopen the gutter menu");
            return;
        };
        let cfg = run_context::config_for_target(&root, &path, &target);
        self.launch_context(cfg, mode);
    }

    /// The configuration for the active Python editor: the test or file at
    /// the caret (`caret`), else the whole file ("Current File").
    pub(super) fn context_cfg(&self, caret: bool) -> Option<RunConfig> {
        let tab = self.active.and_then(|i| self.tabs.get(i))?;
        let path = tab.state.path.clone().filter(|p| {
            !p.starts_with("<typeshed>")
                && p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|extension| matches!(extension, "py" | "pyw"))
        })?;
        let (root, text) = (self.project_root(), tab.state.doc.text());
        Some(if caret {
            run_context::context_config(
                &root,
                &path,
                &text,
                tab.state.doc.line_of(tab.state.primary().head()),
            )
        } else {
            run_context::file_config(&root, &path, &text)
        })
    }

    /// Run / Debug for the toolbar, Shift+F10 / Shift+F9 (`context` false:
    /// the selected configuration, else "Current File": the editor's file,
    /// all tests of a test file) and Ctrl+Shift+F10 / Ctrl+Shift+F9
    /// (`context` true: the test at the caret). "Current File" stays selected.
    pub(super) fn launch_selected_or_context(&mut self, context: bool, mode: RunMode) {
        let selected = self
            .project
            .as_ref()
            .and_then(|p| p.run_configs.selected_config().cloned());
        let current_file_mode = selected.is_none();
        if let (false, Some(sel)) = (context, selected) {
            self.launch_config(sel, mode);
            return;
        }
        let Some(cfg) = self.context_cfg(context) else {
            self.set_status(if mode == RunMode::Debug {
                "Nothing to debug: open a Python file or create a run configuration"
            } else {
                "Nothing to run: open a Python file or create a run configuration"
            });
            return;
        };
        self.launch_context(cfg, mode);
        if current_file_mode {
            self.select_current_file();
        }
    }

    fn select_current_file(&mut self) {
        if let Some(p) = &mut self.project {
            if p.run_configs.selected.take().is_some() {
                let _ = p.run_configs.save(&p.root);
            }
        }
    }

    /// Debug a node of the Tests tool window with the last run's interpreter and options.
    pub(super) fn debug_test_node(&mut self, id: String) {
        if self.project_safe_mode() {
            self.set_status("Trust the project before launching or probing execution backends");
            return;
        }
        let Some(last) = self.test_panel.last_run().cloned() else {
            return;
        };
        let kind = match last.framework {
            Framework::Pytest => RunConfigKind::Pytest {
                target: id.clone(),
                keywords: String::new(),
            },
            Framework::Unittest => RunConfigKind::Unittest { target: id.clone() },
            other => {
                self.set_status(format!(
                    "Debugging {} tests is not available yet",
                    other.label()
                ));
                return;
            }
        };
        let label = if last.framework == Framework::Unittest {
            "Unittests"
        } else {
            "pytest"
        };
        let cfg = RunConfig {
            name: format!("{label} for {}", run_context::dotted_test_name(&id)),
            kind,
            interpreter: Some(last.interpreter.clone()),
            env: last.options.env.iter().cloned().collect(),
            working_dir: Some(
                last.options
                    .cwd
                    .clone()
                    .unwrap_or_else(|| last.root.clone()),
            ),
            interpreter_options: quote_args(&last.options.interpreter_options),
            temporary: true,
            ..Default::default()
        };
        self.launch_config(cfg, RunMode::Debug);
    }

    /// Rerun (Ctrl+F5): the Tests or Debug tool window's last launch when it
    /// is shown, else the active Run session, else the last test run.
    pub(super) fn rerun_action(&mut self) {
        if self.bottom == Some(BottomTool::Tests) && self.test_panel.has_run() {
            self.rerun_tests();
            return;
        }
        if self.bottom == Some(BottomTool::Debug) {
            if let Some(name) = self.debug.as_ref().map(|d| d.name.clone()) {
                let own = self
                    .run_panel
                    .launcher
                    .debug_launched
                    .clone()
                    .filter(|c| c.name == name);
                match self
                    .project
                    .as_ref()
                    .and_then(|p| p.run_configs.config_for_rerun(&name, own.as_ref()))
                {
                    Some(cfg) => self.launch_config(cfg, RunMode::Debug),
                    None => self.set_status(format!(
                        "Cannot rerun '{name}': its configuration no longer exists"
                    )),
                }
                return;
            }
        }
        if let Some(id) = self.run_panel.active {
            self.rerun_session(id);
            return;
        }
        if self.test_panel.has_run() {
            self.rerun_tests();
            return;
        }
        self.set_status("Nothing to rerun");
    }

    fn rerun_tests(&mut self) {
        match self.test_panel.rerun() {
            Ok(()) => self.bottom = Some(BottomTool::Tests),
            Err(e) => self.set_status(e),
        }
    }

    /// Runs the configuration or Run Anything command of a Run session again
    /// (see [`crate::run_panel::RunPanel::rerun_plan`]). What to run is
    /// resolved first: a running session is stopped and replaced only when
    /// its replacement starts (not in safe mode, with a missing input file,
    /// or when its `$Prompt$` is cancelled).
    pub(super) fn rerun_session(&mut self, id: u64) {
        let project_safe_mode = self.project_safe_mode();
        if !self.run_panel.authorize_rerun(id) {
            self.bottom = Some(BottomTool::Run);
            return;
        }
        if project_safe_mode {
            self.set_status(SAFE_MODE);
            return;
        }
        if self.rerun_external_tool(id) {
            return;
        }
        // An External Tool's session repeats its command line (tools_ui.rs).
        if let Some(cfg) = self
            .run_panel
            .sessions
            .iter()
            .find(|s| s.id == id)
            .and_then(|s| s.config.clone())
            .filter(|c| matches!(c.kind, RunConfigKind::Program { .. }))
        {
            self.start_program_session(cfg, true);
            return;
        }
        let Some(name) = self
            .run_panel
            .sessions
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.name.clone())
        else {
            return;
        };
        let none = RunConfigStore::default();
        let store = self.project.as_ref().map_or(&none, |p| &p.run_configs);
        let Some(plan) = self.run_panel.rerun_plan(id, store) else {
            self.set_status(format!(
                "Cannot rerun '{name}': its configuration no longer exists"
            ));
            return;
        };
        match plan {
            RerunPlan::Config(cfg) => self.launch_resolved(*cfg, RunMode::Run, false, Some(id)),
            RerunPlan::Command(name, spec) => {
                self.spawn_command_session_replacing(&name, spec, Some(id));
            }
        }
    }

    pub(super) fn spawn_command_session(&mut self, name: &str, spec: CommandSpec) {
        self.spawn_command_session_replacing(name, spec, None);
    }

    pub(super) fn spawn_command_session_replacing(
        &mut self,
        name: &str,
        spec: CommandSpec,
        replaces: Option<u64>,
    ) {
        if self.project_safe_mode() {
            self.set_status(SAFE_MODE);
            return;
        }
        if self.defer_run_replacement(
            replaces,
            super::run_configuration_workflows::ReplacementLaunch::Command(
                name.into(),
                spec.clone(),
            ),
        ) {
            return;
        }
        let id = self.next_session_id;
        self.next_session_id += 1;
        let env: std::collections::BTreeMap<String, String> = spec.env.iter().cloned().collect();
        if let Some(old) = replaces {
            if let Some(session) = self
                .run_panel
                .sessions
                .iter_mut()
                .find(|s| s.id == old && s.owns_process())
            {
                session.stop();
            }
        }
        let session =
            RunSession::spawn_command(id, name, &spec.program, &spec.args, &spec.cwd, &env);
        if !matches!(session.status, rustpy_run::SessionStatus::Failed(_)) {
            if let Some(old) = replaces.filter(|id| self.run_panel.can_replace(*id)) {
                self.run_panel.sessions.retain(|s| s.id != old);
            }
        }
        self.run_panel
            .launcher
            .commands
            .insert(name.to_string(), spec);
        self.run_panel.add_session(session);
        self.bottom = Some(BottomTool::Run);
    }

    /// Stop (Ctrl+F2): stops the only running process, or offers a picker
    /// when several are running.
    pub(super) fn stop_or_pick(&mut self) {
        let mut items: Vec<StopItem> = self
            .run_panel
            .sessions
            .iter()
            .filter(|s| s.owns_process())
            .map(|s| StopItem::Session(s.id, s.name.clone()))
            .collect();
        for debug in self
            .debug
            .iter()
            .chain(self.debug_sessions.iter().map(|(session, _)| session))
        {
            if debug.is_active() {
                items.push(StopItem::DebugSession(debug.id, debug.name.clone()));
            }
        }
        items.extend(self.py_console.configuration_stop_items());
        let ai = self.ai_worker_ids();
        if !ai.is_empty() {
            items.push(StopItem::AiWorkers(ai));
        }
        if self.web_languages.is_running() {
            items.push(StopItem::WebLanguages(self.web_languages.stamps()));
        }
        if self.collaboration_running() {
            items.push(StopItem::Collaboration(self.collaboration_stamp()));
        }
        items.extend(
            self.settings_sync_pending()
                .into_iter()
                .map(|(id, name, stamp)| StopItem::SettingsSync(id, name, stamp)),
        );
        items.extend(
            self.remote_development_pending()
                .into_iter()
                .map(|(id, name, stamp)| StopItem::RemoteDevelopment(id, name, stamp)),
        );
        items.extend(
            self.plugin_jobs()
                .into_iter()
                .map(|(id, name)| StopItem::PluginJob(id, name)),
        );
        items.extend(
            self.window_operation_jobs()
                .into_iter()
                .map(|(id, name)| StopItem::WindowOperation(id, name)),
        );
        if let Some((id, name, stamp)) = self.docker_services_pending() {
            items.push(StopItem::DockerSnapshot(id, name, stamp));
        }
        if let Some((id, title)) = self.ide_capture_owner() {
            items.push(StopItem::IdeDiagnostic(id, title));
        }
        if let Some(id) = self.diagram_panel.capture_owner() {
            items.push(StopItem::DiagramCapture(id));
        }
        items.extend(
            self.db_panel
                .database_owners()
                .into_iter()
                .map(|(id, name, _, _)| StopItem::Database(id, name)),
        );
        items.extend(self.before_launch_stop_items());
        items.extend(self.measured_stop_items());
        if let Some((id, label, _)) = self.update_transfer_state() {
            items.push(StopItem::Update(id, label));
        }
        items.extend(
            self.deploy
                .pending_transfers()
                .into_iter()
                .map(|(id, label, stamp)| StopItem::Deployment(id, label, stamp)),
        );
        if let Some((id, host)) = self.shared_index_download_owner() {
            items.push(StopItem::SharedIndex(id, host));
        }
        if self.web_preview_pending() {
            items.push(StopItem::WebPreview(self.web_preview_stamps()));
        }
        if self.data_files.busy() {
            items.push(StopItem::DataFiles(self.data_files.stamp()));
        }
        if !self.file_watcher_pending_labels().is_empty() {
            items.push(StopItem::FileWatchers(self.file_watcher_lifecycle_stamp()));
        }
        items.extend(
            self.http_panel
                .running()
                .into_iter()
                .map(|request| StopItem::Http(request.id, request.configuration)),
        );
        if items.len() > 1 {
            self.run_panel.launcher.open_stop(items);
        } else {
            self.stop_items(items);
        }
    }

    fn stop_items(&mut self, items: Vec<StopItem>) {
        for item in items {
            match item {
                StopItem::AiWorkers(ids) => rustpy_ai::cancel_workers(&ids),
                StopItem::PluginJob(id, _) => {
                    self.stop_plugin_job(id);
                }
                StopItem::WindowOperation(id, _) => self.stop_window_operation(id),
                StopItem::DockerSnapshot(id, _, stamp) => self.stop_docker_services(id, &stamp),
                StopItem::RemoteDevelopment(id, _, stamp) => {
                    if self
                        .remote_development_pending()
                        .iter()
                        .any(|owner| owner.0 == id && owner.2 == stamp)
                    {
                        self.stop_remote_development(id);
                    }
                }
                StopItem::WebLanguages(stamps) => {
                    if stamps == self.web_languages.stamps() {
                        self.stop_web_languages();
                    }
                }
                StopItem::Collaboration(stamp) => {
                    if stamp == self.collaboration_stamp() {
                        self.stop_collaboration();
                    }
                }
                StopItem::SettingsSync(id, _, stamp) => {
                    if self
                        .settings_sync_pending()
                        .iter()
                        .any(|owner| owner.0 == id && owner.2 == stamp)
                    {
                        self.stop_settings_sync(id);
                    }
                }
                StopItem::Database(id, _) => self.db_panel.stop_database(id),
                StopItem::IdeDiagnostic(id, _) => self.stop_ide_capture(Some(id)),
                StopItem::DiagramCapture(id) => {
                    if self.diagram_panel.capture_owner() == Some(id) {
                        self.diagram_panel.cancel_capture();
                    }
                }
                StopItem::Update(id, _) => {
                    self.cancel_update_transfer(id);
                }
                StopItem::Deployment(id, _, stamp) => {
                    self.stop_deployment(id, &stamp);
                }
                StopItem::Session(id, _) => {
                    self.run_panel.request_stop(id);
                    self.bottom = Some(BottomTool::Run);
                }
                StopItem::DebugSession(id, _) => {
                    for debug in self
                        .debug
                        .iter_mut()
                        .chain(self.debug_sessions.iter_mut().map(|(session, _)| session))
                    {
                        if debug.id == id {
                            debug.stop();
                            break;
                        }
                    }
                }
                StopItem::Console(id, _) => {
                    if self.py_console.select_service(id) {
                        self.py_console.stop();
                    }
                }
                StopItem::BeforeLaunch(id, _) => self.stop_before_launch(id),
                StopItem::SharedIndex(id, _) => self.stop_shared_index_download(Some(id)),
                StopItem::WebPreview(stamps) => self.stop_web_preview(&stamps),
                StopItem::DataFiles(stamp) => {
                    if stamp == self.data_files.stamp() {
                        self.data_files.cancel();
                    } else {
                        self.set_status("Data-file operation changed; open Stop again");
                    }
                }
                StopItem::FileWatchers(stamp) => {
                    if stamp == self.file_watcher_lifecycle_stamp() {
                        self.stop_file_watchers();
                    } else {
                        self.set_status("Pending file watchers changed; open Stop again to select the current jobs");
                    }
                }
                StopItem::Http(id, _) => {
                    self.stop_http_request(id);
                }
                StopItem::Debug(_) => {
                    if let Some(d) = &mut self.debug {
                        d.stop();
                    }
                }
                StopItem::Measured(id, _) => self.stop_measurement(id),
                StopItem::Tests => self.test_panel.stop(),
                StopItem::Coverage => {
                    if let Some(r) = &mut self.coverage_panel.run {
                        r.stop();
                    }
                }
                StopItem::Concurrency => self.concurrency_panel.stop(),
                StopItem::Profiler => {
                    if let Some(r) = &mut self.profiler_panel.run {
                        r.stop();
                    }
                }
            }
        }
    }

    /// File name of the active editor when it can be run as "Current File".
    fn current_file_label(&self) -> Option<String> {
        let path = self
            .active
            .and_then(|i| self.tabs.get(i))
            .and_then(|t| t.state.path.clone())?;
        (!path.starts_with("<typeshed>") && path.extension().and_then(|e| e.to_str()) == Some("py"))
            .then(|| {
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
    }

    /// Opens the Run… (`debug` false) or Debug… configuration chooser.
    pub(super) fn open_run_chooser(&mut self, debug: bool) {
        let root = self.project_root();
        let current_file = self.current_file_label();
        let (configs, selected): (&[RunConfig], Option<&str>) = match &self.project {
            Some(p) => (
                &p.run_configs.configs,
                p.run_configs.selected_config().map(|c| c.name.as_str()),
            ),
            None => (&[], None),
        };
        let input = LauncherInput {
            root: &root,
            configs,
            selected_config: selected,
            current_file,
        };
        self.run_panel.launcher.open_chooser(debug, &input);
    }

    /// Opens Run Anything (double Ctrl).
    pub(super) fn open_run_anything(&mut self) {
        let Some(root) = self.project.as_ref().map(|p| p.root.clone()) else {
            self.set_status("Open a project to use Run Anything");
            return;
        };
        self.run_panel.launcher.open_anything(&root);
    }

    /// Launcher keys: double Ctrl opens Run Anything; while a launcher popup
    /// is open it owns the keyboard (returns true: skip other shortcuts).
    pub(super) fn launcher_shortcuts(&mut self, ui: &mut egui::Ui) -> bool {
        let can_arm = self.editor_keyboard()
            && !self.find_in_files.visible
            && !self.nav.popup_visible()
            && !self.completion.visible
            && self.intentions.is_none()
            && !self.coordinated_quit_active()
            && !self.coordinated_quit_committed
            && !self.run_panel.launcher.is_open();
        if can_arm {
            let editor = self
                .active
                .and_then(|index| self.tabs.get(index))
                .map(|tab| (tab.state.doc.identity(), tab.view.id));
            let owner = egui::Id::new((
                "run-anything-owner",
                editor,
                ui.memory(|memory| memory.focused()),
            ));
            if self.run_panel.launcher.double_ctrl_for_owner(ui, owner) {
                self.open_run_anything();
            }
        } else {
            self.run_panel.launcher.reset_ctrl_gesture();
        }
        if !self.run_panel.launcher.is_open() {
            return false;
        }
        let root = self.project_root();
        let current_file = self.current_file_label();
        let (open, events) = {
            let (configs, selected): (&[RunConfig], Option<&str>) = match &self.project {
                Some(p) => (
                    &p.run_configs.configs,
                    p.run_configs.selected_config().map(|c| c.name.as_str()),
                ),
                None => (&[], None),
            };
            let input = LauncherInput {
                root: &root,
                configs,
                selected_config: selected,
                current_file,
            };
            self.run_panel.launcher.intercept_keys(ui, &input)
        };
        for ev in events {
            self.handle_launcher_event(ev);
        }
        open
    }

    /// Draws the open launcher popup.
    pub(super) fn launcher_ui(&mut self, ctx: &egui::Context) {
        if !self.run_panel.launcher.is_open() {
            return;
        }
        let theme = self.theme.clone();
        let root = self.project_root();
        let current_file = self.current_file_label();
        let events = {
            let (configs, selected): (&[RunConfig], Option<&str>) = match &self.project {
                Some(p) => (
                    &p.run_configs.configs,
                    p.run_configs.selected_config().map(|c| c.name.as_str()),
                ),
                None => (&[], None),
            };
            let input = LauncherInput {
                root: &root,
                configs,
                selected_config: selected,
                current_file,
            };
            self.run_panel.launcher.ui(ctx, &theme, &input)
        };
        if !self.run_panel.launcher.is_open() {
            self.focus_editor = true;
        }
        for ev in events {
            self.handle_launcher_event(ev);
        }
    }

    fn handle_launcher_event(&mut self, ev: LauncherEvent) {
        match ev {
            LauncherEvent::Launch(name, mode) => {
                match self.project.as_ref().and_then(|p| {
                    p.run_configs
                        .configs
                        .iter()
                        .find(|c| c.name == name)
                        .cloned()
                }) {
                    Some(cfg) => self.launch_config(cfg, mode),
                    None => self.set_status(format!("No run configuration named '{name}'")),
                }
            }
            LauncherEvent::LaunchCurrentFile(mode) => {
                self.select_current_file();
                self.launch_selected_or_context(false, mode);
            }
            LauncherEvent::EditConfigurations => self.dialog = Dialog::RunConfigurations,
            LauncherEvent::Command { line, debug } => self.run_anything_command(&line, debug),
            LauncherEvent::Stop(items) => self.stop_items(items),
            LauncherEvent::Prompted {
                cfg,
                mode,
                answer,
                replaces,
            } => {
                let mut cfg = *cfg;
                cfg.parameters = cfg.parameters.replace("$Prompt$", &answer);
                if let Some(wd) = &cfg.working_dir {
                    cfg.working_dir = Some(PathBuf::from(
                        wd.to_string_lossy().replace("$Prompt$", &answer),
                    ));
                }
                self.launch_now_with_prompt(cfg, mode, replaces, Some(&answer));
            }
        }
    }

    /// Executes a Run Anything command line (see [`crate::run_anything::plan_command`]).
    fn run_anything_command(&mut self, line: &str, debug: bool) {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode {
            self.set_status(SAFE_MODE);
            return;
        }
        let Some(p) = &self.project else { return };
        let root = p.root.clone();
        let interp = p.interpreter.clone();
        self.run_panel.launcher.remember(&root, line);
        let Some(plan) = crate::run_anything::plan_command(line, &root) else {
            return;
        };
        let name: String = if line.chars().count() > 48 {
            format!("{}…", line.chars().take(47).collect::<String>())
        } else {
            line.trim().to_string()
        };
        match plan {
            CommandPlan::Pytest { targets, args } => {
                if debug {
                    let all: Vec<String> = targets.into_iter().chain(args).collect();
                    let cfg = RunConfig {
                        name,
                        kind: RunConfigKind::PythonModule {
                            module: "pytest".into(),
                        },
                        parameters: quote_args(&all),
                        working_dir: Some(root),
                        temporary: true,
                        ..Default::default()
                    };
                    self.launch_config(cfg, RunMode::Debug);
                    return;
                }
                let Some(interp) = interp.map(|i| i.path) else {
                    self.set_status("No Python interpreter configured");
                    return;
                };
                if !self.save_all() {
                    return;
                }
                let options = self.default_test_options();
                self.bottom = Some(BottomTool::Tests);
                self.begin_measurement(super::run_configuration_sessions::Kind::Tests, &name);
                if let Err(e) = self.test_panel.start_with(
                    Framework::Pytest,
                    &interp,
                    &root,
                    targets,
                    args,
                    options,
                ) {
                    self.set_status(e);
                }
            }
            CommandPlan::Script {
                interpreter_options,
                script,
                args,
            } => {
                let path = if Path::new(&script).is_absolute() {
                    PathBuf::from(&script)
                } else {
                    root.join(&script)
                };
                let mut cfg = RunConfig::for_script(&path);
                cfg.name = name;
                cfg.working_dir = Some(root);
                cfg.parameters = quote_args(&args);
                cfg.interpreter_options = quote_args(&interpreter_options);
                self.launch_config(cfg, if debug { RunMode::Debug } else { RunMode::Run });
            }
            CommandPlan::Process { program, args } => {
                if debug {
                    self.set_status(
                        "Only Python scripts and pytest commands can be debugged; running instead",
                    );
                }
                if !self.save_all() {
                    return;
                }
                let program = match program {
                    Some(p) => PathBuf::from(p),
                    None => interp.as_ref().map(|i| i.path.clone()).unwrap_or_else(|| {
                        PathBuf::from(if cfg!(windows) { "python" } else { "python3" })
                    }),
                };
                let mut env: Vec<(String, String)> =
                    crate::terminal_panel::venv_env(interp.as_ref());
                env.push(("PYTHONUNBUFFERED".into(), "1".into()));
                env.push(("PYTHONIOENCODING".into(), "utf-8".into()));
                if let Some(pp) = self.pythonpath_for(&RunConfig::default()) {
                    env.push(("PYTHONPATH".into(), pp));
                }
                self.spawn_command_session(
                    &name,
                    CommandSpec {
                        program,
                        args,
                        cwd: root,
                        env,
                    },
                );
            }
        }
    }

    /// The Run/Debug Configurations dialog; returns whether it stays open.
    pub(super) fn run_configurations_dialog(&mut self, ctx: &egui::Context) -> bool {
        let mut open = true;
        let theme = self.theme.clone();
        let Some(p) = &mut self.project else {
            return false;
        };
        let events = {
            let input = DialogInput {
                root: &p.root,
                interpreters: &p.interpreters,
                project_interpreter: p.interpreter.as_ref(),
            };
            crate::run_config_dialog::show(ctx, &theme, &mut p.run_configs, &input, &mut open)
        };
        let mut launch = None;
        for ev in events {
            match ev {
                RunCfgEvent::Changed => {
                    let _ = p.run_configs.save(&p.root);
                }
                RunCfgEvent::Run(cfg) => launch = Some((cfg, RunMode::Run)),
                RunCfgEvent::Debug(cfg) => launch = Some((cfg, RunMode::Debug)),
            }
        }
        if let Some((cfg, mode)) = launch {
            self.approve_run_configuration(&cfg, mode);
            self.launch_config(cfg, mode);
        }
        open
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every action of the Run menu has its own `handle_action` arm ("Debug…"
    /// used to end in the "is not implemented yet" catch-all).
    #[test]
    fn module_run_sdk_and_cwd_follow_target_and_preserve_explicit_overrides() {
        let root = Path::new("/work/app");
        let module = PathBuf::from("/work/service");
        let model = rustpy_project::ProjectModel {
            sdk: Some(PathBuf::from("/python/project")),
            modules: vec![rustpy_project::project::ProjectModule {
                content_root: module.clone(),
                sdk: Some(PathBuf::from("/python/service")),
                ..Default::default()
            }],
            ..Default::default()
        };
        let config = RunConfig::for_script(&module.join("main.py"));
        let launch = materialize_project_run_config(&config, root, &model);
        assert_eq!(
            launch.interpreter.as_deref(),
            Some(Path::new("/python/service"))
        );
        assert_eq!(launch.working_dir, Some(module.clone()));
        let mut explicit = config;
        explicit.interpreter = Some(PathBuf::from("/python/override"));
        explicit.working_dir = Some(PathBuf::from("/work/custom"));
        let launch = materialize_project_run_config(&explicit, root, &model);
        assert_eq!(launch.interpreter, explicit.interpreter);
        assert_eq!(launch.working_dir, explicit.working_dir);
        let test = RunConfig::for_pytest("../service/tests/test_api.py::test_get", root);
        let launch = materialize_project_run_config(&test, root, &model);
        assert_eq!(
            launch.interpreter.as_deref(),
            Some(Path::new("/python/service"))
        );
    }

    #[test]
    fn run_menu_actions_are_handled() {
        let src = include_str!("../app.rs");
        let start = src.find("ui.menu_button(\"Run\"").expect("Run menu");
        let menu = &src[start..start + src[start..].find(']').expect("Run menu actions")];
        let arms = super::super::source_inspection::handled_actions(src);
        let actions: Vec<String> = menu
            .match_indices("Action::")
            .map(|(i, _)| {
                menu[i + "Action::".len()..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .collect()
            })
            .collect();
        for expected in [
            "ChooseRunConfiguration",
            "ChooseDebugConfiguration",
            "Rerun",
            "RunAnything",
            "Stop",
        ] {
            assert!(
                actions.iter().any(|a| a == expected),
                "{expected} is not in the Run menu: {actions:?}"
            );
        }
        let missing: Vec<&String> = actions
            .iter()
            .filter(|a| !arms.contains(a.as_str()))
            .collect();
        assert!(
            missing.is_empty(),
            "Run menu actions without a handle_action arm: {missing:?}"
        );
    }
}

#[cfg(all(test, unix))]
mod rerun_tests {
    use super::*;
    use crate::run_anything::Popup;

    /// A scratch project directory with a script, removed on drop.
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("rustpy-rerun-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("srv.py"), "print('serving')\n").unwrap();
        Scratch(dir.canonicalize().unwrap())
    }

    /// An IDE window on the project `root`, without interpreter discovery or Git.
    fn app_with_project(root: &Path) -> IdeApp {
        let mut settings = Settings::default();
        settings.run_console.confirm_rerun = false;
        settings.trust_project(root);
        let mut app = IdeApp::new(settings, None);
        app.run_panel.settings.confirm_rerun = false;
        app.project_trust_identity = rustpy_settings::trust::DirectoryIdentity::capture(root).ok();
        app.project = Some(Project {
            root: root.to_path_buf(),
            tree: ProjectTree::new(root.to_path_buf()),
            analysis: Arc::new(Analysis::for_project(root)),
            run_configs: RunConfigStore::load(root),
            interpreters: Vec::new(),
            interpreter: None,
            interpreter_rx: None,
            git: None,
            git_refreshed: -100.0,
            git_branch: None,
        });
        app
    }

    #[test]
    fn actual_launcher_frames_ignore_dirty_clipboard_ctrl_chords() {
        let dir = scratch("clipboard-ctrl");
        let ctx = egui::Context::default();
        let mut app = app_with_project(&dir.0);
        let ctrl = |pressed| egui::Event::Key {
            key: egui::Key::ControlLeft,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: if pressed {
                egui::Modifiers::CTRL
            } else {
                egui::Modifiers::NONE
            },
        };
        for (time, events) in [
            (1.0, vec![ctrl(true), egui::Event::Copy, ctrl(false)]),
            (
                1.1,
                vec![
                    ctrl(true),
                    egui::Event::Paste("clipboard".into()),
                    ctrl(false),
                ],
            ),
        ] {
            let output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| {
                    assert!(!app.launcher_shortcuts(ui));
                    assert!(!app.run_panel.launcher.is_open());
                },
            );
            output.drop_without_applying_deltas();
        }
        assert_eq!(
            std::fs::read_to_string(dir.0.join("srv.py")).unwrap(),
            "print('serving')\n"
        );
    }

    #[test]
    fn launcher_ctrl_tap_does_not_cross_a_modal_owner() {
        let dir = scratch("modal-ctrl");
        let ctx = egui::Context::default();
        let mut app = app_with_project(&dir.0);
        let tap = || {
            vec![true, false]
                .into_iter()
                .map(|pressed| egui::Event::Key {
                    key: egui::Key::ControlLeft,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers: if pressed {
                        egui::Modifiers::CTRL
                    } else {
                        egui::Modifiers::NONE
                    },
                })
                .collect()
        };
        for (time, modal, events) in [
            (1.0, false, tap()),
            (1.05, true, vec![]),
            (1.1, false, tap()),
        ] {
            app.dialog = if modal {
                Dialog::GoToLine(String::new())
            } else {
                Dialog::None
            };
            let output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| {
                    assert!(!app.launcher_shortcuts(ui));
                },
            );
            output.drop_without_applying_deltas();
            assert!(!app.run_panel.launcher.is_open());
        }
    }

    #[test]
    fn completed_tap_launcher_preserves_actual_held_ctrl_caret_clone() {
        let dir = scratch("held-ctrl-clone");
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let mut app = app_with_project(&dir.0);
        let source = "alpha π😀\nbeta π😀\n";
        app.push_tab(EditorState::from_text(source, Language::PlainText));
        let key = |key, pressed| egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: if pressed {
                egui::Modifiers::CTRL
            } else {
                egui::Modifiers::NONE
            },
        };
        for (time, events) in [
            (0.0, vec![]),
            (0.01, vec![]),
            (1.0, vec![key(egui::Key::ControlLeft, true)]),
            (1.02, vec![key(egui::Key::ControlLeft, false)]),
            (1.1, vec![key(egui::Key::ControlLeft, true)]),
            (1.12, vec![key(egui::Key::ArrowDown, true)]),
            (1.14, vec![key(egui::Key::ControlLeft, false)]),
        ] {
            let output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 720.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    app.global_shortcuts(ui);
                    egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
                    assert!(!app.run_panel.launcher.is_open());
                },
            );
            output.drop_without_applying_deltas();
        }
        let state = &app.tabs[app.active.unwrap()].state;
        assert_eq!(state.carets.len(), 2);
        assert_eq!(state.doc.text(), source);
        assert!(!state.doc.can_undo());
    }

    /// A running Run session started from `cfg` (a long `sleep` stands in for its process).
    fn running(app: &mut IdeApp, cfg: &RunConfig) -> u64 {
        let id = app.next_session_id;
        app.next_session_id += 1;
        app.run_panel.add_session(RunSession::spawn_command(
            id,
            &cfg.name,
            Path::new("sleep"),
            &["30".into()],
            Path::new("/"),
            &Default::default(),
        ));
        app.run_panel.record_launch(id, cfg.clone());
        id
    }

    fn poll_replacement(app: &mut IdeApp) {
        let ctx = egui::Context::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while app.run_workflows_pending() && std::time::Instant::now() < deadline {
            app.run_panel.poll();
            app.poll_run_workflows(&ctx);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn is_running(app: &IdeApp, id: u64) -> bool {
        app.run_panel
            .sessions
            .iter()
            .any(|s| s.id == id && s.is_running())
    }

    fn stop_all(app: &mut IdeApp) {
        for s in &mut app.run_panel.sessions {
            s.stop();
        }
    }

    /// Rerun of a running session whose configuration has a `$Prompt$`
    /// parameter asks first: cancelling the prompt keeps the session (it
    /// used to be stopped and its tab and output removed before the prompt
    /// opened); the answer replaces it with the new run.
    #[test]
    fn rerun_replaces_a_session_only_when_the_new_run_starts() {
        let dir = scratch("prompt");
        let mut app = app_with_project(&dir.0);
        let cfg = RunConfig {
            name: "srv".into(),
            parameters: "--port $Prompt$".into(),
            temporary: false,
            ..RunConfig::for_script(&dir.0.join("srv.py"))
        };
        app.project
            .as_mut()
            .unwrap()
            .run_configs
            .upsert(cfg.clone());
        let id = running(&mut app, &cfg);
        app.rerun_session(id);
        poll_replacement(&mut app);
        let asked = app.run_panel.launcher.is_open();
        let kept_while_asking = is_running(&app, id);
        app.run_panel.launcher.popup = None; // Cancel
        let kept_after_cancel = is_running(&app, id);
        app.rerun_session(id);
        poll_replacement(&mut app);
        let answered = match app.run_panel.launcher.popup.take() {
            Some(Popup::Prompt {
                cfg,
                mode,
                replaces,
                ..
            }) => {
                app.handle_launcher_event(LauncherEvent::Prompted {
                    cfg,
                    mode,
                    answer: "8080".into(),
                    replaces,
                });
                replaces
            }
            _ => None,
        };
        poll_replacement(&mut app);
        let sessions: Vec<(u64, String)> = app
            .run_panel
            .sessions
            .iter()
            .map(|s| (s.id, s.name.clone()))
            .collect();
        stop_all(&mut app);
        assert!(asked, "the prompt asks for the value");
        assert!(
            kept_while_asking && kept_after_cancel,
            "nothing stops before the answer"
        );
        assert_eq!(
            answered,
            Some(id),
            "the prompt knows the session it replaces"
        );
        assert!(
            sessions.iter().all(|(s, _)| *s != id) && sessions.iter().any(|(_, n)| n == "srv"),
            "{sessions:?}"
        );
    }

    #[test]
    fn server_rerun_releases_socket_before_replacement_executes() {
        let dir = scratch("server-socket");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        std::fs::write(dir.0.join("srv.py"), "import os, signal, socket, time\nsignal.signal(signal.SIGTERM, lambda *_: time.sleep(30))\ns = socket.socket()\ns.bind(('127.0.0.1', int(__import__('sys').argv[1])))\ns.listen()\nopen('ready', 'w').write(str(os.getpid()))\ntime.sleep(30)\n").unwrap();
        let mut app = app_with_project(&dir.0);
        let cfg = RunConfig {
            parameters: port.to_string(),
            working_dir: Some(dir.0.clone()),
            ..RunConfig::for_script(&dir.0.join("srv.py"))
        };
        app.launch_config(cfg, RunMode::Run);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !dir.0.join("ready").exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let first_pid = std::fs::read_to_string(dir.0.join("ready")).ok();
        let id = app.run_panel.sessions[0].id;
        app.rerun_session(id);
        poll_replacement(&mut app);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut second_pid = first_pid.clone();
        while second_pid == first_pid && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
            second_pid = std::fs::read_to_string(dir.0.join("ready")).ok();
        }
        stop_all(&mut app);
        assert!(
            first_pid.is_some() && second_pid.is_some() && first_pid != second_pid,
            "replacement must bind the same port after the old process exits"
        );
    }

    #[test]
    fn failed_replacement_preserves_session_output() {
        let dir = scratch("spawn-failure");
        let mut app = app_with_project(&dir.0);
        let cfg = RunConfig {
            interpreter: Some(dir.0.join("missing-python")),
            ..RunConfig::for_script(&dir.0.join("srv.py"))
        };
        let id = running(&mut app, &cfg);
        app.run_panel
            .sessions
            .iter_mut()
            .find(|s| s.id == id)
            .unwrap()
            .output
            .push(rustpy_run::OutputChunk {
                kind: rustpy_run::OutputKind::Stdout,
                text: "valuable output".into(),
            });
        app.rerun_session(id);
        poll_replacement(&mut app);
        let kept =
            app.run_panel.sessions.iter().any(|s| {
                s.id == id && s.output.iter().any(|chunk| chunk.text == "valuable output")
            });
        let failed = app
            .run_panel
            .sessions
            .iter()
            .any(|s| s.id != id && matches!(s.status, rustpy_run::SessionStatus::Failed(_)));
        stop_all(&mut app);
        assert!(
            kept && failed,
            "failed spawn must retain the original output and report the failure"
        );
    }

    #[test]
    fn failed_rerun_preserves_output_after_the_old_session_is_polled() {
        let dir = scratch("finished-output");
        let mut app = app_with_project(&dir.0);
        let cfg = RunConfig {
            interpreter: Some(dir.0.join("missing-python")),
            ..RunConfig::for_script(&dir.0.join("srv.py"))
        };
        let id = running(&mut app, &cfg);
        let old = app
            .run_panel
            .sessions
            .iter_mut()
            .find(|s| s.id == id)
            .unwrap();
        old.output.push(rustpy_run::OutputChunk {
            kind: rustpy_run::OutputKind::Stdout,
            text: "valuable output".into(),
        });
        old.stop();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while old.is_running() && std::time::Instant::now() < deadline {
            old.poll();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!old.is_running());
        app.rerun_session(id);
        poll_replacement(&mut app);
        app.run_panel.poll();
        app.rerun_session(id);
        poll_replacement(&mut app);
        assert!(app.run_panel.sessions.iter().any(|s| s.id == id && s.output.iter().any(|chunk| chunk.text == "valuable output")), "repeated failed reruns preserve finished output");
    }

    #[test]
    fn missing_rerun_prerequisite_keeps_running_session_and_output() {
        let dir = scratch("missing-prerequisite");
        let mut app = app_with_project(&dir.0);
        let mut cfg = RunConfig::for_script(&dir.0.join("srv.py"));
        cfg.workflow
            .before_launch
            .push(rustpy_project::run_workflow::BeforeLaunch {
                enabled: true,
                task: rustpy_project::run_workflow::Task::Configuration {
                    name: "missing".into(),
                },
            });
        let id = running(&mut app, &cfg);
        app.run_panel
            .sessions
            .iter_mut()
            .find(|s| s.id == id)
            .unwrap()
            .output
            .push(rustpy_run::OutputChunk {
                kind: rustpy_run::OutputKind::Stdout,
                text: "valuable output".into(),
            });
        app.rerun_session(id);
        assert!(is_running(&app, id));
        assert!(!app.run_workflows_pending());
        assert!(app
            .run_panel
            .sessions
            .iter()
            .any(|s| s.id == id && s.output.iter().any(|chunk| chunk.text == "valuable output")));
        stop_all(&mut app);
    }

    #[test]
    fn deferred_transient_launch_preserves_absent_config_identity_and_quit_membership() {
        for add_same_name in [false, true] {
            let dir = scratch(if add_same_name {
                "transient-replaced"
            } else {
                "transient-absent"
            });
            let mut app = app_with_project(&dir.0);
            let cfg = RunConfig::for_script(&dir.0.join("srv.py"));
            let id = running(&mut app, &cfg);
            assert!(app.defer_run_replacement(
                Some(id),
                super::super::run_configuration_workflows::ReplacementLaunch::Configuration(
                    Box::new(cfg.clone()),
                    RunMode::Run
                )
            ));
            assert!(app
                .run_workflow_stamps()
                .iter()
                .any(|stamp| stamp.starts_with(&format!("replacement:{id}:"))));
            if add_same_name {
                app.project
                    .as_mut()
                    .unwrap()
                    .run_configs
                    .upsert(cfg.clone());
            }
            poll_replacement(&mut app);
            assert!(!app.run_workflows_pending());
            assert!(app.run_workflow_stamps().is_empty());
            let launched = app
                .run_panel
                .sessions
                .iter()
                .any(|session| session.id != id);
            assert_eq!(
                launched, !add_same_name,
                "an absent source remains valid only while no same-name configuration is added"
            );
            stop_all(&mut app);
        }
    }

    #[test]
    fn deferred_rerun_is_cancelled_if_configuration_changes_before_retirement() {
        let dir = scratch("changed-before-retirement");
        let mut app = app_with_project(&dir.0);
        let cfg = RunConfig::for_script(&dir.0.join("srv.py"));
        app.project
            .as_mut()
            .unwrap()
            .run_configs
            .upsert(cfg.clone());
        let id = running(&mut app, &cfg);
        app.rerun_session(id);
        assert!(
            app.run_workflows_pending(),
            "termination must be polled before replacement execution"
        );
        app.project
            .as_mut()
            .unwrap()
            .run_configs
            .configs
            .iter_mut()
            .find(|stored| stored.name == cfg.name)
            .unwrap()
            .parameters = "changed".into();
        poll_replacement(&mut app);
        assert_eq!(app.run_panel.sessions.len(), 1);
        assert_eq!(app.run_panel.sessions[0].id, id);
        assert!(!app.run_workflows_pending());
        stop_all(&mut app);
    }

    #[test]
    fn safe_mode_command_rerun_keeps_existing_session() {
        let dir = scratch("safe-command");
        let mut app = app_with_project(&dir.0);
        let cfg = RunConfig::for_script(&dir.0.join("srv.py"));
        let id = running(&mut app, &cfg);
        app.run_panel.launcher.launched.remove(&id);
        app.run_panel.launcher.commands.insert(
            cfg.name.clone(),
            CommandSpec {
                program: PathBuf::from("sleep"),
                args: vec!["30".into()],
                cwd: dir.0.clone(),
                env: vec![],
            },
        );
        app.safe_mode = true;
        app.rerun_session(id);
        poll_replacement(&mut app);
        let kept = is_running(&app, id) && app.run_panel.sessions.len() == 1;
        stop_all(&mut app);
        assert!(kept, "safe mode must not rerun bare commands");
    }

    #[test]
    fn failed_command_rerun_preserves_session_output() {
        let dir = scratch("command-failure");
        let mut app = app_with_project(&dir.0);
        let cfg = RunConfig::for_script(&dir.0.join("srv.py"));
        let id = running(&mut app, &cfg);
        app.run_panel.launcher.launched.remove(&id);
        app.run_panel.launcher.commands.insert(
            cfg.name.clone(),
            CommandSpec {
                program: dir.0.join("missing-command"),
                args: vec![],
                cwd: dir.0.clone(),
                env: vec![],
            },
        );
        app.rerun_session(id);
        poll_replacement(&mut app);
        let kept = app.run_panel.sessions.iter().any(|s| s.id == id);
        let failed = app
            .run_panel
            .sessions
            .iter()
            .any(|s| s.id != id && matches!(s.status, rustpy_run::SessionStatus::Failed(_)));
        stop_all(&mut app);
        assert!(
            kept && failed,
            "failed command spawn must retain the original session"
        );
    }

    /// Rerun of a session whose permanent configuration was deleted runs it
    /// as a temporary configuration: the rerun used to store it again and
    /// save it to the project's run configurations.
    #[test]
    fn rerun_of_a_deleted_configuration_does_not_store_it_again() {
        let dir = scratch("deleted");
        let mut app = app_with_project(&dir.0);
        let cfg = RunConfig {
            name: "plain".into(),
            temporary: false,
            ..RunConfig::for_script(&dir.0.join("srv.py"))
        };
        // Started from `cfg`, which is no longer in the store.
        let id = running(&mut app, &cfg);
        app.rerun_session(id);
        poll_replacement(&mut app);
        let stored = app
            .project
            .as_ref()
            .and_then(|p| p.run_configs.configs.iter().find(|c| c.name == "plain"))
            .map(|c| c.temporary);
        let reran = app
            .run_panel
            .sessions
            .iter()
            .any(|s| s.id != id && s.name == "plain");
        stop_all(&mut app);
        assert!(reran, "the configuration runs again");
        assert_eq!(stored, Some(true), "as a temporary configuration");
        assert!(
            RunConfigStore::load(&dir.0).configs.is_empty(),
            "nothing is saved"
        );
    }
}

/// Lines of the run targets of a Python file (tests, main guard, doctests, `#%%` cells).
fn python_run_lines_in(
    root: &std::path::Path,
    path: &std::path::Path,
    text: &str,
    parse: &rustpy_syntax::Parse,
) -> Vec<usize> {
    let mut lines: Vec<usize> = run_context::gutter_targets_in(root, path, text, parse)
        .into_iter()
        .map(|(l, _)| l)
        .collect();
    lines.extend(crate::editor::console_code::cell_marker_lines(text));
    lines.sort_unstable();
    lines.dedup();
    lines
}
