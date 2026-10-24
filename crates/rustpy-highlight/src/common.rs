//! Shared byte-level scanning helpers for the token-based highlighters.
//!
//! All scanners work on bytes but only ever start or end spans at ASCII
//! delimiters (or at the end of a run that includes multi-byte characters), so
//! span boundaries land on char boundaries. `finish` enforces the invariants
//! the editor relies on anyway: sorted, non-overlapping, inside the text and
//! on char boundaries.

use rustpy_core::{TextRange, TextSize};

use crate::{HighlightTag, Span};

/// Builds a span for `start..end`, or `None` when the range is empty.
pub(crate) fn span(start: usize, end: usize, tag: HighlightTag) -> Option<Span> {
    if start < end {
        Some(Span {
            range: TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32)),
            tag,
        })
    } else {
        None
    }
}

/// Collects spans; `push` ignores empty ranges.
#[derive(Default)]
pub(crate) struct Sink {
    pub spans: Vec<Span>,
}

impl Sink {
    pub(crate) fn push(&mut self, start: usize, end: usize, tag: HighlightTag) {
        if let Some(s) = span(start, end, tag) {
            self.spans.push(s);
        }
    }

    /// Appends spans produced for a sub-slice starting at byte `base`.
    pub(crate) fn push_offset(&mut self, spans: Vec<Span>, base: usize) {
        let off = TextSize::new(base as u32);
        for s in spans {
            self.spans.push(Span {
                range: TextRange::new(s.range.start() + off, s.range.end() + off),
                tag: s.tag,
            });
        }
    }
}

/// Sorts the spans, clips them to the text and drops empty, overlapping and
/// mis-aligned ones (earlier span wins on overlap).
pub(crate) fn finish(src: &str, mut spans: Vec<Span>) -> Vec<Span> {
    let len = src.len();
    spans.retain(|s| {
        let (a, b) = (usize::from(s.range.start()), usize::from(s.range.end()));
        a < b && b <= len && src.is_char_boundary(a) && src.is_char_boundary(b)
    });
    spans.sort_by_key(|s| (s.range.start(), s.range.end()));
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    let mut last_end = TextSize::new(0);
    for s in spans {
        if s.range.start() >= last_end {
            last_end = s.range.end();
            out.push(s);
        }
    }
    out
}

pub(crate) fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

pub(crate) fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// End of the identifier starting at `i` (`i` itself is not checked).
pub(crate) fn scan_ident(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && is_ident_continue(bytes[i]) {
        i += 1;
    }
    i
}

/// End of a number literal starting at `i` (decimal/hex/binary/octal, `_`
/// separators, fraction, exponent, and a trailing alphabetic suffix such as
/// `n`, `L`, `px` or `%`).
pub(crate) fn scan_number(bytes: &[u8], mut i: usize) -> usize {
    let start = i;
    if i + 1 < bytes.len()
        && bytes[i] == b'0'
        && matches!(bytes[i + 1], b'x' | b'X' | b'b' | b'B' | b'o' | b'O')
    {
        i += 2;
        while i < bytes.len() && (bytes[i].is_ascii_hexdigit() || bytes[i] == b'_') {
            i += 1;
        }
    } else {
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
            i += 1;
        }
        if i < bytes.len()
            && bytes[i] == b'.'
            && i + 1 < bytes.len()
            && bytes[i + 1].is_ascii_digit()
        {
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'_') {
                i += 1;
            }
        }
        if i < bytes.len() && matches!(bytes[i], b'e' | b'E') {
            let mut j = i + 1;
            if j < bytes.len() && matches!(bytes[j], b'+' | b'-') {
                j += 1;
            }
            if j < bytes.len() && bytes[j].is_ascii_digit() {
                i = j;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
            }
        }
    }
    // Suffix (units, bigint `n`, `%`).
    while i < bytes.len() && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'%') {
        i += 1;
    }
    i.max(start + 1)
}

/// End of the quoted string starting at `i` (which holds the opening quote).
/// Returns the index just past the closing quote, or the end of the line /
/// text when unterminated (`multiline` lets the string span lines).
pub(crate) fn scan_quoted(
    bytes: &[u8],
    i: usize,
    quote: u8,
    escapes: bool,
    multiline: bool,
) -> usize {
    let mut j = i + 1;
    while j < bytes.len() {
        let b = bytes[j];
        if escapes && b == b'\\' {
            j += 2;
            continue;
        }
        if b == quote {
            return j + 1;
        }
        if b == b'\n' && !multiline {
            return j;
        }
        j += 1;
    }
    bytes.len()
}

/// End of the triple-quoted string starting at `i` (`i` is the first quote).
pub(crate) fn scan_triple(bytes: &[u8], i: usize, quote: u8, escapes: bool) -> usize {
    let mut j = i + 3;
    while j < bytes.len() {
        if escapes && bytes[j] == b'\\' {
            j += 2;
            continue;
        }
        if bytes[j] == quote
            && j + 2 < bytes.len()
            && bytes[j + 1] == quote
            && bytes[j + 2] == quote
        {
            return j + 3;
        }
        j += 1;
    }
    bytes.len()
}

/// Index of the newline ending the line that contains `i`, or the text length.
pub(crate) fn line_end(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    i
}

/// Skips spaces and tabs.
pub(crate) fn skip_blank(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    i
}

/// `(start, end)` byte ranges of every line, excluding the newline.
pub(crate) fn lines(src: &str) -> Vec<(usize, usize)> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    loop {
        let end = line_end(bytes, start);
        out.push((start, end));
        if end >= bytes.len() {
            break;
        }
        start = end + 1;
    }
    out
}

/// True when the comment text contains a TODO-like marker as a word.
pub(crate) fn is_todo_comment(text: &str) -> bool {
    let upper = text.to_ascii_uppercase();
    for marker in ["TODO", "FIXME", "XXX", "HACK", "BUG"] {
        let mut from = 0;
        while let Some(pos) = upper[from..].find(marker) {
            let p = from + pos;
            let before_ok = p == 0 || !upper.as_bytes()[p - 1].is_ascii_alphanumeric();
            let after = p + marker.len();
            let after_ok = after >= upper.len() || !upper.as_bytes()[after].is_ascii_alphanumeric();
            if before_ok && after_ok {
                return true;
            }
            from = after;
        }
    }
    false
}

/// Comment tag honoring TODO markers.
pub(crate) fn comment_tag(text: &str) -> HighlightTag {
    if is_todo_comment(text) {
        HighlightTag::TodoComment
    } else {
        HighlightTag::Comment
    }
}

/// Finds `needle` in `bytes` starting at `from`.
pub(crate) fn find_from(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || from >= bytes.len() {
        return None;
    }
    bytes[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| from + p)
}

/// Tag for a bracket character.
pub(crate) fn bracket_tag(b: u8) -> Option<HighlightTag> {
    match b {
        b'(' | b')' => Some(HighlightTag::Parenthesis),
        b'[' | b']' => Some(HighlightTag::Bracket),
        b'{' | b'}' => Some(HighlightTag::Brace),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// Asserts the span invariants the editor relies on.
    pub(crate) fn check_invariants(src: &str, spans: &[Span]) {
        let mut last_end = 0usize;
        for s in spans {
            let (a, b) = (usize::from(s.range.start()), usize::from(s.range.end()));
            assert!(a < b, "empty span {s:?} in {src:?}");
            assert!(b <= src.len(), "span {s:?} past end of {src:?}");
            assert!(
                src.is_char_boundary(a) && src.is_char_boundary(b),
                "span {s:?} not on char boundary in {src:?}"
            );
            assert!(
                a >= last_end,
                "span {s:?} overlaps previous (ends at {last_end}) in {src:?}"
            );
            last_end = b;
        }
    }

    /// Highlights every prefix and suffix of `sample` (fuzz-ish truncation) and checks the invariants.
    pub(crate) fn check_all(f: fn(&str) -> Vec<Span>, sample: &str) {
        for i in 0..=sample.len() {
            if !sample.is_char_boundary(i) {
                continue;
            }
            let part = &sample[..i];
            check_invariants(part, &f(part));
            let part = &sample[i..];
            check_invariants(part, &f(part));
        }
    }

    /// `(text, tag)` pairs for assertions.
    pub(crate) fn tagged(src: &str, spans: &[Span]) -> Vec<(String, HighlightTag)> {
        spans
            .iter()
            .map(|s| (src[s.range].to_string(), s.tag))
            .collect()
    }

    pub(crate) fn has(t: &[(String, HighlightTag)], text: &str, tag: HighlightTag) -> bool {
        t.iter().any(|(s, k)| s == text && *k == tag)
    }
}
