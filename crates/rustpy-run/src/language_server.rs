//! Outbound stdio LSP transport with bounded frames/queues and owned process tree.
//! Starting it is an explicit trusted-project admission made by the IDE.
use crate::process_tree::ProcessTree;
use crossbeam_channel::{bounded, Receiver, Sender, TrySendError};
use serde_json::{json, Value};
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    path::Path,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};
const MAX_FRAME: usize = 8 * 1024 * 1024;
#[derive(Debug)]
pub enum Event {
    Message(Value),
    Log(String),
    Error(String),
    Exited(Option<i32>),
}
pub struct Client {
    tx: Sender<Value>,
    pub events: Receiver<Event>,
    stopping: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    next_id: u64,
    stamp: String,
    pub ready: bool,
}
impl Client {
    pub fn start(
        program: &Path,
        args: &[String],
        root: &Path,
        root_uri: &str,
        initialization: Value,
    ) -> io::Result<Self> {
        let mut command = rustpy_project::interpreter::interpreter_command(program);
        command
            .args(args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        ProcessTree::prepare(&mut command);
        let mut child = command.spawn()?;
        let tree = match ProcessTree::new(&mut child) {
            Ok(tree) => Arc::new(tree),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("Missing LSP stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("Missing LSP stdout"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("Missing LSP stderr"))?;
        let (tx, rx) = bounded::<Value>(32);
        let (events_tx, events) = bounded::<Event>(16);
        let stopping = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let stamp = tree.stamp();
        let writer = {
            let stopping = stopping.clone();
            let events = events_tx.clone();
            thread::spawn(move || {
                let mut stdin = stdin;
                while !stopping.load(Ordering::Acquire) {
                    let value = match rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(value) => value,
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                        Err(_) => break,
                    };
                    if let Err(error) = write_frame(&mut stdin, &value) {
                        let _ = events.try_send(Event::Error(error.to_string()));
                        stopping.store(true, Ordering::Release);
                        break;
                    }
                }
            })
        };
        let stdout_reader = {
            let stopping = stopping.clone();
            let events = events_tx.clone();
            thread::spawn(move || {
                let mut stdout = BufReader::new(stdout);
                loop {
                    match read_frame(&mut stdout) {
                        Ok(Some(message)) => {
                            if events
                                .send_timeout(Event::Message(message), Duration::from_millis(250))
                                .is_err()
                            {
                                stopping.store(true, Ordering::Release);
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            let _ = events.try_send(Event::Error(error.to_string()));
                            break;
                        }
                    }
                    if stopping.load(Ordering::Acquire) {
                        break;
                    }
                }
                stopping.store(true, Ordering::Release);
            })
        };
        let stderr_reader = {
            let events = events_tx.clone();
            thread::spawn(move || {
                let mut stderr = BufReader::new(stderr);
                let mut buffer = Vec::new();
                loop {
                    buffer.clear();
                    match stderr.by_ref().take(8193).read_until(b'\n', &mut buffer) {
                        Ok(0) | Err(_) => break,
                        _ => {
                            let message =
                                String::from_utf8_lossy(&buffer[..buffer.len().min(8192)])
                                    .trim_end()
                                    .to_owned();
                            let _ = events.try_send(Event::Log(message));
                        }
                    }
                }
            })
        };
        {
            let stopping = stopping.clone();
            let alive = alive.clone();
            thread::spawn(move || {
                let code = loop {
                    if stopping.load(Ordering::Acquire) {
                        tree.force(&mut child);
                        let status = child.wait().ok();
                        break status.and_then(|status| status.code());
                    }
                    match tree.poll(&mut child) {
                        Ok(Some(status)) => {
                            tree.force(&mut child);
                            break status.code();
                        }
                        Ok(None) => thread::sleep(Duration::from_millis(40)),
                        Err(error) => {
                            let _ = events_tx.try_send(Event::Error(error.to_string()));
                            tree.force(&mut child);
                            let _ = child.wait();
                            break None;
                        }
                    }
                };
                stopping.store(true, Ordering::Release);
                // The background supervisor retains every pipe owner until the
                // tree has retired and blocked reads/writes can finish. Neither
                // UI Stop nor Client Drop waits on these joins. In particular,
                // is_running must not become false while an output worker still
                // owns a server pipe or the writer still owns its stdin.
                for worker in [writer, stdout_reader, stderr_reader] {
                    if worker.join().is_err() {
                        let _ = events_tx
                            .try_send(Event::Error("Language-server pipe worker panicked".into()));
                    }
                }
                alive.store(false, Ordering::Release);
                let _ = events_tx.try_send(Event::Exited(code));
            });
        }
        let client = Self {
            tx,
            events,
            stopping,
            alive,
            next_id: 2,
            stamp,
            ready: false,
        };
        client.send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"processId":std::process::id(),"rootUri":root_uri,"workspaceFolders":[{"uri":root_uri,"name":root.file_name().unwrap_or_default().to_string_lossy()}],"capabilities":{"general":{"positionEncodings":["utf-16"]},"workspace":{"workspaceFolders":true,"configuration":true,"applyEdit":false},"textDocument":{"synchronization":{"didSave":true},"completion":{"completionItem":{"snippetSupport":true,"documentationFormat":["plaintext","markdown"]}},"hover":{"contentFormat":["markdown","plaintext"]},"definition":{"linkSupport":true},"publishDiagnostics":{"versionSupport":true},"formatting":{},"codeAction":{"resolveSupport":{"properties":["edit","command"]},"codeActionLiteralSupport":{"codeActionKind":{"valueSet":["quickfix","source.organizeImports","source.fixAll.eslint"]}}},"rename":{"prepareSupport":false}}},"initializationOptions":initialization,"clientInfo":{"name":"RustPy","version":env!("CARGO_PKG_VERSION")}}})).map_err(io::Error::other)?;
        Ok(client)
    }
    pub fn send(&self, value: Value) -> Result<(), String> {
        if self.stopping.load(Ordering::Acquire) {
            return Err("Language server is stopping".into());
        }
        if serde_json::to_vec(&value)
            .map_err(|error| error.to_string())?
            .len()
            > MAX_FRAME
        {
            return Err("LSP message exceeds 8 MiB".into());
        }
        self.tx.try_send(value).map_err(|error| match error {
            TrySendError::Full(_) => "Language-server request queue is full".into(),
            TrySendError::Disconnected(_) => "Language server writer closed".into(),
        })
    }
    pub fn request(&mut self, method: &str, params: Value) -> Result<u64, String> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or("LSP request ID exhausted")?;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
        Ok(id)
    }
    pub fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.send(json!({"jsonrpc":"2.0","method":method,"params":params}))
    }
    pub fn reply(&self, id: Value, result: Value) -> Result<(), String> {
        self.send(json!({"jsonrpc":"2.0","id":id,"result":result}))
    }
    pub fn cancel(&self, id: u64) {
        let _ = self.notify("$/cancelRequest", json!({"id":id}));
    }
    pub fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
    }
    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }
    pub fn is_running(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }
    pub fn stamp(&self) -> &str {
        &self.stamp
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.stop();
    }
}
pub fn write_frame(output: &mut impl Write, value: &Value) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if bytes.len() > MAX_FRAME {
        return Err(io::Error::other("LSP frame exceeds 8 MiB"));
    }
    write!(output, "Content-Length: {}\r\n\r\n", bytes.len())?;
    output.write_all(&bytes)?;
    output.flush()
}
pub fn read_frame(input: &mut impl BufRead) -> io::Result<Option<Value>> {
    let mut length = None;
    let mut header = 0;
    loop {
        let mut line = String::new();
        let count = input.by_ref().take(16385).read_line(&mut line)?;
        if count == 0 {
            return if header == 0 {
                Ok(None)
            } else {
                Err(io::Error::other("Truncated LSP header"))
            };
        }
        header += count;
        if header > 16384 {
            return Err(io::Error::other("LSP header exceeds 16 KiB"));
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.eq_ignore_ascii_case("Content-Length") {
                if length.is_some() {
                    return Err(io::Error::other("Duplicate LSP length"));
                }
                length = Some(value.trim().parse::<usize>().map_err(io::Error::other)?);
            }
        }
    }
    let length = length
        .filter(|length| *length <= MAX_FRAME)
        .ok_or_else(|| io::Error::other("Missing or excessive LSP frame length"))?;
    let mut bytes = vec![0; length];
    input.read_exact(&mut bytes)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(io::Error::other)
}
#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn fragmented_unicode_frames_round_trip_without_line_protocol_assumptions() {
        let value = json!({"id":42,"result":{"label":"π😀"}});
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &value).unwrap();
        let mut reader = BufReader::with_capacity(1, bytes.as_slice());
        assert_eq!(read_frame(&mut reader).unwrap(), Some(value));
        assert!(read_frame(&mut reader).unwrap().is_none());
    }
    #[test]
    fn malformed_or_oversized_framing_is_rejected() {
        for bytes in [
            b"Content-Length: 999999999\r\n\r\n".as_slice(),
            b"Content-Length: 2\r\nContent-Length: 2\r\n\r\n{}",
            b"Content-Length: 4\r\n\r\n{}",
        ] {
            assert!(read_frame(&mut BufReader::new(bytes)).is_err());
        }
    }
}
