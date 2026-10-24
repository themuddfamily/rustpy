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

#[test]
fn constant_elif_excludes_impossible_else_assignment() {
    reaching("def f():\n    if False:\n        pass\n    elif True:\n        x=1\n    else:\n        x=2\n    return x\n", "x", &[5]);
    reaching("def f():\n    x=1\n    if False:\n        pass\n    elif False:\n        x=2\n    return x\n", "x", &[2]);
}

#[test]
fn false_while_break_cannot_contribute_assignment() {
    reaching(
        "def f():\n    x=1\n    while False:\n        x=2\n        break\n    return x\n",
        "x",
        &[2],
    );
}

#[test]
fn finally_assignment_transforms_pending_break_edge() {
    reaching("def f():\n    while True:\n        try:\n            x=1\n            break\n        finally:\n            x=2\n    return x\n", "x", &[7]);
}

#[test]
fn irrefutable_as_pattern_has_no_unassigned_exit() {
    reaching("def f(value):\n    match value:\n        case _ as whole:\n            x=1\n    return x\n", "x", &[4]);
}

#[test]
fn explicit_raise_does_not_expose_later_assignment_to_handler() {
    reaching("def f():\n    try:\n        raise ValueError\n        x=1\n    except ValueError:\n        return x\n", "x", &[]);
}
