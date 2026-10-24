use rustpy_semantic::{db::Analysis, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn query(source: &str, text: &str) -> Type {
    let db = Analysis::for_project(Path::new("/expected-context-controls"));
    let id = db.set_file_text(Path::new("/expected-context-controls/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let expression = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .filter(|expression| expression.syntax().text() == text)
        .last()
        .unwrap();
    Inferer::new(&db).type_of_expr(&file, &expression)
}
#[test]
fn nested_empty_lists_receive_actual_rhs_context() {
    assert_eq!(
        query("values:list[list[int]]=[[]]\n", "[[]]").to_string(),
        "list[list[int]]"
    );
}
#[test]
fn nested_dictionary_values_receive_context() {
    assert_eq!(
        query("values:dict[str,list[int]]={'key':[]}\n", "{'key':[]}").to_string(),
        "dict[str, list[int]]"
    );
}
#[test]
fn concrete_collection_conflicts_remain_visible() {
    assert_eq!(
        query("values:list[int]=['bad']\n", "['bad']").to_string(),
        "list[str]"
    );
    assert_eq!(query("value:str=1\n", "1").to_string(), "Literal[1]");
}
#[test]
fn actual_return_annotation_supplies_collection_context() {
    assert_eq!(
        query("def values()->list[str]:\n    return []\n", "[]").to_string(),
        "list[str]"
    );
}
#[test]
fn actual_call_parameter_supplies_collection_context() {
    assert_eq!(
        query("def take(values:list[int])->None:pass\ntake([])\n", "[]").to_string(),
        "list[int]"
    );
}
#[test]
fn contextual_lambda_retains_actual_return_conflict() {
    let ty = query(
        "from typing import Callable\nf:Callable[[str],int]=lambda text:text\n",
        "lambda text:text",
    );
    let Type::Function(signature) = ty else {
        panic!("actual lambda required")
    };
    assert_eq!(signature.params[0].ty.to_string(), "str");
    assert_eq!(signature.ret.to_string(), "str");
}
#[test]
fn actual_call_parameter_context_types_lambda_body() {
    let ty=query("from typing import Callable\ndef take(fn:Callable[[str],str])->None:pass\ntake(lambda text:text.upper())\n","lambda text:text.upper()");
    let Type::Function(signature) = ty else {
        panic!("actual lambda required")
    };
    assert_eq!(signature.params[0].ty.to_string(), "str");
    assert_eq!(signature.ret.to_string(), "str");
}
#[test]
fn expected_return_does_not_overwrite_argument_solution() {
    assert_eq!(query("from typing import TypeVar\nT=TypeVar('T')\ndef identity(value:T)->T:return value\nvalue:str=identity(1)\n","identity(1)").to_string(),"int");
}
#[test]
fn expected_solution_validates_declared_bound() {
    assert_eq!(query("from typing import TypeVar\nT=TypeVar('T',bound=str)\ndef make()->T:raise RuntimeError\nvalue:int=make()\n","make()").to_string(),"Unknown");
}
#[test]
fn expected_solution_selects_declared_constraint() {
    assert_eq!(query("from typing import TypeVar\nT=TypeVar('T',str,bytes)\ndef make()->T:raise RuntimeError\nvalue:bytes=make()\n","make()").to_string(),"bytes");
}
#[test]
fn expected_context_precedes_unsolved_default_only() {
    assert_eq!(query("from typing_extensions import TypeVar\nT=TypeVar('T',default=int)\ndef make()->T:raise RuntimeError\nvalue:str=make()\n","make()").to_string(),"str");
}

#[test]
fn ordinary_callee_and_full_call_do_not_reenter_parent_context() {
    let ty = query(
        "def ordinary(value:int)->int:return value\nordinary(1)\n",
        "ordinary",
    );
    let Type::Function(signature) = ty else {
        panic!("ordinary callable required")
    };
    assert_eq!(signature.params[0].ty.to_string(), "int");
    assert_eq!(
        query(
            "def ordinary(value:int)->int:return value\nordinary(1)\n",
            "ordinary(1)"
        )
        .to_string(),
        "int"
    );
}

#[test]
fn expected_return_types_nested_empty_generic_call_argument() {
    assert_eq!(query("from typing import TypeVar\nT=TypeVar('T')\ndef identity(values:list[T])->list[T]:return values\nvalue:list[str]=identity([])\n","identity([])").to_string(),"list[str]");
}
