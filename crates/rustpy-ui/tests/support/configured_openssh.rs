//! Shared genuine private OpenSSH fixture. No system daemon/config or fake SSH.
use rustpy_deploy::exec::{self, CommandSpec};
use rustpy_run::process_owner::ProcessOwner;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub(super) fn executable(name: &str) -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os(name).unwrap_or_else(|| panic!("required explicit executable {name}")),
    );
    assert!(
        path.is_absolute() && path.is_file(),
        "{name} must select an absolute executable"
    );
    path
}

pub(super) fn run(program: &Path, args: &[String]) -> exec::Output {
    let output = exec::run(
        &CommandSpec::new(program.to_string_lossy()).args(args.iter().cloned()),
        Duration::from_secs(15),
    )
    .unwrap_or_else(|error| panic!("admitted tool {} failed: {error}", program.display()));
    assert!(output.success, "{}: {}", program.display(), output.stderr);
    output
}

pub(super) fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Identity {
    pub(super) pid: u32,
    pub(super) start: u64,
    pub(super) parent: u32,
}
pub(super) fn identity(pid: u32) -> Option<Identity> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<_> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    Some(Identity {
        pid,
        parent: fields.get(1)?.parse().ok()?,
        start: fields.get(19)?.parse().ok()?,
    })
}
pub(super) fn is_owned_descendant(mut child: Identity, ancestor: Identity) -> bool {
    for _ in 0..64 {
        if child.pid == ancestor.pid {
            return child.start == ancestor.start;
        }
        let Some(parent) = identity(child.parent) else {
            return false;
        };
        if parent.pid == child.pid {
            return false;
        }
        child = parent;
    }
    false
}
pub(super) fn retired(captured: Identity) -> bool {
    identity(captured.pid).is_none_or(|current| current.start != captured.start)
}

pub(super) struct Subreaper {
    #[cfg(target_os = "linux")]
    previous: i32,
}
impl Subreaper {
    pub(super) fn enter() -> Self {
        #[cfg(target_os = "linux")]
        {
            let mut previous = 0i32;
            assert_eq!(
                unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut previous as *mut i32) },
                0
            );
            assert_eq!(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) }, 0);
            Self { previous }
        }
        #[cfg(not(target_os = "linux"))]
        panic!("real SSH session reaping acceptance requires Linux");
    }
}
impl Drop for Subreaper {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, self.previous) } != 0 {
            let error = std::io::Error::last_os_error();
            if std::thread::panicking() {
                eprintln!("cannot restore subreaper: {error}");
            } else {
                panic!("cannot restore subreaper: {error}");
            }
        }
    }
}

pub(super) struct PinnedSession {
    pub(super) captured: Identity,
    #[cfg(target_os = "linux")]
    descriptor: i32,
}
impl PinnedSession {
    #[cfg(target_os = "linux")]
    pub(super) fn capture(ancestor: Identity) -> Vec<Self> {
        let mut sessions = Vec::new();
        for entry in std::fs::read_dir("/proc").unwrap().flatten() {
            let Some(captured) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<u32>().ok())
                .and_then(identity)
            else {
                continue;
            };
            if captured.pid == ancestor.pid || !is_owned_descendant(captured, ancestor) {
                continue;
            }
            let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, captured.pid, 0) } as i32;
            if descriptor < 0 {
                assert!(
                    retired(captured),
                    "cannot pin live owned SSH descendant: {}",
                    std::io::Error::last_os_error()
                );
                continue;
            }
            let pinned = Self {
                captured,
                descriptor,
            };
            // Revalidate after pinning, including ancestry. Never signal a
            // reused PID or a process that was not this daemon's descendant.
            if !retired(captured) && is_owned_descendant(captured, ancestor) {
                sessions.push(pinned);
            }
        }
        sessions
    }
    #[cfg(not(target_os = "linux"))]
    pub(super) fn capture(_ancestor: Identity) -> Vec<Self> {
        panic!("real SSH session pinning acceptance requires Linux");
    }
    pub(super) fn terminate(&self) {
        #[cfg(target_os = "linux")]
        if !retired(self.captured) {
            eprintln!(
                "retiring captured SSH session {:?}: {}",
                self.captured,
                std::fs::read_to_string(format!("/proc/{}/stat", self.captured.pid))
                    .unwrap_or_default()
            );
            let result = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.descriptor,
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            };
            assert!(
                result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            );
        }
    }
    pub(super) fn reap_if_adopted(&self) {
        #[cfg(target_os = "linux")]
        if !retired(self.captured) {
            let result = unsafe {
                libc::waitpid(
                    self.captured.pid as i32,
                    std::ptr::null_mut(),
                    libc::WNOHANG,
                )
            };
            assert!(
                result >= 0
                    || matches!(
                        std::io::Error::last_os_error().raw_os_error(),
                        Some(libc::ECHILD | libc::EINTR)
                    )
            );
        }
    }
}
impl Drop for PinnedSession {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        unsafe {
            libc::close(self.descriptor);
        }
    }
}

pub(super) struct Daemon {
    pub(super) child: Child,
    pub(super) owner: ProcessOwner,
    pub(super) identity: Identity,
    stopped: bool,
}
impl Daemon {
    pub(super) fn start(sshd: &Path, config: &Path, log: &Path) -> Self {
        let mut command = Command::new(sshd);
        command
            .args(["-D", "-e", "-f"])
            .arg(config)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(log).unwrap());
        ProcessOwner::prepare(&mut command);
        let mut child = command.spawn().expect("explicit sshd must start");
        let owner = ProcessOwner::attach(&mut child).unwrap_or_else(|error| {
            let _ = child.kill();
            let _ = child.wait();
            panic!("cannot own sshd: {error}")
        });
        let captured = identity(child.id()).expect("Linux /proc sshd identity required");
        Self {
            child,
            owner,
            identity: captured,
            stopped: false,
        }
    }
    pub(super) fn stop(&mut self) {
        if !self.stopped {
            // sshd creates new session/PGIDs. The public listener owner cannot
            // own those escaped groups. This isolated fixture pins only its
            // verified descendants and adopts/reaps them via scoped subreaper.
            let sessions = PinnedSession::capture(self.identity);
            for session in &sessions {
                session.terminate();
            }
            self.owner.stop(&mut self.child);
            let deadline = Instant::now() + Duration::from_secs(5);
            while sessions.iter().any(|s| !retired(s.captured)) {
                for session in &sessions {
                    session.reap_if_adopted();
                }
                assert!(
                    Instant::now() < deadline,
                    "captured SSH sessions not physically retired: {:?}",
                    sessions
                        .iter()
                        .filter(|s| !retired(s.captured))
                        .map(|s| s.captured)
                        .collect::<Vec<_>>()
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            self.stopped = true;
        }
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Keys, trust files and daemon belong only to this nested private fixture.
/// Field order preserves daemon/session retirement before files/subreaper.
pub(super) struct Fixture {
    pub(super) daemon: Option<Daemon>,
    pub(super) directory: tempfile::TempDir,
    pub(super) port: u16,
    pub(super) known: PathBuf,
    pub(super) client_config: PathBuf,
    pub(super) log: PathBuf,
    _subreaper: Subreaper,
}
impl Fixture {
    pub(super) fn start(sshd: &Path, keygen: &Path, user: &str) -> Self {
        assert!(
            !user.is_empty()
                && user
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
        );
        let _subreaper = Subreaper::enter();
        let directory = tempfile::Builder::new()
            .prefix("configured-openssh-")
            .tempdir()
            .unwrap();
        let root = directory.path();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        for name in ["host", "user", "wrong-host"] {
            run(
                keygen,
                &[
                    "-q".into(),
                    "-t".into(),
                    "ed25519".into(),
                    "-N".into(),
                    "".into(),
                    "-f".into(),
                    root.join(name).to_string_lossy().into_owned(),
                ],
            );
        }
        let authorized = root.join("authorized_keys");
        std::fs::copy(root.join("user.pub"), &authorized).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&authorized, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener); // sshd must own the actual listener; a bind race fails readiness.
                        // StrictModes rejects world-writable /tmp ancestors even for sealed private
                        // fixtures. Use the daemon's explicit read-only key command, preserving
                        // strict server modes and actual key authentication without user SSH files.
        assert!(Path::new("/usr/bin/cat").is_file());
        let server_config = root.join("sshd_config");
        let path = |name: &str| root.join(name).to_string_lossy().into_owned();
        std::fs::write(&server_config, format!(
        "Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nPidFile {}\nAuthorizedKeysFile none\nAuthorizedKeysCommand /usr/bin/cat {}\nAuthorizedKeysCommandUser {user}\nAllowUsers {user}\nAuthenticationMethods publickey\nPubkeyAuthentication yes\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\nPermitRootLogin prohibit-password\nStrictModes yes\nUseDNS no\nSetEnv RAYON_NUM_THREADS=2 OMP_NUM_THREADS=2 OPENBLAS_NUM_THREADS=2 MKL_NUM_THREADS=2 NUMEXPR_NUM_THREADS=2\nAllowAgentForwarding no\nAllowTcpForwarding no\nX11Forwarding no\nPermitTunnel no\nPermitUserEnvironment no\nSubsystem sftp internal-sftp\nLogLevel VERBOSE\n",
        quote(&path("host")), quote(&path("sshd.pid")), quote(&authorized.to_string_lossy())
    )).unwrap();
        let known = root.join("known_hosts");
        let known_line = |key: &str| {
            format!(
                "[127.0.0.1]:{port} {}\n",
                std::fs::read_to_string(root.join(key)).unwrap().trim()
            )
        };
        std::fs::write(&known, known_line("host.pub")).unwrap();
        let client_config = root.join("ssh_config");
        std::fs::write(&client_config, format!(
        "Host *\n  HostName 127.0.0.1\n  Port {port}\n  User {user}\n  IdentityFile {}\n  IdentitiesOnly yes\n  IdentityAgent none\n  UserKnownHostsFile {}\n  GlobalKnownHostsFile /dev/null\n  StrictHostKeyChecking yes\n  BatchMode yes\n  PasswordAuthentication no\n  KbdInteractiveAuthentication no\n  NumberOfPasswordPrompts 0\n  ConnectTimeout 2\n  ConnectionAttempts 1\n  ControlMaster no\n  ControlPath none\n  RequestTTY no\n  ForwardAgent no\n  ClearAllForwardings yes\n",
        quote(&path("user")), quote(&known.to_string_lossy())
    )).unwrap();
        let log = root.join("sshd.log");
        let daemon = Daemon::start(sshd, &server_config, &log);
        Self {
            daemon: Some(daemon),
            directory,
            port,
            known,
            client_config,
            log,
            _subreaper,
        }
    }
    pub(super) fn known_line(&self, key: &str) -> String {
        format!(
            "[127.0.0.1]:{} {}\n",
            self.port,
            std::fs::read_to_string(self.directory.path().join(key))
                .unwrap()
                .trim()
        )
    }
}
