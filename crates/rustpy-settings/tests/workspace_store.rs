use rustpy_settings::{
    BookmarkState, Workspace, WorkspaceBreakpoints, WorkspaceError, WorkspacePatch,
    WorkspaceSession, WorkspaceStore,
};
use std::{collections::BTreeMap, path::Path};
fn codec(path: impl AsRef<Path>, value: Option<&str>) -> WorkspacePatch {
    WorkspacePatch {
        file_encodings: BTreeMap::from([(path.as_ref().into(), value.map(str::to_owned))]),
        ..Default::default()
    }
}

#[test]
fn missing_is_explicit_but_corrupt_and_unreadable_are_errors_and_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::new(dir.path());
    let missing = store.load().unwrap();
    assert_eq!(missing.revision(), None);
    std::fs::create_dir(dir.path().join(".rustpy")).unwrap();
    let path = Workspace::path_for(dir.path());
    std::fs::write(&path, b"{corrupt").unwrap();
    assert!(matches!(store.load(), Err(WorkspaceError::Invalid { .. })));
    assert!(store
        .commit(&missing, &codec("a.py", Some("utf-8")), None)
        .is_err());
    assert!(Workspace::default().save(dir.path()).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"{corrupt");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(store.load().is_err());
    assert!(Workspace::default().save(dir.path()).is_err());
    assert!(path.is_dir());
}
#[test]
fn two_stores_merge_disjoint_codecs_and_explicit_deletions_without_resurrection() {
    let dir = tempfile::tempdir().unwrap();
    let a = WorkspaceStore::new(dir.path());
    let b = WorkspaceStore::new(dir.path());
    let base_a = a.load().unwrap();
    let base_b = b.load().unwrap();
    a.commit(&base_a, &codec("a.py", Some("latin-1")), None)
        .unwrap();
    let merged = b
        .commit(&base_b, &codec("b.py", Some("future-codec")), None)
        .unwrap();
    assert_eq!(merged.workspace().file_encodings.len(), 2);
    let deleted = a.commit(&merged, &codec("a.py", None), None).unwrap();
    assert_eq!(
        deleted.workspace().file_encodings[0].encoding,
        "future-codec"
    );
    assert!(matches!(
        b.commit(&merged, &codec("a.py", Some("utf-8")), None),
        Err(WorkspaceError::Conflict(_))
    ));
    assert_eq!(a.load().unwrap().workspace(), deleted.workspace());
}
#[test]
fn disjoint_named_tasks_and_bookmarks_merge_but_same_identity_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::new(dir.path());
    let base = store.load().unwrap();
    let mut a = WorkspacePatch::default();
    a.startup_tasks.insert("serve".into(), true);
    a.bookmarks.insert(
        (dir.path().join("a.py"), 2),
        Some(BookmarkState {
            path: "ignored".into(),
            line: 99,
            description: "a".into(),
            ..Default::default()
        }),
    );
    store.commit(&base, &a, None).unwrap();
    let mut b = WorkspacePatch::default();
    b.startup_tasks.insert("test".into(), true);
    b.bookmarks.insert(
        (dir.path().join("b.py"), 3),
        Some(BookmarkState {
            description: "b".into(),
            ..Default::default()
        }),
    );
    let merged = store.commit(&base, &b, None).unwrap();
    assert_eq!(merged.workspace().startup_tasks, vec!["serve", "test"]);
    assert_eq!(merged.workspace().bookmarks.len(), 2);
    a.bookmarks
        .values_mut()
        .for_each(|v| v.as_mut().unwrap().description = "conflict".into());
    assert!(matches!(
        store.commit(&base, &a, None),
        Err(WorkspaceError::Conflict(_))
    ));
    let mut delete = WorkspacePatch::default();
    delete.startup_tasks.insert("serve".into(), false);
    delete.bookmarks.insert((dir.path().join("a.py"), 2), None);
    let result = store.commit(&merged, &delete, None).unwrap();
    assert_eq!(result.workspace().startup_tasks, vec!["test"]);
    assert_eq!(result.workspace().bookmarks.len(), 1);
}
#[test]
fn scalar_conflicts_and_existing_file_deletion_never_replace_disk() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::new(dir.path());
    let base = store.load().unwrap();
    let a = WorkspacePatch {
        interpreter: Some(Some(dir.path().join("first"))),
        ..Default::default()
    };
    let written = store.commit(&base, &a, None).unwrap();
    let b = WorkspacePatch {
        interpreter: Some(Some(dir.path().join("second"))),
        ..Default::default()
    };
    assert!(matches!(
        store.commit(&base, &b, None),
        Err(WorkspaceError::Conflict(_))
    ));
    std::fs::remove_file(Workspace::path_for(dir.path())).unwrap();
    assert!(matches!(
        store.commit(&written, &codec("a.py", Some("utf-8")), None),
        Err(WorkspaceError::Conflict(_))
    ));
    assert!(!Workspace::path_for(dir.path()).exists());
}
#[test]
fn whole_session_requires_its_own_live_lease_but_other_windows_can_write_codecs() {
    let dir = tempfile::tempdir().unwrap();
    let a = WorkspaceStore::new(dir.path());
    let b = WorkspaceStore::new(dir.path());
    let base = a.load().unwrap();
    let lease = a.try_acquire_session().unwrap().unwrap();
    assert!(b.try_acquire_session().unwrap().is_none());
    let session = WorkspaceSession::from_workspace(base.workspace());
    let patch = WorkspacePatch {
        session: Some(session),
        ..Default::default()
    };
    assert!(matches!(
        a.commit(&base, &patch, None),
        Err(WorkspaceError::SessionRequired)
    ));
    assert!(matches!(
        b.commit(&base, &patch, Some(&lease)),
        Err(WorkspaceError::WrongStore)
    ));
    b.commit(&base, &codec("a.py", Some("utf-8")), None)
        .unwrap();
    assert_eq!(
        a.commit(&base, &patch, Some(&lease))
            .unwrap()
            .workspace()
            .file_encodings
            .len(),
        1
    );
    drop(lease);
    assert!(b.try_acquire_session().unwrap().is_some());
}
#[test]
fn known_edits_preserve_unknown_top_level_nested_and_absolute_record_fields() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".rustpy")).unwrap();
    let path = Workspace::path_for(dir.path());
    let raw = serde_json::json!({"root":dir.path(),"future":{"a":1},"file_encodings":[{"path":dir.path().join("a.py"),"encoding":"future-codec","extra":"keep"}],"line_breakpoints":[{"path":dir.path().join("a.py"),"line":2,"future_condition":7}],"exception_breakpoints":{"classes":[{"name":"ValueError","future":"class"}]},"tool_windows":{"future_layout":"keep"}});
    std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();
    let store = WorkspaceStore::new(dir.path());
    let base = store.load().unwrap();
    let mut patch = codec("a.py", Some("utf-8"));
    let mut bp = WorkspaceBreakpoints::from_workspace(base.workspace());
    bp.breakpoints_muted = true;
    patch.breakpoints = Some(bp);
    let lease = store.try_acquire_session().unwrap().unwrap();
    let mut session = WorkspaceSession::from_workspace(base.workspace());
    session.tool_windows.project_width = 411.0;
    patch.session = Some(session);
    store.commit(&base, &patch, Some(&lease)).unwrap();
    let result: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(result["future"], raw["future"]);
    assert_eq!(result["file_encodings"][0]["extra"], "keep");
    assert_eq!(result["line_breakpoints"][0]["future_condition"], 7);
    assert_eq!(
        result["exception_breakpoints"]["classes"][0]["future"],
        "class"
    );
    assert_eq!(result["tool_windows"]["future_layout"], "keep");
}
#[test]
fn breakpoint_deletion_clears_plain_and_rich_representations_together() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::new(dir.path());
    let base = store.load().unwrap();
    let mut bp = WorkspaceBreakpoints::from_workspace(base.workspace());
    bp.breakpoints.push((dir.path().join("a.py"), vec![2]));
    bp.line_breakpoints
        .push(rustpy_settings::LineBreakpointState {
            path: dir.path().join("a.py"),
            line: 2,
            ..Default::default()
        });
    let state = store
        .commit(
            &base,
            &WorkspacePatch {
                breakpoints: Some(bp),
                ..Default::default()
            },
            None,
        )
        .unwrap();
    let empty = WorkspaceBreakpoints::from_workspace(&Workspace::default());
    let deleted = store
        .commit(
            &state,
            &WorkspacePatch {
                breakpoints: Some(empty),
                ..Default::default()
            },
            None,
        )
        .unwrap();
    assert!(deleted.workspace().breakpoints.is_empty());
    assert!(deleted.workspace().line_breakpoints.is_empty());
}
#[cfg(unix)]
#[test]
fn symlink_project_aliases_share_session_lock_and_special_workspace_files_refuse() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let alias = dir.path().join("alias");
    symlink(&project, &alias).unwrap();
    let a = WorkspaceStore::new(&project);
    let b = WorkspaceStore::new(&alias);
    let lease = a.try_acquire_session().unwrap().unwrap();
    assert!(b.try_acquire_session().unwrap().is_none());
    drop(lease);
    let target = dir.path().join("target");
    std::fs::write(&target, b"{}").unwrap();
    symlink(&target, Workspace::path_for(&project)).unwrap();
    assert!(a.load().is_err());
    assert!(Workspace::default().save(&project).is_err());
    assert_eq!(std::fs::read(target).unwrap(), b"{}");
}

#[test]
fn process_worker() {
    let Ok(root) = std::env::var("RUSTPY_WORKSPACE_PROCESS_ROOT") else {
        return;
    };
    let id = std::env::var("RUSTPY_WORKSPACE_PROCESS_ID").unwrap();
    let mode = std::env::var("RUSTPY_WORKSPACE_PROCESS_MODE").unwrap();
    let store = WorkspaceStore::new(&root);
    let baseline = store.load().unwrap();
    let lease = if mode == "session" {
        Some(store.try_acquire_session().unwrap().unwrap())
    } else {
        None
    };
    std::fs::write(Path::new(&root).join(format!("ready-{id}")), b"ready").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !Path::new(&root).join("go").exists() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let patch = if mode == "session" {
        WorkspacePatch {
            session: Some(WorkspaceSession::from_workspace(baseline.workspace())),
            ..Default::default()
        }
    } else {
        codec(
            if mode == "conflict" {
                "same.py".into()
            } else {
                format!("{id}.py")
            },
            Some(&id),
        )
    };
    match store.commit(&baseline, &patch, lease.as_ref()) {
        Ok(_) => println!("RESULT:OK pid={} id={id}", std::process::id()),
        Err(WorkspaceError::Conflict(_)) => {
            println!("RESULT:CONFLICT pid={} id={id}", std::process::id())
        }
        Err(e) => panic!("unexpected {e}"),
    }
}
fn spawn(root: &Path, id: &str, mode: &str) -> std::process::Child {
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "process_worker", "--nocapture"])
        .env("RUSTPY_WORKSPACE_PROCESS_ROOT", root)
        .env("RUSTPY_WORKSPACE_PROCESS_ID", id)
        .env("RUSTPY_WORKSPACE_PROCESS_MODE", mode)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdin.take());
    child
}
fn ready(root: &Path, ids: &[&str]) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !ids
        .iter()
        .all(|id| root.join(format!("ready-{id}")).exists())
    {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
#[test]
fn actual_processes_merge_disjoint_writes_and_report_same_key_conflict() {
    for mode in ["disjoint", "conflict"] {
        let dir = tempfile::tempdir().unwrap();
        let a = spawn(dir.path(), "first", mode);
        let b = spawn(dir.path(), "second", mode);
        ready(dir.path(), &["first", "second"]);
        std::fs::write(dir.path().join("go"), b"go").unwrap();
        let outputs = [a.wait_with_output().unwrap(), b.wait_with_output().unwrap()];
        for o in &outputs {
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        }
        for o in &outputs {
            println!("{}", String::from_utf8_lossy(&o.stdout));
        }
        let outcomes: Vec<_> = outputs
            .iter()
            .map(|o| String::from_utf8_lossy(&o.stdout))
            .collect();
        assert_eq!(
            outcomes.iter().filter(|o| o.contains("RESULT:OK")).count(),
            if mode == "disjoint" { 2 } else { 1 }
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|o| o.contains("RESULT:CONFLICT"))
                .count(),
            if mode == "disjoint" { 0 } else { 1 }
        );
        assert_eq!(
            WorkspaceStore::new(dir.path())
                .load()
                .unwrap()
                .workspace()
                .file_encodings
                .len(),
            if mode == "disjoint" { 2 } else { 1 }
        );
    }
}
#[test]
fn actual_process_session_lease_blocks_other_owner_not_independent_patch_and_releases() {
    let dir = tempfile::tempdir().unwrap();
    let owner = spawn(dir.path(), "owner", "session");
    ready(dir.path(), &["owner"]);
    let store = WorkspaceStore::new(dir.path());
    assert!(store.try_acquire_session().unwrap().is_none());
    let baseline = store.load().unwrap();
    store
        .commit(&baseline, &codec("outside.py", Some("utf-8")), None)
        .unwrap();
    std::fs::write(dir.path().join("go"), b"go").unwrap();
    let output = owner.wait_with_output().unwrap();
    println!(
        "session owner subprocess: {} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("RESULT:OK"));
    assert!(store.try_acquire_session().unwrap().is_some());
    assert_eq!(store.load().unwrap().workspace().file_encodings.len(), 1);
}

#[test]
fn invalid_output_format_is_prevalidated_and_does_not_publish_null_float() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::new(dir.path());
    let baseline = store.load().unwrap();
    let lease = store.try_acquire_session().unwrap().unwrap();
    let mut session = WorkspaceSession::from_workspace(baseline.workspace());
    session.tool_windows.project_width = f32::NAN;
    assert!(matches!(
        store.commit(
            &baseline,
            &WorkspacePatch {
                session: Some(session),
                ..Default::default()
            },
            Some(&lease)
        ),
        Err(WorkspaceError::Invalid { .. })
    ));
    assert!(!Workspace::path_for(dir.path()).exists());
}
#[cfg(unix)]
#[test]
fn replaced_lease_and_fifo_refuse_without_touching_workspace_or_blocking() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::new(dir.path());
    let base = store.load().unwrap();
    let lease = store.try_acquire_session().unwrap().unwrap();
    let lock = dir.path().join(".rustpy/workspace.session.lock");
    std::fs::rename(&lock, lock.with_extension("old")).unwrap();
    let other = WorkspaceStore::new(dir.path());
    let replacement = other.try_acquire_session().unwrap().unwrap();
    let patch = WorkspacePatch {
        session: Some(WorkspaceSession::from_workspace(base.workspace())),
        ..Default::default()
    };
    assert!(matches!(
        store.commit(&base, &patch, Some(&lease)),
        Err(WorkspaceError::SessionRequired)
    ));
    assert!(!Workspace::path_for(dir.path()).exists());
    drop(replacement);
    let path = Workspace::path_for(dir.path());
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: the fixture path is a valid, owned null-terminated string.
    assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
    assert!(matches!(store.load(), Err(WorkspaceError::Invalid { .. })));
}
#[cfg(unix)]
#[test]
fn atomic_replacement_preserves_permissions_and_refuses_explicit_readonly() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::new(dir.path());
    let baseline = store.load().unwrap();
    let first = store
        .commit(&baseline, &codec("a.py", Some("utf-8")), None)
        .unwrap();
    let path = Workspace::path_for(dir.path());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    let second = store
        .commit(&first, &codec("a.py", Some("latin-1")), None)
        .unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o640
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o440)).unwrap();
    let original = std::fs::read(&path).unwrap();
    assert!(store
        .commit(&second, &codec("a.py", Some("utf-8")), None)
        .is_err());
    assert_eq!(std::fs::read(path).unwrap(), original);
}
#[test]
fn unrelated_patch_preserves_copied_absolute_legacy_path_meanings() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("old");
    let copied = dir.path().join("copied");
    std::fs::create_dir_all(copied.join(".rustpy")).unwrap();
    let raw = serde_json::json!({"root":old,"active_file":old.join("a.py"),"open_files":[{"path":old.join("a.py"),"future":17}],"expanded_dirs":[old.join("pkg")],"recent_files":[old.join("a.py")],"breakpoints":[[old.join("a.py"),[3]]],"source_roots":[old.join("src")],"excluded_dirs":[old.join("out")],"interpreter":old.join(".venv/bin/python")});
    let path = Workspace::path_for(&copied);
    std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();
    let store = WorkspaceStore::new(&copied);
    let before = store.load().unwrap();
    let after = store
        .commit(&before, &codec("b.py", Some("utf-8")), None)
        .unwrap();
    let mut expected = before.workspace().clone();
    expected.file_encodings = after.workspace().file_encodings.clone();
    assert_eq!(after.workspace(), &expected);
    assert_eq!(store.load().unwrap().workspace(), &expected);
    let result: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(result["open_files"][0]["future"], 17);
    assert_eq!(result["active_file"], "a.py");
}

#[test]
fn exact_output_limit_is_loadable_and_oversized_patch_preserves_written_baseline() {
    const LIMIT: usize = 8 * 1024 * 1024;
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::new(dir.path());
    let missing = store.load().unwrap();
    let empty = store
        .commit(
            &missing,
            &WorkspacePatch {
                watches: Some(vec![String::new()]),
                ..Default::default()
            },
            None,
        )
        .unwrap();
    let path = Workspace::path_for(dir.path());
    let overhead = std::fs::read(&path).unwrap().len();
    let payload_len = LIMIT - overhead;
    let at_limit = WorkspacePatch {
        watches: Some(vec!["a".repeat(payload_len)]),
        ..Default::default()
    };
    let written = store.commit(&empty, &at_limit, None).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), LIMIT as u64);
    let reloaded = store.load().unwrap();
    assert_eq!(reloaded.revision(), written.revision());
    assert_eq!(reloaded.workspace().watches[0].len(), payload_len);
    let original = std::fs::read(&path).unwrap();
    let oversized = WorkspacePatch {
        watches: Some(vec!["b".repeat(payload_len + 1)]),
        ..Default::default()
    };
    assert!(matches!(
        store.commit(&written, &oversized, None),
        Err(WorkspaceError::Invalid { .. })
    ));
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(store.load().unwrap().revision(), written.revision());
    assert_eq!(written.workspace().watches[0].len(), payload_len);
    assert_eq!(
        oversized.watches.as_ref().unwrap()[0].len(),
        payload_len + 1
    );
}

#[test]
fn oversized_first_patch_cannot_create_an_unloadable_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::new(dir.path());
    let missing = store.load().unwrap();
    let patch = WorkspacePatch {
        watches: Some(vec!["a".repeat(8 * 1024 * 1024)]),
        ..Default::default()
    };
    assert!(matches!(
        store.commit(&missing, &patch, None),
        Err(WorkspaceError::Invalid { .. })
    ));
    assert!(!Workspace::path_for(dir.path()).exists());
    assert_eq!(store.load().unwrap().revision(), None);
    assert_eq!(missing.revision(), None);
    assert_eq!(patch.watches.as_ref().unwrap()[0].len(), 8 * 1024 * 1024);
}

#[test]
fn empty_bookmark_lists_persist_and_concurrent_list_changes_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let store = WorkspaceStore::new(dir.path());
    let base = store.load().unwrap();
    let saved = store
        .commit(
            &base,
            &WorkspacePatch {
                bookmark_lists: Some(vec!["Default".into(), "Empty review".into()]),
                ..Default::default()
            },
            None,
        )
        .unwrap();
    assert!(saved.workspace().bookmarks.is_empty());
    assert_eq!(
        store.load().unwrap().workspace().bookmark_lists,
        ["Default", "Empty review"]
    );
    let competing = WorkspaceStore::new(dir.path());
    let competing_base = competing.load().unwrap();
    let accepted = store
        .commit(
            &saved,
            &WorkspacePatch {
                bookmark_lists: Some(vec!["Default".into(), "Renamed".into()]),
                ..Default::default()
            },
            None,
        )
        .unwrap();
    assert!(matches!(competing.commit(&competing_base, &WorkspacePatch {
        bookmark_lists: Some(vec!["Default".into(), "Other owner".into()]),
        ..Default::default()
    }, None), Err(WorkspaceError::Conflict(field)) if field == "bookmark_lists"));
    assert_eq!(store.load().unwrap().workspace(), accepted.workspace());
    let mut compatible = accepted.workspace().clone();
    compatible.bookmark_lists.push("Still empty".into());
    compatible.save(dir.path()).unwrap();
    assert_eq!(
        Workspace::load(dir.path()).bookmark_lists,
        compatible.bookmark_lists
    );
}

#[test]
fn whole_file_and_line_bookmarks_coexist_and_membership_metadata_roundtrips() {
    use rustpy_settings::BookmarkKind;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("same.py");
    let store = WorkspaceStore::new(dir.path());
    let base = store.load().unwrap();
    let mut patch = WorkspacePatch {
        bookmark_lists: Some(vec!["Default".into(), "Review".into()]),
        ..Default::default()
    };
    patch.bookmarks.insert(
        (path.clone(), 0),
        Some(BookmarkState {
            path: path.clone(),
            line: 0,
            kind: BookmarkKind::File,
            list: "Review".into(),
            ..Default::default()
        }),
    );
    patch.bookmarks.insert(
        (path.clone(), 1),
        Some(BookmarkState {
            path: path.clone(),
            line: 1,
            kind: BookmarkKind::Line,
            list: "Review".into(),
            description: "π🙂".into(),
            ..Default::default()
        }),
    );
    let saved = store.commit(&base, &patch, None).unwrap();
    assert_eq!(saved.workspace().bookmarks.len(), 2);
    let mut changed = WorkspacePatch::default();
    let mut line = saved
        .workspace()
        .bookmarks
        .iter()
        .find(|b| b.line == 1)
        .unwrap()
        .clone();
    line.list = "Default".into();
    changed.bookmarks.insert((path.clone(), 1), Some(line));
    store.commit(&saved, &changed, None).unwrap();
    let loaded = Workspace::load(dir.path());
    assert_eq!(loaded.bookmarks.len(), 2);
    let file = loaded
        .bookmarks
        .iter()
        .find(|b| b.kind == BookmarkKind::File)
        .unwrap();
    assert_eq!((file.line, file.list.as_str()), (0, "Review"));
    let line = loaded
        .bookmarks
        .iter()
        .find(|b| b.kind == BookmarkKind::Line)
        .unwrap();
    assert_eq!(line.list, "Default");
    assert_eq!(line.description, "π🙂");
}
