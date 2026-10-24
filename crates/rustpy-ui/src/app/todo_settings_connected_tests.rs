//! Real Settings window interaction, isolated from the user's global config.
use super::*;

#[test]
fn todo_settings_apply_cancel_and_persist() {
    let config = tempfile::tempdir().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "app::settings_ui::todo_settings_connected_tests::todo_settings_dialog_child",
            "--nocapture",
        ])
        .env("RUSTPY_CONFIG_DIR", config.path())
        .env("RUSTPY_TODO_SETTINGS_CHILD", "1")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("MIR_SOCKET")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdin.take());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("owned TODO settings fixture exceeded 30s");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn frame(
    app: &mut IdeApp,
    ctx: &egui::Context,
    events: Vec<egui::Event>,
) -> Vec<(String, egui::Pos2)> {
    let output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 900.0),
            )),
            events,
            ..Default::default()
        },
        |_ui| app.settings_window(ctx),
    );
    let labels = output
        .shapes
        .iter()
        .filter_map(|s| {
            if let egui::Shape::Text(text) = &s.shape {
                Some((
                    text.galley.text().to_string(),
                    text.pos + text.galley.rect.center().to_vec2(),
                ))
            } else {
                None
            }
        })
        .collect();
    output.drop_without_applying_deltas();
    labels
}
fn click(app: &mut IdeApp, ctx: &egui::Context, labels: &[(String, egui::Pos2)], label: &str) {
    let pos = labels
        .iter()
        .find(|(text, _)| text == label)
        .unwrap_or_else(|| panic!("missing actual UI label {label}: {labels:?}"))
        .1;
    frame(
        app,
        ctx,
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ],
    );
    frame(
        app,
        ctx,
        vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
    );
}
#[test]
fn todo_settings_dialog_child() {
    if std::env::var_os("RUSTPY_TODO_SETTINGS_CHILD").is_none() {
        return;
    }
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    let mut app = IdeApp::new(Settings::default(), None);
    let original = app.settings.todo.clone();
    app.dialog = Dialog::Settings;
    app.settings_ui.page = SettingsPage::Todo;
    frame(&mut app, &ctx, vec![]);
    let labels = frame(&mut app, &ctx, vec![]);
    assert!(labels.iter().any(|(text, _)| text == "TODO patterns"));
    assert!(TREE
        .iter()
        .any(|(_, pages)| pages.contains(&("TODO", SettingsPage::Todo))));
    click(
        &mut app,
        &ctx,
        &labels,
        "Include indented continuation comment lines",
    );
    assert_ne!(app.settings.todo.multiline, original.multiline);
    assert_eq!(app.settings_to_persist().todo, original);
    assert!(page_modified(
        SettingsPage::Todo,
        &Settings::default(),
        &app.settings
    ));
    assert!(!page_modified(
        SettingsPage::EditorGeneral,
        &Settings::default(),
        &app.settings
    ));
    let labels = frame(&mut app, &ctx, vec![]);
    click(&mut app, &ctx, &labels, "Apply");
    let applied = app.settings.todo.clone();
    assert_eq!(app.settings_to_persist().todo, applied);
    assert_eq!(Settings::load().todo, applied);
    let labels = frame(&mut app, &ctx, vec![]);
    click(&mut app, &ctx, &labels, "Add pattern");
    assert_eq!(app.settings.todo.patterns.len(), applied.patterns.len() + 1);
    let labels = frame(&mut app, &ctx, vec![]);
    click(&mut app, &ctx, &labels, "Cancel");
    assert_eq!(app.settings.todo, applied);
    assert_eq!(Settings::load().todo, applied);
    assert!(app.settings_ui.snapshot.is_none());
}
