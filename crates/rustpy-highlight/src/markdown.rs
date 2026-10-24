//! Markdown highlighter: headings, emphasis, code spans and fences (Python
//! fences delegate to the Python highlighter), links, lists, quotes, tables.

use crate::common::{finish, lines, skip_blank, Sink};
use crate::{HighlightTag, Span};

fn is_setext_underline(line: &str) -> bool {
    let t = line.trim_end();
    t.len() >= 3 && (t.chars().all(|c| c == '=') || t.chars().all(|c| c == '-'))
}

fn is_thematic_break(line: &str) -> bool {
    let t: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    t.len() >= 3
        && (t.chars().all(|c| c == '-')
            || t.chars().all(|c| c == '*')
            || t.chars().all(|c| c == '_'))
}

/// `(fence char, fence length, info string)` when the line opens a fenced code block.
fn fence(line: &str) -> Option<(u8, usize, String)> {
    let t = line.trim_start();
    let b = t.as_bytes();
    let c = *b.first()?;
    if c != b'`' && c != b'~' {
        return None;
    }
    let n = b.iter().take_while(|x| **x == c).count();
    if n < 3 {
        return None;
    }
    let info = t[n..].trim().to_string();
    if c == b'`' && info.contains('`') {
        return None;
    }
    Some((c, n, info))
}

/// Highlights inline constructs in `start..end`.
fn inline(sink: &mut Sink, src: &str, start: usize, end: usize) {
    let bytes = src.as_bytes();
    let mut i = start;
    while i < end {
        let b = bytes[i];
        match b {
            b'\\' if i + 1 < end && bytes[i + 1].is_ascii_punctuation() => {
                sink.push(i, i + 2, HighlightTag::EscapeSequence);
                i += 2;
            }
            b'`' => {
                let n = bytes[i..end].iter().take_while(|c| **c == b'`').count();
                // Closing run of the same length.
                let mut j = i + n;
                let mut closed = None;
                while j < end {
                    if bytes[j] == b'`' {
                        let m = bytes[j..end].iter().take_while(|c| **c == b'`').count();
                        if m == n {
                            closed = Some(j + m);
                            break;
                        }
                        j += m;
                    } else {
                        j += 1;
                    }
                }
                match closed {
                    Some(e) => {
                        sink.push(i, e, HighlightTag::String);
                        i = e;
                    }
                    None => i += n,
                }
            }
            b'!' if i + 1 < end && bytes[i + 1] == b'[' => {
                i = link(sink, src, i, i + 1, end);
            }
            b'[' => {
                i = link(sink, src, i, i, end);
            }
            b'<' if i + 1 < end
                && (bytes[i + 1].is_ascii_alphabetic()
                    || bytes[i + 1] == b'/'
                    || bytes[i + 1] == b'!') =>
            {
                let close = bytes[i..end]
                    .iter()
                    .position(|c| *c == b'>')
                    .map(|p| i + p + 1);
                match close {
                    Some(e)
                        if !src[i..e].contains(' ')
                            && (src[i..e].contains("://") || src[i..e].contains('@')) =>
                    {
                        sink.push(i, e, HighlightTag::Link);
                        i = e;
                    }
                    Some(e) if !src[i..e].contains('\n') => {
                        sink.push(i, e, HighlightTag::Tag);
                        i = e;
                    }
                    _ => i += 1,
                }
            }
            b'&' => {
                let mut j = i + 1;
                if j < end && bytes[j] == b'#' {
                    j += 1;
                }
                while j < end && bytes[j].is_ascii_alphanumeric() {
                    j += 1;
                }
                if j < end && bytes[j] == b';' && j > i + 1 {
                    sink.push(i, j + 1, HighlightTag::EscapeSequence);
                    i = j + 1;
                } else {
                    i += 1;
                }
            }
            b'h' if src[i..end].starts_with("http://") || src[i..end].starts_with("https://") => {
                let mut j = i;
                while j < end
                    && !matches!(
                        bytes[j],
                        b' ' | b'\t' | b'\n' | b')' | b'>' | b'"' | b'\'' | b'<'
                    )
                {
                    j += 1;
                }
                while j > i && matches!(bytes[j - 1], b'.' | b',' | b';' | b':') {
                    j -= 1;
                }
                sink.push(i, j, HighlightTag::Link);
                i = j.max(i + 1);
            }
            b'*' | b'_' | b'~' => {
                let n = bytes[i..end].iter().take_while(|c| **c == b).count().min(3);
                let marker = &bytes[i..i + n];
                // Find the closing marker on the same line, not followed by the marker char.
                let search_from = i + n;
                if search_from < end && !bytes[search_from].is_ascii_whitespace() {
                    let mut j = search_from;
                    let mut closed = None;
                    while j + n <= end {
                        if &bytes[j..j + n] == marker
                            && !bytes[j - 1].is_ascii_whitespace()
                            && (j + n >= end || bytes[j + n] != b)
                        {
                            closed = Some(j + n);
                            break;
                        }
                        if bytes[j] == b'\n' {
                            break;
                        }
                        j += 1;
                    }
                    if let Some(e) = closed {
                        let tag = if b == b'~' || n >= 2 {
                            HighlightTag::Strong
                        } else {
                            HighlightTag::Emphasis
                        };
                        sink.push(i, e, tag);
                        // Nested inline code / links inside emphasis are not re-scanned.
                        i = e;
                        continue;
                    }
                }
                i += n.max(1);
            }
            _ => i += 1,
        }
    }
}

/// Highlights `[text](dest)` / `[text][ref]` / `[ref]: dest` starting at `start`
/// (`bracket` is the index of `[`). Returns the index to continue from.
fn link(sink: &mut Sink, src: &str, start: usize, bracket: usize, end: usize) -> usize {
    let bytes = src.as_bytes();
    let mut depth = 0;
    let mut j = bracket;
    let mut close = None;
    while j < end && bytes[j] != b'\n' {
        match bytes[j] {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(j);
                    break;
                }
            }
            b'\\' => j += 1,
            _ => {}
        }
        j += 1;
    }
    let Some(close) = close else { return start + 1 };
    let text_end = close + 1;
    // Footnote / checkbox.
    let inner = &src[bracket + 1..close];
    if inner == " " || inner == "x" || inner == "X" {
        sink.push(bracket, text_end, HighlightTag::Keyword);
        return text_end;
    }
    if text_end < end && bytes[text_end] == b'(' {
        let mut k = text_end + 1;
        let mut d = 1;
        while k < end && bytes[k] != b'\n' {
            match bytes[k] {
                b'(' => d += 1,
                b')' => {
                    d -= 1;
                    if d == 0 {
                        break;
                    }
                }
                _ => {}
            }
            k += 1;
        }
        if k < end && bytes[k] == b')' {
            sink.push(start, text_end, HighlightTag::Link);
            sink.push(text_end, k + 1, HighlightTag::String);
            return k + 1;
        }
    }
    if text_end < end && bytes[text_end] == b'[' {
        if let Some(p) = bytes[text_end..end]
            .iter()
            .position(|c| *c == b']' || *c == b'\n')
        {
            if bytes[text_end + p] == b']' {
                sink.push(start, text_end + p + 1, HighlightTag::Link);
                return text_end + p + 1;
            }
        }
    }
    if text_end < end && bytes[text_end] == b':' && bracket == start {
        // Reference definition `[id]: url`.
        sink.push(start, text_end + 1, HighlightTag::Link);
        let mut k = skip_blank(bytes, text_end + 1);
        let s = k;
        while k < end && !matches!(bytes[k], b' ' | b'\t' | b'\n') {
            k += 1;
        }
        sink.push(s, k, HighlightTag::String);
        return k;
    }
    if inner.starts_with('^') {
        sink.push(start, text_end, HighlightTag::Link);
        return text_end;
    }
    sink.push(start, text_end, HighlightTag::Link);
    text_end
}

pub fn highlight_markdown(src: &str) -> Vec<Span> {
    let bytes = src.as_bytes();
    let mut sink = Sink::default();
    let all = lines(src);
    let mut fence_state: Option<(u8, usize, String, usize)> = None; // char, len, info, body start
    let mut prev_blank = true;
    let mut prev_text_line: Option<(usize, usize)> = None;
    let mut idx = 0;
    while idx < all.len() {
        let (start, end) = all[idx];
        let line = &src[start..end];
        idx += 1;
        if let Some((c, n, info, body_start)) = &fence_state {
            let t = line.trim_start();
            let closes = t.as_bytes().iter().take_while(|x| **x == *c).count() >= *n
                && t.trim_end().chars().all(|ch| ch as u32 == *c as u32);
            if closes {
                let body_end = start.saturating_sub(1).max(*body_start);
                if info.eq_ignore_ascii_case("python")
                    || info.eq_ignore_ascii_case("py")
                    || info.starts_with("python ")
                {
                    let spans = crate::python::highlight_python(&src[*body_start..body_end]);
                    sink.push_offset(spans, *body_start);
                } else {
                    sink.push(*body_start, body_end, HighlightTag::String);
                }
                sink.push(start, end, HighlightTag::String);
                fence_state = None;
                prev_blank = false;
            }
            continue;
        }
        if let Some((c, n, info)) = fence(line) {
            sink.push(start, end, HighlightTag::String);
            fence_state = Some((c, n, info, (end + 1).min(bytes.len())));
            prev_blank = false;
            prev_text_line = None;
            continue;
        }
        let content_start = skip_blank(bytes, start);
        if content_start >= end {
            prev_blank = true;
            prev_text_line = None;
            continue;
        }
        let indent = content_start - start;
        // Indented code block (after a blank line).
        if indent >= 4 && prev_blank {
            sink.push(content_start, end, HighlightTag::String);
            prev_blank = false;
            prev_text_line = None;
            continue;
        }
        let rest = &src[content_start..end];
        // ATX heading.
        if rest.starts_with('#') {
            let hashes = rest.bytes().take_while(|b| *b == b'#').count();
            if hashes <= 6
                && (rest.len() == hashes
                    || rest.as_bytes()[hashes] == b' '
                    || rest.as_bytes()[hashes] == b'\t')
            {
                sink.push(content_start, end, HighlightTag::Heading);
                prev_blank = false;
                prev_text_line = None;
                continue;
            }
        }
        // Setext heading underline.
        if is_setext_underline(rest) && !prev_blank {
            if let Some((ts, te)) = prev_text_line {
                sink.spans.retain(|s| {
                    usize::from(s.range.start()) < ts || usize::from(s.range.start()) >= te
                });
                sink.push(ts, te, HighlightTag::Heading);
                sink.push(content_start, end, HighlightTag::Heading);
                prev_blank = false;
                prev_text_line = None;
                continue;
            }
        }
        if is_thematic_break(rest) {
            sink.push(content_start, end, HighlightTag::Punctuation);
            prev_blank = false;
            prev_text_line = None;
            continue;
        }
        // Block quote markers and list markers (possibly nested).
        let mut i = content_start;
        loop {
            let b = bytes[i];
            if b == b'>' {
                sink.push(i, i + 1, HighlightTag::Punctuation);
                i = skip_blank(bytes, i + 1);
                if i >= end {
                    break;
                }
                continue;
            }
            if matches!(b, b'-' | b'*' | b'+')
                && i + 1 < end
                && (bytes[i + 1] == b' ' || bytes[i + 1] == b'\t')
            {
                sink.push(i, i + 1, HighlightTag::Punctuation);
                i = skip_blank(bytes, i + 1);
                if i >= end {
                    break;
                }
                continue;
            }
            if b.is_ascii_digit() {
                let mut j = i;
                while j < end && bytes[j].is_ascii_digit() && j - i < 9 {
                    j += 1;
                }
                if j < end
                    && matches!(bytes[j], b'.' | b')')
                    && j + 1 < end
                    && (bytes[j + 1] == b' ' || bytes[j + 1] == b'\t')
                {
                    sink.push(i, j + 1, HighlightTag::Punctuation);
                    i = skip_blank(bytes, j + 1);
                    if i >= end {
                        break;
                    }
                    continue;
                }
            }
            break;
        }
        // Tables: pipe rows.
        if src[i..end].contains('|') {
            let row = &src[i..end];
            let sep = row
                .chars()
                .all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t'))
                && row.contains('-');
            if sep {
                sink.push(i, end, HighlightTag::Punctuation);
                prev_blank = false;
                prev_text_line = None;
                continue;
            }
            let mut k = i;
            while k < end {
                if bytes[k] == b'|' {
                    sink.push(k, k + 1, HighlightTag::Punctuation);
                }
                k += 1;
            }
        }
        inline(&mut sink, src, i, end);
        prev_text_line = Some((content_start, end));
        prev_blank = false;
    }
    if let Some((_, _, info, body_start)) = fence_state {
        // Unterminated fence: body runs to the end of the text.
        if body_start < bytes.len() {
            if info.eq_ignore_ascii_case("python") || info.eq_ignore_ascii_case("py") {
                let spans = crate::python::highlight_python(&src[body_start..]);
                sink.push_offset(spans, body_start);
            } else {
                sink.push(body_start, bytes.len(), HighlightTag::String);
            }
        }
    }
    finish(src, sink.spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::testing::*;

    const SAMPLE: &str = "# Title\n\nSetext\n======\n\nSome *em* and **strong** with `code` and [link](http://x.y) and <b>tag</b> &amp; https://a.b/c.\n\n- item one\n  - [x] done\n1. first\n> quote\n\n```python\ndef f():\n    return 1  # c\n```\n\n~~~\nplain\n~~~\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n---\n[ref]: https://example.com\n";

    #[test]
    fn tags() {
        let t = tagged(SAMPLE, &highlight_markdown(SAMPLE));
        assert!(has(&t, "# Title", HighlightTag::Heading));
        assert!(has(&t, "Setext", HighlightTag::Heading));
        assert!(has(&t, "======", HighlightTag::Heading));
        assert!(has(&t, "*em*", HighlightTag::Emphasis));
        assert!(has(&t, "**strong**", HighlightTag::Strong));
        assert!(has(&t, "`code`", HighlightTag::String));
        assert!(has(&t, "[link]", HighlightTag::Link));
        assert!(has(&t, "(http://x.y)", HighlightTag::String));
        assert!(has(&t, "<b>", HighlightTag::Tag));
        assert!(has(&t, "&amp;", HighlightTag::EscapeSequence));
        assert!(has(&t, "https://a.b/c", HighlightTag::Link));
        assert!(has(&t, "-", HighlightTag::Punctuation));
        assert!(has(&t, "[x]", HighlightTag::Keyword));
        assert!(has(&t, "1.", HighlightTag::Punctuation));
        assert!(has(&t, ">", HighlightTag::Punctuation));
        assert!(has(&t, "```python", HighlightTag::String));
        assert!(has(&t, "def", HighlightTag::Keyword));
        assert!(has(&t, "# c", HighlightTag::Comment));
        assert!(has(&t, "plain", HighlightTag::String));
        assert!(has(&t, "|---|---|", HighlightTag::Punctuation));
        assert!(has(&t, "---", HighlightTag::Punctuation));
        assert!(has(&t, "[ref]:", HighlightTag::Link));
        assert!(has(&t, "https://example.com", HighlightTag::String));
    }

    #[test]
    fn invariants_and_truncation() {
        check_all(highlight_markdown, SAMPLE);
        check_all(highlight_markdown, "```py\nx = (\n\n**open *em [link](x\n");
        check_all(
            highlight_markdown,
            "# Заголовок\n\nТекст *курсив* `код` [ссылка](http://пример.рф)\n",
        );
    }
}
