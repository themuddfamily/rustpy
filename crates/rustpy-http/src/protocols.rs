//! Real WebSocket frames and grpcurl RPC execution with retained cancellation ownership.
use crate::{
    runtime::{error, Control, Options},
    HttpRequest, HttpResponse,
};
use base64::Engine;
use std::{
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
fn remaining(deadline: Instant) -> Result<Duration, String> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| "WebSocket connection deadline exceeded".into())
}
fn connect(address: &str, deadline: Instant) -> Result<TcpStream, String> {
    let addresses = address
        .to_socket_addrs()
        .map_err(|error| error.to_string())?;
    let mut last = "No address resolved".to_string();
    for address in addresses {
        match TcpStream::connect_timeout(&address, remaining(deadline)?) {
            Ok(stream) => return Ok(stream),
            Err(error) => last = error.to_string(),
        }
    }
    Err(last)
}
/// Set the remaining overall budget before every handshake read/write rather
/// than renewing a full timeout for every byte supplied by a slow proxy.
struct TimedSocket {
    stream: TcpStream,
    deadline: Instant,
}
impl Read for TimedSocket {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let timeout = remaining(self.deadline).map_err(std::io::Error::other)?;
        self.stream.set_read_timeout(Some(timeout))?;
        self.stream.read(buffer)
    }
}
impl Write for TimedSocket {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let timeout = remaining(self.deadline).map_err(std::io::Error::other)?;
        self.stream.set_write_timeout(Some(timeout))?;
        self.stream.write(buffer)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let timeout = remaining(self.deadline).map_err(std::io::Error::other)?;
        self.stream.set_write_timeout(Some(timeout))?;
        self.stream.flush()
    }
}
fn ordered_routes<T>(
    choice: rustpy_net::ProxyChoice,
    mut attempt: impl FnMut(rustpy_net::ProxyChoice) -> Result<T, String>,
) -> Result<T, String> {
    let mut pending = vec![choice];
    let mut routes = Vec::new();
    let mut visited = 0;
    while let Some(choice) = pending.pop() {
        visited += 1;
        if visited > 256 {
            return Err("PAC route nesting exceeds limit".into());
        }
        match choice {
            rustpy_net::ProxyChoice::Pac(candidates) => {
                if candidates.len() > 16 || pending.len() + candidates.len() > 16 {
                    return Err("PAC exceeds sixteen routes".into());
                }
                pending.extend(candidates.into_iter().rev());
            }
            route => {
                routes.push(route);
                if routes.len() > 16 {
                    return Err("PAC exceeds sixteen routes".into());
                }
            }
        }
    }
    let mut failures = Vec::new();
    for route in routes {
        match route {
            rustpy_net::ProxyChoice::Unavailable(message) => failures.push(message),
            route => match attempt(route) {
                Ok(stream) => return Ok(stream),
                Err(error) => failures.push(error),
            },
        }
    }
    Err(if failures.is_empty() {
        "PAC returned no routes".into()
    } else {
        failures.join("; ")
    })
}
fn socket(
    url: &url::Url,
    policy: &rustpy_net::NetConfig,
    deadline: Instant,
) -> Result<TcpStream, String> {
    let host = url.host_str().ok_or("WebSocket URL has no host")?;
    let port = url
        .port_or_known_default()
        .ok_or("WebSocket URL has no port")?;
    let authority = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let mut proxy_url = url.clone();
    let _ = proxy_url.set_scheme(if url.scheme() == "wss" {
        "https"
    } else {
        "http"
    });
    let route = if policy.proxy.mode == rustpy_net::ProxyMode::Pac {
        rustpy_net::pac::choose_with(policy, proxy_url.as_str())
    } else {
        rustpy_net::proxy::choose(
            &policy.proxy,
            proxy_url.as_str(),
            &rustpy_net::proxy::process_env,
        )
    };
    ordered_routes(route, |route| {
        remaining(deadline)?;
        match route {
            rustpy_net::ProxyChoice::Direct => connect(&authority, deadline),
            rustpy_net::ProxyChoice::Socks(proxy, version) => {
                let mut tunnel = TimedSocket {
                    stream: connect(&proxy.address(), deadline)?,
                    deadline,
                };
                match version {
                    rustpy_net::SocksVersion::V5 => {
                        rustpy_net::socks::socks5_handshake(&mut tunnel, &proxy, host, port)
                    }
                    version => rustpy_net::socks::socks4_handshake(
                        &mut tunnel,
                        &proxy,
                        host,
                        port,
                        version,
                    ),
                }
                .map_err(|error| error.to_string())?;
                remaining(deadline)?;
                Ok(tunnel.stream)
            }
            rustpy_net::ProxyChoice::Http(proxy) => {
                let mut tunnel = TimedSocket {
                    stream: connect(&proxy.address(), deadline)?,
                    deadline,
                };
                let auth = proxy
                    .user
                    .as_ref()
                    .map(|user| {
                        format!(
                            "Proxy-Authorization: Basic {}\r\n",
                            base64::engine::general_purpose::STANDARD.encode(format!(
                                "{user}:{}",
                                proxy.password.as_deref().unwrap_or("")
                            ))
                        )
                    })
                    .unwrap_or_default();
                tunnel
                    .write_all(
                        format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n{auth}\r\n")
                            .as_bytes(),
                    )
                    .map_err(|error| error.to_string())?;
                let mut response = Vec::new();
                while !response.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    tunnel
                        .read_exact(&mut byte)
                        .map_err(|error| error.to_string())?;
                    response.push(byte[0]);
                    if response.len() > 64 * 1024 {
                        return Err("Proxy CONNECT response too large".into());
                    }
                }
                if !String::from_utf8_lossy(&response)
                    .lines()
                    .next()
                    .is_some_and(|line| line.split_whitespace().nth(1) == Some("200"))
                {
                    return Err("Proxy refused WebSocket CONNECT tunnel".into());
                }
                Ok(tunnel.stream)
            }
            rustpy_net::ProxyChoice::Unavailable(error) => Err(error),
            rustpy_net::ProxyChoice::Pac(_) => Err("Nested PAC route was not expanded".into()),
        }
    })
}
pub fn websocket(request: &HttpRequest, options: &Options, control: &Control) -> HttpResponse {
    let started = Instant::now();
    let result = (|| -> Result<HttpResponse, String> {
        use tungstenite::client::IntoClientRequest;
        let url = url::Url::parse(&request.url).map_err(|error| error.to_string())?;
        if !matches!(url.scheme(), "ws" | "wss") {
            return Err("WEBSOCKET requires ws:// or wss://".into());
        }
        let timeout = Duration::from_millis(
            request
                .connection_timeout_ms
                .unwrap_or(5000)
                .clamp(1, 60_000),
        );
        let network_policy = rustpy_net::config();
        let connection_deadline = Instant::now() + timeout;
        let stream = socket(&url, &network_policy, connection_deadline)?;
        let handshake_timeout = remaining(connection_deadline)?;
        control.set_socket(stream.try_clone().map_err(|error| error.to_string())?);
        stream
            .set_read_timeout(Some(handshake_timeout))
            .map_err(|error| error.to_string())?;
        stream
            .set_write_timeout(Some(handshake_timeout))
            .map_err(|error| error.to_string())?;
        let mut handshake = request
            .url
            .as_str()
            .into_client_request()
            .map_err(|error| error.to_string())?;
        for (key, value) in &request.headers {
            let name = tungstenite::http::header::HeaderName::from_bytes(key.as_bytes())
                .map_err(|error| error.to_string())?;
            handshake.headers_mut().insert(
                name,
                value
                    .parse()
                    .map_err(|error| format!("Invalid WebSocket header: {error}"))?,
            );
        }
        let mut configuration = tungstenite::protocol::WebSocketConfig::default();
        configuration.max_message_size = Some(16 * 1024 * 1024);
        configuration.max_frame_size = Some(16 * 1024 * 1024);
        let (mut socket, response) = tungstenite::client_tls_with_config(
            handshake,
            stream,
            Some(configuration),
            Some(tungstenite::Connector::Rustls(rustpy_net::tls_config(
                &network_policy,
            ))),
        )
        .map_err(|error| error.to_string())?;
        match socket.get_mut() {
            tungstenite::stream::MaybeTlsStream::Plain(stream) => {
                let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
            }
            tungstenite::stream::MaybeTlsStream::Rustls(stream) => {
                let _ = stream
                    .sock
                    .set_read_timeout(Some(Duration::from_millis(100)));
            }
            _ => {}
        }
        for message in request
            .body
            .as_deref()
            .unwrap_or("")
            .split("\n===\n")
            .filter(|message| !message.is_empty())
        {
            socket
                .send(tungstenite::Message::Text(message.to_owned().into()))
                .map_err(|error| error.to_string())?;
        }
        let limit = Duration::from_millis(
            request
                .timeout_ms
                .unwrap_or(options.timeout.as_millis() as u64)
                .clamp(1, 300_000),
        );
        let mut body = String::new();
        while started.elapsed() < limit && !control.cancelled() {
            for message in control.take_outgoing() {
                socket
                    .send(tungstenite::Message::Text(message.into()))
                    .map_err(|error| error.to_string())?;
            }
            match socket.read() {
                Ok(tungstenite::Message::Text(message)) => {
                    if body.len() + message.len() > 16 * 1024 * 1024 {
                        return Err("WebSocket transcript exceeds 16 MiB".into());
                    }
                    body.push_str(&message);
                    body.push('\n');
                    control.message(message.to_string());
                    control
                        .received
                        .store(body.len(), std::sync::atomic::Ordering::Relaxed);
                }
                Ok(tungstenite::Message::Binary(bytes)) => {
                    let message = format!(
                        "[binary {} bytes] {}",
                        bytes.len(),
                        base64::engine::general_purpose::STANDARD.encode(bytes)
                    );
                    if body.len() + message.len() + 1 > 16 * 1024 * 1024 {
                        return Err("WebSocket transcript exceeds 16 MiB".into());
                    }
                    body.push_str(&message);
                    body.push('\n');
                    control.message(message);
                }
                Ok(tungstenite::Message::Close(_)) | Err(tungstenite::Error::ConnectionClosed) => {
                    break
                }
                Ok(_) => {
                    let _ = socket.flush();
                }
                Err(tungstenite::Error::Io(error))
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        let _ = socket.close(None);
        let bytes = body.as_bytes().to_vec();
        Ok(HttpResponse {
            status: 101,
            status_text: "Switching Protocols".into(),
            headers: response
                .headers()
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_str().unwrap_or("").into()))
                .collect(),
            size: bytes.len(),
            bytes,
            body,
            content_type: "text/websocket-transcript".into(),
            elapsed: started.elapsed(),
            ..Default::default()
        })
    })();
    result.unwrap_or_else(error)
}
pub fn grpc(request: &HttpRequest, options: &Options, control: &Control) -> HttpResponse {
    let started = Instant::now();
    let result = (|| -> Result<HttpResponse, String> {
        let url = url::Url::parse(&request.url).map_err(|error| error.to_string())?;
        let method = url.path().trim_start_matches('/');
        if method.is_empty() {
            return Err("GRPC URL must include /package.Service/Method".into());
        }
        let host = url.host_str().ok_or("GRPC URL has no host")?;
        let authority = format!(
            "{host}:{}",
            url.port()
                .unwrap_or(if matches!(url.scheme(), "grpc" | "http") {
                    80
                } else {
                    443
                })
        );
        let mut command = Command::new(&options.grpcurl);
        options.process_environment.apply(&mut command);
        command
            .envs(rustpy_net::proxy_env_vars())
            .current_dir(
                options
                    .process_environment
                    .working_dir
                    .as_deref()
                    .unwrap_or(&options.base),
            )
            .args([
                "-max-time",
                &request
                    .timeout_ms
                    .map(|millis| (millis as f64 / 1000.0).to_string())
                    .unwrap_or_else(|| options.timeout.as_secs_f64().to_string()),
            ]);
        if matches!(url.scheme(), "grpc" | "http") {
            command.arg("-plaintext");
        }
        if rustpy_net::config().certificates.accept_automatically {
            command.arg("-insecure");
        }
        if let Some(path) = request.directives.get("grpc-cacert") {
            command.args(["-cacert", path]);
        }
        for (key, flag) in [("proto", "-proto"), ("import-path", "-import-path")] {
            if let Some(value) = request.directives.get(key) {
                command.args([flag, value]);
            }
        }
        for (key, value) in &request.headers {
            command.args(["-H", &format!("{key}: {value}")]);
        }
        command
            .args(["-d", "@", &authority, method])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| format!("Cannot start grpcurl: {error}"))?;
        if let Err(error) = child
            .stdin
            .take()
            .unwrap()
            .write_all(request.body.as_deref().unwrap_or("{}").as_bytes())
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.to_string());
        }
        let output = child.stdout.take().unwrap();
        let errors = child.stderr.take().unwrap();
        let out = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            output
                .take(16 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        });
        let err = std::thread::spawn(move || {
            let mut bytes = String::new();
            errors
                .take(64 * 1024)
                .read_to_string(&mut bytes)
                .map(|_| bytes)
        });
        let status = loop {
            if control.cancelled()
                || started.elapsed()
                    > request
                        .timeout_ms
                        .map(Duration::from_millis)
                        .unwrap_or(options.timeout)
            {
                let _ = child.kill();
                let _ = child.wait();
                let _ = out.join();
                let _ = err.join();
                return Err("gRPC request cancelled or timed out".into());
            }
            if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                break status;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let bytes = out
            .join()
            .map_err(|_| "gRPC stdout worker stopped")?
            .map_err(|error| error.to_string())?;
        let stderr = err.join().ok().and_then(Result::ok).unwrap_or_default();
        if bytes.len() > 16 * 1024 * 1024 {
            return Err("gRPC response exceeds 16 MiB".into());
        }
        Ok(HttpResponse {
            status: if status.success() { 200 } else { 500 },
            status_text: if status.success() {
                "gRPC OK"
            } else {
                "gRPC error"
            }
            .into(),
            body: String::from_utf8_lossy(&bytes).into_owned(),
            size: bytes.len(),
            bytes,
            error: (!status.success()).then_some(stderr),
            content_type: "application/grpc+json".into(),
            elapsed: started.elapsed(),
            ..Default::default()
        })
    })();
    result.unwrap_or_else(error)
}

#[cfg(test)]
mod pac_route_tests {
    use super::*;
    use rustpy_net::ProxyChoice;
    #[test]
    fn ordered_candidates_fail_over_without_inventing_direct() {
        let proxy = rustpy_net::ProxyServer {
            host: "first.proxy".into(),
            port: 3128,
            user: None,
            password: None,
        };
        let routes = ProxyChoice::Pac(vec![ProxyChoice::Http(proxy), ProxyChoice::Direct]);
        let mut seen = Vec::new();
        let result = ordered_routes(routes, |route| {
            seen.push(route.clone());
            if matches!(route, ProxyChoice::Direct) {
                Ok(7)
            } else {
                Err("proxy refused".into())
            }
        });
        assert_eq!(result.unwrap(), 7);
        assert!(matches!(
            seen.as_slice(),
            [ProxyChoice::Http(_), ProxyChoice::Direct]
        ));
        let unavailable =
            ordered_routes::<()>(ProxyChoice::Unavailable("PAC failed".into()), |_| {
                panic!("no route may be invented")
            });
        assert_eq!(unavailable.unwrap_err(), "PAC failed");
        let mut attempts = 0;
        let only_proxy = ProxyChoice::Pac(vec![ProxyChoice::Http(rustpy_net::ProxyServer {
            host: "only.proxy".into(),
            port: 3128,
            user: None,
            password: None,
        })]);
        assert!(ordered_routes::<()>(only_proxy, |route| {
            assert!(matches!(route, ProxyChoice::Http(_)));
            attempts += 1;
            Err("unreachable proxy".into())
        })
        .is_err());
        assert_eq!(attempts, 1);
    }
    #[test]
    fn empty_and_oversized_pac_never_connect_and_deadlines_do_not_renew() {
        assert!(
            ordered_routes::<()>(ProxyChoice::Pac(vec![]), |_| panic!("empty policy")).is_err()
        );
        assert!(
            ordered_routes::<()>(ProxyChoice::Pac(vec![ProxyChoice::Direct; 17]), |_| panic!(
                "oversized policy"
            ))
            .is_err()
        );
        assert!(remaining(Instant::now() - Duration::from_millis(1)).is_err());
    }
}
