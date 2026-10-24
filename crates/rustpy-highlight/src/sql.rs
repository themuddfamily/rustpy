//! SQL highlighter (generic dialect): keywords, types, functions, strings,
//! quoted identifiers, numbers, parameters, comments.

use rustc_hash::FxHashSet;

use crate::common::{
    comment_tag, find_from, finish, is_ident_continue, is_ident_start, line_end, scan_number, Sink,
};
use crate::{HighlightTag, Span};

static KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "INSERT",
    "INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE",
    "CREATE",
    "TABLE",
    "DROP",
    "ALTER",
    "ADD",
    "COLUMN",
    "INDEX",
    "VIEW",
    "JOIN",
    "INNER",
    "LEFT",
    "RIGHT",
    "FULL",
    "OUTER",
    "CROSS",
    "NATURAL",
    "ON",
    "USING",
    "AS",
    "AND",
    "OR",
    "NOT",
    "NULL",
    "IS",
    "IN",
    "LIKE",
    "ILIKE",
    "BETWEEN",
    "EXISTS",
    "GROUP",
    "BY",
    "ORDER",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "UNION",
    "ALL",
    "DISTINCT",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "PRIMARY",
    "KEY",
    "FOREIGN",
    "REFERENCES",
    "DEFAULT",
    "UNIQUE",
    "CHECK",
    "CONSTRAINT",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "TRANSACTION",
    "WITH",
    "RECURSIVE",
    "RETURNING",
    "ASC",
    "DESC",
    "CAST",
    "TRUE",
    "FALSE",
    "IF",
    "EXCEPT",
    "INTERSECT",
    "FETCH",
    "FIRST",
    "NEXT",
    "ROWS",
    "ONLY",
    "OVER",
    "PARTITION",
    "WINDOW",
    "TRUNCATE",
    "GRANT",
    "REVOKE",
    "TEMPORARY",
    "TEMP",
    "REPLACE",
    "EXPLAIN",
    "ANALYZE",
    "VACUUM",
    "SCHEMA",
    "DATABASE",
    "TRIGGER",
    "FUNCTION",
    "PROCEDURE",
    "RETURNS",
    "RETURN",
    "DECLARE",
    "CURSOR",
    "FOR",
    "LOOP",
    "WHILE",
    "DO",
    "ELSIF",
    "EXECUTE",
    "PREPARE",
    "SAVEPOINT",
    "RELEASE",
    "COLLATE",
    "AUTOINCREMENT",
    "AUTO_INCREMENT",
    "CASCADE",
    "RESTRICT",
    "NO",
    "ACTION",
    "CONFLICT",
    "NOTHING",
    "ANY",
    "SOME",
    "LATERAL",
    "MATERIALIZED",
    "GENERATED",
    "ALWAYS",
    "IDENTITY",
    "SEQUENCE",
    "INCREMENT",
    "START",
    "OWNED",
    "COMMENT",
    "RENAME",
    "TO",
    "ENUM",
    "TYPE",
    "DOMAIN",
    "EXTENSION",
    "LANGUAGE",
    "IMMUTABLE",
    "STABLE",
    "VOLATILE",
    "STRICT",
    "NULLS",
    "LAST",
    "FILTER",
    "WITHIN",
    "ARRAY",
    "UNNEST",
    "SIMILAR",
    "ESCAPE",
    "GLOB",
    "REGEXP",
    "MATCH",
    "PRAGMA",
    "WITHOUT",
    "ROWID",
    "VIRTUAL",
    "INTERVAL",
    "ZONE",
    "AT",
    "LOCAL",
    "SESSION",
    "CURRENT",
];

static TYPES: &[&str] = &[
    "INT",
    "INTEGER",
    "SMALLINT",
    "BIGINT",
    "TINYINT",
    "SERIAL",
    "BIGSERIAL",
    "TEXT",
    "VARCHAR",
    "CHAR",
    "CHARACTER",
    "VARYING",
    "BOOLEAN",
    "BOOL",
    "DATE",
    "TIME",
    "TIMESTAMP",
    "TIMESTAMPTZ",
    "DATETIME",
    "FLOAT",
    "REAL",
    "DOUBLE",
    "PRECISION",
    "DECIMAL",
    "NUMERIC",
    "MONEY",
    "BLOB",
    "BYTEA",
    "JSON",
    "JSONB",
    "UUID",
    "XML",
    "BIT",
    "VARBINARY",
    "BINARY",
    "CLOB",
    "NCHAR",
    "NVARCHAR",
    "INET",
    "CIDR",
    "MACADDR",
    "TSVECTOR",
    "POINT",
    "GEOMETRY",
];

static FUNCTIONS: &[&str] = &[
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "COALESCE",
    "NULLIF",
    "NOW",
    "CURRENT_DATE",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "LENGTH",
    "CHAR_LENGTH",
    "UPPER",
    "LOWER",
    "SUBSTR",
    "SUBSTRING",
    "TRIM",
    "LTRIM",
    "RTRIM",
    "CONCAT",
    "CONCAT_WS",
    "REPLACE",
    "ROUND",
    "FLOOR",
    "CEIL",
    "CEILING",
    "ABS",
    "RANDOM",
    "RAND",
    "DATE_TRUNC",
    "DATE_PART",
    "EXTRACT",
    "TO_CHAR",
    "TO_DATE",
    "TO_NUMBER",
    "JSON_EXTRACT",
    "JSON_AGG",
    "ARRAY_AGG",
    "STRING_AGG",
    "GROUP_CONCAT",
    "ROW_NUMBER",
    "RANK",
    "DENSE_RANK",
    "LAG",
    "LEAD",
    "FIRST_VALUE",
    "LAST_VALUE",
    "NTILE",
    "GREATEST",
    "LEAST",
    "IFNULL",
    "ISNULL",
    "STRFTIME",
    "DATEADD",
    "DATEDIFF",
    "GETDATE",
    "GENERATE_SERIES",
    "POSITION",
    "LEFT",
    "RIGHT",
    "INITCAP",
    "MD5",
    "TYPEOF",
    "PRINTF",
    "FORMAT",
];

fn set(items: &'static [&'static str]) -> FxHashSet<&'static str> {
    items.iter().copied().collect()
}

fn keyword_set() -> &'static FxHashSet<&'static str> {
    static SET: std::sync::OnceLock<FxHashSet<&'static str>> = std::sync::OnceLock::new();
    SET.get_or_init(|| set(KEYWORDS))
}

fn type_set() -> &'static FxHashSet<&'static str> {
    static SET: std::sync::OnceLock<FxHashSet<&'static str>> = std::sync::OnceLock::new();
    SET.get_or_init(|| set(TYPES))
}

fn function_set() -> &'static FxHashSet<&'static str> {
    static SET: std::sync::OnceLock<FxHashSet<&'static str>> = std::sync::OnceLock::new();
    SET.get_or_init(|| set(FUNCTIONS))
}

pub fn highlight_sql(src: &str) -> Vec<Span> {
    let bytes = src.as_bytes();
    let end = bytes.len();
    let mut sink = Sink::default();
    let mut i = 0;
    while i < end {
        let b = bytes[i];
        match b {
            b'-' if i + 1 < end && bytes[i + 1] == b'-' => {
                let e = line_end(bytes, i);
                sink.push(i, e, comment_tag(&src[i..e]));
                i = e;
            }
            b'#' if i == 0 || bytes[i - 1] == b'\n' => {
                let e = line_end(bytes, i);
                sink.push(i, e, comment_tag(&src[i..e]));
                i = e;
            }
            b'/' if i + 1 < end && bytes[i + 1] == b'*' => {
                let e = find_from(bytes, i + 2, b"*/").map(|p| p + 2).unwrap_or(end);
                sink.push(i, e, comment_tag(&src[i..e]));
                i = e;
            }
            b'\'' => {
                // Standard string with `''` escapes (and tolerant of backslash escapes).
                let mut j = i + 1;
                loop {
                    if j >= end {
                        break;
                    }
                    match bytes[j] {
                        b'\\' => j += 2,
                        b'\'' if j + 1 < end && bytes[j + 1] == b'\'' => j += 2,
                        b'\'' => {
                            j += 1;
                            break;
                        }
                        _ => j += 1,
                    }
                }
                let e = j.min(end);
                sink.push(i, e, HighlightTag::String);
                i = e;
            }
            b'"' | b'`' => {
                let mut j = i + 1;
                while j < end && bytes[j] != b && bytes[j] != b'\n' {
                    j += 1;
                }
                let e = if j < end && bytes[j] == b { j + 1 } else { j };
                sink.push(i, e, HighlightTag::Parameter);
                i = e.max(i + 1);
            }
            b'[' | b']' => {
                sink.push(i, i + 1, HighlightTag::Bracket);
                i += 1;
            }
            b'(' | b')' => {
                sink.push(i, i + 1, HighlightTag::Parenthesis);
                i += 1;
            }
            b',' => {
                sink.push(i, i + 1, HighlightTag::Comma);
                i += 1;
            }
            b';' => {
                sink.push(i, i + 1, HighlightTag::Punctuation);
                i += 1;
            }
            b'.' if !(i + 1 < end && bytes[i + 1].is_ascii_digit()) => {
                sink.push(i, i + 1, HighlightTag::Dot);
                i += 1;
            }
            b'0'..=b'9' => {
                let e = scan_number(bytes, i);
                sink.push(i, e, HighlightTag::Number);
                i = e;
            }
            b'.' => {
                let e = scan_number(bytes, i + 1);
                sink.push(i, e, HighlightTag::Number);
                i = e;
            }
            b':' | b'@' | b'$'
                if i + 1 < end
                    && (is_ident_start(bytes[i + 1]) || bytes[i + 1].is_ascii_digit())
                    && !(b == b':' && i > 0 && bytes[i - 1] == b':') =>
            {
                let mut j = i + 1;
                while j < end && is_ident_continue(bytes[j]) {
                    j += 1;
                }
                sink.push(i, j, HighlightTag::Variable);
                i = j;
            }
            b'?' => {
                sink.push(i, i + 1, HighlightTag::Variable);
                i += 1;
            }
            b if is_ident_start(b) => {
                let mut j = i;
                while j < end && is_ident_continue(bytes[j]) {
                    j += 1;
                }
                let word = &src[i..j];
                let upper = word.to_ascii_uppercase();
                let mut k = j;
                while k < end && (bytes[k] == b' ' || bytes[k] == b'\t') {
                    k += 1;
                }
                let next_paren = k < end && bytes[k] == b'(';
                let tag = if keyword_set().contains(upper.as_str())
                    && !(next_paren && function_set().contains(upper.as_str()))
                {
                    Some(HighlightTag::Keyword)
                } else if type_set().contains(upper.as_str()) {
                    Some(HighlightTag::Builtin)
                } else if function_set().contains(upper.as_str()) || next_paren {
                    Some(HighlightTag::FunctionCall)
                } else {
                    None
                };
                if let Some(t) = tag {
                    sink.push(i, j, t);
                }
                i = j;
            }
            b'=' | b'<' | b'>' | b'!' | b'+' | b'-' | b'*' | b'/' | b'%' | b'|' | b'&' | b'^'
            | b'~' | b':' => {
                let mut j = i + 1;
                while j < end
                    && matches!(bytes[j], b'=' | b'<' | b'>' | b'|' | b':' | b'-' | b'*')
                    && j - i < 3
                {
                    j += 1;
                }
                sink.push(i, j, HighlightTag::Operator);
                i = j;
            }
            _ => i += 1,
        }
    }
    finish(src, sink.spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::testing::*;

    const SAMPLE: &str = "-- users\nSELECT u.id, COUNT(*) AS n, \"quoted col\"\nFROM users u LEFT JOIN orders o ON o.user_id = u.id\nWHERE u.name LIKE 'A%' AND o.total >= 10.5 AND u.created > :since\n/* block\n comment */\nGROUP BY u.id HAVING COUNT(*) > 1 ORDER BY n DESC LIMIT 10;\nCREATE TABLE t (id INTEGER PRIMARY KEY, name VARCHAR(50) NOT NULL, price NUMERIC(10, 2));\nINSERT INTO t VALUES (1, 'it''s', $1);\n";

    #[test]
    fn tags() {
        let t = tagged(SAMPLE, &highlight_sql(SAMPLE));
        assert!(has(&t, "-- users", HighlightTag::Comment));
        assert!(has(&t, "SELECT", HighlightTag::Keyword));
        assert!(has(&t, "COUNT", HighlightTag::FunctionCall));
        assert!(has(&t, "\"quoted col\"", HighlightTag::Parameter));
        assert!(has(&t, "LEFT", HighlightTag::Keyword));
        assert!(has(&t, "'A%'", HighlightTag::String));
        assert!(has(&t, "10.5", HighlightTag::Number));
        assert!(has(&t, ":since", HighlightTag::Variable));
        assert!(has(&t, "/* block\n comment */", HighlightTag::Comment));
        assert!(has(&t, "INTEGER", HighlightTag::Builtin));
        assert!(has(&t, "VARCHAR", HighlightTag::Builtin));
        assert!(has(&t, "'it''s'", HighlightTag::String));
        assert!(has(&t, "$1", HighlightTag::Variable));
        assert!(has(&t, ">=", HighlightTag::Operator));
        assert!(has(&t, ";", HighlightTag::Punctuation));
    }

    #[test]
    fn invariants_and_truncation() {
        check_all(highlight_sql, SAMPLE);
        check_all(highlight_sql, "SELECT 'open\n FROM /* open\n");
        check_all(highlight_sql, "SELECT 'привет' AS имя FROM таблица;\n");
    }
}
