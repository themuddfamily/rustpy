//! Awaitable result contracts apply even when the await operand is not a Call.
use rustpy_semantic::{db::Analysis, flow, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(source: &str, text: &str, terminal: bool) {
    let db = Analysis::for_project(Path::new("/direct-awaitable-audit"));
    let id = db.set_file_text(Path::new("/direct-awaitable-audit/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let selected: Vec<_> = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::AwaitExpr::cast)
        .filter(|expression| expression.syntax().text() == text)
        .collect();
    assert_eq!(selected.len(), 1, "exact unique await event");
    let ty = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Await(selected[0].clone()));
    assert_eq!(
        matches!(ty, Type::Never),
        terminal,
        "actual maintained await result BEFORE Flow: {ty}; {source}"
    );
    assert!(!ty.is_unknown(), "normal await result must be concrete");
    let reference = file
        .scopes
        .references
        .iter()
        .rev()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(
        &db,
        &file,
        flow::flow_scope(&file, reference.scope).unwrap(),
    );
    assert_eq!(
        !info.refs.contains_key(&reference.range.start()),
        terminal,
        "await completion: {source}"
    );
}
#[test]
fn direct_awaitable_and_coroutine_never_results_stop_normal_completion() {
    for head in ["Awaitable", "Coroutine"] {
        for (ret, terminal) in [("Never", true), ("int", false)] {
            let annotation = if head == "Coroutine" {
                format!("Coroutine[Any,Any,{ret}]")
            } else {
                format!("Awaitable[{ret}]")
            };
            let source=format!("from typing import Awaitable,Coroutine,Any,Never\nasync def f(pending:{annotation}):\n    marker=1\n    await pending\n    return marker\n");
            check(&source, "await pending", terminal);
        }
    }
}
#[test]
fn awaitable_alias_and_operand_rebinding_use_actual_acquired_value() {
    let source="from typing import Awaitable,Never\nasync def f(pending:Awaitable[Never]):\n    marker=1\n    alias=pending\n    await alias\n    return marker\n";
    check(source, "await alias", true);
    let source="from typing import Awaitable,Never\nasync def f(pending:Awaitable[Never],normal:Awaitable[int]):\n    marker=1\n    await (pending:=normal)\n    return marker\n";
    check(source, "await (pending:=normal)", false);
}
