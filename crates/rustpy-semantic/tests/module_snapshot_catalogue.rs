use rustpy_semantic::{db::Analysis, module::ModuleResolver};

#[test]
fn unsaved_project_modules_replace_bundled_providers_live_and_in_snapshot() {
    let root = std::path::Path::new("/unsaved-provider-authority");
    // Equal project/search roots are still project source, not an interpreter
    // library merely because imports also search that root.
    let db = Analysis::new(ModuleResolver::new(
        vec![root.to_path_buf()],
        vec![root.to_path_buf()],
        (3, 12),
    ));
    let mut paths = Vec::new();
    for name in ["random", "requests"] {
        assert!(db.module(name).unwrap().path.starts_with("<typeshed>"));
        let path = root.join(format!("{name}.py"));
        db.set_file_text(&path, "local_authority = True\n");
        assert_eq!(db.module(name).unwrap().path, path);
        paths.push(path);
    }
    let snapshot = db
        .snapshot(&paths, &rustpy_core::CancellationToken::new())
        .unwrap();
    for (name, path) in ["random", "requests"].into_iter().zip(paths) {
        let captured = snapshot.analysis().module(name).unwrap();
        assert_eq!(captured.path, path);
        assert_eq!(&*captured.text, "local_authority = True\n");
    }
    assert!(snapshot.complete());
}

#[test]
fn opening_nested_library_source_does_not_replace_bundled_types() {
    let temp = tempfile::TempDir::with_prefix("nested-library-buffer-provider").unwrap();
    let site = temp.path().join(".venv/site-packages");
    let package = site.join("requests");
    std::fs::create_dir_all(&package).unwrap();
    let source = package.join("__init__.py");
    std::fs::write(&source, "runtime_value = 1\n").unwrap();
    let db = Analysis::new(ModuleResolver::new(
        vec![temp.path().to_path_buf()],
        vec![site],
        (3, 12),
    ));
    assert!(db
        .module("requests")
        .unwrap()
        .path
        .starts_with("<typeshed>"));
    db.set_file_text(&source, "runtime_value = 2\n");
    assert!(db
        .module("requests")
        .unwrap()
        .path
        .starts_with("<typeshed>"));
    assert_eq!(
        &*db.file_for_path(&source).unwrap().text,
        "runtime_value = 2\n"
    );
    let snapshot = db
        .snapshot(&[source], &rustpy_core::CancellationToken::new())
        .unwrap();
    assert!(snapshot
        .analysis()
        .module("requests")
        .unwrap()
        .path
        .starts_with("<typeshed>"));
    assert!(snapshot.complete());
}
#[test]
fn frozen_completion_catalogue_omits_primed_failed_and_excluded_imports() {
    let root = tempfile::TempDir::with_prefix("snapshot-catalogue").unwrap();
    let pkg = root.path().join("pkg");
    std::fs::create_dir(&pkg).unwrap();
    let visible = root.path().join("visible.py");
    let hidden = root.path().join("hidden.py");
    let init = pkg.join("__init__.py");
    let child = pkg.join("visible.py");
    let hidden_child = pkg.join("hidden.py");
    for path in [&visible, &hidden, &init, &child, &hidden_child] {
        std::fs::write(path, "value = 1\n").unwrap();
    }
    let db = Analysis::new(
        ModuleResolver::new(vec![root.path().to_path_buf()], vec![], (3, 12))
            .with_excluded([hidden, hidden_child]),
    );
    assert!(db.resolver.resolve("hidden").is_none());
    assert!(db.resolver.resolve("missing").is_none());
    assert!(db.resolver.resolve("pkg.hidden").is_none());
    assert!(db.resolver.resolve("pkg.missing").is_none());
    assert!(db.resolver.resolve("visible").is_some());
    assert!(db.resolver.resolve("pkg.visible").is_some());
    let snapshot = db
        .snapshot(
            &[visible, init, child],
            &rustpy_core::CancellationToken::new(),
        )
        .unwrap();
    let resolver = &snapshot.analysis().resolver;
    let top = resolver.top_level_modules();
    let children = resolver.submodules("pkg");
    assert!(top.contains(&"visible".into()));
    assert!(children.contains(&"visible".into()));
    assert!(
        !top.contains(&"hidden".into()),
        "cached excluded result is not an importable module"
    );
    assert!(
        !top.contains(&"missing".into()),
        "cached failed result is not an importable module"
    );
    assert!(
        !children.contains(&"hidden".into()),
        "cached excluded child is not an importable module"
    );
    assert!(
        !children.contains(&"missing".into()),
        "cached failed child is not an importable module"
    );
}
