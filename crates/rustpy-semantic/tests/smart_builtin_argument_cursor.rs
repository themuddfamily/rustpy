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
fn empty_builtin_expected_parameter_ranks_current_sized_variable() {
    let source = "def run():\n    value='text'\n    value #current\n    len()\n    value=1\n";
    let db = Analysis::for_project(Path::new("/smart-builtin-cursor"));
    let id = db.set_file_text(Path::new("/smart-builtin-cursor/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let name = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|n| usize::from(n.text_range().start()) == source.find("value #current").unwrap())
        .unwrap();
    let current = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Name(name));
    assert!(current.to_string() == "str" || current.to_string() == "Literal[\"text\"]");
    let offset = TextSize::new((source.find("len()").unwrap() + 4) as u32);
    let (expected, _) = expected_type(&db, &file, offset).unwrap();
    let Type::Instance(class, _) = expected else {
        panic!("actual len expected parameter");
    };
    assert_eq!(class.name.as_str(), "Sized");
    let items = smart_complete(&db, &file, offset);
    assert_eq!(
        items.iter().find(|i| i.label == "value").unwrap().priority,
        0
    );
}
#[test]
fn builtin_argument_gap_observes_completed_operand_before_implicit_invocation() {
    for (rhs, expected_current, compatible) in [
        ("'text'", "Literal[\"text\"]", true),
        ("1", "Literal[1]", false),
    ] {
        let source = format!(
            "def run():\n    value=None\n    len((value:={rhs}))\n    value #actual_after_operand\n"
        );
        let db = Analysis::for_project(Path::new("/smart-builtin-cursor"));
        let id = db.set_file_text(Path::new("/smart-builtin-cursor/m.py"), &source);
        let file = db.file(id).unwrap();
        assert!(file.parse.errors().is_empty());
        let name = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::NameExpr::cast)
            .find(|n| {
                usize::from(n.text_range().start())
                    == source.find("value #actual_after_operand").unwrap()
            })
            .unwrap();
        let current = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Name(name));
        assert!(
            current.to_string() == expected_current
                || current.to_string() == if compatible { "str" } else { "int" },
            "actual known operand store before cursor query: {current}"
        );
        let call = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::CallExpr::cast)
            .find(|c| c.callee().is_some_and(|e| e.syntax().text() == "len"))
            .unwrap();
        let offset = TextSize::new(u32::from(call.text_range().end()) - 1);
        let (expected, _) = expected_type(&db, &file, offset).unwrap();
        let Type::Instance(class, _) = &expected else {
            panic!("actual len expected parameter: {expected}");
        };
        assert_eq!(class.name.as_str(), "Sized");
        let items = smart_complete(&db, &file, offset);
        assert_eq!(
            items.iter().find(|i| i.label == "value").unwrap().priority == 0,
            compatible,
            "completed operand phase must remain distinct from operation result: {items:?}"
        );
    }
}
