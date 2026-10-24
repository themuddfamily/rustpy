//! Git operations beyond status/commit/log: blame, stash, log graph, diffs,
//! conflicts, branches/tags, file history, remotes, ignore helpers (roadmap §4).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context};
use git2::{BranchType, Oid, Repository, RepositoryState};

use crate::graph::{assign_lanes, GraphCommit};
use crate::{CommitInfo, FileStatus, GitRepo};

/// One line of a blame (annotate) result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameLine {
    /// 1-based line number in the working-tree file.
    pub line: usize,
    /// Short commit id (`0000000` for uncommitted lines).
    pub commit: String,
    pub author: String,
    /// Seconds since the epoch (0 for uncommitted lines).
    pub time: i64,
    pub summary: String,
    pub is_uncommitted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StashEntry {
    pub index: usize,
    pub message: String,
    /// Seconds since the epoch.
    pub time: i64,
    pub id: String,
}

/// A changed file with its unified diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    pub path: PathBuf,
    /// Previous path for renames.
    pub old_path: Option<PathBuf>,
    pub status: FileStatus,
    pub additions: usize,
    pub deletions: usize,
    /// Unified diff text (`diff --git` header, hunks).
    pub patch: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeOutcome {
    pub fast_forward: bool,
    pub up_to_date: bool,
    /// Conflicted paths; when non-empty the merge is left in progress for resolution.
    pub conflicts: Vec<PathBuf>,
    /// Id of the merge commit (or the fast-forwarded head).
    pub merge_commit: Option<String>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ResetMode {
    Soft,
    Mixed,
    Hard,
}

/// Appends `pattern` to `<root>/.gitignore` (creating the file) unless present.
pub fn add_to_gitignore(root: &Path, pattern: &str) -> anyhow::Result<()> {
    let _operation = crate::RepositoryOperation::enter(root, "Updating gitignore")?;
    let path = root.join(".gitignore");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == pattern.trim()) {
        return Ok(());
    }
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(pattern.trim());
    text.push('\n');
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn short(oid: &Oid) -> String {
    let s = oid.to_string();
    s[..7.min(s.len())].to_string()
}

fn delta_status(d: git2::Delta) -> FileStatus {
    match d {
        git2::Delta::Added => FileStatus::Added,
        git2::Delta::Deleted => FileStatus::Deleted,
        git2::Delta::Renamed => FileStatus::Renamed,
        git2::Delta::Untracked => FileStatus::Untracked,
        git2::Delta::Conflicted => FileStatus::Conflicted,
        git2::Delta::Ignored => FileStatus::Ignored,
        git2::Delta::Unmodified => FileStatus::Unmodified,
        _ => FileStatus::Modified,
    }
}

fn entry_path(e: &git2::IndexEntry) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&e.path).into_owned())
}

impl GitRepo {
    // ---- blame -----------------------------------------------------------

    /// Annotates the working-tree content of `path`; lines not in HEAD are flagged uncommitted.
    pub fn blame(&self, path: &Path) -> anyhow::Result<Vec<BlameLine>> {
        self.rel(path)
            .ok_or_else(|| anyhow!("{} is outside the repository", path.display()))?;
        let content = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        self.blame_contents(path, &content)
    }

    /// Blame an actual editor buffer, including unsaved/uncommitted lines. The
    /// repository's committed history supplies authors; buffer bytes supply rows.
    pub fn blame_contents(&self, path: &Path, content: &[u8]) -> anyhow::Result<Vec<BlameLine>> {
        self.blame_contents_with_options(path, content, false)
    }
    pub fn blame_contents_with_options(
        &self,
        path: &Path,
        content: &[u8],
        ignore_whitespace: bool,
    ) -> anyhow::Result<Vec<BlameLine>> {
        let rel = self
            .rel(path)
            .ok_or_else(|| anyhow!("{} is outside the repository", path.display()))?;
        let line_count = content.iter().filter(|b| **b == b'\n').count()
            + usize::from(!content.is_empty() && !content.ends_with(b"\n"));
        let uncommitted = |line: usize| BlameLine {
            line,
            commit: "0000000".into(),
            author: String::new(),
            time: 0,
            summary: "Uncommitted changes".into(),
            is_uncommitted: true,
        };
        let mut opts = git2::BlameOptions::new();
        opts.ignore_whitespace(ignore_whitespace);
        opts.track_copies_same_file(false);
        let committed = match self.repo.blame_file(&rel, Some(&mut opts)) {
            Ok(b) => b,
            Err(e) if e.code() == git2::ErrorCode::NotFound => {
                return Ok((1..=line_count).map(uncommitted).collect())
            }
            Err(e) => return Err(e.into()),
        };
        let blame = committed.blame_buffer(content)?;
        let mut cache: HashMap<Oid, (String, i64, String)> = HashMap::new();
        let mut out: Vec<BlameLine> = Vec::with_capacity(line_count);
        for hunk in blame.iter() {
            let oid = hunk.final_commit_id();
            let start = hunk.final_start_line();
            for i in 0..hunk.lines_in_hunk() {
                let line = start + i;
                if oid.is_zero() {
                    out.push(uncommitted(line));
                    continue;
                }
                let (author, time, summary) = cache
                    .entry(oid)
                    .or_insert_with(|| match self.repo.find_commit(oid) {
                        Ok(c) => (
                            c.author().name().unwrap_or("").to_string(),
                            c.time().seconds(),
                            c.summary().ok().flatten().unwrap_or("").to_string(),
                        ),
                        Err(_) => (String::new(), 0, String::new()),
                    })
                    .clone();
                out.push(BlameLine {
                    line,
                    commit: short(&oid),
                    author,
                    time,
                    summary,
                    is_uncommitted: false,
                });
            }
        }
        out.sort_by_key(|l| l.line);
        out.dedup_by_key(|l| l.line);
        // Lines beyond what libgit2 reported (e.g. trailing additions) are uncommitted.
        let reported = out.len();
        for line in reported + 1..=line_count {
            out.push(uncommitted(line));
        }
        Ok(out)
    }

    // ---- stash -----------------------------------------------------------

    fn mutable_handle(&self) -> anyhow::Result<Repository> {
        Ok(Repository::open(&self.root)?)
    }

    pub fn stash_list(&self) -> anyhow::Result<Vec<StashEntry>> {
        let mut r = self.mutable_handle()?;
        let mut raw: Vec<(usize, String, Oid)> = Vec::new();
        r.stash_foreach(|index, message, oid| {
            raw.push((index, message.to_string(), *oid));
            true
        })?;
        Ok(raw
            .into_iter()
            .map(|(index, message, oid)| StashEntry {
                index,
                message,
                time: r.find_commit(oid).map(|c| c.time().seconds()).unwrap_or(0),
                id: oid.to_string(),
            })
            .collect())
    }

    pub fn stash_push(&self, message: Option<&str>, include_untracked: bool) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git stash push")?;
        let sig = self.signature()?;
        let mut r = self.mutable_handle()?;
        let flags = if include_untracked {
            git2::StashFlags::INCLUDE_UNTRACKED
        } else {
            git2::StashFlags::DEFAULT
        };
        r.stash_save2(&sig, message, Some(flags))?;
        Ok(())
    }

    pub fn stash_pop(&self, index: usize) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git stash pop")?;
        let mut r = self.mutable_handle()?;
        let mut opts = git2::StashApplyOptions::new();
        opts.reinstantiate_index();
        r.stash_pop(index, Some(&mut opts))?;
        Ok(())
    }

    pub fn stash_apply(&self, index: usize) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git stash apply")?;
        let mut r = self.mutable_handle()?;
        let mut opts = git2::StashApplyOptions::new();
        opts.reinstantiate_index();
        r.stash_apply(index, Some(&mut opts))?;
        Ok(())
    }

    pub fn stash_drop(&self, index: usize) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git stash drop")?;
        let mut r = self.mutable_handle()?;
        r.stash_drop(index)?;
        Ok(())
    }

    // ---- log graph -------------------------------------------------------

    fn ref_map(&self) -> HashMap<Oid, Vec<String>> {
        let mut refs: HashMap<Oid, Vec<String>> = HashMap::new();
        if let Ok(iter) = self.repo.references() {
            for r in iter.flatten() {
                let Ok(name) = r.shorthand() else { continue };
                if name == "HEAD" {
                    continue;
                }
                // Annotated tags point at a tag object; resolve to the commit.
                let target = r
                    .target()
                    .and_then(|oid| self.repo.find_object(oid, None).ok())
                    .and_then(|o| o.peel_to_commit().ok())
                    .map(|c| c.id());
                if let Some(oid) = target {
                    refs.entry(oid).or_default().push(name.to_string());
                }
            }
        }
        refs
    }

    fn commit_info(&self, c: &git2::Commit<'_>, refs: &HashMap<Oid, Vec<String>>) -> CommitInfo {
        let oid = c.id();
        let id = oid.to_string();
        CommitInfo {
            short_id: short(&oid),
            id,
            summary: c.summary().ok().flatten().unwrap_or("").to_string(),
            message: c.message().unwrap_or("").to_string(),
            author: c.author().name().unwrap_or("").to_string(),
            email: c.author().email().unwrap_or("").to_string(),
            time: c.time().seconds(),
            parents: c.parent_ids().map(|p| p.to_string()).collect(),
            refs: refs.get(&oid).cloned().unwrap_or_default(),
        }
    }

    /// Commits with graph lanes, newest first (HEAD only, or every local and remote branch).
    pub fn log_graph(&self, limit: usize, all_branches: bool) -> anyhow::Result<Vec<GraphCommit>> {
        let mut walk = self.repo.revwalk()?;
        walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
        let mut pushed = walk.push_head().is_ok();
        if all_branches {
            pushed |= walk.push_glob("refs/heads/*").is_ok();
            pushed |= walk.push_glob("refs/remotes/*").is_ok();
        }
        if !pushed {
            return Ok(Vec::new());
        }
        let refs = self.ref_map();
        let mut commits: Vec<GraphCommit> = Vec::new();
        for oid in walk.flatten() {
            if commits.len() >= limit {
                break;
            }
            let c = self.repo.find_commit(oid)?;
            commits.push(GraphCommit {
                id: oid.to_string(),
                short_id: short(&oid),
                summary: c.summary().ok().flatten().unwrap_or("").to_string(),
                message: c.message().unwrap_or("").to_string(),
                author: c.author().name().unwrap_or("").to_string(),
                email: c.author().email().unwrap_or("").to_string(),
                time: c.time().seconds(),
                parents: c.parent_ids().map(|p| p.to_string()).collect(),
                refs: refs.get(&oid).cloned().unwrap_or_default(),
                lane: 0,
                lanes_in: Vec::new(),
                lane_count: 1,
            });
        }
        assign_lanes(&mut commits);
        Ok(commits)
    }

    // ---- diffs -------------------------------------------------------------

    pub(crate) fn file_diffs(&self, diff: &git2::Diff<'_>) -> Vec<FileDiff> {
        let mut out = Vec::new();
        for (i, d) in diff.deltas().enumerate() {
            let new_path = d.new_file().path().map(Path::to_path_buf);
            let old_path = d.old_file().path().map(Path::to_path_buf);
            let path = self.root.join(
                new_path
                    .clone()
                    .or_else(|| old_path.clone())
                    .unwrap_or_default(),
            );
            let status = delta_status(d.status());
            let old_path = if status == FileStatus::Renamed {
                old_path.map(|p| self.root.join(p))
            } else {
                None
            };
            let (mut additions, mut deletions, mut patch) = (0, 0, String::new());
            if let Ok(Some(mut p)) = git2::Patch::from_diff(diff, i) {
                if let Ok((_, a, dl)) = p.line_stats() {
                    additions = a;
                    deletions = dl;
                }
                if let Ok(buf) = p.to_buf() {
                    patch = String::from_utf8_lossy(&buf).into_owned();
                }
            }
            out.push(FileDiff {
                path,
                old_path,
                status,
                additions,
                deletions,
                patch,
            });
        }
        out
    }

    fn tree_of(&self, spec: &str) -> anyhow::Result<git2::Tree<'_>> {
        let obj = self
            .repo
            .revparse_single(spec)
            .with_context(|| format!("unknown revision {spec}"))?;
        Ok(obj.peel_to_tree()?)
    }

    /// Files changed between two revisions (anything `git rev-parse` accepts).
    pub fn diff_commits(&self, old: &str, new: &str) -> anyhow::Result<Vec<FileDiff>> {
        let old_tree = self.tree_of(old)?;
        let new_tree = self.tree_of(new)?;
        let mut diff = self
            .repo
            .diff_tree_to_tree(Some(&old_tree), Some(&new_tree), None)?;
        let mut find = git2::DiffFindOptions::new();
        find.renames(true);
        let _ = diff.find_similar(Some(&mut find));
        Ok(self.file_diffs(&diff))
    }

    /// Unified diff of the working-tree file against HEAD.
    pub fn diff_file_to_head(&self, path: &Path) -> anyhow::Result<String> {
        let rel = self
            .rel(path)
            .ok_or_else(|| anyhow!("{} is outside the repository", path.display()))?;
        let head_tree = self.repo.head().ok().and_then(|h| h.peel_to_tree().ok());
        let mut opts = git2::DiffOptions::new();
        opts.pathspec(rel)
            .include_untracked(true)
            .recurse_untracked_dirs(true)
            .show_untracked_content(true);
        let diff = self
            .repo
            .diff_tree_to_workdir_with_index(head_tree.as_ref(), Some(&mut opts))?;
        let mut text = String::new();
        diff.print(git2::DiffFormat::Patch, |_, _, line| {
            if matches!(line.origin(), '+' | '-' | ' ') {
                text.push(line.origin());
            }
            text.push_str(&String::from_utf8_lossy(line.content()));
            true
        })?;
        Ok(text)
    }

    /// Content of `path` at `commit` (any revision spec).
    pub fn file_at_commit(&self, commit: &str, path: &Path) -> anyhow::Result<String> {
        let rel = self.rel(path).unwrap_or_else(|| path.to_path_buf());
        let tree = self.tree_of(commit)?;
        let entry = tree
            .get_path(&rel)
            .with_context(|| format!("{} not in {commit}", rel.display()))?;
        let blob = self.repo.find_blob(entry.id())?;
        Ok(String::from_utf8_lossy(blob.content()).into_owned())
    }

    /// Changes in the working tree (staged + unstaged + untracked) relative to HEAD.
    pub fn diff_working_tree(&self, path: Option<&Path>) -> anyhow::Result<Vec<FileDiff>> {
        let head_tree = self.repo.head().ok().and_then(|h| h.peel_to_tree().ok());
        let mut opts = git2::DiffOptions::new();
        opts.include_untracked(true)
            .recurse_untracked_dirs(true)
            .show_untracked_content(true);
        if let Some(p) = path {
            let rel = self.rel(p).unwrap_or_else(|| p.to_path_buf());
            opts.pathspec(rel);
        }
        let mut diff = self
            .repo
            .diff_tree_to_workdir_with_index(head_tree.as_ref(), Some(&mut opts))?;
        let mut find = git2::DiffFindOptions::new();
        find.renames(true);
        let _ = diff.find_similar(Some(&mut find));
        Ok(self.file_diffs(&diff))
    }

    // ---- conflicts ---------------------------------------------------------

    /// Paths with unresolved merge conflicts.
    pub fn conflicts(&self) -> anyhow::Result<Vec<PathBuf>> {
        let mut index = self.repo.index()?;
        // Pick up changes made by the git CLI (merge --abort, etc.).
        index.read(false)?;
        let mut out: Vec<PathBuf> = Vec::new();
        for c in index.conflicts()? {
            let c = c?;
            let Some(e) = c.our.as_ref().or(c.their.as_ref()).or(c.ancestor.as_ref()) else {
                continue;
            };
            let p = self.root.join(entry_path(e));
            if !out.contains(&p) {
                out.push(p);
            }
        }
        Ok(out)
    }

    /// `(base, ours, theirs)` contents of a conflicted file from the index stages.
    pub fn conflict_sides(&self, path: &Path) -> anyhow::Result<(String, String, String)> {
        let rel = self
            .rel(path)
            .ok_or_else(|| anyhow!("{} is outside the repository", path.display()))?;
        let mut index = self.repo.index()?;
        index.read(false)?;
        let text = |e: &Option<git2::IndexEntry>| -> String {
            e.as_ref()
                .and_then(|e| self.repo.find_blob(e.id).ok())
                .map(|b| String::from_utf8_lossy(b.content()).into_owned())
                .unwrap_or_default()
        };
        for c in index.conflicts()? {
            let c = c?;
            let p = c
                .our
                .as_ref()
                .or(c.their.as_ref())
                .or(c.ancestor.as_ref())
                .map(entry_path);
            if p.as_deref() == Some(rel.as_path()) {
                return Ok((text(&c.ancestor), text(&c.our), text(&c.their)));
            }
        }
        bail!("{} has no conflict", rel.display())
    }

    /// Writes the resolved content and marks the conflict as resolved in the index.
    pub fn resolve_conflict(&self, path: &Path, content: &str) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git resolve conflict")?;
        let rel = self
            .rel(path)
            .ok_or_else(|| anyhow!("{} is outside the repository", path.display()))?;
        std::fs::write(path, content)?;
        let mut index = self.repo.index()?;
        index.read(false)?;
        index.add_path(&rel)?;
        if index.has_conflicts() {
            // Older libgit2 keeps the stages; drop them explicitly.
            let _ = index.conflict_remove(&rel);
            index.add_path(&rel)?;
        }
        index.write()?;
        Ok(())
    }

    pub fn merge_in_progress(&self) -> bool {
        self.repo.state() == RepositoryState::Merge
    }

    /// The operation in progress: `merge`, `rebase`, `cherry-pick`, `revert`, `bisect`, `am`.
    pub fn current_operation(&self) -> Option<String> {
        Some(
            match self.repo.state() {
                RepositoryState::Clean => return None,
                RepositoryState::Merge => "merge",
                RepositoryState::Revert | RepositoryState::RevertSequence => "revert",
                RepositoryState::CherryPick | RepositoryState::CherryPickSequence => "cherry-pick",
                RepositoryState::Bisect => "bisect",
                RepositoryState::Rebase
                | RepositoryState::RebaseInteractive
                | RepositoryState::RebaseMerge => "rebase",
                RepositoryState::ApplyMailbox | RepositoryState::ApplyMailboxOrRebase => "am",
            }
            .to_string(),
        )
    }

    /// Aborts the operation in progress (merge, cherry-pick, revert, rebase), restoring HEAD.
    pub fn abort_operation(&self) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git abort operation")?;
        let cli: Option<&[&str]> = match self.repo.state() {
            RepositoryState::Clean => return Ok(()),
            RepositoryState::Merge => Some(&["merge", "--abort"]),
            RepositoryState::CherryPick | RepositoryState::CherryPickSequence => {
                Some(&["cherry-pick", "--abort"])
            }
            RepositoryState::Revert | RepositoryState::RevertSequence => {
                Some(&["revert", "--abort"])
            }
            RepositoryState::Rebase
            | RepositoryState::RebaseInteractive
            | RepositoryState::RebaseMerge => Some(&["rebase", "--abort"]),
            _ => None,
        };
        if let Some(args) = cli {
            if self.run_git(args).is_ok() && self.repo.state() == RepositoryState::Clean {
                return Ok(());
            }
        }
        // Fallback: hard reset to HEAD and clear the state files (keeping any autostash).
        self.keep_autostash();
        let head = self.repo.head()?.peel(git2::ObjectType::Commit)?;
        self.repo.reset(&head, git2::ResetType::Hard, None)?;
        self.repo.cleanup_state()?;
        Ok(())
    }

    pub fn abort_merge(&self) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git abort merge")?;
        self.abort_operation()
    }

    /// Commits a resolved merge (parents: HEAD + MERGE_HEAD) and clears the merge state.
    pub fn finish_merge(&self, message: &str) -> anyhow::Result<String> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git finish merge")?;
        if !self.conflicts()?.is_empty() {
            bail!("unresolved conflicts remain");
        }
        if self.repo.state() != RepositoryState::Merge {
            bail!("no merge in progress");
        }
        self.commit(&[], message, false)
    }

    // ---- branches & tags ---------------------------------------------------

    /// `(tag name, short id of the tagged commit)`.
    pub fn tags(&self) -> anyhow::Result<Vec<(String, String)>> {
        let names = self.repo.tag_names(None)?;
        let mut out = Vec::new();
        for name in names.iter().flatten().flatten() {
            let target = self
                .repo
                .revparse_single(&format!("refs/tags/{name}"))
                .ok()
                .map(|o| o.peel_to_commit().map(|c| c.id()).unwrap_or(o.id()));
            out.push((
                name.to_string(),
                target.map(|o| short(&o)).unwrap_or_default(),
            ));
        }
        Ok(out)
    }

    /// Creates a lightweight tag (no message) or an annotated one at HEAD.
    pub fn create_tag(&self, name: &str, message: Option<&str>) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git create tag")?;
        self.create_tag_at(name, "HEAD", message)
    }

    /// Creates a lightweight tag (no message) or an annotated one at `target`
    /// (a commit id or any revision).
    pub fn create_tag_at(
        &self,
        name: &str,
        target: &str,
        message: Option<&str>,
    ) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git create tag at")?;
        let name = name.trim();
        if name.is_empty() {
            bail!("the tag name is empty");
        }
        if !git2::Reference::is_valid_name(&format!("refs/tags/{name}")) {
            bail!("'{name}' is not a valid tag name");
        }
        let target = self
            .repo
            .revparse_single(target)
            .with_context(|| format!("unknown revision {target}"))?
            .peel(git2::ObjectType::Commit)?;
        match message.map(str::trim).filter(|m| !m.is_empty()) {
            Some(msg) => {
                let sig = self.signature()?;
                self.repo.tag(name, &target, &sig, msg, false)?;
            }
            None => {
                self.repo.tag_lightweight(name, &target, false)?;
            }
        }
        Ok(())
    }

    pub fn delete_tag(&self, name: &str) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git delete tag")?;
        self.repo.tag_delete(name)?;
        Ok(())
    }

    /// Deletes a local branch; without `force` the branch must be merged into HEAD.
    pub fn delete_branch(&self, name: &str, force: bool) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git delete branch")?;
        let mut branch = self.repo.find_branch(name, BranchType::Local)?;
        if branch.is_head() {
            bail!("cannot delete the current branch '{name}'");
        }
        if !force {
            let head = self.repo.head()?.target();
            let target = branch.get().target();
            if let (Some(h), Some(t)) = (head, target) {
                let (ahead, _) = self.repo.graph_ahead_behind(t, h)?;
                if ahead > 0 {
                    bail!("branch '{name}' is not fully merged ({ahead} commit(s) would be lost)");
                }
            }
        }
        branch.delete()?;
        Ok(())
    }

    pub fn rename_branch(&self, old: &str, new: &str) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git rename branch")?;
        let mut branch = self.repo.find_branch(old, BranchType::Local)?;
        branch.rename(new, false)?;
        Ok(())
    }

    /// Merges `name` (branch or revision) into HEAD: fast-forward when possible,
    /// otherwise a merge commit; conflicts leave the merge in progress.
    pub fn merge_branch(&self, name: &str) -> anyhow::Result<MergeOutcome> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git merge branch")?;
        // The branch, not a tag that happens to have its name.
        let spec = self.branch_spec(name);
        let (obj, reference) = self.repo.revparse_ext(&spec)?;
        let annotated = match &reference {
            Some(r) => self.repo.reference_to_annotated_commit(r)?,
            None => self
                .repo
                .find_annotated_commit(obj.peel_to_commit()?.id())?,
        };
        let (analysis, _) = self.repo.merge_analysis(&[&annotated])?;
        if analysis.is_up_to_date() {
            return Ok(MergeOutcome {
                fast_forward: false,
                up_to_date: true,
                conflicts: Vec::new(),
                merge_commit: None,
            });
        }
        let target = annotated.id();
        if analysis.is_fast_forward() || analysis.is_unborn() {
            let commit_obj = self.repo.find_object(target, None)?;
            self.repo.checkout_tree(
                &commit_obj,
                Some(git2::build::CheckoutBuilder::new().safe()),
            )?;
            match self.repo.head() {
                Ok(h) if h.is_branch() => {
                    let refname = h.name().map_err(|_| anyhow!("invalid HEAD"))?.to_string();
                    self.repo.reference(
                        &refname,
                        target,
                        true,
                        &format!("merge {name}: Fast-forward"),
                    )?;
                }
                _ => self.repo.set_head_detached(target)?,
            }
            return Ok(MergeOutcome {
                fast_forward: true,
                up_to_date: false,
                conflicts: Vec::new(),
                merge_commit: Some(target.to_string()),
            });
        }
        if let Err(e) = self.repo.merge(
            &[&annotated],
            None,
            Some(git2::build::CheckoutBuilder::new().safe()),
        ) {
            // CLI fallback (e.g. checkout refused by libgit2).
            match self.run_git(&["merge", "--no-edit", &self.branch_cli_arg(name)]) {
                Ok(_) => {
                    return Ok(MergeOutcome {
                        fast_forward: false,
                        up_to_date: false,
                        conflicts: Vec::new(),
                        merge_commit: self
                            .repo
                            .head()
                            .ok()
                            .and_then(|h| h.target())
                            .map(|o| o.to_string()),
                    });
                }
                Err(cli_err) => {
                    let conflicts = self.conflicts().unwrap_or_default();
                    if !conflicts.is_empty() {
                        return Ok(MergeOutcome {
                            fast_forward: false,
                            up_to_date: false,
                            conflicts,
                            merge_commit: None,
                        });
                    }
                    bail!("merge failed: {e}; {cli_err}");
                }
            }
        }
        let conflicts = self.conflicts()?;
        if !conflicts.is_empty() {
            return Ok(MergeOutcome {
                fast_forward: false,
                up_to_date: false,
                conflicts,
                merge_commit: None,
            });
        }
        let id = self.finish_merge(&format!("Merge branch '{name}'"))?;
        Ok(MergeOutcome {
            fast_forward: false,
            up_to_date: false,
            conflicts: Vec::new(),
            merge_commit: Some(id),
        })
    }

    fn commit_index_with(
        &self,
        message: &str,
        author: Option<git2::Signature<'_>>,
    ) -> anyhow::Result<String> {
        let mut index = self.repo.index()?;
        let tree_oid = index.write_tree()?;
        let tree = self.repo.find_tree(tree_oid)?;
        let head = self.repo.head()?.peel_to_commit()?;
        let sig = self.signature()?;
        let author = author.unwrap_or_else(|| sig.clone());
        let oid = self
            .repo
            .commit(Some("HEAD"), &author, &sig, message, &tree, &[&head])?;
        Ok(oid.to_string())
    }

    /// Cherry-picks `commit` onto HEAD; returns conflicted paths (the pick stays in progress) or an empty list.
    pub fn cherry_pick(&self, commit: &str) -> anyhow::Result<Vec<PathBuf>> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git cherry pick")?;
        let c = self.repo.revparse_single(commit)?.peel_to_commit()?;
        self.repo.cherrypick(&c, None)?;
        let conflicts = self.conflicts()?;
        if !conflicts.is_empty() {
            return Ok(conflicts);
        }
        if !crate::policy::current().cherry_pick_commit {
            return Ok(Vec::new());
        }
        let message = c.message().unwrap_or("").to_string();
        self.commit_index_with(&message, Some(c.author().to_owned()))?;
        self.repo.cleanup_state()?;
        Ok(Vec::new())
    }

    /// Reverts `commit` with a new commit; returns conflicted paths when it cannot be applied cleanly.
    pub fn revert_commit(&self, commit: &str) -> anyhow::Result<Vec<PathBuf>> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git revert commit")?;
        let c = self.repo.revparse_single(commit)?.peel_to_commit()?;
        self.repo.revert(&c, None)?;
        let conflicts = self.conflicts()?;
        if !conflicts.is_empty() {
            return Ok(conflicts);
        }
        let message = format!(
            "Revert \"{}\"\n\nThis reverts commit {}.\n",
            c.summary().ok().flatten().unwrap_or(""),
            c.id()
        );
        self.commit_index_with(&message, None)?;
        self.repo.cleanup_state()?;
        Ok(Vec::new())
    }

    pub fn reset(&self, commit: &str, mode: ResetMode) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git reset")?;
        let obj = self.repo.revparse_single(commit)?;
        let kind = match mode {
            ResetMode::Soft => git2::ResetType::Soft,
            ResetMode::Mixed => git2::ResetType::Mixed,
            ResetMode::Hard => git2::ResetType::Hard,
        };
        self.repo.reset(&obj, kind, None)?;
        Ok(())
    }

    /// Checks out a commit, detaching HEAD.
    pub fn checkout_commit(&self, commit: &str) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git checkout commit")?;
        let obj = self.repo.revparse_single(commit)?;
        let id = obj.peel_to_commit()?.id();
        self.repo
            .checkout_tree(&obj, Some(git2::build::CheckoutBuilder::new().safe()))?;
        self.repo.set_head_detached(id)?;
        Ok(())
    }

    // ---- history -------------------------------------------------------------

    /// Commits that changed `path`, newest first, following renames.
    pub fn file_history(&self, path: &Path, limit: usize) -> anyhow::Result<Vec<CommitInfo>> {
        let mut rel = self
            .rel(path)
            .ok_or_else(|| anyhow!("{} is outside the repository", path.display()))?;
        let mut walk = self.repo.revwalk()?;
        walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
        if walk.push_head().is_err() {
            return Ok(Vec::new());
        }
        let refs = self.ref_map();
        let mut out = Vec::new();
        for oid in walk.flatten() {
            if out.len() >= limit {
                break;
            }
            let c = self.repo.find_commit(oid)?;
            let tree = c.tree()?;
            let entry = tree.get_path(&rel).ok().map(|e| e.id());
            let parent = c.parent(0).ok();
            let parent_tree = parent.as_ref().and_then(|p| p.tree().ok());
            let parent_entry = parent_tree
                .as_ref()
                .and_then(|t| t.get_path(&rel).ok().map(|e| e.id()));
            if entry == parent_entry {
                continue;
            }
            out.push(self.commit_info(&c, &refs));
            if entry.is_some() && parent_entry.is_none() {
                // Added here: was it renamed from somewhere?
                if let Some(pt) = parent_tree.as_ref() {
                    let mut diff = self.repo.diff_tree_to_tree(Some(pt), Some(&tree), None)?;
                    let mut find = git2::DiffFindOptions::new();
                    find.renames(true);
                    if diff.find_similar(Some(&mut find)).is_ok() {
                        if let Some(old) = diff
                            .deltas()
                            .find(|d| {
                                d.status() == git2::Delta::Renamed
                                    && d.new_file().path() == Some(rel.as_path())
                            })
                            .and_then(|d| d.old_file().path().map(Path::to_path_buf))
                        {
                            rel = old;
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// Files that differ between `branch` and HEAD (changes from the branch to the current tree).
    pub fn compare_with_branch(&self, branch: &str) -> anyhow::Result<Vec<FileDiff>> {
        self.diff_commits(&self.branch_spec(branch), "HEAD")
    }

    // ---- remotes -------------------------------------------------------------

    /// `(name, url)` of every remote.
    pub fn remotes_with_urls(&self) -> anyhow::Result<Vec<(String, String)>> {
        let names = self.repo.remotes()?;
        let mut out = Vec::new();
        for name in names.iter().flatten().flatten() {
            let url = self
                .repo
                .find_remote(name)
                .ok()
                .and_then(|r| r.url().ok().map(str::to_owned))
                .unwrap_or_default();
            out.push((name.to_string(), url));
        }
        Ok(out)
    }

    pub fn add_remote(&self, name: &str, url: &str) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git add remote")?;
        self.repo.remote(name, url)?;
        Ok(())
    }

    pub fn remove_remote(&self, name: &str) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git remove remote")?;
        self.repo.remote_delete(name)?;
        Ok(())
    }

    // ---- ignore ----------------------------------------------------------------

    /// Appends `pattern` to the repository's `.gitignore`.
    pub fn add_to_gitignore(&self, pattern: &str) -> anyhow::Result<()> {
        let _operation = crate::RepositoryOperation::enter(&self.root, "Git add to gitignore")?;
        add_to_gitignore(&self.root, pattern)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, text).unwrap();
        p
    }

    #[test]
    fn blame_marks_committed_and_uncommitted_lines() {
        let (dir, r) = repo();
        let first = r.log(1).unwrap()[0].short_id.clone();
        let a = write(dir.path(), "a.py", "line1\nchanged\nline3\nline4\n");
        let b = r.blame(&a).unwrap();
        assert_eq!(b.len(), 4);
        assert_eq!(b[0].commit, first);
        assert_eq!(b[0].author, "Test");
        assert_eq!(b[0].summary, "initial");
        assert!(!b[0].is_uncommitted);
        assert!(b[1].is_uncommitted);
        assert!(!b[2].is_uncommitted);
        assert!(b[3].is_uncommitted);
        assert_eq!(
            b.iter().map(|l| l.line).collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        // Untracked file: everything uncommitted.
        let n = write(dir.path(), "new.py", "x\ny\n");
        let b = r.blame(&n).unwrap();
        assert_eq!(b.len(), 2);
        assert!(b.iter().all(|l| l.is_uncommitted));
    }

    #[test]
    fn stash_round_trip() {
        let (dir, r) = repo();
        let a = write(dir.path(), "a.py", "line1\nWIP\nline3\n");
        r.stash_push(Some("wip work"), false).unwrap();
        assert_eq!(
            std::fs::read_to_string(&a).unwrap(),
            "line1\nline2\nline3\n"
        );
        let list = r.stash_list().unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].message.contains("wip work"));
        assert_eq!(list[0].index, 0);
        assert!(list[0].time > 0);
        r.stash_pop(0).unwrap();
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "line1\nWIP\nline3\n");
        assert!(r.stash_list().unwrap().is_empty());
        // Untracked files are only stashed on request.
        write(dir.path(), "u.py", "u\n");
        r.stash_push(None, true).unwrap();
        assert!(!dir.path().join("u.py").exists());
        r.stash_apply(0).unwrap();
        assert!(dir.path().join("u.py").exists());
        r.stash_drop(0).unwrap();
        assert!(r.stash_list().unwrap().is_empty());
    }

    #[test]
    fn log_graph_linear_and_merge() {
        let (dir, r) = repo();
        let main = r.current_branch().unwrap();
        write(dir.path(), "b.py", "b\n");
        r.commit(&[dir.path().join("b.py")], "second", false)
            .unwrap();
        let g = r.log_graph(10, false).unwrap();
        assert_eq!(g.len(), 2);
        assert!(g.iter().all(|c| c.lane == 0 && c.lane_count == 1));
        assert_eq!(g[0].lanes_in, vec![(0, 0)]);
        assert!(g[0].refs.iter().any(|x| x == &main));
        // Branch + merge.
        r.create_branch("feature", true).unwrap();
        write(dir.path(), "c.py", "c\n");
        r.commit(
            &[dir.path().join("c.py")],
            "feature work\n\nWith a body.",
            false,
        )
        .unwrap();
        r.checkout_branch(&main).unwrap();
        write(dir.path(), "d.py", "d\n");
        r.commit(&[dir.path().join("d.py")], "main work", false)
            .unwrap();
        // Before the merge the feature-only commit is listed with all branches,
        // with its full message, and the rows come straight from the graph.
        let only_head = r.log_graph(10, false).unwrap();
        assert!(!only_head.iter().any(|c| c.summary == "feature work"));
        let all = r.log_graph(10, true).unwrap();
        let feature = all
            .iter()
            .find(|c| c.summary == "feature work")
            .expect("feature-only commit listed");
        assert_eq!(feature.to_info().message, "feature work\n\nWith a body.");
        assert_eq!(feature.email, "t@example.com");
        assert_eq!(all.len(), 4);
        let out = r.merge_branch("feature").unwrap();
        assert!(!out.fast_forward && out.conflicts.is_empty() && out.merge_commit.is_some());
        let g = r.log_graph(10, true).unwrap();
        assert_eq!(g.len(), 5);
        let m = &g[0];
        assert_eq!(m.parents.len(), 2);
        assert_eq!(m.lane, 0);
        assert_eq!(m.lanes_in, vec![(0, 0), (0, 1)]);
        assert_eq!(m.lane_count, 2);
        let by = |s: &str| g.iter().find(|c| c.summary == s).unwrap();
        assert_eq!(by("main work").lane, 0);
        assert_eq!(by("feature work").lane, 1);
        assert_eq!(by("second").lane, 0);
        assert_eq!(by("initial").lane, 0);
        assert!(by("feature work").refs.iter().any(|x| x == "feature"));
        assert!(g.iter().all(|c| c.lane_count <= 2));
    }

    #[test]
    fn fast_forward_merge() {
        let (dir, r) = repo();
        let main = r.current_branch().unwrap();
        r.create_branch("ff", true).unwrap();
        write(dir.path(), "b.py", "b\n");
        let id = r
            .commit(&[dir.path().join("b.py")], "on ff", false)
            .unwrap();
        r.checkout_branch(&main).unwrap();
        assert!(!dir.path().join("b.py").exists());
        let out = r.merge_branch("ff").unwrap();
        assert!(out.fast_forward);
        assert_eq!(out.merge_commit.as_deref(), Some(id.as_str()));
        assert!(dir.path().join("b.py").exists());
        assert_eq!(r.current_branch().as_deref(), Some(main.as_str()));
        assert!(r.merge_branch("ff").unwrap().up_to_date);
    }

    #[test]
    fn conflicts_resolve_and_abort() {
        let (dir, r) = repo();
        let main = r.current_branch().unwrap();
        r.create_branch("feature", true).unwrap();
        write(dir.path(), "a.py", "line1\ntheirs\nline3\n");
        r.commit(&[dir.path().join("a.py")], "theirs", false)
            .unwrap();
        r.checkout_branch(&main).unwrap();
        write(dir.path(), "a.py", "line1\nours\nline3\n");
        r.commit(&[dir.path().join("a.py")], "ours", false).unwrap();
        let out = r.merge_branch("feature").unwrap();
        assert_eq!(out.conflicts, vec![dir.path().join("a.py")]);
        assert!(r.merge_in_progress());
        assert_eq!(r.current_operation().as_deref(), Some("merge"));
        assert_eq!(r.conflicts().unwrap(), vec![dir.path().join("a.py")]);
        let (base, ours, theirs) = r.conflict_sides(&dir.path().join("a.py")).unwrap();
        assert!(base.contains("line2") && ours.contains("ours") && theirs.contains("theirs"));
        let st = r.status().unwrap();
        assert_eq!(st[0].status, FileStatus::Conflicted);
        r.resolve_conflict(&dir.path().join("a.py"), "line1\nresolved\nline3\n")
            .unwrap();
        assert!(r.conflicts().unwrap().is_empty());
        let id = r.finish_merge("merge feature").unwrap();
        assert!(!r.merge_in_progress());
        let log = r.log(1).unwrap();
        assert_eq!(log[0].id, id);
        assert_eq!(log[0].parents.len(), 2);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.py")).unwrap(),
            "line1\nresolved\nline3\n"
        );
        // Abort path.
        r.create_branch("feature2", true).unwrap();
        write(dir.path(), "a.py", "line1\nF2\nline3\n");
        r.commit(&[dir.path().join("a.py")], "f2", false).unwrap();
        r.checkout_branch(&main).unwrap();
        write(dir.path(), "a.py", "line1\nM2\nline3\n");
        r.commit(&[dir.path().join("a.py")], "m2", false).unwrap();
        let out = r.merge_branch("feature2").unwrap();
        assert_eq!(out.conflicts.len(), 1);
        r.abort_merge().unwrap();
        assert!(!r.merge_in_progress());
        assert!(r.conflicts().unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.py")).unwrap(),
            "line1\nM2\nline3\n"
        );
    }

    #[test]
    fn tags_and_branches() {
        let (dir, r) = repo();
        let head = r.log(1).unwrap()[0].short_id.clone();
        r.create_tag("v1", None).unwrap();
        r.create_tag("v2", Some("release two")).unwrap();
        let tags = r.tags().unwrap();
        assert!(tags.contains(&("v1".to_string(), head.clone())));
        assert!(tags.contains(&("v2".to_string(), head.clone())));
        assert!(r.log(1).unwrap()[0].refs.iter().any(|x| x == "v1"));
        r.delete_tag("v1").unwrap();
        assert!(!r.tags().unwrap().iter().any(|(n, _)| n == "v1"));
        // Tags at a selected (older) commit, lightweight and annotated.
        let first = r.log(1).unwrap()[0].id.clone();
        write(dir.path(), "n.py", "n\n");
        r.commit(&[dir.path().join("n.py")], "newer", false)
            .unwrap();
        r.create_tag_at("v0.9", &first, None).unwrap();
        r.create_tag_at("v0.9.1", &first[..10], Some("annotated"))
            .unwrap();
        let tags = r.tags().unwrap();
        assert!(
            tags.contains(&("v0.9".to_string(), head.clone())),
            "{tags:?}"
        );
        assert!(
            tags.contains(&("v0.9.1".to_string(), head.clone())),
            "{tags:?}"
        );
        assert!(
            !r.log(1).unwrap()[0]
                .refs
                .iter()
                .any(|x| x.starts_with("v0.9")),
            "HEAD is not tagged"
        );
        assert!(r.create_tag_at("bad tag", &first, None).is_err());
        assert!(r.create_tag_at("", &first, None).is_err());
        assert!(r.create_tag_at("v0.9", &first, None).is_err(), "exists");
        let main = r.current_branch().unwrap();
        r.create_branch("topic", true).unwrap();
        write(dir.path(), "t.py", "t\n");
        r.commit(&[dir.path().join("t.py")], "topic commit", false)
            .unwrap();
        assert!(
            r.delete_branch("topic", false).is_err(),
            "current branch cannot be deleted"
        );
        r.checkout_branch(&main).unwrap();
        assert!(
            r.delete_branch("topic", false).is_err(),
            "unmerged branch needs force"
        );
        r.rename_branch("topic", "topic2").unwrap();
        assert!(r.branches().iter().any(|b| b.name == "topic2"));
        r.delete_branch("topic2", true).unwrap();
        assert!(!r.branches().iter().any(|b| b.name == "topic2"));
    }

    #[test]
    fn file_history_follows_renames() {
        let (dir, r) = repo();
        write(dir.path(), "b.py", "b\n");
        r.commit(&[dir.path().join("b.py")], "add b", false)
            .unwrap();
        write(dir.path(), "a.py", "line1\nline2\nline3\nline4\n");
        r.commit(&[dir.path().join("a.py")], "edit a", false)
            .unwrap();
        let h = r.file_history(&dir.path().join("a.py"), 10).unwrap();
        assert_eq!(
            h.iter().map(|c| c.summary.as_str()).collect::<Vec<_>>(),
            vec!["edit a", "initial"]
        );
        // Rename a.py → c.py.
        std::fs::rename(dir.path().join("a.py"), dir.path().join("c.py")).unwrap();
        r.commit(
            &[dir.path().join("a.py"), dir.path().join("c.py")],
            "rename a to c",
            false,
        )
        .unwrap();
        let h = r.file_history(&dir.path().join("c.py"), 10).unwrap();
        assert_eq!(
            h.iter().map(|c| c.summary.as_str()).collect::<Vec<_>>(),
            vec!["rename a to c", "edit a", "initial"]
        );
        assert_eq!(
            r.file_at_commit("HEAD~1", &dir.path().join("a.py"))
                .unwrap(),
            "line1\nline2\nline3\nline4\n"
        );
        let diffs = r.diff_commits("HEAD~1", "HEAD").unwrap();
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].status, FileStatus::Renamed);
        assert_eq!(diffs[0].old_path, Some(dir.path().join("a.py")));
        assert_eq!(diffs[0].path, dir.path().join("c.py"));
        let cmp = r.compare_with_branch("HEAD~2").unwrap();
        assert!(cmp.iter().any(|d| d.path.ends_with("c.py")));
    }

    #[test]
    fn working_tree_diffs() {
        let (dir, r) = repo();
        write(dir.path(), "a.py", "line1\nchanged\nline3\n");
        write(dir.path(), "n.py", "new\n");
        let text = r.diff_file_to_head(&dir.path().join("a.py")).unwrap();
        assert!(text.contains("-line2") && text.contains("+changed"));
        let all = r.diff_working_tree(None).unwrap();
        assert_eq!(all.len(), 2);
        let a = all.iter().find(|d| d.path.ends_with("a.py")).unwrap();
        assert_eq!(
            (a.status, a.additions, a.deletions),
            (FileStatus::Modified, 1, 1)
        );
        assert!(a.patch.contains("+changed"));
        let n = all.iter().find(|d| d.path.ends_with("n.py")).unwrap();
        assert_eq!(n.status, FileStatus::Untracked);
        assert!(n.patch.contains("+new"));
        let only = r.diff_working_tree(Some(&dir.path().join("n.py"))).unwrap();
        assert_eq!(only.len(), 1);
    }

    #[test]
    fn reset_cherry_pick_revert_checkout() {
        let (dir, r) = repo();
        let first = r.log(1).unwrap()[0].id.clone();
        write(dir.path(), "a.py", "line1\nline2\nline3\nmore\n");
        let second = r
            .commit(&[dir.path().join("a.py")], "second", false)
            .unwrap();
        r.reset(&first, ResetMode::Soft).unwrap();
        assert_eq!(r.log(1).unwrap()[0].id, first);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.py")).unwrap(),
            "line1\nline2\nline3\nmore\n"
        );
        r.reset(&first, ResetMode::Hard).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.py")).unwrap(),
            "line1\nline2\nline3\n"
        );
        // Cherry-pick the orphaned second commit back.
        assert!(r.cherry_pick(&second).unwrap().is_empty());
        let head = r.log(1).unwrap()[0].clone();
        assert_eq!(head.summary, "second");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.py")).unwrap(),
            "line1\nline2\nline3\nmore\n"
        );
        assert_eq!(r.current_operation(), None);
        assert!(r.revert_commit(&head.id).unwrap().is_empty());
        let head2 = r.log(1).unwrap()[0].clone();
        assert!(head2.summary.starts_with("Revert \"second\""));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.py")).unwrap(),
            "line1\nline2\nline3\n"
        );
        r.checkout_commit(&first).unwrap();
        assert!(r.current_branch().unwrap().starts_with("HEAD detached"));
    }

    #[test]
    fn blame_large_file_is_fast() {
        let (dir, r) = repo();
        let big: String = (0..5000).map(|i| format!("line {i}\n")).collect();
        let p = write(dir.path(), "big.py", &big);
        r.commit(std::slice::from_ref(&p), "big 1", false).unwrap();
        for step in 1..=5 {
            let text: String = (0..5000)
                .map(|i| {
                    if i % 97 == step {
                        format!("edit{step} {i}\n")
                    } else {
                        format!("line {i}\n")
                    }
                })
                .collect();
            write(dir.path(), "big.py", &text);
            r.commit(
                std::slice::from_ref(&p),
                &format!("big {}", step + 1),
                false,
            )
            .unwrap();
        }
        let start = std::time::Instant::now();
        let b = r.blame(&p).unwrap();
        assert_eq!(b.len(), 5000);
        assert!(b.iter().all(|l| !l.is_uncommitted));
        assert!(b.iter().any(|l| l.summary == "big 6"));
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "blame took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn remotes_and_gitignore() {
        let (dir, r) = repo();
        r.add_remote("origin", "https://example.com/repo.git")
            .unwrap();
        assert_eq!(
            r.remotes_with_urls().unwrap(),
            vec![(
                "origin".to_string(),
                "https://example.com/repo.git".to_string()
            )]
        );
        assert_eq!(r.remotes(), vec!["origin".to_string()]);
        r.remove_remote("origin").unwrap();
        assert!(r.remotes_with_urls().unwrap().is_empty());
        r.add_to_gitignore("*.log").unwrap();
        r.add_to_gitignore("*.log").unwrap();
        add_to_gitignore(dir.path(), "build/").unwrap();
        let text = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(text, "*.log\nbuild/\n");
        write(dir.path(), "x.log", "");
        assert!(r.is_ignored(&dir.path().join("x.log")));
        assert!(!r.is_ignored(&dir.path().join("a.py")));
    }
}
