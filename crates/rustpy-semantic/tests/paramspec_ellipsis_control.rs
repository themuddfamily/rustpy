use rustpy_semantic::{db::Analysis, infer::Inferer, types::ParamKind};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

#[test]
fn parenthesized_callable_ellipsis_keeps_ordinary_signature() {
    let db = Analysis::for_project(Path::new("/paramspec-ellipsis"));
    let id = db.set_file_text(
        Path::new("/paramspec-ellipsis/m.py"),
        "from typing import Callable\ncallback:Callable[(...),int]\ncallback\n",
    );
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
    let ast::Stmt::Expr(expression) = module.statements().last().unwrap() else {
        panic!("expression");
    };
    let ty = Inferer::new(&db).type_of_expr(&file, &expression.expr().unwrap());
    let signature = ty.signature().expect("ordinary callable");
    assert_eq!(signature.ret.to_string(), "int");
    assert_eq!(signature.params.len(), 1);
    assert_eq!(signature.params[0].kind, ParamKind::VarArgs);
    assert!(signature.params[0].ty.is_unknown());
}
