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
fn fake_overload_decorator_cannot_grant_covariance() {
    let prefix=String::from(NOMINAL)+"def overload(function):return function\nclass Box[T]:\n    @overload\n    def get(self,key:int)->T:raise RuntimeError\n    @overload\n    def get(self,key:str)->T:raise RuntimeError\n    def get(self,key:int|str)->T:raise RuntimeError\n";
    assert!(!compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n"
    )));
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
}
#[test]
fn canonical_overload_alias_checks_every_exposed_signature() {
    let prefix=String::from(NOMINAL)+"from typing import overload as ov\nclass Box[T]:\n    @ov\n    def convert(self,key:int,value:T)->None:...\n    @ov\n    def convert(self,key:str)->T:...\n    def convert(self,key:int|str,value:T|None=None)->T|None:raise RuntimeError\n";
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
fn nested_final_metadata_cannot_mark_a_plain_payload_readonly() {
    let prefix = String::from(NOMINAL)
        + "from typing import Annotated,Final\nclass Box[T]:\n    value:Annotated[T,Final[int]]\n";
    assert!(!compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n"
    )));
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
}
