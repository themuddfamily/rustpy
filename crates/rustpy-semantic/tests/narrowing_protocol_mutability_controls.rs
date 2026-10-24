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

fn incompatible_member_premise(source: &str, needle: &str, target: &str) {
    let db = Analysis::for_project(Path::new("/narrowing-gaps"));
    let id = db.set_file_text(Path::new("/narrowing-gaps/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let inferer = Inferer::new(&db);
    let read = |needle: &str| {
        let offset = source.find(needle).unwrap();
        let name = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::NameExpr::cast)
            .find(|name| usize::from(name.text_range().start()) == offset)
            .unwrap();
        inferer.type_of_expr(&file, &ast::Expr::Name(name))
    };
    let actual = read(needle);
    let expected = read(target);
    assert!(
        !inferer.is_compatible(&actual, &expected),
        "actual maintained writable capability premise: {actual:?} vs {expected:?}"
    );
}
#[test]
fn writable_protocol_fields_are_not_covariant() {
    let source="from typing import Protocol\nfrom typing_extensions import TypeIs\nclass Writable(Protocol):\n name:object\nclass Valid:\n name:object=1\nclass Narrow:\n name:int=1\ndef guard(value:object)->TypeIs[Writable]:return hasattr(value,'name')\ndef use(value:Valid|Narrow,narrow:Narrow,target:Writable):\n narrow #premise\n target #target\n if guard(value):\n  value #yes\n else:\n  value #no\n";
    incompatible_member_premise(source, "narrow #premise", "target #target");
    check(source, "value #yes", &["Valid"]);
    check(source, "value #no", &["Narrow"]);
}
#[test]
fn read_only_property_cannot_prove_writable_protocol_membership() {
    let source="from typing import Protocol\nfrom typing_extensions import TypeIs\nclass Writable(Protocol):\n name:str\nclass Valid:\n name:str='text'\nclass ReadOnly:\n @property\n def name(self)->str:return 'text'\ndef guard(value:object)->TypeIs[Writable]:return hasattr(value,'name')\ndef use(value:Valid|ReadOnly,readonly:ReadOnly,target:Writable):\n readonly #premise\n target #target\n if guard(value):\n  value #yes\n else:\n  value #no\n";
    incompatible_member_premise(source, "readonly #premise", "target #target");
    check(source, "value #yes", &["Valid", "ReadOnly"]);
    check(source, "value #no", &["ReadOnly"]);
}
