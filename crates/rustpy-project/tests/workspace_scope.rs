use rustpy_project::files::{project_files_in, project_files_in_with_cancel};
use rustpy_project::{ProjectModel, ProjectScope, WorkspaceSnapshot};

fn write(path: &std::path::Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn saved_models_define_ordered_workspace_imports_and_scopes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("app");
    let shared = dir.path().join("shared");
    let other = dir.path().join("other");
    for path in [
        root.join("src/a.py"),
        root.join("tests/b.py"),
        root.join("vendor/c.py"),
        shared.join("d.py"),
        other.join("e.py"),
    ] {
        write(&path, "");
    }
    ProjectModel {
        content_roots: vec![shared.clone()],
        source_roots: vec![root.join("src")],
        test_roots: vec![root.join("tests")],
        excluded: vec![root.join("vendor")],
        ..Default::default()
    }
    .save(&root)
    .unwrap();
    let model =
        WorkspaceSnapshot::try_load(vec![root.clone(), other.clone(), root.clone()]).unwrap();
    assert_eq!(model.folders, vec![root.clone(), other.clone()]);
    assert_eq!(
        model.roots(),
        vec![
            root.clone(),
            other.clone(),
            shared.clone(),
            root.join("src"),
            root.join("tests")
        ]
    );
    let mut expected = vec![
        root.join("src/a.py"),
        root.join("tests/b.py"),
        shared.join("d.py"),
        other.join("e.py"),
    ];
    expected.sort();
    assert_eq!(project_files_in(&model.scope()), expected);
    assert!(model.scope().contains(&shared.join("d.py")));
    assert!(!model.scope().contains(&root.join("vendor/c.py")));
}

#[test]
fn legacy_copy_migrates_without_writing_and_shared_errors_do_not_fall_back() {
    let original = tempfile::tempdir().unwrap();
    let copy = tempfile::tempdir().unwrap();
    let ws = rustpy_settings::Workspace {
        source_roots: vec![original.path().join("src")],
        excluded_dirs: vec![original.path().join("vendor")],
        ..Default::default()
    };
    ws.save(original.path()).unwrap();
    std::fs::create_dir(copy.path().join(".rustpy")).unwrap();
    std::fs::copy(
        rustpy_settings::Workspace::path_for(original.path()),
        rustpy_settings::Workspace::path_for(copy.path()),
    )
    .unwrap();
    let model = WorkspaceSnapshot::try_load(vec![copy.path().into()]).unwrap();
    assert_eq!(model.source_roots, vec![copy.path().join("src")]);
    assert_eq!(model.excluded, vec![copy.path().join("vendor")]);
    assert!(!ProjectModel::exists(copy.path()));
    write(
        &ProjectModel::path_for(copy.path()),
        "<<<<<<< unresolved merge",
    );
    let error = WorkspaceSnapshot::try_load(vec![copy.path().into()]).unwrap_err();
    assert!(
        error.contains("project.json") && error.contains("not valid"),
        "{error}"
    );
    let failed = WorkspaceSnapshot::unavailable(vec![copy.path().into()]);
    assert_eq!(failed.primary(), Some(copy.path()));
    assert!(failed.roots().is_empty());
    assert!(project_files_in(&failed.scope()).is_empty());
}

#[test]
fn walks_respect_ignore_exclusions_overlap_and_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("app");
    let shared = dir.path().join("shared");
    write(&root.join(".git/config"), "");
    write(&root.join(".gitignore"), "ignored.py\n");
    write(&root.join("ignored.py"), "");
    write(&root.join("src/a.py"), "");
    write(&root.join("vendor/a.py"), "");
    write(&root.join("vendorx/a.py"), "");
    write(&root.join(".venv/a.py"), "");
    write(&shared.join("b.py"), "");
    let scope = ProjectScope::new(&root)
        .with_content_roots([root.join("src"), shared.clone()])
        .with_excluded([root.join("vendor")]);
    let mut expected = vec![
        root.join("src/a.py"),
        root.join("vendorx/a.py"),
        shared.join("b.py"),
    ];
    expected.sort();
    assert_eq!(project_files_in(&scope), expected);
    assert!(project_files_in_with_cancel(&scope, &|| true).is_empty());
    let seen = std::cell::Cell::new(0);
    let partial = project_files_in_with_cancel(&scope, &|| {
        seen.set(seen.get() + 1);
        seen.get() > 3
    });
    assert!(seen.get() > 3);
    assert!(partial.len() < expected.len());
    assert!(partial.iter().all(|p| expected.contains(p)));
}

#[cfg(unix)]
#[test]
fn workspace_folder_identity_deduplicates_symlink_aliases_and_overlap() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("app");
    write(&root.join("nested/a.py"), "");
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&root, &alias).unwrap();
    let snapshot =
        WorkspaceSnapshot::try_load(vec![alias, root.clone(), root.join("nested")]).unwrap();
    assert_eq!(snapshot.folders, vec![root.clone(), root.join("nested")]);
    assert_eq!(snapshot.scope().walk_roots(), vec![root.clone()]);
    assert_eq!(
        project_files_in(&snapshot.scope()),
        vec![root.join("nested/a.py")]
    );
}
