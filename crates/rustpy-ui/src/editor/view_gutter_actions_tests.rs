//! Real View layout/input: gutter intentions do not edit or move source carets.
use super::super::Language;
use super::*;

fn frame(
    ctx: &egui::Context,
    state: &mut EditorState,
    view: &mut EditorView,
    markers: &[GutterMarker],
    events: Vec<egui::Event>,
    time: f64,
) -> (Vec<egui::epaint::ClippedShape>, Vec<ViewEvent>) {
    let settings = EditorSettings::default();
    let scheme = ColorScheme::dark();
    let theme = UiTheme::dark();
    let opts = EditOptions::from_settings(&settings);
    let keymap = Keymap::default_keymap();
    let cfg = ViewConfig {
        code_vision: None,
        gutter_markers: markers,
        gutter_quick_fixes: &[TextSize::new(0)],
        scheme: &scheme,
        theme: &theme,
        settings: &settings,
        opts: &opts,
        keymap: &keymap,
        font_size: 14.0,
        search_highlights: &[],
        request_focus: true,
        run_lines: &[0],
        breakpoints: &[(0, BpGlyph::Enabled)],
        bookmarks: &[(0, None)],
        line_changes: &[rustpy_vcs::LineChange {
            kind: rustpy_vcs::ChangeKind::Added,
            start: 0,
            count: 1,
            removed: 0,
            old_text: String::new(),
        }],
        exec_line: Some(0),
        inline_values: &[],
        annotations: None,
        coverage: &[],
        keyboard: true,
    };
    let mut result = Vec::new();
    let output = ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(650.0, 320.0))),
            ..Default::default()
        },
        |ui| {
            result = show(ui, state, view, &cfg);
        },
    );
    let shapes = output.shapes.clone();
    output.drop_without_applying_deltas();
    (shapes, result)
}
fn glyph(shapes: &[egui::epaint::ClippedShape], value: &str) -> Vec<Rect> {
    shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::Shape::Text(t) if t.galley.text() == value => {
                Some(t.galley.rect.translate(t.pos.to_vec2()))
            }
            _ => None,
        })
        .collect()
}
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx
}
fn marker(offset: TextSize) -> GutterMarker {
    GutterMarker {
        offset,
        has_super: true,
        has_implementations: true,
    }
}

#[test]
fn lanes_coexist_and_pointer_clicks_emit_exact_intents_without_caret_or_edit_changes() {
    let ctx = context();
    let mut state = EditorState::from_text("def f():\n    pass\n", Language::Python);
    let mut view = EditorView::new(egui::Id::new("gutter-actions"));
    let markers = [marker(TextSize::new(0))];
    let (shapes, _) = frame(&ctx, &mut state, &mut view, &markers, vec![], 0.0);
    let super_rect = glyph(&shapes, "↑")[0];
    let implementation_rect = glyph(&shapes, "↓")[0];
    let quick_rect = glyph(&shapes, "💡")[0];
    assert!(
        quick_rect.center().x < super_rect.center().x
            && super_rect.center().x < implementation_rect.center().x
    );
    let source = state.doc.text();
    let carets = state.carets.clone();
    let version = state.doc.version();
    for (index, (rect, kind)) in [
        (quick_rect, super::super::gutter_actions::Kind::QuickFix),
        (super_rect, super::super::gutter_actions::Kind::Super),
        (
            implementation_rect,
            super::super::gutter_actions::Kind::Implementations,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let pos = rect.center();
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        frame(
            &ctx,
            &mut state,
            &mut view,
            &markers,
            vec![egui::Event::PointerMoved(pos), button(true)],
            0.1 + index as f64,
        );
        let (_, events) = frame(
            &ctx,
            &mut state,
            &mut view,
            &markers,
            vec![button(false)],
            0.2 + index as f64,
        );
        assert_eq!(
            events
                .iter()
                .filter_map(|e| if let ViewEvent::GutterIntent(i) = e {
                    Some(*i)
                } else {
                    None
                })
                .collect::<Vec<_>>(),
            vec![super::super::gutter_actions::Intent {
                offset: TextSize::new(0),
                kind
            }]
        );
        assert!(!events.iter().any(|e| matches!(
            e,
            ViewEvent::GutterClick(_) | ViewEvent::GutterRun(_) | ViewEvent::GutterContext(_)
        )));
        assert_eq!(state.carets, carets);
        assert_eq!(state.doc.version(), version);
        assert_eq!(state.doc.text(), source);
    }
}
#[test]
fn folded_nested_markers_are_hidden_without_aliasing_the_header() {
    let ctx = context();
    let source = "class Outer:\n    def inner(self):\n        pass\n";
    let mut state = EditorState::from_text(source, Language::Python);
    let mut view = EditorView::new(egui::Id::new("gutter-fold"));
    let markers = [
        marker(TextSize::new(0)),
        marker(TextSize::new(source.find("def").unwrap() as u32)),
    ];
    let (expanded, _) = frame(&ctx, &mut state, &mut view, &markers, vec![], 0.0);
    assert_eq!(glyph(&expanded, "↑").len(), 2);
    state.toggle_fold_at_line(0);
    let (folded, _) = frame(&ctx, &mut state, &mut view, &markers, vec![], 0.1);
    assert_eq!(glyph(&folded, "↑").len(), 1);
    assert_eq!(glyph(&folded, "↓").len(), 1);
    assert_eq!(state.doc.text(), source);
}
#[test]
fn read_only_super_lane_still_emits_navigation_without_refusing_an_edit() {
    let ctx = context();
    let mut state = EditorState::from_text("def f():\n    pass\n", Language::Python);
    state.doc.set_read_only(true);
    let mut view = EditorView::new(egui::Id::new("gutter-readonly"));
    let markers = [marker(TextSize::new(0))];
    let (shapes, _) = frame(&ctx, &mut state, &mut view, &markers, vec![], 0.0);
    let pos = glyph(&shapes, "↑")[0].center();
    let button = |pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    frame(
        &ctx,
        &mut state,
        &mut view,
        &markers,
        vec![egui::Event::PointerMoved(pos), button(true)],
        0.1,
    );
    let (_, events) = frame(
        &ctx,
        &mut state,
        &mut view,
        &markers,
        vec![button(false)],
        0.2,
    );
    assert!(events.iter().any(|e| matches!(e, ViewEvent::GutterIntent(i) if i.kind == super::super::gutter_actions::Kind::Super)));
    assert!(!state.read_only_refused);
    assert_eq!(state.doc.text(), "def f():\n    pass\n");
}

#[test]
fn gutter_press_owns_drag_and_release_after_pointer_leaves_the_lane() {
    let ctx = context();
    let source = "def f():\n    pass\n";
    let mut state = EditorState::from_text(source, Language::Python);
    let mut view = EditorView::new(egui::Id::new("gutter-drag"));
    let markers = [marker(TextSize::new(0))];
    let (shapes, _) = frame(&ctx, &mut state, &mut view, &markers, vec![], 0.0);
    let press = glyph(&shapes, "↑")[0].center();
    let target = glyph(&shapes, "def f():")[0].right_bottom();
    let carets = state.carets.clone();
    let version = state.doc.version();
    frame(
        &ctx,
        &mut state,
        &mut view,
        &markers,
        vec![
            egui::Event::PointerMoved(press),
            egui::Event::PointerButton {
                pos: press,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
        ],
        0.1,
    );
    // Ownership survives even if current marker inputs become unavailable mid-gesture.
    frame(
        &ctx,
        &mut state,
        &mut view,
        &[],
        vec![egui::Event::PointerMoved(target)],
        0.2,
    );
    let (_, events) = frame(
        &ctx,
        &mut state,
        &mut view,
        &[],
        vec![egui::Event::PointerButton {
            pos: target,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        }],
        0.3,
    );
    assert_eq!(state.carets, carets);
    assert_eq!(state.doc.text(), source);
    assert_eq!(state.doc.version(), version);
    assert!(!state.undo(), "gutter drag must create no history edit");
    assert!(!events
        .iter()
        .any(|e| matches!(e, ViewEvent::GutterIntent(_))));
}
