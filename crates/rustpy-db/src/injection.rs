//! SQL scopes, static connection imports and source-safe user parameters.
use crate::workspace::*;
use anyhow::{bail, Result};
use std::collections::BTreeMap;
/// Substitute user parameters only outside quoted strings, identifiers and comments.
/// Values are explicit SQL literals, never identifiers or statement fragments.
pub fn bind_user_parameters(
    sql: &str,
    prefix: &str,
    values: &BTreeMap<String, String>,
) -> Result<String> {
    if prefix.is_empty() || prefix.len() > 8 {
        bail!("Parameter prefix must have 1–8 bytes");
    }
    let bytes = sql.as_bytes();
    let mut output = String::new();
    let (mut i, mut quote, mut line, mut block) = (0, None, false, 0usize);
    let mut dollar: Option<String> = None;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(tag) = &dollar {
            if sql[i..].starts_with(tag.as_str()) {
                output.push_str(tag);
                i += tag.len();
                dollar = None;
            } else {
                let ch = sql[i..].chars().next().unwrap();
                output.push(ch);
                i += ch.len_utf8();
            }
            continue;
        }

        if line {
            let ch = sql[i..].chars().next().unwrap();
            output.push(ch);
            i += ch.len_utf8();
            if c == b'\n' {
                line = false;
            }
            continue;
        }
        if block > 0 {
            if bytes[i..].starts_with(b"*/") {
                output.push_str("*/");
                block -= 1;
                i += 2;
            } else if bytes[i..].starts_with(b"/*") {
                output.push_str("/*");
                block += 1;
                i += 2;
            } else {
                let ch = sql[i..].chars().next().unwrap();
                output.push(ch);
                i += ch.len_utf8();
            }
            continue;
        }
        if let Some(q) = quote {
            let ch = sql[i..].chars().next().unwrap();
            output.push(ch);
            i += ch.len_utf8();
            if c == q {
                if bytes.get(i) == Some(&q) {
                    output.push(q as char);
                    i += 1;
                } else {
                    quote = None;
                }
            }
            continue;
        }
        if bytes[i..].starts_with(b"--") {
            output.push_str("--");
            line = true;
            i += 2;
            continue;
        }
        if bytes[i..].starts_with(b"/*") {
            output.push_str("/*");
            block = 1;
            i += 2;
            continue;
        }
        if c == b'$' {
            if let Some(end) = sql[i + 1..].find('$') {
                let tag = &sql[i..i + end + 2];
                if tag[1..tag.len() - 1]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    dollar = Some(tag.into());
                    output.push_str(tag);
                    i += tag.len();
                    continue;
                }
            }
        }
        if c == b'[' {
            quote = Some(b']');
            output.push('[');
            i += 1;
            continue;
        }
        if b"'\"`".contains(&c) {
            quote = Some(c);
            output.push(c as char);
            i += 1;
            continue;
        }
        if sql[i..].starts_with(prefix)
            && !(prefix == ":"
                && (bytes.get(i.wrapping_sub(1)) == Some(&b':') || bytes.get(i + 1) == Some(&b':')))
        {
            let start = i + prefix.len();
            let end = bytes[start..]
                .iter()
                .position(|b| !b.is_ascii_alphanumeric() && *b != b'_')
                .map(|n| start + n)
                .unwrap_or(bytes.len());
            if end > start {
                let name = &sql[start..end];
                let value = values
                    .get(name)
                    .ok_or_else(|| anyhow::anyhow!("Missing parameter {name}"))?;
                validate_literal(value)?;
                output.push_str(value);
                i = end;
                continue;
            }
        }
        let ch = sql[i..].chars().next().unwrap();
        output.push(ch);
        i += ch.len_utf8();
    }
    Ok(output)
}
fn validate_literal(s: &str) -> Result<()> {
    if s.contains('\\') {
        bail!(
            "Use a backslash-free SQL literal; use the grid for values requiring dialect-specific escaping"
        );
    }

    let s = s.trim();
    if s.eq_ignore_ascii_case("NULL")
        || s.eq_ignore_ascii_case("TRUE")
        || s.eq_ignore_ascii_case("FALSE")
        || s.parse::<i64>().is_ok()
        || s.parse::<f64>().is_ok_and(f64::is_finite)
    {
        return Ok(());
    }
    // National character literals (N'...') keep non-ASCII text intact on
    // providers such as SQL Server, where '...' uses the database code page.
    let s = s
        .strip_prefix(['N', 'n'])
        .filter(|rest| rest.starts_with('\''))
        .unwrap_or(s);
    if s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2 {
        let inner = &s[1..s.len() - 1];
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\'' && chars.next() != Some('\'') {
                bail!("Single quotes in parameter literals must be doubled");
            }
        }
        return Ok(());
    }
    bail!("Parameters accept SQL literals only: 'text', N'text', numbers, TRUE/FALSE or NULL")
}
pub fn source_from_url(text: &str, id: u64) -> Result<(Source, String)> {
    let url = url::Url::parse(text.trim())?;
    if url.scheme() == "mongodb+srv" {
        bail!("MongoDB SRV discovery/TLS seed authority is not yet supported by the native single-host source; keep the original URL and configure a supported explicit host instead");
    }
    let dialect = match url.scheme().split('+').next().unwrap_or("") {
        "postgres" | "postgresql" => Dialect::Postgres,
        "mysql" | "mariadb" => Dialect::Mysql,
        "sqlite" => Dialect::Sqlite,
        "mssql" | "sqlserver" => Dialect::SqlServer,
        "oracle" => Dialect::Oracle,
        "db2" => Dialect::Db2,
        "clickhouse" => Dialect::ClickHouse,
        "snowflake" => Dialect::Snowflake,
        "bigquery" => Dialect::BigQuery,
        "redshift" => Dialect::Redshift,
        "mongodb" | "mongodb+srv" => Dialect::Mongo,
        "redis" | "rediss" => Dialect::Redis,
        "cassandra" => Dialect::Cassandra,
        "duckdb" => Dialect::DuckDb,
        _ => bail!("Unsupported database URL scheme"),
    };
    let name = url.host_str().unwrap_or("Local database").to_string();
    let mut options = BTreeMap::new();
    let mut secret = decode_url_component(url.password().unwrap_or(""))?;
    for (k, v) in url.query_pairs() {
        if ["password", "pwd", "token"].contains(&k.to_ascii_lowercase().as_str()) {
            secret = v.into_owned();
        } else {
            options.insert(k.into_owned(), v.into_owned());
        }
    }
    if url.scheme() == "rediss" {
        if options.get("tls").is_some_and(|value| value != "true") {
            bail!("A rediss URL cannot disable TLS");
        }
        options.insert("tls".into(), "true".into());
    }
    let driver = options.remove("driver").unwrap_or_default();
    let file = if dialect == Dialect::Sqlite || dialect == Dialect::DuckDb {
        url.path().into()
    } else {
        Default::default()
    };
    Ok((
        Source {
            id,
            name,
            dialect,
            file,
            host: url.host_str().unwrap_or("").into(),
            port: url.port().unwrap_or(match dialect {
                Dialect::Redis => 6379,
                Dialect::Mongo => 27017,
                _ => 0,
            }),
            database: decode_url_component(url.path().trim_start_matches('/'))?,
            user: decode_url_component(url.username())?,
            driver,
            options,
            ..Default::default()
        },
        secret,
    ))
}
pub fn completions(sql: &str, objects: &[Object]) -> Vec<String> {
    let suffix = sql
        .rsplit(|c: char| !c.is_alphanumeric() && c != '_')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut result = crate::keywords()
        .iter()
        .map(|s| s.to_string())
        .chain(objects.iter().flat_map(|o| {
            std::iter::once(o.name.clone()).chain(o.columns.iter().map(|c| c.name.clone()))
        }))
        .filter(|s| s.to_ascii_lowercase().starts_with(&suffix))
        .collect::<Vec<_>>();
    result.sort();
    result.dedup();
    result
}
pub fn inspect_sql(sql: &str, objects: &[Object]) -> Vec<String> {
    if objects.is_empty() {
        return vec!["Refresh the selected data source to resolve SQL objects".into()];
    }
    let tokens: Vec<_> = sql.split_whitespace().collect();
    let mut issues = vec![];
    for pair in tokens.windows(2) {
        if ["FROM", "JOIN", "UPDATE", "INTO"].contains(&pair[0].to_uppercase().as_str()) {
            let name = pair[1].trim_matches(['"', '`', '[', ']', ',', ';']);
            if !objects.iter().any(|o| {
                o.name.eq_ignore_ascii_case(name) || o.qualified().eq_ignore_ascii_case(name)
            }) && !name.starts_with('(')
            {
                issues.push(format!(
                    "Unresolved object {name} in selected resolution scope"
                ));
            }
        }
    }
    issues
}
pub fn format_sql(sql: &str) -> String {
    let mut result = String::new();
    for (_, statement) in crate::split_statements(sql) {
        if !result.is_empty() {
            result.push_str(";\n\n");
        }
        result.push_str(statement.trim());
    }
    result
}
#[derive(Clone, Debug)]
pub struct Migration {
    pub path: std::path::PathBuf,
    pub framework: String,
    pub revision: String,
    pub dependencies: Vec<String>,
}
/// Reads only the captured project migration files; never imports Django or Alembic.
pub fn migrations(root: &std::path::Path) -> Vec<Migration> {
    let mut out = vec![];
    let mut stack = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > 8 || out.len() > 5000 {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().ok().is_some_and(|t| t.is_symlink()) {
                continue;
            }
            if path.is_dir() {
                if ![".git", ".venv", "venv", "node_modules", "target"]
                    .contains(&entry.file_name().to_string_lossy().as_ref())
                {
                    stack.push((path, depth + 1));
                }
            } else if path.extension().is_some_and(|e| e == "py")
                && path.components().any(|c| {
                    ["migrations", "versions"].contains(&c.as_os_str().to_string_lossy().as_ref())
                })
            {
                if std::fs::metadata(&path)
                    .ok()
                    .is_none_or(|m| m.len() > 2 * 1024 * 1024)
                {
                    continue;
                }
                if let Ok(text) = std::fs::read_to_string(&path) {
                    let framework = if text.contains("alembic") {
                        "Alembic"
                    } else if text.contains("migrations.Migration") {
                        "Django"
                    } else {
                        continue;
                    };
                    let revision = text
                        .lines()
                        .find(|s| s.trim_start().starts_with("revision"))
                        .and_then(|s| s.split_once('='))
                        .map(|(_, s)| s.trim().to_string())
                        .unwrap_or_else(|| {
                            path.file_stem()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into()
                        });
                    let dependencies = text
                        .lines()
                        .filter(|s| s.contains("down_revision") || s.contains("dependencies"))
                        .map(str::to_string)
                        .collect();
                    out.push(Migration {
                        path,
                        framework: framework.into(),
                        revision,
                        dependencies,
                    });
                }
            }
        }
    }
    out
}
/// Imports explicit literal Django DATABASES dictionaries supplied by the facet authority.
/// Computed settings remain unresolved rather than executing project code.
pub fn from_django_literals(
    id: u64,
    name: &str,
    values: &BTreeMap<String, String>,
) -> Result<(Source, String)> {
    let engine = values.get("ENGINE").map(String::as_str).unwrap_or("");
    let dialect = if engine.contains("sqlite") {
        Dialect::Sqlite
    } else if engine.contains("postgres") {
        Dialect::Postgres
    } else if engine.contains("mysql") {
        Dialect::Mysql
    } else if engine.contains("oracle") {
        Dialect::Oracle
    } else {
        bail!("Django database engine requires an explicitly configured native ODBC driver")
    };
    Ok((
        Source {
            id,
            name: name.into(),
            dialect,
            file: values.get("NAME").cloned().unwrap_or_default().into(),
            database: values.get("NAME").cloned().unwrap_or_default(),
            host: values.get("HOST").cloned().unwrap_or_default(),
            port: values.get("PORT").and_then(|s| s.parse().ok()).unwrap_or(0),
            user: values.get("USER").cloned().unwrap_or_default(),
            ..Default::default()
        },
        values.get("PASSWORD").cloned().unwrap_or_default(),
    ))
}

pub fn parse_hex(text: &str) -> Result<Vec<u8>> {
    let compact = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>();
    if !compact.len().is_multiple_of(2) || !compact.is_ascii() {
        bail!("Binary edits require pairs of hexadecimal digits");
    }
    compact
        .as_bytes()
        .chunks(2)
        .map(|pair| Ok(u8::from_str_radix(std::str::from_utf8(pair)?, 16)?))
        .collect()
}

fn decode_url_component(text: &str) -> Result<String> {
    let mut bytes = Vec::new();
    let mut i = 0;
    while i < text.len() {
        if text.as_bytes()[i] == b'%' {
            if i + 2 >= text.len() {
                bail!("Incomplete URL escape");
            }
            bytes.push(u8::from_str_radix(&text[i + 1..i + 3], 16)?);
            i += 3;
        } else {
            bytes.push(text.as_bytes()[i]);
            i += 1;
        }
    }
    Ok(String::from_utf8(bytes)?)
}

#[cfg(test)]
mod native_url_authority_tests {
    use super::*;
    #[test]
    fn rediss_scheme_carries_tls_and_session_secret_without_plaintext_downgrade() {
        let (source, secret) =
            source_from_url("rediss://person:private%40token@db.example/2", 7).unwrap();
        assert_eq!(source.dialect, Dialect::Redis);
        assert_eq!(source.options.get("tls").map(String::as_str), Some("true"));
        assert_eq!(source.port, 6379);
        assert_eq!(secret, "private@token");
        assert!(!serde_json::to_string(&source).unwrap().contains("private"));
        assert!(source_from_url("rediss://db.example/2?tls=false", 7).is_err());
    }
    #[test]
    fn srv_url_is_rejected_before_a_single_host_definition_can_lose_discovery_authority() {
        let error =
            source_from_url("mongodb+srv://person:secret@cluster.example/data", 8).unwrap_err();
        assert!(error
            .to_string()
            .contains("SRV discovery/TLS seed authority"));
        assert!(!error.to_string().contains("secret"));
        let (source, _) = source_from_url("mongodb://localhost/data", 9).unwrap();
        assert_eq!(source.port, 27017);
    }
}
