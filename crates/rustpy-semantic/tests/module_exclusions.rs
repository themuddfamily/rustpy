use rustpy_semantic::{db::Analysis, module::ModuleResolver};
use std::path::Path;
fn touch(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "value = 1\n").unwrap();
}
#[test]
fn excluded_descendants_are_not_imports_but_explicit_editor_files_stay_available() {
    let temp = tempfile::TempDir::with_prefix("module-exclusions").unwrap();
    let root = temp.path();
    let excluded = root.join("vendor");
    let file = excluded.join("hidden.py");
    touch(&file);
    touch(&root.join("vendor2/visible.py"));
    touch(&root.join("main.py"));
    let mut db = Analysis::new(ModuleResolver::new(
        vec![root.to_path_buf()],
        vec![root.to_path_buf()],
        (3, 12),
    ));
    let id = db.set_file_text(&file, "unsaved = 2\n");
    assert!(
        db.module("vendor.hidden").is_some(),
        "baseline cached module is real before exclusion"
    );
    db.resolver = ModuleResolver::new(vec![root.to_path_buf()], vec![root.to_path_buf()], (3, 12))
        .with_excluded([excluded.clone()]);
    assert!(db.resolver.resolve("vendor.hidden").is_none());
    assert!(db.resolver.resolve_source("vendor.hidden").is_none());
    assert!(db.resolver.module_name_of(&file).is_none());
    assert!(db.resolver.resolve_relative(&file, 1, "").is_none());
    assert!(db.resolver.resolve_relative(&file, 1, "hidden").is_none());
    assert!(!db.resolver.top_level_modules().contains(&"vendor".into()));
    assert!(db.resolver.submodules("vendor").is_empty());
    assert!(db.resolver.resolve("vendor2.visible").is_some());
    assert_eq!(db.file(id).unwrap().text.as_ref(), "unsaved = 2\n");
    assert!(db.module("vendor.hidden").is_none());
    assert!(db.file_for_path(&file).is_some());
    let snapshot = db
        .snapshot(
            &[root.join("main.py"), file.clone()],
            &rustpy_core::CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(snapshot.analysis().resolver.excluded_paths(), &[excluded]);
    assert!(snapshot.analysis().module("vendor.hidden").is_none());
    assert!(snapshot.analysis().file_for_path(&file).is_some());
}
#[test]
fn exclusion_builder_invalidates_prior_successful_resolution_cache() {
    let temp = tempfile::TempDir::with_prefix("module-exclusions").unwrap();
    let file = temp.path().join("hidden.py");
    touch(&file);
    let db = ModuleResolver::new(vec![temp.path().to_path_buf()], vec![], (3, 12));
    assert!(db.resolve("hidden").is_some());
    let db = db.with_excluded([file]);
    assert!(db.resolve("hidden").is_none());
    let names = db.top_level_modules();
    assert!(
        !names.contains(&"hidden".to_owned()),
        "excluded module is absent from completion catalogue"
    );
    assert!(
        names.contains(&"builtins".to_owned()),
        "bundled builtin catalogue remains available"
    );
}
