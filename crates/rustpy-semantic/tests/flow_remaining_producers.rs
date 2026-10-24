//! Source-only proposals: premise failures must be reported separately from Flow gaps.
use rustpy_semantic::{db::Analysis, flow, infer::Inferer, types::Type};
use rustpy_syntax::ast::{self, AstNode};
use std::path::Path;
fn check(source: &str, call_text: &str, never: Option<bool>, terminal: bool) {
    let db = Analysis::for_project(Path::new("/remaining-producer"));
    let id = db.set_file_text(Path::new("/remaining-producer/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    let call = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .find(|call| call.syntax().text() == call_text)
        .unwrap();
    if let Some(expected) = never {
        let ty = Inferer::new(&db).type_of_expr(&file, &ast::Expr::Call(call));
        assert_eq!(
            matches!(ty, Type::Never),
            expected,
            "Inferer premise BEFORE Flow: {ty}; {source}"
        );
    }
    let reference = file
        .scopes
        .references
        .iter()
        .rev()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    let info = flow::flow_of_with_db(
        &db,
        &file,
        flow::flow_scope(&file, reference.scope).unwrap(),
    );
    assert_eq!(
        !info.refs.contains_key(&reference.range.start()),
        terminal,
        "Flow completion: {source}"
    );
}
#[test]
fn nominal_selected_overload() {
    check("from typing import overload,Never\nclass A:pass\nclass B:pass\n@overload\ndef stop(x:A)->Never:...\n@overload\ndef stop(x:B)->int:...\ndef stop(x):return 1\ndef f(value:A):\n    marker=1\n    stop(value)\n    return marker\n","stop(value)",Some(true),true);
    check("from typing import overload,Never\nclass A:pass\nclass B:pass\n@overload\ndef stop(x:A)->Never:...\n@overload\ndef stop(x:B)->int:...\ndef stop(x):return 1\ndef f(value:B):\n    marker=1\n    stop(value)\n    return marker\n","stop(value)",Some(false),false);
    check("from typing import overload,Never\nclass A:pass\nclass B:pass\n@overload\ndef stop(x:A)->Never:...\n@overload\ndef stop(x:B)->int:...\ndef stop(x):return 1\ndef f(value):\n    marker=1\n    stop(value)\n    return marker\n","stop(value)",None,false);
}
#[test]
fn literal_selected_overload() {
    check("from typing import overload,Never,Literal\n@overload\ndef stop(x:Literal[0])->Never:...\n@overload\ndef stop(x:int)->int:...\ndef stop(x):return 1\ndef f(value):\n    marker=1\n    stop(0)\n    return marker\n","stop(0)",Some(true),true);
    check("from typing import overload,Never,Literal\n@overload\ndef stop(x:Literal[0])->Never:...\n@overload\ndef stop(x:int)->int:...\ndef stop(x):return 1\ndef f(value):\n    marker=1\n    stop(1)\n    return marker\n","stop(1)",Some(false),false);
    check("from typing import overload,Never,Literal\n@overload\ndef stop(x:Literal[0])->Never:...\n@overload\ndef stop(x:int)->int:...\ndef stop(x):return 1\ndef f(value):\n    marker=1\n    stop(value)\n    return marker\n","stop(value)",None,false);
}
#[test]
fn variadic_selected_overload() {
    check("from typing import overload,Never\n@overload\ndef stop(*x:int)->Never:...\n@overload\ndef stop(*x:str)->int:...\ndef stop(*x):return 1\ndef f(value):\n    marker=1\n    stop(0,1)\n    return marker\n","stop(0,1)",Some(true),true);
    check("from typing import overload,Never\n@overload\ndef stop(*x:int)->Never:...\n@overload\ndef stop(*x:str)->int:...\ndef stop(*x):return 1\ndef f(value):\n    marker=1\n    stop('a','b')\n    return marker\n","stop('a','b')",Some(false),false);
    check("from typing import overload,Never\n@overload\ndef stop(*x:int)->Never:...\n@overload\ndef stop(*x:str)->int:...\ndef stop(*x):return 1\ndef f(value):\n    marker=1\n    stop(value)\n    return marker\n","stop(value)",None,false);
}
#[test]
fn conditional_callee() {
    check("from typing import Never\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\ndef f(flag):\n    marker=1\n    (stop if flag else stop)()\n    return marker\n","(stop if flag else stop)()",Some(true),true);
    check("from typing import Never\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\ndef f(flag):\n    marker=1\n    (normal if flag else normal)()\n    return marker\n","(normal if flag else normal)()",Some(false),false);
    check("from typing import Never\ndef stop()->Never:raise RuntimeError\ndef normal()->int:return 1\ndef f(flag):\n    marker=1\n    (stop if flag else normal)()\n    return marker\n","(stop if flag else normal)()",Some(false),false);
}
