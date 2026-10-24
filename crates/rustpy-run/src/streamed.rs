//! Internal helper: a child process whose stdout/stderr lines are streamed
//! through channels (shared by the coverage and profiler runners).

use crate::output_encoding::{Decoder, Encoding};
use crate::process_tree::ProcessTree;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::process::RunSession;
use rustpy_project::config::{RunConfig, RunConfigKind};
use rustpy_project::interpreter::Interpreter;

/// How a child process ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Exit {
    Code(i32),
    Killed,
}

impl Exit {
    pub(crate) fn code(&self) -> Option<i32> {
        match self {
            Exit::Code(c) => Some(*c),
            Exit::Killed => None,
        }
    }
}

/// The pieces of a Python command line produced by [`RunSession::command_for`],
/// split so that `-m coverage run …`, the profiler runner or `-m debugpy …` can be
/// inserted between the interpreter options and the target.
pub struct PythonCommand {
    pub python: PathBuf,
    pub interpreter_options: Vec<String>,
    /// Everything after the interpreter options: `script args…` or `-m module args…`.
    pub target: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    /// The configuration's "Redirect input from" file.
    pub input: Option<PathBuf>,
    pub inherit_environment: bool,
}

impl PythonCommand {
    /// Splits the configuration's command; errors for non-Python configurations.
    pub fn for_config(
        cfg: &RunConfig,
        interpreter: Option<&Interpreter>,
        project_root: &Path,
        feature: &str,
    ) -> anyhow::Result<PythonCommand> {
        if matches!(
            cfg.kind,
            RunConfigKind::ShellScript { .. } | RunConfigKind::Program { .. }
        ) {
            anyhow::bail!("{feature} is only available for Python run configurations");
        }
        let (program, args, cwd, env) = RunSession::command_for(cfg, interpreter, project_root);
        let n = RunConfig::split_args(&cfg.interpreter_options)
            .len()
            .min(args.len());
        let (opts, target) = args.split_at(n);
        Ok(PythonCommand {
            python: program,
            interpreter_options: opts.to_vec(),
            target: target.to_vec(),
            cwd,
            env,
            input: cfg.input_file(project_root),
            inherit_environment: cfg.workflow.inherit_environment,
        })
    }

    /// Standard input of the process: the redirect file (empty when it
    /// cannot be opened), otherwise none.
    pub(crate) fn stdin(&self) -> Stdio {
        match &self.input {
            Some(path) => std::fs::File::open(path)
                .map(Stdio::from)
                .unwrap_or_else(|_| Stdio::null()),
            None => Stdio::null(),
        }
    }

    /// `python [opts] <inserted…> <target…>` rendered for display.
    pub fn command_line(&self, inserted: &[String]) -> String {
        std::iter::once(self.python.to_string_lossy().into_owned())
            .chain(self.interpreter_options.iter().cloned())
            .chain(inserted.iter().cloned())
            .chain(self.target.iter().cloned())
            .map(|a| {
                if a.contains(' ') {
                    format!("\"{a}\"")
                } else {
                    a
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// A bounded reader chunk. A physical stdout/stderr line can span several
/// chunks; protocol consumers must not invent newlines at chunk boundaries.
pub(crate) struct OutputChunk {
    pub text: String,
    pub is_stderr: bool,
    pub line_end: bool,
}

/// A spawned process with line-streamed output.
pub(crate) struct Streamed {
    child: Arc<Mutex<Child>>,
    tree: Arc<ProcessTree>,
    capture_cancel: Arc<AtomicBool>,
    readers: Arc<AtomicUsize>,
    pending_exit: Option<Exit>,
    rx: Receiver<OutputChunk>,
    exit_rx: Receiver<Exit>,
    exit: Option<Exit>,
    pub started: Instant,
    pub finished_at: Option<Instant>,
}

impl Streamed {
    pub(crate) fn spawn(
        python: &Path,
        args: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        stdin: Stdio,
    ) -> anyhow::Result<Streamed> {
        Self::spawn_with_environment(python, args, cwd, env, stdin, true)
    }
    pub(crate) fn spawn_with_environment(
        python: &Path,
        args: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        stdin: Stdio,
        inherit_environment: bool,
    ) -> anyhow::Result<Streamed> {
        let mut cmd = rustpy_project::interpreter::interpreter_command(python);
        if !inherit_environment {
            cmd.env_clear();
        }
        cmd.args(args)
            .current_dir(cwd)
            .envs(env)
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        ProcessTree::prepare(&mut cmd);
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("Cannot run program \"{}\": {e}", python.display()))?;
        let tree = match ProcessTree::new(&mut child) {
            Ok(tree) => Arc::new(tree),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        };
        let capture_cancel = Arc::new(AtomicBool::new(false));
        let readers = Arc::new(AtomicUsize::new(0));
        let encoding = Encoding::parse(
            env.get("PYTHONIOENCODING")
                .map(|label| label.split(':').next().unwrap_or(label))
                .unwrap_or("utf-8"),
        )
        .unwrap_or(Encoding::Utf8);
        let (tx, rx) = crossbeam_channel::bounded(256);
        let (exit_tx, exit_rx) = unbounded();
        if let Some(out) = child.stdout.take() {
            spawn_reader(
                out,
                false,
                tx.clone(),
                encoding,
                capture_cancel.clone(),
                readers.clone(),
            );
        }
        if let Some(err) = child.stderr.take() {
            spawn_reader(
                err,
                true,
                tx,
                encoding,
                capture_cancel.clone(),
                readers.clone(),
            );
        }
        let child = Arc::new(Mutex::new(child));
        let waiter = child.clone();
        let owner = tree.clone();
        std::thread::spawn(move || {
            let status = loop {
                let r = waiter.lock().map(|mut child| owner.poll(&mut child));
                match r {
                    Ok(Ok(Some(status))) => break status,
                    Ok(Ok(None)) => std::thread::sleep(Duration::from_millis(25)),
                    _ => {
                        let _ = exit_tx.send(Exit::Killed);
                        return;
                    }
                }
            };
            #[cfg(unix)]
            let killed = {
                use std::os::unix::process::ExitStatusExt;
                status.signal().is_some()
            };
            #[cfg(not(unix))]
            let killed = false;
            let _ = exit_tx.send(if killed {
                Exit::Killed
            } else {
                Exit::Code(status.code().unwrap_or(-1))
            });
        });
        Ok(Streamed {
            child,
            tree,
            capture_cancel,
            readers,
            pending_exit: None,
            rx,
            exit_rx,
            exit: None,
            started: Instant::now(),
            finished_at: None,
        })
    }

    /// Console-compatible chunks (long physical lines keep their existing
    /// 64 KiB splitting behavior, without trailing newlines).
    pub(crate) fn poll_lines(&mut self) -> Vec<(String, bool)> {
        self.poll_chunks()
            .into_iter()
            .map(|chunk| (chunk.text, chunk.is_stderr))
            .collect()
    }

    /// New chunks including their physical line boundary for protocol readers.
    pub(crate) fn poll_chunks(&mut self) -> Vec<OutputChunk> {
        let mut output = Vec::new();
        for _ in 0..128 {
            match self.rx.try_recv() {
                Ok(line) => output.push(line),
                Err(_) => break,
            }
        }
        if self.exit.is_none() {
            if self.pending_exit.is_none() {
                if let Ok(exit) = self.exit_rx.try_recv() {
                    self.pending_exit = Some(exit);
                }
            }
            if self.pending_exit.is_some()
                && self.readers.load(Ordering::Acquire) == 0
                && self.rx.is_empty()
            {
                self.exit = self.pending_exit.take();
                self.finished_at = Some(Instant::now());
            }
        }
        output
    }
    pub(crate) fn lifecycle_stamp(&self) -> String {
        self.tree.stamp()
    }
    pub(crate) fn interrupt(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            if let Err(error) = self.tree.interrupt(&mut child) {
                log::warn!("Cannot interrupt measured run: {error}");
            }
        }
    }
    pub(crate) fn exit(&self) -> Option<&Exit> {
        self.exit.as_ref()
    }

    pub(crate) fn is_running(&self) -> bool {
        self.exit.is_none()
    }

    pub(crate) fn stop(&mut self) {
        self.capture_cancel.store(true, Ordering::Release);
        // The retained waiter owns child + tree through physical retirement;
        // cancellation never waits for its mutex or cleanup on the UI thread.
        self.tree.request_force();
    }
    pub(crate) fn elapsed(&self) -> Duration {
        self.finished_at
            .unwrap_or_else(Instant::now)
            .duration_since(self.started)
    }
}

impl Drop for Streamed {
    fn drop(&mut self) {
        self.capture_cancel.store(true, Ordering::Release);
        self.stop();
    }
}
fn spawn_reader<R: Read + Send + 'static>(
    mut reader: R,
    is_stderr: bool,
    tx: Sender<OutputChunk>,
    encoding: Encoding,
    cancel: Arc<AtomicBool>,
    readers: Arc<AtomicUsize>,
) {
    readers.fetch_add(1, Ordering::AcqRel);
    std::thread::spawn(move || {
        struct Finished(Arc<AtomicUsize>);
        impl Drop for Finished {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::AcqRel);
            }
        }
        let _finished = Finished(readers);
        let mut decoder = Decoder::new(encoding);
        let mut bytes = [0; 8192];
        let mut pending = String::new();
        let mut capture = true;
        let send = |line: String, line_end: bool, capture: &mut bool| {
            if !*capture {
                return;
            }
            let mut item = OutputChunk {
                text: line,
                is_stderr,
                line_end,
            };
            loop {
                if cancel.load(Ordering::Acquire) {
                    *capture = false;
                    return;
                }
                match tx.send_timeout(item, Duration::from_millis(25)) {
                    Ok(()) => return,
                    Err(crossbeam_channel::SendTimeoutError::Timeout(line)) => item = line,
                    Err(_) => {
                        *capture = false;
                        return;
                    }
                }
            }
        };
        loop {
            let finished = match reader.read(&mut bytes) {
                Ok(0) => true,
                Ok(count) => {
                    if capture {
                        pending.push_str(&decoder.push(&bytes[..count], false));
                    }
                    false
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => true,
            };
            if finished {
                pending.push_str(&decoder.push(&[], true));
            }
            while let Some(newline) = pending.find('\n') {
                let rest = pending.split_off(newline + 1);
                let mut line = std::mem::replace(&mut pending, rest);
                while line.ends_with('\r') || line.ends_with('\n') {
                    line.pop();
                }
                send(line, true, &mut capture);
            }
            while pending.len() > 64 * 1024 {
                let mut end = 64 * 1024;
                while !pending.is_char_boundary(end) {
                    end -= 1;
                }
                let rest = pending.split_off(end);
                send(std::mem::replace(&mut pending, rest), false, &mut capture);
            }
            if !capture {
                pending.clear();
            }
            if finished {
                if !pending.is_empty() {
                    send(pending, true, &mut capture);
                }
                break;
            }
        }
    });
}

/// Runs `python args…` to completion with a timeout, returning stdout on success.
pub(crate) fn run_quiet(
    python: &Path,
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
) -> Option<String> {
    let arguments = args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    let directory = cwd
        .map(Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())?;
    let mut process = Streamed::spawn(
        python,
        &arguments,
        &directory,
        &BTreeMap::new(),
        Stdio::null(),
    )
    .ok()?;
    let started = Instant::now();
    let mut output = String::new();
    loop {
        for chunk in process.poll_chunks() {
            if !chunk.is_stderr {
                if output.len() + chunk.text.len() + usize::from(chunk.line_end) > 1024 * 1024 {
                    process.stop();
                    return None;
                }
                output.push_str(&chunk.text);
                if chunk.line_end {
                    output.push('\n');
                }
            }
        }
        if let Some(exit) = process.exit() {
            return (exit.code() == Some(0)).then_some(output);
        }
        if started.elapsed() > timeout {
            process.stop();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(all(test, unix))]
mod lifecycle_tests {
    use super::*;

    #[test]
    fn streamed_exit_preserves_partial_utf8_and_inherited_child_output() {
        let mut process = Streamed::spawn_with_environment(
            Path::new("/bin/sh"),
            &[
                "-c".into(),
                r"printf '\342'; printf '\202\254'; (printf ' helper\n') & wait".into(),
            ],
            Path::new("/"),
            &BTreeMap::new(),
            Stdio::null(),
            false,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut lines = Vec::new();
        while process.is_running() && Instant::now() < deadline {
            lines.extend(process.poll_lines());
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(process.exit().and_then(Exit::code), Some(0));
        assert_eq!(lines, vec![("€ helper".into(), false)]);
    }
}
