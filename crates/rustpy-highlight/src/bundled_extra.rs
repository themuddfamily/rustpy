//! Lightweight lossless coloring for the remaining bundled text formats.
use crate::common::{comment_tag, finish, lines, Sink};
use crate::{HighlightTag as T, Span};

pub fn highlight_ignore(src: &str) -> Vec<Span> {
    ignore_patterns(src, false)
}

/// `.dockerignore`: like `.gitignore`, but Docker trims each line before it
/// looks for the `!` exception marker (`#` still only comments in column 1).
pub fn highlight_dockerignore(src: &str) -> Vec<Span> {
    ignore_patterns(src, true)
}

fn ignore_patterns(src: &str, trim_leading: bool) -> Vec<Span> {
    let mut out = Sink::default();
    for (start, end) in lines(src) {
        let text = &src[start..end];
        if text.starts_with('#') {
            out.push(start, end, comment_tag(text));
            continue;
        }
        let start = if trim_leading {
            end - text.trim_start().len()
        } else {
            start
        };
        let mut i = start;
        while i < end {
            match src.as_bytes()[i] {
                b'\\' if i + 1 < end => {
                    let next = i + 1 + src[i + 1..end].chars().next().unwrap().len_utf8();
                    out.push(i, next, T::EscapeSequence);
                    i = next;
                }
                // Like the .ignore plugin: the negation marker reads as a
                // keyword and glob syntax as a pattern (both differ from the
                // plain-text colour in every bundled scheme).
                b'!' if i == start => {
                    out.push(i, i + 1, T::Keyword);
                    i += 1;
                }
                b'*' | b'?' => {
                    let mut next = i + 1;
                    while next < end && src.as_bytes()[next] == b'*' {
                        next += 1;
                    }
                    out.push(i, next, T::Number);
                    i = next;
                }
                b'[' => {
                    // A character class `[...]` is one glob token; an unclosed
                    // `[` is still glob syntax on its own.
                    let close = src[i + 1..end].find(']').map_or(i + 1, |at| i + 1 + at + 1);
                    out.push(i, close, T::Number);
                    i = close;
                }
                b']' => {
                    out.push(i, i + 1, T::Number);
                    i += 1;
                }
                b'/' => {
                    out.push(i, i + 1, T::Punctuation);
                    i += 1;
                }
                _ => i += src[i..].chars().next().unwrap().len_utf8(),
            }
        }
    }
    finish(src, out.spans)
}

pub fn highlight_diff(src: &str) -> Vec<Span> {
    let mut out = Sink::default();
    for (start, end) in lines(src) {
        let line = &src[start..end];
        let tag = if line.starts_with("@@") {
            Some(T::Heading)
        } else if line.starts_with("diff ")
            || line.starts_with("index ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
        {
            Some(T::Tag)
        } else if line.starts_with('+') {
            Some(T::String)
        } else if line.starts_with('-') {
            Some(T::Error)
        } else if line.starts_with("\\ No newline") {
            Some(T::Comment)
        } else {
            None
        };
        if let Some(tag) = tag {
            out.push(start, end, tag);
        }
    }
    finish(src, out.spans)
}

pub fn highlight_csv(src: &str) -> Vec<Span> {
    let bytes = src.as_bytes();
    let mut out = Sink::default();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let start = i;
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'"' {
                        if bytes.get(i + 1) == Some(&b'"') {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                out.push(start, i, T::String);
            }
            b',' | b';' | b'\t' => {
                out.push(i, i + 1, T::Comma);
                i += 1;
            }
            _ => {
                let start = i;
                while i < bytes.len()
                    && !matches!(bytes[i], b',' | b';' | b'\t' | b'\n' | b'\r' | b'"')
                {
                    i += 1;
                }
                if i == start {
                    i += 1;
                } else {
                    let value = src[start..i].trim();
                    if !value.is_empty() && value.parse::<f64>().is_ok() {
                        out.push(start, i, T::Number);
                    }
                }
            }
        }
    }
    finish(src, out.spans)
}

pub fn highlight_regex(src: &str) -> Vec<Span> {
    let mut out = Sink::default();
    let mut i = 0;
    let bytes = src.as_bytes();
    let mut in_class = false;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                let end = if i + 1 < bytes.len() {
                    i + 1 + src[i + 1..].chars().next().unwrap().len_utf8()
                } else {
                    i + 1
                };
                out.push(i, end, T::EscapeSequence);
                i = end;
            }
            b'[' if !in_class => {
                in_class = true;
                out.push(i, i + 1, T::Bracket);
                i += 1;
            }
            b']' if in_class => {
                in_class = false;
                out.push(i, i + 1, T::Bracket);
                i += 1;
            }
            b'(' | b')' if !in_class => {
                out.push(i, i + 1, T::Parenthesis);
                i += 1;
            }
            b'*' | b'+' | b'?' | b'|' | b'.' | b'^' | b'$' | b'{' | b'}' if !in_class => {
                out.push(i, i + 1, T::Operator);
                i += 1;
            }
            _ => i += src[i..].chars().next().unwrap().len_utf8(),
        }
    }
    finish(src, out.spans)
}

pub fn highlight_makefile(src: &str) -> Vec<Span> {
    let mut out = Sink::default();
    let bytes = src.as_bytes();
    for (start, end) in lines(src) {
        let line = &src[start..end];
        if line.starts_with('\t') {
            out.push_offset(crate::highlight_shell(&src[start + 1..end]), start + 1);
            continue;
        }
        let trimmed = line.trim_start();
        let first = start + line.len() - trimmed.len();
        if trimmed.starts_with('#') {
            out.push(first, end, comment_tag(trimmed));
            continue;
        }
        let word = trimmed.split_whitespace().next().unwrap_or("");
        if matches!(
            word,
            "include"
                | "-include"
                | "sinclude"
                | "ifdef"
                | "ifndef"
                | "ifeq"
                | "ifneq"
                | "else"
                | "endif"
                | "define"
                | "endef"
                | "override"
                | "export"
                | "unexport"
                | "private"
                | "vpath"
        ) {
            out.push(first, first + word.len(), T::Keyword);
        } else if let Some(separator) = line.find(['=', ':']) {
            let tail = &line[separator..];
            let assignment = tail.starts_with('=')
                || tail.starts_with(":=")
                || tail.starts_with("::=")
                || tail.starts_with(":::=");
            let mut key_end = start + separator;
            if assignment && key_end > start && matches!(bytes[key_end - 1], b'+' | b'?' | b'!') {
                key_end -= 1;
            }
            while key_end > start && bytes[key_end - 1].is_ascii_whitespace() {
                key_end -= 1;
            }
            out.push(
                start,
                key_end,
                if assignment { T::Variable } else { T::Heading },
            );
            let operator_end = if assignment {
                start + separator + tail.find('=').unwrap() + 1
            } else {
                start + separator + 1
            };
            out.push(key_end, operator_end, T::Operator);
        }
        let mut i = first;
        while i < end {
            if bytes[i] == b'$' {
                let a = i;
                i += 1;
                if i < end && matches!(bytes[i], b'(' | b'{') {
                    let close = if bytes[i] == b'(' { b')' } else { b'}' };
                    i += 1;
                    while i < end && bytes[i] != close {
                        i += 1;
                    }
                    if i < end {
                        i += 1;
                    }
                } else if i < end {
                    i += src[i..end].chars().next().unwrap().len_utf8();
                }
                out.push(a, i, T::Variable);
            } else {
                i += src[i..end].chars().next().unwrap().len_utf8();
            }
        }
    }
    finish(src, out.spans)
}

pub fn highlight_properties(src: &str) -> Vec<Span> {
    let mut out = Sink::default();
    let mut continuation = false;
    for (start, end) in lines(src) {
        let text = &src[start..end];
        let trimmed = text.trim_start();
        let first = start + text.len() - trimmed.len();
        if !continuation && (trimmed.starts_with('#') || trimmed.starts_with('!')) {
            out.push(first, end, comment_tag(trimmed));
            continue;
        }
        let mut escaped = false;
        let mut separator = None;
        if !continuation {
            for (offset, c) in trimmed.char_indices() {
                if escaped {
                    escaped = false;
                    continue;
                }
                if c == '\\' {
                    escaped = true;
                    continue;
                }
                if c == '=' || c == ':' || c.is_ascii_whitespace() {
                    separator = Some(first + offset);
                    break;
                }
            }
        }
        let value = if continuation {
            first
        } else if let Some(sep) = separator {
            out.push(first, sep, T::Key);
            let mut v = sep;
            while v < end && src.as_bytes()[v].is_ascii_whitespace() {
                v += 1;
            }
            if v < end && matches!(src.as_bytes()[v], b'=' | b':') {
                v += 1;
            }
            while v < end && src.as_bytes()[v].is_ascii_whitespace() {
                v += 1;
            }
            out.push(sep, v, T::Operator);
            v
        } else {
            out.push(first, end, T::Key);
            end
        };
        out.push(value, end, T::String);
        continuation = text
            .as_bytes()
            .iter()
            .rev()
            .take_while(|b| **b == b'\\')
            .count()
            % 2
            == 1;
    }
    finish(src, out.spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tagged(src: &str, spans: &[Span], tag: T) -> Vec<String> {
        spans
            .iter()
            .filter(|s| s.tag == tag)
            .map(|s| src[usize::from(s.range.start())..usize::from(s.range.end())].to_string())
            .collect()
    }

    #[test]
    fn dockerignore_colours_comments_negation_and_globs() {
        let src = "# context\n**/__pycache__\n  !keep/*.py[co]\nlogs/?.txt\n";
        let spans = highlight_dockerignore(src);
        assert_eq!(tagged(src, &spans, T::Comment), ["# context"]);
        assert_eq!(tagged(src, &spans, T::Keyword), ["!"]);
        assert_eq!(tagged(src, &spans, T::Number), ["**", "*", "[co]", "?"]);
        assert!(tagged(src, &spans, T::Operator).is_empty());
        assert_eq!(tagged(src, &spans, T::Punctuation), ["/", "/", "/"]);
        // .gitignore keeps `!` significant only in column 1.
        let git = highlight_ignore("  !keep\n!keep\n");
        assert_eq!(tagged("  !keep\n!keep\n", &git, T::Keyword), ["!"]);
    }

    #[test]
    fn ignore_negation_and_globs_differ_from_plain_text_in_every_scheme() {
        for name in crate::scheme::ColorScheme::builtin_names() {
            let scheme = crate::scheme::ColorScheme::by_name(name).unwrap();
            let plain = scheme.style(T::Default).fg;
            for tag in [T::Keyword, T::Number] {
                assert_ne!(scheme.style(tag).fg, plain, "{name}: {tag:?}");
            }
            assert_ne!(
                scheme.style(T::Keyword).fg,
                scheme.style(T::Number).fg,
                "{name}"
            );
        }
    }
}
