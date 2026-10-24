//! Actual viewport wheel/key scrolling and overview pointer interactions.
use super::super::Language;
use super::*;
use crate::a11y::DialogWindow;

struct Frame {
    offset: Vec2,
    shapes: Vec<egui::epaint::ClippedShape>,
    popup_offset: Vec2,
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
    settings: &EditorSettings,
    time: f64,
    events: Vec<egui::Event>,
    highlights: &[TextRange],
) -> Frame {
    frame_with_popup(ctx, state, view, settings, time, events, highlights, false)
}
#[expect(clippy::too_many_arguments)]
fn frame_with_popup(
    ctx: &egui::Context,
    state: &mut EditorState,
    view: &mut EditorView,
    settings: &EditorSettings,
    time: f64,
    events: Vec<egui::Event>,
    highlights: &[TextRange],
    popup: bool,
) -> Frame {
    let options = EditOptions::from_settings(settings);
    let theme = UiTheme::dark();
    let scheme = ColorScheme::dark();
    let keymap = Keymap::default_keymap();
    let cfg = ViewConfig {
        scheme: &scheme,
        theme: &theme,
        settings,
        opts: &options,
        keymap: &keymap,
        font_size: 14.0,
        search_highlights: highlights,
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
    let mut id = None;
    let mut popup_offset = Vec2::ZERO;
    let output = ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 220.0))),
            ..Default::default()
        },
        |ui| {
            if popup {
                egui::Window::new("Foreground wheel owner")
                    .fixed_pos(Pos2::new(150.0, 20.0))
                    .fixed_size(Vec2::new(240.0, 160.0))
                    .resizable(false)
                    .show_modeless(ui.ctx(), |ui| {
                        popup_offset = egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .id_salt("foreground-wheel-scroll")
                            .show(ui, |ui| {
                                for line in 0..100 {
                                    ui.label(format!("Popup line {line}"));
                                }
                            })
                            .state
                            .offset;
                    });
            }
            id = Some(ui.make_persistent_id(egui::IdSalt::new(view.id.with("scroll"))));
            show(ui, state, view, &cfg);
        },
    );
    let offset = egui::scroll_area::State::load(ctx, id.unwrap())
        .unwrap()
        .offset;
    let shapes = output.shapes.clone();
    output.drop_without_applying_deltas();
    Frame {
        offset,
        shapes,
        popup_offset,
    }
}
fn wheel(delta: Vec2) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(Pos2::new(250.0, 100.0)),
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta,
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}
#[test]
fn smooth_setting_controls_actual_wheel_and_keyboard_scroll_progression() {
    let source = "line π😀 content\n".repeat(100);
    let mut wheel_first = Vec::new();
    let mut keyboard_first = Vec::new();
    for smooth in [false, true] {
        let ctx = context();
        let mut state = EditorState::from_text(&source, Language::PlainText);
        let mut view = EditorView::new(egui::Id::new(("scroll-wheel", smooth)));
        let settings = EditorSettings {
            smooth_scrolling: smooth,
            ..Default::default()
        };
        frame(&ctx, &mut state, &mut view, &settings, 0.0, vec![], &[]);
        let first = frame(
            &ctx,
            &mut state,
            &mut view,
            &settings,
            0.016,
            wheel(Vec2::new(0.0, -120.0)),
            &[],
        );
        wheel_first.push(first.offset.y);
        let second = frame(&ctx, &mut state, &mut view, &settings, 0.032, vec![], &[]);
        if smooth {
            assert!(second.offset.y > first.offset.y);
        } else {
            assert!((first.offset.y - 120.0).abs() < 1.0);
            assert_eq!(second.offset.y, first.offset.y);
        }
        assert_eq!(state.doc.text(), source);
        assert!(!state.doc.can_undo());
        let ctx = context();
        let mut state = EditorState::from_text(&source, Language::PlainText);
        let mut view = EditorView::new(egui::Id::new(("scroll-key", smooth)));
        frame(&ctx, &mut state, &mut view, &settings, 0.0, vec![], &[]);
        let first = frame(
            &ctx,
            &mut state,
            &mut view,
            &settings,
            0.016,
            vec![egui::Event::Key {
                key: egui::Key::ArrowDown,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::CTRL,
            }],
            &[],
        );
        keyboard_first.push(first.offset.y);
        let later = frame(&ctx, &mut state, &mut view, &settings, 0.5, vec![], &[]);
        assert!(later.offset.y > 0.0);
        assert_eq!(state.doc.text(), source);
        assert!(!state.doc.can_undo());
    }
    assert!(wheel_first[1] > 0.0 && wheel_first[1] < wheel_first[0]);
    assert!(keyboard_first[0] > keyboard_first[1]);
}
#[test]
fn horizontal_and_beyond_end_offsets_remain_connected_and_content_unchanged() {
    let source = ("π".repeat(300) + "\n").repeat(10);
    let mut offsets = Vec::new();
    for beyond in [false, true] {
        let ctx = context();
        let mut state = EditorState::from_text(&source, Language::PlainText);
        let mut view = EditorView::new(egui::Id::new(("scroll-end", beyond)));
        let settings = EditorSettings {
            smooth_scrolling: false,
            scroll_beyond_end: beyond,
            ..Default::default()
        };
        frame(&ctx, &mut state, &mut view, &settings, 0.0, vec![], &[]);
        let end = frame(
            &ctx,
            &mut state,
            &mut view,
            &settings,
            0.1,
            wheel(Vec2::new(-80.0, -10000.0)),
            &[],
        );
        assert!(end.offset.x > 0.0);
        offsets.push(end.offset.y);
        assert_eq!(state.doc.text(), source);
        assert!(!state.doc.can_undo());
    }
    assert!(offsets[1] > offsets[0] + 100.0);
}
fn marker(frame: &Frame) -> Pos2 {
    frame
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Rect(rect)
                if rect.fill == Color32::from_rgb(0x5F, 0xA8, 0x6E)
                    && (rect.rect.width() - 6.0).abs() < 0.1
                    && (rect.rect.height() - 2.0).abs() < 0.1 =>
            {
                Some(rect.rect.center())
            }
            _ => None,
        })
        .expect("real search overview marker must be painted")
}
fn click(pos: Pos2, pressed: bool) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(pos),
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        },
    ]
}
#[test]
fn overview_hover_renders_unicode_source_preview_and_click_navigates_readonly_location() {
    let source = (0..80)
        .map(|line| format!("line {line:03} π😀\n"))
        .collect::<String>();
    let ctx = context();
    let mut state = EditorState::from_text(&source, Language::PlainText);
    state.doc.set_read_only(true);
    let offset = state.doc.line_range(42).start();
    let highlights = [TextRange::empty(offset)];
    let mut view = EditorView::new(egui::Id::new("scroll-preview"));
    let settings = EditorSettings {
        smooth_scrolling: false,
        ..Default::default()
    };
    let first = frame(
        &ctx,
        &mut state,
        &mut view,
        &settings,
        0.0,
        vec![],
        &highlights,
    );
    let point = marker(&first);
    frame(
        &ctx,
        &mut state,
        &mut view,
        &settings,
        0.1,
        vec![egui::Event::PointerMoved(point)],
        &highlights,
    );
    // Leave the pointer still through the hover delay and size the tooltip.
    frame(
        &ctx,
        &mut state,
        &mut view,
        &settings,
        1.0,
        vec![],
        &highlights,
    );

    let hover = frame(
        &ctx,
        &mut state,
        &mut view,
        &settings,
        1.016,
        vec![],
        &highlights,
    );
    assert!(hover
        .shapes
        .iter()
        .any(|shape| matches!(&shape.shape,egui::Shape::Text(text)
        if text.galley.text().contains("line 042 π😀"))));
    frame(
        &ctx,
        &mut state,
        &mut view,
        &settings,
        1.1,
        click(point, true),
        &highlights,
    );
    frame(
        &ctx,
        &mut state,
        &mut view,
        &settings,
        1.11,
        click(point, false),
        &highlights,
    );
    assert_eq!(state.primary().head(), offset);
    assert_eq!(state.doc.text(), source);
    assert!(!state.doc.can_undo());
    assert!(state.doc.is_read_only());
}
#[test]
fn hidden_fold_marker_click_reveals_actual_source_without_a_text_click_or_edit() {
    let source = "def f():\n    hidden = 'π😀'\n    return hidden\n\noutside = 1\n";
    let ctx = context();
    let mut state = EditorState::from_text(source, Language::Python);
    state.collapse_all_folds();
    assert!(!state.hidden_line_ranges().is_empty());
    let offset = state.doc.line_range(1).start() + TextSize::new(4);
    let highlights = [TextRange::empty(offset)];
    let mut view = EditorView::new(egui::Id::new("scroll-fold-preview"));
    let settings = EditorSettings {
        smooth_scrolling: false,
        ..Default::default()
    };
    let first = frame(
        &ctx,
        &mut state,
        &mut view,
        &settings,
        0.0,
        vec![],
        &highlights,
    );
    let point = marker(&first);
    frame(
        &ctx,
        &mut state,
        &mut view,
        &settings,
        0.1,
        click(point, true),
        &highlights,
    );
    frame(
        &ctx,
        &mut state,
        &mut view,
        &settings,
        0.11,
        click(point, false),
        &highlights,
    );
    assert_eq!(state.primary().head(), offset);
    assert!(state.hidden_line_ranges().is_empty());
    assert_eq!(state.doc.text(), source);
    assert!(!state.doc.can_undo());
}

#[test]
fn foreground_scroll_area_consumes_wheel_without_editor_replay() {
    let ctx = context();
    let source = "editor line π😀\n".repeat(100);
    let mut state = EditorState::from_text(&source, Language::PlainText);
    let mut view = EditorView::new(egui::Id::new("overlay-editor"));
    let settings = EditorSettings {
        smooth_scrolling: false,
        ..Default::default()
    };
    for time in [0.0, 0.016] {
        frame_with_popup(
            &ctx,
            &mut state,
            &mut view,
            &settings,
            time,
            vec![egui::Event::PointerMoved(Pos2::new(250.0, 100.0))],
            &[],
            true,
        );
    }
    let result = frame_with_popup(
        &ctx,
        &mut state,
        &mut view,
        &settings,
        0.032,
        wheel(Vec2::new(0.0, -120.0)),
        &[],
        true,
    );
    assert!(result.popup_offset.y > 0.0);
    assert_eq!(result.offset, Vec2::ZERO);
    assert_eq!(ctx.input(|input| input.smooth_scroll_delta), Vec2::ZERO);
    assert_eq!(state.doc.text(), source);
    assert!(!state.doc.can_undo());
}

#[test]
fn released_trackpad_modifier_keeps_actual_gesture_axis() {
    let ctx = context();
    let source = format!("{}\n", "wide π😀 ".repeat(100)).repeat(100);
    let mut state = EditorState::from_text(&source, Language::PlainText);
    let mut view = EditorView::new(egui::Id::new("gesture-editor"));
    let settings = EditorSettings {
        smooth_scrolling: false,
        ..Default::default()
    };
    frame(&ctx, &mut state, &mut view, &settings, 0.0, vec![], &[]);
    let event = |phase, modifiers, delta| egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        phase,
        modifiers,
        delta,
    };
    frame(
        &ctx,
        &mut state,
        &mut view,
        &settings,
        0.016,
        vec![
            egui::Event::PointerMoved(Pos2::new(250.0, 100.0)),
            event(egui::TouchPhase::Start, egui::Modifiers::SHIFT, Vec2::ZERO),
        ],
        &[],
    );
    let moved = frame(
        &ctx,
        &mut state,
        &mut view,
        &settings,
        0.032,
        vec![event(
            egui::TouchPhase::Move,
            egui::Modifiers::NONE,
            Vec2::new(0.0, -80.0),
        )],
        &[],
    );
    assert_eq!(moved.offset.x, 80.0);
    assert_eq!(moved.offset.y, 0.0);
    frame(
        &ctx,
        &mut state,
        &mut view,
        &settings,
        0.048,
        vec![event(
            egui::TouchPhase::End,
            egui::Modifiers::NONE,
            Vec2::ZERO,
        )],
        &[],
    );
    let vertical = frame(
        &ctx,
        &mut state,
        &mut view,
        &settings,
        0.064,
        wheel(Vec2::new(0.0, -80.0)),
        &[],
    );
    assert_eq!(vertical.offset.x, 80.0);
    assert_eq!(vertical.offset.y, 80.0);
    assert_eq!(state.doc.text(), source);
    assert!(!state.doc.can_undo());
}

#[test]
fn overview_pointer_target_never_steals_tab_after_initial_or_reopened_editor_focus() {
    let ctx = context();
    let mut state = EditorState::from_text("π", Language::PlainText);
    let mut view = EditorView::new(egui::Id::new("overview-tab-focus"));
    let settings = EditorSettings::default();
    let highlights = [TextRange::new(TextSize::new(0), TextSize::of("π"))];
    state.set_carets(vec![Caret::at(state.doc.len())]);
    for (phase, time) in [("initial", 0.0), ("reopened", 1.0)] {
        // Simulate returning from an owning popup: the editor must acquire new
        // focus, whose Tab lock is not yet established at egui begin_pass.
        ctx.memory_mut(|memory| memory.surrender_focus(view.id));
        frame(
            &ctx,
            &mut state,
            &mut view,
            &settings,
            time,
            vec![],
            &highlights,
        );
        assert!(view.focused, "{phase} editor must acquire focus");
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(view.id));
        frame(
            &ctx,
            &mut state,
            &mut view,
            &settings,
            time + 0.1,
            vec![
                egui::Event::Key {
                    key: egui::Key::Tab,
                    physical_key: Some(egui::Key::Tab),
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::Key {
                    key: egui::Key::Tab,
                    physical_key: Some(egui::Key::Tab),
                    pressed: false,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            &highlights,
        );
        assert_eq!(
            state.doc.text(),
            "π   ",
            "{phase} Tab must indent in the editor"
        );
        assert!(view.focused, "{phase} overview must not take focus");
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(view.id));
        assert_ne!(
            ctx.memory(|memory| memory.focused()),
            Some(view.id.with("stripe-preview"))
        );
        assert!(state.undo());
        assert_eq!(state.doc.text(), "π");
        assert!(!state.doc.can_undo());
    }
}
