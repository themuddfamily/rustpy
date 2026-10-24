//! Code-transform intentions (roadmap §2.6): invert / split / merge `if`,
//! flip or negate comparisons, redundant parentheses, string conversions
//! (quotes, f-strings, triple quotes, split/join), `dict()` → literal,
//! comprehension ↔ loop, lambda → def, method / property / static method
//! conversions, import conversions, boolean-return simplification and
//! ternary ↔ if/else.
//!
//! Every intention returns text edits over the current file text.

use std::sync::Arc;

use rustpy_core::{TextRange, TextSize};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::{SyntaxKind, SyntaxNode};

use rustpy_semantic::db::{Analysis, FileInfo};
use rustpy_semantic::edit_helpers::{
    after_last_line, ancestor_at, deletion_range, element_with_comma, indent_at,
    is_assignment_target, is_atom, line_start, normalize, reindent, rng, slice, stmt_lines,
    token_at, Edit,
};
use rustpy_semantic::scope::BindingKind;

/// A code-transform intention: a title and the edits that apply it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Intention {
    pub title: String,
    pub edits: Vec<Edit>,
}

/// Applies `edits` to `text` (same as [`rustpy_semantic::edit_helpers::apply_edits`]).
pub fn apply_edits(text: &str, edits: &[Edit]) -> String {
    rustpy_semantic::edit_helpers::apply_edits(text, edits)
}

pub mod control_flow_actions;
pub mod definition_actions;
pub mod editor_actions;
pub mod import_actions;
pub mod string_actions;

type IntentionFn = fn(&Arc<FileInfo>, TextSize) -> Option<Intention>;

const ALL: &[IntentionFn] = &[
    invert_if,
    split_if,
    merge_ifs,
    flip_comparison,
    negate_comparison,
    remove_redundant_parens,
    convert_to_fstring,
    dict_call_to_literal,
    loop_to_comprehension,
    comprehension_to_loop,
    lambda_to_function,
    method_to_property,
    property_to_method,
    add_self,
    make_static,
    static_to_function,
    simplify_bool_return,
    ternary_to_if,
    if_to_ternary,
    from_import_to_import,
    import_to_from_import,
    move_import_to_top,
];

/// All intentions applicable at `offset`.
pub fn intentions_at(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Vec<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let mut out: Vec<_> = ALL.iter().filter_map(|f| f(file, offset)).collect();
    out.extend(import_actions::at(db, file, offset));
    out.extend(string_actions::at(db, file, offset));
    out.extend(control_flow_actions::at(db, file, offset));
    out.extend(definition_actions::at(db, file, offset));
    out.extend(editor_actions::at(db, file, offset));
    out
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn root_and_text(file: &FileInfo) -> (SyntaxNode, &str) {
    (file.parse.syntax_node(), &file.text)
}

fn expr_text(text: &str, e: &ast::Expr) -> String {
    slice(text, e.text_range()).trim().to_string()
}

/// Lines of an indented block body with its indentation; `None` for inline bodies.
fn block_lines(text: &str, block: &ast::Block) -> Option<(TextRange, String)> {
    let has_indent = block
        .syntax()
        .children_with_tokens()
        .any(|e| e.kind() == SyntaxKind::INDENT);
    if !has_indent {
        return None;
    }
    let stmts: Vec<ast::Stmt> = block.statements().collect();
    let first = stmts.first()?;
    let last = stmts.last()?;
    let start = line_start(text, first.text_range().start().into());
    let end: usize = stmt_lines(text, last.text_range()).end().into();
    Some((
        rng(start, end),
        indent_at(text, first.text_range().start().into()),
    ))
}

/// Innermost compound statement of type `N` whose header (before the body) contains `offset`.
fn header_at<N: AstNode>(
    root: &SyntaxNode,
    offset: TextSize,
    body: impl Fn(&N) -> Option<ast::Block>,
) -> Option<N> {
    token_at(root, offset)?
        .parent_ancestors()
        .filter_map(N::cast)
        .find(|n| {
            body(n).is_some_and(|b| offset < b.text_range().start())
                && n.text_range().start() <= offset
        })
}

fn if_at(root: &SyntaxNode, offset: TextSize) -> Option<ast::IfStmt> {
    header_at(root, offset, |i: &ast::IfStmt| i.body())
}

fn func_at(root: &SyntaxNode, offset: TextSize) -> Option<ast::FuncDef> {
    header_at(root, offset, |f: &ast::FuncDef| f.body())
}

fn is_plain_if(i: &ast::IfStmt) -> bool {
    i.elif_clauses().next().is_none() && i.else_clause().is_none()
}

fn single_stmt(block: &ast::Block) -> Option<ast::Stmt> {
    let mut it = block.statements();
    let first = it.next()?;
    if it.next().is_some() {
        return None;
    }
    Some(first)
}

fn negated_op(op: &str) -> Option<&'static str> {
    Some(match op {
        "==" => "!=",
        "!=" => "==",
        "<" => ">=",
        ">" => "<=",
        "<=" => ">",
        ">=" => "<",
        "is" => "is not",
        "is not" => "is",
        "in" => "not in",
        "not in" => "in",
        _ => return None,
    })
}

fn flipped_op(op: &str) -> Option<&'static str> {
    Some(match op {
        "<" => ">",
        ">" => "<",
        "<=" => ">=",
        ">=" => "<=",
        "==" => "==",
        "!=" => "!=",
        "is" => "is",
        "is not" => "is not",
        _ => return None,
    })
}

fn single_compare(c: &ast::CompareExpr) -> Option<(ast::Expr, String, ast::Expr)> {
    let ops: Vec<ast::CompareOp> = c.ops().collect();
    let operands: Vec<ast::Expr> = c.operands().collect();
    if ops.len() != 1 || operands.len() != 2 {
        return None;
    }
    Some((operands[0].clone(), ops[0].text(), operands[1].clone()))
}

/// Source text of the negation of `cond`.
pub fn negate(text: &str, cond: &ast::Expr) -> String {
    let src = expr_text(text, cond);
    match cond {
        ast::Expr::Not(n) => n
            .operand()
            .map(|o| expr_text(text, &o))
            .unwrap_or_else(|| "True".into()),
        ast::Expr::Paren(p) => p
            .inner()
            .map(|i| negate(text, &i))
            .unwrap_or_else(|| format!("not {src}")),
        ast::Expr::Compare(c) => match single_compare(c) {
            Some((l, op, r)) => match negated_op(&op) {
                Some(n) => format!("{} {n} {}", expr_text(text, &l), expr_text(text, &r)),
                None => format!("not ({src})"),
            },
            None => format!("not ({src})"),
        },
        ast::Expr::Literal(l) => match l.kind() {
            Some(SyntaxKind::TRUE_KW) => "False".into(),
            Some(SyntaxKind::FALSE_KW) => "True".into(),
            _ => format!("not {src}"),
        },
        _ if is_atom(cond)
            || matches!(
                cond,
                ast::Expr::Unary(_) | ast::Expr::Binary(_) | ast::Expr::Await(_)
            ) =>
        {
            format!("not {src}")
        }
        _ => format!("not ({src})"),
    }
}

fn and_operand(text: &str, e: &ast::Expr) -> String {
    let src = expr_text(text, e);
    let needs = matches!(e, ast::Expr::BoolOp(b) if b.is_or())
        || matches!(
            e,
            ast::Expr::Conditional(_) | ast::Expr::Lambda(_) | ast::Expr::Named(_)
        );
    if needs {
        format!("({src})")
    } else {
        src
    }
}

fn string_at(root: &SyntaxNode, offset: TextSize) -> Option<ast::StringExpr> {
    ancestor_at::<ast::StringExpr>(root, offset)
}

/// `(prefix, quote, content, is_triple)` of a string literal.
fn string_parts(raw: &str) -> Option<(String, char, String, bool)> {
    let prefix: String = raw
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    let body = &raw[prefix.len()..];
    let q = body.chars().next()?;
    if q != '"' && q != '\'' {
        return None;
    }
    let triple = body.len() >= 6
        && body.starts_with(&q.to_string().repeat(3))
        && body.ends_with(&q.to_string().repeat(3));
    let ql = if triple { 3 } else { 1 };
    if body.len() < 2 * ql || !body.ends_with(q) {
        return None;
    }
    Some((prefix, q, body[ql..body.len() - ql].to_string(), triple))
}

// ---------------------------------------------------------------------------
// if statements
// ---------------------------------------------------------------------------

/// Invert 'if' condition: negates the condition and swaps the branches
/// (an `if` without `else` gets a `pass` body and the old body as `else`).
pub fn invert_if(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, text) = root_and_text(file);
    let if_ = if_at(&root, offset)?;
    if if_.elif_clauses().next().is_some() {
        return None;
    }
    let cond = if_.condition()?;
    let (body_lines, body_indent) = block_lines(text, &if_.body()?)?;
    let indent = indent_at(text, if_.text_range().start().into());
    let mut edits = vec![(cond.text_range(), negate(text, &cond))];
    match if_.else_clause().and_then(|e| e.body()) {
        Some(else_body) => {
            let (else_lines, _) = block_lines(text, &else_body)?;
            edits.push((body_lines, slice(text, else_lines).to_string()));
            edits.push((else_lines, slice(text, body_lines).to_string()));
        }
        None => edits.push((
            body_lines,
            format!(
                "{body_indent}pass\n{indent}else:\n{}",
                slice(text, body_lines)
            ),
        )),
    }
    Some(Intention {
        title: "Invert 'if' condition".into(),
        edits,
    })
}

/// Split `if a and b:` into nested `if a:` / `if b:`.
pub fn split_if(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, text) = root_and_text(file);
    let if_ = if_at(&root, offset)?;
    if !is_plain_if(&if_) {
        return None;
    }
    let cond = if_.condition()?.unparen();
    let ast::Expr::BoolOp(b) = &cond else {
        return None;
    };
    if b.is_or() {
        return None;
    }
    let operands: Vec<ast::Expr> = b.operands().collect();
    if operands.len() < 2 {
        return None;
    }
    let (body_lines, body_indent) = block_lines(text, &if_.body()?)?;
    let first = expr_text(text, &operands[0]);
    let rest: Vec<String> = operands[1..].iter().map(|o| expr_text(text, o)).collect();
    let inner_indent = format!("{body_indent}    ");
    let new_body = format!(
        "{body_indent}if {}:\n{}",
        rest.join(" and "),
        reindent(slice(text, body_lines), &body_indent, &inner_indent)
    );
    Some(Intention {
        title: "Split 'if' into nested ifs".into(),
        edits: vec![
            (if_.condition()?.text_range(), first),
            (body_lines, new_body),
        ],
    })
}

/// Merge `if a:` containing only `if b:` into `if a and b:`.
pub fn merge_ifs(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, text) = root_and_text(file);
    let outer = if_at(&root, offset)?;
    if !is_plain_if(&outer) {
        return None;
    }
    let outer_body = outer.body()?;
    let ast::Stmt::If(inner) = single_stmt(&outer_body)? else {
        return None;
    };
    if !is_plain_if(&inner) {
        return None;
    }
    let (outer_lines, outer_indent) = block_lines(text, &outer_body)?;
    let (inner_lines, inner_indent) = block_lines(text, &inner.body()?)?;
    let new_cond = format!(
        "{} and {}",
        and_operand(text, &outer.condition()?),
        and_operand(text, &inner.condition()?)
    );
    Some(Intention {
        title: "Merge nested 'if's".into(),
        edits: vec![
            (outer.condition()?.text_range(), new_cond),
            (
                outer_lines,
                reindent(slice(text, inner_lines), &inner_indent, &outer_indent),
            ),
        ],
    })
}

// ---------------------------------------------------------------------------
// comparisons & parentheses
// ---------------------------------------------------------------------------

/// `a < b` → `b > a`.
pub fn flip_comparison(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, text) = root_and_text(file);
    let c = ancestor_at::<ast::CompareExpr>(&root, offset)?;
    let (l, op, r) = single_compare(&c)?;
    let flipped = flipped_op(&op)?;
    Some(Intention {
        title: format!("Flip '{op}'"),
        edits: vec![(
            c.text_range(),
            format!("{} {flipped} {}", expr_text(text, &r), expr_text(text, &l)),
        )],
    })
}

/// `a == b` → `a != b`; `not a == b` → `a != b`.
pub fn negate_comparison(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, text) = root_and_text(file);
    let c = ancestor_at::<ast::CompareExpr>(&root, offset)?;
    let (l, op, r) = single_compare(&c)?;
    let neg = negated_op(&op)?;
    let replacement = format!("{} {neg} {}", expr_text(text, &l), expr_text(text, &r));
    // `not (a == b)` / `not a == b`: drop the `not`.
    let mut outer = c.syntax().clone();
    while outer
        .parent()
        .is_some_and(|p| p.kind() == SyntaxKind::PAREN_EXPR)
    {
        outer = outer.parent().unwrap();
    }
    if let Some(p) = outer.parent().filter(|p| p.kind() == SyntaxKind::NOT_EXPR) {
        return Some(Intention {
            title: format!(
                "Simplify 'not {}' to '{replacement}'",
                normalize(slice(text, c.text_range()))
            ),
            edits: vec![(p.text_range(), replacement)],
        });
    }
    Some(Intention {
        title: format!("Change '{op}' to '{neg}'"),
        edits: vec![(c.text_range(), replacement)],
    })
}

/// Removes parentheses that do not change the meaning.
pub fn remove_redundant_parens(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, text) = root_and_text(file);
    let p = ancestor_at::<ast::ParenExpr>(&root, offset)?;
    let inner = p.inner()?;
    let src = expr_text(text, &inner);
    if src.contains('\n') {
        return None;
    }
    let parent = p.syntax().parent()?;
    let is_number = matches!(&inner, ast::Expr::Literal(l) if matches!(l.kind(), Some(SyntaxKind::INT_NUMBER | SyntaxKind::FLOAT_NUMBER | SyntaxKind::COMPLEX_NUMBER)));
    let special = matches!(
        inner,
        ast::Expr::Yield(_) | ast::Expr::YieldFrom(_) | ast::Expr::Named(_) | ast::Expr::Star(_)
    );
    let first_child = parent.children().next().map(|c| c.text_range()) == Some(p.text_range());
    let redundant = match parent.kind() {
        SyntaxKind::ASSIGN_STMT
        | SyntaxKind::ANN_ASSIGN_STMT
        | SyntaxKind::AUG_ASSIGN_STMT
        | SyntaxKind::RETURN_STMT
        | SyntaxKind::EXPR_STMT
        | SyntaxKind::ARG
        | SyntaxKind::KEYWORD_ARG
        | SyntaxKind::IF_STMT
        | SyntaxKind::ELIF_CLAUSE
        | SyntaxKind::WHILE_STMT
        | SyntaxKind::ASSERT_STMT
        | SyntaxKind::PAREN_EXPR
        | SyntaxKind::DICT_ITEM
        | SyntaxKind::LIST_EXPR
        | SyntaxKind::SET_EXPR
        | SyntaxKind::DEFAULT_VALUE
        | SyntaxKind::WITH_ITEM
        | SyntaxKind::DEL_STMT
        | SyntaxKind::YIELD_EXPR => !special,
        SyntaxKind::FOR_STMT | SyntaxKind::COMP_FOR => {
            !special && !is_assignment_target(p.syntax())
        }
        SyntaxKind::SUBSCRIPT_EXPR => !first_child || (is_atom(&inner) && !is_number),
        SyntaxKind::ATTRIBUTE_EXPR | SyntaxKind::CALL_EXPR => is_atom(&inner) && !is_number,
        _ => is_atom(&inner),
    };
    if !redundant {
        return None;
    }
    Some(Intention {
        title: "Remove redundant parentheses".into(),
        edits: vec![(p.text_range(), src)],
    })
}

// ---------------------------------------------------------------------------
// strings
// ---------------------------------------------------------------------------

/// Switches a single-quoted literal to double quotes (and vice versa).
pub fn convert_quotes(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, _) = root_and_text(file);
    let s = string_at(&root, offset)?;
    let (prefix, q, content, triple) = string_parts(&s.raw_text())?;
    if triple {
        return None;
    }
    let nq = if q == '"' { '\'' } else { '"' };
    let is_raw = prefix.to_ascii_lowercase().contains('r');
    let new_content = if is_raw {
        if content.contains(nq) {
            return None;
        }
        content
    } else {
        let mut out = String::with_capacity(content.len());
        let mut chars = content.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some(n) if n == q => out.push(q),
                    Some(n) => {
                        out.push('\\');
                        out.push(n);
                    }
                    None => out.push('\\'),
                }
            } else if c == nq {
                out.push('\\');
                out.push(nq);
            } else {
                out.push(c);
            }
        }
        out
    };
    let title = if nq == '"' {
        "Convert to double quotes"
    } else {
        "Convert to single quotes"
    };
    Some(Intention {
        title: title.into(),
        edits: vec![(s.text_range(), format!("{prefix}{nq}{new_content}{nq}"))],
    })
}

fn fstring_expr_ok(expr: &str, q: char) -> bool {
    !expr.contains(q) && !expr.contains('\\') && !expr.contains('\n') && !expr.is_empty()
}

fn escape_braces(s: &str) -> String {
    s.replace('{', "{{").replace('}', "}}")
}

/// Converts a `%`-formatted string into an f-string.
fn percent_to_fstring(text: &str, bin: &ast::BinaryExpr) -> Option<String> {
    if bin.op()?.kind() != SyntaxKind::PERCENT {
        return None;
    }
    let ast::Expr::String(s) = bin.lhs()? else {
        return None;
    };
    let (prefix, q, content, triple) = string_parts(&s.raw_text())?;
    if !(prefix.is_empty() || prefix.eq_ignore_ascii_case("u")) || triple {
        return None;
    }
    let rhs = bin.rhs()?;
    let args: Vec<String> = match &rhs {
        ast::Expr::Tuple(t)
            if t.syntax()
                .first_token()
                .is_some_and(|t| t.kind() == SyntaxKind::L_PAREN) =>
        {
            t.elements().map(|e| expr_text(text, &e)).collect()
        }
        ast::Expr::Paren(p) => vec![expr_text(text, &p.inner()?)],
        other => vec![expr_text(text, other)],
    };
    if args.iter().any(|a| !fstring_expr_ok(a, q)) {
        return None;
    }
    let mut out = String::new();
    let mut literal = String::new();
    let mut chars = content.chars().peekable();
    let mut idx = 0;
    while let Some(c) = chars.next() {
        if c != '%' {
            literal.push(c);
            continue;
        }
        match chars.peek() {
            Some('%') => {
                chars.next();
                literal.push('%');
                continue;
            }
            Some('(') => return None,
            None => return None,
            _ => {}
        }
        let mut flags = String::new();
        while let Some(&f) = chars.peek() {
            if "-+ 0#".contains(f) {
                flags.push(f);
                chars.next();
            } else {
                break;
            }
        }
        let mut width = String::new();
        while let Some(&d) = chars.peek() {
            if d.is_ascii_digit() {
                width.push(d);
                chars.next();
            } else if d == '*' {
                return None;
            } else {
                break;
            }
        }
        let mut precision = String::new();
        if chars.peek() == Some(&'.') {
            chars.next();
            precision.push('.');
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() {
                    precision.push(d);
                    chars.next();
                } else if d == '*' {
                    return None;
                } else {
                    break;
                }
            }
        }
        while let Some(&m) = chars.peek() {
            if "hlL".contains(m) {
                chars.next();
            } else {
                break;
            }
        }
        let conv = chars.next()?;
        let expr = args.get(idx)?.clone();
        idx += 1;
        out.push_str(&escape_braces(&literal));
        literal.clear();
        let (conversion, ty): (&str, &str) = match conv {
            's' => ("!s", ""),
            'r' => ("!r", ""),
            'a' => ("!a", ""),
            'd' | 'i' | 'u' => ("", "d"),
            'f' | 'F' | 'e' | 'E' | 'g' | 'G' | 'x' | 'X' | 'o' | 'c' => ("", &content[0..0]),
            _ => return None,
        };
        let ty = if conv == 'd' || conv == 'i' || conv == 'u' {
            "d".to_string()
        } else if ty.is_empty() && conv != 's' && conv != 'r' && conv != 'a' {
            conv.to_string()
        } else {
            String::new()
        };
        let plain_int = ty == "d" && flags.is_empty() && width.is_empty() && precision.is_empty();
        let mut spec = String::new();
        if !plain_int
            && (!flags.is_empty() || !width.is_empty() || !precision.is_empty() || !ty.is_empty())
        {
            let left = flags.contains('-');
            if left {
                spec.push('<');
            } else if conv == 's' && !width.is_empty() {
                spec.push('>');
            }
            if flags.contains('+') {
                spec.push('+');
            } else if flags.contains(' ') {
                spec.push(' ');
            }
            if flags.contains('#') {
                spec.push('#');
            }
            if flags.contains('0') && !left {
                spec.push('0');
            }
            spec.push_str(&width);
            spec.push_str(&precision);
            spec.push_str(&ty);
        }
        if spec.is_empty() {
            out.push_str(&format!("{{{expr}{conversion}}}"));
        } else {
            out.push_str(&format!("{{{expr}{conversion}:{spec}}}"));
        }
    }
    if idx != args.len() {
        return None;
    }
    out.push_str(&escape_braces(&literal));
    Some(format!("f{q}{out}{q}"))
}

/// Converts `"...".format(...)` into an f-string.
fn format_call_to_fstring(text: &str, call: &ast::CallExpr) -> Option<String> {
    let ast::Expr::Attribute(a) = call.callee()? else {
        return None;
    };
    if a.attr_text() != "format" {
        return None;
    }
    let ast::Expr::String(s) = a.object()? else {
        return None;
    };
    let (prefix, q, content, triple) = string_parts(&s.raw_text())?;
    if !(prefix.is_empty() || prefix.eq_ignore_ascii_case("u")) || triple {
        return None;
    }
    let mut positional: Vec<String> = Vec::new();
    let mut keywords: Vec<(String, String)> = Vec::new();
    for arg in call.arg_list()?.args() {
        match arg {
            ast::AnyArg::Positional(p) => positional.push(expr_text(text, &p.expr()?)),
            ast::AnyArg::Keyword(k) => keywords.push((k.name_text(), expr_text(text, &k.value()?))),
            _ => return None,
        }
    }
    if positional
        .iter()
        .chain(keywords.iter().map(|(_, v)| v))
        .any(|e| !fstring_expr_ok(e, q))
    {
        return None;
    }
    let mut out = String::new();
    let mut chars = content.chars().peekable();
    let mut auto = 0usize;
    while let Some(c) = chars.next() {
        match c {
            '{' => {
                if chars.peek() == Some(&'{') {
                    chars.next();
                    out.push_str("{{");
                    continue;
                }
                let mut field = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some('{') | None => return None,
                        Some(ch) => field.push(ch),
                    }
                }
                let split = field.find(['!', ':']).unwrap_or(field.len());
                let (name, rest) = field.split_at(split);
                let (head, suffix) = match name.find(['.', '[']) {
                    Some(i) => name.split_at(i),
                    None => (name, ""),
                };
                let expr = if head.is_empty() {
                    let e = positional.get(auto)?.clone();
                    auto += 1;
                    e
                } else if let Ok(i) = head.parse::<usize>() {
                    positional.get(i)?.clone()
                } else {
                    keywords.iter().find(|(k, _)| k == head)?.1.clone()
                };
                let expr = if suffix.is_empty() || is_simple_name(&expr) {
                    format!("{expr}{suffix}")
                } else {
                    format!("({expr}){suffix}")
                };
                out.push_str(&format!("{{{expr}{rest}}}"));
            }
            '}' => {
                if chars.peek() == Some(&'}') {
                    chars.next();
                    out.push_str("}}");
                } else {
                    return None;
                }
            }
            other => out.push(other),
        }
    }
    Some(format!("f{q}{out}{q}"))
}

fn is_simple_name(expr: &str) -> bool {
    expr.chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
}

/// Convert `%` formatting or `str.format` to an f-string.
pub fn convert_to_fstring(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, text) = root_and_text(file);
    let tok = token_at(&root, offset)?;
    for n in tok.parent_ancestors() {
        if let Some(bin) = ast::BinaryExpr::cast(n.clone()) {
            if let Some(f) = percent_to_fstring(text, &bin) {
                return Some(Intention {
                    title: "Convert to f-string".into(),
                    edits: vec![(bin.text_range(), f)],
                });
            }
        }
        if let Some(call) = ast::CallExpr::cast(n.clone()) {
            if let Some(f) = format_call_to_fstring(text, &call) {
                return Some(Intention {
                    title: "Convert to f-string".into(),
                    edits: vec![(call.text_range(), f)],
                });
            }
        }
        if ast::Stmt::can_cast(n.kind()) {
            break;
        }
    }
    None
}

/// Toggles between single- and triple-quoted string literals.
pub fn convert_triple_quoted(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, _) = root_and_text(file);
    let s = string_at(&root, offset)?;
    let (prefix, q, content, triple) = string_parts(&s.raw_text())?;
    let qs = q.to_string();
    if triple {
        if content.contains('\n') || content.contains(q) || content.ends_with('\\') {
            return None;
        }
        Some(Intention {
            title: "Convert to single-quoted string".into(),
            edits: vec![(s.text_range(), format!("{prefix}{q}{content}{q}"))],
        })
    } else {
        if content.ends_with(q) || content.contains(&qs.repeat(3)) {
            return None;
        }
        let t = qs.repeat(3);
        Some(Intention {
            title: "Convert to triple-quoted string".into(),
            edits: vec![(s.text_range(), format!("{prefix}{t}{content}{t}"))],
        })
    }
}

/// Splits a string literal at the caret into two implicitly concatenated literals.
pub fn split_string(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, text) = root_and_text(file);
    let s = string_at(&root, offset)?;
    let (prefix, q, _, triple) = string_parts(&s.raw_text())?;
    if triple || prefix.to_ascii_lowercase().contains('f') {
        return None;
    }
    let cr = s.content_range();
    if offset <= cr.start() || offset >= cr.end() {
        return None;
    }
    let off: usize = offset.into();
    if text.as_bytes().get(off - 1) == Some(&b'\\') {
        return None;
    }
    let c1 = slice(text, TextRange::new(cr.start(), offset));
    let c2 = slice(text, TextRange::new(offset, cr.end()));
    Some(Intention {
        title: "Split string at caret".into(),
        edits: vec![(
            s.text_range(),
            format!("{prefix}{q}{c1}{q} {prefix}{q}{c2}{q}"),
        )],
    })
}

/// Joins implicitly concatenated string literals into one.
pub fn join_string(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, _) = root_and_text(file);
    let c = ancestor_at::<ast::ConcatStringExpr>(&root, offset)?;
    let mut parts: Vec<(String, char, String)> = Vec::new();
    for p in c.parts() {
        let ast::Expr::String(s) = p else { return None };
        let (prefix, q, content, triple) = string_parts(&s.raw_text())?;
        if triple {
            return None;
        }
        parts.push((prefix.to_ascii_lowercase(), q, content));
    }
    let (prefix, q, _) = parts.first()?.clone();
    if parts.iter().any(|(p, qq, _)| *p != prefix || *qq != q) {
        return None;
    }
    let joined: String = parts.iter().map(|(_, _, c)| c.as_str()).collect();
    Some(Intention {
        title: "Join string literals".into(),
        edits: vec![(c.text_range(), format!("{prefix}{q}{joined}{q}"))],
    })
}

// ---------------------------------------------------------------------------
// collections, loops, lambdas
// ---------------------------------------------------------------------------

/// `dict(a=1, b=2)` → `{"a": 1, "b": 2}`.
pub fn dict_call_to_literal(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, text) = root_and_text(file);
    let call = token_at(&root, offset)?
        .parent_ancestors()
        .filter_map(ast::CallExpr::cast)
        .find(|c| matches!(c.callee(), Some(ast::Expr::Name(n)) if n.name_text() == "dict"))?;
    let mut items: Vec<String> = Vec::new();
    if let Some(args) = call.arg_list() {
        for a in args.args() {
            match a {
                ast::AnyArg::Keyword(k) => items.push(format!(
                    "\"{}\": {}",
                    k.name_text(),
                    expr_text(text, &k.value()?)
                )),
                _ => return None,
            }
        }
    }
    Some(Intention {
        title: "Replace dict() call with dict literal".into(),
        edits: vec![(call.text_range(), format!("{{{}}}", items.join(", ")))],
    })
}

enum CompKind {
    List,
    Set,
    Dict,
}

impl CompKind {
    fn name(&self) -> &'static str {
        match self {
            CompKind::List => "list",
            CompKind::Set => "set",
            CompKind::Dict => "dict",
        }
    }
}

/// Whether `value` is an empty list / set / dict constructor of `kind`.
fn is_empty_collection(text: &str, value: &ast::Expr, kind: &CompKind) -> bool {
    let src = normalize(&expr_text(text, value));
    match kind {
        CompKind::List => src == "[]" || src == "list()",
        CompKind::Set => src == "set()",
        CompKind::Dict => src == "{}" || src == "dict()",
    }
}

/// `result = []; for x in it: (if c:) result.append(e)` → comprehension.
pub fn loop_to_comprehension(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, text) = root_and_text(file);
    let for_ = header_at(&root, offset, |f: &ast::ForStmt| f.body())?;
    if for_.else_clause().is_some() || for_.is_async() {
        return None;
    }
    let body = for_.body()?;
    let mut inner = single_stmt(&body)?;
    let mut cond: Option<String> = None;
    if let ast::Stmt::If(i) = &inner {
        if !is_plain_if(i) {
            return None;
        }
        cond = Some(expr_text(text, &i.condition()?));
        inner = single_stmt(&i.body()?)?;
    }
    let (target_name, kind, elem) = match &inner {
        ast::Stmt::Expr(e) => {
            let ast::Expr::Call(call) = e.expr()? else {
                return None;
            };
            let ast::Expr::Attribute(a) = call.callee()? else {
                return None;
            };
            let ast::Expr::Name(obj) = a.object()? else {
                return None;
            };
            let kind = match a.attr_text().as_str() {
                "append" => CompKind::List,
                "add" => CompKind::Set,
                _ => return None,
            };
            let args: Vec<ast::AnyArg> = call.arg_list()?.args().collect();
            let [ast::AnyArg::Positional(arg)] = args.as_slice() else {
                return None;
            };
            (obj.name_text(), kind, expr_text(text, &arg.expr()?))
        }
        ast::Stmt::Assign(a) => {
            let targets = a.targets();
            let [ast::Expr::Subscript(sub)] = targets.as_slice() else {
                return None;
            };
            let ast::Expr::Name(obj) = sub.object()? else {
                return None;
            };
            let key = sub.index_expr()?;
            (
                obj.name_text(),
                CompKind::Dict,
                format!(
                    "{}: {}",
                    expr_text(text, &key),
                    expr_text(text, &a.value()?)
                ),
            )
        }
        _ => return None,
    };
    let prev = for_
        .syntax()
        .prev_sibling()
        .and_then(ast::AssignStmt::cast)?;
    let prev_targets = prev.targets();
    let [ast::Expr::Name(pt)] = prev_targets.as_slice() else {
        return None;
    };
    if pt.name_text() != target_name || !is_empty_collection(text, &prev.value()?, &kind) {
        return None;
    }
    let loop_target = expr_text(text, &for_.target()?);
    let iter = expr_text(text, &for_.iter()?);
    let cond_text = cond.map(|c| format!(" if {c}")).unwrap_or_default();
    let comp = match kind {
        CompKind::List => format!("[{elem} for {loop_target} in {iter}{cond_text}]"),
        CompKind::Set | CompKind::Dict => {
            format!("{{{elem} for {loop_target} in {iter}{cond_text}}}")
        }
    };
    let indent = indent_at(text, prev.text_range().start().into());
    let range = TextRange::new(
        stmt_lines(text, prev.text_range()).start(),
        stmt_lines(text, for_.text_range()).end(),
    );
    Some(Intention {
        title: format!("Replace loop with {} comprehension", kind.name()),
        edits: vec![(range, format!("{indent}{target_name} = {comp}\n"))],
    })
}

/// `result = [e for x in it if c]` → explicit loop.
pub fn comprehension_to_loop(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, text) = root_and_text(file);
    let a = ancestor_at::<ast::AssignStmt>(&root, offset)?;
    let targets = a.targets();
    let [ast::Expr::Name(t)] = targets.as_slice() else {
        return None;
    };
    let name = t.name_text();
    let value = a.value()?.unparen();
    let (kind, elem, clauses): (CompKind, String, Vec<ast::CompFor>) = match &value {
        ast::Expr::ListComp(c) => (
            CompKind::List,
            expr_text(text, &c.element()?),
            c.clauses().collect(),
        ),
        ast::Expr::SetComp(c) => (
            CompKind::Set,
            expr_text(text, &c.element()?),
            c.clauses().collect(),
        ),
        ast::Expr::DictComp(c) => {
            let item = c.item()?;
            (
                CompKind::Dict,
                format!(
                    "[{}] = {}",
                    expr_text(text, &item.key()?),
                    expr_text(text, &item.value()?)
                ),
                c.clauses().collect(),
            )
        }
        _ => return None,
    };
    if clauses.is_empty() || clauses.iter().any(|c| c.is_async()) {
        return None;
    }
    let indent = indent_at(text, a.text_range().start().into());
    let init = match kind {
        CompKind::List => "[]",
        CompKind::Set => "set()",
        CompKind::Dict => "{}",
    };
    let mut out = format!("{indent}{name} = {init}\n");
    let mut cur = indent.clone();
    for c in &clauses {
        out.push_str(&format!(
            "{cur}for {} in {}:\n",
            expr_text(text, &c.target()?),
            expr_text(text, &c.iter()?)
        ));
        cur.push_str("    ");
        for cond in c.conditions() {
            out.push_str(&format!("{cur}if {}:\n", expr_text(text, &cond)));
            cur.push_str("    ");
        }
    }
    let stmt = match kind {
        CompKind::List => format!("{name}.append({elem})"),
        CompKind::Set => format!("{name}.add({elem})"),
        CompKind::Dict => format!("{name}{elem}"),
    };
    out.push_str(&format!("{cur}{stmt}\n"));
    Some(Intention {
        title: format!("Convert {} comprehension to for loop", kind.name()),
        edits: vec![(stmt_lines(text, a.text_range()), out)],
    })
}

/// `f = lambda x: x + 1` → `def f(x): return x + 1`.
pub fn lambda_to_function(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, text) = root_and_text(file);
    let a = ancestor_at::<ast::AssignStmt>(&root, offset)?;
    let targets = a.targets();
    let [ast::Expr::Name(t)] = targets.as_slice() else {
        return None;
    };
    let ast::Expr::Lambda(l) = a.value()?.unparen() else {
        return None;
    };
    let params = l.params().map(|p| normalize(&p.text())).unwrap_or_default();
    let body = expr_text(text, &l.body()?);
    let indent = indent_at(text, a.text_range().start().into());
    let out = format!(
        "{indent}def {}({params}):\n{indent}    return {body}\n",
        t.name_text()
    );
    Some(Intention {
        title: "Convert lambda to function".into(),
        edits: vec![(stmt_lines(text, a.text_range()), out)],
    })
}

// ---------------------------------------------------------------------------
// methods
// ---------------------------------------------------------------------------

fn decorator_names(f: &ast::FuncDef) -> Vec<String> {
    f.decorators().filter_map(|d| d.name_text()).collect()
}

fn first_param_name(f: &ast::FuncDef) -> Option<String> {
    f.params()?.plain_params().next().map(|p| p.name_text())
}

fn def_line_start(text: &str, f: &ast::FuncDef) -> usize {
    let def = f
        .def_token()
        .map(|t| t.text_range().start())
        .unwrap_or(f.text_range().start());
    line_start(text, def.into())
}

/// Adds `@property` to a parameterless method.
pub fn method_to_property(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, text) = root_and_text(file);
    let f = func_at(&root, offset)?;
    if !f.is_method() || f.decorators().next().is_some() || f.name_text().starts_with("__") {
        return None;
    }
    let params: Vec<ast::AnyParam> = f.params()?.params().collect();
    if params.len() != 1 || first_param_name(&f).as_deref() != Some("self") {
        return None;
    }
    if f.body()?.syntax().descendants().any(|n| {
        matches!(
            n.kind(),
            SyntaxKind::YIELD_EXPR | SyntaxKind::YIELD_FROM_EXPR
        )
    }) {
        return None;
    }
    let at = def_line_start(text, &f);
    let indent = indent_at(text, at);
    Some(Intention {
        title: "Convert method to property".into(),
        edits: vec![(rng(at, at), format!("{indent}@property\n"))],
    })
}

/// Removes a sole `@property` decorator.
pub fn property_to_method(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, text) = root_and_text(file);
    let f = func_at(&root, offset)?;
    let decos: Vec<ast::Decorator> = f.decorators().collect();
    if decos.len() != 1 || decos[0].name_text().as_deref() != Some("property") {
        return None;
    }
    Some(Intention {
        title: "Convert property to method".into(),
        edits: vec![(stmt_lines(text, decos[0].text_range()), String::new())],
    })
}

/// Inserts a missing `self` parameter.
pub fn add_self(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, text) = root_and_text(file);
    let f = func_at(&root, offset)?;
    if !f.is_method() {
        return None;
    }
    let decos = decorator_names(&f);
    if decos
        .iter()
        .any(|d| d == "staticmethod" || d == "classmethod")
    {
        return None;
    }
    if matches!(first_param_name(&f).as_deref(), Some("self") | Some("cls")) {
        return None;
    }
    let params = f.params()?;
    let open = params
        .syntax()
        .first_token()
        .filter(|t| t.kind() == SyntaxKind::L_PAREN)?;
    let at: usize = open.text_range().end().into();
    let sep = if params.params().next().is_some() {
        ", "
    } else {
        ""
    };
    let _ = text;
    Some(Intention {
        title: "Add 'self' parameter".into(),
        edits: vec![(rng(at, at), format!("self{sep}"))],
    })
}

/// `@staticmethod` for a method that never uses `self`.
pub fn make_static(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, text) = root_and_text(file);
    let f = func_at(&root, offset)?;
    if !f.is_method() || f.decorators().next().is_some() || f.name_text().starts_with("__") {
        return None;
    }
    let first = first_param_name(&f)?;
    if first != "self" && first != "cls" {
        return None;
    }
    let body = f.body()?;
    if body
        .syntax()
        .descendants()
        .any(|n| n.kind() == SyntaxKind::NAME_EXPR && n.text().to_string() == first)
    {
        return None;
    }
    let param = f.params()?.plain_params().next()?;
    let at = def_line_start(text, &f);
    let indent = indent_at(text, at);
    Some(Intention {
        title: "Make method static".into(),
        edits: vec![
            (rng(at, at), format!("{indent}@staticmethod\n")),
            (element_with_comma(text, param.syntax()), String::new()),
        ],
    })
}

/// Moves a `@staticmethod` out of its class as a module-level function and
/// rewrites `Class.name(...)` / `self.name(...)` calls in this file.
pub fn static_to_function(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, text) = root_and_text(file);
    let f = func_at(&root, offset)?;
    let decos: Vec<ast::Decorator> = f.decorators().collect();
    if !f.is_method() || decos.len() != 1 || decos[0].name_text().as_deref() != Some("staticmethod")
    {
        return None;
    }
    let class = f
        .syntax()
        .parent()?
        .parent()
        .and_then(ast::ClassDef::cast)?;
    let class_name = class.name_text();
    let name = f.name_text();
    let func_range = f.text_range();
    // Attribute calls to rewrite, split into those inside and outside the moved text.
    let mut outside: Vec<Edit> = Vec::new();
    let mut inside: Vec<Edit> = Vec::new();
    for attr in root.descendants().filter_map(ast::AttributeExpr::cast) {
        if attr.attr_text() != name {
            continue;
        }
        let obj = attr
            .object()
            .map(|o| normalize(&expr_text(text, &o)))
            .unwrap_or_default();
        if obj != class_name && obj != "self" && obj != "cls" {
            continue;
        }
        if func_range.contains_range(attr.text_range()) {
            inside.push((attr.text_range() - func_range.start(), name.clone()));
        } else {
            outside.push((attr.text_range(), name.clone()));
        }
    }
    // Build the moved text: drop the decorator line, apply inner rewrites, dedent.
    let lines = stmt_lines(text, func_range);
    let func_text = slice(text, lines);
    let rel = func_range.start() - lines.start();
    let inside: Vec<Edit> = inside.into_iter().map(|(r, t)| (r + rel, t)).collect();
    let mut moved = apply_edits(func_text, &inside);
    let deco_line_len = {
        let d: usize = (decos[0].text_range().start() - lines.start()).into();
        func_text[d..]
            .find('\n')
            .map(|i| d + i + 1)
            .unwrap_or(func_text.len())
    };
    moved.replace_range(0..deco_line_len, "");
    let indent = indent_at(text, func_range.start().into());
    let moved = reindent(&moved, &indent, "");
    let mut edits = outside;
    let siblings = f
        .syntax()
        .parent()
        .map(|p| {
            p.children()
                .filter(|c| ast::Stmt::can_cast(c.kind()))
                .count()
        })
        .unwrap_or(2);
    if siblings <= 1 {
        edits.push((lines, format!("{indent}pass\n")));
    } else {
        edits.push((deletion_range(text, func_range), String::new()));
    }
    let at = after_last_line(text, class.text_range().end().into());
    let nl = if at == text.len() && !text.ends_with('\n') {
        "\n"
    } else {
        ""
    };
    edits.push((rng(at, at), format!("{nl}\n\n{moved}")));
    Some(Intention {
        title: "Convert static method to function".into(),
        edits,
    })
}

// ---------------------------------------------------------------------------
// returns & ternaries
// ---------------------------------------------------------------------------

fn return_bool(stmt: &ast::Stmt) -> Option<bool> {
    let ast::Stmt::Return(r) = stmt else {
        return None;
    };
    let ast::Expr::Literal(l) = r.value()? else {
        return None;
    };
    match l.kind()? {
        SyntaxKind::TRUE_KW => Some(true),
        SyntaxKind::FALSE_KW => Some(false),
        _ => None,
    }
}

/// `if c: return True else: return False` → `return c`.
pub fn simplify_bool_return(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, text) = root_and_text(file);
    let if_ = if_at(&root, offset)?;
    if if_.elif_clauses().next().is_some() {
        return None;
    }
    let cond = if_.condition()?;
    let then = return_bool(&single_stmt(&if_.body()?)?)?;
    let (end, other) = match if_.else_clause() {
        Some(e) => (
            stmt_lines(text, if_.text_range()).end(),
            return_bool(&single_stmt(&e.body()?)?)?,
        ),
        None => {
            let next = if_.syntax().next_sibling().and_then(ast::Stmt::cast)?;
            (
                stmt_lines(text, next.text_range()).end(),
                return_bool(&next)?,
            )
        }
    };
    if then == other {
        return None;
    }
    let value = if then {
        match &cond {
            ast::Expr::Compare(_) | ast::Expr::Not(_) | ast::Expr::Literal(_) => {
                expr_text(text, &cond)
            }
            ast::Expr::BoolOp(b)
                if b.operands()
                    .all(|o| matches!(o, ast::Expr::Compare(_) | ast::Expr::Not(_))) =>
            {
                expr_text(text, &cond)
            }
            _ => format!("bool({})", expr_text(text, &cond)),
        }
    } else {
        negate(text, &cond)
    };
    let indent = indent_at(text, if_.text_range().start().into());
    let range = TextRange::new(stmt_lines(text, if_.text_range()).start(), end);
    Some(Intention {
        title: format!("Simplify to 'return {value}'"),
        edits: vec![(range, format!("{indent}return {value}\n"))],
    })
}

/// `x = a if c else b` → `if c: x = a else: x = b` (also for `return`).
pub fn ternary_to_if(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, text) = root_and_text(file);
    let stmt = token_at(&root, offset)?
        .parent_ancestors()
        .find_map(ast::Stmt::cast)?;
    let (prefix, value) = match &stmt {
        ast::Stmt::Assign(a) => {
            let targets = a.targets();
            let [t] = targets.as_slice() else { return None };
            (format!("{} = ", expr_text(text, t)), a.value()?)
        }
        ast::Stmt::Return(r) => ("return ".to_string(), r.value()?),
        _ => return None,
    };
    let ast::Expr::Conditional(c) = value.unparen() else {
        return None;
    };
    let indent = indent_at(text, stmt.text_range().start().into());
    let out = format!(
        "{indent}if {}:\n{indent}    {prefix}{}\n{indent}else:\n{indent}    {prefix}{}\n",
        expr_text(text, &c.condition()?),
        expr_text(text, &c.true_expr()?),
        expr_text(text, &c.false_expr()?)
    );
    Some(Intention {
        title: "Convert ternary to if/else statement".into(),
        edits: vec![(stmt_lines(text, stmt.text_range()), out)],
    })
}

/// `if c: x = a else: x = b` → `x = a if c else b` (also for `return`).
pub fn if_to_ternary(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, text) = root_and_text(file);
    let if_ = if_at(&root, offset)?;
    if if_.elif_clauses().next().is_some() {
        return None;
    }
    let then = single_stmt(&if_.body()?)?;
    let other = single_stmt(&if_.else_clause()?.body()?)?;
    let cond = expr_text(text, &if_.condition()?);
    let out = match (&then, &other) {
        (ast::Stmt::Assign(a), ast::Stmt::Assign(b)) => {
            let (ta, tb) = (a.targets(), b.targets());
            let ([t1], [t2]) = (ta.as_slice(), tb.as_slice()) else {
                return None;
            };
            if normalize(&expr_text(text, t1)) != normalize(&expr_text(text, t2)) {
                return None;
            }
            format!(
                "{} = {} if {cond} else {}",
                expr_text(text, t1),
                expr_text(text, &a.value()?),
                expr_text(text, &b.value()?)
            )
        }
        (ast::Stmt::Return(a), ast::Stmt::Return(b)) => format!(
            "return {} if {cond} else {}",
            expr_text(text, &a.value()?),
            expr_text(text, &b.value()?)
        ),
        _ => return None,
    };
    let indent = indent_at(text, if_.text_range().start().into());
    Some(Intention {
        title: "Convert if/else to ternary".into(),
        edits: vec![(
            stmt_lines(text, if_.text_range()),
            format!("{indent}{out}\n"),
        )],
    })
}

// ---------------------------------------------------------------------------
// imports
// ---------------------------------------------------------------------------

/// Binding id of an import alias node.
fn alias_binding(
    file: &FileInfo,
    alias: &ast::ImportAlias,
) -> Option<rustpy_semantic::scope::BindingId> {
    file.scopes
        .bindings
        .iter()
        .enumerate()
        .find(|(_, b)| {
            matches!(b.kind, BindingKind::Import(_)) && alias.text_range().contains_range(b.range)
        })
        .map(|(i, _)| rustpy_semantic::scope::BindingId(i as u32))
}

/// `from a.b import c` → `import a.b` + `a.b.c` at usages.
pub fn from_import_to_import(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, text) = root_and_text(file);
    let imp = ancestor_at::<ast::ImportFromStmt>(&root, offset)?;
    if imp.level() != 0 || imp.is_star() {
        return None;
    }
    let module = imp.module()?.path();
    let mut edits: Vec<Edit> = Vec::new();
    for alias in imp.aliases() {
        let imported = alias.name();
        let id = alias_binding(file, &alias)?;
        for r in file.scopes.references_to(id) {
            edits.push((r.range, format!("{module}.{imported}")));
        }
    }
    let _ = text;
    edits.push((imp.text_range(), format!("import {module}")));
    Some(Intention {
        title: format!("Convert to 'import {module}'"),
        edits,
    })
}

/// `import a.b` with usages `a.b.c` → `from a.b import c`.
pub fn import_to_from_import(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, text) = root_and_text(file);
    let imp = ancestor_at::<ast::ImportStmt>(&root, offset)?;
    let aliases: Vec<ast::ImportAlias> = imp.aliases().collect();
    let [alias] = aliases.as_slice() else {
        return None;
    };
    if alias.alias().is_some() {
        return None;
    }
    let module = alias.name();
    let id = alias_binding(file, alias)?;
    let refs = file.scopes.references_to(id);
    if refs.is_empty() {
        return None;
    }
    let mut names: Vec<String> = Vec::new();
    let mut edits: Vec<Edit> = Vec::new();
    for r in refs {
        let tok = token_at(&root, r.range.start())?;
        let mut cur = tok.parent()?;
        if cur.kind() != SyntaxKind::NAME_EXPR {
            return None;
        }
        while normalize(&cur.text().to_string()) != module {
            let parent = cur.parent()?;
            if parent.kind() != SyntaxKind::ATTRIBUTE_EXPR
                || parent.children().next().as_ref() != Some(&cur)
            {
                return None;
            }
            cur = parent;
        }
        let parent = cur.parent()?;
        if parent.kind() != SyntaxKind::ATTRIBUTE_EXPR
            || parent.children().next().as_ref() != Some(&cur)
        {
            return None;
        }
        let attr = ast::AttributeExpr::cast(parent.clone())?.attr_text();
        if !names.contains(&attr) {
            names.push(attr.clone());
        }
        edits.push((parent.text_range(), attr));
    }
    names.sort();
    let _ = text;
    edits.push((
        imp.text_range(),
        format!("from {module} import {}", names.join(", ")),
    ));
    Some(Intention {
        title: format!("Convert to 'from {module} import …'"),
        edits,
    })
}

/// Moves an import statement to the top-level import block.
pub fn move_import_to_top(file: &Arc<FileInfo>, offset: TextSize) -> Option<Intention> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let (root, text) = root_and_text(file);
    let stmt = token_at(&root, offset)?.parent_ancestors().find(|n| {
        matches!(
            n.kind(),
            SyntaxKind::IMPORT_STMT | SyntaxKind::IMPORT_FROM_STMT
        )
    })?;
    if !import_actions::isolated_statement(file, &ast::Stmt::cast(stmt.clone())?) {
        return None;
    }
    let insert_at: usize = rustpy_semantic::import_placement::import_insert_offset(file).into();
    let start: usize = stmt.text_range().start().into();
    let at_top = stmt
        .parent()
        .is_some_and(|p| p.kind() == SyntaxKind::MODULE);
    if at_top && start < insert_at {
        return None;
    }
    let siblings = stmt
        .parent()
        .map(|p| {
            p.children()
                .filter(|c| ast::Stmt::can_cast(c.kind()))
                .count()
        })
        .unwrap_or(2);
    if siblings <= 1 && !at_top {
        return None;
    }
    if insert_at > start {
        return None;
    }
    let import_text = normalize(&stmt.text().to_string());
    Some(Intention {
        title: "Move import to top of file".into(),
        edits: vec![
            (rng(insert_at, insert_at), format!("{import_text}\n")),
            (stmt_lines(text, stmt.text_range()), String::new()),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn setup(src: &str) -> (Analysis, Arc<FileInfo>) {
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let f = db.file(id).unwrap();
        (db, f)
    }

    fn at(src: &str, needle: &str) -> TextSize {
        TextSize::new(src.find(needle).expect("needle") as u32)
    }

    fn run(f: IntentionFn, src: &str, needle: &str) -> String {
        let (_, file) = setup(src);
        let i = f(&file, at(src, needle))
            .unwrap_or_else(|| panic!("intention not available at {needle:?}"));
        apply_edits(src, &i.edits)
    }

    #[test]
    fn invert_split_merge_if() {
        let src = "def f(a, b):\n    if a > 1 and b:\n        x = 1\n    else:\n        x = 2\n    return x\n";
        assert_eq!(run(invert_if, src, "if a"), "def f(a, b):\n    if not (a > 1 and b):\n        x = 2\n    else:\n        x = 1\n    return x\n");
        let src2 = "def f(a):\n    if a == 1:\n        print(a)\n    return a\n";
        assert_eq!(
            run(invert_if, src2, "if a"),
            "def f(a):\n    if a != 1:\n        pass\n    else:\n        print(a)\n    return a\n"
        );
        let split = run(split_if, src2.replace("a == 1", "a and b").as_str(), "if a");
        assert_eq!(
            split,
            "def f(a):\n    if a:\n        if b:\n            print(a)\n    return a\n"
        );
        let (_, file) = setup(&split);
        let merged = apply_edits(&split, &merge_ifs(&file, at(&split, "if a")).unwrap().edits);
        assert_eq!(
            merged,
            "def f(a):\n    if a and b:\n        print(a)\n    return a\n"
        );
        let (_, file) = setup(src2);
        assert!(merge_ifs(&file, at(src2, "if a")).is_none());
        assert!(invert_if(&file, at(src2, "print")).is_none());
    }

    #[test]
    fn comparisons_and_parens() {
        let src = "if a < b:\n    pass\n";
        assert_eq!(run(flip_comparison, src, "<"), "if b > a:\n    pass\n");
        assert_eq!(run(negate_comparison, src, "<"), "if a >= b:\n    pass\n");
        let src = "if not a == b:\n    pass\n";
        assert_eq!(run(negate_comparison, src, "=="), "if a != b:\n    pass\n");
        let src = "x = (a + b)\ny = (a + b) * 2\nz = -(1)\nw = (1).real\n";
        assert_eq!(
            run(remove_redundant_parens, src, "(a + b)"),
            "x = a + b\ny = (a + b) * 2\nz = -(1)\nw = (1).real\n"
        );
        let (_, file) = setup(src);
        assert!(remove_redundant_parens(&file, at(src, "(a + b) * 2")).is_none());
        assert!(remove_redundant_parens(&file, at(src, "(1).real")).is_none());
        assert_eq!(
            run(remove_redundant_parens, src, "(1)\n"),
            "x = (a + b)\ny = (a + b) * 2\nz = -1\nw = (1).real\n"
        );
    }

    #[test]
    fn string_conversions() {
        let src = "s = 'it\\'s \"ok\"'\n";
        assert_eq!(run(convert_quotes, src, "'it"), "s = \"it's \\\"ok\\\"\"\n");
        let src = "s = \"abc\"\n";
        assert_eq!(
            run(convert_triple_quoted, src, "\"abc"),
            "s = \"\"\"abc\"\"\"\n"
        );
        assert_eq!(
            run(convert_triple_quoted, "s = \"\"\"abc\"\"\"\n", "abc"),
            "s = \"abc\"\n"
        );
        assert_eq!(run(split_string, src, "bc"), "s = \"a\" \"bc\"\n");
        assert_eq!(
            run(join_string, "s = \"a\" \"bc\"\n", "bc"),
            "s = \"abc\"\n"
        );
        let src = "s = \"%s has %d items (%.2f%%) %r\" % (name, n, pct, obj)\n";
        assert_eq!(
            run(convert_to_fstring, src, "%s"),
            "s = f\"{name!s} has {n} items ({pct:.2f}%) {obj!r}\"\n"
        );
        let src = "s = \"%5s|%-4d|%05.1f\" % (a, b, c)\n";
        assert_eq!(
            run(convert_to_fstring, src, "%5s"),
            "s = f\"{a!s:>5}|{b:<4d}|{c:05.1f}\"\n"
        );
        let src = "s = \"{} and {name}: {0:>4} {{x}}\".format(a, name=b)\n";
        assert_eq!(
            run(convert_to_fstring, src, "{}"),
            "s = f\"{a} and {b}: {a:>4} {{x}}\"\n"
        );
        let (_, file) = setup("s = \"%(k)s\" % d\n");
        assert!(convert_to_fstring(&file, at("s = \"%(k)s\" % d\n", "%(")).is_none());
    }

    #[test]
    fn collections_loops_lambdas() {
        let src = "d = dict(a=1, b=x)\n";
        assert_eq!(
            run(dict_call_to_literal, src, "dict"),
            "d = {\"a\": 1, \"b\": x}\n"
        );
        let src = "def f(xs):\n    out = []\n    for x in xs:\n        if x > 0:\n            out.append(x * 2)\n    return out\n";
        let comp = run(loop_to_comprehension, src, "for x");
        assert_eq!(
            comp,
            "def f(xs):\n    out = [x * 2 for x in xs if x > 0]\n    return out\n"
        );
        assert_eq!(run(comprehension_to_loop, &comp, "out = ["), src);
        let src = "m = {k: v for k, v in pairs}\n";
        assert_eq!(
            run(comprehension_to_loop, src, "m ="),
            "m = {}\nfor k, v in pairs:\n    m[k] = v\n"
        );
        let back = run(
            loop_to_comprehension,
            "m = {}\nfor k, v in pairs:\n    m[k] = v\n",
            "for k",
        );
        assert_eq!(back, src);
        let src = "add = lambda x, y=1: x + y\n";
        assert_eq!(
            run(lambda_to_function, src, "lambda"),
            "def add(x, y=1):\n    return x + y\n"
        );
    }

    #[test]
    fn method_conversions() {
        let src = "class C:\n    def size(self):\n        return 1\n\n    def helper(self, x):\n        return x\n\n    def broken(x):\n        return x\n\n    @staticmethod\n    def util(x):\n        return x\n\n    def use(self):\n        return self.util(1) + C.util(2)\n\nC.util(3)\n";
        assert_eq!(
            run(method_to_property, src, "def size").lines().nth(1),
            Some("    @property")
        );
        assert!(
            run(make_static, src, "def helper").contains("    @staticmethod\n    def helper(x):")
        );
        assert!(run(add_self, src, "def broken").contains("def broken(self, x):"));
        let (_, file) = setup(src);
        assert!(make_static(&file, at(src, "def use")).is_none());
        assert!(method_to_property(&file, at(src, "def helper")).is_none());
        let out = run(static_to_function, src, "def util");
        assert!(out.contains("\n\n\ndef util(x):\n    return x\n"), "{out}");
        assert!(out.contains("return util(1) + util(2)"), "{out}");
        assert!(out.ends_with("util(3)\n"), "{out}");
        assert!(!out.contains("@staticmethod"), "{out}");
        let prop = "class C:\n    @property\n    def size(self):\n        return 1\n";
        assert_eq!(
            run(property_to_method, prop, "def size"),
            "class C:\n    def size(self):\n        return 1\n"
        );
    }

    #[test]
    fn returns_and_ternaries() {
        let src =
            "def f(a):\n    if a > 1:\n        return True\n    else:\n        return False\n";
        assert_eq!(
            run(simplify_bool_return, src, "if a"),
            "def f(a):\n    return a > 1\n"
        );
        let src = "def f(a):\n    if a:\n        return False\n    return True\n";
        assert_eq!(
            run(simplify_bool_return, src, "if a"),
            "def f(a):\n    return not a\n"
        );
        let src = "x = a if c else b\n";
        assert_eq!(
            run(ternary_to_if, src, "x ="),
            "if c:\n    x = a\nelse:\n    x = b\n"
        );
        assert_eq!(
            run(
                if_to_ternary,
                "if c:\n    x = a\nelse:\n    x = b\n",
                "if c"
            ),
            src
        );
        assert_eq!(
            run(
                if_to_ternary,
                "def f():\n    if c:\n        return 1\n    else:\n        return 2\n",
                "if c"
            ),
            "def f():\n    return 1 if c else 2\n"
        );
    }

    #[test]
    fn import_conversions() {
        let src = "from os import path, sep\n\nprint(path.join(sep))\n";
        assert_eq!(
            run(from_import_to_import, src, "from os"),
            "import os\n\nprint(os.path.join(os.sep))\n"
        );
        let src = "import os.path\n\nprint(os.path.join(os.path.sep))\n";
        assert_eq!(
            run(import_to_from_import, src, "import os"),
            "from os.path import join, sep\n\nprint(join(sep))\n"
        );
        let src = "import os\n\ndef f():\n    import sys\n    return sys.argv\n";
        assert_eq!(
            run(move_import_to_top, src, "import sys"),
            "import os\nimport sys\n\ndef f():\n    return sys.argv\n"
        );
        let (_, file) = setup(src);
        assert!(move_import_to_top(&file, at(src, "import os")).is_none());
    }

    #[test]
    fn aggregator_lists_applicable_intentions() {
        let src = "if a < b:\n    return True\nelse:\n    return False\n";
        let (db, file) = setup(src);
        let titles: Vec<String> = intentions_at(&db, &file, at(src, "<"))
            .into_iter()
            .map(|i| i.title)
            .collect();
        assert!(
            titles.contains(&"Invert 'if' condition".to_string()),
            "{titles:?}"
        );
        assert!(titles.contains(&"Flip '<'".to_string()), "{titles:?}");
        assert!(
            titles.iter().any(|t| t.starts_with("Simplify to 'return")),
            "{titles:?}"
        );
    }
}
