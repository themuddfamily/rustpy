//! Fixture-only canonical declared Never shapes. Inferer premise precedes Flow.
use rustpy_semantic::{db::Analysis, flow, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
fn check(source: &str, never: bool, tail_reachable: bool) {
    let directory = tempfile::TempDir::with_prefix("rustpy-canonical-callable-shapes").unwrap();
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
                .is_some_and(|callee| callee.syntax().text() == "stop")
        })
        .collect();
    assert_eq!(calls.len(), 1, "actual unique stop invocation required");
    let expression = ast::Expr::Call(calls.into_iter().next().unwrap());
    let result = Inferer::new(db).type_of_expr(&file, &expression);
    assert_eq!(
        matches!(result, Type::Never),
        never,
        "maintained Inferer premise BEFORE Flow: {result}; {source}"
    );
    let references: Vec<_> = file
        .scopes
        .references
        .iter()
        .filter(|reference| reference.name == "marker" && !reference.is_write)
        .collect();
    assert_eq!(references.len(), 1, "actual assigned marker tail required");
    let reference = references[0];
    let info = flow::flow_of_with_db(db, &file, flow::flow_scope(&file, reference.scope).unwrap());
    assert_eq!(
        info.refs.contains_key(&reference.range.start()),
        tail_reachable,
        "actual normal completion after declared call: {source}"
    );
}
fn function(prefix: &str, annotation: &str, invocation: &str) -> String {
    format!(
        "{prefix}def f(stop:{annotation}):\n    marker=1\n    {invocation}\n    return marker\n"
    )
}
#[test]
fn concatenate_paramspec_canonical_noreturn_call_stops_flow() {
    check(
        &function(
            "from typing import Callable,Concatenate,ParamSpec,NoReturn\nP=ParamSpec('P')\n",
            "Callable[Concatenate[int,P],NoReturn]",
            "stop(0)",
        ),
        true,
        false,
    );
}
#[test]
fn annotated_canonical_callable_noreturn_stops_flow() {
    check(
        &function(
            "from typing import Annotated,Callable,NoReturn\n",
            "Annotated[Callable[[],NoReturn],'terminal-contract']",
            "stop()",
        ),
        true,
        false,
    );
}
#[test]
fn canonical_paramspec_builtin_object_bound_stops_flow() {
    check(
        &function(
            "from typing import Callable,ParamSpec,NoReturn\nP=ParamSpec('P',bound=object)\n",
            "Callable[P,NoReturn]",
            "stop()",
        ),
        true,
        false,
    );
}
#[test]
fn canonical_shapes_keep_returning_and_shadowed_marker_controls() {
    for (prefix, returning, shadowed, invocation) in [
        (
            "from typing import Callable,Concatenate,ParamSpec\nP=ParamSpec('P')\n",
            "Callable[Concatenate[int,P],int]",
            "Callable[Concatenate[int,P],NoReturn]",
            "stop(0)",
        ),
        (
            "from typing import Annotated,Callable\n",
            "Annotated[Callable[[],int],'returning']",
            "Annotated[Callable[[],NoReturn],'shadowed']",
            "stop()",
        ),
        (
            "from typing import Callable,ParamSpec\nP=ParamSpec('P',bound=object)\n",
            "Callable[P,int]",
            "Callable[P,NoReturn]",
            "stop()",
        ),
    ] {
        check(&function(prefix, returning, invocation), false, true);
        check(
            &function(
                &format!("{prefix}class NoReturn:\n    pass\n"),
                shadowed,
                invocation,
            ),
            false,
            true,
        );
    }
}
#[test]
fn unknown_callable_annotation_retains_normal_continuation() {
    check(&function("", "object", "stop()"), false, true);
}
