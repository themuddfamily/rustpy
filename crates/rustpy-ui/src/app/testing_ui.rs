//! Integrated testing preferences and test-window launch actions.
use super::*;
use crate::a11y::DialogWindow;
use crate::run_context;
#[derive(Default)]
pub(super) struct State {
    pending_main: Option<(RunConfig, run_context::RunMode)>,
    remember: bool,
}
pub(super) fn manager_executable(
    settings: &rustpy_settings::testing::TestingSettings,
    manager: rustpy_project::environment_manager::Manager,
) -> String {
    use rustpy_project::environment_manager::Manager;
    let configured = match manager {
        Manager::Pipenv => &settings.pipenv,
        Manager::Poetry => &settings.poetry,
        Manager::Uv => &settings.uv,
        _ => return manager.executable().into(),
    };
    if configured.trim().is_empty() {
        manager.executable().into()
    } else {
        configured.clone()
    }
}
impl IdeApp {
    pub(super) fn test_fixture_usages(&mut self, path: PathBuf, line: usize) {
        self.open_file(&path);
        if let Some(index) = self.opened_editor_index(&path) {
            let text = self.tabs[index].state.doc.text();
            let location = text
                .lines()
                .enumerate()
                .skip(line.saturating_sub(1))
                .take(32)
                .find_map(|(row, text)| {
                    if text.trim_start().starts_with("def ")
                        || text.trim_start().starts_with("async def ")
                    {
                        text.find("def ").map(|column| (row + 1, column + 5))
                    } else {
                        None
                    }
                });
            let (line, column) = location
                .map(|(line, column)| (line, Some(column)))
                .unwrap_or((line, None));
            self.tabs[index].state.go_to_line(line, column);
            self.find_usages();
        }
    }
    pub(super) fn testing_settings_page(&mut self, ui: &mut egui::Ui) {
        if self.settings.testing.default_runner == "auto"
            && self.settings.python_testing.runner != "auto"
        {
            self.settings.testing.default_runner = self.settings.python_testing.runner.clone();
        }
        if self.settings.testing.unittest_pattern == "test*.py"
            && self.settings.python_testing.unittest_pattern != "test*.py"
        {
            self.settings.testing.unittest_pattern =
                self.settings.python_testing.unittest_pattern.clone();
        }
        if self.settings.testing.pytest_path.is_none()
            && !self.settings.python_testing.pytest_path.is_empty()
        {
            self.settings.testing.pytest_path =
                Some(PathBuf::from(&self.settings.python_testing.pytest_path));
        }
        let settings = &mut self.settings.testing;
        let previous_runner = settings.default_runner.clone();
        ui.heading("Testing");
        egui::ComboBox::from_label("Default test runner")
            .selected_text(&settings.default_runner)
            .show_ui(ui, |ui| {
                for runner in [
                    "auto", "pytest", "unittest", "doctest", "nose", "nose2", "trial", "tox",
                    "behave", "lettuce",
                ] {
                    ui.selectable_value(
                        &mut settings.default_runner,
                        runner.into(),
                        crate::i18n::tr_static(runner),
                    );
                }
            });
        fn path_option(ui: &mut egui::Ui, label: &str, path: &mut Option<PathBuf>) {
            ui.horizontal(|ui| {
                ui.label(label);
                let mut text = path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                if ui.text_edit_singleline(&mut text).changed() {
                    *path = (!text.trim().is_empty()).then(|| PathBuf::from(text.trim()));
                }
            });
        }
        path_option(
            ui,
            "pytest package / module directory",
            &mut settings.pytest_path,
        );
        ui.checkbox(
            &mut settings.suppress_buffering,
            "Suppress pytest output buffering (-s)",
        );
        ui.horizontal(|ui| {
            ui.label("unittest discovery pattern");
            ui.text_edit_singleline(&mut settings.unittest_pattern);
        });
        path_option(
            ui,
            "unittest top-level directory",
            &mut settings.unittest_top_level,
        );
        path_option(ui, "Requirements file", &mut settings.requirements_file);
        for (label, executable) in [
            ("Pipenv executable", &mut settings.pipenv),
            ("Poetry executable", &mut settings.poetry),
            ("uv executable", &mut settings.uv),
        ] {
            ui.horizontal(|ui| {
                ui.label(crate::i18n::tr_static(label));
                ui.text_edit_singleline(executable);
            });
        }
        ui.checkbox(
            &mut settings.notify_detection,
            "Notify when the project test framework is detected",
        );
        egui::ComboBox::from_label("Files containing tests and __main__")
            .selected_text(&settings.main_conflict)
            .show_ui(ui, |ui| {
                for (value, label) in [
                    ("ask", "Ask which to run"),
                    ("script", "Run as script"),
                    ("tests", "Run tests"),
                ] {
                    ui.selectable_value(
                        &mut settings.main_conflict,
                        value.into(),
                        crate::i18n::tr_static(label),
                    );
                }
            });
        if settings.default_runner != previous_runner {
            self.settings.python_testing.runner = settings.default_runner.clone();
        }
        self.settings.python_testing.unittest_pattern = settings.unittest_pattern.clone();
        self.settings.python_testing.pytest_path = settings
            .pytest_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
    }
    pub(super) fn configured_test_framework(&self) -> Option<rustpy_test::Framework> {
        self.preferred_test_framework(
            &self
                .project
                .as_ref()
                .map(|project| project.root.clone())
                .unwrap_or_default(),
        )
    }
    pub(super) fn configure_test_tools(
        &self,
        framework: rustpy_test::Framework,
        targets: &[String],
        args: &mut Vec<String>,
        options: &mut rustpy_test::TestRunOptions,
    ) {
        let settings = &self.settings.testing;
        if framework == rustpy_test::Framework::Pytest {
            if settings.suppress_buffering
                && !args.iter().any(|arg| arg == "-s" || arg == "--capture=no")
            {
                args.push("-s".into());
            }
            if let Some(path) = &settings.pytest_path {
                let path = if path.is_absolute() {
                    path.clone()
                } else {
                    self.project
                        .as_ref()
                        .map(|project| project.root.join(path))
                        .unwrap_or_else(|| path.clone())
                };
                options
                    .env
                    .push(("RUSTPY_PYTEST_PATH".into(), path.display().to_string()));
            }
        }
        if framework == rustpy_test::Framework::Unittest
            && (targets.is_empty()
                || targets.iter().all(|target| {
                    let path = PathBuf::from(target);
                    if path.is_absolute() {
                        path.is_dir()
                    } else {
                        self.project
                            .as_ref()
                            .is_some_and(|project| project.root.join(path).is_dir())
                    }
                }))
        {
            let pattern = if settings.unittest_pattern == "test*.py" {
                &self.settings.python_testing.unittest_pattern
            } else {
                &settings.unittest_pattern
            };
            if !pattern.trim().is_empty()
                && !args.iter().any(|arg| arg == "-p" || arg == "--pattern")
            {
                args.extend(["-p".into(), pattern.clone()]);
            }
            if let Some(path) = &settings.unittest_top_level {
                if !args
                    .iter()
                    .any(|arg| arg == "-t" || arg == "--top-level-directory")
                {
                    args.extend(["-t".into(), path.display().to_string()]);
                }
            }
        }
    }
    pub(super) fn poll_testing_ui(&mut self, ctx: &egui::Context) {
        let project_safe_mode = self.project_safe_mode();
        self.test_panel.set_execution_allowed(!project_safe_mode);
        if let Some(project) = &self.project {
            self.test_panel.set_project_history(&project.root);
        }
        if project_safe_mode {
            self.test_panel.suspend_automatic_runs();
        } else if let Some((root, interpreter)) = self.project.as_ref().map(|project| {
            (
                project.root.clone(),
                project
                    .interpreter
                    .as_ref()
                    .map(|interpreter| interpreter.path.clone())
                    .unwrap_or_default(),
            )
        }) {
            self.test_panel
                .detect_framework_background(&root, &interpreter);
        }
        if let Some((root, framework)) = self.test_panel.poll_framework_detection() {
            if self.settings.testing.notify_detection
                && self
                    .project
                    .as_ref()
                    .is_some_and(|project| project.root == root)
            {
                self.notify(crate::notifications::NotificationKind::Info, "Testing", "Test framework detected", format!("{} is the detected test framework for {}. Change the default in Python Integrated Tools.", framework.label(), root.display()), Vec::new());
            }
        }
        if let Some(message) = self.test_panel.take_completion_message() {
            self.set_status(message);
        }
        if self.test_panel.wants_repaint() {
            ctx.request_repaint_after(std::time::Duration::from_millis(60));
        }
    }
    pub(super) fn measure_test_run(&mut self, profile: bool, target: Option<String>) {
        let Some(last) = self.test_panel.last_run().cloned() else {
            return;
        };
        let targets = target
            .map(|target| vec![target])
            .unwrap_or_else(|| last.targets.clone());
        let module = if last.framework.label() == "trial" {
            "twisted.trial"
        } else {
            last.framework.label()
        };
        let mut arguments = last.args.clone();
        let kind = if matches!(last.framework.label(), "nose" | "nose2" | "lettuce" | "tox") {
            let script = match rustpy_test::provision_reporter(
                &Settings::system_dir().join("test-reporters"),
            ) {
                Ok(script) => script,
                Err(error) => {
                    self.set_status(format!(
                        "Cannot prepare the test measurement adapter: {error}"
                    ));
                    return;
                }
            };
            let mut adapted = vec![
                "--framework".into(),
                last.framework.label().into(),
                "--root".into(),
                last.root.display().to_string(),
                "--".into(),
            ];
            adapted.extend(arguments);
            if last.framework.label() == "tox" {
                let environments: Vec<_> = targets
                    .iter()
                    .map(|target| {
                        target
                            .strip_prefix("tox::")
                            .unwrap_or(target)
                            .split("::")
                            .next()
                            .unwrap_or(target)
                            .to_string()
                    })
                    .collect();
                if !environments.is_empty() {
                    adapted.extend(["-e".into(), environments.join(",")]);
                }
            } else {
                adapted.extend(targets);
            }
            arguments = adapted;
            RunConfigKind::Python { script }
        } else if module == "pytest" && targets.len() <= 1 {
            RunConfigKind::Pytest {
                target: targets.first().cloned().unwrap_or_default(),
                keywords: String::new(),
            }
        } else if module == "unittest" && targets.len() <= 1 {
            RunConfigKind::Unittest {
                target: targets.first().cloned().unwrap_or_default(),
            }
        } else {
            arguments.extend(targets);
            RunConfigKind::PythonModule {
                module: module.into(),
            }
        };
        let mut config = RunConfig {
            name: format!(
                "{} tests ({})",
                if profile { "Profile" } else { "Coverage" },
                last.framework.label()
            ),
            kind,
            interpreter: Some(last.interpreter),
            working_dir: last.options.cwd.or(Some(last.root)),
            env: last.options.env.into_iter().collect(),
            parameters: crate::run_anything::quote_args(&arguments),
            interpreter_options: crate::run_anything::quote_args(&last.options.interpreter_options),
            temporary: true,
            ..Default::default()
        };
        config.workflow.inherit_environment = last.options.inherit_environment;
        self.launch_config(
            config,
            if profile {
                run_context::RunMode::Profile
            } else {
                run_context::RunMode::Coverage
            },
        );
    }
    pub(super) fn maybe_choose_test_or_main(
        &mut self,
        config: &RunConfig,
        mode: run_context::RunMode,
    ) -> bool {
        let RunConfigKind::Pytest { target, .. } = &config.kind else {
            return false;
        };
        if target.contains("::") {
            return false;
        }
        let Some(project) = &self.project else {
            return false;
        };
        let path = run_context::target_path(&project.root, target);
        let text = self
            .opened_editor_index(&path)
            .map(|index| self.tabs[index].state.doc.text())
            .or_else(|| std::fs::read_to_string(&path).ok())
            .unwrap_or_default();
        // The parsed gutter classifier only emits Script alongside tests for a
        // main guard, so string/comment mentions do not trigger the chooser.
        let targets = run_context::gutter_targets(&project.root, &path, &text);
        if !targets
            .iter()
            .any(|(_, target)| matches!(target, run_context::GutterTarget::Script))
            || !targets
                .iter()
                .any(|(_, target)| matches!(target, run_context::GutterTarget::Test { .. }))
        {
            return false;
        }
        match self.settings.testing.main_conflict.as_str() {
            "tests" => false,
            "script" => {
                self.launch_config(RunConfig::for_script(&path), mode);
                true
            }
            _ => {
                self.testing_ui.pending_main = Some((config.clone(), mode));
                true
            }
        }
    }
    pub(super) fn test_main_choice_dialog(&mut self, ctx: &egui::Context) {
        let Some((config, mode)) = self.testing_ui.pending_main.clone() else {
            return;
        };
        let mut choice = None;
        let mut open = true;
        egui::Window::new("Run file or tests?")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show_modal(ctx, |ui| {
                ui.label("This file contains tests and a __main__ entry point.");
                ui.checkbox(&mut self.testing_ui.remember, "Remember this preference");
                ui.horizontal(|ui| {
                    if ui.button("Run tests").clicked() {
                        choice = Some(true);
                    }
                    if ui.button("Run __main__").clicked() {
                        choice = Some(false);
                    }
                    if ui.button("Cancel").clicked() {
                        self.testing_ui.pending_main = None;
                    }
                });
            });
        if !open {
            self.testing_ui.pending_main = None;
        }
        if let Some(tests) = choice {
            self.testing_ui.pending_main = None;
            if self.testing_ui.remember {
                self.settings.testing.main_conflict = if tests { "tests" } else { "script" }.into();
            }
            if tests {
                self.launch_config(config, mode);
            } else if let (Some(project), RunConfigKind::Pytest { target, .. }) =
                (&self.project, &config.kind)
            {
                let path = run_context::target_path(&project.root, target);
                self.launch_config(RunConfig::for_script(&path), mode);
            }
        }
    }
}
