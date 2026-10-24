//! JSON / JSONC highlighter: keys, strings, numbers, literals, comments.

use crate::common::{
    bracket_tag, comment_tag, find_from, finish, line_end, scan_number, scan_quoted, skip_blank,
    Sink,
};
use crate::{HighlightTag, Span};

pub fn highlight_json(src: &str) -> Vec<Span> {
    let bytes = src.as_bytes();
    let mut sink = Sink::default();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'"' => {
                let end = scan_quoted(bytes, i, b'"', true, false);
                // A string followed by `:` is an object key.
                let mut j = end;
                while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b'\r' | b'\n') {
                    j += 1;
                }
                let tag = if j < bytes.len() && bytes[j] == b':' {
                    HighlightTag::Key
                } else {
                    HighlightTag::String
                };
                sink.push(i, end, tag);
                i = end.max(i + 1);
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                let end = line_end(bytes, i);
                sink.push(i, end, comment_tag(&src[i..end]));
                i = end;
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                let end = find_from(bytes, i + 2, b"*/")
                    .map(|p| p + 2)
                    .unwrap_or(bytes.len());
                sink.push(i, end, comment_tag(&src[i..end]));
                i = end;
            }
            b'0'..=b'9' => {
                let end = scan_number(bytes, i);
                sink.push(i, end, HighlightTag::Number);
                i = end;
            }
            b'-' | b'+' if i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() => {
                let end = scan_number(bytes, i + 1);
                sink.push(i, end, HighlightTag::Number);
                i = end;
            }
            b'{' | b'}' | b'[' | b']' => {
                sink.push(
                    i,
                    i + 1,
                    bracket_tag(b).unwrap_or(HighlightTag::Punctuation),
                );
                i += 1;
            }
            b',' => {
                sink.push(i, i + 1, HighlightTag::Comma);
                i += 1;
            }
            b':' => {
                sink.push(i, i + 1, HighlightTag::Punctuation);
                i += 1;
            }
            b if b.is_ascii_alphabetic() => {
                let mut j = i;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    j += 1;
                }
                let word = &src[i..j];
                let tag = match word {
                    "true" | "false" | "null" => HighlightTag::Keyword,
                    "NaN" | "Infinity" => HighlightTag::Number,
                    _ => HighlightTag::Error,
                };
                // Unquoted words are invalid JSON unless they are literals; JSON5-style keys are tolerated.
                let k = skip_blank(bytes, j);
                let tag = if tag == HighlightTag::Error && k < bytes.len() && bytes[k] == b':' {
                    HighlightTag::Key
                } else {
                    tag
                };
                sink.push(i, j, tag);
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

    const SAMPLE: &str = "{\n  \"name\": \"rustpy\", // comment\n  \"version\": 1.5e3,\n  \"ok\": true, \"none\": null,\n  \"list\": [1, -2, \"x\\\"y\"],\n  /* block\n  comment */ \"nested\": {\"k\": [] }\n}\n";

    #[test]
    fn tags() {
        let t = tagged(SAMPLE, &highlight_json(SAMPLE));
        assert!(has(&t, "\"name\"", HighlightTag::Key));
        assert!(has(&t, "\"rustpy\"", HighlightTag::String));
        assert!(has(&t, "1.5e3", HighlightTag::Number));
        assert!(has(&t, "-2", HighlightTag::Number));
        assert!(has(&t, "true", HighlightTag::Keyword));
        assert!(has(&t, "null", HighlightTag::Keyword));
        assert!(has(&t, "// comment", HighlightTag::Comment));
        assert!(has(&t, "/* block\n  comment */", HighlightTag::Comment));
        assert!(has(&t, "\"x\\\"y\"", HighlightTag::String));
        assert!(has(&t, "{", HighlightTag::Brace));
        assert!(has(&t, "[", HighlightTag::Bracket));
    }

    #[test]
    fn invariants_and_truncation() {
        check_all(highlight_json, SAMPLE);
        check_all(
            highlight_json,
            "{\"a\": \"unterminated\n, \"b\": [1, 2 /* open",
        );
        check_all(
            highlight_json,
            "{\"ключ\": \"значение\", \"n\": 42, bad: x}",
        );
    }
}
