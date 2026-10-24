//! Git › Push… (roadmap §4.1 "Push (dialog with commits preview, target
//! remote/branch editing, push tags, force push with lease with
//! protected-branch check, push to new remote branch tracking)"), "Define
//! remote" for a repository without one, and the quick push of the Git tool
//! window and Commit and Push.

use super::*;
use crate::a11y::AccessibleCombo;
use crate::a11y::DialogWindow;
use crate::git_jobs::GitOp;
use rustpy_vcs::{is_protected_branch, is_valid_branch_name, CommitInfo, PushRequest, PushTags};

/// Commits listed in the dialog (more are counted as "and more").
const COMMIT_LIMIT: usize = 300;

pub(super) struct PushState {
    root: PathBuf,
    run_hooks: bool,
    use_lease: bool,
    local_branch: String,
    /// `(remote, branch)` the local branch tracks.
    upstream: Option<(String, String)>,
    /// `(name, url)` of every remote.
    remotes: Vec<(String, String)>,
    remote: String,
    remote_branch: String,
    commits: Vec<CommitInfo>,
    more: bool,
    /// The remote has the target branch (per the last fetch).
    exists: bool,
    /// Target the commit list was computed for.
    computed: Option<(String, String)>,
    force: bool,
    tags: PushTags,
    define_name: String,
    define_url: String,
    /// Why the default target needs a look (the upstream branch has another
    /// name, `push.default = nothing`, …): the reason Push opened the dialog.
    note: Option<String>,
    error: Option<String>,
}

impl PushState {
    fn new(repo: &GitRepo) -> Result<PushState, String> {
        let t = repo
            .push_target()
            .ok_or("Nothing to push: check out a branch that has commits")?;
        let remotes = repo.remotes_with_urls().unwrap_or_default();
        let mut st = PushState {
            root: repo.root().to_path_buf(),
            run_hooks: true,
            use_lease: true,
            local_branch: t.local_branch,
            upstream: t.upstream,
            remotes,
            remote: t.remote.unwrap_or_default(),
            remote_branch: t.remote_branch,
            commits: Vec::new(),
            more: false,
            exists: false,
            computed: None,
            force: false,
            tags: PushTags::None,
            define_name: "origin".into(),
            define_url: String::new(),
            note: t.confirm,
            error: None,
        };
        st.refresh(repo);
        Ok(st)
    }

    /// Recomputes the outgoing commits after the target changed.
    fn refresh(&mut self, repo: &GitRepo) {
        let key = (self.remote.clone(), self.remote_branch.trim().to_string());
        if self.computed.as_ref() == Some(&key) {
            return;
        }
        self.computed = Some(key);
        if self.remote.is_empty() || !is_valid_branch_name(self.remote_branch.trim()) {
            self.commits.clear();
            self.more = false;
            self.exists = false;
            return;
        }
        let branch = self.remote_branch.trim();
        self.exists = repo.remote_branch_exists(&self.remote, branch);
        match repo.outgoing_commits(&self.local_branch, &self.remote, branch, COMMIT_LIMIT) {
            Ok((c, more)) => {
                self.commits = c;
                self.more = more;
            }
            Err(e) => {
                self.commits.clear();
                self.error = Some(e.to_string());
            }
        }
    }

    fn target_is_upstream(&self) -> bool {
        self.upstream
            .as_ref()
            .is_some_and(|(r, b)| *r == self.remote && b == self.remote_branch.trim())
    }

    fn request(&self) -> PushRequest {
        PushRequest {
            local_branch: self.local_branch.clone(),
            remote: self.remote.clone(),
            remote_branch: self.remote_branch.trim().to_string(),
            set_upstream: self.upstream.is_none(),
            force_with_lease: self.force && !is_protected_branch(self.remote_branch.trim()),
            tags: self.tags,
        }
    }

    /// Why Push is disabled, if it is.
    fn blocker(&self) -> Option<&'static str> {
        if self.remote.is_empty() {
            return Some("Define a remote first");
        }
        if !is_valid_branch_name(self.remote_branch.trim()) {
            return Some("Enter a valid branch name");
        }
        if self.exists && self.commits.is_empty() && self.tags == PushTags::None {
            return Some("Nothing to push");
        }
        None
    }
}

/// The push that Push in the Git tool window and Commit and Push run at
/// once, or `None` when the Push dialog opens instead (see `quick_push`).
fn direct_push(repo: &GitRepo, confirm_before_push: bool) -> Option<PushRequest> {
    if confirm_before_push {
        return None;
    }
    repo.push_target()
        .filter(|t| t.is_direct())
        .and_then(|t| PushRequest::from_target(&t))
}

impl IdeApp {
    fn git_repo(&self) -> Option<&GitRepo> {
        self.project.as_ref().and_then(|p| p.git.as_ref())
    }

    /// Git › Push… (Ctrl+Shift+K).
    pub(super) fn open_push_dialog(&mut self) {
        let Some(g) = self.git_repo() else {
            self.set_status("Not a Git repository");
            return;
        };
        match PushState::new(g) {
            Ok(mut st) => {
                st.run_hooks = self.settings.vcs_operations.push_hooks;
                st.use_lease = self.settings.vcs_operations.push_force_with_lease;
                self.dialog = Dialog::Push(Box::new(st));
            }
            Err(e) => self.set_status(e),
        }
    }

    /// Push from the Git tool window and Commit and Push: straight to the
    /// default target (where `git push` would push), or through the Push
    /// dialog when Settings › Version Control › "Confirm before push" is
    /// on, no remote is defined, or the target needs a look: `git push`
    /// would refuse it, or the remote branch has another name than the
    /// local one (a branch created from `origin/main` tracks main).
    pub(super) fn quick_push(&mut self) {
        let Some(g) = self.git_repo() else {
            self.set_status("Not a Git repository");
            return;
        };
        match direct_push(g, self.settings.vcs.confirm_before_push) {
            Some(req) => {
                let commits = g
                    .outgoing_commits(
                        &req.local_branch,
                        &req.remote,
                        &req.remote_branch,
                        COMMIT_LIMIT,
                    )
                    .ok()
                    .filter(|(_, more)| !more)
                    .map(|(c, _)| c.len());
                self.start_push(req, commits);
            }
            None => self.open_push_dialog(),
        }
    }

    /// Runs a push in the background (the result goes to the status bar and
    /// the Git console).
    pub(super) fn start_push(&mut self, request: PushRequest, commits: Option<usize>) {
        self.git_panel.operations_workspace.auto_retries = 0;
        let Some(root) = self.git_repo().map(|repo| repo.root().to_path_buf()) else {
            return;
        };
        self.start_push_at_root(
            root,
            request,
            commits,
            self.settings.vcs_operations.push_hooks,
            self.settings.vcs_operations.push_force_with_lease,
        );
    }
    pub(super) fn start_push_at_root(
        &mut self,
        root: PathBuf,
        request: PushRequest,
        commits: Option<usize>,
        run_hooks: bool,
        use_lease: bool,
    ) {
        if !self.builtin_plugin_enabled("git") {
            self.set_status("Git bundled capability is disabled in Plugins");
            return;
        }

        if self.project_safe_mode() {
            self.notify_safe_mode("Pushing commits and running push hooks");
            return;
        }

        rustpy_vcs::set_git_program(&self.settings.vcs.git_executable);
        if request.force_with_lease && is_protected_branch(&request.remote_branch) {
            self.set_status("Force push to a protected branch is disabled");
            return;
        }
        let mut args = request.args();
        if !run_hooks {
            args.insert(1, "--no-verify".into());
        }
        if request.force_with_lease && !use_lease {
            args.retain(|arg| !arg.starts_with("--force-with-lease"));
            args.insert(1, "--force".into());
        }
        let status = format!(
            "Pushing {} to {}…",
            request.local_branch,
            request.target_label()
        );
        match self
            .git_panel
            .git_jobs
            .start(GitOp::Push { request, commits }, &root, args)
        {
            Ok(()) => self.set_status(status),
            Err(error) => self.set_status(format!("Cannot push: {error}")),
        }
    }

    pub(super) fn push_dialog(&mut self, ctx: &egui::Context, mut st: Box<PushState>) {
        let theme = self.theme.clone();
        let mut next_root = None;
        let roots = self
            .vcs_workspace
            .mappings
            .roots
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        let Some(repository) = GitRepo::discover(&st.root) else {
            return;
        };
        let repo = &repository;
        st.refresh(repo);
        let title = format!(
            "Push Commits to {}",
            repo.root()
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
        let (mut open, mut push, mut cancel, mut define) = (true, false, false, false);
        egui::Window::new(title).id(egui::Id::new("git_push_dialog")).collapsible(false).resizable(true).default_width(680.0).anchor(Align2::CENTER_CENTER, Vec2::ZERO).open(&mut open).show_modal(ctx, |ui| {
            if let Some(note) = &st.note {
                ui.label(egui::RichText::new(note).color(theme.warning));
                ui.add_space(4.0);
            }
            // Target: local → remote : branch.
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&st.local_branch).strong());
                // The UI font has no arrow glyph; the editor font does.
                ui.label(egui::RichText::new("→").monospace());
                if st.remotes.is_empty() {
                    ui.label(egui::RichText::new("no remote").color(theme.warning));
                } else {
                    egui::ComboBox::from_id_salt("push_remote").selected_text(&st.remote).show_ui(ui, |ui| {
                        for (name, url) in st.remotes.clone() {
                            ui.selectable_value(&mut st.remote, name.clone(), &name).on_hover_text(url);
                        }
                    }).combo_named(crate::i18n::tr_static("Remote"));
                }
                ui.label(":");
                ui.add(egui::TextEdit::singleline(&mut st.remote_branch).desired_width(220.0)).on_hover_text("Branch on the remote; a new name creates it");
                if !st.remote.is_empty() && !st.exists && is_valid_branch_name(st.remote_branch.trim()) {
                    ui.label(egui::RichText::new(" New ").background_color(theme.accent).color(egui::Color32::WHITE).small()).on_hover_text("The remote has no branch with this name (as of the last fetch): the push creates it");
                }
            });
            if st.upstream.is_none() && !st.remote.is_empty() {
                ui.label(egui::RichText::new(format!("{} will track {}/{} after the push.", st.local_branch, st.remote, st.remote_branch.trim())).color(theme.text_dim).small());
            } else if st.upstream.is_some() && !st.target_is_upstream() {
                let (r, b) = st.upstream.clone().unwrap_or_default();
                ui.label(egui::RichText::new(format!("{} tracks {r}/{b}; pushing elsewhere does not change that.", st.local_branch)).color(theme.text_dim).small());
            }
            // No remote: define one here.
            if st.remotes.is_empty() {
                ui.add_space(6.0);
                ui.label(egui::RichText::new("This repository has no remote. Define one to push to:").color(theme.text));
                egui::Grid::new("push_define_remote").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("Name:");
                    ui.add(egui::TextEdit::singleline(&mut st.define_name).desired_width(160.0));
                    ui.end_row();
                    ui.label("URL:");
                    let r = ui.add(egui::TextEdit::singleline(&mut st.define_url).desired_width(440.0).hint_text("git@github.com:user/repo.git or https://github.com/user/repo.git"));
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        define = true;
                    }
                    ui.end_row();
                });
                if ui.button("Define Remote").clicked() {
                    define = true;
                }
            }
            ui.separator();
            // Outgoing commits.
            let heading = match (st.commits.len(), st.more) {
                (0, _) if st.remote.is_empty() => "Commits".to_string(),
                (0, _) => "Nothing to push: the remote branch has every commit".to_string(),
                (n, true) => format!("Commits to push ({n}+)"),
                (n, false) => format!("Commits to push ({n})"),
            };
            ui.label(egui::RichText::new(heading).strong());
            egui::ScrollArea::vertical().id_salt("push_commits").max_height(260.0).auto_shrink([false, true]).show(ui, |ui| {
                for c in &st.commits {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&c.short_id).monospace().color(theme.text_dim));
                        ui.label(egui::RichText::new(&c.summary).color(theme.text));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(egui::RichText::new(c.date_string()).color(theme.text_dim).small());
                            ui.label(egui::RichText::new(&c.author).color(theme.text_dim).small());
                        });
                    });
                }
                if st.more {
                    ui.label(egui::RichText::new(format!("… and more (only the newest {COMMIT_LIMIT} are listed)")).color(theme.text_dim).small());
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                let repository_label = ui.label("Repository:");
                egui::ComboBox::from_id_salt("push-root").selected_text(st.root.display().to_string()).show_ui(ui,|ui|{for root in &roots{if ui.selectable_label(root==&st.root,root.display().to_string()).clicked(){next_root=Some(root.clone());}}}).combo_labelled(&repository_label);
            });
            ui.checkbox(&mut st.run_hooks,"Run hooks");
            ui.checkbox(&mut st.use_lease,"Use --force-with-lease for force push");
            ui.horizontal(|ui| {
                let tags_label = ui.label("Push tags:");
                egui::ComboBox::from_id_salt("push_tags").selected_text(st.tags.label()).show_ui(ui, |ui| {
                    for t in [PushTags::None, PushTags::CurrentBranch, PushTags::All] {ui.selectable_value(&mut st.tags,t,t.label());}
                }).combo_labelled(&tags_label);
            });
            let protected = is_protected_branch(st.remote_branch.trim());
            ui.add_enabled_ui(!protected, |ui| {
                ui.checkbox(&mut st.force, if st.use_lease {"Force push with lease"}else{"Force push (overwrite remote history)"}).on_hover_text(if st.use_lease {"Refuses to overwrite a remote branch that moved since the last fetch"}else{"Overwrites remote branch history without checking its previous revision"}).on_disabled_hover_text("Force push to a protected branch (main, master) is not allowed");
            });
            if protected {
                st.force = false;
            } else if st.force {
                ui.label(egui::RichText::new("The remote branch is overwritten with your commits.").color(theme.warning).small());
            }
            if let Some(e) = &st.error {
                ui.label(egui::RichText::new(e).color(theme.error));
            }
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    let blocker = st.blocker();
                    let label = if st.force { "Force Push" } else { "Push" };
                    let r = ui.add_enabled(blocker.is_none(), egui::Button::new(label));
                    if r.clicked() {
                        push = true;
                    }
                    if let Some(b) = blocker {
                        r.on_disabled_hover_text(b);
                    }
                });
            });
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                cancel = true;
            }
        });
        if define {
            let (name, url) = (
                st.define_name.trim().to_string(),
                st.define_url.trim().to_string(),
            );
            match repo.add_remote_checked(&name, &url) {
                Ok(()) => {
                    st.remotes = repo.remotes_with_urls().unwrap_or_default();
                    st.remote = name.clone();
                    st.computed = None;
                    st.error = None;
                    self.set_status(format!("Remote '{name}' added: {url}"));
                }
                Err(e) => st.error = Some(e.to_string()),
            }
        }
        if let Some(root) = next_root {
            if let Some(repo) = GitRepo::discover(&root) {
                match PushState::new(&repo) {
                    Ok(mut next) => {
                        next.run_hooks = st.run_hooks;
                        next.use_lease = st.use_lease;
                        self.dialog = Dialog::Push(Box::new(next));
                    }
                    Err(error) => self.set_status(error),
                }
            }
            return;
        }
        if push {
            let commits = (!st.more).then_some(st.commits.len());
            let req = st.request();
            self.git_panel.operations_workspace.auto_retries = 0;
            self.start_push_at_root(st.root.clone(), req, commits, st.run_hooks, st.use_lease);
            self.focus_editor = true;
        } else if open && !cancel {
            self.dialog = Dialog::Push(st);
        } else {
            self.focus_editor = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn the_dialog_targets_the_upstream_or_a_new_branch() {
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let dir = crate::git_jobs::TestDir::new("push-dialog");
        let (bare, work) = (dir.path().join("o.git"), dir.path().join("w"));
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        git(&bare, &["init", "--bare", "-q", "-b", "main"]);
        git(&work, &["init", "-q", "-b", "main"]);
        std::fs::write(work.join("a"), "1").unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "-q", "-m", "one"]);
        // No remote: Push is blocked until one is defined.
        let repo = GitRepo::discover(&work).unwrap();
        let st = PushState::new(&repo).unwrap();
        assert!(st.remotes.is_empty());
        assert_eq!(st.blocker(), Some("Define a remote first"));
        repo.add_remote_checked("origin", bare.to_str().unwrap())
            .unwrap();
        let mut st = PushState::new(&repo).unwrap();
        assert_eq!(
            (st.remote.as_str(), st.remote_branch.as_str(), st.exists),
            ("origin", "main", false)
        );
        assert_eq!(st.commits.len(), 1);
        assert_eq!(st.blocker(), None);
        let req = st.request();
        assert!(req.set_upstream && !req.force_with_lease);
        // Force is never requested for a protected branch.
        st.force = true;
        assert!(!st.request().force_with_lease);
        st.remote_branch = "topic".into();
        st.refresh(&repo);
        assert!(st.request().force_with_lease);
        st.remote_branch = "bad..name".into();
        st.refresh(&repo);
        assert_eq!(st.blocker(), Some("Enter a valid branch name"));
        // After pushing with an upstream: nothing left to push.
        git(&work, &["push", "-q", "-u", "origin", "main"]);
        let st = PushState::new(&repo).unwrap();
        assert!(st.exists && st.commits.is_empty() && st.target_is_upstream());
        assert_eq!(st.blocker(), Some("Nothing to push"));
        assert!(!st.request().set_upstream);
    }

    /// Review: with `git checkout -b feature origin/main`, Push in the Git
    /// tool window pushed `refs/heads/feature:refs/heads/main` at once.
    #[test]
    fn quick_push_of_a_branch_tracking_another_name_opens_the_dialog() {
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let dir = crate::git_jobs::TestDir::new("push-quick");
        let (bare, work) = (dir.path().join("o.git"), dir.path().join("w"));
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        git(&bare, &["init", "--bare", "-q", "-b", "main"]);
        git(&work, &["init", "-q", "-b", "main"]);
        git(&work, &["config", "push.default", "simple"]);
        std::fs::write(work.join("a"), "1").unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "-q", "-m", "one"]);
        git(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
        git(&work, &["push", "-q", "-u", "origin", "main"]);
        // On main, tracking origin/main: pushed at once (unless confirmation is asked for).
        let repo = GitRepo::discover(&work).unwrap();
        let req = direct_push(&repo, false).expect("a direct push");
        assert_eq!(
            req.args().last().map(String::as_str),
            Some("refs/heads/main:refs/heads/main")
        );
        assert!(direct_push(&repo, true).is_none());
        // A feature branch created from origin/main: the dialog, proposing origin/feature.
        git(
            &work,
            &["checkout", "-q", "-b", "feature", "--track", "origin/main"],
        );
        std::fs::write(work.join("f"), "feature").unwrap();
        git(&work, &["add", "."]);
        git(&work, &["commit", "-q", "-m", "feature work"]);
        let repo = GitRepo::discover(&work).unwrap();
        assert!(
            direct_push(&repo, false).is_none(),
            "never straight to origin/main"
        );
        let st = PushState::new(&repo).unwrap();
        assert_eq!(
            (st.remote.as_str(), st.remote_branch.as_str(), st.exists),
            ("origin", "feature", false)
        );
        assert!(
            st.note
                .as_deref()
                .is_some_and(|n| n.contains("feature tracks origin/main")),
            "{:?}",
            st.note
        );
        assert_eq!(
            st.upstream,
            Some(("origin".to_string(), "main".to_string()))
        );
        assert!(!st.target_is_upstream());
        assert_eq!(
            st.commits
                .iter()
                .map(|c| c.summary.as_str())
                .collect::<Vec<_>>(),
            vec!["feature work"]
        );
        let req = st.request();
        assert_eq!(
            req.args().last().map(String::as_str),
            Some("refs/heads/feature:refs/heads/feature")
        );
        assert!(!req.set_upstream, "the upstream stays origin/main");
    }
}
