use super::settings_ui::SettingsPage;
use super::*;
use crate::a11y::DialogWindow;
use rustpy_vcs::mapping::{Detection, DetectionLimits, Detector, DirectoryMappings};
use rustpy_vcs::provider::{Integration, Provider};

pub(super) struct PendingCommit {
    pub rx: std::sync::mpsc::Receiver<Result<String, String>>,
    pub root: PathBuf,
    pub message: String,
    pub amend: bool,
    pub push: bool,
    pub rewrote: bool,
    pub id: u64,
    pub task_close: Option<(PathBuf, String, bool)>,
}

type IntegrationResult = Result<(PathBuf, Provider, String), String>;

#[derive(Default)]
pub(super) struct VcsWorkspace {
    integration: Integration,
    integration_project: PathBuf,
    integration_job: Option<std::sync::mpsc::Receiver<IntegrationResult>>,
    pub fetch_queue: std::collections::VecDeque<PathBuf>,
    pub commit: Option<PendingCommit>,
    pub retry_push: Option<(rustpy_vcs::PushRequest, Option<usize>, PathBuf)>,
    pub push_retried: bool,
    project: PathBuf,
    pub mappings: DirectoryMappings,
    detector: Option<Detector>,
    detected: Detection,
    path_input: String,
    version: Option<std::sync::mpsc::Receiver<String>>,
    last_incoming: f64,
    last_file: Option<PathBuf>,
}
impl IdeApp {
    pub(super) fn poll_vcs_mappings(&mut self, ctx: &egui::Context) {
        if let Some(result) = self
            .vcs_workspace
            .integration_job
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.vcs_workspace.integration_job = None;
            match result {
                Ok((_, _, message))
                    if self.project.as_ref().map(|p| &p.root)
                        != Some(&self.vcs_workspace.integration_project) =>
                {
                    self.set_status(format!("Previous project: {message}"))
                }
                Ok((root, provider, message)) => {
                    self.vcs_workspace
                        .mappings
                        .register_provider(root, provider);
                    if let Err(error) = self
                        .vcs_workspace
                        .mappings
                        .save(&self.vcs_workspace.project)
                    {
                        self.set_status(error.to_string());
                    } else {
                        self.set_status(message);
                    }
                    self.vcs_workspace.detector = Some(Detector::start(
                        self.vcs_workspace.project.clone(),
                        DetectionLimits::default(),
                    ));
                }
                Err(error) => self.set_status(format!(
                    "Cannot enable version control integration: {error}"
                )),
            }
        }
        if self.vcs_workspace.integration_job.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let committed = self
            .vcs_workspace
            .commit
            .as_ref()
            .and_then(|c| c.rx.try_recv().ok());
        if let Some(result) = committed {
            let pending = self.vcs_workspace.commit.take().unwrap();
            self.git_panel.git_jobs.job_finished(
                pending.id,
                result.is_ok(),
                result.as_ref().err().map(String::as_str).unwrap_or(""),
            );
            match result {
                Ok(id) => {
                    if self.git_panel.commit_workspace.loaded_root == pending.root
                        && self.git_panel.message.trim() == pending.message
                        && self.git_panel.amend == pending.amend
                    {
                        if self.git_panel.commit_workspace.clear_message_after_commit {
                            self.git_panel.message.clear();
                        }
                        self.git_panel.amend = false;
                    }
                    self.git_panel.last_refresh = -100.0;
                    if let Some(project) = &mut self.project {
                        project.git_refreshed = -100.0;
                    }
                    self.set_status(format!("Committed as {}", &id[..7.min(id.len())]));
                    self.task_commit_completed(pending.task_close);
                    if pending.push {
                        if let Some(repo) = GitRepo::discover(&pending.root) {
                            if pending.rewrote {
                                self.git_panel.ask_force_push(&repo);
                            } else if self
                                .project
                                .as_ref()
                                .and_then(|p| p.git.as_ref())
                                .is_some_and(|g| g.root() == pending.root)
                            {
                                self.quick_push();
                            } else {
                                self.set_status("Commit complete; select its repository to push");
                            }
                        }
                    }
                }
                Err(error) => {
                    self.git_panel.commit_workspace.failure = Some(error.clone());
                    self.report_git_status(format!("Commit failed: {error}"));
                }
            }
        }
        if self.vcs_workspace.commit.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        rustpy_vcs::policy::configure(&self.settings.vcs.preferences);
        let root = self
            .project
            .as_ref()
            .map(|p| p.root.clone())
            .unwrap_or_default();
        if self.vcs_workspace.project != root {
            let mut previous = std::mem::take(&mut self.vcs_workspace);
            self.vcs_workspace = VcsWorkspace {
                project: root.clone(),
                mappings: DirectoryMappings::load(&root),
                commit: previous.commit.take(),
                version: previous.version.take(),
                integration_job: previous.integration_job.take(),
                integration_project: previous.integration_project.clone(),
                integration: Integration {
                    root: root.clone(),
                    ..Default::default()
                },
                ..Default::default()
            };
            if !root.as_os_str().is_empty() {
                self.vcs_workspace.detector =
                    Some(Detector::start(root, DetectionLimits::default()));
            }
        }
        let result = self
            .vcs_workspace
            .detector
            .as_ref()
            .and_then(|d| d.receiver.try_recv().ok());
        if let Some(result) = result {
            self.vcs_workspace.detected = result;
            self.vcs_workspace.detector = None;
            // The enclosing root is the existing project integration; nested roots require consent.
            if self.vcs_workspace.mappings.roots.is_empty() {
                if let Some(git) = self.project.as_ref().and_then(|p| p.git.as_ref()) {
                    self.vcs_workspace
                        .mappings
                        .register(git.root().to_path_buf());
                }
            }
        }
        if let Some(message) = self
            .vcs_workspace
            .version
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.settings_ui.message = Some(message);
            self.vcs_workspace.version = None;
        }
        if self.vcs_workspace.detector.is_some() || self.vcs_workspace.version.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        let unregistered = self.vcs_workspace.detected.roots.iter().any(|r| {
            !self.vcs_workspace.mappings.roots.contains(&r.path)
                && !self.vcs_workspace.mappings.providers.contains_key(&r.path)
                && !self.vcs_workspace.mappings.ignored.contains(&r.path)
        });
        if unregistered && self.bottom != Some(BottomTool::Git) {
            egui::Window::new("Unregistered VCS root detected")
                .id(egui::Id::new("vcs_unregistered_roots"))
                .resizable(false)
                .show_modeless(ctx, |ui| self.vcs_roots_ui(ui));
        }
        let candidate = self
            .active
            .and_then(|i| self.tabs.get(i))
            .and_then(|t| t.state.path.as_ref());
        let changed_file = candidate != self.vcs_workspace.last_file.as_ref();
        let selected = if self.vcs_workspace.mappings.all_roots || changed_file {
            candidate
                .and_then(|p| self.vcs_workspace.mappings.root_for(p))
                .cloned()
                .or_else(|| self.vcs_workspace.mappings.selected.clone())
        } else {
            self.vcs_workspace.mappings.selected.clone()
        };
        self.vcs_workspace.last_file = candidate.cloned();
        if let (Some(project), Some(selected)) = (&mut self.project, selected) {
            self.vcs_workspace.mappings.selected = Some(selected.clone());
            if project.git.as_ref().is_none_or(|g| g.root() != selected) {
                project.git = GitRepo::discover(&selected);
                project.git_refreshed = -100.0;
                self.git_panel.last_refresh = -100.0;
            }
        }
        if self.vcs_workspace.mappings.roots.is_empty() {
            if let Some(git) = self.project.as_ref().and_then(|p| p.git.as_ref()) {
                if !self.vcs_workspace.mappings.ignored.contains(git.root()) {
                    self.vcs_workspace
                        .mappings
                        .register(git.root().to_path_buf());
                }
            }
        }
        let now = ctx.input(|i| i.time);
        // The trust check probes the disk: only once a fetch is due.
        if self.settings.vcs.preferences.check_incoming
            && now - self.vcs_workspace.last_incoming > 300.0
            && !self.git_panel.git_jobs.is_busy()
            && self.project.as_ref().is_some_and(|project| {
                rustpy_settings::trust_all_from_env()
                    || self.settings.is_project_trusted(&project.root)
            })
        {
            self.vcs_workspace.last_incoming = now;
            self.start_git_op(crate::git_jobs::GitOp::Fetch);
        }
    }
    pub(super) fn vcs_roots_ui(&mut self, ui: &mut egui::Ui) {
        if let Some(path) = self
            .active
            .and_then(|i| self.tabs.get(i))
            .and_then(|tab| tab.state.path.as_ref())
        {
            if let Some((root, provider)) = self.vcs_workspace.mappings.provider_for(path) {
                if provider != Provider::Git {
                    ui.label(format!("{} mapping: {}", provider.label(), root.display()));
                }
            }
        }
        let mut changed = false;
        let mappings = &mut self.vcs_workspace.mappings;
        ui.horizontal_wrapped(|ui| {
            changed |= ui.checkbox(&mut mappings.all_roots, "All roots").changed();
            for root in mappings.roots.clone() {
                let selected = mappings.selected.as_ref() == Some(&root);
                if ui
                    .selectable_label(
                        selected,
                        root.file_name()
                            .unwrap_or(root.as_os_str())
                            .to_string_lossy(),
                    )
                    .on_hover_text(root.display().to_string())
                    .clicked()
                {
                    mappings.selected = Some(root);
                    mappings.all_roots = false;
                    changed = true;
                }
            }
        });
        for root in self.vcs_workspace.detected.roots.clone() {
            if mappings.roots.contains(&root.path)
                || mappings.providers.contains_key(&root.path)
                || mappings.ignored.contains(&root.path)
            {
                continue;
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(format!(
                    "Unregistered {} root detected: {}{}",
                    root.provider.label(),
                    root.path.display(),
                    if root.linked_worktree {
                        " (worktree)"
                    } else if root.submodule {
                        " (submodule)"
                    } else {
                        ""
                    }
                ));
                if ui.button("Add root").clicked() {
                    mappings.register_provider(root.path.clone(), root.provider);
                    changed = true;
                }
                if ui.button("Ignore").clicked() {
                    mappings.ignored.insert(root.path.clone());
                    changed = true;
                }
            });
        }
        if self.vcs_workspace.detected.truncated {
            ui.label("Detection reached its directory, depth or time limit. Add further roots in Directory mappings.");
        }
        for (path, error) in &self.vcs_workspace.detected.errors {
            ui.colored_label(self.theme.warning, format!("{}: {error}", path.display()));
            if error.contains("safe") || error.contains("ownership") {
                ui.label("Git refused this directory's ownership. Verify its owner before explicitly adding its canonical path to your global Git safe.directory configuration. RustPy never trusts directories automatically.");
            }
        }
        if changed {
            if let Err(e) = mappings.save(&self.vcs_workspace.project) {
                self.settings_ui.message = Some(format!("Cannot save VCS mappings: {e}"));
            }
        }
    }
    pub(super) fn test_git_executable(&mut self) {
        let executable = self.settings.vcs.git_executable.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.vcs_workspace.version = Some(rx);
        std::thread::spawn(move || {
            let executable = if executable.trim().is_empty() {
                "git"
            } else {
                executable.trim()
            };
            let result = std::process::Command::new(executable)
                .arg("--version")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn();
            let text = match result {
                Err(e) => format!("Cannot run Git: {e}"),
                Ok(mut child) => {
                    let start = std::time::Instant::now();
                    loop {
                        match child.try_wait() {
                            Ok(Some(_)) => {
                                break match child.wait_with_output() {
                                    Ok(o) => format!(
                                        "{}{}",
                                        String::from_utf8_lossy(&o.stdout),
                                        String::from_utf8_lossy(&o.stderr)
                                    )
                                    .trim()
                                    .to_string(),
                                    Err(e) => e.to_string(),
                                }
                            }
                            Err(e) => break e.to_string(),
                            _ if start.elapsed() > std::time::Duration::from_secs(5) => {
                                let _ = child.kill();
                                let _ = child.wait();
                                break "Git version check timed out".into();
                            }
                            _ => std::thread::sleep(std::time::Duration::from_millis(25)),
                        }
                    }
                }
            };
            let _ = tx.send(text);
        });
    }
    pub(super) fn vcs_mapping_settings(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        ui.heading("Directory mappings");
        if !self.vcs_workspace.mappings.providers.is_empty()
            && ui
                .button("Open Mercurial / SVN / Perforce tool window")
                .clicked()
        {
            self.dialog = Dialog::None;
            self.bottom = Some(BottomTool::OtherVcs);
        }
        self.vcs_roots_ui(ui);
        ui.horizontal(|ui| {
            ui.label("VCS provider");
            for provider in Provider::ALL {
                ui.radio_value(
                    &mut self.vcs_workspace.integration.provider,
                    provider,
                    provider.label(),
                );
            }
        });
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut self.vcs_workspace.path_input);
            if ui.button("Add mapping").clicked() {
                let path = PathBuf::from(self.vcs_workspace.path_input.trim());
                let path = if path.is_absolute() {
                    path
                } else {
                    self.vcs_workspace.project.join(path)
                };
                let provider = self.vcs_workspace.integration.provider;
                if provider != Provider::Git
                    && (provider.detect(&path)
                        || (provider == Provider::Tfs
                            && path.is_dir()
                            && self.builtin_plugin_enabled("tfs")))
                {
                    self.vcs_workspace
                        .mappings
                        .register_provider(path, provider);
                    let _ = self
                        .vcs_workspace
                        .mappings
                        .save(&self.vcs_workspace.project);
                } else if provider == Provider::Git {
                    if let Some(repo) = GitRepo::discover(&path) {
                        self.vcs_workspace
                            .mappings
                            .register(repo.root().to_path_buf());
                        let _ = self
                            .vcs_workspace
                            .mappings
                            .save(&self.vcs_workspace.project);
                    } else {
                        self.settings_ui.message = Some(
                        "No Git repository at this path. Enable Version Control Integration first."
                            .into(),
                    );
                    }
                } else {
                    self.settings_ui.message = Some(format!(
                        "No {} working copy at this path; use Enable Version Control Integration",
                        provider.label()
                    ));
                }
            }
            if ui.button("Detect again").clicked() {
                self.vcs_workspace.detector = Some(Detector::start(
                    self.vcs_workspace.project.clone(),
                    DetectionLimits::default(),
                ));
            }
        });
        self.vcs_integration_settings(ui);
        for (root, provider) in self.vcs_workspace.mappings.providers.clone() {
            ui.horizontal(|ui| {
                ui.label(format!("{} — {}", root.display(), provider.label()));
                if ui.small_button("Remove mapping").clicked() {
                    self.vcs_workspace.mappings.providers.remove(&root);
                    let _ = self
                        .vcs_workspace
                        .mappings
                        .save(&self.vcs_workspace.project);
                }
            });
        }
        for root in self.vcs_workspace.mappings.roots.clone() {
            ui.horizontal(|ui| {
                ui.label(root.display().to_string());
                if ui.small_button("Remove mapping").clicked() {
                    self.vcs_workspace.mappings.roots.remove(&root);
                    if self.vcs_workspace.mappings.selected.as_ref() == Some(&root) {
                        self.vcs_workspace.mappings.selected =
                            self.vcs_workspace.mappings.roots.first().cloned();
                    }
                    let _ = self
                        .vcs_workspace
                        .mappings
                        .save(&self.vcs_workspace.project);
                }
            });
        }
        for root in self.vcs_workspace.mappings.ignored.clone() {
            if ui
                .button(format!("Stop ignoring {}", root.display()))
                .clicked()
            {
                self.vcs_workspace.mappings.ignored.remove(&root);
                let _ = self
                    .vcs_workspace
                    .mappings
                    .save(&self.vcs_workspace.project);
            }
        }
        let p = &mut self.settings.vcs.preferences;
        ui.separator();
        ui.heading("Authentication and SSH");
        ui.horizontal(|ui| {
            ui.label("SSH transport");
            ui.radio_value(&mut p.ssh_transport, "native".into(), "Native OpenSSH");
            ui.radio_value(
                &mut p.ssh_transport,
                "builtin".into(),
                "Built-in libgit2 / libssh2",
            );
        });
        ui.checkbox(&mut p.credential_helper, "Use credential helper");
        ui.label("HTTPS tokens and SSH key passphrases use the Git ask-pass dialog. SSH inherits SSH_AUTH_SOCK and the operating system agent; host verification stays enabled.");
        ui.horizontal(|ui| {
            ui.label("SSH executable (empty = system SSH)");
            ui.text_edit_singleline(&mut p.ssh_executable);
        });
        ui.horizontal(|ui| {
            ui.label("SSH private key");
            ui.text_edit_singleline(&mut p.ssh_key);
        });
        if ui
            .button("GitHub accounts, tokens and OAuth device sign-in…")
            .clicked()
        {
            self.settings_ui.page = SettingsPage::GitHub;
        }
        ui.separator();
        ui.heading("Commit");
        ui.horizontal(|ui| {
            ui.label("core.autocrlf");
            for (value, label) in [
                ("inherit", "Git configuration"),
                ("true", "Checkout CRLF, commit LF"),
                ("input", "Commit LF"),
                ("false", "No conversion"),
            ] {
                ui.radio_value(&mut p.autocrlf, value.into(), crate::i18n::tr_static(label));
            }
        });
        ui.horizontal(|ui| {
            ui.label("Protected branches (comma-separated glob patterns)");
            ui.text_edit_singleline(&mut p.protected_branches);
        });
        ui.checkbox(
            &mut p.warn_detached,
            "Warn when committing in detached HEAD",
        );
        ui.checkbox(
            &mut p.warn_crlf,
            "Warn if CRLF line separators are about to be committed",
        );
        ui.checkbox(
            &mut p.fix_crlf,
            "Fix CRLF to LF before committing selected working files",
        );
        ui.checkbox(&mut p.sign_off, "Add Signed-off-by");
        ui.checkbox(&mut p.sign_commits, "Sign commits");
        ui.horizontal(|ui| {
            ui.label("Signing key");
            ui.text_edit_singleline(&mut p.signing_key);
        });
        ui.horizontal(|ui| {
            ui.radio_value(&mut p.signing_format, "openpgp".into(), "OpenPGP");
            ui.radio_value(&mut p.signing_format, "ssh".into(), "SSH signing");
        });
        ui.checkbox(
            &mut p.staging_area,
            "Enable staging area (commit the index)",
        );
        ui.checkbox(&mut p.non_modal_commit, "Use non-modal commit interface");
        ui.checkbox(&mut p.show_status_branch, "Show Git branches in status bar");
        ui.checkbox(
            &mut p.synchronize_branches,
            "Synchronize branches by name between repositories",
        );
        ui.checkbox(
            &mut p.cherry_pick_commit,
            "Commit automatically on cherry-pick",
        );
        ui.checkbox(
            &mut p.auto_update_rejected_push,
            "Auto-update if push rejected",
        );
        ui.checkbox(
            &mut p.check_incoming,
            "Explicitly check for incoming commits on remotes (every five minutes)",
        );
        ui.horizontal(|ui| {
            ui.label("Load shallow history depth (0 = complete)");
            ui.add(egui::DragValue::new(&mut p.shallow_depth).range(0..=1_000_000));
        });
        ui.horizontal(|ui| {
            ui.label("Filter Update Project information by paths");
            ui.text_edit_singleline(&mut p.update_path_filter);
        });
    }

    fn vcs_integration_settings(&mut self, ui: &mut egui::Ui) {
        ui.collapsing("Enable Version Control Integration…",|ui| {
            ui.label("Uses the directory above, or the project directory when it is empty. Existing files are preserved.");
            let plan=&mut self.vcs_workspace.integration;
            if plan.provider==Provider::Tfs {
                plan.create_local=false;
                ui.label("Optional TFVC adapter: configure Visual Studio tf.exe and sign in with the vendor client first. No passwords are passed in command arguments.");
                for (label,value) in [("HTTPS collection URL",&mut plan.endpoint),("Workspace name",&mut plan.workspace),("Server folder ($/Project/Main)",&mut plan.depot)] {ui.horizontal(|ui|{ui.label(crate::i18n::tr_static(label));ui.text_edit_singleline(value);});}
            }
            if matches!(plan.provider,Provider::Subversion|Provider::Perforce) {
                ui.checkbox(&mut plan.create_local,"Create a local repository / personal server");
                if plan.provider==Provider::Subversion && plan.create_local {
                    let mut value=plan.repository.to_string_lossy().into_owned();
                    ui.horizontal(|ui| {ui.label("SVN repository directory (outside the checkout)");if ui.text_edit_singleline(&mut value).changed(){plan.repository=PathBuf::from(value.clone());}});
                } else if !plan.create_local {
                    ui.horizontal(|ui| {ui.label(if plan.provider==Provider::Subversion{"SVN repository URL"}else{"Perforce server"});ui.text_edit_singleline(&mut plan.endpoint);});
                    if plan.provider==Provider::Perforce {
                        for (label,value) in [("Workspace",&mut plan.workspace),("User",&mut plan.user),("Depot view",&mut plan.depot)] {ui.horizontal(|ui|{ui.label(crate::i18n::tr_static(label));ui.text_edit_singleline(value);});}
                    }
                }
            }
            let preferences=&mut self.settings.vcs.preferences;
            let executable=match plan.provider {Provider::Git=>&mut self.settings.vcs.git_executable,Provider::Mercurial=>&mut preferences.hg_executable,Provider::Subversion=>&mut preferences.svn_executable,Provider::Perforce=>&mut preferences.p4_executable,Provider::Tfs=>&mut preferences.tf_executable};
            ui.horizontal(|ui|{ui.label(if plan.provider==Provider::Tfs{"Absolute Visual Studio tf.exe path (required)"}else{"Provider executable (empty = PATH)"});ui.text_edit_singleline(executable);});
            if ui.add_enabled(self.vcs_workspace.integration_job.is_none(),egui::Button::new(format!("Enable {} integration",plan.provider.label()))).clicked() {
                let mut request=plan.clone();request.executable=executable.clone();
                request.root=if self.vcs_workspace.path_input.trim().is_empty(){self.vcs_workspace.project.clone()}else{PathBuf::from(self.vcs_workspace.path_input.trim())};
                if !request.root.is_absolute(){request.root=self.vcs_workspace.project.join(request.root);}
                if request.provider==Provider::Tfs { self.enable_tfs_integration(request); return; }
                let (tx,rx)=std::sync::mpsc::channel();self.vcs_workspace.integration_job=Some(rx);self.vcs_workspace.integration_project=self.vcs_workspace.project.clone();
                std::thread::spawn(move || {let result=request.enable().map(|message|(request.root.clone(),request.provider,message)).map_err(|e|e.to_string());let _=tx.send(result);});
            }
            if self.vcs_workspace.integration_job.is_some(){ui.label("Enabling integration…");}
        });
    }
}
