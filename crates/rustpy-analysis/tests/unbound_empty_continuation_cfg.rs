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
fn continued_empty_text_and_bytes_for_body_are_dead() {
    let source =
        "def f():\n    for target in '\\\r\n':\n        print(x)\n    x=1\n    return target\n";
    assert_eq!(count(source, "x"), 0);
    assert_eq!(count(source, "target"), 1);
    let source =
        "def f():\n    for target in b'\\\r\n':\n        print(x)\n    x=1\n    return target\n";
    assert_eq!(count(source, "x"), 0);
    assert_eq!(count(source, "target"), 1);
}
#[test]
fn continued_empty_text_and_bytes_conditions_are_false() {
    assert_eq!(
        count(
            "def f():\n    if '\\\r\n':\n        print(x)\n    x=1\n",
            "x"
        ),
        0
    );
    assert_eq!(
        count(
            "def f():\n    if b'\\\r\n':\n        print(x)\n    x=1\n",
            "x"
        ),
        0
    );
}
