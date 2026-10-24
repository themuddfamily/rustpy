//! JavaScript / TypeScript highlighter: keywords, strings, template literals,
//! regex literals, comments, numbers, calls and definitions.

use rustc_hash::FxHashSet;

use crate::common::{
    bracket_tag, comment_tag, find_from, finish, is_ident_start, line_end, scan_ident, scan_number,
    scan_quoted, Sink,
};
use crate::{HighlightTag, Span};

static KEYWORDS: &[&str] = &[
    "abstract",
    "as",
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "constructor",
    "continue",
    "debugger",
    "declare",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "function",
    "get",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "is",
    "keyof",
    "let",
    "module",
    "namespace",
    "new",
    "null",
    "of",
    "override",
    "package",
    "private",
    "protected",
    "public",
    "readonly",
    "return",
    "satisfies",
    "set",
    "static",
    "switch",
    "throw",
    "true",
    "try",
    "type",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

static TYPES: &[&str] = &[
    "any", "unknown", "never", "string", "number", "boolean", "symbol", "bigint", "object",
];

static BUILTINS: &[&str] = &[
    "console",
    "window",
    "document",
    "globalThis",
    "Math",
    "JSON",
    "Promise",
    "Array",
    "Object",
    "String",
    "Number",
    "Boolean",
    "Map",
    "Set",
    "WeakMap",
    "WeakSet",
    "Symbol",
    "Error",
    "TypeError",
    "RangeError",
    "Date",
    "RegExp",
    "Function",
    "Reflect",
    "Proxy",
    "Intl",
    "parseInt",
    "parseFloat",
    "isNaN",
    "isFinite",
    "setTimeout",
    "setInterval",
    "clearTimeout",
    "clearInterval",
    "fetch",
    "require",
    "module",
    "exports",
    "process",
    "Buffer",
    "NaN",
    "Infinity",
    "arguments",
    "navigator",
    "location",
    "alert",
    "encodeURIComponent",
    "decodeURIComponent",
    "structuredClone",
    "queueMicrotask",
    "BigInt",
];

/// Keywords after which a `/` starts a regular expression.
static REGEX_PREFIX_KEYWORDS: &[&str] = &[
    "return",
    "typeof",
    "instanceof",
    "in",
    "of",
    "new",
    "delete",
    "void",
    "throw",
    "case",
    "do",
    "else",
    "yield",
    "await",
];

fn keyword_set() -> &'static FxHashSet<&'static str> {
    static SET: std::sync::OnceLock<FxHashSet<&'static str>> = std::sync::OnceLock::new();
    SET.get_or_init(|| KEYWORDS.iter().copied().collect())
}

fn builtin_set() -> &'static FxHashSet<&'static str> {
    static SET: std::sync::OnceLock<FxHashSet<&'static str>> = std::sync::OnceLock::new();
    SET.get_or_init(|| BUILTINS.iter().copied().collect())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Prev {
    /// Start of input or after an operator / open bracket / keyword: a `/` starts a regex.
    Operand,
    /// After an identifier, number, string or closing bracket: a `/` divides.
    Value,
    Function,
    Class,
    Dot,
}

fn is_regex_prefix_keyword(w: &str) -> bool {
    REGEX_PREFIX_KEYWORDS.contains(&w)
}

/// Scans `src[start..end]` as JavaScript, appending spans to `sink`.
fn scan_js(src: &str, start: usize, end: usize, sink: &mut Sink) {
    let bytes = src.as_bytes();
    let mut i = start;
    let mut prev = Prev::Operand;
    while i < end {
        let b = bytes[i];
        match b {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'/' if i + 1 < end && bytes[i + 1] == b'/' => {
                let e = line_end(bytes, i).min(end);
                sink.push(i, e, comment_tag(&src[i..e]));
                i = e;
            }
            b'/' if i + 1 < end && bytes[i + 1] == b'*' => {
                let e = find_from(&bytes[..end], i + 2, b"*/")
                    .map(|p| p + 2)
                    .unwrap_or(end);
                sink.push(i, e, comment_tag(&src[i..e]));
                i = e;
            }
            b'/' if prev == Prev::Operand => {
                // Regular expression literal.
                let mut j = i + 1;
                let mut in_class = false;
                let mut closed = false;
                while j < end && bytes[j] != b'\n' {
                    match bytes[j] {
                        b'\\' => j += 1,
                        b'[' => in_class = true,
                        b']' => in_class = false,
                        b'/' if !in_class => {
                            closed = true;
                            break;
                        }
                        _ => {}
                    }
                    j += 1;
                }
                if closed {
                    j += 1;
                    while j < end && bytes[j].is_ascii_alphabetic() {
                        j += 1;
                    }
                    sink.push(i, j, HighlightTag::String);
                    i = j;
                } else {
                    sink.push(i, i + 1, HighlightTag::Operator);
                    i += 1;
                }
                prev = Prev::Value;
            }
            b'"' | b'\'' => {
                let e = scan_quoted(bytes, i, b, true, false).min(end);
                sink.push(i, e, HighlightTag::String);
                i = e.max(i + 1);
                prev = Prev::Value;
            }
            b'`' => {
                i = scan_template(src, i, end, sink);
                prev = Prev::Value;
            }
            b'0'..=b'9' => {
                let e = scan_number(bytes, i).min(end);
                sink.push(i, e, HighlightTag::Number);
                i = e;
                prev = Prev::Value;
            }
            b'.' if i + 1 < end && bytes[i + 1].is_ascii_digit() => {
                let e = scan_number(bytes, i + 1).min(end);
                sink.push(i, e, HighlightTag::Number);
                i = e;
                prev = Prev::Value;
            }
            b'@' if i + 1 < end && is_ident_start(bytes[i + 1]) => {
                let e = scan_ident(bytes, i + 1).min(end);
                sink.push(i, e, HighlightTag::Decorator);
                i = e;
                prev = Prev::Operand;
            }
            b'#' if i + 1 < end && is_ident_start(bytes[i + 1]) => {
                // Private class member.
                let e = scan_ident(bytes, i + 1).min(end);
                sink.push(i, e, HighlightTag::Parameter);
                i = e;
                prev = Prev::Value;
            }
            b if is_ident_start(b) => {
                let e = scan_ident(bytes, i).min(end);
                let word = &src[i..e];
                let mut k = e;
                while k < end && (bytes[k] == b' ' || bytes[k] == b'\t') {
                    k += 1;
                }
                let next = bytes.get(k).copied().filter(|_| k < end);
                let tag = if prev == Prev::Dot {
                    if next == Some(b'(') {
                        Some(HighlightTag::FunctionCall)
                    } else {
                        None
                    }
                } else if prev == Prev::Function {
                    Some(HighlightTag::FunctionDefinition)
                } else if prev == Prev::Class {
                    Some(HighlightTag::ClassDefinition)
                } else if word == "this" || word == "super" {
                    Some(HighlightTag::SelfParameter)
                } else if keyword_set().contains(word) {
                    Some(HighlightTag::Keyword)
                } else if TYPES.contains(&word) || builtin_set().contains(word) {
                    Some(HighlightTag::Builtin)
                } else if next == Some(b'(') {
                    Some(HighlightTag::FunctionCall)
                } else {
                    None
                };
                if let Some(t) = tag {
                    sink.push(i, e, t);
                }
                prev = match word {
                    "function" => Prev::Function,
                    "class" => Prev::Class,
                    w if keyword_set().contains(w) && is_regex_prefix_keyword(w) => Prev::Operand,
                    _ => Prev::Value,
                };
                i = e;
            }
            b'(' | b')' | b'[' | b']' | b'{' | b'}' => {
                sink.push(
                    i,
                    i + 1,
                    bracket_tag(b).unwrap_or(HighlightTag::Punctuation),
                );
                prev = if matches!(b, b')' | b']' | b'}') {
                    Prev::Value
                } else {
                    Prev::Operand
                };
                i += 1;
            }
            b',' => {
                sink.push(i, i + 1, HighlightTag::Comma);
                prev = Prev::Operand;
                i += 1;
            }
            b';' => {
                sink.push(i, i + 1, HighlightTag::Punctuation);
                prev = Prev::Operand;
                i += 1;
            }
            b'.' => {
                sink.push(i, i + 1, HighlightTag::Dot);
                prev = Prev::Dot;
                i += 1;
            }
            b'=' | b'+' | b'-' | b'*' | b'%' | b'<' | b'>' | b'!' | b'&' | b'|' | b'^' | b'~'
            | b'?' | b':' | b'/' => {
                let mut j = i + 1;
                while j < end
                    && matches!(
                        bytes[j],
                        b'=' | b'+' | b'-' | b'*' | b'>' | b'<' | b'&' | b'|' | b'?' | b'.'
                    )
                    && j - i < 3
                {
                    j += 1;
                }
                sink.push(i, j, HighlightTag::Operator);
                prev = Prev::Operand;
                i = j;
            }
            _ => i += 1,
        }
    }
}

/// Scans a template literal starting at `i` (the backtick); returns the index after it.
fn scan_template(src: &str, i: usize, end: usize, sink: &mut Sink) -> usize {
    let bytes = src.as_bytes();
    let mut seg = i;
    let mut j = i + 1;
    while j < end {
        match bytes[j] {
            b'\\' => j += 2,
            b'`' => {
                sink.push(seg, j + 1, HighlightTag::FString);
                return j + 1;
            }
            b'$' if j + 1 < end && bytes[j + 1] == b'{' => {
                sink.push(seg, j, HighlightTag::FString);
                sink.push(j, j + 2, HighlightTag::FStringBrace);
                // Find the matching `}` (brace depth, skipping nested strings/templates roughly).
                let mut depth = 1;
                let mut k = j + 2;
                while k < end && depth > 0 {
                    match bytes[k] {
                        b'{' => depth += 1,
                        b'}' => depth -= 1,
                        b'"' | b'\'' => {
                            k = scan_quoted(bytes, k, bytes[k], true, false).min(end);
                            continue;
                        }
                        b'`' => {
                            k = scan_template(src, k, end, sink);
                            continue;
                        }
                        _ => {}
                    }
                    if depth > 0 {
                        k += 1;
                    }
                }
                let expr_end = k.min(end);
                scan_js(src, j + 2, expr_end, sink);
                if k < end {
                    sink.push(k, k + 1, HighlightTag::FStringBrace);
                    j = k + 1;
                    seg = j;
                } else {
                    return end;
                }
            }
            _ => j += 1,
        }
    }
    sink.push(seg, end, HighlightTag::FString);
    end
}

pub fn highlight_javascript(src: &str) -> Vec<Span> {
    let mut sink = Sink::default();
    scan_js(src, 0, src.len(), &mut sink);
    finish(src, sink.spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::testing::*;

    const SAMPLE: &str = "// line\n/* block\n */\nimport { x } from './x.js';\n@dec\nclass Foo extends Bar {\n  #secret = 1;\n  constructor(a) { super(); this.a = a; }\n}\nfunction greet(name = 'w') {\n  const s = `hi ${name.toUpperCase()} and ${1 + 2}`;\n  return s.replace(/\\s+/g, ' ') / 2;\n}\nlet n = 0x1F + 1_000n + .5e3;\nconsole.log(greet(\"you\"), true, null, undefined);\nconst t: number = 1;\n";

    #[test]
    fn tags() {
        let t = tagged(SAMPLE, &highlight_javascript(SAMPLE));
        assert!(has(&t, "// line", HighlightTag::Comment));
        assert!(has(&t, "/* block\n */", HighlightTag::Comment));
        assert!(has(&t, "import", HighlightTag::Keyword));
        assert!(has(&t, "'./x.js'", HighlightTag::String));
        assert!(has(&t, "@dec", HighlightTag::Decorator));
        assert!(has(&t, "Foo", HighlightTag::ClassDefinition));
        assert!(has(&t, "#secret", HighlightTag::Parameter));
        assert!(has(&t, "this", HighlightTag::SelfParameter));
        assert!(has(&t, "greet", HighlightTag::FunctionDefinition));
        assert!(has(&t, "`hi ", HighlightTag::FString));
        assert!(has(&t, "${", HighlightTag::FStringBrace));
        assert!(has(&t, "toUpperCase", HighlightTag::FunctionCall));
        assert!(has(&t, "/\\s+/g", HighlightTag::String));
        assert!(has(&t, "0x1F", HighlightTag::Number));
        assert!(has(&t, "1_000n", HighlightTag::Number));
        assert!(has(&t, ".5e3", HighlightTag::Number));
        assert!(has(&t, "console", HighlightTag::Builtin));
        assert!(has(&t, "log", HighlightTag::FunctionCall));
        assert!(has(&t, "number", HighlightTag::Builtin));
        assert!(has(&t, "undefined", HighlightTag::Keyword));
    }

    #[test]
    fn division_is_not_regex() {
        let src = "a = b / c / d;\n";
        let t = tagged(src, &highlight_javascript(src));
        assert!(!t.iter().any(|(_, k)| *k == HighlightTag::String));
    }

    #[test]
    fn invariants_and_truncation() {
        check_all(highlight_javascript, SAMPLE);
        check_all(
            highlight_javascript,
            "const s = `open ${ {a: `nested ${x}`} ;\n'unterminated\n/* open",
        );
        check_all(highlight_javascript, "const имя = 'значение'; // коммент\n");
    }
}
