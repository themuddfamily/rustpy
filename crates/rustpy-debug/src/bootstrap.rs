//! Private startup channel for the endpoint bound by this debuggee's debugpy.
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver};
use serde_json::Value;

pub(crate) const PYTHON: &str = r#"import os, sys, runpy
try:
    import debugpy
except ModuleNotFoundError as error:
    if error.name != 'debugpy':
        raise
    sys.stderr.write(sys.executable + ': No module named debugpy\n')
    sys.exit(1)
_rustpy_address = os.environ.pop('RUSTPY_DEBUG_BOOTSTRAP_ADDRESS')
_rustpy_token = os.environ.pop('RUSTPY_DEBUG_BOOTSTRAP_TOKEN')
_rustpy_listen = debugpy.listen
def _rustpy_publish(address, *args, **kwargs):
    endpoint = _rustpy_listen(address, *args, **kwargs)
    debugpy.listen = _rustpy_listen
    if os.environ.pop('RUSTPY_DEBUG_REPR_ENABLED', '1') == '0':
        # Use pydevd's presentation provider extension before the first values
        # request. Avoid calling user __repr__/__str__ for custom objects.
        from _pydevd_bundle.pydevd_extension_api import StrPresentationProvider
        from _pydevd_bundle.pydevd_extension_utils import extensions_of_type
        class RustPyObjectIdentity(StrPresentationProvider):
            def can_provide(self, type_object, type_name):
                return getattr(type_object, '__module__', '') != 'builtins'
            def get_str(self, value):
                return object.__repr__(value)
        extensions_of_type(StrPresentationProvider).insert(0, RustPyObjectIdentity())
    import socket, json, time
    host, port = _rustpy_address.rsplit(':', 1)
    deadline = time.monotonic() + 20
    with socket.create_connection((host, int(port)), timeout=20) as channel:
        channel.sendall((json.dumps({'token': _rustpy_token, 'pid': os.getpid(), 'host': endpoint[0], 'port': endpoint[1]}) + '\n').encode('utf-8'))
        acknowledgement = b''
        while len(acknowledgement) < 3:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError('RustPy debugger startup acknowledgement timed out')
            channel.settimeout(remaining)
            part = channel.recv(3 - len(acknowledgement))
            if not part:
                raise RuntimeError('RustPy debugger startup cancelled')
            acknowledgement += part
        if acknowledgement != b'OK\n':
            raise RuntimeError('Invalid RustPy debugger startup acknowledgement')
    return endpoint
debugpy.listen = _rustpy_publish
sys.argv[0] = 'debugpy'
runpy.run_module('debugpy', run_name='__main__', alter_sys=True)
"#;

pub(crate) struct Bootstrap {
    listener: TcpListener,
    token: String,
}

pub(crate) struct PendingEndpoint {
    pub(crate) incoming: Receiver<Result<String, String>>,
    cancel: Arc<AtomicBool>,
}

impl Drop for PendingEndpoint {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

impl Bootstrap {
    pub(crate) fn new() -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|error| {
            anyhow::anyhow!("Cannot generate debugger startup capability: {error}")
        })?;
        let token = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(Self { listener, token })
    }

    pub(crate) fn address(&self) -> anyhow::Result<String> {
        Ok(self.listener.local_addr()?.to_string())
    }

    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    pub(crate) fn receive(self, pid: u32) -> anyhow::Result<PendingEndpoint> {
        let (sender, incoming) = unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        std::thread::Builder::new()
            .name("debugpy-endpoint".into())
            .spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(20);
                let result = (|| {
                    while !stop.load(Ordering::SeqCst) && Instant::now() < deadline {
                        match self.listener.accept() {
                            Ok((mut stream, peer)) if peer.ip().is_loopback() => {
                                if let Some(endpoint) =
                                    read_endpoint(&mut stream, &self.token, pid, deadline, &stop)
                                {
                                    if stop.load(Ordering::SeqCst) {
                                        return None;
                                    }
                                    return Some(
                                        stream.write_all(b"OK\n").map(|()| endpoint).map_err(
                                            |error| {
                                                format!(
                                                    "Cannot acknowledge debugger endpoint: {error}"
                                                )
                                            },
                                        ),
                                    );
                                }
                            }
                            Ok(_) => {}
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                std::thread::sleep(Duration::from_millis(20))
                            }
                            Err(error) => {
                                return Some(Err(format!(
                                    "Cannot receive debugger endpoint: {error}"
                                )));
                            }
                        }
                    }
                    if stop.load(Ordering::SeqCst) {
                        None
                    } else {
                        Some(Err("Timed out waiting for owned debugpy endpoint".into()))
                    }
                })();
                // Physical socket release precedes both result publication and
                // sender disconnection: cancellation observers can rely on it.
                drop(self.listener);
                if !stop.load(Ordering::SeqCst) {
                    if let Some(result) = result {
                        let _ = sender.send(result);
                    }
                }
            })?;
        Ok(PendingEndpoint { incoming, cancel })
    }
}

fn read_endpoint(
    stream: &mut TcpStream,
    token: &str,
    pid: u32,
    deadline: Instant,
    cancel: &AtomicBool,
) -> Option<String> {
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .ok()?;
    stream
        .set_write_timeout(Some(Duration::from_millis(100)))
        .ok()?;
    // An unauthenticated idle connection cannot consume the whole startup
    // budget and prevent the genuine child from publishing its endpoint.
    let deadline = deadline.min(Instant::now() + Duration::from_secs(1));
    let mut buffer = Vec::new();
    while !cancel.load(Ordering::SeqCst) && Instant::now() < deadline {
        let mut bytes = [0; 512];
        match stream.read(&mut bytes) {
            Ok(0) => return None,
            Ok(count) => {
                buffer.extend_from_slice(&bytes[..count]);
                if buffer.len() > 4096 {
                    return None;
                }
                if let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
                    let value: Value = serde_json::from_slice(&buffer[..end]).ok()?;
                    return validate_endpoint(&value, token, pid);
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return None,
        }
    }
    None
}

fn validate_endpoint(value: &Value, token: &str, pid: u32) -> Option<String> {
    if value["token"].as_str()? != token || value["pid"].as_u64()? != u64::from(pid) {
        return None;
    }
    let host: std::net::IpAddr = value["host"].as_str()?.parse().ok()?;
    let port = u16::try_from(value["port"].as_u64()?).ok()?;
    if !host.is_loopback() || port == 0 {
        return None;
    }
    Some(std::net::SocketAddr::new(host, port).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn endpoint_requires_capability_child_identity_and_actual_loopback_port() {
        let good = json!({"token":"private", "pid":12, "host":"127.0.0.1", "port":4321});
        assert_eq!(
            validate_endpoint(&good, "private", 12),
            Some("127.0.0.1:4321".into())
        );
        for (key, replacement) in [
            ("token", json!("wrong")),
            ("pid", json!(13)),
            ("host", json!("192.0.2.1")),
            ("port", json!(0)),
            ("port", json!(65536)),
        ] {
            let mut bad = good.clone();
            bad[key] = replacement;
            assert!(validate_endpoint(&bad, "private", 12).is_none());
        }
    }
    #[test]
    fn actual_channel_rejects_invalid_frames_then_accepts_owned_endpoint() {
        let bootstrap = Bootstrap::new().unwrap();
        let address = bootstrap.address().unwrap();
        let token = bootstrap.token().to_owned();
        let pending = bootstrap.receive(123).unwrap();
        for frame in [b"not-json\n".as_slice(), b"{}\n".as_slice()] {
            let mut unrelated = TcpStream::connect(&address).unwrap();
            unrelated.write_all(frame).unwrap();
        }
        let mut owner = TcpStream::connect(&address).unwrap();
        owner
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let frame = json!({"token":token, "pid":123, "host":"127.0.0.1", "port":4321});
        writeln!(owner, "{frame}").unwrap();
        let mut acknowledgement = [0; 3];
        owner.read_exact(&mut acknowledgement).unwrap();
        assert_eq!(&acknowledgement, b"OK\n");
        assert_eq!(
            pending
                .incoming
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap(),
            "127.0.0.1:4321"
        );
    }

    #[test]
    fn cancellation_drops_actual_private_listener_without_publishing_endpoint() {
        let bootstrap = Bootstrap::new().unwrap();
        // Observe the owned kernel socket, not its reusable numeric address.
        // Another parallel test can bind that address after our listener closes.
        #[cfg(not(target_os = "linux"))]
        let address = bootstrap.address().unwrap();
        #[cfg(target_os = "linux")]
        let (descriptor_path, original_socket) = {
            use std::os::fd::AsRawFd;
            let path = format!("/proc/self/fd/{}", bootstrap.listener.as_raw_fd());
            let socket = std::fs::read_link(&path).unwrap();
            assert!(socket.to_string_lossy().starts_with("socket:["));
            (path, socket)
        };
        let pending = bootstrap.receive(123).unwrap();
        let incoming = pending.incoming.clone();
        drop(pending);
        assert!(matches!(
            incoming.recv_timeout(Duration::from_secs(2)),
            Err(crossbeam_channel::RecvTimeoutError::Disconnected)
        ));
        #[cfg(target_os = "linux")]
        match std::fs::read_link(descriptor_path) {
            Ok(current) => assert_ne!(
                current, original_socket,
                "the exact owned listener must be released before disconnection"
            ),
            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
        }
        #[cfg(not(target_os = "linux"))]
        assert!(TcpStream::connect(&address).is_err());
    }
    #[test]
    fn python_shim_accepts_fragmented_ack_and_delegates_cli_once() {
        use std::process::{Command, Stdio};
        let Some(python) = which::which("python3").ok() else {
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        let package = directory.path().join("debugpy");
        std::fs::create_dir(&package).unwrap();
        std::fs::write(
            package.join("__init__.py"),
            "def listen(address):\n    return ('127.0.0.1', 4321)\n",
        )
        .unwrap();
        std::fs::write(package.join("__main__.py"), "import debugpy,sys\ndebugpy.listen(('127.0.0.1',0))\nprint('cli-once',repr(sys.argv[1:]))\n").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        struct OwnedChild(std::process::Child);
        impl Drop for OwnedChild {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut child = OwnedChild(
            Command::new(python)
                .args([
                    "-c",
                    PYTHON,
                    "--listen",
                    "127.0.0.1:0",
                    "--wait-for-client",
                    "target.py",
                    "argument",
                ])
                .current_dir(directory.path())
                .env_remove("DISPLAY")
                .env_remove("WAYLAND_DISPLAY")
                .env(
                    "RUSTPY_DEBUG_BOOTSTRAP_ADDRESS",
                    listener.local_addr().unwrap().to_string(),
                )
                .env("RUSTPY_DEBUG_BOOTSTRAP_TOKEN", "fragment-test")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        drop(child.0.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut channel = loop {
            match listener.accept() {
                Ok((channel, _)) => break channel,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(20))
                }
                other => panic!("shim did not publish its endpoint: {other:?}"),
            }
        };
        assert_eq!(
            read_endpoint(
                &mut channel,
                "fragment-test",
                child.0.id(),
                deadline,
                &AtomicBool::new(false)
            ),
            Some("127.0.0.1:4321".into())
        );
        for byte in b"OK\n" {
            channel.write_all(&[*byte]).unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
        while child.0.try_wait().unwrap().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(child
            .0
            .try_wait()
            .unwrap()
            .is_some_and(|status| status.success()));
        let mut output = String::new();
        child
            .0
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut output)
            .unwrap();
        assert_eq!(
            output,
            "cli-once ['--listen', '127.0.0.1:0', '--wait-for-client', 'target.py', 'argument']\n"
        );
    }
}
