use super::Message;
use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
const FRAME: usize = 2 * 1024 * 1024;
const QUEUE: usize = 4 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
enum Wire {
    Hello { token: String, name: String },
    Data(Message),
}
#[derive(Debug)]
pub enum Event {
    Joined { peer: u64, name: String },
    Message { peer: u64, message: Message },
    Left(u64),
    Error(String),
    Stopped,
}
enum Command {
    Send(Option<u64>, Message),
    Revoke(u64),
}
pub struct Session {
    pub host: bool,
    pub port: u16,
    pub invitation: Option<String>,
    tx: Sender<Command>,
    rx: Receiver<Event>,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    owner: u64,
}
impl Session {
    pub fn host() -> Result<Self, String> {
        let listener =
            TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let mut random = [0; 32];
        getrandom::fill(&mut random).map_err(|e| e.to_string())?;
        let token: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let invitation = format!("rustpy-cwm://127.0.0.1:{port}/{token}");
        Self::spawn(true, port, Some(invitation), move |rx, tx, stop| {
            run_host(listener, token, rx, tx, stop)
        })
    }
    pub fn join(invite: &str, name: String) -> Result<Self, String> {
        let (address, token) = parse_invitation(invite)?;
        let port = address.port();
        Self::spawn(false, port, None, move |rx, tx, stop| {
            let stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))
                .map_err(|e| e.to_string())?;
            let mut peer = Peer::new(stream)?;
            peer.enqueue(&Wire::Hello {
                token,
                name: name.chars().take(80).collect(),
            })?;
            run_guest(peer, rx, tx, stop)
        })
    }
    fn spawn(
        host: bool,
        port: u16,
        invitation: Option<String>,
        run: impl FnOnce(Receiver<Command>, Sender<Event>, Arc<AtomicBool>) -> Result<(), String>
            + Send
            + 'static,
    ) -> Result<Self, String> {
        let (tx, commands) = crossbeam_channel::bounded(128);
        let (events, rx) = crossbeam_channel::bounded(128);
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_done = done.clone();
        let mut nonce = [0; 8];
        getrandom::fill(&mut nonce).map_err(|e| e.to_string())?;
        let owner = u64::from_le_bytes(nonce);
        std::thread::Builder::new()
            .name("collaboration".into())
            .spawn(move || {
                if let Err(error) = run(commands, events.clone(), worker_stop) {
                    let _ = events.try_send(Event::Error(error));
                }
                worker_done.store(true, Ordering::Release);
                let _ = events.try_send(Event::Stopped);
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            host,
            port,
            invitation,
            tx,
            rx,
            stop,
            done,
            owner,
        })
    }
    pub fn send(&self, to: Option<u64>, message: Message) -> Result<(), String> {
        self.tx
            .try_send(Command::Send(to, message))
            .map_err(|_| "Collaboration queue is full or disconnected".into())
    }
    pub fn revoke(&self, peer: u64) {
        if self.tx.try_send(Command::Revoke(peer)).is_err() {
            self.stop();
        }
    }
    pub fn poll(&self) -> Vec<Event> {
        self.rx.try_iter().take(128).collect()
    }
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
    pub fn is_running(&self) -> bool {
        !self.done.load(Ordering::Acquire)
    }
    pub fn lifecycle_stamp(&self) -> String {
        format!(
            "collaboration:{}:{}:{}:{}",
            self.owner,
            self.host,
            self.port,
            self.stop.load(Ordering::Acquire)
        )
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}
fn parse_invitation(invite: &str) -> Result<(SocketAddr, String), String> {
    let (address, token) = invite
        .trim()
        .strip_prefix("rustpy-cwm://")
        .and_then(|s| s.split_once('/'))
        .ok_or("Invalid invitation")?;
    let address: SocketAddr = address
        .parse()
        .map_err(|_| "Invitation must use a loopback IP address")?;
    if !address.ip().is_loopback()
        || token.len() != 64
        || !token.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(
            "Use a loopback invitation, optionally through an authenticated SSH tunnel".into(),
        );
    }
    Ok((address, token.into()))
}
fn same_token(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |v, (a, b)| v | (a ^ b)) == 0
}
struct Peer {
    stream: TcpStream,
    input: Vec<u8>,
    output: VecDeque<Vec<u8>>,
    written: usize,
    authenticated: bool,
    started: Instant,
}
impl Peer {
    fn new(stream: TcpStream) -> Result<Self, String> {
        stream.set_nonblocking(true).map_err(|e| e.to_string())?;
        stream.set_nodelay(true).map_err(|e| e.to_string())?;
        Ok(Self {
            stream,
            input: Vec::new(),
            output: VecDeque::new(),
            written: 0,
            authenticated: false,
            started: Instant::now(),
        })
    }
    fn enqueue(&mut self, wire: &Wire) -> Result<(), String> {
        let data = serde_json::to_vec(wire).map_err(|e| e.to_string())?;
        if data.len() > FRAME
            || self.output.iter().map(Vec::len).sum::<usize>() + data.len() > QUEUE
        {
            return Err("Peer output exceeds bounded queue".into());
        }
        let mut frame = (data.len() as u32).to_be_bytes().to_vec();
        frame.extend(data);
        self.output.push_back(frame);
        Ok(())
    }
    fn tick(&mut self) -> Result<Vec<Wire>, String> {
        while let Some(frame) = self.output.front() {
            match self.stream.write(&frame[self.written..]) {
                Ok(0) => return Err("Disconnected".into()),
                Ok(n) => {
                    self.written += n;
                    if self.written == frame.len() {
                        self.output.pop_front();
                        self.written = 0;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.to_string()),
            }
        }
        let mut chunk = [0; 16384];
        let mut messages = Vec::new();
        for _ in 0..16 {
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err("Disconnected".into()),
                Ok(n) => {
                    self.input.extend_from_slice(&chunk[..n]);
                    self.decode(&mut messages)?;
                    if self.input.len() > FRAME + 4 {
                        return Err("Peer frame exceeds limit".into());
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(messages)
    }
    fn decode(&mut self, messages: &mut Vec<Wire>) -> Result<(), String> {
        while self.input.len() >= 4 {
            let n = u32::from_be_bytes(self.input[..4].try_into().unwrap()) as usize;
            if n > FRAME {
                return Err("Peer frame exceeds limit".into());
            }
            if self.input.len() < 4 + n {
                break;
            }
            messages.push(
                serde_json::from_slice(&self.input[4..4 + n])
                    .map_err(|_| "Invalid collaboration message")?,
            );
            self.input.drain(..4 + n);
        }
        Ok(())
    }
}
fn run_host(
    listener: TcpListener,
    token: String,
    commands: Receiver<Command>,
    events: Sender<Event>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    let mut peers = BTreeMap::<u64, Peer>::new();
    let mut next = 1u64;
    while !stop.load(Ordering::Acquire) {
        for _ in 0..8 {
            match listener.accept() {
                Ok((stream, _)) if peers.len() < 16 => {
                    peers.insert(next, Peer::new(stream)?);
                    next += 1;
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => return Err(e.to_string()),
            }
        }
        for command in commands.try_iter().take(128) {
            match command {
                Command::Revoke(id) => {
                    peers.remove(&id);
                    let _ = events.try_send(Event::Left(id));
                }
                Command::Send(to, message) => {
                    let mut failed = Vec::new();
                    for (id, peer) in &mut peers {
                        if peer.authenticated
                            && to.is_none_or(|to| to == *id)
                            && peer.enqueue(&Wire::Data(message.clone())).is_err()
                        {
                            failed.push(*id);
                        }
                    }
                    for id in failed {
                        peers.remove(&id);
                        let _ = events.try_send(Event::Left(id));
                    }
                }
            }
        }
        let mut remove = Vec::new();
        for (id, peer) in &mut peers {
            if !peer.authenticated && peer.started.elapsed() > Duration::from_secs(10) {
                remove.push(*id);
                continue;
            }
            match peer.tick() {
                Err(_) => remove.push(*id),
                Ok(messages) => {
                    for message in messages {
                        match message {
                            Wire::Hello {
                                token: provided,
                                name,
                            } if !peer.authenticated && same_token(&provided, &token) => {
                                peer.authenticated = true;
                                peer.enqueue(&Wire::Data(Message::Welcome { peer: *id }))?;
                                if events
                                    .try_send(Event::Joined {
                                        peer: *id,
                                        name: name.chars().take(80).collect(),
                                    })
                                    .is_err()
                                {
                                    remove.push(*id);
                                }
                            }
                            Wire::Data(message) if peer.authenticated => {
                                if events
                                    .try_send(Event::Message { peer: *id, message })
                                    .is_err()
                                {
                                    remove.push(*id);
                                }
                            }
                            _ => remove.push(*id),
                        }
                    }
                }
            }
        }
        for id in remove {
            peers.remove(&id);
            let _ = events.try_send(Event::Left(id));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
fn run_guest(
    mut peer: Peer,
    commands: Receiver<Command>,
    events: Sender<Event>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    while !stop.load(Ordering::Acquire) {
        for command in commands.try_iter().take(128) {
            if let Command::Send(_, message) = command {
                peer.enqueue(&Wire::Data(message))?;
            }
        }
        for message in peer.tick()? {
            if let Wire::Data(message) = message {
                if matches!(message, Message::Welcome { .. }) {
                    peer.authenticated = true;
                }
                if !peer.authenticated {
                    return Err("Host did not authenticate the session".into());
                }
                events
                    .try_send(Event::Message { peer: 0, message })
                    .map_err(|_| "Collaboration receive queue is full")?;
            }
        }
        if !peer.authenticated && peer.started.elapsed() > Duration::from_secs(10) {
            return Err("Host authentication timed out".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invitations_require_loopback_and_full_token() {
        assert!(parse_invitation("rustpy-cwm://192.0.2.1:12/abc").is_err());
        assert!(parse_invitation(&format!("rustpy-cwm://127.0.0.1:12/{}", "a".repeat(64))).is_ok());
        assert!(!same_token("abcd", "abce"));
    }
}

#[cfg(test)]
mod session_fixtures {
    use super::*;
    fn until(session: &Session, mut accept: impl FnMut(Event) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            for event in session.poll() {
                if accept(event) {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("Expected collaboration protocol event");
    }
    #[test]
    fn authenticated_real_exchange_and_revocation() {
        let host = Session::host().unwrap();
        let guest = Session::join(host.invitation.as_ref().unwrap(), "Guest".into()).unwrap();
        let mut peer = 0;
        until(&host, |event| {
            if let Event::Joined { peer: id, name } = event {
                assert_eq!(name, "Guest");
                peer = id;
                true
            } else {
                false
            }
        });
        guest
            .send(
                None,
                Message::Edit {
                    id: 4,
                    base: 7,
                    text: "actual message".into(),
                },
            )
            .unwrap();
        until(
            &host,
            |event| matches!(event,Event::Message{peer:id,message:Message::Edit{id:4,base:7,text}}if id==peer&&text=="actual message"),
        );
        host.send(Some(peer), Message::Permission { write: false })
            .unwrap();
        until(&guest, |event| {
            matches!(
                event,
                Event::Message {
                    message: Message::Permission { write: false },
                    ..
                }
            )
        });
        host.revoke(peer);
        until(&guest, |event| {
            matches!(event, Event::Error(_) | Event::Stopped)
        });
        host.stop();
        until(&host, |event| matches!(event, Event::Stopped));
    }
}
