use super::*;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    interpreter: PathBuf,
    interpreter_sha256: String,
    version: String,
    module_path: PathBuf,
    module_sha256: String,
    library_root: PathBuf,
    module_name: String,
    callable_name: String,
}

/// Root supplies a freshly probed, explicitly trusted CPython standard-library
/// extension. Ordinary CI neither discovers nor executes an interpreter.
#[test]
#[ignore = "requires explicit Root-admitted CPython interpreter/native image config"]
fn actual_trusted_cpython_extension_action_publishes_verified_stub_and_revokes_on_close() {
    let config_path = std::env::var_os("RUSTPY_NATIVE_SUCCESS_CONFIG")
        .expect("explicit runner --config is required");
    let config: Config = serde_json::from_slice(&std::fs::read(config_path).unwrap()).unwrap();
    let token = CancellationToken::new();
    assert_eq!(
        std::fs::canonicalize(&config.interpreter).unwrap(),
        config.interpreter
    );
    assert_eq!(
        std::fs::canonicalize(&config.module_path).unwrap(),
        config.module_path
    );
    assert_eq!(
        std::fs::canonicalize(&config.library_root).unwrap(),
        config.library_root
    );
    assert!(config.module_path.starts_with(&config.library_root));
    assert_eq!(
        rustpy_semantic::module::extension_module_name(
            config.module_path.file_name().unwrap().to_str().unwrap()
        ),
        Some(config.module_name.rsplit('.').next().unwrap())
    );
    assert_eq!(
        rustpy_project::native_skeleton::fingerprint_file(&config.interpreter, &token).unwrap(),
        config.interpreter_sha256
    );
    assert_eq!(
        rustpy_project::native_skeleton::fingerprint_file(&config.module_path, &token).unwrap(),
        config.module_sha256
    );

    let project_root = tempfile::tempdir().unwrap();
    let mut settings = Settings::default();
    settings.trust_project(project_root.path());
    let mut app = IdeApp::new(settings, Some(project_root.path().into()));
    let project = app.project.as_mut().unwrap();
    project.interpreter_rx = None;
    project.interpreter = Some(Interpreter {
        path: config.interpreter.clone(),
        version: config.version.clone(),
        prefix: config.library_root.clone(),
        sys_path: vec![config.library_root.clone()],
        is_venv: false,
        platform: std::env::consts::OS.into(),
        kind: "Root-admitted actual CPython".into(),
    });
    project.analysis = Arc::new(Analysis::new(rustpy_semantic::module::ModuleResolver::new(
        vec![project_root.path().into()],
        vec![config.library_root.clone()],
        (3, 12),
    )));
    project.tree.selected = Some(config.module_path.clone());
    let analysis = project.analysis.clone();
    assert!(!config.module_name.is_empty());
    assert!(config
        .module_name
        .split('.')
        .all(rustpy_semantic::module::is_identifier));
    assert!(rustpy_semantic::module::is_identifier(
        &config.callable_name
    ));
    let supplied = analysis.module(&config.module_name).unwrap();
    assert!(supplied.path.to_string_lossy().contains(&format!(
        "<typeshed>/{}.pyi",
        config.module_name.replace('.', "/")
    )));
    let supplied_text = supplied.text.clone();
    let ctx = egui::Context::default();
    app.handle_action(Action::GenerateNativeStub, &ctx);
    assert!(
        app.native_skeletons_pending(),
        "real action must own a managed worker"
    );
    let original_task = app.native_skeleton_ui.active.as_ref().unwrap().handle.id();
    let manager = app
        .native_skeleton_ui
        .active
        .as_ref()
        .unwrap()
        .manager
        .clone();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while app.native_skeletons_pending() {
        app.poll_native_skeletons(&ctx);
        assert!(
            std::time::Instant::now() < deadline,
            "actual producer/verification did not retire"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(!manager.is_running(original_task));
    assert!(app.native_skeleton_ui.active.is_none());
    assert!(app.native_skeleton_ui.draining.is_empty());
    assert_eq!(
        app.native_skeleton_ui.records.len(),
        1,
        "actual success, not fallback Any"
    );
    let record = &app.native_skeleton_ui.records[0];
    assert_eq!(record.module_name, config.module_name);
    assert_eq!(record.module_path, config.module_path);
    assert_eq!(record.module_sha256, config.module_sha256);
    assert_eq!(record.interpreter.path, config.interpreter);
    assert_eq!(record.interpreter.sha256, config.interpreter_sha256);
    assert_eq!(record.interpreter.version, config.version);
    assert!(
        record
            .pyi
            .contains(&format!("def {}(", config.callable_name)),
        "real standard extension members must be present"
    );
    assert!(rustpy_syntax::parse(&record.pyi).errors().is_empty());
    // A published generated record must not override a supplied authoritative stub.
    let resolved = analysis.module(&config.module_name).unwrap();
    assert!(resolved.path.to_string_lossy().contains(&format!(
        "<typeshed>/{}.pyi",
        config.module_name.replace('.', "/")
    )));
    assert_eq!(resolved.text, supplied_text);
    app.cancel_native_skeletons();
    assert!(app.native_skeleton_ui.records.is_empty());
    assert_eq!(
        analysis.module(&config.module_name).unwrap().text,
        supplied_text
    );
    app.close_project();
    assert!(app.project.is_none());
    assert!(!app.native_skeletons_pending());
    assert!(app.native_skeleton_ui.records.is_empty());
    assert_eq!(
        rustpy_project::native_skeleton::fingerprint_file(&config.interpreter, &token).unwrap(),
        config.interpreter_sha256
    );
    assert_eq!(
        rustpy_project::native_skeleton::fingerprint_file(&config.module_path, &token).unwrap(),
        config.module_sha256
    );
}
