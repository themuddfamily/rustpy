//! Retained, bounded session owner: one connection and transaction per console.
use crate::{workspace::*, QueryResult, Value};
use anyhow::{bail, Context, Result};
use odbc_api::{
    handles::{SqlText, Statement},
    Cursor, ResultSetMetadata,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc,
    },
    thread::JoinHandle,
    time::Instant,
};

#[derive(Clone, Debug)]
pub enum Request {
    Query { sql: String, limit: usize },
    Catalog,
    ObjectDetails(Box<Object>),
    Explain(String),
    Search(String),
    Submit(Vec<Change>),
    Commit,
    Rollback,
    AutoCommit(bool),
    ReadOnly(bool),
    Isolation(String),
    Stop,
}
pub enum Reply {
    Connected,
    State { auto_commit: bool, read_only: bool },
    Results(Vec<(String, QueryResult)>),
    Catalog(Vec<Object>),
    Done(String),
    Error(String),
    Retired,
}
pub struct Session {
    pub id: u64,
    pub source: Source,
    pub name: String,
    tx: SyncSender<Request>,
    rx: Receiver<Reply>,
    cancel: Arc<AtomicBool>,
    interrupt: Arc<std::sync::Mutex<Option<rusqlite::InterruptHandle>>>,
    join: Option<JoinHandle<()>>,
    pub busy: bool,
    pub stopping: bool,
}
impl Session {
    pub fn start(
        id: u64,
        source: Source,
        name: String,
        secret: String,
        settings: Settings,
    ) -> Result<Self> {
        let (tx, requests) = mpsc::sync_channel(8);
        let (replies, rx) = mpsc::sync_channel(16);
        let cancel = Arc::new(AtomicBool::new(false));
        let stopped = cancel.clone();
        let captured = source.clone();
        let interrupt = Arc::new(std::sync::Mutex::new(None));
        let worker_interrupt = interrupt.clone();
        let join = std::thread::Builder::new()
            .name(format!("database-{id}"))
            .spawn(move || {
                let outcome = (|| -> Result<()> {
                    let _tunnel = TunnelProcess::open(captured.ssh.as_ref())?;
                    let mut settings = settings;
                    let mut session_read_only = captured.read_only;
                    let mut backend = Backend::open(&captured, &secret, &settings)?;
                    if let Backend::Sqlite(c) = &backend {
                        if let Ok(mut slot) = worker_interrupt.lock() {
                            *slot = Some(c.conn.get_interrupt_handle());
                        }
                    }
                    let _ = replies.send(Reply::Connected);
                    while let Ok(request) = requests.recv() {
                        if stopped.load(Ordering::Acquire) || matches!(request, Request::Stop) {
                            break;
                        }
                        let read_only_request = if let Request::ReadOnly(enabled) = &request {
                            Some(*enabled)
                        } else {
                            None
                        };
                        let reply = backend
                            .handle(request, &captured, &mut settings, &stopped)
                            .unwrap_or_else(|e| Reply::Error(redact(&format!("{e:#}"), &secret)));
                        match &backend {
                            Backend::Sqlite(c) => settings.auto_commit = c.conn.is_autocommit(),
                            Backend::Odbc(c) => settings.auto_commit = c.auto_commit,
                            Backend::Ddl(_) => {}
                        }
                        if !matches!(&reply, Reply::Error(_)) {
                            if let Some(value) = read_only_request {
                                session_read_only = value;
                            }
                        }
                        if replies.send(reply).is_err() {
                            break;
                        }
                        let read_only = match &backend {
                            Backend::Sqlite(c) => c
                                .conn
                                .pragma_query_value(None, "query_only", |row| row.get::<_, bool>(0))
                                .unwrap_or(captured.read_only),
                            _ => session_read_only,
                        };
                        if replies
                            .send(Reply::State {
                                auto_commit: settings.auto_commit,
                                read_only,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    backend.rollback()?;
                    Ok(())
                })();
                if let Err(e) = outcome {
                    let _ = replies.send(Reply::Error(redact(&format!("{e:#}"), &secret)));
                }
                let _ = replies.send(Reply::Retired);
            })?;
        Ok(Self {
            id,
            source,
            name,
            tx,
            rx,
            cancel,
            interrupt,
            join: Some(join),
            busy: true,
            stopping: false,
        })
    }
    pub fn send(&mut self, request: Request) -> Result<()> {
        if self.stopping {
            bail!("Session is stopping");
        }
        if self.busy {
            bail!("Wait for this console's current request");
        }
        self.tx
            .try_send(request)
            .context("Database request queue unavailable")?;
        self.busy = true;
        Ok(())
    }
    pub fn poll(&mut self) -> Vec<Reply> {
        let mut out = vec![];
        while let Ok(reply) = self.rx.try_recv() {
            self.busy = false;
            out.push(reply);
        }
        if self.join.as_ref().is_some_and(|j| j.is_finished()) {
            if let Some(j) = self.join.take() {
                let _ = j.join();
            }
        }
        out
    }
    /// Cancellation is cooperative between fetches/statements. Driver timeouts bound SQL calls
    /// where supported; ownership remains visible until the driver actually returns.
    pub fn stop(&mut self) {
        self.stopping = true;
        if let Ok(slot) = self.interrupt.lock() {
            if let Some(interrupt) = slot.as_ref() {
                interrupt.interrupt();
            }
        }
        self.cancel.store(true, Ordering::Release);
        let _ = self.tx.try_send(Request::Stop);
    }
    pub fn alive(&self) -> bool {
        self.join.is_some()
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}
fn redact(s: &str, secret: &str) -> String {
    if secret.is_empty() {
        s.into()
    } else {
        s.replace(&secret.replace('}', "}}"), "<credential>")
            .replace(secret, "<credential>")
    }
}
enum Backend {
    Sqlite(Box<crate::Connection>),
    Odbc(OdbcConnection),
    Ddl(Vec<Object>),
}
/// Oracle's client converts CHAR/VARCHAR2 data through the NLS_LANG client
/// character set even for SQL_C_WCHAR buffers; when NLS_LANG is unset the
/// Unix default is US7ASCII and every non-ASCII character reads back as '?'.
/// An explicit user NLS_LANG is respected; otherwise select AL32UTF8 once,
/// before the driver creates its first OCI environment.
fn oracle_client_charset() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if cfg!(unix) && std::env::var_os("NLS_LANG").is_none() {
            std::env::set_var("NLS_LANG", ".AL32UTF8");
        }
    });
}
fn oracle_anonymous_block(sql: &str) -> bool {
    let first = sql
        .trim_start()
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .next()
        .unwrap_or("");
    first.eq_ignore_ascii_case("DECLARE") || first.eq_ignore_ascii_case("BEGIN")
}

// A successful explicit boundary is the only authority for enabling ODBC
// auto-commit after manual work. Reads/metadata and failed statements may also
// begin a transaction, so this deliberately does not classify SQL as read-only.
struct OdbcConnection {
    connection: odbc_api::Connection<'static>,
    pending: bool,
    auto_commit: bool,
    oracle_implicit_results: Option<String>,
}
impl OdbcConnection {
    fn oracle_implicit_results(&mut self, timeout: usize, cancel: &AtomicBool) -> Result<String> {
        if let Some(version) = &self.oracle_implicit_results {
            return Ok(version.clone());
        }
        let version_results = self.query(
            "SELECT VERSION FROM PRODUCT_COMPONENT_VERSION WHERE PRODUCT LIKE 'Oracle%Database%'",
            2,
            timeout,
            cancel,
        )?;
        let versions = version_results
            .iter()
            .filter(|result| !result.columns.is_empty())
            .collect::<Vec<_>>();
        anyhow::ensure!(
            versions.len() == 1
                && versions[0].columns.len() == 1
                && !versions[0].truncated
                && versions[0].rows.len() == 1
                && versions[0].rows[0].len() == 1,
            "Oracle server version was unavailable"
        );
        let version = versions[0].rows[0][0].display();
        anyhow::ensure!(
            crate::routine::oracle_server_version_supported(&version),
            "Oracle implicit results require a captured server version of at least 12"
        );
        // This is a real, read-only result roundtrip through the selected driver,
        // not an inference from the server version or a driver filename.
        let results = self.query(crate::routine::ORACLE_IMPLICIT_PROBE, 2, timeout, cancel)?;
        let cursors = results
            .iter()
            .filter(|result| !result.columns.is_empty())
            .collect::<Vec<_>>();
        anyhow::ensure!(
            cursors.len() == 1
                && cursors[0].columns.len() == 1
                && !cursors[0].truncated
                && cursors[0].rows.len() == 1
                && cursors[0].rows[0].len() == 1
                && cursors[0].rows[0][0].display() == "rustpy_implicit_result_v1",
            "Selected Oracle ODBC driver did not return the implicit-result sentinel"
        );
        self.oracle_implicit_results = Some(version.clone());
        Ok(version)
    }
    fn query(
        &mut self,
        sql: &str,
        limit: usize,
        timeout: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<QueryResult>> {
        if !self.auto_commit {
            self.pending = true;
        }
        let query_started = Instant::now();
        let mut statement = self.preallocate()?;
        statement.set_query_timeout_sec(timeout.clamp(1, 300))?;
        let mut results = vec![];
        let mut bytes = 0;
        // Keep the statement even when SQLExecDirect starts with a row
        // count. Preallocated::execute returns None for that state and
        // would hide any later SELECT/implicit result sets.
        let mut handle = statement.into_handle();
        // SAFETY: this freshly allocated statement has no bound buffers
        // or parameters; SQL text is owned for the duration of the call.
        unsafe { handle.exec_direct(&SqlText::new(sql)) }.into_result_bool(&handle)?;
        loop {
            if cancel.load(Ordering::Acquire) {
                bail!("Canceled; transaction state should be checked before reuse");
            }
            let count = handle.num_result_cols().into_result(&handle)?;
            if count == 0 {
                let affected = handle.row_count().into_result(&handle)?;
                results.push(QueryResult {
                    affected: usize::try_from(affected).ok(),
                    elapsed: query_started.elapsed(),
                    ..Default::default()
                });
                // SAFETY: no buffers are bound and SQLExecDirect or the
                // previous SQLMoreResults completed synchronously.
                if !unsafe { handle.more_results() }.into_result_bool(&handle)? {
                    break;
                }
                anyhow::ensure!(results.len() < 32, "Query exceeds 32 result sets");
                continue;
            }
            anyhow::ensure!(count > 0, "Invalid ODBC result column count");
            // SAFETY: positive SQLNumResultCols proves cursor state;
            // this owner has exclusive access and no bound buffers.
            let mut current = unsafe { odbc_api::CursorImpl::new(handle) };
            let start = Instant::now();
            let columns = current
                .column_names()?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let count = columns.len();
            let binary = (1..=count)
                .map(|i| {
                    current
                        .col_data_type(i as u16)
                        .map(|t| format!("{t:?}").to_ascii_uppercase().contains("BINARY"))
                })
                .collect::<std::result::Result<Vec<_>, _>>()?;
            if count > 4096 {
                bail!("Result has too many columns");
            }
            let mut result = QueryResult {
                columns,
                ..Default::default()
            };
            while let Some(mut row) = current.next_row()? {
                if cancel.load(Ordering::Acquire) {
                    bail!("Canceled; transaction state should be checked before reuse");
                }
                if result.rows.len() >= limit.min(100_000) {
                    result.truncated = true;
                    break;
                }
                let mut values = vec![];
                for (index, is_binary) in binary.iter().copied().enumerate() {
                    let value = read_cell(&mut row, (index + 1) as u16, is_binary, cancel)?;
                    bytes += value.display().len();
                    if bytes > 32 * 1024 * 1024 {
                        bail!("Result exceeds 32 MiB; narrow the query");
                    }
                    values.push(value);
                }
                result.rows.push(values);
            }
            result.elapsed = query_started.elapsed().max(start.elapsed());
            results.push(result);
            handle = current.into_stmt();
            // SAFETY: row data was read synchronously and no pointers
            // to row or parameter buffers remain bound.
            if !unsafe { handle.more_results() }.into_result_bool(&handle)? {
                break;
            }
            anyhow::ensure!(results.len() < 32, "Query exceeds 32 result sets");
        }
        if results.is_empty() {
            results.push(QueryResult::default());
        }
        Ok(results)
    }

    fn set_mode(&mut self, enabled: bool) -> Result<()> {
        if self.auto_commit == enabled {
            return Ok(());
        }
        if enabled && self.pending {
            bail!("Commit or roll back pending ODBC work before enabling auto-commit");
        }
        self.connection.set_autocommit(enabled)?;
        self.auto_commit = enabled;
        Ok(())
    }
}
impl std::ops::Deref for OdbcConnection {
    type Target = odbc_api::Connection<'static>;
    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}
impl std::ops::DerefMut for OdbcConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.connection
    }
}
impl Backend {
    fn open(source: &Source, secret: &str, settings: &Settings) -> Result<Self> {
        if source.ddl_only {
            return Ok(Self::Ddl(crate::schema::parse_ddl(
                &std::fs::read_to_string(&source.file)?,
            )));
        }
        if source.dialect == Dialect::Sqlite {
            if !settings.allow_local {
                bail!("Local data sources are disabled");
            }
            let ds = crate::DataSource::sqlite(&source.name, &source.file);
            let c = if source.read_only {
                crate::Connection {
                    conn: rusqlite::Connection::open_with_flags(
                        &source.file,
                        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                    )?,
                    source: ds,
                }
            } else {
                crate::Connection::open(&ds)?
            };
            c.conn.busy_timeout(std::time::Duration::from_secs(
                settings.timeout_seconds.clamp(1, 300) as u64,
            ))?;
            if source.read_only {
                c.execute_script("PRAGMA query_only=ON")?;
            }
            if !settings.auto_commit {
                c.execute_script("BEGIN")?;
            }
            Ok(Self::Sqlite(Box::new(c)))
        } else {
            if source.dialect == Dialect::Oracle {
                oracle_client_charset();
            }
            let env = odbc_api::environment()?;
            let read_only_sql = if source.read_only {
                Some(read_only_statement(source.dialect, true)?)
            } else {
                None
            };
            let connection_string = source.connection_string(secret)?;
            let start = Instant::now();
            let conn = loop {
                match env.connect_with_connection_string(
                    &connection_string,
                    odbc_api::ConnectionOptions {
                        login_timeout_sec: Some(if source.ssh.is_some() {
                            3
                        } else {
                            settings.timeout_seconds.clamp(1, 300) as u32
                        }),
                        ..Default::default()
                    },
                ) {
                    Ok(connection) => break connection,
                    Err(_)
                        if source.ssh.is_some()
                            && start.elapsed() < std::time::Duration::from_secs(15) =>
                    {
                        std::thread::sleep(std::time::Duration::from_millis(100))
                    }
                    Err(e) => return Err(e.into()),
                }
            };
            if let Some(sql) = read_only_sql {
                let _ = conn.execute(sql, (), Some(settings.timeout_seconds.clamp(1, 300)))?;
            }
            conn.set_autocommit(settings.auto_commit)?;
            Ok(Self::Odbc(OdbcConnection {
                connection: conn,
                pending: false,
                auto_commit: settings.auto_commit,
                oracle_implicit_results: None,
            }))
        }
    }
    fn query(
        &mut self,
        sql: &str,
        limit: usize,
        timeout: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<QueryResult>> {
        match self {
            Self::Sqlite(c) => {
                if cancel.load(Ordering::Acquire) {
                    bail!("Canceled");
                }
                let start = Instant::now();
                let deadline = std::time::Duration::from_secs(timeout.clamp(1, 300) as u64);
                c.conn
                    .progress_handler(10000, Some(move || start.elapsed() > deadline));
                let mut stmt = c.conn.prepare(sql)?;
                let columns = stmt
                    .column_names()
                    .into_iter()
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                if columns.is_empty() {
                    let affected = stmt.execute([])?;
                    return Ok(vec![QueryResult {
                        affected: Some(affected),
                        elapsed: start.elapsed(),
                        ..Default::default()
                    }]);
                }
                let count = columns.len();
                let mut result = QueryResult {
                    columns,
                    ..Default::default()
                };
                let mut rows = stmt.query([])?;
                let mut bytes = 0;
                while let Some(row) = rows.next()? {
                    if cancel.load(Ordering::Acquire) {
                        bail!("Canceled");
                    }
                    if result.rows.len() >= limit.min(100000) {
                        result.truncated = true;
                        break;
                    }
                    let mut values = vec![];
                    for i in 0..count {
                        let value = match row.get_ref(i)? {
                            rusqlite::types::ValueRef::Blob(b) => Value::Binary(b.to_vec()),
                            other => Value::from_ref(other),
                        };
                        bytes += value.display().len();
                        if bytes > 32 * 1024 * 1024 {
                            bail!("Result exceeds 32 MiB");
                        }
                        values.push(value);
                    }
                    result.rows.push(values);
                }
                result.elapsed = start.elapsed();
                Ok(vec![result])
            }
            Self::Ddl(_) => bail!(
                "An offline DDL source supports catalog, completion, diagrams and comparison; it cannot execute SQL"
            ),
            Self::Odbc(c) => c.query(sql, limit, timeout, cancel),
        }
    }
    fn rollback(&mut self) -> Result<()> {
        match self {
            Self::Sqlite(c) => {
                if !c.conn.is_autocommit() {
                    c.execute_script("ROLLBACK")?;
                }
            }
            Self::Odbc(c) => {
                c.pending = true;
                c.rollback()?;
                c.pending = false;
            }
            _ => {}
        }
        Ok(())
    }
    fn handle(
        &mut self,
        request: Request,
        source: &Source,
        settings: &mut Settings,
        cancel: &AtomicBool,
    ) -> Result<Reply> {
        if let Self::Sqlite(c) = self {
            let start = Instant::now();
            let timeout =
                std::time::Duration::from_secs(settings.timeout_seconds.clamp(1, 300) as u64);
            c.conn
                .progress_handler(10000, Some(move || start.elapsed() > timeout));
        }
        if let Self::Odbc(c) = self {
            if !settings.auto_commit
                && matches!(
                    &request,
                    Request::Query { .. }
                        | Request::Catalog
                        | Request::ObjectDetails(_)
                        | Request::Explain(_)
                        | Request::Search(_)
                        | Request::Submit(_)
                        | Request::Isolation(_)
                        | Request::ReadOnly(_)
                )
            {
                c.pending = true;
            }
        }
        Ok(match request {
            Request::Query { sql, limit } => {
                let mut results = vec![];
                let statements =
                    if source.dialect == Dialect::Oracle && oracle_anonymous_block(&sql) {
                        if sql
                            .trim_start()
                            .starts_with(crate::routine::ORACLE_OUTPUT_MARKER)
                        {
                            let Self::Odbc(connection) = self else {
                                bail!("Oracle output bindings require a live ODBC connection");
                            };
                            connection.oracle_implicit_results(settings.timeout_seconds, cancel)?;
                        }
                        vec![(Default::default(), sql)]
                    } else {
                        crate::split_statements(&sql)
                    };
                for (_, text) in statements {
                    for result in self.query(&text, limit, settings.timeout_seconds, cancel)? {
                        results.push((text.clone(), result));
                    }
                }
                Reply::Results(results)
            }
            Request::Catalog => Reply::Catalog(self.catalog(source, settings, cancel)?),
            Request::ObjectDetails(object) => {
                let mut results = vec![];
                for (label, sql) in crate::catalog::object_details(source.dialect, &object) {
                    match self.query(
                        &sql,
                        settings.result_limit,
                        settings.timeout_seconds,
                        cancel,
                    ) {
                        Ok(sets) => {
                            results.extend(sets.into_iter().map(|result| (label.clone(), result)))
                        }
                        Err(e) => results.push((
                            label,
                            QueryResult {
                                columns: vec!["Capability / server diagnostic".into()],
                                rows: vec![vec![Value::Text(e.to_string())]],
                                ..Default::default()
                            },
                        )),
                    }
                }
                Reply::Results(results)
            }
            Request::Search(needle) => {
                if needle.trim().is_empty() {
                    bail!("Enter search text");
                }
                let mut objects = self
                    .catalog(source, settings, cancel)?
                    .into_iter()
                    .filter(|o| o.kind.eq_ignore_ascii_case("TABLE"))
                    .collect::<Vec<_>>();
                // Servers such as SQL Server introspect every database the
                // login can see (master/msdb sort first); search the connected
                // database's tables before the bounded remainder.
                if source.dialect == Dialect::Oracle && !source.user.is_empty() {
                    // Oracle has no catalogs; the login's own schema sorts
                    // after dictionary-owning schemas (CTXSYS, MDSYS, ...).
                    objects.sort_by_key(|o| !o.schema.eq_ignore_ascii_case(&source.user));
                } else if !source.database.is_empty() {
                    objects.sort_by_key(|o| {
                        !o.catalog.is_empty() && !o.catalog.eq_ignore_ascii_case(&source.database)
                    });
                }
                let mut results = vec![];
                for object in objects.into_iter().take(100) {
                    let cast = match source.dialect {
                        Dialect::Oracle => "VARCHAR2(4000)",
                        Dialect::SqlServer => "NVARCHAR(MAX)",
                        Dialect::BigQuery => "STRING",
                        _ => "VARCHAR(4096)",
                    };
                    let pattern =
                        dialect_literal(&Value::Text(format!("%{needle}%")), source.dialect)?;
                    let filter = object
                        .columns
                        .iter()
                        .filter(|c| {
                            let ty = c.data_type.to_uppercase();
                            ty.contains("CHAR") || ty.contains("TEXT") || ty.contains("JSON")
                        })
                        .map(|c| {
                            format!(
                                "CAST({} AS {cast}) LIKE {pattern}",
                                source.dialect.quote(&c.name)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" OR ");
                    if filter.is_empty() {
                        continue;
                    }
                    for result in self.query(
                        &source.dialect.page(
                            &object.qualified(),
                            &filter,
                            "",
                            0,
                            settings.page_size.min(100),
                        ),
                        settings.page_size.min(100),
                        settings.timeout_seconds,
                        cancel,
                    )? {
                        if !result.rows.is_empty() {
                            results.push((format!("Search {}", object.qualified()), result));
                        }
                    }
                    if results.len() >= 32 {
                        break;
                    }
                }
                Reply::Results(results)
            }
            Request::Explain(sql) => {
                let limit = settings.result_limit;
                let timeout = settings.timeout_seconds;
                let results = if source.dialect == Dialect::SqlServer {
                    self.query("SET SHOWPLAN_XML ON", 1, timeout, cancel)?;
                    let result = self.query(&sql, limit, timeout, cancel);
                    let reset =
                        self.query("SET SHOWPLAN_XML OFF", 1, timeout, &AtomicBool::new(false));
                    reset?;
                    result?
                } else if source.dialect == Dialect::Oracle {
                    self.query(&format!("EXPLAIN PLAN FOR {sql}"), 1, timeout, cancel)?;
                    self.query(
                        "SELECT * FROM TABLE(DBMS_XPLAN.DISPLAY())",
                        limit,
                        timeout,
                        cancel,
                    )?
                } else {
                    self.query(&source.dialect.explain(&sql), limit, timeout, cancel)?
                };
                Reply::Results(results.into_iter().map(|r| (sql.clone(), r)).collect())
            }
            Request::Commit => {
                match self {
                    Self::Sqlite(c) => {
                        if !c.conn.is_autocommit() {
                            c.execute_script("COMMIT")?;
                        }
                        if !settings.auto_commit {
                            c.execute_script("BEGIN")?;
                        }
                    }
                    Self::Odbc(c) => {
                        c.pending = true;
                        c.commit()?;
                        c.pending = false;
                    }
                    _ => bail!("Offline source"),
                };
                Reply::Done("Transaction committed".into())
            }
            Request::Rollback => {
                self.rollback()?;
                if !settings.auto_commit {
                    if let Self::Sqlite(c) = self {
                        c.execute_script("BEGIN")?;
                    }
                }
                Reply::Done("Transaction rolled back".into())
            }
            Request::AutoCommit(enabled) => {
                match self {
                    Self::Sqlite(c) => {
                        if enabled && !c.conn.is_autocommit() {
                            bail!("Commit or roll back before enabling auto-commit");
                        }
                        if !enabled && c.conn.is_autocommit() {
                            c.execute_script("BEGIN")?;
                        }
                    }
                    Self::Odbc(c) => {
                        if enabled && !settings.auto_commit && c.pending {
                            bail!(
                                "Commit or roll back pending ODBC work before enabling auto-commit"
                            );
                        }
                        if enabled != settings.auto_commit {
                            c.set_mode(enabled)?;
                        }
                    }
                    _ => {}
                }
                settings.auto_commit = enabled;
                Reply::Done(format!("Auto-commit {enabled}"))
            }
            Request::ReadOnly(enabled) => {
                if source.read_only && !enabled {
                    bail!(
                        "Change the data-source read-only policy and reconnect before enabling writes"
                    );
                }
                match self {
                    Self::Sqlite(c) => c.execute_script(if enabled {
                        "PRAGMA query_only=ON"
                    } else {
                        "PRAGMA query_only=OFF"
                    })?,
                    Self::Odbc(c) => {
                        if !settings.auto_commit {
                            bail!(
                                "Reconnect in auto-commit mode to change the session read-only policy without discarding a transaction"
                            );
                        }
                        let _ = c.execute(
                            read_only_statement(source.dialect, enabled)?,
                            (),
                            Some(settings.timeout_seconds.clamp(1, 300)),
                        )?;
                    }
                    Self::Ddl(_) => bail!("Offline source"),
                };
                Reply::Done("Read-only mode changed".into())
            }
            Request::Isolation(level) => {
                if ![
                    "READ UNCOMMITTED",
                    "READ COMMITTED",
                    "REPEATABLE READ",
                    "SERIALIZABLE",
                ]
                .contains(&level.as_str())
                {
                    bail!("Unknown isolation level");
                }
                let sql = if source.dialect == Dialect::Sqlite {
                    format!(
                        "PRAGMA read_uncommitted={}",
                        usize::from(level == "READ UNCOMMITTED")
                    )
                } else if source.dialect == Dialect::Oracle {
                    // Oracle's SET TRANSACTION only covers the current
                    // transaction (and is lost under auto-commit); the session
                    // default is ALTER SESSION. Oracle has no dirty or
                    // repeatable-read level.
                    if !matches!(level.as_str(), "READ COMMITTED" | "SERIALIZABLE") {
                        bail!("Oracle supports READ COMMITTED and SERIALIZABLE isolation");
                    }
                    format!("ALTER SESSION SET ISOLATION_LEVEL = {level}")
                } else {
                    format!("SET TRANSACTION ISOLATION LEVEL {level}")
                };
                self.query(&sql, 1, settings.timeout_seconds, cancel)?;
                Reply::Done("Isolation level changed".into())
            }
            Request::Submit(changes) => {
                if source.read_only {
                    bail!("Read-only source");
                }
                if changes.is_empty() {
                    bail!("No changes to submit");
                }
                validate_submit_transaction(source.dialect, settings.auto_commit, &changes)?;
                if changes
                    .iter()
                    .any(|c| matches!(c, Change::SqliteRebuild { .. }))
                {
                    if changes.len() != 1 {
                        bail!("Submit a SQLite rebuild separately from other queued changes");
                    }
                    let Change::SqliteRebuild { old, design } = &changes[0] else {
                        unreachable!()
                    };
                    let Self::Sqlite(c) = self else {
                        bail!("Rebuild belongs to SQLite");
                    };
                    if !c.conn.is_autocommit() || !settings.auto_commit {
                        bail!(
                            "Commit or roll back and reconnect in auto-commit mode before rebuilding a table"
                        );
                    }
                    let current = c.ddl(&old.name)?;
                    if current != old.ddl {
                        bail!("The table schema changed; refresh before rebuilding");
                    }
                    let foreign_keys = c
                        .conn
                        .pragma_query_value(None, "foreign_keys", |r| r.get::<_, bool>(0))?;
                    let start = Instant::now();
                    let timeout = std::time::Duration::from_secs(
                        settings.timeout_seconds.clamp(1, 300) as u64,
                    );
                    c.conn
                        .progress_handler(10000, Some(move || start.elapsed() > timeout));
                    c.execute_script("PRAGMA foreign_keys=OFF")?;
                    if let Err(error) = c.execute_script("BEGIN IMMEDIATE") {
                        c.execute_script(if foreign_keys {
                            "PRAGMA foreign_keys=ON"
                        } else {
                            "PRAGMA foreign_keys=OFF"
                        })?;
                        return Err(error);
                    }
                    let result = (|| -> Result<()> {
                        if cancel.load(Ordering::Acquire) {
                            bail!("Canceled");
                        }
                        c.execute_script(&crate::schema::sqlite_rebuild_preview(old, design)?)?;
                        let violations = c.query("PRAGMA foreign_key_check", 1)?;
                        if !violations.rows.is_empty() {
                            bail!(
                                "Rebuild would violate foreign keys; the transaction was rolled back"
                            );
                        }
                        c.execute_script("COMMIT")?;
                        Ok(())
                    })();
                    if result.is_err() {
                        let _ = c.execute_script("ROLLBACK");
                    }
                    c.execute_script(if foreign_keys {
                        "PRAGMA foreign_keys=ON"
                    } else {
                        "PRAGMA foreign_keys=OFF"
                    })?;
                    result?;
                    return Ok(Reply::Done("Changes submitted and committed; SQLite table rebuilt with related indexes/triggers and foreign-key validation".into()));
                }
                let savepoint = if settings.auto_commit {
                    None
                } else {
                    Some(submit_savepoint(source.dialect)?)
                };
                let sqlite_auto = matches!(self,Self::Sqlite(c) if c.conn.is_autocommit());
                if sqlite_auto {
                    self.query("BEGIN", 1, settings.timeout_seconds, cancel)?;
                }
                if let Self::Odbc(c) = self {
                    c.set_mode(false)?;
                }
                if let Some((begin, _, _)) = savepoint {
                    self.query(begin, 1, settings.timeout_seconds, cancel)?;
                }
                let result = (|| -> Result<()> {
                    for change in changes {
                        for (_, statement) in crate::split_statements(&change.sql(source.dialect)?)
                        {
                            self.query(&statement, 1, settings.timeout_seconds, cancel)?;
                        }
                    }
                    Ok(())
                })();
                if let Err(e) = result {
                    if let Some((_, rollback, release)) = savepoint {
                        // A cancelled request must still retire its own batch.
                        // Never roll back earlier pending user work.
                        let cleanup = AtomicBool::new(false);
                        self.query(rollback, 1, settings.timeout_seconds, &cleanup)
                            .with_context(|| {
                                format!("Submit failed ({e}); batch rollback failed")
                            })?;
                        if let Some(release) = release {
                            self.query(release, 1, settings.timeout_seconds, &cleanup)?;
                        }
                        return Err(e);
                    }
                    self.rollback()?;
                    if let Self::Odbc(c) = self {
                        if settings.auto_commit {
                            c.set_mode(true)?;
                        } else {
                            c.pending = true;
                        }
                    }
                    return Err(e);
                }
                if let Some((_, _, Some(release))) = savepoint {
                    self.query(release, 1, settings.timeout_seconds, cancel)?;
                }
                if settings.auto_commit {
                    match self {
                        Self::Sqlite(c) => {
                            if sqlite_auto {
                                c.execute_script("COMMIT")?;
                            }
                        }
                        Self::Odbc(c) => {
                            c.pending = true;
                            c.commit()?;
                            c.pending = false;
                            c.set_mode(true)?;
                        }
                        _ => {}
                    }
                }
                Reply::Done(
                    if settings.auto_commit {
                        "Changes submitted and committed"
                    } else {
                        "Changes submitted; transaction remains pending"
                    }
                    .into(),
                )
            }
            Request::Stop => Reply::Retired,
        })
    }
    fn catalog(
        &mut self,
        source: &Source,
        settings: &Settings,
        cancel: &AtomicBool,
    ) -> Result<Vec<Object>> {
        match self {
            Self::Ddl(objects) => Ok(objects.clone()),
            Self::Sqlite(c) => {
                let mut out = vec![];
                for table in c.tables()? {
                    let ddl = c.ddl(&table.name)?;
                    let related_ddl=c.query(&format!("SELECT sql FROM sqlite_master WHERE tbl_name={} AND type IN ('index','trigger') AND sql IS NOT NULL",literal(&Value::Text(table.name.clone()))?),10000)?.rows.into_iter().filter_map(|row|row.first().map(Value::display)).collect();
                    let keys = c
                        .query(
                            &format!(
                                "PRAGMA foreign_key_list({})",
                                crate::quote_ident(&table.name)
                            ),
                            10_000,
                        )?
                        .rows
                        .into_iter()
                        .filter_map(|r| {
                            Some(ForeignKey {
                                column: r.get(3)?.display(),
                                target: r.get(2)?.display(),
                                target_column: r.get(4)?.display(),
                            })
                        })
                        .collect();
                    out.push(Object {
                        routine: None,
                        related_ddl,
                        catalog: String::new(),
                        schema: String::new(),
                        name: table.name,
                        kind: format!("{:?}", table.kind),
                        columns: table
                            .columns
                            .into_iter()
                            .map(|c| Column {
                                name: c.name,
                                data_type: c.data_type,
                                nullable: c.nullable,
                                primary: c.primary_key,
                                default: c.default,
                            })
                            .collect(),
                        keys,
                        ddl,
                    });
                }
                Ok(out)
            }
            Self::Odbc(c) => {
                let mut objects = vec![];
                // Oracle exposes thousands of dictionary tables (SYS, MDSYS,
                // ...) and its driver answers SQLColumns/keys per table
                // slowly; full introspection takes minutes. Without explicit
                // source schemas, introspect the session's current schema.
                let mut schemas = source.schemas.clone();
                if source.dialect == Dialect::Oracle && schemas.is_empty() {
                    let current = c.query(
                        "SELECT SYS_CONTEXT('USERENV','CURRENT_SCHEMA') FROM dual",
                        1,
                        settings.timeout_seconds,
                        cancel,
                    )?;
                    if let Some(schema) = current
                        .iter()
                        .flat_map(|result| result.rows.first())
                        .flat_map(|row| row.first())
                        .map(Value::display)
                        .find(|schema| !schema.is_empty())
                    {
                        schemas.push(schema);
                    }
                }
                let schemas = &schemas;
                // Oracle has no catalogs and its driver rejects a "%"
                // catalog pattern with a spurious HYT00 "Timeout expired".
                let catalogs = if source.dialect == Dialect::Oracle {
                    ""
                } else {
                    "%"
                };
                for row in c.tables(catalogs, "%", "%", "")? {
                    if cancel.load(Ordering::Acquire) {
                        bail!("Canceled");
                    }
                    let row = row?;
                    let catalog = row.catalog.as_str()?.unwrap_or("").to_string();
                    let schema = row.schema.as_str()?.unwrap_or("").to_string();
                    if !schemas.is_empty() && !schemas.contains(&schema) {
                        continue;
                    }
                    let name = row.table.as_str()?.unwrap_or("").to_string();
                    let kind = row.table_type.as_str()?.unwrap_or("").to_string();
                    objects.push(Object {
                        routine: None,
                        related_ddl: vec![],
                        catalog,
                        schema,
                        name,
                        kind,
                        columns: vec![],
                        keys: vec![],
                        ddl: String::new(),
                    });
                    if objects.len()
                        >= if source.introspection_depth == 0 {
                            10_000
                        } else {
                            source.introspection_depth.min(10_000)
                        }
                    {
                        break;
                    }
                }
                for object in &mut objects {
                    if settings.timeout_seconds == 0 {
                        break;
                    }
                    let mut warnings = vec![];
                    let mut oracle_defaults = vec![];
                    if source.dialect == Dialect::Oracle {
                        // Oracle's driver leaves SQLColumns COLUMN_DEF empty;
                        // the dictionary keeps defaults in a LONG column.
                        let sql = format!(
                            "SELECT COLUMN_NAME, DATA_DEFAULT FROM ALL_TAB_COLUMNS WHERE OWNER={} AND TABLE_NAME={} AND DEFAULT_LENGTH>0",
                            dialect_literal(&Value::Text(object.schema.clone()), source.dialect)?,
                            dialect_literal(&Value::Text(object.name.clone()), source.dialect)?,
                        );
                        match c.query(&sql, 1000, settings.timeout_seconds, cancel) {
                            Ok(results) => {
                                for row in results.iter().flat_map(|r| &r.rows) {
                                    if let [name, default] = &row[..] {
                                        let default = default.display().trim().to_string();
                                        if !default.is_empty() {
                                            oracle_defaults.push((name.display(), default));
                                        }
                                    }
                                }
                            }
                            Err(e) => warnings.push(format!("Column defaults unavailable: {e}")),
                        }
                    }
                    let mut statement = c.preallocate()?;
                    let columns = read_catalog(statement.columns_cursor(
                        &object.catalog,
                        &object.schema,
                        &object.name,
                        "%",
                    )?)?;
                    for row in columns {
                        let name = row.get(3).cloned().unwrap_or_default();
                        let default =
                            row.get(12).filter(|s| !s.is_empty()).cloned().or_else(|| {
                                oracle_defaults
                                    .iter()
                                    .find(|(column, _)| *column == name)
                                    .map(|(_, default)| default.clone())
                            });
                        object.columns.push(Column {
                            name,
                            data_type: row.get(5).cloned().unwrap_or_default(),
                            nullable: row.get(10).is_none_or(|v| v != "0"),
                            primary: false,
                            default,
                        });
                    }
                    let primary = (|| -> Result<Vec<Vec<String>>> {
                        read_catalog(statement.primary_keys_cursor(
                            Some(&object.catalog),
                            Some(&object.schema),
                            &object.name,
                        )?)
                    })();
                    let primary = match primary {
                        Ok(rows) => rows,
                        Err(e) => {
                            warnings.push(format!("Primary key metadata unavailable: {e}"));
                            vec![]
                        }
                    };
                    for row in primary {
                        if let Some(col) = object
                            .columns
                            .iter_mut()
                            .find(|c| Some(&c.name) == row.get(3))
                        {
                            col.primary = true;
                        }
                    }
                    let foreign = (|| -> Result<Vec<Vec<String>>> {
                        read_catalog(statement.foreign_keys_cursor(
                            "",
                            "",
                            "",
                            &object.catalog,
                            &object.schema,
                            &object.name,
                        )?)
                    })();
                    let foreign = match foreign {
                        Ok(rows) => rows,
                        Err(e) => {
                            warnings.push(format!("Foreign key metadata unavailable: {e}"));
                            vec![]
                        }
                    };
                    for row in foreign {
                        if let (Some(column), Some(target), Some(target_column)) =
                            (row.get(7), row.get(2), row.get(3))
                        {
                            object.keys.push(ForeignKey {
                                column: column.clone(),
                                target: [row.first(), row.get(1), row.get(2)]
                                    .into_iter()
                                    .flatten()
                                    .filter(|s| !s.is_empty())
                                    .cloned()
                                    .collect::<Vec<_>>()
                                    .join("."),
                                target_column: target_column.clone(),
                            });
                            let _ = target;
                        }
                    }
                    let design = crate::schema::TableDesign {
                        name: object.qualified(),
                        columns: object.columns.clone(),
                        primary: object
                            .columns
                            .iter()
                            .filter(|c| c.primary)
                            .map(|c| c.name.clone())
                            .collect(),
                        foreign_keys: object.keys.clone(),
                        ..Default::default()
                    };
                    object.ddl = design.create_sql(source.dialect).unwrap_or_default();
                    for warning in warnings {
                        object.ddl.push_str(&format!("\n-- {warning}"));
                    }
                }
                for (kind, sql) in crate::catalog::extras(source.dialect) {
                    if cancel.load(Ordering::Acquire) {
                        bail!("Canceled");
                    }
                    match (|| -> Result<Vec<Vec<String>>> {
                        let Some(cursor) =
                            c.execute(sql, (), Some(settings.timeout_seconds.clamp(1, 300)))?
                        else {
                            return Ok(vec![]);
                        };
                        read_catalog(cursor)
                    })() {
                        Ok(rows) => {
                            for row in rows {
                                if row.len() < 3 {
                                    continue;
                                }
                                let schema = row[1].clone();
                                if !schemas.is_empty()
                                    && !schema.is_empty()
                                    && !schemas.contains(&schema)
                                {
                                    continue;
                                }
                                objects.push(Object{routine:None,related_ddl:vec![],catalog:row[0].clone(),schema,name:row[2].clone(),kind:kind.into(),columns:vec![],keys:vec![],ddl:format!("-- {kind} {}; use the provider's catalog/DDL routines for the complete definition",row[2])});
                            }
                        }
                        Err(e) => objects.push(Object {
                            routine: None,
                            related_ddl: vec![],
                            catalog: String::new(),
                            schema: String::new(),
                            name: format!("{kind} introspection unavailable"),
                            kind: "CAPABILITY".into(),
                            columns: vec![],
                            keys: vec![],
                            ddl: format!("-- Driver/server reported: {e}"),
                        }),
                    }
                }
                if let Some(signature_sql) = crate::routine::metadata_query(source.dialect) {
                    if cancel.load(Ordering::Acquire) {
                        bail!("Canceled");
                    }
                    let signatures = (|| -> Result<Vec<Object>> {
                        let cursor = c
                            .execute(
                                signature_sql,
                                (),
                                Some(settings.timeout_seconds.clamp(1, 300)),
                            )?
                            .context("Provider routine metadata returned no result")?;
                        crate::routine::signature_objects(read_catalog(cursor)?, schemas)
                    })();
                    match signatures {
                        Ok(mut signatures) => {
                            if source.dialect == Dialect::Oracle {
                                // Unknown/unsupported drivers retain ordinary input-only
                                // routine metadata, but never gain output authority.
                                let capability = c
                                    .oracle_implicit_results(settings.timeout_seconds, cancel)
                                    .ok();
                                for object in &mut signatures {
                                    if let Some(routine) = &mut object.routine {
                                        routine.oracle_implicit_results = capability.clone();
                                    }
                                }
                            }
                            objects.retain(|object| object.kind != "ROUTINE");
                            objects.extend(signatures);
                        }
                        Err(error) => objects.push(Object {
                            routine: None,
                            related_ddl: vec![],
                            catalog: String::new(),
                            schema: String::new(),
                            name: "Routine signature introspection unavailable".into(),
                            kind: "CAPABILITY".into(),
                            columns: vec![],
                            keys: vec![],
                            ddl: format!("-- Driver/server reported: {error}"),
                        }),
                    }
                }
                Ok(objects)
            }
        }
    }
}
fn ssh_forward_host(host: &str) -> Result<String> {
    anyhow::ensure!(
        !host.is_empty()
            && !host.starts_with('-')
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':')),
        "Invalid SSH forwarding host"
    );
    Ok(if host.contains(':') {
        format!("[{host}]")
    } else {
        host.into()
    })
}

fn ssh_command_base(t: &Tunnel) -> Result<std::process::Command> {
    anyhow::ensure!(
        !t.destination.is_empty()
            && !t.destination.starts_with('-')
            && t.destination.chars().all(|c| c.is_ascii_alphanumeric()
                || matches!(c, '.' | '-' | '_' | '@' | ':' | '[' | ']' | '+')),
        "Invalid SSH destination"
    );
    let program = if t.ssh_program.as_os_str().is_empty() {
        std::path::Path::new("ssh")
    } else {
        anyhow::ensure!(
            t.ssh_program.is_absolute()
                && t.ssh_program.is_file()
                && t.ssh_program
                    .to_str()
                    .is_some_and(|s| !s.chars().any(char::is_control)),
            "Choose an existing absolute SSH executable without control characters"
        );
        t.ssh_program.as_path()
    };
    let mut command = std::process::Command::new(program);
    if let Some(config) = &t.config_file {
        validate_ssh_file(config)?;
        command.arg("-F").arg(config);
    }
    command.args([
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "StrictHostKeyChecking=yes",
        "-o",
        "ExitOnForwardFailure=yes",
        "-o",
        "ConnectTimeout=15",
    ]);
    if !t.identity.as_os_str().is_empty() {
        let identity = capture_ssh_file(&t.identity)?;
        command.arg("-i").arg(identity);
    }
    if !t.known_hosts.as_os_str().is_empty() {
        let known_hosts = capture_ssh_file(&t.known_hosts)?;
        command.arg("-o").arg(format!(
            "UserKnownHostsFile=\"{}\"",
            known_hosts
                .to_string_lossy()
                .replace('\\', "\\\\")
                .replace('\"', "\\\"")
        ));
    }
    Ok(command)
}
fn capture_ssh_file(path: &std::path::Path) -> Result<std::path::PathBuf> {
    // Preserve legacy relative identity/known-hosts selections, while freezing
    // the existing file's absolute authority before a consumer changes cwd.
    let captured = std::fs::canonicalize(path).context("Capture selected SSH file")?;
    validate_ssh_file(&captured)?;
    Ok(captured)
}
fn validate_ssh_file(path: &std::path::Path) -> Result<()> {
    anyhow::ensure!(
        path.is_absolute()
            && path.is_file()
            && path
                .to_str()
                .is_some_and(|s| !s.chars().any(char::is_control)),
        "Choose an existing absolute SSH file without control characters"
    );
    Ok(())
}

/// Build a strict SSH stdio-forward command without spawning it. The caller
/// owns captured trust/configuration authority, stdio and physical retirement.
/// Original TLS hostname/SNI stays with that caller's protocol client.
pub(crate) fn captured_ssh_command(
    t: &Tunnel,
    target_host: &str,
    target_port: u16,
) -> Result<std::process::Command> {
    anyhow::ensure!(target_port > 0, "Invalid SSH forwarding port");
    let mut command = ssh_command_base(t)?;
    command
        .arg("-W")
        .arg(format!("{}:{target_port}", ssh_forward_host(target_host)?))
        .arg(&t.destination);
    Ok(command)
}

struct TunnelProcess(Option<std::process::Child>);
impl TunnelProcess {
    fn open(tunnel: Option<&Tunnel>) -> Result<Self> {
        let Some(t) = tunnel else {
            return Ok(Self(None));
        };
        if t.destination.starts_with('-')
            || t.destination.is_empty()
            || t.local_port == 0
            || t.remote_port == 0
        {
            bail!("Invalid SSH tunnel endpoint");
        }
        let mut command = ssh_command_base(t)?;
        command.args(["-N", "-L"]).arg(format!(
            "127.0.0.1:{}:{}:{}",
            t.local_port,
            ssh_forward_host(&t.remote_host)?,
            t.remote_port
        ));
        command
            .arg(&t.destination)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let child = command.spawn().context("Starting admitted SSH tunnel")?;
        Ok(Self(Some(child)))
    }
}
impl Drop for TunnelProcess {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn validate_submit_transaction(
    dialect: Dialect,
    auto_commit: bool,
    changes: &[Change],
) -> Result<()> {
    // Oracle/MySQL DDL can commit the user's outer transaction before executing
    // and destroy its savepoint. Opaque scripts (including executable comments
    // and dynamic SQL) cannot be certified as transaction-preserving here.
    // Typed row mutations remain supported in manual mode; arbitrary scripts
    // require the user to finish earlier work and explicitly choose auto mode.
    if !auto_commit
        && matches!(dialect, Dialect::Mysql | Dialect::Oracle)
        && changes
            .iter()
            .any(|change| matches!(change, Change::Sql(_)))
    {
        bail!(
            "This provider may implicitly commit raw SQL/DDL. Commit or roll back earlier work and choose auto-commit before submitting a script; typed row changes remain supported in manual mode"
        );
    }
    Ok(())
}
fn submit_savepoint(
    dialect: Dialect,
) -> Result<(&'static str, &'static str, Option<&'static str>)> {
    Ok(match dialect {
        Dialect::Sqlite | Dialect::Postgres | Dialect::Mysql | Dialect::DuckDb => (
            "SAVEPOINT rustpy_submit_batch",
            "ROLLBACK TO SAVEPOINT rustpy_submit_batch",
            Some("RELEASE SAVEPOINT rustpy_submit_batch"),
        ),
        Dialect::SqlServer => (
            "SAVE TRANSACTION rustpy_submit_batch",
            "ROLLBACK TRANSACTION rustpy_submit_batch",
            None,
        ),
        Dialect::Oracle => (
            "SAVEPOINT rustpy_submit_batch",
            "ROLLBACK TO rustpy_submit_batch",
            None,
        ),
        Dialect::Db2 => (
            "SAVEPOINT rustpy_submit_batch ON ROLLBACK RETAIN CURSORS",
            "ROLLBACK TO SAVEPOINT rustpy_submit_batch",
            Some("RELEASE SAVEPOINT rustpy_submit_batch"),
        ),
        _ => bail!(
            "This provider does not expose a supported Submit savepoint; earlier pending work has been preserved"
        ),
    })
}
fn read_catalog(mut cursor: impl Cursor) -> Result<Vec<Vec<String>>> {
    let count = cursor.num_result_cols()?;
    let mut rows = vec![];
    while let Some(mut row) = cursor.next_row()? {
        let mut values = vec![];
        for i in 1..=count {
            let value = read_cell(&mut row, i as u16, false, &AtomicBool::new(false))?;
            values.push(if value.is_null() {
                String::new()
            } else {
                value.display()
            });
        }
        rows.push(values);
        if rows.len() >= 10_000 {
            break;
        }
    }
    Ok(rows)
}
/// Explicit local driver-manager enumeration, dispatched by the UI on a worker.
pub fn installed_drivers() -> Result<Vec<String>> {
    Ok(odbc_api::environment()?
        .drivers()?
        .into_iter()
        .map(|d| d.description)
        .collect())
}

fn read_cell(
    row: &mut odbc_api::CursorRow<'_>,
    column: u16,
    binary: bool,
    cancel: &AtomicBool,
) -> Result<Value> {
    let mut bytes = Vec::new();
    let mut wide = Vec::<u16>::new();
    let mut is_null = false;
    loop {
        if cancel.load(Ordering::Acquire) {
            bail!("Canceled");
        }
        let complete = if binary {
            let mut buffer = odbc_api::parameter::VarBinaryArray::<8192>::NULL;
            row.get_data(column, &mut buffer)?;
            match buffer.as_bytes() {
                Some(chunk) => bytes.extend_from_slice(chunk),
                None => is_null = true,
            };
            buffer.is_complete()
        } else {
            // SQL_C_WCHAR: drivers convert to UTF-16 independently of the
            // process locale (SQL_C_CHAR uses the driver's client code page).
            let mut buffer = odbc_api::parameter::VarWCharArray::<4096>::NULL;
            row.get_data(column, &mut buffer)?;
            match buffer.as_slice() {
                Some(chunk) => wide.extend_from_slice(chunk),
                None => is_null = true,
            };
            buffer.is_complete()
        };
        if bytes.len() + wide.len() * 2 > 1024 * 1024 {
            bail!("Cell exceeds 1 MiB; select a smaller substring or use a database-side export");
        }
        if complete || is_null {
            break;
        }
    }
    Ok(if is_null {
        Value::Null
    } else if binary {
        Value::Binary(bytes)
    } else {
        Value::Text(String::from_utf16(&wide).context("Driver returned invalid UTF-16 text")?)
    })
}

fn read_only_statement(d: Dialect, enabled: bool) -> Result<&'static str> {
    Ok(match (d, enabled) {
        (Dialect::Postgres | Dialect::Redshift, true) => "SET default_transaction_read_only=on",
        (Dialect::Postgres | Dialect::Redshift, false) => "SET default_transaction_read_only=off",
        (Dialect::Mysql, true) => "SET SESSION TRANSACTION READ ONLY",
        (Dialect::Mysql, false) => "SET SESSION TRANSACTION READ WRITE",
        (Dialect::ClickHouse, true) => "SET readonly=1",
        (Dialect::ClickHouse, false) => "SET readonly=0",
        _ => bail!(
            "This driver/provider does not expose a session read-only switch; use a restricted database account or its documented connection option"
        ),
    })
}

#[cfg(test)]
mod workspace_session_tests {
    use super::*;
    #[test]
    fn captured_ssh_stdio_command_keeps_strict_policy_and_exact_forwarding_args() {
        let fixture = tempfile::tempdir().unwrap();
        let config = fixture.path().join("ssh config");
        let key = fixture.path().join("identity");
        let hosts = fixture.path().join("known hosts");
        for path in [&config, &key, &hosts] {
            std::fs::write(path, "fixture").unwrap();
        }
        let tunnel = Tunnel {
            destination: "owned@127.0.0.1".into(),
            config_file: Some(config.clone()),
            identity: key.clone(),
            known_hosts: hosts.clone(),
            ..Default::default()
        };
        let command = captured_ssh_command(&tunnel, "2001:db8::1", 6380).unwrap();
        let args = command
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(command.get_program(), "ssh");
        assert!(args
            .windows(2)
            .any(|a| a == ["-F", config.to_str().unwrap()]));
        assert!(args.windows(2).any(|a| a == ["-i", key.to_str().unwrap()]));
        assert!(args.iter().any(|a| a == "StrictHostKeyChecking=yes"));
        assert!(args.iter().any(|a| a == "BatchMode=yes"));
        assert!(args
            .iter()
            .any(|a| a == &format!("UserKnownHostsFile=\"{}\"", hosts.display())));
        assert!(args.windows(2).any(|a| a == ["-W", "[2001:db8::1]:6380"]));
        assert_eq!(args.last().unwrap(), "owned@127.0.0.1");
        assert!(!args.iter().any(|a| a == "-N" || a == "-L"));
        assert!(captured_ssh_command(&tunnel, "host;touch owned", 6380).is_err());
        assert!(captured_ssh_command(&tunnel, "127.0.0.1", 0).is_err());
        let malicious = Tunnel {
            destination: "-oStrictHostKeyChecking=no".into(),
            ..tunnel
        };
        assert!(captured_ssh_command(&malicious, "127.0.0.1", 6380).is_err());
    }

    #[test]
    fn oracle_anonymous_blocks_keep_declarations_and_internal_statements_together() {
        assert!(oracle_anonymous_block(
            crate::routine::ORACLE_IMPLICIT_PROBE
        ));
        assert!(oracle_anonymous_block("  begin p(); end;"));
        assert!(!oracle_anonymous_block("DECLARE_NOT_A_BLOCK"));
        assert!(!oracle_anonymous_block("SELECT 'BEGIN' FROM dual"));
    }

    #[test]
    fn sqlite_manual_submit_remains_rollbackable_and_binary_is_retained() {
        let source = Source {
            file: ":memory:".into(),
            ..Default::default()
        };
        let mut settings = Settings {
            auto_commit: false,
            ..Default::default()
        };
        let mut backend = Backend::open(&source, "", &settings).unwrap();
        let cancel = AtomicBool::new(false);
        backend
            .handle(
                Request::Query {
                    sql: "CREATE TABLE sample(id INTEGER PRIMARY KEY, payload BLOB); COMMIT; BEGIN"
                        .into(),
                    limit: 10,
                },
                &source,
                &mut settings,
                &cancel,
            )
            .unwrap();
        backend
            .handle(
                Request::Submit(vec![Change::Insert {
                    table: "sample".into(),
                    values: vec![
                        ("id".into(), Value::Int(1)),
                        ("payload".into(), Value::Binary(vec![0, 255])),
                    ],
                }]),
                &source,
                &mut settings,
                &cancel,
            )
            .unwrap();
        let Reply::Results(results) = backend
            .handle(
                Request::Query {
                    sql: "SELECT payload FROM sample".into(),
                    limit: 10,
                },
                &source,
                &mut settings,
                &cancel,
            )
            .unwrap()
        else {
            panic!("results")
        };
        assert_eq!(results[0].1.rows[0][0], Value::Binary(vec![0, 255]));
        backend
            .handle(Request::Rollback, &source, &mut settings, &cancel)
            .unwrap();
        let Reply::Results(results) = backend
            .handle(
                Request::Query {
                    sql: "SELECT * FROM sample".into(),
                    limit: 10,
                },
                &source,
                &mut settings,
                &cancel,
            )
            .unwrap()
        else {
            panic!("results")
        };
        assert!(results[0].1.rows.is_empty());
    }
    #[test]
    fn offline_source_never_executes_queries() {
        let mut backend = Backend::Ddl(crate::schema::parse_ddl(
            "CREATE TABLE example(id INTEGER PRIMARY KEY, label TEXT);",
        ));
        let source = Source {
            ddl_only: true,
            ..Default::default()
        };
        let mut settings = Settings::default();
        assert!(
            matches!(backend.handle(Request::Catalog,&source,&mut settings,&AtomicBool::new(false)).unwrap(),Reply::Catalog(objects)if objects.len()==1)
        );
        assert!(backend
            .query("SELECT 1", 10, 1, &AtomicBool::new(false))
            .is_err());
    }
    #[test]
    fn implicit_commit_provider_scripts_are_rejected_before_a_mixed_manual_batch() {
        let row = Change::Insert {
            table: "items".into(),
            values: vec![("id".into(), Value::Int(1))],
        };
        for dialect in [Dialect::Mysql, Dialect::Oracle] {
            assert!(
                validate_submit_transaction(dialect, false, std::slice::from_ref(&row)).is_ok()
            );
            for script in [
                "ALTER TABLE items ADD label INTEGER",
                "/* leading */ CREATE TABLE other(id INT)",
                "/*!50000 ALTER TABLE items ADD label INTEGER */",
                "BEGIN EXECUTE IMMEDIATE 'DROP TABLE items'; END;",
            ] {
                let mixed = [row.clone(), Change::Sql(script.into())];
                assert!(validate_submit_transaction(dialect, false, &mixed).is_err());
                assert!(validate_submit_transaction(dialect, true, &mixed).is_ok());
            }
        }
        assert!(validate_submit_transaction(
            Dialect::Postgres,
            false,
            &[
                row,
                Change::Sql("ALTER TABLE items ADD label INTEGER".into())
            ]
        )
        .is_ok());
    }
}
