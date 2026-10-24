//! Public producer premise is mandatory before a Flow completion assertion.
use rustpy_semantic::{db::Analysis, flow, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

fn check(source: &str, expression_text: &str, terminal: bool) {
    let db = Analysis::for_project(Path::new("/finite-operator-audit"));
    let id = db.set_file_text(Path::new("/finite-operator-audit/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let selected: Vec<_> = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .filter(|expression| expression.syntax().text() == expression_text)
        .collect();
    assert_eq!(
        selected.len(),
        1,
        "exact unique operation: {expression_text}"
    );
    let ty = Inferer::new(&db).type_of_expr(&file, &selected[0]);
    assert_eq!(
        matches!(ty, Type::Never),
        terminal,
        "actual Inferer producer premise BEFORE Flow: {ty}; {source}"
    );
    let reference = file
        .scopes
        .references
        .iter()
        .rev()
        .find(|reference| reference.name == "marker" && !reference.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(
        &db,
        &file,
        flow::flow_scope(&file, reference.scope).unwrap(),
    );
    assert_eq!(
        !info.refs.contains_key(&reference.range.start()),
        terminal,
        "normal completion after exact constructed operands: {source}"
    );
}

#[test]
fn distinct_exact_same_class_operands_preserve_operator_callable_contract() {
    for (return_type, implementation, terminal) in [
        ("Never", "raise RuntimeError", true),
        ("int", "return 1", false),
    ] {
        let source = format!("from typing import Callable,Never\ndef result()->{return_type}:{implementation}\nclass Box:\n    def __add__(self,other)->Callable[[],{return_type}]:return result\ndef f():\n    marker=1\n    left=Box()\n    right=Box()\n    (left+right)()\n    return marker\n");
        check(&source, "(left+right)()", terminal);
    }
}

#[test]
fn captured_builtin_integer_operand_preserves_reflected_callable_contract() {
    for (return_type, implementation, terminal) in [
        ("Never", "raise RuntimeError", true),
        ("int", "return 1", false),
    ] {
        let source = format!("from typing import Callable,Never\ndef result()->{return_type}:{implementation}\nclass Box:\n    def __radd__(self,other:int)->Callable[[],{return_type}]:return result\ndef f():\n    marker=1\n    left=1\n    box=Box()\n    (left+box)()\n    return marker\n");
        check(&source, "(left+box)()", terminal);
    }
}

#[test]
fn implicit_operator_invocation_obeys_its_genuine_never_return() {
    for (return_type, implementation, terminal) in [
        ("Never", "raise RuntimeError", true),
        ("int", "return 1", false),
    ] {
        let source = format!("from typing import Never\nclass Box:\n    def __add__(self,other)->{return_type}:{implementation}\ndef f():\n    marker=1\n    box=Box()\n    box+box\n    return marker\n");
        check(&source, "box+box", terminal);
    }
}
