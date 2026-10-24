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
fn quoted_and_nested_final_fields_are_readonly() {
    let prefix = String::from(NOMINAL)
        + "from typing import Final,Annotated\nclass Box[T]:\n    value:'Final[T]'\n    def __init__(self,value:T)->None:self.value=value\n";
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n"
    )));
    assert!(!compatible(&format!(
        "{prefix}argument:Box[Animal]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
    let prefix=String::from(NOMINAL)+"from typing import Final,Annotated\nclass Box[T]:\n    value:Annotated[Final[T],'note']\n    def __init__(self,value:T)->None:self.value=value\n";
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n"
    )));
    assert!(!compatible(&format!(
        "{prefix}argument:Box[Animal]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
    let prefix=String::from(NOMINAL)+"from typing import Final,Annotated\nclass Box[T]:\n    value:Final[Annotated[T,'note']]\n    def __init__(self,value:T)->None:self.value=value\n";
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
fn canonical_overloaded_method_contracts_are_covariant() {
    let prefix=String::from(NOMINAL)+"from typing import overload\nclass Box[T]:\n    @overload\n    def get(self,key:int)->T:...\n    @overload\n    def get(self,key:str)->T:...\n    def get(self,key:int|str)->T:raise RuntimeError\n";
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
fn recursive_readonly_class_is_covariant() {
    let prefix=String::from(NOMINAL)+"class Box[T]:\n    def get(self)->T:raise RuntimeError\n    def next(self)->'Box[T]|None':return None\n";
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
fn self_result_does_not_erase_other_covariant_contracts() {
    let prefix=String::from(NOMINAL)+"from typing import Self\nclass Box[T]:\n    def get(self)->T:raise RuntimeError\n    def clone(self)->Self:return self\n";
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
fn writable_recursive_fields_are_invariant() {
    let prefix = String::from(NOMINAL)
        + "class Box[T]:\n    def get(self)->T:raise RuntimeError\n    next:'Box[T]|None'\n";
    assert!(!compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n"
    )));
    assert!(!compatible(&format!(
        "{prefix}argument:Box[Animal]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
    let prefix = String::from(NOMINAL)
        + "from typing import Self\nclass Box[T]:\n    def get(self)->T:raise RuntimeError\n    next:Self|None\n";
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
fn bound_usage_and_constraints_preserve_variance_independence() {
    let prefix =
        String::from(NOMINAL) + "class Box[T:Animal]:\n    def get(self)->T:raise RuntimeError\n";
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n"
    )));
    assert!(!compatible(&format!(
        "{prefix}argument:Box[Animal]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
    let prefix = "class Box[T:(int,str)]:\n    def get(self)->T:raise RuntimeError\n";
    assert!(compatible(&format!(
        "{prefix}argument:Box[int]\ndef accept(parameter:Box[int])->None:pass\n"
    )));
    assert!(!compatible(&format!(
        "{prefix}argument:Box[int]\ndef accept(parameter:Box[str])->None:pass\n"
    )));
}
