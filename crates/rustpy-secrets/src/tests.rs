//! Password safe tests: keyring fallback, moving secrets between storages, the
//! session cache. Keyrings are faked; the encrypted file is real.

use std::sync::atomic::{AtomicBool, Ordering};

use super::*;

/// A fake keyring that can be switched off.
#[derive(Default)]
struct FakeKeyring {
    entries: MemoryBackend,
    broken: AtomicBool,
}

impl FakeKeyring {
    fn check(&self) -> Result<(), SafeError> {
        if self.broken.load(Ordering::SeqCst) {
            Err(SafeError::Unavailable("keyring daemon stopped".into()))
        } else {
            Ok(())
        }
    }
}

impl Backend for FakeKeyring {
    fn name(&self) -> String {
        "fake keyring".into()
    }
    fn get(&self, key: &str) -> Result<Option<String>, SafeError> {
        self.check()?;
        self.entries.get(key)
    }
    fn set(&self, key: &str, secret: &str) -> Result<(), SafeError> {
        self.check()?;
        self.entries.set(key, secret)
    }
    fn delete(&self, key: &str) -> Result<(), SafeError> {
        self.check()?;
        self.entries.delete(key)
    }
    fn keys(&self) -> Result<Vec<String>, SafeError> {
        self.check()?;
        self.entries.keys()
    }
}

fn config(dir: &Path, storage: PasswordStorage) -> SafeConfig {
    SafeConfig::from_settings(
        &PasswordSettings {
            storage,
            database: String::new(),
        },
        dir,
    )
}

fn fast_file(dir: &Path) -> EncryptedFile {
    EncryptedFile::with_iterations(dir.join("passwords.db"), dir.join("passwords.key"), 1000)
}

fn native_safe(dir: &Path, keyring: Arc<FakeKeyring>) -> Safe {
    Safe::with_native(
        config(dir, PasswordStorage::Native),
        NativeKeyring::new(move || Ok(keyring.clone() as Arc<dyn Backend>)),
    )
    .with_file(fast_file(dir))
}

fn unavailable_safe(dir: &Path) -> Safe {
    Safe::with_native(
        config(dir, PasswordStorage::Native),
        NativeKeyring::new(|| Err("no D-Bus session bus".into())),
    )
    .with_file(fast_file(dir))
}

#[test]
fn native_storage_uses_the_keyring_when_it_works() {
    let dir = tempfile::tempdir().unwrap();
    let keyring = Arc::new(FakeKeyring::default());
    let safe = native_safe(dir.path(), keyring.clone());
    safe.set("k", "v").unwrap();
    assert_eq!(keyring.entries.get("k").unwrap().as_deref(), Some("v"));
    assert!(
        !dir.path().join("passwords.db").exists(),
        "nothing in the fallback file"
    );
    assert_eq!(safe.get("k").unwrap().as_deref(), Some("v"));
    assert_eq!(
        safe.status(),
        SafeStatus {
            location: "fake keyring".into(),
            problem: None,
            locked: false,
            master_password: false
        }
    );
    safe.delete("k").unwrap();
    assert_eq!(safe.get("k").unwrap(), None);
}

#[test]
fn unavailable_keyring_falls_back_to_the_encrypted_file() {
    let dir = tempfile::tempdir().unwrap();
    let safe = unavailable_safe(dir.path());
    safe.set("k", "secret").unwrap();
    assert!(dir.path().join("passwords.db").exists());
    assert_eq!(safe.get("k").unwrap().as_deref(), Some("secret"));
    assert_eq!(safe.keys().unwrap(), vec!["k".to_string()]);
    let status = safe.status();
    assert_eq!(status.problem.as_deref(), Some("no D-Bus session bus"));
    assert!(status.location.starts_with("encrypted file"), "{status:?}");
    safe.delete("k").unwrap();
    assert_eq!(safe.get("k").unwrap(), None);
}

#[test]
fn keyring_failures_fall_back_and_secrets_move_back_later() {
    let dir = tempfile::tempdir().unwrap();
    let keyring = Arc::new(FakeKeyring::default());
    let safe = native_safe(dir.path(), keyring.clone());
    // The keyring stops answering: writes go to the file, reads still work.
    keyring.broken.store(true, Ordering::SeqCst);
    safe.set("ai", "sk-1").unwrap();
    assert_eq!(safe.get("ai").unwrap().as_deref(), Some("sk-1"));
    assert_eq!(
        fast_file(dir.path()).get("ai").unwrap().as_deref(),
        Some("sk-1")
    );
    // Back again: the next read moves the secret into the keyring.
    keyring.broken.store(false, Ordering::SeqCst);
    assert_eq!(safe.get("ai").unwrap().as_deref(), Some("sk-1"));
    assert_eq!(keyring.entries.get("ai").unwrap().as_deref(), Some("sk-1"));
    assert_eq!(
        fast_file(dir.path()).get("ai").unwrap(),
        None,
        "the fallback copy is gone"
    );
    // A keyring write also removes a stale fallback copy.
    keyring.broken.store(true, Ordering::SeqCst);
    safe.set("p", "old").unwrap();
    keyring.broken.store(false, Ordering::SeqCst);
    safe.set("p", "new").unwrap();
    assert_eq!(fast_file(dir.path()).get("p").unwrap(), None);
    assert_eq!(safe.get("p").unwrap().as_deref(), Some("new"));
    assert_eq!(
        safe.keys().unwrap(),
        vec!["ai".to_string(), "p".to_string()]
    );
    // Deleting while the keyring is down still clears the file and reports success.
    keyring.broken.store(true, Ordering::SeqCst);
    safe.delete("x").unwrap();
}

#[test]
fn locked_fallback_file_does_not_hide_working_keyring() {
    let dir = tempfile::tempdir().unwrap();
    let file = fast_file(dir.path());
    file.set("old", "v").unwrap();
    file.set_master_password(Some("pw")).unwrap();
    let keyring = Arc::new(FakeKeyring::default());
    keyring.entries.set("k", "from keyring").unwrap();
    let safe = native_safe(dir.path(), keyring.clone());
    assert_eq!(safe.get("k").unwrap().as_deref(), Some("from keyring"));
    assert_eq!(
        safe.get("missing").unwrap(),
        None,
        "the locked file is not consulted when the keyring answers"
    );
    assert!(safe.status().locked);
    // Without the keyring, the lock is reported.
    keyring.broken.store(true, Ordering::SeqCst);
    assert_eq!(safe.get("old"), Err(SafeError::Locked));
    safe.file().unwrap().unlock("pw").unwrap();
    assert_eq!(safe.get("old").unwrap().as_deref(), Some("v"));
}

#[test]
fn memory_and_file_storage() {
    let dir = tempfile::tempdir().unwrap();
    let mem = Safe::new(config(dir.path(), PasswordStorage::Memory));
    mem.set("k", "v").unwrap();
    assert_eq!(mem.get("k").unwrap().as_deref(), Some("v"));
    assert!(!dir.path().join("passwords.db").exists());
    assert!(mem.status().location.contains("memory"));
    let file = Safe::new(config(dir.path(), PasswordStorage::EncryptedFile))
        .with_file(fast_file(dir.path()));
    file.set("k", "v").unwrap();
    assert_eq!(file.get("k").unwrap().as_deref(), Some("v"));
    assert!(dir.path().join("passwords.db").exists());
    file.file()
        .unwrap()
        .set_master_password(Some("pw"))
        .unwrap();
    let reopened = Safe::new(config(dir.path(), PasswordStorage::EncryptedFile))
        .with_file(fast_file(dir.path()));
    let status = reopened.status();
    assert!(status.locked && status.master_password, "{status:?}");
    assert_eq!(reopened.get("k"), Err(SafeError::Locked));
}

#[test]
fn custom_database_path() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = SafeConfig::from_settings(
        &PasswordSettings {
            storage: PasswordStorage::EncryptedFile,
            database: dir.path().join("x/my.db").display().to_string(),
        },
        dir.path(),
    );
    assert_eq!(cfg.database, dir.path().join("x/my.db"));
    assert_eq!(cfg.key_file, dir.path().join("passwords.key"));
}

#[test]
fn global_safe_cache_and_switching_storage() {
    let _g = test_lock();
    let dir = tempfile::tempdir().unwrap();
    let keyring = Arc::new(FakeKeyring::default());
    install(native_safe(dir.path(), keyring.clone()));
    set(AI_API_KEY, "sk-ant").unwrap();
    assert_eq!(peek(AI_API_KEY).as_deref(), Some("sk-ant"));
    assert_eq!(get(AI_API_KEY).unwrap().as_deref(), Some("sk-ant"));
    // "Remember" off: available this session, removed from storage.
    set_for_session(PROXY_PASSWORD, "proxy-pw").unwrap();
    assert_eq!(get(PROXY_PASSWORD).unwrap().as_deref(), Some("proxy-pw"));
    assert_eq!(keyring.entries.get(PROXY_PASSWORD).unwrap(), None);
    // Native -> encrypted file moves the stored secrets and clears the keyring.
    let to_file = Safe::new(config(dir.path(), PasswordStorage::EncryptedFile))
        .with_file(fast_file(dir.path()));
    assert_eq!(switch_storage_to(to_file).unwrap(), 1);
    assert_eq!(keyring.entries.keys().unwrap(), Vec::<String>::new());
    assert_eq!(
        fast_file(dir.path()).get(AI_API_KEY).unwrap().as_deref(),
        Some("sk-ant")
    );
    assert_eq!(safe().config().storage, PasswordStorage::EncryptedFile);
    // File -> memory empties the file.
    let to_memory = Safe::new(config(dir.path(), PasswordStorage::Memory));
    assert_eq!(switch_storage_to(to_memory).unwrap(), 1);
    assert_eq!(fast_file(dir.path()).keys().unwrap(), Vec::<String>::new());
    assert_eq!(safe().get(AI_API_KEY).unwrap().as_deref(), Some("sk-ant"));
    // Memory -> native.
    assert_eq!(
        switch_storage_to(native_safe(dir.path(), keyring.clone())).unwrap(),
        1
    );
    assert_eq!(
        keyring.entries.get(AI_API_KEY).unwrap().as_deref(),
        Some("sk-ant")
    );
    delete(AI_API_KEY).unwrap();
    assert_eq!(get(AI_API_KEY).unwrap(), None);
    assert_eq!(peek(AI_API_KEY), None);
    // Configuring the same storage again keeps the session cache.
    configure(safe().config().clone());
    assert_eq!(peek(PROXY_PASSWORD).as_deref(), Some("proxy-pw"));
    install(Safe::new(SafeConfig::memory()));
}

#[test]
fn a_secret_kept_for_the_session_survives_an_unlock() {
    let _g = test_lock();
    let dir = tempfile::tempdir().unwrap();
    let file = fast_file(dir.path());
    file.set("k", "stored").unwrap();
    file.set_master_password(Some("pw")).unwrap();
    install(
        Safe::new(config(dir.path(), PasswordStorage::EncryptedFile))
            .with_file(fast_file(dir.path())),
    );
    assert_eq!(set("ftp", "typed"), Err(SafeError::Locked));
    keep_for_session("ftp", "typed");
    assert_eq!(
        get("ftp").unwrap().as_deref(),
        Some("typed"),
        "usable while locked"
    );
    unlock("pw").unwrap();
    assert_eq!(peek("ftp").as_deref(), Some("typed"));
    assert_eq!(safe().get("ftp").unwrap(), None, "storage was not touched");
    install(Safe::new(SafeConfig::memory()));
}

#[test]
fn switching_from_a_locked_file_fails_without_losing_anything() {
    let _g = test_lock();
    let dir = tempfile::tempdir().unwrap();
    let file = fast_file(dir.path());
    file.set("k", "v").unwrap();
    file.set_master_password(Some("pw")).unwrap();
    install(
        Safe::new(config(dir.path(), PasswordStorage::EncryptedFile))
            .with_file(fast_file(dir.path())),
    );
    assert_eq!(get("k"), Err(SafeError::Locked));
    let err =
        switch_storage_to(Safe::new(config(dir.path(), PasswordStorage::Memory))).unwrap_err();
    assert_eq!(err, SafeError::Locked);
    assert_eq!(
        safe().config().storage,
        PasswordStorage::EncryptedFile,
        "the old safe stays"
    );
    assert_eq!(unlock("nope"), Err(SafeError::WrongPassword));
    unlock("pw").unwrap();
    assert_eq!(get("k").unwrap().as_deref(), Some("v"));
    install(Safe::new(SafeConfig::memory()));
}

#[test]
fn switching_keeps_an_unlocked_master_password() {
    let _g = test_lock();
    let dir = tempfile::tempdir().unwrap();
    let file = fast_file(dir.path());
    file.set("k", "v").unwrap();
    file.set_master_password(Some("pw")).unwrap();
    install(
        Safe::new(config(dir.path(), PasswordStorage::EncryptedFile))
            .with_file(fast_file(dir.path())),
    );
    unlock("pw").unwrap();
    // Encrypted file -> keyring, with the same file as fallback: no second unlock.
    let keyring = Arc::new(FakeKeyring::default());
    assert_eq!(
        switch_storage_to(native_safe(dir.path(), keyring.clone())).unwrap(),
        1
    );
    assert_eq!(keyring.entries.get("k").unwrap().as_deref(), Some("v"));
    assert!(!safe().status().locked, "the fallback file stays unlocked");
    let reopened = fast_file(dir.path());
    reopened.unlock("pw").unwrap();
    assert_eq!(
        reopened.get("k").unwrap(),
        None,
        "the secret was moved, not copied"
    );
    install(Safe::new(SafeConfig::memory()));
}

#[test]
fn deployment_keys() {
    assert_eq!(
        deployment_key("ftp", "deploy", "example.com", 21),
        "deployment/ftp://deploy@example.com:21"
    );
}

#[test]
fn reconnects_to_a_keyring_that_starts_later() {
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let a = attempts.clone();
    let native = NativeKeyring::new(move || {
        a.fetch_add(1, Ordering::SeqCst);
        Err("not yet".into())
    });
    assert!(native.backend().is_err());
    assert!(native.backend().is_err());
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "failures are cached for a while"
    );
}
