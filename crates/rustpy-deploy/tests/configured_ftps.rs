//! Real explicit FTPS against an admitted SDK's loopback-only pyftpdlib server.
//! Requires pyftpdlib + pyOpenSSL, OpenSSL CLI and curl; never installs anything.
#![cfg(unix)]
use rustpy_deploy::{
    execute_sync, plan_server_sync,
    transfer::{self, Context},
    transport_for, FtpTarget, Server, ServerKind, SyncAction, SyncDirection,
};
use std::{
    fs::File,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const SERVER: &str = r#"import json, logging, pathlib, sys
from pyftpdlib.authorizers import DummyAuthorizer
from pyftpdlib.handlers import TLS_FTPHandler
from pyftpdlib.servers import FTPServer
logging.disable(logging.CRITICAL)
cfg = json.loads(pathlib.Path(sys.argv[1]).read_text())
authorizer = DummyAuthorizer()
authorizer.add_user(cfg['user'], cfg['password'], cfg['root'], perm='elradfmwMT')
class Handler(TLS_FTPHandler):
    certfile = cfg['cert']
    keyfile = cfg['key']
    tls_control_required = True
    tls_data_required = True
    def record(self, kind):
        with open(cfg['audit'], 'a') as output:
            output.write(kind + '\n')
    def on_login(self, username):
        self.record('LOGIN_TLS')
    def on_file_received(self, path):
        self.record('RECEIVED_TLS')
    def on_file_sent(self, path):
        self.record('SENT_TLS')
Handler.authorizer = authorizer
server = FTPServer(('127.0.0.1', 0), Handler)
pathlib.Path(cfg['ready']).write_text(str(server.socket.getsockname()[1]))
server.serve_forever(timeout=0.1, blocking=True, handle_exit=True)
"#;
struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct OwnedServer(Child);
impl OwnedServer {
    fn stop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
        }
        self.0.wait().unwrap();
        assert!(
            self.0.try_wait().unwrap().is_some(),
            "owned TLS server must be reaped"
        );
    }
}
impl Drop for OwnedServer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn tool(name: &str) -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os(name).unwrap_or_else(|| panic!("explicit {name} admission required")),
    );
    assert!(path.is_absolute() && path.is_file());
    path
}
fn safe(error: impl std::fmt::Display, password: &str) -> String {
    format!("{error:#}").replace(password, "[redacted]")
}

#[test]
#[ignore = "requires admitted Python with pyftpdlib/pyOpenSSL, curl/OpenSSL and isolated root"]
fn configured_explicit_ftps_transfers_sync_reject_untrusted_cert_and_reap_server() {
    let python = tool("RUSTPY_TEST_FTPS_PYTHON");
    let openssl = tool("RUSTPY_TEST_FTPS_OPENSSL");
    let curl = tool("RUSTPY_TEST_FTPS_CURL");
    let parent = PathBuf::from(
        std::env::var_os("RUSTPY_TEST_FTPS_ROOT").expect("isolated existing root required"),
    );
    assert!(parent.is_absolute() && parent.is_dir());
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory =
        Directory(parent.join(format!("configured-ftps-{}-{unique}", std::process::id())));
    std::fs::create_dir(&directory.0).unwrap();
    std::fs::set_permissions(&directory.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = &directory.0;
    let remote_root = root.join("remote");
    std::fs::create_dir(&remote_root).unwrap();
    let local = root.join("local");
    std::fs::create_dir(&local).unwrap();
    let certificate = root.join("server.pem");
    let key = root.join("server-key.pem");
    let certificate_result = Command::new(&openssl)
        .args([
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
        ])
        .arg(&key)
        .arg("-out")
        .arg(&certificate)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        certificate_result.status.success(),
        "admitted OpenSSL must generate the fixture certificate"
    );
    let password = format!("owned-{}-{unique}-p\"w\\d:x y", std::process::id());
    let ready = root.join("ready-port");
    let audit = root.join("audit.log");
    std::fs::write(&audit, "").unwrap();
    let config = root.join("server-config.json");
    std::fs::write(
        &config,
        serde_json::to_vec(&serde_json::json!({
            "user": "owned", "password": password, "root": remote_root,
            "cert": certificate, "key": key, "ready": ready, "audit": audit,
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
    let script = root.join("tls_server.py");
    std::fs::write(&script, SERVER).unwrap();
    let stderr = root.join("server.stderr");
    let mut server = OwnedServer(
        Command::new(&python)
            .arg("-I")
            .arg(&script)
            .arg(&config)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(File::create(&stderr).unwrap())
            .spawn()
            .expect("admitted server interpreter must start"),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let port = loop {
        if let Some(port) = std::fs::read_to_string(&ready)
            .ok()
            .and_then(|text| text.parse::<u16>().ok())
        {
            break port;
        }
        assert!(
            server.0.try_wait().unwrap().is_none(),
            "TLS server failed: {}",
            safe(
                std::fs::read_to_string(&stderr).unwrap_or_default(),
                &password
            )
        );
        assert!(
            Instant::now() < deadline,
            "TLS server did not publish owned loopback readiness"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let target = FtpTarget::new("127.0.0.1", port, "owned", Some(password.clone()));
    let spec =
        rustpy_deploy::ftp::curl_download_command(&target, "/missing", &root.join("not-created"));
    assert!(!spec.display().contains(&password));
    assert!(!format!("{spec:?} {target:?}").contains(&password));
    assert!(
        spec.stdin.is_some(),
        "credentials travel through stdin, not argv"
    );
    let mut deployment = Server::new(
        "owned TLS acceptance",
        ServerKind::Ftp {
            host: "127.0.0.1".into(),
            port,
            user: "owned".into(),
            password: Some(password.clone()),
            save_password: false,
        },
        "/",
    );
    deployment.options.ftp_tls = "explicit".into();
    deployment.options.curl_executable = curl.to_str().unwrap().into();
    deployment.options.timeout_seconds = 10;
    let context = Context {
        environment: vec![
            ("NO_PROXY".into(), "127.0.0.1,localhost".into()),
            ("no_proxy".into(), "127.0.0.1,localhost".into()),
        ],
        ..Default::default()
    };
    transfer::with_context(context, || {
        let untrusted = transport_for(&deployment).unwrap();
        let error = untrusted
            .test_connection()
            .expect_err("fresh self-signed certificate must not be trusted automatically");
        assert!(
            !format!("{error:#}").contains(&password),
            "transport diagnostics must redact session credentials"
        );
        assert!(
            safe(format!("{error:#}"), &password)
                .to_lowercase()
                .contains("certificate"),
            "must fail certificate validation rather than pretend a transfer succeeded"
        );
        assert!(
            std::fs::read_to_string(&audit).unwrap().is_empty(),
            "untrusted TLS must fail before login"
        );
        deployment.options.tls_ca_file = certificate.to_str().unwrap().into();
        let transport = transport_for(&deployment).unwrap();
        transport
            .test_connection()
            .unwrap_or_else(|e| panic!("trusted FTPS connection: {}", safe(e, &password)));
        let upload = local.join("owned.txt");
        std::fs::write(&upload, "owned TLS bytes λ\n").unwrap();
        transport
            .upload(&upload, "/owned.txt")
            .unwrap_or_else(|e| panic!("FTPS upload: {}", safe(e, &password)));
        assert_eq!(
            std::fs::read(remote_root.join("owned.txt")).unwrap(),
            std::fs::read(&upload).unwrap()
        );
        assert!(transport
            .list("/")
            .unwrap_or_else(|e| panic!("FTPS listing: {}", safe(e, &password)))
            .iter()
            .any(|e| e.name == "owned.txt"));
        let downloaded = root.join("downloaded.txt");
        transport
            .download("/owned.txt", &downloaded)
            .unwrap_or_else(|e| panic!("FTPS download: {}", safe(e, &password)));
        assert_eq!(
            std::fs::read(&downloaded).unwrap(),
            std::fs::read(&upload).unwrap()
        );
        std::fs::write(
            local.join("sync-only.txt"),
            "synchronized through real FTPS\n",
        )
        .unwrap();
        let items = plan_server_sync(
            transport.as_ref(),
            &deployment,
            &local,
            &[],
            SyncDirection::Upload,
        )
        .unwrap_or_else(|e| panic!("FTPS sync inventory: {}", safe(e, &password)));
        assert!(items
            .iter()
            .any(|item| item.action == SyncAction::Upload && item.rel_path == "sync-only.txt"));
        let results = execute_sync(
            transport.as_ref(),
            &deployment,
            &local,
            &items,
            |_, _, _| {},
        );
        assert!(!results.is_empty());
        for (_, result) in results {
            result.unwrap_or_else(|e| panic!("FTPS sync execute: {}", safe(e, &password)));
        }
        assert_eq!(
            std::fs::read(remote_root.join("sync-only.txt")).unwrap(),
            std::fs::read(local.join("sync-only.txt")).unwrap()
        );
    });
    let events = std::fs::read_to_string(&audit).unwrap();
    assert!(
        events.contains("LOGIN_TLS")
            && events.contains("RECEIVED_TLS")
            && events.contains("SENT_TLS")
    );
    assert!(!events.contains(&password));
    assert!(!std::fs::read_to_string(&stderr)
        .unwrap()
        .contains(&password));
    server.stop();
    assert!(
        std::net::TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse().unwrap(),
            Duration::from_millis(200)
        )
        .is_err(),
        "retired server must release its listener"
    );
}
