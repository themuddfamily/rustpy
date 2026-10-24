//! Real App/debugpy variable-tree keyboard and AccessKit integration.
use super::*;
use std::time::{Duration, Instant};

fn key(key: egui::Key) -> Vec<egui::Event> {
    modified_key(key, egui::Modifiers::NONE)
}
fn modified_key(key: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
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
        },
    );
    let update = output.platform_output.accesskit_update.clone().unwrap();
    output.drop_without_applying_deltas();
    update
}
fn stopped(app: &mut IdeApp) {
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        let session = app.debug.as_mut().unwrap();
        session.poll();
        if session.is_stopped() && !session.scopes.is_empty() {
            let references: Vec<_> = session
                .scopes
                .iter()
                .map(|scope| scope.variables_reference)
                .collect();
            for reference in references {
                session.request_variables(reference);
            }
            if session.variables.values().flatten().any(|v| v.name == "π") {
                return;
            }
        }
        assert!(
            Instant::now() < until,
            "actual debugpy stop/π variables: {:?}",
            session.state
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn actual_debugger_tab_focus_expands_unicode_value_and_retires_old_suspension_focus() {
    let Some(python) = ["python3", "python"]
        .into_iter()
        .find_map(|name| which::which(name).ok())
    else {
        return;
    };
    if !std::process::Command::new(&python)
        .args(["-c", "import debugpy"])
        .stdin(std::process::Stdio::piped())
        .output()
        .unwrap()
        .status
        .success()
    {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("values.py");
    let text = "π = {'child': '😀'}\npass\npass\n";
    std::fs::write(&path, text).unwrap();
    let mut settings = Settings::default();
    settings.trust_project(root.path());
    let mut app = IdeApp::new(settings, Some(root.path().into()));
    app.open_file(&path);
    app.theme = UiTheme::high_contrast();
    app.bottom = Some(BottomTool::Debug);
    let mut config = rustpy_project::RunConfig::for_script(&path);
    config.interpreter = Some(python);
    app.debug = Some(
        DebugSession::launch(
            188,
            &config,
            None,
            root.path(),
            rustpy_debug::LaunchSetup {
                breakpoints: std::collections::HashMap::from([(
                    path.clone(),
                    vec![
                        rustpy_debug::Breakpoint::new(2),
                        rustpy_debug::Breakpoint::new(3),
                    ],
                )]),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    stopped(&mut app);
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    frame(&mut app, &ctx, 0.0, vec![]);
    frame(&mut app, &ctx, 0.1, vec![]);
    let mut focused = None;
    for n in 0..80 {
        let update = frame(&mut app, &ctx, 0.2 + n as f64 * 0.1, key(egui::Key::Tab));
        let focus = ctx.memory(|m| m.focused());
        if let Some((id, node)) = update.nodes.iter().find(|(id, node)| {
            Some(*id) == focus.map(|id| id.accesskit_id())
                && node.label().is_some_and(|label| label.starts_with("π:"))
                && node.role() != egui::accesskit::Role::Status
        }) {
            assert_eq!(node.is_expanded(), Some(false));
            assert!(node.label().unwrap().contains("😀"));
            assert!(node.label().unwrap().contains("type dict"));
            focused = Some(*id);
            break;
        }
    }
    let old_row = focused.expect("real Tab traversal reaches the variable row");
    frame(&mut app, &ctx, 10.0, key(egui::Key::ArrowRight));
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let session = app.debug.as_mut().unwrap();
        session.poll();
        if session
            .variables
            .values()
            .flatten()
            .any(|v| v.name.contains("child") && v.value.contains("😀"))
        {
            break;
        }
        assert!(Instant::now() < until, "Right loads real dict children");
        std::thread::sleep(Duration::from_millis(10));
    }
    let update = frame(&mut app, &ctx, 10.1, vec![]);
    assert!(
        update
            .nodes
            .iter()
            .any(|(_, node)| node.role() != egui::accesskit::Role::Status
                && node
                    .label()
                    .is_some_and(|label| label.contains("child") && label.contains("😀"))),
        "real expanded child is named"
    );
    let parent = update.nodes.iter().find(|(id, _)| *id == old_row).unwrap();
    assert_eq!(parent.1.is_expanded(), Some(true));
    let (status_id, status) = update
        .nodes
        .iter()
        .find(|(_, node)| {
            node.role() == egui::accesskit::Role::Status
                && node.label().is_some_and(|label| label.starts_with("π:"))
        })
        .expect("focused real variable announces its state");
    assert!(status.label().unwrap().contains("Expanded"));
    assert!(parent.1.children().contains(status_id));
    // Left and Enter act on the focused row, without editing the source.
    frame(&mut app, &ctx, 10.2, key(egui::Key::ArrowLeft));
    assert_eq!(
        frame(&mut app, &ctx, 10.3, vec![])
            .nodes
            .iter()
            .find(|(id, _)| *id == old_row)
            .unwrap()
            .1
            .is_expanded(),
        Some(false)
    );
    frame(&mut app, &ctx, 10.4, key(egui::Key::Enter));
    let after_enter = frame(&mut app, &ctx, 10.5, vec![]);
    let retained = after_enter.nodes.iter().find(|(id, _)| *id == old_row);
    assert!(retained.is_some(), "Enter retains exact row {old_row:?}; focus={:?}, dialog_none={}, epoch={}, expanded={:?}, nodes={:?}",
        ctx.memory(|memory| memory.focused()), matches!(app.dialog, Dialog::None),
        app.debug.as_ref().unwrap().stop_epoch, app.debug_panel.expanded,
        after_enter.nodes.iter().map(|(id, node)| (*id, node.label(), node.is_expanded())).collect::<Vec<_>>());
    assert_eq!(retained.unwrap().1.is_expanded(), Some(true));
    frame(
        &mut app,
        &ctx,
        10.6,
        modified_key(egui::Key::F10, egui::Modifiers::SHIFT),
    );
    assert!(
        egui::Popup::is_any_open(&ctx),
        "focused real row opens its ordinary actions with Shift F10"
    );
    let menu = frame(&mut app, &ctx, 10.7, vec![]);
    for title in [
        "Copy Value",
        "Copy Name",
        "Copy as Expression",
        "Add to Watches",
    ] {
        assert!(
            menu.nodes
                .iter()
                .any(|(_, node)| node.label() == Some(title)),
            "keyboard-opened action: {title}"
        );
    }
    frame(&mut app, &ctx, 10.8, key(egui::Key::Escape));
    assert!(!egui::Popup::is_any_open(&ctx));
    for n in 0..80 {
        if ctx
            .memory(|memory| memory.focused())
            .is_some_and(|id| id.accesskit_id() == old_row)
        {
            break;
        }
        frame(&mut app, &ctx, 10.9 + n as f64 * 0.01, key(egui::Key::Tab));
    }
    assert!(
        ctx.memory(|memory| memory.focused())
            .is_some_and(|id| id.accesskit_id() == old_row),
        "actual keyboard focus is on the first suspension before retiring it"
    );
    let first_epoch = app.debug.as_ref().unwrap().stop_epoch;
    app.debug.as_mut().unwrap().continue_();
    // Wait for the actual next stop, not merely the still-cached previous stop.
    let until = Instant::now() + Duration::from_secs(15);
    while app.debug.as_ref().unwrap().stop_epoch == first_epoch {
        app.debug.as_mut().unwrap().poll();
        assert!(Instant::now() < until);
        std::thread::sleep(Duration::from_millis(10));
    }
    stopped(&mut app);
    let update = frame(&mut app, &ctx, 20.0, key(egui::Key::ArrowLeft));
    assert!(
        !update.nodes.iter().any(|(id, _)| *id == old_row),
        "retired suspension has no live row identity"
    );
    assert!(
        !update
            .nodes
            .iter()
            .any(|(_, node)| node.role() == egui::accesskit::Role::Status
                && node.label().is_some_and(|label| label.starts_with("π:"))),
        "old focus cannot claim the new suspension"
    );
    assert_eq!(app.tabs[0].state.doc.text(), text);
    assert!(!app.tabs[0].state.doc.can_undo());
    app.debug.as_mut().unwrap().stop();
}
