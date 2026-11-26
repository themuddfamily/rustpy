//! Deployment options, owned transfer console and captured Remote Host operations.
use super::*;
use crate::a11y::DialogWindow;
#[path = "deployment_targets.rs"]
mod targets;
#[derive(Default)]
pub(super) struct Workspace {
    pub(super) external: std::collections::BTreeMap<(PathBuf, PathBuf), (bool, std::time::Instant)>,
    pub(super) remote_conflict: Option<(PathBuf, Server, String, PathBuf, PathBuf)>,
    pub(super) recent_saves: HashMap<PathBuf, std::time::Instant>,
    pub(super) tools: Option<targets::State>,
    pub(super) rename: String,
    pub(super) permissions: String,
    pub(super) external_command: String,
    pub(super) confirmation: Option<(PathBuf, Server, String)>,
    pub(super) upload_choice: Option<(PathBuf, Vec<PathBuf>, bool)>,
    pub(super) newer: Option<(PathBuf, Server, Vec<PathBuf>, Vec<String>)>,
}
impl DeployUi {
    pub(super) fn transfer_console(&mut self, ctx: &egui::Context) {
        if !self.console_open && self.jobs.is_empty() {
            return;
        }
        let mut open = true;
        egui::Window::new("File Transfer — Deployment Console")
            .open(&mut open)
            .default_size([720.0, 260.0])
            .show_modeless(ctx, |ui| {
                for job in &self.jobs {
                    ui.horizontal(|ui| {
                        ui.label(format!("{} · {}", job.label, job.owner.root.display()));
                        if ui.button("Cancel").clicked() {
                            job.owner.cancel();
                        }
                    });
                }
                ui.horizontal(|ui| {
                    if ui.button("Clear log").clicked() {
                        self.log.clear();
                    }
                    if ui.button("Copy log").clicked() {
                        ui.ctx().copy_text(self.log.join("\n"));
                    }
                });
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for line in &self.log {
                            ui.monospace(line);
                        }
                    });
            });
        self.console_open = open;
    }
    pub(super) fn server_options(ui: &mut egui::Ui, server: &mut Server) {
        ui.collapsing("Deployment Options", |ui| {
            let options = &mut server.options;
            for (label, value) in [
                (
                    "Upload external changes",
                    &mut options.upload_external_changes,
                ),
                (
                    "Delete remote files when local deleted",
                    &mut options.delete_remote_on_local_delete,
                ),
                ("Preserve timestamps", &mut options.preserve_timestamps),
                (
                    "Warn when uploading over newer file",
                    &mut options.warn_newer,
                ),
                (
                    "Notify about remote changes",
                    &mut options.notify_remote_changes,
                ),
            ] {
                ui.checkbox(value, crate::i18n::tr_static(label));
            }
            ui.horizontal(|ui| {
                ui.label("Timeout seconds");
                ui.add(egui::DragValue::new(&mut options.timeout_seconds).range(5..=3600));
            });
            ui.label("Exclude paths / items by name (glob patterns, one per line)");
            let mut excluded = options.excluded.join("\n");
            if ui.text_edit_multiline(&mut excluded).changed() {
                options.excluded = excluded
                    .lines()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
            }
            if matches!(server.kind, ServerKind::Ftp { .. }) {
                egui::ComboBox::from_label("FTP security")
                    .selected_text(&options.ftp_tls)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut options.ftp_tls, "plain".into(), "FTP");
                        ui.selectable_value(
                            &mut options.ftp_tls,
                            "explicit".into(),
                            "FTPS — explicit TLS",
                        );
                        ui.selectable_value(
                            &mut options.ftp_tls,
                            "implicit".into(),
                            "FTPS — implicit TLS",
                        );
                    });
                ui.checkbox(&mut options.passive_ftp, "Passive mode");
                for (label, value) in [
                    ("Trusted CA certificate", &mut options.tls_ca_file),
                    ("Client certificate", &mut options.tls_client_certificate),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(crate::i18n::tr_static(label));
                        ui.text_edit_singleline(value);
                    });
                }
            }
            if matches!(
                server.kind,
                ServerKind::Sftp { .. } | ServerKind::Rsync { .. }
            ) {
                egui::ComboBox::from_label("SSH authentication")
                    .selected_text(&options.ssh_auth)
                    .show_ui(ui, |ui| {
                        for value in ["password", "key", "agent", "config"] {
                            ui.selectable_value(
                                &mut options.ssh_auth,
                                value.into(),
                                crate::i18n::tr_static(value),
                            );
                        }
                    });
                for (label, value) in [
                    ("OpenSSH configuration file", &mut options.ssh_config_file),
                    ("OpenSSH Host alias", &mut options.ssh_alias),
                    ("Jump hosts (ProxyJump)", &mut options.jump_host),
                    ("Remote Python executable", &mut options.remote_python),
                    ("Helpers directory", &mut options.helper_directory),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(crate::i18n::tr_static(label));
                        ui.text_edit_singleline(value);
                    });
                }
                ui.checkbox(
                    &mut options.sudo,
                    "Execute remote interpreter using sudo -n",
                );
                // Old persisted auto-accept flags no longer grant host trust.
                options.accept_new_host_keys = false;
                ui.label("Unknown SSH keys require confirmation; changed keys are refused.");
            }
            ui.collapsing("Tool executable paths", |ui| {
                for (label, value) in [
                    ("ssh", &mut options.ssh_executable),
                    ("scp", &mut options.scp_executable),
                    ("curl", &mut options.curl_executable),
                    ("rsync", &mut options.rsync_executable),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(crate::i18n::tr_static(label));
                        ui.text_edit_singleline(value);
                    });
                }
            });
        });
    }
    pub(super) fn remote_operation(
        &mut self,
        root: &Path,
        server: Server,
        remote: String,
        action: &str,
    ) {
        self.flush_external_changes();
        let root = root.to_path_buf();
        let action = action.to_string();
        let rename = self.extra.rename.clone();
        let permissions = self.extra.permissions.clone();
        if action == "Delete" {
            self.extra.confirmation = Some((root, server, remote));
            return;
        }
        self.spawn(format!("{action} {remote}"), move || {
            remote_operation_result(root, server, remote, action, rename, permissions)
        });
    }
    fn flush_external_changes(&mut self) {
        if self.background_paused {
            return;
        }
        if self.jobs.len() < 4 {
            let ready = self
                .extra
                .external
                .iter()
                .filter(|(_, (_, time))| time.elapsed() >= std::time::Duration::from_millis(250))
                .take(4 - self.jobs.len())
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            for (root, path) in ready {
                if let Some((removed, _)) =
                    self.extra.external.remove(&(root.clone(), path.clone()))
                {
                    self.process_external_change(&root, &path, removed);
                }
            }
        }
    }
}
fn remote_operation_result(
    root: PathBuf,
    server: Server,
    remote: String,
    action: String,
    rename: String,
    permissions: String,
) -> JobResult {
    let result = (|| -> anyhow::Result<JobResult> {
        let t = transport(&server)?;
        match action.as_str() {
            "Rename" => {
                anyhow::ensure!(
                    !rename.is_empty()
                        && !rename.contains(['/', '\\', '\0', '\r', '\n'])
                        && rename != "."
                        && rename != "..",
                    "Enter a single file name"
                );
                let destination = rustpy_deploy::join_remote(
                    &rustpy_deploy::remote_parent(&remote).unwrap_or_default(),
                    &rename,
                );
                t.rename(&remote, &destination)?;
            }
            "Permissions" => {
                let mode = u32::from_str_radix(permissions.trim(), 8)?;
                t.permissions(&remote, mode)?;
            }
            "Upload local" => {
                let local = rustpy_deploy::local_path(&server, &root, &remote)
                    .ok_or_else(|| anyhow::anyhow!("No local mapping for {remote}"))?;
                t.upload(&local, &remote)?;
            }
            "Edit" | "Compare with local" => {
                let local = rustpy_deploy::local_path(&server, &root, &remote);
                let cache = remote_cache(&root, &server.name, &remote);
                if let Some(parent) = cache.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                t.download(&remote, &cache)?;
                if action == "Edit" {
                    anyhow::ensure!(
                        std::fs::metadata(&cache)?.len() <= 8 * 1024 * 1024,
                        "Remote file is too large for editing"
                    );
                    std::fs::read_to_string(&cache)?;
                    let baseline = content_hash(&std::fs::read(&cache)?);
                    return Ok(JobResult::Edited(server, root, remote, cache, baseline));
                }
                return Ok(JobResult::Compare(
                    local.ok_or_else(|| anyhow::anyhow!("No local mapping for {remote}"))?,
                    cache,
                ));
            }
            _ => anyhow::bail!("Unknown remote operation"),
        }
        Ok(JobResult::Message(format!("{action}: {remote}"), false))
    })();
    match result {
        Ok(result) => result,
        Err(error) => JobResult::Message(format!("{action} failed: {error}"), false),
    }
}
impl DeployUi {
    pub(super) fn workspace_windows(
        &mut self,
        ctx: &egui::Context,
        theme: &UiTheme,
        events: &mut Vec<DeployEvent>,
    ) {
        self.flush_external_changes();
        if let Some((root, server, remote, local, comparison)) = self.extra.remote_conflict.clone()
        {
            let mut choice = None;
            egui::Window::new("Remote file changed").show_modal(ctx, |ui| {
                ui.colored_label(
                    theme.warning,
                    format!("{remote} changed since it was opened"),
                );
                if ui.button("Compare").clicked() {
                    events.push(DeployEvent::Compare(local.clone(), comparison.clone()));
                }
                if ui.button("Overwrite remote").clicked() {
                    choice = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(false);
                }
            });
            if let Some(overwrite) = choice {
                self.extra.remote_conflict = None;
                if overwrite {
                    self.save_remote_edit(&root, server, remote, local, 0, true);
                }
            }
        }
        if let Some((root, server, path)) = self.extra.confirmation.clone() {
            let mut answer = None;
            egui::Window::new("Delete remote item?")
                .collapsible(false)
                .show_modal(ctx, |ui| {
                    ui.colored_label(
                        theme.warning,
                        format!("Delete {path} from {}?", server.name),
                    );
                    if ui.button("Delete").clicked() {
                        answer = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        answer = Some(false);
                    }
                });
            if let Some(delete) = answer {
                self.extra.confirmation = None;
                if delete {
                    self.capture_root(&root);
                    self.spawn(format!("Delete {path}"), move || {
                        match transport(&server).and_then(|t| t.delete(&path)) {
                            Ok(()) => JobResult::Message(format!("Deleted {path}"), false),
                            Err(error) => {
                                JobResult::Message(format!("Delete failed: {error}"), false)
                            }
                        }
                    });
                }
            }
        }
        if let Some((root, paths, download)) = self.extra.upload_choice.clone() {
            let mut choice = None;
            let mut open = true;
            egui::Window::new(if download {
                "Download from…"
            } else {
                "Upload to…"
            })
            .open(&mut open)
            .show_modal(ctx, |ui| {
                for server in DeploymentConfig::load_or_default(&root).servers {
                    if ui.button(&server.name).clicked() {
                        choice = Some(server);
                    }
                }
            });
            if let Some(server) = choice {
                self.extra.upload_choice = None;
                self.transfer_selected(&root, server, paths, download, false);
            } else if !open {
                self.extra.upload_choice = None;
            }
        }
        if let Some((root, server, paths, warnings)) = self.extra.newer.clone() {
            let mut answer = None;
            egui::Window::new("Newer deployed files").show_modal(ctx, |ui| {
                ui.colored_label(
                    theme.warning,
                    "These deployed files are newer. Overwrite them?",
                );
                for warning in &warnings {
                    ui.label(warning);
                }
                if ui.button("Overwrite").clicked() {
                    answer = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(false);
                }
            });
            if let Some(overwrite) = answer {
                self.extra.newer = None;
                if overwrite {
                    self.transfer_selected(&root, server, paths, false, true);
                }
            }
        }
        let _ = events;
    }
}
fn remote_cache(root: &Path, server: &str, remote: &str) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    (server, remote).hash(&mut hash);
    let name = rustpy_deploy::remote_file_name(remote);
    root.join(".rustpy").join("remote-edit").join(format!(
        "{:016x}-{}",
        hash.finish(),
        name.replace(['/', '\\'], "_")
    ))
}
impl DeployUi {
    pub(super) fn transfer_selected(
        &mut self,
        root: &Path,
        server: Server,
        paths: Vec<PathBuf>,
        download: bool,
        overwrite: bool,
    ) {
        self.capture_root(root);
        let root = root.to_path_buf();
        let mut excluded = DeploymentConfig::load_or_default(&root).excluded;
        excluded.extend(server.options.excluded.clone());
        self.spawn(
            format!(
                "{} {} item(s) {} {}",
                if download { "Downloading" } else { "Uploading" },
                paths.len(),
                if download { "from" } else { "to" },
                server.name
            ),
            move || {
                let operation = (|| -> anyhow::Result<JobResult> {
                    let t = transport(&server)?;
                    let files = if download {
                        paths.clone()
                    } else {
                        rustpy_deploy::collect_files(&root, &paths, &excluded)
                    };
                    if !download && server.options.warn_newer && !overwrite {
                        let mut warnings = Vec::new();
                        for file in &files {
                            rustpy_deploy::transfer::check()?;
                            if let Some(remote) = rustpy_deploy::remote_path(&server, &root, file) {
                                let parent =
                                    rustpy_deploy::remote_parent(&remote).unwrap_or_default();
                                if let Ok(entries) = t.list(&parent) {
                                    if let Some(entry) = entries.iter().find(|entry| {
                                        entry.name == rustpy_deploy::remote_file_name(&remote)
                                    }) {
                                        let local = std::fs::metadata(file)?
                                            .modified()?
                                            .duration_since(std::time::UNIX_EPOCH)?
                                            .as_secs()
                                            as i64;
                                        if entry.mtime.is_some_and(|mtime| mtime > local + 2) {
                                            warnings.push(remote);
                                        }
                                    }
                                }
                            }
                        }
                        if !warnings.is_empty() {
                            return Ok(JobResult::Newer(root, server, paths, warnings));
                        }
                    }
                    let progress = |current, total, path: &str| {
                        if let Some(owner) = rustpy_deploy::transfer::current().owner {
                            owner.progress(current, total, path, "Transferring");
                        }
                    };
                    let results = if download {
                        rustpy_deploy::download_paths(
                            t.as_ref(),
                            &server,
                            &root,
                            &files,
                            &excluded,
                            progress,
                        )
                    } else {
                        rustpy_deploy::upload_paths(t.as_ref(), &server, &root, &files, progress)
                    };
                    let failures = results
                        .iter()
                        .filter_map(|(path, result)| {
                            result
                                .as_ref()
                                .err()
                                .map(|error| format!("{}: {error}", path.display()))
                        })
                        .collect::<Vec<_>>();
                    if !failures.is_empty() {
                        anyhow::bail!("{}", failures.join("; "));
                    }
                    rustpy_deploy::transfer::check()?;
                    Ok(JobResult::Message(
                        format!(
                            "{} {} file(s) {} {}",
                            if download { "Downloaded" } else { "Uploaded" },
                            results.len(),
                            if download { "from" } else { "to" },
                            server.name
                        ),
                        download,
                    ))
                })();
                match operation {
                    Ok(result) => result,
                    Err(error) => JobResult::Message(format!("Transfer failed: {error}"), false),
                }
            },
        );
    }
    fn process_external_change(&mut self, root: &Path, path: &Path, removed: bool) {
        if self
            .extra
            .recent_saves
            .get(path)
            .is_some_and(|time| time.elapsed() < std::time::Duration::from_secs(2))
        {
            return;
        }
        self.extra
            .recent_saves
            .retain(|_, time| time.elapsed() < std::time::Duration::from_secs(3));
        let config = DeploymentConfig::load_or_default(root);
        let Some(server) = config.default_server_entry().cloned() else {
            return;
        };
        if (removed && config.auto_upload == AutoUpload::Never)
            || (!removed
                && (!server.options.upload_external_changes
                    || config.auto_upload != AutoUpload::Always))
        {
            return;
        }
        if config.is_excluded(&path.strip_prefix(root).unwrap_or(path).to_string_lossy())
            || rustpy_deploy::is_excluded(
                &server.options.excluded,
                &path.strip_prefix(root).unwrap_or(path).to_string_lossy(),
            )
        {
            return;
        }
        if removed {
            if !server.options.delete_remote_on_local_delete {
                return;
            }
            let Some(remote) = rustpy_deploy::remote_path(&server, root, path) else {
                return;
            };
            self.capture_root(root);
            self.spawn(
                format!("Remove deployed {remote}"),
                move || match transport(&server).and_then(|t| t.delete(&remote)) {
                    Ok(()) => JobResult::Message(format!("Removed deployed {remote}"), false),
                    Err(error) => {
                        JobResult::Message(format!("Remote delete failed: {error}"), false)
                    }
                },
            );
        } else if path.is_file() {
            self.capture_root(root);
            self.transfer_selected(root, server, vec![path.into()], false, false);
        }
    }
}
impl DeployUi {
    pub fn prepare_interpreter(&mut self, root: &Path, server: Server) {
        self.capture_root(root);
        let root = root.to_path_buf();
        self.spawn(
            format!("Prepare SSH interpreter on {}", server.name),
            move || {
                let result = (|| -> anyhow::Result<rustpy_project::Interpreter> {
                    let wrapper = rustpy_deploy::ssh_tools::install_interpreter(&server, &root)?;
                    let command = rustpy_deploy::ssh_tools::python(
                        &server,
                        &[
                            "-I".into(),
                            "-c".into(),
                            rustpy_project::interpreter::PROBE.into(),
                        ],
                    )?;
                    let mut context = rustpy_deploy::transfer::current();
                    context.options = server.options.clone();
                    let output = rustpy_deploy::transfer::with_context(context, || {
                        rustpy_deploy::exec::run_checked(
                            &command,
                            std::time::Duration::from_secs(30),
                            |_| None,
                        )
                    })?;
                    let mut interpreter = rustpy_project::interpreter::decode_metadata(
                        &wrapper,
                        "SSH deployment",
                        &output.stdout,
                    )
                    .map_err(anyhow::Error::msg)?
                    .interpreter;
                    // Only local mirror paths enter the local index; sys.path remains remote at runtime.
                    interpreter.sys_path = server
                        .effective_mappings()
                        .iter()
                        .map(|mapping| rustpy_deploy::mapping_local_root(&root, mapping))
                        .collect();
                    Ok(interpreter)
                })();
                match result {
                    Ok(interpreter) => JobResult::InterpreterReady(interpreter, root),
                    Err(error) => {
                        JobResult::Message(format!("SSH interpreter failed: {error}"), false)
                    }
                }
            },
        );
    }
    pub(super) fn ssh_configuration_controls(
        &mut self,
        root: &Path,
        server: Server,
        action: &str,
        events: &mut Vec<DeployEvent>,
    ) {
        match action {
            "SSH terminal" => match rustpy_deploy::ssh_tools::terminal(&server) {
                Ok(command) => events.push(DeployEvent::Terminal(
                    format!("SSH {}", server.name),
                    command.program,
                    command.args,
                    root.into(),
                )),
                Err(error) => events.push(DeployEvent::Status(error.to_string())),
            },
            "Use SSH interpreter" => {
                events.push(DeployEvent::Interpreter(Box::new(server), root.into()))
            }
            "Sync folders" => {
                let config = DeploymentConfig::load_or_default(root);
                self.sync_dialog = Some(SyncState {
                    captured: server.clone(),
                    server: server.name.clone(),
                    direction: SyncDirection::Both,
                    items: None,
                    error: None,
                });
                self.plan(root, server, config.excluded, SyncDirection::Both);
            }
            "Upload helpers" => {
                let root = root.to_path_buf();
                self.spawn(format!("Upload helpers to {}",server.name),move||{let operation=(||->anyhow::Result<()>{let t=transport(&server)?;let local=root.join(".rustpy").join("deployment-helper.py");std::fs::create_dir_all(local.parent().unwrap())?;std::fs::write(&local,"import json, os, sys\ndef metadata():\n    return {'cwd': os.getcwd(), 'path': sys.path, 'executable': sys.executable}\nif __name__ == '__main__':\n    print(json.dumps(metadata()))\n")?;let directory=rustpy_deploy::join_remote(&server.root_path,&server.options.helper_directory);t.mkdirs(&directory)?;t.upload(&local,&rustpy_deploy::join_remote(&directory,"rustpy_deployment.py"))?;Ok(())})();match operation{Ok(())=>JobResult::Message("Remote interpreter helpers uploaded".into(),false),Err(error)=>JobResult::Message(format!("Helper upload failed: {error}"),false)}});
            }
            "Run SSH external tool" => {
                let command = self.extra.external_command.clone();
                self.spawn(format!("SSH external tool on {}", server.name), move || {
                    let result =
                        rustpy_deploy::ssh_tools::external(&server, &command).and_then(|command| {
                            let mut context = rustpy_deploy::transfer::current();
                            context.options = server.options.clone();
                            Ok(rustpy_deploy::transfer::with_context(context, || {
                                rustpy_deploy::exec::run_checked(
                                    &command,
                                    std::time::Duration::from_secs(server.options.timeout_seconds),
                                    |_| None,
                                )
                            })?)
                        });
                    match result {
                        Ok(output) => JobResult::Message(output.stdout_text(), false),
                        Err(error) => {
                            JobResult::Message(format!("SSH tool failed: {error}"), false)
                        }
                    }
                });
            }
            _ => {}
        }
    }
}
impl DeployUi {
    pub(super) fn parse_ssh_configuration(&mut self, server: Server) {
        self.spawn(format!("Read SSH config for {}", server.name), move || {
            let alias = if server.options.ssh_alias.is_empty() {
                match &server.kind {
                    ServerKind::Sftp { host, .. } | ServerKind::Rsync { host, .. } => host.clone(),
                    _ => String::new(),
                }
            } else {
                server.options.ssh_alias.clone()
            };
            let file = if server.options.ssh_config_file.is_empty() {
                PathBuf::from(
                    std::env::var_os("HOME")
                        .or_else(|| std::env::var_os("USERPROFILE"))
                        .unwrap_or_default(),
                )
                .join(".ssh")
                .join("config")
            } else {
                server.options.ssh_config_file.clone().into()
            };
            match rustpy_deploy::ssh_config::resolve(&file, &alias) {
                Ok(resolved) => JobResult::SshResolved(server.name, resolved),
                Err(error) => JobResult::Message(format!("SSH config failed: {error}"), false),
            }
        });
    }
}
fn content_hash(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hash);
    hash.finish()
}
impl DeployUi {
    pub fn note_local_save(&mut self, path: &Path) {
        self.extra
            .recent_saves
            .insert(path.into(), std::time::Instant::now());
    }
    pub fn remote_edit_saved(&mut self, root: &Path, path: &Path) -> bool {
        if self
            .remote_edits
            .get(path)
            .is_some_and(|binding| binding.0 == root)
        {
            self.on_saved(root, path, true);
            true
        } else {
            false
        }
    }
    pub(super) fn save_remote_edit(
        &mut self,
        root: &Path,
        server: Server,
        remote: String,
        local: PathBuf,
        baseline: u64,
        overwrite: bool,
    ) {
        let root = root.to_path_buf();
        self.spawn(format!("Save remote edit {remote}"), move || {
            let operation = (|| -> anyhow::Result<JobResult> {
                let t = transport(&server)?;
                let saved = std::fs::read(&local)?;
                let saved_hash = content_hash(&saved);
                if server.options.warn_newer && !overwrite {
                    let check =
                        remote_cache(&root, &format!("{}-remote-version", server.name), &remote);
                    t.download(&remote, &check)?;
                    let remote_hash = content_hash(&std::fs::read(&check)?);
                    if remote_hash != baseline && remote_hash != saved_hash {
                        return Ok(JobResult::RemoteConflict(
                            root, server, remote, local, check,
                        ));
                    }
                }
                let id = rustpy_deploy::transfer::current()
                    .owner
                    .map_or(0, |owner| owner.id);
                let snapshot = root
                    .join(".rustpy/remote-edit")
                    .join(format!("upload-{id}"));
                std::fs::write(&snapshot, &saved)?;
                struct Remove(PathBuf);
                impl Drop for Remove {
                    fn drop(&mut self) {
                        let _ = std::fs::remove_file(&self.0);
                    }
                }
                let _remove = Remove(snapshot.clone());
                t.upload(&snapshot, &remote)?;
                Ok(JobResult::RemoteSaved(local, saved_hash))
            })();
            match operation {
                Ok(result) => result,
                Err(error) => JobResult::Message(format!("Remote save failed: {error}"), false),
            }
        });
    }
}
impl DeployUi {
    pub fn on_external_change(&mut self, root: &Path, path: &Path, removed: bool) {
        if self.background_paused {
            return;
        }
        if self.extra.external.len() >= 10000 {
            if !self.log.last().is_some_and(|line| {
                line == "External upload queue full; use Sync with Deployed to reconcile"
            }) {
                self.log
                    .push("External upload queue full; use Sync with Deployed to reconcile".into());
            }
            return;
        }
        self.extra.external.insert(
            (root.into(), path.into()),
            (removed, std::time::Instant::now()),
        );
    }
}
#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn remote_edit_versions_and_cache_owners_are_distinct() {
        assert_ne!(content_hash(b"before"), content_hash(b"after"));
        let root = Path::new("/project");
        assert_ne!(
            remote_cache(root, "server-a", "/srv/a.py"),
            remote_cache(root, "server-b", "/srv/a.py")
        );
        assert!(remote_cache(root, "server-a", "/srv/a.py")
            .starts_with(root.join(".rustpy/remote-edit")));
    }
    #[test]
    fn local_save_events_coalesce_before_external_upload() {
        let mut ui = DeployUi::default();
        ui.root = Some("/project".into());
        ui.on_external_change(Path::new("/project"), Path::new("/project/a.py"), false);
        ui.on_external_change(Path::new("/project"), Path::new("/project/a.py"), true);
        assert_eq!(ui.extra.external.len(), 1);
        assert!(ui.extra.external.values().next().unwrap().0);
        assert!(ui.pending_transfers().iter().any(|(id, _, _)| *id == 0));
        assert!(ui.stop_transfer(0));
        assert!(ui.extra.external.is_empty());
    }
}
