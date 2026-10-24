//! Official Mongo SOCKS5 connector boundary. The local plaintext leg requires
//! a per-owner random credential; the outbound leg alone owns original-host TLS.
use super::*;
use std::{
    io::{self, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener},
};
use transport::{Policy, Socket};
pub(super) struct Bridge {
    pub address: SocketAddr,
    pub password: String,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
struct RelayOwners {
    workers: Vec<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    control: Arc<Control>,
}
impl Drop for RelayOwners {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.control.shutdown_sockets();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}
impl Bridge {
    pub fn start(policy: Policy) -> Result<Self> {
        let mut random = [0u8; 32];
        rustls::crypto::ring::default_provider()
            .secure_random
            .fill(&mut random)
            .map_err(|_| anyhow::anyhow!("Native bridge credential generation failed"))?;
        let password = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let credential = password.clone();
        let thread = std::thread::Builder::new()
            .name("mongo-native-bridge".into())
            .spawn(move || {
                let mut owners = RelayOwners {
                    workers: Vec::new(),
                    stop: worker_stop.clone(),
                    control: policy.control.clone(),
                };
                while !worker_stop.load(Ordering::Acquire)
                    && !policy.control.stop.load(Ordering::Acquire)
                {
                    let mut index = 0;
                    while index < owners.workers.len() {
                        if owners.workers[index].is_finished() {
                            let _ = owners.workers.swap_remove(index).join();
                        } else {
                            index += 1;
                        }
                    }
                    match listener.accept() {
                        Ok((socket, peer)) => {
                            if !peer.ip().is_loopback() || owners.workers.len() >= 8 {
                                drop(socket);
                                continue;
                            }
                            let policy = policy.clone();
                            let stop = worker_stop.clone();
                            let credential = credential.clone();
                            if let Ok(worker) = std::thread::Builder::new()
                                .name("mongo-native-relay".into())
                                .spawn(move || {
                                    let _ = serve(socket, policy, stop, &credential);
                                })
                            {
                                owners.workers.push(worker);
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10))
                        }
                        Err(_) => break,
                    }
                }
                drop(owners);
            })?;
        Ok(Self {
            address,
            password,
            stop,
            thread: Some(thread),
        })
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn same_secret(actual: &[u8], expected: &[u8]) -> bool {
    if actual.len() != expected.len() {
        return false;
    }
    actual
        .iter()
        .zip(expected)
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}
fn read_field(socket: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut length = [0];
    socket.read_exact(&mut length)?;
    let mut value = vec![0; length[0] as usize];
    socket.read_exact(&mut value)?;
    Ok(value)
}
fn authorize(
    socket: &mut (impl Read + Write),
    password: &str,
    host: &str,
    port: u16,
) -> Result<()> {
    let mut greeting = [0; 2];
    socket.read_exact(&mut greeting)?;
    ensure!(
        greeting[0] == 5 && greeting[1] > 0,
        "Invalid native SOCKS greeting"
    );
    let mut methods = vec![0; greeting[1] as usize];
    socket.read_exact(&mut methods)?;
    ensure!(
        methods.contains(&2),
        "Native bridge requires credential authentication"
    );
    socket.write_all(&[5, 2])?;
    let mut version = [0];
    socket.read_exact(&mut version)?;
    ensure!(version[0] == 1, "Invalid native SOCKS authentication");
    let user = read_field(socket)?;
    let pass = read_field(socket)?;
    let authenticated = same_secret(&user, b"rustpy") && same_secret(&pass, password.as_bytes());
    socket.write_all(&[1, if authenticated { 0 } else { 1 }])?;
    ensure!(authenticated, "Native bridge credential rejected");
    let mut request = [0; 4];
    socket.read_exact(&mut request)?;
    ensure!(
        request[..3] == [5, 1, 0],
        "Native bridge accepts SOCKS CONNECT only"
    );
    let requested = match request[3] {
        1 => {
            let mut ip = [0; 4];
            socket.read_exact(&mut ip)?;
            std::net::Ipv4Addr::from(ip).to_string()
        }
        3 => String::from_utf8(read_field(socket)?)?,
        4 => {
            let mut ip = [0; 16];
            socket.read_exact(&mut ip)?;
            std::net::Ipv6Addr::from(ip).to_string()
        }
        _ => bail!("Unsupported native SOCKS address"),
    };
    let mut requested_port = [0; 2];
    socket.read_exact(&mut requested_port)?;
    ensure!(
        requested.eq_ignore_ascii_case(host) && u16::from_be_bytes(requested_port) == port,
        "Native bridge rejected target outside captured source authority"
    );
    Ok(())
}
fn idle(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}
fn serve(
    socket: std::net::TcpStream,
    policy: Policy,
    stop: Arc<AtomicBool>,
    password: &str,
) -> Result<()> {
    let mut socket = Socket::new(socket, policy.control.clone(), Duration::from_millis(100))?;
    authorize(
        &mut socket,
        password,
        &policy.source.host,
        policy.source.port,
    )?;
    ensure!(
        !stop.load(Ordering::Acquire) && !policy.interrupted(),
        "Native bridge canceled before upstream admission"
    );
    let mut upstream = policy.open(true)?;
    socket.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])?;
    let mut bytes = [0u8; 8192];
    while !stop.load(Ordering::Acquire) && !policy.interrupted() {
        match socket.read(&mut bytes) {
            Ok(0) => break,
            Ok(n) => {
                upstream.write_all(&bytes[..n])?;
                upstream.flush()?;
            }
            Err(error) if idle(&error) => {}
            Err(error) => return Err(error.into()),
        }
        match upstream.read(&mut bytes) {
            Ok(0) => break,
            Ok(n) => {
                socket.write_all(&bytes[..n])?;
                socket.flush()?;
            }
            Err(error) if idle(&error) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Script {
        input: std::io::Cursor<Vec<u8>>,
        output: Vec<u8>,
    }
    impl Read for Script {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.input.read(bytes)
        }
    }
    impl Write for Script {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.output.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    fn script(password: &str, target: &str, port: u16) -> Script {
        let mut bytes = vec![5, 1, 2, 1, 6];
        bytes.extend_from_slice(b"rustpy");
        bytes.push(password.len() as u8);
        bytes.extend_from_slice(password.as_bytes());
        bytes.extend_from_slice(&[5, 1, 0, 3]);
        bytes.push(target.len() as u8);
        bytes.extend_from_slice(target.as_bytes());
        bytes.extend_from_slice(&port.to_be_bytes());
        Script {
            input: std::io::Cursor::new(bytes),
            output: Vec::new(),
        }
    }
    #[test]
    fn authenticated_original_target_is_required_before_any_outbound_admission() {
        let mut valid = script("captured-private", "db.internal", 27017);
        assert!(authorize(&mut valid, "captured-private", "db.internal", 27017).is_ok());
        assert_eq!(valid.output, vec![5, 2, 1, 0]);
        let mut foreign = script("captured-private", "attacker.internal", 27017);
        assert!(
            authorize(&mut foreign, "captured-private", "db.internal", 27017)
                .unwrap_err()
                .to_string()
                .contains("captured source")
        );
        let mut wrong_port = script("captured-private", "db.internal", 22);
        assert!(authorize(&mut wrong_port, "captured-private", "db.internal", 27017).is_err());
        let mut denied = script("other-secret", "db.internal", 27017);
        assert!(authorize(&mut denied, "captured-private", "db.internal", 27017).is_err());
        assert_eq!(denied.output, vec![5, 2, 1, 1]);
    }
    #[test]
    fn private_authentication_requires_exact_bytes() {
        assert!(same_secret(b"captured", b"captured"));
        assert!(!same_secret(b"captured", b"captureD"));
        assert!(!same_secret(b"", b"captured"));
    }
}
