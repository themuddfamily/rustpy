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
fn decorator_alias_and_shadow_use_actual_callable_binding() {
    check("from typing import Callable\ndef original(cls)->Callable[[],str]:raise RuntimeError\nalias=original\n@alias\nclass Box:pass\nvalue=Box()\n", "str");
    check("from typing import Callable\ndef original(cls)->Callable[[],str]:raise RuntimeError\ndef other(cls)->Callable[[],int]:raise RuntimeError\nalias=other\n@alias\nclass Box:pass\nvalue=Box()\n", "int");
}

#[test]
fn decorator_factory_and_reverse_application_preserve_actual_results() {
    check("from typing import Callable\ndef replace(cls)->Callable[[],str]:raise RuntimeError\ndef choose()->Callable[[type],Callable[[],str]]:return replace\n@choose()\nclass Box:pass\nvalue=Box()\n", "str");
    check("from typing import Callable\ndef lower(cls)->Callable[[],str]:raise RuntimeError\ndef upper(fn:Callable[[],str])->Callable[[],int]:raise RuntimeError\n@upper\n@lower\nclass Box:pass\nvalue=Box()\n", "int");
}
