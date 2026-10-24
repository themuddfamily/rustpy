use rustpy_semantic::{db::Analysis, flow};
use std::path::Path;
const PREFIX: &str="from typing import overload,Never\n@overload\ndef stop(value:int)->Never: ...\n@overload\ndef stop(value:str)->str: ...\ndef stop(value:int|str):\n    if isinstance(value,int): raise RuntimeError\n    return value\n";
fn check(extra: &str, body: &str, terminal: bool) {
    let source = format!("{PREFIX}{extra}\n{body}\n");
    let db = Analysis::for_project(Path::new("/selected-overload-controls"));
    let id = db.set_file_text(Path::new("/selected-overload-controls/m.py"), &source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
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
        "{source}"
    );
}
#[test]
fn keyword_selects_terminal_branch() {
    check(
        "",
        "def f():\n    marker=1\n    stop(value=0)\n    return marker",
        true,
    );
}
#[test]
fn current_rebound_argument_keeps_returning_tail() {
    check("","def f(value:int):\n    marker=1\n    value='returning'\n    stop(value)\n    return marker",false);
}
#[test]
fn alias_preserves_selected_contract() {
    check(
        "alias=stop",
        "def f():\n    marker=1\n    alias(0)\n    return marker",
        true,
    );
}
#[test]
fn callback_argument_is_not_speculatively_selected() {
    check(
        "def obtain():\n    return 'returning'",
        "def f():\n    marker=1\n    stop(obtain())\n    return marker",
        false,
    );
}
#[test]
fn walrus_argument_uses_no_stale_annotation() {
    check(
        "",
        "def f(value:int):\n    marker=1\n    stop((value := 'returning'))\n    return marker",
        false,
    );
}
#[test]
fn first_compatible_numeric_branch_wins() {
    check("@overload\ndef numeric(value:int)->Never: ...\n@overload\ndef numeric(value:float)->str: ...\ndef numeric(value):\n    raise RuntimeError","def f():\n    marker=1\n    numeric(0)\n    return marker",true);
}
#[test]
fn async_selection_requires_await() {
    let extra="@overload\nasync def pending(value:int)->Never: ...\n@overload\nasync def pending(value:str)->str: ...\nasync def pending(value):\n    raise RuntimeError";
    check(
        extra,
        "async def f():\n    marker=1\n    await pending(0)\n    return marker",
        true,
    );
    check(
        extra,
        "async def f():\n    marker=1\n    pending(0)\n    return marker",
        false,
    );
}
