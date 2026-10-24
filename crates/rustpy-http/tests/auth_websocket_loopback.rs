//! Explicit actual protocol acceptance; all servers and credentials are owned,
//! ephemeral and loopback-only. No external services or client tools are used.
use rustpy_http::{
    runtime::{self, Control, Options},
    HttpRequest,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

struct Received {
    target: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}
struct Reply {
    status: &'static str,
    headers: Vec<(&'static str, String)>,
    body: String,
}
impl Reply {
    fn json(body: Value) -> Self {
        Self {
            status: "200 OK",
            headers: vec![("Content-Type", "application/json".into())],
            body: body.to_string(),
        }
    }
}
struct Server<T> {
    address: String,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<T>>,
}
impl<T: Send + 'static> Server<T> {
    fn start(handler: impl FnOnce(TcpListener, Arc<AtomicBool>) -> T + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = stop.clone();
        let worker = std::thread::spawn(move || handler(listener, cancelled));
        Self {
            address,
            stop,
            worker: Some(worker),
        }
    }
    fn finish(&mut self) -> T {
        self.worker
            .take()
            .unwrap()
            .join()
            .unwrap_or_else(|_| panic!("owned protocol server failed"))
    }
}
impl<T> Drop for Server<T> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn accept(listener: &TcpListener, stop: &AtomicBool) -> Option<TcpStream> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if stop.load(Ordering::Acquire) {
            return None;
        }
        match listener.accept() {
            Ok((stream, peer)) => {
                assert!(peer.ip().is_loopback());
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                return Some(stream);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "protocol server accept timed out"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("protocol accept failed: {error}"),
        }
    }
}
fn read_request(stream: &mut TcpStream) -> Received {
    let mut bytes = Vec::new();
    let mut buffer = [0; 1024];
    let end = loop {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
        assert!(bytes.len() <= 65536);
        if let Some(at) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8(bytes[..end].to_vec()).unwrap();
    let mut lines = head.lines();
    let target = lines.next().unwrap().to_string();
    let headers: HashMap<_, _> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    let length = headers
        .get("content-length")
        .map_or(0, |value| value.parse::<usize>().unwrap());
    assert!(end + length <= 65536);
    while bytes.len() < end + length {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
        assert!(bytes.len() <= 65536);
    }
    Received {
        target,
        headers,
        body: bytes[end..end + length].to_vec(),
    }
}
fn http_server(
    count: usize,
    mut handler: impl FnMut(usize, &Received) -> Reply + Send + 'static,
) -> Server<usize> {
    Server::start(move |listener, stop| {
        for index in 0..count {
            let Some(mut stream) = accept(&listener, &stop) else {
                return index;
            };
            let request = read_request(&mut stream);
            let reply = handler(index, &request);
            write!(
                stream,
                "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                reply.status,
                reply.body.len()
            )
            .unwrap();
            for (name, value) in reply.headers {
                write!(stream, "{name}: {value}\r\n").unwrap();
            }
            write!(stream, "\r\n{}", reply.body).unwrap();
        }
        count
    })
}
fn options(root: &std::path::Path) -> Options {
    let mut options = Options::new(root.into());
    options.timeout = Duration::from_secs(4);
    options
}
fn ephemeral() -> String {
    rustpy_http::builtin_variable("$uuid").unwrap()
}
fn history_is_private(root: &std::path::Path, completed: &runtime::Completed, secrets: &[&str]) {
    rustpy_http::append_history(root, &completed.request, &completed.response).unwrap();
    let history = std::fs::read_to_string(rustpy_http::history_path(root)).unwrap();
    for secret in secrets {
        assert!(
            !history.contains(secret),
            "history retained an authentication secret"
        );
    }
    let entries = rustpy_http::load_history(root, 1);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].status, completed.response.status);
}
fn sha256(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
// Independent server extraction: only named quoted fields are decoded; the
// expected digest is computed from the independently held challenge/secret.
fn quoted_field(header: &str, field: &str) -> String {
    let start = header
        .find(&format!("{field}=\""))
        .expect("Digest field missing")
        + field.len()
        + 2;
    let mut chars = header[start..].chars();
    let mut value = String::new();
    loop {
        match chars.next().expect("unterminated Digest quoted field") {
            '"' => return value,
            '\\' => value.push(chars.next().unwrap()),
            c => value.push(c),
        }
    }
}

#[test]
#[ignore = "explicit actual Digest SHA-256 loopback challenge/retry and refusal acceptance"]
fn digest_quoted_qop_and_realm_complete_real_sha256_exchange_without_secret_history() {
    let root = tempfile::tempdir().unwrap();
    let password = ephemeral();
    let nonce = ephemeral();
    let realm = "private, \"scope\"";
    let opaque = "opaque,\\marker";
    let server_password = password.clone();
    let server_nonce = nonce.clone();
    let mut server = http_server(2, move |index, request| {
        assert_eq!(request.target, "GET /digest?x=1 HTTP/1.1");
        if index == 0 {
            assert!(!request.headers.contains_key("authorization"));
            Reply { status: "401 Unauthorized", headers: vec![("WWW-Authenticate", format!("Digest realm=\"private, \\\"scope\\\"\", nonce=\"{server_nonce}\", algorithm=SHA-256, qop=\"auth-int,auth\", opaque=\"opaque,\\\\marker\""))], body: String::new() }
        } else {
            let header = &request.headers["authorization"];
            assert!(header.starts_with("Digest "));
            assert_eq!(quoted_field(header, "username"), "fixture-user");
            assert_eq!(quoted_field(header, "realm"), realm);
            assert_eq!(quoted_field(header, "nonce"), server_nonce);
            assert_eq!(quoted_field(header, "opaque"), opaque);
            assert_eq!(quoted_field(header, "uri"), "/digest?x=1");
            assert!(header.contains("qop=auth, nc=00000001"));
            let cnonce = quoted_field(header, "cnonce");
            assert!(!cnonce.is_empty());
            let ha1 = sha256(&format!("fixture-user:{realm}:{server_password}"));
            let ha2 = sha256("GET:/digest?x=1");
            let expected = sha256(&format!(
                "{ha1}:{server_nonce}:00000001:{cnonce}:auth:{ha2}"
            ));
            assert_eq!(quoted_field(header, "response"), expected);
            Reply::json(json!({"authenticated":"digest"}))
        }
    });
    let mut request = HttpRequest {
        method: "GET".into(),
        url: format!("http://{}/digest?x=1", server.address),
        ..Default::default()
    };
    request.directives.insert(
        "auth".into(),
        json!({"type":"digest","username":"fixture-user","password":password}).to_string(),
    );
    let completed = runtime::execute(request, options(root.path()), &Control::default());
    assert!(
        completed.response.is_success(),
        "{:?}",
        completed.response.error
    );
    assert_eq!(
        serde_json::from_str::<Value>(&completed.response.body).unwrap(),
        json!({"authenticated":"digest"})
    );
    assert_eq!(server.finish(), 2);
    history_is_private(root.path(), &completed, &[&password]);
    for (challenge, error) in [
        (
            "Digest realm=\"private\", nonce=\"nonce\", qop=\"auth-int\"",
            "qop=auth",
        ),
        (
            "Digest realm=\"unterminated, nonce=\"nonce\"",
            "Malformed Digest challenge",
        ),
        (
            "Basic realm=\"private\"",
            "Unsupported authentication challenge",
        ),
    ] {
        let mut server = http_server(1, move |_, request| {
            assert!(!request.headers.contains_key("authorization"));
            Reply {
                status: "401 Unauthorized",
                headers: vec![("WWW-Authenticate", challenge.into())],
                body: String::new(),
            }
        });
        let mut request = completed.request.clone();
        request
            .headers
            .retain(|(name, _)| !name.eq_ignore_ascii_case("authorization"));
        request.url = format!("http://{}/digest?x=1", server.address);
        let refused = runtime::execute(request, options(root.path()), &Control::default());
        assert_eq!(refused.response.status, 0);
        assert!(refused
            .response
            .error
            .as_deref()
            .is_some_and(|message| message.contains(error)));
        assert_eq!(
            server.finish(),
            1,
            "refusal must not retry with credentials"
        );
    }
}

#[test]
#[ignore = "explicit actual OAuth2 token form and protected resource loopback acceptance"]
fn oauth2_real_token_exchange_authorizes_resource_and_refuses_failed_or_missing_token() {
    let root = tempfile::tempdir().unwrap();
    let secret = format!("{} +&", ephemeral());
    let token = ephemeral();
    let server_secret = secret.clone();
    let server_token = token.clone();
    let mut server = http_server(2, move |index, request| {
        if index == 0 {
            assert_eq!(request.target, "POST /token HTTP/1.1");
            assert_eq!(
                request.headers["content-type"],
                "application/x-www-form-urlencoded"
            );
            assert!(!request.headers.contains_key("authorization"));
            let form: HashMap<_, _> = url::form_urlencoded::parse(&request.body)
                .into_owned()
                .collect();
            assert_eq!(form["grant_type"], "client_credentials");
            assert_eq!(form["client_id"], "client π");
            assert!(
                form.get("client_secret") == Some(&server_secret),
                "token form must preserve the private encoded secret"
            );
            assert_eq!(form["scope"], "read write");
            Reply::json(json!({"access_token":server_token,"token_type":"Bearer"}))
        } else {
            assert_eq!(request.target, "GET /protected HTTP/1.1");
            assert!(
                request.headers.get("authorization") == Some(&format!("Bearer {server_token}")),
                "resource must receive acquired Bearer token"
            );
            assert!(request.body.is_empty());
            Reply::json(json!({"authenticated":"oauth2"}))
        }
    });
    let mut request = HttpRequest {
        method: "GET".into(),
        url: format!("http://{}/protected", server.address),
        ..Default::default()
    };
    request.directives.insert("auth".into(), json!({"type":"oauth2","token_url":format!("http://{}/token",server.address),"client_id":"client π","client_secret":secret,"scope":"read write"}).to_string());
    let completed = runtime::execute(request, options(root.path()), &Control::default());
    assert!(
        completed.response.is_success(),
        "{:?}",
        completed.response.error
    );
    assert_eq!(
        serde_json::from_str::<Value>(&completed.response.body).unwrap(),
        json!({"authenticated":"oauth2"})
    );
    assert_eq!(server.finish(), 2);
    history_is_private(root.path(), &completed, &[&secret, &token]);
    for rejected in [true, false] {
        let mut server = http_server(1, move |_, request| {
            assert_eq!(request.target, "POST /token HTTP/1.1");
            if rejected {
                Reply {
                    status: "401 Unauthorized",
                    headers: vec![],
                    body: String::new(),
                }
            } else {
                Reply::json(json!({"token_type":"Bearer"}))
            }
        });
        let mut request = HttpRequest {
            method: "GET".into(),
            url: format!("http://{}/protected", server.address),
            ..Default::default()
        };
        request.directives.insert("auth".into(), json!({"type":"oauth2","token_url":format!("http://{}/token",server.address),"client_id":"fixture","client_secret":secret}).to_string());
        let failed = runtime::execute(request, options(root.path()), &Control::default());
        assert_eq!(failed.response.status, 0);
        let error = failed.response.error.as_deref().unwrap();
        assert!(error.contains(if rejected {
            "OAuth2 token exchange failed"
        } else {
            "access_token"
        }));
        assert_eq!(
            server.finish(),
            1,
            "failed token must not reach protected resource"
        );
    }
}

struct OwnedHandshake;
impl tungstenite::handshake::server::Callback for OwnedHandshake {
    fn on_request(
        self,
        request: &tungstenite::handshake::server::Request,
        mut response: tungstenite::handshake::server::Response,
    ) -> Result<
        tungstenite::handshake::server::Response,
        tungstenite::handshake::server::ErrorResponse,
    > {
        assert_eq!(request.uri().path(), "/events");
        assert_eq!(request.headers()["x-captured"], "owned");
        assert!(request.headers().contains_key("sec-websocket-key"));
        response.headers_mut().insert(
            "x-accepted",
            tungstenite::http::HeaderValue::from_static("owned"),
        );
        Ok(response)
    }
}

#[test]
#[ignore = "explicit actual WebSocket upgrade, bidirectional frames and rejection acceptance"]
fn websocket_real_upgrade_frames_live_queue_and_handshake_refusal() {
    let root = tempfile::tempdir().unwrap();
    let mut server = Server::start(|listener, stop| {
        let Some(stream) = accept(&listener, &stop) else {
            return false;
        };
        let mut socket = tungstenite::accept_hdr(stream, OwnedHandshake)
            .expect("real WebSocket server handshake");
        assert_eq!(
            socket.read().unwrap().into_text().unwrap().as_str(),
            "subscribe π"
        );
        assert_eq!(
            socket.read().unwrap().into_text().unwrap().as_str(),
            "live π"
        );
        socket
            .send(tungstenite::Message::Text("echo π".into()))
            .unwrap();
        socket
            .send(tungstenite::Message::Binary(vec![0, 1, 255].into()))
            .unwrap();
        socket.close(None).unwrap();
        true
    });
    let request = HttpRequest {
        method: "WEBSOCKET".into(),
        url: format!("ws://{}/events", server.address),
        headers: vec![("X-Captured".into(), "owned".into())],
        body: Some("subscribe π".into()),
        timeout_ms: Some(3000),
        connection_timeout_ms: Some(2000),
        ..Default::default()
    };
    let control = Control::default();
    control.send("live π".into());
    let completed = runtime::execute(request, options(root.path()), &control);
    assert!(
        completed.response.error.is_none(),
        "{:?}",
        completed.response.error
    );
    assert_eq!(completed.response.status, 101);
    assert_eq!(completed.response.header("x-accepted"), Some("owned"));
    assert!(completed.response.body.contains("echo π\n"));
    assert!(completed.response.body.contains("[binary 3 bytes] AAH/"));
    assert_eq!(control.messages(), vec!["echo π", "[binary 3 bytes] AAH/"]);
    assert!(server.finish());
    let mut refused = http_server(1, |_, request| {
        assert_eq!(request.target, "GET /not-websocket HTTP/1.1");
        assert!(request.headers.contains_key("sec-websocket-key"));
        Reply::json(json!({"error":"no upgrade"}))
    });
    let request = HttpRequest {
        method: "WEBSOCKET".into(),
        url: format!("ws://{}/not-websocket", refused.address),
        ..Default::default()
    };
    let control = Control::default();
    let failed = runtime::execute(request, options(root.path()), &control);
    assert_eq!(failed.response.status, 0);
    assert!(failed.response.error.is_some());
    assert!(control.messages().is_empty());
    assert_eq!(refused.finish(), 1);
}
