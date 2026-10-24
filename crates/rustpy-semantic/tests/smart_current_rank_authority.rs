use rustpy_core::TextSize;
use rustpy_semantic::{
    completion2::{expected_type, smart_complete},
    db::Analysis,
    infer::Inferer,
    types::Type,
};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
#[test]
fn imported_terminal_branch_keeps_original_database_flow_authority() {
    for terminal in [true, false] {
        let db = Analysis::for_project(Path::new("/smart-original-authority"));
        let library = if terminal {
            "from typing import Never\ndef stop()->Never:raise RuntimeError\n"
        } else {
            "def stop()->int:return 0\n"
        };
        db.set_file_text(Path::new("/smart-original-authority/lib.py"), library);
        let source = "from lib import stop\ndef take(x:int):pass\ndef run(flag:bool):\n    if flag:\n        value=1\n    else:\n        value='text'\n        stop()\n    value #actual\n    take(v)\n";
        let id = db.set_file_text(Path::new("/smart-original-authority/m.py"), source);
        let file = db.file(id).unwrap();
        assert!(file.parse.errors().is_empty());
        let name = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::NameExpr::cast)
            .find(|n| usize::from(n.text_range().start()) == source.find("value #actual").unwrap())
            .unwrap();
        let ty = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Name(name));
        if terminal {
            assert!(
                ty.to_string() == "int" || ty.to_string() == "Literal[1]",
                "actual original dependency-backed current value: {ty}"
            );
        } else {
            assert!(
                matches!(ty, Type::Union(_)),
                "actual returning branch alternatives: {ty}"
            );
        }
        let offset = TextSize::new((source.find("take(v)").unwrap() + 6) as u32);
        let (expected, _) = expected_type(&db, &file, offset).unwrap();
        assert_eq!(expected.to_string(), "int");
        let items = smart_complete(&db, &file, offset);
        let item = items.iter().find(|i| i.label == "value").unwrap();
        assert_eq!(
            item.priority == 0,
            terminal,
            "original dependency authority must remain unchanged: {items:?}"
        );
    }
}
#[test]
fn previous_argument_assignment_is_seen_before_current_argument() {
    let source = "def take(first:str,second:int):pass\ndef run():\n    value:int=1\n    take((value:='changed'),value)\n";
    let db = Analysis::for_project(Path::new("/smart-original-authority"));
    let id = db.set_file_text(Path::new("/smart-original-authority/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let read = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|name| usize::from(name.text_range().start()) == source.find(",value)").unwrap() + 1)
        .unwrap();
    let current = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Name(read));
    assert!(
        current.to_string() == "str" || current.to_string() == "Literal[\"changed\"]",
        "actual previous-argument store BEFORE completion: {current}"
    );
    let offset = TextSize::new((source.find(",value)").unwrap() + 2) as u32);
    let (expected, _) = expected_type(&db, &file, offset).unwrap();
    assert_eq!(expected.to_string(), "int");
    let items = smart_complete(&db, &file, offset);
    assert!(
        items.iter().find(|i| i.label == "value").unwrap().priority >= 10,
        "actual earlier argument store cannot remain int-ranked: {items:?}"
    );
}
