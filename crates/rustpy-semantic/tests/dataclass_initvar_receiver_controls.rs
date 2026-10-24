use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
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
fn renamed_receiver_store_remains_instance_member() {
    with_class(
        "from dataclasses import dataclass, InitVar\n@dataclass\nclass C:\n    seed: InitVar[int]\n    def __post_init__(this,seed:int):\n        this.seed: int=seed\n",
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
fn static_and_classmethod_stores_do_not_create_instance_member() {
    for decorator in ["staticmethod", "classmethod"] {
        with_class(
            &format!(
                "from dataclasses import dataclass, InitVar\n@dataclass\nclass C:\n    seed: InitVar[int]\n    @{decorator}\n    def helper(this,seed:int):\n        this.seed: int=seed\n"
            ),
            "C",
            |inf, instance| assert!(inf.lookup_member(&instance, "seed").is_none()),
        );
    }
}
