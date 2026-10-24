//! Real Python process and actual App keyboard/AccessKit ownership.
use super::*;
use std::time::{Duration, Instant};

struct Fixture {
    app: IdeApp,
    ctx: egui::Context,
    _root: tempfile::TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.app.py_console.stop();
    }
}
fn fixture() -> Option<Fixture> {
    let python = ["python3", "python"]
        .into_iter()
        .find_map(|name| which::which(name).ok())?;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("π.py");
    std::fs::write(&source, "π = 1\n").unwrap();
    let mut settings = Settings::default();
    settings.trust_project(root.path());
    let mut app = IdeApp::new(settings, Some(root.path().into()));
    app.open_file(&source);
    app.bottom = Some(BottomTool::PythonConsole);
    app.theme = UiTheme::high_contrast();
    app.py_console.start(&python, root.path(), &[], root.path());
    assert!(app.py_console.is_running(), "installed real Python starts");
    app.py_console.execute("π = {'child': '😀'}");
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    Some(Fixture {
        app,
        ctx,
        _root: root,
    })
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
    fixture: &mut Fixture,
    time: f64,
    events: Vec<egui::Event>,
) -> egui::accesskit::TreeUpdate {
    fixture.app.py_console.poll();
    let output = fixture.ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            focused: true,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1100.0, 800.0))),
            ..Default::default()
        },
        |ui| {
            fixture.app.global_shortcuts(ui);
            egui::CentralPanel::default().show(ui, |ui| fixture.app.bottom_panel(ui));
            fixture.app.dialogs(ui.ctx());
        },
    );
    let update = output.platform_output.accesskit_update.clone().unwrap();
    output.drop_without_applying_deltas();
    update
}
fn wait(fixture: &mut Fixture, wanted: impl Fn(&egui::accesskit::TreeUpdate) -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    let mut time = fixture.ctx.input(|input| input.time) + 0.02;
    loop {
        let update = frame(fixture, time, vec![]);
        if wanted(&update) {
            return;
        }
        assert!(
            Instant::now() < until,
            "real Python response becomes visible"
        );
        std::thread::sleep(Duration::from_millis(10));
        time += 0.02;
    }
}
#[test]
fn actual_python_variable_tab_expansion_menu_and_retired_focus_are_guarded() {
    let Some(mut fixture) = fixture() else { return };
    wait(&mut fixture, |update| {
        update
            .nodes
            .iter()
            .any(|(_, node)| node.label().is_some_and(|text| text.starts_with("π = ")))
    });
    let mut row = None;
    for n in 0..80 {
        let update = frame(
            &mut fixture,
            30.0 + n as f64 * 0.1,
            key(egui::Key::Tab, egui::Modifiers::NONE),
        );
        row = update
            .nodes
            .iter()
            .find(|(id, node)| {
                node.label().is_some_and(|text| text.starts_with("π = "))
                    && fixture
                        .ctx
                        .memory(|memory| memory.focused())
                        .is_some_and(|focus| focus.accesskit_id() == *id)
            })
            .map(|(id, _)| *id);
        if row.is_some() {
            break;
        }
    }
    let row = row.expect("actual Tab reaches runtime Unicode variable");
    let update = frame(
        &mut fixture,
        40.0,
        key(egui::Key::ArrowRight, egui::Modifiers::NONE),
    );
    assert!(update
        .nodes
        .iter()
        .any(|(id, node)| *id == row && node.is_expanded() == Some(true)));
    wait(&mut fixture, |update| {
        update.nodes.iter().any(|(_, node)| {
            node.label()
                .is_some_and(|text| text.starts_with("'child' = ") && text.contains("😀"))
        })
    });
    frame(
        &mut fixture,
        41.0,
        key(egui::Key::F10, egui::Modifiers::SHIFT),
    );
    let menu = frame(&mut fixture, 41.1, vec![]);
    assert!(egui::Popup::is_any_open(&fixture.ctx));
    for label in ["View as table (SciView)", "Copy value"] {
        assert!(
            menu.nodes
                .iter()
                .any(|(_, node)| node.label() == Some(label)),
            "{label}"
        );
    }
    frame(
        &mut fixture,
        41.2,
        key(egui::Key::Escape, egui::Modifiers::NONE),
    );
    assert!(!egui::Popup::is_any_open(&fixture.ctx));
    let mut reacquired = false;
    for n in 0..80 {
        frame(
            &mut fixture,
            42.0 + n as f64 * 0.1,
            key(egui::Key::Tab, egui::Modifiers::NONE),
        );
        reacquired = fixture.app.py_console.claims_tree_menu(
            &fixture.ctx,
            egui::Key::F10,
            egui::Modifiers::SHIFT,
        );
        if reacquired {
            break;
        }
    }
    assert!(
        reacquired,
        "real Tab reacquires an actual variable row before its value is retired"
    );
    fixture.app.py_console.execute("π = 2");
    wait(&mut fixture, |update| {
        update
            .nodes
            .iter()
            .any(|(_, node)| node.label().is_some_and(|text| text.starts_with("π = 2.")))
    });
    assert!(
        !fixture.app.py_console.claims_tree_menu(
            &fixture.ctx,
            egui::Key::F10,
            egui::Modifiers::SHIFT
        ),
        "retired value row cannot claim Run shortcut"
    );
    assert_eq!(fixture.app.tabs[0].state.doc.text(), "π = 1\n");
    assert!(!fixture.app.tabs[0].state.doc.can_undo());
}
#[test]
fn actual_python_prompt_announces_output_and_settings_suppresses_it() {
    let Some(mut fixture) = fixture() else { return };
    fixture.app.py_console.request_focus();
    fixture.app.py_console.execute("print('π😀')");
    wait(&mut fixture, |update| {
        update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Status
                && node.label().is_some_and(|text| {
                    text.starts_with("Python console prompt.") && text.contains("π😀")
                })
        })
    });
    fixture.app.handle_action(Action::Settings, &fixture.ctx);
    let update = frame(&mut fixture, 50.0, vec![]);
    assert!(!update
        .nodes
        .iter()
        .any(|(_, node)| node.role() == egui::accesskit::Role::Status
            && node
                .label()
                .is_some_and(|text| text.starts_with("Python console "))));
    assert_eq!(fixture.app.tabs[0].state.doc.text(), "π = 1\n");
    assert!(!fixture.app.tabs[0].state.doc.can_undo());
}

#[test]
fn actual_settings_owner_prevents_retained_python_row_enter_and_space_expansion() {
    let Some(mut fixture) = fixture() else { return };
    wait(&mut fixture, |update| {
        update
            .nodes
            .iter()
            .any(|(_, node)| node.label().is_some_and(|text| text.starts_with("π = ")))
    });
    let mut reached = false;
    for n in 0..80 {
        frame(
            &mut fixture,
            30.0 + n as f64 * 0.1,
            key(egui::Key::Tab, egui::Modifiers::NONE),
        );
        reached = fixture.app.py_console.claims_tree_menu(
            &fixture.ctx,
            egui::Key::F10,
            egui::Modifiers::SHIFT,
        );
        if reached {
            break;
        }
    }
    assert!(
        reached,
        "actual runtime row has focus before competing Settings owner"
    );
    assert_eq!(
        fixture.app.py_console.testing_variable_state("π"),
        (false, false)
    );
    fixture.app.handle_action(Action::Settings, &fixture.ctx);
    for (n, pressed) in [egui::Key::Enter, egui::Key::Space].into_iter().enumerate() {
        frame(
            &mut fixture,
            40.0 + n as f64 * 0.1,
            key(pressed, egui::Modifiers::NONE),
        );
        assert_eq!(
            fixture.app.py_console.testing_variable_state("π"),
            (false, false),
            "Settings retains activation; the row must not expand or request children"
        );
    }
    assert_eq!(fixture.app.tabs[0].state.doc.text(), "π = 1\n");
    assert!(!fixture.app.tabs[0].state.doc.can_undo());
}
