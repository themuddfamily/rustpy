use rustpy_semantic::{db::Analysis, flow, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(source: &str) {
    let db = Analysis::for_project(Path::new("/terminal-method-contracts"));
    let id = db.set_file_text(Path::new("/terminal-method-contracts/m.py"), source);
    let file = db.file(id).unwrap();
    let call = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .find(|c| {
            c.callee()
                .is_some_and(|e| e.syntax().text().to_string().ends_with(".stop"))
        })
        .unwrap();
    let expression = ast::Expr::cast(call.syntax().clone()).unwrap();
    assert!(
        matches!(
            Inferer::new(&db).type_of_expr(&file, &expression),
            Type::Never
        ),
        "maintained producer must establish real terminal method contract"
    );
    let reference = file
        .scopes
        .references
        .iter()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(
        &db,
        &file,
        flow::flow_scope(&file, reference.scope).unwrap(),
    );
    assert!(
        !info.refs.contains_key(&reference.range.start()),
        "proven method contract prevents actual assigned marker tail: {source}"
    );
}
#[test]
fn ordinary_instance_method() {
    check("from typing import NoReturn\nclass C:\n    def stop(self)->NoReturn:\n        raise RuntimeError\ndef f():\n    c=C()\n    marker=1\n    c.stop()\n    return marker\n");
}
#[test]
fn canonical_static_method() {
    check("from typing import NoReturn\nclass C:\n    @staticmethod\n    def stop()->NoReturn:\n        raise RuntimeError\ndef f():\n    marker=1\n    C.stop()\n    return marker\n");
}
#[test]
fn canonical_class_method() {
    check("from typing import NoReturn\nclass C:\n    @classmethod\n    def stop(cls)->NoReturn:\n        raise RuntimeError\ndef f():\n    marker=1\n    C.stop()\n    return marker\n");
}
