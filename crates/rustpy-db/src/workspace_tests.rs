use super::*;
use crate::workspace::*;
#[test]
fn named_odbc_providers_escape_connection_attributes_and_reject_persisted_secrets() {
    for dialect in Dialect::ALL
        .iter()
        .copied()
        .filter(|d| *d != Dialect::Sqlite)
    {
        let source = Source {
            dialect,
            driver: "installed } driver".into(),
            host: "db.example".into(),
            database: "warehouse".into(),
            ..Default::default()
        };
        let connection = source.connection_string("secret};UID=intruder").unwrap();
        assert!(connection.contains("Driver={installed }} driver};"));
        assert!(connection.contains("PWD={secret}};UID=intruder};"));
        assert!(!serde_json::to_string(&source).unwrap().contains("secret"));
    }
    let mut source = Source {
        driver: "driver".into(),
        ..Default::default()
    };
    source
        .options
        .insert("AccessToken".into(), "never persist".into());
    assert!(source.connection_string("").is_err());
}
#[test]
fn postgres_odbc_scalar_attributes_are_not_literal_braced_values() {
    let source = Source {
        dialect: Dialect::Postgres,
        driver: "PostgreSQL Unicode".into(),
        host: "127.0.0.1".into(),
        port: 34015,
        database: "rustpy_disposable".into(),
        user: "rustpy_fixture".into(),
        ..Default::default()
    };
    assert_eq!(source.connection_string("session-only-password").unwrap(),
        "Database=rustpy_disposable;Driver={PostgreSQL Unicode};PWD=session-only-password;Port=34015;Server=127.0.0.1;UID=rustpy_fixture;");
    assert!(!serde_json::to_string(&source)
        .unwrap()
        .contains("session-only-password"));
}

#[test]
fn sqlserver_odbc_port_is_part_of_server_because_microsoft_drivers_ignore_port() {
    let mut source = Source {
        dialect: Dialect::SqlServer,
        driver: "ODBC Driver 18 for SQL Server".into(),
        host: "127.0.0.1".into(),
        port: 14330,
        database: "rustpy_disposable".into(),
        user: "sa".into(),
        ..Default::default()
    };
    source
        .options
        .insert("TrustServerCertificate".into(), "yes".into());
    assert_eq!(source.connection_string("pw").unwrap(),
        "Database=rustpy_disposable;Driver={ODBC Driver 18 for SQL Server};PWD=pw;Server=127.0.0.1,14330;TrustServerCertificate=yes;UID=sa;");
    source.port = 0;
    assert!(source
        .connection_string("")
        .unwrap()
        .contains("Server=127.0.0.1;"));
    source.port = 1433;
    source.host = "db.example\\SQLEXPRESS".into();
    let named = source.connection_string("").unwrap();
    assert!(named.contains("Server=db.example\\SQLEXPRESS;") && !named.contains("Port="));
    source.host = "db.example,1444".into();
    assert!(source
        .connection_string("")
        .unwrap()
        .contains("Server=db.example,1444;"));
    source.host = "remote.example".into();
    source.ssh = Some(Tunnel {
        local_port: 40000,
        ..Default::default()
    });
    let tunneled = source.connection_string("").unwrap();
    assert!(tunneled.contains("Server=127.0.0.1,40000;") && !tunneled.contains("Port="));
}

#[test]
fn odbc_special_values_remain_single_attributes_without_losing_whitespace() {
    let mut source = Source {
        dialect: Dialect::Postgres,
        driver: "installed } driver".into(),
        host: "host;UID=intruder".into(),
        database: "{warehouse}".into(),
        user: " fixture user ".into(),
        ..Default::default()
    };
    source
        .options
        .insert("ApplicationName".into(), " name;PWD=injected} ".into());
    assert_eq!(source.connection_string("secret};UID=intruder").unwrap(),
        "ApplicationName={ name;PWD=injected}} };Database={{warehouse}}};Driver={installed }} driver};PWD={secret}};UID=intruder};Server={host;UID=intruder};UID={ fixture user };");
    source.options.clear();
    source.host = "localhost".into();
    source.database = "warehouse with spaces".into();
    source.user = "fixture".into();
    let connection = source.connection_string("a+b").unwrap();
    assert!(connection.contains("PWD={a+b};"));
    assert!(connection.contains("Database=warehouse with spaces;"));
    source.database = " leading and trailing ".into();
    assert!(source
        .connection_string("")
        .unwrap()
        .contains("Database={ leading and trailing };"));
}

#[test]
fn odbc_encoding_refuses_nul_and_secret_options_without_echoing_values() {
    let mut source = Source {
        driver: "driver".into(),
        ..Default::default()
    };
    let secret = "session-only-secret";
    for key in ["pWd", "Password", "AccessToken", "ClientSecret"] {
        source.options.insert(key.into(), secret.into());
        let error = source.connection_string(secret).unwrap_err().to_string();
        assert!(!error.contains(secret));
        source.options.clear();
    }
    for value in ["injected\0;UID=other", "\0"] {
        source.database = value.into();
        let error = source.connection_string(secret).unwrap_err().to_string();
        assert!(error.contains("NUL"));
        assert!(!error.contains(secret));
        assert!(!error.contains(value));
    }
    source.database.clear();
    assert!(source.connection_string("secret\0suffix").is_err());
}

#[test]
fn parameters_respect_literals_comments_dollar_quotes_casts_unicode_and_literal_only_values() {
    let mut params = std::collections::BTreeMap::new();
    params.insert("name".into(), "'O''Reilly'".into());
    let source = "SELECT :name, ':name', $$:name$$, [id:name], value::text -- π :name\n/* :name */";
    let output = crate::injection::bind_user_parameters(source, ":", &params).unwrap();
    assert!(output.starts_with("SELECT 'O''Reilly', ':name', $$:name$$, [id:name], value::text"));
    assert!(output.contains("-- π :name"));
    params.insert("name".into(), "N'π ''x'''".into());
    let output = crate::injection::bind_user_parameters(source, ":", &params).unwrap();
    assert!(output.starts_with("SELECT N'π ''x''', ':name'"));
    for invalid in [
        "1; DROP TABLE users",
        "N'a' + 'b'",
        "N'unterminated",
        "N1",
        "NN'a'",
    ] {
        params.insert("name".into(), invalid.into());
        assert!(crate::injection::bind_user_parameters(source, ":", &params).is_err());
    }
}
#[test]
fn composite_key_mutations_preserve_null_and_escape_mysql_backslashes() {
    let change = Change::Update {
        table: "app.people".into(),
        key: vec![("tenant".into(), Value::Int(7)), ("id".into(), Value::Null)],
        column: "name".into(),
        value: Value::Text("slash\\' ; DELETE FROM people".into()),
    };
    let sql = change.sql(Dialect::Mysql).unwrap();
    assert!(sql.contains("CONVERT(X'"));
    assert!(sql.contains("`tenant` = 7 AND `id` IS NULL"));
    assert!(!sql.contains("DELETE FROM people"));
    assert!(Change::Delete {
        table: "people".into(),
        key: vec![]
    }
    .sql(Dialect::Sqlite)
    .is_err());
}
#[test]
fn mapped_csv_and_exports_keep_multiline_quotes_and_html_safe() {
    let changes = crate::schema::import_delimited(
        "name,note\nAda,\"one\ntwo\"\n",
        b',',
        "people",
        &[(1, "note".into()), (0, "name".into())],
        true,
    )
    .unwrap();
    assert_eq!(changes.len(), 1);
    assert!(changes[0]
        .sql(Dialect::Sqlite)
        .unwrap()
        .contains("'one\ntwo'"));
    let result = QueryResult {
        columns: vec!["<name>".into()],
        rows: vec![vec![Value::Text("<script>".into())]],
        ..Default::default()
    };
    assert!(
        workspace::export(&result, "HTML", "people", Dialect::Sqlite)
            .unwrap()
            .contains("&lt;script&gt;")
    );
    assert!(
        workspace::export(&result, "JSON", "people", Dialect::Sqlite)
            .unwrap()
            .contains("<name>")
    );
}
#[test]
fn file_scopes_choose_the_longest_source_and_provider_paging_is_real() {
    let workspace = Workspace {
        settings: Settings {
            scopes: vec![
                Scope {
                    path: "/project".into(),
                    source: 1,
                    dialect: Dialect::Postgres,
                },
                Scope {
                    path: "/project/migrations".into(),
                    source: 2,
                    dialect: Dialect::Sqlite,
                },
            ],
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(
        workspace
            .scope(std::path::Path::new("/project/migrations/one.sql"))
            .unwrap()
            .source,
        2
    );
    assert!(Dialect::SqlServer
        .page("dbo.people", "id>5", "", 100, 50)
        .contains("OFFSET 100 ROWS FETCH NEXT 50 ROWS ONLY"));
    assert!(Dialect::Mysql
        .page("people", "", "id DESC", 0, 20)
        .contains("LIMIT 20 OFFSET 0"));
}
#[test]
fn url_import_decodes_session_secret_without_persisting_it() {
    let (source, secret) = crate::injection::source_from_url(
        "postgresql://alice:p%40ss@db.example:5432/warehouse?driver=PostgreSQL",
        4,
    )
    .unwrap();
    assert_eq!(secret, "p@ss");
    assert_eq!(source.id, 4);
    assert!(!serde_json::to_string(&source).unwrap().contains("p@ss"));
    assert_eq!(source.dialect, Dialect::Postgres);
}
#[test]
fn provider_script_split_preserves_dollar_bodies_triggers_and_nested_comments() {
    let pg="CREATE FUNCTION sample() RETURNS void AS $$ BEGIN PERFORM 1; PERFORM 2; END; $$ LANGUAGE plpgsql; SELECT 3;";
    assert_eq!(crate::split_statements(pg).len(), 2);
    let trigger="CREATE TRIGGER sample AFTER INSERT ON source BEGIN INSERT INTO audit VALUES (';'); UPDATE count SET n=n+1; END; SELECT 1;";
    assert_eq!(crate::split_statements(trigger).len(), 2);
    assert_eq!(
        crate::split_statements("/* outer /* inner ; */ ; */ SELECT 1;SELECT 2").len(),
        2
    );
}
#[test]
fn schema_comparison_reports_changed_types_and_safe_designer_guards() {
    let before =
        crate::schema::parse_ddl("CREATE TABLE sample(id INTEGER PRIMARY KEY, label TEXT);");
    let mut after = before.clone();
    after[0].columns[1].data_type = "VARCHAR(100)".into();
    let sql = workspace::schema_diff(&before, &after, Dialect::Postgres);
    assert!(sql.contains("ALTER COLUMN \"label\" TYPE VARCHAR(100)"));
    let mut constrained = before[0].clone();
    constrained.ddl = "CREATE TABLE sample(id INTEGER PRIMARY KEY, label TEXT UNIQUE)".into();
    let design = crate::schema::TableDesign {
        name: "sample".into(),
        columns: constrained.columns.clone(),
        ..Default::default()
    };
    assert!(crate::schema::sqlite_rebuild_preview(&constrained, &design).is_err());
}

#[test]
fn oracle_odbc_addresses_the_server_through_dbq_ezconnect() {
    let mut source = Source {
        dialect: Dialect::Oracle,
        driver: "Oracle 23 ODBC driver".into(),
        host: "127.0.0.1".into(),
        port: 15210,
        database: "FREEPDB1".into(),
        user: "rustpy".into(),
        ..Default::default()
    };
    assert_eq!(
        source.connection_string("pw").unwrap(),
        "DBQ=//127.0.0.1:15210/FREEPDB1;Driver={Oracle 23 ODBC driver};PWD=pw;UID=rustpy;"
    );
    source.port = 0;
    source.host = "::1".into();
    assert!(source
        .connection_string("")
        .unwrap()
        .contains("DBQ=//[::1]/FREEPDB1;"));
    source.host.clear();
    source.database = "ORCL_ALIAS".into();
    assert!(source
        .connection_string("")
        .unwrap()
        .contains("DBQ=ORCL_ALIAS;"));
    source.options.insert("DBQ".into(), "explicit".into());
    let explicit = source.connection_string("").unwrap();
    assert!(explicit.contains("DBQ=explicit;") && !explicit.contains("ORCL_ALIAS"));
}
