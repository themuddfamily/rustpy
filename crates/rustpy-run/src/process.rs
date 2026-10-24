//! Process execution with streamed output (roadmap §3.2).

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crossbeam_channel::{unbounded, Receiver, Sender};
use once_cell::sync::Lazy;
use regex::Regex;

use crate::console_buffer::ConsoleBuffer;
use crate::output_encoding::{Decoder, Encoding};
use crate::process_tree::ProcessTree;
use rustpy_project::config::{RunConfig, RunConfigKind};
use rustpy_project::interpreter::Interpreter;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
mod compose_lifecycle;
mod docker_lifecycle;

/// Standard-input ownership and environment policy for a captured command.
pub struct CommandInput {
    pub stdin: Option<Stdio>,
    pub inherit_environment: bool,
}
/// Redirected input remains a file path until the PTY host owns the command.
pub struct PtyInput<'a> {
    pub inherit_environment: bool,
    pub path: Option<&'a Path>,
}

/// Bytes of raw output a session keeps; older chunks are dropped. The console
/// view has its own line-based cycle buffer ([`ConsoleBuffer`]).
pub const RAW_OUTPUT_LIMIT: usize = 4 << 20;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OutputKind {
    Stdout,
    Stderr,
    System,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputChunk {
    pub kind: OutputKind,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    Running,
    Exited(i32),
    Killed,
    Failed(String),
}

/// A running (or finished) process with its captured output.
pub struct RunSession {
    pub id: u64,
    pub name: String,
    pub command_line: String,
    pub started: Instant,
    pub status: SessionStatus,
    /// Raw output chunks as read, the newest [`RAW_OUTPUT_LIMIT`] bytes.
    pub output: Vec<OutputChunk>,
    /// The output as console lines (colors, `\r` progress bars, cycle buffer).
    pub console: ConsoleBuffer,
    output_bytes: usize,
    stdout_revision: u64,
    stderr_revision: u64,
    raw_limit: usize,
    input_encoding: Encoding,
    pty: Option<PtySession>,
    tree: Option<Arc<ProcessTree>>,
    capture_cancel: Arc<AtomicBool>,
    readers: Arc<AtomicUsize>,
    pending_exit: Option<SessionStatus>,
    docker_owner: Option<docker_lifecycle::Owner>,
    local_retired: Arc<AtomicBool>,
    docker_local: Arc<docker_lifecycle::Local>,
    launched: bool,
    pub finished_at: Option<Instant>,
    /// The configuration the session runs, as launched (macros expanded; set
    /// by [`RunSession::spawn`], `None` for a bare command line). Rerun of an
    /// External Tool's session repeats it; a Run configuration's Rerun goes
    /// through the stored configuration (see
    /// [`RunConfigStore::config_for_rerun`](crate::RunConfigStore::config_for_rerun)).
    pub config: Option<RunConfig>,
    child: Option<Arc<Mutex<Child>>>,
    rx: Receiver<OutputChunk>,
    status_rx: Receiver<SessionStatus>,
    stdin: Option<Arc<Mutex<std::process::ChildStdin>>>,
    redirected_input: bool,
}

struct PtySession {
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    writer: Mutex<Box<dyn Write + Send>>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    size: Mutex<(u16, u16)>,
}

#[cfg(not(windows))]
struct PtyParts {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
}
impl Drop for RunSession {
    fn drop(&mut self) {
        self.capture_cancel.store(true, Ordering::Release);
        self.stop();
    }
}

impl RunSession {
    pub fn interpreter_option_count(cfg: &RunConfig) -> usize {
        RunConfig::split_args(&cfg.interpreter_options).len()
    }
    /// Builds the command for a configuration.
    pub fn command_for(
        cfg: &RunConfig,
        interpreter: Option<&Interpreter>,
        project_root: &Path,
    ) -> (PathBuf, Vec<String>, PathBuf, BTreeMap<String, String>) {
        let python = cfg
            .interpreter
            .clone()
            .or_else(|| interpreter.map(|i| i.path.clone()))
            .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "python" } else { "python3" }));
        let mut args: Vec<String> = Vec::new();
        let mut program = python.clone();
        let interp_opts = RunConfig::split_args(&cfg.interpreter_options);
        let cwd = Self::working_dir_for(cfg, project_root);
        match &cfg.kind {
            RunConfigKind::Python { script } => {
                args.extend(interp_opts);
                args.push(script.to_string_lossy().into_owned());
            }
            RunConfigKind::PythonModule { module } => {
                args.extend(interp_opts);
                args.push("-m".into());
                args.push(module.clone());
            }
            RunConfigKind::Pytest { target, keywords } => {
                args.extend(interp_opts);
                args.extend(["-m".to_string(), "pytest".to_string()]);
                if !target.is_empty() {
                    // Node ids are relative to the project root; keep them
                    // valid when the process runs in another directory.
                    let file_part = target.split("::").next().unwrap_or(target);
                    let rel = Path::new(file_part);
                    if cwd != project_root && !rel.is_absolute() && project_root.join(rel).exists()
                    {
                        args.push(project_root.join(target).to_string_lossy().into_owned());
                    } else {
                        args.push(target.clone());
                    }
                }
                if !keywords.is_empty() {
                    args.push("-k".into());
                    args.push(keywords.clone());
                }
            }
            RunConfigKind::Unittest { target } => {
                args.extend(interp_opts);
                args.extend(["-m".to_string(), "unittest".to_string()]);
                args.extend(rustpy_project::config::unittest_cli_args(
                    target,
                    project_root,
                ));
            }
            RunConfigKind::ShellScript { script } => {
                program = PathBuf::from(if cfg!(windows) { "cmd" } else { "sh" });
                args.push(script.to_string_lossy().into_owned());
            }
            RunConfigKind::Program {
                program: p,
                args: a,
            } => {
                program = resolve_program(p);
                args.extend(a.iter().cloned());
            }
        }
        args.extend(RunConfig::split_args(&cfg.parameters));
        let (mut env, errors) = cfg.load_env(project_root);
        for e in errors {
            log::warn!("{}: {e}", cfg.name);
        }
        env.entry("PYTHONUNBUFFERED".into()).or_insert("1".into());
        env.entry("PYTHONIOENCODING".into())
            .or_insert_with(|| cfg.console_encoding.clone());
        if cfg.console_encoding.eq_ignore_ascii_case("utf-8") {
            env.entry("PYTHONUTF8".into()).or_insert("1".into());
        }
        if cfg.add_content_roots {
            let mut pp = project_root.to_string_lossy().into_owned();
            if let Some(existing) = cfg
                .workflow
                .inherit_environment
                .then(|| std::env::var("PYTHONPATH").ok())
                .flatten()
            {
                if !existing.is_empty() {
                    pp.push(if cfg!(windows) { ';' } else { ':' });
                    pp.push_str(&existing);
                }
            }
            env.entry("PYTHONPATH".into()).or_insert(pp);
        }
        (program, args, cwd, env)
    }

    /// The configuration's working directory (see [`RunConfig::working_dir_in`]).
    pub fn working_dir_for(cfg: &RunConfig, project_root: &Path) -> PathBuf {
        cfg.working_dir_in(project_root)
    }

    /// Spawns the configuration's process; its "Redirect input from" file
    /// becomes the standard input (see [`config_stdin`]).
    pub fn spawn(
        id: u64,
        cfg: &RunConfig,
        interpreter: Option<&Interpreter>,
        project_root: &Path,
    ) -> RunSession {
        let (program, mut args, cwd, env) = Self::command_for(cfg, interpreter, project_root);
        if cfg.type_options.as_ref().is_some_and(|spec| {
            spec.kind == rustpy_project::configuration_types::ConfigurationType::DockerCompose
                && spec.compose_owned_project
        }) {
            return compose_lifecycle::spawn(id, cfg, &program, &args, &cwd, &env);
        }
        let prepared = match docker_lifecycle::prepare(cfg, &program, &mut args, &cwd, &env) {
            Ok(prepared) => prepared,
            Err(error) => {
                let mut session = Self::failed_preparation(id, &cfg.name, error);
                session.config = Some(cfg.clone());
                return session;
            }
        };
        let mut session = if cfg.emulate_terminal {
            Self::spawn_pty_command_with_input(
                id,
                &cfg.name,
                &program,
                &args,
                &cwd,
                &env,
                PtyInput {
                    inherit_environment: cfg.workflow.inherit_environment,
                    path: cfg.input_file(project_root).as_deref(),
                },
            )
        } else {
            Self::spawn_command_with_environment(
                id,
                &cfg.name,
                &program,
                &args,
                &cwd,
                &env,
                CommandInput {
                    stdin: config_stdin(cfg, project_root),
                    inherit_environment: cfg.workflow.inherit_environment,
                },
            )
        };
        if let Some(kib) = cfg.console_buffer_kib {
            session.set_capture_limit(kib.saturating_mul(1024));
        }
        session.close_redirected_stdin(cfg, project_root);
        session.config = Some(cfg.clone());
        if let Some(prepared) = prepared {
            if session.status != SessionStatus::Running {
                session.docker_local.finish(session.status.clone());
                session.local_retired.store(true, Ordering::Release);
            }
            session.docker_owner = Some(prepared.start(
                session.local_retired.clone(),
                session.docker_local.clone(),
                session.launched,
                session.tree.clone(),
            ));
            // Even a local launch failure cannot publish completion while a
            // daemon create request from a spawned CLI remains unresolved.
            if session.status != SessionStatus::Running {
                session.pending_exit = Some(std::mem::replace(
                    &mut session.status,
                    SessionStatus::Running,
                ));
                session.finished_at = None;
            }
        }
        session
    }
    fn failed_preparation(id: u64, name: &str, error: String) -> Self {
        let (_, rx) = unbounded();
        let (_, status_rx) = unbounded();
        let mut session = Self {
            id,
            name: name.into(),
            command_line: String::new(),
            started: Instant::now(),
            status: SessionStatus::Failed(error.clone()),
            output: Vec::new(),
            console: ConsoleBuffer::default(),
            output_bytes: 0,
            stdout_revision: 0,
            stderr_revision: 0,
            raw_limit: RAW_OUTPUT_LIMIT,
            input_encoding: Encoding::Utf8,
            pty: None,
            tree: None,
            capture_cancel: Arc::new(AtomicBool::new(false)),
            readers: Arc::new(AtomicUsize::new(0)),
            pending_exit: None,
            docker_owner: None,
            local_retired: Arc::new(AtomicBool::new(true)),
            docker_local: Arc::new(docker_lifecycle::Local::default()),
            launched: false,
            finished_at: Some(Instant::now()),
            config: None,
            child: None,
            rx,
            status_rx,
            stdin: None,
            redirected_input: false,
        };
        session.push_output(OutputChunk {
            kind: OutputKind::System,
            text: format!("{error}\n"),
        });
        session
    }

    /// Spawns an explicit command line as a run session.
    pub fn spawn_command(
        id: u64,
        name: &str,
        program: &Path,
        args: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> RunSession {
        Self::spawn_command_with_stdin(id, name, program, args, cwd, env, None)
    }

    /// [`RunSession::spawn_command`] with the given standard input; `None`
    /// is a pipe that [`RunSession::write_stdin`] writes to.
    pub fn spawn_command_with_stdin(
        id: u64,
        name: &str,
        program: &Path,
        args: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        stdin: Option<Stdio>,
    ) -> RunSession {
        Self::spawn_command_with_environment(
            id,
            name,
            program,
            args,
            cwd,
            env,
            CommandInput {
                stdin,
                inherit_environment: true,
            },
        )
    }
    pub fn spawn_command_with_environment(
        id: u64,
        name: &str,
        program: &Path,
        args: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        input: CommandInput,
    ) -> RunSession {
        Self::spawn_owned(id, name, program, args, cwd, env, input, false)
    }
    /// Starts a Program configuration that launches an external GUI program,
    /// such as a web browser for "Open in browser" and Open URL. Unlike
    /// [`RunSession::spawn`] its standard input is empty, it never uses a
    /// pseudoterminal, and the processes the program hands off to (Chromium's
    /// browser processes after the launcher exits) keep running until they
    /// exit or the session is stopped, which still ends the whole tree.
    pub fn spawn_launcher(id: u64, cfg: &RunConfig, project_root: &Path) -> RunSession {
        let (program, args, cwd, env) = Self::command_for(cfg, None, project_root);
        let mut session = Self::spawn_owned(
            id,
            &cfg.name,
            &program,
            &args,
            &cwd,
            &env,
            CommandInput {
                stdin: Some(Stdio::null()),
                inherit_environment: cfg.workflow.inherit_environment,
            },
            true,
        );
        session.config = Some(cfg.clone());
        session
    }
    #[allow(clippy::too_many_arguments)]
    fn spawn_owned(
        id: u64,
        name: &str,
        program: &Path,
        args: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        input: CommandInput,
        launcher: bool,
    ) -> RunSession {
        let CommandInput {
            stdin,
            inherit_environment,
        } = input;
        let command_line = std::iter::once(program.to_string_lossy().into_owned())
            .chain(args.iter().cloned())
            .map(|a| {
                if a.contains(' ') {
                    format!("\"{a}\"")
                } else {
                    a
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let (tx, rx) = crossbeam_channel::bounded(256);
        let (status_tx, status_rx) = unbounded();
        let mut session = RunSession {
            id,
            name: name.to_string(),
            command_line: command_line.clone(),
            started: Instant::now(),
            status: SessionStatus::Running,
            output: Vec::new(),
            console: ConsoleBuffer::default(),
            output_bytes: 0,
            stdout_revision: 0,
            stderr_revision: 0,
            raw_limit: RAW_OUTPUT_LIMIT,
            input_encoding: Encoding::parse(
                env.get("PYTHONIOENCODING")
                    .map(|s| s.split(':').next().unwrap_or(s))
                    .unwrap_or("utf-8"),
            )
            .unwrap_or(Encoding::Utf8),
            pty: None,
            tree: None,
            capture_cancel: Arc::new(AtomicBool::new(false)),
            readers: Arc::new(AtomicUsize::new(0)),
            pending_exit: None,
            docker_owner: None,
            local_retired: Arc::new(AtomicBool::new(false)),
            docker_local: Arc::new(docker_lifecycle::Local::default()),
            launched: false,
            finished_at: None,
            config: None,
            child: None,
            rx,
            status_rx,
            stdin: None,
            redirected_input: false,
        };
        // A RustPy remote wrapper starts its adapter Python directly.
        let mut cmd = rustpy_project::interpreter::interpreter_command(program);
        if !inherit_environment {
            cmd.env_clear();
        }
        // Output is decoded as UTF-8 unless the caller chose an encoding, so
        // Python must also write UTF-8 (Windows consoles default to a code page).
        let mut env = env.clone();
        env.entry("PYTHONIOENCODING".into())
            .or_insert_with(|| "utf-8".into());
        env.entry("PYTHONUTF8".into()).or_insert_with(|| "1".into());
        cmd.args(args)
            .current_dir(cwd)
            .envs(&env)
            .stdin(stdin.unwrap_or_else(Stdio::piped))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        ProcessTree::prepare(&mut cmd);
        match cmd.spawn() {
            Ok(mut child) => {
                session.launched = true;
                let owned = if launcher {
                    ProcessTree::new_launcher(&mut child)
                } else {
                    ProcessTree::new(&mut child)
                };
                let tree = match owned {
                    Ok(tree) => Arc::new(tree),
                    Err(error) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        session.status =
                            SessionStatus::Failed(format!("Cannot own process tree: {error}"));
                        session.finished_at = Some(Instant::now());
                        session.push_output(OutputChunk {
                            kind: OutputKind::Stderr,
                            text: format!("Cannot own process tree: {error}\n"),
                        });
                        return session;
                    }
                };
                session.tree = Some(tree.clone());
                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                let stdin = child.stdin.take().map(|s| Arc::new(Mutex::new(s)));
                let child = Arc::new(Mutex::new(child));
                session.child = Some(child.clone());
                session.stdin = stdin;
                let _ = tx.send(OutputChunk {
                    kind: OutputKind::System,
                    text: format!("{command_line}\n"),
                });
                if let Some(out) = stdout {
                    spawn_reader_encoding(
                        out,
                        OutputKind::Stdout,
                        tx.clone(),
                        session.input_encoding,
                        session.capture_cancel.clone(),
                        session.readers.clone(),
                    );
                }
                if let Some(err) = stderr {
                    spawn_reader_encoding(
                        docker_lifecycle::DiagnosticReader(err, session.docker_local.clone()),
                        OutputKind::Stderr,
                        tx.clone(),
                        session.input_encoding,
                        session.capture_cancel.clone(),
                        session.readers.clone(),
                    );
                }
                let local_retired = session.local_retired.clone();
                let docker_local = session.docker_local.clone();
                std::thread::spawn(move || {
                    struct Retired(Arc<AtomicBool>);
                    impl Drop for Retired {
                        fn drop(&mut self) {
                            self.0.store(true, Ordering::Release);
                        }
                    }
                    let _retired = Retired(local_retired);
                    let status = loop {
                        let result = child
                            .lock()
                            .map_err(|_| "Owned child lock was poisoned".to_string())
                            .and_then(|mut child| tree.poll(&mut child).map_err(|e| e.to_string()));
                        match result {
                            Ok(Some(status)) => break status,
                            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(30)),
                            Err(error) => {
                                docker_local.finish(SessionStatus::Failed(error.clone()));
                                let _ = status_tx.send(SessionStatus::Failed(error));
                                return;
                            }
                        }
                    };
                    let code = status.code().unwrap_or(-1);
                    #[cfg(unix)]
                    let killed = {
                        use std::os::unix::process::ExitStatusExt;
                        status.signal().is_some()
                    };
                    #[cfg(not(unix))]
                    let killed = false;
                    let status = if killed {
                        SessionStatus::Killed
                    } else {
                        SessionStatus::Exited(code)
                    };
                    docker_local.finish(status.clone());
                    let _ = status_tx.send(status);
                });
            }
            Err(e) => {
                session.status = SessionStatus::Failed(format!(
                    "Cannot run program \"{}\": {e}",
                    program.display()
                ));
                session.finished_at = Some(Instant::now());
                session.push_output(OutputChunk {
                    kind: OutputKind::System,
                    text: format!("{command_line}\n"),
                });
                session.push_output(OutputChunk {
                    kind: OutputKind::Stderr,
                    text: format!("Cannot run program \"{}\": {e}\n", program.display()),
                });
            }
        }
        session
    }

    pub fn output_activity(&self) -> (u64, u64) {
        (self.stdout_revision, self.stderr_revision)
    }
    pub fn lifecycle_stamp(&self) -> String {
        format!(
            "run:{}:{}:readers={}:capture_cancel={}:pending_exit={:?}:docker={}",
            self.id,
            self.tree
                .as_ref()
                .map(|tree| tree.stamp())
                .unwrap_or_else(|| "no-process".into()),
            self.readers.load(Ordering::Acquire),
            self.capture_cancel.load(Ordering::Acquire),
            self.pending_exit,
            self.docker_owner
                .as_ref()
                .map(|owner| owner.stamp())
                .unwrap_or_default()
        )
    }
    pub fn process_id(&self) -> Option<u32> {
        let tree = self.tree.as_ref()?;
        self.pty
            .as_ref()
            .and_then(|pty| {
                pty.child.try_lock().ok().and_then(|mut child| {
                    if matches!(tree.poll_pty(child.as_mut()), Ok(None)) {
                        Some(tree.pid())
                    } else {
                        None
                    }
                })
            })
            .or_else(|| {
                self.child.as_ref().and_then(|child| {
                    child.try_lock().ok().and_then(|mut child| {
                        if matches!(tree.poll(&mut child), Ok(None)) {
                            Some(tree.pid())
                        } else {
                            None
                        }
                    })
                })
            })
    }
    pub fn accepts_stdin(&self) -> bool {
        !self.redirected_input && (self.stdin.is_some() || self.pty.is_some())
    }
    pub fn is_pty(&self) -> bool {
        self.pty.is_some()
    }
    pub fn resize_pty(&self, cols: u16, rows: u16) {
        if let Some(pty) = &self.pty {
            let size = (cols.max(1), rows.max(1));
            if let Ok(mut previous) = pty.size.lock() {
                if *previous != size
                    && pty
                        .master
                        .resize(portable_pty::PtySize {
                            rows: size.1,
                            cols: size.0,
                            pixel_width: 0,
                            pixel_height: 0,
                        })
                        .is_ok()
                {
                    *previous = size;
                }
            }
        }
    }
    pub fn set_capture_limit(&mut self, bytes: usize) {
        self.raw_limit = bytes.clamp(1024, 256 * 1024 * 1024);
        self.console.set_max_bytes(self.raw_limit);
        let mut drop_count = 0;
        while self.output_bytes > self.raw_limit && drop_count < self.output.len() {
            self.output_bytes -= self.output[drop_count].text.len();
            drop_count += 1;
        }
        self.output.drain(..drop_count);
    }
    /// Send an interrupt without blocking rendering. A later explicit Stop can
    /// force termination after the user has seen that the process is still live.
    pub fn interrupt(&mut self) {
        if !self.is_running() {
            return;
        }
        let Some(tree) = &self.tree else {
            return;
        };
        if let Some(pty) = &self.pty {
            tree.mark_interrupt();
            if let Ok(mut writer) = pty.writer.lock() {
                let _ = writer.write_all(b"\x03");
                let _ = writer.flush();
            }
            return;
        }
        if let Some(child) = &self.child {
            if let Ok(mut child) = child.try_lock() {
                if let Err(error) = tree.interrupt(&mut child) {
                    log::warn!("Cannot interrupt run {}: {error}", self.id);
                }
            }
        }
    }
    pub fn spawn_pty_command(
        id: u64,
        name: &str,
        program: &Path,
        args: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> RunSession {
        Self::spawn_pty_command_with_environment(id, name, program, args, cwd, env, true)
    }
    pub fn spawn_pty_command_with_environment(
        id: u64,
        name: &str,
        program: &Path,
        args: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        inherit_environment: bool,
    ) -> RunSession {
        Self::spawn_pty_command_with_input(
            id,
            name,
            program,
            args,
            cwd,
            env,
            PtyInput {
                inherit_environment,
                path: None,
            },
        )
    }
    pub fn spawn_pty_command_with_input(
        id: u64,
        name: &str,
        program: &Path,
        args: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        input: PtyInput<'_>,
    ) -> RunSession {
        let PtyInput {
            inherit_environment,
            path: input,
        } = input;
        let command_line = std::iter::once(program.display().to_string())
            .chain(args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ");
        let (tx, rx) = crossbeam_channel::bounded(256);
        let (status_tx, status_rx) = unbounded();
        let encoding = Encoding::parse(
            env.get("PYTHONIOENCODING")
                .map(|s| s.split(':').next().unwrap_or(s))
                .unwrap_or("utf-8"),
        )
        .unwrap_or(Encoding::Utf8);
        let mut session = Self {
            id,
            name: name.into(),
            command_line: command_line.clone(),
            started: Instant::now(),
            status: SessionStatus::Running,
            output: Vec::new(),
            console: ConsoleBuffer::default(),
            output_bytes: 0,
            stdout_revision: 0,
            stderr_revision: 0,
            raw_limit: RAW_OUTPUT_LIMIT,
            input_encoding: encoding,
            pty: None,
            tree: None,
            capture_cancel: Arc::new(AtomicBool::new(false)),
            readers: Arc::new(AtomicUsize::new(0)),
            pending_exit: None,
            docker_owner: None,
            local_retired: Arc::new(AtomicBool::new(false)),
            docker_local: Arc::new(docker_lifecycle::Local::default()),
            launched: false,
            finished_at: None,
            config: None,
            child: None,
            rx,
            status_rx,
            stdin: None,
            redirected_input: false,
        };
        session.redirected_input = input.is_some();
        let (program, args) = &launch_parts(program, args);
        let (program, args) = (program.as_path(), args.as_slice());
        let result = (|| -> anyhow::Result<PtySession> {
            let redirected = input
                .map(|input| crate::redirect_input::command(program, args, input))
                .transpose()?;
            let (program, args) = redirected
                .as_ref()
                .map(|(program, args)| (program.as_path(), args.as_slice()))
                .unwrap_or((program, args));
            #[cfg(windows)]
            let parts = crate::windows_pty::spawn(program, args, cwd, env, inherit_environment)?;
            #[cfg(not(windows))]
            let parts = {
                let pair = portable_pty::native_pty_system().openpty(portable_pty::PtySize {
                    rows: 24,
                    cols: 120,
                    pixel_width: 0,
                    pixel_height: 0,
                })?;
                let mut command = portable_pty::CommandBuilder::new(program);
                if !inherit_environment {
                    command.env_clear();
                }
                command.args(args);
                command.cwd(cwd);
                command.env("TERM", "xterm-256color");
                command.env("COLORTERM", "truecolor");
                for (key, value) in env {
                    command.env(key, value);
                }
                let mut child = pair.slave.spawn_command(command)?;
                session.launched = true;
                drop(pair.slave);
                let endpoints = (|| -> anyhow::Result<_> {
                    Ok((pair.master.try_clone_reader()?, pair.master.take_writer()?))
                })();
                let (reader, writer) = match endpoints {
                    Ok(parts) => parts,
                    Err(error) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(error);
                    }
                };
                PtyParts {
                    child,
                    reader,
                    writer,
                    master: pair.master,
                }
            };
            let mut child = parts.child;
            session.launched = true;
            let reader = parts.reader;
            let writer = parts.writer;
            let master = parts.master;
            let tree = match ProcessTree::for_pty(child.as_mut()) {
                Ok(tree) => Arc::new(tree),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error.into());
                }
            };
            session.tree = Some(tree.clone());
            let child = Arc::new(Mutex::new(child));
            let worker = child.clone();
            spawn_reader_encoding(
                docker_lifecycle::DiagnosticReader(reader, session.docker_local.clone()),
                OutputKind::Stdout,
                tx.clone(),
                encoding,
                session.capture_cancel.clone(),
                session.readers.clone(),
            );
            let local_retired = session.local_retired.clone();
            let docker_local = session.docker_local.clone();
            std::thread::spawn(move || {
                struct Retired(Arc<AtomicBool>);
                impl Drop for Retired {
                    fn drop(&mut self) {
                        self.0.store(true, Ordering::Release);
                    }
                }
                let _retired = Retired(local_retired);
                loop {
                    let result = worker
                        .lock()
                        .map_err(|_| "Owned PTY child lock was poisoned".to_string())
                        .and_then(|mut child| {
                            let status =
                                tree.poll_pty(child.as_mut()).map_err(|e| e.to_string())?;
                            #[cfg(windows)]
                            if status.is_some() {
                                // The Job barrier has retired the child and its
                                // descendants. Finalize ConPTY on this worker:
                                // try_wait alone leaves HPCON alive, preventing
                                // the concurrently draining reader from EOF.
                                child.wait().map_err(|e| e.to_string())?;
                            }
                            Ok(status)
                        });
                    match result {
                        Ok(Some(status)) => {
                            docker_local.finish(SessionStatus::Exited(status.exit_code() as i32));
                            let _ =
                                status_tx.send(SessionStatus::Exited(status.exit_code() as i32));
                            break;
                        }
                        Ok(None) => {}
                        Err(error) => {
                            docker_local.finish(SessionStatus::Failed(error.clone()));
                            let _ = status_tx.send(SessionStatus::Failed(error));
                            break;
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(30));
                }
            });
            Ok(PtySession {
                child,
                writer: Mutex::new(writer),
                master,
                size: Mutex::new((120, 24)),
            })
        })();
        match result {
            Ok(pty) => {
                session.pty = Some(pty);
                session.push_output(OutputChunk {
                    kind: OutputKind::System,
                    text: format!("{command_line}\n"),
                });
            }
            Err(error) => {
                session.status = SessionStatus::Failed(error.to_string());
                session.finished_at = Some(Instant::now());
                session.push_output(OutputChunk {
                    kind: OutputKind::Stderr,
                    text: format!("Cannot start PTY: {error}\n"),
                });
            }
        }
        session
    }

    /// Records a chunk of output: in the console and in the raw log, whose
    /// oldest chunks go once it exceeds [`RAW_OUTPUT_LIMIT`].
    fn push_output(&mut self, mut chunk: OutputChunk) {
        // Windows text-mode children write CRLF; the console keeps one line ending.
        if cfg!(windows) && chunk.kind != OutputKind::System && chunk.text.contains("\r\n") {
            chunk.text = chunk.text.replace("\r\n", "\n");
        }
        match chunk.kind {
            OutputKind::Stdout => self.stdout_revision = self.stdout_revision.wrapping_add(1),
            OutputKind::Stderr => self.stderr_revision = self.stderr_revision.wrapping_add(1),
            OutputKind::System => {}
        }
        self.console.push(chunk.kind, &chunk.text);
        if chunk.text.len() > self.raw_limit {
            let mut cut = chunk.text.len() - self.raw_limit;
            while !chunk.text.is_char_boundary(cut) {
                cut += 1;
            }
            chunk.text.drain(..cut);
        }
        self.output_bytes += chunk.text.len();
        self.output.push(chunk);
        if self.output_bytes > self.raw_limit {
            // Drop down to 3/4 of the limit so the shift is amortized.
            let mut n = 0;
            while self.output_bytes > self.raw_limit / 4 * 3 && n + 1 < self.output.len() {
                self.output_bytes -= self.output[n].text.len();
                n += 1;
            }
            self.output.drain(..n);
        }
    }

    /// Clears the console and the raw output ("Clear All").
    pub fn clear_output(&mut self) {
        self.output.clear();
        self.output_bytes = 0;
        self.console.clear();
    }

    /// Drains new output; returns true if anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        for _ in 0..128 {
            match self.rx.try_recv() {
                Ok(chunk) => {
                    self.push_output(chunk);
                    changed = true;
                }
                Err(_) => break,
            }
        }
        if self.status == SessionStatus::Running {
            if self.pending_exit.is_none() {
                if let Ok(status) = self.status_rx.try_recv() {
                    self.pending_exit = Some(status);
                }
            }
            if let Some(owner) = &self.docker_owner {
                if owner.unresolved() && !owner.retired() {
                    let error = format!(
                        "Docker retirement unresolved; background ownership retained: {}",
                        owner.stamp()
                    );
                    self.status = SessionStatus::Failed(error.clone());
                    self.finished_at = Some(Instant::now());
                    self.push_output(OutputChunk {
                        kind: OutputKind::System,
                        text: format!("\n{error}\n"),
                    });
                    return true;
                }
            }
            if self.pending_exit.is_some()
                && self
                    .docker_owner
                    .as_ref()
                    .is_none_or(|owner| owner.retired())
                && self.readers.load(Ordering::Acquire) == 0
                && self.rx.is_empty()
            {
                let status = self.pending_exit.take().unwrap();
                let message = match &status {
                    SessionStatus::Exited(code) => {
                        format!("\nProcess finished with exit code {code}\n")
                    }
                    SessionStatus::Killed => "\nProcess terminated (SIGKILL)\n".into(),
                    SessionStatus::Failed(error) => format!("\n{error}\n"),
                    SessionStatus::Running => String::new(),
                };
                self.push_output(OutputChunk {
                    kind: OutputKind::System,
                    text: message,
                });
                self.status = status;
                self.finished_at = Some(Instant::now());
                changed = true;
            }
        }
        changed
    }
    pub fn is_running(&self) -> bool {
        self.status == SessionStatus::Running
    }

    /// Physical ownership can outlive a displayed failure. In particular a
    /// Docker transport failure must retain its console and shutdown barrier
    /// until daemon reconciliation and local process retirement both finish.
    pub fn owns_process(&self) -> bool {
        self.is_running() || self.has_pending_daemon_retirement()
    }

    pub fn has_pending_daemon_retirement(&self) -> bool {
        self.docker_owner.as_ref().is_some_and(|owner| {
            !owner.retired()
                || !self.local_retired.load(Ordering::Acquire)
                || self.readers.load(Ordering::Acquire) != 0
        })
    }

    /// Requests forced process-tree termination without waiting on the UI thread.
    pub fn stop(&mut self) {
        // A forced close must unblock a reader waiting on a full UI channel;
        // it keeps draining/discarding until the owned pipes reach EOF.
        self.capture_cancel.store(true, Ordering::Release);
        if let Some(owner) = &self.docker_owner {
            // Keep docker run alive until the daemon identity is resolved.
            // The background owner removes that exact container before it
            // requests local process-tree termination.
            owner.stop();
            return;
        }
        let Some(tree) = &self.tree else {
            return;
        };
        // Both pipe and PTY spawn paths retain an Arc owner plus child in
        // their background waiter until poll observes physical retirement.
        // UI Stop/Drop must neither acquire that mutex nor perform cleanup here.
        tree.request_force();
    }
    /// A redirected input file never accepts console input. If opening it
    /// failed, closing the fallback pipe delivers EOF without depending on NUL.
    pub(crate) fn close_redirected_stdin(&mut self, cfg: &RunConfig, project_root: &Path) {
        if cfg.input_file(project_root).is_some() {
            self.redirected_input = true;
            self.stdin.take();
        }
    }

    /// Writes to the process's stdin.
    pub fn write_stdin(&mut self, text: &str) {
        if self.redirected_input {
            return;
        }
        if let Some(stdin) = &self.stdin {
            if let Ok(mut s) = stdin.lock() {
                let _ = s.write_all(&self.input_encoding.encode(text));
                let _ = s.flush();
            }
        }
        if let Some(pty) = &self.pty {
            if let Ok(mut writer) = pty.writer.lock() {
                let _ = writer.write_all(&self.input_encoding.encode(text));
                let _ = writer.flush();
            }
            return;
        }
        self.push_output(OutputChunk {
            kind: OutputKind::System,
            text: text.to_string(),
        });
    }

    pub fn elapsed(&self) -> std::time::Duration {
        self.finished_at
            .unwrap_or_else(Instant::now)
            .duration_since(self.started)
    }

    /// Whole output as plain text.
    pub fn text(&self) -> String {
        self.output.iter().map(|c| c.text.as_str()).collect()
    }
}

/// The standard input of a configuration's process (run, debug, coverage,
/// profile): its "Redirect input from" file, or `None` for the default. A
/// file that cannot be opened gives a pipe whose parent writer the launcher
/// must close immediately after spawning, delivering EOF. Launchers report
/// [`RunConfig::input_problem`] instead of starting when possible.
pub fn config_stdin(cfg: &RunConfig, project_root: &Path) -> Option<Stdio> {
    let path = cfg.input_file(project_root)?;
    Some(match std::fs::File::open(&path) {
        Ok(f) => Stdio::from(f),
        Err(e) => {
            log::warn!(
                "{}: cannot redirect input from {}: {e}",
                cfg.name,
                path.display()
            );
            Stdio::piped()
        }
    })
}

/// A bare program name (`black`) looked up on `PATH` the way the shell would,
/// including `PATHEXT` on Windows (`black.cmd`); paths and unknown names are
/// returned as they are, so the spawn error names what the user typed.
/// Program and arguments that start `program` with `args`. On Windows a
/// RustPy remote-interpreter wrapper (`remote-<key>.cmd`) becomes its adapter
/// Python plus `-I <adapter.py>`, so no argument is re-parsed by cmd.
pub fn launch_parts(program: &Path, args: &[String]) -> (PathBuf, Vec<String>) {
    let (launcher, leading) = rustpy_project::interpreter::interpreter_invocation(program);
    if leading.is_empty() {
        return (launcher, args.to_vec());
    }
    let mut all: Vec<String> = leading
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    all.extend_from_slice(args);
    (launcher, all)
}

pub fn resolve_program(program: &Path) -> PathBuf {
    if program.components().count() == 1 && !program.is_absolute() {
        if let Ok(found) = which::which(program) {
            return found;
        }
    }
    program.to_path_buf()
}

/// Forwards a pipe's output as soon as it arrives, not only at line ends, so
/// prompts and `\r` progress bars show while they are written.
fn spawn_reader_encoding<R: Read + Send + 'static>(
    mut reader: R,
    kind: OutputKind,
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
        let mut buffer = [0; 8192];
        let mut decoder = Decoder::new(encoding);
        let mut capture = true;
        let send = |text: String, capture: &mut bool| {
            if text.is_empty() || !*capture {
                return;
            }
            let mut chunk = OutputChunk { kind, text };
            loop {
                if cancel.load(Ordering::Acquire) {
                    *capture = false;
                    return;
                }
                match tx.send_timeout(chunk, std::time::Duration::from_millis(25)) {
                    Ok(()) => return,
                    Err(crossbeam_channel::SendTimeoutError::Timeout(pending)) => chunk = pending,
                    Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => {
                        *capture = false;
                        return;
                    }
                }
            }
        };
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    if capture {
                        send(decoder.push(&buffer[..count], false), &mut capture);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        send(decoder.push(&[], true), &mut capture);
    });
}

/// Decodes `bytes` up to an incomplete UTF-8 sequence at the end, which stays
/// in `bytes` for the next read; invalid bytes become U+FFFD. For readers that
/// forward output as it arrives (here and in the debugger).
pub fn take_utf8_prefix(bytes: &mut Vec<u8>) -> String {
    let keep = incomplete_utf8_tail(bytes);
    let rest = bytes.split_off(bytes.len() - keep);
    let text = String::from_utf8_lossy(bytes).into_owned();
    *bytes = rest;
    text
}

/// Length of a UTF-8 sequence cut off at the end of `b` (0 if none).
fn incomplete_utf8_tail(b: &[u8]) -> usize {
    for back in 1..=b.len().min(3) {
        let byte = b[b.len() - back];
        if byte & 0b1100_0000 == 0b1000_0000 {
            continue; // continuation byte: look further back for the lead byte
        }
        let need = match byte {
            0xF0..=0xF7 => 4,
            0xE0..=0xEF => 3,
            0xC0..=0xDF => 2,
            _ => 1,
        };
        return if need > back { back } else { 0 };
    }
    0
}

static TRACEBACK_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"File "([^"]+)", line (\d+)"#).unwrap());
static PATH_LINE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?:^|[\s(])((?:[A-Za-z]:)?[^\s:"()]+\.py):(\d+)"#).unwrap());

/// A clickable file link inside an output line: `(byte range, path, line)`.
pub fn file_links(line: &str) -> Vec<(std::ops::Range<usize>, PathBuf, usize)> {
    let mut out = Vec::new();
    for cap in TRACEBACK_RE.captures_iter(line) {
        let m = cap.get(0).unwrap();
        let path = PathBuf::from(&cap[1]);
        let line_no: usize = cap[2].parse().unwrap_or(1);
        out.push((m.range(), path, line_no));
    }
    if out.is_empty() {
        for cap in PATH_LINE_RE.captures_iter(line) {
            let m = cap.get(1).unwrap();
            let end = cap.get(2).unwrap().end();
            out.push((
                m.start()..end,
                PathBuf::from(&cap[1]),
                cap[2].parse().unwrap_or(1),
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_runs_without_a_shell() {
        let Ok(py) = which::which("python3") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        // Spaces, quotes and shell metacharacters reach the program unchanged.
        let args: Vec<String> = [
            "-c",
            "import sys; print(sys.argv[1:])",
            "/My Projects/a b.py",
            "$HOME;echo x|y",
            "it's",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let cfg = RunConfig {
            name: "tool".into(),
            kind: RunConfigKind::Program {
                program: py,
                args: args.clone(),
            },
            working_dir: Some(dir.path().to_path_buf()),
            ..Default::default()
        };
        let (_, built, cwd, _) = RunSession::command_for(&cfg, None, dir.path());
        assert_eq!(built, args);
        assert_eq!(cwd, dir.path());
        let mut s = RunSession::spawn(1, &cfg, None, dir.path());
        let start = Instant::now();
        while s.is_running() && start.elapsed().as_secs() < 20 {
            s.poll();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        s.poll();
        let text = s.text();
        assert!(
            text.contains(r#"['/My Projects/a b.py', '$HOME;echo x|y', "it's"]"#),
            "{text}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn launcher_session_keeps_hand_off_until_stop_and_run_session_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = RunConfig {
            name: "Browser launcher: hand-off".into(),
            kind: RunConfigKind::Program {
                program: PathBuf::from("/bin/sh"),
                args: vec![
                    "-c".into(),
                    "sleep 30 </dev/null >/dev/null 2>&1 & echo launched".into(),
                ],
            },
            working_dir: Some(dir.path().to_path_buf()),
            temporary: true,
            ..Default::default()
        };
        // A Program run configuration ends its tree with the leader.
        let mut plain = RunSession::spawn(1, &cfg, None, dir.path());
        let start = Instant::now();
        while plain.is_running() && start.elapsed().as_secs() < 20 {
            plain.poll();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!plain.is_running(), "{}", plain.text());
        // A launcher's hand-off (the real browser) outlives the launcher.
        let mut launcher = RunSession::spawn_launcher(2, &cfg, dir.path());
        while !launcher.text().contains("launched") && start.elapsed().as_secs() < 20 {
            launcher.poll();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        std::thread::sleep(std::time::Duration::from_millis(300));
        launcher.poll();
        assert!(launcher.is_running(), "{}", launcher.text());
        launcher.stop();
        while launcher.is_running() && start.elapsed().as_secs() < 30 {
            launcher.poll();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!launcher.is_running(), "Stop must end the launcher's tree");
    }

    #[test]
    fn bare_program_names_resolve_on_path() {
        if which::which("python3").is_err() {
            return;
        }
        let p = resolve_program(Path::new("python3"));
        assert!(p.is_absolute(), "{p:?}");
        // Paths and names that are not found stay as written.
        assert_eq!(
            resolve_program(Path::new("./tool.sh")),
            PathBuf::from("./tool.sh")
        );
        assert_eq!(
            resolve_program(Path::new("no-such-program-xyz")),
            PathBuf::from("no-such-program-xyz")
        );
    }

    #[test]
    fn runs_python_and_captures_output() {
        let Ok(py) = which::which("python3") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("s.py");
        std::fs::write(
            &script,
            "import sys\nprint('hello')\nprint('err', file=sys.stderr)\nsys.exit(3)\n",
        )
        .unwrap();
        let mut cfg = RunConfig::for_script(&script);
        cfg.interpreter = Some(py);
        let mut s = RunSession::spawn(1, &cfg, None, dir.path());
        let start = Instant::now();
        while s.is_running() && start.elapsed().as_secs() < 20 {
            s.poll();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        s.poll();
        assert_eq!(s.status, SessionStatus::Exited(3));
        // stdout and stderr arrive as separate chunks whose interleaving in the
        // merged text depends on scheduling; check each stream on its own.
        let stream = |kind: OutputKind| {
            s.output
                .iter()
                .filter(|c| c.kind == kind)
                .map(|c| c.text.as_str())
                .collect::<String>()
        };
        assert!(
            stream(OutputKind::Stdout).contains("hello\n"),
            "{}",
            s.text()
        );
        assert!(stream(OutputKind::Stderr).contains("err"), "{}", s.text());
        assert!(s.text().contains("exit code 3"));
    }

    #[cfg(unix)]
    #[test]
    fn stop_does_not_signal_the_group_of_a_finished_process() {
        let mut session = RunSession::spawn_command(
            91,
            "finished parent",
            Path::new("sh"),
            &["-c".into(), "sleep 30 & echo $!; exit 23".into()],
            Path::new("/"),
            &Default::default(),
        );
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while session.is_running() && Instant::now() < deadline {
            session.poll();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        session.poll();
        let pid = session
            .output
            .iter()
            .filter(|chunk| chunk.kind == OutputKind::Stdout)
            .find_map(|chunk| chunk.text.trim().parse::<u32>().ok())
            .expect("child PID");
        // Natural exit now deliberately cleans up the owned descendant tree
        // before reaping the leader. The former child-survival expectation
        // contradicted that ownership contract. Stop after retirement must
        // preserve an unrelated process/group, rather than keep an orphan alive.
        use std::os::unix::process::CommandExt;
        let mut sentinel = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        session.stop();
        let alive = sentinel.try_wait().unwrap().is_none();
        let _ = sentinel.kill();
        let _ = sentinel.wait();
        // The descendant's PID was still collected from the actual run output.
        assert_ne!(pid, sentinel.id());
        assert_eq!(session.status, SessionStatus::Exited(23));
        assert!(
            alive,
            "stopping a retired session must not signal an unrelated process group"
        );
    }

    #[cfg(unix)]
    #[test]
    fn stop_returns_without_waiting_for_the_background_child_lock() {
        let mut session = RunSession::spawn_command(
            92,
            "held waiter",
            Path::new("/bin/sh"),
            &["-c".into(), "while :; do sleep 1; done".into()],
            Path::new("/"),
            &BTreeMap::new(),
        );
        let child = session.child.as_ref().expect("owned child").clone();
        let guard = child.lock().unwrap();
        let (sent, received) = std::sync::mpsc::channel();
        let stopper = std::thread::spawn(move || {
            session.stop();
            let _ = sent.send(session);
        });
        let requested = received.recv_timeout(std::time::Duration::from_secs(2));
        let returned_without_lock = requested.is_ok();
        // Always release the artificial barrier before assertions, including
        // the failure path, so the real waiter can retire its process tree.
        drop(guard);
        let mut session = requested.unwrap_or_else(|_| {
            received
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap()
        });
        stopper.join().unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while session.is_running() && Instant::now() < deadline {
            session.poll();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            !session.is_running(),
            "background waiter must complete retirement"
        );
        assert!(
            returned_without_lock,
            "Stop must not acquire the waiter's child mutex"
        );
    }

    #[test]
    fn stops_running_process() {
        let Ok(py) = which::which("python3") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("loop.py");
        std::fs::write(&script, "import time\nwhile True:\n    time.sleep(0.1)\n").unwrap();
        let mut cfg = RunConfig::for_script(&script);
        cfg.interpreter = Some(py);
        let mut s = RunSession::spawn(2, &cfg, None, dir.path());
        std::thread::sleep(std::time::Duration::from_millis(300));
        s.stop();
        let start = Instant::now();
        while s.is_running() && start.elapsed().as_secs() < 10 {
            s.poll();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!s.is_running());
    }

    fn wait_for_exit(s: &mut RunSession) {
        let start = Instant::now();
        while s.is_running() && start.elapsed().as_secs() < 20 {
            s.poll();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        s.poll();
    }

    #[test]
    fn redirects_input_from_a_file() {
        let Ok(py) = which::which("python3") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("s.py"),
            "import sys\nprint('got:' + sys.stdin.read().strip())\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("input.txt"), "from the file\n").unwrap();
        let mut cfg = RunConfig::for_script(&dir.path().join("s.py"));
        cfg.interpreter = Some(py);
        cfg.redirect_input = Some(PathBuf::from("input.txt"));
        assert_eq!(cfg.input_problem(dir.path()), None);
        let mut s = RunSession::spawn(3, &cfg, None, dir.path());
        wait_for_exit(&mut s);
        assert!(s.text().contains("got:from the file"), "{}", s.text());
        // A missing file is reported before launching; if it disappears in
        // between, the process reads an empty input instead of waiting forever.
        cfg.redirect_input = Some(PathBuf::from("missing.txt"));
        assert!(cfg
            .input_problem(dir.path())
            .is_some_and(|p| p.contains("missing.txt")));
        let mut s = RunSession::spawn(4, &cfg, None, dir.path());
        wait_for_exit(&mut s);
        assert_eq!(s.status, SessionStatus::Exited(0), "{}", s.text());
        assert!(
            s.stdin.is_none(),
            "missing redirected input must close its writer"
        );
        assert!(s.text().contains("got:\n"), "{}", s.text());
    }

    #[test]
    fn disappearing_redirect_delivers_eof_and_default_input_stays_interactive() {
        let py = which::which("python3").expect("Python fixture interpreter");
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("input.py");
        std::fs::write(
            &script,
            "import sys\nprint('got:' + sys.stdin.readline().strip())\n",
        )
        .unwrap();
        // An owned regular 'null' file is deliberately nonempty. A redirect
        // reads these exact bytes; its disappearance must instead deliver EOF.
        let input = dir.path().join("null");
        std::fs::write(&input, "pip fixture output\n").unwrap();
        let mut cfg = RunConfig::for_script(&script);
        cfg.interpreter = Some(py);
        cfg.redirect_input = Some(input.clone());
        let stdout = |session: &RunSession| {
            session
                .output
                .iter()
                .filter(|chunk| chunk.kind == OutputKind::Stdout)
                .map(|chunk| chunk.text.as_str())
                .collect::<String>()
        };
        let mut session = RunSession::spawn(5, &cfg, None, dir.path());
        wait_for_exit(&mut session);
        assert_eq!(session.status, SessionStatus::Exited(0));
        assert_eq!(stdout(&session), "got:pip fixture output\n");
        std::fs::remove_file(input).unwrap();
        let mut session = RunSession::spawn(6, &cfg, None, dir.path());
        assert!(session.stdin.is_none());
        wait_for_exit(&mut session);
        assert_eq!(session.status, SessionStatus::Exited(0));
        assert_eq!(stdout(&session), "got:\n");
        for redirect in [None, Some(PathBuf::new())] {
            cfg.redirect_input = redirect;
            let mut session = RunSession::spawn(7, &cfg, None, dir.path());
            assert!(
                session.stdin.is_some(),
                "default input must stay interactive"
            );
            session.write_stdin("interactive answer\n");
            wait_for_exit(&mut session);
            assert_eq!(session.status, SessionStatus::Exited(0));
            assert_eq!(stdout(&session), "got:interactive answer\n");
        }
    }

    /// Debug, Coverage and Profile run unittest targets as processes: a
    /// directory target runs its tests, as a plain directory or a package
    /// (`python -m unittest tests` used to report "Ran 0 tests").
    #[test]
    fn unittest_directory_target_runs_its_tests() {
        let Ok(py) = which::which("python3") else {
            return;
        };
        for package in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let root = dunce::canonicalize(dir.path()).unwrap();
            std::fs::create_dir_all(root.join("tests")).unwrap();
            if package {
                std::fs::write(root.join("tests/__init__.py"), "").unwrap();
                std::fs::write(root.join("tests/helpers.py"), "VALUE = 1\n").unwrap();
            }
            let import = if package {
                "from tests.helpers import VALUE"
            } else {
                "VALUE = 1"
            };
            std::fs::write(root.join("tests/test_a.py"), format!("import unittest\n{import}\n\n\nclass A(unittest.TestCase):\n    def test_one(self):\n        self.assertEqual(VALUE, 1)\n\n    def test_two(self):\n        pass\n")).unwrap();
            let cfg = RunConfig {
                name: "u".into(),
                kind: RunConfigKind::Unittest {
                    target: "tests".into(),
                },
                interpreter: Some(py.clone()),
                ..Default::default()
            };
            let mut s = RunSession::spawn(10, &cfg, None, &root);
            wait_for_exit(&mut s);
            assert_eq!(
                s.status,
                SessionStatus::Exited(0),
                "package={package}: {}",
                s.text()
            );
            assert!(
                s.text().contains("Ran 2 tests"),
                "package={package}: {}",
                s.text()
            );
        }
    }

    #[test]
    fn actual_command_receives_quoted_env_values_and_never_invalid_binding() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".env"),
            "\u{feff}'TOKEN'='it\\'s'\nBAD=\"lost\"suffix\nUNICODE=λ🙂\n",
        )
        .unwrap();
        let cfg = RunConfig {
            env_files: vec![PathBuf::from(".env")],
            ..Default::default()
        };
        let (_, _, _, env) = RunSession::command_for(&cfg, None, dir.path());
        assert_eq!(env.get("TOKEN").map(String::as_str), Some("it's"));
        assert_eq!(env.get("UNICODE").map(String::as_str), Some("λ🙂"));
        assert!(!env.contains_key("BAD"));
    }

    #[test]
    fn command_uses_env_files_and_resolves_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join(".env"), "TOKEN=secret\nPYTHONUNBUFFERED=0\n").unwrap();
        let mut cfg = RunConfig {
            name: "t".into(),
            kind: RunConfigKind::Unittest {
                target: "tests/test_x.py::Case::test_a".into(),
            },
            working_dir: Some(PathBuf::from("sub")),
            env_files: vec![PathBuf::from(".env")],
            ..Default::default()
        };
        cfg.env.insert("EXTRA".into(), "1".into());
        let (_, args, cwd, env) = RunSession::command_for(&cfg, None, root);
        assert_eq!(cwd, root.join("sub"));
        assert_eq!(args, vec!["-m", "unittest", "tests.test_x.Case.test_a"]);
        assert_eq!(env.get("TOKEN").map(String::as_str), Some("secret"));
        assert_eq!(env.get("EXTRA").map(String::as_str), Some("1"));
        assert_eq!(
            env.get("PYTHONUNBUFFERED").map(String::as_str),
            Some("0"),
            "the configuration's environment wins over defaults"
        );
        // pytest node ids stay valid from another working directory.
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("tests/test_x.py"), "").unwrap();
        cfg.kind = RunConfigKind::Pytest {
            target: "tests/test_x.py::test_a".into(),
            keywords: String::new(),
        };
        let (_, args, _, _) = RunSession::command_for(&cfg, None, root);
        assert_eq!(
            args[2],
            root.join("tests/test_x.py::test_a").to_string_lossy()
        );
        cfg.working_dir = None;
        let (_, args, cwd, _) = RunSession::command_for(&cfg, None, root);
        assert_eq!(
            (args[2].as_str(), cwd.as_path()),
            ("tests/test_x.py::test_a", root)
        );
    }

    #[test]
    fn utf8_split_between_reads_is_kept_whole() {
        let mut b = "aé".as_bytes().to_vec();
        let tail = b.pop().unwrap(); // second byte of `é`
        assert_eq!(take_utf8_prefix(&mut b), "a");
        assert_eq!(b.len(), 1, "the lead byte waits for the rest");
        b.push(tail);
        b.extend_from_slice("€".as_bytes());
        assert_eq!(take_utf8_prefix(&mut b), "é€");
        assert!(b.is_empty());
        let mut euro = "€".as_bytes()[..2].to_vec();
        assert_eq!(take_utf8_prefix(&mut euro), "");
        assert_eq!(euro.len(), 2);
        let mut bad = vec![b'x', 0xFF];
        assert_eq!(take_utf8_prefix(&mut bad), "x\u{FFFD}");
    }

    #[test]
    fn streams_output_before_the_line_ends() {
        let Ok(py) = which::which("python3") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("progress.py");
        std::fs::write(&script, "import sys, time\nsys.stdout.write('step 1/2')\nsys.stdout.flush()\ntime.sleep(2)\nsys.stdout.write('\\rstep 2/2 done\\n')\n").unwrap();
        let mut cfg = RunConfig::for_script(&script);
        cfg.interpreter = Some(py);
        let mut s = RunSession::spawn(3, &cfg, None, dir.path());
        let start = Instant::now();
        while !s.console.text().contains("step 1/2") && start.elapsed().as_secs() < 10 {
            s.poll();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            s.is_running(),
            "the partial line arrived while the process still ran"
        );
        assert!(
            s.console.text().contains("step 1/2"),
            "{}",
            s.console.text()
        );
        while s.is_running() && start.elapsed().as_secs() < 20 {
            s.poll();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let lines: Vec<String> = s
            .console
            .lines_from(0)
            .map(|(_, l)| l.text.clone())
            .collect();
        assert!(
            lines.contains(&"step 2/2 done".to_string()),
            "carriage return replaced the line: {lines:?}"
        );
        assert!(!lines.iter().any(|l| l.contains("step 1/2")), "{lines:?}");
    }

    #[test]
    fn raw_output_is_bounded_and_clear_resets_the_console() {
        let mut s = RunSession::spawn_command(
            4,
            "missing",
            Path::new("/nonexistent/rustpy-test-program"),
            &[],
            Path::new("."),
            &BTreeMap::new(),
        );
        assert!(matches!(s.status, SessionStatus::Failed(_)));
        let line = "x".repeat(1023) + "\n";
        for _ in 0..(RAW_OUTPUT_LIMIT / 1024 + 100) {
            s.push_output(OutputChunk {
                kind: OutputKind::Stdout,
                text: line.clone(),
            });
        }
        let raw: usize = s.output.iter().map(|c| c.text.len()).sum();
        assert!(
            raw <= RAW_OUTPUT_LIMIT && raw == s.output_bytes,
            "raw output kept: {raw}"
        );
        assert!(
            s.console.len() > raw / 1024,
            "the console has its own (line) limit"
        );
        s.clear_output();
        assert!(s.output.is_empty() && s.console.is_empty() && s.output_bytes == 0);
    }

    #[test]
    fn finds_links() {
        let l = file_links("  File \"/tmp/x/main.py\", line 12, in <module>");
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].1, PathBuf::from("/tmp/x/main.py"));
        assert_eq!(l[0].2, 12);
        let l = file_links("tests/test_a.py:7: AssertionError");
        assert_eq!(l[0].2, 7);
    }
    #[test]
    fn console_encoding_and_environment_inheritance_are_command_policy() {
        let mut config = RunConfig::for_script(Path::new("/project/main.py"));
        config.console_encoding = "latin-1".into();
        config.interpreter_options = "-X utf8".into();
        config.workflow.inherit_environment = false;
        let (_, args, _, env) = RunSession::command_for(&config, None, Path::new("/project"));
        assert_eq!(&args[..2], &["-X", "utf8"]);
        assert_eq!(
            env.get("PYTHONIOENCODING").map(String::as_str),
            Some("latin-1")
        );
        assert_eq!(env.get("PYTHONPATH").map(String::as_str), Some("/project"));
    }
    #[cfg(target_os = "linux")]
    fn child_pid(session: &mut RunSession) -> u32 {
        let deadline = Instant::now() + std::time::Duration::from_secs(3);
        while Instant::now() < deadline {
            session.poll();
            if let Some(pid) = session
                .output
                .iter()
                .filter(|chunk| chunk.kind == OutputKind::Stdout)
                .flat_map(|chunk| chunk.text.lines())
                .find_map(|line| line.trim().parse().ok())
            {
                return pid;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("child PID was not reported");
    }
    #[cfg(target_os = "linux")]
    fn live_process(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| {
                stat.rsplit_once(") ")
                    .map(|(_, tail)| !tail.starts_with('Z'))
            })
            .unwrap_or(false)
    }
    #[cfg(target_os = "linux")]
    fn assert_not_live(pid: u32) {
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        while live_process(pid) && Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!live_process(pid), "owned process {pid} survived cleanup");
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn dropping_session_kills_descendants_and_reaps_the_group_leader() {
        let mut session = RunSession::spawn_command(
            900,
            "owned group",
            Path::new("/bin/sh"),
            &["-c".into(), "sleep 30 & printf '%s\n' $!; wait".into()],
            Path::new("/"),
            &BTreeMap::new(),
        );
        let child = child_pid(&mut session);
        let leader = session.process_id().unwrap();
        let stamp = session.lifecycle_stamp();
        session
            .console
            .push(OutputKind::Stdout, "presentation only");
        assert_eq!(session.lifecycle_stamp(), stamp);
        drop(session);
        assert_not_live(child);
        assert_not_live(leader);
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn parent_exit_cleans_inherited_helpers_before_publishing_exit() {
        let mut session = RunSession::spawn_command(
            901,
            "short parent",
            Path::new("/bin/sh"),
            &["-c".into(), "sleep 30 & printf '%s\n' $!; exit 0".into()],
            Path::new("/"),
            &BTreeMap::new(),
        );
        let child = child_pid(&mut session);
        let deadline = Instant::now() + std::time::Duration::from_secs(3);
        while session.is_running() && Instant::now() < deadline {
            session.poll();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(session.status, SessionStatus::Exited(0));
        assert_not_live(child);
        assert!(session
            .console
            .text()
            .contains("Process finished with exit code 0"));
    }
    #[cfg(unix)]
    #[test]
    fn pty_capture_accepts_input_and_keeps_an_owned_lifecycle() {
        let mut session = RunSession::spawn_pty_command(
            902,
            "interactive",
            Path::new("/bin/sh"),
            &[
                "-c".into(),
                "read value; printf 'received:%s\n' \"$value\"".into(),
            ],
            Path::new("/"),
            &BTreeMap::new(),
        );
        assert!(session.is_pty());
        assert!(session.lifecycle_stamp().contains("pty=true"));
        session.write_stdin("hello\n");
        let deadline = Instant::now() + std::time::Duration::from_secs(3);
        while session.is_running() && Instant::now() < deadline {
            session.poll();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(session.console.text().contains("received:hello"));
        assert!(!session.is_running());
    }
}
