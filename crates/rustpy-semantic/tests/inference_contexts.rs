//! Lambda parameters from call-site context, the implicit `__class__` cell,
//! PEP 695 annotation scopes inside classes, match captures on member
//! subjects and `dataclass_transform` `__match_args__`.
use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

/// The type of the expression spelled exactly by `needle` (unique in `source`).
fn infer_at(source: &str, needle: &str) -> String {
    let db = Analysis::for_project(Path::new("/inference-contexts"));
    let id = db.set_file_text(Path::new("/inference-contexts/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    assert_eq!(source.matches(needle).count(), 1, "{needle}");
    let start = source.find(needle).unwrap();
    // The longest expression starting at the needle that the needle spells.
    let expression = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .filter(|e| {
            usize::from(e.text_range().start()) == start
                && needle.starts_with(&e.syntax().text().to_string())
        })
        .max_by_key(|e| e.text_range().len())
        .unwrap_or_else(|| panic!("no expression at {needle}"));
    Inferer::new(&db)
        .type_of_expr(&file, &expression)
        .to_string()
}

const PEOPLE: &str = "from typing import Callable, overload\nclass Person:\n    name: str\npeople: list[Person] = []\nnames: list[str] = []\n";

#[test]
fn sorted_key_lambda_parameter_comes_from_iterable() {
    let source = format!("{PEOPLE}sorted(people, key=lambda p: p.name)\n");
    assert_eq!(infer_at(&source, "p.name"), "str");
    let source = format!("{PEOPLE}sorted(people, key=lambda person: person)\n");
    assert_eq!(infer_at(&source, "person)"), "Person");
}

#[test]
fn map_and_list_sort_lambda_parameters() {
    let source = format!("{PEOPLE}map(lambda s: s.upper(), names)\n");
    assert_eq!(infer_at(&source, "s.upper()"), "str");
    let source = format!("{PEOPLE}people.sort(key=lambda q: q.name)\n");
    assert_eq!(infer_at(&source, "q.name"), "str");
}

#[test]
fn annotated_callable_assignment_argument_and_return() {
    let source = format!("{PEOPLE}f: Callable[[int], str] = lambda x: str(x)\n");
    assert_eq!(infer_at(&source, "x)"), "int");
    let source = format!(
        "{PEOPLE}def use(cb: Callable[[int, bytes], None]) -> None: ...\nuse(lambda a, b: (b))\n"
    );
    assert_eq!(infer_at(&source, "b))"), "bytes");
    let source =
        format!("{PEOPLE}def make() -> Callable[[float], int]:\n    return lambda d: int(d)\n");
    assert_eq!(infer_at(&source, "d)"), "float");
}

#[test]
fn overloads_choose_the_lambda_parameter_type() {
    let source = format!(
        "{PEOPLE}@overload\ndef g(x: int, cb: Callable[[int], None]) -> None: ...\n@overload\ndef g(x: str, cb: Callable[[str], None]) -> None: ...\ndef g(x, cb): ...\ng('a', lambda v: (v))\n"
    );
    assert_eq!(infer_at(&source, "v))"), "str");
}

#[test]
fn lambda_without_context_stays_unknown() {
    let source = "h = lambda w: (w)\n";
    assert_eq!(infer_at(source, "w)\n"), "Unknown");
}

#[test]
fn implicit_class_cell_in_methods() {
    let source = "class C:\n    def m(self):\n        return __class__\n";
    assert_eq!(infer_at(source, "__class__"), "type[C]");
    let source = "class C:\n    def m(self):\n        f = lambda: (__class__)\n";
    assert_eq!(infer_at(source, "__class__)"), "type[C]");
    let source = "def free():\n    return __class__\n";
    assert_ne!(infer_at(source, "__class__"), "type[C]");
}

#[test]
fn annotation_scope_sees_enclosing_class_names() {
    let source = "class O:\n    class I:\n        v: int\n    def m[T](self, x: I, t: T) -> T:\n        x.v #use\n        return t\n";
    assert_eq!(infer_at(source, "x.v #"), "int");
    // The function body itself still does not see the class scope.
    let source = "class O:\n    class I: ...\n    def m[T](self) -> T:\n        return (I)\n";
    assert_eq!(infer_at(source, "I)"), "Unknown");
}

#[test]
fn match_captures_on_member_subjects() {
    let source = "from dataclasses import dataclass\n@dataclass\nclass Foo:\n    x: int\n    y: str\nclass Holder:\n    node: Foo | None\n    def run(self):\n        match self.node:\n            case Foo(x=v):\n                v #kw\n            case Foo(_, w):\n                w #pos\n";
    assert_eq!(infer_at(source, "v #kw"), "int");
    assert_eq!(infer_at(source, "w #pos"), "str");
    let source = "class Box:\n    item: int | str\ndef f(b: Box):\n    match b.item:\n        case int() as n:\n            n #cap\n";
    assert_eq!(infer_at(source, "n #cap"), "int");
}

#[test]
fn dataclass_transform_classes_get_match_args() {
    let base = "from typing import dataclass_transform\n@dataclass_transform()\nclass ModelBase:\n    def __init_subclass__(cls, **kw) -> None: ...\nclass Point(ModelBase):\n    x: int\n    y: str\n";
    let source = format!(
        "{base}def f(p: Point):\n    match p:\n        case Point(a, b):\n            b #cap\n"
    );
    assert_eq!(infer_at(&source, "b #cap"), "str");
    let source = format!("{base}def f(p: Point | None):\n    match p:\n        case Point(y=b):\n            b #cap\n");
    assert_eq!(infer_at(&source, "b #cap"), "str");
    let source = format!("{base}Point.__match_args__\n");
    assert_eq!(
        infer_at(&source, "Point.__match_args__"),
        "tuple[Literal[\"x\"], Literal[\"y\"]]"
    );
    // Decorator form, and keyword-only fields excluded.
    let source = "from typing import dataclass_transform\n@dataclass_transform(kw_only_default=True)\ndef model(cls): return cls\n@model\nclass Q:\n    a: int\nQ.__match_args__\n";
    assert_eq!(infer_at(source, "Q.__match_args__"), "tuple[()]");
    // `match_args=False` opts out.
    let source = format!("{base}class R(ModelBase, match_args=False):\n    z: int\ndef f(r: R):\n    match r:\n        case R(c):\n            c #none\n");
    assert_eq!(infer_at(&source, "c #none"), "Unknown");
}
