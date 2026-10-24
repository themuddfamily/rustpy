//! Dockerfile highlighter: instructions, flags, strings, variables, JSON exec
//! forms, with shell-form `RUN`/`CMD`/`ENTRYPOINT` bodies highlighted as shell.

use crate::common::{
    comment_tag, finish, is_ident_continue, is_ident_start, line_end, scan_quoted, skip_blank, Sink,
};
use crate::{HighlightTag, Span};

static INSTRUCTIONS: &[&str] = &[
    "FROM",
    "RUN",
    "CMD",
    "LABEL",
    "MAINTAINER",
    "EXPOSE",
    "ENV",
    "ADD",
    "COPY",
    "ENTRYPOINT",
    "VOLUME",
    "USER",
    "WORKDIR",
    "ARG",
    "ONBUILD",
    "STOPSIGNAL",
    "HEALTHCHECK",
    "SHELL",
];

/// Highlights `$VAR` / `${VAR}` / quoted strings / numbers in a generic argument range.
fn generic_args(sink: &mut Sink, src: &str, start: usize, end: usize, key_values: bool) {
    let bytes = src.as_bytes();
    let mut i = start;
    let mut expect_key = key_values;
    while i < end {
        let b = bytes[i];
        match b {
            b' ' | b'\t' | b'\n' | b'\r' => {
                i += 1;
                if key_values {
                    expect_key = true;
                }
            }
            b'\\' => {
                sink.push(i, i + 1, HighlightTag::Operator);
                i += 1;
            }
            b'"' => {
                let e = scan_quoted(bytes, i, b'"', true, true).min(end);
                sink.push(i, e, HighlightTag::String);
                i = e.max(i + 1);
                expect_key = false;
            }
            b'\'' => {
                let e = scan_quoted(bytes, i, b'\'', false, true).min(end);
                sink.push(i, e, HighlightTag::String);
                i = e.max(i + 1);
                expect_key = false;
            }
            b'$' => {
                let mut j = i + 1;
                if j < end && bytes[j] == b'{' {
                    while j < end && bytes[j] != b'}' && bytes[j] != b'\n' {
                        j += 1;
                    }
                    if j < end && bytes[j] == b'}' {
                        j += 1;
                    }
                } else {
                    while j < end && is_ident_continue(bytes[j]) {
                        j += 1;
                    }
                }
                if j > i + 1 {
                    sink.push(i, j, HighlightTag::Variable);
                }
                i = j.max(i + 1);
                expect_key = false;
            }
            b'-' if i + 1 < end && bytes[i + 1] == b'-' => {
                let mut j = i + 2;
                while j < end && (is_ident_continue(bytes[j]) || bytes[j] == b'-') {
                    j += 1;
                }
                sink.push(i, j, HighlightTag::Parameter);
                if j < end && bytes[j] == b'=' {
                    sink.push(j, j + 1, HighlightTag::Operator);
                    j += 1;
                }
                i = j;
                expect_key = false;
            }
            b if is_ident_start(b) => {
                let mut j = i;
                while j < end
                    && (is_ident_continue(bytes[j]) || bytes[j] == b'.' || bytes[j] == b'-')
                {
                    j += 1;
                }
                let word = &src[i..j];
                if word.eq_ignore_ascii_case("AS") && !key_values {
                    sink.push(i, j, HighlightTag::Keyword);
                } else if expect_key && j < end && bytes[j] == b'=' {
                    sink.push(i, j, HighlightTag::Key);
                    sink.push(j, j + 1, HighlightTag::Operator);
                    j += 1;
                } else if expect_key && key_values {
                    // `ENV KEY value` legacy form.
                    sink.push(i, j, HighlightTag::Key);
                }
                expect_key = false;
                i = j.max(i + 1);
            }
            b'0'..=b'9' => {
                let mut j = i;
                while j < end
                    && (bytes[j].is_ascii_digit()
                        || bytes[j] == b'/'
                        || bytes[j] == b'.'
                        || bytes[j] == b'-')
                {
                    j += 1;
                }
                sink.push(i, j, HighlightTag::Number);
                i = j;
                expect_key = false;
            }
            _ => {
                i += 1;
                expect_key = false;
            }
        }
    }
}

/// JSON exec form `["a", "b"]`.
fn json_args(sink: &mut Sink, src: &str, start: usize, end: usize) {
    let bytes = src.as_bytes();
    let mut i = start;
    while i < end {
        match bytes[i] {
            b'[' | b']' => {
                sink.push(i, i + 1, HighlightTag::Bracket);
                i += 1;
            }
            b',' => {
                sink.push(i, i + 1, HighlightTag::Comma);
                i += 1;
            }
            b'"' => {
                let e = scan_quoted(bytes, i, b'"', true, true).min(end);
                sink.push(i, e, HighlightTag::String);
                i = e.max(i + 1);
            }
            _ => i += 1,
        }
    }
}

pub fn highlight_dockerfile(src: &str) -> Vec<Span> {
    let bytes = src.as_bytes();
    let mut sink = Sink::default();
    let mut i = 0;
    while i < bytes.len() {
        let line_start = skip_blank(bytes, i);
        let le = line_end(bytes, line_start);
        if line_start >= le {
            i = le + 1;
            continue;
        }
        if bytes[line_start] == b'#' {
            sink.push(line_start, le, comment_tag(&src[line_start..le]));
            i = le + 1;
            continue;
        }
        // Instruction spans continuation lines ending with `\`.
        let mut stmt_end = le;
        while stmt_end < bytes.len() && src[..stmt_end].trim_end().ends_with('\\') {
            let next = line_end(bytes, stmt_end + 1);
            stmt_end = next;
            if next >= bytes.len() {
                break;
            }
        }
        let mut j = line_start;
        while j < stmt_end && bytes[j].is_ascii_alphabetic() {
            j += 1;
        }
        let word = &src[line_start..j];
        let upper = word.to_ascii_uppercase();
        if INSTRUCTIONS.contains(&upper.as_str()) {
            sink.push(line_start, j, HighlightTag::Keyword);
            let mut a = skip_blank(bytes, j);
            // Flags right after the instruction (`--from=builder`, `--platform=...`).
            while a + 1 < stmt_end && bytes[a] == b'-' && bytes[a + 1] == b'-' {
                let mut k = a + 2;
                while k < stmt_end && (is_ident_continue(bytes[k]) || bytes[k] == b'-') {
                    k += 1;
                }
                sink.push(a, k, HighlightTag::Parameter);
                if k < stmt_end && bytes[k] == b'=' {
                    sink.push(k, k + 1, HighlightTag::Operator);
                    k += 1;
                    let vs = k;
                    while k < stmt_end && !matches!(bytes[k], b' ' | b'\t' | b'\n') {
                        k += 1;
                    }
                    sink.push(vs, k, HighlightTag::String);
                }
                a = skip_blank(bytes, k);
            }
            let first = if a < stmt_end { bytes[a] } else { b'\n' };
            match upper.as_str() {
                "RUN" | "CMD" | "ENTRYPOINT" | "SHELL" | "HEALTHCHECK" if first == b'[' => {
                    json_args(&mut sink, src, a, stmt_end)
                }
                "RUN" | "CMD" | "ENTRYPOINT" => {
                    let spans = crate::shell::highlight_shell(&src[a..stmt_end]);
                    sink.push_offset(spans, a);
                }
                "HEALTHCHECK" => {
                    // HEALTHCHECK [options] CMD command
                    let rest = &src[a..stmt_end];
                    if let Some(p) = rest.find("CMD") {
                        generic_args(&mut sink, src, a, a + p, false);
                        sink.push(a + p, a + p + 3, HighlightTag::Keyword);
                        let spans = crate::shell::highlight_shell(&src[a + p + 3..stmt_end]);
                        sink.push_offset(spans, a + p + 3);
                    } else {
                        generic_args(&mut sink, src, a, stmt_end, false);
                    }
                }
                "ENV" | "ARG" | "LABEL" => generic_args(&mut sink, src, a, stmt_end, true),
                "ONBUILD" => {
                    // Nested instruction.
                    let mut k = a;
                    while k < stmt_end && bytes[k].is_ascii_alphabetic() {
                        k += 1;
                    }
                    if INSTRUCTIONS.contains(&src[a..k].to_ascii_uppercase().as_str()) {
                        sink.push(a, k, HighlightTag::Keyword);
                    }
                    generic_args(&mut sink, src, k, stmt_end, false);
                }
                _ if first == b'[' => json_args(&mut sink, src, a, stmt_end),
                _ => generic_args(&mut sink, src, a, stmt_end, false),
            }
        } else {
            generic_args(&mut sink, src, line_start, stmt_end, false);
        }
        i = stmt_end + 1;
    }
    finish(src, sink.spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::testing::*;

    const SAMPLE: &str = "# syntax=docker/dockerfile:1\nFROM python:3.12-slim AS base\nARG APP_HOME=/app\nENV PYTHONUNBUFFERED=1 \\\n    PATH=\"${APP_HOME}/bin:$PATH\"\nWORKDIR $APP_HOME\nCOPY --from=base --chown=1000:1000 . .\nRUN pip install -r requirements.txt && \\\n    echo \"done\" # TODO cache\nEXPOSE 8000\nHEALTHCHECK --interval=30s CMD curl -f http://localhost/ || exit 1\nCMD [\"python\", \"-m\", \"app\"]\nLABEL maintainer=\"me\"\n";

    #[test]
    fn tags() {
        let t = tagged(SAMPLE, &highlight_dockerfile(SAMPLE));
        assert!(has(
            &t,
            "# syntax=docker/dockerfile:1",
            HighlightTag::Comment
        ));
        assert!(has(&t, "FROM", HighlightTag::Keyword));
        assert!(has(&t, "AS", HighlightTag::Keyword));
        assert!(has(&t, "ARG", HighlightTag::Keyword));
        assert!(has(&t, "APP_HOME", HighlightTag::Key));
        assert!(has(&t, "PYTHONUNBUFFERED", HighlightTag::Key));
        assert!(has(&t, "PATH", HighlightTag::Key));
        assert!(has(&t, "\"${APP_HOME}/bin:$PATH\"", HighlightTag::String));
        assert!(has(&t, "$APP_HOME", HighlightTag::Variable));
        assert!(has(&t, "--from", HighlightTag::Parameter));
        assert!(has(&t, "base", HighlightTag::String));
        assert!(has(&t, "pip", HighlightTag::Builtin));
        assert!(has(&t, "-r", HighlightTag::Parameter));
        assert!(has(&t, "&&", HighlightTag::Operator));
        assert!(has(&t, "# TODO cache", HighlightTag::TodoComment));
        assert!(has(&t, "8000", HighlightTag::Number));
        assert!(has(&t, "HEALTHCHECK", HighlightTag::Keyword));
        assert!(has(&t, "CMD", HighlightTag::Keyword));
        assert!(has(&t, "\"python\"", HighlightTag::String));
        assert!(has(&t, "maintainer", HighlightTag::Key));
    }

    #[test]
    fn invariants_and_truncation() {
        check_all(highlight_dockerfile, SAMPLE);
        check_all(
            highlight_dockerfile,
            "RUN echo \"open \\\n  more\nENV X=\nCMD [\"a\", \n",
        );
        check_all(
            highlight_dockerfile,
            "LABEL описание=\"значение\"\nRUN echo привет\n",
        );
    }
}
