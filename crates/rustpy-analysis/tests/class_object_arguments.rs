//! Class objects passed to `type[X]` parameters follow the numeric tower.
use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::db::Analysis;
use std::path::Path;

fn type_warnings(source: &str) -> Vec<String> {
    let db = Analysis::for_project(Path::new("/class-object-arguments"));
    let id = db.set_file_text(Path::new("/class-object-arguments/m.py"), source);
    let file = db.file(id).unwrap();
    assert!(file.parse.errors().is_empty(), "{source}");
    inspections::inspect(&db, &file, &InspectionOptions::default())
        .into_iter()
        .filter(|problem| problem.diagnostic.code == "PyTypeChecker")
        .map(|problem| problem.diagnostic.message)
        .collect()
}

#[test]
fn numeric_classes_are_promoted_for_type_parameters() {
    let source = "def real(kind: type[float]) -> None: ...\ndef cplx(kind: type[complex]) -> None: ...\nwhole = int\nflag = bool\nfloating = float\nreal(whole)\nreal(flag)\nreal(floating)\ncplx(whole)\ncplx(floating)\ncplx(flag)\n";
    assert_eq!(type_warnings(source), Vec::<String>::new());
}

#[test]
fn unrelated_classes_are_still_rejected_for_type_parameters() {
    let source = "def real(kind: type[float]) -> None: ...\ndef whole(kind: type[int]) -> None: ...\ntext = str\nfloating = float\nreal(text)\nwhole(floating)\n";
    let problems = type_warnings(source);
    assert_eq!(problems.len(), 2, "{problems:#?}");
}
