//! Explicit configured acceptance of documentation Run tasks, not inspection peers.
use super::*;

fn configured_app(root: &Path) -> IdeApp {
    assert!(
        std::env::var("RUSTPY_REQUIRE_PROVIDER_TESTS").is_ok_and(|value| value == "1"),
        "configured task acceptance requires RUSTPY_REQUIRE_PROVIDER_TESTS=1"
    );
    let python = PathBuf::from(
        std::env::var_os("RUSTPY_PROVIDER_PYTHON")
            .expect("configure an admitted RUSTPY_PROVIDER_PYTHON with Sphinx and Docutils"),
    );
    assert!(
        python.is_absolute() && python.is_file(),
        "configured Python must be an absolute installed executable"
    );
    let mut settings = Settings::default();
    settings.trust_project(root);
    let mut app = IdeApp::new(settings, Some(root.into()));
    app.project.as_mut().unwrap().interpreter = Some(rustpy_project::Interpreter {
        prefix: python.parent().unwrap().parent().unwrap().into(),
        path: python,
        version: String::new(),
        sys_path: Vec::new(),
        is_venv: true,
        platform: std::env::consts::OS.into(),
        kind: "configured documentation acceptance".into(),
    });
    app
}

fn finish(app: &mut IdeApp, id: u64) -> (rustpy_run::SessionStatus, String) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
    loop {
        app.run_panel.poll();
        let session = app
            .run_panel
            .sessions
            .iter()
            .find(|session| session.id == id)
            .expect("actual Run task must retain its captured session");
        if !session.is_running() {
            return (
                session.status.clone(),
                session
                    .output
                    .iter()
                    .map(|chunk| chunk.text.as_str())
                    .collect(),
            );
        }
        if std::time::Instant::now() >= deadline {
            let output: String = session
                .output
                .iter()
                .map(|chunk| chunk.text.as_str())
                .collect();
            app.run_panel
                .sessions
                .iter_mut()
                .find(|session| session.id == id)
                .unwrap()
                .stop();
            panic!("documentation Run task did not retire: {output}");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn last_session(app: &IdeApp) -> u64 {
    let session = app
        .run_panel
        .sessions
        .last()
        .unwrap_or_else(|| panic!("documentation task was not admitted: {:?}", app.status));
    let selected = &app
        .project
        .as_ref()
        .unwrap()
        .interpreter
        .as_ref()
        .unwrap()
        .path;
    assert!(
        session
            .command_line
            .contains(selected.to_string_lossy().as_ref()),
        "documentation task must execute the selected SDK: {}",
        session.command_line
    );
    session.id
}

#[test]
#[ignore = "requires explicit admitted SDK with real Sphinx; runs actual documentation Run configuration"]
fn configured_sphinx_task_builds_html_and_reports_real_warning_failure() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let docs = root.join("docs with spaces");
    std::fs::create_dir(&docs).unwrap();
    std::fs::write(docs.join("conf.py"), "project = 'RustPy actual task'\nmaster_doc = 'index'\nextensions = []\nhtml_theme = 'basic'\n").unwrap();
    let source = docs.join("index.rst");
    std::fs::write(&source, "Original\n========\n").unwrap();
    let mut app = configured_app(&root);
    app.settings.sphinx.working_directory = "docs with spaces".into();
    app.settings.sphinx.output_directory = "built html".into();
    app.settings.sphinx.fresh_environment = true;
    app.settings.sphinx.warnings_as_errors = true;
    app.open_file(&source);
    let index = app.active.unwrap();
    let clean = "Actual Sphinx Heading\n=====================\n\nACTUAL_SPHINX_TASK_BODY\n";
    app.tabs[index].state.doc.set_text(clean, &[], &[]).unwrap();
    app.build_sphinx_docs();
    let id = last_session(&app);
    let captured = app
        .run_panel
        .sessions
        .iter()
        .find(|session| session.id == id)
        .unwrap()
        .config
        .clone()
        .unwrap();
    assert!(matches!(&captured.kind, RunConfigKind::PythonModule { module } if module == "sphinx"));
    assert!(captured.parameters.contains("built html"));
    // Editing preferences cannot retarget the already admitted child command.
    app.settings.sphinx.output_directory = "later output".into();
    let (status, output) = finish(&mut app, id);
    assert!(
        matches!(status, rustpy_run::SessionStatus::Exited(0)),
        "{status:?}: {output}"
    );
    assert!(output.contains("build succeeded"), "{output}");
    let html = std::fs::read_to_string(docs.join("built html/index.html"))
        .expect("real Sphinx HTML publication");
    assert!(html.contains("Actual Sphinx Heading") && html.contains("ACTUAL_SPHINX_TASK_BODY"));
    assert!(!docs.join("later output").exists());
    assert_eq!(
        std::fs::read_to_string(&source).unwrap(),
        clean,
        "actual task saves the captured dirty editor before execution"
    );
    assert!(app
        .project
        .as_ref()
        .unwrap()
        .run_configs
        .configs
        .iter()
        .any(|config| config.name == captured.name));

    app.settings.sphinx.output_directory = "failed html".into();
    app.tabs[index]
        .state
        .doc
        .set_text(
            "Failure\n=======\n\n.. rustpy_actual_missing_directive::\n",
            &[],
            &[],
        )
        .unwrap();
    app.build_sphinx_docs();
    let failed = last_session(&app);
    assert_ne!(failed, id);
    let (status, output) = finish(&mut app, failed);
    assert!(
        matches!(status, rustpy_run::SessionStatus::Exited(code) if code != 0),
        "warnings-as-errors must fail actual Sphinx: {status:?}: {output}"
    );
    assert!(
        output.contains("rustpy_actual_missing_directive") && output.contains("index.rst"),
        "actual provider diagnostic must reach Run output: {output}"
    );
    let count = app.run_panel.sessions.len();
    app.safe_mode = true;
    app.settings.sphinx.output_directory = "untrusted html".into();
    app.build_sphinx_docs();
    assert_eq!(app.run_panel.sessions.len(), count);
    assert!(!docs.join("untrusted html").exists());
}

#[test]
#[ignore = "requires explicit admitted SDK with real Docutils; runs actual documentation Run configuration"]
fn configured_docutils_task_saves_html_and_exposes_real_document_errors() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let source = root.join("document with spaces.rst");
    std::fs::write(&source, "Original\n========\n").unwrap();
    let mut app = configured_app(&root);
    app.open_file(&source);
    let index = app.active.unwrap();
    let clean = "Actual Docutils Heading\n=======================\n\nACTUAL_DOCUTILS_TASK_BODY\n";
    app.tabs[index].state.doc.set_text(clean, &[], &[]).unwrap();
    let output_path = app.docutils_output().unwrap();
    app.run_docutils_task();
    let id = last_session(&app);
    let config = app
        .run_panel
        .sessions
        .iter()
        .find(|session| session.id == id)
        .unwrap()
        .config
        .as_ref()
        .unwrap();
    assert!(
        matches!(&config.kind, RunConfigKind::Program { program, args } if program.is_absolute() && args.iter().any(|argument| argument.contains("publish_cmdline")))
    );
    let (status, output) = finish(&mut app, id);
    assert!(
        matches!(status, rustpy_run::SessionStatus::Exited(0)),
        "{status:?}: {output}"
    );
    let html = std::fs::read_to_string(&output_path).expect("actual Docutils HTML publication");
    assert!(html.contains("Actual Docutils Heading") && html.contains("ACTUAL_DOCUTILS_TASK_BODY"));
    assert_eq!(std::fs::read_to_string(&source).unwrap(), clean);

    app.tabs[index]
        .state
        .doc
        .set_text(
            "Failure\n=======\n\n.. rustpy_docutils_missing_directive::\n",
            &[],
            &[],
        )
        .unwrap();
    app.run_docutils_task();
    let failed = last_session(&app);
    assert_ne!(failed, id);
    let (status, output) = finish(&mut app, failed);
    assert!(
        matches!(status, rustpy_run::SessionStatus::Exited(_)),
        "provider must really execute: {status:?}: {output}"
    );
    // Docutils' default exit threshold may return zero for an ERROR diagnostic;
    // require its actual source-bound diagnostic instead of fabricating failure status.
    assert!(
        output.contains("ERROR")
            && output.contains("rustpy_docutils_missing_directive")
            && output.contains("document with spaces.rst"),
        "{output}"
    );
    let count = app.run_panel.sessions.len();
    app.safe_mode = true;
    app.run_docutils_task();
    assert_eq!(app.run_panel.sessions.len(), count);
}
