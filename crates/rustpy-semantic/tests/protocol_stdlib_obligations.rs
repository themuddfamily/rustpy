use rustpy_semantic::{
    db::Analysis,
    infer::Inferer,
    types::{LiteralValue, Type},
};
use std::path::Path;
#[test]
fn builtin_literal_satisfies_supports_index_without_protocol_scaffolding() {
    let db = Analysis::for_project(Path::new("/stdlib-protocol-index"));
    let file = db.module("typing").unwrap();
    let inf = Inferer::new(&db);
    let definition = inf
        .resolve_name(&file, file.scopes.module_scope(), "SupportsIndex")
        .unwrap();
    let Type::Class(class, args) = inf.type_of_binding(&definition.file, definition.binding) else {
        panic!("SupportsIndex class required")
    };
    assert!(inf.is_compatible(
        &Type::Literal(LiteralValue::Int(1)),
        &Type::Instance(class, args)
    ));
}
#[test]
fn iterable_element_placeholder_accepts_int_but_concrete_wrong_type_rejects() {
    let db = Analysis::for_project(Path::new("/stdlib-protocol-iterable"));
    let file = db.module("typing").unwrap();
    let inf = Inferer::new(&db);
    let definition = inf
        .resolve_name(&file, file.scopes.module_scope(), "Iterable")
        .unwrap();
    let Type::Class(class, _) = inf.type_of_binding(&definition.file, definition.binding) else {
        panic!("Iterable class required")
    };
    let actual = Type::Instance(
        inf.builtin_class("list").unwrap(),
        vec![inf.instance_of("int")],
    );
    assert!(inf.is_compatible(
        &actual,
        &Type::Instance(class.clone(), vec![Type::TypeVar("T".into())])
    ));
    assert!(inf.is_compatible(
        &actual,
        &Type::Instance(class.clone(), vec![inf.instance_of("int")])
    ));
    assert!(!inf.is_compatible(
        &actual,
        &Type::Instance(class, vec![inf.instance_of("str")])
    ));
}
