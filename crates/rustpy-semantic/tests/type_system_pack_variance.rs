use rustpy_semantic::{
    db::Analysis,
    infer::Inferer,
    types::{Type, Variance},
};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn compatible(source: &str) -> bool {
    let db = Analysis::for_project(Path::new("/pack-variance"));
    let id = db.set_file_text(Path::new("/pack-variance/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
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
    let infer = Inferer::new(&db);
    infer.is_compatible(
        &infer.annotation_type(&file, &annotations[0]),
        &infer.annotation_type(&file, &annotations[1]),
    )
}
#[test]
fn pep695_variadic_return_is_covariant_without_erasing_arity() {
    let prefix="class Animal:pass\nclass Dog(Animal):pass\nclass Values[*Ts]:\n def values(self)->tuple[*Ts]:...\n";
    assert!(compatible(&format!(
        "{prefix}actual:Values[Dog,Dog]\nexpected:Values[Animal,Animal]\n"
    )));
    assert!(!compatible(&format!(
        "{prefix}actual:Values[Animal,Animal]\nexpected:Values[Dog,Dog]\n"
    )));
    assert!(!compatible(&format!(
        "{prefix}actual:Values[Dog]\nexpected:Values[Animal,Animal]\n"
    )));
}
#[test]
fn traditional_tuple_pack_flags_preserve_default_invariance() {
    for (options, accepted) in [
        ("", false),
        (",covariant=True", true),
        (",infer_variance=True", true),
    ] {
        let source=format!("from typing import Generic\nfrom typing_extensions import TypeVarTuple,Unpack\nTs=TypeVarTuple('Ts'{options})\nclass Animal:pass\nclass Dog(Animal):pass\nclass Values(Generic[Unpack[Ts]]):\n def values(self)->tuple[Unpack[Ts]]:...\nactual:Values[Dog,Dog]\nexpected:Values[Animal,Animal]\n");
        assert_eq!(compatible(&source), accepted, "{source}");
    }
}
#[test]
fn parameter_specification_usage_composes_callable_contravariance() {
    let prefix="from typing import Callable,Generic\nfrom typing_extensions import ParamSpec\nP=ParamSpec('P',infer_variance=True)\nclass Animal:pass\nclass Dog(Animal):pass\nclass Callback(Generic[P]):\n def callback(self)->Callable[P,int]:...\n";
    assert!(compatible(&format!(
        "{prefix}actual:Callback[[Animal]]\nexpected:Callback[[Dog]]\n"
    )));
    assert!(!compatible(&format!(
        "{prefix}actual:Callback[[Dog]]\nexpected:Callback[[Animal]]\n"
    )));
    assert!(!compatible(&format!(
        "{prefix}actual:Callback[[Animal,Animal]]\nexpected:Callback[[Dog]]\n"
    )));
}
#[test]
fn contradictory_flags_are_unknown_and_parameter_identity_is_physical() {
    let source="from typing_extensions import ParamSpec,TypeVarTuple\nP=ParamSpec('P',covariant=True,contravariant=True)\nTs=TypeVarTuple('Ts',covariant=True,infer_variance=True)\nGood=TypeVarTuple('Good',contravariant=True)\n";
    let db = Analysis::for_project(Path::new("/pack-variance-authority"));
    let id = db.set_file_text(Path::new("/pack-variance-authority/m.py"), source);
    let file = db.file(id).unwrap();
    let infer = Inferer::new(&db);
    for name in ["P", "Ts"] {
        let binding = file
            .scopes
            .resolve(file.scopes.module_scope(), name)
            .unwrap()[0];
        assert!(matches!(
            infer.type_of_binding(&file, binding),
            Type::Unknown
        ));
    }
    let binding = file
        .scopes
        .resolve(file.scopes.module_scope(), "Good")
        .unwrap()[0];
    let Type::TypeVarTuple(definition) = infer.type_of_binding(&file, binding) else {
        panic!("tuple pack declaration")
    };
    assert_eq!(definition.variance, Variance::Contravariant);
}
