use rustpy_semantic::{
    db::Analysis,
    infer::Inferer,
    types::{ParamKind, Type},
};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

// Select the actual RHS, never the annotated variable's declared type.
fn rhs(source: &str) -> Type {
    let db = Analysis::for_project(Path::new("/expected-type-context"));
    let id = db.set_file_text(Path::new("/expected-type-context/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
    let ast::Stmt::AnnAssign(assign) = module.statements().last().unwrap() else {
        panic!("final annotation");
    };
    assert!(assign.annotation().is_some());
    Inferer::new(&db).type_of_expr(&file, &assign.value().unwrap())
}
#[test]
fn annotated_assignment_supplies_empty_list_element_context() {
    assert_eq!(rhs("values:list[int]=[]\n").to_string(), "list[int]");
}
#[test]
fn return_only_generic_call_uses_assignment_context() {
    assert_eq!(rhs("from typing import TypeVar\nT=TypeVar('T')\ndef empty()->list[T]:return []\nvalues:list[str]=empty()\n").to_string(), "list[str]");
}
#[test]
fn contextual_lambda_parameters_are_not_unknown() {
    let ty =
        rhs("from typing import Callable\ntransform:Callable[[str],int]=lambda text:len(text)\n");
    let Type::Function(sig) = ty else {
        panic!("lambda function required, got {ty:?}");
    };
    assert_eq!(sig.params.len(), 1);
    assert_eq!(sig.params[0].kind, ParamKind::Normal);
    assert_eq!(sig.params[0].ty.to_string(), "str");
    assert_eq!(sig.ret.to_string(), "int");
}
