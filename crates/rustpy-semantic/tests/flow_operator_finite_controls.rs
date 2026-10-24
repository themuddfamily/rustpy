//! Public producer premise is mandatory before a Flow completion assertion.
use rustpy_semantic::{db::Analysis, flow, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

fn check(source: &str, expression_text: &str, premise: Option<bool>, terminal: bool) {
    let db = Analysis::for_project(Path::new("/finite-operator-audit"));
    let id = db.set_file_text(Path::new("/finite-operator-audit/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let selected: Vec<_> = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .filter(|expression| expression.syntax().text() == expression_text)
        .collect();
    assert_eq!(
        selected.len(),
        1,
        "exact unique operation: {expression_text}"
    );
    let ty = Inferer::new(&db).type_of_expr(&file, &selected[0]);
    if let Some(expected) = premise {
        assert_eq!(
            matches!(ty, Type::Never),
            expected,
            "actual Inferer premise: {ty}; {source}"
        );
    }
    let reference = file
        .scopes
        .references
        .iter()
        .rev()
        .find(|reference| reference.name == "marker" && !reference.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(
        &db,
        &file,
        flow::flow_scope(&file, reference.scope).unwrap(),
    );
    assert_eq!(
        !info.refs.contains_key(&reference.range.start()),
        terminal,
        "normal completion after exact constructed operands: {source}"
    );
}

#[test]
fn rebinding_integer_operand_does_not_retain_builtin_reflection() {
    check("from typing import Callable,Never\ndef normal()->int:return 1\ndef halt()->Never:raise RuntimeError\nclass Left:\n    def __add__(self,other)->Callable[[],int]:return normal\nclass Box:\n    def __radd__(self,other:int)->Callable[[],Never]:return halt\ndef f():\n    marker=1\n    left=1\n    left=Left()\n    box=Box()\n    (left+box)()\n    return marker\n","(left+box)()",Some(false),false);
}
#[test]
fn implicit_operator_member_replacement_keeps_tail() {
    check("from typing import Never\nclass Box:\n    def __add__(self,other)->Never:raise RuntimeError\nBox.__add__=lambda self,other:1\ndef f():\n    marker=1\n    box=Box()\n    box+box\n    return marker\n","box+box",None,false);
}
