use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_semantic::db::Analysis;
use std::path::Path;
fn count(source: &str, name: &str) -> usize {
    let db = Analysis::for_project(Path::new("/empty-analysis"));
    let id = db.set_file_text(Path::new("/empty-analysis/m.py"), source);
    let file = db.file(id).unwrap();
    inspections::inspect(&db, &file, &InspectionOptions::default())
        .iter()
        .filter(|p| {
            p.diagnostic.code == "PyUnboundLocalVariable"
                && &source
                    [usize::from(p.diagnostic.range.start())..usize::from(p.diagnostic.range.end())]
                    == name
        })
        .count()
}
#[test]
fn empty_for_does_not_report_dead_body_but_target_stays_unbound() {
    for literal in ["[]", "()", "{}", "''", "b''"] {
        let source = format!(
            "def f():\n    for target in {literal}:\n        print(x)\n    x=1\n    return target\n"
        );
        assert_eq!(count(&source, "x"), 0, "{literal}");
        assert_eq!(count(&source, "target"), 1, "{literal}");
    }
}
#[test]
fn empty_for_else_is_live_and_nonempty_body_remains_live() {
    assert_eq!(
        count(
            "def f():\n    for i in []:\n        pass\n    else:\n        print(x)\n    x=1\n",
            "x"
        ),
        1
    );
    assert_eq!(
        count(
            "def f():\n    for i in [1]:\n        print(x)\n    x=1\n",
            "x"
        ),
        1
    );
}
