//! Finite row220 terminal-completion gaps; inspected source is never executed.
use rustpy_semantic::{db::Analysis, flow};
use std::path::Path;
fn terminal(source: &str) {
    let db = Analysis::for_project(Path::new("/finite-terminal-gaps"));
    let id = db.set_file_text(Path::new("/finite-terminal-gaps/m.py"), source);
    let file = db.file(id).unwrap();
    let reference = file
        .scopes
        .references
        .iter()
        .rev()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    let scope = flow::flow_scope(&file, reference.scope).unwrap();
    let info = flow::flow_of_with_db(&db, &file, scope);
    assert!(
        !info.refs.contains_key(&reference.range.start()),
        "terminal call must prevent actual assigned-local tail read: {source}"
    );
}
#[test]
fn class_builtin_alias_in_quiet_finally() {
    terminal("def f():\n    marker=1\n    try:\n        pass\n    finally:\n        class C:\n            stop=exit\n            exit=lambda:None\n            stop()\n    return marker\n");
}
#[test]
fn callable_noreturn_parameter() {
    terminal("from typing import Callable, NoReturn\ndef f(stop: Callable[[], NoReturn]):\n    marker=1\n    stop()\n    return marker\n");
}
#[test]
fn constructor_new_noreturn() {
    terminal("from typing import NoReturn\nclass C:\n    def __new__(cls)->NoReturn:\n        raise RuntimeError\ndef f():\n    marker=1\n    C()\n    return marker\n");
}
#[test]
fn callable_instance_noreturn() {
    terminal("from typing import NoReturn\nclass C:\n    def __call__(self)->NoReturn:\n        raise RuntimeError\ndef f():\n    marker=1\n    stop=C()\n    stop()\n    return marker\n");
}
