//! Remaining multicaret default-keymap and pointer routes through the real view.
use super::super::{Caret, Language};
use super::*;
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
    modifiers: egui::Modifiers,
    mut events: Vec<egui::Event>,
) -> Vec<egui::epaint::ClippedShape> {
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
    events.insert(0, egui::Event::ModifiersChanged(modifiers));
    let output = ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 300.0))),
            ..Default::default()
        },
        |ui| {
            show(ui, state, view, &cfg);
        },
    );
    let shapes = output.shapes.clone();
    output.drop_without_applying_deltas();
    shapes
}

fn event(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}
fn text_position(shapes: &[egui::epaint::ClippedShape], line: &str, column: usize) -> Pos2 {
    for shape in shapes {
        if let egui::Shape::Text(text) = &shape.shape {
            if text.galley.job.text == line {
                let row = &text.galley.rows[0];
                let glyph = &row.glyphs[column];
                return text.pos + row.pos.to_vec2() + Vec2::new(glyph.pos.x, row.size.y / 2.0);
            }
        }
    }
    panic!("rendered source line absent: {line:?}");
}

fn unchanged(state: &EditorState, text: &str) {
    assert_eq!(state.doc.text(), text);
    assert!(
        !state.doc.can_undo(),
        "navigation must not create edit history"
    );
}
#[test]
fn actual_alt_click_adds_unicode_caret_and_escape_keeps_primary() {
    let text = "αβx\néβx";
    let ctx = context();
    let mut state = EditorState::from_text(text, Language::Python);
    let mut view = EditorView::new(egui::Id::new("multicaret-alt-click"));
    let shapes = frame(&ctx, &mut state, &mut view, 0.0, Default::default(), vec![]);
    let target = text_position(&shapes, "éβx", 1);
    let alt = egui::Modifiers {
        alt: true,
        ..Default::default()
    };
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.1,
        alt,
        vec![
            egui::Event::PointerMoved(target),
            egui::Event::PointerButton {
                pos: target,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: alt,
            },
        ],
    );
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.15,
        alt,
        vec![egui::Event::PointerButton {
            pos: target,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: alt,
        }],
    );
    assert_eq!(
        state.carets.len(),
        2,
        "target={target:?}, carets={:?}",
        state.carets
    );
    assert_eq!(state.primary().head(), TextSize::new(0));
    let expected = state.doc.line_range(1).start() + TextSize::new("é".len() as u32);
    assert!(
        state.carets.iter().any(|c| c.head() == expected),
        "target={target:?}, carets={:?}",
        state.carets
    );
    unchanged(&state, text);
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.2,
        Default::default(),
        vec![event(egui::Key::Escape, Default::default())],
    );
    assert_eq!(state.carets, vec![Caret::at(TextSize::new(0))]);
    unchanged(&state, text);
}
#[test]
fn actual_occurrence_shortcuts_add_remove_select_all_and_escape() {
    let text = "β = 1\nβeta = 2\nβ = 3\nβ = 4";
    let ctx = context();
    let mut state = EditorState::from_text(text, Language::Python);
    let mut view = EditorView::new(egui::Id::new("multicaret-occurrences"));
    frame(&ctx, &mut state, &mut view, 0.0, Default::default(), vec![]);
    let alt = egui::Modifiers {
        alt: true,
        ..Default::default()
    };
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.1,
        alt,
        vec![event(egui::Key::J, alt)],
    );
    assert_eq!(state.carets.len(), 1);
    assert_eq!(state.doc.slice(state.primary().range()), "β");
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.2,
        alt,
        vec![event(egui::Key::J, alt)],
    );
    assert_eq!(state.carets.len(), 2);
    assert!(state
        .carets
        .iter()
        .any(|c| state.doc.line_of(c.head()) == 2));
    let remove = egui::Modifiers { shift: true, ..alt };
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.3,
        remove,
        vec![event(egui::Key::J, remove)],
    );
    assert_eq!(state.carets.len(), 1);
    assert_eq!(state.doc.line_of(state.primary().head()), 0);
    // With no selection Select All Occurrences uses whole-word matching;
    // an explicit selected fragment intentionally uses substring matching.
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.33,
        Default::default(),
        vec![event(egui::Key::Escape, Default::default())],
    );
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.36,
        Default::default(),
        vec![event(egui::Key::Home, Default::default())],
    );
    let all = egui::Modifiers {
        ctrl: true,
        command: true,
        shift: true,
        ..alt
    };
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.4,
        all,
        vec![event(egui::Key::J, all)],
    );
    assert_eq!(state.carets.len(), 3, "carets={:?}", state.carets);
    assert!(state
        .carets
        .iter()
        .all(|c| state.doc.slice(c.range()) == "β"));
    assert!(state
        .carets
        .iter()
        .all(|c| state.doc.line_of(c.head()) != 1));
    let primary_head = state.primary().head();
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.5,
        Default::default(),
        vec![event(egui::Key::Escape, Default::default())],
    );
    assert_eq!(state.carets.len(), 1);
    assert_eq!(state.primary().head(), primary_head);
    assert!(!state.has_selection());
    unchanged(&state, text);
}
fn control(pressed: bool, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key: egui::Key::ControlLeft,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers,
    }
}
#[test]
fn actual_double_control_hold_clones_below_and_above_then_release_stops_gesture() {
    let text = "αβx\nαβx\nαβx";
    let ctx = context();
    let mut state = EditorState::from_text(text, Language::Python);
    state.doc.set_read_only(true);
    state.set_carets(vec![Caret::at(
        state.doc.line_range(1).start() + TextSize::new("α".len() as u32),
    )]);
    let primary = state.primary();
    let mut view = EditorView::new(egui::Id::new("multicaret-control-clone"));
    frame(&ctx, &mut state, &mut view, 0.0, Default::default(), vec![]);
    let ctrl = egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    };
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.1,
        ctrl,
        vec![control(true, ctrl)],
    );
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.15,
        Default::default(),
        vec![control(false, Default::default())],
    );
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.2,
        ctrl,
        vec![control(true, ctrl)],
    );
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.25,
        ctrl,
        vec![event(egui::Key::ArrowDown, ctrl)],
    );
    assert_eq!(state.carets.len(), 2, "carets={:?}", state.carets);
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.3,
        ctrl,
        vec![event(egui::Key::ArrowUp, ctrl)],
    );
    assert_eq!(state.carets.len(), 3, "carets={:?}", state.carets);
    assert_eq!(state.primary(), primary);
    let mut positions: Vec<_> = state
        .carets
        .iter()
        .map(|c| state.doc.offset_to_position(c.head()))
        .collect();
    positions.sort_by_key(|p| p.line);
    assert_eq!(
        positions,
        vec![
            Position::new(0, 1),
            Position::new(1, 1),
            Position::new(2, 1)
        ]
    );
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.35,
        Default::default(),
        vec![control(false, Default::default())],
    );
    assert!(!view.clones_caret(egui::Key::ArrowDown, ctrl));
    unchanged(&state, text);
}
