//! Glue between the IDE settings and the network and password-safe crates
//! (`rustpy-net`, `rustpy-secrets`): configuration at startup and on Apply,
//! which secrets Apply stores, keeps for the session or forgets, and the
//! worker thread that runs password-safe operations.
//!
//! Keyring calls can block (D-Bus round trips, the keyring's own unlock
//! prompt) and deriving a master-password key takes a moment, so the UI thread
//! never calls the safe: it queues [`SafeJob`]s on a [`SafeWorker`] and
//! applies the [`SafeOutcome`]s it polls each frame. Jobs run in order, so a
//! storage switch queued before a store lands the secret in the new storage.
//! Deployment transfers read and store their passwords on their own worker
//! threads; when the database is locked they ask for the master password and
//! wait for the answer ([`wait_for_unlock`]).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use rustpy_secrets::{SafeConfig, SafeError, SafeStatus, AI_API_KEY, PROXY_PASSWORD};
use rustpy_settings::{PasswordSettings, ProxyMode, Settings};

/// `<config dir>/certs`: certificates accepted under Tools › Server Certificates.
pub fn certs_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("certs")
}

/// The network configuration for `settings` (with the session's proxy password).
pub fn net_config(settings: &Settings, config_dir: &Path) -> rustpy_net::NetConfig {
    rustpy_net::NetConfig {
        proxy: settings.http_proxy.clone(),
        certificates: settings.certificates.clone(),
        certs_dir: Some(certs_dir(config_dir)),
    }
}

/// The password safe for `settings` (`<config dir>/passwords.db` by default).
pub fn safe_config(settings: &Settings, config_dir: &Path) -> SafeConfig {
    SafeConfig::from_settings(&settings.passwords, config_dir)
}

/// Configures the password safe and networking for freshly loaded settings,
/// before anything connects. An API key that older versions stored in
/// settings.json is used for this session right away; [`SafeJob::Load`] then
/// moves it into the safe.
pub fn configure(settings: &mut Settings, config_dir: &Path) {
    rustpy_secrets::configure(safe_config(settings, config_dir));
    configure_network(settings, config_dir);
}

/// [`configure`] without installing a password safe (settings re-read while
/// the IDE runs move the secrets with [`SafeJob::Switch`] instead).
pub fn configure_network(settings: &mut Settings, config_dir: &Path) {
    let legacy = settings.ai.legacy_api_key.trim();
    if settings.ai.api_key.is_empty() && !legacy.is_empty() {
        settings.ai.api_key = legacy.to_string();
    }
    rustpy_net::configure(net_config(settings, config_dir));
}

/// For settings re-read from disk (File › Manage IDE Settings › Import,
/// Restore Defaults) that choose another password storage than the safe in
/// use: the job that moves the saved secrets there, as Apply does. Installing
/// the new safe directly would leave them behind, out of reach.
pub fn reload_storage_switch(settings: &Settings, config_dir: &Path) -> Option<SafeJob> {
    let installed = rustpy_secrets::safe().config().clone();
    let wanted = safe_config(settings, config_dir);
    (installed != wanted).then(|| SafeJob::Switch {
        to: wanted,
        previous: password_settings_for(&installed, config_dir),
    })
}

/// The Passwords settings that give `config` (the inverse of [`safe_config`]),
/// e.g. to put back the storage in use when a switch fails.
pub fn password_settings_for(config: &SafeConfig, config_dir: &Path) -> PasswordSettings {
    let database = if config.database == config_dir.join("passwords.db")
        || config.database.as_os_str().is_empty()
    {
        String::new()
    } else {
        config.database.display().to_string()
    };
    PasswordSettings {
        storage: config.storage,
        database,
    }
}

/// Lets untrusted server certificates ask the user: queued prompts repaint the
/// UI, and connections made on the calling (UI) thread never wait for an answer.
/// Background work that needs a locked password database can ask for the
/// master password the same way ([`wait_for_unlock`]).
pub fn install_certificate_prompts(ctx: &egui::Context) {
    let repaint = ctx.clone();
    rustpy_net::set_prompt_handler(
        move || repaint.request_repaint(),
        Some(std::thread::current().id()),
    );
    let repaint = ctx.clone();
    set_unlock_handler(move || repaint.request_repaint());
}

/// Final process shutdown only: release certificate/unlock waiters and UI callbacks.
/// Password-safe writes must be drained first; this does not discard queued writes.
pub fn shutdown_prompts() {
    rustpy_net::clear_prompt_handler();
    let mut requests = unlock_requests();
    requests.notify = None;
    requests.waiting = 0;
    requests.answers += 1;
    drop(requests);
    UNLOCK_ANSWERED.notify_all();
}

// ----- background work waiting for the master password ----------------------

/// How long background work waits for the master-password prompt to be answered.
pub const UNLOCK_WAIT: Duration = Duration::from_secs(300);

type Notify = Arc<dyn Fn() + Send + Sync>;

struct UnlockRequests {
    /// Wakes the UI (None without a UI: nobody can answer, so nobody waits).
    notify: Option<Notify>,
    /// Threads waiting for the next answer (an answer serves them all).
    waiting: usize,
    /// Counts answers (unlocked, or "Not now").
    answers: u64,
    /// Secrets to store once the database is unlocked.
    stores: Vec<SecretOp>,
}

static UNLOCK: Mutex<UnlockRequests> = Mutex::new(UnlockRequests {
    notify: None,
    waiting: 0,
    answers: 0,
    stores: Vec::new(),
});
static UNLOCK_ANSWERED: Condvar = Condvar::new();

fn unlock_requests() -> MutexGuard<'static, UnlockRequests> {
    UNLOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Enables [`wait_for_unlock`]: `notify` wakes the UI, which then shows the
/// master-password prompt ([`unlock_wanted`]) and reports the answer
/// ([`unlock_answered`]).
pub fn set_unlock_handler(notify: impl Fn() + Send + Sync + 'static) {
    unlock_requests().notify = Some(Arc::new(notify));
}

/// Disables [`wait_for_unlock`]; threads waiting now stop waiting.
pub fn clear_unlock_handler() {
    unlock_requests().notify = None;
    UNLOCK_ANSWERED.notify_all();
}

/// Called by a worker thread (never the UI thread, never the password-safe
/// worker, which runs the unlock itself) that found the password database
/// locked: asks the UI for the master password and waits until the prompt is
/// answered or `timeout` passes. True when it was answered: the caller then
/// tries again (the answer may have been "Not now"). False at once without a UI.
pub fn wait_for_unlock(timeout: Duration) -> bool {
    let mut requests = unlock_requests();
    let Some(notify) = requests.notify.clone() else {
        return false;
    };
    let seen = requests.answers;
    requests.waiting += 1;
    drop(requests);
    notify();
    let requests = unlock_requests();
    let (mut requests, _) = UNLOCK_ANSWERED
        .wait_timeout_while(requests, timeout, |r| {
            r.answers == seen && r.notify.is_some()
        })
        .unwrap_or_else(|e| e.into_inner());
    if requests.answers == seen {
        // Gave up without an answer.
        requests.waiting = requests.waiting.saturating_sub(1);
        return false;
    }
    true
}

/// UI: background work waits for the master password.
pub fn unlock_wanted() -> bool {
    unlock_requests().waiting > 0
}

/// UI: the master-password prompt was answered (unlocked, or "Not now"): every
/// waiting thread continues, and [`unlock_wanted`] is false until another asks.
pub fn unlock_answered() {
    let mut requests = unlock_requests();
    requests.answers += 1;
    requests.waiting = 0;
    drop(requests);
    UNLOCK_ANSWERED.notify_all();
}

/// Remembers a secret the locked database did not take, to store it once the
/// database is unlocked ([`take_stores_after_unlock`]). A newer one for the same key replaces it.
pub fn store_after_unlock(op: SecretOp) {
    let mut requests = unlock_requests();
    requests.stores.retain(|o| o.key() != op.key());
    requests.stores.push(op);
}

/// The secrets waiting for [`store_after_unlock`], oldest first.
pub fn take_stores_after_unlock() -> Vec<SecretOp> {
    std::mem::take(&mut unlock_requests().stores)
}

/// The secrets to read from the safe at startup: the legacy key to move into
/// the safe, and which of the remembered secrets are needed now.
pub fn startup_load(settings: &Settings) -> SafeJob {
    let legacy = settings.ai.legacy_api_key.trim();
    SafeJob::Load {
        legacy_ai_key: (!legacy.is_empty()).then(|| legacy.to_string()),
        ai_key: settings.ai.remember_api_key,
        openai_key: settings.ai.remember_openai_api_key,
        proxy_password: needs_proxy_password(settings),
    }
}

/// A manual/PAC proxy needs a remembered authentication password not loaded yet.
pub fn needs_proxy_password(settings: &Settings) -> bool {
    let p = &settings.http_proxy;
    matches!(p.mode, ProxyMode::Manual | ProxyMode::Pac)
        && p.authenticate
        && p.remember_password
        && p.password.is_empty()
}

/// A change to a stored secret, made when settings are applied.
#[derive(Clone, PartialEq, Eq)]
pub enum SecretOp {
    /// "Remember" is on: keep it in the safe.
    Store { key: String, secret: String },
    /// "Remember" is off: usable this session, removed from storage.
    Session { key: String, secret: String },
    /// The field was cleared.
    Delete { key: String },
}

impl SecretOp {
    pub fn key(&self) -> &str {
        match self {
            SecretOp::Store { key, .. }
            | SecretOp::Session { key, .. }
            | SecretOp::Delete { key } => key,
        }
    }
}

impl std::fmt::Debug for SecretOp {
    // Never prints the secret.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            SecretOp::Store { .. } => "Store",
            SecretOp::Session { .. } => "Session",
            SecretOp::Delete { .. } => "Delete",
        };
        write!(f, "{kind}({})", self.key())
    }
}

/// What applying `new` over `old` does to the stored AI API key and proxy password.
pub fn secret_ops(old: &Settings, new: &Settings) -> Vec<SecretOp> {
    let mut ops = Vec::new();
    // API keys are pasted with stray whitespace; passwords are taken as typed.
    let (old_key, new_key) = (old.ai.api_key.trim(), new.ai.api_key.trim());
    push_op(
        &mut ops,
        AI_API_KEY,
        (old_key, old.ai.remember_api_key),
        (new_key, new.ai.remember_api_key),
    );
    push_op(
        &mut ops,
        rustpy_secrets::OPENAI_API_KEY,
        (old.ai.openai_api_key.trim(), old.ai.remember_openai_api_key),
        (new.ai.openai_api_key.trim(), new.ai.remember_openai_api_key),
    );
    push_op(
        &mut ops,
        PROXY_PASSWORD,
        (&old.http_proxy.password, old.http_proxy.remember_password),
        (&new.http_proxy.password, new.http_proxy.remember_password),
    );
    ops
}

fn push_op(ops: &mut Vec<SecretOp>, key: &str, old: (&str, bool), new: (&str, bool)) {
    if old == new {
        return;
    }
    let (secret, remember) = new;
    let key = key.to_string();
    ops.push(if secret.is_empty() {
        SecretOp::Delete { key }
    } else if remember {
        SecretOp::Store {
            key,
            secret: secret.to_string(),
        }
    } else {
        SecretOp::Session {
            key,
            secret: secret.to_string(),
        }
    });
}

/// User-facing name of a stored secret.
pub fn secret_label(key: &str) -> String {
    match key {
        AI_API_KEY => "AI Assistant API key".into(),
        rustpy_secrets::OPENAI_API_KEY => "OpenAI-compatible API key".into(),
        PROXY_PASSWORD => "HTTP proxy password".into(),
        _ => match key.strip_prefix("deployment/") {
            Some(target) => format!("Deployment server {target}"),
            None => key.to_string(),
        },
    }
}

/// Work for the password-safe thread.
pub enum SafeJob {
    /// Moves the AI key older versions kept in settings.json into the safe, then
    /// reads the remembered secrets the session needs.
    Load {
        legacy_ai_key: Option<String>,
        ai_key: bool,
        openai_key: bool,
        proxy_password: bool,
    },
    Apply(Vec<SecretOp>),
    /// Moves every secret to another storage (Settings › Passwords).
    Switch {
        to: SafeConfig,
        previous: PasswordSettings,
    },
    Unlock(String),
    /// Sets, changes (`Some`) or removes (`None`) the master password.
    SetMasterPassword(Option<String>),
    /// Status and stored keys for the Passwords page.
    Status,
    Delete(String),
    /// Moves FTP passwords out of a project's deployment.json.
    MigrateDeployment(PathBuf),
}

impl std::fmt::Debug for SafeJob {
    // Never prints a secret.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            SafeJob::Load { .. } => "Load",
            SafeJob::Apply(_) => "Apply",
            SafeJob::Switch { .. } => "Switch",
            SafeJob::Unlock(_) => "Unlock",
            SafeJob::SetMasterPassword(_) => "SetMasterPassword",
            SafeJob::Status => "Status",
            SafeJob::Delete(_) => "Delete",
            SafeJob::MigrateDeployment(_) => "MigrateDeployment",
        };
        f.write_str(name)
    }
}

/// Result of a [`SafeJob`].
#[derive(Debug, PartialEq, Eq)]
pub enum SafeOutcome {
    Loaded {
        legacy_moved: bool,
        ai_key: Option<Result<Option<String>, SafeError>>,
        openai_key: Option<Result<Option<String>, SafeError>>,
        proxy_password: Option<Result<Option<String>, SafeError>>,
    },
    /// Every operation with its result (failed ones can be retried, e.g. after unlocking).
    Applied(Vec<(SecretOp, Result<(), SafeError>)>),
    Switched {
        result: Result<usize, SafeError>,
        previous: PasswordSettings,
    },
    Unlocked(Result<(), SafeError>),
    MasterPasswordSet {
        set: bool,
        result: Result<(), SafeError>,
    },
    Status {
        status: SafeStatus,
        keys: Result<Vec<String>, SafeError>,
    },
    Deleted(String, Result<(), SafeError>),
    Message(String),
    WorkerFailed(String),
}

/// Persistence failures must retire privately during a prepared quit and be
/// published only after quit preparation is aborted and ordinary UI resumes.
pub fn retirement_failure(outcome: &SafeOutcome) -> Option<String> {
    match outcome {
        SafeOutcome::Applied(results) => results.iter().find_map(|(operation, result)| {
            result
                .as_ref()
                .err()
                .map(|error| format!("Cannot save {}: {error}", secret_label(operation.key())))
        }),
        SafeOutcome::Switched {
            result: Err(error), ..
        } => Some(format!("Password storage switch failed: {error}")),
        SafeOutcome::MasterPasswordSet {
            result: Err(error), ..
        } => Some(format!("Master password change failed: {error}")),
        SafeOutcome::Deleted(key, Err(error)) => {
            Some(format!("Cannot forget {}: {error}", secret_label(key)))
        }
        SafeOutcome::WorkerFailed(error) => Some(error.clone()),
        _ => None,
    }
}

/// Runs one job against the process-wide safe (blocking).
pub fn run_job(job: SafeJob) -> SafeOutcome {
    match job {
        SafeJob::Load {
            legacy_ai_key,
            ai_key,
            openai_key,
            proxy_password,
        } => {
            let mut legacy_moved = false;
            if let Some(key) = legacy_ai_key {
                match rustpy_secrets::set(AI_API_KEY, &key) {
                    Ok(()) => {
                        legacy_moved = true;
                        log::info!("moved the AI Assistant API key from settings.json into the password safe");
                    }
                    Err(e) => log::warn!(
                        "cannot move the AI Assistant API key into the password safe: {e}"
                    ),
                }
            }
            SafeOutcome::Loaded {
                legacy_moved,
                ai_key: ai_key.then(|| rustpy_secrets::get(AI_API_KEY)),
                openai_key: openai_key.then(|| rustpy_secrets::get(rustpy_secrets::OPENAI_API_KEY)),
                proxy_password: proxy_password.then(|| rustpy_secrets::get(PROXY_PASSWORD)),
            }
        }
        SafeJob::Apply(ops) => SafeOutcome::Applied(
            ops.into_iter()
                .map(|op| {
                    let result = match &op {
                        SecretOp::Store { key, secret } => rustpy_secrets::set(key, secret),
                        SecretOp::Session { key, secret } => {
                            rustpy_secrets::set_for_session(key, secret)
                        }
                        SecretOp::Delete { key } => rustpy_secrets::delete(key),
                    };
                    (op, result)
                })
                .collect(),
        ),
        SafeJob::Switch { to, previous } => SafeOutcome::Switched {
            result: rustpy_secrets::switch_storage(to),
            previous,
        },
        SafeJob::Unlock(password) => SafeOutcome::Unlocked(rustpy_secrets::unlock(&password)),
        SafeJob::SetMasterPassword(password) => SafeOutcome::MasterPasswordSet {
            set: password.is_some(),
            result: rustpy_secrets::set_master_password(password.as_deref()),
        },
        SafeJob::Status => SafeOutcome::Status {
            status: rustpy_secrets::status(),
            keys: rustpy_secrets::safe().keys(),
        },
        SafeJob::Delete(key) => {
            let result = rustpy_secrets::delete(&key);
            SafeOutcome::Deleted(key, result)
        }
        SafeJob::MigrateDeployment(root) => SafeOutcome::Message(
            crate::deploy_ui::migrate_plaintext_passwords(&root).unwrap_or_default(),
        ),
    }
}

/// The password-safe thread: runs [`SafeJob`]s in order.
pub struct SafeWorker {
    tx: Sender<SafeJob>,
    rx: Receiver<SafeOutcome>,
    pending: usize,
    failed: Vec<SafeOutcome>,
}

impl SafeWorker {
    /// Starts the thread; `notify` is called after each finished job (repaint).
    pub fn start(notify: impl Fn() + Send + 'static) -> SafeWorker {
        let (tx, jobs) = crossbeam_channel::unbounded::<SafeJob>();
        let (done, rx) = crossbeam_channel::unbounded();
        let spawned = std::thread::Builder::new()
            .name("rustpy-password-safe".into())
            .spawn(move || {
                for job in jobs {
                    log::debug!("password safe: {job:?}");
                    if done.send(run_job(job)).is_err() {
                        break;
                    }
                    notify();
                }
            });
        if let Err(e) = spawned {
            log::error!("cannot start the password safe thread: {e}");
        }
        SafeWorker {
            tx,
            rx,
            pending: 0,
            failed: Vec::new(),
        }
    }

    pub fn send(&mut self, job: SafeJob) {
        match self.tx.send(job) {
            Ok(()) => self.pending += 1,
            Err(_) => self.failed.push(SafeOutcome::WorkerFailed(
                "Password-safe worker is unavailable; the requested operation was not saved".into(),
            )),
        }
    }

    /// Finished jobs, oldest first.
    pub fn poll(&mut self) -> Vec<SafeOutcome> {
        let mut done = std::mem::take(&mut self.failed);
        loop {
            match self.rx.try_recv() {
                Ok(outcome) => {
                    self.pending = self.pending.saturating_sub(1);
                    done.push(outcome);
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    if self.pending > 0 {
                        self.pending = 0;
                        done.push(SafeOutcome::WorkerFailed("Password-safe worker stopped before returning write results; saved changes could not be confirmed".into()));
                    }
                    break;
                }
            }
        }
        done
    }

    /// Jobs queued or running.
    pub fn is_busy(&self) -> bool {
        self.pending > 0 || !self.failed.is_empty()
    }
}

/// Runs global unlock-state writers in their own current test executable.
#[cfg(test)]
pub(crate) fn isolated_unlock_test(selector: &str) -> bool {
    const CHILD: &str = "RUSTPY_UNLOCK_FIXTURE_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(selector) {
        return false;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", selector, "--nocapture"]);
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("RUSTPY_")
            || name.starts_with("XDG_")
            || matches!(
                name.as_ref(),
                "DISPLAY" | "WAYLAND_DISPLAY" | "MIR_SOCKET" | "RUST_TEST_THREADS"
            )
        {
            command.env_remove(key);
        }
    }
    for (key, name) in [
        ("RUSTPY_CONFIG_DIR", "config"),
        ("RUSTPY_SYSTEM_DIR", "system"),
        ("RUSTPY_LOG_DIR", "log"),
        ("XDG_CONFIG_HOME", "xdg-config"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_RUNTIME_DIR", "runtime"),
    ] {
        let path = dir.path().join(name);
        std::fs::create_dir(&path).unwrap();
        command.env(key, path);
    }
    let stdout = dir.path().join("stdout.log");
    let stderr = dir.path().join("stderr.log");
    command
        .env(CHILD, selector)
        .stdin(std::process::Stdio::null())
        .stdout(std::fs::File::create(&stdout).unwrap())
        .stderr(std::fs::File::create(&stderr).unwrap());
    let mut child = command.spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("owned unlock fixture wait failed: {selector}: {error}");
            }
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("owned unlock fixture exceeded watchdog: {selector}");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let out = std::fs::read_to_string(stdout).unwrap();
    let err = std::fs::read_to_string(stderr).unwrap();
    assert!(
        status.success() && out.contains("1 passed; 0 failed"),
        "owned exact fixture {selector}: {status}\n{out}\n{err}"
    );
    true
}

/// Serializes tests that install a process-wide password safe or network configuration.
#[cfg(test)]
pub(crate) fn safe_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_secrets::{Backend, MemoryBackend, NativeKeyring, PasswordStorage, Safe};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    fn settings() -> Settings {
        serde_json::from_str("{}").unwrap()
    }

    #[test]
    fn apply_stores_keeps_or_forgets_changed_secrets() {
        let old = settings();
        let mut new = old.clone();
        assert!(secret_ops(&old, &new).is_empty(), "nothing changed");
        new.ai.api_key = "  sk-ant-1 \n".into();
        assert_eq!(
            secret_ops(&old, &new),
            vec![SecretOp::Store {
                key: AI_API_KEY.into(),
                secret: "sk-ant-1".into()
            }]
        );
        // Only whitespace around the key changed: nothing to do.
        let mut padded = new.clone();
        padded.ai.api_key = "sk-ant-1".into();
        assert!(secret_ops(&new, &padded).is_empty());
        // "Remember" off: kept for the session and removed from storage.
        let mut session = new.clone();
        session.ai.remember_api_key = false;
        assert_eq!(
            secret_ops(&new, &session),
            vec![SecretOp::Session {
                key: AI_API_KEY.into(),
                secret: "sk-ant-1".into()
            }]
        );
        // Cleared field: forgotten.
        let mut cleared = new.clone();
        cleared.ai.api_key.clear();
        assert_eq!(
            secret_ops(&new, &cleared),
            vec![SecretOp::Delete {
                key: AI_API_KEY.into()
            }]
        );
        // Proxy passwords keep their spaces.
        let mut proxy = old.clone();
        proxy.http_proxy.password = " p w ".into();
        assert_eq!(
            secret_ops(&old, &proxy),
            vec![SecretOp::Store {
                key: PROXY_PASSWORD.into(),
                secret: " p w ".into()
            }]
        );
        assert_eq!(secret_ops(&old, &proxy)[0].key(), PROXY_PASSWORD);
        assert_eq!(
            format!("{:?}", secret_ops(&old, &proxy)),
            "[Store(http-proxy/password)]",
            "Debug never shows the secret"
        );
    }

    #[test]
    fn startup_loads_what_the_session_needs() {
        let mut s = settings();
        let SafeJob::Load {
            legacy_ai_key,
            ai_key,
            proxy_password,
            ..
        } = startup_load(&s)
        else {
            panic!()
        };
        assert_eq!((legacy_ai_key, ai_key, proxy_password), (None, true, false));
        s.ai.legacy_api_key = " sk-old ".into();
        s.http_proxy.mode = ProxyMode::Manual;
        s.http_proxy.authenticate = true;
        let SafeJob::Load {
            legacy_ai_key,
            proxy_password,
            ..
        } = startup_load(&s)
        else {
            panic!()
        };
        assert_eq!(legacy_ai_key.as_deref(), Some("sk-old"));
        assert!(proxy_password);
        s.http_proxy.password = "typed".into();
        assert!(!needs_proxy_password(&s), "already known");
        assert_eq!(secret_label(AI_API_KEY), "AI Assistant API key");
        assert_eq!(
            secret_label("deployment/ftp://u@h:21"),
            "Deployment server ftp://u@h:21"
        );
    }

    #[test]
    fn password_settings_round_trip_through_the_safe_configuration() {
        let dir = Path::new("/cfg");
        for settings in [
            PasswordSettings::default(),
            PasswordSettings {
                storage: PasswordStorage::Memory,
                database: String::new(),
            },
            PasswordSettings {
                storage: PasswordStorage::EncryptedFile,
                database: "/elsewhere/my.db".into(),
            },
        ] {
            let config = SafeConfig::from_settings(&settings, dir);
            assert_eq!(password_settings_for(&config, dir), settings);
        }
        // The memory safe used before anything is configured.
        assert_eq!(
            password_settings_for(&SafeConfig::memory(), dir).database,
            ""
        );
    }

    #[test]
    fn background_work_waits_for_the_master_password_prompt() {
        if crate::netsec::isolated_unlock_test(
            "netsec::tests::background_work_waits_for_the_master_password_prompt",
        ) {
            return;
        }
        let _g = safe_test_lock();
        // No UI: nobody can answer, so nothing waits.
        clear_unlock_handler();
        assert!(!wait_for_unlock(Duration::from_secs(5)));
        let asked = Arc::new(Mutex::new(0usize));
        let a = asked.clone();
        set_unlock_handler(move || *a.lock().unwrap() += 1);
        let worker = std::thread::spawn(|| wait_for_unlock(Duration::from_secs(20)));
        let deadline = Instant::now() + Duration::from_secs(20);
        while !unlock_wanted() {
            assert!(Instant::now() < deadline, "the worker never asked");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(*asked.lock().unwrap(), 1, "the UI was woken");
        unlock_answered();
        assert!(
            !unlock_wanted(),
            "answered at once, before the worker wakes up (no second prompt)"
        );
        assert!(worker.join().unwrap(), "answered");
        // Unanswered: gives up after the timeout.
        let started = Instant::now();
        assert!(!wait_for_unlock(Duration::from_millis(50)));
        assert!(started.elapsed() >= Duration::from_millis(50));
        // Secrets the locked database refused wait for the unlock; the newest per key wins.
        store_after_unlock(SecretOp::Store {
            key: "deployment/ftp://a@h:21".into(),
            secret: "old".into(),
        });
        store_after_unlock(SecretOp::Store {
            key: AI_API_KEY.into(),
            secret: "sk".into(),
        });
        store_after_unlock(SecretOp::Store {
            key: "deployment/ftp://a@h:21".into(),
            secret: "new".into(),
        });
        let stores = take_stores_after_unlock();
        assert_eq!(
            stores,
            vec![
                SecretOp::Store {
                    key: AI_API_KEY.into(),
                    secret: "sk".into()
                },
                SecretOp::Store {
                    key: "deployment/ftp://a@h:21".into(),
                    secret: "new".into()
                }
            ]
        );
        assert!(take_stores_after_unlock().is_empty());
        clear_unlock_handler();
    }

    #[test]
    fn reloaded_settings_with_another_storage_move_the_secrets() {
        let _g = safe_test_lock();
        let dir = tempfile::tempdir().unwrap();
        // The secrets are in the keyring (Native storage).
        let keyring = Arc::new(Keyring::default());
        let k = keyring.clone();
        let native = SafeConfig::from_settings(&PasswordSettings::default(), dir.path());
        rustpy_secrets::install(Safe::with_native(
            native,
            NativeKeyring::new(move || Ok(k.clone() as Arc<dyn Backend>)),
        ));
        rustpy_secrets::set(AI_API_KEY, "sk-ant").unwrap();
        rustpy_secrets::set(PROXY_PASSWORD, "pw").unwrap();
        let mut s = settings();
        assert!(
            reload_storage_switch(&s, dir.path()).is_none(),
            "same storage: nothing to move"
        );
        // An imported settings bundle chooses the encrypted file: the secrets move
        // there (before, the file safe was installed empty and they stayed behind).
        s.passwords.storage = PasswordStorage::EncryptedFile;
        let job = reload_storage_switch(&s, dir.path()).unwrap();
        assert!(
            matches!(&job, SafeJob::Switch { previous, .. } if *previous == PasswordSettings::default()),
            "{job:?}"
        );
        assert_eq!(
            run_job(job),
            SafeOutcome::Switched {
                result: Ok(2),
                previous: PasswordSettings::default()
            }
        );
        assert!(
            keyring.entries.keys().unwrap().is_empty(),
            "nothing left behind in the keyring"
        );
        let file = rustpy_secrets::EncryptedFile::new(
            dir.path().join("passwords.db"),
            dir.path().join("passwords.key"),
        );
        assert_eq!(file.get(AI_API_KEY).unwrap().as_deref(), Some("sk-ant"));
        assert_eq!(
            rustpy_secrets::get(PROXY_PASSWORD).unwrap().as_deref(),
            Some("pw")
        );
        assert_eq!(
            rustpy_secrets::safe().config().storage,
            PasswordStorage::EncryptedFile
        );
        assert!(reload_storage_switch(&s, dir.path()).is_none(), "moved");
        rustpy_secrets::install(Safe::new(SafeConfig::memory()));
    }

    #[test]
    fn configure_uses_a_legacy_key_for_the_session() {
        let _g = safe_test_lock();
        let dir = tempfile::tempdir().unwrap();
        let mut s = settings();
        s.passwords.storage = PasswordStorage::Memory;
        s.ai.legacy_api_key = "sk-legacy".into();
        configure(&mut s, dir.path());
        assert_eq!(s.ai.api_key, "sk-legacy");
        assert_eq!(
            rustpy_net::config().certs_dir.as_deref(),
            Some(dir.path().join("certs").as_path())
        );
        assert_eq!(
            rustpy_secrets::safe().config().storage,
            PasswordStorage::Memory
        );
        rustpy_net::configure(rustpy_net::NetConfig::default());
    }

    /// An in-memory stand-in for the OS keyring.
    #[derive(Default)]
    struct Keyring {
        entries: MemoryBackend,
    }

    impl Backend for Keyring {
        fn name(&self) -> String {
            "test keyring".into()
        }
        fn get(&self, key: &str) -> Result<Option<String>, SafeError> {
            self.entries.get(key)
        }
        fn set(&self, key: &str, secret: &str) -> Result<(), SafeError> {
            self.entries.set(key, secret)
        }
        fn delete(&self, key: &str) -> Result<(), SafeError> {
            self.entries.delete(key)
        }
        fn keys(&self) -> Result<Vec<String>, SafeError> {
            self.entries.keys()
        }
    }

    fn wait(worker: &mut SafeWorker) -> SafeOutcome {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(o) = worker.poll().into_iter().next() {
                return o;
            }
            assert!(Instant::now() < deadline, "the worker did not answer");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn worker_migrates_loads_applies_and_switches() {
        let _g = safe_test_lock();
        let dir = tempfile::tempdir().unwrap();
        let keyring = Arc::new(Keyring::default());
        let k = keyring.clone();
        let native = SafeConfig::from_settings(
            &PasswordSettings {
                storage: PasswordStorage::Native,
                database: String::new(),
            },
            dir.path(),
        );
        rustpy_secrets::install(Safe::with_native(
            native,
            NativeKeyring::new(move || Ok(k.clone() as Arc<dyn Backend>)),
        ));
        let notified = Arc::new(Mutex::new(0usize));
        let n = notified.clone();
        let mut worker = SafeWorker::start(move || *n.lock().unwrap() += 1);
        // Startup: the legacy key moves into the keyring and is read back.
        worker.send(SafeJob::Load {
            legacy_ai_key: Some("sk-legacy".into()),
            ai_key: true,
            openai_key: true,
            proxy_password: true,
        });
        assert!(worker.is_busy());
        assert_eq!(
            wait(&mut worker),
            SafeOutcome::Loaded {
                legacy_moved: true,
                ai_key: Some(Ok(Some("sk-legacy".into()))),
                openai_key: Some(Ok(None)),
                proxy_password: Some(Ok(None))
            }
        );
        assert!(!worker.is_busy());
        assert_eq!(
            keyring.entries.get(AI_API_KEY).unwrap().as_deref(),
            Some("sk-legacy")
        );
        // Apply: store the proxy password, keep a new AI key for this session only.
        let old = settings();
        let mut new = old.clone();
        new.http_proxy.password = "pw".into();
        new.ai.api_key = "sk-session".into();
        new.ai.remember_api_key = false;
        worker.send(SafeJob::Apply(secret_ops(&old, &new)));
        let SafeOutcome::Applied(results) = wait(&mut worker) else {
            panic!()
        };
        assert!(results.iter().all(|(_, r)| r.is_ok()), "{results:?}");
        assert_eq!(
            keyring.entries.get(PROXY_PASSWORD).unwrap().as_deref(),
            Some("pw")
        );
        assert_eq!(
            keyring.entries.get(AI_API_KEY).unwrap(),
            None,
            "session-only key left the keyring"
        );
        assert_eq!(
            rustpy_secrets::peek(AI_API_KEY).as_deref(),
            Some("sk-session")
        );
        // Status, then a switch to the encrypted file moves the stored secrets.
        worker.send(SafeJob::Status);
        let SafeOutcome::Status { status, keys } = wait(&mut worker) else {
            panic!()
        };
        assert_eq!(status.location, "test keyring");
        assert_eq!(keys.unwrap(), vec![PROXY_PASSWORD.to_string()]);
        let to = SafeConfig::from_settings(
            &PasswordSettings {
                storage: PasswordStorage::EncryptedFile,
                database: String::new(),
            },
            dir.path(),
        );
        worker.send(SafeJob::Switch {
            to,
            previous: PasswordSettings::default(),
        });
        assert_eq!(
            wait(&mut worker),
            SafeOutcome::Switched {
                result: Ok(1),
                previous: PasswordSettings::default()
            }
        );
        assert!(dir.path().join("passwords.db").exists());
        assert!(keyring.entries.keys().unwrap().is_empty());
        worker.send(SafeJob::Delete(PROXY_PASSWORD.into()));
        assert_eq!(
            wait(&mut worker),
            SafeOutcome::Deleted(PROXY_PASSWORD.into(), Ok(()))
        );
        assert!(*notified.lock().unwrap() >= 5);
        rustpy_secrets::install(Safe::new(SafeConfig::memory()));
    }
}

#[cfg(test)]
mod retirement_fixtures {
    use super::*;
    #[test]
    fn worker_loss_retires_to_an_error_instead_of_hanging_or_claiming_a_save() {
        let (tx, jobs) = crossbeam_channel::unbounded();
        let (done, rx) = crossbeam_channel::unbounded();
        let mut worker = SafeWorker {
            tx,
            rx,
            pending: 1,
            failed: Vec::new(),
        };
        drop(done);
        let outcomes = worker.poll();
        assert!(!worker.is_busy());
        assert_eq!(outcomes.len(), 1);
        assert!(retirement_failure(&outcomes[0]).is_some());
        drop(jobs);
        worker.send(SafeJob::Apply(vec![SecretOp::Store {
            key: AI_API_KEY.into(),
            secret: "must-not-be-in-diagnostics".into(),
        }]));
        assert!(worker.is_busy());
        let outcomes = worker.poll();
        assert!(!worker.is_busy());
        let error = retirement_failure(&outcomes[0]).unwrap();
        assert!(!error.contains("must-not-be-in-diagnostics"));
    }
    #[test]
    fn retirement_inspects_write_results_without_publishing_or_retrying_operations() {
        let outcome = SafeOutcome::Applied(vec![(
            SecretOp::Store {
                key: PROXY_PASSWORD.into(),
                secret: "private".into(),
            },
            Err(SafeError::Locked),
        )]);
        let message = retirement_failure(&outcome).unwrap();
        assert!(message.contains("locked"));
        assert!(!message.contains("private"));
        assert!(retirement_failure(&SafeOutcome::MasterPasswordSet {
            set: true,
            result: Ok(())
        })
        .is_none());
        assert!(
            retirement_failure(&SafeOutcome::Unlocked(Err(SafeError::WrongPassword))).is_none()
        );
    }
}
