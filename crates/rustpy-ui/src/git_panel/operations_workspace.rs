use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use crate::actions::Action;
use rustpy_settings::vcs_operations::{FilePolicy, Settings};
use rustpy_vcs::operations_workspace::{Merge, Pull, Rebase, Request, Reset};
use std::collections::VecDeque;

pub struct Workspace {
    pub update_before: std::collections::BTreeMap<PathBuf, Option<String>>,
    inspection: Option<rustpy_vcs::operation_inspection::Inspection>,
    pub auto_retries: u8,
    pub rejected_push: Option<(PathBuf, rustpy_vcs::PushRequest, Option<usize>)>,
    pub retry_after_update: bool,
    pub open: bool,
    pub root: PathBuf,
    pub roots: Vec<PathBuf>,
    pub file: Option<PathBuf>,
    pub selection: Option<(usize, usize)>,
    pub preferences: Settings,
    pub queue: VecDeque<crate::git_jobs::GitOp>,
    patch_path: String,
    mode: usize,
    pull: Pull,
    merge: Merge,
    rebase: Rebase,
    revision: String,
    other_revision: String,
    path: String,
    suffix: bool,
    no_commit: bool,
    mainline: u32,
    reset: Reset,
    stash_message: String,
    stash_untracked: bool,
    stash_keep_index: bool,
    stash_branch: String,
    pending: Option<Request>,
    error: String,
    preview: String,
    history: Vec<rustpy_vcs::operations_workspace::HistoryRevision>,
    selected: Option<String>,
    follow: bool,
    all_branches: bool,
    blame: Vec<rustpy_vcs::BlameLine>,
    ignore_whitespace: bool,
    blame_author: bool,
    blame_date: bool,
    blame_revision: bool,
    blame_lines: bool,
    blame_age: bool,
    short_names: bool,
    browse_path: PathBuf,
    browse: Vec<rustpy_vcs::operations_workspace::RevisionEntry>,
    compare_files: Vec<rustpy_vcs::FileDiff>,
    clean_strategy: usize,
    update_rebase: bool,
    repo_input: String,
}
impl Default for Workspace {
    fn default() -> Self {
        Self {
            update_before: Default::default(),
            inspection: None,
            auto_retries: 0,
            rejected_push: None,
            retry_after_update: false,
            open: false,
            root: PathBuf::new(),
            roots: Vec::new(),
            file: None,
            selection: None,
            preferences: Settings::default(),
            queue: VecDeque::new(),
            patch_path: String::new(),
            mode: 0,
            pull: Pull::default(),
            merge: Merge::default(),
            rebase: Rebase::default(),
            revision: "HEAD".into(),
            other_revision: "HEAD~1".into(),
            path: String::new(),
            suffix: true,
            no_commit: false,
            mainline: 0,
            reset: Reset::Mixed,
            stash_message: String::new(),
            stash_untracked: true,
            stash_keep_index: false,
            stash_branch: String::new(),
            pending: None,
            error: String::new(),
            preview: String::new(),
            history: Vec::new(),
            selected: None,
            follow: true,
            all_branches: false,
            blame: Vec::new(),
            ignore_whitespace: false,
            blame_author: true,
            blame_date: true,
            blame_revision: true,
            blame_lines: true,
            blame_age: true,
            short_names: false,
            browse_path: PathBuf::new(),
            browse: Vec::new(),
            compare_files: Vec::new(),
            clean_strategy: 0,
            update_rebase: false,
            repo_input: String::new(),
        }
    }
}
impl Workspace {
    fn inspect(&mut self, request: rustpy_vcs::operation_inspection::Request) {
        self.inspection = Some(rustpy_vcs::operation_inspection::Inspection::start(
            self.root.clone(),
            request,
        ));
    }

    fn request(&mut self, result: anyhow::Result<Request>) {
        match result {
            Ok(request) => self.pending = Some(request),
            Err(error) => self.error = error.to_string(),
        }
    }
    fn selected_path(&self) -> PathBuf {
        if self.path.is_empty() {
            self.file.clone().unwrap_or_else(|| self.root.clone())
        } else {
            let path = PathBuf::from(&self.path);
            if path.is_absolute() {
                path
            } else {
                self.root.join(path)
            }
        }
    }
    fn enqueue(&mut self, request: Request) {
        self.queue.push_back(crate::git_jobs::GitOp::BranchNetwork {
            root: self.root.clone(),
            label: request.label,
            args: request.args,
        });
    }
    fn ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        repo: &GitRepo,
        events: &mut Vec<GitEvent>,
    ) {
        if let Some(inspection) = &self.inspection {
            if let Some(result) = inspection.poll() {
                match result {
                    Ok(result) => match result {
                        rustpy_vcs::operation_inspection::Result::History(rows) => {
                            self.history = rows
                        }
                        rustpy_vcs::operation_inspection::Result::Blame(rows) => self.blame = rows,
                        rustpy_vcs::operation_inspection::Result::Compare(files) => {
                            self.compare_files = files
                        }
                        rustpy_vcs::operation_inspection::Result::Browse(entries) => {
                            self.browse = entries
                        }
                    },
                    Err(error) => self.error = error,
                }
                self.inspection = None;
            } else {
                let cancel = ui
                    .horizontal(|ui| {
                        crate::a11y::spinner(ui);
                        ui.label(format!(
                            "Loading revision data… {} commits",
                            inspection
                                .progress
                                .load(std::sync::atomic::Ordering::Relaxed)
                        ));
                        ui.button("Cancel inspection").clicked()
                    })
                    .inner;
                if cancel {
                    self.inspection = None;
                }
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(50));
            }
        }
        let original_preferences = self.preferences.clone();
        if let Some((root, request, _)) = self.rejected_push.clone() {
            if !self.retry_after_update {
                ui.colored_label(
                    theme.warning,
                    format!(
                        "Push from {} to {} was rejected because the remote branch advanced.",
                        root.display(),
                        request.target_label()
                    ),
                );
                ui.horizontal(|ui| {
                    for (label, rebase) in [("Merge and retry", false), ("Rebase and retry", true)]
                    {
                        if ui.button(crate::i18n::tr_static(label)).clicked() {
                            match Request::pull(&Pull {
                                remote: request.remote.clone(),
                                branch: request.remote_branch.clone(),
                                rebase,
                                autostash: true,
                                ..Default::default()
                            }) {
                                Ok(update) => {
                                    self.queue.push_back(crate::git_jobs::GitOp::BranchNetwork {
                                        root: root.clone(),
                                        label: "Update before retrying push".into(),
                                        args: update.args,
                                    });
                                    self.retry_after_update = true;
                                }
                                Err(e) => self.error = e.to_string(),
                            }
                        }
                    }
                    if ui.button("Cancel push recovery").clicked() {
                        self.rejected_push = None;
                        self.auto_retries = 0;
                    }
                });
            }
        }
        ui.horizontal_wrapped(|ui| {
            for (label, action) in [
                ("Branches…", Action::GitBranches),
                ("Commit…", Action::Commit),
                ("Push…", Action::Push),
                ("Fetch", Action::GitFetch),
                ("Remotes…", Action::ManageRemotes),
                ("Get from VCS…", Action::GetFromVcs),
            ] {
                if ui.button(crate::i18n::tr_static(label)).clicked() {
                    events.push(GitEvent::Invoke {
                        root: self.root.clone(),
                        action,
                    });
                    self.open = false;
                }
            }
        });
        ui.horizontal(|ui| {
            let repository_label = ui.label("Repository");
            egui::ComboBox::from_id_salt("operation-root")
                .selected_text(self.root.display().to_string())
                .show_ui(ui, |ui| {
                    for root in self.roots.clone() {
                        if ui
                            .selectable_value(
                                &mut self.root,
                                root.clone(),
                                root.display().to_string(),
                            )
                            .changed()
                        {
                            self.history.clear();
                            self.blame.clear();
                            self.browse.clear();
                            self.selected = None;
                            self.pending = None;
                            self.inspection = None;
                        }
                    }
                })
                .combo_labelled(&repository_label);
        });
        if self.root != repo.root() {
            ui.ctx().request_repaint();
            return;
        }
        if let Some(lock) = repo.operation_lock() {
            ui.colored_label(theme.error,format!("Repository is locked: {}. Wait for other Git operations; remove a stale lock only after checking that no Git process owns it.",lock.display()));
        }
        if let Some(operation) = repo.current_operation() {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(theme.warning, format!("{operation} in progress"));
                for action in ["continue", "skip", "abort"] {
                    if ui.button(crate::i18n::tr_static(action)).clicked() {
                        self.request(Request::continuation(&operation.to_lowercase(), action));
                    }
                }
            });
        }
        let modes = [
            "Update / Pull",
            "Merge",
            "Rebase",
            "Cherry-pick / Revert / Reset",
            "Stash / Unstash",
            "History",
            "Annotate",
            "Compare",
            "Browse revision",
            "Resolve conflicts",
            "Submodules / Worktrees",
            "File operations / Attributes",
            "Preferences",
        ];
        ui.horizontal_wrapped(|ui| {
            for (n, label) in modes.into_iter().enumerate() {
                if ui.selectable_label(self.mode == n, label).clicked() {
                    self.mode = n;
                }
            }
        });
        ui.separator();
        match self.mode {
            0 => self.pull_ui(ui),
            1 => self.merge_ui(ui),
            2 => self.rebase_ui(ui),
            3 => self.revision_ui(ui),
            4 => self.stash_ui(ui, repo),
            5 => self.history_ui(ui, repo, events),
            6 => self.blame_ui(ui, repo, theme, events),
            7 => self.compare_ui(ui, repo, events),
            8 => self.browse_ui(ui, repo, events),
            9 => self.conflict_ui(ui, repo, events),
            10 => self.roots_ui(ui, repo, events),
            11 => self.file_ui(ui, repo, events),
            12 => self.preferences_ui(ui, events),
            _ => {}
        }
        if let Some(request) = self.pending.clone() {
            ui.separator();
            ui.strong(&request.label);
            ui.monospace(format!("git {}", request.args.join(" ")));
            if request.destructive {
                ui.colored_label(theme.warning,"This discards or rewrites repository content. A Local History snapshot is taken before execution.");
            }
            if ui.button("Confirm operation").clicked() {
                self.enqueue(request);
                self.pending = None;
            }
            if ui.button("Cancel operation request").clicked() {
                self.pending = None;
            }
        }
        if self.preferences != original_preferences {
            events.push(GitEvent::OperationsSettings(self.preferences.clone()));
        }
        if !self.error.is_empty() {
            ui.colored_label(theme.error, &self.error);
            if ui.small_button("Dismiss error").clicked() {
                self.error.clear();
            }
        }
        if !self.preview.is_empty() {
            ui.collapsing("Diff / repository content", |ui| {
                egui::ScrollArea::both().max_height(350.0).show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.preview)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(900.0)
                            .desired_rows(18)
                            .interactive(false),
                    );
                });
            });
        }
    }
    fn pull_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Remote (empty = upstream)");
            ui.text_edit_singleline(&mut self.pull.remote);
            ui.label("Branch");
            ui.text_edit_singleline(&mut self.pull.branch);
        });
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.pull.rebase, "Pull with rebase");
            ui.checkbox(&mut self.pull.no_commit, "No commit");
            ui.checkbox(&mut self.pull.squash, "Squash");
            ui.checkbox(&mut self.pull.ff_only, "Fast-forward only");
            ui.checkbox(&mut self.pull.no_ff, "No fast-forward");
            ui.checkbox(&mut self.pull.no_verify, "Skip hooks (--no-verify)");
            ui.checkbox(&mut self.pull.autostash, "Autostash");
        });
        if ui.button("Pull…").clicked() {
            self.request(Request::pull(&self.pull));
        }
        ui.separator();
        ui.strong("Update Project");
        ui.checkbox(&mut self.update_rebase, "Rebase (otherwise merge)");
        ui.checkbox(&mut self.preferences.update_all_roots, "Update all roots");
        ui.checkbox(
            &mut self.preferences.update_submodules,
            "Initialize/update submodules after successful update",
        );
        egui::ComboBox::from_id_salt("update-clean")
            .selected_text(
                [
                    "Stash local changes",
                    "Shelve local changes",
                    "Do not clean working tree",
                ][self.clean_strategy],
            )
            .show_ui(ui, |ui| {
                for (n, label) in [
                    "Stash local changes",
                    "Shelve local changes",
                    "Do not clean working tree",
                ]
                .into_iter()
                .enumerate()
                {
                    ui.selectable_value(&mut self.clean_strategy, n, label);
                }
            })
            .combo_named(crate::i18n::tr_static("Clean working tree"));
        if ui.button("Update selected roots…").clicked() {
            let roots = if self.preferences.update_all_roots {
                self.roots.clone()
            } else {
                vec![self.root.clone()]
            };
            for root in roots {
                match Request::update(
                    self.update_rebase,
                    self.clean_strategy,
                    self.preferences.update_submodules,
                ) {
                    Ok(requests) => {
                        for request in requests {
                            self.queue.push_back(crate::git_jobs::GitOp::BranchNetwork {
                                root: root.clone(),
                                label: request.label,
                                args: request.args,
                            });
                        }
                    }
                    Err(e) => self.error = e.to_string(),
                }
            }
        }
    }
    fn merge_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Branch");
            ui.text_edit_singleline(&mut self.merge.branch);
            ui.label("Strategy");
            ui.text_edit_singleline(&mut self.merge.strategy);
            ui.label("Strategy option (-X)");
            ui.text_edit_singleline(&mut self.merge.strategy_option);
        });
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.merge.no_commit, "No commit");
            ui.checkbox(&mut self.merge.squash, "Squash");
            ui.checkbox(&mut self.merge.ff_only, "Fast-forward only");
            ui.checkbox(&mut self.merge.no_ff, "No fast-forward");
            ui.checkbox(&mut self.merge.no_verify, "Skip hooks");
        });
        ui.label("Merge message");
        ui.text_edit_multiline(&mut self.merge.message);
        if ui.button("Merge…").clicked() {
            self.request(Request::merge(&self.merge));
        }
    }
    fn rebase_ui(&mut self, ui: &mut egui::Ui) {
        for (label, value) in [
            ("Onto (optional)", &mut self.rebase.onto),
            ("From / upstream", &mut self.rebase.upstream),
            ("Branch (empty = current)", &mut self.rebase.branch),
        ] {
            ui.horizontal(|ui| {
                ui.label(crate::i18n::tr_static(label));
                ui.text_edit_singleline(value);
            });
        }
        ui.checkbox(
            &mut self.rebase.preserve_merges,
            "Preserve merges (--rebase-merges)",
        );
        ui.checkbox(&mut self.rebase.autostash, "Autostash");
        if ui.button("Rebase…").clicked() {
            self.request(Request::rebase(&self.rebase));
        }
        ui.label("Use Interactive Rebase on a commit in Log to pick/reword/edit/squash/fixup/drop and reorder.");
    }
    fn revision_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Revision");
            ui.text_edit_singleline(&mut self.revision);
            ui.label("Mainline parent (0 = nonmerge)");
            ui.add(egui::DragValue::new(&mut self.mainline).range(0..=32));
        });
        ui.checkbox(&mut self.suffix, "Add cherry-pick suffix (-x)");
        ui.checkbox(&mut self.no_commit, "Apply without committing");
        let mainline = (self.mainline > 0).then_some(self.mainline);
        ui.horizontal(|ui| {
            if ui.button("Cherry-pick…").clicked() {
                self.request(Request::cherry_pick(
                    &self.revision,
                    self.suffix,
                    self.no_commit,
                    mainline,
                ));
            }
            if ui.button("Revert…").clicked() {
                self.request(Request::revert(&self.revision, self.no_commit, mainline));
            }
        });
        for mode in [Reset::Soft, Reset::Mixed, Reset::Hard, Reset::Keep] {
            ui.radio_value(&mut self.reset, mode, mode.flag());
        }
        if ui.button("Reset current branch…").clicked() {
            self.request(Request::reset(&self.revision, self.reset));
        }
    }
    fn stash_ui(&mut self, ui: &mut egui::Ui, repo: &GitRepo) {
        ui.text_edit_singleline(&mut self.stash_message);
        ui.checkbox(&mut self.stash_untracked, "Include untracked files");
        ui.checkbox(&mut self.stash_keep_index, "Keep index");
        if ui.button("Stash Changes…").clicked() {
            self.pending = Some(Request::stash(
                &self.stash_message,
                self.stash_untracked,
                self.stash_keep_index,
            ));
        }
        ui.label("Shelf stores a named recoverable set; stash uses Git's stack. Both remain available until explicitly removed.");
        ui.text_edit_singleline(&mut self.stash_branch);
        for (reference, message) in repo.operation_shelves() {
            ui.horizontal(|ui| {
                ui.label(format!("Shelf: {message} ({reference})"));
                if ui.button("Unshelve exact shelf…").clicked() {
                    self.request(Request::unshelve(&reference));
                }
            });
        }
        for stash in repo.stash_list().unwrap_or_default() {
            ui.horizontal(|ui| {
                ui.label(format!("stash@{{{}}}: {}", stash.index, stash.message));
                for action in ["apply", "pop", "drop", "branch"] {
                    if ui.button(crate::i18n::tr_static(action)).clicked() {
                        self.request(Request::stash_action(
                            action,
                            stash.index,
                            Some(&self.stash_branch),
                        ));
                    }
                }
            });
        }
    }
    fn history_ui(&mut self, ui: &mut egui::Ui, repo: &GitRepo, events: &mut Vec<GitEvent>) {
        self.path_field(ui);
        ui.horizontal(|ui| {
            ui.label("From revision");
            ui.text_edit_singleline(&mut self.revision);
            ui.checkbox(&mut self.follow, "Follow renames");
            ui.checkbox(&mut self.all_branches, "Show all branches");
        });
        ui.horizontal(|ui| {
            if ui.button("Show History").clicked() {
                self.inspect(rustpy_vcs::operation_inspection::Request::History {
                    path: self.selected_path(),
                    revision: self.revision.clone(),
                    follow: self.follow,
                    all: self.all_branches,
                    selection: None,
                });
            }
            if ui
                .add_enabled(
                    self.selection.is_some(),
                    egui::Button::new("History for selection"),
                )
                .clicked()
            {
                self.inspect(rustpy_vcs::operation_inspection::Request::History {
                    path: self.selected_path(),
                    revision: self.revision.clone(),
                    follow: self.follow,
                    all: self.all_branches,
                    selection: self.selection,
                });
            }
        });
        ui.horizontal(|ui| {
            ui.label("Patch output file");
            ui.text_edit_singleline(&mut self.patch_path);
        });
        for revision in self.history.clone() {
            let c = revision.commit;
            let history_path = revision.path;
            let response = ui.selectable_label(
                self.selected.as_deref() == Some(c.id.as_str()),
                format!(
                    "{} {} {} {}",
                    c.short_id,
                    c.date_string(),
                    c.author,
                    c.summary
                ),
            );
            if response.clicked() {
                self.selected = Some(c.id.clone());
                self.preview = repo
                    .commit_file_diff(&c.id, &history_path)
                    .unwrap_or_default();
            }
            response.context_menu(|ui| {
                if ui.button("Show commit in Git Log").clicked() {
                    events.push(GitEvent::ShowCommit(c.id.clone()));
                    self.open = false;
                    ui.close();
                }
                if ui.button("Show all affected files").clicked() {
                    events.push(GitEvent::ShowCommit(c.id.clone()));
                    self.open = false;
                    ui.close();
                }
                if ui.button("Open repository version").clicked() {
                    self.preview = repo
                        .file_at_commit(&c.id, &history_path)
                        .unwrap_or_else(|e| e.to_string());
                    ui.close();
                }
                if ui.button("Compare with local").clicked() {
                    let old = repo
                        .file_at_commit(&c.id, &history_path)
                        .unwrap_or_default();
                    let new = std::fs::read_to_string(self.selected_path()).unwrap_or_default();
                    self.preview = similar::TextDiff::from_lines(&old, &new)
                        .unified_diff()
                        .to_string();
                    ui.close();
                }
                if ui.button("Compare with previous").clicked() {
                    self.preview = repo
                        .commit_file_diff(&c.id, &history_path)
                        .unwrap_or_default();
                    ui.close();
                }
                if ui.button("Create patch").clicked() {
                    match repo.log_patch(&c.id).and_then(|text| {
                        anyhow::ensure!(
                            !self.patch_path.is_empty(),
                            "Choose a patch output file first"
                        );
                        std::fs::write(&self.patch_path, text)?;
                        Ok(())
                    }) {
                        Ok(()) => events.push(GitEvent::Status(format!(
                            "Patch saved to {}",
                            self.patch_path
                        ))),
                        Err(error) => self.error = error.to_string(),
                    }
                    ui.close();
                }
                if ui.button("Get this version…").clicked() {
                    self.request(Request::get_revision(repo, &c.id, &history_path));
                    ui.close();
                }
                if ui.button("Annotate this revision").clicked() {
                    self.file = Some(history_path.clone());
                    self.path.clear();
                    self.revision = c.id.clone();
                    self.mode = 6;
                    self.load_blame(repo);
                    ui.close();
                }
            });
        }
    }
    fn path_field(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("File / directory");
            ui.add(
                egui::TextEdit::singleline(&mut self.path)
                    .hint_text(
                        self.file
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "Repository root".into()),
                    )
                    .desired_width(600.0),
            );
        });
    }
    fn load_blame(&mut self, _repo: &GitRepo) {
        self.inspect(rustpy_vcs::operation_inspection::Request::Blame {
            path: self.selected_path(),
            revision: self.revision.clone(),
            ignore_whitespace: self.ignore_whitespace,
        });
    }
    fn blame_ui(
        &mut self,
        ui: &mut egui::Ui,
        repo: &GitRepo,
        theme: &UiTheme,
        events: &mut Vec<GitEvent>,
    ) {
        self.path_field(ui);
        ui.text_edit_singleline(&mut self.revision);
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.ignore_whitespace, "Ignore whitespace");
            ui.checkbox(&mut self.blame_author, "Author");
            ui.checkbox(&mut self.blame_date, "Date");
            ui.checkbox(&mut self.blame_revision, "Revision");
            ui.checkbox(&mut self.blame_lines, "Line numbers");
            ui.checkbox(&mut self.blame_age, "Colors by age (otherwise author)");
            ui.checkbox(&mut self.short_names, "Short author names");
        });
        if ui.button("Annotate").clicked() {
            self.load_blame(repo);
        }
        if ui.button("Close annotations").clicked() {
            self.blame.clear();
        }
        let source = repo
            .file_at_commit(&self.revision, &self.selected_path())
            .unwrap_or_default();
        let source_lines = source.lines().collect::<Vec<_>>();
        for row in self.blame.clone() {
            let color = if self.blame_age {
                let days = (chrono::Utc::now().timestamp() - row.time).max(0) / 86400;
                Color32::from_rgb((60 + days.min(150)) as u8, 110, 140)
            } else {
                let hash = row.author.bytes().fold(0u8, |a, b| a.wrapping_add(b));
                Color32::from_rgb(80 + hash % 120, 120, 170)
            };
            let mut fields = Vec::new();
            if self.blame_lines {
                fields.push(row.line.to_string());
            }
            if self.blame_revision {
                fields.push(row.commit[..8].into());
            }
            if self.blame_author {
                fields.push(if self.short_names {
                    row.author.split_whitespace().next().unwrap_or("").into()
                } else {
                    row.author.clone()
                });
            }
            if self.blame_date {
                fields.push(
                    chrono::DateTime::from_timestamp(row.time, 0)
                        .map(|d| d.format("%Y-%m-%d").to_string())
                        .unwrap_or_default(),
                );
            }
            let response = ui
                .label(
                    egui::RichText::new(format!(
                        "{} │ {}",
                        fields.join(" · "),
                        source_lines.get(row.line - 1).unwrap_or(&"")
                    ))
                    .color(if theme.high_contrast {
                        theme.text
                    } else {
                        color
                    })
                    .monospace(),
                )
                .on_hover_ui(|ui| {
                    ui.label(
                        repo.operation_revision_message(&row.commit)
                            .unwrap_or_else(|_| row.summary.clone()),
                    );
                });
            response.context_menu(|ui| {
                if ui.button("Show commit in Git Log").clicked() {
                    events.push(GitEvent::ShowCommit(row.commit.clone()));
                    self.open = false;
                    ui.close();
                }
                if ui.button("Copy revision").clicked() {
                    ui.ctx().copy_text(row.commit.clone());
                    ui.close();
                }
                if ui.button("Show diff").clicked() {
                    self.preview = repo
                        .commit_file_diff(&row.commit, &self.selected_path())
                        .unwrap_or_default();
                    ui.close();
                }
                if ui.button("Annotate previous revision").clicked() {
                    if let Ok(path) = repo.operation_parent_path(&self.selected_path(), &row.commit)
                    {
                        self.file = Some(path);
                        self.path.clear();
                    }
                    self.revision = format!("{}^", row.commit);
                    self.load_blame(repo);
                    ui.close();
                }
            });
        }
    }
    fn compare_ui(&mut self, ui: &mut egui::Ui, repo: &GitRepo, events: &mut Vec<GitEvent>) {
        self.path_field(ui);
        ui.horizontal(|ui| {
            ui.label("Old revision / branch");
            ui.text_edit_singleline(&mut self.other_revision);
            ui.label("New revision / branch");
            ui.text_edit_singleline(&mut self.revision);
        });
        if ui.button("Compare revisions").clicked() {
            self.inspect(rustpy_vcs::operation_inspection::Request::Compare {
                old: self.other_revision.clone(),
                new: self.revision.clone(),
                path: (!self.path.is_empty() || self.file.is_some()).then(|| self.selected_path()),
            });
        }
        if ui
            .button("Compare with same repository version / local")
            .clicked()
        {
            self.preview = repo
                .log_diff_local(&self.revision)
                .unwrap_or_else(|e| e.to_string());
        }
        for file in self.compare_files.clone() {
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(false, file.path.display().to_string())
                    .clicked()
                {
                    self.preview = file.patch;
                }
                if ui.small_button("Open").clicked() {
                    events.push(GitEvent::OpenFile(file.path));
                }
            });
        }
    }
    fn browse_ui(&mut self, ui: &mut egui::Ui, repo: &GitRepo, events: &mut Vec<GitEvent>) {
        ui.text_edit_singleline(&mut self.revision);
        if ui.button("Browse repository at revision").clicked() {
            self.inspect(rustpy_vcs::operation_inspection::Request::Browse {
                revision: self.revision.clone(),
                directory: self.browse_path.clone(),
            });
        }
        if ui.button("Parent directory").clicked() {
            self.browse_path.pop();
            self.inspect(rustpy_vcs::operation_inspection::Request::Browse {
                revision: self.revision.clone(),
                directory: self.browse_path.clone(),
            });
        }
        for entry in self.browse.clone() {
            let response = ui.selectable_label(
                false,
                format!(
                    "{} {} ({} bytes)",
                    if entry.directory { "📁" } else { "" },
                    entry.path.display(),
                    entry.size
                ),
            );
            if response.clicked() {
                if entry.directory {
                    self.browse_path = entry.path;
                    self.inspect(rustpy_vcs::operation_inspection::Request::Browse {
                        revision: self.revision.clone(),
                        directory: self.browse_path.clone(),
                    });
                } else {
                    self.preview = repo
                        .file_at_commit(&self.revision, &repo.root().join(entry.path))
                        .unwrap_or_else(|e| e.to_string());
                }
            }
        }
        if ui.button("Show current file in Log").clicked() {
            events.push(GitEvent::ShowCommit(self.revision.clone()));
            self.open = false;
        }
    }
    fn conflict_ui(&mut self, ui: &mut egui::Ui, repo: &GitRepo, events: &mut Vec<GitEvent>) {
        for path in repo.conflicts().unwrap_or_default() {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    path.strip_prefix(repo.root())
                        .unwrap_or(&path)
                        .display()
                        .to_string(),
                );
                if ui.button("Merge…").clicked() {
                    events.push(GitEvent::ResolveConflict(path.clone()));
                    self.open = false;
                }
                for (label, theirs) in [("Accept Yours", false), ("Accept Theirs", true)] {
                    if ui.button(crate::i18n::tr_static(label)).clicked() {
                        snapshot_working_tree(
                            repo,
                            repo.root(),
                            "Before accepting conflict side",
                            true,
                        );
                        match repo.accept_operation_side(&path, theirs) {
                            Ok(()) => events.push(GitEvent::FilesChanged),
                            Err(e) => self.error = e.to_string(),
                        }
                    }
                }
                if ui.button("Mark resolved").clicked() {
                    match repo.mark_operation_resolved(&path) {
                        Ok(()) => events.push(GitEvent::FilesChanged),
                        Err(e) => self.error = e.to_string(),
                    }
                }
                if ui.button("git mergetool…").clicked() {
                    self.request(Request::paths(
                        repo,
                        "mergetool",
                        std::slice::from_ref(&path),
                    ));
                }
            });
        }
    }
    fn roots_ui(&mut self, ui: &mut egui::Ui, repo: &GitRepo, events: &mut Vec<GitEvent>) {
        for (name, path) in repo.operation_worktrees() {
            ui.horizontal(|ui| {
                ui.label(format!("Worktree: {name} ({})", path.display()));
                if ui.button("Open as project").clicked() {
                    events.push(GitEvent::OpenRepository(path));
                    self.open = false;
                }
            });
        }
        if ui.button("Update / initialize submodules…").clicked() {
            self.pending = Some(Request::submodules());
        }
        for module in repo.operation_submodules().unwrap_or_default() {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "{} {} {}",
                    module.name,
                    if module.initialized {
                        "initialized"
                    } else {
                        "uninitialized"
                    },
                    if module.dirty { "modified" } else { "clean" }
                ));
                if ui.button("Submodule log").clicked() {
                    self.root = module.path;
                    events.push(GitEvent::ShowCommit("HEAD".into()));
                    self.open = false;
                    self.path.clear();
                }
            });
        }
        ui.separator();
        ui.label("Open a worktree or another repository as a project");
        ui.text_edit_singleline(&mut self.repo_input);
        if ui.button("Open repository as project").clicked() {
            let root = PathBuf::from(&self.repo_input);
            if let Some(repo) = GitRepo::discover(&root) {
                events.push(GitEvent::OpenRepository(repo.root().to_path_buf()));
                self.open = false;
            } else {
                self.error = "Not a repository".into();
            }
        }
    }
    fn file_ui(&mut self, ui: &mut egui::Ui, repo: &GitRepo, events: &mut Vec<GitEvent>) {
        self.path_field(ui);
        let path = self.selected_path();
        for (label, command) in [
            ("Add to Git…", "add"),
            ("Remove from Git…", "rm"),
            ("Discard local changes…", "restore"),
        ] {
            if ui.button(crate::i18n::tr_static(label)).clicked() {
                self.request(Request::paths(repo, command, std::slice::from_ref(&path)));
            }
        }
        if ui.button("Edit .gitignore").clicked() {
            events.push(GitEvent::OpenFile(repo.root().join(".gitignore")));
            self.open = false;
        }
        if let Ok(attributes) = repo.operation_attributes(&path) {
            ui.label(format!(
                ".gitattributes: filter={:?}, text={:?}, eol={:?}, encoding={:?}",
                attributes.filter,
                attributes.text,
                attributes.eol,
                attributes.working_tree_encoding
            ));
            if attributes.large {
                ui.label("Large file (>10 MiB): consider Git LFS before adding it.");
            }
            if attributes.lfs_pointer || attributes.filter.as_deref() == Some("lfs") {
                ui.label("Git LFS file: the repository stores a pointer; content requires the configured LFS filters.");
            }
        }
    }
    fn preferences_ui(&mut self, ui: &mut egui::Ui, events: &mut Vec<GitEvent>) {
        let before = self.preferences.clone();
        ui.strong("Editor Git annotations");
        for (value, label) in [
            (&mut self.preferences.blame_author, "Show author"),
            (&mut self.preferences.blame_date, "Show date"),
            (&mut self.preferences.blame_revision, "Show revision number"),
            (&mut self.preferences.blame_lines, "Show line numbers"),
            (
                &mut self.preferences.blame_age,
                "Color by age (otherwise author)",
            ),
            (
                &mut self.preferences.blame_short_names,
                "Short author names",
            ),
            (
                &mut self.preferences.blame_ignore_whitespace,
                "Ignore whitespace",
            ),
            (
                &mut self.preferences.annotate_title,
                "Mark annotated editor in window title",
            ),
        ] {
            ui.checkbox(value, crate::i18n::tr_static(label));
        }
        for (label, value) in [
            ("When files are created", &mut self.preferences.created),
            ("When files are deleted", &mut self.preferences.removed),
        ] {
            ui.label(crate::i18n::tr_static(label));
            for (mode, label) in [
                (FilePolicy::Add, "Add / stage removal automatically"),
                (FilePolicy::Ask, "Ask"),
                (FilePolicy::DoNotAdd, "Do not stage"),
            ] {
                ui.radio_value(value, mode, crate::i18n::tr_static(label));
            }
        }
        ui.checkbox(
            &mut self.preferences.show_commands,
            "Show Git commands in Console",
        );
        ui.checkbox(&mut self.preferences.push_hooks, "Run push hooks");
        ui.checkbox(
            &mut self.preferences.push_force_with_lease,
            "Use lease when force pushing",
        );
        ui.checkbox(
            &mut self.preferences.update_after_rejected_push,
            "Automatically update and retry once after a rejected push",
        );
        if self.preferences != before {
            events.push(GitEvent::OperationsSettings(self.preferences.clone()));
        }
    }
}
impl GitPanel {
    pub fn open_revision_history(
        &mut self,
        root: PathBuf,
        file: PathBuf,
        selection: Option<(usize, usize)>,
        preferences: Settings,
    ) {
        self.open_operations(root, Some(file.clone()), selection, preferences);
        let state = &mut self.operations_workspace;
        state.mode = 5;
        state.inspect(rustpy_vcs::operation_inspection::Request::History {
            path: file,
            revision: "HEAD".into(),
            follow: true,
            all: false,
            selection: None,
        });
    }
    pub fn open_annotation_revision(
        &mut self,
        repo: &GitRepo,
        revision: String,
        intent: crate::editor::view::BlameIntent,
    ) {
        let state = &mut self.operations_workspace;
        state.revision = revision.clone();
        state.mode = match intent {
            crate::editor::view::BlameIntent::Options => 12,
            crate::editor::view::BlameIntent::Diff => 7,
            _ => 6,
        };
        if intent == crate::editor::view::BlameIntent::Previous {
            if let Ok(path) = repo.operation_parent_path(&state.selected_path(), &revision) {
                state.file = Some(path);
                state.path.clear();
            }
            state.revision = format!("{revision}^");
            state.load_blame(repo);
        }
        if intent == crate::editor::view::BlameIntent::Diff {
            state.preview = repo
                .commit_file_diff(&revision, &state.selected_path())
                .unwrap_or_else(|e| e.to_string());
        }
    }
    pub fn prepare_file_policy(&mut self, path: PathBuf, args: Vec<String>, removed: bool) {
        self.operations_workspace.file = Some(path);
        self.operations_workspace.pending = Some(Request {
            label: if removed {
                "Stage removed file?".into()
            } else {
                "Add created file?".into()
            },
            args,
            mutates: true,
            destructive: false,
        });
    }
    pub fn open_operations(
        &mut self,
        root: PathBuf,
        file: Option<PathBuf>,
        selection: Option<(usize, usize)>,
        preferences: Settings,
    ) {
        if self.operations_workspace.root != root || self.operations_workspace.file != file {
            self.operations_workspace.pending = None;
            self.operations_workspace.inspection = None;
            self.operations_workspace.history.clear();
            self.operations_workspace.blame.clear();
            self.operations_workspace.preview.clear();
            self.operations_workspace.path.clear();
            self.operations_workspace.selected = None;
        }
        self.operations_workspace.root = root.clone();
        if !self.operations_workspace.roots.contains(&root) {
            self.operations_workspace.roots.push(root);
        }
        self.operations_workspace.file = file;
        self.operations_workspace.selection = selection;
        self.operations_workspace.preferences = preferences;
        self.operations_workspace.open = true;
    }
    pub fn operations_modal_active(&self) -> bool {
        self.operations_workspace.open
    }
    pub fn cancel_operation_project(&mut self) {
        self.operations_workspace = Workspace::default();
    }
    pub fn poll_operations_queue(&mut self) -> Option<crate::git_jobs::GitOp> {
        if self.git_jobs.is_busy() || self.operations_workspace.queue.front().is_some_and(|op| matches!(op,crate::git_jobs::GitOp::BranchNetwork{root,..} if rustpy_vcs::running_operation(root).is_some())) {
            None
        } else {
            self.operations_workspace.queue.pop_front()
        }
    }
    pub fn operation_failed(&mut self) {
        self.operations_workspace.queue.clear();
    }
    pub(super) fn operation_shelf_ui(
        &mut self,
        ui: &mut egui::Ui,
        repo: &GitRepo,
        now: f64,
        events: &mut Vec<GitEvent>,
    ) {
        self.commit_shelves_ui(ui, repo, now, events);
        ui.separator();
        ui.label("Recoverable working-tree shelves");
        for (reference, message) in repo.operation_shelves() {
            ui.horizontal(|ui| {
                ui.label(format!("{message} ({reference})"));
                if ui.button("Unshelve…").clicked() {
                    self.operations_workspace.root = repo.root().to_path_buf();
                    self.operations_workspace.open = true;
                    self.operations_workspace
                        .request(Request::unshelve(&reference));
                }
            });
        }
    }
    pub(super) fn operations_ui(
        &mut self,
        ui: &mut egui::Ui,
        theme: &UiTheme,
        repo: &GitRepo,
        events: &mut Vec<GitEvent>,
    ) {
        if !self.operations_workspace.open {
            return;
        }
        let mut state = std::mem::take(&mut self.operations_workspace);
        let other = GitRepo::discover(&state.root);
        let repo = other.as_ref().unwrap_or(repo);
        let mut open = true;
        let mut local_events = Vec::new();
        egui::Window::new("VCS Operations")
            .id(egui::Id::new("vcs-operations"))
            .open(&mut open)
            .resizable(true)
            .default_size([1000.0, 650.0])
            .show_modeless(ui.ctx(), |ui| {
                egui::ScrollArea::both().show(ui, |ui| {
                    state.ui(ui, theme, repo, &mut local_events);
                });
            });
        state.open &= open;
        for event in local_events {
            if let GitEvent::ShowCommit(revision) = event {
                self.reveal_log_root(state.root.clone(), revision);
            } else {
                events.push(event);
            }
        }
        self.operations_workspace = state;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_cancellation_discards_captured_requests_and_recovery_intents() {
        let mut panel = GitPanel::default();
        panel.operations_workspace.open = true;
        panel.operations_workspace.file = Some("/old/main.py".into());
        panel.operations_workspace.pending = Some(Request::reset("HEAD~1", Reset::Hard).unwrap());
        panel
            .operations_workspace
            .queue
            .push_back(crate::git_jobs::GitOp::BranchNetwork {
                root: "/old".into(),
                label: "Pull".into(),
                args: vec!["pull".into()],
            });
        panel.cancel_operation_project();
        assert!(!panel.operations_modal_active());
        assert!(panel.poll_operations_queue().is_none());
        assert!(panel.operations_workspace.file.is_none());
        assert!(panel.operations_workspace.pending.is_none());
    }
}
