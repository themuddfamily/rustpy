use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn read(source: &str, marker: &str) -> String {
    let db = Analysis::for_project(Path::new("/membership-controls"));
    let id = db.set_file_text(Path::new("/membership-controls/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let offset = source.find(marker).unwrap();
    let name = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|name| usize::from(name.text_range().start()) == offset)
        .unwrap();
    Inferer::new(&db)
        .type_of_expr(&file, &ast::Expr::Name(name))
        .to_string()
}
#[test]
fn dynamic_and_nonliteral_containers_do_not_supply_complement_proofs() {
    for container in ["items", "(unknown,)"] {
        let source = format!("from typing import Literal\ndef f(value:Literal[1,2],items:list[int],unknown:int):\n    value #before\n    if value not in {container}:\n        value #after\n");
        assert_eq!(
            read(&source, "value #after"),
            read(&source, "value #before")
        );
    }
}
#[test]
fn broad_nonliteral_subject_is_not_fabricated_into_literal_complement() {
    let source =
        "def f(value:int):\n    value #before\n    if value not in (1,2):\n        value #after\n";
    assert_eq!(read(source, "value #before"), "int");
    assert_eq!(read(source, "value #after"), "int");
}
#[test]
fn integer_boolean_equal_members_are_removed_from_negative_branch() {
    let source = "from typing import Literal\ndef f(value:Literal[True,2]):\n    value #before\n    if value not in (1,):\n        value #after\n";
    assert!(read(source, "value #before").contains("True"));
    assert_eq!(read(source, "value #after"), "Literal[2]");
}
