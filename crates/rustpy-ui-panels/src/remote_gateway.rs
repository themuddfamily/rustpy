//! Owned Gateway client transports and loopback forwarding. This module never
//! discovers targets, installs tools, starts containers or executes project hooks.
#[cfg(all(test, target_os = "linux"))]
mod configured_tests;
use rustpy_lsp::gateway::{self, Envelope, Request, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};
static NEXT: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum Kind {
    #[default]
    Ssh,
    Docker,
    Wsl,
    Devcontainer,
    Local,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Target {
    pub kind: Kind,
    pub target: String,
    pub root: String,
    pub executable: String,
    pub transport: String,
    pub local_cwd: PathBuf,
    pub user: String,
    pub port: u16,
    pub key: String,
    pub ssh_config: String,
    pub jump: String,
    pub python: String,
    pub python_version: (u32, u32),
}
impl Default for Target {
    fn default() -> Self {
        Self {
            kind: Kind::Ssh,
            target: String::new(),
            root: "/workspace".into(),
            executable: "rustpy".into(),
            transport: String::new(),
            local_cwd: PathBuf::new(),
            user: String::new(),
            port: 22,
            key: String::new(),
            ssh_config: String::new(),
            jump: String::new(),
            python: "python3".into(),
            python_version: (3, 12),
        }
    }
}
impl Target {
    pub fn load() -> Self {
        std::fs::read(rustpy_settings::Settings::system_dir().join("remote-development.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }
    pub fn save(&self) -> Result<(), String> {
        self.validate()?;
        let path = rustpy_settings::Settings::system_dir().join("remote-development.json");
        std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(
            path,
            serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
    pub fn validate(&self) -> Result<(), String> {
        for text in [
            &self.target,
            &self.root,
            &self.executable,
            &self.transport,
            &self.user,
            &self.key,
            &self.ssh_config,
            &self.jump,
            &self.python,
        ] {
            if text.chars().any(char::is_control) {
                return Err("Remote target fields cannot contain control characters".into());
            }
        }
        if self.executable.is_empty()
            || self.executable.starts_with('-')
            || self.python.is_empty()
            || self.python.starts_with('-')
        {
            return Err("Configure installed RustPy and Python executables".into());
        }
        if self.kind != Kind::Local
            && (!self.root.starts_with('/') || self.root.split('/').any(|part| part == ".."))
        {
            return Err("Choose an absolute POSIX remote workspace root".into());
        }
        if self.kind == Kind::Local && !Path::new(&self.root).is_absolute() {
            return Err("Choose an absolute local server root".into());
        }
        if self.kind != Kind::Local && (self.target.is_empty() || self.target.starts_with('-')) {
            return Err(
                "Choose an explicit host, container, WSL distribution or devcontainer workspace"
                    .into(),
            );
        }
        if !self.local_cwd.is_absolute() {
            return Err("Choose an absolute local transport working directory".into());
        }
        if self.port == 0 {
            return Err("SSH port must be nonzero".into());
        }
        Ok(())
    }
    pub fn command(
        &self,
        program: &str,
        args: &[String],
        interactive: bool,
    ) -> Result<rustpy_deploy::CommandSpec, String> {
        self.validate()?;
        let spec = match self.kind {
            Kind::Ssh => {
                let mut server = rustpy_deploy::Server::new(
                    "Remote Development",
                    rustpy_deploy::ServerKind::Sftp {
                        host: self.target.clone(),
                        port: self.port,
                        user: self.user.clone(),
                        key_file: (!self.key.is_empty()).then(|| PathBuf::from(&self.key)),
                    },
                    self.root.clone(),
                );
                server.root_path = self.root.clone();
                server.options.ssh_config_file = self.ssh_config.clone();
                server.options.jump_host = self.jump.clone();
                server.options.ssh_auth = "password".into();
                server.options.ssh_executable = self.transport.clone();
                let command = std::iter::once(program.to_owned())
                    .chain(args.iter().cloned())
                    .map(|value| rustpy_deploy::sh_quote(&value))
                    .collect::<Vec<_>>()
                    .join(" ");
                let mut spec = rustpy_deploy::ssh_tools::external(&server, &command)
                    .map_err(|e| e.to_string())?;
                if interactive {
                    spec.args.insert(0, "-tt".into());
                }
                spec
            }
            Kind::Docker => rustpy_deploy::CommandSpec::new(if self.transport.is_empty() {
                "docker"
            } else {
                &self.transport
            })
            .args([
                "exec",
                if interactive { "-it" } else { "-i" },
                "--workdir",
                &self.root,
                &self.target,
                program,
            ])
            .args(args.iter().cloned()),
            Kind::Wsl => rustpy_deploy::CommandSpec::new(if self.transport.is_empty() {
                "wsl.exe"
            } else {
                &self.transport
            })
            .args([
                "--distribution",
                &self.target,
                "--cd",
                &self.root,
                "--exec",
                program,
            ])
            .args(args.iter().cloned()),
            Kind::Devcontainer => rustpy_deploy::CommandSpec::new(if self.transport.is_empty() {
                "devcontainer"
            } else {
                &self.transport
            })
            .args(["exec", "--workspace-folder", &self.target, "--", program])
            .args(args.iter().cloned()),
            Kind::Local => rustpy_deploy::CommandSpec::new(program).args(args.iter().cloned()),
        };
        Ok(spec)
    }
    pub fn server(&self) -> Result<rustpy_deploy::CommandSpec, String> {
        self.command(
            &self.executable,
            &[
                "remote-server".into(),
                "--root".into(),
                self.root.clone(),
                "--admit-client".into(),
            ],
            false,
        )
    }
    pub fn terminal(&self) -> Result<rustpy_deploy::CommandSpec, String> {
        self.command(
            "/bin/sh",
            &[
                "-c".into(),
                format!(
                    "cd -- {} && exec /bin/sh",
                    rustpy_deploy::sh_quote(&self.root)
                ),
            ],
            true,
        )
    }
    pub fn run_python(
        &self,
        path: &str,
        debug: Option<u16>,
    ) -> Result<rustpy_deploy::CommandSpec, String> {
        gateway::relative(path)?;
        let mut args = Vec::new();
        if let Some(port) = debug {
            args.extend([
                "-m".into(),
                "debugpy".into(),
                "--listen".into(),
                format!("127.0.0.1:{port}"),
                "--wait-for-client".into(),
            ]);
        }
        args.push(format!("{}/{}", self.root.trim_end_matches('/'), path));
        self.command(&self.python, &args, false)
    }
    pub fn mirror(&self) -> PathBuf {
        let identity = serde_json::to_vec(self).unwrap_or_default();
        rustpy_settings::Settings::system_dir()
            .join("remote-workspaces")
            .join(gateway::revision(&identity))
    }
}
type Reply = mpsc::Receiver<Result<Value, String>>;
struct Pending {
    sender: mpsc::Sender<Result<Value, String>>,
    since: Instant,
}
struct Shared {
    id: u64,
    target: Target,
    canonical_root: Mutex<String>,
    tx: mpsc::SyncSender<Envelope>,
    next: AtomicU64,
    pending: Mutex<HashMap<u64, Pending>>,
    stop: AtomicBool,
    done: AtomicBool,
    ready: AtomicBool,
    stamp: Mutex<String>,
    log: Mutex<VecDeque<String>>,
}
pub struct Client {
    shared: Arc<Shared>,
}
fn request(shared: &Arc<Shared>, message: Request) -> Result<(u64, Reply), String> {
    if shared.stop.load(Ordering::Acquire) {
        return Err("The remote development connection is closed".into());
    }
    if !shared.ready.load(Ordering::Acquire) && !matches!(message, Request::Hello { .. }) {
        return Err("Wait for the remote version/root handshake".into());
    }
    let id = shared.next.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel();
    let mut pending = shared.pending.lock().unwrap();
    if pending.len() >= 32 {
        return Err("Remote request queue is full".into());
    }
    pending.insert(
        id,
        Pending {
            sender: tx,
            since: Instant::now(),
        },
    );
    drop(pending);
    if shared
        .tx
        .try_send(Envelope {
            id,
            request: message,
        })
        .is_err()
    {
        shared.pending.lock().unwrap().remove(&id);
        return Err("Remote transport queue is full".into());
    }
    Ok((id, rx))
}
impl Client {
    pub fn spawn(
        target: Target,
        environment: &[(String, String)],
    ) -> Result<(Self, Reply), String> {
        let spec = target.server()?;
        let mut command = std::process::Command::new(&spec.program);
        command
            .args(&spec.args)
            .current_dir(&target.local_cwd)
            .envs(environment.iter().cloned())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        rustpy_run::process_owner::ProcessOwner::prepare(&mut command);
        let mut child = command.spawn().map_err(|e| e.to_string())?;
        let owner = match rustpy_run::process_owner::ProcessOwner::attach(&mut child) {
            Ok(owner) => owner,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.to_string());
            }
        };
        let mut input = child.stdin.take().ok_or("Remote stdin is missing")?;
        let mut output = child.stdout.take().ok_or("Remote stdout is missing")?;
        let mut errors = child.stderr.take().ok_or("Remote stderr is missing")?;
        let (tx, rx) = mpsc::sync_channel::<Envelope>(32);
        let shared = Arc::new(Shared {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            target: target.clone(),
            canonical_root: Mutex::new(target.root.clone()),
            tx,
            next: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            stop: AtomicBool::new(false),
            done: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            stamp: Mutex::new(owner.stamp()),
            log: Mutex::new(VecDeque::new()),
        });
        let writer_done = Arc::new(AtomicBool::new(false));
        let reader_done = Arc::new(AtomicBool::new(false));
        let errors_done = Arc::new(AtomicBool::new(false));
        let state = shared.clone();
        let done = writer_done.clone();
        std::thread::spawn(move || {
            while !state.stop.load(Ordering::Acquire) {
                match rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(message) => {
                        if let Err(error) = gateway::write_frame(&mut input, &message) {
                            state.log.lock().unwrap().push_back(error);
                            state.stop.store(true, Ordering::Release);
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(_) => break,
                }
            }
            drop(input);
            done.store(true, Ordering::Release);
        });
        let state = shared.clone();
        let done = reader_done.clone();
        std::thread::spawn(move || {
            loop {
                match gateway::read_value::<Response>(&mut output) {
                    Ok(Some(response)) => {
                        if response.id == 1 && response.error.is_none() {
                            let valid = response.result.as_ref().is_some_and(|value| {
                                value["version"] == env!("CARGO_PKG_VERSION")
                                    && value["protocol"] == gateway::PROTOCOL
                                    && value["root"].as_str().is_some_and(|root| {
                                        !root.is_empty() && !root.chars().any(char::is_control)
                                    })
                            });
                            if valid {
                                *state.canonical_root.lock().unwrap() =
                                    response.result.as_ref().unwrap()["root"]
                                        .as_str()
                                        .unwrap()
                                        .to_owned();
                                state.ready.store(true, Ordering::Release);
                            } else {
                                state.stop.store(true, Ordering::Release);
                            }
                        }
                        if let Some(pending) = state.pending.lock().unwrap().remove(&response.id) {
                            let _ =
                                pending
                                    .sender
                                    .send(response.error.map(Err).unwrap_or_else(|| {
                                        response
                                            .result
                                            .ok_or_else(|| "Empty gateway response".into())
                                    }));
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        state.log.lock().unwrap().push_back(error);
                        break;
                    }
                }
            }
            state.stop.store(true, Ordering::Release);
            done.store(true, Ordering::Release);
        });
        let state = shared.clone();
        let done = errors_done.clone();
        std::thread::spawn(move || {
            let mut bytes = [0; 2048];
            let mut retained = 0;
            while let Ok(count) = errors.read(&mut bytes) {
                if count == 0 {
                    break;
                }
                let text = String::from_utf8_lossy(&bytes[..count]).into_owned();
                let mut log = state.log.lock().unwrap();
                log.push_back(text);
                retained += count;
                while retained > 65536 {
                    if let Some(line) = log.pop_front() {
                        retained = retained.saturating_sub(line.len());
                    } else {
                        break;
                    }
                }
            }
            done.store(true, Ordering::Release);
        });
        let state = shared.clone();
        std::thread::spawn(move || {
            let mut exited = false;
            loop {
                let expired = state
                    .pending
                    .lock()
                    .unwrap()
                    .iter()
                    .filter_map(|(id, pending)| {
                        (pending.since.elapsed() > Duration::from_secs(45)).then_some(*id)
                    })
                    .collect::<Vec<_>>();
                for id in expired {
                    if let Some(pending) = state.pending.lock().unwrap().remove(&id) {
                        let _ = pending.sender.send(Err("Remote request timed out".into()));
                        let _ = state.tx.try_send(Envelope {
                            id: 0,
                            request: Request::Cancel { id },
                        });
                    }
                }
                if state.stop.load(Ordering::Acquire) {
                    owner.stop(&mut child);
                }
                match owner.poll(&mut child) {
                    Ok(Some(_)) => {
                        exited = true;
                        state.stop.store(true, Ordering::Release);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        state.log.lock().unwrap().push_back(error.to_string());
                        owner.stop(&mut child);
                    }
                }
                if state.stop.load(Ordering::Acquire) {
                    for (_, pending) in state.pending.lock().unwrap().drain() {
                        let _ = pending
                            .sender
                            .send(Err("Remote development disconnected".into()));
                    }
                }
                if exited
                    && writer_done.load(Ordering::Acquire)
                    && reader_done.load(Ordering::Acquire)
                    && errors_done.load(Ordering::Acquire)
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            state.done.store(true, Ordering::Release);
        });
        let client = Self { shared };
        let (_, reply) = client.request(Request::Hello {
            version: env!("CARGO_PKG_VERSION").into(),
            protocol: gateway::PROTOCOL,
            root: target.root.clone(),
            admitted: true,
            python: target.python_version,
        })?;
        Ok((client, reply))
    }
    pub fn request(&self, message: Request) -> Result<(u64, Reply), String> {
        request(&self.shared, message)
    }
    pub fn id(&self) -> u64 {
        self.shared.id
    }
    pub fn target(&self) -> &Target {
        &self.shared.target
    }
    pub fn effective_target(&self) -> Target {
        let mut target = self.shared.target.clone();
        target.root = self.shared.canonical_root.lock().unwrap().clone();
        target
    }
    pub fn ready(&self) -> bool {
        self.shared.ready.load(Ordering::Acquire) && !self.shared.stop.load(Ordering::Acquire)
    }
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Release);
    }
    pub fn finished(&self) -> bool {
        self.shared.done.load(Ordering::Acquire)
    }
    pub fn stamp(&self) -> String {
        let mut ids = self
            .shared
            .pending
            .lock()
            .unwrap()
            .keys()
            .copied()
            .collect::<Vec<_>>();
        ids.sort();
        format!(
            "{}:{}:{}:{}:{:?}",
            self.id(),
            self.shared.canonical_root.lock().unwrap(),
            env!("CARGO_PKG_VERSION"),
            self.shared.stamp.lock().unwrap(),
            ids
        )
    }
    pub fn logs(&self) -> String {
        self.shared
            .log
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("")
    }
    pub fn cancel(&self, id: u64) {
        if let Some(pending) = self.shared.pending.lock().unwrap().remove(&id) {
            let _ = pending.sender.send(Err("Remote request cancelled".into()));
        }
        let _ = self.shared.tx.try_send(Envelope {
            id: 0,
            request: Request::Cancel { id },
        });
    }
    pub fn forward(&self, local: u16, remote: u16) -> Result<Forward, String> {
        if !self.ready() {
            return Err("Connect and admit the remote workspace first".into());
        }
        if remote == 0 {
            return Err("Remote port must be nonzero".into());
        }
        let listener =
            TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, local)).map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let local = listener.local_addr().map_err(|e| e.to_string())?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicUsize::new(0));
        let state = self.shared.clone();
        let cancel = stop.clone();
        let finished = done.clone();
        std::thread::spawn(move || {
            while !cancel.load(Ordering::Acquire) && !state.stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((socket, _)) => {
                        if active.load(Ordering::Acquire) >= 8 {
                            drop(socket);
                            continue;
                        }
                        active.fetch_add(1, Ordering::AcqRel);
                        let active = active.clone();
                        let state = state.clone();
                        let cancel = cancel.clone();
                        std::thread::spawn(move || {
                            let _ = forward_socket(socket, remote, &state, &cancel);
                            active.fetch_sub(1, Ordering::AcqRel);
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(20))
                    }
                    Err(_) => break,
                }
            }
            drop(listener);
            while active.load(Ordering::Acquire) != 0 {
                std::thread::sleep(Duration::from_millis(20));
            }
            finished.store(true, Ordering::Release);
        });
        Ok(Forward {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            local,
            remote,
            stop,
            done,
        })
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.stop();
    }
}
pub struct Forward {
    pub id: u64,
    pub local: u16,
    pub remote: u16,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
}
impl Forward {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
    pub fn finished(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }
    pub fn stamp(&self) -> String {
        format!("{}:127.0.0.1:{}:{}", self.id, self.local, self.remote)
    }
}
impl Drop for Forward {
    fn drop(&mut self) {
        self.stop();
    }
}
fn wait(id: u64, reply: Reply, state: &Arc<Shared>, cancel: &AtomicBool) -> Result<Value, String> {
    loop {
        if cancel.load(Ordering::Acquire) || state.stop.load(Ordering::Acquire) {
            state.pending.lock().unwrap().remove(&id);
            let _ = state.tx.try_send(Envelope {
                id: 0,
                request: Request::Cancel { id },
            });
            return Err("Forward stopped".into());
        }
        match reply.recv_timeout(Duration::from_millis(100)) {
            Ok(value) => return value,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => return Err("Remote reply disconnected".into()),
        }
    }
}
fn forward_socket(
    mut socket: TcpStream,
    port: u16,
    state: &Arc<Shared>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    socket
        .set_read_timeout(Some(Duration::from_millis(20)))
        .map_err(|e| e.to_string())?;
    socket
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    let stream = NEXT.fetch_add(1, Ordering::Relaxed);
    let (id, reply) = request(state, Request::ForwardConnect { port, stream })?;
    match wait(id, reply, state, cancel) {
        Ok(value) if value["stream"].as_u64() == Some(stream) => {}
        result => {
            close_forward(state, stream);
            return Err(match result {
                Err(error) => error,
                Ok(_) => "Unexpected forwarded stream ID".into(),
            });
        }
    }
    let result = (|| {
        let mut bytes = [0; 32768];
        while !cancel.load(Ordering::Acquire) && !state.stop.load(Ordering::Acquire) {
            let count = match socket.read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => count,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    0
                }
                Err(error) => return Err(error.to_string()),
            };
            let (id, reply) = request(
                state,
                Request::ForwardPump {
                    stream,
                    data: bytes[..count].to_vec(),
                },
            )?;
            let value = wait(id, reply, state, cancel)?;
            let data: Vec<u8> =
                serde_json::from_value(value["data"].clone()).map_err(|e| e.to_string())?;
            socket.write_all(&data).map_err(|e| e.to_string())?;
            if value["eof"].as_bool() == Some(true) {
                break;
            }
        }
        Ok(())
    })();
    close_forward(state, stream);
    let _ = socket.shutdown(Shutdown::Both);
    result
}
fn close_forward(state: &Arc<Shared>, stream: u64) {
    if state.stop.load(Ordering::Acquire) {
        return;
    }
    let closed = request(state, Request::ForwardClose { stream })
        .ok()
        .and_then(|(_, reply)| reply.recv_timeout(Duration::from_secs(5)).ok())
        .is_some_and(|result| result.is_ok());
    // An unacknowledged stream close cannot be retired as successful. Retain
    // the session owner while forcing its transport and all remote sockets down.
    if !closed {
        state.stop.store(true, Ordering::Release);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn target_plans_do_not_start_or_install_anything() {
        let target = Target {
            kind: Kind::Docker,
            target: "chosen-container".into(),
            local_cwd: PathBuf::from(if cfg!(windows) {
                r"C:\local\project"
            } else {
                "/local/project"
            }),
            ..Default::default()
        };
        let command = target.server().unwrap();
        assert_eq!(command.program, "docker");
        assert!(command.args.contains(&"remote-server".into()));
        assert!(command.args.contains(&"--admit-client".into()));
        assert!(!command
            .args
            .iter()
            .any(|arg| matches!(arg.as_str(), "up" | "build" | "install")));
    }
    #[test]
    fn all_remote_paths_and_commands_remain_arguments() {
        let target = Target {
            kind: Kind::Wsl,
            target: "Ubuntu".into(),
            root: "/home/me/project with spaces".into(),
            local_cwd: PathBuf::from(if cfg!(windows) {
                r"C:\local\project"
            } else {
                "/local/project"
            }),
            ..Default::default()
        };
        let command = target.server().unwrap();
        assert!(command.args.contains(&target.root));
        assert!(target.run_python("../escape.py", None).is_err());
    }
}
