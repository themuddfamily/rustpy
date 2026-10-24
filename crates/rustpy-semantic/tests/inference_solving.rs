//! TypeVar solving through protocols, truthiness-aware `and`/`or` values and
//! iteration through the legacy `__getitem__` protocol and canonical classes.
use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

/// The type of the last expression statement anywhere in `source`.
fn inferred(source: &str) -> String {
    let db = Analysis::for_project(Path::new("/inference-solving"));
    let id = db.set_file_text(Path::new("/inference-solving/main.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let last = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::ExprStmt::cast)
        .last()
        .expect("expression statement fixture");
    Inferer::new(&db)
        .type_of_expr(&file, &last.expr().unwrap())
        .to_string()
}

#[test]
fn typevar_solves_through_typeshed_protocol_members() {
    let source = "from typing import SupportsAbs, TypeVar\nT = TypeVar('T')\ndef f(x: SupportsAbs[T]) -> T: ...\nf(-3)\n";
    assert_eq!(inferred(source), "int");
}

#[test]
fn list_constructor_solves_iterable_from_a_structural_iter_method() {
    let source = "from typing import Iterator\nclass Words:\n    def __iter__(self) -> Iterator[str]: ...\nlist(Words())\n";
    assert_eq!(inferred(source), "list[str]");
}

#[test]
fn typevar_solves_through_user_protocols_and_requires_every_member() {
    let protocol = "from typing import Protocol, TypeVar\nT = TypeVar('T')\nclass Box(Protocol[T]):\n    def get(self) -> T: ...\ndef unbox(b: Box[T]) -> T: ...\n";
    let matching =
        format!("{protocol}class IntBox:\n    def get(self) -> int: return 1\nunbox(IntBox())\n");
    assert_eq!(inferred(&matching), "int");
    let missing =
        format!("{protocol}class Other:\n    def put(self) -> int: return 1\nunbox(Other())\n");
    assert_ne!(inferred(&missing), "int");
}

#[test]
fn or_keeps_the_truthy_part_of_earlier_operands() {
    let with = |annotation: &str, expression: &str| {
        inferred(&format!(
            "class Plain: pass\ndef g(x: {annotation}):\n    {expression}\n"
        ))
    };
    assert_eq!(with("str | None", "x or 'd'"), "str");
    assert_eq!(with("int", "x or 'd'"), "int | str");
    assert_eq!(with("Plain | None", "x or 1"), "Plain | int");
    assert_eq!(with("object", "x or 1"), "object | int");
    assert_eq!(with("bool", "x or 'd'"), "bool | str");
    assert_eq!(inferred("None or 5\n"), "int");
    assert_eq!(inferred("0 or ''\n"), "str");
    assert_eq!(inferred("'' or 0 or 1.5\n"), "float");
}

#[test]
fn and_keeps_the_falsy_part_of_earlier_operands() {
    let with = |annotation: &str, expression: &str| {
        inferred(&format!(
            "class Plain: pass\ndef g(x: {annotation}):\n    {expression}\n"
        ))
    };
    assert_eq!(with("str | None", "x and 1"), "str | None | int");
    // Instances without __bool__/__len__ are always truthy.
    assert_eq!(with("Plain", "x and 1"), "int");
    assert_eq!(with("Plain | None", "x and 1"), "None | int");
    assert_eq!(inferred("1 and 'a'\n"), "str");
    assert_eq!(inferred("None and 1\n"), "None");
}

#[test]
fn iteration_falls_back_to_the_legacy_getitem_protocol() {
    let source = "class Seq:\n    def __getitem__(self, index: int) -> float: ...\nfor item in Seq():\n    item\n";
    assert_eq!(inferred(source), "float");
    let comprehension =
        "class Seq:\n    def __getitem__(self, index: int) -> float: ...\n[v for v in Seq()]\n";
    assert_eq!(inferred(comprehension), "list[float]");
    // A mapping-style __getitem__ that rejects ints does not make a sequence.
    let keyed = "class Keyed:\n    def __getitem__(self, key: str) -> float: ...\nfor item in Keyed():\n    item\n";
    assert_ne!(inferred(keyed), "float");
}

#[test]
fn iteration_identifies_canonical_classes_by_identity_not_name() {
    let user_list = "from typing import Generic, Iterator, TypeVar\nT = TypeVar('T')\nclass list(Generic[T]):\n    def __iter__(self) -> Iterator[bytes]: ...\ndef g(xs: list[int]):\n    for x in xs:\n        x\n";
    assert_eq!(inferred(user_list), "bytes");
    let user_iterator = "from typing import Generic, TypeVar\nT = TypeVar('T')\nclass Iterator(Generic[T]):\n    def __next__(self) -> bytes: ...\nclass Source:\n    def __iter__(self) -> Iterator[int]: ...\nfor y in Source():\n    y\n";
    assert_eq!(inferred(user_iterator), "bytes");
    // The real typeshed classes keep their element shortcuts.
    let builtin = "def g(xs: list[int], d: dict[str, int]):\n    for k in d:\n        k\n";
    assert_eq!(inferred(builtin), "str");
}
