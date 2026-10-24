use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

fn result(source: &str, expression: &str) -> String {
    let db = Analysis::for_project(Path::new("/operator-result-contracts"));
    let id = db.set_file_text(Path::new("/operator-result-contracts/m.py"), source);
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
    Inferer::new(&db).type_of_expr(&file, &node).to_string()
}

#[test]
fn strict_subclass_reflected_override_precedes_compatible_left_slot() {
    let source = r#"class Left:
    def __add__(self, other: object) -> int: return 1
class Right(Left):
    def __radd__(self, other: object) -> str: return 'right'
class Peer:
    def __radd__(self, other: object) -> bytes: return b'peer'
left = Left()
right = Right()
peer = Peer()
left.__add__(right)
right.__radd__(left)
left + left
left + peer
left + right
"#;
    // Actual public slot-result premises precede operator selection.
    assert_eq!(result(source, "left.__add__(right)"), "int");
    assert_eq!(result(source, "right.__radd__(left)"), "str");
    // An unrelated reflected slot cannot preempt a compatible left slot.
    assert_eq!(result(source, "left + left"), "int");
    assert_eq!(result(source, "left + peer"), "int");
    assert_eq!(result(source, "left + right"), "str");
}

#[test]
fn rich_comparison_preserves_actual_value_without_changing_boolean_conversion() {
    let source = r#"class Item:
    def __lt__(self, other: object) -> str: return 'comparison'
class Ordinary:
    def __lt__(self, other: object) -> bool: return True
first = Item()
second = Item()
ordinary = Ordinary()
first.__lt__(second)
ordinary.__lt__(ordinary)
first < second
ordinary < ordinary
not first
"#;
    assert_eq!(result(source, "first.__lt__(second)"), "str");
    assert_eq!(result(source, "ordinary.__lt__(ordinary)"), "bool");
    assert_eq!(result(source, "ordinary < ordinary"), "bool");
    assert_eq!(result(source, "not first"), "bool");
    assert_eq!(result(source, "first < second"), "str");
}
