//! Full EditorView ownership and geometry regressions. Registered inside view.
use super::super::code_vision_view::Display;
use super::*;
use egui::Id;
use rustpy_search::code_vision::UsageLens;
use rustpy_semantic::features::{InlayHint, InlayKind};
use std::sync::Arc;

fn display(state: &EditorState, lines: &[usize]) -> Display {
    Display {
        document: state.doc.identity(),
        version: state.doc.version(),
        path: state.path.clone().unwrap(),
        source: Arc::from(state.doc.text()),
        returns: vec![],
        authors: vec![],
        repo_head: None,
        usages: lines
            .iter()
            .map(|&line| {
                let text = state.doc.line_text(line);
                let start = state.doc.line_range(line).start()
                    + TextSize::new(text.find('π').unwrap() as u32);
                UsageLens {
                    range: TextRange::new(start, start + TextSize::new(2)),
                    declaration: state.doc.line_range(line),
                    name: "π".into(),
                    line,
                    usages: 7,
                }
            })
            .collect(),
    }
}
fn state(source: &str) -> EditorState {
    let mut state = EditorState::from_text(source, super::super::Language::Python);
    state.path = Some("/owned/main.py".into());
    state
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
    vision: Option<&Display>,
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
        code_vision: vision,
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
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(300.0, 180.0))),
            ..Default::default()
        },
        |ui| result = show(ui, state, view, &cfg),
    );
    (output, result)
}
fn text_rect(output: &egui::FullOutput, needle: &str) -> Rect {
    output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text() == needle => {
                Some(Rect::from_min_size(text.pos, text.galley.size()))
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("actual View did not paint {needle:?}"))
}
fn pointer(pos: Pos2, button: egui::PointerButton, pressed: bool) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(pos),
        egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}
#[test]
fn actual_unicode_header_click_and_drag_never_edit_or_move_source() {
    let mut state = state("def π():\n    return 42\nπ()\n");
    let vision = display(&state, &[0]);
    let ctx = context();
    let mut view = EditorView::new(Id::new("vision-pointer"));
    let (output, _) = frame(
        &ctx,
        &mut state,
        &mut view,
        Some(&vision),
        false,
        0.0,
        vec![],
    );
    let header = text_rect(&output, "7 usages");
    let source_glyph = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text().contains("def π()") => {
                Some(Rect::from_min_size(text.pos, text.galley.size()))
            }
            _ => None,
        })
        .expect("actual source declaration glyph must be painted");
    assert!(source_glyph.top() >= header.bottom());
    let caret = view.caret_screen_rect.unwrap();
    assert!(
        caret.top() >= header.bottom(),
        "source caret must be below reserved header"
    );
    output.drop_without_applying_deltas();
    let source = state.doc.text();
    let carets = state.carets.clone();
    let version = state.doc.version();
    let (output, _) = frame(
        &ctx,
        &mut state,
        &mut view,
        Some(&vision),
        false,
        0.02,
        pointer(header.center(), egui::PointerButton::Primary, true),
    );
    output.drop_without_applying_deltas();
    let (output, events) = frame(
        &ctx,
        &mut state,
        &mut view,
        Some(&vision),
        false,
        0.04,
        pointer(header.center(), egui::PointerButton::Primary, false),
    );
    // Retire atlas payload before any assertion can unwind this headless output.
    let mut output = output;
    output.textures_delta.clear();
    let targets: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            ViewEvent::CodeVisionFindUsages(target) => Some(target),
            _ => None,
        })
        .collect();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].range, vision.usages[0].range);
    assert_eq!(targets[0].document, state.doc.identity());
    assert_eq!(targets[0].name, "π");
    output.drop_without_applying_deltas();
    for (time, events) in [
        (
            0.06,
            pointer(header.center(), egui::PointerButton::Secondary, true),
        ),
        (
            0.08,
            pointer(header.center(), egui::PointerButton::Secondary, false),
        ),
        (
            0.10,
            pointer(header.center(), egui::PointerButton::Primary, true),
        ),
        (
            0.12,
            vec![egui::Event::PointerMoved(
                header.center() + Vec2::new(50.0, 70.0),
            )],
        ),
        (
            0.14,
            pointer(
                header.center() + Vec2::new(50.0, 70.0),
                egui::PointerButton::Primary,
                false,
            ),
        ),
    ] {
        frame(
            &ctx,
            &mut state,
            &mut view,
            Some(&vision),
            false,
            time,
            events,
        )
        .0
        .drop_without_applying_deltas();
        assert_eq!(state.carets, carets, "header pointer frame at {time}");
        assert_eq!(state.doc.text(), source, "header pointer frame at {time}");
        assert_eq!(
            state.doc.version(),
            version,
            "header pointer frame at {time}"
        );
    }
    assert_eq!(state.carets, carets);
    assert_eq!(state.doc.text(), source);
    assert_eq!(state.doc.version(), version);
    assert!(!state.doc.can_undo());
}
#[test]
fn actual_fold_hides_nested_declaration_header_and_unfold_restores_it() {
    let mut state = state("def outer():\n    def π():\n        return 42\n    return π()\n");
    let vision = display(&state, &[1]);
    let ctx = context();
    let mut view = EditorView::new(Id::new("vision-fold"));
    let output = frame(
        &ctx,
        &mut state,
        &mut view,
        Some(&vision),
        false,
        0.0,
        vec![],
    )
    .0;
    text_rect(&output, "7 usages");
    output.drop_without_applying_deltas();
    state.toggle_fold_at_line(0);
    assert!(state
        .hidden_line_ranges()
        .iter()
        .any(|&(start, end)| start <= 1 && end > 1));
    let output = frame(
        &ctx,
        &mut state,
        &mut view,
        Some(&vision),
        false,
        0.02,
        vec![],
    )
    .0;
    assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape,
        egui::Shape::Text(text) if text.galley.text() == "7 usages")));
    output.drop_without_applying_deltas();
    state.expand_all_folds();
    let output = frame(
        &ctx,
        &mut state,
        &mut view,
        Some(&vision),
        false,
        0.04,
        vec![],
    )
    .0;
    text_rect(&output, "7 usages");
    output.drop_without_applying_deltas();
    assert!(!state.doc.can_undo());
}
#[test]
fn actual_return_hint_toggle_recomputes_same_version_wrap_cache() {
    let mut state = state("def π():\n    return 42\n");
    let mut vision = display(&state, &[]);
    vision.returns.push(InlayHint {
        offset: state.doc.line_range(0).end() - TextSize::new(1),
        label: format!(" -> {}", "inferred_return_type".repeat(8)),
        kind: InlayKind::Type,
    });
    let ctx = context();
    let mut view = EditorView::new(Id::new("vision-wrap"));
    frame(&ctx, &mut state, &mut view, None, true, 0.0, vec![])
        .0
        .drop_without_applying_deltas();
    let before = view.wrap_cache.clone().unwrap();
    let source = state.doc.text();
    frame(
        &ctx,
        &mut state,
        &mut view,
        Some(&vision),
        true,
        0.02,
        vec![],
    )
    .0
    .drop_without_applying_deltas();
    let with = view.wrap_cache.clone().unwrap();
    assert_eq!(with.0, before.0);
    assert_eq!(with.1, before.1);
    assert!(with.2[0] > before.2[0]);
    frame(&ctx, &mut state, &mut view, None, true, 0.04, vec![])
        .0
        .drop_without_applying_deltas();
    assert_eq!(view.wrap_cache.as_ref().unwrap(), &before);
    assert_eq!(state.doc.text(), source);
    assert!(!state.doc.can_undo());
}
#[test]
fn actual_ctrl_scroll_resolves_header_to_real_source_row() {
    let source = (0..40)
        .map(|n| format!("def π{n}(): return {n}\n"))
        .collect::<String>();
    let mut state = state(&source);
    let vision = display(&state, &(0..40).collect::<Vec<_>>());
    let ctx = context();
    let mut view = EditorView::new(Id::new("vision-scroll"));
    frame(
        &ctx,
        &mut state,
        &mut view,
        Some(&vision),
        false,
        0.0,
        vec![],
    )
    .0
    .drop_without_applying_deltas();
    for (n, key) in [
        egui::Key::ArrowDown,
        egui::Key::ArrowDown,
        egui::Key::ArrowUp,
    ]
    .into_iter()
    .enumerate()
    {
        let (output, events) = frame(
            &ctx,
            &mut state,
            &mut view,
            Some(&vision),
            false,
            0.02 + n as f64 * 0.02,
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::CTRL,
            }],
        );
        output.drop_without_applying_deltas();
        assert!(events.iter().any(|event| matches!(event, ViewEvent::Performed(action)
            if *action == if key == egui::Key::ArrowDown { Action::ScrollDown } else { Action::ScrollUp })));
        let position = state.primary_position();
        assert_eq!(
            position.column, 0,
            "a header must resolve to actual source column, not MAX subrow"
        );
        assert!(position.line < 40);
        if n == 1 {
            assert_eq!(
                position.line, 1,
                "second scroll reaches the next declaration header and resolves its source row"
            );
        }
        assert_eq!(state.doc.text(), source);
        assert!(!state.doc.can_undo());
    }
    assert!(view.pending_scroll_line.is_none());
}
