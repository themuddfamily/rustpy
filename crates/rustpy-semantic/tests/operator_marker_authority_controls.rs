use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(source: &str, expected: Option<&str>) {
    let db = Analysis::for_project(Path::new("/operator-marker-authority"));
    let id = db.set_file_text(Path::new("/operator-marker-authority/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let infer = Inferer::new(&db);
    let result = |expression: &str| {
        let node = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::Expr::cast)
            .filter(|node| node.syntax().text() == expression)
            .last()
            .unwrap();
        infer.type_of_expr(&file, &node)
    };
    let Type::Instance(left, _) = result("left") else {
        panic!("actual left receiver premise");
    };
    let Type::Instance(right, _) = result("right") else {
        panic!("actual right receiver premise");
    };
    assert_eq!(left.name, "Left");
    assert_eq!(right.name, "Right");
    assert_eq!(result("left.__add__(right)").to_string(), "int");
    assert_eq!(result("right.__radd__(left)").to_string(), "str");
    let actual = result("left+right");
    match expected {
        Some(expected) => assert_eq!(actual.to_string(), expected),
        None => assert!(actual.is_unknown()),
    }
}
const TAIL: &str = " def __radd__(self,other:object)->str:...\nleft=Left()\nright=Right()\nleft.__add__(right)\nright.__radd__(left)\nleft+right\n";
#[test]
fn canonical_generic_marker_and_import_alias_keep_reflected_priority() {
    for marker in ["Generic", "Marker"] {
        let import = if marker == "Generic" {
            "from typing import Generic,TypeVar\n"
        } else {
            "from typing import Generic as Marker,TypeVar\n"
        };
        let source = format!("{import}T=TypeVar('T')\nclass Left:\n def __add__(self,other:object)->int:...\nclass Right(Left,{marker}[T]):\n{TAIL}");
        check(&source, Some("str"));
    }
}
#[test]
fn opaque_project_marker_spellings_do_not_certify_an_omitted_mro_edge() {
    for marker in ["Generic", "Protocol"] {
        let source = format!("def make_base():...\n{marker}=make_base()\nclass Left:\n def __add__(self,other:object)->int:...\nclass Right(Left,{marker}):\n{TAIL}");
        check(&source, None);
    }
}
