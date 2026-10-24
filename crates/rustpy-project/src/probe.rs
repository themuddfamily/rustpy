//! Bounded subprocess output for isolated interpreter metadata probes.
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeError {
    Spawn(String),
    Failed,
    TimedOut,
    OutputLimit,
    InvalidOutput,
}

/// Highest descriptor number the fallback descriptor sweep visits.
#[cfg(unix)]
fn inherited_fd_scan_limit() -> libc::c_int {
    let max = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
    if max <= 0 {
        4096
    } else {
        max.min(65_536) as libc::c_int
    }
}

/// Runs in the forked probe child before `exec`:
///
/// * Linux: the child is killed when the thread that spawned it dies, so a
///   probe can never outlive a caller whose process exits mid-probe (the child
///   is in its own process group, so terminal signals do not reach it).
/// * Every descriptor >= 3 is marked close-on-exec. Rust opens its own
///   descriptors with `CLOEXEC`, but descriptors inherited from whoever
///   started this process (for example a build lock held through `flock`)
///   are not, and a hung interpreter would otherwise keep them open. Marking
///   instead of closing keeps std's exec-error pipe working.
#[cfg(unix)]
fn harden_probe_child(parent: libc::pid_t, fd_limit: libc::c_int) {
    unsafe {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            libc::prctl(
                libc::PR_SET_PDEATHSIG,
                libc::SIGKILL as libc::c_ulong,
                0,
                0,
                0,
            );
            if libc::getppid() != parent {
                // The caller died between fork and prctl.
                libc::_exit(127);
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let _ = parent;
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            const CLOSE_RANGE_CLOEXEC: libc::c_uint = 1 << 2;
            if libc::syscall(
                libc::SYS_close_range,
                3 as libc::c_uint,
                libc::c_uint::MAX,
                CLOSE_RANGE_CLOEXEC,
            ) == 0
            {
                return;
            }
        }
        for fd in 3..fd_limit {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags >= 0 && flags & libc::FD_CLOEXEC == 0 {
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
            }
        }
    }
}

/// Drains stdout while the child runs, so a full pipe cannot deadlock a probe.
/// Both output size and runtime are bounded; every child is reaped.
pub fn command_output(mut command: Command, timeout: Duration) -> Result<Vec<u8>, ProbeError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        let parent = unsafe { libc::getpid() };
        let fd_limit = inherited_fd_scan_limit();
        // SAFETY: the hook runs between fork and exec and only makes
        // async-signal-safe calls (prctl, getppid, syscall, fcntl, _exit).
        unsafe {
            command.pre_exec(move || {
                harden_probe_child(parent, fd_limit);
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: captured metadata has no interactive console.
        command.creation_flags(0x0800_0000);
    }
    let mut child = command
        .spawn()
        .map_err(|e| ProbeError::Spawn(e.to_string()))?;
    let stdout = child.stdout.take().ok_or(ProbeError::InvalidOutput)?;
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ProbeError::InvalidOutput)
            .and({
                if bytes.len() > 1024 * 1024 {
                    Err(ProbeError::OutputLimit)
                } else {
                    Ok(bytes)
                }
            });
        let _ = tx.send(result);
    });
    let start = Instant::now();
    let mut output = None;
    let result = loop {
        if output.is_none() {
            match rx.try_recv() {
                Ok(Err(error)) => break Err(error),
                Ok(Ok(bytes)) => output = Some(bytes),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    break Err(ProbeError::InvalidOutput);
                }
                Err(_) => {}
            }
        }
        match child.try_wait() {
            Ok(Some(status)) if !status.success() => break Err(ProbeError::Failed),
            Ok(Some(_)) if output.is_some() => break Ok(output.take().unwrap()),
            Err(_) => break Err(ProbeError::Failed),
            _ => {}
        }
        if start.elapsed() >= timeout {
            break Err(ProbeError::TimedOut);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    #[cfg(windows)]
    if result.is_err() {
        // A batch adapter can own Python/SSH/WSL descendants. Killing only
        // cmd.exe would leave both the process and its stdout reader alive.
        use std::os::windows::process::CommandExt;
        if let Ok(mut killer) = Command::new("taskkill.exe")
            .creation_flags(0x0800_0000)
            .args(["/F", "/T", "/PID", &child.id().to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            let deadline = Instant::now() + Duration::from_secs(1);
            while matches!(killer.try_wait(), Ok(None)) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = killer.kill();
            let _ = killer.wait();
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    result
}

pub fn probe_output(path: &Path, args: &[&str], timeout: Duration) -> Option<Vec<u8>> {
    let mut command = crate::interpreter::interpreter_command(path);
    command.args(args);
    command_output(command, timeout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_child_fixture() {
        match std::env::var("RUSTPY_METADATA_PROBE_FIXTURE").as_deref() {
            Ok("large") => {
                use std::io::Write;
                std::io::stdout()
                    .write_all(&vec![b'x'; 1024 * 1024 + 4096])
                    .unwrap();
            }
            Ok("timeout") => std::thread::sleep(Duration::from_secs(20)),
            #[cfg(target_os = "linux")]
            Ok("abandon") => {
                // The caller process exits while its probe is still running.
                let pid_file = std::env::var("RUSTPY_METADATA_PROBE_PIDFILE").unwrap();
                let script = format!(
                    "echo $$ > '{pid_file}.tmp'; mv '{pid_file}.tmp' '{pid_file}'; exec sleep 600"
                );
                std::thread::spawn(move || {
                    let mut command = Command::new("/bin/sh");
                    command.args(["-c", &script]);
                    let _ = command_output(command, Duration::from_secs(120));
                });
                let deadline = Instant::now() + Duration::from_secs(10);
                while !Path::new(&std::env::var("RUSTPY_METADATA_PROBE_PIDFILE").unwrap()).exists()
                    && Instant::now() < deadline
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                std::process::exit(0);
            }
            #[cfg(windows)]
            Ok("no-window") => {
                #[link(name = "kernel32")]
                extern "system" {
                    fn GetConsoleWindow() -> *mut std::ffi::c_void;
                }
                assert!(
                    unsafe { GetConsoleWindow() }.is_null(),
                    "captured metadata child allocated a native console window"
                );
                println!("ACTUAL_METADATA_CHILD_NO_CONSOLE");
            }
            _ => {}
        }
    }
    fn child(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "probe::tests::output_child_fixture",
                "--nocapture",
            ])
            .env("RUSTPY_METADATA_PROBE_FIXTURE", mode);
        command
    }
    #[cfg(windows)]
    #[test]
    fn captured_metadata_child_has_no_console_window() {
        let output = command_output(child("no-window"), Duration::from_secs(5)).unwrap();
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("ACTUAL_METADATA_CHILD_NO_CONSOLE"));
    }
    /// Gone, or a zombie already orphaned to init (which reaps it).
    #[cfg(target_os = "linux")]
    fn process_gone(pid: i32) -> bool {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            Ok(stat) => stat
                .rsplit_once(')')
                .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn probe_child_does_not_inherit_non_cloexec_descriptors() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("cargo.lock");
        std::fs::write(&lock, b"").unwrap();
        let path = std::ffi::CString::new(lock.to_str().unwrap()).unwrap();
        // Like a descriptor inherited from `flock`: open without O_CLOEXEC.
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY) };
        assert!(fd >= 3);
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            &format!("if [ -e /proc/$$/fd/{fd} ]; then echo open; else echo closed; fi"),
        ]);
        let output = command_output(command, Duration::from_secs(10));
        unsafe { libc::close(fd) };
        assert_eq!(String::from_utf8(output.unwrap()).unwrap().trim(), "closed");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn probe_child_dies_with_an_exiting_caller() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("probe.pid");
        let mut caller = child("abandon");
        caller
            .env("RUSTPY_METADATA_PROBE_PIDFILE", &pid_file)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let status = caller.status().unwrap();
        assert!(status.success());
        let pid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !process_gone(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let gone = process_gone(pid);
        if !gone {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        assert!(gone, "probe child {pid} outlived its exited caller");
    }

    #[test]
    fn drains_large_output_without_pipe_deadlock_and_reaps_timeouts() {
        assert_eq!(
            command_output(child("large"), Duration::from_secs(3)),
            Err(ProbeError::OutputLimit)
        );
        let started = Instant::now();
        assert_eq!(
            command_output(child("timeout"), Duration::from_millis(100)),
            Err(ProbeError::TimedOut)
        );
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
