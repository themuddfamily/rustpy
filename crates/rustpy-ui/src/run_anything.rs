//! Keyboard launchers (roadmap §3.1): Run Anything (double Ctrl), the
//! Run… / Debug… configuration choosers (Alt+Shift+F10 / Alt+Shift+F9), the
//! Stop picker (Ctrl+F2 with several running processes) and the `$Prompt$`
//! input asked for before a configuration runs.
//!
//! The popups only produce [`LauncherEvent`]s; the application starts the
//! processes. Run Anything commands are classified by [`plan_command`]:
//! `pytest …` goes to the Test Runner, `python script.py …` runs (or debugs)
//! the script, `pip …` uses the project interpreter, anything else runs as a
//! process with the project's virtualenv on `PATH`.

use crate::a11y::DialogWindow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use egui::{Align2, Color32, Sense, Vec2};
use rustpy_project::RunConfig;

use crate::run_context::RunMode;
use crate::theme::UiTheme;

/// Something the Stop picker can stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopItem {
    /// A Run tool window session: `(id, name)`.
    Session(u64, String),
    /// A measured run captured by identity, independent of selected tool tab.
    Measured(u64, String),
    /// The debug session (its name).
    Debug(String),
    DebugSession(u64, String),
    Console(u64, String),
    Http(u64, String),
    SharedIndex(u64, String),
    WebPreview(Vec<String>),
    DataFiles(Vec<u64>),
    Deployment(u64, String, String),
    Update(u64, String),
    Database(u64, String),
    IdeDiagnostic(u64, String),
    DiagramCapture(u64),
    SettingsSync(u64, String, String),
    WebLanguages(Vec<String>),
    Collaboration(String),
    AiWorkers(Vec<u64>),
    RemoteDevelopment(u64, String, String),
    PluginJob(u64, String),
    WindowOperation(u64, String),
    DockerSnapshot(u64, String, String),
    FileWatchers(String),
    BeforeLaunch(u64, String),
    Tests,
    Coverage,
    Profiler,
    Concurrency,
}

impl StopItem {
    pub fn label(&self) -> String {
        match self {
            StopItem::Session(_, name) | StopItem::Measured(_, name) => name.clone(),
            StopItem::Debug(name) | StopItem::DebugSession(_, name) => format!("{name} (debug)"),
            StopItem::Console(_, name) => format!("{name} (Python console)"),
            StopItem::Http(_, name) => format!("{name} (HTTP request)"),
            StopItem::SharedIndex(_, host) => format!("Shared index from {host}"),
            StopItem::WebPreview(_) => "Web preview".into(),
            StopItem::DataFiles(_) => "Data-file operation".into(),
            StopItem::Deployment(_, label, _) => label.clone(),
            StopItem::Update(_, label) => label.clone(),
            StopItem::Database(_, name) => format!("Database {name}"),
            StopItem::IdeDiagnostic(_, title) => title.clone(),
            StopItem::DiagramCapture(_) => "Diagram model capture".into(),
            StopItem::SettingsSync(_, name, _) => name.clone(),
            StopItem::WebLanguages(_) => "Web language servers".into(),
            StopItem::Collaboration(_) => "Collaboration session".into(),
            StopItem::AiWorkers(_) => "AI requests".into(),
            StopItem::RemoteDevelopment(_, name, _) => name.clone(),
            StopItem::PluginJob(_, name) => name.clone(),
            StopItem::WindowOperation(_, name) => name.clone(),
            StopItem::DockerSnapshot(_, name, _) => name.clone(),
            StopItem::FileWatchers(_) => "Pending file watchers".into(),
            StopItem::BeforeLaunch(_, name) => format!("{name} (before launch)"),
            StopItem::Tests => "Tests".into(),
            StopItem::Coverage => "Run with Coverage".into(),
            StopItem::Profiler => "Profiler".into(),
            StopItem::Concurrency => "Concurrency capture".into(),
        }
    }
}

/// What a launcher popup asks the application to do.
#[derive(Clone, Debug, PartialEq)]
pub enum LauncherEvent {
    /// Run / debug the named configuration.
    Launch(String, RunMode),
    /// Run / debug the current editor file ("Current File").
    LaunchCurrentFile(RunMode),
    EditConfigurations,
    /// Run Anything: execute a command line (`debug` from Shift+Enter).
    Command {
        line: String,
        debug: bool,
    },
    /// Stop the given processes (all of them for "Stop All").
    Stop(Vec<StopItem>),
    /// The `$Prompt$` answer for a configuration launch; `replaces` is the
    /// Run session a Rerun replaces once the launch starts.
    Prompted {
        cfg: Box<RunConfig>,
        mode: RunMode,
        answer: String,
        replaces: Option<u64>,
    },
}

/// How a Run Anything command line is executed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandPlan {
    /// pytest in the Test Runner: targets (paths / node ids) and other arguments.
    Pytest {
        targets: Vec<String>,
        args: Vec<String>,
    },
    /// `python [opts] script.py args` or `script.py args`: a script run with the project interpreter.
    Script {
        interpreter_options: Vec<String>,
        script: String,
        args: Vec<String>,
    },
    /// A process; `program: None` means the project interpreter.
    Process {
        program: Option<String>,
        args: Vec<String>,
    },
}

/// pytest options that take a separate value (`-k EXPR`).
const PYTEST_VALUE_OPTIONS: &[&str] = &[
    "-k",
    "-m",
    "-p",
    "-c",
    "-o",
    "-n",
    "-W",
    "-r",
    "--tb",
    "--maxfail",
    "--rootdir",
    "--deselect",
    "--ignore",
    "--ignore-glob",
    "--durations",
    "--junitxml",
    "--basetemp",
    "--confcutdir",
    "--log-level",
    "--cov",
    "--cov-report",
    "--dist",
    "--import-mode",
    "--capture",
    "--color",
    "--override-ini",
    "--lf-no-failures",
];

fn is_python(token: &str) -> bool {
    let name = Path::new(token)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(token);
    let name = name.strip_suffix(".exe").unwrap_or(name);
    name == "python"
        || name == "py"
        || (name.starts_with("python") && name[6..].chars().all(|c| c.is_ascii_digit() || c == '.'))
}

fn split_pytest_args(args: &[String], root: &Path) -> (Vec<String>, Vec<String>) {
    let mut targets = Vec::new();
    let mut rest = Vec::new();
    let mut value_next = false;
    for a in args {
        if value_next {
            rest.push(a.clone());
            value_next = false;
        } else if a.starts_with('-') {
            value_next = PYTEST_VALUE_OPTIONS.contains(&a.as_str());
            rest.push(a.clone());
        } else {
            let file = a.split("::").next().unwrap_or(a);
            if a.contains("::")
                || file.ends_with(".py")
                || root.join(file).exists()
                || Path::new(file).is_absolute()
            {
                targets.push(a.clone());
            } else {
                rest.push(a.clone());
            }
        }
    }
    (targets, rest)
}

/// Classifies a Run Anything command line (see the module docs).
pub fn plan_command(line: &str, root: &Path) -> Option<CommandPlan> {
    let tokens = RunConfig::split_args(line.trim());
    let (first, rest) = tokens.split_first()?;
    if first == "pytest" || first == "py.test" {
        let (targets, args) = split_pytest_args(rest, root);
        return Some(CommandPlan::Pytest { targets, args });
    }
    if is_python(first) {
        // Interpreter options up to the script / `-m` / `-c` (`-X dev` and `-W x` take a value).
        let mut n = 0;
        while let Some(a) = rest.get(n) {
            if !a.starts_with('-') || a == "-m" || a == "-c" {
                break;
            }
            n += if (a == "-X" || a == "-W") && n + 1 < rest.len() {
                2
            } else {
                1
            };
        }
        let options = rest[..n].to_vec();
        let after = &rest[n..];
        if after.first().map(String::as_str) == Some("-m")
            && after.get(1).map(String::as_str) == Some("pytest")
        {
            let (targets, args) = split_pytest_args(&after[2..], root);
            return Some(CommandPlan::Pytest { targets, args });
        }
        if let Some(script) = after
            .first()
            .filter(|s| s.ends_with(".py") || s.ends_with(".pyw"))
        {
            return Some(CommandPlan::Script {
                interpreter_options: options,
                script: script.clone(),
                args: after[1..].to_vec(),
            });
        }
        return Some(CommandPlan::Process {
            program: None,
            args: rest.to_vec(),
        });
    }
    if first == "pip" || first == "pip3" {
        return Some(CommandPlan::Process {
            program: None,
            args: ["-m".to_string(), "pip".to_string()]
                .into_iter()
                .chain(rest.iter().cloned())
                .collect(),
        });
    }
    if first.ends_with(".py") {
        return Some(CommandPlan::Script {
            interpreter_options: Vec::new(),
            script: first.clone(),
            args: rest.to_vec(),
        });
    }
    Some(CommandPlan::Process {
        program: Some(first.clone()),
        args: rest.to_vec(),
    })
}

/// Joins arguments into a parameter string that [`RunConfig::split_args`] splits back.
pub fn quote_args(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if !a.is_empty() && !a.contains(|c: char| c.is_whitespace() || c == '"' || c == '\'') {
                a.clone()
            } else if a.contains('"') {
                format!("'{a}'")
            } else {
                format!("\"{a}\"")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A command started from Run Anything (kept for Rerun).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq)]
enum AnythingItem {
    Config(String, &'static str),
    Command(String),
    Recent(String),
    /// Replaces the query (path completion after `python `).
    Complete(String),
}

pub struct AnythingState {
    pub query: String,
    selected: usize,
    focus: bool,
    /// Project `.py` files for completion (relative, loaded on demand).
    py_files: Option<Vec<String>>,
    last_query: String,
}

pub enum Popup {
    Anything(AnythingState),
    Chooser {
        debug: bool,
        selected: usize,
    },
    Stop {
        items: Vec<StopItem>,
        selected: usize,
    },
    Prompt {
        cfg: Box<RunConfig>,
        mode: RunMode,
        input: String,
        focus: bool,
        replaces: Option<u64>,
    },
}

/// Launcher popups and their persistent bits (lives in the Run tool window state).
#[derive(Default)]
pub struct Launcher {
    pub popup: Option<Popup>,
    last_ctrl: Option<f64>,
    ctrl_clean: bool,
    ctrl_down: Option<egui::Key>,
    ctrl_owner: Option<egui::Id>,
    history: Vec<String>,
    history_root: Option<PathBuf>,
    /// Run Anything commands by Run-window session name (Rerun repeats them).
    pub commands: HashMap<String, CommandSpec>,
    /// The configuration each Run-window session was started from, by
    /// session id, and the one of the last debug session: Rerun uses them
    /// when the configuration itself is gone (temporary configurations are
    /// evicted by newer context launches).
    pub launched: HashMap<u64, RunConfig>,
    pub debug_launched: Option<RunConfig>,
    /// ▶ gutter lines per editor tab: `(document version, path, lines)`.
    pub gutter_cache: HashMap<egui::Id, (u64, PathBuf, Vec<usize>)>,
    /// The chooser row last scrolled into view (keyboard selection).
    shown_selection: Option<usize>,
}

const HISTORY_FILE: &str = "runAnything.json";
const HISTORY_LIMIT: usize = 30;

/// Data the launcher popups show.
pub struct LauncherInput<'a> {
    pub root: &'a Path,
    pub configs: &'a [RunConfig],
    pub selected_config: Option<&'a str>,
    /// File name of the current editor file when it can be run.
    pub current_file: Option<String>,
}

impl Launcher {
    pub fn is_open(&self) -> bool {
        self.popup.is_some()
    }

    pub fn open_anything(&mut self, root: &Path) {
        self.load_history(root);
        self.popup = Some(Popup::Anything(AnythingState {
            query: String::new(),
            selected: 0,
            focus: true,
            py_files: None,
            last_query: String::new(),
        }));
    }

    /// Opens the Run… (or Debug…) chooser with the selected configuration
    /// (or "Current File") highlighted.
    pub fn open_chooser(&mut self, debug: bool, input: &LauncherInput) {
        let items = chooser_items(input);
        let selected = match input.selected_config {
            Some(name) => items
                .iter()
                .position(|i| matches!(i, ChooserItem::Config(n, _) if n == name)),
            None => items.iter().position(|i| *i == ChooserItem::CurrentFile),
        };
        self.popup = Some(Popup::Chooser {
            debug,
            selected: selected.unwrap_or(0),
        });
        self.shown_selection = None;
    }

    pub fn open_stop(&mut self, items: Vec<StopItem>) {
        self.popup = Some(Popup::Stop { items, selected: 0 });
    }

    /// Asks for the `$Prompt$` value of a launch; `replaces`: the Run session
    /// a Rerun replaces (it keeps running when the prompt is cancelled).
    pub fn open_prompt(&mut self, cfg: RunConfig, mode: RunMode, replaces: Option<u64>) {
        self.popup = Some(Popup::Prompt {
            cfg: Box::new(cfg),
            mode,
            input: String::new(),
            focus: true,
            replaces,
        });
    }

    fn load_history(&mut self, root: &Path) {
        if self.history_root.as_deref() == Some(root) {
            return;
        }
        self.history = std::fs::read_to_string(root.join(".rustpy").join(HISTORY_FILE))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        self.history_root = Some(root.to_path_buf());
    }

    /// Recent Run Anything commands (most recent first).
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Records a command in the project's Run Anything history.
    pub fn remember(&mut self, root: &Path, line: &str) {
        self.load_history(root);
        let line = line.trim().to_string();
        if line.is_empty() {
            return;
        }
        self.history.retain(|h| h != &line);
        self.history.insert(0, line);
        self.history.truncate(HISTORY_LIMIT);
        let dir = root.join(".rustpy");
        if std::fs::create_dir_all(&dir).is_ok() {
            if let Ok(json) = serde_json::to_string_pretty(&self.history) {
                let _ = std::fs::write(dir.join(HISTORY_FILE), json);
            }
        }
    }

    /// Interrupt a pending Ctrl gesture when another input owner takes over.
    pub fn reset_ctrl_gesture(&mut self) {
        self.last_ctrl = None;
        self.ctrl_clean = false;
        self.ctrl_down = None;
        self.ctrl_owner = None;
    }

    /// Run Anything requires two completed clean Ctrl taps within 350 ms.
    /// Waiting for release leaves a second held Ctrl available for caret cloning.
    pub fn double_ctrl(&mut self, ui: &egui::Ui) -> bool {
        let owner = egui::Id::new(("run-anything-focus", ui.memory(|memory| memory.focused())));
        self.double_ctrl_for_owner(ui, owner)
    }

    pub fn double_ctrl_for_owner(&mut self, ui: &egui::Ui, owner: egui::Id) -> bool {
        if !ui.input(|input| input.focused) {
            self.reset_ctrl_gesture();
            return false;
        }
        if self.ctrl_owner != Some(owner) {
            self.reset_ctrl_gesture();
            self.ctrl_owner = Some(owner);
        }
        let now = ui.input(|input| input.time);
        let mut fired = false;
        ui.input(|input| {
            for event in &input.events {
                match event {
                    egui::Event::Key {
                        key: key @ (egui::Key::ControlLeft | egui::Key::ControlRight),
                        pressed: true,
                        repeat: false,
                        modifiers,
                        ..
                    } => {
                        if self.ctrl_down.is_some()
                            || modifiers.shift
                            || modifiers.alt
                            || modifiers.mac_cmd
                        {
                            self.last_ctrl = None;
                            self.ctrl_clean = false;
                        } else {
                            self.ctrl_clean = true;
                        }
                        self.ctrl_down = Some(*key);
                    }
                    egui::Event::Key {
                        key: key @ (egui::Key::ControlLeft | egui::Key::ControlRight),
                        pressed: false,
                        ..
                    } => {
                        if self.ctrl_down == Some(*key) && self.ctrl_clean {
                            if self
                                .last_ctrl
                                .is_some_and(|last| now >= last && now - last < 0.35)
                            {
                                fired = true;
                                self.last_ctrl = None;
                            } else {
                                self.last_ctrl = Some(now);
                            }
                        } else {
                            self.last_ctrl = None;
                        }
                        self.ctrl_down = None;
                        self.ctrl_clean = false;
                    }
                    egui::Event::ModifiersChanged(modifiers)
                        if !modifiers.shift && !modifiers.alt && !modifiers.mac_cmd => {}
                    egui::Event::PointerMoved(_) | egui::Event::Screenshot { .. } => {}
                    _ => {
                        // Clipboard transport, text/IME, all other keys, clicks,
                        // wheel and focus transitions invalidate adjacency.
                        self.ctrl_clean = false;
                        self.last_ctrl = None;
                    }
                }
            }
        });
        fired
    }

    /// Keyboard handling for popups without a text field (choosers, Stop
    /// picker): consumes their keys so the editor does not see them. Returns
    /// true while any launcher popup is open (other shortcuts are skipped).
    pub fn intercept_keys(
        &mut self,
        ui: &mut egui::Ui,
        input: &LauncherInput,
    ) -> (bool, Vec<LauncherEvent>) {
        let mut events = Vec::new();
        let Some(popup) = &mut self.popup else {
            return (false, events);
        };
        let n = match popup {
            Popup::Chooser { .. } => chooser_items(input).len(),
            Popup::Stop { items, .. } => items.len() + 1,
            Popup::Anything(_) | Popup::Prompt { .. } => return (true, events),
        };
        let shift_pressed = ui.input(|i| {
            i.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::Key {
                        key: egui::Key::ShiftLeft | egui::Key::ShiftRight,
                        pressed: true,
                        repeat: false,
                        ..
                    }
                )
            })
        });
        let mut activate: Option<usize> = None;
        let mut close = false;
        ui.input_mut(|i| {
            let mut retained = Vec::new();
            for e in std::mem::take(&mut i.events) {
                match &e {
                    egui::Event::Key { key, pressed, .. } => {
                        if *pressed {
                            let selected = match popup {
                                Popup::Chooser { selected, .. } | Popup::Stop { selected, .. } => {
                                    selected
                                }
                                _ => unreachable!(),
                            };
                            match key {
                                egui::Key::ArrowDown => {
                                    *selected = (*selected + 1).min(n.saturating_sub(1))
                                }
                                egui::Key::ArrowUp => *selected = selected.saturating_sub(1),
                                egui::Key::Enter => activate = Some(*selected),
                                egui::Key::Escape => close = true,
                                k => {
                                    let digit = [
                                        egui::Key::Num1,
                                        egui::Key::Num2,
                                        egui::Key::Num3,
                                        egui::Key::Num4,
                                        egui::Key::Num5,
                                        egui::Key::Num6,
                                        egui::Key::Num7,
                                        egui::Key::Num8,
                                        egui::Key::Num9,
                                    ]
                                    .iter()
                                    .position(|d| d == k);
                                    if let Some(d) = digit.filter(|d| *d < n) {
                                        activate = Some(d);
                                    }
                                }
                            }
                        }
                    }
                    egui::Event::Text(_) => {}
                    _ => retained.push(e),
                }
            }
            i.events = retained;
        });
        if shift_pressed {
            if let Popup::Chooser { debug, .. } = popup {
                *debug = !*debug;
            }
        }
        if close {
            self.popup = None;
        } else if let Some(idx) = activate {
            if let Some(ev) = self.activate(idx, input) {
                events.push(ev);
            }
        }
        (true, events)
    }

    fn activate(&mut self, idx: usize, input: &LauncherInput) -> Option<LauncherEvent> {
        let popup = self.popup.take()?;
        match popup {
            Popup::Chooser { debug, .. } => {
                let mode = if debug { RunMode::Debug } else { RunMode::Run };
                match chooser_items(input).into_iter().nth(idx)? {
                    ChooserItem::CurrentFile => Some(LauncherEvent::LaunchCurrentFile(mode)),
                    ChooserItem::Config(name, _) => Some(LauncherEvent::Launch(name, mode)),
                    ChooserItem::Edit => Some(LauncherEvent::EditConfigurations),
                }
            }
            Popup::Stop { items, .. } => {
                if idx < items.len() {
                    Some(LauncherEvent::Stop(vec![items[idx].clone()]))
                } else {
                    Some(LauncherEvent::Stop(items))
                }
            }
            other => {
                self.popup = Some(other);
                None
            }
        }
    }

    /// Draws the open popup; returns what to launch.
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        theme: &UiTheme,
        input: &LauncherInput,
    ) -> Vec<LauncherEvent> {
        let mut events = Vec::new();
        let Some(popup) = self.popup.take() else {
            return events;
        };
        let keep = match popup {
            Popup::Anything(mut st) => {
                let keep = self.anything_ui(ctx, theme, input, &mut st, &mut events);
                keep.then_some(Popup::Anything(st))
            }
            Popup::Chooser { debug, selected } => {
                let mut clicked = None;
                let mut toggle = false;
                let items = chooser_items(input);
                let title = if debug { "Debug" } else { "Run" };
                let hint = if debug {
                    "Press Shift to switch to Run"
                } else {
                    "Press Shift to switch to Debug"
                };
                let closed = popup_frame(ctx, theme, "run_chooser", 420.0, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(title).strong().color(theme.text));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .small_button(if debug {
                                    "Switch to Run"
                                } else {
                                    "Switch to Debug"
                                })
                                .clicked()
                            {
                                toggle = true;
                            }
                            ui.label(egui::RichText::new(hint).color(theme.text_dim).small());
                        });
                    });
                    ui.separator();
                    // Many configurations scroll; the keyboard selection stays visible.
                    let list_height =
                        (items.len() as f32 * (24.0 + ui.spacing().item_spacing.y)).min(480.0);
                    let shown = &mut self.shown_selection;
                    egui::ScrollArea::vertical()
                        .max_height(480.0)
                        .min_scrolled_height(list_height)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for (idx, item) in items.iter().enumerate() {
                                let (label, detail) = match item {
                                    ChooserItem::CurrentFile => (
                                        format!(
                                            "Current File{}",
                                            input
                                                .current_file
                                                .as_ref()
                                                .map(|f| format!(" ({f})"))
                                                .unwrap_or_default()
                                        ),
                                        String::new(),
                                    ),
                                    ChooserItem::Config(name, kind) => {
                                        (name.clone(), kind.to_string())
                                    }
                                    ChooserItem::Edit => {
                                        ("Edit Configurations…".to_string(), String::new())
                                    }
                                };
                                let number = if idx < 9 {
                                    format!("{}", idx + 1)
                                } else {
                                    String::new()
                                };
                                let resp = row_response(
                                    ui,
                                    theme,
                                    idx == selected,
                                    &number,
                                    &label,
                                    &detail,
                                );
                                if idx == selected && *shown != Some(selected) {
                                    resp.scroll_to_me(None);
                                    *shown = Some(selected);
                                }
                                if resp.clicked() {
                                    clicked = Some(idx);
                                }
                            }
                        });
                });
                if closed {
                    None
                } else if let Some(idx) = clicked {
                    self.popup = Some(Popup::Chooser { debug, selected });
                    events.extend(self.activate(idx, input));
                    self.popup.take()
                } else {
                    Some(Popup::Chooser {
                        debug: debug ^ toggle,
                        selected,
                    })
                }
            }
            Popup::Stop { items, selected } => {
                let mut clicked = None;
                let closed = popup_frame(ctx, theme, "stop_picker", 380.0, |ui| {
                    ui.label(egui::RichText::new("Stop").strong().color(theme.text));
                    ui.separator();
                    for (idx, item) in items.iter().enumerate() {
                        let number = if idx < 9 {
                            format!("{}", idx + 1)
                        } else {
                            String::new()
                        };
                        if row(ui, theme, idx == selected, &number, &item.label(), "") {
                            clicked = Some(idx);
                        }
                    }
                    ui.separator();
                    let all = items.len();
                    if row(ui, theme, selected == all, "", "Stop All", "") {
                        clicked = Some(all);
                    }
                });
                if closed {
                    None
                } else if let Some(idx) = clicked {
                    self.popup = Some(Popup::Stop { items, selected });
                    events.extend(self.activate(idx, input));
                    self.popup.take()
                } else {
                    Some(Popup::Stop { items, selected })
                }
            }
            Popup::Prompt {
                cfg,
                mode,
                mut input,
                mut focus,
                replaces,
            } => {
                let mut done: Option<bool> = None;
                egui::Window::new("Enter Parameters")
                    .id(egui::Id::new("run_prompt"))
                    .collapsible(false)
                    .resizable(false)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .show_modal(ctx, |ui| {
                        ui.label(format!("Value for $Prompt$ in '{}':", cfg.name));
                        let resp =
                            ui.add(egui::TextEdit::singleline(&mut input).desired_width(380.0));
                        if focus {
                            resp.request_focus();
                            focus = false;
                        }
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            done = Some(true);
                        }
                        ui.horizontal(|ui| {
                            if ui.button("OK").clicked() {
                                done = Some(true);
                            }
                            if ui.button("Cancel").clicked()
                                || ui.input(|i| i.key_pressed(egui::Key::Escape))
                            {
                                done = Some(false);
                            }
                        });
                    });
                match done {
                    Some(true) => {
                        events.push(LauncherEvent::Prompted {
                            cfg,
                            mode,
                            answer: input,
                            replaces,
                        });
                        None
                    }
                    Some(false) => None,
                    None => Some(Popup::Prompt {
                        cfg,
                        mode,
                        input,
                        focus,
                        replaces,
                    }),
                }
            }
        };
        if self.popup.is_none() {
            self.popup = keep;
        }
        events
    }

    fn anything_items(&self, input: &LauncherInput, st: &mut AnythingState) -> Vec<AnythingItem> {
        let query = st.query.clone();
        let q = query.trim();
        let ql = q.to_lowercase();
        let mut items = Vec::new();
        let configs: Vec<AnythingItem> = input
            .configs
            .iter()
            .filter(|c| ql.is_empty() || c.name.to_lowercase().contains(&ql))
            .map(|c| AnythingItem::Config(c.name.clone(), c.kind_label()))
            .collect();
        if !q.is_empty() {
            items.extend(configs.iter().cloned());
            items.push(AnythingItem::Command(q.to_string()));
            items.extend(
                completions(input.root, st, q)
                    .into_iter()
                    .map(AnythingItem::Complete),
            );
        } else {
            items.extend(configs);
        }
        items.extend(
            self.history
                .iter()
                .filter(|h| h.as_str() != q && (ql.is_empty() || h.to_lowercase().contains(&ql)))
                .take(10)
                .map(|h| AnythingItem::Recent(h.clone())),
        );
        items
    }

    fn anything_ui(
        &mut self,
        ctx: &egui::Context,
        theme: &UiTheme,
        input: &LauncherInput,
        st: &mut AnythingState,
        events: &mut Vec<LauncherEvent>,
    ) -> bool {
        let items = self.anything_items(input, st);
        if st.query != st.last_query {
            st.selected = 0;
            st.last_query = st.query.clone();
        }
        st.selected = st.selected.min(items.len().saturating_sub(1));
        let mut keep = true;
        let mut activate: Option<(usize, bool)> = None;
        let mut complete: Option<String> = None;
        let closed = popup_frame(ctx, theme, "run_anything", 640.0, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Run Anything")
                        .strong()
                        .color(theme.text),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(
                            "Enter to run · Shift+Enter to debug · Tab to complete",
                        )
                        .color(theme.text_dim)
                        .small(),
                    );
                });
            });
            let resp = ui.add(egui::TextEdit::singleline(&mut st.query).id(egui::Id::new("run_anything_query")).desired_width(f32::INFINITY).hint_text("Run a configuration or a command: pytest -k name, python manage.py migrate, pip install …").font(egui::FontId::proportional(15.0)));
            if st.focus {
                resp.request_focus();
                st.focus = false;
            }
            let (up, down, enter, shift, tab) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::ArrowDown),
                    i.key_pressed(egui::Key::Enter),
                    i.modifiers.shift,
                    i.key_pressed(egui::Key::Tab),
                )
            });
            if down && st.selected + 1 < items.len() {
                st.selected += 1;
            }
            if up && st.selected > 0 {
                st.selected -= 1;
            }
            if enter {
                activate = Some((st.selected, shift));
            }
            if tab {
                if let Some(AnythingItem::Complete(c) | AnythingItem::Recent(c)) =
                    items.get(st.selected)
                {
                    complete = Some(c.clone());
                }
            }
            ui.separator();
            // The popup area offers only last frame's height, so size the list
            // from its rows (it would otherwise stay at the scroll minimum).
            let list_height =
                (items.len().max(1) as f32 * (24.0 + ui.spacing().item_spacing.y)).min(360.0);
            egui::ScrollArea::vertical()
                .max_height(360.0)
                .min_scrolled_height(list_height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    for (idx, item) in items.iter().enumerate() {
                        let (badge, label, detail) = match item {
                            AnythingItem::Config(name, kind) => {
                                ("▶", name.clone(), format!("{kind} configuration"))
                            }
                            AnythingItem::Command(c) => {
                                ("$", format!("Run '{c}'"), "command".to_string())
                            }
                            AnythingItem::Recent(c) => ("↺", c.clone(), "recent".to_string()),
                            AnythingItem::Complete(c) => ("…", c.clone(), "complete".to_string()),
                        };
                        let resp =
                            row_response(ui, theme, idx == st.selected, badge, &label, &detail);
                        if idx == st.selected && (up || down) {
                            resp.scroll_to_me(None);
                        }
                        if resp.clicked() {
                            activate = Some((idx, ui.input(|i| i.modifiers.shift)));
                        }
                    }
                    if items.is_empty() {
                        ui.label(
                            egui::RichText::new(
                                "Type a command or the name of a run configuration",
                            )
                            .color(theme.text_dim),
                        );
                    }
                });
        });
        if closed {
            return false;
        }
        if let Some(c) = complete {
            set_query(ctx, st, c);
        }
        if let Some((idx, debug)) = activate {
            match items.get(idx) {
                Some(AnythingItem::Config(name, _)) => {
                    events.push(LauncherEvent::Launch(
                        name.clone(),
                        if debug { RunMode::Debug } else { RunMode::Run },
                    ));
                    keep = false;
                }
                Some(AnythingItem::Command(c)) | Some(AnythingItem::Recent(c)) => {
                    events.push(LauncherEvent::Command {
                        line: c.clone(),
                        debug,
                    });
                    keep = false;
                }
                Some(AnythingItem::Complete(c)) => set_query(ctx, st, c.clone()),
                None => {}
            }
        }
        keep
    }
}

/// Replaces the Run Anything query and puts the cursor at its end.
fn set_query(ctx: &egui::Context, st: &mut AnythingState, text: String) {
    st.query = text;
    st.focus = true;
    let id = egui::Id::new("run_anything_query");
    if let Some(mut state) = egui::TextEdit::load_state(ctx, id) {
        let end = egui::text::CCursor::new(st.query.chars().count());
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::one(end)));
        state.store(ctx, id);
    }
}

/// `.py` completions for the last word after `python `.
fn completions(root: &Path, st: &mut AnythingState, query: &str) -> Vec<String> {
    let tokens: Vec<&str> = query.split(' ').collect();
    let Some(first) = tokens.first() else {
        return Vec::new();
    };
    if tokens.len() < 2 || !is_python(first) {
        return Vec::new();
    }
    let last = *tokens.last().unwrap_or(&"");
    if last.starts_with('-') || last.ends_with(".py") && root.join(last).is_file() {
        return Vec::new();
    }
    let files = st.py_files.get_or_insert_with(|| python_files(root));
    let prefix = &query[..query.len() - last.len()];
    let needle = last.to_lowercase();
    let mut matches: Vec<&String> = files
        .iter()
        .filter(|f| f.to_lowercase().contains(&needle))
        .collect();
    matches.sort_by_key(|f| (!f.to_lowercase().starts_with(&needle), f.len()));
    matches
        .into_iter()
        .take(12)
        .map(|f| format!("{prefix}{f}"))
        .collect()
}

/// Project `.py` files relative to the root (gitignore-aware, capped).
fn python_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .max_depth(Some(12))
        .filter_entry(|e| {
            !matches!(
                e.file_name().to_str(),
                Some(
                    "venv"
                        | ".venv"
                        | "env"
                        | "node_modules"
                        | "__pycache__"
                        | "site-packages"
                        | "build"
                        | "dist"
                )
            )
        })
        .build();
    for entry in walker.flatten() {
        if entry.file_type().is_some_and(|t| t.is_file())
            && entry.path().extension().is_some_and(|e| e == "py")
        {
            if let Ok(rel) = entry.path().strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
            if out.len() >= 5000 {
                break;
            }
        }
    }
    out.sort();
    out
}

#[derive(Clone, Debug, PartialEq)]
enum ChooserItem {
    CurrentFile,
    Config(String, &'static str),
    Edit,
}

fn chooser_items(input: &LauncherInput) -> Vec<ChooserItem> {
    let mut items = Vec::new();
    if input.current_file.is_some() {
        items.push(ChooserItem::CurrentFile);
    }
    items.extend(
        input
            .configs
            .iter()
            .map(|c| ChooserItem::Config(c.name.clone(), c.kind_label())),
    );
    items.push(ChooserItem::Edit);
    items
}

/// A popup frame anchored at the top center; returns true when it should
/// close (Escape or a click outside).
fn popup_frame(
    ctx: &egui::Context,
    theme: &UiTheme,
    id: &str,
    width: f32,
    add: impl FnOnce(&mut egui::Ui),
) -> bool {
    let area = egui::Area::new(egui::Id::new(id))
        .order(egui::Order::Foreground)
        .anchor(Align2::CENTER_TOP, Vec2::new(0.0, 90.0))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .fill(theme.panel_bg)
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.set_width(width);
                    add(ui);
                });
        });
    let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    let clicked_outside = ctx.input(|i| i.pointer.any_pressed())
        && ctx
            .input(|i| i.pointer.interact_pos())
            .is_some_and(|p| !area.response.rect.contains(p));
    esc || clicked_outside
}

fn row_response(
    ui: &mut egui::Ui,
    theme: &UiTheme,
    selected: bool,
    badge: &str,
    label: &str,
    detail: &str,
) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::click());
    if selected {
        ui.painter().rect_filled(rect, 3.0, theme.selection_bg);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 3.0, theme.hover_bg);
    }
    let fg = if selected { Color32::WHITE } else { theme.text };
    let dim = if selected {
        Color32::from_gray(220)
    } else {
        theme.text_dim
    };
    let y = rect.center().y;
    ui.painter().text(
        egui::Pos2::new(rect.left() + 10.0, y),
        Align2::LEFT_CENTER,
        badge,
        egui::FontId::proportional(12.0),
        dim,
    );
    let clip = ui
        .painter()
        .with_clip_rect(rect.shrink2(Vec2::new(4.0, 0.0)));
    clip.text(
        egui::Pos2::new(rect.left() + 30.0, y),
        Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(13.5),
        fg,
    );
    ui.painter().text(
        egui::Pos2::new(rect.right() - 8.0, y),
        Align2::RIGHT_CENTER,
        detail,
        egui::FontId::proportional(12.0),
        dim,
    );
    resp
}

fn row(
    ui: &mut egui::Ui,
    theme: &UiTheme,
    selected: bool,
    badge: &str,
    label: &str,
    detail: &str,
) -> bool {
    row_response(ui, theme, selected, badge, label, detail).clicked()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_context::tests::ScratchDir;

    fn ctrl(pressed: bool) -> egui::Event {
        egui::Event::Key {
            key: egui::Key::ControlLeft,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: if pressed {
                egui::Modifiers::CTRL
            } else {
                egui::Modifiers::NONE
            },
        }
    }
    fn gesture_frame(
        ctx: &egui::Context,
        launcher: &mut Launcher,
        time: f64,
        events: Vec<egui::Event>,
    ) -> bool {
        let mut fired = false;
        let output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                events,
                ..Default::default()
            },
            |ui| {
                fired = launcher.double_ctrl(ui);
            },
        );
        output.drop_without_applying_deltas();
        fired
    }
    #[test]
    fn run_anything_requires_two_completed_clean_ctrl_taps() {
        let ctx = egui::Context::default();
        let mut launcher = Launcher::default();
        assert!(!gesture_frame(&ctx, &mut launcher, 1.0, vec![ctrl(true)]));
        assert!(!gesture_frame(&ctx, &mut launcher, 1.02, vec![ctrl(false)]));
        assert!(
            !gesture_frame(&ctx, &mut launcher, 1.10, vec![ctrl(true)]),
            "second Ctrl press must stay available for held caret cloning"
        );
        assert!(gesture_frame(&ctx, &mut launcher, 1.369, vec![ctrl(false)]));
        assert!(!gesture_frame(
            &ctx,
            &mut launcher,
            2.0,
            vec![ctrl(true), ctrl(false)]
        ));
        assert!(!gesture_frame(
            &ctx,
            &mut launcher,
            2.351,
            vec![ctrl(true), ctrl(false)]
        ));
    }
    #[test]
    fn clipboard_transport_interrupts_run_anything_ctrl_taps() {
        for event in [
            egui::Event::Copy,
            egui::Event::Cut,
            egui::Event::Paste("π😀".into()),
        ] {
            let ctx = egui::Context::default();
            let mut launcher = Launcher::default();
            assert!(!gesture_frame(
                &ctx,
                &mut launcher,
                1.0,
                vec![ctrl(true), event, ctrl(false)]
            ));
            assert!(
                !gesture_frame(&ctx, &mut launcher, 1.1, vec![ctrl(true)]),
                "clipboard chord cannot prime the Run Anything launcher"
            );
            assert!(!gesture_frame(&ctx, &mut launcher, 1.12, vec![ctrl(false)]));
        }
    }
    #[test]
    fn held_second_ctrl_arrow_is_dirty_and_does_not_launch_on_release() {
        let ctx = egui::Context::default();
        let mut launcher = Launcher::default();
        assert!(!gesture_frame(
            &ctx,
            &mut launcher,
            1.0,
            vec![ctrl(true), ctrl(false)]
        ));
        assert!(!gesture_frame(&ctx, &mut launcher, 1.1, vec![ctrl(true)]));
        assert!(!gesture_frame(
            &ctx,
            &mut launcher,
            1.12,
            vec![egui::Event::Key {
                key: egui::Key::ArrowDown,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::CTRL,
            }]
        ));
        assert!(!gesture_frame(&ctx, &mut launcher, 1.14, vec![ctrl(false)]));
    }

    #[test]
    fn ctrl_tap_resets_on_window_focus_and_keyboard_owner_change() {
        let ctx = egui::Context::default();
        let mut launcher = Launcher::default();
        assert!(!gesture_frame(
            &ctx,
            &mut launcher,
            1.0,
            vec![ctrl(true), ctrl(false)]
        ));
        let output = ctx.run_ui(
            egui::RawInput {
                time: Some(1.05),
                focused: false,
                ..Default::default()
            },
            |ui| {
                assert!(!launcher.double_ctrl(ui));
            },
        );
        output.drop_without_applying_deltas();
        assert!(!gesture_frame(
            &ctx,
            &mut launcher,
            1.1,
            vec![ctrl(true), ctrl(false)]
        ));
        let mut field = String::new();
        for (time, expected) in [(1.2, false), (1.3, true)] {
            let output = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events: vec![ctrl(true), ctrl(false)],
                    ..Default::default()
                },
                |ui| {
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut field)
                            .id(egui::Id::new("another-input-owner")),
                    );
                    response.request_focus();
                    assert!(ui.memory(|memory| memory.has_focus(response.id)));
                    assert_eq!(launcher.double_ctrl(ui), expected);
                },
            );
            output.drop_without_applying_deltas();
        }
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn plans_commands() {
        let dir = ScratchDir::new("anything");
        let root = dir.0.as_path();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        assert_eq!(
            plan_command("pytest -k slow tests -x", root),
            Some(CommandPlan::Pytest {
                targets: s(&["tests"]),
                args: s(&["-k", "slow", "-x"])
            })
        );
        assert_eq!(
            plan_command("python -m pytest tests/test_a.py::test_b --maxfail 2", root),
            Some(CommandPlan::Pytest {
                targets: s(&["tests/test_a.py::test_b"]),
                args: s(&["--maxfail", "2"])
            })
        );
        assert_eq!(
            plan_command("python3 -X dev manage.py migrate --plan", root),
            Some(CommandPlan::Script {
                interpreter_options: s(&["-X", "dev"]),
                script: "manage.py".into(),
                args: s(&["migrate", "--plan"])
            })
        );
        assert_eq!(
            plan_command("manage.py runserver", root),
            Some(CommandPlan::Script {
                interpreter_options: Vec::new(),
                script: "manage.py".into(),
                args: s(&["runserver"])
            })
        );
        assert_eq!(
            plan_command("python -c \"print(1)\"", root),
            Some(CommandPlan::Process {
                program: None,
                args: s(&["-c", "print(1)"])
            })
        );
        assert_eq!(
            plan_command("pip install 'requests>=2'", root),
            Some(CommandPlan::Process {
                program: None,
                args: s(&["-m", "pip", "install", "requests>=2"])
            })
        );
        assert_eq!(
            plan_command("uv sync", root),
            Some(CommandPlan::Process {
                program: Some("uv".into()),
                args: s(&["sync"])
            })
        );
        assert_eq!(
            plan_command("/usr/bin/python3.12 x.py", root),
            Some(CommandPlan::Script {
                interpreter_options: Vec::new(),
                script: "x.py".into(),
                args: Vec::new()
            })
        );
        assert_eq!(plan_command("   ", root), None);
    }

    #[test]
    fn quoting_round_trips() {
        let args = s(&["plain", "two words", "say \"hi\"", "", "it's"]);
        assert_eq!(RunConfig::split_args(&quote_args(&args)), args);
    }

    #[test]
    fn history_is_persisted_most_recent_first() {
        let dir = ScratchDir::new("history");
        let mut l = Launcher::default();
        l.remember(&dir.0, "pytest -x");
        l.remember(&dir.0, "pip list");
        l.remember(&dir.0, "pytest -x");
        let mut fresh = Launcher::default();
        fresh.load_history(&dir.0);
        assert_eq!(fresh.history(), &s(&["pytest -x", "pip list"])[..]);
    }

    #[test]
    fn chooser_lists_current_file_configs_and_edit() {
        let configs = vec![
            RunConfig {
                name: "main".into(),
                ..Default::default()
            },
            RunConfig::for_pytest("tests", Path::new("/p")),
        ];
        let input = LauncherInput {
            root: Path::new("/p"),
            configs: &configs,
            selected_config: None,
            current_file: Some("app.py".into()),
        };
        let items = chooser_items(&input);
        assert_eq!(
            items,
            vec![
                ChooserItem::CurrentFile,
                ChooserItem::Config("main".into(), "Python"),
                ChooserItem::Config("pytest in tests".into(), "pytest"),
                ChooserItem::Edit
            ]
        );
        let mut l = Launcher::default();
        l.open_chooser(true, &input);
        assert_eq!(
            l.activate(1, &input),
            Some(LauncherEvent::Launch("main".into(), RunMode::Debug))
        );
        assert!(l.popup.is_none());
        l.open_chooser(false, &input);
        assert_eq!(
            l.activate(0, &input),
            Some(LauncherEvent::LaunchCurrentFile(RunMode::Run))
        );
        l.open_chooser(false, &input);
        assert_eq!(
            l.activate(3, &input),
            Some(LauncherEvent::EditConfigurations)
        );
        let stop = vec![StopItem::Session(3, "main".into()), StopItem::Tests];
        l.open_stop(stop.clone());
        assert_eq!(
            l.activate(1, &input),
            Some(LauncherEvent::Stop(vec![StopItem::Tests]))
        );
        l.open_stop(stop.clone());
        assert_eq!(l.activate(2, &input), Some(LauncherEvent::Stop(stop)));
    }

    #[test]
    fn anything_lists_configs_command_completions_and_history() {
        let dir = ScratchDir::new("complete");
        let root = dir.0.as_path();
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::write(root.join("app/manage.py"), "").unwrap();
        std::fs::write(root.join("main.py"), "").unwrap();
        let configs = vec![RunConfig {
            name: "Django server".into(),
            ..Default::default()
        }];
        let input = LauncherInput {
            root,
            configs: &configs,
            selected_config: None,
            current_file: None,
        };
        let mut l = Launcher::default();
        l.remember(root, "python main.py");
        l.open_anything(root);
        let Some(Popup::Anything(mut st)) = l.popup.take() else {
            panic!()
        };
        assert_eq!(
            l.anything_items(&input, &mut st),
            vec![
                AnythingItem::Config("Django server".into(), "Python"),
                AnythingItem::Recent("python main.py".into())
            ]
        );
        st.query = "python man".into();
        let items = l.anything_items(&input, &mut st);
        assert_eq!(
            items,
            vec![
                AnythingItem::Command("python man".into()),
                AnythingItem::Complete("python app/manage.py".into())
            ]
        );
        st.query = "django".into();
        assert_eq!(
            l.anything_items(&input, &mut st)[0],
            AnythingItem::Config("Django server".into(), "Python")
        );
    }
}
