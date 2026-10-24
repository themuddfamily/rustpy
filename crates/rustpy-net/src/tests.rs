//! End-to-end tests against local servers: TLS with untrusted certificates
//! (prompts, accepted certificates, accepted CAs, automatic acceptance) and HTTP
//! proxies (CONNECT tunnels, absolute-form requests, Basic authentication).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use ureq::rustls;

use crate::test_certs::*;
use crate::*;

/// HTTPS server answering every request with `body`.
fn tls_server(cert_pem: &str, key_pem: &str, body: &'static str) -> SocketAddr {
    tls_server_recording(cert_pem, key_pem, body).0
}

/// [`tls_server`] that also records the request heads it receives.
fn tls_server_recording(
    cert_pem: &str,
    key_pem: &str,
    body: &'static str,
) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let certs = vec![CertificateDer::from(pem_to_der(cert_pem))];
    let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).unwrap();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(certs, key)
    .unwrap();
    let config = Arc::new(config);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let heads = Arc::new(Mutex::new(Vec::new()));
    let recorded = heads.clone();
    std::thread::spawn(move || {
        for tcp in listener.incoming().flatten() {
            let config = config.clone();
            let recorded = recorded.clone();
            std::thread::spawn(move || {
                let _ = tcp.set_read_timeout(Some(Duration::from_secs(150)));
                let conn = rustls::ServerConnection::new(config).unwrap();
                let mut tls = rustls::StreamOwned::new(conn, tcp);
                let mut head = Vec::new();
                let mut b = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    match tls.read(&mut b) {
                        Ok(1) => head.push(b[0]),
                        _ => return,
                    }
                }
                recorded
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&head).into_owned());
                let _ = write!(
                    tls,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = tls.flush();
                tls.conn.send_close_notify();
                let _ = tls.flush();
            });
        }
    });
    (addr, heads)
}

/// Answers prompts on a helper thread: `accept` for each, and records them.
fn auto_answer(accept: bool) -> Arc<Mutex<Vec<CertificatePrompt>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    set_prompt_handler(
        move || {
            for p in pending_prompts() {
                seen2.lock().unwrap().push(p.clone());
                let id = p.id;
                std::thread::spawn(move || answer_prompt(id, accept));
            }
        },
        None,
    );
    seen
}

fn get(cfg: &NetConfig, url: &str, connect_to: SocketAddr) -> Result<String, String> {
    let agent = agent_builder_with(cfg, url)
        .resolver(move |_: &str| Ok(vec![connect_to]))
        .timeout(Duration::from_secs(20))
        .build();
    match agent.get(url).call() {
        Ok(r) => Ok(r.into_string().unwrap()),
        Err(e) => Err(e.to_string()),
    }
}

fn config_in(dir: &std::path::Path) -> NetConfig {
    NetConfig {
        certs_dir: Some(dir.join("certs")),
        certificates: CertificateSettings {
            accept_automatically: false,
            use_system_store: false,
        },
        ..Default::default()
    }
}

/// Serializes the test, forgets prompt state from other tests, and configures a
/// fresh certificates folder.
fn setup() -> (
    std::sync::MutexGuard<'static, ()>,
    tempfile::TempDir,
    NetConfig,
) {
    let guard = test_lock();
    clear_prompt_handler();
    crate::verifier::tests_reset();
    let dir = tempfile::tempdir().unwrap();
    let cfg = config_in(dir.path());
    configure(cfg.clone());
    (guard, dir, cfg)
}

#[test]
fn untrusted_certificate_prompt_accept_then_trusted() {
    let (_g, _dir, cfg) = setup();
    let server = tls_server(SELF_SIGNED_PEM, SELF_SIGNED_KEY, "secure hello");
    let url = format!("https://self.test:{}/", server.port());

    // No prompt handler: the certificate is simply rejected.
    clear_prompt_handler();
    let e = get(&cfg, &url, server).unwrap_err();
    assert!(e.contains("UnknownIssuer"), "{e}");

    // The user accepts: the waiting request goes through and the certificate is stored.
    let seen = auto_answer(true);
    assert_eq!(get(&cfg, &url, server).unwrap(), "secure hello");
    let prompts = seen.lock().unwrap().clone();
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].host, "self.test");
    assert!(prompts[0].waiting);
    assert!(
        prompts[0]
            .problem
            .contains("not issued by a trusted certificate authority"),
        "{}",
        prompts[0].problem
    );
    assert_eq!(prompts[0].chain[0].subject.common_name(), Some("localhost"));
    let stored = cfg.cert_store().list();
    assert_eq!(stored.len(), 1);
    assert!(stored[0].note.contains("self.test"), "{}", stored[0].note);

    // Trusted from now on, also after a restart (reconfigure), without prompts.
    clear_prompt_handler();
    configure(cfg.clone());
    assert_eq!(get(&cfg, &url, server).unwrap(), "secure hello");
    // Only for the host it was accepted for: `localhost` (also in the
    // certificate) is not covered.
    assert!(get(
        &cfg,
        &format!("https://localhost:{}/", server.port()),
        server
    )
    .is_err());
    // Removing it brings the error back.
    cfg.cert_store().remove(&stored[0].info.sha256).unwrap();
    assert!(get(&cfg, &url, server).is_err());
    clear_prompt_handler();
}

#[test]
fn rejected_certificate_is_not_asked_again_soon() {
    let (_g, _dir, cfg) = setup();
    let server = tls_server(SELF_SIGNED_PEM, SELF_SIGNED_KEY, "nope");
    let url = format!("https://self.test:{}/", server.port());
    let seen = auto_answer(false);
    assert!(get(&cfg, &url, server).is_err());
    assert!(get(&cfg, &url, server).is_err());
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "the second request fails without asking"
    );
    assert!(cfg.cert_store().list().is_empty());
    clear_prompt_handler();
}

#[test]
fn ui_thread_connections_do_not_wait() {
    let (_g, _dir, cfg) = setup();
    let server = tls_server(SELF_SIGNED_PEM, SELF_SIGNED_KEY, "later");
    let url = format!("https://self.test:{}/", server.port());
    set_prompt_handler(|| {}, Some(std::thread::current().id()));
    let started = std::time::Instant::now();
    assert!(get(&cfg, &url, server).is_err());
    assert!(started.elapsed() < Duration::from_secs(10));
    let pending = pending_prompts();
    assert_eq!(pending.len(), 1);
    assert!(!pending[0].waiting);
    // Accepting afterwards makes the retry work.
    answer_prompt(pending[0].id, true);
    assert!(pending_prompts().is_empty());
    assert_eq!(get(&cfg, &url, server).unwrap(), "later");
    clear_prompt_handler();
}

#[test]
fn accept_automatically_stores_the_certificate() {
    let (_g, _dir, mut cfg) = setup();
    cfg.certificates.accept_automatically = true;
    configure(cfg.clone());
    let server = tls_server(SELF_SIGNED_PEM, SELF_SIGNED_KEY, "auto");
    let url = format!("https://self.test:{}/", server.port());
    assert_eq!(get(&cfg, &url, server).unwrap(), "auto");
    let stored = cfg.cert_store().list();
    assert_eq!(stored.len(), 1);
    assert_eq!(
        (stored[0].hosts.clone(), stored[0].anchor),
        (vec!["self.test".to_string()], false)
    );
}

#[test]
fn a_certificate_accepted_from_a_prompt_is_no_certificate_authority() {
    let (_g, dir, cfg) = setup();
    let server = tls_server(LEAF_PEM, LEAF_KEY, "from leaf");
    let url = format!("https://leaf.test:{}/", server.port());
    // A CA certificate accepted for its own server (as from a prompt) does not
    // vouch for the certificates it issued...
    cfg.cert_store()
        .add(&pem_to_der(CA_PEM), "ca.test")
        .unwrap();
    configure(cfg.clone());
    let e = get(&cfg, &url, server).unwrap_err();
    assert!(e.contains("UnknownIssuer"), "{e}");
    // ...an imported one does.
    let ca_file = dir.path().join("ca.pem");
    std::fs::write(&ca_file, CA_PEM).unwrap();
    cfg.cert_store().import_file(&ca_file).unwrap();
    configure(cfg.clone());
    assert_eq!(get(&cfg, &url, server).unwrap(), "from leaf");
}

#[test]
fn accepted_ca_trusts_its_certificates_but_names_are_checked() {
    let (_g, dir, cfg) = setup();
    // Import the corporate CA, as from the Server Certificates page.
    let ca_file = dir.path().join("corp-ca.pem");
    std::fs::write(&ca_file, CA_PEM).unwrap();
    assert_eq!(cfg.cert_store().import_file(&ca_file).unwrap(), 1);
    configure(cfg.clone());
    let server = tls_server(LEAF_PEM, LEAF_KEY, "from leaf");
    assert_eq!(
        get(
            &cfg,
            &format!("https://leaf.test:{}/", server.port()),
            server
        )
        .unwrap(),
        "from leaf"
    );
    // The same certificate for another name is not trusted; the prompt says why.
    let seen = auto_answer(false);
    let e = get(
        &cfg,
        &format!("https://other.test:{}/", server.port()),
        server,
    )
    .unwrap_err();
    assert!(e.contains("not valid for name"), "{e}");
    let prompts = seen.lock().unwrap().clone();
    assert_eq!(
        prompts[0].problem,
        "The certificate is issued for leaf.test, not for other.test."
    );
    clear_prompt_handler();
}

/// HTTP proxy: CONNECT tunnels and absolute-form requests, optional Basic auth.
/// Every target host is mapped to `target`. Records the request lines.
fn http_proxy(
    target: SocketAddr,
    auth: Option<&'static str>,
) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    http_proxy_routed(move |_| target, auth)
}

/// [`http_proxy`] that picks the server for each request line with `route`.
fn http_proxy_routed(
    route: impl Fn(&str) -> SocketAddr + Send + Sync + 'static,
    auth: Option<&'static str>,
) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    let route = Arc::new(route);
    std::thread::spawn(move || {
        for mut c in listener.incoming().flatten() {
            let seen = seen2.clone();
            let route = route.clone();
            std::thread::spawn(move || {
                let mut head = Vec::new();
                let mut b = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if c.read(&mut b).unwrap_or(0) == 0 {
                        return;
                    }
                    head.push(b[0]);
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                let line = head.lines().next().unwrap_or("").to_string();
                seen.lock().unwrap().push(line.clone());
                if let Some(expected) = auth {
                    let want = format!(
                        "basic {}",
                        base64::engine::general_purpose::STANDARD.encode(expected)
                    );
                    if !head.lines().any(|l| {
                        l.to_ascii_lowercase().starts_with("proxy-authorization:")
                            && l.split_once(':')
                                .unwrap()
                                .1
                                .trim()
                                .eq_ignore_ascii_case(&want)
                    }) {
                        let _ = c.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"t\"\r\nContent-Length: 0\r\n\r\n");
                        return;
                    }
                }
                let Ok(mut upstream) = TcpStream::connect(route(&line)) else {
                    return;
                };
                if line.starts_with("CONNECT ") {
                    let _ = c.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n");
                } else {
                    // Forward the absolute-form request as is (the test server ignores the path).
                    let _ = upstream.write_all(head.as_bytes());
                }
                let mut c2 = c.try_clone().unwrap();
                let mut u2 = upstream.try_clone().unwrap();
                std::thread::spawn(move || {
                    let _ = std::io::copy(&mut c2, &mut u2);
                    let _ = u2.shutdown(std::net::Shutdown::Both);
                });
                let _ = std::io::copy(&mut upstream, &mut c);
                let _ = c.shutdown(std::net::Shutdown::Both);
            });
        }
    });
    (addr, seen)
}

fn manual_proxy(addr: SocketAddr, login: Option<(&str, &str)>) -> ProxySettings {
    ProxySettings {
        mode: ProxyMode::Manual,
        kind: ProxyKind::Http,
        host: "127.0.0.1".into(),
        port: addr.port(),
        authenticate: login.is_some(),
        login: login.map(|l| l.0.to_string()).unwrap_or_default(),
        password: login.map(|l| l.1.to_string()).unwrap_or_default(),
        no_proxy: "direct.test".into(),
        ..Default::default()
    }
}

#[test]
fn https_through_an_authenticating_http_proxy() {
    let (_g, _dir, mut cfg) = setup();
    let server = tls_server(SELF_SIGNED_PEM, SELF_SIGNED_KEY, "tunnelled");
    let (proxy_addr, seen) = http_proxy(server, Some("me:p@ss:word"));
    cfg.proxy = manual_proxy(proxy_addr, Some(("me", "p@ss:word")));
    cfg.cert_store()
        .add(&pem_to_der(SELF_SIGNED_PEM), "self.test")
        .unwrap();
    configure(cfg.clone());
    let url = format!("https://self.test:{}/", server.port());
    // No custom resolver: the proxy resolves the name.
    let agent = agent_builder_with(&cfg, &url)
        .timeout(Duration::from_secs(20))
        .build();
    assert_eq!(
        agent.get(&url).call().unwrap().into_string().unwrap(),
        "tunnelled"
    );
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        &[format!("CONNECT self.test:{} HTTP/1.1", server.port())]
    );
    // Wrong password.
    cfg.proxy.password = "wrong".into();
    let agent = agent_builder_with(&cfg, &url)
        .timeout(Duration::from_secs(20))
        .build();
    let ureq::Error::Transport(t) = agent.get(&url).call().unwrap_err() else {
        panic!("expected a transport error")
    };
    assert!(describe_transport_error(&t).contains("407"), "{t}");
    assert!(check_connection(&cfg, &url).unwrap_err().contains("407"));
}

#[test]
fn plain_http_through_an_authenticating_proxy() {
    let (_g, _dir, _) = setup();
    let web = crate::socks::tests::http_server("plain with auth");
    let (proxy_addr, seen) = http_proxy(web, Some("me:p@ss:word"));
    let mut cfg = NetConfig {
        proxy: manual_proxy(proxy_addr, Some(("me", "p@ss:word"))),
        ..Default::default()
    };
    // ureq alone would send no credentials here: the bridge adds them for the proxy.
    let url = format!("http://far.test:{}/x?q=1", web.port());
    let agent = agent_builder_with(&cfg, &url)
        .timeout(Duration::from_secs(20))
        .build();
    assert_eq!(
        agent.get(&url).call().unwrap().into_string().unwrap(),
        "plain with auth"
    );
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        &[format!("GET http://far.test:{}/x?q=1 HTTP/1.1", web.port())]
    );
    assert!(check_connection(&cfg, &url).unwrap().contains("HTTP 200"));
    // A POST body reaches the server behind the proxy.
    let resp = agent_builder_with(&cfg, &url)
        .build()
        .post(&url)
        .send_string("payload")
        .unwrap();
    assert_eq!(resp.into_string().unwrap(), "plain with auth");
    // Wrong password: the proxy's 407 is reported.
    cfg.proxy.password = "wrong".into();
    let e = check_connection(&cfg, &url).unwrap_err();
    assert!(e.contains("407"), "{e}");
}

#[test]
fn proxy_credentials_never_reach_the_origin_after_a_redirect_to_https() {
    let (_g, _dir, cfg) = setup();
    cfg.cert_store()
        .add(&pem_to_der(SELF_SIGNED_PEM), "self.test")
        .unwrap();
    let (tls, heads) =
        tls_server_recording(SELF_SIGNED_PEM, SELF_SIGNED_KEY, "secure after redirect");
    // http://far.test redirects to https://self.test.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let redirector = listener.local_addr().unwrap();
    let location = format!("https://self.test:{}/landing", tls.port());
    std::thread::spawn(move || {
        for mut c in listener.incoming().flatten() {
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && c.read(&mut b).unwrap_or(0) == 1 {
                head.push(b[0]);
            }
            let _ = write!(c, "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    let (proxy_addr, seen) = http_proxy_routed(
        move |line| {
            if line.contains("self.test") {
                tls
            } else {
                redirector
            }
        },
        Some("me:pw"),
    );
    let mut cfg = cfg;
    cfg.proxy = manual_proxy(proxy_addr, Some(("me", "pw")));
    let url = format!("http://far.test:{}/start", redirector.port());
    let agent = agent_builder_with(&cfg, &url)
        .timeout(Duration::from_secs(20))
        .redirects(5)
        .build();
    assert_eq!(
        agent.get(&url).call().unwrap().into_string().unwrap(),
        "secure after redirect"
    );
    let lines = seen.lock().unwrap().clone();
    assert_eq!(
        lines,
        vec![
            format!("GET http://far.test:{}/start HTTP/1.1", redirector.port()),
            format!("CONNECT self.test:{} HTTP/1.1", tls.port())
        ],
        "both legs went through the authenticating proxy"
    );
    let heads = heads.lock().unwrap().clone();
    assert_eq!(heads.len(), 1);
    assert!(
        heads[0].starts_with("GET /landing HTTP/1.1"),
        "{}",
        heads[0]
    );
    assert!(
        !heads[0]
            .to_ascii_lowercase()
            .contains("proxy-authorization"),
        "{}",
        heads[0]
    );
}

#[test]
fn plain_http_through_proxy_and_no_proxy_list() {
    let (_g, _dir, _) = setup();
    let web = crate::socks::tests::http_server("plain");
    let (proxy_addr, seen) = http_proxy(web, None);
    let cfg = NetConfig {
        proxy: manual_proxy(proxy_addr, None),
        ..Default::default()
    };
    let url = format!("http://far.test:{}/x", web.port());
    let agent = agent_builder_with(&cfg, &url)
        .timeout(Duration::from_secs(20))
        .build();
    assert_eq!(
        agent.get(&url).call().unwrap().into_string().unwrap(),
        "plain"
    );
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        &[format!("GET http://far.test:{}/x HTTP/1.1", web.port())]
    );
    // "No proxy for" hosts connect directly.
    let direct = format!("http://direct.test:{}/", web.port());
    let agent = agent_builder_with(&cfg, &direct)
        .resolver(move |_: &str| Ok(vec![web]))
        .build();
    assert_eq!(
        agent.get(&direct).call().unwrap().into_string().unwrap(),
        "plain"
    );
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "the proxy saw no second request"
    );
    let msg = check_connection(&cfg, &url).unwrap();
    assert!(
        msg.contains("HTTP 200")
            && msg.contains(&format!("HTTP proxy 127.0.0.1:{}", proxy_addr.port())),
        "{msg}"
    );
}

#[test]
fn socks_proxy_from_settings() {
    let (_g, _dir, _) = setup();
    let (socks_addr, seen) = crate::socks::tests::socks5_server(Some(("u", "pw")));
    let web = crate::socks::tests::http_server("via socks");
    let cfg = NetConfig {
        proxy: ProxySettings {
            kind: ProxyKind::Socks,
            ..manual_proxy(socks_addr, Some(("u", "pw")))
        },
        ..Default::default()
    };
    // Only the (test) SOCKS server can resolve `*.test` names.
    let url = format!("http://far.test:{}/", web.port());
    let msg = check_connection(&cfg, &url).unwrap();
    assert!(
        msg.contains(&format!("SOCKS5 proxy 127.0.0.1:{}", socks_addr.port()))
            && msg.contains("HTTP 200"),
        "{msg}"
    );
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        &[format!("far.test:{}", web.port())]
    );
    // Loopback stays direct.
    let local = format!("http://127.0.0.1:{}/", web.port());
    assert!(check_connection(&cfg, &local)
        .unwrap()
        .contains("direct connection"));
    assert_eq!(seen.lock().unwrap().len(), 1);
}

/// An HTTP proxy that answers absolute-form requests itself (as if it were
/// the server) with `ok`, and everything else with 407. With `keep_alive` it
/// serves further requests on a connection and ignores `Connection: close`;
/// without, it closes after one response but does not say so in it. Records
/// `"<request line> creds=<none|ok|bad>"`.
fn answering_proxy(auth: &'static str, keep_alive: bool) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    let want = format!(
        "basic {}",
        base64::engine::general_purpose::STANDARD.encode(auth)
    );
    std::thread::spawn(move || {
        for mut c in listener.incoming().flatten() {
            let (seen, want) = (seen2.clone(), want.clone());
            std::thread::spawn(move || loop {
                let mut head = Vec::new();
                let mut b = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if c.read(&mut b).unwrap_or(0) == 0 {
                        return;
                    }
                    head.push(b[0]);
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                let header = |name: &str| {
                    head.lines().find_map(|l| {
                        l.split_once(':')
                            .filter(|(n, _)| n.trim().eq_ignore_ascii_case(name))
                            .map(|(_, v)| v.trim().to_string())
                    })
                };
                let length: usize = header("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                let mut body = vec![0u8; length];
                if c.read_exact(&mut body).is_err() {
                    return;
                }
                let creds = match header("proxy-authorization") {
                    None => "none",
                    Some(v) if v.eq_ignore_ascii_case(&want) => "ok",
                    Some(_) => "bad",
                };
                let line = head.lines().next().unwrap_or("").to_string();
                seen.lock().unwrap().push(format!("{line} creds={creds}"));
                let answer: &[u8] = if creds == "ok" && line.contains(" http://") {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"
                } else {
                    b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"t\"\r\nContent-Length: 0\r\n\r\n"
                };
                if c.write_all(answer).is_err() {
                    return;
                }
                if !keep_alive {
                    let _ = c.shutdown(std::net::Shutdown::Both);
                    return;
                }
            });
        }
    });
    (addr, seen)
}

#[test]
fn no_proxy_credentials_while_the_password_is_unknown() {
    let (_g, _dir, _) = setup();
    let (proxy_addr, seen) = answering_proxy("CORP\\alice:pw", false);
    // Authentication with a login, but the remembered password is not loaded
    // (or the database is locked, or it was not remembered).
    let cfg = NetConfig {
        proxy: manual_proxy(proxy_addr, Some(("CORP\\alice", ""))),
        ..Default::default()
    };
    // HTTPS: ureq's own CONNECT; plain HTTP: absolute form.
    for url in ["https://pypi.org/simple/", "http://far.test/x"] {
        let e = check_connection(&cfg, url).unwrap_err();
        assert!(e.contains("no proxy password is known"), "{url}: {e}");
    }
    let lines = seen.lock().unwrap().clone();
    assert_eq!(
        lines,
        vec![
            "CONNECT pypi.org:443 HTTP/1.1 creds=none".to_string(),
            "GET http://far.test/x HTTP/1.1 creds=none".to_string()
        ],
        "no `login:` with an empty password"
    );
    // With the password, the same requests authenticate.
    let cfg = NetConfig {
        proxy: manual_proxy(proxy_addr, Some(("CORP\\alice", "pw"))),
        ..Default::default()
    };
    assert!(check_connection(&cfg, "http://far.test/x")
        .unwrap()
        .contains("HTTP 200"));
    assert_eq!(
        seen.lock().unwrap().last().unwrap(),
        "GET http://far.test/x HTTP/1.1 creds=ok"
    );
}

#[test]
fn bridged_connections_are_not_reused_by_a_keep_alive_proxy() {
    let (_g, _dir, _) = setup();
    // The proxy keeps connections open although it was asked to close them.
    let (proxy_addr, seen) = answering_proxy("me:pw", true);
    let cfg = NetConfig {
        proxy: manual_proxy(proxy_addr, Some(("me", "pw"))),
        ..Default::default()
    };
    let agent = agent_builder_with(&cfg, "http://far.test/a")
        .timeout(Duration::from_secs(20))
        .build();
    for path in ["a", "b", "c"] {
        let resp = agent
            .get(&format!("http://far.test/{path}"))
            .call()
            .unwrap_or_else(|e| panic!("/{path}: {e}"));
        assert_eq!(resp.into_string().unwrap(), "ok");
    }
    let lines = seen.lock().unwrap().clone();
    assert_eq!(
        lines,
        ["a", "b", "c"]
            .map(|p| format!("GET http://far.test/{p} HTTP/1.1 creds=ok"))
            .to_vec(),
        "every request rewritten, with credentials"
    );
}

#[test]
fn bridged_posts_never_meet_a_connection_the_proxy_closed() {
    let (_g, _dir, _) = setup();
    // The proxy closes after each response without saying `Connection: close`.
    let (proxy_addr, seen) = answering_proxy("me:pw", false);
    let cfg = NetConfig {
        proxy: manual_proxy(proxy_addr, Some(("me", "pw"))),
        ..Default::default()
    };
    let agent = agent_builder_with(&cfg, "http://far.test/")
        .timeout(Duration::from_secs(20))
        .build();
    for i in 0..20 {
        let url = format!("http://far.test/{i}");
        let resp = if i % 2 == 0 {
            agent.get(&url).call()
        } else {
            agent.post(&url).send_string("payload")
        };
        assert_eq!(
            resp.unwrap_or_else(|e| panic!("request {i}: {e}"))
                .into_string()
                .unwrap(),
            "ok"
        );
    }
    assert_eq!(seen.lock().unwrap().len(), 20);
}

/// A keep-alive HTTP proxy that answers every request with an interim
/// `interim` response before the final `200 OK` (`ok`): in the same write when
/// `together`, else in two. Counts the requests.
fn interim_proxy(interim: &'static str, together: bool) -> (SocketAddr, Arc<Mutex<usize>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let count = Arc::new(Mutex::new(0usize));
    let count2 = count.clone();
    std::thread::spawn(move || {
        for mut c in listener.incoming().flatten() {
            let count = count2.clone();
            std::thread::spawn(move || loop {
                let mut head = Vec::new();
                let mut b = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if c.read(&mut b).unwrap_or(0) == 0 {
                        return;
                    }
                    head.push(b[0]);
                }
                *count.lock().unwrap() += 1;
                let final_response = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
                let written = if together {
                    c.write_all(format!("{interim}{final_response}").as_bytes())
                } else {
                    c.write_all(interim.as_bytes()).and_then(|_| {
                        c.flush()?;
                        std::thread::sleep(Duration::from_millis(50));
                        c.write_all(final_response.as_bytes())
                    })
                };
                if written.is_err() {
                    return;
                }
            });
        }
    });
    (addr, count)
}

#[test]
fn interim_responses_through_the_bridge() {
    let (_g, _dir, _) = setup();
    // ureq knows no 1xx responses: it would take one for the final response, and
    // read the real one as its body until the (keep-alive) proxy closes.
    for (interim, together) in [
        (
            "HTTP/1.1 103 Early Hints\r\nLink: </s.css>; rel=preload\r\n\r\n",
            true,
        ),
        ("HTTP/1.1 100 Continue\r\n\r\n", false),
        (
            "HTTP/1.1 102 Processing\r\n\r\nHTTP/1.1 103 Early Hints\r\n\r\n",
            true,
        ),
    ] {
        let (proxy_addr, count) = interim_proxy(interim, together);
        let cfg = NetConfig {
            proxy: manual_proxy(proxy_addr, Some(("me", "pw"))),
            ..Default::default()
        };
        let agent = agent_builder_with(&cfg, "http://far.test/")
            .timeout(Duration::from_secs(10))
            .build();
        for path in ["a", "b"] {
            let resp = agent
                .get(&format!("http://far.test/{path}"))
                .call()
                .unwrap_or_else(|e| panic!("{interim:?} /{path}: {e}"));
            assert_eq!(resp.status(), 200, "{interim:?}");
            assert_eq!(resp.header("connection"), Some("close"), "{interim:?}");
            assert_eq!(resp.into_string().unwrap(), "ok", "{interim:?}");
        }
        assert_eq!(*count.lock().unwrap(), 2, "{interim:?}");
    }
}

#[test]
fn socks_proxy_without_a_known_password() {
    let (_g, _dir, _) = setup();
    let (socks_addr, seen) = crate::socks::tests::socks5_server(Some(("u", "pw")));
    // Authentication with a login, but the password is not known in this session.
    let cfg = NetConfig {
        proxy: ProxySettings {
            kind: ProxyKind::Socks,
            ..manual_proxy(socks_addr, Some(("u", "")))
        },
        ..Default::default()
    };
    let e = check_connection(&cfg, "http://far.test/").unwrap_err();
    assert!(e.contains("no proxy password is known"), "{e}");
    assert!(seen.lock().unwrap().is_empty());
    // A wrong password is reported as such.
    let cfg = NetConfig {
        proxy: ProxySettings {
            kind: ProxyKind::Socks,
            ..manual_proxy(socks_addr, Some(("u", "wrong")))
        },
        ..Default::default()
    };
    let e = check_connection(&cfg, "http://far.test/").unwrap_err();
    assert!(e.contains("authentication failed"), "{e}");
}

#[test]
fn the_bridge_forwards_no_request_for_another_host() {
    let (_g, _dir, _) = setup();
    let (proxy_addr, seen) = answering_proxy("me:pw", false);
    let proxy = ProxyServer {
        host: "127.0.0.1".into(),
        port: proxy_addr.port(),
        user: Some("me".into()),
        password: Some("pw".into()),
    };
    // A program that connects to the bridge before ureq does and asks for its own target.
    let bridge =
        ureq::Resolver::resolve(&http_bridge::HttpProxyResolver::new(proxy), "far.test:80")
            .unwrap()[0];
    let mut c = TcpStream::connect(bridge).unwrap();
    c.write_all(b"GET http://attacker.example/steal HTTP/1.1\r\nHost: attacker.example\r\n\r\n")
        .unwrap();
    let mut answer = Vec::new();
    let _ = c.read_to_end(&mut answer);
    assert!(answer.is_empty(), "{}", String::from_utf8_lossy(&answer));
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        seen.lock().unwrap().is_empty(),
        "the proxy got {:?}",
        seen.lock().unwrap()
    );
}

#[test]
fn check_connection_reports_bad_urls_and_refused_connections() {
    let (_g, _dir, _) = setup();
    let cfg = NetConfig::default();
    assert!(check_connection(&cfg, "http://")
        .unwrap_err()
        .contains("Invalid URL"));
    // A closed port on loopback.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let e = check_connection(&cfg, &format!("http://127.0.0.1:{port}/")).unwrap_err();
    assert!(e.contains("direct connection"), "{e}");
}
