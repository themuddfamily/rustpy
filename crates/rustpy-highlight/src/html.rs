//! HTML / XML / SVG highlighter: tags, attributes, values, entities, comments,
//! with embedded `<script>` (JavaScript) and `<style>` (CSS) bodies.

use crate::common::{
    comment_tag, find_from, finish, is_ident_continue, is_ident_start, scan_quoted, Sink,
};
use crate::{HighlightTag, Span};

fn find_ci(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from >= bytes.len() || needle.is_empty() {
        return None;
    }
    bytes[from..]
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
        .map(|p| from + p)
}

/// Scans a tag starting at `<`; returns the index after `>` (or `end`), the
/// lowercase tag name, and whether the tag has a body to embed (an opening,
/// non-self-closing tag).
fn scan_tag(src: &str, i: usize, end: usize, sink: &mut Sink) -> (usize, String, bool) {
    let bytes = src.as_bytes();
    let mut j = i + 1;
    let closing = j < end && bytes[j] == b'/';
    if closing {
        j += 1;
    }
    let name_start = j;
    while j < end
        && (is_ident_continue(bytes[j]) || bytes[j] == b'-' || bytes[j] == b':' || bytes[j] == b'.')
    {
        j += 1;
    }
    let name = src[name_start..j].to_ascii_lowercase();
    sink.push(i, j, HighlightTag::Tag);
    // Attributes.
    while j < end {
        let b = bytes[j];
        match b {
            b'>' => {
                sink.push(j, j + 1, HighlightTag::Tag);
                return (j + 1, name, !closing);
            }
            b'/' | b'?' if j + 1 < end && bytes[j + 1] == b'>' => {
                sink.push(j, j + 2, HighlightTag::Tag);
                return (j + 2, name, false);
            }
            b'<' => {
                // Malformed: a new tag starts.
                return (j, name, false);
            }
            b'=' => {
                sink.push(j, j + 1, HighlightTag::Operator);
                j += 1;
                // Attribute value.
                while j < end && matches!(bytes[j], b' ' | b'\t' | b'\r' | b'\n') {
                    j += 1;
                }
                if j < end && (bytes[j] == b'"' || bytes[j] == b'\'') {
                    let e = scan_quoted(bytes, j, bytes[j], false, true).min(end);
                    sink.push(j, e, HighlightTag::String);
                    j = e.max(j + 1);
                } else {
                    let s = j;
                    while j < end
                        && !matches!(bytes[j], b' ' | b'\t' | b'\r' | b'\n' | b'>')
                        && !(bytes[j] == b'/' && j + 1 < end && bytes[j + 1] == b'>')
                    {
                        j += 1;
                    }
                    sink.push(s, j, HighlightTag::String);
                }
            }
            b if is_ident_start(b) => {
                let s = j;
                while j < end
                    && (is_ident_continue(bytes[j])
                        || matches!(
                            bytes[j],
                            b'-' | b':' | b'.' | b'@' | b'[' | b']' | b'(' | b')'
                        ))
                {
                    j += 1;
                }
                sink.push(s, j, HighlightTag::Parameter);
            }
            _ => j += 1,
        }
    }
    (end, name, false)
}

pub fn highlight_html(src: &str) -> Vec<Span> {
    let bytes = src.as_bytes();
    let end = bytes.len();
    let mut sink = Sink::default();
    let mut i = 0;
    while i < end {
        let b = bytes[i];
        if b == b'<' {
            if bytes[i..].starts_with(b"<!--") {
                let e = find_from(bytes, i + 4, b"-->")
                    .map(|p| p + 3)
                    .unwrap_or(end);
                sink.push(i, e, comment_tag(&src[i..e]));
                i = e;
                continue;
            }
            if bytes[i..].starts_with(b"<![CDATA[") {
                let e = find_from(bytes, i + 9, b"]]>")
                    .map(|p| p + 3)
                    .unwrap_or(end);
                sink.push(i, e, HighlightTag::String);
                i = e;
                continue;
            }
            if i + 1 < end && (bytes[i + 1] == b'!' || bytes[i + 1] == b'?') {
                // <!DOCTYPE ...> / <?xml ... ?>
                let e = bytes[i..]
                    .iter()
                    .position(|c| *c == b'>')
                    .map(|p| i + p + 1)
                    .unwrap_or(end);
                sink.push(i, e, HighlightTag::Keyword);
                i = e;
                continue;
            }
            if i + 1 < end && (is_ident_start(bytes[i + 1]) || bytes[i + 1] == b'/') {
                let (after, name, has_body) = scan_tag(src, i, end, &mut sink);
                i = after;
                if has_body && !name.is_empty() {
                    let embedded = match name.as_str() {
                        "script" => Some("script"),
                        "style" => Some("style"),
                        _ => None,
                    };
                    if let Some(kind) = embedded {
                        let close_needle = format!("</{kind}");
                        let body_end = find_ci(bytes, i, close_needle.as_bytes()).unwrap_or(end);
                        if body_end > i {
                            let spans = if kind == "script" {
                                crate::javascript::highlight_javascript(&src[i..body_end])
                            } else {
                                crate::css::highlight_css(&src[i..body_end])
                            };
                            sink.push_offset(spans, i);
                        }
                        i = body_end;
                    }
                }
                continue;
            }
            i += 1;
            continue;
        }
        if b == b'&' {
            // Entity: &name; or &#123; or &#x1F;
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
                continue;
            }
        }
        i += 1;
    }
    finish(src, sink.spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::testing::*;

    const SAMPLE: &str = "<!DOCTYPE html>\n<html lang=\"en\">\n<!-- a comment\n spanning -->\n<head>\n  <style>body { color: red; }</style>\n  <script type=\"module\">const x = 1; // js</script>\n</head>\n<body class='main' data-id=5>\n  <p>Tom &amp; Jerry &#169;</p>\n  <br/>\n  <svg:rect width=\"10\"/>\n</body>\n</html>\n";

    #[test]
    fn tags() {
        let t = tagged(SAMPLE, &highlight_html(SAMPLE));
        assert!(has(&t, "<!DOCTYPE html>", HighlightTag::Keyword));
        assert!(has(&t, "<html", HighlightTag::Tag));
        assert!(has(&t, "lang", HighlightTag::Parameter));
        assert!(has(&t, "\"en\"", HighlightTag::String));
        assert!(has(
            &t,
            "<!-- a comment\n spanning -->",
            HighlightTag::Comment
        ));
        assert!(has(&t, "color", HighlightTag::Key));
        assert!(has(&t, "const", HighlightTag::Keyword));
        assert!(has(&t, "// js", HighlightTag::Comment));
        assert!(has(&t, "</script", HighlightTag::Tag));
        assert!(has(&t, "'main'", HighlightTag::String));
        assert!(has(&t, "5", HighlightTag::String));
        assert!(has(&t, "&amp;", HighlightTag::EscapeSequence));
        assert!(has(&t, "&#169;", HighlightTag::EscapeSequence));
        assert!(has(&t, "/>", HighlightTag::Tag));
        assert!(has(&t, "<svg:rect", HighlightTag::Tag));
    }

    #[test]
    fn invariants_and_truncation() {
        check_all(highlight_html, SAMPLE);
        check_all(
            highlight_html,
            "<div class=\"open <p>x</p> <!-- open <script>let a = `t ${",
        );
        check_all(
            highlight_html,
            "<p title=\"заголовок\">текст &amp; ещё</p>\n",
        );
    }
}
