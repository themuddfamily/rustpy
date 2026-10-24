//! Flow-sensitive name lookup (roadmap §2.2 "Narrowing", "unbound/possibly
//! unbound flow analysis"): for every load of a name in a scope, which
//! bindings of that scope can reach it, and which conditions are known to
//! hold on the way (`if`/`elif`/`while` tests, early `return`/`raise`/
//! `continue`/`break`, `assert`).
//!
//! The walk is purely syntactic; `narrow.rs` turns the result into types.

use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};
use rustpy_core::{TextRange, TextSize};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::{SyntaxKind, SyntaxNode};
use smol_str::SmolStr;

use crate::db::FileInfo;

#[path = "flow_cursor.rs"]
mod cursor;
#[path = "flow_terminal.rs"]
mod terminal;
pub use cursor::reaching_at_with_db;

#[path = "flow_query.rs"]
mod query;
pub(crate) use query::{
    capture_authority, flow_with_authority, CapturedAuthority, MAX_AUTHORITY_CACHE_BYTES,
    MAX_AUTHORITY_CACHE_FILES,
};

#[cfg(test)]
#[path = "flow_terminal_tests.rs"]
mod terminal_authority_tests;
use crate::scope::{BindingId, ScopeId, ScopeKind};

/// A condition known to be true (`positive`) or false on the way to a point.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Pred {
    /// Range of the condition expression.
    pub cond: TextRange,
    pub positive: bool,
}

/// One way a value can reach a point: a binding of the analysed scope, or
/// (`None`) the value the name has in enclosing scopes, narrowed by `preds`
/// (applied in order).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Reach {
    pub binding: Option<BindingId>,
    pub preds: Vec<Pred>,
}

/// Runtime operation phase at an expression point. A truth test is not a
/// value read; store/delete do not invoke the final target's getter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OperationKind {
    Value,
    Truth,
    Store,
    Delete,
    Inplace,
    IteratorAcquisition,
    /// Iteration needed to bind this structural target; not the RHS value read.
    Unpack,
    /// Truth conversion of the preceding result before the next comparison.
    ComparisonTruth(u16),
    /// Invocation result of one comparator after both operands are evaluated.
    ComparisonValue(u16),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OperationPoint {
    pub range: TextRange,
    pub kind: OperationKind,
}

/// Result of analysing one scope.
#[derive(Clone, Debug, Default)]
pub struct FlowInfo {
    /// Name loads (by name token start) → the values that can reach them.
    /// An empty list means the name is unbound at that point.
    pub refs: FxHashMap<TextSize, Vec<Reach>>,
    /// Loads of narrowable member references (`self.x`, `a.b.c`, `d["k"]`,
    /// `t[0]`; see [`reference_key`]) by expression range → the conditions
    /// known on each path since the reference was last written. Every reach
    /// has `binding: None`: the base type is the member's declared type.
    /// Absent when no condition narrows the reference at that point.
    pub member_refs: FxHashMap<TextRange, Vec<Reach>>,
    /// Binding targets (by name token start) → their binding.
    pub targets: FxHashMap<TextSize, BindingId>,
    /// Expression statements proven terminal under this query's authority.
    pub terminal_calls: FxHashSet<TextRange>,
    /// Operations proven terminal on every visited execution at this phase.
    pub terminal_operations: FxHashSet<OperationPoint>,
    /// Name-read points with captured ordinary exit/quit builtin provenance.
    pub builtin_refs: FxHashSet<TextSize>,
    pub contexts: FxHashMap<TextRange, ContextContract>,
    pub raise_names: FxHashMap<TextSize, SmolStr>,
    unknown_raise_points: FxHashSet<TextSize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ContextSuppression {
    #[default]
    Unknown,
    False,
    True,
    Exceptions(Vec<SmolStr>),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextContract {
    pub entry_terminal: bool,
    pub exit_terminal: bool,
    pub suppression: ContextSuppression,
    pub exit_may_raise: bool,
}
impl Default for ContextContract {
    fn default() -> Self {
        Self {
            entry_terminal: false,
            exit_terminal: false,
            suppression: ContextSuppression::Unknown,
            exit_may_raise: true,
        }
    }
}

/// Per-file cache of analysed scopes (lives as long as the file version).
#[derive(Debug, Default)]
pub struct FlowCache(parking_lot::Mutex<FxHashMap<ScopeId, Arc<FlowInfo>>>);

/// Flow information for `scope` of `file`, computed once per file version.
/// Only module, class and function scopes are analysed.
pub fn flow_of(file: &FileInfo, scope: ScopeId) -> Arc<FlowInfo> {
    let scope = walked_scope(file, scope);
    if let Some(f) = file.flow.0.lock().get(&scope) {
        return f.clone();
    }
    let info = Arc::new(analyze(file, scope, terminal::Authority::default()));
    // Cut short by a cancelled job: incomplete, so not cached.
    if crate::infer::ambient_cancelled() {
        return info;
    }
    file.flow.0.lock().entry(scope).or_insert(info).clone()
}

/// Uncached flow using immutable file/module values and checked database epoch.
/// Live and frozen databases use the same bounded dependency capture. This
/// does not populate the FileInfo-only cache with dependency-sensitive results.
/// No inference is called, so inference -> flow cannot recurse through types.
pub fn flow_of_with_db(db: &crate::db::Analysis, file: &FileInfo, scope: ScopeId) -> Arc<FlowInfo> {
    let current = || {
        db.file(file.id)
            .is_some_and(|captured| std::ptr::eq(captured.as_ref(), file))
    };
    if !current() {
        return flow_of(file, scope);
    }
    // Capture only the explicit canonical module dependencies (at most four).
    // FileInfo values are immutable; no inference or whole-database cloning.
    let authority = terminal::Authority::captured(db, file);
    let epoch = db.revision_excluding(&file.path);
    if !current() || !authority.is_current(db) || db.revision_excluding(&file.path) != epoch {
        return flow_of(file, scope);
    }
    let result = Arc::new(analyze(file, scope, authority.clone()));
    if !current() || !authority.is_current(db) || db.revision_excluding(&file.path) != epoch {
        return flow_of(file, scope);
    }
    result
}

/// Cheap authority query for a call statement; never invokes inference.
/// Uses exactly the reaching-state proof recorded by the flow walker.
/// Imported calls require checked file/module authority; the FileInfo-only
/// API remains conservative rather than guessing standard-library provenance.
pub fn call_terminates_with_db(
    db: &crate::db::Analysis,
    file: &FileInfo,
    call: &ast::Expr,
) -> bool {
    let call = call.clone().unparen();
    let Some(scope) = flow_scope(
        file,
        file.scopes.scope_at(call.syntax().text_range().start()),
    ) else {
        return false;
    };
    flow_of_with_db(db, file, scope)
        .terminal_calls
        .contains(&call.syntax().text_range())
}

/// Read the acquisition-time contract from the maintained flow result.
/// This consumer query is not called by the Walker itself.
pub fn context_contract_with_db(
    db: &crate::db::Analysis,
    file: &FileInfo,
    context: &ast::Expr,
) -> ContextContract {
    let Some(scope) = flow_scope(
        file,
        file.scopes.scope_at(context.syntax().text_range().start()),
    ) else {
        return ContextContract::default();
    };
    flow_of_with_db(db, file, scope)
        .contexts
        .get(&context.syntax().text_range())
        .cloned()
        .unwrap_or_default()
}
pub fn raised_exception_name_with_db(
    db: &crate::db::Analysis,
    file: &FileInfo,
    statement: &ast::RaiseStmt,
) -> Option<SmolStr> {
    raised_exception_name_at_with_db(db, file, statement.syntax().text_range().start())
}
pub fn raised_exception_name_at_with_db(
    db: &crate::db::Analysis,
    file: &FileInfo,
    point: TextSize,
) -> Option<SmolStr> {
    let scope = flow_scope(file, file.scopes.scope_at(point))?;
    flow_of_with_db(db, file, scope)
        .raise_names
        .get(&point)
        .cloned()
}

/// The scope whose flow analysis covers references made in `scope`:
/// comprehensions run immediately, so they use their enclosing scope.
/// `None` inside lambdas and annotation scopes.
pub fn flow_scope(file: &FileInfo, scope: ScopeId) -> Option<ScopeId> {
    let mut cur = scope;
    loop {
        let s = file.scopes.scope(cur);
        match s.kind {
            ScopeKind::Module | ScopeKind::Function | ScopeKind::Class => return Some(cur),
            ScopeKind::Comprehension => cur = s.parent?,
            ScopeKind::Lambda | ScopeKind::TypeParams => return None,
        }
    }
}

/// The scope whose walk produces the flow of `scope`: a class body is walked
/// as part of its enclosing execution scope, so every class of a module
/// shares the module's single analysis (and cache entry).
pub(crate) fn walked_scope(file: &FileInfo, scope: ScopeId) -> ScopeId {
    execution_scope(file, scope).unwrap_or(scope)
}

// Class queries retain their public namespace identity; execution is seeded
// from the enclosing eager construction context rather than an empty class.
fn execution_scope(file: &FileInfo, scope: ScopeId) -> Option<ScopeId> {
    let mut cur = scope;
    loop {
        let s = file.scopes.scope(cur);
        match s.kind {
            ScopeKind::Module | ScopeKind::Function => return Some(cur),
            ScopeKind::Class => {
                cur = s.parent?;
                while file.scopes.scope(cur).kind == ScopeKind::TypeParams {
                    cur = file.scopes.scope(cur).parent?;
                }
            }
            ScopeKind::Comprehension => cur = s.parent?,
            ScopeKind::Lambda | ScopeKind::TypeParams => return None,
        }
    }
}

/// More paths than this per name are collapsed per binding.
const MAX_REACH: usize = 8;
/// Conditions remembered per path.
const MAX_PREDS: usize = 12;
/// Loops nested deeper than this use one over-approximating pass.
const MAX_PRECISE_LOOP_DEPTH: usize = 3;

type State = FxHashMap<SmolStr, Vec<Reach>>;

struct ComprehensionReplay {
    exact: bool,
    remaining: usize,
    truncated: bool,
}

// NUL cannot occur in a Python identifier. Captured external namespaces stay
// per-path without sharing names with the current class dictionary.
fn scoped_key(scope: ScopeId, name: &str) -> SmolStr {
    SmolStr::new(format!("\0{}:{name}", scope.0))
}
fn scoped_name(key: &str) -> Option<(ScopeId, &str)> {
    let (scope, name) = key.strip_prefix('\0')?.split_once(':')?;
    Some((ScopeId(scope.parse().ok()?), name))
}

#[derive(Default)]
struct LoopCtx {
    id: TextSize,
    breaks: Vec<State>,
    continues: Vec<State>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExitReason {
    Break(TextSize),
    Continue(TextSize),
    Return,
    Raise,
    KnownRaise(TextSize),
}

struct PendingExit {
    reason: ExitReason,
    state: State,
}

fn analyze(file: &FileInfo, scope: ScopeId, authority: terminal::Authority) -> FlowInfo {
    analyze_observed(file, scope, authority, None)
}
fn analyze_observed(
    file: &FileInfo,
    scope: ScopeId,
    authority: terminal::Authority,
    cursor: Option<cursor::Observer>,
) -> FlowInfo {
    let scope = execution_scope(file, scope).unwrap_or(scope);
    let sc = file.scopes.scope(scope);
    let mut w = Walker::new(file, scope, authority);
    w.cursor = cursor;
    let root = file.parse.syntax_node();
    match sc.kind {
        ScopeKind::Module => {
            if let Some(m) = ast::Module::cast(root) {
                w.stmts(m.statements(), Some(State::default()));
            }
        }
        ScopeKind::Function => {
            let Some(f) =
                node_at(&root, sc.range, SyntaxKind::FUNC_DEF).and_then(ast::FuncDef::cast)
            else {
                return w.info;
            };
            let mut st = State::default();
            for p in f.params().into_iter().flat_map(|p| p.params()) {
                if let Some(n) = p.name() {
                    w.bind_at(&mut st, n.text_range().start());
                }
            }
            w.block(f.body(), Some(st));
        }
        ScopeKind::Class => {
            let Some(c) =
                node_at(&root, sc.range, SyntaxKind::CLASS_DEF).and_then(ast::ClassDef::cast)
            else {
                return w.info;
            };
            w.block(c.body(), Some(State::default()));
        }
        _ => {}
    }
    w.info
}

fn node_at(root: &SyntaxNode, range: TextRange, kind: SyntaxKind) -> Option<SyntaxNode> {
    let start = match root.covering_element(range) {
        rustpy_syntax::rowan::NodeOrToken::Node(n) => n,
        rustpy_syntax::rowan::NodeOrToken::Token(t) => t.parent()?,
    };
    start
        .ancestors()
        .find(|a| a.text_range() == range && a.kind() == kind)
}

struct Walker<'a> {
    cursor: Option<cursor::Observer>,
    file: &'a FileInfo,
    scope: ScopeId,
    authority: terminal::Authority,
    nonterminal_calls: FxHashSet<TextRange>,
    nonterminal_operations: FxHashSet<OperationPoint>,
    /// Private acquisition-time returned value modes: factory-await then callback-await.
    factory_results: FxHashMap<TextRange, [[bool; 2]; 2]>,
    acquired_awaited_calls: FxHashMap<TextRange, bool>,
    comparison_truth_results: FxHashMap<TextRange, bool>,
    is_class: bool,
    class_namespace_unknown: bool,
    /// Names bound in the scope (minus `global`/`nonlocal` declarations).
    tracked: FxHashSet<SmolStr>,
    /// Name token start → binding of the scope.
    by_pos: FxHashMap<TextSize, BindingId>,
    loops: Vec<LoopCtx>,
    pending: Vec<PendingExit>,
    exception_frames: Vec<Option<State>>,
    record_reads: bool,
    deferred_annotations: bool,
    /// Names bound by the comprehensions being walked (they shadow ours).
    shadowed: Vec<FxHashSet<SmolStr>>,
    info: FlowInfo,
}

impl<'a> Walker<'a> {
    fn new(file: &'a FileInfo, scope: ScopeId, authority: terminal::Authority) -> Self {
        let sc = file.scopes.scope(scope);
        let mut tracked = FxHashSet::default();
        let mut by_pos = FxHashMap::default();
        for (name, ids) in &sc.bindings {
            let external =
                sc.globals.iter().any(|g| g == name) || sc.nonlocals.iter().any(|g| g == name);
            // Global/nonlocal stores execute here too. Keep their source
            // targets but not ordinary-local tracking; stored_key projects
            // each write into this walker's private captured-cell state.
            for id in ids {
                let b = file.scopes.binding(*id);
                if b.is_definition() {
                    if !external {
                        tracked.insert(name.clone());
                    }
                    by_pos.insert(b.range.start(), *id);
                }
            }
        }
        Self {
            cursor: None,
            file,
            scope,
            authority,
            nonterminal_calls: FxHashSet::default(),
            nonterminal_operations: FxHashSet::default(),
            factory_results: FxHashMap::default(),
            acquired_awaited_calls: FxHashMap::default(),
            comparison_truth_results: FxHashMap::default(),
            is_class: sc.kind == ScopeKind::Class,
            class_namespace_unknown: false,
            tracked,
            by_pos,
            loops: Vec::new(),
            pending: Vec::new(),
            exception_frames: Vec::new(),
            record_reads: true,
            deferred_annotations: ast::Module::cast(file.parse.syntax_node()).is_some_and(
                |module| {
                    module.statements().any(|statement| match statement {
                        ast::Stmt::ImportFrom(import) => {
                            import.module_path() == "__future__"
                                && import.aliases().any(|alias| alias.name() == "annotations")
                        }
                        _ => false,
                    })
                },
            ),
            shadowed: Vec::new(),
            info: FlowInfo::default(),
        }
    }

    fn nonlocal_scope(&self, name: &str) -> Option<ScopeId> {
        let mut scope = self.file.scopes.scope(self.scope).parent;
        while let Some(id) = scope {
            let candidate = self.file.scopes.scope(id);
            if matches!(candidate.kind, ScopeKind::Function | ScopeKind::Lambda)
                && !candidate.globals.iter().any(|n| n == name)
                && !candidate.nonlocals.iter().any(|n| n == name)
                && self
                    .file
                    .scopes
                    .bindings_in(id, name)
                    .iter()
                    .any(|id| self.file.scopes.binding(*id).is_definition())
            {
                return Some(id);
            }
            scope = candidate.parent;
        }
        None
    }

    fn external_destination(&self, name: &str) -> Option<ScopeId> {
        let scope = self.file.scopes.scope(self.scope);
        if scope.globals.iter().any(|n| n == name) {
            return Some(self.file.scopes.module_scope());
        }
        if scope.nonlocals.iter().any(|n| n == name) {
            return self.nonlocal_scope(name);
        }
        None
    }

    fn lexical_destination(&self, name: &str) -> ScopeId {
        if let Some(scope) = self.external_destination(name) {
            return scope;
        }
        if self.tracked.contains(name) {
            return self.scope;
        }
        self.file
            .scopes
            .resolve(self.scope, name)
            .and_then(|ids| {
                ids.iter()
                    .find(|id| self.file.scopes.binding(**id).is_definition())
            })
            .map(|id| self.file.scopes.binding(*id).scope)
            .unwrap_or_else(|| self.file.scopes.module_scope())
    }

    fn stored_key(&self, name: &str) -> SmolStr {
        match self.external_destination(name) {
            Some(scope) if scope != self.scope => scoped_key(scope, name),
            _ => SmolStr::new(name),
        }
    }

    fn captured_lookup(&self, scope: ScopeId, name: &str, state: &State) -> Option<Vec<Reach>> {
        if scope == self.scope {
            return state.get(name).cloned();
        }
        state.get(&scoped_key(scope, name)).cloned()
    }

    pub(super) fn known_builtin_name(&self, name: &str, state: &State) -> bool {
        if !matches!(name, "exit" | "quit")
            || self.shadowed.iter().any(|names| names.contains(name))
            || self
                .file
                .scopes
                .scopes
                .iter()
                .any(|scope| !scope.star_imports.is_empty())
        {
            return false;
        }
        let global = self.file.scopes.module_scope();
        if self.is_class && self.external_destination(name).is_none() {
            if self.class_namespace_unknown
                || state
                    .get(name)
                    .is_some_and(|values| values.iter().any(|reach| reach.binding.is_some()))
            {
                return false;
            }
            // An unassigned class name may be a captured cell; an assigned
            // class name instead has LOAD_NAME globals/builtins fallback.
            if !self.tracked.contains(name) && self.lexical_destination(name) != global {
                return false;
            }
        } else if self.lexical_destination(name) != global {
            return false;
        }
        match self.captured_lookup(global, name, state) {
            Some(values) => values.is_empty(),
            None => !self
                .file
                .scopes
                .bindings_in(global, name)
                .iter()
                .any(|id| self.file.scopes.binding(*id).is_definition()),
        }
    }

    /// Some(empty) is authoritative absence, Some(unknown) cannot prove a
    /// callable contract, and None alone permits lexical/builtin fallback.
    pub(super) fn reaching_name(&self, name: &str, state: &State) -> Option<Vec<Reach>> {
        if self.is_class && self.external_destination(name).is_none() {
            if self.class_namespace_unknown {
                return Some(vec![Reach {
                    binding: None,
                    preds: Vec::new(),
                }]);
            }
            if self.tracked.contains(name) {
                // LOAD_NAME of a class-assigned name falls back to globals,
                // not a same-named enclosing function cell.
                let global = self.file.scopes.module_scope();
                let fallback = self.captured_lookup(global, name, state).or_else(|| {
                    let values: Vec<_> = self
                        .file
                        .scopes
                        .bindings_in(global, name)
                        .iter()
                        .filter(|id| self.file.scopes.binding(**id).is_definition())
                        .map(|id| Reach {
                            binding: Some(*id),
                            preds: Vec::new(),
                        })
                        .collect();
                    (!values.is_empty()).then_some(values)
                });
                let own = state.get(name);
                let mut values: Vec<Reach> = own
                    .into_iter()
                    .flatten()
                    .filter(|r| r.binding.is_some())
                    .cloned()
                    .collect();
                if own.is_none_or(|r| r.is_empty() || r.iter().any(|r| r.binding.is_none())) {
                    values.extend(fallback.unwrap_or_else(|| {
                        vec![Reach {
                            binding: None,
                            preds: Vec::new(),
                        }]
                    }));
                }
                compact(&mut values);
                return Some(values);
            }
        }
        let destination = self.lexical_destination(name);
        if destination == self.scope && self.tracked.contains(name) {
            return Some(state.get(name).cloned().unwrap_or_default());
        }
        self.captured_lookup(destination, name, state)
    }

    fn project_class_state(&self, before: &State, class: &State) -> State {
        let mut projected = before.clone();
        for (key, values) in class {
            if let Some((scope, name)) = scoped_name(key) {
                let destination = if scope == self.scope {
                    SmolStr::new(name)
                } else {
                    key.clone()
                };
                projected.insert(destination, values.clone());
            }
        }
        projected
    }

    fn class_body(&mut self, class: &ast::ClassDef, before: State) -> Option<State> {
        let scope = self
            .file
            .scopes
            .scope_for_node(class.syntax().text_range())?;
        let mut body = Walker::new(self.file, scope, self.authority.clone());
        body.deferred_annotations = self.deferred_annotations;
        body.record_reads = self.record_reads;
        body.cursor = self.cursor.clone();
        body.class_namespace_unknown = class
            .arguments()
            .is_some_and(|arguments| arguments.args().next().is_some());
        let mut input = State::default();
        for (key, values) in &before {
            let key = if scoped_name(key).is_some() {
                key.clone()
            } else {
                scoped_key(self.scope, key)
            };
            input.insert(key, values.clone());
        }
        for name in &self.tracked {
            input.entry(scoped_key(self.scope, name)).or_default();
        }
        // Every abrupt class-body completion leaves as a raise. Outside any
        // enclosing try/with frame nothing can observe those raises (each try
        // or with takes the pending exits on entry), so skip collecting them:
        // merging the whole projected namespace once per class statement is
        // quadratic in the number of classes of a module.
        let observed = !self.exception_frames.is_empty();
        if observed {
            body.exception_frames.push(None);
        }
        let normal = body.block(class.body(), Some(input));
        if let Some(failed) = observed
            .then(|| body.exception_frames.pop().flatten())
            .flatten()
        {
            body.emit_exit(ExitReason::Raise, failed);
        }
        self.info
            .unknown_raise_points
            .extend(body.info.unknown_raise_points.iter().copied());
        for (point, name) in &body.info.raise_names {
            if !self.info.unknown_raise_points.contains(point) {
                self.info.raise_names.insert(*point, name.clone());
            }
        }
        for point in &self.info.unknown_raise_points {
            self.info.raise_names.remove(point);
        }
        for exit in body.pending.iter().filter(|_| observed) {
            let state = self.project_class_state(&before, &exit.state);
            self.exception_point(&state);
            self.emit_exit(
                if is_raise(exit.reason) {
                    exit.reason
                } else {
                    ExitReason::Raise
                },
                state,
            );
        }
        if self.record_reads {
            for (offset, values) in body.info.refs {
                let existing = self.info.refs.entry(offset).or_default();
                for value in values {
                    if !existing.contains(&value) {
                        existing.push(value);
                    }
                }
                compact(existing);
            }
            for offset in body.info.builtin_refs {
                if !self.info.refs.contains_key(&offset) {
                    self.info.builtin_refs.insert(offset);
                }
            }
            for offset in self.info.refs.keys() {
                self.info.builtin_refs.remove(offset);
            }
            for (point, contract) in body.info.contexts {
                self.record_context(point, contract);
            }
            self.info.targets.extend(body.info.targets);
            self.info.member_refs.extend(body.info.member_refs);
            self.nonterminal_operations
                .extend(body.nonterminal_operations);
            self.info
                .terminal_operations
                .extend(body.info.terminal_operations);
            self.info
                .terminal_operations
                .retain(|point| !self.nonterminal_operations.contains(point));
            self.nonterminal_calls.extend(body.nonterminal_calls);
            self.info.terminal_calls.extend(body.info.terminal_calls);
            self.info
                .terminal_calls
                .retain(|range| !self.nonterminal_calls.contains(range));
        }
        normal.map(|state| self.project_class_state(&before, &state))
    }

    // ----- state helpers -------------------------------------------------------

    /// Binds the binding whose name token starts at `start` (if it belongs to the scope).
    fn bind_at(&mut self, st: &mut State, start: TextSize) {
        let Some(&id) = self.by_pos.get(&start) else {
            return;
        };
        let name = &self.file.scopes.binding(id).name;
        // Rebinding `self` retires every narrowed `self.…` reference.
        invalidate_references(st, name);
        if !self.tracked.contains(name) && self.external_destination(name).is_none() {
            return;
        }
        self.info.targets.insert(start, id);
        st.insert(
            self.stored_key(name),
            vec![Reach {
                binding: Some(id),
                preds: Vec::new(),
            }],
        );
    }

    fn record(&mut self, st: &State, name: &str, start: TextSize) {
        // Quiet finalizer passes still need captured RHS provenance for alias
        // proofs. Their FlowInfo is private and discarded by the caller.
        if self.shadowed.iter().any(|s| s.contains(name)) {
            return;
        }
        // Class bodies are invisible from inside comprehensions.
        if self.is_class && !self.shadowed.is_empty() {
            return;
        }
        if self.known_builtin_name(name, st) {
            if !self.info.refs.contains_key(&start) {
                self.info.builtin_refs.insert(start);
            }
            return;
        }
        if let Some(values) = self.reaching_name(name, st) {
            self.info.builtin_refs.remove(&start);
            self.info.refs.insert(start, values);
        } else if let Some(values) = st.get(name) {
            self.info.refs.insert(start, values.clone());
        }
    }

    /// Records the conditions reaching a load of a narrowable member
    /// reference. Nothing is recorded when the reference is not narrowed.
    fn record_reference(&mut self, st: &State, expression: &ast::Expr) {
        let Some(reference) = reference_key(expression).filter(|key| is_member_key(key)) else {
            return;
        };
        if self.reference_shadowed(&reference) {
            return;
        }
        let range = expression.syntax().text_range();
        match st.get(member_key(&reference).as_str()) {
            Some(values) => {
                self.info.member_refs.insert(range, values.clone());
            }
            None => {
                self.info.member_refs.remove(&range);
            }
        }
    }

    /// A reference rooted at a name the walked comprehensions rebind.
    fn reference_shadowed(&self, reference: &str) -> bool {
        let root = reference_root(reference);
        self.shadowed.iter().any(|names| names.contains(root))
    }

    /// A store/delete of a member reference retires its narrowing and that
    /// of every reference below it (`self.a = …` also retires `self.a.b`).
    /// A plain assignment narrows the reference to the assigned value
    /// (`value`, applied by `narrow.rs`), like pyright's assignment narrowing.
    fn write_reference(&self, st: &mut State, target: &ast::Expr, value: Option<&ast::Expr>) {
        retire_stored_references(st, target);
        let Some(reference) = reference_key(target).filter(|key| is_member_key(key)) else {
            return;
        };
        if let Some(value) = value {
            if !self.reference_shadowed(&reference) {
                st.insert(
                    member_key(&reference),
                    vec![Reach {
                        binding: None,
                        preds: vec![Pred {
                            cond: value.syntax().text_range(),
                            positive: true,
                        }],
                    }],
                );
            }
        }
    }

    /// Records that `cond` evaluated to `positive` for every name it can narrow.
    fn narrow(&self, st: &mut State, cond: &ast::Expr, positive: bool) {
        let pred = Pred {
            cond: cond.text_range(),
            positive,
        };
        let mut names = Vec::new();
        subjects(cond, &mut names, 0);
        names.dedup();
        for name in names {
            if let Some(reference) = name.strip_prefix(MEMBER_MARK) {
                if self.reference_shadowed(reference) {
                    continue;
                }
                let rs = st.entry(name.clone()).or_insert_with(|| {
                    vec![Reach {
                        binding: None,
                        preds: Vec::new(),
                    }]
                });
                for r in rs.iter_mut() {
                    push_pred(r, pred);
                }
                compact(rs);
                continue;
            }
            if self.is_class {
                if self.class_namespace_unknown && self.external_destination(&name).is_none() {
                    continue;
                }
                let destination = if self.tracked.contains(&name)
                    && st
                        .get(&name)
                        .is_some_and(|values| values.iter().any(|r| r.binding.is_some()))
                {
                    self.scope
                } else if self.tracked.contains(&name) && self.external_destination(&name).is_none()
                {
                    self.file.scopes.module_scope()
                } else {
                    self.lexical_destination(&name)
                };
                let key = if destination == self.scope {
                    SmolStr::new(&name)
                } else {
                    scoped_key(destination, &name)
                };
                let values = self.reaching_name(&name, st).unwrap_or_else(|| {
                    vec![Reach {
                        binding: None,
                        preds: Vec::new(),
                    }]
                });
                let values = st.entry(key).or_insert(values);
                for reach in values.iter_mut() {
                    push_pred(reach, pred);
                }
                compact(values);
                continue;
            }
            if self.tracked.contains(&name) {
                if let Some(rs) = st.get_mut(&name) {
                    for r in rs.iter_mut() {
                        push_pred(r, pred);
                    }
                    compact(rs);
                }
            } else {
                let rs = st.entry(name).or_insert_with(|| {
                    vec![Reach {
                        binding: None,
                        preds: Vec::new(),
                    }]
                });
                for r in rs.iter_mut() {
                    push_pred(r, pred);
                }
                compact(rs);
            }
        }
    }

    /// Joins the states of paths meeting at one point (`None`: unreachable).
    fn merge(&self, states: Vec<Option<State>>) -> Option<State> {
        let mut live: Vec<State> = states.into_iter().flatten().collect();
        match live.len() {
            0 => return None,
            1 => return live.pop(),
            _ => {}
        }
        let mut keys: Vec<SmolStr> = Vec::new();
        let mut seen: FxHashSet<SmolStr> = FxHashSet::default();
        for s in &live {
            for k in s.keys() {
                if seen.insert(k.clone()) {
                    keys.push(k.clone());
                }
            }
        }
        let plain = Reach {
            binding: None,
            preds: Vec::new(),
        };
        let mut out = State::default();
        for k in keys {
            let tracked = self.tracked.contains(&k);
            let mut rs: Vec<Reach> = Vec::new();
            for s in &live {
                match s.get(&k) {
                    Some(v) => {
                        for r in v {
                            if !rs.contains(r) {
                                rs.push(r.clone());
                            }
                        }
                    }
                    // An enclosing-scope name without conditions on this path.
                    None if !rs.contains(&plain) => rs.push(plain.clone()),
                    None => {}
                }
            }
            if !rs.is_empty()
                && live
                    .iter()
                    .any(|state| state.get(&k).is_some_and(Vec::is_empty))
                && !rs.contains(&plain)
            {
                rs.push(plain.clone());
            }
            compact(&mut rs);
            if !tracked && rs.len() == 1 && rs[0] == plain {
                continue;
            }
            out.insert(k, rs);
        }
        Some(out)
    }

    /// Adds every binding made inside `range` as a possible value (used where
    /// precise ordering is unknown: deeply nested loops).
    fn with_bindings_in(&self, mut st: State, range: TextRange) -> State {
        // Writes inside `range` are not ordered here: keep no member narrowing.
        st.retain(|key, _| !key.starts_with(MEMBER_MARK));
        for (pos, id) in &self.by_pos {
            if !range.contains(*pos) {
                continue;
            }
            let name = &self.file.scopes.binding(*id).name;
            if !self.tracked.contains(name)
                && !(self.is_class && self.external_destination(name).is_some())
            {
                continue;
            }
            let key = self.stored_key(name);
            let r = Reach {
                binding: Some(*id),
                preds: Vec::new(),
            };
            let rs = st.entry(key).or_insert_with(|| {
                if self.is_class {
                    vec![Reach {
                        binding: None,
                        preds: Vec::new(),
                    }]
                } else {
                    Vec::new()
                }
            });
            if !rs.contains(&r) {
                rs.push(r);
            }
            compact(rs);
        }
        st
    }

    fn merge_exit(&self, exits: &mut Vec<PendingExit>, reason: ExitReason, state: State) {
        if let Some(existing) = exits.iter_mut().find(|exit| exit.reason == reason) {
            existing.state = self
                .merge(vec![Some(existing.state.clone()), Some(state)])
                .expect("at least one reachable completion");
        } else {
            exits.push(PendingExit { reason, state });
        }
    }

    fn emit_exit(&mut self, reason: ExitReason, state: State) {
        let mut exits = std::mem::take(&mut self.pending);
        self.merge_exit(&mut exits, reason, state);
        self.pending = exits;
    }

    fn loop_exits(&mut self, id: TextSize) -> LoopCtx {
        let mut out = LoopCtx {
            id,
            ..LoopCtx::default()
        };
        let pending = std::mem::take(&mut self.pending);
        for exit in pending {
            match exit.reason {
                ExitReason::Break(target) if target == id => out.breaks.push(exit.state),
                ExitReason::Continue(target) if target == id => out.continues.push(exit.state),
                _ => self.emit_exit(exit.reason, exit.state),
            }
        }
        out
    }

    fn exception_point(&mut self, state: &State) {
        if let Some(index) = self.exception_frames.len().checked_sub(1) {
            let old = self.exception_frames[index].take();
            self.exception_frames[index] = self.merge(vec![old, Some(state.clone())]);
        }
    }

    // ----- statements ------------------------------------------------------------

    fn block(&mut self, block: Option<ast::Block>, st: Option<State>) -> Option<State> {
        match block {
            Some(b) => self.stmts(b.statements(), st),
            None => st,
        }
    }

    fn stmts(
        &mut self,
        stmts: impl Iterator<Item = ast::Stmt>,
        st: Option<State>,
    ) -> Option<State> {
        let mut cur = st;
        for s in stmts {
            // Code after `return`/`raise`/… is not analysed.
            let state = cur?;
            // A cancelled background job stops here: the rest of the scope
            // is treated as unreachable (and the result is never cached).
            if crate::infer::ambient_cancelled() {
                return None;
            }
            cur = self.stmt(&s, state);
        }
        cur
    }

    fn stmt(&mut self, s: &ast::Stmt, mut st: State) -> Option<State> {
        if !matches!(s, ast::Stmt::Raise(raise) if raise.exception().is_some_and(|expression| self.builtin_exception_name(&expression, &st).is_some()))
            && statement_may_raise(
                s,
                self.file.scopes.scope(self.scope).kind == ScopeKind::Function,
            )
        {
            self.exception_point(&st);
        }
        if let Some(observer) = &self.cursor {
            observer.observe(self, &st, s.syntax());
        }
        match s {
            ast::Stmt::Expr(e) => {
                let x = e.expr()?.unparen();
                self.visit(x.syntax(), &mut st)?;
                Some(st)
            }
            ast::Stmt::Assign(a) => {
                if let Some(v) = a.value() {
                    self.visit(v.syntax(), &mut st)?;
                }
                let unpack = a
                    .value()
                    .map(|value| self.protocol_unpack_contract(&value, &st));
                for t in a.targets() {
                    if let Some(contract) = unpack {
                        let terminal = self.protocol_unpack_terminal(contract, &t);
                        self.complete_operation(&t, OperationKind::Unpack, terminal, &mut st)?;
                    }
                    self.target(&t, &mut st)?;
                    self.write_reference(&mut st, &t, a.value().as_ref());
                }
                Some(st)
            }
            ast::Stmt::AugAssign(a) => {
                // The target is read before it is written.
                if let Some(t) = a.target() {
                    self.visit(t.syntax(), &mut st)?;
                }
                let captured_target = st.clone();
                if let Some(v) = a.value() {
                    self.visit(v.syntax(), &mut st)?;
                }
                if let (Some(target), Some(operator)) = (a.target(), a.op()) {
                    let terminal =
                        self.implicit_inplace_terminal(&target, operator.kind(), &captured_target);
                    self.complete_operation(&target, OperationKind::Inplace, terminal, &mut st)?;
                }
                if let Some(ast::Expr::Name(n)) = a.target() {
                    if let Some(tok) = n.name_token() {
                        self.bind_at(&mut st, tok.text_range().start());
                    }
                }
                if let Some(target) = a.target() {
                    self.write_reference(&mut st, &target, None);
                }
                Some(st)
            }
            ast::Stmt::AnnAssign(a) => {
                if let Some(v) = a.value() {
                    self.visit(v.syntax(), &mut st)?;
                }
                if let Some(t) = a.target() {
                    if a.value().is_some() {
                        self.target(&t, &mut st)?;
                        self.write_reference(&mut st, &t, a.value().as_ref());
                    } else if !matches!(t.clone().unparen(), ast::Expr::Name(_)) {
                        // Annotation-only declarations do not store a value.
                        // Expression targets still evaluate their object/index.
                        self.target_operands(&t, &mut st)?;
                    }
                }
                if !self.deferred_annotations
                    && self.file.scopes.scope(self.scope).kind != ScopeKind::Function
                {
                    if let Some(ann) = a.annotation() {
                        self.visit(ann.syntax(), &mut st)?;
                    }
                }
                Some(st)
            }
            ast::Stmt::Return(r) => {
                if let Some(v) = r.value() {
                    self.visit(v.syntax(), &mut st)?;
                }
                self.emit_exit(ExitReason::Return, st);
                None
            }
            ast::Stmt::Raise(r) => {
                for e in [r.exception(), r.cause()].into_iter().flatten() {
                    self.visit(e.syntax(), &mut st)?;
                }
                let reason = if let Some(name) = r
                    .exception()
                    .and_then(|expression| self.builtin_exception_name(&expression, &st))
                {
                    let point = r.syntax().text_range().start();
                    if !self.info.unknown_raise_points.contains(&point) {
                        self.info.raise_names.insert(point, name);
                    }
                    ExitReason::KnownRaise(point)
                } else {
                    let point = r.syntax().text_range().start();
                    self.info.unknown_raise_points.insert(point);
                    self.info.raise_names.remove(&point);
                    ExitReason::Raise
                };
                self.emit_exit(reason, st);
                None
            }
            ast::Stmt::Pass(_) | ast::Stmt::Global(_) | ast::Stmt::Nonlocal(_) => Some(st),
            ast::Stmt::Break(_) => {
                if let Some(loop_id) = self.loops.last().map(|l| l.id) {
                    self.emit_exit(ExitReason::Break(loop_id), st);
                }
                None
            }
            ast::Stmt::Continue(_) => {
                if let Some(loop_id) = self.loops.last().map(|l| l.id) {
                    self.emit_exit(ExitReason::Continue(loop_id), st);
                }
                None
            }
            ast::Stmt::Del(d) => {
                for t in d.targets() {
                    self.del_target(&t, &mut st)?;
                }
                Some(st)
            }
            ast::Stmt::Assert(a) => {
                let test = a.test();
                if let Some(t) = &test {
                    self.visit_truth(t, &mut st)?;
                }
                match test {
                    Some(t) if is_constant(&t) == Some(false) => {
                        if let Some(m) = a.message() {
                            self.visit(m.syntax(), &mut st)?;
                        }
                        self.emit_exit(ExitReason::Raise, st);
                        None
                    }
                    Some(t) => {
                        if is_constant(&t).is_none() {
                            let mut failed = st.clone();
                            if let Some(m) = a.message() {
                                let _ = self.visit(m.syntax(), &mut failed);
                            }
                            self.emit_exit(ExitReason::Raise, failed);
                        }
                        self.narrow(&mut st, &t, true);
                        Some(st)
                    }
                    None => Some(st),
                }
            }
            ast::Stmt::Import(i) => {
                for alias in i.aliases() {
                    if let Some(tok) = alias.alias().or_else(|| alias.name_token()) {
                        self.bind_at(&mut st, tok.text_range().start());
                    }
                }
                Some(st)
            }
            ast::Stmt::ImportFrom(f) => {
                for alias in f.aliases() {
                    if let Some(tok) = alias.alias().or_else(|| alias.name_token()) {
                        self.bind_at(&mut st, tok.text_range().start());
                    }
                }
                Some(st)
            }
            ast::Stmt::If(i) => self.if_stmt(i, st),
            ast::Stmt::While(w) => self.while_stmt(w, st),
            ast::Stmt::For(f) => self.for_stmt(f, st),
            ast::Stmt::Try(t) => self.try_stmt(t, st),
            ast::Stmt::With(w) => self.with_stmt(w, st),
            ast::Stmt::FuncDef(f) => {
                // Decorator expressions execute first; application happens only
                // after defaults and eager annotations finish successfully.
                let mut decorators = Vec::new();
                for decorator in f.decorators() {
                    if let Some(expression) = decorator.expr() {
                        self.visit(expression.syntax(), &mut st)?;
                        decorators.push(self.is_terminal_callable(&expression, &st));
                    }
                }
                let type_names: FxHashSet<SmolStr> = f
                    .type_params()
                    .into_iter()
                    .flat_map(|params| params.names().collect::<Vec<_>>())
                    .map(|name| SmolStr::new(name.text()))
                    .collect();
                self.shadowed.push(type_names);
                let header = (|| {
                    for parameter in f.params().into_iter().flat_map(|params| params.params()) {
                        if let Some(default) = parameter.default() {
                            self.visit(default.syntax(), &mut st)?;
                        }
                    }
                    if !self.deferred_annotations {
                        for parameter in f.params().into_iter().flat_map(|params| params.params()) {
                            if let Some(annotation) = parameter.annotation() {
                                self.visit(annotation.syntax(), &mut st)?;
                            }
                        }
                        if let Some(annotation) = f.return_annotation() {
                            self.visit(annotation.syntax(), &mut st)?;
                        }
                    }
                    Some(())
                })();
                self.shadowed.pop();
                header?;
                for terminal in decorators.into_iter().rev() {
                    if terminal {
                        self.emit_exit(ExitReason::Raise, st);
                        return None;
                    }
                }
                if let Some(name) = f.name() {
                    self.bind_at(&mut st, name.text_range().start());
                }
                Some(st)
            }
            ast::Stmt::ClassDef(c) => {
                let mut decorators = Vec::new();
                for d in c.decorators() {
                    if let Some(e) = d.expr() {
                        self.visit(e.syntax(), &mut st)?;
                        decorators.push(self.is_terminal_callable(&e, &st));
                    }
                }
                let type_names: FxHashSet<SmolStr> = c
                    .type_params()
                    .into_iter()
                    .flat_map(|params| params.names().collect::<Vec<_>>())
                    .map(|name| SmolStr::new(name.text()))
                    .collect();
                self.shadowed.push(type_names);
                let bases = match c.arguments() {
                    Some(arguments) => self.visit(arguments.syntax(), &mut st),
                    None => Some(()),
                };
                self.shadowed.pop();
                bases?;
                st = self.class_body(c, st)?;
                for terminal in decorators.into_iter().rev() {
                    if terminal {
                        self.emit_exit(ExitReason::Raise, st);
                        return None;
                    }
                }
                if let Some(n) = c.name() {
                    self.bind_at(&mut st, n.text_range().start());
                }
                Some(st)
            }
            ast::Stmt::TypeAlias(t) => {
                if let Some(n) = t.name() {
                    self.bind_at(&mut st, n.text_range().start());
                }
                Some(st)
            }
            ast::Stmt::Match(m) => self.match_stmt(m, st),
        }
    }

    fn complete_operation(
        &mut self,
        expression: &ast::Expr,
        kind: OperationKind,
        terminal: bool,
        state: &mut State,
    ) -> Option<()> {
        self.record_operation(expression, kind, terminal);
        self.complete_implicit(terminal, state)
    }
    fn complete_implicit(&mut self, terminal: bool, state: &mut State) -> Option<()> {
        if terminal {
            self.exception_point(state);
            self.emit_exit(ExitReason::Raise, state.clone());
            None
        } else {
            Some(())
        }
    }
    fn protocol_truth_terminal(&self, expression: &ast::Expr, state: &State) -> bool {
        self.implicit_truth_terminal(expression, state)
            || self
                .comparison_truth_results
                .get(&expression.clone().unparen().text_range())
                .copied()
                .unwrap_or(false)
    }
    fn complete_truth(&mut self, expression: &ast::Expr, state: &mut State) -> Option<()> {
        self.complete_operation(
            expression,
            OperationKind::Truth,
            self.protocol_truth_terminal(expression, state),
            state,
        )
    }
    fn visit_truth(&mut self, expression: &ast::Expr, state: &mut State) -> Option<()> {
        self.visit(expression.syntax(), state)?;
        self.complete_truth(expression, state)
    }
    fn target_operands(
        &mut self,
        target: &ast::Expr,
        state: &mut State,
    ) -> Option<Option<(ast::Expr, State)>> {
        match target.clone().unparen() {
            ast::Expr::Attribute(attribute) => {
                let object = attribute.object()?;
                self.visit(object.syntax(), state)?;
                Some(Some((object, state.clone())))
            }
            ast::Expr::Subscript(subscript) => {
                let object = subscript.object()?;
                self.visit(object.syntax(), state)?;
                let captured = state.clone();
                if let Some(index) = subscript.index() {
                    self.visit(&index, state)?;
                }
                Some(Some((object, captured)))
            }
            ast::Expr::Name(_) => Some(None),
            expression => {
                self.visit(expression.syntax(), state)?;
                Some(None)
            }
        }
    }

    fn target(&mut self, t: &ast::Expr, st: &mut State) -> Option<()> {
        match t {
            ast::Expr::Name(n) => {
                if let Some(tok) = n.name_token() {
                    self.bind_at(st, tok.text_range().start());
                }
            }
            ast::Expr::Tuple(x) => {
                for e in x.elements() {
                    self.target(&e, st)?;
                }
            }
            ast::Expr::List(x) => {
                for e in x.elements() {
                    self.target(&e, st)?;
                }
            }
            ast::Expr::Paren(p) => {
                if let Some(i) = p.inner() {
                    self.target(&i, st)?;
                }
            }
            ast::Expr::Star(s) => {
                if let Some(i) = s.expr() {
                    self.target(&i, st)?;
                }
            }
            // Store evaluates object/index, never the item's/getter's READ.
            other => {
                let terminal = self.target_operands(other, st)?;
                retire_stored_references(st, other);
                let slot = match other.clone().unparen() {
                    ast::Expr::Attribute(_) => "__setattr__",
                    ast::Expr::Subscript(_) => "__setitem__",
                    _ => "",
                };
                self.complete_operation(
                    other,
                    OperationKind::Store,
                    terminal.is_some_and(|(receiver, captured)| {
                        self.implicit_slot_terminal(&receiver, slot, &captured)
                    }),
                    st,
                )?;
            }
        }
        Some(())
    }

    fn del_target(&mut self, t: &ast::Expr, st: &mut State) -> Option<()> {
        match t {
            ast::Expr::Name(n) => {
                if let Some(tok) = n.name_token() {
                    let key = self.stored_key(tok.text());
                    if self.is_class
                        && !self.class_namespace_unknown
                        && self.external_destination(tok.text()).is_none()
                        && st.get(&key).is_none_or(|values| values.is_empty())
                    {
                        self.exception_point(st);
                        self.emit_exit(ExitReason::Raise, st.clone());
                        return None;
                    }
                    if self.tracked.contains(tok.text()) {
                        st.insert(key, Vec::new());
                    } else if self.is_class && self.external_destination(tok.text()).is_some() {
                        st.insert(self.stored_key(tok.text()), Vec::new());
                    } else {
                        st.remove(tok.text());
                    }
                }
            }
            ast::Expr::Tuple(x) => {
                for e in x.elements() {
                    self.del_target(&e, st)?;
                }
            }
            ast::Expr::List(x) => {
                for e in x.elements() {
                    self.del_target(&e, st)?;
                }
            }
            ast::Expr::Paren(p) => {
                if let Some(i) = p.inner() {
                    self.del_target(&i, st)?;
                }
            }
            other => {
                let receiver = self.target_operands(other, st)?;
                retire_stored_references(st, other);
                let slot = match other.clone().unparen() {
                    ast::Expr::Attribute(_) => "__delattr__",
                    ast::Expr::Subscript(_) => "__delitem__",
                    _ => "",
                };
                self.complete_operation(
                    other,
                    OperationKind::Delete,
                    receiver.is_some_and(|(receiver, captured)| {
                        self.implicit_slot_terminal(&receiver, slot, &captured)
                    }),
                    st,
                )?;
            }
        }
        Some(())
    }

    fn if_stmt(&mut self, i: &ast::IfStmt, st: State) -> Option<State> {
        let mut ends = Vec::new();
        let mut rest = Some(st);
        let arms = std::iter::once((i.condition(), i.body()))
            .chain(i.elif_clauses().map(|e| (e.condition(), e.body())));
        for (condition, body) in arms {
            let Some(mut incoming) = rest.take() else {
                break;
            };
            if let Some(c) = &condition {
                if is_constant(c).is_none() {
                    self.exception_point(&incoming);
                }
                if self.visit_truth(c, &mut incoming).is_none() {
                    break;
                }
            }
            let constant = condition.as_ref().and_then(is_constant);
            if constant != Some(false) {
                let mut branch = incoming.clone();
                if let Some(c) = &condition {
                    self.narrow(&mut branch, c, true);
                }
                ends.push(self.block(body, Some(branch)));
            }
            if constant != Some(true) {
                if let Some(c) = &condition {
                    self.narrow(&mut incoming, c, false);
                }
                rest = Some(incoming);
            }
        }
        ends.push(match i.else_clause() {
            Some(e) => self.block(e.body(), rest),
            None => rest,
        });
        self.merge(ends)
    }

    fn while_stmt(&mut self, w: &ast::WhileStmt, st: State) -> Option<State> {
        let cond = w.condition();
        let truth = cond.as_ref().and_then(is_constant);
        if truth == Some(false) {
            let mut exit = st;
            if let Some(c) = &cond {
                self.visit_truth(c, &mut exit)?;
                self.narrow(&mut exit, c, false);
            }
            return match w.else_clause() {
                Some(e) => self.block(e.body(), Some(exit)),
                None => Some(exit),
            };
        }
        let infinite = truth == Some(true);
        let precise = self.loops.len() < MAX_PRECISE_LOOP_DEPTH;
        let passes = if precise { 2 } else { 1 };
        let mut head = if precise {
            st.clone()
        } else {
            self.with_bindings_in(st.clone(), w.syntax().text_range())
        };
        let mut cond_state = head.clone();
        let mut last = LoopCtx::default();
        for pass in 0..passes {
            let mut h = head.clone();
            if let Some(c) = &cond {
                if is_constant(c).is_none() {
                    self.exception_point(&h);
                }
                if self.visit_truth(c, &mut h).is_none() {
                    return self.merge(last.breaks.into_iter().map(Some).collect());
                }
            }
            let mut body_in = h.clone();
            if let Some(c) = &cond {
                self.narrow(&mut body_in, c, true);
            }
            let id = w.syntax().text_range().start();
            self.loops.push(LoopCtx {
                id,
                ..LoopCtx::default()
            });
            let end = self.block(w.body(), Some(body_in));
            self.loops.pop();
            let mut ctx = self.loop_exits(id);
            ctx.breaks.extend(std::mem::take(&mut last.breaks));
            cond_state = h;
            if pass + 1 < passes {
                let mut all = vec![Some(st.clone()), end];
                all.extend(ctx.continues.into_iter().map(Some));
                head = self.merge(all).unwrap_or_else(|| st.clone());
                last.breaks = ctx.breaks;
            } else {
                last = ctx;
            }
        }
        // The loop ends normally when its condition is false.
        let exit = if infinite {
            None
        } else {
            let mut e = cond_state;
            if let Some(c) = &cond {
                self.narrow(&mut e, c, false);
            }
            Some(e)
        };
        let after = match w.else_clause() {
            Some(e) => self.block(e.body(), exit),
            None => exit,
        };
        let mut ends = vec![after];
        ends.extend(last.breaks.into_iter().map(Some));
        self.merge(ends)
    }

    fn builtin_exception_name(&self, expression: &ast::Expr, state: &State) -> Option<SmolStr> {
        let expression = expression.clone().unparen();
        let name = match expression {
            ast::Expr::Name(name) => name.name_text(),
            ast::Expr::Call(call) => match call.callee()?.unparen() {
                ast::Expr::Name(name) => name.name_text(),
                _ => return None,
            },
            _ => return None,
        };
        if !matches!(
            name.as_str(),
            "BaseException"
                | "Exception"
                | "ValueError"
                | "RuntimeError"
                | "TypeError"
                | "KeyError"
                | "IndexError"
                | "LookupError"
                | "AssertionError"
                | "OSError"
                | "MemoryError"
                | "StopIteration"
                | "SystemExit"
                | "KeyboardInterrupt"
                | "GeneratorExit"
        ) || self
            .shadowed
            .iter()
            .any(|names| names.contains(name.as_str()))
            || self.class_namespace_unknown
            || self.reaching_name(&name, state).is_some()
            || self.file.scopes.resolve(self.scope, &name).is_some()
        {
            return None;
        }
        Some(name.into())
    }

    fn record_context(&mut self, point: TextRange, contract: ContextContract) {
        self.info
            .contexts
            .entry(point)
            .and_modify(|previous| {
                if *previous != contract {
                    *previous = ContextContract::default();
                }
            })
            .or_insert(contract);
    }
    fn with_stmt(&mut self, statement: &ast::WithStmt, state: State) -> Option<State> {
        let outside = std::mem::take(&mut self.pending);
        let mut entered: Vec<ContextContract> = Vec::new();
        let mut paths: Vec<(Option<ExitReason>, State, Vec<ContextContract>)> = Vec::new();
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
            self.exception_frames.push(None);
            let evaluated = self.visit(context.syntax(), &mut current).is_some();
            if let Some(failed) = self.exception_frames.pop().flatten() {
                paths.push((Some(ExitReason::Raise), failed, evaluation_prefix.clone()));
            }
            for exit in std::mem::take(&mut self.pending) {
                paths.push((Some(exit.reason), exit.state, evaluation_prefix.clone()));
            }
            if !evaluated {
                break;
            }
            let contract = self.context_contract(&context, &current, statement.is_async());
            if self.record_reads {
                self.record_context(context.syntax().text_range(), contract.clone());
            }
            // Enter failure belongs to the exact previous acquired prefix.
            paths.push((Some(ExitReason::Raise), current.clone(), entered.clone()));
            if contract.entry_terminal {
                break;
            }
            entered.push(contract);
            self.exception_frames.push(None);
            let target_completed = item
                .target()
                .is_none_or(|target| self.target(&target, &mut current).is_some());
            if let Some(failed) = self.exception_frames.pop().flatten() {
                paths.push((Some(ExitReason::Raise), failed, entered.clone()));
            }
            for exit in std::mem::take(&mut self.pending) {
                paths.push((Some(exit.reason), exit.state, entered.clone()));
            }
            if !target_completed {
                break;
            }
            normal = Some(current);
        }
        if let Some(current) = normal {
            self.exception_frames.push(None);
            if let Some(end) = self.block(statement.body(), Some(current)) {
                paths.push((None, end, entered.clone()));
            }
            if let Some(failed) = self.exception_frames.pop().flatten() {
                paths.push((Some(ExitReason::Raise), failed, entered.clone()));
            }
            for exit in std::mem::take(&mut self.pending) {
                paths.push((Some(exit.reason), exit.state, entered.clone()));
            }
        }
        let mut normal_paths = Vec::new();
        let mut outputs = Vec::new();
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
                                Some(ExitReason::KnownRaise(point)) => {
                                    self.info.raise_names.get(&point).map(|raised| {
                                        names.iter().any(|name| exception_matches(raised, name))
                                    })
                                }
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
                let next = unique;
                reasons = next;
            }
            for reason in reasons {
                match reason {
                    Some(reason) => self.merge_exit(&mut outputs, reason, state.clone()),
                    None => normal_paths.push(Some(state.clone())),
                }
            }
        }
        self.pending = outside;
        for exit in outputs {
            self.emit_exit(exit.reason, exit.state);
        }
        self.merge(normal_paths)
    }

    fn for_stmt(&mut self, f: &ast::ForStmt, mut st: State) -> Option<State> {
        if let Some(it) = f.iter() {
            self.visit(it.syntax(), &mut st)?;
            if f.is_async() {
                self.complete_operation(
                    &it,
                    OperationKind::IteratorAcquisition,
                    self.protocol_async_acquisition(&it, &st),
                    &mut st,
                )?;
            } else {
                self.complete_operation(
                    &it,
                    OperationKind::IteratorAcquisition,
                    self.implicit_slot_terminal(&it, "__iter__", &st),
                    &mut st,
                )?;
            }
            if statically_empty_iterable(&it) {
                return match f.else_clause() {
                    Some(clause) => self.block(clause.body(), Some(st)),
                    None => Some(st),
                };
            }
        }
        let cardinality = if f.is_async() {
            None
        } else {
            f.iter()
                .and_then(|expression| literal_iterable_cardinality(&expression))
        };
        let first_required = cardinality.is_some_and(|count| count > 0);
        let precise = self.loops.len() < MAX_PRECISE_LOOP_DEPTH;
        let passes = if precise && cardinality != Some(1) {
            2
        } else {
            1
        };
        let mut head = if precise {
            st.clone()
        } else {
            self.with_bindings_in(st.clone(), f.syntax().text_range())
        };
        let mut exit = None;
        let mut last = LoopCtx::default();
        let mut first_breaks = Vec::new();
        for pass in 0..passes {
            let mut body_in = head.clone();
            let target_completed = match f.target() {
                Some(t) => self.target(&t, &mut body_in).is_some(),
                None => true,
            };
            let id = f.syntax().text_range().start();
            self.loops.push(LoopCtx {
                id,
                ..LoopCtx::default()
            });
            let end = self.block(f.body(), target_completed.then_some(body_in));
            self.loops.pop();
            let ctx = self.loop_exits(id);
            let mut all = vec![
                Some(if pass + 1 < passes {
                    st.clone()
                } else {
                    head.clone()
                }),
                end,
            ];
            all.extend(ctx.continues.iter().cloned().map(Some));
            if pass == 0 && first_required {
                // The normally evaluated exact literal must produce its first
                // element: only body completion/continue can reach exhaustion.
                all.remove(0);
            }
            let merged = self.merge(all);
            if pass == 0 && first_required && merged.is_none() {
                // Return/raise/break cannot be replaced by an invented later
                // zero-iteration path. Break exits are joined below, bypassing else.
                last = ctx;
                break;
            }
            if pass == 0 && first_required && pass + 1 < passes {
                first_breaks.extend(ctx.breaks.iter().cloned());
            }
            if pass + 1 < passes {
                head = merged.unwrap_or_else(|| st.clone());
            } else {
                // The iterable may be exhausted before any (further) iteration.
                exit = merged;
                last = ctx;
            }
        }
        let after = match f.else_clause() {
            Some(e) => self.block(e.body(), exit),
            None => exit,
        };
        let mut ends = vec![after];
        ends.extend(first_breaks.into_iter().map(Some));
        ends.extend(last.breaks.into_iter().map(Some));
        self.merge(ends)
    }

    fn try_stmt(&mut self, t: &ast::TryStmt, st: State) -> Option<State> {
        // Own only completions produced by this statement. An enclosing branch
        // may already have pending exits with the same reason/loop target.
        let outside = std::mem::take(&mut self.pending);
        let finally = t.finally_clause();
        self.exception_frames.push(None);
        let body_end = self.block(t.body(), Some(st));
        let exceptional = self.exception_frames.pop().flatten();
        let mut handler_states = vec![exceptional];
        handler_states.extend(
            self.pending
                .iter()
                .filter(|exit| is_raise(exit.reason))
                .map(|exit| Some(exit.state.clone())),
        );
        let handler_in = self.merge(handler_states);
        let catches_all = t
            .except_clauses()
            .any(|handler| handler.exception_type().is_none());
        if catches_all {
            self.pending.retain(|exit| !is_raise(exit.reason));
        } else if let Some(state) = handler_in.clone() {
            self.emit_exit(ExitReason::Raise, state);
        }
        // Else/handler failures bypass these handlers but still enter finally.
        self.exception_frames.push(None);
        let normal = match t.else_clause() {
            Some(e) => self.block(e.body(), body_end),
            None => body_end,
        };
        let mut ends = vec![normal];
        for handler in t.except_clauses() {
            let Some(mut input) = handler_in.clone() else {
                break;
            };
            if let Some(ty) = handler.exception_type() {
                if self.visit(ty.syntax(), &mut input).is_none() {
                    break;
                }
            }
            let name = handler.name();
            if let Some(name) = &name {
                self.bind_at(&mut input, name.text_range().start());
            }
            let previous = std::mem::take(&mut self.pending);
            let mut end = self.block(handler.body(), Some(input));
            // Exception targets are deleted even on return/break/continue.
            if let Some(name) = &name {
                if self.tracked.contains(name.text())
                    || (self.is_class && self.external_destination(name.text()).is_some())
                {
                    let key = self.stored_key(name.text());
                    if let Some(state) = &mut end {
                        state.insert(key.clone(), Vec::new());
                    }
                    for exit in &mut self.pending {
                        exit.state.insert(key.clone(), Vec::new());
                    }
                }
            }
            for exit in previous {
                self.emit_exit(exit.reason, exit.state);
            }
            ends.push(end);
            if handler.exception_type().is_none() {
                break;
            }
        }
        if let Some(state) = self.exception_frames.pop().flatten() {
            self.emit_exit(ExitReason::Raise, state);
        }
        let normal = self.merge(ends);
        let result = if let Some(finally) = finally {
            let pending = std::mem::take(&mut self.pending);
            let mut entries: Vec<(Option<ExitReason>, State)> = pending
                .into_iter()
                .map(|exit| (Some(exit.reason), exit.state))
                .collect();
            if let Some(state) = normal {
                entries.insert(0, (None, state));
            }
            let reads = self.merge(
                entries
                    .iter()
                    .map(|(_, state)| Some(state.clone()))
                    .collect(),
            );
            let record = self.record_reads;
            self.record_reads = false;
            let mut outputs = Vec::new();
            let mut normal_outputs = Vec::new();
            for (reason, state) in entries {
                let published_info = self.info.clone();
                self.exception_frames.push(None);
                let end = self.block(finally.body(), Some(state));
                if let Some(state) = self.exception_frames.pop().flatten() {
                    self.emit_exit(ExitReason::Raise, state);
                }
                self.info = published_info;
                // A new abrupt completion from finally replaces the old one.
                for exit in std::mem::take(&mut self.pending) {
                    self.merge_exit(&mut outputs, exit.reason, exit.state);
                }
                if let Some(state) = end {
                    match reason {
                        Some(reason) => self.merge_exit(&mut outputs, reason, state),
                        None => normal_outputs.push(Some(state)),
                    }
                }
            }
            self.record_reads = record;
            // Observe all actual entry paths once. No observation-only exit is
            // published, and no last per-reason visit overwrites earlier reads.
            self.exception_frames.push(None);
            let _ = self.block(finally.body(), reads);
            self.exception_frames.pop();
            self.pending.clear();
            for exit in outputs {
                self.emit_exit(exit.reason, exit.state);
            }
            self.merge(normal_outputs)
        } else {
            normal
        };
        let escaping: Vec<State> = self
            .pending
            .iter()
            .filter(|exit| is_raise(exit.reason))
            .map(|exit| exit.state.clone())
            .collect();
        for state in escaping {
            self.exception_point(&state);
        }
        for exit in outside {
            self.emit_exit(exit.reason, exit.state);
        }
        result
    }

    /// Reuses range predicates for patterns; no type inference runs in Flow.
    fn narrow_match_subject(
        &self,
        state: &mut State,
        statement: &ast::MatchStmt,
        pattern: &SyntaxNode,
        positive: bool,
    ) {
        if self.is_class {
            return;
        }
        let pred = Pred {
            cond: pattern.text_range(),
            positive,
        };
        // A member reference subject (`match self.state:`) narrows like a
        // condition on it; writes retire it through the reference key.
        if let Some(reference) = statement
            .subject()
            .and_then(|subject| reference_key(&subject))
            .filter(|key| is_member_key(key) && !self.reference_shadowed(key))
        {
            let reaches = state.entry(member_key(&reference)).or_insert_with(|| {
                vec![Reach {
                    binding: None,
                    preds: Vec::new(),
                }]
            });
            for reach in reaches.iter_mut() {
                push_pred(reach, pred);
            }
            compact(reaches);
            return;
        }
        let Some(ast::Expr::Name(subject)) = statement.subject().map(|subject| subject.unparen())
        else {
            return;
        };
        let name = subject.name_text();
        if !self.tracked.contains(name.as_str())
            || !crate::match_support::subject_name_stable(self.file, &name)
        {
            return;
        }
        if let Some(reaches) = state.get_mut(&self.stored_key(&name)) {
            for reach in reaches.iter_mut() {
                push_pred(reach, pred);
            }
            compact(reaches);
        }
    }

    fn match_stmt(&mut self, m: &ast::MatchStmt, mut st: State) -> Option<State> {
        if let Some(subject) = m.subject() {
            self.visit(subject.syntax(), &mut st)?;
        }
        let scalar = crate::match_support::finite_integer_coverage(self.file, m, |module| {
            self.authority.canonical_match_literal(module)
        });
        let enums = self.authority.enum_match_coverage(self.file, m);
        let structural = crate::match_support::structural_coverage(self.file, m, |module| {
            self.authority.canonical_match_literal(module)
        });
        let coverage = crate::match_support::merge_coverage(
            crate::match_support::merge_coverage(scalar, enums),
            structural,
        );
        let mut rest = Some(st);
        let mut ends = Vec::new();
        for (case_index, case) in m.cases().enumerate() {
            let Some(input) = rest.take() else {
                break;
            };
            let mut matched = input.clone();
            let mut irrefutable = false;
            if let Some(pattern) = case.pattern() {
                irrefutable = is_irrefutable(&pattern)
                    || coverage
                        .as_ref()
                        .is_some_and(|coverage| coverage.get(case_index) == Some(&true));
                self.narrow_match_subject(&mut matched, m, &pattern, true);
                for token in pattern
                    .descendants_with_tokens()
                    .filter_map(|e| e.into_token())
                    .filter(|t| t.kind() == SyntaxKind::IDENT)
                {
                    self.bind_at(&mut matched, token.text_range().start());
                }
            }
            // A failed non-irrefutable pattern preserves the original state.
            let unmatched = (!irrefutable).then(|| {
                let mut unmatched = input;
                if let Some(pattern) = case.pattern() {
                    self.narrow_match_subject(&mut unmatched, m, &pattern, false);
                }
                unmatched
            });
            if let Some(guard) = case.guard() {
                if self.visit(guard.syntax(), &mut matched).is_none() {
                    rest = unmatched;
                    continue;
                }
                let truth = is_constant(&guard);
                if truth != Some(false) {
                    let mut passed = matched.clone();
                    self.narrow(&mut passed, &guard, true);
                    ends.push(self.block(case.body(), Some(passed)));
                }
                let failed = (truth != Some(true)).then(|| {
                    self.narrow(&mut matched, &guard, false);
                    matched
                });
                rest = self.merge(vec![unmatched, failed]);
            } else {
                ends.push(self.block(case.body(), Some(matched)));
                rest = unmatched;
            }
        }
        ends.push(rest);
        self.merge(ends)
    }

    // ----- expressions -------------------------------------------------------------

    /// Evaluate now, retaining only normal completion. Deferred bodies are not
    /// invocation sites; each actual invocation records the shared AND proof.
    fn visit(&mut self, node: &SyntaxNode, st: &mut State) -> Option<()> {
        if node.kind() != SyntaxKind::ARG_LIST {
            if let Some(observer) = &self.cursor {
                observer.observe(self, st, node);
            }
        }
        match node.kind() {
            SyntaxKind::NAME_EXPR => {
                if let Some(tok) = ast::NameExpr::cast(node.clone()).and_then(|n| n.name_token()) {
                    self.record(st, tok.text(), tok.text_range().start());
                    if self.is_class
                        && (!self.class_namespace_unknown
                            || self
                                .file
                                .scopes
                                .scope(self.scope)
                                .nonlocals
                                .iter()
                                .any(|name| name == tok.text()))
                    {
                        let destination = self.lexical_destination(tok.text());
                        if destination != self.scope
                            && matches!(
                                self.file.scopes.scope(destination).kind,
                                ScopeKind::Function | ScopeKind::Lambda
                            )
                        {
                            if let Some(values) = self.captured_lookup(destination, tok.text(), st)
                            {
                                if values.is_empty()
                                    || values.iter().any(|reach| reach.binding.is_none())
                                {
                                    let mut failed = st.clone();
                                    failed.insert(
                                        scoped_key(destination, tok.text()),
                                        values
                                            .iter()
                                            .filter(|reach| reach.binding.is_none())
                                            .cloned()
                                            .collect(),
                                    );
                                    self.exception_point(&failed);
                                    self.emit_exit(ExitReason::Raise, failed);
                                    let normal: Vec<_> = values
                                        .into_iter()
                                        .filter(|reach| reach.binding.is_some())
                                        .collect();
                                    if normal.is_empty() {
                                        return None;
                                    }
                                    st.insert(scoped_key(destination, tok.text()), normal);
                                }
                            }
                        }
                    }
                    // Fast-local loads have no globals/class fallback. Keep the
                    // diagnostic load itself, then stop definite unbound paths.
                    if self.file.scopes.scope(self.scope).kind == ScopeKind::Function
                        && self.tracked.contains(tok.text())
                        && !self.shadowed.iter().any(|names| names.contains(tok.text()))
                    {
                        let reaches = st.get(tok.text());
                        let unbound = reaches.is_none_or(|values| {
                            values.is_empty() || values.iter().any(|reach| reach.binding.is_none())
                        });
                        if unbound {
                            self.emit_exit(ExitReason::Raise, st.clone());
                            let mut normal: Vec<Reach> = reaches
                                .into_iter()
                                .flatten()
                                .filter(|reach| reach.binding.is_some())
                                .cloned()
                                .collect();
                            if normal.is_empty() {
                                return None;
                            }
                            compact(&mut normal);
                            st.insert(SmolStr::new(tok.text()), normal);
                        }
                    }
                }
            }
            SyntaxKind::NAMED_EXPR => {
                let n = ast::NamedExpr::cast(node.clone())?;
                if let Some(v) = n.value() {
                    self.visit(v.syntax(), st)?;
                }
                if let Some(t) = n.target().and_then(|t| t.name_token()) {
                    self.bind_at(st, t.text_range().start());
                }
            }
            SyntaxKind::LAMBDA_EXPR => {
                for p in ast::LambdaExpr::cast(node.clone())
                    .and_then(|l| l.params())
                    .into_iter()
                    .flat_map(|p| p.params())
                {
                    if let Some(d) = p.default() {
                        self.visit(d.syntax(), st)?;
                    }
                }
            }
            SyntaxKind::BINARY_EXPR => {
                let binary = ast::BinaryExpr::cast(node.clone())?;
                self.visit(binary.lhs()?.syntax(), st)?;
                self.visit(binary.rhs()?.syntax(), st)?;
                // Operand evaluation precedes implicit operator dispatch. The
                // proof declines effectful operands rather than reusing stale
                // callable/receiver identities from before their evaluation.
                let terminal = self.operator_invocation_terminal(&binary, st);
                self.record_invocation(&ast::Expr::Binary(binary), terminal);
                if terminal {
                    self.emit_exit(ExitReason::Raise, st.clone());
                    return None;
                }
            }
            SyntaxKind::NOT_EXPR => {
                let expression = ast::NotExpr::cast(node.clone())?;
                let operand = expression.operand()?;
                self.visit(operand.syntax(), st)?;
                let terminal = self.protocol_truth_terminal(&operand, st);
                self.record_operation(&ast::Expr::Not(expression), OperationKind::Value, terminal);
                self.complete_truth(&operand, st)?;
            }
            SyntaxKind::UNARY_EXPR => {
                let unary = ast::UnaryExpr::cast(node.clone())?;
                let operand = unary.operand()?;
                self.visit(operand.syntax(), st)?;
                let slot = match unary.op()?.kind() {
                    SyntaxKind::MINUS => "__neg__",
                    SyntaxKind::PLUS => "__pos__",
                    SyntaxKind::TILDE => "__invert__",
                    SyntaxKind::NOT_KW => {
                        let terminal = self.protocol_truth_terminal(&operand, st);
                        self.record_operation(
                            &ast::Expr::Unary(unary),
                            OperationKind::Value,
                            terminal,
                        );
                        self.complete_truth(&operand, st)?;
                        return Some(());
                    }
                    _ => "",
                };
                self.complete_operation(
                    &ast::Expr::Unary(unary),
                    OperationKind::Value,
                    self.implicit_slot_terminal(&operand, slot, st),
                    st,
                )?;
            }
            SyntaxKind::SUBSCRIPT_EXPR => {
                let subscript = ast::SubscriptExpr::cast(node.clone())?;
                let object = subscript.object()?;
                self.visit(object.syntax(), st)?;
                let captured = st.clone();
                let terminal = self.implicit_slot_terminal(&object, "__getitem__", &captured);
                if let Some(index) = subscript.index() {
                    self.visit(&index, st)?;
                }
                let expression = ast::Expr::Subscript(subscript);
                self.record_reference(st, &expression);
                self.complete_operation(&expression, OperationKind::Value, terminal, st)?;
            }
            SyntaxKind::BOOL_OP_EXPR => {
                let boolean = ast::BoolOpExpr::cast(node.clone())?;
                let mut operands = boolean.operands();
                let first = operands.next()?;
                self.visit(first.syntax(), st)?;
                let mut previous = first;
                let mut skipped = Vec::new();
                for next in operands {
                    if self.complete_truth(&previous, st).is_none() {
                        *st = self.merge(skipped)?;
                        return Some(());
                    }
                    let truth = is_constant(&previous);
                    let continue_when = !boolean.is_or();
                    if truth == Some(!continue_when) {
                        skipped.push(Some(st.clone()));
                        *st = self.merge(skipped)?;
                        return Some(());
                    }
                    if truth.is_none() {
                        skipped.push(Some(st.clone()));
                    }
                    if self.visit(next.syntax(), st).is_none() {
                        *st = self.merge(skipped)?;
                        return Some(());
                    }
                    previous = next;
                }
                skipped.push(Some(st.clone()));
                *st = self.merge(skipped)?;
            }
            SyntaxKind::CONDITIONAL_EXPR => {
                let conditional = ast::ConditionalExpr::cast(node.clone())?;
                let condition = conditional.condition()?;
                self.visit_truth(&condition, st)?;
                let mut ends = Vec::new();
                for (take, arm) in [
                    (true, conditional.true_expr()),
                    (false, conditional.false_expr()),
                ] {
                    if is_constant(&condition) == Some(!take) {
                        continue;
                    }
                    if let Some(arm) = arm {
                        let mut branch = st.clone();
                        self.narrow(&mut branch, &condition, take);
                        if self.visit(arm.syntax(), &mut branch).is_some() {
                            ends.push(Some(branch));
                        }
                    }
                }
                *st = self.merge(ends)?;
            }
            SyntaxKind::COMPARE_EXPR => {
                let comparison = ast::CompareExpr::cast(node.clone())?;
                let whole = ast::Expr::Compare(comparison.clone());
                let mut operands = comparison.operands();
                let mut previous = operands.next()?;
                self.visit(previous.syntax(), st)?;
                let mut captured = st.clone();
                let mut skipped = Vec::new();
                let mut previous_truth = false;
                let mut operators = comparison.ops();
                for (index, operand) in operands.enumerate() {
                    if index > 0 {
                        if index <= 64 {
                            self.record_operation(
                                &whole,
                                OperationKind::ComparisonTruth((index - 1) as u16),
                                previous_truth,
                            );
                        }
                        if previous_truth {
                            let _ = self.complete_implicit(true, st);
                            let normal = self.merge(skipped);
                            self.record_operation(&whole, OperationKind::Value, normal.is_none());
                            self.comparison_truth_results
                                .insert(comparison.text_range(), false);
                            *st = normal?;
                            return Some(());
                        }
                        skipped.push(Some(st.clone()));
                    }
                    if self.visit(operand.syntax(), st).is_none() {
                        *st = self.merge(skipped)?;
                        return Some(());
                    }
                    let operator = operators.next();
                    let proof = if index < 64 {
                        operator.map(|operator| {
                            self.protocol_comparison(
                                &previous,
                                &operand,
                                &operator.text(),
                                &captured,
                                st,
                            )
                        })
                    } else {
                        None
                    };
                    let terminal = proof.is_some_and(|proof| proof.invocation_terminal);
                    if index < 64 {
                        self.record_operation(
                            &whole,
                            OperationKind::ComparisonValue(index as u16),
                            terminal,
                        );
                    }
                    if terminal {
                        let _ = self.complete_implicit(true, st);
                        let normal = self.merge(skipped);
                        self.record_operation(&whole, OperationKind::Value, normal.is_none());
                        self.comparison_truth_results
                            .insert(comparison.text_range(), false);
                        *st = normal?;
                        return Some(());
                    }
                    previous_truth = proof.is_some_and(|proof| proof.truth_terminal);
                    previous = operand;
                    captured = st.clone();
                }
                let all_result_paths_terminal_truth = skipped.is_empty() && previous_truth;
                skipped.push(Some(st.clone()));
                *st = self.merge(skipped)?;
                self.comparison_truth_results
                    .insert(comparison.text_range(), all_result_paths_terminal_truth);
                self.record_operation(&whole, OperationKind::Value, false);
            }
            SyntaxKind::ARG_LIST => {
                let arguments = ast::ArgList::cast(node.clone())?;
                let values: Vec<ast::AnyArg> = arguments.args().collect();
                // Python assembles positional/starred arguments before keyword
                // arguments, even when a star appears later in source text.
                for keywords in [false, true] {
                    for argument in &values {
                        let is_keyword =
                            matches!(argument, ast::AnyArg::Keyword(_) | ast::AnyArg::StarStar(_));
                        if is_keyword == keywords {
                            if let Some(value) = argument.value() {
                                self.visit(value.syntax(), st)?;
                            }
                        }
                    }
                }
            }
            SyntaxKind::CALL_EXPR => {
                let call = ast::CallExpr::cast(node.clone())?;
                if let Some(callee) = call.callee() {
                    self.visit(callee.syntax(), st)?;
                }
                // The runtime callable is captured before argument expressions
                // may rebind its name. Proof uses exactly that captured state.
                let expression = ast::Expr::Call(call.clone());
                let modes = self.acquired_call_modes(&call, st);
                let mut terminal = modes[0];
                let protocol = self.protocol_builtin_call(&call, st);
                self.capture_factory_result(&call, st);
                if let Some((kind, arguments)) = protocol {
                    let first = arguments.first()?;
                    self.visit(first.syntax(), st)?;
                    let receiver_state = st.clone();
                    for argument in arguments.iter().skip(1) {
                        self.visit(argument.syntax(), st)?;
                    }
                    // The protocol fast path evaluates arguments directly;
                    // expose the same post-operand cursor phase as ARG_LIST.
                    if let (Some(observer), Some(list)) = (&self.cursor, call.arg_list()) {
                        observer.observe(self, st, list.syntax());
                    }
                    terminal |= self.protocol_builtin_terminal(
                        kind,
                        first,
                        &receiver_state,
                        arguments.len() == 2,
                    );
                } else if let Some(arguments) = call.arg_list() {
                    self.visit(arguments.syntax(), st)?;
                }
                self.acquired_awaited_calls
                    .insert(call.text_range(), modes[1]);
                self.record_invocation(&expression, terminal);
                if terminal {
                    self.emit_exit(ExitReason::Raise, st.clone());
                    return None;
                }
            }
            SyntaxKind::ATTRIBUTE_EXPR => {
                let attribute = ast::AttributeExpr::cast(node.clone())?;
                let terminal = self.property_getter_terminal(&attribute, st);
                if let Some(object) = attribute.object() {
                    self.visit(object.syntax(), st)?;
                }
                let expression = ast::Expr::Attribute(attribute);
                self.record_reference(st, &expression);
                self.record_invocation(&expression, terminal);
                if terminal {
                    self.emit_exit(ExitReason::Raise, st.clone());
                    return None;
                }
            }
            SyntaxKind::AWAIT_EXPR => {
                let await_expression = ast::AwaitExpr::cast(node.clone())?;
                let expression = ast::Expr::Await(await_expression.clone());
                let declared_operand = await_expression
                    .expr()
                    .is_some_and(|inner| self.declared_await_operand_terminal(&inner, st));
                if let Some(inner) = await_expression.expr() {
                    self.visit(inner.syntax(), st)?;
                }
                // The inner call stores its callable mode before evaluating its
                // own arguments. Use that capture after evaluation, never re-resolve.
                let terminal = declared_operand
                    || await_expression
                        .expr()
                        .map(ast::Expr::unparen)
                        .and_then(|inner| match inner {
                            ast::Expr::Call(call) => {
                                self.acquired_awaited_calls.get(&call.text_range()).copied()
                            }
                            _ => None,
                        })
                        .unwrap_or(false);
                self.record_invocation(&expression, terminal);
                if terminal {
                    self.emit_exit(ExitReason::Raise, st.clone());
                    return None;
                }
            }
            SyntaxKind::LIST_COMP
            | SyntaxKind::SET_COMP
            | SyntaxKind::GENERATOR_EXPR
            | SyntaxKind::DICT_COMP => {
                self.comprehension(node, st)?;
            }
            _ => {
                for child in node.children() {
                    self.visit(&child, st)?;
                }
            }
        }
        if node.kind() == SyntaxKind::ARG_LIST {
            if let Some(observer) = &self.cursor {
                observer.observe(self, st, node);
            }
        }
        Some(())
    }

    fn record_operation(&mut self, expression: &ast::Expr, kind: OperationKind, terminal: bool) {
        if !self.record_reads {
            return;
        }
        let point = OperationPoint {
            range: expression.clone().unparen().syntax().text_range(),
            kind,
        };
        if terminal && !self.nonterminal_operations.contains(&point) {
            self.info.terminal_operations.insert(point);
        } else {
            self.nonterminal_operations.insert(point);
            self.info.terminal_operations.remove(&point);
        }
    }
    fn record_invocation(&mut self, expression: &ast::Expr, terminal: bool) {
        self.record_operation(expression, OperationKind::Value, terminal);
        if !self.record_reads {
            return;
        }
        let range = expression.syntax().text_range();
        if terminal && !self.nonterminal_calls.contains(&range) {
            self.info.terminal_calls.insert(range);
        } else {
            self.nonterminal_calls.insert(range);
            self.info.terminal_calls.remove(&range);
        }
    }

    fn comprehension_target(&mut self, target: &ast::Expr, state: &mut State) -> Option<()> {
        match target {
            ast::Expr::Name(_) => {} // Inner locals are already shadowed.
            ast::Expr::Tuple(tuple) => {
                for target in tuple.elements() {
                    self.comprehension_target(&target, state)?;
                }
            }
            ast::Expr::List(list) => {
                for target in list.elements() {
                    self.comprehension_target(&target, state)?;
                }
            }
            ast::Expr::Paren(paren) => {
                if let Some(target) = paren.inner() {
                    self.comprehension_target(&target, state)?;
                }
            }
            ast::Expr::Star(star) => {
                if let Some(target) = star.expr() {
                    self.comprehension_target(&target, state)?;
                }
            }
            ast::Expr::Attribute(_) | ast::Expr::Subscript(_) => {
                self.target(target, state)?;
            }
            other => {
                self.visit(other.syntax(), state)?;
            }
        }
        Some(())
    }

    fn comprehension_assignable_target(
        target: &ast::Expr,
        depth: usize,
        remaining: &mut usize,
    ) -> bool {
        if depth > 16 || *remaining == 0 {
            return false;
        }
        *remaining -= 1;
        match target.clone().unparen() {
            ast::Expr::Name(name) => name.name_token().is_some(),
            ast::Expr::Attribute(attribute) => {
                attribute.object().is_some() && attribute.attr().is_some()
            }
            ast::Expr::Subscript(subscript) => {
                subscript.object().is_some() && subscript.index().is_some()
            }
            ast::Expr::Tuple(tuple) => tuple.elements().all(|element| {
                Self::comprehension_assignable_target(&element, depth + 1, remaining)
            }),
            ast::Expr::List(list) => list.elements().all(|element| {
                Self::comprehension_assignable_target(&element, depth + 1, remaining)
            }),
            ast::Expr::Star(star) => star.expr().is_some_and(|element| {
                Self::comprehension_assignable_target(&element, depth + 1, remaining)
            }),
            _ => false,
        }
    }

    /// Widen only states that already completed a mandatory iteration. Never
    /// reintroduce the pre-loop zero-iteration state for a known nonempty shape.
    fn comprehension_successor(&self, state: State, range: TextRange) -> State {
        let before = state.clone();
        let mut widened = self.with_bindings_in(state, range);
        for (name, values) in &mut widened {
            if before.get(name).is_none_or(|values| values.is_empty()) {
                let unbound = Reach {
                    binding: None,
                    preds: Vec::new(),
                };
                if !values.contains(&unbound) {
                    values.push(unbound);
                }
            }
            for value in values.iter_mut() {
                value.preds.clear();
            }
            compact(values);
        }
        widened
    }

    fn comprehension(&mut self, node: &SyntaxNode, st: &mut State) -> Option<()> {
        if node.kind() == SyntaxKind::GENERATOR_EXPR {
            return self.comprehension_fallback(node, st);
        }
        let clauses: Vec<_> = node
            .children()
            .filter(|child| matches!(child.kind(), SyntaxKind::COMP_FOR | SyntaxKind::COMP_IF))
            .take(17)
            .collect();
        let mut product = 1usize;
        let mut exact_replay = true;
        let mut target_budget = 256usize;
        if clauses.is_empty() || clauses.len() > 16 {
            return self.comprehension_fallback(node, st);
        }
        for clause in &clauses {
            if let Some(clause) = ast::CompFor::cast(clause.clone()) {
                if clause.is_async()
                    || clause.target().is_none_or(|target| {
                        !Self::comprehension_assignable_target(&target, 0, &mut target_budget)
                    })
                    || clause.iter().is_none()
                {
                    return self.comprehension_fallback(node, st);
                }
                let cardinality = clause
                    .iter()
                    .and_then(|expression| literal_iterable_cardinality(&expression))
                    .unwrap_or(1);
                product = product.saturating_mul(cardinality.max(1));
                exact_replay &= cardinality <= 16 && product <= 64;
            } else if clause.children().find_map(ast::Expr::cast).is_none() {
                // Missing recovery fields cannot establish abrupt completion.
                return self.comprehension_fallback(node, st);
            }
        }
        // The outer iterable belongs to the surrounding scope, before the
        // comprehension's target names begin shadowing that scope.
        let first = ast::CompFor::cast(clauses[0].clone())?;
        let iterable = first.iter()?;
        self.visit(iterable.syntax(), st)?;
        self.complete_operation(
            &iterable,
            OperationKind::IteratorAcquisition,
            self.implicit_slot_terminal(&iterable, "__iter__", st),
            st,
        )?;
        let own = self
            .file
            .scopes
            .scope_for_node(node.text_range())
            .map(|scope| {
                self.file
                    .scopes
                    .scope(scope)
                    .bindings
                    .keys()
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        self.shadowed.push(own);
        let mut replay = ComprehensionReplay {
            exact: exact_replay,
            remaining: 2048,
            truncated: false,
        };
        let result = self.comprehension_clauses(node, &clauses, 0, st.clone(), true, &mut replay);
        if result.is_some() && replay.truncated && self.record_reads {
            // Unreplayed visits cannot supply universal operation-point proof.
            let range = node.text_range();
            let calls: Vec<_> = self
                .info
                .terminal_calls
                .iter()
                .copied()
                .filter(|point| range.contains_range(*point))
                .collect();
            self.nonterminal_calls.extend(calls);
            self.info
                .terminal_calls
                .retain(|point| !range.contains_range(*point));
            let operations: Vec<_> = self
                .info
                .terminal_operations
                .iter()
                .copied()
                .filter(|point| range.contains_range(point.range))
                .collect();
            self.nonterminal_operations.extend(operations);
            self.info
                .terminal_operations
                .retain(|point| !range.contains_range(point.range));
        }
        self.shadowed.pop();
        *st = result?;
        Some(())
    }

    fn comprehension_clauses(
        &mut self,
        node: &SyntaxNode,
        clauses: &[SyntaxNode],
        index: usize,
        mut state: State,
        first_acquired: bool,
        replay: &mut ComprehensionReplay,
    ) -> Option<State> {
        if replay.remaining == 0 {
            replay.truncated = true;
            return Some(self.comprehension_successor(state, node.text_range()));
        }
        replay.remaining -= 1;
        let Some(clause) = clauses.get(index) else {
            for element in node
                .children()
                .filter(|child| !matches!(child.kind(), SyntaxKind::COMP_FOR | SyntaxKind::COMP_IF))
            {
                self.visit(&element, &mut state)?;
            }
            return Some(state);
        };
        if let Some(clause) = ast::CompFor::cast(clause.clone()) {
            let iterable = clause.iter()?;
            if !first_acquired {
                self.visit(iterable.syntax(), &mut state)?;
                self.complete_operation(
                    &iterable,
                    OperationKind::IteratorAcquisition,
                    self.implicit_slot_terminal(&iterable, "__iter__", &state),
                    &mut state,
                )?;
            }
            let count = literal_iterable_cardinality(&iterable);
            if count == Some(0) {
                return Some(state);
            }
            // Unknown iterables retain the zero-iteration alternative. Exact
            // small cardinalities execute every required iteration in order.
            let skipped = count.is_none().then(|| state.clone());
            let mut current = Some(state);
            let iterations = if replay.exact {
                count.unwrap_or(1)
            } else {
                count.unwrap_or(1).min(2)
            };
            for pass in 0..iterations {
                if pass > 0 && replay.remaining <= clauses.len() + 1 {
                    replay.truncated = true;
                    current =
                        current.map(|state| self.comprehension_successor(state, node.text_range()));
                    break;
                }
                if pass > 0 && !replay.exact {
                    current =
                        current.map(|state| self.comprehension_successor(state, node.text_range()));
                }
                let Some(mut iteration) = current.take() else {
                    break;
                };
                if let Some(target) = clause.target() {
                    if self.comprehension_target(&target, &mut iteration).is_none() {
                        current = None;
                        break;
                    }
                }
                current =
                    self.comprehension_clauses(node, clauses, index + 1, iteration, false, replay);
            }
            if count.is_some_and(|count| count > iterations) {
                replay.truncated = true;
                current =
                    current.map(|state| self.comprehension_successor(state, node.text_range()));
            }
            self.merge(vec![skipped, current])
        } else {
            let condition = clause.children().find_map(ast::Expr::cast)?;
            self.visit_truth(&condition, &mut state)?;
            match is_constant(&condition) {
                Some(false) => Some(state),
                Some(true) => {
                    self.comprehension_clauses(node, clauses, index + 1, state, false, replay)
                }
                None => {
                    let skipped = state.clone();
                    let taken =
                        self.comprehension_clauses(node, clauses, index + 1, state, false, replay);
                    self.merge(vec![Some(skipped), taken])
                }
            }
        }
    }

    fn comprehension_fallback(&mut self, node: &SyntaxNode, st: &mut State) -> Option<()> {
        let own: FxHashSet<SmolStr> = self
            .file
            .scopes
            .scope_for_node(node.text_range())
            .map(|s| self.file.scopes.scope(s).bindings.keys().cloned().collect())
            .unwrap_or_default();
        let clauses: Vec<SyntaxNode> = node
            .children()
            .filter(|c| matches!(c.kind(), SyntaxKind::COMP_FOR | SyntaxKind::COMP_IF))
            .collect();
        let mut clauses = clauses.into_iter();
        let mut known_empty = false;
        let mut first_target = None;
        if let Some(first) = clauses.next() {
            let first = ast::CompFor::cast(first)?;
            first_target = first.target();
            if let Some(iterable) = first.iter() {
                known_empty = match iterable.clone().unparen() {
                    ast::Expr::List(list) => list.elements().next().is_none(),
                    ast::Expr::Tuple(tuple) => tuple.elements().next().is_none(),
                    _ => false,
                };
                self.visit(iterable.syntax(), st)?;
            }
        }
        // Generator construction evaluates only the outermost iterable.
        if node.kind() == SyntaxKind::GENERATOR_EXPR || known_empty {
            return Some(());
        }
        // An eager comprehension may be empty: iteration-only termination must
        // not eliminate its normal creation path or bind outer walrus names.
        let skipped = st.clone();
        let mut iteration = st.clone();
        let mut normal_paths = vec![Some(skipped)];
        self.shadowed.push(own);
        let mut normal = first_target
            .as_ref()
            .is_none_or(|target| self.comprehension_target(target, &mut iteration).is_some());
        for clause in clauses {
            if !normal {
                break;
            }
            if clause.kind() == SyntaxKind::COMP_FOR {
                normal_paths.push(Some(iteration.clone()));
                if let Some(clause) = ast::CompFor::cast(clause) {
                    if let Some(iterable) = clause.iter() {
                        if self.visit(iterable.syntax(), &mut iteration).is_none() {
                            normal = false;
                            break;
                        }
                    }
                    if let Some(target) = clause.target() {
                        if self.comprehension_target(&target, &mut iteration).is_none() {
                            normal = false;
                            break;
                        }
                    }
                }
            } else {
                for condition in clause.children() {
                    if self.visit(&condition, &mut iteration).is_none() {
                        normal = false;
                        break;
                    }
                    match ast::Expr::cast(condition).as_ref().and_then(is_constant) {
                        Some(false) => {
                            normal_paths.push(Some(iteration.clone()));
                            normal = false;
                            break;
                        }
                        None => normal_paths.push(Some(iteration.clone())),
                        Some(true) => {}
                    }
                }
                if !normal {
                    break;
                }
            }
        }
        if normal {
            for element in node
                .children()
                .filter(|c| !matches!(c.kind(), SyntaxKind::COMP_FOR | SyntaxKind::COMP_IF))
            {
                if self.visit(&element, &mut iteration).is_none() {
                    normal = false;
                    break;
                }
            }
        }
        self.shadowed.pop();
        if normal {
            normal_paths.push(Some(iteration));
        }
        *st = self.merge(normal_paths)?;
        Some(())
    }
}

fn push_pred(r: &mut Reach, p: Pred) {
    if r.preds.len() < MAX_PREDS && !r.preds.contains(&p) {
        r.preds.push(p);
    }
}

/// Drops paths implied by a less narrowed path of the same binding and
/// collapses long lists.
fn compact(rs: &mut Vec<Reach>) {
    let mut i = 0;
    while i < rs.len() {
        let subsumed = rs.iter().enumerate().any(|(j, o)| {
            j != i
                && o.binding == rs[i].binding
                && o.preds.len() < rs[i].preds.len()
                && o.preds.iter().all(|p| rs[i].preds.contains(p))
        });
        if subsumed {
            rs.remove(i);
        } else {
            i += 1;
        }
    }
    if rs.len() > MAX_REACH {
        // Keep one path per binding with the conditions common to all of them.
        let mut out: Vec<Reach> = Vec::new();
        for r in rs.drain(..) {
            match out.iter_mut().find(|o| o.binding == r.binding) {
                Some(o) => o.preds.retain(|p| r.preds.contains(p)),
                None => out.push(r),
            }
        }
        *rs = out;
    }
}

/// State-key prefix of member references; NUL-free and never an identifier.
const MEMBER_MARK: char = '\u{1}';
/// Longest member reference key that is narrowed.
const MAX_REFERENCE_KEY: usize = 256;

/// Structural key of a narrowable reference, like pyright's reference keys:
/// a name followed by attribute accesses and subscripts with an `int` or
/// `str` literal index (`self.x`, `a.b.c`, `d["k"]`, `t[-1].y`). Equal keys
/// denote the same storage as long as no write to the reference or one of its
/// prefixes intervenes; calls in between keep the narrowing (pyright's
/// choice: a callee mutating the object is not modelled).
pub(crate) fn reference_key(expression: &ast::Expr) -> Option<SmolStr> {
    fn build(expression: ast::Expr, out: &mut String, depth: usize) -> Option<()> {
        if depth > 8 || out.len() > MAX_REFERENCE_KEY {
            return None;
        }
        match expression.unparen() {
            ast::Expr::Name(name) => out.push_str(&name.name_text()),
            ast::Expr::Attribute(attribute) => {
                let attr = attribute.attr_text();
                if attr.is_empty() {
                    return None;
                }
                build(attribute.object()?, out, depth + 1)?;
                out.push('.');
                out.push_str(&attr);
            }
            ast::Expr::Subscript(subscript) => {
                let index = literal_index(subscript.index_expr()?)?;
                build(subscript.object()?, out, depth + 1)?;
                out.push('[');
                out.push_str(&index);
                out.push(']');
            }
            _ => return None,
        }
        Some(())
    }
    let mut out = String::new();
    build(expression.clone(), &mut out, 0)?;
    (!out.is_empty() && out.len() <= MAX_REFERENCE_KEY).then(|| SmolStr::new(out))
}

/// Canonical text of an `int` / `str` literal subscript index.
fn literal_index(index: ast::Expr) -> Option<String> {
    let int = |literal: &ast::LiteralExpr| -> Option<i64> {
        let token = literal.token()?;
        if token.kind() != SyntaxKind::INT_NUMBER {
            return None;
        }
        token.text().replace('_', "").parse().ok()
    };
    match index.unparen() {
        ast::Expr::Literal(literal) => int(&literal).map(|value| value.to_string()),
        ast::Expr::Unary(unary) if unary.op().is_some_and(|op| op.kind() == SyntaxKind::MINUS) => {
            match unary.operand()?.unparen() {
                ast::Expr::Literal(literal) => int(&literal).map(|value| (-value).to_string()),
                _ => None,
            }
        }
        ast::Expr::String(string) => {
            let prefix = string.prefix();
            (!prefix.contains('b') && !prefix.contains('f'))
                .then(|| format!("{:?}", string.value()))
        }
        _ => None,
    }
}

/// Keys with an attribute or subscript (plain names use name bindings).
fn is_member_key(key: &str) -> bool {
    key.contains(['.', '['])
}

fn member_key(reference: &str) -> SmolStr {
    SmolStr::new(format!("{MEMBER_MARK}{reference}"))
}

/// The name a reference key starts with.
fn reference_root(reference: &str) -> &str {
    reference.split(['.', '[']).next().unwrap_or(reference)
}

/// Drops the narrowing of `reference` and every reference it prefixes.
fn invalidate_references(st: &mut State, reference: &str) {
    if !st.keys().any(|key| key.starts_with(MEMBER_MARK)) {
        return;
    }
    st.retain(|key, _| {
        let Some(key) = key.strip_prefix(MEMBER_MARK) else {
            return true;
        };
        let Some(rest) = key.strip_prefix(reference) else {
            return true;
        };
        !(rest.is_empty() || rest.starts_with(['.', '[']))
    });
}

/// A store or delete through `target`: retires the stored reference, and for
/// a dynamic subscript (`d[k] = …`) every literal-keyed item of the owner.
fn retire_stored_references(st: &mut State, target: &ast::Expr) {
    match target.clone().unparen() {
        ast::Expr::Attribute(_) => {
            if let Some(reference) = reference_key(target) {
                invalidate_references(st, &reference);
            }
        }
        ast::Expr::Subscript(subscript) => {
            if let Some(reference) = reference_key(target) {
                invalidate_references(st, &reference);
            } else if let Some(owner) = subscript.object().as_ref().and_then(reference_key) {
                let items = format!("{owner}[");
                st.retain(|key, _| {
                    key.strip_prefix(MEMBER_MARK)
                        .is_none_or(|key| !key.starts_with(&items))
                });
            }
        }
        _ => {}
    }
}

fn push_reference_subject(expression: &ast::Expr, out: &mut Vec<SmolStr>) {
    if let Some(reference) = reference_key(expression).filter(|key| is_member_key(key)) {
        let key = member_key(&reference);
        if !out.contains(&key) {
            out.push(key);
        }
    }
}

/// Potential names for a predicate. This discovers syntax only; narrowing owns
/// canonical callable/member/alias authority. Exhaustion discards the whole set.
fn subjects(cond: &ast::Expr, out: &mut Vec<SmolStr>, depth: usize) {
    let original = out.len();
    let mut remaining = 256usize;
    if !collect_subjects(cond, out, depth, &mut remaining) {
        out.truncate(original);
    }
}
fn collect_subjects(
    expression: &ast::Expr,
    out: &mut Vec<SmolStr>,
    depth: usize,
    remaining: &mut usize,
) -> bool {
    if depth > 16 || *remaining == 0 {
        return false;
    }
    *remaining -= 1;
    match expression.clone().unparen() {
        ast::Expr::Name(name) => {
            let name = SmolStr::new(name.name_text());
            if !out.contains(&name) {
                out.push(name);
            }
        }
        ast::Expr::Named(named) => {
            if let Some(target) = named.target() {
                let name = SmolStr::new(target.name_text());
                if !out.contains(&name) {
                    out.push(name);
                }
            }
            if let Some(value) = named.value() {
                if !collect_subjects(&value, out, depth + 1, remaining) {
                    return false;
                }
            }
        }
        ast::Expr::Not(not) => {
            if let Some(operand) = not.operand() {
                if !collect_subjects(&operand, out, depth + 1, remaining) {
                    return false;
                }
            }
        }
        ast::Expr::BoolOp(boolean) => {
            for operand in boolean.operands() {
                if !collect_subjects(&operand, out, depth + 1, remaining) {
                    return false;
                }
            }
        }
        ast::Expr::Call(call) => {
            // Signature binding and guard identity are deliberately not decided
            // here. Keyword and positional argument values may carry the subject.
            if let Some(arguments) = call.arg_list() {
                for argument in arguments.args() {
                    if let Some(value) = argument.value() {
                        if !collect_subjects(&value, out, depth + 1, remaining) {
                            return false;
                        }
                    }
                }
            }
        }
        ast::Expr::Attribute(attribute) => {
            push_reference_subject(expression, out);
            if let Some(owner) = attribute.object() {
                if !collect_subjects(&owner, out, depth + 1, remaining) {
                    return false;
                }
            }
        }
        ast::Expr::Subscript(subscript) => {
            // Owners carry member/key predicates. A dynamic index is not itself
            // the tested container and receives no invented type refinement.
            push_reference_subject(expression, out);
            if let Some(owner) = subscript.object() {
                if !collect_subjects(&owner, out, depth + 1, remaining) {
                    return false;
                }
            }
        }
        ast::Expr::Compare(compare) => {
            for operand in compare.operands() {
                if !collect_subjects(&operand, out, depth + 1, remaining) {
                    return false;
                }
            }
        }
        _ => {}
    }
    true
}

/// `True`/`1` → `Some(true)`, `False`/`0` → `Some(false)`.
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

/// Cardinality of finite, nonexpanding builtin literals after normal evaluation.
/// Unknown/oversized shapes preserve the ordinary possibly-empty loop model.
fn literal_iterable_cardinality(expression: &ast::Expr) -> Option<usize> {
    fn count(elements: impl Iterator<Item = ast::Expr>) -> Option<usize> {
        let mut size = 0;
        for element in elements.take(257) {
            if matches!(element.unparen(), ast::Expr::Star(_)) || size == 256 {
                return None;
            }
            size += 1;
        }
        Some(size)
    }
    match expression.clone().unparen() {
        ast::Expr::Tuple(tuple) => count(tuple.elements()),
        ast::Expr::List(list) => count(list.elements()),
        ast::Expr::String(string) => {
            use rustpy_syntax::string_literal::{self, Decoded};
            if string.syntax().text().len() > TextSize::from(4096) {
                return None;
            }
            let raw = string.raw_text();
            let size = match string_literal::string(&raw).ok()? {
                Decoded::Text(text, _) => text.chars().count(),
                Decoded::Bytes(bytes) => bytes.len(),
                Decoded::SurrogateText(points, _) => points.len(),
            };
            (size <= 256).then_some(size)
        }
        _ => None,
    }
}

fn statically_empty_iterable(expression: &ast::Expr) -> bool {
    match expression.clone().unparen() {
        ast::Expr::Tuple(tuple) => tuple.elements().next().is_none(),
        ast::Expr::List(list) => list.elements().next().is_none(),
        ast::Expr::Set(set) => set.elements().next().is_none(),
        ast::Expr::Dict(dict) => dict.items().next().is_none() && dict.unpacks().next().is_none(),
        ast::Expr::String(_) => is_constant(expression) == Some(false),
        _ => false,
    }
}

fn is_constant(e: &ast::Expr) -> Option<bool> {
    match e.clone().unparen() {
        ast::Expr::Literal(l) => match l.kind()? {
            SyntaxKind::TRUE_KW => Some(true),
            SyntaxKind::FALSE_KW | SyntaxKind::NONE_KW => Some(false),
            SyntaxKind::INT_NUMBER => {
                let text = l.text();
                let digits = if text
                    .get(..2)
                    .is_some_and(|prefix| matches!(prefix, "0x" | "0X" | "0o" | "0O" | "0b" | "0B"))
                {
                    &text[2..]
                } else {
                    &text
                };
                // Truth requires no bounded machine-integer conversion: even
                // arbitrarily large valid integer tokens are zero iff every
                // digit is zero (underscores do not contribute a digit).
                Some(digits.bytes().any(|digit| digit != b'0' && digit != b'_'))
            }
            _ => None,
        },
        ast::Expr::String(string) => {
            use rustpy_syntax::string_literal::{self, Decoded};
            match string_literal::string(&string.raw_text()).ok()? {
                Decoded::Text(text, _) => Some(!text.is_empty()),
                Decoded::Bytes(bytes) => Some(!bytes.is_empty()),
                Decoded::SurrogateText(points, _) => Some(!points.is_empty()),
            }
        }
        ast::Expr::Tuple(tuple) if tuple.elements().next().is_none() => Some(false),
        ast::Expr::List(list) if list.elements().next().is_none() => Some(false),
        ast::Expr::Set(set) if set.elements().next().is_none() => Some(false),
        ast::Expr::Dict(dict)
            if dict.items().next().is_none() && dict.unpacks().next().is_none() =>
        {
            Some(false)
        }
        _ => None,
    }
}

/// Exceptions are captured before potentially failing operations, not from
/// future syntax. Literal name stores cannot fail before assigning the name.
fn statement_may_raise(statement: &ast::Stmt, local_annotations: bool) -> bool {
    fn literal(node: &SyntaxNode) -> bool {
        match node.kind() {
            SyntaxKind::LITERAL_EXPR | SyntaxKind::STRING_EXPR => true,
            SyntaxKind::PAREN_EXPR
            | SyntaxKind::TUPLE_EXPR
            | SyntaxKind::LIST_EXPR
            | SyntaxKind::CONCAT_STRING_EXPR => node.children().all(|child| literal(&child)),
            _ => false,
        }
    }
    match statement {
        ast::Stmt::Try(_)
        | ast::Stmt::If(_)
        | ast::Stmt::While(_)
        | ast::Stmt::Pass(_)
        | ast::Stmt::Global(_)
        | ast::Stmt::Nonlocal(_)
        | ast::Stmt::Break(_)
        | ast::Stmt::Continue(_) => false,
        ast::Stmt::Assign(assign) => {
            !(local_annotations
                && assign
                    .targets()
                    .iter()
                    .all(|target| matches!(target, ast::Expr::Name(_)))
                && assign.value().is_some_and(|value| literal(value.syntax())))
        }
        ast::Stmt::AnnAssign(assign) => {
            !(local_annotations
                && matches!(assign.target(), Some(ast::Expr::Name(_)))
                && assign.value().is_none_or(|value| literal(value.syntax())))
        }
        ast::Stmt::Assert(assertion) => {
            assertion.test().as_ref().and_then(is_constant) != Some(true)
        }
        ast::Stmt::Return(ret) => ret.value().is_some_and(|value| !literal(value.syntax())),
        ast::Stmt::Expr(expr) => expr.expr().is_some_and(|value| !literal(value.syntax())),
        _ => true,
    }
}

/// Patterns that match every subject (`_`, a capture, `x as y`, alternatives containing one).
fn is_irrefutable(p: &SyntaxNode) -> bool {
    match p.kind() {
        SyntaxKind::MATCH_WILDCARD_PATTERN | SyntaxKind::MATCH_CAPTURE_PATTERN => true,
        SyntaxKind::MATCH_AS_PATTERN => p
            .children()
            .find(|c| c.kind().is_pattern())
            .is_none_or(|c| is_irrefutable(&c)),
        SyntaxKind::MATCH_OR_PATTERN => p.children().any(|c| is_irrefutable(&c)),
        SyntaxKind::MATCH_GROUP_PATTERN => p.children().next().is_some_and(|c| is_irrefutable(&c)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use crate::db::Analysis;

    /// Names of the bindings (as `name@line`) reaching the `nth` load of `name`.
    fn reaching(src: &str, name: &str, nth: usize) -> Vec<String> {
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let file = db.file(id).unwrap();
        let loads: Vec<&crate::scope::Reference> = file
            .scopes
            .references
            .iter()
            .filter(|r| r.name == name && !r.is_write)
            .collect();
        let r = loads[nth];
        let scope = flow_scope(&file, r.scope).unwrap();
        let info = flow_of(&file, scope);
        let line = |off: TextSize| src[..usize::from(off)].matches('\n').count() + 1;
        let mut out: Vec<String> = info
            .refs
            .get(&r.range.start())
            .map(|rs| {
                rs.iter()
                    .map(|x| match x.binding {
                        Some(b) => format!(
                            "{}@{}{}",
                            name,
                            line(file.scopes.binding(b).range.start()),
                            if x.preds.is_empty() { "" } else { "?" }
                        ),
                        None => format!("outer{}", if x.preds.is_empty() { "" } else { "?" }),
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    }

    #[test]
    fn class_scopes_share_their_execution_scope_walk() {
        let src = "class A:\n    x = 1\nclass B:\n    class C:\n        y = x\n";
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let file = db.file(id).unwrap();
        let module = file.scopes.module_scope();
        let classes: Vec<ScopeId> = (0..file.scopes.scopes.len())
            .map(|index| ScopeId(index as u32))
            .filter(|scope| file.scopes.scope(*scope).kind == ScopeKind::Class)
            .collect();
        assert_eq!(classes.len(), 3);
        let shared = flow_of(&file, module);
        for class in &classes {
            assert!(Arc::ptr_eq(&shared, &flow_of(&file, *class)));
        }
        assert_eq!(file.flow.0.lock().len(), 1);
        let inferer = crate::infer::Inferer::new(&db);
        let first = inferer.scope_flow(&file, classes[0]);
        for class in &classes {
            assert!(Arc::ptr_eq(&first, &inferer.scope_flow(&file, *class)));
        }
        assert!(Arc::ptr_eq(&first, &inferer.scope_flow(&file, module)));
        assert_eq!(inferer.scope_flow_cache.borrow().len(), 1);
    }

    #[test]
    fn class_body_raises_still_reach_enclosing_handlers() {
        // Unobserved class-body raises are skipped; observed ones still flow.
        let src = "x = 1\ntry:\n    class A:\n        global x\n        x = 2\n        f()\n        x = 3\nexcept E:\n    x\nclass B:\n    global x\n    x = 4\n    g()\n    x = 5\nx\n";
        assert_eq!(reaching(src, "x", 0), vec!["x@1", "x@5"]);
        assert_eq!(reaching(src, "x", 1), vec!["x@14"]);
    }

    #[test]
    fn sequential_assignments_kill_earlier_ones() {
        let src = "parts = 'a,b'.split(',')\nparts.append('c')\nparts = ','.join(parts)\nparts\n";
        assert_eq!(reaching(src, "parts", 0), vec!["parts@1"]);
        // The right-hand side still sees the old binding.
        assert_eq!(reaching(src, "parts", 1), vec!["parts@1"]);
        assert_eq!(reaching(src, "parts", 2), vec!["parts@3"]);
    }

    #[test]
    fn branches_merge_and_early_exits_drop_paths() {
        let src = "def f(c):\n    x = None\n    if c:\n        x = 1\n    x\n    y = 1\n    if c:\n        y = 'a'\n        return\n    y\n";
        assert_eq!(reaching(src, "x", 0), vec!["x@2", "x@4"]);
        assert_eq!(reaching(src, "y", 0), vec!["y@6"]);
    }

    #[test]
    fn loops_carry_bindings_around() {
        let src = "x = 0\nfor i in range(3):\n    x\n    x = 'a'\nx\nwhile True:\n    z = 1\n    break\nz\n";
        assert_eq!(reaching(src, "x", 0), vec!["x@1", "x@4"]);
        assert_eq!(reaching(src, "x", 1), vec!["x@1", "x@4"]);
        assert_eq!(reaching(src, "z", 0), vec!["z@7"]);
    }

    #[test]
    fn try_handlers_and_del() {
        let src = "try:\n    v = int('1')\nexcept ValueError:\n    v = None\nv\nw = 1\ndel w\nw\n";
        assert_eq!(reaching(src, "v", 0), vec!["v@2", "v@4"]);
        assert!(reaching(src, "w", 0).is_empty());
    }

    #[test]
    fn conditions_narrow_outer_names_and_walrus_targets() {
        let src = "g = None\ndef f():\n    if g is not None:\n        g\n    if (m := len('')) and m:\n        m\n";
        assert_eq!(reaching(src, "g", 1), vec!["outer?"]);
        assert_eq!(reaching(src, "m", 1), vec!["m@5?"]);
    }

    #[test]
    fn comprehension_targets_shadow_and_class_scope_is_skipped() {
        let src = "x = 1\nys = [x for x in 'ab']\nx\n";
        // `x` inside the comprehension is its own target: not recorded for the module.
        assert!(reaching(src, "x", 0).is_empty());
        assert_eq!(reaching(src, "x", 1), vec!["x@1"]);
    }

    fn terminal_call_preserves_tail(src: &str, visited: bool) {
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let file = db.file(id).unwrap();
        let reference = file
            .scopes
            .references
            .iter()
            .find(|r| r.name == "marker" && !r.is_write)
            .unwrap();
        let scope = flow_scope(&file, reference.scope).unwrap();
        let flow = flow_of_with_db(&db, &file, scope);
        assert_eq!(
            flow.refs.contains_key(&reference.range.start()),
            visited,
            "{src}"
        );
        if visited {
            let reaches = &flow.refs[&reference.range.start()];
            assert_eq!(reaches.len(), 1);
            let binding = file.scopes.binding(reaches[0].binding.unwrap());
            assert_eq!(binding.name, "marker");
            assert!(matches!(
                binding.kind,
                crate::scope::BindingKind::Assignment
            ));
        }
    }

    #[test]
    fn local_exit_callable_does_not_terminate_flow() {
        terminal_call_preserves_tail(
            "def f(exit):\n    marker = 1\n    exit()\n    marker\n",
            true,
        );
        terminal_call_preserves_tail(
            "def f():\n    exit = lambda: None\n    marker = 1\n    exit()\n    marker\n",
            true,
        );
    }

    #[test]
    fn local_module_objects_do_not_terminate_flow() {
        for call in [
            "sys.exit()",
            "os._exit(0)",
            "os.abort()",
            "typing.assert_never(None)",
        ] {
            let src = format!("def f(sys, os, typing):\n    marker = 1\n    {call}\n    marker\n");
            terminal_call_preserves_tail(&src, true);
        }
    }

    #[test]
    fn rebound_import_does_not_terminate_flow() {
        terminal_call_preserves_tail(
            "def f(fake):\n    import sys\n    sys = fake\n    marker = 1\n    sys.exit()\n    marker\n",
            true,
        );
    }

    #[test]
    fn canonical_terminal_calls_still_drop_tail() {
        terminal_call_preserves_tail("def f():\n    marker = 1\n    exit()\n    marker\n", false);
        terminal_call_preserves_tail(
            "def f():\n    import sys\n    marker = 1\n    sys.exit()\n    marker\n",
            false,
        );
    }
}
