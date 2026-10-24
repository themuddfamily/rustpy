use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::db::Analysis;

#[test]
fn actual_inspection_uses_imported_exports_and_retains_suppression_policy() {
    let root = tempfile::TempDir::with_prefix("inspection-cross-exports").unwrap();
    std::fs::write(root.path().join("names.py"), "EXPORTS=['os']\n").unwrap();
    let db = Analysis::for_project(root.path());
    let source = "from names import EXPORTS\nimport os\nimport json\nimport sys  # noqa: F401\n__all__=EXPORTS\n";
    let id = db.set_file_text(&root.path().join("api.py"), source);
    let file = db.file(id).unwrap();
    let problems = inspections::inspect(&db, &file, &InspectionOptions::default());
    let unused: Vec<_> = problems
        .iter()
        .filter(|p| p.diagnostic.code == "PyUnusedImports")
        .collect();
    assert_eq!(unused.len(), 1);
    assert_eq!(
        &source[usize::from(unused[0].diagnostic.range.start())
            ..usize::from(unused[0].diagnostic.range.end())],
        "json"
    );
    assert!(unused[0]
        .fixes
        .iter()
        .any(|fix| fix.title == "Optimize imports"));
    db.set_file_text(&root.path().join("names.py"), "EXPORTS=['json']\n");
    let problems = inspections::inspect(&db, &file, &InspectionOptions::default());
    let unused: Vec<_> = problems
        .iter()
        .filter(|p| p.diagnostic.code == "PyUnusedImports")
        .collect();
    assert_eq!(unused.len(), 1);
    assert_eq!(
        &source[usize::from(unused[0].diagnostic.range.start())
            ..usize::from(unused[0].diagnostic.range.end())],
        "os"
    );
    assert_eq!(db.file(id).unwrap().text.as_ref(), source);
}
