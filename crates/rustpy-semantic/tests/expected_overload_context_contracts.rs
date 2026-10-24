use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn rhs(source: &str) -> Type {
    let db = Analysis::for_project(Path::new("/expected-overload"));
    let id = db.set_file_text(Path::new("/expected-overload/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
    let ast::Stmt::AnnAssign(assign) = module.statements().last().unwrap() else {
        panic!("actual final RHS required")
    };
    Inferer::new(&db).type_of_expr(&file, &assign.value().unwrap())
}
const EMPTY:&str="from typing import TypeVar,overload\nT=TypeVar('T')\n@overload\ndef choose(kind:int)->list[T]:...\n@overload\ndef choose(kind:str)->dict[str,T]:...\ndef choose(kind):\n    if isinstance(kind,int):return []\n    return {}\n";
#[test]
fn known_argument_selects_overload_before_empty_result_context() {
    let mismatched = rhs(&format!("{EMPTY}value:list[str]=choose('mapping')\n"));
    let Type::Instance(class, _) = mismatched else {
        panic!("actual selected mapping result required")
    };
    assert_eq!(
        class.name.as_str(),
        "dict",
        "expected list must not select int overload for actual str"
    );
    assert_eq!(
        rhs(&format!("{EMPTY}value:list[str]=choose(0)\n")).to_string(),
        "list[str]"
    );
    assert_eq!(
        rhs(&format!("{EMPTY}value:dict[str,int]=choose('mapping')\n")).to_string(),
        "dict[str, int]"
    );
}
const TRANSFORM:&str="from typing import TypeVar,overload,Callable\nR=TypeVar('R')\n@overload\ndef apply(kind:int,transform:Callable[[str],R])->list[R]:...\n@overload\ndef apply(kind:str,transform:Callable[[int],R])->dict[str,R]:...\ndef apply(kind,transform):raise RuntimeError\n";
#[test]
fn selected_argument_contract_contextualizes_actual_lambda_body() {
    assert_eq!(
        rhs(&format!(
            "{TRANSFORM}value:list[str]=apply(0,lambda text:text.upper())\n"
        ))
        .to_string(),
        "list[str]"
    );
    assert_eq!(
        rhs(&format!(
            "{TRANSFORM}value:dict[str,int]=apply('mapping',lambda number:number+1)\n"
        ))
        .to_string(),
        "dict[str, int]"
    );
}
#[test]
fn selected_lambda_actual_return_conflict_is_not_stamped() {
    assert_eq!(
        rhs(&format!(
            "{TRANSFORM}value:list[int]=apply(0,lambda text:text.upper())\n"
        ))
        .to_string(),
        "list[str]"
    );
}
#[test]
fn selected_known_argument_solution_beats_expected_return() {
    let prefix="from typing import TypeVar,overload\nT=TypeVar('T')\n@overload\ndef wrap(kind:int,value:T)->list[T]:...\n@overload\ndef wrap(kind:str,value:T)->dict[str,T]:...\ndef wrap(kind,value):raise RuntimeError\n";
    assert_eq!(
        rhs(&format!("{prefix}value:list[str]=wrap(0,1)\n")).to_string(),
        "list[int]"
    );
}
#[test]
fn selected_unsolved_context_precedes_default_and_validates_bound() {
    let defaults="from typing import overload\nfrom typing_extensions import TypeVar\nT=TypeVar('T',str,bytes,default=bytes)\n@overload\ndef make(kind:int)->list[T]:...\n@overload\ndef make(kind:str)->dict[str,T]:...\ndef make(kind):raise RuntimeError\n";
    assert_eq!(
        rhs(&format!("{defaults}value:list[str]=make(0)\n")).to_string(),
        "list[str]"
    );
    let bounded="from typing import TypeVar,overload\nT=TypeVar('T',bound=str)\n@overload\ndef make(kind:int)->list[T]:...\n@overload\ndef make(kind:str)->dict[str,T]:...\ndef make(kind):raise RuntimeError\n";
    let invalid = rhs(&format!("{bounded}value:list[int]=make(0)\n"));
    assert_ne!(
        invalid.to_string(),
        "list[int]",
        "incompatible expected int must not become a valid str-bounded solution"
    );
}
