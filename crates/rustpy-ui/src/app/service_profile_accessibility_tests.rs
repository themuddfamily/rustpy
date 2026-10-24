//! Actual App keyboard controls over retained report/catalog models.
//! These fixtures do not assert Docker daemon or profiler process availability.
use super::*;
struct Fixture {
    app: IdeApp,
    ctx: egui::Context,
    root: tempfile::TempDir,
}
fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source.py");
    std::fs::write(&path, "value = 1\n").unwrap();
    let mut settings = Settings::default();
    settings.trust_project(root.path());
    let mut app = IdeApp::new(settings, Some(root.path().into()));
    app.open_file(&path);
    app.theme = UiTheme::high_contrast();
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    Fixture { app, ctx, root }
}
fn retain_docker_catalog(f: &mut Fixture) {
    // Model the host admission normally performed by the update loop, then
    // pause refreshes so rendering the injected catalog cannot start a daemon.
    f.app.services_panel.docker_enabled = true;
    f.app
        .services_panel
        .docker
        .set_admission(true, f.root.path());
    f.app.services_panel.docker.cancel();
    assert!(f.app.services_panel.docker.admitted());
    assert!(f.app.services_panel.docker.pending().is_none());
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
fn frame(f: &mut Fixture, events: Vec<egui::Event>) -> egui::accesskit::TreeUpdate {
    let time = f.ctx.input(|input| input.time) + 0.01;
    let output = f.ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            focused: true,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1300.0, 800.0))),
            ..Default::default()
        },
        |ui| {
            f.app.global_shortcuts(ui);
            egui::CentralPanel::default().show(ui, |ui| f.app.bottom_panel(ui));
            f.app.dialogs(ui.ctx());
        },
    );
    assert!(
        f.app.services_panel.docker.pending().is_none(),
        "retained catalog keyboard fixtures must never launch Docker"
    );
    let update = output.platform_output.accesskit_update.clone().unwrap();
    output.drop_without_applying_deltas();
    update
}
fn tab_to(f: &mut Fixture, prefix: &str) {
    for _ in 0..80 {
        let update = frame(f, key(egui::Key::Tab, egui::Modifiers::NONE));
        if update.nodes.iter().any(|(id, node)| {
            node.label()
                .or_else(|| node.value())
                .is_some_and(|text| text.starts_with(prefix))
                && f.ctx
                    .memory(|memory| memory.focused())
                    .is_some_and(|focus| focus.accesskit_id() == *id)
        }) {
            return;
        }
    }
    panic!("actual Tab reaches {prefix}");
}
#[test]
fn actual_profiler_selection_has_keyboard_source_navigation_and_builtin_refusal() {
    let mut f = fixture();
    let target = f.root.path().join("π.py");
    std::fs::write(&target, "def π_target():\n    return '😀'\n").unwrap();
    f.app.profiler_panel.report = Some(rustpy_run::ProfileReport {
        functions: vec![rustpy_run::FunctionStats {
            file: target.to_string_lossy().into_owned(),
            line: 1,
            name: "π_target".into(),
            ncalls: 1,
            primitive_calls: 1,
            ..Default::default()
        }],
        ..Default::default()
    });
    f.app.bottom = Some(BottomTool::Profiler);
    frame(&mut f, vec![]);
    frame(&mut f, vec![]);
    tab_to(&mut f, "π_target");
    frame(&mut f, key(egui::Key::Enter, egui::Modifiers::NONE));
    assert_eq!(f.app.profiler_panel.selected, Some(0));
    tab_to(&mut f, "Open selected source");
    frame(&mut f, key(egui::Key::Enter, egui::Modifiers::NONE));
    assert_eq!(
        f.app.tabs[f.app.active.unwrap()].state.path.as_deref(),
        Some(target.as_path())
    );
    assert!(f.app.tabs.iter().all(|tab| !tab.state.doc.can_undo()));
    f.app.profiler_panel.report.as_mut().unwrap().functions[0].file = "~".into();
    let update = frame(&mut f, vec![]);
    assert!(update
        .nodes
        .iter()
        .any(|(_, node)| node.label() == Some("Open selected source") && node.is_disabled()));
}
#[test]
fn actual_services_keyboard_menu_checks_catalog_membership_and_modal_ownership() {
    let mut f = fixture();
    f.app.bottom = Some(BottomTool::Services);
    retain_docker_catalog(&mut f);
    f.app.services_panel.docker.snapshot = Some(Ok(rustpy_docker::Snapshot {
        containers: vec![rustpy_docker::Container {
            id: "catalog-container".into(),
            names: "π service".into(),
            state: "running".into(),
            ..Default::default()
        }],
        ..Default::default()
    }));
    frame(&mut f, vec![]);
    frame(&mut f, vec![]);
    tab_to(&mut f, "π service");
    assert!(f.app.services_panel.claims_context_menu(
        &f.ctx,
        egui::Key::F10,
        egui::Modifiers::SHIFT
    ));
    frame(&mut f, key(egui::Key::F10, egui::Modifiers::SHIFT));
    let menu = frame(&mut f, vec![]);
    assert!(egui::Popup::is_any_open(&f.ctx));
    for label in ["Show Log", "Open Shell (exec sh)"] {
        assert!(
            menu.nodes
                .iter()
                .any(|(_, node)| node.label() == Some(label)),
            "{label}"
        );
    }
    frame(&mut f, key(egui::Key::Escape, egui::Modifiers::NONE));
    assert!(!egui::Popup::is_any_open(&f.ctx));
    tab_to(&mut f, "π service");
    assert!(f.app.services_panel.claims_context_menu(
        &f.ctx,
        egui::Key::F10,
        egui::Modifiers::SHIFT
    ));
    f.app.services_panel.docker.snapshot = Some(Ok(rustpy_docker::Snapshot::default()));
    assert!(
        !f.app
            .services_panel
            .claims_context_menu(&f.ctx, egui::Key::F10, egui::Modifiers::SHIFT),
        "removed container cannot claim Run key"
    );
    f.app.services_panel.docker.snapshot = Some(Ok(rustpy_docker::Snapshot {
        containers: vec![rustpy_docker::Container {
            id: "catalog-container".into(),
            names: "π service".into(),
            state: "running".into(),
            ..Default::default()
        }],
        ..Default::default()
    }));
    frame(&mut f, vec![]);
    tab_to(&mut f, "π service");
    assert!(
        f.app
            .services_panel
            .claims_context_menu(&f.ctx, egui::Key::F10, egui::Modifiers::SHIFT),
        "current row reacquired before Settings"
    );
    f.app.services_panel.docker.snapshot = Some(Ok(rustpy_docker::Snapshot {
        containers: vec![rustpy_docker::Container {
            id: "replacement-container".into(),
            names: "different service".into(),
            state: "running".into(),
            ..Default::default()
        }],
        ..Default::default()
    }));
    let replaced = frame(&mut f, vec![]);
    assert!(
        !replaced
            .nodes
            .iter()
            .any(|(id, node)| node.label() == Some("different service")
                && f.ctx
                    .memory(|memory| memory.focused())
                    .is_some_and(|focus| focus.accesskit_id() == *id)),
        "no-input catalog replacement must not inherit row focus"
    );
    assert!(
        !f.app
            .services_panel
            .claims_context_menu(&f.ctx, egui::Key::F10, egui::Modifiers::SHIFT),
        "new item must be explicitly focused before menu dispatch"
    );
    f.app.services_panel.docker.snapshot = Some(Ok(rustpy_docker::Snapshot {
        containers: vec![rustpy_docker::Container {
            id: "catalog-container".into(),
            names: "π service".into(),
            state: "running".into(),
            ..Default::default()
        }],
        ..Default::default()
    }));
    frame(&mut f, vec![]);
    tab_to(&mut f, "π service");
    assert!(f.app.services_panel.claims_context_menu(
        &f.ctx,
        egui::Key::F10,
        egui::Modifiers::SHIFT
    ));
    f.app.handle_action(Action::Settings, &f.ctx);
    frame(&mut f, key(egui::Key::F10, egui::Modifiers::SHIFT));
    assert!(!egui::Popup::is_any_open(&f.ctx));
    assert_eq!(f.app.tabs[0].state.doc.text(), "value = 1\n");
    assert!(!f.app.tabs[0].state.doc.can_undo());
}

#[test]
fn actual_services_image_header_and_actions_are_keyboard_reachable() {
    let mut f = fixture();
    f.app.bottom = Some(BottomTool::Services);
    retain_docker_catalog(&mut f);
    f.app.services_panel.docker.snapshot = Some(Ok(rustpy_docker::Snapshot {
        images: vec![rustpy_docker::Image {
            id: "catalog-image".into(),
            repository: "π".into(),
            tag: "latest".into(),
            ..Default::default()
        }],
        ..Default::default()
    }));
    frame(&mut f, vec![]);
    frame(&mut f, vec![]);
    tab_to(&mut f, "Images (1)");
    frame(&mut f, key(egui::Key::Enter, egui::Modifiers::NONE));
    frame(&mut f, vec![]);
    tab_to(&mut f, "π:latest");
    frame(&mut f, key(egui::Key::F10, egui::Modifiers::SHIFT));
    let menu = frame(&mut f, vec![]);
    assert!(egui::Popup::is_any_open(&f.ctx));
    assert!(menu
        .nodes
        .iter()
        .any(|(_, node)| node.label() == Some("Create and Run Container")));
    frame(&mut f, key(egui::Key::Escape, egui::Modifiers::NONE));
    assert!(!egui::Popup::is_any_open(&f.ctx));
    assert_eq!(f.app.tabs[0].state.doc.text(), "value = 1\n");
    assert!(!f.app.tabs[0].state.doc.can_undo());
}
