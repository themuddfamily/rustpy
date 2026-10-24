//! Navigation regressions driven through the actual editor viewport.
use super::super::{Caret, Language};
use super::*;

struct Frame {
    caret: Option<(Rect, Rect)>,
}
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx
}
fn frame(
    ctx: &egui::Context,
    state: &mut EditorState,
    view: &mut EditorView,
    time: f64,
    height: f32,
    events: Vec<egui::Event>,
) -> Frame {
    let settings = EditorSettings::default();
    let scheme = ColorScheme::dark();
    let theme = UiTheme::dark();
    let opts = EditOptions::from_settings(&settings);
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
    let output = ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, height))),
            ..Default::default()
        },
        |ui| {
            show(ui, state, view, &cfg);
        },
    );
    let caret = output.shapes.iter().find_map(|s| match &s.shape {
        egui::Shape::Rect(r) if r.fill == rgb(scheme.editor.caret) => Some((r.rect, s.clip_rect)),
        _ => None,
    });
    output.drop_without_applying_deltas();
    Frame { caret }
}
fn key(key: egui::Key, shift: bool) -> Vec<egui::Event> {
    vec![egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers {
            shift,
            ..Default::default()
        },
    }]
}
fn long_state() -> EditorState {
    EditorState::from_text(&"αβ界 suffix long enough\n".repeat(100), Language::Python)
}
fn at(state: &EditorState, line: usize, column: usize) -> TextSize {
    let text = state.doc.line_text(line);
    let byte = text
        .char_indices()
        .nth(column)
        .map_or(text.len(), |(i, _)| i);
    state.doc.line_range(line).start() + TextSize::new(byte as u32)
}
fn centered(frame: &Frame, view: &EditorView) {
    let (caret, clip) = frame.caret.expect("centered caret must be painted");
    assert!(
        (caret.center().y - clip.center().y).abs() <= caret.height(),
        "caret={caret:?}, viewport={clip:?}, top={}, page={}",
        view.top_line,
        view.page_lines
    );
}

#[test]
fn actual_small_viewport_pages_keep_unicode_goal_and_read_only_selection() {
    let ctx = context();
    let mut state = long_state();
    let start = at(&state, 20, 3);
    state.set_carets(vec![Caret::at(start)]);
    state.doc.set_read_only(true);
    let mut view = EditorView::new(egui::Id::new("navigation-page"));
    frame(&ctx, &mut state, &mut view, 0.0, 180.0, vec![]);
    let page = view.page_lines;
    assert!(page > 0 && page < 20, "actual page={page}");
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.1,
        180.0,
        key(egui::Key::PageDown, true),
    );
    assert_eq!(state.primary().sel.anchor, start);
    assert_eq!(
        state.primary().head(),
        at(&state, 20 + page, 3),
        "page={page}, caret={:?}",
        state.primary()
    );
    assert_eq!(state.primary().goal_column, Some(3));
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.2,
        180.0,
        key(egui::Key::PageUp, true),
    );
    assert_eq!(state.primary().head(), start);
    assert!(!state.doc.can_undo());
    assert_eq!(state.doc.text(), "αβ界 suffix long enough\n".repeat(100));
}

#[test]
fn center_reveals_nested_folds_without_changing_selection_or_history() {
    let ctx = context();
    let source = format!(
        "{}class Outer:\n    def inner(self):\n{}",
        "# preceding\n".repeat(40),
        "        value = 41\n".repeat(40)
    );
    let mut state = EditorState::from_text(&source, Language::Python);
    state.toggle_fold_at_line(41);
    state.toggle_fold_at_line(40);
    assert!(!state.hidden_line_ranges().is_empty());
    let caret = Caret::new(at(&state, 55, 9), at(&state, 56, 12));
    state.set_carets(vec![caret]);
    let mut view = EditorView::new(egui::Id::new("navigation-fold-center"));
    view.request_center(&mut state);
    assert!(
        state.hidden_line_ranges().is_empty(),
        "ranges={:?}",
        state.hidden_line_ranges()
    );
    assert_eq!(state.primary(), caret);
    frame(&ctx, &mut state, &mut view, 0.0, 180.0, vec![]);
    let painted = frame(&ctx, &mut state, &mut view, 0.05, 180.0, vec![]);
    centered(&painted, &view);
    assert_eq!(state.primary(), caret);
    assert_eq!(state.doc.text(), source);
    assert!(!state.doc.can_undo());
    assert_eq!(view.center_request.take_offset(0.0, 1.0, 1.0, 1.0), None);
}

#[test]
fn consumed_center_does_not_override_actual_manual_wheel_scroll() {
    let ctx = context();
    let mut state = long_state();
    state.set_carets(vec![Caret::at(at(&state, 60, 3))]);
    let mut view = EditorView::new(egui::Id::new("navigation-wheel"));
    view.request_center(&mut state);
    frame(&ctx, &mut state, &mut view, 0.0, 180.0, vec![]);
    let first = frame(&ctx, &mut state, &mut view, 0.05, 180.0, vec![]);
    centered(&first, &view);
    let before = view.top_line;
    let (_, clip) = first.caret.unwrap();
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.1,
        180.0,
        vec![
            egui::Event::PointerMoved(clip.center()),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                phase: egui::TouchPhase::Move,
                delta: Vec2::new(0.0, -100.0),
                modifiers: Default::default(),
            },
        ],
    );
    frame(&ctx, &mut state, &mut view, 0.5, 180.0, vec![]);
    let after = view.top_line;
    assert!(
        after > before,
        "manual wheel before={before}, after={after}, page={}",
        view.page_lines
    );
    frame(&ctx, &mut state, &mut view, 1.0, 180.0, vec![]);
    assert_eq!(view.top_line, after, "one-shot center must not repeat");
    assert_eq!(state.primary().head(), at(&state, 60, 3));
    assert!(!state.doc.can_undo());
}

#[test]
fn center_request_is_owned_by_one_split_view() {
    let ctx = context();
    let mut state = long_state();
    let mut left = EditorView::new(egui::Id::new("navigation-split-left"));
    let mut right = EditorView::new(egui::Id::new("navigation-split-right"));
    frame(&ctx, &mut state, &mut right, 0.0, 180.0, vec![]);
    let right_top = right.top_line;
    state.set_carets(vec![Caret::at(at(&state, 60, 3))]);
    left.request_center(&mut state);
    frame(&ctx, &mut state, &mut left, 0.1, 180.0, vec![]);
    centered(
        &frame(&ctx, &mut state, &mut left, 0.15, 180.0, vec![]),
        &left,
    );
    frame(&ctx, &mut state, &mut right, 0.2, 180.0, vec![]);
    assert_eq!(
        right.top_line, right_top,
        "left top={}, right top={}",
        left.top_line, right.top_line
    );
    assert_eq!(right.center_request.take_offset(0.0, 1.0, 1.0, 1.0), None);
    assert_eq!(state.primary().head(), at(&state, 60, 3));
}

#[test]
fn default_ctrl_m_dispatch_centers_without_changing_selection_or_history() {
    let ctx = context();
    let mut state = long_state();
    let caret = Caret::new(at(&state, 59, 2), at(&state, 60, 3));
    state.set_carets(vec![caret]);
    // Start with the caret offscreen so ordinary initial caret reveal cannot
    // substitute for the shortcut's explicit center request.
    state.scroll_to_caret = false;
    let mut view = EditorView::new(egui::Id::new("navigation-default-ctrl-m"));
    frame(&ctx, &mut state, &mut view, 0.0, 180.0, vec![]);
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.1,
        180.0,
        vec![egui::Event::Key {
            key: egui::Key::M,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                ctrl: true,
                ..Default::default()
            },
        }],
    );
    frame(&ctx, &mut state, &mut view, 0.15, 180.0, vec![]);
    let painted = frame(&ctx, &mut state, &mut view, 0.2, 180.0, vec![]);
    centered(&painted, &view);
    assert_eq!(state.primary(), caret);
    assert_eq!(state.doc.text(), "αβ界 suffix long enough\n".repeat(100));
    assert!(!state.doc.can_undo());
    assert_eq!(view.center_request.take_offset(0.0, 1.0, 1.0, 1.0), None);
}

#[test]
fn actual_middle_click_and_ctrl_click_dispatch_same_source_without_editing() {
    for (button, modifiers) in [
        (egui::PointerButton::Middle, egui::Modifiers::NONE),
        (egui::PointerButton::Primary, egui::Modifiers::CTRL),
    ] {
        let ctx = context();
        let source = "target = 1\nprint(target)\n";
        let mut state = EditorState::from_text(source, Language::Python);
        let offset = TextSize::new((source.rfind("target").unwrap() + 2) as u32);
        state.set_carets(vec![Caret::at(offset)]);
        let mut view = EditorView::new(egui::Id::new("declaration-pointer"));
        frame(&ctx, &mut state, &mut view, 0.0, 300.0, vec![]);
        let point = view
            .caret_screen_rect
            .expect("actual measured caret")
            .center()
            + egui::vec2(0.5, 0.0);
        let before = state.selections();
        for (time, pressed) in [(0.1, true), (0.2, false)] {
            frame(
                &ctx,
                &mut state,
                &mut view,
                time,
                300.0,
                vec![
                    egui::Event::ModifiersChanged(modifiers),
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button,
                        pressed,
                        modifiers,
                    },
                ],
            );
        }
        let selected = view.ctrl_click.expect("actual pointer route");
        let start = source.rfind("target").unwrap();
        assert!((start..=start + 6).contains(&usize::from(selected)));
        assert_eq!(state.selections(), before);
        assert_eq!(state.doc.text(), source);
        assert!(!state.doc.can_undo());
    }
}
