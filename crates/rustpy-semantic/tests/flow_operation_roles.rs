//! GREEN-only public API tests: exact runtime phase is part of point authority.
use rustpy_semantic::{db::Analysis, flow::OperationKind, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn points(source: &str, text: &str, phase: OperationKind, terminal: bool) {
    let db = Analysis::for_project(Path::new("/operation-role-audit"));
    let id = db.set_file_text(Path::new("/operation-role-audit/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let selected: Vec<_> = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .filter(|expression| expression.syntax().text() == text)
        .collect();
    assert_eq!(selected.len(), 1, "unique actual phase expression");
    let inf = Inferer::new(&db);
    assert_eq!(
        inf.operation_terminates(&file, &selected[0], phase),
        terminal,
        "{source}"
    );
    if phase != OperationKind::Value {
        assert!(
            !inf.operation_terminates(&file, &selected[0], OperationKind::Value),
            "nonvalue phase must not poison a value read"
        );
    }
}
#[test]
fn truth_name_is_not_an_ordinary_value_read() {
    let source="from typing import Never\nclass C:\n    def __bool__(self)->Never:raise RuntimeError\ndef f():\n    value=C()\n    if value:\n        pass\n";
    // Select the condition through its concrete AST owner, avoiding its store.
    let db = Analysis::for_project(Path::new("/truth-role-audit"));
    let id = db.set_file_text(Path::new("/truth-role-audit/m.py"), source);
    let file = db.file(id).unwrap();
    let condition = file
        .parse
        .syntax_node()
        .descendants()
        .find_map(ast::IfStmt::cast)
        .unwrap()
        .condition()
        .unwrap();
    let inf = Inferer::new(&db);
    assert!(inf.operation_terminates(&file, &condition, OperationKind::Truth));
    assert!(!inf.operation_terminates(&file, &condition, OperationKind::Value));
    assert!(
        !inf.call_terminates(&file, &condition),
        "legacy call proof must remain separate"
    );
    points("from typing import Never\nclass C:\n    def __bool__(self)->Never:raise RuntimeError\ndef f():\n    value=C()\n    not value\n","not value",OperationKind::Value,true);
}
#[test]
fn subscript_store_delete_and_read_keep_distinct_roles() {
    points("from typing import Never\nclass C:\n    def __getitem__(self,index:int)->Never:raise RuntimeError\n    def __setitem__(self,index:int,value:int)->None:pass\ndef f():\n    value=C()\n    value[0]=1\n","value[0]",OperationKind::Store,false);
    points("from typing import Never\nclass C:\n    def __getitem__(self,index:int)->int:return 1\n    def __setitem__(self,index:int,value:int)->Never:raise RuntimeError\ndef f():\n    value=C()\n    value[0]=1\n","value[0]",OperationKind::Store,true);
    points("from typing import Never\nclass C:\n    def __getitem__(self,index:int)->int:return 1\n    def __delitem__(self,index:int)->Never:raise RuntimeError\ndef f():\n    value=C()\n    del value[0]\n","value[0]",OperationKind::Delete,true);
    points("from typing import Never\nclass C:\n    def __getitem__(self,index:int)->Never:raise RuntimeError\ndef f():\n    value=C()\n    value[0]\n","value[0]",OperationKind::Value,true);
}
#[test]
fn inplace_and_iterator_acquisition_are_not_name_value_reads() {
    let source="from typing import Never\nclass C:\n    def __iadd__(self,other:int)->Never:raise RuntimeError\ndef f():\n    value=C()\n    value+=1\n";
    let db = Analysis::for_project(Path::new("/inplace-role-audit"));
    let id = db.set_file_text(Path::new("/inplace-role-audit/m.py"), source);
    let file = db.file(id).unwrap();
    let target = file
        .parse
        .syntax_node()
        .descendants()
        .find_map(ast::AugAssignStmt::cast)
        .unwrap()
        .target()
        .unwrap();
    let inf = Inferer::new(&db);
    assert!(inf.operation_terminates(&file, &target, OperationKind::Inplace));
    assert!(!inf.operation_terminates(&file, &target, OperationKind::Value));
    assert!(!inf.operation_terminates(&file, &target, OperationKind::Store));
    let source="from typing import Never\nclass C:\n    def __iter__(self)->Never:raise RuntimeError\ndef f():\n    items=C()\n    for value in items:\n        pass\n";
    let db = Analysis::for_project(Path::new("/iteration-role-audit"));
    let id = db.set_file_text(Path::new("/iteration-role-audit/m.py"), source);
    let file = db.file(id).unwrap();
    let iterable = file
        .parse
        .syntax_node()
        .descendants()
        .find_map(ast::ForStmt::cast)
        .unwrap()
        .iter()
        .unwrap();
    let inf = Inferer::new(&db);
    assert!(inf.operation_terminates(&file, &iterable, OperationKind::IteratorAcquisition));
    assert!(!inf.operation_terminates(&file, &iterable, OperationKind::Value));
    assert!(!inf.operation_terminates(&file, &iterable, OperationKind::Truth));
}
#[test]
fn changed_source_and_loop_replay_do_not_retain_stale_value_authority() {
    let db = Analysis::for_project(Path::new("/role-source-audit"));
    let path = Path::new("/role-source-audit/m.py");
    let source="from typing import Never\nclass C:\n    def __neg__(self)->Never:raise RuntimeError\ndef f():\n    value=C()\n    -value\n";
    let id = db.set_file_text(path, source);
    let inf = Inferer::new(&db);
    let file = db.file(id).unwrap();
    let expr = file
        .parse
        .syntax_node()
        .descendants()
        .find_map(ast::UnaryExpr::cast)
        .unwrap();
    assert!(inf.operation_terminates(&file, &ast::Expr::Unary(expr), OperationKind::Value));
    db.set_file_text(
        path,
        &source.replace("->Never:raise RuntimeError", "->int:return 1"),
    );
    let current = db.file(id).unwrap();
    let expr = current
        .parse
        .syntax_node()
        .descendants()
        .find_map(ast::UnaryExpr::cast)
        .unwrap();
    assert!(!inf.operation_terminates(&current, &ast::Expr::Unary(expr), OperationKind::Value));
    points("from typing import Never\nclass Normal:\n    def __neg__(self)->int:return 1\nclass Stop:\n    def __neg__(self)->Never:raise RuntimeError\ndef f(flag:bool):\n    value=Normal()\n    while flag:\n        -value\n        value=Stop()\n","-value",OperationKind::Value,false);
}
