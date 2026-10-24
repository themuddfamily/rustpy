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
#[test]
fn alternate_integer_spellings_select_the_same_terminal_slot() {
    for index in ["0x0", "0o0", "0b0", "0_0", "-2"] {
        let source = format!(
            "from typing import Callable,NoReturn\ndef f(stops:tuple[Callable[[],NoReturn],Callable[[],int]]):\n    marker=1\n    stops[{index}]()\n    marker\n"
        );
        check(&source, Some("Never"), false);
    }
    // Unary plus inference currently widens the index; a one-slot contract
    // retains its public Never premise, while selection still validates index0.
    check(
        "from typing import Callable,NoReturn\ndef f(stops:tuple[Callable[[],NoReturn]]):\n    marker=1\n    stops[+0]()\n    marker\n",
        Some("Never"),
        false,
    );
}
#[test]
fn alternate_spelling_of_invalid_slot_keeps_flow() {
    check(
        "from typing import Callable,NoReturn\ndef f(stops:tuple[Callable[[],NoReturn]]):\n    marker=1\n    stops[0x2]()\n    marker\n",
        Some("Unknown"),
        true,
    );
}
