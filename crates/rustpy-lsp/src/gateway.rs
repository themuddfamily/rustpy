//! Gateway protocol over an authenticated, owned stdio transport. No listener,
//! project hooks, interpreter discovery, plugin execution or package installation.
use rustpy_core::{CancellationToken, TextSize};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{Shutdown, TcpStream},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
pub const PROTOCOL: u32 = 1;
pub const MAX_FRAME: usize = 16 * 1024 * 1024;
pub const MAX_FILE: usize = 2 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Envelope {
    pub id: u64,
    pub request: Request,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Hello {
        version: String,
        protocol: u32,
        root: String,
        admitted: bool,
        python: (u32, u32),
    },
    List {
        path: String,
        offset: usize,
    },
    Read {
        path: String,
    },
    Write {
        path: String,
        base: String,
        text: String,
    },
    Mkdir {
        path: String,
    },
    Analyze {
        path: String,
        text: String,
        version: u64,
        cursor: u32,
        query: Query,
    },
    ForwardConnect {
        port: u16,
        stream: u64,
    },
    ForwardPump {
        stream: u64,
        data: Vec<u8>,
    },
    ForwardClose {
        stream: u64,
    },
    Cancel {
        id: u64,
    },
    Shutdown,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Query {
    Diagnostics,
    Completion,
    Definition,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub result: Option<Value>,
    pub error: Option<String>,
}
pub fn read_frame(input: &mut impl Read) -> Result<Option<Envelope>, String> {
    read_value(input)
}
pub fn read_value<T: serde::de::DeserializeOwned>(
    input: &mut impl Read,
) -> Result<Option<T>, String> {
    let mut length = [0; 4];
    match input.read(&mut length[..1]) {
        Ok(0) => return Ok(None),
        Ok(_) => {}
        Err(error) => return Err(error.to_string()),
    }
    input
        .read_exact(&mut length[1..])
        .map_err(|e| e.to_string())?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_FRAME {
        return Err("Gateway frame exceeds the protocol limit".into());
    }
    let mut bytes = vec![0; length];
    input.read_exact(&mut bytes).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| e.to_string())
}
pub fn write_frame(output: &mut impl Write, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_FRAME {
        return Err("Gateway frame exceeds the protocol limit".into());
    }
    output
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .and_then(|_| output.write_all(&bytes))
        .and_then(|_| output.flush())
        .map_err(|e| e.to_string())
}
pub fn revision(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn relative(path: &str) -> Result<PathBuf, String> {
    let value = Path::new(path);
    if path.contains(['\\', '\0', ':'])
        || value.is_absolute()
        || value
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err("Use a relative path inside the admitted remote root".into());
    }
    Ok(value.into())
}
struct Session {
    root: PathBuf,
    admitted: bool,
    python: (u32, u32),
    forwards: HashMap<u64, TcpStream>,
    next_forward: u64,
}
impl Session {
    fn path(&self, path: &str) -> Result<PathBuf, String> {
        if std::fs::symlink_metadata(&self.root)
            .map_err(|e| e.to_string())?
            .file_type()
            .is_symlink()
            || self.root.canonicalize().map_err(|e| e.to_string())? != self.root
        {
            return Err("The captured remote root changed; reconnect explicitly".into());
        }
        let relative = relative(path)?;
        let mut target = self.root.clone();
        for part in relative.components() {
            target.push(part.as_os_str());
            if let Ok(metadata) = std::fs::symlink_metadata(&target) {
                if metadata.file_type().is_symlink() {
                    return Err("Gateway paths cannot traverse symbolic links".into());
                }
            }
        }
        if target.exists() {
            let canonical = target.canonicalize().map_err(|e| e.to_string())?;
            if !canonical.starts_with(&self.root) {
                return Err("Remote path escaped the captured root".into());
            }
        }
        Ok(target)
    }
    fn read(&self, path: &str) -> Result<(PathBuf, String, String), String> {
        let file = self.path(path)?;
        let metadata = std::fs::metadata(&file).map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.len() > MAX_FILE as u64 {
            return Err("Only text files up to 2 MiB can be opened".into());
        }
        let mut bytes = Vec::new();
        std::fs::File::open(&file)
            .map_err(|e| e.to_string())?
            .take(MAX_FILE as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > MAX_FILE {
            return Err("Remote file grew beyond the text limit".into());
        }
        let stamp = revision(&bytes);
        let text = String::from_utf8(bytes).map_err(|_| "This remote file is not UTF-8 text")?;
        Ok((file, text, stamp))
    }
    fn execute(&mut self, request: Request, cancel: &CancellationToken) -> Result<Value, String> {
        if let Request::Hello {
            version,
            protocol,
            root,
            admitted,
            python,
        } = request
        {
            if self.admitted {
                return Err("This gateway session is already initialized".into());
            }
            if version != env!("CARGO_PKG_VERSION") || protocol != PROTOCOL {
                return Err(format!("Gateway version mismatch: server {} protocol {PROTOCOL}, client {version} protocol {protocol}",env!("CARGO_PKG_VERSION")));
            }
            let root = PathBuf::from(root)
                .canonicalize()
                .map_err(|e| e.to_string())?;
            if root != self.root || !admitted {
                return Err(
                    "The client did not explicitly admit this exact target and root".into(),
                );
            }
            if python.0 != 3 || !(8..=14).contains(&python.1) {
                return Err("Choose a supported Python static-analysis version".into());
            }
            self.admitted = true;
            self.python = python;
            return Ok(
                json!({"version":env!("CARGO_PKG_VERSION"),"protocol":PROTOCOL,"root":self.root,"capabilities":["list","read","checked_write","analysis","completion","definition","loopback_forward"]}),
            );
        }
        if !self.admitted {
            return Err("Initialize and admit this remote workspace first".into());
        }
        cancel.check().map_err(|e| e.to_string())?;
        match request {
            Request::List { path, offset } => {
                let directory = self.path(&path)?;
                let mut rows = Vec::new();
                for entry in std::fs::read_dir(directory).map_err(|e| e.to_string())? {
                    cancel.check().map_err(|e| e.to_string())?;
                    let entry = entry.map_err(|e| e.to_string())?;
                    let metadata = entry.file_type().map_err(|e| e.to_string())?;
                    if metadata.is_symlink() {
                        continue;
                    }
                    if rows.len() >= 20000 {
                        return Err("Directory exceeds the 20,000 entry browse limit".into());
                    }
                    let name = entry
                        .file_name()
                        .into_string()
                        .map_err(|_| "Remote name is not UTF-8")?;
                    let child = if path.is_empty() {
                        name.clone()
                    } else {
                        format!("{path}/{name}")
                    };
                    rows.push(json!({"name":name,"path":child,"directory":metadata.is_dir(),"size":entry.metadata().map(|m|m.len()).unwrap_or(0)}));
                }
                rows.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
                let total = rows.len();
                let rows = rows.into_iter().skip(offset).take(500).collect::<Vec<_>>();
                Ok(json!({"entries":rows,"next":if offset+500<total{Some(offset+500)}else{None}}))
            }
            Request::Read { path } => {
                let (_, text, stamp) = self.read(&path)?;
                Ok(json!({"path":path,"text":text,"revision":stamp}))
            }
            Request::Write { path, base, text } => {
                if text.len() > MAX_FILE {
                    return Err("Remote text exceeds 2 MiB".into());
                }
                let destination = self.path(&path)?;
                let current = if destination.exists() {
                    let (_, _, stamp) = self.read(&path)?;
                    stamp
                } else {
                    String::new()
                };
                if current != base {
                    return Err("Remote file changed since it was opened. Download/compare before overwriting; no write was performed.".into());
                }
                cancel.check().map_err(|e| e.to_string())?;
                let parent = destination.parent().ok_or("Missing remote parent")?;
                if !parent.is_dir() {
                    return Err("Create the remote parent directory first".into());
                }
                let temporary = parent.join(format!(
                    ".rustpy-gateway-{}-{}.tmp",
                    std::process::id(),
                    crate::gateway::revision(text.as_bytes())
                ));
                let mut output = std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&temporary)
                    .map_err(|e| e.to_string())?;
                let result = (|| -> Result<(), String> {
                    for bytes in text.as_bytes().chunks(65536) {
                        cancel.check().map_err(|e| e.to_string())?;
                        output.write_all(bytes).map_err(|e| e.to_string())?;
                    }
                    if let Ok(metadata) = std::fs::metadata(&destination) {
                        output
                            .set_permissions(metadata.permissions())
                            .map_err(|e| e.to_string())?;
                    }
                    output.sync_all().map_err(|e| e.to_string())?;
                    drop(output);
                    let latest = if destination.exists() {
                        self.read(&path)?.2
                    } else {
                        String::new()
                    };
                    if latest != base {
                        return Err(
                            "Remote file changed during upload; no overwrite was performed".into(),
                        );
                    }
                    cancel.check().map_err(|e| e.to_string())?;
                    std::fs::rename(&temporary, &destination).map_err(|e| e.to_string())?;
                    Ok(())
                })();
                if result.is_err() {
                    let _ = std::fs::remove_file(&temporary);
                }
                result?;
                Ok(json!({"path":path,"revision":revision(text.as_bytes())}))
            }
            Request::Mkdir { path } => {
                let path = self.path(&path)?;
                std::fs::create_dir(&path).map_err(|e| e.to_string())?;
                Ok(json!({"created":true}))
            }
            Request::Analyze {
                path,
                text,
                version,
                cursor,
                query,
            } => self.analyze(&path, &text, version, cursor, query, cancel),
            Request::ForwardConnect {
                port,
                stream: stream_id,
            } => {
                if port == 0
                    || stream_id == 0
                    || self.forwards.contains_key(&stream_id)
                    || self.forwards.len() >= 16
                {
                    return Err("Invalid port or too many forwarded connections".into());
                }
                let stream = TcpStream::connect_timeout(
                    &([127, 0, 0, 1], port).into(),
                    Duration::from_secs(3),
                )
                .map_err(|e| e.to_string())?;
                cancel.check().map_err(|e| e.to_string())?;
                stream
                    .set_read_timeout(Some(Duration::from_millis(20)))
                    .map_err(|e| e.to_string())?;
                stream
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .map_err(|e| e.to_string())?;
                self.next_forward = self.next_forward.max(stream_id);
                self.forwards.insert(stream_id, stream);
                Ok(json!({"stream":stream_id}))
            }
            Request::ForwardPump { stream, data } => {
                if data.len() > 32768 {
                    return Err("Forward frame exceeds 32 KiB".into());
                }
                let socket = self
                    .forwards
                    .get_mut(&stream)
                    .ok_or("Unknown forwarded stream")?;
                socket.write_all(&data).map_err(|e| e.to_string())?;
                let mut bytes = vec![0; 32768];
                let count = match socket.read(&mut bytes) {
                    Ok(count) => Some(count),
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        None
                    }
                    Err(error) => return Err(error.to_string()),
                };
                let eof = count == Some(0);
                bytes.truncate(count.unwrap_or(0));
                if eof {
                    self.forwards.remove(&stream);
                }
                Ok(json!({"data":bytes,"eof":eof}))
            }
            Request::ForwardClose { stream } => {
                if let Some(socket) = self.forwards.remove(&stream) {
                    let _ = socket.shutdown(Shutdown::Both);
                }
                Ok(json!({"closed":true}))
            }
            Request::Shutdown => {
                for (_, socket) in self.forwards.drain() {
                    let _ = socket.shutdown(Shutdown::Both);
                }
                Ok(json!({"shutdown":true}))
            }
            Request::Cancel { .. } | Request::Hello { .. } => Err("Invalid session request".into()),
        }
    }
    fn analyze(
        &self,
        path: &str,
        text: &str,
        version: u64,
        cursor: u32,
        query: Query,
        cancel: &CancellationToken,
    ) -> Result<Value, String> {
        if text.len() > MAX_FILE {
            return Err("Remote buffer exceeds 2 MiB".into());
        }
        let file = self.path(path)?;
        let mut paths = Vec::new();
        let mut bytes = 0usize;
        let mut truncated = false;
        let db = rustpy_semantic::Analysis::new(rustpy_semantic::module::ModuleResolver::new(
            vec![self.root.clone()],
            vec![],
            self.python,
        ));
        Catalogue {
            root: &self.root,
            db: &db,
            cancel,
        }
        .collect(&mut paths, &mut bytes, &mut truncated)?;
        db.set_file_text(&file, text);
        if !paths.contains(&file) {
            paths.push(file.clone());
        }
        let snapshot = db
            .snapshot(&paths, cancel)
            .map_err(|error| format!("Remote analysis snapshot: {error:?}"))?;
        let analysis = snapshot.analysis();
        let info = analysis
            .file_for_path(&file)
            .ok_or("Remote analysis could not load the buffer")?;
        let cursor = TextSize::new(cursor.min(text.len() as u32));
        let result = match query {
            Query::Completion => {
                let items=rustpy_semantic::features::complete(analysis,&info,cursor).into_iter().filter(|item|item.label.len()<=512&&item.insert.len()<=4096).take(500).map(|item|json!({"label":item.label.to_string(),"insert":item.insert,"detail":item.detail.chars().take(512).collect::<String>(),"kind":format!("{:?}",item.kind)})).collect::<Vec<_>>();
                json!({"items":items})
            }
            Query::Definition => {
                let locations=rustpy_semantic::features::definition_at(analysis,&info,cursor).into_iter().filter_map(|location|{let relative=location.path.strip_prefix(&self.root).ok()?;Some(json!({"path":relative.to_string_lossy().replace('\\',"/"),"start":u32::from(location.range.start()),"end":u32::from(location.range.end())}))}).take(100).collect::<Vec<_>>();
                json!({"locations":locations})
            }
            Query::Diagnostics => {
                let options = rustpy_analysis::inspections::InspectionOptions {
                    editorconfig_enabled: false,
                    python_version: self.python,
                    style: rustpy_settings::style_inspections::StyleInspections {
                        interpreter: None,
                        pep8_backend: "builtin".into(),
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let options = rustpy_analysis::file_analysis::FileAnalysisOptions {
                    inspections: &options,
                    grammar: false,
                    dictionary: None,
                    stub_supplements: rustpy_analysis::file_analysis::StubSupplementPolicy::Include,
                    dictionary_actions:
                        rustpy_analysis::file_analysis::DictionaryActionPolicy::ReplacementsOnly,
                };
                let problems = rustpy_analysis::file_analysis::analyze_python(
                    analysis, &info, &options, cancel,
                )
                .map_err(|e| e.to_string())?;
                let diagnostics=problems.into_iter().take(2000).map(|problem|{let diagnostic=problem.diagnostic;json!({"start":u32::from(diagnostic.range.start()),"end":u32::from(diagnostic.range.end()),"message":diagnostic.message.chars().take(512).collect::<String>(),"code":diagnostic.code.to_string(),"severity":format!("{:?}",diagnostic.severity)})}).collect::<Vec<_>>();
                json!({"diagnostics":diagnostics})
            }
        };
        cancel.check().map_err(|e| e.to_string())?;
        Ok(json!({"version":version,"query":query,"truncated":truncated,"payload":result}))
    }
}
/// One request's admitted root, analysis snapshot builder and cancellation.
struct Catalogue<'a> {
    root: &'a Path,
    db: &'a rustpy_semantic::Analysis,
    cancel: &'a CancellationToken,
}
impl Catalogue<'_> {
    fn collect(
        &self,
        paths: &mut Vec<PathBuf>,
        bytes: &mut usize,
        truncated: &mut bool,
    ) -> Result<(), String> {
        self.walk(self.root, 0, paths, bytes, truncated)
    }
    fn walk(
        &self,
        path: &Path,
        depth: usize,
        paths: &mut Vec<PathBuf>,
        bytes: &mut usize,
        truncated: &mut bool,
    ) -> Result<(), String> {
        self.cancel.check().map_err(|e| e.to_string())?;
        if depth > 32 || paths.len() >= 5000 || *bytes >= 32 * 1024 * 1024 {
            *truncated = true;
            return Ok(());
        }
        for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
            self.cancel.check().map_err(|e| e.to_string())?;
            let entry = entry.map_err(|e| e.to_string())?;
            let metadata = entry.file_type().map_err(|e| e.to_string())?;
            if metadata.is_symlink() {
                continue;
            }
            let path = entry.path();
            if metadata.is_dir() {
                let name = entry.file_name();
                if [
                    ".git",
                    ".venv",
                    "venv",
                    "node_modules",
                    "__pycache__",
                    ".rustpy",
                ]
                .iter()
                .any(|skip| name == *skip)
                {
                    continue;
                }
                self.walk(&path, depth + 1, paths, bytes, truncated)?;
            } else if path
                .extension()
                .is_some_and(|extension| extension == "py" || extension == "pyi")
                && entry
                    .metadata()
                    .is_ok_and(|metadata| metadata.len() <= MAX_FILE as u64)
            {
                if paths.len() >= 5000 || *bytes >= 32 * 1024 * 1024 {
                    *truncated = true;
                    break;
                }
                let mut text = String::new();
                if std::fs::File::open(&path)
                    .and_then(|file| file.take(MAX_FILE as u64 + 1).read_to_string(&mut text))
                    .is_ok()
                    && text.len() <= MAX_FILE
                    && *bytes + text.len() <= 32 * 1024 * 1024
                {
                    *bytes += text.len();
                    let normalized = text
                        .trim_start_matches('\u{feff}')
                        .replace("\r\n", "\n")
                        .replace('\r', "\n");
                    self.db.set_file_text(&path, &normalized);
                    paths.push(path);
                }
            }
        }
        Ok(())
    }
}
/// Invoked only by the explicit headless remote-server CLI command.
pub fn run_stdio(root: PathBuf, admit_client: bool) -> Result<(), String> {
    if !admit_client {
        return Err("remote-server requires explicit --admit-client for the selected root".into());
    }
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    if !root.is_dir() {
        return Err("Choose an existing remote directory".into());
    }
    let cancellations = Arc::new(Mutex::new(HashMap::<u64, CancellationToken>::new()));
    let writer = Arc::new(Mutex::new(std::io::stdout()));
    let (tx, rx) = std::sync::mpsc::sync_channel::<(Envelope, CancellationToken)>(16);
    let active = cancellations.clone();
    let output = writer.clone();
    let worker = std::thread::Builder::new()
        .name("remote-engine".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let mut session = Session {
                root,
                admitted: false,
                python: (3, 12),
                forwards: HashMap::new(),
                next_forward: 0,
            };
            for (envelope, cancel) in rx {
                let result = session.execute(envelope.request, &cancel);
                active.lock().unwrap().remove(&envelope.id);
                let response = match result {
                    Ok(value) => Response {
                        id: envelope.id,
                        result: Some(value),
                        error: None,
                    },
                    Err(error) => Response {
                        id: envelope.id,
                        result: None,
                        error: Some(error),
                    },
                };
                if write_frame(&mut *output.lock().unwrap(), &response).is_err() {
                    break;
                }
            }
        })
        .map_err(|e| e.to_string())?;
    let mut input = std::io::stdin().lock();
    let mut result = Ok(());
    loop {
        let envelope = match read_frame(&mut input) {
            Ok(Some(envelope)) => envelope,
            Ok(None) => break,
            Err(error) => {
                result = Err(error);
                break;
            }
        };
        if let Request::Cancel { id } = envelope.request {
            if let Some(token) = cancellations.lock().unwrap().get(&id) {
                token.cancel();
            }
            continue;
        }
        let shutdown = matches!(envelope.request, Request::Shutdown);
        let cancel = CancellationToken::new();
        let duplicate = {
            let mut pending = cancellations.lock().unwrap();
            if pending.contains_key(&envelope.id) || pending.len() >= 32 {
                true
            } else {
                pending.insert(envelope.id, cancel.clone());
                false
            }
        };
        if duplicate {
            let _ = write_frame(
                &mut *writer.lock().unwrap(),
                &Response {
                    id: envelope.id,
                    result: None,
                    error: Some("Duplicate request or full request queue".into()),
                },
            );
            continue;
        }
        let id = envelope.id;
        if tx.try_send((envelope, cancel)).is_err() {
            cancellations.lock().unwrap().remove(&id);
            let _ = write_frame(
                &mut *writer.lock().unwrap(),
                &Response {
                    id,
                    result: None,
                    error: Some(
                        "Remote engine is busy; retry after pending requests complete".into(),
                    ),
                },
            );
        }
        if shutdown {
            break;
        }
    }
    drop(tx);
    for token in cancellations.lock().unwrap().values() {
        token.cancel();
    }
    worker
        .join()
        .map_err(|_| "Remote engine worker stopped unexpectedly".to_owned())?;
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frames_are_bounded_and_paths_cannot_escape() {
        for path in [
            "../secret",
            "/etc/passwd",
            "a/../secret",
            "C:/secret",
            "a\\b",
        ] {
            assert!(relative(path).is_err());
        }
        let mut input = &u32::MAX.to_be_bytes()[..];
        assert!(read_value::<Envelope>(&mut input).is_err());
    }
    #[test]
    fn admission_and_cancel_are_required_before_any_mutation() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let mut session = Session {
            root: root.clone(),
            admitted: false,
            python: (3, 12),
            forwards: HashMap::new(),
            next_forward: 0,
        };
        let token = CancellationToken::new();
        assert!(session
            .execute(
                Request::List {
                    path: String::new(),
                    offset: 0
                },
                &token
            )
            .is_err());
        assert!(session
            .execute(
                Request::Hello {
                    version: "different".into(),
                    protocol: PROTOCOL,
                    root: root.to_string_lossy().into_owned(),
                    admitted: true,
                    python: (3, 12)
                },
                &token
            )
            .is_err());
        session
            .execute(
                Request::Hello {
                    version: env!("CARGO_PKG_VERSION").into(),
                    protocol: PROTOCOL,
                    root: root.to_string_lossy().into_owned(),
                    admitted: true,
                    python: (3, 12),
                },
                &token,
            )
            .unwrap();
        token.cancel();
        assert!(session
            .execute(
                Request::Write {
                    path: "new.py".into(),
                    base: String::new(),
                    text: "value=1".into()
                },
                &token
            )
            .is_err());
        assert!(!root.join("new.py").exists());
    }
    #[cfg(unix)]
    #[test]
    fn remote_filesystem_rejects_symlink_authority_escape() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.py"), "secret=1").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).unwrap();
        let session = Session {
            root: root.path().canonicalize().unwrap(),
            admitted: true,
            python: (3, 12),
            forwards: HashMap::new(),
            next_forward: 0,
        };
        assert!(session.read("linked/secret.py").is_err());
    }
    #[test]
    fn checked_writes_reject_stale_revisions() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.py"), "value=1\n").unwrap();
        let mut session = Session {
            root: root.path().canonicalize().unwrap(),
            admitted: true,
            python: (3, 12),
            forwards: HashMap::new(),
            next_forward: 0,
        };
        assert!(session
            .execute(
                Request::Write {
                    path: "a.py".into(),
                    base: "stale".into(),
                    text: "changed".into()
                },
                &CancellationToken::new()
            )
            .is_err());
        assert_eq!(
            std::fs::read_to_string(root.path().join("a.py")).unwrap(),
            "value=1\n"
        );
    }
    #[test]
    fn actual_remote_engine_completes_and_finds_project_definitions() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("model.py"),
            "class RemoteWidget:\n    pass\n",
        )
        .unwrap();
        let session = Session {
            root: root.path().canonicalize().unwrap(),
            admitted: true,
            python: (3, 12),
            forwards: HashMap::new(),
            next_forward: 0,
        };
        let text = "from model import RemoteWidget\nRemoteWidget\n";
        let result = session
            .analyze(
                "main.py",
                text,
                1,
                text.len() as u32 - 2,
                Query::Definition,
                &CancellationToken::new(),
            )
            .unwrap();
        assert_eq!(result["version"], 1);
        assert!(result["payload"]["locations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|location| location["path"] == "model.py"));
        let completion = session
            .analyze(
                "main.py",
                "from model import RemoteWidget\nRemoteW",
                2,
                38,
                Query::Completion,
                &CancellationToken::new(),
            )
            .unwrap();
        assert!(completion["payload"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["label"] == "RemoteWidget"));
        let diagnostics = session
            .analyze(
                "bad.py",
                "def broken(:\n",
                3,
                0,
                Query::Diagnostics,
                &CancellationToken::new(),
            )
            .unwrap();
        assert!(!diagnostics["payload"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty());
    }
}
