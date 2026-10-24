//! CSS / SCSS highlighter: selectors, properties, values, at-rules, comments.

use crate::common::{
    comment_tag, find_from, finish, is_ident_continue, is_ident_start, line_end, scan_number,
    scan_quoted, Sink,
};
use crate::{HighlightTag, Span};

/// Whether the statement starting at `i` (inside a block) is a nested rule
/// (reaches `{` before `;`/`}`) rather than a declaration.
fn statement_is_rule(bytes: &[u8], mut i: usize, end: usize) -> bool {
    while i < end {
        match bytes[i] {
            b'{' => return true,
            b';' | b'}' => return false,
            b'"' | b'\'' => i = scan_quoted(bytes, i, bytes[i], true, false),
            b'/' if i + 1 < end && bytes[i + 1] == b'*' => {
                i = find_from(bytes, i + 2, b"*/").map(|p| p + 2).unwrap_or(end)
            }
            _ => i += 1,
        }
    }
    false
}

fn scan_css(src: &str, start: usize, end: usize, sink: &mut Sink) {
    let bytes = src.as_bytes();
    let mut i = start;
    let mut depth = 0usize;
    // Whether the current statement is a selector / at-rule prelude.
    let mut selector_mode = true;
    let mut statement_start = true;
    let mut after_colon = false;
    while i < end {
        let b = bytes[i];
        if statement_start && !matches!(b, b' ' | b'\t' | b'\r' | b'\n') {
            statement_start = false;
            after_colon = false;
            selector_mode =
                depth == 0 || b == b'@' || b == b'&' || statement_is_rule(bytes, i, end);
        }
        match b {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'/' if i + 1 < end && bytes[i + 1] == b'*' => {
                let e = find_from(&bytes[..end], i + 2, b"*/")
                    .map(|p| p + 2)
                    .unwrap_or(end);
                sink.push(i, e, comment_tag(&src[i..e]));
                i = e;
            }
            b'/' if i + 1 < end && bytes[i + 1] == b'/' => {
                // SCSS line comment.
                let e = line_end(bytes, i).min(end);
                sink.push(i, e, comment_tag(&src[i..e]));
                i = e;
            }
            b'{' => {
                sink.push(i, i + 1, HighlightTag::Brace);
                depth += 1;
                statement_start = true;
                i += 1;
            }
            b'}' => {
                sink.push(i, i + 1, HighlightTag::Brace);
                depth = depth.saturating_sub(1);
                statement_start = true;
                i += 1;
            }
            b';' => {
                sink.push(i, i + 1, HighlightTag::Punctuation);
                statement_start = true;
                i += 1;
            }
            b'"' | b'\'' => {
                let e = scan_quoted(bytes, i, b, true, false).min(end);
                sink.push(i, e, HighlightTag::String);
                i = e.max(i + 1);
            }
            b'@' => {
                let e = scan_ident(bytes, i + 1, end);
                sink.push(i, e, HighlightTag::Keyword);
                i = e.max(i + 1);
            }
            b'$' if i + 1 < end && is_ident_start(bytes[i + 1]) => {
                let e = scan_ident(bytes, i + 1, end);
                sink.push(i, e, HighlightTag::Variable);
                i = e;
            }
            b'-' if i + 2 < end && bytes[i + 1] == b'-' && is_ident_start(bytes[i + 2]) => {
                let e = scan_ident(bytes, i + 2, end);
                sink.push(i, e, HighlightTag::Variable);
                i = e;
            }
            b'#' => {
                let e = scan_ident(bytes, i + 1, end);
                if selector_mode {
                    sink.push(i, e, HighlightTag::Decorator);
                } else {
                    sink.push(i, e, HighlightTag::Number); // color
                }
                i = e.max(i + 1);
            }
            b'.' if selector_mode && i + 1 < end && is_ident_start(bytes[i + 1]) => {
                let e = scan_ident(bytes, i + 1, end);
                sink.push(i, e, HighlightTag::ClassDefinition);
                i = e;
            }
            b':' if selector_mode => {
                let mut j = i + 1;
                if j < end && bytes[j] == b':' {
                    j += 1;
                }
                let e = scan_ident(bytes, j, end);
                sink.push(i, e.max(i + 1), HighlightTag::Keyword);
                i = e.max(i + 1);
            }
            b':' => {
                sink.push(i, i + 1, HighlightTag::Punctuation);
                after_colon = true;
                i += 1;
            }
            b'!' => {
                let e = scan_ident(bytes, i + 1, end);
                sink.push(i, e, HighlightTag::Keyword);
                i = e.max(i + 1);
            }
            b'[' if selector_mode => {
                let close = bytes[i..end]
                    .iter()
                    .position(|c| *c == b']')
                    .map(|p| i + p + 1)
                    .unwrap_or(end);
                sink.push(i, i + 1, HighlightTag::Bracket);
                let name_end = scan_ident(bytes, i + 1, close);
                sink.push(i + 1, name_end, HighlightTag::Parameter);
                let mut k = name_end;
                while k < close {
                    if bytes[k] == b'"' || bytes[k] == b'\'' {
                        let e = scan_quoted(bytes, k, bytes[k], true, false).min(close);
                        sink.push(k, e, HighlightTag::String);
                        k = e.max(k + 1);
                    } else {
                        k += 1;
                    }
                }
                if close > i + 1 && bytes[close - 1] == b']' {
                    sink.push(close - 1, close, HighlightTag::Bracket);
                }
                i = close;
            }
            b'(' | b')' => {
                sink.push(i, i + 1, HighlightTag::Parenthesis);
                i += 1;
            }
            b',' => {
                sink.push(i, i + 1, HighlightTag::Comma);
                i += 1;
            }
            b'0'..=b'9' => {
                let e = scan_number(bytes, i).min(end);
                sink.push(i, e, HighlightTag::Number);
                i = e;
            }
            b'.' if i + 1 < end && bytes[i + 1].is_ascii_digit() => {
                let e = scan_number(bytes, i + 1).min(end);
                sink.push(i, e, HighlightTag::Number);
                i = e;
            }
            b if is_ident_start(b) || b == b'-' => {
                let e = scan_ident(bytes, i + 1, end);
                let e = e.max(i + 1);
                let word = &src[i..e];
                let mut k = e;
                while k < end && (bytes[k] == b' ' || bytes[k] == b'\t') {
                    k += 1;
                }
                let next = if k < end { Some(bytes[k]) } else { None };
                let tag = if selector_mode {
                    if next == Some(b'(') {
                        HighlightTag::FunctionCall
                    } else if depth > 0
                        && !after_colon
                        && next == Some(b':')
                        && !statement_is_rule(bytes, i, end)
                    {
                        HighlightTag::Key
                    } else {
                        HighlightTag::Tag
                    }
                } else if !after_colon && next == Some(b':') {
                    HighlightTag::Key
                } else if next == Some(b'(') {
                    HighlightTag::FunctionCall
                } else if matches!(
                    word,
                    "inherit" | "initial" | "unset" | "none" | "auto" | "important"
                ) {
                    HighlightTag::Keyword
                } else {
                    HighlightTag::Default
                };
                if tag != HighlightTag::Default && !(word == "-" || word.is_empty()) {
                    sink.push(i, e, tag);
                }
                i = e;
            }
            b'>' | b'+' | b'~' | b'*' | b'&' | b'=' | b'%' => {
                sink.push(i, i + 1, HighlightTag::Operator);
                i += 1;
            }
            _ => i += 1,
        }
    }
}

/// Identifier end allowing dashes (CSS names).
fn scan_ident(bytes: &[u8], mut i: usize, end: usize) -> usize {
    while i < end && (is_ident_continue(bytes[i]) || bytes[i] == b'-') {
        i += 1;
    }
    i
}

pub fn highlight_css(src: &str) -> Vec<Span> {
    let mut sink = Sink::default();
    scan_css(src, 0, src.len(), &mut sink);
    finish(src, sink.spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::testing::*;

    const SAMPLE: &str = "/* reset */\n@import url(\"base.css\");\n:root { --main: #333; }\nbody, h1.title > a:hover::before {\n  color: var(--main) !important;\n  margin: 1.5em 0 10px;\n  background: rgb(255, 0, 0) url('x.png');\n  font-family: \"Fira\", sans-serif;\n}\n@media (min-width: 600px) {\n  .card[data-x=\"1\"] { width: 50%; }\n}\n#main { display: none; }\n";

    #[test]
    fn tags() {
        let t = tagged(SAMPLE, &highlight_css(SAMPLE));
        assert!(has(&t, "/* reset */", HighlightTag::Comment));
        assert!(has(&t, "@import", HighlightTag::Keyword));
        assert!(has(&t, "\"base.css\"", HighlightTag::String));
        assert!(has(&t, "--main", HighlightTag::Variable));
        assert!(has(&t, "#333", HighlightTag::Number));
        assert!(has(&t, "body", HighlightTag::Tag));
        assert!(has(&t, "h1", HighlightTag::Tag));
        assert!(has(&t, ".title", HighlightTag::ClassDefinition));
        assert!(has(&t, ":hover", HighlightTag::Keyword));
        assert!(has(&t, "::before", HighlightTag::Keyword));
        assert!(has(&t, "color", HighlightTag::Key));
        assert!(has(&t, "var", HighlightTag::FunctionCall));
        assert!(has(&t, "!important", HighlightTag::Keyword));
        assert!(has(&t, "1.5em", HighlightTag::Number));
        assert!(has(&t, "10px", HighlightTag::Number));
        assert!(has(&t, "rgb", HighlightTag::FunctionCall));
        assert!(has(&t, "@media", HighlightTag::Keyword));
        assert!(has(&t, ".card", HighlightTag::ClassDefinition));
        assert!(has(&t, "data-x", HighlightTag::Parameter));
        assert!(has(&t, "50%", HighlightTag::Number));
        assert!(has(&t, "#main", HighlightTag::Decorator));
        assert!(has(&t, "none", HighlightTag::Keyword));
    }

    #[test]
    fn invariants_and_truncation() {
        check_all(highlight_css, SAMPLE);
        check_all(highlight_css, "a { color: 'open\n b { /* open");
        check_all(highlight_css, ".класс { содержимое: \"текст\"; }\n");
    }
}
