//! RustPy IDE — Git integration via libgit2 (roadmap §4.1).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail};
use git2::{BranchType, Repository, RepositoryState, StatusOptions};
use serde::{Deserialize, Serialize};

pub mod askpass;
pub mod branches;
pub mod code_vision;
mod commit_workspace;
pub mod github;
pub mod gitignore_templates;
mod graph;
mod job;
pub mod local_history;
pub mod log_workspace;
pub mod mapping;
mod merge_text;
mod operation;
pub mod operation_inspection;
pub mod operations_workspace;
mod ops;
pub mod other_vcs;
pub mod policy;
pub mod provider;
mod push;
mod ssh_transport;
mod workflow;

pub use graph::GraphCommit;
pub use job::{
    describe_failure, git_command, git_program, is_auth_failure, set_git_program, GitEnv, GitJob,
};
pub use merge_text::merge_texts;
pub use operation::{running_operation, RepositoryOperation, RepositoryReservation};
pub use ops::{add_to_gitignore, BlameLine, FileDiff, MergeOutcome, ResetMode, StashEntry};
pub use push::{
    is_protected_branch, is_valid_branch_name, is_valid_remote_name, parse_push_porcelain,
    push_result_message, PushRequest, PushTags, PushTarget, PushedRef,
};
pub use workflow::{failure_reason, CheckoutBlockers, CheckoutOutcome, SyncOutcome, UpdateMethod};

/// Working-tree status of a file ("file status colors").
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FileStatus {
    Unmodified,
    Untracked,
    Added,
    Modified,
    Deleted,
    Renamed,
    Conflicted,
    Ignored,
}

impl FileStatus {
    pub fn label(self) -> &'static str {
        match self {
            FileStatus::Unmodified => "Unmodified",
            FileStatus::Untracked => "Unversioned",
            FileStatus::Added => "Added",
            FileStatus::Modified => "Modified",
            FileStatus::Deleted => "Deleted",
            FileStatus::Renamed => "Renamed",
            FileStatus::Conflicted => "Merge conflict",
            FileStatus::Ignored => "Ignored",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusEntry {
    /// The file's current path (the new path of a rename).
    pub path: PathBuf,
    pub status: FileStatus,
    /// The change is in the index. For a rename: the new path is tracked (as
    /// after `git mv`), not just a new file that looks like the deleted one.
    pub staged: bool,
    /// The previous path of a renamed or moved file.
    pub old_path: Option<PathBuf>,
}

impl StatusEntry {
    /// Whether the entry's file is not under version control yet: an untracked
    /// file, or a rename found only in the working tree (a deleted tracked file
    /// plus a similar new file nobody added). Like unversioned files, such
    /// entries are not included in a commit unless the user checks them.
    pub fn is_unversioned(&self) -> bool {
        self.status == FileStatus::Untracked || (self.status == FileStatus::Renamed && !self.staged)
    }
}

/// A changed region relative to HEAD, in current-document line numbers (0-based).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineChange {
    pub kind: ChangeKind,
    /// First changed line in the current text.
    pub start: usize,
    /// Number of lines in the current text (0 for pure deletions).
    pub count: usize,
    /// Lines removed from HEAD at this position.
    pub removed: usize,
    /// Original text of the removed/modified lines (for "Show Diff"/"Rollback").
    pub old_text: String,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitInfo {
    pub id: String,
    pub short_id: String,
    pub summary: String,
    pub message: String,
    pub author: String,
    pub email: String,
    /// Seconds since the epoch.
    pub time: i64,
    pub parents: Vec<String>,
    pub refs: Vec<String>,
}

impl CommitInfo {
    pub fn date_string(&self) -> String {
        chrono::DateTime::from_timestamp(self.time, 0)
            .map(|d| {
                d.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchInfo {
    pub name: String,
    pub is_remote: bool,
    pub is_current: bool,
    pub upstream: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitFile {
    pub path: PathBuf,
    pub status: FileStatus,
    pub additions: usize,
    pub deletions: usize,
}

/// A git repository containing the project.
pub struct GitRepo {
    command_env: GitEnv,
    repo: Repository,
    root: PathBuf,
    operation_key: PathBuf,
}

impl GitRepo {
    /// Finds the repository containing `path` (walking up).
    pub fn discover(path: &Path) -> Option<GitRepo> {
        let repo = Repository::discover(path).ok()?;
        let root = repo.workdir()?.to_path_buf();
        let operation_key = repo
            .commondir()
            .canonicalize()
            .unwrap_or_else(|_| repo.commondir().to_path_buf());
        Some(GitRepo {
            command_env: GitEnv::default(),
            repo,
            root,
            operation_key,
        })
    }

    pub fn init(path: &Path) -> anyhow::Result<GitRepo> {
        let _operation = RepositoryOperation::enter(path, "Git initialize")?;
        let repo = Repository::init(path)?;
        let root = repo
            .workdir()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.to_path_buf());
        let operation_key = repo
            .commondir()
            .canonicalize()
            .unwrap_or_else(|_| repo.commondir().to_path_buf());
        Ok(GitRepo {
            command_env: GitEnv::default(),
            repo,
            root,
            operation_key,
        })
    }

    /// Attach an IDE credential bridge to background repository workflows.
    pub fn with_git_environment(mut self, env: GitEnv) -> Self {
        self.command_env = env;
        self
    }

    /// The current operation, using the cached common-directory key for UI polling.
    pub fn running_operation(&self) -> Option<String> {
        operation::running_operation_key(&self.operation_key)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn is_shallow(&self) -> bool {
        self.repo.is_shallow()
    }

    fn rel(&self, path: &Path) -> Option<PathBuf> {
        path.strip_prefix(&self.root).ok().map(Path::to_path_buf)
    }

    /// Status of every changed/untracked file (ignored files excluded).
    pub fn status(&self) -> anyhow::Result<Vec<StatusEntry>> {
        let mut opts = StatusOptions::new();
        opts.include_untracked(true)
            .recurse_untracked_dirs(true)
            .include_ignored(false)
            .renames_head_to_index(true)
            .renames_index_to_workdir(true);
        let statuses = self.repo.statuses(Some(&mut opts))?;
        let mut out = Vec::new();
        for e in statuses.iter() {
            let s = e.status();
            let path = match e.path() {
                Ok(p) => self.root.join(p),
                Err(_) => continue,
            };
            let (status, staged) = if s.is_conflicted() {
                (FileStatus::Conflicted, false)
            } else if s.is_wt_new() || s.is_index_new() {
                (
                    if s.is_index_new() {
                        FileStatus::Added
                    } else {
                        FileStatus::Untracked
                    },
                    s.is_index_new(),
                )
            } else if s.is_wt_deleted() || s.is_index_deleted() {
                (
                    FileStatus::Deleted,
                    s.is_index_deleted() && !s.is_wt_deleted(),
                )
            } else if s.is_wt_renamed() || s.is_index_renamed() {
                // A working-tree rename pairs a deleted file with an untracked one.
                (
                    FileStatus::Renamed,
                    s.is_index_renamed() && !s.is_wt_renamed(),
                )
            } else if s.is_wt_modified()
                || s.is_index_modified()
                || s.is_wt_typechange()
                || s.is_index_typechange()
            {
                (
                    FileStatus::Modified,
                    s.is_index_modified() && !s.is_wt_modified(),
                )
            } else if s.is_ignored() {
                (FileStatus::Ignored, false)
            } else {
                continue;
            };
            // `StatusEntry::path` is the *old* side of a rename; report the file
            // under its new name and remember where it came from.
            let (path, old_path) = match status {
                FileStatus::Renamed => {
                    let (h2i, i2w) = (e.head_to_index(), e.index_to_workdir());
                    let new = i2w
                        .as_ref()
                        .and_then(|d| d.new_file().path())
                        .or_else(|| h2i.as_ref().and_then(|d| d.new_file().path()));
                    let old = h2i
                        .as_ref()
                        .and_then(|d| d.old_file().path())
                        .or_else(|| i2w.as_ref().and_then(|d| d.old_file().path()));
                    match (new, old) {
                        (Some(n), Some(o)) if n != o => {
                            (self.root.join(n), Some(self.root.join(o)))
                        }
                        _ => (path, None),
                    }
                }
                _ => (path, None),
            };
            out.push(StatusEntry {
                path,
                status,
                staged,
                old_path,
            });
        }
        Ok(out)
    }

    /// Status per path plus propagated status for parent directories
    /// (a directory shows Modified when any descendant changed).
    pub fn status_map(&self) -> HashMap<PathBuf, FileStatus> {
        let mut map: HashMap<PathBuf, FileStatus> = HashMap::new();
        for e in self.status().unwrap_or_default() {
            let mut parent = e.path.parent();
            map.insert(e.path.clone(), e.status);
            while let Some(d) = parent {
                if !d.starts_with(&self.root) || d == self.root {
                    break;
                }
                let cur = map.get(d).copied();
                let next = match (cur, e.status) {
                    (None, FileStatus::Untracked) => FileStatus::Untracked,
                    (Some(FileStatus::Untracked), FileStatus::Untracked) => FileStatus::Untracked,
                    _ => FileStatus::Modified,
                };
                map.insert(d.to_path_buf(), next);
                parent = d.parent();
            }
        }
        map
    }

    /// Whether `path` is ignored by `.gitignore`.
    pub fn is_ignored(&self, path: &Path) -> bool {
        self.rel(path)
            .map(|r| self.repo.is_path_ignored(r).unwrap_or(false))
            .unwrap_or(false)
    }

    /// Content of `path` in HEAD, if tracked.
    pub fn head_text(&self, path: &Path) -> Option<String> {
        self.head_bytes(path)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    /// Raw content of `path` in HEAD (binary-safe), if tracked.
    pub fn head_bytes(&self, path: &Path) -> Option<Vec<u8>> {
        let rel = self.rel(path)?;
        let head = self.repo.head().ok()?.peel_to_tree().ok()?;
        let entry = head.get_path(&rel).ok()?;
        let blob = self.repo.find_blob(entry.id()).ok()?;
        Some(blob.content().to_vec())
    }

    /// Gutter change markers: diff of HEAD vs `current` (LF-normalized text).
    pub fn line_changes(&self, path: &Path, current: &str) -> Vec<LineChange> {
        match self.head_text(path) {
            Some(old) => line_changes(&old, current),
            None => Vec::new(),
        }
    }

    pub fn current_branch(&self) -> Option<String> {
        let head = self.repo.head().ok()?;
        if head.is_branch() {
            head.shorthand().ok().map(str::to_owned)
        } else {
            let oid = head.target()?;
            Some(format!("HEAD detached at {}", &oid.to_string()[..7]))
        }
    }

    pub fn branches(&self) -> Vec<BranchInfo> {
        let current = self.current_branch();
        let mut out = Vec::new();
        for ty in [BranchType::Local, BranchType::Remote] {
            if let Ok(iter) = self.repo.branches(Some(ty)) {
                for (b, _) in iter.flatten() {
                    let Some(name) = b.name().ok().flatten() else {
                        continue;
                    };
                    if name.ends_with("/HEAD") {
                        continue;
                    }
                    let upstream = b
                        .upstream()
                        .ok()
                        .and_then(|u| u.name().ok().flatten().map(str::to_owned));
                    out.push(BranchInfo {
                        name: name.to_string(),
                        is_remote: ty == BranchType::Remote,
                        is_current: current.as_deref() == Some(name),
                        upstream,
                    });
                }
            }
        }
        out.sort_by(|a, b| {
            a.is_remote
                .cmp(&b.is_remote)
                .then_with(|| a.name.cmp(&b.name))
        });
        out
    }

    /// Creates a branch at HEAD (see [`GitRepo::create_branch_from`]).
    pub fn create_branch(&self, name: &str, checkout: bool) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git create branch")?;
        self.create_branch_from(name, "HEAD", checkout)
    }

    /// The full reference name of the branch `name` as the Branches popup lists
    /// it: `refs/heads/<name>` for a local branch, `refs/remotes/<name>` for a
    /// remote-tracking one, otherwise `name` unchanged (a tag, a commit, `HEAD`).
    /// Git's revision lookup tries `refs/tags/<name>` before the branches, so a
    /// tag named like a branch would otherwise be checked out, merged or compared
    /// instead of the branch.
    pub fn branch_spec(&self, name: &str) -> String {
        for full in [format!("refs/heads/{name}"), format!("refs/remotes/{name}")] {
            if self.repo.find_reference(&full).is_ok() {
                return full;
            }
        }
        name.to_string()
    }

    /// `name` as an argument for the git CLI: the short name, which gives git's
    /// usual messages and conflict labels (`origin/main`), unless git would
    /// resolve it to another commit than the branch (a tag of the same name);
    /// then the full reference name.
    pub(crate) fn branch_cli_arg(&self, name: &str) -> String {
        let spec = self.branch_spec(name);
        let commit = |rev: &str| {
            self.repo
                .revparse_single(rev)
                .ok()
                .and_then(|o| o.peel_to_commit().ok())
                .map(|c| c.id())
        };
        if spec != name && commit(name) != commit(&spec) {
            spec
        } else {
            name.to_string()
        }
    }

    /// Checks out `name`. A local branch wins over a tag or revision of the same
    /// name (like `git checkout`) and HEAD is attached to it; anything else (a
    /// tag, a commit, a remote-tracking branch) is checked out detached.
    pub fn checkout_branch(&self, name: &str) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git checkout branch")?;
        let (object, reference) = self.repo.revparse_ext(&self.branch_spec(name))?;
        self.repo
            .checkout_tree(&object, Some(git2::build::CheckoutBuilder::new().safe()))?;
        match reference {
            Some(r) => self.repo.set_head(r.name().unwrap_or(name))?,
            None => self.repo.set_head_detached(object.id())?,
        }
        Ok(())
    }

    /// Recent commits reachable from HEAD, newest first.
    pub fn log(&self, limit: usize) -> anyhow::Result<Vec<CommitInfo>> {
        let mut walk = self.repo.revwalk()?;
        walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
        if walk.push_head().is_err() {
            return Ok(Vec::new());
        }
        // Decorations: branch/tag names per commit.
        let mut refs: HashMap<git2::Oid, Vec<String>> = HashMap::new();
        if let Ok(iter) = self.repo.references() {
            for r in iter.flatten() {
                if let (Ok(name), Some(oid)) = (r.shorthand(), r.target()) {
                    if name != "HEAD" {
                        refs.entry(oid).or_default().push(name.to_string());
                    }
                }
            }
        }
        let mut out = Vec::new();
        for oid in walk.take(limit).flatten() {
            let c = self.repo.find_commit(oid)?;
            let id = oid.to_string();
            out.push(CommitInfo {
                short_id: id[..7.min(id.len())].to_string(),
                id,
                summary: c.summary().ok().flatten().unwrap_or("").to_string(),
                message: c.message().unwrap_or("").to_string(),
                author: c.author().name().unwrap_or("").to_string(),
                email: c.author().email().unwrap_or("").to_string(),
                time: c.time().seconds(),
                parents: c.parent_ids().map(|p| p.to_string()).collect(),
                refs: refs.get(&oid).cloned().unwrap_or_default(),
            });
        }
        Ok(out)
    }

    /// Files changed by a commit, with line counts.
    pub fn commit_files(&self, id: &str) -> anyhow::Result<Vec<CommitFile>> {
        let oid = git2::Oid::from_str(id)?;
        let commit = self.repo.find_commit(oid)?;
        let tree = commit.tree()?;
        let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
        let diff = self
            .repo
            .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)?;
        let mut out = Vec::new();
        for (i, d) in diff.deltas().enumerate() {
            let path = d
                .new_file()
                .path()
                .or_else(|| d.old_file().path())
                .map(|p| self.root.join(p))
                .unwrap_or_default();
            let status = match d.status() {
                git2::Delta::Added => FileStatus::Added,
                git2::Delta::Deleted => FileStatus::Deleted,
                git2::Delta::Renamed => FileStatus::Renamed,
                _ => FileStatus::Modified,
            };
            let (mut additions, mut deletions) = (0, 0);
            if let Ok(Some(patch)) = git2::Patch::from_diff(&diff, i) {
                if let Ok((_, a, dl)) = patch.line_stats() {
                    additions = a;
                    deletions = dl;
                }
            }
            out.push(CommitFile {
                path,
                status,
                additions,
                deletions,
            });
        }
        Ok(out)
    }

    /// Unified diff text of one file in a commit.
    pub fn commit_file_diff(&self, id: &str, path: &Path) -> anyhow::Result<String> {
        let oid = git2::Oid::from_str(id)?;
        let commit = self.repo.find_commit(oid)?;
        let tree = commit.tree()?;
        let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
        let rel = self.rel(path).unwrap_or_else(|| path.to_path_buf());
        let mut opts = git2::DiffOptions::new();
        opts.pathspec(rel);
        let diff =
            self.repo
                .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))?;
        let mut text = String::new();
        diff.print(git2::DiffFormat::Patch, |_, _, line| {
            let prefix = match line.origin() {
                '+' | '-' | ' ' => line.origin().to_string(),
                _ => String::new(),
            };
            text.push_str(&prefix);
            text.push_str(&String::from_utf8_lossy(line.content()));
            true
        })?;
        Ok(text)
    }

    /// Commits the working-tree state of `paths` with `message` (`git commit --only`).
    ///
    /// The new tree is HEAD's tree plus the selected paths, so changes staged
    /// outside the IDE stay out of the commit unless selected; the selected paths
    /// are staged in the real index as well. A rename is one change: selecting
    /// either its old or its new path commits the deletion and the addition.
    ///
    /// While a merge, cherry-pick, revert or rebase is in progress the whole
    /// index is committed (git refuses partial commits there) and an empty
    /// `message` falls back to the prepared `MERGE_MSG`. A merge commit gets HEAD
    /// plus every `MERGE_HEAD` as parents, a cherry-pick keeps the picked
    /// commit's author, and the merge/cherry-pick/revert state is cleared.
    pub fn commit(&self, paths: &[PathBuf], message: &str, amend: bool) -> anyhow::Result<String> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git commit")?;
        let state = self.repo.state();
        let in_operation = !matches!(state, RepositoryState::Clean | RepositoryState::Bisect);
        if amend && in_operation {
            bail!(
                "cannot amend while a {} is in progress",
                self.current_operation().unwrap_or_default()
            );
        }
        let mut rels: Vec<PathBuf> = paths.iter().filter_map(|p| self.rel(p)).collect();
        for e in self.status().unwrap_or_default() {
            let (Some(new), Some(old)) = (
                self.rel(&e.path),
                e.old_path.as_deref().and_then(|o| self.rel(o)),
            ) else {
                continue;
            };
            if rels.contains(&new) || rels.contains(&old) {
                for r in [old, new] {
                    if !rels.contains(&r) {
                        rels.push(r);
                    }
                }
            }
        }
        let mut index = self.repo.index()?;
        // Pick up index changes made by the git CLI.
        index.read(false)?;
        if index.has_conflicts() {
            bail!("resolve the merge conflicts before committing");
        }
        for rel in &rels {
            match std::fs::symlink_metadata(self.root.join(rel)) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => index.add_path(rel)?,
                Err(_) => {
                    let _ = index.remove_path(rel);
                }
            }
        }
        index.write()?;
        let head = self.repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let tree_oid = if in_operation {
            index.write_tree()?
        } else {
            let mut partial = git2::Index::new()?;
            if let Some(tree) = head.as_ref().and_then(|c| c.tree().ok()) {
                partial.read_tree(&tree)?;
            }
            for rel in &rels {
                match index.get_path(rel, 0) {
                    Some(entry) => partial.add(&entry)?,
                    None => {
                        let _ = partial.remove_path(rel);
                    }
                }
            }
            partial.write_tree_to(&self.repo)?
        };
        let tree = self.repo.find_tree(tree_oid)?;
        let sig = self.signature()?;
        let message = if message.trim().is_empty() && in_operation {
            self.merge_message().unwrap_or_default()
        } else {
            message.to_string()
        };
        let oid = if amend {
            let head = head.ok_or_else(|| anyhow!("nothing to amend"))?;
            // An empty message keeps the amended commit's message, and the
            // author stays (like `git commit --amend`); the committer is us.
            let message = Some(message.as_str()).filter(|m| !m.trim().is_empty());
            head.amend(Some("HEAD"), None, Some(&sig), None, message, Some(&tree))?
        } else {
            if message.trim().is_empty() {
                bail!("the commit message is empty");
            }
            let mut parents: Vec<git2::Commit> = head.into_iter().collect();
            let mut author = sig.clone();
            match state {
                RepositoryState::Merge => {
                    for oid in self.merge_heads()? {
                        parents.push(self.repo.find_commit(oid)?);
                    }
                }
                RepositoryState::CherryPick | RepositoryState::CherryPickSequence => {
                    if let Some(picked) = self
                        .repo
                        .find_reference("CHERRY_PICK_HEAD")
                        .ok()
                        .and_then(|r| r.peel_to_commit().ok())
                    {
                        author = picked.author().to_owned();
                    }
                }
                _ => {}
            }
            let parent_refs: Vec<&git2::Commit> = parents.iter().collect();
            self.repo
                .commit(Some("HEAD"), &author, &sig, &message, &tree, &parent_refs)?
        };
        // A rebase or a multi-commit sequence continues through the git CLI.
        if matches!(
            state,
            RepositoryState::Merge | RepositoryState::CherryPick | RepositoryState::Revert
        ) {
            self.repo.cleanup_state()?;
        }
        if state == RepositoryState::Merge {
            self.restore_merge_autostash();
        }
        Ok(oid.to_string())
    }

    /// The commits being merged in (`MERGE_HEAD`).
    pub(crate) fn merge_heads(&self) -> anyhow::Result<Vec<git2::Oid>> {
        let mut heads = Vec::new();
        let mut handle = Repository::open(&self.root)?;
        match handle.mergehead_foreach(|oid| {
            heads.push(*oid);
            true
        }) {
            Ok(()) => Ok(heads),
            Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
    }

    /// The prepared message of an interrupted merge, cherry-pick or revert
    /// (`.git/MERGE_MSG`) without its `#` comment lines.
    pub fn merge_message(&self) -> Option<String> {
        let raw = self.repo.message().ok()?;
        let text = raw
            .lines()
            .filter(|l| !l.starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_string())
    }

    fn signature(&self) -> anyhow::Result<git2::Signature<'static>> {
        if let Ok(sig) = self.repo.signature() {
            return Ok(sig.to_owned());
        }
        let name = std::env::var("GIT_AUTHOR_NAME").unwrap_or_else(|_| "RustPy User".into());
        let email = std::env::var("GIT_AUTHOR_EMAIL").unwrap_or_else(|_| "rustpy@localhost".into());
        Ok(git2::Signature::now(&name, &email)?)
    }

    /// Reverts a file to its HEAD content (Rollback), in the working tree and the
    /// index (like `git checkout HEAD -- <path>`). A renamed file is rolled back
    /// as a whole: its old path comes back and the new one goes.
    pub fn rollback(&self, path: &Path) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git rollback")?;
        let mut paths = vec![path.to_path_buf()];
        if let Some(old) = self.renamed_from(path) {
            self.rollback_file(&old)?;
            paths.push(old);
        }
        self.rollback_file(path)?;
        self.reset_index_to_head(&paths)
    }

    /// Rollback that keeps files which are not in HEAD: a new file is only
    /// unstaged, and a renamed file gets its old path back from HEAD while the
    /// new file stays as an unversioned copy.
    pub fn rollback_keeping_new(&self, path: &Path) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git rollback keeping new")?;
        let mut paths = vec![path.to_path_buf()];
        if let Some(old) = self.renamed_from(path) {
            self.rollback_file(&old)?;
            paths.push(old);
        }
        self.reset_index_to_head(&paths)
    }

    /// The old path of `path` when Git sees it as renamed.
    fn renamed_from(&self, path: &Path) -> Option<PathBuf> {
        self.status()
            .unwrap_or_default()
            .into_iter()
            .find(|e| e.path == path)
            .and_then(|e| e.old_path)
    }

    /// Resets the index entries of `paths` to HEAD (entries not in HEAD are removed).
    fn reset_index_to_head(&self, paths: &[PathBuf]) -> anyhow::Result<()> {
        let rels: Vec<PathBuf> = paths.iter().filter_map(|p| self.rel(p)).collect();
        if rels.is_empty() {
            return Ok(());
        }
        match self.repo.head().ok().and_then(|h| h.peel_to_commit().ok()) {
            Some(head) => self
                .repo
                .reset_default(Some(head.as_object()), rels.iter().map(PathBuf::as_path))?,
            None => {
                let mut index = self.repo.index()?;
                for rel in &rels {
                    let _ = index.remove_path(rel);
                }
                index.write()?;
            }
        }
        Ok(())
    }

    fn rollback_file(&self, path: &Path) -> anyhow::Result<()> {
        // Bytes, not text: a binary file must come back byte for byte.
        match self.head_bytes(path) {
            Some(bytes) => {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(path, bytes)?;
                Ok(())
            }
            None => {
                // Untracked / added: delete.
                if path.exists() {
                    std::fs::remove_file(path)?;
                }
                Ok(())
            }
        }
    }

    /// Records that `from` (a file or a folder) was renamed or moved to `to` by
    /// the IDE, like `git mv` after the fact: the index entries move to the new
    /// path with their staged content, so Local Changes shows one rename that is
    /// committed as such. Untracked files and conflicted entries are left alone;
    /// the working tree is not touched. Returns the number of entries moved.
    pub fn track_move(&self, from: &Path, to: &Path) -> anyhow::Result<usize> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git track move")?;
        let (Some(from_rel), Some(to_rel)) = (self.rel(from), self.rel(to)) else {
            return Ok(0);
        };
        if from_rel.as_os_str().is_empty() || to_rel.as_os_str().is_empty() {
            return Ok(0);
        }
        let mut index = self.repo.index()?;
        // Pick up index changes made by the git CLI.
        index.read(false)?;
        let entries: Vec<git2::IndexEntry> = index.iter().collect();
        let stage = |e: &git2::IndexEntry| (e.flags >> 12) & 0x3;
        let conflicted: std::collections::HashSet<Vec<u8>> = entries
            .iter()
            .filter(|e| stage(e) != 0)
            .map(|e| e.path.clone())
            .collect();
        let mut moved = 0;
        for mut entry in entries {
            if stage(&entry) != 0 || conflicted.contains(&entry.path) {
                continue;
            }
            let rel = PathBuf::from(String::from_utf8_lossy(&entry.path).into_owned());
            let Ok(rest) = rel.strip_prefix(&from_rel) else {
                continue;
            };
            let new_rel = if rest.as_os_str().is_empty() {
                to_rel.clone()
            } else {
                to_rel.join(rest)
            };
            // Only where the file really is now (the IDE moved it).
            if !self.root.join(&new_rel).is_file() || self.root.join(&rel).exists() {
                continue;
            }
            let old_path = entry.path.clone();
            entry.path = new_rel.to_string_lossy().replace('\\', "/").into_bytes();
            index.add(&entry)?;
            index.remove_path(Path::new(&String::from_utf8_lossy(&old_path).into_owned()))?;
            moved += 1;
        }
        if moved > 0 {
            index.write()?;
        }
        Ok(moved)
    }

    /// Removes `path` from the index and keeps the working-tree file: rolls back a
    /// staged addition without deleting the local copy.
    pub fn unstage(&self, path: &Path) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git unstage")?;
        let rel = self
            .rel(path)
            .ok_or_else(|| anyhow::anyhow!("{} is outside the repository", path.display()))?;
        match self.repo.head().ok().and_then(|h| h.peel_to_commit().ok()) {
            Some(head) => self
                .repo
                .reset_default(Some(head.as_object()), [rel.as_path()])?,
            None => {
                let mut index = self.repo.index()?;
                index.remove_path(&rel)?;
                index.write()?;
            }
        }
        Ok(())
    }

    /// Last commit message (for the commit dialog's history / amend).
    pub fn head_message(&self) -> Option<String> {
        self.repo
            .head()
            .ok()?
            .peel_to_commit()
            .ok()?
            .message()
            .ok()
            .map(str::to_owned)
    }

    /// Remote tracking info: `(ahead, behind)` relative to upstream.
    pub fn ahead_behind(&self) -> Option<(usize, usize)> {
        let head = self.repo.head().ok()?;
        let local = head.target()?;
        let branch = self
            .repo
            .find_branch(head.shorthand().ok()?, BranchType::Local)
            .ok()?;
        let upstream = branch.upstream().ok()?.get().target()?;
        self.repo.graph_ahead_behind(local, upstream).ok()
    }

    /// Runs the `git` CLI in the repository and waits (local operations such
    /// as merge; network operations run as a [`GitJob`]).
    pub fn run_git(&self, args: &[&str]) -> anyhow::Result<String> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git run git")?;
        let owned: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        if let Some(result) = ssh_transport::run_inline(&self.root, &owned, &self.command_env) {
            return result;
        }
        let out = git_command(&self.root, &owned, &self.command_env).output()?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        if out.status.success() {
            Ok(format!("{stdout}{stderr}"))
        } else {
            anyhow::bail!("git {} failed:\n{stderr}{stdout}", args.join(" "))
        }
    }

    pub fn remotes(&self) -> Vec<String> {
        self.repo
            .remotes()
            .map(|r| {
                (0..r.len())
                    .filter_map(|i| r.get(i).ok().flatten())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Upstream (`origin/main`) of the current branch, if one is configured.
    pub fn upstream_of_current(&self) -> Option<String> {
        let head = self.repo.head().ok()?;
        let name = head.shorthand().ok()?.to_string();
        let branch = self.repo.find_branch(&name, git2::BranchType::Local).ok()?;
        let up = branch.upstream().ok()?;
        up.name().ok().flatten().map(str::to_owned)
    }

    /// `git push` arguments: sets the upstream on the first push of a branch
    /// (to `origin`, or the only remote) instead of failing.
    pub fn push_args(&self) -> Vec<String> {
        if self.upstream_of_current().is_some() {
            return vec!["push".into()];
        }
        let remotes = self.remotes();
        let remote = if remotes.iter().any(|r| r == "origin") {
            Some("origin".to_string())
        } else {
            remotes.first().cloned()
        };
        match (remote, self.current_branch()) {
            (Some(r), Some(b)) => vec!["push".into(), "--set-upstream".into(), r, b],
            _ => vec!["push".into()],
        }
    }

    /// Runs `git <args>` in `root` without terminal prompts (for background threads).
    pub fn run_git_in(root: &std::path::Path, args: &[String]) -> anyhow::Result<String> {
        Self::run_git_in_with_environment(root, args, &GitEnv::default())
    }

    pub fn run_git_in_with_environment(
        root: &Path,
        args: &[String],
        env: &GitEnv,
    ) -> anyhow::Result<String> {
        let _operation = RepositoryOperation::enter(root, format!("git {}", args.join(" ")))?;
        if let Some(result) = ssh_transport::run_inline(root, args, env) {
            return result;
        }
        let out = git_command(root, args, env).output()?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        if out.status.success() {
            Ok(format!("{stdout}{stderr}"))
        } else {
            anyhow::bail!("git {} failed:\n{stderr}{stdout}", args.join(" "))
        }
    }

    /// [`GitRepo::run_git_in`] for background tasks (push, fetch, pull): the
    /// `git` process is killed as soon as `token` is cancelled. Errors carry
    /// the same text as `run_git_in`'s.
    pub fn run_git_cancellable(
        root: &std::path::Path,
        args: &[String],
        token: &rustpy_core::CancellationToken,
    ) -> Result<String, rustpy_core::TaskError> {
        if token.is_cancelled() {
            return Err(rustpy_core::TaskError::Cancelled);
        }
        let _operation = RepositoryOperation::enter(root, format!("git {}", args.join(" ")))
            .map_err(|e| rustpy_core::TaskError::Failed(e.to_string()))?;
        if let Some(result) = ssh_transport::run_cancellable(root, args, &GitEnv::default(), token)
        {
            return result;
        }
        let mut cmd = git_command(root, args, &GitEnv::default());
        let out = rustpy_core::process::output_cancellable(&mut cmd, token)?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        if out.status.success() {
            Ok(format!("{stdout}{stderr}"))
        } else {
            Err(rustpy_core::TaskError::Failed(format!(
                "git {} failed:\n{stderr}{stdout}",
                args.join(" ")
            )))
        }
    }
}

/// Line-level diff between `old` and `new` texts (for gutter markers).
pub fn line_changes(old: &str, new: &str) -> Vec<LineChange> {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_lines(old, new);
    let mut out = Vec::new();
    for group in diff.grouped_ops(0) {
        for op in group {
            let (removed, added): (Vec<_>, Vec<_>) =
                diff.iter_changes(&op)
                    .fold((Vec::new(), Vec::new()), |(mut r, mut a), c| {
                        match c.tag() {
                            ChangeTag::Delete => r.push(c),
                            ChangeTag::Insert => a.push(c),
                            ChangeTag::Equal => {}
                        }
                        (r, a)
                    });
            if removed.is_empty() && added.is_empty() {
                continue;
            }
            let old_text: String = removed
                .iter()
                .map(|c| c.to_string_lossy().into_owned())
                .collect();
            let start = added
                .first()
                .and_then(|c| c.new_index())
                .or(match op {
                    similar::DiffOp::Delete { new_index, .. } => Some(new_index),
                    _ => None,
                })
                .unwrap_or(0);
            let kind = if added.is_empty() {
                ChangeKind::Deleted
            } else if removed.is_empty() {
                ChangeKind::Added
            } else {
                ChangeKind::Modified
            };
            out.push(LineChange {
                kind,
                start,
                count: added.len(),
                removed: removed.len(),
                old_text,
            });
        }
    }
    out
}

impl LineChange {
    /// The document line whose gutter shows this change's marker (a deletion is
    /// marked on the line after the removed lines, clamped to the last line).
    pub fn marker_line(&self, line_count: usize) -> usize {
        match self.kind {
            ChangeKind::Deleted => self.start.min(line_count.saturating_sub(1)),
            _ => self.start,
        }
    }

    /// Whether `line`'s gutter marker belongs to this change.
    pub fn covers_line(&self, line: usize, line_count: usize) -> bool {
        match self.kind {
            ChangeKind::Deleted => line == self.marker_line(line_count),
            _ => line >= self.start && line < self.start + self.count,
        }
    }
}

/// Index of the change whose gutter marker is on `line`.
pub fn change_at_line(changes: &[LineChange], line: usize, line_count: usize) -> Option<usize> {
    changes.iter().position(|c| c.covers_line(line, line_count))
}

/// Index of the first change starting after `line` (`forward`) or the last one
/// before it, as for Next/Previous Change; `None` at either end.
pub fn adjacent_change(
    changes: &[LineChange],
    line: usize,
    forward: bool,
    line_count: usize,
) -> Option<usize> {
    if forward {
        changes
            .iter()
            .position(|c| c.marker_line(line_count) > line)
    } else {
        changes
            .iter()
            .rposition(|c| c.marker_line(line_count) < line && !c.covers_line(line, line_count))
    }
}

/// Rollback Lines: the byte range of `current` (LF line endings) that `change`
/// covers and the HEAD text that replaces it.
pub fn rollback_range(current: &str, change: &LineChange) -> (std::ops::Range<usize>, String) {
    // Start of document line `n` (lines as in an editor: a trailing newline opens an empty last line).
    let line_start = |n: usize| -> usize {
        if n == 0 {
            return 0;
        }
        current
            .match_indices('\n')
            .nth(n - 1)
            .map(|(i, _)| i + 1)
            .unwrap_or(current.len())
    };
    let start = line_start(change.start);
    let end = if change.count == 0 {
        start
    } else {
        line_start(change.start + change.count)
    };
    (start..end, change.old_text.replace("\r\n", "\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> (tempfile::TempDir, GitRepo) {
        let dir = tempfile::tempdir().unwrap();
        let r = GitRepo::init(dir.path()).unwrap();
        r.repo
            .config()
            .unwrap()
            .set_str("user.name", "Test")
            .unwrap();
        r.repo
            .config()
            .unwrap()
            .set_str("user.email", "t@example.com")
            .unwrap();
        std::fs::write(dir.path().join("a.py"), "line1\nline2\nline3\n").unwrap();
        r.commit(&[dir.path().join("a.py")], "initial", false)
            .unwrap();
        (dir, r)
    }

    #[test]
    fn rollback_restores_binary_files_byte_for_byte() {
        let (dir, r) = repo();
        let bytes: Vec<u8> = (0..=255u8).chain([0xff, 0xfe, 0x00, 0x80]).collect();
        let file = dir.path().join("image.bin");
        std::fs::write(&file, &bytes).unwrap();
        r.commit(std::slice::from_ref(&file), "add binary", false)
            .unwrap();
        std::fs::write(&file, b"changed").unwrap();
        r.rollback(&file).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), bytes);
    }

    #[test]
    fn status_commit_log_and_branches() {
        let (dir, r) = repo();
        assert_eq!(r.status().unwrap().len(), 0);
        std::fs::write(dir.path().join("a.py"), "line1\nchanged\nline3\nline4\n").unwrap();
        std::fs::write(dir.path().join("b.py"), "new\n").unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/c.py"), "x\n").unwrap();
        let st = r.status().unwrap();
        let find = |n: &str| st.iter().find(|e| e.path.ends_with(n)).map(|e| e.status);
        assert_eq!(find("a.py"), Some(FileStatus::Modified));
        assert_eq!(find("b.py"), Some(FileStatus::Untracked));
        let map = r.status_map();
        assert_eq!(
            map.get(&dir.path().join("sub")).copied(),
            Some(FileStatus::Untracked)
        );
        let changes = r.line_changes(&dir.path().join("a.py"), "line1\nchanged\nline3\nline4\n");
        assert_eq!(changes.len(), 2);
        assert_eq!(
            changes[0],
            LineChange {
                kind: ChangeKind::Modified,
                start: 1,
                count: 1,
                removed: 1,
                old_text: "line2\n".into()
            }
        );
        assert_eq!(changes[1].kind, ChangeKind::Added);
        assert_eq!(changes[1].start, 3);
        let id = r
            .commit(
                &[dir.path().join("a.py"), dir.path().join("b.py")],
                "second\n\nbody",
                false,
            )
            .unwrap();
        let log = r.log(10).unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].summary, "second");
        assert_eq!(log[0].id, id);
        assert!(log[0].refs.iter().any(|r| r == "master" || r == "main"));
        let files = r.commit_files(&id).unwrap();
        assert_eq!(files.len(), 2);
        assert!(r
            .commit_file_diff(&id, &dir.path().join("a.py"))
            .unwrap()
            .contains("+changed"));
        assert!(r.current_branch().is_some());
        r.create_branch("feature", true).unwrap();
        assert_eq!(r.current_branch().as_deref(), Some("feature"));
        let branches = r.branches();
        assert!(branches.iter().any(|b| b.name == "feature" && b.is_current));
        r.rollback(&dir.path().join("sub/c.py")).unwrap();
        assert!(!dir.path().join("sub/c.py").exists());
        assert!(r.head_message().unwrap().starts_with("second"));
    }

    #[test]
    fn unstage_keeps_the_local_copy() {
        let (dir, r) = repo();
        let f = dir.path().join("new.py");
        std::fs::write(&f, "x = 1\n").unwrap();
        let mut index = r.repo.index().unwrap();
        index.add_path(Path::new("new.py")).unwrap();
        index.write().unwrap();
        let status_of = |r: &GitRepo| {
            r.status()
                .unwrap()
                .into_iter()
                .find(|e| e.path.ends_with("new.py"))
                .map(|e| e.status)
        };
        assert_eq!(status_of(&r), Some(FileStatus::Added));
        r.unstage(&r.root().join("new.py")).unwrap();
        assert!(f.exists(), "the file is kept");
        assert_eq!(status_of(&r), Some(FileStatus::Untracked));
    }

    #[test]
    fn pure_deletion_change() {
        let c = line_changes("a\nb\nc\n", "a\nc\n");
        assert_eq!(
            c,
            vec![LineChange {
                kind: ChangeKind::Deleted,
                start: 1,
                count: 0,
                removed: 1,
                old_text: "b\n".into()
            }]
        );
    }

    fn head_paths(r: &GitRepo) -> Vec<String> {
        let tree = r.repo.head().unwrap().peel_to_tree().unwrap();
        let mut out = Vec::new();
        tree.walk(git2::TreeWalkMode::PreOrder, |dir, e| {
            if e.kind() == Some(git2::ObjectType::Blob) {
                out.push(format!("{dir}{}", e.name().unwrap_or("")));
            }
            git2::TreeWalkResult::Ok
        })
        .unwrap();
        out.sort();
        out
    }

    #[test]
    fn working_tree_rename_commits_both_paths() {
        let (dir, r) = repo();
        std::fs::write(dir.path().join("b.py"), "b\n").unwrap();
        r.commit(&[dir.path().join("b.py")], "add b", false)
            .unwrap();
        // Project-view rename and a Move refactoring into a package.
        std::fs::rename(dir.path().join("a.py"), dir.path().join("c.py")).unwrap();
        std::fs::create_dir_all(dir.path().join("pkg")).unwrap();
        std::fs::rename(dir.path().join("b.py"), dir.path().join("pkg/b.py")).unwrap();
        let st = r.status().unwrap();
        assert_eq!(st.len(), 2, "{st:?}");
        let c = st
            .iter()
            .find(|e| e.path == dir.path().join("c.py"))
            .expect("listed under the new name");
        assert_eq!(
            (c.status, c.old_path.clone()),
            (FileStatus::Renamed, Some(dir.path().join("a.py")))
        );
        let b = st
            .iter()
            .find(|e| e.path == dir.path().join("pkg/b.py"))
            .unwrap();
        assert_eq!(b.old_path, Some(dir.path().join("b.py")));
        // Selecting the entries (their new paths) commits deletion and addition.
        r.commit(
            &[dir.path().join("c.py"), dir.path().join("pkg/b.py")],
            "rename",
            false,
        )
        .unwrap();
        assert_eq!(head_paths(&r), vec!["c.py", "pkg/b.py"]);
        assert!(
            r.status().unwrap().is_empty(),
            "nothing left unversioned: {:?}",
            r.status().unwrap()
        );
        // Selecting only the old path of a rename works too.
        std::fs::rename(dir.path().join("c.py"), dir.path().join("d.py")).unwrap();
        r.commit(&[dir.path().join("c.py")], "rename again", false)
            .unwrap();
        assert_eq!(head_paths(&r), vec!["d.py", "pkg/b.py"]);
        assert!(r.status().unwrap().is_empty());
        // Rolling back a rename restores the old file and removes the new one.
        std::fs::rename(dir.path().join("pkg/b.py"), dir.path().join("e.py")).unwrap();
        std::fs::remove_dir(dir.path().join("pkg")).unwrap();
        r.rollback(&dir.path().join("e.py")).unwrap();
        assert!(!dir.path().join("e.py").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("pkg/b.py")).unwrap(),
            "b\n"
        );
        assert!(r.status().unwrap().is_empty());
    }

    #[test]
    fn unchecked_staged_files_stay_out_of_the_commit() {
        let (dir, r) = repo();
        // `git add` outside the IDE.
        std::fs::write(dir.path().join("staged.py"), "s\n").unwrap();
        let mut index = r.repo.index().unwrap();
        index.add_path(Path::new("staged.py")).unwrap();
        index.write().unwrap();
        std::fs::write(dir.path().join("a.py"), "line1\nchanged\nline3\n").unwrap();
        r.commit(&[dir.path().join("a.py")], "only a", false)
            .unwrap();
        assert_eq!(head_paths(&r), vec!["a.py"]);
        assert_eq!(
            r.head_text(&dir.path().join("a.py")).unwrap(),
            "line1\nchanged\nline3\n"
        );
        let st = r.status().unwrap();
        assert_eq!(st.len(), 1);
        assert_eq!(
            (st[0].status, st[0].staged),
            (FileStatus::Added, true),
            "still staged for a later commit"
        );
        // Amending without a message keeps the message.
        r.commit(&[dir.path().join("staged.py")], "", true).unwrap();
        assert_eq!(r.head_message().unwrap(), "only a");
        assert_eq!(head_paths(&r), vec!["a.py", "staged.py"]);
    }

    #[test]
    fn commit_during_a_merge_creates_the_merge_commit() {
        let (dir, r) = repo();
        let main = r.current_branch().unwrap();
        std::fs::write(dir.path().join("b.py"), "b\n").unwrap();
        r.commit(&[dir.path().join("b.py")], "add b", false)
            .unwrap();
        r.create_branch("feature", true).unwrap();
        std::fs::write(dir.path().join("a.py"), "line1\ntheirs\nline3\n").unwrap();
        r.commit(&[dir.path().join("a.py")], "theirs", false)
            .unwrap();
        r.checkout_branch(&main).unwrap();
        std::fs::write(dir.path().join("a.py"), "line1\nours\nline3\n").unwrap();
        r.commit(&[dir.path().join("a.py")], "ours", false).unwrap();
        let out = r.merge_branch("feature").unwrap();
        assert_eq!(out.conflicts.len(), 1);
        assert!(
            r.commit(&[dir.path().join("a.py")], "too early", false)
                .is_err(),
            "conflicts must be resolved first"
        );
        assert!(
            r.merge_message().unwrap().starts_with("Merge"),
            "{:?}",
            r.merge_message()
        );
        r.resolve_conflict(&dir.path().join("a.py"), "line1\nboth\nline3\n")
            .unwrap();
        // An unrelated, unselected edit stays in the working tree.
        std::fs::write(dir.path().join("b.py"), "b changed\n").unwrap();
        assert!(r.commit(&[], "x", true).is_err(), "no amend during a merge");
        let id = r
            .commit(&[dir.path().join("a.py")], "Merge feature", false)
            .unwrap();
        let head = r.log(1).unwrap().remove(0);
        assert_eq!(head.id, id);
        assert_eq!(head.parents.len(), 2, "the merged branch stays a parent");
        assert_eq!(r.current_operation(), None, "MERGE_HEAD is cleared");
        assert_eq!(r.head_text(&dir.path().join("b.py")).unwrap(), "b\n");
        assert_eq!(r.status().unwrap().len(), 1);
    }

    fn commit_as(r: &GitRepo, path: &Path, text: &str, message: &str, author: &str) -> String {
        std::fs::write(path, text).unwrap();
        let mut index = r.repo.index().unwrap();
        index.add_path(path.strip_prefix(&r.root).unwrap()).unwrap();
        index.write().unwrap();
        let tree = r.repo.find_tree(index.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now(author, &format!("{}@example.com", author.to_lowercase()))
            .unwrap();
        let head = r.repo.head().unwrap().peel_to_commit().unwrap();
        r.repo
            .commit(Some("HEAD"), &sig, &sig, message, &tree, &[&head])
            .unwrap()
            .to_string()
    }

    #[test]
    fn continuing_a_cherry_pick_or_revert_keeps_author_and_message() {
        let (dir, r) = repo();
        let a = dir.path().join("a.py");
        let main = r.current_branch().unwrap();
        r.create_branch("feature", true).unwrap();
        let picked = commit_as(
            &r,
            &a,
            "line1\nbob\nline3\n",
            "Bob's fix\n\nDetails.\n",
            "Bob",
        );
        r.checkout_branch(&main).unwrap();
        std::fs::write(&a, "line1\nours\nline3\n").unwrap();
        r.commit(std::slice::from_ref(&a), "ours", false).unwrap();
        assert_eq!(r.cherry_pick(&picked).unwrap(), vec![a.clone()]);
        assert_eq!(r.current_operation().as_deref(), Some("cherry-pick"));
        assert_eq!(r.merge_message().as_deref(), Some("Bob's fix\n\nDetails."));
        r.resolve_conflict(&a, "line1\nbob and ours\nline3\n")
            .unwrap();
        // The Commit button with an empty message box (Continue).
        r.commit(std::slice::from_ref(&a), "", false).unwrap();
        let head = r.log(1).unwrap().remove(0);
        assert_eq!(
            (head.author.as_str(), head.summary.as_str()),
            ("Bob", "Bob's fix")
        );
        assert_eq!(head.parents.len(), 1);
        assert_eq!(r.current_operation(), None);
        // Revert an older commit whose line changed again since.
        let x = commit_as(&r, &a, "line1\nx\nline3\n", "x", "Test");
        commit_as(&r, &a, "line1\ny\nline3\n", "y", "Test");
        assert_eq!(r.revert_commit(&x).unwrap().len(), 1);
        assert_eq!(r.current_operation().as_deref(), Some("revert"));
        r.resolve_conflict(&a, "line1\nreverted\nline3\n").unwrap();
        let out = r.continue_operation("").unwrap();
        assert!(out.conflicts.is_empty());
        let head = r.log(1).unwrap().remove(0);
        assert!(head.summary.starts_with("Revert \"x\""), "{}", head.summary);
        assert_eq!(r.current_operation(), None);
    }

    #[test]
    fn amending_keeps_the_author() {
        let (dir, r) = repo();
        let a = dir.path().join("a.py");
        commit_as(&r, &a, "line1\nbob\nline3\n", "Bob's fix", "Bob");
        std::fs::write(&a, "line1\nbob, amended\nline3\n").unwrap();
        r.commit(std::slice::from_ref(&a), "Bob's fix, amended", true)
            .unwrap();
        let head = r.repo.head().unwrap().peel_to_commit().unwrap();
        assert_eq!(
            (
                head.author().name().unwrap_or(""),
                head.committer().name().unwrap_or("")
            ),
            ("Bob", "Test"),
            "author kept, committer updated"
        );
        assert_eq!(head.summary().ok().flatten(), Some("Bob's fix, amended"));
        assert_eq!(
            r.head_text(&a).as_deref(),
            Some("line1\nbob, amended\nline3\n")
        );
        assert_eq!(r.log(10).unwrap().len(), 2, "amended, not added");
    }

    #[test]
    fn gutter_change_helpers() {
        let old = "a\nb\nc\nd\ne\n";
        let new = "a\nB\nc\nnew\nd\n";
        let ch = line_changes(old, new);
        // b→B (line 1), +new (line 3), -e (deletion at the end, marked on the last line).
        assert_eq!(
            ch.iter().map(|c| c.kind).collect::<Vec<_>>(),
            vec![ChangeKind::Modified, ChangeKind::Added, ChangeKind::Deleted]
        );
        let lines = new.split('\n').count();
        assert_eq!(change_at_line(&ch, 1, lines), Some(0));
        assert_eq!(change_at_line(&ch, 2, lines), None);
        assert_eq!(change_at_line(&ch, 3, lines), Some(1));
        assert_eq!(change_at_line(&ch, 5, lines), Some(2));
        assert_eq!(adjacent_change(&ch, 0, true, lines), Some(0));
        assert_eq!(adjacent_change(&ch, 1, true, lines), Some(1));
        assert_eq!(adjacent_change(&ch, 3, true, lines), Some(2));
        assert_eq!(adjacent_change(&ch, 5, true, lines), None);
        assert_eq!(adjacent_change(&ch, 5, false, lines), Some(1));
        assert_eq!(adjacent_change(&ch, 3, false, lines), Some(0));
        assert_eq!(adjacent_change(&ch, 1, false, lines), None);
    }

    fn apply_rollback(text: &str, change: &LineChange) -> String {
        let (range, replacement) = rollback_range(text, change);
        format!(
            "{}{replacement}{}",
            &text[..range.start],
            &text[range.end..]
        )
    }

    #[test]
    fn rollback_lines_restores_head_text() {
        let cases = [
            ("a\nb\nc\n", "a\nX\nc\n"),
            ("a\nb\nc\n", "a\nc\n"),
            ("a\nb\nc\n", "a\nb\nc\nd\n"),
            ("a\nb\nc\n", "new\na\nb\nc\n"),
            ("a\nb\n", "a\nc"),
            ("a\nb", "a\nb\nc\n"),
            ("a\nb\n", "a\n"),
            ("a\nb\nc\nd\ne\nf\n", "a\nB\nc\nd\nE1\nE2\n"),
            ("x\n", ""),
            ("", "x\ny\n"),
        ];
        for (old, new) in cases {
            let changes = line_changes(old, new);
            assert!(!changes.is_empty(), "{old:?} → {new:?}");
            // Each hunk alone: exactly that change disappears.
            for (i, c) in changes.iter().enumerate() {
                let rolled = apply_rollback(new, c);
                let left = line_changes(old, &rolled);
                assert_eq!(
                    left.len(),
                    changes.len() - 1,
                    "{old:?} → {new:?}, rolling back #{i}: {rolled:?}"
                );
            }
            // All hunks, bottom-up: back to HEAD.
            let mut text = new.to_string();
            for c in changes.iter().rev() {
                text = apply_rollback(&text, c);
            }
            assert_eq!(text, old, "{old:?} → {new:?}");
        }
        // The editor buffer is LF-only; CRLF from HEAD is normalised.
        let c = LineChange {
            kind: ChangeKind::Modified,
            start: 1,
            count: 1,
            removed: 1,
            old_text: "two\r\n".into(),
        };
        assert_eq!(apply_rollback("one\nTWO\n", &c), "one\ntwo\n");
    }
}

#[cfg(test)]
mod push_args_tests {
    use super::*;

    fn git(dir: &std::path::Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn first_push_sets_the_upstream() {
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let root = std::env::temp_dir().join(format!("rustpy-push-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (bare, work) = (root.join("bare.git"), root.join("work"));
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        git(&bare, &["init", "--bare", "-q"]);
        git(&work, &["init", "-q", "-b", "main"]);
        std::fs::write(work.join("a.txt"), "x").unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "-q", "-m", "init"]);
        git(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
        let repo = GitRepo::discover(&work).unwrap();
        assert_eq!(
            repo.push_args(),
            vec!["push", "--set-upstream", "origin", "main"]
        );
        GitRepo::run_git_in(&work, &repo.push_args()).expect("first push");
        let repo = GitRepo::discover(&work).unwrap();
        assert_eq!(repo.upstream_of_current().as_deref(), Some("origin/main"));
        assert_eq!(repo.push_args(), vec!["push"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cancellable_runs_report_output_errors_and_cancellation() {
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let root = std::env::temp_dir().join(format!("rustpy-git-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        let token = rustpy_core::CancellationToken::new();
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let out =
            GitRepo::run_git_cancellable(&root, &args(&["status", "--porcelain=v1", "-b"]), &token)
                .unwrap();
        assert!(out.contains("main"), "{out}");
        // Same error text as `run_git_in` (the UI matches on it).
        let err = GitRepo::run_git_cancellable(&root, &args(&["fetch", "no-such-remote"]), &token)
            .unwrap_err();
        let plain = GitRepo::run_git_in(&root, &args(&["fetch", "no-such-remote"]))
            .unwrap_err()
            .to_string();
        assert_eq!(err, rustpy_core::TaskError::Failed(plain));
        token.cancel();
        assert_eq!(
            GitRepo::run_git_cancellable(&root, &args(&["status"]), &token),
            Err(rustpy_core::TaskError::Cancelled)
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

mod known_hosts;
