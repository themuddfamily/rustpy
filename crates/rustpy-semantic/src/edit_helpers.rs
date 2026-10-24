//! Shared syntax and text-edit operations for semantic queries, refactorings and analysis.
//!
//! This lower layer has no dependency on inspections or refactoring operations.

use rustpy_core::{TextRange, TextSize};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::rowan::{NodeOrToken, TokenAtOffset};
use rustpy_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

/// A text edit inside the refactored file.
pub type Edit = (TextRange, String);

/// Applies `edits` (descending by position) to `text`.
pub fn apply_edits(text: &str, edits: &[Edit]) -> String {
    let mut edits = edits.to_vec();
    edits.sort_by(|(a, _), (b, _)| b.start().cmp(&a.start()).then(b.end().cmp(&a.end())));
    let mut out = text.to_string();
    for (r, rep) in edits {
        let s = usize::from(r.start()).min(out.len());
        let e = usize::from(r.end()).min(out.len()).max(s);
        out.replace_range(s..e, &rep);
    }
    out
}

/// Applies `edits` like [`apply_edits`], but refuses edits that do not fit
/// `text` (out of bounds, splitting a multi-byte character, or overlapping
/// another edit) instead of panicking or splicing garbage. Use it for text
/// that may differ from the text the edits were computed on (files on disk).
pub fn try_apply_edits(text: &str, edits: &[Edit]) -> Result<String, String> {
    rustpy_syntax::printer::apply_text_edits(text, edits).map_err(|error| error.to_string())
}

// ---------------------------------------------------------------------------
// Text helpers (shared with `intentions`)
// ---------------------------------------------------------------------------

pub fn ts(n: usize) -> TextSize {
    TextSize::new(n as u32)
}

pub fn rng(s: usize, e: usize) -> TextRange {
    TextRange::new(ts(s), ts(e.max(s)))
}

pub fn line_start(text: &str, off: usize) -> usize {
    text[..off.min(text.len())]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(0)
}

/// Offset of the `\n` ending the line containing `off` (or the text length).
pub fn line_end(text: &str, off: usize) -> usize {
    let off = off.min(text.len());
    text[off..]
        .find('\n')
        .map(|i| off + i)
        .unwrap_or(text.len())
}

pub fn next_line_start(text: &str, off: usize) -> usize {
    let e = line_end(text, off);
    (e + 1).min(text.len())
}

pub fn indent_at(text: &str, off: usize) -> String {
    let ls = line_start(text, off);
    text[ls..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// Whole lines of a statement (including the trailing newline). Compound
/// statements end at the DEDENT position after trailing blank lines, which
/// are stepped back over.
pub fn stmt_lines(text: &str, range: TextRange) -> TextRange {
    let start = line_start(text, range.start().into());
    let mut end: usize = usize::from(range.end()).min(text.len());
    while end > start && text.as_bytes()[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    rng(start, next_line_start(text, end))
}

/// Lines of a statement to delete: its own lines plus the blank lines that
/// follow when the statement is preceded by a blank line (or starts the
/// file), so that the surrounding spacing is preserved.
pub fn deletion_range(text: &str, range: TextRange) -> TextRange {
    let lines = stmt_lines(text, range);
    let start: usize = lines.start().into();
    let mut end: usize = lines.end().into();
    let preceded_by_blank = start == 0
        || (start >= 2
            && text.as_bytes()[start - 1] == b'\n'
            && text.as_bytes()[start - 2] == b'\n');
    if preceded_by_blank {
        while end < text.len() && text.as_bytes()[end] == b'\n' {
            end += 1;
        }
    }
    rng(start, end)
}

/// Edits removing the statements `stmts` (all of one file) without breaking
/// the code around them: a block that loses every statement keeps a `pass`
/// (the body of `if TYPE_CHECKING:` or `try:`), a statement sharing its line
/// (`import os; import sys`, `if x: import os`) loses only itself and one
/// `;`, and a statement on lines of its own takes those lines (see
/// [`deletion_range`]).
pub fn statement_removals(text: &str, stmts: &[SyntaxNode]) -> Vec<Edit> {
    let mut out: Vec<Edit> = Vec::new();
    let mut emptied: Vec<SyntaxNode> = Vec::new();
    for stmt in stmts {
        let Some(parent) = stmt.parent() else {
            continue;
        };
        let siblings: Vec<SyntaxNode> = parent
            .children()
            .filter(|c| ast::Stmt::can_cast(c.kind()))
            .collect();
        if parent.kind() == SyntaxKind::BLOCK && siblings.iter().all(|s| stmts.contains(s)) {
            if emptied.contains(&parent) {
                continue;
            }
            emptied.push(parent);
            let (first, last) = (&siblings[0], &siblings[siblings.len() - 1]);
            let start: usize = first.text_range().start().into();
            if text[line_start(text, start)..start].trim().is_empty() {
                let ind = indent_at(text, start);
                out.push((
                    rng(
                        line_start(text, start),
                        stmt_lines(text, last.text_range()).end().into(),
                    ),
                    format!("{ind}pass\n"),
                ));
            } else {
                // `if x: import os`: the body is on the header's line.
                out.push((
                    TextRange::new(first.text_range().start(), last.text_range().end()),
                    "pass".into(),
                ));
            }
            continue;
        }
        out.push((single_statement_removal(text, stmt, stmts), String::new()));
    }
    // Neighbours on one line both going (`import a; import b`) share their `;`.
    out.sort_by_key(|(r, _)| (r.start(), r.end()));
    let mut merged: Vec<Edit> = Vec::new();
    for (r, t) in out {
        match merged.last_mut() {
            Some((last, lt)) if last.end() >= r.start() && lt.is_empty() && t.is_empty() => {
                *last = last.cover(r)
            }
            _ => merged.push((r, t)),
        }
    }
    // A line left without statements goes as a whole.
    for (r, t) in &mut merged {
        let (s, e): (usize, usize) = (r.start().into(), r.end().into());
        let ends_line = e > 0 && text.as_bytes()[e - 1] == b'\n';
        if t.is_empty()
            && !ends_line
            && text[line_start(text, s)..s].trim().is_empty()
            && text[e..line_end(text, e)].trim().is_empty()
        {
            *r = deletion_range(text, *r);
        }
    }
    merged
}

/// The range removing `stmt` when its block keeps other statements: a `;`
/// shared with a statement on the same line goes with it (preferably the
/// one towards a statement that stays), otherwise its whole lines.
fn single_statement_removal(text: &str, stmt: &SyntaxNode, removed: &[SyntaxNode]) -> TextRange {
    let (s, e): (usize, usize) = (
        stmt.text_range().start().into(),
        stmt.text_range().end().into(),
    );
    let same_line = |a: usize, b: usize| {
        text.get(a..b)
            .is_some_and(|between| !between.contains('\n') && between.contains(';'))
    };
    let next = stmt
        .next_sibling()
        .filter(|n| ast::Stmt::can_cast(n.kind()) && same_line(e, n.text_range().start().into()));
    let prev = stmt
        .prev_sibling()
        .filter(|p| ast::Stmt::can_cast(p.kind()) && same_line(p.text_range().end().into(), s));
    match (next, prev) {
        // `stmt; kept`: drop `stmt; `.
        (Some(n), _) if !removed.contains(&n) => rng(s, n.text_range().start().into()),
        // `other; stmt`: drop `; stmt`.
        (_, Some(p)) => rng(p.text_range().end().into(), e),
        // `stmt; removed` first on its line: drop `stmt; `, the next removal takes the rest.
        (Some(n), None) => rng(s, n.text_range().start().into()),
        (None, None) => deletion_range(text, stmt.text_range()),
    }
}

/// Offset right after the last non-blank line ending before `end`.
pub fn after_last_line(text: &str, end: usize) -> usize {
    let mut e = end.min(text.len());
    while e > 0 && text.as_bytes()[e - 1].is_ascii_whitespace() {
        e -= 1;
    }
    next_line_start(text, e)
}

pub fn slice(text: &str, range: TextRange) -> &str {
    let s = usize::from(range.start()).min(text.len());
    let e = usize::from(range.end()).min(text.len()).max(s);
    &text[s..e]
}

/// Collapses whitespace runs (for structural text comparison).
pub fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Re-indents `body` (a sequence of lines) from `from` to `to` indentation.
pub fn reindent(body: &str, from: &str, to: &str) -> String {
    let mut out = String::new();
    for line in body.split_inclusive('\n') {
        let content = line.trim_end_matches('\n');
        if content.trim().is_empty() {
            out.push_str(line.trim_end_matches([' ', '\t']));
            if !line.ends_with('\n') && !out.ends_with('\n') {
                // keep as-is
            }
            continue;
        }
        let stripped = content
            .strip_prefix(from)
            .unwrap_or_else(|| content.trim_start_matches([' ', '\t']));
        out.push_str(to);
        out.push_str(stripped);
        if line.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

pub fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if rustpy_core::is_ident_start(c))
        && chars.all(rustpy_core::is_ident_continue)
        && !crate::features::PYTHON_KEYWORDS.contains(&name)
}

pub fn to_snake(s: &str) -> String {
    let mut out = String::new();
    let mut prev_lower = false;
    for c in s.chars() {
        if c.is_uppercase() {
            if prev_lower && !out.is_empty() {
                out.push('_');
            }
            out.extend(c.to_lowercase());
            prev_lower = false;
        } else if c.is_alphanumeric() || c == '_' {
            out.push(c);
            prev_lower = c.is_lowercase() || c.is_ascii_digit();
        } else {
            if !out.ends_with('_') && !out.is_empty() {
                out.push('_');
            }
            prev_lower = false;
        }
    }
    out.trim_matches('_').to_string()
}

// ---------------------------------------------------------------------------
// Tree helpers
// ---------------------------------------------------------------------------

/// Non-trivia token at `offset` (right-biased).
pub fn token_at(root: &SyntaxNode, offset: TextSize) -> Option<SyntaxToken> {
    let offset = offset.min(root.text_range().end());
    match root.token_at_offset(offset) {
        TokenAtOffset::Single(t) => Some(t),
        TokenAtOffset::Between(l, r) => {
            if r.kind().is_trivia() && !l.kind().is_trivia() {
                Some(l)
            } else {
                Some(r)
            }
        }
        TokenAtOffset::None => root.last_token(),
    }
}

/// Innermost typed ancestor of the token at `offset`.
pub fn ancestor_at<N: AstNode>(root: &SyntaxNode, offset: TextSize) -> Option<N> {
    token_at(root, offset)?.parent_ancestors().find_map(N::cast)
}

/// The node with exactly this range (innermost).
pub fn node_with_range(root: &SyntaxNode, range: TextRange) -> Option<SyntaxNode> {
    let el = root.covering_element(range);
    let node = match el {
        NodeOrToken::Node(n) => n,
        NodeOrToken::Token(t) => t.parent()?,
    };
    node.ancestors().find(|n| n.text_range() == range)
}

/// Nearest statement containing `node` (including itself).
pub fn stmt_of(node: &SyntaxNode) -> Option<ast::Stmt> {
    node.ancestors().find_map(ast::Stmt::cast)
}

/// Enclosing function definition of `node` (excluding `node` itself).
pub fn enclosing_func(node: &SyntaxNode) -> Option<ast::FuncDef> {
    node.ancestors().skip(1).find_map(ast::FuncDef::cast)
}

/// Whether `node` is written to by its parent (assignment target, loop
/// variable, `del` target, walrus target, ...).
pub fn is_assignment_target(node: &SyntaxNode) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    let first = parent.children().next();
    let is_first = first.as_ref() == Some(node);
    match parent.kind() {
        SyntaxKind::ASSIGN_STMT => parent.children().last().as_ref() != Some(node),
        SyntaxKind::AUG_ASSIGN_STMT
        | SyntaxKind::ANN_ASSIGN_STMT
        | SyntaxKind::FOR_STMT
        | SyntaxKind::COMP_FOR
        | SyntaxKind::NAMED_EXPR => is_first,
        SyntaxKind::WITH_ITEM => !is_first,
        SyntaxKind::DEL_STMT => true,
        SyntaxKind::TUPLE_EXPR
        | SyntaxKind::LIST_EXPR
        | SyntaxKind::STAR_EXPR
        | SyntaxKind::PAREN_EXPR => is_assignment_target(&parent),
        _ => false,
    }
}

/// Whether an expression can be substituted into any context without parentheses.
pub fn is_atom(expr: &ast::Expr) -> bool {
    matches!(
        expr,
        ast::Expr::Name(_)
            | ast::Expr::Literal(_)
            | ast::Expr::String(_)
            | ast::Expr::FString(_)
            | ast::Expr::Paren(_)
            | ast::Expr::List(_)
            | ast::Expr::Set(_)
            | ast::Expr::Dict(_)
            | ast::Expr::ListComp(_)
            | ast::Expr::SetComp(_)
            | ast::Expr::DictComp(_)
            | ast::Expr::Attribute(_)
            | ast::Expr::Subscript(_)
            | ast::Expr::Call(_)
    ) || matches!(expr, ast::Expr::Tuple(t) if t.syntax().first_token().is_some_and(|t| t.kind() == SyntaxKind::L_PAREN))
}

/// Text of `value` substituted for `at`, parenthesized when the context requires it.
pub fn substitution_text(text: &str, value: &ast::Expr, at: &SyntaxNode) -> String {
    let src = slice(text, value.text_range()).trim().to_string();
    if is_atom(value) {
        return src;
    }
    let parent_kind = at.parent().map(|p| p.kind());
    let bare_tuple = matches!(value, ast::Expr::Tuple(_));
    let top_level = matches!(
        parent_kind,
        Some(
            SyntaxKind::ASSIGN_STMT
                | SyntaxKind::RETURN_STMT
                | SyntaxKind::EXPR_STMT
                | SyntaxKind::ANN_ASSIGN_STMT
                | SyntaxKind::AUG_ASSIGN_STMT
        )
    );
    if top_level && !is_assignment_target(at) {
        return src;
    }
    if bare_tuple {
        return format!("({src})");
    }
    let simple_ctx = matches!(
        parent_kind,
        Some(
            SyntaxKind::ARG
                | SyntaxKind::KEYWORD_ARG
                | SyntaxKind::DICT_ITEM
                | SyntaxKind::LIST_EXPR
                | SyntaxKind::SET_EXPR
                | SyntaxKind::IF_STMT
                | SyntaxKind::ELIF_CLAUSE
                | SyntaxKind::WHILE_STMT
                | SyntaxKind::ASSERT_STMT
                | SyntaxKind::WITH_ITEM
                | SyntaxKind::PAREN_EXPR
                | SyntaxKind::YIELD_EXPR
                | SyntaxKind::DEFAULT_VALUE
                | SyntaxKind::ANNOTATION
                | SyntaxKind::RETURN_ANNOTATION
                | SyntaxKind::DICT_UNPACK
        )
    ) || (parent_kind == Some(SyntaxKind::FOR_STMT) && !is_assignment_target(at))
        || (parent_kind == Some(SyntaxKind::COMP_FOR) && !is_assignment_target(at))
        || (parent_kind == Some(SyntaxKind::SUBSCRIPT_EXPR)
            && at.parent().and_then(|p| p.children().next()).as_ref() != Some(at));
    if simple_ctx
        && !matches!(
            value,
            ast::Expr::Named(_) | ast::Expr::Yield(_) | ast::Expr::YieldFrom(_)
        )
    {
        return src;
    }
    format!("({src})")
}

/// Range of a list element plus its separating comma (and whitespace).
pub fn element_with_comma(text: &str, node: &SyntaxNode) -> TextRange {
    let mut s: usize = node.text_range().start().into();
    let mut e: usize = node.text_range().end().into();
    // Following comma?
    let mut cursor = e;
    while cursor < text.len()
        && (text.as_bytes()[cursor] == b' ' || text.as_bytes()[cursor] == b'\t')
    {
        cursor += 1;
    }
    if cursor < text.len() && text.as_bytes()[cursor] == b',' {
        cursor += 1;
        while cursor < text.len()
            && (text.as_bytes()[cursor] == b' ' || text.as_bytes()[cursor] == b'\t')
        {
            cursor += 1;
        }
        e = cursor;
    } else {
        // Preceding comma.
        let mut back = s;
        while back > 0
            && (text.as_bytes()[back - 1] == b' '
                || text.as_bytes()[back - 1] == b'\t'
                || text.as_bytes()[back - 1] == b'\n')
        {
            back -= 1;
        }
        if back > 0 && text.as_bytes()[back - 1] == b',' {
            s = back - 1;
        }
    }
    rng(s, e)
}
