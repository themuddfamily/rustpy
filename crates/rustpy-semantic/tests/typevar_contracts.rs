use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn result(source: &str) -> String {
    let db = Analysis::for_project(Path::new("/typevar-contracts"));
    let id = db.set_file_text(Path::new("/typevar-contracts/m.py"), source);
    let file = db.file(id).unwrap();
    let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
    let ast::Stmt::Expr(statement) = module.statements().last().unwrap() else {
        panic!("expression")
    };
    Inferer::new(&db)
        .type_of_expr(&file, &statement.expr().unwrap())
        .to_string()
}
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
#[test]
fn constrained_typevar_promotes_str_subclass_to_selected_constraint() {
    assert_eq!(
        result(
            "from typing import TypeVar\nT=TypeVar('T',str,bytes)\nclass Text(str):pass\ndef identity(value:T)->T:return value\nidentity(Text())\n"
        ),
        "str"
    );
}
#[test]
fn constrained_typevar_promotes_bytes_subclass_to_selected_constraint() {
    assert_eq!(
        result(
            "from typing import TypeVar\nT=TypeVar('T',str,bytes)\nclass Data(bytes):pass\ndef identity(value:T)->T:return value\nidentity(Data())\n"
        ),
        "bytes"
    );
}
#[test]
fn bound_typevar_preserves_valid_subclass_but_rejects_unrelated_type() {
    assert_eq!(
        result(
            "from typing import TypeVar\nT=TypeVar('T',bound=str)\nclass Text(str):pass\ndef identity(value:T)->T:return value\nidentity(Text())\n"
        ),
        "Text"
    );
    assert!(!compatible(
        "from typing import TypeVar\nT=TypeVar('T',bound=str)\nargument:int\ndef accept(parameter:T)->None:pass\n"
    ));
    assert!(compatible(
        "from typing import TypeVar\nT=TypeVar('T',bound=str)\nargument:str\ndef accept(parameter:T)->None:pass\n"
    ));
}
#[test]
fn constrained_typevar_rejects_type_outside_all_constraints() {
    assert!(!compatible(
        "from typing import TypeVar\nT=TypeVar('T',str,bytes)\nargument:int\ndef accept(parameter:T)->None:pass\n"
    ));
    assert!(compatible(
        "from typing import TypeVar\nT=TypeVar('T',str,bytes)\nargument:bytes\ndef accept(parameter:T)->None:pass\n"
    ));
}
#[test]
fn covariant_generic_compatibility_respects_argument_direction() {
    assert!(compatible(
        "from typing import TypeVar,Generic\nT=TypeVar('T',covariant=True)\nclass Animal:pass\nclass Dog(Animal):pass\nclass Box(Generic[T]):pass\nargument:Box[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n"
    ));
    assert!(!compatible(
        "from typing import TypeVar,Generic\nT=TypeVar('T',covariant=True)\nclass Animal:pass\nclass Dog(Animal):pass\nclass Box(Generic[T]):pass\nargument:Box[Animal]\ndef accept(parameter:Box[Dog])->None:pass\n"
    ));
    assert!(compatible(
        "from typing import TypeVar,Generic\nT=TypeVar('T',covariant=True)\nclass Animal:pass\nclass Dog(Animal):pass\nclass Box(Generic[T]):pass\nargument:Box[Dog]\ndef accept(parameter:Box[Dog])->None:pass\n"
    ));
}
#[test]
fn contravariant_generic_compatibility_respects_argument_direction() {
    assert!(compatible(
        "from typing import TypeVar,Generic\nT=TypeVar('T',contravariant=True)\nclass Animal:pass\nclass Dog(Animal):pass\nclass Box(Generic[T]):pass\nargument:Box[Animal]\ndef accept(parameter:Box[Dog])->None:pass\n"
    ));
    assert!(!compatible(
        "from typing import TypeVar,Generic\nT=TypeVar('T',contravariant=True)\nclass Animal:pass\nclass Dog(Animal):pass\nclass Box(Generic[T]):pass\nargument:Box[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n"
    ));
    assert!(compatible(
        "from typing import TypeVar,Generic\nT=TypeVar('T',contravariant=True)\nclass Animal:pass\nclass Dog(Animal):pass\nclass Box(Generic[T]):pass\nargument:Box[Dog]\ndef accept(parameter:Box[Dog])->None:pass\n"
    ));
}
#[test]
fn invariant_generic_compatibility_respects_argument_direction() {
    assert!(!compatible(
        "from typing import TypeVar,Generic\nT=TypeVar('T')\nclass Animal:pass\nclass Dog(Animal):pass\nclass Box(Generic[T]):pass\nargument:Box[Dog]\ndef accept(parameter:Box[Animal])->None:pass\n"
    ));
    assert!(!compatible(
        "from typing import TypeVar,Generic\nT=TypeVar('T')\nclass Animal:pass\nclass Dog(Animal):pass\nclass Box(Generic[T]):pass\nargument:Box[Animal]\ndef accept(parameter:Box[Dog])->None:pass\n"
    ));
    assert!(compatible(
        "from typing import TypeVar,Generic\nT=TypeVar('T')\nclass Animal:pass\nclass Dog(Animal):pass\nclass Box(Generic[T]):pass\nargument:Box[Dog]\ndef accept(parameter:Box[Dog])->None:pass\n"
    ));
}
