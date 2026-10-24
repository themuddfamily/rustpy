//! Branch workflows and synchronisation with remotes (roadmap §4.1 Branches and
//! Operations): remote branches checked out as local tracking branches, Smart
//! Checkout (stash, checkout, unstash), branches from any start point, Update
//! Project and Pull with merge or rebase, rebase onto/continue/skip, and
//! finishing an interrupted merge, cherry-pick or revert.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context};
use git2::{BranchType, ErrorClass, ErrorCode, RepositoryState};

use crate::GitRepo;

/// How Update Project and Pull integrate incoming commits.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum UpdateMethod {
    /// `git merge` (a fast-forward when there are no local commits).
    #[default]
    Merge,
    /// `git rebase`: local commits are replayed on top of the incoming ones.
    Rebase,
}

impl UpdateMethod {
    /// Parses the settings value (`"rebase"`; anything else is merge).
    pub fn from_setting(value: &str) -> UpdateMethod {
        if value.trim().eq_ignore_ascii_case("rebase") {
            UpdateMethod::Rebase
        } else {
            UpdateMethod::Merge
        }
    }

    /// The settings value.
    pub fn setting(self) -> &'static str {
        match self {
            UpdateMethod::Merge => "merge",
            UpdateMethod::Rebase => "rebase",
        }
    }
}

/// What [`GitRepo::smart_checkout`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckoutOutcome {
    /// Uncommitted changes were stashed and re-applied around the checkout.
    pub stashed: bool,
    /// Files where re-applying the changes conflicted (left for the conflict resolver).
    pub conflicts: Vec<PathBuf>,
    /// The stash entry is still `stash@{0}` because re-applying it failed or conflicted.
    pub stash_kept: bool,
}

/// What stops a checkout (see [`GitRepo::checkout_blockers`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckoutBlockers {
    /// Tracked files whose local changes the checkout would overwrite (Smart
    /// Checkout stashes and re-applies them).
    pub changed: Vec<PathBuf>,
    /// Untracked files where the branch has a file of its own (even an
    /// identical one: the checkout still refuses to replace them), or a file
    /// where they sit in a folder. Smart Checkout does not stash untracked
    /// files, so these must be moved or deleted first.
    pub untracked: Vec<PathBuf>,
}

impl CheckoutBlockers {
    /// Status line for untracked files that block checking out `name`.
    pub fn untracked_message(&self, root: &Path, name: &str) -> String {
        let names: Vec<String> = self
            .untracked
            .iter()
            .map(|p| p.strip_prefix(root).unwrap_or(p).display().to_string())
            .collect();
        let mut list = names.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
        if names.len() > 3 {
            list.push_str(&format!(" and {} more", names.len() - 3));
        }
        format!("Untracked files would be overwritten by checking out '{name}': {list}. Move or delete them first, then check out again")
    }
}

/// Whether `text` has a complete Git conflict block (`<<<<<<<` … `>>>>>>>` lines).
fn has_conflict_markers(text: &[u8]) -> bool {
    let text = String::from_utf8_lossy(text);
    let mut open = false;
    for line in text.lines() {
        if line.starts_with("<<<<<<<") {
            open = true;
        } else if open && line.starts_with(">>>>>>>") {
            return true;
        }
    }
    false
}

/// Result of an update, pull, rebase or continue run through the git CLI.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncOutcome {
    /// Nothing to integrate.
    pub up_to_date: bool,
    /// Integrated by fast-forward (there were no local commits).
    pub fast_forward: bool,
    /// Number of incoming commits.
    pub incoming: usize,
    /// Conflicted paths left for resolution.
    pub conflicts: Vec<PathBuf>,
    /// The operation left in progress (`merge`, `rebase`, …) when it stopped.
    pub stopped: Option<String>,
    /// The last line of git's output.
    pub detail: String,
}

fn last_line(text: &str) -> String {
    text.lines()
        .rfind(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// A one-line reason for a failed operation, for the status bar: git's first
/// `error:` or `fatal:` line (followed by the files it lists after a trailing
/// colon), otherwise the last line of the message. The last line alone can
/// mislead: when local changes block a merge, git prints `Updating 1a2b..3c4d`
/// after the reason.
pub fn failure_reason(err: &anyhow::Error) -> String {
    let text = format!("{err:#}");
    let lines: Vec<&str> = text.lines().collect();
    let Some(i) = lines
        .iter()
        .position(|l| l.starts_with("error: ") || l.starts_with("fatal: "))
    else {
        return last_line(&text);
    };
    let mut reason = lines[i]
        .split_once(": ")
        .map_or(lines[i], |(_, r)| r)
        .trim()
        .to_string();
    if reason.ends_with(':') {
        let files: Vec<&str> = lines[i + 1..]
            .iter()
            .take_while(|l| l.starts_with('\t') || l.starts_with("    "))
            .map(|l| l.trim())
            .collect();
        if !files.is_empty() {
            reason = format!(
                "{reason} {}",
                files.iter().take(3).copied().collect::<Vec<_>>().join(", ")
            );
            if files.len() > 3 {
                reason.push_str(&format!(" and {} more", files.len() - 3));
            }
        }
    }
    reason
}

impl GitRepo {
    /// Whether `err` is libgit2 refusing a checkout because uncommitted changes
    /// (or untracked files) would be overwritten.
    pub fn is_checkout_conflict(err: &anyhow::Error) -> bool {
        err.chain().any(|e| {
            e.downcast_ref::<git2::Error>().is_some_and(|g| {
                g.code() == ErrorCode::Conflict && g.class() == ErrorClass::Checkout
            })
        })
    }

    /// Splits a remote branch name (`origin/feature/x`) into the remote and the
    /// branch, preferring the longest configured remote name.
    pub fn split_remote_ref(&self, remote_ref: &str) -> Option<(String, String)> {
        let best = self
            .remotes()
            .into_iter()
            .filter(|r| {
                remote_ref
                    .strip_prefix(r.as_str())
                    .is_some_and(|rest| rest.len() > 1 && rest.starts_with('/'))
            })
            .max_by_key(String::len);
        match best {
            Some(r) => {
                let branch = remote_ref[r.len() + 1..].to_string();
                Some((r, branch))
            }
            None => remote_ref
                .split_once('/')
                .filter(|(r, b)| !r.is_empty() && !b.is_empty())
                .map(|(r, b)| (r.to_string(), b.to_string())),
        }
    }

    /// Checks out the remote branch `remote_ref` (`origin/feature`) as a local
    /// branch: a local branch of the same name is reused (the
    /// message says whether it is behind), otherwise one is created at the remote
    /// commit with `remote_ref` as its upstream. HEAD is never detached.
    ///
    /// When uncommitted changes block the checkout this fails like
    /// [`GitRepo::checkout_branch`] (see [`GitRepo::is_checkout_conflict`]); the
    /// local branch exists by then, so [`GitRepo::smart_checkout`] can take over.
    pub fn checkout_remote_branch(&self, remote_ref: &str) -> anyhow::Result<String> {
        let _operation =
            crate::RepositoryOperation::enter(&self.root, "Git checkout remote branch")?;
        let (_, short) = self
            .split_remote_ref(remote_ref)
            .ok_or_else(|| anyhow!("'{remote_ref}' is not a remote branch"))?;
        let remote_oid = self
            .repo
            .find_branch(remote_ref, BranchType::Remote)
            .with_context(|| format!("no remote branch '{remote_ref}'"))?
            .get()
            .target()
            .ok_or_else(|| anyhow!("'{remote_ref}' does not point at a commit"))?;
        let existing = self
            .repo
            .find_branch(&short, BranchType::Local)
            .ok()
            .and_then(|b| b.get().target());
        match existing {
            Some(local_oid) => {
                self.checkout_branch(&short)?;
                let (ahead, behind) = self
                    .repo
                    .graph_ahead_behind(local_oid, remote_oid)
                    .unwrap_or((0, 0));
                Ok(match (ahead, behind) {
                    (0, 0) => format!("Checked out '{short}'"),
                    (0, b) => format!("Checked out '{short}' ({b} commit(s) behind '{remote_ref}')"),
                    (a, 0) => format!("Checked out '{short}' ({a} commit(s) ahead of '{remote_ref}')"),
                    (a, b) => format!("Checked out '{short}' ({a} commit(s) ahead of and {b} behind '{remote_ref}')"),
                })
            }
            None => {
                let commit = self.repo.find_commit(remote_oid)?;
                let mut branch = self.repo.branch(&short, &commit, false)?;
                branch.set_upstream(Some(remote_ref))?;
                self.checkout_branch(&short)?;
                Ok(format!(
                    "Checked out new branch '{short}' tracking '{remote_ref}'"
                ))
            }
        }
    }

    /// Creates branch `name` at `start_point` (a branch, tag or any revision) and
    /// optionally checks it out. A branch created from the remote branch of the
    /// same name tracks it, like `git branch x origin/x`.
    pub fn create_branch_from(
        &self,
        name: &str,
        start_point: &str,
        checkout: bool,
    ) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git create branch from")?;
        let name = name.trim();
        if name.is_empty() || !git2::Branch::name_is_valid(name).unwrap_or(false) {
            bail!("'{name}' is not a valid branch name");
        }
        if self.repo.find_branch(name, BranchType::Local).is_ok() {
            bail!("a branch named '{name}' already exists");
        }
        let (object, reference) = self
            .repo
            .revparse_ext(&self.branch_spec(start_point))
            .with_context(|| format!("unknown revision '{start_point}'"))?;
        let commit = object.peel_to_commit()?;
        let mut branch = self.repo.branch(name, &commit, false)?;
        let remote = reference
            .as_ref()
            .filter(|r| r.is_remote())
            .and_then(|r| r.shorthand().ok().map(str::to_owned));
        if let Some(remote) = remote {
            if self
                .split_remote_ref(&remote)
                .is_some_and(|(_, b)| b == name)
            {
                branch.set_upstream(Some(&remote))?;
            }
        }
        if checkout {
            self.checkout_branch(name)?;
        }
        Ok(())
    }

    /// What stops checking out `name` (read-only): tracked files with local
    /// changes that the checkout rewrites, and untracked files where the branch
    /// has a file or folder of its own (or a file where they sit in a folder).
    pub fn checkout_blockers(&self, name: &str) -> anyhow::Result<CheckoutBlockers> {
        let target = self
            .repo
            .revparse_single(&self.branch_spec(name))?
            .peel_to_tree()?;
        let head = self.repo.head().ok().and_then(|h| h.peel_to_tree().ok());
        // The paths a checkout rewrites: everything that differs between HEAD and the target.
        let diff = self
            .repo
            .diff_tree_to_tree(head.as_ref(), Some(&target), None)?;
        let mut rewritten: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        for d in diff.deltas() {
            for f in [d.old_file(), d.new_file()] {
                if let Some(p) = f.path() {
                    rewritten.insert(p.to_path_buf());
                }
            }
        }
        let is_blob = |rel: &Path| {
            target
                .get_path(rel)
                .is_ok_and(|e| e.kind() == Some(git2::ObjectType::Blob))
        };
        let mut opts = git2::StatusOptions::new();
        opts.include_untracked(true)
            .recurse_untracked_dirs(true)
            .include_ignored(false);
        let mut out = CheckoutBlockers::default();
        for e in self.repo.statuses(Some(&mut opts))?.iter() {
            let Ok(rel) = e.path().map(PathBuf::from) else {
                continue;
            };
            let s = e.status();
            if s.is_wt_new() {
                // libgit2 refuses to replace an untracked file even when the
                // branch's copy is identical.
                let in_the_way = target.get_path(&rel).is_ok()
                    || rel
                        .ancestors()
                        .skip(1)
                        .any(|a| !a.as_os_str().is_empty() && is_blob(a));
                if in_the_way {
                    out.untracked.push(self.root.join(&rel));
                }
            } else if !s.is_ignored() && rewritten.contains(&rel) {
                out.changed.push(self.root.join(&rel));
            }
        }
        Ok(out)
    }

    /// Smart Checkout: checks out `name`; when local changes to
    /// tracked files would be overwritten they are stashed, the branch is checked
    /// out and the stash is re-applied. Untracked files are never stashed: when
    /// the branch would overwrite some, nothing is done and the error names them.
    /// Conflicts while re-applying are left for the conflict resolver and the
    /// stash is kept; it is dropped only after it went back cleanly.
    pub fn smart_checkout(&self, name: &str) -> anyhow::Result<CheckoutOutcome> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git smart checkout")?;
        let refused = match self.checkout_branch(name) {
            Ok(()) => return Ok(CheckoutOutcome::default()),
            Err(e) if Self::is_checkout_conflict(&e) => e,
            Err(e) => return Err(e),
        };
        let blockers = self.checkout_blockers(name)?;
        if !blockers.untracked.is_empty() {
            bail!("{}", blockers.untracked_message(&self.root, name));
        }
        if blockers.changed.is_empty() {
            // Nothing a stash could carry over: report why the checkout failed.
            return Err(refused.context(format!("cannot check out '{name}'")));
        }
        let from = self.current_branch().unwrap_or_else(|| "HEAD".into());
        self.stash_push(
            Some(&format!("RustPy smart checkout: {from} → {name}")),
            false,
        )?;
        let Some(stash) = self
            .stash_list()
            .ok()
            .and_then(|l| l.into_iter().next())
            .map(|s| s.id)
        else {
            // Stashed, but the entry cannot be read back: put the changes back rather than going on blind.
            let restored = self.stash_pop(0).is_ok();
            bail!(
                "cannot check out '{name}': the stash could not be read; {}",
                if restored {
                    "local changes restored"
                } else {
                    "local changes are in stash@{0}"
                }
            );
        };
        if let Err(e) = self.checkout_branch(name) {
            let restored = self.stash_pop(0).is_ok();
            return Err(e.context(if restored {
                format!("cannot check out '{name}'; local changes restored")
            } else {
                format!("cannot check out '{name}'; local changes are in stash@{{0}}")
            }));
        }
        let mut out = CheckoutOutcome {
            stashed: true,
            ..CheckoutOutcome::default()
        };
        // Staged changes that no longer apply to the index are re-applied unstaged.
        let applied = match self.stash_apply(0) {
            Err(e)
                if e.downcast_ref::<git2::Error>()
                    .is_some_and(|g| g.code() == ErrorCode::Conflict) =>
            {
                self.stash_apply_unstaged(0)
            }
            other => other,
        };
        match applied {
            Ok(()) => {
                let mut conflicts = self.conflicts().unwrap_or_default();
                // Belt and braces: conflict markers in a file the stash changed
                // count even when the index has no conflict entry for it.
                for path in self.stash_marker_conflicts(&stash) {
                    if !conflicts.contains(&path) {
                        conflicts.push(path);
                    }
                }
                out.conflicts = conflicts;
                // Only our own stash entry is ever dropped, and only after a clean re-apply.
                let ours = self
                    .stash_list()
                    .ok()
                    .and_then(|l| l.into_iter().next())
                    .is_some_and(|s| s.id == stash);
                if out.conflicts.is_empty() && ours {
                    self.stash_drop(0)?;
                } else {
                    out.stash_kept = true;
                }
            }
            Err(_) => out.stash_kept = true,
        }
        Ok(out)
    }

    /// Files the stash `id` changed that now hold conflict markers their stashed
    /// version did not have.
    fn stash_marker_conflicts(&self, id: &str) -> Vec<PathBuf> {
        let Some(stash) = git2::Oid::from_str(id)
            .ok()
            .and_then(|oid| self.repo.find_commit(oid).ok())
        else {
            return Vec::new();
        };
        let (Ok(tree), Some(base)) = (
            stash.tree(),
            stash.parent(0).ok().and_then(|p| p.tree().ok()),
        ) else {
            return Vec::new();
        };
        let Ok(diff) = self.repo.diff_tree_to_tree(Some(&base), Some(&tree), None) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for d in diff.deltas() {
            let Some(rel) = d.new_file().path() else {
                continue;
            };
            let path = self.root.join(rel);
            let Ok(now) = std::fs::read(&path) else {
                continue;
            };
            let stashed = self
                .repo
                .find_blob(d.new_file().id())
                .map(|b| b.content().to_vec())
                .unwrap_or_default();
            if has_conflict_markers(&now) && !has_conflict_markers(&stashed) {
                out.push(path);
            }
        }
        out
    }

    fn stash_apply_unstaged(&self, index: usize) -> anyhow::Result<()> {
        let mut r = git2::Repository::open(&self.root)?;
        r.stash_apply(index, None)?;
        Ok(())
    }

    // ---- update, pull, rebase ------------------------------------------------

    /// Runs `git` without prompts or editors (messages are taken as prepared).
    pub(crate) fn git_cli(&self, args: &[&str]) -> anyhow::Result<String> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git git cli")?;
        let owned: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        if let Some(result) =
            crate::ssh_transport::run_inline(&self.root, &owned, &self.command_env)
        {
            return result;
        }
        let out = crate::git_command(&self.root, &owned, &self.command_env)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_EDITOR", "true")
            .env("GIT_SEQUENCE_EDITOR", "true")
            .env("GIT_MERGE_AUTOEDIT", "no")
            .stdin(std::process::Stdio::null())
            .output()
            .context("cannot run git")?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        if out.status.success() {
            Ok(format!("{stdout}{stderr}"))
        } else {
            bail!("git {} failed:\n{stderr}{stdout}", args.join(" "))
        }
    }

    /// Update Project: fetches the current branch's remote and integrates its
    /// upstream branch by merge or rebase. `autostash` stashes uncommitted
    /// changes around the operation (`--autostash`); when it stops on conflicts
    /// git restores them once the operation is continued or aborted.
    pub fn update(&self, method: UpdateMethod, autostash: bool) -> anyhow::Result<SyncOutcome> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git update")?;
        if let Some(op) = self.current_operation() {
            bail!("finish or abort the {op} in progress first");
        }
        let upstream = self.upstream_of_current().ok_or_else(|| anyhow!("the current branch has no upstream branch: push it first or check out a remote branch"))?;
        if let Some((remote, _)) = self
            .split_remote_ref(&upstream)
            .filter(|(r, _)| self.remotes().contains(r))
        {
            self.git_cli(&["fetch", &remote])?;
        }
        self.integrate(&upstream, method, autostash)
    }

    /// Pull into current using merge or rebase: fetches `remote_ref`
    /// (`origin/feature`) and integrates it into the current branch.
    pub fn pull_into_current(
        &self,
        remote_ref: &str,
        method: UpdateMethod,
        autostash: bool,
    ) -> anyhow::Result<SyncOutcome> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git pull into current")?;
        if let Some(op) = self.current_operation() {
            bail!("finish or abort the {op} in progress first");
        }
        let (remote, branch) = self
            .split_remote_ref(remote_ref)
            .ok_or_else(|| anyhow!("'{remote_ref}' is not a remote branch"))?;
        self.git_cli(&["fetch", &remote, &branch])?;
        self.integrate(remote_ref, method, autostash)
    }

    /// Rebase current onto `upstream` (a branch or revision).
    pub fn rebase_onto(&self, upstream: &str, autostash: bool) -> anyhow::Result<SyncOutcome> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git rebase onto")?;
        if let Some(op) = self.current_operation() {
            bail!("finish or abort the {op} in progress first");
        }
        self.integrate(upstream, UpdateMethod::Rebase, autostash)
    }

    /// Continues a rebase stopped on conflicts (after they were resolved).
    pub fn rebase_continue(&self) -> anyhow::Result<SyncOutcome> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git rebase continue")?;
        let conflicts = self.conflicts()?;
        if !conflicts.is_empty() {
            bail!("resolve the {} conflicted file(s) first", conflicts.len());
        }
        let result = self.git_cli(&["rebase", "--continue"]);
        self.sync_outcome(result, false, 0)
    }

    /// Skips the commit a rebase stopped on.
    pub fn rebase_skip(&self) -> anyhow::Result<SyncOutcome> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git rebase skip")?;
        let result = self.git_cli(&["rebase", "--skip"]);
        self.sync_outcome(result, false, 0)
    }

    /// Continue: finishes the operation in progress once its conflicts are
    /// resolved. A merge, cherry-pick or revert is committed with `message`
    /// (empty: the prepared message, see [`GitRepo::commit`]); a rebase or a
    /// multi-commit cherry-pick/revert continues through the git CLI.
    pub fn continue_operation(&self, message: &str) -> anyhow::Result<SyncOutcome> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git continue operation")?;
        let conflicts = self.conflicts()?;
        if !conflicts.is_empty() {
            bail!("resolve the {} conflicted file(s) first", conflicts.len());
        }
        match self.repo.state() {
            RepositoryState::Clean => bail!("no operation in progress"),
            RepositoryState::Merge | RepositoryState::CherryPick | RepositoryState::Revert => {
                let id = self.commit(&[], message, false)?;
                Ok(SyncOutcome {
                    detail: id,
                    ..SyncOutcome::default()
                })
            }
            RepositoryState::Rebase
            | RepositoryState::RebaseInteractive
            | RepositoryState::RebaseMerge => self.rebase_continue(),
            RepositoryState::CherryPickSequence => {
                let result = self.git_cli(&["cherry-pick", "--continue"]);
                self.sync_outcome(result, false, 0)
            }
            RepositoryState::RevertSequence => {
                let result = self.git_cli(&["revert", "--continue"]);
                self.sync_outcome(result, false, 0)
            }
            _ => bail!(
                "cannot continue a {}",
                self.current_operation().unwrap_or_default()
            ),
        }
    }

    /// Merges or rebases the current branch onto `target` with the git CLI.
    pub(crate) fn integrate(
        &self,
        target: &str,
        method: UpdateMethod,
        autostash: bool,
    ) -> anyhow::Result<SyncOutcome> {
        let head = self
            .repo
            .head()
            .context("the repository has no commits yet")?
            .peel_to_commit()?
            .id();
        // A branch name means the branch, even when a tag has the same name.
        let theirs = self
            .repo
            .revparse_single(&self.branch_spec(target))
            .with_context(|| format!("unknown revision '{target}'"))?
            .peel_to_commit()?
            .id();
        let arg = self.branch_cli_arg(target);
        let target = arg.as_str();
        let (ahead, behind) = self.repo.graph_ahead_behind(head, theirs)?;
        if behind == 0 {
            return Ok(SyncOutcome {
                up_to_date: true,
                ..SyncOutcome::default()
            });
        }
        let mut args: Vec<&str> = match method {
            UpdateMethod::Merge => vec!["merge", "--no-edit"],
            UpdateMethod::Rebase => vec!["rebase"],
        };
        if autostash {
            args.push("--autostash");
        }
        args.push(target);
        let result = self.git_cli(&args);
        self.sync_outcome(result, ahead == 0, behind)
    }

    fn sync_outcome(
        &self,
        result: anyhow::Result<String>,
        fast_forward: bool,
        incoming: usize,
    ) -> anyhow::Result<SyncOutcome> {
        let conflicts = self.conflicts().unwrap_or_default();
        let stopped = self.current_operation();
        match result {
            Ok(out) => Ok(SyncOutcome {
                up_to_date: false,
                fast_forward: fast_forward && conflicts.is_empty(),
                incoming,
                conflicts,
                stopped,
                detail: last_line(&out),
            }),
            Err(e) if !conflicts.is_empty() => Ok(SyncOutcome {
                up_to_date: false,
                fast_forward: false,
                incoming,
                conflicts,
                stopped,
                detail: last_line(&e.to_string()),
            }),
            Err(e) => Err(e),
        }
    }

    // ---- autostash safety nets -------------------------------------------------

    /// `git merge --autostash` keeps the stash in `MERGE_AUTOSTASH` until the
    /// merge is committed. Commits made with libgit2 re-apply it here like `git
    /// commit` does; when that conflicts the stash goes to the stash list.
    pub(crate) fn restore_merge_autostash(&self) {
        let file = self.repo.path().join("MERGE_AUTOSTASH");
        let Ok(oid) = std::fs::read_to_string(&file) else {
            return;
        };
        let _ = std::fs::remove_file(&file);
        let oid = oid.trim();
        if oid.is_empty() {
            return;
        }
        if self.git_cli(&["stash", "apply", oid]).is_err()
            || !self.conflicts().unwrap_or_default().is_empty()
        {
            let _ = self.git_cli(&["stash", "store", "-m", "autostash", oid]);
        }
    }

    /// Before the state files of a merge or rebase are deleted, moves their
    /// autostash (if any) to the stash list so it is not lost.
    pub(crate) fn keep_autostash(&self) {
        let git_dir = self.repo.path();
        for file in [
            git_dir.join("MERGE_AUTOSTASH"),
            git_dir.join("rebase-merge/autostash"),
            git_dir.join("rebase-apply/autostash"),
        ] {
            if let Ok(oid) = std::fs::read_to_string(&file) {
                if !oid.trim().is_empty()
                    && self
                        .git_cli(&["stash", "store", "-m", "autostash", oid.trim()])
                        .is_ok()
                {
                    let _ = std::fs::remove_file(&file);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

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
        std::fs::write(dir.join(name), text).unwrap();
    }

    fn read(dir: &Path, name: &str) -> String {
        std::fs::read_to_string(dir.join(name)).unwrap()
    }

    /// A bare `origin` (file:// URL) with `main` and `feature`, cloned into
    /// `work` (only `main` checked out) and `other` (a colleague's clone).
    struct Remote {
        _tmp: tempfile::TempDir,
        work: PathBuf,
        other: PathBuf,
    }

    fn remote_setup() -> Remote {
        let tmp = tempfile::tempdir().unwrap();
        let (bare, seed, work, other) = (
            tmp.path().join("origin.git"),
            tmp.path().join("seed"),
            tmp.path().join("work"),
            tmp.path().join("other"),
        );
        git2::Repository::init_bare(&bare).unwrap();
        std::fs::create_dir_all(&seed).unwrap();
        git(&seed, &["init", "-q", "-b", "main"]);
        for (k, v) in [("user.name", "Ann"), ("user.email", "ann@example.com")] {
            git(&seed, &["config", k, v]);
        }
        write(&seed, "a.py", "line1\nline2\nline3\n");
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "-q", "-m", "initial"]);
        git(&seed, &["checkout", "-q", "-b", "feature"]);
        write(&seed, "a.py", "line1\nline2\nfeature3\n");
        write(&seed, "f.py", "feature\n");
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "-q", "-m", "feature work"]);
        let url = format!("file://{}", bare.display());
        let seed_repo = GitRepo::discover(&seed).unwrap();
        seed_repo.add_remote("origin", &url).unwrap();
        git(&seed, &["push", "-q", "origin", "main", "feature"]);
        for dir in [&work, &other] {
            git(
                tmp.path(),
                &[
                    "clone",
                    "-q",
                    "-b",
                    "main",
                    &url,
                    dir.file_name().unwrap().to_str().unwrap(),
                ],
            );
            for (k, v) in [("user.name", "Ann"), ("user.email", "ann@example.com")] {
                git(dir, &["config", k, v]);
            }
        }
        Remote {
            _tmp: tmp,
            work,
            other,
        }
    }

    #[test]
    fn remote_branch_checkout_creates_a_tracking_branch() {
        if !git_available() {
            return;
        }
        let r = remote_setup();
        let repo = GitRepo::discover(&r.work).unwrap();
        assert!(repo
            .branches()
            .iter()
            .any(|b| b.is_remote && b.name == "origin/feature"));
        assert_eq!(
            repo.split_remote_ref("origin/feature/x"),
            Some(("origin".into(), "feature/x".into()))
        );
        let msg = repo.checkout_remote_branch("origin/feature").unwrap();
        assert!(msg.contains("new branch 'feature'"), "{msg}");
        assert_eq!(
            repo.current_branch().as_deref(),
            Some("feature"),
            "HEAD must not be detached"
        );
        assert_eq!(
            repo.upstream_of_current().as_deref(),
            Some("origin/feature")
        );
        assert_eq!(read(&r.work, "a.py"), "line1\nline2\nfeature3\n");
        // A commit made now lands on the branch and is pushable to its upstream.
        write(&r.work, "g.py", "g\n");
        repo.commit(&[r.work.join("g.py")], "more feature", false)
            .unwrap();
        assert_eq!(repo.ahead_behind(), Some((1, 0)));
        // Checking it out again reuses the local branch and reports its distance.
        repo.checkout_branch("main").unwrap();
        let msg = repo.checkout_remote_branch("origin/feature").unwrap();
        assert!(msg.contains("1 commit(s) ahead"), "{msg}");
        assert_eq!(repo.current_branch().as_deref(), Some("feature"));
        assert_eq!(
            repo.branches()
                .iter()
                .filter(|b| !b.is_remote && b.name == "feature")
                .count(),
            1
        );
    }

    #[test]
    fn new_branch_from_a_start_point() {
        if !git_available() {
            return;
        }
        let r = remote_setup();
        let repo = GitRepo::discover(&r.work).unwrap();
        repo.create_branch_from("review", "origin/feature", false)
            .unwrap();
        assert_eq!(repo.current_branch().as_deref(), Some("main"));
        let review = repo
            .branches()
            .into_iter()
            .find(|b| b.name == "review")
            .unwrap();
        assert_eq!(
            review.upstream, None,
            "a differently named branch does not track the remote one"
        );
        repo.create_branch_from("feature", "origin/feature", true)
            .unwrap();
        assert_eq!(repo.current_branch().as_deref(), Some("feature"));
        assert_eq!(
            repo.upstream_of_current().as_deref(),
            Some("origin/feature")
        );
        let first = repo.log(10).unwrap().last().unwrap().id.clone();
        repo.create_branch_from("old", &first, true).unwrap();
        assert_eq!(repo.log(10).unwrap().len(), 1);
        assert!(
            repo.create_branch_from("old", "HEAD", false).is_err(),
            "duplicate name"
        );
        assert!(
            repo.create_branch_from("bad name", "HEAD", false).is_err(),
            "invalid name"
        );
    }

    #[test]
    fn smart_checkout_stashes_and_restores_local_changes() {
        if !git_available() {
            return;
        }
        let r = remote_setup();
        let repo = GitRepo::discover(&r.work).unwrap();
        repo.create_branch_from("feature", "origin/feature", false)
            .unwrap();
        // A local edit of a file that differs on 'feature' blocks a plain checkout.
        write(&r.work, "a.py", "local1\nline2\nline3\n");
        write(&r.work, "note.txt", "untracked\n");
        let err = repo.checkout_branch("feature").unwrap_err();
        assert!(GitRepo::is_checkout_conflict(&err), "{err:#}");
        let out = repo.smart_checkout("feature").unwrap();
        assert!(
            out.stashed && out.conflicts.is_empty() && !out.stash_kept,
            "{out:?}"
        );
        assert_eq!(repo.current_branch().as_deref(), Some("feature"));
        assert_eq!(
            read(&r.work, "a.py"),
            "local1\nline2\nfeature3\n",
            "local edit merged onto the branch"
        );
        assert_eq!(read(&r.work, "note.txt"), "untracked\n");
        assert!(repo.stash_list().unwrap().is_empty());
        // Conflicting local edit: the conflict is left for the resolver and the stash is kept.
        repo.checkout_branch("main").unwrap_err();
        let out = repo.smart_checkout("main").unwrap();
        assert!(out.conflicts.is_empty(), "{out:?}");
        assert_eq!(read(&r.work, "a.py"), "local1\nline2\nline3\n");
        write(&r.work, "a.py", "line1\nline2\nmine3\n");
        let out = repo.smart_checkout("feature").unwrap();
        assert_eq!(out.conflicts, vec![r.work.join("a.py")]);
        assert!(out.stash_kept);
        assert_eq!(repo.current_branch().as_deref(), Some("feature"));
        assert_eq!(repo.stash_list().unwrap().len(), 1);
        // Without local changes a smart checkout is a plain checkout.
        let r2 = remote_setup();
        let repo2 = GitRepo::discover(&r2.work).unwrap();
        repo2
            .create_branch_from("feature", "origin/feature", false)
            .unwrap();
        assert_eq!(
            repo2.smart_checkout("feature").unwrap(),
            CheckoutOutcome::default()
        );
    }

    /// The colleague pushes `their` to main while `work` commits `mine` locally.
    fn diverge(r: &Remote, mine: (&str, &str), their: (&str, &str)) -> GitRepo {
        write(&r.other, their.0, their.1);
        git(&r.other, &["add", "."]);
        git(&r.other, &["commit", "-q", "-m", "theirs"]);
        git(&r.other, &["push", "-q", "origin", "main"]);
        let repo = GitRepo::discover(&r.work).unwrap();
        write(&r.work, mine.0, mine.1);
        repo.commit(&[r.work.join(mine.0)], "mine", false).unwrap();
        repo
    }

    #[test]
    fn update_project_merges_or_rebases_diverged_history() {
        if !git_available() {
            return;
        }
        let r = remote_setup();
        let repo = diverge(&r, ("m.py", "m\n"), ("t.py", "t\n"));
        let out = repo.update(UpdateMethod::Merge, true).unwrap();
        assert!(
            !out.up_to_date
                && !out.fast_forward
                && out.conflicts.is_empty()
                && out.stopped.is_none(),
            "{out:?}"
        );
        assert_eq!(out.incoming, 1);
        let head = repo.log(1).unwrap().remove(0);
        assert_eq!(head.parents.len(), 2, "merge commit");
        assert!(r.work.join("t.py").exists() && r.work.join("m.py").exists());
        assert!(repo.update(UpdateMethod::Merge, true).unwrap().up_to_date);

        let r = remote_setup();
        let repo = diverge(&r, ("m.py", "m\n"), ("t.py", "t\n"));
        // Uncommitted work survives thanks to the autostash.
        write(&r.work, "a.py", "line1\nwip\nline3\n");
        let out = repo.update(UpdateMethod::Rebase, true).unwrap();
        assert!(out.conflicts.is_empty() && out.stopped.is_none(), "{out:?}");
        let log = repo.log(10).unwrap();
        assert_eq!(
            log.iter().map(|c| c.summary.as_str()).collect::<Vec<_>>(),
            vec!["mine", "theirs", "initial"],
            "linear history"
        );
        assert!(log.iter().all(|c| c.parents.len() <= 1));
        assert_eq!(read(&r.work, "a.py"), "line1\nwip\nline3\n");
        assert_eq!(repo.current_branch().as_deref(), Some("main"));
        assert_eq!(repo.ahead_behind(), Some((1, 0)));
    }

    #[test]
    fn rebase_conflicts_continue_and_skip() {
        if !git_available() {
            return;
        }
        let r = remote_setup();
        let repo = diverge(
            &r,
            ("a.py", "line1\nmine\nline3\n"),
            ("a.py", "line1\ntheirs\nline3\n"),
        );
        let out = repo.update(UpdateMethod::Rebase, false).unwrap();
        assert_eq!(out.conflicts, vec![r.work.join("a.py")]);
        assert_eq!(out.stopped.as_deref(), Some("rebase"));
        assert_eq!(repo.current_operation().as_deref(), Some("rebase"));
        assert!(
            repo.rebase_continue().is_err(),
            "conflicts must be resolved first"
        );
        repo.resolve_conflict(&r.work.join("a.py"), "line1\nboth\nline3\n")
            .unwrap();
        let out = repo.continue_operation("").unwrap();
        assert!(out.stopped.is_none() && out.conflicts.is_empty(), "{out:?}");
        assert_eq!(repo.current_operation(), None);
        assert_eq!(repo.current_branch().as_deref(), Some("main"));
        let log = repo.log(10).unwrap();
        assert_eq!(log[0].summary, "mine", "the original message is kept");
        assert_eq!(log[1].summary, "theirs");
        assert_eq!(read(&r.work, "a.py"), "line1\nboth\nline3\n");

        // Skip drops the conflicting local commit.
        let r = remote_setup();
        let repo = diverge(
            &r,
            ("a.py", "line1\nmine\nline3\n"),
            ("a.py", "line1\ntheirs\nline3\n"),
        );
        let upstream = repo.upstream_of_current().unwrap();
        repo.git_cli(&["fetch", "origin"]).unwrap();
        let out = repo.rebase_onto(&upstream, false).unwrap();
        assert_eq!(out.conflicts.len(), 1);
        let out = repo.rebase_skip().unwrap();
        assert!(out.stopped.is_none() && out.conflicts.is_empty(), "{out:?}");
        assert_eq!(repo.log(1).unwrap()[0].summary, "theirs");
        assert_eq!(read(&r.work, "a.py"), "line1\ntheirs\nline3\n");

        // Abort restores the local commit.
        let r = remote_setup();
        let repo = diverge(
            &r,
            ("a.py", "line1\nmine\nline3\n"),
            ("a.py", "line1\ntheirs\nline3\n"),
        );
        repo.update(UpdateMethod::Rebase, false).unwrap();
        repo.abort_operation().unwrap();
        assert_eq!(repo.current_operation(), None);
        assert_eq!(repo.log(1).unwrap()[0].summary, "mine");
        assert_eq!(repo.current_branch().as_deref(), Some("main"));
    }

    #[test]
    fn merge_update_with_conflicts_continues_with_both_parents() {
        if !git_available() {
            return;
        }
        let r = remote_setup();
        let repo = diverge(
            &r,
            ("a.py", "line1\nmine\nline3\n"),
            ("a.py", "line1\ntheirs\nline3\n"),
        );
        // Uncommitted work in another file is autostashed and comes back after Continue.
        write(&r.work, "w.py", "wip\n");
        repo.commit(&[r.work.join("w.py")], "add w", false).unwrap();
        write(&r.work, "w.py", "wip changed\n");
        let out = repo.update(UpdateMethod::Merge, true).unwrap();
        assert_eq!(out.conflicts, vec![r.work.join("a.py")]);
        assert_eq!(out.stopped.as_deref(), Some("merge"));
        assert_eq!(
            repo.merge_message()
                .as_deref()
                .map(|m| m.starts_with("Merge")),
            Some(true)
        );
        repo.resolve_conflict(&r.work.join("a.py"), "line1\nboth\nline3\n")
            .unwrap();
        repo.continue_operation("").unwrap();
        assert_eq!(repo.current_operation(), None);
        let head = repo.log(1).unwrap().remove(0);
        assert_eq!(head.parents.len(), 2);
        assert!(head.summary.starts_with("Merge"), "{}", head.summary);
        assert_eq!(
            read(&r.work, "w.py"),
            "wip changed\n",
            "autostash re-applied"
        );
        assert!(repo.stash_list().unwrap().is_empty());
        assert!(!repo.repo.path().join("MERGE_AUTOSTASH").exists());
    }

    #[test]
    fn pull_into_current_from_another_remote_branch() {
        if !git_available() {
            return;
        }
        let r = remote_setup();
        let repo = GitRepo::discover(&r.work).unwrap();
        let out = repo
            .pull_into_current("origin/feature", UpdateMethod::Merge, true)
            .unwrap();
        assert!(out.fast_forward, "{out:?}");
        assert_eq!(repo.current_branch().as_deref(), Some("main"));
        assert_eq!(read(&r.work, "a.py"), "line1\nline2\nfeature3\n");
        assert!(r.work.join("f.py").exists());
        assert!(
            repo.pull_into_current("origin/feature", UpdateMethod::Rebase, true)
                .unwrap()
                .up_to_date
        );
        assert!(repo.update(UpdateMethod::Merge, true).is_ok());
        assert!(repo
            .pull_into_current("nonsense", UpdateMethod::Merge, true)
            .is_err());
    }

    #[test]
    fn failed_updates_report_gits_reason() {
        if !git_available() {
            return;
        }
        // The colleague changes a.py while we have an uncommitted edit of it; no autostash.
        let r = remote_setup();
        write(&r.other, "a.py", "line1\ntheirs\nline3\n");
        git(&r.other, &["commit", "-q", "-a", "-m", "theirs"]);
        git(&r.other, &["push", "-q", "origin", "main"]);
        let repo = GitRepo::discover(&r.work).unwrap();
        write(&r.work, "a.py", "line1\nmine\nline3\n");
        let err = repo.update(UpdateMethod::Merge, false).unwrap_err();
        assert!(
            last_line(&format!("{err:#}")).starts_with("Updating "),
            "the last line is git's progress line, not the reason: {err:#}"
        );
        let reason = failure_reason(&err);
        assert!(
            reason.starts_with(
                "Your local changes to the following files would be overwritten by merge:"
            ) && reason.ends_with(": a.py"),
            "{reason}"
        );
        let reason = failure_reason(&repo.update(UpdateMethod::Rebase, false).unwrap_err());
        assert!(reason.contains("unstaged changes"), "{reason}");
        assert_eq!(
            read(&r.work, "a.py"),
            "line1\nmine\nline3\n",
            "nothing was touched"
        );
        assert_eq!(repo.current_operation(), None);
        // Several files are named briefly; messages without git output are kept.
        let err = anyhow!("git merge x failed:\nerror: The following untracked working tree files would be overwritten by merge:\n\ta\n\tb\n\tc\n\td\n\te\nPlease move or remove them before you merge.\nAborting\n");
        assert_eq!(failure_reason(&err), "The following untracked working tree files would be overwritten by merge: a, b, c and 2 more");
        let err = anyhow!("git fetch origin failed:\nfatal: 'origin' does not appear to be a git repository\nfatal: Could not read from remote repository.\n");
        assert_eq!(
            failure_reason(&err),
            "'origin' does not appear to be a git repository"
        );
        assert_eq!(
            failure_reason(&anyhow!("the current branch has no upstream branch")),
            "the current branch has no upstream branch"
        );
    }
}
