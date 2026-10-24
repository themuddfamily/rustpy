use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

fn capture(source: &str) -> Type {
    let db = Analysis::for_project(Path::new("/match-field-authority"));
    let id = db.set_file_text(Path::new("/match-field-authority/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let offset = source.find("captured #result").unwrap();
    let node = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .find(|node| usize::from(node.text_range().start()) == offset)
        .unwrap();
    Inferer::new(&db).type_of_expr(&file, &ast::Expr::Name(node))
}
#[test]
fn loop_context_and_comprehension_field_writes_retire_stale_member_projection() {
    let prefix = "class Point:\n    def __init__(self): self.x: int = 1\nclass Context:\n    def __enter__(self)->str:return 'changed'\n    def __exit__(self,*args):pass\n";
    let baseline = format!("{prefix}def use(point:Point,context:Context):\n    match point:\n        case Point(x=captured):\n            captured #result\n");
    assert_eq!(capture(&baseline).to_string(), "int");
    for writer in [
        "    for point.x in ['changed']: pass\n",
        "    with context as point.x: pass\n",
        "    [0 for point.x in ['changed']]\n",
    ] {
        let source = format!("{prefix}def use(point:Point,context:Context):\n{writer}    match point:\n        case Point(x=captured):\n            captured #result\n");
        assert!(matches!(capture(&source), Type::Unknown), "{writer}");
    }
}
#[test]
fn custom_attribute_setter_and_deleter_decline_plain_field_authority() {
    for method in [
        "    def __setattr__(self,name,value):pass\n",
        "    def __delattr__(self,name):pass\n",
    ] {
        let source = format!("class Point:\n    def __init__(self): self.x: int = 1\n{method}def use(point:Point):\n    match point:\n        case Point(x=captured):\n            captured #result\n");
        assert!(matches!(capture(&source), Type::Unknown), "{method}");
    }
}
