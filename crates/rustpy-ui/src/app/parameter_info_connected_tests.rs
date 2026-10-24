//! Connected action, shortcut ownership and ordinary typing parameter-info routes.
use super::*;
const SOURCE: &str = "from typing import overload\n@overload\ndef f(value: str, count: int) -> str: ...\n@overload\ndef f(value: int, count: int) -> int: ...\ndef f(*args): ...\nf";
fn fixture() -> (tempfile::TempDir, IdeApp, egui::Context) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("fixture.py");
    std::fs::write(&path, SOURCE).unwrap();
    let mut settings = Settings::default();
    settings.completion.show_as_you_type = false;
    settings.trust_project(root.path());
    let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
    app.open_file(&path);
    app.tabs[app.active.unwrap()].state.move_doc_end(false);
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, 0.0, vec![]);
    frame(&mut app, &ctx, 0.02, vec![]);
    (root, app, ctx)
}
fn frame(app: &mut IdeApp, ctx: &egui::Context, time: f64, events: Vec<egui::Event>) {
    ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            focused: true,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 650.0))),
            ..Default::default()
        },
        |ui| {
            app.global_shortcuts(ui);
            app.dialogs(ui.ctx());
            egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
        },
    )
    .drop_without_applying_deltas();
}
fn key(key: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
    vec![
        egui::Event::ModifiersChanged(modifiers),
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        },
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: false,
            repeat: false,
            modifiers,
        },
    ]
}
fn enter_call(app: &mut IdeApp, ctx: &egui::Context) {
    frame(app, ctx, 0.04, vec![egui::Event::Text("(".into())]);
    assert!(
        app.param_info.is_some(),
        "ordinary EditorView typing must auto-open help"
    );
}
#[test]
fn actual_ctrl_p_action_and_global_shortcut_repeat_cycle_without_document_edits() {
    let (root, mut app, ctx) = fixture();
    enter_call(&mut app, &ctx);
    let i = app.active.unwrap();
    let text = app.tabs[i].state.doc.text();
    let version = app.tabs[i].state.doc.version();
    let caret = app.tabs[i].state.primary();
    let first = app.param_info.as_ref().unwrap().active_signature;
    app.handle_action(Action::ParameterInfo, &ctx);
    assert_eq!(
        app.param_info.as_ref().unwrap().active_signature,
        (first + 1) % 2
    );
    frame(
        &mut app,
        &ctx,
        0.06,
        key(egui::Key::P, egui::Modifiers::CTRL),
    );
    assert_eq!(app.param_info.as_ref().unwrap().active_signature, first);
    assert_eq!(app.tabs[i].state.doc.text(), text);
    assert_eq!(app.tabs[i].state.doc.version(), version);
    assert_eq!(app.tabs[i].state.primary(), caret);
    assert_eq!(
        std::fs::read_to_string(root.path().join("fixture.py")).unwrap(),
        SOURCE
    );
}
#[test]
fn actual_alt_overload_keys_respect_dialog_and_find_field_ownership() {
    let (_root, mut app, ctx) = fixture();
    enter_call(&mut app, &ctx);
    let first = app.param_info.as_ref().unwrap().active_signature;
    frame(
        &mut app,
        &ctx,
        0.06,
        key(egui::Key::ArrowDown, egui::Modifiers::ALT),
    );
    let selected = (first + 1) % 2;
    assert_eq!(app.param_info.as_ref().unwrap().active_signature, selected);
    app.dialog = Dialog::GoToLine(String::new());
    frame(
        &mut app,
        &ctx,
        0.08,
        key(egui::Key::ArrowUp, egui::Modifiers::ALT),
    );
    assert_eq!(app.param_info.as_ref().unwrap().active_signature, selected);
    app.dialog = Dialog::None;
    app.handle_action(Action::Find, &ctx);
    frame(&mut app, &ctx, 0.10, vec![]);
    frame(
        &mut app,
        &ctx,
        0.12,
        key(egui::Key::ArrowUp, egui::Modifiers::ALT),
    );
    assert_eq!(app.param_info.as_ref().unwrap().active_signature, selected);
    app.close_find_bar();
    app.handle_action(Action::JumpToNavigationBar, &ctx);
    frame(&mut app, &ctx, 0.14, vec![]);
    frame(
        &mut app,
        &ctx,
        0.16,
        key(egui::Key::ArrowUp, egui::Modifiers::ALT),
    );
    assert_eq!(app.param_info.as_ref().unwrap().active_signature, selected);
}
#[test]
fn actual_parenthesis_comma_typing_tracks_current_argument_and_manual_choice() {
    let (root, mut app, ctx) = fixture();
    enter_call(&mut app, &ctx);
    assert_eq!(app.param_info.as_ref().unwrap().active, Some(0));
    frame(
        &mut app,
        &ctx,
        0.06,
        key(egui::Key::ArrowDown, egui::Modifiers::ALT),
    );
    let selected = app.param_info.as_ref().unwrap().active_signature;
    frame(
        &mut app,
        &ctx,
        0.08,
        vec![
            egui::Event::ModifiersChanged(egui::Modifiers::NONE),
            egui::Event::Text("1,".into()),
        ],
    );
    assert_eq!(app.param_info.as_ref().unwrap().active_signature, selected);
    assert_eq!(app.param_info.as_ref().unwrap().active, Some(1));
    frame(&mut app, &ctx, 0.10, vec![egui::Event::Text(" 2".into())]);
    assert_eq!(app.param_info.as_ref().unwrap().active, Some(1));
    assert_eq!(app.param_info.as_ref().unwrap().active_signature, selected);
    assert!(app.tabs[app.active.unwrap()]
        .state
        .doc
        .text()
        .contains("f(1, 2"));
    assert_eq!(
        std::fs::read_to_string(root.path().join("fixture.py")).unwrap(),
        SOURCE
    );
}
