//! Feature fixtures; execution is deferred to the final verification stage.
use rustpy_packages::project::*;
use std::path::Path;

#[test]
fn manifests_merge_pep621_poetry_and_requirements_without_running_python() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("pyproject.toml"),
        r#"
[project]
dependencies = ["requests>=2.30", "rich; python_version >= '3.9'"]
[tool.poetry.dependencies]
python = "^3.11"
attrs = "^0.4.1"
httpx = {version="~0.27.1", extras=["http2"]}
"#,
    )
    .unwrap();
    std::fs::write(
        root.path().join("requirements.txt"),
        "pytest>=8\nrequests>=2.30\n",
    )
    .unwrap();
    let manifest = read_manifest(root.path());
    assert_eq!(manifest.manager, Manager::Poetry);
    assert_eq!(manifest.dependencies.len(), 5);
    assert!(manifest
        .dependencies
        .iter()
        .any(|r| r.raw == "attrs>=0.4.1,<0.5"));
    assert!(manifest
        .dependencies
        .iter()
        .any(|r| r.raw == "httpx[http2]>=0.27.1,<0.28"));
    assert!(manifest.warnings.is_empty());
}

#[test]
fn every_manager_produces_a_streamable_plan_with_explicit_target() {
    let root = tempfile::tempdir().unwrap();
    let interpreter = Path::new("/project/.venv/bin/python");
    for manager in Manager::ALL {
        let preferences = Preferences {
            manager,
            ..Default::default()
        };
        let plan = command_plan(
            root.path(),
            interpreter,
            &preferences,
            &Operation::Install(vec!["requests>=2".into()]),
        )
        .unwrap();
        assert_eq!(plan.cwd, root.path());
        assert!(plan.args.iter().any(|arg| arg == "requests>=2"));
        assert!(!plan.args.iter().any(|arg| arg == "sh" || arg == "-c"));
    }
    let preferences = Preferences {
        manager: Manager::Conda,
        conda_channels: vec!["conda-forge".into()],
        ..Default::default()
    };
    let plan = command_plan(
        root.path(),
        interpreter,
        &preferences,
        &Operation::Upgrade(vec!["numpy".into()]),
    )
    .unwrap();
    assert_eq!(
        plan.args,
        vec![
            "update",
            "--yes",
            "--prefix",
            "/project/.venv",
            "-c",
            "conda-forge",
            "numpy"
        ]
    );
}

#[test]
fn lock_updates_prompt_only_after_baseline_and_uv_sync_preserves_lock() {
    let root = tempfile::tempdir().unwrap();
    let lock = root.path().join("uv.lock");
    std::fs::write(&lock, "version = 1\n").unwrap();
    let mut watcher = LockWatcher::default();
    assert!(watcher.poll(root.path()).is_empty());
    std::fs::write(&lock, "version = 2\n").unwrap();
    assert_eq!(watcher.poll(root.path()), vec![lock]);
    assert!(watcher.poll(root.path()).is_empty());
    let plan = command_plan(
        root.path(),
        Path::new("/p/bin/python"),
        &Preferences::default(),
        &Operation::Sync,
    )
    .unwrap();
    assert_eq!(plan.program, Path::new("uv"));
    assert_eq!(
        plan.args,
        vec!["sync", "--active", "--frozen", "--python", "/p/bin/python"]
    );
}

#[test]
fn setup_creation_never_overwrites_and_build_uses_selected_interpreter() {
    let root = tempfile::tempdir().unwrap();
    let path = create_setup(root.path()).unwrap();
    let source = std::fs::read_to_string(&path).unwrap();
    assert!(create_setup(root.path()).is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), source);
    let plan = command_plan(
        root.path(),
        Path::new("/chosen/python"),
        &Preferences::default(),
        &Operation::Build,
    )
    .unwrap();
    assert_eq!(plan.program, Path::new("/chosen/python"));
    assert_eq!(plan.args, vec!["-u", "-m", "build"]);
    assert!(command_plan(
        root.path(),
        Path::new("python"),
        &Preferences::default(),
        &Operation::Setup("arbitrary; command".into())
    )
    .is_err());
}

#[test]
fn repository_preferences_keep_secret_references_out_of_argv() {
    let root = tempfile::tempdir().unwrap();
    let preferences = Preferences {
        manager: Manager::Pip,
        repositories: vec![Repository {
            url: "https://packages.example/simple".into(),
            trusted_host: "packages.example".into(),
            ..Default::default()
        }],
        prereleases: true,
        user: true,
        ..Default::default()
    };
    preferences.save(root.path()).unwrap();
    let preferences = Preferences::load(root.path());
    let plan = command_plan(
        root.path(),
        Path::new("python"),
        &preferences,
        &Operation::Install(vec!["example".into()]),
    )
    .unwrap();
    assert!(plan.args.contains(&"--pre".into()) && plan.args.contains(&"--user".into()));
    assert!(!plan.args.iter().any(|arg| arg.contains("packages.example")));
    assert!(plan.env.contains(&(
        "PIP_EXTRA_INDEX_URL".into(),
        "https://packages.example/simple".into()
    )));
}

#[test]
fn search_ranks_exact_names_then_prefixes_then_substrings() {
    let names = vec![
        "my-requests-wrapper".into(),
        "Requests".into(),
        "requests-toolbelt".into(),
        "other".into(),
    ];
    assert_eq!(
        rustpy_packages::pypi::rank_search_names("requests", &names, 10),
        vec!["Requests", "requests-toolbelt", "my-requests-wrapper"]
    );
}
