//! Real painted fenced-Python action, selected SDK and captured editor source.
use super::*;
use std::time::{Duration, Instant};

struct Fixture {
    app: IdeApp,
    directory: tempfile::TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for session in &mut self.app.run_panel.sessions {
            session.stop();
        }
        while self.app.run_panel.sessions.iter().any(|s| s.owns_process()) {
            self.app.run_panel.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
        self.app.poll_markdown_jobs();
    }
}
fn frame(
    app: &mut IdeApp,
    ctx: &egui::Context,
    time: f64,
    events: Vec<egui::Event>,
) -> Vec<(String, egui::Rect)> {
    let output = ctx.run_ui(
        egui::RawInput {
            time: Some(time),
            focused: true,
            events,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400., 1000.),
            )),
            ..Default::default()
        },
        |ui| {
            egui::CentralPanel::default().show(ui, |ui| app.editor_area(ui));
        },
    );
    let labels = output
        .shapes
        .iter()
        .filter_map(|shape| {
            if let egui::Shape::Text(text) = &shape.shape {
                Some((
                    text.galley.text().to_owned(),
                    egui::Rect::from_min_size(text.pos, text.galley.size()),
                ))
            } else {
                None
            }
        })
        .collect();
    output.drop_without_applying_deltas();
    labels
}
fn click_run(app: &mut IdeApp, ctx: &egui::Context, time: &mut f64) {
    *time += 0.1;
    let labels = frame(app, ctx, *time, vec![]);
    let targets = labels
        .iter()
        .filter(|(text, _)| text == "Run Python block")
        .collect::<Vec<_>>();
    assert_eq!(targets.len(), 1, "one actual current-buffer fenced action");
    let point = targets[0].1.center();
    for pressed in [true, false] {
        *time += 0.1;
        frame(
            app,
            ctx,
            *time,
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
fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("explicit {name} required"))
}
#[cfg(target_os = "linux")]
fn identity(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()?
        .rsplit_once(") ")?
        .1
        .split_whitespace()
        .nth(19)
        .map(str::to_owned)
}

#[test]
#[ignore = "requires Root-admitted genuine absolute Python SDK and private config/system/log/TMPDIR"]
fn configured_markdown_fenced_python_runs_selected_sdk_and_current_snapshot() {
    assert_eq!(required("RUSTPY_MARKDOWN_PYTHON_ADMITTED"), "1");
    assert!(std::env::var_os("RUSTPY_TRUST_ALL").is_none());
    let python = PathBuf::from(required("RUSTPY_MARKDOWN_PYTHON"));
    assert!(python.is_absolute() && python.is_file());
    let hash = rustpy_net::x509::sha256(&std::fs::read(&python).unwrap())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    assert_eq!(hash, required("RUSTPY_MARKDOWN_PYTHON_SHA256"));
    let temporary = PathBuf::from(required("TMPDIR")).canonicalize().unwrap();
    for name in ["RUSTPY_CONFIG_DIR", "RUSTPY_SYSTEM_DIR", "RUSTPY_LOG_DIR"] {
        let path = PathBuf::from(required(name));
        assert!(path.is_absolute() && path.starts_with(&temporary));
        std::fs::create_dir_all(&path).unwrap();
        assert!(path.canonicalize().unwrap().starts_with(&temporary));
    }
    let directory = tempfile::Builder::new()
        .prefix("markdown python π ")
        .tempdir_in(&temporary)
        .unwrap();
    let path = directory.path().join("current λ.md");
    let disk = "# Disk authority\n\n```python\nraise RuntimeError('disk must not run')\n```\n";
    std::fs::write(&path, disk).unwrap();
    let code = "import json, os, pathlib, sys, time\nprint('MARKDOWN_RESULT=' + json.dumps(['current café π 42',sys.executable,os.getcwd(),pathlib.Path(__file__).read_text(),os.getpid()]),flush=True)\nwhile not pathlib.Path('owned-release').exists():\n    time.sleep(0.01)\n";
    let source = format!("# Current fenced snapshot\n\n```python\n{code}```\n");
    let mut app = IdeApp::new(Settings::default(), Some(directory.path().into()));
    app.project.as_mut().unwrap().interpreter_rx = None;
    app.project.as_mut().unwrap().interpreter = None;
    app.open_file(&path);
    let index = app.active.unwrap();
    app.tabs[index]
        .state
        .doc
        .set_text(&source, &[], &[])
        .unwrap();
    app.markdown_mode = MarkdownMode::PreviewOnly;
    app.markdown_workspace.initialized = true;
    let mut fixture = Fixture { app, directory };
    let ctx = egui::Context::default();
    let mut time = 0.;
    fixture.app.safe_mode = true;
    click_run(&mut fixture.app, &ctx, &mut time);
    assert!(fixture.app.run_panel.sessions.is_empty());
    assert!(fixture.app.markdown_workspace.message.contains("Trust"));
    assert!(fixture.app.settings.trust_project(fixture.directory.path()));
    fixture.app.safe_mode = false;
    click_run(&mut fixture.app, &ctx, &mut time);
    assert!(fixture.app.run_panel.sessions.is_empty());
    assert!(fixture
        .app
        .markdown_workspace
        .message
        .contains("Select a Python"));
    let sdk = rustpy_project::interpreter::probe(&python, "Root-admitted Markdown SDK")
        .expect("genuine selected SDK probe");
    fixture.app.select_interpreter(sdk);
    assert_eq!(
        fixture
            .app
            .project
            .as_ref()
            .unwrap()
            .interpreter
            .as_ref()
            .unwrap()
            .path,
        python
    );
    click_run(&mut fixture.app, &ctx, &mut time);
    assert_eq!(fixture.app.run_panel.sessions.len(), 1);
    let session = &fixture.app.run_panel.sessions[0];
    let id = session.id;
    let pid = session.process_id().expect("actual fenced Python process");
    #[cfg(target_os = "linux")]
    let start = identity(pid).expect("live exact Python identity");
    let cfg = session.config.as_ref().unwrap();
    let RunConfigKind::Program { program, args } = &cfg.kind else {
        panic!("actual fenced Python Program route");
    };
    assert_eq!(program, &python);
    assert_eq!(cfg.working_dir.as_deref(), Some(fixture.directory.path()));
    assert_eq!(args[0], "-u");
    let snapshot = PathBuf::from(&args[1]);
    assert!(snapshot.starts_with(Settings::config_dir().join("markdown")));
    assert_eq!(std::fs::read_to_string(&snapshot).unwrap(), code);
    assert_eq!(fixture.app.markdown_workspace.artifacts.len(), 1);
    let artifact = &fixture.app.markdown_workspace.artifacts[0];
    assert_eq!(artifact.id, id);
    assert_eq!(artifact.file.as_deref(), Some(path.as_path()));
    assert_eq!(
        artifact.version,
        fixture.app.tabs[index].state.doc.version()
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let observed: serde_json::Value = loop {
        fixture.app.run_panel.poll();
        let session = fixture
            .app
            .run_panel
            .sessions
            .iter()
            .find(|s| s.id == id)
            .unwrap();
        let text = session.text();
        if let Some(line) = text
            .lines()
            .find_map(|line| line.strip_prefix("MARKDOWN_RESULT="))
        {
            break serde_json::from_str(line).unwrap();
        }
        assert!(session.owns_process(), "real SDK failed: {text}");
        assert!(Instant::now() < deadline, "real fenced output deadline");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(observed[0], "current café π 42");
    assert_eq!(observed[1], serde_json::json!(python));
    assert_eq!(observed[2], serde_json::json!(fixture.directory.path()));
    assert_eq!(observed[3], code);
    assert_eq!(observed[4], pid);
    fixture.app.tabs[index]
        .state
        .doc
        .set_text("# New revision after capture\n", &[], &[])
        .unwrap();
    std::fs::write(
        fixture.directory.path().join("owned-release"),
        "release actual owned child\n",
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        fixture.app.run_panel.poll();
        let session = fixture
            .app
            .run_panel
            .sessions
            .iter()
            .find(|s| s.id == id)
            .unwrap();
        if !session.owns_process() {
            assert_eq!(
                session.status,
                rustpy_run::SessionStatus::Exited(0),
                "{}",
                session.text()
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "real fenced child physical retirement deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    #[cfg(target_os = "linux")]
    assert_ne!(identity(pid).as_deref(), Some(start.as_str()));
    fixture.app.poll_markdown_jobs();
    assert!(fixture.app.markdown_workspace.artifacts.is_empty());
    assert!(
        !snapshot.exists(),
        "captured source staging retires after actual child"
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), disk);
    assert_eq!(
        fixture.app.tabs[index].state.doc.text(),
        "# New revision after capture\n"
    );
}
