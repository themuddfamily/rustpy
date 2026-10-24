use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

fn actual_call_type(source: &str, call_text: &str) -> Type {
    let db = Analysis::for_project(Path::new("/contextual-constructors"));
    let id = db.set_file_text(Path::new("/contextual-constructors/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let calls: Vec<_> = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .filter(|call| call.syntax().text() == call_text)
        .collect();
    assert_eq!(calls.len(), 1, "unique actual call: {call_text}");
    Inferer::new(&db).type_of_expr(&file, &ast::Expr::Call(calls[0].clone()))
}

fn check(source: &str, call_text: &str, expected: &str) {
    assert_eq!(
        actual_call_type(source, call_text).to_string(),
        expected,
        "{source}"
    );
}

fn check_unknown_box(source: &str, call_text: &str) {
    let actual = actual_call_type(source, call_text);
    let Type::Instance(class, arguments) = &actual else {
        panic!("expected Box instance, got {actual}: {source}");
    };
    assert_eq!(class.name.as_str(), "Box", "{source}");
    assert_eq!(
        arguments.len(),
        1,
        "one declared scalar parameter: {source}"
    );
    assert!(
        arguments
            .iter()
            .all(|argument| matches!(argument, Type::Unknown)),
        "{actual:?}: {source}"
    );
    assert_eq!(
        actual.to_string(),
        "Box",
        "all-Unknown arguments are omitted: {source}"
    );
}

#[test]
fn missing_constructor_arguments_use_real_annotation_return_and_argument_contexts() {
    check(
        "class Box[T]:pass\nvalue:Box[str]=Box()\n",
        "Box()",
        "Box[str]",
    );
    check(
        "class Box[T]:pass\ndef make()->Box[str]:return Box()\n",
        "Box()",
        "Box[str]",
    );
    check(
        "class Box[T]:pass\ndef accept(value:Box[str])->None:pass\naccept(Box())\n",
        "Box()",
        "Box[str]",
    );
    check_unknown_box("class Box[T]:pass\nvalue=Box()\n", "Box()");
}

#[test]
fn explicit_and_argument_derived_constructor_choices_are_not_expected_stamped() {
    check(
        "class Box[T]:\n    def __init__(self,value:T)->None:pass\nvalue:Box[str]=Box(1)\n",
        "Box(1)",
        "Box[int]",
    );
    check(
        "class Box[T]:pass\nvalue:Box[str]=Box[int]()\n",
        "Box[int]()",
        "Box[int]",
    );
    check(
        "class Box[T]:\n    def __init__(self,value:T)->None:pass\nvalue:Box[str]=Box('ok')\n",
        "Box('ok')",
        "Box[str]",
    );
}

#[test]
fn contextual_constructor_solutions_validate_bounds_and_constraints() {
    check(
        "class Box[T:str]:pass\nvalue:Box[str]=Box()\n",
        "Box()",
        "Box[str]",
    );
    check_unknown_box("class Box[T:str]:pass\nvalue:Box[int]=Box()\n", "Box()");
    check(
        "class Box[T:(str,bytes)]:pass\nvalue:Box[bytes]=Box()\n",
        "Box()",
        "Box[bytes]",
    );
    check_unknown_box(
        "class Box[T:(str,bytes)]:pass\nvalue:Box[int]=Box()\n",
        "Box()",
    );
}

#[test]
fn contextual_solutions_precede_defaults_without_overriding_explicit_arguments() {
    check(
        "class Box[T=int]:pass\nvalue:Box[str]=Box()\n",
        "Box()",
        "Box[str]",
    );
    check("class Box[T=int]:pass\nvalue=Box()\n", "Box()", "Box[int]");
    check(
        "class Box[T=int]:pass\nvalue:Box[str]=Box[int]()\n",
        "Box[int]()",
        "Box[int]",
    );
    check(
        "class Pair[T,U=list[T]]:pass\nvalue:Pair[str]=Pair()\n",
        "Pair()",
        "Pair[str, list[str]]",
    );
}

#[test]
fn custom_runtime_dispatch_and_factories_preserve_their_actual_return() {
    check("class Meta(type):\n    def __call__(cls)->int:return 1\nclass Box[T](metaclass=Meta):pass\nvalue:Box[str]=Box()\n", "Box()", "int");
    check("class Foreign:pass\nclass Box[T]:\n    def __new__(cls)->Foreign:return Foreign()\nvalue:Box[str]=Box()\n", "Box()", "Foreign");
    check(
        "class Box[T]:pass\ndef factory()->int:return 1\nvalue:Box[str]=factory()\n",
        "factory()",
        "int",
    );
    check(
        "def opaque(cls):return cls\n@opaque\nclass Box[T]:pass\nvalue:Box[str]=Box()\n",
        "Box()",
        "Box",
    );
}
