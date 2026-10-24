//! Shell script highlighter (bash/zsh/sh): keywords, builtins, strings with
//! interpolation, heredocs, variables, options, operators, comments.

use rustc_hash::FxHashSet;

use crate::common::{
    comment_tag, finish, is_ident_continue, is_ident_start, line_end, scan_ident, skip_blank, Sink,
};
use crate::{HighlightTag, Span};

static KEYWORDS: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac",
    "in", "function", "select", "time", "coproc", "return", "break", "continue", "exit", "export",
    "local", "readonly", "declare", "typeset", "unset", "shift", "source", "alias", "unalias",
    "set",
];

static BUILTINS: &[&str] = &[
    "echo", "cd", "pwd", "printf", "read", "test", "eval", "exec", "true", "false", "pushd",
    "popd", "dirs", "trap", "wait", "kill", "jobs", "fg", "bg", "let", "getopts", "command",
    "type", "hash", "ulimit", "umask", "sudo", "ls", "cat", "grep", "sed", "awk", "find", "xargs",
    "mkdir", "rm", "cp", "mv", "touch", "chmod", "chown", "tar", "curl", "wget", "git", "python",
    "python3", "pip", "make", "cargo", "docker", "ssh", "env", "which", "sort", "uniq", "head",
    "tail", "wc", "tr", "cut", "tee", "sleep", "date", "basename", "dirname", "realpath", "rmdir",
    "ln", "mktemp",
];

fn keyword_set() -> &'static FxHashSet<&'static str> {
    static SET: std::sync::OnceLock<FxHashSet<&'static str>> = std::sync::OnceLock::new();
    SET.get_or_init(|| KEYWORDS.iter().copied().collect())
}

fn builtin_set() -> &'static FxHashSet<&'static str> {
    static SET: std::sync::OnceLock<FxHashSet<&'static str>> = std::sync::OnceLock::new();
    SET.get_or_init(|| BUILTINS.iter().copied().collect())
}

/// End of a `$...` expansion starting at `i`, or `i` when none.
fn scan_expansion(bytes: &[u8], i: usize, end: usize) -> usize {
    if i + 1 >= end {
        return i;
    }
    let c = bytes[i + 1];
    if c == b'{' {
        let mut depth = 0;
        let mut j = i + 1;
        while j < end {
            match bytes[j] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return j + 1;
                    }
                }
                b'\n' => return j,
                _ => {}
            }
            j += 1;
        }
        end
    } else if is_ident_start(c) {
        scan_ident(bytes, i + 1).min(end)
    } else if c.is_ascii_digit() || matches!(c, b'@' | b'*' | b'?' | b'$' | b'!' | b'#' | b'-') {
        i + 2
    } else {
        i
    }
}

/// Double-quoted string with `$` interpolation; returns the end index.
fn scan_double(src: &str, i: usize, end: usize, sink: &mut Sink) -> usize {
    let bytes = src.as_bytes();
    let mut seg = i;
    let mut j = i + 1;
    while j < end {
        match bytes[j] {
            b'\\' => j += 2,
            b'"' => {
                sink.push(seg, j + 1, HighlightTag::String);
                return j + 1;
            }
            b'$' => {
                let e = scan_expansion(bytes, j, end);
                if e > j {
                    sink.push(seg, j, HighlightTag::String);
                    sink.push(j, e, HighlightTag::Variable);
                    seg = e;
                    j = e;
                } else {
                    j += 1;
                }
            }
            _ => j += 1,
        }
    }
    sink.push(seg, end, HighlightTag::String);
    end
}

pub fn highlight_shell(src: &str) -> Vec<Span> {
    let bytes = src.as_bytes();
    let end = bytes.len();
    let mut sink = Sink::default();
    let mut i = 0;
    // True when the next word starts a command (so keywords/builtins apply).
    let mut command_start = true;
    let mut pending_heredoc: Option<(String, bool)> = None; // delimiter, strip tabs
    while i < end {
        let b = bytes[i];
        match b {
            b'\n' => {
                i += 1;
                command_start = true;
                if let Some((delim, strip)) = pending_heredoc.take() {
                    // Heredoc body: lines until the delimiter.
                    let body_start = i;
                    let mut body_end = i;
                    let mut term: Option<(usize, usize)> = None;
                    let mut k = i;
                    while k <= end {
                        let le = line_end(bytes, k);
                        let line = &src[k..le];
                        let candidate = if strip {
                            line.trim_start_matches('\t')
                        } else {
                            line
                        };
                        if candidate == delim {
                            term = Some((k, le));
                            break;
                        }
                        body_end = le;
                        if le >= end {
                            break;
                        }
                        k = le + 1;
                    }
                    match term {
                        Some((ts, te)) => {
                            sink.push(body_start, ts, HighlightTag::String);
                            sink.push(ts, te, HighlightTag::Keyword);
                            i = te;
                        }
                        None => {
                            sink.push(body_start, body_end.max(body_start), HighlightTag::String);
                            i = end;
                        }
                    }
                }
            }
            b' ' | b'\t' | b'\r' => i += 1,
            b'#' if i == 0
                || matches!(
                    bytes[i - 1],
                    b' ' | b'\t' | b'\n' | b';' | b'(' | b'&' | b'|'
                ) =>
            {
                let e = line_end(bytes, i);
                sink.push(i, e, comment_tag(&src[i..e]));
                i = e;
            }
            b'\'' => {
                let mut j = i + 1;
                while j < end && bytes[j] != b'\'' {
                    j += 1;
                }
                let e = (j + 1).min(end);
                sink.push(i, e, HighlightTag::String);
                i = e;
                command_start = false;
            }
            b'"' => {
                i = scan_double(src, i, end, &mut sink);
                command_start = false;
            }
            b'$' if i + 1 < end && (bytes[i + 1] == b'\'' || bytes[i + 1] == b'"') => {
                if bytes[i + 1] == b'\'' {
                    let mut j = i + 2;
                    while j < end && bytes[j] != b'\'' {
                        if bytes[j] == b'\\' {
                            j += 1;
                        }
                        j += 1;
                    }
                    let e = (j + 1).min(end);
                    sink.push(i, e, HighlightTag::String);
                    i = e;
                } else {
                    sink.push(i, i + 1, HighlightTag::String);
                    i = scan_double(src, i + 1, end, &mut sink);
                }
                command_start = false;
            }
            b'$' if i + 1 < end && bytes[i + 1] == b'(' => {
                let n = if i + 2 < end && bytes[i + 2] == b'(' {
                    3
                } else {
                    2
                };
                sink.push(i, i + n, HighlightTag::Parenthesis);
                i += n;
                command_start = true;
            }
            b'$' => {
                let e = scan_expansion(bytes, i, end);
                if e > i {
                    sink.push(i, e, HighlightTag::Variable);
                    i = e;
                } else {
                    i += 1;
                }
                command_start = false;
            }
            b'`' => {
                sink.push(i, i + 1, HighlightTag::Punctuation);
                i += 1;
                command_start = true;
            }
            b'<' if i + 1 < end
                && bytes[i + 1] == b'<'
                && !(i + 2 < end && bytes[i + 2] == b'<') =>
            {
                // Heredoc operator.
                let mut j = i + 2;
                let strip = j < end && bytes[j] == b'-';
                if strip {
                    j += 1;
                }
                j = skip_blank(bytes, j);
                let quoted = j < end && (bytes[j] == b'\'' || bytes[j] == b'"');
                let ds = if quoted { j + 1 } else { j };
                let mut de = ds;
                while de < end
                    && !matches!(
                        bytes[de],
                        b' ' | b'\t' | b'\n' | b';' | b'\'' | b'"' | b'|' | b'&' | b'<' | b'>'
                    )
                {
                    de += 1;
                }
                let delim = src[ds..de].to_string();
                let mut e = de;
                if quoted && e < end && (bytes[e] == b'\'' || bytes[e] == b'"') {
                    e += 1;
                }
                sink.push(i, e.max(i + 2), HighlightTag::Keyword);
                if !delim.is_empty() {
                    pending_heredoc = Some((delim, strip));
                }
                i = e.max(i + 2);
                command_start = false;
            }
            b'|' | b'&' | b';' | b'>' | b'<' => {
                let mut j = i + 1;
                while j < end && matches!(bytes[j], b'|' | b'&' | b'>' | b'<') && j - i < 3 {
                    j += 1;
                }
                sink.push(i, j, HighlightTag::Operator);
                i = j;
                command_start = true;
            }
            b'(' | b')' => {
                sink.push(i, i + 1, HighlightTag::Parenthesis);
                command_start = b == b'(';
                i += 1;
            }
            b'{' | b'}' => {
                sink.push(i, i + 1, HighlightTag::Brace);
                command_start = true;
                i += 1;
            }
            b'[' | b']' => {
                let n = if i + 1 < end && bytes[i + 1] == b {
                    2
                } else {
                    1
                };
                sink.push(i, i + n, HighlightTag::Bracket);
                i += n;
                command_start = false;
            }
            b'-' if i + 1 < end
                && (bytes[i + 1] == b'-' || bytes[i + 1].is_ascii_alphanumeric())
                && (i == 0 || matches!(bytes[i - 1], b' ' | b'\t' | b'\n')) =>
            {
                let mut j = i + 1;
                while j < end && (is_ident_continue(bytes[j]) || bytes[j] == b'-') {
                    j += 1;
                }
                sink.push(i, j, HighlightTag::Parameter);
                i = j;
                command_start = false;
            }
            b if is_ident_start(b) => {
                let e = scan_ident(bytes, i);
                let word = &src[i..e];
                let k = skip_blank(bytes, e);
                let next = if k < end { Some(bytes[k]) } else { None };
                if command_start && next == Some(b'(') && k + 1 < end && bytes[k + 1] == b')' {
                    sink.push(i, e, HighlightTag::FunctionDefinition);
                } else if e < end && bytes[e] == b'=' && command_start {
                    sink.push(i, e, HighlightTag::Variable);
                    sink.push(e, e + 1, HighlightTag::Operator);
                    i = e + 1;
                    continue;
                } else if keyword_set().contains(word) {
                    sink.push(i, e, HighlightTag::Keyword);
                    if word == "function" {
                        // `function name`
                        let ns = skip_blank(bytes, e);
                        if ns < end && is_ident_start(bytes[ns]) {
                            let ne = scan_ident(bytes, ns);
                            sink.push(ns, ne, HighlightTag::FunctionDefinition);
                            i = ne;
                            command_start = false;
                            continue;
                        }
                    }
                    command_start = matches!(
                        word,
                        "if" | "then"
                            | "else"
                            | "elif"
                            | "do"
                            | "while"
                            | "until"
                            | "time"
                            | "exec"
                            | "in"
                    );
                    i = e;
                    continue;
                } else if command_start && builtin_set().contains(word) {
                    sink.push(i, e, HighlightTag::Builtin);
                }
                command_start = false;
                i = e;
            }
            b'0'..=b'9' if i == 0 || matches!(bytes[i - 1], b' ' | b'\t' | b'\n' | b'=') => {
                let mut j = i;
                while j < end && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                if j < end && (bytes[j] == b'>' || bytes[j] == b'<') {
                    // Redirection with a file descriptor: `2>&1`, `2>>log`, `3<file`.
                    while j < end && matches!(bytes[j], b'>' | b'<' | b'&' | b'0'..=b'9') {
                        j += 1;
                    }
                    sink.push(i, j, HighlightTag::Operator);
                } else if j >= end
                    || matches!(
                        bytes[j],
                        b' ' | b'\t' | b'\n' | b';' | b'&' | b'|' | b')' | b']'
                    )
                {
                    sink.push(i, j, HighlightTag::Number);
                }
                i = j;
                command_start = false;
            }
            b'!' => {
                sink.push(i, i + 1, HighlightTag::Keyword);
                i += 1;
            }
            _ => {
                i += 1;
                command_start = false;
            }
        }
    }
    finish(src, sink.spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::testing::*;

    const SAMPLE: &str = "#!/usr/bin/env bash\nset -euo pipefail\nNAME=\"world $USER\"\ngreet() {\n  echo \"hi ${NAME}\" | tr a-z A-Z > /tmp/out 2>&1\n}\nif [[ -f \"$1\" ]]; then\n  cat <<EOF\nbody $NAME\nEOF\nfi\nfor i in 1 2 3; do echo $i; done # TODO\nfunction other { ls -la --color=auto; }\n";

    #[test]
    fn tags() {
        let t = tagged(SAMPLE, &highlight_shell(SAMPLE));
        assert!(has(&t, "#!/usr/bin/env bash", HighlightTag::Comment));
        assert!(has(&t, "set", HighlightTag::Keyword));
        assert!(has(&t, "-euo", HighlightTag::Parameter));
        assert!(has(&t, "NAME", HighlightTag::Variable));
        assert!(has(&t, "$USER", HighlightTag::Variable));
        assert!(has(&t, "\"world ", HighlightTag::String));
        assert!(has(&t, "greet", HighlightTag::FunctionDefinition));
        assert!(has(&t, "echo", HighlightTag::Builtin));
        assert!(has(&t, "${NAME}", HighlightTag::Variable));
        assert!(has(&t, "|", HighlightTag::Operator));
        assert!(has(&t, "2>&1", HighlightTag::Operator));
        assert!(has(&t, "if", HighlightTag::Keyword));
        assert!(has(&t, "[[", HighlightTag::Bracket));
        assert!(has(&t, "<<EOF", HighlightTag::Keyword));
        assert!(has(&t, "body $NAME\n", HighlightTag::String));
        assert!(has(&t, "EOF", HighlightTag::Keyword));
        assert!(has(&t, "fi", HighlightTag::Keyword));
        assert!(has(&t, "# TODO", HighlightTag::TodoComment));
        assert!(has(&t, "other", HighlightTag::FunctionDefinition));
        assert!(has(&t, "--color", HighlightTag::Parameter));
        assert!(has(&t, "1", HighlightTag::Number));
    }

    #[test]
    fn invariants_and_truncation() {
        check_all(highlight_shell, SAMPLE);
        check_all(highlight_shell, "echo \"open ${X\ncat <<'EOF'\nnever\n");
        check_all(highlight_shell, "echo \"привет $USER\" # комментарий\n");
    }
}
