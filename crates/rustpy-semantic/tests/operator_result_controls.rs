use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn result(source: &str, expression: &str) -> String {
    let db = Analysis::for_project(Path::new("/operator-result-controls"));
    let id = db.set_file_text(Path::new("/operator-result-controls/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let node = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .filter(|node| node.syntax().text() == expression)
        .last()
        .unwrap();
    let infer = Inferer::new(&db);
    infer.type_of_expr(&file, &node).to_string()
}
#[test]
fn inherited_reflected_slot_cannot_preempt_a_compatible_left_slot() {
    let source = "class Left:\n def __add__(self,other:object)->int:...\n def __radd__(self,other:object)->str:...\nclass Right(Left):...\nleft=Left()\nright=Right()\nleft+right\n";
    assert_eq!(result(source, "left+right"), "int");
}
#[test]
fn not_implemented_allows_reflected_fallback_without_becoming_a_value_result() {
    let source = "from types import NotImplementedType\nclass Left:\n def __add__(self,other:object)->NotImplementedType: return NotImplemented\nclass Right:\n def __radd__(self,other:object)->str:...\nleft=Left()\nright=Right()\nleft+right\n";
    assert_eq!(result(source, "left+right"), "str");
    let source = "from types import NotImplementedType\nclass Left:\n def __add__(self,other:object)->int:...\nclass Right(Left):\n def __radd__(self,other:object)->NotImplementedType:return NotImplemented\nleft=Left()\nright=Right()\nleft+right\n";
    assert_eq!(result(source, "left+right"), "int");
}
#[test]
fn comparison_chains_return_comparison_objects_while_identity_and_membership_are_bool() {
    let source = "class Item:\n def __lt__(self,other:object)->str:...\nclass Other:\n def __lt__(self,other:object)->bytes:...\nfirst=Item()\nsecond=Other()\nthird=Other()\nfirst<second<third\nfirst is second\nfirst in [second]\nnot first\n";
    let actual = result(source, "first<second<third");
    assert!(
        actual == "str | bytes" || actual == "bytes | str",
        "{actual}"
    );
    for expression in ["first is second", "first in [second]", "not first"] {
        assert_eq!(result(source, expression), "bool", "{expression}");
    }
}
#[test]
fn rich_comparison_subclass_reflection_and_operand_unions_keep_real_result_types() {
    let source = "class Left:\n def __lt__(self,other:object)->int:...\nclass Right(Left):\n def __gt__(self,other:object)->str:...\nleft=Left()\nright=Right()\nleft<right\n";
    assert_eq!(result(source, "left<right"), "str");
    let source = "class Text:\n def __lt__(self,other:object)->str:...\nclass Bytes:\n def __lt__(self,other:object)->bytes:...\ndef f(value:Text|Bytes):\n return value<0\n";
    let actual = result(source, "value<0");
    assert!(
        actual == "str | bytes" || actual == "bytes | str",
        "{actual}"
    );
}
#[test]
fn unknown_reflected_override_never_reuses_a_known_left_return_as_authority() {
    let source = "def replace(fn):...\nclass Left:\n def __add__(self,other:object)->int:...\nclass Right(Left):\n @replace\n def __radd__(self,other:object)->str:...\nleft=Left()\nright=Right()\nleft+right\n";
    assert_eq!(result(source, "left+right"), "Unknown");
}
