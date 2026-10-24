//! Public return inference reads explicit external-cell stores in the executing scope.
use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn inferred(source: &str) -> String {
    let db = Analysis::for_project(Path::new("/external-cell-return"));
    let id = db.set_file_text(Path::new("/external-cell-return/main.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
    let ast::Stmt::Expr(expr) = module.statements().last().unwrap() else {
        panic!("expression fixture")
    };
    Inferer::new(&db)
        .type_of_expr(&file, &expr.expr().unwrap())
        .to_string()
}
#[test]
fn nonlocal_store_changes_actual_function_return_type() {
    assert_eq!(
        inferred(
            "def outer():\n    value='old'\n    def inner():\n        nonlocal value\n        value=1\n        return value\n    return inner()\nouter()\n"
        ),
        "int"
    );
}
#[test]
fn global_store_changes_actual_callable_return_type() {
    assert_eq!(
        inferred(
            "from typing import Never\ndef halt()->Never: raise RuntimeError\ndef normal()->int: return 1\nstop=halt\ndef factory():\n    global stop\n    stop=normal\n    return stop\nfactory()()\n"
        ),
        "int"
    );
}
#[test]
fn unexecuted_nested_writer_does_not_change_outer_read() {
    assert_eq!(
        inferred(
            "def outer():\n    value='old'\n    def inner():\n        nonlocal value\n        value=1\n        return value\n    return value\nouter()\n"
        ),
        "str"
    );
}
#[test]
fn global_store_targets_module_cell_despite_nested_same_name() {
    assert_eq!(
        inferred(
            "value='module'\ndef outer():\n    value=False\n    def inner():\n        global value\n        value=1\n        return value\n    return inner()\nouter()\n"
        ),
        "int"
    );
}
