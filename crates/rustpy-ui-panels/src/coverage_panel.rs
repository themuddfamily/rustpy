//! Coverage tool window: run output, a directory tree of per-file statement
//! and branch coverage with sortable columns, Hide/Show coverage (editor
//! gutter), Clear, Import (`.coverage`, `coverage.json`, Cobertura XML) and
//! the coverage.py HTML report.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};

use egui::Sense;
use rustpy_run::coverage::{percent_of, CoverageOptions};
use rustpy_run::{CoverageReport, CoverageRun, LineCoverage};

use crate::theme::UiTheme;
mod suites;
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CoverageSuite {
    pub id: u64,
    pub name: String,
    pub active: bool,
    pub report: CoverageReport,
}

pub enum CoverageEvent {
    OpenFile(PathBuf),
    Status(String),
}

/// Columns of the coverage table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortBy {
    #[default]
    Name,
    Statements,
    LinePercent,
    BranchPercent,
}

pub struct CoveragePanel {
    pub settings: rustpy_settings::coverage::CoverageSettings,
    pub suites: Vec<CoverageSuite>,
    root: Option<PathBuf>,
    pub modified_files: HashSet<PathBuf>,
    only_modified: bool,
    merge_new: bool,
    export_path: String,
    pub run: Option<CoverageRun>,
    pub report: Option<CoverageReport>,
    pub log: Vec<(String, bool)>,
    pub show_log: bool,
    /// "Hide coverage": the report stays, the editor gutter shows nothing.
    pub hidden: bool,
    /// Measure branch coverage in the next run.
    pub branch: bool,
    /// Interpreter used to import `.coverage` files and write HTML reports (set by the app).
    pub python: Option<PathBuf>,
    pub sort: SortBy,
    pub descending: bool,
    /// Collapsed directories (keys from [`table_rows`]).
    pub collapsed: HashSet<String>,
    /// Path typed in the Import row (`Some` while the row is shown).
    pub import_path: Option<String>,
    import_error: Option<String>,
    import_rx: Option<Receiver<Result<CoverageReport, String>>>,
    html_rx: Option<Receiver<Result<PathBuf, String>>>,
    /// `index.html` of the last HTML report.
    pub html_index: Option<PathBuf>,
}

impl Default for CoveragePanel {
    fn default() -> Self {
        Self {
            settings: Default::default(),
            suites: Vec::new(),
            root: None,
            modified_files: HashSet::new(),
            only_modified: false,
            merge_new: false,
            export_path: String::new(),
            run: None,
            report: None,
            log: Vec::new(),
            show_log: false,
            hidden: false,
            branch: true,
            python: None,
            sort: SortBy::Name,
            descending: false,
            collapsed: HashSet::new(),
            import_path: None,
            import_error: None,
            import_rx: None,
            html_rx: None,
            html_index: None,
        }
    }
}

/// A row of the coverage table: a directory with totals over its files, or a file.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub depth: usize,
    pub name: String,
    /// Directory key (for collapsing); `None` for a file row.
    pub dir: Option<String>,
    /// Index into `CoverageReport::files` for a file row.
    pub file: Option<usize>,
    pub covered: usize,
    pub statements: usize,
    pub covered_branches: usize,
    pub branches: usize,
}

impl Row {
    pub fn line_percent(&self) -> f32 {
        percent_of(self.covered, self.statements)
    }

    pub fn branch_percent(&self) -> Option<f32> {
        (self.branches > 0).then(|| percent_of(self.covered_branches, self.branches))
    }
}

#[derive(Default)]
struct Node {
    dirs: BTreeMap<String, Node>,
    files: Vec<usize>,
    totals: [usize; 4],
}

/// The coverage table as a tree: directories (relative to `root`; files
/// elsewhere are grouped under their absolute directory) with aggregated
/// statement and branch counts, sorted by `sort` within each directory,
/// directories before files. Children of `collapsed` directories are left out.
pub fn table_rows(
    report: &CoverageReport,
    root: &Path,
    sort: SortBy,
    descending: bool,
    collapsed: &HashSet<String>,
) -> Vec<Row> {
    let mut tree = Node::default();
    for (i, f) in report.files.iter().enumerate() {
        let dirs: Vec<String> = match f.path.strip_prefix(root) {
            Ok(rel) => rel
                .parent()
                .map(|p| {
                    p.components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default(),
            Err(_) => f
                .path
                .parent()
                .map(|p| vec![p.display().to_string()])
                .unwrap_or_default(),
        };
        let counts = [
            f.executed.len(),
            f.statements(),
            f.covered_branches,
            f.num_branches,
        ];
        let mut node = &mut tree;
        for d in dirs {
            for (t, c) in node.totals.iter_mut().zip(counts) {
                *t += c;
            }
            node = node.dirs.entry(d).or_default();
        }
        for (t, c) in node.totals.iter_mut().zip(counts) {
            *t += c;
        }
        node.files.push(i);
    }
    let mut rows = Vec::new();
    flatten(&tree, "", 0, report, sort, descending, collapsed, &mut rows);
    rows
}

#[allow(clippy::too_many_arguments)]
fn flatten(
    node: &Node,
    prefix: &str,
    depth: usize,
    report: &CoverageReport,
    sort: SortBy,
    descending: bool,
    collapsed: &HashSet<String>,
    out: &mut Vec<Row>,
) {
    let mut dir_rows: Vec<(Row, &Node)> = node
        .dirs
        .iter()
        .map(|(name, child)| {
            let key = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let [covered, statements, covered_branches, branches] = child.totals;
            (
                Row {
                    depth,
                    name: name.clone(),
                    dir: Some(key),
                    file: None,
                    covered,
                    statements,
                    covered_branches,
                    branches,
                },
                child,
            )
        })
        .collect();
    let mut file_rows: Vec<Row> = node
        .files
        .iter()
        .map(|&i| {
            let f = &report.files[i];
            let name = f
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| f.path.display().to_string());
            Row {
                depth,
                name,
                dir: None,
                file: Some(i),
                covered: f.executed.len(),
                statements: f.statements(),
                covered_branches: f.covered_branches,
                branches: f.num_branches,
            }
        })
        .collect();
    let cmp = |a: &Row, b: &Row| {
        let by_name = a.name.to_lowercase().cmp(&b.name.to_lowercase());
        let ord = match sort {
            SortBy::Name => by_name,
            SortBy::Statements => a.statements.cmp(&b.statements),
            SortBy::LinePercent => a.line_percent().total_cmp(&b.line_percent()),
            SortBy::BranchPercent => a
                .branch_percent()
                .unwrap_or(-1.0)
                .total_cmp(&b.branch_percent().unwrap_or(-1.0)),
        };
        // Ties stay in name order whichever way the column sorts.
        (if descending { ord.reverse() } else { ord }).then(by_name)
    };
    dir_rows.sort_by(|a, b| cmp(&a.0, &b.0));
    file_rows.sort_by(cmp);
    for (row, child) in dir_rows {
        let key = row.dir.clone().unwrap_or_default();
        out.push(row);
        if !collapsed.contains(&key) {
            flatten(
                child,
                &key,
                depth + 1,
                report,
                sort,
                descending,
                collapsed,
                out,
            );
        }
    }
    out.extend(file_rows);
}

/// Opens `path` with the system's default application (the browser for HTML).
fn open_with_system(path: &Path) -> Result<(), String> {
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(path).spawn().map(|_| ()).map_err(|e| e.to_string())
}

impl CoveragePanel {
    pub fn is_running(&self) -> bool {
        self.run.as_ref().is_some_and(|r| r.is_running())
    }

    /// Stops the running process (■ Stop here, Ctrl+F2, the toolbar ■).
    pub fn stop(&mut self) {
        if let Some(r) = &mut self.run {
            r.stop();
        }
    }

    /// Options for the next "Run with Coverage".
    pub fn options(&self) -> CoverageOptions {
        CoverageOptions {
            branch: self.branch,
            bundled: self.settings.bundled,
            executable: (!self.settings.executable.trim().is_empty())
                .then(|| PathBuf::from(self.settings.executable.trim())),
            pytest_cov: self.settings.pytest_cov,
            include: self.settings.include.clone(),
            omit: self.settings.exclude.clone(),
            collect_tests: self.settings.collect_test_folders,
        }
    }

    pub fn start(&mut self, run: CoverageRun) {
        self.log.clear();
        self.log.push((format!("$ {}", run.command_line), false));
        self.hidden = false;
        self.html_index = None;
        self.show_log = true;
        self.run = Some(run);
    }

    /// Forgets the report (and the gutter marks); the kept data file stays on disk.
    pub fn clear(&mut self) {
        for suite in &mut self.suites {
            suite.active = false;
        }
        self.persist_suites();
        self.report = None;
        self.html_index = None;
        self.log.clear();
    }

    /// Drains output and background jobs; true when something changed or a job is pending.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.import_rx {
            match rx.try_recv() {
                Ok(Ok(report)) => {
                    let from = self.import_path.take().unwrap_or_default();
                    self.log.push((
                        format!(
                            "Imported coverage from {from}: {} file(s)",
                            report.files.len()
                        ),
                        false,
                    ));
                    self.accept_report(from, report);
                    self.hidden = false;
                    self.html_index = None;
                    self.show_log = false;
                    self.import_error = None;
                    self.import_rx = None;
                }
                Ok(Err(e)) => {
                    self.import_error = Some(e);
                    self.import_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.import_rx = None,
            }
            changed = true;
        }
        if let Some(rx) = &self.html_rx {
            match rx.try_recv() {
                Ok(Ok(index)) => {
                    self.log
                        .push((format!("HTML report: {}", index.display()), false));
                    self.html_index = Some(index);
                    self.html_rx = None;
                }
                Ok(Err(e)) => {
                    self.log.push((e, true));
                    self.show_log = true;
                    self.html_rx = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.html_rx = None,
            }
            changed = true;
        }
        let Some(run) = &mut self.run else {
            return changed;
        };
        let lines = run.poll();
        changed |= !lines.is_empty();
        self.log.extend(lines);
        let mut completed = None;
        if !run.is_running() {
            // (An error is in `lines` already.)
            if let Some(r) = run.report() {
                completed = Some((run.name.clone(), r.clone()));
                self.show_log = false;
            }
            self.run = None;
            changed = true;
        }
        if let Some((name, report)) = completed {
            self.accept_report(name, report);
        }
        changed
    }

    /// Gutter states for one file: `(0-based line, state)`; nothing while hidden.
    pub fn lines_for(&self, path: &Path) -> Vec<(usize, LineCoverage)> {
        if self.hidden {
            return Vec::new();
        }
        let Some(report) = &self.report else {
            return Vec::new();
        };
        let canon = dunce::canonicalize(path).ok();
        let Some(f) = report
            .file(path)
            .or_else(|| canon.as_deref().and_then(|c| report.file(c)))
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for &l in &f.executed {
            let state = if f.partial.binary_search(&l).is_ok() {
                LineCoverage::Partial
            } else {
                LineCoverage::Covered
            };
            out.push((l.saturating_sub(1), state));
        }
        for l in &f.missing {
            out.push((l.saturating_sub(1), LineCoverage::Missed));
        }
        for l in &f.excluded {
            out.push((l.saturating_sub(1), LineCoverage::Excluded));
        }
        out.sort_by_key(|(l, _)| *l);
        out
    }

    fn python(&self) -> PathBuf {
        self.python
            .clone()
            .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "python" } else { "python3" }))
    }

    fn start_import(&mut self, root: &Path) {
        let Some(text) = self.import_path.clone() else {
            return;
        };
        let path = PathBuf::from(text.trim());
        let path = if path.is_absolute() {
            path
        } else {
            root.join(path)
        };
        let (tx, rx) = channel();
        let (root, python) = (root.to_path_buf(), self.python());
        std::thread::spawn(move || {
            let _ = tx.send(
                rustpy_run::coverage::import_report(&path, &root, &python)
                    .map_err(|e| e.to_string()),
            );
        });
        self.import_error = None;
        self.import_rx = Some(rx);
    }

    fn start_html(&mut self, root: &Path) {
        let Some(report) = &self.report else { return };
        let Some(data) = report.data_file.clone().filter(|path| path.is_file()) else {
            let report = report.clone();
            let out = root.join(".rustpy/coverage/html/active");
            let (tx, rx) = channel();
            std::thread::spawn(move || {
                let _ = tx.send(
                    rustpy_run::coverage_suites::export_html(&report, &out)
                        .map_err(|error| error.to_string()),
                );
            });
            self.html_rx = Some(rx);
            return;
        };
        let stem = data
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "coverage".into());
        let out_dir = root
            .join(".rustpy")
            .join("coverage")
            .join("html")
            .join(stem);
        let (tx, rx) = channel();
        let (root, python, rcfile) = (root.to_path_buf(), self.python(), report.rcfile.clone());
        std::thread::spawn(move || {
            let _ = tx.send(
                rustpy_run::coverage::html_report(
                    &python,
                    &data,
                    rcfile.as_deref(),
                    &out_dir,
                    &root,
                )
                .map_err(|e| e.to_string()),
            );
        });
        self.html_index = None;
        self.html_rx = Some(rx);
    }

    /// The Import row's starting path: an existing report in the project, else the project directory.
    fn default_import_path(root: &Path) -> String {
        [".coverage", "coverage.json", "coverage.xml"]
            .iter()
            .map(|n| root.join(n))
            .find(|p| p.is_file())
            .unwrap_or_else(|| root.join(""))
            .display()
            .to_string()
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        mono: egui::FontId,
        root: &Path,
    ) -> Vec<CoverageEvent> {
        let mut events = Vec::new();
        self.suites_ui(ui, root, &mut events);
        ui.horizontal(|ui| {
            if self.is_running() {
                ui.label(egui::RichText::new("Running with coverage…").color(theme.warning));
                if ui.button(egui::RichText::new("■ Stop").color(theme.error)).clicked() {
                    self.stop();
                }
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(80));
            } else if let Some(r) = &self.report {
                let (covered, total) = r.totals();
                let line_pct = percent_of(covered, total);
                let (cb, b) = r.branch_totals();
                let branches = if b > 0 { format!(", {:.1}% of branches ({cb}/{b})", percent_of(cb, b)) } else { String::new() };
                let text = format!("Coverage: {line_pct:.1}% of statements ({covered}/{total}){branches} in {} file(s)", r.files.len());
                ui.label(egui::RichText::new(text).strong().color(if line_pct >= 80.0 { theme.success } else { theme.warning }));
            } else {
                ui.label(egui::RichText::new("Run › Run with Coverage to collect line and branch coverage.").color(theme.text_dim));
            }
            ui.separator();
            let has_report = self.report.is_some();
            let hide_text = if self.hidden { "Show Coverage" } else { "Hide Coverage" };
            if ui.add_enabled(has_report, egui::Button::new(hide_text)).on_hover_text("Show or hide the coverage marks in the editor gutter").clicked() {
                self.hidden = !self.hidden;
            }
            if ui.add_enabled(has_report && !self.is_running(), egui::Button::new("Clear")).on_hover_text("Close this coverage report").clicked() {
                self.clear();
            }
            if ui.add_enabled(self.import_rx.is_none(), egui::Button::new("Import…")).on_hover_text("Load a .coverage data file, coverage.json or Cobertura XML report").clicked() {
                self.import_path = match self.import_path {
                    Some(_) => None,
                    None => Some(Self::default_import_path(root)),
                };
                self.import_error = None;
            }
            let can_html = self.report.is_some() && self.html_rx.is_none();
            let html_tip = if has_report && !can_html && self.html_rx.is_none() { "Needs coverage data: run with coverage or import a .coverage file" } else { "Write coverage.py's HTML report" };
            if ui.add_enabled(can_html, egui::Button::new(if self.html_rx.is_some() { "Writing HTML…" } else { "Generate HTML report" })).on_hover_text(html_tip).clicked() {
                self.start_html(root);
            }
            if let Some(index) = self.html_index.clone() {
                if ui.link("Open in Browser").on_hover_text(index.display().to_string()).clicked() {
                    match open_with_system(&index) {
                        Ok(()) => events.push(CoverageEvent::Status(format!("Opened {}", index.display()))),
                        Err(e) => events.push(CoverageEvent::Status(format!("Cannot open {}: {e}", index.display()))),
                    }
                }
            }
            ui.checkbox(&mut self.branch, "Branch coverage").on_hover_text("Measure branches in the next run (partially covered lines are yellow)");
            ui.toggle_value(&mut self.show_log, "Output");
        });
        if self.import_path.is_some() {
            let mut start = false;
            let mut cancel = false;
            ui.horizontal(|ui| {
                ui.label("Import coverage from:");
                if let Some(p) = &mut self.import_path {
                    let resp = ui.add(
                        egui::TextEdit::singleline(p)
                            .desired_width(420.0)
                            .hint_text(".coverage, coverage.json or coverage.xml"),
                    );
                    start |= resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                }
                start |= ui
                    .add_enabled(self.import_rx.is_none(), egui::Button::new("Import"))
                    .clicked();
                cancel = ui.button("Cancel").clicked();
                if self.import_rx.is_some() {
                    crate::a11y::spinner(ui);
                }
                if let Some(e) = &self.import_error {
                    ui.label(egui::RichText::new(e).color(theme.error));
                }
            });
            if cancel {
                self.import_path = None;
                self.import_error = None;
            } else if start && self.import_rx.is_none() {
                self.start_import(root);
            }
        }
        if self.import_rx.is_some() || self.html_rx.is_some() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        ui.separator();
        if self.show_log || self.report.is_none() {
            egui::ScrollArea::vertical()
                .id_salt("coverage_log")
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .max_height(if self.report.is_some() {
                    120.0
                } else {
                    f32::INFINITY
                })
                .show(ui, |ui| {
                    for (line, err) in &self.log {
                        ui.label(egui::RichText::new(line).font(mono.clone()).color(if *err {
                            theme.error
                        } else {
                            theme.text
                        }));
                    }
                });
            if self.report.is_some() {
                ui.separator();
            }
        }
        if let Some(r) = &self.report {
            let filtered = CoverageReport {
                files: r
                    .files
                    .iter()
                    .filter(|file| {
                        (!self.only_modified || self.modified_files.contains(&file.path))
                            && (!self.settings.hide_complete || file.percent < 100.0)
                    })
                    .cloned()
                    .collect(),
                ..r.clone()
            };
            let r = &filtered;
            let rows = table_rows(r, root, self.sort, self.descending, &self.collapsed);
            let mut toggle_dir: Option<String> = None;
            let mut sort_by: Option<SortBy> = None;
            egui::ScrollArea::vertical()
                .id_salt("coverage_files")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Grid::new("coverage_grid")
                        .num_columns(5)
                        .striped(true)
                        .spacing([16.0, 3.0])
                        .show(ui, |ui| {
                            for (title, col) in [
                                ("Name", Some(SortBy::Name)),
                                ("Statements", Some(SortBy::Statements)),
                                ("Line %", Some(SortBy::LinePercent)),
                                ("Branches", None),
                                ("Branch %", Some(SortBy::BranchPercent)),
                            ] {
                                let Some(col) = col else {
                                    ui.label(egui::RichText::new(title).strong());
                                    continue;
                                };
                                // (⏶/⏷ come from egui's icon font; the UI font has no ▲/▼.)
                                let arrow = if self.sort == col {
                                    if self.descending {
                                        " ⏷"
                                    } else {
                                        " ⏶"
                                    }
                                } else {
                                    ""
                                };
                                if ui
                                    .add(
                                        egui::Label::new(
                                            egui::RichText::new(format!("{title}{arrow}")).strong(),
                                        )
                                        .sense(Sense::click()),
                                    )
                                    .on_hover_text("Sort")
                                    .clicked()
                                {
                                    sort_by = Some(col);
                                }
                            }
                            ui.end_row();
                            let pct_color = |p: f32| {
                                if p >= 80.0 {
                                    theme.success
                                } else if p >= 50.0 {
                                    theme.warning
                                } else {
                                    theme.error
                                }
                            };
                            for row in &rows {
                                ui.horizontal(|ui| {
                                    ui.add_space(row.depth as f32 * 16.0);
                                    match (&row.dir, row.file) {
                                        (Some(key), _) => {
                                            let arrow = if self.collapsed.contains(key) {
                                                "⏵"
                                            } else {
                                                "⏷"
                                            };
                                            if ui
                                                .add(
                                                    egui::Label::new(
                                                        egui::RichText::new(format!(
                                                            "{arrow} {}",
                                                            row.name
                                                        ))
                                                        .color(theme.text),
                                                    )
                                                    .sense(Sense::click()),
                                                )
                                                .clicked()
                                            {
                                                toggle_dir = Some(key.clone());
                                            }
                                        }
                                        (None, Some(i)) => {
                                            let link = ui
                                                .add(
                                                    egui::Label::new(
                                                        egui::RichText::new(&row.name)
                                                            .color(theme.accent),
                                                    )
                                                    .sense(Sense::click()),
                                                )
                                                .on_hover_text(
                                                    r.files[i].path.display().to_string(),
                                                );
                                            if link.clicked() {
                                                events.push(CoverageEvent::OpenFile(
                                                    r.files[i].path.clone(),
                                                ));
                                            }
                                        }
                                        _ => {}
                                    }
                                });
                                ui.label(format!("{}/{}", row.covered, row.statements));
                                ui.label(
                                    egui::RichText::new(format!("{:.1}%", row.line_percent()))
                                        .color(pct_color(row.line_percent())),
                                );
                                match row.branch_percent() {
                                    Some(p) => {
                                        ui.label(format!(
                                            "{}/{}",
                                            row.covered_branches, row.branches
                                        ));
                                        ui.label(
                                            egui::RichText::new(format!("{p:.1}%"))
                                                .color(pct_color(p)),
                                        );
                                    }
                                    None => {
                                        ui.label(egui::RichText::new("—").color(theme.text_dim));
                                        ui.label(egui::RichText::new("—").color(theme.text_dim));
                                    }
                                }
                                ui.end_row();
                            }
                        });
                });
            if let Some(key) = toggle_dir {
                if !self.collapsed.remove(&key) {
                    self.collapsed.insert(key);
                }
            }
            if let Some(col) = sort_by {
                if self.sort == col {
                    self.descending = !self.descending;
                } else {
                    self.sort = col;
                    // Largest files first; for percentages the lowest coverage first.
                    self.descending = col == SortBy::Statements;
                }
            }
        }
        if let Some(e) = self
            .run
            .as_ref()
            .and_then(|r| r.error())
            .map(|e| e.to_string())
        {
            events.push(CoverageEvent::Status(e));
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_run::FileCoverage;

    fn file(
        path: &str,
        executed: Vec<usize>,
        missing: Vec<usize>,
        branches: (usize, usize),
        partial: Vec<usize>,
    ) -> FileCoverage {
        FileCoverage {
            path: PathBuf::from(path),
            executed,
            missing,
            partial,
            covered_branches: branches.0,
            num_branches: branches.1,
            ..Default::default()
        }
    }

    fn report() -> CoverageReport {
        CoverageReport {
            files: vec![
                file("/p/app/models.py", vec![1, 2, 3], vec![4], (1, 2), vec![2]),
                file("/p/app/views.py", vec![1], vec![2, 3, 4], (0, 0), vec![]),
                file("/p/main.py", vec![1, 2], vec![], (2, 2), vec![]),
                file("/p/app/sub/deep.py", vec![1, 2], vec![2], (0, 0), vec![]),
                file("/usr/lib/other.py", vec![1], vec![1], (0, 0), vec![]),
            ],
            total_percent: 0.0,
            branch: true,
            ..Default::default()
        }
    }

    fn names(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth), r.name))
            .collect()
    }

    #[test]
    fn rows_form_a_directory_tree_with_totals() {
        let r = report();
        let rows = table_rows(&r, Path::new("/p"), SortBy::Name, false, &HashSet::new());
        assert_eq!(
            names(&rows),
            vec![
                "/usr/lib",
                "  other.py",
                "app",
                "  sub",
                "    deep.py",
                "  models.py",
                "  views.py",
                "main.py"
            ]
        );
        let app = &rows[2];
        assert_eq!(app.dir.as_deref(), Some("app"));
        // models 3/4 + views 1/4 + deep 2/3 (line 2 counted once as executed and once missing).
        assert_eq!((app.covered, app.statements), (6, 11));
        assert_eq!((app.covered_branches, app.branches), (1, 2));
        assert_eq!(app.branch_percent(), Some(50.0));
        assert_eq!(rows[3].dir.as_deref(), Some("app/sub"));
        assert_eq!(rows[7].file, Some(2));
        assert_eq!(rows[7].branch_percent(), Some(100.0));
        assert_eq!(rows[6].branch_percent(), None);
        // Collapsing a directory hides its subtree.
        let collapsed: HashSet<String> = ["app".to_string()].into();
        assert_eq!(
            names(&table_rows(
                &r,
                Path::new("/p"),
                SortBy::Name,
                false,
                &collapsed
            )),
            vec!["/usr/lib", "  other.py", "app", "main.py"]
        );
    }

    #[test]
    fn rows_sort_by_column_within_each_directory() {
        let r = report();
        // Ascending: lowest coverage first (/usr/lib 50% < app 54.5%; views 25% < models 75%).
        let rows = table_rows(
            &r,
            Path::new("/p"),
            SortBy::LinePercent,
            false,
            &HashSet::new(),
        );
        assert_eq!(
            names(&rows),
            vec![
                "/usr/lib",
                "  other.py",
                "app",
                "  sub",
                "    deep.py",
                "  views.py",
                "  models.py",
                "main.py"
            ]
        );
        let rows = table_rows(
            &r,
            Path::new("/p"),
            SortBy::LinePercent,
            true,
            &HashSet::new(),
        );
        assert_eq!(
            names(&rows),
            vec![
                "app",
                "  sub",
                "    deep.py",
                "  models.py",
                "  views.py",
                "/usr/lib",
                "  other.py",
                "main.py"
            ]
        );
        // Equal statement counts (models and views have 4) stay in name order.
        let rows = table_rows(
            &r,
            Path::new("/p"),
            SortBy::Statements,
            true,
            &HashSet::new(),
        );
        assert_eq!(
            names(&rows),
            vec![
                "app",
                "  sub",
                "    deep.py",
                "  models.py",
                "  views.py",
                "/usr/lib",
                "  other.py",
                "main.py"
            ]
        );
        // Files without branches sort below every measured one.
        let rows = table_rows(
            &r,
            Path::new("/p"),
            SortBy::BranchPercent,
            true,
            &HashSet::new(),
        );
        let app_files: Vec<String> = rows
            .iter()
            .filter(|r| r.depth == 1 && r.file.is_some() && r.name != "other.py")
            .map(|r| r.name.clone())
            .collect();
        assert_eq!(app_files, vec!["models.py", "views.py"]);
    }

    #[test]
    fn hidden_coverage_and_partial_gutter_states() {
        let mut panel = CoveragePanel {
            report: Some(report()),
            ..Default::default()
        };
        let lines = panel.lines_for(Path::new("/p/app/models.py"));
        assert_eq!(
            lines,
            vec![
                (0, LineCoverage::Covered),
                (1, LineCoverage::Partial),
                (2, LineCoverage::Covered),
                (3, LineCoverage::Missed)
            ]
        );
        panel.hidden = true;
        assert!(panel.lines_for(Path::new("/p/app/models.py")).is_empty());
        panel.hidden = false;
        panel.clear();
        assert!(
            panel.report.is_none() && panel.lines_for(Path::new("/p/app/models.py")).is_empty()
        );
        assert!(panel.options().branch);
        panel.branch = false;
        assert_eq!(
            panel.options(),
            CoverageOptions {
                branch: false,
                ..Default::default()
            }
        );
    }

    /// QA: only the panel's own ■ stopped "Run with Coverage"; Ctrl+F2 and
    /// the toolbar ■ now call [`CoveragePanel::stop`] too.
    #[test]
    fn stop_ends_a_coverage_run_with_one_message() {
        let python = Path::new("python3");
        if !rustpy_run::coverage_available(python) {
            eprintln!("python3 with coverage not available; skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("rustpy-coverage-stop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("loop.py"),
            "import time\nprint('tick', flush=True)\nwhile True:\n    time.sleep(0.05)\n",
        )
        .unwrap();
        let mut cfg = rustpy_project::RunConfig::for_script(&dir.join("loop.py"));
        cfg.interpreter = Some(python.to_path_buf());
        let mut panel = CoveragePanel::default();
        panel.start(
            rustpy_run::coverage::run_with_coverage_opts(1, &cfg, None, &dir, &panel.options())
                .expect("coverage starts"),
        );
        let wait = |panel: &mut CoveragePanel, done: &dyn Fn(&CoveragePanel) -> bool| {
            let start = std::time::Instant::now();
            while !done(panel) && start.elapsed() < std::time::Duration::from_secs(30) {
                panel.poll();
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        };
        wait(&mut panel, &|p| p.log.iter().any(|(l, _)| l == "tick"));
        assert!(panel.is_running(), "{:?}", panel.log);
        panel.stop();
        wait(&mut panel, &|p| !p.is_running());
        assert!(!panel.is_running());
        let stopped = panel
            .log
            .iter()
            .filter(|(l, err)| *err && l.contains("stopped before"))
            .count();
        assert_eq!(stopped, 1, "{:?}", panel.log);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
