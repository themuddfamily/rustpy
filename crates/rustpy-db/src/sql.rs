//! SQL helpers: statement splitting, keyword list, completions.

use rustpy_core::{TextRange, TextSize};

use crate::Connection;

const KEYWORDS: &[&str] = &[
    "ABORT",
    "ACTION",
    "ADD",
    "AFTER",
    "ALL",
    "ALTER",
    "ANALYZE",
    "AND",
    "AS",
    "ASC",
    "ATTACH",
    "AUTOINCREMENT",
    "BEFORE",
    "BEGIN",
    "BETWEEN",
    "BY",
    "CASCADE",
    "CASE",
    "CAST",
    "CHECK",
    "COLLATE",
    "COLUMN",
    "COMMIT",
    "CONFLICT",
    "CONSTRAINT",
    "CREATE",
    "CROSS",
    "CURRENT_DATE",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "DATABASE",
    "DEFAULT",
    "DEFERRABLE",
    "DEFERRED",
    "DELETE",
    "DESC",
    "DETACH",
    "DISTINCT",
    "DROP",
    "EACH",
    "ELSE",
    "END",
    "ESCAPE",
    "EXCEPT",
    "EXCLUSIVE",
    "EXISTS",
    "EXPLAIN",
    "FAIL",
    "FILTER",
    "FOR",
    "FOREIGN",
    "FROM",
    "FULL",
    "GLOB",
    "GROUP",
    "HAVING",
    "IF",
    "IGNORE",
    "IMMEDIATE",
    "IN",
    "INDEX",
    "INDEXED",
    "INITIALLY",
    "INNER",
    "INSERT",
    "INSTEAD",
    "INTERSECT",
    "INTO",
    "IS",
    "ISNULL",
    "JOIN",
    "KEY",
    "LEFT",
    "LIKE",
    "LIMIT",
    "MATCH",
    "NATURAL",
    "NO",
    "NOT",
    "NOTNULL",
    "NULL",
    "OF",
    "OFFSET",
    "ON",
    "OR",
    "ORDER",
    "OUTER",
    "OVER",
    "PARTITION",
    "PLAN",
    "PRAGMA",
    "PRIMARY",
    "QUERY",
    "RAISE",
    "RECURSIVE",
    "REFERENCES",
    "REGEXP",
    "REINDEX",
    "RELEASE",
    "RENAME",
    "REPLACE",
    "RESTRICT",
    "RETURNING",
    "RIGHT",
    "ROLLBACK",
    "ROW",
    "ROWS",
    "SAVEPOINT",
    "SELECT",
    "SET",
    "TABLE",
    "TEMP",
    "TEMPORARY",
    "THEN",
    "TO",
    "TRANSACTION",
    "TRIGGER",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USING",
    "VACUUM",
    "VALUES",
    "VIEW",
    "VIRTUAL",
    "WHEN",
    "WHERE",
    "WINDOW",
    "WITH",
    "WITHOUT",
    // Common functions / types
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "LENGTH",
    "LOWER",
    "UPPER",
    "SUBSTR",
    "COALESCE",
    "IFNULL",
    "ROUND",
    "ABS",
    "RANDOM",
    "DATE",
    "TIME",
    "DATETIME",
    "STRFTIME",
    "INTEGER",
    "TEXT",
    "REAL",
    "BLOB",
    "NUMERIC",
    "BOOLEAN",
    "VARCHAR",
];

/// SQL keywords (upper case), SQLite dialect.
pub fn keywords() -> &'static [&'static str] {
    KEYWORDS
}

/// Splits a script into statements on `;` outside strings, quoted
/// identifiers and comments. Ranges are byte offsets of each statement's
/// trimmed text in `sql`.
pub fn split_statements(sql: &str) -> Vec<(TextRange, String)> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    let mut routine = false;
    let mut block_depth = 0usize;
    let push = |a: usize, b: usize, out: &mut Vec<(TextRange, String)>| {
        let raw = &sql[a..b];
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return;
        }
        let lead = raw.len() - raw.trim_start().len();
        let s = a + lead;
        let e = s + trimmed.len();
        out.push((
            TextRange::new(TextSize::new(s as u32), TextSize::new(e as u32)),
            trimmed.to_string(),
        ));
    };
    while i < bytes.len() {
        match bytes[i] {
            b'$' => {
                let end = sql[i + 1..].find('$').map(|n| i + n + 2);
                if let Some(end) = end.filter(|end| {
                    sql[i + 1..end - 1]
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                }) {
                    let tag = &sql[i..end];
                    if let Some(close) = sql[end..].find(tag) {
                        i = end + close + tag.len();
                    } else {
                        i = bytes.len();
                    }
                } else {
                    i += 1;
                }
            }
            byte if byte.is_ascii_alphabetic() || byte == b'_' => {
                let begin = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let word = sql[begin..i].to_ascii_uppercase();
                let prefix = sql[start..begin].to_ascii_uppercase();
                if ["TRIGGER", "PROCEDURE", "FUNCTION"].contains(&word.as_str())
                    && prefix.contains("CREATE")
                {
                    routine = true;
                }
                if routine {
                    if ["BEGIN", "CASE"].contains(&word.as_str()) {
                        block_depth += 1;
                    } else if word == "END" {
                        block_depth = block_depth.saturating_sub(1);
                    }
                }
            }
            b'\'' | b'"' | b'`' => {
                let q = bytes[i];
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == q {
                        if i + 1 < bytes.len() && bytes[i + 1] == q {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }
            b'[' => {
                while i < bytes.len() && bytes[i] != b']' {
                    i += 1;
                }
                i += 1;
            }
            b'-' if i + 1 < bytes.len() && bytes[i + 1] == b'-' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                let mut nested = 1;
                while i + 1 < bytes.len() && nested > 0 {
                    if bytes[i..].starts_with(b"/*") {
                        nested += 1;
                        i += 2;
                    } else if bytes[i..].starts_with(b"*/") {
                        nested -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            b';' if block_depth == 0 => {
                push(start, if routine { i + 1 } else { i }, &mut out);
                i += 1;
                start = i;
                routine = false;
            }
            _ => i += 1,
        }
    }
    push(start, bytes.len().min(sql.len()), &mut out);
    out
}

/// Completion candidates for `prefix` (case-insensitive): table names,
/// column names, then keywords. Keyword candidates follow the case of the
/// prefix (lower-case prefix → lower-case keyword).
pub fn sql_completions(conn: &Connection, prefix: &str) -> Vec<String> {
    let p = prefix.trim();
    let lower = p.to_ascii_lowercase();
    let mut out: Vec<String> = Vec::new();
    let push = |s: String, out: &mut Vec<String>| {
        if !out.contains(&s) {
            out.push(s);
        }
    };
    if let Ok(tables) = conn.tables() {
        for t in &tables {
            if t.name.to_ascii_lowercase().starts_with(&lower) {
                push(t.name.clone(), &mut out);
            }
        }
        for t in &tables {
            for c in &t.columns {
                if c.name.to_ascii_lowercase().starts_with(&lower) {
                    push(c.name.clone(), &mut out);
                }
            }
        }
    }
    let lowercase_kw = !p.is_empty() && p.chars().all(|c| !c.is_ascii_uppercase());
    for kw in KEYWORDS {
        if kw.to_ascii_lowercase().starts_with(&lower) {
            push(
                if lowercase_kw {
                    kw.to_ascii_lowercase()
                } else {
                    kw.to_string()
                },
                &mut out,
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_statements() {
        let sql = "SELECT 'a;b' AS x; -- comment; with semicolon\nINSERT INTO t VALUES (\"q;\"); /* block; */ UPDATE t SET a = 1;\n\n  ";
        let parts = split_statements(sql);
        let texts: Vec<&str> = parts.iter().map(|(_, s)| s.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "SELECT 'a;b' AS x",
                "-- comment; with semicolon\nINSERT INTO t VALUES (\"q;\")",
                "/* block; */ UPDATE t SET a = 1"
            ]
        );
        for (range, text) in &parts {
            assert_eq!(
                &sql[usize::from(range.start())..usize::from(range.end())],
                text
            );
        }
        assert!(split_statements("  ;;  ").is_empty());
        assert_eq!(split_statements("SELECT 1").len(), 1);
        assert_eq!(split_statements("SELECT [a;b] FROM t").len(), 1);
    }

    #[test]
    fn completions() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_script("CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT); CREATE TABLE sessions (user_id INTEGER)").unwrap();
        let comps = sql_completions(&c, "us");
        assert_eq!(comps[0], "users");
        assert!(comps.contains(&"username".to_string()));
        assert!(comps.contains(&"user_id".to_string()));
        assert!(comps.contains(&"using".to_string()));
        let upper = sql_completions(&c, "SEL");
        assert_eq!(upper, vec!["SELECT".to_string()]);
        assert!(keywords().contains(&"WHERE"));
    }
}
