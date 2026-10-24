use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::{collections::BTreeSet, path::Path};
fn read(source: &str, expression: &str, marker: &str) -> Type {
    let db = Analysis::for_project(Path::new("/remaining-match-families"));
    let id = db.set_file_text(Path::new("/remaining-match-families/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let needle = format!("{expression} #{marker}");
    assert_eq!(source.matches(&needle).count(), 1);
    let start = source.find(&needle).unwrap();
    let node = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .find(|node| {
            usize::from(node.text_range().start()) == start
                && usize::from(node.text_range().len()) == expression.len()
        })
        .unwrap();
    Inferer::new(&db).type_of_expr(&file, &node)
}
fn check(source: &str, expression: &str, marker: &str, expected: &[&str]) {
    let ty = read(source, expression, marker);
    let arms = match ty {
        Type::Union(arms) => arms,
        other => vec![other],
    };
    let actual: BTreeSet<_> = arms.into_iter().map(|arm| arm.to_string()).collect();
    let expected: BTreeSet<_> = expected.iter().map(|item| item.to_string()).collect();
    assert_eq!(actual, expected, "{marker}");
}
#[test]
fn or_string_and_enum_value_patterns_need_real_finite_subject_domains() {
    let source="from typing import Literal\ndef strings(value:Literal['left','right','other']):\n    value #domain\n    match value:\n        case 'left' | 'right':\n            value #matched\n        case _:\n            value #remaining\n";
    check(
        source,
        "value",
        "domain",
        &[
            "Literal[\"left\"]",
            "Literal[\"right\"]",
            "Literal[\"other\"]",
        ],
    );
    check(
        source,
        "value",
        "matched",
        &["Literal[\"left\"]", "Literal[\"right\"]"],
    );
    check(source, "value", "remaining", &["Literal[\"other\"]"]);
    let bytes="from typing import Literal\ndef use(value:Literal[b'a',b'b']):\n    value #domain\n    match value:\n        case b'a': value #matched\n        case _: value #remaining\n";
    check(
        bytes,
        "value",
        "domain",
        &["Literal[b\"a\"]", "Literal[b\"b\"]"],
    );
    check(bytes, "value", "matched", &["Literal[b\"a\"]"]);
    check(bytes, "value", "remaining", &["Literal[b\"b\"]"]);
    let none="def use(value:int|None):\n    value #domain\n    match value:\n        case None: value #matched\n        case _: value #remaining\n";
    check(none, "value", "domain", &["int", "None"]);
    check(none, "value", "matched", &["None"]);
    check(none, "value", "remaining", &["int"]);
    let singleton="from typing import Literal\ndef use(value:Literal[True,False]):\n    value #domain\n    match value:\n        case True: value #matched\n        case False: value #remaining\n";
    check(
        singleton,
        "value",
        "domain",
        &["Literal[True]", "Literal[False]"],
    );
    check(singleton, "value", "matched", &["Literal[True]"]);
    check(singleton, "value", "remaining", &["Literal[False]"]);
    let enums="from enum import Enum\nclass Color(Enum):\n    RED=1\n    BLUE=2\nColor.RED #member\ndef use(color:Color):\n    color #domain\n    match color:\n        case Color.RED: color #red\n        case Color.BLUE: color #blue\n";
    // Canonical Enum members need identity-bearing literal types, not raw
    // integer initializer types. A failed premise is a provider prerequisite.
    check(enums, "Color.RED", "member", &["Literal[Color.RED]"]);
    check(enums, "color", "domain", &["Color"]);
    check(enums, "color", "red", &["Literal[Color.RED]"]);
    check(enums, "color", "blue", &["Literal[Color.BLUE]"]);
}
#[test]
fn sequence_star_and_nested_captures_project_real_tuple_elements() {
    let source="def use(value:tuple[int,str,str,bool]):\n    value #domain\n    match value:\n        case [head,*middle,last]:\n            head #head\n            middle #middle\n            last #last\n";
    check(source, "value", "domain", &["tuple[int, str, str, bool]"]);
    check(source, "head", "head", &["int"]);
    check(source, "middle", "middle", &["list[str]"]);
    check(source, "last", "last", &["bool"]);
    let nested="def use(value:tuple[int,tuple[str]]):\n    value #domain\n    match value:\n        case (1,(nested,)):\n            nested #nested\n";
    check(nested, "value", "domain", &["tuple[int, tuple[str]]"]);
    check(nested, "nested", "nested", &["str"]);
    let opaque="def use(value:object):\n    value #domain\n    match value:\n        case [unknown]: unknown #unknown\n";
    check(opaque, "value", "domain", &["object"]);
    assert!(read(opaque, "unknown", "unknown").is_unknown());
}
#[test]
fn mapping_rest_and_user_class_positional_keyword_patterns_project_owned_values() {
    let mapping="def use(value:dict[str,int]):\n    value #domain\n    match value:\n        case {'count':count,**rest}:\n            count #count\n            rest #rest\n";
    check(mapping, "value", "domain", &["dict[str, int]"]);
    check(mapping, "count", "count", &["int"]);
    check(mapping, "rest", "rest", &["dict[str, int]"]);
    let classes="class Point:\n    __match_args__=('x','y')\n    def __init__(self):\n        self.x=1\n        self.y='text'\ndef positional(point:Point):\n    point.x #producerx\n    point.y #producery\n    match point:\n        case Point(x,y):\n            x #x\n            y #y\ndef keyword(point:Point):\n    match point:\n        case Point(y=text): text #text\n";
    check(classes, "point.x", "producerx", &["int"]);
    check(classes, "point.y", "producery", &["str"]);
    check(classes, "x", "x", &["int"]);
    check(classes, "y", "y", &["str"]);
    check(classes, "text", "text", &["str"]);
}
