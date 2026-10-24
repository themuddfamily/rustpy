//! Authenticated callback for captured/parallel test workers. The listener and
//! every accepted socket have bounded work and follow the owning TestRun.
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};
pub(crate) struct DebugEvents {
    pub address: String,
    pub token: String,
    cancel: Arc<AtomicBool>,
    events: mpsc::Receiver<(String, u16)>,
}
impl DebugEvents {
    pub fn new() -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?.to_string();
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes)
            .map_err(|error| anyhow::anyhow!("Cannot generate test debugger token: {error}"))?;
        let token = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let expected = token.clone();
        let (cancel, events) = (Arc::new(AtomicBool::new(false)), mpsc::sync_channel(16));
        let stop = cancel.clone();
        let (sender, receiver) = events;
        std::thread::Builder::new()
            .name("test-debug-events".into())
            .spawn(move || {
                let mut peers: Vec<(TcpStream, Vec<u8>, Instant)> = Vec::new();
                while !stop.load(Ordering::SeqCst) {
                    for _ in 0..16 {
                        match listener.accept() {
                            Ok((stream, _)) => {
                                if peers.len() >= 32 {
                                    continue;
                                }
                                if stream.set_nonblocking(true).is_ok() {
                                    peers.push((stream, Vec::new(), Instant::now()));
                                }
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(_) => break,
                        }
                    }
                    peers.retain_mut(|(stream, buffer, started)| {
                        if started.elapsed() > Duration::from_secs(3) {
                            return false;
                        }
                        let mut bytes = [0; 1024];
                        match stream.read(&mut bytes) {
                            Ok(0) => return false,
                            Ok(count) => buffer.extend_from_slice(&bytes[..count]),
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                return true
                            }
                            Err(_) => return false,
                        }
                        if buffer.len() > 4096 {
                            return false;
                        }
                        let Some(end) = buffer.iter().position(|byte| *byte == b'\n') else {
                            return true;
                        };
                        let accepted = serde_json::from_slice::<serde_json::Value>(&buffer[..end])
                            .ok()
                            .and_then(|value| {
                                if value["token"].as_str() != Some(expected.as_str()) {
                                    return None;
                                }
                                let host = value["host"].as_str()?;
                                if !matches!(host, "127.0.0.1" | "::1" | "localhost") {
                                    return None;
                                }
                                let port = u16::try_from(value["port"].as_u64()?)
                                    .ok()
                                    .filter(|port| *port > 0)?;
                                sender.try_send((host.into(), port)).ok()
                            })
                            .is_some();
                        let _ = stream.write_all(if accepted { b"OK\n" } else { b"NO\n" });
                        false
                    });
                    std::thread::sleep(Duration::from_millis(20));
                }
            })?;
        Ok(Self {
            address,
            token,
            cancel,
            events: receiver,
        })
    }
    pub fn poll(&self) -> Vec<(String, u16)> {
        self.events.try_iter().collect()
    }
}
impl Drop for DebugEvents {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn callback_requires_own_token_and_loopback_debugger() {
        let events = DebugEvents::new().unwrap();
        let mut peer = TcpStream::connect(&events.address).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        writeln!(
            peer,
            "{}",
            serde_json::json!({"token":"wrong","host":"127.0.0.1","port":5678})
        )
        .unwrap();
        let mut acknowledgement = [0; 3];
        peer.read_exact(&mut acknowledgement).unwrap();
        assert_eq!(&acknowledgement, b"NO\n");
        assert!(events.poll().is_empty());
        let mut peer = TcpStream::connect(&events.address).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        writeln!(
            peer,
            "{}",
            serde_json::json!({"token":events.token,"host":"127.0.0.1","port":5678})
        )
        .unwrap();
        peer.read_exact(&mut acknowledgement).unwrap();
        assert_eq!(&acknowledgement, b"OK\n");
        assert_eq!(events.poll(), vec![("127.0.0.1".into(), 5678)]);
    }
}
