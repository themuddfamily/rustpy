use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn result(source: &str) -> String {
    let db = Analysis::for_project(Path::new("/generic-default"));
    let id = db.set_file_text(Path::new("/generic-default/m.py"), source);
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
fn unsolved_function_default_and_inference_precedence() {
    let p = "from typing import TypeVar\nT=TypeVar('T',default=str)\n";
    assert_eq!(result(&format!("{p}def make()->T:...\nmake()\n")), "str");
    assert_eq!(
        result(&format!("{p}def identity(x:T)->T:return x\nidentity(1)\n")),
        "int"
    );
}
#[test]
fn dependent_defaults_use_earlier_declaration_keys() {
    let p="from typing import TypeVar\nT=TypeVar('T',default=str)\nU=TypeVar('U',default=list[T])\ndef make()->tuple[T,U]:...\n";
    assert_eq!(result(&format!("{p}make()\n")), "tuple[str, list[str]]");
    assert_eq!(result("from typing import TypeVar\nT=TypeVar('T',default=str)\nU=TypeVar('U',default=list[T])\ndef make(x:T)->tuple[T,U]:...\nmake(1)\n"),"tuple[int, list[int]]");
}
#[test]
fn defaults_validate_bounds_and_exact_constraints() {
    assert_eq!(result("from typing import TypeVar\nT=TypeVar('T',bound=str,default=int)\ndef make()->T:...\nmake()\n"),"Unknown");
    assert_eq!(result("from typing import TypeVar\nT=TypeVar('T',str,bytes,default=int)\ndef make()->T:...\nmake()\n"),"Unknown");
    assert_eq!(result("from typing import TypeVar\nclass Text(str):pass\nT=TypeVar('T',str,bytes,default=Text)\ndef make()->T:...\nmake()\n"),"Unknown");
    assert_eq!(result("from typing import TypeVar\nT=TypeVar('T',str,bytes,default=bytes)\ndef make()->T:...\nmake()\n"),"bytes");
}
#[test]
fn omitted_class_annotation_slots_use_defaults() {
    assert_eq!(result("from typing import TypeVar,Generic\nT=TypeVar('T',default=str)\nclass Box(Generic[T]):pass\nvalue:Box\nvalue\n"),"Box[str]");
    let p="from typing import TypeVar,Generic\nT=TypeVar('T')\nU=TypeVar('U',default=bytes)\nclass Pair(Generic[T,U]):pass\n";
    assert_eq!(
        result(&format!("{p}value:Pair[int]\nvalue\n")),
        "Pair[int, bytes]"
    );
    assert_eq!(
        result(&format!("{p}value:Pair[int,str]\nvalue\n")),
        "Pair[int, str]"
    );
}
#[test]
fn constructors_apply_defaults_only_after_argument_inference() {
    let p="from typing import TypeVar,Generic\nT=TypeVar('T',default=str)\nclass Box(Generic[T]):\n    def __init__(self,value:T='a')->None:pass\n";
    assert_eq!(result(&format!("{p}Box()\n")), "Box[str]");
    assert_eq!(result(&format!("{p}Box(1)\n")), "Box[int]");
    assert_eq!(result(&format!("{p}Box[bytes]()\n")), "Box[bytes]");
}
#[test]
fn dependent_class_defaults_and_explicit_unknown_are_preserved() {
    let p="from typing import TypeVar,Generic,Any\nT=TypeVar('T',default=str)\nU=TypeVar('U',default=list[T])\nclass Pair(Generic[T,U]):pass\n";
    assert_eq!(
        result(&format!("{p}value:Pair[int]\nvalue\n")),
        "Pair[int, list[int]]"
    );
    assert_eq!(result(&format!("{p}Pair[int]()\n")), "Pair[int, list[int]]");
    assert_eq!(
        result(&format!("{p}value:Pair[Any]\nvalue\n")),
        "Pair[Any, list[Any]]"
    );
}
#[test]
fn invalid_parameter_order_and_conflicting_solution_do_not_use_defaults() {
    assert_eq!(result("from typing import TypeVar,Generic\nT=TypeVar('T',default=str)\nU=TypeVar('U',default=T)\nclass Reversed(Generic[U,T]):pass\nvalue:Reversed\nvalue\n"),"Reversed[Unknown, str]");
    assert_eq!(result("from typing import TypeVar\nT=TypeVar('T',str,bytes,default=str)\ndef pair(x:T,y:T)->T:...\npair('x',b'y')\n"),"Unknown");
}

#[test]
fn pep695_defaults_use_same_declared_solver() {
    assert_eq!(result("def make[T=str]()->T:...\nmake()\n"), "str");
    assert_eq!(
        result("class Box[T=str]:pass\nvalue:Box\nvalue\n"),
        "Box[str]"
    );
    assert_eq!(
        result("class Pair[T=str,U=list[T]]:pass\nvalue:Pair[int]\nvalue\n"),
        "Pair[int, list[int]]"
    );
}

#[test]
fn function_declared_order_does_not_depend_on_result_occurrences() {
    assert_eq!(
        result("def make[T=str,U=list[T]]()->tuple[U,T]:...\nmake()\n"),
        "tuple[list[str], str]"
    );
    assert_eq!(
        result("def make[T=str,U=list[T]]()->U:...\nmake()\n"),
        "list[str]"
    );
    assert_eq!(
        result("def make[T=str,U=list[T]](x:T)->U:...\nmake(1)\n"),
        "list[int]"
    );
}
