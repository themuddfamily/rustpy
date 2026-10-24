use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::{collections::BTreeSet, path::Path};
fn check(source: &str, needle: &str, expected: &[&str]) {
    let db = Analysis::for_project(Path::new("/match-narrowing"));
    let id = db.set_file_text(Path::new("/match-narrowing/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    assert_eq!(source.matches(needle).count(), 1);
    let offset = source.find(needle).unwrap();
    let name = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|name| usize::from(name.text_range().start()) == offset)
        .unwrap();
    let actual = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Name(name));
    let values = match actual {
        Type::Union(values) => values,
        value => vec![value],
    };
    let actual: BTreeSet<_> = values.into_iter().map(|v| v.to_string()).collect();
    let expected: BTreeSet<_> = expected.iter().map(|s| s.to_string()).collect();
    assert_eq!(actual, expected, "{source}\n{needle}");
}
#[test]
fn class_as_pattern_narrows_actual_subject_and_capture() {
    let source = "def use(value:int|str):\n    value #declared\n    match value:\n        case int() as captured:\n            captured #capture\n            value #integer\n        case _:\n            value #remaining\n";
    check(source, "value #declared", &["int", "str"]);
    check(source, "captured #capture", &["int"]);
    check(source, "value #integer", &["int"]);
    check(source, "value #remaining", &["str"]);
}
