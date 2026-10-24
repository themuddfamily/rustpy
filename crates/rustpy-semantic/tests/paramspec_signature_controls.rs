use rustpy_semantic::{
    db::Analysis,
    infer::Inferer,
    types::{ParamKind, Type},
};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

fn result(source: &str) -> Type {
    let db = Analysis::for_project(Path::new("/paramspec-controls"));
    let id = db.set_file_text(Path::new("/paramspec-controls/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
    let ast::Stmt::Expr(expression) = module.statements().last().unwrap() else {
        panic!("expression");
    };
    Inferer::new(&db).type_of_expr(&file, &expression.expr().unwrap())
}
fn one_parameter(ty: Type, name: &str, kind: ParamKind, parameter: &str, returned: &str) {
    let sig = ty.signature().expect("actual callable signature");
    assert_eq!(sig.params.len(), 1);
    assert_eq!(sig.params[0].name.as_str(), name);
    assert_eq!(sig.params[0].kind, kind);
    assert_eq!(sig.params[0].ty.to_string(), parameter);
    assert_eq!(sig.ret.to_string(), returned);
}
#[test]
fn canonical_factory_import_alias_and_parameter_alias_preserve_shape() {
    one_parameter(result("from typing import Callable,ParamSpec as PS,TypeVar\nFactory=PS\nP=Factory('P')\nAlias=P\nR=TypeVar('R')\ndef relay(fn:Callable[Alias,R])->Callable[Alias,R]:return fn\ndef target(*,value:str)->int:return 1\nrelay(target)\n"), "value", ParamKind::KeywordOnly, "str", "int");
}
#[test]
fn pep695_parameter_specification_preserves_positional_only_shape() {
    one_parameter(result("from typing import Callable\ndef relay[**P,R](fn:Callable[P,R])->Callable[P,R]:return fn\ndef target(value:int,/)->str:return ''\nrelay(target)\n"), "value", ParamKind::PositionalOnly, "int", "str");
}
#[test]
fn bound_method_signature_does_not_reintroduce_self() {
    one_parameter(result("from typing import Callable,ParamSpec,TypeVar\nP=ParamSpec('P')\nR=TypeVar('R')\ndef relay(fn:Callable[P,R])->Callable[P,R]:return fn\nclass C:\n def method(self,*,value:bytes)->int:return 1\nrelay(C().method)\n"), "value", ParamKind::KeywordOnly, "bytes", "int");
}
#[test]
fn fake_parameter_specification_factory_does_not_create_shape_authority() {
    assert!(result("from typing import Callable,TypeVar\ndef ParamSpec(name):return object\nP=ParamSpec('P')\nR=TypeVar('R')\ndef relay(fn:Callable[P,R])->Callable[P,R]:return fn\ndef target(value:int)->str:return ''\nrelay(target)\n").is_unknown());
}
#[test]
fn rebound_parameter_specification_factory_is_not_the_import() {
    assert!(result("from typing import Callable,ParamSpec,TypeVar\ndef fake(name):return object\nParamSpec=fake\nP=ParamSpec('P')\nR=TypeVar('R')\ndef relay(fn:Callable[P,R])->Callable[P,R]:return fn\ndef target(value:int)->str:return ''\nrelay(target)\n").is_unknown());
}
#[test]
fn mismatched_concatenated_prefix_does_not_produce_an_exact_tail() {
    assert!(result("from typing import Callable,Concatenate,ParamSpec,TypeVar\nP=ParamSpec('P')\nR=TypeVar('R')\ndef remove(fn:Callable[Concatenate[str,P],R])->Callable[P,R]:...\ndef target(context:int,/,value:bytes)->int:return 1\nremove(target)\n").is_unknown());
}

#[test]
fn module_member_replacement_retires_canonical_factory_authority() {
    assert!(result("import typing\nfrom typing import Callable,TypeVar\ndef fake(name):return object\ntyping.ParamSpec=fake\nP=typing.ParamSpec('P')\nR=TypeVar('R')\ndef relay(fn:Callable[P,R])->Callable[P,R]:return fn\ndef target(value:int)->str:return ''\nrelay(target)\n").is_unknown());
}

#[test]
fn unknown_prepared_class_namespace_does_not_supply_factory_authority() {
    let ty = result("from typing import ParamSpec\ndef fake(name):return object\nclass Meta(type):\n @classmethod\n def __prepare__(cls,name,bases):return dict(ParamSpec=fake)\nclass C(metaclass=Meta):\n P=ParamSpec('P')\nC.P\nfrom typing import Callable,TypeVar\nR=TypeVar('R')\ndef relay(fn:Callable[C.P,R])->Callable[C.P,R]:return fn\ndef target(value:int,/,flag:bool=True)->str:return ''\nrelay(target)\n");
    let falsely_captured = ty.signature().is_some_and(|signature| {
        signature.params.len() == 2
            && signature.params[0].name.as_str() == "value"
            && signature.params[0].kind == ParamKind::PositionalOnly
            && signature.params[0].ty.to_string() == "int"
            && signature.params[1].name.as_str() == "flag"
            && signature.params[1].kind == ParamKind::Normal
            && signature.params[1].ty.to_string() == "bool"
            && signature.params[1].has_default
            && signature.ret.to_string() == "str"
    });
    assert!(!falsely_captured, "{ty:?}");
}
