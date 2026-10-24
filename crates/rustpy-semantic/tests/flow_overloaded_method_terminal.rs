//! Real bound overload producers before flow authority; Python source is not run.
use rustpy_semantic::{db::Analysis, flow, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(source: &str, all_never: bool, call_never: Option<bool>, terminal: bool) {
    let db = Analysis::for_project(Path::new("/overloaded-method-terminal"));
    let id = db.set_file_text(Path::new("/overloaded-method-terminal/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(
        file.parse.errors().is_empty(),
        "{source}\n{:?}",
        file.parse.errors()
    );
    let inf = Inferer::new(&db);
    let call = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .find(|call| call.syntax().text() == "c.stop(value)")
        .unwrap();
    let Type::Overloaded(signatures) = inf.type_of_expr(&file, &call.callee().unwrap()) else {
        panic!("actual bound overloaded member required: {source}")
    };
    assert_eq!(signatures.len(), 2, "{source}");
    assert!(
        signatures
            .iter()
            .all(|signature| signature.params.len() == 1),
        "actual receiver binding must remove self: {source}\n{signatures:?}"
    );
    assert_eq!(
        signatures
            .iter()
            .all(|signature| matches!(signature.ret, Type::Never)),
        all_never,
        "{source}"
    );
    if let Some(expected) = call_never {
        let actual = inf.type_of_expr(&file, &ast::Expr::Call(call));
        assert_eq!(
            matches!(actual, Type::Never),
            expected,
            "actual call producer {actual}: {source}"
        );
    }
    let reference = file
        .scopes
        .references
        .iter()
        .rev()
        .find(|reference| reference.name == "marker" && !reference.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(
        &db,
        &file,
        flow::flow_scope(&file, reference.scope).unwrap(),
    );
    assert_eq!(
        !info.refs.contains_key(&reference.range.start()),
        terminal,
        "actual bound-method tail authority: {source}"
    );
}
#[test]
fn all_never_instance_method_overloads_terminate_actual_tail() {
    check("from typing import overload,Never,NoReturn\nclass C:\n    @overload\n    def stop(self,value:int)->Never: ...\n    @overload\n    def stop(self,value:str)->NoReturn: ...\n    def stop(self,value:object)->NoReturn: raise RuntimeError\ndef f(value:object):\n    c=C()\n    marker=1\n    c.stop(value)\n    return marker\n",true,Some(true),true);
}
#[test]
fn returning_instance_overloads_preserve_tail() {
    check("from typing import overload\nclass C:\n    @overload\n    def stop(self,value:int)->int: ...\n    @overload\n    def stop(self,value:str)->str: ...\n    def stop(self,value:object): return value\ndef f(value:int):\n    c=C()\n    marker=1\n    c.stop(value)\n    return marker\n",false,Some(false),false);
}
#[test]
fn mixed_instance_overloads_cannot_prove_all_calls_terminal() {
    check("from typing import overload,Never\nclass C:\n    @overload\n    def stop(self,value:int)->Never: ...\n    @overload\n    def stop(self,value:str)->str: ...\n    def stop(self,value:object): return value\ndef f(value:object):\n    c=C()\n    marker=1\n    c.stop(value)\n    return marker\n",false,None,false);
}
#[test]
fn unknown_decorator_on_implementation_retires_stale_never_overloads() {
    check("from typing import overload,Never\ndef replace(method): return lambda self,value:None\nclass C:\n    @overload\n    def stop(self,value:int)->Never: ...\n    @overload\n    def stop(self,value:str)->Never: ...\n    @replace\n    def stop(self,value:object)->Never: raise RuntimeError\ndef f(value:object):\n    c=C()\n    marker=1\n    c.stop(value)\n    return marker\n",true,Some(true),false);
}
