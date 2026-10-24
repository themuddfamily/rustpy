use rustpy_semantic::{db::Analysis, flow};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
#[test]
fn missing_comprehension_fields_do_not_prove_terminal_or_definite_store() {
    for (expression, missing) in [
        ("[(assigned:=1) for item in ]", "iterator"),
        ("[(assigned:=1) for in ]", "target"),
        ("[(assigned:=1) for item in (0,) if ]", "condition"),
    ] {
        let source =
            format!("def f():\n marker=1\n values={expression}\n return marker,assigned\n");
        let db = Analysis::for_project(Path::new("/comprehension-recovery"));
        let id = db.set_file_text(Path::new("/comprehension-recovery/m.py"), &source);
        let file = db.file(id).unwrap();
        assert!(
            !file.parse.errors().is_empty(),
            "actual recovery syntax {missing}"
        );
        let root = file.parse.syntax_node();
        let clause = root
            .descendants()
            .find_map(ast::CompFor::cast)
            .expect("actual comprehension clause retained");
        match missing {
            "iterator" => assert!(clause.iter().is_none()),
            "target" => assert!(clause.target().is_none()),
            _ => {
                let condition = root
                    .descendants()
                    .find(|node| node.kind() == rustpy_syntax::SyntaxKind::COMP_IF)
                    .expect("actual incomplete filter retained");
                assert!(condition.children().find_map(ast::Expr::cast).is_none());
            }
        }
        let marker: Vec<_> = file
            .scopes
            .references
            .iter()
            .filter(|read| read.name == "marker" && !read.is_write)
            .collect();
        let assigned: Vec<_> = file
            .scopes
            .references
            .iter()
            .filter(|read| read.name == "assigned" && !read.is_write)
            .collect();
        assert_eq!(marker.len(), 1);
        assert_eq!(assigned.len(), 1);
        let info = flow::flow_of_with_db(
            &db,
            &file,
            flow::flow_scope(&file, marker[0].scope).unwrap(),
        );
        let marker_reaches = info
            .refs
            .get(&marker[0].range.start())
            .expect("recovery preserves normal tail");
        assert!(!marker_reaches.is_empty());
        assert!(marker_reaches.iter().all(|reach| reach.binding.is_some()));
        let reaches = info
            .refs
            .get(&assigned[0].range.start())
            .expect("recovery tail assignment read visited");
        assert!(
            reaches.is_empty() || reaches.iter().any(|reach| reach.binding.is_none()),
            "incomplete comprehension does not guarantee walrus: {missing}"
        );
    }
}
