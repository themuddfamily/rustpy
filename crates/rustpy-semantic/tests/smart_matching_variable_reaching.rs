use rustpy_core::TextSize;
use rustpy_semantic::{
    completion2::{expected_type, smart_complete},
    db::Analysis,
    infer::Inferer,
};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
#[test]
fn matching_variable_ranks_its_current_value_not_a_later_assignment() {
    let source = "def take(x:int):pass\ndef run():\n    value=1\n    value #actual_current\n    take(v)\n    value='future'\n";
    let db = Analysis::for_project(Path::new("/smart-current-variable"));
    let id = db.set_file_text(Path::new("/smart-current-variable/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let name = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|n| {
            usize::from(n.text_range().start()) == source.find("value #actual_current").unwrap()
        })
        .unwrap();
    let current = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Name(name));
    assert!(
        current.to_string() == "Literal[1]" || current.to_string() == "int",
        "actual current variable BEFORE completion: {current}"
    );
    let offset = TextSize::new((source.find("take(v)").unwrap() + 6) as u32);
    let (expected, _) = expected_type(&db, &file, offset).unwrap();
    assert_eq!(expected.to_string(), "int");
    let items = smart_complete(&db, &file, offset);
    let item = items
        .iter()
        .find(|i| i.label == "value")
        .expect("real matching local variable");
    assert_eq!(
        item.priority, 0,
        "current int value must rank compatible despite later returningstr store: {items:?}"
    );
}
#[test]
fn returning_current_variable_is_not_ranked_by_a_later_integer_store() {
    let source = "def take(x:int):pass\ndef run():\n    value='now'\n    value #actual_current\n    take(v)\n    value=1\n";
    let db = Analysis::for_project(Path::new("/smart-current-variable"));
    let id = db.set_file_text(Path::new("/smart-current-variable/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let name = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|n| {
            usize::from(n.text_range().start()) == source.find("value #actual_current").unwrap()
        })
        .unwrap();
    let current = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Name(name));
    assert!(
        current.to_string() == "Literal[\"now\"]" || current.to_string() == "str",
        "actual current variable BEFORE completion: {current}"
    );
    let offset = TextSize::new((source.find("take(v)").unwrap() + 6) as u32);
    let (expected, _) = expected_type(&db, &file, offset).unwrap();
    assert_eq!(expected.to_string(), "int");
    let items = smart_complete(&db, &file, offset);
    let item = items.iter().find(|i| i.label == "value").unwrap();
    assert!(
        item.priority >= 10,
        "currentstr variable must not be ranked intcompatible via futurestore: {items:?}"
    );
}
