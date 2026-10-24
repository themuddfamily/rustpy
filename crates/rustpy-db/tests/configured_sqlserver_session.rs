//! Genuine SQL Server session, introspection and data-editor acceptance:
//! table/column/key catalog, object details, paging, full-text search, explain
//! plan, manual-mode Submit with savepoint isolation, commit/rollback visibility
//! from a second connection, auto-commit Submit, and physical retirement.
//! Requires an explicitly admitted disposable loopback server and ODBC driver;
//! never provisions or skips.
use rustpy_db::{
    injection::bind_user_parameters,
    session::{Reply, Request, Session},
    workspace::{Change, Dialect, Settings, Source},
    Value,
};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(30);

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
                "actual SQL Server session".into(),
                secret.into(),
                Settings {
                    timeout_seconds: 10,
                    ..Default::default()
                },
            )
            .expect("start SQL Server worker"),
        );
        let end = Instant::now() + WAIT;
        loop {
            for reply in owned.0.poll() {
                match reply {
                    Reply::Connected => return owned,
                    Reply::Error(_) => panic!(
                        "actual SQL Server connection failed; diagnostic suppressed to protect credentials"
                    ),
                    Reply::Retired => panic!("worker retired before connection"),
                    _ => {}
                }
            }
            assert!(
                Instant::now() < end,
                "actual SQL Server connection timed out"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn outcome(&mut self, request: Request) -> Result<(Reply, bool), &'static str> {
        self.0
            .send(request)
            .map_err(|_| "idle retained SQL Server session must accept request")?;
        let end = Instant::now() + WAIT;
        let mut reply = None;
        loop {
            for value in self.0.poll() {
                match value {
                    Reply::Retired => return Err("SQL Server worker retired prematurely"),
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
                return Err("actual SQL Server request timed out");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn request(&mut self, request: Request) -> (Reply, bool) {
        let answer = self
            .outcome(request)
            .expect("actual SQL Server request completes");
        assert!(
            !matches!(answer.0, Reply::Error(_)),
            "actual SQL Server request failed; driver diagnostic suppressed to protect credentials"
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
            panic!("actual SQL Server results required");
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
            assert!(!self.0.alive(), "SQL Server worker must physically retire");
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
            "SELECT ISNULL(CONVERT(varchar(20),OBJECT_ID(N'[dbo].[{name}]',N'U')),'')"
        ));
        rows[0][0].clone()
    }
    fn create(&mut self, name: &str, ddl: String) {
        assert!(
            name.starts_with("rustpy_owned_editor_")
                && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        );
        assert!(
            self.object_id(name).is_empty(),
            "fixture name must be unused"
        );
        self.admin.results(Request::Query { sql: ddl, limit: 1 });
        let id = self.object_id(name);
        assert!(
            id.parse::<i64>().is_ok(),
            "created table must be identified"
        );
        self.tables.push((name.into(), id));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let mut clean = true;
        while let Some((name, id)) = self.tables.pop() {
            let sql = format!(
                "IF OBJECT_ID(N'[dbo].[{name}]',N'U')={id} DROP TABLE [dbo].[{name}] ELSE THROW 51000,N'Owned table identity changed; cleanup refused',1;"
            );
            clean &= self
                .admin
                .outcome(Request::Query { sql, limit: 1 })
                .is_ok_and(|(reply, _)| matches!(reply, Reply::Results(_)));
        }
        if !clean {
            eprintln!("SQL Server owned table cleanup failed or identity changed");
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
#[ignore = "requires admitted SQL Server ODBC driver and disposable loopback server; CREATE TABLE in dbo"]
fn configured_sqlserver_catalog_editor_submit_commit_rollback_and_paging() {
    assert_eq!(required("RUSTPY_TEST_SQLSERVER_DISPOSABLE"), "yes");
    let source: Source = serde_json::from_str(&required("RUSTPY_TEST_SQLSERVER_SOURCE_JSON"))
        .expect("explicit source JSON must decode");
    assert_eq!(source.dialect, Dialect::SqlServer);
    assert_eq!(source.host, "127.0.0.1");
    assert!(source.port > 0 && !source.driver.is_empty() && !source.database.is_empty());
    assert!(source.ssh.is_none() && !source.ddl_only && !source.read_only);
    let secret = required("RUSTPY_TEST_SQLSERVER_PASSWORD");
    assert!(!secret.is_empty());

    let nonce = format!(
        "{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let parent = format!("rustpy_owned_editor_{nonce}");
    let child = format!("rustpy_owned_editor_child_{nonce}");
    let mut fixture = Fixture {
        admin: Owned::start(1, &source, &secret),
        tables: vec![],
    };
    fixture.create(
        &parent,
        format!("CREATE TABLE [dbo].[{parent}] (id int NOT NULL PRIMARY KEY, label nvarchar(40) NOT NULL, amount decimal(10,2) NULL DEFAULT 1.50); INSERT INTO [dbo].[{parent}](id,label) VALUES (1,N'one'),(2,N'two'),(3,N'three π')"),
    );
    fixture.create(
        &child,
        format!("CREATE TABLE [dbo].[{child}] (id int NOT NULL PRIMARY KEY, parent_id int NOT NULL REFERENCES [dbo].[{parent}](id))"),
    );
    let table = format!("[dbo].[{parent}]");
    let select = format!("SELECT id,label FROM {table} ORDER BY id");

    // Introspection: tables, typed columns, primary key and foreign key.
    let mut editor = Owned::start(2, &source, &secret);
    let (Reply::Catalog(objects), _) = editor.request(Request::Catalog) else {
        panic!("actual SQL Server catalog required");
    };
    let object = objects
        .iter()
        .find(|o| o.schema == "dbo" && o.name == parent)
        .expect("created table must be introspected");
    assert!(object.kind.eq_ignore_ascii_case("TABLE"));
    let names = object
        .columns
        .iter()
        .map(|c| c.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["id", "label", "amount"]);
    let column = |name: &str| object.columns.iter().find(|c| c.name == name).unwrap();
    assert!(column("id").primary && !column("id").nullable);
    assert!(!column("label").primary && !column("label").nullable);
    assert!(column("label")
        .data_type
        .to_lowercase()
        .contains("nvarchar"));
    assert!(column("amount").nullable);
    assert!(column("amount")
        .data_type
        .to_lowercase()
        .contains("decimal"));
    assert!(column("amount")
        .default
        .as_deref()
        .is_some_and(|value| value.contains("1.50")));
    let referencing = objects
        .iter()
        .find(|o| o.schema == "dbo" && o.name == child)
        .expect("child table must be introspected");
    assert!(
        referencing.keys.iter().any(|k| k.column == "parent_id"
            && k.target.contains(&parent)
            && k.target_column == "id"),
        "actual foreign key must be introspected"
    );

    // The grid and editor address tables by their unquoted catalog path.
    let grid = object.qualified();
    assert_eq!(grid, format!("{}.dbo.{parent}", source.database));

    // Object details reach the real catalog views (index on the primary key).
    let details = editor.results(Request::ObjectDetails(Box::new(object.clone())));
    let indexes = details
        .iter()
        .find(|(label, result)| label == "Indexes" && !result.columns.is_empty())
        .expect("index details required");
    assert!(indexes
        .1
        .rows
        .iter()
        .any(|row| row.iter().any(|value| value.display() == "id")));

    // Data-grid paging uses the dialect's OFFSET/FETCH form.
    let page = Dialect::SqlServer.page(&grid, "id>1", "id", 1, 1);
    let paged = editor.rows(page);
    assert_eq!(paged.len(), 1);
    assert_eq!(paged[0][0], "3");
    assert_eq!(paged[0][1], "three π");

    // Full-text search over character columns.
    let found = editor.results(Request::Search("three".into()));
    assert!(found.iter().any(|(label, result)| label.contains(&parent)
        && result
            .rows
            .iter()
            .any(|row| row.iter().any(|v| v.display() == "three π"))));

    // Explain plan is real SHOWPLAN XML and leaves the session executable.
    let plan = editor.results(Request::Explain(format!(
        "SELECT label FROM {table} WHERE id=1"
    )));
    assert!(plan.iter().any(|(_, result)| result
        .rows
        .iter()
        .flatten()
        .any(|v| v.display().contains("ShowPlanXML"))));
    assert_eq!(
        editor.rows(select.clone()),
        pairs(&[(1, "one"), (2, "two"), (3, "three π")])
    );

    // Console user parameters: national literals keep non-ASCII text intact.
    let values = [("p".to_string(), "N'π ✓ 𝄞'".to_string())]
        .into_iter()
        .collect();
    let bound = bind_user_parameters("SELECT @p AS v, UNICODE(@p) AS u", "@", &values).unwrap();
    assert_eq!(
        editor.rows(bound),
        [["π ✓ 𝄞".to_string(), "960".to_string()]]
    );

    // Manual mode: the first statement of a fresh transaction is a Submit.
    assert!(!editor.done(Request::AutoCommit(false)));
    editor.done(Request::Commit);
    assert!(!editor.done(Request::Submit(vec![
        Change::Insert {
            table: grid.clone(),
            values: vec![("id".into(), int(4)), ("label".into(), text("four"))],
        },
        Change::Update {
            table: grid.clone(),
            key: vec![("id".into(), int(1))],
            column: "label".into(),
            value: text("uno ✓"),
        },
        Change::Delete {
            table: grid.clone(),
            key: vec![("id".into(), int(2))],
        },
    ])));
    let pending = pairs(&[(1, "uno ✓"), (3, "three π"), (4, "four")]);
    assert_eq!(editor.rows(select.clone()), pending);

    // A failing batch rolls back only itself, preserving earlier pending work.
    let (failed, auto_commit) = editor
        .outcome(Request::Submit(vec![
            Change::Insert {
                table: grid.clone(),
                values: vec![("id".into(), int(5)), ("label".into(), text("partial"))],
            },
            Change::Insert {
                table: grid.clone(),
                values: vec![("id".into(), int(1)), ("label".into(), text("duplicate"))],
            },
        ]))
        .unwrap();
    assert!(matches!(failed, Reply::Error(_)), "duplicate key must fail");
    assert!(!auto_commit, "failed Submit must preserve manual mode");
    assert_eq!(editor.rows(select.clone()), pending);

    // A second connection never sees uncommitted work (locked rows skipped).
    let mut observer = Owned::start(3, &source, &secret);
    let observed = format!("SELECT id,label FROM {table} WITH (READPAST) ORDER BY id");
    assert_eq!(observer.rows(observed.clone()), pairs(&[(3, "three π")]));

    // Explicit rollback restores the committed state for both connections.
    editor.done(Request::Rollback);
    let original = pairs(&[(1, "one"), (2, "two"), (3, "three π")]);
    assert_eq!(editor.rows(select.clone()), original);
    assert_eq!(observer.rows(observed.clone()), original);

    // Enabling auto-commit with pending work is refused, not silently committed.
    editor.done(Request::Submit(vec![Change::Update {
        table: grid.clone(),
        key: vec![("id".into(), int(2))],
        column: "label".into(),
        value: text("two committed"),
    }]));
    let (refused, auto_commit) = editor.outcome(Request::AutoCommit(true)).unwrap();
    assert!(matches!(refused, Reply::Error(_)));
    assert!(!auto_commit);
    editor.done(Request::Commit);
    let committed = pairs(&[(1, "one"), (2, "two committed"), (3, "three π")]);
    assert_eq!(observer.rows(select.clone()), committed);

    // Auto-commit Submit is immediately visible to another connection.
    assert!(editor.done(Request::AutoCommit(true)));
    assert!(editor.done(Request::Submit(vec![Change::Delete {
        table: grid.clone(),
        key: vec![("id".into(), int(3))],
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
                values: vec![("id".into(), int(6)), ("label".into(), text("partial"))],
            },
            Change::Insert {
                table: grid.clone(),
                values: vec![("id".into(), int(2)), ("label".into(), text("duplicate"))],
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

    // Isolation level changes are accepted by the real server.
    editor.done(Request::Isolation("SERIALIZABLE".into()));
    assert_eq!(
        editor.rows("SELECT CONVERT(varchar(4),transaction_isolation_level) FROM sys.dm_exec_sessions WHERE session_id=@@SPID".into()),
        [["4".to_string()]]
    );
    drop(editor);
    drop(observer);
}
