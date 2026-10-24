//! Encrypted password database (`<config dir>/passwords.db`).
//!
//! The file is JSON: a header plus the AES-256-GCM encryption of
//! `{"entries": {key: secret}}`. The 256-bit key is either
//!
//! * derived from a master password with PBKDF2-HMAC-SHA256 (600 000
//!   iterations, random 16-byte salt) — the database must be unlocked with
//!   the password once per session; or
//! * 32 random bytes in a key file (`passwords.key`, owner-only on Unix), for
//!   when no master password is set: the database alone (backups, synced
//!   folders) reveals nothing.
//!
//! Every write uses a fresh random nonce; the header is authenticated as
//! associated data. Writes go to a temporary owner-only file that replaces the
//! database atomically.
//!
//! Every RustPy window is a process of its own, and they share the database:
//! each change (read, modify, write; creating the key file) runs under an
//! exclusive lock on `<database>.lock` ([`DatabaseLock`]), so concurrent
//! changes from two windows both land, and a key file is created once.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

use crate::{Backend, SafeError};

const FORMAT: &str = "rustpy-passwords";
const VERSION: u32 = 1;
/// PBKDF2 iterations for new master passwords (OWASP 2023 guidance).
pub const DEFAULT_ITERATIONS: u32 = 600_000;
const SALT_LEN: usize = 16;
/// How long a change waits for another process to finish its change.
const LOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// How the database key is obtained.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Protection {
    /// Random key in the key file.
    KeyFile,
    /// Key derived from the master password.
    MasterPassword,
}

impl Protection {
    fn tag(self) -> &'static str {
        match self {
            Protection::KeyFile => "key-file",
            Protection::MasterPassword => "master-password",
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    format: String,
    version: u32,
    protection: String,
    #[serde(default)]
    salt: String,
    #[serde(default)]
    iterations: u32,
    nonce: String,
    data: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Contents {
    entries: BTreeMap<String, String>,
}

/// A master-password key, after unlocking.
#[derive(Clone)]
struct MasterKey {
    key: [u8; 32],
    salt: Vec<u8>,
    iterations: u32,
}

/// The encrypted database file.
pub struct EncryptedFile {
    path: PathBuf,
    key_file: PathBuf,
    iterations: u32,
    master: Mutex<Option<MasterKey>>,
    /// Serializes read-modify-write cycles of this instance's threads; other
    /// instances and processes are kept out by [`DatabaseLock`].
    writing: Mutex<()>,
}

/// An exclusive advisory lock on `<database>.lock`, held while a change reads,
/// modifies and writes the database (and creates the key file). The lock
/// belongs to the open file (`flock` / `LockFileEx`), so it also keeps out
/// other instances in this process; the OS releases it when a process ends.
/// The lock file itself stays (removing it would race with the next locker).
pub(crate) struct DatabaseLock {
    file: Option<std::fs::File>,
}

/// Held during a change; fields drop in order, so the file lock is released
/// before the next thread of this instance can try it.
struct WriteGuard<'a> {
    _processes: DatabaseLock,
    _threads: std::sync::MutexGuard<'a, ()>,
}

/// `<database>.lock`.
pub fn lock_path(database: &Path) -> PathBuf {
    let mut name = database
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".lock");
    database.with_file_name(name)
}

impl DatabaseLock {
    pub(crate) fn acquire(database: &Path) -> Result<DatabaseLock, SafeError> {
        let path = lock_path(database);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| io_err("create", dir, e))?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let file = options.open(&path).map_err(|e| io_err("open", &path, e))?;
        let deadline = Instant::now() + LOCK_TIMEOUT;
        loop {
            match os_lock::try_lock(&file) {
                Ok(true) => return Ok(DatabaseLock { file: Some(file) }),
                Ok(false) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                Ok(false) => return Err(SafeError::Failed(format!("the password database {} is busy: another RustPy window has been changing it for {} s", database.display(), LOCK_TIMEOUT.as_secs()))),
                // Refuse changes rather than silently losing another window's secrets.
                Err(e) if os_lock::unsupported(&e) => {
                    return Err(SafeError::Failed(format!("cannot safely change the password database: {} does not support file locking ({e})", path.display())));
                }
                Err(e) => return Err(io_err("lock", &path, e)),
            }
        }
    }
}

impl Drop for DatabaseLock {
    fn drop(&mut self) {
        if let Some(file) = &self.file {
            os_lock::unlock(file);
        }
    }
}

#[cfg(unix)]
mod os_lock {
    use std::os::unix::io::AsRawFd as _;

    /// Takes the exclusive lock without waiting: false when another holds it.
    pub fn try_lock(file: &std::fs::File) -> std::io::Result<bool> {
        // SAFETY: flock on a file descriptor this `File` owns.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(true);
        }
        let e = std::io::Error::last_os_error();
        match e.kind() {
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted => Ok(false),
            _ => Err(e),
        }
    }

    pub fn unlock(file: &std::fs::File) {
        // SAFETY: as above; closing the file would release the lock as well.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    }

    pub fn unsupported(e: &std::io::Error) -> bool {
        matches!(e.raw_os_error(), Some(code) if code == libc::ENOLCK || code == libc::EOPNOTSUPP || code == libc::ENOTSUP || code == libc::ENOSYS)
    }
}

#[cfg(windows)]
mod os_lock {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        LockFileEx, UnlockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    /// `ERROR_LOCK_VIOLATION`: another handle holds the lock.
    const LOCK_VIOLATION: i32 = 33;

    pub fn try_lock(file: &std::fs::File) -> std::io::Result<bool> {
        // SAFETY: a zeroed OVERLAPPED locks from offset 0; the handle is owned by `file`.
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            LockFileEx(
                file.as_raw_handle(),
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                u32::MAX,
                u32::MAX,
                &mut overlapped,
            )
        };
        if ok != 0 {
            return Ok(true);
        }
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(LOCK_VIOLATION) {
            Ok(false)
        } else {
            Err(e)
        }
    }

    pub fn unlock(file: &std::fs::File) {
        // SAFETY: as above.
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        unsafe { UnlockFileEx(file.as_raw_handle(), 0, u32::MAX, u32::MAX, &mut overlapped) };
    }

    pub fn unsupported(_e: &std::io::Error) -> bool {
        false
    }
}

#[cfg(not(any(unix, windows)))]
mod os_lock {
    pub fn try_lock(_file: &std::fs::File) -> std::io::Result<bool> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no file locks on this platform",
        ))
    }

    pub fn unlock(_file: &std::fs::File) {}

    pub fn unsupported(_e: &std::io::Error) -> bool {
        true
    }
}

impl std::fmt::Debug for EncryptedFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptedFile")
            .field("path", &self.path)
            .field("key_file", &self.key_file)
            .finish()
    }
}

fn aad(protection: Protection, salt: &str, iterations: u32) -> Vec<u8> {
    format!(
        "{FORMAT}:{VERSION}:{}:{salt}:{iterations}",
        protection.tag()
    )
    .into_bytes()
}

fn derive(password: &str, salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut key = [0u8; 32];
    let iterations = NonZeroU32::new(iterations.max(1)).unwrap_or(NonZeroU32::MIN);
    ring::pbkdf2::derive(
        ring::pbkdf2::PBKDF2_HMAC_SHA256,
        iterations,
        salt,
        password.as_bytes(),
        &mut key,
    );
    key
}

fn random<const N: usize>() -> Result<[u8; N], SafeError> {
    let mut b = [0u8; N];
    SystemRandom::new()
        .fill(&mut b)
        .map_err(|_| SafeError::Failed("no secure random numbers available".into()))?;
    Ok(b)
}

fn io_err(what: &str, path: &Path, e: std::io::Error) -> SafeError {
    SafeError::Failed(format!("cannot {what} {}: {e}", path.display()))
}

/// A temporary file name next to `path`, unique per call, also between threads
/// and instances of one process.
fn temp_path(path: &Path) -> PathBuf {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        WRITES.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Options that create a new file (failing when it exists), readable by the
/// owner only (Unix).
fn new_private_file() -> std::fs::OpenOptions {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
}

/// Creates `path` (it must not exist) with `data`, flushed to disk.
fn create_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut f = new_private_file().open(path)?;
    f.write_all(data).and_then(|_| f.sync_all())
}

/// Writes `data` to `path` atomically, readable by the owner only (Unix).
pub(crate) fn write_private(path: &Path, data: &[u8]) -> Result<(), SafeError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| io_err("create", dir, e))?;
    }
    let tmp = temp_path(path);
    let _ = std::fs::remove_file(&tmp);
    if let Err(e) = create_private(&tmp, data) {
        let _ = std::fs::remove_file(&tmp);
        return Err(io_err("write", &tmp, e));
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io_err("write", path, e)
    })
}

impl EncryptedFile {
    pub fn new(path: PathBuf, key_file: PathBuf) -> Self {
        Self::with_iterations(path, key_file, DEFAULT_ITERATIONS)
    }

    /// Like [`EncryptedFile::new`] with a custom PBKDF2 iteration count for new master passwords.
    pub fn with_iterations(path: PathBuf, key_file: PathBuf, iterations: u32) -> Self {
        EncryptedFile {
            path,
            key_file,
            iterations,
            master: Mutex::new(None),
            writing: Mutex::new(()),
        }
    }

    /// Exclusive access for a change: this instance's threads, then other
    /// instances and processes.
    fn write_lock(&self) -> Result<WriteGuard<'_>, SafeError> {
        let threads = self.writing.lock().unwrap_or_else(|e| e.into_inner());
        let processes = DatabaseLock::acquire(&self.path)?;
        Ok(WriteGuard {
            _processes: processes,
            _threads: threads,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn envelope(&self) -> Result<Option<Envelope>, SafeError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_err("read", &self.path, e)),
        };
        let env: Envelope = serde_json::from_str(&text).map_err(|e| {
            SafeError::Failed(format!(
                "{} is not a password database: {e}",
                self.path.display()
            ))
        })?;
        if env.format != FORMAT || env.version != VERSION {
            return Err(SafeError::Failed(format!(
                "{} has an unsupported format",
                self.path.display()
            )));
        }
        Ok(Some(env))
    }

    fn protection_of(env: &Envelope) -> Result<Protection, SafeError> {
        match env.protection.as_str() {
            "key-file" => Ok(Protection::KeyFile),
            "master-password" => Ok(Protection::MasterPassword),
            other => Err(SafeError::Failed(format!(
                "unknown password database protection '{other}'"
            ))),
        }
    }

    /// The protection of the existing database (None when there is no file yet).
    pub fn protection(&self) -> Result<Option<Protection>, SafeError> {
        self.envelope()?
            .map(|e| Self::protection_of(&e))
            .transpose()
    }

    /// The database is protected by a master password that was not entered yet
    /// (or that another window has changed since).
    pub fn is_locked(&self) -> bool {
        match self.envelope() {
            Ok(Some(env)) if Self::protection_of(&env) == Ok(Protection::MasterPassword) => {
                self.master_for(&env).is_err()
            }
            _ => false,
        }
    }

    /// The unlocked master key, when it is the key of the master password `env`
    /// is encrypted with. A key for another salt was unlocked before another
    /// window (a process of its own) changed the master password: it is
    /// forgotten, so the database is locked again and the new password is asked
    /// for, instead of every read failing as a wrong password.
    fn master_for(&self, env: &Envelope) -> Result<MasterKey, SafeError> {
        let mut master = self.master.lock().unwrap_or_else(|e| e.into_inner());
        let current = master.as_ref().is_some_and(|m| {
            m.iterations == env.iterations && B64.decode(&env.salt).is_ok_and(|salt| salt == m.salt)
        });
        if current {
            return master.clone().ok_or(SafeError::Locked);
        }
        if master.take().is_some() {
            log::info!(
                "the master password of {} was changed elsewhere; it is asked for again",
                self.path.display()
            );
        }
        Err(SafeError::Locked)
    }

    fn key_file_key(&self, create: bool) -> Result<[u8; 32], SafeError> {
        // Other databases can share this key. When creating a database, lock
        // before reading too: a creator without hard-link support may have
        // created the key file but not written its bytes yet.
        let _key_lock = if create {
            Some(DatabaseLock::acquire(&self.key_file)?)
        } else {
            None
        };
        match std::fs::read(&self.key_file) {
            Ok(b) if b.len() == 32 => {
                let mut k = [0u8; 32];
                k.copy_from_slice(&b);
                Ok(k)
            }
            Ok(_) => Err(SafeError::Failed(format!(
                "the key file {} is damaged",
                self.key_file.display()
            ))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && create => self.create_key_file(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(SafeError::Failed(format!(
                "the key file {} is missing, so the password database cannot be decrypted",
                self.key_file.display()
            ))),
            Err(e) => Err(io_err("read", &self.key_file, e)),
        }
    }

    /// Creates the key file, unless another process (one with another database
    /// sharing the key file) just did: then its key is used, so a database is
    /// never encrypted with a key that a second creator then replaces. The key
    /// is written in full under a temporary name and then linked into place, so
    /// nobody reads a half-written key file, and a crash leaves none behind.
    fn create_key_file(&self) -> Result<[u8; 32], SafeError> {
        let k: [u8; 32] = random()?;
        if let Some(dir) = self.key_file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| io_err("create", dir, e))?;
        }
        let tmp = temp_path(&self.key_file);
        let _ = std::fs::remove_file(&tmp);
        if let Err(e) = create_private(&tmp, &k) {
            let _ = std::fs::remove_file(&tmp);
            return Err(io_err("write", &tmp, e));
        }
        let linked = std::fs::hard_link(&tmp, &self.key_file);
        let _ = std::fs::remove_file(&tmp);
        match linked {
            Ok(()) => Ok(k),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => self.key_file_key(false),
            Err(e) => {
                log::debug!(
                    "cannot link {} into place ({e}); writing it directly",
                    self.key_file.display()
                );
                self.create_key_file_in_place(k)
            }
        }
    }

    /// [`EncryptedFile::create_key_file`] on a file system without hard links.
    fn create_key_file_in_place(&self, k: [u8; 32]) -> Result<[u8; 32], SafeError> {
        match new_private_file().open(&self.key_file) {
            Ok(mut f) => match f.write_all(&k).and_then(|_| f.sync_all()) {
                Ok(()) => Ok(k),
                Err(e) => {
                    drop(f);
                    let _ = std::fs::remove_file(&self.key_file);
                    Err(io_err("write", &self.key_file, e))
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => self.key_file_key(false),
            Err(e) => Err(io_err("write", &self.key_file, e)),
        }
    }

    fn decrypt(
        env: &Envelope,
        key: &[u8; 32],
        protection: Protection,
    ) -> Result<Contents, SafeError> {
        let nonce: [u8; NONCE_LEN] = B64
            .decode(&env.nonce)
            .ok()
            .and_then(|n| n.try_into().ok())
            .ok_or_else(|| SafeError::Failed("damaged password database (nonce)".into()))?;
        let mut data = B64
            .decode(&env.data)
            .map_err(|_| SafeError::Failed("damaged password database (data)".into()))?;
        let k = LessSafeKey::new(
            UnboundKey::new(&AES_256_GCM, key).map_err(|_| SafeError::Failed("bad key".into()))?,
        );
        let plain = k.open_in_place(Nonce::assume_unique_for_key(nonce), Aad::from(aad(protection, &env.salt, env.iterations)), &mut data).map_err(|_| match protection {
            Protection::MasterPassword => SafeError::WrongPassword,
            Protection::KeyFile => SafeError::Failed("the password database cannot be decrypted with the key file (was the key file replaced?)".into()),
        })?;
        serde_json::from_slice(plain)
            .map_err(|e| SafeError::Failed(format!("damaged password database: {e}")))
    }

    fn encrypt(
        contents: &Contents,
        key: &[u8; 32],
        protection: Protection,
        salt: &[u8],
        iterations: u32,
    ) -> Result<Vec<u8>, SafeError> {
        let salt = B64.encode(salt);
        let nonce: [u8; NONCE_LEN] = random()?;
        let mut data =
            serde_json::to_vec(contents).map_err(|e| SafeError::Failed(e.to_string()))?;
        let k = LessSafeKey::new(
            UnboundKey::new(&AES_256_GCM, key).map_err(|_| SafeError::Failed("bad key".into()))?,
        );
        k.seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(aad(protection, &salt, iterations)),
            &mut data,
        )
        .map_err(|_| SafeError::Failed("encryption failed".into()))?;
        let env = Envelope {
            format: FORMAT.into(),
            version: VERSION,
            protection: protection.tag().into(),
            salt,
            iterations,
            nonce: B64.encode(nonce),
            data: B64.encode(&data),
        };
        let mut text =
            serde_json::to_vec_pretty(&env).map_err(|e| SafeError::Failed(e.to_string()))?;
        text.push(b'\n');
        Ok(text)
    }

    /// Unlocks a master-password database (checks the password by decrypting).
    pub fn unlock(&self, password: &str) -> Result<(), SafeError> {
        let Some(env) = self.envelope()? else {
            return Ok(());
        };
        if Self::protection_of(&env)? != Protection::MasterPassword {
            return Ok(());
        }
        let salt = B64
            .decode(&env.salt)
            .map_err(|_| SafeError::Failed("damaged password database (salt)".into()))?;
        let key = derive(password, &salt, env.iterations);
        Self::decrypt(&env, &key, Protection::MasterPassword)?;
        *self.master.lock().unwrap_or_else(|e| e.into_inner()) = Some(MasterKey {
            key,
            salt,
            iterations: env.iterations,
        });
        Ok(())
    }

    /// Takes over the master key `other` was unlocked with when both are the same
    /// database (a new safe for the same file does not ask again).
    pub fn adopt_unlocked(&self, other: &EncryptedFile) {
        if self.path != other.path {
            return;
        }
        let key = other
            .master
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut mine = self.master.lock().unwrap_or_else(|e| e.into_inner());
        if mine.is_none() {
            *mine = key;
        }
    }

    /// Forgets the unlocked master key.
    pub fn lock(&self) {
        *self.master.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    fn load(&self) -> Result<Contents, SafeError> {
        let Some(env) = self.envelope()? else {
            return Ok(Contents::default());
        };
        match Self::protection_of(&env)? {
            Protection::KeyFile => {
                Self::decrypt(&env, &self.key_file_key(false)?, Protection::KeyFile)
            }
            Protection::MasterPassword => Self::decrypt(
                &env,
                &self.master_for(&env)?.key,
                Protection::MasterPassword,
            ),
        }
    }

    /// Saves with the database's current protection (key file for a new database).
    fn store(&self, contents: &Contents) -> Result<(), SafeError> {
        let protection = self.protection()?.unwrap_or(Protection::KeyFile);
        let bytes = match protection {
            Protection::KeyFile => Self::encrypt(
                contents,
                &self.key_file_key(true)?,
                Protection::KeyFile,
                &[],
                0,
            )?,
            Protection::MasterPassword => {
                let m = self
                    .master
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                    .ok_or(SafeError::Locked)?;
                Self::encrypt(
                    contents,
                    &m.key,
                    Protection::MasterPassword,
                    &m.salt,
                    m.iterations,
                )?
            }
        };
        write_private(&self.path, &bytes)
    }

    /// Sets (`Some`) or removes (`None`: key file protection) the master
    /// password, re-encrypting the entries. A locked database must be unlocked first.
    pub fn set_master_password(&self, password: Option<&str>) -> Result<(), SafeError> {
        let _w = self.write_lock()?;
        let contents = self.load()?;
        let bytes = match password {
            Some(pw) => {
                let salt: [u8; SALT_LEN] = random()?;
                let key = derive(pw, &salt, self.iterations);
                let bytes = Self::encrypt(
                    &contents,
                    &key,
                    Protection::MasterPassword,
                    &salt,
                    self.iterations,
                )?;
                *self.master.lock().unwrap_or_else(|e| e.into_inner()) = Some(MasterKey {
                    key,
                    salt: salt.to_vec(),
                    iterations: self.iterations,
                });
                bytes
            }
            None => {
                let bytes = Self::encrypt(
                    &contents,
                    &self.key_file_key(true)?,
                    Protection::KeyFile,
                    &[],
                    0,
                )?;
                self.lock();
                bytes
            }
        };
        write_private(&self.path, &bytes)
    }

    /// Deletes the database file (all entries). The key file is kept.
    pub fn clear(&self) -> Result<(), SafeError> {
        let _w = self.write_lock()?;
        self.lock();
        match std::fs::remove_file(&self.path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(io_err("delete", &self.path, e))
            }
            _ => Ok(()),
        }
    }
}

impl Backend for EncryptedFile {
    fn name(&self) -> String {
        format!("encrypted file {}", self.path.display())
    }

    fn get(&self, key: &str) -> Result<Option<String>, SafeError> {
        Ok(self.load()?.entries.get(key).cloned())
    }

    fn set(&self, key: &str, secret: &str) -> Result<(), SafeError> {
        let _w = self.write_lock()?;
        let mut c = self.load()?;
        if c.entries.get(key).map(String::as_str) == Some(secret) {
            return Ok(());
        }
        c.entries.insert(key.to_string(), secret.to_string());
        self.store(&c)
    }

    fn delete(&self, key: &str) -> Result<(), SafeError> {
        if !self.path.exists() {
            return Ok(());
        }
        let _w = self.write_lock()?;
        let mut c = self.load()?;
        if c.entries.remove(key).is_some() {
            self.store(&c)?;
        }
        Ok(())
    }

    fn keys(&self) -> Result<Vec<String>, SafeError> {
        Ok(self.load()?.entries.into_keys().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(dir: &Path) -> EncryptedFile {
        EncryptedFile::with_iterations(dir.join("passwords.db"), dir.join("passwords.key"), 1000)
    }

    #[test]
    fn key_file_round_trip_and_privacy() {
        let dir = tempfile::tempdir().unwrap();
        let f = db(dir.path());
        assert_eq!(f.get("a").unwrap(), None);
        assert_eq!(f.protection().unwrap(), None);
        f.delete("a").unwrap();
        assert!(
            !f.path().exists(),
            "deleting from a missing database creates nothing"
        );
        f.set("ai", "sk-ant-secret").unwrap();
        f.set("ftp://u@h:21", "pä$$ wörd").unwrap();
        assert_eq!(f.protection().unwrap(), Some(Protection::KeyFile));
        let raw = std::fs::read_to_string(f.path()).unwrap();
        assert!(
            !raw.contains("sk-ant") && !raw.contains("ftp://"),
            "keys and secrets are encrypted: {raw}"
        );
        // A fresh instance (next session) reads it with the key file.
        let g = db(dir.path());
        assert_eq!(g.get("ai").unwrap().as_deref(), Some("sk-ant-secret"));
        assert_eq!(g.get("ftp://u@h:21").unwrap().as_deref(), Some("pä$$ wörd"));
        assert_eq!(
            g.keys().unwrap(),
            vec!["ai".to_string(), "ftp://u@h:21".to_string()]
        );
        g.delete("ai").unwrap();
        assert_eq!(f.get("ai").unwrap(), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            for p in [f.path(), &dir.path().join("passwords.key")] {
                assert_eq!(
                    std::fs::metadata(p).unwrap().permissions().mode() & 0o777,
                    0o600,
                    "{}",
                    p.display()
                );
            }
        }
        // Without the key file the database cannot be read.
        std::fs::remove_file(dir.path().join("passwords.key")).unwrap();
        assert!(
            matches!(db(dir.path()).get("ftp://u@h:21"), Err(SafeError::Failed(m)) if m.contains("key file"))
        );
    }

    #[test]
    fn master_password_lock_unlock_and_change() {
        let dir = tempfile::tempdir().unwrap();
        let f = db(dir.path());
        f.set("k", "v").unwrap();
        f.set_master_password(Some("correct horse")).unwrap();
        assert_eq!(f.protection().unwrap(), Some(Protection::MasterPassword));
        assert!(
            !f.is_locked(),
            "setting the password keeps the session unlocked"
        );
        f.set("k2", "v2").unwrap();
        // Next session: locked until the password is entered.
        let g = db(dir.path());
        assert!(g.is_locked());
        assert_eq!(g.get("k"), Err(SafeError::Locked));
        assert_eq!(g.set("x", "y"), Err(SafeError::Locked));
        assert_eq!(g.unlock("wrong"), Err(SafeError::WrongPassword));
        assert!(g.is_locked());
        g.unlock("correct horse").unwrap();
        assert_eq!(g.get("k2").unwrap().as_deref(), Some("v2"));
        // Change, then remove the master password.
        g.set_master_password(Some("new pw")).unwrap();
        let h = db(dir.path());
        assert_eq!(h.unlock("correct horse"), Err(SafeError::WrongPassword));
        h.unlock("new pw").unwrap();
        h.set_master_password(None).unwrap();
        let i = db(dir.path());
        assert!(!i.is_locked());
        assert_eq!(i.get("k").unwrap().as_deref(), Some("v"));
        i.clear().unwrap();
        assert_eq!(i.get("k").unwrap(), None);
    }

    #[test]
    fn concurrent_writers_keep_every_entry() {
        let dir = tempfile::tempdir().unwrap();
        let f = std::sync::Arc::new(db(dir.path()));
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let f = f.clone();
                std::thread::spawn(move || {
                    for j in 0..5 {
                        f.set(&format!("k{i}-{j}"), "v").unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(f.keys().unwrap().len(), 40);
    }

    /// Several instances of one database (each RustPy window has its own) store
    /// different secrets at once, on a fresh folder: every secret lands, and the
    /// database stays readable with the key file.
    #[test]
    fn concurrent_instances_keep_every_entry_and_one_key() {
        for trial in 0..25 {
            let dir = tempfile::tempdir().unwrap();
            let start = std::sync::Arc::new(std::sync::Barrier::new(4));
            let threads: Vec<_> = (0..4)
                .map(|i| {
                    let (dir, start) = (dir.path().to_path_buf(), start.clone());
                    std::thread::spawn(move || {
                        let mine = db(&dir);
                        start.wait();
                        mine.set(&format!("k{i}"), "v").unwrap();
                    })
                })
                .collect();
            for t in threads {
                t.join().unwrap();
            }
            let keys = db(dir.path())
                .keys()
                .unwrap_or_else(|e| panic!("trial {trial}: {e}"));
            assert_eq!(keys, vec!["k0", "k1", "k2", "k3"], "trial {trial}");
        }
    }

    /// The other half of [`concurrent_processes_keep_every_entry`]: in a child
    /// process, waits for the start file and stores one secret.
    #[test]
    fn child_writer() {
        let (Ok(dir), Ok(key)) = (
            std::env::var("RUSTPY_SECRETS_TEST_DIR"),
            std::env::var("RUSTPY_SECRETS_TEST_KEY"),
        ) else {
            return;
        };
        let dir = PathBuf::from(dir);
        let f = db(&dir);
        let deadline = Instant::now() + Duration::from_secs(30);
        while !dir.join("go").exists() {
            assert!(Instant::now() < deadline, "no start signal");
            std::thread::yield_now();
        }
        f.set(&key, "from another process").unwrap();
    }

    /// RustPy windows are separate processes: secrets they store at the same
    /// moment all land, on a fresh folder (where the key file is created too).
    #[test]
    fn concurrent_processes_keep_every_entry() {
        let exe = std::env::current_exe().unwrap();
        for trial in 0..6 {
            let dir = tempfile::tempdir().unwrap();
            let children: Vec<_> = (0..3)
                .map(|i| {
                    std::process::Command::new(&exe)
                        .args([
                            "file::tests::child_writer",
                            "--exact",
                            "--nocapture",
                            "--test-threads=1",
                        ])
                        .env("RUSTPY_SECRETS_TEST_DIR", dir.path())
                        .env("RUSTPY_SECRETS_TEST_KEY", format!("window{i}"))
                        .stdout(std::process::Stdio::null())
                        .spawn()
                        .unwrap()
                })
                .collect();
            // Give the children time to start, then let them all write at once.
            std::thread::sleep(Duration::from_millis(300));
            std::fs::write(dir.path().join("go"), "").unwrap();
            for mut child in children {
                assert!(
                    child.wait().unwrap().success(),
                    "trial {trial}: a child failed"
                );
            }
            let keys = db(dir.path())
                .keys()
                .unwrap_or_else(|e| panic!("trial {trial}: {e}"));
            assert_eq!(keys, vec!["window0", "window1", "window2"], "trial {trial}");
        }
    }

    #[test]
    fn lock_files_sit_next_to_the_database() {
        assert_eq!(
            lock_path(Path::new("/c/passwords.db")),
            PathBuf::from("/c/passwords.db.lock")
        );
        let dir = tempfile::tempdir().unwrap();
        let f = db(dir.path());
        f.set("k", "v").unwrap();
        assert!(dir.path().join("passwords.db.lock").exists());
        // While another instance holds the lock, a change waits for it.
        let held = DatabaseLock::acquire(f.path()).unwrap();
        let other = std::thread::spawn({
            let dir = dir.path().to_path_buf();
            move || {
                let started = Instant::now();
                db(&dir).set("k2", "v2").unwrap();
                started.elapsed()
            }
        });
        std::thread::sleep(Duration::from_millis(200));
        drop(held);
        assert!(
            other.join().unwrap() >= Duration::from_millis(150),
            "the change waited for the lock"
        );
        assert_eq!(f.keys().unwrap(), vec!["k".to_string(), "k2".to_string()]);
        // Temporary files of writes do not collide or stay behind.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// Two windows share the database: when one changes the master password, the
    /// other's unlocked key is stale. It is locked again (the UI asks for the new
    /// password) instead of failing every read with "wrong master password".
    #[test]
    fn a_master_password_changed_in_another_window_locks_this_one() {
        let dir = tempfile::tempdir().unwrap();
        let first = db(dir.path());
        first.set("k", "v").unwrap();
        first.set_master_password(Some("old")).unwrap();
        let second = db(dir.path());
        second.unlock("old").unwrap();
        assert_eq!(second.get("k").unwrap().as_deref(), Some("v"));
        first.set_master_password(Some("new")).unwrap();
        assert!(second.is_locked());
        assert_eq!(second.get("k"), Err(SafeError::Locked));
        assert_eq!(second.set("k2", "v2"), Err(SafeError::Locked));
        assert_eq!(second.unlock("old"), Err(SafeError::WrongPassword));
        second.unlock("new").unwrap();
        assert_eq!(second.get("k").unwrap().as_deref(), Some("v"));
        second.set("k2", "v2").unwrap();
        // Removed and set again elsewhere: the key from before is stale too.
        first.unlock("new").unwrap();
        first.set_master_password(None).unwrap();
        assert!(
            !second.is_locked(),
            "a key-file database needs no master password"
        );
        assert_eq!(second.get("k2").unwrap().as_deref(), Some("v2"));
        first.set_master_password(Some("third")).unwrap();
        assert_eq!(second.get("k"), Err(SafeError::Locked));
    }

    /// Databases in other folders share the key file (`<config dir>/passwords.key`).
    /// When they are created at the same moment, each process either creates the
    /// key file or reads the one another created, never a half-written one.
    #[test]
    fn concurrent_databases_share_one_complete_key_file() {
        for trial in 0..40 {
            let dir = tempfile::tempdir().unwrap();
            let start = std::sync::Arc::new(std::sync::Barrier::new(6));
            let threads: Vec<_> = (0..6)
                .map(|i| {
                    let (dir, start) = (dir.path().to_path_buf(), start.clone());
                    std::thread::spawn(move || {
                        let mine = EncryptedFile::with_iterations(
                            dir.join(format!("db{i}/passwords.db")),
                            dir.join("passwords.key"),
                            1000,
                        );
                        start.wait();
                        mine.set("k", &format!("v{i}"))
                    })
                })
                .collect();
            for (i, t) in threads.into_iter().enumerate() {
                t.join()
                    .unwrap()
                    .unwrap_or_else(|e| panic!("trial {trial}, database {i}: {e}"));
            }
            for i in 0..6 {
                let reopened = EncryptedFile::with_iterations(
                    dir.path().join(format!("db{i}/passwords.db")),
                    dir.path().join("passwords.key"),
                    1000,
                );
                assert_eq!(
                    reopened
                        .get("k")
                        .unwrap_or_else(|e| panic!("trial {trial}, database {i}: {e}")),
                    Some(format!("v{i}"))
                );
            }
        }
    }

    #[test]
    fn shared_key_read_waits_for_a_creator_to_finish_writing() {
        let dir = tempfile::tempdir().unwrap();
        let key_file = dir.path().join("passwords.key");
        let held = DatabaseLock::acquire(&key_file).unwrap();
        // Simulate the non-hard-link fallback, which publishes an empty file
        // before writing it. Another database must wait instead of rejecting
        // this temporary state as a damaged key.
        create_private(&key_file, &[]).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let path = dir.path().to_path_buf();
        let worker = std::thread::spawn(move || tx.send(db(&path).set("k", "v")).unwrap());
        assert!(matches!(
            rx.recv_timeout(Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        std::fs::write(&key_file, [7u8; 32]).unwrap();
        drop(held);
        rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
        worker.join().unwrap();
        assert_eq!(db(dir.path()).get("k").unwrap(), Some("v".into()));
    }

    #[test]
    fn tampering_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let f = db(dir.path());
        f.set("k", "v").unwrap();
        f.set_master_password(Some("pw")).unwrap();
        let text = std::fs::read_to_string(f.path()).unwrap();
        // Downgrading the header to key-file protection breaks authentication.
        let forged = text.replace("\"master-password\"", "\"key-file\"");
        std::fs::write(f.path(), forged).unwrap();
        assert!(db(dir.path()).get("k").is_err());
        std::fs::write(f.path(), "{not json").unwrap();
        assert!(
            matches!(db(dir.path()).get("k"), Err(SafeError::Failed(m)) if m.contains("not a password database"))
        );
        std::fs::write(f.path(), text.replace("\"version\": 1", "\"version\": 9")).unwrap();
        assert!(
            matches!(db(dir.path()).get("k"), Err(SafeError::Failed(m)) if m.contains("unsupported"))
        );
    }
}
