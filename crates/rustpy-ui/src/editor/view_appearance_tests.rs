//! Actual view regressions for caret painting and source-aligned whitespace.
use super::super::{Caret, Language};
use super::*;
use rustpy_settings::CaretShape;

struct Frame {
    shapes: Vec<egui::epaint::ClippedShape>,
    ime: Option<egui::output::IMEOutput>,
}
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx
}
fn paint(
    ctx: &egui::Context,
    state: &mut EditorState,
    view: &mut EditorView,
    settings: &EditorSettings,
    time: f64,
    focus: bool,
    keyboard: bool,
) -> Frame {
    paint_events(ctx, state, view, settings, time, (focus, keyboard), vec![])
}
fn paint_events(
    ctx: &egui::Context,
    state: &mut EditorState,
    view: &mut EditorView,
    settings: &EditorSettings,
    time: f64,
    input_owner: (bool, bool),
    events: Vec<egui::Event>,
) -> Frame {
    let (focus, keyboard) = input_owner;
    let scheme = ColorScheme::dark();
    let theme = UiTheme::dark();
    let opts = EditOptions::from_settings(settings);
    let keymap = Keymap::default_keymap();
    let cfg = ViewConfig {
        scheme: &scheme,
        theme: &theme,
        settings,
        opts: &opts,
        keymap: &keymap,
        font_size: 14.0,
        search_highlights: &[],
        request_focus: focus,
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
        keyboard,
    };
    let output = ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0))),
            ..Default::default()
        },
        |ui| {
            show(ui, state, view, &cfg);
        },
    );
    let result = Frame {
        shapes: output.shapes.clone(),
        ime: output.platform_output.ime,
    };
    output.drop_without_applying_deltas();
    result
}
fn carets(frame: &Frame) -> Vec<Rect> {
    let color = rgb(ColorScheme::dark().editor.caret);
    frame
        .shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::Shape::Rect(r) if r.fill == color => Some(r.rect),
            _ => None,
        })
        .collect()
}
fn dots(frame: &Frame) -> Vec<Pos2> {
    let color = rgb(ColorScheme::dark().editor.whitespace);
    frame
        .shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::Shape::Circle(c) if c.fill == color && c.radius == 1.0 => Some(c.center),
            _ => None,
        })
        .collect()
}
fn ending_marks(frame: &Frame) -> usize {
    frame
        .shapes
        .iter()
        .filter(|s| matches!(&s.shape, egui::Shape::Text(t) if t.galley.job.text == "↵"))
        .count()
}

#[test]
fn actual_thin_and_underscore_shapes_match_ime_geometry() {
    let ctx = context();
    let mut state = EditorState::from_text("hello", Language::PlainText);
    let mut view = EditorView::new(egui::Id::new("appearance-shapes"));
    let mut settings = EditorSettings {
        caret_blinking: false,
        caret_width: 5,
        ..Default::default()
    };
    let thin = paint(&ctx, &mut state, &mut view, &settings, 0.0, true, true);
    let rect = view.caret_screen_rect.unwrap();
    assert_eq!(carets(&thin), vec![rect]);
    assert_eq!(thin.ime.unwrap().cursor_rect, rect);
    assert!((rect.width() - 5.0).abs() < 0.01);
    settings.caret_shape = CaretShape::Underscore;
    let underscore = paint(&ctx, &mut state, &mut view, &settings, 0.1, false, true);
    let bottom = view.caret_screen_rect.unwrap();
    assert_eq!(carets(&underscore), vec![bottom]);
    assert_eq!(underscore.ime.unwrap().cursor_rect, bottom);
    assert!((bottom.height() - 5.0).abs() < 0.01);
    assert!((bottom.bottom() - rect.bottom()).abs() < 0.01);
    assert!(bottom.width() > rect.width());
}

#[test]
fn actual_blink_off_keeps_ime_and_focus_reentry_resets_on_phase() {
    let ctx = context();
    let mut state = EditorState::from_text("x", Language::PlainText);
    let mut view = EditorView::new(egui::Id::new("appearance-blink"));
    let settings = EditorSettings::default();
    let on = paint(&ctx, &mut state, &mut view, &settings, 10.0, true, true);
    assert_eq!(carets(&on).len(), 1);
    let rect = view.caret_screen_rect.unwrap();
    let off = paint(&ctx, &mut state, &mut view, &settings, 10.6, false, true);
    assert!(carets(&off).is_empty());
    assert_eq!(view.caret_screen_rect, Some(rect));
    assert_eq!(off.ime.unwrap().cursor_rect, rect);
    paint(&ctx, &mut state, &mut view, &settings, 10.65, false, false);
    assert!(!view.focused);
    let reacquired = paint(&ctx, &mut state, &mut view, &settings, 10.7, true, true);
    assert!(view.focused);
    assert_eq!(carets(&reacquired).len(), 1);
    let later = paint(&ctx, &mut state, &mut view, &settings, 11.21, false, true);
    assert!(
        carets(&later).is_empty(),
        "ordinary frames must not restart blink"
    );
}

#[test]
fn actual_overwrite_blocks_use_wide_unicode_and_tab_glyph_geometry() {
    for source in ["界x", "\tx"] {
        let ctx = context();
        let mut state = EditorState::from_text(source, Language::PlainText);
        state.overwrite_mode = true;
        let mut view = EditorView::new(egui::Id::new("appearance-overwrite"));
        let settings = EditorSettings {
            caret_blinking: false,
            caret_shape: CaretShape::Underscore,
            ..Default::default()
        };
        let frame = paint(&ctx, &mut state, &mut view, &settings, 0.0, true, true);
        let rect = view.caret_screen_rect.unwrap();
        assert_eq!(carets(&frame), vec![rect]);
        // Galley::size is pixel-rounded overall layout bounds, not the
        // measured source-cell advances used for the painted caret. Inspect
        // the actual text primitive and independently measure its placed source glyph interval.
        let displayed = if source.starts_with('\t') {
            "    x"
        } else {
            "界x"
        };
        let text = frame
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == displayed => Some(text),
                _ => None,
            })
            .expect("the source text must actually be painted");
        let count = if source.starts_with('\t') { 4 } else { 1 };
        let row = &text.galley.rows[0];
        assert!(
            row.glyphs.len() > count,
            "following source glyph must remain outside caret"
        );
        // egui pixel-snaps each placed glyph independently; summed advances
        // and overall rounded galley bounds are not the actual glyph interval.
        let first = &row.glyphs[0];
        let last = &row.glyphs[count - 1];
        let expected_left = text.pos.x + row.pos.x + first.pos.x;
        let expected_right = text.pos.x + row.pos.x + last.pos.x + last.advance_width;
        assert!(
            (rect.left() - expected_left).abs() < 0.01
                && (rect.right() - expected_right).abs() < 0.01,
            "{source:?}: {rect:?} versus painted source interval {expected_left}..{expected_right}; positions {:?}",
            row.glyphs.iter().map(|glyph| (glyph.pos.x, glyph.advance_width)).collect::<Vec<_>>()
        );
        assert!(
            row.glyphs[count].pos.x > last.pos.x,
            "following source glyph has a distinct position outside the selected glyph set"
        );
        if source.starts_with('\t') {
            assert!(
                row.glyphs[..count].iter().all(|glyph| glyph.chr == ' '),
                "tab source must paint four actual expanded space glyphs"
            );
            assert_eq!(row.glyphs[count].chr, 'x');
        }
        if source.starts_with('\t') {
            assert!(
                rect.width() > 3.9 * row.glyphs[0].advance_width,
                "tab overwrite must cover all four expanded source cells"
            );
        }
        assert!(
            rect.height() > 10.0,
            "overwrite overrides underscore thickness"
        );
    }
}

#[test]
fn actual_whitespace_categories_and_no_phantom_terminal_ending() {
    let ctx = context();
    let mut state = EditorState::from_text("  a b  \nlast", Language::PlainText);
    let mut view = EditorView::new(egui::Id::new("appearance-categories"));
    let mut settings = EditorSettings {
        show_whitespaces: true,
        show_line_separators: true,
        show_leading_whitespaces: true,
        show_inner_whitespaces: false,
        show_trailing_whitespaces: false,
        ..Default::default()
    };
    let leading = paint(&ctx, &mut state, &mut view, &settings, 0.0, false, false);
    assert_eq!(dots(&leading).len(), 2);
    assert_eq!(ending_marks(&leading), 1);
    settings.show_leading_whitespaces = false;
    settings.show_inner_whitespaces = true;
    let inner = paint(&ctx, &mut state, &mut view, &settings, 0.1, false, false);
    assert_eq!(dots(&inner).len(), 1);
    settings.show_inner_whitespaces = false;
    settings.show_trailing_whitespaces = true;
    let trailing = paint(&ctx, &mut state, &mut view, &settings, 0.2, false, false);
    assert_eq!(dots(&trailing).len(), 2);
    settings.show_whitespaces = false;
    settings.show_line_separators = false;
    let hidden = paint(&ctx, &mut state, &mut view, &settings, 0.3, false, false);
    assert!(dots(&hidden).is_empty());
    assert_eq!(ending_marks(&hidden), 0);
}

#[test]
fn actual_marker_at_same_offset_hint_is_after_hint_glyphs() {
    let ctx = context();
    let mut state = EditorState::from_text("a b", Language::PlainText);
    let mut view = EditorView::new(egui::Id::new("appearance-inlay"));
    let settings = EditorSettings {
        show_whitespaces: true,
        ..Default::default()
    };
    let baseline = paint(&ctx, &mut state, &mut view, &settings, 0.0, false, false);
    let before = dots(&baseline);
    assert_eq!(before.len(), 1);
    state.inlay_hints = vec![rustpy_semantic::features::InlayHint {
        offset: TextSize::new(1),
        label: "hint".into(),
        kind: rustpy_semantic::features::InlayKind::Type,
    }];
    let hinted = paint(&ctx, &mut state, &mut view, &settings, 0.1, false, false);
    let after = dots(&hinted);
    assert_eq!(after.len(), 1);
    assert!(
        after[0].x > before[0].x + 20.0,
        "source-space dot must not occupy hint"
    );
    assert!((after[0].y - before[0].y).abs() < 0.1);
}

#[test]
fn actual_hidden_reader_comments_do_not_publish_whitespace_markers() {
    let ctx = context();
    let source = "value = 1  # hidden comment spaces  \nnext = 2\n";
    let mut state = EditorState::from_text(source, Language::Python);
    state.doc.set_read_only(true);
    state.set_carets(vec![Caret::at(state.doc.line_range(1).start())]);
    let mut view = EditorView::new(egui::Id::new("appearance-reader-marks"));
    let settings = EditorSettings {
        show_whitespaces: true,
        ..Default::default()
    };
    let hidden = paint(&ctx, &mut state, &mut view, &settings, 0.0, false, false);
    let hidden_count = dots(&hidden).len();
    state.set_carets(vec![Caret::at(TextSize::new(0))]);
    let revealed = paint(&ctx, &mut state, &mut view, &settings, 0.1, false, false);
    assert_eq!(
        dots(&revealed).len(),
        hidden_count + 5,
        "all comment spaces reappear on caret source line"
    );
    assert_eq!(state.doc.text(), source);
    assert!(!state.doc.is_modified());
}

#[test]
fn actual_wrapped_galley_groups_tab_glyphs_by_visual_row_and_places_boundary_space() {
    let ctx = context();
    let mut state = EditorState::from_text("x", Language::PlainText);
    let mut view = EditorView::new(egui::Id::new("appearance-galley"));
    paint(
        &ctx,
        &mut state,
        &mut view,
        &EditorSettings::default(),
        0.0,
        false,
        false,
    );
    let font = FontId::new(14.0, mono_family(false, false));
    let galley =
        ctx.fonts_mut(|fonts| fonts.layout("abcd    z".into(), font, Color32::WHITE, 25.0));
    assert!(galley.rows.len() > 1);
    let ll = LineLayout {
        visual: None,
        line: 0,
        galley,
        origin: Pos2::ZERO,
        map: None,
        hints: vec![],
    };
    let rects = glyph_span_rects(
        &ll,
        source_glyph_index(&ll, "abcd\tz", 4, 4),
        source_glyph_len("abcd\tz", 4, 4),
    );
    assert!(!rects.is_empty());
    assert!(rects.iter().all(|r| r.width() > 0.0 && r.height() > 0.0));
    for rect in &rects {
        assert!(ll
            .galley
            .rows
            .iter()
            .any(|row| (row.pos.y - rect.top()).abs() < 0.01));
    }
    // Pin an actual row boundary and ensure source glyphs choose that next row,
    // unlike cursor-boundary placement preferring the previous row.
    let boundary = ll.galley.rows[0].glyphs.len();
    let glyph = glyph_span_rects(&ll, boundary, 1);
    assert_eq!(glyph.len(), 1);
    assert!((glyph[0].top() - ll.galley.rows[1].pos.y).abs() < 0.01);
}

#[test]
fn actual_typing_restarts_blink_and_insert_dispatch_preserves_unicode_undo() {
    let ctx = context();
    let mut state = EditorState::from_text("界x\nnext", Language::PlainText);
    let mut view = EditorView::new(egui::Id::new("appearance-input"));
    let settings = EditorSettings::default();
    paint(&ctx, &mut state, &mut view, &settings, 20.0, true, true);
    let off = paint(&ctx, &mut state, &mut view, &settings, 20.6, false, true);
    assert!(carets(&off).is_empty());
    let modifiers = egui::Modifiers::default();
    paint_events(
        &ctx,
        &mut state,
        &mut view,
        &settings,
        20.65,
        (false, true),
        vec![egui::Event::Key {
            key: egui::Key::Insert,
            physical_key: Some(egui::Key::Insert),
            pressed: true,
            repeat: false,
            modifiers,
        }],
    );
    assert!(
        state.overwrite_mode,
        "real editor Insert action must toggle overwrite"
    );
    paint_events(
        &ctx,
        &mut state,
        &mut view,
        &settings,
        20.7,
        (false, true),
        vec![egui::Event::Text("é".into())],
    );
    assert_eq!(state.doc.text(), "éx\nnext");
    // Input is handled after painting; the following frame sees the activity clock.
    let after = paint(&ctx, &mut state, &mut view, &settings, 20.71, false, true);
    assert_eq!(carets(&after).len(), 1);
    assert!(state.undo());
    assert_eq!(state.doc.text(), "界x\nnext");
    assert_eq!(state.primary().head(), TextSize::new(0));
}

fn method_stripes(frame: &Frame) -> Vec<[Pos2; 2]> {
    let color = rgb(ColorScheme::dark().editor.gutter_separator);
    frame
        .shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::Shape::LineSegment { points, stroke }
                if stroke.color == color
                    && (points[0].y - points[1].y).abs() < 0.01
                    && points[1].x - points[0].x > 100.0 =>
            {
                Some(*points)
            }
            _ => None,
        })
        .collect()
}
fn filled_color_rects(frame: &Frame, color: Color32) -> Vec<Rect> {
    frame
        .shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::Shape::Rect(r) if r.fill == color && (r.rect.height() - 10.0).abs() < 0.01 => {
                Some(r.rect)
            }
            _ => None,
        })
        .collect()
}

#[test]
fn actual_definition_stripes_follow_settings_and_hidden_fold_headers() {
    let ctx = context();
    let source =
        "# heading\nclass Outer:\n    def inner(self):\n        pass\n\ndef after():\n    pass\n";
    let mut state = EditorState::from_text(source, Language::Python);
    let mut view = EditorView::new(egui::Id::new("appearance-methods"));
    let mut settings = EditorSettings {
        show_method_separators: true,
        ..Default::default()
    };
    let expanded = paint(&ctx, &mut state, &mut view, &settings, 0.0, false, false);
    let stripes = method_stripes(&expanded);
    assert_eq!(
        stripes.len(),
        3,
        "class, nested method and later function boundaries"
    );
    assert!(stripes.windows(2).all(|s| s[0][0].y < s[1][0].y));
    state.toggle_fold_at_line(1);
    let folded = paint(&ctx, &mut state, &mut view, &settings, 0.1, false, false);
    assert_eq!(
        method_stripes(&folded).len(),
        2,
        "hidden nested header must not paint"
    );
    settings.show_method_separators = false;
    assert!(method_stripes(&paint(
        &ctx, &mut state, &mut view, &settings, 0.2, false, false
    ))
    .is_empty());
    assert_eq!(state.doc.text(), source);
    assert!(!state.doc.is_modified());
}

#[test]
fn actual_css_swatches_are_distinct_exact_gutter_colors_and_obey_language_and_setting() {
    let ctx = context();
    let source = "a { color: #ff1234; background: #12abef; }\n";
    let mut state = EditorState::from_text(source, Language::Css);
    let mut view = EditorView::new(egui::Id::new("appearance-css"));
    let mut settings = EditorSettings {
        show_css_color_preview: true,
        ..Default::default()
    };
    let shown = paint(&ctx, &mut state, &mut view, &settings, 0.0, false, false);
    let red = filled_color_rects(&shown, Color32::from_rgb(255, 18, 52));
    let blue = filled_color_rects(&shown, Color32::from_rgb(18, 171, 239));
    assert_eq!(red.len(), 1);
    assert_eq!(blue.len(), 1);
    assert!(
        red[0].right() <= blue[0].left(),
        "two source colors get distinct swatch slots"
    );
    settings.show_css_color_preview = false;
    let disabled = paint(&ctx, &mut state, &mut view, &settings, 0.1, false, false);
    assert!(filled_color_rects(&disabled, Color32::from_rgb(255, 18, 52)).is_empty());
    settings.show_css_color_preview = true;
    state.language = Language::Python;
    let python = paint(&ctx, &mut state, &mut view, &settings, 0.2, false, false);
    assert!(filled_color_rects(&python, Color32::from_rgb(18, 171, 239)).is_empty());
    assert_eq!(state.doc.text(), source);
    assert!(!state.doc.is_modified());
}

#[test]
fn actual_definition_and_color_caches_invalidate_on_equal_version_document_swap() {
    let ctx = context();
    let mut view = EditorView::new(egui::Id::new("appearance-cache-swap"));
    let settings = EditorSettings {
        show_method_separators: true,
        show_css_color_preview: true,
        ..Default::default()
    };
    let mut state = EditorState::from_text("# heading\ndef old(): pass\n", Language::Python);
    let original_version = state.doc.version();
    assert_eq!(
        method_stripes(&paint(
            &ctx, &mut state, &mut view, &settings, 0.0, false, false
        ))
        .len(),
        1
    );
    let replacement = EditorState::from_text("plain = 1\n", Language::Python);
    assert_eq!(replacement.doc.version(), original_version);
    state = replacement;
    assert!(method_stripes(&paint(
        &ctx, &mut state, &mut view, &settings, 0.1, false, false
    ))
    .is_empty());
    state = EditorState::from_text("a { color: #ff1234; }\n", Language::Css);
    assert_eq!(
        filled_color_rects(
            &paint(&ctx, &mut state, &mut view, &settings, 0.2, false, false),
            Color32::from_rgb(255, 18, 52)
        )
        .len(),
        1
    );
    let replacement = EditorState::from_text("a { color: #12abef; }\n", Language::Css);
    assert_eq!(replacement.doc.version(), state.doc.version());
    state = replacement;
    let final_frame = paint(&ctx, &mut state, &mut view, &settings, 0.3, false, false);
    assert!(filled_color_rects(&final_frame, Color32::from_rgb(255, 18, 52)).is_empty());
    assert_eq!(
        filled_color_rects(&final_frame, Color32::from_rgb(18, 171, 239)).len(),
        1
    );
    assert_eq!(state.doc.text(), "a { color: #12abef; }\n");
    assert!(!state.doc.is_modified());
}

#[test]
fn actual_dense_color_line_uses_readable_swatches_and_explicit_overflow() {
    let source = format!(
        "a {{ background: linear-gradient({}); }}",
        ["#123456"; 120].join(", ")
    );
    let mut state = EditorState::from_text(&source, Language::Css);
    let mut view = EditorView::new(egui::Id::new("dense-color-preview"));
    let ctx = context();
    let frame = paint(
        &ctx,
        &mut state,
        &mut view,
        &EditorSettings::default(),
        0.0,
        false,
        false,
    );
    let rects = frame
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Rect(rect) if rect.fill == Color32::from_rgb(0x12, 0x34, 0x56) => {
                Some(rect.rect)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(rects.len(), 7);
    assert!(rects.iter().all(|rect| rect.width() == 10.0));
    assert!(rects
        .windows(2)
        .all(|pair| pair[0].right() < pair[1].left()));
    assert!(frame.shapes.iter().any(
        |shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == "+")
    ));
    assert_eq!(state.doc.text(), source);
}
