//! Type inference (roadmap §2.2 "Type inference", "Narrowing" basics).

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use indexmap::IndexMap;
use rustc_hash::FxHashMap;
use rustpy_core::{FileId, TextRange, TextSize};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::{SyntaxKind, SyntaxNode};
use smol_str::SmolStr;

use crate::classes::{c3_merge, ClassInfo, Member, MemberKind};
use crate::db::{Analysis, FileInfo};
use crate::scope::{BindingId, BindingKind, ScopeId, ScopeKind};
use crate::types::{ClassRef, FunctionRef, LiteralValue, Param, ParamKind, Signature, Type};

mod async_iteration;
mod await_results;
mod builtin_type_call;
mod class_decorators;
mod constructor_result;
mod dataclass_members;
mod enum_values;
mod expected_context;
mod function_decorators;
mod generic_defaults;
mod iteration_protocols;
mod library_returns;
mod member_protocols;
mod module_exports;
mod operator_results;
mod paramspec_contracts;
mod partial_helpers;
mod protocol_compat;
mod protocol_solving;
mod slice_results;
mod synth;
mod target_projection;
mod truthiness_results;
mod tuple_pack_contracts;
mod type_capabilities;
mod type_factories;
mod type_forms;
mod typevar_contracts;
mod yield_results;
pub(crate) use library_returns::{library_path, LibraryReturns};
pub use synth::{SynthKind, SynthSpec};

pub(crate) type ScopeFlowCache =
    FxHashMap<(FileId, ScopeId), (Arc<FileInfo>, u64, Arc<crate::flow::FlowInfo>)>;

/// Classes of an MRO (or a tail of one) with their type arguments.
pub type MroWithArgs = Vec<(ClassRef, Vec<Type>)>;

/// Where a resolved name lives.
#[derive(Clone, Debug)]
pub struct Definition {
    pub file: Arc<FileInfo>,
    pub binding: BindingId,
}

impl Definition {
    pub fn binding(&self) -> &crate::scope::Binding {
        self.file.scopes.binding(self.binding)
    }
    pub fn range(&self) -> TextRange {
        self.binding().range
    }
}

/// Lazily caching inference engine over an [`Analysis`] database.
#[derive(Copy, Clone, PartialEq, Eq, Hash)]
enum GuardKind {
    Expr,
    Binding,
    Annotation,
    Return,
    Constructor,
    FunctionDecorator,
}

/// A computation in progress (see [`Inferer::enter`]).
struct Frame {
    /// Lowest depth of an unfinished computation whose result this one used
    /// (a cut cycle or a provisional type); `usize::MAX` when none.
    low: usize,
    /// Provisional binding types that depend on this computation being unfinished.
    dependents: Vec<(FileId, TextRange)>,
}

type GuardKey = (FileId, TextRange, GuardKind);

#[cfg(test)]
thread_local! {
    /// Test-only: how many more binding types this thread may compute, so a
    /// test catches runaway recomputation quickly instead of hanging.
    pub(crate) static BINDING_BUDGET: std::cell::Cell<usize> = const { std::cell::Cell::new(usize::MAX) };
}
thread_local! {
    /// The token of the background job running on this thread (see
    /// [`with_cancellation`]): sessions started while it is set stop
    /// inferring once it is cancelled.
    static AMBIENT_TOKEN: RefCell<Option<rustpy_core::CancellationToken>> =
        const { RefCell::new(None) };
}

/// Runs `f` with `token` as the cancellation token of every inference
/// session it starts ([`Inferer::new`]): once the token is cancelled each new
/// guarded computation yields `Unknown`, so long inspection passes over a
/// large file end within milliseconds of the cancellation instead of
/// finishing their inference first. The caller discards the results of a
/// cancelled run (they are less precise), as it already must.
pub fn with_cancellation<R>(token: &rustpy_core::CancellationToken, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<rustpy_core::CancellationToken>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            AMBIENT_TOKEN.with(|t| *t.borrow_mut() = previous);
        }
    }
    let _restore = Restore(AMBIENT_TOKEN.with(|t| t.borrow_mut().replace(token.clone())));
    f()
}

/// Whether the token installed by [`with_cancellation`] on this thread is
/// cancelled (long loops outside inference sessions check it).
pub fn ambient_cancelled() -> bool {
    AMBIENT_TOKEN.with(|t| t.borrow().as_ref().is_some_and(|t| t.is_cancelled()))
}

pub static DBG_BINDINGS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static DBG_EXPRS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Reuse is confined to one inference session and immutable source/dependency
/// authority. Contextual and provisional computations never enter this store.
const MAX_BINARY_MEMO_ENTRIES: usize = 100_000;
struct BinaryMemo {
    file: Arc<FileInfo>,
    epoch: u64,
    values: FxHashMap<TextRange, Type>,
}

pub struct Inferer<'a> {
    pub db: &'a Analysis,
    /// Computations in progress (cycle detection), with their depth: their
    /// 1-based index in `frames`.
    guard: RefCell<FxHashMap<GuardKey, usize>>,
    /// One frame per computation in progress, innermost last.
    frames: RefCell<Vec<Frame>>,
    /// Binding types no unfinished computation contributed to.
    binding_cache: RefCell<FxHashMap<(FileId, TextRange), Type>>,
    binary_memo: RefCell<Option<BinaryMemo>>,
    #[cfg(test)]
    binary_applications: std::cell::Cell<usize>,
    expected_context: expected_context::ExpectedContext,
    /// Binding types computed inside a cycle, with the depth of the
    /// unfinished computation they read (the cycle's head): they hold while
    /// it runs and are dropped when it ends. Without them, every binding of
    /// a loop-carried cycle would be recomputed in every order (exponential).
    provisional: RefCell<FxHashMap<(FileId, TextRange), (usize, Type)>>,
    class_cache: RefCell<FxHashMap<ClassRef, Arc<ClassInfo>>>,
    mro_cache: RefCell<FxHashMap<ClassRef, Arc<Vec<ClassRef>>>>,
    sig_cache: RefCell<FxHashMap<FunctionRef, Arc<Signature>>>,
    synth_cache: RefCell<FxHashMap<ClassRef, Option<SynthSpec>>>,
    export_cache: RefCell<FxHashMap<FileId, Option<rustpy_syntax::exports::Exports>>>,
    enum_query_file: RefCell<Option<Arc<FileInfo>>>,
    member_protocol_depth: std::cell::Cell<usize>,
    functools_values: RefCell<FxHashMap<ClassRef, partial_helpers::FunctoolsValue>>,
    /// Per-consumer captured source/epoch identity, never mixed into FileInfo.
    /// Shared with [`Inferer::flow_session`] sessions (flow never infers).
    pub(crate) scope_flow_cache: Rc<RefCell<ScopeFlowCache>>,
    pub(crate) terminal_authority_cache: Rc<RefCell<Vec<Arc<crate::flow::CapturedAuthority>>>>,
    /// Interactive work (completion) stops inferring once this passes or the
    /// token is cancelled: see [`Inferer::with_budget`].
    budget: Option<(std::time::Instant, Option<rustpy_core::CancellationToken>)>,
    spent: std::cell::Cell<bool>,
}

/// A deadline that never passes (a budget bounded by its token only).
fn far_future() -> std::time::Instant {
    let now = std::time::Instant::now();
    now.checked_add(std::time::Duration::from_secs(10 * 365 * 24 * 3600))
        .unwrap_or(now)
}

const BUILTIN_SPECIAL: &[&str] = &[
    "Any",
    "Union",
    "Optional",
    "List",
    "Dict",
    "Set",
    "FrozenSet",
    "Tuple",
    "Type",
    "Callable",
    "Literal",
    "Final",
    "ClassVar",
    "Annotated",
    "TypeAlias",
    "Self",
    "Never",
    "NoReturn",
    "Required",
    "NotRequired",
    "ReadOnly",
    "TypeGuard",
    "TypeIs",
    "Unpack",
    "Concatenate",
    "LiteralString",
    "DefaultDict",
    "OrderedDict",
    "Counter",
    "Deque",
    "ChainMap",
    "Awaitable",
    "Coroutine",
];

impl<'a> Inferer<'a> {
    /// A fresh inference session (no type caches, overrides or budget) that
    /// shares this Inferer's flow caches. Flow results never depend on
    /// inference state; without sharing, every isolated expected-type query
    /// re-walked its whole scope, which is quadratic in module size.
    pub(crate) fn flow_session(&self) -> Inferer<'a> {
        let mut session = Inferer::new(self.db);
        session.scope_flow_cache = Rc::clone(&self.scope_flow_cache);
        session.terminal_authority_cache = Rc::clone(&self.terminal_authority_cache);
        session
    }

    pub fn new(db: &'a Analysis) -> Self {
        Self {
            db,
            guard: RefCell::new(FxHashMap::default()),
            frames: RefCell::new(Vec::new()),
            binding_cache: RefCell::new(FxHashMap::default()),
            binary_memo: RefCell::new(None),
            #[cfg(test)]
            binary_applications: std::cell::Cell::new(0),
            expected_context: expected_context::ExpectedContext::default(),
            provisional: RefCell::new(FxHashMap::default()),
            class_cache: RefCell::new(FxHashMap::default()),
            mro_cache: RefCell::new(FxHashMap::default()),
            sig_cache: RefCell::new(FxHashMap::default()),
            synth_cache: RefCell::new(FxHashMap::default()),
            export_cache: RefCell::new(FxHashMap::default()),
            enum_query_file: RefCell::new(None),
            member_protocol_depth: std::cell::Cell::new(0),
            functools_values: RefCell::new(FxHashMap::default()),
            scope_flow_cache: Rc::new(RefCell::new(FxHashMap::default())),
            terminal_authority_cache: Rc::new(RefCell::new(Vec::new())),
            // A background job's token (no deadline): see `with_cancellation`.
            budget: AMBIENT_TOKEN
                .with(|t| t.borrow().clone())
                .map(|token| (far_future(), Some(token))),
            spent: std::cell::Cell::new(false),
        }
    }

    /// Bounds this session's inference: once `deadline` passes (or `token` is
    /// cancelled) every new guarded computation yields `Unknown` as if it were
    /// a cut cycle, so the session ends quickly with less precise types.
    /// Results computed after that point never reach the shared library
    /// return cache.
    pub fn with_budget(
        mut self,
        deadline: std::time::Instant,
        token: Option<rustpy_core::CancellationToken>,
    ) -> Self {
        // An explicit token wins; else the session keeps the ambient one.
        let token = token.or_else(|| self.budget.take().and_then(|(_, ambient)| ambient));
        self.budget = Some((deadline, token));
        self
    }

    /// Whether the budget given to [`Inferer::with_budget`] is spent.
    pub fn budget_spent(&self) -> bool {
        if self.spent.get() {
            return true;
        }
        let Some((deadline, token)) = &self.budget else {
            return false;
        };
        let spent = std::time::Instant::now() >= *deadline
            || token.as_ref().is_some_and(|t| t.is_cancelled());
        self.spent.set(spent);
        spent
    }

    // ----- builtins helpers ---------------------------------------------------

    pub fn builtin_class(&self, name: &str) -> Option<ClassRef> {
        let b = self.db.builtins()?;
        let scope = b.scopes.module_scope();
        let id = *b
            .scopes
            .bindings_in(scope, name)
            .iter()
            .find(|id| matches!(b.scopes.binding(**id).kind, BindingKind::Class))?;
        let binding = b.scopes.binding(id);
        Some(ClassRef {
            file: b.id,
            range: binding.node_range,
            name: SmolStr::new(name),
        })
    }

    pub fn instance_of(&self, name: &str) -> Type {
        self.builtin_class(name)
            .map(|c| Type::Instance(c, Vec::new()))
            .unwrap_or(Type::Unknown)
    }

    fn typing_class(&self, name: &str) -> Option<ClassRef> {
        let m = self.db.module("typing")?;
        let scope = m.scopes.module_scope();
        let id = *m
            .scopes
            .bindings_in(scope, name)
            .iter()
            .find(|id| matches!(m.scopes.binding(**id).kind, BindingKind::Class))?;
        Some(ClassRef {
            file: m.id,
            range: m.scopes.binding(id).node_range,
            name: SmolStr::new(name),
        })
    }

    // ----- name resolution ----------------------------------------------------

    /// Resolves `name` seen from `scope` in `file`, falling back to star
    /// imports and builtins.
    pub fn resolve_name(
        &self,
        file: &Arc<FileInfo>,
        scope: ScopeId,
        name: &str,
    ) -> Option<Definition> {
        self.resolve_name_inner(file, scope, name, 0)
    }

    fn resolve_name_inner(
        &self,
        file: &Arc<FileInfo>,
        scope: ScopeId,
        name: &str,
        depth: usize,
    ) -> Option<Definition> {
        if depth > 8 {
            return None;
        }
        if let Some(ids) = file.scopes.resolve(scope, name) {
            if let Some(id) = ids.iter().rev().find(|id| {
                file.scopes.binding(**id).is_definition()
                    && self.import_fallback_binding_is_live(file, **id)
            }) {
                return Some(Definition {
                    file: file.clone(),
                    binding: *id,
                });
            }
        }
        // Star imports in the module scope (and enclosing scopes).
        let mut cur = Some(scope);
        while let Some(s) = cur {
            let sc = file.scopes.scope(s);
            for star in &sc.star_imports {
                if let Some(m) =
                    self.db
                        .module_relative(file, star.level, star.module.trim_start_matches('.'))
                {
                    if !self.star_import_allows(&m, name) {
                        continue;
                    }
                    if let Some(def) = self.resolve_in_module(&m, name, depth + 1) {
                        return Some(def);
                    }
                }
            }
            cur = sc.parent;
        }
        if name == "reveal_type" {
            return None;
        }
        let b = self.db.builtins()?;
        if Arc::ptr_eq(&b, file) {
            return None;
        }
        self.resolve_in_module(&b, name, depth + 1)
    }

    /// Resolves a name exported by a module (module scope, then its star imports).
    pub fn resolve_in_module(
        &self,
        module: &Arc<FileInfo>,
        name: &str,
        depth: usize,
    ) -> Option<Definition> {
        let scope = module.scopes.module_scope();
        let ids = module.scopes.bindings_in(scope, name);
        if let Some(id) = ids.iter().rev().find(|id| {
            module.scopes.binding(**id).is_definition()
                && self.import_fallback_binding_is_live(module, **id)
        }) {
            return Some(Definition {
                file: module.clone(),
                binding: *id,
            });
        }
        if depth > 8 {
            return None;
        }
        for star in &module.scopes.scope(scope).star_imports {
            if let Some(m) =
                self.db
                    .module_relative(module, star.level, star.module.trim_start_matches('.'))
            {
                if !self.star_import_allows(&m, name) {
                    continue;
                }
                if let Some(def) = self.resolve_in_module(&m, name, depth + 1) {
                    return Some(def);
                }
            }
        }
        None
    }

    /// Selects only statically decidable import-only try/except paths. Arbitrary
    /// executable bodies and unknown exception handlers retain all bindings.
    fn import_fallback_binding_is_live(&self, file: &FileInfo, id: BindingId) -> bool {
        let binding = file.scopes.binding(id);
        let root = file.parse.syntax_node();
        let start = match root.covering_element(binding.node_range) {
            rustpy_syntax::rowan::NodeOrToken::Node(node) => node,
            rustpy_syntax::rowan::NodeOrToken::Token(token) => {
                let Some(parent) = token.parent() else {
                    return true;
                };
                parent
            }
        };
        for node in start.ancestors() {
            let Some(statement) = ast::TryStmt::cast(node) else {
                continue;
            };
            let Some(body) = statement.body() else {
                continue;
            };
            let handlers: Vec<_> = statement.except_clauses().collect();
            if handlers.len() != 1 || handlers[0].is_star() {
                continue;
            }
            let Some(ast::Expr::Name(exception)) = handlers[0].exception_type() else {
                continue;
            };
            let exception_name = exception.name_text();
            if !matches!(
                exception_name.as_str(),
                "ImportError" | "ModuleNotFoundError"
            ) || file
                .scopes
                .resolve(binding.scope, &exception_name)
                .is_some()
            {
                continue;
            }
            let statements: Vec<_> = body.statements().collect();
            if statements.is_empty() {
                continue;
            }
            // Validate the whole syntax before using sequential import authority.
            if statements
                .iter()
                .any(|s| !matches!(s, ast::Stmt::Import(_) | ast::Stmt::ImportFrom(_)))
            {
                continue;
            }
            let mut failure: Option<TextSize> = None;
            let mut decided = true;
            for import in statements {
                if failure.is_some() {
                    break;
                }
                match import {
                    ast::Stmt::Import(import) => {
                        for alias in import.aliases() {
                            if self.db.module_from(file, &alias.name()).is_none() {
                                failure = Some(alias.text_range().start());
                                break;
                            }
                        }
                    }
                    ast::Stmt::ImportFrom(import)
                        if !import.is_star() && exception_name == "ImportError" =>
                    {
                        let module_name = import.module_path();
                        let Some(module) = self.db.module_relative(
                            file,
                            import.level(),
                            module_name.trim_start_matches('.'),
                        ) else {
                            failure = Some(import.text_range().start());
                            break;
                        };
                        for alias in import.aliases() {
                            let name = alias.name();
                            let definitions = module
                                .scopes
                                .bindings_in(module.scopes.module_scope(), &name);
                            if !definitions.is_empty() {
                                // A re-export might itself fail. Do not assume that
                                // syntactic binding existence proves import success.
                                if definitions.iter().any(|id| {
                                    matches!(
                                        module.scopes.binding(*id).kind,
                                        BindingKind::Import(_)
                                    )
                                }) {
                                    decided = false;
                                    break;
                                }
                                continue;
                            }
                            if !module
                                .scopes
                                .bindings_in(module.scopes.module_scope(), "__getattr__")
                                .is_empty()
                                || !module
                                    .scopes
                                    .scope(module.scopes.module_scope())
                                    .star_imports
                                    .is_empty()
                            {
                                decided = false;
                                break;
                            }
                            let full = if module_name.trim_start_matches('.').is_empty() {
                                name
                            } else {
                                format!("{}.{}", module_name.trim_start_matches('.'), name)
                            };
                            if self
                                .db
                                .module_relative(file, import.level(), &full)
                                .is_none()
                            {
                                failure = Some(alias.text_range().start());
                                break;
                            }
                        }
                    }
                    _ => {
                        decided = false;
                        break;
                    }
                }
            }
            if !decided {
                continue;
            }
            if body.text_range().contains_range(binding.node_range)
                && failure.is_some_and(|start| binding.node_range.start() >= start)
            {
                return false;
            }
            if handlers[0].text_range().contains_range(binding.node_range) && failure.is_none() {
                return false;
            }
            if statement
                .else_clause()
                .and_then(|e| e.body())
                .is_some_and(|b| b.text_range().contains_range(binding.node_range))
                && failure.is_some()
            {
                return false;
            }
        }
        true
    }

    /// Follows import bindings to the ultimate definition (for navigation).
    pub fn follow_imports(&self, def: Definition) -> Definition {
        let mut cur = def;
        for _ in 0..16 {
            let b = cur.binding().clone();
            let BindingKind::Import(info) = &b.kind else {
                return cur;
            };
            let Some(name) = &info.name else {
                // `import a.b` → the module file itself.
                return cur;
            };
            let Some(module) =
                self.db
                    .module_relative(&cur.file, info.level, info.module.trim_start_matches('.'))
            else {
                return cur;
            };
            match self.resolve_in_module(&module, name, 0) {
                Some(next) => cur = next,
                None => return cur,
            }
        }
        cur
    }

    /// The module file an import binding points to (for `import x` / `from x import y`).
    pub fn import_target_module(&self, def: &Definition) -> Option<Arc<FileInfo>> {
        let b = def.binding();
        let BindingKind::Import(info) = &b.kind else {
            return None;
        };
        let module = info.module.trim_start_matches('.');
        if let Some(name) = &info.name {
            let m = self.db.module_relative(&def.file, info.level, module)?;
            if self.resolve_in_module(&m, name, 0).is_some() {
                return Some(m);
            }
            let full = if module.is_empty() {
                name.to_string()
            } else {
                format!("{module}.{name}")
            };
            return self.db.module_relative(&def.file, info.level, &full);
        }
        let first = module.split('.').next().unwrap_or(module);
        self.db.module_from(&def.file, first)
    }

    // ----- binding types -----------------------------------------------------

    /// Starts the computation `key`. `false` when it is already in progress:
    /// a cycle, which the caller cuts (usually with an unknown type).
    fn enter(&self, key: GuardKey) -> bool {
        if self.budget.is_some() && self.budget_spent() {
            // Everything still running depends on this cut: none of it is final.
            self.note_cycle(1);
            return false;
        }
        let mut guard = self.guard.borrow_mut();
        if let Some(&depth) = guard.get(&key) {
            drop(guard);
            self.note_cycle(depth);
            return false;
        }
        let mut frames = self.frames.borrow_mut();
        frames.push(Frame {
            low: usize::MAX,
            dependents: Vec::new(),
        });
        guard.insert(key, frames.len());
        true
    }

    /// Ends the computation `key` started by [`Self::enter`]. Returns the
    /// depth of the outermost unfinished computation its result used, when
    /// that is an enclosing one (the result is then incomplete until it ends).
    fn leave(&self, key: GuardKey) -> Option<usize> {
        self.guard.borrow_mut().remove(&key);
        let mut frames = self.frames.borrow_mut();
        let depth = frames.len();
        let frame = frames.pop()?;
        if !frame.dependents.is_empty() {
            let mut provisional = self.provisional.borrow_mut();
            for k in &frame.dependents {
                if provisional.get(k).is_some_and(|(head, _)| *head == depth) {
                    provisional.remove(k);
                }
            }
        }
        if frame.low >= depth {
            return None;
        }
        if let Some(parent) = frames.last_mut() {
            parent.low = parent.low.min(frame.low);
        }
        Some(frame.low)
    }

    /// The depth of the computation of binding `id` when it is in progress.
    pub(crate) fn binding_in_progress(&self, file: &FileInfo, id: BindingId) -> Option<usize> {
        self.guard
            .borrow()
            .get(&(file.id, file.scopes.binding(id).range, GuardKind::Binding))
            .copied()
    }

    /// Notes that the current computation used the unfinished result of the
    /// one at `depth` (a cut cycle).
    pub(crate) fn note_cycle(&self, depth: usize) {
        if let Some(frame) = self.frames.borrow_mut().last_mut() {
            frame.low = frame.low.min(depth);
        }
    }

    pub fn type_of_binding(&self, file: &Arc<FileInfo>, id: BindingId) -> Type {
        if let Some(ty) = self
            .expected_context
            .overrides
            .borrow()
            .get(&(file.id, id))
            .cloned()
        {
            return ty;
        }
        let b = file.scopes.binding(id);
        let cache_key = (file.id, b.range);
        if let Some(t) = self.binding_cache.borrow().get(&cache_key) {
            return t.clone();
        }
        let provisional = self.provisional.borrow().get(&cache_key).cloned();
        if let Some((head, t)) = provisional {
            self.note_cycle(head);
            return t;
        }
        let b = b.clone();
        let key = (file.id, b.range, GuardKind::Binding);
        if !self.enter(key) {
            return Type::Unknown;
        }
        crate::infer::DBG_BINDINGS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        #[cfg(test)]
        BINDING_BUDGET.with(|left| {
            assert!(
                left.get() > 0,
                "binding type computations over the test budget"
            );
            left.set(left.get() - 1);
        });
        let ty = self.type_of_binding_inner(file, id, &b);
        match self.leave(key) {
            // No cycle leads outside this computation: final.
            None => {
                self.binding_cache
                    .borrow_mut()
                    .insert(cache_key, ty.clone());
            }
            // Read an enclosing unfinished computation: valid until it ends.
            Some(head) => {
                self.provisional
                    .borrow_mut()
                    .insert(cache_key, (head, ty.clone()));
                if let Some(frame) = self.frames.borrow_mut().get_mut(head - 1) {
                    frame.dependents.push(cache_key);
                }
            }
        }
        ty
    }

    fn type_of_binding_inner(
        &self,
        file: &Arc<FileInfo>,
        id: BindingId,
        b: &crate::scope::Binding,
    ) -> Type {
        if !self.import_fallback_binding_is_live(file, id) {
            return Type::Never;
        }
        match &b.kind {
            BindingKind::Import(info) => {
                let module = info.module.trim_start_matches('.');
                match &info.name {
                    None => {
                        let first = module.split('.').next().unwrap_or(module);
                        self.db
                            .module(first)
                            .map(|m| Type::Module(m.id))
                            .unwrap_or(Type::Unknown)
                    }
                    Some(name) => {
                        let Some(m) = self.db.module_relative(file, info.level, module) else {
                            return Type::Unknown;
                        };
                        let attr = self
                            .resolve_in_module(&m, name, 0)
                            .map(|def| self.type_of_binding(&def.file, def.binding));
                        if let Some(t) = &attr {
                            if !t.is_unknown() {
                                return t.clone();
                            }
                        }
                        let full = if module.is_empty() {
                            name.to_string()
                        } else {
                            format!("{module}.{name}")
                        };
                        match self.db.module_relative(file, info.level, &full) {
                            Some(sub) => Type::Module(sub.id),
                            None => attr.unwrap_or(Type::Unknown),
                        }
                    }
                }
            }
            BindingKind::Class => self.decorated_class_binding(
                file,
                ClassRef {
                    file: file.id,
                    range: b.node_range,
                    name: b.name.clone(),
                },
            ),
            BindingKind::Function => {
                let root = file.parse.syntax_node();
                // Collect every `def` of this name in the scope; @overload sets become Overloaded.
                let defs: Vec<ast::FuncDef> = file
                    .scopes
                    .bindings_in(b.scope, &b.name)
                    .iter()
                    .filter(|x| matches!(file.scopes.binding(**x).kind, BindingKind::Function))
                    .filter_map(|x| {
                        node_with_range(&root, file.scopes.binding(*x).node_range, |n| {
                            n.kind() == SyntaxKind::FUNC_DEF
                        })
                        .and_then(ast::FuncDef::cast)
                    })
                    .collect();
                let overloads: Vec<&ast::FuncDef> = defs
                    .iter()
                    .filter(|f| f.has_decorator("overload") || f.has_decorator("typing.overload"))
                    .collect();
                if overloads.len() > 1 {
                    let class = self.enclosing_class(file, b.scope);
                    if overloads
                        .iter()
                        .any(|function| self.has_replacement_function_decorator(file, function))
                    {
                        return self
                            .decorated_overload_signatures(file, &overloads, class.as_ref(), false)
                            .map(Type::Overloaded)
                            .unwrap_or(Type::Unknown);
                    }
                    return Type::Overloaded(
                        overloads
                            .iter()
                            .map(|f| self.signature(file, f, class.as_ref()))
                            .collect(),
                    );
                }
                let node =
                    node_with_range(&root, b.node_range, |n| n.kind() == SyntaxKind::FUNC_DEF)
                        .and_then(ast::FuncDef::cast);
                match node {
                    Some(f) => self.function_type(file, &f, None),
                    None => Type::Unknown,
                }
            }
            BindingKind::Param | BindingKind::StarParam | BindingKind::StarStarParam => {
                self.param_type(file, id, b)
            }
            BindingKind::Assignment | BindingKind::Walrus => {
                // `x: Optional[str] = None` declares more than its value shows.
                if let Some(a) = b.annotation {
                    let declared = self.annotation_type_range(file, a);
                    if !declared.is_unknown() {
                        return declared;
                    }
                }
                if let Some(declared) = self.stub_binding_type(file, b) {
                    return declared;
                }
                if let Some(factory) = self.type_factory_binding(file, b) {
                    return factory;
                }
                match b.value {
                    Some(v) => self.literal_string_assignment(file, b, self.type_of_range(file, v)),
                    None => self.unpacked_target_type(file, b),
                }
            }
            BindingKind::Annotation => b
                .annotation
                .map(|a| self.annotation_type_range(file, a))
                .unwrap_or(Type::Unknown),
            BindingKind::AugAssignment => {
                let old = match self.aug_assign_read_type(file, b) {
                    Some(t) => t,
                    None => {
                        let ids = file.scopes.bindings_in(b.scope, &b.name);
                        let prev = ids
                            .iter()
                            .rev()
                            .find(|x| {
                                file.scopes.binding(**x).range.start() < b.range.start()
                                    && file.scopes.binding(**x).is_definition()
                            })
                            .copied();
                        prev.map(|p| self.type_of_binding(file, p))
                            .unwrap_or(Type::Unknown)
                    }
                };
                self.aug_assign_result(file, b, old)
            }
            BindingKind::For | BindingKind::CompTarget => self.iteration_target_type(file, b),
            BindingKind::With => self.structural_target_type(file, b),
            BindingKind::Except => match b.value {
                Some(v) => match self.type_of_range(file, v) {
                    Type::Class(c, args) => Type::Instance(c, args),
                    Type::Tuple(ts) => Type::union(
                        ts.into_iter()
                            .map(|t| match t {
                                Type::Class(c, a) => Type::Instance(c, a),
                                o => o,
                            })
                            .collect(),
                    ),
                    _ => Type::Unknown,
                },
                None => Type::Unknown,
            },
            BindingKind::TypeAlias => b
                .value
                .map(|v| self.annotation_type_range(file, v))
                .unwrap_or(Type::Unknown),
            BindingKind::TypeParam => self.declared_type_parameter(file, b),
            BindingKind::MatchCapture => self.match_capture_type(file, b),
            BindingKind::Global | BindingKind::Nonlocal | BindingKind::Del => Type::Unknown,
        }
    }

    /// Type of an element of a tuple-unpacking target (`a, b = value`).
    fn unpacked_target_type(&self, file: &Arc<FileInfo>, b: &crate::scope::Binding) -> Type {
        self.structural_target_type(file, b)
    }

    fn index_unpack(&self, ty: &Type, idx: usize) -> Type {
        match ty {
            Type::Tuple(ts) => ts.get(idx).cloned().unwrap_or(Type::Unknown),
            other => self.iter_type(other),
        }
    }

    fn param_type(&self, file: &Arc<FileInfo>, id: BindingId, b: &crate::scope::Binding) -> Type {
        if let Some(a) = b.annotation {
            let t = self.annotation_type_range(file, a);
            return match b.kind {
                BindingKind::StarParam => Type::Instance(
                    self.builtin_class("tuple").unwrap_or_else(unknown_class),
                    vec![t],
                ),
                BindingKind::StarStarParam => Type::Instance(
                    self.builtin_class("dict").unwrap_or_else(unknown_class),
                    vec![self.instance_of("str"), t],
                ),
                _ => t,
            };
        }
        if let Some(t) = self.contextual_lambda_parameter(file, b) {
            return t;
        }
        let scope = file.scopes.scope(b.scope);
        let function = file
            .parse
            .syntax_node()
            .token_at_offset(b.range.start())
            .right_biased()
            .and_then(|token| {
                token
                    .parent_ancestors()
                    .take(64)
                    .find_map(ast::FuncDef::cast)
            });
        if let Some(t) = function
            .as_ref()
            .and_then(|function| self.stub_parameter_type(file, function, &b.name))
        {
            return match b.kind {
                BindingKind::StarParam => Type::Instance(
                    self.builtin_class("tuple").unwrap_or_else(unknown_class),
                    vec![t],
                ),
                BindingKind::StarStarParam => Type::Instance(
                    self.builtin_class("dict").unwrap_or_else(unknown_class),
                    vec![self.instance_of("str"), t],
                ),
                _ => t,
            };
        }
        // First parameter of a method: self / cls.
        if b.kind == BindingKind::Param && scope.kind == ScopeKind::Function {
            let first = scope.bindings.values().flatten().next().copied();
            if first == Some(id) {
                if let Some(class) = self.enclosing_class(file, b.scope) {
                    let func = file
                        .parse
                        .syntax_node()
                        .covering_element(scope.range)
                        .into_node()
                        .and_then(ast::FuncDef::cast);
                    let decos: Vec<String> = func
                        .iter()
                        .flat_map(|f| {
                            f.decorators()
                                .filter_map(|d| d.name_text())
                                .collect::<Vec<_>>()
                        })
                        .collect();
                    if decos.iter().any(|d| d == "staticmethod") {
                        return Type::Unknown;
                    }
                    let cref = class;
                    if decos.iter().any(|d| d == "classmethod")
                        || func.as_ref().is_some_and(|f| {
                            matches!(
                                f.name_text().as_str(),
                                "__new__" | "__init_subclass__" | "__class_getitem__"
                            )
                        })
                    {
                        return Type::Class(cref, Vec::new());
                    }
                    let args = self.class_parameter_types(&cref);
                    return Type::Instance(cref, args);
                }
            }
        }
        match b.kind {
            BindingKind::StarParam => Type::Instance(
                self.builtin_class("tuple").unwrap_or_else(unknown_class),
                vec![Type::Unknown],
            ),
            BindingKind::StarStarParam => Type::Instance(
                self.builtin_class("dict").unwrap_or_else(unknown_class),
                vec![self.instance_of("str"), Type::Unknown],
            ),
            _ => match b.value {
                Some(v) => {
                    let t = self.widen(self.type_of_range(file, v));
                    if t.is_none() {
                        self.runtime_param_type(file, b.scope, &b.name)
                            .unwrap_or(Type::Unknown)
                    } else {
                        t
                    }
                }
                None => self
                    .runtime_param_type(file, b.scope, &b.name)
                    .unwrap_or(Type::Unknown),
            },
        }
    }

    /// Python-style `__qualname__` of the function owning `scope`.
    fn scope_qualname(&self, file: &Arc<FileInfo>, scope: ScopeId) -> Option<String> {
        let s = file.scopes.scope(scope);
        if s.kind != ScopeKind::Function {
            return None;
        }
        let mut parts = vec![s.name.clone()?.to_string()];
        let mut parent = s.parent;
        while let Some(p) = parent {
            let ps = file.scopes.scope(p);
            match ps.kind {
                ScopeKind::Class => parts.push(ps.name.clone()?.to_string()),
                ScopeKind::Function => parts.push(format!("{}.<locals>", ps.name.clone()?)),
                ScopeKind::TypeParams => {}
                _ => break,
            }
            parent = ps.parent;
        }
        parts.reverse();
        Some(parts.join("."))
    }

    /// Type of an unannotated parameter observed at run time, if collected.
    fn runtime_param_type(&self, file: &Arc<FileInfo>, scope: ScopeId, name: &str) -> Option<Type> {
        let rt = self.db.runtime_types();
        if rt.is_empty() || file.is_stub {
            return None;
        }
        let qual = self.scope_qualname(file, scope)?;
        let names = rt.function(&file.path, &qual)?.params.get(name)?.clone();
        self.type_from_runtime_names(&names)
    }

    /// Return type observed at run time for function `f`, if collected.
    fn runtime_return_type(&self, file: &Arc<FileInfo>, f: &ast::FuncDef) -> Option<Type> {
        let rt = self.db.runtime_types();
        if rt.is_empty() {
            return None;
        }
        let range = f.syntax().text_range();
        let mut cur = Some(
            file.scopes
                .scope_at(f.body()?.syntax().text_range().start()),
        );
        let mut scope = None;
        while let Some(sid) = cur {
            let sc = file.scopes.scope(sid);
            if sc.kind == ScopeKind::Function && sc.range == range {
                scope = Some(sid);
                break;
            }
            cur = sc.parent;
        }
        let scope = scope?;
        let qual = self.scope_qualname(file, scope)?;
        let names = rt.function(&file.path, &qual)?.returns.clone();
        self.type_from_runtime_names(&names)
    }

    fn type_from_runtime_names(&self, names: &[String]) -> Option<Type> {
        let mut types = Vec::new();
        for n in names {
            let t = match n.as_str() {
                "NoneType" => Type::None,
                simple if !simple.contains('.') => self.instance_of(simple),
                dotted => {
                    let (module, class) = dotted.rsplit_once('.')?;
                    let m = self.db.module(module);
                    m.and_then(|m| {
                        let scope = m.scopes.module_scope();
                        let id =
                            *m.scopes.bindings_in(scope, class).iter().find(|id| {
                                matches!(m.scopes.binding(**id).kind, BindingKind::Class)
                            })?;
                        Some(Type::Instance(
                            ClassRef {
                                file: m.id,
                                range: m.scopes.binding(id).node_range,
                                name: SmolStr::new(class),
                            },
                            Vec::new(),
                        ))
                    })
                    .unwrap_or(Type::Unknown)
                }
            };
            if !matches!(t, Type::Unknown) && !types.contains(&t) {
                types.push(t);
            }
        }
        match types.len() {
            0 => None,
            1 => types.pop(),
            _ => Some(Type::union(types)),
        }
    }

    /// The class whose body directly contains function scope `scope`.
    fn enclosing_class(&self, file: &Arc<FileInfo>, scope: ScopeId) -> Option<ClassRef> {
        let mut parent = file.scopes.scope(scope).parent;
        while let Some(p) = parent {
            let s = file.scopes.scope(p);
            match s.kind {
                ScopeKind::Class => {
                    return Some(ClassRef {
                        file: file.id,
                        range: s.range,
                        name: s.name.clone().unwrap_or_default(),
                    })
                }
                ScopeKind::TypeParams => parent = s.parent,
                _ => return None,
            }
        }
        None
    }

    /// The implicit `__class__` cell seen from `scope`: the class whose body
    /// directly defines the innermost enclosing method (`type[C]`).
    pub(crate) fn implicit_class_cell(&self, file: &Arc<FileInfo>, scope: ScopeId) -> Option<Type> {
        let mut current = Some(scope);
        while let Some(id) = current {
            let s = file.scopes.scope(id);
            match s.kind {
                ScopeKind::Function => {
                    if let Some(class) = self.enclosing_class(file, id) {
                        return Some(Type::Class(class, Vec::new()));
                    }
                }
                ScopeKind::Lambda | ScopeKind::Comprehension | ScopeKind::TypeParams => {}
                ScopeKind::Class | ScopeKind::Module => return None,
            }
            current = s.parent;
        }
        None
    }

    /// Literal types widen to their classes for variable inference.
    pub(crate) fn widen(&self, t: Type) -> Type {
        match t {
            Type::Literal(LiteralValue::Int(_)) => self.instance_of("int"),
            Type::Literal(LiteralValue::Str(_)) | Type::LiteralStringValue => {
                self.instance_of("str")
            }
            Type::Literal(LiteralValue::Bytes(_)) => self.instance_of("bytes"),
            Type::Literal(LiteralValue::Bool(_)) => self.instance_of("bool"),
            Type::Union(ts) => Type::union(ts.into_iter().map(|t| self.widen(t)).collect()),
            o => o,
        }
    }

    // ----- expressions ----------------------------------------------------------

    fn type_of_range(&self, file: &Arc<FileInfo>, range: TextRange) -> Type {
        let root = file.parse.syntax_node();
        let node = node_with_range(&root, range, |a| ast::Expr::can_cast(a.kind()));
        match node.and_then(ast::Expr::cast) {
            Some(e) => self.type_of_expr(file, &e),
            None => Type::Unknown,
        }
    }

    fn binary_memo_epoch(&self, file: &Arc<FileInfo>) -> Option<u64> {
        if self.expected_context.active.get()
            || !self.expected_context.overrides.borrow().is_empty()
            || self.member_protocol_depth.get() != 0
            || self
                .db
                .file(file.id)
                .is_none_or(|current| !Arc::ptr_eq(&current, file))
        {
            return None;
        }
        Some(self.db.revision_excluding(&file.path))
    }

    fn memoized_binary(&self, file: &Arc<FileInfo>, epoch: u64, range: TextRange) -> Option<Type> {
        if self.binary_memo_epoch(file) != Some(epoch) {
            return None;
        }
        let memo = self.binary_memo.borrow();
        let entry = memo.as_ref()?;
        if !Arc::ptr_eq(&entry.file, file) || entry.epoch != epoch {
            return None;
        }
        entry.values.get(&range).cloned()
    }

    fn remember_binary(&self, file: &Arc<FileInfo>, epoch: u64, range: TextRange, ty: Type) {
        if self.binary_memo_epoch(file) != Some(epoch) {
            return;
        }
        let mut memo = self.binary_memo.borrow_mut();
        if memo
            .as_ref()
            .is_none_or(|entry| !Arc::ptr_eq(&entry.file, file) || entry.epoch != epoch)
        {
            *memo = Some(BinaryMemo {
                file: file.clone(),
                epoch,
                values: FxHashMap::default(),
            });
        }
        let entry = memo.as_mut().expect("binary memo authority initialized");
        // Retain only one captured file and at most this many expressions.
        // Exhaustion changes reuse only; it never changes an inferred result.
        if entry.values.len() < MAX_BINARY_MEMO_ENTRIES {
            entry.values.insert(range, ty);
        }
    }

    pub fn type_of_expr(&self, file: &Arc<FileInfo>, expr: &ast::Expr) -> Type {
        if let Some(expected) = self.parent_expected_type(file, expr) {
            return self.type_of_expr_with_expected(file, expr, &expected);
        }
        let memo_epoch = (matches!(expr, ast::Expr::Binary(_)) && self.frames.borrow().is_empty())
            .then(|| self.binary_memo_epoch(file))
            .flatten();
        if let Some(ty) =
            memo_epoch.and_then(|epoch| self.memoized_binary(file, epoch, expr.text_range()))
        {
            return ty;
        }
        if self.frames.borrow().is_empty() {
            self.settle_loop_variables(file, expr);
        }
        let key = (file.id, expr.text_range(), GuardKind::Expr);
        if !self.enter(key) {
            return Type::Unknown;
        }
        crate::infer::DBG_EXPRS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let previous_enum_query = self.enum_query_file.replace(Some(file.clone()));
        let t = self.type_of_expr_inner(file, expr);
        self.enum_query_file.replace(previous_enum_query);
        let dependency = self.leave(key);
        let ty = self.simplify_inferred_string_unions(t, &mut 1024, 0);
        if let Some(epoch) = memo_epoch
            .filter(|epoch| dependency.is_none() && self.binary_memo_epoch(file) == Some(*epoch))
        {
            self.remember_binary(file, epoch, expr.text_range(), ty.clone());
        }
        ty
    }

    pub(crate) fn scope_of(&self, file: &FileInfo, node: &SyntaxNode) -> ScopeId {
        // Expressions in default values / decorators / base classes belong to the enclosing scope.
        let mut n = Some(node.clone());
        while let Some(cur) = n {
            if matches!(
                cur.kind(),
                SyntaxKind::DEFAULT_VALUE
                    | SyntaxKind::DECORATOR
                    | SyntaxKind::ANNOTATION
                    | SyntaxKind::RETURN_ANNOTATION
            ) {
                if let Some(def) = cur.ancestors().find(|a| {
                    matches!(
                        a.kind(),
                        SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF | SyntaxKind::LAMBDA_EXPR
                    )
                }) {
                    // Generic definition annotations resolve through their
                    // maintained annotation scope, not the enclosing runtime
                    // namespace. Decorators still use the enclosing scope.
                    if matches!(
                        cur.kind(),
                        SyntaxKind::ANNOTATION | SyntaxKind::RETURN_ANNOTATION
                    ) || (cur.kind() == SyntaxKind::DEFAULT_VALUE
                        && cur
                            .parent()
                            .is_some_and(|parent| parent.kind() == SyntaxKind::TYPE_PARAM))
                    {
                        if let Some(parameters) = def
                            .children()
                            .find(|child| child.kind() == SyntaxKind::TYPE_PARAM_LIST)
                        {
                            if let Some(scope) = file.scopes.scope_for_node(parameters.text_range())
                            {
                                return scope;
                            }
                        }
                    }
                    if let Some(p) = def.parent() {
                        return file.scopes.scope_at(
                            p.text_range()
                                .start()
                                .max(def.text_range().start() - TextSize::new(0)),
                        );
                    }
                }
            }
            // `type Pair[T] = tuple[T, T]`: the value sees the alias's own parameters.
            if let Some(alias) = cur.parent().filter(|p| {
                p.kind() == SyntaxKind::TYPE_ALIAS_STMT && ast::Expr::can_cast(cur.kind())
            }) {
                if let Some(scope) = alias
                    .children()
                    .find(|child| child.kind() == SyntaxKind::TYPE_PARAM_LIST)
                    .and_then(|parameters| file.scopes.scope_for_node(parameters.text_range()))
                {
                    return scope;
                }
            }
            if cur.kind() == SyntaxKind::ARG_LIST
                && cur
                    .parent()
                    .is_some_and(|p| p.kind() == SyntaxKind::CLASS_DEF)
            {
                let def = cur.parent().unwrap();
                return def
                    .parent()
                    .map(|p| file.scopes.scope_at(p.text_range().start()))
                    .unwrap_or(file.scopes.module_scope());
            }
            n = cur.parent();
        }
        file.scopes.scope_at(node.text_range().start())
    }

    fn type_of_expr_inner(&self, file: &Arc<FileInfo>, expr: &ast::Expr) -> Type {
        match expr {
            // Flow-sensitive and narrowed (see narrow.rs).
            ast::Expr::Name(n) => {
                let ty = self.name_type(file, n);
                if ty.is_unknown() {
                    if let Some(module) = self.star_imported_module(
                        file,
                        self.scope_of(file, n.syntax()),
                        &n.name_text(),
                    ) {
                        return Type::Module(module.id);
                    }
                }
                ty
            }
            ast::Expr::Literal(l) => match l.kind() {
                Some(SyntaxKind::INT_NUMBER) => {
                    let text = l.text();
                    match parse_int(&text) {
                        Some(v) => Type::Literal(LiteralValue::Int(v)),
                        None => self.instance_of("int"),
                    }
                }
                Some(SyntaxKind::FLOAT_NUMBER) => self.instance_of("float"),
                Some(SyntaxKind::COMPLEX_NUMBER) => self.instance_of("complex"),
                Some(SyntaxKind::TRUE_KW) => Type::Literal(LiteralValue::Bool(true)),
                Some(SyntaxKind::FALSE_KW) => Type::Literal(LiteralValue::Bool(false)),
                Some(SyntaxKind::NONE_KW) => Type::None,
                Some(SyntaxKind::ELLIPSIS) => self.instance_of("ellipsis"),
                _ => Type::Unknown,
            },
            ast::Expr::String(s) => {
                if s.is_bytes() {
                    Type::Literal(LiteralValue::Bytes(SmolStr::new(s.value())))
                } else {
                    Type::Literal(LiteralValue::Str(SmolStr::new(s.value())))
                }
            }
            ast::Expr::FString(value) => self
                .literal_string_fstring(file, value)
                .unwrap_or_else(|| self.instance_of("str")),
            ast::Expr::ConcatString(c) => {
                let bytes = c
                    .parts()
                    .any(|p| matches!(&p, ast::Expr::String(s) if s.is_bytes()));
                self.instance_of(if bytes { "bytes" } else { "str" })
            }
            ast::Expr::Paren(p) => p
                .inner()
                .map(|i| self.type_of_expr(file, &i))
                .unwrap_or(Type::Unknown),
            ast::Expr::Tuple(t) => {
                let elems: Vec<Type> = t
                    .elements()
                    .map(|e| self.widen(self.type_of_expr(file, &e)))
                    .collect();
                if t.elements().any(|e| matches!(e, ast::Expr::Star(_))) {
                    Type::Instance(
                        self.builtin_class("tuple").unwrap_or_else(unknown_class),
                        vec![Type::Unknown],
                    )
                } else {
                    Type::Tuple(elems)
                }
            }
            ast::Expr::List(l) => self.container(
                "list",
                l.elements().map(|e| self.element_type(file, &e)).collect(),
            ),
            ast::Expr::Set(s) => self.container(
                "set",
                s.elements().map(|e| self.element_type(file, &e)).collect(),
            ),
            ast::Expr::Dict(d) => {
                let keys: Vec<Type> = d
                    .items()
                    .filter_map(|i| i.key())
                    .map(|k| self.widen(self.type_of_expr(file, &k)))
                    .collect();
                let vals: Vec<Type> = d
                    .items()
                    .filter_map(|i| i.value())
                    .map(|v| self.widen(self.type_of_expr(file, &v)))
                    .collect();
                let mut k = Type::union(keys);
                let mut v = Type::union(vals);
                for u in d.unpacks() {
                    if let Some(Type::Instance(c, args)) =
                        u.expr().map(|e| self.type_of_expr(file, &e))
                    {
                        if c.name == "dict" && args.len() == 2 {
                            k = Type::union(vec![k, args[0].clone()]);
                            v = Type::union(vec![v, args[1].clone()]);
                        }
                    }
                }
                Type::Instance(
                    self.builtin_class("dict").unwrap_or_else(unknown_class),
                    vec![k, v],
                )
            }
            ast::Expr::ListComp(c) => self.container(
                "list",
                c.element()
                    .map(|e| self.widen(self.type_of_expr(file, &e)))
                    .into_iter()
                    .collect(),
            ),
            ast::Expr::SetComp(c) => self.container(
                "set",
                c.element()
                    .map(|e| self.widen(self.type_of_expr(file, &e)))
                    .into_iter()
                    .collect(),
            ),
            ast::Expr::Generator(c) => {
                let elem = c
                    .element()
                    .map(|e| self.widen(self.type_of_expr(file, &e)))
                    .unwrap_or(Type::Unknown);
                self.typing_class("Generator")
                    .map(|g| Type::Instance(g, vec![elem, Type::None, Type::None]))
                    .unwrap_or(Type::Unknown)
            }
            ast::Expr::DictComp(c) => {
                let k = c
                    .item()
                    .and_then(|i| i.key())
                    .map(|k| self.widen(self.type_of_expr(file, &k)))
                    .unwrap_or(Type::Unknown);
                let v = c
                    .item()
                    .and_then(|i| i.value())
                    .map(|v| self.widen(self.type_of_expr(file, &v)))
                    .unwrap_or(Type::Unknown);
                Type::Instance(
                    self.builtin_class("dict").unwrap_or_else(unknown_class),
                    vec![k, v],
                )
            }
            ast::Expr::Attribute(a) => {
                let Some(obj) = a.object() else {
                    return Type::Unknown;
                };
                let base = self.type_of_expr(file, &obj);
                let member = self
                    .lookup_member(&base, &a.attr_text())
                    .unwrap_or(Type::Unknown);
                // `if self.x is not None: self.x.foo()` (see narrow.rs).
                self.reference_type(file, expr, member)
            }
            ast::Expr::Call(c) => {
                if let Some(result) = crate::frameworks::call_result(self, file, c) {
                    return result;
                }
                let Some(callee) = c.callee() else {
                    return Type::Unknown;
                };
                if let Some(imported) = self.literal_dynamic_import(file, c) {
                    return imported;
                }
                if let Some(result) = self.literal_string_call(file, c) {
                    return result;
                }
                let callee_ty = self.type_of_expr(file, &callee);
                if let Some(pack) = self.declared_tuple_pack_call(file, c) {
                    return pack;
                }
                if let Some(specification) = self.declared_paramspec_call(file, c) {
                    return specification;
                }
                if self.canonical_typevar_factory(
                    file,
                    &callee,
                    &mut rustc_hash::FxHashSet::default(),
                ) {
                    return self.declared_typevar_call(file, c).unwrap_or(Type::Unknown);
                }
                if let Some(result) = self.reflected_builtin_type_call(file, c, &callee_ty) {
                    return result;
                }
                // Special forms.
                if let ast::Expr::Name(n) = &callee {
                    if n.name_text().as_str() == "super" {
                        if let Some(t) = self.super_call_type(file, c, &callee_ty) {
                            return t;
                        }
                    }
                }
                // `object.__new__(cls)` / `super().__new__(cls)` create a `cls` instance.
                if let ast::Expr::Attribute(a) = &callee {
                    if a.attr_text() == "__new__" {
                        if let Some(Type::Class(cls, args)) = c
                            .arg_list()
                            .and_then(|l| l.positional().next())
                            .map(|first| self.type_of_expr(file, &first))
                        {
                            return Type::Instance(cls, args);
                        }
                    }
                }
                if let Some(result) = self.tuple_pack_star_call(file, c, &callee_ty) {
                    return result;
                }
                if let Some(result) = self.functools_call(file, c, &callee_ty) {
                    return result;
                }
                let (pos, kw) = self.call_args(file, c);
                if let Some(factory) = self.type_factory_call(file, c, &callee_ty) {
                    return factory;
                }
                self.call_result(&callee_ty, &pos, &kw)
            }
            ast::Expr::Subscript(s) => {
                let Some(obj) = s.object() else {
                    return Type::Unknown;
                };
                let base = self.type_of_expr(file, &obj);
                let base = self.type_factory_tuple_value(&base).unwrap_or(base);
                match base {
                    Type::Class(c, _) if self.is_enum_class(&c) => Type::Instance(c, Vec::new()),
                    Type::Class(c, _) => {
                        // Generic specialization in expression context: list[int]
                        let args = self.subscript_args(file, s);
                        Type::Class(c, args)
                    }
                    base => {
                        if let Some(result) = self.slice_subscript_result(file, s, &base) {
                            return result;
                        }
                        let idx_ty = s
                            .index_expr()
                            .map(|i| self.type_of_expr(file, &i))
                            .unwrap_or(Type::Unknown);
                        let item = self.subscript_result(&base, &idx_ty);
                        // `if d["k"] is not None: d["k"].foo()` (see narrow.rs).
                        self.reference_type(file, expr, item)
                    }
                }
            }
            ast::Expr::Unary(u) => {
                let op = u.op().map(|t| t.kind());
                let operand = u
                    .operand()
                    .map(|o| self.type_of_expr(file, &o))
                    .unwrap_or(Type::Unknown);
                if op == Some(SyntaxKind::TILDE) {
                    if let Some(result) = self.flag_invert_result(&operand) {
                        return result;
                    }
                }
                match op {
                    Some(SyntaxKind::MINUS) => match &operand {
                        Type::Literal(LiteralValue::Int(i)) => Type::Literal(LiteralValue::Int(-i)),
                        _ => self
                            .dunder_call(&operand, "__neg__", &[])
                            .unwrap_or_else(|| self.widen(operand.clone())),
                    },
                    Some(SyntaxKind::PLUS) => match &operand {
                        Type::Literal(LiteralValue::Int(value)) => {
                            Type::Literal(LiteralValue::Int(*value))
                        }
                        _ => self
                            .dunder_call(&operand, "__pos__", &[])
                            .unwrap_or_else(|| self.widen(operand.clone())),
                    },
                    Some(SyntaxKind::TILDE) => self
                        .dunder_call(&operand, "__invert__", &[])
                        .unwrap_or_else(|| self.widen(operand.clone())),
                    _ => Type::Unknown,
                }
            }
            ast::Expr::Binary(_) => self.binary_expression(file, expr),
            ast::Expr::BoolOp(b) => self.bool_op_result(file, b),
            ast::Expr::Compare(comparison) => self.comparison_value_result(file, comparison),
            ast::Expr::Not(_) => self.instance_of("bool"),
            ast::Expr::Conditional(c) => Type::union(vec![
                c.true_expr()
                    .map(|e| self.widen(self.type_of_expr(file, &e)))
                    .unwrap_or(Type::Unknown),
                c.false_expr()
                    .map(|e| self.widen(self.type_of_expr(file, &e)))
                    .unwrap_or(Type::Unknown),
            ]),
            ast::Expr::Named(n) => n
                .value()
                .map(|v| self.type_of_expr(file, &v))
                .unwrap_or(Type::Unknown),
            ast::Expr::Await(a) => {
                let inner = a
                    .expr()
                    .map(|e| self.type_of_expr(file, &e))
                    .unwrap_or(Type::Unknown);
                self.direct_await_at(file, &inner, a.text_range().start())
            }
            ast::Expr::Lambda(l) => {
                let params = l
                    .params()
                    .map(|p| self.params_from_list(file, &p, None, false))
                    .unwrap_or_default();
                let ret = l
                    .body()
                    .map(|b| self.widen(self.type_of_expr(file, &b)))
                    .unwrap_or(Type::Unknown);
                Type::Function(Arc::new(Signature {
                    params,
                    ret,
                    is_async: false,
                    def: None,
                    docstring: None,
                    ret_inferred: true,
                }))
            }
            ast::Expr::Slice(slice) => self.slice_value_type(file, slice),
            ast::Expr::Yield(expression) => self.yield_expression_result(file, expression),
            ast::Expr::YieldFrom(expression) => self.yield_from_expression_result(file, expression),
            ast::Expr::Star(_) => Type::Unknown,
        }
    }

    /// Evaluate binary syntax in postorder without consuming one native stack
    /// frame per operator. Each nested expression retains its ordinary cycle
    /// guard and final simplification; operands still run left before right.
    fn binary_expression(&self, file: &Arc<FileInfo>, root: &ast::Expr) -> Type {
        enum Work {
            Operand(Option<ast::Expr>),
            Apply(Option<SyntaxKind>, Option<GuardKey>),
        }
        let ast::Expr::Binary(binary) = root else {
            unreachable!("binary expression entry");
        };
        // Only the ordinary top-level query may publish descendants. Inference
        // entered through bindings, protocols or expected types stays isolated.
        let memo_epoch = (self.frames.borrow().len() == 1)
            .then(|| self.binary_memo_epoch(file))
            .flatten();
        let mut work = vec![
            Work::Apply(binary.op().map(|op| op.kind()), None),
            Work::Operand(binary.rhs()),
            Work::Operand(binary.lhs()),
        ];
        let mut values = Vec::new();
        while let Some(step) = work.pop() {
            match step {
                Work::Operand(None) => values.push(Type::Unknown),
                Work::Operand(Some(expression)) => {
                    if let ast::Expr::Binary(binary) = &expression {
                        // Contextual inference owns its isolated session exactly
                        // as in type_of_expr; do not bypass that authority.
                        if self.parent_expected_type(file, &expression).is_some() {
                            values.push(self.type_of_expr(file, &expression));
                            continue;
                        }
                        let key = (file.id, expression.text_range(), GuardKind::Expr);
                        if let Some(ty) = memo_epoch.and_then(|epoch| {
                            self.memoized_binary(file, epoch, expression.text_range())
                        }) {
                            values.push(ty);
                            continue;
                        }
                        if !self.enter(key) {
                            values.push(Type::Unknown);
                            continue;
                        }
                        DBG_EXPRS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        work.push(Work::Apply(binary.op().map(|op| op.kind()), Some(key)));
                        work.push(Work::Operand(binary.rhs()));
                        work.push(Work::Operand(binary.lhs()));
                    } else {
                        values.push(self.type_of_expr(file, &expression));
                    }
                }
                Work::Apply(op, key) => {
                    #[cfg(test)]
                    self.binary_applications
                        .set(self.binary_applications.get() + 1);
                    let rhs = values.pop().expect("binary right operand");
                    let lhs = values.pop().expect("binary left operand");
                    let mut result = op
                        .map(|op| self.binary_op(op, &lhs, &rhs))
                        .unwrap_or(Type::Unknown);
                    if let Some(key) = key {
                        let dependency = self.leave(key);
                        result = self.simplify_inferred_string_unions(result, &mut 1024, 0);
                        if let Some(epoch) = memo_epoch.filter(|_| dependency.is_none()) {
                            self.remember_binary(file, epoch, key.1, result.clone());
                        }
                    }
                    values.push(result);
                }
            }
        }
        values.pop().expect("binary expression result")
    }

    fn element_type(&self, file: &Arc<FileInfo>, e: &ast::Expr) -> Type {
        match e {
            ast::Expr::Star(s) => s
                .expr()
                .map(|inner| self.iter_type(&self.type_of_expr(file, &inner)))
                .unwrap_or(Type::Unknown),
            other => self.widen(self.type_of_expr(file, other)),
        }
    }

    fn container(&self, name: &str, elems: Vec<Type>) -> Type {
        let elem = Type::union(elems);
        Type::Instance(
            self.builtin_class(name).unwrap_or_else(unknown_class),
            vec![elem],
        )
    }

    fn subscript_args(&self, file: &Arc<FileInfo>, s: &ast::SubscriptExpr) -> Vec<Type> {
        match s.index() {
            Some(idx) if idx.kind() == SyntaxKind::SLICE_LIST => idx
                .children()
                .filter_map(ast::Expr::cast)
                .map(|e| self.annotation_type(file, &e))
                .collect(),
            Some(idx) => match ast::Expr::cast(idx) {
                Some(ast::Expr::Tuple(t)) => t
                    .elements()
                    .map(|e| self.annotation_type(file, &e))
                    .collect(),
                Some(e) => vec![self.annotation_type(file, &e)],
                None => Vec::new(),
            },
            None => Vec::new(),
        }
    }

    fn subscript_result(&self, base: &Type, index: &Type) -> Type {
        if matches!(base, Type::LiteralString | Type::LiteralStringValue) {
            return Type::LiteralStringValue;
        }
        if matches!(base, Type::Tuple(_)) {
            if let Type::Literal(LiteralValue::EnumMember { class, member }) = index {
                if let Some(value @ (LiteralValue::Int(_) | LiteralValue::Bool(_))) =
                    self.enum_primitive_value(class, member)
                {
                    let value = match value {
                        LiteralValue::Bool(value) => LiteralValue::Int(i64::from(value)),
                        other => other,
                    };
                    return self.subscript_result(base, &Type::Literal(value));
                }
            }
        }
        match base {
            Type::Tuple(ts) => match index {
                Type::Literal(LiteralValue::Int(i)) => {
                    let idx = if *i < 0 { ts.len() as i64 + i } else { *i };
                    ts.get(idx as usize).cloned().unwrap_or(Type::Unknown)
                }
                _ => Type::union(ts.clone()),
            },
            Type::Union(ts) => {
                Type::union(ts.iter().map(|t| self.subscript_result(t, index)).collect())
            }
            Type::Literal(_) => self.subscript_result(&self.widen(base.clone()), index),
            _ => self
                .dunder_call(base, "__getitem__", std::slice::from_ref(index))
                .unwrap_or(Type::Unknown),
        }
    }

    fn binary_op(&self, op: SyntaxKind, lhs: &Type, rhs: &Type) -> Type {
        if let Some(result) = self.literal_string_binary(op, lhs, rhs) {
            return result;
        }
        if matches!(lhs, Type::LiteralString | Type::LiteralStringValue) {
            return self.binary_op(op, &self.instance_of("str"), rhs);
        }
        if matches!(rhs, Type::LiteralString | Type::LiteralStringValue) {
            return self.binary_op(op, lhs, &self.instance_of("str"));
        }
        if let Some(result) = self.flag_binary_result(op, lhs, rhs) {
            return result;
        }
        let (name, rname) = match op {
            SyntaxKind::PLUS => ("__add__", "__radd__"),
            SyntaxKind::MINUS => ("__sub__", "__rsub__"),
            SyntaxKind::STAR => ("__mul__", "__rmul__"),
            SyntaxKind::SLASH => ("__truediv__", "__rtruediv__"),
            SyntaxKind::SLASH_SLASH => ("__floordiv__", "__rfloordiv__"),
            SyntaxKind::PERCENT => ("__mod__", "__rmod__"),
            SyntaxKind::STAR_STAR => ("__pow__", "__rpow__"),
            SyntaxKind::AT => ("__matmul__", "__rmatmul__"),
            SyntaxKind::AMP => ("__and__", "__rand__"),
            SyntaxKind::PIPE => ("__or__", "__ror__"),
            SyntaxKind::CARET => ("__xor__", "__rxor__"),
            SyntaxKind::SHL => ("__lshift__", "__rlshift__"),
            SyntaxKind::SHR => ("__rshift__", "__rrshift__"),
            _ => return Type::Unknown,
        };
        // Constant folding for small ints / string concatenation keeps literal types.
        if let (Type::Literal(LiteralValue::Int(a)), Type::Literal(LiteralValue::Int(b))) =
            (lhs, rhs)
        {
            let v = match op {
                SyntaxKind::PLUS => a.checked_add(*b),
                SyntaxKind::MINUS => a.checked_sub(*b),
                SyntaxKind::STAR => a.checked_mul(*b),
                _ => None,
            };
            if let Some(v) = v {
                return Type::Literal(LiteralValue::Int(v));
            }
        }
        if let (Type::Literal(LiteralValue::Str(a)), Type::Literal(LiteralValue::Str(b))) =
            (lhs, rhs)
        {
            if op == SyntaxKind::PLUS {
                return Type::Literal(LiteralValue::Str(SmolStr::new(format!("{a}{b}"))));
            }
        }
        let result = match self.binary_protocol_result(lhs, rhs, name, rname) {
            Ok(result) => result,
            Err(()) => return Type::Unknown,
        };
        if !result.is_unknown() {
            return result;
        }
        // Type-expression unions: `int | None` as a runtime value.
        if op == SyntaxKind::PIPE && matches!(lhs, Type::Class(..) | Type::None) {
            return Type::Class(
                self.builtin_class("type").unwrap_or_else(unknown_class),
                vec![],
            );
        }
        Type::Unknown
    }

    /// The value `x op= v` binds: `x.__iop__(v)`, else `x op v` (with the
    /// reflected operator); `old`, the value read, when neither is known.
    fn aug_assign_result(
        &self,
        file: &Arc<FileInfo>,
        b: &crate::scope::Binding,
        old: Type,
    ) -> Type {
        use SyntaxKind as K;
        if old.is_unknown() {
            return old;
        }
        let Some(stmt) = node_with_range(&file.parse.syntax_node(), b.node_range, |n| {
            n.kind() == K::AUG_ASSIGN_STMT
        }) else {
            return old;
        };
        let Some(op) = stmt
            .children_with_tokens()
            .filter_map(|e| e.into_token())
            .map(|t| t.kind())
            .find(|k| k.is_augmented_assign_op())
        else {
            return old;
        };
        let (binary, inplace) = match op {
            K::PLUS_EQ => (K::PLUS, "__iadd__"),
            K::MINUS_EQ => (K::MINUS, "__isub__"),
            K::STAR_EQ => (K::STAR, "__imul__"),
            K::SLASH_EQ => (K::SLASH, "__itruediv__"),
            K::SLASH_SLASH_EQ => (K::SLASH_SLASH, "__ifloordiv__"),
            K::PERCENT_EQ => (K::PERCENT, "__imod__"),
            K::STAR_STAR_EQ => (K::STAR_STAR, "__ipow__"),
            K::AT_EQ => (K::AT, "__imatmul__"),
            K::AMP_EQ => (K::AMP, "__iand__"),
            K::PIPE_EQ => (K::PIPE, "__ior__"),
            K::CARET_EQ => (K::CARET, "__ixor__"),
            K::SHL_EQ => (K::SHL, "__ilshift__"),
            K::SHR_EQ => (K::SHR, "__irshift__"),
            _ => return old,
        };
        let Some(rhs) = b.value.map(|v| self.type_of_range(file, v)) else {
            return old;
        };
        let lhs = self.widen(old.clone());
        let inplace_result = if matches!(lhs, Type::Union(_)) {
            None
        } else {
            self.dunder_call_checked(
                &lhs,
                inplace,
                std::slice::from_ref(&self.widen(rhs.clone())),
            )
            .filter(|t| !t.is_unknown())
        };
        let result = inplace_result.unwrap_or_else(|| self.binary_op(binary, &lhs, &rhs));
        if result.is_unknown() {
            old
        } else {
            self.widen(result)
        }
    }

    fn dunder_call(&self, recv: &Type, name: &str, args: &[Type]) -> Option<Type> {
        let m = self.implicit_slot(recv, name)?;
        if !m.is_callable() {
            return None;
        }
        Some(self.call_result(&m, args, &[]))
    }

    /// Like [`dunder_call`] but only when some signature accepts the arguments
    /// (binary operators return `NotImplemented` otherwise).
    fn dunder_call_checked(&self, recv: &Type, name: &str, args: &[Type]) -> Option<Type> {
        let m = self.implicit_slot(recv, name)?;
        let sigs: Vec<Arc<Signature>> = match &m {
            Type::Overloaded(v) => v.clone(),
            Type::Function(s) | Type::BoundMethod(s, _) | Type::Callable(s) => vec![s.clone()],
            _ => return None,
        };
        let sig = sigs.iter().find(|s| self.arguments_fit(s, args, &[]))?;
        Some(self.apply_signature(sig, args, &[]))
    }

    pub fn await_type(&self, t: &Type) -> Type {
        self.direct_await_result(t)
    }

    /// Element type produced by iterating over `t`.
    pub fn iter_type(&self, t: &Type) -> Type {
        if let Some(element) = self.type_factory_iter(t) {
            return element;
        }
        match t {
            Type::LiteralString | Type::LiteralStringValue => Type::LiteralStringValue,
            Type::Tuple(ts) => Type::union(ts.clone()),
            Type::Literal(LiteralValue::Str(_)) => self.instance_of("str"),
            Type::Literal(LiteralValue::Bytes(_)) => self.instance_of("int"),
            Type::Instance(c, args) => {
                if let Some(element) = self.canonical_collection_iter(c, args) {
                    return element;
                }
                self.protocol_iter_type(t)
            }
            Type::Union(ts) => Type::union(ts.iter().map(|t| self.iter_type(t)).collect()),
            Type::Unknown | Type::Any => t.clone(),
            other => self.protocol_iter_type(other),
        }
    }

    // ----- calls ------------------------------------------------------------------

    /// Inferred positional, generator and named argument types for a call.
    /// Literal imports are resolved without executing user code. Only the actual
    /// standard-library callable qualifies; spelling alone is never authority.
    fn literal_dynamic_import(&self, file: &Arc<FileInfo>, call: &ast::CallExpr) -> Option<Type> {
        let callee = call.callee()?;
        let (module, function) = match &callee {
            ast::Expr::Name(n) => {
                let def =
                    self.resolve_name(file, self.scope_of(file, n.syntax()), &n.name_text())?;
                let def = self.follow_imports(def);
                (def.file.clone(), def.binding().name.to_string())
            }
            ast::Expr::Attribute(a) => {
                let Type::Module(id) = self.type_of_expr(file, &a.object()?) else {
                    return None;
                };
                (self.db.file(id)?, a.attr_text())
            }
            _ => return None,
        };
        let canonical = module.module_name.as_deref()?;
        if !module.path.starts_with("<typeshed>") {
            return None;
        }
        let import_module = canonical == "importlib" && function == "import_module";
        let builtin_import = canonical == "builtins" && function == "__import__";
        if !import_module && !builtin_import {
            return None;
        }
        let args = call.arg_list()?;
        if args
            .args()
            .any(|a| !matches!(a, ast::AnyArg::Positional(_) | ast::AnyArg::Keyword(_)))
        {
            return Some(Type::Unknown);
        }
        let positional: Vec<_> = args.positional().collect();
        let names: &[&str] = if import_module {
            &["name", "package"]
        } else {
            &["name", "globals", "locals", "fromlist", "level"]
        };
        if positional.len() > names.len() {
            return Some(Type::Unknown);
        }
        let mut seen = std::collections::HashSet::new();
        for keyword in args.keywords() {
            let name = keyword.name_text();
            let Some(index) = names.iter().position(|allowed| *allowed == name) else {
                return Some(Type::Unknown);
            };
            if index < positional.len() || !seen.insert(name) {
                return Some(Type::Unknown);
            }
        }
        let argument = |index: usize, name: &str| -> Option<ast::Expr> {
            let keyword: Vec<_> = args.keywords().filter(|k| k.name_text() == name).collect();
            if keyword.len() > 1 || (positional.get(index).is_some() && !keyword.is_empty()) {
                return None;
            }
            positional
                .get(index)
                .cloned()
                .or_else(|| keyword.first().and_then(|k| k.value()))
        };
        let Some(ast::Expr::String(name)) = argument(0, "name") else {
            return Some(Type::Unknown);
        };
        if name.is_bytes() {
            return Some(Type::Unknown);
        }
        let mut name = name.value();
        if name.is_empty() {
            return Some(Type::Unknown);
        }
        if import_module {
            if positional.len() > 2
                || args
                    .keywords()
                    .any(|k| !matches!(k.name_text().as_str(), "name" | "package"))
            {
                return Some(Type::Unknown);
            }
            let level = name.bytes().take_while(|b| *b == b'.').count();
            if level > 0 {
                let Some(ast::Expr::String(package)) = argument(1, "package") else {
                    return Some(Type::Unknown);
                };
                if package.is_bytes() {
                    return Some(Type::Unknown);
                }
                let package = package.value();
                let mut parts: Vec<_> = package.split('.').collect();
                if parts.iter().any(|p| p.is_empty()) || level > parts.len() {
                    return Some(Type::Unknown);
                }
                parts.truncate(parts.len() - level + 1);
                let tail = &name[level..];
                let mut resolved = parts.join(".");
                if !tail.is_empty() {
                    resolved.push('.');
                    resolved.push_str(tail);
                }
                name = resolved;
            }
        } else {
            // A globals-dependent relative builtin import is deliberately unknown.
            if positional.len() > 5
                || args.keywords().any(|k| {
                    !matches!(
                        k.name_text().as_str(),
                        "name" | "globals" | "locals" | "fromlist" | "level"
                    )
                })
            {
                return Some(Type::Unknown);
            }
            if let Some(level) = argument(4, "level") {
                if level.syntax().text() != "0" {
                    return Some(Type::Unknown);
                }
            }
            let fromlist = match argument(3, "fromlist") {
                None => false,
                Some(ast::Expr::Tuple(t)) => {
                    if t.elements()
                        .any(|e| !matches!(e, ast::Expr::String(s) if !s.is_bytes()))
                    {
                        return Some(Type::Unknown);
                    }
                    t.elements().next().is_some()
                }
                Some(ast::Expr::List(t)) => {
                    if t.elements()
                        .any(|e| !matches!(e, ast::Expr::String(s) if !s.is_bytes()))
                    {
                        return Some(Type::Unknown);
                    }
                    t.elements().next().is_some()
                }
                _ => return Some(Type::Unknown),
            };
            // Importing a dotted name must first resolve that entire name, even
            // when the builtin returns its top-level package.
            if self.db.module(&name).is_none() {
                return Some(Type::Unknown);
            }
            if !fromlist {
                name = name.split('.').next()?.to_owned();
            }
        }
        Some(
            self.db
                .module(&name)
                .map(|m| Type::Module(m.id))
                .unwrap_or(Type::Unknown),
        )
    }

    /// Unpacked `*args` and `**kwargs` are omitted, as their arity and names
    /// may be unknown. The result can be passed to overload selection.
    pub fn call_args(
        &self,
        file: &Arc<FileInfo>,
        c: &ast::CallExpr,
    ) -> (Vec<Type>, Vec<(SmolStr, Type)>) {
        let mut pos = Vec::new();
        let mut kw = Vec::new();
        if let Some(args) = c.arg_list() {
            for a in args.args() {
                match &a {
                    ast::AnyArg::Positional(p) => pos.push(
                        p.expr()
                            .map(|e| self.type_of_expr(file, &e))
                            .unwrap_or(Type::Unknown),
                    ),
                    ast::AnyArg::Keyword(k) => kw.push((
                        SmolStr::new(k.name_text()),
                        k.value()
                            .map(|e| self.type_of_expr(file, &e))
                            .unwrap_or(Type::Unknown),
                    )),
                    ast::AnyArg::Generator(g) => {
                        pos.push(self.type_of_expr(file, &ast::Expr::Generator(g.clone())))
                    }
                    _ => {}
                }
            }
        }
        (pos, kw)
    }

    /// Result type of calling `callee` with the given argument types.
    pub fn call_result(&self, callee: &Type, pos: &[Type], kw: &[(SmolStr, Type)]) -> Type {
        if let Some(result) = self.functools_call_result(callee, pos, kw) {
            return result;
        }
        if let Some(instance) = self.type_variable_class_call(callee) {
            return instance;
        }
        match callee {
            Type::Function(sig) | Type::BoundMethod(sig, _) | Type::Callable(sig) => {
                self.apply_signature(sig, pos, kw)
            }
            Type::Overloaded(sigs) => {
                let sig = self.select_overload(sigs, pos, kw);
                self.apply_signature(&sig, pos, kw)
            }
            Type::Class(c, args) => {
                if let Some(instance) = self.type_factory_construct(c) {
                    return instance;
                }
                if c.name == "type" && pos.len() == 1 {
                    return match &pos[0] {
                        Type::Instance(x, a) => Type::Class(x.clone(), a.clone()),
                        _ => self.instance_of("type"),
                    };
                }
                self.class_call_result(c, args, pos, kw)
            }
            Type::Instance(..) | Type::SelfType(_) => self
                .implicit_slot(callee, "__call__")
                .map(|slot| self.call_result(&slot, pos, kw))
                .unwrap_or(Type::Unknown),
            Type::Union(ts) => {
                Type::union(ts.iter().map(|t| self.call_result(t, pos, kw)).collect())
            }
            Type::Any => Type::Any,
            Type::DeclaredTypeVar(definition) => match &definition.bound {
                Some(bound) => self.call_result(bound, pos, kw),
                None if !definition.constraints.is_empty() => Type::union(
                    definition
                        .constraints
                        .iter()
                        .map(|constraint| self.call_result(constraint, pos, kw))
                        .collect(),
                ),
                None => Type::Unknown,
            },
            _ => Type::Unknown,
        }
    }

    /// The constructor to use for type-argument inference: the nearest
    /// `__init__` or `__new__` in the MRO that is not `object`'s, or the
    /// `__init__` generated for a dataclass-like class (see [`SynthKind`]).
    pub fn constructor(&self, c: &ClassRef) -> Option<Type> {
        if let Some(constructor) = self.type_factory_constructor(c) {
            return Some(constructor);
        }
        self.find_constructor(c).map(|(signature, _)| signature)
    }

    fn apply_signature(&self, sig: &Signature, pos: &[Type], kw: &[(SmolStr, Type)]) -> Type {
        let mut subst: FxHashMap<SmolStr, Type> = FxHashMap::default();
        self.solve_typevars(&sig.params, pos, kw, &mut subst);
        self.finish_signature_result(sig, subst)
    }

    pub(super) fn finish_signature_result(
        &self,
        sig: &Signature,
        mut subst: FxHashMap<SmolStr, Type>,
    ) -> Type {
        if self.unresolved_parameter_specifications(sig, &subst)
            || self.unresolved_tuple_packs(sig, &subst)
        {
            return Type::Unknown;
        }
        self.solve_signature_defaults(sig, &mut subst);
        let ret = sig.ret.substitute(&|n| subst.get(n).cloned());
        // Unsolved type variables become Unknown rather than leaking.
        let ret = self.normalize_class_objects(ret);
        self.inferred_string_result(ret.substitute(&|_| Some(Type::Unknown)))
    }

    fn solve_typevars(
        &self,
        params: &[Param],
        pos: &[Type],
        kw: &[(SmolStr, Type)],
        subst: &mut FxHashMap<SmolStr, Type>,
    ) {
        let mut pos_iter = pos.iter();
        for p in params {
            let arg = match p.kind {
                ParamKind::PositionalOnly | ParamKind::Normal => {
                    pos_iter.next().cloned().or_else(|| {
                        kw.iter()
                            .find(|(n, _)| *n == p.name)
                            .map(|(_, t)| t.clone())
                    })
                }
                ParamKind::KeywordOnly => kw
                    .iter()
                    .find(|(n, _)| *n == p.name)
                    .map(|(_, t)| t.clone()),
                ParamKind::VarArgs => {
                    let rest: Vec<Type> = pos_iter.by_ref().cloned().collect();
                    if let Type::Unpack(inner) = &p.ty {
                        if let Type::TypeVarTuple(definition) = inner.as_ref() {
                            self.bind_tuple_pack(definition, rest, subst);
                            continue;
                        }
                    }
                    if rest.is_empty() {
                        None
                    } else {
                        Some(Type::union(rest))
                    }
                }
                ParamKind::KwArgs => None,
            };
            if let Some(arg) = arg {
                self.unify(&p.ty, &self.widen(arg), subst);
            }
        }
    }

    fn unify(&self, param: &Type, arg: &Type, subst: &mut FxHashMap<SmolStr, Type>) {
        if self.unify_class_object(param, arg, subst) {
            return;
        }
        match (param, arg) {
            (Type::DeclaredTypeVar(definition), argument) => {
                self.solve_declared_typevar(definition, argument, subst);
            }
            (Type::TypeVar(n), a) => {
                if a.is_unknown() {
                    return;
                }
                let entry = subst.entry(n.clone()).or_insert_with(|| a.clone());
                if entry != a {
                    *entry = Type::union(vec![entry.clone(), a.clone()]);
                }
            }
            (Type::Instance(pc, pargs), Type::Instance(ac, aargs)) => {
                if pc == ac {
                    for (p, a) in pargs.iter().zip(aargs) {
                        self.unify(p, a, subst);
                    }
                } else if !pargs.is_empty() {
                    // Find pc in the MRO of ac with mapped args (e.g. Iterable[_T] from list[int]).
                    let nominal = self
                        .mro_with_args(ac, aargs)
                        .into_iter()
                        .find(|(cls, _)| cls == pc);
                    match nominal {
                        Some((_, args)) => {
                            for (p, a) in pargs.iter().zip(&args) {
                                self.unify(p, a, subst);
                            }
                        }
                        // Structural: `SupportsAbs[T]` from `int.__abs__`.
                        None => self.unify_protocol_members(param, arg, subst),
                    }
                }
            }
            // An enum class iterates its members (`list(Color)`).
            (Type::Instance(pc, pargs), Type::Class(ac, _))
                if pargs.len() == 1
                    && matches!(pc.name.as_str(), "Iterable" | "Collection")
                    && self.is_enum_class(ac) =>
            {
                self.unify(&pargs[0], &Type::Instance(ac.clone(), Vec::new()), subst);
            }
            (Type::Instance(pc, pargs), Type::Tuple(ts))
                if pc.name == "tuple" || pc.name == "Iterable" || pc.name == "Sequence" =>
            {
                if let Some(p) = pargs.first() {
                    self.unify(p, &Type::union(ts.clone()), subst);
                }
            }
            (Type::Tuple(ps), Type::Tuple(ts)) => {
                if !self.unify_tuple_pack(ps, ts, subst) {
                    for (p, a) in ps.iter().zip(ts) {
                        self.unify(p, a, subst);
                    }
                }
            }
            (Type::Union(ps), a) => {
                for p in ps {
                    if !matches!(p, Type::None) {
                        self.unify(p, a, subst);
                    }
                }
            }
            (
                Type::Callable(psig) | Type::Function(psig),
                Type::Function(asig) | Type::BoundMethod(asig, _) | Type::Callable(asig),
            ) => {
                if !self.unify_parameter_specification(psig, asig, subst) {
                    for (p, a) in psig.params.iter().zip(&asig.params) {
                        self.unify(&p.ty, &a.ty, subst);
                    }
                }
                self.unify(&psig.ret, &asig.ret, subst);
            }
            (Type::Callable(psig) | Type::Function(psig), Type::Class(c, args)) => {
                // A class used as a factory: calling it yields an instance.
                let inst = Type::Instance(c.clone(), args.clone());
                self.unify(&psig.ret, &inst, subst);
            }
            (Type::Callable(psig) | Type::Function(psig), Type::Overloaded(sigs)) => {
                if let Some(a) = sigs.first() {
                    self.unify(&psig.ret, &a.ret, subst);
                }
            }
            (Type::Class(pc, pargs), Type::Class(ac, aargs)) if pc == ac => {
                for (p, a) in pargs.iter().zip(aargs) {
                    self.unify(p, a, subst);
                }
            }
            (Type::Class(pc, pargs), Type::Class(ac, _))
                if pc.name == "type" && pargs.len() == 1 =>
            {
                self.unify(&pargs[0], &Type::Instance(ac.clone(), vec![]), subst);
            }
            _ => {}
        }
    }

    /// Selects the applicable signature using the same inference argument mapping.
    pub fn select_overload(
        &self,
        sigs: &[Arc<Signature>],
        pos: &[Type],
        kw: &[(SmolStr, Type)],
    ) -> Arc<Signature> {
        for sig in sigs {
            if self.arguments_fit(sig, pos, kw) {
                return sig.clone();
            }
        }
        sigs.last().cloned().unwrap_or_else(|| {
            Arc::new(Signature {
                params: vec![],
                ret: Type::Unknown,
                is_async: false,
                def: None,
                docstring: None,
                ret_inferred: false,
            })
        })
    }

    pub(crate) fn arguments_fit(
        &self,
        sig: &Signature,
        pos: &[Type],
        kw: &[(SmolStr, Type)],
    ) -> bool {
        let mut pos_iter = pos.iter();
        let mut consumed = vec![false; kw.len()];
        let mut kwargs = None;
        for parameter in &sig.params {
            match parameter.kind {
                ParamKind::PositionalOnly | ParamKind::Normal => {
                    if let Some(actual) = pos_iter.next() {
                        if !self.is_compatible(actual, &parameter.ty)
                            || (parameter.kind == ParamKind::Normal
                                && kw.iter().any(|(name, _)| *name == parameter.name))
                        {
                            return false;
                        }
                    } else if parameter.kind == ParamKind::Normal {
                        if let Some((index, (_, actual))) = kw
                            .iter()
                            .enumerate()
                            .find(|(_, (name, _))| *name == parameter.name)
                        {
                            consumed[index] = true;
                            if !self.is_compatible(actual, &parameter.ty) {
                                return false;
                            }
                        } else if !parameter.has_default {
                            return false;
                        }
                    } else if !parameter.has_default {
                        return false;
                    }
                }
                ParamKind::KeywordOnly => {
                    if let Some((index, (_, actual))) = kw
                        .iter()
                        .enumerate()
                        .find(|(_, (name, _))| *name == parameter.name)
                    {
                        consumed[index] = true;
                        if !self.is_compatible(actual, &parameter.ty) {
                            return false;
                        }
                    } else if !parameter.has_default {
                        return false;
                    }
                }
                ParamKind::VarArgs => {
                    for actual in pos_iter.by_ref() {
                        if !self.is_compatible(actual, &parameter.ty) {
                            return false;
                        }
                    }
                }
                ParamKind::KwArgs => kwargs = Some(&parameter.ty),
            }
        }
        if pos_iter.next().is_some() {
            return false;
        }
        for (index, (name, actual)) in kw.iter().enumerate() {
            if kw[..index].iter().any(|(previous, _)| previous == name) {
                return false;
            }
            if !consumed[index] {
                let Some(expected) = kwargs else {
                    return false;
                };
                if !self.is_compatible(actual, expected) {
                    return false;
                }
            }
        }
        true
    }

    /// Loose assignability used for overload selection.
    /// The numeric tower: `actual` is accepted where `expected` is declared
    /// (`int` → `float`/`complex`, `float` → `complex`, `bool` → all three).
    fn numeric_promotion(&self, actual: &ClassRef, expected: &ClassRef) -> bool {
        [
            ("int", "float"),
            ("int", "complex"),
            ("float", "complex"),
            ("bool", "int"),
            ("bool", "float"),
            ("bool", "complex"),
        ]
        .iter()
        .any(|(from, to)| {
            self.builtin_class(to).as_ref() == Some(expected)
                && self
                    .builtin_class(from)
                    .is_some_and(|builtin| self.mro(actual).contains(&builtin))
        })
    }

    pub fn is_compatible(&self, arg: &Type, param: &Type) -> bool {
        if let Some(result) = self.literal_string_compatibility(arg, param) {
            return result;
        }
        if let Some(result) = self.typed_dict_capability_compatible(arg, param) {
            return result;
        }
        if let Some(compatible) = self.type_factory_compatible(arg, param) {
            return compatible;
        }
        if let Some(compatible) = self.protocol_compatible(arg, param) {
            return compatible;
        }
        match (arg, param) {
            (Type::DeclaredTypeVar(left), Type::DeclaredTypeVar(right))
                if left.key == right.key =>
            {
                true
            }
            (_, Type::DeclaredTypeVar(definition)) => self.accepts_typevar(definition, arg),
            (Type::DeclaredTypeVar(definition), _) => {
                if let Some(bound) = &definition.bound {
                    self.is_compatible(bound, param)
                } else if !definition.constraints.is_empty() {
                    definition
                        .constraints
                        .iter()
                        .all(|constraint| self.is_compatible(constraint, param))
                } else {
                    matches!(param, Type::Any | Type::Unknown)
                        || matches!(param, Type::Instance(class,_) if self.builtin_class("object").as_ref() == Some(class))
                }
            }
            (_, Type::Unknown | Type::Any | Type::TypeVar(_)) | (Type::Unknown | Type::Any, _) => {
                true
            }
            (_, Type::Union(ps)) => ps.iter().any(|p| self.is_compatible(arg, p)),
            (Type::Union(ts), _) => ts.iter().all(|t| self.is_compatible(t, param)),
            (Type::None, Type::None) => true,
            (Type::ParamSpec(actual), Type::ParamSpec(expected)) => actual.key == expected.key,
            (Type::TypeVarTuple(actual), Type::TypeVarTuple(expected)) => {
                actual.key == expected.key
            }
            (Type::TypePack(actual), Type::TypePack(expected)) => {
                actual.len() == expected.len()
                    && actual
                        .iter()
                        .zip(expected)
                        .all(|(actual, expected)| self.is_compatible(actual, expected))
            }
            (Type::ParameterList(actual), Type::ParameterList(expected)) => {
                actual.len() == expected.len()
                    && actual.iter().zip(expected).all(|(actual, expected)| {
                        actual.kind == expected.kind
                            && (actual.kind == ParamKind::PositionalOnly
                                || actual.name == expected.name)
                            && (!expected.has_default || actual.has_default)
                            && self.is_compatible(&actual.ty, &expected.ty)
                    })
            }
            (Type::None, Type::Instance(c, _)) => ["object", "NoneType"]
                .iter()
                .any(|name| self.builtin_class(name).as_ref() == Some(c)),
            (Type::Literal(a), Type::Literal(b)) => a == b,
            (Type::Literal(LiteralValue::EnumMember { class, .. }), p) => {
                self.is_compatible(&Type::Instance(class.clone(), Vec::new()), p)
            }
            (Type::Literal(l), p) => {
                self.is_compatible(&self.instance_of(literal_class_name(l)), p)
            }
            (Type::Tuple(elements), Type::Instance(c, arguments)) => {
                let canonical = self.db.file(c.file).is_some_and(|file| {
                    file.is_stub && file.path.to_string_lossy().starts_with("<typeshed>/")
                });
                canonical
                    && matches!(
                        c.name.as_str(),
                        "tuple"
                            | "Sequence"
                            | "Iterable"
                            | "Collection"
                            | "object"
                            | "Container"
                            | "Reversible"
                            | "SupportsLenAndGetItem"
                    )
                    && (c.name == "object"
                        || arguments.first().is_none_or(|expected| {
                            elements
                                .iter()
                                .all(|element| self.is_compatible(element, expected))
                        }))
            }
            (Type::Tuple(a), Type::Tuple(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(a, b)| self.is_compatible(a, b))
            }
            (Type::Instance(ac, actual), Type::Instance(pc, expected)) => {
                if self.builtin_class("object").as_ref() == Some(pc) {
                    return true;
                }
                if ac == pc {
                    return self.generic_arguments_compatible(pc, actual, expected);
                }
                if self.numeric_promotion(ac, pc) {
                    return true;
                }
                if let Some((_, arguments)) = self
                    .mro_with_args(ac, actual)
                    .into_iter()
                    .find(|(class, _)| class == pc)
                {
                    return self.generic_arguments_compatible(pc, &arguments, expected);
                }
                false
            }
            (Type::SelfType(class), Type::Instance(expected, arguments)) => self.is_compatible(
                &Type::Instance(class.clone(), Vec::new()),
                &Type::Instance(expected.clone(), arguments.clone()),
            ),
            (Type::Class(..), Type::Instance(pc, _)) => {
                matches!(pc.name.as_str(), "type" | "object" | "Callable")
            }
            // A plain `type` (or metaclass) value may be any class object.
            (Type::Instance(ac, _), Type::Class(..))
                if self.mro(ac).iter().any(|c| self.is_builtin_type_class(c)) =>
            {
                true
            }
            (Type::Class(ac, actual), Type::Class(pc, expected)) => {
                if ac == pc {
                    self.generic_arguments_compatible(pc, actual, expected)
                } else if self.numeric_promotion(ac, pc) {
                    // `type[float]` accepts `int`, as `float` accepts an int.
                    true
                } else {
                    self.mro_with_args(ac, actual)
                        .into_iter()
                        .find(|(class, _)| class == pc)
                        .is_some_and(|(_, arguments)| {
                            self.generic_arguments_compatible(pc, &arguments, expected)
                        })
                }
            }
            (
                Type::Function(_) | Type::BoundMethod(..) | Type::Callable(_) | Type::Overloaded(_),
                Type::Callable(_) | Type::Function(_) | Type::BoundMethod(..) | Type::Overloaded(_),
            ) => self.callable_assignable(arg, param),
            (
                Type::Function(_) | Type::BoundMethod(..) | Type::Callable(_) | Type::Overloaded(_),
                Type::Instance(pc, _),
            ) => {
                matches!(pc.name.as_str(), "object" | "function" | "Callable")
            }
            (Type::Module(_), Type::Instance(pc, _)) => {
                matches!(pc.name.as_str(), "object" | "ModuleType")
            }
            _ => false,
        }
    }

    // ----- classes & members ---------------------------------------------------------

    pub fn class_info(&self, c: &ClassRef) -> Option<Arc<ClassInfo>> {
        if let Some(info) = self.type_factory_class_info(c) {
            return Some(info);
        }
        if let Some(i) = self.class_cache.borrow().get(c) {
            return Some(i.clone());
        }
        let file = self.db.file(c.file)?;
        let class = node_with_range(&file.parse.syntax_node(), c.range, |n| {
            n.kind() == SyntaxKind::CLASS_DEF
        })
        .and_then(ast::ClassDef::cast)?;
        let mut info = ClassInfo::new(&file, &class)?;
        if class.type_params().is_none() {
            // Generic[Unpack[Ts]] contains a qualifier as well as a parameter.
            // Only a captured canonical qualifier may be removed from metadata.
            let qualifiers: Vec<_> = class
                .bases()
                .into_iter()
                .take(32)
                .flat_map(|base| {
                    base.syntax()
                        .descendants()
                        .take(256)
                        .filter_map(ast::NameExpr::cast)
                        .collect::<Vec<_>>()
                })
                .filter(|name| {
                    self.canonical_parameter_symbol(
                        &file,
                        &ast::Expr::Name(name.clone()),
                        "Unpack",
                        &mut rustc_hash::FxHashSet::default(),
                    )
                })
                .map(|name| name.name_text())
                .collect();
            info.type_params.retain(|name| {
                !qualifiers
                    .iter()
                    .any(|qualifier| qualifier == name.as_str())
            });
        }
        self.refine_dataclass_initvars(&file, &class, &mut info);
        let info = Arc::new(info);
        self.class_cache
            .borrow_mut()
            .insert(c.clone(), info.clone());
        Some(info)
    }

    /// Whether some class in `c`'s hierarchy has a base expression that does
    /// not resolve to a class (an unresolved import, a call, a special form):
    /// the full set of attributes is then unknowable.
    pub fn has_unknown_base(&self, c: &ClassRef) -> bool {
        self.has_unknown_base_ignoring(c, &[])
    }

    /// [`Self::has_unknown_base`], also accepting the special-form bases named in `ignore`.
    pub fn has_unknown_base_ignoring(&self, c: &ClassRef, ignore: &[&str]) -> bool {
        self.mro(c).iter().any(|cls| {
            let (Some(info), Some(file)) = (self.class_info(cls), self.db.file(cls.file)) else {
                return false;
            };
            let root = file.parse.syntax_node();
            info.base_exprs.iter().any(|r| {
                let Some(expr) = node_with_range(&root, *r, |a| ast::Expr::can_cast(a.kind()))
                    .and_then(ast::Expr::cast)
                else {
                    return false;
                };
                // `Generic[T]` / `Protocol[T]` markers are declared as special forms in typeshed.
                let head = match &expr {
                    ast::Expr::Subscript(s) => s
                        .object()
                        .map(|o| o.syntax().text().to_string())
                        .unwrap_or_default(),
                    other => other.syntax().text().to_string(),
                };
                let last = head.rsplit('.').next().unwrap_or("");
                if matches!(last, "Generic" | "Protocol") || ignore.contains(&last) {
                    return false;
                }
                !matches!(
                    self.annotation_type(&file, &expr),
                    Type::Instance(..) | Type::Class(..)
                )
            })
        })
    }

    /// Direct bases as `(class, type args)`; unknown bases are skipped.
    fn direct_bases(&self, c: &ClassRef) -> Vec<(ClassRef, Vec<Type>)> {
        if let Some(bases) = self.type_factory_bases(c) {
            return bases;
        }
        let Some(info) = self.class_info(c) else {
            return Vec::new();
        };
        let Some(file) = self.db.file(c.file) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for r in &info.base_exprs {
            let root = file.parse.syntax_node();
            let Some(expr) = node_with_range(&root, *r, |a| ast::Expr::can_cast(a.kind()))
                .and_then(ast::Expr::cast)
            else {
                continue;
            };
            // Skip Generic[...] / Protocol[...] marker bases but keep real classes.
            let text = expr.syntax().text().to_string();
            if text.starts_with("Generic[") || text == "Generic" {
                continue;
            }
            match self.annotation_type(&file, &expr) {
                Type::Instance(b, args) if &b != c => out.push((b, args)),
                Type::Class(b, args) if &b != c => out.push((b, args)),
                _ => {}
            }
        }
        out
    }

    /// Method resolution order (class refs only).
    pub fn mro(&self, c: &ClassRef) -> Arc<Vec<ClassRef>> {
        if let Some(m) = self.mro_cache.borrow().get(c) {
            return m.clone();
        }
        // Guard against cycles while computing.
        self.mro_cache
            .borrow_mut()
            .insert(c.clone(), Arc::new(vec![c.clone()]));
        let bases = self.direct_bases(c);
        let base_mros: Vec<Vec<ClassRef>> =
            bases.iter().map(|(b, _)| (*self.mro(b)).clone()).collect();
        let mut mro = c3_merge(c.clone(), base_mros);
        if let Some(obj) = self.builtin_class("object") {
            if !mro.contains(&obj) {
                mro.push(obj);
            }
        }
        let arc = Arc::new(mro);
        self.mro_cache.borrow_mut().insert(c.clone(), arc.clone());
        arc
    }

    /// MRO with type arguments propagated through generic bases.
    pub fn mro_with_args(&self, c: &ClassRef, args: &[Type]) -> Vec<(ClassRef, Vec<Type>)> {
        let mro = self.mro(c);
        let mut arg_map: FxHashMap<ClassRef, Vec<Type>> = FxHashMap::default();
        arg_map.insert(c.clone(), args.to_vec());
        for cls in mro.iter() {
            let own_args = arg_map.get(cls).cloned().unwrap_or_default();
            let subst = |name: &str| self.class_substitution(cls, &own_args, name);
            for (b, bargs) in self.direct_bases(cls) {
                arg_map
                    .entry(b)
                    .or_insert_with(|| bargs.iter().map(|a| a.substitute(&subst)).collect());
            }
        }
        mro.iter()
            .map(|cls| (cls.clone(), arg_map.get(cls).cloned().unwrap_or_default()))
            .collect()
    }

    /// Looks up an attribute on any type.
    pub fn lookup_member(&self, ty: &Type, name: &str) -> Option<Type> {
        let depth = self.member_protocol_depth.get();
        if depth >= 32 {
            return Some(Type::Unknown);
        }
        self.member_protocol_depth.set(depth + 1);
        let result = self.lookup_member_inner(ty, name);
        self.member_protocol_depth.set(depth);
        result
    }
    fn lookup_member_inner(&self, ty: &Type, name: &str) -> Option<Type> {
        if let Type::Instance(class, _) | Type::Class(class, _) | Type::SelfType(class) = ty {
            if let Some(file) = self.db.file(class.file) {
                if !file.is_stub {
                    if let Some(field) = crate::frameworks::field_type(self, &file, class, name) {
                        return Some(field);
                    }
                }
            }
        }
        if let Some(member) = self.functools_member(ty, name) {
            return Some(member);
        }
        if let Some(member) = self.type_factory_member(ty, name) {
            return Some(member);
        }
        match ty {
            Type::LiteralString | Type::LiteralStringValue => {
                self.lookup_member(&self.instance_of("str"), name)
            }
            Type::Module(fid) => {
                let m = self.db.file(*fid)?;
                let attr = self
                    .resolve_in_module(&m, name, 0)
                    .map(|def| self.type_of_binding(&def.file, def.binding));
                if let Some(t) = &attr {
                    if !t.is_unknown() {
                        return Some(t.clone());
                    }
                }
                if let Some(modname) = &m.module_name {
                    if let Some(sub) = self.db.module_from(&m, &format!("{modname}.{name}")) {
                        return Some(Type::Module(sub.id));
                    }
                }
                if attr.is_some() {
                    return attr;
                }
                match name {
                    "__name__" | "__file__" | "__doc__" | "__package__" => {
                        Some(self.instance_of("str"))
                    }
                    "__dict__" => Some(self.instance_of("dict")),
                    // PEP 562: a module-level `__getattr__` provides every other
                    // attribute (incomplete stubs, compiled modules, lazy imports).
                    _ if name != "__getattr__" => {
                        let getattr = self.resolve_in_module(&m, "__getattr__", 0)?;
                        let ret = match self.type_of_binding(&getattr.file, getattr.binding) {
                            Type::Function(sig) => sig.ret.clone(),
                            _ => Type::Unknown,
                        };
                        Some(ret)
                    }
                    _ => None,
                }
            }
            Type::Instance(c, args) => self
                .instance_attribute(c, args, ty, name)
                .or_else(|| crate::frameworks::model_member(self, c, name)),
            Type::SelfType(c) => self
                .instance_attribute(c, &[], ty, name)
                .or_else(|| crate::frameworks::model_member(self, c, name)),
            Type::Class(c, args) => self
                .class_attribute(c, args, ty, name)
                .or_else(|| crate::frameworks::model_member(self, c, name)),
            Type::Tuple(_) => self.lookup_member(
                &Type::Instance(self.builtin_class("tuple")?, vec![Type::Unknown]),
                name,
            ),
            Type::Literal(LiteralValue::EnumMember { class, member }) => self
                .enum_value_attribute(class, member, name)
                .or_else(|| self.lookup_member(&Type::Instance(class.clone(), Vec::new()), name)),
            Type::Literal(l) => self.lookup_member(&self.instance_of(literal_class_name(l)), name),
            Type::None => {
                let nt = self
                    .db
                    .module("types")
                    .and_then(|m| self.resolve_in_module(&m, "NoneType", 0))
                    .map(|d| self.type_of_binding(&d.file, d.binding));
                match nt {
                    Some(Type::Class(c, _)) => self.lookup_in_mro(&c, &[], name, true, ty),
                    _ => None,
                }
            }
            Type::Function(_) | Type::BoundMethod(..) | Type::Callable(_) | Type::Overloaded(_) => {
                // `@functools.cache` functions also have `cache_clear()` & co.
                if let Some(found) = ty
                    .signature()
                    .and_then(|s| self.function_wrapper(s))
                    .and_then(|w| self.lookup_member(&w, name))
                {
                    return Some(found);
                }
                let f = self
                    .db
                    .module("types")
                    .and_then(|m| self.resolve_in_module(&m, "FunctionType", 0))
                    .map(|d| self.type_of_binding(&d.file, d.binding));
                match f {
                    Some(Type::Class(c, _)) => self.lookup_in_mro(&c, &[], name, true, ty),
                    _ => None,
                }
            }
            Type::Union(ts) => {
                let found: Vec<Type> = ts
                    .iter()
                    .filter(|t| !t.is_none())
                    .filter_map(|t| self.lookup_member(t, name))
                    .collect();
                if found.is_empty() {
                    None
                } else {
                    Some(Type::union(found))
                }
            }
            Type::Any => Some(Type::Any),
            Type::Super { after, receiver } => {
                let (classes, is_instance) = self.super_mro(after, receiver)?;
                self.lookup_in_classes(classes, name, is_instance, receiver)
            }
            Type::DeclaredTypeVar(definition) => match &definition.bound {
                Some(bound) => self.lookup_member(bound, name),
                None if !definition.constraints.is_empty() => definition
                    .constraints
                    .iter()
                    .map(|constraint| self.lookup_member(constraint, name))
                    .collect::<Option<Vec<_>>>()
                    .map(Type::union),
                None => None,
            },
            Type::TypeVar(_)
            | Type::ParamSpec(_)
            | Type::ParameterList(_)
            | Type::Concatenate(_, _)
            | Type::TypeVarTuple(_)
            | Type::TypePack(_)
            | Type::Unpack(_)
            | Type::Unknown
            | Type::Never => None,
        }
    }

    fn lookup_in_mro(
        &self,
        c: &ClassRef,
        args: &[Type],
        name: &str,
        is_instance: bool,
        receiver: &Type,
    ) -> Option<Type> {
        self.lookup_in_classes(self.mro_with_args(c, args), name, is_instance, receiver)
    }

    /// The classes a `super()` proxy searches (the receiver's MRO after
    /// `after`, or `after`'s own bases when the receiver does not derive
    /// from it) and whether the receiver is an instance.
    pub fn super_mro(&self, after: &ClassRef, receiver: &Type) -> Option<(MroWithArgs, bool)> {
        let (c, args, is_instance) = match receiver {
            Type::Instance(c, a) => (c, a.as_slice(), true),
            Type::SelfType(c) => (c, &[][..], true),
            Type::Class(c, a) => (c, a.as_slice(), false),
            _ => return None,
        };
        let mro = self.mro_with_args(c, args);
        let tail = match mro.iter().position(|(cls, _)| cls == after) {
            Some(i) => mro[i + 1..].to_vec(),
            None => self.mro_with_args(after, &[]).into_iter().skip(1).collect(),
        };
        Some((tail, is_instance))
    }

    /// The type of `super()` / `super(C, obj)` at `call`, when the callee is
    /// the builtin `super` and the class and receiver are known.
    fn super_call_type(
        &self,
        file: &Arc<FileInfo>,
        call: &ast::CallExpr,
        callee_ty: &Type,
    ) -> Option<Type> {
        let Type::Class(sc, _) = callee_ty else {
            return None;
        };
        if sc.name != "super" || self.builtin_class("super").as_ref() != Some(sc) {
            return None;
        }
        let args: Vec<ast::Expr> = call
            .arg_list()
            .map(|a| a.positional().collect())
            .unwrap_or_default();
        let receiver_of = |t: Type| {
            matches!(t, Type::Instance(..) | Type::Class(..) | Type::SelfType(_)).then_some(t)
        };
        match args.as_slice() {
            [] => {
                // Zero-argument form: the class enclosing the innermost function
                // (comprehensions are transparent) and that function's first parameter.
                let mut scope = self.scope_of(file, call.syntax());
                loop {
                    let s = file.scopes.scope(scope);
                    match s.kind {
                        ScopeKind::Comprehension => scope = s.parent?,
                        ScopeKind::Function => break,
                        _ => return None,
                    }
                }
                let after = self.enclosing_class(file, scope)?;
                let first = *file
                    .scopes
                    .scope(scope)
                    .bindings
                    .values()
                    .flatten()
                    .next()?;
                if file.scopes.binding(first).kind != BindingKind::Param {
                    return None;
                }
                let receiver = receiver_of(self.type_of_binding(file, first))?;
                Some(Type::Super {
                    after,
                    receiver: Box::new(receiver),
                })
            }
            [class, obj] => {
                let Type::Class(after, _) = self.type_of_expr(file, class) else {
                    return None;
                };
                let receiver = receiver_of(self.type_of_expr(file, obj))
                    .unwrap_or_else(|| Type::Instance(after.clone(), Vec::new()));
                Some(Type::Super {
                    after,
                    receiver: Box::new(receiver),
                })
            }
            _ => None,
        }
    }

    /// First definition of `name` in `classes` (an MRO or a tail of one).
    fn lookup_in_classes(
        &self,
        classes: Vec<(ClassRef, Vec<Type>)>,
        name: &str,
        is_instance: bool,
        receiver: &Type,
    ) -> Option<Type> {
        for (cls, cls_args) in classes {
            let Some(info) = self.class_info(&cls) else {
                continue;
            };
            if let Some(m) = info.members.get(name) {
                if !is_instance && m.kind == MemberKind::InstanceVar && m.value.is_none() {
                    continue;
                }
                let subst = |name: &str| self.class_substitution(&cls, &cls_args, name);
                let t = self.member_type(&cls, m, is_instance, receiver)?;
                return Some(t.substitute(&subst));
            }
        }
        None
    }

    fn member_type(
        &self,
        cls: &ClassRef,
        m: &Member,
        is_instance: bool,
        receiver: &Type,
    ) -> Option<Type> {
        let file = self.db.file(m.file)?;
        match m.kind {
            MemberKind::Method
            | MemberKind::StaticMethod
            | MemberKind::ClassMethod
            | MemberKind::Property => {
                let funcs = self.overloads_named(&file, cls, &m.name);
                if funcs.is_empty() {
                    return None;
                }
                if let [function] = funcs.as_slice() {
                    if self.has_replacement_function_decorator(&file, function) {
                        let value = self.decorated_function_value(&file, function, Some(cls));
                        let bind = matches!(m.kind, MemberKind::ClassMethod)
                            || (is_instance && matches!(m.kind, MemberKind::Method));
                        return Some(self.bind_replacement_function_value(value, receiver, bind));
                    }
                }
                let sigs: Vec<Arc<Signature>> = if funcs.len() > 1
                    && funcs
                        .iter()
                        .any(|function| self.has_replacement_function_decorator(&file, function))
                {
                    let declarations: Vec<_> = funcs.iter().collect();
                    let Some(signatures) = self.decorated_overload_signatures(
                        &file,
                        &declarations,
                        Some(cls),
                        !matches!(m.kind, MemberKind::StaticMethod),
                    ) else {
                        return Some(Type::Unknown);
                    };
                    signatures
                } else {
                    funcs
                        .iter()
                        .map(|f| {
                            self.decorated_signature(&file, f, self.signature(&file, f, Some(cls)))
                        })
                        .collect()
                };
                let bind = |sig: &Arc<Signature>, drop_first: bool| -> Arc<Signature> {
                    if drop_first && !sig.params.is_empty() {
                        Arc::new(Signature {
                            params: sig.params[1..].to_vec(),
                            ..(**sig).clone()
                        })
                    } else {
                        sig.clone()
                    }
                };
                let is_new = m.name == "__new__";
                let result = match m.kind {
                    MemberKind::StaticMethod if !is_new => {
                        if sigs.len() == 1 {
                            Type::Function(sigs[0].clone())
                        } else {
                            Type::Overloaded(sigs.clone())
                        }
                    }
                    MemberKind::Method | MemberKind::StaticMethod if is_new => {
                        let bound: Vec<Arc<Signature>> =
                            sigs.iter().map(|s| bind(s, true)).collect();
                        if bound.len() == 1 {
                            Type::BoundMethod(bound[0].clone(), Box::new(receiver.clone()))
                        } else {
                            Type::Overloaded(bound)
                        }
                    }
                    MemberKind::ClassMethod => {
                        let bound: Vec<Arc<Signature>> =
                            sigs.iter().map(|s| bind(s, true)).collect();
                        if bound.len() == 1 {
                            Type::BoundMethod(bound[0].clone(), Box::new(receiver.clone()))
                        } else {
                            Type::Overloaded(bound)
                        }
                    }
                    MemberKind::Property => {
                        if is_instance {
                            sigs[0].ret.clone()
                        } else {
                            self.instance_of("property")
                        }
                    }
                    _ => {
                        if is_instance {
                            let bound: Vec<Arc<Signature>> =
                                sigs.iter().map(|s| bind(s, true)).collect();
                            if bound.len() == 1 {
                                Type::BoundMethod(bound[0].clone(), Box::new(receiver.clone()))
                            } else {
                                Type::Overloaded(bound)
                            }
                        } else if sigs.len() == 1 {
                            Type::Function(sigs[0].clone())
                        } else {
                            Type::Overloaded(sigs.clone())
                        }
                    }
                };
                // Replace Self with the receiver type; inferred return types that
                // mention the declaring class follow the receiver's class.
                let recv_instance = match receiver {
                    Type::Class(rc, a) => Type::Instance(rc.clone(), a.clone()),
                    other => other.clone(),
                };
                let recv_class = receiver.class_ref().cloned();
                let result = result.substitute(&|n| {
                    if n == "Self" {
                        Some(recv_instance.clone())
                    } else {
                        None
                    }
                });
                let result = replace_self_type(&result, &recv_instance);
                let result = match recv_class {
                    Some(rc) if &rc != cls => specialize_self(&result, cls, &rc),
                    _ => result,
                };
                Some(result)
            }
            MemberKind::ClassVar | MemberKind::InstanceVar => {
                if let Some(a) = m.annotation {
                    let t = self.annotation_type_range(&file, a);
                    // Strip ClassVar[...] wrapper handled in annotation_type.
                    return Some(t);
                }
                match m.value {
                    Some(v) => Some(self.widen(self.type_of_range(&file, v))),
                    // Declared without a type (`__slots__` entries).
                    None => (m.kind == MemberKind::InstanceVar).then_some(Type::Unknown),
                }
            }
            MemberKind::NestedClass => Some(Type::Class(
                ClassRef {
                    file: m.file,
                    range: file.scopes.binding(m.binding?).node_range,
                    name: m.name.clone(),
                },
                vec![],
            )),
        }
    }

    /// All `def name` overloads in the class body (in order).
    fn overloads_named(
        &self,
        file: &Arc<FileInfo>,
        cls: &ClassRef,
        name: &str,
    ) -> Vec<ast::FuncDef> {
        let root = file.parse.syntax_node();
        let Some(class) = node_with_range(&root, cls.range, |n| n.kind() == SyntaxKind::CLASS_DEF)
            .and_then(ast::ClassDef::cast)
        else {
            return Vec::new();
        };
        let mut out: Vec<ast::FuncDef> = Vec::new();
        // Version/platform branches: in stubs only the live ones (see `static_cond`).
        let stmts = class
            .body()
            .map(|b| crate::static_cond::flatten_live(&b, file.static_env.as_ref()))
            .unwrap_or_default();
        for stmt in stmts {
            if let ast::Stmt::FuncDef(f) = stmt {
                // Property setters/deleters don't define the value type.
                if f.name_text() != name
                    || f.decorators().any(|d| {
                        d.name_text()
                            .is_some_and(|n| n.ends_with(".setter") || n.ends_with(".deleter"))
                    })
                {
                    continue;
                }
                out.push(f);
            }
        }
        // Keep only @overload variants if present (drop the implementation).
        let overloads: Vec<ast::FuncDef> = out
            .iter()
            .filter(|f| f.has_decorator("overload") || f.has_decorator("typing.overload"))
            .cloned()
            .collect();
        if overloads.is_empty() {
            out
        } else {
            overloads
        }
    }

    /// Members visible on a type (for completion): name → type, nearest class first.
    pub fn all_members(&self, ty: &Type) -> IndexMap<SmolStr, Type> {
        if let Some(members) = self.functools_members(ty) {
            return members;
        }
        let mut out: IndexMap<SmolStr, Type> = IndexMap::new();
        match ty {
            Type::LiteralString | Type::LiteralStringValue => {
                return self.all_members(&self.instance_of("str"))
            }
            Type::Module(fid) => {
                if let Some(m) = self.db.file(*fid) {
                    let scope = m.scopes.module_scope();
                    for (name, ids) in &m.scopes.scope(scope).bindings {
                        if let Some(id) = ids
                            .iter()
                            .rev()
                            .find(|id| m.scopes.binding(**id).is_definition())
                        {
                            out.insert(name.clone(), self.type_of_binding(&m, *id));
                        }
                    }
                    for star in &m.scopes.scope(scope).star_imports {
                        if let Some(target) = self.db.module_relative(
                            &m,
                            star.level,
                            star.module.trim_start_matches('.'),
                        ) {
                            for (name, ty) in self.star_members(&target) {
                                out.entry(name).or_insert(ty);
                            }
                        }
                    }
                    if let Some(modname) = &m.module_name {
                        for sub in self.db.resolver.submodules(modname) {
                            out.entry(SmolStr::new(&sub)).or_insert(Type::Unknown);
                        }
                    }
                }
            }
            Type::Instance(c, args) | Type::Class(c, args) => {
                let is_instance = matches!(ty, Type::Instance(..));
                for (cls, _) in self.mro_with_args(c, args) {
                    if let Some(info) = self.class_info(&cls) {
                        for (name, _) in &info.members {
                            if !out.contains_key(name) {
                                if let Some(t) = self.lookup_in_mro(c, args, name, is_instance, ty)
                                {
                                    out.insert(name.clone(), t);
                                }
                            }
                        }
                    }
                }
                if !is_instance {
                    if let Some(meta) = self.builtin_class("type") {
                        for (name, t) in self.all_members(&Type::Instance(meta, vec![])) {
                            out.entry(name).or_insert(t);
                        }
                    }
                }
            }
            Type::SelfType(c) => return self.all_members(&Type::Instance(c.clone(), vec![])),
            Type::Super { after, receiver } => {
                if let Some((classes, is_instance)) = self.super_mro(after, receiver) {
                    for (cls, _) in &classes {
                        let Some(info) = self.class_info(cls) else {
                            continue;
                        };
                        for name in info.members.keys() {
                            if !out.contains_key(name) {
                                if let Some(t) = self.lookup_in_classes(
                                    classes.clone(),
                                    name,
                                    is_instance,
                                    receiver,
                                ) {
                                    out.insert(name.clone(), t);
                                }
                            }
                        }
                    }
                }
            }
            Type::Tuple(_) => {
                return self.all_members(&Type::Instance(
                    self.builtin_class("tuple").unwrap_or_else(unknown_class),
                    vec![Type::Unknown],
                ))
            }
            Type::Literal(LiteralValue::EnumMember { class, .. }) => {
                return self.all_members(&Type::Instance(class.clone(), Vec::new()))
            }
            Type::Literal(l) => return self.all_members(&self.instance_of(literal_class_name(l))),
            Type::Function(sig) => {
                if let Some(w) = self.function_wrapper(sig) {
                    return self.all_members(&w);
                }
            }
            Type::Union(ts) => {
                for t in ts {
                    if !t.is_none() {
                        for (k, v) in self.all_members(t) {
                            out.entry(k).or_insert(v);
                        }
                    }
                }
            }
            _ => {}
        }
        out
    }

    // ----- signatures -------------------------------------------------------------

    pub fn function_type(
        &self,
        file: &Arc<FileInfo>,
        f: &ast::FuncDef,
        class: Option<&ClassRef>,
    ) -> Type {
        self.decorated_function_value(file, f, class)
    }

    pub fn signature(
        &self,
        file: &Arc<FileInfo>,
        f: &ast::FuncDef,
        class: Option<&ClassRef>,
    ) -> Arc<Signature> {
        let key = FunctionRef {
            file: file.id,
            range: f.text_range(),
            name: SmolStr::new(f.name_text()),
        };
        if let Some(s) = self.sig_cache.borrow().get(&key) {
            return s.clone();
        }
        let is_static = f.has_decorator("staticmethod");
        let params = f
            .params()
            .map(|p| self.params_from_list(file, &p, class, !is_static && f.is_method()))
            .unwrap_or_default();
        let (ret, ret_inferred) = match f.return_annotation() {
            Some(r) => {
                let t = self.annotation_type(file, &r);
                // `async def` returns a coroutine, except async generators (annotated `AsyncIterator[...]`).
                (
                    if f.is_async() && !has_own_yield(f) {
                        self.wrap_coroutine(t)
                    } else {
                        t
                    },
                    false,
                )
            }
            None => match self.stub_return_type(file, f) {
                Some(t) => (
                    if f.is_async() && !has_own_yield(f) {
                        self.wrap_coroutine(t)
                    } else {
                        t
                    },
                    false,
                ),
                None => (self.infer_return_type(file, f), true),
            },
        };
        let sig = Arc::new(Signature {
            params,
            ret,
            is_async: f.is_async(),
            def: Some(key.clone()),
            docstring: f.docstring().map(|d| Arc::from(d.value().as_str())),
            ret_inferred,
        });
        self.sig_cache.borrow_mut().insert(key, sig.clone());
        sig
    }

    pub(crate) fn documented_generator_type(
        &self,
        function: &ast::FuncDef,
        element: Type,
    ) -> Option<Type> {
        if !has_own_yield(function) {
            return None;
        }
        if function.is_async() {
            Some(Type::Instance(
                self.typing_class("AsyncGenerator")?,
                vec![element, Type::Unknown],
            ))
        } else {
            Some(Type::Instance(
                self.typing_class("Generator")?,
                vec![element, Type::Unknown, Type::Unknown],
            ))
        }
    }

    /// Yield documentation describes the element, while an explicit return
    /// annotation describes the generator/iterator object. Only actual typing
    /// wrappers may expose a payload; similarly named user classes do not.
    pub fn documentation_yield_type(&self, declared: &Type, is_async: bool) -> Type {
        let Type::Instance(class, args) = declared else {
            return Type::Unknown;
        };
        let names: &[&str] = if is_async {
            &["AsyncGenerator", "AsyncIterator", "AsyncIterable"]
        } else {
            &["Generator", "Iterator", "Iterable"]
        };
        if names
            .iter()
            .any(|name| self.typing_class(name).as_ref() == Some(class))
        {
            args.first().cloned().unwrap_or(Type::Unknown)
        } else {
            Type::Unknown
        }
    }

    fn wrap_coroutine(&self, t: Type) -> Type {
        match self.typing_class("Coroutine") {
            Some(c) => Type::Instance(c, vec![Type::Any, Type::Any, t]),
            None => t,
        }
    }

    fn params_from_list(
        &self,
        file: &Arc<FileInfo>,
        list: &ast::ParamList,
        class: Option<&ClassRef>,
        is_method: bool,
    ) -> Vec<Param> {
        let function = list
            .syntax()
            .ancestors()
            .take(32)
            .find_map(ast::FuncDef::cast);
        let mut out = Vec::new();
        let mut kw_only = false;
        let mut seen_slash_pos = None;
        for (i, p) in list.params().enumerate() {
            match &p {
                ast::AnyParam::Slash(_) => {
                    seen_slash_pos = Some(out.len());
                    continue;
                }
                ast::AnyParam::StarSep(_) => {
                    kw_only = true;
                    continue;
                }
                _ => {}
            }
            let Some(name_tok) = p.name() else { continue };
            let name = SmolStr::new(name_tok.text());
            let kind = match &p {
                ast::AnyParam::Star(_) => {
                    kw_only = true;
                    ParamKind::VarArgs
                }
                ast::AnyParam::StarStar(_) => ParamKind::KwArgs,
                _ if kw_only => ParamKind::KeywordOnly,
                _ => ParamKind::Normal,
            };
            let mut ty = match p.annotation() {
                Some(a) => self.annotation_type(file, &a),
                None => match function
                    .as_ref()
                    .and_then(|function| self.stub_parameter_type(file, function, &name))
                {
                    Some(t) => t,
                    None => match p.default() {
                        Some(d) => {
                            let t = self.widen(self.type_of_expr(file, &d));
                            if t.is_none() {
                                Type::Unknown
                            } else {
                                t
                            }
                        }
                        None => Type::Unknown,
                    },
                },
            };
            if i == 0 && is_method && p.annotation().is_none() {
                if let Some(c) = class {
                    let args = self.class_parameter_types(c);
                    ty = Type::Instance(c.clone(), args);
                }
            }
            // `*args: T` / `**kw: T` keep the element type `T` in the signature;
            // the tuple/dict wrapping applies to the parameter binding inside the body.
            let default_text = p
                .default()
                .map(|d| SmolStr::new(d.syntax().text().to_string()));
            out.push(Param {
                name,
                kind,
                ty,
                has_default: p.default().is_some(),
                default_text,
            });
        }
        if let Some(n) = seen_slash_pos {
            for p in out.iter_mut().take(n) {
                p.kind = ParamKind::PositionalOnly;
            }
        }
        out
    }

    fn infer_return_type(&self, file: &Arc<FileInfo>, f: &ast::FuncDef) -> Type {
        let t = self.infer_return_type_static(file, f);
        if matches!(t, Type::Unknown) {
            if let Some(rt) = self.runtime_return_type(file, f) {
                return rt;
            }
        }
        t
    }

    fn infer_return_type_static(&self, file: &Arc<FileInfo>, f: &ast::FuncDef) -> Type {
        let epoch = match self.cached_library_return(file, f) {
            Ok(ty) => return ty,
            Err(epoch) => epoch,
        };
        let (ty, complete) = self.infer_return_type_body(file, f);
        if let (Some(epoch), true) = (epoch, complete) {
            self.store_library_return(file, f, epoch, &ty);
        }
        ty
    }

    /// The return type of `f`'s body, and whether it is final: no unfinished
    /// enclosing computation (a cut cycle) contributed to it.
    fn infer_return_type_body(&self, file: &Arc<FileInfo>, f: &ast::FuncDef) -> (Type, bool) {
        if file.is_stub {
            return (Type::Unknown, true);
        }
        let Some(body) = f.body() else {
            return (Type::Unknown, true);
        };
        let key = (file.id, f.text_range(), GuardKind::Return);
        if !self.enter(key) {
            return (Type::Unknown, false);
        }
        let mut returns: Vec<Type> = Vec::new();
        let mut yields: Vec<Type> = Vec::new();
        let mut has_bare_return = false;
        let mut skip_end: Option<TextSize> = None;
        for n in body.syntax().descendants().skip(1) {
            if let Some(end) = skip_end {
                if n.text_range().start() < end {
                    continue;
                }
                skip_end = None;
            }
            match n.kind() {
                SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF | SyntaxKind::LAMBDA_EXPR => {
                    skip_end = Some(n.text_range().end());
                    continue;
                }
                SyntaxKind::RETURN_STMT => {
                    let r = ast::ReturnStmt::cast(n.clone()).unwrap();
                    match r.value() {
                        Some(v) => returns.push(self.widen(self.type_of_expr(file, &v))),
                        None => has_bare_return = true,
                    }
                }
                SyntaxKind::YIELD_EXPR => {
                    let y = ast::YieldExpr::cast(n.clone()).unwrap();
                    yields.push(
                        y.value()
                            .map(|v| self.widen(self.type_of_expr(file, &v)))
                            .unwrap_or(Type::None),
                    );
                }
                SyntaxKind::YIELD_FROM_EXPR => {
                    let y = ast::YieldFromExpr::cast(n.clone()).unwrap();
                    yields.push(
                        y.expr()
                            .map(|v| self.iter_type(&self.type_of_expr(file, &v)))
                            .unwrap_or(Type::Unknown),
                    );
                }
                _ => {}
            }
        }
        let complete = self.leave(key).is_none();
        if !yields.is_empty() {
            let y = Type::union(yields);
            let r = if returns.is_empty() {
                Type::None
            } else {
                Type::union(returns)
            };
            let gen = if f.is_async() {
                self.typing_class("AsyncGenerator")
                    .map(|g| Type::Instance(g, vec![y, Type::None]))
            } else {
                self.typing_class("Generator")
                    .map(|g| Type::Instance(g, vec![y, Type::None, r]))
            };
            return (gen.unwrap_or(Type::Unknown), complete);
        }
        let falls_through = !ends_with_terminal(&body);
        if has_bare_return || falls_through {
            returns.push(Type::None);
        }
        let ret = if returns.is_empty() {
            Type::None
        } else {
            Type::union(returns)
        };
        let ret = if f.is_async() {
            self.wrap_coroutine(ret)
        } else {
            ret
        };
        (ret, complete)
    }

    // ----- annotations ----------------------------------------------------------------

    pub fn annotation_type_range(&self, file: &Arc<FileInfo>, range: TextRange) -> Type {
        let root = file.parse.syntax_node();
        let node = node_with_range(&root, range, |a| ast::Expr::can_cast(a.kind()));
        match node.and_then(ast::Expr::cast) {
            Some(e) => self.annotation_type(file, &e),
            None => Type::Unknown,
        }
    }

    /// Interprets `expr` as a type expression.
    pub fn annotation_type(&self, file: &Arc<FileInfo>, expr: &ast::Expr) -> Type {
        let key = (file.id, expr.text_range(), GuardKind::Annotation);
        if !self.enter(key) {
            return Type::Unknown;
        }
        let t = self.annotation_type_inner(file, expr);
        self.leave(key);
        t
    }

    fn annotation_type_inner(&self, file: &Arc<FileInfo>, expr: &ast::Expr) -> Type {
        match expr {
            ast::Expr::Star(star) => star
                .expr()
                .map(|operand| self.unpacked_annotation(file, &operand))
                .unwrap_or(Type::Unknown),
            ast::Expr::Literal(l) if l.kind() == Some(SyntaxKind::NONE_KW) => Type::None,
            ast::Expr::Literal(_) => Type::Unknown,
            ast::Expr::String(s) => {
                // Forward reference: parse the string as an expression.
                let text = s.value();
                let p = rustpy_syntax::parse_expression(&text);
                let Some(inner) = p.syntax_node().children().find_map(ast::Expr::cast) else {
                    return Type::Unknown;
                };
                self.annotation_type_in_scope(file, &inner, self.scope_of(file, expr.syntax()))
            }
            ast::Expr::Paren(p) => p
                .inner()
                .map(|i| self.annotation_type(file, &i))
                .unwrap_or(Type::Unknown),
            ast::Expr::Name(_) | ast::Expr::Attribute(_) => {
                let special = self.special_form_name(file, expr);
                match special.as_deref() {
                    Some("Any") => return Type::Any,
                    Some("Never") | Some("NoReturn") => return Type::Never,
                    Some("Self") => {
                        let scope = self.scope_of(file, expr.syntax());
                        let mut s = Some(scope);
                        while let Some(id) = s {
                            let sc = file.scopes.scope(id);
                            if sc.kind == ScopeKind::Class {
                                return Type::SelfType(ClassRef {
                                    file: file.id,
                                    range: sc.range,
                                    name: sc.name.clone().unwrap_or_default(),
                                });
                            }
                            s = sc.parent;
                        }
                        return Type::Unknown;
                    }
                    Some("LiteralString") => {
                        let scope = self.scope_of(file, expr.syntax());
                        if self.scoped_qualifier_name(file, scope, expr).as_deref()
                            == Some("LiteralString")
                        {
                            return Type::LiteralString;
                        }
                    }
                    Some("List") => {
                        return Type::Instance(
                            self.builtin_class("list").unwrap_or_else(unknown_class),
                            vec![Type::Unknown],
                        )
                    }
                    Some("Dict") => {
                        return Type::Instance(
                            self.builtin_class("dict").unwrap_or_else(unknown_class),
                            vec![Type::Unknown, Type::Unknown],
                        )
                    }
                    Some("Set") => {
                        return Type::Instance(
                            self.builtin_class("set").unwrap_or_else(unknown_class),
                            vec![Type::Unknown],
                        )
                    }
                    Some("FrozenSet") => {
                        return Type::Instance(
                            self.builtin_class("frozenset")
                                .unwrap_or_else(unknown_class),
                            vec![Type::Unknown],
                        )
                    }
                    Some("Tuple") => {
                        return Type::Instance(
                            self.builtin_class("tuple").unwrap_or_else(unknown_class),
                            vec![Type::Unknown],
                        )
                    }
                    Some("Type") => {
                        return Type::Instance(
                            self.builtin_class("type").unwrap_or_else(unknown_class),
                            vec![],
                        )
                    }
                    Some(
                        name @ ("Counter" | "OrderedDict" | "ChainMap" | "DefaultDict" | "Deque"),
                    ) => {
                        if let Some(instance) = self.collection_alias_instance(name, &[]) {
                            return instance;
                        }
                    }
                    Some("Callable") => {
                        return Type::Callable(Arc::new(Signature {
                            params: vec![Param {
                                name: "args".into(),
                                kind: ParamKind::VarArgs,
                                ty: Type::Unknown,
                                has_default: false,
                                default_text: None,
                            }],
                            ret: Type::Unknown,
                            is_async: false,
                            def: None,
                            docstring: None,
                            ret_inferred: false,
                        }))
                    }
                    _ => {}
                }
                // Type aliases (`StrPath: TypeAlias = str | PathLike[str]`, `Alias = list[int]`)
                // are interpreted as type expressions before falling back to runtime values.
                if let Some(t) = self.alias_target(file, expr) {
                    return t;
                }
                match self.type_of_expr(file, expr) {
                    Type::Class(c, args) => {
                        let args = self.complete_class_defaults(&c, &args);
                        Type::Instance(c, args)
                    }
                    Type::TypeVar(n) => Type::TypeVar(n),
                    Type::DeclaredTypeVar(definition) => Type::DeclaredTypeVar(definition),
                    Type::None => Type::None,
                    Type::Instance(c, _) if self.canonical_typevar_class(&c) => {
                        // `T = TypeVar("T")` resolved through a binding whose value call we did not special-case.
                        Type::TypeVar(SmolStr::new(expr.syntax().text().to_string()))
                    }
                    _ => Type::Unknown,
                }
            }
            ast::Expr::Subscript(s) => {
                let Some(obj) = s.object() else {
                    return Type::Unknown;
                };
                let args = self.subscript_args(file, s);
                let special = self.special_form_name(file, &obj);
                match special.as_deref() {
                    Some("Optional") => {
                        return Type::union(vec![
                            args.into_iter().next().unwrap_or(Type::Unknown),
                            Type::None,
                        ])
                    }
                    Some("Union") => return Type::union(args),
                    Some("List") => {
                        return Type::Instance(
                            self.builtin_class("list").unwrap_or_else(unknown_class),
                            args,
                        )
                    }
                    Some("Dict") => {
                        return Type::Instance(
                            self.builtin_class("dict").unwrap_or_else(unknown_class),
                            args,
                        );
                    }
                    Some(
                        name @ ("Counter" | "OrderedDict" | "ChainMap" | "DefaultDict" | "Deque"),
                    ) => {
                        if let Some(instance) = self.collection_alias_instance(name, &args) {
                            return instance;
                        }
                    }
                    Some("Set") => {
                        return Type::Instance(
                            self.builtin_class("set").unwrap_or_else(unknown_class),
                            args,
                        )
                    }
                    Some("FrozenSet") => {
                        return Type::Instance(
                            self.builtin_class("frozenset")
                                .unwrap_or_else(unknown_class),
                            args,
                        )
                    }
                    Some("Tuple") => {
                        let is_variadic = s
                            .index()
                            .map(|i| i.text().to_string().contains("..."))
                            .unwrap_or(false);
                        return if is_variadic {
                            Type::Instance(
                                self.builtin_class("tuple").unwrap_or_else(unknown_class),
                                vec![args.into_iter().next().unwrap_or(Type::Unknown)],
                            )
                        } else {
                            Type::Tuple(args)
                        };
                    }
                    Some("Type") => {
                        return match args.first().and_then(|a| self.class_object_form(a)) {
                            Some(form) => form,
                            None => self.instance_of("type"),
                        }
                    }
                    Some("Callable") => {
                        let params_node = s.index().and_then(|i| {
                            if i.kind() == SyntaxKind::SLICE_LIST {
                                i.children().next()
                            } else {
                                None
                            }
                        });
                        let params: Vec<Param> = match params_node.and_then(ast::Expr::cast) {
                            Some(ast::Expr::List(l)) => l
                                .elements()
                                .enumerate()
                                .map(|(i, e)| Param {
                                    name: SmolStr::new(format!("p{i}")),
                                    kind: ParamKind::PositionalOnly,
                                    ty: self.annotation_type(file, &e),
                                    has_default: false,
                                    default_text: None,
                                })
                                .collect(),
                            Some(expression) if !matches!(expression.clone().unparen(), ast::Expr::Literal(literal) if literal.kind() == Some(SyntaxKind::ELLIPSIS)) =>
                            {
                                if !self.canonical_callable_annotation(file, &obj) {
                                    return Type::Unknown;
                                }
                                let Some(specification) =
                                    self.parameter_specification_annotation(file, &expression)
                                else {
                                    return Type::Unknown;
                                };
                                vec![Param {
                                    name: "args".into(),
                                    kind: ParamKind::VarArgs,
                                    ty: specification,
                                    has_default: false,
                                    default_text: None,
                                }]
                            }
                            _ => vec![Param {
                                name: "args".into(),
                                kind: ParamKind::VarArgs,
                                ty: Type::Unknown,
                                has_default: false,
                                default_text: None,
                            }],
                        };
                        let ret = args.last().cloned().unwrap_or(Type::Unknown);
                        return Type::Callable(Arc::new(Signature {
                            params,
                            ret,
                            is_async: false,
                            def: None,
                            docstring: None,
                            ret_inferred: false,
                        }));
                    }
                    Some("Literal") => {
                        let lits: Vec<Type> = s
                            .index()
                            .into_iter()
                            .flat_map(|i| {
                                if i.kind() == SyntaxKind::SLICE_LIST {
                                    i.children().filter_map(ast::Expr::cast).collect::<Vec<_>>()
                                } else {
                                    ast::Expr::cast(i).into_iter().collect()
                                }
                            })
                            .map(|e| match self.type_of_expr(file, &e) {
                                Type::Literal(l) => Type::Literal(l),
                                Type::None => Type::None,
                                other => self.widen(other),
                            })
                            .collect();
                        return Type::union(lits);
                    }
                    Some("Unpack") => {
                        if !self.unpack_symbol_is_canonical(file, &obj) {
                            return Type::Unknown;
                        }
                        let raw: Vec<_> = s
                            .index()
                            .into_iter()
                            .flat_map(|index| {
                                if index.kind() == SyntaxKind::SLICE_LIST {
                                    index
                                        .children()
                                        .filter_map(ast::Expr::cast)
                                        .collect::<Vec<_>>()
                                } else {
                                    ast::Expr::cast(index).into_iter().collect()
                                }
                            })
                            .take(2)
                            .collect();
                        return if raw.len() == 1 {
                            self.unpacked_annotation(file, &raw[0])
                        } else {
                            Type::Unknown
                        };
                    }
                    Some("Final") | Some("ClassVar") | Some("Annotated") | Some("Required")
                    | Some("NotRequired") | Some("ReadOnly") | Some("TypeGuard")
                    | Some("TypeIs") => {
                        if matches!(special.as_deref(), Some("TypeGuard") | Some("TypeIs")) {
                            return self.instance_of("bool");
                        }
                        return args.into_iter().next().unwrap_or(Type::Unknown);
                    }
                    _ => {}
                }
                if let Some((target, parameters)) = self.generic_alias(file, &obj) {
                    if !parameters.is_empty() {
                        return self.specialize_alias(&target, &parameters, &args);
                    }
                }
                let is_variadic = s
                    .index()
                    .map(|i| i.text().to_string().contains("..."))
                    .unwrap_or(false);
                match self.type_of_expr(file, &obj) {
                    // `type[A | B]` / `type[T]`: class objects, not `type` instances.
                    Type::Class(c, _) if args.len() == 1 && self.is_builtin_type_class(&c) => {
                        match self.class_object_form(&args[0]) {
                            Some(form) => form,
                            None => Type::Instance(c, args),
                        }
                    }
                    Type::Class(c, _)
                        if c.name == "tuple"
                            && self.builtin_class("tuple").as_ref() == Some(&c) =>
                    {
                        if is_variadic {
                            Type::Instance(
                                c,
                                vec![args.into_iter().next().unwrap_or(Type::Unknown)],
                            )
                        } else {
                            Type::Tuple(args)
                        }
                    }
                    Type::Class(c, _) => {
                        let args = self.complete_class_defaults(&c, &args);
                        Type::Instance(c, args)
                    }
                    _ => match self.alias_target(file, &obj) {
                        Some(Type::Instance(c, _)) => {
                            let args = self.complete_class_defaults(&c, &args);
                            Type::Instance(c, args)
                        }
                        _ => Type::Unknown,
                    },
                }
            }
            ast::Expr::Binary(b) if b.op().map(|t| t.kind()) == Some(SyntaxKind::PIPE) => {
                Type::union(vec![
                    b.lhs()
                        .map(|l| self.annotation_type(file, &l))
                        .unwrap_or(Type::Unknown),
                    b.rhs()
                        .map(|r| self.annotation_type(file, &r))
                        .unwrap_or(Type::Unknown),
                ])
            }
            ast::Expr::Tuple(t) => Type::Tuple(
                t.elements()
                    .map(|e| self.annotation_type(file, &e))
                    .collect(),
            ),
            ast::Expr::List(l) => Type::Tuple(
                l.elements()
                    .map(|e| self.annotation_type(file, &e))
                    .collect(),
            ),
            _ => Type::Unknown,
        }
    }

    fn scoped_qualifier_name(
        &self,
        file: &Arc<FileInfo>,
        scope: ScopeId,
        head: &ast::Expr,
    ) -> Option<String> {
        let (name, member) = match head.clone().unparen() {
            ast::Expr::Name(name) => (name, None),
            ast::Expr::Attribute(attribute) => {
                let ast::Expr::Name(name) = attribute.object()?.unparen() else {
                    return None;
                };
                (name, Some(attribute.attr_text()))
            }
            _ => return None,
        };
        // Forward references resolve in the provided lexical scope. Its end is
        // the lookup cutoff; alias RHS captures use their own earlier cutoff.
        let before = file.scopes.scope(scope).range.end();
        let ids: Vec<_> = file
            .scopes
            .resolve(scope, &name.name_text())?
            .iter()
            .copied()
            .filter(|id| file.scopes.binding(*id).range.start() < before)
            .collect();
        if ids.len() != 1 {
            return None;
        }
        self.scoped_qualifier_binding(
            file,
            ids[0],
            member.as_deref(),
            before,
            &mut rustc_hash::FxHashSet::default(),
        )
    }
    fn scoped_qualifier_binding(
        &self,
        file: &Arc<FileInfo>,
        id: BindingId,
        member: Option<&str>,
        before: TextSize,
        seen: &mut rustc_hash::FxHashSet<(FileId, BindingId)>,
    ) -> Option<String> {
        if seen.len() >= 16 || !seen.insert((file.id, id)) {
            return None;
        }
        let binding = file.scopes.binding(id);
        if binding.range.start() >= before
            || !file.scopes.scope(binding.scope).star_imports.is_empty()
        {
            return None;
        }
        let bundled = file.is_stub
            && matches!(
                file.module_name.as_deref(),
                Some("typing" | "typing_extensions")
            )
            && file.path.to_string_lossy()
                == format!("<typeshed>/{}.pyi", file.module_name.as_deref()?);
        if member.is_none()
            && bundled
            && matches!(binding.kind, BindingKind::Annotation)
            && matches!(
                binding.name.as_str(),
                "Final"
                    | "Annotated"
                    | "Required"
                    | "NotRequired"
                    | "ReadOnly"
                    | "LiteralString"
                    | "ClassVar"
            )
        {
            return Some(binding.name.to_string());
        }
        if let BindingKind::Import(import) = &binding.kind {
            let symbol = match (import.name.as_deref(), member) {
                (Some(name), None) | (None, Some(name)) => name,
                _ => return None,
            };
            let target = self.db.module_relative(
                file,
                import.level,
                import.module.trim_start_matches('.'),
            )?;
            let ids = target
                .scopes
                .bindings_in(target.scopes.module_scope(), symbol);
            if ids.len() != 1 {
                // Bundled backport declarations can expose the same canonical
                // qualifier through version-gated import/annotation alternatives.
                let trusted = target.is_stub
                    && matches!(
                        target.module_name.as_deref(),
                        Some("typing" | "typing_extensions")
                    )
                    && target.path.to_string_lossy()
                        == format!("<typeshed>/{}.pyi", target.module_name.as_deref()?);
                if trusted
                    && !ids.is_empty()
                    && matches!(
                        symbol,
                        "Final"
                            | "Annotated"
                            | "Required"
                            | "NotRequired"
                            | "ReadOnly"
                            | "LiteralString"
                            | "ClassVar"
                    )
                    && ids.iter().all(|id| {
                        let alternative = target.scopes.binding(*id);
                        alternative.name == symbol
                            && match &alternative.kind {
                                BindingKind::Annotation => true,
                                BindingKind::Import(import) => {
                                    matches!(import.module.as_str(), "typing" | "typing_extensions")
                                        && import.name.as_deref() == Some(symbol)
                                }
                                _ => false,
                            }
                    })
                {
                    return Some(symbol.to_owned());
                }
                return None;
            }
            return self.scoped_qualifier_binding(
                &target,
                ids[0],
                None,
                target
                    .scopes
                    .scope(target.scopes.module_scope())
                    .range
                    .end(),
                seen,
            );
        }
        if member.is_some() {
            return None;
        }
        if matches!(binding.kind, BindingKind::Assignment) {
            let expression = node_with_range(&file.parse.syntax_node(), binding.value?, |node| {
                ast::Expr::can_cast(node.kind())
            })
            .and_then(ast::Expr::cast)?
            .unparen();
            let (name, member) = match expression {
                ast::Expr::Name(name) => (name, None),
                ast::Expr::Attribute(attribute) => {
                    let ast::Expr::Name(name) = attribute.object()?.unparen() else {
                        return None;
                    };
                    (name, Some(attribute.attr_text()))
                }
                _ => return None,
            };
            let ids: Vec<_> = file
                .scopes
                .resolve(binding.scope, &name.name_text())?
                .iter()
                .copied()
                .filter(|id| file.scopes.binding(*id).range.start() < binding.range.start())
                .collect();
            if ids.len() != 1 {
                return None;
            }
            return self.scoped_qualifier_binding(
                file,
                ids[0],
                member.as_deref(),
                binding.range.start(),
                seen,
            );
        }
        None
    }

    pub(crate) fn annotation_type_in_scope(
        &self,
        file: &Arc<FileInfo>,
        expr: &ast::Expr,
        scope: ScopeId,
    ) -> Type {
        if self.scoped_qualifier_name(file, scope, expr).as_deref() == Some("LiteralString") {
            return Type::LiteralString;
        }
        // Forward-reference strings: resolve simple names in the given scope.
        match expr {
            ast::Expr::Name(n) => match self.resolve_name(file, scope, &n.name_text()) {
                Some(def) => match self.type_of_binding(&def.file, def.binding) {
                    Type::Class(c, _) => {
                        let args = self.complete_class_defaults(&c, &[]);
                        Type::Instance(c, args)
                    }
                    Type::TypeVar(t) => Type::TypeVar(t),
                    Type::DeclaredTypeVar(definition) => Type::DeclaredTypeVar(definition),
                    Type::None => Type::None,
                    _ => Type::Unknown,
                },
                None => match n.name_text().as_str() {
                    "None" => Type::None,
                    "Any" => Type::Any,
                    _ => Type::Unknown,
                },
            },
            ast::Expr::Subscript(s) => {
                // Reparsed quotes retain their physical declaration namespace.
                // Only actual bundled qualifiers can expose a payload; names
                // alone must not unwrap user classes called Final/Annotated.
                if let Some(head) = s.object() {
                    if let Some(qualifier) = self.scoped_qualifier_name(file, scope, &head) {
                        let arguments: Vec<_> = match s.index() {
                            Some(index) if index.kind() == SyntaxKind::SLICE_LIST => index
                                .children()
                                .filter_map(ast::Expr::cast)
                                .take(17)
                                .collect(),
                            Some(index) => match ast::Expr::cast(index) {
                                Some(ast::Expr::Tuple(tuple)) => {
                                    tuple.elements().take(17).collect()
                                }
                                Some(expression) => vec![expression],
                                None => Vec::new(),
                            },
                            None => Vec::new(),
                        };
                        let valid = if qualifier == "Annotated" {
                            arguments.len() >= 2 && arguments.len() <= 16
                        } else {
                            arguments.len() == 1
                        };
                        return if valid {
                            self.annotation_type_in_scope(file, &arguments[0], scope)
                        } else {
                            Type::Unknown
                        };
                    }
                }
                let base = s
                    .object()
                    .map(|o| self.annotation_type_in_scope(file, &o, scope))
                    .unwrap_or(Type::Unknown);
                let args: Vec<Type> = match s.index() {
                    Some(idx) if idx.kind() == SyntaxKind::SLICE_LIST => idx
                        .children()
                        .filter_map(ast::Expr::cast)
                        .map(|e| self.annotation_type_in_scope(file, &e, scope))
                        .collect(),
                    Some(idx) => ast::Expr::cast(idx)
                        .map(|e| vec![self.annotation_type_in_scope(file, &e, scope)])
                        .unwrap_or_default(),
                    None => Vec::new(),
                };
                let base_name = s
                    .object()
                    .map(|o| o.syntax().text().to_string())
                    .unwrap_or_default();
                match base_name.rsplit('.').next().unwrap_or("") {
                    "Optional" => Type::union(vec![
                        args.into_iter().next().unwrap_or(Type::Unknown),
                        Type::None,
                    ]),
                    "Union" => Type::union(args),
                    "List" => Type::Instance(
                        self.builtin_class("list").unwrap_or_else(unknown_class),
                        args,
                    ),
                    "Dict" => Type::Instance(
                        self.builtin_class("dict").unwrap_or_else(unknown_class),
                        args,
                    ),
                    _ => match base {
                        Type::Instance(c, _)
                            if args.len() == 1 && self.is_builtin_type_class(&c) =>
                        {
                            self.class_object_form(&args[0])
                                .unwrap_or(Type::Instance(c, args))
                        }
                        Type::Instance(c, _) => {
                            let args = self.complete_class_defaults(&c, &args);
                            Type::Instance(c, args)
                        }
                        other => other,
                    },
                }
            }
            ast::Expr::Binary(b) if b.op().map(|t| t.kind()) == Some(SyntaxKind::PIPE) => {
                Type::union(vec![
                    b.lhs()
                        .map(|l| self.annotation_type_in_scope(file, &l, scope))
                        .unwrap_or(Type::Unknown),
                    b.rhs()
                        .map(|r| self.annotation_type_in_scope(file, &r, scope))
                        .unwrap_or(Type::Unknown),
                ])
            }
            ast::Expr::Literal(l) if l.kind() == Some(SyntaxKind::NONE_KW) => Type::None,
            ast::Expr::Attribute(_) => self.annotation_type(file, expr),
            _ => Type::Unknown,
        }
    }

    /// If `expr` names a `typing` special form (possibly via `typing.X`, aliases or
    /// `typing_extensions`), returns its canonical name.
    fn special_form_name(&self, file: &Arc<FileInfo>, expr: &ast::Expr) -> Option<String> {
        let (name, scope) = match expr {
            ast::Expr::Name(n) => (n.name_text(), self.scope_of(file, n.syntax())),
            ast::Expr::Attribute(a) => {
                let attr = a.attr_text();
                let obj = a.object()?;
                let module_name = match self.type_of_expr(file, &obj) {
                    Type::Module(fid) => self.db.file(fid).and_then(|m| m.module_name.clone()),
                    _ => None,
                }?;
                return if matches!(
                    module_name.as_str(),
                    "typing" | "typing_extensions" | "collections.abc" | "collections"
                ) && BUILTIN_SPECIAL.contains(&attr.as_str())
                {
                    Some(attr)
                } else {
                    None
                };
            }
            _ => return None,
        };
        let def = self.resolve_name(file, scope, &name)?;
        let def = self.follow_imports(def);
        let module = def.file.module_name.as_deref()?;
        let b = def.binding();
        if matches!(
            module,
            "typing"
                | "typing_extensions"
                | "_typeshed"
                | "builtins"
                | "collections.abc"
                | "collections"
        ) && BUILTIN_SPECIAL.contains(&b.name.as_str())
        {
            return Some(b.name.to_string());
        }
        None
    }

    /// Follows a plain assignment alias (`Alias = list[int]`) as a type
    /// expression; a generic alias used bare takes its defaults (else `Any`).
    fn alias_target(&self, file: &Arc<FileInfo>, expr: &ast::Expr) -> Option<Type> {
        self.generic_alias(file, expr)
            .map(|(target, parameters)| self.specialize_alias(&target, &parameters, &[]))
    }
}

/// Replaces `typing.Self` occurrences with the receiver instance type.
fn replace_self_type(ty: &Type, recv: &Type) -> Type {
    match ty {
        Type::SelfType(_) => recv.clone(),
        Type::Instance(c, a) => Type::Instance(
            c.clone(),
            a.iter().map(|x| replace_self_type(x, recv)).collect(),
        ),
        Type::Class(c, a) => Type::Class(
            c.clone(),
            a.iter().map(|x| replace_self_type(x, recv)).collect(),
        ),
        Type::Tuple(ts) => Type::Tuple(ts.iter().map(|x| replace_self_type(x, recv)).collect()),
        Type::Union(ts) => Type::union(ts.iter().map(|x| replace_self_type(x, recv)).collect()),
        Type::Function(sig) => Type::Function(Arc::new(replace_self_sig(sig, recv))),
        Type::Callable(sig) => Type::Callable(Arc::new(replace_self_sig(sig, recv))),
        Type::BoundMethod(sig, r) => {
            Type::BoundMethod(Arc::new(replace_self_sig(sig, recv)), r.clone())
        }
        Type::Overloaded(sigs) => Type::Overloaded(
            sigs.iter()
                .map(|s| Arc::new(replace_self_sig(s, recv)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn replace_self_sig(sig: &Signature, recv: &Type) -> Signature {
    Signature {
        params: sig
            .params
            .iter()
            .map(|p| Param {
                ty: replace_self_type(&p.ty, recv),
                ..p.clone()
            })
            .collect(),
        ret: replace_self_type(&sig.ret, recv),
        ..sig.clone()
    }
}

/// Rewrites inferred return types mentioning `declaring` into `receiver` (self-type
/// covariance for inherited methods returning `self`/`cls(...)`).
fn specialize_self(ty: &Type, declaring: &ClassRef, receiver: &ClassRef) -> Type {
    let map = |t: &Type| -> Type {
        match t {
            Type::Instance(c, a) if c == declaring => Type::Instance(receiver.clone(), a.clone()),
            Type::Class(c, a) if c == declaring => Type::Class(receiver.clone(), a.clone()),
            Type::SelfType(c) if c == declaring => Type::Instance(receiver.clone(), vec![]),
            Type::Union(ts) => Type::union(
                ts.iter()
                    .map(|x| specialize_self(x, declaring, receiver))
                    .collect(),
            ),
            other => other.clone(),
        }
    };
    match ty {
        Type::Function(sig) if sig.ret_inferred => Type::Function(Arc::new(Signature {
            ret: map(&sig.ret),
            ..(**sig).clone()
        })),
        Type::BoundMethod(sig, r) if sig.ret_inferred => Type::BoundMethod(
            Arc::new(Signature {
                ret: map(&sig.ret),
                ..(**sig).clone()
            }),
            r.clone(),
        ),
        Type::Overloaded(sigs) => Type::Overloaded(
            sigs.iter()
                .map(|s| {
                    if s.ret_inferred {
                        Arc::new(Signature {
                            ret: map(&s.ret),
                            ..(**s).clone()
                        })
                    } else {
                        s.clone()
                    }
                })
                .collect(),
        ),
        other => map(other),
    }
}

/// Finds the node with exactly `range` (optionally filtered by `pred`).
fn node_with_range(
    root: &SyntaxNode,
    range: TextRange,
    pred: impl Fn(&SyntaxNode) -> bool,
) -> Option<SyntaxNode> {
    let el = root.covering_element(range);
    let start = match el {
        rustpy_syntax::rowan::NodeOrToken::Node(n) => n,
        rustpy_syntax::rowan::NodeOrToken::Token(t) => t.parent()?,
    };
    start
        .ancestors()
        .find(|a| a.text_range() == range && pred(a))
}

fn unknown_class() -> ClassRef {
    ClassRef {
        file: FileId(0),
        range: TextRange::empty(TextSize::new(0)),
        name: "Unknown".into(),
    }
}

fn literal_class_name(l: &LiteralValue) -> &'static str {
    match l {
        LiteralValue::Int(_) => "int",
        LiteralValue::Str(_) => "str",
        LiteralValue::Bytes(_) => "bytes",
        LiteralValue::Bool(_) => "bool",
        LiteralValue::EnumMember { .. } => "object",
    }
}

fn parse_int(text: &str) -> Option<i64> {
    let t: String = text.chars().filter(|c| *c != '_').collect();
    let lower = t.to_ascii_lowercase();
    if let Some(h) = lower.strip_prefix("0x") {
        return i64::from_str_radix(h, 16).ok();
    }
    if let Some(o) = lower.strip_prefix("0o") {
        return i64::from_str_radix(o, 8).ok();
    }
    if let Some(b) = lower.strip_prefix("0b") {
        return i64::from_str_radix(b, 2).ok();
    }
    t.parse().ok()
}

/// Whether `f`'s own body (not nested functions, lambdas or classes) yields.
pub(crate) fn has_own_yield(f: &ast::FuncDef) -> bool {
    let Some(body) = f.body() else { return false };
    let own = |n: &SyntaxNode| {
        !n.ancestors().take_while(|a| a != f.syntax()).any(|a| {
            matches!(
                a.kind(),
                SyntaxKind::FUNC_DEF | SyntaxKind::LAMBDA_EXPR | SyntaxKind::CLASS_DEF
            )
        })
    };
    body.syntax().descendants().any(|n| {
        matches!(
            n.kind(),
            SyntaxKind::YIELD_EXPR | SyntaxKind::YIELD_FROM_EXPR
        ) && own(&n)
    })
}

/// Whether a block always ends in return/raise (so no implicit `None`).
fn ends_with_terminal(block: &ast::Block) -> bool {
    let Some(last) = block.statements().last() else {
        return false;
    };
    match last {
        ast::Stmt::Return(_) | ast::Stmt::Raise(_) => true,
        ast::Stmt::If(i) => {
            i.else_clause().is_some()
                && i.body().is_some_and(|b| ends_with_terminal(&b))
                && i.elif_clauses()
                    .all(|e| e.body().is_some_and(|b| ends_with_terminal(&b)))
                && i.else_clause()
                    .and_then(|e| e.body())
                    .is_some_and(|b| ends_with_terminal(&b))
        }
        ast::Stmt::Try(t) => {
            t.body().is_some_and(|b| ends_with_terminal(&b))
                && t.except_clauses()
                    .all(|e| e.body().is_some_and(|b| ends_with_terminal(&b)))
        }
        ast::Stmt::While(w) => w.condition().is_some_and(|c| c.syntax().text() == "True"),
        ast::Stmt::With(w) => w.body().is_some_and(|b| ends_with_terminal(&b)),
        ast::Stmt::Match(m) => {
            m.cases()
                .all(|c| c.body().is_some_and(|b| ends_with_terminal(&b)))
                && m.cases().any(|c| {
                    c.pattern().is_some_and(|p| {
                        p.kind() == SyntaxKind::MATCH_WILDCARD_PATTERN
                            || p.kind() == SyntaxKind::MATCH_CAPTURE_PATTERN
                    })
                })
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Infers the type of the expression on the last line (an expression statement).
    fn infer(src: &str) -> String {
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let file = db.file(id).unwrap();
        let inf = Inferer::new(&db);
        let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
        let last = module.statements().last().unwrap();
        let ast::Stmt::Expr(e) = last else {
            panic!("last statement must be an expression")
        };
        inf.type_of_expr(&file, &e.expr().unwrap()).to_string()
    }

    #[test]
    fn runtime_types_fill_unannotated_parameters_and_returns() {
        let db = Analysis::for_project(Path::new("/proj"));
        let src = "class Box:\n    def scale(self, factor):\n        return factor\n\n\ndef area(w, h=None):\n    w\n    return compute(w, h)\n\n\nBox().scale(2)\narea(1)\n";
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let json = r#"{"functions":{"/proj/m.py::area":{"params":{"w":["int","float"],"h":["NoneType"]},"return":["float"]},"/proj/m.py::Box.scale":{"params":{"factor":["int"]},"return":["int"]}}}"#;
        db.set_runtime_types(crate::runtime_types::RuntimeTypes::from_json(json));
        let file = db.file(id).unwrap();
        let inf = Inferer::new(&db);
        let module = ast::Module::cast(file.parse.syntax_node()).unwrap();
        let stmts: Vec<ast::Stmt> = module.statements().collect();
        let ty = |e: &ast::Stmt| match e {
            ast::Stmt::Expr(x) => inf.type_of_expr(&file, &x.expr().unwrap()).to_string(),
            _ => panic!("expression expected"),
        };
        // `area(1)`: unknown static return → the observed float.
        assert_eq!(ty(stmts.last().unwrap()), "float");
        // `Box().scale(2)` returns `factor`, typed from the run-time int.
        assert_eq!(ty(&stmts[stmts.len() - 2]), "int");
        // Without collected data nothing changes.
        db.set_runtime_types(Default::default());
        let inf2 = Inferer::new(&db);
        let last = match stmts.last().unwrap() {
            ast::Stmt::Expr(x) => inf2.type_of_expr(&file, &x.expr().unwrap()).to_string(),
            _ => unreachable!(),
        };
        assert_ne!(last, "float");
    }

    #[test]
    fn literals_and_builtins() {
        assert_eq!(infer("1\n"), "Literal[1]");
        assert_eq!(infer("x = 1\nx\n"), "int");
        assert_eq!(infer("'a'.upper()\n"), "str");
        assert_eq!(infer("len([1])\n"), "int");
        assert_eq!(infer("[1, 2]\n"), "list[int]");
        assert_eq!(infer("[1, 'a']\n"), "list[int | str]");
        assert_eq!(infer("{'a': 1}\n"), "dict[str, int]");
        assert_eq!(infer("(1, 'a')\n"), "tuple[int, str]");
        assert_eq!(infer("x = [1, 2]\nx[0]\n"), "int");
        assert_eq!(infer("x = [1, 2]\nx[1:]\n"), "list[int]");
        assert_eq!(infer("d = {'a': 1}\nd['a']\n"), "int");
        assert_eq!(infer("d = {'a': 1}\nd.get('b')\n"), "int | None");
        assert_eq!(infer("1 + 2\n"), "Literal[3]");
        assert_eq!(infer("a = 1\nb = 2.5\na + b\n"), "float");
        assert_eq!(infer("'a' * 3\n"), "str");
        assert_eq!(infer("1 < 2\n"), "bool");
        assert_eq!(infer("x = None\nx\n"), "None");
        assert_eq!(infer("'abc'.split(',')\n"), "list[str]");
        assert_eq!(infer("[x * 2 for x in [1, 2]]\n"), "list[int]");
        assert_eq!(infer("s = {1}\ns\n"), "set[int]");
        assert_eq!(infer("f = 1.5\nf.is_integer()\n"), "bool");
        assert!(infer("print\n").starts_with("(*values"));
    }

    #[test]
    fn classes_and_methods() {
        let src = "class Animal:\n    sound: str = 'generic'\n\n    def __init__(self, name: str):\n        self.name = name\n        self.age = 0\n\n    def speak(self) -> str:\n        return self.sound\n\n    @property\n    def label(self):\n        return self.name + '!'\n\n    @classmethod\n    def make(cls):\n        return cls('x')\n\n\nclass Dog(Animal):\n    def fetch(self):\n        return [self.name]\n\n\nd = Dog('rex')\n";
        assert_eq!(infer(&format!("{src}d\n")), "Dog");
        assert_eq!(infer(&format!("{src}d.name\n")), "str");
        assert_eq!(infer(&format!("{src}d.age\n")), "int");
        assert_eq!(infer(&format!("{src}d.speak()\n")), "str");
        assert_eq!(infer(&format!("{src}d.label\n")), "str");
        assert_eq!(infer(&format!("{src}d.fetch()\n")), "list[str]");
        assert_eq!(infer(&format!("{src}Dog.make()\n")), "Dog");
        assert_eq!(infer(&format!("{src}Dog.sound\n")), "str");
        assert_eq!(infer(&format!("{src}Dog\n")), "type[Dog]");
        assert_eq!(infer(&format!("{src}type(d)\n")), "type[Dog]");
        assert_eq!(infer(&format!("{src}d.speak\n")), "() -> str");
    }

    #[test]
    fn functions_generics_and_annotations() {
        assert_eq!(
            infer("def f(a: int, b: str = 'x') -> list[str]: ...\nf(1)\n"),
            "list[str]"
        );
        assert_eq!(infer("def f(a, b=2):\n    return b\nf(1)\n"), "int");
        assert_eq!(
            infer("def f(x):\n    if x:\n        return 1\n    return 'a'\nf(0)\n"),
            "int | str"
        );
        assert_eq!(infer("def f():\n    pass\nf()\n"), "None");
        assert_eq!(
            infer("def g():\n    yield 1\ng()\n"),
            "Generator[int, None, None]"
        );
        assert_eq!(infer("from typing import Optional, List, Dict\ndef f(a: Optional[int], b: List[str], c: Dict[str, int]) -> Optional[str]: ...\nf(None, [], {})\n"), "str | None");
        assert_eq!(
            infer("def f(a: 'int | None'): return a\nf(1)\n"),
            "int | None"
        );
        assert_eq!(infer("from typing import TypeVar, Generic\nT = TypeVar('T')\nclass Box(Generic[T]):\n    def __init__(self, item: T):\n        self.item = item\n    def get(self) -> T:\n        return self.item\nb = Box(1)\nb.get()\n"), "int");
        assert_eq!(infer("from typing import TypeVar, Generic\nT = TypeVar('T')\nclass Box(Generic[T]):\n    def __init__(self, item: T):\n        self.item = item\nBox('s').item\n"), "str");
        assert_eq!(infer("class Stack[T]:\n    def __init__(self) -> None:\n        self.items: list[T] = []\n    def pop(self) -> T: ...\nStack[int]().pop()\n"), "int");
        assert_eq!(
            infer("def first(xs: list[int]):\n    for x in xs:\n        return x\nfirst([1])\n"),
            "int | None"
        );
        assert!(infer(
            "async def f() -> int: ...\nasync def g():\n    v = await f()\n    return v\ng()\n"
        )
        .contains("Coroutine"));
        assert_eq!(infer("async def f() -> int: ...\nasync def g():\n    return await f()\nimport asyncio\nasyncio.run(g())\n"), "int");
        assert_eq!(infer("lam = lambda x: 1\nlam(0)\n"), "int");
        assert_eq!(
            infer("def f(*args: int, **kw: str): return args\nf()\n"),
            "tuple[int]"
        );
        assert_eq!(infer("a, b = 1, 'x'\nb\n"), "str");
        assert_eq!(infer("for i, s in enumerate(['a']):\n    pass\ns\n"), "str");
        assert_eq!(
            infer("with open('f') as fh:\n    pass\nfh\n"),
            "TextIOWrapper[_WrappedBuffer]"
        );
        assert_eq!(
            infer("with open('f', 'rb') as fh:\n    pass\nfh\n"),
            "BufferedReader[_BufferedReaderStream]"
        );
        assert_eq!(
            infer("try:\n    pass\nexcept ValueError as e:\n    pass\ne\n"),
            "ValueError"
        );
    }

    #[test]
    fn modules_and_imports() {
        assert_eq!(infer("import os\nos.path.join('a', 'b')\n"), "str");
        assert_eq!(infer("import os.path\nos.getcwd()\n"), "str");
        assert_eq!(infer("from os import getcwd\ngetcwd()\n"), "str");
        assert_eq!(infer("from os import path as p\np.exists('x')\n"), "bool");
        assert_eq!(infer("import json\njson.loads('1')\n"), "Any");
        assert_eq!(
            infer("import re\nm = re.match('a', 'b')\nm\n"),
            "Match[str] | None"
        );
        assert_eq!(infer("import re\nre.match('a', 'b').group(0)\n"), "str");
        assert_eq!(
            infer("from collections import defaultdict\nd = defaultdict(list)\nd\n"),
            "defaultdict[Unknown, list]"
        );
        assert_eq!(infer("import sys\nsys.argv\n"), "list[str]");
        assert_eq!(
            infer("from pathlib import Path\nPath('x').read_text()\n"),
            "str"
        );
        assert_eq!(infer("from dataclasses import dataclass\n@dataclass\nclass P:\n    x: int\n    y: float = 0.0\nP(1).y\n"), "float");
    }
}

#[cfg(test)]
#[path = "infer_binary_memo_tests.rs"]
mod binary_memo_tests;
