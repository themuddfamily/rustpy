//! Flow-sensitive name types and type narrowing (roadmap §2.2 "Narrowing"):
//! `isinstance`/`issubclass` (classes, tuples, `X | Y`), `is None` /
//! `is not None` / `== None`, truthiness, `callable()`, `type(x) is C`,
//! `==`/`!=`/`in` with literals, `not`, `and`/`or`, walrus subjects; applied
//! inside `if`/`elif`/`while` bodies, after early exits and `assert`
//! (through [`crate::flow`]), and inside `and`/`or` operands, conditional
//! expressions and comprehension conditions.
//!
//! Member references (`self.x`, `a.b.c`, `d["k"]`, `t[0]`) narrow like
//! names, keyed structurally by [`flow::reference_key`]: every predicate
//! above applies to them. As in pyright, a write to the reference or to any
//! prefix of it (`self.x = …`, `del self.x`, `self = …`) retires the
//! narrowing, while intervening calls keep it; a plain assignment narrows the
//! reference to the assigned value.

#[path = "narrow_match.rs"]
mod match_patterns;

use std::sync::Arc;

use rustpy_core::{TextRange, TextSize};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::{SyntaxKind, SyntaxNode};
use smol_str::SmolStr;

use crate::db::FileInfo;
use crate::flow::{self, Pred, Reach};
use crate::infer::Inferer;
use crate::scope::{BindingId, ScopeId};
use crate::types::{ClassRef, LiteralValue, Type};
#[path = "narrow_predicates.rs"]
mod predicates;

impl Inferer<'_> {
    pub(crate) fn scope_flow(&self, file: &FileInfo, scope: ScopeId) -> Arc<flow::FlowInfo> {
        let Some(current) = self.db.file_recently_checked(file.id) else {
            return flow::flow_of(file, scope);
        };
        if !std::ptr::eq(current.as_ref(), file) {
            return flow::flow_of(file, scope);
        }
        // Class scopes share their execution scope's walk and cache entry.
        let scope = flow::walked_scope(file, scope);
        let epoch = self.db.revision_excluding_file(file);
        let key = (file.id, scope);
        if let Some((captured, cached_epoch, flow)) = self.scope_flow_cache.borrow().get(&key) {
            if *cached_epoch == epoch
                && Arc::ptr_eq(captured, &current)
                && self.db.revision_excluding_file(file) == epoch
                && self
                    .db
                    .file_recently_checked(file.id)
                    .is_some_and(|latest| Arc::ptr_eq(&latest, &current))
            {
                return flow.clone();
            }
        }
        // No borrow is held while flow obtains module authority. FileInfo
        // values stay immutable, and the retained Arc prevents pointer reuse.
        // Clone the small cache before querying the database. No RefCell
        // borrow survives authority validation, module capture or flow work.
        let entries = self.terminal_authority_cache.borrow().clone();
        let authority = entries
            .into_iter()
            .find(|entry| entry.matches(self.db, &current, epoch))
            .or_else(|| flow::capture_authority(self.db, &current));
        let flow = authority.as_ref().map_or_else(
            || flow::flow_of_with_db(self.db, file, scope),
            |authority| flow::flow_with_authority(self.db, file, scope, authority),
        );
        if let Some(authority) = authority.filter(|entry| {
            entry.bytes() <= flow::MAX_AUTHORITY_CACHE_BYTES
                && entry.matches(self.db, &current, epoch)
        }) {
            let mut cache = self.terminal_authority_cache.borrow_mut();
            cache.retain(|entry| entry.file_id() != file.id);
            while !cache.is_empty()
                && (cache.len() >= flow::MAX_AUTHORITY_CACHE_FILES
                    || cache
                        .iter()
                        .map(|entry| entry.bytes())
                        .sum::<usize>()
                        .saturating_add(authority.bytes())
                        > flow::MAX_AUTHORITY_CACHE_BYTES)
            {
                cache.remove(0);
            }
            cache.push(authority);
        }
        if self.db.revision_excluding_file(file) == epoch
            && self
                .db
                .file_recently_checked(file.id)
                .is_some_and(|latest| Arc::ptr_eq(&latest, &current))
        {
            self.scope_flow_cache
                .borrow_mut()
                .insert(key, (current, epoch, flow.clone()));
        }
        flow
    }

    /// True only for a call statement proved terminal by the current flow.
    /// Does not infer callee types or recursively inspect function bodies.
    pub fn call_terminates(&self, file: &FileInfo, call: &ast::Expr) -> bool {
        let call = call.clone().unparen();
        let Some(scope) = flow::flow_scope(
            file,
            file.scopes.scope_at(call.syntax().text_range().start()),
        ) else {
            return false;
        };
        self.scope_flow(file, scope)
            .terminal_calls
            .contains(&call.syntax().text_range())
    }

    /// [`flow::raised_exception_name_at_with_db`] through this Inferer's
    /// flow cache: one walk per scope instead of one per queried statement.
    pub fn raised_exception_name_at(&self, file: &FileInfo, point: TextSize) -> Option<SmolStr> {
        let scope = flow::flow_scope(file, file.scopes.scope_at(point))?;
        self.scope_flow(file, scope)
            .raise_names
            .get(&point)
            .cloned()
    }

    /// [`flow::context_contract_with_db`] through this Inferer's flow cache.
    pub fn context_contract(&self, file: &FileInfo, context: &ast::Expr) -> flow::ContextContract {
        let range = context.syntax().text_range();
        let Some(scope) = flow::flow_scope(file, file.scopes.scope_at(range.start())) else {
            return flow::ContextContract::default();
        };
        self.scope_flow(file, scope)
            .contexts
            .get(&range)
            .cloned()
            .unwrap_or_default()
    }

    /// A phase-specific operation proved terminal by captured semantic flow.
    /// This query reuses immutable-source/epoch cache authority and never infers
    /// a type or executes the receiver's body. Missing/unknown points are false.
    pub fn operation_terminates(
        &self,
        file: &FileInfo,
        expression: &ast::Expr,
        kind: flow::OperationKind,
    ) -> bool {
        let expression = expression.clone().unparen();
        let range = expression.syntax().text_range();
        let Some(scope) = flow::flow_scope(file, file.scopes.scope_at(range.start())) else {
            return false;
        };
        self.scope_flow(file, scope)
            .terminal_operations
            .contains(&flow::OperationPoint { range, kind })
    }

    /// Current database-aware reaching definitions, using this Inferer's
    /// bounded immutable-source/epoch flow cache. None means possibly unbound.
    pub fn reaching_bindings(
        &self,
        file: &FileInfo,
        name: &ast::NameExpr,
    ) -> Option<Vec<BindingId>> {
        if file.is_stub {
            return None;
        }
        let token = name.name_token()?;
        let scope = flow::flow_scope(file, file.scopes.scope_at(token.text_range().start()))?;
        let info = self.scope_flow(file, scope);
        let reaches = info.refs.get(&token.text_range().start())?;
        if reaches.iter().any(|reach| reach.binding.is_none()) {
            return None;
        }
        let mut ids: Vec<_> = reaches.iter().filter_map(|reach| reach.binding).collect();
        ids.sort_unstable();
        ids.dedup();
        Some(ids)
    }

    /// Type of a name expression: the bindings that reach it in its scope
    /// (narrowed by the conditions on the way), then narrowed by the
    /// expression around it.
    pub(crate) fn name_type(&self, file: &Arc<FileInfo>, n: &ast::NameExpr) -> Type {
        let name = n.name_text();
        let scope = self.scope_of(file, n.syntax());
        let flow = if file.is_stub {
            None
        } else {
            flow::flow_scope(file, scope).map(|s| self.scope_flow(file, s))
        };
        let start = n.name_token().map(|t| t.text_range().start());
        if let (Some(flow), Some(start)) = (&flow, start) {
            // A binding target: the value it binds.
            if let Some(id) = flow.targets.get(&start) {
                return self.type_of_binding(file, *id);
            }
        }
        let reached = match (&flow, start) {
            (Some(flow), Some(start)) => flow
                .refs
                .get(&start)
                .and_then(|rs| self.reach_type(file, scope, &name, rs)),
            _ => None,
        };
        let unreachable = reached.is_none()
            && matches!((&flow, start), (Some(flow), Some(start)) if !flow.refs.contains_key(&start));
        let ty = reached.unwrap_or_else(|| match self.resolve_name(file, scope, &name) {
            Some(def) => self.type_of_binding(&def.file, def.binding),
            None if name == "__class__" => self
                .implicit_class_cell(file, scope)
                .unwrap_or(Type::Unknown),
            None => Type::Unknown,
        });
        // A case body made unreachable because earlier cases exhausted the
        // subject's finite domain sees the subject as `Never`.
        if unreachable && self.finite_domain(&ty) {
            let exhausted_subject = n
                .syntax()
                .ancestors()
                .take(64)
                .filter_map(ast::CaseClause::cast)
                .filter_map(|case| case.syntax().ancestors().find_map(ast::MatchStmt::cast))
                .any(|statement| {
                    statement
                        .subject()
                        .and_then(|subject| flow::reference_key(&subject))
                        .as_deref()
                        == Some(name.as_str())
                });
            if exhausted_subject {
                return Type::Never;
            }
        }
        self.narrow_by_context(file, n.syntax(), &name, ty)
    }

    /// Computes the types of the variables `expr` reads before a query
    /// evaluates `expr` inside a loop. A loop-carried variable whose value
    /// contains `expr` (`out = out + x`) would otherwise meet `expr`
    /// unfinished: its type would come out incomplete and provisional, and
    /// every later query would compute the whole cycle again.
    pub(crate) fn settle_loop_variables(&self, file: &Arc<FileInfo>, expr: &ast::Expr) {
        if file.is_stub {
            return;
        }
        let in_loop = expr
            .syntax()
            .ancestors()
            .take_while(|a| {
                !matches!(
                    a.kind(),
                    SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF | SyntaxKind::LAMBDA_EXPR
                )
            })
            .any(|a| {
                matches!(
                    a.kind(),
                    SyntaxKind::FOR_STMT
                        | SyntaxKind::WHILE_STMT
                        | SyntaxKind::LIST_COMP
                        | SyntaxKind::SET_COMP
                        | SyntaxKind::DICT_COMP
                        | SyntaxKind::GENERATOR_EXPR
                )
            });
        if !in_loop {
            return;
        }
        for n in expr.syntax().descendants().filter_map(ast::NameExpr::cast) {
            let Some(tok) = n.name_token() else { continue };
            let Some(scope) = flow::flow_scope(file, self.scope_of(file, n.syntax())) else {
                continue;
            };
            let flow = self.scope_flow(file, scope);
            for r in flow
                .refs
                .get(&tok.text_range().start())
                .into_iter()
                .flatten()
            {
                if let Some(id) = r.binding {
                    self.type_of_binding(file, id);
                }
            }
        }
    }

    /// The value read by an augmented assignment (`x += …`), flow-sensitively.
    pub(crate) fn aug_assign_read_type(
        &self,
        file: &Arc<FileInfo>,
        b: &crate::scope::Binding,
    ) -> Option<Type> {
        if file.is_stub {
            return None;
        }
        let scope = flow::flow_scope(file, b.scope)?;
        let flow = self.scope_flow(file, scope);
        let rs = flow.refs.get(&b.range.start())?;
        self.reach_type(file, b.scope, &b.name, rs)
    }

    /// Union of the narrowed types of the values that can reach a point.
    /// `None` when nothing reaches it (unbound): callers fall back to name
    /// resolution.
    pub(crate) fn reach_type(
        &self,
        file: &Arc<FileInfo>,
        scope: ScopeId,
        name: &str,
        reaches: &[Reach],
    ) -> Option<Type> {
        if reaches.is_empty() {
            return None;
        }
        let mut bases = Vec::new();
        let mut types = Vec::new();
        let mut cyclic = false;
        for r in reaches {
            let base = match r.binding {
                Some(id) => {
                    // A loop-carried value still being computed adds nothing new.
                    if let Some(depth) = self.binding_in_progress(file, id) {
                        self.note_cycle(depth);
                        cyclic = true;
                        continue;
                    }
                    self.type_of_binding(file, id)
                }
                None => match self.resolve_name(file, scope, name) {
                    Some(def) => self.type_of_binding(&def.file, def.binding),
                    None => Type::Unknown,
                },
            };
            let narrowed = self.apply_preds(file, base.clone(), &r.preds, name);
            bases.push(base);
            // Paths whose conditions contradict each other are impossible.
            if !matches!(narrowed, Type::Never) {
                types.push(narrowed);
            }
        }
        if bases.is_empty() {
            return cyclic.then_some(Type::Unknown);
        }
        // A finite domain (enum members, literals, bool) whose every value was
        // excluded is exhausted: `Never`, as `assert_never` expects.
        if types.is_empty() && bases.iter().all(|base| self.finite_domain(base)) {
            return Some(Type::Never);
        }
        // Unreachable code: show the declared types rather than nothing.
        Some(Type::union(if types.is_empty() { bases } else { types }))
    }

    fn finite_domain(&self, ty: &Type) -> bool {
        members(ty).iter().all(|member| match member {
            Type::Literal(_) | Type::None => true,
            Type::Instance(class, _) => {
                self.builtin_class("bool").as_ref() == Some(class)
                    || self.enum_member_values(class).is_some()
            }
            _ => false,
        })
    }

    /// Type of a load of a narrowable member reference: the member type
    /// `declared` narrowed by the conditions reaching the load (flow) and by
    /// the expression around it. Other expressions keep `declared`.
    pub(crate) fn reference_type(
        &self,
        file: &Arc<FileInfo>,
        expression: &ast::Expr,
        declared: Type,
    ) -> Type {
        if file.is_stub {
            return declared;
        }
        let Some(reference) =
            flow::reference_key(expression).filter(|key| key.contains(['.', '[']))
        else {
            return declared;
        };
        let node = expression.syntax();
        let scope = self.scope_of(file, node);
        let mut ty = declared.clone();
        if let Some(flow) = flow::flow_scope(file, scope).map(|s| self.scope_flow(file, s)) {
            if let Some(reaches) = flow.member_refs.get(&node.text_range()) {
                let types: Vec<Type> = reaches
                    .iter()
                    .map(|reach| {
                        self.apply_reference_preds(file, declared.clone(), &reach.preds, &reference)
                    })
                    .filter(|narrowed| !matches!(narrowed, Type::Never))
                    .collect();
                // Contradicting paths only: show the declared type.
                if !types.is_empty() {
                    ty = Type::union(types);
                }
            }
        }
        self.narrow_by_context(file, node, &reference, ty)
    }

    fn apply_reference_preds(
        &self,
        file: &Arc<FileInfo>,
        ty: Type,
        preds: &[Pred],
        reference: &str,
    ) -> Type {
        let mut ty = ty;
        for p in preds {
            match assigned_value(file, p.cond) {
                Some(value) => ty = self.narrow_on_assignment(file, &ty, &value),
                None => ty = self.apply_preds(file, ty, std::slice::from_ref(p), reference),
            }
        }
        ty
    }

    /// Assignment narrowing of a member reference: storing a value that
    /// cannot be `None` removes `None` from the declared member type. Other
    /// declared members are kept (the declaration stays authoritative).
    fn narrow_on_assignment(
        &self,
        file: &Arc<FileInfo>,
        declared: &Type,
        value: &ast::Expr,
    ) -> Type {
        let assigned = self.type_of_expr(file, value);
        if Self::may_include_none(&assigned, 0) {
            return declared.clone();
        }
        match self.narrow_none(declared, false) {
            Type::Never => declared.clone(),
            narrowed => narrowed,
        }
    }

    fn apply_preds(&self, file: &Arc<FileInfo>, ty: Type, preds: &[Pred], name: &str) -> Type {
        let mut ty = ty;
        for p in preds {
            // A value pattern and its nested StringExpr can have identical
            // ranges. Resolve the pattern first: it is an acquired-subject
            // predicate, not a condition testing the string literal itself.
            if let Some(pattern) = crate::match_support::pattern_at(file, p.cond) {
                ty = self.narrow_match_predicate(file, &ty, &pattern, p.positive, name);
            } else if let Some(cond) = expr_at(file, p.cond) {
                ty = self.narrow_cond(file, &ty, &cond, p.positive, name, 0);
            }
        }
        ty
    }

    /// Narrowing from the expression around a name: earlier `and`/`or`
    /// operands, the test of a conditional expression, comprehension
    /// conditions.
    fn narrow_by_context(
        &self,
        file: &Arc<FileInfo>,
        node: &SyntaxNode,
        name: &str,
        declared: Type,
    ) -> Type {
        let mut ty = declared.clone();
        let mut child = node.clone();
        for anc in node.ancestors().skip(1) {
            match anc.kind() {
                SyntaxKind::BOOL_OP_EXPR => {
                    if let Some(b) = ast::BoolOpExpr::cast(anc.clone()) {
                        let is_and = !b.is_or();
                        for op in b.operands() {
                            if op.syntax() == &child {
                                break;
                            }
                            // `a and b`: `a` was true; `a or b`: `a` was false.
                            ty = self.narrow_cond(file, &ty, &op, is_and, name, 0);
                        }
                    }
                }
                SyntaxKind::CONDITIONAL_EXPR => {
                    if let Some(c) = ast::ConditionalExpr::cast(anc.clone()) {
                        if let Some(cond) = c.condition() {
                            if c.true_expr().is_some_and(|e| e.syntax() == &child) {
                                ty = self.narrow_cond(file, &ty, &cond, true, name, 0);
                            } else if c.false_expr().is_some_and(|e| e.syntax() == &child) {
                                ty = self.narrow_cond(file, &ty, &cond, false, name, 0);
                            }
                        }
                    }
                }
                SyntaxKind::LIST_COMP
                | SyntaxKind::SET_COMP
                | SyntaxKind::GENERATOR_EXPR
                | SyntaxKind::DICT_COMP => {
                    // The element runs after every condition; a clause after the earlier ones.
                    let is_clause =
                        matches!(child.kind(), SyntaxKind::COMP_FOR | SyntaxKind::COMP_IF);
                    for c in anc.children().filter(|c| c.kind() == SyntaxKind::COMP_IF) {
                        if is_clause && c.text_range().start() >= child.text_range().start() {
                            break;
                        }
                        if let Some(cond) = c.children().find_map(ast::Expr::cast) {
                            ty = self.narrow_cond(file, &ty, &cond, true, name, 0);
                        }
                    }
                }
                SyntaxKind::LAMBDA_EXPR | SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF => break,
                k if ast::Stmt::can_cast(k) => break,
                _ => {}
            }
            child = anc;
        }
        if matches!(ty, Type::Never) {
            declared
        } else {
            ty
        }
    }

    /// Narrows the type `ty` of `name` knowing that `cond` is `positive`.
    fn narrow_cond(
        &self,
        file: &Arc<FileInfo>,
        ty: &Type,
        cond: &ast::Expr,
        positive: bool,
        name: &str,
        depth: usize,
    ) -> Type {
        if depth > 12 {
            return ty.clone();
        }
        match cond.clone().unparen() {
            ast::Expr::Not(n) => match n.operand() {
                Some(o) => self.narrow_cond(file, ty, &o, !positive, name, depth + 1),
                None => ty.clone(),
            },
            ast::Expr::BoolOp(b) => {
                let ops: Vec<ast::Expr> = b.operands().collect();
                let is_and = !b.is_or();
                if is_and == positive {
                    // Every operand of `and` held / every operand of `or` failed.
                    ops.iter().fold(ty.clone(), |t, o| {
                        self.narrow_cond(file, &t, o, positive, name, depth + 1)
                    })
                } else {
                    // Some operand decided the result: the earlier ones went the other way.
                    let mut parts = Vec::new();
                    let mut prefix = ty.clone();
                    for o in &ops {
                        parts.push(self.narrow_cond(file, &prefix, o, positive, name, depth + 1));
                        prefix = self.narrow_cond(file, &prefix, o, !positive, name, depth + 1);
                    }
                    union_live(parts)
                }
            }
            ast::Expr::Named(named) => {
                if named
                    .target()
                    .is_some_and(|target| target.name_text() == name)
                {
                    // The condition tests the value just assigned, rather than
                    // the incoming type from an earlier boolean operand.
                    named.value().map_or_else(
                        || ty.clone(),
                        |value| {
                            let assigned = self.widen(self.type_of_expr(file, &value));
                            self.narrow_truthy(&assigned, positive)
                        },
                    )
                } else {
                    ty.clone()
                }
            }
            e @ (ast::Expr::Name(_) | ast::Expr::Attribute(_) | ast::Expr::Subscript(_)) => {
                if is_subject(&e, name) {
                    self.narrow_truthy(ty, positive)
                } else {
                    ty.clone()
                }
            }
            ast::Expr::Call(c) => self.narrow_call(file, ty, &c, positive, name),
            ast::Expr::Compare(c) => self.narrow_compare(file, ty, &c, positive, name),
            _ => ty.clone(),
        }
    }

    fn narrow_call(
        &self,
        file: &Arc<FileInfo>,
        ty: &Type,
        c: &ast::CallExpr,
        positive: bool,
        name: &str,
    ) -> Type {
        if !Self::predicate_subject_stable(c.syntax(), name) {
            return ty.clone();
        }
        if let Some(narrowed) = self.narrow_declared_predicate(file, ty, c, positive, name) {
            return narrowed;
        }
        let Some(callee) = c
            .callee()
            .and_then(|callee| self.narrowing_builtin(file, &callee))
        else {
            return ty.clone();
        };
        let Some(args) = Self::narrowing_positional_arguments(c) else {
            return ty.clone();
        };
        match callee.as_str() {
            "isinstance" | "issubclass" if args.len() == 2 && is_subject(&args[0], name) => {
                let mut targets = Vec::new();
                if !self.class_targets(file, &args[1], &mut targets, 0) {
                    return ty.clone();
                }
                self.narrow_to_classes(ty, &targets, positive, callee == "issubclass")
            }
            "callable" if args.len() == 1 && is_subject(&args[0], name) => {
                self.narrow_callable(ty, positive)
            }
            "hasattr" if args.len() == 2 && is_subject(&args[0], name) => {
                if let Type::Literal(LiteralValue::Str(member)) = self.type_of_expr(file, &args[1])
                {
                    self.narrow_known_attribute(ty, &member, positive)
                } else {
                    ty.clone()
                }
            }
            _ => ty.clone(),
        }
    }

    fn narrow_compare(
        &self,
        file: &Arc<FileInfo>,
        ty: &Type,
        c: &ast::CompareExpr,
        positive: bool,
        name: &str,
    ) -> Type {
        if !Self::predicate_subject_stable(c.syntax(), name) {
            return ty.clone();
        }
        if let Some(narrowed) = self.narrow_projected_comparison(file, ty, c, positive, name) {
            return narrowed;
        }
        let ops: Vec<String> = c.ops().map(|o| o.text()).collect();
        let operands: Vec<ast::Expr> = c.operands().map(|o| o.unparen()).collect();
        if ops.len() != 1 || operands.len() != 2 {
            return ty.clone();
        }
        let (l, r) = (&operands[0], &operands[1]);
        let is_none = |e: &ast::Expr| matches!(e, ast::Expr::Literal(x) if x.kind() == Some(SyntaxKind::NONE_KW));
        match ops[0].as_str() {
            op @ ("is" | "==" | "is not" | "!=") => {
                let equal = matches!(op, "is" | "==") == positive;
                let (subject_l, subject_r) = (is_subject(l, name), is_subject(r, name));
                if (subject_l && is_none(r)) || (subject_r && is_none(l)) {
                    return self.narrow_none(ty, equal);
                }
                // `type(x) is C`
                if equal {
                    for (call, other) in [(l, r), (r, l)] {
                        if let ast::Expr::Call(call) = call {
                            let is_type_call = call.callee().is_some_and(|callee| {
                                self.narrowing_builtin(file, &callee).as_deref() == Some("type")
                            });
                            let args: Vec<ast::Expr> = call
                                .arg_list()
                                .map(|a| a.positional().collect())
                                .unwrap_or_default();
                            if is_type_call && args.len() == 1 && is_subject(&args[0], name) {
                                if let Type::Class(c, a) = self.type_of_expr(file, other) {
                                    return Type::Instance(c, a);
                                }
                            }
                        }
                    }
                }
                // `x == "literal"`
                let other = if subject_l {
                    Some(r)
                } else if subject_r {
                    Some(l)
                } else {
                    None
                };
                match other.map(|o| self.type_of_expr(file, o)) {
                    Some(Type::Literal(lit)) => self.narrow_literal(ty, &lit, equal),
                    _ => ty.clone(),
                }
            }
            op @ ("in" | "not in") => {
                let inside = (op == "in") == positive;
                if !is_subject(l, name) {
                    return ty.clone();
                }
                let elems: Vec<ast::Expr> = match r {
                    ast::Expr::Tuple(t) => {
                        if inside {
                            t.elements().collect()
                        } else {
                            t.elements().take(257).collect()
                        }
                    }
                    ast::Expr::List(t) => {
                        if inside {
                            t.elements().collect()
                        } else {
                            t.elements().take(257).collect()
                        }
                    }
                    ast::Expr::Set(t) => {
                        if inside {
                            t.elements().collect()
                        } else {
                            t.elements().take(257).collect()
                        }
                    }
                    _ => return ty.clone(),
                };
                // A negative proof must remain bounded and cannot mine dynamic
                // containers. Positive membership retains its existing policy.
                if !inside && elems.len() > 256 {
                    return ty.clone();
                }
                let mut lits = Vec::new();
                for e in &elems {
                    match self.type_of_expr(file, e) {
                        Type::Literal(l) => lits.push(Type::Literal(l)),
                        Type::None => lits.push(Type::None),
                        _ => return ty.clone(),
                    }
                }
                let members = members(ty);
                let out: Vec<Type> = members
                    .into_iter()
                    .filter(|m| match m {
                        Type::None if !inside => !lits.contains(&Type::None),
                        Type::Literal(value) if !inside => {
                            !lits.iter().any(|candidate| match (value, candidate) {
                                (LiteralValue::Bool(a), Type::Literal(LiteralValue::Int(b))) => {
                                    i64::from(*a) == *b
                                }
                                (LiteralValue::Int(a), Type::Literal(LiteralValue::Bool(b))) => {
                                    *a == i64::from(*b)
                                }
                                _ => candidate == m,
                            })
                        }
                        Type::None => lits.contains(&Type::None),
                        Type::Literal(_) => lits.contains(m),
                        _ => true,
                    })
                    .collect();
                union_live(out)
            }
            _ => ty.clone(),
        }
    }

    fn may_include_none(ty: &Type, depth: u8) -> bool {
        // Declared metadata is finite, but nested bounds/constraints remain budgeted.
        // Exhaustion means unknown, never proof that None is impossible.
        if depth >= 16 {
            return true;
        }
        match ty {
            Type::DeclaredTypeVar(definition) => {
                if let Some(bound) = &definition.bound {
                    Self::may_include_none(bound, depth + 1)
                } else {
                    definition.constraints.is_empty()
                        || definition
                            .constraints
                            .iter()
                            .any(|constraint| Self::may_include_none(constraint, depth + 1))
                }
            }
            Type::Union(types) => types.iter().any(|ty| Self::may_include_none(ty, depth + 1)),
            Type::None | Type::Unknown | Type::Any | Type::TypeVar(_) => true,
            Type::Instance(class, _) => class.name == "object",
            _ => false,
        }
    }

    fn narrow_none(&self, ty: &Type, is_none: bool) -> Type {
        let members = members(ty);
        if is_none {
            let maybe_none = members.iter().any(|m| Self::may_include_none(m, 0));
            if maybe_none {
                Type::None
            } else {
                Type::Never
            }
        } else {
            union_live(members.into_iter().filter(|m| !m.is_none()).collect())
        }
    }

    fn narrow_truthy(&self, ty: &Type, positive: bool) -> Type {
        let members = members(ty);
        let out: Vec<Type> = if positive {
            members
                .into_iter()
                .filter(|m| !m.is_none() && truthiness(m) != Some(false))
                .collect()
        } else {
            members
                .into_iter()
                .filter(|m| truthiness(m) != Some(true))
                .collect()
        };
        union_live(out)
    }

    fn narrow_literal(&self, ty: &Type, lit: &LiteralValue, equal: bool) -> Type {
        let mut domain = Vec::new();
        for member in members(ty) {
            if let Type::Instance(class, _) = &member {
                if let Some(values) = self.enum_member_values(class) {
                    domain.extend(values);
                    continue;
                }
            }
            domain.push(member);
        }
        union_live(
            domain
                .into_iter()
                .filter(|member| match member {
                    Type::Literal(value) => {
                        let matches = if matches!(value, LiteralValue::EnumMember { .. })
                            || matches!(lit, LiteralValue::EnumMember { .. })
                        {
                            self.enum_pattern_equal(value, lit, false)
                        } else {
                            Some(crate::match_support::literal_pattern_equal(
                                value, lit, false,
                            ))
                        };
                        matches.is_none_or(|matches| matches == equal)
                    }
                    Type::None => !equal,
                    _ => true,
                })
                .collect(),
        )
    }

    fn narrow_callable(&self, ty: &Type, positive: bool) -> Type {
        let members = members(ty);
        let out: Vec<Type> = members
            .into_iter()
            .filter(|m| match self.is_callable_member(m) {
                Some(c) => c == positive,
                None => true,
            })
            .collect();
        union_live(out)
    }

    /// Whether a value of type `m` is callable (`None`: cannot tell).
    fn is_callable_member(&self, m: &Type) -> Option<bool> {
        match m {
            Type::Function(_)
            | Type::BoundMethod(..)
            | Type::Callable(_)
            | Type::Overloaded(_)
            | Type::Class(..) => Some(true),
            Type::Literal(LiteralValue::EnumMember { class, .. }) => Some(
                self.lookup_member(&Type::Instance(class.clone(), Vec::new()), "__call__")
                    .is_some(),
            ),
            Type::None | Type::Literal(_) | Type::Tuple(_) | Type::Module(_) => Some(false),
            Type::Instance(..) => Some(self.lookup_member(m, "__call__").is_some()),
            _ => None,
        }
    }

    /// Collects the classes tested by the second argument of `isinstance`
    /// (as instance types; `None` for `NoneType`). False if any is unknown.
    fn class_targets(
        &self,
        file: &Arc<FileInfo>,
        e: &ast::Expr,
        out: &mut Vec<Type>,
        depth: usize,
    ) -> bool {
        if depth > 8 {
            return false;
        }
        match e.clone().unparen() {
            ast::Expr::Tuple(t) => t.elements().all(|x| self.class_targets(file, &x, out, depth + 1)),
            ast::Expr::Binary(b) if b.op().map(|t| t.kind()) == Some(SyntaxKind::PIPE) => [b.lhs(), b.rhs()].into_iter().all(|x| x.is_some_and(|x| self.class_targets(file, &x, out, depth + 1))),
            ast::Expr::Literal(l) if l.kind() == Some(SyntaxKind::NONE_KW) => {
                out.push(Type::None);
                true
            }
            ast::Expr::Call(c) if c.callee().is_some_and(|callee| self.narrowing_builtin(file, &callee).as_deref() == Some("type")) && c.arg_list().and_then(|a| a.positional().next()).is_some_and(|a| matches!(a, ast::Expr::Literal(l) if l.kind() == Some(SyntaxKind::NONE_KW))) => {
                out.push(Type::None);
                true
            }
            other => match self.type_of_expr(file, &other) {
                Type::Class(c, args) => {
                    out.push(self.class_instance(c, args));
                    true
                }
                Type::Tuple(ts) => ts.into_iter().all(|t| match t {
                    Type::Class(c, args) => {
                        out.push(self.class_instance(c, args));
                        true
                    }
                    _ => false,
                }),
                _ => false,
            },
        }
    }

    fn class_instance(&self, c: ClassRef, args: Vec<Type>) -> Type {
        if c.name == "NoneType" {
            return Type::None;
        }
        let args = if args.is_empty() {
            self.class_info(&c)
                .map(|i| i.type_params.iter().map(|_| Type::Unknown).collect())
                .unwrap_or_default()
        } else {
            args
        };
        Type::Instance(c, args)
    }

    /// `isinstance(x, targets)` (or `issubclass` when `class_objects`).
    fn narrow_to_classes(
        &self,
        ty: &Type,
        targets: &[Type],
        positive: bool,
        class_objects: bool,
    ) -> Type {
        // Work on instance types; `issubclass` subjects are class objects.
        let to_instance = |m: &Type| -> Type {
            match m {
                Type::Class(c, a) if class_objects => Type::Instance(c.clone(), a.clone()),
                other => other.clone(),
            }
        };
        let back = |m: Type| -> Type {
            match m {
                Type::Instance(c, a) if class_objects => Type::Class(c, a),
                other => other,
            }
        };
        let members: Vec<Type> = members(ty).iter().map(to_instance).collect();
        let result: Vec<Type> = if positive {
            let mut out = Vec::new();
            for m in &members {
                match self.member_class(m) {
                    MemberClass::Unknown => {
                        // `isinstance(x, type)` says no more than "some class".
                        if !(targets.len() == 1
                            && matches!(&targets[0], Type::Instance(c, _) if c.name == "type"))
                            || matches!(m, Type::Never)
                        {
                            out.extend(targets.iter().cloned());
                        } else {
                            out.push(m.clone());
                        }
                    }
                    MemberClass::NoneType => {
                        if targets.iter().any(|t| t.is_none() || is_object(t)) {
                            out.push(Type::None);
                        }
                    }
                    MemberClass::Known(c) => {
                        if targets.iter().any(|t| self.is_instance_of(&c, t)) {
                            out.push(m.clone());
                        } else {
                            // A subclass of the declared type.
                            for t in targets {
                                if let Type::Instance(tc, _) = t {
                                    if self.is_subclass(tc, &c) && !out.contains(t) {
                                        out.push(t.clone());
                                    }
                                }
                            }
                        }
                    }
                    MemberClass::Other => {
                        if targets.iter().any(is_object) {
                            out.push(m.clone());
                        }
                    }
                }
            }
            if out.is_empty() {
                targets.to_vec()
            } else {
                out
            }
        } else {
            let out: Vec<Type> = members
                .iter()
                .filter(|m| match self.member_class(m) {
                    MemberClass::Known(c) => !targets.iter().any(|t| self.is_instance_of(&c, t)),
                    MemberClass::NoneType => !targets.iter().any(|t| t.is_none() || is_object(t)),
                    _ => true,
                })
                .cloned()
                .collect();
            if out.is_empty() {
                return Type::Never;
            }
            out
        };
        Type::union(result.into_iter().map(back).collect())
    }

    fn member_class(&self, m: &Type) -> MemberClass {
        match m {
            Type::LiteralString | Type::LiteralStringValue => self
                .builtin_class("str")
                .map(MemberClass::Known)
                .unwrap_or(MemberClass::Unknown),
            Type::Instance(c, _) | Type::SelfType(c) => MemberClass::Known(c.clone()),
            Type::Literal(LiteralValue::EnumMember { class, .. }) => {
                MemberClass::Known(class.clone())
            }
            Type::Literal(l) => literal_class(l)
                .and_then(|name| self.builtin_class(name))
                .map(MemberClass::Known)
                .unwrap_or(MemberClass::Unknown),
            Type::Tuple(_) => self
                .builtin_class("tuple")
                .map(MemberClass::Known)
                .unwrap_or(MemberClass::Unknown),
            Type::Class(..) => self
                .builtin_class("type")
                .map(MemberClass::Known)
                .unwrap_or(MemberClass::Unknown),
            Type::None => MemberClass::NoneType,
            // `Never`: earlier tests ruled out the inferred type (often only a
            // guess from a default value); a later `isinstance` still tells.
            Type::Unknown | Type::Any | Type::TypeVar(_) | Type::Never => MemberClass::Unknown,
            _ => MemberClass::Other,
        }
    }

    /// Whether instances of class `c` are instances of the target type.
    fn is_instance_of(&self, c: &ClassRef, target: &Type) -> bool {
        match target {
            Type::Instance(t, _) => {
                if self.is_subclass(c, t) {
                    return true;
                }
                // `@runtime_checkable` protocols match structurally.
                self.class_info(t).is_some_and(|i| i.is_protocol)
                    && self.is_compatible(&Type::Instance(c.clone(), Vec::new()), target)
            }
            _ => false,
        }
    }

    fn is_subclass(&self, c: &ClassRef, of: &ClassRef) -> bool {
        c == of
            || (of.name == "object" && self.builtin_class("object").as_ref() == Some(of))
            || self.mro(c).iter().any(|k| k == of)
    }
}

enum MemberClass {
    Known(ClassRef),
    NoneType,
    Unknown,
    /// Functions, modules and other values without a modelled class.
    Other,
}

/// Union of the possible types, `Never` when none is left.
fn union_live(ts: Vec<Type>) -> Type {
    let live: Vec<Type> = ts
        .into_iter()
        .filter(|t| !matches!(t, Type::Never))
        .collect();
    if live.is_empty() {
        Type::Never
    } else {
        Type::union(live)
    }
}

fn members(ty: &Type) -> Vec<Type> {
    match ty {
        Type::Union(ts) => ts.clone(),
        t => vec![t.clone()],
    }
}

fn is_object(t: &Type) -> bool {
    matches!(t, Type::Instance(c, _) if c.name == "object")
}

/// Known truthiness of a value of type `m`.
pub(crate) fn truthiness(m: &Type) -> Option<bool> {
    match m {
        Type::None => Some(false),
        Type::Literal(LiteralValue::Bool(b)) => Some(*b),
        Type::Literal(LiteralValue::Int(i)) => Some(*i != 0),
        Type::Literal(LiteralValue::Str(s)) | Type::Literal(LiteralValue::Bytes(s)) => {
            Some(!s.is_empty())
        }
        Type::Tuple(ts) => Some(!ts.is_empty()),
        Type::Class(..)
        | Type::Module(_)
        | Type::Function(_)
        | Type::BoundMethod(..)
        | Type::Overloaded(_) => Some(true),
        _ => None,
    }
}

fn literal_class(l: &LiteralValue) -> Option<&'static str> {
    match l {
        LiteralValue::Int(_) => Some("int"),
        LiteralValue::Str(_) => Some("str"),
        LiteralValue::Bytes(_) => Some("bytes"),
        LiteralValue::Bool(_) => Some("bool"),
        LiteralValue::EnumMember { .. } => None,
    }
}

/// Whether `e` is the narrowed name itself (or a walrus binding it), or the
/// narrowed member reference (`name` is then a [`flow::reference_key`]).
fn is_subject(e: &ast::Expr, name: &str) -> bool {
    match e.clone().unparen() {
        ast::Expr::Name(n) => n.name_text() == name,
        ast::Expr::Named(w) => w.target().is_some_and(|t| t.name_text() == name),
        e @ (ast::Expr::Attribute(_) | ast::Expr::Subscript(_)) => {
            name.contains(['.', '[']) && flow::reference_key(&e).as_deref() == Some(name)
        }
        _ => false,
    }
}

/// The value of the assignment statement whose value expression has exactly
/// `range` (a member reference's assignment-narrowing predicate).
fn assigned_value(file: &FileInfo, range: TextRange) -> Option<ast::Expr> {
    let value = expr_at(file, range)?;
    let parent = value
        .syntax()
        .ancestors()
        .find(|ancestor| ancestor.text_range() != range)?;
    let assigned = match parent.kind() {
        SyntaxKind::ASSIGN_STMT => ast::AssignStmt::cast(parent)?.value(),
        SyntaxKind::ANN_ASSIGN_STMT => ast::AnnAssignStmt::cast(parent)?.value(),
        _ => None,
    }?;
    (assigned.syntax().text_range() == range).then_some(value)
}

/// The expression node with exactly `range`.
fn expr_at(file: &FileInfo, range: TextRange) -> Option<ast::Expr> {
    let root = file.parse.syntax_node();
    let start = match root.covering_element(range) {
        rustpy_syntax::rowan::NodeOrToken::Node(n) => n,
        rustpy_syntax::rowan::NodeOrToken::Token(t) => t.parent()?,
    };
    start
        .ancestors()
        .find(|a| a.text_range() == range && ast::Expr::can_cast(a.kind()))
        .and_then(ast::Expr::cast)
}

/// Bindings that can reach the name expression `n` in its scope, if flow
/// analysis covers it (`None` otherwise, e.g. in lambdas or stubs).
pub fn reaching_bindings(file: &FileInfo, n: &ast::NameExpr) -> Option<Vec<BindingId>> {
    if file.is_stub {
        return None;
    }
    let tok = n.name_token()?;
    let scope = flow::flow_scope(file, file.scopes.scope_at(tok.text_range().start()))?;
    let info = flow::flow_of(file, scope);
    let rs = info.refs.get(&tok.text_range().start())?;
    if rs.iter().any(|r| r.binding.is_none()) {
        return None;
    }
    let mut out: Vec<BindingId> = rs.iter().filter_map(|r| r.binding).collect();
    out.dedup();
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use crate::db::Analysis;

    /// `out = out + 'i'` (or `out += 'i'`) in `k` branches of a loop: every
    /// rebinding reaches every other one.
    fn branchy_loop(k: usize, aug: bool) -> String {
        let mut src = String::from("def f(items, eol):\n    out = ''\n    for line in items:\n");
        for i in 0..k {
            src.push_str(&format!(
                "        {} line == '{i}':\n",
                if i == 0 { "if" } else { "elif" }
            ));
            src.push_str(&if aug {
                format!("            out += '{i}'\n")
            } else {
                format!("            out = out + '{i}'\n")
            });
        }
        src.push_str(if aug {
            "        else:\n            out += eol\n"
        } else {
            "        else:\n            out = out + eol\n"
        });
        src.push_str("    return out.upper()\n");
        src
    }

    /// Binding types computed by `f` on this thread.
    fn computations(f: impl FnOnce()) -> usize {
        const BUDGET: usize = 3_000;
        crate::infer::BINDING_BUDGET.with(|b| b.set(BUDGET));
        f();
        let used = BUDGET - crate::infer::BINDING_BUDGET.with(|b| b.get());
        crate::infer::BINDING_BUDGET.with(|b| b.set(usize::MAX));
        used
    }

    #[test]
    fn loop_carried_rebindings_are_not_recomputed_in_every_order() {
        // A type computed while a cycle was cut used to be cached nowhere, so
        // each rebinding was recomputed in every order: 13 s for 6 branches,
        // exponential in their number. Now a cycle through enclosing,
        // unfinished computations is cached until they end.
        for aug in [false, true] {
            let src = branchy_loop(10, aug);
            let db = Analysis::for_project(Path::new("/proj"));
            let id = db.set_file_text(Path::new("/proj/m.py"), &src);
            let file = db.file(id).unwrap();
            let used = computations(|| {
                let inf = Inferer::new(&db);
                for n in file
                    .parse
                    .syntax_node()
                    .descendants()
                    .filter_map(ast::NameExpr::cast)
                    .filter(|n| n.name_text() == "out")
                {
                    assert_eq!(
                        inf.type_of_expr(&file, &ast::Expr::Name(n)).to_string(),
                        "str",
                        "{src}"
                    );
                }
            });
            assert!(
                used < 400,
                "{used} binding types computed for one pass over the reads"
            );
            let used = computations(|| {
                crate::features::inlay_hints(&db, &file, true, true);
                crate::features::diagnostics(&db, &file);
            });
            assert!(
                used < 2_000,
                "{used} binding types computed by inlay hints and diagnostics"
            );
        }
    }

    #[test]
    fn loop_carried_types_do_not_depend_on_query_order() {
        let src = "def g(xs):\n    total = 0\n    for x in xs:\n        if x:\n            total = total + 1.5\n        else:\n            total += 2\n    return total\n";
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let file = db.file(id).unwrap();
        let ids: Vec<BindingId> = (0..file.scopes.bindings.len() as u32)
            .map(BindingId)
            .filter(|id| file.scopes.binding(*id).name == "total")
            .collect();
        assert_eq!(ids.len(), 3);
        let forward: Vec<String> = {
            let inf = Inferer::new(&db);
            ids.iter()
                .map(|id| inf.type_of_binding(&file, *id).to_string())
                .collect()
        };
        let backward: Vec<String> = {
            let inf = Inferer::new(&db);
            let mut v: Vec<String> = ids
                .iter()
                .rev()
                .map(|id| inf.type_of_binding(&file, *id).to_string())
                .collect();
            v.reverse();
            v
        };
        assert_eq!(forward, backward);
        assert_eq!(forward[1], "float");
        // Each read is computed afresh too.
        for i in 0..ids.len() {
            let inf = Inferer::new(&db);
            assert_eq!(inf.type_of_binding(&file, ids[i]).to_string(), forward[i]);
        }
        let ret = infer_at(src, "total", 4);
        assert!(ret.contains("int") && ret.contains("float"), "{ret}");
    }

    /// Type of the expression on the last line.
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

    /// Type of the `nth` occurrence of the expression `needle` in `src`.
    fn infer_at(src: &str, needle: &str, nth: usize) -> String {
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let file = db.file(id).unwrap();
        let inf = Inferer::new(&db);
        let off = src
            .match_indices(needle)
            .nth(nth)
            .unwrap_or_else(|| panic!("{needle} #{nth} not found"))
            .0;
        // Surrounding whitespace in the needle only disambiguates.
        let lead = needle.len() - needle.trim_start().len();
        let range = TextRange::at(
            rustpy_core::TextSize::new((off + lead) as u32),
            rustpy_core::TextSize::new(needle.trim().len() as u32),
        );
        let e = expr_at(&file, range).unwrap_or_else(|| panic!("no expression at {needle}"));
        inf.type_of_expr(&file, &e).to_string()
    }

    #[test]
    fn reassigned_names_use_the_binding_that_reaches_them() {
        let src = "line = 'a,b'\nparts = line.split(',')\nparts.append('c')\nparts = ','.join(parts)\nparts\n";
        assert_eq!(infer_at(src, "parts", 1), "list[str]");
        assert_eq!(infer_at(src, "parts", 3), "list[str]");
        assert_eq!(infer(src), "str");
        // Hover on a binding target shows what it binds.
        assert_eq!(infer_at(src, "parts", 2), "str");
        assert_eq!(infer_at(src, "parts", 0), "list[str]");
    }

    #[test]
    fn branches_join_into_unions() {
        assert_eq!(
            infer("import random\nx = None\nif random.random() > 0.5:\n    x = 1\nx\n"),
            "int | None"
        );
        assert_eq!(
            infer("import random\nif random.random():\n    v = 'a'\nelse:\n    v = 2\nv\n"),
            "str | int"
        );
        assert_eq!(
            infer("x = 0\nfor i in range(3):\n    x = 'a'\nx\n"),
            "int | str"
        );
        assert_eq!(
            infer("try:\n    v = int('1')\nexcept ValueError:\n    v = None\nv\n"),
            "int | None"
        );
    }

    #[test]
    fn none_checks_narrow_optionals() {
        let src = "class Foo:\n    def go(self) -> int: ...\ndef f(x: Foo | None):\n    if x is not None:\n        x\n    if x is None:\n        return\n    x\n";
        assert_eq!(infer_at(src, "        x\n", 0), "Foo");
        assert_eq!(infer_at(src, "    x\n", 1), "Foo");
        let src = "import re\ndef f(s: str):\n    m = re.match('a', s)\n    if not m:\n        raise ValueError(s)\n    m\n";
        assert_eq!(infer_at(src, "m\n", 0), "Match[str]");
        let src = "from typing import Optional\ndef f(x: Optional[int]):\n    assert x is not None\n    x\n";
        assert_eq!(infer_at(src, "x\n", 0), "int");
        let src = "def f(x: int | None):\n    while x is None:\n        x = 1\n    x\n";
        assert_eq!(infer_at(src, "x\n", 0), "int");
    }

    #[test]
    fn isinstance_and_other_guards() {
        let src = "def f(x: int | str | None, y):\n    if isinstance(x, str):\n        x\n    elif isinstance(x, (int, float)):\n        x\n    else:\n        x\n    if isinstance(y, list):\n        y\n";
        assert_eq!(infer_at(src, "x\n", 0), "str");
        assert_eq!(infer_at(src, "x\n", 1), "int");
        assert_eq!(infer_at(src, "x\n", 2), "None");
        assert_eq!(infer_at(src, "y\n", 0), "list");
        let src = "def f(x: int | str):\n    if not isinstance(x, int):\n        return\n    x\n";
        assert_eq!(infer_at(src, "x\n", 0), "int");
        let src = "def f(x: int | str):\n    if type(x) is str:\n        x\n";
        assert_eq!(infer_at(src, "x\n", 0), "str");
        let src = "from typing import Callable\ndef f(x: Callable[[], int] | None):\n    if callable(x):\n        x\n";
        assert_eq!(infer_at(src, "x\n", 0), "() -> int");
        let src = "from typing import Literal\ndef f(m: Literal['r', 'w'] | None):\n    if m == 'r':\n        m\n    if m in ('r', 'w'):\n        m\n";
        assert_eq!(infer_at(src, "m\n", 0), "Literal[\"r\"]");
        assert_eq!(infer_at(src, "m\n", 1), "Literal[\"r\"] | Literal[\"w\"]");
    }

    #[test]
    fn expression_context_narrows() {
        let src = "class Foo:\n    name: str\ndef f(x: Foo | None, xs: list[Foo | None]):\n    a = x is not None and x.name\n    b = x.name if x else ''\n    c = [y.name for y in xs if y is not None]\n";
        assert_eq!(infer_at(src, "x.name", 0), "str");
        assert_eq!(infer_at(src, "x", 3), "Foo");
        assert_eq!(infer_at(src, "y.name", 0), "str");
        assert_eq!(infer_at(src, "y", 0), "Foo");
    }

    #[test]
    fn early_exits_in_loops_and_outer_names() {
        let src = "def f(items: list[int | None]):\n    for it in items:\n        if it is None:\n            continue\n        it\n";
        assert_eq!(infer_at(src, "it\n", 0), "int");
        let src = "from typing import Optional\nCONFIG: Optional[str] = None\ndef f():\n    if CONFIG is not None:\n        CONFIG\n";
        assert_eq!(infer_at(src, "CONFIG\n", 0), "str");
    }

    #[test]
    fn rebound_names_get_no_false_warnings_and_hover_is_narrowed() {
        let db = Analysis::for_project(Path::new("/proj"));
        let src = "def f(line: str, x: str):\n    parts = line.split(',')\n    parts.append(x)\n    parts = ','.join(parts)\n    return parts.upper()\n";
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let file = db.file(id).unwrap();
        let msgs: Vec<String> = crate::features::diagnostics(&db, &file)
            .into_iter()
            .map(|d| d.message)
            .collect();
        assert!(
            !msgs
                .iter()
                .any(|m| m.contains("append") || m.contains("upper")),
            "{msgs:?}"
        );
        let src = "class Foo:\n    def go(self) -> int: ...\ndef f(x: Foo | None):\n    if x is not None:\n        x.go()\n";
        let id = db.set_file_text(Path::new("/proj/n.py"), src);
        let file = db.file(id).unwrap();
        let off = rustpy_core::TextSize::new(src.find("x.go").unwrap() as u32);
        let h = crate::features::hover_at(&db, &file, off).unwrap();
        assert_eq!(h.signature, "x: Foo");
        // Completion after `x.` offers Foo's members.
        let src2 = format!("{src}        x.\n");
        let id = db.set_file_text(Path::new("/proj/n.py"), &src2);
        let file = db.file(id).unwrap();
        let items = crate::features::complete(
            &db,
            &file,
            rustpy_core::TextSize::new(src2.len() as u32 - 1),
        );
        assert!(
            items.iter().any(|i| i.label == "go"),
            "{:?}",
            items.iter().map(|i| i.label.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn slots_and_explicit_dunder_new() {
        let src = "class D:\n    __slots__ = ('_v', '_w')\n    def __new__(cls, v):\n        self = object.__new__(cls)\n        self._v = v\n        return self\n    def get(self):\n        return self._v, self._w\ndef make():\n    d = object.__new__(D)\n    d._v = 1\n    return d\nmake()\n";
        assert_eq!(infer(src), "D");
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let file = db.file(id).unwrap();
        let msgs: Vec<String> = crate::features::diagnostics(&db, &file)
            .into_iter()
            .map(|d| d.message)
            .collect();
        assert!(msgs.is_empty(), "{msgs:?}");
    }

    #[test]
    fn augmented_assignments_bind_the_operation_result() {
        assert_eq!(infer("total = 0\ntotal += 1.5\ntotal\n"), "float");
        assert_eq!(
            infer("total = 0\nfor i in range(3):\n    total += 1.5\ntotal\n"),
            "int | float"
        );
        assert_eq!(infer("items = [1]\nitems += [2]\nitems\n"), "list[int]");
        assert_eq!(infer("s = 'a'\ns *= 3\ns\n"), "str");
    }

    #[test]
    fn only_the_normal_path_continues_after_try_finally() {
        let src = "x = 1\ntry:\n    x = 'a'\nfinally:\n    x\nx\n";
        // Inside `finally` the body may have stopped early; after it, it completed.
        assert_eq!(infer_at(src, "    x\n", 0), "str | int");
        assert_eq!(infer(src), "str");
        assert_eq!(
            infer(
                "x = 1\ntry:\n    x = 'a'\nexcept ValueError:\n    pass\nfinally:\n    pass\nx\n"
            ),
            "str | int"
        );
    }

    #[test]
    fn loop_carried_values_do_not_poison_types() {
        let src = "total = 0\nfor i in range(3):\n    total = total + i\ntotal\n";
        assert_eq!(infer(src), "int");
        let src = "n = 0\nwhile n < 3:\n    n += 1\nn\n";
        assert_eq!(infer(src), "int");
    }
}

#[cfg(test)]
mod literal_membership_boundary_tests {
    use super::*;
    use crate::db::Analysis;
    use std::path::Path;
    fn project(annotation: &str, container: &str) -> (Type, Type) {
        let source = format!("from typing import Literal\ndef f(value:{annotation}):\n    value #premise\n    if value not in {container}:\n        pass\n");
        let db = Analysis::for_project(Path::new("/membership-boundary"));
        let id = db.set_file_text(Path::new("/membership-boundary/m.py"), &source);
        let file = db.file(id).unwrap();
        assert!(file.parse.errors().is_empty());
        let root = file.parse.syntax_node();
        let at = source.find("value #premise").unwrap();
        let read = root
            .descendants()
            .filter_map(ast::NameExpr::cast)
            .find(|name| usize::from(name.text_range().start()) == at)
            .unwrap();
        let inf = Inferer::new(&db);
        let actual = inf.type_of_expr(&file, &ast::Expr::Name(read));
        let condition = root.descendants().find_map(ast::CompareExpr::cast).unwrap();
        let narrowed = inf.narrow_cond(
            &file,
            &actual,
            &ast::Expr::Compare(condition),
            true,
            "value",
            0,
        );
        (actual, narrowed)
    }
    #[test]
    fn none_and_all_removed_complements_use_actual_declared_input() {
        let (actual, narrowed) = project("Literal[1]|None", "(None,)");
        let Type::Union(arms) = actual else {
            panic!("actual Optional literal premise");
        };
        assert!(arms.iter().any(|arm| matches!(arm, Type::None)));
        assert!(arms
            .iter()
            .any(|arm| matches!(arm, Type::Literal(LiteralValue::Int(1)))));
        assert!(matches!(narrowed, Type::Literal(LiteralValue::Int(1))));
        let (actual, narrowed) = project("Literal[1,2]", "(1,2)");
        let Type::Union(arms) = actual else {
            panic!("actual literal union premise");
        };
        assert_eq!(arms.len(), 2);
        assert!(arms.iter().all(|arm| matches!(arm, Type::Literal(_))));
        assert!(matches!(narrowed, Type::Never));
    }
    #[test]
    fn empty_literal_container_preserves_actual_complement() {
        let (actual, narrowed) = project("Literal[1,2]", "()");
        let Type::Union(arms) = &actual else {
            panic!("actual literal union premise");
        };
        assert_eq!(arms.len(), 2);
        assert_eq!(narrowed, actual);
    }
    #[test]
    fn negative_container_admission_boundary_is_256_elements() {
        for count in [256, 257] {
            let container = format!("({})", vec!["1"; count].join(","));
            let (actual, narrowed) = project("Literal[1,2]", &container);
            let Type::Union(arms) = &actual else {
                panic!("actual literal union premise");
            };
            assert_eq!(arms.len(), 2);
            if count == 256 {
                assert!(matches!(narrowed, Type::Literal(LiteralValue::Int(2))));
            } else {
                assert_eq!(narrowed, actual);
            }
        }
    }
}
