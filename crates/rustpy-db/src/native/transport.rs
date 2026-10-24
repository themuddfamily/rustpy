//! Captured native socket authority. TLS always verifies the original source
//! hostname, including when raw bytes travel through an owned OpenSSH pipe.
use super::*;
use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer, ServerName};
use std::{
    io::{self, Read, Write},
    net::{TcpStream, ToSocketAddrs},
    sync::Mutex,
    time::Instant,
};
#[derive(Clone)]
pub(super) struct Policy {
    pub source: Source,
    pub cfg: Arc<rustpy_net::NetConfig>,
    pub env: Vec<(String, Option<String>)>,
    pub control: Arc<Control>,
    pub timeout: Duration,
    pub tls: Option<Arc<rustls::ClientConfig>>,
}
impl Policy {
    pub fn capture(
        source: Source,
        cfg: Arc<rustpy_net::NetConfig>,
        env: Vec<(String, Option<String>)>,
        control: Arc<Control>,
        timeout: Duration,
    ) -> Result<Self> {
        let tls = if source.options.get("tls").is_some_and(|v| v == "true") {
            let roots = if let Some(path) = source.options.get("tlsCAFile") {
                let pem = read_private(path)?;
                CertificateDer::pem_slice_iter(&pem)
                    .map(|cert| cert.map(|cert| cert.as_ref().to_vec()))
                    .collect::<std::result::Result<Vec<_>, _>>()?
            } else {
                Vec::new()
            };
            ensure!(
                !source.options.contains_key("tlsCAFile") || !roots.is_empty(),
                "Selected source CA file contains no PEM certificate"
            );
            let mut tls = (*rustpy_net::tls_config_with_roots(&cfg, &roots)?).clone();
            let combined = source
                .options
                .get("tlsCertificateKeyFile")
                .map(|path| read_private(path))
                .transpose()?;
            let cert = source
                .options
                .get("tlsClientCert")
                .map(|path| read_private(path))
                .transpose()?
                .or_else(|| combined.clone());
            let key = source
                .options
                .get("tlsClientKey")
                .map(|path| read_private(path))
                .transpose()?
                .or(combined);
            ensure!(
                cert.is_some() == key.is_some(),
                "Select both native TLS client certificate and key"
            );
            if let Some((cert, key)) = cert.zip(key) {
                let certs = CertificateDer::pem_slice_iter(&cert)
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let key = PrivateKeyDer::from_pem_slice(&key)?;
                let certified = rustls::sign::CertifiedKey::from_der(
                    certs,
                    key,
                    &rustls::crypto::ring::default_provider(),
                )?;
                tls.client_auth_cert_resolver =
                    Arc::new(rustls::sign::SingleCertAndKey::from(certified));
            }
            Some(Arc::new(tls))
        } else {
            None
        };
        Ok(Self {
            source,
            cfg,
            env,
            control,
            timeout,
            tls,
        })
    }
    pub fn interrupted(&self) -> bool {
        self.control.interrupted()
    }
    pub fn open(&self, polling: bool) -> Result<Wire> {
        ensure!(
            !self.interrupted(),
            "Native transport canceled before connection"
        );
        let mut target = url::Url::parse("https://localhost/")?;
        target
            .set_host(Some(&self.source.host))
            .map_err(|_| anyhow::anyhow!("Invalid source hostname"))?;
        target
            .set_port(Some(self.source.port))
            .map_err(|_| anyhow::anyhow!("Invalid source port"))?;
        let route = if self.cfg.proxy.mode == rustpy_net::ProxyMode::Pac {
            rustpy_net::pac::choose_with(&self.cfg, target.as_str())
        } else {
            rustpy_net::proxy::choose(&self.cfg.proxy, target.as_str(), &|name| {
                self.env
                    .iter()
                    .find(|(key, _)| key == name)
                    .and_then(|(_, value)| value.clone())
            })
        };
        let raw = if let Some(tunnel) = &self.source.ssh {
            ensure!(matches!(route,rustpy_net::ProxyChoice::Direct),"Combining native SSH with a shared proxy route is not supported; no direct SSH fallback was attempted");
            Raw::Ssh(SshPipe::open(self, tunnel)?)
        } else {
            Raw::Tcp(Socket::new(
                route_socket(route, &self.source.host, self.source.port, self.timeout)?,
                self.control.clone(),
                self.timeout,
            )?)
        };
        let mut wire = if let Some(config) = &self.tls {
            let name = ServerName::try_from(self.source.host.clone())?;
            let mut stream =
                rustls::StreamOwned::new(rustls::ClientConnection::new(config.clone(), name)?, raw);
            while stream.conn.is_handshaking() {
                ensure!(!self.interrupted(), "Native TLS handshake canceled");
                stream.conn.complete_io(&mut stream.sock)?;
            }
            Wire::Tls(Box::new(stream))
        } else {
            Wire::Plain(raw)
        };
        if polling {
            wire.set_read_timeout(Duration::from_millis(20))?;
        }
        Ok(wire)
    }
}
fn read_private(path: &str) -> Result<Vec<u8>> {
    let metadata = std::fs::metadata(path)?;
    ensure!(
        metadata.is_file() && metadata.len() <= 4 * 1024 * 1024,
        "Native TLS file exceeds 4 MiB"
    );
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 4 * 1024 * 1024,
        "Native TLS file changed beyond its 4 MiB budget"
    );
    Ok(bytes)
}
fn connect(host: &str, port: u16, timeout: Duration) -> io::Result<TcpStream> {
    let deadline = Instant::now() + timeout;
    let mut error = io::Error::other("No native endpoint addresses");
    for address in (host, port).to_socket_addrs()?.take(32) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        match TcpStream::connect_timeout(&address, remaining) {
            Ok(stream) => {
                stream.set_read_timeout(Some(remaining))?;
                stream.set_write_timeout(Some(remaining))?;
                return Ok(stream);
            }
            Err(e) => error = e,
        }
    }
    Err(error)
}
fn route_socket(
    route: rustpy_net::ProxyChoice,
    host: &str,
    port: u16,
    timeout: Duration,
) -> Result<TcpStream> {
    let routes = match route {
        rustpy_net::ProxyChoice::Pac(routes) => routes,
        route => vec![route],
    };
    ensure!(
        !routes.is_empty() && routes.len() <= 16,
        "Native PAC route list must contain 1..16 routes"
    );
    let deadline = Instant::now() + timeout;
    let mut last = anyhow::anyhow!("No native proxy routes");
    for route in routes {
        let remaining = deadline.saturating_duration_since(Instant::now());
        ensure!(!remaining.is_zero(), "Native route deadline elapsed");
        let result = (|| -> Result<TcpStream> {
            match route {
                rustpy_net::ProxyChoice::Direct => Ok(connect(host, port, remaining)?),
                rustpy_net::ProxyChoice::Http(proxy) => {
                    let mut stream = connect(&proxy.host, proxy.port, remaining)?;
                    rustpy_net::http_bridge::connect_tunnel(&mut stream, &proxy, host, port)?;
                    Ok(stream)
                }
                rustpy_net::ProxyChoice::Socks(proxy, version) => {
                    let stream =
                        rustpy_net::socks::connect(&proxy, version, host, port, remaining)?;
                    stream.set_read_timeout(Some(remaining))?;
                    stream.set_write_timeout(Some(remaining))?;
                    Ok(stream)
                }
                rustpy_net::ProxyChoice::Unavailable(error) => {
                    bail!("Native proxy unavailable: {error}")
                }
                rustpy_net::ProxyChoice::Pac(_) => bail!("Nested native PAC route is invalid"),
            }
        })();
        match result {
            Ok(stream) => return Ok(stream),
            Err(error) => last = error,
        }
    }
    Err(last)
}
pub(super) enum Wire {
    Plain(Raw),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, Raw>>),
}
impl Wire {
    pub fn set_read_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        match self {
            Self::Plain(raw) => raw.set_read_timeout(timeout),
            Self::Tls(stream) => stream.sock.set_read_timeout(timeout),
        }
    }
}
impl Read for Wire {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(raw) => raw.read(bytes),
            Self::Tls(stream) => stream.read(bytes),
        }
    }
}
impl Write for Wire {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(raw) => raw.write(bytes),
            Self::Tls(stream) => stream.write(bytes),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(raw) => raw.flush(),
            Self::Tls(stream) => stream.flush(),
        }
    }
}
pub(super) struct Socket {
    stream: TcpStream,
    control: Arc<Control>,
    id: u64,
    read_timeout: Duration,
    write_timeout: Duration,
}
impl Socket {
    pub fn new(stream: TcpStream, control: Arc<Control>, timeout: Duration) -> io::Result<Self> {
        let id = control.next_socket.fetch_add(1, Ordering::Relaxed);
        control
            .sockets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, stream.try_clone()?);
        let socket = Self {
            stream,
            control,
            id,
            read_timeout: timeout,
            write_timeout: timeout,
        };
        if socket.control.interrupted() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        Ok(socket)
    }
}
impl Read for Socket {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if self.control.interrupted() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        self.stream
            .set_read_timeout(Some(self.control.remaining(self.read_timeout)))?;
        self.stream.read(bytes)
    }
}
impl Write for Socket {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.control.interrupted() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        self.stream
            .set_write_timeout(Some(self.control.remaining(self.write_timeout)))?;
        self.stream.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}
impl Drop for Socket {
    fn drop(&mut self) {
        self.control
            .sockets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}
pub(super) enum Raw {
    Tcp(Socket),
    Ssh(SshPipe),
}
impl Raw {
    fn set_read_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        match self {
            Self::Tcp(socket) => {
                socket.read_timeout = timeout;
                Ok(())
            }
            Self::Ssh(pipe) => {
                pipe.read_timeout = timeout;
                Ok(())
            }
        }
    }
}
impl Read for Raw {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(socket) => socket.read(bytes),
            Self::Ssh(pipe) => pipe.read(bytes),
        }
    }
}
impl Write for Raw {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(socket) => socket.write(bytes),
            Self::Ssh(pipe) => pipe.write(bytes),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(socket) => socket.flush(),
            Self::Ssh(pipe) => pipe.flush(),
        }
    }
}
pub(super) struct SshPipe {
    writer: Option<std::process::ChildStdin>,
    rx: Receiver<Vec<u8>>,
    buffer: std::collections::VecDeque<u8>,
    stop: Arc<AtomicBool>,
    waiter: Option<JoinHandle<()>>,
    read_timeout: Duration,
}
impl SshPipe {
    fn open(policy: &Policy, tunnel: &crate::workspace::Tunnel) -> Result<Self> {
        let mut command =
            crate::session::captured_ssh_command(tunnel, &tunnel.remote_host, tunnel.remote_port)?;
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        rustpy_core::process_tree::ProcessTree::prepare(&mut command);
        let mut child = command.spawn()?;
        let owner = match rustpy_core::process_tree::ProcessTree::new(&mut child) {
            Ok(owner) => owner,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        };
        let writer = child.stdin.take();
        let mut stdout = child.stdout.take().expect("piped SSH stdout");
        let mut stderr = child.stderr.take().expect("piped SSH stderr");
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = sync_channel(4);
        let reader_stop = stop.clone();
        let reader = std::thread::Builder::new()
            .name("native-ssh-output".into())
            .spawn(move || {
                let mut bytes = [0u8; 8192];
                loop {
                    match stdout.read(&mut bytes) {
                        Ok(0) => break,
                        Ok(n) => {
                            let mut chunk = bytes[..n].to_vec();
                            while !reader_stop.load(Ordering::Acquire) {
                                match tx.try_send(chunk) {
                                    Ok(()) => break,
                                    Err(std::sync::mpsc::TrySendError::Full(value)) => {
                                        chunk = value;
                                        std::thread::sleep(Duration::from_millis(5));
                                    }
                                    Err(_) => return,
                                }
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
            });
        let reader = match reader {
            Ok(reader) => reader,
            Err(error) => {
                owner.force(&mut child);
                return Err(error.into());
            }
        };
        let stderr_reader = std::thread::Builder::new()
            .name("native-ssh-errors".into())
            .spawn(move || {
                let mut bytes = [0u8; 8192];
                loop {
                    match stderr.read(&mut bytes) {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
            });
        let stderr_reader = match stderr_reader {
            Ok(reader) => reader,
            Err(error) => {
                stop.store(true, Ordering::Release);
                owner.force(&mut child);
                let _ = reader.join();
                return Err(error.into());
            }
        };
        let parts = Arc::new(Mutex::new(Some((child, owner, reader, stderr_reader))));
        let worker_parts = parts.clone();
        let worker_stop = stop.clone();
        let control = policy.control.clone();
        let waiter = std::thread::Builder::new()
            .name("native-ssh-owner".into())
            .spawn(move || {
                let (mut child, owner, reader, stderr_reader) = worker_parts
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take()
                    .expect("SSH ownership transferred once");
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| loop {
                    if worker_stop.load(Ordering::Acquire) || control.interrupted() {
                        owner.request_force();
                    }
                    match owner.poll(&mut child) {
                        Ok(Some(_)) => break,
                        Ok(None) => {}
                        Err(_) => owner.request_force(),
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }));
                worker_stop.store(true, Ordering::Release);
                owner.force(&mut child);
                let _ = reader.join();
                let _ = stderr_reader.join();
            });
        let waiter = match waiter {
            Ok(waiter) => waiter,
            Err(error) => {
                stop.store(true, Ordering::Release);
                if let Some((mut child, owner, reader, stderr_reader)) =
                    parts.lock().unwrap_or_else(|e| e.into_inner()).take()
                {
                    owner.force(&mut child);
                    let _ = reader.join();
                    let _ = stderr_reader.join();
                }
                return Err(error.into());
            }
        };
        Ok(Self {
            writer,
            rx,
            buffer: Default::default(),
            stop,
            waiter: Some(waiter),
            read_timeout: policy.timeout,
        })
    }
}
impl Read for SshPipe {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        if self.buffer.is_empty() {
            match self.rx.recv_timeout(self.read_timeout) {
                Ok(chunk) => self.buffer.extend(chunk),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    return Err(io::ErrorKind::TimedOut.into())
                }
                Err(_) => return Ok(0),
            }
        }
        let n = bytes.len().min(self.buffer.len());
        for target in &mut bytes[..n] {
            *target = self.buffer.pop_front().expect("buffer length checked");
        }
        Ok(n)
    }
}
impl Write for SshPipe {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.writer
            .as_mut()
            .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?
            .write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.writer
            .as_mut()
            .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?
            .flush()
    }
}
impl Drop for SshPipe {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        drop(self.writer.take());
        if let Some(waiter) = self.waiter.take() {
            let _ = waiter.join();
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn source_private_invalid_roots_are_rejected_by_shared_verifier_builder() {
        let config = rustpy_net::NetConfig::default();
        assert!(rustpy_net::tls_config_with_roots(&config, &[vec![0, 255, 1]]).is_err());
        assert!(rustpy_net::tls_config_with_roots(&config, &[]).is_ok());
    }
}
