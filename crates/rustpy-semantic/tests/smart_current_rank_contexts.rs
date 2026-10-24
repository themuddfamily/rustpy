use rustpy_core::TextSize;
use rustpy_semantic::{
    completion2::{expected_type, smart_complete},
    db::Analysis,
    infer::Inferer,
};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(
    source: &str,
    read: &str,
    call: &str,
    extra: usize,
    current: &str,
    expected: &str,
    compatible: bool,
) {
    let db = Analysis::for_project(Path::new("/smart-current-contexts"));
    let id = db.set_file_text(Path::new("/smart-current-contexts/m.py"), source);
    let file = db.file(id).unwrap();
    let point = source.find(read).unwrap();
    let name = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|n| usize::from(n.text_range().start()) == point)
        .unwrap();
    assert_eq!(
        Inferer::new(&db)
            .type_of_expr(&file, &ast::Expr::Name(name))
            .to_string(),
        current,
        "actual current read BEFORE completion"
    );
    let offset = TextSize::new((source.find(call).unwrap() + extra) as u32);
    let (wanted, _) = expected_type(&db, &file, offset).unwrap();
    assert_eq!(wanted.to_string(), expected);
    let items = smart_complete(&db, &file, offset);
    let item = items.iter().find(|i| i.label == "value").unwrap();
    assert_eq!(item.priority == 0, compatible, "{items:?}; {source}");
}
#[test]
fn declared_union_uses_actual_current_positive_and_negative_predicate() {
    let source = "def take(x:int):pass\ndef run(value:int|str):\n    if isinstance(value,int):\n        value #integer\n        take(v) #yes\n    else:\n        value #text\n        take(v) #no\n";
    check(
        source,
        "value #integer",
        "take(v) #yes",
        6,
        "int",
        "int",
        true,
    );
    check(source, "value #text", "take(v) #no", 6, "str", "int", false);
}
#[test]
fn current_generic_instance_preserves_original_class_identity() {
    let source = "class Box[T]:\n    def __init__(self,value:T):self.value=value\ndef take(x:Box[int]):pass\ndef run():\n    value=Box(1)\n    value #actual\n    take(v)\n    value=Box('future')\n";
    check(
        source,
        "value #actual",
        "take(v)",
        6,
        "Box[int]",
        "Box[int]",
        true,
    );
}
#[test]
fn partial_identifier_suffix_is_not_left_in_candidate_expression() {
    let source = "def take(x:int):pass\ndef run():\n    value:int=1\n    value #actual\n    take(val)\n    value='future'\n";
    check(source, "value #actual", "take(val)", 7, "int", "int", true);
}
#[test]
fn unfinished_assignment_and_return_share_actual_current_rank() {
    for context in ["    target:int= v\n", "    return v\n"] {
        let source = format!(
            "def run()->int:\n    value:int=1\n    value #actual\n{context}    value='future'\n"
        );
        let query = if context.contains("target") {
            "target:int= v"
        } else {
            "return v"
        };
        check(
            &source,
            "value #actual",
            query,
            query.len(),
            "int",
            "int",
            true,
        );
    }
}
