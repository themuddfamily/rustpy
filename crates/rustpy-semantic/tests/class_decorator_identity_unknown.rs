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
fn sole_returned_parameter_is_actual_identity_not_unknown_fallback() {
    check(
        "def identity(cls):return cls\n@identity\nclass Box:pass\nvalue=Box()\n",
        "Box",
    );
    check(
        "def identity[T](cls:T)->T:return cls\n@identity\nclass Box:pass\nvalue=Box()\n",
        "Box",
    );
}

#[test]
fn unknown_and_any_decorator_results_do_not_manufacture_class_identity() {
    check("from unavailable_plugin import transform\ndef opaque(cls):return transform(cls)\n@opaque\nclass Box:pass\nvalue=Box()\n", "Unknown");
    check("from typing import Any\ndef opaque(cls)->Any:return cls\n@opaque\nclass Box:pass\nvalue=Box()\n", "Any");
    check(
        "def apply(decorator):\n    @decorator\n    class Box:pass\n    return Box()\n",
        "Unknown",
    );
}

#[test]
fn known_identity_does_not_override_a_real_replacement_or_rebound_parameter() {
    check("from typing import Callable\ndef identity(cls):return cls\ndef replace(cls)->Callable[[],str]:raise RuntimeError\n@identity\n@replace\nclass Box:pass\nvalue=Box()\n", "str");
    check("def not_identity(cls):\n    cls=1\n    return cls\n@not_identity\nclass Box:pass\nvalue=Box()\n", "Unknown");
}
