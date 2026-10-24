use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn inspect(source: &str, code: &str) -> usize {
    let db = Analysis::for_project(Path::new("/match-exhaustion"));
    let id = db.set_file_text(Path::new("/match-exhaustion/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    inspections::inspect(&db, &file, &InspectionOptions::default())
        .iter()
        .filter(|p| p.diagnostic.code == code)
        .count()
}
fn literal_domain(source: &str) {
    let db = Analysis::for_project(Path::new("/match-domain"));
    let id = db.set_file_text(Path::new("/match-domain/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let off = source.find("subject #domain").unwrap();
    let node = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|node| usize::from(node.text_range().start()) == off)
        .unwrap();
    let actual = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Name(node));
    let Type::Union(arms) = actual else {
        panic!("actual finite Literal union required: {actual:?}")
    };
    assert_eq!(arms.len(), 2);
    assert!(arms.iter().all(|a| matches!(a, Type::Literal(_))));
}
#[test]
fn finite_literal_cases_exhaust_domain_but_missing_or_guarded_case_keeps_unbound() {
    let prefix = "from typing import Literal\ndef use(subject:Literal[1,2], flag:bool):\n    subject #domain\n    match subject:\n        case 1: result=1\n";
    let missing = format!("{prefix}    return result\n");
    let guarded = format!("{prefix}        case 2 if flag: result=2\n    return result\n");
    let covered = format!("{prefix}        case 2: result=2\n    return result\n");
    literal_domain(&covered);
    assert_eq!(inspect(&missing, "PyUnboundLocalVariable"), 1);
    assert_eq!(inspect(&guarded, "PyUnboundLocalVariable"), 1);
    assert_eq!(inspect(&covered, "PyUnboundLocalVariable"), 0);
}
#[test]
fn reachable_concrete_arguments_to_canonical_and_user_never_parameters_are_rejected() {
    let source = "from typing import Never, assert_never\ndef own(value:Never)->Never: raise AssertionError\ndef canonical(value:int):\n    assert_never(value)\ndef custom(value:str):\n    own(value)\n";
    assert_eq!(inspect(source, "PyTypeChecker"), 2);
    let returning = "def own(value:int)->int:return value\nown(1)\n";
    assert_eq!(inspect(returning, "PyTypeChecker"), 0);
}
