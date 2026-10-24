use rustpy_semantic::{db::Analysis, module::ModuleResolver};
#[test]
fn attached_interpreter_libraries_survive_excluded_project_venv_with_nested_exclusion_respected() {
    let temp = tempfile::TempDir::with_prefix("module-library-role").unwrap();
    let root = temp.path();
    let venv = root.join(".venv");
    let site = venv.join("lib/python3.12/site-packages");
    let package = site.join("dependency");
    let excluded = package.join("internal");
    std::fs::create_dir_all(&excluded).unwrap();
    std::fs::create_dir_all(root.join("vendor")).unwrap();
    let api = package.join("api.py");
    let main = root.join("main.py");
    for file in [
        package.join("__init__.py"),
        api.clone(),
        excluded.join("hidden.py"),
        root.join("vendor/hidden.py"),
        main.clone(),
    ] {
        std::fs::write(file, "value = 1\n").unwrap();
    }
    let db = Analysis::new(
        ModuleResolver::new(
            vec![root.to_path_buf()],
            vec![root.to_path_buf(), site.clone()],
            (3, 12),
        )
        .with_excluded([venv.clone(), root.join("vendor"), excluded.clone()]),
    );
    assert!(db.resolver.resolve("dependency.api").is_some());
    assert_eq!(
        db.resolver.resolve_source("dependency.api"),
        Some(api.clone())
    );
    assert_eq!(
        db.resolver.module_name_of(&api).as_deref(),
        Some("dependency.api")
    );
    assert!(db
        .resolver
        .top_level_modules()
        .contains(&"dependency".into()));
    assert!(db.resolver.submodules("dependency").contains(&"api".into()));
    assert!(db.resolver.resolve("dependency.internal.hidden").is_none());
    assert!(!db
        .resolver
        .submodules("dependency")
        .contains(&"internal".into()));
    assert!(
        db.resolver.resolve("vendor.hidden").is_none(),
        "broad search root equal project root does not exempt vendor"
    );
    db.set_file_text(&api, "unsaved = 2\n");
    assert!(
        db.module("dependency.api").is_some(),
        "cached editor library retains module authority"
    );
    let snapshot = db
        .snapshot(&[main, api.clone()], &rustpy_core::CancellationToken::new())
        .unwrap();
    assert!(!snapshot.analysis().resolver.is_excluded(&api));
    assert!(snapshot.analysis().module("dependency.api").is_some());
    assert!(snapshot
        .analysis()
        .resolver
        .is_excluded(&excluded.join("hidden.py")));
    assert!(snapshot
        .analysis()
        .resolver
        .is_excluded(&venv.join("project_only.py")));
}
