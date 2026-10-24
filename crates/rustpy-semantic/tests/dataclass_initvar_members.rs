use rustpy_semantic::{
    db::Analysis,
    infer::Inferer,
    types::{ParamKind, Type},
};
use std::path::Path;
fn with_class(source: &str, name: &str, check: impl FnOnce(&Inferer<'_>, Type)) {
    let db = Analysis::for_project(Path::new("/initvar-member"));
    let id = db.set_file_text(Path::new("/initvar-member/m.py"), source);
    let file = db.file(id).unwrap();
    let inf = Inferer::new(&db);
    let definition = inf
        .resolve_name(&file, file.scopes.module_scope(), name)
        .unwrap();
    let Type::Class(class, args) = inf.type_of_binding(&definition.file, definition.binding) else {
        panic!("class required")
    };
    check(&inf, Type::Instance(class, args));
}
#[test]
fn initvar_without_default_is_not_instance_attribute() {
    with_class(
        "from dataclasses import dataclass, InitVar\n@dataclass\nclass C:\n    seed: InitVar[int]\n",
        "C",
        |inf, instance| assert!(inf.lookup_member(&instance, "seed").is_none()),
    );
}
#[test]
fn initvar_default_remains_accessible_class_attribute() {
    with_class(
        "from dataclasses import dataclass, InitVar\n@dataclass\nclass C:\n    seed: InitVar[int]=7\n",
        "C",
        |inf, instance| {
            assert_eq!(
                inf.lookup_member(&instance, "seed").unwrap().to_string(),
                "int"
            )
        },
    );
}
#[test]
fn post_init_explicit_self_store_remains_instance_attribute() {
    with_class(
        "from dataclasses import dataclass, InitVar\n@dataclass\nclass C:\n    seed: InitVar[int]\n    def __post_init__(self,seed:int):\n        self.seed=seed\n",
        "C",
        |inf, instance| assert!(inf.lookup_member(&instance, "seed").is_some()),
    );
}
#[test]
fn initvar_constructor_parameter_survives_with_keyword_only_and_classvar() {
    with_class(
        "from dataclasses import dataclass, InitVar\nfrom typing import ClassVar\n@dataclass(kw_only=True)\nclass C:\n    seed: InitVar[int]\n    shared: ClassVar[int]=2\n    value: str='x'\n",
        "C",
        |inf, instance| {
            let class = instance.class_ref().unwrap();
            let (signature, _) = inf.generated_constructor(class).unwrap();
            let seed = signature
                .params
                .iter()
                .find(|parameter| parameter.name == "seed")
                .unwrap();
            assert_eq!(seed.ty.to_string(), "int");
            assert_eq!(seed.kind, ParamKind::KeywordOnly);
            assert!(
                !signature
                    .params
                    .iter()
                    .any(|parameter| parameter.name == "shared")
            );
            assert!(inf.lookup_member(&instance, "value").is_some());
        },
    );
}
#[test]
fn inherited_initvar_is_constructor_only() {
    with_class(
        "from dataclasses import dataclass, InitVar\n@dataclass\nclass Base:\n    seed: InitVar[int]\n@dataclass\nclass C(Base):\n    value: int=0\n",
        "C",
        |inf, instance| {
            assert!(inf.lookup_member(&instance, "seed").is_none());
            let (signature, _) = inf
                .generated_constructor(instance.class_ref().unwrap())
                .unwrap();
            assert!(
                signature
                    .params
                    .iter()
                    .any(|parameter| parameter.name == "seed" && parameter.ty.to_string() == "int")
            );
        },
    );
}
