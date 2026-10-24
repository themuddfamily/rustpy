//! Editor inspection summary and fold-aware VCS overview data.
//! The owning view supplies analysis readiness and paints/navigates markers.
use egui::{Align2, Color32, FontId, Id, Rect, Sense, Ui};
use rustpy_core::{Diagnostic, Severity, TextRange, TextSize};
use rustpy_text::Document;
use rustpy_vcs::{ChangeKind, LineChange};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Readiness {
    /// Semantic results match the current document version.
    Complete,
    /// A real analysis job is running; syntax findings can still be shown.
    Pending,
    /// Only the currently available findings are known (not a completed analysis).
    CurrentFindings,
    Unavailable,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Summary {
    pub errors: usize,
    pub warnings: usize,
    pub other: usize,
}
impl Summary {
    pub fn from_diagnostics(diagnostics: &[Diagnostic]) -> Self {
        let mut summary = Self::default();
        for diagnostic in diagnostics {
            match diagnostic.severity {
                Severity::Error => summary.errors += 1,
                Severity::Warning | Severity::WeakWarning => summary.warnings += 1,
                Severity::Info | Severity::Typo => summary.other += 1,
            }
        }
        summary
    }
    pub fn label(self, readiness: Readiness) -> String {
        let counts = format!(
            "{} errors · {} warnings · {} other",
            self.errors, self.warnings, self.other
        );
        match readiness {
            Readiness::Complete if self == Self::default() => "No problems found".into(),
            Readiness::Complete => counts,
            Readiness::Pending => format!("Analysing… · {counts}"),
            Readiness::CurrentFindings => format!("Current findings · {counts}"),
            Readiness::Unavailable => "Inspections unavailable".into(),
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct WidgetOutput {
    /// The App bridge must open Problems, rather than toggle a visible panel off.
    pub open_problems: bool,
    pub consumed_pointer: bool,
}
/// A pointer-only control: it must not become an editor Tab/indent focus stop.
pub(super) fn show(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    summary: Summary,
    readiness: Readiness,
    text_color: Color32,
    background: Color32,
) -> WidgetOutput {
    let rect = rect.intersect(ui.clip_rect());
    if !rect.is_positive() {
        return WidgetOutput::default();
    }
    let response = ui.interact(rect, id, Sense::CLICK);
    let label = summary.label(readiness);
    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, 3.0, background);
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        &label,
        FontId::proportional(12.0),
        text_color,
    );
    response
        .clone()
        .on_hover_text(format!("{label}\nOpen current-file Problems"));
    WidgetOutput {
        open_problems: response.clicked(),
        consumed_pointer: response.contains_pointer()
            && ui.input(|input| {
                input.pointer.primary_down()
                    || input.pointer.button_released(egui::PointerButton::Primary)
            }),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct VcsMarker {
    pub kind: ChangeKind,
    pub first_row: usize,
    pub last_row: usize,
    /// Full actual changed range; pure deletions have an empty current-source anchor.
    pub source: TextRange,
    /// Deepest changed source line represented, including inside a folded body.
    pub target: TextSize,
}
impl VcsMarker {
    pub fn description(self) -> &'static str {
        match self.kind {
            ChangeKind::Added => "Added lines",
            ChangeKind::Modified => "Modified lines",
            ChangeKind::Deleted => "Deleted lines",
        }
    }
}
/// `visible_lines` is the existing sorted view row→source line map. Duplicate
/// lines from soft wrapping map to their first visible row. Hidden source lines
/// map to the preceding visible header. One output per hunk, never per source
/// line: huge hunks require only two logarithmic row lookups.
pub(super) fn vcs_markers(
    doc: &Document,
    changes: &[LineChange],
    visible_lines: &[usize],
) -> Vec<VcsMarker> {
    if visible_lines.is_empty() || doc.line_count() == 0 {
        return Vec::new();
    }
    let row_for_line = |line: usize| {
        let after = visible_lines.partition_point(|visible| *visible <= line);
        let visible = visible_lines[after.saturating_sub(1)];
        visible_lines.partition_point(|candidate| *candidate < visible)
    };
    let last_line = doc.line_count() - 1;
    changes
        .iter()
        .filter_map(|change| {
            let first = change.start.min(last_line);
            if change.kind != ChangeKind::Deleted && (change.count == 0 || change.start > last_line)
            {
                return None;
            }
            let last = if change.kind == ChangeKind::Deleted {
                first
            } else {
                change.start.saturating_add(change.count - 1).min(last_line)
            };
            let start = doc.line_range(first).start();
            let source = if change.kind == ChangeKind::Deleted {
                TextRange::empty(start)
            } else {
                TextRange::new(start, doc.line_range(last).end())
            };
            Some(VcsMarker {
                kind: change.kind,
                first_row: row_for_line(first),
                last_row: row_for_line(last),
                source,
                target: doc.line_range(last).start(),
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "view_inspection_widget_tests.rs"]
mod tests;
