//! Completion batch 2 (roadmap §2.4): type-aware *smart completion*
//! (Ctrl+Shift+Space), *second-press* completion that offers symbols from
//! other project modules and the stdlib together with their import edit, and
//! configurable lookup filtering (prefix / middle / camel-hump matching).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use once_cell::sync::Lazy;
use rustpy_core::{TextRange, TextSize};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};
use smol_str::SmolStr;

use crate::classes::MemberKind;
use crate::db::{Analysis, FileInfo};
use crate::features::{self, CompletionItem, CompletionKind};
use crate::infer::Inferer;
use crate::scope::{BindingKind, ScopeId};
use crate::types::Type;
use crate::typeshed;

// ---------------------------------------------------------------------------
// Prefix helpers
// ---------------------------------------------------------------------------

/// `(byte start of the identifier being typed, its text)`.
fn typed_prefix(text: &str, offset: TextSize) -> (usize, String) {
    let off = usize::from(offset).min(text.len());
    let before = &text[..off];
    let start = before
        .rfind(|c: char| !rustpy_core::is_ident_continue(c))
        .map(|i| {
            i + before[i..]
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(1)
        })
        .unwrap_or(0);
    (start, before[start..].to_string())
}

/// Text before the typed prefix on the same line, trimmed at the end.
fn line_prefix(text: &str, prefix_start: usize) -> &str {
    let line_start = text[..prefix_start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    text[line_start..prefix_start].trim_end()
}

/// The last non-trivia token that ends at or before `offset`.
fn token_before(root: &SyntaxNode, offset: TextSize) -> Option<SyntaxToken> {
    let mut tok = root.token_at_offset(offset).left_biased()?;
    loop {
        if !tok.kind().is_trivia()
            && !matches!(
                tok.kind(),
                SyntaxKind::NEWLINE | SyntaxKind::INDENT | SyntaxKind::DEDENT
            )
        {
            return Some(tok);
        }
        tok = tok.prev_token()?;
    }
}

// ---------------------------------------------------------------------------
// Expected type at the caret
// ---------------------------------------------------------------------------

/// Where the caret is and what type a completed expression should have there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExpectedContext {
    /// Argument of a call: `(callee name, parameter name)`.
    Argument(String, String),
    /// Right-hand side of an assignment to `name`.
    Assignment(String),
    /// `return` inside an annotated function.
    Return,
    /// Operand of a comparison / binary operator.
    Operand,
    /// `x in <caret>` (container expected) or `<caret> in xs` (element expected).
    Membership,
}

/// The type expected for an expression typed at `offset`, if it can be derived
/// from the surrounding call, assignment, `return`, comparison or `in` test.
pub fn expected_type(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
) -> Option<(Type, ExpectedContext)> {
    let offset = crate::clamp_offset(file, offset);
    let inf = Inferer::new(db);
    let text: &str = &file.text;
    let (prefix_start, _) = typed_prefix(text, offset);
    let anchor = TextSize::new(prefix_start as u32);
    let root = file.parse.syntax_node();
    // Candidate tokens: the identifier being typed, then the token just before it.
    let mut candidates: Vec<SyntaxToken> = Vec::new();
    if let Some(t) = root.token_at_offset(anchor).right_biased() {
        if t.kind() == SyntaxKind::IDENT && t.text_range().start() == anchor {
            candidates.push(t);
        }
    }
    if let Some(t) = token_before(&root, anchor) {
        candidates.push(t);
    }
    for tok in candidates {
        if let Some(found) = context_from_token(&inf, file, &tok, anchor) {
            if !found.0.is_unknown() {
                return Some(found);
            }
        }
    }
    // Text fallbacks for incomplete statements the parser could not shape:
    // `return `, `name = `, `name == ` / `name < ` …
    let pre = line_prefix(text, prefix_start);
    if pre == "return" || pre.ends_with(" return") {
        if let Some(t) = enclosing_return_type(&inf, file, anchor) {
            return Some((t, ExpectedContext::Return));
        }
    }
    let scope = file.scopes.scope_at(anchor);
    let trailing_ident = |s: &str| -> String {
        s.chars()
            .rev()
            .take_while(|c| rustpy_core::is_ident_continue(*c) || *c == '.')
            .collect::<String>()
            .chars()
            .rev()
            .collect()
    };
    for op in [
        "==", "!=", "<=", ">=", "<", ">", " not in", " in", " is not", " is",
    ] {
        if let Some(lhs) = pre.strip_suffix(op) {
            let name = trailing_ident(lhs.trim_end());
            if name.is_empty() || name.ends_with('.') {
                break;
            }
            let t = type_of_dotted_name(&inf, file, scope, &name)?;
            if op.trim() == "in" || op.trim() == "not in" {
                let containers: Vec<Type> = ["list", "set", "frozenset", "tuple", "dict", "str"]
                    .iter()
                    .map(|c| inf.instance_of(c))
                    .filter(|t| !t.is_unknown())
                    .collect();
                return Some((Type::union(containers), ExpectedContext::Membership));
            }
            return Some((t, ExpectedContext::Operand));
        }
    }
    if let Some(lhs) = pre.strip_suffix('=') {
        let lhs = lhs.trim_end();
        if lhs.is_empty()
            || lhs.ends_with([
                '=', '!', '<', '>', ':', '+', '-', '*', '/', '%', '&', '|', '^',
            ])
        {
            return None;
        }
        // `name: T = ` → the annotation.
        if let Some((target, ann)) = lhs.split_once(':') {
            let target = target.trim();
            if !target.is_empty()
                && target
                    .chars()
                    .all(|c| rustpy_core::is_ident_continue(c) || c == '.')
            {
                // Simple annotations (`int`, `MyClass`, `pkg.Cls`) resolve as names; the class becomes an instance.
                let t = type_of_dotted_name(&inf, file, scope, ann.trim()).map(|t| match t {
                    Type::Class(c, args) => Type::Instance(c, args),
                    other => other,
                });
                if let Some(t) = t.filter(|t| !t.is_unknown()) {
                    return Some((t, ExpectedContext::Assignment(target.to_string())));
                }
            }
        }
        let name = trailing_ident(lhs);
        if !name.is_empty() && name == lhs {
            let t = type_of_dotted_name(&inf, file, scope, &name)?;
            return Some((t, ExpectedContext::Assignment(name)));
        }
    }
    None
}

/// Type of `a.b.c` resolved from `scope` (declared annotation first for plain names).
fn type_of_dotted_name(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    scope: ScopeId,
    dotted: &str,
) -> Option<Type> {
    let mut parts = dotted.split('.');
    let first = parts.next()?;
    let mut ty = {
        let ids = file.scopes.resolve(scope, first);
        let annotated = ids.and_then(|ids| {
            ids.iter()
                .find(|id| file.scopes.binding(**id).annotation.is_some())
                .copied()
        });
        match annotated {
            Some(id) => inf.type_of_binding(file, id),
            None => {
                let def = inf.resolve_name(file, scope, first)?;
                inf.type_of_binding(&def.file, def.binding)
            }
        }
    };
    for part in parts {
        ty = inf.lookup_member(&ty, part)?;
    }
    (!ty.is_unknown()).then_some(ty)
}

fn context_from_token(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    tok: &SyntaxToken,
    anchor: TextSize,
) -> Option<(Type, ExpectedContext)> {
    for n in tok.parent_ancestors() {
        match n.kind() {
            SyntaxKind::ARG_LIST => {
                if let Some(r) = call_argument_type(inf, file, &n, anchor) {
                    return Some(r);
                }
            }
            SyntaxKind::RETURN_STMT => {
                let t = enclosing_return_type(inf, file, anchor)?;
                return Some((t, ExpectedContext::Return));
            }
            SyntaxKind::ANN_ASSIGN_STMT => {
                let stmt = ast::AnnAssignStmt::cast(n.clone())?;
                let eq = n
                    .children_with_tokens()
                    .filter_map(|e| e.into_token())
                    .find(|t| t.kind() == SyntaxKind::EQ)?;
                if anchor >= eq.text_range().end() {
                    let ann = stmt.annotation()?;
                    let target = stmt
                        .target()
                        .map(|t| t.syntax().text().to_string())
                        .unwrap_or_default();
                    return Some((
                        inf.annotation_type(file, &ann),
                        ExpectedContext::Assignment(target),
                    ));
                }
                return None;
            }
            SyntaxKind::ASSIGN_STMT => {
                let stmt = ast::AssignStmt::cast(n.clone())?;
                let eq = n
                    .children_with_tokens()
                    .filter_map(|e| e.into_token())
                    .find(|t| t.kind() == SyntaxKind::EQ)?;
                if anchor < eq.text_range().end() {
                    return None;
                }
                let target = stmt.targets().into_iter().next()?;
                let name = target.syntax().text().to_string();
                let ty = assignment_target_type(inf, file, &target, anchor)?;
                return Some((ty, ExpectedContext::Assignment(name)));
            }
            SyntaxKind::COMPARE_EXPR => {
                let cmp = ast::CompareExpr::cast(n.clone())?;
                let operands: Vec<ast::Expr> = cmp.operands().collect();
                let ops: Vec<String> = cmp.ops().map(|o| o.text()).collect();
                // Position of the caret among the operands.
                let pos = operands
                    .iter()
                    .position(|o| o.syntax().text_range().contains_inclusive(anchor))
                    .unwrap_or(operands.len());
                let before_is_in =
                    pos > 0 && ops.get(pos - 1).is_some_and(|o| o == "in" || o == "not in");
                let after_is_in = ops.get(pos).is_some_and(|o| o == "in" || o == "not in");
                if before_is_in {
                    // `<elem> in <caret>`: a container.
                    let containers: Vec<Type> =
                        ["list", "set", "frozenset", "tuple", "dict", "str"]
                            .iter()
                            .map(|c| inf.instance_of(c))
                            .filter(|t| !t.is_unknown())
                            .collect();
                    return Some((Type::union(containers), ExpectedContext::Membership));
                }
                if after_is_in {
                    // `<caret> in xs`: an element of xs.
                    let container = operands.get(pos + 1)?;
                    let t = inf.iter_type(&inf.type_of_expr(file, container));
                    return Some((t, ExpectedContext::Membership));
                }
                let other = if pos > 0 {
                    operands.get(pos - 1)
                } else {
                    operands.get(pos + 1).or(operands.first())
                }?;
                if other.syntax().text_range().contains_inclusive(anchor) {
                    return None;
                }
                let t = inf.type_of_expr(file, other);
                return Some((t, ExpectedContext::Operand));
            }
            SyntaxKind::BINARY_EXPR => {
                let bin = ast::BinaryExpr::cast(n.clone())?;
                let (lhs, rhs) = (bin.lhs(), bin.rhs());
                let other = match (&lhs, &rhs) {
                    (Some(l), _)
                        if !l.syntax().text_range().contains_inclusive(anchor)
                            && anchor >= l.syntax().text_range().end() =>
                    {
                        Some(l.clone())
                    }
                    (_, Some(r)) if !r.syntax().text_range().contains_inclusive(anchor) => {
                        Some(r.clone())
                    }
                    (Some(l), _) if !l.syntax().text_range().contains_inclusive(anchor) => {
                        Some(l.clone())
                    }
                    _ => None,
                }?;
                let t = inf.type_of_expr(file, &other);
                return Some((t, ExpectedContext::Operand));
            }
            SyntaxKind::FUNC_DEF
            | SyntaxKind::CLASS_DEF
            | SyntaxKind::LAMBDA_EXPR
            | SyntaxKind::BLOCK => return None,
            _ => {}
        }
    }
    None
}

/// Type of the parameter that the argument at `anchor` fills.
fn call_argument_type(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    arg_list: &SyntaxNode,
    anchor: TextSize,
) -> Option<(Type, ExpectedContext)> {
    let open = arg_list
        .first_token()
        .filter(|t| t.kind() == SyntaxKind::L_PAREN)?;
    if anchor < open.text_range().end() {
        return None;
    }
    if let Some(close) = arg_list
        .last_token()
        .filter(|t| t.kind() == SyntaxKind::R_PAREN)
    {
        if anchor > close.text_range().start() {
            return None;
        }
    }
    // The overload the arguments typed so far select; keywords map by name.
    let ctx = crate::call_help::call_context_in(inf, file, arg_list, anchor)?;
    let (name, ty) = ctx.expected_parameter(inf)?;
    Some((ty, ExpectedContext::Argument(ctx.name, name.to_string())))
}

fn enclosing_return_type(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    anchor: TextSize,
) -> Option<Type> {
    let root = file.parse.syntax_node();
    let tok = root
        .token_at_offset(anchor)
        .left_biased()
        .or_else(|| root.token_at_offset(anchor).right_biased())?;
    let func = tok
        .parent_ancestors()
        .find(|n| n.kind() == SyntaxKind::FUNC_DEF)?;
    let f = ast::FuncDef::cast(func)?;
    let ann = f.return_annotation()?;
    Some(inf.annotation_type(file, &ann))
}

/// Type a plain assignment target already has: a declared annotation or the
/// type of an earlier assignment (the current statement is ignored).
fn assignment_target_type(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    target: &ast::Expr,
    anchor: TextSize,
) -> Option<Type> {
    match target {
        ast::Expr::Name(n) => {
            let name = n.name_text();
            let scope = file.scopes.scope_at(anchor);
            let ids = file.scopes.resolve(scope, &name)?;
            let bindings: Vec<_> = ids
                .iter()
                .map(|id| (*id, file.scopes.binding(*id)))
                .filter(|(_, b)| b.is_definition() && !b.node_range.contains_inclusive(anchor))
                .collect();
            if let Some((id, _)) = bindings.iter().find(|(_, b)| b.annotation.is_some()) {
                let t = inf.type_of_binding(file, *id);
                if !t.is_unknown() {
                    return Some(t);
                }
            }
            let (id, _) = bindings
                .iter()
                .rev()
                .find(|(_, b)| b.range.start() < anchor)?;
            let t = inf.type_of_binding(file, *id);
            (!t.is_unknown()).then_some(t)
        }
        other => {
            let t = inf.type_of_expr(file, other);
            (!t.is_unknown()).then_some(t)
        }
    }
}

// ---------------------------------------------------------------------------
// Smart completion
// ---------------------------------------------------------------------------

/// Value type an item produces once inserted: functions are inserted as calls
/// and yield their return type; a bare class name stays a class object (the
/// constructor call is offered separately).
fn value_type(ty: &Type) -> Type {
    match ty {
        Type::Function(sig) | Type::BoundMethod(sig, _) | Type::Callable(sig) => sig.ret.clone(),
        Type::Overloaded(v) => v.first().map(|s| s.ret.clone()).unwrap_or(Type::Unknown),
        other => other.clone(),
    }
}

/// Base type for `expr.<prefix>` member completion, when the caret follows a dot.
fn member_base(inf: &Inferer<'_>, file: &Arc<FileInfo>, prefix_start: usize) -> Option<Type> {
    let text: &str = &file.text;
    let pre = line_prefix(text, prefix_start);
    let stripped = pre.strip_suffix('.')?;
    if stripped.ends_with('.') {
        return None;
    }
    let line_start = text[..prefix_start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let dot_off = TextSize::new((line_start + pre.len() - 1) as u32);
    let root = file.parse.syntax_node();
    let obj = root
        .token_at_offset(dot_off)
        .left_biased()
        .and_then(|t| t.parent())
        .and_then(|n| {
            n.ancestors().find_map(|a| {
                if a.kind() == SyntaxKind::ATTRIBUTE_EXPR
                    && a.children_with_tokens()
                        .filter_map(|e| e.into_token())
                        .any(|t| t.kind() == SyntaxKind::DOT && t.text_range().start() == dot_off)
                {
                    return ast::AttributeExpr::cast(a).and_then(|x| x.object());
                }
                None
            })
        })
        .or_else(|| {
            root.token_at_offset(dot_off)
                .left_biased()
                .and_then(|t| t.parent())
                .and_then(|n| {
                    n.ancestors()
                        .filter_map(ast::Expr::cast)
                        .find(|e| e.text_range().end() == dot_off)
                })
        })?;
    Some(inf.type_of_expr(file, &obj))
}

fn item_type(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    scope: ScopeId,
    base: Option<&Type>,
    item: &CompletionItem,
) -> Option<Type> {
    match item.kind {
        CompletionKind::Keyword => match item.label.as_str() {
            "None" => Some(Type::None),
            "True" | "False" => Some(inf.instance_of("bool")),
            _ => None,
        },
        CompletionKind::Template | CompletionKind::Postfix => None,
        _ => {
            let ty = match base {
                Some(b) => inf.lookup_member(b, &item.label)?,
                None => {
                    let def = inf.resolve_name(file, scope, &item.label)?;
                    inf.type_of_binding(&def.file, def.binding)
                }
            };
            let v = value_type(&ty);
            // `Any`/unknown results (e.g. `eval()`) are never ranked as compatible.
            (!v.is_unknown() && !matches!(v, Type::Any | Type::TypeVar(_))).then_some(v)
        }
    }
}

/// Whether `name` resolves to the class `c` from `scope` (so a constructor call can be suggested).
fn class_in_scope(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    scope: ScopeId,
    c: &crate::types::ClassRef,
) -> bool {
    if inf.builtin_class(&c.name).as_ref() == Some(c) {
        return true;
    }
    let Some(def) = inf.resolve_name(file, scope, &c.name) else {
        return false;
    };
    matches!(inf.type_of_binding(&def.file, def.binding), Type::Class(ref found, _) if found == c)
}

/// Finite plain positional callables have a direct Python lambda header.
/// Optional parameters use an editable ellipsis default rather than copying
/// arbitrary declaration expressions into a new evaluation context.
fn lambda_skeleton(signature: &crate::types::Signature) -> Option<String> {
    use crate::types::ParamKind;
    use unicode_normalization::UnicodeNormalization;
    if signature.is_async
        || signature.params.len() > 32
        || signature
            .params
            .iter()
            .any(|p| !matches!(p.kind, ParamKind::Normal | ParamKind::PositionalOnly))
    {
        return None;
    }
    const KEYWORDS: &[&str] = &[
        "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class",
        "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global",
        "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return",
        "try", "while", "with", "yield",
    ];
    let mut names = std::collections::HashSet::new();
    for parameter in signature
        .params
        .iter()
        .filter(|p| p.kind == ParamKind::Normal)
    {
        let name = parameter.name.as_str();
        let mut chars = name.chars();
        if !chars.next().is_some_and(rustpy_core::is_ident_start)
            || !chars.all(rustpy_core::is_ident_continue)
            || KEYWORDS.contains(&name)
            || !names.insert(name.nfkc().collect::<String>())
        {
            return None;
        }
    }
    let mut parts = Vec::new();
    let mut normal_seen = false;
    let mut default_seen = false;
    for (index, parameter) in signature.params.iter().enumerate() {
        if parameter.kind == ParamKind::PositionalOnly && normal_seen {
            return None;
        }
        normal_seen |= parameter.kind == ParamKind::Normal;
        if default_seen && !parameter.has_default {
            return None;
        }
        default_seen |= parameter.has_default;
        let name = if parameter.kind == ParamKind::Normal {
            parameter.name.to_string()
        } else {
            let mut name = if signature.params.len() == 1 {
                "value".to_owned()
            } else {
                format!("value{}", index + 1)
            };
            while names.contains(&name.nfkc().collect::<String>()) {
                name.push('_');
            }
            names.insert(name.nfkc().collect::<String>());
            name
        };
        parts.push(if parameter.has_default {
            format!("{name}=...")
        } else {
            name
        });
        if parameter.kind == ParamKind::PositionalOnly
            && signature
                .params
                .get(index + 1)
                .is_none_or(|next| next.kind != ParamKind::PositionalOnly)
        {
            parts.push("/".to_owned());
        }
    }
    Some(if parts.is_empty() {
        "lambda: ".to_owned()
    } else {
        format!("lambda {}: ", parts.join(", "))
    })
}

/// Extra lookup items derived from the expected type (`None`, `True`/`False`,
/// literal skeletons, constructor calls, enum members).
fn suggestions_for(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    scope: ScopeId,
    expected: &Type,
    prefix: &str,
) -> Vec<CompletionItem> {
    let mut out: Vec<CompletionItem> = Vec::new();
    let lower = prefix.to_lowercase();
    let mut push =
        |label: &str, kind: CompletionKind, detail: &str, insert: &str, caret: Option<usize>| {
            if features::matches_prefix(label, &lower) && !out.iter().any(|i| i.label == label) {
                out.push(CompletionItem {
                    label: SmolStr::new(label),
                    kind,
                    detail: detail.to_string(),
                    insert: insert.to_string(),
                    caret_offset: caret,
                    priority: 0,
                });
            }
        };
    let members: Vec<Type> = match expected {
        Type::Union(ts) => ts.clone(),
        t => vec![t.clone()],
    };
    for t in &members {
        match t {
            Type::Callable(signature) => {
                if let Some(insert) = lambda_skeleton(signature) {
                    let label = format!("{insert}…");
                    push(
                        &label,
                        CompletionKind::Keyword,
                        "editable callable body",
                        &insert,
                        Some(insert.len()),
                    );
                }
            }
            Type::None => push("None", CompletionKind::Keyword, "", "None", None),
            Type::Instance(c, _) => match c.name.as_str() {
                "bool" => {
                    push("True", CompletionKind::Keyword, "bool", "True", None);
                    push("False", CompletionKind::Keyword, "bool", "False", None);
                }
                "str" => push("\"\"", CompletionKind::Keyword, "str", "\"\"", Some(1)),
                "int" => push("0", CompletionKind::Keyword, "int", "0", None),
                "float" => push("0.0", CompletionKind::Keyword, "float", "0.0", None),
                "list" => push("[]", CompletionKind::Keyword, "list", "[]", Some(1)),
                "dict" => push("{}", CompletionKind::Keyword, "dict", "{}", Some(1)),
                "set" => push("set()", CompletionKind::Keyword, "set", "set()", None),
                "tuple" => push("()", CompletionKind::Keyword, "tuple", "()", Some(1)),
                "bytes" => push("b\"\"", CompletionKind::Keyword, "bytes", "b\"\"", Some(2)),
                "object" | "Any" => {}
                _ => {
                    let info = inf.class_info(c);
                    if let Some(info) = info.filter(|i| i.is_enum_like) {
                        for m in info.members.values() {
                            if m.kind == MemberKind::ClassVar && !m.name.starts_with('_') {
                                let label = format!("{}.{}", c.name, m.name);
                                push(&label, CompletionKind::Constant, &c.name, &label, None);
                            }
                        }
                    } else if class_in_scope(inf, file, scope, c) {
                        let has_params = inf
                            .constructor(c)
                            .and_then(|t| {
                                t.signature()
                                    .map(|s| s.params.iter().any(|p| p.name != "self"))
                            })
                            .unwrap_or(false);
                        let insert = format!("{}()", c.name);
                        let caret = if has_params {
                            Some(c.name.len() + 1)
                        } else {
                            None
                        };
                        push(
                            &insert,
                            CompletionKind::Class,
                            &format!("new {}", c.name),
                            &insert,
                            caret,
                        );
                    }
                }
            },
            Type::Literal(crate::types::LiteralValue::Str(s)) => {
                let lit = format!("\"{s}\"");
                push(&lit, CompletionKind::Keyword, "str", &lit, None);
            }
            Type::Literal(crate::types::LiteralValue::Int(i)) => {
                let lit = i.to_string();
                push(&lit, CompletionKind::Keyword, "int", &lit, None);
            }
            Type::Literal(crate::types::LiteralValue::Bool(b)) => {
                let lit = if *b { "True" } else { "False" };
                push(lit, CompletionKind::Keyword, "bool", lit, None);
            }
            _ => {}
        }
    }
    out
}

/// Await is legal only in the nearest ordinary asynchronous function body.
/// A class or lambda is a boundary even inside an asynchronous function.
fn async_completion_scope(file: &Arc<FileInfo>, mut scope: ScopeId, offset: TextSize) -> bool {
    for _ in 0..64 {
        let current = file.scopes.scope(scope);
        match current.kind {
            crate::scope::ScopeKind::Function => {
                if !current.is_async {
                    return false;
                }
                return file
                    .parse
                    .syntax_node()
                    .token_at_offset(offset)
                    .left_biased()
                    .and_then(|token| token.parent())
                    .and_then(|node| node.ancestors().find_map(ast::FuncDef::cast))
                    .filter(|function| function.text_range() == current.range)
                    .and_then(|function| function.body())
                    .is_some_and(|body| body.text_range().contains(offset));
            }
            crate::scope::ScopeKind::Class
            | crate::scope::ScopeKind::Lambda
            | crate::scope::ScopeKind::Module => return false,
            _ => {}
        }
        let Some(parent) = current.parent else {
            return false;
        };
        scope = parent;
    }
    false
}

fn canonical_awaitable(inf: &Inferer<'_>, ty: &Type) -> bool {
    let Type::Instance(class, arguments) = ty else {
        return false;
    };
    let path = match class.name.as_str() {
        "Coroutine" if arguments.len() == 3 => "<typeshed>/typing.pyi",
        "Awaitable" if arguments.len() == 1 => "<typeshed>/typing.pyi",
        "Future" | "Task" if arguments.len() == 1 => "<typeshed>/_asyncio.pyi",
        _ => return false,
    };
    let Some(file) = inf.db.file(class.file) else {
        return false;
    };
    if !file.is_stub || file.path.as_path() != Path::new(path) {
        return false;
    }
    inf.lookup_member(ty, "__await__").is_some()
}

/// A single local direct-body definition dominates a later body completion.
/// Refuse branch/loop/captured/module definitions and any alternate writes.
fn stable_local_completion_binding(
    file: &Arc<FileInfo>,
    scope: ScopeId,
    name: &str,
    offset: TextSize,
) -> Option<crate::scope::BindingId> {
    let current = file.scopes.scope(scope);
    if current.kind != crate::scope::ScopeKind::Function {
        return None;
    }
    if current.globals.iter().any(|n| n == name) || current.nonlocals.iter().any(|n| n == name) {
        return None;
    }
    if file.scopes.scopes.len() > 8192 {
        return None;
    }
    // A nested function can write this captured cell when called. Do not
    // pretend the outer scope's sole lexical assignment remains current.
    for candidate in &file.scopes.scopes {
        if !candidate.nonlocals.iter().any(|n| n == name)
            && !candidate.globals.iter().any(|n| n == name)
        {
            continue;
        }
        let mut parent = candidate.parent;
        let mut resolved = false;
        for _ in 0..64 {
            let Some(id) = parent else {
                resolved = true;
                break;
            };
            if id == scope {
                return None;
            }
            parent = file.scopes.scope(id).parent;
        }
        if !resolved {
            return None;
        }
    }
    let bindings = current.bindings.get(name)?;
    if bindings.len() != 1 {
        return None;
    }
    let id = bindings[0];
    let binding = file.scopes.binding(id);
    if binding.scope != scope || binding.node_range.end() > offset {
        return None;
    }
    match binding.kind {
        BindingKind::Param => Some(id),
        BindingKind::Assignment => {
            let node = file
                .parse
                .syntax_node()
                .token_at_offset(binding.range.start())
                .right_biased()?
                .parent()?;
            let statement = node.ancestors().find_map(ast::AssignStmt::cast)?;
            let block = statement.syntax().parent()?;
            let function = block.parent().and_then(ast::FuncDef::cast)?;
            (function.text_range() == current.range
                && function.body().is_some_and(|body| body.syntax() == &block))
            .then_some(id)
        }
        _ => None,
    }
}

/// Read original-AST reaching values at the cursor's evaluation phase.
fn current_completion_variables(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    offset: TextSize,
    _prefix_start: usize,
    items: &[CompletionItem],
) -> std::collections::HashMap<SmolStr, Type> {
    let mut out = std::collections::HashMap::new();
    let epoch = inf.db.revision_excluding(&file.path);
    let mut names = Vec::new();
    for item in items {
        if !matches!(
            item.kind,
            CompletionKind::Variable | CompletionKind::Parameter
        ) {
            continue;
        }
        if !names.contains(&item.label) {
            names.push(item.label.clone());
        }
        if names.len() > 128 {
            return out;
        }
    }
    let Some(scope) = crate::flow::flow_scope(file, file.scopes.scope_at(offset)) else {
        return out;
    };
    let Some(reaches) = crate::flow::reaching_at_with_db(inf.db, file, scope, offset, &names)
    else {
        return out;
    };
    for (name, values) in reaches {
        if values.is_empty() || values.iter().any(|reach| reach.binding.is_none()) {
            continue;
        }
        if let Some(ty) = inf.reach_type(file, file.scopes.scope_at(offset), &name, &values) {
            if !ty.is_unknown() && !matches!(ty, Type::Any) {
                out.insert(name, ty);
            }
        }
    }
    if inf.db.revision_excluding(&file.path) != epoch
        || !inf
            .db
            .file(file.id)
            .is_some_and(|current| Arc::ptr_eq(&current, file))
    {
        out.clear();
    }
    out
}

/// *Smart Type Completion*: the regular lookup reordered so that
/// items compatible with the expected type come first (priority 0), followed
/// by type-derived suggestions, then everything else.
pub fn smart_complete(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
) -> Vec<CompletionItem> {
    smart_complete_with(db, file, offset, None)
}

/// [`smart_complete`] for interactive lookups: candidate and expected-type
/// inference stop after `budget` or once `token` is cancelled (see
/// [`features::complete_bounded`]).
pub fn smart_complete_bounded(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    budget: std::time::Duration,
    token: Option<rustpy_core::CancellationToken>,
) -> Vec<CompletionItem> {
    smart_complete_with(
        db,
        file,
        offset,
        Some((std::time::Instant::now() + budget, token)),
    )
}

fn smart_complete_with(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    budget: Option<(std::time::Instant, Option<rustpy_core::CancellationToken>)>,
) -> Vec<CompletionItem> {
    let offset = crate::clamp_offset(file, offset);
    let base = match &budget {
        Some((deadline, token)) => features::complete_bounded(
            db,
            file,
            offset,
            deadline.saturating_duration_since(std::time::Instant::now()),
            token.clone(),
        ),
        None => features::complete(db, file, offset),
    };
    // Comments, strings and new-name positions: no expected-type suggestions.
    if !features::completion_context(&file.text, offset).is_code() {
        return base;
    }
    let Some((expected, _ctx)) = expected_type(db, file, offset) else {
        return base;
    };
    if expected.is_unknown() || matches!(expected, Type::Any) {
        return base;
    }
    let inf = match budget {
        Some((deadline, token)) => Inferer::new(db).with_budget(deadline, token),
        None => Inferer::new(db),
    };
    let text: &str = &file.text;
    let (prefix_start, prefix) = typed_prefix(text, offset);
    let scope = file.scopes.scope_at(offset);
    let member = member_base(&inf, file, prefix_start);
    let current_variables = if member.is_none() {
        current_completion_variables(&inf, file, offset, prefix_start, &base)
    } else {
        std::collections::HashMap::new()
    };
    let mut compatible: Vec<CompletionItem> = Vec::new();
    let mut rest: Vec<CompletionItem> = Vec::new();
    let mut awaited = Vec::new();
    let await_allowed = member.is_none() && async_completion_scope(file, scope, offset);

    for mut item in base {
        if await_allowed
            && matches!(
                item.kind,
                CompletionKind::Variable | CompletionKind::Parameter
            )
            && features::matches_prefix(&item.label, &prefix.to_lowercase())
        {
            if let Some(binding) = stable_local_completion_binding(file, scope, &item.label, offset)
            {
                let ty = inf.type_of_binding(file, binding);
                if canonical_awaitable(&inf, &ty) {
                    let result = inf.await_type(&ty);
                    if !result.is_unknown()
                        && !matches!(result, Type::Any)
                        && inf.is_compatible(&result, &expected)
                    {
                        let insert = format!("await {}", item.label);
                        awaited.push(CompletionItem {
                            label: SmolStr::new(&insert),
                            kind: CompletionKind::Keyword,
                            detail: format!("awaited {result}"),
                            insert,
                            caret_offset: None,
                            priority: 0,
                        });
                    }
                }
            }
        }
        if member.is_none()
            && matches!(
                item.kind,
                CompletionKind::Variable | CompletionKind::Parameter
            )
            && !features::is_keyword_argument(&item)
        {
            if let Some(current) = current_variables.get(&item.label) {
                // Display the same original-AST value used for ranking. Keep
                // existing callable/class presentation and insertion metadata.
                item.detail = match current {
                    Type::Function(signature) | Type::BoundMethod(signature, _) => {
                        format!("({}) -> {}", signature.display_params(), signature.ret)
                    }
                    Type::Overloaded(signatures) => signatures
                        .first()
                        .map(|signature| {
                            format!("({}) -> {}", signature.display_params(), signature.ret)
                        })
                        .unwrap_or_default(),
                    Type::Class(class, _) => format!("class {}", class.name),
                    Type::Module(_) => "module".to_owned(),
                    Type::Unknown => String::new(),
                    other => other.to_string(),
                };
            } else {
                // No current-flow proof means a lexical detail would falsely
                // advertise a known current type. The symbol remains in lookup.
                item.detail.clear();
            }
        }
        let current_type = if member.is_none()
            && matches!(
                item.kind,
                CompletionKind::Variable | CompletionKind::Parameter
            ) {
            current_variables
                .get(&item.label)
                .map(value_type)
                .filter(|ty| !ty.is_unknown() && !matches!(ty, Type::Any | Type::TypeVar(_)))
        } else {
            item_type(&inf, file, scope, member.as_ref(), &item)
        };
        match current_type {
            Some(t) if inf.is_compatible(&t, &expected) => {
                item.priority = 0;
                compatible.push(item);
            }
            _ => {
                item.priority = item.priority.saturating_add(10);
                rest.push(item);
            }
        }
    }
    let extras = if member.is_some() {
        Vec::new()
    } else {
        suggestions_for(&inf, file, scope, &expected, &prefix)
    };
    let mut out = compatible;
    for e in extras.into_iter().chain(awaited) {
        if !out.iter().any(|i| i.label == e.label) {
            out.push(e);
        }
    }
    rest.retain(|r| !out.iter().any(|i| i.label == r.label));
    out.extend(rest);
    out
}

// ---------------------------------------------------------------------------
// Second-press completion (names from other modules + import)
// ---------------------------------------------------------------------------

/// A lookup item that needs an import to be usable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportCompletion {
    pub item: CompletionItem,
    /// Module to import from (or to import, when `is_module`).
    pub module: String,
    /// Symbol name (empty when `is_module`).
    pub name: String,
    /// `import module` rather than `from module import name`.
    pub is_module: bool,
}

/// A name that another source (the interpreter's symbol index) says can be
/// imported: `from <module> import <name>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportableName {
    pub module: String,
    pub name: String,
    pub kind: CompletionKind,
}

/// Public names from every bundled module, including third-party stubs.
/// Built once on the import worker, never by painting the lookup.
static STDLIB_NAMES: Lazy<Vec<(String, String, CompletionKind)>> = Lazy::new(|| {
    let mut out = Vec::new();
    for module in typeshed::all_modules() {
        let Some(src) = typeshed::stub_for_module(&module) else {
            continue;
        };
        let parse = rustpy_syntax::parse(src);
        let scopes = crate::scope::build(&parse);
        let scope = scopes.module_scope();
        for (name, ids) in &scopes.scope(scope).bindings {
            if name.starts_with('_') {
                continue;
            }
            let Some(id) = ids
                .iter()
                .rev()
                .find(|id| scopes.binding(**id).is_definition())
            else {
                continue;
            };
            let kind = match &scopes.binding(*id).kind {
                BindingKind::Class => CompletionKind::Class,
                BindingKind::Function => CompletionKind::Function,
                BindingKind::Assignment | BindingKind::Annotation | BindingKind::TypeAlias => {
                    if name
                        .chars()
                        .all(|c| c.is_uppercase() || c == '_' || c.is_ascii_digit())
                    {
                        CompletionKind::Constant
                    } else {
                        CompletionKind::Variable
                    }
                }
                // Explicit re-exports (`from x import y as y`) are part of the module API.
                BindingKind::Import(info) if info.alias.as_deref() == Some(name.as_str()) => {
                    CompletionKind::Variable
                }
                _ => continue,
            };
            out.push((module.to_string(), name.to_string(), kind));
        }
    }
    out
});

fn binding_kind_to_completion(kind: &BindingKind, name: &str, ty: &Type) -> Option<CompletionKind> {
    Some(match (kind, ty) {
        (BindingKind::Class, _) | (_, Type::Class(..)) => CompletionKind::Class,
        (BindingKind::Function, _) | (_, Type::Function(_) | Type::Overloaded(_)) => {
            CompletionKind::Function
        }
        (BindingKind::Assignment | BindingKind::Annotation | BindingKind::TypeAlias, _) => {
            if name
                .chars()
                .all(|c| c.is_uppercase() || c == '_' || c.is_ascii_digit())
                && name.len() > 1
            {
                CompletionKind::Constant
            } else {
                CompletionKind::Variable
            }
        }
        _ => return None,
    })
}

/// Whether `name` is already bound (or a builtin) at `scope`.
fn is_visible(db: &Analysis, file: &FileInfo, scope: ScopeId, name: &str) -> bool {
    if file.scopes.resolve(scope, name).is_some_and(|ids| {
        ids.iter()
            .any(|id| file.scopes.binding(*id).is_definition())
    }) {
        return true;
    }
    db.builtins().is_some_and(|b| {
        !b.scopes
            .bindings_in(b.scopes.module_scope(), name)
            .is_empty()
    })
}

/// Second Ctrl+Space: symbols from other project modules and the stdlib that
/// match the typed prefix and are not yet imported, each with the module to
/// import them from (see [`import_edit_for`]).
pub fn second_press_complete(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    project_files: &[PathBuf],
) -> Vec<ImportCompletion> {
    second_press_complete_with(db, file, offset, project_files, &[])
}

/// [`second_press_complete`] that also offers `library` names (installed
/// packages and the rest of the standard library, from the interpreter's
/// symbol index), ranked after project and curated standard library names.
pub fn second_press_complete_with(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    project_files: &[PathBuf],
    library: &[ImportableName],
) -> Vec<ImportCompletion> {
    import_complete_with(db, file, offset, project_files, library, false)
}
/// Basic completion's class candidates use the same export/import authority,
/// filtering before the result cap instead of after unrelated function hits.
pub fn class_name_complete_with(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    project_files: &[PathBuf],
    library: &[ImportableName],
) -> Vec<ImportCompletion> {
    import_complete_with(db, file, offset, project_files, library, true)
}
fn import_complete_with(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    project_files: &[PathBuf],
    library: &[ImportableName],
    classes_only: bool,
) -> Vec<ImportCompletion> {
    let offset = crate::clamp_offset(file, offset);
    let text: &str = &file.text;
    let (_, prefix) = typed_prefix(text, offset);
    if !features::completion_context(text, offset).is_code() {
        return Vec::new();
    }
    let lower = prefix.to_lowercase();
    let scope = file.scopes.scope_at(offset);
    let inf = Inferer::new(db);
    let mut out: Vec<ImportCompletion> = Vec::new();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    // Project modules.
    for path in project_files {
        if !matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("py" | "pyi")
        ) {
            continue;
        }
        let Some(module) = db.resolver.module_name_of(path) else {
            continue;
        };
        if file.module_name.as_deref() == Some(module.as_str()) || path == &file.path {
            continue;
        }
        let Some(other) = ensure_loaded(db, path) else {
            continue;
        };
        let ms = other.scopes.module_scope();
        for (name, ids) in &other.scopes.scope(ms).bindings {
            if name.starts_with('_')
                || !features::matches_prefix(name, &lower)
                || is_visible(db, file, scope, name)
            {
                continue;
            }
            let Some(id) = ids
                .iter()
                .rev()
                .find(|id| other.scopes.binding(**id).is_definition())
            else {
                continue;
            };
            let b = other.scopes.binding(*id);
            let ty = inf.type_of_binding(&other, *id);
            let Some(kind) = binding_kind_to_completion(&b.kind, name, &ty) else {
                continue;
            };
            if !seen.insert((module.clone(), name.to_string())) {
                continue;
            }
            let (insert, caret) = insert_text(kind, name, &ty);
            let detail = format!("from {module}");
            out.push(ImportCompletion {
                item: CompletionItem {
                    label: name.clone(),
                    kind,
                    detail,
                    insert,
                    caret_offset: caret,
                    priority: 20,
                },
                module: module.clone(),
                name: name.to_string(),
                is_module: false,
            });
        }
    }
    // Installed metadata wins over bundled fallback kinds and signatures.
    for c in library {
        if c.name.starts_with('_')
            || file.module_name.as_deref() == Some(c.module.as_str())
            || !features::matches_prefix(&c.name, &lower)
            || is_visible(db, file, scope, &c.name)
            || !seen.insert((c.module.clone(), c.name.clone()))
        {
            continue;
        }
        let callable = c.kind == CompletionKind::Function;
        let insert = if callable {
            format!("{}()", c.name)
        } else {
            c.name.clone()
        };
        let caret = callable.then_some(c.name.len() + 1);
        out.push(ImportCompletion {
            item: CompletionItem {
                label: SmolStr::new(&c.name),
                kind: c.kind,
                detail: format!("from {}", c.module),
                insert,
                caret_offset: caret,
                priority: 35,
            },
            module: c.module.clone(),
            name: c.name.clone(),
            is_module: false,
        });
    }
    // Bundled standard-library and third-party symbols.
    for (module, name, kind) in STDLIB_NAMES.iter() {
        if !features::matches_prefix(name, &lower)
            || is_visible(db, file, scope, name)
            || !seen.insert((module.clone(), name.clone()))
        {
            continue;
        }
        let insert = match kind {
            CompletionKind::Function => format!("{name}()"),
            _ => name.clone(),
        };
        let caret = (*kind == CompletionKind::Function).then_some(name.len() + 1);
        out.push(ImportCompletion {
            item: CompletionItem {
                label: SmolStr::new(name),
                kind: *kind,
                detail: format!("from {module}"),
                insert,
                caret_offset: caret,
                priority: if typeshed::is_stdlib(module) { 30 } else { 36 },
            },
            module: module.clone(),
            name: name.clone(),
            is_module: false,
        });
    }
    // Modules themselves (`import m`).
    for m in db.resolver.top_level_modules() {
        if m.starts_with('_')
            || !features::matches_prefix(&m, &lower)
            || is_visible(db, file, scope, &m)
        {
            continue;
        }
        out.push(ImportCompletion {
            item: CompletionItem {
                label: SmolStr::new(&m),
                kind: CompletionKind::Module,
                detail: "module".into(),
                insert: m.clone(),
                caret_offset: None,
                priority: 40,
            },
            module: m.clone(),
            name: String::new(),
            is_module: true,
        });
    }
    if classes_only {
        out.retain(|candidate| candidate.item.kind == CompletionKind::Class);
    }
    out.sort_by(|a, b| {
        let a_exact = a.item.label.to_lowercase().starts_with(&lower);
        let b_exact = b.item.label.to_lowercase().starts_with(&lower);
        b_exact
            .cmp(&a_exact)
            .then(a.item.priority.cmp(&b.item.priority))
            .then_with(|| {
                a.item
                    .label
                    .to_lowercase()
                    .cmp(&b.item.label.to_lowercase())
            })
            .then_with(|| a.module.cmp(&b.module))
    });
    out.truncate(200);
    out
}

fn insert_text(kind: CompletionKind, name: &str, ty: &Type) -> (String, Option<usize>) {
    match kind {
        CompletionKind::Function => {
            let no_params = ty.signature().map(|s| s.params.is_empty()).unwrap_or(false);
            (
                format!("{name}()"),
                if no_params {
                    None
                } else {
                    Some(name.len() + 1)
                },
            )
        }
        _ => (name.to_string(), None),
    }
}

/// The edit that imports `name` from `module` (or `import module` when `name`
/// is empty): merged into an existing `from module import …` statement when
/// there is one, otherwise a new statement after the last top-level import.
/// Returns an empty edit when the import already exists.
pub fn import_edit_for(file: &FileInfo, module: &str, name: &str) -> (TextRange, String) {
    let noop = (TextRange::empty(TextSize::new(0)), String::new());
    let root = file.parse.syntax_node();
    let Some(m) = ast::Module::cast(root) else {
        return noop;
    };
    if name.is_empty() {
        for stmt in m.statements() {
            if let ast::Stmt::Import(imp) = &stmt {
                if imp
                    .aliases()
                    .any(|a| a.name() == module && a.alias().is_none())
                {
                    return noop;
                }
            }
        }
        return (
            TextRange::empty(crate::import_placement::import_insert_offset(file)),
            format!("import {module}\n"),
        );
    }
    for stmt in m.statements() {
        let ast::Stmt::ImportFrom(imp) = &stmt else {
            continue;
        };
        if imp.level() != 0
            || imp.module().map(|d| d.path()).as_deref() != Some(module)
            || imp.is_star()
        {
            continue;
        }
        let aliases: Vec<ast::ImportAlias> = imp.aliases().collect();
        if aliases.iter().any(|a| a.name() == name) {
            return noop;
        }
        if let Some(last) = aliases.last() {
            return (
                TextRange::empty(last.syntax().text_range().end()),
                format!(", {name}"),
            );
        }
    }
    (
        TextRange::empty(crate::import_placement::import_insert_offset(file)),
        format!("from {module} import {name}\n"),
    )
}

// ---------------------------------------------------------------------------
// Lookup filtering
// ---------------------------------------------------------------------------

/// Settings › Editor › General › Code Completion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionSettings {
    /// Pop the lookup up automatically while typing.
    pub show_as_you_type: bool,
    /// Prefix matching respects case (humps still match on hump letters).
    pub case_sensitive: bool,
    /// Also match the prefix in the middle of a name.
    pub match_middle: bool,
    /// Hide suggestions whose label is already fully typed.
    pub hide_used: bool,
}

impl Default for CompletionSettings {
    fn default() -> Self {
        Self {
            show_as_you_type: true,
            case_sensitive: false,
            match_middle: true,
            hide_used: false,
        }
    }
}

fn eq_ci(a: char, b: char) -> bool {
    a == b || a.to_lowercase().eq(b.to_lowercase())
}

/// Camel-hump / snake-hump matching: `aT` matches `assertTrue`, `gv` matches
/// `get_value`, `asT` matches `assertTrue` (runs may continue a hump).
pub fn camel_hump_match(label: &str, prefix: &str) -> bool {
    let lc: Vec<char> = label.chars().collect();
    let pc: Vec<char> = prefix.chars().collect();
    if pc.is_empty() {
        return true;
    }
    let is_hump_start = |i: usize| -> bool {
        if i == 0 {
            return true;
        }
        let c = lc[i];
        let prev = lc[i - 1];
        if c == '_' {
            return false;
        }
        prev == '_'
            || (c.is_uppercase() && !prev.is_uppercase())
            || (c.is_ascii_digit() && !prev.is_ascii_digit())
    };
    // A lowercase first letter must match the first character of the name;
    // an uppercase one may start at any hump (`At` → `getAttribute`).
    if pc[0].is_lowercase() && !eq_ci(lc[0], pc[0]) {
        return false;
    }
    let mut li = 0usize;
    let mut pi = 0usize;
    let mut in_run = false;
    while pi < pc.len() {
        if li >= lc.len() {
            return false;
        }
        let p = pc[pi];
        if in_run && eq_ci(lc[li], p) {
            li += 1;
            pi += 1;
            continue;
        }
        match (li..lc.len()).find(|&i| is_hump_start(i) && eq_ci(lc[i], p)) {
            Some(i) => {
                li = i + 1;
                pi += 1;
                in_run = true;
            }
            None => return false,
        }
    }
    true
}

/// Match quality for `label` against `prefix` (lower is better): 0 exact-case
/// prefix, 1 case-insensitive prefix, 2 hump match, 3 middle match, 4 fuzzy.
pub fn match_score(label: &str, prefix: &str, settings: &CompletionSettings) -> Option<u8> {
    if prefix.is_empty() || label.starts_with(prefix) {
        return Some(0);
    }
    let ll = label.to_lowercase();
    let lp = prefix.to_lowercase();
    if !settings.case_sensitive && ll.starts_with(&lp) {
        return Some(1);
    }
    if camel_hump_match(label, prefix) {
        return Some(2);
    }
    if settings.match_middle {
        let hit = if settings.case_sensitive {
            label.contains(prefix)
        } else {
            ll.contains(&lp)
        };
        if hit {
            return Some(3);
        }
    }
    if !settings.match_middle {
        return None;
    }
    // Ordered character matching covers abbreviations that are neither a
    // contiguous middle match nor a sequence of identifier humps.
    let mut wanted = prefix.chars();
    let mut next = wanted.next();
    for ch in label.chars() {
        if next.is_some_and(|want| {
            if settings.case_sensitive {
                ch == want
            } else {
                eq_ci(ch, want)
            }
        }) {
            next = wanted.next();
        }
    }
    if next.is_none() {
        Some(4)
    } else {
        None
    }
}

/// Filters and ranks lookup items for the typed prefix.
pub fn filter_items(
    items: Vec<CompletionItem>,
    prefix: &str,
    settings: &CompletionSettings,
) -> Vec<CompletionItem> {
    let mut scored: Vec<(u8, CompletionItem)> = items
        .into_iter()
        .filter(|i| !(settings.hide_used && i.label.as_str() == prefix))
        .filter_map(|i| match_score(&i.label, prefix, settings).map(|s| (s, i)))
        .collect();
    scored.sort_by(|(sa, a), (sb, b)| {
        sa.cmp(sb)
            .then(a.priority.cmp(&b.priority))
            .then_with(|| features::is_keyword_argument(b).cmp(&features::is_keyword_argument(a)))
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
    });
    scored.dedup_by(|(_, a), (_, b)| a.label == b.label && a.kind == b.kind);
    scored.into_iter().map(|(_, i)| i).collect()
}

/// Loads `path` into the analysis if needed (helper for UIs that index project files).
pub fn ensure_loaded(db: &Analysis, path: &Path) -> Option<Arc<FileInfo>> {
    if let Some(f) = db.file_for_path(path) {
        return Some(f);
    }
    let src = std::fs::read_to_string(path).ok()?;
    db.set_file_text(path, &src);
    db.file_for_path(path)
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

    fn at(src: &str, needle: &str, extra: usize) -> TextSize {
        TextSize::new((src.find(needle).expect("needle") + extra) as u32)
    }

    fn labels(items: &[CompletionItem]) -> Vec<String> {
        items.iter().map(|i| i.label.to_string()).collect()
    }

    #[test]
    fn expected_type_in_call_argument() {
        let src = "def f(x: int, name: str) -> None:\n    pass\n\ncount = 1\ntitle = 'a'\nf(count, title)\n";
        let (db, f) = setup(src);
        let (t, ctx) = expected_type(&db, &f, at(src, "f(count", 2)).unwrap();
        assert_eq!(t.to_string(), "int");
        assert_eq!(ctx, ExpectedContext::Argument("f".into(), "x".into()));
        let (t, ctx) = expected_type(&db, &f, at(src, "title)", 0)).unwrap();
        assert_eq!(t.to_string(), "str");
        assert_eq!(ctx, ExpectedContext::Argument("f".into(), "name".into()));
    }

    #[test]
    fn expected_type_for_keyword_argument_and_return() {
        let src = "def f(x: int, name: str) -> None:\n    pass\n\nf(1, name=)\n\ndef g() -> bool:\n    return \n";
        let (db, f) = setup(src);
        let (t, ctx) = expected_type(&db, &f, at(src, "name=)", 5)).unwrap();
        assert_eq!(t.to_string(), "str");
        assert_eq!(ctx, ExpectedContext::Argument("f".into(), "name".into()));
        let (t, ctx) = expected_type(&db, &f, at(src, "return \n", 7)).unwrap();
        assert_eq!(t.to_string(), "bool");
        assert_eq!(ctx, ExpectedContext::Return);
    }

    #[test]
    fn expected_type_for_assignments_and_operands() {
        let src = "total: int = 0\ntotal = \nname = 'x'\nname = \nif name == :\n    pass\n";
        let (db, f) = setup(src);
        let (t, ctx) = expected_type(&db, &f, at(src, "total = \n", 8)).unwrap();
        assert_eq!(t.to_string(), "int");
        assert_eq!(ctx, ExpectedContext::Assignment("total".into()));
        let (t, _) = expected_type(&db, &f, at(src, "name = \n", 7)).unwrap();
        assert_eq!(t.to_string(), "str");
        let (t, ctx) = expected_type(&db, &f, at(src, "name == :", 8)).unwrap();
        assert_eq!(t.to_string(), "str");
        assert_eq!(ctx, ExpectedContext::Operand);
    }

    #[test]
    fn smart_completion_ranks_compatible_first_and_suggests_literals() {
        let src = "from typing import Optional\n\nclass Point:\n    def __init__(self, x: int) -> None:\n        self.x = x\n\ndef f(x: int, flag: bool, p: Point, o: Optional[str]) -> None:\n    pass\n\ncount = 1\ntitle = 'a'\nf(co)\n";
        let (db, f) = setup(src);
        let items = smart_complete(&db, &f, at(src, "f(co)", 4));
        let ls = labels(&items);
        assert_eq!(ls.first().map(String::as_str), Some("count"), "{ls:?}");
        // bool parameter: True/False suggested before plain names.
        let src2 = src.replace("f(co)", "f(1, )");
        let (db, f) = setup(&src2);
        let items = smart_complete(&db, &f, at(&src2, "f(1, )", 5));
        let ls = labels(&items);
        assert!(
            ls.iter().position(|l| l == "True").unwrap()
                < ls.iter().position(|l| l == "count").unwrap(),
            "{ls:?}"
        );
        assert!(ls.contains(&"False".to_string()));
        // class parameter: constructor call suggested.
        let src3 = src.replace("f(co)", "f(1, True, )");
        let (db, f) = setup(&src3);
        let items = smart_complete(&db, &f, at(&src3, "f(1, True, )", 11));
        let ls = labels(&items);
        assert_eq!(ls.first().map(String::as_str), Some("Point()"), "{ls:?}");
        assert_eq!(items[0].caret_offset, Some(6));
        // Optional[str]: None and "" suggested, str variables first.
        let src4 = src.replace("f(co)", "f(1, True, Point(1), )");
        let (db, f) = setup(&src4);
        let items = smart_complete(&db, &f, at(&src4, "Point(1), )", 10));
        let ls = labels(&items);
        let none_pos = ls.iter().position(|l| l == "None").unwrap();
        let title_pos = ls.iter().position(|l| l == "title").unwrap();
        let count_pos = ls.iter().position(|l| l == "count").unwrap();
        assert!(title_pos < count_pos, "{ls:?}");
        assert!(none_pos < count_pos, "{ls:?}");
        assert!(ls.contains(&"\"\"".to_string()));
    }

    #[test]
    fn smart_completion_offers_enum_members() {
        let src = "from enum import Enum\n\nclass Color(Enum):\n    RED = 1\n    GREEN = 2\n\ndef paint(c: Color) -> None:\n    pass\n\npaint()\n";
        let (db, f) = setup(src);
        let items = smart_complete(&db, &f, at(src, "paint()", 6));
        let ls = labels(&items);
        assert!(ls.contains(&"Color.RED".to_string()), "{ls:?}");
        assert!(ls.contains(&"Color.GREEN".to_string()), "{ls:?}");
    }

    #[test]
    fn camel_humps_and_filtering() {
        assert!(camel_hump_match("assertTrue", "aT"));
        assert!(camel_hump_match("assertTrue", "asT"));
        assert!(camel_hump_match("get_value", "gv"));
        assert!(camel_hump_match("getValue", "gV"));
        assert!(!camel_hump_match("assertTrue", "aF"));
        assert!(!camel_hump_match("value", "vx"));
        let mk = |l: &str| CompletionItem {
            label: SmolStr::new(l),
            kind: CompletionKind::Function,
            detail: String::new(),
            insert: l.to_string(),
            caret_offset: None,
            priority: 5,
        };
        let items = vec![
            mk("assertTrue"),
            mk("assertFalse"),
            mk("value"),
            mk("getAttribute"),
            mk("aTom"),
        ];
        let out = filter_items(items.clone(), "aT", &CompletionSettings::default());
        let ls: Vec<String> = out.iter().map(|i| i.label.to_string()).collect();
        assert_eq!(
            ls,
            vec!["aTom", "assertTrue", "getAttribute", "assertFalse"]
        );
        let out = filter_items(
            items.clone(),
            "at",
            &CompletionSettings {
                match_middle: false,
                ..Default::default()
            },
        );
        let ls: Vec<String> = out.iter().map(|i| i.label.to_string()).collect();
        assert_eq!(ls, vec!["aTom", "assertTrue"]);
        let out = filter_items(items.clone(), "att", &CompletionSettings::default());
        let ls: Vec<String> = out.iter().map(|i| i.label.to_string()).collect();
        assert_eq!(ls, vec!["getAttribute", "assertTrue"]);
        let out = filter_items(
            items,
            "value",
            &CompletionSettings {
                hide_used: true,
                ..Default::default()
            },
        );
        assert!(out.is_empty());
    }

    #[test]
    fn second_press_finds_project_and_stdlib_symbols() {
        let db = Analysis::for_project(Path::new("/proj"));
        let helpers = "def compute_total(xs):\n    return sum(xs)\n\nclass Widget:\n    pass\n\nMAX_SIZE = 3\n_private = 1\n";
        let main = "import os\n\nvalue = comp\n";
        let hp = PathBuf::from("/proj/helpers.py");
        let mp = PathBuf::from("/proj/main.py");
        db.set_file_text(&hp, helpers);
        let id = db.set_file_text(&mp, main);
        let f = db.file(id).unwrap();
        let files = vec![hp.clone(), mp.clone()];
        let found = second_press_complete(&db, &f, at(main, "comp\n", 4), &files);
        let names: Vec<(String, String)> = found
            .iter()
            .map(|c| (c.module.clone(), c.name.clone()))
            .collect();
        assert!(
            names.contains(&("helpers".into(), "compute_total".into())),
            "{names:?}"
        );
        assert!(!names.iter().any(|(_, n)| n == "_private"));
        // Stdlib: `dataclass` from dataclasses.
        let main2 = "import os\n\nvalue = datacl\n";
        let id = db.set_file_text(&mp, main2);
        let f = db.file(id).unwrap();
        let found = second_press_complete(&db, &f, at(main2, "datacl\n", 6), &files);
        assert!(
            found
                .iter()
                .any(|c| c.module == "dataclasses" && c.name == "dataclass"),
            "{:?}",
            found
                .iter()
                .map(|c| (&c.module, &c.name))
                .collect::<Vec<_>>()
        );
        // Already visible names are not offered; module names are.
        let main3 = "import os\nfrom pathlib import Path\n\nvalue = Pat\nx = js\n";
        let id = db.set_file_text(&mp, main3);
        let f = db.file(id).unwrap();
        let found = second_press_complete(&db, &f, at(main3, "Pat\n", 3), &files);
        assert!(!found
            .iter()
            .any(|c| c.module == "pathlib" && c.name == "Path"));
        let found = second_press_complete(&db, &f, at(main3, "js\n", 2), &files);
        assert!(found.iter().any(|c| c.is_module && c.module == "json"));
    }

    #[test]
    fn second_press_offers_library_names() {
        let db = Analysis::for_project(Path::new("/proj"));
        let main = "from mylib import Session\n\ndef local_sess():\n    pass\n\nvalue = sess\n";
        let mp = PathBuf::from("/proj/main.py");
        let id = db.set_file_text(&mp, main);
        let f = db.file(id).unwrap();
        let lib = |module: &str, name: &str, kind: CompletionKind| ImportableName {
            module: module.into(),
            name: name.into(),
            kind,
        };
        let library = vec![
            lib("requests", "session", CompletionKind::Function),
            lib("requests", "Session", CompletionKind::Class),
            lib("aiohttp", "ClientSession", CompletionKind::Class),
            lib("requests", "_session_private", CompletionKind::Function),
            lib("other", "unrelated", CompletionKind::Variable),
            lib("dataclasses", "dataclass", CompletionKind::Function),
        ];
        let found = second_press_complete_with(
            &db,
            &f,
            at(main, "sess\n", 4),
            std::slice::from_ref(&mp),
            &library,
        );
        let got: Vec<(String, String, Option<usize>, String)> = found
            .iter()
            .filter(|c| c.item.priority == 35)
            .map(|c| {
                (
                    c.module.clone(),
                    c.name.clone(),
                    c.item.caret_offset,
                    c.item.insert.clone(),
                )
            })
            .collect();
        // `Session` is already imported (visible), private and unmatched names are left out.
        assert_eq!(
            got,
            vec![
                (
                    "requests".to_string(),
                    "session".to_string(),
                    Some(8),
                    "session()".to_string()
                ),
                (
                    "aiohttp".to_string(),
                    "ClientSession".to_string(),
                    None,
                    "ClientSession".to_string()
                )
            ]
        );
        assert!(found
            .iter()
            .all(|c| c.item.detail == format!("from {}", c.module) || c.is_module));
        // A name the curated stdlib list already offers is not offered twice.
        let main2 = "value = datacl\n";
        let id = db.set_file_text(&mp, main2);
        let f = db.file(id).unwrap();
        let found = second_press_complete_with(&db, &f, at(main2, "datacl\n", 6), &[mp], &library);
        assert_eq!(
            found
                .iter()
                .filter(|c| c.module == "dataclasses" && c.name == "dataclass")
                .count(),
            1
        );
    }

    #[test]
    fn import_edits_merge_into_existing_statements() {
        let src = "\"\"\"Doc.\"\"\"\nimport os\nfrom pathlib import Path\nfrom typing import (\n    Any,\n)\n\nx = 1\n";
        let (_, f) = setup(src);
        // New statement after the last import.
        let (r, t) = import_edit_for(&f, "dataclasses", "dataclass");
        assert_eq!(t, "from dataclasses import dataclass\n");
        // Right after the last import statement (before the blank line).
        assert_eq!(usize::from(r.start()), src.find(")\n").unwrap() + 2);
        // Merge into `from pathlib import Path`.
        let (r, t) = import_edit_for(&f, "pathlib", "PurePath");
        assert_eq!(t, ", PurePath");
        assert_eq!(
            &src[usize::from(r.start()) - 4..usize::from(r.start())],
            "Path"
        );
        // Merge into a parenthesized list.
        let (r, t) = import_edit_for(&f, "typing", "Optional");
        assert_eq!(t, ", Optional");
        assert_eq!(
            &src[usize::from(r.start()) - 3..usize::from(r.start()) + 1],
            "Any,"
        );
        // Already imported → no-op.
        let (_, t) = import_edit_for(&f, "pathlib", "Path");
        assert!(t.is_empty());
        let (_, t) = import_edit_for(&f, "os", "");
        assert!(t.is_empty());
        let (_, t) = import_edit_for(&f, "sys", "");
        assert_eq!(t, "import sys\n");
    }
}

#[cfg(test)]
mod all_module_completion_tests {
    use super::*;
    #[test]
    fn all_bundled_modules_and_project_stubs_supply_import_edits() {
        let db = Analysis::for_project(Path::new("/proj"));
        let stub = PathBuf::from("/proj/contracts.pyi");
        db.set_file_text(&stub, "class ZebraContract: ...\n");
        let path = PathBuf::from("/proj/main.py");
        for (prefix, module, name) in [
            ("ZebraContr", "contracts", "ZebraContract"),
            ("XMLPullPars", "xml.etree.ElementTree", "XMLPullParser"),
            ("HTTPAdapt", "requests.adapters", "HTTPAdapter"),
        ] {
            let source = format!("value = {prefix}");
            let file = db.file(db.set_file_text(&path, &source)).unwrap();
            let items = second_press_complete(
                &db,
                &file,
                TextSize::new(source.len() as u32),
                std::slice::from_ref(&stub),
            );
            let item = items
                .iter()
                .find(|item| item.module == module && item.name == name)
                .expect("all-module import candidate");
            assert_eq!(item.item.label.as_str(), name);
            let (_, edit) = import_edit_for(&file, &item.module, &item.name);
            assert_eq!(edit, format!("from {module} import {name}\n"));
        }
    }
}

#[cfg(test)]
mod fuzzy_lookup_tests {
    use super::*;
    #[test]
    fn fuzzy_subsequence_is_available_and_ranked_after_contiguous_matches() {
        let prefs = CompletionSettings::default();
        assert!(features::matches_prefix("normalization", "nrz"));
        assert_eq!(match_score("normalization", "nrz", &prefs), Some(4));
        assert_eq!(match_score("nrz_value", "nrz", &prefs), Some(0));
        assert_eq!(match_score("normalization", "nrzz", &prefs), None);
        assert_eq!(
            match_score(
                "normalization",
                "nrz",
                &CompletionSettings {
                    match_middle: false,
                    ..Default::default()
                }
            ),
            None
        );
    }
}
