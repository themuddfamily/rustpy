//! Jinja2 / Django template highlighter: HTML with `{{ … }}`, `{% … %}` and
//! `{# … #}` template constructs layered on top.

use crate::html::highlight_html;
use crate::{HighlightTag, Span, TextRange};
use rustpy_core::TextSize;

const KEYWORDS: &[&str] = &[
    "if",
    "elif",
    "else",
    "endif",
    "for",
    "endfor",
    "in",
    "not",
    "and",
    "or",
    "is",
    "block",
    "endblock",
    "extends",
    "include",
    "import",
    "from",
    "as",
    "macro",
    "endmacro",
    "call",
    "endcall",
    "filter",
    "endfilter",
    "set",
    "endset",
    "with",
    "endwith",
    "raw",
    "endraw",
    "autoescape",
    "endautoescape",
    "trans",
    "endtrans",
    "blocktrans",
    "endblocktrans",
    "load",
    "url",
    "static",
    "csrf_token",
    "comment",
    "endcomment",
    "empty",
    "spaceless",
    "endspaceless",
    "verbatim",
    "endverbatim",
    "now",
    "cycle",
    "firstof",
    "ifchanged",
    "endifchanged",
    "regroup",
    "widthratio",
    "templatetag",
    "do",
    "continue",
    "break",
    "recursive",
    "ignore",
    "missing",
    "without",
    "context",
    "only",
    "scoped",
    "required",
    "true",
    "false",
    "none",
    "True",
    "False",
    "None",
    "loop",
    "super",
    "self",
    "varargs",
    "kwargs",
    "caller",
];

fn sp(a: usize, b: usize, tag: HighlightTag) -> Span {
    Span {
        range: TextRange::new(TextSize::new(a as u32), TextSize::new(b as u32)),
        tag,
    }
}

/// Template constructs as `(start, end, kind)` with kind `{`, `%` or `#`.
fn constructs(src: &str) -> Vec<(usize, usize, u8)> {
    rustpy_syntax::template::highlight_constructs(src)
}

fn inner_spans(src: &str, start: usize, end: usize, out: &mut Vec<Span>) {
    let b = src.as_bytes();
    let mut i = start;
    let mut after_pipe = false;
    while i < end {
        let c = b[i];
        if c == b'"' || c == b'\'' {
            let mut j = i + 1;
            while j < end && b[j] != c {
                if b[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            let j = (j + 1).min(end);
            out.push(sp(i, j, HighlightTag::String));
            i = j;
        } else if c.is_ascii_digit() {
            let mut j = i;
            while j < end && (b[j].is_ascii_digit() || b[j] == b'.') {
                j += 1;
            }
            out.push(sp(i, j, HighlightTag::Number));
            i = j;
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let mut j = i;
            while j < end && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            let word = &src[i..j];
            let tag = if after_pipe {
                HighlightTag::FunctionCall
            } else if KEYWORDS.contains(&word) {
                HighlightTag::Keyword
            } else {
                HighlightTag::Variable
            };
            out.push(sp(i, j, tag));
            after_pipe = false;
            i = j;
        } else {
            if c == b'|' {
                after_pipe = true;
                out.push(sp(i, i + 1, HighlightTag::Operator));
            } else if !c.is_ascii_whitespace() {
                after_pipe = false;
                if matches!(
                    c,
                    b'=' | b'<' | b'>' | b'!' | b'+' | b'-' | b'*' | b'/' | b'%' | b'~'
                ) {
                    out.push(sp(i, i + 1, HighlightTag::Operator));
                }
            }
            i += 1;
        }
    }
}

/// Removes base-span portions covered by `holes` (sorted, disjoint).
fn cut(base: Vec<Span>, holes: &[(usize, usize)]) -> Vec<Span> {
    let mut out = Vec::with_capacity(base.len());
    for s in base {
        let (mut a, b) = (usize::from(s.range.start()), usize::from(s.range.end()));
        for &(h0, h1) in holes {
            if h1 <= a || h0 >= b {
                continue;
            }
            if h0 > a {
                out.push(sp(a, h0, s.tag));
            }
            a = a.max(h1);
        }
        if a < b {
            out.push(sp(a, b, s.tag));
        }
    }
    out
}

pub fn highlight_jinja(src: &str) -> Vec<Span> {
    let cs = constructs(src);
    // HTML highlighting of the text with template constructs blanked out.
    let mut masked = src.as_bytes().to_vec();
    for &(a, b, _) in &cs {
        for x in &mut masked[a..b] {
            if *x != b'\n' {
                *x = b' ';
            }
        }
    }
    let masked = String::from_utf8(masked).unwrap_or_default();
    let holes: Vec<(usize, usize)> = cs.iter().map(|&(a, b, _)| (a, b)).collect();
    let mut spans = cut(highlight_html(&masked), &holes);
    for &(a, b, kind) in &cs {
        if kind == b'#' {
            spans.push(sp(a, b, HighlightTag::Comment));
            continue;
        }
        let closed = b >= a + 4 && src.as_bytes()[b - 1] == b'}';
        let inner_end = if closed { b - 2 } else { b };
        // Delimiters, including whitespace-control dashes.
        let mut s = a + 2;
        if src.as_bytes().get(s) == Some(&b'-') || src.as_bytes().get(s) == Some(&b'+') {
            s += 1;
        }
        let mut e = inner_end;
        if e > s && src.as_bytes()[e - 1] == b'-' {
            e -= 1;
        }
        spans.push(sp(a, s, HighlightTag::Decorator));
        inner_spans(src, s, e, &mut spans);
        if closed {
            spans.push(sp(e, b, HighlightTag::Decorator));
        }
    }
    spans.sort_by_key(|s| s.range.start());
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tagged<'a>(src: &'a str, spans: &[Span], tag: HighlightTag) -> Vec<&'a str> {
        spans
            .iter()
            .filter(|s| s.tag == tag)
            .map(|s| &src[usize::from(s.range.start())..usize::from(s.range.end())])
            .collect()
    }

    #[test]
    fn template_constructs_over_html() {
        let src = "{% extends \"base.html\" %}\n{# hidden #}\n<a href=\"{{ url_for('x') }}\">{{ user.name|title }}</a>\n{%- for i in items -%}{{ i }}{% endfor %}";
        let spans = highlight_jinja(src);
        let kw = tagged(src, &spans, HighlightTag::Keyword);
        assert!(
            kw.contains(&"extends")
                && kw.contains(&"for")
                && kw.contains(&"in")
                && kw.contains(&"endfor")
        );
        assert_eq!(
            tagged(src, &spans, HighlightTag::Comment),
            vec!["{# hidden #}"]
        );
        assert!(
            tagged(src, &spans, HighlightTag::FunctionCall).contains(&"title"),
            "filters after | are highlighted"
        );
        assert!(
            tagged(src, &spans, HighlightTag::Tag)
                .iter()
                .any(|t| t.contains('a')),
            "HTML tags still highlighted"
        );
        let delims = tagged(src, &spans, HighlightTag::Decorator);
        assert!(delims.contains(&"{%-") && delims.contains(&"-%}"));
        // No overlapping spans.
        for w in spans.windows(2) {
            assert!(
                w[0].range.end() <= w[1].range.start(),
                "overlap: {:?} {:?}",
                w[0],
                w[1]
            );
        }
    }

    #[test]
    fn unclosed_construct_runs_to_end() {
        let src = "<p>{{ broken";
        let spans = highlight_jinja(src);
        assert!(tagged(src, &spans, HighlightTag::Variable).contains(&"broken"));
    }
}
