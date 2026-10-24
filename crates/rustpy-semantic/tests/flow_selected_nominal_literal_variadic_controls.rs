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
fn nominal_alias_and_inherited_identity() {
    check("from typing import overload,Never\nclass A:pass\nclass Child(A):pass\nclass B:pass\nAlias=A\n@overload\ndef stop(x:Alias)->Never:...\n@overload\ndef stop(x:B)->int:...\ndef stop(x):return 1\ndef f(value:Child):\n    marker=1\n    stop(value)\n    return marker\n","stop(value)",Some(true),true);
    check("from typing import overload,Never\nclass A:pass\nclass Child(A):pass\nclass B:pass\nAlias=A\n@overload\ndef stop(x:Alias)->Never:...\n@overload\ndef stop(x:B)->int:...\ndef stop(x):return 1\ndef f(value:B):\n    marker=1\n    stop(value)\n    return marker\n","stop(value)",Some(false),false);
}
#[test]
fn literal_alias_decoding_and_distinction() {
    check("from typing import overload,Never,Literal as L\n@overload\ndef stop(x:L[\"zero\"])->Never:...\n@overload\ndef stop(x:str)->str:...\ndef stop(x):return x\ndef f():\n    marker=1\n    stop(\"zero\")\n    return marker\n","stop(\"zero\")",Some(true),true);
    check("from typing import overload,Never,Literal as L\n@overload\ndef stop(x:L[\"zero\"])->Never:...\n@overload\ndef stop(x:str)->str:...\ndef stop(x):return x\ndef f():\n    marker=1\n    stop(\"other\")\n    return marker\n","stop(\"other\")",Some(false),false);
}
#[test]
fn shadowed_literal_never_authorizes_selection() {
    check("from typing import overload,Never\nclass Literal:\n    def __class_getitem__(cls,x):return int\n@overload\ndef stop(x:Literal[0])->Never:...\n@overload\ndef stop(x:int)->int:...\ndef stop(x):return 1\ndef f():\n    marker=1\n    stop(0)\n    return marker\n","stop(0)",None,false);
}
#[test]
fn variadic_keyword_only_required_and_duplicate_refusal() {
    check("from typing import overload,Never\n@overload\ndef stop(*x:int,required:str)->Never:...\n@overload\ndef stop(*x:str,required:str)->int:...\ndef stop(*x,required):return 1\ndef f():\n    marker=1\n    stop(1,2,required=\"ok\")\n    return marker\n","stop(1,2,required=\"ok\")",Some(true),true);
    check("from typing import overload,Never\n@overload\ndef stop(*x:int,required:str)->Never:...\n@overload\ndef stop(*x:str,required:str)->int:...\ndef stop(*x,required):return 1\ndef f():\n    marker=1\n    stop(1,2)\n    return marker\n","stop(1,2)",None,false);
}
#[test]
fn kwargs_values_match_each_declared_contract() {
    check("from typing import overload,Never\n@overload\ndef stop(**x:int)->Never:...\n@overload\ndef stop(**x:str)->int:...\ndef stop(**x):return 1\ndef f():\n    marker=1\n    stop(a=1,b=2)\n    return marker\n","stop(a=1,b=2)",Some(true),true);
    check("from typing import overload,Never\n@overload\ndef stop(**x:int)->Never:...\n@overload\ndef stop(**x:str)->int:...\ndef stop(**x):return 1\ndef f():\n    marker=1\n    stop(a=\"x\")\n    return marker\n","stop(a=\"x\")",Some(false),false);
}
#[test]
fn local_rebinding_and_unknown_earlier_overload_remain_normal() {
    check("from typing import overload,Never\n@overload\ndef stop(x:int)->Never:...\n@overload\ndef stop(x:str)->str:...\ndef stop(x):return x\ndef f():\n    marker=1\n    stop=lambda x:1\n    stop(0)\n    return marker\n","stop(0)",Some(false),false);
    check("from typing import overload,Never\n@overload\ndef stop(x:int)->Never:...\n@overload\ndef stop(x:str)->str:...\ndef stop(x):return x\ndef f(value):\n    marker=1\n    stop(value)\n    return marker\n","stop(value)",None,false);
}

#[test]
fn captured_import_alias_retires_live_proof_but_keeps_frozen_world() {
    let directory = tempfile::TempDir::with_prefix("selected-overload-edit").unwrap();
    let main = directory.path().join("main.py");
    let dependency = directory.path().join("stops.py");
    std::fs::write(
        &main,
        "from stops import stop as halt\ndef f():\n    marker=1\n    halt(0)\n    return marker\n",
    )
    .unwrap();
    std::fs::write(&dependency,"from typing import overload,Never\n@overload\ndef stop(x:int)->Never:...\n@overload\ndef stop(x:str)->str:...\ndef stop(x):return x\n").unwrap();
    let db = Analysis::for_project(directory.path());
    let file = db.file_for_path(&main).unwrap();
    let call = file
        .parse
        .syntax_node()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .find(|call| call.syntax().text() == "halt(0)")
        .unwrap();
    assert!(matches!(
        Inferer::new(&db).type_of_expr(&file, &ast::Expr::Call(call)),
        Type::Never
    ));
    let reference = file
        .scopes
        .references
        .iter()
        .rev()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    let scope = flow::flow_scope(&file, reference.scope).unwrap();
    assert!(!flow::flow_of_with_db(&db, &file, scope)
        .refs
        .contains_key(&reference.range.start()));
    let frozen = db
        .snapshot(
            std::slice::from_ref(&main),
            &rustpy_core::CancellationToken::new(),
        )
        .unwrap();
    db.set_file_text(&dependency, "def stop(x:int)->int:return x\n");
    assert!(flow::flow_of_with_db(&db, &file, scope)
        .refs
        .contains_key(&reference.range.start()));
    let old = frozen.analysis().file_for_path(&main).unwrap();
    let old_reference = old
        .scopes
        .references
        .iter()
        .rev()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    assert!(!flow::flow_of_with_db(
        frozen.analysis(),
        &old,
        flow::flow_scope(&old, old_reference.scope).unwrap()
    )
    .refs
    .contains_key(&old_reference.range.start()));
}
