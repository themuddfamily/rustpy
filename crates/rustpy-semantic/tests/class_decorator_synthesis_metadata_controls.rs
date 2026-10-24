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
fn canonical_typing_metadata_keeps_class_but_fake_spelling_does_not() {
    check("from typing_extensions import disjoint_base\n@disjoint_base\nclass Box:pass\nvalue=Box()\n", "Box");
    check(
        "from typing import final as sealed\n@sealed\nclass Box:pass\nvalue=Box()\n",
        "Box",
    );
    check("from typing import Callable\ndef final(cls)->Callable[[],str]:raise RuntimeError\n@final\nclass Box:pass\nvalue=Box()\n", "str");
}

#[test]
fn maintained_unresolved_attrs_models_keep_synthesized_identity() {
    check(
        "import attr\n@attr.s\nclass Box:\n    value=attr.ib(default=1)\nvalue=Box()\n",
        "Box",
    );
    check(
        "from attrs import define as model\n@model\nclass Box:\n    value:int=1\nvalue=Box()\n",
        "Box",
    );
}

#[test]
fn canonical_transform_marker_allows_actual_identity_body_only() {
    check("from typing import dataclass_transform\n@dataclass_transform(kw_only_default=True)\ndef model(cls):return cls\n@model\nclass Box:\n    value:int=1\nvalue=Box()\n", "Box");
    check("from typing import Callable\ndef dataclass_transform(fn)->Callable[[],int]:raise RuntimeError\n@dataclass_transform\ndef model(cls):return cls\n@model\nclass Box:pass\nvalue=Box()\n", "Unknown");
}
