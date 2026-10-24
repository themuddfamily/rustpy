//! Profiler tool window: cProfile statistics with callers / callees.

use std::path::{Path, PathBuf};

use egui::Sense;
use rustpy_run::{ProfileReport, ProfileRun};

use crate::theme::UiTheme;
mod views;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProfileView {
    #[default]
    Statistics,
    CallTree,
    CallGraph,
    FlameGraph,
    Comparison,
}
enum FileResult {
    Imported(ProfileReport, bool),
    Saved(PathBuf),
    Failed(String),
}

pub enum ProfilerEvent {
    Navigate(PathBuf, usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    Name,
    TotTime,
    CumTime,
    Calls,
}

pub struct ProfilerPanel {
    pub options: rustpy_run::profile::ProfileOptions,
    pub python: Option<PathBuf>,
    view: ProfileView,
    baseline: Option<ProfileReport>,
    file_path: String,
    file_rx: Option<std::sync::mpsc::Receiver<FileResult>>,
    graph_zoom: f32,
    graph_root: Option<usize>,
    descending: bool,
    accessibility_blocked: bool,
    pub run: Option<ProfileRun>,
    pub report: Option<ProfileReport>,
    pub log: Vec<(String, bool)>,
    pub sort: SortBy,
    pub project_only: bool,
    pub selected: Option<usize>,
    pub filter: String,
    pub show_log: bool,
}

impl Default for ProfilerPanel {
    fn default() -> Self {
        Self {
            options: Default::default(),
            python: None,
            view: Default::default(),
            baseline: None,
            file_path: String::new(),
            file_rx: None,
            graph_zoom: 1.0,
            graph_root: None,
            descending: true,
            accessibility_blocked: false,
            run: None,
            report: None,
            log: Vec::new(),
            sort: SortBy::TotTime,
            project_only: true,
            selected: None,
            filter: String::new(),
            show_log: false,
        }
    }
}

impl ProfilerPanel {
    pub(crate) fn set_accessibility_owner(&mut self, available: bool) {
        self.accessibility_blocked = !available;
    }

    pub fn is_running(&self) -> bool {
        self.run.as_ref().is_some_and(|r| r.is_running())
    }

    /// Stops the running process (■ Stop here, Ctrl+F2, the toolbar ■).
    pub fn stop(&mut self) {
        if let Some(r) = &mut self.run {
            r.stop();
        }
    }

    pub fn start(&mut self, run: ProfileRun) {
        self.graph_root = None;
        self.log.clear();
        self.log.push((format!("$ {}", run.command_line), false));
        self.report = None;
        self.selected = None;
        self.show_log = true;
        self.run = Some(run);
    }

    pub fn poll(&mut self) -> bool {
        let changed = self.poll_files();
        let Some(run) = &mut self.run else {
            return changed;
        };
        let lines = run.poll();
        let mut changed = changed || !lines.is_empty();
        self.log.extend(lines);
        if !run.is_running() {
            // (An error is in `lines` already.)
            if let Some(r) = run.report() {
                self.report = Some(r.clone());
                self.show_log = false;
            }
            self.run = None;
            changed = true;
        }
        changed
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        mono: egui::FontId,
        root: &Path,
    ) -> Vec<ProfilerEvent> {
        let mut events = Vec::new();
        self.file_controls(ui, root);
        ui.horizontal(|ui| {
            if self.is_running() {
                ui.label(egui::RichText::new("Profiling…").color(theme.warning));
                if ui
                    .button(egui::RichText::new("■ Stop").color(theme.error))
                    .clicked()
                {
                    self.stop();
                }
                if ui.button("Finish and collect").on_hover_text("Interrupt the program and save its profile, including long-running servers").clicked(){if let Some(run)=&mut self.run{run.finish_and_collect();}}
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(80));
            } else if let Some(r) = &self.report {
                ui.label(
                    egui::RichText::new(format!(
                        "{} function(s), {} {}, {:.3}s",
                        r.functions.len(),
                        r.total_calls,
                        if r.sampling{"weighted samples"}else{"calls"},
                        r.total_time
                    ))
                    .strong(),
                );
            } else {
                ui.label(
                    egui::RichText::new("Run → Profile to collect cProfile statistics.")
                        .color(theme.text_dim),
                );
            }
            ui.separator();
            for (label, s) in [
                ("Name",SortBy::Name),
                ("Own time", SortBy::TotTime),
                ("Total time", SortBy::CumTime),
                ("Calls", SortBy::Calls),
            ] {
                if ui.selectable_label(self.sort == s, crate::i18n::tr_static(label)).clicked() {
                    self.sort = s;
                }
            }
            ui.checkbox(&mut self.project_only, "Project code only");
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Filter")
                    .desired_width(140.0),
            );
            ui.toggle_value(&mut self.show_log, "Output");
            ui.checkbox(&mut self.descending,"Descending");
        });
        let source = self
            .report
            .as_ref()
            .and_then(|report| self.selected.and_then(|index| report.functions.get(index)))
            .filter(|function| !function.is_builtin())
            .map(|function| (PathBuf::from(&function.file), function.line));
        if ui
            .add_enabled(
                !self.accessibility_blocked && source.is_some(),
                egui::Button::new("Open selected source"),
            )
            .clicked()
        {
            if let Some((path, line)) = source {
                events.push(ProfilerEvent::Navigate(path, line));
            }
        }
        ui.separator();
        if self.show_log || self.report.is_none() {
            egui::ScrollArea::vertical()
                .id_salt("profile_log")
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .max_height(if self.report.is_some() {
                    100.0
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
        let Some(r) = self.report.clone() else {
            return events;
        };
        ui.horizontal_wrapped(|ui| {
            for (label, view) in [
                ("Statistics", ProfileView::Statistics),
                ("Call Tree", ProfileView::CallTree),
                ("Call Graph", ProfileView::CallGraph),
                ("Flame graph (aggregate)", ProfileView::FlameGraph),
                ("Compare", ProfileView::Comparison),
            ] {
                ui.selectable_value(&mut self.view, view, crate::i18n::tr_static(label));
            }
        });
        if !r.threads.is_empty() {
            ui.collapsing("Threads", |ui| {
                for thread in &r.threads {
                    ui.label(format!(
                        "{} · {} · {:.6}s · {} schedules",
                        thread.id, thread.name, thread.total_time, thread.schedules
                    ));
                }
            });
        }
        if self.view != ProfileView::Statistics {
            self.views_ui(ui, theme, &r, root, &mut events);
            return events;
        }
        let mut order: Vec<usize> = match self.sort {
            SortBy::Name => {
                let mut values: Vec<_> = (0..r.functions.len()).collect();
                values.sort_by_key(|index| r.functions[*index].display_name());
                values.reverse();
                values
            }
            SortBy::TotTime => r.sorted_by_tottime(),
            SortBy::CumTime => r.sorted_by_cumtime(),
            SortBy::Calls => {
                let mut v: Vec<usize> = (0..r.functions.len()).collect();
                v.sort_by(|a, b| r.functions[*b].ncalls.cmp(&r.functions[*a].ncalls));
                v
            }
        };
        if !self.descending {
            order.reverse();
        }
        if self.project_only {
            let proj = r.project_functions(root);
            order.retain(|i| proj.contains(i));
        }
        let filter = self.filter.trim().to_lowercase();
        if !filter.is_empty() {
            order.retain(|i| {
                r.functions[*i]
                    .display_name()
                    .to_lowercase()
                    .contains(&filter)
            });
        }
        // Callers / callees of the selection in a bottom pane.
        if let Some(sel) = self.selected.filter(|s| *s < r.functions.len()) {
            egui::Panel::bottom("profile_calls")
                .resizable(true)
                .default_size(140.0)
                .show(ui, |ui| {
                    let f = &r.functions[sel];
                    ui.label(
                        egui::RichText::new(format!("{}  — callers / callees", f.display_name()))
                            .strong(),
                    );
                    ui.columns(2, |cols| {
                        for (col, (title, list)) in [
                            ("Callers", r.callers_of(sel)),
                            ("Callees", r.callees_of(sel)),
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            let ui = &mut cols[col];
                            ui.label(egui::RichText::new(title).color(theme.text_dim));
                            egui::ScrollArea::vertical()
                                .id_salt(("profile_calls", col))
                                .max_height(100.0)
                                .show(ui, |ui| {
                                    for (idx, calls) in list {
                                        let g = &r.functions[idx];
                                        if ui
                                            .add(
                                                egui::Label::new(
                                                    egui::RichText::new(format!(
                                                        "{} ×{}",
                                                        g.display_name(),
                                                        calls
                                                    ))
                                                    .color(theme.text),
                                                )
                                                .sense(Sense::click()),
                                            )
                                            .clicked()
                                        {
                                            self.selected = Some(idx);
                                        }
                                    }
                                });
                        }
                    });
                });
        }
        egui::ScrollArea::both()
            .id_salt("profile_table")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("profile_grid")
                    .num_columns(6)
                    .striped(true)
                    .spacing([14.0, 3.0])
                    .show(ui, |ui| {
                        for h in [
                            "Function",
                            if r.sampling { "Samples" } else { "Calls" },
                            "Own time",
                            "Own/call",
                            "Total time",
                            "Total/call",
                        ] {
                            ui.label(egui::RichText::new(h).strong());
                        }
                        ui.end_row();
                        for i in order {
                            let f = &r.functions[i];
                            let selected = self.selected == Some(i);
                            let name = egui::RichText::new(f.display_name()).color(if selected {
                                theme.accent
                            } else {
                                theme.text
                            });
                            let resp = ui.add(egui::Label::new(name).sense(Sense::click()));
                            if resp.clicked() {
                                self.selected = Some(i);
                            }
                            if resp.double_clicked() && !f.is_builtin() {
                                events
                                    .push(ProfilerEvent::Navigate(PathBuf::from(&f.file), f.line));
                            }
                            ui.label(if f.ncalls == f.primitive_calls {
                                f.ncalls.to_string()
                            } else {
                                format!("{}/{}", f.ncalls, f.primitive_calls)
                            });
                            ui.label(format!("{:.4}", f.tottime));
                            ui.label(format!("{:.6}", f.per_call()));
                            ui.label(format!("{:.4}", f.cumtime));
                            ui.label(format!("{:.6}", f.cum_per_call()));
                            ui.end_row();
                        }
                    });
            });
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// QA: only the panel's own ■ stopped "Profile"; Ctrl+F2 and the
    /// toolbar ■ now call [`ProfilerPanel::stop`] too.
    #[test]
    fn stop_ends_a_profile_run_with_one_message() {
        let dir = std::env::temp_dir().join(format!("rustpy-profiler-stop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("loop.py"),
            "import time\nprint('tick', flush=True)\nwhile True:\n    time.sleep(0.05)\n",
        )
        .unwrap();
        let mut cfg = rustpy_project::RunConfig::for_script(&dir.join("loop.py"));
        cfg.interpreter = Some("python3".into());
        let mut panel = ProfilerPanel::default();
        let Ok(run) = rustpy_run::run_with_profiler(1, &cfg, None, &dir) else {
            eprintln!("python3 not available; skipping");
            return;
        };
        panel.start(run);
        let wait = |panel: &mut ProfilerPanel, done: &dyn Fn(&ProfilerPanel) -> bool| {
            let start = std::time::Instant::now();
            while !done(panel) && start.elapsed() < std::time::Duration::from_secs(30) {
                panel.poll();
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        };
        wait(&mut panel, &|p| {
            p.log.iter().any(|(l, _)| l == "tick") || !p.is_running()
        });
        if !panel.log.iter().any(|(l, _)| l == "tick") {
            eprintln!("python3 not available; skipping: {:?}", panel.log);
            return;
        }
        assert!(panel.is_running(), "{:?}", panel.log);
        panel.stop();
        wait(&mut panel, &|p| !p.is_running());
        assert!(!panel.is_running());
        let stopped = panel
            .log
            .iter()
            .filter(|(l, err)| {
                // Windows has no interrupt: TerminateProcess leaves no profile data.
                *err && (l.contains("stopped before")
                    || (cfg!(windows) && l.contains("No profile data")))
            })
            .count();
        assert_eq!(stopped, 1, "{:?}", panel.log);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
