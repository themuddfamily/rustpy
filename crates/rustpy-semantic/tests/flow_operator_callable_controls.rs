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
fn distinct_subclass_reflection_cannot_use_left_slot_promise() {
    check("from typing import Never,Callable\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\nclass Left:\n    def __add__(self,other)->Callable[[],Never]:return stop\nclass Right(Left):\n    def __radd__(self,other)->Callable[[],int]:return normal\ndef f(left:Left,right:Right):\n    marker=1\n    (left+right)()\n    return marker\n","(left+right)()",None,false);
}
#[test]
fn replaced_operator_member_retires_initial_result_contract() {
    check("from typing import Never,Callable\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\nclass Box:\n    def __add__(self,other)->Callable[[],Never]:return stop\nBox.__add__=lambda self,other:normal\ndef f(box:Box):\n    marker=1\n    (box+box)()\n    return marker\n","(box+box)()",None,false);
}
#[test]
fn operand_callback_is_not_a_pure_slot_acquisition() {
    check("from typing import Never,Callable\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\nclass Box:\n    def __add__(self,other)->Callable[[],Never]:return stop\ndef replace():\n    Box.__add__=lambda self,other:normal\n    return Box()\ndef f(box:Box):\n    marker=1\n    (box+replace())()\n    return marker\n","(box+replace())()",None,false);
}
