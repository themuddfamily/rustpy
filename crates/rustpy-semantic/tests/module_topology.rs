use rustpy_semantic::module::{ModuleResolver, ModuleSource};
use std::path::{Path, PathBuf};
fn touch(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, "value = 1\n").unwrap();
}
fn path(source: Option<ModuleSource>) -> Option<PathBuf> {
    source.and_then(|s| s.path().map(Path::to_path_buf))
}
#[test]
fn later_regular_package_wins_over_earlier_namespace_portion() {
    let temp = tempfile::TempDir::with_prefix("module-topology").unwrap();
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    touch(&first.join("pkg/ghost.py"));
    touch(&second.join("pkg/__init__.py"));
    touch(&second.join("pkg/live.py"));
    let db = ModuleResolver::new(vec![first.clone()], vec![second.clone()], (3, 12));
    assert_eq!(
        path(db.resolve("pkg")),
        Some(second.join("pkg/__init__.py"))
    );
    assert_eq!(
        path(db.resolve("pkg.live")),
        Some(second.join("pkg/live.py"))
    );
    assert!(
        db.resolve("pkg.ghost").is_none(),
        "earlier namespace does not remain after regular parent wins"
    );
    assert_eq!(db.submodules("pkg"), vec!["live"]);
}
#[test]
fn regular_parent_prevents_dotted_child_leaking_from_other_root() {
    let temp = tempfile::TempDir::with_prefix("module-topology").unwrap();
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    touch(&first.join("pkg/__init__.py"));
    touch(&first.join("pkg/local.py"));
    touch(&second.join("pkg/foreign.py"));
    let db = ModuleResolver::new(vec![first.clone(), second], vec![], (3, 12));
    assert!(db.resolve("pkg.foreign").is_none());
    assert_eq!(
        path(db.resolve("pkg.local")),
        Some(first.join("pkg/local.py"))
    );
    assert!(db
        .resolve_relative(&first.join("pkg/local.py"), 1, "foreign")
        .is_none());
}
#[test]
fn namespace_children_completion_and_relative_import_use_all_portions() {
    let temp = tempfile::TempDir::with_prefix("module-topology").unwrap();
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    touch(&first.join("ns/one.py"));
    touch(&second.join("ns/two.py"));
    let db = ModuleResolver::new(vec![first.clone(), second.clone()], vec![], (3, 12));
    assert_eq!(db.submodules("ns"), vec!["one", "two"]);
    assert_eq!(
        path(db.resolve_relative(&first.join("ns/one.py"), 1, "two")),
        Some(second.join("ns/two.py"))
    );
}
#[test]
fn runtime_source_lookup_obeys_regular_parent_search_location() {
    let temp = tempfile::TempDir::with_prefix("module-topology").unwrap();
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    touch(&first.join("pkg/__init__.py"));
    touch(&second.join("pkg/foreign.py"));
    let db = ModuleResolver::new(vec![], vec![first, second], (3, 12));
    assert!(db.resolve_source("pkg.foreign").is_none());
}
