use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn query(source: &str, expression: &str, marker: &str) -> Type {
    let db = Analysis::for_project(Path::new("/factory-types"));
    let id = db.set_file_text(Path::new("/factory-types/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let needle = format!("{expression} #{marker}");
    assert_eq!(source.matches(&needle).count(), 1);
    let offset = source.find(&needle).unwrap();
    let node = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .find(|node| {
            usize::from(node.text_range().start()) == offset
                && usize::from(node.text_range().len()) == expression.len()
        })
        .unwrap();
    Inferer::new(&db).type_of_expr(&file, &node)
}
#[test]
fn newtype_is_nominal_but_retains_its_base_subtyping() {
    let source = "from typing import NewType\nUser=NewType('User',int)\nOther=NewType('Other',int)\nUser(1) #user\nOther(1) #other\nint(1) #base\n";
    let db = Analysis::for_project(Path::new("/factory-types"));
    db.set_file_text(Path::new("/factory-types/m.py"), source);
    let file = db.file_for_path(Path::new("/factory-types/m.py")).unwrap();
    assert!(file.parse.errors().is_empty());
    let infer = Inferer::new(&db);
    // ClassRef identity is owned by this Analysis. Never compare types obtained
    // from unrelated query() worlds against a different Inferer's database.
    let same_world = |expression: &str, marker: &str| {
        let needle = format!("{expression} #{marker}");
        assert_eq!(source.matches(&needle).count(), 1);
        let offset = source.find(&needle).unwrap();
        let node = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::Expr::cast)
            .find(|node| {
                usize::from(node.text_range().start()) == offset
                    && usize::from(node.text_range().len()) == expression.len()
            })
            .unwrap();
        infer.type_of_expr(&file, &node)
    };
    let user = same_world("User(1)", "user");
    let other = same_world("Other(1)", "other");
    let base = same_world("int(1)", "base");
    assert_eq!(user.class_ref().unwrap().file, file.id);
    assert_eq!(other.class_ref().unwrap().file, file.id);
    assert_eq!(base.class_ref(), infer.builtin_class("int").as_ref());
    assert!(matches!(&base, Type::Instance(class, _) if class.name == "int"));
    assert_eq!(user.to_string(), "User");
    assert!(infer.is_compatible(&user, &base));
    assert!(!infer.is_compatible(
        &user,
        &Type::Literal(rustpy_semantic::types::LiteralValue::Int(1))
    ));
    assert!(!infer.is_compatible(&base, &user));
    assert!(!infer.is_compatible(&user, &other));
}
#[test]
fn functional_namedtuple_preserves_fields_indexing_unpacking_and_tuple_identity() {
    let source = "from typing import NamedTuple\nPoint=NamedTuple('Point',[('x',int),('label',str)])\np=Point(1,'a')\np.x #field\np.label #label\np[0] #index\na,b=p\na #unpacked\nb #unpacked_label\n";
    for (expression, marker, expected) in [
        ("p.x", "field", "int"),
        ("p.label", "label", "str"),
        ("p[0]", "index", "int"),
        ("a", "unpacked", "int"),
        ("b", "unpacked_label", "str"),
    ] {
        assert_eq!(query(source, expression, marker).to_string(), expected);
    }
}
#[test]
fn collections_namedtuple_defaults_rename_and_metadata_are_preserved() {
    let source = "from collections import namedtuple\nRow=namedtuple('Row',['left','class'],rename=True,defaults=[7])\nr=Row(1)\nr #instance\nRow._fields #fields\n";
    assert_eq!(query(source, "r", "instance").to_string(), "Row");
    assert!(matches!(query(source,"Row._fields","fields"),Type::Tuple(values) if values.len()==2));
}
#[test]
fn shadowed_factories_remain_ordinary_and_dynamic_fields_are_unknown() {
    let source = "def NewType(name,base)->str:return name\nNewType('Alias',int) #ordinary\n";
    assert_eq!(
        query(source, "NewType('Alias',int)", "ordinary").to_string(),
        "str"
    );
    let dynamic = "from typing import NamedTuple\ndef make(fields):\n    return NamedTuple('Dynamic',fields) #dynamic\n";
    assert!(matches!(
        query(dynamic, "NamedTuple('Dynamic',fields)", "dynamic"),
        Type::Unknown
    ));
}
