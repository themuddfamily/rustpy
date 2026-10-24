//! Probable bugs from the core "Python" inspections (roadmap §2.5
//! "References & names"): locals that might be read before assignment
//! (`PyUnboundLocalVariable`), names shadowing an outer scope
//! (`PyShadowingNames`), access to protected members (`PyProtectedMember`) and name-mangled
//! private members (`PyPrivateMember`),
//! instance attributes defined outside `__init__` (`PyAttributeOutsideInit`),
//! abstract classes that are subclassed without implementing everything or
//! instantiated (`PyAbstractClass`), and coroutines that are called but not
//! awaited (`PyAsyncCall`).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use rustpy_core::{Severity, TextRange, TextSize};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};
use smol_str::SmolStr;

use super::{indent_at, line_end_incl, QuickFix, Sink};
use rustpy_semantic::db::{Analysis, FileInfo};
use rustpy_semantic::infer::Inferer;
use rustpy_semantic::scope::{BindingKind, ScopeId, ScopeKind};
use rustpy_semantic::types::{ClassRef, Type};

/// Title of the fix that starts the Rename refactoring at the problem (the
/// IDE runs the action instead of applying edits).
pub const RENAME_ELEMENT_FIX: &str = "Rename element";

pub(super) fn run(inf: &Inferer<'_>, file: &Arc<FileInfo>, sink: &mut Sink) {
    let db = inf.db;
    let root = file.parse.syntax_node();
    unbound_locals(inf, file, &root, sink);
    shadowing_names(file, sink);
    protected_members(inf, file, &root, sink);
    private_members(inf, file, &root, sink);
    attributes_outside_init(inf, file, &root, sink);
    abstract_classes(db, inf, file, &root, sink);
    async_calls(inf, file, &root, sink);
}

// ---------------------------------------------------------------------------
// PyUnboundLocalVariable

/// Locals definitely assigned on every path reaching a point (indexed like
/// `Unbound::locals`); `None` when no path reaches it (after `return`,
/// `raise`, `break`, `continue` or a call that never returns).
type Flow = Option<Vec<bool>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum ExitReason {
    Break(TextSize),
    Continue(TextSize),
    Return,
    Raise,
    KnownRaise(TextSize),
}

fn is_raise(reason: ExitReason) -> bool {
    matches!(reason, ExitReason::Raise | ExitReason::KnownRaise(_))
}
fn exception_matches(raised: &str, caught: &str) -> bool {
    raised == caught
        || caught == "BaseException"
        || caught == "Exception"
            && !matches!(
                raised,
                "BaseException" | "SystemExit" | "KeyboardInterrupt" | "GeneratorExit"
            )
        || caught == "LookupError" && matches!(raised, "KeyError" | "IndexError")
}

fn merge_completion(
    channels: &mut HashMap<ExitReason, Vec<bool>>,
    reason: ExitReason,
    state: Vec<bool>,
) {
    if let Some(previous) = channels.get_mut(&reason) {
        for (previous, current) in previous.iter_mut().zip(state) {
            *previous &= current;
        }
    } else {
        channels.insert(reason, state);
    }
}

fn merge_completions(
    into: &mut HashMap<ExitReason, Vec<bool>>,
    from: HashMap<ExitReason, Vec<bool>>,
) {
    for (reason, state) in from {
        merge_completion(into, reason, state);
    }
}

fn meet(a: Flow, b: Flow) -> Flow {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some(a), Some(b)) => Some(a.into_iter().zip(b).map(|(x, y)| x && y).collect()),
    }
}

/// Functions known to never return, by definition (file, range).
type ExitCache = std::cell::RefCell<HashMap<(rustpy_core::FileId, TextRange), bool>>;
type MemberMutationCache =
    std::cell::RefCell<HashMap<rustpy_core::FileId, (Arc<FileInfo>, HashSet<SmolStr>)>>;

struct Unbound<'a, 'b> {
    inf: &'b Inferer<'a>,
    file: &'b Arc<FileInfo>,
    locals: HashMap<SmolStr, usize>,
    /// Lexical loop targets; finally may contain additional nested loops.
    loops: Vec<TextSize>,
    /// Finite completion channels, merged only by reason and exact loop target.
    abrupt: HashMap<ExitReason, Vec<bool>>,
    /// Off while a block is analysed a second time (`finally`).
    report: bool,
    reports: Vec<(TextRange, SmolStr)>,
    /// Nesting of "does this called function ever return?" analyses.
    depth: usize,
    /// Whether calls are inferred to find the ones that never return (beyond
    /// proven cheap terminal candidates); costly, so only when a cheap pass found problems.
    infer_exits: bool,
    exits: &'b ExitCache,
    member_cache: &'b MemberMutationCache,
    mutated_terminal_members: HashSet<SmolStr>,
    annotated_functions: HashSet<SmolStr>,
    comprehension_bound: Vec<HashSet<String>>,
    class_outer_locals: Vec<HashMap<SmolStr, usize>>,
}

/// Definite assignment of the locals of every function: a read of a local
/// that some path reaches without assigning it raises `UnboundLocalError`.
fn unbound_locals(inf: &Inferer<'_>, file: &Arc<FileInfo>, root: &SyntaxNode, sink: &mut Sink) {
    let mut seen: HashSet<TextRange> = HashSet::new();
    let exits = ExitCache::default();
    let member_cache = MemberMutationCache::default();
    let mutated_terminal_members = mutated_terminal_members(root);
    let annotated_functions = annotated_function_names(root);
    for f in root.descendants().filter_map(ast::FuncDef::cast) {
        let Some(scope) = file.scopes.scope_for_node(f.syntax().text_range()) else {
            continue;
        };
        let Some(body) = f.body() else { continue };
        let sc = file.scopes.scope(scope);
        // Names assigned from nested scopes (`nonlocal x`) may be set by a call.
        let assigned_elsewhere = nested_nonlocals(file, scope);
        let mut locals: HashMap<SmolStr, usize> = HashMap::new();
        let mut params: Vec<usize> = Vec::new();
        for (name, ids) in &sc.bindings {
            if sc.globals.contains(name)
                || sc.nonlocals.contains(name)
                || assigned_elsewhere.contains(name)
            {
                continue;
            }
            let bindings: Vec<_> = ids.iter().map(|id| file.scopes.binding(*id)).collect();
            if !bindings.iter().any(|b| b.is_definition()) {
                continue;
            }
            let idx = locals.len();
            locals.insert(name.clone(), idx);
            if bindings.iter().any(|b| {
                matches!(
                    b.kind,
                    BindingKind::Param | BindingKind::StarParam | BindingKind::StarStarParam
                )
            }) {
                params.push(idx);
            }
        }
        if locals.len() == params.len() {
            continue;
        }
        let mut state = vec![false; locals.len()];
        for p in params {
            state[p] = true;
        }
        // Treating a call as returning only adds paths (and reports): without
        // problems in a quick pass, following the calls cannot find any.
        let mut reports = Vec::new();
        for infer_exits in [false, true] {
            let mut a = Unbound {
                inf,
                file,
                locals: locals.clone(),
                loops: Vec::new(),
                abrupt: HashMap::new(),
                report: true,
                reports: Vec::new(),
                depth: 0,
                infer_exits,
                exits: &exits,
                member_cache: &member_cache,
                mutated_terminal_members: mutated_terminal_members.clone(),
                annotated_functions: annotated_functions.clone(),
                comprehension_bound: Vec::new(),
                class_outer_locals: Vec::new(),
            };
            // The function ends after its body: whether the last statement returns does not matter.
            a.block(Some(body.clone()), Some(state.clone()), false);
            reports = a.reports;
            if reports.is_empty() {
                break;
            }
        }
        for (range, name) in reports {
            if seen.insert(range) {
                sink.push(
                    range,
                    Severity::Warning,
                    "PyUnboundLocalVariable",
                    format!("Local variable '{name}' might be referenced before assignment"),
                    Vec::new(),
                );
            }
        }
    }
}

/// Names that scopes nested in `scope` declare `nonlocal`.
fn nested_nonlocals(file: &FileInfo, scope: ScopeId) -> HashSet<SmolStr> {
    let mut out = HashSet::new();
    let mut stack: Vec<ScopeId> = file.scopes.scope(scope).children.clone();
    while let Some(s) = stack.pop() {
        let sc = file.scopes.scope(s);
        // Class suites execute now; their explicit nonlocal stores/deletes are
        // projected during this walk. Deferred function writes stay conservative.
        if sc.kind != ScopeKind::Class {
            out.extend(sc.nonlocals.iter().cloned());
        }
        stack.extend(sc.children.iter().copied());
    }
    out
}

/// `Some(true)` / `Some(false)` for conditions that are constant (`while True:`, `if 0:`).
fn literal_integer(expression: &ast::Expr, depth: usize) -> Option<i128> {
    if depth > 4 {
        return None;
    }
    match expression.clone().unparen() {
        ast::Expr::Literal(literal) => {
            let token = literal.token()?;
            match token.text() {
                "True" => return Some(1),
                "False" => return Some(0),
                _ => {}
            }
            if token.kind() != SyntaxKind::INT_NUMBER || token.text().len() > 128 {
                return None;
            }
            let text = token.text().replace('_', "");
            let (radix, digits) = if let Some(digits) =
                text.strip_prefix("0x").or_else(|| text.strip_prefix("0X"))
            {
                (16, digits)
            } else if let Some(digits) = text.strip_prefix("0o").or_else(|| text.strip_prefix("0O"))
            {
                (8, digits)
            } else if let Some(digits) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B"))
            {
                (2, digits)
            } else {
                (10, text.as_str())
            };
            i128::from_str_radix(digits, radix).ok()
        }
        ast::Expr::Unary(unary) => {
            let value = literal_integer(&unary.operand()?, depth + 1)?;
            match unary.op()?.text() {
                "+" => Some(value),
                "-" => value.checked_neg(),
                _ => None,
            }
        }
        _ => None,
    }
}
// Classify only built-in numeric literals and their safe unary operations.
// No operator is invoked and no user operand is assumed non-raising.
fn literal_numeric_kind(expression: &ast::Expr, depth: usize) -> Option<bool> {
    if depth >= 64 {
        return None;
    }
    match expression.clone().unparen() {
        ast::Expr::Literal(literal) => {
            let token = literal.token()?;
            match token.kind() {
                SyntaxKind::INT_NUMBER => Some(true),
                SyntaxKind::FLOAT_NUMBER | SyntaxKind::COMPLEX_NUMBER => Some(false),
                _ if matches!(token.text(), "True" | "False") => Some(true),
                _ => None,
            }
        }
        ast::Expr::Unary(unary) => {
            let integral = literal_numeric_kind(&unary.operand()?, depth + 1)?;
            match unary.op()?.text() {
                "+" | "-" => Some(integral),
                "~" if integral => Some(true),
                _ => None,
            }
        }
        _ => None,
    }
}

fn constant_comparison(left: &ast::Expr, operator: &str, right: &ast::Expr) -> Option<bool> {
    let (left, right) = (literal_integer(left, 0)?, literal_integer(right, 0)?);
    Some(match operator {
        "==" => left == right,
        "!=" => left != right,
        "<" => left < right,
        "<=" => left <= right,
        ">" => left > right,
        ">=" => left >= right,
        _ => return None,
    })
}
fn decoded_string_truth(string: &ast::StringExpr) -> Option<bool> {
    if usize::from(string.syntax().text_range().len()) > 1024 {
        return None;
    }
    use rustpy_syntax::string_literal::{self, Decoded};
    match string_literal::string(&string.raw_text()).ok()? {
        Decoded::Text(text, _) => Some(!text.is_empty()),
        Decoded::Bytes(bytes) => Some(!bytes.is_empty()),
        Decoded::SurrogateText(points, _) => Some(!points.is_empty()),
    }
}
fn statically_empty(expression: &ast::Expr) -> bool {
    match expression.clone().unparen() {
        ast::Expr::Tuple(tuple) => tuple.elements().next().is_none(),
        ast::Expr::List(list) => list.elements().next().is_none(),
        ast::Expr::Set(set) => set.elements().next().is_none(),
        ast::Expr::Dict(dict) => dict.items().next().is_none() && dict.unpacks().next().is_none(),
        ast::Expr::String(string) => decoded_string_truth(&string) == Some(false),
        _ => false,
    }
}

fn static_truth(condition: Option<ast::Expr>) -> Option<bool> {
    static_truth_at(condition, 0)
}

fn static_truth_at(condition: Option<ast::Expr>, depth: usize) -> Option<bool> {
    if depth >= 64 {
        return None;
    }
    let condition = condition?.unparen();
    if let Some(integer) = literal_integer(&condition, 0) {
        return Some(integer != 0);
    }
    if statically_empty(&condition) {
        return Some(false);
    }
    match condition {
        ast::Expr::Literal(literal)
            if literal.token().is_some_and(|token| token.text() == "None") =>
        {
            Some(false)
        }
        ast::Expr::Not(not) => static_truth_at(not.operand(), depth + 1).map(|truth| !truth),
        ast::Expr::String(string) if usize::from(string.syntax().text_range().len()) <= 1024 => {
            decoded_string_truth(&string)
        }
        _ => None,
    }
}

/// A literal or a display of literals (`0`, `-1`, `'x'`, `None`, `()`, `[]`, `{}`):
/// evaluating it cannot raise.
fn is_constant(e: &SyntaxNode) -> bool {
    match e.kind() {
        SyntaxKind::LITERAL_EXPR | SyntaxKind::STRING_EXPR => true,
        SyntaxKind::CONCAT_STRING_EXPR => e.children().all(|c| c.kind() == SyntaxKind::STRING_EXPR),
        SyntaxKind::UNARY_EXPR => e.children().all(|c| {
            c.kind() == SyntaxKind::LITERAL_EXPR
                && c.first_token().is_some_and(|t| {
                    matches!(
                        t.kind(),
                        SyntaxKind::INT_NUMBER
                            | SyntaxKind::FLOAT_NUMBER
                            | SyntaxKind::COMPLEX_NUMBER
                    )
                })
        }),
        SyntaxKind::PAREN_EXPR | SyntaxKind::TUPLE_EXPR | SyntaxKind::LIST_EXPR => {
            e.children().all(|c| is_constant(&c))
        }
        SyntaxKind::DICT_EXPR | SyntaxKind::SET_EXPR => e.children().next().is_none(),
        _ => false,
    }
}

/// Whether evaluating the operation may raise before its stores complete.
/// Abrupt return/break/continue are separate channels, never caught exceptions.
fn may_raise(s: &ast::Stmt) -> bool {
    match s {
        ast::Stmt::Pass(_)
        | ast::Stmt::Global(_)
        | ast::Stmt::Nonlocal(_)
        | ast::Stmt::Break(_)
        | ast::Stmt::Continue(_)
        | ast::Stmt::Try(_) => false,
        ast::Stmt::Return(statement) => statement
            .value()
            .is_some_and(|value| !is_constant(value.syntax())),
        ast::Stmt::If(statement) => statement
            .condition()
            .is_some_and(|value| !is_constant(value.syntax())),
        ast::Stmt::While(statement) => statement
            .condition()
            .is_some_and(|value| !is_constant(value.syntax())),
        ast::Stmt::For(statement) => {
            !(matches!(statement.target(), Some(ast::Expr::Name(_)))
                && statement.iter().is_some_and(|iterator| {
                    matches!(
                        iterator.syntax().kind(),
                        SyntaxKind::TUPLE_EXPR | SyntaxKind::LIST_EXPR
                    ) && is_constant(iterator.syntax())
                }))
        }
        ast::Stmt::Assign(a) => {
            !(a.targets().iter().all(|t| matches!(t, ast::Expr::Name(_)))
                && a.value().is_some_and(|v| is_constant(v.syntax())))
        }
        // The annotation of a local variable is not evaluated.
        ast::Stmt::AnnAssign(a) => {
            !(matches!(a.target(), Some(ast::Expr::Name(_)))
                && a.value().is_none_or(|v| is_constant(v.syntax())))
        }
        _ => true,
    }
}

/// Cheap candidate names; spelling alone is never termination authority.
const NO_RETURN_NAMES: &[&str] = &[
    "exit",
    "quit",
    "_exit",
    "abort",
    "fail",
    "skip",
    "xfail",
    "skipTest",
    "assert_never",
];

/// Select only likely terminal calls for the inexpensive signature check.
fn is_terminal_candidate(e: &ast::Expr, annotated_functions: &HashSet<SmolStr>) -> bool {
    let ast::Expr::Call(call) = e.clone().unparen() else {
        return false;
    };
    let Some(callee) = call.callee() else {
        return false;
    };
    let last = match &callee.unparen() {
        ast::Expr::Name(n) => n.name_text(),
        ast::Expr::Attribute(a) => a.attr_text(),
        _ => return false,
    };
    if !NO_RETURN_NAMES.contains(&last.as_str()) && !annotated_functions.contains(last.as_str()) {
        return false;
    }
    true
}

/// Conservatively retire a stub-member fallback when any explicit store or
/// delete can replace that member. Semantic reaching-call proofs remain separate.
fn irrefutable_pattern(pattern: &SyntaxNode) -> bool {
    match pattern.kind() {
        SyntaxKind::MATCH_WILDCARD_PATTERN | SyntaxKind::MATCH_CAPTURE_PATTERN => true,
        SyntaxKind::MATCH_AS_PATTERN | SyntaxKind::MATCH_GROUP_PATTERN => pattern
            .children()
            .find(|child| !ast::Expr::can_cast(child.kind()))
            .is_some_and(|child| irrefutable_pattern(&child)),
        SyntaxKind::MATCH_OR_PATTERN => pattern.children().any(|child| irrefutable_pattern(&child)),
        _ => false,
    }
}

fn capture_only_pattern(pattern: &SyntaxNode) -> bool {
    match pattern.kind() {
        SyntaxKind::MATCH_WILDCARD_PATTERN | SyntaxKind::MATCH_CAPTURE_PATTERN => true,
        SyntaxKind::MATCH_AS_PATTERN | SyntaxKind::MATCH_GROUP_PATTERN => {
            pattern.children().next().is_some()
                && pattern.children().all(|child| capture_only_pattern(&child))
        }
        _ => false,
    }
}

fn annotated_function_names(root: &SyntaxNode) -> HashSet<SmolStr> {
    root.descendants()
        .filter_map(ast::FuncDef::cast)
        .filter(|function| function.return_annotation().is_some())
        .map(|function| SmolStr::new(function.name_text()))
        .collect()
}

fn mutated_terminal_members(root: &SyntaxNode) -> HashSet<SmolStr> {
    let mut members = HashSet::new();
    for statement in root.descendants().filter_map(ast::Stmt::cast) {
        let targets: Vec<ast::Expr> = match statement {
            ast::Stmt::Assign(a) => a.targets(),
            ast::Stmt::AnnAssign(a) if a.value().is_some() => a.target().into_iter().collect(),
            ast::Stmt::AugAssign(a) => a.target().into_iter().collect(),
            ast::Stmt::Del(a) => a.targets(),
            _ => continue,
        };
        for target in targets {
            for attribute in target
                .syntax()
                .descendants()
                .filter_map(ast::AttributeExpr::cast)
            {
                members.insert(attribute.attr_text().into());
            }
        }
    }
    members
}

impl Unbound<'_, '_> {
    fn inferred_terminal_member_retired(&self, callee: &ast::Expr) -> bool {
        matches!(callee.clone().unparen(), ast::Expr::Attribute(attribute)
            if self.mutated_terminal_members.contains(attribute.attr_text().as_str()))
    }

    // A reflected method signature cannot prove lookup authority when a class
    // decorator replaces the class or an instance lookup hook replaces members.
    fn terminal_receiver_is_static(&self, callee: &ast::Expr) -> bool {
        let ast::Expr::Attribute(attribute) = callee.clone().unparen() else {
            return true;
        };
        let Some(object) = attribute.object() else {
            return false;
        };
        let receiver = self.inf.type_of_expr(self.file, &object);
        let Some(class) = receiver.class_ref() else {
            // Module functions retain their separate canonical import proof.
            return !matches!(
                self.inf.type_of_expr(self.file, callee),
                Type::BoundMethod(_, _)
            );
        };
        let instance = !matches!(receiver, Type::Class(_, _));
        let intrinsic_object = self.inf.builtin_class("object").filter(|object| {
            self.inf.db.file(object.file).is_some_and(|file| {
                file.is_stub && file.path == Path::new("<typeshed>/builtins.pyi")
            })
        });
        self.inf.mro(class).iter().all(|base| {
            // Bundled object has a typing-only @disjoint_base decorator. Its
            // intrinsic lookup/construction is not a user class transformation.
            if intrinsic_object.as_ref() == Some(base) {
                return true;
            }
            self.inf.class_info(base).is_some_and(|info| {
                info.decorators.is_empty()
                    && !info.keywords.iter().any(|(name, _)| name == "metaclass")
                    && (!instance
                        || (!info.members.contains_key("__getattribute__")
                            && !info.members.contains_key("__getattr__")
                            && !info.members.contains_key("__new__")))
            })
        })
    }

    fn containing_classes_are_static(function: &ast::FuncDef) -> bool {
        function
            .syntax()
            .ancestors()
            .filter_map(ast::ClassDef::cast)
            .all(|class| {
                class.decorators().next().is_none()
                    && class.arguments().is_none_or(|arguments| {
                        !arguments.keywords().any(|keyword| {
                            keyword
                                .name()
                                .is_some_and(|name| name.text() == "metaclass")
                        })
                    })
            })
    }

    fn decorator_terminates(&self, decorator: &ast::Expr) -> bool {
        if self.inferred_terminal_member_retired(decorator)
            || !self.terminal_receiver_is_static(decorator)
        {
            return false;
        }
        // The implicit invocation receives the newly created definition. Only
        // an actual undecorated synchronous canonical Never declaration proves
        // that invocation terminal; evaluating the decorator expression is separate.
        match self.inf.type_of_expr(self.file, decorator) {
            Type::Function(signature) | Type::BoundMethod(signature, _) => {
                !signature.is_async
                    && matches!(signature.ret, Type::Never)
                    && self.canonical_never_signature(&signature)
            }
            _ => false,
        }
    }

    fn annotations_deferred(&self) -> bool {
        self.file
            .parse
            .syntax_node()
            .children()
            .filter_map(ast::ImportFromStmt::cast)
            .any(|import| {
                import.module_path() == "__future__"
                    && import.aliases().any(|alias| alias.name() == "annotations")
            })
    }

    fn is_no_return_call(&self, expression: &ast::Expr) -> bool {
        let expression = expression.clone().unparen();
        if !matches!(expression, ast::Expr::Call(_) | ast::Expr::Await(_)) {
            return false;
        }
        // This proof is produced at the exact statement using semantic reaching
        // bindings and captured canonical import authority, not source spelling.
        if self.inf.call_terminates(self.file, &expression) {
            return true;
        }
        // Scope flow is cached by the Inferer; only candidate signatures require
        // inference. A genuine named NoReturn call need not be the last statement.
        if !is_terminal_candidate(&expression, &self.annotated_functions) {
            return false;
        }
        let ast::Expr::Call(call) = expression else {
            return false;
        };
        let Some(callee) = call.callee() else {
            return false;
        };
        if self.inferred_terminal_member_retired(&callee)
            || !self.terminal_receiver_is_static(&callee)
        {
            return false;
        }
        // Live canonical sys/os stubs and genuine typed methods remain useful.
        // Do not recursively inspect function bodies in this cheap candidate path.
        match self.inf.type_of_expr(self.file, &callee) {
            Type::Function(signature) | Type::BoundMethod(signature, _) => {
                !signature.is_async
                    && matches!(signature.ret, Type::Never)
                    && self.canonical_never_signature(&signature)
            }
            _ => false,
        }
    }

    fn canonical_never_definition(&self, definition: rustpy_semantic::infer::Definition) -> bool {
        let definition = self.inf.follow_imports(definition);
        let Some(module) = definition.file.module_name.as_deref() else {
            return false;
        };
        matches!(module, "typing" | "typing_extensions")
            && matches!(definition.binding().name.as_str(), "Never" | "NoReturn")
            && definition.file.is_stub
            && definition.file.path == Path::new(&format!("<typeshed>/{module}.pyi"))
            && self
                .inf
                .db
                .module(module)
                .is_some_and(|current| Arc::ptr_eq(&current, &definition.file))
    }

    fn file_member_mutations(&self, file: &Arc<FileInfo>) -> HashSet<SmolStr> {
        let mut cache = self.member_cache.borrow_mut();
        let entry = cache.entry(file.id).or_insert_with(|| {
            (
                file.clone(),
                mutated_terminal_members(&file.parse.syntax_node()),
            )
        });
        if !Arc::ptr_eq(&entry.0, file) {
            *entry = (
                file.clone(),
                mutated_terminal_members(&file.parse.syntax_node()),
            );
        }
        entry.1.clone()
    }

    fn canonical_never_signature(&self, signature: &rustpy_semantic::types::Signature) -> bool {
        let Some(definition) = &signature.def else {
            return false;
        };
        let Some(file) = self.inf.db.file(definition.file) else {
            return false;
        };
        let Some(function) = func_def_with_range(&file.parse.syntax_node(), definition.range)
        else {
            return false;
        };
        if function.is_async()
            || function.decorators().next().is_some()
            || !Self::containing_classes_are_static(&function)
        {
            return false;
        }
        let Some(annotation) = function.return_annotation().map(ast::Expr::unparen) else {
            return false;
        };
        // Annotation member stores can invalidate even an unchanged bundled
        // declaration. Inspect the defining file, including imported functions.
        let annotation_members = self.file_member_mutations(&file);
        if annotation_members.contains("Never") || annotation_members.contains("NoReturn") {
            return false;
        }
        match annotation {
            ast::Expr::Name(name) => {
                let scope = file
                    .scopes
                    .reference_at(name.syntax().text_range().start())
                    .map(|reference| reference.scope)
                    .unwrap_or_else(|| file.scopes.scope_at(name.syntax().text_range().start()));
                file.scopes
                    .resolve(scope, &name.name_text())
                    .is_some_and(|bindings| {
                        !bindings.is_empty()
                            && bindings.iter().all(|binding| {
                                self.canonical_never_definition(
                                    rustpy_semantic::infer::Definition {
                                        file: file.clone(),
                                        binding: *binding,
                                    },
                                )
                            })
                    })
            }
            ast::Expr::Attribute(attribute) => {
                let Some(ast::Expr::Name(name)) = attribute.object().map(ast::Expr::unparen) else {
                    return false;
                };
                let scope = file
                    .scopes
                    .reference_at(name.syntax().text_range().start())
                    .map(|reference| reference.scope)
                    .unwrap_or_else(|| file.scopes.scope_at(name.syntax().text_range().start()));
                if !file
                    .scopes
                    .resolve(scope, &name.name_text())
                    .is_some_and(|bindings| {
                        !bindings.is_empty()
                            && bindings.iter().all(|binding| {
                                matches!(file.scopes.binding(*binding).kind, BindingKind::Import(_))
                            })
                    })
                {
                    return false;
                }
                let Type::Module(module) = self.inf.type_of_expr(&file, &ast::Expr::Name(name))
                else {
                    return false;
                };
                self.inf
                    .db
                    .file(module)
                    .and_then(|module| {
                        self.inf
                            .resolve_in_module(&module, &attribute.attr_text(), 0)
                    })
                    .is_some_and(|definition| self.canonical_never_definition(definition))
            }
            _ => false,
        }
    }

    fn operation_may_raise(&self, statement: &ast::Stmt, _state: &[bool]) -> bool {
        // Expression evaluators own their actual operation points. Do not add
        // a fictitious pre-RHS exception before earlier arguments/walrus stores.
        !matches!(
            statement,
            ast::Stmt::Expr(_)
                | ast::Stmt::Assign(_)
                | ast::Stmt::AugAssign(_)
                | ast::Stmt::AnnAssign(_)
                | ast::Stmt::Return(_)
                | ast::Stmt::Raise(_)
                | ast::Stmt::Assert(_)
                | ast::Stmt::If(_)
                | ast::Stmt::While(_)
                | ast::Stmt::For(_)
                | ast::Stmt::With(_)
                | ast::Stmt::Match(_)
                | ast::Stmt::FuncDef(_)
                | ast::Stmt::ClassDef(_)
        ) && may_raise(statement)
    }

    fn assign(&self, name: &str, st: &mut [bool]) {
        if let Some(i) = self.locals.get(name) {
            st[*i] = true;
        }
    }

    fn unassign(&self, name: &str, st: &mut [bool]) {
        if let Some(i) = self.locals.get(name) {
            st[*i] = false;
        }
    }

    fn read(&mut self, tok: &SyntaxToken, st: &mut [bool]) {
        if let Some(i) = self.locals.get(tok.text()) {
            if !st[*i] {
                if self.report {
                    self.reports
                        .push((tok.text_range(), SmolStr::new(tok.text())));
                }
                // Only the first read on a path can fail: past it the name is bound.
                st[*i] = true;
            }
        }
    }

    /// The statements of `block`. `tail` says whether the state after the
    /// block matters (not after a function's own body); only then is a final
    /// call examined for never returning.
    fn block(&mut self, block: Option<ast::Block>, mut st: Flow, tail: bool) -> Flow {
        let Some(b) = block else { return st };
        let mut stmts = b.statements().peekable();
        while let Some(s) = stmts.next() {
            st = self.stmt(&s, st);
            // Statements after `return`/`raise`/… are unreachable: nothing to check.
            let cur = st.as_ref()?;
            let ast::Stmt::Expr(e) = &s else { continue };
            let Some(x) = e.expr() else { continue };
            // A call that never returns ends the block. Inferring the callee is
            // costly, so only a last statement is examined, and only when that
            // can change the outcome: some local is still unassigned, or this
            // is the "does it ever return?" analysis of a called function.
            let last = stmts.peek().is_none();
            if self.is_no_return_call(&x)
                || (self.infer_exits
                    && last
                    && tail
                    && (self.depth > 0 || cur.contains(&false))
                    && self.never_returns(&x))
            {
                merge_completion(&mut self.abrupt, ExitReason::Raise, cur.clone());
                return None;
            }
        }
        st
    }

    fn stmt(&mut self, s: &ast::Stmt, st: Flow) -> Flow {
        let mut cur = st?;
        // Preserve the state BEFORE the operation can fail. Compound try owns
        // its own predecessors, rather than fabricating an exception at entry.
        if !matches!(s, ast::Stmt::Raise(raise) if self.inf.raised_exception_name_at(self.file, raise.syntax().text_range().start()).is_some())
            && self.operation_may_raise(s, &cur)
        {
            merge_completion(&mut self.abrupt, ExitReason::Raise, cur.clone());
        }
        match s {
            ast::Stmt::Expr(e) => {
                if let Some(x) = e.expr() {
                    if !self.expr(x.syntax(), &mut cur) {
                        return None;
                    }
                }
            }
            ast::Stmt::Assign(a) => {
                if let Some(v) = a.value() {
                    if !self.expr(v.syntax(), &mut cur) {
                        return None;
                    }
                }
                for t in a.targets() {
                    if !self.target(&t, &mut cur) {
                        return None;
                    }
                }
            }
            ast::Stmt::AugAssign(a) => {
                if let Some(t) = a.target() {
                    if !self.expr(t.syntax(), &mut cur) {
                        return None;
                    }
                }
                if let Some(v) = a.value() {
                    if !self.expr(v.syntax(), &mut cur) {
                        return None;
                    }
                }
                if let Some(target) = a.target() {
                    if !self.operation_completes(
                        &target,
                        rustpy_semantic::flow::OperationKind::Inplace,
                        &mut cur,
                    ) {
                        return None;
                    }
                }
                if let Some(ast::Expr::Name(n)) = a.target() {
                    self.assign(&n.name_text(), &mut cur);
                }
            }
            ast::Stmt::AnnAssign(assignment) => {
                // Local annotations do not evaluate; without a RHS a name,
                // including a parenthesized one, is not loaded or stored.
                if let Some(value) = assignment.value() {
                    if !self.expr(value.syntax(), &mut cur) {
                        return None;
                    }
                    if let Some(target) = assignment.target() {
                        if !self.target(&target, &mut cur) {
                            return None;
                        }
                    }
                } else if let Some(target) = assignment.target() {
                    if !self.annotation_target(&target, &mut cur) {
                        return None;
                    }
                }
            }
            ast::Stmt::Return(r) => {
                if let Some(v) = r.value() {
                    if !self.expr(v.syntax(), &mut cur) {
                        return None;
                    }
                }
                merge_completion(&mut self.abrupt, ExitReason::Return, cur);
                return None;
            }
            ast::Stmt::Raise(r) => {
                for e in [r.exception(), r.cause()].into_iter().flatten() {
                    if !self.expr(e.syntax(), &mut cur) {
                        return None;
                    }
                }
                let reason = if self
                    .inf
                    .raised_exception_name_at(self.file, r.syntax().text_range().start())
                    .is_some()
                {
                    ExitReason::KnownRaise(r.syntax().text_range().start())
                } else {
                    ExitReason::Raise
                };
                merge_completion(&mut self.abrupt, reason, cur);
                return None;
            }
            ast::Stmt::Break(_) => {
                if let Some(target) = self.loops.last().copied() {
                    merge_completion(&mut self.abrupt, ExitReason::Break(target), cur);
                }
                return None;
            }
            ast::Stmt::Continue(_) => {
                if let Some(target) = self.loops.last().copied() {
                    merge_completion(&mut self.abrupt, ExitReason::Continue(target), cur);
                }
                return None;
            }
            ast::Stmt::Pass(_) | ast::Stmt::Global(_) | ast::Stmt::Nonlocal(_) => {}
            ast::Stmt::Del(d) => {
                for t in d.targets() {
                    match &t {
                        ast::Expr::Name(n) => self.unassign(&n.name_text(), &mut cur),
                        other => {
                            if !self.target_operation(
                                other,
                                rustpy_semantic::flow::OperationKind::Delete,
                                &mut cur,
                            ) {
                                return None;
                            }
                        }
                    }
                }
            }
            ast::Stmt::Assert(assertion) => {
                if let Some(test) = assertion.test() {
                    if !self.truth_expr(&test, &mut cur) {
                        return None;
                    }
                }
                let truth = static_truth(assertion.test());
                if truth != Some(true) {
                    let mut failure = cur.clone();
                    if truth.is_none() {
                        merge_completion(&mut self.abrupt, ExitReason::Raise, failure.clone());
                    }
                    if assertion
                        .message()
                        .is_none_or(|message| self.expr(message.syntax(), &mut failure))
                    {
                        merge_completion(&mut self.abrupt, ExitReason::Raise, failure);
                    }
                }
                if truth == Some(false) {
                    return None;
                }
            }
            ast::Stmt::Import(i) => {
                for alias in i.aliases() {
                    if let Some(bound) = alias.bound_name() {
                        self.assign(&bound, &mut cur);
                    }
                }
            }
            ast::Stmt::ImportFrom(f) => {
                for alias in f.aliases() {
                    if let Some(bound) = alias.bound_name() {
                        self.assign(&bound, &mut cur);
                    }
                }
            }
            ast::Stmt::If(i) => return self.if_stmt(i, cur),
            ast::Stmt::While(w) => return self.while_stmt(w, cur),
            ast::Stmt::For(f) => return self.for_stmt(f, cur),
            ast::Stmt::Try(t) => return self.try_stmt(t, cur),
            ast::Stmt::With(w) => return self.with_stmt(w, cur),
            ast::Stmt::FuncDef(f) => {
                let decorators: Vec<_> = f
                    .decorators()
                    .filter_map(|decorator| decorator.expr())
                    .collect();
                // Capture decorators before defaults/annotations can rebind their names.
                let terminal_decorators: Vec<_> = decorators
                    .iter()
                    .map(|decorator| self.decorator_terminates(decorator))
                    .collect();
                for decorator in &decorators {
                    if !self.expr(decorator.syntax(), &mut cur) {
                        return None;
                    }
                }
                let parameters: Vec<_> = f
                    .params()
                    .into_iter()
                    .flat_map(|parameters| parameters.params().collect::<Vec<_>>())
                    .collect();
                for parameter in &parameters {
                    if let Some(default) = parameter.default() {
                        if !self.expr(default.syntax(), &mut cur) {
                            return None;
                        }
                    }
                }
                if !self.annotations_deferred() {
                    for parameter in &parameters {
                        if let Some(annotation) = parameter.annotation() {
                            if !self.expr(annotation.syntax(), &mut cur) {
                                return None;
                            }
                        }
                    }
                    if let Some(annotation) = f.return_annotation() {
                        if !self.expr(annotation.syntax(), &mut cur) {
                            return None;
                        }
                    }
                }
                // PEP 695 bounds/defaults remain lazy; do not descend type_params.
                for terminal in terminal_decorators.into_iter().rev() {
                    merge_completion(&mut self.abrupt, ExitReason::Raise, cur.clone());
                    if terminal {
                        return None;
                    }
                }
                self.assign(&f.name_text(), &mut cur);
            }
            ast::Stmt::ClassDef(c) => {
                let decorators: Vec<_> = c
                    .decorators()
                    .filter_map(|decorator| decorator.expr())
                    .collect();
                let terminal_decorators: Vec<_> = decorators
                    .iter()
                    .map(|decorator| self.decorator_terminates(decorator))
                    .collect();
                for decorator in &decorators {
                    if !self.expr(decorator.syntax(), &mut cur) {
                        return None;
                    }
                }
                if let Some(arguments) = c.arguments() {
                    if !self.expr(arguments.syntax(), &mut cur) {
                        return None;
                    }
                }
                // Class namespace stores do not STORE_FAST into the enclosing
                // function. Keep stable state indices and temporarily restrict
                // its name map to cells the class actually addresses externally.
                let saved_locals = self.locals.clone();
                // Nested class suites skip every enclosing class namespace.
                // Start from the original function-cell map, not its outer
                // class-filtered view, then apply THIS class's name roles.
                if let Some(lexical_locals) = self.class_outer_locals.first() {
                    self.locals = lexical_locals.clone();
                }
                if let Some(scope) = self.file.scopes.scope_for_node(c.text_range()) {
                    let class_scope = self.file.scopes.scope(scope);
                    self.locals.retain(|name, _| {
                        !class_scope.globals.contains(name)
                            && (!class_scope.bindings.contains_key(name)
                                || class_scope.nonlocals.contains(name))
                    });
                    for name in &class_scope.nonlocals {
                        if let Some(index) = self
                            .class_outer_locals
                            .iter()
                            .find_map(|outer| outer.get(name))
                            .copied()
                        {
                            self.locals.insert(name.clone(), index);
                        }
                    }
                }
                self.class_outer_locals.push(saved_locals.clone());
                let normal = self.block(c.body(), Some(cur), false);
                self.class_outer_locals.pop();
                self.locals = saved_locals;
                cur = normal?;
                for terminal in terminal_decorators.into_iter().rev() {
                    merge_completion(&mut self.abrupt, ExitReason::Raise, cur.clone());
                    if terminal {
                        return None;
                    }
                }
                self.assign(&c.name_text(), &mut cur);
            }
            ast::Stmt::TypeAlias(t) => {
                if let Some(n) = t.name() {
                    self.assign(n.text(), &mut cur);
                }
            }
            ast::Stmt::Match(m) => return self.match_stmt(m, cur),
        }
        Some(cur)
    }

    fn if_stmt(&mut self, i: &ast::IfStmt, mut cur: Vec<bool>) -> Flow {
        if let Some(c) = i.condition() {
            if !self.truth_expr(&c, &mut cur) {
                return None;
            }
        }
        let truth = static_truth(i.condition());
        if truth.is_none() {
            merge_completion(&mut self.abrupt, ExitReason::Raise, cur.clone());
        }
        let mut out: Flow = if truth == Some(false) {
            None
        } else {
            self.block(i.body(), Some(cur.clone()), true)
        };
        if truth == Some(true) {
            return out;
        }
        for e in i.elif_clauses() {
            if let Some(c) = e.condition() {
                if !self.truth_expr(&c, &mut cur) {
                    return out;
                }
            }
            let truth = static_truth(e.condition());
            if truth.is_none() {
                merge_completion(&mut self.abrupt, ExitReason::Raise, cur.clone());
            }
            if truth != Some(false) {
                out = meet(out, self.block(e.body(), Some(cur.clone()), true));
            }
            if truth == Some(true) {
                return out;
            }
        }
        let rest = match i.else_clause() {
            Some(e) => self.block(e.body(), Some(cur), true),
            None => Some(cur),
        };
        meet(out, rest)
    }

    fn while_stmt(&mut self, w: &ast::WhileStmt, mut cur: Vec<bool>) -> Flow {
        if let Some(c) = w.condition() {
            if !self.truth_expr(&c, &mut cur) {
                return None;
            }
        }
        let truth = static_truth(w.condition());
        if truth.is_none() {
            merge_completion(&mut self.abrupt, ExitReason::Raise, cur.clone());
        }
        if truth == Some(false) {
            return match w.else_clause() {
                Some(clause) => self.block(clause.body(), Some(cur), true),
                None => Some(cur),
            };
        }
        let target = w.syntax().text_range().start();
        self.loops.push(target);
        self.block(w.body(), Some(cur.clone()), true);
        self.loops.pop();
        let breaks = self.abrupt.remove(&ExitReason::Break(target));
        self.abrupt.remove(&ExitReason::Continue(target));
        // Preserve the existing bounded loop precision: unknown conditions may
        // fail before the first iteration, while literal True has no normal exit.
        let normal = if truth == Some(true) {
            None
        } else {
            match w.else_clause() {
                Some(clause) => self.block(clause.body(), Some(cur), true),
                None => Some(cur),
            }
        };
        meet(normal, breaks)
    }

    fn with_stmt(&mut self, statement: &ast::WithStmt, state: Vec<bool>) -> Flow {
        use rustpy_semantic::flow::{ContextContract, ContextSuppression};
        let outside = std::mem::take(&mut self.abrupt);
        let mut entered: Vec<ContextContract> = Vec::new();
        let mut paths: Vec<(Option<ExitReason>, Vec<bool>, Vec<ContextContract>)> = Vec::new();
        let mut normal = Some(state);
        for item in statement.items() {
            let Some(mut current) = normal.take() else {
                break;
            };
            let Some(context) = item.context_expr() else {
                normal = Some(current);
                continue;
            };
            let evaluation_prefix = entered.clone();
            let evaluated = self.expr(context.syntax(), &mut current);
            for (reason, failed) in std::mem::take(&mut self.abrupt) {
                paths.push((Some(reason), failed, evaluation_prefix.clone()));
            }
            if !evaluated {
                break;
            }
            let contract = self.inf.context_contract(self.file, &context);
            paths.push((Some(ExitReason::Raise), current.clone(), entered.clone()));
            if contract.entry_terminal {
                break;
            }
            entered.push(contract);
            let target_completed = item
                .target()
                .is_none_or(|target| self.target(&target, &mut current));
            for (reason, failed) in std::mem::take(&mut self.abrupt) {
                paths.push((Some(reason), failed, entered.clone()));
            }
            if !target_completed {
                break;
            }
            normal = Some(current);
        }
        if let Some(current) = normal {
            if let Some(end) = self.block(statement.body(), Some(current), true) {
                paths.push((None, end, entered.clone()));
            }
            for (reason, failed) in std::mem::take(&mut self.abrupt) {
                paths.push((Some(reason), failed, entered.clone()));
            }
        }
        let mut normal = None;
        let mut outputs = HashMap::new();
        for (reason, state, contexts) in paths {
            let mut reasons = vec![reason];
            for contract in contexts.iter().rev() {
                let mut next = Vec::new();
                for reason in reasons {
                    if contract.exit_terminal {
                        next.push(Some(ExitReason::Raise));
                        continue;
                    }
                    if contract.exit_may_raise {
                        next.push(Some(ExitReason::Raise));
                    }
                    if reason.is_some_and(is_raise) {
                        let suppress = match &contract.suppression {
                            ContextSuppression::False => Some(false),
                            ContextSuppression::True => Some(true),
                            ContextSuppression::Unknown => None,
                            ContextSuppression::Exceptions(names) => match reason {
                                Some(ExitReason::KnownRaise(point)) => self
                                    .inf
                                    .raised_exception_name_at(self.file, point)
                                    .map(|raised| {
                                        names
                                            .iter()
                                            .any(|caught| exception_matches(&raised, caught))
                                    }),
                                _ => None,
                            },
                        };
                        if suppress != Some(false) {
                            next.push(None);
                        }
                        if suppress != Some(true) {
                            next.push(reason);
                        }
                    } else {
                        next.push(reason);
                    }
                }
                let mut unique = Vec::new();
                for reason in next {
                    if !unique.contains(&reason) {
                        unique.push(reason);
                    }
                }
                reasons = unique;
            }
            for reason in reasons {
                match reason {
                    Some(reason) => merge_completion(&mut outputs, reason, state.clone()),
                    None => normal = meet(normal, Some(state.clone())),
                }
            }
        }
        self.abrupt = outside;
        merge_completions(&mut self.abrupt, outputs);
        normal
    }

    fn for_stmt(&mut self, f: &ast::ForStmt, mut cur: Vec<bool>) -> Flow {
        if let Some(iterator) = f.iter() {
            if !self.expr(iterator.syntax(), &mut cur) {
                return None;
            }
            if !self.operation_completes(
                &iterator,
                rustpy_semantic::flow::OperationKind::IteratorAcquisition,
                &mut cur,
            ) {
                return None;
            }
            if statically_empty(&iterator) {
                return match f.else_clause() {
                    Some(clause) => self.block(clause.body(), Some(cur), true),
                    None => Some(cur),
                };
            }
            if !matches!(iterator.unparen(), ast::Expr::Tuple(_) | ast::Expr::List(_)) {
                merge_completion(&mut self.abrupt, ExitReason::Raise, cur.clone());
            }
        }
        let mut body_in = cur.clone();
        let target_normal = f
            .target()
            .is_none_or(|target| self.target(&target, &mut body_in));
        let target = f.syntax().text_range().start();
        self.loops.push(target);
        self.block(f.body(), target_normal.then_some(body_in), true);
        self.loops.pop();
        let breaks = self.abrupt.remove(&ExitReason::Break(target));
        self.abrupt.remove(&ExitReason::Continue(target));
        // An empty iterable retains the existing conservative zero-iteration path.
        let normal = match f.else_clause() {
            Some(clause) => self.block(clause.body(), Some(cur), true),
            None => Some(cur),
        };
        meet(normal, breaks)
    }

    fn try_stmt(&mut self, t: &ast::TryStmt, cur: Vec<bool>) -> Flow {
        // Isolate this try's actual exits. Outer pending branches retain ownership.
        let outer = std::mem::take(&mut self.abrupt);
        let body_normal = self.block(t.body(), Some(cur), true);
        let mut body_exits = std::mem::take(&mut self.abrupt);
        let raise_keys: Vec<_> = body_exits
            .keys()
            .copied()
            .filter(|reason| is_raise(*reason))
            .collect();
        let mut exception_in = None;
        for key in raise_keys {
            exception_in = meet(exception_in, body_exits.remove(&key));
        }
        let mut normal = match t.else_clause() {
            Some(clause) => self.block(clause.body(), body_normal, true),
            None => body_normal,
        };
        let mut exits = std::mem::take(&mut self.abrupt);
        merge_completions(&mut exits, body_exits);
        let mut catches_all = false;
        for handler in t.except_clauses() {
            let Some(mut input) = exception_in.clone() else {
                break;
            };
            if let Some(exception_type) = handler.exception_type() {
                if !self.expr(exception_type.syntax(), &mut input) {
                    merge_completions(&mut exits, std::mem::take(&mut self.abrupt));
                    catches_all = true; // dispatch itself abruptly consumed remaining exceptions
                    break;
                }
                merge_completion(&mut exits, ExitReason::Raise, input.clone());
            } else {
                catches_all = true;
            }
            if let Some(name) = handler.name() {
                self.assign(name.text(), &mut input);
            }
            let mut output = self.block(handler.body(), Some(input), true);
            let mut handler_exits = std::mem::take(&mut self.abrupt);
            // Exception targets are deleted on every kind of handler completion.
            if let Some(name) = handler.name() {
                if let Some(state) = &mut output {
                    self.unassign(name.text(), state);
                }
                for state in handler_exits.values_mut() {
                    self.unassign(name.text(), state);
                }
            }
            normal = meet(normal, output);
            merge_completions(&mut exits, handler_exits);
            if catches_all {
                break;
            }
        }
        // Unknown typed matching never promises that every exception is caught.
        if !catches_all {
            if let Some(input) = exception_in {
                merge_completion(&mut exits, ExitReason::Raise, input);
            }
        }
        let Some(finally) = t.finally_clause() else {
            self.abrupt = outer;
            merge_completions(&mut self.abrupt, exits);
            return normal;
        };
        let mut entries: Vec<_> = exits
            .into_iter()
            .map(|(reason, state)| (Some(reason), state))
            .collect();
        if let Some(state) = normal {
            entries.push((None, state));
        }
        let all_inputs = entries
            .iter()
            .fold(None, |merged, (_, state)| meet(merged, Some(state.clone())));
        let original_report = std::mem::replace(&mut self.report, false);
        let mut normal_out = None;
        let mut transformed = HashMap::new();
        for (reason, state) in entries {
            self.abrupt.clear();
            let output = self.block(finally.body(), Some(state), true);
            merge_completions(&mut transformed, std::mem::take(&mut self.abrupt));
            if let Some(output) = output {
                if let Some(reason) = reason {
                    merge_completion(&mut transformed, reason, output);
                } else {
                    normal_out = meet(normal_out, Some(output));
                }
            }
        }
        self.report = original_report;
        // One diagnostic walk observes all actual entry states, without emitting
        // duplicate reports or leaking its artificial completion channels.
        self.abrupt.clear();
        if original_report {
            self.block(finally.body(), all_inputs, true);
        }
        self.abrupt = outer;
        merge_completions(&mut self.abrupt, transformed);
        normal_out
    }

    fn match_stmt(&mut self, m: &ast::MatchStmt, mut cur: Vec<bool>) -> Flow {
        if let Some(s) = m.subject() {
            if !self.expr(s.syntax(), &mut cur) {
                return None;
            }
        }
        let canonical = |module: &str| {
            self.inf.db.module(module).is_some_and(|owner| {
                owner.is_stub
                    && owner.module_name.as_deref() == Some(module)
                    && owner.path == std::path::Path::new(&format!("<typeshed>/{module}.pyi"))
            })
        };
        let scalar =
            rustpy_semantic::match_support::finite_integer_coverage(self.file, m, canonical);
        let structural =
            rustpy_semantic::match_support::structural_coverage(self.file, m, canonical);
        let enums =
            rustpy_semantic::match_support::finite_enum_coverage(self.file, m, |from, id| {
                let rustpy_semantic::scope::BindingKind::Import(import) =
                    &from.scopes.binding(id).kind
                else {
                    return None;
                };
                self.inf.db.module_relative(
                    from,
                    import.level,
                    import.module.trim_start_matches('.'),
                )
            });
        let coverage = rustpy_semantic::match_support::merge_coverage(
            rustpy_semantic::match_support::merge_coverage(scalar, enums),
            structural,
        );
        let mut out: Flow = None;
        let mut exhaustive = false;
        for (case_index, case) in m.cases().enumerate() {
            let mut cin = cur.clone();
            let mut pattern_irrefutable = false;
            if let Some(p) = case.pattern() {
                if !capture_only_pattern(&p) {
                    merge_completion(&mut self.abrupt, ExitReason::Raise, cin.clone());
                }
                self.pattern(&p, &mut cin);
                pattern_irrefutable = irrefutable_pattern(&p);
                exhaustive = (pattern_irrefutable
                    || coverage
                        .as_ref()
                        .is_some_and(|coverage| coverage.get(case_index) == Some(&true)))
                    && (case.guard().is_none() || static_truth(case.guard()) == Some(true));
            }
            if let Some(g) = case.guard() {
                if !self.truth_expr(&g, &mut cin) {
                    if pattern_irrefutable {
                        return out;
                    }
                    continue;
                }
                if static_truth(Some(g)).is_none() {
                    merge_completion(&mut self.abrupt, ExitReason::Raise, cin.clone());
                }
            }
            if case.guard().is_some() {
                // A successful irrefutable pattern binds its captures even
                // when the guard is false. Refutable failure retains the old path.
                cur = if pattern_irrefutable {
                    cin.clone()
                } else {
                    meet(Some(cur), Some(cin.clone())).unwrap()
                };
            }
            if static_truth(case.guard()) != Some(false) {
                out = meet(out, self.block(case.body(), Some(cin), true));
            }
            if exhaustive {
                break;
            }
        }
        if !exhaustive {
            out = meet(out, Some(cur));
        }
        out
    }

    fn pattern(&mut self, p: &SyntaxNode, st: &mut Vec<bool>) {
        match p.kind() {
            SyntaxKind::MATCH_CAPTURE_PATTERN
            | SyntaxKind::MATCH_STAR_PATTERN
            | SyntaxKind::MATCH_MAPPING_REST => {
                if let Some(t) = ast::token(p, SyntaxKind::IDENT) {
                    self.assign(t.text(), st);
                }
            }
            SyntaxKind::MATCH_AS_PATTERN => {
                for c in p.children() {
                    self.pattern(&c, st);
                }
                if let Some(t) = p
                    .children_with_tokens()
                    .filter_map(|e| e.into_token())
                    .filter(|t| t.kind() == SyntaxKind::IDENT)
                    .last()
                {
                    self.assign(t.text(), st);
                }
            }
            SyntaxKind::MATCH_VALUE_PATTERN | SyntaxKind::MATCH_CLASS_PATTERN => {
                if let Some(t) = ast::token(p, SyntaxKind::IDENT) {
                    self.read(&t, st);
                }
                for c in p.children() {
                    self.pattern(&c, st);
                }
            }
            _ => {
                for c in p.children() {
                    if ast::Expr::can_cast(c.kind()) {
                        self.expr(&c, st);
                    } else {
                        self.pattern(&c, st);
                    }
                }
            }
        }
    }

    /// Assignment target: names are assigned, the objects of attribute and
    /// subscript targets are read.
    fn annotation_target(&mut self, target: &ast::Expr, state: &mut Vec<bool>) -> bool {
        match target.clone().unparen() {
            ast::Expr::Name(_) => true,
            ast::Expr::Attribute(attribute) => attribute
                .object()
                .is_none_or(|object| self.expr(object.syntax(), state)),
            ast::Expr::Subscript(subscript) => {
                if subscript
                    .object()
                    .is_some_and(|object| !self.expr(object.syntax(), state))
                {
                    return false;
                }
                subscript
                    .index()
                    .is_none_or(|index| self.expr(&index, state))
            }
            _ => true,
        }
    }

    fn operation_completes(
        &mut self,
        expression: &ast::Expr,
        kind: rustpy_semantic::flow::OperationKind,
        state: &mut [bool],
    ) -> bool {
        if self.inf.operation_terminates(self.file, expression, kind) {
            merge_completion(&mut self.abrupt, ExitReason::Raise, state.to_vec());
            false
        } else {
            true
        }
    }

    fn truth_expr(&mut self, expression: &ast::Expr, state: &mut Vec<bool>) -> bool {
        self.expr(expression.syntax(), state)
            && self.operation_completes(
                expression,
                rustpy_semantic::flow::OperationKind::Truth,
                state,
            )
    }

    fn target_operation(
        &mut self,
        target: &ast::Expr,
        kind: rustpy_semantic::flow::OperationKind,
        state: &mut Vec<bool>,
    ) -> bool {
        let operands_complete = match target.clone().unparen() {
            ast::Expr::Attribute(_) | ast::Expr::Subscript(_) => {
                self.annotation_target(target, state)
            }
            other => self.expr(other.syntax(), state),
        };
        operands_complete && self.operation_completes(target, kind, state)
    }

    fn target(&mut self, target: &ast::Expr, state: &mut Vec<bool>) -> bool {
        if !self.operation_completes(target, rustpy_semantic::flow::OperationKind::Unpack, state) {
            return false;
        }
        match target.clone().unparen() {
            ast::Expr::Name(name) => {
                self.assign(&name.name_text(), state);
                true
            }
            ast::Expr::Tuple(tuple) => {
                merge_completion(&mut self.abrupt, ExitReason::Raise, state.clone());
                tuple.elements().all(|element| self.target(&element, state))
            }
            ast::Expr::List(list) => {
                merge_completion(&mut self.abrupt, ExitReason::Raise, state.clone());
                list.elements().all(|element| self.target(&element, state))
            }
            ast::Expr::Star(star) => star.expr().is_none_or(|inner| self.target(&inner, state)),
            other => {
                self.target_operation(&other, rustpy_semantic::flow::OperationKind::Store, state)
            }
        }
    }

    /// Whether evaluation has a normal completion. Abrupt exceptions retain
    /// the state at their actual evaluation point for the surrounding finally.
    fn expr(&mut self, node: &SyntaxNode, state: &mut Vec<bool>) -> bool {
        match node.kind() {
            SyntaxKind::NAME_EXPR => {
                if let Some(name) = ast::NameExpr::cast(node.clone()) {
                    if self
                        .comprehension_bound
                        .iter()
                        .any(|bound| bound.contains(&name.name_text()))
                    {
                        return true;
                    }
                    if let Some(token) = name.name_token() {
                        let reference = self.file.scopes.reference_at(token.text_range().start());
                        if reference
                            .and_then(|reference| {
                                self.file.scopes.resolve(reference.scope, token.text())
                            })
                            .is_some_and(|bindings| {
                                !bindings.is_empty()
                                    && bindings.iter().all(|binding| {
                                        matches!(
                                            self.file.scopes.binding(*binding).kind,
                                            BindingKind::TypeParam
                                        )
                                    })
                            })
                        {
                            return true;
                        }
                        // Reconcile this actual local load with the authoritative,
                        // source-bound semantic CFG before manufacturing an exception
                        // edge or reporting a read. Empty/unknown reaches prove nothing.
                        if let Some(index) = self.locals.get(token.text()).copied() {
                            if !state[index]
                                && self.class_outer_locals.is_empty()
                                && reference.is_some_and(|reference| {
                                    !reference.is_write
                                        && reference.range == token.text_range()
                                        && reference.name.as_str() == token.text()
                                        && matches!(
                                            self.file.scopes.scope(reference.scope).kind,
                                            ScopeKind::Function | ScopeKind::Lambda
                                        )
                                        && self.inf.reaching_bindings(self.file, &name).is_some_and(
                                            |bindings| {
                                                !bindings.is_empty()
                                                    && bindings.iter().all(|id| {
                                                        let binding = self.file.scopes.binding(*id);
                                                        binding.scope == reference.scope
                                                            && binding.name.as_str() == token.text()
                                                            && matches!(
                                                                binding.kind,
                                                                BindingKind::Assignment
                                                                    | BindingKind::AugAssignment
                                                                    | BindingKind::Param
                                                                    | BindingKind::StarParam
                                                                    | BindingKind::StarStarParam
                                                                    | BindingKind::Function
                                                                    | BindingKind::Class
                                                                    | BindingKind::Import(_)
                                                                    | BindingKind::For
                                                                    | BindingKind::With
                                                                    | BindingKind::Except
                                                                    | BindingKind::Walrus
                                                                    | BindingKind::MatchCapture
                                                            )
                                                    })
                                            },
                                        )
                                })
                            {
                                state[index] = true;
                            }
                            if !state[index] {
                                merge_completion(
                                    &mut self.abrupt,
                                    ExitReason::Raise,
                                    state.clone(),
                                );
                            }
                        }
                        self.read(&token, state);
                    }
                }
                true
            }
            SyntaxKind::NAMED_EXPR => {
                let Some(named) = ast::NamedExpr::cast(node.clone()) else {
                    return true;
                };
                if let Some(value) = named.value() {
                    if !self.expr(value.syntax(), state) {
                        return false;
                    }
                }
                if let Some(target) = named.target().and_then(|target| target.name_token()) {
                    self.assign(target.text(), state);
                }
                true
            }
            SyntaxKind::CALL_EXPR => {
                let Some(call) = ast::CallExpr::cast(node.clone()) else {
                    return true;
                };
                if let Some(callee) = call.callee() {
                    if !self.expr(callee.syntax(), state) {
                        return false;
                    }
                }
                // Runtime captures the callable before evaluating its arguments.
                let terminal = self.is_no_return_call(&ast::Expr::Call(call.clone()));
                if let Some(arguments) = call.arg_list() {
                    let arguments: Vec<_> = arguments.args().collect();
                    // CPython evaluates positional/starred arguments before
                    // keyword/** arguments, even when a star is written later.
                    for keywords in [false, true] {
                        for argument in &arguments {
                            let is_keyword = matches!(
                                argument,
                                ast::AnyArg::Keyword(_) | ast::AnyArg::StarStar(_)
                            );
                            if is_keyword != keywords {
                                continue;
                            }
                            if let Some(value) = argument.value() {
                                if !self.expr(value.syntax(), state) {
                                    return false;
                                }
                            }
                            if matches!(argument, ast::AnyArg::Star(_) | ast::AnyArg::StarStar(_)) {
                                merge_completion(
                                    &mut self.abrupt,
                                    ExitReason::Raise,
                                    state.clone(),
                                );
                            }
                        }
                    }
                }
                merge_completion(&mut self.abrupt, ExitReason::Raise, state.clone());
                !terminal
            }
            SyntaxKind::AWAIT_EXPR => {
                let Some(awaited) = ast::AwaitExpr::cast(node.clone()) else {
                    return true;
                };
                if let Some(value) = awaited.expr() {
                    if !self.expr(value.syntax(), state) {
                        return false;
                    }
                }
                merge_completion(&mut self.abrupt, ExitReason::Raise, state.clone());
                !self.is_no_return_call(&ast::Expr::Await(awaited))
            }
            SyntaxKind::NOT_EXPR => ast::NotExpr::cast(node.clone())
                .and_then(|operation| operation.operand())
                .is_none_or(|operand| self.truth_expr(&operand, state)),
            SyntaxKind::BOOL_OP_EXPR => {
                let Some(operation) = ast::BoolOpExpr::cast(node.clone()) else {
                    return true;
                };
                let mut continuing = state.clone();
                let mut finished = None;
                let mut operands = operation.operands().peekable();
                while let Some(operand) = operands.next() {
                    if !self.expr(operand.syntax(), &mut continuing) {
                        return self.normal_state(state, finished);
                    }
                    if operands.peek().is_none() {
                        return self.normal_state(state, meet(finished, Some(continuing)));
                    }
                    if !self.operation_completes(
                        &operand,
                        rustpy_semantic::flow::OperationKind::Truth,
                        &mut continuing,
                    ) {
                        return self.normal_state(state, finished);
                    }
                    match static_truth(Some(operand)) {
                        Some(truth) if truth == operation.is_or() => {
                            return self.normal_state(state, meet(finished, Some(continuing)));
                        }
                        Some(_) => {}
                        None => {
                            merge_completion(
                                &mut self.abrupt,
                                ExitReason::Raise,
                                continuing.clone(),
                            );
                            finished = meet(finished, Some(continuing.clone()));
                        }
                    }
                }
                self.normal_state(state, meet(finished, Some(continuing)))
            }
            SyntaxKind::CONDITIONAL_EXPR => {
                let Some(conditional) = ast::ConditionalExpr::cast(node.clone()) else {
                    return true;
                };
                if let Some(condition) = conditional.condition() {
                    if !self.truth_expr(&condition, state) {
                        return false;
                    }
                }
                let truth = static_truth(conditional.condition());
                if truth.is_none() {
                    merge_completion(&mut self.abrupt, ExitReason::Raise, state.clone());
                }
                let mut normal = None;
                for (wanted, arm) in [
                    (true, conditional.true_expr()),
                    (false, conditional.false_expr()),
                ] {
                    if truth.is_some_and(|truth| truth != wanted) {
                        continue;
                    }
                    let mut branch = state.clone();
                    if arm.is_none_or(|arm| self.expr(arm.syntax(), &mut branch)) {
                        normal = meet(normal, Some(branch));
                    }
                }
                self.normal_state(state, normal)
            }
            SyntaxKind::COMPARE_EXPR => {
                let Some(comparison) = ast::CompareExpr::cast(node.clone()) else {
                    return true;
                };
                let mut operands = comparison.operands();
                let Some(mut left) = operands.next() else {
                    return true;
                };
                let mut continuing = state.clone();
                if !self.expr(left.syntax(), &mut continuing) {
                    return false;
                }
                let mut finished = None;
                let mut steps = comparison.ops().zip(operands).enumerate().peekable();
                while let Some((index, (operator, right))) = steps.next() {
                    if !self.expr(right.syntax(), &mut continuing) {
                        return self.normal_state(state, finished);
                    }
                    if index <= usize::from(u16::MAX)
                        && !self.operation_completes(
                            &ast::Expr::Compare(comparison.clone()),
                            rustpy_semantic::flow::OperationKind::ComparisonValue(index as u16),
                            &mut continuing,
                        )
                    {
                        return self.normal_state(state, finished);
                    }
                    if steps.peek().is_some()
                        && index < usize::from(u16::MAX)
                        && !self.operation_completes(
                            &ast::Expr::Compare(comparison.clone()),
                            rustpy_semantic::flow::OperationKind::ComparisonTruth(index as u16),
                            &mut continuing,
                        )
                    {
                        return self.normal_state(state, finished);
                    }
                    let truth = constant_comparison(&left, &operator.text(), &right);
                    match truth {
                        Some(false) => {
                            return self.normal_state(state, meet(finished, Some(continuing)));
                        }
                        Some(true) => {}
                        None => {
                            merge_completion(
                                &mut self.abrupt,
                                ExitReason::Raise,
                                continuing.clone(),
                            );
                            if steps.peek().is_some() {
                                finished = meet(finished, Some(continuing.clone()));
                            }
                        }
                    }
                    left = right;
                }
                if !self.operation_completes(
                    &ast::Expr::Compare(comparison),
                    rustpy_semantic::flow::OperationKind::Value,
                    &mut continuing,
                ) {
                    return self.normal_state(state, finished);
                }
                self.normal_state(state, meet(finished, Some(continuing)))
            }
            SyntaxKind::LAMBDA_EXPR => {
                if let Some(parameters) =
                    ast::LambdaExpr::cast(node.clone()).and_then(|lambda| lambda.params())
                {
                    for parameter in parameters.params() {
                        if let Some(default) = parameter.default() {
                            if !self.expr(default.syntax(), state) {
                                return false;
                            }
                        }
                    }
                }
                true // the body is deferred
            }
            SyntaxKind::LIST_COMP
            | SyntaxKind::SET_COMP
            | SyntaxKind::DICT_COMP
            | SyntaxKind::GENERATOR_EXPR => self.comprehension(node, state),
            _ => {
                for child in node.children() {
                    if !self.expr(&child, state) {
                        return false;
                    }
                }
                if matches!(
                    node.kind(),
                    SyntaxKind::ATTRIBUTE_EXPR
                        | SyntaxKind::SUBSCRIPT_EXPR
                        | SyntaxKind::BINARY_EXPR
                        | SyntaxKind::UNARY_EXPR
                        | SyntaxKind::YIELD_FROM_EXPR
                ) && !(node.kind() == SyntaxKind::UNARY_EXPR
                    && ast::Expr::cast(node.clone())
                        .is_some_and(|expression| literal_numeric_kind(&expression, 0).is_some()))
                {
                    merge_completion(&mut self.abrupt, ExitReason::Raise, state.clone());
                }
                ast::Expr::cast(node.clone()).is_none_or(|expression| {
                    self.operation_completes(
                        &expression,
                        rustpy_semantic::flow::OperationKind::Value,
                        state,
                    )
                })
            }
        }
    }

    fn normal_state(&self, into: &mut Vec<bool>, normal: Flow) -> bool {
        if let Some(normal) = normal {
            *into = normal;
            true
        } else {
            false
        }
    }

    fn comprehension_target(&mut self, target: &ast::Expr, state: &mut Vec<bool>) -> bool {
        match target.clone().unparen() {
            ast::Expr::Name(name) => {
                self.comprehension_bound
                    .last_mut()
                    .unwrap()
                    .insert(name.name_text());
                true
            }
            ast::Expr::Tuple(tuple) => tuple
                .elements()
                .all(|item| self.comprehension_target(&item, state)),
            ast::Expr::List(list) => list
                .elements()
                .all(|item| self.comprehension_target(&item, state)),
            ast::Expr::Star(star) => star
                .expr()
                .is_some_and(|item| self.comprehension_target(&item, state)),
            other => self.annotation_target(&other, state),
        }
    }

    fn comprehension(&mut self, node: &SyntaxNode, state: &mut Vec<bool>) -> bool {
        let clauses: Vec<_> = node.children().filter_map(ast::CompFor::cast).collect();
        let Some(first) = clauses.first().and_then(ast::CompFor::iter) else {
            return true;
        };
        if !self.expr(first.syntax(), state) {
            return false;
        }
        // Only the first iterable runs when constructing a generator. A known
        // empty eager comprehension does not visit filters, nested iterables or body.
        if node.kind() == SyntaxKind::GENERATOR_EXPR || statically_empty(&first) {
            return true;
        }
        let empty = state.clone();
        let mut entered = state.clone();
        self.comprehension_bound.push(HashSet::new());
        let mut normal = true;
        for (index, clause) in clauses.iter().enumerate() {
            if index != 0 {
                if let Some(iterator) = clause.iter() {
                    if !self.expr(iterator.syntax(), &mut entered) {
                        normal = false;
                        break;
                    }
                    if statically_empty(&iterator) {
                        normal = false;
                        break;
                    }
                }
            }
            if let Some(target) = clause.target() {
                if !self.comprehension_target(&target, &mut entered) {
                    normal = false;
                    break;
                }
            }
            for condition in clause.conditions() {
                if !self.truth_expr(&condition, &mut entered) {
                    normal = false;
                    break;
                }
                if static_truth(Some(condition)) == Some(false) {
                    normal = false;
                    break;
                }
            }
            if !normal {
                break;
            }
        }
        if normal {
            for child in node
                .children()
                .take_while(|child| child.kind() != SyntaxKind::COMP_FOR)
            {
                if !self.expr(&child, &mut entered) {
                    normal = false;
                    break;
                }
            }
        }
        self.comprehension_bound.pop();
        *state = if normal {
            meet(Some(empty), Some(entered)).unwrap()
        } else {
            empty
        };
        true // retain the possible zero-iteration path
    }

    /// Whether the expression statement `e` never completes: a function
    /// annotated `-> NoReturn`, or a project function whose every path raises
    /// or exits (`usage()` ending in `sys.exit(2)`).
    fn never_returns(&self, e: &ast::Expr) -> bool {
        let ast::Expr::Call(call) = e.clone().unparen() else {
            return false;
        };
        let Some(callee) = call.callee() else {
            return false;
        };
        if self.inferred_terminal_member_retired(&callee)
            || !self.terminal_receiver_is_static(&callee)
        {
            return false;
        }
        if self.depth >= 3 {
            return false;
        }
        let sig = match self.inf.type_of_expr(self.file, &callee) {
            Type::Function(s) | Type::BoundMethod(s, _) => s,
            _ => return false,
        };
        if sig.is_async {
            return false;
        }
        if matches!(sig.ret, Type::Never) && self.canonical_never_signature(&sig) {
            return true;
        }
        let Some(def) = sig.def.clone() else {
            return false;
        };
        let key = (def.file, def.range);
        if let Some(known) = self.exits.borrow().get(&key) {
            return *known;
        }
        // Recursion guard: assume it returns while it is being analysed.
        self.exits.borrow_mut().insert(key, false);
        let exits = self.function_always_exits(&def);
        self.exits.borrow_mut().insert(key, exits);
        exits
    }

    fn function_always_exits(&self, def: &rustpy_semantic::types::FunctionRef) -> bool {
        let Some(file) = self.inf.db.file(def.file) else {
            return false;
        };
        if file.is_stub {
            return false;
        }
        let Some(f) = func_def_with_range(&file.parse.syntax_node(), def.range) else {
            return false;
        };
        if f.is_async()
            || f.decorators().next().is_some()
            || !Self::containing_classes_are_static(&f)
        {
            return false;
        }
        let Some(body) = f.body() else { return false };
        // `return` (or `yield`) anywhere means it can come back.
        let mut stack: Vec<SyntaxNode> = vec![body.syntax().clone()];
        while let Some(n) = stack.pop() {
            for c in n.children() {
                match c.kind() {
                    SyntaxKind::RETURN_STMT
                    | SyntaxKind::YIELD_EXPR
                    | SyntaxKind::YIELD_FROM_EXPR => return false,
                    SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF | SyntaxKind::LAMBDA_EXPR => {}
                    _ => stack.push(c),
                }
            }
        }
        let mut sub = Unbound {
            inf: self.inf,
            file: &file,
            locals: HashMap::new(),
            loops: Vec::new(),
            abrupt: HashMap::new(),
            report: false,
            reports: Vec::new(),
            depth: self.depth + 1,
            infer_exits: true,
            exits: self.exits,
            member_cache: self.member_cache,
            mutated_terminal_members: self.file_member_mutations(&file),
            annotated_functions: annotated_function_names(&file.parse.syntax_node()),
            comprehension_bound: Vec::new(),
            class_outer_locals: Vec::new(),
        };
        sub.block(Some(body), Some(Vec::new()), true).is_none()
    }
}

// ---------------------------------------------------------------------------
// PyShadowingNames

/// Parameters and locals of functions, lambdas and comprehensions that reuse
/// a name bound in an enclosing function or the module (class bodies and
/// comprehensions are not outer scopes that get shadowed).
fn shadowing_names(file: &Arc<FileInfo>, sink: &mut Sink) {
    let scopes = &file.scopes;
    for scope in scopes.scopes.iter() {
        if !matches!(
            scope.kind,
            ScopeKind::Function | ScopeKind::Lambda | ScopeKind::Comprehension
        ) {
            continue;
        }
        for (name, ids) in &scope.bindings {
            if name == "_"
                || (name.starts_with("__") && name.ends_with("__"))
                || scope.globals.contains(name)
                || scope.nonlocals.contains(name)
            {
                continue;
            }
            let Some(first) = ids.iter().map(|id| scopes.binding(*id)).find(|b| {
                matches!(
                    b.kind,
                    BindingKind::Param
                        | BindingKind::StarParam
                        | BindingKind::StarStarParam
                        | BindingKind::Assignment
                        | BindingKind::AugAssignment
                        | BindingKind::Annotation
                        | BindingKind::For
                        | BindingKind::With
                        | BindingKind::Except
                        | BindingKind::Walrus
                        | BindingKind::Function
                        | BindingKind::Class
                        | BindingKind::MatchCapture
                        | BindingKind::CompTarget
                )
            }) else {
                continue;
            };
            // Class bodies are not enclosing scopes for their methods.
            let mut cur = scope.parent;
            let mut shadows = false;
            while let Some(p) = cur {
                let ps = scopes.scope(p);
                if matches!(
                    ps.kind,
                    ScopeKind::Function | ScopeKind::Lambda | ScopeKind::Module
                ) && ps
                    .bindings
                    .get(name)
                    .is_some_and(|ids| ids.iter().any(|id| scopes.binding(*id).is_definition()))
                {
                    shadows = true;
                    break;
                }
                cur = ps.parent;
            }
            if shadows {
                let fix = QuickFix {
                    title: RENAME_ELEMENT_FIX.into(),
                    edits: Vec::new(),
                };
                sink.push(
                    first.range,
                    Severity::WeakWarning,
                    "PyShadowingNames",
                    format!("Shadows name '{name}' from outer scope"),
                    vec![fix],
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// PyProtectedMember

/// `_name`: not dunder or name-mangled `__name`, and not a sunder `_name_`
/// (`Enum._value_`, ctypes `_fields_`), which is public API.
fn is_protected(name: &str) -> bool {
    name.len() > 1 && name.starts_with('_') && !name.starts_with("__") && !name.ends_with('_')
}

/// The public API of `collections.namedtuple` classes.
const NAMEDTUPLE_API: &[&str] = &[
    "_asdict",
    "_replace",
    "_make",
    "_fields",
    "_field_defaults",
    "_source",
];

/// Classes enclosing `node`, innermost first.
fn enclosing_classes(file: &FileInfo, node: &SyntaxNode) -> Vec<ClassRef> {
    node.ancestors()
        .skip(1)
        .filter_map(ast::ClassDef::cast)
        .map(|c| ClassRef {
            file: file.id,
            range: c.syntax().text_range(),
            name: SmolStr::new(c.name_text()),
        })
        .collect()
}

/// Whether `node` is test code (a `test*` function, a `Test*` class or a test module).
fn in_test_code(file: &FileInfo, node: &SyntaxNode) -> bool {
    let stem = file.path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    if stem.starts_with("test_") || stem.ends_with("_test") || stem == "conftest" || stem == "tests"
    {
        return true;
    }
    node.ancestors().any(|a| match a.kind() {
        SyntaxKind::FUNC_DEF => {
            ast::FuncDef::cast(a).is_some_and(|f| f.name_text().starts_with("test"))
        }
        SyntaxKind::CLASS_DEF => {
            ast::ClassDef::cast(a).is_some_and(|c| c.name_text().starts_with("Test"))
        }
        _ => false,
    })
}

/// The class in `c`'s MRO that declares `name`.
fn declaring_class(inf: &Inferer<'_>, c: &ClassRef, name: &str) -> Option<ClassRef> {
    inf.mro(c)
        .iter()
        .find(|cls| {
            inf.class_info(cls)
                .is_some_and(|i| i.members.contains_key(name))
        })
        .cloned()
}

/// Whether `module` belongs to the same top-level package as `file`.
fn same_package(file: &FileInfo, module: Option<&str>) -> bool {
    let top = |m: &str| m.split('.').next().unwrap_or("").to_string();
    match (file.module_name.as_deref(), module) {
        (Some(a), Some(b)) => {
            top(a) == top(b) && (a.contains('.') || file.is_package_init() || a == b)
        }
        _ => false,
    }
}

/// `obj._member` outside the classes that own `_member`, and
/// `from module import _name` from another package.
fn protected_members(inf: &Inferer<'_>, file: &Arc<FileInfo>, root: &SyntaxNode, sink: &mut Sink) {
    for node in root.descendants() {
        match node.kind() {
            SyntaxKind::ATTRIBUTE_EXPR => {
                let Some(a) = ast::AttributeExpr::cast(node.clone()) else {
                    continue;
                };
                let (Some(obj), Some(tok)) = (a.object(), a.attr()) else {
                    continue;
                };
                let name = tok.text();
                if !is_protected(name) || NAMEDTUPLE_API.contains(&name) {
                    continue;
                }
                // The class's own members: `self._x`, `cls._x`, `super()._x`.
                match &obj {
                    ast::Expr::Name(n)
                        if matches!(n.name_text().as_str(), "self" | "cls" | "mcs" | "metacls") =>
                    {
                        continue;
                    }
                    ast::Expr::Call(c)
                        if c.callee().is_some_and(|e| e.syntax().text() == "super") =>
                    {
                        continue;
                    }
                    _ => {}
                }
                if in_test_code(file, &node) {
                    continue;
                }
                let base = inf.type_of_expr(file, &obj);
                let what = match &base {
                    Type::Module(fid) => {
                        let module = inf.db.file(*fid).and_then(|m| m.module_name.clone());
                        if same_package(file, module.as_deref()) || module.is_none() {
                            continue;
                        }
                        "module"
                    }
                    Type::Instance(c, _) | Type::Class(c, _) | Type::SelfType(c) => {
                        let owner = declaring_class(inf, c, name);
                        let enclosing = enclosing_classes(file, &node);
                        let inside_hierarchy = match &owner {
                            Some(o) => enclosing.iter().any(|e| inf.mro(e).contains(o)),
                            // Unknown member: lenient inside classes.
                            None => !enclosing.is_empty(),
                        };
                        if inside_hierarchy {
                            continue;
                        }
                        "class"
                    }
                    _ => {
                        // Untyped objects: report only outside classes, as for an unknown owner.
                        if !enclosing_classes(file, &node).is_empty() {
                            continue;
                        }
                        // A module that cannot be resolved is still a module.
                        let imported = match &obj {
                            ast::Expr::Name(n) => n
                                .name_token()
                                .and_then(|t| file.scopes.reference_at(t.text_range().start()))
                                .and_then(|r| r.resolved)
                                .is_some_and(|b| {
                                    matches!(file.scopes.binding(b).kind, BindingKind::Import(_))
                                }),
                            _ => false,
                        };
                        if imported {
                            "module"
                        } else {
                            "class"
                        }
                    }
                };
                sink.push(
                    tok.text_range(),
                    Severity::WeakWarning,
                    "PyProtectedMember",
                    format!("Access to a protected member {name} of a {what}"),
                    Vec::new(),
                );
            }
            SyntaxKind::IMPORT_FROM_STMT => {
                let Some(imp) = ast::ImportFromStmt::cast(node.clone()) else {
                    continue;
                };
                if imp.level() > 0 || in_test_code(file, &node) {
                    continue;
                }
                let module = imp.module().map(|m| m.path()).unwrap_or_default();
                if module == "__future__" || same_package(file, Some(&module)) {
                    continue;
                }
                for alias in imp.aliases() {
                    let name = alias.name();
                    if !is_protected(&name) {
                        continue;
                    }
                    if let Some(t) = alias.name_token() {
                        sink.push(
                            t.text_range(),
                            Severity::WeakWarning,
                            "PyProtectedMember",
                            format!("Access to a protected member {name} of a module"),
                            Vec::new(),
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// PyPrivateMember

/// `__name` (not dunder): mangled to `_Class__name` inside class `Class`.
fn is_private(name: &str) -> bool {
    name.len() > 2 && name.starts_with("__") && !name.ends_with("__")
}

/// `obj.__member` where name mangling does not reach the class declaring
/// `__member`: outside every class body, or inside an unrelated class (which
/// mangles the name to its own `_Other__member`, a subclass included).
fn private_members(inf: &Inferer<'_>, file: &Arc<FileInfo>, root: &SyntaxNode, sink: &mut Sink) {
    for node in root.descendants() {
        let Some(a) = ast::AttributeExpr::cast(node.clone()) else {
            continue;
        };
        let (Some(obj), Some(tok)) = (a.object(), a.attr()) else {
            continue;
        };
        let name = tok.text();
        if !is_private(name) {
            continue;
        }
        let enclosing = enclosing_classes(file, &node);
        let base = inf.type_of_expr(file, &obj);
        let owner = match &base {
            // Module globals are never mangled; `module.__x` is a plain lookup.
            Type::Module(_) => continue,
            Type::Instance(c, _) | Type::Class(c, _) | Type::SelfType(c) => {
                declaring_class(inf, c, name)
            }
            _ => None,
        };
        let what = match owner {
            // Mangling uses the name of the class whose body contains the access.
            Some(owner) => {
                if enclosing.iter().any(|e| e.name == owner.name) {
                    continue;
                }
                format!("class {}", owner.name)
            }
            // Unknown owner: inside a class the access may be the class's own.
            None if !enclosing.is_empty() => continue,
            // Outside every class body nothing is mangled; only report receivers
            // known to be classes or instances (not untyped or imported names).
            None if matches!(
                base,
                Type::Instance(..) | Type::Class(..) | Type::SelfType(_)
            ) =>
            {
                "class".to_string()
            }
            None => continue,
        };
        sink.push(
            tok.text_range(),
            Severity::WeakWarning,
            "PyPrivateMember",
            format!("Access to a private member {name} of a {what}"),
            Vec::new(),
        );
    }
}

// ---------------------------------------------------------------------------
// PyAttributeOutsideInit

/// Methods that initialize instances (and test fixtures).
const INIT_METHODS: &[&str] = &[
    "__init__",
    "__new__",
    "__post_init__",
    "__setstate__",
    "__init_subclass__",
    "setUp",
    "asyncSetUp",
    "setup_method",
    "setup",
    "setUpClass",
];

/// The names `f` refers to the instance by: its first parameter, and in
/// `__new__` (whose first parameter is the class) the locals bound to the new
/// instance (`self = super().__new__(cls)`, `obj = object.__new__(cls)`).
fn instance_names(f: &ast::FuncDef) -> Vec<String> {
    let mut names: Vec<String> = f
        .params()
        .and_then(|p| p.params().next())
        .and_then(|p| p.name())
        .map(|t| t.text().to_string())
        .into_iter()
        .collect();
    if f.name_text() != "__new__" {
        return names;
    }
    let Some(body) = f.body() else { return names };
    let mut stack: Vec<SyntaxNode> = vec![body.syntax().clone()];
    while let Some(n) = stack.pop() {
        for c in n.children() {
            match c.kind() {
                SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF | SyntaxKind::LAMBDA_EXPR => {}
                SyntaxKind::ASSIGN_STMT => {
                    let Some(a) = ast::AssignStmt::cast(c.clone()) else {
                        continue;
                    };
                    let creates_instance = matches!(a.value(), Some(ast::Expr::Call(call)) if matches!(call.callee(), Some(ast::Expr::Attribute(attr)) if attr.attr_text() == "__new__"));
                    if creates_instance {
                        names.extend(a.targets().iter().filter_map(|t| match t {
                            ast::Expr::Name(n) => Some(n.name_text()),
                            _ => None,
                        }));
                    }
                }
                _ => stack.push(c),
            }
        }
    }
    names
}

/// `self.<name> = …` targets assigned directly in `f` (not in nested functions).
fn self_assignments(f: &ast::FuncDef) -> Vec<SyntaxToken> {
    let self_names = instance_names(f);
    if self_names.is_empty() {
        return Vec::new();
    }
    let Some(body) = f.body() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack: Vec<SyntaxNode> = vec![body.syntax().clone()];
    while let Some(n) = stack.pop() {
        for c in n.children() {
            match c.kind() {
                SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF | SyntaxKind::LAMBDA_EXPR => {}
                SyntaxKind::ASSIGN_STMT => {
                    if let Some(a) = ast::AssignStmt::cast(c.clone()) {
                        for t in a.targets() {
                            self_targets(&t, &self_names, &mut out);
                        }
                    }
                    stack.push(c);
                }
                SyntaxKind::ANN_ASSIGN_STMT => {
                    if let Some(a) = ast::AnnAssignStmt::cast(c.clone()) {
                        if let (Some(t), Some(_)) = (a.target(), a.value()) {
                            self_targets(&t, &self_names, &mut out);
                        }
                    }
                }
                SyntaxKind::FOR_STMT => {
                    if let Some(t) = ast::ForStmt::cast(c.clone()).and_then(|f| f.target()) {
                        self_targets(&t, &self_names, &mut out);
                    }
                    stack.push(c);
                }
                SyntaxKind::WITH_ITEM => {
                    if let Some(t) = ast::WithItem::cast(c.clone()).and_then(|w| w.target()) {
                        self_targets(&t, &self_names, &mut out);
                    }
                }
                _ => stack.push(c),
            }
        }
    }
    out.sort_by_key(|t| t.text_range().start());
    out
}

fn self_targets(t: &ast::Expr, self_names: &[String], out: &mut Vec<SyntaxToken>) {
    match t {
        ast::Expr::Attribute(a) => {
            if let (Some(ast::Expr::Name(obj)), Some(attr)) = (a.object(), a.attr()) {
                if self_names.contains(&obj.name_text()) {
                    out.push(attr);
                }
            }
        }
        ast::Expr::Tuple(tu) => tu
            .elements()
            .for_each(|e| self_targets(&e, self_names, out)),
        ast::Expr::List(l) => l.elements().for_each(|e| self_targets(&e, self_names, out)),
        ast::Expr::Paren(p) => {
            if let Some(i) = p.inner() {
                self_targets(&i, self_names, out);
            }
        }
        ast::Expr::Star(s) => {
            if let Some(i) = s.expr() {
                self_targets(&i, self_names, out);
            }
        }
        _ => {}
    }
}

/// Names of methods `f` calls as `self.<name>(…)` (on the new instance in `__new__`).
fn self_method_calls(f: &ast::FuncDef) -> Vec<String> {
    let self_names = instance_names(f);
    let Some(body) = f.body() else {
        return Vec::new();
    };
    body.syntax()
        .descendants()
        .filter_map(ast::CallExpr::cast)
        .filter_map(|c| match c.callee()? {
            ast::Expr::Attribute(a) => match a.object()? {
                ast::Expr::Name(n) if self_names.contains(&n.name_text()) => Some(a.attr_text()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn is_instance_method(f: &ast::FuncDef) -> bool {
    !f.decorators().filter_map(|d| d.name_text()).any(|d| {
        matches!(
            d.as_str(),
            "staticmethod" | "classmethod" | "abc.abstractclassmethod" | "abstractclassmethod"
        )
    })
}

/// `self.x = …` in a method other than `__init__` (or what `__init__`
/// calls) for an attribute the class body, `__init__` and the base classes
/// never declare.
fn attributes_outside_init(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    root: &SyntaxNode,
    sink: &mut Sink,
) {
    let text: &str = &file.text;
    for class in root.descendants().filter_map(ast::ClassDef::cast) {
        let methods: Vec<ast::FuncDef> = class.methods().filter(is_instance_method).collect();
        let reportable = |m: &ast::FuncDef, init: &HashSet<String>| {
            let name = m.name_text();
            // Special methods (`__enter__`, `__setstate__`, …) set up state on purpose.
            !init.contains(&name) && !(name.starts_with("__") && name.ends_with("__"))
        };
        // Cheap checks first: assignments in other methods that neither the
        // class body nor its own initializers declare.
        let own_init = init_methods(std::slice::from_ref(&class));
        let candidates: Vec<SyntaxToken> = methods
            .iter()
            .filter(|m| reportable(m, &own_init))
            .flat_map(self_assignments)
            .collect();
        if candidates.is_empty() {
            continue;
        }
        let Some(scope) = file.scopes.scope_for_node(class.syntax().text_range()) else {
            continue;
        };
        // Declared: class-level names (attributes, properties, `__slots__`).
        let mut declared: HashSet<String> = file
            .scopes
            .scope(scope)
            .bindings
            .keys()
            .map(|k| k.to_string())
            .collect();
        if let Some(slots) = class
            .body()
            .into_iter()
            .flat_map(|b| b.statements().collect::<Vec<_>>())
            .find_map(|s| match s {
                ast::Stmt::Assign(a)
                    if a.targets().iter().any(|t| t.syntax().text() == "__slots__") =>
                {
                    a.value()
                }
                _ => None,
            })
        {
            declared.extend(
                slots
                    .syntax()
                    .descendants()
                    .filter_map(ast::StringExpr::cast)
                    .map(|s| s.value()),
            );
        }
        for m in methods.iter().filter(|m| own_init.contains(&m.name_text())) {
            declared.extend(self_assignments(m).iter().map(|t| t.text().to_string()));
        }
        if candidates.iter().all(|t| declared.contains(t.text())) {
            continue;
        }
        // The base classes: their members, and the methods their initializers
        // call through `self` (template methods such as `_create_option_list()`
        // overridden here). Unknown bases may declare anything.
        let cref = ClassRef {
            file: file.id,
            range: class.syntax().text_range(),
            name: SmolStr::new(class.name_text()),
        };
        if inf.has_unknown_base(&cref) {
            continue;
        }
        let mro = inf.mro(&cref);
        for base in mro.iter().skip(1) {
            if let Some(bi) = inf.class_info(base) {
                declared.extend(bi.members.keys().map(|k| k.to_string()));
            }
        }
        let hierarchy: Vec<ast::ClassDef> =
            mro.iter().filter_map(|c| class_def_of(inf, c)).collect();
        let init = init_methods(&hierarchy);
        for m in methods
            .iter()
            .filter(|m| init.contains(&m.name_text()) && !own_init.contains(&m.name_text()))
        {
            declared.extend(self_assignments(m).iter().map(|t| t.text().to_string()));
        }
        let init_def = methods.iter().find(|m| m.name_text() == "__init__");
        for m in methods.iter().filter(|m| reportable(m, &init)) {
            let mut reported: HashSet<String> = HashSet::new();
            for tok in self_assignments(m) {
                let name = tok.text().to_string();
                if declared.contains(&name) || !reported.insert(name.clone()) {
                    continue;
                }
                let fixes = init_def
                    .and_then(|f| init_attribute_fix(text, f, &name))
                    .into_iter()
                    .collect();
                sink.push(
                    tok.text_range(),
                    Severity::WeakWarning,
                    "PyAttributeOutsideInit",
                    format!("Instance attribute {name} defined outside __init__"),
                    fixes,
                );
            }
        }
    }
}

/// Names of the initializers ([`INIT_METHODS`]) of `classes` and of the
/// methods they call through `self`, transitively.
fn init_methods(classes: &[ast::ClassDef]) -> HashSet<String> {
    let mut init: HashSet<String> = INIT_METHODS.iter().map(|s| s.to_string()).collect();
    loop {
        let before = init.len();
        for cd in classes {
            for m in cd
                .methods()
                .filter(|m| init.contains(&m.name_text()))
                .collect::<Vec<_>>()
            {
                init.extend(self_method_calls(&m));
            }
        }
        if init.len() == before {
            return init;
        }
    }
}

/// The `class` statement of `c`.
fn class_def_of(inf: &Inferer<'_>, c: &ClassRef) -> Option<ast::ClassDef> {
    let f = inf.db.file(c.file)?;
    let start = match f.parse.syntax_node().covering_element(c.range) {
        rustpy_syntax::rowan::NodeOrToken::Node(n) => n,
        rustpy_syntax::rowan::NodeOrToken::Token(t) => t.parent()?,
    };
    start
        .ancestors()
        .filter(|n| n.text_range() == c.range)
        .find_map(ast::ClassDef::cast)
}

/// "Add 'self.x = None' to __init__": a new last statement of `__init__`.
fn init_attribute_fix(text: &str, init: &ast::FuncDef, name: &str) -> Option<QuickFix> {
    let self_name = init
        .params()
        .and_then(|p| p.params().next())
        .and_then(|p| p.name())?
        .text()
        .to_string();
    let body = init.body()?;
    let first = body.statements().next()?;
    let last = body.statements().last()?;
    let first_start: usize = first.syntax().text_range().start().into();
    let def_start: usize = init.syntax().text_range().start().into();
    if text[..first_start]
        .rfind('\n')
        .is_none_or(|nl| nl < def_start)
    {
        // `def __init__(self): pass` on one line.
        return None;
    }
    let indent = indent_at(text, first_start);
    let end: usize = last.syntax().text_range().end().into();
    let at = line_end_incl(text, end.saturating_sub(1).max(first_start));
    let mut insert = format!("{indent}{self_name}.{name} = None\n");
    if at == text.len() && !text.ends_with('\n') {
        insert.insert(0, '\n');
    }
    Some(QuickFix {
        title: format!("Add '{self_name}.{name} = None' to __init__"),
        edits: vec![(TextRange::empty(TextSize::new(at as u32)), insert)],
    })
}

// ---------------------------------------------------------------------------
// PyAbstractClass

fn is_abstract_method(f: &ast::FuncDef) -> bool {
    f.decorators().filter_map(|d| d.name_text()).any(|d| {
        matches!(
            d.rsplit('.').next().unwrap_or(""),
            "abstractmethod" | "abstractproperty" | "abstractclassmethod" | "abstractstaticmethod"
        )
    })
}

/// Whether the class says it is abstract: an `ABC` / `Protocol` base, an
/// `ABCMeta` metaclass, or abstract methods of its own.
fn declares_abstract(class: &ast::ClassDef) -> bool {
    let bases = class.bases().iter().any(|b| {
        let t = b.syntax().text().to_string();
        let head = t.split('[').next().unwrap_or("");
        matches!(head.rsplit('.').next().unwrap_or(""), "ABC" | "Protocol")
    });
    let meta = class.arguments().is_some_and(|a| {
        a.keywords().any(|k| {
            k.name_text() == "metaclass"
                && k.value()
                    .is_some_and(|v| v.syntax().text().to_string().ends_with("ABCMeta"))
        })
    });
    bases || meta || class.methods().any(|m| is_abstract_method(&m))
}

/// Resolve Protocol's declaration, including aliases and subscripted bases.
/// ClassInfo's spelling flag is useful for inference but cannot prove ABCMeta.
fn protocol_base(inf: &Inferer<'_>, file: &Arc<FileInfo>, expr: &ast::Expr) -> bool {
    let head = match expr {
        ast::Expr::Subscript(s) => s.object(),
        other => Some(other.clone()),
    };
    let Some(head) = head else { return false };
    let def = match &head {
        ast::Expr::Name(n) => inf.resolve_name(
            file,
            file.scopes.scope_at(n.syntax().text_range().start()),
            &n.name_text(),
        ),
        ast::Expr::Attribute(a) => {
            a.object()
                .and_then(|object| match inf.type_of_expr(file, &object) {
                    Type::Module(m) => inf
                        .db
                        .file(m)
                        .and_then(|m| inf.resolve_in_module(&m, &a.attr_text(), 0)),
                    _ => None,
                })
        }
        _ => None,
    };
    def.map(|d| inf.follow_imports(d)).is_some_and(|d| {
        d.binding().name == "Protocol"
            && matches!(
                d.file.module_name.as_deref(),
                Some("typing" | "typing_extensions")
            )
            && d.file.path.starts_with("<typeshed>")
    })
}

fn resolved_protocol_bases(inf: &Inferer<'_>, c: &ClassRef) -> Vec<String> {
    let (Some(info), Some(file)) = (inf.class_info(c), inf.db.file(c.file)) else {
        return Vec::new();
    };
    let root = file.parse.syntax_node();
    info.base_exprs
        .iter()
        .filter_map(|range| {
            let expr = root
                .descendants()
                .find(|n| n.text_range() == *range && ast::Expr::can_cast(n.kind()))
                .and_then(ast::Expr::cast)?;
            if !protocol_base(inf, &file, &expr) {
                return None;
            }
            let head = match expr {
                ast::Expr::Subscript(s) => s.object()?,
                other => other,
            };
            Some(
                head.syntax()
                    .text()
                    .to_string()
                    .rsplit('.')
                    .next()?
                    .to_string(),
            )
        })
        .collect()
}

fn has_unknown_abstract_base(inf: &Inferer<'_>, c: &ClassRef) -> bool {
    let names: Vec<String> = inf
        .mro(c)
        .iter()
        .flat_map(|c| resolved_protocol_bases(inf, c))
        .collect();
    inf.has_unknown_base_ignoring(c, &names.iter().map(String::as_str).collect::<Vec<_>>())
}

/// Whether abstract methods are enforced for `c`: `ABCMeta` (directly, via
/// `ABC`, or via a `Protocol` base, which the MRO leaves out) is the
/// metaclass of a class in its MRO.
fn uses_abc_meta(db: &Analysis, inf: &Inferer<'_>, c: &ClassRef) -> bool {
    let abc_class = |cls: &ClassRef, name: &str| {
        cls.name == name
            && db.file(cls.file).is_some_and(|f| {
                f.module_name.as_deref() == Some("abc")
                    && rustpy_semantic::external_docs::origin_of(db, &f, "abc")
                        == rustpy_semantic::external_docs::Origin::Stdlib
            })
    };
    inf.mro(c).iter().any(|cls| {
        if abc_class(cls, "ABC") {
            return true;
        }
        let (Some(info), Some(f)) = (inf.class_info(cls), db.file(cls.file)) else {
            return false;
        };
        if !resolved_protocol_bases(inf, cls).is_empty() {
            return true;
        }
        let root = f.parse.syntax_node();
        info.keywords.iter().any(|(k, r)| {
            if k != "metaclass" {
                return false;
            }
            let expr = root
                .descendants()
                .find(|n| n.text_range() == *r && ast::Expr::can_cast(n.kind()))
                .and_then(ast::Expr::cast);
            let Some(expr) = expr else { return false };
            let Type::Class(meta, _) = inf.type_of_expr(&f, &expr) else {
                return false;
            };
            inf.mro(&meta).iter().any(|base| abc_class(base, "ABCMeta"))
        })
    })
}

/// Abstract methods that `c` leaves unimplemented (MRO order); its own ones
/// only with `own`. A concrete class from a stub implements everything above
/// it: typeshed derives runtime classes from abstract typing bases
/// (`io.BytesIO` from `typing.BinaryIO`) without repeating every member.
fn unimplemented_abstract(
    db: &Analysis,
    inf: &Inferer<'_>,
    c: &ClassRef,
    own: bool,
) -> Vec<String> {
    let mut seen: HashSet<SmolStr> = HashSet::new();
    let mut out = Vec::new();
    for (i, cls) in inf.mro(c).iter().enumerate() {
        let (Some(info), Some(cfile)) = (inf.class_info(cls), db.file(cls.file)) else {
            continue;
        };
        if cfile.is_stub && class_def_of(inf, cls).is_some_and(|d| !declares_abstract(&d)) {
            break;
        }
        let croot = cfile.parse.syntax_node();
        for (name, m) in info.members.iter() {
            if !seen.insert(name.clone()) {
                continue;
            }
            let def = super::token_at(&croot, m.range)
                .and_then(|t| t.parent_ancestors().find_map(ast::FuncDef::cast))
                .filter(|f| f.name_text() == name.as_str());
            if (i > 0 || own) && def.as_ref().is_some_and(is_abstract_method) {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// The last name of a callee that may be a class being instantiated: `Name`
/// or a dotted `module.Name` in CapWords (inferring every call is too slow).
fn class_like_callee(callee: &ast::Expr) -> Option<String> {
    let last = match callee {
        ast::Expr::Name(n) => n.name_text(),
        ast::Expr::Attribute(a) => {
            let mut obj = a.object();
            loop {
                match obj {
                    Some(ast::Expr::Name(_)) => break,
                    Some(ast::Expr::Attribute(inner)) => obj = inner.object(),
                    _ => return None,
                }
            }
            a.attr_text()
        }
        _ => return None,
    };
    last.starts_with(|c: char| c.is_uppercase()).then_some(last)
}

/// Whether `expr` is a name bound more than once in the scope it resolves to
/// (`try: from a import TopRequest as Request` / `except ImportError: from a
/// import Request`): which class it is depends on the run, while inference
/// only sees the last binding.
fn has_alternative_definitions(file: &FileInfo, expr: &ast::Expr) -> bool {
    let ast::Expr::Name(n) = expr else {
        return false;
    };
    let scope = super::scope_of_node(file, n.syntax());
    file.scopes
        .resolve(scope, &n.name_text())
        .is_some_and(|ids| {
            ids.iter()
                .filter(|id| file.scopes.binding(**id).is_definition())
                .count()
                > 1
        })
}

fn abstract_classes(
    db: &Analysis,
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    root: &SyntaxNode,
    sink: &mut Sink,
) {
    // Concrete subclasses that leave abstract methods unimplemented.
    for class in root.descendants().filter_map(ast::ClassDef::cast) {
        let Some(name) = class.name() else { continue };
        let cref = ClassRef {
            file: file.id,
            range: class.syntax().text_range(),
            name: SmolStr::new(name.text()),
        };
        if declares_abstract(&class)
            || class
                .bases()
                .iter()
                .any(|b| has_alternative_definitions(file, b))
            || has_unknown_abstract_base(inf, &cref)
            || !uses_abc_meta(db, inf, &cref)
        {
            continue;
        }
        let missing = unimplemented_abstract(db, inf, &cref, false);
        if missing.is_empty() {
            continue;
        }
        let mut fixes = Vec::new();
        if let Ok(r) = rustpy_refactor::refactor2::implement_abstract_methods(
            db,
            file,
            name.text_range().start(),
        ) {
            fixes.push(QuickFix {
                title: "Implement abstract methods".into(),
                edits: r.edits,
            });
        }
        sink.push(
            name.text_range(),
            Severity::WeakWarning,
            "PyAbstractClass",
            format!("Class {} must implement all abstract methods", name.text()),
            fixes,
        );
    }
    // Instantiating a class with abstract methods.
    let mut known: HashMap<ClassRef, Vec<String>> = HashMap::new();
    for call in root.descendants().filter_map(ast::CallExpr::cast) {
        let Some(callee) = call.callee() else {
            continue;
        };
        let Some(last) = class_like_callee(&callee) else {
            continue;
        };
        if has_alternative_definitions(file, &callee) {
            continue;
        }
        let Type::Class(c, _) = inf.type_of_expr(file, &callee) else {
            continue;
        };
        // `cls()` / `factory()` may create a concrete subclass: only the class's own name counts.
        if c.name != last {
            continue;
        }
        let missing = known.entry(c.clone()).or_insert_with(|| {
            if has_unknown_abstract_base(inf, &c)
                || !uses_abc_meta(db, inf, &c)
                || !resolved_protocol_bases(inf, &c).is_empty()
            {
                return Vec::new();
            }
            unimplemented_abstract(db, inf, &c, true)
        });
        if missing.is_empty() {
            continue;
        }
        let list = missing
            .iter()
            .map(|m| format!("'{m}'"))
            .collect::<Vec<_>>()
            .join(", ");
        sink.push(
            callee.syntax().text_range(),
            Severity::Warning,
            "PyAbstractClass",
            format!(
                "Cannot instantiate abstract class '{}' (abstract methods: {list})",
                c.name
            ),
            Vec::new(),
        );
    }
}

// ---------------------------------------------------------------------------
// PyAsyncCall

/// The `def` statement with exactly `range`.
fn func_def_with_range(root: &SyntaxNode, range: TextRange) -> Option<ast::FuncDef> {
    let start = match root.covering_element(range) {
        rustpy_syntax::rowan::NodeOrToken::Node(n) => n,
        rustpy_syntax::rowan::NodeOrToken::Token(t) => t.parent()?,
    };
    start
        .ancestors()
        .filter(|n| n.text_range() == range)
        .find_map(ast::FuncDef::cast)
}

/// Whether the function behind `sig` is an async generator (`async def`
/// with `yield`), whose call returns an async iterator, not a coroutine.
fn is_async_generator(db: &Analysis, sig: &rustpy_semantic::types::Signature) -> bool {
    let Some(def) = &sig.def else { return false };
    let Some(f) = db.file(def.file) else {
        return false;
    };
    let Some(body) = func_def_with_range(&f.parse.syntax_node(), def.range).and_then(|f| f.body())
    else {
        return false;
    };
    let mut stack: Vec<SyntaxNode> = vec![body.syntax().clone()];
    while let Some(n) = stack.pop() {
        for c in n.children() {
            match c.kind() {
                SyntaxKind::YIELD_EXPR | SyntaxKind::YIELD_FROM_EXPR => return true,
                SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF | SyntaxKind::LAMBDA_EXPR => {}
                _ => stack.push(c),
            }
        }
    }
    false
}

/// `coroutine_function()` as a statement of an `async def`: the coroutine
/// is created and dropped without running.
fn async_calls(inf: &Inferer<'_>, file: &Arc<FileInfo>, root: &SyntaxNode, sink: &mut Sink) {
    for stmt in root.descendants().filter_map(ast::ExprStmt::cast) {
        let Some(ast::Expr::Call(call)) = stmt.expr() else {
            continue;
        };
        let in_async = stmt
            .syntax()
            .ancestors()
            .find(|a| {
                matches!(
                    a.kind(),
                    SyntaxKind::FUNC_DEF | SyntaxKind::LAMBDA_EXPR | SyntaxKind::CLASS_DEF
                )
            })
            .and_then(ast::FuncDef::cast)
            .is_some_and(|f| f.is_async());
        if !in_async {
            continue;
        }
        let Some(callee) = call.callee() else {
            continue;
        };
        let sig = match inf.type_of_expr(file, &callee) {
            Type::Function(s) | Type::BoundMethod(s, _) => s,
            Type::Overloaded(sigs) => match sigs.first() {
                Some(s) => s.clone(),
                None => continue,
            },
            _ => continue,
        };
        let returns_coroutine = matches!(&sig.ret, Type::Instance(c, _) if matches!(c.name.as_str(), "Coroutine" | "CoroutineType"));
        if !(sig.is_async || returns_coroutine) || is_async_generator(inf.db, &sig) {
            continue;
        }
        let name = match &callee {
            ast::Expr::Name(n) => n.name_text(),
            ast::Expr::Attribute(a) => a.attr_text(),
            other => other.syntax().text().to_string(),
        };
        let fix = QuickFix {
            title: "Add 'await'".into(),
            edits: vec![(
                TextRange::empty(call.syntax().text_range().start()),
                "await ".into(),
            )],
        };
        sink.push(
            call.syntax().text_range(),
            Severity::Warning,
            "PyAsyncCall",
            format!("Coroutine '{name}' is not awaited"),
            vec![fix],
        );
    }
}

#[cfg(test)]
mod tests {
    mod terminal_parentheses_tests {
        include!("core_names_terminal_parentheses_tests.rs");
    }

    mod terminal_contract_tests {
        include!("core_names_terminal_contract_tests.rs");
    }

    mod terminal_call_additional_tests {
        include!("core_names_terminal_call_additional_tests.rs");
    }

    mod terminal_call_tests {
        include!("core_names_terminal_tests.rs");
    }

    use std::path::Path;

    use crate::inspections::{inspect, InspectionOptions, Problem};
    use rustpy_semantic::db::Analysis;

    fn run_in(path: &str, src: &str) -> Vec<Problem> {
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new(path), src);
        let f = db.file(id).unwrap();
        let opts = InspectionOptions {
            pep8: false,
            unused_imports: false,
            unused_variables: false,
            code_smells: false,
            naming: false,
            security: false,
            compatibility: false,
            type_checker: false,
            ..Default::default()
        };
        inspect(&db, &f, &opts)
    }

    fn run(src: &str) -> Vec<Problem> {
        run_in("/proj/m.py", src)
    }

    /// `(code, text of the range, message)` of the problems with `code`.
    fn found(src: &str, problems: &[Problem], code: &str) -> Vec<(String, String)> {
        problems
            .iter()
            .filter(|p| p.diagnostic.code == code)
            .map(|p| {
                (
                    src[p.diagnostic.range].to_string(),
                    p.diagnostic.message.clone(),
                )
            })
            .collect()
    }

    fn apply(src: &str, problems: &[Problem], code: &str, title: &str) -> String {
        let fix = problems
            .iter()
            .filter(|p| p.diagnostic.code == code)
            .flat_map(|p| p.fixes.iter())
            .find(|f| f.title.starts_with(title))
            .unwrap_or_else(|| panic!("no fix {title}"));
        rustpy_semantic::edit_helpers::apply_edits(src, &fix.edits)
    }

    #[test]
    fn unbound_locals_follow_control_flow() {
        let src = "import sys\n\n\ndef branches(x):\n    if x > 0:\n        result = 1\n    elif x < 0:\n        result = -1\n    return result\n\n\ndef all_paths(x):\n    if x:\n        value = 1\n    else:\n        value = 2\n    return value\n\n\ndef handler(s):\n    try:\n        num = int(s)\n    except ValueError:\n        print('bad')\n    return num\n\n\ndef handled(s):\n    try:\n        num = int(s)\n    except ValueError:\n        return None\n    return num\n\n\ndef loop(items):\n    for item in items:\n        last = item\n    return last\n\n\ndef searched(items):\n    for item in items:\n        if item:\n            found = item\n            break\n    else:\n        raise LookupError\n    return found\n\n\ndef forever():\n    while True:\n        line = input()\n        if line:\n            break\n    return line\n\n\ndef exits(flag):\n    if flag:\n        mode = 'a'\n    else:\n        sys.exit(1)\n    return mode\n\n\ndef before():\n    print(total)\n    total = 0\n    return total\n\n\ndef walrus(data):\n    if (n := len(data)) > 3:\n        pass\n    return n\n\n\ndef deleted():\n    tmp = 1\n    del tmp\n    return tmp\n\n\ndef cleanup():\n    try:\n        conn = open('f')\n    finally:\n        conn.close()\n\n\ndef nested():\n    def inner():\n        return later\n    later = 1\n    return inner\n\n\ndef matched(cmd):\n    match cmd:\n        case 'go':\n            speed = 1\n        case _:\n            speed = 0\n    return speed\n\n\ndef usage():\n    print('usage')\n    sys.exit(2)\n\n\ndef parse(argv):\n    try:\n        opts = int(argv)\n    except ValueError:\n        usage()\n    return opts\n";
        let p = run(src);
        let names: Vec<(String, String)> = found(src, &p, "PyUnboundLocalVariable");
        let reported: Vec<&str> = names.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            reported,
            vec!["result", "num", "last", "total", "tmp", "conn"],
            "{names:?}"
        );
        assert_eq!(
            names[0].1,
            "Local variable 'result' might be referenced before assignment"
        );
    }

    #[test]
    fn shadowed_names_from_outer_scopes() {
        let src = "import json\ndata = []\n\n\ndef load(data, _):\n    for json in data:\n        print(json)\n\n\nclass Store:\n    items = 1\n\n    def get(self, items):\n        return items\n\n\ndef outer():\n    count = 0\n\n    def inner():\n        count = 1\n        return count\n    return inner, count\n";
        let p = run(src);
        let names: Vec<String> = found(src, &p, "PyShadowingNames")
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(
            names,
            vec!["data", "json", "count"],
            "class attributes are not an outer scope of methods"
        );
        assert!(p.iter().any(
            |x| x.diagnostic.message == "Shadows name 'data' from outer scope"
                && x.fixes.iter().any(|f| f.title == super::RENAME_ELEMENT_FIX)
        ));
    }

    #[test]
    fn protected_member_access() {
        let src = "import os\nfrom os import _exit\n\n\nclass Base:\n    def __init__(self):\n        self._value = 1\n\n    def same(self, other: 'Base'):\n        return other._value\n\n\nclass Child(Base):\n    def peek(self, other: Base):\n        return other._value\n\n\nclass Other:\n    def peek(self, b: Base):\n        return b._value\n\n\ndef outside(b: Base):\n    os._exit(0)\n    return b._value, b.__class__\n";
        let p = run(src);
        let hits = found(src, &p, "PyProtectedMember");
        let msgs: Vec<&str> = hits.iter().map(|(_, m)| m.as_str()).collect();
        assert_eq!(
            msgs,
            vec![
                "Access to a protected member _exit of a module",
                "Access to a protected member _value of a class",
                "Access to a protected member _exit of a module",
                "Access to a protected member _value of a class"
            ],
            "{hits:?}"
        );
        // Tests may poke at internals.
        assert!(found(src, &run_in("/proj/test_m.py", src), "PyProtectedMember").is_empty());
    }

    #[test]
    fn private_member_access_outside_the_declaring_class() {
        let src = "import os\n\n\nclass Vault:\n    __secret = 1\n\n    def __init__(self):\n        self.__key = 2\n\n    def own(self, other: 'Vault'):\n        return self.__secret, other.__secret, Vault.__secret, self.__dict__\n\n\nclass Child(Vault):\n    def peek(self):\n        return self.__secret\n\n\nclass Other:\n    def peek(self, v: Vault):\n        return v.__secret\n\n\ndef outside(v: Vault, anything):\n    os.__name__\n    anything.__hidden\n    return v.__secret, Vault.__secret, v._Vault__secret\n";
        let p = run(src);
        let hits = found(src, &p, "PyPrivateMember");
        let msgs: Vec<&str> = hits.iter().map(|(_, m)| m.as_str()).collect();
        assert_eq!(
            msgs,
            vec![
                "Access to a private member __secret of a class Vault",
                "Access to a private member __secret of a class Vault",
                "Access to a private member __secret of a class Vault",
                "Access to a private member __secret of a class Vault",
            ],
            "{hits:?}"
        );
        // Protected-member reporting stays independent of name-mangled members.
        assert!(found(src, &p, "PyProtectedMember")
            .iter()
            .all(|(t, _)| t == "_Vault__secret"));
        let suppressed = "class Vault:\n    __secret = 1\n\n\ndef outside(v: Vault):\n    # noinspection PyPrivateMember\n    return v.__secret\n";
        assert!(found(suppressed, &run(suppressed), "PyPrivateMember").is_empty());
    }

    #[test]
    fn attributes_defined_outside_init() {
        let src = "class Base:\n    def __init__(self):\n        self.base_attr = 1\n\n\nclass Thing(Base):\n    declared = None\n\n    def __init__(self):\n        self.size = 0\n        self._setup()\n\n    def _setup(self):\n        self.ready = True\n\n    def grow(self):\n        self.size += 1\n        self.base_attr = 2\n        self.declared = 3\n        self.cache = {}\n        self.cache = {1: 2}\n\n    @property\n    def area(self):\n        return 1\n\n    @area.setter\n    def area(self, v):\n        self.area_value = v\n\n\nclass Container:\n    def __init__(self):\n        self._build()\n\n    def _build(self):\n        pass\n\n\nclass Group(Container):\n    def _build(self):\n        self.options = []\n";
        let p = run(src);
        let hits = found(src, &p, "PyAttributeOutsideInit");
        assert_eq!(
            hits,
            vec![
                (
                    "cache".to_string(),
                    "Instance attribute cache defined outside __init__".to_string()
                ),
                (
                    "area_value".to_string(),
                    "Instance attribute area_value defined outside __init__".to_string()
                )
            ]
        );
        let fixed = apply(src, &p, "PyAttributeOutsideInit", "Add 'self.cache = None'");
        assert!(
            fixed.contains("        self._setup()\n        self.cache = None\n\n    def _setup"),
            "{fixed}"
        );
    }

    #[test]
    fn abstract_classes_and_instantiation() {
        let src = "from abc import ABC, abstractmethod\n\n\nclass Shape(ABC):\n    @abstractmethod\n    def area(self) -> float: ...\n\n    @abstractmethod\n    def name(self) -> str: ...\n\n\nclass Square(Shape):\n    def area(self) -> float:\n        return 1.0\n\n\nclass Circle(Shape):\n    def area(self) -> float:\n        return 3.0\n\n    def name(self) -> str:\n        return 'circle'\n\n\nclass Partial(Shape, ABC):\n    pass\n\n\nclass Plain:\n    @abstractmethod\n    def f(self): ...\n\n\nShape()\nSquare()\nCircle()\nPlain()\n\n\ndef make(cls: type[Shape]):\n    return cls()\n";
        let p = run(src);
        let hits = found(src, &p, "PyAbstractClass");
        let msgs: Vec<&str> = hits.iter().map(|(_, m)| m.as_str()).collect();
        assert_eq!(
            msgs,
            vec![
                "Class Square must implement all abstract methods",
                "Cannot instantiate abstract class 'Shape' (abstract methods: 'area', 'name')",
                "Cannot instantiate abstract class 'Square' (abstract methods: 'name')"
            ],
            "{hits:?}"
        );
        let fixed = apply(src, &p, "PyAbstractClass", "Implement abstract methods");
        assert!(fixed.contains("class Square(Shape):\n    def area(self) -> float:\n        return 1.0\n\n    def name(self) -> str:\n        raise NotImplementedError\n"), "{fixed}");
    }

    #[test]
    fn coroutines_that_are_not_awaited() {
        let src = "import asyncio\n\n\nasync def fetch(url):\n    return url\n\n\nasync def stream():\n    yield 1\n\n\nclass Client:\n    async def close(self):\n        pass\n\n\nasync def main(c: Client):\n    fetch('a')\n    await fetch('b')\n    stream()\n    c.close()\n    asyncio.sleep(1)\n    asyncio.create_task(fetch('c'))\n\n\ndef sync():\n    fetch('d')\n";
        let p = run(src);
        let hits = found(src, &p, "PyAsyncCall");
        let msgs: Vec<&str> = hits.iter().map(|(_, m)| m.as_str()).collect();
        assert_eq!(
            msgs,
            vec![
                "Coroutine 'fetch' is not awaited",
                "Coroutine 'close' is not awaited",
                "Coroutine 'sleep' is not awaited"
            ],
            "{hits:?}"
        );
        let fixed = apply(src, &p, "PyAsyncCall", "Add 'await'");
        assert!(fixed.contains("    await fetch('a')\n"), "{fixed}");
    }

    #[test]
    fn unbound_locals_once_per_path_with_precise_handlers() {
        let src = "def once(flag):\n    if flag:\n        value = 1\n    print(value)\n    print(value)\n    return value\n\n\ndef counter(items):\n    try:\n        i = -1\n        for i, item in enumerate(items):\n            pass\n    except Exception:\n        return i\n    return i\n\n\ndef guarded():\n    try:\n        conn = connect()\n        data = conn.read()\n    except OSError:\n        print(data)\n    finally:\n        print(conn)\n\n\ndef quiet():\n    try:\n        x = 1\n    except Exception:\n        print(x)\n    return x\n\n\ndef comp(items):\n    result = [x * scale for x in items]\n    scale = 2\n    lazy = (y * factor for y in items)\n    factor = 3\n    return result, lazy\n\n\ndef handler_name():\n    try:\n        pass_through()\n    except ValueError as err:\n        pass\n    return err\n";
        let p = run(src);
        let reported: Vec<String> = found(src, &p, "PyUnboundLocalVariable")
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        // `value` once (a failed read raises), nothing in `counter` (`i = -1`
        // cannot raise), `data` in the handler and `conn` in `finally`, the
        // list comprehension's `scale` (not the generator's `factor`), and the
        // handler's `err`, deleted when the handler ends.
        assert_eq!(reported, vec!["value", "data", "conn", "scale", "err"]);
    }

    #[test]
    fn nested_comprehensions_read_their_own_variables() {
        // The inner set comprehension iterates the outer comprehension's
        // `batch`, not the function's (assigned later by the `for` loop).
        let src = "def match(groups):\n    synonyms = [{t.lower() for t in batch} for batch in groups]\n    for batch in synonyms:\n        print(batch)\n\n\ndef eager(items):\n    out = [[x for x in later] for _ in items]\n    later = []\n    return out\n\n\ndef lazy(items):\n    gen = ((y for y in items) for z in rest)\n    rest = []\n    return gen\n";
        let p = run(src);
        let reported: Vec<String> = found(src, &p, "PyUnboundLocalVariable")
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        // An inner list comprehension runs while the outer one does; a
        // generator's first iterable is evaluated at once.
        assert_eq!(reported, vec!["later", "rest"]);
    }

    #[test]
    fn attributes_set_on_the_instance_in_new_are_declared() {
        let src = "class Point:\n    def __new__(cls, x):\n        self = super().__new__(cls)\n        self.x = x\n        self._setup()\n        return self\n\n    def _setup(self):\n        self.ready = True\n\n    def move(self, dx):\n        self.x = self.x + dx\n        self.ready = False\n        self.speed = dx\n\n\nclass Token(str):\n    def __new__(cls, value):\n        obj = str.__new__(cls, value)\n        obj.kind = 'word'\n        return obj\n\n    def retag(self):\n        self.kind = 'tag'\n        self.extra = 1\n";
        let p = run(src);
        let reported: Vec<String> = found(src, &p, "PyAttributeOutsideInit")
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(reported, vec!["speed", "extra"]);
    }

    #[test]
    fn conditionally_imported_classes_are_not_known_to_be_abstract() {
        let db = Analysis::for_project(Path::new("/proj"));
        db.set_file_text(Path::new("/proj/fixtures.py"), "import abc\n\n\nclass FixtureRequest(abc.ABC):\n    @abc.abstractmethod\n    def node(self): ...\n\n\nclass TopRequest(FixtureRequest):\n    def node(self):\n        return 1\n");
        // `FixtureRequest` is the concrete `TopRequest` where it exists.
        let src = "import fixtures\n\ntry:\n    from fixtures import TopRequest as FixtureRequest\nexcept ImportError:\n    from fixtures import FixtureRequest\n\n\ndef make():\n    return FixtureRequest(), fixtures.FixtureRequest()\n\n\nclass Plugin(FixtureRequest):\n    pass\n\n\nclass Broken(fixtures.FixtureRequest):\n    pass\n";
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let f = db.file(id).unwrap();
        let p = inspect(&db, &f, &InspectionOptions::default());
        let hits = found(src, &p, "PyAbstractClass");
        let expected = vec![
            (
                "fixtures.FixtureRequest".to_string(),
                "Cannot instantiate abstract class 'FixtureRequest' (abstract methods: 'node')"
                    .to_string(),
            ),
            (
                "Broken".to_string(),
                "Class Broken must implement all abstract methods".to_string(),
            ),
        ];
        assert_eq!(
            hits, expected,
            "only through the unambiguous `fixtures.FixtureRequest`"
        );
    }

    #[test]
    fn sunder_names_and_special_methods_are_not_reported() {
        let src = "import enum\n\n\nclass Color(enum.Enum):\n    RED = 1\n\n\nclass Resource:\n    def __init__(self):\n        self.handle = None\n\n    def __enter__(self):\n        self.entered = True\n        return self\n\n    def reset(self):\n        self.count = 0\n\n\nprint(Color.RED._value_, Color.RED._name_)\n";
        let p = run(src);
        assert!(
            found(src, &p, "PyProtectedMember").is_empty(),
            "{:?}",
            found(src, &p, "PyProtectedMember")
        );
        let outside: Vec<String> = found(src, &p, "PyAttributeOutsideInit")
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(outside, vec!["count"]);
    }

    #[test]
    fn unresolved_modules_are_modules() {
        let src = "import not_installed\n\nnot_installed._internal()\n";
        let p = run(src);
        let msgs: Vec<String> = found(src, &p, "PyProtectedMember")
            .into_iter()
            .map(|(_, m)| m)
            .collect();
        assert_eq!(
            msgs,
            vec!["Access to a protected member _internal of a module"]
        );
    }

    #[test]
    fn concrete_stub_classes_implement_their_abstract_bases() {
        let src = "import io\nfrom collections.abc import Mapping\n\n\nclass Buffer(io.BytesIO):\n    pass\n\n\nclass Table(Mapping):\n    pass\n\n\nio.StringIO()\nBuffer()\nTable()\nMapping()\n";
        let p = run(src);
        let msgs: Vec<String> = found(src, &p, "PyAbstractClass")
            .into_iter()
            .map(|(_, m)| m)
            .collect();
        assert_eq!(msgs.len(), 3, "{msgs:?}");
        assert_eq!(msgs[0], "Class Table must implement all abstract methods");
        assert!(
            msgs[1].starts_with(
                "Cannot instantiate abstract class 'Table' (abstract methods: '__getitem__'"
            ),
            "{msgs:?}"
        );
        assert!(
            msgs[2].starts_with("Cannot instantiate abstract class 'Mapping'"),
            "{msgs:?}"
        );
    }

    #[test]
    fn abstract_metaclasses_require_resolved_abc_identity() {
        let src = "import abc\nclass ABC: pass\nclass ABCMeta(type): pass\nclass Custom(ABC):\n    @abc.abstractmethod\n    def f(self): pass\nclass Other(metaclass=ABCMeta):\n    @abc.abstractmethod\n    def f(self): pass\nCustom()\nOther()\n";
        assert!(found(src, &run(src), "PyAbstractClass").is_empty());
        let output = std::process::Command::new("python3")
            .args(["-c", src])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let src = "from abc import ABCMeta as Meta\nimport abc as a\nclass DerivedMeta(Meta): pass\nclass Real(metaclass=DerivedMeta):\n    @a.abstractmethod\n    def f(self): pass\nReal()\n";
        let hits = found(src, &run(src), "PyAbstractClass");
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert!(hits[0]
            .1
            .starts_with("Cannot instantiate abstract class 'Real'"));
        let output = std::process::Command::new("python3")
            .args(["-c", src])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("abstract"));
        let src = "from abc import ABC as Base, abstractmethod\nclass Real(Base):\n    @abstractmethod\n    def f(self): pass\nReal()\n";
        assert_eq!(found(src, &run(src), "PyAbstractClass").len(), 1);
        let src = "import abc\nclass Protocol: pass\nclass Custom(Protocol):\n    @abc.abstractmethod\n    def f(self): pass\nclass Derived(Custom): pass\nCustom()\nDerived()\n";
        assert!(found(src, &run(src), "PyAbstractClass").is_empty());
        assert!(std::process::Command::new("python3")
            .args(["-c", src])
            .status()
            .unwrap()
            .success());
        let src = "from typing import Protocol as P\nfrom abc import abstractmethod\nclass Interface(P):\n    @abstractmethod\n    def f(self): pass\nclass Derived(Interface): pass\nDerived()\n";
        let hits = found(src, &run(src), "PyAbstractClass");
        assert_eq!(hits.len(), 2, "{hits:?}");
        let output = std::process::Command::new("python3")
            .args(["-c", src])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("abstract"));
    }

    #[test]
    fn comprehension_variables_shadow_outer_names() {
        let src = "item = None\n\n\ndef first(items):\n    return [item for item in items]\n\n\ntotal = [n for n in range(3)]\nn = 0\n";
        let p = run(src);
        let hits = found(src, &p, "PyShadowingNames");
        let names: Vec<&str> = hits.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["item", "n"], "{hits:?}");
    }

    #[test]
    fn noinspection_comments_silence_statements_and_definitions() {
        let src = "import json\ndata = 1\n\n\ndef deco(f):\n    return f\n\n\n# noinspection PyShadowingNames\n@deco\ndef load(json):\n    data = 2\n    return json, data\n\n\ndef keep(json):\n    # noinspection PyShadowingNames,PyUnusedLocal\n    data = 3\n    return json, data\n\n\ndef other_code(json):\n    # noinspection PyProtectedMember\n    data = 4\n    return json, data\n\n\ndef same_line(data):  # noinspection PyShadowingNames\n    return data\n\n\n# noinspection ALL\ndef everything(data):\n    return data\n";
        let p = run(src);
        let names: Vec<String> = found(src, &p, "PyShadowingNames")
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        // The whole decorated function, one statement, and a comment for
        // another inspection that silences nothing here.
        assert_eq!(names, vec!["json", "json", "data"]);
    }

    #[test]
    fn option_disables_the_group() {
        let src = "def f(x):\n    if x:\n        y = 1\n    return y\n";
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let f = db.file(id).unwrap();
        let on = inspect(&db, &f, &InspectionOptions::default());
        assert!(on
            .iter()
            .any(|p| p.diagnostic.code == "PyUnboundLocalVariable"));
        let off = inspect(
            &db,
            &f,
            &InspectionOptions {
                probable_bugs: false,
                ..Default::default()
            },
        );
        assert!(!off
            .iter()
            .any(|p| p.diagnostic.code == "PyUnboundLocalVariable"));
    }
}
