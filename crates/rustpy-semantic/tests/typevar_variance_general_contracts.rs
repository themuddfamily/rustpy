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
fn public_final_field_is_readonly_covariant() {
    let prefix=String::from(NOMINAL)+"from typing import Final\nclass Box[T]:\n    value:Final[T]\n    def __init__(self,value:T)->None:self.value=value\n";
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
fn callable_argument_polarity_is_composed() {
    let prefix=String::from(NOMINAL)+"from typing import Callable\nclass Box[T]:\n    def consume(self,callback:Callable[[T],None])->None:pass\n";
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
fn private_constructor_storage_keeps_read_interface_covariance() {
    let prefix=String::from(NOMINAL)+"class Box[T]:\n    def __init__(self,value:T)->None:self._value=value\n    def get(self)->T:return self._value\n";
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
fn independent_parameters_and_mixed_declared_variance() {
    let prefix = String::from(NOMINAL)
        + "class Transform[I,O]:\n    def apply(self,value:I)->O:raise RuntimeError\n";
    assert!(compatible(&format!("{prefix}argument:Transform[Animal,Dog]\ndef accept(parameter:Transform[Dog,Animal])->None:pass\n")));
    assert!(!compatible(&format!("{prefix}argument:Transform[Dog,Animal]\ndef accept(parameter:Transform[Animal,Dog])->None:pass\n")));
    let prefix=String::from(NOMINAL)+"from typing import TypeVar,Generic\nI=TypeVar('I',infer_variance=True)\nO=TypeVar('O',covariant=True)\nclass Transform(Generic[I,O]):\n    def apply(self,value:I)->O:raise RuntimeError\n";
    assert!(compatible(&format!("{prefix}argument:Transform[Animal,Dog]\ndef accept(parameter:Transform[Dog,Animal])->None:pass\n")));
    assert!(!compatible(&format!("{prefix}argument:Transform[Dog,Animal]\ndef accept(parameter:Transform[Animal,Dog])->None:pass\n")));
}
