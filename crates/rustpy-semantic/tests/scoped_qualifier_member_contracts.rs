use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn value_type(declaration: &str) -> String {
    let source = format!("class Dog:pass\n{declaration}\nargument:Box[Dog]\n");
    let db = Analysis::for_project(Path::new("/scoped-qualifier"));
    let id = db.set_file_text(Path::new("/scoped-qualifier/m.py"), &source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
    let annotation = module
        .statements()
        .filter_map(|statement| match statement {
            ast::Stmt::AnnAssign(a) => a.annotation(),
            _ => None,
        })
        .last()
        .unwrap();
    let inf = Inferer::new(&db);
    let instance = inf.annotation_type(&file, &annotation);
    inf.lookup_member(&instance, "value")
        .expect("actual generic member")
        .to_string()
}
#[test]
fn quoted_final_and_import_alias_keep_physical_type_parameter_identity() {
    assert_eq!(value_type("from typing import Final\nclass Box[T]:\n    value:Final[T]\n    def __init__(self,value:T)->None:self.value=value\n"),"Dog");
    assert_eq!(value_type("from typing import Final\nclass Box[T]:\n    value:'Final[T]'\n    def __init__(self,value:T)->None:self.value=value\n"),"Dog");
    assert_eq!(value_type("from typing import Final as F\nclass Box[T]:\n    value:'F[T]'\n    def __init__(self,value:T)->None:self.value=value\n"),"Dog");
}
#[test]
fn quoted_annotated_and_typed_dict_qualifiers_expose_payload() {
    assert_eq!(
        value_type(
            "from typing import Annotated\nclass Box[T]:\n    value:'Annotated[T,\"note\"]'\n"
        ),
        "Dog"
    );
    assert_eq!(value_type("from typing import TypedDict,NotRequired\nclass Box[T](TypedDict):\n    value:'NotRequired[T]'\n"),"Dog");
    assert_eq!(value_type("from typing import TypedDict\nfrom typing_extensions import ReadOnly\nclass Box[T](TypedDict):\n    value:'ReadOnly[T]'\n"),"Dog");
}
#[test]
fn user_named_and_shadowed_qualifiers_keep_their_nominal_types() {
    assert_eq!(
        value_type("class Final[T]:pass\nclass Box[T]:\n    value:'Final[T]'\n"),
        "Final[Dog]"
    );
    assert_eq!(
        value_type(
            "from typing import Final\nclass Final[T]:pass\nclass Box[T]:\n    value:'Final[T]'\n"
        ),
        "Final[Dog]"
    );
    assert_eq!(
        value_type("class Annotated[T]:pass\nclass Box[T]:\n    value:'Annotated[T]'\n"),
        "Annotated[Dog]"
    );
}
