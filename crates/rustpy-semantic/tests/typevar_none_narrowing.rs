use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn branch_type(declaration: &str) -> String {
    let source = format!("from typing import TypeVar,Any\n{declaration}\ndef f(x:T):\n    if x is None:\n        x\n");
    let db = Analysis::for_project(Path::new("/declared-none"));
    let id = db.set_file_text(Path::new("/declared-none/m.py"), &source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let name = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .filter(|name| name.name_text() == "x")
        .last()
        .unwrap();
    Inferer::new(&db)
        .type_of_expr(&file, &ast::Expr::Name(name))
        .to_string()
}
fn rejects_none(declaration: &str) {
    let source = format!("from typing import TypeVar\n{declaration}\ndef f(x:T):pass\n");
    let db = Analysis::for_project(Path::new("/declared-none-premise"));
    let id = db.set_file_text(Path::new("/declared-none-premise/m.py"), &source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let function = file
        .parse
        .syntax_node()
        .descendants()
        .find_map(ast::FuncDef::cast)
        .unwrap();
    let inf = Inferer::new(&db);
    let signature = inf.signature(&file, &function, None);
    assert_eq!(signature.params.len(), 1);
    assert!(matches!(&signature.params[0].ty, Type::DeclaredTypeVar(_)));
    assert!(!inf.is_compatible(&Type::None, &signature.params[0].ty));
}
#[test]
fn unbounded_declared_variable_admits_none() {
    assert_eq!(branch_type("T=TypeVar('T')"), "None");
}
#[test]
fn non_none_bound_and_constraints_exclude_none() {
    rejects_none("T=TypeVar('T',bound=str)");
    rejects_none("T=TypeVar('T',str,bytes)");
    // Impossible branches keep their declared presentation, as ordinary str does.
    assert_eq!(branch_type("T=str"), "str");
    assert_eq!(branch_type("T=TypeVar('T',bound=str)"), "T");
    assert_eq!(branch_type("T=TypeVar('T',str,bytes)"), "T");
}
#[test]
fn optional_bound_and_constraint_admit_none() {
    assert_eq!(branch_type("T=TypeVar('T',bound=str|None)"), "None");
    assert_eq!(branch_type("T=TypeVar('T',str,None)"), "None");
}
#[test]
fn object_and_any_bounds_admit_none() {
    assert_eq!(branch_type("T=TypeVar('T',bound=object)"), "None");
    assert_eq!(branch_type("T=TypeVar('T',bound=Any)"), "None");
}
