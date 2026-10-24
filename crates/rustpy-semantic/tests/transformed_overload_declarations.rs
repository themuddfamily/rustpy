use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

#[test]
fn transformed_overload_declarations_preserve_argument_discriminators() {
    let source = r#"from typing import Callable, ParamSpec, TypeVar, overload
P = ParamSpec('P')
R = TypeVar('R')
def replace(fn: Callable[P, R]) -> Callable[P, str]:
    def wrapper(*args: P.args, **kwargs: P.kwargs) -> str:
        return 'replacement'
    return wrapper
def ordinary(value: int) -> int:
    return value
@overload
@replace
def changed(value: int) -> int: ...
@overload
@replace
def changed(value: str) -> str: ...
@replace
def changed(value: int | str) -> int | str:
    return value
@overload
def plain(value: int) -> int: ...
@overload
def plain(value: str) -> str: ...
def plain(value: int | str) -> int | str:
    return value
replace(ordinary)(1)
changed(1)
changed('x')
plain(1)
plain('x')
"#;
    let db = Analysis::for_project(Path::new("/transformed-overload-declarations"));
    let id = db.set_file_text(Path::new("/transformed-overload-declarations/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let infer = Inferer::new(&db);
    let result = |text: &str| {
        let node = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::Expr::cast)
            .filter(|node| node.syntax().text() == text)
            .last()
            .unwrap();
        infer.type_of_expr(&file, &node).to_string()
    };
    // Public producer premise first: replacement really preserves the int call
    // parameter while changing its result. Do not call a failure causal if this fails.
    assert_eq!(result("replace(ordinary)(1)"), "str");
    assert_eq!(result("plain(1)"), "int");
    assert_eq!(result("plain('x')"), "str");
    assert_eq!(result("changed(1)"), "str");
    assert_eq!(result("changed('x')"), "str");
}
