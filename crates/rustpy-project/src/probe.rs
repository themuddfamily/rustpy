//! Bounded subprocess output for isolated interpreter metadata probes.
#[cfg(not(windows))]
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
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

/// Largest probe output accepted.
const OUTPUT_LIMIT: u64 = 1024 * 1024;

/// Late children disposed of by [`bounded_spawn`] (tests).
#[cfg(test)]
static ABANDONED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Runs `spawn` on a disposable thread and waits for it until `deadline`.
///
/// Starting a process is not bounded by the child's own timeout: on Windows
/// `CreateProcessW` of a Store app-execution alias can wait indefinitely for
/// the package activation before the child exists. Past the deadline the
/// caller gets [`ProbeError::TimedOut`] and the thread is left behind; if the
/// spawn ever completes, `abandon` disposes of the late child there.
///
/// On success the spawning thread stays alive until the returned guard is
/// dropped: on Linux the child's parent-death signal fires when the thread
/// that started it exits, so callers keep the guard until they reap the child.
fn bounded_spawn<T: Send + 'static>(
    deadline: Instant,
    spawn: impl FnOnce() -> std::io::Result<T> + Send + 'static,
    abandon: impl FnOnce(T) + Send + 'static,
) -> Result<(T, SpawnGuard), ProbeError> {
    enum Slot<T> {
        Pending,
        Done(std::io::Result<T>),
        Abandoned,
    }
    let shared = Arc::new((Mutex::new(Slot::Pending), Condvar::new()));
    let theirs = Arc::clone(&shared);
    let (guard, finished) = std::sync::mpsc::channel::<()>();
    std::thread::Builder::new()
        .name("probe-spawn".into())
        .spawn(move || {
            let result = spawn();
            let (slot, ready) = &*theirs;
            let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
            if matches!(*slot, Slot::Abandoned) {
                drop(slot);
                if let Ok(child) = result {
                    abandon(child);
                    #[cfg(test)]
                    ABANDONED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            } else {
                *slot = Slot::Done(result);
                ready.notify_all();
                drop(slot);
                // Returns once the caller drops its guard.
                let _ = finished.recv();
            }
        })
        .map_err(|e| ProbeError::Spawn(e.to_string()))?;
    let (slot, ready) = &*shared;
    let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if let Slot::Done(result) = std::mem::replace(&mut *slot, Slot::Pending) {
            return result
                .map(|child| (child, SpawnGuard { _keepalive: guard }))
                .map_err(|e| ProbeError::Spawn(e.to_string()));
        }
        let now = Instant::now();
        if now >= deadline {
            *slot = Slot::Abandoned;
            return Err(ProbeError::TimedOut);
        }
        slot = ready
            .wait_timeout(slot, deadline - now)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
}

/// Keeps a [`bounded_spawn`] thread alive while its child is in use.
struct SpawnGuard {
    _keepalive: std::sync::mpsc::Sender<()>,
}

/// Runs before the real spawn on the spawning thread (tests stall it there).
type SpawnHook = Box<dyn FnOnce() + Send>;

/// Captures stdout of a bounded child: output size and total runtime,
/// including the time to start the process, are limited; every child is
/// reaped. The process start runs on its own thread (see [`bounded_spawn`]);
/// on Windows it also avoids the standard library's process-creation lock, so
/// a start that never returns cannot hold up other process starts.
pub fn command_output(command: Command, timeout: Duration) -> Result<Vec<u8>, ProbeError> {
    command_output_hooked(command, timeout, None)
}

fn command_output_hooked(
    mut command: Command,
    timeout: Duration,
    hook: Option<SpawnHook>,
) -> Result<Vec<u8>, ProbeError> {
    let deadline = Instant::now() + timeout;
    command.stdin(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        windows_output(command, deadline, hook)
    }
    #[cfg(not(windows))]
    {
        unix_output(command, deadline, hook)
    }
}

/// Drains stdout while the child runs, so a full pipe cannot deadlock a probe.
#[cfg(not(windows))]
fn unix_output(
    mut command: Command,
    deadline: Instant,
    hook: Option<SpawnHook>,
) -> Result<Vec<u8>, ProbeError> {
    command.stdout(Stdio::piped());
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
    let (mut child, _guard) = bounded_spawn(
        deadline,
        move || {
            if let Some(hook) = hook {
                hook();
            }
            command.spawn()
        },
        |mut child| kill_unix_child(&mut child),
    )?;
    let stdout = child.stdout.take().ok_or(ProbeError::InvalidOutput);
    let result = stdout.and_then(|stdout| {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout
                .take(OUTPUT_LIMIT + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| ProbeError::InvalidOutput)
                .and({
                    if bytes.len() as u64 > OUTPUT_LIMIT {
                        Err(ProbeError::OutputLimit)
                    } else {
                        Ok(bytes)
                    }
                });
            let _ = tx.send(result);
        });
        let mut output = None;
        loop {
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
            if Instant::now() >= deadline {
                break Err(ProbeError::TimedOut);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    kill_unix_child(&mut child);
    result
}

/// Kills the probe's process group and reaps the child.
#[cfg(not(windows))]
fn kill_unix_child(child: &mut std::process::Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Windows: stdout goes to a delete-on-close temporary file rather than a
/// pipe. The child is started with `CreateProcessW` directly (see
/// [`crate::probe_windows`]), so its inheritable handle can briefly be
/// inherited by an unrelated child started concurrently through the standard
/// library; with a file that only delays the file's deletion, whereas a pipe
/// would never report end of output.
#[cfg(windows)]
fn windows_output(
    command: Command,
    deadline: Instant,
    hook: Option<SpawnHook>,
) -> Result<Vec<u8>, ProbeError> {
    use crate::probe_windows::{kill_tree, OutputFile, ProbeChild};
    let output = OutputFile::create().map_err(|e| ProbeError::Spawn(e.to_string()))?;
    let child_output = output
        .try_clone()
        .map_err(|e| ProbeError::Spawn(e.to_string()))?;
    let (mut child, _guard) = bounded_spawn(
        deadline,
        move || {
            if let Some(hook) = hook {
                hook();
            }
            ProbeChild::spawn(command, child_output)
        },
        |mut child| {
            kill_tree(&mut child);
        },
    )?;
    let result = loop {
        match child.try_wait() {
            Ok(Some(false)) | Err(_) => break Err(ProbeError::Failed),
            Ok(Some(true)) => break output.read(OUTPUT_LIMIT),
            Ok(None) => {}
        }
        if output.len() > OUTPUT_LIMIT {
            break Err(ProbeError::OutputLimit);
        }
        if Instant::now() >= deadline {
            break Err(ProbeError::TimedOut);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if result.is_err() {
        // A batch adapter can own Python/SSH/WSL descendants; kill the tree.
        kill_tree(&mut child);
    } else {
        child.kill_and_reap();
    }
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
            Ok("pid-then-sleep") => {
                let pid_file = std::env::var("RUSTPY_METADATA_PROBE_PIDFILE").unwrap();
                std::fs::write(format!("{pid_file}.tmp"), std::process::id().to_string()).unwrap();
                std::fs::rename(format!("{pid_file}.tmp"), &pid_file).unwrap();
                std::thread::sleep(Duration::from_secs(60));
            }
            Ok("hello") => println!("PROBE_FIXTURE_HELLO"),
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

    /// True once no process with `pid` is running.
    fn pid_gone(pid: u32) -> bool {
        #[cfg(target_os = "linux")]
        {
            process_gone(pid as i32)
        }
        #[cfg(windows)]
        {
            let output = Command::new("tasklist.exe")
                .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
                .output()
                .unwrap();
            !String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        {
            unsafe { libc::kill(pid as i32, 0) != 0 }
        }
    }

    /// Regression: on Windows a `CreateProcessW` of a Store app-execution alias
    /// never returned. The process start must be bounded by the probe timeout,
    /// must not hold up other process starts, and a child that appears after
    /// the caller gave up must be killed.
    #[test]
    fn stalled_process_start_times_out_without_blocking_other_starts() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("late.pid");
        let (release, released) = std::sync::mpsc::channel::<()>();
        let mut late = child("pid-then-sleep");
        late.env("RUSTPY_METADATA_PROBE_PIDFILE", &pid_file);
        let started = Instant::now();
        let stalled = std::thread::spawn(move || {
            command_output_hooked(
                late,
                Duration::from_millis(500),
                Some(Box::new(move || {
                    let _ = released.recv();
                })),
            )
        });
        // While that start is stuck, other probes start and finish normally.
        let output = command_output(child("hello"), Duration::from_secs(30)).unwrap();
        assert!(String::from_utf8_lossy(&output).contains("PROBE_FIXTURE_HELLO"));
        assert_eq!(stalled.join().unwrap(), Err(ProbeError::TimedOut));
        assert!(started.elapsed() < Duration::from_secs(30));
        // The start completes late; the abandoned child is killed (possibly
        // before it got to write its pid).
        let abandoned = ABANDONED.load(std::sync::atomic::Ordering::SeqCst);
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while ABANDONED.load(std::sync::atomic::Ordering::SeqCst) == abandoned
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(ABANDONED.load(std::sync::atomic::Ordering::SeqCst) > abandoned);
        std::thread::sleep(Duration::from_millis(500));
        if let Ok(pid) = std::fs::read_to_string(&pid_file) {
            let pid: u32 = pid.trim().parse().unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !pid_gone(pid) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            assert!(pid_gone(pid), "late probe child {pid} was not killed");
        }
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
