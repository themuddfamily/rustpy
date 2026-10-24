use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn result(source: &str) -> String {
    let db = Analysis::for_project(Path::new("/default-context"));
    let id = db.set_file_text(Path::new("/default-context/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
    let ast::Stmt::Expr(statement) = module.statements().last().unwrap() else {
        panic!("expression")
    };
    Inferer::new(&db)
        .type_of_expr(&file, &statement.expr().unwrap())
        .to_string()
}
#[test]
fn implicit_legacy_declaration_order_is_independent_of_return_order() {
    let prefix =
        "from typing import TypeVar\nT=TypeVar('T',default=str)\nU=TypeVar('U',default=list[T])\n";
    assert_eq!(
        result(&format!(
            "{prefix}def make(seed:T=...)->tuple[U,T]:...\nmake()\n"
        )),
        "tuple[list[str], str]"
    );
    assert_eq!(
        result(&format!("{prefix}def make(seed:T=...)->U:...\nmake()\n")),
        "list[str]"
    );
    assert_eq!(
        result(&format!("{prefix}def make(seed:T=...)->U:...\nmake(1)\n")),
        "list[int]"
    );
}
#[test]
fn unbound_generic_method_keeps_class_inference_and_own_defaults() {
    let prefix = "class Box[T=str]:\n    def combine[U=bytes](self)->tuple[T,U]:...\n";
    assert_eq!(
        result(&format!("{prefix}Box.combine(Box())\n")),
        "tuple[str, bytes]"
    );
    assert_eq!(
        result(&format!("{prefix}Box[int].combine(Box[int]())\n")),
        "tuple[int, bytes]"
    );
    assert_eq!(
        result(&format!("{prefix}Box[int]().combine()\n")),
        "tuple[int, bytes]"
    );
}
#[test]
fn class_parameter_defaults_do_not_replace_explicit_method_arguments() {
    let prefix = "class Box[T=str]:\n    def combine[U=bytes](self,value:U)->tuple[T,U]:...\n";
    assert_eq!(
        result(&format!("{prefix}Box.combine(Box(),1)\n")),
        "tuple[str, int]"
    );
    assert_eq!(
        result(&format!("{prefix}Box[int]().combine('x')\n")),
        "tuple[int, str]"
    );
}
#[test]
fn legacy_default_alias_keys_remain_distinct() {
    assert_eq!(result("from typing import TypeVar\nT=TypeVar('T',default=str)\nAlias=T\nU=TypeVar('U',default=list[Alias])\ndef make(seed:Alias=...)->tuple[U,Alias]:...\nmake()\n"),"tuple[list[str], str]");
}
