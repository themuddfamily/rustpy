use rustpy_semantic::{db::Analysis, flow, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(source: &str, callee_text: &str) {
    let db = Analysis::for_project(Path::new("/receiver-method-contracts"));
    let id = db.set_file_text(Path::new("/receiver-method-contracts/m.py"), source);
    let file = db.file(id).unwrap();
    let call = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .find(|call| {
            call.callee()
                .is_some_and(|callee| callee.syntax().text() == callee_text)
        })
        .unwrap();
    let expression = ast::Expr::cast(call.syntax().clone()).unwrap();
    assert!(
        matches!(
            Inferer::new(&db).type_of_expr(&file, &expression),
            Type::Never
        ),
        "maintained producer must establish genuine Never first: {source}"
    );
    let reference = file
        .scopes
        .references
        .iter()
        .find(|reference| reference.name == "marker" && !reference.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(
        &db,
        &file,
        flow::flow_scope(&file, reference.scope).unwrap(),
    );
    assert!(
        !info.refs.contains_key(&reference.range.start()),
        "actual terminal call prevents assigned marker tail: {source}"
    );
}
const CLASS: &str = "from typing import NoReturn\nclass C:\n    def stop(self)->NoReturn:\n        raise RuntimeError\n";
#[test]
fn ordinary_unbound_method_contract() {
    check(
        &format!("{CLASS}def f():\n    c=C()\n    marker=1\n    C.stop(c)\n    return marker\n"),
        "C.stop",
    );
}
#[test]
fn captured_bound_method_alias_contract() {
    check(&format!("{CLASS}def f():\n    c=C()\n    halt=c.stop\n    marker=1\n    halt()\n    return marker\n"),"halt");
}
#[test]
fn typed_instance_parameter_method_contract() {
    check(
        &format!("{CLASS}def f(c:C):\n    marker=1\n    c.stop()\n    return marker\n"),
        "c.stop",
    );
}
#[test]
fn inline_constructed_receiver_method_contract() {
    check(
        &format!("{CLASS}def f():\n    marker=1\n    C().stop()\n    return marker\n"),
        "C().stop",
    );
}
