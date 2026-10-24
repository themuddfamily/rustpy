//! Test Runner tool window (roadmap §3.4): tree of tests with live results,
//! failure details with navigation, rerun / rerun failed, captured output.

use crate::a11y::DialogWindow;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use egui::{Color32, Sense, Vec2};
use rustpy_test::{
    discover_file_in, discover_project, parse_failure_location, Framework, Outcome, Summary,
    TestEvent, TestItem, TestKind, TestResult, TestRun, TestRunOptions,
};

use crate::diff_view::SideBySide;
use crate::theme::UiTheme;
mod controls;
mod reports;
use reports::RunRecord;

pub enum TestPanelEvent {
    Navigate(PathBuf, usize),
    Status(String),
    /// Debug this test / class / file (node id) with the last run's settings.
    Debug(String),
    Measure {
        profile: bool,
        target: Option<String>,
    },
    FixtureUsages(PathBuf, usize),
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Node {
    id: String,
    name: String,
    kind: TestKind,
    path: Option<PathBuf>,
    line: Option<usize>,
    children: Vec<Node>,
}

impl Node {
    fn from_item(it: &TestItem) -> Self {
        Node {
            id: it.id.clone(),
            name: it.name.clone(),
            kind: it.kind,
            path: Some(it.path.clone()),
            line: Some(it.line),
            children: it.children.iter().map(Node::from_item).collect(),
        }
    }
}

/// Everything needed to repeat a run (Rerun, Rerun Failed, running a node).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LastRun {
    pub framework: Framework,
    pub interpreter: PathBuf,
    pub root: PathBuf,
    pub targets: Vec<String>,
    pub args: Vec<String>,
    /// Environment, working directory and interpreter options of the run configuration.
    pub options: TestRunOptions,
}

/// The "Click to see difference" window: `(test id, expected, actual, diff)`.
type DiffWindow = (String, String, String, SideBySide);

#[derive(Default)]
pub struct TestPanel {
    pub debugger_endpoints: Vec<(String, u16)>,
    run: Option<TestRun>,
    process_epoch: u64,
    accessibility_blocked: bool,
    tree_focus: Option<(egui::Id, u64)>,
    tree: Vec<Node>,
    results: HashMap<String, TestResult>,
    started: HashSet<String>,
    output: Vec<(String, bool)>,
    summary: Option<Summary>,
    error: Option<String>,
    selected: Option<String>,
    hide_passed: bool,
    show_output: bool,
    collapsed: HashSet<String>,
    last: Option<LastRun>,
    pub next_id: u64,
    total_planned: usize,
    diff: Option<DiffWindow>,
    transfer: Option<controls::Transfer>,
    pub view: rustpy_settings::testing::ViewOptions,
    filter: String,
    ignored: HashSet<String>,
    terminated: bool,
    scroll_to: Option<String>,
    statistics: bool,
    history: Vec<RunRecord>,
    history_root: Option<PathBuf>,
    report_root: Option<PathBuf>,
    archived: bool,
    repeat_count: usize,
    repeat_remaining: usize,
    repeat_until_failure: bool,
    pub auto_test: bool,
    auto_pending: Option<std::time::Instant>,
    completion_message: Option<String>,
    execution_blocked: bool,
    scroll_trace: bool,
    metadata: HashMap<String, rustpy_test::TestMetadata>,
    environment_filter: String,
    detecting: Option<((PathBuf, PathBuf), std::sync::mpsc::Receiver<Framework>)>,
    detected: Option<((PathBuf, PathBuf), Framework)>,
}

/// `args` without the options that select tests: `-k EXPR` / `-kEXPR` and,
/// for pytest, the marker expression `-m EXPR` / `-mEXPR` (such a filter
/// could deselect explicitly chosen tests; behave's `-m` is a flag).
fn without_selection_filters(args: &[String], framework: Framework) -> Vec<String> {
    let options: &[&str] = if framework == Framework::Pytest {
        &["-k", "-m"]
    } else {
        &["-k"]
    };
    let mut out = Vec::new();
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
        } else if options.contains(&a.as_str()) {
            skip = true;
        } else if !options.iter().any(|o| a.starts_with(o) && a.len() > 2) {
            out.push(a.clone());
        }
    }
    out
}

impl TestPanel {
    pub(crate) fn service_id(&self) -> Option<u64> {
        (self.process_epoch != 0).then_some(self.process_epoch)
    }
    pub(crate) fn set_accessibility_owner(&mut self, available: bool) {
        self.accessibility_blocked = !available;
    }
    pub(crate) fn claims_tree_menu(
        &self,
        ctx: &egui::Context,
        key: egui::Key,
        mods: egui::Modifiers,
    ) -> bool {
        key == egui::Key::F10
            && mods == egui::Modifiers::SHIFT
            && !egui::Popup::is_any_open(ctx)
            && self.tree_focus.is_some_and(|(id, epoch)| {
                epoch == self.next_id && ctx.memory(|memory| memory.has_focus(id))
            })
    }

    /// Observational identity of the retained process owner; does not poll it.
    pub(crate) fn quit_process_state(&self) -> String {
        format!(
            "{:?}",
            (
                self.process_epoch,
                self.run.as_ref().map(|owner| owner as *const _)
            )
        )
    }
    pub(crate) fn stop_quit_process(&mut self) {
        self.stop();
    }

    pub fn is_running(&self) -> bool {
        self.run.as_ref().is_some_and(|r| r.is_running())
    }

    /// Result consumed by the explicit pre-commit test check.
    pub fn commit_check_result(&self) -> Option<Result<(), String>> {
        if self.is_running() {
            return None;
        }
        if let Some(error) = &self.error {
            return Some(Err(error.clone()));
        }
        Some(
            self.summary
                .as_ref()
                .map(|summary| {
                    if summary.failed == 0
                        && summary.errors == 0
                        && summary.exit_code == Some(0)
                        && summary.total() > 0
                    {
                        Ok(())
                    } else {
                        Err(format!(
                            "Commit test check failed: {} failures, {} errors, exit {:?}",
                            summary.failed, summary.errors, summary.exit_code
                        ))
                    }
                })
                .unwrap_or_else(|| {
                    Err("Commit test check stopped without a completed summary".into())
                }),
        )
    }

    pub fn has_run(&self) -> bool {
        self.last.is_some()
    }

    /// The settings of the last run (for debugging one of its tests).
    pub fn last_run(&self) -> Option<&LastRun> {
        self.last.as_ref()
    }

    /// Starts a test run for `targets` (node ids / paths; empty = whole project).
    pub fn start(
        &mut self,
        framework: Framework,
        interpreter: &Path,
        root: &Path,
        targets: Vec<String>,
        extra_args: Vec<String>,
    ) -> Result<(), String> {
        self.start_with(
            framework,
            interpreter,
            root,
            targets,
            extra_args,
            TestRunOptions::default(),
        )
    }

    /// Runs the given node ids again with the last run's framework, interpreter, options and arguments (minus `-k` / pytest's `-m`).
    pub fn run_nodes(&mut self, ids: Vec<String>) -> Result<(), String> {
        let Some(last) = self.last.clone() else {
            return Err("No previous test run".into());
        };
        let args = without_selection_filters(&last.args, last.framework);
        let ids = ids
            .into_iter()
            .map(|id| {
                id.split_once("::subTest")
                    .map(|(parent, _)| parent.to_string())
                    .unwrap_or(id)
            })
            .collect();
        self.start_with(
            last.framework,
            &last.interpreter,
            &last.root,
            ids,
            args,
            last.options,
        )
    }

    /// [`TestPanel::start`] with the environment, working directory and
    /// interpreter options of a run configuration; Rerun and Rerun Failed keep them.
    pub fn start_with(
        &mut self,
        framework: Framework,
        interpreter: &Path,
        root: &Path,
        targets: Vec<String>,
        mut extra_args: Vec<String>,
        options: TestRunOptions,
    ) -> Result<(), String> {
        if self.execution_blocked {
            return Err("Safe mode: trust the project before running tests".into());
        }
        if let Some(r) = &mut self.run {
            r.stop();
        }
        self.load_history_for(root);
        self.report_root = Some(root.to_path_buf());
        let requested_args = extra_args.clone();
        self.auto_pending = None;
        if framework == Framework::Pytest {
            for id in &self.ignored {
                if !extra_args
                    .windows(2)
                    .any(|pair| pair[0] == "--deselect" && pair[1] == *id)
                {
                    extra_args.extend(["--deselect".into(), id.clone()]);
                }
            }
        }
        self.archived = false;
        self.terminated = false;
        self.debugger_endpoints.clear();
        self.results.clear();
        self.metadata.clear();
        self.started.clear();
        self.output.clear();
        self.summary = None;
        self.error = None;
        self.selected = None;
        self.tree = Vec::new();
        if targets.is_empty() {
            self.tree = discover_project(root).iter().map(Node::from_item).collect();
        } else {
            for t in &targets {
                let nodes = planned_nodes(root, t);
                merge_nodes(&mut self.tree, nodes);
            }
        }
        self.total_planned = self.tree.iter().map(count_leaves).sum();
        self.next_id += 1;
        self.diff = None;
        match TestRun::start_with(
            self.next_id,
            framework,
            interpreter,
            root,
            &targets,
            &extra_args,
            &options,
        ) {
            Ok(run) => {
                self.output.push((format!("$ {}", run.command_line), false));
                self.process_epoch = crate::app::window_lifecycle::next_process_owner();
                self.run = Some(run);
                self.last = Some(LastRun {
                    framework,
                    interpreter: interpreter.to_path_buf(),
                    root: root.to_path_buf(),
                    targets,
                    args: requested_args,
                    options,
                });
                Ok(())
            }
            Err(e) => {
                self.error = Some(e.to_string());
                Err(e.to_string())
            }
        }
    }

    pub fn rerun(&mut self) -> Result<(), String> {
        let Some(last) = self.last.clone() else {
            return Err("No previous test run".into());
        };
        self.start_with(
            last.framework,
            &last.interpreter,
            &last.root,
            last.targets,
            last.args,
            last.options,
        )
    }

    pub fn rerun_failed(&mut self) -> Result<(), String> {
        let Some(last) = self.last.clone() else {
            return Err("No previous test run".into());
        };
        let failed = self.failed_targets();
        if failed.is_empty() {
            return Err("No failed tests to rerun".into());
        }
        self.start_with(
            last.framework,
            &last.interpreter,
            &last.root,
            failed,
            without_selection_filters(&last.args, last.framework),
            last.options,
        )
    }

    pub fn stop(&mut self) {
        self.terminated = true;
        self.repeat_remaining = 0;
        self.repeat_until_failure = false;
        self.auto_pending = None;
        if let Some(r) = &mut self.run {
            r.stop();
        }
    }

    /// Drains runner events; returns true when something changed.
    pub fn poll(&mut self) -> bool {
        let events = self.run.as_mut().map(|run| run.poll()).unwrap_or_default();
        let changed = self.apply(events);
        self.finish_completed_run();
        if !self.is_running()
            && self
                .auto_pending
                .is_some_and(|time| time.elapsed() >= std::time::Duration::from_millis(700))
        {
            self.auto_pending = None;
            if let Err(error) = self.rerun() {
                self.error = Some(error);
            }
            return true;
        }
        changed
    }

    /// Applies runner events; returns true when there were any.
    fn apply(&mut self, events: Vec<TestEvent>) -> bool {
        if events.is_empty() {
            return false;
        }
        for ev in events {
            match ev {
                TestEvent::Debugger { host, port } => self.debugger_endpoints.push((host, port)),
                TestEvent::Collected {
                    id,
                    name,
                    path,
                    line,
                } => {
                    if find_node(&self.tree, &id).is_none() {
                        self.insert_collected(id, name, path, line);
                    }
                }
                TestEvent::Started(id) => {
                    if self.view.track_running {
                        self.selected = Some(id.clone());
                        if self.view.autoscroll {
                            self.scroll_to = Some(id.clone());
                        }
                        self.expand_ancestors(&id);
                    }
                    self.started.insert(id);
                }
                TestEvent::Result(r) => {
                    if find_node(&self.tree, &r.id).is_none() {
                        self.insert_collected(
                            r.id.clone(),
                            r.id.rsplit("::").next().unwrap_or(&r.id).to_string(),
                            None,
                            None,
                        );
                    }
                    // Keep the worst outcome for duplicated ids (teardown errors).
                    let replace = match self.results.get(&r.id) {
                        Some(prev) => r.outcome.is_problem() || !prev.outcome.is_problem(),
                        None => true,
                    };
                    if replace {
                        if self.view.select_first_failure
                            && self.selected.is_none()
                            && r.outcome.is_problem()
                        {
                            self.selected = Some(r.id.clone());
                        }
                        self.results.insert(r.id.clone(), r);
                    }
                }
                TestEvent::Metadata(metadata) => {
                    self.metadata.insert(metadata.id.clone(), metadata);
                }
                TestEvent::Output(line, err) => self.output.push((line, err)),
                TestEvent::Done(s) => {
                    // Nothing ran ("ERROR: not found", nothing collected): the output says why.
                    if !s.is_success() && s.total() == 0 {
                        self.show_output = true;
                    }
                    if self.view.select_first_failure {
                        if let Some(id) = self.first_failure_id() {
                            self.selected = Some(id.clone());
                            self.scroll_to = Some(id.clone());
                            self.expand_ancestors(&id);
                        }
                    }
                    self.summary = Some(s);
                }
                TestEvent::Failed(e) => self.error = Some(e),
                TestEvent::Exited(code) => {
                    if code < 0 {
                        self.terminated = true;
                    }
                }
            }
        }
        true
    }

    fn insert_collected(
        &mut self,
        id: String,
        name: String,
        path: Option<PathBuf>,
        line: Option<usize>,
    ) {
        // Attach parametrized ids / unknown tests under the closest parent by prefix.
        let base = id.split('[').next().unwrap_or(&id).to_string();
        let parent_id = if base != id {
            Some(base.clone())
        } else {
            base.rsplit_once("::").map(|(p, _)| p.to_string())
        };
        let node = Node {
            id: id.clone(),
            name,
            kind: TestKind::Function,
            path,
            line,
            children: Vec::new(),
        };
        if let Some(pid) = parent_id {
            if let Some(parent) = find_node_mut(&mut self.tree, &pid) {
                parent.children.push(node);
                return;
            }
        }
        let file = id.split("::").next().unwrap_or(&id).to_string();
        if let Some(parent) = find_node_mut(&mut self.tree, &file) {
            parent.children.push(node);
        } else {
            self.tree.push(Node {
                id: file.clone(),
                name: file,
                kind: TestKind::Module,
                path: None,
                line: None,
                children: vec![node],
            });
        }
    }

    fn status_of(&self, node: &Node) -> Status {
        if self.ignored.iter().any(|id| {
            node.id == *id
                || node.id.starts_with(&format!("{id}::"))
                || node.id.starts_with(&format!("{id}["))
        }) {
            return Status::Ignored;
        }
        if node.children.is_empty() && self.view.inline_statistics {
            if let Some(r) = self.results.get(&node.id) {
                return match r.outcome {
                    Outcome::Passed => Status::Passed,
                    Outcome::Failed => Status::Failed,
                    Outcome::Error => Status::Error,
                    Outcome::Skipped => Status::Skipped,
                };
            }
            // A parametrized function is a parent in results space.
            let prefix = format!("{}[", node.id);
            let mut st = Status::NotRun;
            for r in self.results.values().filter(|r| r.id.starts_with(&prefix)) {
                st = st.merge(match r.outcome {
                    Outcome::Passed => Status::Passed,
                    Outcome::Failed => Status::Failed,
                    Outcome::Error => Status::Error,
                    Outcome::Skipped => Status::Skipped,
                });
            }
            if st == Status::NotRun && self.started.contains(&node.id) {
                return if self.terminated {
                    Status::Terminated
                } else {
                    Status::Running
                };
            }
            return st;
        }
        let mut st = Status::NotRun;
        for c in &node.children {
            st = st.merge(self.status_of(c));
        }
        st
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        mono: egui::FontId,
    ) -> Vec<TestPanelEvent> {
        let mut events = Vec::new();
        let running = self.is_running();
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !running && self.last.is_some(),
                    egui::Button::new("▶ Rerun"),
                )
                .on_hover_text("Rerun all tests of the last run")
                .clicked()
            {
                if let Err(e) = self.rerun() {
                    events.push(TestPanelEvent::Status(e));
                }
            }
            if ui
                .add_enabled(
                    !running && self.results.values().any(|r| r.outcome.is_problem()),
                    egui::Button::new("↻ Rerun Failed"),
                )
                .clicked()
            {
                if let Err(e) = self.rerun_failed() {
                    events.push(TestPanelEvent::Status(e));
                }
            }
            if ui
                .add_enabled(
                    running,
                    egui::Button::new(egui::RichText::new("■ Stop").color(theme.error)),
                )
                .clicked()
            {
                self.stop();
            }
            ui.separator();
            ui.toggle_value(&mut self.hide_passed, "Hide passed");
            ui.toggle_value(&mut self.show_output, "Output");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (text, color) = self.summary_text(theme);
                ui.label(egui::RichText::new(text).color(color).strong());
                if let Some(run) = &self.run {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} · {:.1}s",
                            run.framework.label(),
                            run.elapsed().as_secs_f32()
                        ))
                        .color(theme.text_dim),
                    );
                }
            });
        });
        self.controls_ui(ui, theme, &mut events);
        // Progress bar.
        let done = self.results.len();
        let planned = self.total_planned.max(done).max(1);
        let frac = if self.summary.is_some() {
            1.0
        } else {
            done as f32 / planned as f32
        };
        let bar_color = if self.terminated {
            theme.warning
        } else if self.has_problems() {
            theme.error
        } else {
            theme.success
        };
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 4.0), Sense::hover());
        ui.painter().rect_filled(rect, 0.0, theme.hover_bg);
        ui.painter().rect_filled(
            egui::Rect::from_min_size(rect.min, Vec2::new(rect.width() * frac, 4.0)),
            0.0,
            bar_color,
        );
        if let Some(e) = &self.error {
            ui.label(egui::RichText::new(e).color(theme.error));
        }
        ui.separator();
        if self.show_output {
            egui::Panel::bottom("tests_output")
                .resizable(true)
                .default_size(140.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("tests_output_scroll")
                        .auto_shrink([false, false])
                        .stick_to_bottom(self.view.autoscroll)
                        .show(ui, |ui| {
                            for (line, err) in &self.output {
                                ui.label(
                                    egui::RichText::new(line).font(mono.clone()).color(if *err {
                                        theme.error
                                    } else {
                                        theme.text
                                    }),
                                );
                            }
                        });
                });
        }
        egui::Panel::left("tests_tree")
            .resizable(true)
            .default_size(380.0)
            .min_size(200.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("tests_tree_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let mut tree = self.tree.clone();
                        self.sort_nodes(&mut tree);
                        if tree.is_empty() {
                            ui.label(
                                egui::RichText::new(if running {
                                    "Collecting tests…"
                                } else {
                                    "No tests. Right-click a test file or use Run → Run All Tests."
                                })
                                .color(theme.text_dim),
                            );
                        }
                        for n in &tree {
                            self.node_ui(ui, theme, n, 0, &mut events);
                        }
                    });
            });
        // Details of the selected test.
        egui::ScrollArea::vertical()
            .id_salt("tests_details")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.statistics {
                    self.statistics_ui(ui, theme, &mut events);
                    return;
                }
                let Some(id) = self.selected.clone() else {
                    ui.label(
                        egui::RichText::new("Select a test to see its output")
                            .color(theme.text_dim),
                    );
                    return;
                };
                ui.label(egui::RichText::new(&id).color(theme.text).strong());
                if let Some(metadata) = self.metadata.get(&id).cloned() {
                    if !metadata.markers.is_empty() {
                        ui.label(format!("Markers: {}", metadata.markers.join(", ")));
                    }
                    if let Some(parameter) = &metadata.parameter_id {
                        ui.label(format!("Parameter: {parameter}"));
                    }
                    for (name, path, line) in &metadata.fixtures {
                        ui.horizontal(|ui| {
                            if ui.link(format!("Fixture: {name}")).clicked() {
                                events.push(TestPanelEvent::Navigate(path.clone(), *line));
                            }
                            if ui.small_button("Usages").clicked() {
                                events.push(TestPanelEvent::FixtureUsages(path.clone(), *line));
                            }
                        });
                    }
                    if !metadata.hypothesis.is_empty() {
                        ui.label(format!("Hypothesis: {}", metadata.hypothesis));
                    }
                    if !metadata.logs.is_empty() {
                        ui.collapsing("Captured logs", |ui| {
                            ui.label(egui::RichText::new(&metadata.logs).font(mono.clone()));
                        });
                    }
                }
                match self.results.get(&id).cloned() {
                    Some(r) => {
                        let (label, color) = outcome_style(r.outcome, theme);
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(label).color(color).strong());
                            ui.label(
                                egui::RichText::new(format!("{:.3}s", r.duration))
                                    .color(theme.text_dim),
                            );
                            if !r.traceback.is_empty()
                                && ui.small_button("Scroll to stack trace").clicked()
                            {
                                self.scroll_trace = true;
                            }
                            if ui.small_button("Copy failure").clicked() {
                                ui.ctx()
                                    .copy_text(format!("{}\n{}\n{}", r.id, r.message, r.traceback));
                            }
                            if r.xfail {
                                ui.label(egui::RichText::new("xfail").color(theme.text_dim));
                            }
                            let root = self
                                .last
                                .as_ref()
                                .map(|l| l.root.clone())
                                .or_else(|| self.report_root.clone())
                                .unwrap_or_default();
                            if let Some((p, l)) = parse_failure_location(&r.traceback, &root) {
                                if ui.small_button("Jump to failure").clicked() {
                                    events.push(TestPanelEvent::Navigate(p, l));
                                }
                            }
                        });
                        if !r.message.is_empty() {
                            ui.label(egui::RichText::new(&r.message).font(mono.clone()).color(
                                if r.outcome.is_problem() {
                                    theme.error
                                } else {
                                    theme.text
                                },
                            ));
                        }
                        if let Some((expected, actual)) = r.comparison() {
                            let link = egui::RichText::new("<Click to see difference>")
                                .color(theme.accent)
                                .underline();
                            if ui
                                .add(egui::Label::new(link).sense(Sense::click()))
                                .on_hover_text(
                                    "Compare the expected and actual values side by side",
                                )
                                .clicked()
                            {
                                self.diff = Some((
                                    id.clone(),
                                    expected.to_string(),
                                    actual.to_string(),
                                    SideBySide::new(expected, actual),
                                ));
                            }
                        }
                        if !r.traceback.is_empty() {
                            let trace_start = ui.label(egui::RichText::new("Stack trace").strong());
                            if self.scroll_trace {
                                trace_start.scroll_to_me(Some(egui::Align::TOP));
                                self.scroll_trace = false;
                            }
                            ui.separator();
                            let root = self
                                .last
                                .as_ref()
                                .map(|l| l.root.clone())
                                .or_else(|| self.report_root.clone())
                                .unwrap_or_default();
                            for line in r.traceback.lines() {
                                let trimmed = line.trim();
                                let link = trimmed
                                    .strip_prefix("File \"")
                                    .and_then(|rest| rest.split_once("\", line "))
                                    .and_then(|(f, rest)| {
                                        rest.split(',')
                                            .next()
                                            .and_then(|n| n.trim().parse::<usize>().ok())
                                            .map(|n| (f.to_string(), n))
                                    })
                                    .or_else(|| {
                                        trimmed.rsplit_once(':').and_then(|(f, n)| {
                                            n.trim()
                                                .parse::<usize>()
                                                .ok()
                                                .filter(|_| f.ends_with(".py"))
                                                .map(|n| (f.to_string(), n))
                                        })
                                    });
                                match link {
                                    Some((f, n)) => {
                                        let text = egui::RichText::new(line)
                                            .font(mono.clone())
                                            .color(theme.accent)
                                            .underline();
                                        if ui
                                            .add(egui::Label::new(text).sense(Sense::click()))
                                            .clicked()
                                        {
                                            let p = if Path::new(&f).is_absolute() {
                                                PathBuf::from(&f)
                                            } else {
                                                root.join(&f)
                                            };
                                            events.push(TestPanelEvent::Navigate(p, n));
                                        }
                                    }
                                    None => {
                                        ui.label(
                                            egui::RichText::new(line)
                                                .font(mono.clone())
                                                .color(theme.text),
                                        );
                                    }
                                }
                            }
                        }
                        if !r.stdout.is_empty() {
                            ui.separator();
                            ui.label(
                                egui::RichText::new("Captured stdout")
                                    .color(theme.text_dim)
                                    .small(),
                            );
                            ui.label(
                                egui::RichText::new(r.stdout.trim_end())
                                    .font(mono.clone())
                                    .color(theme.text),
                            );
                        }
                        if !r.stderr.is_empty() {
                            ui.separator();
                            ui.label(
                                egui::RichText::new("Captured stderr")
                                    .color(theme.text_dim)
                                    .small(),
                            );
                            ui.label(
                                egui::RichText::new(r.stderr.trim_end())
                                    .font(mono.clone())
                                    .color(theme.error),
                            );
                        }
                    }
                    None => {
                        ui.label(
                            egui::RichText::new(if running { "Running…" } else { "Not run" })
                                .color(theme.text_dim),
                        );
                    }
                }
            });
        self.diff_window(ui.ctx(), theme, mono);
        events
    }

    /// The "Comparison Failure" window opened from "<Click to see difference>".
    fn diff_window(&mut self, ctx: &egui::Context, theme: &UiTheme, mono: egui::FontId) {
        let Some((id, expected, actual, diff)) = &self.diff else {
            return;
        };
        let mut open = true;
        let mut close = false;
        egui::Window::new(format!(
            "Comparison Failure: {}",
            id.rsplit("::").next().unwrap_or(id)
        ))
        .id(egui::Id::new("test_comparison_failure"))
        .collapsible(false)
        .resizable(true)
        .default_size([860.0, 480.0])
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .open(&mut open)
        .show_modeless(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(id).color(theme.text_dim).small());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                    if ui.button("Copy Actual").clicked() {
                        ui.ctx().copy_text(actual.clone());
                    }
                    if ui.button("Copy Expected").clicked() {
                        ui.ctx().copy_text(expected.clone());
                    }
                    let n = diff.changes;
                    ui.label(
                        egui::RichText::new(if n == 0 {
                            "No textual difference (the values differ in type or identity)"
                                .to_string()
                        } else {
                            format!("{n} changed line{}", if n == 1 { "" } else { "s" })
                        })
                        .color(theme.text_dim),
                    );
                });
            });
            ui.separator();
            diff.ui(ui, theme, mono, "Expected", "Actual");
        });
        if !open || close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.diff = None;
        }
    }

    fn node_ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        node: &Node,
        depth: usize,
        events: &mut Vec<TestPanelEvent>,
    ) {
        let status = self.status_of(node);
        if !self.node_visible(node) {
            return;
        }
        let (icon, color) = status.style(theme);
        let collapsed = self.collapsed.contains(&node.id);
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 20.0), Sense::hover());
        let id = ui.id().with(("test-node", self.next_id, &node.id));
        let resp = ui.interact(rect, id, Sense::click());
        if self.scroll_to.as_deref() == Some(node.id.as_str()) {
            resp.scroll_to_me(Some(egui::Align::Center));
            self.scroll_to = None;
        }
        let available = !self.accessibility_blocked;
        if resp.has_focus() {
            self.tree_focus = Some((id, self.next_id));
        }
        let status_name = match status {
            Status::NotRun => "Not run",
            Status::Running => "Running",
            Status::Passed => "Passed",
            Status::Skipped => "Skipped",
            Status::Failed => "Failed",
            Status::Error => "Error",
            Status::Ignored => "Ignored",
            Status::Terminated => "Terminated",
        };
        let label = format!(
            "{}. {}",
            node.name.chars().take(160).collect::<String>(),
            status_name
        );
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, available, &label));
        // The node builder owns the Context write lock: snapshot focus before
        // entering it rather than re-entering Context through the Response.
        let focused = resp.has_focus();
        ui.ctx().accesskit_node_builder(id, |item| {
            item.set_role(egui::accesskit::Role::TreeItem);
            item.set_selected(focused);
            if !node.children.is_empty() {
                item.set_expanded(!collapsed);
            }
        });
        let (left, right) = if available && resp.has_focus() && !egui::Popup::is_any_open(ui.ctx())
        {
            crate::a11y::announce(ui.ctx(), id, &format!("{label}. Left collapses, Right expands, Shift F10 opens Run, Debug and source actions."));
            ui.input_mut(|input| {
                (
                    input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft),
                    input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight),
                )
            })
        } else {
            (false, false)
        };
        // Consuming a tree arrow must also cancel egui's queued directional
        // traversal, so the next menu command still belongs to this row.
        if left || right {
            ui.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
        }
        if !node.children.is_empty() {
            if left {
                self.collapsed.insert(node.id.clone());
            }
            if right {
                self.collapsed.remove(&node.id);
            }
        }
        let selected = self.selected.as_deref() == Some(node.id.as_str());
        if selected {
            ui.painter().rect_filled(rect, 2.0, theme.selection_bg);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, 2.0, theme.hover_bg);
        }
        let x = rect.left() + 6.0 + depth as f32 * 16.0;
        let y = rect.center().y;
        if !node.children.is_empty() {
            ui.painter().text(
                egui::Pos2::new(x, y),
                egui::Align2::LEFT_CENTER,
                if collapsed { "▶" } else { "▼" },
                egui::FontId::proportional(9.0),
                theme.text_dim,
            );
        }
        ui.painter().text(
            egui::Pos2::new(x + 14.0, y),
            egui::Align2::LEFT_CENTER,
            icon,
            egui::FontId::proportional(13.0),
            color,
        );
        let name_color = if selected {
            Color32::WHITE
        } else if node.kind == TestKind::Module {
            theme.text_dim
        } else {
            theme.text
        };
        let galley = ui.fonts_mut(|f| {
            f.layout_no_wrap(
                node.name.clone(),
                egui::FontId::proportional(13.0),
                name_color,
            )
        });
        ui.painter().galley(
            egui::Pos2::new(x + 32.0, y - galley.size().y / 2.0),
            galley.clone(),
            name_color,
        );
        if node.children.is_empty() {
            if let Some(r) = self.results.get(&node.id) {
                ui.painter().text(
                    egui::Pos2::new(rect.right() - 8.0, y),
                    egui::Align2::RIGHT_CENTER,
                    format!("{} ms", (r.duration * 1000.0).round() as u64),
                    egui::FontId::proportional(11.0),
                    theme.text_dim,
                );
            }
        } else if self.view.inline_statistics {
            let (p, total) = (count_status(self, node, Status::Passed), count_leaves(node));
            ui.painter().text(
                egui::Pos2::new(rect.right() - 8.0, y),
                egui::Align2::RIGHT_CENTER,
                format!("{p}/{total}"),
                egui::FontId::proportional(11.0),
                theme.text_dim,
            );
        }
        if available && resp.clicked() {
            if !node.children.is_empty()
                && resp.clicked_by(egui::PointerButton::Primary)
                && ui.input(|i| i.pointer.interact_pos().is_some_and(|p| p.x < x + 14.0))
            {
                if collapsed {
                    self.collapsed.remove(&node.id);
                } else {
                    self.collapsed.insert(node.id.clone());
                }
            } else {
                self.selected = Some(node.id.clone());
            }
        }
        let source = || -> Option<(PathBuf, usize)> {
            if let (Some(p), Some(l)) = (&node.path, node.line) {
                let path = if p.is_absolute() {
                    p.clone()
                } else {
                    self.report_root
                        .as_ref()
                        .map(|root| root.join(p))
                        .unwrap_or_else(|| p.clone())
                };
                return Some((path, l));
            }
            let root = self
                .last
                .as_ref()
                .map(|l| l.root.clone())
                .or_else(|| self.report_root.clone())
                .unwrap_or_default();
            self.results
                .get(&node.id)
                .and_then(|r| parse_failure_location(&r.traceback, &root))
        };
        let source = source();
        if available && resp.double_clicked() {
            if let Some((p, l)) = source.clone() {
                events.push(TestPanelEvent::Navigate(p, l));
            }
        }
        let mut run_node = false;
        crate::a11y::keyboard_context_menu(&resp, available).show(|ui| {
            let has_run = self.last.is_some();
            let debuggable = self
                .last
                .as_ref()
                .is_some_and(|l| matches!(l.framework, Framework::Pytest | Framework::Unittest));
            if ui
                .add_enabled(has_run, egui::Button::new(format!("▶ Run '{}'", node.name)))
                .clicked()
            {
                run_node = true;
                ui.close();
            }
            if ui
                .add_enabled(
                    debuggable,
                    egui::Button::new(format!("🐞 Debug '{}'", node.name)),
                )
                .on_disabled_hover_text("Debugging is available for pytest and unittest runs")
                .clicked()
            {
                events.push(TestPanelEvent::Debug(node.id.clone()));
                ui.close();
            }
            if ui
                .add_enabled(
                    self.last
                        .as_ref()
                        .is_some_and(|last| last.framework == Framework::Pytest),
                    egui::Button::new(if self.ignored.contains(&node.id) {
                        "Stop ignoring test"
                    } else {
                        "Ignore on pytest reruns"
                    }),
                )
                .clicked()
            {
                if !self.ignored.remove(&node.id) {
                    self.ignored.insert(node.id.clone());
                }
                ui.close();
            }
            if ui.button("Run with Coverage").clicked() {
                events.push(TestPanelEvent::Measure {
                    profile: false,
                    target: Some(node.id.clone()),
                });
                ui.close();
            }
            if ui.button("Profile").clicked() {
                events.push(TestPanelEvent::Measure {
                    profile: true,
                    target: Some(node.id.clone()),
                });
                ui.close();
            }
            ui.separator();
            if ui
                .add_enabled(source.is_some(), egui::Button::new("Jump to Source"))
                .clicked()
            {
                if let Some((p, l)) = source.clone() {
                    events.push(TestPanelEvent::Navigate(p, l));
                }
                ui.close();
            }
        });
        if run_node {
            if let Err(e) = self.run_nodes(vec![node.id.clone()]) {
                events.push(TestPanelEvent::Status(e));
            }
            return;
        }
        if !collapsed {
            for c in &node.children {
                self.node_ui(ui, theme, c, depth + 1, events);
            }
        }
    }

    /// Whether the run so far failed: a failed test, a runner error, or a
    /// finished run that was not a success (one that ran no test included).
    fn has_problems(&self) -> bool {
        self.results.values().any(|r| r.outcome.is_problem())
            || self.error.is_some()
            || self.summary.as_ref().is_some_and(|s| !s.is_success())
    }

    fn summary_text(&self, theme: &UiTheme) -> (String, Color32) {
        if self.terminated && !self.is_running() {
            return (
                format!("Terminated · {} completed", self.results.len()),
                theme.warning,
            );
        }
        if let Some(s) = &self.summary {
            let color = if s.is_success() {
                theme.success
            } else {
                theme.error
            };
            return (s.label(), color);
        }
        if self.is_running() {
            let failed = self
                .results
                .values()
                .filter(|r| r.outcome.is_problem())
                .count();
            let passed = self
                .results
                .values()
                .filter(|r| r.outcome == Outcome::Passed)
                .count();
            let skipped = self
                .results
                .values()
                .filter(|result| result.outcome == Outcome::Skipped)
                .count();
            return (
                format!(
                    "Running… {}/{} · {passed} passed, {failed} failed, {skipped} skipped",
                    self.results.len(),
                    self.total_planned.max(self.results.len())
                ),
                if failed > 0 { theme.error } else { theme.text },
            );
        }
        if self.error.is_some() {
            return ("Test run failed".into(), theme.error);
        }
        (String::new(), theme.text_dim)
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Status {
    NotRun,
    Running,
    Passed,
    Skipped,
    Failed,
    Error,
    Ignored,
    Terminated,
}

impl Status {
    fn rank(self) -> u8 {
        match self {
            Status::NotRun => 0,
            Status::Skipped => 1,
            Status::Passed => 2,
            Status::Running => 3,
            Status::Failed => 4,
            Status::Error => 5,
            Status::Ignored => 1,
            Status::Terminated => 3,
        }
    }
    fn merge(self, other: Status) -> Status {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
    fn style(self, theme: &UiTheme) -> (&'static str, Color32) {
        match self {
            Status::NotRun => ("·", theme.text_dim),
            Status::Running => ("…", theme.warning),
            Status::Passed => ("✔", theme.success),
            Status::Skipped => ("–", theme.text_dim),
            Status::Failed => ("×", theme.error),
            Status::Error => ("!", theme.error),
            Status::Ignored => ("⊘", theme.text_dim),
            Status::Terminated => ("■", theme.warning),
        }
    }
}

fn outcome_style(o: Outcome, theme: &UiTheme) -> (&'static str, Color32) {
    match o {
        Outcome::Passed => ("Passed", theme.success),
        Outcome::Failed => ("Failed", theme.error),
        Outcome::Error => ("Error", theme.error),
        Outcome::Skipped => ("Skipped", theme.text_dim),
    }
}

fn count_leaves(n: &Node) -> usize {
    if n.children.is_empty() {
        1
    } else {
        n.children.iter().map(count_leaves).sum()
    }
}

fn count_status(panel: &TestPanel, n: &Node, st: Status) -> usize {
    if n.children.is_empty() {
        (panel.status_of(n) == st) as usize
    } else {
        n.children.iter().map(|c| count_status(panel, c, st)).sum()
    }
}

/// The statically discovered tests a target will run, with root-relative
/// ids: a directory's modules, a file's tests, or the path down to one
/// class / test of a file.
fn planned_nodes(root: &Path, target: &str) -> Vec<Node> {
    let (file_part, rest) = match target.split_once("::") {
        Some((f, r)) => (f, Some(r)),
        None => (target, None),
    };
    let path = if Path::new(file_part).is_absolute() {
        PathBuf::from(file_part)
    } else {
        root.join(file_part)
    };
    let rel = path
        .strip_prefix(root)
        .map(|r| r.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| file_part.to_string());
    if path.is_dir() {
        return discover_project(&path)
            .iter()
            .map(|it| {
                let mut n = Node::from_item(it);
                if !rel.is_empty() {
                    rebase(&mut n, &rel);
                }
                n
            })
            .collect();
    }
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let items: Vec<Node> = discover_file_in(root, &path, &text)
        .iter()
        .map(Node::from_item)
        .collect();
    match rest {
        Some(rest) => narrow(items, &format!("{rel}::{rest}")),
        None => items,
    }
}

/// Prefixes ids discovered relative to a sub-directory with that directory.
fn rebase(node: &mut Node, dir: &str) {
    node.id = format!("{dir}/{}", node.id);
    for c in &mut node.children {
        rebase(c, dir);
    }
}

/// Keeps the nodes on the way to `target` and everything below it.
fn narrow(nodes: Vec<Node>, target: &str) -> Vec<Node> {
    nodes
        .into_iter()
        .filter_map(|mut n| {
            let below_or_same = n.id == target
                || n.id.starts_with(&format!("{target}::"))
                || n.id.starts_with(&format!("{target}["));
            if below_or_same || target.starts_with(&format!("{}[", n.id)) {
                Some(n)
            } else if target.starts_with(&format!("{}::", n.id)) {
                n.children = narrow(n.children, target);
                Some(n)
            } else {
                None
            }
        })
        .collect()
}

/// Adds `nodes` to `tree`, merging nodes with the same id.
fn merge_nodes(tree: &mut Vec<Node>, nodes: Vec<Node>) {
    for n in nodes {
        match tree.iter_mut().find(|t| t.id == n.id) {
            Some(existing) => merge_nodes(&mut existing.children, n.children),
            None => tree.push(n),
        }
    }
}

fn find_node<'a>(nodes: &'a [Node], id: &str) -> Option<&'a Node> {
    for n in nodes {
        if n.id == id {
            return Some(n);
        }
        if let Some(f) = find_node(&n.children, id) {
            return Some(f);
        }
    }
    None
}

fn find_node_mut<'a>(nodes: &'a mut [Node], id: &str) -> Option<&'a mut Node> {
    for n in nodes.iter_mut() {
        if n.id == id {
            return Some(n);
        }
        if let Some(f) = find_node_mut(&mut n.children, id) {
            return Some(f);
        }
    }
    None
}

#[cfg(test)]
mod run_status_tests {
    use super::*;

    /// QA: a gutter run of `file::method` (pytest: "ERROR: not found") showed
    /// a green "no tests ran" and a green progress bar.
    #[test]
    fn a_run_that_found_no_test_is_red_and_shows_why() {
        let theme = UiTheme::dark();
        let mut panel = TestPanel::default();
        panel.apply(vec![
            TestEvent::Output(
                "ERROR: not found: tests/test_main.py::test_passes".into(),
                true,
            ),
            TestEvent::Done(Summary {
                duration: 0.02,
                exit_code: Some(4),
                ..Default::default()
            }),
            TestEvent::Exited(4),
        ]);
        assert!(panel.has_problems());
        assert_eq!(
            panel.summary_text(&theme),
            ("no tests ran in 0.02s".to_string(), theme.error)
        );
        assert!(panel.show_output, "the output with pytest's error is shown");
        // A passing run stays green and keeps the output as it was.
        let mut panel = TestPanel::default();
        panel.apply(vec![TestEvent::Done(Summary {
            passed: 1,
            exit_code: Some(0),
            ..Default::default()
        })]);
        assert!(!panel.has_problems());
        assert_eq!(panel.summary_text(&theme).1, theme.success);
        assert!(!panel.show_output);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_context::tests::ScratchDir;

    fn python() -> Option<PathBuf> {
        let ok = std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success());
        ok.then(|| PathBuf::from("python3"))
    }

    fn wait(panel: &mut TestPanel) {
        let start = std::time::Instant::now();
        while panel.is_running() && start.elapsed().as_secs() < 60 {
            panel.poll();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panel.poll();
    }

    fn ids(nodes: &[Node]) -> Vec<String> {
        let mut out = Vec::new();
        for n in nodes {
            out.push(n.id.clone());
            out.extend(ids(&n.children));
        }
        out
    }

    #[test]
    fn planned_tree_uses_root_relative_ids() {
        let dir = ScratchDir::new("plan");
        let root = dir.0.as_path();
        std::fs::create_dir_all(root.join("tests/unit")).unwrap();
        std::fs::write(root.join("tests/unit/test_a.py"), "def test_one():\n    pass\n\n\ndef test_two():\n    pass\n\n\nclass TestK:\n    def test_x(self):\n        pass\n\n    def test_xy(self):\n        pass\n").unwrap();
        let f = "tests/unit/test_a.py";
        // A directory target: the modules below it, ids relative to the project root.
        assert_eq!(
            ids(&planned_nodes(root, "tests")),
            vec![
                f.to_string(),
                format!("{f}::test_one"),
                format!("{f}::test_two"),
                format!("{f}::TestK"),
                format!("{f}::TestK::test_x"),
                format!("{f}::TestK::test_xy")
            ]
        );
        // A method target keeps only the way down to it (not `test_xy`, not the functions).
        assert_eq!(
            ids(&planned_nodes(root, &format!("{f}::TestK::test_x"))),
            vec![format!("{f}::TestK"), format!("{f}::TestK::test_x")]
        );
        let abs = format!("{}::test_two", root.join(f).display());
        assert_eq!(
            ids(&planned_nodes(root, &abs)),
            vec![format!("{f}::test_two")]
        );
        assert_eq!(
            ids(&planned_nodes(root, &format!("{f}::test_one[1-2]"))),
            vec![format!("{f}::test_one")]
        );
        // Several targets in one file share their class node.
        let mut tree = Vec::new();
        merge_nodes(
            &mut tree,
            planned_nodes(root, &format!("{f}::TestK::test_x")),
        );
        merge_nodes(
            &mut tree,
            planned_nodes(root, &format!("{f}::TestK::test_xy")),
        );
        assert_eq!(
            ids(&tree),
            vec![
                format!("{f}::TestK"),
                format!("{f}::TestK::test_x"),
                format!("{f}::TestK::test_xy")
            ]
        );
    }

    /// Running a test of a pytest run made with `-m "not slow"` (a slow test
    /// the tree shows as not run) runs it: the marker filter used to be kept
    /// ("1 deselected", nothing ran).
    #[test]
    fn node_runs_drop_the_marker_filter() {
        let Some(py) = python() else { return };
        if !rustpy_test::pytest_available(&py) {
            return;
        }
        let dir = ScratchDir::new("marker");
        let root = dir.0.clone();
        std::fs::write(
            root.join("pytest.ini"),
            "[pytest]\nmarkers =\n    slow: slow tests\n",
        )
        .unwrap();
        std::fs::write(root.join("test_m.py"), "import pytest\n\n\n@pytest.mark.slow\ndef test_slow():\n    pass\n\n\ndef test_fast():\n    pass\n").unwrap();
        let mut panel = TestPanel::default();
        panel
            .start_with(
                Framework::Pytest,
                &py,
                &root,
                Vec::new(),
                vec![
                    "-p".into(),
                    "no:cacheprovider".into(),
                    "-m".into(),
                    "not slow".into(),
                ],
                TestRunOptions::default(),
            )
            .unwrap();
        wait(&mut panel);
        assert_eq!(
            panel.results.get("test_m.py::test_fast").map(|r| r.outcome),
            Some(Outcome::Passed)
        );
        assert!(!panel.results.contains_key("test_m.py::test_slow"));
        panel
            .run_nodes(vec!["test_m.py::test_slow".into()])
            .unwrap();
        wait(&mut panel);
        assert_eq!(
            panel.results.get("test_m.py::test_slow").map(|r| r.outcome),
            Some(Outcome::Passed),
            "{:?} {:?}",
            panel.summary,
            panel.output
        );
    }

    #[test]
    fn keyword_filters_are_dropped_for_node_runs() {
        let args: Vec<String> = [
            "-x",
            "-k",
            "slow",
            "-kfast",
            "--keep",
            "-v",
            "-m",
            "not slow",
            "-mdb",
            "--maxfail",
            "2",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            without_selection_filters(&args, Framework::Pytest),
            vec!["-x", "--keep", "-v", "--maxfail", "2"]
        );
        // Only pytest's -m is a selection (behave's -m is --no-multiline).
        assert_eq!(
            without_selection_filters(&args, Framework::Behave),
            vec![
                "-x",
                "--keep",
                "-v",
                "-m",
                "not slow",
                "-mdb",
                "--maxfail",
                "2"
            ]
        );
    }

    #[test]
    fn reruns_keep_the_configuration_options() {
        let Some(py) = python() else { return };
        let dir = ScratchDir::new("panel");
        let root = dir.0.clone();
        std::fs::write(root.join("test_env.py"), "import os, unittest\n\nclass Case(unittest.TestCase):\n    def test_env(self):\n        self.assertEqual(os.environ.get('RUSTPY_PANEL'), 'yes')\n\n    def test_values(self):\n        self.assertEqual([1, 2], [1, 3])\n").unwrap();
        let options = TestRunOptions {
            env: vec![("RUSTPY_PANEL".into(), "yes".into())],
            ..Default::default()
        };
        let mut panel = TestPanel::default();
        panel
            .start_with(
                Framework::Unittest,
                &py,
                &root,
                Vec::new(),
                vec!["-k".into(), "test_".into()],
                options.clone(),
            )
            .unwrap();
        wait(&mut panel);
        let id = "test_env.py::Case::test_env";
        assert_eq!(
            panel.results.get(id).map(|r| r.outcome),
            Some(Outcome::Passed),
            "{:?}",
            panel.results
        );
        let values = panel
            .results
            .get("test_env.py::Case::test_values")
            .expect("test_values ran");
        assert_eq!(values.comparison(), Some(("[1, 2]", "[1, 3]")));
        for rerun in [
            TestPanel::rerun as fn(&mut TestPanel) -> Result<(), String>,
            TestPanel::rerun_failed,
        ] {
            rerun(&mut panel).unwrap();
            assert_eq!(panel.last_run().map(|l| &l.options), Some(&options));
            wait(&mut panel);
        }
        assert_eq!(
            panel.last_run().map(|l| l.targets.clone()),
            Some(vec!["test_env.py::Case::test_values".to_string()])
        );
        // Running one node keeps the environment but not the keyword filter.
        panel.run_nodes(vec![id.to_string()]).unwrap();
        assert_eq!(
            panel
                .last_run()
                .map(|l| (l.args.clone(), l.options.clone())),
            Some((Vec::new(), options))
        );
        wait(&mut panel);
        assert_eq!(
            panel.results.get(id).map(|r| r.outcome),
            Some(Outcome::Passed)
        );
    }
}
