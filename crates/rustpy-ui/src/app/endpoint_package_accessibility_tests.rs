//! Actual App menu ownership over scanner and installed-catalog models.
//! No HTTP server, pip operation, registry request or daemon is claimed.
use super::*;
struct Fixture {
    app: IdeApp,
    ctx: egui::Context,
    root: tempfile::TempDir,
}
fn fixture(source: &str) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("π.py");
    std::fs::write(&file, source).unwrap();
    let mut settings = Settings::default();
    settings.trust_project(root.path());
    let mut app = IdeApp::new(settings, Some(root.path().into()));
    app.open_file(&file);
    app.theme = UiTheme::high_contrast();
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    Fixture { app, ctx, root }
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
    let update = output.platform_output.accesskit_update.clone().unwrap();
    output.drop_without_applying_deltas();
    update
}
fn tab_to(f: &mut Fixture, prefix: &str) {
    for _ in 0..80 {
        let update = frame(f, key(egui::Key::Tab, egui::Modifiers::NONE));
        if update.nodes.iter().any(|(id, node)| {
            node.label().is_some_and(|text| text.starts_with(prefix))
                && f.ctx
                    .memory(|memory| memory.focused())
                    .is_some_and(|focus| focus.accesskit_id() == *id)
        }) {
            return;
        }
    }
    panic!("actual Tab reaches {prefix}");
}
fn assert_not_focused(f: &Fixture, update: &egui::accesskit::TreeUpdate, label: &str) {
    assert!(
        !update.nodes.iter().any(|(id, node)| node
            .label()
            .is_some_and(|text| text.starts_with(label))
            && f.ctx
                .memory(|memory| memory.focused())
                .is_some_and(|focus| focus.accesskit_id() == *id)),
        "replacement must not inherit focus"
    );
}
#[test]
fn actual_scanned_endpoint_menu_is_named_and_replacement_modal_safe() {
    let source = "from flask import Flask\napp = Flask(__name__)\n@app.route('/π')\ndef route(): return '😀'\n";
    let mut f = fixture(source);
    let file = f.root.path().join("π.py");
    f.app.endpoints_panel.endpoints = rustpy_semantic::endpoints::scan_file(&file, source);
    assert_eq!(f.app.endpoints_panel.endpoints.len(), 1);
    f.app.endpoints_panel.scanned = true;
    f.app.bottom = Some(BottomTool::Endpoints);
    frame(&mut f, vec![]);
    frame(&mut f, vec![]);
    tab_to(&mut f, "/π");
    frame(
        &mut f,
        key(egui::Key::F10, egui::Modifiers::CTRL | egui::Modifiers::ALT),
    );
    let menu = frame(&mut f, vec![]);
    assert!(egui::Popup::is_any_open(&f.ctx));
    for label in [
        "Jump to Source",
        "Generate HTTP Request",
        "Open in Browser",
        "Copy URL",
    ] {
        assert!(
            menu.nodes
                .iter()
                .any(|(_, node)| node.label() == Some(label)),
            "{label}"
        );
    }
    frame(&mut f, key(egui::Key::Escape, egui::Modifiers::NONE));
    tab_to(&mut f, "/π");
    f.app.endpoints_panel.endpoints[0].path = "/different".into();
    let replaced = frame(&mut f, vec![]);
    assert_not_focused(&f, &replaced, "/different");
    frame(
        &mut f,
        key(egui::Key::F10, egui::Modifiers::CTRL | egui::Modifiers::ALT),
    );
    assert!(!egui::Popup::is_any_open(&f.ctx));
    tab_to(&mut f, "/different");
    f.app.handle_action(Action::Settings, &f.ctx);
    frame(
        &mut f,
        key(egui::Key::F10, egui::Modifiers::CTRL | egui::Modifiers::ALT),
    );
    assert!(!egui::Popup::is_any_open(&f.ctx));
    assert_eq!(f.app.tabs[0].state.doc.text(), source);
    assert!(!f.app.tabs[0].state.doc.can_undo());
}
fn installed(name: &str) -> rustpy_packages::InstalledPackage {
    rustpy_packages::InstalledPackage {
        name: name.into(),
        version: "1.0".into(),
        location: None,
        editable: false,
        summary: None,
    }
}
#[test]
fn actual_installed_package_menu_has_exact_provider_identity_and_modal_ownership() {
    let Some(python) = ["python3", "python"]
        .into_iter()
        .find_map(|name| which::which(name).ok())
    else {
        return;
    };
    let mut f = fixture("value = 'π😀'\n");
    f.app.project.as_mut().unwrap().interpreter = Some(rustpy_project::Interpreter {
        path: python.clone(),
        version: "fixture".into(),
        prefix: f.root.path().into(),
        sys_path: vec![],
        is_venv: false,
        platform: "fixture".into(),
        kind: "fixture".into(),
    });
    f.app
        .packages_panel
        .testing_installed_catalog(python.clone(), vec![installed("package-one")]);
    f.app.bottom = Some(BottomTool::Packages);
    frame(&mut f, vec![]);
    frame(&mut f, vec![]);
    tab_to(&mut f, "package-one 1.0");
    frame(
        &mut f,
        key(egui::Key::F10, egui::Modifiers::CTRL | egui::Modifiers::ALT),
    );
    let menu = frame(&mut f, vec![]);
    assert!(egui::Popup::is_any_open(&f.ctx));
    for label in ["Upgrade", "Uninstall"] {
        assert!(menu
            .nodes
            .iter()
            .any(|(_, node)| node.label() == Some(label)));
    }
    frame(&mut f, key(egui::Key::Escape, egui::Modifiers::NONE));
    tab_to(&mut f, "package-one 1.0");
    f.app
        .packages_panel
        .testing_installed_catalog(python, vec![installed("package-two")]);
    let replaced = frame(&mut f, vec![]);
    assert_not_focused(&f, &replaced, "package-two 1.0");
    frame(
        &mut f,
        key(egui::Key::F10, egui::Modifiers::CTRL | egui::Modifiers::ALT),
    );
    assert!(!egui::Popup::is_any_open(&f.ctx));
    tab_to(&mut f, "package-two 1.0");
    f.app.handle_action(Action::Settings, &f.ctx);
    frame(
        &mut f,
        key(egui::Key::F10, egui::Modifiers::CTRL | egui::Modifiers::ALT),
    );
    assert!(!egui::Popup::is_any_open(&f.ctx));
    assert_eq!(f.app.tabs[0].state.doc.text(), "value = 'π😀'\n");
    assert!(!f.app.tabs[0].state.doc.can_undo());
}
