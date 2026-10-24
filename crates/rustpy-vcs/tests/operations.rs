//! Real repository checks across synchronous mutations and every job owner.

use std::path::Path;
use std::time::Duration;

use rustpy_vcs::{GitEnv, GitJob, GitRepo, RepositoryOperation, RepositoryReservation};

fn repo(path: &Path) -> GitRepo {
    let repo = GitRepo::init(path).unwrap();
    std::fs::write(path.join("a.py"), "original\n").unwrap();
    repo.commit(&[path.join("a.py")], "initial", false).unwrap();
    repo
}

#[test]
fn reservations_block_other_owners_and_mutations_without_changing_files() {
    let dir = tempfile::tempdir().unwrap();
    let repo = repo(dir.path());
    std::fs::write(dir.path().join("a.py"), "edited\n").unwrap();
    let pending = RepositoryReservation::try_acquire(dir.path(), "Updating project").unwrap();
    assert_eq!(
        repo.running_operation().as_deref(),
        Some("Updating project")
    );
    let refused = |error: anyhow::Error| {
        assert!(
            error.to_string().contains("Updating project is running"),
            "{error}"
        )
    };
    refused(
        RepositoryReservation::try_acquire(dir.path(), "Fetch")
            .err()
            .unwrap(),
    );
    refused(
        GitJob::spawn(dir.path(), &["status".into()], &GitEnv::default())
            .err()
            .unwrap(),
    );
    refused(
        repo.commit(&[dir.path().join("a.py")], "overlap", false)
            .unwrap_err(),
    );
    refused(repo.stash_push(None, true).unwrap_err());
    refused(repo.rollback(&dir.path().join("a.py")).unwrap_err());
    refused(repo.create_branch("overlap", true).unwrap_err());
    refused(repo.add_remote("overlap", "/tmp/nowhere").unwrap_err());
    refused(
        RepositoryOperation::enter(&dir.path().join("a.py"), "Save")
            .err()
            .unwrap(),
    );
    refused(
        RepositoryOperation::enter(&dir.path().join("missing/nested/new.py"), "Create")
            .err()
            .unwrap(),
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.py")).unwrap(),
        "edited\n"
    );
    assert_eq!(
        repo.head_text(&dir.path().join("a.py")).unwrap(),
        "original\n"
    );
    assert!(repo.stash_list().unwrap().is_empty());
    assert_eq!(repo.branches().len(), 1);
    assert!(repo.remotes().is_empty());
    // Dispatch enters the reserved scope on its worker; nested mutations
    // belong to that operation, while another worker is still refused.
    let path = dir.path().to_path_buf();
    std::thread::spawn(move || {
        pending.run(|| {
            let repo = GitRepo::discover(&path).unwrap();
            repo.create_branch("owned", true).unwrap();
            repo.commit(&[path.join("a.py")], "owned", false).unwrap();
            assert!(RepositoryReservation::try_acquire(&path, "another worker").is_err());
        })
    })
    .join()
    .unwrap();
    assert!(rustpy_vcs::running_operation(dir.path()).is_none());
    repo.rollback(&dir.path().join("a.py")).unwrap();
}

#[test]
fn different_repositories_run_independently_and_panics_release_the_scope() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let a_repo = repo(a.path());
    let b_repo = repo(b.path());
    let pending = RepositoryReservation::try_acquire(a.path(), "A").unwrap();
    b_repo.create_branch("allowed", true).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pending.run(|| {
            a_repo.create_branch("nested", false).unwrap();
            panic!("worker panic");
        });
    }));
    assert!(result.is_err());
    // The entered thread scope was removed even while the reservation lives.
    assert!(a_repo.create_branch("not-owned-anymore", false).is_err());
    drop(pending);
    a_repo.create_branch("released", false).unwrap();
}

#[test]
fn linked_worktrees_and_nested_project_paths_share_the_reservation() {
    let a = tempfile::tempdir().unwrap();
    let repo = repo(a.path());
    std::fs::create_dir(a.path().join("pkg")).unwrap();
    let linked = tempfile::tempdir().unwrap();
    let worktree = linked.path().join("linked");
    repo.run_git(&[
        "worktree",
        "add",
        "-b",
        "linked",
        worktree.to_str().unwrap(),
    ])
    .unwrap();
    let operation =
        RepositoryReservation::try_acquire(&a.path().join("pkg"), "Main update").unwrap();
    assert!(RepositoryReservation::try_acquire(&worktree, "Linked fetch").is_err());
    assert!(GitRepo::discover(&worktree)
        .unwrap()
        .create_tag("blocked", None)
        .is_err());
    drop(operation);
    GitRepo::discover(&worktree)
        .unwrap()
        .create_tag("allowed", None)
        .unwrap();
}

#[cfg(unix)]
#[test]
fn configured_git_is_used_by_legacy_fetch_workflow_update_and_console() {
    // Isolate the process-wide executable setting from parallel test cases.
    const CHILD: &str = "RUSTPY_TEST_CONFIGURED_GIT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "configured_git_is_used_by_legacy_fetch_workflow_update_and_console",
            ])
            .env(CHILD, "1")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    let bare = dir.path().join("origin.git");
    let log = dir.path().join("commands");
    let wrapper = dir.path().join("configured-git");
    git2::Repository::init_bare(&bare).unwrap();
    std::fs::create_dir(&work).unwrap();
    let repo = repo(&work);
    repo.add_remote("origin", bare.to_str().unwrap()).unwrap();
    repo.run_git(&["push", "--set-upstream", "origin", "HEAD"])
        .unwrap();
    std::fs::write(
        &wrapper,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$RUSTPY_TEST_GIT_LOG\"\nexec git \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("RUSTPY_TEST_GIT_LOG", &log);
    rustpy_vcs::set_git_program(wrapper.to_str().unwrap());
    let args = ["fetch".into(), "--all".into(), "--prune".into()];
    GitRepo::run_git_cancellable(&work, &args, &rustpy_core::CancellationToken::new()).unwrap();
    repo.update(rustpy_vcs::UpdateMethod::Merge, true).unwrap();
    GitRepo::run_git_in(&work, &["status".into(), "--porcelain".into()]).unwrap();
    let mut job = GitJob::spawn(&work, &["status".into()], &GitEnv::default()).unwrap();
    assert!(job.wait(Duration::from_secs(10)) && job.succeeded());
    let log = std::fs::read_to_string(&log).unwrap();
    assert!(log.contains("fetch --all --prune\n"), "{log}");
    assert!(log.contains("fetch origin\n"), "{log}");
    assert!(log.contains("status --porcelain\n"), "{log}");
    assert!(log.ends_with("status\n"), "{log}");
}
