use super::*;
use crate::git_jobs::{FinishedJob, GitOp};
use crate::notifications::{NotificationAction, NotificationKind};
use rustpy_settings::vcs_operations::FilePolicy;

pub(super) fn format_blame_annotations(
    repo: &GitRepo,
    lines: Vec<rustpy_vcs::BlameLine>,
    settings: &rustpy_settings::vcs_operations::Settings,
) -> Vec<(String, u8, String)> {
    let mut metadata = std::collections::HashMap::new();
    let mut result = Vec::new();
    for line in lines {
        while result.len() + 1 < line.line {
            result.push((String::new(), 0, String::new()));
        }
        if line.is_uncommitted {
            result.push(("(uncommitted)".into(), 0, String::new()));
            continue;
        }
        let (id, message) = metadata.entry(line.commit.clone()).or_insert_with(|| {
            (
                repo.resolve_log_revision(&line.commit)
                    .unwrap_or_else(|_| line.commit.clone()),
                repo.operation_revision_message(&line.commit)
                    .unwrap_or_else(|_| line.summary.clone()),
            )
        });
        let date = chrono::DateTime::from_timestamp(line.time, 0)
            .map(|date| date.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        let mut text = Vec::new();
        if settings.blame_lines {
            text.push(line.line.to_string());
        }
        if settings.blame_revision {
            text.push(id.chars().take(8).collect());
        }
        if settings.blame_date {
            text.push(date);
        }
        if settings.blame_author {
            text.push(if settings.blame_short_names {
                line.author.split_whitespace().next().unwrap_or("").into()
            } else {
                line.author.clone()
            });
        }
        let band = if settings.blame_age {
            let days = (chrono::Utc::now().timestamp() - line.time).max(0) / 86400;
            match days {
                0..=6 => 0,
                7..=29 => 1,
                30..=89 => 2,
                90..=179 => 3,
                180..=364 => 4,
                _ => 5,
            }
        } else {
            line.author
                .bytes()
                .fold(0u8, |hash, byte| hash.wrapping_add(byte))
                % 6
        };
        result.push((
            text.join("  "),
            band,
            format!("{id} {}\n{message}", line.author),
        ));
    }
    result
}

fn changes_worktree(args: &[String]) -> bool {
    args.first().is_some_and(|arg| {
        matches!(
            arg.as_str(),
            "add"
                | "pull"
                | "merge"
                | "rebase"
                | "cherry-pick"
                | "revert"
                | "reset"
                | "checkout"
                | "restore"
                | "rm"
                | "stash"
                | "submodule"
                | "mergetool"
                | "-c"
        )
    })
}
impl IdeApp {
    pub(super) fn show_operation_history(&mut self, path: PathBuf) {
        let Some(root) = self.vcs_workspace.mappings.root_for(&path).cloned() else {
            self.set_status("This path has no Git mapping");
            return;
        };
        let selection = self.macro_context(Some(&path)).selection_lines;
        self.git_panel.open_revision_history(
            root,
            path,
            selection,
            self.settings.vcs_operations.clone(),
        );
        self.bottom = Some(BottomTool::Git);
    }
    pub(super) fn invoke_operation_action(
        &mut self,
        root: PathBuf,
        action: Action,
        ctx: &egui::Context,
    ) {
        if action != Action::GetFromVcs {
            if !self.vcs_workspace.mappings.roots.contains(&root) {
                self.set_status("Map this repository in Version Control settings before opening its project actions");
                return;
            }
            self.vcs_workspace.mappings.selected = Some(root.clone());
            self.vcs_workspace.mappings.all_roots = false;
            if let Some(project) = &mut self.project {
                let _ = self.vcs_workspace.mappings.save(&project.root);
                project.git = GitRepo::discover(&root);
                project.git_branch = project.git.as_ref().and_then(GitRepo::current_branch);
                project.git_refreshed = -100.0;
            }
            self.git_panel.last_refresh = -100.0;
        }
        self.handle_action(action, ctx);
    }
    pub(super) fn queue_update_project(&mut self, rebase: bool, autostash: bool) {
        let roots = if self.settings.vcs_operations.update_all_roots
            || self.vcs_workspace.mappings.all_roots
        {
            self.vcs_workspace
                .mappings
                .roots
                .iter()
                .cloned()
                .collect::<Vec<_>>()
        } else {
            self.project
                .as_ref()
                .and_then(|p| p.git.as_ref())
                .map(|repo| vec![repo.root().to_path_buf()])
                .unwrap_or_default()
        };
        for root in roots {
            let mode = if self.settings.vcs_operations.update_branch_default {
                GitRepo::discover(&root).is_some_and(|repo| repo.operation_branch_rebase_default())
            } else {
                rebase
            };
            let clean = if autostash {
                usize::from(self.settings.vcs_operations.update_cleanup.min(1))
            } else {
                2
            };
            match rustpy_vcs::operations_workspace::Request::update(
                mode,
                clean,
                self.settings.vcs_operations.update_submodules,
            ) {
                Ok(requests) => {
                    for request in requests {
                        self.git_panel
                            .operations_workspace
                            .queue
                            .push_back(GitOp::BranchNetwork {
                                root: root.clone(),
                                label: request.label,
                                args: request.args,
                            });
                    }
                }
                Err(error) => self.set_status(error.to_string()),
            }
        }
        self.set_status("Update Project queued; Git Console shows progress and Cancel");
    }
    pub(super) fn vcs_file_moved(&mut self, from: &Path, to: &Path) {
        let old = self.vcs_workspace.mappings.root_for(from).cloned();
        let new = self.vcs_workspace.mappings.root_for(to).cloned();
        if old == new {
            if let Some(root) = old {
                if let Some(repo) = GitRepo::discover(&root) {
                    if let Err(error) = repo.track_move(from, to) {
                        self.set_status(format!("Files moved; Git tracking failed: {error}"));
                    }
                }
            }
        } else {
            self.vcs_file_deleted(from);
            self.vcs_file_created(to);
        }
    }
    pub(super) fn open_blame_context(
        &mut self,
        index: usize,
        revision: String,
        intent: crate::editor::view::BlameIntent,
    ) {
        let Some(path) = self.tabs.get(index).and_then(|tab| tab.state.path.clone()) else {
            return;
        };
        let Some(repo) = GitRepo::discover(&path) else {
            return;
        };
        let root = repo.root().to_path_buf();
        if intent == crate::editor::view::BlameIntent::Log {
            self.git_panel.reveal_log_root(root, revision);
            self.bottom = Some(BottomTool::Git);
            return;
        }
        self.git_panel.open_operations(
            root,
            Some(path),
            None,
            self.settings.vcs_operations.clone(),
        );
        self.git_panel
            .open_annotation_revision(&repo, revision, intent);
        self.bottom = Some(BottomTool::Git);
    }
    pub(super) fn configure_operations_context(&mut self) {
        self.git_panel.operations_workspace.preferences = self.settings.vcs_operations.clone();
        self.git_panel.operations_workspace.roots =
            self.vcs_workspace.mappings.roots.iter().cloned().collect();
        if !self.git_panel.operations_modal_active() {
            let context = self.macro_context(None);
            self.git_panel.operations_workspace.file = context.file;
            self.git_panel.operations_workspace.selection = context.selection_lines;
        }
    }

    pub(super) fn open_git_operations(&mut self) {
        let context = self.macro_context(None);
        let root = context
            .file
            .as_deref()
            .and_then(|path| self.vcs_workspace.mappings.root_for(path).cloned())
            .or_else(|| {
                self.project
                    .as_ref()
                    .and_then(|p| p.git.as_ref())
                    .map(|g| g.root().to_path_buf())
            });
        let Some(root) = root else {
            self.set_status("Enable Git integration or initialize a repository first");
            return;
        };
        self.git_panel.operations_workspace.roots =
            self.vcs_workspace.mappings.roots.iter().cloned().collect();
        self.git_panel.open_operations(
            root,
            context.file,
            context.selection_lines,
            self.settings.vcs_operations.clone(),
        );
        self.bottom = Some(BottomTool::Git);
    }
    pub(super) fn prepare_operation_cli(&mut self, op: &GitOp, root: &Path) -> bool {
        let GitOp::BranchNetwork { args, .. } = op else {
            return true;
        };
        if let Some(lock) = GitRepo::discover(root).and_then(|repo| repo.operation_lock()) {
            self.set_status(format!(
                "Git index is locked: {}. Finish the other Git operation first.",
                lock.display()
            ));
            return false;
        }
        if !changes_worktree(args) {
            return true;
        }
        let unsaved = self.save_for_vcs();
        if !unsaved.failed.is_empty() || !unsaved.changed_on_disk.is_empty() {
            return false;
        }
        if let Some(repo) = GitRepo::discover(root) {
            if args.first().is_some_and(|arg| arg == "pull") {
                self.git_panel
                    .operations_workspace
                    .update_before
                    .insert(root.to_path_buf(), repo.code_vision_head().ok().flatten());
            }
            crate::git_panel::snapshot_working_tree(
                &repo,
                root,
                &format!("Before {}", op.title()),
                true,
            );
        }
        true
    }
    pub(super) fn poll_operation_queue(&mut self) {
        self.git_panel
            .git_jobs
            .set_show_commands(self.settings.vcs_operations.show_commands);
        if let Some(op) = self.git_panel.poll_operations_queue() {
            self.start_git_op(op);
        }
    }
    pub(super) fn finish_operation_cli(&mut self, done: &FinishedJob) {
        if let GitOp::Push { request, commits } = &done.op {
            if done.success {
                self.git_panel.operations_workspace.auto_retries = 0;
                self.notify(
                    NotificationKind::Info,
                    "Git",
                    "Push complete",
                    rustpy_vcs::push_result_message(request, true, &done.output, *commits),
                    vec![(
                        "View commits".into(),
                        NotificationAction::GitLog {
                            root: done.root.clone(),
                            revision: request.local_branch.clone(),
                        },
                    )],
                );
            } else if !done.stopped
                && (done.output.contains("non-fast-forward")
                    || done.output.contains("fetch first")
                    || done.output.contains("[rejected]"))
                && self.vcs_workspace.mappings.roots.contains(&done.root)
            {
                self.git_panel.open_operations(
                    done.root.clone(),
                    None,
                    None,
                    self.settings.vcs_operations.clone(),
                );
                self.git_panel.operations_workspace.rejected_push =
                    Some((done.root.clone(), request.clone(), *commits));
                self.bottom = Some(BottomTool::Git);
                if self.settings.vcs_operations.update_after_rejected_push
                    && self.git_panel.operations_workspace.auto_retries == 0
                {
                    self.git_panel.operations_workspace.auto_retries = 1;
                    let update = rustpy_vcs::operations_workspace::Pull {
                        remote: request.remote.clone(),
                        branch: request.remote_branch.clone(),
                        rebase: self.settings.vcs.update_method == "rebase",
                        autostash: true,
                        ..Default::default()
                    };
                    if let Ok(update) = rustpy_vcs::operations_workspace::Request::pull(&update) {
                        self.git_panel
                            .operations_workspace
                            .queue
                            .push_back(GitOp::BranchNetwork {
                                root: done.root.clone(),
                                label: "Update before retrying push".into(),
                                args: update.args,
                            });
                        self.git_panel.operations_workspace.retry_after_update = true;
                    }
                }
            }
            return;
        }
        let GitOp::BranchNetwork { root, args, .. } = &done.op else {
            return;
        };
        if let Some(before) = self
            .git_panel
            .operations_workspace
            .update_before
            .remove(root)
        {
            if done.success {
                if let Some(repo) = GitRepo::discover(root) {
                    let info = super::git_workflows::update_information(
                        &repo,
                        before.as_deref(),
                        &self.settings.vcs.preferences.update_path_filter,
                        &done.summary,
                    );
                    self.notify(
                        NotificationKind::Info,
                        "Git",
                        "Update Project",
                        info,
                        vec![(
                            "View updated commits".into(),
                            NotificationAction::GitLog {
                                root: root.clone(),
                                revision: "HEAD".into(),
                            },
                        )],
                    );
                }
            }
        }
        if !done.success {
            if let Some(operation) =
                GitRepo::discover(root).and_then(|repo| repo.current_operation())
            {
                let mut actions = vec![(
                    "Resolve conflicts".into(),
                    NotificationAction::ShowToolWindow("Git"),
                )];
                for action in ["continue", "skip", "abort"] {
                    if operation != "merge" || action != "skip" {
                        actions.push((
                            action.into(),
                            NotificationAction::GitContinuation {
                                root: root.clone(),
                                operation: operation.clone(),
                                action: action.into(),
                            },
                        ));
                    }
                }
                self.notify(
                    NotificationKind::Warning,
                    "Git",
                    format!("{} stopped", done.op.title()),
                    done.summary.clone(),
                    actions,
                );
            }
            self.git_panel.operation_failed();
            self.git_panel.operations_workspace.retry_after_update = false;
        }
        if changes_worktree(args) && self.vcs_workspace.mappings.roots.contains(root) {
            self.reload_changed_files();
            self.reload_analysis_from_disk();
            if let Some(project) = &mut self.project {
                project.git_refreshed = -100.0;
            }
            self.git_panel.last_refresh = -100.0;
        }
        if done.success {
            self.notify(
                NotificationKind::Info,
                "Git",
                done.op.title(),
                format!("{} completed in {}", done.op.title(), root.display()),
                vec![(
                    "View commits".into(),
                    NotificationAction::GitLog {
                        root: root.clone(),
                        revision: "HEAD".into(),
                    },
                )],
            );
            if matches!(&done.op,GitOp::BranchNetwork{label,..} if label=="Update before retrying push")
                && self.vcs_workspace.mappings.roots.contains(root)
            {
                if let Some((root, request, commits)) =
                    self.git_panel.operations_workspace.rejected_push.take()
                {
                    self.git_panel.operations_workspace.retry_after_update = false;
                    self.start_push_at_root(
                        root,
                        request,
                        commits,
                        self.settings.vcs_operations.push_hooks,
                        self.settings.vcs_operations.push_force_with_lease,
                    );
                }
            }
        }
    }
    /// File creation/deletion policies run after the filesystem mutation. Paths
    /// and roots stay captured in the request even if the active editor changes.
    pub(super) fn vcs_file_created(&mut self, path: &Path) {
        self.vcs_file_policy(path, false);
    }
    pub(super) fn vcs_file_deleted(&mut self, path: &Path) {
        self.vcs_file_policy(path, true);
    }
    fn vcs_file_policy(&mut self, path: &Path, removed: bool) {
        let policy = if removed {
            self.settings.vcs_operations.removed
        } else {
            self.settings.vcs_operations.created
        };
        let decision = match policy {
            FilePolicy::Add => Some(true),
            FilePolicy::Ask => None,
            FilePolicy::DoNotAdd => Some(false),
        };
        if self.queue_other_vcs_file_change(path, removed, decision) {
            return;
        }
        let Some(root) = self.vcs_workspace.mappings.root_for(path).cloned() else {
            return;
        };
        let Some(repo) = GitRepo::discover(&root) else {
            return;
        };
        if policy == FilePolicy::DoNotAdd {
            return;
        }
        if !removed && repo.is_ignored(path) {
            return;
        }
        let Ok(relative) = repo.operation_relative(path) else {
            return;
        };
        let mut args = vec!["add".into()];
        if removed {
            args.push("--update".into());
        }
        args.extend(["--".into(), relative.to_string_lossy().into_owned()]);
        if policy == FilePolicy::Add {
            self.git_panel
                .operations_workspace
                .queue
                .push_back(GitOp::BranchNetwork {
                    root,
                    label: if removed {
                        "Stage removed file".into()
                    } else {
                        "Add created file".into()
                    },
                    args,
                });
        } else {
            self.git_panel.open_operations(
                root,
                Some(path.to_path_buf()),
                None,
                self.settings.vcs_operations.clone(),
            );
            self.git_panel
                .prepare_file_policy(path.to_path_buf(), args, removed);
            self.bottom = Some(BottomTool::Git);
        }
    }
}
