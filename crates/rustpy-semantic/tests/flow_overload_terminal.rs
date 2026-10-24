//! Public inference premises and flow consumers; Python source is not executed.
use rustpy_semantic::{db::Analysis, flow, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;

fn check(source: &str, all_never: Option<bool>, inferred_never: Option<bool>, terminal: bool) {
    let db = Analysis::for_project(Path::new("/overload-terminal"));
    let id = db.set_file_text(Path::new("/overload-terminal/m.py"), source);
    let file = db.file(id).unwrap();
    let inf = Inferer::new(&db);
    let call = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .find(|call| call.syntax().text() == "stop(value)")
        .unwrap();
    if let Some(expected) = all_never {
        let Type::Overloaded(signatures) = inf.type_of_expr(&file, &call.callee().unwrap()) else {
            panic!("actual callable must retain overload set: {source}")
        };
        assert_eq!(signatures.len(), 2, "{source}");
        assert_eq!(
            signatures
                .iter()
                .all(|signature| matches!(signature.ret, Type::Never)),
            expected,
            "{source}"
        );
    }
    if let Some(expected) = inferred_never {
        let actual = inf.type_of_expr(&file, &ast::Expr::Call(call));
        assert_eq!(
            matches!(actual, Type::Never),
            expected,
            "public actual call result {actual}: {source}"
        );
    }
    let reference = file
        .scopes
        .references
        .iter()
        .rev()
        .find(|reference| reference.name == "marker" && !reference.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(
        &db,
        &file,
        flow::flow_scope(&file, reference.scope).unwrap(),
    );
    assert_eq!(
        !info.refs.contains_key(&reference.range.start()),
        terminal,
        "actual overload completion must preserve tail authority: {source}"
    );
}

#[test]
fn all_never_overloads_stop_actual_flow_tail() {
    check("from typing import overload, Never, NoReturn\n@overload\ndef stop(value:int)->Never: ...\n@overload\ndef stop(value:str)->NoReturn: ...\ndef stop(value:object)->NoReturn:\n    raise RuntimeError\ndef f(value:object):\n    marker=1\n    stop(value)\n    return marker\n", Some(true), Some(true), true);
}
#[test]
fn returning_overloads_keep_actual_tail_reachable() {
    check("from typing import overload\n@overload\ndef stop(value:int)->int: ...\n@overload\ndef stop(value:str)->str: ...\ndef stop(value:object):\n    return value\ndef f(value:int):\n    marker=1\n    stop(value)\n    return marker\n", Some(false), Some(false), false);
}
#[test]
fn mixed_overloads_do_not_prove_every_possible_call_terminal() {
    check("from typing import overload, Never\n@overload\ndef stop(value:int)->Never: ...\n@overload\ndef stop(value:str)->str: ...\ndef stop(value:object):\n    if isinstance(value,int): raise RuntimeError\n    return value\ndef f(value:object):\n    marker=1\n    stop(value)\n    return marker\n", Some(false), None, false);
}
#[test]
fn shadowed_overload_name_has_no_terminal_authority() {
    check("from typing import overload, Never, Callable\n@overload\ndef stop(value:int)->Never: ...\n@overload\ndef stop(value:str)->Never: ...\ndef stop(value:object)->Never: raise RuntimeError\ndef f(stop:Callable[[int],int],value:int):\n    marker=1\n    stop(value)\n    return marker\n", None, Some(false), false);
}
