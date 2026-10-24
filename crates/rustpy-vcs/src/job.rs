//! Git processes run off the UI thread (roadmap §4.1 "Fetch / Pull / Push",
//! "Clone … progress"): output is streamed line by line, progress updates
//! (carriage-return segments) are kept apart, the process never prompts on a
//! terminal, and credentials go through the ask-pass bridge instead.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::RwLock;
use std::time::{Duration, Instant};

use anyhow::Context;

static GIT_PROGRAM: RwLock<String> = RwLock::new(String::new());

/// Sets the git executable for CLI operations (Settings › Version Control ›
/// Git › "Path to Git executable"); empty means `git` on the PATH.
pub fn set_git_program(path: &str) {
    let path = path.trim();
    if GIT_PROGRAM
        .read()
        .map(|p| p.as_str() == path)
        .unwrap_or(true)
    {
        return;
    }
    if let Ok(mut p) = GIT_PROGRAM.write() {
        *p = path.to_string();
    }
}

/// The git executable to run.
pub fn git_program() -> String {
    GIT_PROGRAM
        .read()
        .ok()
        .map(|p| p.clone())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "git".to_string())
}

/// Extra environment for a git process (the ask-pass variables).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitEnv {
    pub vars: Vec<(String, String)>,
}

/// A `git` command that cannot prompt on a terminal: stdin is closed,
/// `GIT_TERMINAL_PROMPT=0`, and on Unix the process starts a new session, so
/// ssh has no controlling terminal and falls back to `SSH_ASKPASS`.
pub fn git_command(cwd: &Path, args: &[String], env: &GitEnv) -> Command {
    let mut cmd = Command::new(git_program());
    crate::policy::apply(&mut cmd);
    cmd.args(args).current_dir(cwd).stdin(Stdio::null());
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    for (k, v) in &env.vars {
        cmd.env(k, v);
    }
    detach(&mut cmd);
    cmd
}

#[cfg(unix)]
pub(crate) fn detach(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: setsid() is async-signal-safe and touches no Rust state.
    unsafe {
        cmd.pre_exec(|| {
            setsid();
            Ok(())
        });
    }
}

#[cfg(windows)]
pub(crate) fn detach(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn detach(_cmd: &mut Command) {}

#[cfg(unix)]
extern "C" {
    fn setsid() -> i32;
    fn kill(pid: i32, sig: i32) -> i32;
}

#[cfg(unix)]
const SIGKILL: i32 = 9;
#[cfg(unix)]
const SIGTERM: i32 = 15;

/// How long a stopped git gets to clean up before it is killed: on SIGTERM
/// it removes a partial clone and its `.lock` files, which SIGKILL leaves.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// Asks the process and its helpers to end (`stop`): SIGTERM to its process
/// group on Unix. Windows has no such request for console programs, so the
/// whole process tree is killed at once. Returns whether that was a kill.
fn terminate(child: &mut Child, _owner: &rustpy_core::process_tree::ProcessTree) -> bool {
    #[cfg(unix)]
    {
        signal_group(child, SIGTERM);
        false
    }
    #[cfg(windows)]
    {
        let _ = &child;
        _owner.request_force();
        true
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = child.kill();
        true
    }
}

/// Kills the process and its helpers.
pub(crate) fn kill_hard(child: &mut Child) {
    #[cfg(unix)]
    signal_group(child, SIGKILL);
    #[cfg(windows)]
    kill_tree(child);
    let _ = child.kill();
}

/// Sends `sig` to the process group the child leads (see `detach`; only
/// while the child has not been waited for, so the group id is still its
/// own), or to the child alone if it has no group of its own.
#[cfg(unix)]
fn signal_group(child: &Child, sig: i32) {
    let Ok(pid) = i32::try_from(child.id()) else {
        return;
    };
    // SAFETY: plain libc calls with a valid pid and signal number.
    unsafe {
        if kill(-pid, sig) != 0 {
            kill(pid, sig);
        }
    }
}

/// `taskkill /T` ends git with the ssh and ask-pass processes it started,
/// which would otherwise keep its output pipes open.
#[cfg(windows)]
fn kill_tree(child: &Child) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = Command::new("taskkill")
        .args(["/PID", &child.id().to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

pub(crate) enum Chunk {
    Line(String),
    Progress(String),
}

const POLL_CHUNKS: usize = 128;
const POLL_BYTES: usize = 256 * 1024;

/// Splits a byte stream into lines (`\n`, `\r\n`) and progress updates (a lone `\r`).
fn pump(mut stream: impl Read, tx: Sender<Chunk>) {
    let mut buf: Vec<u8> = Vec::new();
    let mut cr = false;
    let mut bytes = [0u8; 4096];
    let flush = |buf: &mut Vec<u8>, progress: bool, tx: &Sender<Chunk>| -> bool {
        let text = String::from_utf8_lossy(buf).trim_end().to_string();
        buf.clear();
        if text.trim().is_empty() {
            return true;
        }
        tx.send(if progress {
            Chunk::Progress(text)
        } else {
            Chunk::Line(text)
        })
        .is_ok()
    };
    loop {
        let n = match stream.read(&mut bytes) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for &b in &bytes[..n] {
            if cr {
                cr = false;
                if b == b'\n' {
                    if !flush(&mut buf, false, &tx) {
                        return;
                    }
                    continue;
                }
                if !flush(&mut buf, true, &tx) {
                    return;
                }
            }
            match b {
                b'\n' => {
                    if !flush(&mut buf, false, &tx) {
                        return;
                    }
                }
                b'\r' => cr = true,
                _ => buf.push(b),
            }
        }
    }
    flush(&mut buf, false, &tx);
}

/// A running (or finished) git process. Poll it from the UI thread; `stop`
/// or drop it to terminate the process.
pub struct GitJob {
    builtin: Option<crate::ssh_transport::Control>,
    /// `git <args>` for logs and the Git console.
    pub command_line: String,
    pub started: Instant,
    /// Every output line so far (stdout and stderr interleaved).
    pub lines: Vec<String>,
    /// Latest progress update (`Receiving objects:  45% …`), if any.
    pub progress: Option<String>,
    /// The process, until it has been waited for.
    child: Option<Child>,
    owner: Option<rustpy_core::process_tree::ProcessTree>,
    readers: Vec<std::thread::JoinHandle<()>>,
    rx: Receiver<Chunk>,
    readers_done: bool,
    /// Exit code and time once the process was waited for (its output may
    /// still be arriving).
    ended: Option<(i32, Instant)>,
    exit: Option<i32>,
    /// When `stop` asked the process to end.
    stopped_at: Option<Instant>,
    /// The process group was killed (after `stop_grace`, or at once on Windows).
    killed_hard: bool,
    stop_grace: Duration,
    reservation: Option<crate::RepositoryReservation>,
}

impl GitJob {
    /// Starts `git <args>` in `cwd`.
    pub fn spawn(cwd: &Path, args: &[String], env: &GitEnv) -> anyhow::Result<GitJob> {
        let command_line = std::iter::once("git".to_string())
            .chain(args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ");
        let reservation = crate::RepositoryReservation::try_acquire(cwd, &command_line)?;
        if let Some(plan) = crate::ssh_transport::plan(cwd, args)? {
            let (tx, rx) = channel();
            let builtin =
                crate::ssh_transport::start(cwd.to_path_buf(), plan, env.clone(), reservation, tx);
            return Ok(GitJob {
                command_line: format!("{command_line} [built-in SSH]"),
                started: Instant::now(),
                lines: Vec::new(),
                progress: None,
                builtin: Some(builtin),
                child: None,
                owner: None,
                readers: Vec::new(),
                rx,
                readers_done: false,
                ended: None,
                exit: None,
                stopped_at: None,
                killed_hard: false,
                stop_grace: STOP_GRACE,
                reservation: None,
            });
        }
        let mut cmd = git_command(cwd, args, env);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(windows)]
        {
            // Attach the kill-on-close job before resuming the child.
            use std::os::windows::process::CommandExt;
            rustpy_core::process_tree::ProcessTree::prepare(&mut cmd);
            cmd.creation_flags(0x08000000 | 0x00000004 | 0x00000200);
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("cannot start {} ({})", command_line, git_program()))?;
        // Unix detach() already established the owned session/group. Do not
        // setpgid before setsid: that would prevent detaching the terminal.
        let owner = match rustpy_core::process_tree::ProcessTree::new(&mut child) {
            Ok(owner) => owner,
            Err(error) => {
                kill_hard(&mut child);
                let _ = child.wait();
                return Err(error.into());
            }
        };
        let mut readers = Vec::new();
        let (tx, rx) = channel();
        if let Some(out) = child.stdout.take() {
            let tx = tx.clone();
            readers.push(std::thread::spawn(move || pump(out, tx)));
        }
        if let Some(err) = child.stderr.take() {
            let tx = tx.clone();
            readers.push(std::thread::spawn(move || pump(err, tx)));
        }
        drop(tx);
        Ok(GitJob {
            builtin: None,
            command_line,
            started: Instant::now(),
            lines: Vec::new(),
            progress: None,
            child: Some(child),
            owner: Some(owner),
            readers,
            rx,
            readers_done: false,
            ended: None,
            exit: None,
            stopped_at: None,
            killed_hard: false,
            stop_grace: STOP_GRACE,
            reservation: Some(reservation),
        })
    }

    /// Collects output and the exit status (non-blocking); returns the new
    /// lines. After `stop`, it kills the process group once `stop_grace`
    /// has passed.
    pub fn poll(&mut self) -> Vec<String> {
        let mut new = Vec::new();
        let mut bytes = 0usize;
        // Never let an always-ready producer starve process/Stop polling. A
        // single indivisible line may exceed the byte budget; leave every
        // subsequent chunk queued rather than dropping or splitting output.
        for _ in 0..POLL_CHUNKS {
            if bytes >= POLL_BYTES {
                break;
            }
            match self.rx.try_recv() {
                Ok(Chunk::Line(l)) => {
                    bytes = bytes.saturating_add(l.len());
                    self.progress = None;
                    new.push(l);
                }
                Ok(Chunk::Progress(p)) => {
                    bytes = bytes.saturating_add(p.len());
                    self.progress = Some(p);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.readers_done = true;
                    break;
                }
            }
        }
        self.lines.extend(new.iter().cloned());
        if self.exit.is_some() {
            return new;
        }
        if let Some(builtin) = &self.builtin {
            match builtin.completion.try_recv() {
                Ok(code) => {
                    self.ended = Some((code, Instant::now()));
                }
                Err(TryRecvError::Disconnected) if self.ended.is_none() => {
                    self.ended = Some((-1, Instant::now()));
                }
                _ => {}
            }
        }
        if let Some(child) = &mut self.child {
            // Stopping: once git is gone (it held the pipes until it exited,
            // so it has cleaned up) or the grace period is over, what is left
            // of its process group is killed: helpers that ignore SIGTERM, such
            // as an ssh stuck in a prompt. Git is waited for only afterwards,
            // so until then the group id is still its own.
            if self
                .stopped_at
                .is_some_and(|t| self.readers_done || t.elapsed() >= self.stop_grace)
                && !self.killed_hard
            {
                if let Some(owner) = &self.owner {
                    owner.request_force();
                }
                self.killed_hard = true;
            }
            if self.stopped_at.is_none() || self.killed_hard {
                let code = match self
                    .owner
                    .as_ref()
                    .expect("Git child owns process tree")
                    .poll(child)
                {
                    Ok(Some(status)) => {
                        Some(status.code().unwrap_or(if self.stopped_at.is_some() {
                            130
                        } else {
                            -1
                        }))
                    }
                    Ok(None) => None,
                    Err(error) => {
                        // Observation failure never licenses release/reap on the
                        // UI thread. Retain the child and continue owned cleanup.
                        let message = format!("Git process cleanup pending: {error}");
                        if self.lines.last() != Some(&message) {
                            self.lines.push(message);
                        }
                        self.owner
                            .as_ref()
                            .expect("Git child owns process tree")
                            .request_force();
                        None
                    }
                };
                if let Some(code) = code {
                    self.ended = Some((code, Instant::now()));
                    self.child = None;
                }
            }
        }
        if let Some((code, _)) = self.ended {
            if self.readers_done
                && self
                    .readers
                    .iter()
                    .all(std::thread::JoinHandle::is_finished)
            {
                for reader in self.readers.drain(..) {
                    let _ = reader.join();
                }
                self.exit = Some(code);
                self.progress = None;
                self.reservation = None;
                self.owner = None;
            }
        }
        new
    }

    pub fn is_running(&self) -> bool {
        self.exit.is_none()
    }

    /// Exit code once the process ended and all its output was read.
    pub fn exit_code(&self) -> Option<i32> {
        self.exit
    }

    pub fn succeeded(&self) -> bool {
        self.exit == Some(0) && self.stopped_at.is_none()
    }

    /// True when `stop` ended the process.
    pub fn was_stopped(&self) -> bool {
        self.stopped_at.is_some()
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// The whole output as one text.
    pub fn output(&self) -> String {
        self.lines.join("\n")
    }

    /// The most telling output line: the last error (`fatal:`/`error:`) or
    /// rejection, else the last line.
    pub fn summary_line(&self) -> String {
        let pick = |pred: &dyn Fn(&str) -> bool| {
            self.lines
                .iter()
                .rev()
                .find(|l| pred(l.trim()))
                .map(|l| l.trim().to_string())
        };
        pick(&|l: &str| {
            l.starts_with("fatal:")
                || l.starts_with("error:")
                || l.starts_with("ERROR:")
                || l.contains("[rejected]")
                || l.contains("Permission denied")
        })
        .or_else(|| pick(&|l: &str| !l.is_empty()))
        .unwrap_or_default()
    }

    /// Ends the process with the ssh and helper processes it started. On
    /// Unix they get SIGTERM first, so git can remove a partial clone and
    /// its `.lock` files; `poll` kills them if they are still running after
    /// a grace period (at once when `stop` is called again). On Windows the
    /// process tree is killed.
    pub fn stop(&mut self) {
        if let Some(builtin) = &self.builtin {
            self.stopped_at.get_or_insert_with(Instant::now);
            builtin
                .cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        let Some(child) = &mut self.child else { return };
        if self.stopped_at.is_some() {
            if !self.killed_hard {
                if let Some(owner) = &self.owner {
                    owner.request_force();
                }
                self.killed_hard = true;
            }
            return;
        }
        self.stopped_at = Some(Instant::now());
        self.killed_hard = terminate(
            child,
            self.owner.as_ref().expect("Git child owns process tree"),
        );
    }

    /// Blocks until the process ended (tests, short local commands).
    pub fn wait(&mut self, timeout: Duration) -> bool {
        let start = Instant::now();
        while self.is_running() {
            self.poll();
            if start.elapsed() > timeout {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }
}

impl Drop for GitJob {
    /// A job dropped while running (its dialog was closed) is stopped, and
    /// finishes in the background: git may need a moment to clean up.
    fn drop(&mut self) {
        if let Some(builtin) = &self.builtin {
            builtin
                .cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if self.child.is_none() && self.readers.is_empty() {
            return;
        }
        if self.stopped_at.is_none() {
            self.stop();
        }
        let (_, closed) = channel();
        let mut rest = GitJob {
            builtin: None,
            command_line: std::mem::take(&mut self.command_line),
            started: self.started,
            lines: Vec::new(),
            progress: None,
            child: self.child.take(),
            owner: self.owner.take(),
            readers: std::mem::take(&mut self.readers),
            rx: std::mem::replace(&mut self.rx, closed),
            readers_done: self.readers_done,
            ended: self.ended,
            exit: None,
            stopped_at: self.stopped_at,
            killed_hard: self.killed_hard,
            stop_grace: self.stop_grace,
            reservation: self.reservation.take(),
        };
        std::thread::spawn(move || {
            while rest.is_running() {
                rest.poll();
                rest.lines.clear();
                std::thread::sleep(Duration::from_millis(20));
            }
        });
    }
}

/// A git error line for the status bar: without the `fatal:` prefix, and
/// a cancelled credential prompt ("… terminal prompts disabled", as the
/// IDE runs git without a terminal) said in words.
pub fn describe_failure(line: &str) -> String {
    if line.contains("dubious ownership") || line.contains("not owned by current user") {
        return format!("Git refused this repository's ownership. Verify the directory owner, then explicitly trust its canonical path with git config --global --add safe.directory <path>. RustPy does not change safe.directory automatically.\n{}", line.trim());
    }
    let s = line.trim();
    let s = s
        .strip_prefix("fatal: ")
        .or_else(|| s.strip_prefix("error: "))
        .unwrap_or(s);
    match s.split_once(": terminal prompts disabled") {
        Some((what, _)) => format!("{what}: no answer was given"),
        None => s.to_string(),
    }
}

/// Did the git output say that authentication failed?
pub fn is_auth_failure(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    if [
        "failed to authenticate",
        "authentication failed",
        "authentication required",
        "invalid credentials",
    ]
    .iter()
    .any(|message| lower.contains(message))
    {
        return true;
    }
    [
        "could not read Username",
        "could not read Password",
        "Authentication failed",
        "Permission denied (publickey",
        "Permission denied, please try again",
        "terminal prompts disabled",
        "Host key verification failed",
        "incorrect passphrase",
        "bad passphrase",
    ]
    .iter()
    .any(|m| output.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output_job(rx: Receiver<Chunk>) -> GitJob {
        GitJob {
            builtin: None,
            command_line: String::new(),
            started: Instant::now(),
            lines: Vec::new(),
            progress: None,
            child: None,
            owner: None,
            readers: Vec::new(),
            rx,
            readers_done: false,
            ended: Some((0, Instant::now())),
            exit: None,
            stopped_at: None,
            killed_hard: false,
            stop_grace: STOP_GRACE,
            reservation: None,
        }
    }

    #[test]
    fn ready_producer_output_is_budgeted_and_retained_until_disconnected() {
        let (tx, rx) = channel();
        let count = POLL_CHUNKS * 3;
        std::thread::spawn(move || {
            for index in 0..count {
                tx.send(Chunk::Line(index.to_string())).unwrap();
            }
        })
        .join()
        .unwrap();
        let mut job = output_job(rx);
        for pass in 0..3 {
            let output = job.poll();
            assert_eq!(output.len(), POLL_CHUNKS);
            assert_eq!(output[0], (pass * POLL_CHUNKS).to_string());
            assert!(!job.readers_done, "budget exhaustion is not EOF");
            assert!(job.exit.is_none(), "queued output retains completion");
        }
        assert!(job.poll().is_empty());
        assert!(job.readers_done);
        assert_eq!(job.exit, Some(0));
        assert_eq!(
            job.lines,
            (0..count).map(|i| i.to_string()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn progress_bytes_share_budget_without_discarding_following_lines() {
        let (tx, rx) = channel();
        tx.send(Chunk::Progress("p".repeat(POLL_BYTES))).unwrap();
        tx.send(Chunk::Line("after progress".into())).unwrap();
        drop(tx);
        let mut job = output_job(rx);
        assert!(job.poll().is_empty());
        assert_eq!(job.progress.as_ref().unwrap().len(), POLL_BYTES);
        assert!(!job.readers_done);
        assert!(job.exit.is_none());
        assert_eq!(job.poll(), vec!["after progress"]);
        assert!(job.progress.is_none());
        assert!(job.readers_done);
        assert_eq!(job.exit, Some(0));
    }

    fn chunks(input: &[u8]) -> Vec<String> {
        let (tx, rx) = channel();
        pump(input, tx);
        rx.into_iter()
            .map(|c| match c {
                Chunk::Line(l) => format!("L:{l}"),
                Chunk::Progress(p) => format!("P:{p}"),
            })
            .collect()
    }

    #[test]
    fn lines_and_progress_are_split() {
        assert_eq!(
            chunks(b"one\ntwo\r\nthree"),
            vec!["L:one", "L:two", "L:three"]
        );
        assert_eq!(
            chunks(b"Receiving objects:  10% (1/10)\rReceiving objects: 100% (10/10), done.\nok\n"),
            vec![
                "P:Receiving objects:  10% (1/10)",
                "L:Receiving objects: 100% (10/10), done.",
                "L:ok"
            ]
        );
        assert_eq!(chunks(b"\n\n  \nx\r"), vec!["L:x"]);
    }

    #[test]
    fn a_job_streams_output_and_reports_failure() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut job =
            GitJob::spawn(dir.path(), &["--version".to_string()], &GitEnv::default()).unwrap();
        assert!(job.wait(Duration::from_secs(20)));
        assert!(job.succeeded(), "{:?}", job.lines);
        assert!(job.output().starts_with("git version"));
        let mut job = GitJob::spawn(dir.path(), &["log".to_string()], &GitEnv::default()).unwrap();
        assert!(job.wait(Duration::from_secs(20)));
        assert!(!job.succeeded());
        assert!(
            job.summary_line().starts_with("fatal:"),
            "{}",
            job.summary_line()
        );
    }

    #[test]
    fn stop_ends_a_running_job() {
        if Command::new("git").arg("--version").output().is_err() || cfg!(windows) {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        // `git hash-object --stdin` would read stdin, which is closed; a
        // nonexistent remote helper is not portable either, so wait on a
        // never-ending upload-pack served by a slow ssh replacement.
        std::fs::write(dir.path().join("slow-ssh"), "#!/bin/sh\nsleep 30\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                dir.path().join("slow-ssh"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let env = GitEnv {
            vars: vec![(
                "GIT_SSH_COMMAND".into(),
                format!(
                    "'{}'",
                    dir.path()
                        .join("slow-ssh")
                        .display()
                        .to_string()
                        .replace('\'', "'\\''")
                ),
            )],
        };
        let mut job = GitJob::spawn(
            dir.path(),
            &["ls-remote".into(), "ssh://example.invalid/repo.git".into()],
            &env,
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        job.poll();
        assert!(job.is_running());
        let start = Instant::now();
        job.stop();
        assert!(
            job.wait(Duration::from_secs(10)),
            "the process group was killed"
        );
        assert!(start.elapsed() < Duration::from_secs(10));
        assert!(job.was_stopped() && !job.succeeded());
    }

    /// An executable script in `dir` (a stand-in for ssh).
    #[cfg(unix)]
    fn script(dir: &Path, name: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    /// Whether a process with this pid exists (zombies included).
    #[cfg(unix)]
    fn exists(pid: u32) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        unsafe { kill(pid as i32, 0) == 0 }
    }

    /// Review: `stop` killed git at once, so it could not remove a partial
    /// clone (or `.lock` files). SIGTERM comes first; git cleans up.
    #[cfg(unix)]
    #[test]
    fn a_stopped_clone_removes_its_partial_directory() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let env = GitEnv {
            vars: vec![(
                "GIT_SSH_COMMAND".into(),
                format!(
                    "'{}'",
                    script(dir.path(), "slow-ssh", "#!/bin/sh\nsleep 30\n").replace('\'', "'\\''")
                ),
            )],
        };
        let target = dir.path().join("clone");
        let mut job = GitJob::spawn(
            dir.path(),
            &[
                "clone".into(),
                "ssh://example.invalid/repo.git".into(),
                target.display().to_string(),
            ],
            &env,
        )
        .unwrap();
        let start = Instant::now();
        while !target.join(".git").exists() && start.elapsed() < Duration::from_secs(20) {
            job.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            target.join(".git").exists(),
            "git created the clone directory before connecting"
        );
        let start = Instant::now();
        job.stop();
        assert!(job.wait(Duration::from_secs(20)));
        assert!(
            start.elapsed() < STOP_GRACE,
            "git ended on SIGTERM, before the grace period"
        );
        assert!(job.was_stopped() && !job.succeeded());
        assert!(!target.exists(), "git removed its partial clone");
    }

    /// The pids an ssh stand-in made by [`stubborn_ssh`] ran as.
    #[cfg(unix)]
    fn pids(dir: &Path) -> Vec<u32> {
        std::fs::read_to_string(dir.join("pids"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect()
    }

    /// An ssh stand-in that ignores SIGTERM and never answers.
    #[cfg(unix)]
    fn stubborn_ssh(dir: &Path) -> String {
        let path = script(
            dir,
            "stubborn-ssh",
            &format!(
                "#!/bin/sh\necho $$ >> '{}'\ntrap '' TERM\nexec sleep 30\n",
                dir.join("pids").display()
            ),
        );
        format!("'{}'", path.replace('\'', "'\\''"))
    }

    #[cfg(unix)]
    fn gone(pid: u32) -> bool {
        let start = Instant::now();
        while exists(pid) && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        !exists(pid)
    }

    /// Helpers that ignore SIGTERM do not outlive a stopped job: here git's
    /// `ssh -G` probe, which does not write to the job's pipes.
    #[cfg(unix)]
    #[test]
    fn helpers_ignoring_sigterm_do_not_outlive_a_stopped_job() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let env = GitEnv {
            vars: vec![("GIT_SSH_COMMAND".into(), stubborn_ssh(dir.path()))],
        };
        let mut job = GitJob::spawn(
            dir.path(),
            &["ls-remote".into(), "ssh://example.invalid/repo.git".into()],
            &env,
        )
        .unwrap();
        let start = Instant::now();
        while pids(dir.path()).is_empty() && start.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(pids(dir.path()).len(), 1, "git started the ssh stand-in");
        job.stop();
        assert!(job.wait(Duration::from_secs(10)));
        assert!(job.was_stopped() && !job.succeeded());
        assert!(
            pids(dir.path()).into_iter().all(gone),
            "the stubborn helper was killed"
        );
    }

    /// A helper that ignores SIGTERM and keeps the job's output pipe open is
    /// killed after the grace period, and the job ends.
    #[cfg(target_os = "linux")] // on macOS the job ends before the grace period; see known issues
    #[test]
    fn helpers_holding_the_output_are_killed_after_the_grace_period() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        // GIT_SSH_VARIANT skips the `ssh -G` probe: the stand-in gets git's stderr.
        let env = GitEnv {
            vars: vec![
                ("GIT_SSH_COMMAND".into(), stubborn_ssh(dir.path())),
                ("GIT_SSH_VARIANT".into(), "ssh".into()),
            ],
        };
        let mut job = GitJob::spawn(
            dir.path(),
            &["ls-remote".into(), "ssh://example.invalid/repo.git".into()],
            &env,
        )
        .unwrap();
        job.stop_grace = Duration::from_millis(500);
        let start = Instant::now();
        while pids(dir.path()).is_empty() && start.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let start = Instant::now();
        job.stop();
        std::thread::sleep(Duration::from_millis(200));
        job.poll();
        assert!(
            job.is_running(),
            "the helper still holds the pipe within the grace period"
        );
        assert!(
            crate::RepositoryReservation::try_acquire(dir.path(), "Fetch").is_err(),
            "cancelled job keeps its reservation through cleanup"
        );
        assert!(
            job.wait(Duration::from_secs(10)),
            "the stubborn helper was killed"
        );
        assert!(
            start.elapsed() >= Duration::from_millis(500)
                && start.elapsed() < Duration::from_secs(10)
        );
        assert!(job.killed_hard && job.was_stopped() && !job.succeeded());
        assert!(
            crate::RepositoryReservation::try_acquire(dir.path(), "Fetch").is_ok(),
            "cleanup releases the repository"
        );
        assert!(pids(dir.path()).into_iter().all(gone));
    }

    /// A job dropped while running (its dialog closed) is stopped and
    /// waited for in the background, without blocking the caller.
    #[cfg(unix)]
    #[test]
    fn a_dropped_job_ends_in_the_background() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let env = GitEnv {
            vars: vec![
                ("GIT_SSH_COMMAND".into(), stubborn_ssh(dir.path())),
                ("GIT_SSH_VARIANT".into(), "ssh".into()),
            ],
        };
        let mut job = GitJob::spawn(
            dir.path(),
            &["ls-remote".into(), "ssh://example.invalid/repo.git".into()],
            &env,
        )
        .unwrap();
        job.stop_grace = Duration::from_millis(500);
        let pid = job.child.as_ref().unwrap().id();
        let deadline = Instant::now() + Duration::from_secs(10);
        while pids(dir.path()).is_empty() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        let start = Instant::now();
        drop(job);
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "drop does not wait"
        );
        assert!(
            crate::RepositoryReservation::try_acquire(dir.path(), "Fetch").is_err(),
            "dropping a job transfers its reservation to the cleanup worker"
        );
        while exists(pid) && start.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!exists(pid), "git ended and was waited for");
        while crate::running_operation(dir.path()).is_some() {
            assert!(start.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(crate::RepositoryReservation::try_acquire(dir.path(), "Fetch").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn natural_exit_cleans_owned_helpers_and_joins_pipe_readers_before_retirement() {
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let mut job = GitJob::spawn(
            dir.path(),
            &[
                "-c".into(),
                "alias.owned-fixture=!sh -c 'sleep 30 & echo $!; exit 0'".into(),
                "owned-fixture".into(),
            ],
            &GitEnv::default(),
        )
        .unwrap();
        assert!(
            job.wait(Duration::from_secs(5)),
            "helpers must not hold pipes for their sleep duration"
        );
        assert!(job.succeeded(), "{}", job.output());
        assert!(
            job.lines.iter().any(|line| line.parse::<u32>().is_ok()),
            "actual helper PID was captured"
        );
        assert!(job.child.is_none());
        assert!(job.owner.is_none());
        assert!(
            job.readers_done && job.readers.is_empty(),
            "physical pipe workers retired before completion"
        );
        assert!(crate::RepositoryReservation::try_acquire(dir.path(), "Fetch").is_ok());
    }

    #[test]
    fn auth_failures_are_recognised() {
        assert_eq!(describe_failure("fatal: could not read Username for 'https://github.com': terminal prompts disabled"), "could not read Username for 'https://github.com': no answer was given");
        assert_eq!(
            describe_failure("fatal: Authentication failed for 'https://x/'"),
            "Authentication failed for 'https://x/'"
        );
        assert!(is_auth_failure(
            "fatal: could not read Username for 'https://github.com': terminal prompts disabled"
        ));
        assert!(is_auth_failure(
            "git@github.com: Permission denied (publickey)."
        ));
        assert!(!is_auth_failure("fatal: repository 'x' not found"));
    }
}
