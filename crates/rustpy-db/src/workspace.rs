//! Project-owned database workspace. Credentials are deliberately absent from persisted sources.
use crate::{quote_ident, QueryResult, Value};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dialect {
    #[default]
    Sqlite,
    Postgres,
    Mysql,
    Oracle,
    SqlServer,
    Db2,
    ClickHouse,
    Snowflake,
    BigQuery,
    Redshift,
    Mongo,
    Redis,
    Cassandra,
    DuckDb,
}
impl Dialect {
    pub const ALL: &'static [Self] = &[
        Self::Sqlite,
        Self::Postgres,
        Self::Mysql,
        Self::Oracle,
        Self::SqlServer,
        Self::Db2,
        Self::ClickHouse,
        Self::Snowflake,
        Self::BigQuery,
        Self::Redshift,
        Self::Mongo,
        Self::Redis,
        Self::Cassandra,
        Self::DuckDb,
    ];
    pub fn quote(self, name: &str) -> String {
        match self {
            Self::Mysql => format!("`{}`", name.replace('`', "``")),
            Self::SqlServer => format!("[{}]", name.replace(']', "]]")),
            _ => quote_ident(name),
        }
    }
    pub fn qualified(self, name: &str) -> String {
        name.split('.')
            .map(|n| self.quote(n))
            .collect::<Vec<_>>()
            .join(".")
    }
    pub fn page(
        self,
        table: &str,
        filter: &str,
        order: &str,
        offset: usize,
        limit: usize,
    ) -> String {
        let mut s = format!("SELECT * FROM {}", self.qualified(table));
        if !filter.trim().is_empty() {
            s.push_str(&format!(" WHERE ({filter})"));
        }
        if !order.trim().is_empty() {
            s.push_str(&format!(" ORDER BY {order}"));
        }
        match self {
            Self::SqlServer | Self::Oracle | Self::Db2 => {
                if order.trim().is_empty() && self == Self::SqlServer {
                    s.push_str(" ORDER BY (SELECT NULL)");
                }
                s.push_str(&format!(
                    " OFFSET {offset} ROWS FETCH NEXT {limit} ROWS ONLY"
                ));
            }
            _ => s.push_str(&format!(" LIMIT {limit} OFFSET {offset}")),
        };
        s
    }
    pub fn explain(self, sql: &str) -> String {
        match self {
            Self::Sqlite => format!("EXPLAIN QUERY PLAN {sql}"),
            Self::SqlServer => format!("SET SHOWPLAN_XML ON; {sql}; SET SHOWPLAN_XML OFF"),
            Self::Oracle => {
                format!("EXPLAIN PLAN FOR {sql}; SELECT * FROM TABLE(DBMS_XPLAN.DISPLAY())")
            }
            _ => format!("EXPLAIN {sql}"),
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Source {
    pub id: u64,
    pub name: String,
    pub dialect: Dialect,
    /// SQLite file or offline DDL file. Other dialects use a registered native ODBC driver.
    pub file: PathBuf,
    pub ddl_only: bool,
    pub driver: String,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub user: String,
    /// Driver-specific options, including SSL certificate/key/verification and cloud warehouse/project.
    /// Password/token/key material must be supplied in the session credential field, never here.
    pub options: BTreeMap<String, String>,
    pub schemas: Vec<String>,
    pub introspection_depth: usize,
    pub read_only: bool,
    pub ssh: Option<Tunnel>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tunnel {
    /// Explicit local OpenSSH executable; empty preserves PATH lookup.
    #[serde(default)]
    pub ssh_program: PathBuf,
    /// Optional private OpenSSH configuration, selected by the user.
    pub config_file: Option<PathBuf>,
    pub destination: String,
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
    pub identity: PathBuf,
    pub known_hosts: PathBuf,
}
impl Source {
    pub fn validate(&self) -> Result<()> {
        for key in self.options.keys() {
            if secret_key(key) {
                bail!("Credentials belong in the session-only field");
            }
            if key.contains([';', '=', '{', '}', '\0']) {
                bail!("Invalid driver option name");
            }
        }
        Ok(())
    }
    pub fn connection_string(&self, secret: &str) -> Result<String> {
        self.validate()?;
        if self.driver.trim().is_empty() {
            bail!(
                "Select an installed ODBC driver; no driver is downloaded or connected automatically"
            );
        }
        let mut pairs = self.options.clone();
        for key in pairs.keys() {
            if secret_key(key) {
                bail!("Credentials belong in the session-only secret field");
            }
        }
        pairs.insert("Driver".into(), self.driver.clone());
        let host = if self.ssh.is_some() {
            "127.0.0.1"
        } else {
            self.host.as_str()
        };
        let port = self.ssh.as_ref().map(|s| s.local_port).unwrap_or(self.port);
        if self.dialect == Dialect::SqlServer {
            // Microsoft's SQL Server ODBC drivers ignore a Port attribute and
            // silently dial 1433; the port must be part of Server (FreeTDS
            // accepts the same `host,port` form). An explicit `host,port` or
            // named instance (`host\instance`) is left as the user wrote it.
            if !host.is_empty() {
                let server = if port > 0 && !host.contains([',', '\\']) {
                    format!("{host},{port}")
                } else {
                    host.to_string()
                };
                pairs.insert("Server".into(), server);
            }
        } else if self.dialect == Dialect::Oracle {
            // Oracle's ODBC driver ignores Server/Port/Database and resolves
            // only DBQ (a TNS alias or EZConnect `//host:port/service`).
            // An explicit DBQ option is left exactly as the user wrote it.
            if !pairs.keys().any(|k| k.eq_ignore_ascii_case("DBQ")) {
                let dbq = if !host.is_empty() {
                    let host = if host.contains(':') && !host.starts_with('[') {
                        format!("[{host}]")
                    } else {
                        host.to_string()
                    };
                    let port = if port > 0 {
                        format!(":{port}")
                    } else {
                        String::new()
                    };
                    format!("//{host}{port}/{}", self.database)
                } else {
                    self.database.clone()
                };
                if !dbq.is_empty() {
                    pairs.insert("DBQ".into(), dbq);
                }
            }
        } else {
            if !host.is_empty() {
                pairs.insert("Server".into(), host.into());
            }
            if port > 0 {
                pairs.insert("Port".into(), port.to_string());
            }
        }
        if !self.database.is_empty() && self.dialect != Dialect::Oracle {
            pairs.insert("Database".into(), self.database.clone());
        }
        if !self.user.is_empty() {
            pairs.insert("UID".into(), self.user.clone());
        }
        if !secret.is_empty() {
            pairs.insert("PWD".into(), secret.into());
        }
        let mut connection = String::new();
        for (key, value) in pairs {
            // Driver names require braces, but drivers may treat braces on
            // ordinary scalar attributes literally (notably PostgreSQL Port).
            // The dependency's encoder also handles PostgreSQL '+' passwords.
            // Protect surrounding whitespace and brace syntax explicitly;
            // reject NUL without including session credentials in diagnostics.
            anyhow::ensure!(
                !value.contains('\0'),
                "ODBC attribute values cannot contain NUL"
            );
            let encoded = if key == "Driver" || value.contains(['{', '}']) || value.trim() != value
            {
                format!("{{{}}}", value.replace('}', "}}"))
            } else {
                odbc_api::escape_attribute_value(&value).into_owned()
            };
            connection.push_str(&format!("{key}={encoded};"));
        }
        Ok(connection)
    }
}
fn secret_key(s: &str) -> bool {
    let s = s.to_ascii_lowercase();
    [
        "password",
        "pwd",
        "token",
        "secret",
        "credential",
        "privatekey",
    ]
    .iter()
    .any(|k| s.contains(k))
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub result_limit: usize,
    pub page_size: usize,
    pub timeout_seconds: usize,
    pub auto_commit: bool,
    pub allow_local: bool,
    pub parameter_prefix: String,
    pub scripts_directory: PathBuf,
    pub extractors_directory: PathBuf,
    pub scopes: Vec<Scope>,
    pub extractors: Vec<Extractor>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            result_limit: 1000,
            page_size: 100,
            timeout_seconds: 30,
            auto_commit: true,
            allow_local: true,
            parameter_prefix: ":".into(),
            scripts_directory: PathBuf::from("sql"),
            extractors_directory: PathBuf::from("extractors"),
            scopes: vec![],
            extractors: vec![],
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Scope {
    pub path: PathBuf,
    pub source: u64,
    pub dialect: Dialect,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Extractor {
    pub name: String,
    pub program: PathBuf,
    pub arguments: Vec<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Workspace {
    pub sources: Vec<Source>,
    pub settings: Settings,
    pub history: Vec<History>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct History {
    pub source: u64,
    pub session: String,
    pub sql: String,
    pub success: bool,
    pub elapsed_ms: u128,
}
impl Workspace {
    pub fn load(root: &Path) -> Self {
        std::fs::read(root.join(".rustpy/database-workspace.json"))
            .ok()
            .and_then(|s| serde_json::from_slice(&s).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, root: &Path) -> Result<()> {
        for source in &self.sources {
            for key in source.options.keys() {
                if secret_key(key) {
                    bail!("Remove credential option {key} before saving");
                }
            }
        }
        let path = root.join(".rustpy/database-workspace.json");
        std::fs::create_dir_all(path.parent().unwrap())?;
        let staged = path.with_extension("json.new");
        std::fs::write(&staged, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(staged, path)?;
        Ok(())
    }
    pub fn scope(&self, path: &Path) -> Option<&Scope> {
        self.settings
            .scopes
            .iter()
            .filter(|s| path.starts_with(&s.path))
            .max_by_key(|s| s.path.components().count())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Object {
    #[serde(default)]
    pub routine: Option<crate::routine::Routine>,
    #[serde(default)]
    pub related_ddl: Vec<String>,
    pub catalog: String,
    pub schema: String,
    pub name: String,
    pub kind: String,
    pub columns: Vec<Column>,
    pub keys: Vec<ForeignKey>,
    pub ddl: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub primary: bool,
    pub default: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ForeignKey {
    pub column: String,
    pub target: String,
    pub target_column: String,
}
impl Object {
    pub fn qualified(&self) -> String {
        [
            self.catalog.as_str(),
            self.schema.as_str(),
            self.name.as_str(),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(".")
    }
}
pub fn dialect_literal(value: &Value, d: Dialect) -> Result<String> {
    if let Value::Text(text) = value {
        let hex = text
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        match d {
            Dialect::Mysql => return Ok(format!("CONVERT(X'{hex}' USING utf8mb4)")),
            Dialect::Postgres | Dialect::Redshift => {
                return Ok(format!("convert_from(decode('{hex}','hex'),'UTF8')"));
            }
            Dialect::SqlServer => return Ok(format!("N'{}'", text.replace('\'', "''"))),
            _ => {}
        }
    }

    if let Value::Binary(bytes) = value {
        let hex = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        return Ok(match d {
            Dialect::Postgres | Dialect::Redshift => format!("decode('{hex}','hex')"),
            Dialect::SqlServer => format!("0x{hex}"),
            Dialect::Oracle => format!("HEXTORAW('{hex}')"),
            _ => format!("X'{hex}'"),
        });
    }
    literal(value)
}
pub fn literal(value: &Value) -> Result<String> {
    Ok(match value {
        Value::Null => "NULL".into(),
        Value::Int(v) => v.to_string(),
        Value::Real(v) if v.is_finite() => v.to_string(),
        Value::Text(v) => format!("'{}'", v.replace('\'', "''")),
        Value::Binary(bytes) => format!(
            "X'{}'",
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        ),
        _ => bail!("Use a bound binary parameter for binary/nonfinite values"),
    })
}
#[derive(Clone, Debug)]
pub enum Change {
    SqliteRebuild {
        old: Object,
        design: Box<crate::schema::TableDesign>,
    },
    Update {
        table: String,
        key: Vec<(String, Value)>,
        column: String,
        value: Value,
    },
    Delete {
        table: String,
        key: Vec<(String, Value)>,
    },
    Insert {
        table: String,
        values: Vec<(String, Value)>,
    },
    Sql(String),
}
impl Change {
    pub fn sql(&self, d: Dialect) -> Result<String> {
        let predicate = |key: &[(String, Value)]| -> Result<String> {
            if key.is_empty() {
                bail!("A complete primary key is required to change a row");
            }
            key.iter()
                .map(|(n, v)| {
                    Ok(if v.is_null() {
                        format!("{} IS NULL", d.quote(n))
                    } else {
                        format!("{} = {}", d.quote(n), dialect_literal(v, d)?)
                    })
                })
                .collect::<Result<Vec<_>>>()
                .map(|v| v.join(" AND "))
        };
        Ok(match self {
            Self::SqliteRebuild { old, design } => {
                crate::schema::sqlite_rebuild_preview(old, design)?
            }
            Self::Update {
                table,
                key,
                column,
                value,
            } => format!(
                "UPDATE {} SET {} = {} WHERE {}",
                d.qualified(table),
                d.quote(column),
                dialect_literal(value, d)?,
                predicate(key)?
            ),
            Self::Delete { table, key } => format!(
                "DELETE FROM {} WHERE {}",
                d.qualified(table),
                predicate(key)?
            ),
            Self::Insert { table, values } => {
                if values.is_empty() {
                    bail!("Insert needs at least one mapped column");
                }
                format!(
                    "INSERT INTO {} ({}) VALUES ({})",
                    d.qualified(table),
                    values
                        .iter()
                        .map(|(n, _)| d.quote(n))
                        .collect::<Vec<_>>()
                        .join(","),
                    values
                        .iter()
                        .map(|(_, v)| dialect_literal(v, d))
                        .collect::<Result<Vec<_>>>()?
                        .join(",")
                )
            }
            Self::Sql(s) => s.clone(),
        })
    }
}
pub fn schema_diff(before: &[Object], after: &[Object], d: Dialect) -> String {
    let mut output = String::new();
    for object in after {
        match before.iter().find(|o| o.qualified() == object.qualified()) {
            None => {
                output.push_str(&object.ddl);
                output.push_str(";\n");
            }
            Some(old) => {
                let changed = old
                    .columns
                    .iter()
                    .map(|c| (&c.name, &c.data_type, c.nullable, c.primary, &c.default))
                    .collect::<Vec<_>>()
                    != object
                        .columns
                        .iter()
                        .map(|c| (&c.name, &c.data_type, c.nullable, c.primary, &c.default))
                        .collect::<Vec<_>>();
                if changed {
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
                    match crate::schema::alter_sql(old, &design, d) {
                        Ok(sql) => output.push_str(&format!("{sql};\n")),
                        Err(e) => output.push_str(&format!(
                            "-- {}: {e}\n-- Old DDL:\n{}\n-- New DDL:\n{}\n",
                            object.qualified(),
                            old.ddl,
                            object.ddl
                        )),
                    }
                } else if old.ddl != object.ddl {
                    output.push_str(&format!(
                        "-- Definition changed for {}\n-- Before:\n{}\n-- After:\n{}\n",
                        object.qualified(),
                        old.ddl,
                        object.ddl
                    ));
                }
            }
        }
    }
    for old in before {
        if !after.iter().any(|o| o.qualified() == old.qualified()) {
            output.push_str(&format!(
                "-- Removed {}; review before DROP\n",
                old.qualified()
            ));
        }
    }
    output
}
pub fn diagram(objects: &[Object]) -> String {
    let mut s = "digraph database {\nrankdir=LR;\n".to_string();
    for o in objects {
        let name = serde_json::to_string(&o.qualified()).unwrap();
        s.push_str(&format!(
            "{name} [shape=record,label={}];\n",
            serde_json::to_string(&format!(
                "{}|{}",
                o.name,
                o.columns
                    .iter()
                    .map(|c| c.name.clone())
                    .collect::<Vec<_>>()
                    .join("\\l")
            ))
            .unwrap()
        ));
        for fk in &o.keys {
            s.push_str(&format!(
                "{name} -> {} [label={}];\n",
                serde_json::to_string(&fk.target).unwrap(),
                serde_json::to_string(&fk.column).unwrap()
            ));
        }
    }
    s.push_str("}\n");
    s
}
pub fn export(result: &QueryResult, format: &str, table: &str, d: Dialect) -> Result<String> {
    let text = match format {
        "CSV" => crate::export_csv(result),
        "TSV" => delimited(result, '\t'),
        "JSON" => serde_json::to_string_pretty(
            &result
                .rows
                .iter()
                .map(|row| {
                    result
                        .columns
                        .iter()
                        .zip(row)
                        .map(|(k, v)| {
                            (
                                k.clone(),
                                match v {
                                    Value::Null => serde_json::Value::Null,
                                    Value::Int(i) => (*i).into(),
                                    Value::Real(f) => serde_json::json!(f),
                                    _ => v.display().into(),
                                },
                            )
                        })
                        .collect::<serde_json::Map<_, _>>()
                })
                .collect::<Vec<_>>(),
        )?,
        "SQL" => result
            .rows
            .iter()
            .map(|row| {
                Change::Insert {
                    table: table.into(),
                    values: result
                        .columns
                        .iter()
                        .cloned()
                        .zip(row.iter().cloned())
                        .collect(),
                }
                .sql(d)
                .map(|s| format!("{s};\n"))
            })
            .collect::<Result<String>>()?,
        "Markdown" => {
            let mut s = format!(
                "|{}|\n|{}|\n",
                result.columns.join("|"),
                vec!["---"; result.columns.len()].join("|")
            );
            for row in &result.rows {
                s.push_str(&format!(
                    "|{}|\n",
                    row.iter()
                        .map(|v| v.display().replace('|', "\\|").replace('\n', "<br>"))
                        .collect::<Vec<_>>()
                        .join("|")
                ));
            }
            s
        }
        "HTML" => {
            fn esc(s: &str) -> String {
                s.replace('&', "&amp;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;")
                    .replace('"', "&quot;")
            }
            let mut s = "<table><thead><tr>".to_string();
            for c in &result.columns {
                s.push_str(&format!("<th>{}</th>", esc(c)));
            }
            s.push_str("</tr></thead><tbody>");
            for row in &result.rows {
                s.push_str("<tr>");
                for v in row {
                    s.push_str(&format!("<td>{}</td>", esc(&v.display())));
                }
                s.push_str("</tr>");
            }
            s.push_str("</tbody></table>");
            s
        }
        _ => bail!("Unknown extractor; XLSX/Excel and Big Data connectors require a plugin"),
    };
    Ok(text)
}
fn delimited(r: &QueryResult, delimiter: char) -> String {
    let cell = |s: String| {
        if s.contains(['"', '\n', '\r', delimiter]) {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s
        }
    };
    std::iter::once(r.columns.clone())
        .chain(
            r.rows
                .iter()
                .map(|row| row.iter().map(Value::display).collect()),
        )
        .map(|row| {
            row.into_iter()
                .map(cell)
                .collect::<Vec<_>>()
                .join(&delimiter.to_string())
                + "\n"
        })
        .collect()
}
pub fn statistics(r: &QueryResult, column: usize) -> String {
    let values: Vec<_> = r.rows.iter().filter_map(|row| row.get(column)).collect();
    let numbers: Vec<f64> = values
        .iter()
        .filter_map(|v| match v {
            Value::Int(n) => Some(*n as f64),
            Value::Real(n) => Some(*n),
            Value::Text(s) => s.parse().ok(),
            _ => None,
        })
        .collect();
    let sum: f64 = numbers.iter().sum();
    format!(
        "Loaded rows: {} · NULL: {} · numeric: {} · sum: {} · mean: {} · min: {:?} · max: {:?}",
        values.len(),
        values.iter().filter(|v| v.is_null()).count(),
        numbers.len(),
        sum,
        if numbers.is_empty() {
            0.0
        } else {
            sum / numbers.len() as f64
        },
        numbers.iter().copied().reduce(f64::min),
        numbers.iter().copied().reduce(f64::max)
    )
}

#[cfg(test)]
mod tunnel_compatibility_tests {
    use super::Tunnel;
    use std::path::PathBuf;

    #[test]
    fn legacy_tunnel_json_preserves_connection_and_ambient_ssh_on_roundtrip() {
        let legacy = serde_json::json!({
            "destination": "developer@host",
            "local_port": 15432,
            "remote_host": "database.internal",
            "remote_port": 5432,
            "identity": "/private/id_ed25519",
            "known_hosts": "/private/known_hosts"
        });
        let tunnel: Tunnel = serde_json::from_value(legacy.clone()).unwrap();
        assert!(tunnel.ssh_program.as_os_str().is_empty());
        assert_eq!(tunnel.config_file, None);
        let encoded = serde_json::to_value(&tunnel).unwrap();
        for (key, value) in legacy.as_object().unwrap() {
            assert_eq!(encoded.get(key), Some(value), "legacy field {key}");
        }
        let restored: Tunnel = serde_json::from_value(encoded).unwrap();
        assert_eq!(restored, tunnel);

        let selected = Tunnel {
            ssh_program: PathBuf::from("/selected/bin/ssh"),
            config_file: Some(PathBuf::from("/private/ssh_config")),
            ..restored
        };
        let selected_roundtrip: Tunnel =
            serde_json::from_slice(&serde_json::to_vec(&selected).unwrap()).unwrap();
        assert_eq!(selected_roundtrip, selected);
    }
}
