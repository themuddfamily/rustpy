//! TOML highlighter: tables, keys, strings (incl. multi-line), numbers, dates, booleans.

use crate::common::{comment_tag, finish, line_end, scan_number, scan_quoted, scan_triple, Sink};
use crate::{HighlightTag, Span};

pub fn highlight_toml(src: &str) -> Vec<Span> {
    let bytes = src.as_bytes();
    let mut sink = Sink::default();
    let mut i = 0;
    let mut at_line_start = true;
    let mut after_eq = false;
    let mut in_table_header = false;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' {
            at_line_start = true;
            after_eq = false;
            in_table_header = false;
            i += 1;
            continue;
        }
        if b == b' ' || b == b'\t' || b == b'\r' {
            i += 1;
            continue;
        }
        if b == b'#' {
            let end = line_end(bytes, i);
            sink.push(i, end, comment_tag(&src[i..end]));
            i = end;
            continue;
        }
        if at_line_start && b == b'[' {
            // [table] or [[array of tables]]
            let end = line_end(bytes, i);
            let mut j = i;
            while j < end && bytes[j] == b'[' {
                j += 1;
            }
            let close = bytes[j..end]
                .iter()
                .position(|c| *c == b']')
                .map(|p| j + p)
                .unwrap_or(end);
            let mut k = close;
            while k < end && bytes[k] == b']' {
                k += 1;
            }
            sink.push(i, k, HighlightTag::Heading);
            in_table_header = true;
            at_line_start = false;
            i = k.max(i + 1);
            continue;
        }
        at_line_start = false;
        if !after_eq && !in_table_header {
            // Key (bare, dotted or quoted) up to `=`.
            let key_start = i;
            let mut j = i;
            while j < bytes.len()
                && bytes[j] != b'='
                && bytes[j] != b'\n'
                && bytes[j] != b'#'
                && bytes[j] != b'}'
            {
                if bytes[j] == b'"' || bytes[j] == b'\'' {
                    j = scan_quoted(bytes, j, bytes[j], bytes[j] == b'"', false);
                } else {
                    j += 1;
                }
            }
            let mut key_end = j;
            while key_end > key_start && (bytes[key_end - 1] == b' ' || bytes[key_end - 1] == b'\t')
            {
                key_end -= 1;
            }
            if j < bytes.len() && bytes[j] == b'=' {
                sink.push(key_start, key_end, HighlightTag::Key);
                sink.push(j, j + 1, HighlightTag::Operator);
                after_eq = true;
                i = j + 1;
            } else {
                i = j.max(i + 1);
            }
            continue;
        }
        // Value context.
        match b {
            b'"' if bytes.len() >= i + 3 && &bytes[i..i + 3] == b"\"\"\"" => {
                let end = scan_triple(bytes, i, b'"', true);
                sink.push(i, end, HighlightTag::String);
                i = end;
            }
            b'\'' if bytes.len() >= i + 3 && &bytes[i..i + 3] == b"'''" => {
                let end = scan_triple(bytes, i, b'\'', false);
                sink.push(i, end, HighlightTag::String);
                i = end;
            }
            b'"' => {
                let end = scan_quoted(bytes, i, b'"', true, false);
                sink.push(i, end, HighlightTag::String);
                i = end.max(i + 1);
            }
            b'\'' => {
                let end = scan_quoted(bytes, i, b'\'', false, false);
                sink.push(i, end, HighlightTag::String);
                i = end.max(i + 1);
            }
            b'[' | b']' => {
                sink.push(i, i + 1, HighlightTag::Bracket);
                i += 1;
            }
            b'{' => {
                sink.push(i, i + 1, HighlightTag::Brace);
                // Inline table: keys follow.
                after_eq = false;
                i += 1;
            }
            b'}' => {
                sink.push(i, i + 1, HighlightTag::Brace);
                i += 1;
            }
            b',' => {
                sink.push(i, i + 1, HighlightTag::Comma);
                if in_inline_table(bytes, i) {
                    after_eq = false;
                }
                i += 1;
            }
            b'0'..=b'9' | b'+' | b'-' => {
                let end = scan_toml_number(bytes, i);
                sink.push(i, end, HighlightTag::Number);
                i = end;
            }
            b if b.is_ascii_alphabetic() => {
                let mut j = i;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    j += 1;
                }
                let tag = match &src[i..j] {
                    "true" | "false" => Some(HighlightTag::Keyword),
                    "inf" | "nan" => Some(HighlightTag::Number),
                    _ => None,
                };
                if let Some(t) = tag {
                    sink.push(i, j, t);
                }
                i = j;
            }
            _ => i += 1,
        }
    }
    finish(src, sink.spans)
}

/// Whether position `i` is inside an inline table `{ ... }` on the current line.
fn in_inline_table(bytes: &[u8], i: usize) -> bool {
    let mut depth_brace = 0i32;
    let mut depth_bracket = 0i32;
    let mut j = i;
    while j > 0 && bytes[j - 1] != b'\n' {
        j -= 1;
        match bytes[j] {
            b'{' => depth_brace += 1,
            b'}' => depth_brace -= 1,
            b'[' => depth_bracket += 1,
            b']' => depth_bracket -= 1,
            _ => {}
        }
    }
    depth_brace > 0 && depth_bracket <= 0
}

/// Numbers, dates (`1979-05-27T07:32:00Z`) and times (`07:32:00`).
fn scan_toml_number(bytes: &[u8], i: usize) -> usize {
    let mut j = i;
    if matches!(bytes[j], b'+' | b'-') {
        j += 1;
    }
    if j < bytes.len() && bytes[j].is_ascii_alphabetic() {
        // +inf / -nan
        while j < bytes.len() && bytes[j].is_ascii_alphabetic() {
            j += 1;
        }
        return j;
    }
    if j >= bytes.len() || !bytes[j].is_ascii_digit() {
        return i + 1;
    }
    let end = scan_number(bytes, j);
    let mut k = end;
    // Date / time continuation characters.
    while k < bytes.len()
        && matches!(
            bytes[k],
            b'-' | b':' | b'T' | b't' | b'Z' | b'z' | b'.' | b'+' | b'0'..=b'9'
        )
    {
        k += 1;
    }
    if k > end && bytes[i..k].iter().any(|c| matches!(c, b'-' | b':')) {
        // Allow a space between date and time (`1979-05-27 07:32:00`).
        if k + 1 < bytes.len()
            && bytes[k] == b' '
            && bytes[k + 1].is_ascii_digit()
            && bytes[i..k].iter().filter(|c| **c == b'-').count() == 2
            && !bytes[i..k].contains(&b':')
        {
            let mut m = k + 1;
            while m < bytes.len()
                && matches!(bytes[m], b':' | b'.' | b'0'..=b'9' | b'Z' | b'+' | b'-')
            {
                m += 1;
            }
            return m;
        }
        k
    } else {
        end.max(i + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::testing::*;

    const SAMPLE: &str = "# config\ntitle = \"TOML\" # c\n[owner]\nname = 'Tom'\ndob = 1979-05-27T07:32:00Z\n[[products]]\nids = [1, 2_000, 0x1F, -3.5e2]\nok = true\ndesc = \"\"\"multi\nline\"\"\"\ninline = { a = 1, b.c = \"x\" }\n";

    #[test]
    fn tags() {
        let t = tagged(SAMPLE, &highlight_toml(SAMPLE));
        assert!(has(&t, "# config", HighlightTag::Comment));
        assert!(has(&t, "title", HighlightTag::Key));
        assert!(has(&t, "\"TOML\"", HighlightTag::String));
        assert!(has(&t, "[owner]", HighlightTag::Heading));
        assert!(has(&t, "[[products]]", HighlightTag::Heading));
        assert!(has(&t, "1979-05-27T07:32:00Z", HighlightTag::Number));
        assert!(has(&t, "2_000", HighlightTag::Number));
        assert!(has(&t, "0x1F", HighlightTag::Number));
        assert!(has(&t, "-3.5e2", HighlightTag::Number));
        assert!(has(&t, "true", HighlightTag::Keyword));
        assert!(has(&t, "\"\"\"multi\nline\"\"\"", HighlightTag::String));
        assert!(has(&t, "a", HighlightTag::Key));
        assert!(has(&t, "b.c", HighlightTag::Key));
        assert!(has(&t, "\"x\"", HighlightTag::String));
    }

    #[test]
    fn invariants_and_truncation() {
        check_all(highlight_toml, SAMPLE);
        check_all(
            highlight_toml,
            "x = \"\"\"never closed\nmore\n[t]\ny = 'a\n",
        );
        check_all(
            highlight_toml,
            "имя = \"значение\"\n[раздел]\nd = 2020-01-01 12:00:00\n",
        );
    }
}
