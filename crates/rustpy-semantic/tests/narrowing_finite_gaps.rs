use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::{collections::BTreeSet, path::Path};

fn check(source: &str, needle: &str, expected: &[&str]) {
    let db = Analysis::for_project(Path::new("/narrowing-gaps"));
    let id = db.set_file_text(Path::new("/narrowing-gaps/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let offset = source.find(needle).expect("unique selected read");
    assert_eq!(source.matches(needle).count(), 1);
    let name = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|name| name.text_range().start() == rustpy_core::TextSize::new(offset as u32))
        .unwrap();
    let actual = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Name(name));
    let values = match actual {
        Type::Union(values) => values,
        value => vec![value],
    };
    let actual: BTreeSet<_> = values.into_iter().map(|value| value.to_string()).collect();
    let expected: BTreeSet<_> = expected.iter().map(|value| value.to_string()).collect();
    assert!(!actual.is_empty());
    assert_eq!(actual, expected, "{source}\n{needle}");
}

#[test]
fn typeguard_and_typeis_have_distinct_positive_and_negative_semantics() {
    let source="from typing import TypeGuard\ndef is_text(value:object)->TypeGuard[str]:return isinstance(value,str)\ndef use(value:int|str):\n    if is_text(value):\n        value #positive\n    else:\n        value #negative\n";
    check(source, "value #positive", &["str"]);
    check(source, "value #negative", &["int", "str"]);
    let source="from typing_extensions import TypeIs\ndef is_text(value:object)->TypeIs[str]:return isinstance(value,str)\ncheck_text=is_text\ndef use(value:int|str):\n    if check_text(value):\n        value #positive\n    else:\n        value #negative\n";
    check(source, "value #positive", &["str"]);
    check(source, "value #negative", &["int"]);
    let source="def ordinary(value:object)->bool:return isinstance(value,str)\ndef use(value:int|str):\n    if ordinary(value):\n        value #ordinary\n";
    check(source, "value #ordinary", &["int", "str"]);
}

#[test]
fn literal_attribute_and_typeddict_key_discriminants_narrow_the_owner() {
    let source="from typing import Literal\nclass Cat:\n    kind:Literal['cat']='cat'\nclass Dog:\n    kind:Literal['dog']='dog'\ndef use(value:Cat|Dog):\n    if value.kind=='cat':\n        value #positive\n    else:\n        value #negative\n";
    check(source, "value #positive", &["Cat"]);
    check(source, "value #negative", &["Dog"]);
    let source="from typing import Literal,TypedDict\nclass Cat(TypedDict):\n    kind:Literal['cat']\nclass Dog(TypedDict):\n    kind:Literal['dog']\ndef use(value:Cat|Dog):\n    if value['kind']=='cat':\n        value #positive\n    else:\n        value #negative\n";
    check(source, "value #positive", &["Cat"]);
    check(source, "value #negative", &["Dog"]);
}

#[test]
fn tuple_length_and_known_attribute_guards_require_actual_builtin_authority() {
    let source="def use(value:tuple[int]|tuple[str,str]):\n    if len(value)==1:\n        value #positive\n    else:\n        value #negative\n";
    check(source, "value #positive", &["tuple[int]"]);
    check(source, "value #negative", &["tuple[str, str]"]);
    let source="class A:\n    field:int=1\nclass B:pass\ndef use(value:A|B):\n    if hasattr(value,'field'):\n        value #positive\n    else:\n        value #negative\n";
    check(source, "value #positive", &["A"]);
    check(source, "value #negative", &["B"]);
    let source="def isinstance(*args)->bool:return True\ndef use(value:int|str):\n    if isinstance(value,int):\n        value #shadowed\n";
    check(source, "value #shadowed", &["int", "str"]);
}
