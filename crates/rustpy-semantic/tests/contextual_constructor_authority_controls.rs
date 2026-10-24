use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn result(source: &str) -> String {
    let db = Analysis::for_project(Path::new("/constructor-authority"));
    let id = db.set_file_text(Path::new("/constructor-authority/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let call = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .find(|call| call.syntax().text() == "Box()")
        .unwrap();
    Inferer::new(&db)
        .type_of_expr(&file, &ast::Expr::Call(call))
        .to_string()
}
#[test]
fn unknown_base_does_not_authorize_contextual_class_arguments() {
    assert_ne!(
        result("from absent_library import Base\nclass Box[T](Base):pass\nvalue:Box[str]=Box()\n"),
        "Box[str]",
        "unknown ancestry may own native constructor dispatch"
    );
}
#[test]
fn shadowed_generic_marker_cannot_hide_unknown_ancestry() {
    assert_ne!(
        result("def Generic():return object\nclass Box[T](Generic):pass\nvalue:Box[str]=Box()\n"),
        "Box[str]",
        "same-spelled function is not canonical typing Generic"
    );
}
#[test]
fn inherited_constructor_dispatch_preserves_actual_foreign_results() {
    assert_eq!(result("class Foreign:pass\nclass Base:\n    def __new__(cls)->Foreign:return Foreign()\nclass Box[T](Base):pass\nvalue:Box[str]=Box()\n"),"Foreign");
    assert_eq!(result("class Meta(type):\n    def __call__(cls)->int:return 1\nclass Base(metaclass=Meta):pass\nclass Box[T](Base):pass\nvalue:Box[str]=Box()\n"),"int");
}

#[test]
fn user_base_subscription_factory_cannot_authorize_contextual_arguments() {
    assert_ne!(result("class Foreign:pass\nclass Base:\n    def __class_getitem__(cls,item):return Foreign\nclass Box[T](Base[int]):pass\nvalue:Box[str]=Box()\n"),"Box[str]","runtime base factory is not ordinary generic ancestry");
}
