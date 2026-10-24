//! Genuine installed template engines through the Frameworks Run consumer.
use super::*;
use std::time::{Duration, Instant};

struct Fixture {
    app: IdeApp,
    _root: tempfile::TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.app.cancel_run_workflows();
        for session in &mut self.app.run_panel.sessions {
            session.stop();
        }
        // Retain every session and its project files on assertion failure until
        // the shared supervisor acknowledges process/tree/pipe retirement.
        let context = egui::Context::default();
        while self.app.run_workflows_pending() || self.app.run_panel.has_running() {
            self.app.poll_run_workflows(&context);
            self.app.run_panel.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Fixture {
    fn render(&mut self, path: &Path, key: usize, python: &Path) -> (i32, String) {
        self.app.open_file(path);
        let source = self.app.tabs[self.app.active.unwrap()].state.doc.text();
        let id = self.app.next_session_id;
        self.app.framework_launch(key);
        let context = egui::Context::default();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            self.app.poll_run_workflows(&context);
            self.app.run_panel.poll();
            if self.app.run_panel.launcher.launched.contains_key(&id)
                && self
                    .app
                    .run_panel
                    .sessions
                    .iter()
                    .any(|session| session.id == id)
            {
                break;
            }
            assert!(
                self.app.run_workflows_pending(),
                "framework launch refused before session publication: status={:?}, next_session_id={}, expected={id}",
                self.app.status,
                self.app.next_session_id,
            );
            assert!(
                Instant::now() < deadline,
                "real framework workflow did not publish its captured session"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let config = self
            .app
            .run_panel
            .launcher
            .launched
            .get(&id)
            .expect("real framework route must record its configuration");
        assert_eq!(config.working_dir.as_deref(), Some(self._root.path()));
        let RunConfigKind::Program { program, args } = &config.kind else {
            panic!("render must use selected Python program");
        };
        assert_eq!(program, python);
        assert_eq!(args.first().map(String::as_str), Some("-c"));
        assert!(args[1].contains(path.to_str().unwrap()));
        let session = self
            .app
            .run_panel
            .sessions
            .iter_mut()
            .find(|session| session.id == id)
            .expect("real template Run session");
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            session.poll();
            if !session.is_running() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "template provider did not retire: {}",
                session.text()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = session.text();
        let code = match &session.status {
            rustpy_run::SessionStatus::Exited(code) => *code,
            status => panic!("template child did not exit normally: {status:?}"),
        };
        assert_eq!(std::fs::read_to_string(path).unwrap(), source);
        assert_eq!(
            self.app.tabs[self.app.active.unwrap()].state.doc.text(),
            source
        );
        (code, output)
    }
}

#[test]
#[ignore = "requires explicitly admitted genuine Python SDK with Mako and Chameleon installed; no installation by fixture"]
fn configured_mako_chameleon_render_unicode_expressions_and_report_template_errors() {
    assert_eq!(
        std::env::var("RUSTPY_REQUIRE_TEMPLATE_PROVIDERS").as_deref(),
        Ok("1")
    );
    assert!(
        !rustpy_settings::trust_all_from_env(),
        "safe-mode refusal must be genuine"
    );
    let python = PathBuf::from(
        std::env::var_os("RUSTPY_TEMPLATE_PYTHON").expect("absolute admitted SDK required"),
    );
    assert!(python.is_absolute() && python.is_file());
    let sdk = rustpy_project::interpreter::probe_detailed(&python, "configured-template-provider")
        .expect("actual selected SDK probe")
        .interpreter;
    assert_eq!(sdk.path, python);
    let root = tempfile::Builder::new()
        .prefix("templates λ ")
        .tempdir()
        .unwrap();
    let mako = root.path().join("owned.mako");
    let chameleon = root.path().join("owned.pt");
    std::fs::write(&mako, "Mako λ ${6 * 7}").unwrap();
    std::fs::write(&chameleon, "<p>Chameleon λ ${6 * 7}</p>").unwrap();
    let mut app = IdeApp::new(Settings::default(), Some(root.path().into()));
    app.project.as_mut().unwrap().interpreter_rx = None;
    app.project.as_mut().unwrap().interpreter = Some(sdk);
    let mut fixture = Fixture { app, _root: root };
    fixture.app.open_file(&mako);
    assert!(fixture.app.project_safe_mode());
    fixture.app.framework_launch(16);
    assert!(
        fixture.app.run_panel.sessions.is_empty(),
        "untrusted template must not execute"
    );
    fixture.app.settings.trust_project(fixture._root.path());
    fixture.app.safe_mode = false;
    fixture.app.dialog = Dialog::None;
    assert!(!fixture.app.project_safe_mode());
    for (path, key, expected) in [(&mako, 16, "Mako λ 42"), (&chameleon, 17, "Chameleon λ 42")] {
        let (code, output) = fixture.render(path, key, &python);
        assert_eq!(code, 0, "genuine installed engine failed: {output}");
        assert!(
            output.contains(expected),
            "rendered Unicode/expression missing: {output}"
        );
    }
    for (path, key, invalid, error) in [
        (&mako, 16, "Mako λ ${1 + }\n", "SyntaxException"),
        (
            &chameleon,
            17,
            "<p>Chameleon λ ${1 + }</p>\n",
            "ExpressionError",
        ),
    ] {
        // Edit through the document, then let the production launch save it.
        // Writing the disk first would manufacture an external-change conflict
        // against the editor's captured disk revision and correctly block Run.
        let index = fixture
            .app
            .tabs
            .iter()
            .position(|tab| tab.state.path.as_deref() == Some(path.as_path()))
            .unwrap();
        let length = fixture.app.tabs[index].state.doc.len();
        fixture.app.tabs[index].state.apply_text_edits(
            vec![(TextRange::new(TextSize::new(0), length), invalid.into())],
            "Prepare invalid template",
        );
        let (code, output) = fixture.render(path, key, &python);
        assert_ne!(code, 0, "invalid template unexpectedly rendered: {output}");
        assert!(
            output.contains(error),
            "actual engine error missing: {output}"
        );
        assert!(
            !output.contains("ModuleNotFoundError"),
            "installed provider required: {output}"
        );
    }
    assert!(!fixture.app.run_panel.has_running());
}
