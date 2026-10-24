use rustpy_semantic::{
    db::Analysis,
    flow,
    infer::Inferer,
    types::{LiteralValue, Type},
};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(source: &str, attribute: bool, terminal_store: bool) {
    let db = Analysis::for_project(Path::new("/comprehension-target-modes"));
    let id = db.set_file_text(Path::new("/comprehension-target-modes/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(
        file.parse.errors().is_empty(),
        "valid Python target: {source}"
    );
    let constructor = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .find(|e| e.syntax().text() == "C()")
        .unwrap();
    let inf = Inferer::new(&db);
    let receiver = inf.type_of_expr(&file, &constructor);
    assert!(!receiver.is_unknown());
    let (read, store, args) = if attribute {
        (
            inf.lookup_member(&receiver, "slot").unwrap(),
            inf.lookup_member(&receiver, "__setattr__").unwrap(),
            vec![
                Type::Literal(LiteralValue::Str("slot".into())),
                Type::Literal(LiteralValue::Int(1)),
            ],
        )
    } else {
        let getter = inf.lookup_member(&receiver, "__getitem__").unwrap();
        (
            inf.call_result(&getter, &[Type::Literal(LiteralValue::Int(0))], &[]),
            inf.lookup_member(&receiver, "__setitem__").unwrap(),
            vec![
                Type::Literal(LiteralValue::Int(0)),
                Type::Literal(LiteralValue::Int(1)),
            ],
        )
    };
    assert!(
        matches!(read, Type::Never),
        "actual getter/read Never BEFORE valid store target Flow: {read}"
    );
    let stored = inf.call_result(&store, &args, &[]);
    assert_eq!(
        matches!(stored, Type::Never),
        terminal_store,
        "actual selected store invocation: {stored}"
    );
    assert!(!stored.is_unknown());
    let tail = file
        .scopes
        .references
        .iter()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(&db, &file, flow::flow_scope(&file, tail.scope).unwrap());
    assert_eq!(
        !info.refs.contains_key(&tail.range.start()),
        terminal_store,
        "comprehension target is STORE, not READ: {source}"
    );
}
#[test]
fn property_getter_never_is_not_invoked_by_comprehension_attribute_store() {
    for (ret, body, terminal) in [
        ("None", "pass", false),
        ("Never", "raise RuntimeError", true),
    ] {
        let source = format!(
            "from typing import Never\nclass C:\n    @property\n    def slot(self)->Never:raise RuntimeError\n    def __setattr__(self,name:str,value:int)->{ret}:{body}\ndef f():\n    marker=1\n    obj=C()\n    [0 for obj.slot in (1,)]\n    return marker\n"
        );
        check(&source, true, terminal);
    }
}
#[test]
fn getitem_never_is_not_invoked_by_comprehension_subscript_store() {
    for (ret, body, terminal) in [
        ("None", "pass", false),
        ("Never", "raise RuntimeError", true),
    ] {
        let source = format!(
            "from typing import Never\nclass C:\n    def __getitem__(self,index:int)->Never:raise RuntimeError\n    def __setitem__(self,index:int,value:int)->{ret}:{body}\ndef f():\n    marker=1\n    obj=C()\n    [0 for obj[0] in (1,)]\n    return marker\n"
        );
        check(&source, false, terminal);
    }
}
