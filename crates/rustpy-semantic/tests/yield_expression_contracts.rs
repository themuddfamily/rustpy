use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

fn check(source: &str, expression: &str, expected: &str) {
    let db = Analysis::for_project(Path::new("/yield-expression-contracts"));
    let id = db.set_file_text(Path::new("/yield-expression-contracts/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let expressions: Vec<_> = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .filter(|value| value.syntax().text() == expression)
        .collect();
    assert_eq!(
        expressions.len(),
        1,
        "unique queried expression: {expression}"
    );
    let actual = Inferer::new(&db).type_of_expr(&file, &expressions[0]);
    assert_eq!(actual.to_string(), expected, "{source}");
}

#[test]
fn yield_expression_uses_the_declared_send_type_not_the_yielded_type() {
    check("from typing import Generator\ndef echo()->Generator[int,str,None]:\n    received=yield 1\n", "yield 1", "str");
    check("from typing import Generator\ndef echo()->Generator[str,bytes,None]:\n    received=yield 'sent'\n", "yield 'sent'", "bytes");
}

#[test]
fn yield_from_expression_uses_the_delegated_return_type_not_element_type() {
    check("from typing import Generator\ndef child()->Generator[int,None,bytes]:\n    yield 1\n    return b'done'\ndef parent():\n    result=yield from child()\n", "yield from child()", "bytes");
    check("def child():\n    yield 1\n    return 'done'\ndef parent():\n    result=yield from child()\n", "yield from child()", "str");
    check(
        "def parent(source:object):\n    result=yield from source\n",
        "yield from source",
        "Unknown",
    );
}

#[test]
fn nested_generator_does_not_borrow_the_enclosing_send_annotation() {
    check("from typing import Generator\ndef outer()->Generator[int,str,None]:\n    def inner():\n        received=yield 2\n    yield 1\n", "yield 2", "Unknown");
}
