use rustpy_semantic::{db::Analysis, features::diagnostics};
use std::path::Path;
#[test]
fn unresolved_reference_has_stable_identity_severity_range_and_message() {
    let db = Analysis::for_project(Path::new("/quality-golden"));
    let id = db.set_file_text(
        Path::new("/quality-golden/main.py"),
        include_str!("../goldens/unresolved.py"),
    );
    let file = db.file(id).unwrap();
    let mut rows: Vec<_> = diagnostics(&db, &file)
        .into_iter()
        .filter(|d| d.code.as_str() == "PyUnresolvedReferences")
        .map(|d| {
            format!(
                "{}\t{:?}\t{}\t{}\t{}",
                d.code,
                d.severity,
                u32::from(d.range.start()),
                u32::from(d.range.end()),
                d.message
            )
        })
        .collect();
    rows.sort();
    assert_eq!(
        rows.join("\n") + "\n",
        include_str!("../goldens/unresolved.tsv")
    );
}
#[test]
fn resolved_reference_does_not_reuse_old_unresolved_diagnostic() {
    let db = Analysis::for_project(Path::new("/quality-golden"));
    let path = Path::new("/quality-golden/main.py");
    let id = db.set_file_text(path, include_str!("../goldens/unresolved.py"));
    assert!(diagnostics(&db, &db.file(id).unwrap())
        .iter()
        .any(|d| d.code.as_str() == "PyUnresolvedReferences"));
    db.set_file_text(path, "missing_name = 1\nvalue = missing_name\n");
    assert!(!diagnostics(&db, &db.file(id).unwrap())
        .iter()
        .any(|d| d.code.as_str() == "PyUnresolvedReferences"));
}
