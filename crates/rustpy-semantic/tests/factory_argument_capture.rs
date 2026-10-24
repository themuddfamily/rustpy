//! Runtime acquisition order controls; no post-argument callable lookup is valid.
use rustpy_semantic::{
    db::Analysis,
    flow::{flow_of_with_db, flow_scope},
};
fn check(source: &str, reachable: bool) {
    let directory = tempfile::TempDir::with_prefix("rustpy-factory-acquisition").unwrap();
    let path = directory.path().join("main.py");
    std::fs::write(&path, source).unwrap();
    let db = Analysis::for_project(directory.path());
    let _ = db.module("typing");
    let snapshot = db
        .snapshot(
            std::slice::from_ref(&path),
            &rustpy_core::CancellationToken::new(),
        )
        .unwrap();
    let db = snapshot.analysis();
    let file = db.file_for_path(&path).unwrap();
    let reference = file
        .scopes
        .references
        .iter()
        .find(|r| r.name == "marker" && !r.is_write)
        .unwrap();
    let flow = flow_of_with_db(db, &file, flow_scope(&file, reference.scope).unwrap());
    assert_eq!(
        flow.refs.contains_key(&reference.range.start()),
        reachable,
        "{source}"
    );
}
const HEAD: &str = "from typing import Callable,NoReturn\ndef stop()->NoReturn: raise RuntimeError\ndef terminal_factory(ignored:object)->Callable[[],NoReturn]: return stop\ndef returning_factory(ignored:object)->Callable[[],int]: return lambda:1\n";
#[test]
fn terminal_factory_is_acquired_before_argument_rebinding() {
    check(
        &format!(
            "{HEAD}def f():\n    factory=terminal_factory\n    marker=1\n    factory((factory:=returning_factory))()\n    marker\n"
        ),
        false,
    );
}
#[test]
fn returning_factory_cannot_be_replaced_by_terminal_argument_binding() {
    check(
        &format!(
            "{HEAD}def f():\n    factory=returning_factory\n    marker=1\n    factory((factory:=terminal_factory))()\n    marker\n"
        ),
        true,
    );
}

const OUTER_HEAD: &str = "from typing import Callable,NoReturn\ndef stop(ignored:object)->NoReturn: raise RuntimeError\ndef terminal_factory()->Callable[[object],NoReturn]: return stop\ndef returning_factory()->Callable[[object],int]: return lambda ignored:1\n";
#[test]
fn terminal_returned_callable_is_acquired_before_outer_argument_rebinding() {
    check(
        &format!(
            "{OUTER_HEAD}def f():\n    factory=terminal_factory\n    marker=1\n    factory()((factory:=returning_factory))\n    marker\n"
        ),
        false,
    );
}
#[test]
fn returning_returned_callable_does_not_gain_terminal_outer_argument_binding() {
    check(
        &format!(
            "{OUTER_HEAD}def f():\n    factory=returning_factory\n    marker=1\n    factory()((factory:=terminal_factory))\n    marker\n"
        ),
        true,
    );
}
