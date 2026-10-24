use rustpy_semantic::{
    db::Analysis,
    infer::Inferer,
    types::{LiteralValue, Type},
};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn read(source: &str, marker: &str) -> Type {
    let db = Analysis::for_project(Path::new("/match-authority"));
    let id = db.set_file_text(Path::new("/match-authority/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    assert_eq!(source.matches(marker).count(), 1);
    let point = source.find(marker).unwrap();
    let name = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|name| usize::from(name.text_range().start()) == point)
        .unwrap();
    Inferer::new(&db).type_of_expr(&file, &ast::Expr::Name(name))
}
#[test]
fn integer_value_pattern_uses_python_bool_numeric_equality() {
    let source="from typing import Literal\ndef use(value:Literal[True,2]):\n    value #declared\n    match value:\n        case 1:\n            value #matched\n        case _:\n            value #remaining\n";
    let Type::Union(arms) = read(source, "value #declared") else {
        panic!("actual union premise")
    };
    assert!(arms
        .iter()
        .any(|arm| matches!(arm, Type::Literal(LiteralValue::Bool(true)))));
    assert!(arms
        .iter()
        .any(|arm| matches!(arm, Type::Literal(LiteralValue::Int(2)))));
    assert!(matches!(
        read(source, "value #matched"),
        Type::Literal(LiteralValue::Bool(true))
    ));
    assert!(matches!(
        read(source, "value #remaining"),
        Type::Literal(LiteralValue::Int(2))
    ));
}
#[test]
fn guard_rebinding_cannot_narrow_current_value_as_the_captured_subject() {
    let source="def use(value:int):\n    value #declared\n    match value:\n        case int() if (value := 'text') and False:\n            pass\n        case int() as captured:\n            captured #oldsubject\n            value #current\n";
    assert_eq!(read(source, "value #declared").to_string(), "int");
    let actual = read(source, "value #current");
    let arms = match actual {
        Type::Union(arms) => arms,
        other => vec![other],
    };
    // Conservative old paths may remain, but the executed string store must
    // not disappear under a predicate about the original match subject.
    assert!(arms.iter().any(|arm| arm.to_string() == "str"), "{arms:?}");
    assert_eq!(read(source, "captured #oldsubject").to_string(), "int");
}

#[test]
fn nonlocal_function_definition_callback_retires_subject_name_projection() {
    let source="def use(value:object):\n    value #declared\n    def reset()->None:\n        nonlocal value\n        def value()->str:return 'changed'\n    match value:\n        case int() if reset():pass\n        case int() as captured:\n            captured #oldsubject\n            value #current\n";
    assert_eq!(read(source, "value #declared").to_string(), "object");
    // Retain declared uncertainty; do not emulate the callback body.
    assert_eq!(read(source, "value #current").to_string(), "object");
    assert_eq!(read(source, "captured #oldsubject").to_string(), "int");
}

#[test]
fn nonlocal_import_callback_retires_subject_name_projection() {
    let source="def use(value:object):\n    value #declared\n    def reset()->None:\n        nonlocal value\n        import math as value\n    match value:\n        case int() if reset():pass\n        case int() as captured:\n            captured #oldsubject\n            value #current\n";
    assert_eq!(read(source, "value #declared").to_string(), "object");
    // Retain declared uncertainty; do not emulate the callback body.
    assert_eq!(read(source, "value #current").to_string(), "object");
    assert_eq!(read(source, "captured #oldsubject").to_string(), "int");
}
