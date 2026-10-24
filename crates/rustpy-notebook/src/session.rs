//! Notebook transport: existing embedded console or an explicitly selected Jupyter connection.
use rustpy_console::{ConsoleEvent, ConsoleSession};
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Connection {
    /// embedded, local (registered kernel), managed (Jupyter Server), remote, ssh, docker.
    pub kind: String,
    pub kernel: String,
    pub url: String,
    pub token: String,
    pub password: String,
    pub verify_ssl: bool,
    pub ca_file: String,
    pub host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssh_program: Option<std::path::PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssh_config_file: Option<std::path::PathBuf>,
    pub container: String,
    pub python: String,
    pub debug_host: String,
    pub debug_bind: String,
    pub debug_port: u16,
    pub cwd: String,
}
impl Default for Connection {
    fn default() -> Self {
        Self {
            kind: "embedded".into(),
            kernel: "python3".into(),
            url: String::new(),
            token: String::new(),
            password: String::new(),
            verify_ssl: true,
            ca_file: String::new(),
            host: String::new(),
            ssh_program: None,
            ssh_config_file: None,
            container: String::new(),
            python: String::new(),
            debug_host: String::new(),
            debug_bind: "127.0.0.1".into(),
            debug_port: 0,
            cwd: String::new(),
        }
    }
}
impl Connection {
    pub fn embedded() -> Self {
        Self {
            kind: "embedded".into(),
            verify_ssl: true,
            ..Default::default()
        }
    }
    pub fn normalized(mut self) -> Result<Self, String> {
        if self.kind == "remote" {
            let mut url = url::Url::parse(&self.url).map_err(|error| error.to_string())?;
            if !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err(
                    "Jupyter server URL must be HTTP(S), without embedded credentials".into(),
                );
            }
            if self.token.is_empty() {
                self.token = url
                    .query_pairs()
                    .find(|(key, _)| key == "token")
                    .map(|(_, value)| value.into_owned())
                    .unwrap_or_default();
            }
            url.set_query(None);
            url.set_fragment(None);
            let path = url.path().to_owned();
            let parts = path
                .split('/')
                .filter(|segment| !segment.is_empty())
                .take_while(|segment| !matches!(*segment, "lab" | "tree" | "notebooks"))
                .collect::<Vec<_>>();
            let base = if parts.is_empty() {
                "/".into()
            } else {
                format!("/{}/", parts.join("/"))
            };
            url.set_path(&base);
            self.url = url.to_string();
        }
        Ok(self)
    }
    pub fn validate(&self) -> Result<(), String> {
        if !["embedded", "local", "managed", "remote", "ssh", "docker"]
            .contains(&self.kind.as_str())
        {
            return Err("Unknown notebook connection kind".into());
        }
        if self.kind == "remote"
            && !(self.url.starts_with("https://") || self.url.starts_with("http://"))
        {
            return Err("Remote server requires an HTTP(S) URL".into());
        }
        if self.kind == "ssh"
            && (self.host.is_empty()
                || self.host.starts_with('-')
                || self
                    .host
                    .chars()
                    .any(|character| character.is_whitespace() || character.is_control()))
        {
            return Err("SSH requires one configured host alias".into());
        }
        for (label, path) in [
            ("SSH executable", &self.ssh_program),
            ("SSH config", &self.ssh_config_file),
        ] {
            if let Some(path) = path {
                let Some(text) = path.to_str() else {
                    return Err(format!("{label} path must be UTF-8"));
                };
                if self.kind != "ssh"
                    || !path.is_absolute()
                    || !path.is_file()
                    || text.chars().any(char::is_control)
                {
                    return Err(format!(
                        "{label} requires an existing absolute path without control characters and an SSH connection"
                    ));
                }
            }
        }
        if self.kind == "docker" && (self.container.is_empty() || self.container.starts_with('-')) {
            return Err("Docker requires a configured container".into());
        }
        Ok(())
    }
}
pub(crate) enum Session {
    Embedded(Box<ConsoleSession>),
    Jupyter(Box<Bridge>),
}
impl Session {
    pub fn start(
        python: &Path,
        cwd: &Path,
        env: &[(String, String)],
        startup: &str,
        connection: Connection,
        marker: String,
    ) -> anyhow::Result<Self> {
        connection.validate().map_err(anyhow::Error::msg)?;
        let connection = connection.normalized().map_err(anyhow::Error::msg)?;
        if connection.kind == "embedded" {
            Ok(Self::Embedded(Box::new(ConsoleSession::start(
                python, cwd, env, startup,
            )?)))
        } else {
            Ok(Self::Jupyter(Box::new(Bridge::start(
                python, cwd, env, connection, marker,
            )?)))
        }
    }
    pub fn exec_cell(&mut self, count: u32, source: &str, cell_id: &str) -> u64 {
        match self {
            Self::Embedded(s) => s.exec(&format!(
                "__rustpy_nb__.run({}, {})",
                count,
                serde_json::to_string(source).unwrap()
            )),
            Self::Jupyter(s) => {
                s.request(serde_json::json!({"op":"exec","code":source,"cell_id":cell_id}))
            }
        }
    }
    pub fn request_debugger(&mut self) -> u64 {
        match self {
            Self::Embedded(s) => s.request_debugger(),
            Self::Jupyter(s) => s.request(serde_json::json!({"op":"debugger"})),
        }
    }
    pub fn request_vars(&mut self) -> u64 {
        match self {
            Self::Embedded(s) => s.request_vars(),
            Self::Jupyter(s) => s.request(serde_json::json!({"op":"vars"})),
        }
    }
    pub fn inspect(&mut self, name: &str) -> u64 {
        match self {
            Self::Embedded(s) => s.inspect(name),
            Self::Jupyter(s) => s.request(serde_json::json!({"op":"inspect","name":name})),
        }
    }
    pub fn complete(&mut self, code: &str, cursor: usize) -> u64 {
        let mut cursor = cursor.min(code.len());
        while !code.is_char_boundary(cursor) {
            cursor = cursor.saturating_sub(1);
        }
        match self{Self::Embedded(session)=>session.complete(code,cursor),Self::Jupyter(session)=>session.request(serde_json::json!({"op":"complete","code":code,"cursor":code[..cursor].chars().count()}))}
    }
    pub fn data(&mut self, name: &str, row: usize, col: usize) -> u64 {
        match self{Self::Embedded(s)=>s.request_data(name,row,100,col,20),Self::Jupyter(s)=>s.request(serde_json::json!({"op":"data","name":name,"row_start":row,"row_count":100,"col_start":col,"col_count":20}))}
    }
    pub fn widget(&mut self, id: &str, state: serde_json::Value) {
        if let Self::Jupyter(s) = self {
            s.request(serde_json::json!({"op":"widget","comm_id":id,"state":state}));
        }
    }
    pub fn widget_event(&mut self, id: &str, event: serde_json::Value) {
        if let Self::Jupyter(session) = self {
            session.request(serde_json::json!({"op":"widget_custom","comm_id":id,"content":event}));
        }
    }
    pub fn owns_process(&self) -> bool {
        match self {
            Self::Embedded(session) => session.is_running(),
            Self::Jupyter(session) => session.alive.load(Ordering::Acquire),
        }
    }
    pub fn poll(&mut self) -> Vec<ConsoleEvent> {
        match self {
            Self::Embedded(s) => s.poll(),
            Self::Jupyter(s) => s.poll(),
        }
    }
    pub fn interrupt(&mut self) {
        match self {
            Self::Embedded(s) => s.interrupt(),
            Self::Jupyter(s) => {
                s.interrupt.store(true, Ordering::Release);
            }
        }
    }
    pub fn restart(&mut self) -> bool {
        match self {
            Self::Embedded(session) => {
                session.restart();
                true
            }
            Self::Jupyter(session) => {
                if session.stop.load(Ordering::Acquire) && session.alive.load(Ordering::Acquire) {
                    return false;
                }
                if !session.alive.load(Ordering::Acquire) {
                    let p = session.params.clone();
                    match Bridge::start(&p.python, &p.cwd, &p.env, p.connection, p.marker) {
                        Ok(new) => **session = new,
                        Err(_) => return false,
                    }
                } else {
                    session.request(serde_json::json!({"op":"restart"}));
                }
                true
            }
        }
    }
    pub fn stop(&mut self) {
        match self {
            Self::Embedded(s) => s.stop(),
            Self::Jupyter(s) => s.stop.store(true, Ordering::Release),
        }
    }
}
struct OwnedChild(std::process::Child);
impl std::ops::Deref for OwnedChild {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
fn ssh_command(connection: &Connection, program: &str, script: &str) -> Command {
    let mut command = Command::new(
        connection
            .ssh_program
            .as_deref()
            .unwrap_or(Path::new("ssh")),
    );
    command.args([
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "StrictHostKeyChecking=yes",
    ]);
    if let Some(config) = &connection.ssh_config_file {
        command.arg("-F").arg(config);
    }
    command.args(["--", &connection.host]);
    let run = format!("{} -u -c {}", shell_quote(program), shell_quote(script));
    command.arg(if connection.cwd.is_empty() {
        run
    } else {
        format!("cd -- {} && {run}", shell_quote(&connection.cwd))
    });
    command
}
impl std::ops::DerefMut for OwnedChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(self.0.id() as i32), libc::SIGKILL);
            }
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
#[derive(Clone)]
struct StartParams {
    python: std::path::PathBuf,
    cwd: std::path::PathBuf,
    env: Vec<(String, String)>,
    connection: Connection,
    marker: String,
}
pub(crate) struct Bridge {
    tx: crossbeam_channel::Sender<serde_json::Value>,
    rx: crossbeam_channel::Receiver<ConsoleEvent>,
    stop: Arc<AtomicBool>,
    interrupt: Arc<AtomicBool>,
    next: u64,
    alive: Arc<AtomicBool>,
    params: StartParams,
    retired_reported: bool,
}
impl Bridge {
    fn start(
        python: &Path,
        cwd: &Path,
        env: &[(String, String)],
        connection: Connection,
        marker: String,
    ) -> anyhow::Result<Self> {
        let params = StartParams {
            python: python.into(),
            cwd: cwd.into(),
            env: env.to_vec(),
            connection: connection.clone(),
            marker: marker.clone(),
        };
        let (tx, requests) = crossbeam_channel::bounded::<serde_json::Value>(128);
        let (events, rx) = crossbeam_channel::bounded(4);
        let stop = Arc::new(AtomicBool::new(false));
        let interrupt = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let worker_alive = alive.clone();
        let worker_stop = stop.clone();
        let worker_interrupt = interrupt.clone();
        let cwd = cwd.to_owned();
        let python = python.to_owned();
        let env = env.to_vec();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<()> {
                let script = include_str!("jupyter_bridge.py");
                let program = if connection.python.is_empty() {
                    if ["ssh", "docker"].contains(&connection.kind.as_str()) {
                        "python3".into()
                    } else {
                        python.to_string_lossy().into_owned()
                    }
                } else {
                    connection.python.clone()
                };
                let mut command = match connection.kind.as_str() {
                    "ssh" => ssh_command(&connection, &program, script),
                    "docker" => {
                        let mut c = Command::new("docker");
                        c.args(["exec", "-i"]);
                        if !connection.cwd.is_empty() {
                            c.args(["-w", &connection.cwd]);
                        }
                        c.args([&connection.container, &program, "-u", "-c", script]);
                        c
                    }
                    _ => {
                        let mut c = rustpy_core::interpreter_launch::interpreter_command(
                            std::path::Path::new(&program),
                        );
                        c.args(["-u", "-c", script]);
                        c
                    }
                };
                #[cfg(unix)]
                {
                    use std::os::unix::process::CommandExt;
                    unsafe {
                        command.pre_exec(|| {
                            if libc::setsid() < 0 {
                                return Err(std::io::Error::last_os_error());
                            }
                            Ok(())
                        });
                    }
                }
                let mut child = OwnedChild(
                    command
                        .current_dir(&cwd)
                        .envs(env)
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::piped())
                        .spawn()?,
                );
                let mut input = child.stdin.take().unwrap();
                let output = child.stdout.take().unwrap();
                let stderr = child.stderr.take().unwrap();
                let output_events = events.clone();
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(output);
                    loop {
                        let mut line = String::new();
                        match reader
                            .by_ref()
                            .take(32 * 1024 * 1024 + 1)
                            .read_line(&mut line)
                        {
                            Ok(0) | Err(_) => break,
                            Ok(_) if line.len() > 32 * 1024 * 1024 => {
                                let _ = output_events.send(ConsoleEvent::Failed(
                                    "Jupyter message exceeds 32 MiB".into(),
                                ));
                                break;
                            }
                            Ok(_) => {
                                let event = serde_json::from_str(&line)
                                    .map(decode)
                                    .unwrap_or_else(|_| ConsoleEvent::Stderr(line));
                                if output_events.send(event).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                });
                let error_events = events.clone();
                std::thread::spawn(move || {
                    for line in BufReader::new(stderr).lines().take(10000).flatten() {
                        if error_events
                            .send(ConsoleEvent::Stderr(line.chars().take(65536).collect()))
                            .is_err()
                        {
                            break;
                        }
                    }
                });
                let config = serde_json::json!({"connection":connection,"marker":marker,"values":rustpy_console::VALUE_ENGINE_SOURCE,"scientific_table":rustpy_console::SCIENTIFIC_TABLE_SOURCE});
                let mut write = |value: &serde_json::Value| -> std::io::Result<()> {
                    serde_json::to_writer(&mut input, value)?;
                    input.write_all(b"\n")?;
                    input.flush()
                };
                if let Err(error) = write(&config) {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error.into());
                }
                loop {
                    if worker_stop.load(Ordering::Acquire) {
                        let _ = write(&serde_json::json!({"op":"exit"}));
                        let deadline = std::time::Instant::now() + Duration::from_secs(35);
                        while std::time::Instant::now() < deadline {
                            if child.try_wait()?.is_some() {
                                return Ok(());
                            }
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        #[cfg(unix)]
                        unsafe {
                            libc::kill(-(child.id() as i32), libc::SIGKILL);
                        }
                        let _ = child.kill();
                        let _ = child.wait();
                        return Ok(());
                    }
                    if worker_interrupt.swap(false, Ordering::AcqRel)
                        && write(&serde_json::json!({"op":"interrupt"})).is_err()
                    {
                        #[cfg(unix)]
                        unsafe {
                            libc::kill(-(child.id() as i32), libc::SIGKILL);
                        }
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                    match requests.recv_timeout(Duration::from_millis(20)) {
                        Ok(request) => {
                            if write(&request).is_err() {
                                #[cfg(unix)]
                                unsafe {
                                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                                }
                                let _ = child.kill();
                                let _ = child.wait();
                                break;
                            }
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                            #[cfg(unix)]
                            unsafe {
                                libc::kill(-(child.id() as i32), libc::SIGKILL);
                            }
                            let _ = child.kill();
                            let _ = child.wait();
                            break;
                        }
                        _ => {}
                    }
                    if let Some(status) = child.try_wait()? {
                        let _ = events.send(ConsoleEvent::Exited(status.code().unwrap_or(-1)));
                        break;
                    }
                }
                Ok(())
            })();
            if let Err(error) = result {
                let _ = events.send(ConsoleEvent::Failed(format!("Jupyter bridge: {error}")));
            }
            worker_alive.store(false, Ordering::Release);
        });
        Ok(Self {
            tx,
            rx,
            stop,
            interrupt,
            next: 1,
            alive,
            params,
            retired_reported: false,
        })
    }
    fn request(&mut self, mut value: serde_json::Value) -> u64 {
        let id = self.next;
        self.next = self.next.saturating_add(1);
        value["id"] = id.into();
        if self.tx.try_send(value).is_err() {
            self.stop.store(true, Ordering::Release);
        }
        id
    }
    fn poll(&mut self) -> Vec<ConsoleEvent> {
        let mut events = self.rx.try_iter().take(1024).collect::<Vec<_>>();
        if events
            .iter()
            .any(|event| matches!(event, ConsoleEvent::Exited(_) | ConsoleEvent::Failed(_)))
        {
            self.retired_reported = true;
        }
        if !self.alive.load(Ordering::Acquire) && !self.retired_reported {
            events.push(ConsoleEvent::Exited(0));
            self.retired_reported = true;
        }
        events
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}
fn decode(v: serde_json::Value) -> ConsoleEvent {
    let id = v["id"].as_u64().unwrap_or(0);
    let text = |key: &str| v[key].as_str().unwrap_or_default().to_owned();
    let items = || serde_json::from_value(v["items"].clone()).unwrap_or_default();
    match v["event"].as_str().unwrap_or("") {
        "stdout" => ConsoleEvent::Stdout(text("text")),
        "stderr" => ConsoleEvent::Stderr(text("text")),
        "ready" => ConsoleEvent::Ready {
            version: text("version"),
            executable: text("executable"),
        },
        "result" => ConsoleEvent::Result {
            id,
            value: v["execution_count"].as_u64().map(|count| count.to_string()),
            error: v["error"].as_str().map(str::to_owned),
            more: false,
        },
        "vars" => ConsoleEvent::Vars { id, items: items() },
        "children" => ConsoleEvent::Children {
            id,
            name: text("name"),
            items: items(),
            error: v["error"].as_str().map(str::to_owned),
        },
        "completions" => ConsoleEvent::Completions {
            id,
            items: serde_json::from_value(v["items"].clone()).unwrap_or_default(),
            prefix: text("prefix"),
        },
        "data" => ConsoleEvent::Data {
            id,
            table: serde_json::from_value(v["table"].clone()).ok(),
            error: v["error"].as_str().map(str::to_owned),
        },
        "debugger" => ConsoleEvent::Debugger {
            id,
            host: text("host"),
            port: v["port"].as_u64().unwrap_or(0) as u16,
            error: v["error"].as_str().map(str::to_owned),
        },
        "exited" => ConsoleEvent::Exited(0),
        _ => ConsoleEvent::Stderr(format!("Jupyter: {v}\n")),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bridge_protocol_preserves_owner_and_dynamic_completion() {
        let event = decode(
            serde_json::json!({"event":"completions","id":42,"prefix":"fr","items":[{"name":"frame","kind":"variable"}]}),
        );
        assert!(
            matches!(event,ConsoleEvent::Completions{id:42,items,prefix} if prefix=="fr" && items[0].name=="frame")
        );
        let event =
            decode(serde_json::json!({"event":"result","id":21,"error":"KeyboardInterrupt"}));
        assert!(
            matches!(event,ConsoleEvent::Result{id:21,error:Some(message),..}if message=="KeyboardInterrupt")
        );
    }
    #[test]
    fn shell_transport_arguments_remain_literal() {
        assert_eq!(shell_quote("python; $(bad)'"), "'python; $(bad)'\\'''");
        assert!(Connection::default().verify_ssl);
    }
    #[test]
    fn explicit_ssh_transport_preserves_legacy_defaults_and_strict_host_policy() {
        let legacy = Connection::default();
        let mut serialized = serde_json::to_value(&legacy).unwrap();
        assert!(serialized.get("ssh_program").is_none());
        assert!(serialized.get("ssh_config_file").is_none());
        serialized["kind"] = "ssh".into();
        serialized["host"] = "owned-alias".into();
        let mut configured: Connection = serde_json::from_value(serialized).unwrap();
        assert!(configured.validate().is_ok());
        let legacy_command = ssh_command(&configured, "python3", "pass");
        assert_eq!(legacy_command.get_program(), "ssh");
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("ssh selected");
        let config = directory.path().join("private config");
        std::fs::write(&executable, "not executed by this validation fixture").unwrap();
        std::fs::write(&config, "Host *\n  StrictHostKeyChecking no\n").unwrap();
        configured.ssh_program = Some(executable.clone());
        configured.ssh_config_file = Some(config.clone());
        configured.cwd = "/owned path/'π;$(inert)".into();
        assert!(configured.validate().is_ok());
        let captured = configured.clone();
        let command = ssh_command(&configured, "/sdk/python 'π", "print('literal')");
        assert_eq!(command.get_program(), executable.as_os_str());
        let args: Vec<_> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();
        assert!(args
            .windows(2)
            .any(|pair| pair == ["-o", "StrictHostKeyChecking=yes"]));
        assert!(args
            .windows(2)
            .any(|pair| pair[0] == "-F" && Path::new(&pair[1]) == config));
        assert!(args
            .last()
            .unwrap()
            .starts_with(&format!("cd -- {} && ", shell_quote(&configured.cwd))));
        configured.ssh_config_file = Some(directory.path().join("later config"));
        assert!(configured.validate().is_err());
        assert_eq!(captured.ssh_config_file.as_ref(), Some(&config));
        for path in [
            std::path::PathBuf::from("relative"),
            directory.path().join("missing"),
            directory.path().join("bad\nconfig"),
        ] {
            configured.ssh_config_file = Some(path);
            assert!(configured.validate().is_err());
        }
        configured = captured;
        configured.host = "bad\0host".into();
        assert!(configured.validate().is_err());
    }
}
