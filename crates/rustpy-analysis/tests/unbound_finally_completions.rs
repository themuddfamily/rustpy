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
fn nested_finally_assignment_is_definite_after_break() {
    unbound_x("def f():\n    while True:\n        try:\n            try:\n                break\n            finally:\n                x=1\n        finally:\n            x=2\n    return x\n", 0);
}
#[test]
fn finally_raise_cancels_break_and_its_unbound_tail() {
    unbound_x("def f():\n    while True:\n        try:\n            break\n        finally:\n            raise RuntimeError\n    return x\n    x=1\n", 0);
}
#[test]
fn finally_break_overrides_return_and_retains_assignment() {
    unbound_x("def f():\n    while True:\n        try:\n            return 0\n        finally:\n            x=2\n            break\n    return x\n", 0);
}
#[test]
fn assigned_return_path_is_available_in_finally() {
    unbound_x(
        "def f():\n    try:\n        x=1\n        return 0\n    finally:\n        print(x)\n",
        0,
    );
}
#[test]
fn inner_loop_break_is_consumed_before_outer_finally() {
    unbound_x("def f():\n    while True:\n        try:\n            for item in (1,):\n                break\n            x=1\n            break\n        finally:\n            x=2\n    return x\n", 0);
}
