use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
#[derive(Clone)]
pub(super) struct Transfer {
    export: bool,
    format: String,
    path: String,
}
impl TestPanel {
    pub fn detect_framework_background(&mut self, root: &Path, interpreter: &Path) {
        let key = (root.to_path_buf(), interpreter.to_path_buf());
        if self
            .detected
            .as_ref()
            .is_some_and(|(current, _)| current == &key)
            || self
                .detecting
                .as_ref()
                .is_some_and(|(current, _)| current == &key)
        {
            return;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker_root = root.to_path_buf();
        let worker_interpreter = interpreter.to_path_buf();
        std::thread::spawn(move || {
            let framework = rustpy_test::detect_framework_for(
                &worker_root,
                &[],
                if worker_interpreter.as_os_str().is_empty() {
                    None
                } else {
                    Some(&worker_interpreter)
                },
            );
            let _ = sender.send(framework);
        });
        self.detecting = Some((key, receiver));
    }
    pub fn poll_framework_detection(&mut self) -> Option<(PathBuf, Framework)> {
        let (key, receiver) = self.detecting.as_ref()?;
        let framework = receiver.try_recv().ok()?;
        let key = key.clone();
        self.detected = Some((key.clone(), framework));
        self.detecting = None;
        Some((key.0, framework))
    }
    pub fn set_execution_allowed(&mut self, allowed: bool) {
        self.execution_blocked = !allowed;
        if !allowed {
            self.suspend_automatic_runs();
        }
    }
    pub fn suspend_automatic_runs(&mut self) {
        self.auto_pending = None;
        self.repeat_remaining = 0;
        self.repeat_until_failure = false;
    }

    pub fn on_source_change(&mut self, path: &Path) {
        if self.auto_test
            && self
                .last
                .as_ref()
                .is_some_and(|last| path.starts_with(&last.root))
            && matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("py" | "pyi" | "feature")
            )
        {
            self.auto_pending = Some(std::time::Instant::now());
        }
    }
    pub fn take_completion_message(&mut self) -> Option<String> {
        self.completion_message.take()
    }
    pub fn wants_repaint(&self) -> bool {
        self.is_running() || self.auto_pending.is_some() || self.detecting.is_some()
    }
    pub(super) fn failed_targets(&self) -> Vec<String> {
        let mut failed: Vec<_> = self
            .results
            .values()
            .filter(|result| result.outcome.is_problem())
            .filter(|result| {
                result.id.contains("::")
                    && !self.ignored.iter().any(|id| {
                        result.id == *id
                            || result.id.starts_with(&format!("{id}::"))
                            || result.id.starts_with(&format!("{id}["))
                    })
            })
            .map(|result| {
                result
                    .id
                    .split_once("::subTest")
                    .map(|(parent, _)| parent)
                    .unwrap_or(&result.id)
                    .to_string()
            })
            .collect();
        failed.sort();
        failed.dedup();
        failed
    }
    pub(super) fn expand_ancestors(&mut self, id: &str) {
        self.collapsed.retain(|parent| {
            !id.starts_with(&format!("{parent}::")) && !id.starts_with(&format!("{parent}["))
        });
    }
    pub(super) fn first_failure_id(&self) -> Option<String> {
        fn walk(nodes: &[Node], results: &HashMap<String, TestResult>) -> Option<String> {
            for node in nodes {
                if results
                    .get(&node.id)
                    .is_some_and(|result| result.outcome.is_problem())
                {
                    return Some(node.id.clone());
                }
                if let Some(id) = walk(&node.children, results) {
                    return Some(id);
                }
            }
            None
        }
        walk(&self.tree, &self.results)
    }
    fn node_matches(&self, node: &Node) -> bool {
        if !self.environment_filter.is_empty() {
            let prefix = format!("tox::{}", self.environment_filter);
            if node.id != "tox"
                && !node.id.starts_with(&prefix)
                && !node.children.iter().any(|child| self.node_matches(child))
            {
                return false;
            }
        }
        self.filter.is_empty()
            || node.id.to_lowercase().contains(&self.filter.to_lowercase())
            || node.children.iter().any(|child| self.node_matches(child))
    }
    pub(super) fn node_visible(&self, node: &Node) -> bool {
        if !self.node_matches(node) {
            return false;
        }
        if !node.children.is_empty() {
            return node.children.iter().any(|child| self.node_visible(child));
        }
        let status = self.status_of(node);
        !(self.hide_passed && status == Status::Passed)
            && (self.view.show_ignored || !matches!(status, Status::Skipped | Status::Ignored))
    }
    fn duration_of(&self, node: &Node) -> f64 {
        if let Some(result) = self.results.get(&node.id) {
            result.duration
        } else {
            node.children
                .iter()
                .map(|child| self.duration_of(child))
                .sum()
        }
    }
    pub(super) fn sort_nodes(&self, nodes: &mut [Node]) {
        match self.view.sort.as_str() {
            "alphabetical" => nodes.sort_by(|a, b| {
                a.name
                    .to_lowercase()
                    .cmp(&b.name.to_lowercase())
                    .then(a.id.cmp(&b.id))
            }),
            "duration" => nodes.sort_by(|a, b| {
                self.duration_of(b)
                    .total_cmp(&self.duration_of(a))
                    .then(a.id.cmp(&b.id))
            }),
            _ => {}
        }
        for node in nodes {
            self.sort_nodes(&mut node.children);
        }
    }
    pub fn set_project_history(&mut self, root: &Path) {
        self.load_history_for(root);
    }
    pub(super) fn load_history_for(&mut self, root: &Path) {
        if self.history_root.as_deref() != Some(root) {
            self.history = reports::load_history(root);
            self.history.truncate(20);
            self.history_root = Some(root.to_path_buf());
            self.ignored.clear();
        }
    }
    fn record(&self) -> RunRecord {
        let mut results: Vec<_> = self.results.values().cloned().collect();
        results.sort_by(|a, b| a.id.cmp(&b.id));
        RunRecord {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            last: self.last.clone(),
            tree: self.tree.clone(),
            results,
            summary: self.summary.clone(),
            output: self.output.clone(),
            metadata: self.metadata.values().cloned().collect(),
        }
    }
    fn show_record(&mut self, record: RunRecord, from_history: bool) {
        self.run = None;
        self.report_root = record.last.as_ref().map(|last| last.root.clone());
        self.tree = record.tree;
        self.results = record
            .results
            .into_iter()
            .map(|result| (result.id.clone(), result))
            .collect();
        self.summary = record.summary;
        self.output = record.output;
        self.metadata = record
            .metadata
            .into_iter()
            .map(|metadata| (metadata.id.clone(), metadata))
            .collect();
        // Imported files are data, not authority to execute their embedded SDK.
        self.last = if from_history { record.last } else { None };
        self.started.clear();
        self.selected = self.first_failure_id();
        self.error = None;
        self.archived = true;
        self.terminated = false;
        self.repeat_remaining = 0;
        self.repeat_until_failure = false;
        self.auto_pending = None;
    }
    pub(super) fn finish_completed_run(&mut self) {
        if self.is_running() || self.archived || (self.summary.is_none() && self.error.is_none()) {
            return;
        }
        self.archived = true;
        self.completion_message = Some(if self.terminated {
            "Tests terminated".into()
        } else if let Some(summary) = &self.summary {
            summary.label()
        } else {
            "Test process failed".into()
        });
        let record = self.record();
        if let Some(root) = record.last.as_ref().map(|last| last.root.clone()) {
            self.load_history_for(&root);
        }
        self.history.insert(0, record);
        self.history.truncate(20);
        if let Some(root) = self.history_root.as_deref() {
            if let Err(error) = reports::save_history(root, &self.history) {
                self.completion_message = Some(format!("Test history was not saved: {error}"));
            }
        }
        let success =
            self.summary.as_ref().is_some_and(Summary::is_success) && self.error.is_none();
        if success && !self.terminated && (self.repeat_until_failure || self.repeat_remaining > 1) {
            self.repeat_remaining = self.repeat_remaining.saturating_sub(1);
            if let Err(error) = self.rerun() {
                self.error = Some(error);
                self.repeat_until_failure = false;
                self.repeat_remaining = 0;
            }
        } else {
            self.repeat_remaining = 0;
            self.repeat_until_failure = false;
        }
    }
    pub(super) fn controls_ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        events: &mut Vec<TestPanelEvent>,
    ) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Filter:");
            ui.add(egui::TextEdit::singleline(&mut self.filter).desired_width(160.0));
            egui::ComboBox::from_id_salt("test-sort")
                .selected_text(format!("Sort: {}", self.view.sort))
                .show_ui(ui, |ui| {
                    for (value, label) in [
                        ("collection", "Collection order"),
                        ("alphabetical", "Alphabetically"),
                        ("duration", "Duration (slowest first)"),
                    ] {
                        ui.selectable_value(
                            &mut self.view.sort,
                            value.into(),
                            crate::i18n::tr_static(label),
                        );
                    }
                })
                .combo_named(crate::i18n::tr_static("Sort order"));
            if ui.small_button("Expand all").clicked() {
                self.collapsed.clear();
            }
            if ui.small_button("Collapse all").clicked() {
                fn collect(nodes: &[Node], ids: &mut HashSet<String>) {
                    for node in nodes {
                        if !node.children.is_empty() {
                            ids.insert(node.id.clone());
                            collect(&node.children, ids);
                        }
                    }
                }
                collect(&self.tree, &mut self.collapsed);
            }
            ui.toggle_value(&mut self.statistics, "Statistics");
            ui.toggle_value(&mut self.auto_test, "Auto-test")
                .on_hover_text(
                    "Rerun after saved Python or feature files change (700 ms debounce)",
                );
            ui.menu_button("View", |ui| {
                ui.checkbox(&mut self.view.show_ignored, "Show skipped / ignored");
                ui.checkbox(&mut self.view.inline_statistics, "Show inline statistics");
                ui.checkbox(&mut self.view.track_running, "Track running test");
                ui.checkbox(&mut self.view.autoscroll, "Autoscroll");
                ui.checkbox(
                    &mut self.view.select_first_failure,
                    "Select first failed test on completion",
                );
            });
            ui.menu_button("Repeat", |ui| {
                ui.label("Number of runs");
                ui.add(egui::DragValue::new(&mut self.repeat_count).range(1..=10000));
                if ui
                    .add_enabled(
                        !self.is_running() && self.last.is_some(),
                        egui::Button::new("Run N times"),
                    )
                    .clicked()
                {
                    self.repeat_remaining = self.repeat_count.max(1);
                    self.repeat_until_failure = false;
                    if let Err(error) = self.rerun() {
                        events.push(TestPanelEvent::Status(error));
                    }
                    ui.close();
                }
                if ui
                    .add_enabled(
                        !self.is_running() && self.last.is_some(),
                        egui::Button::new("Rerun until failure"),
                    )
                    .clicked()
                {
                    self.repeat_until_failure = true;
                    if let Err(error) = self.rerun() {
                        events.push(TestPanelEvent::Status(error));
                    }
                    ui.close();
                }
            });
            ui.menu_button("Results", |ui| {
                for (format, label) in [("html", "Export HTML…"), ("xml", "Export JUnit XML…")]
                {
                    if ui
                        .add_enabled(!self.results.is_empty(), egui::Button::new(label))
                        .clicked()
                    {
                        let path = self
                            .history_root
                            .as_ref()
                            .map(|root| root.join(format!("test-results.{format}")))
                            .unwrap_or_else(|| PathBuf::from(format!("test-results.{format}")));
                        self.transfer = Some(Transfer {
                            export: true,
                            format: format.into(),
                            path: path.display().to_string(),
                        });
                        ui.close();
                    }
                }
                if ui
                    .add_enabled(
                        !self.is_running(),
                        egui::Button::new("Import Tests from File…"),
                    )
                    .clicked()
                {
                    self.transfer = Some(Transfer {
                        export: false,
                        format: String::new(),
                        path: String::new(),
                    });
                    ui.close();
                }
                ui.separator();
                ui.label("Test History (last 20 runs)");
                for index in 0..self.history.len() {
                    let record = &self.history[index];
                    let timestamp =
                        chrono::DateTime::<chrono::Utc>::from_timestamp(record.timestamp as i64, 0)
                            .map(|time| {
                                time.with_timezone(&chrono::Local)
                                    .format("%Y-%m-%d %H:%M:%S")
                                    .to_string()
                            })
                            .unwrap_or_else(|| record.timestamp.to_string());
                    let label = format!(
                        "{} · {}",
                        timestamp,
                        record
                            .summary
                            .as_ref()
                            .map(Summary::label)
                            .unwrap_or_else(|| "Terminated / failed".into())
                    );
                    if ui
                        .add_enabled(!self.is_running(), egui::Button::new(label))
                        .clicked()
                    {
                        self.show_record(self.history[index].clone(), true);
                        ui.close();
                        break;
                    }
                }
            });
            if ui
                .add_enabled(
                    self.last.is_some() && !self.is_running(),
                    egui::Button::new("Coverage"),
                )
                .clicked()
            {
                events.push(TestPanelEvent::Measure {
                    profile: false,
                    target: None,
                });
            }
            if ui
                .add_enabled(
                    self.last.is_some() && !self.is_running(),
                    egui::Button::new("Profile"),
                )
                .clicked()
            {
                events.push(TestPanelEvent::Measure {
                    profile: true,
                    target: None,
                });
            }
        });
        self.transfer_ui(ui.ctx(), events);
        let mut environments: Vec<_> = self
            .metadata
            .values()
            .filter_map(|metadata| metadata.environment.clone())
            .collect();
        environments.sort();
        environments.dedup();
        if !environments.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(
                    &mut self.environment_filter,
                    String::new(),
                    "All environments",
                );
                for environment in environments {
                    ui.selectable_value(
                        &mut self.environment_filter,
                        environment.clone(),
                        environment,
                    );
                }
            });
        }
        if let Some(last) = &self.last {
            if last.framework == Framework::Pytest {
                let expression = last.args.iter().enumerate().find_map(|(index, arg)| {
                    if arg == "-k" {
                        last.args.get(index + 1).cloned()
                    } else {
                        arg.strip_prefix("-k")
                            .filter(|text| !text.is_empty())
                            .map(str::to_owned)
                    }
                });
                if let Some(expression) = expression {
                    ui.label(
                        egui::RichText::new(format!("pytest -k {expression}"))
                            .color(theme.text_dim),
                    );
                }
            }
        }
    }
    fn transfer_ui(&mut self, ctx: &egui::Context, events: &mut Vec<TestPanelEvent>) {
        let Some(mut transfer) = self.transfer.clone() else {
            return;
        };
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        egui::Window::new(if transfer.export {
            "Export test results"
        } else {
            "Import Tests from File"
        })
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .show_modal(ctx, |ui| {
            ui.label(if transfer.export {
                "Output file path"
            } else {
                "HTML or JUnit XML report path"
            });
            ui.add(egui::TextEdit::singleline(&mut transfer.path).desired_width(460.0));
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !transfer.path.trim().is_empty(),
                        egui::Button::new(if transfer.export { "Export" } else { "Import" }),
                    )
                    .clicked()
                {
                    apply = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
        });
        if cancel || !open {
            self.transfer = None;
            return;
        }
        if apply {
            let path = PathBuf::from(transfer.path.trim());
            let path = if path.is_absolute() {
                path
            } else {
                self.history_root
                    .as_ref()
                    .map(|root| root.join(&path))
                    .unwrap_or(path)
            };
            let result = if transfer.export {
                self.record()
                    .export(&transfer.format)
                    .and_then(|text| std::fs::write(&path, text).map_err(|error| error.to_string()))
            } else {
                std::fs::metadata(&path)
                    .map_err(|error| error.to_string())
                    .and_then(|metadata| {
                        if metadata.len() > 16 * 1024 * 1024 {
                            Err("Test report exceeds 16 MiB".into())
                        } else {
                            std::fs::read_to_string(&path).map_err(|error| error.to_string())
                        }
                    })
                    .and_then(|text| RunRecord::import(&text))
                    .map(|record| {
                        self.show_record(record, false);
                        if self.report_root.is_none() {
                            self.report_root = path.parent().map(Path::to_path_buf);
                        }
                    })
            };
            match result {
                Ok(()) => {
                    events.push(TestPanelEvent::Status(format!(
                        "{} {}",
                        if transfer.export {
                            "Exported"
                        } else {
                            "Imported"
                        },
                        path.display()
                    )));
                    self.transfer = None;
                }
                Err(error) => {
                    events.push(TestPanelEvent::Status(error));
                    self.transfer = Some(transfer);
                }
            }
        } else {
            self.transfer = Some(transfer);
        }
    }
    pub(super) fn statistics_ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        events: &mut Vec<TestPanelEvent>,
    ) {
        ui.heading("Test durations");
        let mut results: Vec<_> = self.results.values().cloned().collect();
        results.sort_by(|a, b| b.duration.total_cmp(&a.duration).then(a.id.cmp(&b.id)));
        let total: f64 = results.iter().map(|result| result.duration).sum();
        ui.label(format!("{} tests · {:.3}s total", results.len(), total));
        for result in results {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("{:.3}s", result.duration)).color(theme.text_dim),
                );
                if ui
                    .selectable_label(
                        self.selected.as_deref() == Some(result.id.as_str()),
                        &result.id,
                    )
                    .clicked()
                {
                    self.selected = Some(result.id.clone());
                    self.statistics = false;
                }
                if let Some(node) = find_node(&self.tree, &result.id) {
                    if let (Some(path), Some(line)) = (&node.path, node.line) {
                        if ui.small_button("Source").clicked() {
                            events.push(TestPanelEvent::Navigate(path.clone(), line));
                        }
                    }
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn node(id: &str, children: Vec<Node>) -> Node {
        Node {
            id: id.into(),
            name: id.rsplit("::").next().unwrap().into(),
            kind: if children.is_empty() {
                TestKind::Function
            } else {
                TestKind::Module
            },
            path: None,
            line: None,
            children,
        }
    }
    fn result(id: &str, outcome: Outcome, duration: f64) -> TestResult {
        TestResult {
            id: id.into(),
            outcome,
            duration,
            message: String::new(),
            traceback: String::new(),
            stdout: String::new(),
            stderr: String::new(),
            xfail: false,
            expected: None,
            actual: None,
        }
    }
    #[test]
    fn failed_rerun_targets_keep_exact_parameter_ids_and_skip_ignored_nodes() {
        let mut panel = TestPanel::default();
        for result in [
            result("test_a.py::test_value[first]", Outcome::Failed, 1.0),
            result("test_a.py::test_value[second]", Outcome::Passed, 0.2),
            result("test_b.py::test_other", Outcome::Error, 0.3),
        ] {
            panel.results.insert(result.id.clone(), result);
        }
        panel.ignored.insert("test_b.py::test_other".into());
        assert_eq!(panel.failed_targets(), vec!["test_a.py::test_value[first]"]);
        assert_eq!(
            without_selection_filters(
                &["-k".into(), "other".into(), "-mnetwork".into(), "-q".into()],
                Framework::Pytest
            ),
            vec!["-q"]
        );
    }
    #[test]
    fn completion_selects_first_failure_in_tree_order_and_filters_keep_ancestors() {
        let mut panel = TestPanel {
            tree: vec![node(
                "test_a.py",
                vec![
                    node("test_a.py::test_first", Vec::new()),
                    node("test_a.py::test_second", Vec::new()),
                ],
            )],
            ..Default::default()
        };
        panel.apply(vec![
            TestEvent::Result(result("test_a.py::test_second", Outcome::Failed, 0.5)),
            TestEvent::Result(result("test_a.py::test_first", Outcome::Error, 0.1)),
            TestEvent::Done(Summary {
                failed: 1,
                errors: 1,
                exit_code: Some(1),
                ..Default::default()
            }),
        ]);
        assert_eq!(panel.selected.as_deref(), Some("test_a.py::test_first"));
        panel.filter = "second".into();
        assert!(panel.node_matches(&panel.tree[0]));
        assert!(!panel.node_matches(&panel.tree[0].children[0]));
        panel.view.sort = "duration".into();
        let mut nodes = panel.tree[0].children.clone();
        panel.sort_nodes(&mut nodes);
        assert_eq!(nodes[0].id, "test_a.py::test_second");
    }
    #[test]
    fn auto_test_is_debounced_and_safe_mode_refuses_direct_panel_reruns() {
        let mut panel = TestPanel {
            auto_test: true,
            last: Some(LastRun {
                framework: Framework::Pytest,
                interpreter: PathBuf::from("/never/execute"),
                root: PathBuf::from("/project"),
                targets: Vec::new(),
                args: Vec::new(),
                options: TestRunOptions::default(),
            }),
            ..Default::default()
        };
        panel.on_source_change(Path::new("/elsewhere/source.py"));
        assert!(panel.auto_pending.is_none());
        panel.on_source_change(Path::new("/project/source.py"));
        assert!(panel.auto_pending.is_some());
        panel.set_execution_allowed(false);
        assert!(panel.auto_pending.is_none());
        assert!(panel.rerun().unwrap_err().contains("Safe mode"));
    }
    #[test]
    fn stop_marks_started_unfinished_nodes_terminated_and_cancels_repeat() {
        let mut panel = TestPanel::default();
        panel.started.insert("test.py::test_running".into());
        panel.repeat_remaining = 7;
        panel.repeat_until_failure = true;
        panel.stop();
        assert_eq!(
            panel.status_of(&node("test.py::test_running", Vec::new())),
            Status::Terminated
        );
        assert_eq!(panel.repeat_remaining, 0);
        assert!(!panel.repeat_until_failure);
    }
}
