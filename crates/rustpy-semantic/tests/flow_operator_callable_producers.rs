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
fn binary_dunder_produced_callable_carries_terminal_contract() {
    let prefix="from typing import Never,Callable\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\n";
    check(&format!("{prefix}class Box:\n    def __add__(self,other)->Callable[[],Never]:return stop\ndef f(box:Box):\n    marker=1\n    (box+box)()\n    return marker\n"),"(box+box)()",Some(true),true);
    check(&format!("{prefix}class Box:\n    def __add__(self,other)->Callable[[],int]:return normal\ndef f(box:Box):\n    marker=1\n    (box+box)()\n    return marker\n"),"(box+box)()",Some(false),false);
    check(
        &format!("{prefix}def f(box):\n    marker=1\n    (box+box)()\n    return marker\n"),
        "(box+box)()",
        None,
        false,
    );
}
#[test]
fn reflected_dunder_produced_callable_carries_terminal_contract() {
    let prefix="from typing import Never,Callable\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\n";
    check(&format!("{prefix}class Box:\n    def __radd__(self,other:int)->Callable[[],Never]:return stop\ndef f(box:Box):\n    marker=1\n    (1+box)()\n    return marker\n"),"(1+box)()",Some(true),true);
    check(&format!("{prefix}class Box:\n    def __radd__(self,other:int)->Callable[[],int]:return normal\ndef f(box:Box):\n    marker=1\n    (1+box)()\n    return marker\n"),"(1+box)()",Some(false),false);
}
