use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}
fn result(db: &Analysis, source: &Path, text: &str) -> Option<String> {
    let id = db.set_file_text(source, text);
    let file = db.file(id).unwrap();
    let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
    let ast::Stmt::Expr(last) = module.statements().last().unwrap() else {
        panic!("expression required")
    };
    match Inferer::new(db).type_of_expr(&file, &last.expr().unwrap()) {
        Type::Module(id) => db.file(id).unwrap().module_name.clone(),
        _ => None,
    }
}
#[test]
fn literal_importlib_calls_resolve_aliases_keywords_and_relative_packages() {
    let temp = tempfile::TempDir::with_prefix("dynamic-imports").unwrap();
    write(&temp.path().join("pkg/__init__.py"), "");
    write(&temp.path().join("pkg/child.py"), "π = 1\n");
    let db = Analysis::for_project(temp.path());
    let source = temp.path().join("main.py");
    for code in [
        "import importlib\nimportlib.import_module('pkg.child')\n",
        "import importlib as loader\nloader.import_module(name='pkg.child')\n",
        "from importlib import import_module as load\nload('.child', package='pkg')\n",
    ] {
        assert_eq!(
            result(&db, &source, code).as_deref(),
            Some("pkg.child"),
            "{code}"
        );
    }
}
#[test]
fn builtin_import_returns_top_package_unless_literal_fromlist_is_nonempty() {
    let temp = tempfile::TempDir::with_prefix("dynamic-builtin").unwrap();
    write(&temp.path().join("pkg/__init__.py"), "");
    write(&temp.path().join("pkg/child.py"), "value = 1\n");
    let db = Analysis::for_project(temp.path());
    let source = temp.path().join("main.py");
    assert_eq!(
        result(&db, &source, "__import__('pkg.child')\n").as_deref(),
        Some("pkg")
    );
    assert_eq!(
        result(
            &db,
            &source,
            "from builtins import __import__ as load\nload('pkg.child', fromlist=['value'])\n"
        )
        .as_deref(),
        Some("pkg.child")
    );
    assert_eq!(
        result(&db, &source, "__import__('pkg.child', fromlist=())\n").as_deref(),
        Some("pkg")
    );
    assert_eq!(result(&db, &source, "__import__('pkg.missing')\n"), None);
}
#[test]
fn shadows_unknown_arguments_and_unknown_relative_context_never_claim_modules() {
    let temp = tempfile::TempDir::with_prefix("dynamic-shadows").unwrap();
    write(&temp.path().join("pkg/__init__.py"), "");
    let mut db = Analysis::for_project(temp.path());
    let source = temp.path().join("main.py");
    for code in [
        "def __import__(name):\n    return 1\n__import__('pkg')\n",
        "from importlib import import_module\nimport_module = lambda name: 1\nimport_module('pkg')\n",
        "import importlib\nname = 'pkg'\nimportlib.import_module(name)\n",
        "import importlib\nimportlib.import_module('.child', package=unknown)\n",
        "__import__('pkg', fromlist=unknown)\n",
        "__import__('pkg', level=1)\n",
        "import importlib\nimportlib.import_module(*['pkg'])\n",
        "import importlib\nimportlib.import_module(b'pkg')\n",
        "__import__(b'pkg')\n",
        "__import__('pkg', None, None, (), 0, level=1)\n",
        "__import__('pkg', None, globals={})\n",
        "import importlib\nimportlib.import_module('pkg', 'unused', package='other')\n",
    ] { assert_eq!(result(&db, &source, code), None, "{code}"); }
    write(
        &temp.path().join("importlib.py"),
        "def import_module(name):\n    return 1\n",
    );
    db.invalidate_path(&temp.path().join("importlib.py"));
    db.resolver.search_paths.push(temp.path().to_path_buf());
    assert_eq!(
        result(
            &db,
            &source,
            "import importlib\nimportlib.import_module('pkg')\n"
        ),
        None
    );
}
#[test]
fn dynamic_import_answers_follow_actual_dependency_creation_changes_and_deletion() {
    let temp = tempfile::TempDir::with_prefix("dynamic-invalidation").unwrap();
    let db = Analysis::for_project(temp.path());
    let source = temp.path().join("main.py");
    let dependency = temp.path().join("dependency.py");
    let code = "import importlib\nimportlib.import_module('dependency')\n";
    assert_eq!(result(&db, &source, code), None);
    write(&dependency, "value = 1\n");
    db.invalidate_path(&dependency);
    assert_eq!(result(&db, &source, code).as_deref(), Some("dependency"));
    let module = db.module("dependency").unwrap();
    assert!(Inferer::new(&db)
        .resolve_in_module(&module, "value", 0)
        .is_some());
    write(&dependency, "changed = 2\n");
    db.invalidate_path(&dependency);
    let module = db.module("dependency").unwrap();
    assert!(Inferer::new(&db)
        .resolve_in_module(&module, "value", 0)
        .is_none());
    assert!(Inferer::new(&db)
        .resolve_in_module(&module, "changed", 0)
        .is_some());
    std::fs::remove_file(&dependency).unwrap();
    db.invalidate_path(&dependency);
    assert_eq!(result(&db, &source, code), None);
}
