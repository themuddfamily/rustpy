//! Explicitly admitted local HTTPS acceptance, run alone because network policy
//! is process-global. No external endpoint, installed server, or private CA used.
use super::*;
use rustpy_core::{CancellationToken, TaskManager};
use rustpy_index::{refresh, BuildOptions, IndexMeta, RootKind, RootSpec, SourceFile};
use std::io::Write;
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::process::Command;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread::JoinHandle;
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
    address: SocketAddr,
    body: Arc<Mutex<Vec<u8>>>,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl Server {
    fn start(cert: Vec<u8>, key: Vec<u8>, bytes: Vec<u8>) -> Self {
        let cert = CertificateDer::from_pem_slice(&cert).expect("generated certificate PEM");
        let key = PrivateKeyDer::from_pem_slice(&key).expect("generated private key PEM");
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
        let stop = Arc::new(AtomicBool::new(false));
        let body = Arc::new(Mutex::new(bytes));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let worker = {
            let stop = stop.clone();
            let body = body.clone();
            let requests = requests.clone();
            let config = Arc::new(config);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    let tcp = match listener.accept() {
                        Ok((tcp, _)) => tcp,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(error) => panic!("local TLS listener: {error}"),
                    };
                    tcp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                    tcp.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
                    let connection = rustls::ServerConnection::new(config.clone()).unwrap();
                    let mut tls = rustls::StreamOwned::new(connection, tcp);
                    let mut head = Vec::new();
                    let mut byte = [0];
                    while head.len() < 16384 && !head.ends_with(b"\r\n\r\n") {
                        match tls.read(&mut byte) {
                            Ok(1) => head.push(byte[0]),
                            _ => break, // Includes the deliberately rejected TLS handshake.
                        }
                    }
                    if !head.ends_with(b"\r\n\r\n") {
                        continue;
                    }
                    requests
                        .lock()
                        .unwrap()
                        .push(String::from_utf8(head).unwrap());
                    let bytes = body.lock().unwrap().clone();
                    let response = write!(
                        tls,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        bytes.len()
                    )
                    .and_then(|()| tls.write_all(&bytes))
                    .and_then(|()| tls.flush());
                    if response.is_ok() {
                        tls.conn.send_close_notify();
                        let _ = tls.flush();
                    }
                }
            })
        };
        Self {
            address,
            body,
            requests,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            // Socket timeouts bound shutdown even after a failed assertion.
            let result = worker.join();
            if !std::thread::panicking() {
                result.expect("TLS fixture owner retired");
            }
        }
    }
}

fn generate_certificate(openssl: &Path, directory: &Path) {
    let token = CancellationToken::new();
    let (done, receiver) = std::sync::mpsc::channel();
    let deadline_token = token.clone();
    let watchdog = std::thread::spawn(move || {
        if receiver.recv_timeout(Duration::from_secs(30)).is_err() {
            deadline_token.cancel();
        }
    });
    let result = rustpy_core::process::output_cancellable(
        Command::new(openssl).current_dir(directory).args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=localhost",
            "-addext",
            "subjectAltName=IP:127.0.0.1",
            "-keyout",
            "key.pem",
            "-out",
            "cert.pem",
        ]),
        &token,
    );
    let _ = done.send(());
    watchdog.join().unwrap();
    let output = result.expect("explicitly admitted OpenSSL must run under supervisor");
    assert!(
        output.status.success(),
        "certificate generation: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn bundle(root: &Path) -> Vec<u8> {
    std::fs::create_dir_all(root).unwrap();
    let path = root.join("library.py");
    std::fs::write(&path, "def exported(): pass\n").unwrap();
    let (index, _) = refresh(
        None,
        IndexMeta::default(),
        &[RootSpec {
            path: root.into(),
            kind: RootKind::Stdlib,
        }],
        vec![SourceFile { root: 0, path }],
        &[],
        &BuildOptions {
            threads: 1,
            ..Default::default()
        },
        &Default::default(),
    )
    .unwrap();
    rustpy_index::format::encode(&index.unwrap().to_bundle(
        &[RootKind::Stdlib],
        IndexMeta {
            label: "configured HTTPS fixture".into(),
            ..Default::default()
        },
    ))
}

fn download(request: Request, store: IndexStore) -> Result<Ready, TaskError> {
    let manager = TaskManager::new();
    let mut job = manager
        .task("Configured HTTPS shared index")
        .spawn(move |progress| fetch(request, store, progress));
    if let Some(result) = job.wait_timeout(Duration::from_secs(20)) {
        return result;
    }
    job.cancel();
    let result = job.wait(); // Retain the real worker until its cancellation/drain completes.
    panic!(
        "local shared-index transfer exceeded deadline: {}",
        result.err().map(|e| e.to_string()).unwrap_or_default()
    );
}

#[test]
#[ignore = "requires explicit RUSTPY_SHARED_INDEX_HTTPS_ADMITTED=1 and absolute RUSTPY_TEST_OPENSSL; run alone"]
fn configured_https_fetch_checks_tls_hash_schema_then_publishes_reusable_content() {
    assert_eq!(
        std::env::var("RUSTPY_SHARED_INDEX_HTTPS_ADMITTED").as_deref(),
        Ok("1"),
        "explicit local TLS execution admission required"
    );
    let openssl = PathBuf::from(
        std::env::var_os("RUSTPY_TEST_OPENSSL").expect("absolute admitted OpenSSL path required"),
    );
    assert!(
        openssl.is_absolute() && openssl.is_file(),
        "OpenSSL must be an explicitly selected executable"
    );
    let directory = tempfile::tempdir().unwrap();
    generate_certificate(&openssl, directory.path());
    let cert = std::fs::read(directory.path().join("cert.pem")).unwrap();
    let bytes = bundle(&directory.path().join("publisher"));
    let server = Server::start(
        cert.clone(),
        std::fs::read(directory.path().join("key.pem")).unwrap(),
        bytes.clone(),
    );
    let previous = Policy((*rustpy_net::config()).clone());
    let config = rustpy_net::NetConfig {
        proxy: rustpy_net::ProxySettings {
            mode: rustpy_net::ProxyMode::NoProxy,
            ..Default::default()
        },
        certificates: rustpy_net::CertificateSettings {
            accept_automatically: false,
            use_system_store: false,
        },
        certs_dir: Some(directory.path().join("accepted-certs")),
    };
    rustpy_net::configure(config.clone());
    let store = IndexStore::new(directory.path().join("cache"));
    let request = Request {
        url: format!("https://{}/bundle.rpidx", server.address),
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        limit: 1024 * 1024,
    };
    let rejected = download(request.clone(), store.clone())
        .err()
        .expect("untrusted local TLS must be refused");
    let error = rejected.to_string().to_lowercase();
    assert!(
        error.contains("certificate") || error.contains("tls"),
        "expected actual TLS policy rejection, got: {error}"
    );
    assert!(
        server.requests.lock().unwrap().is_empty(),
        "untrusted TLS must not send HTTP"
    );
    let der = rustpy_net::certs::pem_certificates(&cert);
    config.cert_store().add(&der[0], "127.0.0.1").unwrap();
    let ready = download(request.clone(), store.clone())
        .expect("explicit certificate pin admits actual HTTPS fetch");
    assert_eq!(ready.files, 1);
    assert!(
        !ready.path.exists(),
        "worker must only stage, never publish"
    );
    let path = ready.path.clone();
    ready.stage.finish().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let mut bad_hash = request.clone();
    bad_hash.sha256 = "0".repeat(64);
    assert!(download(bad_hash, store.clone())
        .err()
        .unwrap()
        .to_string()
        .contains("SHA-256"));
    let mut future = bytes.clone();
    future[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    *server.body.lock().unwrap() = future.clone();
    let mut future_request = request.clone();
    future_request.sha256 = format!("{:x}", Sha256::digest(&future));
    assert!(
        download(future_request, store.clone()).is_err(),
        "matching hash must not bypass schema validation"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        bytes,
        "rejected transfers preserve installed content"
    );
    *server.body.lock().unwrap() = bytes;
    let staged = download(request, store.clone()).unwrap();
    drop(staged);
    assert!(
        IndexStore::load(&path).is_ok(),
        "discarded replacement preserves committed bundle"
    );
    let root = directory.path().join("recipient");
    std::fs::create_dir(&root).unwrap();
    let source = root.join("library.py");
    std::fs::write(&source, "def exported(): pass\n").unwrap();
    let roots = [RootSpec {
        path: root,
        kind: RootKind::Stdlib,
    }];
    let files = vec![SourceFile {
        root: 0,
        path: source.clone(),
    }];
    let bundles = store.load_bundles();
    assert_eq!(bundles.len(), 1);
    let options = BuildOptions {
        threads: 1,
        ..Default::default()
    };
    let (_, stats) = refresh(
        None,
        IndexMeta::default(),
        &roots,
        files.clone(),
        &bundles,
        &options,
        &Default::default(),
    )
    .unwrap();
    assert_eq!((stats.from_shared, stats.parsed), (1, 0));
    std::fs::write(source, "def changed(): pass\n").unwrap();
    let (_, stats) = refresh(
        None,
        IndexMeta::default(),
        &roots,
        files,
        &bundles,
        &options,
        &Default::default(),
    )
    .unwrap();
    assert_eq!((stats.from_shared, stats.parsed), (0, 1));
    assert_eq!(
        server.requests.lock().unwrap().len(),
        4,
        "all admitted attempts traverse actual HTTPS"
    );
    drop(previous);
    drop(server); // Join the test-owned server before deleting keys and cache.
}
