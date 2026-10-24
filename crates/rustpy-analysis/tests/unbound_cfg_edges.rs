//! Real inspection consumers for the same finite CFG edge requirements.
use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::db::Analysis;
use std::path::Path;

fn unbound_x(source: &str, expected: usize) {
    let db = Analysis::for_project(Path::new("/cfg-edges-analysis"));
    let id = db.set_file_text(Path::new("/cfg-edges-analysis/m.py"), source);
    let file = db.file(id).unwrap();
    let problems = inspections::inspect(&db, &file, &InspectionOptions::default());
    let found: Vec<_> = problems
        .iter()
        .filter(|p| {
            p.diagnostic.code == "PyUnboundLocalVariable"
                && &source
                    [usize::from(p.diagnostic.range.start())..usize::from(p.diagnostic.range.end())]
                    == "x"
        })
        .collect();
    assert_eq!(found.len(), expected, "{source}\n{problems:#?}");
}

#[test]
fn constant_true_elif_definitely_assigns_local() {
    unbound_x(
        "def f():\n    if False:\n        pass\n    elif True:\n        x=1\n    return x\n",
        0,
    );
}

#[test]
fn false_while_body_does_not_report_impossible_local_read() {
    unbound_x(
        "def f():\n    while False:\n        print(x)\n    x=1\n    return x\n",
        0,
    );
}

#[test]
fn finally_assignment_is_definite_after_break() {
    unbound_x("def f():\n    while True:\n        try:\n            break\n        finally:\n            x=1\n    return x\n", 0);
}

#[test]
fn irrefutable_as_pattern_is_definite_but_guarded_pattern_is_not() {
    unbound_x("def f(value):\n    match value:\n        case _ as whole:\n            x=1\n    return x\n", 0);
    unbound_x("def f(value, flag):\n    match value:\n        case _ as whole if flag:\n            x=1\n    return x\n", 1);
}

#[test]
fn explicit_raise_keeps_handler_read_unbound() {
    unbound_x("def f():\n    try:\n        raise ValueError\n        x=1\n    except ValueError:\n        return x\n", 1);
}
