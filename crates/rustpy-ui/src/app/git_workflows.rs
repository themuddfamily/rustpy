//! Git Branches popup and Update Project (roadmap §4.1 Branches, Operations):
//! remote branches checked out as local tracking branches, Smart Checkout when
//! local changes are in the way, New Branch from any branch, merge, rebase and
//! pull into the current branch, and Update Project by merge or rebase on a
//! worker thread.

use super::*;
use crate::a11y::DialogWindow;
use rustpy_vcs::{CheckoutOutcome, SyncOutcome, UpdateMethod};

/// Protect project filesystem changes from concurrent Git operations.
pub(super) fn file_change<R>(
    path: &std::path::Path,
    label: &str,
    change: impl FnOnce() -> std::io::Result<R>,
) -> std::io::Result<R> {
    let _operation =
        rustpy_vcs::RepositoryOperation::enter(path, label).map_err(std::io::Error::other)?;
    change()
}

/// Options of the Update Project dialog (saved to Settings › Version Control › Git).
pub(super) struct UpdateDialog {
    all_roots: bool,
    submodules: bool,
    cleanup: u8,
    branch_default: bool,
    method: UpdateMethod,
    autostash: bool,
    dont_ask: bool,
}

/// What the Branches popup asked for in a frame.
pub(super) enum BranchOp {
    Checkout(String),
    CheckoutRemote(String),
    SmartCheckout(String),
    Rebase(String),
    Pull {
        remote_ref: String,
        method: UpdateMethod,
    },
}

/// Status line for an update, pull or rebase.
pub(super) fn describe_sync(what: &str, out: &SyncOutcome) -> String {
    let n = out.conflicts.len();
    if out.up_to_date {
        format!("{what}: already up to date")
    } else if n > 0 && out.stopped.is_some() {
        format!(
            "{what} stopped on {n} conflict(s): resolve them in the Git tool window, then Continue"
        )
    } else if n > 0 {
        format!("{what} finished, but re-applying your local changes conflicted in {n} file(s); they are also kept in the stash")
    } else if out.fast_forward {
        format!("{what}: fast-forwarded {} commit(s)", out.incoming)
    } else if out.incoming > 0 {
        format!("{what}: {} incoming commit(s) integrated", out.incoming)
    } else {
        format!("{what}: done")
    }
}

pub(super) fn update_information(
    repo: &GitRepo,
    before: Option<&str>,
    filter: &str,
    summary: &str,
) -> String {
    let mut message = summary.to_owned();
    if let Some(before) = before {
        if let Ok(files) = repo.diff_commits(before, "HEAD") {
            let paths = files
                .into_iter()
                .filter(|file| {
                    filter.trim().is_empty()
                        || filter
                            .split(',')
                            .map(str::trim)
                            .filter(|p| !p.is_empty())
                            .any(|prefix| {
                                file.path
                                    .strip_prefix(repo.root())
                                    .unwrap_or(&file.path)
                                    .starts_with(prefix)
                            })
                })
                .map(|file| {
                    file.path
                        .strip_prefix(repo.root())
                        .unwrap_or(&file.path)
                        .display()
                        .to_string()
                })
                .collect::<Vec<_>>();
            message.push_str(&format!(
                "; {} matching updated path(s): {}",
                paths.len(),
                paths.join(", ")
            ));
        }
    }
    message
}

/// Status line for a (smart) checkout.
fn describe_checkout(name: &str, out: &CheckoutOutcome) -> String {
    if !out.stashed {
        format!("Checked out '{name}'")
    } else if !out.conflicts.is_empty() {
        format!("Checked out '{name}'; re-applying your local changes conflicted in {} file(s): resolve them in the Git tool window (the changes are also kept in stash@{{0}})", out.conflicts.len())
    } else if out.stash_kept {
        format!("Checked out '{name}'; your local changes could not be re-applied and are kept in stash@{{0}}")
    } else {
        format!("Checked out '{name}' (local changes were stashed and re-applied)")
    }
}

/// File names for a status line (at most three, then "and N more").
fn file_names(paths: &[PathBuf]) -> String {
    let names: Vec<String> = paths
        .iter()
        .map(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string())
        })
        .collect();
    let mut out = names.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
    if names.len() > 3 {
        out.push_str(&format!(" and {} more", names.len() - 3));
    }
    out
}

/// Editors that [`IdeApp::save_for_vcs`] left unsaved.
#[derive(Debug, Default)]
pub(super) struct Unsaved {
    /// Their files changed on disk since the editor loaded or saved them (or
    /// a question about them is open): saving would overwrite that change.
    pub changed_on_disk: Vec<PathBuf>,
    /// Writing them failed.
    pub failed: Vec<PathBuf>,
}

/// The files among `paths` that a commit of the panel's checked changes would
/// include: checked ones, and the old path of a checked rename.
fn in_commit(panel: &crate::git_panel::GitPanel, paths: &[PathBuf]) -> Vec<PathBuf> {
    let renamed_from_checked = |p: &PathBuf| {
        panel
            .entries
            .iter()
            .any(|e| e.old_path.as_ref() == Some(p) && panel.checked.contains(&e.path))
    };
    paths
        .iter()
        .filter(|p| panel.checked.contains(*p) || renamed_from_checked(p))
        .cloned()
        .collect()
}

/// Status line for a checkout refused because of local files. Smart Checkout is
/// offered for tracked changes. Untracked blockers are named too; the branch
/// workspace can shelve them losslessly or offer an explicit force checkout.
fn refused_checkout(
    git: &GitRepo,
    name: &str,
    blocked: &mut Option<String>,
    changes_message: String,
) -> String {
    match git.checkout_blockers(name) {
        Ok(b) if !b.untracked.is_empty() => {
            *blocked = Some(name.into());
            b.untracked_message(git.root(), name)
        }
        _ => {
            *blocked = Some(name.to_string());
            changes_message
        }
    }
}

/// Why a git operation failed, in one line (git's `error:`/`fatal:` line).
fn short_error(e: &anyhow::Error) -> String {
    rustpy_vcs::failure_reason(e)
}

impl BranchOp {
    fn history_label(&self) -> Option<&'static str> {
        match self {
            Self::Checkout(_) | Self::CheckoutRemote(_) | Self::SmartCheckout(_) => {
                Some("Before checkout")
            }
            Self::Rebase(_) => Some("Before rebase"),
            Self::Pull { .. } => None, // Pull snapshots on its reserved worker.
        }
    }
    fn changes_files(&self) -> bool {
        true
    }
}

impl IdeApp {
    /// Reserve before saving or dispatching, so every job owner sees the
    /// same operation (also while cancelled processes are cleaning up).
    pub(super) fn reserve_git_operation(
        &mut self,
        label: &str,
    ) -> Option<rustpy_vcs::RepositoryReservation> {
        rustpy_vcs::set_git_program(&self.settings.vcs.git_executable);
        let Some(root) = self
            .project
            .as_ref()
            .and_then(|p| p.git.as_ref())
            .map(|g| g.root().to_path_buf())
        else {
            self.set_status("Not a Git repository");
            return None;
        };
        match rustpy_vcs::RepositoryReservation::try_acquire(&root, label) {
            Ok(operation) => Some(operation),
            Err(e) => {
                self.set_status(e.to_string());
                None
            }
        }
    }

    /// Runs a git job on a worker thread with its own repository handle; its
    /// message goes to the status bar and the Git views refresh when it is done.
    /// The changed files are first copied to Local History under `history_label`
    /// (on the worker, before the job touches them).
    pub(super) fn spawn_git_job(
        &mut self,
        busy: &str,
        history_label: &str,
        job: impl FnOnce(&GitRepo) -> String + Send + 'static,
    ) {
        let Some((root, hist_root)) = self.project.as_ref().and_then(|p| {
            p.git
                .as_ref()
                .map(|g| (g.root().to_path_buf(), p.root.clone()))
        }) else {
            self.set_status("Not a Git repository");
            return;
        };
        let Some(operation) = self.reserve_git_operation(busy.trim_end_matches('…')) else {
            return;
        };
        operation.run(|| self.save_for_vcs());
        let label = history_label.to_string();
        let (env, auth_finished) = self.git_panel.git_jobs.new_background_job();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            operation.run(|| {
                let msg = match GitRepo::discover(&root) {
                    Some(repo) => {
                        let repo = repo.with_git_environment(env);
                        crate::git_panel::snapshot_working_tree(&repo, &hist_root, &label, false);
                        job(&repo)
                    }
                    None => "Not a Git repository".to_string(),
                };
                let _ = auth_finished.send(msg.clone());
                let _ = tx.send(msg);
            })
        });
        self.git_panel.jobs.push(rx);
        self.set_status(busy);
    }

    /// Finishes the git jobs (Update Project, Pull) whose worker is done: shows
    /// their message, reloads editors whose files changed and, when a job left
    /// conflicts, opens Git › Local Changes on them. Other background jobs (Push,
    /// Docker commands) never switch tool windows.
    pub(super) fn poll_git_jobs(&mut self, ctx: &egui::Context) {
        if self.git_panel.jobs.is_empty() {
            return;
        }
        let mut done = Vec::new();
        self.git_panel.jobs.retain(|rx| match rx.try_recv() {
            Ok(m) => {
                done.push(m);
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => false,
        });
        ctx.request_repaint_after(std::time::Duration::from_millis(200));
        if done.is_empty() {
            return;
        }
        for m in done {
            self.set_status(m);
        }
        self.reload_changed_files();
        self.git_panel.last_refresh = -100.0;
        if let Some(p) = &mut self.project {
            p.git_refreshed = -100.0;
        }
        let conflicted = self
            .project
            .as_ref()
            .and_then(|p| p.git.as_ref())
            .is_some_and(|g| !g.conflicts().unwrap_or_default().is_empty());
        if conflicted {
            self.bottom = Some(BottomTool::Git);
            self.git_panel.tab = crate::git_panel::GitTab::LocalChanges;
        }
    }

    /// Whether modified tab `i` must not be saved behind the user's back: its
    /// file changed on disk since the editor loaded or saved it (see
    /// [`changed_on_disk`]), or a "File Changed on Disk" or "Unsaved Changes"
    /// question about it is open. The file watcher's queue is not consulted: it
    /// also lists the editor's own saves, while the recorded modification time
    /// tells them apart.
    pub(super) fn tab_changed_on_disk(&self, i: usize) -> bool {
        let Some(path) = self.tabs[i].state.path.as_deref() else {
            return false;
        };
        let pending = match &self.dialog {
            Dialog::ExternalChange(p) => p == path,
            Dialog::ConfirmClose { tab, .. } => *tab == i,
            _ => false,
        };
        pending
            || match self.file_vfs().read(path, self.editor_read_limits()) {
                Ok(snapshot) => {
                    self.disk_revisions.get(path).copied().flatten() != Some(snapshot.revision)
                }
                Err(_) => true,
            }
    }

    /// Saves the modified editors before a VCS operation reads or rewrites their
    /// files (Commit, checkout, merge, rebase, Update Project),
    /// so the operation sees what the editors show. A file changed on disk that
    /// the user has not decided about yet ("File Changed on Disk") is never
    /// overwritten: such tabs are skipped and named in the status bar.
    pub(super) fn save_for_vcs(&mut self) -> Unsaved {
        let mut out = Unsaved::default();
        for i in 0..self.tabs.len() {
            if !self.tabs[i].state.doc.is_modified() {
                continue;
            }
            if self.tab_changed_on_disk(i) {
                out.changed_on_disk.extend(self.tabs[i].state.path.clone());
            } else if !self.save_tab(i) {
                out.failed.extend(self.tabs[i].state.path.clone());
            }
        }
        if !out.changed_on_disk.is_empty() {
            self.set_status(format!(
                "Not saved: {} changed on disk; reload it, or save it to keep your version",
                file_names(&out.changed_on_disk)
            ));
        } else if !out.failed.is_empty() {
            self.set_status(format!("Cannot save {}", file_names(&out.failed)));
        }
        out
    }

    /// Commit / Commit and Push… in the Git tool window: saves the editors first
    /// (see [`IdeApp::save_for_vcs`]) so the commit has what they show, then
    /// commits the checked changes. When a checked file's editor could not be
    /// saved the commit is cancelled rather than committing a version the user
    /// did not choose.
    pub(super) fn commit_from_git_panel(&mut self, push: bool, _now: f64) {
        if !self.builtin_plugin_enabled("git") {
            self.set_status("Git bundled capability is disabled in Plugins");
            return;
        }
        if self.project_safe_mode() {
            self.notify_safe_mode("Committing changes and running commit hooks");
            return;
        }
        if self.vcs_workspace.commit.is_some() {
            self.set_status("A commit is already running");
            return;
        }
        if !self.git_panel.can_commit() {
            self.set_status(
                "Resolve conflicts, enter a message and select changes before committing",
            );
            return;
        }
        if !self.prepare_commit_workspace_submission() {
            return;
        }
        let Some(root) = self
            .project
            .as_ref()
            .and_then(|p| p.git.as_ref())
            .map(|g| g.root().to_path_buf())
        else {
            return;
        };
        if self.settings.code_style.optimize_imports_on_commit {
            if let Err(error) = self.optimize_checked_commit_imports() {
                self.set_status(format!("Commit cancelled: {error}"));
                return;
            }
        }
        let _operation = match rustpy_vcs::RepositoryOperation::enter(&root, "Committing") {
            Ok(operation) => operation,
            Err(e) => {
                self.set_status(e.to_string());
                return;
            }
        };
        if let Err(error) = self.prepare_notebooks_for_commit() {
            self.report_git_status(format!("Commit cancelled: {error}"));
            return;
        }
        let unsaved = self.save_for_vcs();
        let changed = in_commit(&self.git_panel, &unsaved.changed_on_disk);
        if !changed.is_empty() {
            self.set_status(format!(
                "Commit cancelled: {} changed on disk; reload it, or save it to keep your version",
                file_names(&changed)
            ));
            return;
        }
        let failed = in_commit(&self.git_panel, &unsaved.failed);
        if !failed.is_empty() {
            self.set_status(format!(
                "Commit cancelled: {} could not be saved",
                file_names(&failed)
            ));
            return;
        }
        if let Err(error) = self.git_panel.workspace_before_commit() {
            self.report_git_status(format!("Commit cancelled: {error}"));
            return;
        }
        let message = self.git_panel.message.trim().to_string();
        let amend = self.git_panel.amend;
        let mut paths = self.git_panel.checked.iter().cloned().collect::<Vec<_>>();
        for entry in &self.git_panel.entries {
            if self.git_panel.checked.contains(&entry.path) {
                if let Some(old) = &entry.old_path {
                    paths.push(old.clone());
                }
            }
        }
        paths.sort();
        paths.dedup();
        let rewrote = amend
            && self
                .project
                .as_ref()
                .and_then(|p| p.git.as_ref())
                .is_some_and(|g| matches!(g.ahead_behind(), Some((0, _))));
        drop(_operation);
        let reservation = match rustpy_vcs::RepositoryReservation::try_acquire(&root, "Committing")
        {
            Ok(r) => r,
            Err(e) => {
                self.set_status(e.to_string());
                return;
            }
        };
        let mut author_environment = match self.git_panel.commit_environment() {
            Ok(vars) => vars,
            Err(error) => {
                self.report_git_status(error);
                return;
            }
        };
        if self.git_panel.commit_uses_index() {
            author_environment.push(("RUSTPY_COMMIT_INDEX".into(), "1".into()));
        }
        let (id, mut env) = self.git_panel.git_jobs.new_job();
        env.vars.extend(author_environment);
        let (tx, rx) = std::sync::mpsc::channel();
        let task_close = self.take_task_commit_intent();
        self.vcs_workspace.commit = Some(super::vcs_mapping_ui::PendingCommit {
            rx,
            root: root.clone(),
            message: message.clone(),
            amend,
            push,
            rewrote,
            id,
            task_close,
        });
        let history_root = self
            .project
            .as_ref()
            .map(|project| project.root.clone())
            .unwrap_or_else(|| root.clone());
        std::thread::spawn(move || {
            let result = reservation.run(|| {
                let repo = GitRepo::discover(&root)
                    .ok_or_else(|| "Repository is no longer available".to_string())?;
                crate::git_panel::snapshot_working_tree(
                    &repo,
                    &history_root,
                    "Before commit",
                    false,
                );
                repo.commit_with_policy(&paths, &message, amend, &env)
                    .map_err(|e| e.to_string())
            });
            let _ = tx.send(result);
        });
        self.set_status("Committing…");
    }

    /// Force push (`--force-with-lease`) once the user confirmed replacing an
    /// amended commit that was already pushed; refused by the remote when
    /// someone else pushed to the branch since the last fetch.
    pub(super) fn force_push_in_background(&mut self) {
        if !self.builtin_plugin_enabled("git") {
            return;
        }
        if self.project_safe_mode() {
            self.notify_safe_mode("Version control commands and hooks");
            return;
        }
        let Some((root, mut args)) = self.project.as_ref().and_then(|p| {
            p.git
                .as_ref()
                .map(|g| (g.root().to_path_buf(), g.push_args()))
        }) else {
            self.set_status("Not a Git repository");
            return;
        };
        args.push("--force-with-lease".to_string());
        let Some(operation) = self.reserve_git_operation("Force pushing") else {
            return;
        };
        let (env, auth_finished) = self.git_panel.git_jobs.new_background_job();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            operation.run(|| {
                let msg = match GitRepo::run_git_in_with_environment(&root, &args, &env) {
                    Ok(o) => format!(
                        "Force pushed: {}",
                        o.lines()
                            .rfind(|l| !l.trim().is_empty())
                            .unwrap_or("done")
                            .trim()
                    ),
                    Err(e) => format!("Force push failed: {}", short_error(&e)),
                };
                let _ = auth_finished.send(msg.clone());
                let _ = tx.send(msg);
            })
        });
        self.background_status.push(rx);
        self.set_status("Force pushing…");
    }

    /// Update Project (Ctrl+T, Git › Pull): asks for merge or rebase unless
    /// "Don't show again" was chosen.
    pub(super) fn update_project(&mut self) {
        if self.project.as_ref().and_then(|p| p.git.as_ref()).is_none() {
            self.set_status("Not a Git repository");
            return;
        }
        let vcs = &self.settings.vcs;
        if vcs.update_dont_ask {
            self.run_update(
                UpdateMethod::from_setting(&vcs.update_method),
                vcs.update_autostash,
            );
        } else {
            self.dialog = Dialog::UpdateProject(UpdateDialog {
                all_roots: self.settings.vcs_operations.update_all_roots,
                submodules: self.settings.vcs_operations.update_submodules,
                cleanup: self.settings.vcs_operations.update_cleanup.min(1),
                branch_default: self.settings.vcs_operations.update_branch_default,
                method: UpdateMethod::from_setting(&vcs.update_method),
                autostash: vcs.update_autostash,
                dont_ask: false,
            });
        }
    }

    fn run_update(&mut self, method: UpdateMethod, autostash: bool) {
        self.queue_update_project(method == UpdateMethod::Rebase, autostash);
    }

    pub(super) fn update_project_dialog(&mut self, ctx: &egui::Context, mut st: UpdateDialog) {
        let theme = self.theme.clone();
        let mut open = true;
        let (mut go, mut cancel) = (false, false);
        let (branch, upstream) = self
            .project
            .as_ref()
            .and_then(|p| p.git.as_ref())
            .map(|g| (g.current_branch(), g.upstream_of_current()))
            .unwrap_or((None, None));
        egui::Window::new("Update Project").collapsible(false).resizable(false).anchor(Align2::CENTER_CENTER, Vec2::ZERO).open(&mut open).show_modal(ctx, |ui| {
            let name = branch.as_deref().unwrap_or("HEAD");
            match &upstream {
                Some(u) => ui.label(format!("Update '{name}' from '{u}'")),
                None => ui.label(egui::RichText::new(format!("'{name}' has no upstream branch: push it first or check out a remote branch")).color(theme.warning)),
            };
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Update type").strong());
            ui.radio_value(&mut st.method, UpdateMethod::Merge, "Merge incoming changes into the current branch");
            ui.radio_value(&mut st.method, UpdateMethod::Rebase, "Rebase the current branch on top of incoming changes");
            ui.add_space(4.0);
            ui.checkbox(&mut st.autostash, "Stash local changes before updating and restore them afterwards");
            ui.checkbox(&mut st.branch_default,"Use each branch's configured merge/rebase default");
            ui.checkbox(&mut st.all_roots,"Update all mapped Git roots");
            ui.checkbox(&mut st.submodules,"Initialize and update submodules");
            ui.add_enabled_ui(st.autostash,|ui|{ui.radio_value(&mut st.cleanup,0,"Stash tracked changes automatically");ui.radio_value(&mut st.cleanup,1,"Shelve index/worktree/untracked changes with recovery reference");});
            ui.checkbox(&mut st.dont_ask, "Don't show again (Settings › Version Control › Git)");
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                go = ui.add_enabled(upstream.is_some(), egui::Button::new("Update")).clicked() || (upstream.is_some() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                cancel = ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
            });
        });
        if go {
            self.settings.vcs_operations.update_all_roots = st.all_roots;
            self.settings.vcs_operations.update_submodules = st.submodules;
            self.settings.vcs_operations.update_cleanup = st.cleanup;
            self.settings.vcs_operations.update_branch_default = st.branch_default;
            let vcs = &mut self.settings.vcs;
            vcs.update_method = st.method.setting().to_string();
            vcs.update_autostash = st.autostash;
            vcs.update_dont_ask = st.dont_ask;
            let _ = self.settings.save();
            self.run_update(st.method, st.autostash);
        } else if open && !cancel {
            self.dialog = Dialog::UpdateProject(st);
        }
    }

    /// The popup and the docked Branches tab share the same tree and commands.
    pub(super) fn branches_dialog(&mut self, ctx: &egui::Context) {
        self.branch_workspace_dialog(ctx);
    }

    /// Runs a Branches popup operation; returns whether the popup stays open
    /// (a prompt is waiting or the input needs fixing).
    pub(super) fn run_branch_op(&mut self, op: BranchOp) -> bool {
        if self.settings.vcs.preferences.synchronize_branches {
            if let BranchOp::Checkout(name) = &op {
                let roots = self
                    .vcs_workspace
                    .mappings
                    .roots
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>();
                if roots.len() > 1 {
                    let name = name.clone();
                    let project = self
                        .project
                        .as_ref()
                        .map(|p| p.root.clone())
                        .unwrap_or_default();
                    self.save_for_vcs();
                    let (tx, rx) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let mut messages = Vec::new();
                        for root in roots {
                            let Some(repo) = GitRepo::discover(&root) else {
                                continue;
                            };
                            crate::git_panel::snapshot_working_tree(
                                &repo,
                                &project,
                                "Before synchronized checkout",
                                false,
                            );
                            let result = repo.checkout_branch(&name);
                            messages.push(format!(
                                "{}: {}",
                                root.display(),
                                match result {
                                    Ok(()) => format!("checked out {name}"),
                                    Err(e) => e.to_string(),
                                }
                            ));
                        }
                        let _ = tx.send(messages.join("\n"));
                    });
                    self.git_panel.jobs.push(rx);
                    self.set_status("Checking out branch across mapped roots…");
                    return false;
                }
            }
        }
        if let BranchOp::Pull { remote_ref, method } = op {
            let autostash = self.settings.vcs.update_autostash;
            let what = format!("Pull '{remote_ref}' ({})", method.setting());
            let busy = format!("Pulling '{remote_ref}'…");
            self.spawn_git_job(&busy, "Before pull", move |repo| {
                match repo.pull_into_current(&remote_ref, method, autostash) {
                    Ok(out) => describe_sync(&what, &out),
                    Err(e) => format!("{what} failed: {}", short_error(&e)),
                }
            });
            return false;
        }
        let Some(operation) = self.reserve_git_operation("Branch operation") else {
            return true;
        };
        operation.run(|| self.run_reserved_branch_op(op))
    }

    fn run_reserved_branch_op(&mut self, op: BranchOp) -> bool {
        if op.changes_files() {
            self.save_for_vcs();
        }
        let Some(git) = self.project.as_ref().and_then(|p| p.git.as_ref()) else {
            return false;
        };
        // Checkout, merge and rebase rewrite files: copy the changed ones to Local History first.
        if let Some(label) = op.history_label() {
            let hist_root = self
                .project
                .as_ref()
                .map(|p| p.root.clone())
                .unwrap_or_default();
            crate::git_panel::snapshot_working_tree(git, &hist_root, label, false);
        }
        let autostash = self.settings.vcs.update_autostash;
        let mut keep_open = false;
        let mut show_conflicts = false;
        let mut blocked: Option<String> = None;
        let msg = match op {
            BranchOp::Checkout(name) => match git.checkout_branch(&name) {
                Ok(()) => format!("Checked out '{name}'"),
                Err(e) if GitRepo::is_checkout_conflict(&e) => refused_checkout(
                    git,
                    &name,
                    &mut blocked,
                    format!("Local changes block checking out '{name}'"),
                ),
                Err(e) => e.to_string(),
            },
            BranchOp::CheckoutRemote(remote_ref) => {
                match git.checkout_remote_branch(&remote_ref) {
                    Ok(m) => m,
                    Err(e) if GitRepo::is_checkout_conflict(&e) => {
                        let local = git
                            .split_remote_ref(&remote_ref)
                            .map(|(_, b)| b)
                            .unwrap_or(remote_ref.clone());
                        refused_checkout(git, &local, &mut blocked, format!("Local changes block checking out '{local}' (tracking '{remote_ref}')"))
                    }
                    Err(e) => e.to_string(),
                }
            }
            BranchOp::SmartCheckout(name) => {
                self.git_panel.blocked_checkout = None;
                match git.smart_checkout(&name) {
                    Ok(out) => {
                        show_conflicts = !out.conflicts.is_empty();
                        describe_checkout(&name, &out)
                    }
                    Err(e) => format!("Smart Checkout failed: {e:#}"),
                }
            }
            BranchOp::Rebase(name) => match git.rebase_onto(&name, autostash) {
                Ok(out) => {
                    show_conflicts = !out.conflicts.is_empty();
                    describe_sync(&format!("Rebase onto '{name}'"), &out)
                }
                Err(e) => format!("Rebase onto '{name}' failed: {}", short_error(&e)),
            },
            BranchOp::Pull { .. } => unreachable!("pulls run on a reserved worker"),
        };
        if let Some(b) = blocked {
            self.git_panel.blocked_checkout = Some(b);
            keep_open = true;
        }
        self.set_status(msg);
        if let Some(p) = &mut self.project {
            p.git_refreshed = -100.0;
        }
        self.git_panel.last_refresh = -100.0;
        self.reload_changed_files();
        if show_conflicts {
            self.bottom = Some(BottomTool::Git);
            self.git_panel.tab = crate::git_panel::GitTab::LocalChanges;
        }
        keep_open
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_messages() {
        let up = SyncOutcome {
            up_to_date: true,
            ..SyncOutcome::default()
        };
        assert_eq!(
            describe_sync("Update Project", &up),
            "Update Project: already up to date"
        );
        let stopped = SyncOutcome {
            conflicts: vec![PathBuf::from("a.py")],
            stopped: Some("rebase".into()),
            ..SyncOutcome::default()
        };
        assert!(describe_sync("Update Project", &stopped).contains("stopped on 1 conflict(s)"));
        let ff = SyncOutcome {
            fast_forward: true,
            incoming: 3,
            ..SyncOutcome::default()
        };
        assert_eq!(
            describe_sync("Pull", &ff),
            "Pull: fast-forwarded 3 commit(s)"
        );
        let co = CheckoutOutcome {
            stashed: true,
            conflicts: vec![PathBuf::from("a.py")],
            stash_kept: true,
        };
        assert!(describe_checkout("feature", &co).contains("conflicted in 1 file(s)"));
        assert_eq!(
            describe_checkout("main", &CheckoutOutcome::default()),
            "Checked out 'main'"
        );
    }

    #[test]
    fn retained_checkout_routes_save_and_snapshot_before_rewriting() {
        for (op, label) in [
            (BranchOp::Checkout("topic".into()), "Before checkout"),
            (
                BranchOp::CheckoutRemote("origin/topic".into()),
                "Before checkout",
            ),
            (BranchOp::SmartCheckout("topic".into()), "Before checkout"),
            (BranchOp::Rebase("topic".into()), "Before rebase"),
        ] {
            assert!(op.changes_files());
            assert_eq!(op.history_label(), Some(label));
        }
        use rustpy_vcs::branches::LocalAction;
        assert!(LocalAction::Create {
            name: "topic".into(),
            start: "HEAD".into(),
            checkout: true,
            overwrite: false
        }
        .changes_files());
        assert!(!LocalAction::Create {
            name: "topic".into(),
            start: "HEAD".into(),
            checkout: false,
            overwrite: false
        }
        .changes_files());
        assert!(!LocalAction::Delete {
            branch: "topic".into(),
            force: false
        }
        .changes_files());
    }

    #[test]
    fn file_names_are_bounded() {
        assert_eq!(
            file_names(&[PathBuf::from("/p/a.py"), PathBuf::from("/p/pkg/b.py")]),
            "a.py, b.py"
        );
        assert_eq!(
            file_names(
                &(0..5)
                    .map(|i| PathBuf::from(format!("/p/f{i}.py")))
                    .collect::<Vec<_>>()
            ),
            "f0.py, f1.py, f2.py and 2 more"
        );
    }

    #[test]
    fn unsaved_editors_of_checked_changes_cancel_the_commit() {
        use rustpy_vcs::{FileStatus, StatusEntry};
        let p = |n: &str| PathBuf::from("/p").join(n);
        let mut panel = crate::git_panel::GitPanel::default();
        panel.entries = vec![
            StatusEntry {
                path: p("a.py"),
                status: FileStatus::Modified,
                staged: false,
                old_path: None,
            },
            StatusEntry {
                path: p("b.py"),
                status: FileStatus::Modified,
                staged: false,
                old_path: None,
            },
            StatusEntry {
                path: p("new.py"),
                status: FileStatus::Renamed,
                staged: true,
                old_path: Some(p("old.py")),
            },
        ];
        panel.checked = [p("a.py"), p("new.py")].into_iter().collect();
        // a.py is checked; old.py goes with the checked rename; b.py and c.py are not in the commit.
        assert_eq!(
            in_commit(&panel, &[p("a.py"), p("b.py"), p("old.py"), p("c.py")]),
            vec![p("a.py"), p("old.py")]
        );
        panel.checked.clear();
        assert!(in_commit(&panel, &[p("a.py"), p("old.py")]).is_empty());
    }
}
