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
#[test]
fn lambda_inferred_body_contract() {
    check("from typing import Never,Callable\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\ndef f():\n    marker=1\n    (lambda:stop())()\n    return marker\n","(lambda:stop())()",Some(true),true);
    check("from typing import Never,Callable\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\ndef f():\n    marker=1\n    (lambda:normal())()\n    return marker\n","(lambda:normal())()",Some(false),false);
    check("from typing import Never,Callable\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\ndef f(callback):\n    marker=1\n    (lambda:callback())()\n    return marker\n","(lambda:callback())()",None,false);
}
#[test]
fn unannotated_factory_inferred_callable_result() {
    check("from typing import Never,Callable\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\ndef factory():return stop\ndef f():\n    marker=1\n    factory()()\n    return marker\n","factory()()",Some(true),true);
    check("from typing import Never,Callable\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\ndef factory():return normal\ndef f():\n    marker=1\n    factory()()\n    return marker\n","factory()()",Some(false),false);
    check("from typing import Never,Callable\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\ndef factory(callback):return callback\ndef f(callback):\n    marker=1\n    factory(callback)()\n    return marker\n","factory(callback)()",None,false);
}
#[test]
fn getitem_and_literal_list_callable_results() {
    check("from typing import Never,Callable\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\nclass Slots:\n    def __getitem__(self,index:int)->Callable[[],Never]:return stop\ndef f(slots:Slots):\n    marker=1\n    slots[0]()\n    return marker\n","slots[0]()",Some(true),true);
    check("from typing import Never,Callable\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\nclass Slots:\n    def __getitem__(self,index:int)->Callable[[],int]:return normal\ndef f(slots:Slots):\n    marker=1\n    slots[0]()\n    return marker\n","slots[0]()",Some(false),false);
    check("from typing import Never,Callable\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\ndef f():\n    marker=1\n    [stop][0]()\n    return marker\n","[stop][0]()",Some(true),true);
    check("from typing import Never,Callable\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\ndef f():\n    marker=1\n    [normal][0]()\n    return marker\n","[normal][0]()",Some(false),false);
    check("from typing import Never,Callable\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\ndef f(values):\n    marker=1\n    values[0]()\n    return marker\n","values[0]()",None,false);
}
#[test]
fn boolean_alternative_callable_results() {
    check("from typing import Never,Callable\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\ndef f():\n    marker=1\n    (stop or stop)()\n    return marker\n","(stop or stop)()",Some(true),true);
    check("from typing import Never,Callable\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\ndef f():\n    marker=1\n    (normal or normal)()\n    return marker\n","(normal or normal)()",Some(false),false);
    // Functions are always truthy: `stop or normal` is `stop`, so the call never returns.
    check("from typing import Never,Callable\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\ndef f():\n    marker=1\n    (stop or normal)()\n    return marker\n","(stop or normal)()",Some(true),true);
    check("from typing import Never,Callable\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\ndef f(callback):\n    marker=1\n    (stop or callback)()\n    return marker\n","(stop or callback)()",None,false);
}
#[test]
fn boolean_alternative_callable_truthiness_matches_inferer() {
    let call = |expression: &str| {
        format!("from typing import Never,Callable\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\ndef f():\n    marker=1\n    {expression}\n    return marker\n")
    };
    // `and` skips always-truthy functions: the last operand is the result.
    check(
        &call("(normal and stop)()"),
        "(normal and stop)()",
        Some(true),
        true,
    );
    check(
        &call("(stop and normal)()"),
        "(stop and normal)()",
        Some(false),
        false,
    );
    // `or` stops at the first function, whatever follows.
    check(
        &call("(normal or stop)()"),
        "(normal or stop)()",
        Some(false),
        false,
    );
    check(
        &call("(stop or normal or normal)()"),
        "(stop or normal or normal)()",
        Some(true),
        true,
    );
}
