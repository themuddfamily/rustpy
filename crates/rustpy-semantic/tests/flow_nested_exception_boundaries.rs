//! Nested handler boundaries: real public semantic flow consumer.
use rustpy_semantic::{db::Analysis, flow};
use std::path::Path;

fn outer_handler_unreachable(source: &str) {
    let db = Analysis::for_project(Path::new("/cfg-nested-handler"));
    let id = db.set_file_text(Path::new("/cfg-nested-handler/m.py"), source);
    let file = db.file(id).unwrap();
    let reference = file
        .scopes
        .references
        .iter()
        .find(|r| r.name == "outer_marker" && !r.is_write)
        .expect("outer handler has an actual load");
    let info = flow::flow_of(&file, flow::flow_scope(&file, reference.scope).unwrap());
    assert!(
        !info.refs.contains_key(&reference.range.start()),
        "outer handler cannot receive an exception consumed by its inner try: {source}"
    );
    let final_reference = file
        .scopes
        .references
        .iter()
        .rev()
        .find(|r| r.name == "x" && !r.is_write)
        .expect("reachable final x load");
    let values = info
        .refs
        .get(&final_reference.range.start())
        .expect("normal completion retained");
    assert_eq!(
        values.len(),
        1,
        "only actual completed assignment reaches tail"
    );
    assert!(
        values[0].binding.is_some(),
        "no synthetic preassignment exception path"
    );
}

#[test]
fn safe_inner_try_finally_does_not_create_outer_exception() {
    outer_handler_unreachable("def f():\n    outer_marker=0\n    try:\n        try:\n            pass\n        finally:\n            x=1\n    except:\n        outer_marker\n    return x\n");
}

#[test]
fn inner_bare_handler_consumes_explicit_raise_before_outer_handler() {
    outer_handler_unreachable("def f():\n    outer_marker=0\n    try:\n        try:\n            raise ValueError\n        except:\n            x=1\n    except:\n        outer_marker\n    return x\n");
}
