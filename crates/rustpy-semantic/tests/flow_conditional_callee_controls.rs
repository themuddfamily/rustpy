//! Source-only proposals: premise failures must be reported separately from Flow gaps.
use rustpy_semantic::{db::Analysis, flow, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(source: &str, call_text: &str, never: Option<bool>, terminal: bool) {
    let db = Analysis::for_project(Path::new("/remaining-producer"));
    let id = db.set_file_text(Path::new("/remaining-producer/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let call = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .find(|call| call.syntax().text() == call_text)
        .unwrap();
    if let Some(expected) = never {
        let ty = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Call(call));
        assert_eq!(
            matches!(ty, Type::Never),
            expected,
            "Inferer premise BEFORE Flow: {ty}; {source}"
        );
    }
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
        "Flow completion: {source}"
    );
}
const PREFIX: &str =
    "from typing import Never\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\n";
#[test]
fn parenthesized_nested_agreement() {
    check(&format!("{PREFIX}def f(flag,other):\n    marker=1\n    ((stop if other else stop) if flag else stop)()\n    return marker\n"),"((stop if other else stop) if flag else stop)()",Some(true),true);
}
#[test]
fn rebound_branch_retains_tail() {
    check(&format!("{PREFIX}def f(flag):\n    marker=1\n    stop=normal\n    (stop if flag else normal)()\n    return marker\n"),"(stop if flag else normal)()",Some(false),false);
}
#[test]
fn effectful_condition_declines_proof() {
    check(&format!("{PREFIX}def flag():return True\ndef f():\n    marker=1\n    (stop if flag() else stop)()\n    return marker\n"),"(stop if flag() else stop)()",Some(true),false);
}
#[test]
fn asynchronous_agreement_requires_await() {
    let source="from typing import Never\nasync def stop()->Never:raise RuntimeError\nasync def f(flag):\n    marker=1\n    await (stop if flag else stop)()\n    return marker\n";
    check(source, "(stop if flag else stop)()", None, true);
    check(
        &source.replace("await (stop", "(stop"),
        "(stop if flag else stop)()",
        None,
        false,
    );
}

#[test]
fn conditional_async_public_coroutine_and_awaited_never_premises() {
    let source="from typing import Never\nasync def stop()->Never:raise RuntimeError\nasync def f(flag):\n    marker=1\n    await (stop if flag else stop)()\n    return marker\n";
    let db = Analysis::for_project(Path::new("/conditional-async-premise"));
    let id = db.set_file_text(Path::new("/conditional-async-premise/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let inferer = Inferer::new(&db);
    let call = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .find(|call| call.syntax().text() == "(stop if flag else stop)()")
        .unwrap();
    let call_type = inferer.type_of_expr(&file, &ast::Expr::Call(call));
    assert!(
        !matches!(call_type, Type::Never),
        "coroutine construction is normal: {call_type}"
    );
    assert!(
        matches!(&call_type,Type::Instance(class,args) if class.name=="Coroutine" && args.len()==3 && matches!(args[2],Type::Never)),
        "actual Coroutine[...,Never]: {call_type}"
    );
    let awaited = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::AwaitExpr::cast)
        .next()
        .unwrap();
    let awaited_type = inferer.type_of_expr(&file, &ast::Expr::Await(awaited));
    assert!(
        matches!(awaited_type, Type::Never),
        "actual awaited result BEFORE Flow: {awaited_type}"
    );
    check(source, "(stop if flag else stop)()", None, true);
    check(
        &source.replace("await (stop", "(stop"),
        "(stop if flag else stop)()",
        None,
        false,
    );
}
