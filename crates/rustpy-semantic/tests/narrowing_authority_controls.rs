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
fn predicate_alias_head_retirement_and_unknown_arguments_keep_authority() {
    let source="from typing import TypeGuard as TG\nAlias=TG\ndef is_text(value:object)->Alias[str]:return isinstance(value,str)\ndef use(value:int|str):\n    if is_text(value):\n        value #canonical_alias\n";
    check(source, "value #canonical_alias", &["str"]);
    let source="from typing import TypeGuard\ndef is_text(value:object)->TypeGuard[str]:return isinstance(value,str)\ndef ordinary(value:object)->bool:return True\nguard=is_text\nguard=ordinary\ndef use(value:int|str):\n    if guard(value):\n        value #retired\n";
    check(source, "value #retired", &["int", "str"]);
    let source="class TypeGuard[T]:pass\ndef predicate(value:object)->TypeGuard[str]:return TypeGuard[str]()\ndef use(value:int|str):\n    if predicate(value):\n        value #fake_head\n";
    check(source, "value #fake_head", &["int", "str"]);
    let source="from typing import TypeGuard\ndef is_text(value:object)->TypeGuard[str]:return isinstance(value,str)\ndef use(value:int|str,extra:tuple[object,...]):\n    if is_text(value,*extra):\n        value #unknown_star\n";
    check(source, "value #unknown_star", &["int", "str"]);
    let source="from typing import TypeGuard\ndef predicate(first:object,second:object)->TypeGuard[str]:return isinstance(first,str)\ndef use(value:int|str,other:object):\n    if predicate(other,value):\n        value #second_arg\n";
    check(source, "value #second_arg", &["int", "str"]);
}

#[test]
fn builtin_alias_dynamic_attributes_and_projected_writes_are_conservative() {
    let source="test=isinstance\ndef use(value:int|str):\n    if test(value,str):\n        value #builtin_alias\n";
    check(source, "value #builtin_alias", &["str"]);
    let source="def len(value)->int:return 1\ndef use(value:tuple[int]|tuple[str,str]):\n    if len(value)==1:\n        value #fake_len\n";
    check(
        source,
        "value #fake_len",
        &["tuple[int]", "tuple[str, str]"],
    );
    let source="class A:\n    field:int=1\nclass Dynamic:\n    def __getattr__(self,name:str)->object:return 1\ndef use(value:A|Dynamic):\n    if hasattr(value,'field'):\n        value #dynamic\n";
    check(source, "value #dynamic", &["A", "Dynamic"]);
    let source="from typing import Literal\nclass Cat:\n    kind:Literal['cat']='cat'\nclass Dog:\n    kind:Literal['dog']='dog'\ndef use(value:Cat|Dog):\n    if value.kind=='cat':\n        value.kind='dog'\n        value #mutated\n";
    check(source, "value #mutated", &["Cat", "Dog"]);
}
