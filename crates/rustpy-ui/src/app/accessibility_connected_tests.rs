//! Real popup AccessKit announcements, retaining the ordinary App keyboard route.
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
    ctx.enable_accesskit();
    frame(&mut app, &ctx, 0.0, vec![]);
    frame(&mut app, &ctx, 0.02, vec![]);
    (root, app, ctx)
}
fn frame(
    app: &mut IdeApp,
    ctx: &egui::Context,
    time: f64,
    events: Vec<egui::Event>,
) -> egui::accesskit::TreeUpdate {
    let output = ctx.run_ui(
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
    );
    let update = output
        .platform_output
        .accesskit_update
        .clone()
        .expect("enabled AccessKit");
    output.drop_without_applying_deltas();
    update
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
fn announcement(update: &egui::accesskit::TreeUpdate, prefix: &str) -> String {
    update
        .nodes
        .iter()
        .map(|(_, n)| n)
        .find(|n| {
            n.role() == egui::accesskit::Role::Status
                && n.label().is_some_and(|s| s.starts_with(prefix))
        })
        .expect("actual popup live region")
        .label()
        .unwrap()
        .to_owned()
}
#[test]
fn typing_and_ctrl_p_cycle_actual_signature_announcement_without_editing() {
    let (_root, mut app, ctx) = fixture();
    enter_call(&mut app, &ctx);
    let before = app.tabs[app.active.unwrap()].state.doc.text();
    let update = frame(
        &mut app,
        &ctx,
        0.06,
        key(egui::Key::P, egui::Modifiers::CTRL),
    );
    let text = announcement(&update, "Signature ");
    assert!(text.contains("of 2"));
    assert!(text.contains("Current parameter: value:"));
    assert_eq!(app.tabs[app.active.unwrap()].state.doc.text(), before);
}
#[test]
fn painter_completion_rows_expose_selected_name_and_detail() {
    let (_root, mut app, ctx) = fixture();
    app.completion.items = vec![CompletionItem {
        label: "π_value".into(),
        kind: CompletionKind::Variable,
        detail: "int".into(),
        insert: "π_value".into(),
        caret_offset: None,
        priority: 100,
    }];
    app.completion.selected = 0;
    let before = app.tabs[app.active.unwrap()].state.doc.text();
    let mut update: Option<egui::accesskit::TreeUpdate> = None;
    for time in [0.1, 0.2] {
        let output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 650.0))),
                ..Default::default()
            },
            |ui| {
                app.draw_completion(
                    ui,
                    Rect::from_min_size(Pos2::new(100.0, 100.0), Vec2::new(10.0, 20.0)),
                )
            },
        );
        let next = output.platform_output.accesskit_update.clone().unwrap();
        if let Some(all) = &mut update {
            all.nodes.extend(next.nodes);
        } else {
            update = Some(next);
        }
        output.drop_without_applying_deltas();
    }
    let update = update.unwrap();
    assert_eq!(
        announcement(&update, "Completion "),
        "Completion 1 of 1: π_value, int. Up and Down to select; Enter to insert; Escape to close"
    );
    assert!(update
        .nodes
        .iter()
        .any(|(_, n)| n.label() == Some("π_value, int")
            && n.toggled() == Some(egui::accesskit::Toggled::True)));
    assert_eq!(app.tabs[app.active.unwrap()].state.doc.text(), before);
}

#[test]
fn actual_choice_keys_announce_filtered_selection_and_stop_after_escape() {
    let (_root, mut app, ctx) = fixture();
    app.choice = Some(ChoicePopup::new(
        ChoiceKind::Surround,
        "Surround With".into(),
        vec![
            ("if".into(), "Conditional π".into()),
            ("while".into(), "Loop 😀".into()),
        ],
    ));
    let before = app.tabs[app.active.unwrap()].state.doc.text();
    let version = app.tabs[app.active.unwrap()].state.doc.version();
    let paint = |app: &mut IdeApp, time, events| {
        let output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                events,
                focused: true,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 650.0))),
                ..Default::default()
            },
            |ui| {
                app.global_shortcuts(ui);
                app.draw_choice_popup(ui.ctx());
                app.dialogs(ui.ctx());
            },
        );
        let update = output.platform_output.accesskit_update.clone().unwrap();
        output.drop_without_applying_deltas();
        update
    };
    paint(&mut app, 0.1, vec![]);
    paint(&mut app, 0.2, vec![]);
    let update = paint(
        &mut app,
        0.3,
        key(egui::Key::ArrowDown, egui::Modifiers::NONE),
    );
    assert_eq!(app.choice.as_ref().unwrap().selected, 1);
    assert!(announcement(&update, "Surround With.").contains("Choice 2 of 2: while, Loop 😀"));
    assert!(update
        .nodes
        .iter()
        .any(|(_, n)| n.label() == Some("while, Loop 😀") && n.is_selected() == Some(true)));
    let update = paint(&mut app, 0.4, vec![egui::Event::Text("if".into())]);
    assert!(announcement(&update, "Surround With.").contains("Choice 1 of 1: if, Conditional π"));
    let update = paint(&mut app, 0.5, key(egui::Key::Escape, egui::Modifiers::NONE));
    assert!(app.choice.is_none());
    assert!(!update
        .nodes
        .iter()
        .any(|(_, n)| n.role() == egui::accesskit::Role::Status
            && n.label().is_some_and(|s| s.starts_with("Surround With."))));
    assert_eq!(app.tabs[app.active.unwrap()].state.doc.text(), before);
    assert_eq!(app.tabs[app.active.unwrap()].state.doc.version(), version);
}

#[test]
fn choice_does_not_announce_behind_settings() {
    let (_root, mut app, ctx) = fixture();
    app.choice = Some(ChoicePopup::new(
        ChoiceKind::Surround,
        "Surround With".into(),
        vec![("if".into(), "Conditional".into())],
    ));
    app.handle_action(Action::Settings, &ctx);
    assert!(
        app.choice.is_some(),
        "suppression is tested with the underlying chooser retained"
    );
    let output = ctx.run_ui(
        egui::RawInput {
            time: Some(0.2),
            focused: true,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 650.0))),
            ..Default::default()
        },
        |ui| {
            app.draw_choice_popup(ui.ctx());
            app.dialogs(ui.ctx());
        },
    );
    assert!(!output
        .platform_output
        .accesskit_update
        .as_ref()
        .unwrap()
        .nodes
        .iter()
        .any(|(_, n)| n.role() == egui::accesskit::Role::Status
            && n.label().is_some_and(|s| s.starts_with("Surround With."))));
    output.drop_without_applying_deltas();
}
