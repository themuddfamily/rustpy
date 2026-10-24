use rustpy_semantic::{db::Analysis, imports};
use std::sync::Arc;

#[test]
fn immutable_candidate_uses_imported_exports_without_publishing_candidate() {
    let root = tempfile::TempDir::with_prefix("candidate-cross-exports").unwrap();
    std::fs::write(root.path().join("names.py"), "EXPORTS=['os']\n").unwrap();
    let db = Analysis::for_project(root.path());
    let id = db.set_file_text(&root.path().join("api.py"), "import json\n");
    let live = db.file(id).unwrap();
    let candidate = Arc::new(live.with_candidate_text(
        "from names import EXPORTS\nimport os\nimport json\n__all__=EXPORTS\n".into(),
    ));
    assert_eq!(imports::dunder_all_with(&db, &candidate), ["os"]);
    let unused = imports::unused_import_candidates_with(&db, &candidate);
    assert_eq!(
        unused.iter().map(|b| b.name.as_str()).collect::<Vec<_>>(),
        ["json"]
    );
    assert_eq!(db.file(id).unwrap().text.as_ref(), "import json\n");
    assert_eq!(live.path, candidate.path);
    assert_eq!(live.id, candidate.id);
    db.set_file_text(&root.path().join("names.py"), "EXPORTS=['json']\n");
    assert_eq!(imports::dunder_all_with(&db, &candidate), ["json"]);
    assert_eq!(
        imports::unused_import_candidates_with(&db, &candidate)
            .iter()
            .map(|b| b.name.as_str())
            .collect::<Vec<_>>(),
        ["os"]
    );
}

#[test]
fn unresolved_cycle_preserves_potentially_exported_imports() {
    let root = tempfile::TempDir::with_prefix("candidate-cross-cycle").unwrap();
    std::fs::write(
        root.path().join("names.py"),
        "from api import __all__ as EXPORTS\n",
    )
    .unwrap();
    let db = Analysis::for_project(root.path());
    let id = db.set_file_text(
        &root.path().join("api.py"),
        "from names import EXPORTS\nimport os\n__all__=EXPORTS\n",
    );
    let file = db.file(id).unwrap();
    let exports = imports::evaluated_exports(&db, &file).unwrap();
    assert!(!exports.exact);
    assert!(imports::unused_import_candidates_with(&db, &file).is_empty());
}
