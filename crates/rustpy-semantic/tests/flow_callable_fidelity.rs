//! Public producer fidelity first; does not revise prior private guard inputs.
use rustpy_semantic::{db::Analysis, flow, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
fn check(source: &str, callee_text: &str, never: bool, reachable: bool) {
    let directory = tempfile::TempDir::with_prefix("rustpy-callable-shape-fidelity").unwrap();
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
    let calls: Vec<_> = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .filter(|call| {
            call.callee()
                .is_some_and(|callee| callee.syntax().text() == callee_text)
        })
        .collect();
    assert_eq!(calls.len(), 1, "unique actual call required");
    let result =
        Inferer::new(db).type_of_expr(&file, &ast::Expr::Call(calls.into_iter().next().unwrap()));
    assert_eq!(
        matches!(result, Type::Never),
        never,
        "actual maintained Inferer premise BEFORE flow: {result}; {source}"
    );
    let references: Vec<_> = file
        .scopes
        .references
        .iter()
        .filter(|reference| reference.name == "marker" && !reference.is_write)
        .collect();
    assert_eq!(references.len(), 1, "unique actual assigned tail required");
    let reference = references[0];
    let info = flow::flow_of_with_db(db, &file, flow::flow_scope(&file, reference.scope).unwrap());
    assert_eq!(
        info.refs.contains_key(&reference.range.start()),
        reachable,
        "actual flow normal completion: {source}"
    );
}
#[test]
fn variadic_callable_tuple_preserves_known_noreturn_element_contract() {
    check("from typing import Callable,NoReturn\ndef f(stops:tuple[Callable[[],NoReturn],...]):\n    marker=1\n    stops[0]()\n    return marker\n","stops[0]",true,false);
}
#[test]
fn canonical_paramspec_builtin_str_bound_preserves_noreturn_contract() {
    check("from typing import Callable,ParamSpec,NoReturn\nP=ParamSpec('P',bound=str)\ndef f(stop:Callable[P,NoReturn]):\n    marker=1\n    stop()\n    return marker\n","stop",true,false);
}
#[test]
fn variadic_callable_tuple_returning_element_keeps_flow() {
    check("from typing import Callable\ndef f(stops:tuple[Callable[[],int],...]):\n    marker=1\n    stops[0]()\n    return marker\n","stops[0]",false,true);
}
#[test]
fn canonical_paramspec_builtin_str_bound_returning_contract_keeps_flow() {
    check("from typing import Callable,ParamSpec\nP=ParamSpec('P',bound=str)\ndef f(stop:Callable[P,int]):\n    marker=1\n    stop()\n    return marker\n","stop",false,true);
}
