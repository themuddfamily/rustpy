//! Lexical Python highlighting refined by lossless syntax context.

use rustc_hash::{FxHashMap, FxHashSet};
use rustpy_core::{TextRange, TextSize};
use rustpy_syntax::ast::{self, AstNode, Expr};
use rustpy_syntax::lexer::Token;
use rustpy_syntax::SyntaxKind::*;
use rustpy_syntax::{Parse, SyntaxNode};

use crate::{HighlightTag, Span};

#[path = "python/local_roles.rs"]
mod local_roles;

static BUILTINS: &[&str] = &[
    "abs",
    "aiter",
    "all",
    "anext",
    "any",
    "ascii",
    "bin",
    "bool",
    "breakpoint",
    "bytearray",
    "bytes",
    "callable",
    "chr",
    "classmethod",
    "compile",
    "complex",
    "copyright",
    "credits",
    "delattr",
    "dict",
    "dir",
    "divmod",
    "enumerate",
    "eval",
    "exec",
    "exit",
    "filter",
    "float",
    "format",
    "frozenset",
    "getattr",
    "globals",
    "hasattr",
    "hash",
    "help",
    "hex",
    "id",
    "input",
    "int",
    "isinstance",
    "issubclass",
    "iter",
    "len",
    "license",
    "list",
    "locals",
    "map",
    "max",
    "memoryview",
    "min",
    "next",
    "object",
    "oct",
    "open",
    "ord",
    "pow",
    "print",
    "property",
    "quit",
    "range",
    "repr",
    "reversed",
    "round",
    "set",
    "setattr",
    "slice",
    "sorted",
    "staticmethod",
    "str",
    "sum",
    "super",
    "tuple",
    "type",
    "vars",
    "zip",
    "__import__",
    "__build_class__",
    "__debug__",
    "NotImplemented",
    "Ellipsis",
    "BaseException",
    "BaseExceptionGroup",
    "Exception",
    "ExceptionGroup",
    "ArithmeticError",
    "AssertionError",
    "AttributeError",
    "BlockingIOError",
    "BrokenPipeError",
    "BufferError",
    "BytesWarning",
    "ChildProcessError",
    "ConnectionAbortedError",
    "ConnectionError",
    "ConnectionRefusedError",
    "ConnectionResetError",
    "DeprecationWarning",
    "EOFError",
    "EncodingWarning",
    "EnvironmentError",
    "FileExistsError",
    "FileNotFoundError",
    "FloatingPointError",
    "FutureWarning",
    "GeneratorExit",
    "IOError",
    "ImportError",
    "ImportWarning",
    "IndentationError",
    "IndexError",
    "InterruptedError",
    "IsADirectoryError",
    "KeyError",
    "KeyboardInterrupt",
    "LookupError",
    "MemoryError",
    "ModuleNotFoundError",
    "NameError",
    "NotADirectoryError",
    "NotImplementedError",
    "OSError",
    "OverflowError",
    "PendingDeprecationWarning",
    "PermissionError",
    "ProcessLookupError",
    "PythonFinalizationError",
    "RecursionError",
    "ReferenceError",
    "ResourceWarning",
    "RuntimeError",
    "RuntimeWarning",
    "StopAsyncIteration",
    "StopIteration",
    "SyntaxError",
    "SyntaxWarning",
    "SystemError",
    "SystemExit",
    "TabError",
    "TimeoutError",
    "TypeError",
    "UnboundLocalError",
    "UnicodeDecodeError",
    "UnicodeEncodeError",
    "UnicodeError",
    "UnicodeTranslateError",
    "UnicodeWarning",
    "UserWarning",
    "ValueError",
    "Warning",
    "ZeroDivisionError",
];

fn builtins() -> &'static FxHashSet<&'static str> {
    static SET: std::sync::OnceLock<FxHashSet<&'static str>> = std::sync::OnceLock::new();
    SET.get_or_init(|| BUILTINS.iter().copied().collect())
}

/// Highlights Python source. Spans are sorted, non-overlapping and cover
/// only non-default text (whitespace and plain identifiers are omitted).
pub fn highlight_python(src: &str) -> Vec<Span> {
    let parse = rustpy_syntax::parse(src);
    highlight_python_with_parse(src, &parse)
}

/// Highlight using the editor's cached parse, avoiding a second parse. `parse`
/// must describe exactly `src`; lexical tokens inside recovery nodes are kept.
pub fn highlight_python_with_parse(src: &str, parse: &Parse) -> Vec<Span> {
    let root = parse.syntax_node();
    let tokens: Vec<_> = root
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .map(|token| Token {
            kind: token.kind(),
            range: token.text_range(),
        })
        .collect();
    let (context, shadows) = lexical_roles(&root);
    highlight_with_context(src, &tokens, &context, &shadows)
}

/// Highlight an existing lexer token stream, with syntax context from `src`.
pub fn highlight_tokens(src: &str, tokens: &[Token]) -> Vec<Span> {
    let root = rustpy_syntax::parse(src).syntax_node();
    let (context, shadows) = lexical_roles(&root);
    highlight_with_context(src, tokens, &context, &shadows)
}

fn highlight_with_context(
    src: &str,
    tokens: &[Token],
    context: &FxHashMap<TextRange, HighlightTag>,
    shadows: &FxHashSet<TextRange>,
) -> Vec<Span> {
    let mut out = Vec::with_capacity(tokens.len());
    let text = |t: &Token| &src[t.range];
    // Significant (non-trivia) token indices for context lookups.
    let sig: Vec<usize> = tokens
        .iter()
        .enumerate()
        .filter(|(_, t)| !t.kind.is_trivia())
        .map(|(i, _)| i)
        .collect();
    let mut line_start = true; // next significant token starts a logical line
                               // Mirrors the lexer's f-string modes so replacement-field braces can be
                               // told apart from dict/set braces inside the expression.
    let mut frames: Vec<Frame> = Vec::new();
    let mut sig_pos = 0usize;

    for tok in tokens {
        let kind = tok.kind;
        if kind.is_trivia() {
            if kind == COMMENT {
                let t = text(tok);
                let tag = if is_todo_comment(t) {
                    HighlightTag::TodoComment
                } else {
                    HighlightTag::Comment
                };
                out.push(Span {
                    range: tok.range,
                    tag,
                });
            }
            continue;
        }
        let prev_sig = sig_pos.checked_sub(1).map(|p| tokens[sig[p]]);
        let next_sig = sig.get(sig_pos + 1).map(|&j| tokens[j]);
        let next2_sig = sig.get(sig_pos + 2).map(|&j| tokens[j]);
        sig_pos += 1;
        let starts_line = line_start;
        if kind != NEWLINE && kind != INDENT && kind != DEDENT {
            line_start = false;
        }
        let tag = match kind {
            NEWLINE => {
                line_start = true;
                continue;
            }
            INDENT | DEDENT => continue,
            k if k.is_soft_keyword() => HighlightTag::SoftKeyword,
            k if k.is_keyword() => HighlightTag::Keyword,
            IDENT => {
                let t = text(tok);
                let after_dot = prev_sig.is_some_and(|p| p.kind == DOT);
                if let Some(tag) = context.get(&tok.range) {
                    *tag
                } else if prev_sig.is_some_and(|p| p.kind == DEF_KW) {
                    HighlightTag::FunctionDefinition
                } else if prev_sig.is_some_and(|p| p.kind == CLASS_KW) {
                    HighlightTag::ClassDefinition
                } else if starts_line
                    && is_soft_keyword_use(
                        t,
                        next_sig,
                        next2_sig,
                        line_ends_with_colon(tokens, &sig, sig_pos),
                    )
                {
                    HighlightTag::SoftKeyword
                } else if !after_dot && (t == "self" || t == "cls") {
                    HighlightTag::SelfParameter
                } else if t.len() > 4 && t.starts_with("__") && t.ends_with("__") {
                    HighlightTag::PredefinedUsage
                } else if !after_dot && !shadows.contains(&tok.range) && builtins().contains(t) {
                    HighlightTag::Builtin
                } else if next_sig.is_some_and(|n| n.kind == L_PAREN) {
                    if after_dot {
                        HighlightTag::MethodCall
                    } else {
                        HighlightTag::FunctionCall
                    }
                } else {
                    continue;
                }
            }
            INT_NUMBER | FLOAT_NUMBER | COMPLEX_NUMBER => HighlightTag::Number,
            STRING => {
                let t = text(tok);
                let is_bytes = t
                    .bytes()
                    .take_while(|b| b.is_ascii_alphabetic())
                    .any(|b| b == b'b' || b == b'B');
                if is_bytes {
                    push_string_with_escapes(
                        &mut out,
                        tok.range,
                        t,
                        is_raw_string(t),
                        HighlightTag::Bytes,
                    );
                    continue;
                } else if context.get(&tok.range) == Some(&HighlightTag::Docstring) {
                    push_docstring(&mut out, tok.range, t);
                    continue;
                } else {
                    push_string_with_escapes(
                        &mut out,
                        tok.range,
                        t,
                        is_raw_string(t),
                        HighlightTag::String,
                    );
                    continue;
                }
            }
            FSTRING_CONVERSION => HighlightTag::FStringConversion,
            FSTRING_START | TSTRING_START => {
                frames.push(Frame::FString(is_raw_string(text(tok))));
                HighlightTag::FString
            }
            FSTRING_END | TSTRING_END => {
                frames.pop();
                HighlightTag::FString
            }
            FSTRING_MIDDLE | TSTRING_MIDDLE => {
                let raw = frames
                    .iter()
                    .rev()
                    .find_map(|frame| match frame {
                        Frame::FString(raw) => Some(*raw),
                        _ => None,
                    })
                    .unwrap_or(false);
                if matches!(frames.last(), Some(Frame::FormatSpec)) {
                    let base = usize::from(tok.range.start());
                    for (offset, character) in text(tok).char_indices() {
                        let tag = if character.is_ascii_digit() {
                            HighlightTag::FStringFormatNumber
                        } else if "<>=^+- #0,.%_".contains(character) {
                            HighlightTag::FStringFormatCharacter
                        } else {
                            HighlightTag::String
                        };
                        out.push(Span {
                            range: TextRange::new(
                                TextSize::new((base + offset) as u32),
                                TextSize::new((base + offset + character.len_utf8()) as u32),
                            ),
                            tag,
                        });
                    }
                } else {
                    push_string_with_escapes(
                        &mut out,
                        tok.range,
                        text(tok),
                        raw,
                        HighlightTag::FString,
                    );
                }
                continue;
            }
            L_BRACE if matches!(frames.last(), Some(Frame::FString(_) | Frame::FormatSpec)) => {
                frames.push(Frame::Expr(0));
                HighlightTag::FStringBrace
            }
            R_BRACE if matches!(frames.last(), Some(Frame::Expr(0) | Frame::FormatSpec)) => {
                frames.pop();
                HighlightTag::FStringBrace
            }
            L_BRACE if matches!(frames.last(), Some(Frame::Expr(_))) => {
                if let Some(Frame::Expr(d)) = frames.last_mut() {
                    *d += 1;
                }
                HighlightTag::Brace
            }
            R_BRACE if matches!(frames.last(), Some(Frame::Expr(_))) => {
                if let Some(Frame::Expr(d)) = frames.last_mut() {
                    *d -= 1;
                }
                HighlightTag::Brace
            }
            AT if context.get(&tok.range) == Some(&HighlightTag::Decorator) => {
                HighlightTag::Decorator
            }
            L_PAREN => {
                if let Some(Frame::Expr(depth)) = frames.last_mut() {
                    *depth += 1;
                }
                HighlightTag::Parenthesis
            }
            L_BRACK => {
                if let Some(Frame::Expr(depth)) = frames.last_mut() {
                    *depth += 1;
                }
                HighlightTag::Bracket
            }
            L_BRACE => HighlightTag::Brace,
            R_PAREN => {
                if let Some(Frame::Expr(depth)) = frames.last_mut() {
                    *depth = depth.saturating_sub(1);
                }
                HighlightTag::Parenthesis
            }
            R_BRACK => {
                if let Some(Frame::Expr(depth)) = frames.last_mut() {
                    *depth = depth.saturating_sub(1);
                }
                HighlightTag::Bracket
            }
            R_BRACE => HighlightTag::Brace,
            COMMA => HighlightTag::Comma,
            DOT if context.get(&tok.range) == Some(&HighlightTag::Decorator) => {
                HighlightTag::Decorator
            }
            DOT => HighlightTag::Dot,
            ERROR_TOKEN => HighlightTag::Error,
            COLON if matches!(frames.last(), Some(Frame::Expr(0))) => {
                frames.pop();
                frames.push(Frame::FormatSpec);
                HighlightTag::FStringColon
            }
            COLON | SEMICOLON => HighlightTag::Punctuation,
            _ => HighlightTag::Operator,
        };
        out.push(Span {
            range: tok.range,
            tag,
        });
    }
    out
}

fn lexical_roles(root: &SyntaxNode) -> (FxHashMap<TextRange, HighlightTag>, FxHashSet<TextRange>) {
    let mut context = context_tags(root);
    let (locals, shadows) = local_roles::roles(root);
    for range in locals {
        context.entry(range).or_insert(HighlightTag::LocalVariable);
    }
    (context, shadows)
}

/// Context only recolors exact lexical tokens, never punctuation/literals in an
/// annotation expression or call arguments inside a decorator.
fn context_tags(root: &SyntaxNode) -> FxHashMap<TextRange, HighlightTag> {
    let mut tags = FxHashMap::default();
    for node in root.descendants() {
        match node.kind() {
            PARAM | STAR_PARAM | STAR_STAR_PARAM => {
                if let Some(name) = node
                    .children_with_tokens()
                    .filter_map(|element| element.into_token())
                    .find(|token| token.kind() == IDENT)
                {
                    let tag = if matches!(name.text(), "self" | "cls") {
                        HighlightTag::SelfParameter
                    } else {
                        HighlightTag::Parameter
                    };
                    tags.insert(name.text_range(), tag);
                }
            }
            TYPE_PARAM | PARAM_SPEC_PARAM | TYPE_VAR_TUPLE_PARAM => {
                if let Some(name) = node
                    .children_with_tokens()
                    .filter_map(|element| element.into_token())
                    .find(|token| token.kind() == IDENT)
                {
                    tags.insert(name.text_range(), HighlightTag::TypeParameter);
                }
            }
            ANNOTATION | RETURN_ANNOTATION => {
                for token in node
                    .descendants_with_tokens()
                    .filter_map(|element| element.into_token())
                {
                    if token.kind() == IDENT {
                        tags.insert(token.text_range(), HighlightTag::TypeAnnotation);
                    }
                }
            }
            KEYWORD_ARG => {
                if let Some(name) = ast::KeywordArg::cast(node.clone()).and_then(|arg| arg.name()) {
                    tags.insert(name.text_range(), HighlightTag::KeywordArgument);
                }
            }
            DECORATOR => {
                for token in node
                    .children_with_tokens()
                    .filter_map(|element| element.into_token())
                {
                    if token.kind() == AT {
                        tags.insert(token.text_range(), HighlightTag::Decorator);
                    }
                }
                if let Some(expr) =
                    ast::Decorator::cast(node.clone()).and_then(|decorator| decorator.expr())
                {
                    decorator_name(expr, &mut tags);
                }
            }
            MODULE => {
                if let Some(module) = ast::Module::cast(node.clone()) {
                    docstring(module.statements().next(), &mut tags);
                }
            }
            FUNC_DEF => {
                if let Some(function) = ast::FuncDef::cast(node.clone()) {
                    if let Some(name) = function.name() {
                        let text = name.text();
                        let tag =
                            if text.len() > 4 && text.starts_with("__") && text.ends_with("__") {
                                HighlightTag::PredefinedDefinition
                            } else if node
                                .ancestors()
                                .skip(1)
                                .any(|ancestor| ancestor.kind() == FUNC_DEF)
                            {
                                HighlightTag::NestedFunctionDefinition
                            } else {
                                HighlightTag::FunctionDefinition
                            };
                        tags.insert(name.text_range(), tag);
                    }
                }
                if let Some(body) =
                    ast::FuncDef::cast(node.clone()).and_then(|function| function.body())
                {
                    docstring(body.statements().next(), &mut tags);
                }
            }
            CLASS_DEF => {
                if let Some(body) = ast::ClassDef::cast(node.clone()).and_then(|class| class.body())
                {
                    docstring(body.statements().next(), &mut tags);
                }
            }
            _ => {}
        }
    }
    tags
}

fn decorator_name(expr: Expr, tags: &mut FxHashMap<TextRange, HighlightTag>) {
    match expr.unparen() {
        Expr::Call(call) => {
            if let Some(callee) = call.callee() {
                decorator_name(callee, tags);
            }
        }
        Expr::Name(name) => {
            if let Some(token) = name.name_token() {
                tags.insert(token.text_range(), HighlightTag::Decorator);
            }
        }
        Expr::Attribute(attribute) => {
            // Only the dotted callee chain, not a factory call's arguments.
            if let Some(value) = attribute.object() {
                decorator_name(value, tags);
            }
            for token in attribute
                .syntax()
                .children_with_tokens()
                .filter_map(|element| element.into_token())
            {
                if matches!(token.kind(), IDENT | DOT) {
                    tags.insert(token.text_range(), HighlightTag::Decorator);
                }
            }
        }
        _ => {}
    }
}

fn docstring(statement: Option<ast::Stmt>, tags: &mut FxHashMap<TextRange, HighlightTag>) {
    let Some(ast::Stmt::Expr(statement)) = statement else {
        return;
    };
    let Some(expr) = statement.expr().map(Expr::unparen) else {
        return;
    };
    if !matches!(expr, Expr::String(_) | Expr::ConcatString(_)) {
        return;
    }
    let tokens: Vec<_> = expr
        .syntax()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .collect();
    // Bytes and f-strings are expressions, never Python docstrings, including
    // malformed mixed concatenations still represented by a recovery tree.
    if tokens.iter().any(|token| {
        matches!(token.kind(), FSTRING_START | TSTRING_START)
            || (token.kind() == STRING
                && token
                    .text()
                    .bytes()
                    .take_while(u8::is_ascii_alphabetic)
                    .any(|b| matches!(b, b'b' | b'B')))
    }) {
        return;
    }
    for token in tokens {
        if token.kind() == STRING {
            tags.insert(token.text_range(), HighlightTag::Docstring);
        }
    }
}

#[derive(Copy, Clone, Debug)]
enum Frame {
    /// Inside the literal part of an f/t-string.
    FString(bool),
    /// Literal format text; braces begin nested replacement fields.
    FormatSpec,
    /// Inside a replacement field; counts nested expression delimiters.
    Expr(usize),
}

/// Does the logical line containing significant token `sig_pos` end with `:`?
fn line_ends_with_colon(tokens: &[Token], sig: &[usize], sig_pos: usize) -> bool {
    let mut last = None;
    for &j in &sig[sig_pos..] {
        let k = tokens[j].kind;
        if k == NEWLINE {
            break;
        }
        last = Some(k);
    }
    last == Some(COLON)
}

fn is_todo_comment(text: &str) -> bool {
    // As a word only: `debug`, `Mastodon` or `HackerNews` are not TODOs.
    crate::common::is_todo_comment(text)
}

fn is_soft_keyword_use(
    t: &str,
    next: Option<Token>,
    next2: Option<Token>,
    ends_with_colon: bool,
) -> bool {
    match t {
        "match" | "case" => {
            ends_with_colon
                && next.is_some_and(|n| {
                    !matches!(
                        n.kind,
                        EQ | DOT | NEWLINE | COMMA | COLON | PLUS_EQ | MINUS_EQ
                    )
                })
        }
        "type" => {
            next.is_some_and(|n| n.kind == IDENT)
                && next2.is_some_and(|n| matches!(n.kind, EQ | L_BRACK))
        }
        _ => false,
    }
}

fn is_raw_string(text: &str) -> bool {
    text.bytes()
        .take_while(u8::is_ascii_alphabetic)
        .any(|b| matches!(b, b'r' | b'R'))
}

fn push_string_with_escapes(
    out: &mut Vec<Span>,
    range: TextRange,
    text: &str,
    raw: bool,
    base_tag: HighlightTag,
) {
    let base = u32::from(range.start());
    if raw || !text.contains('\\') {
        out.push(Span {
            range,
            tag: base_tag,
        });
        return;
    }
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut seg_start = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let esc_len = match bytes[i + 1] {
                b'x' => 4,
                b'u' => 6,
                b'U' => 10,
                b'N' => bytes[i + 1..]
                    .iter()
                    .position(|&b| b == b'}')
                    .map(|p| p + 2)
                    .unwrap_or(2),
                b'0'..=b'7' => {
                    let mut n = 2;
                    while n < 4 && i + n < bytes.len() && (b'0'..=b'7').contains(&bytes[i + n]) {
                        n += 1;
                    }
                    n
                }
                _ => 2,
            }
            .min(bytes.len() - i);
            // Incomplete or unknown escapes can contain non-ASCII text. Never
            // split that scalar into separately colored, invalid UTF-8 slices.
            let mut esc_len = esc_len;
            while !text.is_char_boundary(i + esc_len) {
                esc_len += 1;
            }
            if seg_start < i {
                out.push(Span {
                    range: TextRange::new(
                        TextSize::new(base + seg_start as u32),
                        TextSize::new(base + i as u32),
                    ),
                    tag: base_tag,
                });
            }
            out.push(Span {
                range: TextRange::new(
                    TextSize::new(base + i as u32),
                    TextSize::new(base + (i + esc_len) as u32),
                ),
                tag: if valid_escape(&text[i..i + esc_len])
                    && !(base_tag == HighlightTag::Bytes
                        && matches!(bytes[i + 1], b'u' | b'U' | b'N'))
                {
                    HighlightTag::EscapeSequence
                } else {
                    HighlightTag::InvalidEscapeSequence
                },
            });
            i += esc_len;
            seg_start = i;
        } else {
            i += 1;
        }
    }
    if seg_start < bytes.len() {
        out.push(Span {
            range: TextRange::new(
                TextSize::new(base + seg_start as u32),
                TextSize::new(base + bytes.len() as u32),
            ),
            tag: base_tag,
        });
    }
}

/// Convenience for tests and tools: the tag covering `offset`, if any.
pub fn tag_at(spans: &[Span], offset: TextSize) -> Option<HighlightTag> {
    spans
        .iter()
        .find(|s| s.range.contains(offset))
        .map(|s| s.tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(src: &str) -> Vec<(String, HighlightTag)> {
        highlight_python(src)
            .into_iter()
            .map(|s| (src[s.range].to_string(), s.tag))
            .collect()
    }

    #[test]
    fn basic_tags() {
        let t =
            tags("@dec.o\ndef f(self, x=1):\n    \"\"\"doc\"\"\"\n    return len(x)  # TODO x\n");
        assert!(t.contains(&("@".into(), HighlightTag::Decorator)));
        assert!(t.contains(&("dec".into(), HighlightTag::Decorator)));
        assert!(t.contains(&("f".into(), HighlightTag::FunctionDefinition)));
        assert!(t.contains(&("self".into(), HighlightTag::SelfParameter)));
        assert!(t.contains(&("\"\"\"doc\"\"\"".into(), HighlightTag::Docstring)));
        assert!(t.contains(&("len".into(), HighlightTag::Builtin)));
        assert!(t.contains(&("# TODO x".into(), HighlightTag::TodoComment)));
    }

    #[test]
    fn keyword_args_and_calls() {
        let t = tags("g(a, key=1)\nkey = 2\n");
        assert!(t.contains(&("g".into(), HighlightTag::FunctionCall)));
        assert!(t.contains(&("key".into(), HighlightTag::KeywordArgument)));
        assert_eq!(t.iter().filter(|(s, _)| s == "key").count(), 1);
    }

    #[test]
    fn fstring_and_escapes() {
        let t = tags("f'a{x}\\n'\n");
        assert!(t.contains(&("f'".into(), HighlightTag::FString)));
        assert!(t.contains(&("{".into(), HighlightTag::FStringBrace)));
        assert!(t.contains(&("\\n".into(), HighlightTag::EscapeSequence)));
        let t = tags("f'{ {1: 2}[1] }'\n");
        assert_eq!(
            t.iter()
                .filter(|(_, k)| *k == HighlightTag::FStringBrace)
                .count(),
            2
        );
        assert_eq!(
            t.iter().filter(|(_, k)| *k == HighlightTag::Brace).count(),
            2
        );
    }

    #[test]
    fn soft_keywords() {
        let t = tags("match x:\n    case 1:\n        pass\nmatch = 3\ntype X = int\n");
        assert!(t.contains(&("match".into(), HighlightTag::SoftKeyword)));
        assert!(t.contains(&("case".into(), HighlightTag::SoftKeyword)));
        assert!(t.contains(&("type".into(), HighlightTag::SoftKeyword)));
        assert_eq!(
            t.iter()
                .filter(|(s, k)| s == "match" && *k == HighlightTag::SoftKeyword)
                .count(),
            1
        );
    }
}

#[cfg(test)]
#[path = "python_tests.rs"]
mod context_tests;

fn valid_escape(text: &str) -> bool {
    let b = text.as_bytes();
    match b.get(1).copied() {
        Some(
            b'\\'
            | b'\''
            | b'"'
            | b'a'
            | b'b'
            | b'f'
            | b'n'
            | b'r'
            | b't'
            | b'v'
            | b'\n'
            | b'\r'
            | b'0'..=b'7',
        ) => true,
        Some(b'x') => b.len() == 4 && b[2..].iter().all(u8::is_ascii_hexdigit),
        Some(b'u') => b.len() == 6 && b[2..].iter().all(u8::is_ascii_hexdigit),
        Some(b'U') => {
            b.len() == 10
                && b[2..].iter().all(u8::is_ascii_hexdigit)
                && rustpy_syntax::string_literal::string(&format!("'{text}'")).is_ok()
        }
        Some(b'N') => {
            text.starts_with("\\N{")
                && text.ends_with('}')
                && text.len() > 4
                && rustpy_syntax::string_literal::string(&format!("'{text}'")).is_ok()
        }
        _ => false,
    }
}

fn push_docstring(out: &mut Vec<Span>, range: TextRange, text: &str) {
    let mut parts = Vec::new();
    push_string_with_escapes(
        &mut parts,
        range,
        text,
        is_raw_string(text),
        HighlightTag::Docstring,
    );
    let base = usize::from(range.start());
    for part in parts {
        if part.tag == HighlightTag::Docstring {
            let start = usize::from(part.range.start()) - base;
            let end = usize::from(part.range.end()) - base;
            push_docstring_tags(out, part.range, &text[start..end]);
        } else {
            out.push(part);
        }
    }
}
fn push_docstring_tags(out: &mut Vec<Span>, range: TextRange, text: &str) {
    let base = usize::from(range.start());
    let bytes = text.as_bytes();
    let mut segment = 0;
    let mut i = 0;
    while i < bytes.len() {
        let begin = i;
        if bytes[i] == b'@' || bytes[i] == b':' {
            i += 1;
            let word = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let name = &text[word..i];
            if matches!(
                name,
                "param"
                    | "parameter"
                    | "type"
                    | "return"
                    | "returns"
                    | "rtype"
                    | "raise"
                    | "raises"
                    | "var"
                    | "ivar"
                    | "cvar"
                    | "keyword"
                    | "yields"
            ) {
                if segment < begin {
                    out.push(Span {
                        range: TextRange::new(
                            TextSize::new((base + segment) as u32),
                            TextSize::new((base + begin) as u32),
                        ),
                        tag: HighlightTag::Docstring,
                    });
                }
                out.push(Span {
                    range: TextRange::new(
                        TextSize::new((base + begin) as u32),
                        TextSize::new((base + i) as u32),
                    ),
                    tag: HighlightTag::DocCommentTag,
                });
                segment = i;
            }
        } else {
            i += 1;
        }
    }
    if segment < text.len() {
        out.push(Span {
            range: TextRange::new(TextSize::new((base + segment) as u32), range.end()),
            tag: HighlightTag::Docstring,
        });
    }
}
