use rustpy_semantic::{db::Analysis, flow, infer::Inferer, scope::ScopeKind, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
const NEVER: &str = "from typing import Never\ndef die()->Never: raise RuntimeError\n";
fn check(source: &str, name: &str, visited: bool, bound: bool, terminal_premise: bool) {
    let db = Analysis::for_project(Path::new("/eager-comprehension"));
    let id = db.set_file_text(Path::new("/eager-comprehension/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let reads: Vec<_> = file
        .scopes
        .references
        .iter()
        .filter(|read| read.name == name && !read.is_write)
        .collect();
    assert_eq!(reads.len(), 1, "unique actual tail load: {source}");
    let read = reads[0];
    let scope = flow::flow_scope(&file, read.scope).unwrap();
    assert_eq!(file.scopes.scope(scope).kind, ScopeKind::Function);
    let bindings = file.scopes.bindings_in(scope, name);
    assert_eq!(
        bindings.len(),
        1,
        "actual outer definition inventory: {source}"
    );
    assert_eq!(
        read.resolved,
        Some(bindings[0]),
        "actual tail resolves outer binding before flow assertion"
    );
    if terminal_premise {
        let calls: Vec<_> = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::CallExpr::cast)
            .filter(|call| {
                call.callee()
                    .is_some_and(|callee| callee.syntax().text() == "die")
            })
            .collect();
        assert_eq!(calls.len(), 1);
        assert!(
            matches!(
                Inferer::new(&db).type_of_expr(&file, &ast::Expr::Call(calls[0].clone())),
                Type::Never
            ),
            "actual canonical Never producer before Flow: {source}"
        );
    }
    let info = flow::flow_of_with_db(&db, &file, scope);
    let reaches = info.refs.get(&read.range.start());
    assert_eq!(
        reaches.is_some(),
        visited,
        "actual tail visitation: {source}"
    );
    if visited {
        let reaches = reaches.unwrap();
        let definitely_bound = !reaches.is_empty()
            && reaches
                .iter()
                .all(|reach| reach.binding == Some(bindings[0]));
        assert_eq!(
            definitely_bound, bound,
            "exact outer BindingId on every normal path: {source}"
        );
    }
}
#[test]
fn eager_nonempty_displays_definitely_store_outer_walrus() {
    for expression in [
        "[(assigned:=1) for item in (0,)]",
        "{(assigned:=1) for item in (0,)}",
        "{item:(assigned:=1) for item in (0,)}",
    ] {
        check(
            &format!("def f():\n values={expression}\n return assigned\n"),
            "assigned",
            true,
            true,
            false,
        );
    }
}
#[test]
fn required_eager_element_never_has_no_empty_creation_tail() {
    for expression in [
        "[die() for item in (0,)]",
        "{die() for item in (0,)}",
        "{item:die() for item in (0,)}",
    ] {
        check(
            &format!("{NEVER}def f():\n marker=1\n values={expression}\n return marker\n"),
            "marker",
            false,
            false,
            true,
        );
    }
}
#[test]
fn empty_false_filtered_and_unknown_paths_preserve_unbound_alternatives() {
    for expression in [
        "[(assigned:=1) for item in ()]",
        "[(assigned:=1) for item in (0,) if False]",
        "[(assigned:=1) for item in items]",
    ] {
        check(
            &format!("def f(items):\n values={expression}\n return assigned\n"),
            "assigned",
            true,
            false,
            false,
        );
    }
    check(
        "def f():\n values=[(assigned:=1) for item in (0,) if True]\n return assigned\n",
        "assigned",
        true,
        true,
        false,
    );
}
#[test]
fn generator_creation_defers_walrus_and_terminal_element() {
    check(
        "def f():\n values=((assigned:=1) for item in (0,))\n return assigned\n",
        "assigned",
        true,
        false,
        false,
    );
    check(
        &format!("{NEVER}def f():\n marker=1\n values=(die() for item in (0,))\n return marker\n"),
        "marker",
        true,
        true,
        true,
    );
}
#[test]
fn nested_required_iterables_and_empty_inner_iterable_are_distinct() {
    check(
        "def f():\n values=[(assigned:=1) for item in (0,) for other in (0,)]\n return assigned\n",
        "assigned",
        true,
        true,
        false,
    );
    check(
        "def f():\n values=[(assigned:=1) for item in (0,) for other in ()]\n return assigned\n",
        "assigned",
        true,
        false,
        false,
    );
    check(&format!("{NEVER}def f():\n marker=1\n values=[die() for item in (0,) for other in (0,)]\n return marker\n"), "marker", false, false, true);
}
