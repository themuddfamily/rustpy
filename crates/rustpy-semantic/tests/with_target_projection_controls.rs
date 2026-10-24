use rustpy_semantic::{db::Analysis, infer::Inferer};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(source: &str, expr: &str, expected: &str) {
    let db = Analysis::for_project(Path::new("/with-target-controls"));
    let id = db.set_file_text(Path::new("/with-target-controls/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty());
    let node = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::Expr::cast)
        .filter(|node| node.syntax().text() == expr)
        .last()
        .unwrap();
    assert_eq!(
        Inferer::new(&db).type_of_expr(&file, &node).to_string(),
        expected,
        "{source}"
    );
}
#[test]
fn nested_and_starred_targets_project_the_actual_entered_tuple() {
    let prefix="class Manager:\n def __enter__(self)->tuple[int,tuple[str,bool],float,bytes]:...\n def __exit__(self,*args)->None:...\n";
    check(
        &format!("{prefix}Manager().__enter__()\n"),
        "Manager().__enter__()",
        "tuple[int, tuple[str, bool], float, bytes]",
    );
    for (name, expected) in [
        ("head", "int"),
        ("label", "str"),
        ("flag", "bool"),
        ("middle", "list[float]"),
        ("tail", "bytes"),
    ] {
        check(
            &format!("{prefix}with Manager() as (head,(label,flag),*middle,tail):\n {name}\n"),
            name,
            expected,
        );
    }
}
#[test]
fn async_and_multiple_items_keep_their_own_acquired_values() {
    let prefix="class Sync:\n def __enter__(self)->str:...\n def __exit__(self,*args)->None:...\nclass Async:\n async def __aenter__(self)->tuple[int,str]:...\n async def __aexit__(self,*args)->None:...\n";
    check(
        &format!("{prefix}async def use():\n async with Async() as (count,label):\n  label\n"),
        "label",
        "str",
    );
    check(
        &format!("{prefix}async def use():\n async with Async() as scalar:\n  scalar\n"),
        "scalar",
        "tuple[int, str]",
    );
    check(
        &format!("{prefix}with Sync() as first, Sync() as second:\n second\n"),
        "second",
        "str",
    );
}
#[test]
fn opaque_enter_results_and_invalid_fixed_arity_remain_conservative() {
    check("class Opaque:\n def __enter__(self):...\n def __exit__(self,*args)->None:...\nwith Opaque() as (left,right):\n right\n","right","Unknown");
    check("class Short:\n def __enter__(self)->tuple[int]:...\n def __exit__(self,*args)->None:...\nwith Short() as (left,right):\n left\n","left","Unknown");
}
