use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
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
fn pep695_automatic_variance_rejects_distinct_concrete_arguments() {
    let prefix = "class Box[T]:\n    value:T\n";
    assert!(!compatible(&format!(
        "{prefix}argument:Box[int]\ndef accept(parameter:Box[str])->None:pass\n"
    )));
    assert!(!compatible(&format!(
        "{prefix}argument:Box[str]\ndef accept(parameter:Box[int])->None:pass\n"
    )));
    assert!(compatible(&format!(
        "{prefix}argument:Box[int]\ndef accept(parameter:Box[int])->None:pass\n"
    )));
}
#[test]
fn factory_automatic_variance_rejects_distinct_concrete_arguments() {
    let prefix = "from typing import TypeVar,Generic\nT=TypeVar(\"T\",infer_variance=True)\nclass Box(Generic[T]):\n    value:T\n";
    assert!(!compatible(&format!(
        "{prefix}argument:Box[int]\ndef accept(parameter:Box[str])->None:pass\n"
    )));
    assert!(!compatible(&format!(
        "{prefix}argument:Box[str]\ndef accept(parameter:Box[int])->None:pass\n"
    )));
    assert!(compatible(&format!(
        "{prefix}argument:Box[int]\ndef accept(parameter:Box[int])->None:pass\n"
    )));
}
