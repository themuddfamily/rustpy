//! Public session regression: a failed grid submission owns only its own batch.
use rustpy_db::{
    session::{Reply, Request, Session},
    workspace::{Change, Dialect, Settings, Source},
    Value,
};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(10);
struct Owned(Session);
impl Drop for Owned {
    fn drop(&mut self) {
        self.0.stop();
        let deadline = Instant::now() + WAIT;
        while self.0.alive() && Instant::now() < deadline {
            self.0.poll();
            std::thread::sleep(Duration::from_millis(2));
        }
        if !std::thread::panicking() {
            assert!(!self.0.alive(), "public SQLite session physically retires");
        }
    }
}
fn open(source: Source) -> Owned {
    let mut session = Owned(
        Session::start(
            1,
            source,
            "manual submit regression".into(),
            String::new(),
            Settings::default(),
        )
        .unwrap(),
    );
    let deadline = Instant::now() + WAIT;
    loop {
        for reply in session.0.poll() {
            match reply {
                Reply::Connected => return session,
                Reply::Error(error) => panic!("SQLite fixture connection: {error}"),
                _ => {}
            }
        }
        assert!(Instant::now() < deadline, "SQLite connection deadline");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn request(session: &mut Session, request: Request) -> (Reply, bool) {
    session.send(request).unwrap();
    let deadline = Instant::now() + WAIT;
    let mut response = None;
    loop {
        for reply in session.poll() {
            match reply {
                Reply::State { auto_commit, .. } => {
                    return (
                        response.expect("response precedes acknowledged state"),
                        auto_commit,
                    );
                }
                Reply::Retired => panic!("SQLite session retired during a request"),
                other => {
                    assert!(response.is_none());
                    response = Some(other);
                }
            }
        }
        assert!(Instant::now() < deadline, "SQLite request deadline");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn done(session: &mut Session, command: Request) {
    match request(session, command).0 {
        Reply::Done(_) => {}
        Reply::Error(error) => panic!("unexpected SQLite transaction error: {error}"),
        _ => panic!("transaction acknowledgment required"),
    }
}
fn rows(session: &mut Session, sql: &str) -> Vec<Vec<Value>> {
    match request(
        session,
        Request::Query {
            sql: sql.into(),
            limit: 100,
        },
    )
    .0
    {
        Reply::Results(mut results) => {
            assert_eq!(results.len(), 1);
            results.remove(0).1.rows
        }
        Reply::Error(error) => panic!("readback failed: {error}"),
        _ => panic!("actual SQL result required"),
    }
}
fn insert(id: i64, payload: &str) -> Change {
    Change::Insert {
        table: "items".into(),
        values: vec![
            ("id".into(), Value::Int(id)),
            ("payload".into(), Value::Text(payload.into())),
        ],
    }
}
fn expected(values: &[(i64, &str)]) -> Vec<Vec<Value>> {
    values
        .iter()
        .map(|(id, text)| vec![Value::Int(*id), Value::Text((*text).into())])
        .collect()
}

#[test]
fn failed_manual_submit_rolls_back_only_batch_and_preserves_prior_pending_work() {
    let directory = tempfile::tempdir().unwrap();
    let source = Source {
        id: 1,
        name: "owned SQLite savepoint regression".into(),
        dialect: Dialect::Sqlite,
        file: directory.path().join("owned.sqlite"),
        ..Default::default()
    };
    let mut editor = open(source.clone());
    rows(
        &mut editor.0,
        "CREATE TABLE items(id INTEGER PRIMARY KEY, payload TEXT NOT NULL)",
    );
    let mut observer = open(source);
    done(&mut editor.0, Request::AutoCommit(false));
    rows(
        &mut editor.0,
        "INSERT INTO items VALUES(1,'prior user work')",
    );

    let (failed, manual_after_failure) = request(
        &mut editor.0,
        Request::Submit(vec![
            insert(2, "partial batch"),
            insert(1, "duplicate constraint failure"),
        ]),
    );
    assert!(
        matches!(failed, Reply::Error(ref error) if error.contains("UNIQUE constraint failed")),
        "actual second-statement constraint failure required"
    );
    assert_eq!(
        rows(&mut editor.0, "SELECT id,payload FROM items ORDER BY id"),
        expected(&[(1, "prior user work")]),
        "failed Submit must remove its first successful INSERT2 while retaining prior pending INSERT1"
    );
    assert!(
        !manual_after_failure,
        "failed Submit must preserve manual transaction mode"
    );
    assert!(
        rows(&mut observer.0, "SELECT id,payload FROM items").is_empty(),
        "retained user work must remain uncommitted"
    );

    done(
        &mut editor.0,
        Request::Submit(vec![insert(3, "later successful batch")]),
    );
    assert_eq!(
        rows(&mut editor.0, "SELECT id,payload FROM items ORDER BY id"),
        expected(&[(1, "prior user work"), (3, "later successful batch")])
    );
    assert!(
        rows(&mut observer.0, "SELECT id,payload FROM items").is_empty(),
        "a later successful manual Submit must not commit earlier work"
    );
    done(&mut editor.0, Request::Commit);
    assert_eq!(
        rows(&mut observer.0, "SELECT id,payload FROM items ORDER BY id"),
        expected(&[(1, "prior user work"), (3, "later successful batch")])
    );

    rows(
        &mut editor.0,
        "INSERT INTO items VALUES(4,'pending then explicit rollback')",
    );
    let (failed, _) = request(
        &mut editor.0,
        Request::Submit(vec![
            insert(5, "partial second batch"),
            insert(4, "duplicate"),
        ]),
    );
    assert!(matches!(failed, Reply::Error(_)));
    assert_eq!(
        rows(&mut editor.0, "SELECT id,payload FROM items ORDER BY id"),
        expected(&[
            (1, "prior user work"),
            (3, "later successful batch"),
            (4, "pending then explicit rollback")
        ])
    );
    done(&mut editor.0, Request::Rollback);
    assert_eq!(
        rows(&mut observer.0, "SELECT id,payload FROM items ORDER BY id"),
        expected(&[(1, "prior user work"), (3, "later successful batch")])
    );
}
