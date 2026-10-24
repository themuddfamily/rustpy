//! Postfix completion: `expr.if` → `if expr:` and friends.

use rustpy_core::{TextRange, TextSize};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::{SyntaxKind, SyntaxNode};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostfixTemplate {
    pub key: &'static str,
    pub description: &'static str,
    pub example: &'static str,
}

const TEMPLATES: &[PostfixTemplate] = &[
    PostfixTemplate {
        key: "if",
        description: "if expr:",
        example: "if expr:",
    },
    PostfixTemplate {
        key: "ifn",
        description: "if expr is None:",
        example: "if expr is None:",
    },
    PostfixTemplate {
        key: "ifnn",
        description: "if expr is not None:",
        example: "if expr is not None:",
    },
    PostfixTemplate {
        key: "not",
        description: "not expr",
        example: "not expr",
    },
    PostfixTemplate {
        key: "return",
        description: "return expr",
        example: "return expr",
    },
    PostfixTemplate {
        key: "print",
        description: "print(expr)",
        example: "print(expr)",
    },
    PostfixTemplate {
        key: "len",
        description: "len(expr)",
        example: "len(expr)",
    },
    PostfixTemplate {
        key: "while",
        description: "while expr:",
        example: "while expr:",
    },
    PostfixTemplate {
        key: "for",
        description: "for item in expr:",
        example: "for item in expr:",
    },
    PostfixTemplate {
        key: "isnone",
        description: "expr is None",
        example: "expr is None",
    },
    PostfixTemplate {
        key: "isnotnone",
        description: "expr is not None",
        example: "expr is not None",
    },
    PostfixTemplate {
        key: "par",
        description: "(expr)",
        example: "(expr)",
    },
    PostfixTemplate {
        key: "main",
        description: "if __name__ == \"__main__\": expr",
        example: "if __name__ == \"__main__\":\n    expr",
    },
    PostfixTemplate {
        key: "lambda",
        description: "lambda: expr",
        example: "lambda: expr",
    },
    PostfixTemplate {
        key: "list",
        description: "list(expr)",
        example: "list(expr)",
    },
    PostfixTemplate {
        key: "set",
        description: "set(expr)",
        example: "set(expr)",
    },
    PostfixTemplate {
        key: "tuple",
        description: "tuple(expr)",
        example: "tuple(expr)",
    },
    PostfixTemplate {
        key: "await",
        description: "await expr",
        example: "await expr",
    },
];

/// The bundled Python postfix templates.
pub fn postfix_templates() -> &'static [PostfixTemplate] {
    TEMPLATES
}

/// Templates whose key starts with `prefix` (empty prefix lists all).
pub fn matching_postfix(prefix: &str) -> Vec<&'static PostfixTemplate> {
    TEMPLATES
        .iter()
        .filter(|t| t.key.starts_with(prefix))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostfixResult {
    /// Range to replace: from the start of the expression to the end of `.key`.
    pub edit_range: TextRange,
    pub text: String,
    /// Caret byte offset inside `text` after the edit.
    pub caret: usize,
}

/// Length in bytes of the identifier characters typed after the dot.
fn typed_key_len(source: &str, dot_offset: usize) -> usize {
    source[dot_offset + 1..]
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .map(char::len_utf8)
        .sum()
}

/// Finds the longest expression that ends right before the dot at
/// `dot_offset` in `source`; the `.key` suffix starting there is ignored.
/// Returns `(start, end, kind)` in byte offsets of `source`.
pub fn expression_before_dot(
    source: &str,
    dot_offset: usize,
) -> Option<(usize, usize, SyntaxKind)> {
    if dot_offset >= source.len()
        || !source.is_char_boundary(dot_offset)
        || source.as_bytes()[dot_offset] != b'.'
    {
        return None;
    }
    // Drop the `.key` suffix so the statement parses cleanly.
    let key_len = typed_key_len(source, dot_offset);
    let cleaned = format!(
        "{}{}",
        &source[..dot_offset],
        &source[dot_offset + 1 + key_len..]
    );
    if let Some(found) = longest_expression_ending_at(&cleaned, dot_offset) {
        return Some(found);
    }
    // The surrounding file may not parse (e.g. an unexpected indent while
    // typing); retry with the current line alone, dedented.
    let line_start = cleaned[..dot_offset].rfind('\n').map_or(0, |i| i + 1);
    let line_end = cleaned[dot_offset..]
        .find('\n')
        .map_or(cleaned.len(), |i| dot_offset + i);
    let line = &cleaned[line_start..line_end];
    let lead = line.len() - line.trim_start().len();
    let base = line_start + lead;
    if dot_offset < base {
        return None;
    }
    longest_expression_ending_at(&line[lead..], dot_offset - base)
        .map(|(s, e, k)| (s + base, e + base, k))
}

/// Parses `text` and returns the outermost expression whose range ends
/// exactly at byte offset `at`.
fn longest_expression_ending_at(text: &str, at: usize) -> Option<(usize, usize, SyntaxKind)> {
    let parse = rustpy_syntax::parse(text);
    let root = parse.syntax_node();
    let end = TextSize::new(at as u32);
    let tok = root.token_at_offset(end).left_biased()?;
    if tok.kind().is_trivia() || tok.text_range().end() != end {
        return None;
    }
    let mut best: Option<SyntaxNode> = None;
    for anc in tok.parent_ancestors() {
        if anc.text_range().end() != end {
            break;
        }
        if ast::Expr::can_cast(anc.kind()) {
            best = Some(anc);
        } else if best.is_some() {
            // Left the expression tree (statement, block, module …).
            break;
        }
    }
    let node = best?;
    let start: usize = node.text_range().start().into();
    Some((start, at, node.kind()))
}

/// Expressions that must be parenthesized before `is None` / `await`.
fn needs_parens(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::BINARY_EXPR
            | SyntaxKind::BOOL_OP_EXPR
            | SyntaxKind::COMPARE_EXPR
            | SyntaxKind::NOT_EXPR
            | SyntaxKind::CONDITIONAL_EXPR
            | SyntaxKind::LAMBDA_EXPR
            | SyntaxKind::NAMED_EXPR
            | SyntaxKind::UNARY_EXPR
            | SyntaxKind::AWAIT_EXPR
            | SyntaxKind::YIELD_EXPR
            | SyntaxKind::YIELD_FROM_EXPR
            | SyntaxKind::STAR_EXPR
    )
}

/// `not` binds looser than comparisons and arithmetic but tighter than
/// `and` / `or` / conditional expressions.
fn needs_parens_for_not(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::BOOL_OP_EXPR
            | SyntaxKind::CONDITIONAL_EXPR
            | SyntaxKind::LAMBDA_EXPR
            | SyntaxKind::NAMED_EXPR
            | SyntaxKind::STAR_EXPR
            | SyntaxKind::YIELD_EXPR
            | SyntaxKind::YIELD_FROM_EXPR
    )
}

fn wrap(expr: &str, parens: bool) -> String {
    if parens {
        format!("({expr})")
    } else {
        expr.to_string()
    }
}

fn block(header: String, body_indent: &str) -> (String, usize) {
    let t = format!("{header}\n{body_indent}");
    let c = t.len();
    (t, c)
}

fn at_end(t: String) -> (String, usize) {
    let c = t.len();
    (t, c)
}

/// Applies the postfix template `key` to the `expr.key` ending at
/// `dot_offset`. `indent` is the indentation of the current line (block
/// bodies get `indent` plus four spaces).
pub fn apply_postfix(
    source: &str,
    dot_offset: usize,
    key: &str,
    indent: &str,
) -> Option<PostfixResult> {
    let (start, end, kind) = expression_before_dot(source, dot_offset)?;
    let edit_end = dot_offset + 1 + typed_key_len(source, dot_offset);
    let expr = source[start..end].trim().to_string();
    let body_indent = format!("{indent}    ");
    let (text, caret) = match key {
        "if" => block(format!("if {expr}:"), &body_indent),
        "ifn" => block(format!("if {expr} is None:"), &body_indent),
        "ifnn" => block(format!("if {expr} is not None:"), &body_indent),
        "while" => block(format!("while {expr}:"), &body_indent),
        "for" => block(format!("for item in {expr}:"), &body_indent),
        "main" => at_end(format!("if __name__ == \"__main__\":\n{body_indent}{expr}")),
        "not" => at_end(format!("not {}", wrap(&expr, needs_parens_for_not(kind)))),
        "return" => at_end(format!("return {expr}")),
        "print" | "len" | "list" | "set" | "tuple" => at_end(format!("{key}({expr})")),
        "isnone" => at_end(format!("{} is None", wrap(&expr, needs_parens(kind)))),
        "isnotnone" => at_end(format!("{} is not None", wrap(&expr, needs_parens(kind)))),
        "par" => at_end(format!("({expr})")),
        "lambda" => at_end(format!("lambda: {expr}")),
        "await" => at_end(format!("await {}", wrap(&expr, needs_parens(kind)))),
        _ => return None,
    };
    Some(PostfixResult {
        edit_range: TextRange::new(TextSize::new(start as u32), TextSize::new(edit_end as u32)),
        text,
        caret,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Applies `key` at the last `.key` occurrence and returns the edited
    /// source plus the absolute caret offset.
    fn apply(src: &str, key: &str) -> (String, usize) {
        let dot = src.rfind(&format!(".{key}")).expect("dot");
        let r = apply_postfix(src, dot, key, "").expect("postfix applies");
        let mut out = src.to_string();
        let start = usize::from(r.edit_range.start());
        out.replace_range(start..usize::from(r.edit_range.end()), &r.text);
        (out, start + r.caret)
    }

    #[test]
    fn names_attributes_calls_and_literals() {
        assert_eq!(apply("x.if", "if").0, "if x:\n    ");
        assert_eq!(
            apply("obj.attr.value.return", "return").0,
            "return obj.attr.value"
        );
        assert_eq!(apply("f(a, b)[0].len", "len").0, "len(f(a, b)[0])");
        assert_eq!(apply("'text'.print", "print").0, "print('text')");
        assert_eq!(apply("42.par", "par").0, "(42)");
        assert_eq!(apply("[1, 2].for", "for").0, "for item in [1, 2]:\n    ");
        assert_eq!(apply("items.ifn", "ifn").0, "if items is None:\n    ");
        assert_eq!(apply("items.ifnn", "ifnn").0, "if items is not None:\n    ");
        assert_eq!(apply("value.isnone", "isnone").0, "value is None");
        assert_eq!(apply("value.isnotnone", "isnotnone").0, "value is not None");
        assert_eq!(apply("coro().await", "await").0, "await coro()");
        assert_eq!(apply("gen.list", "list").0, "list(gen)");
        assert_eq!(apply("gen.set", "set").0, "set(gen)");
        assert_eq!(apply("gen.tuple", "tuple").0, "tuple(gen)");
        assert_eq!(apply("expr.lambda", "lambda").0, "lambda: expr");
        assert_eq!(
            apply("run().main", "main").0,
            "if __name__ == \"__main__\":\n    run()"
        );
        assert_eq!(apply("flag.while", "while").0, "while flag:\n    ");
    }

    #[test]
    fn binary_and_parenthesized_expressions() {
        assert_eq!(apply("a + b.not", "not").0, "not a + b");
        assert_eq!(apply("a and b.not", "not").0, "not (a and b)");
        assert_eq!(apply("a or b.isnone", "isnone").0, "(a or b) is None");
        assert_eq!(
            apply("a == b.isnotnone", "isnotnone").0,
            "(a == b) is not None"
        );
        assert_eq!(apply("(a + b).not", "not").0, "not (a + b)");
        assert_eq!(apply("x = a + b.par", "par").0, "x = (a + b)");
        assert_eq!(apply("a + b.if", "if").0, "if a + b:\n    ");
        assert_eq!(apply("a + b.return", "return").0, "return a + b");
        assert_eq!(apply("a + b.await", "await").0, "await (a + b)");
    }

    #[test]
    fn partial_keys_indent_and_caret() {
        let src = "    value.wh";
        let dot = src.find('.').unwrap();
        let r = apply_postfix(src, dot, "while", "    ").unwrap();
        assert_eq!(r.text, "while value:\n        ");
        assert_eq!(
            r.edit_range,
            TextRange::new(TextSize::new(4), TextSize::new(src.len() as u32))
        );
        assert_eq!(r.caret, r.text.len());
        // Offsets inside a larger file.
        let src = "def f():\n    total = 0\n    total.return\n";
        let dot = src.rfind(".return").unwrap();
        let r = apply_postfix(src, dot, "return", "    ").unwrap();
        assert_eq!(
            usize::from(r.edit_range.start()),
            src.rfind("total.return").unwrap()
        );
        assert_eq!(usize::from(r.edit_range.end()), src.len() - 1);
        assert_eq!(r.text, "return total");
        let (edited, caret) = apply("x = 1\ny.if", "if");
        assert_eq!(edited, "x = 1\nif y:\n    ");
        assert_eq!(caret, edited.len());
        // Caret for inline templates sits after the generated expression.
        let (edited, caret) = apply("z = x.len + 1", "len");
        assert_eq!(edited, "z = len(x) + 1");
        assert_eq!(caret, "z = len(x)".len());
    }

    #[test]
    fn rejects_invalid_positions_and_keys() {
        assert!(apply_postfix("x.if", 5, "if", "").is_none());
        assert!(apply_postfix("x.if", 0, "if", "").is_none());
        assert!(apply_postfix("x.if", 1, "nope", "").is_none());
        assert!(apply_postfix(". if", 0, "if", "").is_none());
        assert_eq!(
            expression_before_dot("a.b.c", 3),
            Some((0, 3, SyntaxKind::ATTRIBUTE_EXPR))
        );
        assert_eq!(
            expression_before_dot("a.b.c", 1),
            Some((0, 1, SyntaxKind::NAME_EXPR))
        );
        assert!(matching_postfix("is").iter().any(|t| t.key == "isnone"));
        assert_eq!(postfix_templates().len(), 18);
    }
}
