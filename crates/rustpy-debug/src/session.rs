//! A debug session: launches the debuggee under debugpy and drives the DAP
//! state machine (breakpoints, stepping, threads, frames, variables, watches,
//! the debug console).

use crate::port::DebugPortLease;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};
use rustpy_project::{Interpreter, RunConfig};
use rustpy_run::{OutputChunk, OutputKind};
use serde_json::{json, Value};

use crate::dap::{DapClient, Message};
#[path = "session/javascript_children.rs"]
mod javascript_children;
pub use crate::model::{
    apply_completion, breakpoint_rejection, completion_position, dap_breakpoints, debug_command,
    identifier_start, log_expression, Breakpoint, ColumnEncoding, CompletionItem, Completions,
    DebugOptions, ExceptionBreakpoints, ExceptionClassBreakpoint, StepInTarget,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DebugState {
    Starting,
    Running,
    Stopped {
        thread_id: i64,
        reason: String,
        description: String,
    },
    Terminated(Option<i32>),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StackFrame {
    pub id: i64,
    pub name: String,
    pub path: Option<PathBuf>,
    pub line: usize,
    /// One-based wire column, in [`DebugSession::column_encoding`] units.
    pub column: usize,
    /// Library / stdlib frame (deemphasized).
    pub is_library: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub name: String,
    pub variables_reference: i64,
    pub expensive: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    pub value: String,
    pub type_name: String,
    pub variables_reference: i64,
    pub evaluate_name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Watch {
    pub expression: String,
    pub result: Option<Variable>,
    pub error: Option<String>,
}

/// An entry of the debug console's evaluation log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplEntry {
    /// Code typed in the console.
    Input(String),
    /// Its result; `variables_reference != 0` can be expanded while suspended.
    Value(Variable),
    Error(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplItem {
    /// Number of `output` chunks printed before this entry: orders the console
    /// chronologically with program output.
    pub at: usize,
    /// The suspension ([`DebugSession::stop_epoch`]) it belongs to.
    pub epoch: u64,
    pub entry: ReplEntry,
}

/// Result of an evaluation for the editor hover or the Evaluate dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvalResult {
    pub expression: String,
    pub result: Result<Variable, String>,
    /// The suspension it belongs to: child references are only valid then.
    pub epoch: u64,
}

/// What a session starts with.
#[derive(Clone, Debug, Default)]
pub struct LaunchSetup {
    pub breakpoints: HashMap<PathBuf, Vec<Breakpoint>>,
    pub exceptions: ExceptionBreakpoints,
    pub muted: bool,
    pub options: DebugOptions,
}

#[derive(Debug)]
enum Pending {
    BackendCommand {
        kind: String,
        epoch: u64,
    },
    Initialize,
    Attach,
    /// The lines sent, in order.
    SetBreakpoints(PathBuf, Vec<usize>),
    SetExceptionBreakpoints,
    ConfigurationDone,
    Threads,
    StackTrace {
        thread: i64,
        epoch: u64,
    },
    Scopes {
        frame: i64,
        epoch: u64,
    },
    Evaluate(EvalTarget, u64),
    SetVariable {
        reference: i64,
        name: String,
        epoch: u64,
    },
    Completions {
        id: u64,
        text: String,
        cursor: usize,
        epoch: u64,
    },
    StepInTargets(u64),
    GotoTargets(usize),
    Goto(usize),
    ExceptionInfo,
    Control,
    Disconnect,
    BackendView {
        expression: String,
        kind: String,
        epoch: u64,
    },
    VariablesPage {
        reference: i64,
        start: usize,
        epoch: u64,
    },
    Protocol {
        command: String,
        epoch: Option<u64>,
        key: Option<i64>,
    },
}

#[derive(Debug, Clone)]
enum EvalTarget {
    Watch(usize, String),
    /// Debug console code.
    Repl(String),
    Hover(String),
    Dialog(String),
    /// The log message of a suspending breakpoint.
    Log,
}

pub struct DebugSession {
    pub id: u64,
    pub name: String,
    /// The configuration being debugged (Restart runs it again).
    pub config: RunConfig,
    pub state: DebugState,
    pub threads: Vec<(i64, String)>,
    /// Thread whose frames are shown: the stopped thread unless another one was picked.
    pub selected_thread: Option<i64>,
    pub frames: Vec<StackFrame>,
    pub selected_frame: usize,
    pub scopes: Vec<Scope>,
    pub variables: HashMap<i64, Vec<Variable>>,
    pub watches: Vec<Watch>,
    pub output: Vec<OutputChunk>,
    pub breakpoints: HashMap<PathBuf, Vec<Breakpoint>>,
    pub exceptions: ExceptionBreakpoints,
    /// Mute Breakpoints: no line or exception breakpoint is active.
    pub muted: bool,
    pub options: DebugOptions,
    /// Debug console evaluations, merged with `output` by [`ReplItem::at`].
    pub repl: Vec<ReplItem>,
    pub hover_result: Option<EvalResult>,
    /// Result of the Evaluate Expression dialog.
    pub dialog_result: Option<EvalResult>,
    pub completions: Option<Completions>,
    /// Calls Smart Step Into offers (answer to [`DebugSession::request_step_in_targets`]).
    pub step_in_targets: Option<Vec<StepInTarget>>,
    /// `(exception id, description, details)` of the current exception stop.
    pub exception_info: Option<(String, String, String)>,
    /// Incremented on every suspension; variable references are only valid within one.
    pub stop_epoch: u64,
    pub started: Instant,
    pub protocol: crate::protocol::ProtocolState,
    pub backend: crate::backend_workspace::BackendWorkspace,
    child: Option<Child>,
    client: Option<DapClient>,
    javascript_children: javascript_children::Children,
    pending: HashMap<u64, Pending>,
    stdio_rx: Receiver<OutputChunk>,
    initialized: bool,
    config_done: bool,
    bp_requests_in_flight: usize,
    vars_in_flight: HashSet<i64>,
    /// Run to Cursor target: an extra breakpoint until the next suspension.
    run_to_cursor: Option<(PathBuf, usize)>,
    /// DAP breakpoint id → where it was set (the line as the IDE knows it).
    bp_ids: HashMap<i64, (PathBuf, usize)>,
    /// `(path, line it was set on)` → the line debugpy put the breakpoint on, when
    /// it moved it off a line without code. debugpy checks lines against the file
    /// on disk, which may not be what the editor shows (unsaved edits), so the
    /// breakpoint keeps its line until the IDE adopts the move.
    adjusted: HashMap<(PathBuf, usize), usize>,
    /// Breakpoints that suspended the program, handled once the frames are known.
    pending_hits: Option<Vec<(PathBuf, usize)>>,
    removed_breakpoints: Vec<(PathBuf, usize)>,
    /// New entries of `adjusted`, `(path, from, to)`, for the IDE.
    moved_breakpoints: Vec<(PathBuf, usize, usize)>,
    errors: Vec<String>,
    completion_seq: u64,
    /// "Process finished with exit code N" was printed.
    exit_reported: bool,
    endpoint: Option<crate::bootstrap::PendingEndpoint>,
    // Last field: release after transport/child fields and explicit teardown.
    _port_lease: DebugPortLease,
}

impl DebugSession {
    /// Connects to an already running console's debugpy adapter. The console
    /// retains process ownership; disconnecting this session never kills it.
    pub fn attach_endpoint(
        host: &str,
        port: u16,
        options: DebugOptions,
        breakpoints: HashMap<PathBuf, Vec<Breakpoint>>,
    ) -> anyhow::Result<Self> {
        let address: std::net::IpAddr = host.parse()?;
        anyhow::ensure!(
            address.is_loopback() && port != 0,
            "Console debugger endpoint must be loopback with a nonzero port"
        );
        Self::attach_adapter(host, port, options, breakpoints, "debugpy")
    }

    /// Attach to a DAP adapter, including remote debugpy and configured JS
    /// adapters. The adapter owns any injection; disconnect never kills the
    /// externally owned debuggee. Connection retries happen on the DAP worker.
    pub fn attach_adapter(
        host: &str,
        port: u16,
        options: DebugOptions,
        breakpoints: HashMap<PathBuf, Vec<Breakpoint>>,
        adapter_id: &str,
    ) -> anyhow::Result<Self> {
        Self::attach_transport(host, port, options, breakpoints, adapter_id, false, None)
    }
    pub fn listen_pydevd_legacy(
        host: &str,
        port: u16,
        options: DebugOptions,
        breakpoints: HashMap<PathBuf, Vec<Breakpoint>>,
    ) -> anyhow::Result<Self> {
        Self::attach_transport(
            host,
            port,
            options,
            breakpoints,
            "pydevd-legacy",
            true,
            None,
        )
    }
    pub fn listen_pydevd(
        host: &str,
        port: u16,
        options: DebugOptions,
        breakpoints: HashMap<PathBuf, Vec<Breakpoint>>,
    ) -> anyhow::Result<Self> {
        Self::attach_transport(host, port, options, breakpoints, "pydevd", true, None)
    }
    fn attach_transport(
        host: &str,
        port: u16,
        options: DebugOptions,
        breakpoints: HashMap<PathBuf, Vec<Breakpoint>>,
        adapter_id: &str,
        listen: bool,
        shared_lease: Option<DebugPortLease>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !host.trim().is_empty() && port != 0,
            "Specify adapter host and nonzero port"
        );
        let lease = match shared_lease {
            Some(lease) => lease,
            None => DebugPortLease::claim(port)?,
        };
        let address = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        let mut client = if adapter_id == "pydevd-legacy" {
            DapClient::listen_legacy(&address, Duration::from_secs(20))?
        } else if listen {
            DapClient::listen(&address, Duration::from_secs(86_400))?
        } else {
            DapClient::connect(&address, Duration::from_secs(20))?
        };
        let seq = client.request("initialize", json!({"clientID":"rustpy", "clientName":"RustPy", "adapterID":adapter_id, "pathFormat":"path", "linesStartAt1":true, "columnsStartAt1":true, "supportsVariableType":true, "supportsVariablePaging":true, "supportsRunInTerminalRequest":false, "supportsStartDebuggingRequest":adapter_id == "javascript"}))?;
        let (_, rx) = unbounded();
        let mut pending = HashMap::new();
        pending.insert(seq, Pending::Initialize);
        let mut protocol = crate::protocol::ProtocolState {
            endpoint: Some((host.to_string(), port, adapter_id.to_string())),
            ..Default::default()
        };
        protocol.deadlines.insert(
            seq,
            (
                Instant::now() + Duration::from_secs(if listen { 86_400 } else { 20 }),
                "initialize".into(),
            ),
        );
        Ok(Self {
            id: 0,
            protocol,
            backend: Default::default(),
            name: "Python Console".into(),
            config: RunConfig::default(),
            state: DebugState::Starting,
            threads: Vec::new(),
            selected_thread: None,
            frames: Vec::new(),
            selected_frame: 0,
            scopes: Vec::new(),
            variables: HashMap::new(),
            watches: Vec::new(),
            output: Vec::new(),
            breakpoints,
            exceptions: ExceptionBreakpoints::default(),
            muted: false,
            options,
            repl: Vec::new(),
            hover_result: None,
            dialog_result: None,
            completions: None,
            step_in_targets: None,
            exception_info: None,
            stop_epoch: 0,
            started: Instant::now(),
            child: None,
            client: Some(client),
            javascript_children: Default::default(),
            pending,
            stdio_rx: rx,
            initialized: false,
            config_done: false,
            bp_requests_in_flight: 0,
            vars_in_flight: HashSet::new(),
            run_to_cursor: None,
            bp_ids: HashMap::new(),
            adjusted: HashMap::new(),
            pending_hits: None,
            removed_breakpoints: Vec::new(),
            moved_breakpoints: Vec::new(),
            errors: Vec::new(),
            completion_seq: 0,
            exit_reported: false,
            endpoint: None,
            _port_lease: lease,
        })
    }

    /// Launches `cfg` under `python -m debugpy --listen` and connects.
    pub fn launch(
        id: u64,
        cfg: &RunConfig,
        interpreter: Option<&Interpreter>,
        project_root: &Path,
        setup: LaunchSetup,
    ) -> anyhow::Result<DebugSession> {
        Self::launch_on_port(
            id,
            cfg,
            interpreter,
            project_root,
            setup,
            DebugPortLease::acquire()?,
        )
    }

    fn launch_on_port(
        id: u64,
        cfg: &RunConfig,
        interpreter: Option<&Interpreter>,
        project_root: &Path,
        setup: LaunchSetup,
        mut port_lease: DebugPortLease,
    ) -> anyhow::Result<DebugSession> {
        let (program, debug_args, cwd, env) =
            debug_command(cfg, interpreter, project_root, 0, &setup.options)?;
        let bootstrap = crate::bootstrap::Bootstrap::new()?;
        // debug_command remains the canonical CLI/target builder. Replace only
        // its debugpy module entrypoint with a one-shot endpoint-publishing shim.
        let module = debug_args
            .windows(2)
            .position(|pair| pair == ["-m", "debugpy"])
            .ok_or_else(|| anyhow::anyhow!("Missing debugpy command entrypoint"))?;
        let bootstrap_source = if setup.options.backend.collect_runtime_types {
            format!(
                "{}\ninstall({}, {})\n{}",
                include_str!("backend_runtime_types.py"),
                serde_json::to_string(&project_root.to_string_lossy()).unwrap(),
                serde_json::to_string(
                    &rustpy_run::typecollect::output_path(project_root).to_string_lossy()
                )
                .unwrap(),
                crate::bootstrap::PYTHON
            )
        } else {
            crate::bootstrap::PYTHON.to_owned()
        };
        let bootstrap_source = if cfg.env.contains_key("RUSTPY_SCIENTIFIC_OUTPUT") {
            format!("{}\nscientific_install(True)\nimport atexit\natexit.register(scientific_capture)\n{}",crate::scientific::PLOT_SOURCE,bootstrap_source)
        } else {
            bootstrap_source
        };
        let mut actual_args = debug_args.clone();
        actual_args.splice(module..module + 2, ["-c".into(), bootstrap_source]);
        let (tx, rx) = unbounded();
        let mut cmd = rustpy_core::interpreter_launch::interpreter_command(&program);
        if !cfg.workflow.inherit_environment {
            cmd.env_clear();
        }
        cmd.env(
            "RUSTPY_DEBUG_REPR_ENABLED",
            if setup.options.backend.enable_repr {
                "1"
            } else {
                "0"
            },
        );
        if !setup.options.backend.use_cython {
            cmd.env("PYDEVD_USE_CYTHON", "NO");
        }
        if !setup.options.backend.use_frame_eval {
            cmd.env("PYDEVD_USE_FRAME_EVAL", "NO");
        }
        if !setup.options.backend.use_monitoring {
            cmd.env("PYDEVD_USE_SYS_MONITORING", "NO");
        }
        // A "Redirect input from" file replaces the console's input pipe.
        let stdin =
            rustpy_run::process::config_stdin(cfg, project_root).unwrap_or_else(Stdio::piped);
        cmd.args(&actual_args)
            .current_dir(&cwd)
            .envs(&env)
            .env("RUSTPY_DEBUG_BOOTSTRAP_ADDRESS", bootstrap.address()?)
            .env("RUSTPY_DEBUG_BOOTSTRAP_TOKEN", bootstrap.token())
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        port_lease.hand_off();
        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("Cannot run program \"{}\": {e}", program.display()))?;
        if cfg.input_file(project_root).is_some() {
            // Close a failed redirect's fallback pipe; a real file has no parent writer.
            child.stdin.take();
        }
        if let Some(out) = child.stdout.take() {
            spawn_reader(out, OutputKind::Stdout, tx.clone());
        }
        if let Some(err) = child.stderr.take() {
            spawn_reader(err, OutputKind::Stderr, tx.clone());
        }
        let command_line = std::iter::once(program.to_string_lossy().into_owned())
            .chain(debug_args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ");
        let mut session = DebugSession {
            protocol: Default::default(),
            backend: Default::default(),
            id,
            name: cfg.name.clone(),
            config: cfg.clone(),
            state: DebugState::Starting,
            threads: Vec::new(),
            selected_thread: None,
            frames: Vec::new(),
            selected_frame: 0,
            scopes: Vec::new(),
            variables: HashMap::new(),
            watches: Vec::new(),
            output: vec![OutputChunk {
                kind: OutputKind::System,
                text: format!("{command_line}\n"),
            }],
            breakpoints: setup.breakpoints,
            exceptions: setup.exceptions,
            muted: setup.muted,
            options: setup.options,
            repl: Vec::new(),
            hover_result: None,
            dialog_result: None,
            completions: None,
            step_in_targets: None,
            exception_info: None,
            stop_epoch: 0,
            started: Instant::now(),
            child: Some(child),
            client: None,
            javascript_children: Default::default(),
            pending: HashMap::new(),
            stdio_rx: rx,
            initialized: false,
            config_done: false,
            bp_requests_in_flight: 0,
            vars_in_flight: HashSet::new(),
            run_to_cursor: None,
            bp_ids: HashMap::new(),
            adjusted: HashMap::new(),
            pending_hits: None,
            removed_breakpoints: Vec::new(),
            moved_breakpoints: Vec::new(),
            errors: Vec::new(),
            completion_seq: 0,
            exit_reported: false,
            endpoint: None,
            _port_lease: port_lease,
        };
        // Build the session before starting the receiver so a worker spawn
        // failure still uses session Drop to kill and reap the owned child.
        session.endpoint = Some(bootstrap.receive(session.child.as_ref().unwrap().id())?);
        Ok(session)
    }

    fn request(&mut self, command: &str, args: Value, pending: Pending) {
        if std::env::var("RUSTPY_DAP_TRACE").is_ok() {
            eprintln!("DAP -> {command} {args}");
        }
        if self.pending.len() >= 4096 {
            self.errors
                .push("Too many pending debugger requests".into());
            return;
        }
        self.protocol.log("→", command, &args);
        if let Some(c) = &mut self.client {
            match c.request(command, args) {
                Ok(seq) => {
                    self.pending.insert(seq, pending);
                    let waiting = command == "attach"
                        && self
                            .config
                            .type_options
                            .as_ref()
                            .is_some_and(|spec| spec.listen);
                    self.protocol.deadlines.insert(
                        seq,
                        (
                            Instant::now() + Duration::from_secs(if waiting { 86_400 } else { 30 }),
                            command.into(),
                        ),
                    );
                }
                Err(e) => {
                    self.errors.push(format!("{command}: {e}"));
                    log::error!("dap request {command} failed: {e}");
                    self.handle_failure(pending, command, e.to_string());
                }
            }
        }
    }

    pub fn is_active(&self) -> bool {
        !matches!(
            self.state,
            DebugState::Terminated(_) | DebugState::Failed(_)
        )
    }

    /// Whether code runs in the `repl` context (console, Evaluate dialog, log messages).
    fn repl_evaluation_pending(&self) -> bool {
        self.pending.values().any(|p| {
            matches!(
                p,
                Pending::Evaluate(
                    EvalTarget::Repl(_) | EvalTarget::Dialog(_) | EvalTarget::Log,
                    _
                )
            )
        })
    }

    pub fn is_stopped(&self) -> bool {
        matches!(self.state, DebugState::Stopped { .. })
    }

    /// The thread that reported the current suspension.
    pub fn stopped_thread(&self) -> Option<i64> {
        match &self.state {
            DebugState::Stopped { thread_id, .. } => Some(*thread_id),
            _ => self.threads.first().map(|t| t.0),
        }
    }

    /// The thread stepping acts on: the one shown in Frames once its frames are
    /// loaded, else the stopped thread.
    fn control_thread(&self) -> Option<i64> {
        match self.selected_thread {
            Some(t) if self.is_stopped() && !self.frames.is_empty() => Some(t),
            _ => self.stopped_thread(),
        }
    }

    /// Processes incoming messages; returns true if anything changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(chunk) = self.stdio_rx.try_recv() {
            push_chunk(&mut self.output, chunk);
            changed = true;
        }
        if let Some(result) =
            self.endpoint
                .as_ref()
                .and_then(|endpoint| match endpoint.incoming.try_recv() {
                    Ok(result) => Some(result),
                    Err(crossbeam_channel::TryRecvError::Empty) => None,
                    Err(crossbeam_channel::TryRecvError::Disconnected) => Some(Err(
                        "Owned debugpy endpoint receiver closed unexpectedly".into(),
                    )),
                })
        {
            self.endpoint = None;
            if self.state == DebugState::Starting {
                // Do not connect to an endpoint whose publishing child has
                // already exited; preserve its original startup diagnostics.
                let result = match self.child.as_mut().map(|child| child.try_wait()) {
                    Some(Ok(None)) => result,
                    Some(Ok(Some(status))) => {
                        self.fail_before_connecting(status.code());
                        return true;
                    }
                    Some(Err(error)) => Err(format!("Cannot verify owned debugger child: {error}")),
                    None => {
                        Err("Owned debugger child disappeared before endpoint connection".into())
                    }
                };
                let client = result.map_err(anyhow::Error::msg).and_then(|address| {
                    let endpoint_port=address.rsplit_once(':').and_then(|(_,port)|port.parse::<u16>().ok()).ok_or_else(||anyhow::anyhow!("Invalid owned debugger endpoint"))?;
                    self._port_lease=DebugPortLease::claim(endpoint_port)?;
                    let remaining = Duration::from_secs(20).saturating_sub(self.started.elapsed());
                    let mut client = DapClient::connect(&address, remaining)?;
                    let seq = client.request("initialize", json!({ "clientID": "rustpy", "clientName": "RustPy", "adapterID": "debugpy", "pathFormat": "path", "linesStartAt1": true, "columnsStartAt1": true, "supportsVariableType": true, "supportsVariablePaging": true, "supportsRunInTerminalRequest": false, "locale": "en" }))?;
                    self.protocol.endpoint=address.rsplit_once(':').and_then(|(host,port)|Some((host.trim_matches(['[',']']).to_string(),port.parse().ok()?,"debugpy".into())));
                    self.protocol.deadlines.insert(seq,(Instant::now()+remaining,"initialize".into()));
                    self.pending.insert(seq, Pending::Initialize);
                    Ok(client)
                });
                match client {
                    Ok(client) => self.client = Some(client),
                    Err(error) => {
                        self.state = DebugState::Failed(error.to_string());
                        self.output.push(OutputChunk {
                            kind: OutputKind::Stderr,
                            text: format!("{error}\n"),
                        });
                        if let Some(child) = &mut self.child {
                            let _ = child.kill();
                        }
                    }
                }
                changed = true;
            }
        }
        // debugpy ended before the debugger could connect (an option it does not
        // accept, debugpy not installed…): say why now, not when connecting times out.
        if self.state == DebugState::Starting
            && self.client.as_ref().is_none_or(|c| !c.is_connected())
        {
            if let Some(status) = self
                .child
                .as_mut()
                .and_then(|c| c.try_wait().ok().flatten())
            {
                self.fail_before_connecting(status.code());
                changed = true;
            }
        }
        let expired = self
            .protocol
            .deadlines
            .iter()
            .filter(|(_, (deadline, _))| Instant::now() >= *deadline)
            .map(|(id, (_, command))| (*id, command.clone()))
            .collect::<Vec<_>>();
        for (id, command) in expired {
            self.protocol.deadlines.remove(&id);
            if let Some(pending) = self.pending.remove(&id) {
                self.handle_failure(pending, &command, format!("{command} timed out"));
                changed = true;
            }
        }
        let messages: Vec<Message> = match &self.client {
            Some(c) => c.incoming.try_iter().collect(),
            None => Vec::new(),
        };
        for m in messages {
            changed = true;
            self.handle(m);
        }
        let exit = if self.exit_reported {
            None
        } else {
            self.child
                .as_mut()
                .and_then(|c| c.try_wait().ok().flatten())
        };
        if let Some(status) = exit {
            // debugpy's `terminated` event has no exit code: the process has it.
            self.exit_reported = true;
            let code = status.code();
            if self.is_active() || self.state == DebugState::Terminated(None) {
                self.state = DebugState::Terminated(code);
            }
            self.end_suspension();
            self.output.push(OutputChunk {
                kind: OutputKind::System,
                text: format!("\nProcess finished with exit code {}\n", code.unwrap_or(-1)),
            });
            changed = true;
        }
        changed |= self.poll_javascript_children();
        if !self.is_active()
            && !self
                .pending
                .values()
                .any(|pending| matches!(pending, Pending::Disconnect))
        {
            if let Some(client) = &mut self.client {
                client.close();
            }
        }
        let inactive = !self.is_active();
        if let Some(client) = &mut self.client {
            if !client.poll_retirement() && inactive {
                changed = true;
            }
        }
        changed
    }

    /// Forgets the frames and variables of the last suspension (resumed or ended).
    fn end_suspension(&mut self) {
        self.frames.clear();
        self.scopes.clear();
        self.variables.clear();
        self.vars_in_flight.clear();
        self.exception_info = None;
        self.backend.page_counts.clear();
        self.backend.page_more.clear();
        self.backend.async_tasks = None;
        self.backend.value_view = None;
        self.backend.data_view = None;
        self.backend.pending_view = false;
        self.protocol.custom_results.remove("dataBreakpointInfo");
        self.hover_result = None;
        self.completions = None;
        self.step_in_targets = None;
    }

    fn handle(&mut self, m: Message) {
        if std::env::var("RUSTPY_DAP_TRACE").is_ok() {
            eprintln!("DAP <- {m:?}");
        }
        match m {
            Message::Event { event, body } => {
                self.protocol.log("event", &event, &body);
                self.handle_event(&event, body)
            }
            Message::Response {
                request_seq,
                command,
                success,
                message,
                body,
            } => {
                self.protocol.deadlines.remove(&request_seq);
                self.protocol.log("←", &command, &body);
                let Some(pending) = self.pending.remove(&request_seq) else {
                    return;
                };
                if !success {
                    let msg = message.filter(|m| !m.is_empty()).unwrap_or_else(|| {
                        body["error"]["format"]
                            .as_str()
                            .unwrap_or("request failed")
                            .to_string()
                    });
                    self.handle_failure(pending, &command, msg);
                    return;
                }
                self.handle_response(pending, body);
            }
            Message::Request {
                seq,
                command,
                arguments,
            } => {
                if command == "startDebugging" && self.start_javascript_child(seq, &arguments) {
                    return;
                }
                // Reverse requests we don't support.
                if let Some(c) = &mut self.client {
                    let _ = c.respond(seq, &command, false, json!({}));
                }
            }
            Message::TransportError(error) => {
                if self.is_active() {
                    self.errors.push(error.clone());
                    self.state = DebugState::Failed(error);
                    self.end_suspension();
                }
            }
            Message::ConnectFailed(e) => {
                if self.is_active() {
                    // A debuggee waiting for its debugger would never go on.
                    if let Some(child) = &mut self.child {
                        let _ = child.kill();
                    }
                    self.state = DebugState::Failed(e.clone());
                    self.output.push(OutputChunk { kind: OutputKind::Stderr, text: format!("{e}\nIs debugpy installed for this interpreter? (pip install debugpy)\n") });
                }
            }
        }
    }

    fn handle_failure(&mut self, pending: Pending, command: &str, msg: String) {
        if self.javascript_children.launch.is_some()
            && matches!(
                &pending,
                Pending::Initialize
                    | Pending::Attach
                    | Pending::ConfigurationDone
                    | Pending::SetBreakpoints(..)
                    | Pending::SetExceptionBreakpoints
            )
        {
            self.javascript_children.handshake_failed = true;
        }
        match pending {
            Pending::Evaluate(target, epoch) => {
                if epoch != self.stop_epoch {
                    return;
                }
                match target {
                    EvalTarget::Watch(i, expr) => {
                        if let Some(w) = self.watches.get_mut(i).filter(|w| w.expression == expr) {
                            w.error = Some(msg);
                            w.result = None;
                        }
                    }
                    EvalTarget::Repl(_) => self.push_repl(ReplEntry::Error(msg)),
                    EvalTarget::Hover(expression) => self.hover_result = Some(EvalResult { expression, result: Err(msg), epoch }),
                    EvalTarget::Dialog(expression) => self.dialog_result = Some(EvalResult { expression, result: Err(msg), epoch }),
                    EvalTarget::Log => self.output.push(OutputChunk { kind: OutputKind::Stderr, text: format!("Breakpoint log message: {msg}\n") }),
                }
            }
            Pending::Initialize | Pending::Attach => {
                self.state = DebugState::Failed(msg.clone());
                self.output.push(OutputChunk { kind: OutputKind::Stderr, text: format!("{msg}\n") });
            }
            Pending::BackendCommand{kind,..}=>{self.backend.force_step=false;self.errors.push(format!("{kind}: {msg}"));},
            Pending::BackendView {expression,kind,epoch}=>{if epoch==self.stop_epoch{self.backend.pending_view=false;self.backend.value_view=Some((expression,kind,format!("Error: {msg}")));}},
            Pending::VariablesPage {reference,epoch,..}=>{if epoch==self.stop_epoch{self.vars_in_flight.remove(&reference);self.backend.page_more.remove(&reference);self.errors.push(msg);}},
            Pending::SetVariable { name, .. } => self.errors.push(format!("Cannot set {name}: {msg}")),
            Pending::Completions { .. } | Pending::StackTrace { .. } | Pending::Scopes { .. } => {}
            Pending::StepInTargets(_) => {
                self.step_in_targets = None;
                self.errors.push(format!("Smart Step Into: {msg}"));
            }
            Pending::GotoTargets(line) | Pending::Goto(line) => self.errors.push(format!("Cannot jump to line {line}: {}", if msg == "request failed" { "the program must be suspended on a line of the top frame (step once, then retry)".to_string() } else { msg })),
            Pending::SetBreakpoints(..) => {
                self.bp_requests_in_flight = self.bp_requests_in_flight.saturating_sub(1);
                self.output.push(OutputChunk { kind: OutputKind::Stderr, text: format!("{command}: {msg}\n") });
                self.maybe_configuration_done();
            }
            _ => self.output.push(OutputChunk { kind: OutputKind::Stderr, text: format!("{command}: {msg}\n") }),
        }
    }

    fn handle_event(&mut self, event: &str, body: Value) {
        match event {
            "debugpyAttach" => {
                if self.options.backend.sub_process && self.backend.subprocesses.len() < 64 {
                    self.backend.subprocesses.push(body);
                }
            }
            "capabilities" => self.protocol.update_capabilities(&body["capabilities"]),
            "module" => {
                let module = &body["module"];
                let id = module["id"].to_string();
                if body["reason"] == "removed" {
                    self.protocol.modules.remove(&id);
                } else if self.protocol.modules.len() < 1024 && module.to_string().len() <= 65_536 {
                    self.protocol.modules.insert(id, module.clone());
                }
            }
            "process" => self.protocol.process = Some(body.clone()),
            "loadedSource" => {
                let source = &body["source"];
                self.protocol.sources.retain(|old| {
                    old["sourceReference"] != source["sourceReference"]
                        || old["path"] != source["path"]
                });
                if body["reason"] != "removed"
                    && self.protocol.sources.len() < 1024
                    && source.to_string().len() <= 65_536
                {
                    self.protocol.sources.push(source.clone());
                }
            }
            "invalidated" => {
                self.variables.clear();
                self.request_scopes();
            }
            "debugpyWaitingForServer" => {
                self.output.push(OutputChunk {
                    kind: OutputKind::System,
                    text: format!(
                        "Waiting for remote Python connection at {}:{}\n",
                        body["host"].as_str().unwrap_or(""),
                        body["port"]
                    ),
                });
            }
            "initialized" => {
                self.initialized = true;
                self.send_all_breakpoints();
                if self.protocol.supports("supportsFunctionBreakpoints")
                    && !self.protocol.function_breakpoints.is_empty()
                {
                    self.set_function_breakpoints(self.protocol.function_breakpoints.clone());
                }
                if self.protocol.supports("supportsDataBreakpoints")
                    && !self.protocol.data_breakpoints.is_empty()
                {
                    self.set_data_breakpoints(self.protocol.data_breakpoints.clone());
                }
                self.send_exception_breakpoints();
                self.maybe_configuration_done();
            }
            "stopped" => {
                let thread_id = body["threadId"].as_i64().unwrap_or(0);
                let reason = body["reason"].as_str().unwrap_or("").to_string();
                let description = body["description"]
                    .as_str()
                    .or_else(|| body["text"].as_str())
                    .unwrap_or("")
                    .to_string();
                let is_exception = reason == "exception";
                // debugpy puts the exception class name in `text`; SystemExit and friends never stop.
                let text = body["text"].as_str().unwrap_or("");
                let exc_name = text
                    .split(|c: char| c.is_whitespace() || c == '(')
                    .next()
                    .unwrap_or("")
                    .rsplit('.')
                    .next()
                    .unwrap_or("")
                    .to_string();
                if is_exception
                    && matches!(
                        exc_name.as_str(),
                        "SystemExit" | "KeyboardInterrupt" | "GeneratorExit"
                    )
                {
                    self.request(
                        "continue",
                        json!({ "threadId": thread_id }),
                        Pending::Control,
                    );
                    self.state = DebugState::Running;
                    return;
                }
                self.stop_epoch += 1;
                self.state = DebugState::Stopped {
                    thread_id,
                    reason: reason.clone(),
                    description,
                };
                self.selected_thread = Some(thread_id);
                self.frames.clear();
                self.scopes.clear();
                self.variables.clear();
                self.vars_in_flight.clear();
                self.exception_info = None;
                self.hover_result = None;
                self.completions = None;
                self.step_in_targets = None;
                self.selected_frame = 0;
                if let Some(muted) = self.backend.restore_mute.take() {
                    self.muted = muted;
                    self.send_all_breakpoints();
                }
                if self.backend.force_step {
                    self.backend.force_step = false;
                    let expression="(lambda _db, _b: _db.set_use_libraries_filter(_b.__dict__.pop('_rustpy_force_step_filter', True)))(__import__('_pydevd_bundle.pydevd_constants', fromlist=['get_global_debugger']).get_global_debugger(), __import__('builtins'))";
                    self.request(
                        "evaluate",
                        json!({"expression":expression,"context":"repl"}),
                        Pending::BackendCommand {
                            kind: "restore-step-filter".into(),
                            epoch: self.stop_epoch,
                        },
                    );
                }
                // Run to Cursor is over at the next suspension, wherever it is.
                let run_to = self.run_to_cursor.take();
                if let Some((path, _)) = &run_to {
                    let path = path.clone();
                    self.send_breakpoints_for(&path);
                }
                self.pending_hits = (reason == "breakpoint").then(|| {
                    let ids = body["hitBreakpointIds"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    ids.iter()
                        .filter_map(Value::as_i64)
                        .filter_map(|id| self.bp_ids.get(&id).cloned())
                        .filter(|hit| Some(hit) != run_to.as_ref())
                        .collect()
                });
                if is_exception && self.protocol.supports("supportsExceptionInfoRequest") {
                    // Learn the exception type first: SystemExit/KeyboardInterrupt are auto-continued.
                    self.request(
                        "exceptionInfo",
                        json!({ "threadId": thread_id }),
                        Pending::ExceptionInfo,
                    );
                } else {
                    self.request("threads", json!({}), Pending::Threads);
                    self.request_stack_trace(thread_id);
                }
            }
            "continued" => {
                if self.is_active() {
                    if self.is_stopped() {
                        self.end_suspension();
                    }
                    self.state = DebugState::Running;
                }
            }
            "thread" => {
                let id = body["threadId"].as_i64().unwrap_or(0);
                match body["reason"].as_str() {
                    Some("started") => {
                        if !self.threads.iter().any(|t| t.0 == id) {
                            self.threads.push((id, format!("Thread {id}")));
                        }
                    }
                    Some("exited") => self.threads.retain(|t| t.0 != id),
                    _ => {}
                }
            }
            "output" => {
                // The debuggee's own stdout/stderr are read from our pipes (redirectOutput
                // is off), so these come from the debugger: logpoint messages arrive as
                // `stdout`, condition errors as `stderr`.
                let text = body["output"].as_str().unwrap_or("").to_string();
                let kind = match body["category"].as_str() {
                    Some("telemetry") => return,
                    Some("stdout") => OutputKind::Stdout,
                    Some("stderr") => OutputKind::Stderr,
                    _ => OutputKind::System,
                };
                // While code runs in the `repl` context pydevd copies what it prints to
                // these events, and it reaches the program's stdout (our pipe) as well.
                if !matches!(kind, OutputKind::System) && self.repl_evaluation_pending() {
                    return;
                }
                self.output.push(OutputChunk { kind, text });
            }
            "terminated" => {
                if self.is_active() {
                    self.state = DebugState::Terminated(None);
                }
                self.end_suspension();
            }
            "exited" => {
                // "Process finished" is reported when the process is gone (see `poll`).
                self.state = DebugState::Terminated(body["exitCode"].as_i64().map(|c| c as i32));
                self.end_suspension();
            }
            "breakpoint" => {
                for command in ["setFunctionBreakpoints", "setDataBreakpoints"] {
                    if let Some(points) = self
                        .protocol
                        .custom_results
                        .get_mut(command)
                        .and_then(|result| result["breakpoints"].as_array_mut())
                    {
                        let id = body["breakpoint"]["id"].as_i64();
                        if let Some(id) = id {
                            if body["reason"] == "removed" {
                                points.retain(|point| point["id"].as_i64() != Some(id));
                            } else if let Some(point) = points
                                .iter_mut()
                                .find(|point| point["id"].as_i64() == Some(id))
                            {
                                *point = body["breakpoint"].clone();
                            }
                        }
                    }
                }
                let bp = &body["breakpoint"];
                let id = bp["id"].as_i64();
                let reported = bp["source"]["path"]
                    .as_str()
                    .map(PathBuf::from)
                    .zip(bp["line"].as_u64().map(|l| l as usize));
                let Some((path, line)) = id
                    .and_then(|id| self.bp_ids.get(&id).cloned())
                    .or_else(|| reported.clone())
                else {
                    return;
                };
                let rejection = breakpoint_rejection(bp);
                // Verified once its module loaded, possibly on another line.
                let actual = reported
                    .filter(|(p, l)| *p == path && *l > 0 && rejection.is_none())
                    .map_or(line, |(_, l)| l);
                self.note_adjusted(&path, line, actual);
                if let Some(b) = self
                    .breakpoints
                    .get_mut(&path)
                    .and_then(|list| list.iter_mut().find(|b| b.line == line))
                {
                    b.verified = rejection.is_none();
                }
            }
            _ => {}
        }
    }

    fn handle_response(&mut self, pending: Pending, body: Value) {
        match pending {
            Pending::Initialize => {
                self.protocol.negotiate(body.clone());
                let mut files: Vec<PathBuf> = self
                    .breakpoints
                    .iter()
                    .filter(|(_, list)| list.iter().any(|b| b.enabled))
                    .map(|(p, _)| p.clone())
                    .collect();
                files.sort();
                let mut args = self.options.attach_args(&files);
                let mut request = "attach".to_string();
                if let Some(spec) = &self.config.type_options {
                    use rustpy_project::configuration_types::ConfigurationType;
                    match spec.kind {
                        ConfigurationType::PythonDebugServer => {
                            if spec.listen && !spec.pydevd {
                                args["listen"] = json!({"host":spec.host,"port":spec.port});
                            }
                        }
                        ConfigurationType::AttachProcess => {
                            args["processId"] = json!(spec.process_id);
                        }
                        ConfigurationType::JavaScriptDebug => {
                            request = spec.debug_request.clone();
                            args = spec.debug_options.clone();
                            let mut environment = serde_json::Map::new();
                            if !self.config.workflow.inherit_environment {
                                for (key, _) in std::env::vars_os() {
                                    if let Some(key) = key.to_str() {
                                        environment.insert(key.into(), Value::Null);
                                    }
                                }
                            }
                            if let Some(explicit) = args["env"].as_object() {
                                environment.extend(
                                    explicit
                                        .iter()
                                        .map(|(key, value)| (key.clone(), value.clone())),
                                );
                            }
                            let root = self
                                .config
                                .working_dir
                                .as_deref()
                                .unwrap_or_else(|| Path::new("."));
                            let (explicit, errors) = self.config.load_env(root);
                            self.errors.extend(errors);
                            environment.extend(
                                explicit
                                    .into_iter()
                                    .map(|(key, value)| (key, Value::String(value))),
                            );
                            if !environment.is_empty() {
                                args["env"] = Value::Object(environment);
                            }
                            if !spec.target.is_empty() {
                                let target = PathBuf::from(&spec.target);
                                let target = if target.is_absolute() {
                                    target
                                } else {
                                    self.config
                                        .working_dir
                                        .clone()
                                        .unwrap_or_default()
                                        .join(target)
                                };
                                args.as_object_mut()
                                    .unwrap()
                                    .entry("program")
                                    .or_insert(json!(target));
                            }
                            if let Some(cwd) = &self.config.working_dir {
                                args.as_object_mut()
                                    .unwrap()
                                    .entry("cwd")
                                    .or_insert(json!(cwd));
                            }
                        }
                        _ => {}
                    }
                    if !self.is_javascript_adapter() {
                        for key in [
                            "justMyCode",
                            "showReturnValue",
                            "rules",
                            "variablePresentation",
                            "debugOptions",
                            "options",
                            "subProcessId",
                        ] {
                            if let Some(value) = spec.debug_options.get(key) {
                                args[key] = value.clone();
                            }
                        }
                    }
                    if !spec.path_mappings.is_empty() {
                        args["pathMappings"] = spec
                            .path_mappings
                            .iter()
                            .map(|(local, remote)| json!({"localRoot":local,"remoteRoot":remote}))
                            .collect::<Vec<_>>()
                            .into();
                    }
                }
                if let Some((child_request, child_args)) = &self.javascript_children.launch {
                    request = child_request.clone();
                    args = child_args.clone();
                }
                self.request(&request, args, Pending::Attach);
            }
            Pending::Attach => {
                self.javascript_children.launch_acknowledged = true;
                if !self.is_javascript_adapter() {
                    self.debugger_system_info();
                }
                if !self.is_stopped() {
                    self.state = DebugState::Running;
                }
                self.maybe_configuration_done();
            }
            Pending::SetBreakpoints(path, lines) => {
                self.bp_requests_in_flight = self.bp_requests_in_flight.saturating_sub(1);
                self.bp_ids.retain(|_, (p, _)| *p != path);
                self.adjusted.retain(|(p, _), _| *p != path);
                if let Some(result) = body["breakpoints"].as_array() {
                    for (line, r) in lines.iter().zip(result) {
                        let rejection = breakpoint_rejection(r);
                        // debugpy moves a breakpoint on a line without code to one with code.
                        let actual = r["line"]
                            .as_u64()
                            .map(|l| l as usize)
                            .filter(|l| *l > 0 && rejection.is_none())
                            .unwrap_or(*line);
                        self.note_adjusted(&path, *line, actual);
                        if let Some(id) = r["id"].as_i64() {
                            self.bp_ids.insert(id, (path.clone(), *line));
                        }
                        if let Some(b) = self
                            .breakpoints
                            .get_mut(&path)
                            .and_then(|list| list.iter_mut().find(|b| b.line == *line))
                        {
                            b.verified = rejection.is_none();
                        }
                        if rejection.is_some_and(|m| m.contains("excluded by filters")) {
                            // A library file that got its first breakpoint after the launch.
                            let file = path
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            self.errors.push(format!("Breakpoint at {file}:{line} is in a library file: it takes effect from the next debug session (\"Do not step into library scripts\" is on)"));
                        }
                    }
                }
                self.maybe_configuration_done();
            }
            Pending::SetExceptionBreakpoints => {}
            Pending::ConfigurationDone => {
                self.javascript_children.configuration_acknowledged = true;
                if self.state == DebugState::Starting {
                    self.state = DebugState::Running;
                }
            }
            Pending::Threads => {
                if let Some(arr) = body["threads"].as_array() {
                    self.threads = arr
                        .iter()
                        .map(|t| {
                            (
                                t["id"].as_i64().unwrap_or(0),
                                t["name"].as_str().unwrap_or("Thread").to_string(),
                            )
                        })
                        .collect();
                }
            }
            Pending::StackTrace { thread, epoch } => {
                if epoch != self.stop_epoch
                    || self.selected_thread != Some(thread)
                    || !self.is_stopped()
                {
                    return;
                }
                if let Some(arr) = body["stackFrames"].as_array() {
                    self.protocol.frame_sources = arr
                        .iter()
                        .filter_map(|frame| Some((frame["id"].as_i64()?, frame["source"].clone())))
                        .collect();
                    self.frames = arr
                        .iter()
                        .map(|f| {
                            let path = f["source"]["path"]
                                .as_str()
                                .map(|path| self.local_source_path(path));
                            let is_library = f["source"]["presentationHint"].as_str()
                                == Some("deemphasize")
                                || path
                                    .as_ref()
                                    .map(|p| {
                                        p.to_string_lossy().contains("site-packages")
                                            || p.to_string_lossy().contains("/lib/python")
                                    })
                                    .unwrap_or(false);
                            StackFrame {
                                id: f["id"].as_i64().unwrap_or(0),
                                name: f["name"].as_str().unwrap_or("").to_string(),
                                path,
                                line: f["line"].as_u64().unwrap_or(1) as usize,
                                column: f["column"].as_u64().unwrap_or(1) as usize,
                                is_library,
                            }
                        })
                        .collect();
                    self.selected_frame = 0;
                    self.request_scopes();
                    self.refresh_watches();
                    if Some(thread) == self.stopped_thread() {
                        if let Some(hits) = self.pending_hits.take() {
                            self.process_breakpoint_hits(hits);
                        }
                    }
                }
            }
            Pending::Scopes { frame, epoch } => {
                if epoch != self.stop_epoch || self.current_frame().map(|f| f.id) != Some(frame) {
                    return;
                }
                if let Some(arr) = body["scopes"].as_array() {
                    self.scopes = arr
                        .iter()
                        .map(|s| Scope {
                            name: s["name"].as_str().unwrap_or("").to_string(),
                            variables_reference: s["variablesReference"].as_i64().unwrap_or(0),
                            expensive: s["expensive"].as_bool().unwrap_or(false),
                        })
                        .collect();
                    // Expand the first (Locals) scope eagerly.
                    use rustpy_settings::debugger_workspace::LoadingPolicy;
                    if self.options.backend.loading == LoadingPolicy::Synchronous {
                        let references = self
                            .scopes
                            .iter()
                            .filter(|scope| !scope.expensive)
                            .map(|scope| scope.variables_reference)
                            .collect::<Vec<_>>();
                        for reference in references {
                            self.request_variables(reference);
                        }
                    } else if self.options.backend.loading == LoadingPolicy::Asynchronous {
                        if let Some(first) = self.scopes.first() {
                            let reference = first.variables_reference;
                            self.request_variables(reference);
                        }
                    }
                }
            }
            Pending::Evaluate(target, epoch) => {
                if epoch != self.stop_epoch {
                    return;
                }
                let var = Variable {
                    name: String::new(),
                    value: body["result"].as_str().unwrap_or("").to_string(),
                    type_name: body["type"].as_str().unwrap_or("").to_string(),
                    variables_reference: body["variablesReference"].as_i64().unwrap_or(0),
                    evaluate_name: None,
                };
                match target {
                    EvalTarget::Watch(i, expr) => {
                        if let Some(w) = self.watches.get_mut(i).filter(|w| w.expression == expr) {
                            w.result = Some(Variable {
                                name: expr.clone(),
                                evaluate_name: Some(expr),
                                ..var
                            });
                            w.error = None;
                        }
                    }
                    EvalTarget::Repl(code) => {
                        // Statements (assignments, prints) have no value to show.
                        if !(var.value.is_empty()
                            && var.type_name.is_empty()
                            && var.variables_reference == 0)
                        {
                            // Named after a one-line expression (its children evaluate from it).
                            let name = if code.contains('\n') {
                                String::new()
                            } else {
                                code.trim().to_string()
                            };
                            self.push_repl(ReplEntry::Value(Variable {
                                evaluate_name: (!name.is_empty()).then(|| name.clone()),
                                name,
                                ..var
                            }));
                        }
                    }
                    EvalTarget::Hover(expression) => {
                        self.hover_result = Some(EvalResult {
                            result: Ok(Variable {
                                name: expression.clone(),
                                evaluate_name: Some(expression.clone()),
                                ..var
                            }),
                            expression,
                            epoch,
                        })
                    }
                    EvalTarget::Dialog(expression) => {
                        self.dialog_result = Some(EvalResult {
                            result: Ok(Variable {
                                name: expression.clone(),
                                evaluate_name: Some(expression.clone()),
                                ..var
                            }),
                            expression,
                            epoch,
                        })
                    }
                    EvalTarget::Log => {
                        // `evaluate` answers with the repr of the string.
                        let text = unquote_python_str(&var.value).unwrap_or(var.value);
                        self.output.push(OutputChunk {
                            kind: OutputKind::Stdout,
                            text: format!("{text}\n"),
                        });
                    }
                }
            }
            Pending::SetVariable {
                reference,
                name,
                epoch,
            } => {
                if epoch != self.stop_epoch {
                    return;
                }
                if let Some(v) = self
                    .variables
                    .get_mut(&reference)
                    .and_then(|vars| vars.iter_mut().find(|v| v.name == name))
                {
                    if let Some(value) = body["value"].as_str() {
                        v.value = value.to_string();
                    }
                    if let Some(t) = body["type"].as_str() {
                        v.type_name = t.to_string();
                    }
                    v.variables_reference = body["variablesReference"].as_i64().unwrap_or(0);
                }
                // Other views of the same objects (children, watches) may have changed too.
                let refs: Vec<i64> = self
                    .variables
                    .keys()
                    .copied()
                    .filter(|r| *r != reference)
                    .collect();
                for r in refs {
                    self.refresh_variables(r);
                }
                self.refresh_watches();
            }
            Pending::Completions {
                id,
                text,
                cursor,
                epoch,
            } => {
                if epoch != self.stop_epoch || id != self.completion_seq {
                    return;
                }
                let items = body["targets"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|t| CompletionItem {
                                label: t["label"].as_str().unwrap_or("").to_string(),
                                kind: t["type"].as_str().unwrap_or("").to_string(),
                            })
                            .filter(|c| !c.label.is_empty())
                            .collect()
                    })
                    .unwrap_or_default();
                self.completions = Some(Completions {
                    id,
                    text,
                    cursor,
                    items,
                });
            }
            Pending::StepInTargets(epoch) => {
                if epoch != self.stop_epoch {
                    return;
                }
                let targets = body["targets"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|t| {
                                Some(StepInTarget {
                                    id: t["id"].as_i64()?,
                                    label: t["label"].as_str().unwrap_or("").to_string(),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                self.step_in_targets = Some(targets);
            }
            Pending::GotoTargets(line) => {
                let target = body["targets"]
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|t| t["id"].as_i64());
                match (target, self.control_thread()) {
                    (Some(target), Some(thread)) if self.is_stopped() => self.request(
                        "goto",
                        json!({ "threadId": thread, "targetId": target }),
                        Pending::Goto(line),
                    ),
                    _ => self.errors.push(format!("Cannot jump to line {line}")),
                }
            }
            Pending::ExceptionInfo => {
                let raw_id = body["exceptionId"]
                    .as_str()
                    .or_else(|| body["details"]["typeName"].as_str())
                    .unwrap_or("");
                // debugpy appends a "(note: …)" hint to the class name.
                let id = raw_id
                    .split(|c: char| c.is_whitespace() || c == '(')
                    .next()
                    .unwrap_or("")
                    .to_string();
                let description = body["description"].as_str().unwrap_or("").to_string();
                let details = body["details"]["stackTrace"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                let base = id.rsplit('.').next().unwrap_or(&id).to_string();
                if matches!(
                    base.as_str(),
                    "SystemExit" | "KeyboardInterrupt" | "GeneratorExit"
                ) {
                    self.continue_();
                    return;
                }
                self.exception_info = Some((id, description, details));
                if let DebugState::Stopped { thread_id, .. } = self.state {
                    self.request("threads", json!({}), Pending::Threads);
                    self.request_stack_trace(thread_id);
                }
            }
            Pending::BackendCommand { kind, epoch } => {
                if epoch == self.stop_epoch && kind == "force-step" {
                    self.backend.force_step = true;
                    self.step_into();
                }
            }
            Pending::BackendView {
                expression,
                kind,
                epoch,
            } => self.backend_view_response(expression, kind, epoch, body),
            Pending::VariablesPage {
                reference,
                start,
                epoch,
            } => self.backend_variables_response(reference, start, epoch, body),
            Pending::Protocol {
                command,
                epoch,
                key,
            } => self.protocol_response(&command, epoch, key, body),
            Pending::Goto(_) | Pending::Control | Pending::Disconnect => {}
        }
    }

    /// Logs the messages of suspending breakpoints that were hit and removes
    /// temporary ("Remove once hit") ones.
    fn process_breakpoint_hits(&mut self, mut hits: Vec<(PathBuf, usize)>) {
        if hits.is_empty() {
            // No breakpoint ids in the event: the top frame tells where it stopped
            // (on the line debugpy moved a breakpoint to, it stands for that one).
            if let Some((path, line)) = self
                .frames
                .first()
                .and_then(|f| Some((f.path.clone()?, f.line)))
            {
                let set_on = self
                    .adjusted
                    .iter()
                    .find(|((p, _), to)| *p == path && **to == line)
                    .map_or(line, |((_, from), _)| *from);
                hits.push((path, set_on));
            }
        }
        let mut resend: Vec<PathBuf> = Vec::new();
        self.apply_breakpoint_dependencies(&hits);
        for (path, line) in hits {
            let Some(list) = self.breakpoints.get_mut(&path) else {
                continue;
            };
            let Some(idx) = list
                .iter()
                .position(|b| b.line == line && b.enabled && b.suspend)
            else {
                continue;
            };
            let bp = list[idx].clone();
            if bp.temporary {
                list.remove(idx);
                self.removed_breakpoints.push((path.clone(), line));
                if !resend.contains(&path) {
                    resend.push(path.clone());
                }
            }
            if let (Some(msg), Some(frame)) = (bp.log_text(), self.frames.first()) {
                let args = json!({ "expression": log_expression(msg), "frameId": frame.id, "context": "repl" });
                let epoch = self.stop_epoch;
                self.request("evaluate", args, Pending::Evaluate(EvalTarget::Log, epoch));
            }
        }
        for p in resend {
            self.send_breakpoints_for(&p);
        }
    }

    fn maybe_configuration_done(&mut self) {
        // The `attach` response only arrives after `configurationDone` (DAP ordering),
        // so do not wait for it here.
        if self.initialized
            && !self.config_done
            && self.bp_requests_in_flight == 0
            && matches!(self.state, DebugState::Running | DebugState::Starting)
        {
            self.config_done = true;
            if !self.protocol.negotiated
                || self.protocol.supports("supportsConfigurationDoneRequest")
            {
                self.request("configurationDone", json!({}), Pending::ConfigurationDone);
            }
        }
    }

    fn send_all_breakpoints(&mut self) {
        let mut paths: Vec<PathBuf> = self.breakpoints.keys().cloned().collect();
        if let Some((p, _)) = &self.run_to_cursor {
            if !paths.contains(p) {
                paths.push(p.clone());
            }
        }
        for p in paths {
            self.send_breakpoints_for(&p);
        }
    }

    fn send_breakpoints_for(&mut self, path: &Path) {
        let run_to = self
            .run_to_cursor
            .as_ref()
            .filter(|(p, _)| p == path)
            .map(|(_, l)| *l);
        let (bps, lines) = match self.breakpoints.get(path) {
            Some(list) => dap_breakpoints(list, self.muted, run_to),
            None => dap_breakpoints(&[], self.muted, run_to),
        };
        self.bp_requests_in_flight += 1;
        self.request(
            "setBreakpoints",
            json!({ "source": { "path": path.to_string_lossy() }, "breakpoints": bps }),
            Pending::SetBreakpoints(path.to_path_buf(), lines),
        );
    }

    fn send_exception_breakpoints(&mut self) {
        let args = if self.is_javascript_adapter() {
            let filters = if self.muted {
                json!([])
            } else {
                self.config
                    .type_options
                    .as_ref()
                    .and_then(|spec| spec.debug_options.get("exceptionFilters"))
                    .cloned()
                    .unwrap_or_else(|| json!(["uncaught"]))
            };
            json!({"filters":filters})
        } else {
            self.exceptions.to_dap(self.muted)
        };
        self.request(
            "setExceptionBreakpoints",
            args,
            Pending::SetExceptionBreakpoints,
        );
    }

    /// Replaces the breakpoints of `path` (takes effect immediately when the session is live).
    pub fn set_breakpoints(&mut self, path: &Path, mut bps: Vec<Breakpoint>) {
        // Keep what the debugger reported for lines that stay; new ones count as
        // accepted until it answers.
        let old = self.breakpoints.get(path);
        for b in &mut bps {
            if self
                .backend
                .dependencies
                .contains(&(path.to_path_buf(), b.line))
            {
                b.enabled = true;
            }
            b.verified = old
                .and_then(|old| old.iter().find(|o| o.line == b.line))
                .is_none_or(|o| o.verified);
        }
        self.breakpoints.insert(path.to_path_buf(), bps);
        if self.initialized && self.is_active() {
            self.send_breakpoints_for(path);
        }
    }

    /// Replaces the exception breakpoints.
    pub fn set_exception_breakpoints(&mut self, exceptions: ExceptionBreakpoints) {
        if exceptions == self.exceptions {
            return;
        }
        self.exceptions = exceptions;
        if self.initialized && self.is_active() {
            self.send_exception_breakpoints();
        }
    }

    /// Mute Breakpoints: while muted no line or exception breakpoint suspends or logs.
    pub fn set_muted(&mut self, muted: bool) {
        if muted == self.muted {
            return;
        }
        self.muted = muted;
        if self.initialized && self.is_active() {
            self.send_all_breakpoints();
            self.send_exception_breakpoints();
            if self.protocol.supports("supportsFunctionBreakpoints") {
                self.set_function_breakpoints(self.protocol.function_breakpoints.clone());
            }
            if self.protocol.supports("supportsDataBreakpoints") {
                self.set_data_breakpoints(self.protocol.data_breakpoints.clone());
            }
        }
    }

    /// Whether the live session's debugger accepts the breakpoint at `line` of
    /// `path`: false once it rejected it, None without a live session.
    pub fn is_verified(&self, path: &Path, line: usize) -> Option<bool> {
        if !self.initialized || !self.is_active() {
            return None;
        }
        self.breakpoints
            .get(path)?
            .iter()
            .find(|b| b.line == line)
            .map(|b| b.verified)
    }

    /// Temporary breakpoints removed after they were hit (drained by the IDE).
    pub fn take_removed_breakpoints(&mut self) -> Vec<(PathBuf, usize)> {
        std::mem::take(&mut self.removed_breakpoints)
    }

    /// Breakpoints the debugger put on another line than the one they were set on,
    /// `(path, from, to)` (drained by the IDE). The session keeps knowing them by
    /// `from` until the IDE adopts the move ([`DebugSession::adopt_moved_breakpoint`]):
    /// debugpy checks the lines against the file on disk, which differs from the
    /// editor's text after unsaved edits.
    pub fn take_moved_breakpoints(&mut self) -> Vec<(PathBuf, usize, usize)> {
        std::mem::take(&mut self.moved_breakpoints)
    }

    /// Records that the debugger put the breakpoint set at `from` on line `to`;
    /// a new place of one of the IDE's breakpoints is reported to it.
    fn note_adjusted(&mut self, path: &Path, from: usize, to: usize) {
        let key = (path.to_path_buf(), from);
        let before = self.adjusted.get(&key).copied().unwrap_or(from);
        if to == from {
            self.adjusted.remove(&key);
            return;
        }
        self.adjusted.insert(key, to);
        // Run to Cursor's own breakpoint is not the IDE's.
        let ours = self
            .breakpoints
            .get(path)
            .is_some_and(|list| list.iter().any(|b| b.line == from));
        if to != before && ours {
            self.moved_breakpoints.push((path.to_path_buf(), from, to));
        }
    }

    /// The IDE keeps the breakpoint set at `from` of `path` on line `to`, where the
    /// debugger moved it: from now on it is known by `to` (it merges into a
    /// breakpoint already there). The debugger already has it there.
    pub fn adopt_moved_breakpoint(&mut self, path: &Path, from: usize, to: usize) {
        if let Some(list) = self.breakpoints.get_mut(path) {
            if let Some(i) = list.iter().position(|b| b.line == from) {
                if list.iter().any(|b| b.line == to) {
                    list.remove(i);
                } else {
                    list[i].line = to;
                }
            }
        }
        self.adjusted.remove(&(path.to_path_buf(), from));
        let rename = |p: &Path, line: &mut usize| {
            if p == path && *line == from {
                *line = to;
            }
        };
        // Ids, hits not handled yet and requests still unanswered name it by `from`.
        for (p, line) in self.bp_ids.values_mut() {
            rename(p.as_path(), line);
        }
        for (p, line) in self.pending_hits.iter_mut().flatten() {
            rename(p.as_path(), line);
        }
        for pending in self.pending.values_mut() {
            if let Pending::SetBreakpoints(p, lines) = pending {
                for line in lines.iter_mut() {
                    rename(p.as_path(), line);
                }
            }
        }
    }

    /// Problems to report to the user (drained by the IDE).
    pub fn take_errors(&mut self) -> Vec<String> {
        std::mem::take(&mut self.errors)
    }

    fn request_stack_trace(&mut self, thread: i64) {
        let epoch = self.stop_epoch;
        self.request(
            "stackTrace",
            json!({ "threadId": thread, "startFrame": 0, "levels": 100 }),
            Pending::StackTrace { thread, epoch },
        );
    }

    fn request_scopes(&mut self) {
        let Some(frame) = self.frames.get(self.selected_frame) else {
            return;
        };
        let id = frame.id;
        let epoch = self.stop_epoch;
        self.scopes.clear();
        self.request(
            "scopes",
            json!({ "frameId": id }),
            Pending::Scopes { frame: id, epoch },
        );
    }

    /// Loads the children of `reference` once (no-op while loaded or loading).
    pub fn request_variables(&mut self, reference: i64) {
        if reference == 0
            || self.variables.contains_key(&reference)
            || self.vars_in_flight.contains(&reference)
            || !self.is_stopped()
        {
            return;
        }
        self.request_variables_page(reference, 0);
    }

    /// Loads the children of `reference` again.
    fn refresh_variables(&mut self, reference: i64) {
        self.vars_in_flight.remove(&reference);
        self.request_variables_page(reference, 0);
    }

    /// Shows the frames of another suspended thread.
    pub fn select_thread(&mut self, thread: i64) {
        if !self.is_stopped() || self.selected_thread == Some(thread) {
            return;
        }
        self.stop_epoch += 1;
        self.backend.value_view = None;
        self.backend.data_view = None;
        self.backend.pending_view = false;
        self.backend.page_counts.clear();
        self.backend.page_more.clear();
        self.selected_thread = Some(thread);
        self.frames.clear();
        self.scopes.clear();
        self.variables.clear();
        self.vars_in_flight.clear();
        self.selected_frame = 0;
        self.request_stack_trace(thread);
    }

    pub fn select_frame(&mut self, idx: usize) {
        if idx < self.frames.len() && idx != self.selected_frame {
            self.stop_epoch += 1;
            self.backend.value_view = None;
            self.backend.data_view = None;
            self.backend.pending_view = false;
            self.backend.page_counts.clear();
            self.backend.page_more.clear();
            self.selected_frame = idx;
            self.variables.clear();
            self.vars_in_flight.clear();
            self.request_scopes();
            self.refresh_watches();
        }
    }

    pub fn current_frame(&self) -> Option<&StackFrame> {
        self.frames.get(self.selected_frame)
    }

    pub fn add_watch(&mut self, expr: &str) {
        self.watches.push(Watch {
            expression: expr.to_string(),
            result: None,
            error: None,
        });
        let i = self.watches.len() - 1;
        self.evaluate_watch(i);
    }

    pub fn remove_watch(&mut self, idx: usize) {
        if idx < self.watches.len() {
            let watch = self.watches.remove(idx);
            self.backend.inline_watches.remove(&watch.expression);
        }
    }

    /// Changes a watch expression and evaluates it again.
    pub fn set_watch_expression(&mut self, idx: usize, expr: &str) {
        let Some(w) = self.watches.get_mut(idx) else {
            return;
        };
        if w.expression == expr {
            return;
        }
        *w = Watch {
            expression: expr.to_string(),
            result: None,
            error: None,
        };
        self.evaluate_watch(idx);
    }

    /// Moves a watch one place up or down.
    pub fn move_watch(&mut self, idx: usize, up: bool) {
        let other = if up {
            idx.checked_sub(1)
        } else {
            Some(idx + 1)
        };
        if let Some(j) = other.filter(|j| *j < self.watches.len() && idx < self.watches.len()) {
            self.watches.swap(idx, j);
        }
    }

    fn refresh_watches(&mut self) {
        for i in 0..self.watches.len() {
            self.evaluate_watch(i);
        }
    }

    fn evaluate_watch(&mut self, i: usize) {
        let Some(frame) = self.frames.get(self.selected_frame) else {
            return;
        };
        let (fid, expr) = (frame.id, self.watches[i].expression.clone());
        let epoch = self.stop_epoch;
        self.request(
            "evaluate",
            json!({ "expression": expr, "frameId": fid, "context": "watch" }),
            Pending::Evaluate(EvalTarget::Watch(i, expr), epoch),
        );
    }

    fn push_repl(&mut self, entry: ReplEntry) {
        self.repl.push(ReplItem {
            at: self.output.len(),
            epoch: self.stop_epoch,
            entry,
        });
    }

    /// Evaluates in the debug console; the result is appended to `repl`.
    pub fn evaluate_repl(&mut self, expr: &str) {
        self.evaluate_console_expression(expr, expr);
    }

    /// Uses a console adapter's wire expression while retaining the user's
    /// original input in the shared DAP evaluation log.
    pub fn evaluate_console_expression(&mut self, expr: &str, wire_expression: &str) {
        let wire_expression = if self.is_javascript_adapter() {
            expr
        } else {
            wire_expression
        };
        let fid = self.frames.get(self.selected_frame).map(|f| f.id);
        let mut args = json!({ "expression": wire_expression, "context": "repl" });
        if let Some(f) = fid {
            args["frameId"] = json!(f);
        }
        self.push_repl(ReplEntry::Input(expr.to_string()));
        let epoch = self.stop_epoch;
        self.request(
            "evaluate",
            args,
            Pending::Evaluate(EvalTarget::Repl(expr.to_string()), epoch),
        );
    }

    /// Evaluates for the Evaluate Expression dialog (`dialog_result`).
    pub fn evaluate_dialog(&mut self, expr: &str) {
        let Some(frame) = self.frames.get(self.selected_frame) else {
            return;
        };
        let fid = frame.id;
        let epoch = self.stop_epoch;
        self.dialog_result = None;
        self.request(
            "evaluate",
            json!({ "expression": expr, "frameId": fid, "context": "repl" }),
            Pending::Evaluate(EvalTarget::Dialog(expr.to_string()), epoch),
        );
    }

    /// Evaluates for the editor's value tooltip (`hover_result`).
    pub fn evaluate_hover(&mut self, expr: &str) {
        let Some(frame) = self.frames.get(self.selected_frame) else {
            return;
        };
        let fid = frame.id;
        let epoch = self.stop_epoch;
        self.request(
            "evaluate",
            json!({ "expression": expr, "frameId": fid, "context": "hover" }),
            Pending::Evaluate(EvalTarget::Hover(expr.to_string()), epoch),
        );
    }

    /// Set Value: assigns `value` (a Python expression) to `name` in the container `reference`.
    pub fn set_variable(&mut self, reference: i64, name: &str, value: &str) {
        if self.protocol.negotiated && !self.protocol.supports("supportsSetVariable") {
            self.errors
                .push("Adapter does not support supportsSetVariable".into());
            return;
        }
        if !self.is_stopped() {
            return;
        }
        let epoch = self.stop_epoch;
        self.request(
            "setVariable",
            json!({ "variablesReference": reference, "name": name, "value": value }),
            Pending::SetVariable {
                reference,
                name: name.to_string(),
                epoch,
            },
        );
    }

    /// Encoding used by this debugpy session for completion and frame columns.
    /// debugpy/pydevd uses scalar indices rather than the DAP UTF-16 contract.
    pub fn column_encoding(&self) -> ColumnEncoding {
        if self.is_javascript_adapter() {
            ColumnEncoding::Utf16
        } else {
            ColumnEncoding::DebugpyScalar
        }
    }
    fn is_javascript_adapter(&self) -> bool {
        self.config.type_options.as_ref().is_some_and(|spec| {
            spec.kind == rustpy_project::configuration_types::ConfigurationType::JavaScriptDebug
        })
    }

    /// Asks for debug console completions of `text` with a UTF-8 byte cursor;
    /// the answer arrives in `completions`.
    pub fn request_completions(&mut self, text: &str, cursor: usize) {
        if self.protocol.negotiated && !self.protocol.supports("supportsCompletionsRequest") {
            self.errors
                .push("Adapter does not support supportsCompletionsRequest".into());
            return;
        }
        let Some(frame) = self.frames.get(self.selected_frame) else {
            return;
        };
        let fid = frame.id;
        let (line, column) = self.column_encoding().position(text, cursor);
        self.completion_seq += 1;
        let (id, epoch) = (self.completion_seq, self.stop_epoch);
        self.completions = None;
        self.request(
            "completions",
            json!({ "text": text, "line": line, "column": column, "frameId": fid }),
            Pending::Completions {
                id,
                text: text.to_string(),
                cursor,
                epoch,
            },
        );
    }

    /// Smart Step Into: asks which calls of the current statement can be entered
    /// (answer in `step_in_targets`).
    pub fn request_step_in_targets(&mut self) {
        if self.protocol.negotiated && !self.protocol.supports("supportsStepInTargetsRequest") {
            self.errors
                .push("Adapter does not support supportsStepInTargetsRequest".into());
            return;
        }
        let Some(frame) = self.frames.first() else {
            return;
        };
        let fid = frame.id;
        self.step_in_targets = None;
        let epoch = self.stop_epoch;
        self.request(
            "stepInTargets",
            json!({ "frameId": fid }),
            Pending::StepInTargets(epoch),
        );
    }

    /// Steps into the call `target_id` of [`DebugSession::step_in_targets`].
    pub fn step_into_target(&mut self, target_id: i64) {
        self.step_in_targets = None;
        let Some(t) = self.control_thread() else {
            return;
        };
        self.request(
            "stepIn",
            json!({ "threadId": t, "targetId": target_id }),
            Pending::Control,
        );
        self.state = DebugState::Running;
        self.end_suspension();
    }

    /// Jump to Cursor / Set Next Statement: continues the top frame at `line`
    /// of `path` without running the code in between.
    pub fn jump_to_line(&mut self, path: &Path, line: usize) {
        if self.protocol.negotiated && !self.protocol.supports("supportsGotoTargetsRequest") {
            self.errors
                .push("Adapter does not support supportsGotoTargetsRequest".into());
            return;
        }
        if !self.is_stopped() {
            return;
        }
        if self.frames.first().and_then(|f| f.path.as_deref()) != Some(path) {
            self.errors.push(
                "Jump to Cursor works only in the file of the current execution point (top frame)"
                    .into(),
            );
            return;
        }
        self.request(
            "gotoTargets",
            json!({ "source": { "path": path.to_string_lossy() }, "line": line }),
            Pending::GotoTargets(line),
        );
    }

    /// Run to Cursor: resumes until `line` of `path` (or an earlier breakpoint);
    /// the breakpoint it needs is removed at the next suspension.
    pub fn run_to_cursor(&mut self, path: &Path, line: usize) {
        if !self.is_stopped() {
            return;
        }
        self.run_to_cursor = Some((path.to_path_buf(), line));
        self.send_breakpoints_for(path);
        self.continue_();
    }

    fn control(&mut self, command: &str) {
        let Some(t) = self.control_thread() else {
            return;
        };
        self.request(command, json!({ "threadId": t }), Pending::Control);
        self.state = DebugState::Running;
        self.end_suspension();
    }

    pub fn continue_(&mut self) {
        self.control("continue");
    }
    pub fn step_over(&mut self) {
        self.control("next");
    }
    pub fn step_into(&mut self) {
        self.control("stepIn");
    }
    pub fn step_out(&mut self) {
        self.control("stepOut");
    }
    pub fn pause(&mut self) {
        let t = self.threads.first().map(|t| t.0).unwrap_or(1);
        self.request("pause", json!({ "threadId": t }), Pending::Control);
    }

    /// Stable captured transport identities, including terminal sessions whose
    /// reader/writer workers have not yet been joined.
    pub fn transport_cleanup_ids(&self) -> Vec<u64> {
        let mut ids = self
            .client
            .as_ref()
            .filter(|client| !client.is_retired())
            .map(DapClient::lifecycle_id)
            .into_iter()
            .collect::<Vec<_>>();
        ids.extend(self.javascript_children.transport_cleanup_ids());
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Disconnect acknowledgements are polled before a replacement claims the endpoint.
    pub fn disconnect_completed(&self) -> bool {
        self.client.as_ref().is_none_or(DapClient::is_retired)
            && self.javascript_children.disconnect_completed()
    }

    /// Terminates the debuggee.
    pub fn stop(&mut self) {
        if self.javascript_children.is_stopping() {
            return;
        }
        self.stop_javascript_children();
        if self.client.as_ref().is_some_and(|c| !c.is_connected()) {
            // The adapter is not up yet: nothing to tell it, and stop connecting.
            self.client = None;
        } else {
            self.request(
                "disconnect",
                json!({ "terminateDebuggee": self.child.is_some() || self.config.type_options.as_ref().is_some_and(|spec|spec.kind==rustpy_project::configuration_types::ConfigurationType::JavaScriptDebug && spec.debug_request=="launch") }),
                Pending::Disconnect,
            );
            if !self.is_javascript_adapter() {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        self.endpoint = None;
        if let Some(child) = &mut self.child {
            let _ = child.kill();
        }
        if self.is_active() {
            self.state = DebugState::Terminated(None);
        }
    }

    /// Sends `text` to the program's standard input and shows it in the
    /// console, as the Run console does.
    pub fn write_stdin(&mut self, text: &str) {
        use std::io::Write;
        if let Some(child) = &mut self.child {
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(text.as_bytes());
                let _ = stdin.flush();
            }
        }
        push_chunk(
            &mut self.output,
            OutputChunk {
                kind: OutputKind::System,
                text: text.to_string(),
            },
        );
    }

    /// The debuggee exited before the debugger connected: the session failed.
    fn fail_before_connecting(&mut self, code: Option<i32>) {
        // Its last output (the reason) is in the pipes, which close at the exit.
        let deadline = Instant::now() + Duration::from_millis(300);
        while let Ok(chunk) = self.stdio_rx.recv_deadline(deadline) {
            push_chunk(&mut self.output, chunk);
        }
        let (message, hint) = startup_failure(code, &self.output);
        if let Some(hint) = hint {
            self.output.push(OutputChunk {
                kind: OutputKind::System,
                text: format!("{hint}\n"),
            });
        }
        self.state = DebugState::Failed(message);
        // No adapter is coming: stop connecting.
        self.client = None;
        self.endpoint = None;
    }
}

impl Drop for DebugSession {
    fn drop(&mut self) {
        self.stop_javascript_children();
        self.endpoint = None;
        self.client = None;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        // The logical port lease drops after teardown, including failed starts.
    }
}

fn parse_variable(v: &Value) -> Variable {
    Variable {
        name: v["name"].as_str().unwrap_or("").to_string(),
        value: v["value"].as_str().unwrap_or("").to_string(),
        type_name: v["type"].as_str().unwrap_or("").to_string(),
        variables_reference: v["variablesReference"].as_i64().unwrap_or(0),
        evaluate_name: v["evaluateName"].as_str().map(str::to_owned),
    }
}

/// The text of a Python `str` repr such as `'it\'s'` (None when `s` is not one).
fn unquote_python_str(s: &str) -> Option<String> {
    let q = s.chars().next().filter(|c| *c == '\'' || *c == '"')?;
    let inner = s.strip_prefix(q)?.strip_suffix(q)?;
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    Some(out)
}

fn spawn_reader<R: std::io::Read + Send + 'static>(
    reader: R,
    kind: OutputKind,
    tx: Sender<OutputChunk>,
) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        // Forward what arrives, not whole lines: a prompt such as
        // `input("Name: ")` has no line break and must show while the program
        // waits for the answer.
        let mut pending: Vec<u8> = Vec::new();
        loop {
            let read = match reader.fill_buf() {
                Ok([]) => break,
                Ok(bytes) => {
                    pending.extend_from_slice(bytes);
                    bytes.len()
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            };
            reader.consume(read);
            let text = rustpy_run::process::take_utf8_prefix(&mut pending);
            if !text.is_empty() && tx.send(OutputChunk { kind, text }).is_err() {
                return;
            }
        }
        if !pending.is_empty() {
            let _ = tx.send(OutputChunk {
                kind,
                text: String::from_utf8_lossy(&pending).into_owned(),
            });
        }
    });
}

/// Appends program output. Text that continues an unfinished line of the same
/// stream joins it, so a line written in pieces (`print` writes the text and
/// the `\n` separately) stays one chunk.
fn push_chunk(output: &mut Vec<OutputChunk>, chunk: OutputChunk) {
    match output.last_mut() {
        Some(last) if last.kind == chunk.kind && !last.text.ends_with('\n') => {
            last.text.push_str(&chunk.text)
        }
        _ => output.push(chunk),
    }
}

/// Why a debuggee that exited before the debugger connected failed (`code`:
/// its exit code), from the last line it wrote to stderr; and, when debugpy is
/// missing, how to install it.
fn startup_failure(code: Option<i32>, output: &[OutputChunk]) -> (String, Option<&'static str>) {
    let stderr: String = output
        .iter()
        .filter(|c| c.kind == OutputKind::Stderr)
        .map(|c| c.text.as_str())
        .collect();
    let mut message = match code {
        Some(code) => format!("debugpy exited with code {code} before the debugger connected"),
        None => "debugpy was terminated before the debugger connected".to_string(),
    };
    if let Some(reason) = stderr.lines().map(str::trim).rfind(|l| !l.is_empty()) {
        message = format!("{message}: {reason}");
    }
    let hint = stderr
        .contains("No module named debugpy")
        .then_some("Is debugpy installed for this interpreter? (pip install debugpy)");
    (message, hint)
}

#[cfg(test)]
mod console_io_tests {
    use super::*;
    use std::process::Command;

    fn chunk(kind: OutputKind, text: &str) -> OutputChunk {
        OutputChunk {
            kind,
            text: text.into(),
        }
    }

    #[test]
    fn pieces_of_a_line_join_per_stream() {
        let mut out = vec![chunk(OutputKind::System, "python -m debugpy …\n")];
        for c in [
            chunk(OutputKind::Stdout, "hello"),
            chunk(OutputKind::Stdout, "\n"),
            chunk(OutputKind::Stdout, "Name: "),
            chunk(OutputKind::System, "Bob\n"),
            chunk(OutputKind::Stdout, "Hi Bob\n"),
            chunk(OutputKind::Stderr, "warn"),
            chunk(OutputKind::Stdout, "x\n"),
        ] {
            push_chunk(&mut out, c);
        }
        let texts: Vec<(OutputKind, &str)> =
            out.iter().map(|c| (c.kind, c.text.as_str())).collect();
        assert_eq!(
            texts,
            vec![
                (OutputKind::System, "python -m debugpy …\n"),
                (OutputKind::Stdout, "hello\n"),
                (OutputKind::Stdout, "Name: "),
                (OutputKind::System, "Bob\n"),
                (OutputKind::Stdout, "Hi Bob\n"),
                (OutputKind::Stderr, "warn"),
                (OutputKind::Stdout, "x\n")
            ]
        );
    }

    /// A prompt without a line break reaches the console while the program
    /// waits for input, and the answer written to stdin reaches the program.
    #[test]
    fn prompts_arrive_before_the_line_ends() {
        let Ok(py) = which::which("python3") else {
            return;
        };
        let mut child = Command::new(py)
            .args(["-c", "import sys\nsys.stdout.write('Enter your name: ')\nsys.stdout.flush()\nname = sys.stdin.readline().strip()\nprint(f'Hello, {name}!')\n"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let (tx, rx) = unbounded();
        spawn_reader(child.stdout.take().unwrap(), OutputKind::Stdout, tx);
        let mut out = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !out
            .iter()
            .any(|c: &OutputChunk| c.text.contains("Enter your name: "))
            && Instant::now() < deadline
        {
            if let Ok(c) = rx.recv_timeout(Duration::from_millis(100)) {
                push_chunk(&mut out, c);
            }
        }
        assert_eq!(
            out.iter().map(|c| c.text.as_str()).collect::<String>(),
            "Enter your name: ",
            "the prompt arrives while the program waits"
        );
        assert!(
            child.try_wait().unwrap().is_none(),
            "still waiting for input"
        );
        {
            use std::io::Write;
            let stdin = child.stdin.as_mut().unwrap();
            stdin.write_all(b"Bob\n").unwrap();
            stdin.flush().unwrap();
        }
        while let Ok(c) = rx.recv_timeout(Duration::from_secs(20)) {
            push_chunk(&mut out, c);
        }
        assert_eq!(
            out.iter().map(|c| c.text.as_str()).collect::<String>(),
            "Enter your name: Hello, Bob!\n"
        );
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn utf8_split_between_reads_is_kept_whole() {
        let Ok(py) = which::which("python3") else {
            return;
        };
        // `é` is written one byte at a time.
        let mut child = Command::new(py).args(["-c", "import os, time\nfor b in 'é!\\n'.encode():\n    os.write(1, bytes([b]))\n    time.sleep(0.05)\n"]).stdout(Stdio::piped()).spawn().unwrap();
        let (tx, rx) = unbounded();
        spawn_reader(child.stdout.take().unwrap(), OutputKind::Stdout, tx);
        let mut out = Vec::new();
        while let Ok(c) = rx.recv_timeout(Duration::from_secs(20)) {
            push_chunk(&mut out, c);
        }
        assert_eq!(out, vec![chunk(OutputKind::Stdout, "é!\n")]);
        let _ = child.wait();
    }

    #[test]
    fn startup_failures_name_the_reason() {
        let out = [
            chunk(OutputKind::System, "python3 -m debugpy …\n"),
            chunk(OutputKind::Stderr, "usage: debugpy …\n"),
            chunk(OutputKind::Stderr, "Error: unrecognized switch -u\n\n"),
        ];
        assert_eq!(startup_failure(Some(2), &out), ("debugpy exited with code 2 before the debugger connected: Error: unrecognized switch -u".to_string(), None));
        let missing = [chunk(
            OutputKind::Stderr,
            "/usr/bin/python3: No module named debugpy\n",
        )];
        let (message, hint) = startup_failure(Some(1), &missing);
        assert_eq!(
            message,
            "debugpy exited with code 1 before the debugger connected: /usr/bin/python3: No module named debugpy"
        );
        assert!(hint.is_some_and(|h| h.contains("pip install debugpy")));
        assert_eq!(
            startup_failure(None, &[]).0,
            "debugpy was terminated before the debugger connected"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn wait_until(s: &mut DebugSession, pred: impl Fn(&DebugSession) -> bool, secs: u64) -> bool {
        let start = Instant::now();
        while start.elapsed().as_secs() < secs {
            s.poll();
            if pred(s) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn failed_spawn_releases_its_port_lease() {
        let dir = tempfile::tempdir().unwrap();
        let lease = DebugPortLease::acquire().unwrap();
        let port = lease.port();
        let mut cfg = RunConfig::for_script(&dir.path().join("main.py"));
        cfg.interpreter = Some(dir.path().join("missing-interpreter"));
        assert!(DebugSession::launch_on_port(
            9000,
            &cfg,
            None,
            dir.path(),
            LaunchSetup::default(),
            lease
        )
        .is_err());
        assert!(!crate::port::is_leased(port));
    }

    #[test]
    fn failed_child_keeps_lease_until_session_teardown() {
        let Some(py) = python_with_debugpy() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let (mut session, _) = launch_script(
            9001,
            &py,
            dir.path(),
            "failed.py",
            "pass\n",
            "--invalid-rustpy-fixture-option",
            |_| LaunchSetup::default(),
        );
        let port = session._port_lease.port();
        assert!(wait_until(
            &mut session,
            |s| matches!(s.state, DebugState::Failed(_)),
            10
        ));
        assert!(crate::port::is_leased(port));
        drop(session);
        assert!(!crate::port::is_leased(port));
    }

    #[test]
    fn simultaneous_debugpy_sessions_keep_their_markers_frames_and_ports() {
        let Some(py) = python_with_debugpy() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let mut workers = Vec::new();
        for n in 0..4 {
            let (py, root, barrier) = (py.clone(), dir.path().to_path_buf(), barrier.clone());
            workers.push(std::thread::spawn(move || {
                let marker = format!("owned_session_{n}");
                let name = format!("session_{n}.py");
                barrier.wait();
                let (session, path) = launch_script(
                    9100 + n,
                    &py,
                    &root,
                    &name,
                    &format!("marker = '{marker}'\nprint(marker, flush=True)\n1 / 0\n"),
                    "",
                    |_| LaunchSetup {
                        exceptions: ExceptionBreakpoints {
                            on_terminate: true,
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                );
                (session, path, marker, n)
            }));
        }
        // All sessions coexist before any can stop or release its port.
        let mut sessions: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        let ports: HashSet<_> = sessions.iter().map(|s| s.0._port_lease.port()).collect();
        assert_eq!(ports.len(), 4);
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(30) {
            for (session, _, _, _) in &mut sessions {
                session.poll();
            }
            if sessions
                .iter()
                .all(|s| s.0.is_stopped() && !s.0.frames.is_empty())
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut traces = Vec::new();
        for (mut session, path, marker, n) in sessions {
            let port = session._port_lease.port();
            let pid = session.child.as_ref().unwrap().id();
            assert!(
                session.is_stopped() && !session.frames.is_empty(),
                "session={} state={:?} output={}",
                session.id,
                session.state,
                output_text(&session)
            );
            assert_eq!(session.id, 9100 + n);
            assert_eq!(session.frames[0].path.as_deref(), Some(path.as_path()));
            assert_eq!(session.frames[0].line, 3);
            assert!(output_text(&session).contains(&marker));
            for other in 0..4 {
                if other != n {
                    assert!(!output_text(&session).contains(&format!("owned_session_{other}")));
                }
            }
            session.stop();
            assert!(
                crate::port::is_leased(port),
                "stop cannot release a still-owned session's port"
            );
            drop(session);
            assert!(!crate::port::is_leased(port));
            traces.push((9100 + n, port, pid, path));
        }
        eprintln!(
            "concurrent debugpy ownership traces (session, port, child PID, frame): {traces:?}"
        );
    }

    /// python3 with debugpy installed (the debugpy tests are skipped without it).
    fn python_with_debugpy() -> Option<PathBuf> {
        let required = std::env::var("RUSTPY_REQUIRE_PROVIDER_TESTS").as_deref() == Ok("1");
        let py = if required {
            let path = PathBuf::from(
                std::env::var_os("RUSTPY_PROVIDER_PYTHON")
                    .expect("required provider acceptance needs RUSTPY_PROVIDER_PYTHON"),
            );
            assert!(
                path.is_absolute() && path.is_file(),
                "admit an absolute installed Python interpreter"
            );
            path
        } else {
            which::which("python3").ok()?
        };
        let result = Command::new(&py)
            .args(["-c", "import debugpy; print(debugpy.__version__)"])
            .output();
        let available = result.as_ref().is_ok_and(|output| output.status.success());
        if required {
            assert!(
                available,
                "required debugpy provider missing for {}: {result:?}",
                py.display()
            );
            eprintln!(
                "REQUIRED_PROVIDER debugpy python={} version={}",
                py.display(),
                String::from_utf8_lossy(&result.as_ref().unwrap().stdout).trim()
            );
        }
        if !available {
            eprintln!("debugpy not installed; skipping");
            return None;
        }
        Some(py)
    }

    fn setup(path: &Path, bps: Vec<Breakpoint>) -> LaunchSetup {
        let mut map = HashMap::new();
        if !bps.is_empty() {
            map.insert(path.to_path_buf(), bps);
        }
        LaunchSetup {
            breakpoints: map,
            options: DebugOptions {
                backend: rustpy_settings::debugger_workspace::DebuggerWorkspaceSettings {
                    // Headless fixtures read locals without a UI expansion:
                    // explicitly select eager loading, not the on-demand default.
                    loading: rustpy_settings::debugger_workspace::LoadingPolicy::Synchronous,
                    ..Default::default()
                },
                just_my_code: false,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn launch_script(
        id: u64,
        py: &Path,
        dir: &Path,
        name: &str,
        code: &str,
        opts: &str,
        setup_fn: impl FnOnce(&Path) -> LaunchSetup,
    ) -> (DebugSession, PathBuf) {
        let script = dir.join(name);
        std::fs::write(&script, code).unwrap();
        let mut cfg = RunConfig::for_script(&script);
        cfg.interpreter = Some(py.to_path_buf());
        cfg.interpreter_options = opts.into();
        let s = DebugSession::launch(id, &cfg, None, dir, setup_fn(&script)).expect("launch");
        (s, script)
    }

    fn output_text(s: &DebugSession) -> String {
        s.output.iter().map(|c| c.text.as_str()).collect()
    }

    fn top_line(s: &DebugSession) -> Option<usize> {
        s.frames.first().map(|f| f.line)
    }

    fn local(s: &DebugSession, name: &str) -> Option<String> {
        let r = s.scopes.first()?.variables_reference;
        s.variables
            .get(&r)?
            .iter()
            .find(|v| v.name == name)
            .map(|v| v.value.clone())
    }

    /// "Do not step into library scripts" (justMyCode): Step Into skips library
    /// code, while breakpoints set in library files before the launch still suspend.
    #[test]
    fn just_my_code_skips_library_code_but_keeps_its_breakpoints() {
        let Some(py) = python_with_debugpy() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        let (plain, marked) = (lib.join("plain.py"), lib.join("marked.py"));
        std::fs::write(&plain, "def twice(v):\n    return v * 2\n").unwrap();
        std::fs::write(&marked, "def thrice(v):\n    u = v * 3\n    return u\n").unwrap();
        let script = dir.path().join("main.py");
        std::fs::write(&script, format!("import sys\nsys.path.insert(0, {:?})\nimport plain, marked\na = plain.twice(1)\nb = marked.thrice(a)\nprint('result', a, b)\n", lib.to_string_lossy())).unwrap();
        let mut cfg = RunConfig::for_script(&script);
        cfg.interpreter = Some(py.clone());
        // pydevd treats LIBRARY_ROOTS like site-packages.
        cfg.env
            .insert("LIBRARY_ROOTS".into(), lib.to_string_lossy().into_owned());
        let mut map = HashMap::new();
        map.insert(script.clone(), vec![Breakpoint::new(4)]);
        map.insert(marked.clone(), vec![Breakpoint::new(2)]);
        let setup = LaunchSetup {
            breakpoints: map,
            options: DebugOptions::default(),
            ..Default::default()
        };
        assert!(setup.options.just_my_code);
        let mut s = DebugSession::launch(9, &cfg, None, dir.path(), setup).expect("launch");
        assert!(
            wait_until(&mut s, |s| s.is_stopped() && top_line(s) == Some(4), 30),
            "state={:?} output={}",
            s.state,
            output_text(&s)
        );
        assert_eq!(
            s.is_verified(&marked, 2),
            Some(true),
            "the library file with a breakpoint counts as the user's code"
        );
        // A breakpoint added to another library file now is reported, and does not suspend.
        s.set_breakpoints(&plain, vec![Breakpoint::new(2)]);
        assert!(
            wait_until(&mut s, |s| s.is_verified(&plain, 2) == Some(false), 10),
            "{:?}",
            s.breakpoints.get(&plain)
        );
        let errors = s.take_errors();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("plain.py:2") && e.contains("library file")),
            "{errors:?}"
        );
        // Step Into does not enter plain.twice (library code).
        s.step_into();
        assert!(
            wait_until(&mut s, |s| s.is_stopped() && !s.frames.is_empty(), 15),
            "{:?}",
            s.state
        );
        assert_eq!(
            (s.frames[0].name.as_str(), top_line(&s)),
            ("<module>", Some(5)),
            "stepped into library code: {:?}",
            s.frames.first()
        );
        // The breakpoint in the library file suspends.
        s.continue_();
        assert!(
            wait_until(
                &mut s,
                |s| s.is_stopped() && s.frames.first().is_some_and(|f| f.name == "thrice"),
                15
            ),
            "library breakpoint ignored: {:?} {:?} {}",
            s.state,
            s.frames.first(),
            output_text(&s)
        );
        assert_eq!(top_line(&s), Some(2));
        s.continue_();
        assert!(wait_until(&mut s, |s| !s.is_active(), 20), "{:?}", s.state);
        assert!(
            output_text(&s).contains("result 2 6"),
            "{}",
            output_text(&s)
        );
    }

    #[test]
    fn on_demand_loading_waits_for_explicit_scope_expansion() {
        let Some(py) = python_with_debugpy() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let (mut session, _) = launch_script(
            92,
            &py,
            dir.path(),
            "on_demand.py",
            "value = 41\nprint(value)\n",
            "",
            |path| {
                let mut setup = setup(path, vec![Breakpoint::new(2)]);
                setup.options.backend.loading =
                    rustpy_settings::debugger_workspace::LoadingPolicy::OnDemand;
                setup
            },
        );
        assert!(wait_until(
            &mut session,
            |session| session.is_stopped()
                && !session.frames.is_empty()
                && !session.scopes.is_empty(),
            30
        ));
        assert!(session.variables.is_empty());
        let references = session
            .scopes
            .iter()
            .filter(|scope| !scope.expensive)
            .map(|scope| scope.variables_reference)
            .collect::<Vec<_>>();
        assert!(!references.is_empty());
        for reference in references {
            session.request_variables(reference);
        }
        assert!(wait_until(
            &mut session,
            |session| local(session, "value").as_deref() == Some("41"),
            15
        ));
        session.continue_();
        assert!(wait_until(&mut session, |session| !session.is_active(), 20));
    }

    #[test]
    fn breakpoint_stop_variables_and_continue() {
        let Some(py) = python_with_debugpy() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let (mut s, _) = launch_script(
            1,
            &py,
            dir.path(),
            "prog.py",
            "import sys\nx = 41\ny = x + 1\nprint('done', y)\nsys.exit(3)\n",
            "",
            |p| setup(p, vec![Breakpoint::new(3)]),
        );
        assert!(
            wait_until(&mut s, |s| s.is_stopped(), 30),
            "did not stop: state={:?} output={}",
            s.state,
            output_text(&s)
        );
        assert!(
            wait_until(
                &mut s,
                |s| !s.frames.is_empty()
                    && !s.scopes.is_empty()
                    && s.variables
                        .values()
                        .any(|v| v.iter().any(|x| x.name == "x")),
                15
            ),
            "frames/vars missing"
        );
        let top = &s.frames[0];
        assert_eq!(top.line, 3);
        assert_eq!(top.name, "<module>");
        let locals = s
            .variables
            .values()
            .flatten()
            .find(|v| v.name == "x")
            .unwrap()
            .clone();
        assert_eq!(locals.value, "41");
        assert_eq!(locals.type_name, "int");
        s.add_watch("x * 2");
        assert!(wait_until(&mut s, |s| s.watches[0].result.is_some(), 10));
        assert_eq!(s.watches[0].result.as_ref().unwrap().value, "82");
        s.step_over();
        assert!(
            wait_until(
                &mut s,
                |s| s.is_stopped() && s.frames.first().map(|f| f.line == 4).unwrap_or(false),
                15
            ),
            "step over failed: {:?}",
            s.state
        );
        s.continue_();
        assert!(
            s.frames.is_empty() && s.scopes.is_empty() && s.variables.is_empty(),
            "the suspension's frames and variables are gone once running"
        );
        // debugpy's `terminated` event has no exit code; the process's is used.
        assert!(
            wait_until(&mut s, |s| s.state == DebugState::Terminated(Some(3)), 20),
            "did not terminate with the exit code: {:?}",
            s.state
        );
        assert!(output_text(&s).contains("done 42"), "{}", output_text(&s));
        s.poll();
        assert_eq!(
            output_text(&s)
                .matches("Process finished with exit code 3")
                .count(),
            1,
            "{}",
            output_text(&s)
        );
    }

    #[test]
    fn interpreter_options_come_before_debugpy() {
        let root = Path::new("/proj");
        let mut cfg = RunConfig::for_script(Path::new("/proj/app.py"));
        cfg.interpreter = Some(PathBuf::from("/usr/bin/python3"));
        cfg.interpreter_options = "-X dev -O".into();
        cfg.parameters = "--port 80".into();
        let (program, args, _, _) =
            debug_command(&cfg, None, root, 5678, &DebugOptions::default()).unwrap();
        assert_eq!(program, PathBuf::from("/usr/bin/python3"));
        assert_eq!(
            args,
            vec![
                "-X",
                "dev",
                "-O",
                "-Xfrozen_modules=off",
                "-m",
                "debugpy",
                "--listen",
                "127.0.0.1:5678",
                "--wait-for-client",
                "--configure-subProcess",
                "true",
                "/proj/app.py",
                "--port",
                "80"
            ]
        );
        // A test configuration debugs `-m pytest <node id>`.
        cfg.kind = rustpy_project::RunConfigKind::Pytest {
            target: "tests/test_a.py::TestK::test_x".into(),
            keywords: String::new(),
        };
        cfg.interpreter_options.clear();
        cfg.parameters.clear();
        cfg.working_dir = None;
        let (_, args, _, _) = debug_command(&cfg, None, root, 1, &DebugOptions::default()).unwrap();
        assert_eq!(
            args[5..],
            [
                "--wait-for-client",
                "--configure-subProcess",
                "true",
                "-m",
                "pytest",
                "tests/test_a.py::TestK::test_x"
            ]
        );
    }

    #[test]
    fn debugs_with_interpreter_options_and_redirected_input() {
        let Ok(py) = which::which("python3") else {
            return;
        };
        if Command::new(&py)
            .args(["-c", "import debugpy"])
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("flags.py");
        std::fs::write(
            &script,
            "import sys\nprint('dev', sys.flags.dev_mode, 'input', sys.stdin.read().strip())\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("input.txt"), "from the file\n").unwrap();
        let mut cfg = RunConfig::for_script(&script);
        cfg.interpreter = Some(py);
        cfg.interpreter_options = "-X dev".into();
        cfg.redirect_input = Some(PathBuf::from("input.txt"));
        let mut s = DebugSession::launch(3, &cfg, None, dir.path(), LaunchSetup::default())
            .expect("launch");
        let finished = wait_until(
            &mut s,
            |s| matches!(s.state, DebugState::Terminated(_) | DebugState::Failed(_)),
            30,
        );
        let text: String = s.output.iter().map(|c| c.text.as_str()).collect();
        assert!(finished, "state={:?} output={text}", s.state);
        assert!(text.contains("dev True input from the file"), "{text}");
        std::fs::remove_file(dir.path().join("input.txt")).unwrap();
        std::fs::write(
            &script,
            "import sys\nprint('redirect EOF:' + repr(sys.stdin.read()))\n",
        )
        .unwrap();
        let mut s = DebugSession::launch(4, &cfg, None, dir.path(), LaunchSetup::default())
            .expect("launch after redirect disappeared");
        assert!(s.child.as_ref().unwrap().stdin.is_none());
        assert!(wait_until(
            &mut s,
            |s| matches!(s.state, DebugState::Terminated(_) | DebugState::Failed(_)),
            30
        ));
        let text: String = s
            .output
            .iter()
            .filter(|chunk| chunk.kind == OutputKind::Stdout)
            .map(|chunk| chunk.text.as_str())
            .collect();
        assert_eq!(text, "redirect EOF:''\n");
    }

    #[test]
    fn uncaught_exception_stops_with_info() {
        let Some(py) = python_with_debugpy() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let (mut s, _) = launch_script(
            2,
            &py,
            dir.path(),
            "boom.py",
            "def f():\n    raise ValueError('bad')\nf()\n",
            "",
            |p| setup(p, vec![]),
        );
        assert!(
            wait_until(
                &mut s,
                |s| s.is_stopped() && s.exception_info.is_some() && !s.frames.is_empty(),
                30
            ),
            "state={:?}",
            s.state
        );
        let (id, desc, _) = s.exception_info.clone().unwrap();
        assert!(id.contains("ValueError"), "{id}");
        assert!(desc.contains("bad"), "{desc}");
        assert_eq!(s.frames[0].name, "f");
        s.stop();
        assert!(wait_until(&mut s, |s| !s.is_active(), 10));
    }

    #[test]
    fn exception_frame_columns_use_debugpy_scalar_compatibility() {
        let Some(py) = python_with_debugpy() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let line = "(\"😀e\u{301}\", 1)[1] / 0";
        let (mut s, script) = launch_script(
            23,
            &py,
            dir.path(),
            "columns.py",
            &format!("{line}\n"),
            "",
            |p| setup(p, vec![]),
        );
        assert!(
            wait_until(
                &mut s,
                |s| s.is_stopped() && s.exception_info.is_some() && !s.frames.is_empty(),
                30
            ),
            "state={:?}",
            s.state
        );
        let frame = s
            .frames
            .iter()
            .find(|f| f.path.as_ref() == Some(&script))
            .expect("source frame");
        assert_eq!(frame.line, 1);
        assert_eq!(
            frame.column, 15,
            "installed debugpy reports the slash in scalar units"
        );
        let byte = s.column_encoding().byte_offset(line, frame.column);
        assert_eq!(byte, line.find('/').unwrap());
        assert_eq!(ColumnEncoding::Utf16.column(line, byte), 16);
        s.stop();
        assert!(wait_until(&mut s, |s| !s.is_active(), 10));
    }

    /// Interpreter options used to end up after debugpy's switches, which it rejects.
    #[test]
    fn interpreter_options_reach_the_debuggee() {
        let Some(py) = python_with_debugpy() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let (mut s, _) = launch_script(
            3,
            &py,
            dir.path(),
            "flags.py",
            "import sys\nmode = sys.flags.dev_mode\nprint('dev', mode)\n",
            "-X dev -u",
            |p| setup(p, vec![Breakpoint::new(3)]),
        );
        assert!(
            wait_until(&mut s, |s| s.is_stopped() && local(s, "mode").is_some(), 30),
            "state={:?} output={}",
            s.state,
            output_text(&s)
        );
        assert_eq!(local(&s, "mode").as_deref(), Some("True"));
        assert!(
            output_text(&s)
                .lines()
                .next()
                .unwrap_or("")
                .contains("-X dev -u -Xfrozen_modules=off -m debugpy"),
            "{}",
            output_text(&s)
        );
        s.continue_();
        assert!(wait_until(&mut s, |s| !s.is_active(), 20));
        assert!(output_text(&s).contains("dev True"), "{}", output_text(&s));
    }

    #[test]
    fn raw_console_completions_follow_physical_newlines() {
        let Some(py) = python_with_debugpy() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let (mut session, _) = launch_script(
            41,
            &py,
            dir.path(),
            "completions.py",
            "items = []\nprint('ready')\n",
            "",
            |path| setup(path, vec![Breakpoint::new(2)]),
        );
        assert!(
            wait_until(
                &mut session,
                |s| s.is_stopped() && local(s, "items").is_some(),
                30
            ),
            "state={:?} output={}",
            session.state,
            output_text(&session)
        );
        for text in [
            "x=1\ritems.appzzz",
            "'😀e\u{301}'\r\nitems.appzzz",
            "'😀e\u{301}'\rx=1\nitems.appzzz",
            "x=1\n'😀e\u{301}'\r\nitems.appzzz",
        ] {
            let cursor = text.find("zzz").unwrap();
            session.request_completions(text, cursor);
            assert!(
                wait_until(&mut session, |s| s.completions.is_some(), 10),
                "{text:?}: no response"
            );
            assert!(
                session
                    .completions
                    .as_ref()
                    .unwrap()
                    .items
                    .iter()
                    .any(|item| item.label == "append"),
                "{text:?}: {:?}",
                session.completions
            );
        }
    }

    #[test]
    fn set_value_completions_threads_and_console_order() {
        let Some(py) = python_with_debugpy() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        // Stepping one thread resumes the others (debugpy's default), so the main
        // thread keeps looping after the breakpoint instead of finishing the program.
        let code = "import threading\nimport time\n\n\ndef worker():\n    n = 0\n    while not stop.is_set():\n        n += 1\n        time.sleep(0.01)\n\n\nstop = threading.Event()\nt = threading.Thread(target=worker, name='Worker-1')\nt.start()\ntime.sleep(0.2)\nitems = [1, 2, 3]\nprint('before', items)\nprint('after', items)\nwhile True:\n    time.sleep(0.01)\n";
        let (mut s, _) = launch_script(4, &py, dir.path(), "threads.py", code, "", |p| {
            setup(p, vec![Breakpoint::new(18)])
        });
        assert!(
            wait_until(
                &mut s,
                |s| s.is_stopped() && local(s, "items").is_some() && s.threads.len() >= 2,
                30
            ),
            "state={:?} threads={:?} output={}",
            s.state,
            s.threads,
            output_text(&s)
        );
        assert!(
            wait_until(&mut s, |s| output_text(s).contains("before [1, 2, 3]"), 10),
            "{}",
            output_text(&s)
        );
        // Set Value, then watches and the variables reflect the new value.
        s.add_watch("len(items)");
        assert!(wait_until(&mut s, |s| s.watches[0].result.is_some(), 10));
        assert_eq!(s.watches[0].result.as_ref().unwrap().value, "3");
        let locals = s.scopes[0].variables_reference;
        s.set_variable(locals, "items", "[7, 8]");
        assert!(
            wait_until(
                &mut s,
                |s| local(s, "items").as_deref() == Some("[7, 8]")
                    && s.watches[0].result.as_ref().is_some_and(|r| r.value == "2"),
                10
            ),
            "set value: {:?} {:?}",
            local(&s, "items"),
            s.watches[0]
        );
        // Completions in the current frame.
        s.request_completions("ite", 3);
        assert!(wait_until(&mut s, |s| s.completions.is_some(), 10));
        assert!(
            s.completions
                .as_ref()
                .unwrap()
                .items
                .iter()
                .any(|c| c.label == "items"),
            "{:?}",
            s.completions
        );
        s.request_completions("items.app", 9);
        assert!(wait_until(&mut s, |s| s.completions.is_some(), 10));
        assert!(
            s.completions
                .as_ref()
                .unwrap()
                .items
                .iter()
                .any(|c| c.label == "append"),
            "{:?}",
            s.completions
        );
        // Multi-line input completes on the cursor's line.
        let multi = "first = 1\nitems.cou";
        s.request_completions(multi, multi.len());
        assert!(wait_until(&mut s, |s| s.completions.is_some(), 10));
        assert!(
            s.completions
                .as_ref()
                .unwrap()
                .items
                .iter()
                .any(|c| c.label == "count"),
            "{:?}",
            s.completions
        );
        // Astral characters distinguish debugpy scalar columns from standard
        // UTF-16. The suffix keeps an oversized column from clamping at EOF.
        for (text, label) in [
            ("('😀', items.appzzz)", "append"),
            ("('e\u{301}', items.appzzz)", "append"),
            ("('😀e\u{301}', items.appzzz)", "append"),
            ("func('😀e\u{301}', itezzz)", "items"),
            ("func('😀e\u{301}', items.appzzz)", "append"),
            ("'😀e\u{301}'\nitems.appzzz", "append"),
        ] {
            let cursor = text.find("zzz").unwrap();
            s.request_completions(text, cursor);
            assert!(wait_until(&mut s, |s| s.completions.is_some(), 10));
            assert!(
                s.completions
                    .as_ref()
                    .unwrap()
                    .items
                    .iter()
                    .any(|c| c.label == label),
                "{text}: {:?}",
                s.completions
            );
        }
        // The console log stays in order: input, result.
        s.evaluate_repl("items + [9]");
        assert!(
            wait_until(
                &mut s,
                |s| {
                    s.repl.iter().any(|r| matches!(&r.entry, ReplEntry::Value(v) if v.value == "[7, 8, 9]" && v.variables_reference != 0 && v.name == "items + [9]"))
                },
                10
            ),
            "{:?}",
            s.repl
        );
        s.evaluate_repl("undefined_name");
        assert!(
            wait_until(
                &mut s,
                |s| s.repl.iter().any(
                    |r| matches!(&r.entry, ReplEntry::Error(e) if e.contains("undefined_name"))
                ),
                10
            ),
            "{:?}",
            s.repl
        );
        // What console code prints shows once: pydevd also copies it to `output` events.
        s.evaluate_repl("print('from-the-console')");
        assert!(
            wait_until(
                &mut s,
                |s| output_text(s).contains("from-the-console") && !s.repl_evaluation_pending(),
                10
            ),
            "{}",
            output_text(&s)
        );
        std::thread::sleep(Duration::from_millis(300));
        s.poll();
        assert_eq!(
            output_text(&s).matches("from-the-console").count(),
            1,
            "{}",
            output_text(&s)
        );
        assert!(
            matches!(s.repl[0].entry, ReplEntry::Input(_))
                && s.repl.windows(2).all(|w| w[0].at <= w[1].at)
        );
        // Another thread: its own frames, and stepping acts on it.
        let main = s.stopped_thread().unwrap();
        let worker = s
            .threads
            .iter()
            .find(|t| t.1 == "Worker-1")
            .map(|t| t.0)
            .expect("worker thread");
        s.select_thread(worker);
        assert!(
            wait_until(
                &mut s,
                |s| s.frames.first().is_some_and(|f| f.name == "worker"),
                10
            ),
            "{:?}",
            s.frames
        );
        let line = top_line(&s).unwrap();
        s.step_over();
        assert!(
            wait_until(
                &mut s,
                |s| s.is_stopped()
                    && s.stopped_thread() == Some(worker)
                    && s.frames
                        .first()
                        .is_some_and(|f| f.name == "worker" && f.line != line),
                15
            ),
            "step on the worker: {:?} {:?}",
            s.state,
            s.frames.first()
        );
        s.select_thread(main);
        assert!(
            wait_until(
                &mut s,
                |s| s
                    .frames
                    .first()
                    .is_some_and(|f| f.name == "<module>" && f.line >= 18),
                10
            ),
            "{:?}",
            s.frames.first()
        );
        s.continue_();
        assert!(
            wait_until(&mut s, |s| output_text(s).contains("after [7, 8]"), 10),
            "the new value is used: {}",
            output_text(&s)
        );
        s.stop();
        assert!(wait_until(&mut s, |s| !s.is_active(), 10), "{:?}", s.state);
    }

    #[test]
    fn conditional_log_and_temporary_breakpoints() {
        let Some(py) = python_with_debugpy() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let code = "total = 0\nfor i in range(6):\n    total += i\n    print('i', i)\nprint('total', total)\nprint('end')\n";
        let (mut s, script) = launch_script(5, &py, dir.path(), "loop.py", code, "", |p| {
            let mut cond = Breakpoint::new(3);
            // Would hold on every later iteration too: only "Remove once hit" stops it.
            cond.condition = Some("i >= 2".into());
            cond.temporary = true;
            cond.log_message = Some("hit at {i}".into());
            let mut logpoint = Breakpoint::new(4);
            logpoint.suspend = false;
            logpoint.log_message = Some("log total={total}".into());
            let mut hits = Breakpoint::new(5);
            hits.hit_condition = Some("1".into());
            setup(p, vec![cond, logpoint, hits])
        });
        assert!(
            wait_until(&mut s, |s| s.is_stopped() && local(s, "i").is_some(), 30),
            "state={:?} output={}",
            s.state,
            output_text(&s)
        );
        assert_eq!(top_line(&s), Some(3));
        assert_eq!(local(&s, "i").as_deref(), Some("2"), "the condition holds");
        assert!(
            wait_until(&mut s, |s| output_text(s).contains("hit at 2\n"), 10),
            "a suspending breakpoint logs its message: {}",
            output_text(&s)
        );
        assert!(
            output_text(&s).contains("log total=0\n") && output_text(&s).contains("log total=1\n"),
            "logpoints: {}",
            output_text(&s)
        );
        // Removed once hit.
        assert!(wait_until(
            &mut s,
            |s| !s.breakpoints[&script].iter().any(|b| b.line == 3),
            10
        ));
        assert_eq!(s.take_removed_breakpoints(), vec![(script.clone(), 3)]);
        s.continue_();
        assert!(
            wait_until(&mut s, |s| s.is_stopped() && top_line(s) == Some(5), 15),
            "next stop is the hit-count breakpoint: {:?} {:?}",
            s.state,
            s.frames.first()
        );
        assert!(
            output_text(&s).contains("log total=15\n"),
            "logpoints never suspend: {}",
            output_text(&s)
        );
        s.continue_();
        assert!(wait_until(&mut s, |s| !s.is_active(), 20), "{:?}", s.state);
    }

    #[test]
    fn exception_class_breakpoints_and_mute() {
        let Some(py) = python_with_debugpy() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let code = "for exc in (KeyError, ValueError, ValueError):\n    try:\n        raise exc('x')\n    except Exception:\n        pass\nprint('end')\n";
        let (mut s, _) = launch_script(6, &py, dir.path(), "exc.py", code, "", |p| {
            let mut setup = setup(p, vec![]);
            setup.exceptions.classes.push(ExceptionClassBreakpoint {
                name: "ValueError".into(),
                enabled: true,
                on_raise: true,
                on_terminate: false,
            });
            setup
        });
        assert!(
            wait_until(&mut s, |s| s.is_stopped() && s.exception_info.is_some(), 30),
            "state={:?} output={}",
            s.state,
            output_text(&s)
        );
        assert!(
            s.exception_info.as_ref().unwrap().0.contains("ValueError"),
            "{:?}",
            s.exception_info
        );
        // Muted: the second ValueError does not stop.
        s.set_muted(true);
        s.continue_();
        assert!(
            wait_until(&mut s, |s| !s.is_active(), 20),
            "muted breakpoints still stopped: {:?}",
            s.state
        );
        assert!(output_text(&s).contains("end"));
    }

    #[test]
    fn run_to_cursor_leaves_no_breakpoint_behind() {
        let Some(py) = python_with_debugpy() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let code = "x = 0\nfor i in range(3):\n    x += 1\n    print('i', i)\nprint('end', x)\n";
        let (mut s, script) = launch_script(7, &py, dir.path(), "rtc.py", code, "", |p| {
            setup(p, vec![Breakpoint::new(1)])
        });
        assert!(
            wait_until(&mut s, |s| s.is_stopped() && top_line(s) == Some(1), 30),
            "state={:?}",
            s.state
        );
        s.run_to_cursor(&script, 4);
        assert!(
            wait_until(&mut s, |s| s.is_stopped() && top_line(s) == Some(4), 15),
            "{:?} {:?}",
            s.state,
            s.frames.first()
        );
        s.continue_();
        assert!(
            wait_until(&mut s, |s| !s.is_active(), 20),
            "stopped again at the Run to Cursor line: {:?} {:?}",
            s.state,
            s.frames.first()
        );
        assert!(output_text(&s).contains("end 3"));
    }

    #[test]
    fn smart_step_into_and_jump_to_line() {
        let Some(py) = python_with_debugpy() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let code = "def double(v):\n    return v * 2\n\n\ndef inc(v):\n    return v + 1\n\n\nmarker = 'a'\nmarker = 'b'\ntotal = double(inc(1))\nprint('total', total, marker)\n";
        let (mut s, script) = launch_script(8, &py, dir.path(), "calls.py", code, "", |p| {
            setup(p, vec![Breakpoint::new(10)])
        });
        assert!(
            wait_until(&mut s, |s| s.is_stopped() && top_line(s) == Some(10), 30),
            "state={:?}",
            s.state
        );
        // Jump over `marker = 'b'`.
        s.jump_to_line(&script, 11);
        assert!(
            wait_until(&mut s, |s| s.is_stopped() && top_line(s) == Some(11), 15),
            "jump: {:?} errors={:?}",
            s.frames.first(),
            s.errors
        );
        s.jump_to_line(Path::new("/elsewhere.py"), 1);
        assert_eq!(s.take_errors().len(), 1, "only within the current file");
        // Smart Step Into `double` (not `inc`, which runs first).
        s.request_step_in_targets();
        assert!(wait_until(&mut s, |s| s.step_in_targets.is_some(), 10));
        let targets = s.step_in_targets.clone().unwrap();
        let double = targets
            .iter()
            .find(|t| t.label.starts_with("double("))
            .unwrap_or_else(|| panic!("{targets:?}"))
            .id;
        s.step_into_target(double);
        assert!(
            wait_until(
                &mut s,
                |s| s.is_stopped() && s.frames.first().is_some_and(|f| f.name == "double"),
                15
            ),
            "smart step into: {:?}",
            s.frames.first()
        );
        assert!(
            wait_until(&mut s, |s| local(s, "v").as_deref() == Some("2"), 10),
            "inc already ran: {:?}",
            local(&s, "v")
        );
        s.continue_();
        assert!(wait_until(&mut s, |s| !s.is_active(), 20), "{:?}", s.state);
        assert!(output_text(&s).contains("total 4 a"), "{}", output_text(&s));
    }

    /// A breakpoint on a line without code: debugpy puts it on a line with code.
    /// The session keeps knowing it by its own line (the editor's text may differ
    /// from the file on disk) until the IDE adopts the move; hits match it either way.
    #[test]
    fn breakpoints_on_lines_without_code_move() {
        let Some(py) = python_with_debugpy() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let code = "def f():\n    a = 1\n    # comment\n    return a + 1\n\n\nprint('f', f())\nprint('g', f())\n";
        let (mut s, script) = launch_script(10, &py, dir.path(), "moved.py", code, "", |p| {
            let mut bp = Breakpoint::new(3);
            bp.log_message = Some("hit here".into());
            setup(p, vec![bp])
        });
        assert!(
            wait_until(&mut s, |s| s.is_stopped() && !s.frames.is_empty(), 30),
            "state={:?} output={}",
            s.state,
            output_text(&s)
        );
        let moved = s.take_moved_breakpoints();
        assert_eq!(moved.len(), 1, "{moved:?}");
        let (path, from, to) = moved[0].clone();
        assert_eq!((path, from), (script.clone(), 3));
        assert!(to == 2 || to == 4, "{to}");
        assert_eq!(top_line(&s), Some(to), "it stops where the breakpoint went");
        assert_eq!(
            s.breakpoints[&script]
                .iter()
                .map(|b| (b.line, b.verified))
                .collect::<Vec<_>>(),
            vec![(3, true)],
            "known by its own line until adopted"
        );
        assert!(
            wait_until(
                &mut s,
                |s| output_text(s).matches("hit here").count() == 1,
                10
            ),
            "the hit is matched: {}",
            output_text(&s)
        );
        s.adopt_moved_breakpoint(&script, 3, to);
        assert_eq!(
            s.breakpoints[&script]
                .iter()
                .map(|b| (b.line, b.verified))
                .collect::<Vec<_>>(),
            vec![(to, true)]
        );
        assert_eq!(
            (s.is_verified(&script, to), s.is_verified(&script, 3)),
            (Some(true), None)
        );
        // The next hit is matched to the adopted line (its log message shows again).
        s.continue_();
        assert!(
            wait_until(
                &mut s,
                |s| s.is_stopped()
                    && top_line(s) == Some(to)
                    && output_text(s).matches("hit here").count() == 2,
                15
            ),
            "{:?} {}",
            s.state,
            output_text(&s)
        );
        assert!(s.take_moved_breakpoints().is_empty());
        s.continue_();
        assert!(wait_until(&mut s, |s| !s.is_active(), 20), "{:?}", s.state);
        assert!(
            output_text(&s).contains("f 2") && output_text(&s).contains("g 2"),
            "{}",
            output_text(&s)
        );
    }

    /// The review's repro: debugpy checks breakpoint lines against the file on
    /// disk. After two lines were typed at the top of the unsaved editor, the IDE
    /// shifts its breakpoints 4 → 6 and 7 → 9; on disk those are a blank line and
    /// past the end, so debugpy answers 4 and 8. The session reports that but keeps
    /// the IDE's lines (it used to move them, and the IDE saved the wrong lines),
    /// and a hit still names the IDE's line.
    #[test]
    fn unsaved_edits_keep_the_ides_breakpoint_lines() {
        let Some(py) = python_with_debugpy() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let code =
            "def f():\n    a = 1\n    b = 2\n    return a + b\n\n\nprint(f())\nprint('end')\n";
        let (mut s, script) = launch_script(11, &py, dir.path(), "edit.py", code, "", |p| {
            setup(p, vec![Breakpoint::new(4), Breakpoint::new(7)])
        });
        assert!(
            wait_until(&mut s, |s| s.is_stopped() && top_line(s) == Some(7), 30),
            "state={:?} output={}",
            s.state,
            output_text(&s)
        );
        assert!(s.take_moved_breakpoints().is_empty());
        let mut temporary = Breakpoint::new(6);
        temporary.temporary = true;
        s.set_breakpoints(&script, vec![temporary, Breakpoint::new(9)]);
        assert!(wait_until(&mut s, |s| s.bp_requests_in_flight == 0, 10));
        assert_eq!(
            s.take_moved_breakpoints(),
            vec![(script.clone(), 6, 4), (script.clone(), 9, 8)],
            "what debugpy made of the lines on disk"
        );
        assert_eq!(
            s.breakpoints[&script]
                .iter()
                .map(|b| (b.line, b.verified))
                .collect::<Vec<_>>(),
            vec![(6, true), (9, true)],
            "the IDE's lines stay"
        );
        assert_eq!(
            (s.is_verified(&script, 6), s.is_verified(&script, 4)),
            (Some(true), None)
        );
        // debugpy stops at 4 for the IDE's line 6: the temporary breakpoint is removed there.
        s.continue_();
        assert!(
            wait_until(
                &mut s,
                |s| s.is_stopped() && top_line(s) == Some(4) && !s.removed_breakpoints.is_empty(),
                15
            ),
            "{:?} {:?}",
            s.state,
            s.frames.first()
        );
        assert_eq!(s.take_removed_breakpoints(), vec![(script.clone(), 6)]);
        assert_eq!(
            s.breakpoints[&script]
                .iter()
                .map(|b| b.line)
                .collect::<Vec<_>>(),
            vec![9]
        );
        s.stop();
        assert!(wait_until(&mut s, |s| !s.is_active(), 10), "{:?}", s.state);
    }

    #[test]
    fn python_reprs_are_unquoted() {
        assert_eq!(unquote_python_str("'it\\'s\\n'").as_deref(), Some("it's\n"));
        assert_eq!(unquote_python_str("\"a\"").as_deref(), Some("a"));
        assert_eq!(unquote_python_str("42"), None);
    }
}

#[cfg(test)]
mod port_handoff_tests;

#[cfg(test)]
mod bootstrap_tests;

mod protocol_requests;

mod backend_requests;
