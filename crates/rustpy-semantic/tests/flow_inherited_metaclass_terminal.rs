//! Public Never premise and actual flow consumer, not arbitrary metaclass execution.
use rustpy_semantic::{
    db::Analysis,
    flow::{flow_of_with_db, flow_scope},
    infer::Inferer,
};
use rustpy_syntax::ast::{self, AstNode};
fn check(source: &str, expected: &str, reachable: bool) {
    let directory = tempfile::TempDir::with_prefix("rustpy-meta-terminal").unwrap();
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
    let call = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .find(|call| {
            call.callee()
                .is_some_and(|callee| callee.syntax().text() == "C")
        })
        .unwrap();
    assert_eq!(
        Inferer::new(db)
            .type_of_expr(&file, &ast::Expr::Call(call))
            .to_string(),
        expected
    );
    let reference = file
        .scopes
        .references
        .iter()
        .find(|reference| reference.name == "marker" && !reference.is_write)
        .unwrap();
    let scope = flow_scope(&file, reference.scope).unwrap();
    let flow = flow_of_with_db(db, &file, scope);
    assert_eq!(flow.refs.contains_key(&reference.range.start()), reachable);
}
#[test]
fn inherited_metaclass_call_never_terminates_following_flow() {
    check(
        "from __future__ import annotations\nfrom typing import Never\nclass Meta(type):\n    def __call__(cls) -> Never: raise RuntimeError\nclass Base(metaclass=Meta): pass\nclass C(Base): pass\ndef f():\n    marker=1\n    C()\n    marker\n",
        "Never",
        false,
    );
}
#[test]
fn returning_inherited_metaclass_call_keeps_following_flow() {
    check(
        "from __future__ import annotations\nclass Meta(type):\n    def __call__(cls) -> int: return 1\nclass Base(metaclass=Meta): pass\nclass C(Base): pass\ndef f():\n    marker=1\n    C()\n    marker\n",
        "int",
        true,
    );
}
