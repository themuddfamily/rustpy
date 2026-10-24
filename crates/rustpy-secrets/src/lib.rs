//! RustPy IDE — password safe (Appearance & Behavior › System Settings ›
//! Passwords): where the AI API key, the proxy password and deployment
//! passwords are kept instead of plain-text settings files.
//!
//! Storage ([`PasswordStorage`]):
//!
//! * **Native** — the OS keyring: Secret Service over D-Bus on Linux/BSD
//!   (GNOME Keyring, KWallet, KeePassXC), the Windows Credential Manager, the
//!   macOS Keychain. Older KWallet services use their native D-Bus interface
//!   when no Secret Service provider is available. When the keyring is unavailable (no D-Bus session, no
//!   provider, a write fails) passwords go to the encrypted file instead, and
//!   are moved into the keyring once it works again.
//! * **Encrypted file** — [`file::EncryptedFile`]: AES-256-GCM with a key from a
//!   master password (unlocked once per session) or a random per-user key file.
//! * **Memory** — "do not save, forget passwords after restart".
//!
//! The UI calls [`configure`] at startup; [`get`] / [`set`] / [`delete`] then
//! work on the configured safe, with a per-session cache so that [`peek`] can
//! answer on the UI thread without touching the keyring. Keyring calls may
//! block (D-Bus, unlock prompts): call [`get`], [`set`] and [`delete`] off the
//! UI thread.

pub mod file;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod keychain;
#[cfg(all(unix, not(target_os = "macos")))]
mod kwallet;
#[cfg(all(unix, not(target_os = "macos")))]
mod secret_service;
#[cfg(windows)]
mod wincred;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

pub use file::{EncryptedFile, Protection};
pub use rustpy_settings::{PasswordSettings, PasswordStorage};

/// Key of the AI Assistant's Anthropic API key.
pub const OPENAI_API_KEY: &str = "ai.openai-compatible.api-key";
pub const AI_API_KEY: &str = "ai-assistant/anthropic-api-key";
/// Key of the HTTP proxy password.
pub const PROXY_PASSWORD: &str = "http-proxy/password";

/// Key of a deployment server password (`deployment/ftp://user@host:21`).
pub fn deployment_key(scheme: &str, user: &str, host: &str, port: u16) -> String {
    format!("deployment/{scheme}://{user}@{host}:{port}")
}

/// Why a password could not be read or stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SafeError {
    /// The encrypted file needs its master password ([`unlock`]).
    Locked,
    /// The master password is wrong.
    WrongPassword,
    /// The keyring cannot be used.
    Unavailable(String),
    Failed(String),
}

impl std::fmt::Display for SafeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SafeError::Locked => f.write_str("the password database is locked: enter the master password (Settings › Appearance & Behavior › System Settings › Passwords)"),
            SafeError::WrongPassword => f.write_str("wrong master password"),
            SafeError::Unavailable(m) | SafeError::Failed(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for SafeError {}

/// A place where secrets are stored.
pub trait Backend: Send + Sync {
    /// Shown in the settings ("Secret Service keyring", "encrypted file …").
    fn name(&self) -> String;
    fn get(&self, key: &str) -> Result<Option<String>, SafeError>;
    fn set(&self, key: &str, secret: &str) -> Result<(), SafeError>;
    /// Deleting a missing key is not an error.
    fn delete(&self, key: &str) -> Result<(), SafeError>;
    fn keys(&self) -> Result<Vec<String>, SafeError>;
}

/// Secrets kept in memory only.
#[derive(Default)]
pub struct MemoryBackend {
    entries: Mutex<BTreeMap<String, String>>,
}

impl Backend for MemoryBackend {
    fn name(&self) -> String {
        "memory (forgotten when RustPy exits)".into()
    }
    fn get(&self, key: &str) -> Result<Option<String>, SafeError> {
        Ok(self
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .cloned())
    }
    fn set(&self, key: &str, secret: &str) -> Result<(), SafeError> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.into(), secret.into());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<(), SafeError> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(key);
        Ok(())
    }
    fn keys(&self) -> Result<Vec<String>, SafeError> {
        Ok(self
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect())
    }
}

type Connector = Box<dyn Fn() -> Result<Arc<dyn Backend>, String> + Send + Sync>;
/// A connected keyring, or why it is unavailable.
type Connection = Result<Arc<dyn Backend>, String>;

/// The OS keyring, connected on first use; a failed connection is retried
/// after a while (the keyring daemon may start later).
pub struct NativeKeyring {
    connect: Connector,
    /// The last connection attempt and when it was made.
    state: Mutex<Option<(Connection, Instant)>>,
}

const RECONNECT_AFTER: Duration = Duration::from_secs(30);

impl NativeKeyring {
    pub fn new(
        connect: impl Fn() -> Result<Arc<dyn Backend>, String> + Send + Sync + 'static,
    ) -> Self {
        NativeKeyring {
            connect: Box::new(connect),
            state: Mutex::new(None),
        }
    }

    /// The platform keyring.
    pub fn platform() -> Self {
        Self::new(platform_keyring)
    }

    /// The connected keyring, or why it is unavailable.
    pub fn backend(&self) -> Result<Arc<dyn Backend>, String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((result, at)) = state.as_ref() {
            if result.is_ok() || at.elapsed() < RECONNECT_AFTER {
                return result.clone();
            }
        }
        let result = (self.connect)();
        if let Err(e) = &result {
            log::info!("native keyring unavailable: {e}");
        }
        *state = Some((result.clone(), Instant::now()));
        result
    }
}

#[allow(unreachable_code)]
fn platform_keyring() -> Result<Arc<dyn Backend>, String> {
    #[cfg(windows)]
    return Ok(Arc::new(wincred::CredentialManager));
    #[cfg(target_os = "macos")]
    return keychain::Keychain::detect().map(|k| Arc::new(k) as Arc<dyn Backend>);
    #[cfg(all(unix, not(target_os = "macos")))]
    return unix_keyring(
        || secret_service::SecretService::connect().map(|s| Arc::new(s) as Arc<dyn Backend>),
        kwallet::KWallet::connect,
    );
    Err("no keyring support on this platform".into())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn unix_keyring(
    secret_service: impl FnOnce() -> Result<Arc<dyn Backend>, String>,
    kwallet: impl FnOnce() -> Result<Arc<dyn Backend>, String>,
) -> Result<Arc<dyn Backend>, String> {
    secret_service().or_else(|secret_error| {
        kwallet().map_err(|wallet_error| format!("{secret_error}; {wallet_error}"))
    })
}

/// Name of the platform keyring, for the settings page.
pub fn platform_keyring_name() -> &'static str {
    if cfg!(windows) {
        "Windows Credential Manager"
    } else if cfg!(target_os = "macos") {
        "macOS Keychain"
    } else {
        "Secret Service (GNOME Keyring, KWallet, KeePassXC)"
    }
}

/// Where the safe keeps its files and which storage it uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafeConfig {
    pub storage: PasswordStorage,
    /// The encrypted database.
    pub database: PathBuf,
    /// Random key for a database without master password.
    pub key_file: PathBuf,
}

impl SafeConfig {
    /// `settings.database` (default `<config dir>/passwords.db`) and `<config dir>/passwords.key`.
    pub fn from_settings(settings: &PasswordSettings, config_dir: &Path) -> SafeConfig {
        let database = if settings.database.trim().is_empty() {
            config_dir.join("passwords.db")
        } else {
            PathBuf::from(settings.database.trim())
        };
        SafeConfig {
            storage: settings.storage,
            database,
            key_file: config_dir.join("passwords.key"),
        }
    }

    /// Memory-only storage (the default before [`configure`]).
    pub fn memory() -> SafeConfig {
        SafeConfig {
            storage: PasswordStorage::Memory,
            database: PathBuf::new(),
            key_file: PathBuf::new(),
        }
    }
}

/// A password safe: the backends for one [`SafeConfig`].
pub struct Safe {
    config: SafeConfig,
    native: Option<NativeKeyring>,
    file: Option<Arc<EncryptedFile>>,
    memory: Option<MemoryBackend>,
}

/// What the settings page shows about the safe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafeStatus {
    /// Where new passwords go, e.g. "Secret Service keyring".
    pub location: String,
    /// Why the keyring is not used (Native storage falling back to the file).
    pub problem: Option<String>,
    /// The encrypted file has a master password that was not entered yet.
    pub locked: bool,
    /// The encrypted file is protected by a master password.
    pub master_password: bool,
}

impl Safe {
    pub fn new(config: SafeConfig) -> Safe {
        Self::with_native(config, NativeKeyring::platform())
    }

    /// A safe whose Native storage uses `native` (tests use fake keyrings).
    pub fn with_native(config: SafeConfig, native: NativeKeyring) -> Safe {
        let file = || {
            Some(Arc::new(EncryptedFile::new(
                config.database.clone(),
                config.key_file.clone(),
            )))
        };
        match config.storage {
            PasswordStorage::Memory => Safe {
                native: None,
                file: None,
                memory: Some(MemoryBackend::default()),
                config,
            },
            PasswordStorage::EncryptedFile => Safe {
                native: None,
                file: file(),
                memory: None,
                config,
            },
            PasswordStorage::Native => Safe {
                native: Some(native),
                file: file(),
                memory: None,
                config,
            },
        }
    }

    /// Replaces the encrypted file (tests use fewer PBKDF2 iterations).
    pub fn with_file(mut self, file: EncryptedFile) -> Safe {
        if self.file.is_some() {
            self.file = Some(Arc::new(file));
        }
        self
    }

    pub fn config(&self) -> &SafeConfig {
        &self.config
    }

    pub fn file(&self) -> Option<&EncryptedFile> {
        self.file.as_deref()
    }

    /// Keeps the master password `old` was unlocked with when both use the same file.
    fn adopt_unlocked(&self, old: &Safe) {
        if let (Some(new_file), Some(old_file)) = (&self.file, &old.file) {
            new_file.adopt_unlocked(old_file);
        }
    }

    fn native(&self) -> Option<Result<Arc<dyn Backend>, String>> {
        self.native.as_ref().map(NativeKeyring::backend)
    }

    /// Reads a secret: the keyring first, then the encrypted file (a secret found
    /// only in the file is moved into a working keyring). File errors only count
    /// when the keyring is unavailable or not used.
    pub fn get(&self, key: &str) -> Result<Option<String>, SafeError> {
        if let Some(m) = &self.memory {
            return m.get(key);
        }
        let native = match self.native() {
            Some(Ok(n)) => match n.get(key) {
                Ok(Some(v)) => return Ok(Some(v)),
                Ok(None) => Some(n),
                Err(e) => {
                    log::warn!("keyring: cannot read {key}: {e}");
                    None
                }
            },
            _ => None,
        };
        let Some(file) = &self.file else {
            return Ok(None);
        };
        match file.get(key) {
            Ok(Some(v)) => {
                if let Some(n) = native {
                    if n.set(key, &v).is_ok() {
                        let _ = file.delete(key);
                        log::info!("moved {key} from the encrypted file into the keyring");
                    }
                }
                Ok(Some(v))
            }
            Ok(None) => Ok(None),
            Err(_) if native.is_some() => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Stores a secret: in the keyring (removing any copy left in the fallback
    /// file), else in the encrypted file.
    pub fn set(&self, key: &str, secret: &str) -> Result<(), SafeError> {
        if let Some(m) = &self.memory {
            return m.set(key, secret);
        }
        if let Some(Ok(n)) = self.native() {
            match n.set(key, secret) {
                Ok(()) => {
                    if let Some(f) = &self.file {
                        let _ = f.delete(key);
                    }
                    return Ok(());
                }
                Err(e) => log::warn!("keyring: cannot store {key} ({e}); using the encrypted file"),
            }
        }
        match &self.file {
            Some(f) => f.set(key, secret),
            None => Err(SafeError::Unavailable("no password storage".into())),
        }
    }

    /// Deletes a secret everywhere it may be.
    pub fn delete(&self, key: &str) -> Result<(), SafeError> {
        if let Some(m) = &self.memory {
            return m.delete(key);
        }
        let mut result = Ok(());
        let mut deleted = false;
        if let Some(Ok(n)) = self.native() {
            match n.delete(key) {
                Ok(()) => deleted = true,
                Err(e) => result = Err(e),
            }
        }
        if let Some(f) = &self.file {
            match f.delete(key) {
                Ok(()) => deleted = true,
                Err(e) => result = Err(e),
            }
        }
        if deleted {
            Ok(())
        } else {
            result
        }
    }

    /// Keys of every stored secret.
    pub fn keys(&self) -> Result<Vec<String>, SafeError> {
        if let Some(m) = &self.memory {
            return m.keys();
        }
        let mut keys = Vec::new();
        let mut error = None;
        let mut any = false;
        if let Some(Ok(n)) = self.native() {
            match n.keys() {
                Ok(k) => {
                    keys.extend(k);
                    any = true;
                }
                Err(e) => error = Some(e),
            }
        }
        if let Some(f) = &self.file {
            match f.keys() {
                Ok(k) => {
                    keys.extend(k);
                    any = true;
                }
                Err(e) => error = Some(e),
            }
        }
        if !any {
            if let Some(e) = error {
                return Err(e);
            }
        }
        keys.sort();
        keys.dedup();
        Ok(keys)
    }

    /// Status for the settings page (connects to the keyring: call off the UI thread).
    pub fn status(&self) -> SafeStatus {
        let (locked, master_password) = match &self.file {
            Some(f) => (
                f.is_locked(),
                matches!(f.protection(), Ok(Some(Protection::MasterPassword))),
            ),
            None => (false, false),
        };
        let file_name = || self.file.as_ref().map(|f| f.name()).unwrap_or_default();
        let (location, problem) = match self.config.storage {
            PasswordStorage::Memory => (
                "memory only (forgotten when RustPy exits)".to_string(),
                None,
            ),
            PasswordStorage::EncryptedFile => (file_name(), None),
            PasswordStorage::Native => match self.native() {
                Some(Ok(n)) => (n.name(), None),
                Some(Err(e)) => (file_name(), Some(e)),
                None => (file_name(), None),
            },
        };
        SafeStatus {
            location,
            problem,
            locked: locked && self.config.storage != PasswordStorage::Memory,
            master_password,
        }
    }
}

// ---------------------------------------------------------------------------
// The process-wide safe

static SAFE: RwLock<Option<Arc<Safe>>> = RwLock::new(None);
/// Values read or written this session: `Some(secret)`, or `None` for "not stored".
static CACHE: Mutex<Option<HashMap<String, Option<String>>>> = Mutex::new(None);

fn cache_put(key: &str, value: Option<String>) {
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(key.to_string(), value);
}

fn cache_get(key: &str) -> Option<Option<String>> {
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|c| c.get(key).cloned())
}

fn cache_clear() {
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The configured safe (memory-only until [`configure`] is called).
pub fn safe() -> Arc<Safe> {
    if let Some(s) = SAFE.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return s.clone();
    }
    let mut w = SAFE.write().unwrap_or_else(|e| e.into_inner());
    w.get_or_insert_with(|| Arc::new(Safe::new(SafeConfig::memory())))
        .clone()
}

/// Uses `config` from now on (unchanged configurations keep the session cache).
pub fn configure(config: SafeConfig) {
    install(Safe::new(config));
}

/// Installs a safe (tests and [`switch_storage`]).
pub fn install(new: Safe) {
    let mut w = SAFE.write().unwrap_or_else(|e| e.into_inner());
    if let Some(old) = w.as_ref() {
        if old.config == new.config {
            return;
        }
        new.adopt_unlocked(old);
    }
    *w = Some(Arc::new(new));
    drop(w);
    cache_clear();
}

/// Reads a secret (cache, then storage). May block on the keyring.
pub fn get(key: &str) -> Result<Option<String>, SafeError> {
    if let Some(v) = cache_get(key) {
        return Ok(v);
    }
    let v = safe().get(key)?;
    cache_put(key, v.clone());
    Ok(v)
}

/// The secret if it is already known this session (never touches storage).
pub fn peek(key: &str) -> Option<String> {
    cache_get(key).flatten()
}

/// Stores a secret. May block on the keyring.
pub fn set(key: &str, secret: &str) -> Result<(), SafeError> {
    safe().set(key, secret)?;
    cache_put(key, Some(secret.to_string()));
    Ok(())
}

/// Keeps a secret for this session only ("Remember" unchecked): it is
/// available through [`get`] / [`peek`] but removed from storage.
pub fn set_for_session(key: &str, secret: &str) -> Result<(), SafeError> {
    let result = safe().delete(key);
    cache_put(key, Some(secret.to_string()));
    result
}

/// Keeps a secret for this session without touching storage (e.g. one the
/// locked database did not take): [`get`] and [`peek`] return it until it is
/// stored, deleted or RustPy exits.
pub fn keep_for_session(key: &str, secret: &str) {
    cache_put(key, Some(secret.to_string()));
}

/// Deletes a secret from storage and the session.
pub fn delete(key: &str) -> Result<(), SafeError> {
    cache_put(key, None);
    safe().delete(key)
}

/// Unlocks the encrypted file with its master password.
pub fn unlock(master_password: &str) -> Result<(), SafeError> {
    let s = safe();
    match s.file() {
        Some(f) => {
            f.unlock(master_password)?;
            // Entries that failed to load while locked can be read now.
            if let Some(cache) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                cache.retain(|_, v| v.is_some());
            }
            Ok(())
        }
        None => Ok(()),
    }
}

/// Sets or removes the master password of the encrypted file.
pub fn set_master_password(password: Option<&str>) -> Result<(), SafeError> {
    match safe().file() {
        Some(f) => f.set_master_password(password),
        None => Err(SafeError::Unavailable(
            "the passwords are not stored in a file".into(),
        )),
    }
}

/// Copies every secret of the current safe into a safe for `to`, removes them
/// from storage the new safe does not use, and installs it. Returns how many
/// secrets were moved; on errors the old safe stays installed.
pub fn switch_storage(to: SafeConfig) -> Result<usize, SafeError> {
    switch_storage_to(Safe::new(to))
}

/// [`switch_storage`] with a prepared safe.
pub fn switch_storage_to(new: Safe) -> Result<usize, SafeError> {
    let old = safe();
    if old.config == new.config {
        return Ok(0);
    }
    new.adopt_unlocked(&old);
    let keys = old.keys()?;
    let mut moved = 0;
    for key in &keys {
        if let Some(v) = old.get(key)? {
            new.set(key, &v)?;
            moved += 1;
        }
    }
    // Remove the old copies from storage the new safe does not use.
    let same_file = old.config.storage != PasswordStorage::Memory
        && new.config.storage != PasswordStorage::Memory
        && old.config.database == new.config.database;
    for key in &keys {
        if let (Some(Ok(n)), None) = (old.native(), new.native.as_ref()) {
            let _ = n.delete(key);
        }
        if let (Some(f), false) = (old.file.as_ref(), same_file) {
            let _ = f.delete(key);
        }
    }
    let mut w = SAFE.write().unwrap_or_else(|e| e.into_inner());
    *w = Some(Arc::new(new));
    Ok(moved)
}

/// Status of the configured safe (may connect to the keyring).
pub fn status() -> SafeStatus {
    safe().status()
}

/// Serializes tests that use the process-wide safe.
#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests;

#[cfg(all(test, unix, not(target_os = "macos")))]
mod native_provider_fixtures {
    use super::*;
    #[test]
    fn kwallet_fallback_is_used_only_when_secret_service_cannot_connect() {
        let backend = unix_keyring(
            || Err("Secret Service unavailable".into()),
            || Ok(Arc::new(MemoryBackend::default()) as Arc<dyn Backend>),
        )
        .unwrap();
        backend.set("github/account", "secret").unwrap();
        assert_eq!(
            backend.get("github/account").unwrap().as_deref(),
            Some("secret")
        );
        let chosen = unix_keyring(
            || Ok(backend.clone()),
            || panic!("must not contact KWallet when Secret Service works"),
        )
        .unwrap();
        assert_eq!(
            chosen.get("github/account").unwrap().as_deref(),
            Some("secret")
        );
        let error = unix_keyring(|| Err("secret error".into()), || Err("wallet error".into()))
            .err()
            .unwrap();
        assert!(error.contains("secret error") && error.contains("wallet error"));
    }
}
