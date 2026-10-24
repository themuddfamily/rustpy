//! Connected Smart Keys consumers through actual editor pointer/key/paste events.
use super::super::{Caret, Language};
use super::*;

fn context() -> egui::Context {
    let context = egui::Context::default();
    crate::theme::install_fonts(&context);
    context
}
fn frame(
    ctx: &egui::Context,
    state: &mut EditorState,
    view: &mut EditorView,
    settings: &EditorSettings,
    time: f64,
    events: Vec<egui::Event>,
) {
    let scheme = ColorScheme::dark();
    let theme = UiTheme::dark();
    let options = EditOptions::from_settings(settings);
    let keymap = Keymap::default_keymap();
    let cfg = ViewConfig {
        scheme: &scheme,
        theme: &theme,
        settings,
        opts: &options,
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
    ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
            ..Default::default()
        },
        |ui| {
            show(ui, state, view, &cfg);
        },
    )
    .drop_without_applying_deltas();
}
fn key(key: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
    vec![egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }]
}

#[test]
fn actual_double_click_uses_effective_camel_humps_and_preserves_source_history() {
    for enabled in [false, true] {
        let ctx = context();
        let mut state = EditorState::from_text("fooBar\n", Language::Python);
        state.set_carets(vec![Caret::at(TextSize::new(4))]);
        let mut view = EditorView::new(egui::Id::new(("smart-double", enabled)));
        let settings = EditorSettings {
            camel_humps_words: enabled,
            ..Default::default()
        };
        frame(&ctx, &mut state, &mut view, &settings, 0.0, vec![]);
        let pointer = view.caret_screen_rect.unwrap().center();
        for (time, pressed) in [(0.1, true), (0.11, false), (0.2, true), (0.21, false)] {
            frame(
                &ctx,
                &mut state,
                &mut view,
                &settings,
                time,
                vec![
                    egui::Event::PointerMoved(pointer),
                    egui::Event::PointerButton {
                        pos: pointer,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        let selected = state.doc.slice(state.primary().range());
        assert_eq!(selected, if enabled { "Bar" } else { "fooBar" });
        assert_eq!(state.doc.text(), "fooBar\n");
        assert!(!state.undo());
    }
}

#[test]
fn ctrl_w_mixed_carets_use_hump_and_syntax_fallback_with_one_shrink_history() {
    let ctx = context();
    let source = "fooBar\nvalue + other\n";
    let original = vec![
        Caret::at(TextSize::new(3)),
        Caret::new(TextSize::new(7), TextSize::new(12)),
    ];
    let mut state = EditorState::from_text(source, Language::Python);
    state.set_carets(original.clone());
    let mut legacy = EditorState::from_text(source, Language::Python);
    legacy.set_carets(original.clone());
    legacy.extend_selection();
    let syntax_expected = legacy.carets[1].range();
    let mut view = EditorView::new(egui::Id::new("smart-mixed"));
    let settings = EditorSettings {
        camel_humps_words: true,
        ..Default::default()
    };
    frame(&ctx, &mut state, &mut view, &settings, 0.0, vec![]);
    frame(
        &ctx,
        &mut state,
        &mut view,
        &settings,
        0.1,
        key(egui::Key::W, egui::Modifiers::CTRL),
    );
    assert_eq!(state.doc.slice(state.carets[0].range()), "Bar");
    assert_eq!(state.carets[1].range(), syntax_expected);
    let modifiers = egui::Modifiers {
        ctrl: true,
        command: true,
        shift: true,
        ..Default::default()
    };
    frame(
        &ctx,
        &mut state,
        &mut view,
        &settings,
        0.2,
        key(egui::Key::W, modifiers),
    );
    assert_eq!(state.carets, original);
    assert_eq!(state.doc.text(), source);
    assert!(!state.undo());
}

#[test]
fn end_and_shift_end_honor_effective_setting_and_keep_selection_anchor() {
    for enabled in [false, true] {
        let ctx = context();
        let mut state = EditorState::from_text("value  \n", Language::PlainText);
        state.set_carets(vec![Caret::at(TextSize::new(1))]);
        let mut view = EditorView::new(egui::Id::new(("smart-end", enabled)));
        let settings = EditorSettings {
            smart_home_end: enabled,
            ..Default::default()
        };
        frame(&ctx, &mut state, &mut view, &settings, 0.0, vec![]);
        frame(
            &ctx,
            &mut state,
            &mut view,
            &settings,
            0.1,
            key(egui::Key::End, egui::Modifiers::SHIFT),
        );
        assert_eq!(state.primary().sel.anchor, TextSize::new(1));
        assert_eq!(
            state.primary().head(),
            TextSize::new(if enabled { 5 } else { 7 })
        );
        frame(
            &ctx,
            &mut state,
            &mut view,
            &settings,
            0.2,
            key(egui::Key::End, egui::Modifiers::NONE),
        );
        assert_eq!(state.primary().head(), TextSize::new(7));
        assert_eq!(state.doc.text(), "value  \n");
        assert!(!state.undo());
    }
}

#[test]
fn actual_paste_reformat_is_opt_in_and_one_undo_redo_preserves_unrelated_bytes() {
    for mode in [
        rustpy_settings::ReformatOnPaste::None,
        rustpy_settings::ReformatOnPaste::IndentBlock,
        rustpy_settings::ReformatOnPaste::Reformat,
    ] {
        let ctx = context();
        let source = "untidy=0\nvalue = old\n";
        let mut state = EditorState::from_text(source, Language::Python);
        state.set_carets(vec![Caret::new(TextSize::new(17), TextSize::new(20))]);
        let mut view = EditorView::new(egui::Id::new(("smart-paste", format!("{mode:?}"))));
        let settings = EditorSettings {
            reformat_on_paste: mode,
            ..Default::default()
        };
        frame(&ctx, &mut state, &mut view, &settings, 0.0, vec![]);
        frame(
            &ctx,
            &mut state,
            &mut view,
            &settings,
            0.1,
            vec![egui::Event::Paste("1+2".into())],
        );
        let expected = if mode == rustpy_settings::ReformatOnPaste::Reformat {
            "untidy=0\nvalue = 1 + 2\n"
        } else {
            "untidy=0\nvalue = 1+2\n"
        };
        assert_eq!(state.doc.text(), expected);
        assert!(state.undo());
        assert_eq!(state.doc.text(), source);
        assert!(state.redo());
        assert_eq!(state.doc.text(), expected);
    }
}

#[test]
fn connected_multicaret_reformat_keeps_column_literal_and_readonly_paste_safe() {
    let settings = EditorSettings {
        reformat_on_paste: rustpy_settings::ReformatOnPaste::Reformat,
        ..Default::default()
    };
    let ctx = context();
    let source = "a = old\nb = old\n";
    let mut state = EditorState::from_text(source, Language::Python);
    state.set_carets(vec![
        Caret::new(TextSize::new(4), TextSize::new(7)),
        Caret::new(TextSize::new(12), TextSize::new(15)),
    ]);
    let mut view = EditorView::new(egui::Id::new("smart-distributed"));
    frame(&ctx, &mut state, &mut view, &settings, 0.0, vec![]);
    frame(
        &ctx,
        &mut state,
        &mut view,
        &settings,
        0.1,
        vec![egui::Event::Paste("1+2\n3+4".into())],
    );
    assert_eq!(state.doc.text(), "a = 1 + 2\nb = 3 + 4\n");
    assert!(state.undo());
    assert_eq!(state.doc.text(), source);
    state.set_block_selection(
        rustpy_core::Position::new(0, 4),
        rustpy_core::Position::new(1, 7),
    );
    frame(
        &ctx,
        &mut state,
        &mut view,
        &settings,
        0.2,
        vec![egui::Event::Paste("1+2".into())],
    );
    assert_eq!(state.doc.text(), "a = 1+2\nb = 1+2\n");
    let ctx = context();
    let mut literal = EditorState::from_text("message = \"old\"\n", Language::Python);
    literal.set_carets(vec![Caret::new(TextSize::new(11), TextSize::new(14))]);
    let mut view = EditorView::new(egui::Id::new("smart-literal"));
    frame(&ctx, &mut literal, &mut view, &settings, 0.0, vec![]);
    frame(
        &ctx,
        &mut literal,
        &mut view,
        &settings,
        0.1,
        vec![egui::Event::Paste("1+2".into())],
    );
    assert_eq!(literal.doc.text(), "message = \"1+2\"\n");
    literal.doc.set_read_only(true);
    let before = literal.doc.text();
    let version = literal.doc.version();
    frame(
        &ctx,
        &mut literal,
        &mut view,
        &settings,
        0.2,
        vec![egui::Event::Paste("changed".into())],
    );
    assert_eq!(literal.doc.text(), before);
    assert_eq!(literal.doc.version(), version);
}
