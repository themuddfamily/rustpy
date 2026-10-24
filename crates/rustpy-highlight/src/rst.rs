//! reStructuredText highlighter: section titles, directives, roles, inline
//! markup, literal blocks, field lists, links, doctests.

use crate::common::{finish, lines, skip_blank, Sink};
use crate::{HighlightTag, Span};

const ADORNMENT: &str = "=-`:'\"~^_*+#<>";

fn is_adornment(line: &str) -> bool {
    let t = line.trim_end();
    let mut chars = t.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    t.len() >= 3 && ADORNMENT.contains(first) && chars.all(|c| c == first)
}

fn is_table_line(line: &str) -> bool {
    let t = line.trim();
    t.len() >= 3
        && t.chars().all(|c| matches!(c, '+' | '-' | '=' | '|' | ' '))
        && (t.starts_with('+') || t.starts_with('|') || t.starts_with('='))
}

/// Inline markup in `start..end`.
fn inline(sink: &mut Sink, src: &str, start: usize, end: usize) {
    let bytes = src.as_bytes();
    let mut i = start;
    while i < end {
        let b = bytes[i];
        match b {
            b'`' if i + 1 < end && bytes[i + 1] == b'`' => {
                // ``literal``
                match src[i + 2..end].find("``") {
                    Some(p) => {
                        let e = i + 2 + p + 2;
                        sink.push(i, e, HighlightTag::String);
                        i = e;
                    }
                    None => i += 2,
                }
            }
            b'`' => {
                // `interpreted text`_ / `link <url>`_ / :role:`text`
                match src[i + 1..end].find('`') {
                    Some(p) => {
                        let mut e = i + 1 + p + 1;
                        let is_link = e < end && bytes[e] == b'_';
                        while e < end && bytes[e] == b'_' {
                            e += 1;
                        }
                        let tag = if is_link {
                            HighlightTag::Link
                        } else {
                            HighlightTag::String
                        };
                        sink.push(i, e, tag);
                        i = e;
                    }
                    None => i += 1,
                }
            }
            b':' if i + 1 < end && bytes[i + 1].is_ascii_alphabetic() => {
                // :role:`text` or :field: (only roles here; fields handled per line)
                let mut j = i + 1;
                while j < end
                    && (bytes[j].is_ascii_alphanumeric()
                        || matches!(bytes[j], b'-' | b'_' | b'.' | b'+'))
                {
                    j += 1;
                }
                if j + 1 < end && bytes[j] == b':' && bytes[j + 1] == b'`' {
                    sink.push(i, j + 1, HighlightTag::Keyword);
                    i = j + 1;
                } else {
                    i = j.max(i + 1);
                }
            }
            b'*' => {
                let n = if i + 1 < end && bytes[i + 1] == b'*' {
                    2
                } else {
                    1
                };
                let marker = &src[i..i + n];
                let search = i + n;
                if search < end && !bytes[search].is_ascii_whitespace() {
                    if let Some(p) = src[search..end].find(marker) {
                        let e = search + p + n;
                        if !src[search..search + p].contains('\n') && search + p > search {
                            sink.push(
                                i,
                                e,
                                if n == 2 {
                                    HighlightTag::Strong
                                } else {
                                    HighlightTag::Emphasis
                                },
                            );
                            i = e;
                            continue;
                        }
                    }
                }
                i += n;
            }
            b'|' if i + 1 < end && bytes[i + 1].is_ascii_alphanumeric() => {
                match src[i + 1..end].find('|') {
                    Some(p) if !src[i + 1..i + 1 + p].contains(' ') => {
                        let e = i + 1 + p + 1;
                        sink.push(i, e, HighlightTag::Variable);
                        i = e;
                    }
                    _ => i += 1,
                }
            }
            b'[' => {
                // [1]_ / [#]_ / [*]_ / [citation]_
                match src[i..end].find("]_") {
                    Some(p) if !src[i..i + p].contains(' ') && p > 1 => {
                        let e = i + p + 2;
                        sink.push(i, e, HighlightTag::Link);
                        i = e;
                    }
                    _ => i += 1,
                }
            }
            b'h' if src[i..end].starts_with("http://") || src[i..end].starts_with("https://") => {
                let mut j = i;
                while j < end && !matches!(bytes[j], b' ' | b'\t' | b'\n' | b'>' | b'`' | b')') {
                    j += 1;
                }
                while j > i && matches!(bytes[j - 1], b'.' | b',' | b';' | b':') {
                    j -= 1;
                }
                sink.push(i, j, HighlightTag::Link);
                i = j.max(i + 1);
            }
            _ => i += 1,
        }
    }
}

pub fn highlight_rst(src: &str) -> Vec<Span> {
    let bytes = src.as_bytes();
    let mut sink = Sink::default();
    let all = lines(src);
    let mut literal_indent: Option<usize> = None;
    let mut pending_literal = false;
    let mut idx = 0;
    while idx < all.len() {
        let (start, end) = all[idx];
        let content_start = skip_blank(bytes, start);
        let indent = content_start - start;
        idx += 1;
        if content_start >= end {
            continue;
        }
        if let Some(li) = literal_indent {
            if indent > li {
                sink.push(content_start, end, HighlightTag::String);
                continue;
            }
            literal_indent = None;
        }
        if pending_literal {
            pending_literal = false;
            if indent > 0 {
                literal_indent = Some(indent.saturating_sub(1));
                sink.push(content_start, end, HighlightTag::String);
                continue;
            }
        }
        let rest = &src[content_start..end];
        // Section adornments: overline/underline.
        if is_adornment(rest) {
            // Overline: next line is the title and the one after repeats the adornment.
            if idx + 1 < all.len() {
                let (ts, te) = all[idx];
                let (us, ue) = all[idx + 1];
                if ts < te
                    && is_adornment(&src[us..ue])
                    && src[us..ue].trim_end() == rest.trim_end()
                {
                    sink.push(content_start, end, HighlightTag::Heading);
                    sink.push(ts, te, HighlightTag::Heading);
                    sink.push(us, ue, HighlightTag::Heading);
                    idx += 2;
                    continue;
                }
            }
            // Underline of the previous non-blank line.
            if idx >= 2 {
                let (ps, pe) = all[idx - 2];
                let prev = &src[ps..pe];
                if !prev.trim().is_empty() && !is_adornment(prev) && !is_table_line(prev) {
                    sink.spans.retain(|s| {
                        usize::from(s.range.start()) < ps || usize::from(s.range.start()) >= pe
                    });
                    sink.push(ps, pe, HighlightTag::Heading);
                    sink.push(content_start, end, HighlightTag::Heading);
                    continue;
                }
            }
            if rest
                .trim_end()
                .chars()
                .all(|c| c == '-' || c == '=' || c == '+')
            {
                sink.push(content_start, end, HighlightTag::Punctuation);
            }
            continue;
        }
        if is_table_line(rest) {
            sink.push(content_start, end, HighlightTag::Punctuation);
            continue;
        }
        // Explicit markup: directives, targets, substitutions, footnotes, comments.
        if rest.starts_with(".. ") || rest == ".." {
            let body = rest[2..].trim_start();
            let body_start = content_start + (rest.len() - body.len());
            if let Some(p) = body.find("::") {
                let name = &body[..p];
                if !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '|' | ' '))
                    && !name.contains("  ")
                {
                    if let Some(after_bar) = name.strip_prefix('|') {
                        // Substitution definition `.. |name| directive::`
                        if let Some(q) = after_bar.find('|') {
                            sink.push(body_start, body_start + q + 2, HighlightTag::Variable);
                            sink.push(content_start, body_start, HighlightTag::Keyword);
                            sink.push(
                                body_start + q + 2,
                                body_start + p + 2,
                                HighlightTag::Keyword,
                            );
                        }
                    } else {
                        sink.push(content_start, body_start + p + 2, HighlightTag::Keyword);
                    }
                    inline(&mut sink, src, body_start + p + 2, end);
                    if body.trim_end().ends_with("::") && body.trim_end().len() == p + 2 {
                        pending_literal = false;
                    }
                    continue;
                }
            }
            if body.starts_with('_') {
                // Hyperlink target `.. _name: url`
                if let Some(p) = body.find(": ").or_else(|| body.rfind(':')) {
                    sink.push(content_start, body_start + p + 1, HighlightTag::Link);
                    inline(&mut sink, src, body_start + p + 1, end);
                    let url_start = skip_blank(bytes, body_start + p + 1);
                    if url_start < end {
                        sink.push(url_start, end, HighlightTag::String);
                    }
                } else {
                    sink.push(content_start, end, HighlightTag::Link);
                }
                continue;
            }
            if body.starts_with('[') {
                // Footnote / citation definition.
                if let Some(p) = body.find(']') {
                    sink.push(content_start, body_start + p + 1, HighlightTag::Link);
                    inline(&mut sink, src, body_start + p + 1, end);
                    continue;
                }
            }
            // Plain comment.
            sink.push(content_start, end, HighlightTag::Comment);
            continue;
        }
        // Doctest.
        if rest.starts_with(">>> ") || rest == ">>>" {
            sink.push(content_start, content_start + 3, HighlightTag::Keyword);
            if rest.len() > 4 {
                let spans = crate::python::highlight_python(&src[content_start + 4..end]);
                sink.push_offset(spans, content_start + 4);
            }
            continue;
        }
        if rest.starts_with("... ") {
            sink.push(content_start, content_start + 3, HighlightTag::Keyword);
            let spans = crate::python::highlight_python(&src[content_start + 4..end]);
            sink.push_offset(spans, content_start + 4);
            continue;
        }
        let mut i = content_start;
        // Field list `:name: value` or directive option.
        if let Some(after_colon) = rest.strip_prefix(':') {
            if let Some(p) = after_colon.find(':') {
                let field = &after_colon[..p];
                if !field.is_empty() && !field.contains('`') {
                    sink.push(i, i + p + 2, HighlightTag::Key);
                    i += p + 2;
                }
            }
        }
        // Bullet / enumerated list markers.
        let r = &src[i..end];
        let rb = r.as_bytes();
        if rb.len() >= 2 && matches!(rb[0], b'-' | b'*' | b'+') && rb[1] == b' ' {
            sink.push(i, i + 1, HighlightTag::Punctuation);
            i += 2;
        } else {
            let mut j = 0;
            while j < rb.len() && (rb[j].is_ascii_alphanumeric() || rb[j] == b'#') && j < 4 {
                j += 1;
            }
            if j > 0 && j + 1 < rb.len() && matches!(rb[j], b'.' | b')') && rb[j + 1] == b' ' {
                sink.push(i, i + j + 1, HighlightTag::Punctuation);
                i += j + 2;
            }
        }
        if src[i..end].trim_end().ends_with("::") {
            let te = i + src[i..end].trim_end().len();
            sink.push(te - 2, te, HighlightTag::Punctuation);
            inline(&mut sink, src, i, te - 2);
            pending_literal = true;
            continue;
        }
        inline(&mut sink, src, i, end);
    }
    finish(src, sink.spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::testing::*;

    const SAMPLE: &str = "=======\n Title\n=======\n\nSection\n-------\n\nText with *em*, **strong**, ``lit`` and :func:`foo` and `Link <http://x>`_ and |sub| [1]_.\n\n.. note:: Something\n   :class: tip\n\n.. _target: https://example.com\n\n.. a comment\n\nExample::\n\n    literal\n    block\n\nBack to text.\n\n>>> print(1)\n1\n\n- bullet\n1. first\n\n:Author: me\n\n+---+---+\n| a | b |\n+---+---+\n";

    #[test]
    fn tags() {
        let t = tagged(SAMPLE, &highlight_rst(SAMPLE));
        assert!(has(&t, " Title", HighlightTag::Heading));
        assert!(has(&t, "=======", HighlightTag::Heading));
        assert!(has(&t, "Section", HighlightTag::Heading));
        assert!(has(&t, "-------", HighlightTag::Heading));
        assert!(has(&t, "*em*", HighlightTag::Emphasis));
        assert!(has(&t, "**strong**", HighlightTag::Strong));
        assert!(has(&t, "``lit``", HighlightTag::String));
        assert!(has(&t, ":func:", HighlightTag::Keyword));
        assert!(has(&t, "`foo`", HighlightTag::String));
        assert!(has(&t, "`Link <http://x>`_", HighlightTag::Link));
        assert!(has(&t, "|sub|", HighlightTag::Variable));
        assert!(has(&t, "[1]_", HighlightTag::Link));
        assert!(has(&t, ".. note::", HighlightTag::Keyword));
        assert!(has(&t, ":class:", HighlightTag::Key));
        assert!(has(&t, ".. _target:", HighlightTag::Link));
        assert!(has(&t, ".. a comment", HighlightTag::Comment));
        assert!(has(&t, "::", HighlightTag::Punctuation));
        assert!(has(&t, "literal", HighlightTag::String));
        assert!(has(&t, "block", HighlightTag::String));
        assert!(!has(&t, "Back to text.", HighlightTag::String));
        assert!(has(&t, ">>>", HighlightTag::Keyword));
        assert!(has(&t, "print", HighlightTag::Builtin));
        assert!(has(&t, "-", HighlightTag::Punctuation));
        assert!(has(&t, "1.", HighlightTag::Punctuation));
        assert!(has(&t, ":Author:", HighlightTag::Key));
        assert!(has(&t, "+---+---+", HighlightTag::Punctuation));
    }

    #[test]
    fn invariants_and_truncation() {
        check_all(highlight_rst, SAMPLE);
        check_all(
            highlight_rst,
            "Title\n=====\n``open\n.. x::\n   :a:\n\nText::\n\n  lit\n*open",
        );
        check_all(
            highlight_rst,
            "Заголовок\n=========\n\nТекст *курсив* ``код``\n",
        );
    }
}
