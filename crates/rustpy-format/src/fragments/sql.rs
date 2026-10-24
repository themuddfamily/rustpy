//! Generic SQL trivia formatting. The existing SQL highlighter identifies
//! clauses; a bounded lossless lexer protects dialect literals/comments and
//! verifies that no token or newline-dependent string concatenation changes.
use super::{Error, Options};
use rustpy_highlight::HighlightTag;
use std::collections::HashSet;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Word,
    Quoted,
    Comment,
    Open,
    Close,
    Comma,
    Operator,
    Other,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Token<'a> {
    kind: Kind,
    text: &'a str,
    start: usize,
    end: usize,
    quoted_newline: bool,
}
fn quoted(source: &str, start: usize, quote: u8) -> Result<usize, Error> {
    let bytes = source.as_bytes();
    let mut end = start + 1;
    while end < bytes.len() {
        if bytes[end] == b'\\' {
            end = end.checked_add(2).ok_or(Error::Budget("SQL literal"))?;
            continue;
        }
        if bytes[end] == quote {
            if bytes.get(end + 1) == Some(&quote) {
                end += 2;
                continue;
            }
            return Ok(end + 1);
        }
        end += 1;
    }
    Err(Error::Syntax("Unterminated SQL quote".into()))
}
fn tokens(source: &str) -> Result<Vec<Token<'_>>, Error> {
    let bytes = source.as_bytes();
    let mut result = Vec::<Token<'_>>::new();
    let mut at = 0usize;
    let mut depth = 0usize;
    while at < bytes.len() {
        if bytes[at].is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if result.len() >= super::MAX_TOKENS {
            return Err(Error::Budget("SQL tokens"));
        }
        let start = at;
        let mut kind = Kind::Other;
        if bytes[at..].starts_with(b"--") || bytes[at] == b'#' {
            at = source[at..]
                .find('\n')
                .map_or(bytes.len(), |offset| at + offset + 1);
            kind = Kind::Comment;
        } else if bytes[at..].starts_with(b"/*") {
            let mut nesting = 1usize;
            at += 2;
            while at < bytes.len() && nesting > 0 {
                if bytes[at..].starts_with(b"/*") {
                    nesting += 1;
                    if nesting > 64 {
                        return Err(Error::Budget("SQL comment nesting"));
                    }
                    at += 2;
                } else if bytes[at..].starts_with(b"*/") {
                    nesting -= 1;
                    at += 2;
                } else {
                    at += 1;
                }
            }
            if nesting != 0 {
                return Err(Error::Syntax("Unterminated SQL block comment".into()));
            }
            kind = Kind::Comment;
        } else if matches!(bytes[at], b'\'' | b'"' | b'`') {
            at = quoted(source, at, bytes[at])?;
            kind = Kind::Quoted;
        } else if bytes[at] == b'[' {
            at += 1;
            loop {
                let Some(offset) = source[at..].find(']') else {
                    return Err(Error::Syntax("Unterminated SQL bracket identifier".into()));
                };
                at += offset + 1;
                if bytes.get(at) == Some(&b']') {
                    at += 1;
                    continue;
                }
                break;
            }
            kind = Kind::Quoted;
        } else if bytes[at] == b'$' {
            let mut end = at + 1;
            while bytes
                .get(end)
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            {
                end += 1;
            }
            if bytes.get(end) == Some(&b'$') && (end == at + 1 || !bytes[at + 1].is_ascii_digit()) {
                if end - at > 64 {
                    return Err(Error::Budget("SQL dollar delimiter"));
                }
                let delimiter = &source[at..=end];
                let content = end + 1;
                let Some(close) = source[content..].find(delimiter) else {
                    return Err(Error::Syntax("Unterminated SQL dollar string".into()));
                };
                at = content + close + delimiter.len();
                kind = Kind::Quoted;
            } else {
                at = end.max(at + 1);
                kind = Kind::Word;
            }
        } else if bytes[at] == b'\\' || matches!(bytes[at], b'{' | b'}') {
            return Err(Error::Unsafe(
                "SQL client commands/brace escapes require a dialect formatter".into(),
            ));
        } else if bytes[at] == b'(' {
            depth += 1;
            if depth > 64 {
                return Err(Error::Budget("SQL parentheses"));
            }
            at += 1;
            kind = Kind::Open;
        } else if bytes[at] == b')' {
            if depth == 0 {
                return Err(Error::Syntax("Unbalanced SQL parentheses".into()));
            }
            depth -= 1;
            at += 1;
            kind = Kind::Close;
        } else if bytes[at] == b',' {
            at += 1;
            kind = Kind::Comma;
        } else if b"=<>!+-*/%|&^~".contains(&bytes[at]) {
            at += 1;
            while bytes
                .get(at)
                .is_some_and(|byte| b"=<>!+-*/%|&^~".contains(byte))
            {
                if bytes[at..].starts_with(b"--") || bytes[at..].starts_with(b"/*") {
                    break;
                }
                at += 1;
            }
            kind = Kind::Operator;
        } else if bytes[at].is_ascii_digit()
            || (bytes[at] == b'.' && bytes.get(at + 1).is_some_and(u8::is_ascii_digit))
        {
            at += 1;
            while let Some(byte) = bytes.get(at) {
                if byte.is_ascii_alphanumeric()
                    || matches!(byte, b'.' | b'_')
                    || (matches!(byte, b'+' | b'-')
                        && matches!(bytes.get(at - 1), Some(b'e' | b'E')))
                {
                    at += 1;
                } else {
                    break;
                }
            }
            kind = Kind::Word;
        } else if bytes[at].is_ascii_alphanumeric()
            || bytes[at] >= 128
            || matches!(bytes[at], b'_' | b':' | b'@' | b'?')
        {
            at += 1;
            while bytes.get(at).is_some_and(|byte| {
                byte.is_ascii_alphanumeric()
                    || *byte >= 128
                    || matches!(byte, b'_' | b'.' | b':' | b'@' | b'?' | b'$')
            }) {
                at += 1;
            }
            kind = Kind::Word;
            let prefix = source[start..at].to_ascii_lowercase();
            if prefix == "u"
                && bytes.get(at) == Some(&b'&')
                && matches!(bytes.get(at + 1), Some(b'\'' | b'"'))
            {
                at = quoted(source, at + 1, bytes[at + 1])?;
                kind = Kind::Quoted;
            } else if bytes.get(at) == Some(&b'\'') && matches!(prefix.as_str(), "q" | "nq") {
                let open = *bytes
                    .get(at + 1)
                    .ok_or_else(|| Error::Syntax("Unterminated Oracle quote".into()))?;
                if open.is_ascii_whitespace() || open >= 128 {
                    return Err(Error::Unsafe("Unsupported Oracle quote delimiter".into()));
                }
                let close = match open {
                    b'[' => b']',
                    b'{' => b'}',
                    b'(' => b')',
                    b'<' => b'>',
                    other => other,
                };
                let mut cursor = at + 2;
                while cursor + 1 < bytes.len()
                    && !(bytes[cursor] == close && bytes[cursor + 1] == b'\'')
                {
                    cursor += 1;
                }
                if cursor + 1 >= bytes.len() {
                    return Err(Error::Syntax(
                        "Unterminated Oracle alternative quote".into(),
                    ));
                }
                at = cursor + 2;
                kind = Kind::Quoted;
            } else if bytes.get(at) == Some(&b'\'')
                && matches!(prefix.as_str(), "e" | "n" | "b" | "x" | "u&")
            {
                at = quoted(source, at, b'\'')?;
                kind = Kind::Quoted;
            }
        } else {
            at += 1;
        }
        if !source.is_char_boundary(at) {
            return Err(Error::Unsafe("Unsupported SQL token boundary".into()));
        }
        let quoted_newline = kind == Kind::Quoted
            && result.last().is_some_and(|previous| {
                previous.kind == Kind::Quoted && source[previous.end..start].contains(['\n', '\r'])
            });
        result.push(Token {
            kind,
            text: &source[start..at],
            start,
            end: at,
            quoted_newline,
        });
    }
    if depth != 0 {
        return Err(Error::Syntax("Unbalanced SQL parentheses".into()));
    }
    Ok(result)
}
fn equivalent(left: &[Token<'_>], right: &[Token<'_>]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            left.kind == right.kind
                && left.text == right.text
                && left.quoted_newline == right.quoted_newline
        })
}
pub(super) fn format(source: &str, options: Options) -> Result<String, Error> {
    let before = tokens(source)?;
    if before
        .iter()
        .any(|token| token.kind == Kind::Word && token.text.eq_ignore_ascii_case("delimiter"))
    {
        return Err(Error::Unsafe(
            "SQL DELIMITER directives are client syntax".into(),
        ));
    }
    let keyword: HashSet<_> = rustpy_highlight::highlight_sql(source)
        .into_iter()
        .filter(|span| span.tag == HighlightTag::Keyword)
        .map(|span| {
            (
                usize::from(span.range.start()),
                usize::from(span.range.end()),
            )
        })
        .collect();
    let newline = super::newline(source);
    let mut output = String::new();
    let mut depth = 0usize;
    for (index, token) in before.iter().enumerate() {
        let previous = index.checked_sub(1).map(|index| &before[index]);
        let gap = previous.map_or(&source[..token.start], |previous| {
            &source[previous.end..token.start]
        });
        if token.kind == Kind::Close {
            depth = depth.saturating_sub(1);
        }
        let clause = token.kind == Kind::Word
            && keyword.contains(&(token.start, token.end))
            && matches!(
                token.text.to_ascii_uppercase().as_str(),
                "SELECT"
                    | "FROM"
                    | "WHERE"
                    | "HAVING"
                    | "LIMIT"
                    | "OFFSET"
                    | "UNION"
                    | "EXCEPT"
                    | "INTERSECT"
                    | "RETURNING"
                    | "VALUES"
                    | "SET"
                    | "JOIN"
                    | "GROUP"
                    | "ORDER"
            );
        if token.quoted_newline {
            output.push_str(gap);
        } else if previous
            .is_some_and(|previous| previous.kind == Kind::Comment && previous.text.ends_with('\n'))
        {
            output.push_str(&" ".repeat(depth * options.indent));
        } else if !gap.is_empty() {
            if clause || gap.contains(['\n', '\r']) {
                output.push_str(newline);
                output.push_str(&" ".repeat(depth * options.indent));
            } else {
                output.push(' ');
            }
        } else if previous.is_some_and(|previous| {
            matches!(previous.kind, Kind::Comma | Kind::Operator) && token.kind != Kind::Close
        }) {
            output.push(' ');
        }
        if token.kind == Kind::Operator
            && previous.is_some_and(|previous| !matches!(previous.kind, Kind::Open | Kind::Comment))
            && !output.ends_with([' ', '\n', '\r'])
        {
            output.push(' ');
        }
        output.push_str(token.text);
        if token.kind == Kind::Open {
            depth += 1;
        }
        super::checked_growth(&output)?;
    }
    if let Some(last) = before.last() {
        output.push_str(&source[last.end..]);
    } else {
        output.push_str(source);
    }
    let after = tokens(&output)?;
    if !equivalent(&before, &after) {
        return Err(Error::Unsafe(
            "SQL token or quoted-newline identity would change".into(),
        ));
    }
    Ok(output)
}
