//! Root-admitted loopback TLS acceptance. No vendor endpoints or real credentials.
use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
use ureq::rustls::{
    self,
    pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer},
};

struct Policy(rustpy_net::NetConfig);
impl Drop for Policy {
    fn drop(&mut self) {
        rustpy_net::configure(self.0.clone());
    }
}
struct Server {
    address: std::net::SocketAddr,
    requests: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    response: Arc<Mutex<(u16, String)>>,
    release: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Server {
    fn start(cert: &[u8], key: &[u8]) -> Self {
        let cert = CertificateDer::from_pem_slice(cert).expect("explicit fixture certificate PEM");
        let key = PrivateKeyDer::from_pem_slice(key).expect("explicit fixture private key PEM");
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let response = Arc::new(Mutex::new((200, String::new())));
        let release = Arc::new(AtomicBool::new(true));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let (requests, response, release, stop) = (
                requests.clone(),
                response.clone(),
                release.clone(),
                stop.clone(),
            );
            std::thread::spawn(move || {
                let config = Arc::new(config);
                while !stop.load(Ordering::Acquire) {
                    let tcp = match listener.accept() {
                        Ok((tcp, _)) => tcp,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(error) => panic!("owned fixture accept: {error}"),
                    };
                    tcp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                    tcp.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
                    let mut tls = rustls::StreamOwned::new(
                        rustls::ServerConnection::new(config.clone()).unwrap(),
                        tcp,
                    );
                    let mut header = Vec::new();
                    let mut byte = [0];
                    while header.len() < 16384 && !header.ends_with(b"\r\n\r\n") {
                        match tls.read(&mut byte) {
                            Ok(1) => header.push(byte[0]),
                            _ => break,
                        }
                    }
                    if !header.ends_with(b"\r\n\r\n") {
                        continue;
                    }
                    let header = String::from_utf8(header).unwrap();
                    let length: usize = header
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                                .map(|(_, value)| value.trim().parse().unwrap())
                        })
                        .expect("actual JSON request Content-Length");
                    assert!(length <= 65536);
                    let mut body = vec![0; length];
                    if tls.read_exact(&mut body).is_err() {
                        continue;
                    }
                    requests
                        .lock()
                        .unwrap()
                        .push((header, serde_json::from_slice(&body).unwrap()));
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while !release.load(Ordering::Acquire)
                        && !stop.load(Ordering::Acquire)
                        && Instant::now() < deadline
                    {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    let (status, body) = response.lock().unwrap().clone();
                    if write!(tls, "HTTP/1.1 {status} Fixture\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).is_err() { continue; }
                    for bytes in body.as_bytes().chunks(11) {
                        if tls.write_all(bytes).and_then(|_| tls.flush()).is_err() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
            })
        };
        Self {
            address,
            requests,
            response,
            release,
            stop,
            worker: Some(worker),
        }
    }
    fn await_requests(&self, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.requests.lock().unwrap().len() < count && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            self.requests.lock().unwrap().len(),
            count,
            "actual HTTPS request must reach owned server"
        );
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.release.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !std::thread::panicking() {
                result.expect("owned TLS server thread retired");
            }
        }
    }
}

fn configured_path(name: &str) -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os(name).unwrap_or_else(|| panic!("explicit fixture path {name} required")),
    );
    assert!(
        path.is_absolute() && path.is_file(),
        "{name} must name an admitted absolute fixture file"
    );
    path
}
fn retire(handle: &StreamHandle) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let ids: Vec<_> = handle.worker_id().into_iter().collect();
    while workers_running(&ids) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        !workers_running(&ids),
        "actual AI transport worker must retire"
    );
}

#[test]
#[ignore = "requires RUSTPY_AI_HTTPS_ADMITTED=1 and explicit OpenSSL-generated fixture cert/key PEM paths; run alone"]
fn configured_selected_provider_https_sse_auth_policy_errors_and_retirement() {
    assert_eq!(
        std::env::var("RUSTPY_AI_HTTPS_ADMITTED").as_deref(),
        Ok("1")
    );
    let cert_path = configured_path("RUSTPY_AI_CERT_PEM");
    let key_path = configured_path("RUSTPY_AI_KEY_PEM");
    let cert = std::fs::read(&cert_path).unwrap();
    let server = Server::start(&cert, &std::fs::read(key_path).unwrap());
    let _previous = Policy((*rustpy_net::config()).clone());
    let store = cert_path.parent().unwrap().join(format!(
        "ai-accepted-certs-{}-{}",
        std::process::id(),
        new_worker_owner()
    ));
    std::fs::create_dir(&store).expect("fresh owned certificate store");
    let policy = rustpy_net::NetConfig {
        proxy: rustpy_net::ProxySettings {
            mode: rustpy_net::ProxyMode::NoProxy,
            ..Default::default()
        },
        certificates: rustpy_net::CertificateSettings {
            accept_automatically: false,
            use_system_store: false,
        },
        certs_dir: Some(store),
    };
    rustpy_net::configure(policy.clone());
    let key = "rustpy-fixture-only-key-not-a-vendor-secret";
    let mut config = AiConfig {
        provider: Provider::OpenAiCompatible,
        worker_owner: new_worker_owner(),
        base_url: format!("https://{}/selected/v1", server.address),
        model: "actual-selected-local-model".into(),
        api_key: Some(key.into()),
        max_tokens: 123,
        ..Default::default()
    };
    let request = ChatRequest {
        system: "fixture system".into(),
        messages: vec![Message::user("ACTUAL_HTTPS_REQUEST")],
    };
    let rejected = start_stream(&config, request.clone());
    let events = rejected.wait(Duration::from_secs(10));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AiEvent::Error(_))),
        "untrusted TLS must fail: {events:?}"
    );
    retire(&rejected);
    assert!(
        server.requests.lock().unwrap().is_empty(),
        "untrusted TLS must not leak HTTP auth/body"
    );
    let der = rustpy_net::certs::pem_certificates(&cert);
    assert_eq!(der.len(), 1);
    policy.cert_store().add(&der[0], "127.0.0.1").unwrap();
    *server.response.lock().unwrap() = (
        200,
        concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"actual \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"TLS π\"}}]}\n\n",
            "data: [DONE]\n\n"
        )
        .into(),
    );
    server.release.store(false, Ordering::Release);
    let stream = start_stream(&config, request.clone());
    server.await_requests(1);
    // The selected transport already owns its policy/request snapshot.
    rustpy_net::configure(rustpy_net::NetConfig {
        certs_dir: None,
        ..policy.clone()
    });
    config.base_url = "https://127.0.0.1:1/replaced".into();
    config.api_key = Some("later-fixture-key".into());
    server.release.store(true, Ordering::Release);
    let events = stream.wait(Duration::from_secs(10));
    let text: String = events
        .iter()
        .filter_map(|event| {
            if let AiEvent::Delta(text) = event {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(text, "actual TLS π");
    assert!(
        matches!(events.last(), Some(AiEvent::Done { .. })),
        "{events:?}"
    );
    retire(&stream);
    {
        let requests = server.requests.lock().unwrap();
        let (header, body) = &requests[0];
        assert!(header.starts_with("POST /selected/v1/chat/completions HTTP/1.1\r\n"));
        assert!(header
            .lines()
            .any(|line| line.eq_ignore_ascii_case(&format!("Authorization: Bearer {key}"))));
        assert_eq!(body["model"], "actual-selected-local-model");
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_completion_tokens"], 123);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "ACTUAL_HTTPS_REQUEST");
    }
    rustpy_net::configure(policy);
    config.base_url = format!("https://{}/selected/v1", server.address);
    config.api_key = Some(key.into());
    *server.response.lock().unwrap() =
        (401, format!("{{\"error\":{{\"message\":\"echo {key}\"}}}}"));
    let failed = start_stream(&config, request);
    let events = failed.wait(Duration::from_secs(10));
    server.await_requests(2);
    assert!(
        matches!(events.as_slice(), [AiEvent::Error(message)] if message.contains("401") && !message.contains(key)),
        "HTTP error must be redacted: {events:?}"
    );
    assert!(!format!("{config:?}").contains(key));
    retire(&failed);
    *server.response.lock().unwrap() = (200, "data: [DONE]\n\n".into());
    server.release.store(false, Ordering::Release);
    let cancelled = start_stream(
        &config,
        ChatRequest {
            system: String::new(),
            messages: vec![Message::user("CANCEL_REAL_HTTPS")],
        },
    );
    server.await_requests(3);
    let worker = cancelled
        .worker_id()
        .expect("actual request has an owned worker");
    assert!(workers_running(&[worker]));
    cancelled.cancel();
    assert!(cancelled.is_cancelled() && cancelled.poll().is_empty());
    server.release.store(true, Ordering::Release);
    retire(&cancelled);
}
