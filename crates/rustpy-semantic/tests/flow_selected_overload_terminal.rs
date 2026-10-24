//! Precise selected-overload premises, distinct from uniform overload contracts.
use rustpy_semantic::{db::Analysis, flow, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
const PREFIX: &str="from typing import overload,Never\n@overload\ndef stop(value:int)->Never: ...\n@overload\ndef stop(value:str)->str: ...\ndef stop(value:int|str):\n    if isinstance(value,int): raise RuntimeError\n    return value\n";
fn check(header: &str, argument: &str, inferred_never: Option<bool>, terminal: bool) {
    let source =
        format!("{PREFIX}{header}\n    marker=1\n    stop({argument})\n    return marker\n");
    let db = Analysis::for_project(Path::new("/selected-overload-terminal"));
    let id = db.set_file_text(Path::new("/selected-overload-terminal/m.py"), &source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let inf = Inferer::new(&db);
    let call = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .find(|call| {
            call.callee()
                .is_some_and(|callee| callee.syntax().text() == "stop")
        })
        .unwrap();
    let Type::Overloaded(signatures) = inf.type_of_expr(&file, &call.callee().unwrap()) else {
        panic!("actual mixed overload set required")
    };
    assert_eq!(signatures.len(), 2);
    assert!(signatures
        .iter()
        .any(|signature| matches!(signature.ret, Type::Never)));
    assert!(signatures
        .iter()
        .any(|signature| !matches!(signature.ret, Type::Never)));
    if let Some(expected) = inferred_never {
        let result = inf.type_of_expr(&file, &ast::Expr::Call(call));
        assert_eq!(
            matches!(result, Type::Never),
            expected,
            "actual selected call result BEFORE flow: {result}; {source}"
        );
    }
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
        "actual selected-overload completion: {source}"
    );
}
#[test]
fn literal_int_selects_never_branch_and_stops_tail() {
    check("def f():", "0", Some(true), true);
}
#[test]
fn declared_int_argument_selects_never_branch_and_stops_tail() {
    check("def f(value:int):", "value", Some(true), true);
}
#[test]
fn declared_str_argument_selects_returning_branch_and_keeps_tail() {
    check("def f(value:str):", "value", Some(false), false);
}
#[test]
fn unknown_argument_does_not_prove_a_specific_mixed_branch() {
    // A first-signature fallback is not authoritative branch selection. Verify
    // the actual mixed set above, and require its returning path to remain.
    check("def f(value):", "value", None, false);
}
