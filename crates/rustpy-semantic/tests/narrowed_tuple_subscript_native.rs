use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
const SOURCE: &str = "from typing import TypeGuard, Literal\nfrom typing_extensions import TypeIs\ndef need_text(value_π: str): pass\ndef need_int(value_π: int): pass\ndef is_text(value_π: object) -> TypeGuard[str]: return isinstance(value_π, str)\ndef exact_text(value_π: object) -> TypeIs[str]: return isinstance(value_π, str)\ndef guard_case(value_π: int | str):\n    if is_text(value_π):\n        need_text(value_π)\n        need_int(value_π)\n    else: need_text(value_π)\ndef exact_case(value_π: int | str):\n    if exact_text(value_π): need_text(value_π)\n    else:\n        need_int(value_π)\n        need_text(value_π)\nclass Cat:\n    kind: Literal['cat'] = 'cat'\nclass Dog:\n    kind: Literal['dog'] = 'dog'\ndef need_cat(value_π: Cat): pass\ndef owner_case(value_π: Cat | Dog):\n    if value_π.kind == 'cat': need_cat(value_π)\n    else: need_cat(value_π)\ndef length_case(value_π: tuple[int] | tuple[str, str]):\n    if len(value_π) == 1: need_int(value_π[0])\n    else:\n        need_text(value_π[0])\n        need_int(value_π[0])\n";

#[test]
fn exact_native_tuple_owner_is_narrowed_before_subscript_consumer() {
    let db = Analysis::for_project(Path::new("/native-narrow-consumer"));
    let id = db.set_file_text(Path::new("/native-narrow-consumer/m.py"), SOURCE);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let inferer = Inferer::new(&db);
    for (needle, expected) in [
        ("need_int(value_π[0])\n    else:", "tuple[int]"),
        ("need_text(value_π[0])", "tuple[str, str]"),
    ] {
        let offset = SOURCE.find(needle).unwrap() + needle.find("value_π").unwrap();
        let node = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::NameExpr::cast)
            .find(|node| usize::from(node.text_range().start()) == offset)
            .unwrap();
        assert_eq!(
            inferer
                .type_of_expr(&file, &ast::Expr::Name(node))
                .to_string(),
            expected,
            "owner premise: {needle}"
        );
    }
}
#[test]
fn exact_native_narrowed_tuple_subscript_produces_scalar_type() {
    let db = Analysis::for_project(Path::new("/native-narrow-consumer"));
    let id = db.set_file_text(Path::new("/native-narrow-consumer/m.py"), SOURCE);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let inferer = Inferer::new(&db);
    for (needle, expected) in [
        ("need_int(value_π[0])\n    else:", "int"),
        ("need_text(value_π[0])", "str"),
        ("        need_int(value_π[0])\n", "str"),
    ] {
        let offset = SOURCE.find(needle).unwrap() + needle.find("value_π").unwrap();
        let node = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::SubscriptExpr::cast)
            .find(|node| usize::from(node.text_range().start()) == offset)
            .unwrap();
        assert_eq!(
            inferer
                .type_of_expr(&file, &ast::Expr::Subscript(node))
                .to_string(),
            expected,
            "subscript provider: {needle}"
        );
    }
}
