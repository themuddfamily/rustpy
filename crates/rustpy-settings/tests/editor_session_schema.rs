use rustpy_settings::{
    DetachedEditorState, EditorGroupMember, EditorLayout, EditorSplit, LogicalScreenBounds,
    OpenFileState, Workspace, WorkspaceError, WorkspacePatch, WorkspaceSession, WorkspaceStore,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
fn open(path: PathBuf) -> OpenFileState {
    OpenFileState {
        path,
        ..Default::default()
    }
}
fn bounds() -> LogicalScreenBounds {
    LogicalScreenBounds {
        x: -1920.0,
        y: 20.0,
        width: 900.0,
        height: 640.0,
    }
}
fn session(root: &Path, split: EditorSplit) -> WorkspaceSession {
    let a = root.join("a.py");
    let b = root.join("b.py");
    WorkspaceSession::from_workspace(&Workspace {
        open_files: vec![open(a.clone()), open(b.clone())],
        active_file: Some(b.clone()),
        editor_layout: Some(EditorLayout {
            instances: Vec::new(),
            active_group: 0,
            detached_group: 1,
            groups: vec![
                EditorGroupMember {
                    path: a.clone(),
                    group: 0,
                },
                EditorGroupMember {
                    path: b.clone(),
                    group: 1,
                },
            ],
            split: Some(split),
            split_fraction: 0.35,
            group_active: [Some(a), Some(b.clone())],
            detached: Some(DetachedEditorState {
                path: b,
                bounds: Some(bounds()),
            }),
        }),
        ..Default::default()
    })
}
fn write(root: &Path, raw: serde_json::Value) {
    std::fs::create_dir_all(root.join(".rustpy")).unwrap();
    std::fs::write(Workspace::path_for(root), serde_json::to_vec(&raw).unwrap()).unwrap();
}

#[test]
fn checked_store_roundtrips_both_split_orientations_and_logical_detached_bounds() {
    for split in [EditorSplit::SideBySide, EditorSplit::Stacked] {
        let dir = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::new(dir.path());
        let baseline = store.load().unwrap();
        let lease = store.try_acquire_session().unwrap().unwrap();
        let desired = session(dir.path(), split);
        let snapshot = store
            .commit(
                &baseline,
                &WorkspacePatch {
                    session: Some(desired.clone()),
                    ..Default::default()
                },
                Some(&lease),
            )
            .unwrap();
        assert_eq!(
            WorkspaceSession::from_workspace(snapshot.workspace()),
            desired
        );
        assert_eq!(
            WorkspaceSession::from_workspace(store.load().unwrap().workspace()),
            desired
        );
        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(Workspace::path_for(dir.path())).unwrap())
                .unwrap();
        assert_eq!(raw["editor_layout"]["groups"][0]["path"], "a.py");
        assert_eq!(raw["editor_layout"]["detached"]["path"], "b.py");
        assert_eq!(raw["editor_layout"]["detached"]["bounds"]["x"], -1920.0);
    }
}
#[test]
fn legacy_flat_session_stays_optional_and_keeps_existing_open_file_fields() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        serde_json::json!({"open_files":[{"path":"a.py","caret_line":5,"caret_column":7,"scroll_line":12,"pinned":true}],"active_file":"a.py"}),
    );
    let store = WorkspaceStore::new(dir.path());
    let base = store.load().unwrap();
    assert!(base.workspace().editor_layout.is_none());
    assert_eq!(base.workspace().open_files[0].caret_column, 7);
    assert!(base.workspace().open_files[0].pinned);
    let patch = WorkspacePatch {
        file_encodings: BTreeMap::from([(dir.path().join("a.py"), Some("future-codec".into()))]),
        ..Default::default()
    };
    let next = store.commit(&base, &patch, None).unwrap();
    assert_eq!(next.workspace().open_files, base.workspace().open_files);
    assert!(next.workspace().editor_layout.is_none());
    let raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(Workspace::path_for(dir.path())).unwrap()).unwrap();
    assert!(raw.get("editor_layout").is_none());
}
#[test]
fn malformed_layout_fields_normalize_narrowly_without_losing_valid_files() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        serde_json::json!({"open_files":[{"path":"a.py"},{"path":"b.py"},{"path":"c.py"}],"active_file":"c.py","editor_layout":{"split":"SideBySide","split_fraction":"bad","groups":[{"path":"b.py","group":1},{"path":"a.py","group":-8},{"path":"b.py","group":0},{"path":"ghost.py","group":1},{"path":42,"group":1}],"group_active":["missing.py","a.py", "c.py"],"detached":{"path":"b.py","bounds":{"x":false,"y":0,"width":800,"height":600}}}}),
    );
    let ws = WorkspaceStore::new(dir.path()).load().unwrap();
    let ws = ws.workspace();
    assert_eq!(ws.open_files.len(), 3);
    assert_eq!(ws.active_file, Some(dir.path().join("c.py")));
    let layout = ws.editor_layout.as_ref().unwrap();
    assert_eq!(layout.split, Some(EditorSplit::SideBySide));
    assert_eq!(layout.split_fraction, 0.5);
    assert_eq!(
        layout
            .groups
            .iter()
            .map(|m| (&m.path, m.group))
            .collect::<Vec<_>>(),
        vec![
            (&dir.path().join("a.py"), 0),
            (&dir.path().join("b.py"), 1),
            (&dir.path().join("c.py"), 0)
        ]
    );
    assert_eq!(
        layout.group_active,
        [Some(dir.path().join("c.py")), Some(dir.path().join("b.py"))]
    );
    // Legacy JSON omits instance fields; its detached file still belongs to group 1.
    assert!(layout.instances.is_empty());
    assert_eq!(layout.detached_group, 1);
    assert!(layout.detached.as_ref().unwrap().bounds.is_none());
}
#[test]
fn unpopulated_split_flattens_and_valid_single_file_detached_reference_survives() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        serde_json::json!({"open_files":[{"path":"only.py"}],"active_file":"missing.py","editor_layout":{"split":"Stacked","groups":[{"path":"only.py","group":1}],"group_active":[null,"missing.py"],"split_fraction":99,"detached":{"path":"only.py","bounds":{"x":0,"y":0,"width":-1,"height":600}}}}),
    );
    let snapshot = WorkspaceStore::new(dir.path()).load().unwrap();
    let ws = snapshot.workspace();
    let layout = ws.editor_layout.as_ref().unwrap();
    assert_eq!(layout.split, None);
    assert_eq!(layout.split_fraction, 0.5);
    assert_eq!(layout.groups[0].group, 0);
    assert_eq!(
        layout.group_active,
        [Some(dir.path().join("only.py")), None]
    );
    assert_eq!(ws.active_file, Some(dir.path().join("only.py")));
    assert_eq!(
        layout.detached.as_ref().unwrap().path,
        dir.path().join("only.py")
    );
    assert!(layout.detached.as_ref().unwrap().bounds.is_none());
}
#[test]
fn copied_project_and_external_layout_paths_follow_existing_path_policy() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("old");
    let copied = dir.path().join("copied");
    let outside = dir.path().join("outside.py");
    write(
        &copied,
        serde_json::json!({"root":old,"open_files":[{"path":old.join("a.py")},{"path":outside}],"active_file":outside,"editor_layout":{"split":"Stacked","groups":[{"path":old.join("a.py"),"group":0},{"path":outside,"group":1}],"group_active":[old.join("a.py"),outside],"detached":{"path":outside,"bounds":{"x":-400,"y":0,"width":800,"height":600}}}}),
    );
    let store = WorkspaceStore::new(&copied);
    let before = store.load().unwrap();
    let layout = before.workspace().editor_layout.as_ref().unwrap();
    assert_eq!(layout.groups[0].path, copied.join("a.py"));
    assert_eq!(layout.groups[1].path, outside);
    assert_eq!(layout.detached.as_ref().unwrap().path, outside);
    let after = store
        .commit(
            &before,
            &WorkspacePatch {
                file_encodings: BTreeMap::from([(copied.join("a.py"), Some("utf-8".into()))]),
                ..Default::default()
            },
            None,
        )
        .unwrap();
    assert_eq!(
        after.workspace().editor_layout,
        before.workspace().editor_layout
    );
    assert_eq!(
        store.load().unwrap().workspace().editor_layout,
        before.workspace().editor_layout
    );
}
#[test]
fn rootless_legacy_layout_drops_other_copy_paths_and_keeps_local_members() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("here");
    let other = dir.path().join("other.py");
    write(
        &root,
        serde_json::json!({"open_files":[{"path":other},{"path":"a.py"}],"editor_layout":{"groups":[{"path":other,"group":1},{"path":"a.py","group":0}],"split":"SideBySide","group_active":["a.py",other],"detached":{"path":other}}}),
    );
    let snapshot = WorkspaceStore::new(&root).load().unwrap();
    let ws = snapshot.workspace();
    assert_eq!(ws.open_files.len(), 1);
    let layout = ws.editor_layout.as_ref().unwrap();
    assert_eq!(layout.groups.len(), 1);
    assert_eq!(layout.groups[0].path, root.join("a.py"));
    assert_eq!(layout.split, None);
    assert!(layout.detached.is_none());
}
#[test]
fn nested_unknown_layout_fields_survive_path_rebasing_and_explicit_session_change() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("old");
    let root = dir.path().join("copy");
    write(
        &root,
        serde_json::json!({"root":old,"open_files":[{"path":old.join("a.py")},{"path":old.join("b.py")}],"editor_layout":{"future_layout":{"key":17},"split":"SideBySide","groups":[{"path":old.join("a.py"),"group":0,"future_group":"a"},{"path":old.join("b.py"),"group":1,"future_group":"b"}],"group_active":[old.join("a.py"),old.join("b.py")],"detached":{"path":old.join("b.py"),"future_detached":"keep","bounds":{"x":0,"y":0,"width":800,"height":600,"future_bounds":99}}}}),
    );
    let store = WorkspaceStore::new(&root);
    let baseline = store.load().unwrap();
    let lease = store.try_acquire_session().unwrap().unwrap();
    let mut desired = WorkspaceSession::from_workspace(baseline.workspace());
    desired.editor_layout.as_mut().unwrap().split = Some(EditorSplit::Stacked);
    store
        .commit(
            &baseline,
            &WorkspacePatch {
                session: Some(desired),
                ..Default::default()
            },
            Some(&lease),
        )
        .unwrap();
    let raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(Workspace::path_for(&root)).unwrap()).unwrap();
    let layout = &raw["editor_layout"];
    assert_eq!(layout["future_layout"]["key"], 17);
    assert_eq!(layout["groups"][0]["future_group"], "a");
    assert_eq!(layout["groups"][1]["future_group"], "b");
    assert_eq!(layout["groups"][0]["path"], "a.py");
    assert_eq!(layout["detached"]["path"], "b.py");
    assert_eq!(layout["detached"]["future_detached"], "keep");
    assert_eq!(layout["detached"]["bounds"]["future_bounds"], 99);
}
#[test]
fn layout_is_one_leased_session_domain_and_stale_orientation_change_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::new(dir.path());
    let other = WorkspaceStore::new(dir.path());
    let missing = store.load().unwrap();
    let lease = store.try_acquire_session().unwrap().unwrap();
    let desired = session(dir.path(), EditorSplit::SideBySide);
    let patch = WorkspacePatch {
        session: Some(desired.clone()),
        ..Default::default()
    };
    assert!(matches!(
        store.commit(&missing, &patch, None),
        Err(WorkspaceError::SessionRequired)
    ));
    assert!(matches!(
        other.commit(&missing, &patch, Some(&lease)),
        Err(WorkspaceError::WrongStore)
    ));
    let first = store.commit(&missing, &patch, Some(&lease)).unwrap();
    let mut changed = desired.clone();
    changed.editor_layout.as_mut().unwrap().split = Some(EditorSplit::Stacked);
    let next = store
        .commit(
            &first,
            &WorkspacePatch {
                session: Some(changed),
                ..Default::default()
            },
            Some(&lease),
        )
        .unwrap();
    let mut stale = desired;
    stale.editor_layout.as_mut().unwrap().split_fraction = 0.6;
    assert!(matches!(
        store.commit(
            &first,
            &WorkspacePatch {
                session: Some(stale),
                ..Default::default()
            },
            Some(&lease)
        ),
        Err(WorkspaceError::Conflict(_))
    ));
    let codec = WorkspacePatch {
        file_encodings: BTreeMap::from([(dir.path().join("a.py"), Some("utf-8".into()))]),
        ..Default::default()
    };
    let merged = other.commit(&first, &codec, None).unwrap();
    assert_eq!(
        merged.workspace().editor_layout,
        next.workspace().editor_layout
    );
}
#[test]
fn invalid_layout_type_is_optional_but_invalid_whole_workspace_still_errors() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        serde_json::json!({"open_files":[{"path":"a.py"}],"editor_layout":"wrong type"}),
    );
    let store = WorkspaceStore::new(dir.path());
    let baseline = store.load().unwrap();
    assert_eq!(baseline.workspace().open_files.len(), 1);
    assert!(baseline.workspace().editor_layout.is_none());
    std::fs::write(Workspace::path_for(dir.path()), b"{malformed").unwrap();
    assert!(matches!(store.load(), Err(WorkspaceError::Invalid { .. })));
}
#[test]
fn manual_nonfinite_geometry_normalizes_before_serialization_and_clear_removes_layout() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::new(dir.path());
    let missing = store.load().unwrap();
    let lease = store.try_acquire_session().unwrap().unwrap();
    let mut desired = session(dir.path(), EditorSplit::Stacked);
    let layout = desired.editor_layout.as_mut().unwrap();
    layout.split_fraction = f32::INFINITY;
    layout
        .detached
        .as_mut()
        .unwrap()
        .bounds
        .as_mut()
        .unwrap()
        .width = f32::NAN;
    let saved = store
        .commit(
            &missing,
            &WorkspacePatch {
                session: Some(desired),
                ..Default::default()
            },
            Some(&lease),
        )
        .unwrap();
    let layout = saved.workspace().editor_layout.as_ref().unwrap();
    assert_eq!(layout.split_fraction, 0.5);
    assert!(layout.detached.as_ref().unwrap().bounds.is_none());
    let mut clear = WorkspaceSession::from_workspace(saved.workspace());
    clear.editor_layout = None;
    let cleared = store
        .commit(
            &saved,
            &WorkspacePatch {
                session: Some(clear),
                ..Default::default()
            },
            Some(&lease),
        )
        .unwrap();
    assert!(cleared.workspace().editor_layout.is_none());
    assert_eq!(cleared.workspace().open_files.len(), 2);
    let raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(Workspace::path_for(dir.path())).unwrap()).unwrap();
    assert!(raw.get("editor_layout").is_none());
}

#[test]
fn same_file_instances_retain_split_positions_and_accept_legacy_layout() {
    let path = std::path::PathBuf::from("/owned/shared.py");
    let mut first = open(path.clone());
    first.caret_line = 1;
    let mut second = first.clone();
    second.caret_line = 12;
    second.scroll_line = 9;
    let mut layout = EditorLayout {
        instances: vec![
            rustpy_settings::EditorInstanceState {
                file: first.clone(),
                group: 0,
            },
            rustpy_settings::EditorInstanceState {
                file: second,
                group: 1,
            },
        ],
        active_group: 1,
        split: Some(EditorSplit::SideBySide),
        group_active: [Some(path.clone()), Some(path)],
        ..Default::default()
    };
    layout.normalize(&[first], None);
    assert_eq!(layout.instances.len(), 2);
    assert_eq!(layout.instances[1].file.caret_line, 12);
    assert_eq!(layout.instances[1].file.scroll_line, 9);
    assert_eq!(layout.split, Some(EditorSplit::SideBySide));
    assert_eq!(layout.active_group, 1);
    let encoded = serde_json::to_vec(&layout).unwrap();
    let restored: EditorLayout = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(restored, layout);
    let legacy: EditorLayout = serde_json::from_str(r#"{"groups":[],"split":null}"#).unwrap();
    assert!(legacy.instances.is_empty());
    assert_eq!(legacy.active_group, 0);
}

#[test]
fn checked_store_relocates_shared_instances_and_preserves_nested_future_fields() {
    let directory = tempfile::tempdir().unwrap();
    let original = directory.path().join("original project");
    let copied = directory.path().join("copied project");
    let file = original.join("π shared.py");
    write(
        &original,
        serde_json::json!({
            "root":original,
            "open_files":[{"path":file,"caret_line":2,"scroll_line":1}],
            "active_file":file,
            "editor_layout":{
                "split":"SideBySide","active_group":1,"detached_group":1,
                "groups":[{"path":file,"group":0}],
                "group_active":[file,file],
                "detached":{"path":file},
                "instances":[
                    {"group":0,"future_instance":"left","file":{"path":file,"caret_line":2,"caret_column":3,"scroll_line":1,"future_file":{"side":"left"}}},
                    {"group":1,"future_instance":"right","file":{"path":file,"caret_line":12,"caret_column":7,"scroll_line":9,"future_file":{"side":"right"}}}
                ]
            }
        }),
    );
    let store = WorkspaceStore::new(&original);
    let baseline = store.load().unwrap();
    let lease = store.try_acquire_session().unwrap().unwrap();
    let mut session = WorkspaceSession::from_workspace(baseline.workspace());
    session.editor_layout.as_mut().unwrap().split_fraction = 0.4;
    store
        .commit(
            &baseline,
            &WorkspacePatch {
                session: Some(session),
                ..Default::default()
            },
            Some(&lease),
        )
        .unwrap();
    let bytes = std::fs::read(Workspace::path_for(&original)).unwrap();
    let raw: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    for (index, side) in ["left", "right"].iter().enumerate() {
        let instance = &raw["editor_layout"]["instances"][index];
        assert_eq!(instance["file"]["path"], "π shared.py");
        assert_eq!(instance["future_instance"], *side);
        assert_eq!(instance["file"]["future_file"]["side"], *side);
    }
    std::fs::create_dir_all(copied.join(".rustpy")).unwrap();
    std::fs::write(Workspace::path_for(&copied), bytes).unwrap();
    let copied_store = WorkspaceStore::new(&copied);
    let restored = copied_store.load().unwrap();
    let layout = restored.workspace().editor_layout.as_ref().unwrap();
    assert_eq!(layout.instances.len(), 2);
    for (index, (line, column, scroll)) in [(2, 3, 1), (12, 7, 9)].into_iter().enumerate() {
        let instance = &layout.instances[index];
        assert_eq!(instance.group, index as u8);
        assert_eq!(instance.file.path, copied.join("π shared.py"));
        assert_eq!(instance.file.caret_line, line);
        assert_eq!(instance.file.caret_column, column);
        assert_eq!(instance.file.scroll_line, scroll);
    }
    assert_eq!(layout.active_group, 1);
    assert_eq!(layout.detached_group, 1);
    assert_eq!(
        layout.group_active,
        [
            Some(copied.join("π shared.py")),
            Some(copied.join("π shared.py"))
        ]
    );
    assert_eq!(
        layout.detached.as_ref().unwrap().path,
        copied.join("π shared.py")
    );
    let copied_lease = copied_store.try_acquire_session().unwrap().unwrap();
    let mut session = WorkspaceSession::from_workspace(restored.workspace());
    session.editor_layout.as_mut().unwrap().instances[1]
        .file
        .caret_line = 13;
    copied_store
        .commit(
            &restored,
            &WorkspacePatch {
                session: Some(session),
                ..Default::default()
            },
            Some(&copied_lease),
        )
        .unwrap();
    let raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(Workspace::path_for(&copied)).unwrap()).unwrap();
    for (index, side) in ["left", "right"].iter().enumerate() {
        let instance = &raw["editor_layout"]["instances"][index];
        assert_eq!(instance["file"]["path"], "π shared.py");
        assert_eq!(instance["future_instance"], *side);
        assert_eq!(instance["file"]["future_file"]["side"], *side);
    }
    assert_eq!(
        copied_store
            .load()
            .unwrap()
            .workspace()
            .editor_layout
            .as_ref()
            .unwrap()
            .instances[1]
            .file
            .caret_line,
        13
    );
}
