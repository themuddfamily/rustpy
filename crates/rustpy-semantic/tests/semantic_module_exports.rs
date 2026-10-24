use rustpy_semantic::{db::Analysis, features, imports, infer::Inferer, TextSize};
use std::path::Path;
fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}
fn open(db: &Analysis, root: &Path, text: &str) -> std::sync::Arc<rustpy_semantic::FileInfo> {
    let id = db.set_file_text(&root.join("consumer.py"), text);
    db.file(id).unwrap()
}
fn labels(db: &Analysis, file: &std::sync::Arc<rustpy_semantic::FileInfo>) -> Vec<String> {
    features::complete(db, file, TextSize::of(&*file.text))
        .into_iter()
        .map(|item| item.label.to_string())
        .collect()
}
#[test]
fn exact_all_governs_star_inference_navigation_diagnostics_and_completion() {
    let temp = tempfile::TempDir::with_prefix("semantic-all-exact").unwrap();
    let api = temp.path().join("api.py");
    write(
        &api,
        "_private = 11\npublic = 22\nleaked = 33\n__all__ = ['_private', 'public']\n",
    );
    let db = Analysis::for_project(temp.path());
    let file = open(
        &db,
        temp.path(),
        "from api import *\n_private\npublic\nleaked\n",
    );
    let inf = Inferer::new(&db);
    for name in ["_private", "public"] {
        assert!(inf
            .resolve_name(&file, file.scopes.module_scope(), name)
            .is_some());
    }
    assert!(inf
        .resolve_name(&file, file.scopes.module_scope(), "leaked")
        .is_none());
    let diagnostics = features::diagnostics(&db, &file);
    assert!(diagnostics.iter().any(|d| d.message.contains("leaked")));
    assert!(!diagnostics
        .iter()
        .any(|d| d.message.contains("_private") || d.message.contains("'public'")));
    let locations = features::definition_at(
        &db,
        &file,
        TextSize::new(file.text.find("_private").unwrap() as u32),
    );
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].path, api);
    let completion = open(&db, temp.path(), "from api import *\n");
    let items = labels(&db, &completion);
    assert!(items.contains(&"_private".to_owned()));
    assert!(items.contains(&"public".to_owned()));
    assert!(!items.contains(&"leaked".to_owned()));
}
#[test]
fn default_star_excludes_private_but_explicit_named_import_remains_valid() {
    let temp = tempfile::TempDir::with_prefix("semantic-all-default").unwrap();
    let api = temp.path().join("api.py");
    write(&api, "_private = 1\npublic = 2\n");
    let db = Analysis::for_project(temp.path());
    let file = open(&db, temp.path(), "from api import *\n");
    let inf = Inferer::new(&db);
    assert!(inf
        .resolve_name(&file, file.scopes.module_scope(), "_private")
        .is_none());
    assert!(inf
        .resolve_name(&file, file.scopes.module_scope(), "public")
        .is_some());
    let items = labels(&db, &file);
    assert!(items.contains(&"public".to_owned()));
    assert!(!items.contains(&"_private".to_owned()));
    let explicit = open(&db, temp.path(), "from api import _private\n_private\n");
    let locations = features::definition_at(
        &db,
        &explicit,
        TextSize::new(explicit.text.rfind("_private").unwrap() as u32),
    );
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].path, api);
}
#[test]
fn alias_mutation_and_annotations_feed_actual_unused_import_facts() {
    let temp = tempfile::TempDir::with_prefix("semantic-all-usage").unwrap();
    let db = Analysis::for_project(temp.path());
    let file = open(&db, temp.path(), "import os\nimport json\nimport unused\n__all__: list[str] = ['os'] + ['json']\nalias = __all__\nalias.remove('os')\n");
    assert_eq!(imports::dunder_all(&file), vec!["json"]);
    let unused: Vec<_> = imports::unused_import_candidates(&file)
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(unused, vec!["os", "unused"]);
    let partial = open(
        &db,
        temp.path(),
        "import os\nimport json\n__all__ = build('not_an_export')\n",
    );
    assert!(
        imports::dunder_all(&partial).is_empty(),
        "unknown calls must not mine arbitrary descendant strings"
    );
    assert!(
        imports::unused_import_candidates(&partial).is_empty(),
        "unknown exports cannot justify deleting potential re-exports"
    );
}
#[test]
fn conditional_partial_all_never_excludes_possible_star_names() {
    let temp = tempfile::TempDir::with_prefix("semantic-all-partial").unwrap();
    write(&temp.path().join("api.py"), "public = 1\n_private = 2\nother = 3\n__all__ = ['public']\nif unknown:\n    __all__.append('_private')\n");
    let db = Analysis::for_project(temp.path());
    let file = open(&db, temp.path(), "from api import *\n");
    let inf = Inferer::new(&db);
    for name in ["public", "_private", "other"] {
        assert!(
            inf.resolve_name(&file, file.scopes.module_scope(), name)
                .is_some(),
            "partial exports cannot disprove {name}"
        );
    }
    let items = labels(&db, &file);
    for name in ["public", "_private", "other"] {
        assert!(items.contains(&name.to_owned()));
    }
}
#[test]
fn transitive_star_reexports_and_cycles_are_bounded_and_dependency_changes_refresh() {
    let temp = tempfile::TempDir::with_prefix("semantic-all-reexports").unwrap();
    let first = temp.path().join("first.py");
    let second = temp.path().join("second.py");
    write(
        &first,
        "from second import *\nown = 'λ'\n__all__ = ['own', 'value']\n",
    );
    write(
        &second,
        "from first import *\nvalue = 1\nhidden = 2\n__all__ = ['value']\n",
    );
    let db = Analysis::for_project(temp.path());
    let file = open(&db, temp.path(), "from first import *\n");
    let inf = Inferer::new(&db);
    assert!(inf
        .resolve_name(&file, file.scopes.module_scope(), "value")
        .is_some());
    assert!(inf
        .resolve_name(&file, file.scopes.module_scope(), "hidden")
        .is_none());
    assert!(inf
        .resolve_name(&file, file.scopes.module_scope(), "missing")
        .is_none());
    let items = labels(&db, &file);
    assert!(items.contains(&"value".to_owned()));
    assert!(!items.contains(&"hidden".to_owned()));
    write(&second, "from first import *\nvalue = 1\n__all__ = []\n");
    db.invalidate_path(&second);
    let inf = Inferer::new(&db);
    assert!(inf
        .resolve_name(&file, file.scopes.module_scope(), "value")
        .is_none());
    assert!(!labels(&db, &file).contains(&"value".to_owned()));
}
#[test]
fn package_child_named_by_all_uses_real_module_identity_and_navigation() {
    let temp = tempfile::TempDir::with_prefix("semantic-all-child").unwrap();
    write(
        &temp.path().join("pkg/__init__.py"),
        "__all__ = ['child']\n",
    );
    let child = temp.path().join("pkg/child.py");
    write(&child, "π = 1\n");
    let db = Analysis::for_project(temp.path());
    let file = open(&db, temp.path(), "from pkg import *\nchild\n");
    let items = labels(&db, &open(&db, temp.path(), "from pkg import *\n"));
    assert!(items.contains(&"child".to_owned()));
    let file = open(&db, temp.path(), &file.text);
    let locations = features::definition_at(
        &db,
        &file,
        TextSize::new(file.text.rfind("child").unwrap() as u32),
    );
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].path, child);
    assert!(!features::diagnostics(&db, &file)
        .iter()
        .any(|d| d.message.contains("'child'")));
    let member = open(&db, temp.path(), "from pkg import *\nchild.π\n");
    let locations = features::definition_at(
        &db,
        &member,
        TextSize::new(member.text.rfind('π').unwrap() as u32),
    );
    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].path, child);
}
