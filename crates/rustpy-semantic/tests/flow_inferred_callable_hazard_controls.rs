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
fn lambda_body_mutation_is_not_a_pure_return_contract() {
    check(
        "from typing import Never\ndef halt()->Never: raise RuntimeError\ndef normal()->int: return 1\ndef f():\n    stop=halt\n    def replace():\n        nonlocal stop\n        stop=normal\n    marker=1\n    (lambda:(replace(),stop())[1])()\n    return marker\n",
        "(lambda:(replace(),stop())[1])()",
        None,
        false,
    );
}

#[test]
fn factory_body_rebinding_cannot_reuse_prior_callable() {
    check(
        "from typing import Never\ndef halt()->Never: raise RuntimeError\ndef normal()->int: return 1\ndef f():\n    stop=halt\n    def factory():\n        nonlocal stop\n        stop=normal\n        return stop\n    marker=1\n    factory()()\n    return marker\n",
        "factory()()",
        Some(false),
        false,
    );
}

#[test]
fn mutated_indexed_container_cannot_reuse_initial_element() {
    check(
        "from typing import Never\ndef halt()->Never: raise RuntimeError\ndef normal()->int: return 1\ndef f():\n    values=[halt]\n    values[0]=normal\n    marker=1\n    values[0]()\n    return marker\n",
        "values[0]()",
        None,
        false,
    );
}

#[test]
fn boolean_truthiness_mutation_precedes_selected_callable() {
    check(
        "from typing import Never\ndef halt()->Never: raise RuntimeError\ndef normal()->int: return 1\ndef f():\n    stop=halt\n    class Flag:\n        def __bool__(self):\n            nonlocal stop\n            stop=normal\n            return False\n        def __call__(self)->Never: raise RuntimeError\n    flag=Flag()\n    marker=1\n    (flag or stop)()\n    return marker\n",
        "(flag or stop)()",
        Some(true),
        false,
    );
}
