//! `type[X]` class-object forms, generic type-alias specialisation and the
//! real `collections` classes behind `typing.Counter`/`Deque`/... annotations.
use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

/// Infers each `expr  #marker` read: the expression spelled before the marker.
fn reads(source: &str) -> Vec<(String, String)> {
    let db = Analysis::for_project(Path::new("/type-forms"));
    let id = db.set_file_text(Path::new("/type-forms/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let inferer = Inferer::new(&db);
    let mut out = Vec::new();
    for line in source.lines() {
        let Some((code, marker)) = line.split_once("  #") else {
            continue;
        };
        let text = code.trim();
        let offset = source.find(line).unwrap() + (code.len() - code.trim_start().len());
        let expr = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::Expr::cast)
            .find(|e| {
                usize::from(e.syntax().text_range().start()) == offset && e.syntax().text() == text
            })
            .unwrap_or_else(|| panic!("no expression `{text}`"));
        out.push((
            marker.to_string(),
            inferer.type_of_expr(&file, &expr).to_string(),
        ));
    }
    out
}

fn check(source: &str, expected: &[(&str, &str)]) {
    let actual = reads(source);
    for (marker, ty) in expected {
        let found = actual
            .iter()
            .find(|(m, _)| m == marker)
            .unwrap_or_else(|| panic!("missing marker {marker}"));
        assert_eq!(found.1, *ty, "{marker} in:\n{source}");
    }
}

#[test]
fn type_forms_are_class_objects_and_construct_instances() {
    check(
        "from typing import Type, TypeVar, Any\nT = TypeVar('T')\nclass A:\n    @classmethod\n    def build(cls) -> 'A': ...\nclass B: ...\ndef f(a: type[A], b: Type[A], u: type[A | B], t: type[T], n: 'type[B]', anything: type[Any], bare: type):\n    a  #a\n    b  #b\n    u  #u\n    t  #t\n    n  #n\n    a()  #a()\n    b()  #b()\n    u()  #u()\n    t()  #t()\n    a.build()  #classmethod\n    anything  #any\n    bare  #bare\n",
        &[
            ("a", "type[A]"),
            ("b", "type[A]"),
            ("u", "type[A] | type[B]"),
            ("t", "type[T]"),
            ("n", "type[B]"),
            ("a()", "A"),
            ("b()", "A"),
            ("u()", "A | B"),
            ("t()", "T"),
            ("classmethod", "A"),
            ("any", "type[Any]"),
            ("bare", "type"),
        ],
    );
}

#[test]
fn type_of_typevar_parameters_solve_from_class_arguments() {
    check(
        "from typing import Type, TypeVar\nT = TypeVar('T')\nclass A: ...\nclass B: ...\ndef make(cls: type[T]) -> T: ...\ndef legacy(cls: Type[T]) -> list[T]: ...\ndef klass(value: T) -> type[T]: ...\ndef pick(flag: bool):\n    k = A if flag else B\n    return make(k)\nmake(A)  #make\nlegacy(B)  #legacy\nklass(A())  #klass\npick(True)  #union\ndef make695[S](cls: type[S]) -> S: ...\nmake695(B)  #pep695\n",
        &[
            ("make", "A"),
            ("legacy", "list[B]"),
            ("klass", "type[A]"),
            ("union", "A | B"),
            ("pep695", "B"),
        ],
    );
}

#[test]
fn old_style_generic_aliases_substitute_their_free_type_variables() {
    check(
        "from typing import TypeVar, Callable, Union\nT = TypeVar('T')\nU = TypeVar('U')\nAlias = dict[str, T]\nPairOf = tuple[T, U]\nMaybe = Union[T, None]\nHandler = Callable[[T], U]\nx: Alias[int]\nx  #dict\np: PairOf[int, str]\np  #tuple\nm: Maybe[bytes]\nm  #union\nh: Handler[int, str]\nh  #callable\nh(1)  #call\nbare: Alias\nbare  #bare\n",
        &[
            ("dict", "dict[str, int]"),
            ("tuple", "tuple[int, str]"),
            ("union", "bytes | None"),
            ("callable", "(p0: int, /) -> str"),
            ("call", "str"),
            ("bare", "dict[str, Any]"),
        ],
    );
}

#[test]
fn pep695_aliases_specialise_with_defaults() {
    check(
        "type Pair[T] = tuple[T, T]\ntype M[T] = T | list[T]\ntype Fn[A, R] = Callable[[A], R]\ntype WithDefault[T = int] = list[T]\nfrom typing import Callable\np: Pair[int]\np  #pair\nm: M[str]\nm  #union\nf: Fn[int, bytes]\nf(1)  #callable\nd: WithDefault\nd  #default\nb: Pair\nb  #bare\n",
        &[
            ("pair", "tuple[int, int]"),
            ("union", "str | list[str]"),
            ("callable", "bytes"),
            ("default", "list[int]"),
            ("bare", "tuple[Any, Any]"),
        ],
    );
}

#[test]
fn typing_collection_aliases_resolve_to_real_collections_classes() {
    check(
        "import collections\nimport typing\nfrom typing import Counter, Deque, OrderedDict, DefaultDict, ChainMap\ndef f(c: Counter[str], d: Deque[int], o: OrderedDict[str, int], dd: DefaultDict[str, int], cm: ChainMap[str, int], cc: collections.Counter[str], tc: typing.Counter[str], bare: Counter):\n    c  #c\n    d  #d\n    o  #o\n    dd  #dd\n    cm  #cm\n    cc  #cc\n    tc  #tc\n    c.most_common()  #most_common\n    d.appendleft(1)  #appendleft\n    d.popleft()  #popleft\n    o.move_to_end('k')  #move_to_end\n    dd.default_factory  #factory\n    cm.maps  #maps\n    bare.most_common  #bare\n",
        &[
            ("c", "Counter[str]"),
            ("d", "deque[int]"),
            ("o", "OrderedDict[str, int]"),
            ("dd", "defaultdict[str, int]"),
            ("cm", "ChainMap[str, int]"),
            ("cc", "Counter[str]"),
            ("tc", "Counter[str]"),
            ("most_common", "list[tuple[str, int]]"),
            ("appendleft", "None"),
            ("popleft", "int"),
            ("move_to_end", "None"),
            ("factory", "() -> int | None"),
            ("maps", "list[MutableMapping[str, int]]"),
            ("bare", "(n: int | None = None) -> list[tuple[Unknown, int]]"),
        ],
    );
}

#[test]
fn casting_to_any_yields_any_even_through_an_import_alias() {
    // typeshed declares `class Any`, but `type[_T]` must not bind it as a
    // class object: `cast(Any, x)` is `Any`, never an `Any` instance.
    check(
        "from typing import Any, Any as A2, cast, cast as c2\nclass C: ...\ncast(Any, None)  #plain\nc2(A2, None)  #aliased\ncast(C, None)  #class\n",
        &[("plain", "Any"), ("aliased", "Any"), ("class", "C")],
    );
}
