//! Source-prepared CFG regressions; all APIs exist on the baseline.
use rustpy_semantic::{db::Analysis, flow};
use std::path::Path;

fn reaching(source: &str, name: &str, expected_assignment_lines: &[usize]) {
    let db = Analysis::for_project(Path::new("/cfg-edges-fixture"));
    let id = db.set_file_text(Path::new("/cfg-edges-fixture/m.py"), source);
    let file = db.file(id).unwrap();
    let reference = file
        .scopes
        .references
        .iter()
        .rev()
        .find(|r| r.name == name && !r.is_write)
        .expect("actual final load");
    let info = flow::flow_of(&file, flow::flow_scope(&file, reference.scope).unwrap());
    let values = info
        .refs
        .get(&reference.range.start())
        .expect("reachable final load");
    let mut lines = Vec::new();
    for value in values {
        let binding = file.scopes.binding(
            value
                .binding
                .expect("local assignment, not outer authority"),
        );
        assert_eq!(binding.name.as_str(), name);
        lines.push(
            source[..usize::from(binding.range.start())]
                .matches('\n')
                .count()
                + 1,
        );
    }
    lines.sort_unstable();
    lines.dedup();
    assert_eq!(lines, expected_assignment_lines, "{source}");
}

fn unreachable(source: &str) {
    let db = Analysis::for_project(Path::new("/cfg-finally-fixture"));
    let id = db.set_file_text(Path::new("/cfg-finally-fixture/m.py"), source);
    let file = db.file(id).unwrap();
    let reference = file
        .scopes
        .references
        .iter()
        .rev()
        .find(|r| r.name == "x" && !r.is_write)
        .unwrap();
    let info = flow::flow_of(&file, flow::flow_scope(&file, reference.scope).unwrap());
    assert!(
        !info.refs.contains_key(&reference.range.start()),
        "unreachable tail must not be visited: {source}"
    );
}
#[test]
fn finally_raise_replaces_pending_break() {
    unreachable("def f():\n    x=1\n    while True:\n        try:\n            break\n        finally:\n            raise RuntimeError\n    return x\n");
}
#[test]
fn finally_continue_replaces_pending_break() {
    // Infinite loop: source/flow fixture only, never execute this function.
    unreachable("def f():\n    x=1\n    while True:\n        try:\n            break\n        finally:\n            continue\n    return x\n");
}
#[test]
fn finally_break_replaces_pending_return() {
    reaching("def f():\n    while True:\n        try:\n            return 0\n        finally:\n            x=2\n            break\n    return x\n", "x", &[6]);
}
#[test]
fn return_path_state_reaches_finally_read() {
    reaching(
        "def f():\n    try:\n        x=1\n        return 0\n    finally:\n        print(x)\n",
        "x",
        &[3],
    );
}
#[test]
fn nested_finally_writes_transform_outer_loop_break() {
    reaching("def f():\n    while True:\n        try:\n            try:\n                break\n            finally:\n                x=1\n        finally:\n            x=2\n    return x\n", "x", &[9]);
}
#[test]
fn handler_preserves_completed_prefix_but_not_post_raise_assignment() {
    reaching("def f():\n    try:\n        x=1\n        raise ValueError\n        x=2\n    except ValueError:\n        return x\n", "x", &[3]);
}
#[test]
fn nested_loop_break_keeps_its_own_target_before_outer_finally() {
    reaching("def f():\n    while True:\n        try:\n            for item in (1,):\n                break\n            x=1\n            break\n        finally:\n            x=2\n    return x\n", "x", &[9]);
}
