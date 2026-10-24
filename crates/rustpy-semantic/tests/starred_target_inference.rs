use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

fn check(source: &str, producer: &str, producer_expected: &str, names: &[(&str, &str)]) {
    let db = Analysis::for_project(Path::new("/starred-target-inference"));
    let id = db.set_file_text(Path::new("/starred-target-inference/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let infer = Inferer::new(&db);
    let producers: Vec<_> = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .filter(|expression| expression.syntax().text() == producer)
        .collect();
    assert_eq!(producers.len(), 1, "unique concrete producer");
    assert_eq!(
        infer.type_of_expr(&file, &producers[0]).to_string(),
        producer_expected,
        "producer premise"
    );
    for (name, expected) in names {
        let reference = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::NameExpr::cast)
            .filter(|reference| reference.name_text() == *name)
            .last()
            .unwrap();
        assert_eq!(
            infer
                .type_of_expr(&file, &ast::Expr::Name(reference))
                .to_string(),
            *expected,
            "{name}: {source}"
        );
    }
}

#[test]
fn fixed_tuple_star_collects_middle_and_indexes_suffix_from_the_end() {
    check(
        "head,*middle,tail=(1,'a','b',True)\nhead\nmiddle\ntail\n",
        "(1,'a','b',True)",
        "tuple[int, str, str, bool]",
        &[("head", "int"), ("middle", "list[str]"), ("tail", "bool")],
    );
    check(
        "*prefix,tail=(1,2,b'end')\nprefix\ntail\n",
        "(1,2,b'end')",
        "tuple[int, int, bytes]",
        &[("prefix", "list[int]"), ("tail", "bytes")],
    );
}

#[test]
fn nested_and_homogeneous_star_targets_keep_list_and_element_types() {
    check(
        "head,(*middle,tail)=(1,('a','b',True))\nhead\nmiddle\ntail\n",
        "(1,('a','b',True))",
        "tuple[int, tuple[str, str, bool]]",
        &[("head", "int"), ("middle", "list[str]"), ("tail", "bool")],
    );
    check(
        "head,*middle,tail=[True,False,True]\nhead\nmiddle\ntail\n",
        "[True,False,True]",
        "list[bool]",
        &[("head", "bool"), ("middle", "list[bool]"), ("tail", "bool")],
    );
}

#[test]
fn empty_star_capture_is_a_list_and_plain_opaque_unpack_stays_unknown() {
    let db = Analysis::for_project(Path::new("/starred-target-inference"));
    let id = db.set_file_text(
        Path::new("/starred-target-inference/m.py"),
        "head,*middle,tail=(1,True)\nmiddle\n",
    );
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let reference = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .filter(|reference| reference.name_text() == "middle")
        .last()
        .unwrap();
    let actual = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Name(reference));
    let Type::Instance(class, arguments) = actual else {
        panic!("empty capture must remain a list");
    };
    assert_eq!(class.name.as_str(), "list");
    assert!(
        arguments.is_empty()
            || arguments
                .iter()
                .all(|argument| matches!(argument, Type::Unknown))
    );
    check(
        "def f(source:object):\n    left,right=source\n    left\n    right\n",
        "source",
        "object",
        &[("left", "Unknown"), ("right", "Unknown")],
    );
}
