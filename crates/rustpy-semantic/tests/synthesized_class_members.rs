//! Generated `__match_args__`, ordering dunders and class-form `NamedTuple`
//! tuple behaviour (dataclasses, attrs, `typing.NamedTuple`).
use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn query(source: &str, expression: &str, marker: &str) -> Type {
    let db = Analysis::for_project(Path::new("/synthesized-members"));
    let id = db.set_file_text(Path::new("/synthesized-members/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let needle = format!("{expression} #{marker}");
    assert_eq!(source.matches(&needle).count(), 1, "{needle}");
    let offset = source.find(&needle).unwrap();
    let node = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .find(|node| {
            usize::from(node.text_range().start()) == offset
                && usize::from(node.text_range().len()) == expression.len()
        })
        .unwrap();
    Inferer::new(&db).type_of_expr(&file, &node)
}
fn check(source: &str, cases: &[(&str, &str, &str)]) {
    for (expression, marker, expected) in cases {
        assert_eq!(
            query(source, expression, marker).to_string(),
            *expected,
            "{marker}"
        );
    }
}
#[test]
fn class_namedtuple_is_a_tuple_of_its_field_types() {
    let source = "from typing import NamedTuple\nclass Point(NamedTuple):\n    x: int\n    label: str\n    def norm(self) -> float: return 0.0\np = Point(1, 'a')\np[0] #first\np[1] #second\np[-1] #negative\np[0:1] #slice\na, b = p\na #a\nb #b\nfor item in p:\n    item #item\nlen(p) #len\np._fields #fields\np.x #attr\nclass Sub(Point):\n    pass\ns = Sub(1, 'b')\ns[1] #sub\n";
    check(
        source,
        &[
            ("p[0]", "first", "int"),
            ("p[1]", "second", "str"),
            ("p[-1]", "negative", "str"),
            ("p[0:1]", "slice", "tuple[int]"),
            ("a", "a", "int"),
            ("b", "b", "str"),
            ("item", "item", "int | str"),
            ("len(p)", "len", "int"),
            (
                "p._fields",
                "fields",
                "tuple[Literal[\"x\"], Literal[\"label\"]]",
            ),
            ("p.x", "attr", "int"),
            ("s[1]", "sub", "str"),
        ],
    );
}
#[test]
fn generated_match_args_type_class_pattern_captures() {
    let dataclass = "from dataclasses import dataclass, KW_ONLY\n@dataclass\nclass Point:\n    x: int\n    y: str\n    _: KW_ONLY\n    z: float\ndef f(p: Point):\n    match p:\n        case Point(x, y):\n            x #x\n            y #y\nPoint.__match_args__ #args\n";
    check(
        dataclass,
        &[
            ("x", "x", "int"),
            ("y", "y", "str"),
            (
                "Point.__match_args__",
                "args",
                "tuple[Literal[\"x\"], Literal[\"y\"]]",
            ),
        ],
    );
    let keyword_only = "from dataclasses import dataclass\n@dataclass(kw_only=True)\nclass K:\n    a: int\nK.__match_args__ #kw\n@dataclass(match_args=False)\nclass N:\n    a: int\n";
    check(keyword_only, &[("K.__match_args__", "kw", "tuple[()]")]);
    let named = "from typing import NamedTuple\nclass Pair(NamedTuple):\n    left: int\n    right: str\ndef f(p: Pair):\n    match p:\n        case Pair(l, r):\n            l #l\n            r #r\n";
    check(named, &[("l", "l", "int"), ("r", "r", "str")]);
    let attrs = "import attr\n@attr.s(auto_attribs=True)\nclass A:\n    first: int\n    second: str\ndef f(a: A):\n    match a:\n        case A(one, two):\n            two #two\n";
    check(attrs, &[("two", "two", "str")]);
}
#[test]
fn ordering_dunders_are_generated_for_order_and_total_ordering() {
    let source = "from dataclasses import dataclass\nfrom functools import total_ordering\n@dataclass(order=True)\nclass V:\n    a: int\nV(1) < V(2) #lt\n@total_ordering\nclass W:\n    def __eq__(self, other: object) -> bool: return True\n    def __lt__(self, other: 'W') -> bool: return True\nW() >= W() #ge\n@dataclass\nclass U:\n    a: int\n";
    check(
        source,
        &[("V(1) < V(2)", "lt", "bool"), ("W() >= W()", "ge", "bool")],
    );
    let db = Analysis::for_project(Path::new("/synthesized-members"));
    let id = db.set_file_text(Path::new("/synthesized-members/m.py"), source);
    let file = db.file(id).unwrap();
    let infer = Inferer::new(&db);
    let class = |name: &str| {
        let scope = file.scopes.module_scope();
        let id = file.scopes.bindings_in(scope, name)[0];
        infer.type_of_binding(&file, id)
    };
    assert!(infer
        .synthesized_ordering_member(&class("V"), "__ge__")
        .is_some());
    assert!(infer
        .synthesized_ordering_member(&class("W"), "__gt__")
        .is_some());
    assert!(infer
        .synthesized_ordering_member(&class("W"), "__lt__")
        .is_none());
    assert!(infer
        .synthesized_ordering_member(&class("U"), "__lt__")
        .is_none());
}
