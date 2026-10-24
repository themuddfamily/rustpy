//! Fixed tuple Callable contracts: public inference premise before real flow assertion.
use rustpy_semantic::{
    db::Analysis,
    flow::{flow_of_with_db, flow_scope},
    infer::Inferer,
};
use rustpy_syntax::ast::{self, AstNode};
fn check(source: &str, expected_type: Option<&str>, reachable: bool) {
    let directory = tempfile::TempDir::with_prefix("rustpy-indexed-terminal").unwrap();
    let path = directory.path().join("main.py");
    std::fs::write(&path, source).unwrap();
    let db = Analysis::for_project(directory.path());
    let _ = db.module("typing");
    let snapshot = db
        .snapshot(
            std::slice::from_ref(&path),
            &rustpy_core::CancellationToken::new(),
        )
        .unwrap();
    let db = snapshot.analysis();
    let file = db.file_for_path(&path).unwrap();
    let call = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .find(|call| {
            call.callee()
                .is_some_and(|callee| matches!(callee.unparen(), ast::Expr::Subscript(_)))
        })
        .unwrap();
    if let Some(expected) = expected_type {
        assert_eq!(
            Inferer::new(db)
                .type_of_expr(&file, &ast::Expr::Call(call))
                .to_string(),
            expected
        );
    }
    let reference = file
        .scopes
        .references
        .iter()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    let flow = flow_of_with_db(db, &file, flow_scope(&file, reference.scope).unwrap());
    assert_eq!(
        flow.refs.contains_key(&reference.range.start()),
        reachable,
        "{source}"
    );
}
const HEAD: &str = "from typing import Callable,NoReturn\ndef f(stops:tuple[Callable[[],NoReturn],Callable[[],int]]):\n    marker=1\n";
#[test]
fn fixed_tuple_noreturn_callable_terminates_flow() {
    check(
        &format!("{HEAD}    stops[0]()\n    marker\n"),
        Some("Never"),
        false,
    );
}
#[test]
fn fixed_tuple_returning_slot_keeps_flow() {
    check(
        &format!("{HEAD}    stops[1]()\n    marker\n"),
        Some("int"),
        true,
    );
}
#[test]
fn rebound_tuple_cannot_reuse_parameter_terminal_contract() {
    // This deliberate contract violation is not a terminal proof: runtime binding returns.
    check(
        &format!("{HEAD}    stops=(lambda:1,lambda:2)\n    stops[0]()\n    marker\n"),
        None,
        true,
    );
}
#[test]
fn out_of_range_slot_has_no_noreturn_authority() {
    // Existing flow does not prove IndexError; an invalid slot cannot fabricate Never.
    check(
        &format!("{HEAD}    stops[2]()\n    marker\n"),
        Some("Unknown"),
        true,
    );
}
