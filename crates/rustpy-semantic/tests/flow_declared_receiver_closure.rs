//! Finite standard receiver routes; no arbitrary runtime descriptor emulation.
use rustpy_semantic::{db::Analysis, flow, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(source: &str, expression_text: &str, terminal: bool) {
    let db = Analysis::for_project(Path::new("/receiver-closure-audit"));
    let id = db.set_file_text(Path::new("/receiver-closure-audit/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let selected: Vec<_> = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .filter(|expression| expression.syntax().text() == expression_text)
        .collect();
    assert_eq!(selected.len(), 1, "exact unique declared producer");
    let ty = Inferer::new(&db).type_of_expr(&file, &selected[0]);
    assert_eq!(
        matches!(ty, Type::Never),
        terminal,
        "public producer BEFORE Flow: {ty}; {source}"
    );
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
        "Flow completion: {source}"
    );
}
#[test]
fn canonical_property_produced_callable_preserves_declared_contract() {
    for (ret, body, terminal) in [
        ("Never", "raise RuntimeError", true),
        ("int", "return 1", false),
    ] {
        let source=format!("from typing import Callable,Never\ndef result()->{ret}:{body}\nclass C:\n    @property\n    def callback(self)->Callable[[],{ret}]:return result\ndef f():\n    marker=1\n    c=C()\n    c.callback()\n    return marker\n");
        check(&source, "c.callback()", terminal);
    }
    // Accessing a builtin property itself invokes its getter. A getter whose
    // actual declared return is Never cannot complete normally either.
    for (ret, body, terminal) in [
        ("Never", "raise RuntimeError", true),
        ("int", "return 1", false),
    ] {
        let source = format!("from typing import Never\nclass C:\n    @property\n    def terminal(self)->{ret}:{body}\ndef f():\n    marker=1\n    c=C()\n    c.terminal\n    return marker\n");
        check(&source, "c.terminal", terminal);
    }
}
#[test]
fn explicit_intrinsic_super_preserves_declared_contract() {
    for (ret, body, terminal) in [
        ("Never", "raise RuntimeError", true),
        ("int", "return 1", false),
    ] {
        let source=format!("from typing import Never\nclass Base:\n    def stop(self)->{ret}:{body}\nclass Child(Base):\n    def run(self):\n        marker=1\n        super(Child,self).stop()\n        return marker\n");
        check(&source, "super(Child,self).stop()", terminal);
    }
}
#[test]
fn two_initialized_receiver_fields_preserve_declared_contract() {
    for (ret, body, terminal) in [
        ("Never", "raise RuntimeError", true),
        ("int", "return 1", false),
    ] {
        let source=format!("from typing import Never\nclass Leaf:\n    def stop(self)->{ret}:{body}\nclass Middle:\n    def __init__(self):self.child=Leaf()\nclass Outer:\n    def __init__(self):self.middle=Middle()\ndef f():\n    marker=1\n    outer=Outer()\n    outer.middle.child.stop()\n    return marker\n");
        check(&source, "outer.middle.child.stop()", terminal);
    }
}
