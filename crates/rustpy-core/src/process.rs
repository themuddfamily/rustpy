//! External commands under a cancellation token: what background tasks use
//! for `git push`, `git fetch` and other network-bound tools, so the cancel
//! button of the status bar stops the process instead of waiting for it.
//!
//! A cancelled command is asked to terminate first (SIGTERM on Unix, so
//! `git` removes its lock files) and killed after [`TERMINATE_GRACE`]. On
//! Linux the processes it started (an `ssh` transport, a credential helper)
//! are stopped with it; they stay in the IDE's process group, so an `ssh`
//! passphrase prompt in the terminal the IDE was started from still works.
//! They are found through `/proc` and recognised by their start time as
//! well as their pid, so a helper that has exited meanwhile is never
//! confused with an unrelated process that got its pid.

use std::io::Read;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

use crate::cancel::CancellationToken;
use crate::tasks::TaskError;

/// Why [`output_cancellable`] produced no output.
#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("cancelled")]
    Cancelled,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl From<ProcessError> for TaskError {
    fn from(e: ProcessError) -> Self {
        match e {
            ProcessError::Cancelled => TaskError::Cancelled,
            ProcessError::Io(e) => TaskError::Failed(e.to_string()),
        }
    }
}

/// How often the child is polled for exit and the token for cancellation.
const POLL: Duration = Duration::from_millis(15);

/// How long a cancelled command may take to exit before it is killed.
pub const TERMINATE_GRACE: Duration = Duration::from_secs(2);

/// Runs `cmd` to completion and collects its output like
/// [`Command::output`], but kills the process as soon as `token` is
/// cancelled. Stdout and stderr are captured; stdin is whatever `cmd` sets
/// Use piped stdin for commands with no input; its writer is closed after spawn.
pub fn output_cancellable(
    cmd: &mut Command,
    token: &CancellationToken,
) -> Result<Output, ProcessError> {
    if token.is_cancelled() {
        return Err(ProcessError::Cancelled);
    }
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    // There is no input argument: a requested stdin pipe must deliver EOF.
    drop(child.stdin.take());
    // Drain both pipes on their own threads so a chatty process never blocks on a full pipe.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        pipe.map(|mut p| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = p.read_to_end(&mut buf);
                buf
            })
        })
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    loop {
        if let Some(status) = child.try_wait()? {
            let collect = |h: Option<std::thread::JoinHandle<Vec<u8>>>| {
                h.map(|h| h.join().unwrap_or_default()).unwrap_or_default()
            };
            return Ok(Output {
                status,
                stdout: collect(stdout),
                stderr: collect(stderr),
            });
        }
        if token.is_cancelled() {
            terminate(&mut child, TERMINATE_GRACE);
            // The reader threads end once every process holding the pipes
            // exits (a grandchild may keep them open); nobody waits for them.
            return Err(ProcessError::Cancelled);
        }
        std::thread::sleep(POLL);
    }
}

/// Stops `child` and the processes it started: politely first, then after
/// `grace` by force. Returns once `child` has exited.
/// Terminates an owned child and its identified helpers, then reaps the child.
/// Callers owning a separate process group should terminate that group too.
pub fn terminate_owned(child: &mut Child, grace: Duration) {
    // A cached exited Child may already have been reaped. Its numeric PID
    // is then no longer ours; never pass it to the Unix signal helper.
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    terminate(child, grace);
}

fn terminate(child: &mut Child, grace: Duration) {
    #[cfg(unix)]
    {
        // `child` itself cannot be confused with another process until it is
        // reaped (`try_wait`); its helpers are checked before every signal.
        let (main, helpers) = (
            Proc {
                pid: child.id(),
                start: None,
            },
            helper_processes(child.id()),
        );
        signal(std::slice::from_ref(&main), sys::SIGTERM);
        signal(&helpers, sys::SIGTERM);
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                // Helpers that ignored the request (or were waiting on the
                // parent) must not outlive it.
                signal(&helpers, sys::SIGKILL);
                return;
            }
            std::thread::sleep(POLL);
        }
        signal(std::slice::from_ref(&main), sys::SIGKILL);
        signal(&helpers, sys::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = grace;
    let _ = child.kill();
    let _ = child.wait();
}

/// A process to signal. `start` identifies it beyond its pid (a pid is
/// reused once its process exits and is reaped): it is signalled only while
/// `/proc` still shows a process with that pid and start time.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Proc {
    pid: u32,
    /// Start time (clock ticks after boot, `/proc/<pid>/stat` field 22);
    /// `None` for our own child, which stays ours until reaped.
    start: Option<u64>,
}

/// `pid`'s descendants with their start times (Linux: from `/proc`; none elsewhere).
#[cfg(unix)]
fn helper_processes(pid: u32) -> Vec<Proc> {
    #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
    let mut out: Vec<Proc> = Vec::new();
    #[cfg(target_os = "linux")]
    {
        // Parent and start time of every process.
        let mut children: std::collections::HashMap<u32, Vec<Proc>> =
            std::collections::HashMap::new();
        for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
            let Some(p) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            if let Some((ppid, start)) = parse_stat(&stat) {
                children.entry(ppid).or_default().push(Proc {
                    pid: p,
                    start: Some(start),
                });
            }
        }
        let mut parents = vec![pid];
        let mut i = 0;
        while i < parents.len() {
            for kid in children.get(&parents[i]).into_iter().flatten() {
                if kid.pid != pid && !out.iter().any(|o| o.pid == kid.pid) {
                    out.push(*kid);
                    parents.push(kid.pid);
                }
            }
            i += 1;
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = pid;
    out
}

/// `(ppid, start time)` from the text of `/proc/<pid>/stat`:
/// `pid (comm) state ppid ... starttime ...` (the name may hold spaces and parentheses).
#[cfg(unix)]
fn parse_stat(stat: &str) -> Option<(u32, u64)> {
    let fields: Vec<&str> = stat[stat.rfind(')')? + 1..].split_whitespace().collect();
    // After the name: state (field 3), ppid (4), ..., starttime (22).
    Some((fields.get(1)?.parse().ok()?, fields.get(19)?.parse().ok()?))
}

/// Whether `p` still names the process it was found as: a helper that
/// exited may have had its pid reused by an unrelated process since.
#[cfg(unix)]
fn still_running(p: &Proc) -> bool {
    match p.start {
        None => true,
        Some(start) => std::fs::read_to_string(format!("/proc/{}/stat", p.pid))
            .ok()
            .and_then(|s| parse_stat(&s))
            .is_some_and(|(_, now)| now == start),
    }
}

#[cfg(unix)]
fn signal(procs: &[Proc], sig: i32) {
    for p in procs {
        let Ok(pid) = i32::try_from(p.pid) else {
            continue;
        };
        if pid <= 0 || !still_running(p) {
            continue;
        }
        // SAFETY: kill(2) takes plain integers and has no memory-safety preconditions.
        unsafe {
            sys::kill(pid, sig);
        }
    }
}

#[cfg(unix)]
mod sys {
    pub const SIGTERM: i32 = 15;
    pub const SIGKILL: i32 = 9;
    extern "C" {
        pub fn kill(pid: i32, sig: i32) -> i32;
    }
}

#[cfg(all(test, unix))]
mod tests {

    #[cfg(unix)]
    #[test]
    fn piped_stdin_reaches_eof_before_cancellation() {
        let token = CancellationToken::new();
        let deadline_token = token.clone();
        let (finished, completion) = std::sync::mpsc::channel();
        let deadline = std::thread::spawn(move || {
            if completion.recv_timeout(Duration::from_secs(3)).is_err() {
                deadline_token.cancel();
            }
        });
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                "if IFS= read -r value; then printf unexpected; exit 2; fi; printf eof",
            ])
            .stdin(Stdio::piped());
        let result = output_cancellable(&mut command, &token);
        let _ = finished.send(());
        deadline.join().unwrap();
        let output = result.expect("a command with no input must see EOF before cancellation");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"eof");
    }
    use super::*;
    use std::time::Instant;

    #[test]
    fn collects_output_and_status() {
        let out = output_cancellable(
            Command::new("sh").args(["-c", "echo out; echo err >&2; exit 3"]),
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "out\n");
        assert_eq!(String::from_utf8_lossy(&out.stderr), "err\n");
        assert_eq!(out.status.code(), Some(3));
    }

    #[test]
    fn large_output_does_not_deadlock() {
        let out = output_cancellable(
            Command::new("sh").args(["-c", "head -c 300000 /dev/zero"]),
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(out.stdout.len(), 300_000);
    }

    #[test]
    fn cancelling_kills_the_process() {
        let token = CancellationToken::new();
        let t2 = token.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            t2.cancel();
        });
        let start = Instant::now();
        let r = output_cancellable(Command::new("sleep").arg("30"), &token);
        canceller.join().unwrap();
        assert!(matches!(r, Err(ProcessError::Cancelled)), "{r:?}");
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "killed promptly: {:?}",
            start.elapsed()
        );
        // Already cancelled: nothing is started.
        assert!(matches!(
            output_cancellable(&mut Command::new("true"), &token),
            Err(ProcessError::Cancelled)
        ));
    }

    /// Whether `pid` is gone (or a zombie waiting for its new parent to reap it).
    #[cfg(target_os = "linux")]
    fn is_gone(pid: u32) -> bool {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            Ok(stat) => {
                stat.rfind(')')
                    .and_then(|i| stat[i + 1..].split_whitespace().next())
                    == Some("Z")
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cancelling_stops_the_processes_a_command_started() {
        let dir = std::env::temp_dir().join(format!("rustpy-cancel-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pidfile = dir.join("pid");
        // A shell with a helper that would outlive it (like an ssh transport).
        let script = format!("sleep 30 & echo $! > {}; wait", pidfile.display());
        let token = CancellationToken::new();
        let t2 = token.clone();
        let watched = pidfile.clone();
        let canceller = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while std::fs::read_to_string(&watched).map_or(true, |s| s.trim().is_empty())
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            t2.cancel();
        });
        let start = Instant::now();
        let r = output_cancellable(Command::new("sh").args(["-c", &script]), &token);
        canceller.join().unwrap();
        assert!(matches!(r, Err(ProcessError::Cancelled)), "{r:?}");
        assert!(
            start.elapsed() < TERMINATE_GRACE + Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
        let helper: u32 = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !is_gone(helper) {
            assert!(
                Instant::now() < deadline,
                "the helper process {helper} survived the cancel"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stat_lines_parse_with_odd_process_names() {
        let stat = "4242 (my (odd) name) S 17 4242 4242 0 -1 4194560 120 0 0 0 1 2 0 0 20 0 1 0 987654 1000 200";
        assert_eq!(parse_stat(stat), Some((17, 987654)));
        assert_eq!(parse_stat("1 (init) S"), None);
        assert_eq!(parse_stat("no parenthesis"), None);
    }

    /// A helper's pid may belong to another process by the time it is
    /// signalled (it exited and was reaped, and the pid was reused): the
    /// start time recorded when it was found no longer matches, so it is
    /// left alone.
    #[cfg(target_os = "linux")]
    #[test]
    fn reused_pids_are_not_signalled() {
        let mut bystander = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = bystander.id();
        let start = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|s| parse_stat(&s))
            .map(|(_, start)| start)
            .unwrap();
        // Recorded with another start time: a different process that had this pid.
        let stale = Proc {
            pid,
            start: Some(start + 1),
        };
        assert!(!still_running(&stale));
        signal(&[stale], sys::SIGKILL);
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            matches!(bystander.try_wait(), Ok(None)),
            "an unrelated process was killed"
        );
        // The process it was found as is signalled.
        let same = Proc {
            pid,
            start: Some(start),
        };
        assert!(still_running(&same));
        signal(&[same], sys::SIGKILL);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !matches!(bystander.try_wait(), Ok(Some(_))) {
            assert!(Instant::now() < deadline, "the process survived SIGKILL");
            std::thread::sleep(Duration::from_millis(10));
        }
        // A pid nobody has any more is skipped too.
        assert!(!still_running(&Proc {
            pid,
            start: Some(start)
        }));
    }

    #[test]
    fn a_command_ignoring_sigterm_is_killed_after_the_grace_period() {
        let mut child = Command::new("sh")
            .args(["-c", "trap '' TERM; sleep 30"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let start = Instant::now();
        terminate(&mut child, Duration::from_millis(300));
        assert!(
            start.elapsed() >= Duration::from_millis(250),
            "it waited for the grace period: {:?}",
            start.elapsed()
        );
        assert!(start.elapsed() < Duration::from_secs(5));
        assert!(matches!(child.try_wait(), Ok(Some(_))));
    }

    #[test]
    fn missing_programs_are_io_errors() {
        let r = output_cancellable(
            &mut Command::new("/nonexistent/program-xyz"),
            &CancellationToken::new(),
        );
        assert!(matches!(r, Err(ProcessError::Io(_))));
        assert!(matches!(
            TaskError::from(r.unwrap_err()),
            TaskError::Failed(_)
        ));
    }
}
