use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(writer: &str, expected: usize) {
    let source = format!("from typing import Literal\ndef f(subject:Literal[1,2]):\n{writer}    match subject:\n        case 1: result=1\n        case 2: result=2\n    return result\n");
    let db = Analysis::for_project(Path::new("/match-domain-retirement"));
    let id = db.set_file_text(Path::new("/match-domain-retirement/m.py"), &source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let annotation = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .find(|node| node.syntax().text() == "Literal[1,2]")
        .unwrap();
    let actual = Inferer::new(&db).annotation_type(&file, &annotation);
    let Type::Union(arms) = actual else {
        panic!("actual declared domain required");
    };
    assert_eq!(arms.len(), 2);
    assert!(arms.iter().all(|arm| matches!(arm, Type::Literal(_))));
    let problems = inspections::inspect(&db, &file, &InspectionOptions::default());
    assert_eq!(
        problems
            .iter()
            .filter(|problem| problem.diagnostic.code == "PyUnboundLocalVariable")
            .count(),
        expected
    );
}
#[test]
fn nested_nonlocal_writer_cannot_certify_original_literal_domain_exhaustion() {
    check(
        "    def replace():\n        nonlocal subject\n        subject=3\n    replace()\n",
        1,
    );
}
#[test]
fn unchanged_parameter_keeps_finite_literal_domain_exhaustion() {
    check("", 0);
}
