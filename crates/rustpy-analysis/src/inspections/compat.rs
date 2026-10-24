//! Python version compatibility (`PyCompatibility`): syntax newer than the
//! configured interpreter version, and Python 2 leftovers.

use std::sync::Arc;

use rustpy_core::{Severity, TextRange};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::{SyntaxKind, SyntaxNode};

use super::{find_inline_comment, line_end_incl, QuickFix, Sink};
use rustpy_semantic::db::FileInfo;

#[path = "compat_context.rs"]
mod context;
#[path = "compat_features.rs"]
mod features;

const CODE: &str = "PyCompatibility";

pub(super) fn run(file: &Arc<FileInfo>, version: (u32, u32), sink: &mut Sink) {
    context::run(file, sink);
    features::run(file, version, sink);
    let text: &str = &file.text;
    let root = file.parse.syntax_node();
    let require = |range: TextRange, needed: (u32, u32), what: &str, sink: &mut Sink| {
        if version < needed {
            sink.push(
                range,
                Severity::Warning,
                CODE,
                format!(
                    "Python version {}.{} does not support {what} (requires {}.{})",
                    version.0, version.1, needed.0, needed.1
                ),
                Vec::new(),
            );
        }
    };
    for node in root.descendants() {
        match node.kind() {
            SyntaxKind::MATCH_STMT => {
                if let Some(t) = node.first_token() {
                    require(t.text_range(), (3, 10), "the 'match' statement", sink);
                }
            }
            SyntaxKind::NAMED_EXPR => {
                let r = node
                    .children_with_tokens()
                    .filter_map(|e| e.into_token())
                    .find(|t| t.kind() == SyntaxKind::COLON_EQ)
                    .map(|t| t.text_range())
                    .unwrap_or(node.text_range());
                require(r, (3, 8), "assignment expressions (':=')", sink);
            }
            SyntaxKind::SLASH_SEPARATOR => require(
                node.text_range(),
                (3, 8),
                "positional-only parameters ('/')",
                sink,
            ),
            SyntaxKind::TYPE_ALIAS_STMT => {
                if let Some(t) = node.first_token() {
                    require(t.text_range(), (3, 12), "the 'type' alias statement", sink);
                }
            }
            SyntaxKind::TYPE_PARAM_LIST => require(
                node.text_range(),
                (3, 12),
                "type parameter syntax (PEP 695)",
                sink,
            ),
            SyntaxKind::EXCEPT_CLAUSE => {
                let toks: Vec<_> = node
                    .children_with_tokens()
                    .filter_map(|e| e.into_token())
                    .filter(|t| !t.kind().is_trivia())
                    .take(2)
                    .collect();
                if toks.len() == 2
                    && toks[0].kind() == SyntaxKind::EXCEPT_KW
                    && toks[1].kind() == SyntaxKind::STAR
                {
                    require(
                        TextRange::new(toks[0].text_range().start(), toks[1].text_range().end()),
                        (3, 11),
                        "'except*' (exception groups)",
                        sink,
                    );
                }
            }
            SyntaxKind::WITH_STMT => {
                let paren = node
                    .children_with_tokens()
                    .filter_map(|e| e.into_token())
                    .find(|t| t.kind() == SyntaxKind::L_PAREN);
                let items: Vec<_> = node
                    .children()
                    .filter(|c| c.kind() == SyntaxKind::WITH_ITEM)
                    .collect();
                let has_as = items.iter().any(|i| {
                    i.children_with_tokens()
                        .filter_map(|e| e.into_token())
                        .any(|t| t.kind() == SyntaxKind::AS_KW)
                });
                if let Some(p) = paren {
                    if items.len() > 1 || has_as {
                        require(
                            p.text_range(),
                            (3, 9),
                            "parenthesized context managers",
                            sink,
                        );
                    }
                }
            }
            SyntaxKind::EXPR_STMT => python2_statement(text, &node, sink),
            _ => {}
        }
    }
    for tok in root
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
    {
        if tok.kind() == SyntaxKind::ERROR_TOKEN && tok.text() == "`" {
            sink.push(
                tok.text_range(),
                Severity::Error,
                CODE,
                "Python 2 backquotes are not supported; use repr()",
                Vec::new(),
            );
        }
    }
}

/// `print "x"` / `exec "code"`: a bare `print`/`exec` name followed by an
/// error node on the same line.
fn python2_statement(text: &str, node: &SyntaxNode, sink: &mut Sink) {
    let Some(stmt) = ast::ExprStmt::cast(node.clone()) else {
        return;
    };
    let Some(ast::Expr::Name(n)) = stmt.expr() else {
        return;
    };
    let name = n.name_text();
    if name != "print" && name != "exec" {
        return;
    }
    let Some(next) = node.next_sibling() else {
        return;
    };
    if next.kind() != SyntaxKind::ERROR {
        return;
    }
    let stmt_end: usize = node.text_range().end().into();
    let err_start: usize = next.text_range().start().into();
    if text[stmt_end..err_start].contains('\n') {
        return;
    }
    let start: usize = node.text_range().start().into();
    let line_end = line_end_incl(text, start);
    let line_end = if text[..line_end].ends_with('\n') {
        line_end - 1
    } else {
        line_end
    };
    let line_text = &text[start..line_end];
    let content_end = find_inline_comment(line_text)
        .map(|i| start + i)
        .unwrap_or(line_end);
    let rest = text[stmt_end..content_end].trim_end();
    let range = TextRange::new(node.text_range().start(), super::ts(stmt_end + rest.len()));
    let mut fixes = Vec::new();
    let payload = rest.trim_start();
    if name == "print"
        && !payload.starts_with(">>")
        && !payload.ends_with(',')
        && !payload.is_empty()
    {
        fixes.push(QuickFix {
            title: "Convert to print() call".into(),
            edits: vec![(
                TextRange::new(super::ts(stmt_end), range.end()),
                format!("({payload})"),
            )],
        });
    }
    sink.push(
        range,
        Severity::Error,
        CODE,
        format!("Python 2 '{name}' statement is not supported; use {name}(...)"),
        fixes,
    );
}
