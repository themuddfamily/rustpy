//! Project-owned persistence reads files written before subsystem extraction.

use rustpy_project::{ProjectModel, RootKind, RunConfigKind, RunConfigStore};
use rustpy_settings::Workspace;

#[test]
fn copied_legacy_project_keeps_structure_and_run_configuration_formats() {
    let original = tempfile::tempdir().unwrap();
    let copy = tempfile::tempdir().unwrap();
    let old_root = original.path();
    let new_root = copy.path();
    std::fs::create_dir(new_root.join(".rustpy")).unwrap();
    // This is the pre-project.json workspace shape, including the previous root
    // used by Workspace to relocate absolute paths when a project is copied.
    let legacy = serde_json::json!({
        "root": old_root,
        "source_roots": [old_root.join("src")],
        "excluded_dirs": [old_root.join("vendor")]
    });
    std::fs::write(Workspace::path_for(new_root), legacy.to_string()).unwrap();
    let structure = ProjectModel::try_load(new_root).unwrap();
    assert_eq!(
        structure.kind_of(&new_root.join("src")),
        Some(RootKind::Sources)
    );
    assert_eq!(structure.excluded, vec![new_root.join("vendor")]);
    assert!(!ProjectModel::exists(new_root));
    structure.save(new_root).unwrap();
    let shared: serde_json::Value =
        serde_json::from_slice(&std::fs::read(ProjectModel::path_for(new_root)).unwrap()).unwrap();
    assert_eq!(shared["source_roots"], serde_json::json!(["src"]));
    assert_eq!(shared["excluded"], serde_json::json!(["vendor"]));

    let run_json = r#"{"configs":[{"name":"legacy tests","kind":{"type":"pytest","target":"tests","keywords":"fast"},"parameters":"-x","env":{"MODE":"test"}}],"selected":"legacy tests"}"#;
    // Exercise the actual pre-directory format, then verify canonical migration.
    let legacy_runs = new_root.join(".rustpy/runConfigurations.json");
    std::fs::write(&legacy_runs, run_json).unwrap();
    assert!(!RunConfigStore::path_for(new_root).exists());
    let store = RunConfigStore::load(new_root);
    let cfg = store.selected_config().unwrap();
    assert_eq!(
        cfg.kind,
        RunConfigKind::Pytest {
            target: "tests".into(),
            keywords: "fast".into()
        }
    );
    assert!(cfg.env_files.is_empty());
    assert!(cfg.add_content_roots && cfg.add_source_roots);
    store.save(new_root).unwrap();
    assert!(RunConfigStore::path_for(new_root).is_file());
    assert_eq!(RunConfigStore::load(new_root), store);
}
