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
fn builtin_numeric_unary_cannot_enter_pre_assignment_handler() {
    for value in ["-1", "+1", "~1", "-1.0", "-1j", "-(+1)"] {
        let source = format!(
            "def f(value):\n    try:\n        marker={value}\n    except Exception:\n        print(marker)\n"
        );
        unbound(&source, "marker", 0);
    }
}
#[test]
fn user_unary_and_invalid_numeric_operator_keep_handler_path() {
    for value in ["-value", "~1.0", "~1j", "-None", "-'text'"] {
        let source = format!(
            "def f(value):\n    try:\n        marker={value}\n    except Exception:\n        print(marker)\n"
        );
        unbound(&source, "marker", 1);
    }
}
