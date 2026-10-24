//! RustPy IDE — Python Console backend (roadmap §3 "Console backend protocol").
//!
//! A helper script (embedded, written to a temp directory on start) runs an
//! interactive Python session and talks to the IDE over JSON lines:
//!
//! * requests on the helper's stdin: `exec` (typed input, or a whole fragment
//!   sent from the editor), `runfile`, `complete`, `vars`, `inspect`,
//!   `startup`, `exit`;
//! * responses on its stdout as lines prefixed with `##rustpy-console## `
//!   (`stdout`/`stderr` chunks streamed during execution, `result`,
//!   `completions`, `vars`, `children`, `ready`).
//!
//! Lines without the prefix (user code writing to the raw file descriptors,
//! child processes) are forwarded as [`ConsoleEvent::Stdout`] /
//! [`ConsoleEvent::Stderr`].

mod history;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};

pub use history::History;

/// The embedded helper script.
pub const HELPER_SOURCE: &str = include_str!("console_helper.py");
pub const VALUE_ENGINE_SOURCE: &str = include_str!("console_values.py");
/// Shared bounded scientific adapter embedded directly into remote notebook kernels.
pub const SCIENTIFIC_TABLE_SOURCE: &str = rustpy_debug::scientific::TABLE_SOURCE;
pub mod debugger;
/// Prefix of protocol lines on the helper's stdout.
pub const PROTOCOL_PREFIX: &str = "##rustpy-console## ";

static HELPER_SEQ: AtomicU64 = AtomicU64::new(0);

/// A completion candidate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Completion {
    /// Text to insert for the last dotted segment (e.g. `join` for `str.jo`).
    pub name: String,
    /// `module` | `class` | `function` | `method` | `variable` | `attribute` | `keyword`.
    pub kind: String,
}

/// A variable (or a child of an inspected value).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VarInfo {
    /// Display name: the variable name, dict key repr, list index or attribute.
    pub name: String,
    /// Expression or opaque value handle (pass it to `inspect` to go deeper).
    /// Handles expire after execution or assignment; refresh variables then.
    #[serde(default)]
    pub expr: String,
    pub type_name: String,
    /// Short repr (truncated).
    pub value: String,
    #[serde(default)]
    pub is_callable: bool,
    #[serde(default)]
    pub is_module: bool,
    #[serde(default)]
    pub has_children: bool,
}

impl VarInfo {
    /// True for names the interpreter or IPython injects into the user namespace
    /// (`In`, `Out`, `exit`, `quit`, `get_ipython`, IPython's `open`) and for
    /// underscore names such as the `_ih`/`_oh`/`_i1`/`_1` history. Variable
    /// views hide these unless "Show special variables" is enabled.
    pub fn is_special(&self) -> bool {
        let callable = self.is_callable;
        self.name.starts_with('_')
            || match self.name.as_str() {
                "In" => self.type_name == "list",
                "Out" => self.type_name == "dict",
                "exit" | "quit" => {
                    matches!(self.type_name.as_str(), "ExitAutocall" | "Quitter")
                }
                "get_ipython" => callable,
                "open" => callable && self.type_name == "function",
                _ => false,
            }
    }
}

/// A bounded rectangular view using target-interpreter display text for cells.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DataPage {
    pub expression: String,
    pub kind: String,
    pub columns: Vec<String>,
    pub index: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub row_start: usize,
    pub col_start: usize,
    pub total_rows: usize,
    pub total_cols: usize,
    pub truncated: bool,
    #[serde(default)]
    pub dtype: String,
    #[serde(default)]
    pub shape: Vec<usize>,
    #[serde(default)]
    pub stats: Vec<serde_json::Value>,
}

/// Events produced by [`ConsoleSession::poll`].
#[derive(Clone, Debug, PartialEq)]
pub enum ConsoleEvent {
    /// Text written to stdout (streamed; may be partial lines).
    Stdout(String),
    /// Text written to stderr, including formatted tracebacks.
    Stderr(String),
    /// An `exec` request finished. `more` means the input is incomplete and the
    /// UI should keep collecting lines (`...` prompt) and resend the whole buffer.
    Result {
        id: u64,
        value: Option<String>,
        error: Option<String>,
        more: bool,
    },
    /// Answer to `complete`; `prefix` is the already-typed last segment.
    Completions {
        id: u64,
        items: Vec<Completion>,
        prefix: String,
    },
    /// Answer to `request_vars`.
    Vars { id: u64, items: Vec<VarInfo> },
    /// Answer to `inspect`; `error` is set when the expression cannot be evaluated.
    Children {
        id: u64,
        name: String,
        items: Vec<VarInfo>,
        error: Option<String>,
    },
    Page {
        id: u64,
        name: String,
        start: usize,
        total: usize,
        items: Vec<VarInfo>,
        error: Option<String>,
    },
    ValueSet {
        id: u64,
        name: String,
        error: Option<String>,
    },
    Data {
        id: u64,
        table: Option<DataPage>,
        error: Option<String>,
    },
    Plot {
        id: u64,
        mime: String,
        data_base64: String,
    },
    Configured {
        id: u64,
        ipython: bool,
        plots: bool,
        error: Option<String>,
    },
    Debugger {
        id: u64,
        host: String,
        port: u16,
        error: Option<String>,
    },
    /// The helper started (interpreter version and executable).
    Ready { version: String, executable: String },
    /// The helper process exited.
    Exited(i32),
    /// The session could not be used (spawn/IO failure).
    Failed(String),
}

#[derive(Debug)]
enum Raw {
    Stdout(String),
    Stderr(String),
    Protocol(serde_json::Value),
    StdoutClosed,
}

#[derive(Clone, Debug)]
struct StartParams {
    interpreter: PathBuf,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    startup_code: String,
    inherit_environment: bool,
    interpreter_options: Vec<String>,
}

/// An interactive Python session backed by the helper process.
pub struct ConsoleSession {
    params: StartParams,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    rx: Receiver<Raw>,
    next_id: u64,
    helper_path: PathBuf,
    pending: Vec<ConsoleEvent>,
    exited: bool,
    stdout_closed: bool,
    pub started: Instant,
}

impl ConsoleSession {
    /// Starts `interpreter` with the helper script in `cwd`; `startup_code` runs
    /// silently first (e.g. `import sys; sys.path.insert(0, "/project")`).
    pub fn start(
        interpreter: &Path,
        cwd: &Path,
        env: &[(String, String)],
        startup_code: &str,
    ) -> anyhow::Result<ConsoleSession> {
        Self::start_with_launch_options(interpreter, cwd, env, startup_code, true, &[])
    }
    pub fn start_with_launch_options(
        interpreter: &Path,
        cwd: &Path,
        env: &[(String, String)],
        startup_code: &str,
        inherit_environment: bool,
        interpreter_options: &[String],
    ) -> anyhow::Result<Self> {
        let params = StartParams {
            interpreter: interpreter.to_path_buf(),
            cwd: cwd.to_path_buf(),
            env: env.to_vec(),
            startup_code: startup_code.to_string(),
            inherit_environment,
            interpreter_options: interpreter_options.to_vec(),
        };
        Self::spawn(params)
    }
    pub fn launch_matches(&self, interpreter: &Path, options: &[String], inherit: bool) -> bool {
        self.params.interpreter == interpreter
            && self.params.interpreter_options == options
            && self.params.inherit_environment == inherit
    }

    fn spawn(params: StartParams) -> anyhow::Result<ConsoleSession> {
        let seq = HELPER_SEQ.fetch_add(1, Ordering::Relaxed);
        if seq == 0 {
            remove_dead_helper_dirs(&std::env::temp_dir());
        }
        let dir = std::env::temp_dir().join(format!("rustpy-console-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let helper_path = dir.join("rustpy_console_helper.py");
        std::fs::write(&helper_path, HELPER_SOURCE)?;
        std::fs::write(dir.join("console_values.py"), VALUE_ENGINE_SOURCE)?;
        std::fs::write(
            dir.join("rustpy_scientific_table.py"),
            rustpy_debug::scientific::TABLE_SOURCE,
        )?;
        std::fs::write(
            dir.join("rustpy_scientific_plots.py"),
            rustpy_debug::scientific::PLOT_SOURCE,
        )?;

        let mut cmd = rustpy_core::interpreter_launch::interpreter_command(&params.interpreter);
        if !params.inherit_environment {
            cmd.env_clear();
        }
        cmd.args(&params.interpreter_options);
        cmd.arg("-u")
            .arg(&helper_path)
            .current_dir(&params.cwd)
            .env("PYTHONUNBUFFERED", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in &params.env {
            cmd.env(k, v);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x00000200);
        }
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                // No session will remove the helper copy.
                let _ = std::fs::remove_dir_all(&dir);
                anyhow::bail!("cannot start {}: {e}", params.interpreter.display());
            }
        };
        let stdin = child.stdin.take();
        let (tx, rx) = crossbeam_channel::unbounded();
        if let Some(out) = child.stdout.take() {
            spawn_stdout_reader(out, tx.clone());
        }
        if let Some(err) = child.stderr.take() {
            spawn_stderr_reader(err, tx);
        }
        let mut session = ConsoleSession {
            params,
            child: Some(child),
            stdin,
            rx,
            next_id: 1,
            helper_path,
            pending: Vec::new(),
            exited: false,
            stdout_closed: false,
            started: Instant::now(),
        };
        let startup = session.params.startup_code.clone();
        if !startup.trim().is_empty() {
            session.send(&serde_json::json!({ "op": "startup", "code": startup }));
        }
        Ok(session)
    }

    fn send(&mut self, msg: &serde_json::Value) {
        let line = format!("{msg}\n");
        let ok = match &mut self.stdin {
            Some(stdin) => stdin
                .write_all(line.as_bytes())
                .and_then(|_| stdin.flush())
                .is_ok(),
            None => false,
        };
        if !ok {
            self.pending.push(ConsoleEvent::Failed(
                "the console process is not accepting input (restart the console)".into(),
            ));
        }
    }

    fn request(&mut self, mut msg: serde_json::Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        msg["id"] = serde_json::Value::from(id);
        self.send(&msg);
        id
    }

    /// Executes `code` (statements and/or a trailing expression); returns the request id.
    ///
    /// Typed-input rules: an unfinished statement (a `def` header, or a block
    /// not yet closed by a blank line) answers `more` and nothing runs.
    pub fn exec(&mut self, code: &str) -> u64 {
        self.request(serde_json::json!({ "op": "exec", "code": code }))
    }

    /// Executes `code` sent from the editor (selection, line, `#%%` cell) as one
    /// fragment: it runs even when it ends with a compound statement, never
    /// answers `more` (incomplete code is a `SyntaxError`), and the value of a
    /// trailing expression is reported.
    pub fn exec_fragment(&mut self, code: &str) -> u64 {
        self.request(serde_json::json!({ "op": "exec", "code": code, "mode": "exec" }))
    }

    /// Runs a script in the console namespace (*Run File in Python Console*):
    /// `__file__` and `sys.argv` (`[path, args…]`) are set for the run, `wdir`
    /// becomes the working directory and `env` is applied to `os.environ`.
    /// The variables the script defines stay in the console.
    pub fn run_file(
        &mut self,
        path: &Path,
        args: &[String],
        wdir: Option<&Path>,
        env: &[(String, String)],
    ) -> u64 {
        self.run_file_with_environment(path, args, wdir, env, true)
    }
    pub fn run_file_with_environment(
        &mut self,
        path: &Path,
        args: &[String],
        wdir: Option<&Path>,
        env: &[(String, String)],
        inherit_environment: bool,
    ) -> u64 {
        let env: serde_json::Map<String, serde_json::Value> = env
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::from(v.as_str())))
            .collect();
        self.request(serde_json::json!({ "op": "runfile", "path": path.to_string_lossy(), "args": args, "wdir": wdir.map(|w| w.to_string_lossy()), "env": env, "inherit_environment":inherit_environment }))
    }

    /// Requests completions for the dotted name ending at byte `cursor` of `code`.
    pub fn complete(&mut self, code: &str, cursor: usize) -> u64 {
        self.request(
            serde_json::json!({ "op": "complete", "code": code, "cursor": cursor.min(code.len()) }),
        )
    }

    /// Requests a snapshot of the user namespace (modules excluded).
    pub fn request_vars(&mut self) -> u64 {
        self.request(serde_json::json!({ "op": "vars", "include_modules": false }))
    }

    /// Requests the children (items / attributes) of the expression `name`.
    pub fn inspect(&mut self, name: &str) -> u64 {
        self.request(serde_json::json!({ "op": "inspect", "name": name, "max_items": 200 }))
    }

    pub fn inspect_page(&mut self, name: &str, start: usize, count: usize) -> u64 {
        self.request(
            serde_json::json!({"op":"page","name":name,"start":start,"count":count.min(200)}),
        )
    }
    pub fn request_vars_page(&mut self, start: usize, count: usize, include_modules: bool) -> u64 {
        self.request(serde_json::json!({"op":"vars_page","start":start,"count":count.min(200),"include_modules":include_modules}))
    }
    pub fn set_value(&mut self, name: &str, value: &str) -> u64 {
        self.request(serde_json::json!({"op":"set","name":name,"value":value}))
    }
    pub fn request_data(
        &mut self,
        name: &str,
        row_start: usize,
        row_count: usize,
        col_start: usize,
        col_count: usize,
    ) -> u64 {
        self.request(serde_json::json!({"op":"data","name":name,"row_start":row_start,"row_count":row_count.min(200),"col_start":col_start,"col_count":col_count.min(50)}))
    }
    pub fn request_scientific_data(&mut self, name: &str, options: serde_json::Value) -> u64 {
        self.request(serde_json::json!({"op":"data","name":name,"options":options}))
    }
    pub fn use_ipython(&mut self, enabled: bool) -> u64 {
        self.request(serde_json::json!({"op":"configure","ipython":enabled}))
    }
    pub fn set_scientific_capture(&mut self, enabled: bool, interactive: bool) -> u64 {
        self.request(
            serde_json::json!({"op":"configure","plots":enabled,"interactive_plots":interactive}),
        )
    }
    pub fn set_plot_capture(&mut self, enabled: bool) -> u64 {
        self.request(serde_json::json!({"op":"configure","plots":enabled}))
    }
    pub fn request_debugger(&mut self) -> u64 {
        self.request(serde_json::json!({"op":"debugger"}))
    }

    /// Sends SIGINT to the helper (raises `KeyboardInterrupt` in running code).
    pub fn interrupt(&mut self) {
        #[cfg(windows)]
        if let Some(child) = &self.child {
            unsafe {
                GenerateConsoleCtrlEvent(1, child.id());
            }
        }
        #[cfg(unix)]
        if let Some(child) = &self.child {
            // SAFETY: plain libc call with our own child's pid.
            unsafe { libc_kill(child.id() as i32, 2) };
        }
    }

    /// Non-blocking: drains everything the helper produced since the last call.
    pub fn poll(&mut self) -> Vec<ConsoleEvent> {
        let mut events = std::mem::take(&mut self.pending);
        while let Ok(raw) = self.rx.try_recv() {
            match raw {
                Raw::Stdout(t) => events.push(ConsoleEvent::Stdout(t)),
                Raw::Stderr(t) => events.push(ConsoleEvent::Stderr(t)),
                Raw::StdoutClosed => self.stdout_closed = true,
                Raw::Protocol(v) => {
                    if let Some(ev) = protocol_event(&v) {
                        events.push(ev);
                    }
                }
            }
        }
        if !self.exited {
            if let Some(child) = &mut self.child {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        self.exited = true;
                        events.push(ConsoleEvent::Exited(status.code().unwrap_or(-1)));
                    }
                    Ok(None) => {}
                    Err(e) => {
                        self.exited = true;
                        events.push(ConsoleEvent::Failed(e.to_string()));
                    }
                }
            }
        }
        events
    }

    pub fn is_running(&self) -> bool {
        !self.exited && self.child.is_some()
    }

    /// The private temporary directory holding this session's helper scripts.
    #[doc(hidden)]
    pub fn helper_dir(&self) -> Option<&Path> {
        self.helper_path.parent()
    }

    /// Seconds since the helper was started.
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Stops the current helper and starts a fresh one with the same parameters.
    pub fn restart(&mut self) {
        self.stop();
        match Self::spawn(self.params.clone()) {
            Ok(fresh) => *self = fresh,
            Err(e) => self.pending.push(ConsoleEvent::Failed(e.to_string())),
        }
    }

    /// Asks the helper to exit, then kills it if it does not comply.
    pub fn stop(&mut self) {
        if self.child.is_some() && !self.exited {
            self.send(&serde_json::json!({ "op": "exit" }));
        }
        self.stdin = None;
        if let Some(mut child) = self.child.take() {
            let deadline = Instant::now() + Duration::from_millis(500);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(20))
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
        }
        self.exited = true;
        // The directory is private to this session; it may also hold bytecode
        // an older helper (or the user's own imports from it) wrote there.
        if let Some(dir) = self.helper_path.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// The process id of a `rustpy-console-<pid>-<seq>` helper directory name.
fn helper_dir_pid(name: &str) -> Option<u32> {
    let (pid, seq) = name.strip_prefix("rustpy-console-")?.split_once('-')?;
    seq.parse::<u64>().ok()?;
    pid.parse().ok()
}

/// Whether process `pid` still exists (unknown counts as alive).
fn process_alive(pid: u32) -> bool {
    if pid == std::process::id() {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        Path::new("/proc").join(pid.to_string()).exists()
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        // Signal 0 only checks; EPERM (another user's process) still means alive.
        unsafe { libc_kill_check(pid as i32) }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
unsafe fn libc_kill_check(pid: i32) -> bool {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe { kill(pid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(1) }
}

/// Removes helper directories left in `temp` by consoles of processes that
/// died without cleaning up (crash, kill).
fn remove_dead_helper_dirs(temp: &Path) {
    let Ok(entries) = std::fs::read_dir(temp) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(helper_dir_pid) else {
            continue;
        };
        if !process_alive(pid) && entry.file_type().is_ok_and(|t| t.is_dir()) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

impl Drop for ConsoleSession {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(unix)]
unsafe fn libc_kill(pid: i32, sig: i32) {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe { kill(pid, sig) };
}

fn spawn_stdout_reader<R: std::io::Read + Send + 'static>(reader: R, tx: Sender<Raw>) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf);
                    match line.strip_prefix(PROTOCOL_PREFIX) {
                        Some(json) => {
                            match serde_json::from_str::<serde_json::Value>(json.trim_end()) {
                                Ok(v) => {
                                    let _ = tx.send(Raw::Protocol(v));
                                }
                                Err(_) => {
                                    let _ = tx.send(Raw::Stdout(line.into_owned()));
                                }
                            }
                        }
                        None => {
                            let _ = tx.send(Raw::Stdout(line.into_owned()));
                        }
                    }
                }
            }
        }
        let _ = tx.send(Raw::StdoutClosed);
    });
}

fn spawn_stderr_reader<R: std::io::Read + Send + 'static>(reader: R, tx: Sender<Raw>) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let _ = tx.send(Raw::Stderr(String::from_utf8_lossy(&buf).into_owned()));
                }
            }
        }
    });
}

fn protocol_event(v: &serde_json::Value) -> Option<ConsoleEvent> {
    let id = v.get("id").and_then(|i| i.as_u64()).unwrap_or(0);
    let text = |key: &str| v.get(key).and_then(|t| t.as_str()).map(str::to_owned);
    match v.get("event").and_then(|e| e.as_str())? {
        "stdout" => Some(ConsoleEvent::Stdout(text("text").unwrap_or_default())),
        "stderr" => Some(ConsoleEvent::Stderr(text("text").unwrap_or_default())),
        "result" => Some(ConsoleEvent::Result {
            id,
            value: text("value"),
            error: text("error"),
            more: v.get("more").and_then(|m| m.as_bool()).unwrap_or(false),
        }),
        "completions" => {
            let items: Vec<Completion> = v
                .get("items")
                .cloned()
                .and_then(|i| serde_json::from_value(i).ok())
                .unwrap_or_default();
            Some(ConsoleEvent::Completions {
                id,
                items,
                prefix: text("prefix").unwrap_or_default(),
            })
        }
        "vars" => {
            let items: Vec<VarInfo> = v
                .get("items")
                .cloned()
                .and_then(|i| serde_json::from_value(i).ok())
                .unwrap_or_default();
            Some(ConsoleEvent::Vars { id, items })
        }
        "children" => {
            let items: Vec<VarInfo> = v
                .get("items")
                .cloned()
                .and_then(|i| serde_json::from_value(i).ok())
                .unwrap_or_default();
            Some(ConsoleEvent::Children {
                id,
                name: text("name").unwrap_or_default(),
                items,
                error: text("error"),
            })
        }
        "page" => Some(ConsoleEvent::Page {
            id,
            name: text("name").unwrap_or_default(),
            start: v["start"].as_u64().unwrap_or(0) as usize,
            total: v["total"].as_u64().unwrap_or(0) as usize,
            items: serde_json::from_value(v["items"].clone()).unwrap_or_default(),
            error: text("error"),
        }),
        "set" => Some(ConsoleEvent::ValueSet {
            id,
            name: text("name").unwrap_or_default(),
            error: text("error"),
        }),
        "data" => Some(ConsoleEvent::Data {
            id,
            table: serde_json::from_value(v["table"].clone()).ok(),
            error: text("error"),
        }),
        "plot" => Some(ConsoleEvent::Plot {
            id,
            mime: text("mime").unwrap_or_default(),
            data_base64: text("data_base64").unwrap_or_default(),
        }),
        "configured" => Some(ConsoleEvent::Configured {
            id,
            ipython: v["ipython"].as_bool().unwrap_or(false),
            plots: v["plots"].as_bool().unwrap_or(false),
            error: text("error"),
        }),
        "debugger" => Some(ConsoleEvent::Debugger {
            id,
            host: text("host").unwrap_or_default(),
            port: v["port"]
                .as_u64()
                .and_then(|port| u16::try_from(port).ok())
                .unwrap_or(0),
            error: text("error"),
        }),
        "ready" => Some(ConsoleEvent::Ready {
            version: text("version").unwrap_or_default(),
            executable: text("executable").unwrap_or_default(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipython_injected_and_history_variables_are_special() {
        let var = |name: &str, type_name: &str, callable: bool| VarInfo {
            name: name.into(),
            expr: name.into(),
            type_name: type_name.into(),
            value: String::new(),
            is_callable: callable,
            is_module: false,
            has_children: false,
        };
        for special in [
            var("In", "list", false),
            var("Out", "dict", false),
            var("exit", "ExitAutocall", true),
            var("quit", "ExitAutocall", true),
            var("get_ipython", "method", true),
            var("open", "function", true),
            var("_ih", "list", false),
            var("_oh", "dict", false),
            var("_i3", "str", false),
            var("_", "int", false),
            var("__builtins__", "module", false),
        ] {
            assert!(special.is_special(), "{} must be hidden", special.name);
        }
        for user in [
            var("df", "DataFrame", false),
            var("arr", "ndarray", false),
            var("In", "int", false),
            var("Out", "str", false),
            var("exit", "int", false),
            var("open", "bool", false),
        ] {
            assert!(!user.is_special(), "{} is a user variable", user.name);
        }
    }

    #[test]
    fn parses_protocol_events() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"id":3,"event":"result","value":"2","error":null,"more":false}"#,
        )
        .unwrap();
        assert_eq!(
            protocol_event(&v),
            Some(ConsoleEvent::Result {
                id: 3,
                value: Some("2".into()),
                error: None,
                more: false
            })
        );
        let v: serde_json::Value = serde_json::from_str(r#"{"id":4,"event":"completions","items":[{"name":"join","kind":"method"}],"prefix":"jo"}"#).unwrap();
        match protocol_event(&v) {
            Some(ConsoleEvent::Completions {
                id: 4,
                items,
                prefix,
            }) => {
                assert_eq!(items[0].name, "join");
                assert_eq!(prefix, "jo");
            }
            other => panic!("unexpected {other:?}"),
        }
        let v: serde_json::Value = serde_json::from_str(r#"{"id":5,"event":"vars","items":[{"name":"x","expr":"x","type_name":"int","value":"1","is_callable":false,"is_module":false,"has_children":false}]}"#).unwrap();
        match protocol_event(&v) {
            Some(ConsoleEvent::Vars { items, .. }) => assert_eq!(items[0].type_name, "int"),
            other => panic!("unexpected {other:?}"),
        }
        let v: serde_json::Value = serde_json::from_str(r#"{"event":"bogus"}"#).unwrap();
        assert_eq!(protocol_event(&v), None);
    }

    #[test]
    fn helper_dirs_of_dead_processes_are_swept() {
        assert_eq!(helper_dir_pid("rustpy-console-42-0"), Some(42));
        assert_eq!(helper_dir_pid("rustpy-console-panel-42"), None);
        assert_eq!(helper_dir_pid("rustpy-console-42-x"), None);
        let temp = tempfile::tempdir().unwrap();
        // Far above any pid_max, so never a live process.
        let dead = temp.path().join("rustpy-console-999999999-3");
        std::fs::create_dir_all(dead.join("__pycache__")).unwrap();
        std::fs::write(dead.join("__pycache__/console_values.pyc"), b"").unwrap();
        let live = temp
            .path()
            .join(format!("rustpy-console-{}-7", std::process::id()));
        std::fs::create_dir_all(&live).unwrap();
        let other = temp.path().join("rustpy-console-panel-999999999");
        std::fs::create_dir_all(&other).unwrap();
        remove_dead_helper_dirs(temp.path());
        assert!(!dead.exists());
        assert!(live.exists());
        assert!(other.exists());
    }

    #[test]
    fn a_failed_start_leaves_no_helper_behind() {
        // No other test in this binary starts a session, so the next helper directory is known.
        let seq = HELPER_SEQ.load(Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("rustpy-console-{}-{seq}", std::process::id()));
        let err = ConsoleSession::start(
            Path::new("/nonexistent/python-xyz"),
            &std::env::temp_dir(),
            &[],
            "",
        )
        .err()
        .expect("no such interpreter");
        assert!(
            err.to_string()
                .contains("cannot start /nonexistent/python-xyz"),
            "{err}"
        );
        assert!(!dir.exists(), "{} was left behind", dir.display());
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn GenerateConsoleCtrlEvent(event: u32, group: u32) -> i32;
}
