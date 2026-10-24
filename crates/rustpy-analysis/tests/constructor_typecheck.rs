//! Actual type-check consumer must use constructor dispatch, not init-first selection.
use rustpy_analysis::inspections::{inspect, InspectionOptions};
use rustpy_semantic::db::Analysis;
use std::path::Path;
#[test]
fn foreign_new_skips_init_and_checks_return_assignment() {
    let source = "class Result: pass\nclass Factory:\n    def __new__(cls, value: int) -> Result: return Result()\n    def __init__(self, forbidden: str, required: str) -> None: pass\ngood: Result = Factory(1)\nbad: str = Factory(1)\n";
    let db = Analysis::for_project(Path::new("/constructor-fixture"));
    let id = db.set_file_text(Path::new("/constructor-fixture/main.py"), source);
    let file = db.file(id).unwrap();
    let problems = inspect(&db, &file, &InspectionOptions::default());
    assert!(
        !problems
            .iter()
            .any(|p| p.diagnostic.code == "PyArgumentList"),
        "{problems:?}"
    );
    let types: Vec<_> = problems
        .iter()
        .filter(|p| p.diagnostic.code == "PyTypeChecker")
        .collect();
    assert_eq!(types.len(), 1, "{types:?}");
    assert!(types[0].diagnostic.message.contains("str"), "{types:?}");
}
