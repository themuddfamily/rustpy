//! Branch workspace commands reuse shared mappings, operation locks and jobs.
use super::*;
use crate::a11y::DialogWindow;
use crate::git_panel::branch_workspace::{Command, Completion, Event};
use rustpy_vcs::branches::LocalAction;
impl IdeApp {
    pub(super) fn prepare_branch_workspace(&mut self) {
        if self.git_panel.branch_workspace.preferences_dirty {
            let preferences = self.git_panel.branch_workspace.preferences.clone();
            self.git_panel.branch_workspace.preferences_dirty = false;
            self.change_setting_now(|settings| settings.vcs.branches = preferences.clone());
        }
        self.git_panel
            .branch_workspace
            .set_preferences(&self.settings.vcs.branches);
        self.git_panel.branch_workspace.roots =
            self.vcs_workspace.mappings.roots.iter().cloned().collect();
        if let Some(target) = self.git_panel.blocked_checkout.take() {
            self.git_panel.branch_workspace.blocked = Some(target);
        }
    }
    pub(super) fn branch_workspace_docked_ui(&mut self, ui: &mut egui::Ui, theme: &UiTheme) {
        self.prepare_branch_workspace();
        self.git_panel.branch_workspace.menu_available = self.component_menu_owner_available(false);
        let events = if let Some(repo) = self
            .project
            .as_ref()
            .and_then(|project| project.git.as_ref())
        {
            self.git_panel
                .branch_workspace
                .ui(ui, theme, repo, ui.input(|input| input.time))
        } else {
            ui.label("No Git repository selected");
            Vec::new()
        };
        for event in events {
            self.handle_branch_workspace_event(event);
        }
        self.prepare_branch_workspace();
    }
    pub(super) fn branch_workspace_dialog(&mut self, ctx: &egui::Context) {
        if !self.component_menu_owner_available(true) {
            return;
        }
        self.prepare_branch_workspace();
        self.git_panel.branch_workspace.menu_available = true;
        let mut open = true;
        let mut events = Vec::new();
        let theme = self.theme.clone();
        egui::Window::new("Git Branches")
            .default_width(600.0)
            .collapsible(false)
            .open(&mut open)
            .show_modal(ctx, |ui| {
                if let Some(repo) = self
                    .project
                    .as_ref()
                    .and_then(|project| project.git.as_ref())
                {
                    events = self.git_panel.branch_workspace.ui(
                        ui,
                        &theme,
                        repo,
                        ui.input(|input| input.time),
                    );
                } else {
                    ui.label("No Git repository selected");
                }
            });
        // Preserve the popup until a command explicitly chooses another dialog.
        if open {
            self.dialog = Dialog::Branches;
        }
        for event in events {
            self.handle_branch_workspace_event(event);
        }
        self.prepare_branch_workspace();
    }
    pub(super) fn handle_branch_workspace_event(&mut self, event: Event) {
        if !self.vcs_workspace.mappings.roots.contains(&event.root)
            && self
                .project
                .as_ref()
                .and_then(|p| p.git.as_ref())
                .is_none_or(|git| git.root() != event.root.as_path())
        {
            self.set_status("The selected repository is no longer mapped");
            return;
        }
        let root = event.root;
        if matches!(
            &event.command,
            Command::Checkout(_)
                | Command::CheckoutRemote(_)
                | Command::SmartCheckout(_)
                | Command::Rebase(_)
                | Command::Pull(_, _)
                | Command::Remotes
        ) && self
            .project
            .as_ref()
            .and_then(|project| project.git.as_ref())
            .is_none_or(|repo| repo.root() != root.as_path())
        {
            self.set_status("Repository selection changed; select the branch again");
            return;
        }
        match event.command {
            Command::SelectRoot(selected) => {
                if !self.vcs_workspace.mappings.roots.contains(&selected) {
                    return;
                }
                self.vcs_workspace.mappings.selected = Some(selected.clone());
                self.vcs_workspace.mappings.all_roots = false;
                if let Some(project) = &mut self.project {
                    let _ = self.vcs_workspace.mappings.save(&project.root);
                    project.git = GitRepo::discover(&selected);
                    project.git_branch = project.git.as_ref().and_then(GitRepo::current_branch);
                    project.git_refreshed = -100.0;
                }
                self.git_panel.branch_workspace.invalidate();
                self.git_panel.last_refresh = -100.0;
            }
            Command::Checkout(name) => {
                self.run_branch_op(git_workflows::BranchOp::Checkout(name));
                self.remember_branch_workspace_current(&root);
            }
            Command::CheckoutRemote(name) => {
                self.run_branch_op(git_workflows::BranchOp::CheckoutRemote(name));
                self.remember_branch_workspace_current(&root);
            }
            Command::SmartCheckout(name) => {
                self.run_branch_op(git_workflows::BranchOp::SmartCheckout(name));
            }
            Command::Rebase(name) => {
                self.run_branch_op(git_workflows::BranchOp::Rebase(name));
            }
            Command::Pull(remote_ref, rebase) => {
                self.run_branch_op(git_workflows::BranchOp::Pull {
                    remote_ref,
                    method: if rebase {
                        rustpy_vcs::UpdateMethod::Rebase
                    } else {
                        rustpy_vcs::UpdateMethod::Merge
                    },
                });
            }
            Command::UpdateBranch(name) => {
                let Some(repo) = GitRepo::discover(&root) else {
                    return;
                };
                if repo.current_branch().as_deref() == Some(name.as_str()) {
                    self.update_project();
                } else if let Some(upstream) = repo
                    .branches()
                    .iter()
                    .find(|branch| branch.name == name)
                    .and_then(|branch| branch.upstream.clone())
                {
                    if let Some((remote, branch)) = repo.split_remote_ref(&upstream) {
                        self.start_git_op(crate::git_jobs::GitOp::BranchNetwork {
                            root,
                            label: format!("Update '{name}'"),
                            args: vec![
                                "fetch".into(),
                                "--progress".into(),
                                "--".into(),
                                remote,
                                format!("refs/heads/{branch}:refs/heads/{name}"),
                            ],
                        });
                    }
                }
            }
            Command::Local(action) => self.start_branch_local(root, action),
            Command::Compare(selected) => {
                if let Some(repo) = GitRepo::discover(&root) {
                    match repo.compare_branch_commits(&selected, 2000) {
                        Ok(comparison) => {
                            self.git_panel.branch_workspace.comparison = Some(comparison);
                            self.git_panel.tab = crate::git_panel::GitTab::Log;
                            self.bottom = Some(BottomTool::Git);
                            self.dialog = Dialog::None;
                        }
                        Err(error) => self.set_status(error.to_string()),
                    }
                }
            }
            Command::CommitDiff(selected) => {
                if let Some(repo) = GitRepo::discover(&root) {
                    match repo.compare_with_branch(&selected) {
                        Ok(diffs) => {
                            self.dialog = Dialog::CompareBranch {
                                branch: format!("{selected} ↔ HEAD"),
                                diffs,
                                selected: 0,
                            }
                        }
                        Err(error) => self.set_status(error.to_string()),
                    }
                }
            }
            Command::WorkingDiff(selected) => {
                if let Some(repo) = GitRepo::discover(&root) {
                    match repo.branch_working_tree_diff(&selected) {
                        Ok(diffs) => {
                            self.dialog = Dialog::CompareBranch {
                                branch: format!("{selected} ↔ working tree"),
                                diffs,
                                selected: 0,
                            }
                        }
                        Err(error) => self.set_status(error.to_string()),
                    }
                }
            }
            Command::Network { label, args } => {
                self.start_git_op(crate::git_jobs::GitOp::BranchNetwork { root, label, args })
            }
            Command::Fetch => self.start_git_op(crate::git_jobs::GitOp::BranchNetwork {
                root,
                label: "Fetch all remotes / prune".into(),
                args: vec![
                    "fetch".into(),
                    "--all".into(),
                    "--prune".into(),
                    "--progress".into(),
                ],
            }),
            Command::Remotes => self.open_remotes_dialog(),
            Command::CreatePullRequest(branch) => self.github_new_pull(Some(branch)),
            Command::ShowWindow => {
                self.bottom = Some(BottomTool::Branches);
                self.dialog = Dialog::None;
            }
            Command::ShowCommit(commit) => {
                self.git_panel.tab = crate::git_panel::GitTab::Log;
                self.git_panel.selected_commit = Some(commit);
                self.bottom = Some(BottomTool::Git);
            }
        }
        self.git_panel.branch_workspace.invalidate();
    }
    fn start_branch_local(&mut self, root: PathBuf, action: LocalAction) {
        if !self.builtin_plugin_enabled("git") {
            self.set_status("Git bundled capability is disabled in Plugins");
            return;
        }
        if self.project_safe_mode() {
            self.notify_safe_mode("Version control commands and hooks");
            return;
        }
        rustpy_vcs::set_git_program(&self.settings.vcs.git_executable);
        let reservation = match rustpy_vcs::RepositoryReservation::try_acquire(
            &root,
            "Branch workspace operation",
        ) {
            Ok(value) => value,
            Err(error) => {
                self.set_status(error.to_string());
                return;
            }
        };
        if action.changes_files() && !reservation.run(|| self.save_all()) {
            return;
        }
        let token = rustpy_core::CancellationToken::new();
        let worker_token = token.clone();
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let completion = reservation.run(|| {
                let mut restore = None;
                let mut recent = None;
                let mut blocked = None;
                let result = GitRepo::discover(&root)
                    .ok_or_else(|| "Repository is no longer available".to_string())
                    .and_then(|repo| {
                        if action.changes_files() {
                            crate::git_panel::snapshot_working_tree(
                                &repo,
                                &root,
                                "Before branch workspace operation",
                                true,
                            );
                        }
                        if let LocalAction::Delete { branch, .. } = &action {
                            restore = repo
                                .branch_tip(branch)
                                .ok()
                                .map(|oid| (branch.clone(), oid));
                        }
                        match &action {
                            LocalAction::Create {
                                name,
                                checkout: true,
                                ..
                            }
                            | LocalAction::CheckoutRevision(name)
                            | LocalAction::CheckoutRebase(name)
                            | LocalAction::ForceCheckout(name)
                            | LocalAction::ShelfCheckout(name) => recent = Some(name.clone()),
                            _ => {}
                        }
                        let result = repo
                            .branch_local_action_cancellable(&action, &worker_token)
                            .map_err(|error| {
                                if GitRepo::is_checkout_conflict(&error) {
                                    blocked = recent.clone();
                                }
                                format!("{error:#}")
                            });
                        if result.is_err() {
                            restore = None;
                            recent = None;
                        }
                        result
                    });
                Completion {
                    root: root.clone(),
                    result,
                    restore,
                    recent,
                    blocked,
                }
            });
            let _ = tx.send(completion);
        });
        self.git_panel.branch_workspace.pending.push(
            crate::git_panel::branch_workspace::BranchJob {
                receiver: rx,
                token,
                id,
            },
        );
        self.set_status("Running branch operation…");
    }
    fn remember_branch_workspace_current(&mut self, root: &Path) {
        if let Some(branch) = GitRepo::discover(root).and_then(|repo| repo.current_branch()) {
            let key = root.display().to_string();
            self.change_setting_now(|settings| settings.vcs.branches.remember(&key, &branch));
        }
    }
    pub(super) fn poll_branch_workspace_jobs(&mut self, ctx: &egui::Context) {
        if !self.git_panel.branch_workspace.pending.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let mut completed = Vec::new();
        self.git_panel
            .branch_workspace
            .pending
            .retain(|job| match job.receiver.try_recv() {
                Ok(completion) => {
                    if !job.token.is_cancelled() {
                        completed.push(completion);
                    }
                    false
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => true,
                Err(_) => false,
            });
        for completion in completed {
            let same = self
                .project
                .as_ref()
                .and_then(|project| project.git.as_ref())
                .is_some_and(|repo| repo.root() == completion.root.as_path());
            match completion.result {
                Ok(message) => {
                    if let Some(recent) = completion.recent {
                        let key = completion.root.display().to_string();
                        self.change_setting_now(|settings| {
                            settings.vcs.branches.remember(&key, &recent)
                        });
                    }
                    let mut actions = vec![(
                        "Branches".into(),
                        crate::notifications::NotificationAction::RunAction(Action::GitBranches),
                    )];
                    if let Some((name, oid)) = &completion.restore {
                        actions.insert(
                            0,
                            (
                                "Restore branch".into(),
                                crate::notifications::NotificationAction::RestoreBranch {
                                    root: completion.root.clone(),
                                    name: name.clone(),
                                    oid: oid.clone(),
                                },
                            ),
                        );
                    }
                    if same {
                        self.git_panel.branch_workspace.error = None;
                        self.git_panel.branch_workspace.blocked = None;
                        self.git_panel.blocked_checkout = None;
                        self.git_panel.branch_workspace.restore = completion.restore;
                        self.reload_changed_files();
                        self.reload_analysis_from_disk();
                    }
                    self.notify(
                        crate::notifications::NotificationKind::Info,
                        "Git",
                        "Branch operation",
                        message.clone(),
                        actions,
                    );
                    self.set_status(message);
                }
                Err(error) => {
                    if same {
                        self.reload_changed_files();
                        self.reload_analysis_from_disk();
                        self.git_panel.branch_workspace.blocked = completion.blocked;
                        self.git_panel.branch_workspace.error = Some(error.clone());
                    }
                    self.set_status(error);
                    if same
                        && GitRepo::discover(&completion.root)
                            .is_some_and(|repo| !repo.conflicts().unwrap_or_default().is_empty())
                    {
                        self.bottom = Some(BottomTool::Git);
                        self.git_panel.tab = crate::git_panel::GitTab::LocalChanges;
                    }
                }
            }
            self.git_panel.branch_workspace.invalidate();
            self.git_panel.last_refresh = -100.0;
            if let Some(project) = &mut self.project {
                project.git_refreshed = -100.0;
            }
        }
    }
}
