//! Real keymap dispatch, editor ownership, and navigation outcomes.
use super::*;
use crate::editor::EditOptions;

fn frame(
    app: &mut IdeApp,
    ctx: &egui::Context,
    time: f64,
    mods: egui::Modifiers,
    key: Option<egui::Key>,
    text: Option<&str>,
) {
    let mut events = vec![egui::Event::ModifiersChanged(mods)];
    if let Some(key) = key {
        for pressed in [true, false] {
            events.push(egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed,
                repeat: false,
                modifiers: mods,
            });
        }
    }
    if let Some(text) = text {
        events.push(egui::Event::Text(text.into()));
    }
    ctx.run_ui(
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
            egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
            app.observe_navigation();
            app.nav_popups(ui.ctx());
        },
    )
    .drop_without_applying_deltas();
}
fn app() -> (IdeApp, egui::Context) {
    let app = IdeApp::new(Settings::default(), None);
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    (app, ctx)
}
#[test]
fn actual_ctrl_e_keyboard_can_show_tool_without_toggling_it_off_or_editing_host() {
    let (mut app, ctx) = app();
    app.push_tab(crate::editor::EditorState::from_text(
        "π🙂",
        crate::editor::Language::PlainText,
    ));
    let version = app.tabs[0].state.doc.version();
    app.layout.left_visible = false;
    frame(&mut app, &ctx, 0.0, egui::Modifiers::NONE, None, None);
    for base in [0.1, 0.6] {
        frame(
            &mut app,
            &ctx,
            base,
            egui::Modifiers::CTRL,
            Some(egui::Key::E),
            None,
        );
        assert!(app.nav.recent_files.visible);
        frame(
            &mut app,
            &ctx,
            base + 0.1,
            egui::Modifiers::NONE,
            Some(egui::Key::ArrowLeft),
            None,
        );
        frame(
            &mut app,
            &ctx,
            base + 0.2,
            egui::Modifiers::NONE,
            Some(egui::Key::ArrowDown),
            None,
        );
        frame(
            &mut app,
            &ctx,
            base + 0.3,
            egui::Modifiers::NONE,
            Some(egui::Key::Enter),
            None,
        );
        assert!(!app.nav.recent_files.visible);
        assert_eq!(app.left_tool, LeftTool::Bookmarks);
        assert!(app.layout.left_visible);
    }
    assert_eq!(app.tabs[0].state.doc.text(), "π🙂");
    assert_eq!(app.tabs[0].state.doc.version(), version);
    assert!(!app.tabs[0].state.doc.can_undo());
}
#[test]
fn actual_ctrl_e_filters_edited_files_and_speed_search_then_enters_exact_file() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("α.py");
    let b = dir.path().join("β.py");
    std::fs::write(&a, "alpha\n").unwrap();
    std::fs::write(&b, "beta\n").unwrap();
    let (mut app, ctx) = app();
    app.open_file(&a);
    app.observe_navigation();
    app.open_file(&b);
    app.observe_navigation();
    let i = app.active.unwrap();
    app.tabs[i].state.insert_text("π", &EditOptions::default());
    app.observe_navigation();
    let version = app.tabs[i].state.doc.version();
    let content = app.tabs[i].state.doc.text();
    frame(&mut app, &ctx, 0.0, egui::Modifiers::NONE, None, None);
    frame(
        &mut app,
        &ctx,
        0.1,
        egui::Modifiers::CTRL,
        Some(egui::Key::E),
        None,
    );
    assert_eq!(app.nav.recent_files.selected_path(), Some(a.clone()));
    frame(
        &mut app,
        &ctx,
        0.2,
        egui::Modifiers::CTRL,
        Some(egui::Key::E),
        None,
    );
    assert_eq!(app.nav.recent_files.shown_paths(), vec![b.clone()]);
    frame(&mut app, &ctx, 0.3, egui::Modifiers::NONE, None, Some("β"));
    assert_eq!(app.nav.recent_files.shown_paths(), vec![b.clone()]);
    frame(
        &mut app,
        &ctx,
        0.4,
        egui::Modifiers::NONE,
        Some(egui::Key::Enter),
        None,
    );
    assert_eq!(app.tabs[app.active.unwrap()].state.path.as_ref(), Some(&b));
    assert_eq!(app.tabs[i].state.doc.text(), content);
    assert_eq!(app.tabs[i].state.doc.version(), version);
    assert_eq!(std::fs::read_to_string(&a).unwrap(), "alpha\n");
    assert_eq!(std::fs::read_to_string(&b).unwrap(), "beta\n");
}
#[test]
fn actual_ctrl_shift_e_shows_unsaved_unicode_snippet_and_changed_only_navigates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("places.py");
    let original = (0..20)
        .map(|i| format!("value_{i} = {i}\n"))
        .collect::<String>();
    std::fs::write(&path, &original).unwrap();
    let (mut app, ctx) = app();
    app.open_file(&path);
    app.observe_navigation();
    let i = app.active.unwrap();
    app.tabs[i].state.go_to_line(4, None);
    app.tabs[i]
        .state
        .insert_text("π🙂", &EditOptions::default());
    app.observe_navigation();
    app.mark_nav_jump();
    app.tabs[i].state.go_to_line(12, None);
    app.observe_navigation();
    let version = app.tabs[i].state.doc.version();
    frame(&mut app, &ctx, 0.0, egui::Modifiers::NONE, None, None);
    let mods = egui::Modifiers {
        ctrl: true,
        shift: true,
        ..Default::default()
    };
    frame(&mut app, &ctx, 0.1, mods, Some(egui::Key::E), None);
    assert!(app.nav.recent_locations.visible);
    assert!(app
        .nav
        .recent_locations
        .shown_titles()
        .iter()
        .any(|t| t.ends_with(":4")));
    frame(&mut app, &ctx, 0.2, mods, Some(egui::Key::E), None);
    assert!(app.nav.recent_locations.changed_only);
    frame(
        &mut app,
        &ctx,
        0.3,
        egui::Modifiers::NONE,
        None,
        Some("π🙂"),
    );
    assert_eq!(
        app.nav.recent_locations.shown_titles(),
        vec!["places.py:4".to_string()]
    );
    frame(
        &mut app,
        &ctx,
        0.4,
        egui::Modifiers::NONE,
        Some(egui::Key::Enter),
        None,
    );
    assert_eq!(app.tabs[i].state.primary_position().line, 3);
    assert_eq!(app.tabs[i].state.doc.version(), version);
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
}

#[test]
fn recent_tool_enter_does_not_activate_the_python_gutter_run_menu() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("β.py");
    let original = "value_0 = 0\nvalue_1 = 1\n";
    std::fs::write(&path, original).unwrap();
    let (mut app, ctx) = app();
    app.open_file(&path);
    let i = app.active.unwrap();
    assert_eq!(app.run_lines(i), vec![0]);
    let run_id = app.tabs[i].view.id.with(("run", 0usize));
    let popup_id = run_id.with("popup");
    let version = app.tabs[i].state.doc.version();
    let carets = app.tabs[i].state.carets.clone();
    frame(&mut app, &ctx, 0.0, egui::Modifiers::NONE, None, None);
    let mut focus_trace = vec![ctx.memory(|m| m.focused())];
    for (time, modifiers, key) in [
        (0.1, egui::Modifiers::CTRL, egui::Key::Home),
        (0.2, egui::Modifiers::CTRL, egui::Key::E),
        (0.3, egui::Modifiers::NONE, egui::Key::ArrowLeft),
        (0.4, egui::Modifiers::NONE, egui::Key::ArrowDown),
    ] {
        frame(&mut app, &ctx, time, modifiers, Some(key), None);
        focus_trace.push(ctx.memory(|m| m.focused()));
        assert!(!egui::Popup::is_id_open(&ctx, popup_id));
    }
    assert!(app.nav.recent_files.visible);
    assert!(!app.editor_keyboard());
    frame(
        &mut app,
        &ctx,
        0.5,
        egui::Modifiers::NONE,
        Some(egui::Key::Enter),
        None,
    );
    focus_trace.push(ctx.memory(|m| m.focused()));
    assert!(!app.nav.recent_files.visible);
    assert_eq!(app.left_tool, LeftTool::Bookmarks);
    assert!(app.layout.left_visible);
    assert!(
        !egui::Popup::is_id_open(&ctx, popup_id),
        "Recent Enter opened gutter run menu: run_id={run_id:?}, focus={focus_trace:?}"
    );
    assert_eq!(app.tabs[i].state.doc.text(), original);
    assert_eq!(app.tabs[i].state.doc.version(), version);
    assert_eq!(app.tabs[i].state.carets, carets);
    assert!(!app.tabs[i].state.doc.can_undo());
    assert_eq!(std::fs::read_to_string(path).unwrap(), original);
}
