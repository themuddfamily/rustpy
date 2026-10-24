//! Real egui summary interactions plus source/fold-aware planner oracles.
use super::*;

fn diagnostic(severity: Severity) -> Diagnostic {
    Diagnostic::new(
        TextRange::empty(TextSize::new(0)),
        severity,
        "test",
        "finding",
    )
}
#[test]
fn summary_counts_and_readiness_never_claim_pending_zero_is_complete() {
    let diagnostics = [
        Severity::Error,
        Severity::Warning,
        Severity::WeakWarning,
        Severity::Typo,
        Severity::Info,
    ]
    .map(diagnostic);
    let summary = Summary::from_diagnostics(&diagnostics);
    assert_eq!(
        summary,
        Summary {
            errors: 1,
            warnings: 2,
            other: 2
        }
    );
    assert_eq!(
        summary.label(Readiness::Complete),
        "1 errors · 2 warnings · 2 other"
    );
    let empty = Summary::default();
    assert_eq!(empty.label(Readiness::Complete), "No problems found");
    assert!(empty.label(Readiness::Pending).starts_with("Analysing…"));
    assert!(empty
        .label(Readiness::CurrentFindings)
        .starts_with("Current findings"));
    assert_eq!(
        empty.label(Readiness::Unavailable),
        "Inspections unavailable"
    );
}
#[test]
fn rendered_widget_reports_open_problems_without_becoming_a_focus_stop() {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    let mut field = "π😀".to_string();
    let editor = Id::new("inspection-test-editor");
    let rect = Rect::from_min_size(egui::pos2(240.0, 20.0), egui::vec2(330.0, 24.0));
    let summary = Summary {
        errors: 1,
        warnings: 2,
        other: 0,
    };
    let mut clicked = false;
    for (time, pressed) in [
        (0.0, None),
        (0.016, None),
        (0.032, Some(true)),
        (0.048, Some(false)),
    ] {
        let events = pressed
            .map(|pressed| {
                vec![
                    egui::Event::PointerMoved(rect.center()),
                    egui::Event::PointerButton {
                        pos: rect.center(),
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]
            })
            .unwrap_or_default();
        let output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                events,
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(600.0, 200.0),
                )),
                ..Default::default()
            },
            |ui| {
                ui.add(egui::TextEdit::singleline(&mut field).id(editor));
                if pressed.is_none() {
                    ui.memory_mut(|memory| memory.request_focus(editor));
                }
                let result = show(
                    ui,
                    rect,
                    Id::new("inspection-test-summary"),
                    summary,
                    Readiness::Pending,
                    Color32::WHITE,
                    Color32::BLACK,
                );
                if pressed.is_some() {
                    assert!(result.consumed_pointer);
                }
                clicked |= result.open_problems;
                assert!(!ui.memory(|memory| memory.has_focus(Id::new("inspection-test-summary"))));
            },
        );
        assert!(output
            .shapes
            .iter()
            .any(|shape| matches!(&shape.shape, egui::Shape::Text(text)
            if text.galley.text().contains("Analysing… · 1 errors · 2 warnings · 0 other"))));
        output.drop_without_applying_deltas();
    }
    assert!(clicked);
    assert_eq!(field, "π😀");
}
fn change(kind: ChangeKind, start: usize, count: usize) -> LineChange {
    LineChange {
        kind,
        start,
        count,
        removed: 1,
        old_text: "old".into(),
    }
}
#[test]
fn vcs_ranges_retain_folded_deepest_source_and_deleted_eof_anchor() {
    let doc = Document::new("header\n    π😀\n    changed\noutside\n");
    let changes = [
        change(ChangeKind::Added, 1, 2),
        change(ChangeKind::Modified, 3, 1),
        change(ChangeKind::Deleted, usize::MAX, 0),
    ];
    // Lines 1 and 2 are hidden; line 3 has two soft-wrap rows.
    let markers = vcs_markers(&doc, &changes, &[0, 3, 3, 4]);
    assert_eq!(markers.len(), 3);
    assert_eq!((markers[0].first_row, markers[0].last_row), (0, 0));
    assert_eq!(
        markers[0].source,
        TextRange::new(doc.line_range(1).start(), doc.line_range(2).end())
    );
    assert_eq!(markers[0].target, doc.line_range(2).start());
    assert_eq!((markers[1].first_row, markers[1].last_row), (1, 1));
    assert_eq!(
        markers[2].source,
        TextRange::empty(doc.line_range(4).start())
    );
    assert_eq!(markers[2].target, doc.line_range(4).start());
    assert_eq!(markers[2].first_row, 3);
}
#[test]
fn huge_hunks_are_clamped_without_per_line_marker_expansion_or_mutation() {
    let doc = Document::new("a\nb\nc\n");
    let source = doc.text();
    let changes = [
        change(ChangeKind::Modified, 1, usize::MAX),
        change(ChangeKind::Added, usize::MAX, 1),
        change(ChangeKind::Added, 0, 0),
    ];
    let markers = vcs_markers(&doc, &changes, &[0, 1, 2, 3]);
    assert_eq!(markers.len(), 1);
    assert_eq!((markers[0].first_row, markers[0].last_row), (1, 3));
    assert_eq!(markers[0].target, doc.line_range(3).start());
    assert_eq!(doc.text(), source);
    assert!(!doc.can_undo());
    assert!(vcs_markers(&doc, &changes, &[]).is_empty());
}
