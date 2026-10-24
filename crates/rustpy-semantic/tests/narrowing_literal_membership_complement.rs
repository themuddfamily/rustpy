use rustpy_semantic::{
    db::Analysis,
    infer::Inferer,
    types::{LiteralValue, Type},
};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(condition: &str, yes: &[i64], no: &[i64]) {
    let source = format!("from typing import Literal\ndef use(value:Literal[1,2,3]):\n    value #declared\n    if {condition}:\n        value #yes\n    else:\n        value #no\n");
    let db = Analysis::for_project(Path::new("/membership-complement"));
    let id = db.set_file_text(Path::new("/membership-complement/m.py"), &source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let inf = Inferer::new(&db);
    let read = |marker: &str| {
        let point = source.find(marker).unwrap();
        let node = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::NameExpr::cast)
            .find(|name| usize::from(name.text_range().start()) == point)
            .unwrap();
        inf.type_of_expr(&file, &ast::Expr::Name(node))
    };
    let declared = read("value #declared");
    let Type::Union(arms) = &declared else {
        panic!("actual declared Literal union: {declared}");
    };
    assert_eq!(arms.len(), 3);
    assert!(arms.iter().all(|arm| matches!(arm, Type::Literal(_))));
    // First preserve the existing positive membership projection, then prove
    // the complementary branch rather than treating it as unchanged union.
    let values = |ty: Type| {
        let arms = match ty {
            Type::Union(arms) => arms,
            other => vec![other],
        };
        let mut integers: Vec<_> = arms
            .into_iter()
            .map(|arm| {
                let Type::Literal(LiteralValue::Int(value)) = arm else {
                    panic!("actual literal result: {arm}");
                };
                value
            })
            .collect();
        integers.sort();
        integers
    };
    assert_eq!(values(read("value #yes")), yes);
    assert_eq!(values(read("value #no")), no);
}
#[test]
fn literal_membership_false_branch_removes_known_members() {
    check("value in (1,2)", &[1, 2], &[3]);
}
#[test]
fn literal_not_membership_true_branch_removes_known_members() {
    // Ordering intentionally checks the complemented branch first here.
    check("value not in [1,2]", &[3], &[1, 2]);
}
