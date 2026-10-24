use rustpy_semantic::{
    db::Analysis,
    infer::Inferer,
    types::{ParamKind, Signature, Type},
};
use rustpy_syntax::ast::{self, AstNode};
use std::{path::Path, sync::Arc};

fn result(source: &str) -> Type {
    let db = Analysis::for_project(Path::new("/variadic-contracts"));
    let id = db.set_file_text(Path::new("/variadic-contracts/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
    let ast::Stmt::Expr(statement) = module.statements().last().unwrap() else {
        panic!("final expression");
    };
    Inferer::new(&db).type_of_expr(&file, &statement.expr().unwrap())
}
fn signature(ty: Type) -> Arc<Signature> {
    match ty {
        Type::Function(sig) | Type::Callable(sig) => sig,
        other => panic!("actual callable required, got {other:?}"),
    }
}
const FORWARD: &str = "from typing import Callable, ParamSpec, TypeVar\nP=ParamSpec('P')\nR=TypeVar('R')\ndef relay(fn:Callable[P,R])->Callable[P,R]:return fn\n";

#[test]
fn paramspec_preserves_parameter_names_kinds_defaults_and_return() {
    let sig = signature(result(&format!("{FORWARD}def target(value:int,/,label:str='x',*,flag:bool=False)->str:return label\nrelay(target)\n")));
    assert_eq!(sig.ret.to_string(), "str");
    assert_eq!(sig.params.len(), 3);
    assert_eq!(sig.params[0].name.as_str(), "value");
    assert_eq!(sig.params[0].kind, ParamKind::PositionalOnly);
    assert_eq!(sig.params[0].ty.to_string(), "int");
    assert!(!sig.params[0].has_default);
    assert_eq!(sig.params[1].name.as_str(), "label");
    assert_eq!(sig.params[1].kind, ParamKind::Normal);
    assert_eq!(sig.params[1].ty.to_string(), "str");
    assert!(sig.params[1].has_default);
    assert_eq!(sig.params[2].name.as_str(), "flag");
    assert_eq!(sig.params[2].kind, ParamKind::KeywordOnly);
    assert_eq!(sig.params[2].ty.to_string(), "bool");
    assert!(sig.params[2].has_default);
}
#[test]
fn paramspec_keeps_varargs_and_kwargs_separate() {
    let sig = signature(result(&format!(
        "{FORWARD}def target(*values:int,**labels:str)->bytes:return b''\nrelay(target)\n"
    )));
    assert_eq!(sig.ret.to_string(), "bytes");
    assert_eq!(sig.params.len(), 2);
    assert_eq!(sig.params[0].kind, ParamKind::VarArgs);
    assert_eq!(sig.params[0].ty.to_string(), "int");
    assert_eq!(sig.params[1].kind, ParamKind::KwArgs);
    assert_eq!(sig.params[1].ty.to_string(), "str");
}
#[test]
fn concatenate_removes_only_the_positional_prefix() {
    let source = "from typing import Callable,Concatenate,ParamSpec,TypeVar\nP=ParamSpec('P')\nR=TypeVar('R')\ndef remove(fn:Callable[Concatenate[int,P],R])->Callable[P,R]:...\ndef target(context:int,/,value:str,*,flag:bool)->bytes:return b''\nremove(target)\n";
    let sig = signature(result(source));
    assert_eq!(sig.ret.to_string(), "bytes");
    assert_eq!(sig.params.len(), 2);
    assert_eq!(sig.params[0].name.as_str(), "value");
    assert_eq!(sig.params[0].kind, ParamKind::Normal);
    assert_eq!(sig.params[0].ty.to_string(), "str");
    assert_eq!(sig.params[1].name.as_str(), "flag");
    assert_eq!(sig.params[1].kind, ParamKind::KeywordOnly);
}
#[test]
fn typevartuple_preserves_order_and_heterogeneous_arity() {
    let ty = result("from typing import TypeVarTuple,Unpack\nTs=TypeVarTuple('Ts')\ndef pack(*values:Unpack[Ts])->tuple[Unpack[Ts]]:return values\npack(1,'x',True)\n");
    let Type::Tuple(elements) = ty else {
        panic!("fixed tuple required, got {ty:?}");
    };
    assert_eq!(
        elements.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["int", "str", "bool"]
    );
}
#[test]
fn typevartuple_captures_an_empty_pack() {
    let ty = result("from typing import TypeVarTuple,Unpack\nTs=TypeVarTuple('Ts')\ndef pack(*values:Unpack[Ts])->tuple[Unpack[Ts]]:return values\npack()\n");
    assert!(
        matches!(ty, Type::Tuple(ref elements) if elements.is_empty()),
        "{ty:?}"
    );
}
#[test]
fn tuple_prefix_and_suffix_leave_exact_pack_middle() {
    let ty = result("from typing import TypeVarTuple,Unpack\nTs=TypeVarTuple('Ts')\ndef middle(value:tuple[int,Unpack[Ts],bytes])->tuple[Unpack[Ts]]:...\nmiddle((1,'x',True,b'end'))\n");
    let Type::Tuple(elements) = ty else {
        panic!("fixed tuple required, got {ty:?}");
    };
    assert_eq!(
        elements.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["str", "bool"]
    );
}
