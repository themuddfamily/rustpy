//! Actual inspection consumers; these Python functions are inspected, not run.
use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::db::Analysis;
use std::path::Path;

fn unbound(source: &str, name: &str, expected: usize) {
    let db = Analysis::for_project(Path::new("/analysis-expression-completions"));
    let id = db.set_file_text(Path::new("/analysis-expression-completions/m.py"), source);
    let file = db.file(id).unwrap();
    let problems = inspections::inspect(&db, &file, &InspectionOptions::default());
    let actual = problems
        .iter()
        .filter(|problem| {
            problem.diagnostic.code == "PyUnboundLocalVariable"
                && &source[usize::from(problem.diagnostic.range.start())
                    ..usize::from(problem.diagnostic.range.end())]
                    == name
        })
        .count();
    assert_eq!(actual, expected, "{source}\n{problems:#?}");
}
fn check(body: &str, reachable: bool) {
    let source = format!(
        "from typing import NoReturn\ndef die() -> NoReturn:\n    raise RuntimeError\ndef f(flag, sink):\n{body}    print(marker)\n    marker=1\n"
    );
    unbound(&source, "marker", usize::from(reachable));
}
#[test]
fn eager_class_body_never_call_stops_enclosing_tail() {
    check("    class C:\n        die()\n", false);
}
#[test]
fn eager_class_body_raise_stops_enclosing_tail() {
    check("    class C:\n        raise RuntimeError\n", false);
}
#[test]
fn class_local_store_does_not_bind_enclosing_function_local() {
    check("    class C:\n        marker=1\n", true);
}
#[test]
fn class_global_store_does_not_bind_enclosing_function_local() {
    check(
        "    class C:\n        global marker\n        marker=1\n",
        true,
    );
}
#[test]
fn class_nonlocal_store_binds_enclosing_function_cell() {
    check(
        "    class C:\n        nonlocal marker\n        marker=1\n",
        false,
    );
}
#[test]
fn normal_class_body_preserves_enclosing_local_assignment() {
    check("    marker=1\n    class C:\n        field=2\n", false);
}
#[test]
fn normal_class_body_without_store_retains_unbound_tail() {
    check("    class C:\n        pass\n", true);
}
#[test]
fn class_nonlocal_delete_retires_enclosing_function_cell() {
    check(
        "    marker=1\n    class C:\n        nonlocal marker\n        del marker\n",
        true,
    );
}
