use rustpy_semantic::{db::Analysis, flow};
use std::path::Path;
fn check(source: &str) {
    let db = Analysis::for_project(Path::new("/declared-contract-precision"));
    let id = db.set_file_text(Path::new("/declared-contract-precision/m.py"), source);
    let file = db.file(id).unwrap();
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
    assert!(
        !info.refs.contains_key(&reference.range.start()),
        "canonical terminal callable contract stops actual assigned-local tail: {source}"
    );
}
#[test]
fn paramspec_callable() {
    check("from typing import Callable, NoReturn, ParamSpec\nP=ParamSpec(\"P\")\ndef f(stop:Callable[P,NoReturn]):\n    marker=1\n    stop()\n    return marker\n");
}
#[test]
fn callable_type_alias() {
    check("from typing import Callable, NoReturn\nStop=Callable[[],NoReturn]\ndef f(stop:Stop):\n    marker=1\n    stop()\n    return marker\n");
}
#[test]
fn quoted_callable() {
    check("from typing import Callable, NoReturn\ndef f(stop:\"Callable[[],NoReturn]\"):\n    marker=1\n    stop()\n    return marker\n");
}
