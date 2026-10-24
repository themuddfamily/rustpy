//! Public premises for finite remaining declared terminal producers; unexecuted.
use rustpy_semantic::{db::Analysis, flow, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
fn check(source: &str, callee_text: &str, never: bool, reachable: bool) {
    let directory = tempfile::TempDir::with_prefix("rustpy-remaining-declared-producers").unwrap();
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
    assert_eq!(calls.len(), 1, "unique actual selected invocation");
    let result =
        Inferer::new(db).type_of_expr(&file, &ast::Expr::Call(calls.into_iter().next().unwrap()));
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
    assert_eq!(references.len(), 1, "unique actual assigned tail read");
    let reference = references[0];
    let info = flow::flow_of_with_db(db, &file, flow::flow_scope(&file, reference.scope).unwrap());
    assert_eq!(
        info.refs.contains_key(&reference.range.start()),
        reachable,
        "actual normal completion after known call: {source}"
    );
}
#[test]
fn canonical_stacked_static_overloads_preserve_never_contract() {
    check("from typing import overload,NoReturn\nclass C:\n    @overload\n    @staticmethod\n    def stop(x:int)->NoReturn:...\n    @overload\n    @staticmethod\n    def stop(x:str)->NoReturn:...\n    @staticmethod\n    def stop(x:int|str)->NoReturn:\n        raise RuntimeError\ndef f():\n    marker=1\n    C.stop(0)\n    return marker\n","C.stop",true,false);
}
#[test]
fn canonical_stacked_class_overloads_preserve_never_contract() {
    check("from typing import overload,NoReturn\nclass C:\n    @overload\n    @classmethod\n    def stop(cls,x:int)->NoReturn:...\n    @overload\n    @classmethod\n    def stop(cls,x:str)->NoReturn:...\n    @classmethod\n    def stop(cls,x:int|str)->NoReturn:\n        raise RuntimeError\ndef f():\n    marker=1\n    C.stop(0)\n    return marker\n","C.stop",true,false);
}
#[test]
fn declared_callable_factory_result_preserves_never_contract() {
    check("from typing import Callable,NoReturn\ndef stop()->NoReturn:\n    raise RuntimeError\ndef factory()->Callable[[],NoReturn]:\n    return stop\ndef f():\n    marker=1\n    factory()()\n    return marker\n","factory()",true,false);
}
#[test]
fn declared_callable_factory_returning_result_keeps_flow() {
    check("from typing import Callable\ndef stop()->int:\n    return 1\ndef factory()->Callable[[],int]:\n    return stop\ndef f():\n    marker=1\n    factory()()\n    return marker\n","factory()",false,true);
}
