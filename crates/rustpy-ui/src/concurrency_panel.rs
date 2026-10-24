//! Live thread and asyncio lanes with source-correlated execution/wait spans.
use crate::a11y::DialogWindow;
use crate::theme::UiTheme;
use egui::{Color32, Sense, Vec2};
use rustpy_run::concurrency::{ConcurrencyReport, ConcurrencyRun, Phase};
use std::path::PathBuf;

pub enum ConcurrencyEvent {
    Navigate(PathBuf, usize),
    Rerun,
    Settings,
}
pub struct ConcurrencyPanel {
    pub run: Option<ConcurrencyRun>,
    pub last_config: Option<rustpy_project::RunConfig>,
    pub last_root: Option<PathBuf>,
    imported: Option<ConcurrencyReport>,
    log: Vec<(String, bool)>,
    filter: String,
    zoom: f32,
    pub minimum_duration_ms: f64,
    pub follow_live: bool,
    threads: bool,
    tasks: bool,
    execution: bool,
    locks: bool,
    awaits: bool,
    selected: Option<(usize, usize)>,
    show_log: bool,
    transfer: Option<bool>,
    path: String,
}
impl Default for ConcurrencyPanel {
    fn default() -> Self {
        Self {
            run: None,
            last_config: None,
            last_root: None,
            imported: None,
            log: Vec::new(),
            filter: String::new(),
            zoom: 1.0,
            minimum_duration_ms: 0.0,
            follow_live: true,
            threads: true,
            tasks: true,
            execution: true,
            locks: true,
            awaits: true,
            selected: None,
            show_log: false,
            transfer: None,
            path: String::new(),
        }
    }
}
fn phase_color(phase: Phase, theme: &UiTheme) -> Color32 {
    match phase {
        Phase::Execution => theme.accent,
        Phase::LockWait => theme.warning,
        Phase::Await => Color32::from_rgb(178, 132, 235),
    }
}
impl ConcurrencyPanel {
    pub fn start(&mut self, run: ConcurrencyRun, config: rustpy_project::RunConfig, root: PathBuf) {
        self.log = vec![(format!("$ {}", run.command_line), false)];
        self.run = Some(run);
        self.last_config = Some(config);
        self.last_root = Some(root);
        self.imported = None;
        self.selected = None;
    }
    pub fn is_running(&self) -> bool {
        self.run.as_ref().is_some_and(ConcurrencyRun::is_running)
    }
    pub fn stop(&mut self) {
        if let Some(run) = &mut self.run {
            run.stop();
        }
    }
    pub fn poll(&mut self) -> bool {
        let Some(run) = &mut self.run else {
            return false;
        };
        let was_running = run.is_running();
        let events = run.poll();
        let changed = !events.is_empty() || was_running;
        self.log.extend(events);
        if was_running && !run.is_running() {
            self.log.push((
                match run.exit_code() {
                    Some(code) => format!("Process finished with exit code {code}"),
                    None => "Process terminated".into(),
                },
                run.exit_code() != Some(0),
            ));
        }
        if self.log.len() > 20_000 {
            self.log.drain(..self.log.len() - 20_000);
        }
        changed
    }
    fn report(&self) -> Option<&ConcurrencyReport> {
        self.imported
            .as_ref()
            .or_else(|| self.run.as_ref().map(ConcurrencyRun::report))
    }
    pub fn ui(&mut self, ui: &mut egui::Ui, theme: &UiTheme) -> Vec<ConcurrencyEvent> {
        let mut events = Vec::new();
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    self.last_config.is_some() && !self.is_running(),
                    egui::Button::new("Rerun"),
                )
                .clicked()
            {
                events.push(ConcurrencyEvent::Rerun);
            }
            if ui
                .add_enabled(self.is_running(), egui::Button::new("Interrupt"))
                .on_hover_text("Interrupt Python and retain the captured timeline")
                .clicked()
            {
                if let Some(run) = &mut self.run {
                    run.interrupt();
                }
            }
            if ui
                .add_enabled(self.is_running(), egui::Button::new("Stop"))
                .clicked()
            {
                self.stop();
            }
            if ui.button("Settings").clicked() {
                events.push(ConcurrencyEvent::Settings);
            }
            ui.label("Filter");
            ui.add(egui::TextEdit::singleline(&mut self.filter).desired_width(150.0));
            if ui.button("Fit").clicked() {
                self.zoom = 1.0;
            }
            ui.add(
                egui::Slider::new(&mut self.zoom, 0.1..=100.0)
                    .logarithmic(true)
                    .text("Zoom"),
            );
            ui.checkbox(&mut self.follow_live, "Follow live");
            ui.checkbox(&mut self.show_log, "Output");
            ui.menu_button("Lanes / spans", |ui| {
                ui.checkbox(&mut self.threads, "Threads");
                ui.checkbox(&mut self.tasks, "Asyncio tasks");
                ui.checkbox(&mut self.execution, "Execution");
                ui.checkbox(&mut self.locks, "Lock waits");
                ui.checkbox(&mut self.awaits, "Awaits");
                ui.add(
                    egui::DragValue::new(&mut self.minimum_duration_ms)
                        .range(0.0..=10000.0)
                        .suffix(" ms minimum"),
                );
            });
            if ui
                .add_enabled(self.report().is_some(), egui::Button::new("Export…"))
                .clicked()
            {
                self.transfer = Some(true);
                self.path = self
                    .last_root
                    .as_ref()
                    .map(|root| root.join("concurrency.json").display().to_string())
                    .unwrap_or_else(|| "concurrency.json".into());
            }
            if ui
                .add_enabled(!self.is_running(), egui::Button::new("Import…"))
                .clicked()
            {
                self.transfer = Some(false);
                self.path.clear();
            }
        });
        let running = self.is_running();
        let live_time = self
            .run
            .as_ref()
            .filter(|run| run.is_running())
            .map(|run| run.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        if let Some(run) = &self.run {
            if let Some(error) = run.error() {
                ui.label(egui::RichText::new(error).color(theme.error));
            }
        }
        if self.show_log {
            egui::CollapsingHeader::new("Program output")
                .default_open(true)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("concurrency-output")
                        .max_height(120.0)
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            for (line, error) in &self.log {
                                ui.label(egui::RichText::new(line).monospace().color(if *error {
                                    theme.error
                                } else {
                                    theme.text
                                }));
                            }
                        });
                });
        }
        let Some(report) = self.report() else {
            ui.label("Run → Run with Concurrency Diagram captures threads, asyncio tasks, lock acquisition waits and coroutine awaits.");
            self.transfer_ui(ui.ctx());
            return events;
        };
        let duration = report.duration.max(live_time).max(0.001);
        ui.horizontal_wrapped(|ui| {
            for (phase, label) in [
                (Phase::Execution, "Execution (call lifetime)"),
                (Phase::LockWait, "Lock acquisition wait"),
                (Phase::Await, "Await / suspension"),
            ] {
                ui.label(egui::RichText::new(label).color(phase_color(phase, theme)));
            }
            ui.label(format!(
                "{} lanes · {} events · {:.3}s{}",
                report.lanes.len(),
                report.event_count,
                duration,
                if running {
                    " · live"
                } else if report.terminated {
                    " · terminated"
                } else {
                    ""
                }
            ));
        });
        if report.dropped > 0 {
            ui.label(egui::RichText::new(format!("Capture limits omitted {} event(s). Increase the event/lane limits in Thread Concurrency Visualization settings.",report.dropped)).color(theme.warning));
        }
        let filter = self.filter.to_lowercase();
        let lanes: Vec<_> = report
            .lanes
            .iter()
            .enumerate()
            .filter(|(_, lane)| {
                (self.threads || lane.kind != "thread")
                    && (self.tasks || lane.kind != "task")
                    && (filter.is_empty()
                        || lane.name.to_lowercase().contains(&filter)
                        || lane
                            .spans
                            .iter()
                            .any(|span| span.name.to_lowercase().contains(&filter)))
            })
            .collect();
        let label_width = 190.0;
        let timeline_width = (ui.available_width() - label_width).max(100.0) * self.zoom;
        let scale = timeline_width / duration as f32;
        let row_height = 66.0;
        let mut selected = self.selected;
        let mut navigate = None;
        egui::ScrollArea::both()
            .id_salt("concurrency-timeline")
            .auto_shrink([false, false])
            .stick_to_right(self.follow_live && running)
            .show_viewport(ui, |ui, viewport| {
                let size = Vec2::new(
                    label_width + timeline_width,
                    (lanes.len() as f32 * row_height + 28.0).max(100.0),
                );
                let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
                let origin = rect.left() + label_width;
                let timeline_clip = ui.clip_rect().intersect(egui::Rect::from_min_max(
                    egui::pos2(rect.left() + viewport.min.x + label_width, rect.top()),
                    rect.max,
                ));
                let painter = ui.painter().with_clip_rect(timeline_clip);
                for tick in 0..=10 {
                    let time = duration * tick as f64 / 10.0;
                    let x = origin + time as f32 * scale;
                    ui.painter().line_segment(
                        [
                            egui::pos2(x, rect.top() + 24.0),
                            egui::pos2(x, rect.bottom()),
                        ],
                        egui::Stroke::new(1.0, theme.border),
                    );
                    ui.painter().text(
                        egui::pos2(x + 3.0, rect.top() + 2.0),
                        egui::Align2::LEFT_TOP,
                        format!("{time:.3}s"),
                        egui::FontId::proportional(10.0),
                        theme.text_dim,
                    );
                }
                for (row, (lane_index, lane)) in lanes.iter().enumerate() {
                    let y = rect.top() + 28.0 + row as f32 * row_height;
                    if y + row_height < rect.top() + viewport.min.y
                        || y > rect.top() + viewport.max.y
                    {
                        continue;
                    }
                    let lane_rect = egui::Rect::from_min_size(
                        egui::pos2(rect.left(), y),
                        Vec2::new(size.x, row_height),
                    );
                    if row.is_multiple_of(2) {
                        ui.painter()
                            .rect_filled(lane_rect, 0.0, theme.panel_bg_darker);
                    }
                    let name = if lane.kind == "task" {
                        format!("  ◇ {}", lane.name)
                    } else {
                        format!("▤ {}", lane.name)
                    };
                    ui.painter().text(
                        egui::pos2(rect.left() + viewport.min.x + 6.0, y + 4.0),
                        egui::Align2::LEFT_TOP,
                        name,
                        egui::FontId::proportional(12.0),
                        theme.text,
                    );
                    for (span_index, span) in lane.spans.iter().enumerate() {
                        if span.duration() * 1000.0 < self.minimum_duration_ms {
                            continue;
                        }
                        let band = match span.phase {
                            Phase::Execution if self.execution => 0.0,
                            Phase::LockWait if self.locks => 20.0,
                            Phase::Await if self.awaits => 40.0,
                            _ => continue,
                        };
                        let x = origin + span.start as f32 * scale;
                        let width = ((span.end - span.start) as f32 * scale).max(2.0);
                        if x + width < rect.left() + viewport.min.x + label_width
                            || x > rect.left() + viewport.max.x
                        {
                            continue;
                        }
                        let bar = egui::Rect::from_min_size(
                            egui::pos2(x, y + band + 3.0 + (span.depth.min(3) as f32 * 2.0)),
                            Vec2::new(width, 14.0),
                        );
                        let response = ui.interact(
                            bar.intersect(timeline_clip),
                            ui.id().with(("concurrency-span", lane_index, span_index)),
                            Sense::click(),
                        );
                        let color = phase_color(span.phase, theme);
                        painter.rect_filled(
                            bar,
                            2.0,
                            if span.incomplete {
                                color.gamma_multiply(0.55)
                            } else {
                                color
                            },
                        );
                        if selected == Some((*lane_index, span_index)) {
                            painter.rect_stroke(
                                bar,
                                2.0,
                                egui::Stroke::new(2.0, theme.text),
                                egui::StrokeKind::Inside,
                            );
                        }
                        if response.clicked() {
                            selected = Some((*lane_index, span_index));
                        }
                        if response.double_clicked() {
                            if let Some(path) = &span.file {
                                navigate = Some((path.clone(), span.line));
                            }
                        }
                        response.on_hover_ui(|ui| {
                            ui.strong(&span.name);
                            ui.label(format!(
                                "{:.3}–{:.3}s · {:.3} ms",
                                span.start,
                                span.end,
                                span.duration() * 1000.0
                            ));
                            if let Some(resource) = &span.resource {
                                ui.label(resource);
                            }
                            if let Some(file) = &span.file {
                                ui.label(format!("{}:{}", file.display(), span.line));
                            }
                            if span.incomplete {
                                ui.label("Span was still open when capture ended");
                            }
                        });
                    }
                    for pending in report.pending_spans().filter(|event| event.lane == lane.id) {
                        let band = match pending.phase {
                            Phase::Execution if self.execution => 0.0,
                            Phase::LockWait if self.locks => 20.0,
                            Phase::Await if self.awaits => 40.0,
                            _ => continue,
                        };
                        let x = origin + pending.time as f32 * scale;
                        let width = ((duration - pending.time) as f32 * scale).max(2.0);
                        painter.rect_filled(
                            egui::Rect::from_min_size(
                                egui::pos2(x, y + band + 3.0),
                                Vec2::new(width, 14.0),
                            ),
                            2.0,
                            phase_color(pending.phase, theme).gamma_multiply(0.6),
                        );
                    }
                }
            });
        if let Some((lane_index, span_index)) = selected {
            if let Some(span) = report
                .lanes
                .get(lane_index)
                .and_then(|lane| lane.spans.get(span_index))
            {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.strong(&span.name);
                    ui.label(format!("{:.3} ms", span.duration() * 1000.0));
                    if let Some(path) = &span.file {
                        if ui
                            .link(format!("{}:{}", path.display(), span.line))
                            .clicked()
                        {
                            navigate = Some((path.clone(), span.line));
                        }
                    }
                });
            }
        }
        self.selected = selected;
        if let Some((path, line)) = navigate {
            events.push(ConcurrencyEvent::Navigate(path, line));
        }
        self.transfer_ui(ui.ctx());
        events
    }
    fn transfer_ui(&mut self, ctx: &egui::Context) {
        let Some(export) = self.transfer else { return };
        let mut open = true;
        let mut apply = false;
        egui::Window::new(if export {
            "Export concurrency report"
        } else {
            "Import concurrency report"
        })
        .collapsible(false)
        .open(&mut open)
        .show_modal(ctx, |ui| {
            ui.label("JSON report file path");
            ui.add(egui::TextEdit::singleline(&mut self.path).desired_width(440.0));
            if ui
                .add_enabled(
                    !self.path.trim().is_empty(),
                    egui::Button::new(if export { "Export" } else { "Import" }),
                )
                .clicked()
            {
                apply = true;
            }
        });
        if !open {
            self.transfer = None;
            return;
        }
        if apply {
            let path = PathBuf::from(self.path.trim());
            let result = if export {
                self.report()
                    .ok_or("No report".to_string())
                    .and_then(|report| {
                        let mut snapshot = report.clone();
                        if let Some(run) = self.run.as_ref().filter(|run| run.is_running()) {
                            snapshot.close_open_spans(run.elapsed().as_secs_f64(), false);
                        }
                        serde_json::to_vec_pretty(&snapshot).map_err(|error| error.to_string())
                    })
                    .and_then(|bytes| {
                        std::fs::write(&path, bytes).map_err(|error| error.to_string())
                    })
            } else {
                std::fs::metadata(&path)
                    .map_err(|error| error.to_string())
                    .and_then(|metadata| {
                        if metadata.len() > 64 * 1024 * 1024 {
                            Err("Concurrency report exceeds 64 MiB".into())
                        } else {
                            std::fs::read_to_string(&path).map_err(|error| error.to_string())
                        }
                    })
                    .and_then(|text| ConcurrencyReport::from_json(&text))
                    .map(|report| {
                        self.imported = Some(report);
                        self.run = None;
                        self.selected = None;
                        self.last_config = None;
                    })
            };
            match result {
                Ok(()) => self.transfer = None,
                Err(error) => {
                    self.show_log = true;
                    self.log.push((error, true));
                }
            }
        }
    }
}
