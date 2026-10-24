//! Executing notebook cells on a Python Console session (`rustpy-console`).
//!
//! The console's REPL rules do not fit notebook cells (a cell holding only a `def` would wait
//! for more input, a statement right after an indented block is a syntax error, and result reprs
//! are flattened to one line). [`CellRunner`] therefore installs a small helper
//! (`kernel_shim.py`) through the console's startup code and sends each cell as
//! `__rustpy_nb__.run(count, source)`. The helper compiles the cell as a module the way IPython
//! does, shows the full repr (plus rich `_repr_*_` formats) of a trailing expression unless the
//! cell ends with `;`, provides `display()`, captures open matplotlib figures, emulates common
//! IPython magics (`!command`, `%pip`, `%cd`, `%pwd`, `%env`, `%time`, `%%time`, `%%writefile`,
//! `%%bash`, `%%sh`, `%%html`/`%%markdown`/`%%latex`/`%%svg`/`%%javascript`; `%matplotlib`,
//! `%config` and autoreload are accepted as no-ops; others are a `UsageError`), makes
//! `sys.modules["__main__"]` the user namespace (so cell-defined objects pickle) and reports
//! errors without its own frames. Results, displays and errors travel back as marker lines on the
//! console's stdout stream; plain stdout/stderr are routed to the running cell in the order they
//! arrive.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::session::{Connection, Session};
use rustpy_console::ConsoleEvent;
use serde::Deserialize;

use crate::model::{random_u64, Cell, CellKind, JsonMap, MimeBundle, Notebook, Output};

/// The helper installed into the console process.
const KERNEL_SHIM: &str = include_str!("kernel_shim.py");

/// State of the Python process behind a [`CellRunner`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelState {
    /// Launched; the interpreter has not reported in yet.
    Starting,
    /// Ready and not executing anything.
    Idle,
    /// Executing a cell.
    Busy,
    /// The process exited or failed; the next run restarts it.
    Dead,
}

/// Execution state of a cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellStatus {
    Idle,
    /// Waiting for earlier cells to finish.
    Queued,
    Running,
}

struct Queued {
    id: String,
    source: String,
}

struct Running {
    id: String,
    request: u64,
    count: u32,
    started: Instant,
    /// The helper reported an error for this cell.
    failed: bool,
}

/// A message from the kernel helper (`##rustpy-nb-<token>## {json}` lines on stdout).
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum KernelMessage {
    ExecuteResult {
        #[serde(default)]
        execution_count: Option<u32>,
        data: MimeBundle,
        #[serde(default)]
        metadata: JsonMap,
    },
    DisplayData {
        #[serde(default)]
        transient: JsonMap,
        data: MimeBundle,
        #[serde(default)]
        metadata: JsonMap,
    },
    UpdateDisplayData {
        data: MimeBundle,
        #[serde(default)]
        metadata: JsonMap,
        #[serde(default)]
        transient: JsonMap,
    },
    ClearOutput {
        #[serde(default)]
        wait: bool,
    },
    CommOpen {
        comm_id: String,
        data: serde_json::Value,
    },
    CommMsg {
        comm_id: String,
        data: serde_json::Value,
    },
    CommClose {
        comm_id: String,
    },
    KernelSpecs {
        names: Vec<String>,
    },
    ServerInfo {
        url: String,
        token: String,
    },
    Resources {
        memory_bytes: u64,
        cpu_seconds: f64,
    },

    Error {
        ename: String,
        evalue: String,
        traceback: Vec<String>,
    },
}

/// Runs the code cells of a notebook, one at a time and in the order they were queued, on a
/// Python process ("kernel"). Cells are tracked by their id ([`Notebook::ensure_cell_ids`] is
/// applied when cells are queued), so cells may be edited, inserted, moved or deleted while
/// others run; the source a cell had when it was queued is what runs.
///
/// Like Jupyter, queueing clears a cell's outputs, execution counts restart at 1 with each
/// kernel, and an error (including an interrupt) cancels the cells still waiting. Nothing blocks
/// (except [`restart`](Self::restart) / [`shutdown`](Self::shutdown), which give a busy process
/// up to half a second to exit before killing it): call [`CellRunner::poll`] regularly (e.g.
/// every frame) to collect output.
pub struct CellRunner {
    session: Session,
    /// Prefix of helper messages on stdout (carries a random per-runner token).
    marker: String,
    queue: VecDeque<Queued>,
    current: Option<Running>,
    counter: u32,
    ready: bool,
    dead: bool,
    version: Option<String>,
    executable: Option<String>,
    /// The cell that ran last; output arriving between cells (background threads) goes there.
    last_cell: Option<String>,
    durations: HashMap<String, Duration>,
    /// Ids of cells changed since the last `poll`.
    changed: Vec<String>,
    /// Completion/variable/inspection answers and output outside cells, for
    /// [`CellRunner::take_events`].
    events: Vec<ConsoleEvent>,
    debug_pending: Option<(u64, Queued)>,
    pub widgets: HashMap<String, serde_json::Value>,
    pub kernels: Vec<String>,
    pub server_info: Option<(String, String)>,
    pub resources: Option<(u64, f64)>,
    pub debug_sources: HashMap<String, String>,
    debug_generation: u64,
    pending_queries: HashSet<u64>,
    displays: HashMap<String, (String, usize)>,
    output_limit: usize,
    clear_wait: HashSet<String>,
}

impl CellRunner {
    /// Starts a kernel: `interpreter` running the console helper in `cwd` with `env` added to the
    /// environment, after `startup_code` (may be empty) ran silently. Unless `env` or the IDE's
    /// environment sets `MPLBACKEND`, matplotlib uses the non-interactive Agg backend and figures
    /// left open by a cell are shown as PNG outputs.
    pub fn start(
        interpreter: &Path,
        cwd: &Path,
        env: &[(String, String)],
        startup_code: &str,
    ) -> anyhow::Result<CellRunner> {
        Self::start_with_connection(interpreter, cwd, env, startup_code, Connection::embedded())
    }
    pub fn start_with_connection(
        interpreter: &Path,
        cwd: &Path,
        env: &[(String, String)],
        startup_code: &str,
        connection: Connection,
    ) -> anyhow::Result<CellRunner> {
        let token = format!("{:016x}", random_u64());
        let mut env = env.to_vec();
        if !env.iter().any(|(key, _)| key == "MPLBACKEND")
            && std::env::var_os("MPLBACKEND").is_none()
        {
            env.push(("MPLBACKEND".into(), "Agg".into()));
        }
        let session = Session::start(
            interpreter,
            cwd,
            &env,
            &startup(&token, startup_code),
            connection,
            format!("##rustpy-nb-{token}## "),
        )?;
        Ok(CellRunner {
            session,
            marker: format!("##rustpy-nb-{token}## "),
            queue: VecDeque::new(),
            current: None,
            counter: 0,
            ready: false,
            dead: false,
            version: None,
            executable: None,
            last_cell: None,
            durations: HashMap::new(),
            changed: Vec::new(),
            events: Vec::new(),
            debug_pending: None,
            widgets: HashMap::new(),
            kernels: Vec::new(),
            server_info: None,
            resources: None,
            debug_sources: HashMap::new(),
            debug_generation: 0,
            pending_queries: HashSet::new(),
            displays: HashMap::new(),
            output_limit: 4 * 1024 * 1024,
            clear_wait: HashSet::new(),
        })
    }

    /// Queues the code cells among `indices` (in that order) and returns the indices queued.
    /// Their outputs and execution counts are cleared; empty code cells are cleared but not run,
    /// and other cell kinds are skipped. A dead kernel is restarted first.
    pub fn run_cells(
        &mut self,
        nb: &mut Notebook,
        indices: impl IntoIterator<Item = usize>,
    ) -> Vec<usize> {
        if self.dead {
            self.restart();
        }
        nb.ensure_cell_ids();
        let mut queued = Vec::new();
        for index in indices {
            let Some(cell) = nb.cells.get_mut(index) else {
                continue;
            };
            let Some(id) = cell.id.clone().filter(|_| cell.is_code()) else {
                continue;
            };
            cell.clear_outputs();
            self.durations.remove(&id);
            self.changed.push(id.clone());
            if cell.source.trim().is_empty() {
                continue; // like Jupyter, empty cells are not sent to the kernel
            }
            self.queue.push_back(Queued {
                id,
                source: cell.source.clone(),
            });
            queued.push(index);
        }
        self.start_next(nb);
        queued
    }

    pub fn debug_source_path(&self, id: &str) -> String {
        format!(
            "<debug-cell-{}-{}-{id}>",
            self.marker
                .trim_start_matches("##rustpy-nb-")
                .trim_end_matches("## "),
            self.debug_generation
        )
    }
    /// Debug one cell in the same kernel namespace. Execution starts only after
    /// debugpy has supplied an endpoint, and waits for the IDE attach handshake.
    pub fn debug_cell(&mut self, nb: &mut Notebook, index: usize) -> bool {
        if self.is_busy() || self.dead || self.debug_pending.is_some() {
            return false;
        }
        nb.ensure_cell_ids();
        let Some(cell) = nb
            .cells
            .get_mut(index)
            .filter(|cell| cell.is_code() && !cell.source.trim().is_empty())
        else {
            return false;
        };
        let Some(id) = cell.id.clone() else {
            return false;
        };
        let path = self.debug_source_path(&id);
        if self.debug_sources.len() >= 16 {
            self.debug_sources.clear();
        }
        self.debug_sources.insert(path.clone(), cell.source.clone());
        let filename = serde_json::to_string(&path).unwrap();
        let source=format!("import debugpy as _rustpy_debugpy\nimport linecache as _rustpy_debug_linecache\n_rustpy_debug_source = {}\n_rustpy_debug_linecache.cache[{filename}] = (len(_rustpy_debug_source), None, _rustpy_debug_source.splitlines(True), {filename})\n_rustpy_debugpy.wait_for_client()\n_rustpy_debugpy.breakpoint()\nexec(compile(_rustpy_debug_source, {filename}, 'exec'), globals())",serde_json::to_string(&cell.source).unwrap());

        cell.clear_outputs();
        self.changed.push(id.clone());
        let request = self.session.request_debugger();
        self.debug_pending = Some((request, Queued { id, source }));
        true
    }

    /// Queues one cell; false when it is not a non-empty code cell.
    pub fn run_cell(&mut self, nb: &mut Notebook, index: usize) -> bool {
        !self.run_cells(nb, [index]).is_empty()
    }

    /// Queues every code cell, top to bottom.
    pub fn run_all(&mut self, nb: &mut Notebook) -> Vec<usize> {
        let count = nb.cells.len();
        self.run_cells(nb, 0..count)
    }

    /// Queues the code cells above `index` (not including it).
    pub fn run_above(&mut self, nb: &mut Notebook, index: usize) -> Vec<usize> {
        let end = index.min(nb.cells.len());
        self.run_cells(nb, 0..end)
    }

    /// Queues the code cell at `index` and every code cell below it.
    pub fn run_below(&mut self, nb: &mut Notebook, index: usize) -> Vec<usize> {
        let count = nb.cells.len();
        self.run_cells(nb, index..count)
    }

    /// Non-blocking: applies everything the kernel produced since the last call to the cells of
    /// `nb`, starts the next queued cell when one finished, and returns the indices (sorted) of the
    /// cells whose outputs, execution count or status changed since the last call.
    pub fn poll(&mut self, nb: &mut Notebook) -> Vec<usize> {
        // Everything in this batch was produced before the next cell is sent below, so output
        // arriving after a cell's result (background threads) stays with that cell.
        for event in self.session.poll() {
            self.handle(nb, event);
        }
        self.start_next(nb);
        let changed: HashSet<String> = std::mem::take(&mut self.changed).into_iter().collect();
        if changed.is_empty() {
            return Vec::new();
        }
        nb.cells
            .iter()
            .enumerate()
            .filter(|(_, c)| c.id.as_ref().is_some_and(|id| changed.contains(id)))
            .map(|(i, _)| i)
            .collect()
    }

    /// Polls until no cell is running or queued, or `timeout` elapses; returns whether the queue
    /// drained. Blocks the calling thread — meant for scripts and tests, not for UI threads.
    pub fn wait_idle(&mut self, nb: &mut Notebook, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            self.poll(nb);
            if !self.is_busy() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Interrupts the running cell (`KeyboardInterrupt`; Unix only) and cancels the queued ones.
    pub fn interrupt(&mut self) {
        self.cancel_queued();
        if self.current.is_some() || !self.pending_queries.is_empty() {
            self.session.interrupt();
        }
    }

    /// Cancels the queued cells; the running one continues.
    pub fn cancel_queued(&mut self) {
        if let Some((_, queued)) = self.debug_pending.take() {
            self.changed.push(queued.id);
        }
        let cancelled: Vec<String> = self.queue.drain(..).map(|q| q.id).collect();
        self.changed.extend(cancelled);
    }

    /// Replaces the kernel with a fresh process: variables are lost, queued cells are cancelled,
    /// the running cell stops (keeping the output it produced, without an execution count) and
    /// execution counts start again at 1.
    pub fn restart(&mut self) {
        if !self.session.restart() {
            return;
        }
        self.cancel_queued();
        if let Some(run) = self.current.take() {
            self.changed.push(run.id);
        }
        self.counter = 0;
        self.ready = false;
        self.dead = false;
        self.version = None;
        self.executable = None;
        self.last_cell = None;
        self.events.clear();
        self.widgets.clear();
        self.displays.clear();
        self.clear_wait.clear();
        self.resources = None;
        self.debug_generation = self.debug_generation.saturating_add(1);
        self.pending_queries.clear();
    }

    /// Stops the kernel process (the next run starts a new one).
    pub fn shutdown(&mut self) {
        self.cancel_queued();
        if let Some(run) = self.current.take() {
            self.changed.push(run.id);
        }
        self.session.stop();
        self.pending_queries.clear();
        self.dead = true;
        self.ready = false;
    }

    pub fn owns_process(&self) -> bool {
        self.session.owns_process()
    }
    pub fn state(&self) -> KernelState {
        if self.dead {
            KernelState::Dead
        } else if !self.ready {
            KernelState::Starting
        } else if self.debug_pending.is_some()
            || self.current.is_some()
            || !self.pending_queries.is_empty()
        {
            KernelState::Busy
        } else {
            KernelState::Idle
        }
    }

    /// Whether a cell is running or queued.
    pub fn is_busy(&self) -> bool {
        self.debug_pending.is_some()
            || self.current.is_some()
            || !self.queue.is_empty()
            || !self.pending_queries.is_empty()
    }

    pub fn cell_status(&self, cell: &Cell) -> CellStatus {
        let Some(id) = cell.id.as_deref() else {
            return CellStatus::Idle;
        };
        if self.current.as_ref().is_some_and(|run| run.id == id) {
            CellStatus::Running
        } else if self
            .debug_pending
            .as_ref()
            .is_some_and(|(_, queued)| queued.id == id)
            || self.queue.iter().any(|q| q.id == id)
        {
            CellStatus::Queued
        } else {
            CellStatus::Idle
        }
    }

    /// Index of the running cell in `nb`.
    pub fn running_cell(&self, nb: &Notebook) -> Option<usize> {
        nb.cell_index(&self.current.as_ref()?.id)
    }

    /// How long the cell has been running, or how long its last run took (this kernel or an earlier one).
    pub fn cell_duration(&self, cell: &Cell) -> Option<Duration> {
        let id = cell.id.as_deref()?;
        match &self.current {
            Some(run) if run.id == id => Some(run.started.elapsed()),
            _ => self.durations.get(id).copied(),
        }
    }

    /// The last execution count handed out by this kernel (0 before the first cell).
    pub fn execution_count(&self) -> u32 {
        self.counter
    }

    /// Python version reported by the kernel (`None` until it started).
    pub fn kernel_version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    /// `sys.executable` of the kernel (`None` until it started).
    pub fn kernel_executable(&self) -> Option<&str> {
        self.executable.as_deref()
    }

    /// Requests a snapshot of the kernel's variables (answered by a [`ConsoleEvent::Vars`] in
    /// [`CellRunner::take_events`] once the running cells finished).
    pub fn request_vars(&mut self) -> u64 {
        let id = self.session.request_vars();
        self.pending_queries.insert(id);
        id
    }

    /// Requests completions at byte `cursor` of `code` ([`ConsoleEvent::Completions`]).
    pub fn complete(&mut self, code: &str, cursor: usize) -> u64 {
        let id = self.session.complete(code, cursor);
        self.pending_queries.insert(id);
        id
    }

    /// Requests the children of the value of `expr` ([`ConsoleEvent::Children`]).
    pub fn inspect(&mut self, expr: &str) -> u64 {
        let id = self.session.inspect(expr);
        self.pending_queries.insert(id);
        id
    }

    pub fn request_data(&mut self, name: &str, row: usize, col: usize) -> u64 {
        let id = self.session.data(name, row, col);
        self.pending_queries.insert(id);
        id
    }
    pub fn widget_update(&mut self, id: &str, state: serde_json::Value) {
        self.session.widget(id, state)
    }
    pub fn widget_event(&mut self, id: &str, event: serde_json::Value) {
        self.session.widget_event(id, event)
    }
    /// Answers to [`request_vars`](Self::request_vars), [`complete`](Self::complete) and
    /// [`inspect`](Self::inspect) received by [`poll`](Self::poll), plus kernel output that
    /// belongs to no cell ([`ConsoleEvent::Stdout`] / [`ConsoleEvent::Stderr`], e.g. an error in
    /// the startup code before any cell ran) for a kernel log.
    pub fn take_events(&mut self) -> Vec<ConsoleEvent> {
        std::mem::take(&mut self.events)
    }

    fn start_next(&mut self, nb: &mut Notebook) {
        if !self.ready || self.dead {
            return;
        }
        while self.current.is_none() {
            let Some(next) = self.queue.pop_front() else {
                return;
            };
            let Some(cell) = nb
                .cells
                .iter_mut()
                .find(|c| c.id.as_deref() == Some(next.id.as_str()))
            else {
                continue; // deleted while queued
            };
            self.changed.push(next.id.clone());
            if cell.kind != CellKind::Code {
                continue; // converted to markdown/raw while queued
            }
            cell.clear_outputs();
            self.counter += 1;
            let request = self.session.exec_cell(self.counter, &next.source, &next.id);
            self.current = Some(Running {
                id: next.id,
                request,
                count: self.counter,
                started: Instant::now(),
                failed: false,
            });
        }
    }

    fn handle(&mut self, nb: &mut Notebook, event: ConsoleEvent) {
        match event {
            ConsoleEvent::Stdout(text) => self.on_stdout(nb, &text),
            ConsoleEvent::Stderr(text) => self.route_stream(nb, "stderr", &text),
            ConsoleEvent::Result {
                id, error, value, ..
            } => {
                self.pending_queries.remove(&id);
                if self.current.as_ref().is_some_and(|run| run.request == id) {
                    if let Some(count) = value.as_ref().and_then(|value| value.parse::<u32>().ok())
                    {
                        if let Some(current) = &mut self.current {
                            current.count = count;
                        }
                        self.counter = self.counter.max(count);
                    }
                    self.finish_current(nb, error);
                }
            }
            ConsoleEvent::Ready {
                version,
                executable,
            } => {
                self.ready = true;
                self.version = Some(version);
                self.executable = Some(executable);
            }
            ConsoleEvent::Exited(code) => {
                self.kernel_died(nb, &format!("the kernel process exited with code {code}"))
            }
            ConsoleEvent::Failed(message) => self.kernel_died(nb, &message),
            event @ (ConsoleEvent::Vars { id, .. }
            | ConsoleEvent::Children { id, .. }
            | ConsoleEvent::Completions { id, .. }
            | ConsoleEvent::Data { id, .. }) => {
                self.pending_queries.remove(&id);
                self.events.push(event);
            }
            event @ ConsoleEvent::Debugger { .. } => {
                if let ConsoleEvent::Debugger { id, error, .. } = &event {
                    if !self
                        .debug_pending
                        .as_ref()
                        .is_some_and(|(request, queued)| {
                            request == id
                                && nb.cells.iter().any(|cell| {
                                    cell.id.as_ref() == Some(&queued.id) && cell.is_code()
                                })
                        })
                    {
                        self.debug_pending = None;
                        return;
                    }

                    if self
                        .debug_pending
                        .as_ref()
                        .is_some_and(|(request, _)| request == id)
                    {
                        if let Some((_, queued)) = self.debug_pending.take() {
                            if error.is_none() {
                                self.queue.push_back(queued);
                            } else {
                                self.route_stream(
                                    nb,
                                    "stderr",
                                    &format!(
                                        "Cannot debug cell: {}\n",
                                        error.as_deref().unwrap_or("unknown error")
                                    ),
                                );
                            }
                        }
                    }
                }
                self.events.push(event);
            }
            event => self.events.push(event),
        }
    }

    fn on_stdout(&mut self, nb: &mut Notebook, text: &str) {
        for piece in split_messages(text, &self.marker) {
            match piece {
                Piece::Text(text) => self.route_stream(nb, "stdout", text),
                Piece::Message(json) => self.on_message(nb, json),
            }
        }
    }

    pub fn set_output_limit(&mut self, limit: usize) {
        self.output_limit = limit.clamp(1024, 64 * 1024 * 1024);
    }
    fn on_message(&mut self, nb: &mut Notebook, json: &str) {
        if json.len() > self.output_limit {
            self.route_stream(
                nb,
                "stderr",
                "[Rich output exceeds notebook output limit]\n",
            );
            return;
        }

        let message = match serde_json::from_str::<KernelMessage>(json) {
            Ok(message) => message,
            Err(e) => {
                log::warn!("notebook kernel: malformed message ({e}): {json:.200}");
                return;
            }
        };
        if matches!(
            &message,
            KernelMessage::ExecuteResult { .. }
                | KernelMessage::DisplayData { .. }
                | KernelMessage::Error { .. }
        ) {
            self.flush_clear(nb);
        }
        let mut display_id = None;
        let mut output_owner = None;
        let output = match message {
            KernelMessage::Resources {
                memory_bytes,
                cpu_seconds,
            } => {
                self.resources = Some((memory_bytes, cpu_seconds));
                return;
            }
            KernelMessage::KernelSpecs { names } => {
                self.kernels = names;
                return;
            }
            KernelMessage::ServerInfo { url, token } => {
                self.server_info = Some((url, token));
                return;
            }
            KernelMessage::ClearOutput { wait } => {
                if wait {
                    if let Some(id) = self
                        .current
                        .as_ref()
                        .map(|current| current.id.clone())
                        .or_else(|| self.last_cell.clone())
                    {
                        self.clear_wait.insert(id);
                    }
                } else if let Some(cell) = self.output_cell(nb) {
                    cell.outputs.clear();
                }
                return;
            }
            KernelMessage::CommOpen { comm_id, data }
            | KernelMessage::CommMsg { comm_id, data } => {
                if data.to_string().len() > 1024 * 1024 {
                    return;
                }
                if self.widgets.len() >= 256 && !self.widgets.contains_key(&comm_id) {
                    return;
                }
                for cell in &nb.cells {
                    if cell.outputs.iter().any(|output| match output {
                        Output::DisplayData { data, .. } | Output::ExecuteResult { data, .. } => {
                            data.get("application/vnd.jupyter.widget-view+json")
                                .is_some_and(|text| text.contains(&comm_id))
                        }
                        _ => false,
                    }) {
                        if let Some(id) = &cell.id {
                            self.changed.push(id.clone());
                        }
                    }
                }
                let state = data.get("state").cloned().unwrap_or_default();
                let current = self
                    .widgets
                    .entry(comm_id)
                    .or_insert_with(|| serde_json::json!({}));
                if let (Some(current), Some(state)) = (current.as_object_mut(), state.as_object()) {
                    current.extend(state.clone());
                }
                return;
            }
            KernelMessage::CommClose { comm_id } => {
                self.widgets.remove(&comm_id);
                return;
            }
            KernelMessage::UpdateDisplayData {
                data,
                metadata,
                transient,
            } => {
                if let Some((id, index)) = transient
                    .get("display_id")
                    .and_then(|value| value.as_str())
                    .and_then(|id| self.displays.get(id))
                    .cloned()
                {
                    if let Some(cell) = nb
                        .cells
                        .iter_mut()
                        .find(|cell| cell.id.as_ref() == Some(&id))
                    {
                        if let Some(output) = cell.outputs.get_mut(index) {
                            *output = Output::DisplayData {
                                data,
                                metadata,
                                extra: JsonMap::new(),
                            };
                            self.changed.push(id);
                        }
                    }
                }
                return;
            }

            KernelMessage::ExecuteResult {
                data,
                metadata,
                execution_count,
            } => {
                let execution_count =
                    execution_count.or_else(|| self.current.as_ref().map(|run| run.count));
                Output::ExecuteResult {
                    execution_count,
                    data,
                    metadata,
                    extra: JsonMap::new(),
                }
            }
            KernelMessage::DisplayData {
                data,
                metadata,
                transient,
            } => {
                output_owner = metadata
                    .get("rustpy_cell_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned);
                display_id = transient
                    .get("display_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned);
                Output::DisplayData {
                    data,
                    metadata,
                    extra: JsonMap::new(),
                }
            }
            KernelMessage::Error {
                ename,
                evalue,
                traceback,
            } => {
                if let Some(run) = &mut self.current {
                    run.failed = true;
                }
                Output::error(ename, evalue, traceback)
            }
        };
        let output_limit = self.output_limit;
        let location = if let Some(cell) = if let Some(id) = output_owner {
            self.changed.push(id.clone());
            nb.cells
                .iter_mut()
                .find(|cell| cell.id.as_ref() == Some(&id))
        } else {
            self.output_cell(nb)
        } {
            let location = cell.id.clone().map(|id| (id, cell.outputs.len()));
            let used = cell.outputs.iter().map(output_size).sum::<usize>();
            if used.saturating_add(output_size(&output)) <= output_limit
                && cell.outputs.len() < 1000
            {
                cell.outputs.push(output);
            }
            location
        } else {
            None
        };
        if let Some((display, location)) = display_id.zip(location) {
            self.displays.insert(display, location);
        }
    }

    fn flush_clear(&mut self, nb: &mut Notebook) {
        if let Some(id) = self
            .current
            .as_ref()
            .map(|current| current.id.clone())
            .or_else(|| self.last_cell.clone())
        {
            if self.clear_wait.remove(&id) {
                if let Some(cell) = nb
                    .cells
                    .iter_mut()
                    .find(|cell| cell.id.as_ref() == Some(&id))
                {
                    cell.outputs.clear();
                    self.changed.push(id);
                }
            }
        }
    }
    fn route_stream(&mut self, nb: &mut Notebook, name: &str, text: &str) {
        if text.is_empty() {
            return;
        }
        self.flush_clear(nb);
        let limit = self.output_limit;
        match self.output_cell(nb) {
            Some(cell) => {
                let used = cell
                    .outputs
                    .iter()
                    .map(|output| match output {
                        Output::Stream { text, .. } => text.len(),
                        _ => 0,
                    })
                    .sum::<usize>();
                let available = limit.saturating_sub(used);
                let mut end = text.len().min(available);
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                if end > 0 {
                    cell.append_stream(name, &text[..end]);
                }
            }
            None if name == "stderr" => self.events.push(ConsoleEvent::Stderr(text.to_owned())),
            None => self.events.push(ConsoleEvent::Stdout(text.to_owned())),
        }
    }

    /// The cell output goes to (the running cell, else the last one that ran), marked as changed.
    fn output_cell<'a>(&mut self, nb: &'a mut Notebook) -> Option<&'a mut Cell> {
        let id = self
            .current
            .as_ref()
            .map(|run| run.id.clone())
            .or_else(|| self.last_cell.clone())?;
        let cell = nb
            .cells
            .iter_mut()
            .find(|c| c.is_code() && c.id.as_deref() == Some(id.as_str()))?;
        if self.changed.last() != Some(&id) {
            self.changed.push(id); // (streams arrive in many chunks)
        }
        Some(cell)
    }

    /// The console finished executing the running cell; `error` is set when the exception
    /// escaped the helper (e.g. an interrupt between two statements of the helper itself).
    fn finish_current(&mut self, nb: &mut Notebook, error: Option<String>) {
        let Some(run) = self.current.take() else {
            return;
        };
        if let Some(cell) = nb
            .cells
            .iter_mut()
            .find(|c| c.is_code() && c.id.as_deref() == Some(run.id.as_str()))
        {
            if let Some(error) = &error {
                // The console also echoed the error on stderr.
                strip_stderr_echo(cell, error);
                if !run.failed {
                    cell.outputs.push(error_from_text(error));
                }
            }
            cell.execution_count = Some(run.count);
        }
        if run.failed || error.is_some() {
            self.cancel_queued();
        }
        self.durations.insert(run.id.clone(), run.started.elapsed());
        self.changed.push(run.id.clone());
        self.last_cell = Some(run.id);
    }

    fn kernel_died(&mut self, nb: &mut Notebook, message: &str) {
        let first = !self.dead;
        self.dead = true;
        self.ready = false;
        if let Some(run) = self.current.take() {
            if let Some(cell) = nb
                .cells
                .iter_mut()
                .find(|c| c.is_code() && c.id.as_deref() == Some(run.id.as_str()))
            {
                cell.outputs.push(Output::error(
                    "DeadKernelError",
                    message,
                    vec![format!("DeadKernelError: {message}")],
                ));
                cell.execution_count = Some(run.count);
            }
            self.durations.insert(run.id.clone(), run.started.elapsed());
            self.changed.push(run.id.clone());
            self.last_cell = Some(run.id);
        }
        self.cancel_queued();
        self.pending_queries.clear();
        if first {
            log::info!("notebook kernel stopped: {message}");
        }
    }
}

fn output_size(output: &Output) -> usize {
    match output {
        Output::Stream { text, .. } => text.len(),
        Output::DisplayData { data, .. } | Output::ExecuteResult { data, .. } => {
            data.values().map(String::len).sum()
        }
        Output::Error {
            ename,
            evalue,
            traceback,
            ..
        } => ename.len() + evalue.len() + traceback.iter().map(String::len).sum::<usize>(),
        Output::Unknown(value) => serde_json::to_vec(value)
            .map(|encoded| encoded.len())
            .unwrap_or(usize::MAX),
    }
}

/// The console startup code: installs the helper (in its own namespace, so only
/// `__rustpy_nb__` and the builtin `display` become visible), then runs the user's startup
/// code. Both run through `exec(compile(...))` so the console's REPL rules do not apply.
fn startup(token: &str, user_code: &str) -> String {
    let mut code = format!(
        "exec(compile({}, '<rustpy-notebook>', 'exec'), {{'__name__': '__rustpy_nb__', '_USER_NS': globals(), '_TOKEN': '{token}'}})\n",
        python_str(KERNEL_SHIM)
    );
    if !user_code.trim().is_empty() {
        code.push_str(&format!(
            "exec(compile({}, '<startup>', 'exec'), globals())\n",
            python_str(user_code)
        ));
    }
    code
}

/// A part of the console's stdout text.
#[derive(Debug, PartialEq)]
enum Piece<'a> {
    /// Output of the cell.
    Text(&'a str),
    /// The JSON of a helper message.
    Message(&'a str),
}

/// Splits helper messages (`marker`, JSON, newline) out of stdout text. The helper writes each
/// message with a single write after flushing the streams, so a message never spans two chunks,
/// but output of other threads may precede or follow it within one.
fn split_messages<'a>(mut text: &'a str, marker: &str) -> Vec<Piece<'a>> {
    let mut pieces = Vec::new();
    while let Some(pos) = text.find(marker) {
        if pos > 0 {
            pieces.push(Piece::Text(&text[..pos]));
        }
        let message = &text[pos + marker.len()..];
        let (json, rest) = message.split_once('\n').unwrap_or((message, ""));
        pieces.push(Piece::Message(json));
        text = rest;
    }
    if !text.is_empty() {
        pieces.push(Piece::Text(text));
    }
    pieces
}

/// A Python string literal for `text` (serde_json's string escapes are valid Python escapes).
fn python_str(text: &str) -> String {
    serde_json::Value::from(text).to_string()
}

/// Removes the console's stderr echo of `error` from the end of the cell's last stderr output.
fn strip_stderr_echo(cell: &mut Cell, error: &str) {
    let Some(pos) = cell
        .outputs
        .iter()
        .rposition(|o| matches!(o, Output::Stream { name, .. } if name == "stderr"))
    else {
        return;
    };
    let Output::Stream { text, .. } = &mut cell.outputs[pos] else {
        return;
    };
    let echo = [format!("{error}\n"), error.to_owned()]
        .into_iter()
        .find(|echo| text.ends_with(echo.as_str()));
    if let Some(echo) = echo {
        text.truncate(text.len() - echo.len());
        if text.is_empty() {
            cell.outputs.remove(pos);
        }
    }
}

/// An error output from a traceback the console reported as text (`ename` / `evalue` are taken
/// from the exception line of the last traceback).
fn error_from_text(text: &str) -> Output {
    let lines: Vec<&str> = text.trim_end_matches('\n').split('\n').collect();
    let start = lines
        .iter()
        .rposition(|l| l.starts_with("Traceback (most recent call last)"))
        .map_or(0, |i| i + 1);
    let exception_line = (start..lines.len())
        .find(|&i| !lines[i].trim().is_empty() && !lines[i].starts_with([' ', '\t']));
    let (ename, evalue) = match exception_line {
        Some(i) => {
            let line = lines[i].trim_end();
            let (name, first) = match line.split_once(':') {
                Some((name, rest)) if is_exception_name(name) => {
                    (name, rest.strip_prefix(' ').unwrap_or(rest))
                }
                _ if is_exception_name(line) => (line, ""),
                _ => ("Error", line),
            };
            let mut value = first.to_owned();
            for more in &lines[i + 1..] {
                value.push('\n');
                value.push_str(more);
            }
            (name.rsplit('.').next().unwrap_or(name).to_owned(), value)
        }
        None => ("Error".to_owned(), text.trim().to_owned()),
    };
    Output::error(
        ename,
        evalue,
        lines.into_iter().map(str::to_owned).collect(),
    )
}

fn is_exception_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit() || c == '.')
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_literals_round_trip_through_json_escapes() {
        assert_eq!(
            python_str("a\"b\\c\nd\té\u{1}"),
            "\"a\\\"b\\\\c\\nd\\té\\u0001\""
        );
    }

    #[test]
    fn helper_messages_are_split_out_of_stdout() {
        let marker = "##rustpy-nb-0123## ";
        assert_eq!(split_messages("plain\n", marker), [Piece::Text("plain\n")]);
        assert_eq!(split_messages("", marker), []);
        assert_eq!(
            split_messages("##rustpy-nb-0123## {\"a\":1}\n", marker),
            [Piece::Message("{\"a\":1}")]
        );
        assert_eq!(
            split_messages(
                "partial##rustpy-nb-0123## {}\n##rustpy-nb-0123## []\nafter",
                marker
            ),
            [
                Piece::Text("partial"),
                Piece::Message("{}"),
                Piece::Message("[]"),
                Piece::Text("after")
            ]
        );
        // Another runner's marker (or user text that looks alike) is ordinary output.
        assert_eq!(
            split_messages("##rustpy-nb-9999## {}\n", marker),
            [Piece::Text("##rustpy-nb-9999## {}\n")]
        );
    }

    #[test]
    fn startup_installs_shim_then_user_code() {
        let code = startup("abc", "import sys\nif True:\n    X = 1");
        let lines: Vec<&str> = code.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("exec(compile(\"") && lines[0].ends_with("'_TOKEN': 'abc'})"));
        assert_eq!(
            lines[1],
            "exec(compile(\"import sys\\nif True:\\n    X = 1\", '<startup>', 'exec'), globals())"
        );
        assert_eq!(startup("abc", "  ").lines().count(), 1);
    }

    #[test]
    fn errors_parsed_from_console_tracebacks() {
        let tb = "Traceback (most recent call last):\n  File \"<console>\", line 1, in <module>\nNameError: name '__rustpy_nb__' is not defined\n";
        let Output::Error {
            ename,
            evalue,
            traceback,
            ..
        } = error_from_text(tb)
        else {
            panic!()
        };
        assert_eq!(ename, "NameError");
        assert_eq!(evalue, "name '__rustpy_nb__' is not defined");
        assert_eq!(traceback.len(), 3);

        let Output::Error { ename, evalue, .. } = error_from_text("KeyboardInterrupt") else {
            panic!()
        };
        assert_eq!((ename.as_str(), evalue.as_str()), ("KeyboardInterrupt", ""));
        let Output::Error { ename, evalue, .. } = error_from_text("SystemExit: 3") else {
            panic!()
        };
        assert_eq!((ename.as_str(), evalue.as_str()), ("SystemExit", "3"));

        let chained = "Traceback (most recent call last):\n  File \"x.py\", line 1\nKeyError: 'k'\n\nDuring handling of the above exception, another exception occurred:\n\nTraceback (most recent call last):\n  File \"x.py\", line 3\njson.decoder.JSONDecodeError: Expecting value: line 1\nsecond line\n";
        let Output::Error { ename, evalue, .. } = error_from_text(chained) else {
            panic!()
        };
        assert_eq!(ename, "JSONDecodeError");
        assert_eq!(evalue, "Expecting value: line 1\nsecond line");

        let syntax =
            "  File \"<console>\", line 1\n    x = = 1\n        ^\nSyntaxError: invalid syntax\n";
        let Output::Error { ename, evalue, .. } = error_from_text(syntax) else {
            panic!()
        };
        assert_eq!(
            (ename.as_str(), evalue.as_str()),
            ("SyntaxError", "invalid syntax")
        );
    }

    #[test]
    fn stderr_echo_is_stripped() {
        let mut cell = Cell::code("");
        cell.append_stream("stderr", "warning\nKeyboardInterrupt\n");
        strip_stderr_echo(&mut cell, "KeyboardInterrupt");
        assert_eq!(cell.outputs, vec![Output::stream("stderr", "warning\n")]);

        let mut cell = Cell::code("");
        let tb = "Traceback (most recent call last):\nValueError: x\n";
        cell.append_stream("stderr", tb);
        cell.append_stream("stdout", "out\n");
        strip_stderr_echo(&mut cell, tb);
        assert_eq!(cell.outputs, vec![Output::stream("stdout", "out\n")]);

        let mut cell = Cell::code("");
        cell.append_stream("stderr", "unrelated\n");
        strip_stderr_echo(&mut cell, "KeyboardInterrupt");
        assert_eq!(cell.outputs, vec![Output::stream("stderr", "unrelated\n")]);
    }
}
#[cfg(test)]
mod jupyter_protocol_fixtures {
    use super::*;
    #[test]
    fn native_protocol_keeps_execution_counts_display_ids_and_widget_state() {
        let result:KernelMessage=serde_json::from_value(serde_json::json!({"type":"execute_result","execution_count":19,"data":{"text/plain":"42"}})).unwrap();
        assert!(matches!(
            result,
            KernelMessage::ExecuteResult {
                execution_count: Some(19),
                ..
            }
        ));
        let display:KernelMessage=serde_json::from_value(serde_json::json!({"type":"update_display_data","data":{"text/plain":"updated"},"transient":{"display_id":"display-1"},"metadata":{"rustpy_cell_id":"captured-cell"}})).unwrap();
        assert!(
            matches!(display,KernelMessage::UpdateDisplayData{transient,..}if transient["display_id"]=="display-1")
        );
        let widget:KernelMessage=serde_json::from_value(serde_json::json!({"type":"comm_msg","comm_id":"widget-1","data":{"method":"update","state":{"value":5}}})).unwrap();
        assert!(
            matches!(widget,KernelMessage::CommMsg{comm_id,data}if comm_id=="widget-1"&&data["state"]["value"]==5)
        );
        let clear: KernelMessage =
            serde_json::from_str(r#"{"type":"clear_output","wait":true}"#).unwrap();
        assert!(matches!(clear, KernelMessage::ClearOutput { wait: true }));
    }
}
