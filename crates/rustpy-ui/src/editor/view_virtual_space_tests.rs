use super::super::Language;
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
    settings: &EditorSettings,
    modifiers: egui::Modifiers,
    mut events: Vec<egui::Event>,
) -> Vec<egui::epaint::ClippedShape> {
    let scheme = ColorScheme::dark();
    let theme = UiTheme::dark();
    let opts = EditOptions::from_settings(settings);
    let mut keymap = Keymap::default_keymap();
    keymap.set_shortcut(
        Action::ToggleStickySelection,
        crate::actions::KeyChord::parse("F6").unwrap(),
    );
    let cfg = ViewConfig {
        scheme: &scheme,
        theme: &theme,
        settings,
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

fn end_of_text(shapes: &[egui::epaint::ClippedShape], line: &str) -> (Pos2, f32) {
    for shape in shapes {
        if let egui::Shape::Text(text) = &shape.shape {
            if text.galley.job.text == line {
                let row = text.galley.rows.last().unwrap();
                let cell = row.glyphs.last().unwrap().advance_width;
                return (
                    text.pos + row.pos.to_vec2() + Vec2::new(row.size.x, row.size.y * 0.5),
                    cell,
                );
            }
        }
    }
    panic!("actual source line missing");
}
fn click(pos: Pos2) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(pos),
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        },
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        },
    ]
}
#[test]
fn real_pointer_virtual_caret_paint_and_typing_materialize_only_on_edit() {
    let ctx = context();
    let mut state = EditorState::from_text("αβ", Language::Python);
    let mut view = EditorView::new(egui::Id::new("virtual-pointer"));
    let settings = EditorSettings {
        allow_caret_after_end_of_line: true,
        ..Default::default()
    };
    let shapes = frame(
        &ctx,
        &mut state,
        &mut view,
        0.0,
        &settings,
        Default::default(),
        vec![],
    );
    let (end, cell) = end_of_text(&shapes, "αβ");
    let target = end + Vec2::new(cell * 4.0, 0.0);
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.1,
        &settings,
        Default::default(),
        click(target),
    );
    assert_eq!(state.virtual_caret(0).head, Position::new(0, 6));
    assert_eq!(state.doc.text(), "αβ");
    assert!(!state.doc.can_undo());
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.2,
        &settings,
        Default::default(),
        vec![],
    );
    assert!((view.caret_screen_rect.unwrap().min.x - target.x).abs() < cell * 0.6);
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.3,
        &settings,
        Default::default(),
        vec![egui::Event::Text("🙂".into())],
    );
    assert_eq!(state.doc.text(), "αβ    🙂");
    assert!(state.undo());
    assert_eq!(state.doc.text(), "αβ");
    assert_eq!(state.virtual_caret(0).head, Position::new(0, 6));
}
#[test]
fn disabled_virtual_pointer_clips_without_padding_and_flags_roundtrip() {
    let ctx = context();
    let mut state = EditorState::from_text("αβ", Language::Python);
    let mut view = EditorView::new(egui::Id::new("virtual-disabled"));
    let settings = EditorSettings::default();
    let shapes = frame(
        &ctx,
        &mut state,
        &mut view,
        0.0,
        &settings,
        Default::default(),
        vec![],
    );
    let (end, cell) = end_of_text(&shapes, "αβ");
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.1,
        &settings,
        Default::default(),
        click(end + Vec2::new(cell * 5.0, 0.0)),
    );
    assert_eq!(state.virtual_caret(0).head, Position::new(0, 2));
    assert_eq!(state.doc.text(), "αβ");
    let custom = EditorSettings {
        allow_caret_after_end_of_line: true,
        allow_caret_after_end_of_line_in_block_selection: false,
        ..Default::default()
    };
    let encoded = serde_json::to_string(&custom).unwrap();
    let decoded: EditorSettings = serde_json::from_str(&encoded).unwrap();
    assert!(decoded.allow_caret_after_end_of_line);
    assert!(!decoded.allow_caret_after_end_of_line_in_block_selection);
    let legacy: EditorSettings = serde_json::from_str("{}").unwrap();
    assert!(!legacy.allow_caret_after_end_of_line);
    assert!(legacy.allow_caret_after_end_of_line_in_block_selection);
}
#[test]
fn actual_sticky_toggle_navigation_smart_end_and_escape_are_distinct_from_sticky_lines() {
    let ctx = context();
    let mut state = EditorState::from_text("αβ   ", Language::Python);
    let mut view = EditorView::new(egui::Id::new("sticky-navigation"));
    let settings = EditorSettings {
        allow_caret_after_end_of_line: true,
        show_sticky_lines: false,
        ..Default::default()
    };
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.0,
        &settings,
        Default::default(),
        vec![],
    );
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.1,
        &settings,
        Default::default(),
        vec![event(egui::Key::F6, Default::default())],
    );
    assert!(state.sticky_selection);
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.2,
        &settings,
        Default::default(),
        vec![event(egui::Key::ArrowRight, Default::default())],
    );
    assert_eq!(state.virtual_caret(0).anchor, Position::new(0, 0));
    assert_eq!(state.virtual_caret(0).head, Position::new(0, 1));
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.3,
        &settings,
        Default::default(),
        vec![event(egui::Key::End, Default::default())],
    );
    assert_eq!(state.virtual_caret(0).anchor, Position::new(0, 0));
    assert_eq!(state.virtual_caret(0).head, Position::new(0, 2));
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.4,
        &settings,
        Default::default(),
        vec![event(egui::Key::Escape, Default::default())],
    );
    assert!(!state.sticky_selection);
    assert!(!state.has_selection());
    assert_eq!(state.doc.text(), "αβ   ");
    assert!(!state.doc.can_undo());
}
#[test]
fn real_shift_selection_paints_virtual_tail_and_readonly_never_materializes() {
    let ctx = context();
    let mut state = EditorState::from_text("αβ", Language::Python);
    let mut view = EditorView::new(egui::Id::new("virtual-selection"));
    let settings = EditorSettings {
        allow_caret_after_end_of_line: true,
        ..Default::default()
    };
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.0,
        &settings,
        Default::default(),
        vec![],
    );
    state.move_primary_to(Position::new(0, 2));
    let shift = egui::Modifiers {
        shift: true,
        ..Default::default()
    };
    for i in 0..3 {
        frame(
            &ctx,
            &mut state,
            &mut view,
            0.1 + i as f64 * 0.1,
            &settings,
            shift,
            vec![event(egui::Key::ArrowRight, shift)],
        );
    }
    assert!(state.has_selection());
    assert_eq!(state.virtual_caret(0).anchor, Position::new(0, 2));
    assert_eq!(state.virtual_caret(0).head, Position::new(0, 5));
    let shapes = frame(
        &ctx,
        &mut state,
        &mut view,
        0.5,
        &settings,
        Default::default(),
        vec![],
    );
    let selection = rgb(ColorScheme::dark().editor.selection);
    assert!(shapes.iter().any(
        |s| matches!(&s.shape,egui::Shape::Rect(r) if r.fill==selection && r.rect.width()>20.0)
    ));
    state.doc.set_read_only(true);
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.6,
        &settings,
        Default::default(),
        vec![egui::Event::Text("x".into())],
    );
    assert_eq!(state.doc.text(), "αβ");
    assert!(!state.doc.can_undo());
}

#[test]
fn wrapped_last_row_pointer_keeps_virtual_column_and_paints_caret_there() {
    let ctx = context();
    let text = "a".repeat(120);
    let mut state = EditorState::from_text(&text, Language::Python);
    let mut view = EditorView::new(egui::Id::new("virtual-wrapped"));
    let settings = EditorSettings {
        allow_caret_after_end_of_line: true,
        soft_wrap: true,
        ..Default::default()
    };
    let shapes = frame(
        &ctx,
        &mut state,
        &mut view,
        0.0,
        &settings,
        Default::default(),
        vec![],
    );
    let (end, cell) = end_of_text(&shapes, &text);
    let target = end + Vec2::new(cell * 3.0, 0.0);
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.1,
        &settings,
        Default::default(),
        click(target),
    );
    assert_eq!(state.virtual_caret(0).head, Position::new(0, 123));
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.2,
        &settings,
        Default::default(),
        vec![],
    );
    assert!((view.caret_screen_rect.unwrap().min.x - target.x).abs() < cell * 0.6);
    assert_eq!(state.doc.text(), text);
    assert!(!state.doc.can_undo());
}

#[test]
fn block_only_virtual_anchor_paints_and_disabled_policy_does_not_pad_paste() {
    let ctx = context();
    let mut state = EditorState::from_text("αβ\nx", Language::Python);
    state.column_mode = true;
    let mut view = EditorView::new(egui::Id::new("virtual-block-policy"));
    let settings = EditorSettings::default();
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.0,
        &settings,
        Default::default(),
        vec![],
    );
    state.set_block_selection(Position::new(0, 4), Position::new(1, 7));
    let shapes = frame(
        &ctx,
        &mut state,
        &mut view,
        0.1,
        &settings,
        Default::default(),
        vec![],
    );
    assert_eq!(state.virtual_caret(0).anchor.column, 4);
    assert_eq!(state.virtual_caret(0).head.column, 7);
    let color = rgb(ColorScheme::dark().editor.selection);
    assert!(shapes.iter().filter(|shape| matches!(&shape.shape,egui::Shape::Rect(rect) if rect.fill==color && rect.rect.width()>20.0)).count()>=2);
    let disabled = EditorSettings {
        allow_caret_after_end_of_line_in_block_selection: false,
        ..Default::default()
    };
    frame(
        &ctx,
        &mut state,
        &mut view,
        0.2,
        &disabled,
        Default::default(),
        vec![egui::Event::Paste("Z".into())],
    );
    assert_eq!(state.doc.text(), "αβZ\nxZ");
    assert!(state.undo());
    assert_eq!(state.doc.text(), "αβ\nx");
}
