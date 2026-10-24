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

#[test]
fn canonical_classvar_protocol_requires_declaration_backed_class_capability() {
    let source="from typing import Protocol,ClassVar as CV\nfrom typing_extensions import TypeIs\nclass ClassNamed(Protocol):\n name:CV[str]\nclass Exact:\n name:CV[str]='text'\nclass InstanceOnly:\n def __init__(self):self.name:str='text'\ndef guard(value:object)->TypeIs[ClassNamed]:return hasattr(value,'name')\ndef use(value:Exact|InstanceOnly,instance:InstanceOnly):\n instance #premise\n if guard(value):\n  value #yes\n else:\n  value #no\n";
    let db = Analysis::for_project(Path::new("/narrowing-gaps"));
    let id = db.set_file_text(Path::new("/narrowing-gaps/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let offset = source.find("instance #premise").unwrap();
    let name = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|node| usize::from(node.text_range().start()) == offset)
        .unwrap();
    let inferer = Inferer::new(&db);
    let receiver = inferer.type_of_expr(&file, &ast::Expr::Name(name));
    assert_eq!(
        inferer
            .lookup_member(&receiver, "name")
            .unwrap()
            .to_string(),
        "str",
        "actual instance read type is insufficient class capability"
    );
    let class = receiver.class_ref().unwrap();
    assert_eq!(
        inferer
            .class_info(class)
            .unwrap()
            .members
            .get("name")
            .unwrap()
            .kind,
        rustpy_semantic::classes::MemberKind::InstanceVar
    );
    let typing = db.module("typing").unwrap();
    assert!(typing.is_stub && typing.path == Path::new("<typeshed>/typing.pyi"));
    assert!(typing
        .scopes
        .scope(typing.scopes.module_scope())
        .bindings
        .contains_key("ClassVar"));
    check(source, "value #yes", &["Exact", "InstanceOnly"]);
    check(source, "value #no", &["InstanceOnly"]);
}
