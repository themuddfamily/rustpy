//! Explicit real local PostgreSQL + OpenSSH acceptance, never an existing server.
//! Root runs this selector alone under its affinity/priority envelope. All
//! provisioned data/keys stay beneath RUSTPY_PG_PROVIDER_ROOT; no accounts or
//! permissions on that root's ancestors are changed.
use rustpy_core::{process_tree::ProcessTree, CancellationToken};
use rustpy_db::{
    session::{Reply, Request, Session},
    workspace::{Change, Dialect, Settings, Source, Tunnel},
};
use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const WAIT: Duration = Duration::from_secs(20);
fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("explicit {name} required"))
}
fn executable(name: &str) -> PathBuf {
    let path = PathBuf::from(env(name));
    assert!(
        path.is_absolute() && path.is_file(),
        "{name} requires a real absolute executable"
    );
    path
}
fn secret() -> String {
    let value = env("RUSTPY_TEST_POSTGRES_PASSWORD");
    assert!(
        !value.is_empty() && !value.contains(['\n', '\r', '\0']),
        "one-line session password required"
    );
    value
}
fn redacted(error: impl std::fmt::Display) -> String {
    let secret = secret();
    let escaped = secret.replace('}', "}}");
    error
        .to_string()
        .replace(&format!("{{{escaped}}}"), "[redacted]")
        .replace(&escaped, "[redacted]")
        .replace(&secret, "[redacted]")
}
fn output(mut command: Command) -> std::process::Output {
    let token = CancellationToken::new();
    let cancel = token.clone();
    let (finished, rx) = std::sync::mpsc::channel();
    let watchdog = std::thread::spawn(move || {
        if rx.recv_timeout(Duration::from_secs(30)).is_err() {
            cancel.cancel();
        }
    });
    let result = rustpy_core::process::output_cancellable(&mut command, &token);
    let _ = finished.send(());
    watchdog.join().unwrap();
    let result = result.unwrap_or_else(|e| panic!("admitted provisioning failed: {}", redacted(e)));
    assert!(
        result.status.success(),
        "admitted provisioning failed: {}",
        redacted(String::from_utf8_lossy(&result.stderr))
    );
    result
}
#[derive(Clone, Copy, Debug)]
struct Identity {
    pid: u32,
    start: u64,
}
fn identity(pid: u32) -> Option<Identity> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    Some(Identity {
        pid,
        start: stat
            .rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(19)?
            .parse()
            .ok()?,
    })
}
fn gone(value: Identity) -> bool {
    identity(value.pid).is_none_or(|now| now.start != value.start)
}
fn retired(values: &[Identity]) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while values.iter().any(|value| !gone(*value)) {
        assert!(
            Instant::now() < deadline,
            "captured owned provider processes did not physically retire: {:?}",
            values.iter().filter(|v| !gone(**v)).collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
// Capture children while the owned listener is alive; it remains responsible for
// reaping them after the forwarding client disconnects. Never signal these PIDs.
fn daemon_children(parent: Identity) -> Vec<Identity> {
    assert!(
        !gone(parent),
        "captured listener must still be the same process"
    );
    let children =
        std::fs::read_to_string(format!("/proc/{}/task/{}/children", parent.pid, parent.pid))
            .unwrap();
    children
        .split_whitespace()
        .filter_map(|pid| identity(pid.parse().unwrap()))
        .collect()
}
fn conninfo_path(path: &Path) -> String {
    let path = path.to_str().expect("admitted libpq path requires UTF-8");
    assert!(!path.chars().any(char::is_control));
    format!("'{}'", path.replace('\\', "\\\\").replace('\'', "\\'"))
}
fn ssh_config_path(path: &Path) -> String {
    let path = path.to_str().expect("admitted OpenSSH path requires UTF-8");
    assert!(!path.chars().any(char::is_control));
    format!("\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\""))
}
fn tunnel_children(program: &Path, config: &Path) -> Vec<Identity> {
    let program = std::fs::canonicalize(program).unwrap();
    let expected = config.to_string_lossy();
    let mut result = Vec::new();
    for task in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
        let Ok(children) = std::fs::read_to_string(task.path().join("children")) else {
            continue;
        };
        for pid in children
            .split_whitespace()
            .filter_map(|s| s.parse::<u32>().ok())
        {
            if std::fs::read_link(format!("/proc/{pid}/exe"))
                .ok()
                .as_deref()
                != Some(program.as_path())
            {
                continue;
            }
            let Ok(args) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
                continue;
            };
            if !args
                .split(|b| *b == 0)
                .any(|arg| arg == expected.as_bytes())
            {
                continue;
            }
            if let Some(value) = identity(pid) {
                result.push(value);
            }
        }
    }
    result
}
struct Daemon {
    child: Child,
    tree: ProcessTree,
    captured: Identity,
    stopped: bool,
}
impl Daemon {
    fn start(mut command: Command, log: &Path) -> Self {
        command
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(log).unwrap())
            .stderr(std::fs::File::options().append(true).open(log).unwrap());
        ProcessTree::prepare(&mut command);
        let mut child = command.spawn().expect("admitted daemon spawn");
        let tree = ProcessTree::new(&mut child).unwrap_or_else(|e| {
            let _ = child.kill();
            let _ = child.wait();
            panic!("daemon owner attach: {e}")
        });
        let captured = identity(child.id()).expect("actual daemon /proc identity");
        eprintln!(
            "owned provider {:?}; private log {}",
            captured,
            log.display()
        );
        Self {
            child,
            tree,
            captured,
            stopped: false,
        }
    }
    fn ready(&mut self, port: u16) {
        let deadline = Instant::now() + WAIT;
        loop {
            assert!(
                self.tree.poll(&mut self.child).unwrap().is_none(),
                "owned daemon exited before readiness"
            );
            if std::net::TcpStream::connect_timeout(
                &([127, 0, 0, 1], port).into(),
                Duration::from_millis(100),
            )
            .is_ok()
            {
                break;
            }
            assert!(Instant::now() < deadline, "owned daemon did not listen");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn stop(&mut self) {
        if !self.stopped {
            self.tree.force(&mut self.child);
            self.stopped = true;
        }
        assert!(gone(self.captured), "daemon physically retired");
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        if !self.stopped {
            self.tree.force(&mut self.child);
            self.stopped = true;
        }
    }
}
struct Database {
    daemon: Daemon,
    ctl: PathBuf,
    data: PathBuf,
    uid: u32,
    gid: u32,
}
impl Database {
    fn stop(&mut self) {
        if !self.daemon.stopped {
            let mut command = Command::new(&self.ctl);
            command
                .args(["-D"])
                .arg(&self.data)
                .args(["-m", "fast", "-w", "-t", "10", "stop"]);
            as_user(&mut command, self.uid, self.gid);
            output(command);
            self.daemon.stop();
        }
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        if !self.daemon.stopped {
            let mut command = Command::new(&self.ctl);
            command
                .arg("-D")
                .arg(&self.data)
                .args(["-m", "fast", "-w", "-t", "10", "stop"]);
            as_user(&mut command, self.uid, self.gid);
            let token = CancellationToken::new();
            let _ = rustpy_core::process::output_cancellable(&mut command, &token);
            self.daemon.tree.force(&mut self.daemon.child);
            self.daemon.stopped = true;
        }
    }
}
fn as_user(command: &mut Command, uid: u32, gid: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.uid(uid).gid(gid);
    }
    #[cfg(not(unix))]
    panic!(
        "this admitted provider fixture requires Unix uid/gid ownership: {command:?} {uid} {gid}"
    );
}
fn private(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(not(unix))]
    panic!(
        "Unix private permissions required: {} {mode}",
        path.display()
    );
}
fn port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
fn cert(openssl: &Path, root: &Path, name: &str) {
    let mut command = Command::new(openssl);
    command
        .current_dir(root)
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
        .arg(format!("{name}.key"))
        .arg("-out")
        .arg(format!("{name}.crt"));
    output(command);
    private(&root.join(format!("{name}.key")), 0o600);
}
struct Connection(Session);
impl Drop for Connection {
    fn drop(&mut self) {
        self.0.stop();
        let until = Instant::now() + WAIT;
        while self.0.alive() && Instant::now() < until {
            self.0.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
        if !std::thread::panicking() {
            assert!(
                !self.0.alive(),
                "ODBC session joined before dropping owned servers"
            );
        }
    }
}
fn connect(source: Source) -> Connection {
    let mut result = Connection(
        Session::start(
            397,
            source,
            "actual SSL/tunnel acceptance".into(),
            secret(),
            Settings {
                timeout_seconds: 5,
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let deadline = Instant::now() + WAIT;
    loop {
        for reply in result.0.poll() {
            match reply {
                Reply::Connected => return result,
                Reply::Error(e) => panic!("actual connection failed: {}", redacted(e)),
                Reply::Retired => panic!("connection worker retired"),
                _ => {}
            }
        }
        assert!(Instant::now() < deadline, "actual ODBC connect deadline");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn request(session: &mut Session, request: Request) -> Reply {
    session.send(request).unwrap();
    let until = Instant::now() + WAIT;
    let mut result = None;
    loop {
        for reply in session.poll() {
            match reply {
                Reply::State { .. } => return result.expect("actual result precedes state"),
                Reply::Retired => panic!("worker retired during request"),
                other => result = Some(other),
            }
        }
        assert!(Instant::now() < until, "actual SQL deadline");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn query(session: &mut Session, sql: &str) -> rustpy_db::QueryResult {
    let reply = request(
        session,
        Request::Query {
            sql: sql.into(),
            limit: 100,
        },
    );
    match reply {
        Reply::Results(mut rows) => {
            assert_eq!(rows.len(), 1);
            rows.remove(0).1
        }
        Reply::Error(e) => panic!("actual SQL failed: {}", redacted(e)),
        _ => panic!("query results required"),
    }
}
fn scalar(session: &mut Session, sql: &str) -> String {
    let rows = query(session, sql);
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0].len(), 1);
    rows.rows[0][0].display()
}
fn done(reply: Reply) {
    match reply {
        Reply::Done(_) => {}
        Reply::Error(e) => panic!("actual session operation failed: {}", redacted(e)),
        _ => panic!("session operation acknowledgment required"),
    }
}

fn generated_routine(
    session: &mut Session,
    objects: &[rustpy_db::workspace::Object],
    name: &str,
    arity: usize,
    omit_defaults: bool,
    values: &[(&str, &str)],
) -> rustpy_db::QueryResult {
    let object = objects
        .iter()
        .find(|object| {
            object.name == name
                && object
                    .routine
                    .as_ref()
                    .is_some_and(|r| r.parameters.len() == arity)
        })
        .expect("actual introspected routine signature");
    let call = rustpy_db::routine::invocation(
        Dialect::Postgres,
        &object.schema,
        &object.name,
        object.routine.as_ref().unwrap(),
        omit_defaults,
    )
    .unwrap();
    let values = values
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    let sql = rustpy_db::injection::bind_user_parameters(&call.sql, ":", &values).unwrap();
    query(session, &sql)
}

#[test]
#[ignore = "requires explicit RUSTPY_PG_TLS_ADMITTED=1, private provider root, admitted existing PG UID/GID and real PostgreSQL/OpenSSL/OpenSSH/ODBC paths; Linux, root, run alone"]
fn configured_owned_postgres_ssl_and_ssh_tunnel_catalog_schema_and_session_policy() {
    std::fs::read_to_string("/proc/self/stat")
        .expect("real Linux /proc required, never silently skip");
    assert_eq!(env("RUSTPY_PG_TLS_ADMITTED"), "1");
    let root = PathBuf::from(env("RUSTPY_PG_PROVIDER_ROOT"));
    assert!(root.is_absolute() && root.is_dir());
    let root_text = root.to_str().expect("provider root requires UTF-8");
    assert!(
        !root_text
            .chars()
            .any(|c| c.is_control() || matches!(c, '\'' | '\\')),
        "private provider root must be representable in PostgreSQL/OpenSSH config literals"
    );
    let uid: u32 = env("RUSTPY_TEST_PG_UID").parse().unwrap();
    let gid: u32 = env("RUSTPY_TEST_PG_GID").parse().unwrap();
    assert_ne!(
        uid, 0,
        "PostgreSQL children require an existing nonroot account"
    );
    let initdb = executable("RUSTPY_TEST_INITDB");
    let postgres = executable("RUSTPY_TEST_POSTGRES");
    let ctl = executable("RUSTPY_TEST_PG_CTL");
    let openssl = executable("RUSTPY_TEST_OPENSSL");
    let chown = executable("RUSTPY_TEST_CHOWN");
    let sshd = executable("RUSTPY_TEST_SSHD");
    let ssh = executable("RUSTPY_TEST_SSH");
    let keygen = executable("RUSTPY_TEST_SSH_KEYGEN");
    let cat = executable("RUSTPY_TEST_CAT");
    // AuthorizedKeysCommand parses a command string and requires its first
    // byte to be '/'; quoting the executable makes sshd reject the directive.
    // This provider fixture explicitly admits a plain absolute tool token.
    let cat_program = cat.to_str().expect("admitted tool requires UTF-8");
    assert!(
        cat_program
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._+-".contains(&byte)),
        "AuthorizedKeysCommand tool must be an admitted plain absolute token"
    );
    let user = env("RUSTPY_TEST_SSH_USER");
    assert!(
        !user.is_empty()
            && user
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
    );
    let driver = env("RUSTPY_TEST_POSTGRES_ODBC_DRIVER");
    assert!(!driver.trim().is_empty());
    if driver.contains('/') {
        assert!(
            Path::new(&driver).is_absolute() && Path::new(&driver).is_file(),
            "selected ODBC driver path must exist and be absolute"
        );
    }
    let case = tempfile::tempdir_in(&root).unwrap().keep();
    eprintln!(
        "private owned provider evidence retained at {}",
        case.display()
    );
    private(&case, 0o755); // Only newly created owned case, never an ancestor.
    let pg = case.join("postgres-owned");
    let sshroot = case.join("ssh-owned");
    std::fs::create_dir(&pg).unwrap();
    std::fs::create_dir(&sshroot).unwrap();
    private(&pg, 0o700);
    private(&sshroot, 0o700);
    cert(&openssl, &pg, "server");
    cert(&openssl, &pg, "wrong-ca");
    let password = pg.join("bootstrap-password");
    std::fs::write(&password, format!("{}\n", secret())).unwrap();
    private(&password, 0o600);
    let mut ownership = Command::new(chown);
    ownership.arg("-R").arg(format!("{uid}:{gid}")).arg(&pg);
    output(ownership);
    let data = pg.join("data");
    let mut initialize = Command::new(initdb);
    initialize
        .arg("-D")
        .arg(&data)
        .args([
            "-U",
            "rustpy_acceptance",
            "--auth-local=trust",
            "--auth-host=scram-sha-256",
            "--no-locale",
        ])
        .arg(format!("--pwfile={}", password.display()));
    as_user(&mut initialize, uid, gid);
    output(initialize);
    std::fs::remove_file(password).unwrap();
    let database_port = port();
    let ssh_port = port();
    let local_port = port();
    let quote = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "''"));
    use std::io::Write as _;
    writeln!(std::fs::OpenOptions::new().append(true).open(data.join("postgresql.conf")).unwrap(),"\nlisten_addresses='127.0.0.1'\nport={database_port}\nunix_socket_directories=''\nssl=on\nssl_cert_file={}\nssl_key_file={}\n",quote(&pg.join("server.crt")),quote(&pg.join("server.key"))).unwrap();
    std::fs::write(data.join("pg_hba.conf"),"local all all trust\nhostnossl all all 127.0.0.1/32 reject\nhostssl all all 127.0.0.1/32 scram-sha-256\n").unwrap();
    let mut server = Command::new(postgres);
    server.arg("-D").arg(&data);
    as_user(&mut server, uid, gid);
    let mut database = Database {
        daemon: Daemon::start(server, &pg.join("server.log")),
        ctl,
        data,
        uid,
        gid,
    };
    database.daemon.ready(database_port);
    for name in ["host", "user"] {
        let mut command = Command::new(&keygen);
        command
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(sshroot.join(name));
        output(command);
    }
    let authorized = sshroot.join("authorized_keys");
    std::fs::copy(sshroot.join("user.pub"), &authorized).unwrap();
    private(&authorized, 0o600);
    let known = sshroot.join("known_hosts");
    std::fs::write(
        &known,
        format!(
            "[127.0.0.1]:{ssh_port} {}",
            std::fs::read_to_string(sshroot.join("host.pub")).unwrap()
        ),
    )
    .unwrap();
    let config = sshroot.join("ssh_config");
    std::fs::write(&config,format!("Host *\n HostName 127.0.0.1\n Port {ssh_port}\n User {user}\n IdentityFile {}\n IdentitiesOnly yes\n IdentityAgent none\n UserKnownHostsFile {}\n GlobalKnownHostsFile /dev/null\n StrictHostKeyChecking yes\n ControlMaster no\n",ssh_config_path(&sshroot.join("user")),ssh_config_path(&known))).unwrap();
    let daemon_config = sshroot.join("sshd_config");
    std::fs::write(&daemon_config,format!("Port {ssh_port}\nListenAddress 127.0.0.1\nHostKey {}\nPidFile {}\nAuthorizedKeysFile none\nAuthorizedKeysCommand {} {}\nAuthorizedKeysCommandUser {user}\nAllowUsers {user}\nAuthenticationMethods publickey\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nUsePAM no\nPermitRootLogin prohibit-password\nStrictModes yes\nAllowTcpForwarding local\nPermitOpen 127.0.0.1:{database_port}\nAllowAgentForwarding no\nX11Forwarding no\n",ssh_config_path(&sshroot.join("host")),ssh_config_path(&sshroot.join("sshd.pid")),cat_program,ssh_config_path(&authorized))).unwrap();
    let mut server = Command::new(sshd);
    server.args(["-D", "-e", "-f"]).arg(daemon_config);
    let mut ssh_server = Daemon::start(server, &sshroot.join("sshd.log"));
    ssh_server.ready(ssh_port);
    let mut source = Source {
        id: 397,
        name: "owned PostgreSQL SSL acceptance".into(),
        dialect: Dialect::Postgres,
        driver,
        host: "127.0.0.1".into(),
        port: database_port,
        database: "postgres".into(),
        user: "rustpy_acceptance".into(),
        schemas: vec!["rustpy_tls_acceptance".into()],
        ..Default::default()
    };
    source
        .options
        .insert("SSLmode".into(), "verify-full".into());
    source.options.insert(
        "Pqopt".into(),
        format!("sslrootcert={}", conninfo_path(&pg.join("server.crt"))),
    );
    let mut direct = connect(source.clone());
    assert_eq!(
        scalar(
            &mut direct.0,
            "SELECT ssl::text FROM pg_stat_ssl WHERE pid=pg_backend_pid()"
        ),
        "true"
    );
    query(&mut direct.0, "CREATE SCHEMA rustpy_tls_acceptance");
    query(
        &mut direct.0,
        "CREATE TABLE rustpy_tls_acceptance.items(id integer PRIMARY KEY)",
    );
    query(
        &mut direct.0,
        "CREATE FUNCTION rustpy_tls_acceptance.answer() RETURNS integer LANGUAGE sql AS 'SELECT 42'",
    );
    query(
        &mut direct.0,
        "CREATE FUNCTION rustpy_tls_acceptance.add_values(a integer, b integer DEFAULT 7) RETURNS integer LANGUAGE sql AS 'SELECT a+b'",
    );
    query(
        &mut direct.0,
        "CREATE FUNCTION rustpy_tls_acceptance.add_values(a text) RETURNS text LANGUAGE sql AS 'SELECT a'",
    );
    query(
        &mut direct.0,
        "CREATE PROCEDURE rustpy_tls_acceptance.adjust(IN increment integer, INOUT total integer, OUT doubled integer) LANGUAGE plpgsql AS $$ BEGIN total := total+increment; doubled := increment*2; END $$",
    );
    let Reply::Catalog(before) = request(&mut direct.0, Request::Catalog) else {
        panic!("actual pre-alter catalog required")
    };
    let answer = generated_routine(&mut direct.0, &before, "answer", 0, false, &[]);
    assert_eq!(answer.rows[0][0].display(), "42");
    let explicit = generated_routine(
        &mut direct.0,
        &before,
        "add_values",
        2,
        false,
        &[("arg_1", "5"), ("arg_2", "8")],
    );
    assert_eq!(explicit.rows[0][0].display(), "13");
    let defaults = generated_routine(
        &mut direct.0,
        &before,
        "add_values",
        2,
        true,
        &[("arg_1", "5")],
    );
    assert_eq!(defaults.rows[0][0].display(), "12");
    let text = generated_routine(
        &mut direct.0,
        &before,
        "add_values",
        1,
        false,
        &[("arg_1", "'quotes '''' and ; stay data'")],
    );
    assert_eq!(text.rows[0][0].display(), "quotes '' and ; stay data");
    let procedure = before
        .iter()
        .find(|o| o.name == "adjust")
        .unwrap()
        .routine
        .as_ref()
        .unwrap();
    assert_eq!(procedure.kind, rustpy_db::routine::RoutineKind::Procedure);
    assert_eq!(
        procedure
            .parameters
            .iter()
            .map(|p| p.mode)
            .collect::<Vec<_>>(),
        [
            rustpy_db::routine::ParameterMode::In,
            rustpy_db::routine::ParameterMode::InOut,
            rustpy_db::routine::ParameterMode::Out
        ]
    );
    let adjusted = generated_routine(
        &mut direct.0,
        &before,
        "adjust",
        3,
        false,
        &[("arg_1", "3"), ("arg_2", "10")],
    );
    assert_eq!(adjusted.rows.len(), 1);
    assert_eq!(
        adjusted.rows[0]
            .iter()
            .map(|v| v.display())
            .collect::<Vec<_>>(),
        ["13", "6"]
    );
    // A new same-prefix sibling makes omission unsafe even though the selected
    // two-input signature itself still has a default. Retain its full call.
    query(
        &mut direct.0,
        "CREATE FUNCTION rustpy_tls_acceptance.add_values(a integer) RETURNS integer LANGUAGE sql AS 'SELECT a+100'",
    );
    let Reply::Catalog(overloaded) = request(&mut direct.0, Request::Catalog) else {
        panic!("refreshed actual overload family required")
    };
    let selected = overloaded
        .iter()
        .find(|o| {
            o.name == "add_values" && o.routine.as_ref().is_some_and(|r| r.parameters.len() == 2)
        })
        .unwrap();
    assert!(rustpy_db::routine::invocation(
        Dialect::Postgres,
        &selected.schema,
        &selected.name,
        selected.routine.as_ref().unwrap(),
        true
    )
    .is_err());
    let full = generated_routine(
        &mut direct.0,
        &overloaded,
        "add_values",
        2,
        false,
        &[("arg_1", "5"), ("arg_2", "8")],
    );
    assert_eq!(full.rows[0][0].display(), "13");
    let old = before.iter().find(|o| o.name == "items").unwrap();
    let mut design = rustpy_db::schema::TableDesign {
        name: old.qualified(),
        columns: old.columns.clone(),
        primary: vec!["id".into()],
        ..Default::default()
    };
    design.columns.push(rustpy_db::workspace::Column {
        name: "label".into(),
        data_type: "text".into(),
        nullable: true,
        primary: false,
        default: None,
    });
    let alteration = rustpy_db::schema::alter_sql(old, &design, Dialect::Postgres).unwrap();
    done(request(
        &mut direct.0,
        Request::Submit(vec![Change::Sql(alteration)]),
    ));
    let Reply::Catalog(objects) = request(&mut direct.0, Request::Catalog) else {
        panic!("actual provider catalog required")
    };
    assert!(objects
        .iter()
        .any(|o| o.kind == "ROUTINE" && o.name == "answer"));
    assert!(objects
        .iter()
        .any(|o| o.name == "items" && o.columns.iter().any(|c| c.name == "label")));
    drop(direct);
    source.ssh = Some(Tunnel {
        ssh_program: ssh.clone(),
        config_file: Some(config.clone()),
        destination: format!("{user}@127.0.0.1"),
        local_port,
        remote_host: "127.0.0.1".into(),
        remote_port: database_port,
        identity: sshroot.join("user"),
        known_hosts: known,
    });
    let mut tunneled = connect(source.clone());
    let tunnel_processes = tunnel_children(&ssh, &config);
    let server_sessions = daemon_children(ssh_server.captured);
    assert!(
        !server_sessions.is_empty(),
        "actual sshd forwarding session required"
    );
    assert_eq!(
        tunnel_processes.len(),
        1,
        "observe the actual session-owned private-config SSH tunnel"
    );
    assert_eq!(
        scalar(
            &mut tunneled.0,
            "SELECT ssl::text FROM pg_stat_ssl WHERE pid=pg_backend_pid()"
        ),
        "true"
    );
    assert_eq!(
        scalar(&mut tunneled.0, "SELECT rustpy_tls_acceptance.answer()"),
        "42"
    );
    done(request(&mut tunneled.0, Request::AutoCommit(false)));
    done(request(
        &mut tunneled.0,
        Request::Isolation("SERIALIZABLE".into()),
    ));
    assert_eq!(
        scalar(&mut tunneled.0, "SHOW transaction_isolation"),
        "serializable"
    );
    done(request(&mut tunneled.0, Request::Rollback));
    done(request(&mut tunneled.0, Request::AutoCommit(true)));
    done(request(&mut tunneled.0, Request::ReadOnly(true)));
    assert_eq!(
        scalar(&mut tunneled.0, "SHOW default_transaction_read_only"),
        "on"
    );
    assert!(matches!(
        request(
            &mut tunneled.0,
            Request::Query {
                sql: "INSERT INTO rustpy_tls_acceptance.items VALUES(1,'must refuse')".into(),
                limit: 10
            }
        ),
        Reply::Error(_)
    ));
    done(request(&mut tunneled.0, Request::ReadOnly(false)));
    assert_eq!(
        scalar(
            &mut tunneled.0,
            "SELECT COUNT(*) FROM rustpy_tls_acceptance.items"
        ),
        "0"
    );
    // A Stop/toggle must never implicitly commit manual user work. Only an
    // acknowledged explicit transaction boundary authorizes the transition.
    done(request(&mut tunneled.0, Request::AutoCommit(true))); // already enabled
    done(request(&mut tunneled.0, Request::AutoCommit(false)));
    query(
        &mut tunneled.0,
        "INSERT INTO rustpy_tls_acceptance.items VALUES(1,'rollback')",
    );
    assert!(matches!(
        request(&mut tunneled.0, Request::AutoCommit(true)),
        Reply::Error(_)
    ));
    done(request(&mut tunneled.0, Request::Rollback));
    done(request(&mut tunneled.0, Request::AutoCommit(true)));
    assert_eq!(
        scalar(
            &mut tunneled.0,
            "SELECT COUNT(*) FROM rustpy_tls_acceptance.items"
        ),
        "0"
    );
    done(request(&mut tunneled.0, Request::AutoCommit(false)));
    assert!(matches!(
        request(
            &mut tunneled.0,
            Request::Query {
                sql: "SELECT nonexistent_column FROM rustpy_tls_acceptance.items".into(),
                limit: 1
            }
        ),
        Reply::Error(_)
    ));
    assert!(matches!(
        request(&mut tunneled.0, Request::AutoCommit(true)),
        Reply::Error(_)
    ));
    done(request(&mut tunneled.0, Request::Rollback));
    done(request(&mut tunneled.0, Request::AutoCommit(true)));
    done(request(&mut tunneled.0, Request::AutoCommit(false)));
    query(
        &mut tunneled.0,
        "INSERT INTO rustpy_tls_acceptance.items VALUES(2,'committed')",
    );
    done(request(&mut tunneled.0, Request::Commit));
    done(request(&mut tunneled.0, Request::AutoCommit(true)));
    assert_eq!(
        scalar(
            &mut tunneled.0,
            "SELECT COUNT(*) FROM rustpy_tls_acceptance.items"
        ),
        "1"
    );
    done(request(&mut tunneled.0, Request::AutoCommit(false)));
    assert!(matches!(
        request(&mut tunneled.0, Request::Catalog),
        Reply::Catalog(_)
    ));
    assert!(matches!(
        request(&mut tunneled.0, Request::AutoCommit(true)),
        Reply::Error(_)
    ));
    done(request(&mut tunneled.0, Request::Rollback));
    done(request(&mut tunneled.0, Request::AutoCommit(true)));
    // Failed Submit rolls back only its savepoint, not the user's earlier row.
    done(request(&mut tunneled.0, Request::AutoCommit(false)));
    query(
        &mut tunneled.0,
        "INSERT INTO rustpy_tls_acceptance.items VALUES(10,'prior')",
    );
    assert!(matches!(
        request(
            &mut tunneled.0,
            Request::Submit(vec![
                Change::Sql("INSERT INTO rustpy_tls_acceptance.items VALUES(11,'batch')".into()),
                Change::Sql(
                    "INSERT INTO rustpy_tls_acceptance.items VALUES(10,'duplicate')".into()
                )
            ])
        ),
        Reply::Error(_)
    ));
    assert_eq!(
        scalar(
            &mut tunneled.0,
            "SELECT COUNT(*) FROM rustpy_tls_acceptance.items"
        ),
        "2"
    );
    let mut observer_source = source.clone();
    observer_source.ssh = None;
    let mut observer = connect(observer_source.clone());
    assert_eq!(
        scalar(
            &mut observer.0,
            "SELECT COUNT(*) FROM rustpy_tls_acceptance.items"
        ),
        "1"
    );
    assert!(matches!(
        request(&mut tunneled.0, Request::AutoCommit(true)),
        Reply::Error(_)
    ));
    done(request(
        &mut tunneled.0,
        Request::Submit(vec![Change::Sql(
            "INSERT INTO rustpy_tls_acceptance.items VALUES(12,'later batch')".into(),
        )]),
    ));
    assert_eq!(
        scalar(
            &mut observer.0,
            "SELECT COUNT(*) FROM rustpy_tls_acceptance.items"
        ),
        "1"
    );
    done(request(&mut tunneled.0, Request::Commit));
    done(request(&mut tunneled.0, Request::AutoCommit(true)));
    assert_eq!(
        scalar(
            &mut observer.0,
            "SELECT COUNT(*) FROM rustpy_tls_acceptance.items"
        ),
        "3"
    );
    done(request(&mut tunneled.0, Request::AutoCommit(false)));
    query(
        &mut tunneled.0,
        "INSERT INTO rustpy_tls_acceptance.items VALUES(13,'explicit rollback')",
    );
    assert!(matches!(
        request(
            &mut tunneled.0,
            Request::Submit(vec![
                Change::Sql("INSERT INTO rustpy_tls_acceptance.items VALUES(14,'batch')".into()),
                Change::Sql(
                    "INSERT INTO rustpy_tls_acceptance.items VALUES(13,'duplicate')".into()
                )
            ])
        ),
        Reply::Error(_)
    ));
    done(request(&mut tunneled.0, Request::Rollback));
    done(request(&mut tunneled.0, Request::AutoCommit(true)));
    assert_eq!(
        scalar(
            &mut observer.0,
            "SELECT COUNT(*) FROM rustpy_tls_acceptance.items"
        ),
        "3"
    );
    // An automatic batch owns its outer rollback; the next ordinary write must
    // really be automatic again, visible to another public Session immediately.
    assert!(matches!(
        request(
            &mut tunneled.0,
            Request::Submit(vec![
                Change::Sql(
                    "INSERT INTO rustpy_tls_acceptance.items VALUES(20,'automatic batch')".into()
                ),
                Change::Sql("INSERT INTO rustpy_tls_acceptance.items VALUES(2,'duplicate')".into())
            ])
        ),
        Reply::Error(_)
    ));
    query(
        &mut tunneled.0,
        "INSERT INTO rustpy_tls_acceptance.items VALUES(21,'automatic restored')",
    );
    assert_eq!(
        scalar(
            &mut observer.0,
            "SELECT COUNT(*) FROM rustpy_tls_acceptance.items"
        ),
        "4"
    );
    drop(observer);
    query(&mut tunneled.0, "DROP SCHEMA rustpy_tls_acceptance CASCADE");
    drop(tunneled);
    retired(&tunnel_processes);
    retired(&server_sessions);
    let until = Instant::now() + WAIT;
    while std::net::TcpStream::connect_timeout(
        &([127, 0, 0, 1], local_port).into(),
        Duration::from_millis(100),
    )
    .is_ok()
    {
        assert!(
            Instant::now() < until,
            "session retirement must close its actual SSH listener"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    // Wrong CA must be rejected by the actual driver, not a local hash helper.
    source.ssh = None;
    source.options.insert(
        "Pqopt".into(),
        format!("sslrootcert={}", conninfo_path(&pg.join("wrong-ca.crt"))),
    );
    let mut rejected = Connection(
        Session::start(
            398,
            source,
            "wrong CA refusal".into(),
            secret(),
            Settings {
                timeout_seconds: 5,
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let deadline = Instant::now() + WAIT;
    let mut refused = false;
    while !refused {
        for reply in rejected.0.poll() {
            match reply {
                Reply::Error(e) => {
                    let safe = redacted(e).to_lowercase();
                    assert!(
                        safe.contains("certificate") || safe.contains("ssl"),
                        "wrong CA must cause TLS verification failure: {safe}"
                    );
                    refused = true;
                }
                Reply::Connected => panic!("wrong CA must never connect"),
                _ => {}
            }
        }
        assert!(Instant::now() < deadline, "wrong CA rejection deadline");
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(rejected);
    ssh_server.stop();
    database.stop();
    assert!(gone(ssh_server.captured) && gone(database.daemon.captured));
}
