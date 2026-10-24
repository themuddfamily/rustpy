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
fn later_argument_and_boolean_operand_stores_do_not_narrow_the_old_subject() {
    let source="from typing import TypeGuard\ndef is_text(value:object,ignored:object)->TypeGuard[str]:return isinstance(value,str)\ndef use(value:int|str):\n    if is_text(value,(value:=1)):\n        value #positive\n    else:\n        value #negative\n";
    check(source, "value #positive", &["int"]);
    check(source, "value #negative", &["int"]);
    let source="from typing import TypeGuard\ndef is_text(value:object)->TypeGuard[str]:return isinstance(value,str)\ndef use(value:int|str):\n    if is_text(value) and (value:=1):\n        value #boolean_store\n";
    check(source, "value #boolean_store", &["int"]);
    let source="def use(value:int|str):\n    if type(value) is (value:=str):\n        value #type_positive\n    else:\n        value #type_negative\n";
    check(source, "value #type_positive", &["type[str]"]);
    check(source, "value #type_negative", &["type[str]"]);
}

#[test]
fn property_presence_does_not_prove_hasattr_when_the_getter_can_raise() {
    let source="class A:\n    field:int=1\nclass C:\n    @property\n    def field(self)->int:raise AttributeError('field')\ndef use(value:A|C):\n    if hasattr(value,'field'):\n        value #positive\n    else:\n        value #negative\n";
    check(source, "value #positive", &["A", "C"]);
    check(source, "value #negative", &["C"]);
}

#[test]
fn unknown_parentage_is_not_a_disjoint_typeis_proof() {
    let source="from unavailable_parent import Base\nfrom typing_extensions import TypeIs\nclass C(Base):pass\ndef is_text(value:object)->TypeIs[str]:return isinstance(value,str)\ndef use(value:str|C):\n    if is_text(value):\n        value #positive\n    else:\n        value #negative\n";
    check(source, "value #positive", &["str", "C"]);
    check(source, "value #negative", &["C"]);
}

#[test]
fn oversized_declared_predicate_proof_keeps_the_unproved_input_type() {
    // This is a bounded-analysis fallback control, not an exact conformance
    // expectation for a checker with unlimited union-proof resources.
    let classes = (0..600)
        .map(|index| format!("class C{index}:pass\n"))
        .collect::<String>();
    let names = (0..600)
        .map(|index| format!("C{index}"))
        .collect::<Vec<_>>()
        .join(",");
    let source=format!("from typing import Union\nfrom typing_extensions import TypeIs\n{classes}def guard(value:object)->TypeIs[Union[{names}]]:return True\ndef use(value:object):\n    if guard(value):\n        value #bounded\n");
    check(&source, "value #bounded", &["object"]);
}
