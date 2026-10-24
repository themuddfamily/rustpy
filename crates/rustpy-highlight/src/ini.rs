//! INI / `.cfg` / `.editorconfig` highlighter: sections, keys, comments.

use crate::common::{comment_tag, finish, lines, skip_blank, Sink};
use crate::{HighlightTag, Span};

pub fn highlight_ini(src: &str) -> Vec<Span> {
    let bytes = src.as_bytes();
    let mut sink = Sink::default();
    for (start, end) in lines(src) {
        let i = skip_blank(bytes, start);
        if i >= end {
            continue;
        }
        let b = bytes[i];
        if b == b';' || b == b'#' {
            sink.push(i, end, comment_tag(&src[i..end]));
            continue;
        }
        if b == b'[' {
            let close = bytes[i..end]
                .iter()
                .position(|c| *c == b']')
                .map(|p| i + p + 1)
                .unwrap_or(end);
            sink.push(i, close, HighlightTag::Heading);
            if close < end {
                let c = skip_blank(bytes, close);
                if c < end && (bytes[c] == b';' || bytes[c] == b'#') {
                    sink.push(c, end, comment_tag(&src[c..end]));
                }
            }
            continue;
        }
        if i > start {
            // Indented continuation of a multi-line value.
            continue;
        }
        // key = value / key : value
        let sep = bytes[i..end]
            .iter()
            .position(|c| *c == b'=' || *c == b':')
            .map(|p| i + p);
        match sep {
            Some(s) => {
                let mut key_end = s;
                while key_end > i && (bytes[key_end - 1] == b' ' || bytes[key_end - 1] == b'\t') {
                    key_end -= 1;
                }
                sink.push(i, key_end, HighlightTag::Key);
                sink.push(s, s + 1, HighlightTag::Operator);
                let v = skip_blank(bytes, s + 1);
                if v < end {
                    let val = src[v..end].trim_end();
                    let ve = v + val.len();
                    let lower = val.to_ascii_lowercase();
                    if matches!(
                        lower.as_str(),
                        "true" | "false" | "yes" | "no" | "on" | "off" | "none" | "null"
                    ) {
                        sink.push(v, ve, HighlightTag::Keyword);
                    } else if val
                        .chars()
                        .all(|c| c.is_ascii_digit() || c == '.' || c == '-')
                        && val.chars().any(|c| c.is_ascii_digit())
                    {
                        sink.push(v, ve, HighlightTag::Number);
                    } else if (val.starts_with('"') && val.ends_with('"') && val.len() > 1)
                        || (val.starts_with('\'') && val.ends_with('\'') && val.len() > 1)
                    {
                        sink.push(v, ve, HighlightTag::String);
                    } else if let Some(p) = val.find(" ;").or_else(|| val.find(" #")) {
                        sink.push(v + p + 1, ve, comment_tag(&val[p + 1..]));
                    }
                }
            }
            None => {
                // A bare key without a value (allowed by many parsers).
                sink.push(i, end, HighlightTag::Key);
            }
        }
    }
    finish(src, sink.spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::testing::*;

    const SAMPLE: &str = "; comment\n[section]  # trailing\nkey = value\nnum: 42\nflag = true\nquoted = \"hi\"\nmulti = first\n    second\n# TODO fix\nbare\n";

    #[test]
    fn tags() {
        let t = tagged(SAMPLE, &highlight_ini(SAMPLE));
        assert!(has(&t, "; comment", HighlightTag::Comment));
        assert!(has(&t, "[section]", HighlightTag::Heading));
        assert!(has(&t, "# trailing", HighlightTag::Comment));
        assert!(has(&t, "key", HighlightTag::Key));
        assert!(has(&t, "=", HighlightTag::Operator));
        assert!(has(&t, "42", HighlightTag::Number));
        assert!(has(&t, "true", HighlightTag::Keyword));
        assert!(has(&t, "\"hi\"", HighlightTag::String));
        assert!(has(&t, "# TODO fix", HighlightTag::TodoComment));
        assert!(has(&t, "bare", HighlightTag::Key));
        assert!(!has(&t, "second", HighlightTag::Key));
    }

    #[test]
    fn invariants_and_truncation() {
        check_all(highlight_ini, SAMPLE);
        check_all(highlight_ini, "[unclosed\nk=\n=v\n");
        check_all(highlight_ini, "[раздел]\nключ = значение\n");
    }
}
