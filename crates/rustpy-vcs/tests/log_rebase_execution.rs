//! Actual public interactive-log workflows against owned local repositories.
//! No remote, credential, provider account or shell script fixture is involved.
use rustpy_vcs::{log_workspace::RebaseAction, GitRepo};
use std::{
    path::Path,
    time::{Duration, Instant},
};

fn fixture() -> (tempfile::TempDir, git2::Repository, GitRepo) {
    let temp = tempfile::tempdir().unwrap();
    // Both quoting characters matter to RustPy's generated sequence editor.
    let root = temp.path().join("log's owned repository");
    std::fs::create_dir(&root).unwrap();
    let native = git2::Repository::init(&root).unwrap();
    let mut config = native.config().unwrap();
    config.set_str("user.name", "Owned Log Fixture").unwrap();
    config.set_str("user.email", "log@example.test").unwrap();
    config.set_bool("commit.gpgsign", false).unwrap();
    config.set_bool("core.autocrlf", false).unwrap();
    let hooks = temp.path().join("empty-hooks");
    std::fs::create_dir(&hooks).unwrap();
    config
        .set_str("core.hooksPath", hooks.to_str().unwrap())
        .unwrap();
    let repo = GitRepo::discover(&root).unwrap();
    (temp, native, repo)
}

fn commit(native: &git2::Repository, file: &str, text: &str, message: &str) -> String {
    std::fs::write(native.workdir().unwrap().join(file), text).unwrap();
    let mut index = native.index().unwrap();
    index.add_path(Path::new(file)).unwrap();
    index.write().unwrap();
    let tree = native.find_tree(index.write_tree().unwrap()).unwrap();
    let parent = native
        .head()
        .ok()
        .and_then(|head| head.peel_to_commit().ok());
    let parents: Vec<_> = parent.iter().collect();
    let signature = git2::Signature::now("Owned Log Fixture", "log@example.test").unwrap();
    native
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parents,
        )
        .unwrap()
        .to_string()
}

fn finish(mut job: rustpy_vcs::GitJob) -> (bool, Vec<String>) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while job.is_running() {
        job.poll();
        assert!(
            Instant::now() < deadline,
            "owned Git workflow did not retire: {:?}",
            job.lines
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        job.exit_code().is_some(),
        "retirement includes output/reader drain"
    );
    (job.succeeded(), job.lines.clone())
}

fn history(native: &git2::Repository) -> Vec<(String, String)> {
    let mut walk = native.revwalk().unwrap();
    walk.push_head().unwrap();
    walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::REVERSE)
        .unwrap();
    walk.map(|id| {
        let commit = native.find_commit(id.unwrap()).unwrap();
        (
            commit.id().to_string(),
            commit.message().unwrap().to_string(),
        )
    })
    .collect()
}

#[test]
fn actual_log_reword_squash_fixup_drop_and_reorder_preserve_intended_tree() {
    let (_temp, native, repo) = fixture();
    let base = commit(&native, "base.txt", "base\n", "Base");
    let first = commit(&native, "a.txt", "alpha\n", "Original A");
    commit(&native, "b.txt", "beta\n", "Squash B");
    commit(
        &native,
        "c.txt",
        "gamma\n",
        "Fixup C message must disappear",
    );
    commit(&native, "drop.txt", "discard\n", "Drop D");
    commit(&native, "e.txt", "epsilon\n", "E before reorder");
    commit(&native, "f.txt", "phi\n", "F before reorder");
    let mut plan = repo.log_rebase_plan(&first).unwrap();
    assert_eq!(plan.base, base);
    assert!(!plan.preserve_merges);
    assert_eq!(plan.entries.len(), 6);
    plan.entries[0].action = RebaseAction::Reword;
    plan.entries[0].message = "Reworded A\n\nUnicode λ retained".into();
    plan.entries[1].action = RebaseAction::Squash;
    plan.entries[2].action = RebaseAction::Fixup;
    plan.entries[3].action = RebaseAction::Drop;
    plan.entries.swap(4, 5);
    let (success, output) = finish(repo.start_log_rebase(&plan).unwrap());
    assert!(success, "actual interactive rebase failed: {output:?}");
    let rewritten = history(&native);
    assert_eq!(rewritten.len(), 4);
    assert_eq!(rewritten[0].0, base);
    assert!(rewritten[1].1.starts_with("Reworded A\n"));
    assert!(rewritten[1].1.contains("Unicode λ retained"));
    assert!(rewritten[1].1.contains("Squash B"));
    assert!(!rewritten[1].1.contains("Fixup C"));
    assert_eq!(rewritten[2].1.trim(), "F before reorder");
    assert_eq!(rewritten[3].1.trim(), "E before reorder");
    let combined = native
        .find_commit(git2::Oid::from_str(&rewritten[1].0).unwrap())
        .unwrap();
    let tree = combined.tree().unwrap();
    for file in ["a.txt", "b.txt", "c.txt"] {
        assert!(tree.get_path(Path::new(file)).is_ok());
    }
    for file in ["drop.txt", "e.txt", "f.txt"] {
        assert!(tree.get_path(Path::new(file)).is_err());
    }
    let root = native.workdir().unwrap();
    for (file, text) in [
        ("a.txt", "alpha\n"),
        ("b.txt", "beta\n"),
        ("c.txt", "gamma\n"),
        ("e.txt", "epsilon\n"),
        ("f.txt", "phi\n"),
    ] {
        assert_eq!(std::fs::read_to_string(root.join(file)).unwrap(), text);
    }
    assert!(!root.join("drop.txt").exists());
    assert!(repo.status().unwrap().is_empty());
    assert_eq!(native.state(), git2::RepositoryState::Clean);
}

#[test]
fn actual_log_rewrite_refuses_stale_dirty_and_invalid_plans_without_mutation() {
    let (_temp, native, repo) = fixture();
    commit(&native, "base.txt", "base\n", "Base");
    let first = commit(&native, "a.txt", "alpha\n", "A");
    let stale = repo.log_rebase_plan(&first).unwrap();
    commit(&native, "b.txt", "beta\n", "B");
    let before = history(&native);
    let assert_refused = |plan: &rustpy_vcs::log_workspace::RebasePlan, fragment: &str| {
        let error = match repo.start_log_rebase(plan) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("invalid plan started Git"),
        };
        assert!(error.contains(fragment), "unexpected guard: {error}");
        assert_eq!(history(&native), before);
        assert_eq!(native.state(), git2::RepositoryState::Clean);
    };
    assert_refused(&stale, "HEAD changed");
    let mut plan = repo.log_rebase_plan(&first).unwrap();
    std::fs::write(
        native.workdir().unwrap().join("a.txt"),
        "unsaved owned work\n",
    )
    .unwrap();
    assert_refused(&plan, "Commit or stash");
    assert_eq!(
        std::fs::read_to_string(native.workdir().unwrap().join("a.txt")).unwrap(),
        "unsaved owned work\n"
    );
    std::fs::write(native.workdir().unwrap().join("a.txt"), "alpha\n").unwrap();
    plan.entries[0].action = RebaseAction::Squash;
    assert_refused(&plan, "preceding commit");
    plan.entries[0].action = RebaseAction::Pick;
    plan.entries.pop();
    assert_refused(&plan, "every original commit");
    assert!(repo.status().unwrap().is_empty());
}

#[test]
fn actual_conflicting_reorder_retains_original_branch_and_public_abort_restores_files() {
    let (_temp, native, repo) = fixture();
    commit(&native, "value.txt", "base\n", "Base");
    let first = commit(&native, "value.txt", "first\n", "First");
    let original_head = commit(&native, "value.txt", "second\n", "Second");
    let branch = native.head().unwrap().name().unwrap().to_string();
    let original_history = history(&native);
    let mut plan = repo.log_rebase_plan(&first).unwrap();
    plan.entries.swap(0, 1);
    let (success, output) = finish(repo.start_log_rebase(&plan).unwrap());
    assert!(!success, "dependent reorder must conflict: {output:?}");
    assert_ne!(
        native.state(),
        git2::RepositoryState::Clean,
        "failed rewrite must retain real rebase state: {output:?}"
    );
    // Seeding used this repository's cached libgit2 index. GitJob executes Git
    // externally, so refresh that cache before inspecting its on-disk stages.
    let mut conflicted_index = native.index().unwrap();
    conflicted_index.read(true).unwrap();
    assert!(
        conflicted_index.has_conflicts(),
        "dependent reorder must publish real index conflicts: {output:?}"
    );
    let conflicts = conflicted_index
        .conflicts()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        conflicts.iter().any(|conflict| {
            conflict
                .ancestor
                .as_ref()
                .is_some_and(|entry| entry.path == b"value.txt")
                && conflict
                    .our
                    .as_ref()
                    .is_some_and(|entry| entry.path == b"value.txt")
                && conflict
                    .their
                    .as_ref()
                    .is_some_and(|entry| entry.path == b"value.txt")
        }),
        "expected three-stage value.txt conflict: {output:?}"
    );
    let conflicted_text =
        std::fs::read_to_string(native.workdir().unwrap().join("value.txt")).unwrap();
    assert!(
        conflicted_text.contains("<<<<<<<")
            && conflicted_text.contains("=======")
            && conflicted_text.contains(">>>>>>>"),
        "real worktree conflict markers required: {output:?}"
    );
    assert_eq!(
        native
            .find_reference(&branch)
            .unwrap()
            .target()
            .unwrap()
            .to_string(),
        original_head
    );
    assert!(
        repo.start_log_rebase(&plan).is_err(),
        "conflict cannot admit a second rewrite"
    );
    let (aborted, output) = finish(
        repo.start_log_command(vec!["rebase".into(), "--abort".into()])
            .unwrap(),
    );
    assert!(aborted, "public abort failed: {output:?}");
    assert_eq!(history(&native), original_history);
    assert_eq!(
        std::fs::read_to_string(native.workdir().unwrap().join("value.txt")).unwrap(),
        "second\n"
    );
    assert!(repo.status().unwrap().is_empty());
    assert_eq!(native.state(), git2::RepositoryState::Clean);
}
