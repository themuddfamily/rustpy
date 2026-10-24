//! Portable bounded pipe drains; the shared process tree retires before leader reap.
use anyhow::{ensure, Result};
use rustpy_core::{process_tree::ProcessTree, CancellationToken};
use std::{
    io::{self, Read},
    process::{Child, ChildStderr, ChildStdout, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
enum Pipe {
    Out(ChildStdout),
    Err(ChildStderr),
}
impl Read for Pipe {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Out(p) => p.read(bytes),
            Self::Err(p) => p.read(bytes),
        }
    }
}
enum Ready {
    Data(usize),
    Empty,
    #[cfg(windows)]
    End,
}
#[cfg(unix)]
fn ready(pipe: &Pipe) -> io::Result<Ready> {
    use std::os::fd::AsRawFd;
    let fd = match pipe {
        Pipe::Out(p) => p.as_raw_fd(),
        Pipe::Err(p) => p.as_raw_fd(),
    };
    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let result = unsafe { libc::poll(&mut descriptor, 1, 10) };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(Ready::Empty);
        }
        return Err(error);
    }
    Ok(if result == 0 {
        Ready::Empty
    } else {
        Ready::Data(4096)
    })
}
#[cfg(windows)]
fn ready(pipe: &Pipe) -> io::Result<Ready> {
    use std::os::windows::io::AsRawHandle;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn PeekNamedPipe(
            handle: *mut std::ffi::c_void,
            buffer: *mut std::ffi::c_void,
            size: u32,
            read: *mut u32,
            available: *mut u32,
            left: *mut u32,
        ) -> i32;
    }
    // Each read handle belongs exclusively to this thread: no concurrent I/O
    // on that synchronous handle can block PeekNamedPipe behind another read.
    // Read only the reported available byte count, never wait for future bytes.
    let handle = match pipe {
        Pipe::Out(p) => p.as_raw_handle(),
        Pipe::Err(p) => p.as_raw_handle(),
    };
    let mut available = 0;
    let success = unsafe {
        PeekNamedPipe(
            handle,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut available,
            std::ptr::null_mut(),
        )
    };
    if success == 0 {
        let error = io::Error::last_os_error();
        return if matches!(error.raw_os_error(), Some(109 | 232)) {
            Ok(Ready::End)
        } else {
            Err(error)
        };
    }
    if available == 0 {
        thread::sleep(Duration::from_millis(10));
        Ok(Ready::Empty)
    } else {
        Ok(Ready::Data(available.min(4096) as usize))
    }
}
struct Owner {
    child: Option<Child>,
    tree: Option<ProcessTree>,
}
impl Owner {
    fn spawn(command: &mut Command) -> io::Result<Self> {
        ProcessTree::prepare(command);
        // Establish an unwind/attach-failure guard before attaching/resuming.
        // A failed Windows attach leaves the child suspended; it cannot launch
        // descendants before this exact child is killed and reaped.
        let mut owner = Self {
            child: Some(command.spawn()?),
            tree: None,
        };
        owner.tree = Some(ProcessTree::new(owner.child.as_mut().unwrap())?);
        Ok(owner)
    }
    fn stop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            if let Some(tree) = &self.tree {
                tree.force(child);
            } else {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    fn poll(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        self.tree
            .as_ref()
            .unwrap()
            .poll(self.child.as_mut().unwrap())
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.stop();
    }
}
struct Drains {
    stop: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    handles: Vec<JoinHandle<Result<Vec<u8>>>>,
}
impl Drop for Drains {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}
impl Drains {
    fn new(pipes: [Pipe; 2], limit: u64, expires: Instant, cancel: &CancellationToken) -> Self {
        let mut this = Self {
            stop: Arc::new(AtomicBool::new(false)),
            finished: Arc::new(AtomicBool::new(false)),
            failed: Arc::new(AtomicBool::new(false)),
            handles: Vec::new(),
        };
        for mut pipe in pipes {
            let stop = this.stop.clone();
            let finished = this.finished.clone();
            let failed = this.failed.clone();
            let cancel = cancel.clone();
            this.handles.push(thread::spawn(move || {
                let result = (|| {
                    let mut output = Vec::new();
                    let mut chunk = [0; 4096];
                    loop {
                        cancel.check()?;
                        ensure!(Instant::now() < expires, "native skeleton drain timeout");
                        if stop.load(Ordering::Acquire) {
                            break;
                        }
                        match ready(&pipe)? {
                            #[cfg(windows)]
                            Ready::End => break,
                            Ready::Empty => {
                                if finished.load(Ordering::Acquire) {
                                    break;
                                }
                                continue;
                            }
                            Ready::Data(count) => {
                                let count = pipe.read(&mut chunk[..count])?;
                                if count == 0 {
                                    break;
                                }
                                ensure!(
                                    (output.len() as u64) + count as u64 <= limit,
                                    "native skeleton output limit"
                                );
                                output.extend_from_slice(&chunk[..count]);
                                continue;
                            }
                        }
                    }
                    Ok(output)
                })();
                if result.is_err() {
                    failed.store(true, Ordering::Release);
                }
                result
            }));
        }
        this
    }
}
pub(super) fn supervised(
    command: &mut Command,
    cancel: &CancellationToken,
    deadline: Duration,
    output_limit: u64,
) -> Result<(Vec<u8>, Vec<u8>)> {
    let (status, out, err) = supervised_observed(command, cancel, deadline, output_limit, |_| {})?;
    ensure!(status.success(), "native introspection failed: {status}");
    Ok((out, err))
}
pub(super) fn supervised_status(
    command: &mut Command,
    cancel: &CancellationToken,
    deadline: Duration,
    output_limit: u64,
) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>)> {
    supervised_observed(command, cancel, deadline, output_limit, |_| {})
}

#[cfg(all(test, target_os = "linux"))]
pub(super) fn supervised_with_owned_pid(
    command: &mut Command,
    cancel: &CancellationToken,
    deadline: Duration,
    output_limit: u64,
    pid: &mut Option<u32>,
) -> Result<(Vec<u8>, Vec<u8>)> {
    let (status, out, err) =
        supervised_observed(command, cancel, deadline, output_limit, |owned| {
            *pid = Some(owned)
        })?;
    ensure!(status.success(), "native introspection failed: {status}");
    Ok((out, err))
}

fn supervised_observed(
    command: &mut Command,
    cancel: &CancellationToken,
    deadline: Duration,
    output_limit: u64,
    on_spawn: impl FnOnce(u32),
) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>)> {
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = Owner::spawn(command)?;
    drop(child.child.as_mut().unwrap().stdin.take());
    let stdout = child.child.as_mut().unwrap().stdout.take().unwrap();
    let stderr = child.child.as_mut().unwrap().stderr.take().unwrap();
    let start = Instant::now();
    // Observation does not wait for child user code or postpone the deadline.
    on_spawn(child.child.as_ref().unwrap().id());
    let mut drains = Drains::new(
        [Pipe::Out(stdout), Pipe::Err(stderr)],
        output_limit,
        start + deadline,
        cancel,
    );
    let status = (|| -> Result<_> {
        loop {
            cancel.check()?;
            ensure!(start.elapsed() < deadline, "native skeleton timeout");
            ensure!(
                !drains.failed.load(Ordering::Acquire),
                "native skeleton output limit or pipe failure"
            );
            if let Some(status) = child.poll()? {
                break Ok(status);
            }
            thread::sleep(Duration::from_millis(10));
        }
    })();
    let status = match status {
        Ok(status) => status,
        Err(error) => {
            // Kill the whole owned tree and reap its leader before joining
            // exclusively owned readers. Drop repeats this safely on unwind.
            child.stop();
            return Err(error);
        }
    };
    // poll() already retired descendants before reaping the leader. Never
    // release a Unix PID/PGID identity first and try to signal it afterward.
    child.child.take();
    drains.finished.store(true, Ordering::Release);
    // A descendant retaining a pipe cannot hold readers after the owned child retires:
    // empty poll/peek + finished retires, while already-buffered bytes still drain.
    let out = drains
        .handles
        .remove(0)
        .join()
        .map_err(|_| anyhow::anyhow!("stdout reader panicked"))??;
    let err = drains
        .handles
        .remove(0)
        .join()
        .map_err(|_| anyhow::anyhow!("stderr reader panicked"))??;
    cancel.check()?;
    Ok((status, out, err))
}

#[cfg(test)]
mod portable_tests {
    use super::*;
    use std::{fs, path::Path};

    #[test]
    fn fake_child_entry() {
        let Ok(mode) = std::env::var("RUSTPY_SKELETON_FAKE_MODE") else {
            return;
        };
        if !matches!(mode.as_str(), "slow-writer" | "tree-leaf") {
            fs::write(
                std::env::var_os("RUSTPY_SKELETON_FAKE_PID").unwrap(),
                std::process::id().to_string(),
            )
            .unwrap();
        }
        match mode.as_str() {
            "closed" => {
                let mut input = String::new();
                std::io::stdin().read_to_string(&mut input).unwrap();
                assert!(input.is_empty());
                println!("closed-input");
            }
            "descendant" => {
                let pid = std::env::var_os("RUSTPY_SKELETON_FAKE_PID").unwrap();
                let mut command = fake("slow-writer", Path::new(&pid));
                command
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .stdin(Stdio::null());
                #[expect(
                    clippy::zombie_processes,
                    reason = "This helper must exit before its descendant to test inherited pipe drainage"
                )]
                let _descendant = command.spawn().unwrap();
                let ready = Path::new(&pid).with_extension("ready");
                let start = Instant::now();
                while !ready.exists() && start.elapsed() < Duration::from_secs(1) {
                    thread::sleep(Duration::from_millis(1));
                }
                assert!(ready.exists());
            }
            "tree-return" | "tree-wait" | "tree-flood" => {
                let pid = std::env::var_os("RUSTPY_SKELETON_FAKE_PID").unwrap();
                let pid = Path::new(&pid);
                let mut command = fake("tree-leaf", pid);
                command
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .stdin(Stdio::null());
                #[expect(
                    clippy::zombie_processes,
                    reason = "The fixture parent deliberately outlives/exits before its descendant; the supervisor must own both"
                )]
                let _descendant = command.spawn().unwrap();
                let started = Instant::now();
                while !pid.with_extension("heartbeat").exists()
                    && started.elapsed() < Duration::from_secs(5)
                {
                    thread::sleep(Duration::from_millis(5));
                }
                assert!(pid.with_extension("heartbeat").exists());
                if mode == "tree-flood" {
                    loop {
                        println!("{}", "x".repeat(4096));
                    }
                }
                if mode == "tree-wait" {
                    thread::sleep(Duration::from_secs(10));
                }
            }
            "tree-leaf" => {
                use std::io::Write;
                let pid = std::env::var_os("RUSTPY_SKELETON_FAKE_PID").unwrap();
                fs::write(
                    Path::new(&pid).with_extension("descendant"),
                    std::process::id().to_string(),
                )
                .unwrap();
                let mut heartbeat = fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(Path::new(&pid).with_extension("heartbeat"))
                    .unwrap();
                // Retain inherited stdout/stderr without relying on writes to
                // those pipes. Closing a drain alone must not stop this child.
                let started = Instant::now();
                while started.elapsed() < Duration::from_secs(8) {
                    heartbeat.write_all(b"x").unwrap();
                    heartbeat.flush().unwrap();
                    thread::sleep(Duration::from_millis(5));
                }
            }
            "slow-writer" => {
                use std::io::Write;
                std::io::stdout().write_all(b"ready").unwrap();
                std::io::stdout().flush().unwrap();
                fs::write(
                    Path::new(&std::env::var_os("RUSTPY_SKELETON_FAKE_PID").unwrap())
                        .with_extension("ready"),
                    b"ready",
                )
                .unwrap();
                let start = Instant::now();
                while start.elapsed() < Duration::from_secs(3) {
                    if std::io::stdout().write_all(b"x").is_err() {
                        break;
                    }
                    let _ = std::io::stdout().flush();
                    thread::sleep(Duration::from_millis(1));
                }
            }
            "flood" => loop {
                println!("{}", "x".repeat(4096));
            },
            "wait" => loop {
                thread::sleep(Duration::from_millis(10));
            },
            _ => panic!("unknown owned fixture mode"),
        }
    }
    fn fake(mode: &str, pid: &Path) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        // libtest names omit the crate component included by module_path!().
        let entry = concat!(module_path!(), "::fake_child_entry");
        let entry = entry.split_once("::").map_or(entry, |(_, suffix)| suffix);
        command
            .args(["--exact", entry, "--nocapture"])
            .env("RUSTPY_SKELETON_FAKE_MODE", mode)
            .env("RUSTPY_SKELETON_FAKE_PID", pid);
        command
    }
    fn retired(path: &Path) {
        let pid: u32 = fs::read_to_string(path).unwrap().parse().unwrap();
        #[cfg(unix)]
        {
            assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
            assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
        }
        #[cfg(windows)]
        {
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
                fn WaitForSingleObject(handle: *mut std::ffi::c_void, millis: u32) -> u32;
                fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
            }
            let handle = unsafe { OpenProcess(0x0010_0000, 0, pid) };
            if !handle.is_null() {
                assert_eq!(unsafe { WaitForSingleObject(handle, 0) }, 0);
                unsafe {
                    CloseHandle(handle);
                }
            }
        }
    }
    #[test]
    fn descendant_output_after_parent_exit_is_retired_before_pipe_drain() {
        let dir = tempfile::tempdir().unwrap();
        let pid = dir.path().join("pid");
        let start = Instant::now();
        let result = supervised(
            &mut fake("descendant", &pid),
            &CancellationToken::new(),
            Duration::from_millis(500),
            16 * 1024 * 1024,
        );
        let (stdout, stderr) =
            result.expect("normal leader exit retires inherited-pipe descendants");
        assert!(stdout.len() <= 16 * 1024 * 1024);
        assert!(stderr.len() <= 16 * 1024 * 1024);
        assert!(start.elapsed() < Duration::from_secs(2));
        retired(&pid);
    }
    fn heartbeat_stopped(pid: &Path) {
        let path = pid.with_extension("heartbeat");
        assert!(path.exists(), "descendant must actually have started");
        let before = fs::metadata(&path).unwrap().len();
        thread::sleep(Duration::from_millis(100));
        assert_eq!(
            before,
            fs::metadata(&path).unwrap().len(),
            "owned descendant kept running after supervisor returned"
        );
    }
    #[test]
    fn descendants_retire_on_success_timeout_overflow_and_cancellation() {
        for mode in ["tree-return", "timeout", "tree-flood", "cancel"] {
            let dir = tempfile::tempdir().unwrap();
            let pid = dir.path().join("pid");
            let token = CancellationToken::new();
            let cancelled = token.clone();
            let observed = pid.with_extension("heartbeat");
            let watcher = (mode == "cancel").then(|| {
                thread::spawn(move || {
                    let started = Instant::now();
                    while !observed.exists() && started.elapsed() < Duration::from_secs(5) {
                        thread::sleep(Duration::from_millis(5));
                    }
                    cancelled.cancel();
                })
            });
            let actual = if matches!(mode, "timeout" | "cancel") {
                "tree-wait"
            } else {
                mode
            };
            let result = supervised(
                &mut fake(actual, &pid),
                &token,
                Duration::from_secs(2),
                8192,
            );
            if let Some(watcher) = watcher {
                watcher.join().unwrap();
            }
            if mode == "tree-return" {
                assert!(result.is_ok(), "{result:?}");
            } else {
                assert!(result.is_err(), "{mode} must fail");
            }
            retired(&pid);
            heartbeat_stopped(&pid);
        }
    }
    #[test]
    fn unwind_after_descendant_start_retires_tree_and_reaps_leader() {
        let dir = tempfile::tempdir().unwrap();
        let pid = dir.path().join("pid");
        let ready = pid.with_extension("heartbeat");
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = supervised_observed(
                &mut fake("tree-wait", &pid),
                &CancellationToken::new(),
                Duration::from_secs(5),
                8192,
                |_| {
                    let started = Instant::now();
                    while !ready.exists() && started.elapsed() < Duration::from_secs(5) {
                        thread::sleep(Duration::from_millis(5));
                    }
                    assert!(ready.exists());
                    panic!("observed callback unwind");
                },
            );
        }));
        assert!(result.is_err());
        retired(&pid);
        heartbeat_stopped(&pid);
    }
    #[test]
    fn portable_closed_input_and_exact_child_retirement() {
        let dir = tempfile::tempdir().unwrap();
        let pid = dir.path().join("pid");
        let (out, _) = supervised(
            &mut fake("closed", &pid),
            &CancellationToken::new(),
            Duration::from_secs(5),
            16384,
        )
        .unwrap();
        assert!(String::from_utf8(out).unwrap().contains("closed-input"));
        retired(&pid);
    }
    #[test]
    fn portable_timeout_output_limit_and_cancellation_retire_owned_child() {
        for mode in ["wait", "flood", "cancel"] {
            let dir = tempfile::tempdir().unwrap();
            let pid = dir.path().join("pid");
            let token = CancellationToken::new();
            let cancel = token.clone();
            let observed = pid.clone();
            let watcher = (mode == "cancel").then(|| {
                thread::spawn(move || {
                    let started = Instant::now();
                    while !observed.exists() && started.elapsed() < Duration::from_secs(5) {
                        thread::sleep(Duration::from_millis(5));
                    }
                    cancel.cancel();
                })
            });
            let error = supervised(
                &mut fake(if mode == "flood" { "flood" } else { "wait" }, &pid),
                &token,
                Duration::from_secs(2),
                8192,
            )
            .unwrap_err();
            if let Some(watcher) = watcher {
                watcher.join().unwrap();
            }
            if mode == "wait" {
                assert!(error.to_string().contains("timeout"));
            }
            if mode == "flood" {
                assert!(error.to_string().contains("output limit"));
            }
            if mode == "cancel" {
                assert!(token.is_cancelled());
            }
            retired(&pid);
        }
    }
}
