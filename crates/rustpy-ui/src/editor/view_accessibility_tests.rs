//! Actual source-editor AccessKit tree, using the real keyboard view route.
use super::super::Language;
use super::*;
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    ctx
}
fn frame(
    ctx: &egui::Context,
    state: &mut EditorState,
    view: &mut EditorView,
    time: f64,
    modifiers: egui::Modifiers,
    mut events: Vec<egui::Event>,
) -> egui::accesskit::TreeUpdate {
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
    let update = output
        .platform_output
        .accesskit_update
        .clone()
        .expect("enabled AccessKit");
    output.drop_without_applying_deltas();
    update
}

#[test]
fn focused_unicode_editor_announces_real_keyboard_selection_without_editing() {
    let ctx = context();
    let mut state = EditorState::from_text("α😀x", Language::Python);
    let mut view = EditorView::new(egui::Id::new("accessible-editor"));
    frame(&ctx, &mut state, &mut view, 0.0, Default::default(), vec![]);
    let mods = egui::Modifiers {
        shift: true,
        ..Default::default()
    };
    let update = frame(
        &ctx,
        &mut state,
        &mut view,
        0.1,
        mods,
        vec![egui::Event::Key {
            key: egui::Key::ArrowRight,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: mods,
        }],
    );
    let node = update
        .nodes
        .iter()
        .find(|(id, _)| *id == view.id.with("announcement").accesskit_id())
        .expect("live region");
    let editor = update
        .nodes
        .iter()
        .find(|(id, _)| *id == view.id.accesskit_id())
        .expect("actual editor owner");
    assert_eq!(editor.1.role(), egui::accesskit::Role::Group);
    assert!(editor.1.children().contains(&node.0));
    assert_eq!(node.1.role(), egui::accesskit::Role::Status);
    assert_eq!(node.1.live(), Some(egui::accesskit::Live::Polite));
    assert!(node.1.label().unwrap().contains("Line 1, column 2"));
    assert!(node
        .1
        .label()
        .unwrap()
        .contains("Selection from line 1, column 1"));
    assert_eq!(state.doc.text(), "α😀x");
    assert!(!state.doc.can_undo());
}
#[test]
fn huge_unicode_line_announcement_is_bounded() {
    let state = EditorState::from_text(&"😀".repeat(100_000), Language::Python);
    let text = super::super::accessibility::status(&state);
    assert!(text.len() < 1000);
    assert!(text.contains("😀"));
}
