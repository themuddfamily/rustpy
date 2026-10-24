//! Original, bounded fixtures inspired by typing-spec categories. Not a claim
//! to pass the complete upstream typing conformance suite.
use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn infer(source: &str) -> String {
    let db = Analysis::for_project(Path::new("/quality-conformance"));
    let id = db.set_file_text(Path::new("/quality-conformance/main.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "invalid fixture: {source}");
    let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
    let ast::Stmt::Expr(last) = module.statements().last().unwrap() else {
        panic!("fixture must end with the expression under test")
    };
    Inferer::new(&db)
        .type_of_expr(&file, &last.expr().unwrap())
        .to_string()
}
#[test]
fn declared_function_return_is_authoritative() {
    assert_eq!(
        infer("def convert(value: int) -> str:\n    return str(value)\nconvert(1)\n"),
        "str"
    );
}
#[test]
fn inherited_constructor_preserves_receiver_type() {
    assert_eq!(infer("class Parent:\n    def __init__(self, value: int) -> None: pass\nclass Child(Parent): pass\nChild(1)\n"), "Child");
}
#[test]
fn nullable_function_return_preserves_both_alternatives() {
    let ty = infer("def maybe() -> int | None:\n    return None\nmaybe()\n");
    let alternatives: std::collections::BTreeSet<_> = ty.split(" | ").collect();
    let expected: std::collections::BTreeSet<_> = ["None", "int"].into_iter().collect();
    assert_eq!(alternatives, expected, "{ty}");
}
#[test]
fn constructor_result_respects_new_return_type() {
    assert_eq!(infer("class Product: pass\nclass Factory:\n    def __new__(cls) -> Product: return Product()\nFactory()\n"), "Product");
}
