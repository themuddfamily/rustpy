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
fn annotated_post_init_store_survives_initvar_refinement() {
    with_class(
        "from dataclasses import dataclass, InitVar\n@dataclass\nclass C:\n    seed: InitVar[int]\n    def __post_init__(self,seed:int):\n        self.seed: int=seed\n",
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
fn later_class_store_is_effective_initvar_default() {
    with_class(
        "from dataclasses import dataclass, InitVar\n@dataclass\nclass C:\n    seed: InitVar[int]\n    seed=7\n",
        "C",
        |inf, instance| {
            assert_eq!(
                inf.lookup_member(&instance, "seed").unwrap().to_string(),
                "int"
            )
        },
    );
}
