use rustpy_semantic::{db::Analysis, infer::Inferer};
use std::path::Path;
fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}
fn allowed(db: &Analysis, root: &Path, module: &str, name: &str) -> bool {
    let id = db.set_file_text(
        &root.join("consumer.py"),
        &format!("from {module} import *\n{name}\n"),
    );
    let file = db.file(id).unwrap();
    Inferer::new(db)
        .resolve_name(&file, file.scopes.module_scope(), name)
        .is_some()
}
#[test]
fn imported_all_and_typed_constants_restrict_actual_star_bindings() {
    let t = tempfile::TempDir::with_prefix("cross-all-typed").unwrap();
    write(
        t.path(),
        "names.py",
        "NAME = '_private'\nEXPORTED = ('public',)\n",
    );
    write(t.path(), "api.py", "from names import NAME as chosen, EXPORTED as exported\n_private=1\npublic=2\nleaked=3\n__all__ = (chosen,) + exported\n");
    let db = Analysis::for_project(t.path());
    assert!(allowed(&db, t.path(), "api", "_private"));
    assert!(allowed(&db, t.path(), "api", "public"));
    assert!(!allowed(&db, t.path(), "api", "leaked"));
}
#[test]
fn relative_and_dotted_module_aliases_keep_real_parent_authority() {
    let t = tempfile::TempDir::with_prefix("cross-all-relative").unwrap();
    write(t.path(), "pkg/names.py", "EXPORTS=['visible']\n");
    write(
        t.path(),
        "pkg/__init__.py",
        "from .names import EXPORTS\nvisible=1\nleaked=2\n__all__=EXPORTS\n",
    );
    write(
        t.path(),
        "api.py",
        "import pkg.names as names\nvisible=1\nleaked=2\n__all__=names.EXPORTS\n",
    );
    let db = Analysis::for_project(t.path());
    for module in ["pkg", "api"] {
        assert!(allowed(&db, t.path(), module, "visible"));
        assert!(!allowed(&db, t.path(), module, "leaked"));
    }
}
#[test]
fn later_shadow_does_not_retarget_earlier_export_expression() {
    let t = tempfile::TempDir::with_prefix("cross-all-order").unwrap();
    write(t.path(), "names.py", "EXPORTS=['visible']\n");
    write(
        t.path(),
        "api.py",
        "import names\nvisible=1\nleaked=2\n__all__=names.EXPORTS\nnames=42\n",
    );
    let db = Analysis::for_project(t.path());
    assert!(allowed(&db, t.path(), "api", "visible"));
    assert!(!allowed(&db, t.path(), "api", "leaked"));
    // An earlier ordinary binding is not borrowed as import authority.
    db.set_file_text(
        &t.path().join("api.py"),
        "import names\nnames=object()\nvisible=1\nleaked=2\n__all__=names.EXPORTS\n",
    );
    assert!(allowed(&db, t.path(), "api", "leaked"));
}
#[test]
fn dependency_changes_and_cycles_do_not_reuse_exact_export_answers() {
    let t = tempfile::TempDir::with_prefix("cross-all-dependency").unwrap();
    write(t.path(), "names.py", "EXPORTS=['old']\n");
    write(
        t.path(),
        "api.py",
        "from names import EXPORTS\nold=1\nnew=2\n__all__=EXPORTS\n",
    );
    let db = Analysis::for_project(t.path());
    assert!(allowed(&db, t.path(), "api", "old"));
    assert!(!allowed(&db, t.path(), "api", "new"));
    db.set_file_text(&t.path().join("names.py"), "EXPORTS=['new']\n");
    assert!(!allowed(&db, t.path(), "api", "old"));
    assert!(allowed(&db, t.path(), "api", "new"));
    db.set_file_text(
        &t.path().join("names.py"),
        "from api import __all__ as EXPORTS\n",
    );
    // Cyclic authority is partial, never an exact empty list hiding valid names.
    assert!(allowed(&db, t.path(), "api", "old"));
    assert!(allowed(&db, t.path(), "api", "new"));
}

#[test]
fn assignment_rhs_uses_previous_import_but_unknown_branch_is_partial() {
    let t = tempfile::TempDir::with_prefix("cross-all-assignment").unwrap();
    write(t.path(), "names.py", "EXPORTS=['visible']\n");
    write(
        t.path(),
        "api.py",
        "import names\nvisible=1\nleaked=2\nnames=names.EXPORTS\n__all__=names\n",
    );
    let db = Analysis::for_project(t.path());
    assert!(allowed(&db, t.path(), "api", "visible"));
    assert!(!allowed(&db, t.path(), "api", "leaked"));
    db.set_file_text(
        &t.path().join("api.py"),
        "if flag:\n    import names\nvisible=1\nleaked=2\n__all__=names.EXPORTS\n",
    );
    assert!(allowed(&db, t.path(), "api", "leaked"));
}

#[test]
fn package_attribute_requires_actual_child_import_not_disk_existence() {
    let t = tempfile::TempDir::with_prefix("cross-all-child-authority").unwrap();
    write(t.path(), "pkg/__init__.py", "");
    write(t.path(), "pkg/names.py", "EXPORTS=['visible']\n");
    write(
        t.path(),
        "api.py",
        "import pkg\nvisible=1\nleaked=2\n__all__=pkg.names.EXPORTS\n",
    );
    let db = Analysis::for_project(t.path());
    assert!(allowed(&db, t.path(), "api", "leaked"));
    db.set_file_text(
        &t.path().join("api.py"),
        "import pkg.names\nvisible=1\nleaked=2\n__all__=pkg.names.EXPORTS\n",
    );
    assert!(allowed(&db, t.path(), "api", "visible"));
    assert!(!allowed(&db, t.path(), "api", "leaked"));
    db.set_file_text(
        &t.path().join("api.py"),
        "from pkg import names\nvisible=1\nleaked=2\n__all__=names.EXPORTS\n",
    );
    assert!(!allowed(&db, t.path(), "api", "leaked"));
    db.set_file_text(&t.path().join("pkg/__init__.py"), "from . import names\n");
    db.set_file_text(
        &t.path().join("api.py"),
        "import pkg\nvisible=1\nleaked=2\n__all__=pkg.names.EXPORTS\n",
    );
    assert!(!allowed(&db, t.path(), "api", "leaked"));
}
