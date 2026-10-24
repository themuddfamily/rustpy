//! Git workflows against real temporary repositories: checkout must not be
//! shadowed by tags, Smart Checkout never stashes untracked files, renames found
//! only in the working tree stay unversioned until the IDE records them, and
//! rollback restores files in deleted folders.

use std::path::{Path, PathBuf};

use rustpy_vcs::{FileStatus, GitRepo, UpdateMethod};

fn git_available() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Ann")
        .env("GIT_AUTHOR_EMAIL", "ann@example.com")
        .env("GIT_COMMITTER_NAME", "Ann")
        .env("GIT_COMMITTER_EMAIL", "ann@example.com")
        .env("GIT_EDITOR", "true")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn write(dir: &Path, name: &str, text: &str) {
    if let Some(parent) = dir.join(name).parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(dir.join(name), text).unwrap();
}

fn read(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap_or_else(|_| "<missing>".into())
}

/// A bare `origin` with `main` and `feature` (which changes a.py and adds f.py),
/// cloned into `work` with only `main` checked out.
struct Remote {
    _tmp: tempfile::TempDir,
    work: PathBuf,
}

fn remote_setup() -> Remote {
    let tmp = tempfile::tempdir().unwrap();
    let (bare, seed, work) = (
        tmp.path().join("origin.git"),
        tmp.path().join("seed"),
        tmp.path().join("work"),
    );
    git(
        tmp.path(),
        &["init", "-q", "--bare", bare.to_str().unwrap()],
    );
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-q", "-b", "main"]);
    write(&seed, "a.py", "line1\nline2\nline3\n");
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-q", "-m", "initial"]);
    git(&seed, &["checkout", "-q", "-b", "feature"]);
    write(&seed, "a.py", "line1\nline2\nfeature3\n");
    write(&seed, "f.py", "feature\n");
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-q", "-m", "feature work"]);
    let url = format!("file://{}", bare.display());
    git(&seed, &["remote", "add", "origin", &url]);
    git(&seed, &["push", "-q", "origin", "main", "feature"]);
    git(tmp.path(), &["clone", "-q", "-b", "main", &url, "work"]);
    for (k, v) in [("user.name", "Ann"), ("user.email", "ann@example.com")] {
        git(&work, &["config", k, v]);
    }
    Remote { _tmp: tmp, work }
}

fn stash_count(dir: &Path) -> usize {
    git(dir, &["stash", "list"]).lines().count()
}

#[test]
fn a_tag_named_like_the_branch_does_not_detach_head() {
    if !git_available() {
        return;
    }
    let r = remote_setup();
    let repo = GitRepo::discover(&r.work).unwrap();
    // A release tag "feature" at main's commit.
    git(&r.work, &["tag", "feature"]);
    let msg = repo.checkout_remote_branch("origin/feature").unwrap();
    assert!(msg.contains("new branch 'feature'"), "{msg}");
    assert_eq!(
        repo.current_branch().as_deref(),
        Some("feature"),
        "HEAD must be on the branch, not detached at the tag"
    );
    assert_eq!(
        read(&r.work, "a.py"),
        "line1\nline2\nfeature3\n",
        "the branch's files, not the tag's"
    );
    assert_eq!(
        git(&r.work, &["symbolic-ref", "HEAD"]).trim(),
        "refs/heads/feature"
    );

    // Plain checkout by name, back and forth.
    repo.checkout_branch("main").unwrap();
    repo.checkout_branch("feature").unwrap();
    assert_eq!(repo.current_branch().as_deref(), Some("feature"));
    assert_eq!(read(&r.work, "a.py"), "line1\nline2\nfeature3\n");

    // New branch from 'feature' starts at the branch, not at the tag.
    repo.checkout_branch("main").unwrap();
    repo.create_branch_from("review", "feature", true).unwrap();
    assert_eq!(repo.current_branch().as_deref(), Some("review"));
    assert_eq!(read(&r.work, "a.py"), "line1\nline2\nfeature3\n");

    // Compare and merge use the branch too.
    repo.checkout_branch("main").unwrap();
    let diffs = repo.compare_with_branch("feature").unwrap();
    assert!(
        diffs.iter().any(|d| d.path.ends_with("f.py")),
        "{:?}",
        diffs.iter().map(|d| &d.path).collect::<Vec<_>>()
    );
    let out = repo.merge_branch("feature").unwrap();
    assert!(out.fast_forward && !out.up_to_date, "{out:?}");
    assert_eq!(repo.current_branch().as_deref(), Some("main"));
    assert_eq!(read(&r.work, "a.py"), "line1\nline2\nfeature3\n");

    // A tag alone still checks out (detached), as before.
    repo.checkout_branch("feature").unwrap();
    git(&r.work, &["tag", "only-a-tag", "HEAD~1"]);
    repo.checkout_branch("only-a-tag").unwrap();
    assert!(repo.current_branch().unwrap().starts_with("HEAD detached"));
    assert_eq!(read(&r.work, "a.py"), "line1\nline2\nline3\n");
}

#[test]
fn rebase_and_merge_use_the_branch_not_a_tag_of_the_same_name() {
    if !git_available() {
        return;
    }
    let r = remote_setup();
    let repo = GitRepo::discover(&r.work).unwrap();
    // main gets a local commit; the tag "feature" marks main's first commit.
    write(&r.work, "m.py", "m\n");
    repo.commit(&[r.work.join("m.py")], "main work", false)
        .unwrap();
    git(&r.work, &["tag", "feature", "HEAD~1"]);
    repo.create_branch_from("feature", "origin/feature", false)
        .unwrap();
    let out = repo.rebase_onto("feature", false).unwrap();
    assert!(out.conflicts.is_empty() && !out.up_to_date, "{out:?}");
    let log: Vec<String> = repo
        .log(10)
        .unwrap()
        .into_iter()
        .map(|c| c.summary)
        .collect();
    assert_eq!(
        log,
        vec!["main work", "feature work", "initial"],
        "rebased onto the branch"
    );
    assert_eq!(read(&r.work, "a.py"), "line1\nline2\nfeature3\n");

    // Without a clash git's usual names appear in messages (no refs/remotes/ prefix).
    let r = remote_setup();
    let repo = GitRepo::discover(&r.work).unwrap();
    write(&r.work, "m.py", "m\n");
    repo.commit(&[r.work.join("m.py")], "main work", false)
        .unwrap();
    let out = repo
        .pull_into_current("origin/feature", UpdateMethod::Merge, false)
        .unwrap();
    assert!(out.conflicts.is_empty(), "{out:?}");
    let head = repo.log(1).unwrap().remove(0);
    assert_eq!(head.parents.len(), 2);
    assert_eq!(
        head.summary,
        "Merge remote-tracking branch 'origin/feature'"
    );
}

#[test]
fn smart_checkout_never_stashes_untracked_files() {
    if !git_available() {
        return;
    }
    let r = remote_setup();
    let repo = GitRepo::discover(&r.work).unwrap();
    repo.create_branch_from("feature", "origin/feature", false)
        .unwrap();
    // An untracked f.py collides with the branch's tracked f.py.
    write(&r.work, "f.py", "my untracked f\n");
    let err = repo.checkout_branch("feature").unwrap_err();
    assert!(GitRepo::is_checkout_conflict(&err), "{err:#}");
    let blockers = repo.checkout_blockers("feature").unwrap();
    assert_eq!(blockers.untracked, vec![r.work.join("f.py")]);
    assert!(blockers.changed.is_empty(), "{blockers:?}");

    let err = repo.smart_checkout("feature").unwrap_err();
    let text = format!("{err:#}");
    assert!(
        text.contains("f.py") && text.contains("Untracked"),
        "{text}"
    );
    assert_eq!(
        repo.current_branch().as_deref(),
        Some("main"),
        "nothing was checked out"
    );
    assert_eq!(
        read(&r.work, "f.py"),
        "my untracked f\n",
        "the untracked file is untouched (no conflict markers)"
    );
    assert_eq!(stash_count(&r.work), 0, "nothing was stashed");
    assert!(git(&r.work, &["ls-files", "-u"]).is_empty());

    // A tracked change on top: still refused, and the change stays in place.
    write(&r.work, "a.py", "local1\nline2\nline3\n");
    let blockers = repo.checkout_blockers("feature").unwrap();
    assert_eq!(blockers.changed, vec![r.work.join("a.py")]);
    assert_eq!(blockers.untracked, vec![r.work.join("f.py")]);
    assert!(repo.smart_checkout("feature").is_err());
    assert_eq!(read(&r.work, "a.py"), "local1\nline2\nline3\n");
    assert_eq!(read(&r.work, "f.py"), "my untracked f\n");
    assert_eq!(stash_count(&r.work), 0);
    assert_eq!(repo.current_branch().as_deref(), Some("main"));

    // Once the untracked file is moved away, Smart Checkout stashes the tracked
    // change only and re-applies it on the branch; unrelated untracked files stay put.
    std::fs::rename(r.work.join("f.py"), r.work.join("f.py.mine")).unwrap();
    write(&r.work, "notes.txt", "scratch\n");
    let out = repo.smart_checkout("feature").unwrap();
    assert!(
        out.stashed && out.conflicts.is_empty() && !out.stash_kept,
        "{out:?}"
    );
    assert_eq!(repo.current_branch().as_deref(), Some("feature"));
    assert_eq!(read(&r.work, "a.py"), "local1\nline2\nfeature3\n");
    assert_eq!(read(&r.work, "f.py"), "feature\n");
    assert_eq!(read(&r.work, "f.py.mine"), "my untracked f\n");
    assert_eq!(read(&r.work, "notes.txt"), "scratch\n");
    assert_eq!(
        stash_count(&r.work),
        0,
        "the stash went back cleanly and was dropped"
    );
}

#[test]
fn smart_checkout_keeps_the_stash_on_conflicts() {
    if !git_available() {
        return;
    }
    let r = remote_setup();
    let repo = GitRepo::discover(&r.work).unwrap();
    repo.create_branch_from("feature", "origin/feature", false)
        .unwrap();
    // The same line changed locally and on the branch.
    write(&r.work, "a.py", "line1\nline2\nmine3\n");
    let out = repo.smart_checkout("feature").unwrap();
    assert_eq!(out.conflicts, vec![r.work.join("a.py")], "{out:?}");
    assert!(out.stash_kept);
    assert_eq!(
        stash_count(&r.work),
        1,
        "the local change is safe in the stash"
    );
    assert_eq!(repo.current_branch().as_deref(), Some("feature"));
    assert!(read(&r.work, "a.py").contains("mine3"));

    // A staged new file that the branch adds too (add/add): a conflict, stash kept.
    let r = remote_setup();
    let repo = GitRepo::discover(&r.work).unwrap();
    repo.create_branch_from("feature", "origin/feature", false)
        .unwrap();
    write(&r.work, "f.py", "my staged f\n");
    git(&r.work, &["add", "f.py"]);
    assert_eq!(
        repo.checkout_blockers("feature").unwrap().changed,
        vec![r.work.join("f.py")]
    );
    let out = repo.smart_checkout("feature").unwrap();
    assert_eq!(
        (out.conflicts.clone(), out.stash_kept),
        (vec![r.work.join("f.py")], true),
        "{out:?}"
    );
    assert!(read(&r.work, "f.py").contains("my staged f"));
    assert_eq!(stash_count(&r.work), 1);

    // A local edit of a file the branch deletes (modify/delete): the edit stays
    // in the file and in the stash.
    let r = remote_setup();
    let repo = GitRepo::discover(&r.work).unwrap();
    git(&r.work, &["checkout", "-q", "-b", "gone"]);
    git(&r.work, &["rm", "-q", "a.py"]);
    git(&r.work, &["commit", "-q", "-m", "drop a.py"]);
    git(&r.work, &["checkout", "-q", "main"]);
    write(&r.work, "a.py", "line1\nmy edit\nline3\n");
    let out = repo.smart_checkout("gone").unwrap();
    assert_eq!(
        (out.conflicts.clone(), out.stash_kept),
        (vec![r.work.join("a.py")], true),
        "{out:?}"
    );
    assert_eq!(read(&r.work, "a.py"), "line1\nmy edit\nline3\n");
    assert_eq!(stash_count(&r.work), 1);
    assert_eq!(repo.current_branch().as_deref(), Some("gone"));
}

#[test]
fn an_identical_untracked_file_still_blocks_and_nothing_is_stashed() {
    if !git_available() {
        return;
    }
    let r = remote_setup();
    let repo = GitRepo::discover(&r.work).unwrap();
    repo.create_branch_from("feature", "origin/feature", false)
        .unwrap();
    // The same content as the branch's f.py: libgit2 still refuses to replace it.
    write(&r.work, "f.py", "feature\n");
    let err = repo.checkout_branch("feature").unwrap_err();
    assert!(GitRepo::is_checkout_conflict(&err), "{err:#}");
    let blockers = repo.checkout_blockers("feature").unwrap();
    assert_eq!(
        blockers.untracked,
        vec![r.work.join("f.py")],
        "reported, so no Smart Checkout is offered"
    );
    let text = format!("{:#}", repo.smart_checkout("feature").unwrap_err());
    assert!(
        text.contains("Untracked") && text.contains("f.py"),
        "{text}"
    );
    assert_eq!(repo.current_branch().as_deref(), Some("main"));
    assert_eq!(stash_count(&r.work), 0);
    assert_eq!(read(&r.work, "f.py"), "feature\n");
    // An untracked folder where the branch has a file blocks as well.
    std::fs::remove_file(r.work.join("f.py")).unwrap();
    write(&r.work, "f.py/inner.txt", "x\n");
    assert_eq!(
        repo.checkout_blockers("feature").unwrap().untracked,
        vec![r.work.join("f.py/inner.txt")]
    );
    assert!(repo.smart_checkout("feature").is_err());
    assert_eq!(read(&r.work, "f.py/inner.txt"), "x\n");
    assert_eq!(stash_count(&r.work), 0);
}

#[test]
fn working_tree_renames_stay_unversioned_until_the_ide_records_them() {
    if !git_available() {
        return;
    }
    let r = remote_setup();
    let repo = GitRepo::discover(&r.work).unwrap();
    // Deleted a.py plus a similar new file nobody added: one rename entry,
    // but its new side is unversioned (not committed unless checked).
    std::fs::remove_file(r.work.join("a.py")).unwrap();
    write(&r.work, "secrets.py", "line1\nline2\nline3\nTOKEN = 'x'\n");
    let st = repo.status().unwrap();
    assert_eq!(st.len(), 1, "{st:?}");
    assert_eq!(st[0].status, FileStatus::Renamed);
    assert_eq!(
        st[0].old_path.as_deref(),
        Some(r.work.join("a.py").as_path())
    );
    assert!(st[0].is_unversioned(), "{st:?}");

    // The IDE's own rename records it like `git mv`: a versioned rename.
    std::fs::remove_file(r.work.join("secrets.py")).unwrap();
    git(&r.work, &["checkout", "--", "a.py"]);
    std::fs::rename(r.work.join("a.py"), r.work.join("b.py")).unwrap();
    assert_eq!(
        repo.track_move(&r.work.join("a.py"), &r.work.join("b.py"))
            .unwrap(),
        1
    );
    let st = repo.status().unwrap();
    assert_eq!(st.len(), 1, "{st:?}");
    assert_eq!(
        (st[0].status, st[0].path.clone(), st[0].old_path.clone()),
        (
            FileStatus::Renamed,
            r.work.join("b.py"),
            Some(r.work.join("a.py"))
        )
    );
    assert!(!st[0].is_unversioned() && st[0].staged);
    repo.commit(&[r.work.join("b.py")], "rename a to b", false)
        .unwrap();
    assert!(
        repo.status().unwrap().is_empty(),
        "{:?}",
        repo.status().unwrap()
    );
    let files = git(&r.work, &["ls-files"]);
    assert_eq!(files.lines().collect::<Vec<_>>(), vec!["b.py"]);
    let show = git(&r.work, &["show", "--stat", "-M", "--format=", "HEAD"]);
    assert!(show.contains("a.py => b.py"), "{show}");

    // A moved package: every tracked file inside moves; untracked ones are left alone.
    write(&r.work, "pkg/__init__.py", "");
    write(&r.work, "pkg/util.py", "def f():\n    return 1\n");
    repo.commit(
        &[r.work.join("pkg/__init__.py"), r.work.join("pkg/util.py")],
        "add pkg",
        false,
    )
    .unwrap();
    write(&r.work, "pkg/scratch.py", "tmp\n");
    std::fs::rename(r.work.join("pkg"), r.work.join("lib")).unwrap();
    assert_eq!(
        repo.track_move(&r.work.join("pkg"), &r.work.join("lib"))
            .unwrap(),
        2
    );
    let st = repo.status().unwrap();
    let renamed: Vec<_> = st
        .iter()
        .filter(|e| e.status == FileStatus::Renamed)
        .map(|e| e.path.strip_prefix(&r.work).unwrap().to_path_buf())
        .collect();
    assert_eq!(renamed.len(), 2, "{st:?}");
    assert!(
        st.iter()
            .any(|e| e.status == FileStatus::Untracked && e.path.ends_with("scratch.py")),
        "{st:?}"
    );
    assert!(st
        .iter()
        .filter(|e| e.status == FileStatus::Renamed)
        .all(|e| !e.is_unversioned()));
    // Nothing to record for untracked or unknown paths.
    assert_eq!(
        repo.track_move(&r.work.join("nope.py"), &r.work.join("other.py"))
            .unwrap(),
        0
    );
}

#[test]
fn rollback_restores_a_file_in_a_deleted_folder() {
    if !git_available() {
        return;
    }
    let r = remote_setup();
    let repo = GitRepo::discover(&r.work).unwrap();
    write(&r.work, "pkg/sub/const.py", "X = 1\n");
    repo.commit(&[r.work.join("pkg/sub/const.py")], "add const", false)
        .unwrap();
    std::fs::remove_dir_all(r.work.join("pkg/sub")).unwrap();
    let st = repo.status().unwrap();
    assert_eq!((st.len(), st[0].status), (1, FileStatus::Deleted), "{st:?}");
    repo.rollback(&r.work.join("pkg/sub/const.py")).unwrap();
    assert_eq!(read(&r.work, "pkg/sub/const.py"), "X = 1\n");
    assert!(repo.status().unwrap().is_empty());
}

#[test]
fn rollback_of_a_recorded_rename_restores_index_and_files() {
    if !git_available() {
        return;
    }
    let r = remote_setup();
    let repo = GitRepo::discover(&r.work).unwrap();
    // The IDE renamed a.py and the user edited the result.
    std::fs::rename(r.work.join("a.py"), r.work.join("b.py")).unwrap();
    repo.track_move(&r.work.join("a.py"), &r.work.join("b.py"))
        .unwrap();
    write(&r.work, "b.py", "line1\nline2\nline3\nmore\n");
    // Rollback keeping the new file: a.py is back as committed, b.py stays unversioned.
    repo.rollback_keeping_new(&r.work.join("b.py")).unwrap();
    assert_eq!(read(&r.work, "a.py"), "line1\nline2\nline3\n");
    assert_eq!(
        read(&r.work, "b.py"),
        "line1\nline2\nline3\nmore\n",
        "the new file is kept"
    );
    let st = repo.status().unwrap();
    assert_eq!(st.len(), 1, "{st:?}");
    assert_eq!(
        (st[0].path.clone(), st[0].status),
        (r.work.join("b.py"), FileStatus::Untracked)
    );
    // Full rollback: back to HEAD in the working tree and the index.
    std::fs::remove_file(r.work.join("a.py")).unwrap();
    repo.track_move(&r.work.join("a.py"), &r.work.join("b.py"))
        .unwrap();
    repo.rollback(&r.work.join("b.py")).unwrap();
    assert_eq!(read(&r.work, "a.py"), "line1\nline2\nline3\n");
    assert!(!r.work.join("b.py").exists());
    assert!(
        repo.status().unwrap().is_empty(),
        "{:?}",
        repo.status().unwrap()
    );
    // A staged modification is rolled back completely too.
    write(&r.work, "a.py", "staged\n");
    git(&r.work, &["add", "a.py"]);
    repo.rollback(&r.work.join("a.py")).unwrap();
    assert_eq!(read(&r.work, "a.py"), "line1\nline2\nline3\n");
    assert!(
        repo.status().unwrap().is_empty(),
        "{:?}",
        repo.status().unwrap()
    );
}
