//! Cooperative configuration-wide admission barrier for guarded application Quit.
//!
//! Every cooperating owner mutation must retain a shared lease through store and
//! endpoint publication. Quit retains an exclusive lease before Prepare through
//! all remote Commit acknowledgements and local-last Commit. These leases do not
//! authorize UI exit, establish UI guards, or exclude external file writers.
//!
//! Acquire startup admission BEFORE `instance::launch` creates/publishes a
//! Server. Drop it on `Launch::Done`; retain it on `Launch::Start` through
//! restore validation, logical acceptance and endpoint project publication.
//! An already-published newborn endpoint could itself make crash recovery
//! uncertain. Project changes likewise acquire before either publication.
//! These APIs are a service contract; UI callers must wire every mutation.
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum AdmissionError {
    Io(io::Error),
    Store(rustpy_settings::window_session_store::WindowSessionError),
    Deadline,
    IdentityChanged,
    PreviousQuitPending,
}
impl std::fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "window admission: {e}"),
            Self::Store(e) => write!(f, "window admission: {e}"),
            Self::Deadline => f.write_str("window admission deadline expired"),
            Self::IdentityChanged => f.write_str("window admission lock identity changed"),
            Self::PreviousQuitPending => {
                f.write_str("previous guarded Quit participants remain live or uncertain")
            }
        }
    }
}
impl std::error::Error for AdmissionError {}
impl From<io::Error> for AdmissionError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<rustpy_settings::window_session_store::WindowSessionError> for AdmissionError {
    fn from(e: rustpy_settings::window_session_store::WindowSessionError) -> Self {
        Self::Store(e)
    }
}

#[derive(Debug)]
pub struct WindowAdmission {
    config: PathBuf,
}
impl WindowAdmission {
    pub fn new(config: &Path) -> Result<Self, AdmissionError> {
        std::fs::create_dir_all(config)?;
        Ok(Self {
            config: std::fs::canonicalize(config)?,
        })
    }
    pub fn acquire_shared(&self, deadline: Instant) -> Result<AdmissionLease, AdmissionError> {
        self.acquire(false, deadline).map(AdmissionLease)
    }
    pub fn acquire_quit(&self, deadline: Instant) -> Result<QuitAdmissionLease, AdmissionError> {
        self.acquire(true, deadline).map(QuitAdmissionLease)
    }
    fn acquire(&self, exclusive: bool, deadline: Instant) -> Result<Lease, AdmissionError> {
        #[cfg(windows)]
        let config_handle = {
            use std::os::windows::fs::OpenOptionsExt;
            OpenOptions::new()
                .read(true)
                .share_mode(3)
                .custom_flags(0x02000000)
                .open(&self.config)?
        };
        let path = self.config.join("window-admission.lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Share read/write, but prohibit replacement while any lease is open.
            options.share_mode(3).custom_flags(0x00200000);
        }
        let file = options.open(&path)?;
        if !file.metadata()?.is_file() {
            return Err(AdmissionError::IdentityChanged);
        }
        loop {
            if Instant::now() >= deadline {
                return Err(AdmissionError::Deadline);
            }
            match os::lock(&file, exclusive) {
                Ok(true) => break,
                Ok(false) => std::thread::sleep(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(5)),
                ),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
        let lease = Lease {
            config: self.config.clone(),
            #[cfg(unix)]
            config_metadata: std::fs::metadata(&self.config)?,
            #[cfg(windows)]
            _config_handle: config_handle,
            path,
            file,
        };
        lease.validate()?;
        let snapshot =
            rustpy_settings::window_session_store::WindowSessionStore::new(&self.config).load()?;
        if let Some(certificate) = snapshot.guarded_quit_certificate() {
            let presence = crate::window_registry::presence_until(&self.config, deadline)?;
            if !presence.uncertain.is_empty()
                || presence.live.iter().any(|live| {
                    certificate
                        .cohort
                        .iter()
                        .any(|old| old.window_id == live.id)
                })
            {
                return Err(AdmissionError::PreviousQuitPending);
            }
        }
        if Instant::now() >= deadline {
            return Err(AdmissionError::Deadline);
        }
        lease.validate()?;
        Ok(lease)
    }
}
#[derive(Debug)]
struct Lease {
    config: PathBuf,
    #[cfg(unix)]
    config_metadata: std::fs::Metadata,
    #[cfg(windows)]
    _config_handle: File,
    path: PathBuf,
    file: File,
}
impl Lease {
    fn validate(&self) -> Result<(), AdmissionError> {
        if std::fs::canonicalize(&self.config)? != self.config {
            return Err(AdmissionError::IdentityChanged);
        }
        let current = std::fs::symlink_metadata(&self.path)?;
        if !current.is_file() {
            return Err(AdmissionError::IdentityChanged);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let config = std::fs::metadata(&self.config)?;
            if (config.dev(), config.ino())
                != (self.config_metadata.dev(), self.config_metadata.ino())
            {
                return Err(AdmissionError::IdentityChanged);
            }
            let held = self.file.metadata()?;
            if (current.dev(), current.ino()) != (held.dev(), held.ino()) {
                return Err(AdmissionError::IdentityChanged);
            }
        }
        Ok(())
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = os::unlock(&self.file);
    }
}
#[derive(Debug)]
pub struct AdmissionLease(Lease);
#[derive(Debug)]
pub struct QuitAdmissionLease(Lease);
impl AdmissionLease {
    /// Recheck immediately before each guarded mutation/publication.
    pub fn validate_identity(&self) -> Result<(), AdmissionError> {
        self.0.validate()
    }
    pub fn configuration(&self) -> &Path {
        &self.0.config
    }
}
impl QuitAdmissionLease {
    pub fn validate_identity(&self) -> Result<(), AdmissionError> {
        self.0.validate()
    }
    pub fn configuration(&self) -> &Path {
        &self.0.config
    }
}
#[cfg(unix)]
mod os {
    use super::*;
    use std::os::fd::AsRawFd;
    pub fn lock(file: &File, exclusive: bool) -> io::Result<bool> {
        let operation = if exclusive {
            libc::LOCK_EX
        } else {
            libc::LOCK_SH
        };
        // SAFETY: live file descriptor, documented flock flags.
        if unsafe { libc::flock(file.as_raw_fd(), operation | libc::LOCK_NB) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            Ok(false)
        } else {
            Err(error)
        }
    }
    pub fn unlock(file: &File) -> io::Result<()> {
        loop {
            // SAFETY: live file descriptor and documented unlock operation.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) } == 0 {
                return Ok(());
            }
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }
}
#[cfg(windows)]
mod os {
    use super::*;
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: u32,
        offset_high: u32,
        event: *mut c_void,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn LockFileEx(
            handle: *mut c_void,
            flags: u32,
            reserved: u32,
            low: u32,
            high: u32,
            overlapped: *mut Overlapped,
        ) -> i32;
        fn UnlockFileEx(
            handle: *mut c_void,
            reserved: u32,
            low: u32,
            high: u32,
            overlapped: *mut Overlapped,
        ) -> i32;
    }
    fn position() -> Overlapped {
        Overlapped {
            internal: 0,
            internal_high: 0,
            offset: 0,
            offset_high: 0,
            event: std::ptr::null_mut(),
        }
    }
    pub fn lock(file: &File, exclusive: bool) -> io::Result<bool> {
        let mut position = position();
        // SAFETY: valid synchronous file handle, initialized OVERLAPPED and one-byte range.
        if unsafe {
            LockFileEx(
                file.as_raw_handle(),
                1 | if exclusive { 2 } else { 0 },
                0,
                1,
                0,
                &mut position,
            )
        } != 0
        {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(33) {
            Ok(false)
        } else {
            Err(error)
        }
    }
    pub fn unlock(file: &File) -> io::Result<()> {
        let mut position = position();
        // SAFETY: same live handle and range used by LockFileEx.
        if unsafe { UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut position) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}
#[cfg(not(any(unix, windows)))]
mod os {
    use super::*;
    pub fn lock(_: &File, _: bool) -> io::Result<bool> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "window admission requires OS locks",
        ))
    }
    pub fn unlock(_: &File) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_settings::window_session_store::{
        GuardedQuitBinding, GuardedQuitWindow, QuitUiGuard, WindowOwnerId, WindowSessionStore,
    };
    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(3)
    }
    fn certify(config: &Path, id: &str) {
        let store = WindowSessionStore::new(config);
        let snapshot = store
            .accept_window(&store.load().unwrap(), WindowOwnerId::new(), None)
            .unwrap()
            .snapshot;
        let live = &snapshot.live_windows()[0];
        let binding = GuardedQuitBinding {
            transaction_id: "admission-test".into(),
            cohort: vec![GuardedQuitWindow {
                owner: live.token.owner().clone(),
                generation: live.token.generation(),
                owner_revision: live.token.revision(),
                window_id: id.into(),
                project: None,
                guard: QuitUiGuard {
                    nonce: "guard-nonce".into(),
                    state: "guard-state".into(),
                },
            }],
        };
        store
            .commit_guarded_quit(store.prepare_quit(&snapshot).unwrap(), binding)
            .unwrap();
    }
    #[test]
    fn shared_coexists_but_quit_excludes_admission() {
        let config = tempfile::tempdir().unwrap();
        let gate = WindowAdmission::new(config.path()).unwrap();
        let first = gate.acquire_shared(deadline()).unwrap();
        let second = gate.acquire_shared(deadline()).unwrap();
        assert!(matches!(
            gate.acquire_quit(Instant::now() + Duration::from_millis(20)),
            Err(AdmissionError::Deadline)
        ));
        drop((first, second));
        let quit = gate.acquire_quit(deadline()).unwrap();
        assert!(matches!(
            gate.acquire_shared(Instant::now() + Duration::from_millis(20)),
            Err(AdmissionError::Deadline)
        ));
        drop(quit);
        gate.acquire_shared(deadline()).unwrap();
    }
    #[test]
    fn crashed_quit_live_certificate_blocks_and_preserves_checkpoint() {
        let config = tempfile::tempdir().unwrap();
        let server = crate::ipc::Server::start_secondary(config.path()).unwrap();
        certify(config.path(), &server.window_id());
        let path = WindowSessionStore::new(config.path()).path();
        let bytes = std::fs::read(&path).unwrap();
        let gate = WindowAdmission::new(config.path()).unwrap();
        assert!(matches!(
            gate.acquire_shared(deadline()),
            Err(AdmissionError::PreviousQuitPending)
        ));
        assert!(matches!(
            gate.acquire_quit(deadline()),
            Err(AdmissionError::PreviousQuitPending)
        ));
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
    #[test]
    fn certified_absence_allows_admission_without_erasing_checkpoint() {
        let config = tempfile::tempdir().unwrap();
        certify(config.path(), "absent-certified-endpoint");
        let path = WindowSessionStore::new(config.path()).path();
        let bytes = std::fs::read(&path).unwrap();
        WindowAdmission::new(config.path())
            .unwrap()
            .acquire_shared(deadline())
            .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
    #[test]
    fn corrupt_state_is_not_admitted_or_overwritten() {
        let config = tempfile::tempdir().unwrap();
        let path = WindowSessionStore::new(config.path()).path();
        std::fs::write(&path, b"{").unwrap();
        assert!(matches!(
            WindowAdmission::new(config.path())
                .unwrap()
                .acquire_shared(deadline()),
            Err(AdmissionError::Store(_))
        ));
        assert_eq!(std::fs::read(path).unwrap(), b"{");
    }
    #[cfg(unix)]
    #[test]
    fn replacement_detected_and_drop_unlocks_shared_description() {
        let config = tempfile::tempdir().unwrap();
        let gate = WindowAdmission::new(config.path()).unwrap();
        let lease = gate.acquire_shared(deadline()).unwrap();
        let clone = lease.0.file.try_clone().unwrap();
        drop(lease);
        gate.acquire_quit(deadline()).unwrap();
        drop(clone);
        let lease = gate.acquire_shared(deadline()).unwrap();
        std::fs::remove_file(&lease.0.path).unwrap();
        std::fs::write(&lease.0.path, b"").unwrap();
        assert!(matches!(
            lease.validate_identity(),
            Err(AdmissionError::IdentityChanged)
        ));
    }
    #[test]
    fn process_helper() {
        let Some(config) = std::env::var_os("RUSTPY_ADMISSION_TEST_CONFIG") else {
            return;
        };
        let config = PathBuf::from(config);
        let gate = WindowAdmission::new(&config).unwrap();
        let mode = std::env::var("RUSTPY_ADMISSION_TEST_MODE").unwrap();
        let _shared;
        let _quit;
        if mode == "shared" {
            _shared = Some(gate.acquire_shared(deadline()).unwrap());
            _quit = None;
        } else {
            _shared = None;
            _quit = Some(gate.acquire_quit(deadline()).unwrap());
        }
        std::fs::write(config.join("helper-ready"), b"ready").unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        while !config.join("helper-stop").exists() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            config.join("helper-stop").exists(),
            "helper release not received"
        );
    }
    struct Helper {
        child: std::process::Child,
        config: PathBuf,
    }
    impl Drop for Helper {
        fn drop(&mut self) {
            let _ = std::fs::write(self.config.join("helper-stop"), b"stop");
            let _ = self.child.wait();
        }
    }
    #[test]
    fn actual_process_shared_and_exclusive_lifetimes() {
        for mode in ["shared", "quit"] {
            let config = tempfile::tempdir().unwrap();
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "window_admission::tests::process_helper",
                    "--nocapture",
                ])
                .env("RUSTPY_ADMISSION_TEST_CONFIG", config.path())
                .env("RUSTPY_ADMISSION_TEST_MODE", mode)
                .env_remove("DISPLAY")
                .env_remove("WAYLAND_DISPLAY")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let mut helper = Helper {
                child,
                config: config.path().into(),
            };
            drop(helper.child.stdin.take());
            let end = deadline();
            while !config.path().join("helper-ready").exists() && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(config.path().join("helper-ready").exists());
            let gate = WindowAdmission::new(config.path()).unwrap();
            if mode == "shared" {
                gate.acquire_shared(deadline()).unwrap();
            } else {
                assert!(matches!(
                    gate.acquire_shared(Instant::now() + Duration::from_millis(20)),
                    Err(AdmissionError::Deadline)
                ));
            }
            assert!(matches!(
                gate.acquire_quit(Instant::now() + Duration::from_millis(20)),
                Err(AdmissionError::Deadline)
            ));
            std::fs::write(config.path().join("helper-stop"), b"stop").unwrap();
            assert!(helper.child.wait().unwrap().success());
            gate.acquire_quit(deadline()).unwrap();
        }
    }
    #[test]
    fn certified_unknown_registry_lease_blocks_admission() {
        let config = tempfile::tempdir().unwrap();
        certify(config.path(), "old-endpoint");
        let directory = config
            .path()
            .join("windows")
            .join(crate::ipc::new_token().unwrap());
        std::fs::create_dir_all(&directory).unwrap();
        let lease = crate::ipc::try_lock_file(&directory.join("lease"))
            .unwrap()
            .unwrap();
        let gate = WindowAdmission::new(config.path()).unwrap();
        assert!(matches!(
            gate.acquire_shared(deadline()),
            Err(AdmissionError::PreviousQuitPending)
        ));
        os::unlock(lease.borrowed_file_for_test()).unwrap();
        drop(lease);
        gate.acquire_shared(deadline()).unwrap();
    }
    /// A crash/raw-close model deliberately has no owning FileLease Drop.
    /// Ordinary owning-guard Drop explicitly unlocks inherited descriptions;
    /// this fixture instead retains the kernel description until manual unlock.
    #[cfg(unix)]
    fn raw_registry_lease_for_crash_fixture(path: &Path) -> File {
        use std::os::unix::fs::OpenOptionsExt;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .unwrap();
        assert!(file.metadata().unwrap().is_file());
        assert!(os::lock(&file, true).unwrap());
        file
    }
    #[cfg(unix)]
    #[test]
    fn duplicate_description_requires_explicit_unlock_before_certified_absence() {
        let config = tempfile::tempdir().unwrap();
        certify(config.path(), "old-endpoint");
        let directory = config
            .path()
            .join("windows")
            .join(crate::ipc::new_token().unwrap());
        std::fs::create_dir_all(&directory).unwrap();
        let lease = raw_registry_lease_for_crash_fixture(&directory.join("lease"));
        let duplicate = lease.try_clone().unwrap();
        drop(lease);
        let gate = WindowAdmission::new(config.path()).unwrap();
        assert!(matches!(
            gate.acquire_shared(deadline()),
            Err(AdmissionError::PreviousQuitPending)
        ));
        os::unlock(&duplicate).unwrap();
        // Descriptor remains open: explicit unlock, rather than last-close,
        // establishes absence even with another reference to the description.
        gate.acquire_shared(deadline()).unwrap();
        drop(duplicate);
        gate.acquire_shared(deadline()).unwrap();
    }
    #[test]
    fn coordinator_process_crash_releases_gate_but_not_certificate_barrier() {
        let config = tempfile::tempdir().unwrap();
        let server = crate::ipc::Server::start_secondary(config.path()).unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "window_admission::tests::process_helper",
                "--nocapture",
            ])
            .env("RUSTPY_ADMISSION_TEST_CONFIG", config.path())
            .env("RUSTPY_ADMISSION_TEST_MODE", "quit")
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut helper = Helper {
            child,
            config: config.path().into(),
        };
        drop(helper.child.stdin.take());
        let end = deadline();
        while !config.path().join("helper-ready").exists() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(config.path().join("helper-ready").exists());
        certify(config.path(), &server.window_id());
        helper.child.kill().unwrap();
        assert!(!helper.child.wait().unwrap().success());
        let path = WindowSessionStore::new(config.path()).path();
        let bytes = std::fs::read(&path).unwrap();
        let gate = WindowAdmission::new(config.path()).unwrap();
        assert!(matches!(
            gate.acquire_shared(deadline()),
            Err(AdmissionError::PreviousQuitPending)
        ));
        assert!(matches!(
            gate.acquire_quit(deadline()),
            Err(AdmissionError::PreviousQuitPending)
        ));
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
}
