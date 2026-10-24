//! Bounded, cancellable TCP relay for the legacy pydevd quoted-line protocol.
use serde_json::Value;
use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
const LIMIT: usize = 8 * 1024 * 1024;
pub(crate) struct LegacyBridge {
    cancel: Arc<AtomicBool>,
}
impl Drop for LegacyBridge {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}
struct Peer {
    stream: TcpStream,
    input: Vec<u8>,
    output: VecDeque<Vec<u8>>,
    offset: usize,
    bytes: usize,
}
impl Peer {
    fn new(stream: TcpStream) -> std::io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Self {
            stream,
            input: Vec::new(),
            output: VecDeque::new(),
            offset: 0,
            bytes: 0,
        })
    }
    fn queue(&mut self, bytes: Vec<u8>) -> std::io::Result<()> {
        if self.bytes + bytes.len() > 16 * 1024 * 1024 {
            return Err(std::io::Error::other(
                "Legacy bridge output budget exceeded",
            ));
        }
        self.bytes += bytes.len();
        self.output.push_back(bytes);
        Ok(())
    }
    fn pump(&mut self) -> std::io::Result<()> {
        let mut chunk = [0; 65536];
        match self.stream.read(&mut chunk) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "Debugger peer closed",
                ))
            }
            Ok(count) => {
                if self.input.len() + count > LIMIT + 8192 + 65536 {
                    return Err(std::io::Error::other(
                        "Legacy debugger input exceeds budget",
                    ));
                }
                self.input.extend_from_slice(&chunk[..count]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }
        for _ in 0..16 {
            let Some(bytes) = self.output.front() else {
                break;
            };
            match self.stream.write(&bytes[self.offset..]) {
                Ok(0) => return Err(std::io::Error::other("Debugger write returned zero")),
                Ok(count) => {
                    self.offset += count;
                    self.bytes -= count;
                    if self.offset == bytes.len() {
                        self.output.pop_front();
                        self.offset = 0;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
    fn dap(&mut self) -> std::io::Result<Option<Value>> {
        let Some(end) = self.input.windows(4).position(|bytes| bytes == b"\r\n\r\n") else {
            if self.input.len() > 8192 {
                return Err(std::io::Error::other("DAP header exceeds budget"));
            }
            return Ok(None);
        };
        let header = std::str::from_utf8(&self.input[..end]).map_err(std::io::Error::other)?;
        let lengths = header
            .lines()
            .filter_map(|line| line.split_once(':'))
            .filter(|(key, _)| key.eq_ignore_ascii_case("Content-Length"))
            .map(|(_, value)| value.trim().parse::<usize>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(std::io::Error::other)?;
        if lengths.len() != 1 || lengths[0] > LIMIT {
            return Err(std::io::Error::other("Invalid DAP content length"));
        }
        let size = end + 4 + lengths[0];
        if self.input.len() < size {
            return Ok(None);
        }
        let value =
            serde_json::from_slice(&self.input[end + 4..size]).map_err(std::io::Error::other)?;
        self.input.drain(..size);
        Ok(Some(value))
    }
    fn line(&mut self) -> std::io::Result<Option<String>> {
        let Some(end) = self.input.iter().position(|byte| *byte == b'\n') else {
            return Ok(None);
        };
        let bytes = self.input.drain(..=end).collect::<Vec<_>>();
        String::from_utf8(bytes)
            .map(|line| Some(line.trim_end_matches(['\r', '\n']).to_owned()))
            .map_err(std::io::Error::other)
    }
}
pub(crate) fn start(address: &str) -> anyhow::Result<(String, LegacyBridge)> {
    let remote = TcpListener::bind(address)?;
    remote.set_nonblocking(true)?;
    let local = TcpListener::bind("127.0.0.1:0")?;
    local.set_nonblocking(true)?;
    let local_address = local.local_addr()?.to_string();
    let cancel = Arc::new(AtomicBool::new(false));
    let stop = cancel.clone();
    std::thread::Builder::new().name("pydevd-legacy-bridge".into()).spawn(move||{
  let mut dap=None;let mut legacy=None;let mut state=crate::legacy_pydevd::State::default();let deadline=Instant::now()+Duration::from_secs(86400);
  let result=(||->anyhow::Result<()>{while !stop.load(Ordering::SeqCst)&&Instant::now()<deadline{
   if dap.is_none(){match local.accept(){Ok((stream,_))=>dap=Some(Peer::new(stream)?),Err(error)if error.kind()==std::io::ErrorKind::WouldBlock=>{},Err(error)=>return Err(error.into()),}}
   if legacy.is_none(){match remote.accept(){Ok((stream,_))=>{legacy=Some(Peer::new(stream)?);state.connected();},Err(error)if error.kind()==std::io::ErrorKind::WouldBlock=>{},Err(error)=>return Err(error.into()),}}
   if let Some(peer)=&mut dap{peer.pump()?;for _ in 0..16{let Some(value)=peer.dap()?else{break;};state.request(value);}}
   if let Some(peer)=&mut legacy{peer.pump()?;for _ in 0..32{let Some(line)=peer.line()?else{break;};state.message(&line);}}
   state.expire();
   if let Some(peer)=&mut dap{for value in state.dap.drain(..){let body=serde_json::to_vec(&value)?;let mut bytes=format!("Content-Length: {}\r\n\r\n",body.len()).into_bytes();bytes.extend(body);peer.queue(bytes)?;}}
   if let Some(peer)=&mut legacy{for line in state.wire.drain(..){peer.queue(line.into_bytes())?;}}
   if state.finished&&dap.as_ref().is_none_or(|peer|peer.output.is_empty())&&legacy.as_ref().is_none_or(|peer|peer.output.is_empty()){break;}
   std::thread::sleep(Duration::from_millis(10));
  }Ok(())})();
  // A bridge/socket failure is not permission to kill the attached program.
  if !state.finished{if let Some(peer)=&mut legacy{let _=peer.stream.set_nonblocking(false);let _=peer.stream.set_write_timeout(Some(Duration::from_millis(200)));let _=peer.stream.write_all(b"101\t9999991\t\n106\t9999993\t*\n");}}
  if let Err(error)=result{if !stop.load(Ordering::SeqCst){if let Some(peer)=&mut dap{
    let _=peer.stream.set_nonblocking(false);let _=peer.stream.set_write_timeout(Some(Duration::from_millis(200)));
    for value in [serde_json::json!({"seq":0,"type":"event","event":"output","body":{"category":"stderr","output":format!("Legacy pydevd: {error}\n")}}),serde_json::json!({"seq":1,"type":"event","event":"terminated","body":{}})]{if let Ok(body)=serde_json::to_vec(&value){let _=peer.stream.write_all(format!("Content-Length: {}\r\n\r\n",body.len()).as_bytes());let _=peer.stream.write_all(&body);}}
  }}}

 })?;
    Ok((local_address, LegacyBridge { cancel }))
}
