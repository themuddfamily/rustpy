//! Genuine Oracle OUT/INOUT roundtrip, including selected-driver implicit results.
//! Root must admit an owned disposable loopback server; no ambient defaults.
use rustpy_db::{
    injection::bind_user_parameters,
    routine::invocation_with_prefix,
    session::{Reply, Request, Session},
    workspace::{Dialect, Settings, Source},
};
use std::time::{Duration, Instant};

struct OwnedObject {
    name: String,
    kind: &'static str,
    snapshot: Vec<Vec<String>>,
}
struct Owner {
    session: Session,
    objects: Vec<OwnedObject>,
}
impl Owner {
    fn create(&mut self, name: String, kind: &'static str, sql: String) {
        let Reply::Results(scope) = query(
            &mut self.session,
            "SELECT SYS_CONTEXT('USERENV','CURRENT_SCHEMA'), USER FROM dual".into(),
        ) else {
            panic!("actual Oracle schema authority required");
        };
        assert!(
            scope.iter().any(|(_, result)| result
                .rows
                .first()
                .is_some_and(|row| row.len() == 2 && row[0].display() == row[1].display())),
            "unqualified fixture DDL must target the actual authenticated owner's schema"
        );
        assert!(
            snapshot(&mut self.session, &name, kind)
                .expect("pre-create identity query")
                .is_empty(),
            "fixture must never replace an existing object"
        );
        query(&mut self.session, sql);
        // Register immediately after successful CREATE, before assertions about
        // catalog, capabilities or results can unwind. Object ID, DDL timestamp
        // and complete stored source identify the exact created object.
        let proof =
            snapshot(&mut self.session, &name, kind).expect("capture created object identity");
        assert!(!proof.is_empty(), "created object identity must exist");
        self.objects.push(OwnedObject {
            name,
            kind,
            snapshot: proof,
        });
    }
    fn cleanup(&mut self) -> bool {
        let mut clean = true;
        for object in self.objects.drain(..).rev() {
            match snapshot(&mut self.session, &object.name, object.kind) {
                Ok(current) if current == object.snapshot => {
                    let reply = request_outcome(
                        &mut self.session,
                        Request::Query {
                            sql: format!(
                                "DROP {} {}",
                                object.kind,
                                Dialect::Oracle.quote(&object.name)
                            ),
                            limit: 20,
                        },
                    );
                    clean &= matches!(reply, Ok(Reply::Results(_)))
                        && snapshot(&mut self.session, &object.name, object.kind)
                            .is_ok_and(|rows| rows.is_empty());
                }
                Ok(current) if current.is_empty() => {}
                _ => clean = false, // Never drop a replaced/unverified object.
            }
        }
        clean
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        // Roll back any deliberate read-only/failed transaction before DDL.
        let _ = request_outcome(&mut self.session, Request::Rollback);
        let clean = self.cleanup();
        self.session.stop();
        let deadline = Instant::now() + Duration::from_secs(30);
        while self.session.alive() {
            self.session.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
        if !clean {
            eprintln!("Oracle fixture cleanup could not verify/remove its exact owned object; no unverified object was dropped");
        }
        if !std::thread::panicking() {
            assert!(clean, "exact owned Oracle object cleanup must succeed");
            assert!(
                Instant::now() < deadline,
                "worker retirement exceeded deadline; ownership retained through retirement"
            );
        }
    }
}
fn snapshot(
    session: &mut Session,
    name: &str,
    kind: &str,
) -> Result<Vec<Vec<String>>, &'static str> {
    // Both identifiers are fixture-generated, never user/provider SQL text.
    if !name
        .bytes()
        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        || !matches!(kind, "PROCEDURE" | "TABLE")
    {
        return Err("invalid fixture identity");
    }
    let sql = format!("SELECT TO_CHAR(o.OBJECT_ID), TO_CHAR(o.CREATED,'YYYY-MM-DD HH24:MI:SS'), TO_CHAR(o.LAST_DDL_TIME,'YYYY-MM-DD HH24:MI:SS'), o.STATUS, COALESCE(s.TEXT,' ') FROM USER_OBJECTS o LEFT JOIN USER_SOURCE s ON s.NAME=o.OBJECT_NAME AND s.TYPE=o.OBJECT_TYPE WHERE o.OBJECT_NAME='{name}' AND o.OBJECT_TYPE='{kind}' ORDER BY s.LINE");
    match request_outcome(session, Request::Query { sql, limit: 1000 })? {
        Reply::Results(sets) => {
            let mut rows = Vec::new();
            for (_, set) in sets {
                if set.truncated {
                    return Err("owned source identity exceeded capture bound");
                }
                rows.extend(
                    set.rows
                        .into_iter()
                        .map(|row| row.into_iter().map(|v| v.display()).collect()),
                );
            }
            Ok(rows)
        }
        _ => Err("object identity query failed"),
    }
}
fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("explicit {name} admission required"))
}
fn receive(session: &mut Session, connected: bool) -> Result<Reply, &'static str> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut response = None;
    let mut state = false;
    loop {
        for reply in session.poll() {
            match reply {
                Reply::Retired => return Err("Oracle worker retired prematurely"),
                Reply::Connected if connected => return Ok(Reply::Connected),
                Reply::State { .. } => state = true,
                other => response = Some(other),
            }
        }
        if state {
            if let Some(response) = response.take() {
                return Ok(response);
            }
        }
        if Instant::now() >= deadline {
            return Err("actual Oracle request timed out");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn request_outcome(session: &mut Session, value: Request) -> Result<Reply, &'static str> {
    session
        .send(value)
        .map_err(|_| "Oracle owner did not accept request")?;
    receive(session, false)
}
fn request(session: &mut Session, value: Request) -> Reply {
    let reply = request_outcome(session, value).expect("actual Oracle request completed");
    assert!(
        !matches!(reply, Reply::Error(_)),
        "actual Oracle request failed; driver diagnostic suppressed to protect credentials"
    );
    reply
}
fn query(session: &mut Session, sql: String) -> Reply {
    request(session, Request::Query { sql, limit: 20 })
}

#[test]
#[ignore = "requires admitted Oracle ODBC driver with genuine implicit results and disposable loopback database"]
fn configured_oracle_catalog_out_inout_and_implicit_result_roundtrip() {
    assert_eq!(required("RUSTPY_TEST_ORACLE_DISPOSABLE"), "yes");
    let source: Source = serde_json::from_str(&required("RUSTPY_TEST_ORACLE_SOURCE_JSON"))
        .expect("explicit source JSON");
    assert_eq!(source.dialect, Dialect::Oracle);
    assert_eq!(source.host, "127.0.0.1");
    assert!(source.port > 0 && !source.driver.is_empty() && !source.database.is_empty());
    assert!(source.ssh.is_none() && !source.ddl_only);
    let secret = required("RUSTPY_TEST_ORACLE_PASSWORD");
    assert!(!secret.is_empty());
    let mut owner = Owner {
        session: Session::start(
            1,
            source,
            "owned Oracle outputs".into(),
            secret,
            Settings {
                timeout_seconds: 10,
                ..Default::default()
            },
        )
        .expect("start worker"),
        objects: Vec::new(),
    };
    receive(&mut owner.session, true).expect("actual Oracle connected");
    // Unique fixture-only identity; root owns and destroys the whole database.
    let name = format!("RUSTPY_OUT_{}", std::process::id());
    owner.create(name.clone(), "PROCEDURE", format!("CREATE PROCEDURE {name}(n IN NUMBER, answer OUT NUMBER, text_value IN OUT VARCHAR2) AS BEGIN answer := n + 1; text_value := text_value || ':owned'; END;"));
    let Reply::Catalog(objects) = request(&mut owner.session, Request::Catalog) else {
        panic!("actual Oracle catalog required");
    };
    let object = objects
        .iter()
        .find(|object| object.name == name)
        .expect("created procedure must be introspected");
    let routine = object
        .routine
        .as_ref()
        .expect("typed real Oracle arguments");
    assert!(
        routine.oracle_implicit_results.is_some(),
        "selected server AND selected ODBC driver must actually return the capability sentinel"
    );
    let call = invocation_with_prefix(
        Dialect::Oracle,
        &object.schema,
        &object.name,
        routine,
        false,
        "@",
    )
    .unwrap();
    assert_eq!(call.parameters, ["arg_1", "arg_3"]);
    let values = [
        ("arg_1".into(), "41".into()),
        ("arg_3".into(), "'seed'".into()),
    ]
    .into_iter()
    .collect();
    let sql = bind_user_parameters(&call.sql, "@", &values).unwrap();
    let Reply::Results(results) = query(&mut owner.session, sql) else {
        panic!("actual implicit output results required");
    };
    let result = results
        .iter()
        .map(|(_, result)| result)
        .find(|result| result.columns == ["arg_2", "arg_3"])
        .expect("driver must return real output columns");
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0][0].display(), "42");
    assert_eq!(result.rows[0][1].display(), "seed:owned");
    let mut legacy = routine.clone();
    legacy.oracle_implicit_results = None;
    assert!(invocation_with_prefix(
        Dialect::Oracle,
        &object.schema,
        &object.name,
        &legacy,
        false,
        "@"
    )
    .is_err());
    // Oracle read-only is transaction-scoped, not a persistent session toggle.
    let table = format!("RUSTPY_RO_{}", std::process::id());
    owner.create(
        table.clone(),
        "TABLE",
        format!("CREATE TABLE {table}(n NUMBER)"),
    );
    query(
        &mut owner.session,
        format!("INSERT INTO {table} VALUES (42)"),
    );
    request(&mut owner.session, Request::Commit);
    request(&mut owner.session, Request::AutoCommit(false));
    // A read-only snapshot may predate the just-created table's dictionary
    // SCN (ORA-01466); that is genuine Oracle behaviour, so retry briefly.
    let settle = Instant::now() + Duration::from_secs(30);
    let rows = loop {
        query(&mut owner.session, "SET TRANSACTION READ ONLY".into());
        match request_outcome(
            &mut owner.session,
            Request::Query {
                sql: format!("SELECT n FROM {table}"),
                limit: 20,
            },
        )
        .expect("read-only SELECT completes")
        {
            Reply::Results(rows) => break rows,
            Reply::Error(error) if error.contains("ORA-01466") && Instant::now() < settle => {
                request(&mut owner.session, Request::Rollback);
                std::thread::sleep(Duration::from_millis(500));
            }
            _ => panic!("read-only SELECT must succeed (diagnostic suppressed)"),
        }
    };
    assert!(rows.iter().any(|(_, result)| result
        .rows
        .first()
        .and_then(|row| row.first())
        .is_some_and(|value| value.display() == "42")));
    let refused = request_outcome(
        &mut owner.session,
        Request::Query {
            sql: format!("INSERT INTO {table} VALUES (43)"),
            limit: 20,
        },
    )
    .expect("actual read-only refusal must complete");
    assert!(matches!(refused, Reply::Error(ref error) if error.contains("ORA-01456")),
        "Oracle must report its genuine read-only transaction write refusal (diagnostic suppressed)");
    request(&mut owner.session, Request::Rollback);
    request(&mut owner.session, Request::AutoCommit(true));
    let Reply::Results(rows) = query(&mut owner.session, format!("SELECT COUNT(*) FROM {table}"))
    else {
        panic!("readback required");
    };
    assert!(rows.iter().any(|(_, result)| result
        .rows
        .first()
        .and_then(|row| row.first())
        .is_some_and(|value| value.display() == "1")));
    assert!(
        owner.cleanup(),
        "exact owned procedure/table must be verified and removed"
    );
}
