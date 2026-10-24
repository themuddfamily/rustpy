//! Member reference narrowing (roadmap "Narrowing"): `self.x`, `a.b.c`,
//! `d["k"]` and `t[0]` narrow like names, keyed structurally; writes to the
//! reference or a prefix retire the narrowing, calls keep it (pyright).
use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::{collections::BTreeSet, path::Path};

/// Type of the read `expression` that starts where `needle` (unique) does.
fn check(source: &str, needle: &str, expression: &str, expected: &[&str]) {
    let db = Analysis::for_project(Path::new("/member-narrowing"));
    let id = db.set_file_text(Path::new("/member-narrowing/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    assert_eq!(source.matches(needle).count(), 1, "{needle}");
    let offset = source.find(needle).unwrap();
    let read = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .find(|candidate| {
            candidate.syntax().text_range().start() == rustpy_core::TextSize::new(offset as u32)
                && candidate.syntax().text() == expression
        })
        .unwrap_or_else(|| panic!("no read {expression:?} at {needle:?}"));
    let actual = Inferer::new(&db).type_of_expr(&file, &read);
    let values = match actual {
        Type::Union(values) => values,
        value => vec![value],
    };
    let actual: BTreeSet<_> = values.into_iter().map(|value| value.to_string()).collect();
    let expected: BTreeSet<_> = expected.iter().map(|value| value.to_string()).collect();
    assert_eq!(actual, expected, "{source}\n{needle}");
}

const HOLDER: &str = "class Node:\n    def name(self) -> str: return ''\ndef make() -> Node | None: ...\ndef work() -> None: ...\nclass Holder:\n    node: Node | None = None\n";

#[test]
fn self_attribute_none_checks_narrow_and_join_after_the_branch() {
    let source = format!(
        "{HOLDER}    def use(self) -> None:\n        if self.node is not None:\n            self.node #narrowed\n        else:\n            self.node #none\n        self.node #after\n        if not self.node:\n            return\n        self.node #exit\n"
    );
    check(&source, "self.node #narrowed", "self.node", &["Node"]);
    check(&source, "self.node #none", "self.node", &["None"]);
    check(&source, "self.node #after", "self.node", &["Node", "None"]);
    check(&source, "self.node #exit", "self.node", &["Node"]);
}

#[test]
fn nested_attribute_chains_and_literal_subscripts_are_references() {
    let source = "class C:\n    value: int | None = None\nclass B:\n    c: C = C()\nclass A:\n    b: B = B()\ndef use(a: A, d: dict[str, int | None], t: tuple[int | None, str]):\n    if a.b.c.value is not None:\n        a.b.c.value #deep\n    if d['k'] is not None:\n        d['k'] #key\n        d['other'] #other\n    if t[0] is not None:\n        t[0] #index\n    if a.b.c.value:\n        a.b.c.value #truthy\n";
    check(source, "a.b.c.value #deep", "a.b.c.value", &["int"]);
    check(source, "d['k'] #key", "d['k']", &["int"]);
    check(source, "d['other'] #other", "d['other']", &["int", "None"]);
    check(source, "t[0] #index", "t[0]", &["int"]);
    check(source, "a.b.c.value #truthy", "a.b.c.value", &["int"]);
}

#[test]
fn isinstance_typeguard_and_boolean_context_narrow_attributes() {
    let source = "from typing import TypeGuard\ndef is_text(value: object) -> TypeGuard[str]: return isinstance(value, str)\nclass Box:\n    item: int | str = 0\ndef use(box: Box):\n    if isinstance(box.item, str):\n        box.item #isinstance\n    else:\n        box.item #else\n    if is_text(box.item):\n        box.item #guard\n    box.item.upper() if isinstance(box.item, str) else box.item #ternary\n";
    check(source, "box.item #isinstance", "box.item", &["str"]);
    check(source, "box.item #else", "box.item", &["int"]);
    check(source, "box.item #guard", "box.item", &["str"]);
    check(source, "box.item #ternary", "box.item", &["int"]);
    let source = format!("{HOLDER}def use(h: Holder):\n    h.node is not None and h.node #and\n");
    check(&source, "h.node #and", "h.node", &["Node"]);
}

#[test]
fn writes_to_the_reference_or_a_prefix_retire_narrowing_but_calls_keep_it() {
    let source = format!(
        "{HOLDER}def use(h: Holder, other: Holder):\n    if h.node is not None:\n        work()\n        h.node #call\n        h.node = make()\n        h.node #assigned\n    if h.node is not None:\n        h = other\n        h.node #prefix\n    if h.node is not None:\n        del h.node\n        h.node #deleted\n    h.node = Node()\n    h.node #stored\n"
    );
    check(&source, "h.node #call", "h.node", &["Node"]);
    check(&source, "h.node #assigned", "h.node", &["Node", "None"]);
    check(&source, "h.node #prefix", "h.node", &["Node", "None"]);
    check(&source, "h.node #deleted", "h.node", &["Node", "None"]);
    check(&source, "h.node #stored", "h.node", &["Node"]);
    let source = "class C:\n    value: int | None = None\nclass B:\n    c: C = C()\ndef use(b: B, d: dict[str, int | None], key: str):\n    if b.c.value is not None:\n        b.c = C()\n        b.c.value #parent\n    if d['k'] is not None:\n        d[key] = None\n        d['k'] #dynamic\n";
    check(source, "b.c.value #parent", "b.c.value", &["int", "None"]);
    check(source, "d['k'] #dynamic", "d['k']", &["int", "None"]);
}

#[test]
fn member_match_subjects_narrow_the_reference() {
    let source = format!(
        "{HOLDER}def use(h: Holder):\n    match h.node:\n        case None:\n            return\n        case _:\n            h.node #match\n"
    );
    check(&source, "h.node #match", "h.node", &["Node"]);
}
