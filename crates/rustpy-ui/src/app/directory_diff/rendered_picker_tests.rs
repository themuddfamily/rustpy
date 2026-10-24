//! Register inside app::directory_diff after actual Root picker integration.
use super::super::*;

fn frame(
    app: &mut IdeApp,
    ctx: &egui::Context,
    time: f64,
    events: Vec<egui::Event>,
) -> Vec<(String, egui::Pos2)> {
    let output = ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            events,
            focused: true,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 800.0),
            )),
            ..Default::default()
        },
        |_ui| app.dialogs(ctx),
    );
    let labels = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Text(text) => Some((
                text.galley.text().to_owned(),
                text.pos + text.galley.rect.center().to_vec2(),
            )),
            _ => None,
        })
        .collect();
    output.drop_without_applying_deltas();
    labels
}
fn point(labels: &[(String, egui::Pos2)], text: &str) -> egui::Pos2 {
    labels
        .iter()
        .find(|(label, _)| label == text)
        .unwrap_or_else(|| panic!("Actual painted picker label missing: {text}; labels={labels:?}"))
        .1
}
fn click(
    app: &mut IdeApp,
    ctx: &egui::Context,
    time: f64,
    pos: egui::Pos2,
) -> Vec<(String, egui::Pos2)> {
    let event = |pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    frame(
        app,
        ctx,
        time,
        vec![egui::Event::PointerMoved(pos), event(true)],
    );
    frame(app, ctx, time + 0.05, vec![event(false)])
}
fn fixture() -> (tempfile::TempDir, IdeApp, egui::Context) {
    let root = tempfile::tempdir().unwrap();
    for name in ["left λ", "right 🙂", "unrelated"] {
        std::fs::create_dir(root.path().join(name)).unwrap();
    }
    std::fs::write(root.path().join("file-not-directory.py"), b"source\n").unwrap();
    let mut settings = Settings::default();
    settings.trust_project(root.path());
    let mut app = IdeApp::new(settings, Some(root.path().into()));
    app.dialog = Dialog::None;
    let ctx = egui::Context::default();
    (root, app, ctx)
}
fn drain(app: &mut IdeApp, ctx: &egui::Context) {
    app.cancel_directory_comparisons();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !app.directory_diff_draining.is_empty() {
        app.poll_directory_comparison(ctx);
        assert!(
            std::time::Instant::now() < deadline,
            "Actual directory task retirement not acknowledged"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}
#[test]
fn actual_picker_directory_click_and_explicit_button_keep_captured_left_owner() {
    let (root, mut app, ctx) = fixture();
    let left = root.path().join("left λ");
    let right = root.path().join("right 🙂");
    app.open_directory_comparison_picker(&left);
    frame(&mut app, &ctx, 0.0, vec![]);
    let labels = frame(&mut app, &ctx, 0.1, vec![]);
    assert!(!labels
        .iter()
        .any(|(label, _)| label == "file-not-directory.py"));
    let row = point(&labels, "right 🙂");
    let labels = click(&mut app, &ctx, 0.2, row);
    assert!(
        matches!(&app.dialog, Dialog::DirectoryComparePick(state) if state.0.selected.as_ref() == Some(&right) && state.1 == left)
    );
    assert!(
        app.directory_comparison.is_none(),
        "Single directory click must select, not start comparison"
    );
    app.project.as_mut().unwrap().tree.selected = Some(root.path().join("unrelated"));
    let compare = point(&labels, "Compare directory");
    click(&mut app, &ctx, 0.4, compare);
    assert!(matches!(app.dialog, Dialog::DirectoryComparison));
    let state = app.directory_comparison.as_ref().unwrap();
    assert_eq!(state.left, left);
    assert_eq!(state.right, right);
    assert_eq!(
        std::fs::read(root.path().join("file-not-directory.py")).unwrap(),
        b"source\n"
    );
    drain(&mut app, &ctx);
}
#[test]
fn actual_picker_cancel_button_and_escape_start_no_job() {
    let (root, mut app, ctx) = fixture();
    let left = root.path().join("left λ");
    app.open_directory_comparison_picker(&left);
    frame(&mut app, &ctx, 0.0, vec![]);
    let labels = frame(&mut app, &ctx, 0.1, vec![]);
    click(&mut app, &ctx, 0.2, point(&labels, "Cancel"));
    assert!(matches!(app.dialog, Dialog::None));
    assert!(app.directory_comparison.is_none());
    assert!(app.directory_diff_draining.is_empty());
    app.open_directory_comparison_picker(&left);
    frame(&mut app, &ctx, 0.4, vec![]);
    frame(
        &mut app,
        &ctx,
        0.5,
        vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: Some(egui::Key::Escape),
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }],
    );
    assert!(matches!(app.dialog, Dialog::None));
    assert!(app.directory_comparison.is_none());
    assert!(app.directory_diff_draining.is_empty());
}
#[test]
fn actual_dialogs_paint_restores_unit_marker_without_cancelling_owned_worker() {
    let (root, mut app, ctx) = fixture();
    app.start_directory_comparison(root.path().join("left λ"), root.path().join("right 🙂"));
    let before = app.directory_comparison.as_ref().unwrap().quit_stamp();
    frame(&mut app, &ctx, 0.0, vec![]);
    frame(&mut app, &ctx, 0.1, vec![]);
    assert!(matches!(app.dialog, Dialog::DirectoryComparison));
    let after = app.directory_comparison.as_ref().unwrap().quit_stamp();
    assert_eq!(before.split(':').nth(1), after.split(':').nth(1));
    assert!(
        after.ends_with(":false"),
        "Actual dialog paint cancelled a retained worker: {after}"
    );
    assert!(app.directory_diff_draining.is_empty());
    drain(&mut app, &ctx);
}
