use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::{collections::BTreeSet, path::Path};

fn check(source: &str, needle: &str, expected: &[&str]) {
    let db = Analysis::for_project(Path::new("/narrowing-gaps"));
    let id = db.set_file_text(Path::new("/narrowing-gaps/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let offset = source.find(needle).expect("unique selected read");
    assert_eq!(source.matches(needle).count(), 1);
    let name = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|name| name.text_range().start() == rustpy_core::TextSize::new(offset as u32))
        .unwrap();
    let actual = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Name(name));
    let values = match actual {
        Type::Union(values) => values,
        value => vec![value],
    };
    let actual: BTreeSet<_> = values.into_iter().map(|value| value.to_string()).collect();
    let expected: BTreeSet<_> = expected.iter().map(|value| value.to_string()).collect();
    assert!(!actual.is_empty());
    assert_eq!(actual, expected, "{source}\n{needle}");
}

fn member_premise(source: &str, any: bool) {
    let db = Analysis::for_project(Path::new("/narrowing-gaps"));
    let id = db.set_file_text(Path::new("/narrowing-gaps/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let offset = source.find("opaque #premise").unwrap();
    let name = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|name| usize::from(name.text_range().start()) == offset)
        .unwrap();
    let inferer = Inferer::new(&db);
    let receiver = inferer.type_of_expr(&file, &ast::Expr::Name(name));
    let member = inferer
        .lookup_member(&receiver, "name")
        .expect("actual declared member");
    assert_eq!(
        member,
        if any { Type::Any } else { Type::Unknown },
        "actual member producer premise: {source}"
    );
}
#[test]
fn unknown_protocol_member_does_not_prove_positive_or_negative_membership() {
    let source="from typing import Protocol,Any\nfrom typing_extensions import TypeIs\nclass Named(Protocol):\n name:str\nclass Person:\n name:str='person'\nclass Opaque:\n name:Unavailable\ndef guard(value:object)->TypeIs[Named]:return hasattr(value,'name')\ndef use(value:Person|Opaque,opaque:Opaque):\n opaque #premise\n if guard(value):\n  value #yes\n else:\n  value #no\n";
    member_premise(source, false);
    check(source, "value #yes", &["Person", "Opaque"]);
    check(source, "value #no", &["Opaque"]);
}
#[test]
fn any_protocol_member_does_not_prove_positive_or_negative_membership() {
    let source="from typing import Protocol,Any\nfrom typing_extensions import TypeIs\nclass Named(Protocol):\n name:str\nclass Person:\n name:str='person'\nclass Opaque:\n name:Any\ndef guard(value:object)->TypeIs[Named]:return hasattr(value,'name')\ndef use(value:Person|Opaque,opaque:Opaque):\n opaque #premise\n if guard(value):\n  value #yes\n else:\n  value #no\n";
    member_premise(source, true);
    check(source, "value #yes", &["Person", "Opaque"]);
    check(source, "value #no", &["Opaque"]);
}
