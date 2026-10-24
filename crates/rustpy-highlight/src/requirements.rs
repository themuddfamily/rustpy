//! `requirements.txt` highlighter: package names, extras, version specifiers,
//! environment markers, URLs, options and comments.

use crate::common::{comment_tag, finish, lines, scan_quoted, skip_blank, Sink};
use crate::{HighlightTag, Span};

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')
}

fn marker_segment(sink: &mut Sink, src: &str, start: usize, end: usize) {
    let bytes = src.as_bytes();
    let mut i = start;
    while i < end {
        let b = bytes[i];
        match b {
            b' ' | b'\t' => i += 1,
            b'"' | b'\'' => {
                let e = scan_quoted(bytes, i, b, false, false).min(end);
                sink.push(i, e, HighlightTag::String);
                i = e.max(i + 1);
            }
            b'(' | b')' => {
                sink.push(i, i + 1, HighlightTag::Parenthesis);
                i += 1;
            }
            b'=' | b'<' | b'>' | b'!' | b'~' => {
                let mut j = i + 1;
                while j < end && matches!(bytes[j], b'=' | b'<' | b'>') && j - i < 3 {
                    j += 1;
                }
                sink.push(i, j, HighlightTag::Operator);
                i = j;
            }
            b if b.is_ascii_alphabetic() || b == b'_' => {
                let mut j = i;
                while j < end
                    && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_' || bytes[j] == b'.')
                {
                    j += 1;
                }
                let word = &src[i..j];
                let tag = match word {
                    "and" | "or" | "not" | "in" => HighlightTag::Keyword,
                    _ => HighlightTag::Variable,
                };
                sink.push(i, j, tag);
                i = j;
            }
            _ => i += 1,
        }
    }
}

/// Version specifier segment (`>=1.2,<2.0.*`).
fn spec_segment(sink: &mut Sink, src: &str, start: usize, end: usize) {
    let bytes = src.as_bytes();
    let mut i = start;
    while i < end {
        let b = bytes[i];
        match b {
            b' ' | b'\t' => i += 1,
            b',' => {
                sink.push(i, i + 1, HighlightTag::Comma);
                i += 1;
            }
            b'(' | b')' => {
                sink.push(i, i + 1, HighlightTag::Parenthesis);
                i += 1;
            }
            b'=' | b'<' | b'>' | b'!' | b'~' => {
                let mut j = i + 1;
                while j < end && matches!(bytes[j], b'=' | b'<' | b'>') && j - i < 3 {
                    j += 1;
                }
                sink.push(i, j, HighlightTag::Operator);
                i = j;
            }
            _ => {
                let mut j = i;
                while j < end && !matches!(bytes[j], b' ' | b'\t' | b',' | b')' | b';') {
                    j += 1;
                }
                if bytes[i].is_ascii_digit() || bytes[i] == b'*' {
                    sink.push(i, j, HighlightTag::Number);
                } else {
                    sink.push(i, j, HighlightTag::String);
                }
                i = j.max(i + 1);
            }
        }
    }
}

pub fn highlight_requirements(src: &str) -> Vec<Span> {
    let bytes = src.as_bytes();
    let mut sink = Sink::default();
    for (start, end) in lines(src) {
        let i = skip_blank(bytes, start);
        if i >= end {
            continue;
        }
        // Inline comment: ` #` or line start `#`.
        let mut code_end = end;
        if bytes[i] == b'#' {
            sink.push(i, end, comment_tag(&src[i..end]));
            continue;
        }
        if let Some(p) = src[i..end].find(" #").or_else(|| src[i..end].find("\t#")) {
            code_end = i + p;
            sink.push(code_end + 1, end, comment_tag(&src[code_end + 1..end]));
        }
        // Line continuation backslash.
        let trimmed_end = {
            let t = src[i..code_end].trim_end();
            let mut e = i + t.len();
            if t.ends_with('\\') {
                sink.push(e - 1, e, HighlightTag::Operator);
                e -= 1;
            }
            e
        };
        let line = &src[i..trimmed_end];
        if line.starts_with('-') {
            // Option: -r file / --index-url URL / --hash=sha256:...
            let mut j = i;
            while j < trimmed_end && !matches!(bytes[j], b' ' | b'\t' | b'=') {
                j += 1;
            }
            sink.push(i, j, HighlightTag::Keyword);
            if j < trimmed_end && bytes[j] == b'=' {
                sink.push(j, j + 1, HighlightTag::Operator);
                j += 1;
            }
            let a = skip_blank(bytes, j);
            if a < trimmed_end {
                let arg = &src[a..trimmed_end];
                let tag = if arg.contains("://") {
                    HighlightTag::Link
                } else {
                    HighlightTag::String
                };
                sink.push(a, trimmed_end, tag);
            }
            continue;
        }
        if line.contains("://") && !line.contains(" @ ") && !line.contains('@') {
            sink.push(i, trimmed_end, HighlightTag::Link);
            continue;
        }
        if line.starts_with('.') || line.starts_with('/') || line.starts_with("file:") {
            sink.push(i, trimmed_end, HighlightTag::String);
            continue;
        }
        // Package name.
        let mut j = i;
        while j < trimmed_end && is_name_byte(bytes[j]) {
            j += 1;
        }
        sink.push(i, j, HighlightTag::Key);
        // Extras.
        let mut k = skip_blank(bytes, j).min(trimmed_end);
        if k < trimmed_end && bytes[k] == b'[' {
            let close = bytes[k..trimmed_end]
                .iter()
                .position(|c| *c == b']')
                .map(|p| k + p + 1)
                .unwrap_or(trimmed_end);
            sink.push(k, k + 1, HighlightTag::Bracket);
            let mut m = k + 1;
            while m < close {
                if is_name_byte(bytes[m]) {
                    let mut n = m;
                    while n < close && is_name_byte(bytes[n]) {
                        n += 1;
                    }
                    sink.push(m, n, HighlightTag::Parameter);
                    m = n;
                } else {
                    if bytes[m] == b',' {
                        sink.push(m, m + 1, HighlightTag::Comma);
                    } else if bytes[m] == b']' {
                        sink.push(m, m + 1, HighlightTag::Bracket);
                    }
                    m += 1;
                }
            }
            k = close;
        }
        // Marker split.
        let marker_at = src[k..trimmed_end].find(';').map(|p| k + p);
        let spec_end = marker_at.unwrap_or(trimmed_end);
        // Direct reference `name @ url`.
        let rest = &src[k..spec_end];
        if let Some(p) = rest.find('@') {
            sink.push(k + p, k + p + 1, HighlightTag::Operator);
            let u = skip_blank(bytes, k + p + 1);
            sink.push(u, spec_end, HighlightTag::Link);
        } else {
            spec_segment(&mut sink, src, k, spec_end);
        }
        if let Some(m) = marker_at {
            sink.push(m, m + 1, HighlightTag::Punctuation);
            marker_segment(&mut sink, src, m + 1, trimmed_end);
        }
    }
    finish(src, sink.spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::testing::*;

    const SAMPLE: &str = "# deps\n-r base.txt\n--index-url https://pypi.org/simple\nrequests>=2.28,<3  # http\nnumpy==1.26.*\nfoo[extra1,extra2] ~= 1.0 ; python_version >= \"3.8\" and sys_platform != 'win32'\nbar @ https://example.com/bar.whl\ngit+https://github.com/u/r.git\n./local/pkg\nbaz --hash=sha256:abc \\\n    --hash=sha256:def\n";

    #[test]
    fn tags() {
        let t = tagged(SAMPLE, &highlight_requirements(SAMPLE));
        assert!(has(&t, "# deps", HighlightTag::Comment));
        assert!(has(&t, "-r", HighlightTag::Keyword));
        assert!(has(&t, "base.txt", HighlightTag::String));
        assert!(has(&t, "--index-url", HighlightTag::Keyword));
        assert!(has(&t, "https://pypi.org/simple", HighlightTag::Link));
        assert!(has(&t, "requests", HighlightTag::Key));
        assert!(has(&t, ">=", HighlightTag::Operator));
        assert!(has(&t, "2.28", HighlightTag::Number));
        assert!(has(&t, "# http", HighlightTag::Comment));
        assert!(has(&t, "1.26.*", HighlightTag::Number));
        assert!(has(&t, "extra1", HighlightTag::Parameter));
        assert!(has(&t, "~=", HighlightTag::Operator));
        assert!(has(&t, ";", HighlightTag::Punctuation));
        assert!(has(&t, "python_version", HighlightTag::Variable));
        assert!(has(&t, "\"3.8\"", HighlightTag::String));
        assert!(has(&t, "and", HighlightTag::Keyword));
        assert!(has(&t, "https://example.com/bar.whl", HighlightTag::Link));
        assert!(has(
            &t,
            "git+https://github.com/u/r.git",
            HighlightTag::Link
        ));
        assert!(has(&t, "./local/pkg", HighlightTag::String));
        assert!(has(&t, "baz", HighlightTag::Key));
    }

    #[test]
    fn invariants_and_truncation() {
        check_all(highlight_requirements, SAMPLE);
        check_all(
            highlight_requirements,
            "pkg[; python_version == \"3\n--hash\n@\n",
        );
        check_all(highlight_requirements, "пакет>=1  # комментарий\n");
    }
}
