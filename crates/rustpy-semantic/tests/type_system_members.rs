use rustpy_semantic::{
    db::Analysis,
    infer::Inferer,
    types::{LiteralValue, Type},
};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

fn result(source: &str) -> String {
    let db = Analysis::for_project(Path::new("/type-system-members"));
    let id = db.set_file_text(Path::new("/type-system-members/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
    let ast::Stmt::Expr(statement) = module.statements().last().unwrap() else {
        panic!("last expression")
    };
    Inferer::new(&db)
        .type_of_expr(&file, &statement.expr().unwrap())
        .to_string()
}
fn write_error(source: &str, name: &str) -> bool {
    let db = Analysis::for_project(Path::new("/type-system-writes"));
    let id = db.set_file_text(Path::new("/type-system-writes/m.py"), source);
    let file = db.file(id).unwrap();
    let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
    let ast::Stmt::Expr(statement) = module.statements().last().unwrap() else {
        panic!("last expression")
    };
    let infer = Inferer::new(&db);
    let receiver = infer.type_of_expr(&file, &statement.expr().unwrap());
    infer.member_write_error(&receiver, name, false).is_some()
}
#[test]
fn descriptors_and_dynamic_attribute_results_follow_real_callables() {
    assert_eq!(result("class Descriptor:\n def __get__(self, obj:object, owner:type)->str:return 'value'\nclass C:\n field=Descriptor()\nC().field\n"),"str");
    assert_eq!(
        result("class C:\n def __getattr__(self,name:str)->bytes:return b'value'\nC().missing\n"),
        "bytes"
    );
    assert_eq!(result("class C:\n visible:str='value'\n def __getattribute__(self,name:str)->int:return 1\nC().visible\n"),"int");
    assert_eq!(result("class Meta(type):\n def __getattr__(cls,name:str)->bytes:return b'value'\nclass C(metaclass=Meta):pass\nC.missing\n"),"bytes");
}
#[test]
fn data_descriptors_precede_instance_storage_and_plain_fields_do_not() {
    assert_eq!(result("class D:\n def __get__(self,obj:object,owner:type)->str:return 'value'\n def __set__(self,obj:object,value:object)->None:pass\nclass C:\n field=D()\n def __init__(self):self.field=1\nC().field\n"),"str");
    assert_eq!(
        result("class C:\n field='class'\n def __init__(self):self.field=1\nC().field\n"),
        "int"
    );
}
#[test]
fn frozen_slots_and_property_writes_distinguish_actual_capabilities() {
    assert!(write_error(
        "from dataclasses import dataclass\n@dataclass(frozen=True)\nclass C:\n value:int\nC(1)\n",
        "value"
    ));
    assert!(!write_error(
        "from dataclasses import dataclass\n@dataclass\nclass C:\n value:int\nC(1)\n",
        "value"
    ));
    assert!(write_error(
        "class C:\n __slots__=('value',)\n value:int\nC()\n",
        "other"
    ));
    assert!(!write_error(
        "class C:\n __slots__=('value','__dict__')\nC()\n",
        "other"
    ));
    assert!(write_error(
        "class C:\n @property\n def value(self)->int:return 1\nC()\n",
        "value"
    ));
    assert!(!write_error("class C:\n @property\n def value(self)->int:return 1\n @value.setter\n def value(self,new:int)->None:pass\nC()\n","value"));
}
#[test]
fn final_override_and_runtime_protocol_consumers_use_canonical_metadata() {
    let source="from typing import final,override,Protocol,runtime_checkable\n@final\nclass Sealed:pass\nclass Bad(Sealed):pass\nclass Base:\n @final\n def value(self)->int:return 1\nclass Child(Base):\n def value(self)->int:return 1\n @override\n def absent(self)->int:return 1\nclass P(Protocol):\n def method(self)->int:...\n@runtime_checkable\nclass R(Protocol):\n def method(self)->int:...\n";
    let db = Analysis::for_project(Path::new("/type-system-markers"));
    let id = db.set_file_text(Path::new("/type-system-markers/m.py"), source);
    let file = db.file(id).unwrap();
    let infer = Inferer::new(&db);
    let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
    let errors: Vec<_> = module
        .statements()
        .filter_map(|statement| {
            if let ast::Stmt::ClassDef(class) = statement {
                Some(infer.class_declaration_errors(&file, &class))
            } else {
                None
            }
        })
        .flatten()
        .collect();
    assert_eq!(errors.len(), 3, "{errors:?}");
    for (name, expected) in [("P", true), ("R", false)] {
        let binding = file
            .scopes
            .resolve(file.scopes.module_scope(), name)
            .unwrap()[0];
        let ty = infer.type_of_binding(&file, binding);
        assert_eq!(
            infer.runtime_class_check_error(&ty, false).is_some(),
            expected
        );
    }
}
#[test]
fn flag_members_are_nominal_and_combinations_are_not_closed_domains() {
    let source="from enum import Flag,IntFlag,auto\nclass Access(Flag):\n READ=auto()\n WRITE=auto()\n BOTH=READ|WRITE\nclass Bits(IntFlag):\n ONE=auto()\nAccess.BOTH\n";
    let db = Analysis::for_project(Path::new("/type-system-flags"));
    let id = db.set_file_text(Path::new("/type-system-flags/m.py"), source);
    let file = db.file(id).unwrap();
    let infer = Inferer::new(&db);
    let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
    let ast::Stmt::Expr(last) = module.statements().last().unwrap() else {
        panic!("expression")
    };
    let value = infer.type_of_expr(&file, &last.expr().unwrap());
    let Type::Literal(LiteralValue::EnumMember { class, member }) = value else {
        panic!("nominal flag member")
    };
    assert_eq!(member, "BOTH");
    assert!(infer.enum_member_values(&class).is_none());
    assert_eq!(
        result("from enum import Flag,auto\nclass A(Flag):\n X=auto()\n Y=auto()\nA.X|A.Y\n"),
        "A"
    );
    assert_eq!(
        result("from enum import IntFlag,auto\nclass A(IntFlag):\n X=auto()\nA.X|8\n"),
        "A"
    );
}

#[test]
fn implicit_special_methods_bypass_instance_attribute_hooks() {
    let prefix="class C:\n def __getattribute__(self,name:str)->str:return 'hook'\n def __add__(self,other:object)->int:return 1\n def __getitem__(self,index:int)->bytes:return b'value'\n def __iter__(self)->'C':return self\n def __next__(self)->float:return 1.0\n def __call__(self,*,value:int)->bool:return True\n";
    assert_eq!(result(&format!("{prefix}C()+C()\n")), "int");
    assert_eq!(result(&format!("{prefix}C().__add__\n")), "str");
    assert_eq!(result(&format!("{prefix}C()[0]\n")), "bytes");
    assert_eq!(result(&format!("{prefix}C()(value=1)\n")), "bool");
    assert_eq!(
        result(&format!("{prefix}values=[item for item in C()]\nvalues\n")),
        "list[float]"
    );
    assert_eq!(result("class Manager:\n def __getattribute__(self,name:str)->str:return 'hook'\n def __enter__(self)->bytes:return b'value'\n def __exit__(self,*args:object)->None:pass\nwith Manager() as value:pass\nvalue\n"),"bytes");
}

#[test]
fn tuple_callable_and_numeric_compatibility_check_contracts_not_display_names() {
    fn compatible(source: &str) -> bool {
        let db = Analysis::for_project(Path::new("/type-system-compatibility"));
        let id = db.set_file_text(Path::new("/type-system-compatibility/m.py"), source);
        let file = db.file(id).unwrap();
        let infer = Inferer::new(&db);
        let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
        let annotations: Vec<_> = module
            .statements()
            .filter_map(|statement| {
                if let ast::Stmt::AnnAssign(statement) = statement {
                    statement.annotation()
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(annotations.len(), 2);
        infer.is_compatible(
            &infer.annotation_type(&file, &annotations[0]),
            &infer.annotation_type(&file, &annotations[1]),
        )
    }
    assert!(compatible(
        "actual:tuple[int,str]\nexpected:tuple[int,str]\n"
    ));
    assert!(!compatible(
        "actual:tuple[int,str]\nexpected:tuple[str,int]\n"
    ));
    assert!(compatible(
        "from typing import Callable\nactual:Callable[[int],str]\nexpected:Callable[[int],str]\n"
    ));
    assert!(!compatible(
        "from typing import Callable\nactual:Callable[[int],str]\nexpected:Callable[[str],str]\n"
    ));
    assert!(compatible("actual:int\nexpected:float\n"));
    assert!(!compatible("class int:pass\nactual:int\nexpected:float\n"));
}

#[test]
fn canonical_transform_frozen_default_follows_aliases_bases_and_metaclasses() {
    assert!(write_error("from typing import dataclass_transform as dt\n@dt(frozen_default=True)\nclass Model:pass\nclass C(Model):\n value:int\nC(1)\n","value"));
    assert!(write_error("from typing import dataclass_transform as dt\n@dt(frozen_default=True)\nclass Meta(type):pass\nclass C(metaclass=Meta):\n value:int\nC(1)\n","value"));
    assert!(!write_error("from typing import dataclass_transform as dt\n@dt(frozen_default=True)\nclass Model:pass\nclass C(Model,frozen=False):\n value:int\nC(1)\n","value"));
}
