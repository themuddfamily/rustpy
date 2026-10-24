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
fn canonical_final_alias_and_ordinary_mutable_fields_have_distinct_variance() {
    for declaration in [
        "from typing import Final as Readonly\nclass Box[T]:\n    value:Readonly[T]\n",
        "import typing as tp\nclass Box[T]:\n    value:tp.Final[T]\n",
    ] {
        let prefix = String::from(NOMINAL) + declaration;
        assert!(compatible(&format!(
            "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n"
        )));
        assert!(!compatible(&format!(
            "{prefix}argument:Box[Animal]\ndef accept(parameter:Box[Dog])->None:pass\n"
        )));
    }
    let prefix = String::from(NOMINAL) + "class Box[T]:\n    value:T\n";
    assert!(!compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n"
    )));
    assert!(compatible(&format!(
        "{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Dog])->None:pass\n"
    )));
}
#[test]
fn fake_and_ambiguous_final_names_cannot_make_a_writable_field_readonly() {
    for declaration in ["class Final:\n    def __class_getitem__(cls,x):return x\nclass Box[T]:\n    value:Final[T]\n", "from typing import Final\nclass Final:\n    def __class_getitem__(cls,x):return x\nclass Box[T]:\n    value:Final[T]\n"] {
  let prefix=String::from(NOMINAL)+declaration;
  assert!(!compatible(&format!("{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n")));
  assert!(compatible(&format!("{prefix}argument:Box[Dog]\ndef accept(parameter:Box[Dog])->None:pass\n")));
 }
}
