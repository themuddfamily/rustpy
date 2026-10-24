//! Actual PTY/App focus and announcement ownership; no shell input injection.
use super::*;
use std::time::{Duration, Instant};

struct Fixture {
    app: IdeApp,
    ctx: egui::Context,
    _root: tempfile::TempDir,
}
// Dropped PTYs publish process-global retirement, which parallel Quit tests
// observe as pending "Terminal cleanup" consent. Retire them before dropping so
// only already-complete owners reach the registry, then reap those.
impl Drop for Fixture {
    fn drop(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let owners: Vec<String> = self
            .app
            .terminal_panel
            .tabs
            .iter()
            .map(|tab| {
                let stamp = tab.terminal.lifecycle_stamp();
                stamp.split(':').take(3).collect::<Vec<_>>().join(":")
            })
            .collect();
        for tab in &mut self.app.terminal_panel.tabs {
            tab.terminal.kill();
        }
        for tab in &mut self.app.terminal_panel.tabs {
            while tab.terminal.is_alive() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        drop(std::mem::take(&mut self.app.terminal_panel.tabs));
        while rustpy_terminal::poll_retiring_terminals()
            .iter()
            .any(|stamp| owners.iter().any(|owner| stamp.starts_with(owner)))
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
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
    app.bottom = Some(BottomTool::Terminal);
    app.theme = UiTheme::high_contrast();
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    let mut fixture = Fixture {
        app,
        ctx,
        _root: root,
    };
    fixture
        .app
        .terminal_panel
        .open_command(
            "Unicode PTY",
            &python.to_string_lossy(),
            &[
                "-c".into(),
                "import sys,time; sys.stdout.write('π😀'); sys.stdout.flush(); time.sleep(60)"
                    .into(),
            ],
            fixture._root.path(),
            &[],
        )
        .unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    while !fixture.app.terminal_panel.tabs[0]
        .terminal
        .contents()
        .contains("π😀")
    {
        fixture.app.terminal_panel.poll();
        assert!(Instant::now() < until, "actual PTY output");
        std::thread::sleep(Duration::from_millis(10));
    }
    Some(fixture)
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
fn focus_controls() -> Vec<egui::Event> {
    let modifiers = egui::Modifiers::CTRL | egui::Modifiers::ALT;
    vec![
        egui::Event::ModifiersChanged(modifiers),
        egui::Event::Key {
            key: egui::Key::F6,
            physical_key: Some(egui::Key::F6),
            pressed: true,
            repeat: false,
            modifiers,
        },
        egui::Event::Key {
            key: egui::Key::F6,
            physical_key: Some(egui::Key::F6),
            pressed: false,
            repeat: false,
            modifiers,
        },
    ]
}
#[test]
#[cfg(unix)] // ConPTY echo and output timing differ from a Unix PTY
fn actual_terminal_group_announces_unicode_and_keyboard_returns_to_tool_controls() {
    let Some(mut fixture) = fixture() else {
        return;
    };
    let ctx = fixture.ctx.clone();
    let app = &mut fixture.app;
    frame(app, &ctx, 0.0, vec![]);
    let update = frame(app, &ctx, 0.1, vec![]);
    assert!(app.terminal_panel.focused);
    let (status_id, status) = update
        .nodes
        .iter()
        .find(|(_, node)| {
            node.role() == egui::accesskit::Role::Status
                && node
                    .label()
                    .is_some_and(|text| text.starts_with("Terminal Unicode PTY."))
        })
        .unwrap();
    assert!(status.label().unwrap().contains("π😀"));
    assert!(status.label().unwrap().contains("Ctrl Alt F6"));
    assert!(update
        .nodes
        .iter()
        .any(|(_, node)| node.role() == egui::accesskit::Role::Group
            && node.label() == Some("Terminal Unicode PTY")
            && node.children().contains(status_id)));
    let controls = frame(app, &ctx, 0.2, focus_controls());
    assert!(
        !app.terminal_panel.focused,
        "actual CtrlAltF6 escapes terminal's Tab lock without sending shell keys"
    );
    assert!(controls.nodes.iter().any(|(_, node)| node
        .label()
        .is_some_and(|text| text.starts_with("Terminal tool window controls. Press Tab"))));
    assert_eq!(app.tabs[0].state.doc.text(), "π = 1\n");
    assert!(!app.tabs[0].state.doc.can_undo());
}
#[test]
fn actual_settings_owner_suppresses_terminal_status_before_settings_paints() {
    let Some(mut fixture) = fixture() else {
        return;
    };
    let ctx = fixture.ctx.clone();
    let app = &mut fixture.app;
    frame(app, &ctx, 0.0, vec![]);
    frame(app, &ctx, 0.1, vec![]);
    assert!(app.terminal_panel.focused);
    app.handle_action(Action::Settings, &ctx);
    assert!(!matches!(app.dialog, Dialog::None));
    let update = frame(app, &ctx, 0.2, vec![]);
    assert!(!update
        .nodes
        .iter()
        .any(|(_, node)| node.role() == egui::accesskit::Role::Status
            && node
                .label()
                .is_some_and(|text| text.starts_with("Terminal Unicode PTY."))));
    assert_eq!(app.tabs[0].state.doc.text(), "π = 1\n");
    assert!(!app.tabs[0].state.doc.can_undo());
}

fn menu_key(key: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
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
#[test]
fn actual_terminal_keyboard_menu_exposes_select_all_and_preserves_source() {
    let Some(mut fixture) = fixture() else { return };
    let ctx = fixture.ctx.clone();
    let app = &mut fixture.app;
    frame(app, &ctx, 0.0, vec![]);
    frame(app, &ctx, 0.1, vec![]);
    assert!(app.terminal_panel.focused);
    let sessions = app.run_panel.sessions.len();
    frame(
        app,
        &ctx,
        0.2,
        menu_key(egui::Key::F10, egui::Modifiers::SHIFT),
    );
    let menu = frame(app, &ctx, 0.3, vec![]);
    assert!(egui::Popup::is_any_open(&ctx));
    for label in ["Paste Ctrl+Shift+V", "Select All"] {
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
        0.4,
        menu_key(egui::Key::Escape, egui::Modifiers::NONE),
    );
    assert!(!egui::Popup::is_any_open(&ctx));
    assert_eq!(app.tabs[0].state.doc.text(), "π = 1\n");
    assert!(!app.tabs[0].state.doc.can_undo());
}

/// Gives the shell time to echo anything that leaked into the PTY.
fn shell_echo(app: &mut IdeApp) -> String {
    std::thread::sleep(Duration::from_millis(150));
    app.terminal_panel.poll();
    app.terminal_panel.tabs[0].terminal.contents()
}
fn focused_terminal() -> Option<Fixture> {
    let mut fixture = fixture()?;
    let ctx = fixture.ctx.clone();
    frame(&mut fixture.app, &ctx, 0.0, vec![]);
    frame(&mut fixture.app, &ctx, 0.1, vec![]);
    assert!(fixture.app.terminal_panel.focused);
    Some(fixture)
}
#[test]
fn ide_shortcuts_not_overridden_by_terminal_reach_actions() {
    let Some(mut fixture) = focused_terminal() else {
        return;
    };
    let ctx = fixture.ctx.clone();
    let app = &mut fixture.app;
    // Ctrl+Shift+A (Find Action) is not in the override list.
    frame(
        app,
        &ctx,
        0.2,
        menu_key(egui::Key::A, egui::Modifiers::CTRL | egui::Modifiers::SHIFT),
    );
    assert!(
        app.everywhere.visible,
        "Find Action opens from the terminal"
    );
    assert_eq!(app.everywhere.tab, crate::search_ui::EverywhereTab::Actions);
    assert!(
        !shell_echo(app).contains("^A"),
        "nothing is typed into the shell"
    );
    app.everywhere.visible = false;
    // Ctrl+E (Recent Files) also goes to the IDE, not to the shell.
    app.terminal_panel.request_focus();
    frame(app, &ctx, 0.3, vec![]);
    frame(
        app,
        &ctx,
        0.4,
        menu_key(egui::Key::E, egui::Modifiers::CTRL),
    );
    assert!(
        app.nav.recent_files.visible,
        "Recent Files opens from the terminal"
    );
    assert!(!shell_echo(app).contains("^E"));
}
#[test]
fn overridden_shortcuts_stay_in_the_terminal() {
    let Some(mut fixture) = focused_terminal() else {
        return;
    };
    let ctx = fixture.ctx.clone();
    let app = &mut fixture.app;
    // Ctrl+R is overridden: the terminal's history search, not an IDE action.
    let sessions = app.run_panel.sessions.len();
    frame(
        app,
        &ctx,
        0.2,
        menu_key(egui::Key::R, egui::Modifiers::CTRL),
    );
    assert!(app.terminal_panel.focused);
    assert_eq!(app.run_panel.sessions.len(), sessions);
    assert!(!app.everywhere.visible);
}
#[test]
fn double_shift_and_alt_f12_work_from_the_terminal() {
    let Some(mut fixture) = focused_terminal() else {
        return;
    };
    let ctx = fixture.ctx.clone();
    let app = &mut fixture.app;
    let shift = |pressed| {
        let modifiers = if pressed {
            egui::Modifiers::SHIFT
        } else {
            egui::Modifiers::NONE
        };
        vec![
            egui::Event::ModifiersChanged(modifiers),
            egui::Event::Key {
                key: egui::Key::ShiftLeft,
                physical_key: Some(egui::Key::ShiftLeft),
                pressed,
                repeat: false,
                modifiers,
            },
        ]
    };
    frame(app, &ctx, 0.20, shift(true));
    frame(app, &ctx, 0.25, shift(false));
    frame(app, &ctx, 0.30, shift(true));
    frame(app, &ctx, 0.35, shift(false));
    assert!(
        app.everywhere.visible,
        "Double Shift opens Search Everywhere"
    );
    assert_eq!(app.everywhere.tab, crate::search_ui::EverywhereTab::All);
    app.everywhere.visible = false;
    app.terminal_panel.request_focus();
    frame(app, &ctx, 0.5, vec![]);
    frame(
        app,
        &ctx,
        0.6,
        menu_key(egui::Key::F12, egui::Modifiers::ALT),
    );
    assert_ne!(
        app.bottom,
        Some(BottomTool::Terminal),
        "Alt+F12 hides the terminal"
    );
}
#[test]
#[cfg(unix)] // ConPTY echo and output timing differ from a Unix PTY
fn escape_returns_focus_to_editor_when_enabled() {
    let Some(mut fixture) = focused_terminal() else {
        return;
    };
    let ctx = fixture.ctx.clone();
    let app = &mut fixture.app;
    assert!(app.settings.terminal.preferences.escape_focuses_editor);
    frame(
        app,
        &ctx,
        0.2,
        menu_key(egui::Key::Escape, egui::Modifiers::NONE),
    );
    frame(app, &ctx, 0.3, vec![]);
    assert!(!app.terminal_panel.focused, "Escape leaves the terminal");
    assert!(app.focus_editor, "the editor takes the keyboard");
    assert_eq!(
        app.bottom,
        Some(BottomTool::Terminal),
        "the tool window stays open"
    );
    // With the setting off, Escape belongs to the shell (e.g. vi mode).
    // The panel's preferences are synced from the settings every frame.
    app.settings.terminal.preferences.escape_focuses_editor = false;
    app.terminal_panel.request_focus();
    frame(app, &ctx, 0.4, vec![]);
    frame(app, &ctx, 0.5, vec![]);
    assert!(app.terminal_panel.focused);
    frame(
        app,
        &ctx,
        0.6,
        menu_key(egui::Key::Escape, egui::Modifiers::NONE),
    );
    frame(app, &ctx, 0.7, vec![]);
    assert!(app.terminal_panel.focused);
    assert!(shell_echo(app).contains("^["), "the shell received Escape");
}
