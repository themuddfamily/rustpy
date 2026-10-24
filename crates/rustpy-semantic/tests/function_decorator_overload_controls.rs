use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

fn result(source: &str, expression: &str) -> Type {
    let db = Analysis::for_project(Path::new("/decorator-overload-controls"));
    let id = db.set_file_text(Path::new("/decorator-overload-controls/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{:?}", file.parse.errors());
    let node = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .filter(|node| node.syntax().text() == expression)
        .last()
        .unwrap();
    let infer = Inferer::new(&db);
    infer.type_of_expr(&file, &node)
}

const PREFIX: &str = "from typing import Callable,ParamSpec,TypeVar,overload\nP=ParamSpec('P')\nR=TypeVar('R')\ndef replace(fn:Callable[P,R])->Callable[P,bytes]:...\n";

#[test]
fn transformed_overloads_preserve_actual_parameter_types() {
    let source = format!("{PREFIX}@overload\n@replace\ndef changed(value:int)->int:...\n@overload\n@replace\ndef changed(value:str)->str:...\n@replace\ndef changed(value:int|str)->int|str:...\nchanged\nchanged(1)\nchanged('x')\n");
    let Type::Overloaded(signatures) = result(&source, "changed") else {
        panic!("discriminated overload required");
    };
    assert_eq!(signatures.len(), 2);
    assert_eq!(signatures[0].params[0].ty.to_string(), "int");
    assert_eq!(signatures[1].params[0].ty.to_string(), "str");
    assert_eq!(result(&source, "changed(1)").to_string(), "bytes");
    assert_eq!(result(&source, "changed('x')").to_string(), "bytes");
}

#[test]
fn implementation_only_decorator_does_not_rewrite_explicit_overload_promises() {
    let source = format!("{PREFIX}@overload\ndef changed(value:int)->int:...\n@overload\ndef changed(value:str)->str:...\n@replace\ndef changed(value:int|str)->int|str:...\nchanged(1)\nchanged('x')\n");
    assert_eq!(result(&source, "changed(1)").to_string(), "int");
    assert_eq!(result(&source, "changed('x')").to_string(), "str");
}

#[test]
fn unknown_declared_replacement_does_not_restore_physical_overloads() {
    let source = "from typing import overload\ndef replace(fn):...\n@overload\n@replace\ndef changed(value:int)->int:...\n@overload\n@replace\ndef changed(value:str)->str:...\ndef changed(value:int|str)->int|str:...\nchanged(1)\n";
    assert!(result(source, "changed(1)").is_unknown());
}

#[test]
fn actual_function_replacements_bind_receiver_but_callable_objects_do_not() {
    let source = "from typing import overload\ndef replace(fn):\n def wrapper(self,value:int)->bytes:...\n return wrapper\nclass C:\n @overload\n @replace\n def changed(self,value:int)->int:...\n @overload\n @replace\n def changed(self,value:str)->str:...\n @replace\n def changed(self,value:int|str)->int|str:...\nC().changed\nC().changed(1)\n";
    let Type::Overloaded(signatures) = result(source, "C().changed") else {
        panic!("actual returned function descriptor required");
    };
    assert_eq!(signatures.len(), 2);
    for signature in signatures {
        assert_eq!(signature.params.len(), 1);
        assert_eq!(signature.params[0].name, "value");
        assert_eq!(signature.params[0].ty.to_string(), "int");
        let definition = signature.def.as_ref().unwrap();
        assert_eq!(definition.name, "wrapper");
    }
    assert_eq!(result(source, "C().changed(1)").to_string(), "bytes");
    let object = "from typing import overload\nclass Replacement:\n def __call__(self,value:int)->bytes:...\ndef replace(fn)->Replacement:...\nclass C:\n @overload\n @replace\n def changed(self,value:int)->int:...\n @overload\n @replace\n def changed(self,value:str)->str:...\n @replace\n def changed(self,value:int|str)->int|str:...\nC().changed\n";
    assert!(result(object, "C().changed").is_unknown());
    let annotation = format!("{PREFIX}class C:\n @overload\n @replace\n def changed(self,value:int)->int:...\n @overload\n @replace\n def changed(self,value:str)->str:...\n @replace\n def changed(self,value:int|str)->int|str:...\nC().changed\n");
    assert!(result(&annotation, "C().changed").is_unknown());
}
