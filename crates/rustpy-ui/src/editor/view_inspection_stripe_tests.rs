//! Actual View summary and VCS-only source preview/navigation.
use super::super::Language;
use super::*;

struct Frame {
    shapes: Vec<egui::epaint::ClippedShape>,
    events: Vec<ViewEvent>,
}
fn frame(
    ctx: &egui::Context,
    state: &mut EditorState,
    view: &mut EditorView,
    changes: &[rustpy_vcs::LineChange],
    time: f64,
    events: Vec<egui::Event>,
    reader: bool,
) -> Frame {
    let settings = EditorSettings {
        smooth_scrolling: false,
        ..Default::default()
    };
    let opts = EditOptions::from_settings(&settings);
    let theme = UiTheme::dark();
    let scheme = ColorScheme::dark();
    let keymap = Keymap::default_keymap();
    let mut settings = settings;
    if reader {
        settings.reader_mode.enabled = true;
        settings.reader_mode.show_warnings = false;
    }
    let cfg = ViewConfig {
        scheme: &scheme,
        theme: &theme,
        settings: &settings,
        opts: &opts,
        keymap: &keymap,
        font_size: 14.0,
        search_highlights: &[],
        request_focus: true,
        run_lines: &[],
        line_changes: changes,
        breakpoints: &[],
        exec_line: None,
        inline_values: &[],
        bookmarks: &[],
        gutter_markers: &[],
        gutter_quick_fixes: &[],
        code_vision: None,
        annotations: None,
        coverage: &[],
        keyboard: true,
    };
    let mut produced = Vec::new();
    let output = ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 240.0))),
            ..Default::default()
        },
        |ui| {
            produced = show(ui, state, view, &cfg);
        },
    );
    let shapes = output.shapes.clone();
    output.drop_without_applying_deltas();
    Frame {
        shapes,
        events: produced,
    }
}
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx
}
fn button(point: Pos2, pressed: bool) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(point),
        egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}
#[test]
fn vcs_only_folded_change_hover_and_click_reveal_actual_readonly_source() {
    let source = "def f():\n    hidden = 'π😀'\n    return hidden\n\noutside = 1\n";
    let ctx = context();
    let mut state = EditorState::from_text(source, Language::Python);
    state.collapse_all_folds();
    assert!(!state.hidden_line_ranges().is_empty());
    state.doc.set_read_only(true);
    let changes = [rustpy_vcs::LineChange {
        kind: rustpy_vcs::ChangeKind::Modified,
        start: 1,
        count: 2,
        removed: 2,
        old_text: "old".into(),
    }];
    let mut view = EditorView::new(egui::Id::new("vcs-only-overview"));
    let first = frame(&ctx, &mut state, &mut view, &changes, 0.0, vec![], false);
    let color = rgb(ColorScheme::dark().editor.modified_line);
    let point = first
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Rect(rect)
                if rect.fill == color && (rect.rect.width() - 3.0).abs() < 0.01 =>
            {
                Some(rect.rect.center())
            }
            _ => None,
        })
        .expect("actual adjacent VCS stripe must be painted without diagnostics/search");
    frame(
        &ctx,
        &mut state,
        &mut view,
        &changes,
        0.1,
        vec![egui::Event::PointerMoved(point)],
        false,
    );
    frame(&ctx, &mut state, &mut view, &changes, 1.0, vec![], false);
    let hover = frame(&ctx, &mut state, &mut view, &changes, 1.016, vec![], false);
    assert!(hover
        .shapes
        .iter()
        .any(|shape| matches!(&shape.shape,egui::Shape::Text(text)
        if text.galley.text().contains("return hidden") && text.galley.text().contains("π😀"))));
    frame(
        &ctx,
        &mut state,
        &mut view,
        &changes,
        1.1,
        button(point, true),
        false,
    );
    let clicked = frame(
        &ctx,
        &mut state,
        &mut view,
        &changes,
        1.116,
        button(point, false),
        false,
    );
    assert!(!clicked
        .events
        .iter()
        .any(|event| matches!(event, ViewEvent::OpenProblems)));
    assert_eq!(state.primary().head(), state.doc.line_range(2).start());
    assert!(state.hidden_line_ranges().is_empty());
    assert!(!state.primary().has_selection());
    assert_eq!(state.doc.text(), source);
    assert!(state.doc.is_read_only());
    assert!(!state.doc.can_undo());
}
#[test]
fn actual_summary_emits_dedicated_problems_intent_without_text_selection() {
    let ctx = context();
    let source = "π😀 source\n";
    let mut state = EditorState::from_text(source, Language::PlainText);
    state.semantic_version = Some(u64::MAX);
    let mut view = EditorView::new(egui::Id::new("inspection-summary-intent"));
    let first = frame(&ctx, &mut state, &mut view, &[], 0.0, vec![], false);
    let point = first
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text)
                if text
                    .galley
                    .text()
                    .contains("Analysing… · 0 errors · 0 warnings · 0 other") =>
            {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
        .expect("pending zero findings must not claim completed inspection");
    frame(&ctx, &mut state, &mut view, &[], 0.016, vec![], false);
    frame(
        &ctx,
        &mut state,
        &mut view,
        &[],
        0.032,
        button(point, true),
        false,
    );
    let clicked = frame(
        &ctx,
        &mut state,
        &mut view,
        &[],
        0.048,
        button(point, false),
        false,
    );
    assert_eq!(
        clicked
            .events
            .iter()
            .filter(|event| matches!(event, ViewEvent::OpenProblems))
            .count(),
        1
    );
    assert!(!state.primary().has_selection());
    assert_eq!(state.primary().head(), TextSize::new(0));
    assert_eq!(state.doc.text(), source);
    assert!(!state.doc.can_undo());
}
#[test]
fn reader_summary_counts_exactly_the_visible_error_policy() {
    let ctx = context();
    let mut state = EditorState::from_text("π = '😀'\n", Language::Python);
    state.doc.set_read_only(true);
    state.semantic_version = Some(state.doc.version());
    state.extra_diagnostics = [
        rustpy_core::Severity::Error,
        rustpy_core::Severity::Warning,
        rustpy_core::Severity::WeakWarning,
    ]
    .map(|severity| {
        rustpy_core::Diagnostic::new(
            TextRange::empty(TextSize::new(0)),
            severity,
            "test",
            "finding",
        )
    })
    .to_vec();
    let mut view = EditorView::new(egui::Id::new("inspection-reader-counts"));
    let output = frame(&ctx, &mut state, &mut view, &[], 0.0, vec![], true);
    assert!(super::super::doc_render::reader_mode(&state));
    assert!(output
        .shapes
        .iter()
        .any(|shape| matches!(&shape.shape,egui::Shape::Text(text)
        if text.galley.text()=="1 errors · 0 warnings · 0 other")));
    assert_eq!(state.extra_diagnostics.len(), 3);
    assert_eq!(state.doc.text(), "π = '😀'\n");
    assert!(!state.doc.can_undo());
}
