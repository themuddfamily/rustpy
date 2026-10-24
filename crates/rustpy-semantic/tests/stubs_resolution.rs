//! Whole Stubs feature: package ownership, explicit roots, and bundled fallback.
use rustpy_semantic::module::{ModuleResolver, ModuleSource};
use std::path::Path;

fn write(root: &Path, relative: &str, text: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}
fn file(resolver: &ModuleResolver, name: &str) -> std::path::PathBuf {
    match resolver.resolve(name).expect("module must resolve") {
        ModuleSource::File { path, .. } => path,
        other => panic!("expected concrete source, got {other:?}"),
    }
}
#[test]
fn complete_stub_package_owns_missing_children_partial_package_merges_runtime() {
    let directory = tempfile::TempDir::with_prefix("rustpy-stubs-packages").unwrap();
    let root = directory.path();
    write(root, "demo-stubs/__init__.pyi", "name: str\n");
    write(root, "demo-stubs/present.pyi", "value: int\n");
    write(root, "demo/__init__.py", "name=1\n");
    write(root, "demo/missing.py", "value=1\n");
    let resolver = ModuleResolver::new(vec![], vec![root.to_path_buf()], (3, 12));
    assert_eq!(
        file(&resolver, "demo"),
        root.join("demo-stubs/__init__.pyi")
    );
    assert_eq!(
        file(&resolver, "demo.present"),
        root.join("demo-stubs/present.pyi")
    );
    assert!(resolver.resolve("demo.missing").is_none());
    write(root, "demo-stubs/py.typed", "partial\n");
    resolver.invalidate();
    assert_eq!(
        file(&resolver, "demo.missing"),
        root.join("demo/missing.py")
    );
    assert_eq!(
        file(&resolver, "demo.present"),
        root.join("demo-stubs/present.pyi")
    );
}
#[test]
fn explicit_stub_roots_precede_project_and_sibling_pyi_precedes_py() {
    let directory = tempfile::TempDir::with_prefix("rustpy-user-stubs").unwrap();
    let root = directory.path();
    write(root, "project/demo.py", "value=1\n");
    write(root, "project/demo.pyi", "value: str\n");
    write(root, "custom/demo.pyi", "value: bytes\n");
    let resolver = ModuleResolver::new(vec![root.join("project")], vec![], (3, 12));
    assert_eq!(file(&resolver, "demo"), root.join("project/demo.pyi"));
    let resolver = resolver.with_stub_roots([root.join("custom")]);
    assert_eq!(file(&resolver, "demo"), root.join("custom/demo.pyi"));
    assert_eq!(
        resolver
            .module_name_of(&root.join("custom/demo.pyi"))
            .as_deref(),
        Some("demo")
    );
}
#[test]
fn third_party_bundle_types_untyped_runtime_but_respects_inline_types() {
    let directory = tempfile::TempDir::with_prefix("rustpy-third-party-stubs").unwrap();
    let root = directory.path();
    let resolver = ModuleResolver::new(vec![], vec![root.to_path_buf()], (3, 12));
    assert!(
        matches!(resolver.resolve("requests"), Some(ModuleSource::Stub(name)) if name=="requests")
    );
    assert!(
        rustpy_semantic::typeshed::stub_for_module("requests.adapters")
            .unwrap()
            .contains("HTTPAdapter")
    );
    assert!(!resolver.import_available("requests"));
    write(root, "requests/__init__.py", "runtime=True\n");
    assert!(!resolver.import_available("requests"));
    resolver.invalidate();
    assert!(
        matches!(resolver.resolve("requests"), Some(ModuleSource::Stub(name)) if name == "requests")
    );
    assert!(resolver.import_available("requests"));
    write(root, "requests/py.typed", "");
    resolver.invalidate();
    assert_eq!(
        file(&resolver, "requests"),
        root.join("requests/__init__.py")
    );
    write(root, "requests/__init__.pyi", "runtime: bool\n");
    resolver.invalidate();
    assert_eq!(
        file(&resolver, "requests"),
        root.join("requests/__init__.pyi")
    );
}

#[test]
fn preloaded_interpreter_source_does_not_replace_vendored_types() {
    use rustpy_semantic::db::Analysis;
    let directory = tempfile::TempDir::with_prefix("rustpy-preloaded-provider").unwrap();
    let root = directory.path();
    write(root, "requests/__init__.py", "runtime=True\n");
    let db = Analysis::new(ModuleResolver::new(
        vec![],
        vec![root.to_path_buf()],
        (3, 12),
    ));
    let loaded = db
        .file_for_path(&root.join("requests/__init__.py"))
        .unwrap();
    assert_eq!(loaded.path, root.join("requests/__init__.py"));
    assert!(matches!(
        db.resolver.resolve("requests"),
        Some(ModuleSource::Stub(_))
    ));
    let selected = db.module("requests").unwrap();
    assert!(selected.path.starts_with("<typeshed>"));
    assert!(db.import_module("requests").is_some());
}
#[test]
fn custom_stub_wins_open_project_buffer_but_selected_stub_edits_are_visible() {
    use rustpy_semantic::db::Analysis;
    let directory = tempfile::TempDir::with_prefix("rustpy-open-provider").unwrap();
    let root = directory.path();
    write(root, "custom/sample.pyi", "value: str\n");
    let db = Analysis::new(
        ModuleResolver::new(vec![root.join("project")], vec![], (3, 12))
            .with_stub_roots([root.join("custom")]),
    );
    db.set_file_text(&root.join("project/sample.py"), "value=1\n");
    assert_eq!(
        db.module("sample").unwrap().path,
        root.join("custom/sample.pyi")
    );
    db.set_file_text(&root.join("custom/sample.pyi"), "value: bytes\n");
    assert_eq!(&*db.module("sample").unwrap().text, "value: bytes\n");
    db.set_file_text(&root.join("project/unsaved.py"), "value=7\n");
    assert_eq!(&*db.import_module("unsaved").unwrap().text, "value=7\n");
    let snapshot = db
        .snapshot(
            &[
                root.join("project/unsaved.py"),
                root.join("custom/sample.pyi"),
            ],
            &rustpy_core::CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(
        &*snapshot.analysis().import_module("unsaved").unwrap().text,
        "value=7\n"
    );
    assert_eq!(
        &*snapshot.analysis().module("sample").unwrap().text,
        "value: bytes\n"
    );
    assert!(snapshot.complete(), "captured editor modules are complete");
}

#[test]
fn unsaved_project_module_shadows_typed_environment_nested_inside_project() {
    use rustpy_semantic::db::Analysis;
    let directory = tempfile::TempDir::with_prefix("rustpy-nested-stub-provider").unwrap();
    let project = directory.path().join("project");
    let site = project.join(".venv/site-packages");
    write(&site, "requests/__init__.py", "installed = True\n");
    write(&site, "requests/py.typed", "");
    let db = Analysis::new(ModuleResolver::new(
        vec![project.clone()],
        vec![site],
        (3, 12),
    ));
    assert!(db
        .module("requests")
        .unwrap()
        .path
        .ends_with("requests/__init__.py"));
    db.set_file_text(&project.join("requests.py"), "local = True\n");
    assert_eq!(
        db.module("requests").unwrap().path,
        project.join("requests.py")
    );
}
