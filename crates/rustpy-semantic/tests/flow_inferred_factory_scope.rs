use rustpy_semantic::{db::Analysis, flow, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
#[test]
fn inferred_factory_return_uses_definition_scope_not_caller_shadow() {
    let source="from typing import Never\ndef stop()->int:return 1\ndef factory():return stop\ndef f():\n    def stop()->Never:raise RuntimeError\n    marker=1\n    factory()()\n    return marker\n";
    let db = Analysis::for_project(Path::new("/factory-definition-scope"));
    let id = db.set_file_text(Path::new("/factory-definition-scope/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let call = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .find(|call| call.syntax().text() == "factory()()")
        .unwrap();
    let result = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Call(call));
    assert!(
        !matches!(result, Type::Never),
        "actual factory result must be returning: {result}"
    );
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
    assert!(
        info.refs.contains_key(&reference.range.start()),
        "module factory cannot capture caller's nested stop"
    );
}
