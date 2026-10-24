//! Debug Adapter Protocol transport: `Content-Length` framed JSON over TCP.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, Receiver, Sender};
use serde_json::{json, Value};

/// An incoming protocol message.
#[derive(Clone, Debug)]
pub enum Message {
    Event {
        event: String,
        body: Value,
    },
    Response {
        request_seq: u64,
        command: String,
        success: bool,
        message: Option<String>,
        body: Value,
    },
    Request {
        seq: u64,
        command: String,
        arguments: Value,
    },
    /// The adapter could not be reached; nothing else arrives.
    ConnectFailed(String),
    TransportError(String),
}

/// A connection to a debug adapter. It is made in the background: requests
/// sent before it is up wait in a queue and go out once it is.
pub struct DapClient {
    _legacy: Option<crate::legacy_wire::LegacyBridge>,
    workers: Arc<Workers>,
    permit: Option<TransportPermit>,
    seq: u64,
    queued_bytes: Arc<AtomicUsize>,
    /// Encoded messages for the writer thread.
    outgoing: Sender<Vec<u8>>,
    pub incoming: Receiver<Message>,
    connected: Arc<AtomicBool>,
    /// Set when the client is dropped: stops connection attempts.
    closed: Arc<AtomicBool>,
    listener: Arc<Mutex<Option<TcpListener>>>,
    socket: Arc<Mutex<Option<TcpStream>>>,
}

const MAX_TRANSPORTS: usize = 256;
static LIVE_TRANSPORTS: AtomicUsize = AtomicUsize::new(0);
static NEXT_TRANSPORT: AtomicU64 = AtomicU64::new(1);

struct TransportPermit {
    id: u64,
    active: AtomicBool,
}
impl TransportPermit {
    fn acquire() -> anyhow::Result<Self> {
        let previous = LIVE_TRANSPORTS.fetch_add(1, Ordering::AcqRel);
        if previous >= MAX_TRANSPORTS {
            LIVE_TRANSPORTS.fetch_sub(1, Ordering::AcqRel);
            anyhow::bail!("Too many active or retiring DAP transports");
        }
        Ok(Self {
            id: NEXT_TRANSPORT.fetch_add(1, Ordering::Relaxed),
            active: AtomicBool::new(true),
        })
    }
}
impl TransportPermit {
    fn release(&self) {
        if self.active.swap(false, Ordering::AcqRel) {
            LIVE_TRANSPORTS.fetch_sub(1, Ordering::AcqRel);
        }
    }
}
impl Drop for TransportPermit {
    fn drop(&mut self) {
        self.release();
    }
}

#[derive(Default)]
struct Workers(Mutex<Vec<JoinHandle<()>>>);
impl Workers {
    fn push(&self, handle: JoinHandle<()>) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(handle);
    }
    fn poll(&self) -> bool {
        let mut handles = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let mut index = 0;
        while index < handles.len() {
            if handles[index].is_finished() {
                // is_finished includes thread-local destruction; this join never
                // waits for live work. The connector stays here until it can no
                // longer publish a writer handle into the same collection.
                let _ = handles.swap_remove(index).join();
            } else {
                index += 1;
            }
        }
        handles.is_empty()
    }
}
struct RetiringTransport {
    permit: TransportPermit,
    workers: Arc<Workers>,
}
fn retiring() -> &'static Mutex<Vec<RetiringTransport>> {
    static RETIRING: OnceLock<Mutex<Vec<RetiringTransport>>> = OnceLock::new();
    RETIRING.get_or_init(Default::default)
}
/// Joins only physically finished dropped transport workers. Stable IDs remain
/// pending until both connector/reader and writer have actually been joined.
pub fn poll_retiring_transports() -> Vec<(u64, String)> {
    let mut entries = retiring().lock().unwrap_or_else(|error| error.into_inner());
    entries.retain(|entry| !entry.workers.poll());
    let mut pending = entries
        .iter()
        .map(|entry| {
            (
                entry.permit.id,
                format!("Debugger transport {} cleanup", entry.permit.id),
            )
        })
        .collect::<Vec<_>>();
    pending.sort_by_key(|(id, _)| *id);
    pending
}

impl DapClient {
    /// Starts connecting to a DAP server, retrying until `timeout` elapses, and
    /// returns at once: the caller (the UI thread) never waits for the adapter
    /// to come up. A failure arrives as [`Message::ConnectFailed`].
    pub fn connect(addr: &str, timeout: Duration) -> anyhow::Result<DapClient> {
        Self::transport(addr, timeout, None)
    }
    /// Accept a pydevd peer configured with protocol='dap'. The socket and
    /// cancellation flag remain owned by the same DAP transport as connections.
    pub fn listen(addr: &str, timeout: Duration) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        Self::transport(addr, timeout, Some(listener))
    }
    pub fn listen_legacy(addr: &str, timeout: Duration) -> anyhow::Result<Self> {
        let (address, bridge) = crate::legacy_wire::start(addr)?;
        let mut client = Self::connect(&address, timeout)?;
        client._legacy = Some(bridge);
        Ok(client)
    }
    fn transport(
        addr: &str,
        timeout: Duration,
        listener: Option<TcpListener>,
    ) -> anyhow::Result<Self> {
        // Reclaim already-finished dropped clients before applying the live
        // transport limit, including non-UI callers without a frame poll.
        let _ = poll_retiring_transports();
        let permit = TransportPermit::acquire()?;
        let workers = Arc::new(Workers::default());
        let peer_workers = workers.clone();
        // Transport direction is captured at admission, not inferred later
        // from a listener that cancellation may already have revoked.
        let accepting = listener.is_some();
        let listener = Arc::new(Mutex::new(listener));
        let socket = Arc::new(Mutex::new(None::<TcpStream>));
        let peer_listener = listener.clone();
        let peer_socket = socket.clone();
        let (in_tx, incoming) = bounded(16);
        let (outgoing, out_rx) = bounded::<Vec<u8>>(4096);
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        let write_budget = queued_bytes.clone();
        let connected = Arc::new(AtomicBool::new(false));
        let closed = Arc::new(AtomicBool::new(false));
        let (addr, up, stop) = (addr.to_string(), connected.clone(), closed.clone());
        let connector = std::thread::Builder::new()
            .name("dap-connect".into())
            .spawn(move || {
                let connection = if accepting {
                    accept_until(peer_listener, timeout, &stop)
                } else {
                    connect_until(&addr, timeout, &stop)
                };
                let stream = match connection {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = in_tx.send(Message::ConnectFailed(e));
                        return;
                    }
                };
                {
                    let mut owned = peer_socket
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if stop.load(Ordering::SeqCst) {
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                        return;
                    }
                    *owned = stream.try_clone().ok();
                }
                let mut writer = match stream.try_clone() {
                    Ok(w) => w,
                    Err(e) => {
                        let _ = in_tx.send(Message::ConnectFailed(format!(
                            "cannot use the debug adapter connection: {e}"
                        )));
                        return;
                    }
                };
                up.store(true, Ordering::SeqCst);
                let write_errors = in_tx.clone();
                let write_up = up.clone();
                let _ = writer.set_write_timeout(Some(Duration::from_secs(5)));
                let writer_stop = stop.clone();
                let writer_handle =
                    std::thread::Builder::new()
                        .name("dap-write".into())
                        .spawn(move || {
                            while !writer_stop.load(Ordering::SeqCst) {
                                let frame = match out_rx.recv_timeout(Duration::from_millis(25)) {
                                    Ok(frame) => frame,
                                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                                };
                                write_budget.fetch_sub(frame.len(), Ordering::AcqRel);
                                if writer
                                    .write_all(&frame)
                                    .and_then(|_| writer.flush())
                                    .is_err()
                                {
                                    write_up.store(false, Ordering::SeqCst);
                                    writer_stop.store(true, Ordering::SeqCst);
                                    let _ = writer.shutdown(std::net::Shutdown::Both);
                                    let _ = write_errors.try_send(Message::TransportError(
                                        "DAP transport write failed or timed out".into(),
                                    ));
                                    return;
                                }
                            }
                        });
                match writer_handle {
                    Ok(handle) => peer_workers.push(handle),
                    Err(error) => {
                        up.store(false, Ordering::SeqCst);
                        let _ = in_tx.try_send(Message::TransportError(format!(
                            "Cannot start DAP writer: {error}"
                        )));
                        return;
                    }
                }
                if let Err(error) = read_messages(stream, &in_tx) {
                    let _ = in_tx.try_send(Message::TransportError(error.to_string()));
                }
                stop.store(true, Ordering::SeqCst);
                up.store(false, Ordering::SeqCst);
            })?;
        workers.push(connector);
        Ok(DapClient {
            workers,
            permit: Some(permit),
            _legacy: None,
            seq: 0,
            queued_bytes,
            outgoing,
            incoming,
            connected,
            closed,
            listener,
            socket,
        })
    }

    /// Request transport retirement without waiting on a worker or mutex-held
    /// network operation. Incoming backpressure is drained by poll_retirement.
    pub fn close(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
        self.listener
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(socket) = self
            .socket
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
    }
    pub fn poll_retirement(&mut self) -> bool {
        if self.closed.load(Ordering::SeqCst) {
            // A bounded incoming channel may hold a reader inside send().
            // Continue draining while it responds to the closed socket.
            while self.incoming.try_recv().is_ok() {}
        }
        self.is_retired()
    }
    pub fn lifecycle_id(&self) -> u64 {
        self.permit.as_ref().expect("live DAP transport permit").id
    }
    pub fn is_retired(&self) -> bool {
        let retired = self.workers.poll();
        if retired {
            if let Some(permit) = &self.permit {
                permit.release();
            }
        }
        retired
    }

    /// Whether the connection to the adapter is up.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    /// Sends a request and returns its sequence number.
    pub fn request(&mut self, command: &str, arguments: Value) -> anyhow::Result<u64> {
        self.seq += 1;
        let seq = self.seq;
        let msg =
            json!({ "seq": seq, "type": "request", "command": command, "arguments": arguments });
        self.send(&msg)?;
        Ok(seq)
    }

    /// Replies to a reverse request (e.g. `runInTerminal`).
    pub fn respond(
        &mut self,
        request_seq: u64,
        command: &str,
        success: bool,
        body: Value,
    ) -> anyhow::Result<()> {
        self.seq += 1;
        let msg = json!({ "seq": self.seq, "type": "response", "request_seq": request_seq, "command": command, "success": success, "body": body });
        self.send(&msg)
    }

    fn send(&mut self, msg: &Value) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.closed.load(Ordering::SeqCst),
            "DAP transport is closed"
        );
        let body = serde_json::to_vec(msg)?;
        anyhow::ensure!(
            body.len() <= MAX_FRAME_BYTES,
            "DAP request exceeds frame size limit"
        );
        let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        frame.extend_from_slice(&body);
        let size = frame.len();
        let mut reserved = self.queued_bytes.load(Ordering::Acquire);
        loop {
            anyhow::ensure!(
                reserved.saturating_add(size) <= 16 * 1024 * 1024,
                "DAP outgoing queue exceeds byte limit"
            );
            match self.queued_bytes.compare_exchange_weak(
                reserved,
                reserved + size,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => reserved = current,
            }
        }
        if self.outgoing.try_send(frame).is_err() {
            self.queued_bytes.fetch_sub(size, Ordering::AcqRel);
            anyhow::bail!("DAP outgoing queue is full or closed");
        }
        Ok(())
    }
}

impl Drop for DapClient {
    fn drop(&mut self) {
        self.close();
        if let Some(permit) = self.permit.take() {
            if !self.workers.poll() {
                // No new thread and no lossy full-queue fallback: admission's
                // global permit already bounds this registry to MAX_TRANSPORTS.
                retiring()
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(RetiringTransport {
                        permit,
                        workers: self.workers.clone(),
                    });
            }
        }
    }
}

/// Connects to `addr`, retrying while the adapter starts, until `timeout`
/// elapses or `closed` is set.
fn accept_until(
    listener: Arc<Mutex<Option<TcpListener>>>,
    timeout: Duration,
    closed: &AtomicBool,
) -> Result<TcpStream, String> {
    let start = Instant::now();
    loop {
        if closed.load(Ordering::SeqCst) {
            return Err("Stopped waiting for pydevd connection".into());
        }
        let result = {
            let guard = listener.lock().unwrap_or_else(|error| error.into_inner());
            let Some(listener) = guard.as_ref() else {
                return Err("Stopped waiting for pydevd connection".into());
            };
            listener.accept()
        };
        match result {
            Ok((stream, _)) => {
                // BSD sockets inherit the listener's non-blocking mode.
                let _ = stream.set_nonblocking(false);
                listener
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .take();
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(format!("Cannot accept pydevd connection: {error}")),
        }
        if start.elapsed() > timeout {
            return Err("Timed out waiting for pydevd connection".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn connect_until(addr: &str, timeout: Duration, closed: &AtomicBool) -> Result<TcpStream, String> {
    let start = Instant::now();
    loop {
        if closed.load(Ordering::SeqCst) {
            return Err(format!("stopped connecting to the debug adapter at {addr}"));
        }
        let attempt = (|| {
            use std::net::ToSocketAddrs;
            let mut last = None;
            for address in addr.to_socket_addrs()? {
                if closed.load(Ordering::SeqCst) {
                    break;
                }
                match TcpStream::connect_timeout(&address, Duration::from_millis(250)) {
                    Ok(stream) => return Ok(stream),
                    Err(error) => last = Some(error),
                }
            }
            Err(last.unwrap_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "No adapter address resolved")
            }))
        })();
        match attempt {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(e) => {
                if closed.load(Ordering::SeqCst) {
                    return Err(format!("stopped connecting to the debug adapter at {addr}"));
                }
                if start.elapsed() > timeout {
                    return Err(format!("cannot connect to debug adapter at {addr}: {e}"));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 8192;
/// A strict bounded frame decoder. Header bytes and declared body lengths are
/// checked before allocating, including duplicate/malformed lengths and UTF-8.
pub fn read_frame(reader: &mut impl BufRead) -> std::io::Result<Value> {
    use std::io::{Error, ErrorKind};
    let invalid = |message: &str| Error::new(ErrorKind::InvalidData, message);
    let mut length = None;
    let mut header_bytes = 0;
    loop {
        let mut line = Vec::new();
        loop {
            let buffer = reader.fill_buf()?;
            if buffer.is_empty() {
                return Err(Error::new(
                    ErrorKind::UnexpectedEof,
                    "DAP transport closed during frame header",
                ));
            }
            let count = buffer
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|index| index + 1)
                .unwrap_or(buffer.len());
            if header_bytes + line.len() + count > MAX_HEADER_BYTES {
                return Err(invalid("DAP header exceeds limit"));
            }
            line.extend_from_slice(&buffer[..count]);
            reader.consume(count);
            if line.ends_with(b"\n") {
                break;
            }
        }
        header_bytes += line.len();
        let line = std::str::from_utf8(&line)
            .map_err(|_| invalid("DAP header is not UTF-8"))?
            .trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| invalid("Malformed DAP header"))?;
        if name.eq_ignore_ascii_case("Content-Length") {
            if length.is_some() {
                return Err(invalid("Duplicate DAP Content-Length"));
            }
            let value = value.trim();
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(invalid("Invalid DAP Content-Length"));
            }
            let size = value
                .parse::<usize>()
                .map_err(|_| invalid("Invalid DAP Content-Length"))?;
            if size == 0 || size > MAX_FRAME_BYTES {
                return Err(invalid("DAP frame exceeds size limit"));
            }
            length = Some(size);
        }
    }
    let length = length.ok_or_else(|| invalid("Missing DAP Content-Length"))?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let value: Value =
        serde_json::from_slice(&body).map_err(|_| invalid("Invalid DAP JSON body"))?;
    if !value.is_object() {
        return Err(invalid("DAP message must be an object"));
    }
    Ok(value)
}
fn read_messages(stream: TcpStream, tx: &Sender<Message>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    loop {
        let value = read_frame(&mut reader)?;
        let string = |key: &str| {
            value[key].as_str().map(str::to_owned).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Missing DAP {key}"),
                )
            })
        };
        let message = match value["type"].as_str() {
            Some("event") => Message::Event {
                event: string("event")?,
                body: value.get("body").cloned().unwrap_or(Value::Null),
            },
            Some("response") => Message::Response {
                request_seq: value["request_seq"]
                    .as_u64()
                    .filter(|id| *id > 0)
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "Invalid DAP request_seq",
                        )
                    })?,
                command: string("command")?,
                success: value["success"].as_bool().ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "Missing DAP success")
                })?,
                message: value["message"].as_str().map(str::to_owned),
                body: value.get("body").cloned().unwrap_or(Value::Null),
            },
            Some("request") => Message::Request {
                seq: value["seq"].as_u64().filter(|id| *id > 0).ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "Invalid DAP sequence")
                })?,
                command: string("command")?,
                arguments: value.get("arguments").cloned().unwrap_or(Value::Null),
            },
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Unknown DAP message type",
                ));
            }
        };
        tx.send_timeout(message, Duration::from_secs(2))
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "DAP event queue is full or closed",
                )
            })?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;

    fn free_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// Reads one framed message from `stream`.
    fn read_frame(reader: &mut BufReader<TcpStream>) -> Value {
        let mut len = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.strip_prefix("Content-Length:") {
                len = v.trim().parse().unwrap();
            }
        }
        let mut body = vec![0u8; len];
        reader.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    /// The adapter starts after the client: `connect` returns at once, and
    /// what was sent meanwhile arrives in order once the adapter listens.
    #[test]
    fn connect_returns_at_once_and_queues_requests_until_the_adapter_listens() {
        let port = free_port();
        let start = Instant::now();
        let mut client =
            DapClient::connect(&format!("127.0.0.1:{port}"), Duration::from_secs(20)).unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "connect waited for the adapter: {:?}",
            start.elapsed()
        );
        assert!(!client.is_connected());
        assert_eq!(
            client
                .request("initialize", json!({ "adapterID": "debugpy" }))
                .unwrap(),
            1
        );
        assert_eq!(client.request("attach", json!({})).unwrap(), 2);
        std::thread::sleep(Duration::from_millis(300));
        let listener = TcpListener::bind(format!("127.0.0.1:{port}")).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let first = read_frame(&mut reader);
        assert_eq!(
            (first["command"].as_str(), first["seq"].as_u64()),
            (Some("initialize"), Some(1))
        );
        assert_eq!(read_frame(&mut reader)["command"], "attach");
        assert!(client.is_connected());
        // Messages from the adapter arrive on `incoming`.
        let body = serde_json::to_vec(&json!({ "type": "response", "request_seq": 1, "command": "initialize", "success": true, "body": {} })).unwrap();
        let mut w = stream;
        w.write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
            .unwrap();
        w.write_all(&body).unwrap();
        match client
            .incoming
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
        {
            Message::Response {
                request_seq,
                command,
                success,
                ..
            } => assert_eq!(
                (request_seq, command.as_str(), success),
                (1, "initialize", true)
            ),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn an_adapter_that_never_listens_is_reported_as_a_message() {
        // Destination port zero cannot name a listening TCP service. An
        // ephemeral listener dropped before connect leaves its port available
        // for an unrelated parallel test or process to claim instead.
        let port = 0;
        let start = Instant::now();
        let client =
            DapClient::connect(&format!("127.0.0.1:{port}"), Duration::from_millis(200)).unwrap();
        assert!(start.elapsed() < Duration::from_secs(1));
        match client
            .incoming
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
        {
            Message::ConnectFailed(e) => assert!(e.contains(&format!("127.0.0.1:{port}")), "{e}"),
            other => panic!("unexpected {other:?}"),
        }
        assert!(!client.is_connected());
    }

    #[test]
    fn dropping_the_client_stops_connecting() {
        let port = free_port();
        let client =
            DapClient::connect(&format!("127.0.0.1:{port}"), Duration::from_secs(60)).unwrap();
        let incoming = client.incoming.clone();
        drop(client);
        // The attempts end long before the 60 s timeout.
        match incoming.recv_timeout(Duration::from_secs(10)) {
            Ok(Message::ConnectFailed(e)) => assert!(e.contains("stopped connecting"), "{e}"),
            other => panic!("still connecting: {other:?}"),
        }
    }
}

#[cfg(test)]
mod reverse_peer_tests {
    use super::*;
    use std::io::Read;
    #[test]
    fn reverse_pydevd_connection_uses_real_dap_framing() {
        let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        let mut client = DapClient::listen(&address.to_string(), Duration::from_secs(5)).unwrap();
        let sequence = client
            .request("initialize", json!({"adapterID":"pydevd"}))
            .unwrap();
        let peer = std::thread::spawn(move || {
            let mut socket = TcpStream::connect(address).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            let length: usize = header
                .trim()
                .strip_prefix("Content-Length: ")
                .unwrap()
                .parse()
                .unwrap();
            let mut blank = String::new();
            reader.read_line(&mut blank).unwrap();
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            let request: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(request["command"], "initialize");
            assert_eq!(request["arguments"]["adapterID"], "pydevd");
            let response=json!({"seq":1,"type":"response","request_seq":sequence,"success":true,"command":"initialize","body":{}}).to_string();
            write!(
                socket,
                "Content-Length: {}\r\n\r\n{}",
                response.len(),
                response
            )
            .unwrap();
        });
        let response = client
            .incoming
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert!(
            matches!(response,Message::Response{request_seq,success:true,..}if request_seq==sequence)
        );
        peer.join().unwrap();
    }
}

#[cfg(test)]
mod reverse_listener_ownership_tests {
    use super::*;
    #[test]
    fn dropping_pending_listener_releases_its_port_synchronously() {
        let listener = DapClient::listen("127.0.0.1:0", Duration::from_secs(30)).unwrap();
        // Retain the same slot, never a clone of the kernel listener. Revoking
        // it must be synchronous even while the accepting worker owns its Arc.
        let owned = listener.listener.clone();
        #[cfg(target_os = "linux")]
        fn kernel_listens(inode: u64) -> bool {
            let inode = inode.to_string();
            std::fs::read_to_string("/proc/net/tcp")
                .expect("Linux kernel TCP listener inventory")
                .lines()
                .skip(1)
                .any(|line| {
                    let fields = line.split_whitespace().collect::<Vec<_>>();
                    fields.len() > 9 && fields[3] == "0A" && fields[9] == inode
                })
        }
        #[cfg(target_os = "linux")]
        let inode = {
            use std::os::{fd::AsRawFd, unix::fs::MetadataExt};
            let slot = owned.lock().unwrap();
            let fd = slot.as_ref().unwrap().as_raw_fd();
            std::fs::metadata(format!("/proc/self/fd/{fd}"))
                .expect("capture the owned listener socket inode")
                .ino()
        };
        #[cfg(target_os = "linux")]
        assert!(kernel_listens(inode), "owned socket must actually listen");
        drop(listener);
        assert!(
            owned.lock().unwrap().is_none(),
            "Drop must revoke the listener slot synchronously"
        );
        // A vacant numeric port can immediately be claimed by another test or
        // process. Assert closure of OUR kernel socket, not continued vacancy.
        // Other platforms retain the shared-slot/RAII ownership assertion above;
        // this Linux kernel oracle does not claim native Windows acceptance.
        #[cfg(target_os = "linux")]
        assert!(
            !kernel_listens(inode),
            "cancelled reverse debugger socket must no longer listen"
        );
    }
}

#[cfg(test)]
mod bounded_codec_tests {
    use super::*;
    use std::io::Cursor;
    fn framed(value: &Value) -> Vec<u8> {
        let body = serde_json::to_vec(value).unwrap();
        let mut frame = format!(
            "content-length: {}\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n",
            body.len()
        )
        .into_bytes();
        frame.extend(body);
        frame
    }
    #[test]
    fn decoder_handles_lowercase_headers_unicode_and_coalesced_frames() {
        let first = json!({"seq":1,"type":"event","event":"output","body":{"output":"π 🌍"}});
        let second = json!({"seq":2,"type":"event","event":"terminated"});
        let mut bytes = framed(&first);
        bytes.extend(framed(&second));
        let mut reader = Cursor::new(bytes);
        assert_eq!(super::read_frame(&mut reader).unwrap(), first);
        assert_eq!(super::read_frame(&mut reader).unwrap(), second);
    }
    #[test]
    fn decoder_rejects_oversized_lengths_before_reading_or_allocating_body() {
        for header in [
            format!("Content-Length: {}\r\n\r\n", MAX_FRAME_BYTES + 1),
            "Content-Length: 184467440737095516160\r\n\r\n".into(),
            "Content-Length: -1\r\n\r\n".into(),
            "Content-Length: 0\r\n\r\n".into(),
        ] {
            assert!(super::read_frame(&mut Cursor::new(header)).is_err());
        }
    }
    #[test]
    fn decoder_rejects_duplicate_missing_truncated_and_non_json_frames() {
        for frame in [
            b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}".as_slice(),
            b"Content-Type: json\r\n\r\n{}",
            b"Content-Length: 3\r\n\r\n{}",
            b"Content-Length: 2\r\n\r\n[]",
            b"Content-Length: 2\r\n\r\nxx",
        ] {
            assert!(super::read_frame(&mut Cursor::new(frame)).is_err());
        }
        assert!(super::read_frame(&mut Cursor::new(vec![b'a'; MAX_HEADER_BYTES + 1])).is_err());
    }
}

#[cfg(test)]
mod worker_retirement_tests {
    use super::*;

    #[test]
    fn close_drains_full_reader_queue_and_joins_both_transport_workers() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = DapClient::connect(
            &listener.local_addr().unwrap().to_string(),
            Duration::from_secs(3),
        )
        .unwrap();
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut peer = loop {
            match listener.accept() {
                Ok((peer, _)) => {
                    // BSD sockets inherit the listener's non-blocking mode.
                    peer.set_nonblocking(false).unwrap();
                    break peer;
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::yield_now()
                }
                Err(error) => panic!("owned DAP connection failed: {error}"),
            }
        };
        peer.set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let body = serde_json::to_vec(
            &json!({"type":"event","event":"output","body":{"output":"owned π"}}),
        )
        .unwrap();
        for _ in 0..32 {
            write!(peer, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
            peer.write_all(&body).unwrap();
        }
        while client.incoming.len() < 16 {
            assert!(
                Instant::now() < deadline,
                "actual reader never filled its bounded queue"
            );
            std::thread::yield_now();
        }
        assert!(!client.is_retired());
        client.close();
        while !client.poll_retirement() {
            assert!(
                Instant::now() < deadline,
                "closed socket/reader backpressure/writer must physically retire"
            );
            std::thread::yield_now();
        }
        assert!(
            client.workers.0.lock().unwrap().is_empty(),
            "both actual worker handles were joined"
        );
    }

    #[test]
    fn drop_retains_stable_worker_owner_until_finished_and_joined() {
        let mut client = DapClient::listen("127.0.0.1:0", Duration::from_secs(3)).unwrap();
        let id = client.lifecycle_id();
        let (release, held) = crossbeam_channel::bounded::<()>(0);
        // A deterministic physical-worker fence prevents scheduler timing from
        // turning the pending-registry assertion into a probabilistic test.
        client.workers.push(std::thread::spawn(move || {
            let _ = held.recv();
        }));
        client.close();
        drop(client);
        assert!(poll_retiring_transports()
            .iter()
            .any(|(pending, _)| *pending == id));
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while poll_retiring_transports()
            .iter()
            .any(|(pending, _)| *pending == id)
        {
            assert!(
                Instant::now() < deadline,
                "captured owner must disappear only after join"
            );
            std::thread::yield_now();
        }
    }
}
