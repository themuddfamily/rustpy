//! YAML highlighter: keys, scalars, anchors/aliases/tags, block scalars, comments.

use crate::common::{
    comment_tag, finish, is_ident_continue, lines, scan_number, scan_quoted, skip_blank, Sink,
};
use crate::{HighlightTag, Span};

/// Start of a `#` comment in `from..end` that is not inside quotes.
fn comment_start(bytes: &[u8], from: usize, end: usize) -> Option<usize> {
    let mut i = from;
    let mut in_single = false;
    let mut in_double = false;
    while i < end {
        let b = bytes[i];
        match b {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => {
                if i == 0 || bytes[i - 1] != b'\\' {
                    in_double = !in_double;
                }
            }
            b'#' if !in_single
                && !in_double
                && (i == from || bytes[i - 1] == b' ' || bytes[i - 1] == b'\t') =>
            {
                return Some(i)
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Highlights a scalar value or flow collection in `start..end`.
fn value(sink: &mut Sink, src: &str, start: usize, end: usize) {
    let bytes = src.as_bytes();
    let mut i = skip_blank(bytes, start);
    while i < end {
        let b = bytes[i];
        match b {
            b' ' | b'\t' => i += 1,
            b'"' => {
                let e = scan_quoted(bytes, i, b'"', true, false).min(end);
                sink.push(i, e, HighlightTag::String);
                i = e.max(i + 1);
            }
            b'\'' => {
                let e = scan_quoted(bytes, i, b'\'', false, false).min(end);
                sink.push(i, e, HighlightTag::String);
                i = e.max(i + 1);
            }
            b'&' | b'*' if i + 1 < end && is_ident_continue(bytes[i + 1]) => {
                let mut j = i + 1;
                while j < end && (is_ident_continue(bytes[j]) || bytes[j] == b'-') {
                    j += 1;
                }
                sink.push(i, j, HighlightTag::Variable);
                i = j;
            }
            b'!' => {
                let mut j = i + 1;
                while j < end && !matches!(bytes[j], b' ' | b'\t' | b',' | b']' | b'}') {
                    j += 1;
                }
                sink.push(i, j, HighlightTag::Decorator);
                i = j;
            }
            b'[' | b']' => {
                sink.push(i, i + 1, HighlightTag::Bracket);
                i += 1;
            }
            b'{' | b'}' => {
                sink.push(i, i + 1, HighlightTag::Brace);
                i += 1;
            }
            b',' => {
                sink.push(i, i + 1, HighlightTag::Comma);
                i += 1;
            }
            b'|' | b'>'
                if i + 1 >= end
                    || matches!(bytes[i + 1], b'-' | b'+' | b'0'..=b'9' | b' ' | b'\t') =>
            {
                let mut j = i + 1;
                while j < end && matches!(bytes[j], b'-' | b'+' | b'0'..=b'9') {
                    j += 1;
                }
                sink.push(i, j, HighlightTag::Keyword);
                i = j;
            }
            _ => {
                // Plain scalar word: a flow key (`word:`), number or literal keyword.
                let mut j = i;
                while j < end
                    && !matches!(bytes[j], b' ' | b'\t' | b',' | b'[' | b']' | b'{' | b'}')
                {
                    j += 1;
                }
                let word = &src[i..j];
                if word.len() > 1 && word.ends_with(':') {
                    sink.push(i, j - 1, HighlightTag::Key);
                    sink.push(j - 1, j, HighlightTag::Punctuation);
                } else if let Some(tag) = scalar_tag(word) {
                    sink.push(i, j, tag);
                }
                i = j.max(i + 1);
            }
        }
    }
}

fn scalar_tag(word: &str) -> Option<HighlightTag> {
    match word {
        "true" | "false" | "True" | "False" | "TRUE" | "FALSE" | "yes" | "no" | "Yes" | "No"
        | "on" | "off" | "null" | "Null" | "NULL" | "~" => Some(HighlightTag::Keyword),
        _ => {
            let b = word.as_bytes();
            let first_digit = b.first().is_some_and(|c| c.is_ascii_digit());
            let signed = b.len() > 1 && matches!(b[0], b'-' | b'+' | b'.') && b[1].is_ascii_digit();
            if !(first_digit || signed) {
                return None;
            }
            let digits_from = if first_digit { 0 } else { 1 };
            if scan_number(b, digits_from) == b.len()
                || word
                    .chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, '-' | ':' | 'T' | 'Z' | '.' | '+'))
            {
                Some(HighlightTag::Number) // numbers, dates and times
            } else {
                None
            }
        }
    }
}

pub fn highlight_yaml(src: &str) -> Vec<Span> {
    let bytes = src.as_bytes();
    let mut sink = Sink::default();
    // Block scalar state: indentation of the owning key; body lines must be indented more.
    let mut block_indent: Option<usize> = None;
    for (start, end) in lines(src) {
        let content_start = skip_blank(bytes, start);
        let indent = content_start - start;
        if content_start >= end {
            continue; // blank line (keeps a block scalar going)
        }
        if let Some(bi) = block_indent {
            if indent > bi {
                sink.push(content_start, end, HighlightTag::String);
                continue;
            }
            block_indent = None;
        }
        let line = &src[content_start..end];
        if line.starts_with("---") || line.starts_with("...") {
            sink.push(content_start, content_start + 3, HighlightTag::Keyword);
            let code_end = comment_start(bytes, content_start + 3, end).unwrap_or(end);
            value(&mut sink, src, content_start + 3, code_end);
            if code_end < end {
                sink.push(code_end, end, comment_tag(&src[code_end..end]));
            }
            continue;
        }
        if line.starts_with('%') {
            sink.push(content_start, end, HighlightTag::Decorator);
            continue;
        }
        let code_end = comment_start(bytes, content_start, end).unwrap_or(end);
        if code_end < end {
            sink.push(code_end, end, comment_tag(&src[code_end..end]));
        }
        let mut i = content_start;
        // List item markers (possibly several: `- - x`).
        let mut key_indent = indent;
        while i + 1 < code_end
            && bytes[i] == b'-'
            && (bytes[i + 1] == b' ' || bytes[i + 1] == b'\t')
        {
            sink.push(i, i + 1, HighlightTag::Punctuation);
            i = skip_blank(bytes, i + 1);
            key_indent = i - start;
        }
        if i < code_end && bytes[i] == b'-' && i + 1 == code_end {
            sink.push(i, i + 1, HighlightTag::Punctuation);
            continue;
        }
        // Explicit key / complex mapping markers.
        if i + 1 < code_end && (bytes[i] == b'?' || bytes[i] == b':') && bytes[i + 1] == b' ' {
            sink.push(i, i + 1, HighlightTag::Punctuation);
            i = skip_blank(bytes, i + 1);
        }
        let key_end = find_key_end(bytes, i, code_end);
        let value_start = match key_end {
            Some(k) => {
                let key_text_end = trim_end(bytes, i, k);
                sink.push(i, key_text_end, HighlightTag::Key);
                sink.push(k, k + 1, HighlightTag::Punctuation);
                k + 1
            }
            None => i,
        };
        let value_start = value_start.min(code_end);
        let trimmed = src[value_start..code_end].trim();
        let is_block_scalar = (trimmed.starts_with('|') || trimmed.starts_with('>'))
            && trimmed
                .trim_start_matches(['|', '>', '-', '+'])
                .chars()
                .all(|c| c.is_ascii_digit());
        if key_end.is_some() && is_block_scalar {
            block_indent = Some(key_indent);
        }
        value(&mut sink, src, value_start, code_end);
    }
    finish(src, sink.spans)
}

fn trim_end(bytes: &[u8], start: usize, mut end: usize) -> usize {
    while end > start && (bytes[end - 1] == b' ' || bytes[end - 1] == b'\t') {
        end -= 1;
    }
    end
}

/// Position of the mapping colon in `start..end`, if the line is a `key: value` entry.
fn find_key_end(bytes: &[u8], start: usize, end: usize) -> Option<usize> {
    let mut i = start;
    if i >= end {
        return None;
    }
    match bytes[i] {
        b'[' | b'{' | b'|' | b'>' | b'&' | b'*' | b'!' | b'#' => return None,
        b'"' | b'\'' => {
            let q = bytes[i];
            i = scan_quoted(bytes, i, q, q == b'"', false).min(end);
        }
        _ => {}
    }
    while i < end {
        if bytes[i] == b':' && (i + 1 >= end || bytes[i + 1] == b' ' || bytes[i + 1] == b'\t') {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::testing::*;

    const SAMPLE: &str = "---\n# top comment\nname: rustpy  # trailing\nversion: 1.2\nenabled: true\ntags: [a, 'b', \"c\"]\nmeta: &anchor\n  ref: *anchor\n  kind: !!str 42\nscript: |\n  echo hi\n  echo there\nnext: value\nlist:\n  - one\n  - key: two\n";

    #[test]
    fn tags() {
        let t = tagged(SAMPLE, &highlight_yaml(SAMPLE));
        assert!(has(&t, "---", HighlightTag::Keyword));
        assert!(has(&t, "# top comment", HighlightTag::Comment));
        assert!(has(&t, "# trailing", HighlightTag::Comment));
        assert!(has(&t, "name", HighlightTag::Key));
        assert!(has(&t, "version", HighlightTag::Key));
        assert!(has(&t, "1.2", HighlightTag::Number));
        assert!(has(&t, "true", HighlightTag::Keyword));
        assert!(has(&t, "'b'", HighlightTag::String));
        assert!(has(&t, "&anchor", HighlightTag::Variable));
        assert!(has(&t, "*anchor", HighlightTag::Variable));
        assert!(has(&t, "!!str", HighlightTag::Decorator));
        assert!(has(&t, "|", HighlightTag::Keyword));
        assert!(has(&t, "echo hi", HighlightTag::String));
        assert!(has(&t, "echo there", HighlightTag::String));
        assert!(has(&t, "next", HighlightTag::Key));
        assert!(has(&t, "key", HighlightTag::Key));
        assert!(!has(&t, "one", HighlightTag::Key));
    }

    #[test]
    fn invariants_and_truncation() {
        check_all(highlight_yaml, SAMPLE);
        check_all(highlight_yaml, "a: \"open string\nb: [1, {c: d}\n  - x\n");
        check_all(highlight_yaml, "ключ: значение\n  - вещь: 1\n");
    }
}
