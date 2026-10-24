//! Cancellable log indexing and explicit revision operations.
use crate::{GitEnv, GitJob, GitRepo, GraphCommit};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver},
        Arc,
    },
};

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Query {
    pub text: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub branches: Vec<String>,
    pub all: bool,
    pub author: String,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub path: Option<PathBuf>,
    pub linear_bek: bool,
    pub highlight: usize,
}
pub struct Page {
    pub graph: Vec<GraphCommit>,
    pub email: String,
    pub current: std::collections::HashSet<String>,
    pub nonpicked: std::collections::HashSet<String>,
}
pub struct Index {
    pub started: std::time::Instant,
    cancel: Arc<AtomicBool>,
    pub visited: Arc<AtomicUsize>,
    result: Receiver<Result<Page, String>>,
}
impl Drop for Index {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}
impl Index {
    pub fn start(root: PathBuf, query: Query, limit: usize) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let visited = Arc::new(AtomicUsize::new(0));
        let (tx, result) = mpsc::channel();
        let stopped = cancel.clone();
        let progress = visited.clone();
        std::thread::spawn(move || {
            let rows = GitRepo::discover(&root)
                .ok_or_else(|| anyhow::anyhow!("Repository unavailable"))
                .and_then(|repo| {
                    let graph = repo.query_log(&query, limit, &stopped, &progress)?;
                    let current = if query.highlight == 3 || query.highlight == 4 {
                        graph
                            .iter()
                            .filter(|c| repo.log_contains("HEAD", &c.id))
                            .map(|c| c.id.clone())
                            .collect()
                    } else {
                        std::collections::HashSet::new()
                    };
                    let mut nonpicked = std::collections::HashSet::new();
                    if query.highlight == 4 {
                        let mut patches = std::collections::HashSet::new();
                        let mut walk = repo.repo.revwalk()?;
                        if walk.push_head().is_ok() {
                            for id in walk {
                                if stopped.load(Ordering::Acquire) {
                                    anyhow::bail!("Log indexing cancelled");
                                }
                                progress.fetch_add(1, Ordering::Relaxed);
                                if let Some(patch) = repo.log_patch_id(id?)? {
                                    patches.insert(patch);
                                }
                            }
                        }
                        for c in &graph {
                            if !current.contains(&c.id)
                                && repo
                                    .log_patch_id(git2::Oid::from_str(&c.id)?)?
                                    .is_none_or(|patch| !patches.contains(&patch))
                            {
                                nonpicked.insert(c.id.clone());
                            }
                        }
                    }
                    Ok(Page {
                        graph,
                        email: repo.log_identity(),
                        current,
                        nonpicked,
                    })
                })
                .map_err(|e| e.to_string());
            let _ = tx.send(rows);
        });
        Self {
            started: std::time::Instant::now(),
            cancel,
            visited,
            result,
        }
    }
    pub fn poll(&self) -> Option<Result<Page, String>> {
        self.result.try_recv().ok()
    }
}
impl GitRepo {
    fn log_patch_id(&self, id: git2::Oid) -> anyhow::Result<Option<git2::Oid>> {
        let commit = self.repo.find_commit(id)?;
        if commit.parent_count() > 1 {
            return Ok(None);
        }
        let tree = commit.tree()?;
        let parent = commit.parent(0).ok().and_then(|p| p.tree().ok());
        let diff = self
            .repo
            .diff_tree_to_tree(parent.as_ref(), Some(&tree), None)?;
        if diff.deltas().len() == 0 {
            return Ok(None);
        }
        Ok(Some(diff.patchid(None)?))
    }
    pub fn query_log(
        &self,
        q: &Query,
        limit: usize,
        cancel: &AtomicBool,
        visited: &AtomicUsize,
    ) -> anyhow::Result<Vec<GraphCommit>> {
        let matcher = if q.text.is_empty() {
            None
        } else {
            Some(
                regex::RegexBuilder::new(&if q.regex {
                    q.text.clone()
                } else {
                    regex::escape(&q.text)
                })
                .case_insensitive(!q.case_sensitive)
                .build()?,
            )
        };
        let mut walk = self.repo.revwalk()?;
        walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;
        if !q.branches.is_empty() {
            for branch in &q.branches {
                walk.push(self.repo.revparse_single(branch)?.peel_to_commit()?.id())?;
            }
        } else if q.all {
            let _ = walk.push_glob("refs/heads/*");
            let _ = walk.push_glob("refs/remotes/*");
            let _ = walk.push_glob("refs/tags/*");
        } else if walk.push_head().is_err() {
            return Ok(Vec::new());
        }
        let mut refs = std::collections::HashMap::<git2::Oid, Vec<String>>::new();
        for reference in self.repo.references()?.flatten() {
            if let Ok(commit) = reference.peel_to_commit() {
                let name = reference.name().unwrap_or("").to_string();
                refs.entry(commit.id()).or_default().push(name);
            }
        }
        if let Ok(head) = self.repo.head().and_then(|h| h.peel_to_commit()) {
            refs.entry(head.id()).or_default().push("HEAD".into());
        }
        let me = self
            .repo
            .signature()
            .ok()
            .and_then(|s| s.email().ok().map(str::to_owned))
            .unwrap_or_default();
        let author = if q.author == "me" { &me } else { &q.author };
        anyhow::ensure!(
            q.author != "me" || !me.is_empty(),
            "Configure Git user.email before filtering by me"
        );
        let mut rows = Vec::new();
        for id in walk {
            if cancel.load(Ordering::Acquire) {
                anyhow::bail!("Log indexing cancelled");
            }
            visited.fetch_add(1, Ordering::Relaxed);
            let c = self.repo.find_commit(id?)?;
            if matcher.as_ref().is_some_and(|pattern| {
                !pattern.is_match(&format!(
                    "{} {} {} {}",
                    c.message().unwrap_or(""),
                    c.author().name().unwrap_or(""),
                    c.author().email().unwrap_or(""),
                    c.id()
                ))
            }) {
                continue;
            }
            if q.since.is_some_and(|s| c.time().seconds() < s)
                || q.until.is_some_and(|s| c.time().seconds() > s)
            {
                continue;
            }
            if !author.is_empty()
                && !format!(
                    "{} {}",
                    c.author().name().unwrap_or(""),
                    c.author().email().unwrap_or("")
                )
                .to_lowercase()
                .contains(&author.to_lowercase())
            {
                continue;
            }
            if let Some(path) = &q.path {
                let path = path.strip_prefix(&self.root).unwrap_or(path);
                let tree = c.tree()?;
                let parents = if c.parent_count() == 0 {
                    vec![None]
                } else {
                    c.parents().map(|p| p.tree().ok()).collect()
                };
                let mut touches = false;
                for parent in parents {
                    let mut options = git2::DiffOptions::new();
                    options.pathspec(path);
                    if self
                        .repo
                        .diff_tree_to_tree(parent.as_ref(), Some(&tree), Some(&mut options))?
                        .deltas()
                        .len()
                        > 0
                    {
                        touches = true;
                        break;
                    }
                }
                if !touches {
                    continue;
                }
            }
            let id = c.id().to_string();
            rows.push(GraphCommit {
                short_id: id[..8].into(),
                id,
                summary: c.summary().ok().flatten().unwrap_or("").into(),
                message: c.message().unwrap_or("").into(),
                author: c.author().name().unwrap_or("").into(),
                email: c.author().email().unwrap_or("").into(),
                time: c.time().seconds(),
                parents: c.parent_ids().map(|p| p.to_string()).collect(),
                refs: refs.remove(&c.id()).unwrap_or_default(),
                lane: 0,
                lanes_in: Vec::new(),
                lane_count: 1,
            });
            if rows.len() >= limit {
                break;
            }
        }
        if q.linear_bek {
            rows = linear_bek(rows);
        }
        crate::graph::assign_lanes(&mut rows);
        Ok(rows)
    }
    pub fn resolve_log_revision(&self, name: &str) -> anyhow::Result<String> {
        Ok(self
            .repo
            .revparse_single(name)?
            .peel_to_commit()?
            .id()
            .to_string())
    }
    pub fn log_identity(&self) -> String {
        self.repo
            .signature()
            .ok()
            .and_then(|s| s.email().ok().map(str::to_owned))
            .unwrap_or_default()
    }
    pub fn log_github_url(&self, id: &str) -> Option<String> {
        git2::Oid::from_str(id).ok()?;
        let remote = self.repo.find_remote("origin").ok()?;
        let url = remote.url().ok()?;
        let path = url
            .strip_prefix("https://github.com/")
            .or_else(|| url.strip_prefix("git@github.com:"))
            .or_else(|| url.strip_prefix("ssh://git@github.com/"))?
            .trim_end_matches('/')
            .trim_end_matches(".git");
        if path.split('/').count() != 2
            || path
                .chars()
                .any(|c| !(c.is_ascii_alphanumeric() || "_-/.".contains(c)))
        {
            return None;
        }
        Some(format!("https://github.com/{path}/commit/{id}"))
    }
    pub fn log_contains(&self, branch: &str, id: &str) -> bool {
        self.repo
            .revparse_single(branch)
            .ok()
            .and_then(|o| o.peel_to_commit().ok())
            .is_some_and(|tip| {
                git2::Oid::from_str(id).ok().is_some_and(|id| {
                    tip.id() == id || self.repo.graph_descendant_of(tip.id(), id).unwrap_or(false)
                })
            })
    }
    pub fn log_parent_diff(&self, id: &str, parent: usize) -> anyhow::Result<Vec<crate::FileDiff>> {
        let c = self.repo.find_commit(git2::Oid::from_str(id)?)?;
        self.diff_commits(&c.parent_id(parent)?.to_string(), id)
    }
    pub fn log_diff_local(&self, id: &str) -> anyhow::Result<String> {
        let tree = self.repo.revparse_single(id)?.peel_to_tree()?;
        let diff = self
            .repo
            .diff_tree_to_workdir_with_index(Some(&tree), None)?;
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
    pub fn log_patch(&self, id: &str) -> anyhow::Result<String> {
        let c = self.repo.revparse_single(id)?.peel_to_commit()?;
        let tree = c.tree()?;
        let parent = c.parent(0).ok().and_then(|p| p.tree().ok());
        let diff = self
            .repo
            .diff_tree_to_tree(parent.as_ref(), Some(&tree), None)?;
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
}

/// Prefer continuing the previous first-parent lane among topologically ready
/// commits. Side branches remain present and joins never precede their children.
fn linear_bek(rows: Vec<GraphCommit>) -> Vec<GraphCommit> {
    let positions = rows
        .iter()
        .enumerate()
        .map(|(n, c)| (c.id.clone(), n))
        .collect::<std::collections::HashMap<_, _>>();
    let mut children = vec![0usize; rows.len()];
    for c in &rows {
        for p in &c.parents {
            if let Some(n) = positions.get(p) {
                children[*n] += 1;
            }
        }
    }
    let mut ready = std::collections::BTreeSet::new();
    for (n, c) in rows.iter().enumerate() {
        if children[n] == 0 {
            ready.insert((c.time, n));
        }
    }
    let mut preferred = None;
    let mut sorted = Vec::with_capacity(rows.len());
    while !ready.is_empty() {
        let key = preferred
            .take()
            .filter(|n: &usize| ready.contains(&(rows[*n].time, *n)))
            .map(|n| (rows[n].time, n))
            .unwrap_or_else(|| *ready.last().unwrap());
        ready.remove(&key);
        let c = &rows[key.1];
        for p in &c.parents {
            if let Some(n) = positions.get(p) {
                children[*n] -= 1;
                if children[*n] == 0 {
                    ready.insert((rows[*n].time, *n));
                }
            }
        }
        preferred = c.parents.first().and_then(|p| positions.get(p)).copied();
        sorted.push(c.clone());
    }
    sorted
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RebaseAction {
    Pick,
    Reword,
    Edit,
    Squash,
    Fixup,
    Drop,
}
impl RebaseAction {
    pub const ALL: [Self; 6] = [
        Self::Pick,
        Self::Reword,
        Self::Edit,
        Self::Squash,
        Self::Fixup,
        Self::Drop,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Pick => "pick",
            Self::Reword => "reword",
            Self::Edit => "edit",
            Self::Squash => "squash",
            Self::Fixup => "fixup",
            Self::Drop => "drop",
        }
    }
}
#[derive(Clone, Debug)]
pub struct RebaseEntry {
    pub id: String,
    pub action: RebaseAction,
    pub message: String,
}
pub struct RebasePlan {
    pub preserve_merges: bool,
    pub base: String,
    pub original_head: String,
    pub entries: Vec<RebaseEntry>,
}
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
impl GitRepo {
    pub fn log_rebase_plan(&self, from: &str) -> anyhow::Result<RebasePlan> {
        let commit = self.repo.revparse_single(from)?.peel_to_commit()?;
        let base = if commit.parent_count() == 0 {
            String::new()
        } else {
            commit.parent_id(0)?.to_string()
        };
        let original_head = self.resolve_log_revision("HEAD")?;
        anyhow::ensure!(
            self.log_contains("HEAD", from),
            "Revision is not on the current branch"
        );
        let mut walk = self.repo.revwalk()?;
        walk.push_head()?;
        if !base.is_empty() {
            walk.hide(git2::Oid::from_str(&base)?)?;
        }
        walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::REVERSE)?;
        let mut entries = Vec::new();
        for id in walk {
            let c = self.repo.find_commit(id?)?;
            entries.push(RebaseEntry {
                id: c.id().to_string(),
                action: RebaseAction::Pick,
                message: c.message().unwrap_or("").into(),
            });
        }
        let preserve_merges = entries.iter().any(|entry| {
            self.repo
                .find_commit(git2::Oid::from_str(&entry.id).unwrap())
                .map(|c| c.parent_count() > 1)
                .unwrap_or(false)
        });
        Ok(RebasePlan {
            preserve_merges,
            base,
            original_head,
            entries,
        })
    }
    pub fn start_log_rebase(&self, plan: &RebasePlan) -> anyhow::Result<GitJob> {
        anyhow::ensure!(
            self.resolve_log_revision("HEAD")? == plan.original_head,
            "HEAD changed; reopen the rebase plan"
        );
        anyhow::ensure!(
            self.status()?.is_empty(),
            "Commit or stash changes before rewriting commits"
        );
        anyhow::ensure!(
            self.repo.state() == git2::RepositoryState::Clean,
            "Another Git operation is in progress"
        );
        anyhow::ensure!(!plan.entries.is_empty(), "Empty rebase plan");
        let mut walk = self.repo.revwalk()?;
        walk.push_head()?;
        if !plan.base.is_empty() {
            walk.hide(git2::Oid::from_str(&plan.base)?)?;
        }
        let expected = walk.collect::<Result<std::collections::HashSet<_>, _>>()?;
        let actual = plan
            .entries
            .iter()
            .map(|entry| git2::Oid::from_str(&entry.id))
            .collect::<Result<std::collections::HashSet<_>, _>>()?;
        anyhow::ensure!(
            actual == expected && actual.len() == plan.entries.len(),
            "Rebase plan must contain every original commit exactly once; use drop to omit a commit"
        );
        let mut seen = std::collections::HashSet::new();
        let folder = self.repo.path().join("rustpy-log-plans").join(format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir_all(&folder)?;
        let mut todo = String::new();
        let mut kept = false;
        let mut previous_kept = None::<git2::Oid>;
        for (n, entry) in plan.entries.iter().enumerate() {
            let id = git2::Oid::from_str(&entry.id)?;
            let commit = self.repo.find_commit(id)?;
            let parents = commit.parent_ids().collect::<Vec<_>>();
            let merging = parents.len() > 1;
            let combined = matches!(entry.action, RebaseAction::Squash | RebaseAction::Fixup);
            anyhow::ensure!(seen.insert(id), "Duplicate commit in rebase plan");
            anyhow::ensure!(!combined || kept, "Squash/fixup needs a preceding commit");
            if !plan.preserve_merges && merging {
                anyhow::ensure!(
                    matches!(entry.action, RebaseAction::Pick | RebaseAction::Drop),
                    "Enable Preserve merges to reword or edit a merge commit"
                );
                todo.push_str(&format!("drop {id}\n"));
                continue;
            }
            if plan.preserve_merges {
                for parent in &parents {
                    anyhow::ensure!(
                        !actual.contains(parent) || seen.contains(parent),
                        "Preserving topology requires parents before children; disable Preserve merges to flatten and freely reorder"
                    );
                }
                anyhow::ensure!(
                    !merging || !combined,
                    "A merge commit cannot be squashed/fixed up while preserving topology"
                );
                if combined {
                    anyhow::ensure!(
                        parents.first().copied() == previous_kept,
                        "Squash/fixup must immediately follow its first parent when preserving topology"
                    );
                } else {
                    let first = parents
                        .first()
                        .map(|p| {
                            if actual.contains(p) {
                                format!("rustpy-{p}")
                            } else {
                                p.to_string()
                            }
                        })
                        .unwrap_or_else(|| "[new root]".into());
                    todo.push_str(&format!("reset {first}\n"));
                }
            }
            if entry.action != RebaseAction::Drop {
                if merging {
                    todo.push_str(&format!("merge -C {id}"));
                    for parent in parents.iter().skip(1) {
                        todo.push(' ');
                        todo.push_str(&if actual.contains(parent) {
                            format!("rustpy-{parent}")
                        } else {
                            parent.to_string()
                        });
                    }
                    todo.push('\n');
                    if entry.action == RebaseAction::Edit {
                        todo.push_str("break\n");
                    }
                } else {
                    let action = if entry.action == RebaseAction::Reword {
                        "pick"
                    } else {
                        entry.action.label()
                    };
                    todo.push_str(&format!("{action} {id}\n"));
                }
                kept = true;
                previous_kept = Some(id);
            } else if !plan.preserve_merges {
                todo.push_str(&format!("drop {id}\n"));
            }
            if entry.action == RebaseAction::Reword {
                anyhow::ensure!(!entry.message.trim().is_empty(), "Reword message is empty");
                let path = folder.join(format!("message-{n}"));
                std::fs::write(&path, &entry.message)?;
                todo.push_str(&format!(
                    "exec {} commit --amend -F {}\n",
                    shell_quote(&crate::git_program().replace('\\', "/")),
                    shell_quote(&path.to_string_lossy().replace('\\', "/"))
                ));
            }
            if plan.preserve_merges {
                if combined {
                    if let Some(parent) = parents.first() {
                        todo.push_str(&format!("label rustpy-{parent}\n"));
                    }
                }
                todo.push_str(&format!("label rustpy-{id}\n"));
            }
        }
        if plan.preserve_merges {
            todo.push_str(&format!("reset rustpy-{}\n", plan.original_head));
        }
        let path = folder.join("todo");
        std::fs::write(&path, todo)?;
        GitJob::spawn(
            &self.root,
            &[
                "rebase".into(),
                "--interactive".into(),
                "--no-autosquash".into(),
                if plan.preserve_merges {
                    "--rebase-merges".into()
                } else {
                    "--no-rebase-merges".into()
                },
                if plan.base.is_empty() {
                    "--root".into()
                } else {
                    plan.base.clone()
                },
            ],
            &GitEnv {
                vars: vec![
                    (
                        "GIT_SEQUENCE_EDITOR".into(),
                        format!(
                            "cp {}",
                            shell_quote(&path.to_string_lossy().replace('\\', "/"))
                        ),
                    ),
                    ("GIT_EDITOR".into(), "true".into()),
                ],
            },
        )
    }
    pub fn start_log_command(&self, args: Vec<String>) -> anyhow::Result<GitJob> {
        if args.first().is_some_and(|a| a == "reset") && args.get(1).is_some_and(|a| a == "--mixed")
        {
            if let Some(expected) = args.get(2).and_then(|target| target.strip_suffix('^')) {
                anyhow::ensure!(
                    self.resolve_log_revision("HEAD")? == expected,
                    "HEAD changed; reopen Undo Commit"
                );
            }
        }
        if args.first().is_some_and(|a| a == "branch") {
            anyhow::ensure!(
                args.get(1)
                    .is_some_and(|name| crate::is_valid_branch_name(name)),
                "Invalid branch name"
            );
        }
        GitJob::spawn(&self.root, &args, &GitEnv::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    #[test]
    fn sequence_editor_paths_are_single_shell_arguments() {
        assert_eq!(shell_quote("a'b c"), "'a'\\''b c'");
    }
    #[test]
    fn action_names_are_git_todo_commands() {
        assert_eq!(
            RebaseAction::ALL.map(|a| a.label()),
            ["pick", "reword", "edit", "squash", "fixup", "drop"]
        );
    }
    fn commit(repo: &git2::Repository, path: &str, text: &str, message: &str) -> git2::Oid {
        let root = repo.workdir().unwrap();
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new(path)).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents = parent.iter().collect::<Vec<_>>();
        let signature = git2::Signature::now("Ada", "ada@example.test").unwrap();
        repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            message,
            &tree,
            &parents,
        )
        .unwrap()
    }
    #[test]
    fn index_searches_full_messages_and_folder_history_before_limiting_results() {
        let dir = tempfile::tempdir().unwrap();
        let native = git2::Repository::init(dir.path()).unwrap();
        let first = commit(
            &native,
            "pkg/mod.py",
            "x=1\n",
            "Initial\n\nISSUE-42 details",
        );
        commit(&native, "other.py", "y=1\n", "Unrelated latest commit");
        let repo = GitRepo::discover(dir.path()).unwrap();
        let progress = AtomicUsize::new(0);
        let cancel = AtomicBool::new(false);
        let query = Query {
            text: r"ISSUE-\d+".into(),
            regex: true,
            path: Some("pkg".into()),
            ..Default::default()
        };
        let rows = repo.query_log(&query, 1, &cancel, &progress).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, first.to_string());
        assert!(progress.load(Ordering::Relaxed) >= 2);
        cancel.store(true, Ordering::Release);
        assert!(repo
            .query_log(&Query::default(), 10, &cancel, &progress)
            .is_err());
    }
    #[test]
    fn rebase_plan_rejects_omitted_commits_before_starting_git() {
        let dir = tempfile::tempdir().unwrap();
        let native = git2::Repository::init(dir.path()).unwrap();
        let first = commit(&native, "a.py", "a=1\n", "First");
        commit(&native, "a.py", "a=2\n", "Second");
        let repo = GitRepo::discover(dir.path()).unwrap();
        let mut plan = repo.log_rebase_plan(&first.to_string()).unwrap();
        assert_eq!(plan.entries.len(), 2);
        assert!(plan.base.is_empty());
        plan.entries.remove(0);
        let error = match repo.start_log_rebase(&plan) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("invalid plan unexpectedly started"),
        };
        assert!(error.contains("every original commit"));
    }
    #[test]
    fn linear_bek_preserves_side_branches_and_parent_order() {
        let row = |id: &str, time: i64, parents: &[&str]| GraphCommit {
            id: id.into(),
            short_id: id.into(),
            summary: String::new(),
            message: String::new(),
            author: String::new(),
            email: String::new(),
            time,
            parents: parents.iter().map(|s| s.to_string()).collect(),
            refs: vec![],
            lane: 0,
            lanes_in: vec![],
            lane_count: 1,
        };
        let sorted = linear_bek(vec![
            row("merge", 5, &["main", "side"]),
            row("side", 4, &["base"]),
            row("main", 3, &["base"]),
            row("base", 1, &[]),
        ]);
        assert_eq!(
            sorted.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["merge", "main", "side", "base"]
        );
    }
    #[test]
    fn interactive_plan_includes_merges_and_defaults_to_preserving_topology() {
        let dir = tempfile::tempdir().unwrap();
        let native = git2::Repository::init(dir.path()).unwrap();
        let base = commit(&native, "a.py", "a=1\n", "Base");
        let main = commit(&native, "a.py", "a=2\n", "Main");
        let base_commit = native.find_commit(base).unwrap();
        let main_commit = native.find_commit(main).unwrap();
        let tree = main_commit.tree().unwrap();
        let signature = git2::Signature::now("Ada", "ada@example.test").unwrap();
        let side = native
            .commit(None, &signature, &signature, "Side", &tree, &[&base_commit])
            .unwrap();
        let side_commit = native.find_commit(side).unwrap();
        let merge = native
            .commit(
                Some("HEAD"),
                &signature,
                &signature,
                "Merge",
                &tree,
                &[&main_commit, &side_commit],
            )
            .unwrap();
        let repo = GitRepo::discover(dir.path()).unwrap();
        let plan = repo.log_rebase_plan(&base.to_string()).unwrap();
        assert!(plan.preserve_merges);
        assert_eq!(plan.entries.len(), 4);
        assert_eq!(plan.entries.last().unwrap().id, merge.to_string());
    }
}
