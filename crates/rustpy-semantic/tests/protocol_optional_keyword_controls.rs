use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use std::path::Path;
fn compatible(definitions: &str, expected: bool) {
    let db = Analysis::for_project(Path::new("/protocol-compatibility"));
    let id = db.set_file_text(Path::new("/protocol-compatibility/m.py"), definitions);
    let file = db.file(id).unwrap();
    let inf = Inferer::new(&db);
    let class = |name: &str| {
        let definition = inf
            .resolve_name(&file, file.scopes.module_scope(), name)
            .unwrap();
        let Type::Class(class, args) = inf.type_of_binding(&definition.file, definition.binding)
        else {
            panic!("class expected")
        };
        Type::Instance(class, args)
    };
    assert_eq!(
        inf.is_compatible(&class("C"), &class("P")),
        expected,
        "{definitions}"
    );
}
#[test]
fn optional_renamed_slot_with_kwargs_accepts_protocol_keyword_route() {
    compatible("from typing import Protocol\nclass P(Protocol):\n    def run(self,value:int)->str: ...\nclass C:\n    def run(self,other:int=0,**kwargs:int)->str: return 'ok'\n",true);
}
#[test]
fn required_renamed_slot_with_kwargs_cannot_omit_required_argument() {
    compatible("from typing import Protocol\nclass P(Protocol):\n    def run(self,value:int)->str: ...\nclass C:\n    def run(self,other:int,**kwargs:int)->str: return 'ok'\n",false);
}
