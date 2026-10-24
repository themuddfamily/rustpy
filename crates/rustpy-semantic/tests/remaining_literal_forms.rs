use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(source: &str, expression: &str, expected: &str) {
    let db = Analysis::for_project(Path::new("/row222-literal-forms"));
    let id = db.set_file_text(Path::new("/row222-literal-forms/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let node = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .filter(|node| node.syntax().text() == expression)
        .last()
        .unwrap();
    assert_eq!(
        Inferer::new(&db).type_of_expr(&file, &node).to_string(),
        expected
    );
}
#[test]
fn context_manager_tuple_result_projects_each_as_target() {
    let prefix="class Manager:\n def __enter__(self)->tuple[int,str]:...\n def __exit__(self,*args)->None:...\n";
    check(
        &format!("{prefix}Manager().__enter__()\n"),
        "Manager().__enter__()",
        "tuple[int, str]",
    );
    check(
        &format!("{prefix}with Manager() as (count,label):\n label\n"),
        "label",
        "str",
    );
    check(
        &format!("{prefix}with Manager() as (count,label):\n count\n"),
        "count",
        "int",
    );
}
#[test]
fn typed_function_decorator_replacement_owns_the_call_result() {
    let prefix="from typing import Callable\ndef replace(fn:Callable[[int],int])->Callable[[int],str]:...\n";
    check(
        &format!("{prefix}def ordinary(value:int)->int:...\nreplace(ordinary)(1)\n"),
        "replace(ordinary)(1)",
        "str",
    );
    check(
        &format!("{prefix}@replace\ndef decorated(value:int)->int:...\ndecorated(1)\n"),
        "decorated(1)",
        "str",
    );
    check(
        "def ordinary(value:int)->int:...\nordinary(1)\n",
        "ordinary(1)",
        "int",
    );
}
