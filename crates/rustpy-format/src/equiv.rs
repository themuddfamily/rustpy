//! Semantic fingerprint of a module, used to prove that formatting did not
//! change the program. It is the CST with everything a formatter may change
//! removed: whitespace, comments, line structure, redundant parentheses and
//! optional commas; strings are compared by value (docstrings with their
//! indentation normalized, as Black's own AST safety check does) and numbers
//! by their normalized spelling.
//!
//! Comments are dropped except for what the AST holds when Python parses with
//! `type_comments=True`, as Black's safety check does: the number of
//! `# type: ignore` comments and the values of the other `# type:` comments,
//! in order. Two comments that end up on one line become one comment, which
//! can hide a type comment from type checkers; Black refuses such output and
//! so does this check. Like Black, a module with a `# type:` comment where
//! Python does not accept one (it then only parses without type comments)
//! contributes no type information.

use rustpy_syntax::ast::decode_string_literal;
use rustpy_syntax::rowan::{NodeOrToken, WalkEvent};
use rustpy_syntax::SyntaxKind::{self, *};
use rustpy_syntax::{SyntaxElement, SyntaxNode, SyntaxToken};

use crate::black::{
    normalize_numeric_literal, normalize_unicode_escape_sequences, py_splitlines, py_strip,
};

/// How CPython's tokenizer reads a comment when parsing with
/// `type_comments=True`.
#[derive(Debug, PartialEq, Eq)]
enum TypeComment<'a> {
    None,
    /// `# type: ignore…`: kept in `Module.type_ignores`.
    Ignore,
    /// Any other `# type: …` comment, with its value.
    Comment(&'a str),
}

/// CPython's rule: `#`, spaces or tabs, `type:`, spaces or tabs; `ignore`
/// followed by the end or an ASCII character that is not alphanumeric makes
/// a type-ignore.
fn type_comment(text: &str) -> TypeComment<'_> {
    let Some(rest) = text.strip_prefix('#') else {
        return TypeComment::None;
    };
    let Some(rest) = rest.trim_start_matches([' ', '\t']).strip_prefix("type:") else {
        return TypeComment::None;
    };
    let value = rest.trim_start_matches([' ', '\t']);
    match value
        .strip_prefix("ignore")
        .map(|after| after.bytes().next())
    {
        Some(None) => TypeComment::Ignore,
        Some(Some(b)) if b.is_ascii() && !b.is_ascii_alphanumeric() => TypeComment::Ignore,
        _ => TypeComment::Comment(value),
    }
}

fn is_type_comment(e: &SyntaxElement) -> bool {
    e.as_token().is_some_and(|t| {
        t.kind() == COMMENT && matches!(type_comment(t.text()), TypeComment::Comment(_))
    })
}

fn prev_non_ws(e: SyntaxElement) -> Option<SyntaxElement> {
    let mut cur = e.prev_sibling_or_token();
    while cur.as_ref().is_some_and(|c| c.kind() == WHITESPACE) {
        cur = cur.and_then(|c| c.prev_sibling_or_token());
    }
    cur
}

/// The next sibling that is not whitespace, a line break inside brackets or
/// a comment.
fn next_significant(e: SyntaxElement) -> Option<SyntaxElement> {
    let mut cur = e.next_sibling_or_token();
    while cur
        .as_ref()
        .is_some_and(|c| matches!(c.kind(), WHITESPACE | NL | COMMENT))
    {
        cur = cur.and_then(|c| c.next_sibling_or_token());
    }
    cur
}

/// Whether CPython's grammar takes a `# type:` comment (other than an
/// ignore) at the position of `t`: at the end of an assignment, after the
/// colon of a `def`, `for` or `with` header, on the line opening a function
/// body, or after a function parameter followed by `,` or `)`. Anywhere else
/// parsing with `type_comments=True` fails.
fn type_comment_allowed(t: &SyntaxToken) -> bool {
    let Some(parent) = t.parent() else {
        return false;
    };
    let me = SyntaxElement::Token(t.clone());
    let prev = prev_non_ws(me.clone());
    let prev_kind = prev.as_ref().map(|e| e.kind());
    let grandparent = parent.parent().map(|p| p.kind());
    match parent.kind() {
        MODULE | BLOCK if prev_kind == Some(ASSIGN_STMT) => {
            let next = me
                .next_sibling_or_token()
                .filter(|e| e.kind() != WHITESPACE);
            matches!(next.map(|e| e.kind()), None | Some(NEWLINE))
        }
        FUNC_DEF | FOR_STMT if prev_kind == Some(COLON) => true,
        // `with (a as b):` takes no type comment; `with (a, b):` (a tuple) does.
        WITH_STMT if prev_kind == Some(COLON) => {
            !parent.children_with_tokens().any(|e| e.kind() == L_PAREN)
                || !parent.children().any(|item| {
                    item.kind() == WITH_ITEM
                        && item.children_with_tokens().any(|e| e.kind() == AS_KW)
                })
        }
        PARAM_LIST if grandparent == Some(FUNC_DEF) => {
            let is_param =
                |e: &SyntaxElement| matches!(e.kind(), PARAM | STAR_PARAM | STAR_STAR_PARAM);
            match prev {
                Some(comma) if comma.kind() == COMMA => {
                    prev_non_ws(comma).is_some_and(|p| is_param(&p))
                }
                Some(param) if is_param(&param) => {
                    next_significant(me).is_some_and(|e| e.kind() == R_PAREN)
                }
                _ => false,
            }
        }
        // `def f(a, b):` / `# type: (int, str) -> None` / body.
        BLOCK if grandparent == Some(FUNC_DEF) => {
            let def = parent.parent().expect("grandparent");
            if def.children_with_tokens().any(|e| is_type_comment(&e)) {
                return false;
            }
            let mut opened = false;
            for e in parent.children_with_tokens() {
                if e.as_token() == Some(t) {
                    return opened;
                }
                match e.kind() {
                    NEWLINE if !opened => opened = true,
                    WHITESPACE | NL => {}
                    COMMENT if !is_type_comment(&e) => {}
                    _ => return false,
                }
            }
            false
        }
        _ => false,
    }
}

/// Fingerprint of a parsed module (see the module docs).
pub fn fingerprint(root: &SyntaxNode) -> Vec<String> {
    let mut out = Vec::new();
    let mut type_ignores = 0usize;
    let mut type_comments = Vec::new();
    let mut type_comments_parse = true;
    for event in root.preorder_with_tokens() {
        match event {
            WalkEvent::Enter(NodeOrToken::Node(n)) => {
                if n.kind() == GENERATOR_EXPR && is_bare_generator_with_siblings(&n) {
                    // `f(x for x in y, z)` is not valid Python (the CST parser accepts it).
                    out.push("!unparenthesized-generator".into());
                }
                if marked(&n) {
                    out.push(format!("({:?}", n.kind()));
                }
            }
            WalkEvent::Leave(NodeOrToken::Node(n)) => {
                if marked(&n) {
                    out.push(")".into());
                }
            }
            WalkEvent::Enter(NodeOrToken::Token(t)) => {
                if t.kind() == COMMENT {
                    match type_comment(t.text()) {
                        TypeComment::None => {}
                        TypeComment::Ignore => type_ignores += 1,
                        // Black compares type comments without trailing whitespace.
                        TypeComment::Comment(v) if type_comment_allowed(&t) => {
                            type_comments.push(format!("type-comment:{}", v.trim_end()))
                        }
                        TypeComment::Comment(_) => type_comments_parse = false,
                    }
                }
                token(&t, &mut out)
            }
            WalkEvent::Leave(NodeOrToken::Token(_)) => {}
        }
    }
    if type_comments_parse {
        out.push(format!("type-ignores:{type_ignores}"));
        out.extend(type_comments);
    }
    out
}

/// A generator expression without its own parentheses that is not the sole
/// argument of a call.
fn is_bare_generator_with_siblings(node: &SyntaxNode) -> bool {
    let first = node
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| !matches!(t.kind(), WHITESPACE | COMMENT | NL));
    if first.map(|t| t.kind() == L_PAREN).unwrap_or(false) {
        return false;
    }
    let Some(parent) = node.parent() else {
        return false;
    };
    parent.kind() == ARG_LIST
        && (parent.children().count() > 1
            || parent.children_with_tokens().any(|e| e.kind() == COMMA))
}

/// Parenthesized expressions and patterns and empty class argument lists
/// are transparent.
fn marked(node: &SyntaxNode) -> bool {
    let kind = node.kind();
    if kind == PAREN_EXPR || kind == MATCH_GROUP_PATTERN {
        return false;
    }
    !(kind == ARG_LIST
        && node
            .parent()
            .map(|p| p.kind() == CLASS_DEF)
            .unwrap_or(false)
        && node.children().next().is_none())
}

fn is_string_statement(t: &SyntaxToken) -> bool {
    let Some(se) = t.parent() else { return false };
    if se.kind() != STRING_EXPR {
        return false;
    }
    let Some(stmt) = se.parent() else {
        return false;
    };
    stmt.kind() == EXPR_STMT && stmt.children().count() == 1
}

fn prefix_of(text: &str) -> String {
    let mut p: Vec<char> = text
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_lowercase())
        .filter(|c| *c != 'u')
        .collect();
    p.sort();
    p.into_iter().collect()
}

fn normalize_docstring(value: &str) -> String {
    let lines: Vec<String> = py_splitlines(value)
        .iter()
        .map(|s| py_strip(s).to_string())
        .collect();
    py_strip(&lines.join("\n")).to_string()
}

/// Decodes the escapes of an f-string literal fragment.
fn decode_fragment(text: &str) -> String {
    let decoded =
        decode_string_literal(&format!("'X{}'", normalize_unicode_escape_sequences(text)));
    decoded.strip_prefix('X').unwrap_or(&decoded).to_string()
}

/// Decode bytes directly, independently of the formatter's escape normalizer
/// and the string decoder. In particular, \u, \U and \N remain literal bytes,
/// and octal escapes wrap at 256, as they do in CPython.
fn decode_bytes_literal(text: &str) -> Option<Vec<u8>> {
    let prefix_len = text.bytes().take_while(u8::is_ascii_alphabetic).count();
    let raw = text[..prefix_len]
        .bytes()
        .any(|b| b.eq_ignore_ascii_case(&b'r'));
    let quoted = &text[prefix_len..];
    let quote = if quoted.starts_with("\"\"\"") {
        "\"\"\""
    } else if quoted.starts_with("'''") {
        "'''"
    } else if quoted.starts_with('"') {
        "\""
    } else {
        "'"
    };
    let body = quoted.strip_prefix(quote)?.strip_suffix(quote)?.as_bytes();
    if !body.is_ascii() {
        return None;
    }
    if raw {
        return Some(body.to_vec());
    }
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        let byte = body[i];
        i += 1;
        if byte != b'\\' {
            out.push(byte);
            continue;
        }
        let escape = *body.get(i)?;
        i += 1;
        match escape {
            b'\\' | b'\'' | b'"' => out.push(escape),
            b'a' => out.push(7),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'n' => out.push(10),
            b'r' => out.push(13),
            b't' => out.push(9),
            b'v' => out.push(11),
            b'\n' => {}
            b'x' => {
                let high = char::from(*body.get(i)?).to_digit(16)?;
                let low = char::from(*body.get(i + 1)?).to_digit(16)?;
                out.push((high * 16 + low) as u8);
                i += 2;
            }
            b'0'..=b'7' => {
                let mut value = u16::from(escape - b'0');
                for _ in 0..2 {
                    let Some(&digit @ b'0'..=b'7') = body.get(i) else {
                        break;
                    };
                    value = value * 8 + u16::from(digit - b'0');
                    i += 1;
                }
                out.push((value & 255) as u8);
            }
            _ => {
                out.push(b'\\');
                out.push(escape);
            }
        }
    }
    Some(out)
}

fn token(t: &SyntaxToken, out: &mut Vec<String>) {
    let kind: SyntaxKind = t.kind();
    let text = t.text();
    match kind {
        WHITESPACE | COMMENT | NL | NEWLINE | INDENT | DEDENT | SEMICOLON | L_PAREN | R_PAREN
        | COMMA | FSTRING_END | TSTRING_END => {}
        STRING => {
            let prefix = prefix_of(text);
            if prefix.contains('b') {
                out.push(match decode_bytes_literal(text) {
                    Some(value) => format!("bytes:{value:?}"),
                    None => format!("invalid-bytes:{text:?}"),
                });
                return;
            }
            let normalized = normalize_unicode_escape_sequences(text);
            let mut value = decode_string_literal(&normalized);
            if is_string_statement(t) {
                value = normalize_docstring(&value);
            }
            out.push(format!("str:{value:?}"));
        }
        FSTRING_START | TSTRING_START => out.push(format!("fstart:{}", prefix_of(text))),
        FSTRING_MIDDLE | TSTRING_MIDDLE => {
            let raw = t
                .parent()
                .and_then(|p| p.ancestors().find(|a| a.kind() == FSTRING_EXPR))
                .and_then(|f| f.first_token())
                .map(|s| prefix_of(s.text()).contains('r'))
                .unwrap_or(false);
            let value = if raw {
                text.to_string()
            } else {
                decode_fragment(text)
            };
            out.push(format!("fmid:{value:?}"));
        }
        INT_NUMBER | FLOAT_NUMBER | COMPLEX_NUMBER => out.push(normalize_numeric_literal(text)),
        _ => out.push(text.to_string()),
    }
}
