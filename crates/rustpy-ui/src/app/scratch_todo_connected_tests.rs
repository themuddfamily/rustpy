//! Actual action dispatch and worker-to-tool painting in an owned config.
use super::*;

#[test]
fn scratch_and_todo_routes_in_owned_config() {
    let dir = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.args([
        "--exact",
        "app::scratch_todo_connected_tests::owned_routes_child",
        "--nocapture",
    ]);
    for (key, _) in std::env::vars() {
        if key.starts_with("RUSTPY_")
            || key.starts_with("XDG_")
            || matches!(key.as_str(), "DISPLAY" | "WAYLAND_DISPLAY" | "MIR_SOCKET")
        {
            command.env_remove(key);
        }
    }
    for (key, name) in [
        ("RUSTPY_CONFIG_DIR", "config"),
        ("RUSTPY_SYSTEM_DIR", "system"),
        ("RUSTPY_LOG_DIR", "log"),
        ("XDG_CONFIG_HOME", "xdg-config"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_RUNTIME_DIR", "runtime"),
    ] {
        let path = dir.path().join(name);
        std::fs::create_dir(&path).unwrap();
        command.env(key, path);
    }
    command.env("RUSTPY_SCRATCH_TODO_ROUTES_CHILD", "1");
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdin.take());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("owned action/worker fixture exceeded 30 seconds");
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

#[test]
fn owned_routes_child() {
    if std::env::var("RUSTPY_SCRATCH_TODO_ROUTES_CHILD").as_deref() != Ok("1") {
        return;
    }
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    let mut app = IdeApp::new_welcome(Settings::default());
    app.handle_action(Action::NewScratchBuffer, &ctx);
    let first = app.active.unwrap();
    let path = app.tabs[first].state.path.clone().unwrap();
    let identity = app.tabs[first].view.id;
    app.tabs[first]
        .state
        .insert_text("buffer λ🙂", &Default::default());
    for _ in 0..crate::scratches::BUFFER_COUNT {
        app.handle_action(Action::NewScratchBuffer, &ctx);
    }
    assert_eq!(app.tabs.len(), crate::scratches::BUFFER_COUNT);
    let index = app.active.unwrap();
    assert_eq!(app.tabs[index].view.id, identity);
    assert_eq!(app.tabs[index].state.doc.text(), "buffer λ🙂");
    assert_eq!(std::fs::read(&path).unwrap(), b"");
    app.tabs[index].state.undo();
    assert_eq!(app.tabs[index].state.doc.text(), "");
    app.tabs[index].state.redo();
    assert_eq!(app.tabs[index].state.doc.text(), "buffer λ🙂");

    let project = tempfile::tempdir().unwrap();
    let file = project.path().join("fixture.py");
    let source = "# REVIEW: λ\n#   continue 🙂\ntext = 'REVIEW: string'\n# review: wrong case\n# TODO: disabled\n";
    std::fs::write(&file, source).unwrap();
    let mut app = IdeApp::new_welcome(Settings::default());
    app.open_project(project.path());
    app.settings.todo.patterns = vec![rustpy_settings::todo::TodoPattern {
        regex: "REVIEW:".into(),
        case_sensitive: true,
        color: [222, 33, 144],
    }];
    app.request_todo_scan(0.0);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.todo_scan_running() {
        app.poll_project_index(&ctx);
        assert!(
            std::time::Instant::now() < deadline,
            "TODO worker never published"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        app.todo_items,
        vec![(file.clone(), 0, "REVIEW: λ\ncontinue 🙂".into())]
    );
    assert_eq!(app.todo_colors, vec![[222, 33, 144]]);
    let output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1200.0, 900.0))),
            ..Default::default()
        },
        |ui| app.todo_view(ui),
    );
    assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text().contains("REVIEW: λ") && text.galley.job.sections.iter().any(|section| section.format.color == Color32::from_rgb(222,33,144)))));
    output.drop_without_applying_deltas();
    app.settings.todo.patterns.clear();
    app.request_todo_scan(1.0);
    assert!(app.todo_items.is_empty() && app.todo_colors.is_empty());
    while app.todo_scan_running() {
        app.poll_project_index(&ctx);
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(app.todo_items.is_empty() && app.todo_colors.is_empty());
    assert_eq!(std::fs::read_to_string(file).unwrap(), source);
}
