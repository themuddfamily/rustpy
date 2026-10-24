use super::*;
use crate::editor::{EditorState, Language};

fn fixture() -> (IdeApp, egui::Context, Vec<egui::Id>) {
    let mut app = IdeApp::new(Settings::default(), None);
    app.push_tab(EditorState::from_text("first π", Language::PlainText));
    app.push_tab(EditorState::from_text("second 🙂", Language::PlainText));
    let mut saved = EditorState::from_text("saved", Language::PlainText);
    saved.path = Some(PathBuf::from("/switcher-fixture/saved.txt"));
    app.push_tab(saved);
    app.record_recent(&PathBuf::from("/switcher-fixture/saved.txt"));
    let ids = app.tabs.iter().map(|t| t.view.id).collect();
    app.active = Some(2);
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    (app, ctx, ids)
}

fn frame(
    app: &mut IdeApp,
    ctx: &egui::Context,
    time: f64,
    modifiers: egui::Modifiers,
    key: Option<egui::Key>,
) {
    let mut events = vec![egui::Event::ModifiersChanged(modifiers)];
    if let Some(key) = key {
        events.push(egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        });
        events.push(egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: false,
            repeat: false,
            modifiers,
        });
    }
    let output = ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            ..Default::default()
        },
        |ui| {
            app.global_shortcuts(ui);
            app.nav_popups(ui.ctx());
        },
    );
    output.drop_without_applying_deltas();
}

#[test]
fn ctrl_tab_release_can_activate_each_distinct_unsaved_editor() {
    let (mut app, ctx, ids) = fixture();
    frame(&mut app, &ctx, 0.0, egui::Modifiers::NONE, None);
    frame(
        &mut app,
        &ctx,
        0.1,
        egui::Modifiers::CTRL,
        Some(egui::Key::Tab),
    );
    assert_eq!(
        app.nav.switcher.selection(),
        Some(PopupOutcome::Editor(ids[0]))
    );
    frame(&mut app, &ctx, 0.2, egui::Modifiers::NONE, None);
    assert_eq!(app.tabs[app.active.unwrap()].view.id, ids[0]);
    assert_eq!(app.tabs[app.active.unwrap()].state.doc.text(), "first π");
    frame(
        &mut app,
        &ctx,
        0.3,
        egui::Modifiers::CTRL,
        Some(egui::Key::Tab),
    );
    // Saved MRU still precedes unvisited editors.
    assert_eq!(
        app.nav.switcher.selection(),
        Some(PopupOutcome::Editor(ids[2]))
    );
    frame(
        &mut app,
        &ctx,
        0.4,
        egui::Modifiers::CTRL,
        Some(egui::Key::Tab),
    );
    assert_eq!(
        app.nav.switcher.selection(),
        Some(PopupOutcome::Editor(ids[1]))
    );
    frame(&mut app, &ctx, 0.5, egui::Modifiers::NONE, None);
    assert_eq!(app.tabs[app.active.unwrap()].view.id, ids[1]);
    assert_eq!(app.tabs.len(), 3);
}

#[test]
fn captured_editor_survives_reorder_and_closed_target_does_not_alias_index() {
    let (mut app, ctx, ids) = fixture();
    frame(&mut app, &ctx, 0.0, egui::Modifiers::NONE, None);
    frame(
        &mut app,
        &ctx,
        0.1,
        egui::Modifiers::CTRL,
        Some(egui::Key::Tab),
    );
    app.tabs.swap(0, 1);
    frame(&mut app, &ctx, 0.2, egui::Modifiers::NONE, None);
    assert_eq!(app.tabs[app.active.unwrap()].view.id, ids[0]);
    frame(
        &mut app,
        &ctx,
        0.3,
        egui::Modifiers::CTRL,
        Some(egui::Key::Tab),
    );
    let Some(PopupOutcome::Editor(target)) = app.nav.switcher.selection() else {
        panic!("editor selection required")
    };
    let index = app.tabs.iter().position(|t| t.view.id == target).unwrap();
    app.close_tab(index);
    let retained = app.active.map(|i| app.tabs[i].view.id);
    frame(&mut app, &ctx, 0.4, egui::Modifiers::NONE, None);
    assert_eq!(app.active.map(|i| app.tabs[i].view.id), retained);
    assert_eq!(app.tabs.len(), 2);
}

#[test]
fn switcher_cancel_preserves_editor_and_tool_column_release_opens_project() {
    let (mut app, ctx, ids) = fixture();
    frame(&mut app, &ctx, 0.0, egui::Modifiers::NONE, None);
    frame(
        &mut app,
        &ctx,
        0.1,
        egui::Modifiers::CTRL,
        Some(egui::Key::Tab),
    );
    frame(
        &mut app,
        &ctx,
        0.2,
        egui::Modifiers::CTRL,
        Some(egui::Key::Escape),
    );
    assert!(!app.nav.switcher.visible);
    assert_eq!(app.tabs[app.active.unwrap()].view.id, ids[2]);
    frame(
        &mut app,
        &ctx,
        0.3,
        egui::Modifiers::CTRL,
        Some(egui::Key::Tab),
    );
    frame(
        &mut app,
        &ctx,
        0.4,
        egui::Modifiers::CTRL,
        Some(egui::Key::ArrowLeft),
    );
    frame(
        &mut app,
        &ctx,
        0.45,
        egui::Modifiers::CTRL,
        Some(egui::Key::ArrowUp),
    );
    frame(&mut app, &ctx, 0.5, egui::Modifiers::NONE, None);
    assert!(app.layout.left_visible);
    assert!(matches!(app.left_tool, LeftTool::Project));
    assert_eq!(app.tabs[app.active.unwrap()].view.id, ids[2]);
    assert!(app.tabs.iter().all(|t| !t.state.doc.can_undo()));
}
