//! Database tools backend (the "Database" tool window,
//! roadmap §6): SQLite data sources persisted per project, schema
//! introspection, queries with row limits, table paging/editing, CSV export
//! and SQL helpers (statement splitting, completions, keywords).

pub mod catalog;
pub mod extractor;
pub mod injection;
pub mod native;
pub mod routine;
pub mod schema;
pub mod session;
pub mod sql;
pub mod workspace;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context as _};
use rusqlite::types::ValueRef;
use serde::{Deserialize, Serialize};

pub use sql::{keywords, split_statements, sql_completions};

// ---------------------------------------------------------------------------
// Data sources
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum DataSourceKind {
    Sqlite {
        path: PathBuf,
    },
    External {
        configuration: Box<workspace::Source>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataSource {
    pub name: String,
    pub kind: DataSourceKind,
}

impl DataSource {
    pub fn sqlite(name: &str, path: impl Into<PathBuf>) -> Self {
        Self {
            name: name.to_string(),
            kind: DataSourceKind::Sqlite { path: path.into() },
        }
    }

    /// Short description for lists, e.g. `SQLite · app.db`.
    pub fn label(&self) -> String {
        match &self.kind {
            DataSourceKind::External { configuration } => {
                format!("{:?} · {}", configuration.dialect, configuration.host)
            }
            DataSourceKind::Sqlite { path } => format!(
                "SQLite · {}",
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string())
            ),
        }
    }
}

pub fn data_sources_path(project: &Path) -> PathBuf {
    project.join(".rustpy").join("dataSources.json")
}

/// Data sources of a project (`<project>/.rustpy/dataSources.json`); missing file → empty.
pub fn load_data_sources(project: &Path) -> Vec<DataSource> {
    std::fs::read_to_string(data_sources_path(project))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_data_sources(project: &Path, sources: &[DataSource]) -> anyhow::Result<()> {
    let path = data_sources_path(project);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(sources)?)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Values and results
// ---------------------------------------------------------------------------

/// A cell value. Blobs only carry their length (contents are not loaded).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Blob(usize),
    /// Retained binary cells in the asynchronous workspace; legacy queries keep Blob length.
    Binary(Vec<u8>),
}

impl Value {
    /// Display text for grids / CSV.
    pub fn display(&self) -> String {
        match self {
            Value::Null => "NULL".into(),
            Value::Int(i) => i.to_string(),
            Value::Real(r) => {
                if r.fract() == 0.0 && r.abs() < 1e15 {
                    format!("{r:.1}")
                } else {
                    r.to_string()
                }
            }
            Value::Text(t) => t.clone(),
            Value::Blob(n) => format!("<blob {n} B>"),
            Value::Binary(bytes) => bytes.iter().map(|b| format!("{b:02x}")).collect(),
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Parses text typed in a grid cell: `NULL`, integers, reals, else text.
    pub fn parse_literal(text: &str, data_type: &str) -> Value {
        let t = text.trim();
        if t.eq_ignore_ascii_case("null") {
            return Value::Null;
        }
        let ty = data_type.to_ascii_uppercase();
        if ty.contains("INT") {
            if let Ok(i) = t.parse::<i64>() {
                return Value::Int(i);
            }
        }
        if ty.contains("REAL")
            || ty.contains("FLOA")
            || ty.contains("DOUB")
            || ty.contains("NUM")
            || ty.contains("DEC")
        {
            if let Ok(r) = t.parse::<f64>() {
                return Value::Real(r);
            }
        }
        if ty.is_empty() {
            if let Ok(i) = t.parse::<i64>() {
                return Value::Int(i);
            }
            if let Ok(r) = t.parse::<f64>() {
                return Value::Real(r);
            }
        }
        Value::Text(text.to_string())
    }

    fn from_ref(v: ValueRef<'_>) -> Value {
        match v {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(i) => Value::Int(i),
            ValueRef::Real(r) => Value::Real(r),
            ValueRef::Text(t) => Value::Text(String::from_utf8_lossy(t).into_owned()),
            ValueRef::Blob(b) => Value::Blob(b.len()),
        }
    }

    fn to_sql_value(&self) -> anyhow::Result<rusqlite::types::Value> {
        Ok(match self {
            Value::Null => rusqlite::types::Value::Null,
            Value::Int(i) => rusqlite::types::Value::Integer(*i),
            Value::Real(r) => rusqlite::types::Value::Real(*r),
            Value::Text(t) => rusqlite::types::Value::Text(t.clone()),
            Value::Binary(bytes) => rusqlite::types::Value::Blob(bytes.clone()),
            Value::Blob(_) => bail!("blob values cannot be written from the grid"),
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
    /// Rows changed by the last non-SELECT statement.
    pub affected: Option<usize>,
    pub elapsed: Duration,
    /// More rows were available than `max_rows`.
    pub truncated: bool,
}

impl QueryResult {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.columns.is_empty()
    }

    /// Human summary like `12 rows (0.3 ms)` / `3 rows affected`.
    pub fn summary(&self) -> String {
        let ms = self.elapsed.as_secs_f64() * 1000.0;
        match self.affected {
            Some(n) if self.columns.is_empty() => format!("{n} row(s) affected ({ms:.1} ms)"),
            _ => format!(
                "{} row(s){} ({ms:.1} ms)",
                self.rows.len(),
                if self.truncated { "+" } else { "" }
            ),
        }
    }
}

/// RFC 4180-ish CSV with a header row.
pub fn export_csv(result: &QueryResult) -> String {
    fn cell(s: &str) -> String {
        if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s.to_string()
        }
    }
    let mut out = String::new();
    out.push_str(
        &result
            .columns
            .iter()
            .map(|c| cell(c))
            .collect::<Vec<_>>()
            .join(","),
    );
    out.push('\n');
    for row in &result.rows {
        let line: Vec<String> = row
            .iter()
            .map(|v| match v {
                Value::Null => String::new(),
                other => cell(&other.display()),
            })
            .collect();
        out.push_str(&line.join(","));
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TableKind {
    Table,
    View,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub primary_key: bool,
    pub default: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableInfo {
    pub name: String,
    pub kind: TableKind,
    pub columns: Vec<ColumnInfo>,
    pub row_count: Option<u64>,
}

impl TableInfo {
    pub fn primary_key(&self) -> Option<&str> {
        self.columns
            .iter()
            .find(|c| c.primary_key)
            .map(|c| c.name.as_str())
    }
}

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

pub struct Connection {
    conn: rusqlite::Connection,
    pub source: DataSource,
}

impl Connection {
    pub fn open(ds: &DataSource) -> anyhow::Result<Connection> {
        let conn = match &ds.kind {
            DataSourceKind::External { .. } => {
                bail!("External drivers require a retained database console session")
            }
            DataSourceKind::Sqlite { path } => {
                if path.as_os_str() == ":memory:" {
                    rusqlite::Connection::open_in_memory()?
                } else {
                    rusqlite::Connection::open(path)
                        .with_context(|| format!("cannot open {}", path.display()))?
                }
            }
        };
        conn.busy_timeout(Duration::from_secs(5))?;
        Ok(Connection {
            conn,
            source: ds.clone(),
        })
    }

    pub fn open_in_memory() -> anyhow::Result<Connection> {
        Self::open(&DataSource::sqlite("memory", ":memory:"))
    }

    /// Tables and views (excluding SQLite internals) with their columns.
    pub fn tables(&self) -> anyhow::Result<Vec<TableInfo>> {
        let mut stmt = self.conn.prepare("SELECT name, type FROM sqlite_master WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' ORDER BY type, name")?;
        let names: Vec<(String, String)> = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .filter_map(Result::ok)
            .collect();
        let mut out = Vec::new();
        for (name, ty) in names {
            let kind = if ty == "view" {
                TableKind::View
            } else {
                TableKind::Table
            };
            let columns = self.columns(&name)?;
            let row_count = self
                .conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {}", quote_ident(&name)),
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .ok()
                .map(|n| n.max(0) as u64);
            out.push(TableInfo {
                name,
                kind,
                columns,
                row_count,
            });
        }
        Ok(out)
    }

    pub fn columns(&self, table: &str) -> anyhow::Result<Vec<ColumnInfo>> {
        let mut stmt = self
            .conn
            .prepare(&format!("PRAGMA table_info({})", quote_ident(table)))?;
        let cols = stmt
            .query_map([], |r| {
                Ok(ColumnInfo {
                    name: r.get::<_, String>(1)?,
                    data_type: r.get::<_, String>(2).unwrap_or_default(),
                    nullable: r.get::<_, i64>(3).unwrap_or(0) == 0,
                    primary_key: r.get::<_, i64>(5).unwrap_or(0) > 0,
                    default: r.get::<_, Option<String>>(4).unwrap_or(None),
                })
            })?
            .filter_map(Result::ok)
            .collect();
        Ok(cols)
    }

    fn run_statement(&self, sql: &str, max_rows: usize) -> anyhow::Result<QueryResult> {
        let started = Instant::now();
        let mut stmt = self.conn.prepare(sql)?;
        if stmt.column_count() == 0 {
            let affected = stmt.execute([])?;
            return Ok(QueryResult {
                columns: Vec::new(),
                rows: Vec::new(),
                affected: Some(affected),
                elapsed: started.elapsed(),
                truncated: false,
            });
        }
        let columns: Vec<String> = stmt.column_names().iter().map(|c| c.to_string()).collect();
        let ncols = columns.len();
        let mut rows = Vec::new();
        let mut truncated = false;
        let mut query = stmt.query([])?;
        while let Some(row) = query.next()? {
            if rows.len() >= max_rows {
                truncated = true;
                break;
            }
            let mut values = Vec::with_capacity(ncols);
            for i in 0..ncols {
                values.push(Value::from_ref(row.get_ref(i)?));
            }
            rows.push(values);
        }
        Ok(QueryResult {
            columns,
            rows,
            affected: None,
            elapsed: started.elapsed(),
            truncated,
        })
    }

    /// Runs one or more statements; the result of the last statement is
    /// returned (rows for SELECT-like statements, `affected` otherwise), with
    /// `affected` summed over all modifying statements.
    pub fn query(&self, sql: &str, max_rows: usize) -> anyhow::Result<QueryResult> {
        let started = Instant::now();
        let statements = split_statements(sql);
        if statements.is_empty() {
            bail!("no SQL statement to run");
        }
        let mut total_affected: Option<usize> = None;
        let mut last = QueryResult::default();
        for (_, text) in statements {
            let r = self
                .run_statement(&text, max_rows)
                .with_context(|| format!("in statement: {}", text.lines().next().unwrap_or("")))?;
            if let Some(a) = r.affected {
                total_affected = Some(total_affected.unwrap_or(0) + a);
            }
            last = r;
        }
        if last.columns.is_empty() {
            last.affected = total_affected;
        }
        last.elapsed = started.elapsed();
        Ok(last)
    }

    /// Executes a script (several statements) without collecting results.
    pub fn execute_script(&self, sql: &str) -> anyhow::Result<()> {
        self.conn.execute_batch(sql)?;
        Ok(())
    }

    /// One page of a table, optionally ordered by `(column, ascending)`.
    pub fn table_data(
        &self,
        name: &str,
        offset: usize,
        limit: usize,
        order_by: Option<(String, bool)>,
    ) -> anyhow::Result<QueryResult> {
        let mut sql = format!("SELECT * FROM {}", quote_ident(name));
        if let Some((col, asc)) = order_by {
            sql.push_str(&format!(
                " ORDER BY {} {}",
                quote_ident(&col),
                if asc { "ASC" } else { "DESC" }
            ));
        }
        sql.push_str(&format!(" LIMIT {} OFFSET {}", limit.max(1), offset));
        self.run_statement(&sql, limit.max(1))
    }

    /// Updates one cell identified by its primary key value.
    pub fn update_cell(
        &self,
        table: &str,
        pk_column: &str,
        pk_value: &Value,
        column: &str,
        new_value: &Value,
    ) -> anyhow::Result<()> {
        let sql = format!(
            "UPDATE {} SET {} = ?1 WHERE {} = ?2",
            quote_ident(table),
            quote_ident(column),
            quote_ident(pk_column)
        );
        let n = self.conn.execute(
            &sql,
            rusqlite::params![new_value.to_sql_value()?, pk_value.to_sql_value()?],
        )?;
        if n == 0 {
            return Err(anyhow!("no row with {pk_column} = {}", pk_value.display()));
        }
        Ok(())
    }

    /// Deletes rows matching a primary key value.
    pub fn delete_row(
        &self,
        table: &str,
        pk_column: &str,
        pk_value: &Value,
    ) -> anyhow::Result<usize> {
        let sql = format!(
            "DELETE FROM {} WHERE {} = ?1",
            quote_ident(table),
            quote_ident(pk_column)
        );
        Ok(self
            .conn
            .execute(&sql, rusqlite::params![pk_value.to_sql_value()?])?)
    }

    /// Inserts a row with the given column values.
    pub fn insert_row(&self, table: &str, values: &[(String, Value)]) -> anyhow::Result<()> {
        if values.is_empty() {
            self.conn.execute(
                &format!("INSERT INTO {} DEFAULT VALUES", quote_ident(table)),
                [],
            )?;
            return Ok(());
        }
        let cols: Vec<String> = values.iter().map(|(c, _)| quote_ident(c)).collect();
        let marks: Vec<String> = (1..=values.len()).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            quote_ident(table),
            cols.join(", "),
            marks.join(", ")
        );
        let params: Vec<rusqlite::types::Value> = values
            .iter()
            .map(|(_, v)| v.to_sql_value())
            .collect::<anyhow::Result<_>>()?;
        self.conn
            .execute(&sql, rusqlite::params_from_iter(params))?;
        Ok(())
    }

    /// `EXPLAIN QUERY PLAN` lines for a statement.
    pub fn explain(&self, sql: &str) -> anyhow::Result<Vec<String>> {
        let first = split_statements(sql)
            .into_iter()
            .next()
            .map(|(_, s)| s)
            .unwrap_or_else(|| sql.to_string());
        let mut stmt = self.conn.prepare(&format!("EXPLAIN QUERY PLAN {first}"))?;
        let ncols = stmt.column_count();
        let mut out = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            // id, parent, notused, detail
            let detail = if ncols >= 4 {
                row.get::<_, String>(3).unwrap_or_default()
            } else {
                (0..ncols)
                    .map(|i| Value::from_ref(row.get_ref(i).unwrap_or(ValueRef::Null)).display())
                    .collect::<Vec<_>>()
                    .join(" ")
            };
            let parent = if ncols >= 2 {
                row.get::<_, i64>(1).unwrap_or(0)
            } else {
                0
            };
            out.push(if parent > 0 {
                format!("  {detail}")
            } else {
                detail
            });
        }
        Ok(out)
    }

    /// The DDL that created a table or view.
    pub fn ddl(&self, name: &str) -> anyhow::Result<String> {
        Ok(self
            .conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = ?1",
                [name],
                |r| r.get::<_, Option<String>>(0),
            )?
            .unwrap_or_default())
    }

    /// Database file path (or `:memory:`).
    pub fn path(&self) -> &Path {
        match &self.source.kind {
            DataSourceKind::Sqlite { path } => path,
            DataSourceKind::External { .. } => Path::new(""),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_script(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL, data BLOB, note TEXT DEFAULT 'n/a');
             INSERT INTO users (name, score, data) VALUES ('alice', 1.5, x'0102'), ('bob', 2.0, NULL), ('carol, \"c\"', NULL, NULL);
             CREATE VIEW top AS SELECT name FROM users WHERE score > 1.6;",
        )
        .unwrap();
        c
    }

    #[test]
    fn schema_introspection() {
        let c = sample();
        let tables = c.tables().unwrap();
        assert_eq!(tables.len(), 2);
        let users = tables.iter().find(|t| t.name == "users").unwrap();
        assert_eq!(users.kind, TableKind::Table);
        assert_eq!(users.row_count, Some(3));
        assert_eq!(users.primary_key(), Some("id"));
        let name = users.columns.iter().find(|c| c.name == "name").unwrap();
        assert_eq!(name.data_type, "TEXT");
        assert!(!name.nullable);
        let note = users.columns.iter().find(|c| c.name == "note").unwrap();
        assert_eq!(note.default.as_deref(), Some("'n/a'"));
        let top = tables.iter().find(|t| t.name == "top").unwrap();
        assert_eq!(top.kind, TableKind::View);
        assert_eq!(top.row_count, Some(1));
        assert!(c.ddl("users").unwrap().starts_with("CREATE TABLE users"));
    }

    #[test]
    fn queries_and_limits() {
        let c = sample();
        let r = c
            .query("SELECT id, name, score, data FROM users ORDER BY id", 10)
            .unwrap();
        assert_eq!(r.columns, vec!["id", "name", "score", "data"]);
        assert_eq!(r.rows.len(), 3);
        assert_eq!(r.rows[0][1], Value::Text("alice".into()));
        assert_eq!(r.rows[0][2], Value::Real(1.5));
        assert_eq!(r.rows[0][3], Value::Blob(2));
        assert_eq!(r.rows[1][3], Value::Null);
        assert!(!r.truncated);
        let r = c.query("SELECT * FROM users", 2).unwrap();
        assert_eq!(r.rows.len(), 2);
        assert!(r.truncated);
        let r = c
            .query(
                "UPDATE users SET score = 3 WHERE id = 1; UPDATE users SET score = 4 WHERE id = 2",
                10,
            )
            .unwrap();
        assert_eq!(r.affected, Some(2));
        assert!(r.columns.is_empty());
        assert!(r.summary().contains("2 row(s) affected"));
        assert!(c.query("SELECT * FROM nope", 10).is_err());
        assert!(c.query("   ", 10).is_err());
    }

    #[test]
    fn table_paging_and_order() {
        let c = sample();
        let page = c
            .table_data("users", 1, 1, Some(("name".into(), true)))
            .unwrap();
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0][1], Value::Text("bob".into()));
        let desc = c
            .table_data("users", 0, 10, Some(("id".into(), false)))
            .unwrap();
        assert_eq!(desc.rows[0][0], Value::Int(3));
        assert_eq!(desc.rows.len(), 3);
    }

    #[test]
    fn edit_rows() {
        let c = sample();
        c.update_cell(
            "users",
            "id",
            &Value::Int(2),
            "name",
            &Value::Text("robert".into()),
        )
        .unwrap();
        let r = c.query("SELECT name FROM users WHERE id = 2", 1).unwrap();
        assert_eq!(r.rows[0][0], Value::Text("robert".into()));
        assert!(c
            .update_cell("users", "id", &Value::Int(99), "name", &Value::Null)
            .is_err());
        c.insert_row(
            "users",
            &[
                ("name".into(), Value::Text("dave".into())),
                ("score".into(), Value::Real(9.0)),
            ],
        )
        .unwrap();
        assert_eq!(
            c.delete_row("users", "name", &Value::Text("dave".into()))
                .unwrap(),
            1
        );
        assert_eq!(c.tables().unwrap()[0].row_count, Some(3));
        assert_eq!(Value::parse_literal("42", "INTEGER"), Value::Int(42));
        assert_eq!(Value::parse_literal("null", "TEXT"), Value::Null);
        assert_eq!(Value::parse_literal("1.5", "REAL"), Value::Real(1.5));
        assert_eq!(
            Value::parse_literal("x", "INTEGER"),
            Value::Text("x".into())
        );
    }

    #[test]
    fn csv_export_and_explain() {
        let c = sample();
        let r = c
            .query("SELECT id, name, score FROM users ORDER BY id", 10)
            .unwrap();
        let csv = export_csv(&r);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines[0], "id,name,score");
        assert_eq!(lines[1], "1,alice,1.5");
        assert_eq!(lines[3], "3,\"carol, \"\"c\"\"\",");
        let plan = c.explain("SELECT * FROM users WHERE id = 1").unwrap();
        assert!(!plan.is_empty());
        assert!(plan.join(" ").contains("users"), "{plan:?}");
    }

    #[test]
    fn data_source_persistence() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_data_sources(dir.path()).is_empty());
        let sources = vec![
            DataSource::sqlite("app", dir.path().join("app.db")),
            DataSource::sqlite("mem", ":memory:"),
        ];
        save_data_sources(dir.path(), &sources).unwrap();
        assert_eq!(load_data_sources(dir.path()), sources);
        assert_eq!(sources[0].label(), "SQLite · app.db");
        // Opening a file-backed source creates the database.
        let conn = Connection::open(&sources[0]).unwrap();
        conn.execute_script("CREATE TABLE t (x INTEGER)").unwrap();
        assert!(dir.path().join("app.db").exists());
        assert_eq!(conn.path(), dir.path().join("app.db"));
    }
}

#[cfg(test)]
mod workspace_tests;
