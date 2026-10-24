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
fn protocol_method_parameter_and_return_mismatch_rejected() {
    compatible(
        "from typing import Protocol\nclass P(Protocol):\n    def run(self,value:int)->str: ...\nclass C:\n    def run(self,value:str)->int: return 1\n",
        false,
    );
}
#[test]
fn protocol_method_contravariant_parameter_covariant_return_accepted() {
    compatible(
        "from typing import Protocol\nclass P(Protocol):\n    def run(self,value:int)->object: ...\nclass C:\n    def run(self,value:object)->str: return 'ok'\n",
        true,
    );
}
#[test]
fn missing_protocol_data_member_rejected() {
    compatible(
        "from typing import Protocol\nclass P(Protocol):\n    value:int\nclass C:\n    pass\n",
        false,
    );
}
#[test]
fn writable_protocol_data_member_invariant() {
    compatible(
        "from typing import Protocol\nclass P(Protocol):\n    value:object\nclass C:\n    value:int\n",
        false,
    );
}
#[test]
fn readonly_protocol_property_covariant() {
    compatible(
        "from typing import Protocol\nclass P(Protocol):\n    @property\n    def value(self)->object: ...\nclass C:\n    @property\n    def value(self)->int: return 1\n",
        true,
    );
}
#[test]
fn readonly_implementation_cannot_satisfy_writable_protocol_attribute() {
    compatible(
        "from typing import Protocol\nclass P(Protocol):\n    value:int\nclass C:\n    @property\n    def value(self)->int: return 1\n",
        false,
    );
}
#[test]
fn inherited_protocol_requirements_are_checked() {
    compatible(
        "from typing import Protocol\nclass Base(Protocol):\n    value:int\nclass P(Base,Protocol):\n    pass\nclass C:\n    pass\n",
        false,
    );
}
#[test]
fn lookalike_protocol_base_has_no_structural_authority() {
    compatible(
        "class Protocol:\n    pass\nclass P(Protocol):\n    value:int\nclass C:\n    value:int\n",
        false,
    );
}
#[test]
fn implementation_cannot_add_required_keyword_only_parameter() {
    compatible(
        "from typing import Protocol\nclass P(Protocol):\n    def run(self,value:int)->str: ...\nclass C:\n    def run(self,value:int,*,extra:int)->str: return 'ok'\n",
        false,
    );
}
