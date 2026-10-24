//! Actual installed-plugin/App/editor-frame checks; process-owned private config.
use super::*;

#[test]
fn csv_plugin_actual_connected_routes() {
    let root = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "app::csv_table_connected_tests::csv_plugin_connected_child",
            "--nocapture",
        ])
        .env("RUSTPY_CSV_CONNECTED_CHILD", "1");
    for (key, directory) in [
        ("RUSTPY_CONFIG_DIR", "config"),
        ("RUSTPY_SYSTEM_DIR", "system"),
        ("RUSTPY_LOG_DIR", "logs"),
        ("XDG_CONFIG_HOME", "xdg-config"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_RUNTIME_DIR", "runtime"),
    ] {
        let directory = root.path().join(directory);
        std::fs::create_dir(&directory).unwrap();
        command.env(key, directory);
    }
    command
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("MIR_SOCKET")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().unwrap();
    drop(child.stdin.take());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("owned CSV fixture timed out");
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
    time: f64,
    events: Vec<egui::Event>,
) -> Vec<(String, Pos2)> {
    let output = ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            focused: true,
            events,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200.0, 800.0))),
            ..Default::default()
        },
        |ui| {
            app.global_shortcuts(ui);
            egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
        },
    );
    let labels = output
        .shapes
        .iter()
        .filter_map(|shape| {
            if let egui::Shape::Text(text) = &shape.shape {
                Some((
                    text.galley.job.text.clone(),
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
fn click(app: &mut IdeApp, ctx: &egui::Context, time: f64, point: Pos2) {
    for (dt, pressed) in [(0.0, true), (0.02, false)] {
        frame(
            app,
            ctx,
            time + dt,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
}
#[test]
fn csv_plugin_connected_child() {
    if std::env::var_os("RUSTPY_CSV_CONNECTED_CHILD").is_none() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let plugin = root.path().join("plugin");
    std::fs::create_dir(&plugin).unwrap();
    std::fs::write(plugin.join("plugin.json"), r#"{"id":"org.test.csv","name":"CSV","version":"1.0.0","contributes":{"table_editors":[{"name":"CSV Table","extensions":["csv"],"kind":"Csv"}]}}"#).unwrap();
    crate::plugin_host::update(|registry| registry.install_from_dir(&plugin)).unwrap();
    assert_eq!(crate::plugin_host::table_editors().len(), 1);
    let source = "name,note\nπ,\"🙂 first\n\"\"second\"\"\"\n";
    let path = root.path().join("fixture.csv");
    std::fs::write(&path, source).unwrap();
    let mut settings = Settings::default();
    settings.trust_project(root.path());
    settings.editor.ensure_newline_at_eof = false;
    let mut app = IdeApp::new(settings, Some(root.path().to_path_buf()));
    app.open_file(&path);
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    frame(&mut app, &ctx, 0.0, vec![]);
    let labels = frame(&mut app, &ctx, 0.05, vec![]);
    assert!(labels.iter().any(|(text, _)| text == "π"));
    assert!(labels.iter().any(|(text, _)| text == "🙂 first↵\"second\""));
    let i = app.active.unwrap();
    let initial_caret = app.tabs[i].state.primary();
    frame(&mut app, &ctx, 0.1, vec![egui::Event::Text("LEAK".into())]);
    assert_eq!(app.tabs[i].state.doc.text(), source);
    assert_eq!(app.tabs[i].state.primary(), initial_caret);
    assert!(!app.tabs[i].state.doc.can_undo());
    let source_point = labels
        .iter()
        .find(|(text, _)| text == "Show as text")
        .expect("actual CSV Show as text control")
        .1;
    app.handle_action(Action::Settings, &ctx);
    click(&mut app, &ctx, 0.12, source_point);
    assert!(matches!(app.dialog, Dialog::Settings));
    assert!(app.csv_table_owns_input());
    assert!(!app.csv_source_mode(app.tabs[i].view.id));
    assert_eq!(app.tabs[i].state.doc.text(), source);
    app.dialog = Dialog::None;
    frame(&mut app, &ctx, 0.17, vec![]);
    click(&mut app, &ctx, 0.2, source_point);
    frame(&mut app, &ctx, 0.25, vec![]);
    assert!(!app.csv_table_owns_input());
    app.tabs[i].state.move_doc_end(false);
    frame(
        &mut app,
        &ctx,
        0.3,
        vec![
            egui::Event::ModifiersChanged(egui::Modifiers::NONE),
            egui::Event::Text("β,🙂\n".into()),
        ],
    );
    assert_eq!(app.tabs[i].state.doc.text(), format!("{source}β,🙂\n"));
    app.handle_action(Action::SaveAll, &ctx);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!("{source}β,🙂\n")
    );
    assert!(app.tabs[i].state.undo());
    assert_eq!(app.tabs[i].state.doc.text(), source);
    let labels = frame(&mut app, &ctx, 0.4, vec![]);
    let table_point = labels.iter().find(|(text, _)| text == "Table").unwrap().1;
    click(&mut app, &ctx, 0.45, table_point);
    let labels = frame(&mut app, &ctx, 0.5, vec![]);
    assert!(app.csv_table_owns_input());
    assert!(labels.iter().any(|(text, _)| text == "🙂 first↵\"second\""));
    let before = app.tabs[i].state.doc.text();
    let before_caret = app.tabs[i].state.primary();
    let undo = app.tabs[i].state.doc.can_undo();
    let redo = app.tabs[i].state.doc.can_redo();
    assert!(
        redo,
        "Source Undo left a real redo entry for refusal testing"
    );
    for action in [
        Action::Undo,
        Action::Redo,
        Action::Paste,
        Action::DeleteLine,
        Action::ReformatCode,
        Action::SelectAll,
        Action::CaretRight,
        Action::CodeCompletion,
        Action::InsertLiveTemplate,
        Action::PlaybackLastMacro,
    ] {
        app.handle_action(action, &ctx);
        assert_eq!(app.tabs[i].state.doc.text(), before, "{action:?}");
        assert_eq!(app.tabs[i].state.primary(), before_caret, "{action:?}");
        assert_eq!(app.tabs[i].state.doc.can_undo(), undo, "{action:?}");
        assert_eq!(app.tabs[i].state.doc.can_redo(), redo, "{action:?}");
        assert!(matches!(app.dialog, Dialog::None), "{action:?}");
    }
    // Project activation reveals a hidden pane; hiding requires actual tree focus.
    app.layout.left_visible = false;
    app.handle_action(Action::ToggleProjectView, &ctx);
    assert!(app.layout.left_visible);
    assert_eq!(app.left_tool, LeftTool::Project);
    assert_eq!(app.tabs[i].state.doc.text(), before);
    assert_eq!(app.tabs[i].state.primary(), before_caret);
    app.split_shared_editor(SplitDir::Horizontal);
    assert_eq!(app.tabs.len(), 2);
    frame(&mut app, &ctx, 0.6, vec![]);
    let left = app.tabs[0].view.id;
    let right = app.tabs[1].view.id;
    assert_ne!(left, right);
    assert_eq!(app.csv_routes.len(), 2);
    let labels = frame(&mut app, &ctx, 0.62, vec![]);
    let right_source = labels
        .iter()
        .filter(|(text, _)| text == "Show as text")
        .max_by(|a, b| a.1.x.total_cmp(&b.1.x))
        .unwrap()
        .1;
    click(&mut app, &ctx, 0.64, right_source);
    frame(&mut app, &ctx, 0.67, vec![]);
    assert!(app.csv_source_mode(right));
    assert!(!app.csv_source_mode(left));
    assert!(app.tabs[0]
        .state
        .doc
        .shares_authority(&app.tabs[1].state.doc));
    // A real pointer into passive visible Source must supersede active Table ownership.
    let labels = frame(&mut app, &ctx, 0.68, vec![]);
    let left_table = labels
        .iter()
        .filter(|(text, _)| text == "Table")
        .min_by(|a, b| a.1.x.total_cmp(&b.1.x))
        .unwrap()
        .1;
    click(&mut app, &ctx, 0.681, left_table);
    assert_eq!(app.active, Some(0));
    assert!(app.csv_table_owns_input());
    let labels = frame(&mut app, &ctx, 0.704, vec![]);
    let passive_source_glyph = labels
        .iter()
        .filter(|(text, pos)| text.contains("name") && pos.x > 600.0)
        .max_by(|a, b| a.1.x.total_cmp(&b.1.x))
        .expect("actual passive source glyph")
        .1;
    click(&mut app, &ctx, 0.705, passive_source_glyph);
    frame(&mut app, &ctx, 0.728, vec![]);
    assert_eq!(app.active, Some(1));
    assert!(ctx.memory(|memory| memory.has_focus(right)));
    assert!(!app.csv_table_owns_input());
    assert_eq!(app.tabs[1].state.doc.text(), source);
    crate::plugin_host::update(|registry| registry.disable("org.test.csv")).unwrap();
    frame(&mut app, &ctx, 0.74, vec![]);
    assert!(!app.csv_table_owns_input());
    assert!(app.csv_routes.is_empty());
    crate::plugin_host::update(|registry| registry.enable("org.test.csv")).unwrap();
    let malformed = root.path().join("malformed.csv");
    std::fs::write(&malformed, "\"unterminated").unwrap();
    app.open_file(&malformed);
    let labels = frame(&mut app, &ctx, 0.8, vec![]);
    assert!(labels
        .iter()
        .any(|(text, _)| text.contains("Unterminated quoted field")));
    assert_eq!(
        std::fs::read_to_string(&malformed).unwrap(),
        "\"unterminated"
    );
    let binary = root.path().join("binary.csv");
    std::fs::write(&binary, b"binary\0bytes").unwrap();
    app.open_file(&binary);
    frame(&mut app, &ctx, 0.9, vec![]);
    assert!(!app.csv_table_owns_input());
    assert_eq!(std::fs::read(&binary).unwrap(), b"binary\0bytes");
    let read_only = root.path().join("readonly.csv");
    std::fs::write(&read_only, "π,🙂\n").unwrap();
    let mut permissions = std::fs::metadata(&read_only).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&read_only, permissions).unwrap();
    app.open_file(&read_only);
    frame(&mut app, &ctx, 1.0, vec![]);
    assert!(app.csv_table_owns_input());
    frame(&mut app, &ctx, 1.1, vec![egui::Event::Text("LEAK".into())]);
    assert_eq!(std::fs::read_to_string(&read_only).unwrap(), "π,🙂\n");
    assert!(app.tabs[app.active.unwrap()].state.doc.is_read_only());
    // Ordinary Python peer must not rely on CSV auxiliary pointer activation.
    let python = root.path().join("plain_peer.py");
    let python_source = "PLAIN_PEER = 'ψ🙂'\n";
    std::fs::write(&python, python_source).unwrap();
    let mut settings = Settings::default();
    settings.trust_project(root.path());
    settings.completion.show_as_you_type = false;
    let mut startup = IdeApp::new(settings, Some(root.path().to_path_buf()));
    startup.open_file(&python);
    startup.open_file(&path);
    startup.open_file(&python);
    startup.split_and_move_editor(SplitDir::Horizontal);
    startup.open_file(&path);
    assert_eq!(startup.tabs.len(), 2);
    let csv_index = startup.active.unwrap();
    let python_index = startup
        .tabs
        .iter()
        .position(|tab| tab.state.path.as_deref() == Some(python.as_path()))
        .unwrap();
    assert_eq!(startup.tabs[csv_index].group, 0);
    assert_eq!(startup.tabs[python_index].group, 1);
    let python_view = startup.tabs[python_index].view.id;
    assert!(startup.csv_table_owns_input());
    assert!(
        startup.focus_editor,
        "real open_file queued initial CSV focus"
    );
    let startup_ctx = egui::Context::default();
    crate::theme::install_fonts(&startup_ctx);
    startup.handle_action(Action::Settings, &startup_ctx);
    frame(&mut startup, &startup_ctx, 0.0, vec![]);
    assert!(
        startup.focus_editor,
        "foreign Settings owner must retain queued editor focus"
    );
    startup.dialog = Dialog::None;
    let labels = frame(&mut startup, &startup_ctx, 0.1, vec![]);
    assert!(
        !startup.focus_editor,
        "active Table must consume its eligible startup focus queue"
    );
    let glyph = labels
        .iter()
        .find(|(text, pos)| text.contains("PLAIN_PEER") && pos.x > 600.0)
        .expect("actual passive Python source glyph")
        .1;
    click(&mut startup, &startup_ctx, 0.2, glyph);
    frame(&mut startup, &startup_ctx, 0.25, vec![]);
    assert_eq!(startup.active, Some(python_index));
    assert!(startup_ctx.memory(|memory| memory.has_focus(python_view)));
    assert!(!startup.csv_table_owns_input());
    assert_eq!(startup.tabs[python_index].state.doc.text(), python_source);
    assert_eq!(
        startup.tabs[csv_index].state.doc.text(),
        std::fs::read_to_string(&path).unwrap()
    );
    assert!(!startup.tabs[python_index].state.doc.can_undo());
    assert!(!startup.tabs[csv_index].state.doc.can_undo());
    assert_eq!(std::fs::read_to_string(&python).unwrap(), python_source);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!("{source}β,🙂\n")
    );
}
