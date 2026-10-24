//! Actual EditorView paint/cache/fold/wrap TODO regressions.
use super::*;
use egui::Id;
use rustpy_settings::todo::{TodoPattern, TodoSettings};
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx
}
const COLOR: [u8; 3] = [254, 37, 139];
fn policy() -> TodoSettings {
    TodoSettings {
        patterns: vec![TodoPattern {
            regex: r"\bREVIEW\b".into(),
            case_sensitive: true,
            color: COLOR,
        }],
        multiline: true,
        ..Default::default()
    }
}
fn painted(output: &egui::FullOutput, needle: &str, color: Color32) -> bool {
    output.shapes.iter().any(|shape| match &shape.shape {
        egui::Shape::Text(text) => text.galley.job.sections.iter().any(|section| {
            section.format.color == color
                && text.galley.job.text[section.byte_range.start.0..section.byte_range.end.0]
                    .contains(needle)
        }),
        _ => false,
    })
}
fn frame(
    ctx: &egui::Context,
    state: &mut EditorState,
    view: &mut EditorView,
    wrap: bool,
    time: f64,
    events: Vec<egui::Event>,
) -> (egui::FullOutput, Vec<ViewEvent>) {
    let scheme = ColorScheme::dark();
    let theme = UiTheme::dark();
    let settings = EditorSettings {
        soft_wrap: wrap,
        smooth_scrolling: false,
        ..Default::default()
    };
    let opts = EditOptions::default();
    let keymap = Keymap::default_keymap();
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
        line_changes: &[],
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
    let mut result = Vec::new();
    let output = ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            focused: true,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(300.0, 480.0))),
            ..Default::default()
        },
        |ui| result = show(ui, state, view, &cfg),
    );
    (output, result)
}

#[test]
fn actual_custom_color_unicode_continuation_and_string_exclusion_preserve_document() {
    let source = "x = '# REVIEW string'\n# review lower\n# REVIEW π😀\n#  continuation Ω\nx=2\n";
    let mut state = EditorState::from_text(source, super::super::Language::Python);
    state.set_todo_settings(&policy());
    let version = state.doc.version();
    let ctx = context();
    let mut view = EditorView::new(Id::new("todo-color"));
    let output = frame(&ctx, &mut state, &mut view, false, 0.0, vec![]).0;
    let color = Color32::from_rgb(COLOR[0], COLOR[1], COLOR[2]);
    assert!(painted(&output, "REVIEW π😀", color));
    assert!(painted(&output, "continuation Ω", color));
    assert!(!painted(&output, "REVIEW string", color));
    assert!(!painted(&output, "review lower", color));
    assert!(!painted(&output, "x=2", color));
    assert_eq!(state.doc.text(), source);
    assert_eq!(state.doc.version(), version);
    assert!(!state.doc.can_undo());
    output.drop_without_applying_deltas();
}
#[test]
fn actual_empty_policy_neutralizes_legacy_todo_tags_and_cache_rebuilds_only_on_change() {
    let mut state =
        EditorState::from_text("# TODO old\n# REVIEW new\n", super::super::Language::Python);
    state.set_todo_settings(&policy());
    let ctx = context();
    let mut view = EditorView::new(Id::new("todo-cache"));
    let output = frame(&ctx, &mut state, &mut view, false, 0.0, vec![]).0;
    assert!(painted(
        &output,
        "REVIEW new",
        Color32::from_rgb(COLOR[0], COLOR[1], COLOR[2])
    ));
    output.drop_without_applying_deltas();
    let builds = state.todo_cache_builds();
    state.set_todo_settings(&policy());
    frame(&ctx, &mut state, &mut view, false, 0.02, vec![])
        .0
        .drop_without_applying_deltas();
    assert_eq!(state.todo_cache_builds(), builds);
    state.set_todo_settings(&TodoSettings {
        patterns: vec![],
        multiline: true,
        ..Default::default()
    });
    let output = frame(&ctx, &mut state, &mut view, false, 0.04, vec![]).0;
    let comment = rgb(ColorScheme::dark().style(HighlightTag::Comment).fg.unwrap());
    assert!(painted(&output, "TODO old", comment));
    assert!(painted(&output, "REVIEW new", comment));
    assert_eq!(state.todo_cache_builds(), builds + 1);
    output.drop_without_applying_deltas();
    state.doc.reset("# REVIEW replacement\n");
    state.set_todo_settings(&policy());
    let output = frame(&ctx, &mut state, &mut view, false, 0.06, vec![]).0;
    assert!(painted(
        &output,
        "REVIEW replacement",
        Color32::from_rgb(COLOR[0], COLOR[1], COLOR[2])
    ));
    assert_eq!(state.todo_cache_builds(), builds + 2);
    output.drop_without_applying_deltas();
}
#[test]
fn actual_fold_and_soft_wrap_keep_todo_color_without_hidden_line_paint() {
    let source = format!(
        "def outer():\n    # REVIEW {}\n    return 1\n",
        "πΩ ".repeat(30)
    );
    let mut state = EditorState::from_text(&source, super::super::Language::Python);
    state.set_todo_settings(&policy());
    let ctx = context();
    let mut view = EditorView::new(Id::new("todo-wrap-fold"));
    let output = frame(&ctx, &mut state, &mut view, true, 0.0, vec![]).0;
    let color = Color32::from_rgb(COLOR[0], COLOR[1], COLOR[2]);
    assert!(painted(&output, "REVIEW", color));
    assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text().contains("REVIEW") && text.galley.rows.len() > 1)));
    output.drop_without_applying_deltas();
    state.toggle_fold_at_line(0);
    assert!(state
        .hidden_line_ranges()
        .iter()
        .any(|&(start, end)| start <= 1 && end > 1));
    let output = frame(&ctx, &mut state, &mut view, true, 0.02, vec![]).0;
    assert!(!painted(&output, "REVIEW", color));
    output.drop_without_applying_deltas();
    state.expand_all_folds();
    let output = frame(&ctx, &mut state, &mut view, true, 0.04, vec![]).0;
    assert!(painted(&output, "REVIEW", color));
    assert_eq!(state.doc.text(), source);
    assert!(!state.doc.can_undo());
    output.drop_without_applying_deltas();
}
