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
fn named_expression_callable_preserves_captured_value_contract() {
    let prefix="from typing import Never\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\n";
    check(
        &format!("{prefix}def f():\n    marker=1\n    (callback := stop)()\n    return marker\n"),
        "(callback := stop)()",
        Some(true),
        true,
    );
    check(
        &format!("{prefix}def f():\n    marker=1\n    (callback := normal)()\n    return marker\n"),
        "(callback := normal)()",
        Some(false),
        false,
    );
    check(
        &format!(
            "{prefix}def f(value):\n    marker=1\n    (callback := value)()\n    return marker\n"
        ),
        "(callback := value)()",
        None,
        false,
    );
}
#[test]
fn builtin_super_bound_method_preserves_declared_terminal_contract() {
    let prefix="from typing import Never\nclass Base:\n    def stop(self)->Never:raise RuntimeError\n    def normal(self)->int:return 1\nclass Child(Base):\n";
    check(&format!("{prefix}    def run(self):\n        marker=1\n        super().stop()\n        return marker\n"),"super().stop()",Some(true),true);
    check(&format!("{prefix}    def run(self):\n        marker=1\n        super().normal()\n        return marker\n"),"super().normal()",Some(false),false);
}
#[test]
fn nested_receiver_member_preserves_declared_terminal_contract() {
    let prefix="from typing import Never\nclass Inner:\n    def stop(self)->Never:raise RuntimeError\n    def normal(self)->int:return 1\nclass Holder:\n    def __init__(self):self.child=Inner()\n";
    check(&format!("{prefix}def f(holder:Holder):\n    marker=1\n    holder.child.stop()\n    return marker\n"),"holder.child.stop()",Some(true),true);
    check(&format!("{prefix}def f(holder:Holder):\n    marker=1\n    holder.child.normal()\n    return marker\n"),"holder.child.normal()",Some(false),false);
}
