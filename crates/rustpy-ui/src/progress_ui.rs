//! Background task progress in the status bar and the Background Tasks popup
//! (roadmap §0.1): the newest running task with its progress bar and a cancel
//! button, "+N" when more run; a click lists every running task and the
//! recently finished ones.

use crate::a11y::DialogWindow;
use std::time::Duration;

use egui::{Align2, Color32, Pos2, Rect, RichText, Sense, Vec2};
use rustpy_core::tasks::{TaskInfo, TaskManager, TaskOutcome};

use crate::theme::UiTheme;

/// Tasks younger than this are not shown: quick work does not flicker.
pub const SHOW_AFTER: Duration = Duration::from_millis(300);

/// The running tasks worth showing in the status bar, newest first.
pub fn shown_tasks(running: &[TaskInfo]) -> Vec<&TaskInfo> {
    running
        .iter()
        .filter(|t| !t.hidden && t.elapsed >= SHOW_AFTER)
        .collect()
}

/// `1.2 s`, `350 ms`, `2 min 05 s`.
pub fn format_elapsed(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1} s", ms as f64 / 1000.0)
    } else {
        format!("{} min {:02} s", ms / 60_000, (ms / 1000) % 60)
    }
}

/// `s` cut to `max` characters with an ellipsis.
pub fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// One-line summary of a finished task.
pub fn outcome_text(t: &TaskInfo) -> String {
    match &t.outcome {
        Some(TaskOutcome::Finished) | None => format!("finished in {}", format_elapsed(t.elapsed)),
        Some(TaskOutcome::Cancelled) => format!("cancelled after {}", format_elapsed(t.elapsed)),
        Some(TaskOutcome::Failed(m)) => format!(
            "failed: {}",
            ellipsize(
                m.lines()
                    .rfind(|l| !l.trim().is_empty())
                    .unwrap_or(m)
                    .trim(),
                90
            )
        ),
    }
}

/// A thin progress bar; `None` animates an indeterminate one.
pub fn progress_bar(
    ui: &mut egui::Ui,
    fraction: Option<f32>,
    width: f32,
    theme: &UiTheme,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 12.0), Sense::click());
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::ProgressIndicator,
            true,
            "Background task progress",
        )
    });
    resp.ctx.accesskit_node_builder(resp.id, |node| {
        node.set_role(egui::accesskit::Role::ProgressIndicator);
        if let Some(fraction) = fraction {
            node.set_numeric_value(f64::from(fraction.clamp(0.0, 1.0)));
            node.set_min_numeric_value(0.0);
            node.set_max_numeric_value(1.0);
        }
    });
    let bar = Rect::from_center_size(rect.center(), Vec2::new(width, 5.0));
    let painter = ui.painter();
    painter.rect_filled(
        bar,
        2.5,
        if theme.dark {
            Color32::from_gray(0x55)
        } else {
            Color32::from_gray(0xC8)
        },
    );
    match fraction {
        Some(f) => {
            let w = bar.width() * f.clamp(0.0, 1.0);
            painter.rect_filled(
                Rect::from_min_size(bar.min, Vec2::new(w, bar.height())),
                2.5,
                theme.accent,
            );
        }
        None if crate::a11y::reduced_motion() => {
            painter.rect_filled(
                Rect::from_center_size(bar.center(), Vec2::new(bar.width() * 0.3, bar.height())),
                2.5,
                theme.accent,
            );
        }
        None => {
            // A segment sliding across the bar.
            let t = ui.input(|i| i.time) as f32;
            let seg = bar.width() * 0.3;
            let x = ((t * 0.9).fract() * (bar.width() + seg)) - seg;
            let left = (bar.left() + x).max(bar.left());
            let right = (bar.left() + x + seg).min(bar.right());
            if right > left {
                painter.rect_filled(
                    Rect::from_min_max(Pos2::new(left, bar.top()), Pos2::new(right, bar.bottom())),
                    2.5,
                    theme.accent,
                );
            }
            // 20 fps: smooth enough, and a long indeterminate task does not keep the UI busy.
            ui.ctx().request_repaint_after(Duration::from_millis(50));
        }
    }
    resp
}

/// The status bar part. Lays out right to left (it sits in the right-hand
/// part of the status bar); returns true when the popup should toggle.
pub fn status_widget(ui: &mut egui::Ui, theme: &UiTheme, manager: &TaskManager) -> bool {
    let running = manager.running();
    let shown = shown_tasks(&running);
    if running.iter().any(|t| !t.hidden) {
        // Young tasks become visible after SHOW_AFTER without other repaints.
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }
    let Some(top) = shown.first() else {
        return false;
    };
    let mut toggle = false;
    if top.cancellable && !top.cancel_requested {
        let label = crate::i18n::tr_fmt("Cancel {task}", &[("task", &top.title)]);
        let cancel = ui
            .add(
                egui::Button::new(RichText::new("×").color(theme.text_dim).size(14.0)).frame(false),
            )
            .on_hover_text(&label);
        // Screen readers announce the task, not the "×" glyph.
        cancel
            .ctx
            .accesskit_node_builder(cancel.id, |node| node.set_label(label));
        if cancel.clicked() {
            manager.cancel(top.id);
        }
    }
    let tooltip = |t: &TaskInfo| {
        let mut s = t.title.clone();
        if !t.text.is_empty() {
            s.push_str(&format!("\n{}", t.text));
        }
        if !t.details.is_empty() {
            s.push_str(&format!("\n{}", t.details));
        }
        s
    };
    if progress_bar(ui, top.fraction, 96.0, theme)
        .on_hover_text(tooltip(top))
        .clicked()
    {
        toggle = true;
    }
    let title = if top.cancel_requested {
        format!("{} (cancelling…)", ellipsize(&top.title, 36))
    } else {
        ellipsize(&top.title, 40)
    };
    if ui
        .add(egui::Label::new(RichText::new(title).color(theme.text)).sense(Sense::click()))
        .on_hover_text("Show all background tasks")
        .clicked()
    {
        toggle = true;
    }
    if shown.len() > 1
        && ui
            .add(
                egui::Label::new(
                    RichText::new(format!("+{}", shown.len() - 1)).color(theme.accent),
                )
                .sense(Sense::click()),
            )
            .on_hover_text(format!("{} background tasks are running", shown.len()))
            .clicked()
    {
        toggle = true;
    }
    toggle
}

/// The Background Tasks popup (anchored above the status bar).
pub fn tasks_popup(ctx: &egui::Context, theme: &UiTheme, manager: &TaskManager, open: &mut bool) {
    if !*open {
        return;
    }
    let running = manager.running();
    let finished = manager.finished();
    let mut keep_open = true;
    egui::Window::new("Background Tasks")
        .id(egui::Id::new("background_tasks_popup"))
        .collapsible(false)
        .resizable(false)
        .default_width(440.0)
        .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-12.0, -30.0))
        .open(&mut keep_open)
        .show_modeless(ctx, |ui| {
            ui.set_min_width(420.0);
            if running.is_empty() {
                ui.label(RichText::new("No background tasks are running").color(theme.text_dim));
            }
            for t in &running {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&t.title).strong().color(theme.text));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if t.cancel_requested {
                            ui.label(RichText::new("cancelling…").color(theme.text_dim).small());
                        } else if t.cancellable && ui.small_button("Cancel").clicked() {
                            manager.cancel(t.id);
                        }
                        ui.label(
                            RichText::new(format_elapsed(t.elapsed))
                                .color(theme.text_dim)
                                .small(),
                        );
                    });
                });
                let width = ui.available_width();
                progress_bar(ui, t.fraction, width, theme);
                let detail = [t.text.as_str(), t.details.as_str()]
                    .iter()
                    .filter(|s| !s.is_empty())
                    .copied()
                    .collect::<Vec<_>>()
                    .join(" · ");
                if !detail.is_empty() {
                    ui.label(
                        RichText::new(ellipsize(&detail, 80))
                            .color(theme.text_dim)
                            .small(),
                    );
                }
                ui.add_space(4.0);
            }
            if !finished.is_empty() {
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Recently finished")
                            .color(theme.text_dim)
                            .small(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Clear").clicked() {
                            manager.clear_finished();
                        }
                    });
                });
                for t in finished.iter().take(8) {
                    let (icon, color) = match &t.outcome {
                        Some(TaskOutcome::Failed(_)) => ("×", theme.error),
                        Some(TaskOutcome::Cancelled) => ("–", theme.text_dim),
                        _ => ("✔", theme.success),
                    };
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icon).color(color));
                        ui.label(RichText::new(&t.title).color(theme.text));
                        ui.label(RichText::new(outcome_text(t)).color(theme.text_dim).small());
                    });
                }
            }
        });
    if !running.is_empty() {
        ctx.request_repaint_after(Duration::from_millis(100));
    }
    if !keep_open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        *open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(title: &str, elapsed_ms: u64, hidden: bool) -> TaskInfo {
        TaskInfo {
            id: 1,
            title: title.into(),
            text: String::new(),
            details: String::new(),
            fraction: None,
            cancellable: true,
            cancel_requested: false,
            hidden,
            elapsed: Duration::from_millis(elapsed_ms),
            cpu_time: None,
            outcome: None,
        }
    }

    #[test]
    fn young_and_hidden_tasks_are_not_shown() {
        let running = vec![
            info("new", 50, false),
            info("analysis", 5000, true),
            info("search", 900, false),
            info("index", 4000, false),
        ];
        let shown: Vec<&str> = shown_tasks(&running)
            .iter()
            .map(|t| t.title.as_str())
            .collect();
        assert_eq!(shown, vec!["search", "index"]);
    }

    #[test]
    fn durations_and_outcomes_read_naturally() {
        assert_eq!(format_elapsed(Duration::from_millis(350)), "350 ms");
        assert_eq!(format_elapsed(Duration::from_millis(1250)), "1.2 s");
        assert_eq!(format_elapsed(Duration::from_secs(125)), "2 min 05 s");
        let mut t = info("Push", 1500, false);
        t.outcome = Some(TaskOutcome::Failed(
            "git push failed:\nfatal: unable to access\n".into(),
        ));
        assert_eq!(outcome_text(&t), "failed: fatal: unable to access");
        t.outcome = Some(TaskOutcome::Cancelled);
        assert_eq!(outcome_text(&t), "cancelled after 1.5 s");
        assert_eq!(ellipsize("abcdef", 4), "abc…");
        assert_eq!(ellipsize("äbc", 3), "äbc");
    }
}
