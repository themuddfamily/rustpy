//! Real app dispatch uses the same editor actions exposed by Edit/Lines.
use super::*;
use crate::actions::{KeyChord, Keymap};
use crate::editor::view::{self, ViewConfig};

#[test]
fn full_transform_row_dispatches_through_active_ide_tab_and_undo_redo() {
    assert_eq!(
        Keymap::default_keymap().lookup(KeyChord::parse("Ctrl+Shift+U").unwrap()),
        Some(Action::ToggleCase)
    );
    let cases = [
        (Action::ToggleCase, "aß", "ASS"),
        (Action::SortLines, "b\na\n", "a\nb\n"),
        (Action::ReverseLines, "a\nb\n", "b\na\n"),
        (Action::RemoveDuplicateLines, "a\na\n\n\n", "a\n\n"),
        (Action::TrimTrailingWhitespace, "a \t\n", "a\n"),
        (Action::ConvertIndentsToSpaces, "\ta\n", "    a\n"),
        (Action::ConvertIndentsToTabs, "    a\n", "\ta\n"),
    ];
    for (action, original, expected) in cases {
        let mut app = IdeApp::new(Settings::default(), None);
        app.push_tab(EditorState::from_text(original, Language::PlainText));
        let before = app.tabs[0].state.selections();
        assert!(app.dispatch_editor_action(action));
        assert_eq!(app.tabs[0].state.doc.text(), expected, "{action:?}");
        let after = app.tabs[0].state.selections();
        assert!(app.dispatch_editor_action(Action::Undo));
        assert_eq!(app.tabs[0].state.doc.text(), original, "{action:?}");
        assert_eq!(app.tabs[0].state.selections(), before, "{action:?}");
        assert!(app.dispatch_editor_action(Action::Redo));
        assert_eq!(app.tabs[0].state.doc.text(), expected, "{action:?}");
        assert_eq!(app.tabs[0].state.selections(), after, "{action:?}");
    }
}

fn frame(app: &mut IdeApp, ctx: &egui::Context, time: f64, events: Vec<egui::Event>) {
    ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 720.0),
            )),
            events,
            ..Default::default()
        },
        |ui| {
            // Keep the real ordering: shortcuts can take keyboard ownership
            // before the actual editor decides whether to process its input.
            app.global_shortcuts(ui);
            egui::CentralPanel::default().show(ui, |ui| {
                let keyboard = app.editor_keyboard();
                let request_focus = std::mem::take(&mut app.focus_editor);
                let opts = EditOptions::default();
                let cfg = ViewConfig {
                    scheme: &app.scheme,
                    theme: &app.theme,
                    settings: &app.settings.editor,
                    opts: &opts,
                    keymap: &app.keymap,
                    font_size: 14.0,
                    search_highlights: &[],
                    request_focus,
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
                let tab = &mut app.tabs[app.active.unwrap()];
                let actions = view::show(ui, &mut tab.state, &mut tab.view, &cfg);
                for action in actions {
                    if let view::ViewEvent::Action(action) = action {
                        app.handle_action(action, ui.ctx());
                    }
                }
            });
        },
    )
    .drop_without_applying_deltas();
}

#[test]
fn ctrl_shift_u_and_ctrl_z_reach_the_real_focused_editor() {
    let mut app = IdeApp::new(Settings::default(), None);
    app.push_tab(EditorState::from_text("straße", Language::PlainText));
    app.tabs[0]
        .state
        .move_primary_to(rustpy_core::Position::new(0, 1));
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, 0.0, vec![]);
    assert!(app.tabs[0].view.focused);
    let before = app.tabs[0].state.selections();
    let modifiers = egui::Modifiers {
        ctrl: true,
        shift: true,
        ..Default::default()
    };
    frame(
        &mut app,
        &ctx,
        0.1,
        vec![
            egui::Event::ModifiersChanged(modifiers),
            egui::Event::Key {
                key: egui::Key::U,
                physical_key: Some(egui::Key::U),
                pressed: true,
                repeat: false,
                modifiers,
            },
        ],
    );
    assert_eq!(app.tabs[0].state.doc.text(), "STRASSE");
    let modifiers = egui::Modifiers {
        ctrl: true,
        ..Default::default()
    };
    frame(
        &mut app,
        &ctx,
        0.2,
        vec![
            egui::Event::ModifiersChanged(modifiers),
            egui::Event::Key {
                key: egui::Key::Z,
                physical_key: Some(egui::Key::Z),
                pressed: true,
                repeat: false,
                modifiers,
            },
        ],
    );
    assert_eq!(app.tabs[0].state.doc.text(), "straße");
    assert_eq!(app.tabs[0].state.selections(), before);
}
