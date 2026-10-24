//! Real tab renderer pointer transport, with geometry obtained from its tree.
use super::*;

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
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 700.0))),
            ..Default::default()
        },
        |ui| {
            egui::CentralPanel::default().show(ui, |ui| app.group_area(ui, 0));
        },
    );
    let update = output.platform_output.accesskit_update.clone().unwrap();
    output.drop_without_applying_deltas();
    update
}
#[test]
fn actual_pointer_drag_from_rendered_tab_outside_viewport_detaches_captured_editor() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("π.py");
    std::fs::write(&path, "π = 1\n").unwrap();
    let mut app = IdeApp::new(Settings::default(), Some(root.path().into()));
    app.open_file(&path);
    let view = app.tabs[0].view.id;
    let source = app.tabs[0].state.doc.text();
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    frame(&mut app, &ctx, 0.0, vec![]);
    let update = frame(&mut app, &ctx, 0.1, vec![]);
    let bounds = update
        .nodes
        .iter()
        .find(|(_, node)| node.role() == egui::accesskit::Role::Tab && node.label() == Some("π.py"))
        .expect("actual tab row")
        .1
        .bounds()
        .expect("actual painted row bounds");
    let point = Pos2::new(
        ((bounds.x0 + bounds.x1) * 0.5) as f32,
        ((bounds.y0 + bounds.y1) * 0.5) as f32,
    );
    frame(
        &mut app,
        &ctx,
        0.2,
        vec![
            egui::Event::PointerMoved(point),
            egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    frame(
        &mut app,
        &ctx,
        0.3,
        vec![egui::Event::PointerMoved(point + Vec2::new(45.0, 0.0))],
    );
    let outside = Pos2::new(-50.0, point.y);
    frame(
        &mut app,
        &ctx,
        0.4,
        vec![egui::Event::PointerMoved(outside)],
    );
    frame(
        &mut app,
        &ctx,
        0.5,
        vec![egui::Event::PointerButton {
            pos: outside,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
    );
    assert_eq!(
        app.tabs[app
            .detached
            .expect("actual drag release requests detached window")]
        .view
        .id,
        view
    );
    assert_eq!(app.tabs[0].state.doc.text(), source);
    assert!(!app.tabs[0].state.doc.can_undo());
}

#[test]
fn actual_multirow_renderer_wraps_named_tabs_and_keeps_editor_rendering_owner() {
    let root = tempfile::tempdir().unwrap();
    let mut app = IdeApp::new(Settings::default(), Some(root.path().into()));
    for name in [
        "alpha.py",
        "beta.py",
        "gamma.py",
        "delta.py",
        "epsilon.py",
        "zeta.py",
    ] {
        let path = root.path().join(name);
        std::fs::write(&path, "π = 1\n").unwrap();
        app.open_file(&path);
    }
    app.settings.editor.tabs.multiple_rows = true;
    let active = app.tabs[app.active.unwrap()].view.id;
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    let mut paint = |time| {
        let output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                focused: true,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(320.0, 700.0))),
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| app.group_area(ui, 0));
            },
        );
        let update = output.platform_output.accesskit_update.clone().unwrap();
        output.drop_without_applying_deltas();
        update
    };
    paint(0.0);
    let update = paint(0.1);
    let ys: std::collections::HashSet<_> = update
        .nodes
        .iter()
        .filter(|(_, node)| node.role() == egui::accesskit::Role::Tab)
        .filter_map(|(_, node)| node.bounds())
        .map(|rect| rect.y0.round() as i64)
        .collect();
    assert!(ys.len() > 1, "actual painted tabs occupy multiple rows");
    assert_eq!(app.tabs[app.active.unwrap()].view.id, active);
    assert!(app.tabs[app.active.unwrap()]
        .view
        .caret_screen_rect
        .is_some());
    assert!(!app.tabs.iter().any(|tab| tab.state.doc.can_undo()));
}
