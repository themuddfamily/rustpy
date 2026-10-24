//! `x or default` drops the falsy `None` arm, so consumers see no Optional.
use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::db::Analysis;
use std::path::Path;

fn messages(source: &str) -> Vec<String> {
    let db = Analysis::for_project(Path::new("/bool-op-truthiness"));
    let id = db.set_file_text(Path::new("/bool-op-truthiness/m.py"), source);
    let file = db.file(id).unwrap();
    inspections::inspect(&db, &file, &InspectionOptions::default())
        .into_iter()
        .map(|problem| {
            format!(
                "{}: {}",
                problem.diagnostic.code, problem.diagnostic.message
            )
        })
        .collect()
}

#[test]
fn or_default_is_not_reported_as_possibly_none() {
    let source = "def f(x: str | None) -> str:\n    y: str = x or 'd'\n    (x or 'd').upper()\n    return (x or 'd').upper() + y\n";
    let problems = messages(source);
    assert!(
        !problems.iter().any(|message| message.contains("None")),
        "{problems:#?}"
    );
}

#[test]
fn and_keeps_the_none_arm_for_consumers() {
    let source = "def h(s: str) -> str: return s\ndef f(x: str | None) -> str:\n    y: str = x and 'd'\n    h(x and 'd')\n    (x and 'd').upper()\n    return x and 'd'\n";
    let problems = messages(source);
    assert!(
        problems
            .iter()
            .any(|message| message.contains("Possible None value has no attribute 'upper'")),
        "{problems:#?}"
    );
}
