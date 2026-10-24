//! Offline DDL catalog and editable schema operations. Never execute generated SQL implicitly.
use crate::workspace::*;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TableDesign {
    pub name: String,
    pub columns: Vec<Column>,
    pub primary: Vec<String>,
    pub indexes: Vec<(String, Vec<String>, bool)>,
    pub foreign_keys: Vec<ForeignKey>,
    pub checks: Vec<String>,
    pub partition: String,
    pub view_query: String,
    #[serde(default)]
    pub drop_constraints: Vec<String>,
    #[serde(default)]
    pub drop_indexes: Vec<String>,
    #[serde(default)]
    pub primary_constraint: Option<String>,
}
impl TableDesign {
    pub fn create_sql(&self, d: Dialect) -> Result<String> {
        if self.name.trim().is_empty() {
            bail!("Object name is required");
        }
        if !self.view_query.trim().is_empty() {
            return Ok(format!(
                "CREATE VIEW {} AS {}",
                d.qualified(&self.name),
                self.view_query
            ));
        }
        if self.columns.is_empty() {
            bail!("A table needs columns");
        }
        let mut parts: Vec<String> = self
            .columns
            .iter()
            .map(|c| {
                format!(
                    "{} {}{}{}",
                    d.quote(&c.name),
                    c.data_type,
                    if c.nullable { "" } else { " NOT NULL" },
                    c.default
                        .as_ref()
                        .map(|v| format!(" DEFAULT {v}"))
                        .unwrap_or_default()
                )
            })
            .collect();
        if !self.primary.is_empty() {
            parts.push(format!(
                "PRIMARY KEY ({})",
                self.primary
                    .iter()
                    .map(|n| d.quote(n))
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
        for fk in &self.foreign_keys {
            parts.push(format!(
                "FOREIGN KEY ({}) REFERENCES {} ({})",
                d.quote(&fk.column),
                d.qualified(&fk.target),
                d.quote(&fk.target_column)
            ));
        }
        for check in &self.checks {
            parts.push(format!("CHECK ({check})"));
        }
        let mut s = format!(
            "CREATE TABLE {} (\n{}\n){};",
            d.qualified(&self.name),
            parts.join(",\n"),
            if self.partition.trim().is_empty() {
                String::new()
            } else {
                format!(" {}", self.partition)
            }
        );
        for (name, columns, unique) in &self.indexes {
            s.push_str(&format!(
                "\nCREATE {}INDEX {} ON {} ({});",
                if *unique { "UNIQUE " } else { "" },
                d.quote(name),
                d.qualified(&self.name),
                columns
                    .iter()
                    .map(|c| d.quote(c))
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
        Ok(s)
    }
}
pub fn parse_ddl(sql: &str) -> Vec<Object> {
    let mut out = vec![];
    for (_, statement) in crate::split_statements(sql) {
        let upper = statement.to_ascii_uppercase();
        let Some(pos) = upper.find("CREATE TABLE") else {
            continue;
        };
        let mut tail = statement[pos + 12..].trim();
        if tail.to_ascii_uppercase().starts_with("IF NOT EXISTS ") {
            tail = tail[14..].trim();
        }
        let Some((name, body)) = tail.split_once('(') else {
            continue;
        };
        let name = name.trim().trim_matches(['"', '`', '[', ']']).to_string();
        let body = body.rsplit_once(')').map(|(s, _)| s).unwrap_or(body);
        let mut columns = vec![];
        for definition in split_columns(body) {
            let mut tokens = definition.split_whitespace();
            let Some(name) = tokens.next() else {
                continue;
            };
            if ["PRIMARY", "FOREIGN", "CONSTRAINT", "CHECK", "UNIQUE"]
                .contains(&name.to_uppercase().as_str())
            {
                continue;
            }
            let ty = tokens.next().unwrap_or("TEXT");
            columns.push(Column {
                name: name.trim_matches(['"', '`', '[', ']']).into(),
                data_type: ty.into(),
                nullable: !definition.to_uppercase().contains("NOT NULL"),
                primary: definition.to_uppercase().contains("PRIMARY KEY"),
                default: None,
            });
        }
        out.push(Object {
            routine: None,
            related_ddl: vec![],
            catalog: String::new(),
            schema: String::new(),
            name,
            kind: "TABLE".into(),
            columns,
            keys: vec![],
            ddl: statement,
        });
    }
    out
}
fn split_columns(body: &str) -> Vec<&str> {
    let (mut depth, mut start, mut quote) = (0i32, 0, None);
    let mut out = vec![];
    let mut chars = body.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if let Some(q) = quote {
            if c == q {
                if chars.peek().is_some_and(|(_, c)| *c == q) {
                    chars.next();
                } else {
                    quote = None;
                }
            }
            continue;
        }
        match c {
            '\'' | '"' | '`' => quote = Some(c),
            '[' => quote = Some(']'),
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(body[start..i].trim());
                start = i + 1
            }
            _ => {}
        }
    }
    out.push(body[start..].trim());
    out
}
/// Mapping is explicit and header names remain identifiers; every value is escaped separately.
pub fn import_delimited(
    text: &str,
    delimiter: u8,
    table: &str,
    mapping: &[(usize, String)],
    has_headers: bool,
) -> Result<Vec<Change>> {
    if mapping.is_empty() {
        bail!("Map input columns before importing");
    }
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(has_headers)
        .from_reader(text.as_bytes());
    let mut changes = vec![];
    for row in reader.records() {
        let row = row?;
        changes.push(Change::Insert {
            table: table.into(),
            values: mapping
                .iter()
                .map(|(index, name)| {
                    Ok((
                        name.clone(),
                        crate::Value::Text(
                            row.get(*index)
                                .ok_or_else(|| anyhow::anyhow!("Mapped column is absent"))?
                                .into(),
                        ),
                    ))
                })
                .collect::<Result<_>>()?,
        });
        if changes.len() > 100_000 {
            bail!("Import exceeds 100000 rows; split the file");
        }
    }
    Ok(changes)
}
pub fn alter_sql(old: &Object, new: &TableDesign, d: Dialect) -> Result<String> {
    let table = d.qualified(&old.qualified());
    let mut statements = vec![];
    if !new.name.is_empty() && new.name != old.qualified() && new.name != old.name {
        bail!("Use an explicit dialect-specific RENAME statement in the query console");
    }
    for c in &new.columns {
        if let Some(previous) = old.columns.iter().find(|o| o.name == c.name) {
            if previous.data_type != c.data_type
                || previous.nullable != c.nullable
                || previous.default != c.default
            {
                if d == Dialect::Sqlite {
                    bail!(
                        "SQLite column changes require a reviewed table rebuild; create the replacement table then migrate data in a manual transaction"
                    );
                }
                let column = d.quote(&c.name);
                let null = if c.nullable { "NULL" } else { "NOT NULL" };
                match d {
                    Dialect::Postgres | Dialect::Redshift => {
                        if previous.data_type != c.data_type {
                            statements.push(format!(
                                "ALTER TABLE {table} ALTER COLUMN {column} TYPE {}",
                                c.data_type
                            ));
                        }
                        if previous.nullable != c.nullable {
                            statements.push(format!(
                                "ALTER TABLE {table} ALTER COLUMN {column} {} NOT NULL",
                                if c.nullable { "DROP" } else { "SET" }
                            ));
                        }
                        if previous.default != c.default {
                            statements.push(format!(
                                "ALTER TABLE {table} ALTER COLUMN {column} {}",
                                c.default
                                    .as_ref()
                                    .map(|v| format!("SET DEFAULT {v}"))
                                    .unwrap_or_else(|| "DROP DEFAULT".into())
                            ));
                        }
                    }
                    Dialect::Mysql => statements.push(format!(
                        "ALTER TABLE {table} MODIFY COLUMN {column} {} {null}{}",
                        c.data_type,
                        c.default
                            .as_ref()
                            .map(|v| format!(" DEFAULT {v}"))
                            .unwrap_or_default()
                    )),
                    Dialect::Oracle => statements.push(format!(
                        "ALTER TABLE {table} MODIFY ({column} {} {null}{})",
                        c.data_type,
                        c.default
                            .as_ref()
                            .map(|v| format!(" DEFAULT {v}"))
                            .unwrap_or_default()
                    )),
                    Dialect::Db2 => {
                        if previous.data_type != c.data_type {
                            statements.push(format!(
                                "ALTER TABLE {table} ALTER COLUMN {column} SET DATA TYPE {}",
                                c.data_type
                            ));
                        }
                        if previous.nullable != c.nullable {
                            statements.push(format!(
                                "ALTER TABLE {table} ALTER COLUMN {column} {} NOT NULL",
                                if c.nullable { "DROP" } else { "SET" }
                            ));
                        }
                    }
                    _ => statements.push(format!(
                        "ALTER TABLE {table} ALTER COLUMN {column} {} {null}",
                        c.data_type
                    )),
                }
            }
        } else {
            statements.push(format!(
                "ALTER TABLE {table} ADD {} {}{}",
                d.quote(&c.name),
                c.data_type,
                if c.nullable { "" } else { " NOT NULL" }
            ));
        }
    }
    for c in &old.columns {
        if !new.columns.iter().any(|n| n.name == c.name) {
            statements.push(format!(
                "ALTER TABLE {table} DROP COLUMN {}",
                d.quote(&c.name)
            ));
        }
    }
    let previous_primary = old
        .columns
        .iter()
        .filter(|c| c.primary)
        .map(|c| c.name.clone())
        .collect::<Vec<_>>();
    if previous_primary != new.primary {
        if !previous_primary.is_empty() {
            if d == Dialect::Mysql {
                statements.push(format!("ALTER TABLE {table} DROP PRIMARY KEY"));
            } else {
                bail!(
                    "Changing an existing primary key requires its constraint name; inspect the provider catalog and queue DROP CONSTRAINT explicitly"
                );
            }
        }
        if !new.primary.is_empty() {
            statements.push(format!(
                "ALTER TABLE {table} ADD PRIMARY KEY ({})",
                new.primary
                    .iter()
                    .map(|c| d.quote(c))
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
    }
    for (name, columns, unique) in &new.indexes {
        statements.push(format!(
            "CREATE {}INDEX {} ON {table} ({})",
            if *unique { "UNIQUE " } else { "" },
            d.quote(name),
            columns
                .iter()
                .map(|c| d.quote(c))
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    for name in &new.drop_constraints {
        statements.push(format!(
            "ALTER TABLE {table} DROP CONSTRAINT {}",
            d.quote(name)
        ));
    }
    for name in &new.drop_indexes {
        statements.push(if matches!(d, Dialect::Mysql | Dialect::SqlServer) {
            format!("DROP INDEX {} ON {table}", d.quote(name))
        } else {
            format!("DROP INDEX {}", d.qualified(name))
        });
    }
    for check in &new.checks {
        statements.push(format!("ALTER TABLE {table} ADD CHECK ({check})"));
    }
    for fk in &new.foreign_keys {
        if !old.keys.iter().any(|k| {
            k.column == fk.column && k.target == fk.target && k.target_column == fk.target_column
        }) {
            statements.push(format!(
                "ALTER TABLE {table} ADD FOREIGN KEY ({}) REFERENCES {} ({})",
                d.quote(&fk.column),
                d.qualified(&fk.target),
                d.quote(&fk.target_column)
            ));
        }
    }
    if !new.partition.is_empty() {
        statements.push(format!("ALTER TABLE {table} {}", new.partition));
    }
    Ok(statements.join(";\n"))
}
/// Reviewed SQLite replacement plan; the worker owns FK suspension, transaction and restoration.
pub fn sqlite_rebuild_preview(old: &Object, new: &TableDesign) -> Result<String> {
    let upper = old.ddl.to_ascii_uppercase();
    if [
        "UNIQUE",
        "CHECK",
        "AUTOINCREMENT",
        "WITHOUT ROWID",
        "STRICT",
        "GENERATED",
        "COLLATE",
    ]
    .iter()
    .any(|clause| upper.contains(clause))
    {
        bail!(
            "This table has constraints/options requiring a source-preserving migration. Use its complete DDL and related indexes/triggers in the SQL console; the simplified designer refuses to discard them"
        );
    }

    let mut replacement = new.clone();
    replacement.name = format!("__rustpy_rebuild_{}", old.name);
    replacement.indexes.clear();
    let common = old
        .columns
        .iter()
        .filter(|old| new.columns.iter().any(|new| new.name == old.name))
        .map(|c| crate::quote_ident(&c.name))
        .collect::<Vec<_>>();
    if common.is_empty() {
        bail!("Rebuild requires a common column to preserve existing rows");
    }
    let columns = common.join(",");
    let mut sql = format!(
        "{}\nINSERT INTO {} ({columns}) SELECT {columns} FROM {};\nDROP TABLE {};\nALTER TABLE {} RENAME TO {};",
        replacement.create_sql(Dialect::Sqlite)?,
        crate::quote_ident(&replacement.name),
        crate::quote_ident(&old.name),
        crate::quote_ident(&old.name),
        crate::quote_ident(&replacement.name),
        crate::quote_ident(&old.name)
    );
    for ddl in &old.related_ddl {
        if !new
            .drop_indexes
            .iter()
            .chain(new.indexes.iter().map(|(name, _, _)| name))
            .any(|name| {
                ddl.contains(&crate::quote_ident(name)) || ddl.contains(&format!(" {name} "))
            })
        {
            sql.push_str(&format!("\n{ddl};"));
        }
    }
    for (name, columns, unique) in &new.indexes {
        sql.push_str(&format!(
            "\nCREATE {}INDEX {} ON {} ({});",
            if *unique { "UNIQUE " } else { "" },
            crate::quote_ident(name),
            crate::quote_ident(&old.name),
            columns
                .iter()
                .map(|c| crate::quote_ident(c))
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    Ok(sql)
}
