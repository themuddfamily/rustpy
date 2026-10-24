use rustpy_core::TextSize;
use rustpy_semantic::{
    completion2::{expected_type, smart_complete},
    db::Analysis,
    infer::Inferer,
};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(source: &str, read: &str, call: &str, expected: &str, allowed: &[&str], compatible: bool) {
    let db = Analysis::for_project(Path::new("/smart-current-details"));
    let id = db.set_file_text(Path::new("/smart-current-details/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let name = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|n| usize::from(n.text_range().start()) == source.find(read).unwrap())
        .unwrap();
    let current = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Name(name));
    assert!(
        allowed.contains(&current.to_string().as_str()),
        "actualcurrentvaluepremise: {current}"
    );
    let offset = TextSize::new((source.find(call).unwrap() + 6) as u32);
    let (wanted, _) = expected_type(&db, &file, offset).unwrap();
    assert_eq!(wanted.to_string(), expected);
    let items = smart_complete(&db, &file, offset);
    let item = items.iter().find(|i| i.label == "value").unwrap();
    assert_eq!(
        item.priority == 0,
        compatible,
        "rankingpremise BEFORE detail"
    );
    assert!(
        allowed.contains(&item.detail.as_str()),
        "current type must bevisible ratherthanfuturelexicalstore: {item:?}"
    );
    assert_eq!(item.insert, "value");
}
#[test]
fn displayed_variable_type_uses_current_scalar_not_future_assignment() {
    check(
        "def take(x:int):pass\ndef run():\n    value=1\n    value #actual\n    take(v)\n    value='future'\n",
        "value #actual",
        "take(v)",
        "int",
        &["int", "Literal[1]"],
        true,
    );
    check(
        "def take(x:int):pass\ndef run():\n    value='now'\n    value #actual\n    take(v)\n    value=1\n",
        "value #actual",
        "take(v)",
        "int",
        &["str", "Literal[\"now\"]"],
        false,
    );
}
#[test]
fn displayed_variable_type_retains_actual_predicate_refinement() {
    let source = "def take(x:int):pass\ndef run(value:int|str):\n    if isinstance(value,int):\n        value #integer\n        take(v) #yes\n    else:\n        value #text\n        take(v) #no\n";
    check(
        source,
        "value #integer",
        "take(v) #yes",
        "int",
        &["int"],
        true,
    );
    check(source, "value #text", "take(v) #no", "int", &["str"], false);
}
#[test]
fn displayed_generic_variable_keeps_original_class_and_argument_identity() {
    check(
        "class Box[T]:\n    def __init__(self,value:T):self.value=value\ndef take(x:Box[int]):pass\ndef run():\n    value=Box(1)\n    value #actual\n    take(v)\n    value=Box('future')\n",
        "value #actual",
        "take(v)",
        "Box[int]",
        &["Box[int]"],
        true,
    );
}
