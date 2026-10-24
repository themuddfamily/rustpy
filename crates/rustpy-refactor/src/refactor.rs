//! Refactorings (roadmap §2.9): extract / inline variable, extract method,
//! introduce constant / field / parameter, change signature, safe delete
//! and move-to-top-level.
//!
//! Every operation returns non-overlapping text edits over the current file
//! text (`(TextRange, String)` byte ranges, same shape as
//! inspection quick-fix edits).

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use rustpy_core::{TextRange, TextSize};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::rowan::NodeOrToken;
use rustpy_syntax::{SyntaxKind, SyntaxNode};

use rustpy_semantic::db::{Analysis, FileInfo};
use rustpy_semantic::edit_helpers::{
    after_last_line, ancestor_at, element_with_comma, enclosing_func, indent_at,
    is_assignment_target, is_identifier, line_start, node_with_range, normalize, reindent, rng,
    slice, statement_removals, stmt_lines, stmt_of, substitution_text, to_snake, token_at, ts,
};
pub use rustpy_semantic::edit_helpers::{apply_edits, try_apply_edits, Edit};
use rustpy_semantic::scope::{BindingId, BindingKind, ScopeId, ScopeKind};
use rustpy_semantic::usages::{self, Symbol};

/// Result of a refactoring: edits plus the ranges of the introduced name in
/// the *resulting* text (for an in-place rename right after applying).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refactoring {
    pub title: String,
    pub edits: Vec<Edit>,
    pub rename_ranges: Vec<TextRange>,
    pub name: String,
}

/// Expression selected by `range` (exactly, modulo surrounding whitespace),
/// or the innermost expression at the caret when `range` is empty.
pub(crate) fn expr_at(file: &FileInfo, range: TextRange) -> Option<ast::Expr> {
    let text: &str = &file.text;
    let root = file.parse.syntax_node();
    let (mut s, mut e) = (
        usize::from(range.start()).min(text.len()),
        usize::from(range.end()).min(text.len()),
    );
    if e < s {
        std::mem::swap(&mut s, &mut e);
    }
    while s < e && text.as_bytes()[s].is_ascii_whitespace() {
        s += 1;
    }
    while e > s && text.as_bytes()[e - 1].is_ascii_whitespace() {
        e -= 1;
    }
    if s == e {
        let tok = token_at(&root, ts(s))?;
        let mut expr = tok.parent_ancestors().find_map(ast::Expr::cast)?;
        // Prefer the call / attribute / subscript chain over a bare callee or object.
        loop {
            let node = expr.syntax().clone();
            let Some(parent) = node.parent() else { break };
            let climb = matches!(
                parent.kind(),
                SyntaxKind::CALL_EXPR | SyntaxKind::ATTRIBUTE_EXPR | SyntaxKind::SUBSCRIPT_EXPR
            ) && parent.children().next().as_ref() == Some(&node);
            if !climb {
                break;
            }
            expr = ast::Expr::cast(parent)?;
        }
        return Some(expr);
    }
    let sel = rng(s, e);
    let node = match root.covering_element(sel) {
        NodeOrToken::Node(n) => n,
        NodeOrToken::Token(t) => t.parent()?,
    };
    node.ancestors()
        .find_map(|n| ast::Expr::cast(n).filter(|x| x.text_range() == sel))
}

/// Expression ranges containing the caret, innermost first (for an
/// "extract which expression?" chooser).
pub fn expression_candidates(file: &FileInfo, offset: TextSize) -> Vec<TextRange> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let root = file.parse.syntax_node();
    let Some(tok) = token_at(&root, offset) else {
        return Vec::new();
    };
    let mut out: Vec<TextRange> = Vec::new();
    for n in tok.parent_ancestors() {
        if ast::Expr::can_cast(n.kind())
            && !is_assignment_target(&n)
            && !matches!(n.kind(), SyntaxKind::STAR_EXPR | SyntaxKind::SLICE_EXPR)
        {
            let r = n.text_range();
            if out.last() != Some(&r) {
                out.push(r);
            }
        }
    }
    out
}

/// Names a new binding in `scope` would capture: names used in it (or in
/// scopes nested in it) that resolve outside it, to an enclosing scope or
/// the builtins (`next = next(it)` raises UnboundLocalError).
fn shadowable_names(file: &FileInfo, scope: ScopeId) -> HashSet<String> {
    let within = |mut s: ScopeId| -> bool {
        loop {
            if s == scope {
                return true;
            }
            match file.scopes.scope(s).parent {
                Some(p) => s = p,
                None => return false,
            }
        }
    };
    file.scopes
        .references
        .iter()
        .filter(|r| {
            within(r.scope)
                && r.resolved
                    .is_none_or(|b| !within(file.scopes.binding(b).scope))
        })
        .map(|r| r.name.to_string())
        .collect()
}

/// Names bound in the scope chain at `offset`.
pub(crate) fn taken_names(file: &FileInfo, offset: TextSize) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut cur = Some(file.scopes.scope_at(offset));
    while let Some(id) = cur {
        let s = file.scopes.scope(id);
        out.extend(s.bindings.keys().map(|k| k.to_string()));
        cur = s.parent;
    }
    out
}

fn unique_name(base: &str, taken: &HashSet<String>) -> String {
    if !taken.contains(base) {
        return base.to_string();
    }
    for i in 1.. {
        let cand = format!("{base}{i}");
        if !taken.contains(&cand) {
            return cand;
        }
    }
    unreachable!()
}

fn base_name(expr: &ast::Expr) -> String {
    // Keyword argument context names the value: `timeout=30` → `timeout`.
    if let Some(p) = expr.syntax().parent() {
        if p.kind() == SyntaxKind::KEYWORD_ARG {
            if let Some(k) = ast::KeywordArg::cast(p) {
                if matches!(
                    expr,
                    ast::Expr::Literal(_) | ast::Expr::String(_) | ast::Expr::Unary(_)
                ) {
                    return k.name_text();
                }
            }
        }
    }
    match expr {
        ast::Expr::Call(c) => {
            let callee = match c.callee() {
                Some(ast::Expr::Name(n)) => n.name_text(),
                Some(ast::Expr::Attribute(a)) => a.attr_text(),
                _ => return "result".into(),
            };
            match callee.as_str() {
                "len" => "length".into(),
                "str" | "repr" | "format" | "input" => "text".into(),
                "next" | "anext" => "item".into(),
                "iter" | "aiter" => "iterator".into(),
                "type" => "kind".into(),
                "id" => "identifier".into(),
                "int" | "float" | "abs" | "round" | "sum" | "min" | "max" => "number".into(),
                "list" | "sorted" | "reversed" | "tuple" => "items".into(),
                "dict" => "mapping".into(),
                "set" | "frozenset" => "values".into(),
                "open" => "file".into(),
                "range" => "indices".into(),
                "enumerate" | "zip" | "map" | "filter" => "pairs".into(),
                _ => {
                    let mut n = callee.clone();
                    for p in [
                        "get_", "make_", "create_", "build_", "compute_", "fetch_", "load_",
                        "read_", "find_", "new_", "to_", "as_",
                    ] {
                        if let Some(rest) = n.strip_prefix(p) {
                            if !rest.is_empty() {
                                n = rest.to_string();
                                break;
                            }
                        }
                    }
                    n
                }
            }
        }
        ast::Expr::Attribute(a) => a.attr_text(),
        ast::Expr::Name(n) => n.name_text(),
        ast::Expr::Subscript(s) => match s.object() {
            Some(ast::Expr::Name(n)) => {
                let n = n.name_text();
                if let Some(stem) = n.strip_suffix('s') {
                    if stem.len() > 1 {
                        return stem.to_string();
                    }
                }
                format!("{n}_item")
            }
            _ => "item".into(),
        },
        ast::Expr::String(s) => {
            let words: Vec<String> = s
                .value()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| !w.is_empty())
                .take(3)
                .map(|w| w.to_lowercase())
                .collect();
            if words.is_empty() {
                "text".into()
            } else {
                words.join("_")
            }
        }
        ast::Expr::FString(_) | ast::Expr::ConcatString(_) => "text".into(),
        ast::Expr::Literal(l) => match l.kind() {
            Some(SyntaxKind::TRUE_KW) | Some(SyntaxKind::FALSE_KW) => "flag".into(),
            Some(SyntaxKind::INT_NUMBER)
            | Some(SyntaxKind::FLOAT_NUMBER)
            | Some(SyntaxKind::COMPLEX_NUMBER) => "number".into(),
            _ => "value".into(),
        },
        ast::Expr::Compare(_) | ast::Expr::Not(_) | ast::Expr::BoolOp(_) => "condition".into(),
        ast::Expr::Lambda(_) => "func".into(),
        ast::Expr::List(_) | ast::Expr::ListComp(_) | ast::Expr::Generator(_) => "items".into(),
        ast::Expr::Dict(_) | ast::Expr::DictComp(_) => "mapping".into(),
        ast::Expr::Set(_) | ast::Expr::SetComp(_) | ast::Expr::Tuple(_) => "values".into(),
        ast::Expr::Binary(_) => "result".into(),
        ast::Expr::Unary(u) => u
            .operand()
            .map(|o| base_name(&o))
            .unwrap_or_else(|| "value".into()),
        ast::Expr::Await(a) => a
            .expr()
            .map(|o| base_name(&o))
            .unwrap_or_else(|| "result".into()),
        ast::Expr::Paren(p) => p
            .inner()
            .map(|o| base_name(&o))
            .unwrap_or_else(|| "value".into()),
        _ => "value".into(),
    }
}

/// Suggests a variable (or `CONSTANT`) name for `expr` that is not taken.
pub fn suggest_name(expr: &ast::Expr, taken: &HashSet<String>, constant: bool) -> String {
    let mut base = to_snake(&base_name(expr));
    if base.is_empty() || !is_identifier(&base) || base.starts_with(|c: char| c.is_ascii_digit()) {
        base = if constant {
            "constant".into()
        } else {
            "value".into()
        };
    }
    if constant {
        base = base.to_uppercase();
    }
    unique_name(&base, taken)
}

/// `(offset, len)` of an introduced name inside an edit's replacement text.
type Mark = (usize, usize);

/// Collects edits and remembers where introduced names land in the result.
#[derive(Default)]
pub(crate) struct Edits {
    items: Vec<(TextRange, String, Vec<Mark>)>,
}

impl Edits {
    pub(crate) fn add(&mut self, range: TextRange, text: impl Into<String>) {
        self.items.push((range, text.into(), Vec::new()));
    }
    pub(crate) fn add_marked(
        &mut self,
        range: TextRange,
        text: impl Into<String>,
        marks: Vec<Mark>,
    ) {
        self.items.push((range, text.into(), marks));
    }
    pub(crate) fn finish(self) -> (Vec<Edit>, Vec<TextRange>) {
        let mut items = self.items;
        items.sort_by_key(|(r, ..)| (r.start(), r.end()));
        let mut delta: i64 = 0;
        let mut marks = Vec::new();
        for (r, t, ms) in &items {
            let start = usize::from(r.start()) as i64 + delta;
            for (rel, len) in ms {
                let s = (start + *rel as i64).max(0) as usize;
                marks.push(rng(s, s + len));
            }
            delta += t.len() as i64 - u32::from(r.len()) as i64;
        }
        (items.into_iter().map(|(r, t, _)| (r, t)).collect(), marks)
    }
}

/// The node that defines `scope` (module root for the module scope).
fn scope_node(file: &FileInfo, scope: ScopeId) -> SyntaxNode {
    let root = file.parse.syntax_node();
    if scope == file.scopes.module_scope() {
        return root;
    }
    let range = file.scopes.scope(scope).range;
    node_with_range(&root, range).unwrap_or(root)
}

fn check_not_in_comprehension(
    file: &FileInfo,
    expr: &ast::Expr,
    anchor: &SyntaxNode,
) -> Result<(), String> {
    for n in expr
        .syntax()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::NAME_EXPR)
    {
        let Some(r) = file.scopes.reference_at(n.text_range().start()) else {
            continue;
        };
        let Some(b) = r.resolved else { continue };
        let s = file.scopes.scope(file.scopes.binding(b).scope);
        if matches!(s.kind, ScopeKind::Comprehension | ScopeKind::Lambda)
            && anchor.text_range().contains_range(s.range)
        {
            return Err(format!(
                "Cannot extract: the expression depends on '{}' bound in a comprehension or lambda",
                r.name
            ));
        }
    }
    Ok(())
}

/// Statement (child of the block containing every occurrence) before which a
/// new variable can be introduced.
fn anchor_stmt(first: &SyntaxNode, occurrences: &[TextRange]) -> Option<SyntaxNode> {
    let mut cand = first.ancestors().find(|n| ast::Stmt::can_cast(n.kind()))?;
    loop {
        let parent = cand.parent()?;
        if occurrences
            .iter()
            .all(|r| parent.text_range().contains_range(*r))
        {
            return Some(cand);
        }
        cand = parent
            .ancestors()
            .skip(1)
            .find(|n| ast::Stmt::can_cast(n.kind()))?;
    }
}

fn check_extractable(expr: &ast::Expr) -> Result<(), String> {
    if matches!(
        expr,
        ast::Expr::Star(_)
            | ast::Expr::Yield(_)
            | ast::Expr::YieldFrom(_)
            | ast::Expr::Named(_)
            | ast::Expr::Slice(_)
    ) {
        return Err("This expression cannot be extracted".into());
    }
    if is_assignment_target(expr.syntax()) {
        return Err("Cannot extract an assignment target".into());
    }
    if expr
        .syntax()
        .ancestors()
        .any(|n| matches!(n.kind(), SyntaxKind::FSTRING_REPLACEMENT))
        && expr
            .syntax()
            .parent()
            .is_some_and(|p| p.kind() != SyntaxKind::FSTRING_REPLACEMENT)
    {
        // Nested inside an f-string replacement field: allow only the whole field expression.
        return Err("Select the whole f-string replacement field".into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Extract variable
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Occurrences replaced by "replace all"
// ---------------------------------------------------------------------------

/// Whether the expression at `range` is a bare number, bool or `None`
/// literal: equal values elsewhere are rarely the same concept, so "replace
/// this occurrence only" is the better default.
pub fn is_trivial_literal(file: &FileInfo, range: TextRange) -> bool {
    match expr_at(file, range) {
        Some(ast::Expr::Literal(_)) => true,
        Some(ast::Expr::Unary(u)) => matches!(u.operand(), Some(ast::Expr::Literal(_))),
        _ => false,
    }
}

/// Whether "replace all occurrences" should be the preselected choice for
/// the expression at `range`: not for a bare literal (equal values
/// elsewhere are rarely the same concept), nor for an expression calling
/// something that may have side effects (`compute(x)`, `obj.load()`):
/// computing it once instead of at every occurrence changes what the code does.
pub fn replace_all_by_default(file: &FileInfo, range: TextRange) -> bool {
    if is_trivial_literal(file, range) {
        return false;
    }
    let Some(expr) = expr_at(file, range) else {
        return false;
    };
    !expr.syntax().descendants().any(|n| match n.kind() {
        SyntaxKind::CALL_EXPR => {
            ast::CallExpr::cast(n).is_none_or(|c| call_effect(&c) != CallEffect::Pure)
        }
        SyntaxKind::AWAIT_EXPR
        | SyntaxKind::YIELD_EXPR
        | SyntaxKind::YIELD_FROM_EXPR
        | SyntaxKind::NAMED_EXPR => true,
        _ => false,
    })
}

/// Occurrences that "replace all occurrences" replaces when extracting the
/// expression at `range` into a variable (`constant == false`) or into a
/// module constant, sorted and always including the selected expression
/// (empty when nothing extractable is selected).
///
/// A variable only takes occurrences at or after the selected statement in
/// the same scope that would still see the same value: none past an
/// assignment to a name the expression reads or a call that may mutate one
/// (`items.append(x)`, `process(items)`, also through an alias `other =
/// items`), none after an occurrence that itself changes it (`items.pop()`,
/// `next(it)`), and none in a loop that changes such a name while the
/// variable would be computed outside the loop.
/// A constant skips docstrings, annotations, `match` patterns and places
/// where a local name would shadow it.
pub fn extract_occurrences(file: &FileInfo, range: TextRange, constant: bool) -> Vec<TextRange> {
    let Some(expr) = expr_at(file, range) else {
        return Vec::new();
    };
    if constant {
        let name = constant_name(file, &expr);
        constant_occurrences(file, &expr, &name)
    } else {
        variable_occurrences(file, &expr)
    }
}

fn variable_occurrences(file: &FileInfo, expr: &ast::Expr) -> Vec<TextRange> {
    let text: &str = &file.text;
    let sel = expr.text_range();
    if matches!(expr, ast::Expr::Name(_)) {
        return vec![sel];
    }
    let Some(stmt) = stmt_of(expr.syntax()) else {
        return vec![sel];
    };
    let root = file.parse.syntax_node();
    let scope = file.scopes.scope_at(sel.start());
    let region = scope_node(file, scope);
    let norm = normalize(slice(text, sel));
    let from = stmt.syntax().text_range().start();
    let mut candidates: Vec<TextRange> = region
        .descendants()
        .filter(|n| n.kind() == expr.syntax().kind())
        .filter(|n| {
            let r = n.text_range();
            r != sel
                && r.start() >= from
                && file.scopes.scope_at(r.start()) == scope
                && !is_assignment_target(n)
                && normalize(slice(text, r)) == norm
        })
        .map(|n| n.text_range())
        .collect();
    candidates.sort_by_key(|r| r.start());
    candidates.dedup();
    let barriers = mutation_barriers(file, expr, scope, &region);
    let mut chosen = vec![sel];
    for c in candidates {
        if chosen
            .iter()
            .any(|o| o.intersect(c).is_some_and(|i| !i.is_empty()))
        {
            continue;
        }
        let mut trial = chosen.clone();
        trial.push(c);
        trial.sort_by_key(|r| r.start());
        let Some(first) = node_with_range(&root, trial[0]) else {
            continue;
        };
        let Some(anchor) = anchor_stmt(&first, &trial) else {
            continue;
        };
        if hoisting_is_safe(&root, &anchor, &trial, &barriers) {
            chosen = trial;
        }
    }
    chosen
}

/// What a call may do to the values it is called on or with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CallEffect {
    /// A side-effect-free builtin or read-only method (`len(x)`, `s.strip()`).
    Pure,
    /// Known to change its receiver or argument, or the world outside
    /// (`items.pop()`, `next(it)`, `print(x)`): every evaluation differs.
    Mutating,
    /// Anything else (`compute(x)`, `obj.load()`): it may change them.
    Unknown,
}

/// Builtins without side effects on their arguments.
/// (Not `all`, `any`, `max`, `min`, `sorted`, `sum`, `tuple` or `frozenset`:
/// given an iterator or a file they consume it.)
const PURE_BUILTINS: &[&str] = &[
    "abs",
    "ascii",
    "bin",
    "bool",
    "callable",
    "chr",
    "complex",
    "divmod",
    "float",
    "format",
    "getattr",
    "hasattr",
    "hash",
    "hex",
    "id",
    "int",
    "isinstance",
    "issubclass",
    "len",
    "oct",
    "ord",
    "pow",
    "repr",
    "round",
    "str",
    "type",
];
/// Builtins that consume or change their arguments, or do I/O.
const MUTATING_BUILTINS: &[&str] = &[
    "anext",
    "breakpoint",
    "delattr",
    "exec",
    "input",
    "next",
    "open",
    "print",
    "setattr",
];
/// Methods that only read their receiver (strings, bytes, numbers, sequences, dict views).
const PURE_METHODS: &[&str] = &[
    "as_integer_ratio",
    "bit_length",
    "capitalize",
    "casefold",
    "center",
    "conjugate",
    "count",
    "decode",
    "encode",
    "endswith",
    "expandtabs",
    "find",
    "format",
    "format_map",
    "index",
    "is_integer",
    "isalnum",
    "isalpha",
    "isascii",
    "isdecimal",
    "isdigit",
    "isdisjoint",
    "isidentifier",
    "islower",
    "isnumeric",
    "isprintable",
    "isspace",
    "issubset",
    "issuperset",
    "istitle",
    "isupper",
    "items",
    "keys",
    "ljust",
    "lower",
    "lstrip",
    "partition",
    "removeprefix",
    "removesuffix",
    "rfind",
    "rindex",
    "rjust",
    "rpartition",
    "rsplit",
    "rstrip",
    "split",
    "splitlines",
    "startswith",
    "strip",
    "swapcase",
    "title",
    "translate",
    "upper",
    "values",
    "zfill",
];
/// Methods that change their receiver or consume from it (containers, iterators, files, queues, locks).
const MUTATING_METHODS: &[&str] = &[
    "__next__",
    "acquire",
    "add",
    "append",
    "appendleft",
    "clear",
    "close",
    "difference_update",
    "discard",
    "extend",
    "extendleft",
    "flush",
    "get_nowait",
    "insert",
    "intersection_update",
    "pop",
    "popitem",
    "popleft",
    "put",
    "put_nowait",
    "read",
    "readline",
    "readlines",
    "release",
    "remove",
    "reverse",
    "rotate",
    "seek",
    "send",
    "setdefault",
    "sort",
    "symmetric_difference_update",
    "throw",
    "truncate",
    "update",
    "write",
    "writelines",
];

fn call_effect(call: &ast::CallExpr) -> CallEffect {
    let (name, pure, mutating) = match call.callee() {
        Some(ast::Expr::Name(n)) => (n.name_text(), PURE_BUILTINS, MUTATING_BUILTINS),
        Some(ast::Expr::Attribute(a)) => (a.attr_text(), PURE_METHODS, MUTATING_METHODS),
        _ => return CallEffect::Unknown,
    };
    if pure.contains(&name.as_str()) {
        CallEffect::Pure
    } else if mutating.contains(&name.as_str()) {
        CallEffect::Mutating
    } else {
        CallEffect::Unknown
    }
}

/// A point where a value read by the extracted expression may change.
struct Barrier {
    /// Position after which the change is visible.
    at: TextSize,
    /// The node making the change.
    range: TextRange,
    /// The change certainly happens (`x = ...`, `items.pop()`): made by an
    /// occurrence itself, it still gives the occurrences after it another value.
    certain: bool,
}

/// Points where a value read by `expr` may change: assignments to (and
/// `del` of) its names, item / attribute assignments on them, and calls of
/// their methods or receiving them as arguments, except side-effect-free
/// ones (`len(items)`, `name.strip()`). Calls through aliases (`other =
/// items; other.append(x)`) count too.
fn mutation_barriers(
    file: &FileInfo,
    expr: &ast::Expr,
    scope: ScopeId,
    region: &SyntaxNode,
) -> Vec<Barrier> {
    let root = file.parse.syntax_node();
    let names: HashSet<String> = expr
        .syntax()
        .descendants()
        .filter_map(ast::NameExpr::cast)
        .map(|n| n.name_text())
        .collect();
    // Names bound to one of those values (`other = items`), transitively.
    let mut watched = names.clone();
    loop {
        let before = watched.len();
        for b in &file.scopes.bindings {
            if b.scope != scope
                || b.kind != BindingKind::Assignment
                || watched.contains(b.name.as_str())
            {
                continue;
            }
            if b.value
                .and_then(|v| node_with_range(&root, v))
                .and_then(ast::NameExpr::cast)
                .is_some_and(|n| watched.contains(&n.name_text()))
            {
                watched.insert(b.name.to_string());
            }
        }
        if watched.len() == before {
            break;
        }
    }
    let mut out = Vec::new();
    for b in &file.scopes.bindings {
        if b.scope != scope || !names.contains(b.name.as_str()) {
            continue;
        }
        // `x = f(x)` reads `x` before writing it: the write lands after the statement.
        let at = match b.kind {
            BindingKind::Assignment
            | BindingKind::AugAssignment
            | BindingKind::Annotation
            | BindingKind::TypeAlias => b.node_range.end(),
            _ => b.range.end(),
        };
        out.push(Barrier {
            at,
            range: b.range,
            certain: true,
        });
    }
    for r in &file.scopes.references {
        if r.scope == scope && r.is_write && names.contains(r.name.as_str()) {
            out.push(Barrier {
                at: r.range.end(),
                range: r.range,
                certain: true,
            });
        }
    }
    let call_barrier = |call: &SyntaxNode| {
        let effect =
            ast::CallExpr::cast(call.clone()).map_or(CallEffect::Unknown, |c| call_effect(&c));
        (effect != CallEffect::Pure).then(|| Barrier {
            at: call.text_range().end(),
            range: call.text_range(),
            certain: effect == CallEffect::Mutating,
        })
    };
    for n in region
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::NAME_EXPR)
    {
        let Some(name) = ast::NameExpr::cast(n.clone()).map(|x| x.name_text()) else {
            continue;
        };
        if !watched.contains(&name) || file.scopes.scope_at(n.text_range().start()) != scope {
            continue;
        }
        // The attribute / subscript chain rooted at the name.
        let mut cur = n.clone();
        while let Some(p) = cur.parent() {
            if matches!(
                p.kind(),
                SyntaxKind::ATTRIBUTE_EXPR | SyntaxKind::SUBSCRIPT_EXPR
            ) && p.children().next().as_ref() == Some(&cur)
            {
                cur = p;
            } else {
                break;
            }
        }
        let Some(parent) = cur.parent() else { continue };
        if cur != n && is_assignment_target(&cur) {
            let at = stmt_of(&cur)
                .map(|s| s.syntax().text_range().end())
                .unwrap_or(cur.text_range().end());
            out.push(Barrier {
                at,
                range: cur.text_range(),
                certain: true,
            });
        } else if cur != n
            && parent.kind() == SyntaxKind::CALL_EXPR
            && parent.children().next().as_ref() == Some(&cur)
        {
            out.extend(call_barrier(&parent));
        } else if matches!(
            parent.kind(),
            SyntaxKind::ARG
                | SyntaxKind::KEYWORD_ARG
                | SyntaxKind::STAR_ARG
                | SyntaxKind::STAR_STAR_ARG
        ) {
            if let Some(call) = parent
                .ancestors()
                .find(|a| a.kind() == SyntaxKind::CALL_EXPR)
            {
                out.extend(call_barrier(&call));
            }
        }
    }
    out
}

/// Whether computing the expression once, right before `anchor`, gives every
/// occurrence the value it computes today.
fn hoisting_is_safe(
    root: &SyntaxNode,
    anchor: &SyntaxNode,
    occurrences: &[TextRange],
    barriers: &[Barrier],
) -> bool {
    let start = anchor.text_range().start();
    // A change an occurrence may make happens either way, unless it is
    // certain (`items.pop()`): then the occurrences after it see another value.
    let live: Vec<&Barrier> = barriers
        .iter()
        .filter(|b| b.certain || !occurrences.iter().any(|o| o.contains_range(b.range)))
        .collect();
    for occ in occurrences {
        if live.iter().any(|b| b.at >= start && b.at <= occ.start()) {
            return false;
        }
        // A loop between the new variable (inserted in the anchor's block) and
        // the occurrence used to re-evaluate the expression on every iteration.
        let Some(node) = node_with_range(root, *occ) else {
            return false;
        };
        let block = anchor.parent();
        for l in node.ancestors().take_while(|a| Some(a) != block.as_ref()) {
            let repeated = match l.kind() {
                SyntaxKind::FOR_STMT => l
                    .children()
                    .find(|c| c.kind() == SyntaxKind::BLOCK)
                    .is_some_and(|b| b.text_range().contains_range(*occ)),
                SyntaxKind::WHILE_STMT => l
                    .children()
                    .find(|c| c.kind() == SyntaxKind::ELSE_CLAUSE)
                    .is_none_or(|e| !e.text_range().contains_range(*occ)),
                _ => false,
            };
            if repeated && live.iter().any(|b| l.text_range().contains_range(b.range)) {
                return false;
            }
        }
    }
    true
}

/// Name Introduce Constant gives the literal (unique at module level and in
/// the scopes around the literal).
fn constant_name(file: &FileInfo, expr: &ast::Expr) -> String {
    let mut taken = taken_names(file, expr.text_range().start());
    taken.extend(
        file.scopes
            .scope(file.scopes.module_scope())
            .bindings
            .keys()
            .map(|k| k.to_string()),
    );
    suggest_name(expr, &taken, true)
}

fn constant_occurrences(file: &FileInfo, expr: &ast::Expr, name: &str) -> Vec<TextRange> {
    let text: &str = &file.text;
    let sel = expr.text_range();
    let norm = normalize(slice(text, sel));
    let mut out = vec![sel];
    for n in file
        .parse
        .syntax_node()
        .descendants()
        .filter(|n| n.kind() == expr.syntax().kind())
    {
        let r = n.text_range();
        if r != sel && normalize(slice(text, r)) == norm && constant_can_replace(file, &n, name) {
            out.push(r);
        }
    }
    out.sort_by_key(|r| r.start());
    out.dedup();
    out
}

/// Whether the literal `node` can become a reference to the module constant `name`.
fn constant_can_replace(file: &FileInfo, node: &SyntaxNode, name: &str) -> bool {
    // Docstrings stay; `Literal[1]` in annotations stays; in `case 1:` a bare
    // name would turn the value pattern into a capture pattern.
    if node
        .parent()
        .is_some_and(|p| p.kind() == SyntaxKind::EXPR_STMT)
        || node.ancestors().any(|a| {
            matches!(
                a.kind(),
                SyntaxKind::ANNOTATION | SyntaxKind::RETURN_ANNOTATION
            ) || a.kind().is_pattern()
        })
    {
        return false;
    }
    // A local binding of the same name would shadow the constant.
    let mut cur = Some(file.scopes.scope_at(node.text_range().start()));
    while let Some(id) = cur {
        let s = file.scopes.scope(id);
        if s.kind != ScopeKind::Module && s.bindings.contains_key(name) {
            return false;
        }
        cur = s.parent;
    }
    true
}

// ---------------------------------------------------------------------------
// Extract variable
// ---------------------------------------------------------------------------

/// Extracts the expression selected by `range` (or at the caret when empty)
/// into a local variable introduced before the containing statement. With
/// `replace_all`, the occurrences of [`extract_occurrences`] are replaced too.
pub fn extract_variable(
    db: &Analysis,
    file: &Arc<FileInfo>,
    range: TextRange,
    replace_all: bool,
) -> Result<Refactoring, String> {
    let _ = db;
    let text: &str = &file.text;
    let root = file.parse.syntax_node();
    let expr = expr_at(file, range).ok_or("Select an expression to extract")?;
    check_extractable(&expr)?;
    let stmt =
        stmt_of(expr.syntax()).ok_or("Cannot find the statement containing the expression")?;
    check_not_in_comprehension(file, &expr, stmt.syntax())?;
    let occurrences = if replace_all {
        variable_occurrences(file, &expr)
    } else {
        vec![expr.text_range()]
    };
    let first = occurrences[0];
    let first_node = node_with_range(&root, first).ok_or("Cannot locate the expression")?;
    let anchor =
        anchor_stmt(&first_node, &occurrences).ok_or("Cannot find a place for the new variable")?;
    let mut taken = taken_names(file, expr.text_range().start());
    taken.extend(shadowable_names(
        file,
        file.scopes.scope_at(anchor.text_range().start()),
    ));
    let name = suggest_name(&expr, &taken, false);
    let indent = indent_at(text, anchor.text_range().start().into());
    let insert_at = line_start(text, anchor.text_range().start().into());
    let expr_text = slice(text, expr.text_range()).trim().to_string();
    let mut ed = Edits::default();
    ed.add_marked(
        rng(insert_at, insert_at),
        format!("{indent}{name} = {expr_text}\n"),
        vec![(indent.len(), name.len())],
    );
    for occ in &occurrences {
        ed.add_marked(*occ, name.clone(), vec![(0, name.len())]);
    }
    let (edits, rename_ranges) = ed.finish();
    Ok(Refactoring {
        title: format!("Extract variable '{name}'"),
        edits,
        rename_ranges,
        name,
    })
}

// ---------------------------------------------------------------------------
// Inline variable
// ---------------------------------------------------------------------------

/// Inlines the local variable at `offset` (a single simple assignment) into
/// all its reads and removes the assignment.
pub fn inline_variable(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
) -> Result<Refactoring, String> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let text: &str = &file.text;
    let root = file.parse.syntax_node();
    let sym = usages::symbol_at(db, file, offset).ok_or("Place the caret on a variable")?;
    let Symbol::Name { def, name } = sym else {
        return Err("Only variables can be inlined".into());
    };
    if def.file.path != file.path {
        return Err(format!("'{name}' is defined in another file"));
    }
    let scope = def.binding().scope;
    let ids: Vec<BindingId> = file.scopes.bindings_in(scope, &name).to_vec();
    let defs: Vec<BindingId> = ids
        .iter()
        .copied()
        .filter(|i| file.scopes.binding(*i).is_definition())
        .collect();
    if defs.len() != 1 {
        return Err(format!("'{name}' is assigned more than once"));
    }
    let id = defs[0];
    let b = file.scopes.binding(id);
    if !matches!(b.kind, BindingKind::Assignment) {
        return Err(format!("'{name}' is not a simple assignment"));
    }
    let node = node_with_range(&root, b.node_range).ok_or("Cannot find the assignment")?;
    let (value, stmt_range) = match node.kind() {
        SyntaxKind::ASSIGN_STMT => {
            let a = ast::AssignStmt::cast(node.clone()).unwrap();
            let targets = a.targets();
            if targets.len() != 1 || !matches!(targets[0], ast::Expr::Name(_)) {
                return Err(format!(
                    "'{name}' is not a simple assignment (tuple or chained target)"
                ));
            }
            (a.value().ok_or("Assignment has no value")?, a.text_range())
        }
        SyntaxKind::ANN_ASSIGN_STMT => {
            let a = ast::AnnAssignStmt::cast(node.clone()).unwrap();
            (a.value().ok_or("Declaration has no value")?, a.text_range())
        }
        _ => return Err(format!("'{name}' is not a simple assignment")),
    };
    let reads: Vec<TextRange> = file
        .scopes
        .references_to(id)
        .into_iter()
        .filter(|r| !r.is_write && !b.node_range.contains_range(r.range))
        .map(|r| r.range)
        .collect();
    if reads.is_empty() {
        return Err(format!("'{name}' has no usages"));
    }
    if reads.iter().any(|r| r.start() < b.range.start()) {
        return Err(format!("'{name}' is used before its assignment"));
    }
    let mut ed = Edits::default();
    ed.add(stmt_lines(text, stmt_range), "");
    for r in reads {
        let at = node_with_range(&root, r)
            .and_then(|n| n.ancestors().find(|a| a.kind() == SyntaxKind::NAME_EXPR))
            .ok_or("Cannot locate a usage")?;
        ed.add(at.text_range(), substitution_text(text, &value, &at));
    }
    let (edits, _) = ed.finish();
    Ok(Refactoring {
        title: format!("Inline variable '{name}'"),
        edits,
        rename_ranges: Vec::new(),
        name,
    })
}

// ---------------------------------------------------------------------------
// Extract method
// ---------------------------------------------------------------------------

/// Whole statements (children of one block) covered by `range`.
pub(crate) fn selected_statements(
    file: &FileInfo,
    range: TextRange,
) -> Result<(Vec<ast::Stmt>, SyntaxNode), String> {
    let text: &str = &file.text;
    let root = file.parse.syntax_node();
    let (mut s, mut e) = (
        usize::from(range.start()).min(text.len()),
        usize::from(range.end()).min(text.len()),
    );
    if e < s {
        std::mem::swap(&mut s, &mut e);
    }
    // Trim whitespace and comment-only lines at the edges.
    while s < e && text.as_bytes()[s].is_ascii_whitespace() {
        s += 1;
    }
    while e > s && text.as_bytes()[e - 1].is_ascii_whitespace() {
        e -= 1;
    }
    if s >= e {
        return Err("Select the statements to extract".into());
    }
    let tok = token_at(&root, ts(s)).ok_or("Select the statements to extract")?;
    let first = tok
        .parent_ancestors()
        .find_map(ast::Stmt::cast)
        .ok_or("Select whole statements")?;
    let block = first.syntax().parent().ok_or("Select whole statements")?;
    if !matches!(block.kind(), SyntaxKind::BLOCK | SyntaxKind::MODULE) {
        return Err("Select whole statements".into());
    }
    let sel = rng(s, e);
    let stmts: Vec<ast::Stmt> = block
        .children()
        .filter_map(ast::Stmt::cast)
        .filter(|st| {
            st.text_range()
                .intersect(sel)
                .is_some_and(|i| !i.is_empty())
                || st.text_range().start() >= sel.start() && st.text_range().start() < sel.end()
        })
        .collect();
    if stmts.is_empty() {
        return Err("Select whole statements".into());
    }
    let first_start: usize = stmts[0].text_range().start().into();
    if s > first_start {
        return Err("Select whole statements (the selection starts inside a statement)".into());
    }
    let last = stmts.last().unwrap();
    let mut last_end: usize = last.text_range().end().into();
    while last_end > first_start && text.as_bytes()[last_end - 1].is_ascii_whitespace() {
        last_end -= 1;
    }
    if e < last_end {
        return Err("Select whole statements (the selection ends inside a statement)".into());
    }
    Ok((stmts, block))
}

pub(crate) fn has_descendant(
    node: &SyntaxNode,
    kinds: &[SyntaxKind],
    stop_at: &[SyntaxKind],
) -> bool {
    fn walk(n: &SyntaxNode, kinds: &[SyntaxKind], stop_at: &[SyntaxKind]) -> bool {
        for c in n.children() {
            if kinds.contains(&c.kind()) {
                return true;
            }
            if stop_at.contains(&c.kind()) {
                continue;
            }
            if walk(&c, kinds, stop_at) {
                return true;
            }
        }
        false
    }
    kinds.contains(&node.kind()) || walk(node, kinds, stop_at)
}

/// Extracts the whole statements covered by `range` into a new function
/// `name` (a method when inside a method), replacing them with a call.
pub fn extract_method(
    db: &Analysis,
    file: &Arc<FileInfo>,
    range: TextRange,
    name: &str,
) -> Result<Refactoring, String> {
    crate::extraction::extract_callable(
        db,
        file,
        range,
        name,
        &crate::extraction::CallableOptions::default(),
    )
}

// ---------------------------------------------------------------------------
// Introduce constant / field / parameter
// ---------------------------------------------------------------------------

fn is_literal_like(expr: &ast::Expr) -> bool {
    match expr {
        ast::Expr::Literal(_) | ast::Expr::String(_) | ast::Expr::ConcatString(_) => true,
        ast::Expr::Unary(u) => u
            .operand()
            .as_ref()
            .is_some_and(|o| matches!(o, ast::Expr::Literal(_))),
        ast::Expr::Tuple(t) => t.elements().all(|e| is_literal_like(&e)),
        _ => false,
    }
}

/// Introduces a module-level `CONSTANT = <literal>` after the imports. With
/// `replace_all`, the occurrences of [`extract_occurrences`] are replaced too.
pub fn introduce_constant(
    file: &Arc<FileInfo>,
    range: TextRange,
    replace_all: bool,
) -> Result<Refactoring, String> {
    let text: &str = &file.text;
    let expr = expr_at(file, range).ok_or("Select a literal to extract")?;
    if !is_literal_like(&expr) {
        return Err("Only literals can be introduced as constants".into());
    }
    if expr
        .syntax()
        .parent()
        .is_some_and(|p| p.kind() == SyntaxKind::EXPR_STMT)
    {
        return Err("A docstring cannot be introduced as a constant".into());
    }
    let name = constant_name(file, &expr);
    if !constant_can_replace(file, expr.syntax(), &name) {
        return Err(
            "This literal cannot be replaced by a name here (annotation or match pattern)".into(),
        );
    }
    let insert_at: usize = rustpy_semantic::import_placement::import_insert_offset(file).into();
    let occurrences = if replace_all {
        constant_occurrences(file, &expr, &name)
    } else {
        vec![expr.text_range()]
    };
    if occurrences
        .iter()
        .any(|r| usize::from(r.start()) < insert_at)
    {
        return Err("The literal is used before the import block".into());
    }
    let literal = slice(text, expr.text_range()).trim().to_string();
    let follows_blank = text[insert_at..].starts_with('\n') || insert_at >= text.len();
    let decl = format!(
        "{name} = {literal}\n{}",
        if follows_blank { "" } else { "\n" }
    );
    let mut ed = Edits::default();
    ed.add_marked(rng(insert_at, insert_at), decl, vec![(0, name.len())]);
    for occ in &occurrences {
        ed.add_marked(*occ, name.clone(), vec![(0, name.len())]);
    }
    let (edits, rename_ranges) = ed.finish();
    Ok(Refactoring {
        title: format!("Introduce constant '{name}'"),
        edits,
        rename_ranges,
        name,
    })
}

fn names_resolving_to_scope(file: &FileInfo, expr: &ast::Expr, scope: ScopeId) -> Vec<String> {
    let mut out = Vec::new();
    for n in expr
        .syntax()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::NAME_EXPR)
    {
        if let Some(r) = file.scopes.reference_at(n.text_range().start()) {
            if r.resolved
                .is_some_and(|b| file.scopes.binding(b).scope == scope)
                && !out.contains(&r.name.to_string())
            {
                out.push(r.name.to_string());
            }
        }
    }
    out
}

/// Introduces `self.<name> = <expr>` (in `__init__` when the expression does
/// not depend on locals of the current method) and replaces the expression.
pub fn introduce_field(
    db: &Analysis,
    file: &Arc<FileInfo>,
    range: TextRange,
) -> Result<Refactoring, String> {
    introduce_field_at(db, file, range, false)
}

/// `current_method` prevents moving evaluation into __init__.
pub fn introduce_field_at(
    db: &Analysis,
    file: &Arc<FileInfo>,
    range: TextRange,
    current_method: bool,
) -> Result<Refactoring, String> {
    let _ = db;
    let text: &str = &file.text;
    let expr = expr_at(file, range).ok_or("Select an expression")?;
    check_extractable(&expr)?;
    let stmt = stmt_of(expr.syntax()).ok_or("Cannot find the containing statement")?;
    let func = enclosing_func(expr.syntax())
        .filter(|f| f.is_method())
        .ok_or("Place the caret inside a method")?;
    let class = func
        .syntax()
        .parent()
        .and_then(|b| b.parent())
        .and_then(ast::ClassDef::cast)
        .ok_or("Place the caret inside a method")?;
    let self_name = func
        .params()
        .and_then(|p| p.plain_params().next())
        .map(|p| p.name_text())
        .filter(|n| n == "self")
        .ok_or("The method has no 'self' parameter")?;
    check_not_in_comprehension(file, &expr, stmt.syntax())?;
    // Names already used on the instance.
    let mut taken: HashSet<String> = class
        .syntax()
        .descendants()
        .filter_map(ast::AttributeExpr::cast)
        .filter(|a| matches!(a.object(), Some(ast::Expr::Name(n)) if n.name_text() == "self"))
        .map(|a| a.attr_text())
        .collect();
    if let Some(cs) = file.scopes.scope_for_node(class.syntax().text_range()) {
        taken.extend(file.scopes.scope(cs).bindings.keys().map(|k| k.to_string()));
    }
    let name = suggest_name(&expr, &taken, false);
    let func_scope = file.scopes.scope_at(expr.text_range().start());
    let locals: Vec<String> = names_resolving_to_scope(file, &expr, func_scope)
        .into_iter()
        .filter(|n| *n != self_name)
        .collect();
    let init = class.methods().find(|m| m.name_text() == "__init__");
    let expr_text = slice(text, expr.text_range()).trim().to_string();
    let (insert_at, insert_text, mark) = match init {
        Some(init)
            if !current_method
                && locals.is_empty()
                && init.syntax().text_range() != func.syntax().text_range() =>
        {
            let body = init.body().ok_or("__init__ has no body")?;
            let last = body.statements().last().ok_or("__init__ has no body")?;
            let at = usize::from(stmt_lines(text, last.text_range()).end());
            let ind = indent_at(text, last.text_range().start().into());
            let nl = if at == text.len() && !text.ends_with('\n') {
                "\n"
            } else {
                ""
            };
            (
                at,
                format!("{nl}{ind}self.{name} = {expr_text}\n"),
                nl.len() + ind.len() + 5,
            )
        }
        _ => {
            let at = line_start(text, stmt.text_range().start().into());
            let ind = indent_at(text, stmt.text_range().start().into());
            (
                at,
                format!("{ind}self.{name} = {expr_text}\n"),
                ind.len() + 5,
            )
        }
    };
    let mut ed = Edits::default();
    ed.add_marked(
        rng(insert_at, insert_at),
        insert_text,
        vec![(mark, name.len())],
    );
    ed.add_marked(
        expr.text_range(),
        format!("self.{name}"),
        vec![(5, name.len())],
    );
    let (edits, rename_ranges) = ed.finish();
    Ok(Refactoring {
        title: format!("Introduce field 'self.{name}'"),
        edits,
        rename_ranges,
        name,
    })
}

/// Adds a parameter with the expression as its default value and replaces
/// the expression with the parameter name.
pub fn introduce_parameter(
    db: &Analysis,
    file: &Arc<FileInfo>,
    range: TextRange,
) -> Result<Refactoring, String> {
    let _ = db;
    let text: &str = &file.text;
    let expr = expr_at(file, range).ok_or("Select an expression")?;
    check_extractable(&expr)?;
    let func = enclosing_func(expr.syntax()).ok_or("Place the caret inside a function")?;
    let body = func.body().ok_or("The function has no body")?;
    if !body.syntax().text_range().contains_range(expr.text_range()) {
        return Err("Select an expression inside the function body".into());
    }
    let func_scope = file.scopes.scope_at(expr.text_range().start());
    let locals = names_resolving_to_scope(file, &expr, func_scope);
    if !locals.is_empty() {
        return Err(format!(
            "The expression depends on local variable(s): {}",
            locals.join(", ")
        ));
    }
    let mut taken = taken_names(file, expr.text_range().start());
    if let Some(fs) = file.scopes.scope_for_node(func.syntax().text_range()) {
        taken.extend(shadowable_names(file, fs));
    }
    let name = suggest_name(&expr, &taken, false);
    let params = func.params().ok_or("The function has no parameter list")?;
    let expr_text = slice(text, expr.text_range()).trim().to_string();
    let star_star = params.params().find_map(|p| match p {
        ast::AnyParam::StarStar(s) => Some(s),
        _ => None,
    });
    let any_param = params.params().next().is_some();
    let (at, ptext, mark) = match star_star {
        Some(s) => (
            usize::from(s.text_range().start()),
            format!("{name}={expr_text}, "),
            0,
        ),
        None => {
            let close = params
                .syntax()
                .last_token()
                .filter(|t| t.kind() == SyntaxKind::R_PAREN)
                .ok_or("Malformed parameter list")?;
            let at: usize = close.text_range().start().into();
            let sep = if any_param { ", " } else { "" };
            (at, format!("{sep}{name}={expr_text}"), sep.len())
        }
    };
    let mut ed = Edits::default();
    ed.add_marked(rng(at, at), ptext, vec![(mark, name.len())]);
    ed.add_marked(expr.text_range(), name.clone(), vec![(0, name.len())]);
    let (edits, rename_ranges) = ed.finish();
    Ok(Refactoring {
        title: format!("Introduce parameter '{name}'"),
        edits,
        rename_ranges,
        name,
    })
}

// ---------------------------------------------------------------------------
// Change signature
// ---------------------------------------------------------------------------

fn func_at(file: &FileInfo, offset: TextSize) -> Result<ast::FuncDef, String> {
    let root = file.parse.syntax_node();
    ancestor_at::<ast::FuncDef>(&root, offset)
        .ok_or_else(|| "Place the caret on a function".to_string())
}

/// Call sites of `func` in this file: `(arg list, called through an instance/attribute)`.
fn call_sites(
    db: &Analysis,
    file: &Arc<FileInfo>,
    func: &ast::FuncDef,
) -> Vec<(ast::ArgList, bool)> {
    let root = file.parse.syntax_node();
    let Some(name_tok) = func.name() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for u in usages::usages_in_file(db, file, name_tok.text_range().start()) {
        if u.is_definition {
            continue;
        }
        let Some(tok) = token_at(&root, u.range.start()) else {
            continue;
        };
        let Some(parent) = tok.parent() else { continue };
        let (callee, via_attr) = match parent.kind() {
            SyntaxKind::NAME_EXPR => (parent, false),
            SyntaxKind::ATTRIBUTE_EXPR => (parent, true),
            _ => continue,
        };
        let Some(call) = callee.parent().and_then(ast::CallExpr::cast) else {
            continue;
        };
        if call.callee().map(|c| c.text_range()) != Some(callee.text_range()) {
            continue;
        }
        if let Some(args) = call.arg_list() {
            out.push((args, via_attr));
        }
    }
    out
}

fn line_no(text: &str, off: usize) -> usize {
    text[..off.min(text.len())].matches('\n').count() + 1
}

/// Adds parameter `name` to the function at `func_offset`. With a `default`
/// the parameter is optional and call sites are untouched; without one it
/// becomes a required positional parameter and every call in this file gets
/// a placeholder argument named like the parameter.
pub fn add_parameter(
    db: &Analysis,
    file: &Arc<FileInfo>,
    func_offset: TextSize,
    name: &str,
    default: Option<&str>,
) -> Result<Refactoring, String> {
    let func_offset = rustpy_semantic::clamp_offset(file, func_offset);
    if !is_identifier(name) {
        return Err(format!("'{name}' is not a valid parameter name"));
    }
    let text: &str = &file.text;
    let func = func_at(file, func_offset)?;
    let params = func.params().ok_or("The function has no parameter list")?;
    if params
        .params()
        .any(|p| p.name().is_some_and(|t| t.text() == name))
    {
        return Err(format!("Parameter '{name}' already exists"));
    }
    let all: Vec<ast::AnyParam> = params.params().collect();
    let close = params
        .syntax()
        .last_token()
        .filter(|t| t.kind() == SyntaxKind::R_PAREN)
        .ok_or("Malformed parameter list")?;
    let mut ed = Edits::default();
    let (at, ptext, mark) = match default {
        Some(d) => {
            let star_star = all.iter().find_map(|p| match p {
                ast::AnyParam::StarStar(s) => Some(s.text_range().start()),
                _ => None,
            });
            match star_star {
                Some(s) => (usize::from(s), format!("{name}={d}, "), 0),
                None => {
                    let sep = if all.is_empty() { "" } else { ", " };
                    (
                        usize::from(close.text_range().start()),
                        format!("{sep}{name}={d}"),
                        sep.len(),
                    )
                }
            }
        }
        None => {
            // After the last required positional parameter.
            let last_required = all.iter().rev().find_map(|p| match p {
                ast::AnyParam::Plain(pp) if pp.default().is_none() => Some(pp.text_range().end()),
                _ => None,
            });
            match last_required {
                Some(end) => (usize::from(end), format!(", {name}"), 2),
                None => {
                    let open = params
                        .syntax()
                        .first_token()
                        .filter(|t| t.kind() == SyntaxKind::L_PAREN)
                        .ok_or("Malformed parameter list")?;
                    let sep = if all.is_empty() { "" } else { ", " };
                    (
                        usize::from(open.text_range().end()),
                        format!("{name}{sep}"),
                        0,
                    )
                }
            }
        }
    };
    ed.add_marked(rng(at, at), ptext, vec![(mark, name.len())]);
    if default.is_none() {
        let required_before = all
            .iter()
            .filter(|p| matches!(p, ast::AnyParam::Plain(pp) if pp.default().is_none()))
            .count();
        for (args, via_attr) in call_sites(db, file, &func) {
            let shift = usize::from(via_attr && func.is_method());
            let index = required_before.saturating_sub(shift);
            let positional: Vec<ast::Arg> = args
                .syntax()
                .children()
                .filter_map(ast::Arg::cast)
                .collect();
            if args.args().any(|a| matches!(a, ast::AnyArg::Star(_))) {
                return Err(format!(
                    "Call at line {} uses *args and cannot be updated automatically",
                    line_no(text, args.text_range().start().into())
                ));
            }
            if index < positional.len() && index > 0 {
                let end: usize = positional[index - 1].text_range().end().into();
                ed.add(rng(end, end), format!(", {name}"));
            } else if let Some(last) = positional.last() {
                let end: usize = last.text_range().end().into();
                ed.add(rng(end, end), format!(", {name}"));
            } else {
                let open = args
                    .syntax()
                    .first_token()
                    .filter(|t| t.kind() == SyntaxKind::L_PAREN)
                    .ok_or("Malformed call")?;
                let at: usize = open.text_range().end().into();
                let sep = if args.args().next().is_some() {
                    ", "
                } else {
                    ""
                };
                ed.add(rng(at, at), format!("{name}{sep}"));
            }
        }
    }
    let (edits, rename_ranges) = ed.finish();
    Ok(Refactoring {
        title: format!("Add parameter '{name}'"),
        edits,
        rename_ranges,
        name: name.to_string(),
    })
}

/// Removes the plain parameter at `index` (counting `self`) from the function
/// at `func_offset` and from every call in this file.
pub fn remove_parameter(
    db: &Analysis,
    file: &Arc<FileInfo>,
    func_offset: TextSize,
    index: usize,
) -> Result<Refactoring, String> {
    let func_offset = rustpy_semantic::clamp_offset(file, func_offset);
    let text: &str = &file.text;
    let func = func_at(file, func_offset)?;
    let params = func.params().ok_or("The function has no parameter list")?;
    let plain: Vec<ast::Param> = params.plain_params().collect();
    let param = plain
        .get(index)
        .ok_or_else(|| format!("No parameter #{index}"))?;
    let pname = param.name_text();
    if func.is_method() && index == 0 && (pname == "self" || pname == "cls") {
        return Err(format!("Cannot remove '{pname}'"));
    }
    if let Some(body) = func.body() {
        let used = body
            .syntax()
            .descendants()
            .any(|n| n.kind() == SyntaxKind::NAME_EXPR && n.text().to_string() == pname);
        if used {
            return Err(format!("'{pname}' is used in the function body"));
        }
    }
    let mut ed = Edits::default();
    ed.add(element_with_comma(text, param.syntax()), "");
    for (args, via_attr) in call_sites(db, file, &func) {
        let shift = usize::from(via_attr && func.is_method());
        if index < shift {
            continue;
        }
        let pos_index = index - shift;
        let positional: Vec<ast::Arg> = args
            .syntax()
            .children()
            .filter_map(ast::Arg::cast)
            .collect();
        if args.args().any(|a| matches!(a, ast::AnyArg::Star(_))) && pos_index >= positional.len() {
            return Err(format!(
                "Call at line {} uses *args and cannot be updated automatically",
                line_no(text, args.text_range().start().into())
            ));
        }
        if let Some(a) = positional.get(pos_index) {
            ed.add(element_with_comma(text, a.syntax()), "");
        } else if let Some(k) = args.keywords().find(|k| k.name_text() == pname) {
            ed.add(element_with_comma(text, k.syntax()), "");
        }
    }
    let (edits, _) = ed.finish();
    Ok(Refactoring {
        title: format!("Remove parameter '{pname}'"),
        edits,
        rename_ranges: Vec::new(),
        name: pname,
    })
}

/// Reorders the plain parameters: `order[i]` is the current index of the
/// parameter that becomes the `i`-th one. Positional arguments at call
/// sites are permuted accordingly.
pub fn reorder_parameters(
    db: &Analysis,
    file: &Arc<FileInfo>,
    func_offset: TextSize,
    order: &[usize],
) -> Result<Refactoring, String> {
    let func_offset = rustpy_semantic::clamp_offset(file, func_offset);
    let text: &str = &file.text;
    let func = func_at(file, func_offset)?;
    let params = func.params().ok_or("The function has no parameter list")?;
    let plain: Vec<ast::Param> = params.plain_params().collect();
    if order.len() != plain.len() {
        return Err(format!(
            "Expected {} indices, got {}",
            plain.len(),
            order.len()
        ));
    }
    let mut seen = vec![false; plain.len()];
    for &o in order {
        if o >= plain.len() || seen[o] {
            return Err("Invalid parameter order".into());
        }
        seen[o] = true;
    }
    let is_method = func.is_method();
    if is_method && !order.is_empty() && order[0] != 0 && plain[0].name_text() == "self" {
        return Err("'self' must stay first".into());
    }
    // Defaults must stay after non-defaults.
    let mut seen_default = false;
    for &o in order {
        let has_default = plain[o].default().is_some();
        if seen_default && !has_default {
            return Err("A parameter without a default cannot follow one with a default".into());
        }
        seen_default |= has_default;
    }
    let mut ed = Edits::default();
    for (i, &o) in order.iter().enumerate() {
        if i != o {
            ed.add(plain[i].text_range(), plain[o].text().to_string());
        }
    }
    for (args, via_attr) in call_sites(db, file, &func) {
        let shift = usize::from(via_attr && is_method);
        if args.args().any(|a| matches!(a, ast::AnyArg::Star(_))) {
            return Err(format!(
                "Call at line {} uses *args and cannot be updated automatically",
                line_no(text, args.text_range().start().into())
            ));
        }
        let positional: Vec<ast::Arg> = args
            .syntax()
            .children()
            .filter_map(ast::Arg::cast)
            .collect();
        let n = positional.len();
        let positional_ok = (0..n).all(|j| {
            order
                .get(j + shift)
                .is_some_and(|&src| src >= shift && src - shift < n)
        });
        if positional_ok {
            for j in 0..n {
                let from = order[j + shift] - shift;
                if from != j {
                    ed.add(
                        positional[j].text_range(),
                        positional[from].text().to_string(),
                    );
                }
            }
        } else {
            // Some moved parameters are passed by keyword: pass every positional argument by name.
            for (j, a) in positional.iter().enumerate() {
                let Some(p) = plain.get(j + shift) else { break };
                ed.add(a.text_range(), format!("{}={}", p.name_text(), a.text()));
            }
        }
    }
    let (edits, _) = ed.finish();
    Ok(Refactoring {
        title: "Reorder parameters".into(),
        edits,
        rename_ranges: Vec::new(),
        name: func.name_text(),
    })
}

// ---------------------------------------------------------------------------
// Safe delete
// ---------------------------------------------------------------------------

/// Why Safe Delete produced no edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SafeDeleteError {
    /// Nothing that can be deleted at the caret.
    Invalid(String),
    /// The symbol is still used: every usage across the project.
    Used {
        name: String,
        usages: Vec<usages::Usage>,
    },
}

impl std::fmt::Display for SafeDeleteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SafeDeleteError::Invalid(m) => f.write_str(m),
            SafeDeleteError::Used { name, usages } => {
                let files: HashSet<&std::path::Path> =
                    usages.iter().map(|u| u.path.as_path()).collect();
                write!(
                    f,
                    "'{name}' has {} usage(s) in {} file(s)",
                    usages.len(),
                    files.len()
                )
            }
        }
    }
}

impl From<String> for SafeDeleteError {
    fn from(m: String) -> Self {
        SafeDeleteError::Invalid(m)
    }
}

impl From<&str> for SafeDeleteError {
    fn from(m: &str) -> Self {
        SafeDeleteError::Invalid(m.to_string())
    }
}

/// The definition Safe Delete removes.
struct DeletePlan {
    name: String,
    /// The whole definition: uses inside it (recursion) do not block.
    def_range: TextRange,
    edits: Vec<Edit>,
    /// For an import alias: its binding (only this file can use it).
    import_binding: Option<BindingId>,
}

fn plan_delete(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
) -> Result<DeletePlan, String> {
    let text: &str = &file.text;
    let root = file.parse.syntax_node();
    // Import aliases: `import os` / `from m import x` (resolved through the scope model).
    if let Some((id, b)) = file
        .scopes
        .binding_at(offset)
        .filter(|(_, b)| matches!(b.kind, BindingKind::Import(_)))
    {
        let name = b.name.to_string();
        let alias = node_with_range(&root, b.node_range).ok_or("Cannot find the import")?;
        let alias = alias
            .ancestors()
            .find(|n| n.kind() == SyntaxKind::IMPORT_ALIAS)
            .ok_or("Cannot find the import")?;
        let stmt = alias.parent().ok_or("Malformed import")?;
        let aliases = stmt
            .children()
            .filter(|c| c.kind() == SyntaxKind::IMPORT_ALIAS)
            .count();
        let edits = if aliases <= 1 {
            statement_removals(text, &[stmt])
        } else {
            vec![(element_with_comma(text, &alias), String::new())]
        };
        return Ok(DeletePlan {
            name,
            def_range: alias.text_range(),
            edits,
            import_binding: Some(id),
        });
    }
    let sym = usages::symbol_at(db, file, offset).ok_or("Place the caret on a definition")?;
    let name = sym.name().to_string();
    let mut import_binding = None;
    let def_node: SyntaxNode = match &sym {
        Symbol::Name { def, .. } => {
            if def.file.path != file.path {
                return Err(format!("'{name}' is defined in another file"));
            }
            let b = def.binding();
            if matches!(b.kind, BindingKind::Import(_)) {
                import_binding = Some(def.binding);
            }
            node_with_range(&root, b.node_range).ok_or("Cannot find the definition")?
        }
        Symbol::Member {
            decl: Some((f, r)), ..
        } if f.path == file.path => {
            // The declaration range is the name token (`def grow`, `self.size = ...`).
            let tok = token_at(&root, r.start()).ok_or("Cannot find the definition")?;
            tok.parent_ancestors()
                .find(|a| {
                    matches!(
                        a.kind(),
                        SyntaxKind::FUNC_DEF
                            | SyntaxKind::ASSIGN_STMT
                            | SyntaxKind::ANN_ASSIGN_STMT
                            | SyntaxKind::CLASS_DEF
                    )
                })
                .ok_or("Cannot find the definition")?
        }
        _ => return Err(format!("Cannot determine the definition of '{name}'")),
    };
    let def_range = def_node.text_range();
    let mut ed = Edits::default();
    match def_node.kind() {
        SyntaxKind::IMPORT_ALIAS => {
            let stmt = def_node.parent().ok_or("Malformed import")?;
            let aliases = stmt
                .children()
                .filter(|c| c.kind() == SyntaxKind::IMPORT_ALIAS)
                .count();
            if aliases <= 1 {
                for (r, t) in statement_removals(text, &[stmt]) {
                    ed.add(r, t);
                }
            } else {
                ed.add(element_with_comma(text, &def_node), "");
            }
        }
        SyntaxKind::PARAM | SyntaxKind::STAR_PARAM | SyntaxKind::STAR_STAR_PARAM => {
            return Err("Use Change Signature to remove a parameter".into());
        }
        _ => {
            let stmt = def_node
                .ancestors()
                .find(|n| ast::Stmt::can_cast(n.kind()))
                .ok_or("Cannot find the definition statement")?;
            if let Some(a) = ast::AssignStmt::cast(stmt.clone()) {
                if a.targets().len() != 1
                    || !matches!(
                        &a.targets()[0],
                        ast::Expr::Name(_) | ast::Expr::Attribute(_)
                    )
                {
                    return Err(
                        "Cannot delete one target of a multiple/destructuring assignment".into(),
                    );
                }
                if matches!(&a.targets()[0], ast::Expr::Attribute(_))
                    && !matches!(&sym, Symbol::Member { .. })
                {
                    return Err("Cannot determine the attribute's physical declaration".into());
                }
            }
            for (r, t) in statement_removals(text, &[stmt]) {
                ed.add(r, t);
            }
        }
    }
    let (edits, _) = ed.finish();
    Ok(DeletePlan {
        name,
        def_range,
        edits,
        import_binding,
    })
}

/// Usages that keep the definition of `plan` alive, across the project.
fn delete_blockers(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    plan: &DeletePlan,
    project_files: &[std::path::PathBuf],
) -> Vec<usages::Usage> {
    let mut out: Vec<usages::Usage> = match plan.import_binding {
        // An import binding: its uses in this file, and in files importing it
        // from here (a re-export, `from .core import Engine` in `pkg/__init__.py`).
        Some(id) => {
            let mut v: Vec<usages::Usage> = file
                .scopes
                .references_to(id)
                .into_iter()
                .map(|r| usages::usage_at(file, r.range, r.is_write, false))
                .collect();
            v.extend(rustpy_search::usages::reexport_usages(
                db,
                &rustpy_semantic::infer::Definition {
                    file: file.clone(),
                    binding: id,
                },
                project_files,
            ));
            v
        }
        None => rustpy_search::usages::find_usages(db, file, offset, project_files)
            .into_iter()
            .filter(|u| {
                !u.is_definition && !(u.path == file.path && plan.def_range.contains_range(u.range))
            })
            .collect(),
    };
    out.sort_by(|a, b| {
        a.path
            .cmp(&b.path)
            .then(a.range.start().cmp(&b.range.start()))
    });
    out.dedup_by(|a, b| a.path == b.path && a.range == b.range);
    out
}

/// Deletes the definition at `offset` when nothing in the project uses it;
/// otherwise reports every usage ([`SafeDeleteError::Used`]).
pub fn safe_delete(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    project_files: &[std::path::PathBuf],
) -> Result<Refactoring, SafeDeleteError> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let plan = plan_delete(db, file, offset)?;
    let usages = delete_blockers(db, file, offset, &plan, project_files);
    if !usages.is_empty() {
        return Err(SafeDeleteError::Used {
            name: plan.name,
            usages,
        });
    }
    Ok(Refactoring {
        title: format!("Safe delete '{}'", plan.name),
        edits: plan.edits,
        rename_ranges: Vec::new(),
        name: plan.name,
    })
}

/// "Delete Anyway": deletes the definition (or re-exporting import) at
/// `offset` although it is still used, and removes the `from module import
/// name` imports of it from other files, which would otherwise fail when the
/// importing module loads. A block left without statements keeps a `pass`.
pub fn safe_delete_anyway(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    project_files: &[std::path::PathBuf],
) -> Result<crate::refactor2::MultiFileRefactoring, String> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let plan = plan_delete(db, file, offset)?;
    let mut changes: Vec<(std::path::PathBuf, Vec<Edit>)> =
        vec![(file.path.clone(), plan.edits.clone())];
    // Import aliases naming the symbol, grouped by importing statement:
    // (file, statement start) -> (file, statement, removed aliases).
    type Removals = (Arc<FileInfo>, TextRange, Vec<TextRange>);
    let mut by_stmt: BTreeMap<(std::path::PathBuf, TextSize), Removals> = BTreeMap::new();
    for u in delete_blockers(db, file, offset, &plan, project_files) {
        if u.path == file.path {
            continue;
        }
        let Some(f) = db.file_for_path(&u.path) else {
            continue;
        };
        let Some(tok) = token_at(&f.parse.syntax_node(), u.range.start()) else {
            continue;
        };
        let Some(alias) = tok
            .parent()
            .filter(|p| p.kind() == SyntaxKind::IMPORT_ALIAS)
        else {
            continue;
        };
        let Some(stmt) = alias
            .parent()
            .filter(|p| p.kind() == SyntaxKind::IMPORT_FROM_STMT)
        else {
            continue;
        };
        let entry = by_stmt
            .entry((u.path.clone(), stmt.text_range().start()))
            .or_insert_with(|| (f.clone(), stmt.text_range(), Vec::new()));
        if !entry.2.contains(&alias.text_range()) {
            entry.2.push(alias.text_range());
        }
    }
    // Per file: statements losing every alias (removed together, so that a
    // block they empty keeps a `pass`), and edits to the others.
    type FileRemovals = (Arc<FileInfo>, Vec<SyntaxNode>, Vec<Edit>);
    let mut by_file: BTreeMap<std::path::PathBuf, FileRemovals> = BTreeMap::new();
    for ((path, _), (f, stmt_range, removed)) in by_stmt {
        let text: &str = &f.text;
        let Some(node) = node_with_range(&f.parse.syntax_node(), stmt_range) else {
            continue;
        };
        let Some(stmt) = ast::ImportFromStmt::cast(node.clone()) else {
            continue;
        };
        let all: Vec<ast::ImportAlias> = stmt.aliases().collect();
        let entry = by_file
            .entry(path)
            .or_insert_with(|| (f.clone(), Vec::new(), Vec::new()));
        if removed.len() >= all.len() {
            entry.1.push(node);
        } else if let [only] = removed.as_slice() {
            let Some(alias) = all.iter().find(|a| a.syntax().text_range() == *only) else {
                continue;
            };
            entry
                .2
                .push((element_with_comma(text, alias.syntax()), String::new()));
        } else {
            let keep: Vec<String> = all
                .iter()
                .filter(|a| !removed.contains(&a.syntax().text_range()))
                .map(|a| a.syntax().text().to_string())
                .collect();
            entry.2.push((
                stmt_range,
                format!("from {} import {}", stmt.module_path(), keep.join(", ")),
            ));
        }
    }
    for (path, (f, whole, mut edits)) in by_file {
        edits.extend(statement_removals(&f.text, &whole));
        edits.sort_by_key(|(r, _)| r.start());
        match changes.iter_mut().find(|(p, _)| *p == path) {
            Some((_, v)) => v.extend(edits),
            None => changes.push((path, edits)),
        }
    }
    Ok(crate::refactor2::MultiFileRefactoring {
        title: format!("Safe delete '{}'", plan.name),
        changes,
    })
}

// ---------------------------------------------------------------------------
// Move to top level
// ---------------------------------------------------------------------------

/// What Move (F6) acts on at the caret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MoveTarget {
    /// A function nested in another function: lifted to the module level
    /// ([`move_to_top_level`]).
    NestedFunction(String),
    /// The top-level function or class around the caret: moved to another
    /// module (`refactor2::move_symbol`).
    TopLevel(String),
}

/// The Move refactoring that applies at `offset`, if any.
pub fn move_target_at(file: &FileInfo, offset: TextSize) -> Option<MoveTarget> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let tok = token_at(&file.parse.syntax_node(), offset)?;
    if let Some(func) = tok.parent_ancestors().find_map(ast::FuncDef::cast) {
        if !func.is_method() && enclosing_func(func.syntax()).is_some() {
            return Some(MoveTarget::NestedFunction(func.name_text()));
        }
    }
    let top = tok
        .parent_ancestors()
        .filter(|n| matches!(n.kind(), SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF))
        .last()?;
    if top.parent().map(|p| p.kind()) != Some(SyntaxKind::MODULE) {
        return None;
    }
    let name = ast::FuncDef::cast(top.clone())
        .map(|f| f.name_text())
        .or_else(|| ast::ClassDef::cast(top).map(|c| c.name_text()))?;
    (!name.is_empty()).then_some(MoveTarget::TopLevel(name))
}

/// Moves the nested function at `offset` out of its enclosing function to the
/// module level (after the enclosing top-level statement).
pub fn move_to_top_level(file: &Arc<FileInfo>, offset: TextSize) -> Result<Refactoring, String> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let text: &str = &file.text;
    let func = func_at(file, offset)?;
    if func.is_method() {
        return Err("Cannot move a method; use 'Convert static method to function'".into());
    }
    let outer = enclosing_func(func.syntax()).ok_or("The function is already at the top level")?;
    let _ = outer;
    let body_start = func
        .body()
        .map(|b| b.text_range().start())
        .unwrap_or(func.text_range().end());
    let func_scope = file.scopes.scope_at(body_start + ts(1));
    // Enclosing function scopes between the function and the module.
    let mut enclosing: Vec<ScopeId> = Vec::new();
    let mut cur = file.scopes.scope(func_scope).parent;
    while let Some(id) = cur {
        let s = file.scopes.scope(id);
        if s.kind != ScopeKind::Module {
            enclosing.push(id);
        }
        cur = s.parent;
    }
    let name = func.name_text();
    let mut free: Vec<String> = Vec::new();
    for r in &file.scopes.references {
        if !func.text_range().contains_range(r.range) {
            continue;
        }
        if let Some(b) = r.resolved {
            let bs = file.scopes.binding(b).scope;
            if enclosing.contains(&bs) && !free.contains(&r.name.to_string()) {
                free.push(r.name.to_string());
            }
        }
    }
    if !free.is_empty() {
        return Err(format!(
            "'{name}' uses variable(s) of the enclosing function: {}",
            free.join(", ")
        ));
    }
    if func
        .syntax()
        .descendants()
        .any(|n| n.kind() == SyntaxKind::NONLOCAL_STMT)
    {
        return Err(format!("'{name}' declares nonlocal variables"));
    }
    let top = func
        .syntax()
        .ancestors()
        .filter(|n| n.parent().is_some_and(|p| p.kind() == SyntaxKind::MODULE))
        .last()
        .ok_or("Cannot find the top-level statement")?;
    use rustpy_syntax::printer::{attached_range, Rewrite, TriviaAttachment};
    let root = func
        .syntax()
        .ancestors()
        .last()
        .ok_or("Cannot find the syntax root")?;
    let mut printer = Rewrite::new(root).map_err(|error| error.to_string())?;
    let lines = attached_range(text, func.syntax(), TriviaAttachment::LeadingComments)
        .map_err(|error| error.to_string())?;
    let indent = indent_at(text, func.text_range().start().into());
    let moved = reindent(slice(text, lines), &indent, "");
    let mut ed = Edits::default();
    let siblings = func
        .syntax()
        .parent()
        .map(|p| {
            p.children()
                .filter(|c| ast::Stmt::can_cast(c.kind()))
                .count()
        })
        .unwrap_or(2);
    let placeholder = if siblings <= 1 {
        format!("{indent}pass\n")
    } else {
        String::new()
    };
    let at = after_last_line(text, top.text_range().end().into());
    let nl = if at == text.len() && !text.ends_with('\n') {
        "\n"
    } else {
        ""
    };
    let name_token = func.name().ok_or("The function has no name token")?;
    let prefix = slice(
        text,
        TextRange::new(lines.start(), name_token.text_range().start()),
    );
    let def_off = reindent(prefix, &indent, "").len();
    let inserted = format!("{nl}\n\n{moved}");
    printer
        .relocate_statement(
            func.syntax(),
            ts(at),
            TriviaAttachment::LeadingComments,
            placeholder,
            |_| inserted.clone(),
        )
        .map_err(|error| error.to_string())?;
    for (range, replacement) in printer.edits() {
        if range.is_empty() && range.start() == ts(at) {
            ed.add_marked(
                *range,
                replacement.clone(),
                vec![(nl.len() + 2 + def_off, name.len())],
            );
        } else {
            ed.add(*range, replacement.clone());
        }
    }
    let (edits, rename_ranges) = ed.finish();
    Ok(Refactoring {
        title: format!("Move '{name}' to the top level"),
        edits,
        rename_ranges,
        name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn setup(src: &str) -> (Analysis, Arc<FileInfo>) {
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let f = db.file(id).unwrap();
        (db, f)
    }

    fn at(src: &str, needle: &str) -> TextSize {
        ts(src.find(needle).expect("needle"))
    }

    fn sel(src: &str, needle: &str) -> TextRange {
        let s = src.find(needle).expect("needle");
        rng(s, s + needle.len())
    }

    #[test]
    fn extract_variable_basic_and_all_occurrences() {
        let src = "def f(items):\n    print(len(items) + 1)\n    return len(items) * 2\n";
        let (db, f) = setup(src);
        let r = extract_variable(&db, &f, sel(src, "len(items)"), false).unwrap();
        assert_eq!(r.name, "length");
        let out = apply_edits(src, &r.edits);
        assert_eq!(out, "def f(items):\n    length = len(items)\n    print(length + 1)\n    return len(items) * 2\n");
        for rr in &r.rename_ranges {
            assert_eq!(slice(&out, *rr), "length");
        }
        assert_eq!(r.rename_ranges.len(), 2);
        let r = extract_variable(&db, &f, sel(src, "len(items)"), true).unwrap();
        let out = apply_edits(src, &r.edits);
        assert_eq!(out, "def f(items):\n    length = len(items)\n    print(length + 1)\n    return length * 2\n");
        assert_eq!(r.rename_ranges.len(), 3);
        for rr in &r.rename_ranges {
            assert_eq!(slice(&out, *rr), "length");
        }
    }

    #[test]
    fn extract_variable_caret_climbs_call_chain_and_anchors_common_block() {
        let src = "def f(a):\n    if a:\n        x = obj.compute(1)\n    y = obj.compute(1)\n    return x + y\n";
        let (db, f) = setup(src);
        let r = extract_variable(&db, &f, TextRange::empty(at(src, "obj.compute")), true).unwrap();
        let out = apply_edits(src, &r.edits);
        assert_eq!(out, "def f(a):\n    compute = obj.compute(1)\n    if a:\n        x = compute\n    y = compute\n    return x + y\n");
    }

    #[test]
    fn extract_variable_rejects_targets_and_comprehension_vars() {
        let src = "def f(xs):\n    y = 1\n    return [i * 2 for i in xs]\n";
        let (db, f) = setup(src);
        assert!(extract_variable(&db, &f, sel(src, "y"), false).is_err());
        assert!(extract_variable(&db, &f, sel(src, "i * 2"), false).is_err());
        let src2 = "value = compute()\n";
        let (db, f) = setup(src2);
        let r = extract_variable(&db, &f, sel(src2, "compute()"), false).unwrap();
        // Not `compute = compute()`, which would replace the function it calls.
        assert_eq!(r.name, "compute1");
        assert_eq!(
            apply_edits(src2, &r.edits),
            "compute1 = compute()\nvalue = compute1\n"
        );
    }

    #[test]
    fn inline_variable_parenthesizes_when_needed() {
        let src =
            "def f(a, b):\n    total = a + b\n    print(total * 2, total)\n    return total\n";
        let (db, f) = setup(src);
        let r = inline_variable(&db, &f, at(src, "total * 2")).unwrap();
        assert_eq!(
            apply_edits(src, &r.edits),
            "def f(a, b):\n    print((a + b) * 2, a + b)\n    return a + b\n"
        );
        let src = "def f(a):\n    x = 1\n    x = 2\n    return x\n";
        let (db, f) = setup(src);
        assert!(inline_variable(&db, &f, at(src, "return x") + ts(7)).is_err());
        let src = "def f(a):\n    x: int = a\n    return x\n";
        let (db, f) = setup(src);
        let r = inline_variable(&db, &f, at(src, "x: int")).unwrap();
        assert_eq!(apply_edits(src, &r.edits), "def f(a):\n    return a\n");
    }

    #[test]
    fn extract_method_in_function_with_params_and_outputs() {
        let src = "def process(items, factor):\n    total = 0\n    for it in items:\n        total += it * factor\n    count = len(items)\n    return total / count\n";
        let (db, f) = setup(src);
        let s = src.find("    for it").unwrap();
        let e = src.find("    return").unwrap();
        let r = extract_method(&db, &f, rng(s, e), "accumulate").unwrap();
        let out = apply_edits(src, &r.edits);
        assert_eq!(
            out,
            "def process(items, factor):\n    total = 0\n    total, count = accumulate(items, total, factor)\n    return total / count\n\n\ndef accumulate(items, total, factor):\n    for it in items:\n        total += it * factor\n    count = len(items)\n    return total, count\n"
        );
        for rr in &r.rename_ranges {
            assert_eq!(slice(&out, *rr), "accumulate");
        }
        assert_eq!(r.rename_ranges.len(), 2);
    }

    #[test]
    fn extract_method_from_method_uses_self() {
        let src = "class C:\n    def run(self, n):\n        self.count += n\n        print(self.count)\n        return n\n\n\nx = 1\n";
        let (db, f) = setup(src);
        let s = src.find("        self.count").unwrap();
        let e = src.find("        return").unwrap();
        let r = extract_method(&db, &f, rng(s, e), "bump").unwrap();
        let out = apply_edits(src, &r.edits);
        assert_eq!(out, "class C:\n    def run(self, n):\n        self.bump(n)\n        return n\n\n    def bump(self, n):\n        self.count += n\n        print(self.count)\n\n\nx = 1\n");
    }

    #[test]
    fn extract_method_errors() {
        let src = "def f(a):\n    if a:\n        return 1\n    return 2\n";
        let (db, f) = setup(src);
        let s = src.find("    if a").unwrap();
        let e = src.find("    return 2").unwrap();
        assert!(extract_method(&db, &f, rng(s, e), "g")
            .unwrap_err()
            .contains("return"));
        assert!(extract_method(&db, &f, rng(s + 2, e), "g").is_err());
        assert!(extract_method(&db, &f, rng(s, e), "1bad").is_err());
    }

    #[test]
    fn extract_method_at_module_level() {
        let src = "import os\n\nx = 1\ny = x + 1\nprint(y)\n";
        let (db, f) = setup(src);
        let s = src.find("y = x").unwrap();
        let e = src.find("print").unwrap();
        let r = extract_method(&db, &f, rng(s, e), "calc").unwrap();
        assert_eq!(apply_edits(src, &r.edits), "import os\n\nx = 1\ndef calc(x):\n    y = x + 1\n    return y\n\n\ny = calc(x)\nprint(y)\n");
    }

    #[test]
    fn introduce_constant_after_imports() {
        let src = "import os\n\ndef f():\n    return connect(\"localhost\", timeout=30)\n";
        let (db, f) = setup(src);
        let _ = db;
        let r = introduce_constant(&f, sel(src, "\"localhost\""), false).unwrap();
        assert_eq!(r.name, "LOCALHOST");
        assert_eq!(apply_edits(src, &r.edits), "import os\nLOCALHOST = \"localhost\"\n\ndef f():\n    return connect(LOCALHOST, timeout=30)\n");
        let r = introduce_constant(&f, sel(src, "30"), true).unwrap();
        assert_eq!(r.name, "TIMEOUT");
        let out = apply_edits(src, &r.edits);
        assert!(out.contains("TIMEOUT = 30\n"), "{out}");
        assert!(out.contains("timeout=TIMEOUT"), "{out}");
        assert!(introduce_constant(&f, sel(src, "connect"), false).is_err());
    }

    #[test]
    fn introduce_field_and_parameter() {
        let src = "class C:\n    def __init__(self):\n        self.a = 1\n\n    def m(self, k):\n        return compute(k) + 10\n";
        let (db, f) = setup(src);
        let r = introduce_field(&db, &f, sel(src, "10")).unwrap();
        assert_eq!(apply_edits(src, &r.edits), "class C:\n    def __init__(self):\n        self.a = 1\n        self.number = 10\n\n    def m(self, k):\n        return compute(k) + self.number\n");
        let r = introduce_field(&db, &f, sel(src, "compute(k)")).unwrap();
        assert_eq!(apply_edits(src, &r.edits), "class C:\n    def __init__(self):\n        self.a = 1\n\n    def m(self, k):\n        self.compute = compute(k)\n        return self.compute + 10\n");
        let r = introduce_parameter(&db, &f, sel(src, "10")).unwrap();
        assert_eq!(apply_edits(src, &r.edits), "class C:\n    def __init__(self):\n        self.a = 1\n\n    def m(self, k, number=10):\n        return compute(k) + number\n");
        assert!(introduce_parameter(&db, &f, sel(src, "compute(k)")).is_err());
    }

    #[test]
    fn change_signature_add_remove_reorder() {
        let src = "def f(a, b=1):\n    return a + b\n\nf(1)\nf(1, 2)\nf(1, b=3)\n";
        let (db, f) = setup(src);
        let r = add_parameter(&db, &f, at(src, "f(a"), "c", Some("0")).unwrap();
        assert_eq!(
            apply_edits(src, &r.edits),
            "def f(a, b=1, c=0):\n    return a + b\n\nf(1)\nf(1, 2)\nf(1, b=3)\n"
        );
        let r = add_parameter(&db, &f, at(src, "f(a"), "c", None).unwrap();
        assert_eq!(
            apply_edits(src, &r.edits),
            "def f(a, c, b=1):\n    return a + b\n\nf(1, c)\nf(1, c, 2)\nf(1, c, b=3)\n"
        );
        assert!(add_parameter(&db, &f, at(src, "f(a"), "a", None).is_err());
        let src2 = "def f(a, b, c=1):\n    return a + c\n\nf(1, 2)\nf(1, 2, c=3)\nf(1, b=2)\n";
        let (db, f) = setup(src2);
        let r = remove_parameter(&db, &f, at(src2, "f(a"), 1).unwrap();
        assert_eq!(
            apply_edits(src2, &r.edits),
            "def f(a, c=1):\n    return a + c\n\nf(1)\nf(1, c=3)\nf(1)\n"
        );
        assert!(remove_parameter(&db, &f, at(src2, "f(a"), 0).is_err());
        let r = reorder_parameters(&db, &f, at(src2, "f(a"), &[1, 0, 2]).unwrap();
        assert_eq!(
            apply_edits(src2, &r.edits),
            "def f(b, a, c=1):\n    return a + c\n\nf(2, 1)\nf(2, 1, c=3)\nf(a=1, b=2)\n"
        );
        assert!(reorder_parameters(&db, &f, at(src2, "f(a"), &[2, 0, 1]).is_err());
    }

    #[test]
    fn change_signature_on_methods() {
        let src = "class C:\n    def m(self, a, b):\n        return a\n\nc = C()\nc.m(1, 2)\n";
        let (db, f) = setup(src);
        let r = remove_parameter(&db, &f, at(src, "m(self"), 2).unwrap();
        assert_eq!(
            apply_edits(src, &r.edits),
            "class C:\n    def m(self, a):\n        return a\n\nc = C()\nc.m(1)\n"
        );
        let r = add_parameter(&db, &f, at(src, "m(self"), "z", None).unwrap();
        assert_eq!(
            apply_edits(src, &r.edits),
            "class C:\n    def m(self, a, b, z):\n        return a\n\nc = C()\nc.m(1, 2, z)\n"
        );
    }

    #[test]
    fn safe_delete_checks_usages() {
        let src = "import os, sys\n\ndef unused():\n    pass\n\ndef used():\n    return sys.argv\n\nused()\n";
        let (db, f) = setup(src);
        let files = vec![f.path.clone()];
        let r = safe_delete(&db, &f, at(src, "unused"), &files).unwrap();
        assert_eq!(
            apply_edits(src, &r.edits),
            "import os, sys\n\ndef used():\n    return sys.argv\n\nused()\n"
        );
        match safe_delete(&db, &f, at(src, "def used") + ts(4), &files).unwrap_err() {
            SafeDeleteError::Used { name, usages } => {
                assert_eq!(name, "used");
                assert_eq!(
                    usages.iter().map(|u| u.line + 1).collect::<Vec<_>>(),
                    vec![9]
                );
            }
            e => panic!("{e}"),
        }
        let r = safe_delete(&db, &f, at(src, "os"), &files).unwrap();
        assert_eq!(
            apply_edits(src, &r.edits),
            "import sys\n\ndef unused():\n    pass\n\ndef used():\n    return sys.argv\n\nused()\n"
        );
        // The import is used in this file: only this file is consulted.
        assert!(matches!(
            safe_delete(&db, &f, at(src, "sys\n"), &files),
            Err(SafeDeleteError::Used { .. })
        ));
    }

    #[test]
    fn safe_delete_sees_usages_in_other_files() {
        let db = Analysis::for_project(Path::new("/proj"));
        let main = "LIMIT = 3\n\n\ndef helper():\n    return helper\n\n\nclass Box:\n    def grow(self):\n        pass\n\n    def spare(self):\n        pass\n";
        let other = "from main import helper, LIMIT\nfrom main import Box\n\n\ndef run(b: Box):\n    b.grow()\n    return helper(), LIMIT\n";
        let mp = PathBuf::from("/proj/main.py");
        let op = PathBuf::from("/proj/other.py");
        db.set_file_text(&mp, main);
        db.set_file_text(&op, other);
        let f = db.file_for_path(&mp).unwrap();
        let files = vec![mp.clone(), op.clone()];
        // A function used only from another module (its own recursion does not count).
        match safe_delete(&db, &f, at(main, "helper():"), &files).unwrap_err() {
            SafeDeleteError::Used { usages, .. } => {
                assert!(usages.iter().all(|u| u.path == op), "{usages:?}");
                assert_eq!(usages.len(), 2, "import + call: {usages:?}");
            }
            e => panic!("{e}"),
        }
        // A method used through a typed instance in another file.
        assert!(matches!(
            safe_delete(&db, &f, at(main, "grow"), &files),
            Err(SafeDeleteError::Used { .. })
        ));
        assert!(safe_delete(&db, &f, at(main, "spare"), &files).is_ok());
        assert!(matches!(
            safe_delete(&db, &f, at(main, "LIMIT"), &files),
            Err(SafeDeleteError::Used { .. })
        ));
        // Delete Anyway removes the definition and the import of it.
        let m = safe_delete_anyway(&db, &f, at(main, "helper():"), &files).unwrap();
        let edits = |p: &PathBuf| {
            m.changes
                .iter()
                .find(|(q, _)| q == p)
                .map(|(_, e)| e.clone())
                .unwrap_or_default()
        };
        assert_eq!(apply_edits(main, &edits(&mp)), "LIMIT = 3\n\n\nclass Box:\n    def grow(self):\n        pass\n\n    def spare(self):\n        pass\n");
        assert_eq!(apply_edits(other, &edits(&op)), "from main import LIMIT\nfrom main import Box\n\n\ndef run(b: Box):\n    b.grow()\n    return helper(), LIMIT\n");
        let m = safe_delete_anyway(&db, &f, at(main, "class Box") + ts(6), &files).unwrap();
        assert_eq!(apply_edits(other, &m.changes.iter().find(|(q, _)| *q == op).map(|(_, e)| e.clone()).unwrap()), "from main import helper, LIMIT\n\n\ndef run(b: Box):\n    b.grow()\n    return helper(), LIMIT\n");
    }

    #[test]
    fn extract_occurrences_offer_this_or_all() {
        let src = "def f(items, x):\n    a = len(items) + 1\n    print(len(items))\n    items.append(x)\n    b = len(items)\n    return a + b\n";
        let (db, f) = setup(src);
        // The mutation stops "all" before the third occurrence.
        let occ = extract_occurrences(&f, sel(src, "len(items)"), false);
        assert_eq!(occ.len(), 2, "{occ:?}");
        let r = extract_variable(&db, &f, sel(src, "len(items)"), true).unwrap();
        assert_eq!(apply_edits(src, &r.edits), "def f(items, x):\n    length = len(items)\n    a = length + 1\n    print(length)\n    items.append(x)\n    b = len(items)\n    return a + b\n");
        let r = extract_variable(&db, &f, sel(src, "len(items)"), false).unwrap();
        assert_eq!(apply_edits(src, &r.edits), "def f(items, x):\n    length = len(items)\n    a = length + 1\n    print(len(items))\n    items.append(x)\n    b = len(items)\n    return a + b\n");
        // Starting after the mutation only reaches later occurrences.
        let third = src.rfind("len(items)").unwrap();
        assert_eq!(
            extract_occurrences(&f, rng(third, third + 10), false),
            vec![rng(third, third + 10)]
        );
        // An assignment to a name the expression reads.
        let src = "def g(n):\n    a = n * 2\n    n = n + 1\n    return a + n * 2\n";
        let (_, f) = setup(src);
        assert_eq!(extract_occurrences(&f, sel(src, "n * 2"), false).len(), 1);
        // A loop that mutates the list: the occurrence inside cannot be hoisted.
        let src = "def h(items):\n    total = sum(items)\n    for i in range(3):\n        print(sum(items))\n        items.append(i)\n";
        let (_, f) = setup(src);
        assert_eq!(
            extract_occurrences(&f, sel(src, "sum(items)"), false).len(),
            1
        );
        // Pure reuse is offered.
        let src = "def k(a, b):\n    if a:\n        return (a + b) * 2\n    return (a + b) * 3\n";
        let (_, f) = setup(src);
        assert_eq!(extract_occurrences(&f, sel(src, "(a + b)"), false).len(), 2);
    }

    #[test]
    fn introduce_constant_occurrences_skip_patterns_annotations_and_shadowing() {
        let src = "from typing import Literal\n\n\ndef f(x: Literal[1], NUMBER=0):\n    match x:\n        case 1:\n            return x[1]\n    return x + 1\n\n\ndef g(y):\n    return y + 1\n";
        let (_, f) = setup(src);
        let ret = src.find("y + 1").unwrap() + 4;
        assert!(is_trivial_literal(&f, rng(ret, ret + 1)));
        let occ = extract_occurrences(&f, rng(ret, ret + 1), true);
        // Only g's `1`: f binds NUMBER (shadowing), the annotation and the case pattern stay literal.
        assert_eq!(occ, vec![rng(ret, ret + 1)]);
        let r = introduce_constant(&f, rng(ret, ret + 1), true).unwrap();
        assert_eq!(r.name, "NUMBER");
        assert!(apply_edits(src, &r.edits).contains("    return y + NUMBER\n"));
        assert!(apply_edits(src, &r.edits).contains("        case 1:\n"));
        let case = src.find("case 1").unwrap() + 5;
        assert!(introduce_constant(&f, rng(case, case + 1), false).is_err());
        let src = "def f():\n    return connect('db', 'db')\n\n\nURL = 'db'\n";
        let (_, f) = setup(src);
        assert_eq!(extract_occurrences(&f, sel(src, "'db'"), true).len(), 3);
        assert!(!is_trivial_literal(&f, sel(src, "'db'")));
    }

    #[test]
    fn move_target_depends_on_the_caret() {
        let src = "def outer():\n    def inner():\n        pass\n    return inner\n\n\nclass C:\n    def m(self):\n        pass\n\n\nX = 1\n";
        let (_, f) = setup(src);
        assert_eq!(
            move_target_at(&f, at(src, "pass")),
            Some(MoveTarget::NestedFunction("inner".into()))
        );
        assert_eq!(
            move_target_at(&f, at(src, "return inner")),
            Some(MoveTarget::TopLevel("outer".into()))
        );
        assert_eq!(
            move_target_at(&f, at(src, "self")),
            Some(MoveTarget::TopLevel("C".into()))
        );
        assert_eq!(move_target_at(&f, at(src, "X = 1")), None);
    }

    #[test]
    fn try_apply_edits_refuses_misfits() {
        assert_eq!(
            try_apply_edits("abc", &[(rng(1, 2), "X".into())]).unwrap(),
            "aXc"
        );
        assert!(try_apply_edits("abc", &[(rng(2, 9), "X".into())]).is_err());
        assert!(try_apply_edits("é", &[(rng(1, 2), "X".into())]).is_err());
        assert!(try_apply_edits(
            "abcdef",
            &[(rng(1, 4), "X".into()), (rng(2, 5), "Y".into())]
        )
        .is_err());
        assert_eq!(
            try_apply_edits("abc", &[(rng(1, 1), "X".into()), (rng(1, 2), "Y".into())]).unwrap(),
            "aXYc"
        );
    }

    #[test]
    fn move_nested_function_to_top_level() {
        let src = "def outer(a):\n    def helper(x):\n        return x * 2\n    return helper(a)\n\n\ndef other():\n    pass\n";
        let (db, f) = setup(src);
        let _ = db;
        let r = move_to_top_level(&f, at(src, "helper(x)")).unwrap();
        let out = apply_edits(src, &r.edits);
        assert_eq!(out, "def outer(a):\n    return helper(a)\n\n\ndef helper(x):\n    return x * 2\n\n\ndef other():\n    pass\n");
        assert_eq!(slice(&out, r.rename_ranges[0]), "helper");
        let src = "def outer(a):\n    def helper(x):\n        return x * a\n    return helper(1)\n";
        let (_, f) = setup(src);
        assert!(move_to_top_level(&f, at(src, "helper(x)"))
            .unwrap_err()
            .contains('a'));
    }

    #[test]
    fn move_nested_function_keeps_attached_comment_decorator_and_inline_trivia() {
        let src = "def outer():\n    # helper documentation π🙂\n    @decorate\n    def helper():\n        return 1  # result\n    return helper()\n";
        let (_, file) = setup(src);
        let result = move_to_top_level(&file, at(src, "helper():")).unwrap();
        let output = try_apply_edits(src, &result.edits).unwrap();
        assert_eq!(output, "def outer():\n    return helper()\n\n\n# helper documentation π🙂\n@decorate\ndef helper():\n    return 1  # result\n");
        assert_eq!(slice(&output, result.rename_ranges[0]), "helper");
        assert!(rustpy_syntax::parse(&output).ok());
    }

    #[test]
    fn move_nested_function_maps_real_name_after_comment_with_def() {
        let src="def outer():\n    # documentation def phantom π🙂\n    def helper():\n        return 1\n    return helper()\n";
        let (_, file) = setup(src);
        let result = move_to_top_level(&file, at(src, "helper():")).unwrap();
        let output = try_apply_edits(src, &result.edits).unwrap();
        assert_eq!(output,"def outer():\n    return helper()\n\n\n# documentation def phantom π🙂\ndef helper():\n    return 1\n");
        assert_eq!(slice(&output, result.rename_ranges[0]), "helper");
        assert!(rustpy_syntax::parse(&output).ok());
    }
    #[test]
    fn move_nested_function_preserves_body_comment_and_formatter_region_at_source() {
        let src="def outer():\n    # fmt: off\n    # documentation\n    def helper():\n        return 1\n        # trailing body π🙂\n    # fmt: on\n    return helper()\n";
        let (_, file) = setup(src);
        let result = move_to_top_level(&file, at(src, "helper():")).unwrap();
        let output = try_apply_edits(src, &result.edits).unwrap();
        assert_eq!(output,"def outer():\n    # fmt: off\n    # fmt: on\n    return helper()\n\n\n# documentation\ndef helper():\n    return 1\n    # trailing body π🙂\n");
        assert_eq!(slice(&output, result.rename_ranges[0]), "helper");
        assert!(rustpy_syntax::parse(&output).ok());
    }

    #[test]
    fn expression_candidates_lists_enclosing_expressions() {
        let src = "y = f(a.b + 1)\n";
        let (_, f) = setup(src);
        let c = expression_candidates(&f, at(src, "a.b"));
        let texts: Vec<&str> = c.iter().map(|r| slice(src, *r)).collect();
        assert_eq!(texts, vec!["a", "a.b", "a.b + 1", "f(a.b + 1)"]);
    }
}
