//! Column paste through real editor key and pointer input.
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
#[test]
fn default_column_shortcut_and_shift_rectangle_paste_undo_redo() {
    let ctx = context();
    let mut state = EditorState::from_text("éβcd\nx", Language::Python);
    state.set_carets(vec![Caret::at(TextSize::new("é".len() as u32))]);
    let original_carets = state.carets.clone();
    let mut view = EditorView::new(egui::Id::new("column-keys"));
    frame(&ctx, &mut state, &mut view, 0.0, Default::default(), vec![]);
    let toggle = egui::Modifiers {
        alt: true,
        shift: true,
        ..Default::default()
    };
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.1,
        toggle,
        vec![event(egui::Key::Insert, toggle)],
    );
    assert!(
        state.column_mode,
        "default Alt+Shift+Insert must reach editor dispatch"
    );
    let shift = egui::Modifiers {
        shift: true,
        ..Default::default()
    };
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.2,
        shift,
        vec![event(egui::Key::ArrowRight, shift)],
    );
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.3,
        shift,
        vec![event(egui::Key::ArrowDown, shift)],
    );
    let rectangle = state.carets.clone();
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.4,
        Default::default(),
        vec![egui::Event::Paste("A\nB\nC".into())],
    );
    assert_eq!(state.doc.text(), "éAcd\nxB\n C");
    let pasted_carets = state.carets.clone();
    let command = egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    };
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.5,
        command,
        vec![event(egui::Key::Z, command)],
    );
    assert_eq!(state.doc.text(), "éβcd\nx");
    assert_eq!(state.carets, rectangle);
    assert!(
        !state.doc.can_undo(),
        "paste must be one edit; rectangle navigation creates no undo"
    );
    let redo = egui::Modifiers {
        shift: true,
        ..command
    };
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.6,
        redo,
        vec![event(egui::Key::Z, redo)],
    );
    assert_eq!(state.doc.text(), "éAcd\nxB\n C");
    assert_eq!(state.carets, pasted_carets);
    assert_ne!(rectangle, original_carets);
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
#[test]
fn actual_alt_drag_retains_rectangle_for_unequal_paste_on_short_line() {
    let ctx = context();
    let mut state = EditorState::from_text("abcd\nx\néβcd", Language::Python);
    let mut view = EditorView::new(egui::Id::new("column-pointer"));
    let shapes = frame(&ctx, &mut state, &mut view, 0.0, Default::default(), vec![]);
    let start = text_position(&shapes, "abcd", 1);
    let end = text_position(&shapes, "éβcd", 3);
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
            egui::Event::PointerMoved(start),
            egui::Event::PointerButton {
                pos: start,
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
        0.2,
        alt,
        vec![egui::Event::PointerMoved(end)],
    );
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.3,
        alt,
        vec![egui::Event::PointerButton {
            pos: end,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: alt,
        }],
    );
    assert!(
        !state.column_mode,
        "Alt-drag must work independently of mode toggle"
    );
    assert!(
        state.block.is_some(),
        "pointer rectangle metadata must survive clipped short row; carets={:?}",
        state.carets
    );
    let selected: Vec<_> = state
        .carets
        .iter()
        .map(|c| state.doc.slice(c.range()))
        .collect();
    assert_eq!(selected, ["bc", "", "βc"], "start={start:?}, end={end:?}");
    let before = state.carets.clone();
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.4,
        Default::default(),
        vec![egui::Event::Paste("Q\n\nR\nS".into())],
    );
    assert_eq!(state.doc.text(), "aQd\nx\néRd\n S");
    assert!(state.undo());
    assert_eq!(state.doc.text(), "abcd\nx\néβcd");
    assert_eq!(state.carets, before);
    assert!(!state.doc.can_undo());
}
