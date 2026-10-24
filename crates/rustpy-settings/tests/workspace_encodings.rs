use std::path::Path;

use rustpy_settings::{FileEncodingOverride, OpenFileState, Workspace};

fn record(path: impl AsRef<Path>, encoding: &str) -> FileEncodingOverride {
    FileEncodingOverride {
        path: path.as_ref().to_path_buf(),
        encoding: encoding.into(),
    }
}

fn write_workspace(project: &Path, value: serde_json::Value) {
    let path = Workspace::path_for(project);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
}

#[test]
fn old_workspace_without_encoding_records_remains_compatible() {
    let fixture = tempfile::tempdir().unwrap();
    let project = fixture.path().join("project");
    write_workspace(
        &project,
        serde_json::json!({"open_files": [{"path": "main.py", "caret_line": 8}], "watches": ["value"]}),
    );
    let workspace = Workspace::load(&project);
    assert!(workspace.file_encodings.is_empty());
    assert_eq!(workspace.open_files[0].path, project.join("main.py"));
    assert_eq!(workspace.open_files[0].caret_line, 8);
    assert_eq!(workspace.watches, ["value"]);
    workspace.save(&project).unwrap();
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(Workspace::path_for(&project)).unwrap()).unwrap();
    assert!(saved.get("file_encodings").is_none());
}

#[test]
fn encoding_records_survive_closed_tabs_and_workspace_roundtrip() {
    let fixture = tempfile::tempdir().unwrap();
    let project = fixture.path().join("project");
    let path = project.join("data.py");
    let mut workspace = Workspace {
        open_files: vec![OpenFileState {
            path: path.clone(),
            ..Default::default()
        }],
        file_encodings: vec![record(&path, "latin-1")],
        ..Default::default()
    };
    workspace.open_files.clear();
    workspace.save(&project).unwrap();
    let loaded = Workspace::load(&project);
    assert!(loaded.open_files.is_empty());
    assert_eq!(loaded.file_encodings, workspace.file_encodings);
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(Workspace::path_for(&project)).unwrap()).unwrap();
    assert_eq!(saved["file_encodings"][0]["path"], "data.py");
    assert_eq!(saved["file_encodings"][0]["encoding"], "latin-1");
    let reopened = Workspace {
        open_files: vec![OpenFileState {
            path,
            ..Default::default()
        }],
        ..loaded
    };
    reopened.save(&project).unwrap();
    assert_eq!(
        Workspace::load(&project).file_encodings,
        workspace.file_encodings
    );
}

#[test]
fn copied_project_rebases_own_overrides_and_preserves_external_paths_and_unknown_keys() {
    let fixture = tempfile::tempdir().unwrap();
    let original = fixture.path().join("original");
    let copied = fixture.path().join("copied");
    let external = fixture.path().join("external/data.txt");
    let workspace = Workspace {
        file_encodings: vec![
            record(original.join("pkg/main.py"), "utf-8"),
            record(&external, "future-codec: Ω"),
        ],
        ..Default::default()
    };
    workspace.save(&original).unwrap();
    std::fs::create_dir_all(Workspace::path_for(&copied).parent().unwrap()).unwrap();
    std::fs::copy(Workspace::path_for(&original), Workspace::path_for(&copied)).unwrap();
    let loaded = Workspace::load(&copied);
    assert_eq!(
        loaded.file_encodings,
        vec![
            record(copied.join("pkg/main.py"), "utf-8"),
            record(&external, "future-codec: Ω")
        ]
    );
    assert!(loaded.open_files.is_empty());
    loaded.save(&copied).unwrap();
    assert_eq!(
        Workspace::load(&copied).file_encodings,
        loaded.file_encodings
    );
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(Workspace::path_for(&copied)).unwrap()).unwrap();
    assert_eq!(
        saved["file_encodings"][1]["path"],
        serde_json::json!(external)
    );
    assert_eq!(saved["file_encodings"][1]["encoding"], "future-codec: Ω");
}

#[test]
fn saved_old_root_absolute_aliases_follow_existing_workspace_path_policy() {
    let fixture = tempfile::tempdir().unwrap();
    let old = fixture.path().join("old");
    let moved = fixture.path().join("moved");
    let external = fixture.path().join("external.py");
    write_workspace(
        &moved,
        serde_json::json!({"root": old, "file_encodings": [record(old.join("legacy.py"), "utf-16le"), record("relative.py", "utf-16be"), record(&external, "unknown")]}),
    );
    assert_eq!(
        Workspace::load(&moved).file_encodings,
        vec![
            record(moved.join("legacy.py"), "utf-16le"),
            record(moved.join("relative.py"), "utf-16be"),
            record(external, "unknown")
        ]
    );
}

#[test]
fn legacy_rootless_absolute_records_use_the_same_filter_as_other_file_records() {
    let fixture = tempfile::tempdir().unwrap();
    let project = fixture.path().join("project");
    let other = fixture.path().join("other/main.py");
    write_workspace(
        &project,
        serde_json::json!({"file_encodings": [record(&other, "utf-8"), record(project.join("local.py"), "unsupported"), record("relative.py", "UTF8")]}),
    );
    assert_eq!(
        Workspace::load(&project).file_encodings,
        vec![
            record(project.join("local.py"), "unsupported"),
            record(project.join("relative.py"), "UTF8")
        ]
    );
}
