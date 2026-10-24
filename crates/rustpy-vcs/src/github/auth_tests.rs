//! Genuine loopback HTTP exchanges through public authorize; no live account.
use super::*;
use rustpy_settings::github::Provider;
use serde_json::json;
use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
};
#[derive(Debug)]
struct Request {
    at: Instant,
    path: String,
    headers: String,
    body: String,
}
struct Http {
    server: Server,
    requests: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl Http {
    fn start(replies: impl FnOnce(&str) -> Vec<String>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let mut replies: VecDeque<_> = replies(&origin).into();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let received = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        // BSD sockets inherit the listener's non-blocking mode.
                        stream.set_nonblocking(false).unwrap();
                        let request = read_request(&stream);
                        received.lock().unwrap().push(request);
                        let body = replies
                            .pop_front()
                            .unwrap_or_else(|| json!({"error":"unexpected_request"}).to_string());
                        let mut stream = stream;
                        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("owned HTTP listener failed: {error}"),
                }
            }
        });
        Self {
            // Server fields are public already. Production Server::new still
            // requires HTTPS; this owned loopback fixture changes no policy.
            server: Server {
                web: origin.clone(),
                api: origin,
                provider: Provider::GitHub,
            },
            requests,
            stop,
            worker: Some(worker),
        }
    }
    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}
impl Drop for Http {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !thread::panicking() {
                result.unwrap();
            }
        }
    }
}
fn read_request(stream: &TcpStream) -> Request {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    assert!(first.starts_with("POST "));
    let path = first.split_whitespace().nth(1).unwrap().to_string();
    let mut headers = String::new();
    let mut length = None;
    loop {
        let mut line = String::new();
        assert!(reader.read_line(&mut line).unwrap() > 0);
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
        headers.push_str(&line.to_ascii_lowercase());
        assert!(headers.len() < 16384);
    }
    let length = length.unwrap();
    assert!(length < 16384);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    Request {
        at: Instant::now(),
        path,
        headers,
        body: String::from_utf8(body).unwrap(),
    }
}
fn code(origin: &str, expires: u64) -> String {
    json!({"device_code":"device +/&π", "user_code":"ABCD-1234", "verification_uri":format!("{origin}/login/device"), "expires_in":expires, "interval":1}).to_string()
}
#[test]
fn public_device_flow_posts_encoded_fields_presents_code_and_obeys_slow_down() {
    let http = Http::start(|origin| {
        vec![
            code(origin, 30),
            json!({"error":"authorization_pending"}).to_string(),
            json!({"error":"slow_down"}).to_string(),
            json!({"access_token":"owned-fixture-token"}).to_string(),
        ]
    });
    let shown = Mutex::new(Vec::new());
    let started = Instant::now();
    let token = authorize(
        &http.server,
        "client +/&π",
        &CancellationToken::new(),
        |value| shown.lock().unwrap().push(value),
    )
    .unwrap();
    assert_eq!(token, "owned-fixture-token");
    let shown = shown.lock().unwrap();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0].code, "ABCD-1234");
    assert_eq!(shown[0].uri, format!("{}/login/device", http.server.web));
    assert_eq!(shown[0].expires_in, 30);
    let requests = http.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0].path, "/login/device/code");
    assert_eq!(requests[0].body, "client_id=client%20%2B%2F%26%CF%80&scope=repo%20gist%20read%3Aorg%20workflow%20notifications");
    for request in &requests[1..] {
        assert_eq!(request.path, "/login/oauth/access_token");
        assert_eq!(request.body, "client_id=client%20%2B%2F%26%CF%80&device_code=device%20%2B%2F%26%CF%80&grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code");
        assert!(request.headers.contains("accept: application/json\r\n"));
        assert!(request
            .headers
            .contains("content-type: application/x-www-form-urlencoded\r\n"));
    }
    assert!(requests[1].at.duration_since(started) >= Duration::from_secs(1));
    assert!(requests[2].at.duration_since(requests[1].at) >= Duration::from_secs(1));
    assert!(requests[3].at.duration_since(requests[2].at) >= Duration::from_secs(6));
}
#[test]
fn public_device_flow_cancellation_and_expiry_prevent_poll_side_effects() {
    let http = Http::start(|origin| vec![code(origin, 30)]);
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(authorize(&http.server, "client", &cancelled, |_| panic!(
        "cancelled flow presented code"
    ))
    .unwrap_err()
    .contains("cancelled"));
    assert_eq!(http.count(), 0);
    let cancel = CancellationToken::new();
    assert!(
        authorize(&http.server, "client", &cancel, |_| cancel.cancel())
            .unwrap_err()
            .contains("cancelled")
    );
    assert_eq!(http.count(), 1);
    let waiting = Http::start(|origin| vec![code(origin, 30)]);
    let during_wait = CancellationToken::new();
    let canceller = Mutex::new(None);
    let result = authorize(&waiting.server, "client", &during_wait, |_| {
        let token = during_wait.clone();
        *canceller.lock().unwrap() = Some(thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            token.cancel();
        }));
    });
    canceller.lock().unwrap().take().unwrap().join().unwrap();
    assert!(result.unwrap_err().contains("cancelled"));
    assert_eq!(
        waiting.count(),
        1,
        "cancelled interval must not send a token request"
    );
    let expired = Http::start(|origin| vec![code(origin, 1)]);
    assert!(
        authorize(&expired.server, "client", &CancellationToken::new(), |_| {})
            .unwrap_err()
            .contains("expired")
    );
    assert_eq!(
        expired.count(),
        1,
        "expiry during interval must not send a token request"
    );
    let delayed = Http::start(|origin| vec![code(origin, 1)]);
    assert!(
        authorize(&delayed.server, "client", &CancellationToken::new(), |_| {
            thread::sleep(Duration::from_millis(1100))
        })
        .unwrap_err()
        .contains("expired")
    );
    assert_eq!(
        delayed.count(),
        1,
        "presentation must not restart the expiry clock"
    );
}
#[test]
fn public_device_flow_denial_server_expiry_and_untrusted_responses_are_refused() {
    for (error, expected) in [("access_denied", "declined"), ("expired_token", "expired")] {
        let http = Http::start(|origin| vec![code(origin, 30), json!({"error":error}).to_string()]);
        assert!(
            authorize(&http.server, "client", &CancellationToken::new(), |_| {})
                .unwrap_err()
                .contains(expected)
        );
        assert_eq!(http.count(), 2);
    }
    for (reply, expected) in [(json!({"device_code":"owned", "user_code":"owned", "verification_uri":"https://foreign.invalid/login/device"}).to_string(), "unexpected origin"), ("x".repeat(65537), "too large")] {
        let http = Http::start(|_| vec![reply]);
        let error = authorize(&http.server, "client", &CancellationToken::new(), |_| panic!("untrusted response was presented")).unwrap_err();
        assert!(error.contains(expected), "{error}");
        assert_eq!(http.count(), 1);
    }
}
