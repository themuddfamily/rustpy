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
#[test]
fn generic_class_body_reads_type_parameter_not_unbound_function_local() {
    unbound(
        "def f():\n    class C[T]:\n        seen=T\n    T=2\n    return C\n",
        "T",
        0,
    );
}
#[test]
fn generic_class_parameter_does_not_initialize_outer_function_cell() {
    unbound(
        "def f():\n    class C[T]:\n        seen=T\n    print(T)\n    T=2\n",
        "T",
        1,
    );
}
