//! Real Run process -> App console output/search accessibility.
use super::*;
use std::time::{Duration, Instant};

struct Fixture {
    app: IdeApp,
    ctx: egui::Context,
    _root: tempfile::TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for session in &mut self.app.run_panel.sessions {
            session.stop();
        }
    }
}
fn fixture() -> Option<Fixture> {
    let python = ["python3", "python"]
        .into_iter()
        .find_map(|name| which::which(name).ok())?;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("π.py");
    std::fs::write(&path, "π = 1\n").unwrap();
    let mut settings = Settings::default();
    settings.trust_project(root.path());
    let mut app = IdeApp::new(settings, Some(root.path().into()));
    app.open_file(&path);
    app.bottom = Some(BottomTool::Run);
    app.theme = UiTheme::high_contrast();
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    let mut fixture = Fixture {
        app,
        ctx,
        _root: root,
    };
    let script = fixture._root.path().join("output.py");
    std::fs::write(
        &script,
        "import sys,time\nsys.stdout.write('π😀\\nπ🙂')\nsys.stdout.flush()\ntime.sleep(60)\n",
    )
    .unwrap();
    let session = rustpy_run::RunSession::spawn_command(
        188,
        "Accessible output",
        &python,
        &["-u".into(), script.to_string_lossy().into_owned()],
        fixture._root.path(),
        &std::collections::BTreeMap::new(),
    );
    assert!(session.is_running(), "real owned output process");
    fixture.app.run_panel.add_session(session);
    let until = Instant::now() + Duration::from_secs(10);
    while !fixture.app.run_panel.sessions[0].text().contains("π🙂") {
        fixture.app.run_panel.sessions[0].poll();
        assert!(Instant::now() < until, "real process Unicode output");
        std::thread::sleep(Duration::from_millis(10));
    }
    Some(fixture)
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
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1100.0, 800.0))),
            ..Default::default()
        },
        |ui| {
            app.global_shortcuts(ui);
            egui::CentralPanel::default().show(ui, |ui| app.bottom_panel(ui));
            app.dialogs(ui.ctx());
        },
    );
    let update = output.platform_output.accesskit_update.clone().unwrap();
    output.drop_without_applying_deltas();
    update
}
#[test]
fn actual_run_console_tab_focus_and_search_announce_current_unicode_results() {
    let Some(mut fixture) = fixture() else {
        return;
    };
    let ctx = fixture.ctx.clone();
    let app = &mut fixture.app;
    frame(app, &ctx, 0.0, vec![]);
    frame(app, &ctx, 0.1, vec![]);
    let mut focused = false;
    for n in 0..80 {
        let update = frame(
            app,
            &ctx,
            0.2 + n as f64 * 0.1,
            key(egui::Key::Tab, egui::Modifiers::NONE),
        );
        if let Some((id, node)) = update.nodes.iter().find(|(id, node)| {
            ctx.memory(|m| m.focused())
                .is_some_and(|focus| focus.accesskit_id() == *id)
                && node.label() == Some("Run console output")
        }) {
            assert_eq!(node.role(), egui::accesskit::Role::Group);
            let status = update
                .nodes
                .iter()
                .find(|(_, node)| {
                    node.role() == egui::accesskit::Role::Status
                        && node
                            .label()
                            .is_some_and(|text| text.starts_with("Console output."))
                })
                .unwrap();
            assert!(status.1.label().unwrap().contains("π🙂"));
            assert!(node.children().contains(&status.0));
            assert_eq!(ctx.memory(|m| m.focused()).unwrap().accesskit_id(), *id);
            focused = true;
            break;
        }
    }
    assert!(focused, "real Tab reaches painted output");
    frame(
        app,
        &ctx,
        10.0,
        key(
            egui::Key::F,
            egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
        ),
    );
    assert!(
        !app.find.visible,
        "Run console retains CtrlF rather than opening source Find"
    );
    let first = frame(app, &ctx, 10.1, vec![egui::Event::Text("π".into())]);
    assert!(first
        .nodes
        .iter()
        .any(|(_, node)| node.role() == egui::accesskit::Role::Status
            && node.label().is_some_and(
                |text| text.contains("Console search. Result 1 of 2. Line 2, column 1. π😀")
            )));
    let next = frame(
        app,
        &ctx,
        10.2,
        key(egui::Key::Enter, egui::Modifiers::NONE),
    );
    assert!(next
        .nodes
        .iter()
        .any(|(_, node)| node.role() == egui::accesskit::Role::Status
            && node.label().is_some_and(
                |text| text.contains("Console search. Result 2 of 2. Line 3, column 1. π🙂")
            )));
    let toolbar = frame(app, &ctx, 10.3, key(egui::Key::Tab, egui::Modifiers::NONE));
    assert!(
        !toolbar
            .nodes
            .iter()
            .any(|(_, node)| node.role() == egui::accesskit::Role::Status
                && node
                    .label()
                    .is_some_and(|text| text.starts_with("Console search.")
                        || text.starts_with("Console output."))),
        "toolbar owns focus, not output/search announcement"
    );
    app.handle_action(Action::Settings, &ctx);
    let settings = frame(app, &ctx, 10.4, vec![]);
    assert!(!settings
        .nodes
        .iter()
        .any(|(_, node)| node.role() == egui::accesskit::Role::Status
            && node
                .label()
                .is_some_and(|text| text.starts_with("Console search.")
                    || text.starts_with("Console output."))));
    assert_eq!(app.tabs[0].state.doc.text(), "π = 1\n");
    assert!(!app.tabs[0].state.doc.can_undo());
}

#[test]
fn actual_run_output_keyboard_menu_exposes_copy_all_and_clear_without_running_source() {
    let Some(mut fixture) = fixture() else { return };
    let ctx = fixture.ctx.clone();
    let app = &mut fixture.app;
    frame(app, &ctx, 0.0, vec![]);
    frame(app, &ctx, 0.1, vec![]);
    let mut reached = false;
    for n in 0..80 {
        let update = frame(
            app,
            &ctx,
            0.2 + n as f64 * 0.1,
            key(egui::Key::Tab, egui::Modifiers::NONE),
        );
        reached = update.nodes.iter().any(|(id, node)| {
            node.label() == Some("Run console output")
                && ctx
                    .memory(|memory| memory.focused())
                    .is_some_and(|focus| focus.accesskit_id() == *id)
        });
        if reached {
            break;
        }
    }
    assert!(reached, "real Tab reaches the output menu owner");
    let sessions = app.run_panel.sessions.len();
    frame(app, &ctx, 10.0, key(egui::Key::F10, egui::Modifiers::SHIFT));
    let menu = frame(app, &ctx, 10.1, vec![]);
    assert!(egui::Popup::is_any_open(&ctx));
    for label in ["Copy All", "Select All Ctrl+A", "Clear All"] {
        assert!(
            menu.nodes
                .iter()
                .any(|(_, node)| node.label() == Some(label)),
            "{label}"
        );
    }
    assert_eq!(
        app.run_panel.sessions.len(),
        sessions,
        "ShiftF10 must not run source"
    );
    frame(
        app,
        &ctx,
        10.2,
        key(egui::Key::Escape, egui::Modifiers::NONE),
    );
    assert!(!egui::Popup::is_any_open(&ctx));
    assert_eq!(app.tabs[0].state.doc.text(), "π = 1\n");
    assert!(!app.tabs[0].state.doc.can_undo());
}
