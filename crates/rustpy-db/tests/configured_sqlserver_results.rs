//! Genuine SQL Server catalog/output and leading/interleaved row-count results.
//! Requires an explicitly admitted disposable loopback server and ODBC driver.
use rustpy_db::{
    injection::bind_user_parameters,
    routine::{invocation_with_prefix, ParameterMode, RoutineKind},
    session::{Reply, Request, Session},
    workspace::{Dialect, Settings, Source},
    Value,
};
use std::time::{Duration, Instant};

struct Proof {
    name: String,
    rows: Vec<Vec<String>>,
}
struct Owner {
    session: Session,
    connected: bool,
    procedure: Option<Proof>,
}
impl Drop for Owner {
    fn drop(&mut self) {
        let clean = if self.connected {
            // CREATE PROCEDURE is transactional on SQL Server. Rollback also
            // covers failure after CREATE but before identity registration.
            let rollback = outcome(&mut self.session, Request::Rollback)
                .is_ok_and(|answer| matches!(answer.reply, Reply::Done(_)));
            let mode = rollback
                && outcome(&mut self.session, Request::AutoCommit(true))
                    .is_ok_and(|answer| matches!(answer.reply, Reply::Done(_)));
            let object = mode
                && self.procedure.take().is_none_or(|proof| {
                    let Ok(current) = snapshot(&mut self.session, &proof.name) else {
                        return false;
                    };
                    if current.is_empty() {
                        return true;
                    }
                    if current != proof.rows {
                        return false;
                    }
                    outcome(&mut self.session, query_request(guarded_drop(&proof)))
                        .is_ok_and(|answer| matches!(answer.reply, Reply::Results(_)))
                        && snapshot(&mut self.session, &proof.name)
                            .is_ok_and(|rows| rows.is_empty())
                });
            rollback && mode && object
        } else {
            true
        };
        self.session.stop();
        let end = Instant::now() + Duration::from_secs(20);
        while self.session.alive() {
            self.session.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
        if !clean {
            eprintln!("SQL Server owned procedure cleanup failed or identity changed; no unverified object was dropped");
        }
        if !std::thread::panicking() {
            assert!(clean, "exact owned procedure cleanup must succeed");
            assert!(
                Instant::now() < end,
                "worker retirement exceeded deadline; ownership retained until physical retirement"
            );
        }
    }
}
struct Answer {
    reply: Reply,
    auto_commit: bool,
    read_only: bool,
}
fn required(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("explicit {key} required"))
}
fn outcome(session: &mut Session, request: Request) -> Result<Answer, &'static str> {
    session
        .send(request)
        .map_err(|_| "idle retained SQL Server owner must accept request")?;
    let end = Instant::now() + Duration::from_secs(30);
    let mut reply = None;
    let mut state = None;
    loop {
        for value in session.poll() {
            match value {
                Reply::Retired => return Err("SQL Server worker retired prematurely"),
                Reply::State {
                    auto_commit,
                    read_only,
                } => state = Some((auto_commit, read_only)),
                value => reply = Some(value),
            }
        }
        if let Some((auto_commit, read_only)) = state {
            if let Some(reply) = reply.take() {
                return Ok(Answer {
                    reply,
                    auto_commit,
                    read_only,
                });
            }
        }
        if Instant::now() >= end {
            return Err("actual SQL Server request timed out");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn request(session: &mut Session, request: Request) -> Answer {
    let answer = outcome(session, request).expect("actual SQL Server request completes");
    assert!(
        !matches!(answer.reply, Reply::Error(_)),
        "actual SQL Server request failed; driver diagnostic suppressed to protect credentials"
    );
    answer
}
fn query_request(sql: String) -> Request {
    Request::Query { sql, limit: 100 }
}
fn query(session: &mut Session, sql: String) -> Vec<rustpy_db::QueryResult> {
    let Reply::Results(results) = request(session, query_request(sql)).reply else {
        panic!("actual SQL Server results required");
    };
    results.into_iter().map(|(_, result)| result).collect()
}
fn snapshot(session: &mut Session, name: &str) -> Result<Vec<Vec<String>>, &'static str> {
    if !name.starts_with("rustpy_owned_outputs_")
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err("invalid fixture-owned procedure identity");
    }
    let sql = format!("SELECT CONVERT(varchar(20),o.object_id), CONVERT(varchar(33),o.create_date,126), CONVERT(varchar(33),o.modify_date,126), OBJECT_DEFINITION(o.object_id), CONVERT(varchar(64),HASHBYTES('SHA2_256',CONVERT(varbinary(max),OBJECT_DEFINITION(o.object_id))),2) FROM sys.objects o JOIN sys.schemas s ON s.schema_id=o.schema_id WHERE s.name=N'dbo' AND o.name=N'{name}' AND o.type='P'");
    let answer = outcome(session, query_request(sql))?;
    let Reply::Results(results) = answer.reply else {
        return Err("owned procedure identity query failed");
    };
    let mut rows = Vec::new();
    for (_, result) in results {
        if result.truncated {
            return Err("owned procedure identity was truncated");
        }
        rows.extend(
            result
                .rows
                .into_iter()
                .map(|row| row.into_iter().map(|value| value.display()).collect()),
        );
    }
    Ok(rows)
}
fn guarded_drop(proof: &Proof) -> String {
    let row = &proof.rows[0];
    let created = row[1].replace('\'', "''");
    let modified = row[2].replace('\'', "''");
    let body = format!("IF OBJECT_ID(N'dbo.{}',N'P')={} AND EXISTS(SELECT 1 FROM sys.objects WHERE object_id={} AND CONVERT(varchar(33),create_date,126)=N'{created}' AND CONVERT(varchar(33),modify_date,126)=N'{modified}') AND CONVERT(varchar(64),HASHBYTES('SHA2_256',CONVERT(varbinary(max),OBJECT_DEFINITION({}))),2)=N'{}' BEGIN DROP PROCEDURE [dbo].[{}]; END ELSE THROW 51000,N'Owned routine identity changed; cleanup refused',1;", proof.name, row[0], row[0], row[0], row[4], proof.name);
    format!("EXEC sys.sp_executesql N'{}'", body.replace('\'', "''"))
}
fn proof(session: &mut Session, name: &str) -> Proof {
    let rows = snapshot(session, name).expect("capture exact created procedure");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 5);
    assert!(rows[0][0].parse::<u32>().is_ok());
    assert!(!rows[0][3].is_empty());
    assert!(rows[0][4].len() == 64 && rows[0][4].bytes().all(|byte| byte.is_ascii_hexdigit()));
    Proof {
        name: name.into(),
        rows,
    }
}
fn rowcount_chain(results: &[rustpy_db::QueryResult]) {
    let rows = results
        .iter()
        .filter(|result| !result.columns.is_empty())
        .collect::<Vec<_>>();
    assert!(
        rows.len() >= 2,
        "both SELECT results must survive row counts"
    );
    assert_eq!(rows[0].rows[0][0].display(), "41");
    assert_eq!(rows[1].rows[0][0].display(), "42");
    assert!(results
        .iter()
        .any(|result| result.columns.is_empty() && result.affected == Some(1)));
}

#[test]
#[ignore = "requires admitted SQL Server ODBC driver and disposable loopback server; CREATE PROCEDURE and VIEW DEFINITION in dbo"]
fn configured_sqlserver_leading_and_interleaved_rowcounts_preserve_later_results() {
    assert_eq!(required("RUSTPY_TEST_SQLSERVER_DISPOSABLE"), "yes");
    let source: Source = serde_json::from_str(&required("RUSTPY_TEST_SQLSERVER_SOURCE_JSON"))
        .expect("explicit source JSON must decode");
    assert_eq!(source.dialect, Dialect::SqlServer);
    assert_eq!(source.host, "127.0.0.1");
    assert!(
        source.port > 0
            && !source.driver.is_empty()
            && !source.database.is_empty()
            && !source.user.is_empty()
    );
    assert!(source.ssh.is_none() && !source.ddl_only && !source.read_only);
    let secret = required("RUSTPY_TEST_SQLSERVER_PASSWORD");
    assert!(!secret.is_empty());
    let database = source.database.clone();
    let mut owner = Owner {
        session: Session::start(
            1,
            source,
            "actual SQL Server output bindings".into(),
            secret,
            Settings {
                timeout_seconds: 10,
                ..Default::default()
            },
        )
        .expect("start owner"),
        connected: false,
        procedure: None,
    };
    let end = Instant::now() + Duration::from_secs(30);
    while !owner.connected {
        for reply in owner.session.poll() {
            match reply {
                Reply::Connected => owner.connected = true,
                Reply::Error(_) => panic!("actual SQL Server connection failed; diagnostic suppressed to protect credentials"),
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
    assert_eq!(
        query(&mut owner.session, "SELECT DB_NAME()".into())[0].rows[0][0].display(),
        database
    );
    let original = query(&mut owner.session, "EXEC sys.sp_executesql N'SET NOCOUNT OFF; CREATE TABLE #rustpy_results (n int); INSERT INTO #rustpy_results VALUES (41); SELECT n AS first_result FROM #rustpy_results; UPDATE #rustpy_results SET n=n+1; SELECT n AS last_result FROM #rustpy_results; DROP TABLE #rustpy_results'".into());
    assert_eq!(
        original
            .iter()
            .filter(|result| !result.columns.is_empty())
            .count(),
        2
    );
    rowcount_chain(&original);

    // SQL Server has no persistent read-only switch in this public provider
    // policy. Refusal must preserve the actual state, never pretend it enabled.
    let refused = outcome(&mut owner.session, Request::ReadOnly(true)).unwrap();
    assert!(matches!(refused.reply, Reply::Error(_)));
    assert!(!refused.read_only && refused.auto_commit);

    let name = format!(
        "rustpy_owned_outputs_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    assert!(snapshot(&mut owner.session, &name).unwrap().is_empty());
    request(&mut owner.session, Request::AutoCommit(false));
    let definition = format!("CREATE PROCEDURE [dbo].[{name}] @input int, @amount decimal(18,4) OUTPUT, @text nvarchar(120) OUTPUT, @bytes varbinary(16) OUTPUT AS BEGIN SET NOCOUNT OFF; CREATE TABLE #rustpy_routine(n int); INSERT INTO #rustpy_routine VALUES(@input); SELECT n AS before_update FROM #rustpy_routine; UPDATE #rustpy_routine SET n=n+1; SELECT n AS after_update FROM #rustpy_routine; SET @amount=@amount+CAST(0.0001 AS decimal(18,4)); SET @text=@text+N':π-owned'; SET @bytes=@bytes+0x00FF; DROP TABLE #rustpy_routine; END;");
    query(&mut owner.session, definition.clone());
    // Register before COMMIT: every earlier failure rolls back transactional DDL.
    owner.procedure = Some(proof(&mut owner.session, &name));
    request(&mut owner.session, Request::Commit);
    request(&mut owner.session, Request::AutoCommit(true));
    let Reply::Catalog(objects) = request(&mut owner.session, Request::Catalog).reply else {
        panic!("actual SQL Server catalog required");
    };
    let object = objects
        .iter()
        .find(|object| object.schema == "dbo" && object.name == name)
        .expect("created procedure must be introspected");
    let routine = object
        .routine
        .as_ref()
        .expect("actual typed procedure metadata");
    assert_eq!(routine.kind, RoutineKind::Procedure);
    assert_eq!(
        routine.specific_name,
        owner.procedure.as_ref().unwrap().rows[0][0]
    );
    assert_eq!(
        routine
            .parameters
            .iter()
            .map(|arg| arg.mode)
            .collect::<Vec<_>>(),
        [
            ParameterMode::In,
            ParameterMode::InOut,
            ParameterMode::InOut,
            ParameterMode::InOut
        ]
    );
    assert_eq!(routine.parameters[1].data_type, "decimal(18,4)");
    assert_eq!(routine.parameters[2].data_type, "nvarchar(120)");
    assert_eq!(routine.parameters[3].data_type, "varbinary(16)");
    let invocation = invocation_with_prefix(
        Dialect::SqlServer,
        &object.schema,
        &object.name,
        routine,
        false,
        "@",
    )
    .unwrap();
    assert_eq!(invocation.parameters, ["arg_1", "arg_2", "arg_3", "arg_4"]);
    let text = "long-input-".repeat(8);
    let values = [
        ("arg_1".into(), "41".into()),
        ("arg_2".into(), "12345678901234.0001".into()),
        ("arg_3".into(), format!("'{text}'")),
        ("arg_4".into(), "'AB'".into()),
    ]
    .into_iter()
    .collect();
    let sql = bind_user_parameters(&invocation.sql, "@", &values).unwrap();
    let results = query(&mut owner.session, sql);
    rowcount_chain(&results);
    assert_eq!(
        results
            .iter()
            .filter(|result| !result.columns.is_empty())
            .count(),
        3
    );
    let output = results
        .iter()
        .find(|result| result.columns == ["arg_2", "arg_3", "arg_4"])
        .expect("real OUTPUT values must reach final result");
    assert_eq!(output.rows.len(), 1);
    assert_eq!(output.rows[0][0].display(), "12345678901234.0002");
    assert_eq!(output.rows[0][1].display(), format!("{text}:π-owned"));
    assert_eq!(output.rows[0][2], Value::Binary(vec![b'A', b'B', 0, 255]));

    // Our acknowledged ALTER changes definition authority. The old captured
    // identity/hash must refuse DROP, then an explicit new capture can clean up.
    let altered = definition
        .replacen("CREATE PROCEDURE", "ALTER PROCEDURE", 1)
        .replacen("CAST(0.0001 AS", "CAST(0.0002 AS", 1);
    query(
        &mut owner.session,
        format!("EXEC sys.sp_executesql N'{}'", altered.replace('\'', "''")),
    );
    let newer = proof(&mut owner.session, &name);
    let prior = owner.procedure.as_ref().unwrap();
    assert_eq!(newer.rows[0][0], prior.rows[0][0]);
    assert_ne!(newer.rows[0][4], prior.rows[0][4]);
    let rejected = outcome(&mut owner.session, query_request(guarded_drop(prior))).unwrap();
    assert!(
        matches!(rejected.reply, Reply::Error(ref error) if error.contains("Owned routine identity changed"))
    );
    assert_eq!(snapshot(&mut owner.session, &name).unwrap(), newer.rows);
    owner.procedure = Some(newer);
}
