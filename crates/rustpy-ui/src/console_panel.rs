//! Python Console tool window backed by `rustpy-console`: prompt with history and
//! continuation, streamed output, variables pane with lazy expansion, completion.

use crate::a11y::DialogWindow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use egui::{Color32, Sense, Vec2};
use rustpy_console::{Completion, ConsoleEvent, ConsoleSession, History, VarInfo};

use crate::a11y::Accessible;
use crate::theme::UiTheme;

#[derive(Clone)]
enum Line {
    Input(String),
    Stdout(String),
    Stderr(String),
    Result(String),
    System(String),
}

pub struct PythonConsole {
    session: Option<ConsoleSession>,
    successful_code: VecDeque<String>,
    pending_source: HashMap<u64, String>,
    semantic_key: Option<(String, u64, usize)>,
    semantic_diagnostics: Vec<rustpy_core::Diagnostic>,
    pub plots_requested: bool,
    scientific_plot_events: Vec<(String, String)>,
    scientific_capture: bool,
    scientific_interactive: bool,
    preferences: rustpy_settings::python_console::PythonConsoleSettings,
    output_preferences: rustpy_settings::run_console::Settings,
    pub settings_requested: bool,
    pub print_requested: Option<String>,
    pub navigate_request: Option<(PathBuf, usize)>,
    pub debugger_endpoint: Option<(String, u16)>,
    pub scientific_debug_code: Option<String>,
    history_popup: bool,
    show_queue: bool,
    scroll_end: bool,
    edit_variable: Option<(String, String)>,
    plots: Vec<(String, String)>,
    process_epoch: u64,
    variables_epoch: u64,
    tree_focus: Option<(egui::Id, u64)>,
    accessibility_owner: bool,
    lines: Vec<Line>,
    pub input: String,
    buffer: String,
    continuation: bool,
    history: History,
    vars: Vec<VarInfo>,
    children: HashMap<String, Vec<VarInfo>>,
    expanded: HashSet<String>,
    /// The latest request whose answer is awaited.
    pending_exec: Option<u64>,
    configuration_name: Option<String>,
    /// Typed input awaiting its answer, oldest first: request id and the code
    /// sent. Only these answers move the prompt (`...`) and add history entries.
    typed: VecDeque<(u64, String)>,
    completions: Vec<Completion>,
    completion_selected: usize,
    completion_visible: bool,
    pub show_variables: bool,
    /// Lists interpreter/IPython-injected names (`In`, `Out`, `_ih`, …) too.
    pub show_special_variables: bool,
    pub focused: bool,
    pub want_focus: bool,
    ready: Option<String>,
    error: Option<String>,
    history_dir: Option<PathBuf>,
    /// A start was attempted: the tool window starts the console only the first
    /// time it is shown, so one that was stopped, exited or could not start
    /// stays that way (no restart on every frame) until Rerun or editor code.
    start_attempted: bool,
    /// Rerun (⟳) was pressed while no console session exists.
    start_requested: bool,
    /// Captured table JSON from "View as table" (consumed by the app).
    pub table_json: Option<(String, String)>,
    pub table_request: Option<String>,
}

impl Default for PythonConsole {
    fn default() -> Self {
        Self {
            session: None,
            preferences: Default::default(),
            output_preferences: Default::default(),
            successful_code: VecDeque::new(),
            pending_source: HashMap::new(),
            semantic_key: None,
            semantic_diagnostics: Vec::new(),
            plots_requested: false,
            scientific_plot_events: Vec::new(),
            scientific_capture: true,
            scientific_interactive: true,
            settings_requested: false,
            print_requested: None,
            navigate_request: None,
            debugger_endpoint: None,
            scientific_debug_code: None,
            history_popup: false,
            show_queue: false,
            scroll_end: true,
            edit_variable: None,
            plots: Vec::new(),
            process_epoch: 0,
            variables_epoch: 0,
            tree_focus: None,
            accessibility_owner: true,
            lines: Vec::new(),
            input: String::new(),
            buffer: String::new(),
            continuation: false,
            history: History::in_memory(),
            vars: Vec::new(),
            children: HashMap::new(),
            expanded: HashSet::new(),
            pending_exec: None,
            configuration_name: None,
            typed: VecDeque::new(),
            completions: Vec::new(),
            completion_selected: 0,
            completion_visible: false,
            show_variables: true,
            show_special_variables: false,
            focused: false,
            want_focus: false,
            ready: None,
            error: None,
            history_dir: None,
            start_attempted: false,
            start_requested: false,
            table_json: None,
            table_request: None,
        }
    }
}

impl PythonConsole {
    pub(crate) fn service_id(&self) -> Option<u64> {
        (self.process_epoch != 0).then_some(self.process_epoch)
    }
    pub fn configure(
        &mut self,
        preferences: &rustpy_settings::python_console::PythonConsoleSettings,
    ) {
        self.preferences = preferences.clone();
        self.history.set_limit(preferences.history_limit);
        self.show_variables = preferences.show_variables;
    }

    pub(crate) fn configure_output(
        &mut self,
        preferences: &rustpy_settings::run_console::Settings,
    ) {
        if self.output_preferences != *preferences {
            self.output_preferences = preferences.clone();
            self.trim_output_preferences();
        }
    }
    fn trim_output_preferences(&mut self) {
        let maximum = self.output_preferences.buffer_kib.clamp(1, 262144) * 1024;
        let mut bytes = 0usize;
        let mut keep = self.lines.len();
        for (index, line) in self.lines.iter().enumerate().rev() {
            let text = match line {
                Line::Input(t)
                | Line::Stdout(t)
                | Line::Stderr(t)
                | Line::Result(t)
                | Line::System(t) => t,
            };
            if bytes.saturating_add(text.len()) > maximum {
                if keep == self.lines.len() {
                    keep = index;
                }
                break;
            }
            bytes += text.len();
            keep = index;
        }
        if keep > 0 {
            self.lines.drain(..keep);
        }
        if let Some(line) = self.lines.first_mut() {
            let text = match line {
                Line::Input(t)
                | Line::Stdout(t)
                | Line::Stderr(t)
                | Line::Result(t)
                | Line::System(t) => t,
            };
            if text.len() > maximum {
                let mut start = text.len() - maximum;
                while !text.is_char_boundary(start) {
                    start += 1;
                }
                text.drain(..start);
            }
        }
    }

    pub(crate) fn set_accessibility_owner(&mut self, available: bool) {
        self.accessibility_owner = available;
    }

    #[cfg(test)]
    pub(crate) fn testing_variable_state(&self, expr: &str) -> (bool, bool) {
        (
            self.expanded.contains(expr),
            self.children.contains_key(expr),
        )
    }

    pub(crate) fn claims_tree_menu(
        &self,
        ctx: &egui::Context,
        key: egui::Key,
        mods: egui::Modifiers,
    ) -> bool {
        key == egui::Key::F10
            && mods == egui::Modifiers::SHIFT
            && self.is_running()
            && self.show_variables
            && !egui::Popup::is_any_open(ctx)
            && self.tree_focus.is_some_and(|(id, epoch)| {
                epoch == self.variables_epoch && ctx.memory(|memory| memory.has_focus(id))
            })
    }

    fn retire_variable_focus(&mut self) {
        self.variables_epoch = crate::app::window_lifecycle::next_process_owner();
        self.tree_focus = None;
    }

    /// Observational identity of the retained process owner; does not poll it.
    pub(crate) fn quit_process_state(&self) -> String {
        format!(
            "{:?}",
            (
                self.process_epoch,
                self.session.as_ref().map(|owner| owner as *const _)
            )
        )
    }
    pub(crate) fn stop_quit_process(&mut self) {
        self.stop();
    }

    pub fn is_running(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.is_running())
    }

    pub fn request_focus(&mut self) {
        self.want_focus = true;
    }

    /// Whether the tool window should start the console now: the first time
    /// it is shown, or after Rerun (⟳) while no session exists.
    pub fn take_start_request(&mut self) -> bool {
        let wanted = (!self.start_attempted || self.start_requested) && !self.is_running();
        self.start_requested = false;
        wanted
    }

    /// Rerun (⟳): restarts the session, or asks the app to start a new one.
    fn rerun(&mut self) {
        self.retire_variable_focus();
        match &mut self.session {
            Some(s) => {
                s.restart();
                self.lines.push(Line::System("Console restarted".into()));
                self.vars.clear();
                self.children.clear();
                self.continuation = false;
                self.buffer.clear();
                self.forget_requests();
            }
            None => self.start_requested = true,
        }
    }

    /// Starts (or restarts) the session with the given interpreter.
    pub fn start(
        &mut self,
        interpreter: &Path,
        cwd: &Path,
        env: &[(String, String)],
        project_root: &Path,
    ) {
        self.start_with_launch_options(interpreter, cwd, env, project_root, true, &[]);
    }
    pub fn start_with_launch_options(
        &mut self,
        interpreter: &Path,
        cwd: &Path,
        env: &[(String, String)],
        project_root: &Path,
        inherit_environment: bool,
        interpreter_options: &[String],
    ) {
        self.retire_variable_focus();
        self.start_attempted = true;
        self.start_requested = false;
        if let Some(s) = &mut self.session {
            s.stop();
        }
        let startup = if self.preferences.add_content_roots {
            format!(
                "import sys\nsys.path.insert(0, {})\n{}",
                serde_json::to_string(&project_root.to_string_lossy()).unwrap_or_default(),
                self.preferences.startup_script
            )
        } else {
            self.preferences.startup_script.clone()
        };
        self.lines.clear();
        self.successful_code.clear();
        self.pending_source.clear();
        self.semantic_key = None;
        self.plots.clear();
        self.vars.clear();
        self.children.clear();
        self.ready = None;
        self.error = None;
        self.continuation = false;
        self.buffer.clear();
        self.forget_requests();
        let hist_dir = project_root.join(".rustpy");
        let _ = std::fs::create_dir_all(&hist_dir);
        self.history = History::load(&hist_dir);
        self.history.set_limit(self.preferences.history_limit);
        self.history_dir = Some(hist_dir);
        let mut scientific_environment = env.to_vec();
        scientific_environment.push((
            "RUSTPY_SCIENTIFIC_CAPTURE".into(),
            (if self.scientific_capture { "1" } else { "0" }).into(),
        ));
        scientific_environment.push((
            "RUSTPY_SCIENTIFIC_INTERACTIVE".into(),
            (if self.scientific_interactive {
                "1"
            } else {
                "0"
            })
            .into(),
        ));
        match ConsoleSession::start_with_launch_options(
            interpreter,
            cwd,
            &scientific_environment,
            &startup,
            inherit_environment,
            interpreter_options,
        ) {
            Ok(mut s) => {
                s.use_ipython(self.preferences.use_ipython);
                s.set_scientific_capture(self.scientific_capture, self.scientific_interactive);
                self.lines.push(Line::System(format!(
                    "Python console: {}",
                    interpreter.display()
                )));
                self.process_epoch = crate::app::window_lifecycle::next_process_owner();
                self.session = Some(s);
            }
            Err(e) => self.error = Some(format!("Cannot start the console: {e}")),
        }
    }

    pub fn stop(&mut self) {
        self.retire_variable_focus();
        if let Some(s) = &mut self.session {
            s.stop();
        }
        self.session = None;
        self.continuation = false;
        self.buffer.clear();
        self.forget_requests();
    }

    /// Why the last start failed, if it did.
    pub(crate) fn execution_pending(&self) -> bool {
        self.pending_exec.is_some()
    }
    pub(crate) fn mark_run_configuration(&mut self, name: &str) {
        self.configuration_name = Some(name.into());
    }
    pub(crate) fn pending_configuration_name(&self) -> Option<&str> {
        self.pending_exec.and(self.configuration_name.as_deref())
    }
    pub(crate) fn running_configuration(&self, name: &str) -> bool {
        self.configuration_name.as_deref() == Some(name) && self.pending_exec.is_some()
    }

    pub fn start_error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Runs `code` sent from the editor (*Execute Selection*, *Run Cell*): it is
    /// echoed like typed input, recorded in the history and executed as one
    /// fragment, so a block ending with `def`/`for` runs without waiting for a
    /// blank line. An unfinished statement typed at the prompt is abandoned.
    /// Returns false when the console is not running.
    pub fn execute_fragment(&mut self, code: &str) -> bool {
        if self.session.is_none() {
            return false;
        }
        self.abandon_unfinished_input();
        let Some(s) = &mut self.session else {
            return false;
        };
        self.lines.push(Line::Input(code.trim_end().to_string()));
        self.history.push(code);
        let id = s.exec_fragment(code);
        self.pending_source.insert(id, code.to_string());
        self.pending_exec = Some(id);
        true
    }

    /// *Run File in Python Console*: runs the script in the console namespace
    /// and echoes the equivalent `runfile(…)` call (kept in the history).
    pub fn run_file(
        &mut self,
        path: &Path,
        args: &[String],
        wdir: Option<&Path>,
        env: &[(String, String)],
    ) -> bool {
        self.run_file_with_environment(path, args, wdir, env, true)
    }
    pub fn launch_matches(&self, interpreter: &Path, options: &[String], inherit: bool) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.launch_matches(interpreter, options, inherit))
    }
    pub fn configuration_matches(
        &self,
        cfg: &rustpy_project::RunConfig,
        interpreter: &Path,
        options: &[String],
    ) -> bool {
        self.launch_matches(interpreter, options, cfg.workflow.inherit_environment)
            && self.preferences.add_content_roots == cfg.add_content_roots
            && self.preferences.add_source_roots == cfg.add_source_roots
    }
    pub fn run_file_with_environment(
        &mut self,
        path: &Path,
        args: &[String],
        wdir: Option<&Path>,
        env: &[(String, String)],
        inherit_environment: bool,
    ) -> bool {
        if self.session.is_none() {
            return false;
        }
        self.abandon_unfinished_input();
        let Some(s) = &mut self.session else {
            return false;
        };
        let wdir_text = wdir.map(|w| w.to_string_lossy().into_owned());
        let command = crate::editor::console_code::runfile_command(
            &path.to_string_lossy(),
            args,
            wdir_text.as_deref(),
        );
        self.lines.push(Line::Input(command.clone()));
        self.history.push(&command);
        self.pending_exec =
            Some(s.run_file_with_environment(path, args, wdir, env, inherit_environment));
        true
    }

    /// Executes `code` as if typed (used by smoke tests and "Execute selection in console").
    pub fn execute(&mut self, code: &str) {
        let Some(s) = &mut self.session else { return };
        let code = code.to_string();
        self.lines.push(Line::Input(code.clone()));
        let full = if self.continuation {
            format!("{}{}", self.buffer, code)
        } else {
            code.clone()
        };
        let id = s.exec(&full);
        self.pending_source.insert(id, full.clone());
        self.pending_exec = Some(id);
        self.typed.push_back((id, full.clone()));
        self.buffer = full;
        self.buffer.push('\n');
    }

    /// Asks the interpreter for a JSON table of `expr` (DataFrame, ndarray, list of rows/dicts, dict).
    pub fn scientific_debug(&mut self, code: String) -> bool {
        if !self.pending_source.is_empty() || self.scientific_debug_code.is_some() {
            return false;
        }
        if let Some(session) = &mut self.session {
            self.scientific_debug_code = Some(code);
            session.request_debugger();
            true
        } else {
            false
        }
    }
    pub fn configure_scientific_capture(&mut self, enabled: bool, interactive: bool) {
        if self.scientific_capture != enabled || self.scientific_interactive != interactive {
            self.scientific_capture = enabled;
            self.scientific_interactive = interactive;
            if let Some(session) = &mut self.session {
                session.set_scientific_capture(enabled, interactive);
            }
        }
    }
    pub fn take_scientific_plots(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.scientific_plot_events)
    }
    pub fn scientific_refresh(&mut self, name: &str, options: serde_json::Value) -> bool {
        if !self.pending_source.is_empty() {
            return false;
        }
        if let Some(session) = &mut self.session {
            session.request_scientific_data(name, options);
            true
        } else {
            false
        }
    }
    pub fn request_table(&mut self, expr: &str) {
        self.plots_requested = true;
        if let Some(session) = &mut self.session {
            session.request_data(expr, 0, 1000, 0, 100);
        }
    }

    /// Drains session events; returns true when something changed.
    pub fn poll(&mut self) -> bool {
        let Some(s) = &mut self.session else {
            return false;
        };
        let events = s.poll();
        if events.is_empty() {
            return false;
        }
        let mut vars_needed = false;
        for ev in events {
            match ev {
                ConsoleEvent::Debugger {
                    host, port, error, ..
                } => {
                    if let Some(error) = error {
                        self.scientific_debug_code = None;
                        self.error = Some(error);
                    } else {
                        self.debugger_endpoint = Some((host, port));
                    }
                }
                ConsoleEvent::Configured { error, .. } | ConsoleEvent::ValueSet { error, .. } => {
                    if let Some(error) = error {
                        self.lines.push(Line::Stderr(error));
                    }
                    vars_needed = true;
                }
                ConsoleEvent::Plot {
                    mime, data_base64, ..
                } => {
                    self.scientific_plot_events
                        .push((mime.clone(), data_base64.clone()));
                    while self.scientific_plot_events.len() > 64
                        || self
                            .scientific_plot_events
                            .iter()
                            .map(|(_, data)| data.len())
                            .sum::<usize>()
                            > 32 * 1024 * 1024
                    {
                        self.scientific_plot_events.remove(0);
                    }
                    self.plots.push((mime, data_base64));
                    while self.plots.len() > 8
                        || self.plots.iter().map(|(_, data)| data.len()).sum::<usize>()
                            > 32 * 1024 * 1024
                    {
                        self.plots.remove(0);
                    }
                    self.plots_requested = true;
                }
                ConsoleEvent::Page {
                    name, items, error, ..
                } => {
                    if let Some(error) = error {
                        self.lines.push(Line::Stderr(error));
                    } else {
                        self.children.insert(name, items);
                    }
                }
                ConsoleEvent::Data { table, error, .. } => {
                    if let Some(error) = error {
                        self.lines.push(Line::Stderr(error));
                    }
                    if let Some(table) = table {
                        self.table_json = Some((
                            table.expression.clone(),
                            serde_json::to_string(&table).unwrap_or_default(),
                        ));
                    }
                }
                ConsoleEvent::Stdout(t) if t.contains("##rustpy-table##") => {
                    if let Some(json) = t.split("##rustpy-table##").nth(1) {
                        let name = self.table_request.take().unwrap_or_default();
                        self.table_json = Some((name, json.trim().to_string()));
                    }
                }
                ConsoleEvent::Stdout(t) => self.lines.push(Line::Stdout(t)),
                ConsoleEvent::Stderr(t) => self.lines.push(Line::Stderr(t)),
                ConsoleEvent::Result {
                    id,
                    value,
                    error,
                    more,
                } => {
                    if let Some(source) = self.pending_source.remove(&id) {
                        if !more && error.is_none() {
                            self.successful_code.push_back(source);
                            while self.successful_code.iter().map(String::len).sum::<usize>()
                                > 32_768
                            {
                                self.successful_code.pop_front();
                            }
                            self.semantic_key = None;
                        }
                    }
                    // Code from the editor and internal requests (tables) leave
                    // the input typed at the prompt alone.
                    if let Some(pos) = self.typed.iter().position(|(t, _)| *t == id) {
                        let (_, code) = self.typed.remove(pos).unwrap_or_default();
                        let entry = code.trim_end();
                        if !more && !entry.is_empty() {
                            self.history.push(entry);
                        }
                        // The latest typed input decides the prompt.
                        if self.typed.is_empty() {
                            self.continuation = more;
                            self.buffer = if more {
                                format!("{code}\n")
                            } else {
                                String::new()
                            };
                        }
                    }
                    if !more {
                        vars_needed = true;
                    }
                    if let Some(v) = value {
                        self.lines.push(Line::Result(v));
                    }
                    let _ = error;
                    if self.pending_exec == Some(id) {
                        self.pending_exec = None;
                    }
                }
                ConsoleEvent::Completions { items, prefix, .. } => {
                    self.completions = applicable_completions(&self.input, &prefix, items);
                    self.completion_selected = 0;
                    self.completion_visible = !self.completions.is_empty();
                }
                ConsoleEvent::Vars { items, .. } => {
                    self.variables_epoch = crate::app::window_lifecycle::next_process_owner();
                    self.tree_focus = None;
                    self.vars = items;
                    self.semantic_key = None;
                }
                ConsoleEvent::Children { name, items, .. } => {
                    self.children.insert(name, items);
                }
                ConsoleEvent::Ready {
                    version,
                    executable,
                } => self.ready = Some(format!("Python {version} ({executable})")),
                ConsoleEvent::Exited(code) => {
                    self.scientific_debug_code = None;
                    self.debugger_endpoint = None;
                    self.lines.push(Line::System(format!(
                        "Process finished with exit code {code}"
                    )));
                    // No answers will come.
                    self.pending_exec = None;
                    self.typed.clear();
                }
                ConsoleEvent::Failed(e) => {
                    self.scientific_debug_code = None;
                    self.debugger_endpoint = None;
                    self.error = Some(e);
                    self.pending_exec = None;
                    self.typed.clear();
                }
            }
        }
        if vars_needed {
            s.request_vars();
        }
        self.trim_output_preferences();
        true
    }

    /// Abandons a statement left unfinished at the prompt (`...`) when code from
    /// the editor runs; typed input still waiting for its answer is left to it.
    fn abandon_unfinished_input(&mut self) {
        if self.typed.is_empty() {
            self.continuation = false;
            self.buffer.clear();
        }
    }

    /// No answers will come (new or dead process).
    fn forget_requests(&mut self) {
        self.scientific_debug_code = None;
        self.debugger_endpoint = None;
        self.scientific_plot_events.clear();
        self.pending_exec = None;
        self.pending_source.clear();
        self.typed.clear();
    }

    fn submit(&mut self) {
        if !self.preferences.command_queue && self.pending_exec.is_some() {
            return;
        }
        let code = std::mem::take(&mut self.input);
        self.history.reset_cursor();
        self.completion_visible = false;
        if self.preferences.interactive {
            self.execute(&code);
        } else {
            self.execute_fragment(&code);
        }
    }

    fn accept_completion(&mut self) {
        if let Some(c) = self.completions.get(self.completion_selected) {
            // The name being typed is replaced: it may have grown since the list came.
            let start = name_start(&self.input);
            self.input.truncate(start);
            self.input.push_str(&c.name);
        }
        self.completion_visible = false;
    }

    /// Isolated bounded overlay: runtime-only globals remain Any, then successful
    /// statements refine names before the current input is inspected.
    pub fn update_semantic_inspections(
        &mut self,
        project: &rustpy_semantic::Analysis,
        root: &Path,
        open_files: &[(PathBuf, String)],
    ) {
        let path = root.join("__rustpy_console_inspection__.py");
        let revision = project.revision_excluding(&path);
        let key = (self.input.clone(), revision, self.vars.len());
        if self.semantic_key.as_ref() == Some(&key) {
            return;
        }
        self.semantic_key = Some(key);
        self.semantic_diagnostics.clear();
        if self.input.trim().is_empty() || self.input.len() > 16_384 {
            return;
        }
        let resolver = &project.resolver;
        let overlay = rustpy_semantic::Analysis::new(
            rustpy_semantic::module::ModuleResolver::new(
                resolver.roots.clone(),
                resolver.search_paths.clone(),
                resolver.python_version,
            )
            .with_stub_roots(resolver.stub_roots.clone())
            .with_platform(&resolver.platform)
            .with_excluded(resolver.excluded_paths().iter().cloned()),
        );
        overlay.set_cache_limits(rustpy_semantic::db::CacheLimits {
            max_files: 64,
            max_bytes: 2_097_152,
        });
        let mut budget = 524_288usize;
        for (path, text) in open_files.iter().take(16) {
            if text.len() <= budget {
                overlay.set_file_text(path, text);
                budget -= text.len();
            }
        }
        // Runtime-only values have no known static initializer. In particular,
        // assigning None here would narrow an opaque runtime value to None.
        // Keep the synthetic imports private even when the user chose a similar
        // name in the console or in a successful statement.
        let mut slot = 0usize;
        let (any, cast) = loop {
            let any = format!("__rustpy_console_Any_{slot}");
            let cast = format!("__rustpy_console_cast_{slot}");
            let occupied = |name: &str| {
                self.input.contains(name)
                    || self.successful_code.iter().any(|code| code.contains(name))
                    || self.vars.iter().any(|variable| variable.name == name)
            };
            if !occupied(&any) && !occupied(&cast) {
                break (any, cast);
            }
            slot += 1;
        };
        let mut prefix = format!("from typing import Any as {any}, cast as {cast}\n");
        for variable in self.vars.iter().take(1000) {
            if rustpy_semantic::module::is_identifier(&variable.name) {
                prefix.push_str(&format!("{} = {cast}({any}, None)\n", variable.name));
            }
        }
        for statement in &self.successful_code {
            prefix.push_str(statement);
            prefix.push('\n');
        }
        let offset = prefix.len();
        prefix.push_str(&self.input);
        let id = overlay.set_file_text(&path, &prefix);
        let Some(file) = overlay.file(id) else { return };
        let options = rustpy_analysis::inspections::InspectionOptions {
            unused_imports: false,
            unused_variables: false,
            pep8: false,
            ..Default::default()
        };
        self.semantic_diagnostics =
            rustpy_analysis::inspections::inspect(&overlay, &file, &options)
                .into_iter()
                .map(|problem| problem.diagnostic)
                .filter_map(|mut diagnostic| {
                    let start = usize::from(diagnostic.range.start());
                    let end = usize::from(diagnostic.range.end());
                    if start < offset {
                        return None;
                    }
                    diagnostic.range = rustpy_core::TextRange::new(
                        rustpy_core::TextSize::new((start - offset) as u32),
                        rustpy_core::TextSize::new((end - offset) as u32),
                    );
                    Some(diagnostic)
                })
                .take(64)
                .collect();
    }

    /// Top-level variables shown in the Variables pane; interpreter/IPython
    /// internals stay hidden unless "Show special variables" is on.
    pub(crate) fn visible_variables(&self) -> Vec<VarInfo> {
        self.vars
            .iter()
            .filter(|variable| self.show_special_variables || !variable.is_special())
            .cloned()
            .collect()
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, theme: &UiTheme, mut mono: egui::FontId) {
        mono.family = crate::fonts::console_family(false, false);
        mono.size = self.output_preferences.font_size.clamp(8.0, 40.0);
        let running = self.is_running();
        ui.horizontal(|ui| {
            if ui
                .button("⟳")
                .icon_hint("Rerun console (restart the interpreter)")
                .clicked()
            {
                self.rerun();
            }
            if ui
                .add_enabled(running, egui::Button::new("Interrupt"))
                .on_hover_text("Send KeyboardInterrupt (Ctrl+C)")
                .clicked()
            {
                if let Some(s) = &mut self.session {
                    s.interrupt();
                }
            }
            if ui
                .add_enabled(
                    running,
                    egui::Button::new(egui::RichText::new("■ Stop").color(theme.error)),
                )
                .on_hover_text("Stop the console (Rerun starts it again)")
                .clicked()
            {
                self.stop();
                self.lines.push(Line::System("Console stopped".into()));
            }
            if ui.button("Execute").clicked() && running {
                self.submit();
            }
            let compact = ui.available_width() < 950.0;
            let mut secondary_controls = |ui: &mut egui::Ui| {
                ui.checkbox(&mut self.preferences.soft_wrap, "Soft-wrap");
                if ui.button("History").clicked() {
                    self.history_popup = true;
                }
                ui.toggle_value(&mut self.show_queue, "Show command queue");
                if ui.button("Scroll to end").clicked() {
                    self.scroll_end = true;
                }
                if ui
                    .button("Print")
                    .on_hover_text("Export printable console transcript")
                    .clicked()
                {
                    self.print_requested = Some(
                        self.lines
                            .iter()
                            .map(|line| match line {
                                Line::Input(t) => format!(">>> {t}"),
                                Line::Stdout(t)
                                | Line::Stderr(t)
                                | Line::Result(t)
                                | Line::System(t) => t.clone(),
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    );
                }
                if ui.button("Settings…").clicked() {
                    self.settings_requested = true;
                }
                if ui
                    .add_enabled(running, egui::Button::new("Attach debugger to console"))
                    .clicked()
                {
                    if let Some(session) = &mut self.session {
                        session.request_debugger();
                    }
                }
                if ui.button("Clear all").clicked() {
                    self.lines.clear();
                }
            };
            if compact {
                ui.menu_button("More…", secondary_controls);
            } else {
                secondary_controls(ui);
            }
            ui.toggle_value(&mut self.show_variables, "Variables");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let status = match (&self.error, &self.ready, running) {
                    (Some(e), _, _) => (e.clone(), theme.error),
                    (_, Some(r), true) => (r.clone(), theme.text_dim),
                    (_, _, true) => ("Starting…".to_string(), theme.text_dim),
                    _ => ("Not running".to_string(), theme.text_dim),
                };
                // Elided to the room left: a long error or interpreter path must not cover the buttons.
                ui.add(
                    egui::Label::new(egui::RichText::new(status.0).color(status.1).small())
                        .truncate(),
                );
            });
        });
        ui.separator();
        if self.focused && ui.input_mut(|i| i.consume_key(egui::Modifiers::CTRL, egui::Key::L)) {
            self.lines.clear();
        }
        if self.focused
            && ui.input_mut(|i| {
                i.consume_key(egui::Modifiers::CTRL | egui::Modifiers::ALT, egui::Key::E)
            })
        {
            self.history_popup = true;
        }
        if self.show_queue {
            ui.label(format!("Pending commands: {}", self.typed.len()));
            for (_, code) in &self.typed {
                ui.monospace(code);
            }
        }
        if self.history_popup {
            let entries = self.history.entries().to_vec();
            egui::Window::new("Console history")
                .open(&mut self.history_popup)
                .show_modeless(ui.ctx(), |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for entry in entries.iter().rev() {
                            if ui.selectable_label(false, entry).clicked() {
                                self.input = entry.clone();
                                self.want_focus = true;
                            }
                        }
                    });
                });
        }
        if let Some((name, mut value)) = self.edit_variable.take() {
            let mut open = true;
            let mut saved = false;
            egui::Window::new(format!("Change value: {name}"))
                .open(&mut open)
                .show_modal(ui.ctx(), |ui| {
                    ui.text_edit_multiline(&mut value);
                    if ui.button("Set value").clicked() {
                        if let Some(session) = &mut self.session {
                            session.set_value(&name, &value);
                        }
                        saved = true;
                    }
                });
            if open && !saved {
                self.edit_variable = Some((name, value));
            }
        }
        if !self.plots.is_empty() && ui.button("Open Plots / SciView").clicked() {
            self.plots_requested = true;
        }
        if self.show_variables {
            egui::Panel::right("console_vars")
                .resizable(true)
                .default_size(320.0)
                .min_size(160.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("Variables").strong());
                        ui.checkbox(&mut self.show_special_variables, "Show special variables");
                    });
                    egui::ScrollArea::vertical()
                        .id_salt("console_vars_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            let vars = self.visible_variables();
                            if vars.is_empty() {
                                ui.label(
                                    egui::RichText::new("No variables yet").color(theme.text_dim),
                                );
                            }
                            let mut to_inspect: Vec<String> = Vec::new();
                            for v in &vars {
                                self.var_row(ui, theme, v, 0, &mut to_inspect);
                            }
                            for expr in to_inspect {
                                if let Some(e) = expr.strip_prefix("##table##") {
                                    let e = e.to_string();
                                    self.request_table(&e);
                                } else if let Some(s) = &mut self.session {
                                    s.inspect(&expr);
                                }
                            }
                        });
                });
        }
        // Output + prompt.
        let input_h = 30.0 + 18.0 * (self.input.matches('\n').count() as f32).min(6.0);
        let avail = ui.available_height() - input_h - 8.0;
        egui::ScrollArea::vertical()
            .id_salt("console_output")
            .auto_shrink([false, false])
            .max_height(avail.max(40.0))
            .stick_to_bottom(self.scroll_end)
            .show(ui, |ui| {
                for line in &self.lines {
                    let (prefix, text, color) = match line {
                        Line::Input(t) => (">>> ", t.as_str(), theme.accent),
                        Line::Stdout(t) => ("", t.as_str(), theme.text),
                        Line::Stderr(t) => ("", t.as_str(), theme.error),
                        Line::Result(t) => (
                            "",
                            t.as_str(),
                            if theme.high_contrast {
                                theme.text
                            } else {
                                Color32::from_rgb(0x6A, 0xB0, 0xDE)
                            },
                        ),
                        Line::System(t) => ("", t.as_str(), theme.text_dim),
                    };
                    for (i, l) in text.lines().enumerate() {
                        let p = if i == 0 {
                            prefix
                        } else if prefix.is_empty() {
                            ""
                        } else {
                            "... "
                        };
                        if let Some((path, line)) = traceback_location(l) {
                            if ui.link(l).clicked() {
                                self.navigate_request = Some((path, line));
                            }
                        } else {
                            ui.add(
                                egui::Label::new(ansi_layout_with_palette(
                                    &format!("{p}{l}"),
                                    mono.clone(),
                                    color,
                                    &self.output_preferences.ansi_palette,
                                ))
                                .wrap_mode(
                                    if self.preferences.soft_wrap {
                                        egui::TextWrapMode::Wrap
                                    } else {
                                        egui::TextWrapMode::Extend
                                    },
                                ),
                            );
                        }
                    }
                }
            });
        for diagnostic in self.semantic_diagnostics.iter().take(4) {
            ui.label(
                egui::RichText::new(format!("{}: {}", diagnostic.code, diagnostic.message))
                    .small()
                    .color(theme.warning),
            );
        }
        // While the completion list shows, it takes the keys that move in it,
        // accept and close it before the prompt sees them.
        let popup = self.completion_visible && !self.completions.is_empty();
        let keys = if popup && self.focused {
            ui.input_mut(CompletionKeys::take)
        } else {
            CompletionKeys::default()
        };
        let input_id = ui.make_persistent_id("console_input");
        if self.focused {
            ui.input_mut(|input| {
                for event in &mut input.events {
                    if let egui::Event::Paste(text) = event {
                        *text = dedent_paste(text);
                    }
                }
            });
        }
        let mut pick: Option<usize> = None;
        let prompt_rect = ui.horizontal(|ui| {
            let prompt = if self.continuation { "... " } else { ">>> " };
            ui.label(egui::RichText::new(prompt).font(mono.clone()).color(theme.accent));
            // Escape closes the list and leaves the caret in the prompt.
            let filter = egui::EventFilter { tab: true, horizontal_arrows: true, vertical_arrows: true, escape: popup };
            let mut layouter = |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, width: f32| {
                let mut job = python_layout(buffer.as_str(),mono.clone(),theme);
                job.wrap.max_width = width;
                ui.fonts_mut(|fonts| fonts.layout_job(job))
            };
            let resp = ui.add_enabled(running, egui::TextEdit::multiline(&mut self.input).layouter(&mut layouter).id(input_id).font(egui::TextStyle::Monospace).desired_rows(1).desired_width(f32::INFINITY).event_filter(filter).hint_text(if running { "Enter runs, Shift+Enter inserts a newline, Up/Down history, Ctrl+Space completes" } else { "Console is not running" }));
            if self.want_focus {
                resp.request_focus();
                self.want_focus = false;
            }
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, running, "Python console prompt"));
            self.focused = resp.has_focus();
            if popup {
                if keys.down {
                    self.completion_selected = (self.completion_selected + 1).min(self.completions.len().min(MAX_COMPLETIONS_SHOWN).saturating_sub(1));
                } else if keys.up {
                    self.completion_selected = self.completion_selected.saturating_sub(1);
                } else if keys.accept {
                    pick = Some(self.completion_selected);
                } else if keys.escape {
                    self.completion_visible = false;
                } else if resp.changed() {
                    // Typing goes on: complete the longer name, or close the list once it ends.
                    match &mut self.session {
                        Some(s) if completing_name(&self.input) => {
                            s.complete(&self.input, self.input.len());
                        }
                        _ => self.completion_visible = false,
                    }
                }
            } else if resp.has_focus() {
                let (enter, shift, up, down, ctrl_space) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.modifiers.shift, i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::ArrowDown), i.modifiers.ctrl && i.key_pressed(egui::Key::Space)));
                if enter && !shift {
                    // The TextEdit already inserted the newline: remove it and run.
                    if self.input.ends_with('\n') {
                        self.input.pop();
                    }
                    self.submit();
                } else if enter && shift {
                    let previous = self.input.trim_end_matches('\n').rsplit('\n').next().unwrap_or("");
                    let mut indent: String = previous.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
                    if previous.trim_end().ends_with(':') { indent.push_str("    "); }
                    self.input.push_str(&indent);
                } else if up && !self.input.contains('\n') {
                    if let Some(h) = self.history.prev() {
                        self.input = h.to_string();
                    }
                } else if down && !self.input.contains('\n') {
                    self.input = self.history.next().map(|h| h.to_string()).unwrap_or_default();
                } else if ctrl_space {
                    if let Some(s) = &mut self.session {
                        let cursor = self.input.len();
                        s.complete(&self.input, cursor);
                    }
                } else if resp.changed() {
                    self.history.reset_cursor();
                    if self.input.ends_with('.') {
                        if let Some(s) = &mut self.session {
                            let cursor = self.input.len();
                            s.complete(&self.input, cursor);
                        }
                    }
                }
            }
            resp.rect
        });
        // The list floats over the output above the prompt: laid out in the
        // tool window it would grow the window and push the prompt out of view.
        let popup_id = input_id.with("completion_popup");
        if self.completion_visible && !self.completions.is_empty() {
            // A click anywhere else closes it.
            let list_rect = ui.ctx().memory(|m| m.area_rect(popup_id));
            let clicked_elsewhere = ui.input(|i| {
                i.pointer.any_pressed()
                    && i.pointer.interact_pos().is_some_and(|p| {
                        !prompt_rect.inner.contains(p) && !list_rect.is_some_and(|r| r.contains(p))
                    })
            });
            if clicked_elsewhere {
                self.completion_visible = false;
            }
        }
        if self.completion_visible && !self.completions.is_empty() {
            egui::Area::new(popup_id)
                .order(egui::Order::Foreground)
                .pivot(egui::Align2::LEFT_BOTTOM)
                .fixed_pos(prompt_rect.inner.left_top() - Vec2::new(0.0, 2.0))
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style())
                        .fill(theme.panel_bg)
                        .show(ui, |ui| {
                            ui.set_min_width(220.0);
                            egui::ScrollArea::vertical()
                                .id_salt("console_completion")
                                .max_height(160.0)
                                .show(ui, |ui| {
                                    for (i, c) in self
                                        .completions
                                        .iter()
                                        .enumerate()
                                        .take(MAX_COMPLETIONS_SHOWN)
                                    {
                                        let text = format!("{}  {}", c.name, c.kind);
                                        let r = ui.selectable_label(
                                            i == self.completion_selected,
                                            egui::RichText::new(text).font(mono.clone()),
                                        );
                                        if i == self.completion_selected && (keys.up || keys.down) {
                                            r.scroll_to_me(None);
                                        }
                                        if r.clicked() {
                                            pick = Some(i);
                                        }
                                    }
                                });
                        });
                });
        }
        if let Some(i) = pick {
            self.completion_selected = i;
            self.accept_completion();
            // The caret goes after the inserted name, and stays in the prompt.
            if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), input_id) {
                let end = egui::text::CCursor::new(self.input.chars().count());
                state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::one(end)));
                state.store(ui.ctx(), input_id);
            }
            self.want_focus = true;
        }
        if self.accessibility_owner && self.focused && !egui::Popup::is_any_open(ui.ctx()) {
            let status = if self.completion_visible && !self.completions.is_empty() {
                let item =
                    &self.completions[self.completion_selected.min(self.completions.len() - 1)];
                format!("Python console completion {} of {}. {}. {}. Up Down select, Enter or Tab accepts, Escape closes.",
                    self.completion_selected + 1, self.completions.len().min(MAX_COMPLETIONS_SHOWN),
                    item.name.chars().take(160).collect::<String>(), item.kind.chars().take(80).collect::<String>())
            } else {
                let output = self
                    .lines
                    .last()
                    .map(|line| match line {
                        Line::Input(text)
                        | Line::Stdout(text)
                        | Line::Stderr(text)
                        | Line::Result(text)
                        | Line::System(text) => text,
                    })
                    .map(|text| text.chars().take(160).collect::<String>())
                    .unwrap_or_default();
                format!("Python console {}. {}. Enter runs, Shift Enter inserts a newline, Up Down history, Ctrl Space completes.",
                    if self.pending_exec.is_some() { "running input" } else if self.continuation { "continuation prompt" } else { "prompt" }, output)
            };
            crate::a11y::announce(ui.ctx(), input_id, &status);
        }
        if running || self.pending_exec.is_some() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(60));
        }
    }

    fn var_row(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        v: &VarInfo,
        depth: usize,
        to_inspect: &mut Vec<String>,
    ) {
        let key = if v.expr.is_empty() {
            v.name.clone()
        } else {
            v.expr.clone()
        };
        let expanded = self.expanded.contains(&key);
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 20.0), Sense::hover());
        let id = ui.id().with((
            "python-variable",
            self.process_epoch,
            self.variables_epoch,
            &key,
        ));
        let resp = ui.interact(rect, id, Sense::click());
        let focused = resp.has_focus();
        if focused {
            self.tree_focus = Some((id, self.variables_epoch));
        }
        let available = self.accessibility_owner && self.is_running();
        let label = format!(
            "{} = {}. Type {}",
            v.name.chars().take(160).collect::<String>(),
            v.value.chars().take(320).collect::<String>(),
            v.type_name.chars().take(80).collect::<String>()
        );
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, available, &label));
        if available && focused && !egui::Popup::is_any_open(ui.ctx()) {
            crate::a11y::announce(ui.ctx(), id, &format!("{label}. Left collapses, Right expands, Enter toggles, Shift F10 opens actions."));
        }
        if resp.hovered() {
            ui.painter().rect_filled(rect, 2.0, theme.hover_bg);
        }
        let x = rect.left() + 6.0 + depth as f32 * 14.0;
        let y = rect.center().y;
        if v.has_children {
            ui.painter().text(
                egui::Pos2::new(x, y),
                egui::Align2::LEFT_CENTER,
                if expanded { "▼" } else { "▶" },
                egui::FontId::proportional(9.0),
                theme.text_dim,
            );
        }
        let name_color = if theme.high_contrast {
            theme.text
        } else if v.is_module {
            theme.text_dim
        } else {
            Color32::from_rgb(0xC8, 0x8F, 0xD6)
        };
        let g = ui.fonts_mut(|f| {
            f.layout_no_wrap(v.name.clone(), egui::FontId::proportional(13.0), name_color)
        });
        ui.painter().galley(
            egui::Pos2::new(x + 14.0, y - g.size().y / 2.0),
            g.clone(),
            name_color,
        );
        let mut value = v.value.clone();
        if value.chars().count() > 120 {
            value = value.chars().take(117).collect();
            value.push('…');
        }
        let text = format!(" = {}", value.replace('\n', "⏎"));
        let g2 =
            ui.fonts_mut(|f| f.layout_no_wrap(text, egui::FontId::proportional(13.0), theme.text));
        let vx = x + 14.0 + g.size().x;
        ui.painter().galley(
            egui::Pos2::new(vx, y - g2.size().y / 2.0),
            g2.clone(),
            theme.text,
        );
        ui.painter().text(
            egui::Pos2::new(vx + g2.size().x + 8.0, y),
            egui::Align2::LEFT_CENTER,
            format!("{{{}}}", v.type_name),
            egui::FontId::proportional(11.0),
            theme.text_dim,
        );
        crate::a11y::keyboard_context_menu(&resp, available).show(|ui| {
            if ui.button("View as table (SciView)").clicked() {
                to_inspect.push(format!("##table##{key}"));
                ui.close();
            }
            if ui.button("Change value…").clicked() {
                self.edit_variable = Some((key.clone(), v.value.clone()));
                ui.close();
            }
            if ui.button("Copy value").clicked() {
                ui.ctx().copy_text(v.value.clone());
                ui.close();
            }
        });
        let keys = if available && focused && !egui::Popup::is_any_open(ui.ctx()) {
            ui.input_mut(|input| {
                (
                    input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft),
                    input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight),
                )
            })
        } else {
            (false, false)
        };
        let toggle = available && resp.clicked(); // Enter/Space share the same current owner gate as pointer activation.
        if v.has_children && (toggle || keys.0 && expanded || keys.1 && !expanded) {
            if expanded {
                self.expanded.remove(&key);
            } else {
                self.expanded.insert(key.clone());
                if !self.children.contains_key(&key) {
                    to_inspect.push(key.clone());
                }
            }
        }
        ui.ctx().accesskit_node_builder(id, |node| {
            node.set_role(egui::accesskit::Role::TreeItem);
            node.set_selected(focused);
            if v.has_children {
                node.set_expanded(self.expanded.contains(&key));
            }
        });
        resp.on_hover_text(&v.value);
        if expanded {
            match self.children.get(&key).cloned() {
                Some(children) => {
                    for c in &children {
                        self.var_row(ui, theme, c, depth + 1, to_inspect);
                    }
                }
                None => {
                    ui.label(egui::RichText::new("…").color(theme.text_dim));
                }
            }
        }
    }
}

/// Completions the list shows (typing more of the name narrows it).
const MAX_COMPLETIONS_SHOWN: usize = 50;

/// Keys the completion list takes from the prompt while it shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CompletionKeys {
    up: bool,
    down: bool,
    /// Enter or Tab.
    accept: bool,
    escape: bool,
}

impl CompletionKeys {
    fn take(input: &mut egui::InputState) -> CompletionKeys {
        use egui::{Key, Modifiers};
        let enter = input.consume_key(Modifiers::NONE, Key::Enter);
        let tab = input.consume_key(Modifiers::NONE, Key::Tab);
        CompletionKeys {
            up: input.consume_key(Modifiers::NONE, Key::ArrowUp),
            down: input.consume_key(Modifiers::NONE, Key::ArrowDown),
            accept: enter || tab,
            escape: input.consume_key(Modifiers::NONE, Key::Escape),
        }
    }
}

/// Byte offset where the name at the end of `input` starts (the part a
/// completion replaces: `jo` in `", ".jo`).
fn name_start(input: &str) -> usize {
    input
        .trim_end_matches(|c: char| c == '_' || c.is_alphanumeric())
        .len()
}

/// Whether `input` still ends in a name, or the `.` before one, that the
/// completion list can go on with.
fn completing_name(input: &str) -> bool {
    input
        .chars()
        .next_back()
        .is_some_and(|c| c == '.' || c == '_' || c.is_alphanumeric())
}

/// The completions answered for the name `prefix` that fit what the prompt
/// holds now: typing goes on while the answer comes.
fn applicable_completions(input: &str, prefix: &str, items: Vec<Completion>) -> Vec<Completion> {
    let typed = &input[name_start(input)..];
    if !completing_name(input) || !typed.starts_with(prefix) {
        return Vec::new();
    }
    items
        .into_iter()
        .filter(|c| c.name.starts_with(typed))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completion(name: &str) -> Completion {
        Completion {
            name: name.into(),
            kind: "function".into(),
        }
    }

    #[test]
    fn completions_follow_the_name_being_typed() {
        assert_eq!(name_start("os."), 3);
        assert_eq!(name_start("x = os.getc"), 7);
        assert_eq!(name_start("größe"), 0);
        assert!(
            completing_name("os.")
                && completing_name("os.ge")
                && !completing_name("os.getcwd(")
                && !completing_name("")
        );
        let items = || {
            vec![
                completion("getcwd"),
                completion("getenv"),
                completion("abc"),
            ]
        };
        let names = |v: Vec<Completion>| v.into_iter().map(|c| c.name).collect::<Vec<_>>();
        // The answer for `os.` while `os.getc` is typed: only what still fits.
        assert_eq!(
            names(applicable_completions("os.getc", "", items())),
            vec!["getcwd"]
        );
        assert_eq!(
            names(applicable_completions("os.", "", items())),
            vec!["getcwd", "getenv", "abc"]
        );
        // The name ended, or was deleted back past the answer's prefix: nothing.
        assert!(applicable_completions("os.getcwd(", "getc", items()).is_empty());
        assert!(applicable_completions("os.g", "getc", items()).is_empty());
    }

    /// Accepting replaces the name typed so far, even when it grew after the
    /// list came.
    #[test]
    fn accepting_a_completion_replaces_the_typed_name() {
        let mut c = PythonConsole {
            input: "print(os.get".into(),
            completions: vec![completion("getcwd"), completion("getenv")],
            completion_selected: 1,
            completion_visible: true,
            ..Default::default()
        };
        c.accept_completion();
        assert_eq!(c.input, "print(os.getenv");
        assert!(!c.completion_visible);
    }

    /// One frame of the console in a resizable bottom tool window: the
    /// window's rect and the completion list's (a foreground area), if shown.
    fn console_frame(
        ctx: &egui::Context,
        console: &mut PythonConsole,
        events: Vec<egui::Event>,
    ) -> (egui::Rect, Option<egui::Rect>) {
        let theme = UiTheme::dark();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(900.0, 400.0),
            )),
            events,
            ..Default::default()
        };
        let mut window = egui::Rect::NOTHING;
        ctx.run_ui(input, |ui| {
            window = egui::Panel::bottom("console_test_window")
                .resizable(true)
                .default_size(200.0)
                .show(ui, |ui| {
                    console.ui(ui, &theme, egui::FontId::monospace(13.0))
                })
                .response
                .rect;
        })
        .drop_without_applying_deltas();
        (
            window,
            ctx.memory(|m| {
                m.layer_ids()
                    .filter(|l| l.order == egui::Order::Foreground && m.areas().is_visible(l))
                    .find_map(|l| m.area_rect(l.id))
            }),
        )
    }

    /// The completion list floats over the output: the tool window keeps its
    /// size and the prompt its place. The list takes the keys that move in it,
    /// Escape closes it with the caret still in the prompt, and Enter accepts
    /// the selected name instead of starting a new line.
    #[test]
    fn the_completion_list_floats_over_the_console() {
        let dir =
            std::env::temp_dir().join(format!("rustpy-console-panel-popup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut console = PythonConsole::default();
        console.start(Path::new("python3"), &dir, &[], &dir);
        if !console.is_running() {
            eprintln!("python3 not available; skipping");
            return;
        }
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let key = |key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        console.input = "os.".into();
        console.want_focus = true;
        console_frame(&ctx, &mut console, Vec::new());
        let (window, no_list) = console_frame(&ctx, &mut console, Vec::new());
        assert!(no_list.is_none() && console.focused);
        console.completions = (0..30).map(|i| completion(&format!("name{i}"))).collect();
        console.completion_visible = true;
        for _ in 0..3 {
            let (with_list, list) = console_frame(&ctx, &mut console, Vec::new());
            assert_eq!(with_list, window, "the list must not grow the tool window");
            let list = list.expect("the list shows");
            assert!(
                list.height() > 100.0 && list.bottom() <= window.bottom(),
                "{list:?} in {window:?}"
            );
        }
        console_frame(&ctx, &mut console, vec![key(egui::Key::ArrowDown)]);
        assert_eq!(console.completion_selected, 1);
        console_frame(&ctx, &mut console, vec![key(egui::Key::Escape)]);
        assert!(!console.completion_visible, "Escape closes the list");
        console_frame(&ctx, &mut console, Vec::new());
        assert!(console.focused, "the caret stays in the prompt");
        assert_eq!(
            console.input, "os.",
            "the keys went to the list, not into the text"
        );
        console.completion_visible = true;
        console_frame(&ctx, &mut console, Vec::new());
        console_frame(&ctx, &mut console, vec![key(egui::Key::Enter)]);
        assert_eq!(console.input, "os.name1");
        let (_, list) = console_frame(&ctx, &mut console, Vec::new());
        assert!(list.is_none() && console.focused);
        console.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Painted texts and where they are, from one frame's shapes.
    fn painted_texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<(String, egui::Rect)> {
        fn walk(shape: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::Shape::Text(t) => out.push((
                    t.galley.text().to_string(),
                    t.galley.rect.translate(t.pos.to_vec2()),
                )),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for s in shapes {
            walk(&s.shape, &mut out);
        }
        out
    }

    /// A long status (here an error) is elided to the room right of the
    /// toolbar's buttons instead of being painted over them, where it would
    /// also take their clicks.
    #[test]
    fn a_long_status_leaves_the_toolbar_buttons_alone() {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let theme = UiTheme::dark();
        let reason = format!(
            "Cannot start the console: {}",
            "the interpreter at /home/user/projects/example/.venv/bin/python is gone ".repeat(4)
        );
        let mut console = PythonConsole {
            error: Some(reason),
            ..Default::default()
        };
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(800.0, 300.0),
            )),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            egui::Panel::bottom("console_status_window")
                .default_size(200.0)
                .show(ui, |ui| {
                    console.ui(ui, &theme, egui::FontId::monospace(13.0))
                });
        });
        let texts = painted_texts(&output.shapes);
        output.drop_without_applying_deltas();
        let toggle = texts
            .iter()
            .filter(|(t, _)| t == "Variables")
            .map(|(_, r)| *r)
            .min_by(|a, b| a.top().total_cmp(&b.top()))
            .expect("Variables toggle");
        let status = texts
            .iter()
            .find(|(t, _)| t.starts_with("Cannot start"))
            .map(|(_, r)| *r)
            .expect("status");
        assert!(
            status.left() >= toggle.right() && status.right() <= 800.0,
            "status {status:?} over the toggle {toggle:?}"
        );
    }

    /// Typing `os.` at the prompt opens the list with the module's names, and
    /// typing on narrows it.
    #[test]
    fn typing_a_dot_lists_the_attributes() {
        let dir =
            std::env::temp_dir().join(format!("rustpy-console-panel-dot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut console = PythonConsole::default();
        console.start(Path::new("python3"), &dir, &[], &dir);
        if !console.is_running() {
            eprintln!("python3 not available; skipping");
            return;
        }
        console.execute("import os");
        settle(&mut console);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        console.want_focus = true;
        console_frame(&ctx, &mut console, Vec::new());
        // Polls and draws until `done` holds.
        let wait = |console: &mut PythonConsole, done: &dyn Fn(&PythonConsole) -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while !done(console) && std::time::Instant::now() < deadline {
                console.poll();
                console_frame(&ctx, console, Vec::new());
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(
                done(console),
                "input {:?}, visible {}, focused {}, list {:?}",
                console.input,
                console.completion_visible,
                console.focused,
                console
                    .completions
                    .iter()
                    .map(|c| &c.name)
                    .take(5)
                    .collect::<Vec<_>>()
            );
        };
        for c in ["o", "s", "."] {
            console_frame(&ctx, &mut console, vec![egui::Event::Text(c.into())]);
        }
        assert_eq!(console.input, "os.");
        // No-argument callables come with their parentheses (`getcwd()`).
        wait(&mut console, &|c| {
            c.completion_visible && c.completions.iter().any(|i| i.name.starts_with("getcwd"))
        });
        let (_, list) = console_frame(&ctx, &mut console, Vec::new());
        assert!(list.is_some(), "the list shows");
        for c in ["g", "e", "t", "c"] {
            console_frame(&ctx, &mut console, vec![egui::Event::Text(c.into())]);
        }
        wait(&mut console, &|c| {
            c.completion_visible
                && !c.completions.is_empty()
                && c.completions.iter().all(|i| i.name.starts_with("getc"))
        });
        console_frame(&ctx, &mut console, vec![egui::Event::Text("(".into())]);
        assert!(!console.completion_visible, "the name ended");
        console.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Polls until no request is outstanding.
    fn settle(c: &mut PythonConsole) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while c.pending_exec.is_some() && std::time::Instant::now() < deadline {
            c.poll();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            c.pending_exec.is_none(),
            "console did not answer; lines: {:?}",
            c.lines.iter().map(line_text).collect::<Vec<_>>()
        );
    }

    fn line_text(l: &Line) -> String {
        match l {
            Line::Input(t) => format!(">>> {t}"),
            Line::Stdout(t) | Line::Stderr(t) | Line::Result(t) | Line::System(t) => t.clone(),
        }
    }

    #[test]
    fn editor_code_is_echoed_and_runs_as_a_fragment() {
        let dir = std::env::temp_dir().join(format!("rustpy-console-panel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("job.py"),
            "import sys\nanswer = 6 * 7\nprint('job', sys.argv[1:])\n",
        )
        .unwrap();
        let mut c = PythonConsole::default();
        c.start(Path::new("python3"), &dir, &[], &dir);
        if !c.is_running() {
            eprintln!("python3 not available; skipping");
            return;
        }
        // Nothing is sent while the console is stopped.
        let mut stopped = PythonConsole::default();
        assert!(!stopped.execute_fragment("1"));

        // A typed open block leaves the prompt in continuation mode...
        c.execute("def f():");
        settle(&mut c);
        assert!(c.continuation);
        // ...code from the editor abandons it and runs in one go, trailing expression shown.
        assert!(c.execute_fragment("def f():\n    return 41\n\nf() + 1"));
        settle(&mut c);
        assert!(!c.continuation && c.buffer.is_empty());
        let texts: Vec<String> = c.lines.iter().map(line_text).collect();
        assert!(
            texts.contains(&">>> def f():\n    return 41\n\nf() + 1".to_string()),
            "{texts:?}"
        );
        assert_eq!(texts.last().map(String::as_str), Some("42"), "{texts:?}");
        assert_eq!(
            c.history.entries().last().map(String::as_str),
            Some("def f():\n    return 41\n\nf() + 1")
        );

        // Run File: echoed as a runfile() call, variables stay in the console.
        assert!(c.run_file(&dir.join("job.py"), &["a".into()], Some(&dir), &[]));
        settle(&mut c);
        let texts: Vec<String> = c.lines.iter().map(line_text).collect();
        let echo = format!(
            ">>> runfile('{}', args=['a'], wdir='{}')",
            dir.join("job.py").display(),
            dir.display()
        );
        assert!(texts.contains(&echo), "{texts:?}");
        assert!(texts.contains(&"job ['a']\n".to_string()), "{texts:?}");
        c.execute("answer");
        settle(&mut c);
        assert_eq!(c.lines.last().map(line_text).as_deref(), Some("42"));
        c.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_tool_window_starts_the_console_once() {
        let dir =
            std::env::temp_dir().join(format!("rustpy-console-panel-start-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut c = PythonConsole::default();
        assert!(
            c.take_start_request(),
            "started when the window is first shown"
        );
        // A console that cannot start is not retried on every frame...
        c.start(Path::new("/nonexistent/python-xyz"), &dir, &[], &dir);
        assert!(!c.is_running() && c.start_error().is_some());
        assert!(!c.take_start_request());
        assert!(!c.take_start_request());
        // ...until Rerun asks for one new attempt.
        c.rerun();
        assert!(c.take_start_request());
        assert!(!c.take_start_request());
        // A stopped console stays stopped, with its output.
        c.start(Path::new("python3"), &dir, &[], &dir);
        if c.is_running() {
            c.execute("1 + 1");
            settle(&mut c);
            c.stop();
            assert!(!c.is_running() && !c.take_start_request());
            assert!(
                c.lines.iter().map(line_text).any(|t| t == "2"),
                "the output is kept"
            );
            c.rerun();
            assert!(c.take_start_request());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn results_only_affect_the_typed_input_they_answer() {
        let dir =
            std::env::temp_dir().join(format!("rustpy-console-panel-ids-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut c = PythonConsole::default();
        c.start(Path::new("python3"), &dir, &[], &dir);
        if !c.is_running() {
            eprintln!("python3 not available; skipping");
            return;
        }
        // A cell sent from the editor is still running when an unfinished block
        // is typed at the prompt: the cell's result must not end that block.
        assert!(c.execute_fragment("import time\ntime.sleep(0.5)\n'slept'"));
        c.execute("def g():");
        settle(&mut c);
        assert!(c.continuation, "the typed block still waits for its body");
        assert_eq!(c.buffer, "def g():\n");
        c.execute("    return 7");
        c.execute("");
        settle(&mut c);
        assert!(!c.continuation && c.buffer.is_empty());
        c.execute("g()");
        settle(&mut c);
        assert_eq!(c.lines.last().map(line_text).as_deref(), Some("7"));
        let history = c.history.entries().to_vec();
        assert!(
            history.contains(&"def g():\n    return 7".to_string()),
            "{history:?}"
        );
        assert!(
            !history.contains(&"def g():".to_string()),
            "an unfinished block is not a history entry: {history:?}"
        );
        // A table request answered while a block is open does not close it either.
        c.execute("for i in range(2):");
        settle(&mut c);
        assert!(c.continuation);
        c.request_table("[[1, 2]]");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while c.table_json.is_none() && std::time::Instant::now() < deadline {
            c.poll();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(c.table_json.is_some(), "the table arrived");
        c.poll();
        assert!(
            c.continuation && c.buffer == "for i in range(2):\n",
            "buffer: {:?}",
            c.buffer
        );
        // When the process dies with a request outstanding, nothing is awaited any more.
        c.execute_fragment("import os\nos._exit(3)");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while c.is_running() && std::time::Instant::now() < deadline {
            c.poll();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        c.poll();
        assert!(!c.is_running());
        assert_eq!(c.pending_exec, None);
        c.stop();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn traceback_location(text: &str) -> Option<(PathBuf, usize)> {
    let rest = text.trim().strip_prefix("File \"")?;
    let (path, rest) = rest.split_once("\", line ")?;
    let line = rest
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    Some((PathBuf::from(path), line))
}
#[cfg(test)]
fn ansi_layout(text: &str, font: egui::FontId, default: Color32) -> egui::text::LayoutJob {
    ansi_layout_with_palette(text, font, default, &[])
}
fn ansi_layout_with_palette(
    text: &str,
    font: egui::FontId,
    default: Color32,
    palette: &[[u8; 3]],
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let mut color = default;
    let mut rest = text;
    while let Some(at) = rest.find("\x1b[") {
        job.append(
            &rest[..at],
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color,
                ..Default::default()
            },
        );
        rest = &rest[at + 2..];
        let Some(end) = rest.find('m') else { break };
        for code in rest[..end].split(';').filter_map(|c| c.parse::<u8>().ok()) {
            let index = match code {
                30..=37 => Some(code - 30),
                90..=97 => Some(code - 90 + 8),
                _ => None,
            };
            if let Some(rgb) = index.and_then(|index| palette.get(index as usize)) {
                color = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
                continue;
            }
            color = match code {
                0 | 39 => default,
                30 => Color32::BLACK,
                31 => Color32::LIGHT_RED,
                32 => Color32::LIGHT_GREEN,
                33 => Color32::YELLOW,
                34 => Color32::LIGHT_BLUE,
                35 => Color32::from_rgb(220, 120, 220),
                36 => Color32::from_rgb(100, 220, 220),
                37 => Color32::WHITE,
                _ => color,
            };
        }
        rest = &rest[end + 1..];
    }
    job.append(
        rest,
        0.0,
        egui::TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    job
}

fn dedent_paste(text: &str) -> String {
    if !text.contains('\n') {
        return text.to_string();
    }
    let indent = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            line.bytes()
                .take_while(|b| *b == b' ' || *b == b'\t')
                .count()
        })
        .min()
        .unwrap_or(0);
    text.split('\n')
        .map(|line| {
            &line[indent.min(
                line.bytes()
                    .take_while(|b| *b == b' ' || *b == b'\t')
                    .count(),
            )..]
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn python_layout(text: &str, font: egui::FontId, theme: &UiTheme) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    let mut position = 0;
    for span in rustpy_highlight::highlight_python(text) {
        let start = usize::from(span.range.start());
        let end = usize::from(span.range.end());
        if start > position {
            job.append(
                &text[position..start],
                0.0,
                egui::TextFormat {
                    font_id: font.clone(),
                    color: theme.text,
                    ..Default::default()
                },
            );
        }
        let color = match span.tag {
            rustpy_highlight::HighlightTag::Keyword => theme.accent,
            rustpy_highlight::HighlightTag::String => Color32::LIGHT_GREEN,
            rustpy_highlight::HighlightTag::Comment => theme.text_dim,
            _ => theme.text,
        };
        job.append(
            &text[start..end],
            0.0,
            egui::TextFormat {
                font_id: font.clone(),
                color,
                ..Default::default()
            },
        );
        position = end;
    }
    if position < text.len() {
        job.append(
            &text[position..],
            0.0,
            egui::TextFormat {
                font_id: font,
                color: theme.text,
                ..Default::default()
            },
        );
    }
    job
}
#[cfg(test)]
mod console_preferences_tests {
    use super::*;
    #[test]
    fn pasted_blocks_and_traceback_links_preserve_unicode_and_relative_paths() {
        assert_eq!(
            dedent_paste("    if True:\n        π = 1\n"),
            "if True:\n    π = 1\n"
        );
        assert_eq!(
            traceback_location("  File \"src/π.py\", line 12, in run"),
            Some((PathBuf::from("src/π.py"), 12))
        );
        assert!(traceback_location("arbitrary output").is_none());
    }
    #[test]
    fn ansi_output_retains_text_and_segment_colours() {
        let output = ansi_layout(
            "before \x1b[31merror\x1b[0m after",
            egui::FontId::monospace(12.0),
            Color32::WHITE,
        );
        assert_eq!(output.text, "before error after");
        assert_eq!(output.sections[1].format.color, Color32::LIGHT_RED);
    }
}

#[cfg(test)]
mod scientific_console_tests {
    use super::*;
    #[test]
    fn variables_pane_hides_ipython_internals_until_special_variables_are_shown() {
        let mut console = PythonConsole::default();
        for (name, type_name, callable) in [
            ("In", "list", false),
            ("Out", "dict", false),
            ("exit", "ExitAutocall", true),
            ("get_ipython", "method", true),
            ("open", "function", true),
            ("_ih", "list", false),
            ("df", "DataFrame", false),
            ("arr", "ndarray", false),
        ] {
            console.vars.push(VarInfo {
                name: name.into(),
                expr: name.into(),
                type_name: type_name.into(),
                value: String::new(),
                is_callable: callable,
                is_module: false,
                has_children: false,
            });
        }
        let names = |console: &PythonConsole| {
            console
                .visible_variables()
                .into_iter()
                .map(|variable| variable.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&console), ["df", "arr"]);
        console.show_special_variables = true;
        assert_eq!(names(&console).len(), 8);
    }
    #[test]
    fn semantic_overlay_resolves_previous_definitions_but_reports_current_unknown_names() {
        let root = Path::new("/rustpy-console-overlay-fixture");
        let analysis = rustpy_semantic::Analysis::for_project(root);
        let mut console = PythonConsole::default();
        console
            .successful_code
            .push_back("__rustpy_console_Any_0 = 42\n__rustpy_console_cast_0 = 42\n".into());
        console
            .successful_code
            .push_back("def prior(value: int) -> int:\n    return value\n".into());
        console.vars.push(VarInfo {
            name: "dynamic_global".into(),
            expr: "dynamic_global".into(),
            type_name: "opaque".into(),
            value: "<opaque>".into(),
            is_callable: false,
            is_module: false,
            has_children: true,
        });
        console.input = "prior(1)\ndynamic_global.unknown_attribute\nmissing_console_name".into();
        console.update_semantic_inspections(&analysis, root, &[]);
        assert!(console
            .semantic_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("missing_console_name")));
        assert!(!console
            .semantic_diagnostics
            .iter()
            .any(
                |diagnostic| diagnostic.message.contains("Unresolved reference 'prior'")
                    || diagnostic.message.contains("unknown_attribute")
            ));
        assert!(console
            .semantic_diagnostics
            .iter()
            .all(|diagnostic| usize::from(diagnostic.range.end()) <= console.input.len()));
        assert!(analysis
            .cached_file_for_path(&root.join("__rustpy_console_inspection__.py"))
            .is_none());
        // Successful code supersedes an opaque runtime snapshot; the overlay
        // must still diagnose attributes of a newly known concrete value.
        console
            .successful_code
            .push_back("dynamic_global = 1\n".into());
        console.semantic_key = None;
        console.update_semantic_inspections(&analysis, root, &[]);
        assert!(console
            .semantic_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("unknown_attribute")));
    }
    #[test]
    fn oversized_console_input_is_not_analyzed() {
        let root = Path::new("/rustpy-console-overlay-fixture");
        let analysis = rustpy_semantic::Analysis::for_project(root);
        let mut console = PythonConsole {
            input: "x".repeat(16_385),
            ..Default::default()
        };
        console.update_semantic_inspections(&analysis, root, &[]);
        assert!(console.semantic_diagnostics.is_empty());
    }
}

#[cfg(test)]
mod editor_console_preferences_fixture {
    use super::*;
    #[test]
    fn real_output_cycle_and_ansi_palette_follow_editor_console_preferences() {
        let mut panel = PythonConsole {
            lines: vec![
                Line::Stdout("old".repeat(500)),
                Line::Stdout("retained".into()),
            ],
            ..Default::default()
        };
        let prefs = rustpy_settings::run_console::Settings {
            buffer_kib: 1,
            ..Default::default()
        };
        panel.configure_output(&prefs);
        assert_eq!(panel.lines.len(), 1);
        let mut palette = vec![[0, 0, 0]; 16];
        palette[1] = [1, 2, 3];
        let job = ansi_layout_with_palette(
            "\x1b[31mred",
            egui::FontId::monospace(13.0),
            Color32::WHITE,
            &palette,
        );
        assert_eq!(
            job.sections.last().unwrap().format.color,
            Color32::from_rgb(1, 2, 3)
        );
    }
}
