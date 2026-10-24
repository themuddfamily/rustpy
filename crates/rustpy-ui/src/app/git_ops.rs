//! Network Git operations off the UI thread (roadmap §4.1 Fetch, Pull,
//! Update Project, Push): starting them, and reacting when they end. The
//! jobs themselves, the Git console and the ask-pass dialog live in
//! [`crate::git_jobs`].

use super::*;
use crate::git_jobs::{FinishedJob, GitOp};

impl IdeApp {
    /// Starts Fetch or Pull (Update Project) in the background.
    pub(super) fn start_git_op(&mut self, op: GitOp) {
        if !self.builtin_plugin_enabled("git") {
            self.set_status("Git bundled capability is disabled in Plugins");
            return;
        }
        if self.project_safe_mode() {
            self.notify_safe_mode("Git network operations and hooks");
            return;
        }
        rustpy_vcs::set_git_program(&self.settings.vcs.git_executable);
        let Some((root, project_root)) = self.project.as_ref().and_then(|p| {
            p.git
                .as_ref()
                .map(|g| (g.root().to_path_buf(), p.root.clone()))
        }) else {
            self.set_status("Not a Git repository");
            return;
        };
        let root = if let GitOp::BranchNetwork { root, .. } = &op {
            root.clone()
        } else {
            root
        };
        if let Some(running) = rustpy_vcs::running_operation(&root) {
            self.set_status(format!(
                "{running} is running: wait for it to finish before starting another Git operation"
            ));
            return;
        }
        let mut args: Vec<String> = match &op {
            GitOp::BranchNetwork { args, .. } => args.clone(),
            GitOp::Fetch => vec![
                "fetch".into(),
                "--all".into(),
                "--prune".into(),
                "--progress".into(),
            ],
            GitOp::Pull => {
                let mut args = vec![
                    "pull".into(),
                    if self.settings.vcs.update_method == "rebase" {
                        "--rebase".into()
                    } else {
                        "--no-rebase".into()
                    },
                    "--progress".into(),
                ];
                if self.settings.vcs.update_autostash {
                    args.push("--autostash".into());
                }
                args
            }
            GitOp::Push { .. } => return,
        };
        let depth = self.settings.vcs.preferences.shallow_depth;
        if depth > 0 && op == GitOp::Fetch {
            args.push(format!("--depth={depth}"));
        } else if op == GitOp::Fetch
            && self
                .project
                .as_ref()
                .and_then(|p| p.git.as_ref())
                .is_some_and(|g| g.is_shallow())
        {
            args.push("--unshallow".into());
        }
        if op == GitOp::Fetch && self.vcs_workspace.mappings.all_roots {
            self.vcs_workspace.fetch_queue = self
                .vcs_workspace
                .mappings
                .operation_roots()
                .into_iter()
                .filter(|r| r != &root)
                .collect();
        }
        if op == GitOp::Pull {
            // Update Project: editors are saved and the working
            // tree is copied to Local History before files change.
            self.save_for_vcs();
            if let Some(g) = self.project.as_ref().and_then(|p| p.git.as_ref()) {
                crate::git_panel::snapshot_working_tree(g, &project_root, "Before update", false);
            }
        }
        if !self.prepare_operation_cli(&op, &root) {
            self.git_panel.operation_failed();
            return;
        }
        let status = format!("{}…", op.title());
        match self.git_panel.git_jobs.start(op, &root, args) {
            Ok(()) => self.set_status(status),
            Err(e) => {
                self.git_panel.operation_failed();
                self.set_status(e);
            }
        }
    }

    /// Every frame: applies the Git executable setting, collects the
    /// running job's output, and shows the ask-pass dialog.
    pub(super) fn poll_git_console_jobs(&mut self, ctx: &egui::Context) {
        for event in self.git_panel.poll_log_job() {
            match event {
                GitEvent::Status(message) => self.report_git_status(message),
                GitEvent::FilesChanged => {
                    self.reload_changed_files();
                    self.reload_analysis_from_disk();
                    self.git_panel.last_refresh = -100.0;
                    if let Some(project) = &mut self.project {
                        project.git_refreshed = -100.0;
                    }
                }
                _ => {}
            }
        }
        if self.git_panel.log_job_label().is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        rustpy_vcs::set_git_program(&self.settings.vcs.git_executable);
        for notice in self.git_panel.git_jobs.poll_prompts() {
            self.set_status(notice);
        }
        if let Some(done) = self.git_panel.git_jobs.poll() {
            self.git_job_finished(done);
        }
        if self.git_panel.git_jobs.is_busy() || self.git_panel.git_jobs.has_prompt() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.poll_operation_queue();
        let theme = self.theme.clone();
        self.git_panel.git_jobs.prompt_window(ctx, &theme);
    }

    fn git_job_finished(&mut self, done: FinishedJob) {
        if done.op == GitOp::Fetch {
            if done.stopped {
                self.vcs_workspace.fetch_queue.clear();
            } else if let Some(root) = self.vcs_workspace.fetch_queue.pop_front() {
                let mut args = vec![
                    "fetch".into(),
                    "--all".into(),
                    "--prune".into(),
                    "--progress".into(),
                ];
                let depth = self.settings.vcs.preferences.shallow_depth;
                if depth > 0 {
                    args.push(format!("--depth={depth}"));
                } else if GitRepo::discover(&root).is_some_and(|repo| repo.is_shallow()) {
                    args.push("--unshallow".into());
                }
                if let Err(error) = self.git_panel.git_jobs.start(GitOp::Fetch, &root, args) {
                    self.set_status(error);
                }
            }
        }
        if let GitOp::Push { request, commits } = &done.op {
            if !done.success
                && !done.stopped
                && !request.force_with_lease
                && self.settings.vcs.preferences.auto_update_rejected_push
                && !self.vcs_workspace.push_retried
                && (done.output.contains("non-fast-forward") || done.output.contains("fetch first"))
                && self
                    .project
                    .as_ref()
                    .and_then(|p| p.git.as_ref())
                    .is_some_and(|g| g.root() == done.root)
            {
                let root = done.root.clone();
                self.vcs_workspace.retry_push = Some((request.clone(), *commits, root));
                self.vcs_workspace.push_retried = true;
                self.start_git_op(GitOp::Pull);
                return;
            }
            if done.success {
                self.vcs_workspace.push_retried = false;
            }
        }
        if done.op == GitOp::Pull {
            if let Some((request, commits, root)) = self.vcs_workspace.retry_push.take() {
                if done.success && !done.stopped {
                    let args = request.args();
                    let result = self.git_panel.git_jobs.start(
                        GitOp::Push { request, commits },
                        &root,
                        args,
                    );
                    self.reload_changed_files();
                    self.set_status(match result {
                        Ok(()) => "Updated; retrying push once…".into(),
                        Err(e) => e,
                    });
                    return;
                }
            }
        }
        self.finish_operation_cli(&done);
        self.github_git_job_finished(&done);
        // The whole output is in the Git tool window › Console.
        let failure = |what: &str| {
            format!(
                "{what} failed: {}",
                rustpy_vcs::describe_failure(&done.summary)
            )
        };
        if done.op == GitOp::Pull {
            // Even a failed or cancelled pull may have changed files.
            self.reload_changed_files();
            self.reload_analysis_from_disk();
        }
        let msg = match &done.op {
            _ if done.stopped => format!("{} cancelled", done.op.title()),
            GitOp::Fetch if done.success => "Fetched from all remotes".to_string(),
            GitOp::Fetch => failure("Fetch"),
            GitOp::Pull if done.success => {
                let last = done
                    .output
                    .lines()
                    .rev()
                    .map(str::trim)
                    .find(|l| !l.is_empty() && !l.starts_with("From "))
                    .unwrap_or("done");
                format!("Project updated: {last}")
            }
            GitOp::Pull => failure("Update"),
            GitOp::BranchNetwork { label, .. } => {
                if done.success {
                    format!("{label}: done")
                } else {
                    failure(label)
                }
            }
            GitOp::Push { request, commits } => {
                rustpy_vcs::push_result_message(request, done.success, &done.output, *commits)
            }
        };
        if let Some(p) = &mut self.project {
            p.git_refreshed = -100.0;
        }
        self.git_panel.last_refresh = -100.0;
        self.set_status(msg);
    }
}
