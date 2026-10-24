//! Tools › Deployment: server configuration, upload/download, Sync with
//! Deployed, automatic upload on save and a remote host browser, on top of
//! `rustpy-deploy`. Every transfer runs on a worker thread.
//!
//! FTP passwords live in the password safe (`rustpy-secrets`, key
//! `deployment/ftp://user@host:port`), never in `.rustpy/deployment.json`:
//! the configuration dialog reads them from the safe and stores them on Save
//! ("Save password" off keeps one for the session only), transfers look them
//! up on their worker thread, and passwords that older versions wrote into
//! deployment.json are moved into the safe ([`migrate_plaintext_passwords`]).
//! When the password database is locked, these worker threads ask for the
//! master password and wait for the answer ([`crate::netsec::wait_for_unlock`]);
//! a password the safe does not take is kept for the session and stored once
//! the database is unlocked, never written into deployment.json.

use crate::a11y::DialogWindow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};

use egui::{Align2, RichText, Vec2};
use rustpy_deploy::{
    AutoUpload, DeploymentConfig, Mapping, RemoteEntry, Server, ServerKind, SyncAction,
    SyncDirection, SyncItem,
};
use rustpy_secrets::SafeError;

use crate::a11y::AccessibleCombo;
use crate::netsec::{self, SecretOp};
use crate::theme::UiTheme;
#[path = "deployment_kubernetes.rs"]
mod kubernetes;
#[path = "deployment_workspace.rs"]
mod workspace;

pub enum DeployEvent {
    DockerDebug(rustpy_deploy::CommandSpec, String, u16, String, PathBuf),
    OpenFile(PathBuf),
    Compare(PathBuf, PathBuf),
    Terminal(String, String, Vec<String>, PathBuf),
    Interpreter(Box<Server>, PathBuf),
    InterpreterReady(rustpy_project::Interpreter, PathBuf),
    Status(String),
    /// Local files changed (downloads): reload open editors.
    FilesChanged,
}

enum JobResult {
    KubernetesOutput(u64, String, String),
    RemoteConflict(PathBuf, Server, String, PathBuf, PathBuf),
    RemoteSaved(PathBuf, u64),
    TargetOutput(u64, String, String),
    SshResolved(String, rustpy_deploy::ssh_config::Resolved),
    InterpreterReady(rustpy_project::Interpreter, PathBuf),
    Newer(PathBuf, Server, Vec<PathBuf>, Vec<String>),
    Edited(Server, PathBuf, String, PathBuf, u64),
    Compare(PathBuf, PathBuf),
    Message(String, bool),
    Plan(String, SyncDirection, Result<Vec<SyncItem>, String>),
    Listing(String, String, Result<Vec<RemoteEntry>, String>),
    /// Passwords found in the password safe, by key.
    Passwords(HashMap<String, String>),
    /// Finished without anything to report.
    Done,
}

struct Job {
    owner: rustpy_deploy::transfer::TransferOwner,
    auth_id: u64,
    label: String,
    rx: Receiver<JobResult>,
}

struct ConfigState {
    config: DeploymentConfig,
    selected: usize,
    errors: Vec<String>,
    /// Passwords read from the password safe (None until the read finished).
    stored: Option<HashMap<String, String>>,
}

/// Password-safe key of an FTP server's password: `deployment/ftp://user@host:port`.
pub fn password_key(server: &Server) -> Option<String> {
    match &server.kind {
        ServerKind::Ftp {
            host, port, user, ..
        } => Some(rustpy_secrets::deployment_key(
            "ftp",
            user.trim(),
            &host.trim().to_ascii_lowercase(),
            *port,
        )),
        _ => None,
    }
}

/// Takes the FTP passwords out of `cfg` (they belong in the password safe):
/// `(key, password, save password)` for every FTP server, `None` for an empty field.
pub fn take_passwords(cfg: &mut DeploymentConfig) -> Vec<(String, Option<String>, bool)> {
    let mut out = Vec::new();
    for server in &mut cfg.servers {
        let Some(key) = password_key(server) else {
            continue;
        };
        if let ServerKind::Ftp {
            password,
            save_password,
            ..
        } = &mut server.kind
        {
            out.push((
                key,
                password.take().filter(|p| !p.is_empty()),
                *save_password,
            ));
        }
    }
    out
}

/// Runs a password-safe operation; when the database is locked, asks for the
/// master password, waits for the answer and tries once more. Blocking
/// (keyring, the user): deployment worker threads only.
fn with_unlock<T>(op: impl Fn() -> Result<T, SafeError>) -> Result<T, SafeError> {
    match op() {
        Err(SafeError::Locked) => {
            let deadline = std::time::Instant::now() + netsec::UNLOCK_WAIT;
            while std::time::Instant::now() < deadline {
                if rustpy_deploy::transfer::current()
                    .owner
                    .is_some_and(|owner| owner.cancelled())
                {
                    return Err(SafeError::Locked);
                }
                let tick = std::time::Instant::now();
                if netsec::wait_for_unlock(std::time::Duration::from_millis(200)) {
                    return op();
                }
                if tick.elapsed() < std::time::Duration::from_millis(150) {
                    return Err(SafeError::Locked);
                }
            }
            Err(SafeError::Locked)
        }
        other => other,
    }
}

/// Appended to transfer errors when the saved password could not be read.
const LOCKED_HINT: &str = "the password database is locked, so the saved FTP password was not used: unlock it under Settings › Appearance & Behavior › System Settings › Passwords";

/// `server` with its FTP password from the password safe when the
/// configuration has none, and whether the safe stayed locked. Blocking
/// (keyring, the master-password prompt): worker threads only.
fn with_stored_password(mut server: Server) -> (Server, bool) {
    let key = password_key(&server);
    let mut locked = false;
    if let (Some(key), ServerKind::Ftp { password, .. }) = (key, &mut server.kind) {
        if password.as_deref().is_none_or(str::is_empty) {
            match with_unlock(|| rustpy_secrets::get(&key)) {
                Ok(found) => *password = found,
                // Not unlocked: try without (e.g. ~/.netrc); a failure then says why.
                Err(SafeError::Locked) => locked = true,
                Err(e) => log::warn!(
                    "deployment: cannot read the password of {} from the password safe: {e}",
                    server.name
                ),
            }
        }
    }
    (server, locked)
}

/// A transport whose errors say that the saved password was not available.
struct WithoutSavedPassword(Box<dyn rustpy_deploy::Transport>);

fn hinted(e: anyhow::Error) -> anyhow::Error {
    anyhow::anyhow!("{e:#} ({LOCKED_HINT})")
}

impl rustpy_deploy::Transport for WithoutSavedPassword {
    fn rename(&self, source: &str, destination: &str) -> anyhow::Result<()> {
        self.0.rename(source, destination).map_err(hinted)
    }
    fn permissions(&self, path: &str, mode: u32) -> anyhow::Result<()> {
        self.0.permissions(path, mode).map_err(hinted)
    }
    fn upload(&self, local: &Path, remote: &str) -> anyhow::Result<()> {
        self.0.upload(local, remote).map_err(hinted)
    }
    fn download(&self, remote: &str, local: &Path) -> anyhow::Result<()> {
        self.0.download(remote, local).map_err(hinted)
    }
    fn list(&self, remote_dir: &str) -> anyhow::Result<Vec<RemoteEntry>> {
        self.0.list(remote_dir).map_err(hinted)
    }
    fn delete(&self, remote: &str) -> anyhow::Result<()> {
        self.0.delete(remote).map_err(hinted)
    }
    fn mkdirs(&self, remote_dir: &str) -> anyhow::Result<()> {
        self.0.mkdirs(remote_dir).map_err(hinted)
    }
    fn test_connection(&self) -> anyhow::Result<String> {
        self.0.test_connection().map_err(hinted)
    }
    fn coarse_times(&self) -> bool {
        self.0.coarse_times()
    }
    fn list_recursive(
        &self,
        remote_dir: &str,
        excluded: &[String],
    ) -> anyhow::Result<Vec<RemoteEntry>> {
        self.0.list_recursive(remote_dir, excluded).map_err(hinted)
    }
    fn creates_parent_dirs(&self) -> bool {
        self.0.creates_parent_dirs()
    }
    fn describe(&self) -> String {
        self.0.describe()
    }
}

/// The transport for `server`, with its password from the password safe.
pub(crate) fn transport(server: &Server) -> anyhow::Result<Box<dyn rustpy_deploy::Transport>> {
    let (server, locked) = with_stored_password(server.clone());
    let transport = rustpy_deploy::transport_for(&server)?;
    Ok(if locked {
        Box::new(WithoutSavedPassword(transport))
    } else {
        transport
    })
}

/// Moves FTP passwords that older versions stored in plain text in the
/// project's deployment.json into the password safe and removes them from the
/// file. A password the safe does not take stays in the file (the UI runs this
/// again after the database is unlocked). Blocking (keyring): worker threads
/// only, but it never waits for the master password, as it runs on the
/// password-safe thread. Returns a message when something moved or failed.
pub fn migrate_plaintext_passwords(root: &Path) -> Option<String> {
    let mut cfg = DeploymentConfig::load(root).ok()?;
    if cfg.plaintext_password_servers().is_empty() {
        return None;
    }
    let (mut moved, mut failed) = (Vec::new(), Vec::new());
    for server in &mut cfg.servers {
        let Some(key) = password_key(server) else {
            continue;
        };
        let name = server.name.clone();
        if let ServerKind::Ftp {
            password,
            save_password,
            ..
        } = &mut server.kind
        {
            let Some(secret) = password.clone().filter(|p| !p.is_empty()) else {
                continue;
            };
            let result = if *save_password {
                rustpy_secrets::set(&key, &secret)
            } else {
                rustpy_secrets::set_for_session(&key, &secret)
            };
            match result {
                Ok(()) => {
                    *password = None;
                    moved.push(name);
                }
                Err(e) => failed.push(format!("{name} ({e})")),
            }
        }
    }
    let mut messages = Vec::new();
    if !moved.is_empty() {
        match cfg.save(root) {
            Ok(()) => {
                log::info!(
                    "deployment: moved the FTP password(s) of {} into the password safe",
                    moved.join(", ")
                );
                messages.push(format!("Moved the FTP password of {} from .rustpy/deployment.json into the password safe", moved.join(", ")));
            }
            Err(e) => messages.push(format!("Cannot update .rustpy/deployment.json: {e:#}")),
        }
    }
    if !failed.is_empty() {
        messages.push(format!(
            "FTP passwords stay in plain text in .rustpy/deployment.json: {}",
            failed.join(", ")
        ));
    }
    Some(messages.join(". "))
}

/// Stores the passwords taken out of the configuration on Save. A locked
/// database first asks for the master password; a password the safe still
/// does not take is kept for this session (and, when the database was locked,
/// stored once it is unlocked). Passwords are never written back into
/// deployment.json.
fn store_passwords(
    secrets: Vec<(String, Option<String>, bool)>,
    stored: Option<HashMap<String, String>>,
) -> JobResult {
    let (mut waiting, mut failed) = (0usize, Vec::new());
    for (key, password, save) in secrets {
        let result = match &password {
            Some(p) if save => with_unlock(|| rustpy_secrets::set(&key, p)),
            Some(p) => with_unlock(|| rustpy_secrets::set_for_session(&key, p)),
            // Cleared field: forget the stored password (only one this dialog had read).
            None if save && stored.as_ref().is_some_and(|s| s.contains_key(&key)) => {
                with_unlock(|| rustpy_secrets::delete(&key))
            }
            None if save => Ok(()),
            // "Save password" off before the dialog had read the password: keep a
            // stored one for this session only.
            None => match with_unlock(|| rustpy_secrets::get(&key)) {
                Ok(Some(p)) => with_unlock(|| rustpy_secrets::set_for_session(&key, &p)),
                Ok(None) => Ok(()),
                Err(e) => Err(e),
            },
        };
        match (result, password) {
            (Ok(()), _) => {}
            (Err(e), Some(p)) => {
                rustpy_secrets::keep_for_session(&key, &p);
                if e == SafeError::Locked {
                    waiting += 1;
                    netsec::store_after_unlock(if save {
                        SecretOp::Store { key, secret: p }
                    } else {
                        SecretOp::Session { key, secret: p }
                    });
                } else {
                    failed.push(e.to_string());
                }
            }
            (Err(e), None) => failed.push(e.to_string()),
        }
    }
    let mut messages = Vec::new();
    if waiting > 0 {
        messages.push("The password database is locked: the FTP password is used for this session and saved once you unlock the database".to_string());
    }
    if !failed.is_empty() {
        messages.push(format!("The password safe did not take the FTP password ({}); it is used for this session only", failed.join("; ")));
    }
    if messages.is_empty() {
        JobResult::Done
    } else {
        JobResult::Message(messages.join(". "), false)
    }
}

struct SyncState {
    captured: Server,
    server: String,
    direction: SyncDirection,
    items: Option<Vec<(SyncItem, bool)>>,
    error: Option<String>,
}

struct BrowseState {
    captured: Server,
    last_refresh: f64,
    server: String,
    dir: String,
    entries: Vec<RemoteEntry>,
    error: Option<String>,
}

#[derive(Default)]
pub struct DeployUi {
    kubernetes: Option<kubernetes::State>,
    background_paused: bool,
    remote_edits: HashMap<PathBuf, (PathBuf, Server, String, u64)>,
    extra: workspace::Workspace,
    root: Option<PathBuf>,
    credentials: crate::git_jobs::GitJobs,
    console_open: bool,
    config_dialog: Option<ConfigState>,
    sync_dialog: Option<SyncState>,
    browser: Option<BrowseState>,
    jobs: Vec<Job>,
    pub log: Vec<String>,
}

fn kind_name(k: &ServerKind) -> &'static str {
    match k {
        ServerKind::Local { .. } => "Local",
        ServerKind::Sftp { .. } => "SFTP",
        ServerKind::Rsync { .. } => "rsync",
        ServerKind::Ftp { .. } => "FTP",
    }
}

impl DeployUi {
    pub fn open_targets(&mut self, root: &Path) {
        self.open_target_tools(root);
    }

    pub fn is_busy(&self) -> bool {
        !self.jobs.is_empty() || !self.extra.external.is_empty()
    }

    fn spawn(&mut self, label: impl Into<String>, f: impl FnOnce() -> JobResult + Send + 'static) {
        let label = label.into();
        let owner = rustpy_deploy::transfer::TransferOwner::new(
            self.root.clone().unwrap_or_default(),
            label.clone(),
        );
        let worker = owner.clone();
        let (auth_id, environment) = self.credentials.new_job();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            struct Finish(rustpy_deploy::transfer::TransferOwner);
            impl Drop for Finish {
                fn drop(&mut self) {
                    self.0.finish();
                }
            }
            let _finish = Finish(worker.clone());
            let _ = tx.send(worker.run(|| {
                let mut context = rustpy_deploy::transfer::current();
                context.environment = environment.vars;
                rustpy_deploy::transfer::with_context(context, f)
            }));
        });
        self.jobs.push(Job {
            owner,
            auth_id,
            label,
            rx,
        });
    }
    pub fn pending_transfers(&self) -> Vec<(u64, String, String)> {
        let mut pending = self
            .jobs
            .iter()
            .map(|job| (job.owner.id, job.label.clone(), job.owner.lifecycle_stamp()))
            .collect::<Vec<_>>();
        if !self.extra.external.is_empty() {
            pending.push((
                0,
                format!("{} queued deployment changes", self.extra.external.len()),
                format!("{:?}", self.extra.external.keys().collect::<Vec<_>>()),
            ));
        }
        pending
    }
    pub fn stop_transfer(&mut self, id: u64) -> bool {
        if id == 0 && !self.extra.external.is_empty() {
            self.extra.external.clear();
            return true;
        }
        if let Some(job) = self.jobs.iter().find(|job| job.owner.id == id) {
            job.owner.cancel();
            true
        } else {
            false
        }
    }
    pub fn cancel_all(&mut self) {
        self.credentials.cancel();
        self.background_paused = true;
        self.extra.external.clear();
        self.extra.upload_choice = None;
        self.extra.newer = None;
        self.extra.remote_conflict = None;
        for job in &self.jobs {
            job.owner.cancel();
        }
    }
    pub fn resume_background(&mut self) {
        self.background_paused = false;
    }
    /// Drain cancelled owners without publishing results or drawing their old dialogs.
    pub fn poll_retirement(&mut self, ctx: &egui::Context) {
        let mut finished = Vec::new();
        self.jobs.retain(|job| {
            if job.owner.finished() {
                finished.push(job.auth_id);
                false
            } else {
                true
            }
        });
        for id in finished {
            self.credentials.job_finished(id, false, "Cancelled");
        }
        if !self.jobs.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
    fn capture_root(&mut self, root: &Path) {
        if self.root.as_deref() != Some(root) {
            self.cancel_all();
            self.config_dialog = None;
            self.sync_dialog = None;
            self.browser = None;
            self.extra = Default::default();
            self.kubernetes = None;
            self.root = Some(root.into());
            self.background_paused = false;
        }
    }

    fn server_or_default(
        root: &Path,
        name: Option<&str>,
    ) -> Result<(DeploymentConfig, Server), String> {
        let cfg = DeploymentConfig::load(root).map_err(|e| e.to_string())?;
        let server = match name {
            Some(n) => cfg.server(n).cloned(),
            None => cfg.default_server_entry().cloned(),
        };
        match server {
            Some(s) => Ok((cfg, s)),
            None => Err(
                "No deployment server is configured (Tools › Deployment › Configuration…)".into(),
            ),
        }
    }

    pub fn open_config(&mut self, root: &Path) {
        self.capture_root(root);
        self.resume_background();
        let config = DeploymentConfig::load_or_default(root);
        // The password fields show what the password safe holds (read off the UI thread).
        let keys: Vec<String> = config.servers.iter().filter_map(password_key).collect();
        self.config_dialog = Some(ConfigState {
            config,
            selected: 0,
            errors: Vec::new(),
            stored: None,
        });
        self.spawn("Reading deployment passwords", move || {
            let mut found = HashMap::new();
            for key in keys {
                match with_unlock(|| rustpy_secrets::get(&key)) {
                    Ok(Some(p)) => {
                        found.insert(key, p);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        log::warn!("deployment: cannot read a password from the password safe: {e}")
                    }
                }
            }
            JobResult::Passwords(found)
        });
    }

    /// Uploads paths to the default server (before-launch callers retain this API).
    pub fn upload(&mut self, root: &Path, paths: Vec<PathBuf>) -> Option<String> {
        self.capture_root(root);
        let (_, server) = match Self::server_or_default(root, None) {
            Ok(value) => value,
            Err(error) => return Some(error),
        };
        self.transfer_selected(root, server, paths, false, false);
        None
    }
    pub fn download(&mut self, root: &Path, paths: Vec<PathBuf>) -> Option<String> {
        self.capture_root(root);
        let (_, server) = match Self::server_or_default(root, None) {
            Ok(value) => value,
            Err(error) => return Some(error),
        };
        self.transfer_selected(root, server, paths, true, false);
        None
    }
    pub fn choose_transfer(
        &mut self,
        root: &Path,
        paths: Vec<PathBuf>,
        download: bool,
    ) -> Option<String> {
        self.capture_root(root);
        let config = DeploymentConfig::load_or_default(root);
        if config.servers.len() > 1 {
            self.extra.upload_choice = Some((root.into(), paths, download));
            None
        } else if download {
            self.download(root, paths)
        } else {
            self.upload(root, paths)
        }
    }
    pub fn on_saved(&mut self, root: &Path, file: &Path, explicit: bool) {
        self.capture_root(root);
        if self.background_paused {
            return;
        }
        if explicit {
            if let Some((_, server, remote, baseline)) = self
                .remote_edits
                .get(file)
                .cloned()
                .filter(|(owner, _, _, _)| owner == root)
            {
                self.save_remote_edit(root, server, remote, file.to_path_buf(), baseline, false);
                return;
            }
        }
        let config = DeploymentConfig::load_or_default(root);
        if let Some(server) = config.auto_upload_target(root, file, explicit).cloned() {
            self.transfer_selected(root, server, vec![file.into()], false, false);
        }
    }
    pub fn open_sync(&mut self, root: &Path) -> Option<String> {
        self.capture_root(root);
        let (cfg, server) = match Self::server_or_default(root, None) {
            Ok(v) => v,
            Err(e) => return Some(e),
        };
        self.sync_dialog = Some(SyncState {
            captured: server.clone(),
            server: server.name.clone(),
            direction: SyncDirection::Both,
            items: None,
            error: None,
        });
        self.plan(root, server, cfg.excluded, SyncDirection::Both);
        None
    }

    fn plan(
        &mut self,
        root: &Path,
        server: Server,
        excluded: Vec<String>,
        direction: SyncDirection,
    ) {
        let root = root.to_path_buf();
        self.spawn(format!("Comparing with {}", server.name), move || {
            let res = transport(&server).map_err(|e| e.to_string()).and_then(|t| {
                rustpy_deploy::plan_server_sync(t.as_ref(), &server, &root, &excluded, direction)
                    .map_err(|e| e.to_string())
            });
            JobResult::Plan(server.name, direction, res)
        });
    }

    pub fn open_browser(&mut self, root: &Path) -> Option<String> {
        self.capture_root(root);
        self.resume_background();
        let (_, server) = match Self::server_or_default(root, None) {
            Ok(v) => v,
            Err(e) => return Some(e),
        };
        let dir = server.root_path.clone();
        self.browser = Some(BrowseState {
            captured: server.clone(),
            last_refresh: 0.0,
            server: server.name.clone(),
            dir: dir.clone(),
            entries: Vec::new(),
            error: None,
        });
        self.list(server, dir);
        None
    }

    fn list(&mut self, server: Server, dir: String) {
        self.spawn(format!("Listing {dir}"), move || {
            let res = transport(&server)
                .map_err(|e| e.to_string())
                .and_then(|t| t.list(&dir).map_err(|e| e.to_string()));
            JobResult::Listing(server.name, dir, res)
        });
    }

    /// Draws the open dialogs and collects finished jobs.
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        theme: &UiTheme,
        root: Option<&Path>,
    ) -> Vec<DeployEvent> {
        if self.root.as_deref() != root {
            self.cancel_all();
            self.config_dialog = None;
            self.sync_dialog = None;
            self.browser = None;
            self.extra = Default::default();
            self.kubernetes = None;
            self.root = root.map(Path::to_path_buf);
            self.background_paused = false;
        }
        let mut events = Vec::new();
        // Finished jobs.
        let mut finished = Vec::new();
        self.jobs.retain(|j| match j.rx.try_recv() {
            Ok(r) => {
                finished.push((j.label.clone(), j.owner.root.clone(), j.auth_id, r));
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(_) => {
                finished.push((
                    j.label.clone(),
                    j.owner.root.clone(),
                    j.auth_id,
                    JobResult::Message("Transfer worker ended without a result".into(), false),
                ));
                false
            }
        });
        if !self.jobs.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
        if !self.extra.external.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        for (_, owner_root, auth_id, r) in finished {
            let success = match &r {
                JobResult::Message(message, _) => {
                    !message.to_ascii_lowercase().contains("failed")
                        && !message.to_ascii_lowercase().contains("cancelled")
                }
                JobResult::Plan(_, _, result) => result.is_ok(),
                JobResult::Listing(_, _, result) => result.is_ok(),
                _ => true,
            };
            self.credentials.job_finished(auth_id, success, "");
            if root != Some(owner_root.as_path()) {
                continue;
            }
            match r {
                JobResult::RemoteSaved(local, baseline) => {
                    if let Some(binding) = self.remote_edits.get_mut(&local) {
                        binding.3 = baseline;
                    }
                    events.push(DeployEvent::Status("Remote edit saved".into()));
                }
                JobResult::RemoteConflict(root, server, remote, local, comparison) => {
                    self.extra.remote_conflict = Some((root, server, remote, local, comparison))
                }
                JobResult::KubernetesOutput(id, label, output) => {
                    self.kubernetes_result(id, label, output)
                }
                JobResult::TargetOutput(id, label, output) => {
                    self.log.push(format!("{label}: {output}"));
                    if let Some(state) = &mut self.extra.tools {
                        if state.expected != Some(id) {
                            continue;
                        }
                        state.operation = label;
                        state.rows = serde_json::from_str::<Vec<serde_json::Value>>(&output)
                            .unwrap_or_else(|_| {
                                output
                                    .lines()
                                    .filter_map(|line| serde_json::from_str(line).ok())
                                    .collect()
                            });
                        state.output = output;
                    }
                }
                JobResult::SshResolved(name, resolved) => {
                    if let Some(server) = self.config_dialog.as_mut().and_then(|state| {
                        state
                            .config
                            .servers
                            .iter_mut()
                            .find(|server| server.name == name)
                    }) {
                        match &mut server.kind {
                            ServerKind::Sftp {
                                host,
                                user,
                                port,
                                key_file,
                            } => {
                                *host = resolved.hostname;
                                *user = resolved.user;
                                *port = resolved.port;
                                *key_file = resolved.identity;
                            }
                            ServerKind::Rsync { host, user, port } => {
                                *host = resolved.hostname;
                                *user = resolved.user;
                                *port = resolved.port;
                            }
                            _ => {}
                        }
                        server.options.jump_host = resolved.proxy_jump;
                    }
                }
                JobResult::InterpreterReady(interpreter, root) => {
                    events.push(DeployEvent::InterpreterReady(interpreter, root))
                }
                JobResult::Newer(root, server, paths, warnings) => {
                    self.extra.newer = Some((root, server, paths, warnings))
                }
                JobResult::Edited(server, owner_root, remote, local, baseline) => {
                    self.remote_edits
                        .insert(local.clone(), (owner_root, server, remote, baseline));
                    events.push(DeployEvent::OpenFile(local));
                }
                JobResult::Compare(local, remote) => {
                    events.push(DeployEvent::Compare(local, remote))
                }
                JobResult::Message(m, changed) => {
                    self.log.push(m.clone());
                    events.push(DeployEvent::Status(m));
                    if changed {
                        events.push(DeployEvent::FilesChanged);
                    }
                }
                JobResult::Plan(server, direction, res) => {
                    if let Some(s) = &mut self.sync_dialog {
                        if s.server != server || s.direction != direction {
                            continue;
                        }
                        match res {
                            Ok(items) => {
                                s.items = Some(
                                    items
                                        .into_iter()
                                        .map(|i| {
                                            let on = matches!(
                                                i.action,
                                                SyncAction::Upload | SyncAction::Download
                                            );
                                            (i, on)
                                        })
                                        .collect(),
                                )
                            }
                            Err(e) => s.error = Some(e),
                        }
                    }
                }
                JobResult::Listing(server, dir, res) => {
                    if let Some(b) = &mut self.browser {
                        if b.dir == dir && b.server == server {
                            if b.captured.options.notify_remote_changes
                                && !b.entries.is_empty()
                                && res.as_ref().is_ok_and(|entries| entries != &b.entries)
                            {
                                events.push(DeployEvent::Status(format!(
                                    "Remote files changed on {}",
                                    b.server
                                )));
                            }
                            match res {
                                Ok(mut entries) => {
                                    entries.sort_by(|a, b| {
                                        b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name))
                                    });
                                    b.entries = entries;
                                    b.error = None;
                                }
                                Err(e) => b.error = Some(e),
                            }
                        }
                    }
                }
                JobResult::Passwords(found) => {
                    if let Some(st) = &mut self.config_dialog {
                        // Fill the fields the user has not typed into yet.
                        for server in &mut st.config.servers {
                            let key = password_key(server);
                            if let (Some(key), ServerKind::Ftp { password, .. }) =
                                (key, &mut server.kind)
                            {
                                if password.is_none() {
                                    *password = found.get(&key).cloned();
                                }
                            }
                        }
                        st.stored = Some(found);
                    }
                }
                JobResult::Done => {}
            }
        }
        for job in &self.jobs {
            for progress in job.owner.drain_progress() {
                self.log.push(format!(
                    "{}: {} {}/{} {}",
                    job.label, progress.message, progress.current, progress.total, progress.path
                ));
            }
        }
        if self.log.len() > 2000 {
            self.log.drain(..self.log.len() - 2000);
        }
        for message in self.credentials.poll_prompts() {
            self.log.push(message);
        }
        self.credentials.prompt_window(ctx, theme);
        self.transfer_console(ctx);
        self.workspace_windows(ctx, theme, &mut events);
        self.targets_window(ctx, &mut events);
        self.kubernetes_window(ctx, &mut events);
        let Some(root) = root else { return events };
        self.config_window(ctx, theme, root, &mut events);
        self.sync_window(ctx, theme, root, &mut events);
        self.browser_window(ctx, theme, root, &mut events);
        events
    }

    fn config_window(
        &mut self,
        ctx: &egui::Context,
        theme: &UiTheme,
        root: &Path,
        events: &mut Vec<DeployEvent>,
    ) {
        let Some(st) = &mut self.config_dialog else {
            return;
        };
        let mut open = true;
        let mut save = false;
        let mut test: Option<Server> = None;
        let mut ssh_action = None;
        let mut target_tools = false;
        egui::Window::new("Deployment Configuration").collapsible(false).resizable(true).default_size([760.0, 460.0]).anchor(Align2::CENTER_CENTER, Vec2::ZERO).open(&mut open).show_modal(ctx, |ui| {
            ui.horizontal(|ui| {
                for (label, kind) in [("+ SFTP", 0), ("+ rsync", 1), ("+ FTP", 2), ("+ Local folder", 3),("+ FTPS",4)] {
                    if ui.button(crate::i18n::tr_static(label)).clicked() {
                        let n = st.config.servers.len() + 1;
                        let server = match kind {
                            0 => Server::sftp(format!("server{n}"), "example.com", "", "/var/www"),
                            1 => Server::rsync(format!("server{n}"), "example.com", "", "/var/www"),
                            2 => Server::ftp(format!("server{n}"), "ftp.example.com", "", None, "www"),
                            4 => {let mut server=Server::ftp(format!("server{n}"),"ftp.example.com","",None,"www");server.options.ftp_tls="explicit".into();server},
                            _ => Server::local(format!("server{n}"), root.join("deploy")),
                        };
                        st.config.upsert_server(server);
                        st.selected = st.config.servers.len() - 1;
                    }
                }
                if ui.add_enabled(!st.config.servers.is_empty(), egui::Button::new("Remove")).clicked() && st.selected < st.config.servers.len() {
                    let name = st.config.servers[st.selected].name.clone();
                    st.config.remove_server(&name);
                    st.selected = st.selected.saturating_sub(1);
                }
                ui.separator();
                ui.label("Automatic upload:");
                for a in [AutoUpload::Never, AutoUpload::OnExplicitSave, AutoUpload::Always] {
                    if ui.selectable_label(st.config.auto_upload == a, a.label()).clicked() {
                        st.config.auto_upload = a;
                    }
                }
            });
            ui.separator();
            egui::Panel::left("deploy_servers").resizable(true).default_size(180.0).show(ui, |ui| {
                for (i, s) in st.config.servers.iter().enumerate() {
                    let default = st.config.default_server.as_deref() == Some(s.name.as_str());
                    let label = format!("{}{} ({})", s.name, if default { " ★" } else { "" }, kind_name(&s.kind));
                    if ui.selectable_label(i == st.selected, label).clicked() {
                        st.selected = i;
                    }
                }
                if st.config.servers.is_empty() {
                    ui.label(RichText::new("No servers yet").color(theme.text_dim));
                }
            });
            let selected = st.selected;
            let default_name = st.config.default_server.clone();
            let passwords_read = st.stored.is_some();
            if let Some(s) = st.config.servers.get_mut(selected) {
                egui::Grid::new("deploy_server_grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                    ui.label("Name:");
                    ui.text_edit_singleline(&mut s.name);
                    ui.end_row();
                    ui.label("Type:");
                    ui.label(s.kind.type_label());
                    ui.end_row();
                    match &mut s.kind {
                        ServerKind::Local { root: folder } => {
                            ui.label("Folder:");
                            let mut t = folder.display().to_string();
                            if ui.text_edit_singleline(&mut t).changed() {
                                *folder = PathBuf::from(t);
                            }
                            ui.end_row();
                        }
                        ServerKind::Sftp { host, port, user, key_file } => {
                            ui.label("Host:");
                            ui.text_edit_singleline(host);
                            ui.end_row();
                            ui.label("Port:");
                            ui.add(egui::DragValue::new(port));
                            ui.end_row();
                            ui.label("User:");
                            ui.text_edit_singleline(user);
                            ui.end_row();
                            ui.label("Key file:");
                            let mut t = key_file.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                            if ui.add(egui::TextEdit::singleline(&mut t).hint_text("ssh-agent / ~/.ssh/config when empty")).changed() {
                                *key_file = if t.trim().is_empty() { None } else { Some(PathBuf::from(t)) };
                            }
                            ui.end_row();
                        }
                        ServerKind::Rsync { host, user, port } => {
                            ui.label("Host:");
                            ui.text_edit_singleline(host);
                            ui.end_row();
                            ui.label("Port:");
                            ui.add(egui::DragValue::new(port));
                            ui.end_row();
                            ui.label("User:");
                            ui.text_edit_singleline(user);
                            ui.end_row();
                        }
                        ServerKind::Ftp { host, port, user, password, save_password } => {
                            ui.label("Host:");
                            ui.text_edit_singleline(host);
                            ui.end_row();
                            ui.label("Port:");
                            ui.add(egui::DragValue::new(port));
                            ui.end_row();
                            ui.label("User:");
                            ui.text_edit_singleline(user);
                            ui.end_row();
                            ui.label("Password:");
                            ui.horizontal(|ui| {
                                let mut t = password.clone().unwrap_or_default();
                                let hint = if passwords_read { "~/.netrc when empty" } else { "reading the password safe…" };
                                if ui.add(egui::TextEdit::singleline(&mut t).password(true).hint_text(hint)).changed() {
                                    *password = if t.is_empty() { None } else { Some(t) };
                                }
                                ui.checkbox(save_password, "Save password").on_hover_text("Keep the password in the password safe (Settings › Appearance & Behavior › System Settings › Passwords). Otherwise it is forgotten when RustPy exits. Passwords are never written to .rustpy/deployment.json.");
                            });
                            ui.end_row();
                        }
                    }
                    ui.label("Root path:");
                    ui.text_edit_singleline(&mut s.root_path);
                    ui.end_row();
                    ui.label("Web URL:");
                    let mut w = s.web_url.clone().unwrap_or_default();
                    if ui.text_edit_singleline(&mut w).changed() {
                        s.web_url = if w.trim().is_empty() { None } else { Some(w) };
                    }
                    ui.end_row();
                });
                if matches!(s.kind, ServerKind::Ftp { .. }) {
                    ui.label(RichText::new("The password is kept in the IDE's password safe, not in .rustpy/deployment.json.").color(theme.text_dim).small());
                }
                Self::server_options(ui,s);
                if matches!(s.kind,ServerKind::Sftp{..}|ServerKind::Rsync{..}){ui.horizontal_wrapped(|ui|{for action in ["Parse SSH config","SSH terminal","Use SSH interpreter","Sync folders","Upload helpers"]{if ui.button(crate::i18n::tr_static(action)).clicked(){ssh_action=Some((s.clone(),action.to_string()));}}});ui.horizontal(|ui|{ui.text_edit_singleline(&mut self.extra.external_command);if ui.button("Run SSH external tool").clicked(){ssh_action=Some((s.clone(),"Run SSH external tool".into()));}});}
                ui.add_space(4.0);
                ui.label(RichText::new("Mappings (local path relative to the project → deployment path)").strong());
                let mut remove: Option<usize> = None;
                for (i, m) in s.mappings.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut m.local).hint_text("(project root)").desired_width(200.0));
                        ui.label("→");
                        ui.add(egui::TextEdit::singleline(&mut m.remote).hint_text("(root path)").desired_width(200.0));
                        ui.label("Web:");ui.add(egui::TextEdit::singleline(&mut m.web_path).hint_text("web path").desired_width(130.0));
                        if ui.small_button("×").clicked() {
                            remove = Some(i);
                        }
                    });
                }
                if let Some(i) = remove {
                    s.mappings.remove(i);
                }
                if ui.small_button("+ Mapping").clicked() {
                    s.mappings.push(Mapping::new("", ""));
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let is_default = default_name.as_deref() == Some(s.name.as_str());
                    if ui.add_enabled(!is_default, egui::Button::new("Use as default")).clicked() {
                        st.config.default_server = Some(s.name.clone());
                    }
                    if ui.button("Test Connection").clicked() {
                        test = Some(s.clone());
                    }
                });
            }
            ui.separator();
            if ui.button("Docker / Compose / Vagrant targets…").clicked(){target_tools=true;}
            for e in &st.errors {
                ui.colored_label(theme.error, e);
            }
            ui.label("Project excluded paths / names (one glob per line)");let mut excluded=st.config.excluded.join("\n");if ui.text_edit_multiline(&mut excluded).changed(){st.config.excluded=excluded.lines().map(str::trim).filter(|line|!line.is_empty()).map(str::to_string).collect();}
if ui.button("File Transfer console").clicked(){self.console_open=true;}
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
                    save = true;
                }
                ui.label(RichText::new("Saved to .rustpy/deployment.json").color(theme.text_dim).small());
            });
        });
        if target_tools {
            self.open_target_tools(root);
        }
        if let Some((server, action)) = ssh_action {
            if action == "Parse SSH config" {
                self.parse_ssh_configuration(server);
            } else {
                self.ssh_configuration_controls(root, server, &action, events);
            }
        }
        if let Some(server) = test {
            self.spawn(
                format!("Testing {}", server.name),
                move || match transport(&server).and_then(|t| t.test_connection()) {
                    Ok(m) => {
                        JobResult::Message(format!("{}: connection OK — {m}", server.name), false)
                    }
                    Err(e) => JobResult::Message(
                        format!("{}: connection failed — {e}", server.name),
                        false,
                    ),
                },
            );
        }
        let Some(st) = &mut self.config_dialog else {
            return;
        };
        if save {
            if st.config.default_server.is_none() {
                st.config.default_server = st.config.servers.first().map(|s| s.name.clone());
            }
            st.errors = st.config.validate();
            if st.errors.is_empty() {
                // Passwords go to the password safe, not into the file.
                let mut to_write = st.config.clone();
                let secrets = take_passwords(&mut to_write);
                match to_write.save(root) {
                    Ok(()) => {
                        events.push(DeployEvent::Status("Deployment configuration saved".into()));
                        let stored = st.stored.take();
                        self.config_dialog = None;
                        if !secrets.is_empty() {
                            self.spawn("Storing deployment passwords", move || {
                                store_passwords(secrets, stored)
                            });
                        }
                        return;
                    }
                    Err(e) => st.errors.push(e.to_string()),
                }
            }
        }
        if !open {
            self.config_dialog = None;
        }
    }

    fn sync_window(
        &mut self,
        ctx: &egui::Context,
        theme: &UiTheme,
        root: &Path,
        events: &mut Vec<DeployEvent>,
    ) {
        let Some(st) = &mut self.sync_dialog else {
            return;
        };
        let mut open = true;
        let mut run = false;
        let mut compare = None;
        let mut replan: Option<SyncDirection> = None;
        let busy = !self.jobs.is_empty();
        egui::Window::new(format!("Sync with {}", st.server))
            .collapsible(false)
            .resizable(true)
            .default_size([720.0, 440.0])
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .show_modal(ctx, |ui| {
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("sync-deployment-server")
                        .selected_text(&st.server)
                        .show_ui(ui, |ui| {
                            for server in DeploymentConfig::load_or_default(root).servers {
                                if ui
                                    .selectable_label(server.name == st.server, &server.name)
                                    .clicked()
                                {
                                    st.server = server.name.clone();
                                    st.captured = server;
                                    replan = Some(st.direction);
                                }
                            }
                        })
                        .combo_named(crate::i18n::tr_static("Deployment server"));
                    for (label, d) in [
                        ("Both ways", SyncDirection::Both),
                        ("Upload only", SyncDirection::Upload),
                        ("Download only", SyncDirection::Download),
                    ] {
                        if ui
                            .selectable_label(st.direction == d, crate::i18n::tr_static(label))
                            .clicked()
                            && st.direction != d
                        {
                            st.direction = d;
                            replan = Some(d);
                        }
                    }
                });
                ui.separator();
                if let Some(e) = &st.error {
                    ui.colored_label(theme.error, e);
                }
                match &mut st.items {
                    None => {
                        ui.label(
                            RichText::new("Comparing local files with the server…")
                                .color(theme.text_dim),
                        );
                    }
                    Some(items) if items.is_empty() => {
                        ui.label(RichText::new("Everything is in sync.").color(theme.success));
                    }
                    Some(items) => {
                        egui::ScrollArea::vertical()
                            .max_height(320.0)
                            .show(ui, |ui| {
                                egui::Grid::new("sync_grid")
                                    .striped(true)
                                    .spacing([12.0, 3.0])
                                    .show(ui, |ui| {
                                        ui.label("");
                                        ui.label(RichText::new("File").strong());
                                        ui.label("Compare");
                                        ui.label(RichText::new("Action").strong());
                                        ui.label(RichText::new("Local / remote size").strong());
                                        ui.end_row();
                                        for (item, on) in items.iter_mut() {
                                            ui.checkbox(on, "");
                                            ui.label(&item.rel_path);
                                            if ui
                                                .add_enabled(
                                                    item.exists_locally() && item.exists_remotely(),
                                                    egui::Button::new("Compare"),
                                                )
                                                .clicked()
                                            {
                                                compare = Some(item.rel_path.clone());
                                            }
                                            let mut action = item.action;
                                            egui::ComboBox::from_id_salt((
                                                "sync_action",
                                                &item.rel_path,
                                            ))
                                            .selected_text(format!("{action:?}"))
                                            .show_ui(ui, |ui| {
                                                for a in [
                                                    SyncAction::Upload,
                                                    SyncAction::Download,
                                                    SyncAction::Skip,
                                                    SyncAction::DeleteRemote,
                                                ] {
                                                    let allowed = match a {
                                                        SyncAction::Upload => item.exists_locally(),
                                                        SyncAction::Download
                                                        | SyncAction::DeleteRemote => {
                                                            item.exists_remotely()
                                                        }
                                                        _ => true,
                                                    };
                                                    if allowed
                                                        && ui
                                                            .selectable_label(
                                                                action == a,
                                                                format!("{a:?}"),
                                                            )
                                                            .clicked()
                                                    {
                                                        action = a;
                                                    }
                                                }
                                            })
                                            .combo_named(&format!(
                                                "{}: {}",
                                                crate::i18n::tr_static("Sync action"),
                                                item.rel_path
                                            ));
                                            if action != item.action {
                                                item.action = action;
                                                *on = matches!(
                                                    action,
                                                    SyncAction::Upload
                                                        | SyncAction::Download
                                                        | SyncAction::DeleteRemote
                                                );
                                            }
                                            let fmt = |s: Option<u64>| {
                                                s.map(|b| format!("{b} B"))
                                                    .unwrap_or_else(|| "—".into())
                                            };
                                            ui.label(
                                                RichText::new(format!(
                                                    "{} / {}",
                                                    fmt(item.local_size),
                                                    fmt(item.remote_size)
                                                ))
                                                .color(theme.text_dim)
                                                .small(),
                                            );
                                            ui.end_row();
                                        }
                                    });
                            });
                        let n = items
                            .iter()
                            .filter(|(i, on)| {
                                *on && matches!(
                                    i.action,
                                    SyncAction::Upload
                                        | SyncAction::Download
                                        | SyncAction::DeleteRemote
                                )
                            })
                            .count();
                        ui.horizontal(|ui| {
                            if ui
                                .add_enabled(
                                    n > 0 && !busy,
                                    egui::Button::new(format!("Synchronize {n} item(s)")),
                                )
                                .clicked()
                            {
                                run = true;
                            }
                            if items
                                .iter()
                                .any(|(i, on)| *on && i.action == SyncAction::DeleteRemote)
                            {
                                ui.colored_label(theme.warning, "Remote files will be deleted");
                            }
                        });
                    }
                }
            });
        let captured = st.captured.clone();
        if let Some(d) = replan {
            st.items = None;
            self.plan(
                root,
                captured.clone(),
                DeploymentConfig::load_or_default(root).excluded,
                d,
            );
        }
        if let Some(path) = compare {
            let local = root.join(path);
            if let Some(remote) = rustpy_deploy::remote_path(&captured, root, &local) {
                self.remote_operation(root, captured.clone(), remote, "Compare with local");
            }
        }
        if run {
            let chosen: Vec<SyncItem> = self
                .sync_dialog
                .as_ref()
                .and_then(|s| s.items.as_ref())
                .map(|items| {
                    items
                        .iter()
                        .filter(|(_, on)| *on)
                        .map(|(i, _)| i.clone())
                        .collect()
                })
                .unwrap_or_default();
            {
                let server = captured;
                let root_c = root.to_path_buf();
                self.spawn(format!("Synchronizing with {}", server.name), move || {
                    let transport = match transport(&server) {
                        Ok(t) => t,
                        Err(e) => return JobResult::Message(format!("Sync failed: {e}"), false),
                    };
                    let results = rustpy_deploy::execute_sync(
                        transport.as_ref(),
                        &server,
                        &root_c,
                        &chosen,
                        |current, total, path| {
                            if let Some(owner) = rustpy_deploy::transfer::current().owner {
                                owner.progress(current, total, path, "Synchronizing");
                            }
                        },
                    );
                    let failed: Vec<String> = results
                        .iter()
                        .filter_map(|(p, r)| {
                            r.as_ref().err().map(|e| format!("{}: {e}", p.display()))
                        })
                        .collect();
                    JobResult::Message(
                        if failed.is_empty() {
                            format!(
                                "Synchronized {} item(s) with {}",
                                results.len(),
                                server.name
                            )
                        } else {
                            format!(
                                "Sync with {}: {} failed — {}",
                                server.name,
                                failed.len(),
                                failed.join("; ")
                            )
                        },
                        true,
                    )
                });
                self.sync_dialog = None;
                events.push(DeployEvent::Status("Synchronizing…".into()));
                return;
            }
        }
        if !open {
            self.sync_dialog = None;
        }
    }

    fn browser_window(
        &mut self,
        ctx: &egui::Context,
        theme: &UiTheme,
        root: &Path,
        events: &mut Vec<DeployEvent>,
    ) {
        let Some(st) = &mut self.browser else { return };
        let mut open = true;
        let mut go: Option<String> = None;
        let mut download: Option<String> = None;
        let mut operation = None;
        let now = ctx.input(|input| input.time);
        if st.captured.options.notify_remote_changes
            && !self.background_paused
            && now - st.last_refresh >= 10.0
            && self.jobs.is_empty()
        {
            go = Some(st.dir.clone());
            st.last_refresh = now;
        }
        egui::Window::new(format!("Remote Host: {}", st.server))
            .collapsible(false)
            .resizable(true)
            .default_size([560.0, 420.0])
            .open(&mut open)
            .show_modeless(ctx, |ui| {
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("remote-host-server")
                        .selected_text(&st.server)
                        .show_ui(ui, |ui| {
                            for server in DeploymentConfig::load_or_default(root).servers {
                                if ui
                                    .selectable_label(server.name == st.server, &server.name)
                                    .clicked()
                                {
                                    st.server = server.name.clone();
                                    st.dir = server.root_path.clone();
                                    st.captured = server;
                                    go = Some(st.dir.clone());
                                }
                            }
                        })
                        .combo_named(crate::i18n::tr_static("Deployment server"));
                    if ui.button("Up").clicked() {
                        go = Some(
                            rustpy_deploy::remote_parent(&st.dir).unwrap_or_else(|| "/".into()),
                        );
                    }
                    ui.label(RichText::new(&st.dir).monospace());
                    if ui.button("⟳").clicked() {
                        go = Some(st.dir.clone());
                    }
                });
                if let Some(e) = &st.error {
                    ui.colored_label(theme.error, e);
                }
                ui.horizontal(|ui| {
                    ui.label("Rename to");
                    ui.text_edit_singleline(&mut self.extra.rename);
                    ui.label("Mode (octal)");
                    ui.text_edit_singleline(&mut self.extra.permissions);
                    if ui.button("Transfer console").clicked() {
                        self.console_open = true;
                    }
                });
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for e in &st.entries {
                        ui.horizontal(|ui| {
                            if e.is_dir {
                                if ui.link(format!("{}/", e.name)).clicked() {
                                    go = Some(rustpy_deploy::join_remote(&st.dir, &e.name));
                                }
                            } else {
                                ui.label(&e.name);
                                ui.label(
                                    RichText::new(format!("{} B", e.size))
                                        .color(theme.text_dim)
                                        .small(),
                                );
                                if ui.small_button("Download").clicked() {
                                    download = Some(rustpy_deploy::join_remote(&st.dir, &e.name));
                                }
                            }
                            for action in [
                                "Edit",
                                "Compare with local",
                                "Upload local",
                                "Rename",
                                "Delete",
                                "Permissions",
                            ] {
                                if ui.small_button(crate::i18n::tr_static(action)).clicked() {
                                    operation = Some((
                                        rustpy_deploy::join_remote(&st.dir, &e.name),
                                        action,
                                    ));
                                }
                            }
                        });
                    }
                });
            });
        let captured = st.captured.clone();
        if let Some((remote, action)) = operation {
            self.remote_operation(root, captured.clone(), remote, action);
        }
        if let Some(dir) = go {
            if let Some(b) = &mut self.browser {
                b.dir = dir.clone();
                b.entries.clear();
            }
            self.list(captured.clone(), dir);
        }
        if let Some(remote) = download {
            let server = captured;
            match rustpy_deploy::local_path(&server, root, &remote) {
                Some(local) => {
                    let label = format!("Downloading {remote}");
                    self.spawn(label, move || {
                        match transport(&server).and_then(|t| t.download(&remote, &local)) {
                            Ok(()) => {
                                JobResult::Message(format!("Downloaded {}", local.display()), true)
                            }
                            Err(e) => JobResult::Message(format!("Download failed: {e}"), false),
                        }
                    });
                }
                None => events.push(DeployEvent::Status(format!(
                    "{remote} is outside the deployment mappings"
                ))),
            }
        }
        if !open {
            self.browser = None;
        }
    }
}

impl Drop for DeployUi {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_secrets::{
        Backend as _, EncryptedFile, PasswordSettings, PasswordStorage, Safe, SafeConfig,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const KEY: &str = "deployment/ftp://bob@ftp.example.com:21";

    #[test]
    fn passwords_are_taken_out_of_the_configuration() {
        let mut cfg = DeploymentConfig::default();
        cfg.servers.push(Server::ftp(
            "f",
            " FTP.Example.com ",
            "bob",
            Some("pw".into()),
            "/",
        ));
        cfg.servers
            .push(Server::ftp("g", "h", "", Some(String::new()), "/"));
        cfg.servers.push(Server::sftp("s", "h", "me", "/"));
        assert_eq!(
            password_key(&cfg.servers[0]).as_deref(),
            Some(KEY),
            "host case and spaces do not matter"
        );
        assert_eq!(password_key(&cfg.servers[2]), None);
        let taken = take_passwords(&mut cfg);
        assert_eq!(
            taken,
            vec![
                (KEY.to_string(), Some("pw".to_string()), true),
                ("deployment/ftp://@h:21".to_string(), None, true)
            ]
        );
        assert!(cfg.plaintext_password_servers().is_empty());
        assert!(!serde_json::to_string(&cfg).unwrap().contains("\"pw\""));
    }

    #[test]
    fn plaintext_passwords_move_into_the_safe() {
        let _g = crate::netsec::safe_test_lock();
        rustpy_secrets::install(Safe::new(SafeConfig::memory()));
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = DeploymentConfig::default();
        cfg.servers.push(Server::ftp(
            "legacy",
            "ftp.example.com",
            "bob",
            Some("s3cret".into()),
            "/",
        ));
        cfg.servers
            .push(Server::local("folder", dir.path().join("out")));
        cfg.save(dir.path()).unwrap();
        // Older versions kept the password in deployment.json: it moves into the safe.
        let msg = migrate_plaintext_passwords(dir.path()).unwrap();
        assert!(msg.contains("legacy"), "{msg}");
        let text = std::fs::read_to_string(DeploymentConfig::path(dir.path())).unwrap();
        assert!(!text.contains("s3cret"), "{text}");
        assert_eq!(rustpy_secrets::get(KEY).unwrap().as_deref(), Some("s3cret"));
        assert_eq!(
            migrate_plaintext_passwords(dir.path()),
            None,
            "nothing left to move"
        );
        // Transfers find it in the safe.
        let server = DeploymentConfig::load(dir.path()).unwrap().servers[0].clone();
        let (server, locked) = with_stored_password(server);
        let ServerKind::Ftp { password, .. } = server.kind else {
            panic!("ftp")
        };
        assert_eq!((password.as_deref(), locked), (Some("s3cret"), false));
        // "Save password" off: usable this session, gone from storage — also when
        // the dialog had not read the password yet (empty field).
        for typed in [Some("s3cret".to_string()), None] {
            rustpy_secrets::set(KEY, "s3cret").unwrap();
            assert!(matches!(
                store_passwords(vec![(KEY.into(), typed.clone(), false)], None),
                JobResult::Done
            ));
            assert_eq!(rustpy_secrets::safe().get(KEY).unwrap(), None, "{typed:?}");
            assert_eq!(
                rustpy_secrets::peek(KEY).as_deref(),
                Some("s3cret"),
                "{typed:?}"
            );
        }
        // A cleared field forgets the password the dialog had read.
        rustpy_secrets::set(KEY, "s3cret").unwrap();
        let stored = HashMap::from([(KEY.to_string(), "s3cret".to_string())]);
        assert!(matches!(
            store_passwords(vec![(KEY.into(), None, true)], Some(stored)),
            JobResult::Done
        ));
        assert_eq!(rustpy_secrets::get(KEY).unwrap(), None);
        // An empty field the dialog never filled leaves the stored password alone.
        rustpy_secrets::set(KEY, "kept").unwrap();
        assert!(matches!(
            store_passwords(vec![(KEY.into(), None, true)], None),
            JobResult::Done
        ));
        assert_eq!(
            rustpy_secrets::safe().get(KEY).unwrap().as_deref(),
            Some("kept")
        );
        rustpy_secrets::install(Safe::new(SafeConfig::memory()));
    }

    /// Installs a fresh password database `dir/name` holding `entries`,
    /// protected by the master password "master" that was not entered yet.
    fn install_locked_safe(dir: &Path, name: &str, entries: &[(&str, &str)]) -> std::path::PathBuf {
        let db = dir.join(name);
        let file = || {
            EncryptedFile::with_iterations(db.join("passwords.db"), db.join("passwords.key"), 1000)
        };
        let setup = file();
        setup.set("x", "y").unwrap();
        for (k, v) in entries {
            setup.set(k, v).unwrap();
        }
        setup.set_master_password(Some("master")).unwrap();
        // A safe for another configuration first, so that this one is new (no unlocked key, no session values).
        rustpy_secrets::install(Safe::new(SafeConfig::memory()));
        let config = SafeConfig::from_settings(
            &PasswordSettings {
                storage: PasswordStorage::EncryptedFile,
                database: String::new(),
            },
            &db,
        );
        rustpy_secrets::install(Safe::new(config).with_file(file()));
        db
    }

    /// Answers the master-password prompt once, like the UI: `password`
    /// unlocks, `None` is "Not now". Join the thread, then read how often it was asked.
    fn answer_unlock_prompt(
        password: Option<&'static str>,
    ) -> (Arc<AtomicUsize>, std::thread::JoinHandle<()>) {
        let asked = Arc::new(AtomicUsize::new(0));
        let a = asked.clone();
        netsec::set_unlock_handler(|| {});
        let answering = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            while Instant::now() < deadline && a.load(Ordering::SeqCst) == 0 {
                if netsec::unlock_wanted() {
                    a.fetch_add(1, Ordering::SeqCst);
                    if let Some(pw) = password {
                        rustpy_secrets::unlock(pw).unwrap();
                    }
                    netsec::unlock_answered();
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        (asked, answering)
    }

    #[test]
    fn a_locked_database_asks_for_the_master_password_and_never_falls_back_to_plain_text() {
        if crate::netsec::isolated_unlock_test("deploy_ui::tests::a_locked_database_asks_for_the_master_password_and_never_falls_back_to_plain_text") { return; }
        let _g = crate::netsec::safe_test_lock();
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = DeploymentConfig::default();
        cfg.servers
            .push(Server::ftp("f", "ftp.example.com", "bob", None, "/"));
        cfg.save(dir.path()).unwrap();
        let file_text = || std::fs::read_to_string(DeploymentConfig::path(dir.path())).unwrap();
        let message = |r: &JobResult| match r {
            JobResult::Message(m, _) => m.clone(),
            _ => String::new(),
        };

        // Save, "Not now": the typed password is used this session and stored after a
        // later unlock, never written into deployment.json.
        install_locked_safe(dir.path(), "a", &[]);
        let (asked, answering) = answer_unlock_prompt(None);
        let result = store_passwords(vec![(KEY.into(), Some("typed-pw".into()), true)], None);
        answering.join().unwrap();
        assert_eq!(
            asked.load(Ordering::SeqCst),
            1,
            "the master password was asked for"
        );
        assert!(
            message(&result).contains("saved once you unlock the database"),
            "{}",
            message(&result)
        );
        assert!(!file_text().contains("typed-pw"), "{}", file_text());
        assert_eq!(
            rustpy_secrets::get(KEY).unwrap().as_deref(),
            Some("typed-pw"),
            "kept for the session"
        );
        assert_eq!(
            netsec::take_stores_after_unlock(),
            vec![SecretOp::Store {
                key: KEY.into(),
                secret: "typed-pw".into()
            }]
        );

        // Save, unlocked when asked: the password lands in the safe.
        let db = install_locked_safe(dir.path(), "b", &[]);
        let (asked, answering) = answer_unlock_prompt(Some("master"));
        assert!(matches!(
            store_passwords(vec![(KEY.into(), Some("typed-pw".into()), true)], None),
            JobResult::Done
        ));
        answering.join().unwrap();
        assert_eq!(asked.load(Ordering::SeqCst), 1);
        assert!(!file_text().contains("typed-pw"));
        let reopened =
            EncryptedFile::with_iterations(db.join("passwords.db"), db.join("passwords.key"), 1000);
        reopened.unlock("master").unwrap();
        assert_eq!(reopened.get(KEY).unwrap().as_deref(), Some("typed-pw"));

        // A transfer, unlocked when asked: it gets the saved password.
        let server = DeploymentConfig::load(dir.path()).unwrap().servers[0].clone();
        install_locked_safe(dir.path(), "c", &[(KEY, "saved-pw")]);
        let (asked, answering) = answer_unlock_prompt(Some("master"));
        let (with, locked) = with_stored_password(server.clone());
        answering.join().unwrap();
        let ServerKind::Ftp { password, .. } = with.kind else {
            panic!("ftp")
        };
        assert_eq!(
            (password.as_deref(), locked, asked.load(Ordering::SeqCst)),
            (Some("saved-pw"), false, 1)
        );

        // A transfer, "Not now": it tries without, and a failure says why.
        install_locked_safe(dir.path(), "d", &[(KEY, "saved-pw")]);
        let (asked, answering) = answer_unlock_prompt(None);
        let (without, locked) = with_stored_password(server);
        answering.join().unwrap();
        let ServerKind::Ftp { password, .. } = &without.kind else {
            panic!("ftp")
        };
        assert_eq!(
            (password, locked, asked.load(Ordering::SeqCst)),
            (&None, true, 1)
        );
        let failing = WithoutSavedPassword(Box::new(Failing));
        let e = rustpy_deploy::Transport::test_connection(&failing)
            .unwrap_err()
            .to_string();
        assert!(
            e.starts_with("530 Login incorrect") && e.contains("password database is locked"),
            "{e}"
        );
        assert_eq!(
            rustpy_deploy::Transport::describe(&failing),
            "ftp://bob@ftp.example.com:21"
        );
        netsec::clear_unlock_handler();
        rustpy_secrets::install(Safe::new(SafeConfig::memory()));
    }

    /// A transport whose every operation fails like an FTP login without password.
    struct Failing;

    impl rustpy_deploy::Transport for Failing {
        fn upload(&self, _: &Path, _: &str) -> anyhow::Result<()> {
            anyhow::bail!("530 Login incorrect")
        }
        fn download(&self, _: &str, _: &Path) -> anyhow::Result<()> {
            anyhow::bail!("530 Login incorrect")
        }
        fn list(&self, _: &str) -> anyhow::Result<Vec<RemoteEntry>> {
            anyhow::bail!("530 Login incorrect")
        }
        fn delete(&self, _: &str) -> anyhow::Result<()> {
            anyhow::bail!("530 Login incorrect")
        }
        fn mkdirs(&self, _: &str) -> anyhow::Result<()> {
            anyhow::bail!("530 Login incorrect")
        }
        fn test_connection(&self) -> anyhow::Result<String> {
            anyhow::bail!("530 Login incorrect")
        }
        fn describe(&self) -> String {
            "ftp://bob@ftp.example.com:21".into()
        }
    }
}
