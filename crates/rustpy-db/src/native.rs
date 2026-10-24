//! Typed native document/key-value sessions. No SQL or provider CLI emulation.
use crate::workspace::{Dialect, Source};
use anyhow::{bail, ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{sync_channel, Receiver, SyncSender},
        Arc,
    },
    thread::JoinHandle,
    time::Duration,
};
mod mongo;
mod mongo_document;
pub use mongo_document::prepare_mongo_document;
mod mongo_bridge;
mod redis;
#[cfg(test)]
mod tests;
mod transport;
pub const MAX_BYTES: usize = 1024 * 1024;
pub const MAX_ROWS: usize = 1000;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RedisValue {
    Missing,
    String(Vec<u8>),
    List(Vec<Vec<u8>>),
    Set(Vec<Vec<u8>>),
    Hash(Vec<(Vec<u8>, Vec<u8>)>),
    SortedSet(Vec<(Vec<u8>, f64)>),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RedisRecord {
    pub key: Vec<u8>,
    pub kind: String,
    pub ttl_ms: i64,
    pub value: RedisValue,
    pub next_cursor: Option<u64>,
    pub version: Option<Vec<u8>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RedisMutation {
    SetString(Vec<u8>),
    Delete,
    Expire { millis: i64 },
    HashSet { field: Vec<u8>, value: Vec<u8> },
    HashDelete { field: Vec<u8> },
    ListSet { index: i64, value: Vec<u8> },
    ListPush { value: Vec<u8>, left: bool },
    SetAdd(Vec<u8>),
    SetRemove(Vec<u8>),
    SortedSetAdd { member: Vec<u8>, score: f64 },
    SortedSetRemove(Vec<u8>),
}
#[derive(Clone, Debug)]
pub enum NativeRequest {
    RedisScan {
        cursor: u64,
        pattern: Vec<u8>,
        count: usize,
    },
    RedisRead {
        key: Vec<u8>,
        cursor: u64,
        limit: usize,
    },
    RedisCompare {
        key: Vec<u8>,
        expected: Option<Vec<u8>>,
        mutation: RedisMutation,
    },
    MongoCollections,
    MongoFind {
        collection: String,
        filter: Value,
        sort: Value,
        skip: u64,
        limit: usize,
    },
    MongoInsert {
        collection: String,
        document: Value,
    },
    MongoReplace {
        collection: String,
        expected: Value,
        replacement: Value,
    },
    MongoDelete {
        collection: String,
        expected: Value,
    },
}
impl NativeRequest {
    pub fn validate_for(&self, source: &Source) -> Result<()> {
        ensure!(
            !source.ddl_only,
            "Offline DDL sources cannot admit native requests"
        );
        ensure!(
            !source.read_only || !self.is_mutation(),
            "Native source is read-only"
        );
        let redis = matches!(
            self,
            Self::RedisScan { .. } | Self::RedisRead { .. } | Self::RedisCompare { .. }
        );
        ensure!(
            redis == (source.dialect == Dialect::Redis)
                && matches!(source.dialect, Dialect::Redis | Dialect::Mongo),
            "Request does not match the captured native provider"
        );
        Ok(())
    }
    pub fn is_mutation(&self) -> bool {
        matches!(
            self,
            Self::RedisCompare { .. }
                | Self::MongoInsert { .. }
                | Self::MongoReplace { .. }
                | Self::MongoDelete { .. }
        )
    }
}
#[derive(Clone, Debug)]
pub enum NativeEvent {
    Connected,
    RedisKeys {
        cursor: u64,
        keys: Vec<Vec<u8>>,
    },
    RedisValue(RedisRecord),
    MongoCollections(Vec<String>),
    MongoDocuments {
        documents: Vec<Value>,
        has_more: bool,
    },
    Mutation {
        affected: u64,
    },
    Conflict,
    Error(String),
    Retired,
}
#[derive(Clone, Debug)]
pub struct NativeReply {
    pub session_id: u64,
    pub source_id: u64,
    pub request_id: u64,
    pub event: NativeEvent,
}
struct Control {
    stop: AtomicBool,
    canceled: AtomicU64,
    active: AtomicU64,
    deadline: std::sync::Mutex<Option<std::time::Instant>>,
    sockets: std::sync::Mutex<std::collections::BTreeMap<u64, std::net::TcpStream>>,
    next_socket: AtomicU64,
}
impl Control {
    fn interrupted(&self) -> bool {
        let active = self.active.load(Ordering::Acquire);
        self.stop.load(Ordering::Acquire)
            || (active != 0 && self.canceled.load(Ordering::Acquire) == active)
            || self
                .deadline
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some_and(|deadline| std::time::Instant::now() >= deadline)
    }
    fn shutdown_sockets(&self) {
        for socket in self
            .sockets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
    }
    fn remaining(&self, fallback: Duration) -> Duration {
        self.deadline
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map(|deadline| {
                deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .min(fallback)
            })
            .unwrap_or(fallback)
            .max(Duration::from_millis(1))
    }
}
struct Retiring {
    label: String,
    thread: JoinHandle<()>,
}
static RETIRING: std::sync::Mutex<Vec<Retiring>> = std::sync::Mutex::new(Vec::new());
/// Retained dropped owners. Poll even when all database tabs have closed.
pub fn poll_retiring() -> Vec<String> {
    let mut owners = RETIRING.lock().unwrap_or_else(|e| e.into_inner());
    let mut labels = Vec::new();
    let mut i = 0;
    while i < owners.len() {
        if owners[i].thread.is_finished() {
            let _ = owners.swap_remove(i).thread.join();
        } else {
            labels.push(owners[i].label.clone());
            i += 1;
        }
    }
    labels
}
pub struct NativeSession {
    pub id: u64,
    pub source: Source,
    tx: SyncSender<(u64, NativeRequest)>,
    rx: Receiver<NativeReply>,
    control: Arc<Control>,
    thread: Option<JoinHandle<()>>,
    pending: Option<u64>,
    last: u64,
}
impl NativeSession {
    pub fn start(id: u64, source: Source, secret: String, timeout: Duration) -> Result<Self> {
        ensure!(
            matches!(source.dialect, Dialect::Redis | Dialect::Mongo),
            "Select a native Redis or Mongo source"
        );
        source.validate()?;
        ensure!(
            !source.ddl_only,
            "Offline DDL sources cannot open native network sessions"
        );
        ensure!(
            !source.host.is_empty() && source.port > 0,
            "Native source requires host and port"
        );
        let cfg = rustpy_net::config();
        let proxy_env: [(String, Option<String>); 8] = [
            "https_proxy",
            "HTTPS_PROXY",
            "http_proxy",
            "HTTP_PROXY",
            "all_proxy",
            "ALL_PROXY",
            "no_proxy",
            "NO_PROXY",
        ]
        .map(|name| (name.into(), rustpy_net::proxy::process_env(name)));
        let (tx, requests) = sync_channel::<(u64, NativeRequest)>(1);
        let (replies, rx) = sync_channel(4);
        let control = Arc::new(Control {
            stop: AtomicBool::new(false),
            canceled: AtomicU64::new(0),
            active: AtomicU64::new(0),
            deadline: std::sync::Mutex::new(None),
            sockets: Default::default(),
            next_socket: AtomicU64::new(1),
        });
        let state = control.clone();
        let captured = source.clone();
        let thread=std::thread::Builder::new().name(format!("native-db-{id}")).spawn(move||{
   let send=|request_id,event|{let _=replies.try_send(NativeReply{session_id:id,source_id:captured.id,request_id,event});};
   let timeout=timeout.clamp(Duration::from_secs(1),Duration::from_secs(60));
   let connected=(||->Result<Backend>{
    validate_transport(&captured,&cfg,&proxy_env)?;
    ensure!(!state.stop.load(Ordering::Acquire),"Native connection canceled");
    let policy=transport::Policy::capture(captured.clone(),cfg,proxy_env.to_vec(),state.clone(),timeout)?;
    *state.deadline.lock().unwrap_or_else(|e|e.into_inner())=Some(std::time::Instant::now()+timeout);
    let backend=match captured.dialect{Dialect::Redis=>Ok(Backend::Redis(redis::connect(&captured,&secret,policy)?)),Dialect::Mongo=>Ok(Backend::Mongo(mongo::connect(&captured,&secret,policy)?)),_=>unreachable!()};
    *state.deadline.lock().unwrap_or_else(|e|e.into_inner())=None;
    backend
   })();
   match connected{
    Ok(mut backend)=>{send(0,NativeEvent::Connected);
     while !state.stop.load(Ordering::Acquire){let Ok((request_id,request))=requests.recv_timeout(Duration::from_millis(100))else{continue};
      if state.stop.load(Ordering::Acquire){break;}
      let mutation=request.is_mutation();
      state.active.store(request_id,Ordering::Release);*state.deadline.lock().unwrap_or_else(|e|e.into_inner())=Some(std::time::Instant::now()+timeout);
      let result=(||{request.validate_for(&captured)?;ensure!(state.canceled.load(Ordering::Acquire)!=request_id,"Request canceled before admission");backend.request(request)})();
      let event=if state.canceled.load(Ordering::Acquire)==request_id||state.stop.load(Ordering::Acquire){NativeEvent::Error(if mutation{"Request canceled; mutation outcome may be uncertain. Refresh before editing again."}else{"Request canceled"}.into())}else{match result{Ok(event)=>event,Err(error)=>NativeEvent::Error(redact(&format!("{error:#}"),&secret))}};
      state.active.store(0,Ordering::Release);*state.deadline.lock().unwrap_or_else(|e|e.into_inner())=None;send(request_id,event);
     }
     backend.close();
    },Err(error)=>send(0,NativeEvent::Error(redact(&format!("{error:#}"),&secret))),
   }send(0,NativeEvent::Retired);
  })?;
        Ok(Self {
            id,
            source,
            tx,
            rx,
            control,
            thread: Some(thread),
            pending: None,
            last: 0,
        })
    }
    pub fn send(&mut self, request_id: u64, request: NativeRequest) -> Result<()> {
        ensure!(
            !self.control.stop.load(Ordering::Acquire) && self.alive(),
            "Native session is stopping"
        );
        ensure!(
            self.pending.is_none(),
            "Wait for the current native request"
        );
        ensure!(request_id > self.last, "Native request IDs must increase");
        request.validate_for(&self.source)?;
        self.tx.try_send((request_id, request))?;
        self.pending = Some(request_id);
        self.last = request_id;
        Ok(())
    }
    pub fn poll(&mut self) -> Vec<NativeReply> {
        let mut out = Vec::new();
        for _ in 0..4 {
            let Ok(reply) = self.rx.try_recv() else { break };
            if self.pending == Some(reply.request_id) {
                self.pending = None;
            }
            out.push(reply);
        }
        if self.thread.as_ref().is_some_and(JoinHandle::is_finished) {
            if let Some(thread) = self.thread.take() {
                if thread.join().is_err() {
                    out.push(NativeReply {
                        session_id: self.id,
                        source_id: self.source.id,
                        request_id: self.pending.take().unwrap_or(0),
                        event: NativeEvent::Error(
                            "Native worker failed; refresh before retrying mutations".into(),
                        ),
                    });
                }
            }
            self.pending = None;
        }
        out
    }
    pub fn stop(&mut self) {
        self.control.stop.store(true, Ordering::Release);
        self.control.shutdown_sockets();
    }
    pub fn cancel(&self, request_id: u64) {
        self.control.canceled.store(request_id, Ordering::Release);
        if self.control.active.load(Ordering::Acquire) == request_id {
            self.control.shutdown_sockets();
        }
    }
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }
    pub fn alive(&self) -> bool {
        self.thread.is_some()
    }
    pub fn lifecycle_stamp(&self) -> String {
        format!(
            "native-db:{}:source={}:pending={:?}:stop={}:alive={}",
            self.id,
            self.source.id,
            self.pending,
            self.control.stop.load(Ordering::Acquire),
            self.alive()
        )
    }
}
impl Drop for NativeSession {
    fn drop(&mut self) {
        self.stop();
        let label = self.lifecycle_stamp();
        if let Some(thread) = self.thread.take() {
            RETIRING
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(Retiring { label, thread });
        }
    }
}
enum Backend {
    Redis(redis::Connection),
    Mongo(mongo::Connection),
}
impl Backend {
    fn request(&mut self, request: NativeRequest) -> Result<NativeEvent> {
        match self {
            Self::Redis(c) => redis::request(c, request),
            Self::Mongo(c) => mongo::request(c, request),
        }
    }
    fn close(self) {
        drop(self);
    }
}
fn redact(message: &str, secret: &str) -> String {
    if secret.is_empty() {
        return message.chars().take(4096).collect();
    }
    let encoded = url::form_urlencoded::byte_serialize(secret.as_bytes()).collect::<String>();
    let mut uri = url::Url::parse("redis://localhost/").expect("static credential redaction URL");
    let _ = uri.set_password(Some(secret));
    message
        .replace(secret, "<credential>")
        .replace(&encoded, "<credential>")
        .replace(uri.password().unwrap_or(secret), "<credential>")
        .chars()
        .take(4096)
        .collect()
}
fn validate_transport(
    source: &Source,
    _cfg: &rustpy_net::NetConfig,
    _env: &[(String, Option<String>)],
) -> Result<()> {
    if let Some(tunnel) = &source.ssh {
        let _ =
            crate::session::captured_ssh_command(tunnel, &tunnel.remote_host, tunnel.remote_port)?;
    }
    let mut target = url::Url::parse("https://localhost/")?;
    target
        .set_host(Some(&source.host))
        .map_err(|_| anyhow::anyhow!("Invalid native host"))?;
    target
        .set_port(Some(source.port))
        .map_err(|_| anyhow::anyhow!("Invalid native port"))?;
    let tls = source
        .options
        .get("tls")
        .is_some_and(|value| value == "true");
    for (key, value) in &source.options {
        ensure!(
            [
                "tls",
                "tlsCAFile",
                "tlsClientCert",
                "tlsClientKey",
                "tlsCertificateKeyFile",
                "authSource"
            ]
            .contains(&key.as_str()),
            "Unsupported native connection option: {key}"
        );
        if key == "tls" {
            ensure!(
                matches!(value.as_str(), "true" | "false"),
                "tls must be true or false"
            );
        } else if key != "authSource" {
            let path = std::path::Path::new(value);
            ensure!(
                path.is_absolute() && path.is_file(),
                "Native TLS files must be existing absolute paths"
            );
            ensure!(tls, "TLS file options require tls=true");
        }
    }
    Ok(())
}
pub(crate) fn bounded(event: NativeEvent) -> Result<NativeEvent> {
    let bytes = match &event {
        NativeEvent::RedisKeys { keys, .. } => keys.iter().map(Vec::len).sum(),
        NativeEvent::RedisValue(record) => serde_json::to_vec(record)?.len(),
        NativeEvent::MongoDocuments { documents, .. } => serde_json::to_vec(documents)?.len(),
        NativeEvent::MongoCollections(names) => names.iter().map(String::len).sum(),
        _ => 0,
    };
    ensure!(
        bytes <= MAX_BYTES,
        "Native reply exceeds 1 MiB; narrow the selection"
    );
    Ok(event)
}
