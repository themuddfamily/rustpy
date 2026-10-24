use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn compatible(source: &str) -> bool {
    let db = Analysis::for_project(Path::new("/typevar-compatible"));
    let id = db.set_file_text(Path::new("/typevar-compatible/m.py"), source);
    let file = db.file(id).unwrap();
    let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
    let inf = Inferer::new(&db);
    let annotations: Vec<_> = module
        .statements()
        .filter_map(|statement| match statement {
            ast::Stmt::AnnAssign(a) => a.annotation(),
            _ => None,
        })
        .collect();
    assert_eq!(annotations.len(), 1);
    let function = module
        .statements()
        .find_map(|statement| match statement {
            ast::Stmt::FuncDef(function) if function.name_text() == "accept" => Some(function),
            _ => None,
        })
        .unwrap();
    let signature = inf.signature(&file, &function, None);
    assert_eq!(signature.params.len(), 1);
    inf.is_compatible(
        &inf.annotation_type(&file, &annotations[0]),
        &signature.params[0].ty,
    )
}
const NOMINAL: &str = "class Animal:pass\nclass Dog(Animal):pass\n";
#[test]
fn return_is_covariant() {
    let prefix = String::from(NOMINAL) + "class Box[T]:\n    def get(self)->T:raise RuntimeError\n";
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n"
    )));
    assert!(!compatible(&format!(
        "{prefix}argument:Box[Animal]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
    let prefix=String::from(NOMINAL)+"from typing import TypeVar,Generic\nT=TypeVar(\"T\",infer_variance=True)\nclass Box(Generic[T]):\n    def get(self)->T:raise RuntimeError\n";
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n"
    )));
    assert!(!compatible(&format!(
        "{prefix}argument:Box[Animal]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
}
#[test]
fn parameter_is_contravariant() {
    let prefix = String::from(NOMINAL) + "class Box[T]:\n    def put(self,value:T)->None:pass\n";
    assert!(!compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n"
    )));
    assert!(compatible(&format!(
        "{prefix}argument:Box[Animal]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
    let prefix=String::from(NOMINAL)+"from typing import TypeVar,Generic\nT=TypeVar(\"T\",infer_variance=True)\nclass Box(Generic[T]):\n    def put(self,value:T)->None:pass\n";
    assert!(!compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n"
    )));
    assert!(compatible(&format!(
        "{prefix}argument:Box[Animal]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
}
#[test]
fn mutable_field_is_invariant() {
    let prefix = String::from(NOMINAL) + "class Box[T]:\n    value:T\n";
    assert!(!compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n"
    )));
    assert!(!compatible(&format!(
        "{prefix}argument:Box[Animal]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
    let prefix=String::from(NOMINAL)+"from typing import TypeVar,Generic\nT=TypeVar(\"T\",infer_variance=True)\nclass Box(Generic[T]):\n    value:T\n";
    assert!(!compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n"
    )));
    assert!(!compatible(&format!(
        "{prefix}argument:Box[Animal]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
}
#[test]
fn property_getter_is_covariant() {
    let prefix = String::from(NOMINAL)
        + "class Box[T]:\n    @property\n    def value(self)->T:raise RuntimeError\n";
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n"
    )));
    assert!(!compatible(&format!(
        "{prefix}argument:Box[Animal]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
    let prefix=String::from(NOMINAL)+"from typing import TypeVar,Generic\nT=TypeVar(\"T\",infer_variance=True)\nclass Box(Generic[T]):\n    @property\n    def value(self)->T:raise RuntimeError\n";
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n"
    )));
    assert!(!compatible(&format!(
        "{prefix}argument:Box[Animal]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
}
#[test]
fn property_setter_restores_invariance() {
    let prefix=String::from(NOMINAL)+"class Box[T]:\n    @property\n    def value(self)->T:raise RuntimeError\n    @value.setter\n    def value(self,new:T)->None:pass\n";
    assert!(!compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n"
    )));
    assert!(!compatible(&format!(
        "{prefix}argument:Box[Animal]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
    let prefix=String::from(NOMINAL)+"from typing import TypeVar,Generic\nT=TypeVar(\"T\",infer_variance=True)\nclass Box(Generic[T]):\n    @property\n    def value(self)->T:raise RuntimeError\n    @value.setter\n    def value(self,new:T)->None:pass\n";
    assert!(!compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n"
    )));
    assert!(!compatible(&format!(
        "{prefix}argument:Box[Animal]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
}
#[test]
fn inherited_and_nested_variance_preserve_actual_parameter_keys() {
    let prefix=String::from(NOMINAL)+"class Producer[T]:\n    def get(self)->T:raise RuntimeError\nclass Derived[T](Producer[T]):pass\nclass Consumer[T]:\n    def put(self,value:T)->None:pass\nclass Twice[T]:\n    def put(self,value:Consumer[T])->None:pass\n";
    assert!(compatible(&format!(
        "{prefix}argument:Derived[Dog]\ndef accept(parameter:Derived[Animal])->None:pass\n"
    )));
    assert!(!compatible(&format!(
        "{prefix}argument:Derived[Animal]\ndef accept(parameter:Derived[Dog])->None:pass\n"
    )));
    assert!(compatible(&format!(
        "{prefix}argument:Twice[Dog]\ndef accept(parameter:Twice[Animal])->None:pass\n"
    )));
    assert!(!compatible(&format!(
        "{prefix}argument:Twice[Animal]\ndef accept(parameter:Twice[Dog])->None:pass\n"
    )));
}
#[test]
fn opaque_class_decorator_cannot_grant_automatic_covariance() {
    let prefix=String::from(NOMINAL)+"def opaque(cls):return cls\n@opaque\nclass Box[T]:\n    def get(self)->T:raise RuntimeError\n";
    assert!(!compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n"
    )));
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
}

#[test]
fn custom_global_setter_and_deleter_cannot_grant_covariance() {
    for decorator in ["setter", "deleter"] {
        let prefix=String::from(NOMINAL)+&format!("class OpaqueDescriptor:\n    def setter(self,function):return function\n    def deleter(self,function):return function\nvalue=OpaqueDescriptor()\nclass Box[T]:\n    @value.{decorator}\n    def value(self)->T:raise RuntimeError\n");
        assert!(!compatible(&format!(
            "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n"
        )));
        assert!(!compatible(&format!(
            "{prefix}argument:Box[Animal]\ndef accept(parameter:Box[Dog])->None:pass\n"
        )));
        assert!(compatible(&format!(
            "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Dog])->None:pass\n"
        )));
    }
}
