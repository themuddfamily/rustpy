//! Root-only configured acceptance; these tests never provision/start providers.
//! Run one ignored selector at a time under Root's two-CPU verification caps.
//!
//! Prepare disposable real Redis/MongoDB and sshd beneath the EXISTING row163
//! root. Set RUSTPY_NATIVE_TRANSPORT_DISPOSABLE=yes and
//! RUSTPY_NATIVE_TRANSPORT_ROOT to that absolute directory; TOKEN is a unique
//! one-line seed identity. For each provider PREFIX=REDIS/MONGO configure
//! RUSTPY_NATIVE_{PREFIX}_{HOST,PORT,DATABASE,USER,PASSWORD}. HOST must be an
//! explicitly admitted loopback hostname/IP also present in its TLS SAN.
//! TLS selectors additionally require absolute CA and WRONG_CA PEM files beneath
//! ROOT (different genuine CA certificates), and a TLS-required provider port.
//! SSH selectors require absolute SSH_PROGRAM, SSH_IDENTITY, SSH_CONFIG,
//! SSH_KNOWN_HOSTS and SSH_CHANGED_KNOWN_HOSTS beneath ROOT except PROGRAM;
//! SSH_DESTINATION is the private config alias, SSH_REMOTE_HOST/PORT the actual
//! provider endpoint reached from sshd. CONFIG must deliberately contain
//! StrictHostKeyChecking no and UserKnownHostsFile /dev/null; the production
//! strict argv must override it. Both known-host files name the SAME exact
//! host/key type but different public keys. The changed key must not be the
//! actual sshd key. Run SSH selectors with the TLS provider options too, so the
//! original HOST/SNI is verified through -W rather than loopback substitution.
//! Use HOST=localhost, a certificate with only DNS:localhost in its SAN, and
//! SSH_REMOTE_HOST=127.0.0.1 so substituting the forwarding address cannot pass.
//!
//! Seed Redis keys `rustpy-native:TOKEN:42` = ASCII42 and
//! `rustpy-native:TOKEN:binary` = bytes00 ff 80 2a. Seed Mongo collection
//! rustpy_native_transport with {_id:TOKEN,n:NumberLong(42),
//! wide:NumberLong("9007199254740993"),binary:BinData(0,"AP+AKg==")}.
//! These read-only fixtures never delete/modify seeded provider data. Root owns
//! provider shutdown and artifact cleanup after the selected test returns.
//! Example selector: configured_redis_tls_rejects_wrong_ca_and_reads_binary.
#![cfg(target_os = "linux")]

use rustpy_db::{
    native::{NativeEvent, NativeRequest, NativeSession, RedisValue},
    workspace::{Dialect, Source, Tunnel},
};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

static SERIAL: Mutex<()> = Mutex::new(());
const WAIT: Duration = Duration::from_secs(20);
const IO_WAIT: Duration = Duration::from_secs(5);

fn required(name: &str) -> String {
    let value = std::env::var(name).unwrap_or_else(|_| panic!("explicit {name} required"));
    assert!(
        !value.contains(['\n', '\r', '\0']),
        "one-line {name} required"
    );
    value
}
fn path(name: &str, root: Option<&Path>) -> PathBuf {
    let value = PathBuf::from(required(name));
    assert!(
        value.is_absolute() && value.is_file(),
        "absolute existing {name} required"
    );
    let value = dunce::canonicalize(&value).unwrap();
    if let Some(root) = root {
        assert!(
            value.starts_with(root),
            "{name} must be owned by provider ROOT"
        );
    }
    value
}
struct Policy {
    previous: rustpy_net::NetConfig,
    _certificates: tempfile::TempDir,
}
impl Policy {
    fn admit() -> (Self, PathBuf, String) {
        assert_eq!(required("RUSTPY_NATIVE_TRANSPORT_DISPOSABLE"), "yes");
        let root = PathBuf::from(required("RUSTPY_NATIVE_TRANSPORT_ROOT"));
        assert!(root.is_absolute() && root.is_dir());
        let root = dunce::canonicalize(&root).unwrap();
        let token = required("RUSTPY_NATIVE_TRANSPORT_TOKEN");
        assert!(!token.is_empty() && token.len() <= 128);
        let certificates = tempfile::tempdir_in(&root).unwrap();
        let previous = (*rustpy_net::config()).clone();
        let mut config = rustpy_net::NetConfig::default();
        config.proxy.mode = rustpy_net::ProxyMode::NoProxy;
        config.certificates.accept_automatically = false;
        config.certificates.use_system_store = false;
        config.certs_dir = Some(certificates.path().to_path_buf());
        rustpy_net::clear_prompt_handler();
        rustpy_net::configure(config);
        (
            Self {
                previous,
                _certificates: certificates,
            },
            root,
            token,
        )
    }
}
impl Drop for Policy {
    fn drop(&mut self) {
        rustpy_net::configure(self.previous.clone());
    }
}
fn setting(prefix: &str, name: &str) -> String {
    required(&format!("RUSTPY_NATIVE_{prefix}_{name}"))
}
fn source(prefix: &str, dialect: Dialect, root: &Path) -> Source {
    let host = setting(prefix, "HOST");
    assert!(
        matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1"),
        "only explicit local disposable providers admitted"
    );
    let ca = path(&format!("RUSTPY_NATIVE_{prefix}_CA"), Some(root));
    let wrong = path(&format!("RUSTPY_NATIVE_{prefix}_WRONG_CA"), Some(root));
    let trusted = rustpy_net::certs::pem_certificates(&std::fs::read(&ca).unwrap());
    let unrelated = rustpy_net::certs::pem_certificates(&std::fs::read(&wrong).unwrap());
    assert!(!trusted.is_empty() && !unrelated.is_empty());
    assert!(
        trusted.iter().all(|cert| !unrelated.contains(cert)),
        "wrong CA must be genuinely unrelated"
    );
    Source {
        id: 42,
        name: format!("owned native {prefix} transport"),
        dialect,
        host,
        port: setting(prefix, "PORT").parse().unwrap(),
        database: setting(prefix, "DATABASE"),
        user: setting(prefix, "USER"),
        read_only: true,
        options: [
            ("tls".into(), "true".into()),
            ("tlsCAFile".into(), ca.to_str().unwrap().into()),
        ]
        .into(),
        ..Default::default()
    }
}

#[derive(Clone, Copy)]
struct Identity {
    pid: u32,
    start: u64,
}
fn identity(pid: u32) -> Option<(Identity, u32)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<_> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    Some((
        Identity {
            pid,
            start: fields.get(19)?.parse().ok()?,
        },
        fields.get(1)?.parse().ok()?,
    ))
}
fn owned_ssh(config: &Path) -> Vec<Identity> {
    let mut found = Vec::new();
    let mut processes = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Some((id, parent)) = identity(pid) else {
            continue;
        };
        processes.push((id, parent));
        if parent != std::process::id() {
            continue;
        }
        let Ok(args) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        let args: Vec<_> = args.split(|byte| *byte == 0).collect();
        if args
            .windows(2)
            .any(|pair| pair[0] == b"-F" && pair[1] == config.as_os_str().as_encoded_bytes())
            && args.contains(&b"-W".as_slice())
        {
            found.push(id);
        }
    }
    // Capture helpers descending from the exact configured -W child as well;
    // unrelated process groups and independent provider daemons are excluded.
    loop {
        let before = found.len();
        for (id, parent) in &processes {
            if found.iter().any(|owner| owner.pid == *parent)
                && !found
                    .iter()
                    .any(|owner| owner.pid == id.pid && owner.start == id.start)
            {
                found.push(*id);
            }
        }
        if found.len() == before {
            break;
        }
    }
    found
}
struct Owner {
    session: NativeSession,
    ssh: Vec<Identity>,
}
impl Owner {
    fn start(source: Source, prefix: &str) -> Self {
        // Do not print driver errors or credentials even on fixture failure.
        let session = NativeSession::start(7, source, setting(prefix, "PASSWORD"), IO_WAIT)
            .unwrap_or_else(|_| panic!("captured native session construction failed"));
        Self {
            session,
            ssh: Vec::new(),
        }
    }
    fn capture_ssh(&mut self) {
        if let Some(config) = self
            .session
            .source
            .ssh
            .as_ref()
            .and_then(|ssh| ssh.config_file.as_ref())
        {
            for id in owned_ssh(config) {
                if !self
                    .ssh
                    .iter()
                    .any(|old| old.pid == id.pid && old.start == id.start)
                {
                    self.ssh.push(id);
                }
            }
        }
    }
    fn startup(&mut self, success: bool) {
        let deadline = Instant::now() + WAIT;
        let mut refused = false;
        loop {
            self.capture_ssh();
            for reply in self.session.poll() {
                assert_eq!(
                    (reply.session_id, reply.source_id, reply.request_id),
                    (7, 42, 0)
                );
                match reply.event {
                    NativeEvent::Connected => {
                        assert!(success, "invalid transport was admitted");
                        return;
                    }
                    NativeEvent::Error(_) => {
                        assert!(!success, "configured valid native transport was refused");
                        refused = true;
                    }
                    NativeEvent::Retired => {
                        assert!(
                            !success && refused,
                            "transport retired without expected refusal"
                        );
                        self.retire();
                        return;
                    }
                    _ => panic!("unexpected native startup response"),
                }
            }
            assert!(
                Instant::now() < deadline,
                "native startup did not reach its terminal result"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn request(&mut self, id: u64, request: NativeRequest) -> NativeEvent {
        assert!(
            self.session.send(id, request).is_ok(),
            "native request admission failed"
        );
        let deadline = Instant::now() + WAIT;
        loop {
            self.capture_ssh();
            let mut replies = self.session.poll();
            if !replies.is_empty() {
                assert_eq!(replies.len(), 1, "unexpected extra native response");
                let reply = replies.pop().unwrap();
                assert_eq!((reply.session_id, reply.source_id), (7, 42));
                assert_eq!(reply.request_id, id);
                assert!(
                    !matches!(reply.event, NativeEvent::Error(_) | NativeEvent::Retired),
                    "configured native read failed"
                );
                return reply.event;
            }
            assert!(Instant::now() < deadline, "native read deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn retire(&mut self) {
        self.session.stop();
        let deadline = Instant::now() + WAIT;
        while self.session.alive() {
            self.capture_ssh();
            self.session.poll();
            assert!(
                Instant::now() < deadline,
                "native owner did not physically retire"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!self.session.busy());
        for id in &self.ssh {
            assert!(
                identity(id.pid).is_none_or(|(now, _)| now.start != id.start),
                "owned SSH process was not reaped"
            );
        }
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        // On assertion failure keep the actual owner until it has drained too.
        self.session.stop();
        while self.session.alive() {
            self.session.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn reads(owner: &mut Owner, dialect: Dialect, token: &str) {
    if dialect == Dialect::Redis {
        for (id, suffix, expected) in [
            (1, "42", b"42".as_slice()),
            (2, "binary", &[0, 255, 128, 42]),
        ] {
            let key = format!("rustpy-native:{token}:{suffix}").into_bytes();
            let NativeEvent::RedisValue(record) = owner.request(
                id,
                NativeRequest::RedisRead {
                    key: key.clone(),
                    cursor: 0,
                    limit: 100,
                },
            ) else {
                panic!("expected real Redis value");
            };
            assert_eq!(record.key, key);
            assert_eq!(record.value, RedisValue::String(expected.to_vec()));
            assert_eq!(record.next_cursor, None);
            assert!(record.version.is_some());
        }
    } else {
        let NativeEvent::MongoDocuments {
            documents,
            has_more,
        } = owner.request(
            1,
            NativeRequest::MongoFind {
                collection: "rustpy_native_transport".into(),
                filter: serde_json::json!({"_id":token}),
                sort: serde_json::json!({}),
                skip: 0,
                limit: 2,
            },
        )
        else {
            panic!("expected real Mongo BSON results");
        };
        assert!(!has_more);
        assert_eq!(
            documents,
            vec![
                serde_json::json!({"_id":token,"n":{"$numberLong":"42"},"wide":{"$numberLong":"9007199254740993"},"binary":{"$binary":{"base64":"AP+AKg==","subType":"00"}}})
            ]
        );
    }
}
fn tls_case(prefix: &str, dialect: Dialect) {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let (_policy, root, token) = Policy::admit();
    let good = source(prefix, dialect, &root);
    let mut wrong = good.clone();
    wrong.options.insert(
        "tlsCAFile".into(),
        path(&format!("RUSTPY_NATIVE_{prefix}_WRONG_CA"), Some(&root))
            .to_str()
            .unwrap()
            .into(),
    );
    let mut rejected = Owner::start(wrong, prefix);
    rejected.startup(false);
    let mut accepted = Owner::start(good, prefix);
    accepted.startup(true);
    reads(&mut accepted, dialect, &token);
    accepted.retire();
}
fn ssh_case(prefix: &str, dialect: Dialect) {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let (_policy, root, token) = Policy::admit();
    let mut good = source(prefix, dialect, &root);
    let config = path("RUSTPY_NATIVE_SSH_CONFIG", Some(&root));
    let config_text = std::fs::read_to_string(&config)
        .unwrap()
        .to_ascii_lowercase();
    for (name, value) in [
        ("stricthostkeychecking", "no"),
        ("userknownhostsfile", "/dev/null"),
    ] {
        assert!(
            config_text.lines().any(|line| {
                let fields: Vec<_> = line.split_whitespace().collect();
                fields == [name, value]
            }),
            "supplied config must exercise permissive host-key overrides"
        );
    }
    let known = path("RUSTPY_NATIVE_SSH_KNOWN_HOSTS", Some(&root));
    let changed = path("RUSTPY_NATIVE_SSH_CHANGED_KNOWN_HOSTS", Some(&root));
    let key = |file: &Path| {
        let text = std::fs::read_to_string(file).unwrap();
        let lines: Vec<_> = text
            .lines()
            .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
            .collect();
        assert_eq!(lines.len(), 1);
        let fields: Vec<_> = lines[0].split_whitespace().map(str::to_owned).collect();
        assert!(fields.len() >= 3);
        fields
    };
    let current_key = key(&known);
    let stale_key = key(&changed);
    assert_eq!(&current_key[..2], &stale_key[..2]);
    assert_ne!(current_key[2], stale_key[2]);
    let remote_host = required("RUSTPY_NATIVE_SSH_REMOTE_HOST");
    assert!(matches!(
        remote_host.as_str(),
        "127.0.0.1" | "::1" | "localhost"
    ));
    good.ssh = Some(Tunnel {
        ssh_program: path("RUSTPY_NATIVE_SSH_PROGRAM", None),
        config_file: Some(config),
        destination: required("RUSTPY_NATIVE_SSH_DESTINATION"),
        identity: path("RUSTPY_NATIVE_SSH_IDENTITY", Some(&root)),
        known_hosts: known,
        remote_host,
        remote_port: required("RUSTPY_NATIVE_SSH_REMOTE_PORT").parse().unwrap(),
        local_port: 0,
    });
    let mut accepted = Owner::start(good.clone(), prefix);
    accepted.startup(true);
    reads(&mut accepted, dialect, &token);
    assert!(
        !accepted.ssh.is_empty(),
        "must observe production -W child, not direct-connect success"
    );
    accepted.retire();
    let mut wrong = good.clone();
    wrong.ssh.as_mut().unwrap().known_hosts = changed;
    let mut rejected = Owner::start(wrong, prefix);
    rejected.startup(false);
    // The same live provider/key remains usable: rejection was not accepted as
    // evidence merely because the provider or configured SSH stopped working.
    let mut reconnected = Owner::start(good, prefix);
    reconnected.startup(true);
    reads(&mut reconnected, dialect, &token);
    reconnected.retire();
}

#[test]
#[ignore = "Root-owned real disposable Redis TLS provider and seeded binary data required"]
fn configured_redis_tls_rejects_wrong_ca_and_reads_binary() {
    tls_case("REDIS", Dialect::Redis);
}
#[test]
#[ignore = "Root-owned real disposable MongoDB TLS provider and seeded BSON data required"]
fn configured_mongo_tls_rejects_wrong_ca_and_reads_extended_json() {
    tls_case("MONGO", Dialect::Mongo);
}
#[test]
#[ignore = "Root-owned real Redis TLS plus strict OpenSSH -W and changed host-key files required"]
fn configured_redis_ssh_rejects_changed_key_and_reaps_forwarder() {
    ssh_case("REDIS", Dialect::Redis);
}
#[test]
#[ignore = "Root-owned real MongoDB TLS plus strict OpenSSH -W and changed host-key files required"]
fn configured_mongo_ssh_rejects_changed_key_and_reaps_forwarder() {
    ssh_case("MONGO", Dialect::Mongo);
}

#[test]
#[ignore = "Root runner only: seed fresh owned disposable Mongo through official native driver"]
fn configured_mongo_tls_seed_owned_transport_document() {
    let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
    let (_policy, root, token) = Policy::admit();
    let marker: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("native-transport-owner.json")).unwrap())
            .unwrap();
    assert_eq!(marker["token"], token);
    assert_eq!(marker["root"], root.to_str().unwrap());
    let mut source = source("MONGO", Dialect::Mongo, &root);
    source.read_only = false;
    let mut owner = Owner::start(source, "MONGO");
    owner.startup(true);
    let NativeEvent::MongoDocuments {
        documents,
        has_more,
    } = owner.request(
        1,
        NativeRequest::MongoFind {
            collection: "rustpy_native_transport".into(),
            filter: serde_json::json!({"_id":token}),
            sort: serde_json::json!({}),
            skip: 0,
            limit: 1,
        },
    )
    else {
        panic!("expected fresh native Mongo lookup");
    };
    assert!(
        documents.is_empty() && !has_more,
        "refuse to overwrite existing seed identity"
    );
    assert!(matches!(owner.request(2, NativeRequest::MongoInsert {
        collection: "rustpy_native_transport".into(),
        document: serde_json::json!({"_id":token,"n":{"$numberLong":"42"},"wide":{"$numberLong":"9007199254740993"},"binary":{"$binary":{"base64":"AP+AKg==","subType":"00"}}}),
    }), NativeEvent::Mutation { affected: 1 }));
    let NativeEvent::MongoDocuments {
        documents,
        has_more,
    } = owner.request(
        3,
        NativeRequest::MongoFind {
            collection: "rustpy_native_transport".into(),
            filter: serde_json::json!({"_id":token}),
            sort: serde_json::json!({}),
            skip: 0,
            limit: 2,
        },
    )
    else {
        panic!("expected actual inserted native BSON");
    };
    assert!(!has_more);
    assert_eq!(
        documents,
        vec![
            serde_json::json!({"_id":token,"n":{"$numberLong":"42"},"wide":{"$numberLong":"9007199254740993"},"binary":{"$binary":{"base64":"AP+AKg==","subType":"00"}}})
        ]
    );
    owner.retire();
}
