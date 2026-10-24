//! Python Console execution from the editor (roadmap §2.11): *Execute
//! Selection in Python Console* (Alt+Shift+E), *Run File in Python Console*,
//! `#%%` code cells (*Run Cell*, ▶ on marker lines) and run configurations
//! with "Run with Python Console".

use super::*;
use crate::editor::console_code;

impl IdeApp {
    pub(super) fn python_console_tabs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            for (index, name) in self.py_console.names().iter().enumerate() {
                if ui
                    .selectable_label(index == self.py_console.active(), name)
                    .clicked()
                {
                    self.py_console.select(index);
                }
            }
            if ui.button("+ Console").clicked() {
                self.py_console.add("Console");
            }
            if ui.button("Django shell").clicked() {
                self.framework_shell(true);
            }
            if ui.button("Flask shell").clicked() {
                self.framework_shell(false);
            }
            if ui.button("Run above").clicked() {
                if let Some(index) = self.active.filter(|&i| i < self.tabs.len()) {
                    let text = self.tabs[index].state.doc.text();
                    let line = self.tabs[index]
                        .state
                        .doc
                        .line_of(self.tabs[index].state.primary().head());
                    if let Some(cell) = console_code::cell_at(&text, line) {
                        let code = text
                            .lines()
                            .take(cell.marker.unwrap_or(0))
                            .collect::<Vec<_>>()
                            .join("\n");
                        if !code.trim().is_empty() && self.console_for_editor_code() {
                            self.py_console.execute_fragment(&code);
                        }
                    }
                }
            }
            if ui.button("Close console").clicked() {
                self.py_console.close_active();
            }
        });
    }
    pub(super) fn consume_python_console_requests(&mut self) {
        if self.project_safe_mode() {
            self.py_console.debugger_endpoint = None;
            self.py_console.scientific_debug_code = None;
            return;
        }
        if let Some(transcript) = self.py_console.print_requested.take() {
            let directory = rustpy_settings::Settings::config_dir().join("console-transcripts");
            let path = directory.join(format!(
                "console-{}.txt",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
            match std::fs::create_dir_all(&directory)
                .and_then(|()| std::fs::write(&path, transcript))
            {
                Ok(()) => {
                    self.open_file(&path);
                    self.set_status(format!("Printable console transcript: {}", path.display()));
                }
                Err(error) => self.set_status(format!("Cannot export console: {error}")),
            }
        }
        if self.py_console.settings_requested {
            self.py_console.settings_requested = false;
            self.settings_ui.page = settings_ui::SettingsPage::PythonConsole;
            self.dialog = Dialog::Settings;
        }
        if let Some((path, line)) = self.py_console.navigate_request.take() {
            self.open_file(&path);
            if let Some(tab) = self.active.and_then(|i| self.tabs.get_mut(i)) {
                tab.state.go_to_line(line, None);
            }
        }
        if let Some((host, port)) = self.py_console.debugger_endpoint.take() {
            let setup = self.debug_launch_setup();
            match rustpy_debug::DebugSession::attach_endpoint(
                &host,
                port,
                setup.options,
                setup.breakpoints,
            ) {
                Ok(mut session) => {
                    session.id = self.next_session_id;
                    self.next_session_id += 1;
                    self.retain_debug_session();
                    self.debug = Some(session);
                    self.bottom = Some(BottomTool::Debug);
                }
                Err(error) => {
                    self.py_console.scientific_debug_code = None;
                    self.set_status(format!("Console debugger: {error}"));
                }
            }
        }
    }

    /// Starts the Python Console unless it is running: the project interpreter
    /// (or `python3`) with its virtualenv, PYTHONPATH holding the content and
    /// source roots. Returns whether the console runs; never starts in safe mode.
    pub(super) fn ensure_py_console(&mut self) -> bool {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode {
            return false;
        }
        if self.py_console.is_running() {
            return true;
        }
        let project_root = self
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let prefs = self.settings.python_console.clone();
        let cwd = prefs
            .working_directory
            .clone()
            .unwrap_or_else(|| project_root.clone());
        let interpreter = self.project.as_ref().and_then(|p| p.interpreter.as_ref());
        let python = interpreter
            .map(|i| i.path.clone())
            .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "python" } else { "python3" }));
        let mut env = crate::terminal_panel::venv_env(interpreter);
        if let Some(project) = &self.project {
            let facet = project.tree.model().frameworks;
            if facet.enabled(&project.root) {
                env.extend(facet.environment(&project.root));
            }
        }
        let pythonpath = self
            .pythonpath_for(&RunConfig {
                add_content_roots: prefs.add_content_roots,
                add_source_roots: prefs.add_source_roots,
                ..RunConfig::default()
            })
            .unwrap_or_else(|| project_root.to_string_lossy().into_owned());
        if prefs.add_content_roots || prefs.add_source_roots {
            env.push(("PYTHONPATH".into(), pythonpath));
        }
        env.extend(
            prefs
                .environment
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        self.py_console.configure_scientific_capture(
            self.settings.scientific.show_plots,
            self.settings.scientific.capture_interactive,
        );
        self.py_console.configure(&prefs);
        self.py_console.start(&python, &cwd, &env, &project_root);
        self.py_console.is_running()
    }

    /// Shows the Python Console for code sent from the editor, starting it when
    /// needed; false (with a status message) when it cannot run.
    fn console_for_editor_code(&mut self) -> bool {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode {
            self.set_status("Safe mode: this project is not trusted yet (File → Trust Project to allow running code)");
            return false;
        }
        if !self.settings.python_console.use_existing_console {
            self.py_console.add("Console");
        }
        let running = self.ensure_py_console();
        self.bottom = Some(BottomTool::PythonConsole);
        if !running {
            let why = self
                .py_console
                .start_error()
                .unwrap_or("the interpreter did not start")
                .to_string();
            self.set_status(format!("Python Console: {why}"));
        }
        running
    }

    /// *Execute Selection in Python Console* (Alt+Shift+E): the selection, or
    /// the statement on the caret line, after which the caret moves below it so
    /// repeated presses step through the file. The editor keeps the focus.
    pub(super) fn execute_selection_in_console(&mut self) {
        let Some(i) = self.active.filter(|&i| i < self.tabs.len()) else {
            self.set_status("Open a file to execute code in the Python Console");
            return;
        };
        let fragment = console_code::fragment_at_caret(&mut self.tabs[i].state);
        if !fragment.code.is_empty() {
            if !self.console_for_editor_code() {
                return;
            }
            if self.settings.python_console.execute_by_lines {
                for line in fragment.code.lines() {
                    self.py_console.execute(line);
                }
            } else {
                self.py_console.execute_fragment(&fragment.code);
            }
        }
        if let Some(pos) = fragment.next_caret {
            self.tabs[i].view.cancel_session_restore();
            self.tabs[i].state.move_primary_to(pos);
        }
        self.focus_editor = true;
    }

    /// *Run File in Python Console*: saves, then runs the active Python file
    /// with `runfile` in its directory; its variables stay in the console.
    pub(super) fn run_file_in_console(&mut self) {
        let path = self
            .active
            .and_then(|i| self.tabs.get(i))
            .and_then(|t| t.state.path.clone())
            .filter(|p| {
                !p.starts_with("<typeshed>")
                    && p.extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| e == "py" || e == "pyw")
            });
        let Some(path) = path else {
            self.set_status("Run File in Python Console works on Python files");
            return;
        };
        if !self.save_all() {
            return;
        }
        if !self.console_for_editor_code() {
            return;
        }
        let wdir = path.parent().map(Path::to_path_buf);
        self.py_console.run_file(&path, &[], wdir.as_deref(), &[]);
    }

    /// *Run Cell* / *Run Cell and Select Below* for the `#%%` cell at the caret.
    pub(super) fn run_cell_at_caret(&mut self, advance: bool) {
        let Some(i) = self.active.filter(|&i| i < self.tabs.len()) else {
            return;
        };
        let line = self.tabs[i]
            .state
            .doc
            .line_of(self.tabs[i].state.primary().head());
        self.run_cell(i, line, advance);
    }

    /// Runs the `#%%` cell of tab `i` that contains `line` (also the ▶ on a
    /// marker line); `advance` moves the caret into the next cell.
    pub(super) fn run_cell(&mut self, i: usize, line: usize, advance: bool) {
        if self.tabs[i].state.language != Language::Python {
            self.set_status("Code cells (#%%) are available in Python files");
            return;
        }
        let text = self.tabs[i].state.doc.text();
        let Some(cell) = console_code::cell_at(&text, line) else {
            self.set_status("No #%% code cells in this file");
            return;
        };
        let code = console_code::cell_code(&text, &cell);
        if code.is_empty() {
            self.set_status("The cell is empty");
        } else {
            if !self.console_for_editor_code() {
                return;
            }
            self.py_console.execute_fragment(&code);
        }
        if advance {
            if let Some(next) = cell.next {
                let state = &mut self.tabs[i].state;
                let target = (next + 1).min(state.doc.line_count().saturating_sub(1));
                state.move_primary_to(rustpy_core::Position::new(target as u32, 0));
            }
        }
        self.focus_editor = true;
    }

    /// A Python-script configuration with "Run with Python Console" runs with
    /// `runfile` in the console (parameters, working directory and environment
    /// applied) instead of in a Run tab. False for any other configuration.
    /// Context configurations reuse the script's existing configuration (see
    /// `RunConfigStore::resolve_temporary`); the launcher stores `cfg` before
    /// its macros are expanded, so it is not saved here.
    pub(super) fn run_config_in_console(&mut self, cfg: &RunConfig) -> bool {
        let project_safe_mode = self.project_safe_mode();
        if project_safe_mode {
            self.set_status("Trust the project before running a Python Console configuration");
            return true;
        }
        let RunConfigKind::Python { script } = &cfg.kind else {
            return false;
        };
        if !cfg.run_with_console {
            return false;
        }
        if !self.save_all() {
            return false;
        }
        let root = self
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .unwrap_or_default();
        let wdir = cfg.working_dir.clone().unwrap_or(root);
        let interpreter = cfg
            .interpreter
            .clone()
            .or_else(|| {
                self.project.as_ref().and_then(|project| {
                    project
                        .interpreter
                        .as_ref()
                        .map(|interpreter| interpreter.path.clone())
                })
            })
            .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "python" } else { "python3" }));
        let options = RunConfig::split_args(&cfg.interpreter_options);
        if self.py_console.is_running()
            && (!self
                .py_console
                .configuration_matches(cfg, &interpreter, &options)
                || cfg.workflow.allow_parallel
                || self.py_console.execution_pending()
                || !self.settings.python_console.use_existing_console)
        {
            self.py_console.add(&cfg.name);
        }
        if !self.py_console.is_running() {
            if project_safe_mode {
                self.set_status("Trust the project before running a Python Console configuration");
                return true;
            }
            let mut preferences = self.settings.python_console.clone();
            preferences.add_content_roots = cfg.add_content_roots;
            preferences.add_source_roots = cfg.add_source_roots;
            self.py_console.configure_scientific_capture(
                self.settings.scientific.show_plots,
                self.settings.scientific.capture_interactive,
            );
            self.py_console.configure(&preferences);
            let environment = cfg
                .env
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<Vec<_>>();
            let project_root = self
                .project
                .as_ref()
                .map(|project| project.root.clone())
                .unwrap_or_default();
            self.py_console.start_with_launch_options(
                &interpreter,
                &wdir,
                &environment,
                &project_root,
                cfg.workflow.inherit_environment,
                &options,
            );
        }
        self.bottom = Some(BottomTool::PythonConsole);
        let script = if script.is_absolute() {
            script.clone()
        } else {
            wdir.join(script)
        };
        let args = RunConfig::split_args(&cfg.parameters);
        let env: Vec<(String, String)> = cfg
            .env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        self.py_console.mark_run_configuration(&cfg.name);
        self.py_console.run_file_with_environment(
            &script,
            &args,
            Some(&wdir),
            &env,
            cfg.workflow.inherit_environment,
        );
        true
    }
}
