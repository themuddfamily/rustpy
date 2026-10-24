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
fn canonical_dataclass_alias_and_factory_keep_synthesized_class() {
    check("from dataclasses import dataclass as dc\n@dc\nclass Box:\n    value:float=1.0\nvalue=Box()\n", "Box");
    check("import dataclasses as dc\n@dc.dataclass(kw_only=True)\nclass Box:\n    value:float=1.0\nvalue=Box()\n", "Box");
}
#[test]
fn user_dataclass_spelling_keeps_actual_replacement_contract() {
    check("from typing import Callable\ndef dataclass(cls)->Callable[[],str]:raise RuntimeError\n@dataclass\nclass Box:pass\nvalue=Box()\n", "str");
}

#[test]
fn canonical_dataclass_does_not_reuse_original_synthesis_after_class_replacement() {
    check("from dataclasses import dataclass\nclass Other:pass\ndef replace(cls)->type[Other]:return Other\n@dataclass\n@replace\nclass Box:\n    value:float=1.0\nvalue=Box()\n", "Unknown");
}
