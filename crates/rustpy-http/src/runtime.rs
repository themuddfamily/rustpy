//! Captured request ownership. Cancellation retains the worker until bounded transport retires.
use crate::{HttpRequest, HttpResponse};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
#[derive(Clone, Default)]
pub struct Control {
    cancel: Arc<AtomicBool>,
    pub phase: Arc<AtomicU8>,
    pub received: Arc<AtomicUsize>,
    socket: Arc<std::sync::Mutex<Option<std::net::TcpStream>>>,
    messages: Arc<std::sync::Mutex<Vec<String>>>,
    outgoing: Arc<std::sync::Mutex<Vec<String>>>,
}
impl Control {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
        if let Ok(socket) = self.socket.lock() {
            if let Some(socket) = socket.as_ref() {
                let _ = socket.shutdown(std::net::Shutdown::Both);
            }
        }
    }
    pub fn set_socket(&self, socket: std::net::TcpStream) {
        if self.cancelled() {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
        if let Ok(mut value) = self.socket.lock() {
            *value = Some(socket);
        }
    }
    pub fn message(&self, message: String) {
        if let Ok(mut messages) = self.messages.lock() {
            if messages.len() >= 1000 {
                messages.remove(0);
            }
            messages.push(message);
        }
    }
    pub fn messages(&self) -> Vec<String> {
        self.messages
            .lock()
            .map(|messages| messages.clone())
            .unwrap_or_default()
    }
    pub fn send(&self, message: String) {
        if message.len() <= 1024 * 1024 {
            if let Ok(mut queue) = self.outgoing.lock() {
                if queue.len() < 100 {
                    queue.push(message);
                }
            }
        }
    }
    pub fn take_outgoing(&self) -> Vec<String> {
        self.outgoing
            .lock()
            .map(|mut queue| std::mem::take(&mut *queue))
            .unwrap_or_default()
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }
    pub fn phase(&self) -> &'static str {
        match self.phase.load(Ordering::Relaxed) {
            0 => "Queued",
            1 => "Pre-request script",
            2 => "Connecting / sending",
            3 => "Receiving",
            4 => "Response script",
            _ => "Finished",
        }
    }
}
/// Environment captured by a configuration, never installed in the IDE process.
#[derive(Clone, Debug)]
pub struct ProcessEnvironment {
    pub inherit: bool,
    pub variables: HashMap<String, String>,
    pub working_dir: Option<PathBuf>,
}
impl Default for ProcessEnvironment {
    fn default() -> Self {
        Self {
            inherit: true,
            variables: HashMap::new(),
            working_dir: None,
        }
    }
}
impl ProcessEnvironment {
    pub fn apply(&self, command: &mut std::process::Command) {
        if !self.inherit {
            command.env_clear();
        }
        command.envs(&self.variables);
    }
}
#[derive(Clone)]
pub struct Options {
    pub project: PathBuf,
    pub base: PathBuf,
    pub variables: HashMap<String, String>,
    pub globals: HashMap<String, String>,
    pub process_environment: ProcessEnvironment,
    pub timeout: Duration,
    pub node: String,
    pub grpcurl: String,
    pub logs: Vec<String>,
}
impl Options {
    pub fn new(base: PathBuf) -> Self {
        Self {
            project: base.clone(),
            base,
            variables: HashMap::new(),
            globals: HashMap::new(),
            process_environment: ProcessEnvironment::default(),
            timeout: Duration::from_secs(30),
            node: "node".into(),
            grpcurl: "grpcurl".into(),
            logs: Vec::new(),
        }
    }
}
/// Apply only this execution's changes, preserving globals written by parallel requests.
pub fn merge_globals(
    current: &mut HashMap<String, String>,
    baseline: &HashMap<String, String>,
    completed: HashMap<String, String>,
) {
    for key in baseline.keys() {
        if !completed.contains_key(key) {
            current.remove(key);
        }
    }
    for (key, value) in completed {
        if baseline.get(&key) != Some(&value) {
            current.insert(key, value);
        }
    }
}
pub struct Completed {
    pub request: HttpRequest,
    pub response: HttpResponse,
    pub globals: HashMap<String, String>,
}
pub struct Job {
    pub control: Control,
    rx: crossbeam_channel::Receiver<Completed>,
    pub started: Instant,
}
impl Job {
    pub fn spawn(request: HttpRequest, options: Options) -> Self {
        let control = Control::default();
        let worker = control.clone();
        let (tx, rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            let result = execute(request, options, &worker);
            let _ = tx.send(result);
        });
        Self {
            control,
            rx,
            started: Instant::now(),
        }
    }
    pub fn try_result(&self) -> Result<Option<Completed>, String> {
        match self.rx.try_recv() {
            Ok(result) => Ok(Some(result)),
            Err(crossbeam_channel::TryRecvError::Empty) => Ok(None),
            Err(_) => Err("HTTP request worker stopped".into()),
        }
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        self.control.cancel();
    }
}
pub fn error(message: impl Into<String>) -> HttpResponse {
    HttpResponse {
        error: Some(message.into()),
        ..Default::default()
    }
}
fn script(
    source: &str,
    options: &mut Options,
    response: Option<&HttpResponse>,
    control: &Control,
) -> Result<Vec<crate::scripts::ScriptTest>, String> {
    let result = crate::scripts::run_with_environment(
        source,
        &options.base,
        &options.variables,
        &options.globals,
        response,
        control,
        &options.node,
        &options.process_environment,
    )?;
    options.logs.extend(result.logs);
    options.globals = result.globals;
    options.variables.extend(result.variables);
    Ok(result.tests)
}
pub fn execute(mut request: HttpRequest, mut options: Options, control: &Control) -> Completed {
    let mut tests = Vec::new();
    let result = (|| -> Result<HttpResponse, String> {
        if control.cancelled() {
            return Err("Request cancelled".into());
        }
        control.phase.store(1, Ordering::Relaxed);
        if let Some(source) = request.pre_script.clone() {
            tests.extend(script(&source, &mut options, None, control)?);
        }
        let mut variables = options.globals.clone();
        variables.extend(options.variables.clone());
        request = crate::resolve_request(&request, &variables);
        if request.url.contains("{{") {
            return Err("Request has unresolved URL variables".into());
        }
        if let Some(path) = &request.body_file {
            let body_path = options.base.join(path);
            request.binary_body = Some(
                rustpy_core::vfs::Vfs::new()
                    .read(
                        &body_path,
                        rustpy_core::vfs::ReadLimits {
                            max_file_bytes: 16 * 1024 * 1024,
                            max_cached_bytes: 0,
                            max_cached_files: 0,
                        },
                    )
                    .map_err(|error| {
                        format!("Cannot read request body {}: {error}", body_path.display())
                    })?
                    .bytes
                    .to_vec(),
            );
        }
        if request
            .body
            .as_ref()
            .is_some_and(|body| body.lines().any(|line| line.starts_with("< ")))
        {
            request.binary_body = Some(crate::workspace::multipart_body(
                request.body.as_deref().unwrap(),
                &options.base,
            )?);
        }
        if request.method == "GRAPHQL" {
            if let Some(bytes) = request.binary_body.take() {
                request.body = Some(
                    String::from_utf8(bytes)
                        .map_err(|error| format!("GraphQL body must be UTF-8: {error}"))?,
                );
                request.body_file = None;
            }
            request.method = "POST".into();
            if serde_json::from_str::<serde_json::Value>(request.body.as_deref().unwrap_or(""))
                .is_err()
            {
                request.body = Some(
                    serde_json::json!({"query":request.body.clone().unwrap_or_default()})
                        .to_string(),
                );
            }
            if !request
                .headers
                .iter()
                .any(|(key, _)| key.eq_ignore_ascii_case("content-type"))
            {
                request
                    .headers
                    .push(("Content-Type".into(), "application/json".into()));
            }
        }
        crate::auth::prepare(&mut request, &options, control)?;
        control.phase.store(2, Ordering::Relaxed);
        let mut response = match request.method.as_str() {
            "WEBSOCKET" => crate::protocols::websocket(&request, &options, control),
            "GRPC" => crate::protocols::grpc(&request, &options, control),
            _ => crate::exec::execute_transport(&request, &options, control),
        };
        if control.cancelled() {
            return Err("Request cancelled".into());
        }
        control.phase.store(4, Ordering::Relaxed);
        if let Some(source) = request.handler.clone() {
            tests.extend(script(&source, &mut options, Some(&response), control)?);
        }
        response.tests = tests.clone();
        response.logs = options.logs.clone();
        if tests.iter().any(|test| !test.passed) {
            response.error = Some("Response handler assertions failed".into());
        }
        if control.cancelled() {
            return Err("Request cancelled".into());
        }
        if let Some(path) = request
            .save_response
            .as_ref()
            .filter(|_| response.status != 0)
        {
            save(
                &options.base,
                path,
                &response.bytes,
                request.overwrite_response,
            )?;
        }
        Ok(response)
    })();
    control.phase.store(5, Ordering::Relaxed);
    Completed {
        request,
        response: result.unwrap_or_else(error),
        globals: options.globals,
    }
}
fn save(base: &Path, path: &str, bytes: &[u8], overwrite: bool) -> Result<(), String> {
    use std::io::Write;
    let path = base.join(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    if overwrite {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    let mut file = options
        .open(&path)
        .map_err(|error| format!("Cannot save response {}: {error}", path.display()))?;
    file.write_all(bytes).map_err(|error| error.to_string())
}

#[cfg(test)]
mod configuration_environment_tests {
    use super::*;
    #[test]
    fn isolated_helper_environment_is_captured_without_mutating_parent_or_next_launch() {
        let parent = std::env::var_os("RUSTPY_HTTP_CAPTURED");
        let mut command = std::process::Command::new("node");
        command.env("RUSTPY_HTTP_DISCARDED", "old");
        ProcessEnvironment {
            inherit: false,
            variables: [("RUSTPY_HTTP_CAPTURED".into(), "two words".into())].into(),
            working_dir: None,
        }
        .apply(&mut command);
        let entries: Vec<_> = command
            .get_envs()
            .map(|(key, value)| (key.to_owned(), value.map(std::ffi::OsStr::to_owned)))
            .collect();
        assert_eq!(
            entries,
            vec![("RUSTPY_HTTP_CAPTURED".into(), Some("two words".into()))]
        );
        let mut later = std::process::Command::new("node");
        ProcessEnvironment::default().apply(&mut later);
        assert_eq!(later.get_envs().count(), 0);
        assert_eq!(std::env::var_os("RUSTPY_HTTP_CAPTURED"), parent);
    }
}
