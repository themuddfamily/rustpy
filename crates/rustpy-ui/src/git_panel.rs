//! Git tool window: Local Changes (commit), Log, and branch operations
//! (roadmap §4.1).

mod log_workspace;
mod operations_workspace;

pub mod branch_workspace;
use crate::a11y::DialogWindow;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use egui::{Color32, Sense, Vec2};
use rustpy_vcs::{CommitFile, CommitInfo, FileStatus, GitRepo, StatusEntry};

use crate::diff_view::SideBySide;
use crate::git_jobs::{GitJobs, GitOp};
use crate::history_store;
use crate::theme::UiTheme;
pub mod commit_workspace;

/// A destructive operation waiting for the user's confirmation.
#[derive(Clone, Debug, PartialEq)]
enum Confirm {
    /// Rollback of one file. `deletes`: the file is not in HEAD, so a rollback
    /// deletes it; then only `delete_local` (off by default) allows that, otherwise
    /// the file is just unstaged. `renamed_from`: the file is a rename of that
    /// path, which the rollback restores either way.
    Rollback {
        path: PathBuf,
        deletes: bool,
        delete_local: bool,
        renamed_from: Option<PathBuf>,
    },
    /// Delete an unversioned file.
    DeleteUnversioned(PathBuf),
    /// Reset Current Branch to Here › Hard.
    ResetHard(String),
    DropStash {
        index: usize,
        message: String,
    },
    /// Abort the merge / rebase / cherry-pick in progress.
    Abort(String),
    /// Skip the commit a rebase stopped on.
    SkipCommit,
    /// Push an amended commit that replaces one already on the upstream branch.
    ForcePush(String),
}

impl Confirm {
    fn title(&self) -> &'static str {
        match self {
            Confirm::Rollback { .. } => "Rollback Changes",
            Confirm::DeleteUnversioned(_) => "Delete Unversioned File",
            Confirm::ResetHard(_) => "Reset --hard",
            Confirm::DropStash { .. } => "Drop Stash",
            Confirm::Abort(_) => "Abort Operation",
            Confirm::SkipCommit => "Skip Commit",
            Confirm::ForcePush(_) => "Force Push",
        }
    }

    fn button(&self) -> &'static str {
        match self {
            Confirm::Rollback {
                deletes: true,
                delete_local: false,
                renamed_from: None,
                ..
            } => "Unstage",
            Confirm::Rollback { .. } => "Rollback",
            Confirm::DeleteUnversioned(_) => "Delete",
            Confirm::ResetHard(_) => "Reset",
            Confirm::DropStash { .. } => "Drop",
            Confirm::Abort(_) => "Abort",
            Confirm::SkipCommit => "Skip",
            Confirm::ForcePush(_) => "Force Push",
        }
    }

    fn message(&self, root: &Path) -> String {
        let rel = |p: &Path| p.strip_prefix(root).unwrap_or(p).display().to_string();
        match self {
            Confirm::Rollback { path, deletes: false, .. } => format!("Discard the changes to {} and restore the committed version?", rel(path)),
            Confirm::Rollback { path, deletes: true, renamed_from: Some(old), .. } => format!("{} was renamed from {}: rolling it back restores {} as committed. {} is kept as an unversioned file unless you delete it below.", rel(path), rel(old), rel(old), rel(path)),
            Confirm::Rollback { path, deletes: true, .. } => format!("{} is not in the last commit: rolling it back removes it from Git. The local file is kept unless you delete it below.", rel(path)),
            Confirm::DeleteUnversioned(path) => format!("Delete the unversioned file {}?", rel(path)),
            Confirm::ResetHard(id) => format!("Reset the current branch to {} and discard all uncommitted changes in tracked files?", &id[..7.min(id.len())]),
            Confirm::DropStash { index, message } => format!("Drop stash@{{{index}}} ({message})? Its changes cannot be recovered."),
            Confirm::Abort(op) => format!("Abort the {op} in progress? Conflict resolutions and other changes made during it are discarded."),
            Confirm::SkipCommit => "Skip the commit the rebase stopped on? Its changes are left out of the rebased branch and the conflict resolutions made for it are discarded.".to_string(),
            Confirm::ForcePush(upstream) => format!("The amended commit replaces one that is already on {upstream}, so a normal push is rejected. Force push with --force-with-lease? It is refused if someone else pushed to {upstream} since your last fetch."),
        }
    }

    /// Whether the working tree content is copied to Local History first.
    fn keeps_history(&self) -> bool {
        !matches!(
            self,
            Confirm::DropStash { .. }
                | Confirm::Rollback {
                    deletes: true,
                    delete_local: false,
                    ..
                }
                | Confirm::ForcePush(_)
        )
    }

    /// What the dialog says about Local History (None: nothing is kept).
    fn history_note(&self) -> Option<&'static str> {
        if !self.keeps_history() {
            return None;
        }
        Some(match self {
            Confirm::Rollback { path, .. } | Confirm::DeleteUnversioned(path) if path.is_file() && !history_store::can_snapshot(path) => "This file is binary or larger than 2 MB, so Local History cannot keep a copy: its current content will be lost.",
            Confirm::Rollback { .. } | Confirm::DeleteUnversioned(_) => "The current content is copied to Local History first.",
            _ => "Changed text files are copied to Local History first (binary files and files over 2 MB are not).",
        })
    }
}

/// Copies the files Git reports as changed (and, with `untracked`, unversioned
/// files) to Local History before an operation rewrites the working tree.
pub fn snapshot_working_tree(repo: &GitRepo, root: &Path, label: &str, untracked: bool) -> usize {
    snapshot_working_tree_in(&history_store::base_dir(), repo, root, label, untracked)
}

fn snapshot_working_tree_in(
    base: &Path,
    repo: &GitRepo,
    root: &Path,
    label: &str,
    untracked: bool,
) -> usize {
    let paths: Vec<PathBuf> = repo
        .status()
        .unwrap_or_default()
        .into_iter()
        .filter(|e| untracked || e.status != FileStatus::Untracked)
        .map(|e| e.path)
        .collect();
    if paths.is_empty() {
        return 0;
    }
    history_store::snapshot_paths(base, Some(root), &paths, label, false)
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum GitTab {
    LocalChanges,
    Branches,
    Log,
    Shelf,
    /// Git commands the IDE ran and their output.
    Console,
}

pub enum GitEvent {
    GitHubCommit {
        root: PathBuf,
        commit: String,
    },
    BranchWorkspace(branch_workspace::Event),
    Invoke {
        root: PathBuf,
        action: crate::actions::Action,
    },
    OpenRepository(PathBuf),
    OperationsSettings(rustpy_settings::vcs_operations::Settings),
    OpenFile(PathBuf),
    /// Generate .gitignore: the template picker for this folder's file.
    GitignoreTemplates(PathBuf),
    ApplyDiff(crate::diff_view::ApplyEdit),
    CommitWorkspace(commit_workspace::Event),
    /// Open the conflict resolver for a path.
    ResolveConflict(PathBuf),
    /// Navigate to a commit in the log (e.g. from annotations).
    ShowCommit(String),
    Status(String),
    /// Files on disk changed (rollback/checkout): reload editors.
    FilesChanged,
    /// "Generate with AI" in the commit message box.
    GenerateCommitMessage,
    /// Push the current branch (directly, or through the Push dialog).
    Push,
    /// `git fetch --all --prune` (a cancellable background task).
    Fetch,
    /// Pull: Update Project with merge or rebase (run in the background by the app).
    Update,
    /// Commit / Commit and Push… was clicked: the app saves the editors, then
    /// calls [`GitPanel::commit_checked`] (and pushes when `push`).
    Commit {
        push: bool,
    },
    /// Force push (`--force-with-lease`) after amending a pushed commit, confirmed by the user.
    ForcePush,
    /// Fetch or Pull through the Git console (git_jobs.rs).
    Run(GitOp),
}

/// What [`GitPanel::commit_checked`] did.
#[derive(Debug, PartialEq)]
pub struct Committed {
    /// Status line ("Committed 2 file(s) as 1a2b3c4").
    pub status: String,
    /// An amend replaced a commit that is already on the upstream branch, so a
    /// normal push would be rejected.
    pub rewrote_pushed: bool,
}

/// "Create Tag…" prompt for a Log row.
struct PendingTag {
    target: String,
    name: String,
    message: String,
    focus: bool,
}

pub struct GitPanel {
    pub operations_workspace: operations_workspace::Workspace,
    pub log_workspace: log_workspace::Workspace,
    pub commit_workspace: commit_workspace::Workspace,
    pub branch_workspace: branch_workspace::Workspace,
    accessibility_owner: bool,
    /// Grammar hints under the commit message (Settings › Inspections › Grammar).
    pub check_grammar: bool,
    pub grammar_policy: Result<rustpy_grammar::policy::Policy, rustpy_grammar::policy::PolicyError>,
    pub tab: GitTab,
    /// Lane layout for the log (`log` is built from the same rows).
    pub graph: Vec<rustpy_vcs::GraphCommit>,
    pub all_branches: bool,
    /// Log restricted to one file's history.
    pub log_filter: Option<PathBuf>,
    /// Commits loaded into the Log ("Load more" raises it).
    pub log_limit: usize,
    /// Log filter text: matches summary, author and hash, case-insensitively.
    pub log_query: String,
    pending_tag: Option<PendingTag>,
    pub conflicts: Vec<PathBuf>,
    pub operation: Option<String>,
    /// The operation whose prepared message (`MERGE_MSG`) was put in the message box, and that text.
    prefilled: Option<(String, String)>,
    pub stashes: Vec<rustpy_vcs::StashEntry>,
    pub stash_message: String,
    pub stash_untracked: bool,
    /// Git Branches: "New Branch from…" prompt `(start point, new name, checkout)`.
    pub branch_from: Option<(String, String, bool)>,
    /// Git Branches: a checkout refused because of local changes, offered as Smart Checkout.
    pub blocked_checkout: Option<String>,
    /// Local Changes preview as unified text instead of side by side.
    pub unified_preview: bool,
    local_diff: Option<SideBySide>,
    pub entries: Vec<StatusEntry>,
    /// Paths of `entries` at the last refresh (to tell new changes apart).
    seen: HashSet<PathBuf>,
    pub checked: HashSet<PathBuf>,
    pub message: String,
    pub amend: bool,
    pub log: Vec<CommitInfo>,
    pub selected_commit: Option<String>,
    commit_files: Vec<CommitFile>,
    selected_file: Option<PathBuf>,
    diff_text: String,
    pub last_refresh: f64,
    pub focus_message: bool,
    pub new_branch_name: String,
    pub branch_popup: bool,
    /// Git jobs on worker threads (Update Project, Pull): their status lines
    /// arrive here (see `IdeApp::poll_git_jobs`).
    pub jobs: Vec<std::sync::mpsc::Receiver<String>>,
    /// Destructive operation waiting for confirmation.
    confirm: Option<Confirm>,
    /// Local History note of the pending confirmation, keyed by its message.
    confirm_note: Option<(String, Option<&'static str>)>,
    /// Project root (Local History is kept per project).
    root: PathBuf,
    /// The background Git operation running ("Pulling…"): one runs at a
    /// time, so the panel's operations wait for it (set by the app).
    pub busy: Option<&'static str>,
    /// Push dialog / Manage Remotes / Clone jobs, the Git console and ask-pass prompts.
    pub git_jobs: GitJobs,
}

impl Default for GitPanel {
    fn default() -> Self {
        Self {
            commit_workspace: commit_workspace::Workspace::initial(),
            accessibility_owner: true,
            operations_workspace: operations_workspace::Workspace::default(),
            log_workspace: log_workspace::Workspace::default(),
            tab: GitTab::LocalChanges,
            graph: Vec::new(),
            all_branches: false,
            log_filter: None,
            log_limit: LOG_PAGE,
            log_query: String::new(),
            pending_tag: None,
            conflicts: Vec::new(),
            operation: None,
            prefilled: None,
            stashes: Vec::new(),
            stash_message: String::new(),
            stash_untracked: false,
            branch_from: None,
            blocked_checkout: None,
            unified_preview: false,
            local_diff: None,
            entries: Vec::new(),
            seen: HashSet::new(),
            checked: HashSet::new(),
            message: String::new(),
            amend: false,
            log: Vec::new(),
            selected_commit: None,
            commit_files: Vec::new(),
            selected_file: None,
            diff_text: String::new(),
            last_refresh: -100.0,
            focus_message: false,
            new_branch_name: String::new(),
            branch_popup: false,
            branch_workspace: branch_workspace::Workspace::default(),
            check_grammar: true,
            grammar_policy: Ok(Default::default()),
            jobs: Vec::new(),
            confirm: None,
            confirm_note: None,
            root: PathBuf::new(),
            busy: None,
            git_jobs: GitJobs::default(),
        }
    }
}

/// Commits per Log page ("Load more" adds another page).
const LOG_PAGE: usize = 300;

/// Log filter: `query` (case-insensitive) in the summary, the author's name or
/// email, or the hash.
pub fn commit_matches(c: &CommitInfo, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    q.is_empty()
        || c.summary.to_lowercase().contains(&q)
        || c.author.to_lowercase().contains(&q)
        || c.email.to_lowercase().contains(&q)
        || c.id.contains(&q)
}

/// A line of the Log graph: `([from, to], lane)`, the lane picking the colour.
pub type GraphSegment = ([egui::Pos2; 2], usize);

/// The commit graph inside one Log row, as [`GraphSegment`]s: the lower halves
/// of the edges arriving from the row above (`above`, that row's `lanes_in`)
/// and the upper halves of this row's own `edges`. Each edge is split where it
/// crosses the boundary between the rows (`top` and `bottom`: the middle of the
/// spacing between them), so the halves meet even with spacing between rows,
/// each is painted after its own row's hover background, and a branch tip gets
/// no line from above.
pub fn graph_row_segments(
    above: Option<&[(usize, usize)]>,
    edges: &[(usize, usize)],
    lane_x: impl Fn(usize) -> f32,
    y: f32,
    top: f32,
    bottom: f32,
) -> Vec<GraphSegment> {
    let mid = |a: usize, b: usize| (lane_x(a) + lane_x(b)) / 2.0;
    let mut out = Vec::new();
    for &(a, b) in above.unwrap_or_default() {
        out.push((
            [
                egui::Pos2::new(mid(a, b), top),
                egui::Pos2::new(lane_x(b), y),
            ],
            b,
        ));
    }
    for &(a, b) in edges {
        out.push((
            [
                egui::Pos2::new(lane_x(a), y),
                egui::Pos2::new(mid(a, b), bottom),
            ],
            b,
        ));
    }
    out
}

/// File status → text color ("File Status Colors").
pub fn status_color(status: FileStatus, theme: &UiTheme) -> Option<Color32> {
    let key = match status {
        FileStatus::Unmodified => "file.unmodified",
        FileStatus::Untracked => "file.untracked",
        FileStatus::Added => "file.added",
        FileStatus::Modified => "file.modified",
        FileStatus::Deleted => "file.deleted",
        FileStatus::Renamed => "file.renamed",
        FileStatus::Conflicted => "file.conflicted",
        FileStatus::Ignored => "file.ignored",
    };
    if let Some(color) = theme.scheme_override(key) {
        return Some(color);
    }

    if theme.high_contrast {
        return (status != FileStatus::Unmodified).then_some(theme.text);
    }
    Some(match status {
        FileStatus::Unmodified => return None,
        FileStatus::Untracked => {
            if theme.dark {
                Color32::from_rgb(0xD1, 0x67, 0x5A)
            } else {
                Color32::from_rgb(0xBF, 0x3F, 0x24)
            }
        }
        FileStatus::Added => {
            if theme.dark {
                Color32::from_rgb(0x73, 0xBE, 0x70)
            } else {
                Color32::from_rgb(0x0A, 0x7A, 0x0A)
            }
        }
        FileStatus::Modified => {
            if theme.dark {
                Color32::from_rgb(0x6C, 0x9E, 0xE2)
            } else {
                Color32::from_rgb(0x00, 0x32, 0xA0)
            }
        }
        FileStatus::Deleted => Color32::from_gray(0x80),
        FileStatus::Renamed => {
            if theme.dark {
                Color32::from_rgb(0x3A, 0xA7, 0xA3)
            } else {
                Color32::from_rgb(0x00, 0x7C, 0x7C)
            }
        }
        FileStatus::Conflicted => {
            if theme.dark {
                Color32::from_rgb(0xD5, 0x75, 0x6C)
            } else {
                Color32::from_rgb(0xC0, 0x1F, 0x1F)
            }
        }
        FileStatus::Ignored => Color32::from_gray(0x6E),
    })
}

impl GitPanel {
    /// Select the active editor's actual buffer in Local Changes without writing it.
    pub(crate) fn select_working_file(&mut self, repo: &GitRepo, path: &Path, current: &str) {
        self.tab = GitTab::LocalChanges;
        let head_path = self
            .entries
            .iter()
            .find(|entry| entry.path == path)
            .and_then(|entry| entry.old_path.as_deref())
            .unwrap_or(path);
        let old = repo.head_text(head_path).unwrap_or_default();
        self.diff_text = unified_diff(&old, current, &path.display().to_string());
        self.local_diff = Some(
            SideBySide::new(&old, current).with_edit_target(path.to_path_buf(), current.to_owned()),
        );
        self.selected_file = Some(path.to_path_buf());
    }
    pub(crate) fn acknowledge_diff_apply(&self, path: &Path, text: String) {
        if self.selected_file.as_deref() == Some(path) {
            if let Some(diff) = &self.local_diff {
                diff.acknowledge_apply(text);
            }
        }
    }
    pub(crate) fn selected_path(&self) -> Option<&Path> {
        self.selected_file.as_deref()
    }

    pub(crate) fn grammar_issues(
        &self,
    ) -> Result<Vec<rustpy_grammar::Issue>, rustpy_grammar::policy::PolicyError> {
        if !self.check_grammar {
            return Ok(Vec::new());
        }
        self.grammar_policy
            .as_ref()
            .map(|policy| {
                rustpy_grammar::policy::check(
                    &self.message,
                    rustpy_grammar::policy::Context::CommitMessage,
                    *policy,
                )
            })
            .map_err(Clone::clone)
    }

    /// Reloads status and log from the repository.
    pub fn refresh(&mut self, repo: &GitRepo, now: f64) {
        self.entries = repo.status().unwrap_or_default();
        self.entries.sort_by(|a, b| a.path.cmp(&b.path));
        // Newly appeared changes default to checked (the default
        // changelist); ones the user unchecked stay unchecked.
        let previous = std::mem::take(&mut self.seen);
        self.seen = self.entries.iter().map(|e| e.path.clone()).collect();
        self.checked.retain(|p| self.seen.contains(p));
        for e in &self.entries {
            // Unversioned files (also a deleted file plus a look-alike nobody
            // added, shown as a rename) start unchecked.
            if !previous.contains(&e.path) && !e.is_unversioned() && e.status != FileStatus::Ignored
            {
                self.checked.insert(e.path.clone());
            }
        }
        self.reload_log(repo);
        self.conflicts = repo.conflicts().unwrap_or_default();
        self.operation = repo.current_operation();
        self.prefill_operation_message(repo);
        self.stashes = repo.stash_list().unwrap_or_default();
        self.last_refresh = now;
    }

    /// An interrupted merge/cherry-pick/revert offers its prepared message
    /// (`MERGE_MSG`) in the empty message box, once per operation.
    fn prefill_operation_message(&mut self, repo: &GitRepo) {
        match &self.operation {
            Some(op) if self.prefilled.as_ref().map(|(o, _)| o) != Some(op) => {
                let text = repo
                    .merge_message()
                    .filter(|_| self.message.trim().is_empty())
                    .unwrap_or_default();
                if !text.is_empty() {
                    self.message = text.clone();
                }
                self.prefilled = Some((op.clone(), text));
            }
            Some(_) => {}
            None => {
                // Finished or aborted elsewhere: drop a message we put there.
                if let Some((_, text)) = self.prefilled.take() {
                    if !text.is_empty() && self.message == text {
                        self.message.clear();
                    }
                }
            }
        }
    }

    fn reload_log(&mut self, repo: &GitRepo) {
        if self.log_workspace.has_query() {
            self.log_workspace.reload(repo.root(), self.log_limit);
            return;
        }
        match &self.log_filter {
            Some(path) => {
                self.log = repo.file_history(path, self.log_limit).unwrap_or_default();
                self.graph.clear();
            }
            None => {
                // One walk feeds the rows and their lanes, so "All branches" lists
                // every branch's commits and edges join adjacent rows.
                self.graph = repo
                    .log_graph(self.log_limit, self.all_branches)
                    .unwrap_or_default();
                self.log = self
                    .graph
                    .iter()
                    .map(rustpy_vcs::GraphCommit::to_info)
                    .collect();
            }
        }
    }

    /// Whether Commit and Commit and Push… are enabled: no unresolved merge
    /// conflicts, a message (also when amending, so the last commit's message
    /// cannot be wiped) and at least one checked change, unless amending
    /// (rewording the last commit).
    pub fn can_commit(&self) -> bool {
        self.commit_blocker().is_none()
    }

    /// Why Commit is disabled (see [`GitPanel::can_commit`]).
    fn commit_blocker(&self) -> Option<&'static str> {
        if !self.conflicts.is_empty() {
            Some("Resolve the merge conflicts first")
        } else if self.message.trim().is_empty() {
            Some("Enter a commit message")
        } else if self.checked.is_empty()
            && !self.amend
            && !((self.commit_uses_index() || rustpy_vcs::policy::current().staging_area)
                && self.entries.iter().any(|entry| entry.staged))
        {
            Some("Check the changes to commit")
        } else {
            None
        }
    }

    /// Commits the checked changes with the message, amending the last commit
    /// when Amend is on (the app saves the editors first). Clears the message and
    /// Amend afterwards; on failure nothing changes and the reason is returned.
    pub fn commit_checked(&mut self, repo: &GitRepo, now: f64) -> Result<Committed, String> {
        self.workspace_before_commit()?;
        if let Some(why) = self.commit_blocker() {
            return Err(format!("Commit failed: {}", why.to_lowercase()));
        }
        let mut paths: Vec<PathBuf> = self.checked.iter().cloned().collect();
        let selected_count = paths.len();
        // The selected destination represents a whole rename, including removal
        // of its tracked source. Never infer selection from a look-alike source:
        // its unchecked untracked destination may contain private new content.
        for entry in &self.entries {
            if self.checked.contains(&entry.path) {
                if let Some(old) = &entry.old_path {
                    paths.push(old.clone());
                }
            }
        }
        paths.sort();
        paths.dedup();
        let amend = self.amend;
        // Amending a commit that is already on the upstream branch rewrites
        // published history: pushing it afterwards needs a force push.
        let rewrote_pushed = amend && matches!(repo.ahead_behind(), Some((0, _)));
        let mut env = rustpy_vcs::GitEnv {
            vars: self.commit_environment()?,
        };
        if self.commit_uses_index() {
            env.vars.push(("RUSTPY_COMMIT_INDEX".into(), "1".into()));
        }
        snapshot_working_tree(repo, &self.root, "Before commit", false);
        match repo.commit_with_policy(&paths, self.message.trim(), amend, &env) {
            Ok(id) => {
                let short = &id[..7.min(id.len())];
                let status = if amend {
                    format!(
                        "Amended the last commit ({} file(s)) as {short}",
                        selected_count
                    )
                } else {
                    format!("Committed {} file(s) as {short}", selected_count)
                };
                if self.commit_workspace.clear_message_after_commit {
                    self.message.clear();
                }
                self.amend = false;
                self.refresh(repo, now);
                Ok(Committed {
                    status,
                    rewrote_pushed,
                })
            }
            Err(e) => Err(format!("Commit failed: {e}")),
        }
    }

    /// Asks before force pushing an amended commit that replaced a pushed one.
    pub fn ask_force_push(&mut self, repo: &GitRepo) {
        self.confirm = Some(Confirm::ForcePush(
            repo.upstream_of_current()
                .unwrap_or_else(|| "the remote branch".to_string()),
        ));
    }

    /// Shows the history of one file in the Log tab.
    pub fn show_file_history(&mut self, repo: &GitRepo, path: &Path) {
        self.log_filter = Some(path.to_path_buf());
        self.tab = GitTab::Log;
        self.selected_commit = None;
        self.reload_log(repo);
    }

    /// Supplied by the App before painting, independently of popup focus.
    pub(crate) fn set_accessibility_owner(&mut self, available: bool) {
        self.accessibility_owner = available;
        self.branch_workspace.menu_available = available;
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        repo: Option<&GitRepo>,
        root: &Path,
        now: f64,
    ) -> Vec<GitEvent> {
        let mut events = Vec::new();
        if self.root != root {
            self.workspace_root(repo.map(GitRepo::root).unwrap_or(root));
            self.root = root.to_path_buf();
            self.confirm = None;
        }
        let Some(repo) = repo else {
            ui.label(
                egui::RichText::new("This project is not under Git version control.")
                    .color(theme.text_dim),
            );
            if ui.button("Create Git Repository").clicked() {
                match GitRepo::init(root) {
                    Ok(_) => events.push(GitEvent::Status("Git repository created".into())),
                    Err(e) => {
                        events.push(GitEvent::Status(format!("Cannot create repository: {e}")))
                    }
                }
            }
            return events;
        };
        if now - self.last_refresh > 2.0 {
            self.refresh(repo, now);
        }
        // One Git operation at a time: while a background push, fetch or
        // pull runs, the panel's operations (commit, stash, rollback, …) wait.
        if let Some(running) = self.busy {
            ui.label(
                egui::RichText::new(format!(
                    "{running} Other Git operations are unavailable until it finishes."
                ))
                .color(theme.warning),
            );
        }
        if ui.button("Operations…").clicked() {
            self.open_operations(
                repo.root().to_path_buf(),
                self.operations_workspace.file.clone(),
                self.operations_workspace.selection,
                self.operations_workspace.preferences.clone(),
            );
        }
        ui.horizontal(|ui| {
            if ui
                .selectable_label(
                    self.tab == GitTab::LocalChanges,
                    format!("Local Changes ({})", self.entries.len()),
                )
                .clicked()
            {
                self.tab = GitTab::LocalChanges;
            }
            if ui
                .selectable_label(self.tab == GitTab::Branches, "Branches")
                .clicked()
            {
                self.tab = GitTab::Branches;
            }
            if ui
                .selectable_label(self.tab == GitTab::Log, "Log")
                .clicked()
            {
                self.tab = GitTab::Log;
            }
            if ui
                .selectable_label(self.tab == GitTab::Shelf, "Shelf")
                .clicked()
            {
                self.tab = GitTab::Shelf;
            }
            if ui
                .selectable_label(self.tab == GitTab::Console, "Console")
                .on_hover_text("Git commands run by the IDE and their output")
                .clicked()
            {
                self.tab = GitTab::Console;
            }
            if ui.small_button("⟳").on_hover_text("Refresh").clicked() {
                self.refresh(repo, now);
            }
            let busy = self.busy.is_some() || self.git_jobs.is_busy();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(!busy, egui::Button::new("Fetch")).clicked() {
                    events.push(GitEvent::Fetch);
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("Pull"))
                    .on_hover_text("Update Project: fetch and merge or rebase the upstream branch")
                    .clicked()
                {
                    events.push(GitEvent::Update);
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("Push"))
                    .on_hover_text(
                        "Push the current branch; Git › Push… shows what will be pushed first",
                    )
                    .clicked()
                {
                    events.push(GitEvent::Push);
                }
                self.git_jobs.status_ui(ui, theme);
            });
        });
        ui.separator();
        if self.busy.is_some() {
            ui.disable();
        }
        match self.tab {
            GitTab::LocalChanges => {
                if rustpy_vcs::policy::current().non_modal_commit {
                    self.local_changes(ui, theme, repo, now, &mut events);
                } else if self.busy.is_some() || self.git_jobs.has_prompt() {
                    // A modal commit pane must not obscure the foreground
                    // credential dialog used by its running hooks/signing job.
                    ui.label("The Git operation is running; credentials appear in the authentication dialog.");
                } else {
                    egui::Modal::new(egui::Id::new("vcs_modal_commit")).show(ui.ctx(), |ui| {
                        ui.set_min_size(egui::vec2(800.0, 500.0));
                        ui.heading("Commit Changes");
                        if ui.button("Close").clicked() {
                            self.tab = GitTab::Log;
                        }
                        self.local_changes(ui, theme, repo, now, &mut events);
                    });
                }
            }
            GitTab::Log => self.log_workspace_ui(ui, theme, repo, &mut events),
            GitTab::Branches => events.extend(
                self.branch_workspace
                    .ui(ui, theme, repo, now)
                    .into_iter()
                    .map(GitEvent::BranchWorkspace),
            ),
            GitTab::Shelf => self.operation_shelf_ui(ui, repo, now, &mut events),
            GitTab::Console => self.git_jobs.console_ui(ui, theme),
        }
        self.confirm_window(ui.ctx(), theme, repo, now, &mut events);
        self.operations_ui(ui, theme, repo, &mut events);
        events
    }

    /// The pending confirmation, as a small window; runs the operation on OK.
    fn confirm_window(
        &mut self,
        ctx: &egui::Context,
        theme: &UiTheme,
        repo: &GitRepo,
        now: f64,
        events: &mut Vec<GitEvent>,
    ) {
        let Some(mut c) = self.confirm.take() else {
            return;
        };
        let mut decided: Option<bool> = None;
        egui::Window::new(c.title())
            .id(egui::Id::new("git_confirm"))
            .collapsible(false)
            .resizable(false)
            .default_width(460.0)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .order(egui::Order::Foreground)
            .show_modal(ctx, |ui| {
                let message = c.message(&self.root);
                ui.label(&message);
                if let Confirm::Rollback {
                    deletes: true,
                    delete_local,
                    ..
                } = &mut c
                {
                    ui.checkbox(delete_local, "Delete the local copy too");
                }
                if self
                    .confirm_note
                    .as_ref()
                    .is_none_or(|(m, _)| *m != message)
                {
                    self.confirm_note = Some((message.clone(), c.history_note()));
                }
                if let Some((_, Some(note))) = &self.confirm_note {
                    ui.label(egui::RichText::new(*note).color(theme.text_dim).small());
                }
                if let Some(running) = self.busy {
                    ui.label(
                        egui::RichText::new(format!("{running} Wait until it finishes."))
                            .color(theme.warning),
                    );
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            self.busy.is_none(),
                            egui::Button::new(egui::RichText::new(c.button()).color(theme.error)),
                        )
                        .clicked()
                    {
                        decided = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        decided = Some(false);
                    }
                });
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    decided = Some(false);
                }
            });
        match decided {
            Some(true) => self.run_confirmed(c, repo, now, events),
            Some(false) => {}
            None => self.confirm = Some(c),
        }
    }

    fn run_confirmed(&mut self, c: Confirm, repo: &GitRepo, now: f64, events: &mut Vec<GitEvent>) {
        let _operation = match rustpy_vcs::RepositoryOperation::enter(repo.root(), c.title()) {
            Ok(operation) => operation,
            Err(e) => {
                events.push(GitEvent::Status(e.to_string()));
                self.confirm = Some(c);
                return;
            }
        };
        let root = self.root.clone();
        let base = history_store::base_dir();
        let rel = |p: &Path| p.strip_prefix(&root).unwrap_or(p).display().to_string();
        let msg = match &c {
            Confirm::Rollback {
                path,
                deletes: true,
                delete_local: false,
                renamed_from,
            } => repo.rollback_keeping_new(path).map(|_| match renamed_from {
                Some(old) => format!(
                    "Restored {} ({} was kept as an unversioned file)",
                    rel(old),
                    rel(path)
                ),
                None => format!("Unstaged {} (the file was kept)", rel(path)),
            }),
            Confirm::Rollback { path, deletes, .. } => {
                history_store::snapshot_paths(
                    &base,
                    Some(&root),
                    std::slice::from_ref(path),
                    "Before rollback",
                    *deletes,
                );
                repo.rollback(path)
                    .map(|_| format!("Rolled back {}", rel(path)))
            }
            Confirm::DeleteUnversioned(path) => {
                history_store::snapshot_paths(
                    &base,
                    Some(&root),
                    std::slice::from_ref(path),
                    "Before delete",
                    true,
                );
                std::fs::remove_file(path)
                    .map(|_| format!("Deleted {}", rel(path)))
                    .map_err(anyhow::Error::from)
            }
            Confirm::ResetHard(id) => {
                snapshot_working_tree(repo, &root, "Before reset --hard", false);
                repo.reset(id, rustpy_vcs::ResetMode::Hard)
                    .map(|_| format!("Reset to {}", &id[..7.min(id.len())]))
            }
            Confirm::DropStash { index, .. } => {
                let r = repo.stash_drop(*index).map(|_| "Stash dropped".to_string());
                self.stashes = repo.stash_list().unwrap_or_default();
                r
            }
            Confirm::Abort(op) => {
                snapshot_working_tree(repo, &root, &format!("Before {op} abort"), false);
                repo.abort_operation()
                    .map(|_| format!("{} aborted", capitalize(op)))
            }
            Confirm::ForcePush(_) => {
                events.push(GitEvent::ForcePush);
                return;
            }
            Confirm::SkipCommit => {
                snapshot_working_tree(repo, &root, "Before rebase skip", false);
                repo.rebase_skip().map(|out| {
                    if !out.conflicts.is_empty() {
                        format!(
                            "Rebase stopped again with {} conflict(s)",
                            out.conflicts.len()
                        )
                    } else if out.stopped.is_some() {
                        "Commit skipped".to_string()
                    } else {
                        "Rebase finished".to_string()
                    }
                })
            }
        };
        events.push(GitEvent::Status(match msg {
            Ok(m) => m,
            Err(e) => format!("{} failed: {e}", c.title()),
        }));
        events.push(GitEvent::FilesChanged);
        self.refresh(repo, now);
    }

    fn local_changes(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        repo: &GitRepo,
        now: f64,
        events: &mut Vec<GitEvent>,
    ) {
        let root = repo.root().to_path_buf();
        ui.collapsing("Ignored files", |ui| {
            if self.commit_workspace.show_ignored {for path in repo.ignored_files().unwrap_or_default() {
                ui.label(path.strip_prefix(&root).unwrap_or(&path).display().to_string());
            }}
            ui.label("Git uses .gitignore, .git/info/exclude and core.excludesfile. .ignore affects project indexing only.");
            for path in repo.ignore_sources() { ui.label(path.display().to_string()); }
            if ui
                .button("Generate .gitignore…")
                .on_hover_text("Add patterns from bundled templates (Python, Jupyter, Django, IDE and OS files…)")
                .clicked()
            {
                events.push(GitEvent::GitignoreTemplates(root.clone()));
            }
            if (!repo.root().join(".gitignore").is_file() || !std::fs::read_to_string(repo.root().join(".gitignore")).unwrap_or_default().contains(".rustpy/workspace.json"))
                && ui.button("Ignore private .rustpy/workspace.json").clicked() {
                if let Err(e)=repo.add_to_gitignore("/.rustpy/workspace.json") { events.push(GitEvent::Status(e.to_string())); }
            }
        });
        if rustpy_vcs::policy::current().staging_area {
            ui.collapsing("Staging area", |ui| {
                for entry in self.entries.clone() {
                    ui.horizontal(|ui| {
                        ui.label(
                            entry
                                .path
                                .strip_prefix(&root)
                                .unwrap_or(&entry.path)
                                .display()
                                .to_string(),
                        );
                        if ui
                            .small_button(if entry.staged { "Unstage" } else { "Stage" })
                            .clicked()
                        {
                            let result = if entry.staged {
                                repo.unstage(&entry.path)
                            } else {
                                repo.stage(&entry.path)
                            };
                            if let Err(e) = result {
                                events.push(GitEvent::Status(e.to_string()));
                            }
                            self.last_refresh = -100.0;
                        }
                    });
                }
            });
        }
        self.workspace_tools(ui, repo, now, events);
        if self.commit_workspace.shelf_tab {
            return;
        }

        egui::Panel::right("git_commit_pane")
            .resizable(true)
            .default_size(380.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Commit Message").strong());
                    if ui
                        .small_button("Generate with AI")
                        .on_hover_text(
                            "Write the message from the checked changes with the AI Assistant",
                        )
                        .clicked()
                    {
                        events.push(GitEvent::GenerateCommitMessage);
                    }
                });
                let resp = ui.add(
                    egui::TextEdit::multiline(&mut self.message)
                        .desired_rows(5)
                        .desired_width(f32::INFINITY)
                        .hint_text("Commit message"),
                );
                if self.focus_message {
                    resp.request_focus();
                    self.focus_message = false;
                }
                self.workspace_message_tools(ui, repo, events);
                crate::task_trackers::commit_completion(ui, &mut self.message, &resp);
                // Grammar hints for the commit message, each with a one-click fix.
                if self.check_grammar && !self.message.trim().is_empty() {
                    let issues = match self.grammar_issues() {
                        Ok(issues) => issues,
                        Err(error) => {
                            ui.colored_label(theme.warning, error.to_string());
                            Vec::new()
                        }
                    };
                    let mut apply: Option<rustpy_grammar::Issue> = None;
                    for issue in issues.iter().take(5) {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!("Grammar: {}", issue.message))
                                    .color(theme.warning)
                                    .small(),
                            );
                            if let Some(r) = issue.replacements.first() {
                                if ui.small_button(format!("→ {r}")).clicked() {
                                    apply = Some(issue.clone());
                                }
                            }
                        });
                    }
                    if let Some(i) = apply {
                        self.message = rustpy_grammar::apply_first_fixes(&self.message, &[i]);
                    }
                }
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut self.amend, "Amend").changed()
                        && self.amend
                        && self.message.trim().is_empty()
                    {
                        self.message = repo.head_message().unwrap_or_default();
                    }
                    let n = self.checked.len();
                    let can = self.can_commit();
                    let why = self.commit_blocker().unwrap_or_default();
                    // The app saves the editors first, then commits (see `commit_checked`).
                    if ui
                        .add_enabled(can, egui::Button::new(format!("Commit ({n})")))
                        .on_disabled_hover_text(why)
                        .clicked()
                    {
                        events.push(GitEvent::Commit { push: false });
                    }
                    if ui
                        .add_enabled(can, egui::Button::new("Commit and Push…"))
                        .on_disabled_hover_text(why)
                        .clicked()
                    {
                        events.push(GitEvent::Commit { push: true });
                    }
                });
                ui.separator();
                // Diff preview of the selected file (HEAD on the left, working tree on the right).
                if let Some(sel) = self
                    .selected_file
                    .clone()
                    .filter(|_| self.commit_workspace.preview)
                {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(
                                sel.strip_prefix(&root)
                                    .unwrap_or(&sel)
                                    .display()
                                    .to_string(),
                            )
                            .color(theme.text_dim)
                            .small(),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.selectable_value(&mut self.unified_preview, true, "Unified");
                            ui.selectable_value(&mut self.unified_preview, false, "Side-by-side");
                        });
                    });
                    self.workspace_diff_hunks(ui, repo, &sel, now, events);
                    match &self.local_diff {
                        Some(d) if !self.unified_preview => d.ui(
                            ui,
                            theme,
                            egui::FontId::monospace(12.0),
                            "HEAD",
                            "Your version",
                        ),
                        _ => {
                            egui::ScrollArea::both()
                                .id_salt("git_local_diff")
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    diff_text_ui(ui, theme, &self.diff_text);
                                });
                        }
                    }
                    if let Some(diff) = &self.local_diff {
                        if let Some(edit) = diff.take_apply() {
                            events.push(GitEvent::ApplyDiff(edit));
                        }
                    }
                }
            });
        egui::ScrollArea::vertical()
            .id_salt("git_changes")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Merge / rebase in progress and conflicts.
                if let Some(op) = self.operation.clone() {
                    egui::Frame::new()
                        .fill(theme.hover_bg)
                        .inner_margin(egui::Margin::same(6))
                        .corner_radius(3)
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    egui::RichText::new(format!("{} in progress", capitalize(&op)))
                                        .strong()
                                        .color(theme.warning),
                                );
                                let continuable = matches!(
                                    op.as_str(),
                                    "merge" | "cherry-pick" | "revert" | "rebase"
                                );
                                if continuable {
                                    let hint = if op == "rebase" {
                                        "Continue rebasing with the resolved changes"
                                    } else {
                                        "Commit the resolved changes with the message on the right"
                                    };
                                    let resp = ui
                                        .add_enabled(
                                            self.conflicts.is_empty(),
                                            egui::Button::new("Continue"),
                                        )
                                        .on_hover_text(hint)
                                        .on_disabled_hover_text("Resolve the conflicts first");
                                    if resp.clicked() {
                                        match repo.continue_operation(self.message.trim()) {
                                            Ok(out) if !out.conflicts.is_empty() => {
                                                events.push(GitEvent::Status(format!(
                                                    "{} stopped again with {} conflict(s)",
                                                    capitalize(&op),
                                                    out.conflicts.len()
                                                )))
                                            }
                                            Ok(_) => {
                                                events.push(GitEvent::Status(format!(
                                                    "{} finished",
                                                    capitalize(&op)
                                                )));
                                                if self
                                                    .prefilled
                                                    .as_ref()
                                                    .is_some_and(|(_, t)| *t == self.message)
                                                    || op != "rebase"
                                                {
                                                    self.message.clear();
                                                }
                                            }
                                            Err(e) => events.push(GitEvent::Status(format!(
                                                "Cannot continue: {e}"
                                            ))),
                                        }
                                        events.push(GitEvent::FilesChanged);
                                        self.refresh(repo, now);
                                    }
                                }
                                if op == "rebase"
                                    && ui
                                        .button("Skip…")
                                        .on_hover_text("Drop the commit the rebase stopped on")
                                        .clicked()
                                {
                                    self.confirm = Some(Confirm::SkipCommit);
                                }
                                if ui
                                    .button(egui::RichText::new("Abort…").color(theme.error))
                                    .clicked()
                                {
                                    self.confirm = Some(Confirm::Abort(op.clone()));
                                }
                                if !self.conflicts.is_empty() && continuable {
                                    ui.label(
                                        egui::RichText::new(
                                            "Resolve the conflicts below, then Continue",
                                        )
                                        .color(theme.text_dim)
                                        .small(),
                                    );
                                }
                            });
                        });
                }
                if !self.conflicts.is_empty() {
                    ui.label(
                        egui::RichText::new(format!("Merge Conflicts ({})", self.conflicts.len()))
                            .strong()
                            .color(theme.error),
                    );
                    let conflicts = self.conflicts.clone();
                    for p in conflicts {
                        let rel = p.strip_prefix(&root).unwrap_or(&p).display().to_string();
                        ui.horizontal(|ui| {
                            ui.add_space(16.0);
                            ui.label(egui::RichText::new(&rel).color(theme.error));
                            if ui.small_button("Resolve…").clicked() {
                                events.push(GitEvent::ResolveConflict(p.clone()));
                            }
                            if ui.small_button("Accept Yours").clicked() {
                                history_store::snapshot_paths(
                                    &history_store::base_dir(),
                                    Some(&self.root),
                                    std::slice::from_ref(&p),
                                    "Before resolve",
                                    false,
                                );
                                match repo
                                    .conflict_sides(&p)
                                    .and_then(|(_, ours, _)| repo.resolve_conflict(&p, &ours))
                                {
                                    Ok(()) => events.push(GitEvent::FilesChanged),
                                    Err(e) => events.push(GitEvent::Status(e.to_string())),
                                }
                                self.refresh(repo, now);
                            }
                            if ui.small_button("Accept Theirs").clicked() {
                                history_store::snapshot_paths(
                                    &history_store::base_dir(),
                                    Some(&self.root),
                                    std::slice::from_ref(&p),
                                    "Before resolve",
                                    false,
                                );
                                match repo
                                    .conflict_sides(&p)
                                    .and_then(|(_, _, theirs)| repo.resolve_conflict(&p, &theirs))
                                {
                                    Ok(()) => events.push(GitEvent::FilesChanged),
                                    Err(e) => events.push(GitEvent::Status(e.to_string())),
                                }
                                self.refresh(repo, now);
                            }
                        });
                    }
                    ui.separator();
                }
                let mut toggle: Option<PathBuf> = None;
                let mut grouped = std::collections::BTreeMap::<String, Vec<StatusEntry>>::new();
                let visible_entries = if self.commit_uses_index() {
                    repo.workspace_layers()
                        .unwrap_or_else(|_| self.entries.clone())
                } else {
                    self.entries.clone()
                };
                for entry in &visible_entries {
                    if entry.status == FileStatus::Ignored && !self.commit_workspace.show_ignored {
                        continue;
                    }
                    if entry.is_unversioned() && !self.commit_workspace.show_unversioned {
                        continue;
                    }
                    let title = if entry.status == FileStatus::Ignored {
                        "Ignored Files".into()
                    } else if entry.is_unversioned() {
                        "Unversioned Files".into()
                    } else if self.commit_uses_index() {
                        if entry.staged {
                            "Staged".into()
                        } else {
                            "Unstaged".into()
                        }
                    } else {
                        self.commit_workspace
                            .lists
                            .iter()
                            .find(|list| list.paths.contains(&entry.path))
                            .map(|list| list.name.clone())
                            .unwrap_or_else(|| "Default".into())
                    };
                    let title = if self.commit_workspace.tree {
                        let relative = entry.path.strip_prefix(&root).unwrap_or(&entry.path);
                        let group = match self.commit_workspace.grouping {
                            0 => relative
                                .parent()
                                .unwrap_or(Path::new(""))
                                .display()
                                .to_string(),
                            1 => relative
                                .components()
                                .next()
                                .map(|component| {
                                    component.as_os_str().to_string_lossy().into_owned()
                                })
                                .unwrap_or_default(),
                            _ => root.display().to_string(),
                        };
                        format!("{title} / {group}")
                    } else {
                        title
                    };
                    grouped.entry(title).or_default().push(entry.clone());
                }
                let groups: Vec<_> = grouped.into_iter().collect();
                let mut rollback: Option<PathBuf> = None;
                let mut delete: Option<PathBuf> = None;
                let mut select: Option<(PathBuf, bool)> = None;
                let mut open: Option<PathBuf> = None;
                for (title, idxs) in groups {
                    if idxs.is_empty() {
                        continue;
                    }
                    ui.horizontal(|ui| {
                        let all = idxs.iter().all(|entry| {
                            if self.commit_uses_index() {
                                entry.staged
                            } else {
                                self.checked.contains(&entry.path)
                            }
                        });
                        let mut v = all;
                        if ui.checkbox(&mut v, "").changed() && self.accessibility_owner {
                            for i in &idxs {
                                let p = i.path.clone();
                                if self.commit_uses_index() {
                                    let result = if v {
                                        repo.workspace_stage(&p)
                                    } else {
                                        repo.unstage(&p)
                                    };
                                    if let Err(error) = result {
                                        events.push(GitEvent::Status(error.to_string()));
                                    }
                                    self.last_refresh = -100.0;
                                    continue;
                                }
                                if v {
                                    self.checked.insert(p);
                                } else {
                                    self.checked.remove(&p);
                                }
                            }
                        }
                        ui.label(egui::RichText::new(format!("{title} ({})", idxs.len())).strong());
                    });
                    if !self.commit_workspace.expanded {
                        continue;
                    }
                    for e in idxs {
                        let rel = change_label(&e, &root);
                        let color = status_color(e.status, theme).unwrap_or(theme.text);
                        ui.push_id(("git-change", &root, &e.path, e.staged), |ui| {
                            ui.horizontal(|ui| {
                                ui.add_space(16.0);
                                let mut c = if self.commit_uses_index() {
                                    e.staged
                                } else {
                                    self.checked.contains(&e.path)
                                };
                                if ui.checkbox(&mut c, "").changed() && self.accessibility_owner {
                                    if self.commit_uses_index() {
                                        let result = if c {
                                            repo.workspace_stage(&e.path)
                                        } else {
                                            repo.unstage(&e.path)
                                        };
                                        if let Err(error) = result {
                                            events.push(GitEvent::Status(error.to_string()));
                                        }
                                        self.last_refresh = -100.0;
                                    } else {
                                        toggle = Some(e.path.clone());
                                    }
                                }
                                let label = ui.add(
                                    egui::Label::new(egui::RichText::new(&rel).color(color))
                                        .sense(Sense::click()),
                                );
                                if label.clicked() && self.accessibility_owner {
                                    select = Some((e.path.clone(), e.staged));
                                }
                                if label.double_clicked() && self.accessibility_owner {
                                    open = Some(e.path.clone());
                                }
                                let authority = egui::Id::new((
                                    &root,
                                    &e.path,
                                    e.staged,
                                    &e.old_path,
                                    e.status.label(),
                                ));
                                let available = self.accessibility_owner && self.confirm.is_none();
                                crate::a11y::bound_keyboard_context_menu(
                                    &label, available, authority,
                                )
                                .show(|ui| {
                                    if !available {
                                        ui.disable();
                                    }
                                    if ui.button("Show Diff").clicked() {
                                        select = Some((e.path.clone(), e.staged));
                                        ui.close();
                                    }
                                    if ui.button("Open in Editor").clicked() {
                                        open = Some(e.path.clone());
                                        ui.close();
                                    }
                                    self.workspace_change_menu(ui, repo, &e, events);
                                    if e.status == FileStatus::Untracked {
                                        if ui.button("Add to .gitignore").clicked() {
                                            let rel = e
                                                .path
                                                .strip_prefix(&root)
                                                .unwrap_or(&e.path)
                                                .to_string_lossy()
                                                .replace('\\', "/");
                                            if let Err(error) =
                                                repo.add_to_gitignore(&format!("/{rel}"))
                                            {
                                                events.push(GitEvent::Status(error.to_string()));
                                            }
                                            ui.close();
                                        }
                                        if ui.button("Delete…").clicked() {
                                            delete = Some(e.path.clone());
                                            ui.close();
                                        }
                                    } else if ui.button("Rollback…").clicked() {
                                        rollback = Some(e.path.clone());
                                        ui.close();
                                    }
                                });
                                // A rename nobody added yet is unversioned, which is why it starts unchecked.
                                let what = if e.status == FileStatus::Renamed && e.is_unversioned()
                                {
                                    "Renamed, unversioned"
                                } else {
                                    e.status.label()
                                };
                                ui.label(egui::RichText::new(what).color(theme.text_dim).small());
                            });
                        });
                    }
                }
                if let Some(p) = toggle {
                    if !self.checked.remove(&p) {
                        self.checked.insert(p);
                    }
                }
                if let Some((p, staged)) = select {
                    let current = if self.commit_uses_index() && staged {
                        repo.workspace_index_text(&p).unwrap_or_default()
                    } else {
                        std::fs::read_to_string(&p).unwrap_or_default()
                    };
                    // A renamed file is compared with HEAD's copy under its old name.
                    let head_path = self
                        .entries
                        .iter()
                        .find(|e| e.path == p)
                        .and_then(|e| e.old_path.clone())
                        .unwrap_or_else(|| p.clone());
                    let old = if self.commit_uses_index() && !staged {
                        repo.workspace_index_text(&p).unwrap_or_default()
                    } else {
                        repo.head_text(&head_path).unwrap_or_default()
                    };
                    self.diff_text = unified_diff(
                        &old,
                        &current,
                        &p.strip_prefix(&root).unwrap_or(&p).display().to_string(),
                    );
                    self.local_diff = Some(if self.commit_uses_index() {
                        SideBySide::new(&old, &current)
                    } else {
                        SideBySide::new(&old, &current).with_edit_target(p.clone(), current.clone())
                    });
                    self.selected_file = Some(p);
                }
                if let Some(p) = open {
                    events.push(GitEvent::OpenFile(p));
                }
                if let Some(p) = rollback {
                    let renamed_from = self
                        .entries
                        .iter()
                        .find(|e| e.path == p)
                        .and_then(|e| e.old_path.clone());
                    self.confirm = Some(Confirm::Rollback {
                        deletes: repo.head_text(&p).is_none(),
                        delete_local: false,
                        path: p,
                        renamed_from,
                    });
                }
                if let Some(p) = delete {
                    self.confirm = Some(Confirm::DeleteUnversioned(p));
                }
                if self.entries.is_empty() {
                    ui.label(egui::RichText::new("No changes").color(theme.text_dim));
                }
            });
    }

    fn log_ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        repo: &GitRepo,
        events: &mut Vec<GitEvent>,
    ) {
        self.branch_comparison_ui(ui, events, repo.root());
        let root = repo.root().to_path_buf();
        ui.horizontal(|ui| {
            if let Some(f) = self.log_filter.clone() {
                ui.label(
                    egui::RichText::new(format!(
                        "History: {}",
                        f.strip_prefix(&root).unwrap_or(&f).display()
                    ))
                    .strong(),
                );
                if ui.small_button("Show all commits").clicked() {
                    self.log_filter = None;
                    self.reload_log(repo);
                }
            } else if ui
                .checkbox(&mut self.all_branches, "All branches")
                .changed()
            {
                self.log_workspace.set_all(self.all_branches);
                self.reload_log(repo);
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.log_query)
                    .hint_text("Filter: text, author or hash")
                    .desired_width(180.0),
            );
            if !self.log_query.is_empty()
                && ui
                    .small_button("×")
                    .on_hover_text("Clear the filter")
                    .clicked()
            {
                self.log_query.clear();
            }
            ui.separator();
            ui.label(
                egui::RichText::new(format!("Stashes ({})", self.stashes.len()))
                    .color(theme.text_dim),
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.stash_message)
                    .hint_text("Stash message")
                    .desired_width(160.0),
            );
            ui.checkbox(&mut self.stash_untracked, "untracked");
            if ui.small_button("Stash changes").clicked() {
                let msg = self.stash_message.trim().to_string();
                snapshot_working_tree(repo, &root, "Before stash", self.stash_untracked);
                events.push(GitEvent::Status(
                    match repo.stash_push(
                        Some(msg.as_str()).filter(|m| !m.is_empty()),
                        self.stash_untracked,
                    ) {
                        Ok(()) => "Changes stashed".into(),
                        Err(e) => e.to_string(),
                    },
                ));
                self.stash_message.clear();
                events.push(GitEvent::FilesChanged);
                self.stashes = repo.stash_list().unwrap_or_default();
            }
            let stashes = self.stashes.clone();
            for st in stashes.iter().take(5) {
                ui.menu_button(
                    format!("stash@{{{}}}: {}", st.index, truncate(&st.message, 28)),
                    |ui| {
                        if ui.button("Pop").clicked() {
                            snapshot_working_tree(repo, &root, "Before unstash", false);
                            events.push(GitEvent::Status(match repo.stash_pop(st.index) {
                                Ok(()) => "Stash popped".into(),
                                Err(e) => e.to_string(),
                            }));
                            events.push(GitEvent::FilesChanged);
                            self.stashes = repo.stash_list().unwrap_or_default();
                            ui.close();
                        }
                        if ui.button("Apply").clicked() {
                            snapshot_working_tree(repo, &root, "Before unstash", false);
                            events.push(GitEvent::Status(match repo.stash_apply(st.index) {
                                Ok(()) => "Stash applied".into(),
                                Err(e) => e.to_string(),
                            }));
                            events.push(GitEvent::FilesChanged);
                            ui.close();
                        }
                        if ui.button("Drop…").clicked() {
                            self.confirm = Some(Confirm::DropStash {
                                index: st.index,
                                message: truncate(&st.message, 60),
                            });
                            ui.close();
                        }
                    },
                );
            }
        });
        ui.separator();
        egui::Panel::right("git_log_details")
            .resizable(true)
            .default_size(420.0)
            .show(ui, |ui| {
                let Some(id) = self.selected_commit.clone() else {
                    ui.label(
                        egui::RichText::new("Select a commit to see details").color(theme.text_dim),
                    );
                    return;
                };
                let Some(c) = self.log.iter().find(|c| c.id == id).cloned() else {
                    return;
                };
                if let Some(files) = self.log_workspace.details_header(ui, repo, &c) {
                    self.commit_files = files;
                    self.selected_file = None;
                    self.diff_text.clear();
                }
                self.github_issue_links(ui, &c.message, repo);
                if ui.small_button("Open commit on GitHub / GitLab").clicked() {
                    events.push(GitEvent::GitHubCommit {
                        root: root.clone(),
                        commit: c.id.clone(),
                    });
                }
                ui.label(
                    egui::RichText::new(format!(
                        "{} <{}> · {} · {}",
                        c.author,
                        c.email,
                        rustpy_i18n::regional::timestamp_seconds(c.time),
                        c.short_id
                    ))
                    .color(theme.text_dim)
                    .small(),
                );
                ui.separator();
                let pick = self.log_workspace.changed_files_ui(
                    ui,
                    theme,
                    repo,
                    &id,
                    &self.commit_files,
                    events,
                );
                if let Some(p) = pick {
                    self.diff_text = self.log_workspace.file_diff(repo, &id, &p);
                    self.local_diff = None;
                    self.selected_file = Some(p);
                }
                if self.selected_file.is_some() && !self.diff_text.is_empty() {
                    ui.separator();
                    egui::ScrollArea::both()
                        .id_salt("git_commit_diff")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            diff_text_ui(ui, theme, &self.diff_text);
                        });
                }
            });
        if self.pending_tag.is_some() {
            self.tag_prompt(ui, repo, events);
        }
        egui::ScrollArea::vertical()
            .id_salt("git_log")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut pick: Option<String> = None;
                let mut op: Option<(String, LogOp)> = None;
                let lane_w = 14.0;
                // Index results carry their own lanes; hide stale lanes during a new query.
                let filtering = self.log_workspace.collapsed
                    || !self.log_workspace.graph_ready(&self.log_query);
                let graph_w = if self.graph.is_empty() || filtering {
                    0.0
                } else {
                    (self.graph.iter().map(|g| g.lane_count).max().unwrap_or(1) as f32 * lane_w
                        + 8.0)
                        .min(140.0)
                };
                let lane_colors = [
                    Color32::from_rgb(0x4A, 0x88, 0xC7),
                    Color32::from_rgb(0x59, 0xA8, 0x69),
                    Color32::from_rgb(0xC7, 0x7C, 0x3A),
                    Color32::from_rgb(0xB0, 0x6A, 0xC9),
                    Color32::from_rgb(0xC9, 0x5A, 0x5A),
                    Color32::from_rgb(0x3A, 0xB0, 0xB0),
                ];
                let graph_by_id: std::collections::HashMap<&str, &rustpy_vcs::GraphCommit> =
                    if filtering {
                        Default::default()
                    } else {
                        self.graph.iter().map(|g| (g.id.as_str(), g)).collect()
                    };
                // Rows are `row_gap` apart; graph edges cross that gap between their rows.
                let row_gap = ui.spacing().item_spacing.y;
                let mut above: Option<&[(usize, usize)]> = None;
                let mut shown = 0usize;
                let visible_commits = self
                    .log
                    .iter()
                    .filter(|c| self.log_workspace.matches(c, &self.log_query))
                    .filter(|c| {
                        !self.log_workspace.collapsed
                            || !self.log_query.is_empty()
                            || !c.refs.is_empty()
                            || c.parents.len() != 1
                            || self.selected_commit.as_deref() == Some(c.id.as_str())
                            || self
                                .log
                                .iter()
                                .filter(|child| child.parents.contains(&c.id))
                                .count()
                                != 1
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                for c in &visible_commits {
                    shown += 1;
                    let selected = self.selected_commit.as_deref() == Some(c.id.as_str());
                    let (rect, _) = ui
                        .allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::hover());
                    let resp = ui.interact(
                        rect,
                        ui.id().with(("git-commit", &root, &c.id)),
                        Sense::click(),
                    );
                    if self.log_workspace.take_scroll(&c.id) {
                        resp.scroll_to_me(Some(egui::Align::Center));
                    }
                    resp.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::SelectableLabel,
                            true,
                            selected,
                            format!(
                                "Revision {}. {}. {}",
                                c.short_id,
                                truncate(&c.summary, 160),
                                truncate(&c.author, 80)
                            ),
                        )
                    });
                    resp.ctx.accesskit_node_builder(resp.id, |node| {
                        node.set_label(format!(
                            "Revision {}. {}. {}",
                            c.short_id,
                            truncate(&c.summary, 160),
                            truncate(&c.author, 80)
                        ));
                        node.set_selected(selected);
                    });
                    if selected {
                        ui.painter()
                            .rect_filled(rect, 2.0, theme.selection_inactive_bg);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(rect, 2.0, theme.hover_bg);
                    }
                    let mut x = rect.left() + 6.0;
                    let y = rect.center().y;
                    let graph_row = graph_by_id.get(c.id.as_str()).copied();
                    if let Some(g) = graph_row {
                        let lane_x = |lane: usize| rect.left() + 8.0 + lane as f32 * lane_w;
                        let segments = if self.log_workspace.long_edges {
                            graph_row_segments(
                                above,
                                &g.lanes_in,
                                lane_x,
                                y,
                                rect.top() - row_gap / 2.0,
                                rect.bottom() + row_gap / 2.0,
                            )
                        } else {
                            let mut segments = Vec::new();
                            let position = self
                                .graph
                                .iter()
                                .position(|row| row.id == g.id)
                                .unwrap_or(0);
                            if let Some(previous) =
                                position.checked_sub(1).and_then(|n| self.graph.get(n))
                            {
                                if previous.parents.contains(&g.id) {
                                    segments.push((
                                        [
                                            egui::Pos2::new(
                                                (lane_x(previous.lane) + lane_x(g.lane)) / 2.0,
                                                rect.top() - row_gap / 2.0,
                                            ),
                                            egui::Pos2::new(lane_x(g.lane), y),
                                        ],
                                        g.lane,
                                    ));
                                }
                            }
                            if let Some(next) = self.graph.get(position + 1) {
                                if g.parents.contains(&next.id) {
                                    segments.push((
                                        [
                                            egui::Pos2::new(lane_x(g.lane), y),
                                            egui::Pos2::new(
                                                (lane_x(g.lane) + lane_x(next.lane)) / 2.0,
                                                rect.bottom() + row_gap / 2.0,
                                            ),
                                        ],
                                        next.lane,
                                    ));
                                }
                            }
                            segments
                        };
                        for (segment, lane) in segments {
                            ui.painter().line_segment(
                                segment,
                                egui::Stroke::new(
                                    1.5,
                                    if theme.high_contrast {
                                        theme.text
                                    } else {
                                        lane_colors[lane % lane_colors.len()]
                                    },
                                ),
                            );
                        }
                        ui.painter().circle_filled(
                            egui::Pos2::new(lane_x(g.lane), y),
                            4.0,
                            if theme.high_contrast {
                                theme.text
                            } else {
                                lane_colors[g.lane % lane_colors.len()]
                            },
                        );
                        x += graph_w;
                    }
                    above = graph_row.map(|g| g.lanes_in.as_slice());
                    let available = self.accessibility_owner
                        && self.confirm.is_none()
                        && self.pending_tag.is_none();
                    let authority = egui::Id::new((&root, &c.id));
                    crate::a11y::bound_keyboard_context_menu(&resp, available, authority).show(
                        |ui| {
                            if !available {
                                ui.disable();
                            }
                            if ui.button("Checkout Revision").clicked() {
                                op = Some((c.id.clone(), LogOp::Checkout));
                                ui.close();
                            }
                            if ui.button("Cherry-pick").clicked() {
                                op = Some((c.id.clone(), LogOp::CherryPick));
                                ui.close();
                            }
                            if ui.button("Revert Commit").clicked() {
                                op = Some((c.id.clone(), LogOp::Revert));
                                ui.close();
                            }
                            ui.menu_button("Reset Current Branch to Here", |ui| {
                                for (label, mode) in [
                                    ("Soft (keep changes staged)", rustpy_vcs::ResetMode::Soft),
                                    ("Mixed (keep changes)", rustpy_vcs::ResetMode::Mixed),
                                    ("Hard (discard changes)…", rustpy_vcs::ResetMode::Hard),
                                ] {
                                    if ui.button(crate::i18n::tr_static(label)).clicked() {
                                        op = Some((c.id.clone(), LogOp::Reset(mode)));
                                        ui.close();
                                    }
                                }
                            });
                            if ui.button("Create Tag…").clicked() {
                                op = Some((c.id.clone(), LogOp::Tag));
                                ui.close();
                            }
                            self.log_workspace.extended_context(ui, repo, c);
                            if ui.button("Copy Revision Number").clicked() {
                                ui.ctx().copy_text(c.id.clone());
                                ui.close();
                            }
                        },
                    );
                    for r in &self.log_workspace.refs(c) {
                        let bg = self.log_workspace.reference_color(c, r, theme);
                        let galley = ui.fonts_mut(|f| {
                            f.layout_no_wrap(
                                r.clone(),
                                egui::FontId::proportional(11.0),
                                if theme.high_contrast {
                                    theme.text
                                } else {
                                    Color32::WHITE
                                },
                            )
                        });
                        let w = galley.size().x + 8.0;
                        ui.painter().rect_filled(
                            egui::Rect::from_min_size(
                                egui::Pos2::new(x, y - 8.0),
                                Vec2::new(w, 16.0),
                            ),
                            3.0,
                            bg,
                        );
                        ui.painter().galley(
                            egui::Pos2::new(x + 4.0, y - galley.size().y / 2.0),
                            galley,
                            if theme.high_contrast {
                                theme.text
                            } else {
                                Color32::WHITE
                            },
                        );
                        x += w + 4.0;
                    }
                    let summary_w = rect.width() - (x - rect.left()) - 380.0;
                    let mut summary = if self.log_workspace.columns[0] {
                        c.summary.clone()
                    } else {
                        String::new()
                    };
                    if self.log_workspace.root_names {
                        summary = format!(
                            "[{}] {summary}",
                            root.file_name().unwrap_or_default().to_string_lossy()
                        );
                    }
                    let max_chars = (summary_w / 7.0).max(10.0) as usize;
                    if summary.chars().count() > max_chars {
                        summary = summary
                            .chars()
                            .take(max_chars.saturating_sub(1))
                            .collect::<String>()
                            + "…";
                    }
                    ui.painter().text(
                        egui::Pos2::new(x, y),
                        egui::Align2::LEFT_CENTER,
                        summary,
                        egui::FontId::proportional(13.0),
                        self.log_workspace.color(c, theme),
                    );
                    ui.painter().text(
                        egui::Pos2::new(rect.right() - 290.0, y),
                        egui::Align2::LEFT_CENTER,
                        if self.log_workspace.columns[1] {
                            c.author.as_str()
                        } else {
                            ""
                        },
                        egui::FontId::proportional(12.0),
                        theme.text_dim,
                    );
                    ui.painter().text(
                        egui::Pos2::new(rect.right() - 175.0, y),
                        egui::Align2::LEFT_CENTER,
                        if self.log_workspace.columns[2] {
                            self.log_workspace.date(c)
                        } else {
                            String::new()
                        },
                        egui::FontId::proportional(12.0),
                        theme.text_dim,
                    );
                    ui.painter().text(
                        egui::Pos2::new(rect.right() - 6.0, y),
                        egui::Align2::RIGHT_CENTER,
                        if self.log_workspace.columns[3] {
                            c.short_id.as_str()
                        } else {
                            ""
                        },
                        egui::FontId::monospace(12.0),
                        theme.text_dim,
                    );
                    if resp.clicked() && self.accessibility_owner {
                        pick = Some(c.id.clone());
                    }
                }
                if let Some(id) = pick {
                    self.commit_files = repo.commit_files(&id).unwrap_or_default();
                    self.selected_commit = Some(id);
                    self.selected_file = None;
                    self.diff_text.clear();
                }
                if let Some((id, LogOp::Reset(rustpy_vcs::ResetMode::Hard))) = &op {
                    self.confirm = Some(Confirm::ResetHard(id.clone()));
                    op = None;
                }
                if let Some((id, op)) = op {
                    let short = id[..7.min(id.len())].to_string();
                    let label = match op {
                        LogOp::Checkout => "Before checkout",
                        LogOp::CherryPick => "Before cherry-pick",
                        LogOp::Revert => "Before revert commit",
                        LogOp::Reset(_) | LogOp::Tag => "",
                    };
                    if !label.is_empty() {
                        snapshot_working_tree(repo, &root, label, false);
                    }
                    let result = match op {
                        LogOp::Tag => {
                            self.pending_tag = Some(PendingTag {
                                target: id.clone(),
                                name: String::new(),
                                message: String::new(),
                                focus: true,
                            });
                            None
                        }
                        LogOp::Checkout => Some(
                            repo.checkout_commit(&id)
                                .map(|_| format!("Checked out {short} (detached HEAD)")),
                        ),
                        LogOp::CherryPick => Some(repo.cherry_pick(&id).map(|c| {
                            if c.is_empty() {
                                format!("Cherry-picked {short}")
                            } else {
                                format!("Cherry-pick of {short} has {} conflict(s)", c.len())
                            }
                        })),
                        LogOp::Revert => Some(repo.revert_commit(&id).map(|c| {
                            if c.is_empty() {
                                format!("Reverted {short}")
                            } else {
                                format!("Revert of {short} has {} conflict(s)", c.len())
                            }
                        })),
                        LogOp::Reset(mode) => {
                            Some(repo.reset(&id, mode).map(|_| format!("Reset to {short}")))
                        }
                    };
                    if let Some(result) = result {
                        events.push(GitEvent::Status(match result {
                            Ok(m) => m,
                            Err(e) => e.to_string(),
                        }));
                        events.push(GitEvent::FilesChanged);
                        self.refresh(repo, 0.0);
                    }
                }
                if self.log.is_empty() {
                    ui.label(egui::RichText::new("No commits yet").color(theme.text_dim));
                } else if shown == 0 {
                    ui.label(
                        egui::RichText::new("No commits match the filter").color(theme.text_dim),
                    );
                }
                if self.log.len() >= self.log_limit {
                    ui.add_space(4.0);
                    if ui
                        .button(format!("Load more (showing the latest {})", self.log.len()))
                        .clicked()
                    {
                        self.log_limit += LOG_PAGE;
                        self.reload_log(repo);
                    }
                }
            });
    }

    /// Inline "Create Tag" prompt for the commit chosen in the Log.
    fn tag_prompt(&mut self, ui: &mut egui::Ui, repo: &GitRepo, events: &mut Vec<GitEvent>) {
        let Some(t) = self.pending_tag.as_mut() else {
            return;
        };
        let short = t.target[..7.min(t.target.len())].to_string();
        let mut create = false;
        let mut cancel = false;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("New tag at {short}:")).strong());
            let name = ui.add(
                egui::TextEdit::singleline(&mut t.name)
                    .hint_text("Tag name")
                    .desired_width(140.0),
            );
            if t.focus {
                name.request_focus();
                t.focus = false;
            }
            let message = ui.add(
                egui::TextEdit::singleline(&mut t.message)
                    .hint_text("Message (makes an annotated tag)")
                    .desired_width(220.0),
            );
            let enter = (name.lost_focus() || message.lost_focus())
                && ui.input(|i| i.key_pressed(egui::Key::Enter));
            create = ui
                .add_enabled(!t.name.trim().is_empty(), egui::Button::new("Create Tag"))
                .clicked()
                || (enter && !t.name.trim().is_empty());
            cancel =
                ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
        });
        ui.separator();
        if create {
            let name = t.name.trim().to_string();
            match repo.create_tag_at(&name, &t.target, Some(t.message.as_str())) {
                Ok(()) => {
                    events.push(GitEvent::Status(format!("Created tag '{name}' at {short}")));
                    self.pending_tag = None;
                    self.reload_log(repo);
                }
                Err(e) => events.push(GitEvent::Status(format!("Cannot create tag: {e}"))),
            }
        } else if cancel {
            self.pending_tag = None;
        }
    }
}

#[derive(Clone, Copy)]
enum LogOp {
    Checkout,
    CherryPick,
    Revert,
    Reset(rustpy_vcs::ResetMode),
    Tag,
}

/// Row label of a local change: its path relative to `root`; a rename also
/// names the old file (`c.py (renamed from b.py)`, `(moved from …)`).
pub fn change_label(e: &StatusEntry, root: &Path) -> String {
    let rel = |p: &Path| p.strip_prefix(root).unwrap_or(p).display().to_string();
    match &e.old_path {
        Some(old) if old.parent() == e.path.parent() => format!(
            "{} (renamed from {})",
            rel(&e.path),
            old.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        ),
        Some(old) => format!("{} (moved from {})", rel(&e.path), rel(old)),
        None => rel(&e.path),
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() > n {
        s.chars().take(n.saturating_sub(1)).collect::<String>() + "…"
    } else {
        s.to_string()
    }
}

/// Renders unified diff text with +/- coloring.
pub fn diff_text_ui(ui: &mut egui::Ui, theme: &UiTheme, text: &str) {
    let mono = egui::FontId::monospace(12.0);
    for line in text.lines() {
        let (color, bg) = if line.starts_with('+') && !line.starts_with("+++") {
            (
                theme.success,
                Some(if theme.dark {
                    Color32::from_rgb(0x29, 0x44, 0x36)
                } else {
                    Color32::from_rgb(0xDD, 0xFF, 0xDD)
                }),
            )
        } else if line.starts_with('-') && !line.starts_with("---") {
            (
                theme.error,
                Some(if theme.dark {
                    Color32::from_rgb(0x4B, 0x2B, 0x2B)
                } else {
                    Color32::from_rgb(0xFF, 0xDD, 0xDD)
                }),
            )
        } else if line.starts_with("@@") {
            (theme.accent, None)
        } else {
            (theme.text_dim, None)
        };
        let mut rt = egui::RichText::new(line).font(mono.clone()).color(color);
        if let Some(bg) = bg {
            rt = rt.background_color(bg);
        }
        ui.label(rt);
    }
}

/// Builds a unified diff for display.
pub fn unified_diff(old: &str, new: &str, name: &str) -> String {
    use similar::TextDiff;
    let diff = TextDiff::from_lines(old, new);
    diff.unified_diff()
        .context_radius(3)
        .header(&format!("a/{name}"), &format!("b/{name}"))
        .to_string()
}

/// Map used by the project tree for file colors.
pub type StatusMap = HashMap<PathBuf, FileStatus>;

impl GitPanel {
    fn github_issue_links(&self, ui: &mut egui::Ui, text: &str, repo: &GitRepo) {
        let settings = rustpy_settings::Settings::load();
        let mut servers: Vec<_> = settings
            .github
            .accounts
            .iter()
            .filter_map(|account| {
                rustpy_vcs::github::Server::new(&account.server, account.provider).ok()
            })
            .collect();
        for (origin, provider) in [
            (
                "https://github.com",
                rustpy_settings::github::Provider::GitHub,
            ),
            (
                "https://gitlab.com",
                rustpy_settings::github::Provider::GitLab,
            ),
        ] {
            if let Ok(server) = rustpy_vcs::github::Server::new(origin, provider) {
                servers.push(server);
            }
        }
        for (_, remote) in repo.remotes_with_urls().unwrap_or_default() {
            for server in &servers {
                if let Some(repository) = server.remote_repository(&remote) {
                    for url in crate::github_panel::issue_urls(server, &repository, text) {
                        ui.hyperlink_to("Open issue", url);
                    }
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmations_describe_what_happens() {
        let root = Path::new("/p");
        let added = Confirm::Rollback {
            path: root.join("new.py"),
            deletes: true,
            delete_local: false,
            renamed_from: None,
        };
        assert_eq!(
            added.button(),
            "Unstage",
            "an added file is kept unless the box is ticked"
        );
        assert!(!added.keeps_history());
        assert!(added.message(root).contains("new.py"));
        let delete = Confirm::Rollback {
            path: root.join("new.py"),
            deletes: true,
            delete_local: true,
            renamed_from: None,
        };
        assert_eq!(delete.button(), "Rollback");
        assert!(delete.keeps_history());
        let modified = Confirm::Rollback {
            path: root.join("a.py"),
            deletes: false,
            delete_local: false,
            renamed_from: None,
        };
        assert!(modified
            .message(root)
            .starts_with("Discard the changes to a.py"));
        let renamed = Confirm::Rollback {
            path: root.join("geometry.py"),
            deletes: true,
            delete_local: false,
            renamed_from: Some(root.join("shapes.py")),
        };
        assert_eq!(
            renamed.button(),
            "Rollback",
            "the old file comes back either way"
        );
        assert!(
            renamed.message(root).contains("restores shapes.py")
                && renamed.message(root).contains("geometry.py is kept"),
            "{}",
            renamed.message(root)
        );
        assert!(Confirm::ResetHard("0123456789abcdef".into())
            .message(root)
            .contains("0123456"));
        assert!(!Confirm::DropStash {
            index: 0,
            message: "wip".into()
        }
        .keeps_history());
        assert!(Confirm::Abort("merge".into()).keeps_history());
        assert_eq!(Confirm::SkipCommit.button(), "Skip");
        assert!(
            Confirm::SkipCommit.keeps_history(),
            "a skipped commit's resolutions are copied to Local History first"
        );
        let force = Confirm::ForcePush("origin/main".into());
        assert!(
            force.message(root).contains("--force-with-lease")
                && force.message(root).contains("origin/main")
        );
        assert!(!force.keeps_history());
    }

    #[test]
    fn working_tree_snapshot_covers_changed_files() {
        let dir = std::env::temp_dir().join(format!("rustpy-gitpanel-snap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let work = dir.join("repo");
        std::fs::create_dir_all(&work).unwrap();
        let repo = GitRepo::init(&work).unwrap();
        let root = repo.root().to_path_buf();
        std::fs::write(root.join("a.py"), "a = 1\n").unwrap();
        repo.commit(&[root.join("a.py")], "init", false).unwrap();
        std::fs::write(root.join("a.py"), "a = 2\n").unwrap();
        std::fs::write(root.join("scratch.py"), "tmp\n").unwrap();
        let base = dir.join("history");
        assert_eq!(
            snapshot_working_tree_in(&base, &repo, &root, "Before reset --hard", false),
            1,
            "tracked changes only"
        );
        assert_eq!(
            snapshot_working_tree_in(&base, &repo, &root, "Before stash", true),
            1,
            "a.py is unchanged since; scratch.py is new"
        );
        let entries = history_store::list_entries(&history_store::file_dir(
            &base,
            Some(&root),
            &root.join("a.py"),
        ));
        assert_eq!(entries.len(), 1);
        assert_eq!(
            std::fs::read_to_string(&entries[0].file).unwrap(),
            "a = 2\n"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    /// A fresh repository in a unique temporary directory (removed on drop).
    struct TempRepo(PathBuf);

    impl TempRepo {
        fn new(name: &str) -> (TempRepo, GitRepo) {
            let dir = std::env::temp_dir()
                .join(format!("rustpy-vcs-panel-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let repo = GitRepo::init(&dir).unwrap();
            // Commits go through the git CLI, which needs an identity; hosted CI
            // runners have no global one, so the fixture repository sets its own.
            for (key, value) in [("user.name", "Ann"), ("user.email", "ann@example.com")] {
                let _ = std::process::Command::new("git")
                    .args(["config", key, value])
                    .current_dir(&dir)
                    .output();
            }
            (TempRepo(dir), repo)
        }

        fn commit(&self, repo: &GitRepo, file: &str, text: &str, message: &str) -> String {
            std::fs::write(self.0.join(file), text).unwrap();
            repo.commit(&[self.0.join(file)], message, false).unwrap()
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn real_git_change_and_log_rows_keep_keyboard_menu_identity_and_modal_authority() {
        fn key(key: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
            vec![
                egui::Event::ModifiersChanged(modifiers),
                egui::Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed: true,
                    repeat: false,
                    modifiers,
                },
                egui::Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed: false,
                    repeat: false,
                    modifiers,
                },
            ]
        }
        fn frame(
            panel: &mut GitPanel,
            repo: &GitRepo,
            ctx: &egui::Context,
            events: Vec<egui::Event>,
        ) -> egui::accesskit::TreeUpdate {
            let output = ctx.run_ui(
                egui::RawInput {
                    events,
                    focused: true,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1400.0, 850.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    panel.ui(ui, &UiTheme::high_contrast(), Some(repo), repo.root(), 0.1);
                },
            );
            let update = output.platform_output.accesskit_update.clone().unwrap();
            output.drop_without_applying_deltas();
            update
        }
        fn tab_to(
            panel: &mut GitPanel,
            repo: &GitRepo,
            ctx: &egui::Context,
            target: &str,
        ) -> egui::Id {
            for _ in 0..100 {
                let update = frame(panel, repo, ctx, key(egui::Key::Tab, egui::Modifiers::NONE));
                let focused = ctx.memory(|memory| memory.focused());
                if update.nodes.iter().any(|(id, node)| {
                    node.label()
                        .or_else(|| node.value())
                        .is_some_and(|label| label.contains(target))
                        && focused.is_some_and(|focused| focused.accesskit_id() == *id)
                }) {
                    return focused.unwrap();
                }
            }
            panic!("real Tab reaches {target}");
        }
        let (temporary, repo) = TempRepo::new("keyboard-menu-π");
        temporary.commit(&repo, "tracked.py", "π = 1\n", "Unicode π initial");
        std::fs::write(temporary.0.join("π.py"), "π = '😀'\n").unwrap();
        let mut panel = GitPanel::default();
        panel.refresh(&repo, 0.0);
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.enable_accesskit();
        frame(&mut panel, &repo, &ctx, vec![]);
        frame(&mut panel, &repo, &ctx, vec![]);
        tab_to(&mut panel, &repo, &ctx, "π.py");
        frame(
            &mut panel,
            &repo,
            &ctx,
            key(egui::Key::F10, egui::Modifiers::CTRL | egui::Modifiers::ALT),
        );
        let update = frame(&mut panel, &repo, &ctx, vec![]);
        for label in ["Show Diff", "Open in Editor", "Delete…"] {
            assert!(
                update
                    .nodes
                    .iter()
                    .any(|(_, node)| node.label() == Some(label)),
                "existing change menu contains {label}"
            );
        }
        frame(
            &mut panel,
            &repo,
            &ctx,
            key(egui::Key::Escape, egui::Modifiers::NONE),
        );
        tab_to(&mut panel, &repo, &ctx, "π.py");
        // Replace the real untracked entry, then obtain a fresh provider snapshot.
        std::fs::remove_file(temporary.0.join("π.py")).unwrap();
        std::fs::write(temporary.0.join("replacement.py"), "other = 2\n").unwrap();
        panel.refresh(&repo, 0.0);
        let update = frame(&mut panel, &repo, &ctx, vec![]);
        assert!(
            !update.nodes.iter().any(|(id, node)| node
                .label()
                .or_else(|| node.value())
                .is_some_and(|label| label.contains("replacement.py"))
                && ctx
                    .memory(|memory| memory.focused())
                    .is_some_and(|focused| focused.accesskit_id() == *id)),
            "a replacement change cannot inherit focused-row authority"
        );
        panel.tab = GitTab::Log;
        frame(&mut panel, &repo, &ctx, vec![]);
        tab_to(&mut panel, &repo, &ctx, "Revision");
        frame(
            &mut panel,
            &repo,
            &ctx,
            key(egui::Key::F10, egui::Modifiers::CTRL | egui::Modifiers::ALT),
        );
        let update = frame(&mut panel, &repo, &ctx, vec![]);
        for label in ["Checkout Revision", "Cherry-pick", "Copy Revision Number"] {
            assert!(
                update
                    .nodes
                    .iter()
                    .any(|(_, node)| node.label() == Some(label)),
                "existing log menu contains {label}"
            );
        }
        frame(
            &mut panel,
            &repo,
            &ctx,
            key(egui::Key::Escape, egui::Modifiers::NONE),
        );
        tab_to(&mut panel, &repo, &ctx, "Revision");
        panel.set_accessibility_owner(false);
        frame(
            &mut panel,
            &repo,
            &ctx,
            key(egui::Key::F10, egui::Modifiers::CTRL | egui::Modifiers::ALT),
        );
        assert!(
            !egui::Popup::is_any_open(&ctx),
            "foreign owner blocks the retained Git row"
        );
        assert_eq!(
            std::fs::read_to_string(temporary.0.join("tracked.py")).unwrap(),
            "π = 1\n"
        );
        for status in [
            FileStatus::Added,
            FileStatus::Modified,
            FileStatus::Deleted,
            FileStatus::Untracked,
        ] {
            assert_eq!(
                status_color(status, &UiTheme::high_contrast()),
                Some(UiTheme::high_contrast().text)
            );
        }
    }

    #[test]
    fn all_branches_log_lists_every_branch_in_graph_order() {
        let (t, repo) = TempRepo::new("log");
        t.commit(&repo, "a.py", "a\n", "initial");
        let main = repo.current_branch().unwrap();
        repo.create_branch("feature", true).unwrap();
        t.commit(&repo, "f.py", "f\n", "feature only");
        repo.checkout_branch(&main).unwrap();
        t.commit(&repo, "m.py", "m\n", "main only");
        let mut panel = GitPanel::default();
        panel.refresh(&repo, 0.0);
        assert_eq!(panel.log.len(), 2);
        assert!(!panel.log.iter().any(|c| c.summary == "feature only"));
        panel.all_branches = true;
        panel.reload_log(&repo);
        assert_eq!(panel.log.len(), 3);
        assert!(
            panel.log.iter().any(|c| c.summary == "feature only"),
            "commits only on other branches are listed"
        );
        // Rows and lanes come from the same walk, so edges join adjacent rows.
        let log_ids: Vec<&String> = panel.log.iter().map(|c| &c.id).collect();
        let graph_ids: Vec<&String> = panel.graph.iter().map(|g| &g.id).collect();
        assert_eq!(log_ids, graph_ids);
        // Filter by text (any case), author and hash.
        assert_eq!(
            panel
                .log
                .iter()
                .filter(|c| commit_matches(c, "FEATURE"))
                .count(),
            1
        );
        let author = panel.log[0].author.to_uppercase();
        assert_eq!(
            panel
                .log
                .iter()
                .filter(|c| commit_matches(c, &author))
                .count(),
            3
        );
        let first = panel.log[0].clone();
        assert!(commit_matches(&first, &first.id[..8]));
        assert!(!commit_matches(&first, "no such text"));
        assert!(commit_matches(&first, "  "));
        // Paging: the limit bounds the walk and "Load more" raises it.
        panel.log_limit = 2;
        panel.reload_log(&repo);
        assert_eq!(panel.log.len(), 2);
        panel.log_limit += LOG_PAGE;
        panel.reload_log(&repo);
        assert_eq!(panel.log.len(), 3);
    }

    #[test]
    fn graph_lanes_join_between_rows_and_branch_tips_start_at_their_dot() {
        // Three unmerged branches: main, plus topic and feature forked off it.
        let (t, repo) = TempRepo::new("lanes");
        t.commit(&repo, "a.py", "a\n", "initial");
        let main = repo.current_branch().unwrap();
        repo.create_branch("feature", true).unwrap();
        t.commit(&repo, "f.py", "f\n", "feature one");
        t.commit(&repo, "g.py", "g\n", "feature two");
        repo.checkout_branch(&main).unwrap();
        t.commit(&repo, "b.py", "b\n", "main second");
        repo.create_branch("topic", true).unwrap();
        t.commit(&repo, "t.py", "t\n", "topic work");
        repo.checkout_branch(&main).unwrap();
        t.commit(&repo, "c.py", "c\n", "main third");
        let graph = repo.log_graph(50, true).unwrap();
        assert_eq!(graph.len(), 6);
        // Rows as laid out in the Log: 22 px high and 4 px apart; edges cross
        // from one row to the next in the middle of the gap.
        let (height, gap) = (22.0, 4.0);
        let lane_x = |lane: usize| 8.0 + lane as f32 * 14.0;
        let rows: Vec<(f32, f32, f32, Vec<GraphSegment>)> = (0..graph.len())
            .map(|r| {
                let top = r as f32 * (height + gap);
                let (y, upper, lower) = (
                    top + height / 2.0,
                    top - gap / 2.0,
                    top + height + gap / 2.0,
                );
                let above = r.checked_sub(1).map(|p| graph[p].lanes_in.as_slice());
                (
                    y,
                    upper,
                    lower,
                    graph_row_segments(above, &graph[r].lanes_in, lane_x, y, upper, lower),
                )
            })
            .collect();
        let at = |p: egui::Pos2, lane: usize| ((p.x * 100.0).round() as i64, lane);
        let mut tips_below_the_first_row = 0;
        for (r, g) in graph.iter().enumerate() {
            let (y, upper, lower, segments) = &rows[r];
            // A line runs into the dot from above exactly when a child is listed above.
            let dot = egui::Pos2::new(lane_x(g.lane), *y);
            let line_from_above = segments
                .iter()
                .any(|(s, _)| s[0].y == *upper && s[1] == dot);
            let has_child_above = graph[..r].iter().any(|c| c.parents.contains(&g.id));
            assert_eq!(line_from_above, has_child_above, "row {r} '{}'", g.summary);
            tips_below_the_first_row += usize::from(r > 0 && !has_child_above);
            // Every lane leaving a row through its bottom enters the next row at the same point.
            if let Some((_, next_upper, _, next)) = rows.get(r + 1) {
                assert_eq!(lower, next_upper);
                let mut leaving: Vec<_> = segments
                    .iter()
                    .filter(|(s, _)| s[1].y == *lower)
                    .map(|(s, lane)| at(s[1], *lane))
                    .collect();
                let mut entering: Vec<_> = next
                    .iter()
                    .filter(|(s, _)| s[0].y == *next_upper)
                    .map(|(s, lane)| at(s[0], *lane))
                    .collect();
                leaving.sort_unstable();
                entering.sort_unstable();
                assert!(!leaving.is_empty(), "rows {r} and {} are connected", r + 1);
                assert_eq!(leaving, entering, "rows {r} and {}", r + 1);
            }
        }
        assert_eq!(
            tips_below_the_first_row, 2,
            "two of the three branch tips are listed below the first row"
        );
        assert!(
            rows[0].3.iter().all(|(s, _)| s[0].y >= rows[0].0),
            "nothing above the newest commit"
        );
    }

    #[test]
    fn interrupted_cherry_pick_prefills_the_commit_message() {
        let (t, repo) = TempRepo::new("prefill");
        t.commit(&repo, "a.py", "1\n2\n3\n", "initial");
        let main = repo.current_branch().unwrap();
        repo.create_branch("feature", true).unwrap();
        let picked = t.commit(&repo, "a.py", "1\ntheirs\n3\n", "Fix the second line");
        repo.checkout_branch(&main).unwrap();
        t.commit(&repo, "a.py", "1\nours\n3\n", "ours");
        assert_eq!(repo.cherry_pick(&picked).unwrap().len(), 1);
        let mut panel = GitPanel::default();
        panel.refresh(&repo, 0.0);
        assert_eq!(panel.operation.as_deref(), Some("cherry-pick"));
        assert_eq!(panel.conflicts.len(), 1);
        assert_eq!(panel.message, "Fix the second line");
        // Unresolved conflicts: no Commit, whatever is checked.
        assert!(!panel.can_commit());
        assert_eq!(
            panel.commit_checked(&repo, 1.0).unwrap_err(),
            "Commit failed: resolve the merge conflicts first"
        );
        // The user's edits are not overwritten by later refreshes.
        panel.message = "Fix the second line (picked)".into();
        panel.refresh(&repo, 3.0);
        assert_eq!(panel.message, "Fix the second line (picked)");
        // Aborting elsewhere drops an untouched prefilled message only.
        panel.message = "Fix the second line".into();
        repo.abort_operation().unwrap();
        panel.refresh(&repo, 6.0);
        assert_eq!(panel.operation, None);
        assert!(panel.message.is_empty());
    }

    #[test]
    fn unchecked_changes_stay_unchecked_across_refreshes() {
        let (t, repo) = TempRepo::new("checked");
        t.commit(&repo, "a.py", "a\n", "initial");
        t.commit(&repo, "b.py", "b\n", "second");
        t.commit(&repo, "c.py", "c\n", "third");
        std::fs::write(t.0.join("a.py"), "a2\n").unwrap();
        std::fs::write(t.0.join("b.py"), "b2\n").unwrap();
        std::fs::write(t.0.join("new.py"), "n\n").unwrap();
        let mut panel = GitPanel::default();
        panel.refresh(&repo, 0.0);
        assert!(
            panel.checked.contains(&t.0.join("a.py")) && panel.checked.contains(&t.0.join("b.py"))
        );
        assert!(
            !panel.checked.contains(&t.0.join("new.py")),
            "unversioned files start unchecked"
        );
        panel.checked.remove(&t.0.join("b.py"));
        panel.refresh(&repo, 3.0);
        assert!(
            !panel.checked.contains(&t.0.join("b.py")),
            "the periodic refresh must not re-check it"
        );
        // Committing the checked change leaves the unchecked one alone; a change
        // that appears later starts checked.
        let paths: Vec<PathBuf> = panel.checked.iter().cloned().collect();
        assert_eq!(paths, vec![t.0.join("a.py")]);
        repo.commit(&paths, "only a", false).unwrap();
        panel.refresh(&repo, 6.0);
        assert_eq!(
            panel
                .entries
                .iter()
                .filter(|e| e.status != FileStatus::Untracked)
                .count(),
            1
        );
        std::fs::write(t.0.join("c.py"), "c2\n").unwrap();
        panel.refresh(&repo, 9.0);
        assert!(!panel.checked.contains(&t.0.join("b.py")));
        assert!(panel.checked.contains(&t.0.join("c.py")));
        assert_eq!(
            repo.head_text(&t.0.join("b.py")).as_deref(),
            Some("b\n"),
            "the unchecked change was not committed"
        );
    }

    #[test]
    fn renamed_changes_show_both_paths() {
        let root = PathBuf::from("/p");
        let e = StatusEntry {
            path: root.join("pkg/b.py"),
            status: FileStatus::Renamed,
            staged: false,
            old_path: Some(root.join("b.py")),
        };
        assert_eq!(change_label(&e, &root), "pkg/b.py (moved from b.py)");
        let e = StatusEntry {
            path: root.join("pkg/c.py"),
            status: FileStatus::Renamed,
            staged: true,
            old_path: Some(root.join("pkg/b.py")),
        };
        assert_eq!(change_label(&e, &root), "pkg/c.py (renamed from b.py)");
        let e = StatusEntry {
            path: root.join("a.py"),
            status: FileStatus::Modified,
            staged: false,
            old_path: None,
        };
        assert_eq!(change_label(&e, &root), "a.py");
    }

    #[test]
    fn a_rename_nobody_added_starts_unchecked() {
        let (t, repo) = TempRepo::new("unversioned-rename");
        t.commit(&repo, "a.py", "line1\nline2\nline3\nline4\n", "initial");
        // A deleted tracked file plus a similar new file that was never added.
        std::fs::remove_file(t.0.join("a.py")).unwrap();
        std::fs::write(
            t.0.join("secrets.py"),
            "line1\nline2\nline3\nline4\nTOKEN = 'x'\n",
        )
        .unwrap();
        std::fs::write(t.0.join("new.py"), "n\n").unwrap();
        let mut panel = GitPanel::default();
        panel.refresh(&repo, 0.0);
        let rename = panel
            .entries
            .iter()
            .find(|e| e.status == FileStatus::Renamed)
            .expect("listed as one rename");
        assert!(rename.is_unversioned());
        assert!(
            panel.checked.is_empty(),
            "neither the look-alike nor the untracked file is committed by default: {:?}",
            panel.checked
        );
        assert!(!panel.can_commit());
        // Checking it commits the rename as a whole.
        panel.checked.insert(t.0.join("secrets.py"));
        panel.message = "Rename a to secrets".into();
        let done = panel.commit_checked(&repo, 1.0).unwrap();
        assert!(
            done.status.starts_with("Committed 1 file(s)"),
            "{}",
            done.status
        );
        assert_eq!(repo.head_text(&t.0.join("a.py")), None);
        assert!(repo.head_text(&t.0.join("secrets.py")).is_some());
        assert_eq!(repo.head_text(&t.0.join("new.py")), None);
        assert_eq!(std::fs::read_to_string(t.0.join("new.py")).unwrap(), "n\n");
        assert_eq!(
            panel.entries.len(),
            1,
            "only the untracked new.py is left: {:?}",
            panel.entries
        );
    }

    #[test]
    fn commit_needs_a_message_even_when_amending() {
        let (t, repo) = TempRepo::new("amend-message");
        t.commit(&repo, "a.py", "a\n", "initial");
        t.commit(&repo, "b.py", "b\n", "Third commit: runme");
        let mut panel = GitPanel::default();
        panel.refresh(&repo, 0.0);
        // Amend with the message box emptied: refused, the last message stays.
        panel.amend = true;
        panel.message = "   ".into();
        assert!(!panel.can_commit());
        assert!(panel.commit_checked(&repo, 1.0).is_err());
        assert_eq!(repo.head_message().unwrap().trim(), "Third commit: runme");
        assert!(panel.amend, "nothing changed");
        // Rewording without checked changes is fine with a message.
        panel.message = "Third commit: runme (reworded)".into();
        assert!(panel.can_commit());
        let done = panel.commit_checked(&repo, 2.0).unwrap();
        assert!(
            done.status.starts_with("Amended the last commit"),
            "{}",
            done.status
        );
        assert!(!done.rewrote_pushed, "no upstream");
        assert_eq!(repo.log(10).unwrap().len(), 2, "amended, not a new commit");
        assert_eq!(
            repo.head_message().unwrap().trim(),
            "Third commit: runme (reworded)"
        );
        assert!(
            !panel.amend && panel.message.is_empty(),
            "Amend and the message are reset"
        );
        // A normal commit needs a message and a checked change.
        std::fs::write(t.0.join("a.py"), "a2\n").unwrap();
        panel.refresh(&repo, 3.0);
        assert!(!panel.can_commit(), "no message");
        panel.message = "Change a".into();
        assert!(panel.can_commit());
        panel.checked.clear();
        assert!(!panel.can_commit(), "nothing checked");
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Ann")
            .env("GIT_AUTHOR_EMAIL", "ann@example.com")
            .env("GIT_COMMITTER_NAME", "Ann")
            .env("GIT_COMMITTER_EMAIL", "ann@example.com")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn amending_a_pushed_commit_asks_for_a_force_push() {
        if !std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            return;
        }
        let (t, repo) = TempRepo::new("amend-pushed");
        t.commit(&repo, "a.py", "a\n", "initial");
        let bare = t.0.with_extension("origin.git");
        let _ = std::fs::remove_dir_all(&bare);
        git(&t.0, &["init", "-q", "--bare", bare.to_str().unwrap()]);
        git(&t.0, &["remote", "add", "origin", bare.to_str().unwrap()]);
        let branch = repo.current_branch().unwrap();
        git(&t.0, &["push", "-q", "-u", "origin", &branch]);
        let mut panel = GitPanel::default();
        panel.refresh(&repo, 0.0);
        // Commit and Push… with Amend on: the commit is amended, not duplicated.
        std::fs::write(t.0.join("a.py"), "a amended\n").unwrap();
        panel.refresh(&repo, 1.0);
        panel.amend = true;
        panel.message = repo.head_message().unwrap();
        let done = panel.commit_checked(&repo, 2.0).unwrap();
        assert!(
            done.rewrote_pushed,
            "the amended commit replaced one already on origin"
        );
        assert_eq!(repo.log(10).unwrap().len(), 1);
        assert_eq!(
            repo.head_text(&t.0.join("a.py")).as_deref(),
            Some("a amended\n")
        );
        panel.ask_force_push(&repo);
        assert!(
            matches!(&panel.confirm, Some(Confirm::ForcePush(up)) if up.ends_with(&branch)),
            "{:?}",
            panel.confirm
        );
        // Amending a commit that was never pushed needs no force push.
        std::fs::write(t.0.join("b.py"), "b\n").unwrap();
        panel.refresh(&repo, 3.0);
        panel.checked.insert(t.0.join("b.py"));
        panel.message = "local only".into();
        assert!(!panel.commit_checked(&repo, 4.0).unwrap().rewrote_pushed);
        panel.amend = true;
        panel.message = "local only, reworded".into();
        assert!(!panel.commit_checked(&repo, 5.0).unwrap().rewrote_pushed);
        std::fs::remove_dir_all(&bare).ok();
    }
}
