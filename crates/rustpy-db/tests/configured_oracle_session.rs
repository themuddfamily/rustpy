//! Genuine Oracle session, introspection and data-editor acceptance:
//! table/column/key catalog, object details, paging, search, explain plan,
//! non-ASCII VARCHAR2 text, manual-mode Submit with savepoint isolation,
//! commit/rollback visibility from a second connection, auto-commit Submit,
//! session isolation level and physical retirement.
//! Requires an explicitly admitted disposable loopback server and ODBC driver;
//! never provisions or skips.
use rustpy_db::{
    injection::bind_user_parameters,
    session::{Reply, Request, Session},
    workspace::{Change, Dialect, Settings, Source},
    Value,
};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(120);

fn required(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("explicit {key} required"))
}

struct Owned(Session);
impl Owned {
    fn start(id: u64, source: &Source, secret: &str) -> Self {
        let mut owned = Self(
            Session::start(
                id,
                source.clone(),
                "actual Oracle session".into(),
                secret.into(),
                Settings {
                    timeout_seconds: 10,
                    ..Default::default()
                },
            )
            .expect("start Oracle worker"),
        );
        let end = Instant::now() + WAIT;
        loop {
            for reply in owned.0.poll() {
                match reply {
                    Reply::Connected => return owned,
                    Reply::Error(_) => panic!(
                        "actual Oracle connection failed; diagnostic suppressed to protect credentials"
                    ),
                    Reply::Retired => panic!("worker retired before connection"),
                    _ => {}
                }
            }
            assert!(Instant::now() < end, "actual Oracle connection timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn outcome(&mut self, request: Request) -> Result<(Reply, bool), &'static str> {
        self.0
            .send(request)
            .map_err(|_| "idle retained Oracle session must accept request")?;
        let end = Instant::now() + WAIT;
        let mut reply = None;
        loop {
            for value in self.0.poll() {
                match value {
                    Reply::Retired => return Err("Oracle worker retired prematurely"),
                    Reply::State { auto_commit, .. } => {
                        return reply
                            .take()
                            .map(|reply| (reply, auto_commit))
                            .ok_or("request reply must precede its state acknowledgment")
                    }
                    value => reply = Some(value),
                }
            }
            if Instant::now() >= end {
                return Err("actual Oracle request timed out");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn request(&mut self, request: Request) -> (Reply, bool) {
        let answer = self
            .outcome(request)
            .expect("actual Oracle request completes");
        assert!(
            !matches!(answer.0, Reply::Error(_)),
            "actual Oracle request failed; driver diagnostic suppressed to protect credentials"
        );
        answer
    }
    fn done(&mut self, request: Request) -> bool {
        let (reply, auto_commit) = self.request(request);
        assert!(matches!(reply, Reply::Done(_)), "request must complete");
        auto_commit
    }
    fn results(&mut self, request: Request) -> Vec<(String, rustpy_db::QueryResult)> {
        let Reply::Results(results) = self.request(request).0 else {
            panic!("actual Oracle results required");
        };
        results
    }
    fn rows(&mut self, sql: String) -> Vec<Vec<String>> {
        self.results(Request::Query { sql, limit: 100 })
            .into_iter()
            .filter(|(_, result)| !result.columns.is_empty())
            .flat_map(|(_, result)| result.rows)
            .map(|row| row.iter().map(Value::display).collect())
            .collect()
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        self.0.stop();
        let end = Instant::now() + Duration::from_secs(20);
        while self.0.alive() && Instant::now() < end {
            self.0.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
        if !std::thread::panicking() {
            assert!(!self.0.alive(), "Oracle worker must physically retire");
        }
    }
}

/// Fixture-owned tables, dropped only while their captured object ids match.
struct Fixture {
    admin: Owned,
    tables: Vec<(String, String)>,
}
impl Fixture {
    fn object_id(&mut self, name: &str) -> String {
        let rows = self.admin.rows(format!(
            "SELECT COALESCE(MAX(TO_CHAR(OBJECT_ID)),' ') FROM USER_OBJECTS WHERE OBJECT_NAME='{name}' AND OBJECT_TYPE='TABLE'"
        ));
        rows[0][0].trim().to_string()
    }
    fn create(&mut self, name: &str, ddl: String) {
        assert!(
            name.starts_with("RUSTPY_OWNED_ED")
                && name
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        );
        assert!(
            self.object_id(name).is_empty(),
            "fixture name must be unused"
        );
        // The first statement is the CREATE; register the table before any
        // following fixture statement can fail.
        let (create, rest) = ddl.split_once(';').unwrap_or((&ddl, ""));
        self.admin.results(Request::Query {
            sql: create.into(),
            limit: 1,
        });
        let id = self.object_id(name);
        assert!(
            id.parse::<i64>().is_ok(),
            "created table must be identified"
        );
        self.tables.push((name.into(), id));
        if !rest.trim().is_empty() {
            self.admin.results(Request::Query {
                sql: rest.into(),
                limit: 1,
            });
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let mut clean = true;
        while let Some((name, id)) = self.tables.pop() {
            let sql = format!(
                "DECLARE n NUMBER; BEGIN SELECT COUNT(*) INTO n FROM USER_OBJECTS WHERE OBJECT_NAME='{name}' AND OBJECT_TYPE='TABLE' AND OBJECT_ID={id}; IF n=1 THEN EXECUTE IMMEDIATE 'DROP TABLE \"{name}\" CASCADE CONSTRAINTS PURGE'; ELSE RAISE_APPLICATION_ERROR(-20000,'Owned table identity changed; cleanup refused'); END IF; END;"
            );
            clean &= self
                .admin
                .outcome(Request::Query { sql, limit: 1 })
                .is_ok_and(|(reply, _)| matches!(reply, Reply::Results(_)));
        }
        if !clean {
            eprintln!("Oracle owned table cleanup failed or identity changed");
        }
        if !std::thread::panicking() {
            assert!(clean, "exact owned table cleanup must succeed");
        }
    }
}

fn int(value: i64) -> Value {
    Value::Int(value)
}
fn text(value: &str) -> Value {
    Value::Text(value.into())
}
fn pairs(values: &[(i64, &str)]) -> Vec<Vec<String>> {
    values
        .iter()
        .map(|(id, label)| vec![id.to_string(), (*label).to_string()])
        .collect()
}

#[test]
#[ignore = "requires admitted Oracle ODBC driver and disposable loopback server; CREATE TABLE in the login schema"]
fn configured_oracle_catalog_editor_submit_commit_rollback_and_paging() {
    assert_eq!(required("RUSTPY_TEST_ORACLE_DISPOSABLE"), "yes");
    let source: Source = serde_json::from_str(&required("RUSTPY_TEST_ORACLE_SOURCE_JSON"))
        .expect("explicit source JSON must decode");
    assert_eq!(source.dialect, Dialect::Oracle);
    assert_eq!(source.host, "127.0.0.1");
    assert!(source.port > 0 && !source.driver.is_empty() && !source.database.is_empty());
    assert!(source.ssh.is_none() && !source.ddl_only && !source.read_only);
    assert!(!source.user.is_empty());
    let secret = required("RUSTPY_TEST_ORACLE_PASSWORD");
    assert!(!secret.is_empty());
    let owner = source.user.to_ascii_uppercase();

    let nonce = format!(
        "{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
            % 1_000_000_000
    );
    let parent = format!("RUSTPY_OWNED_ED_{nonce}");
    let child = format!("RUSTPY_OWNED_EDC_{nonce}");
    let mut fixture = Fixture {
        admin: Owned::start(1, &source, &secret),
        tables: vec![],
    };
    fixture.create(
        &parent,
        format!("CREATE TABLE {parent} (ID NUMBER(10) NOT NULL PRIMARY KEY, LABEL VARCHAR2(40 CHAR) NOT NULL, AMOUNT NUMBER(10,2) DEFAULT 1.50 NULL); INSERT INTO {parent}(ID,LABEL) VALUES (1,'one'); INSERT INTO {parent}(ID,LABEL) VALUES (2,'two'); INSERT INTO {parent}(ID,LABEL) VALUES (3,'three π')"),
    );
    fixture.create(
        &child,
        format!("CREATE TABLE {child} (ID NUMBER(10) NOT NULL PRIMARY KEY, PARENT_ID NUMBER(10) NOT NULL REFERENCES {parent}(ID))"),
    );
    let table = format!("\"{owner}\".\"{parent}\"");
    let select = format!("SELECT ID,LABEL FROM {table} ORDER BY ID");

    // Introspection: tables, typed columns, primary key and foreign key.
    let mut editor = Owned::start(2, &source, &secret);
    let (Reply::Catalog(objects), _) = editor.request(Request::Catalog) else {
        panic!("actual Oracle catalog required");
    };
    let object = objects
        .iter()
        .find(|o| o.schema == owner && o.name == parent)
        .expect("created table must be introspected");
    assert!(object.kind.eq_ignore_ascii_case("TABLE"));
    let names = object
        .columns
        .iter()
        .map(|c| c.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["ID", "LABEL", "AMOUNT"]);
    let column = |name: &str| object.columns.iter().find(|c| c.name == name).unwrap();
    assert!(column("ID").primary && !column("ID").nullable);
    assert!(!column("LABEL").primary && !column("LABEL").nullable);
    assert!(column("LABEL")
        .data_type
        .to_uppercase()
        .contains("VARCHAR2"));
    assert!(column("AMOUNT").nullable);
    // Oracle's driver reports NUMBER(p,s) through SQLColumns as DECIMAL.
    let amount_type = column("AMOUNT").data_type.to_uppercase();
    assert!(amount_type.contains("NUMBER") || amount_type.contains("DECIMAL"));
    assert!(column("AMOUNT")
        .default
        .as_deref()
        .is_some_and(|value| value.contains("1.50")));
    let referencing = objects
        .iter()
        .find(|o| o.schema == owner && o.name == child)
        .expect("child table must be introspected");
    assert!(
        referencing.keys.iter().any(|k| k.column == "PARENT_ID"
            && k.target.contains(&parent)
            && k.target_column == "ID"),
        "actual foreign key must be introspected"
    );

    // The grid and editor address tables by their unquoted catalog path.
    let grid = object.qualified();
    assert_eq!(grid, format!("{owner}.{parent}"));

    // Object details reach the real dictionary views (primary key index).
    let details = editor.results(Request::ObjectDetails(Box::new(object.clone())));
    let indexes = details
        .iter()
        .find(|(label, result)| label == "Indexes" && result.columns.len() == 4)
        .expect("index details required");
    assert!(indexes
        .1
        .rows
        .iter()
        .any(|row| row[1].display() == "UNIQUE" && row[2].display() == "ID"));
    assert!(details.iter().any(|(label, result)| label == "DDL"
        && result
            .rows
            .iter()
            .flatten()
            .any(|v| v.display().contains("CREATE TABLE") && v.display().contains(&parent))));

    // Data-grid paging uses the dialect's OFFSET/FETCH form; VARCHAR2 text is
    // non-ASCII-safe without a user NLS_LANG.
    let page = Dialect::Oracle.page(&grid, "ID>1", "ID", 1, 1);
    let paged = editor.rows(page);
    assert_eq!(paged.len(), 1);
    assert_eq!(paged[0][0], "3");
    assert_eq!(paged[0][1], "three π");

    // Search over character columns in the login's own schema.
    let found = editor.results(Request::Search("three".into()));
    assert!(found.iter().any(|(label, result)| label.contains(&parent)
        && result
            .rows
            .iter()
            .any(|row| row.iter().any(|v| v.display() == "three π"))));

    // Explain plan is a real DBMS_XPLAN display and leaves the session usable.
    let plan = editor.results(Request::Explain(format!(
        "SELECT LABEL FROM {table} WHERE ID=1"
    )));
    assert!(plan.iter().any(|(_, result)| result
        .rows
        .iter()
        .flatten()
        .any(|v| v.display().contains("Plan hash value"))));
    assert_eq!(
        editor.rows(select.clone()),
        pairs(&[(1, "one"), (2, "two"), (3, "three π")])
    );

    // Console user parameters keep non-ASCII text intact.
    let values = [("p".to_string(), "'π ✓ 𝄞'".to_string())]
        .into_iter()
        .collect();
    let bound =
        bind_user_parameters("SELECT @p AS v, LENGTH(@p) AS n FROM dual", "@", &values).unwrap();
    assert_eq!(editor.rows(bound), [["π ✓ 𝄞".to_string(), "5".to_string()]]);

    // Manual mode: the first statement of a fresh transaction is a Submit.
    assert!(!editor.done(Request::AutoCommit(false)));
    editor.done(Request::Commit);
    assert!(!editor.done(Request::Submit(vec![
        Change::Insert {
            table: grid.clone(),
            values: vec![("ID".into(), int(4)), ("LABEL".into(), text("four"))],
        },
        Change::Update {
            table: grid.clone(),
            key: vec![("ID".into(), int(1))],
            column: "LABEL".into(),
            value: text("uno ✓"),
        },
        Change::Delete {
            table: grid.clone(),
            key: vec![("ID".into(), int(2))],
        },
    ])));
    let pending = pairs(&[(1, "uno ✓"), (3, "three π"), (4, "four")]);
    assert_eq!(editor.rows(select.clone()), pending);

    // A failing batch rolls back only itself, preserving earlier pending work.
    let (failed, auto_commit) = editor
        .outcome(Request::Submit(vec![
            Change::Insert {
                table: grid.clone(),
                values: vec![("ID".into(), int(5)), ("LABEL".into(), text("partial"))],
            },
            Change::Insert {
                table: grid.clone(),
                values: vec![("ID".into(), int(1)), ("LABEL".into(), text("duplicate"))],
            },
        ]))
        .unwrap();
    assert!(
        matches!(failed, Reply::Error(ref e) if e.contains("ORA-00001")),
        "duplicate key must fail"
    );
    assert!(!auto_commit, "failed Submit must preserve manual mode");
    assert_eq!(editor.rows(select.clone()), pending);

    // A second connection reads only committed data (Oracle readers do not block).
    let mut observer = Owned::start(3, &source, &secret);
    let original = pairs(&[(1, "one"), (2, "two"), (3, "three π")]);
    assert_eq!(observer.rows(select.clone()), original);

    // Explicit rollback restores the committed state for both connections.
    editor.done(Request::Rollback);
    assert_eq!(editor.rows(select.clone()), original);
    assert_eq!(observer.rows(select.clone()), original);

    // Enabling auto-commit with pending work is refused, not silently committed.
    editor.done(Request::Submit(vec![Change::Update {
        table: grid.clone(),
        key: vec![("ID".into(), int(2))],
        column: "LABEL".into(),
        value: text("two committed"),
    }]));
    let (refused, auto_commit) = editor.outcome(Request::AutoCommit(true)).unwrap();
    assert!(matches!(refused, Reply::Error(_)));
    assert!(!auto_commit);
    assert_eq!(observer.rows(select.clone()), original);
    editor.done(Request::Commit);
    let committed = pairs(&[(1, "one"), (2, "two committed"), (3, "three π")]);
    assert_eq!(observer.rows(select.clone()), committed);

    // Raw SQL Submit is refused in manual mode (Oracle DDL commits implicitly).
    let (refused, _) = editor
        .outcome(Request::Submit(vec![Change::Sql(format!(
            "DELETE FROM {table}"
        ))]))
        .unwrap();
    assert!(matches!(refused, Reply::Error(_)));
    assert_eq!(observer.rows(select.clone()), committed);

    // Session isolation: SERIALIZABLE keeps a transaction-consistent snapshot
    // across a concurrent committed change; READ COMMITTED sees it.
    editor.done(Request::Isolation("SERIALIZABLE".into()));
    assert_eq!(editor.rows(select.clone()), committed);
    observer.results(Request::Query {
        sql: format!("UPDATE {table} SET LABEL='one later' WHERE ID=1"),
        limit: 1,
    });
    assert_eq!(editor.rows(select.clone()), committed);
    editor.done(Request::Rollback);
    let later = pairs(&[(1, "one later"), (2, "two committed"), (3, "three π")]);
    assert_eq!(editor.rows(select.clone()), later);
    editor.done(Request::Rollback);
    editor.done(Request::Isolation("READ COMMITTED".into()));
    assert_eq!(editor.rows(select.clone()), later);
    observer.results(Request::Query {
        sql: format!("UPDATE {table} SET LABEL='one' WHERE ID=1"),
        limit: 1,
    });
    assert_eq!(editor.rows(select.clone()), committed);
    let (unsupported, _) = editor
        .outcome(Request::Isolation("REPEATABLE READ".into()))
        .unwrap();
    assert!(matches!(unsupported, Reply::Error(_)));
    editor.done(Request::Rollback);

    // Auto-commit Submit is immediately visible to another connection.
    assert!(editor.done(Request::AutoCommit(true)));
    assert!(editor.done(Request::Submit(vec![Change::Delete {
        table: grid.clone(),
        key: vec![("ID".into(), int(3))],
    }])));
    assert_eq!(
        observer.rows(select.clone()),
        pairs(&[(1, "one"), (2, "two committed")])
    );

    // Auto-commit Submit failure leaves no partial batch behind.
    let (failed, auto_commit) = editor
        .outcome(Request::Submit(vec![
            Change::Insert {
                table: grid.clone(),
                values: vec![("ID".into(), int(6)), ("LABEL".into(), text("partial"))],
            },
            Change::Insert {
                table: grid.clone(),
                values: vec![("ID".into(), int(2)), ("LABEL".into(), text("duplicate"))],
            },
        ]))
        .unwrap();
    assert!(matches!(failed, Reply::Error(_)));
    assert!(
        auto_commit,
        "failed auto-commit Submit restores auto-commit"
    );
    assert_eq!(
        observer.rows(select.clone()),
        pairs(&[(1, "one"), (2, "two committed")])
    );
    drop(editor);
    drop(observer);
}
