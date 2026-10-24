//! Explicitly admitted PostgreSQL ODBC acceptance; never provisions or skips.
//! Requires a disposable PostgreSQL database reachable only at 127.0.0.1,
//! an admitted PostgreSQL ODBC driver, and explicit connection environment.
//! This proves one actual provider, not every advertised ODBC dialect.
use rustpy_db::{
    session::{Reply, Request, Session},
    workspace::{Change, Dialect, Settings, Source, Workspace},
    Value,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

const WAIT: Duration = Duration::from_secs(20);

struct OwnedSession(Session);
impl Drop for OwnedSession {
    fn drop(&mut self) {
        self.0.stop();
        let deadline = Instant::now() + WAIT;
        while self.0.alive() && Instant::now() < deadline {
            self.0.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
        if !std::thread::panicking() {
            assert!(!self.0.alive(), "ODBC worker must physically retire");
        }
    }
}

// Driver diagnostics can quote either the session password or its ODBC
// attribute encoding. Scrub both before surfacing the actual connection error.
fn safe_driver_error(error: &str) -> String {
    let Ok(secret) = std::env::var("RUSTPY_TEST_POSTGRES_PASSWORD") else {
        return "ODBC error unavailable: session-secret admission is missing".into();
    };
    if secret.is_empty() {
        return "ODBC error unavailable: session-secret admission is empty".into();
    }
    let escaped = secret.replace('}', "}}");
    let mut forms = [format!("{{{escaped}}}"), escaped, secret];
    forms.sort_by_key(|value| std::cmp::Reverse(value.len()));
    forms.into_iter().fold(error.to_string(), |message, value| {
        message.replace(&value, "[redacted]")
    })
}

fn connected(session: &mut Session) {
    let deadline = Instant::now() + WAIT;
    loop {
        for reply in session.poll() {
            match reply {
                Reply::Connected => return,
                Reply::Error(error) => panic!(
                    "admitted PostgreSQL ODBC connection failed: {}",
                    safe_driver_error(&error)
                ),
                Reply::Retired => panic!("ODBC worker retired before connection"),
                _ => {}
            }
        }
        assert!(Instant::now() < deadline, "ODBC connection timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn request(session: &mut Session, request: Request) -> Reply {
    session
        .send(request)
        .expect("idle retained session accepts request");
    let deadline = Instant::now() + WAIT;
    let mut response = None;
    let mut state = false;
    loop {
        for reply in session.poll() {
            match reply {
                Reply::Error(error) => panic!(
                    "configured ODBC request failed: {}",
                    safe_driver_error(&error)
                ),
                Reply::Retired => panic!("ODBC worker retired during request"),
                Reply::State { .. } => state = true,
                other => response = Some(other),
            }
        }
        if state {
            return response.expect("request response must precede its state acknowledgment");
        }
        assert!(Instant::now() < deadline, "ODBC request timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn count(session: &mut Session) -> String {
    let Reply::Results(results) = request(
        session,
        Request::Query {
            sql: "SELECT COUNT(*) FROM rustpy_odbc_acceptance".into(),
            limit: 10,
        },
    ) else {
        panic!("query must return actual results")
    };
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].1.rows.len(), 1);
    assert_eq!(results[0].1.rows[0].len(), 1);
    results[0].1.rows[0][0].display()
}

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("explicit {name} admission required"))
}

#[test]
#[ignore = "requires explicitly admitted PostgreSQL ODBC driver and disposable loopback server"]
fn configured_postgres_odbc_session_catalog_submit_rollback_and_retirement() {
    let driver = required("RUSTPY_TEST_POSTGRES_ODBC_DRIVER");
    assert!(
        !driver.trim().is_empty(),
        "driver admission must not be empty"
    );
    if driver.contains('/') {
        assert!(
            PathBuf::from(&driver).is_absolute() && PathBuf::from(&driver).is_file(),
            "admitted driver library must be an existing absolute path"
        );
    }
    let host = required("RUSTPY_TEST_POSTGRES_HOST");
    assert_eq!(
        host, "127.0.0.1",
        "only explicitly admitted loopback servers allowed"
    );
    let port: u16 = required("RUSTPY_TEST_POSTGRES_PORT")
        .parse()
        .expect("explicit numeric port");
    assert_ne!(port, 0);
    let database = required("RUSTPY_TEST_POSTGRES_DATABASE");
    let user = required("RUSTPY_TEST_POSTGRES_USER");
    assert!(!database.is_empty() && !user.is_empty());
    let secret = required("RUSTPY_TEST_POSTGRES_PASSWORD");
    assert!(
        !secret.is_empty(),
        "acceptance requires explicit session password authority"
    );
    assert_eq!(
        required("RUSTPY_TEST_POSTGRES_DISPOSABLE"),
        "yes",
        "root must admit this database as disposable"
    );
    let root = PathBuf::from(
        std::env::var_os("RUSTPY_TEST_ODBC_DISPOSABLE_ROOT")
            .expect("explicit disposable local root required"),
    );
    assert!(root.is_absolute() && root.is_dir());
    let fixture = tempfile::Builder::new()
        .prefix("configured-odbc-")
        .tempdir_in(root)
        .unwrap();
    let source = Source {
        id: 1,
        name: "isolated PostgreSQL ODBC acceptance".into(),
        dialect: Dialect::Postgres,
        driver,
        host,
        port,
        database,
        user,
        ..Default::default()
    };
    let workspace = Workspace {
        sources: vec![source.clone()],
        ..Default::default()
    };
    workspace.save(fixture.path()).unwrap();
    let persisted_path = fixture.path().join(".rustpy/database-workspace.json");
    let persisted = std::fs::read(&persisted_path).unwrap();
    assert!(!String::from_utf8_lossy(&persisted).contains(&secret));
    assert_eq!(
        Workspace::load(fixture.path()).sources,
        vec![source.clone()]
    );
    let mut rejected = workspace.clone();
    rejected.sources[0]
        .options
        .insert("PWD".into(), secret.clone());
    assert!(
        rejected.save(fixture.path()).is_err(),
        "persistence must reject password authority"
    );
    assert_eq!(std::fs::read(&persisted_path).unwrap(), persisted);

    let settings = Settings {
        auto_commit: true,
        timeout_seconds: 5,
        ..Default::default()
    };
    let mut session = OwnedSession(
        Session::start(
            1,
            source.clone(),
            "ODBC acceptance".into(),
            secret.clone(),
            settings.clone(),
        )
        .unwrap(),
    );
    connected(&mut session.0);
    // CREATE deliberately fails on an existing fixture table: no shared database
    // default or overwrite of a previous run is silently accepted.
    assert!(matches!(request(&mut session.0, Request::Query {
        sql: "CREATE TABLE rustpy_odbc_acceptance(id INTEGER PRIMARY KEY, label TEXT NOT NULL); INSERT INTO rustpy_odbc_acceptance VALUES (1, 'retained')".into(), limit: 10,
    }), Reply::Results(_)));
    assert!(matches!(
        request(&mut session.0, Request::AutoCommit(false)),
        Reply::Done(_)
    ));
    let Reply::Catalog(objects) = request(&mut session.0, Request::Catalog) else {
        panic!("actual ODBC catalog required")
    };
    let table = objects
        .iter()
        .find(|o| o.name == "rustpy_odbc_acceptance")
        .expect("seeded table must be discovered by driver metadata");
    assert!(table.columns.iter().any(|c| c.name == "label"));
    assert_eq!(count(&mut session.0), "1");
    assert!(matches!(
        request(
            &mut session.0,
            Request::Submit(vec![Change::Insert {
                table: "rustpy_odbc_acceptance".into(),
                values: vec![
                    ("id".into(), Value::Int(2)),
                    ("label".into(), Value::Text("rollback me".into()))
                ],
            }])
        ),
        Reply::Done(_)
    ));
    assert_eq!(count(&mut session.0), "2");
    assert!(matches!(
        request(&mut session.0, Request::Rollback),
        Reply::Done(_)
    ));
    assert_eq!(count(&mut session.0), "1");
    session.0.stop();
    let deadline = Instant::now() + WAIT;
    let mut retired = false;
    while session.0.alive() && Instant::now() < deadline {
        retired |= session.0.poll().iter().any(|r| matches!(r, Reply::Retired));
        std::thread::sleep(Duration::from_millis(5));
    }
    retired |= session.0.poll().iter().any(|r| matches!(r, Reply::Retired));
    assert!(retired, "worker must publish retirement");
    assert!(
        !session.0.alive(),
        "poll must join the physically finished worker"
    );
    // A second actual ODBC connection proves rollback persisted across sessions.
    let mut verifier =
        OwnedSession(Session::start(2, source, "ODBC readback".into(), secret, settings).unwrap());
    connected(&mut verifier.0);
    assert_eq!(count(&mut verifier.0), "1");
    assert!(matches!(
        request(
            &mut verifier.0,
            Request::Query {
                sql: "DROP TABLE rustpy_odbc_acceptance".into(),
                limit: 10,
            }
        ),
        Reply::Results(_)
    ));
    drop(verifier);
    assert_eq!(std::fs::read(&persisted_path).unwrap(), persisted);
}
