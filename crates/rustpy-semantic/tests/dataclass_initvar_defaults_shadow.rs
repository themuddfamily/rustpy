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
fn initvar_field_without_effective_default_is_not_member() {
    for declaration in ["field()", "field(default=MISSING)"] {
        with_class(
            &format!(
                "from dataclasses import dataclass, InitVar, field, MISSING\n@dataclass\nclass C:\n    seed: InitVar[int]={declaration}\n"
            ),
            "C",
            |inf, instance| assert!(inf.lookup_member(&instance, "seed").is_none()),
        );
    }
}
#[test]
fn initvar_field_default_retains_actual_default_type() {
    with_class(
        "from dataclasses import dataclass, InitVar, field\n@dataclass\nclass C:\n    seed: InitVar[int]=field(default=7)\n",
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
fn project_dataclasses_shadow_does_not_erase_ordinary_member() {
    let root = tempfile::TempDir::with_prefix("shadow-dataclasses-initvar").unwrap();
    std::fs::write(
        root.path().join("dataclasses.py"),
        "def dataclass(cls): return cls\nclass InitVar:\n    pass\n",
    )
    .unwrap();
    let db = Analysis::for_project(root.path());
    let id = db.set_file_text(
        &root.path().join("m.py"),
        "from dataclasses import dataclass, InitVar\n@dataclass\nclass C:\n    seed: InitVar\n",
    );
    let file = db.file(id).unwrap();
    let inf = Inferer::new(&db);
    let definition = inf
        .resolve_name(&file, file.scopes.module_scope(), "C")
        .unwrap();
    let Type::Class(class, args) = inf.type_of_binding(&definition.file, definition.binding) else {
        panic!("class required")
    };
    let module = db.module("dataclasses").unwrap();
    assert_eq!(module.path, root.path().join("dataclasses.py"));
    assert!(!module.is_stub);
    assert!(inf
        .lookup_member(&Type::Instance(class, args), "seed")
        .is_some());
}
