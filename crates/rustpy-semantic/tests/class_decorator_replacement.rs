use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

fn check(source: &str, expected: &str) {
    let db = Analysis::for_project(Path::new("/class-decorator-replacement"));
    let id = db.set_file_text(Path::new("/class-decorator-replacement/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let calls: Vec<_> = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .filter(|call| call.syntax().text() == "Box()")
        .collect();
    assert_eq!(calls.len(), 1);
    let actual = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Call(calls[0].clone()));
    assert_eq!(actual.to_string(), expected, "{source}");
}

#[test]
fn class_decorator_returned_callable_owns_actual_call_result() {
    check("from typing import Callable\ndef replacement()->str:return 'replacement'\ndef replace(cls)->Callable[[],str]:return replacement\n@replace\nclass Box:pass\nvalue=Box()\n", "str");
    // A generic class declaration is replaced by the same callable too.
    check("from typing import Callable\ndef replacement()->str:return 'replacement'\ndef replace(cls)->Callable[[],str]:return replacement\n@replace\nclass Box[T]:pass\nvalue=Box()\n", "str");
}

#[test]
fn undecorated_class_preserves_ordinary_constructor_result() {
    check("class Box:pass\nvalue=Box()\n", "Box");
}
