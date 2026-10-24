//! Bounded, inert module __all__ evaluation shared by indexing and semantic clients.
use crate::{
    ast::{self, AstNode},
    SyntaxKind, SyntaxNode,
};
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::{Rc, Weak},
};
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Exports {
    pub names: Vec<String>,
    pub exact: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SequenceKind {
    List,
    Tuple,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedExportValue {
    String {
        value: String,
        exact: bool,
    },
    Sequence {
        exports: Exports,
        kind: SequenceKind,
    },
}
pub const MAX_NAMES: usize = 20_000;
pub const MAX_TEXT: usize = 1 << 20;
pub const MAX_STEPS: usize = 250_000;
const MAX_BINDINGS: usize = 1024;
#[derive(Clone)]
enum Value {
    String(String, bool),
    List(Rc<RefCell<Exports>>, bool),
}
struct Eval<'a> {
    resolver: &'a mut dyn FnMut(&ast::Expr) -> Option<ResolvedExportValue>,
    resolved: HashMap<SyntaxNode, Option<Value>>,
    resolver_tainted: bool,
    external_sequences: Vec<Weak<RefCell<Exports>>>,
    external_uses: usize,
    bindings: HashMap<String, Value>,
    remaining: usize,
    bytes: usize,
    exhausted: bool,
    saw_all: bool,
}
impl Eval<'_> {
    fn step(&mut self) -> bool {
        if self.remaining == 0 {
            self.exhausted = true;
            false
        } else {
            self.remaining -= 1;
            true
        }
    }
    fn resolve(&mut self, expr: &ast::Expr) -> Option<Value> {
        if self.resolver_tainted || !self.step() {
            return None;
        }
        // Only unresolved names/attributes are delegated, never execution/calls.
        if !matches!(expr, ast::Expr::Name(_) | ast::Expr::Attribute(_)) {
            return None;
        }
        if let Some(value) = self.resolved.get(expr.syntax()) {
            if matches!(value, Some(Value::List(_, false))) {
                self.external_uses += 1;
            }
            return value.clone();
        }
        if self.resolved.len() >= MAX_BINDINGS {
            self.exhausted = true;
            return None;
        }
        let value = (self.resolver)(expr).and_then(|value| {
            let (size, count) = match &value {
                ResolvedExportValue::String { value, .. } => (value.len(), 1),
                ResolvedExportValue::Sequence { exports, .. } => (
                    exports
                        .names
                        .iter()
                        .try_fold(0usize, |sum, name| sum.checked_add(name.len()))?,
                    exports.names.len(),
                ),
            };
            if count > MAX_NAMES || self.bytes.saturating_add(size) > MAX_TEXT {
                self.exhausted = true;
                return None;
            }
            self.bytes += size;
            Some(match value {
                ResolvedExportValue::String { value, exact } => Value::String(value, exact),
                ResolvedExportValue::Sequence { exports, kind } => {
                    Value::List(Rc::new(RefCell::new(exports)), kind == SequenceKind::Tuple)
                }
            })
        });
        if matches!(value, Some(Value::List(_, false))) {
            self.external_uses += 1;
        }
        self.resolved.insert(expr.syntax().clone(), value.clone());
        value
    }
    fn retire_external_peers(&mut self, target: &Rc<RefCell<Exports>>) {
        if !self
            .external_sequences
            .iter()
            .filter_map(Weak::upgrade)
            .any(|value| Rc::ptr_eq(&value, target))
        {
            return;
        }
        for value in self.external_sequences.iter().filter_map(Weak::upgrade) {
            if !Rc::ptr_eq(&value, target) {
                value.borrow_mut().exact = false;
            }
        }
    }
    fn scalar(&mut self, expr: ast::Expr) -> Option<String> {
        let mut pending = vec![expr];
        let mut out = String::new();
        while let Some(expr) = pending.pop() {
            if !self.step() {
                return None;
            }
            match expr.unparen() {
                ast::Expr::String(s) => {
                    let raw = s.raw_text();
                    if raw.len() > MAX_TEXT {
                        return None;
                    }
                    let value = crate::string_literal::text(&raw).ok()?;
                    if out.len() + value.len() > MAX_TEXT {
                        return None;
                    }
                    out.push_str(&value);
                }
                ast::Expr::ConcatString(s) => {
                    let parts: Vec<_> = s.parts().collect();
                    if parts.len() > self.remaining {
                        return None;
                    }
                    pending.extend(parts.into_iter().rev());
                }
                expr @ (ast::Expr::Name(_) | ast::Expr::Attribute(_)) => {
                    let value = if let ast::Expr::Name(name) = &expr {
                        self.bindings
                            .get(&name.name_text())
                            .cloned()
                            .or_else(|| self.resolve(&expr))
                    } else {
                        self.resolve(&expr)
                    };
                    match value {
                        Some(Value::String(value, true)) if out.len() + value.len() <= MAX_TEXT => {
                            out.push_str(&value)
                        }
                        _ => return None,
                    }
                }
                ast::Expr::Binary(b) if plus(&b) => {
                    let mut parts = ast::children::<ast::Expr>(b.syntax());
                    let (Some(l), Some(r)) = (parts.next(), parts.next()) else {
                        return None;
                    };
                    pending.push(r);
                    pending.push(l);
                }
                _ => return None,
            }
        }
        Some(out)
    }
    fn sequence(&mut self, expr: ast::Expr) -> (Exports, bool) {
        let mut out = Exports {
            names: Vec::new(),
            exact: true,
        };
        let mut bytes = 0;
        let mut kind = None;
        let mut pending = vec![expr];
        while let Some(expr) = pending.pop() {
            if !self.step() {
                out.exact = false;
                break;
            }
            match expr.unparen() {
                ast::Expr::List(l) => {
                    merge_kind(&mut kind, false, &mut out);
                    for e in l.elements() {
                        match self.scalar(e) {
                            Some(s) => append(&mut out, &mut bytes, s),
                            None => out.exact = false,
                        }
                    }
                }
                ast::Expr::Tuple(t) => {
                    merge_kind(&mut kind, true, &mut out);
                    for e in t.elements() {
                        match self.scalar(e) {
                            Some(s) => append(&mut out, &mut bytes, s),
                            None => out.exact = false,
                        }
                    }
                }
                expr @ (ast::Expr::Name(_) | ast::Expr::Attribute(_)) => {
                    let value = if let ast::Expr::Name(name) = &expr {
                        self.bindings
                            .get(&name.name_text())
                            .cloned()
                            .or_else(|| self.resolve(&expr))
                    } else {
                        self.resolve(&expr)
                    };
                    if let Some(Value::List(value, tuple)) = value {
                        merge_kind(&mut kind, tuple, &mut out);
                        let value = value.borrow();
                        out.exact &= value.exact;
                        for name in &value.names {
                            append(&mut out, &mut bytes, name.clone());
                        }
                    } else {
                        out.exact = false;
                    }
                }
                ast::Expr::Binary(b) => {
                    out.exact &= plus(&b);
                    let mut parts = ast::children::<ast::Expr>(b.syntax());
                    if let (Some(l), Some(r)) = (parts.next(), parts.next()) {
                        pending.push(r);
                        pending.push(l)
                    } else {
                        out.exact = false
                    }
                }
                _ => out.exact = false,
            }
        }
        (out, kind.unwrap_or(false))
    }
    fn value(&mut self, expr: ast::Expr) -> Value {
        if let ast::Expr::Name(n) = expr.clone().unparen() {
            if let Some(value) = self.bindings.get(&n.name_text()) {
                return value.clone();
            }
        }
        if matches!(
            expr.clone().unparen(),
            ast::Expr::Name(_) | ast::Expr::Attribute(_)
        ) {
            if let Some(value) = self.resolve(&expr.clone().unparen()) {
                if let Value::List(list, false) = &value {
                    if self.external_sequences.len() >= MAX_BINDINGS {
                        self.exhausted = true;
                        list.borrow_mut().exact = false;
                    } else {
                        self.external_sequences.push(Rc::downgrade(list));
                    }
                }
                return value;
            }
        }
        let external_before = self.external_uses;
        if let Some(value) = self.scalar(expr.clone()) {
            Value::String(value, true)
        } else {
            let (value, tuple) = self.sequence(expr);
            let value = Rc::new(RefCell::new(value));
            if !tuple && self.external_uses > external_before {
                if self.external_sequences.len() >= MAX_BINDINGS {
                    self.exhausted = true;
                    value.borrow_mut().exact = false;
                } else {
                    self.external_sequences.push(Rc::downgrade(&value));
                }
            }
            Value::List(value, tuple)
        }
    }
    fn assign(&mut self, name: String, mut value: Value, uncertain: bool) {
        self.saw_all |= name == "__all__";
        if uncertain {
            match &mut value {
                Value::List(v, _) => v.borrow_mut().exact = false,
                Value::String(_, exact) => *exact = false,
            }
        }
        let size = match &value {
            Value::String(s, _) => s.len(),
            Value::List(v, _) => v.borrow().names.iter().map(String::len).sum(),
        };
        if self.bindings.len() >= MAX_BINDINGS || self.bytes.saturating_add(size) > MAX_TEXT {
            self.exhausted = true;
            self.bindings.remove(&name);
            return;
        }
        self.bytes += size;
        self.bindings.insert(name, value);
    }
    fn invalidate(&mut self, target: &ast::Expr) {
        self.resolver_tainted = true;
        let mut expr = target.clone().unparen();
        loop {
            match expr {
                ast::Expr::Name(n) => {
                    self.saw_all |= n.name_text() == "__all__";
                    if let Some(Value::List(v, _)) = self.bindings.get(&n.name_text()).cloned() {
                        self.retire_external_peers(&v);
                        v.borrow_mut().exact = false;
                    } else {
                        self.bindings.remove(&n.name_text());
                    }
                    break;
                }
                ast::Expr::Subscript(s) => {
                    if let Some(e) = s.object() {
                        expr = e.unparen();
                    } else {
                        break;
                    }
                }
                ast::Expr::Attribute(a) => {
                    if let Some(e) = a.object() {
                        expr = e.unparen();
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
    }
    fn invalidate_runtime_authority(&mut self) {
        self.resolver_tainted = true;
        for value in self.bindings.values_mut() {
            match value {
                Value::String(_, exact) => *exact = false,
                Value::List(value, _) => value.borrow_mut().exact = false,
            }
        }
    }
    fn proven_mutation(&self, expr: &ast::Expr) -> bool {
        let ast::Expr::Call(call) = expr.clone().unparen() else {
            return false;
        };
        let Some(ast::Expr::Attribute(attr)) = call.callee() else {
            return false;
        };
        let Some(ast::Expr::Name(name)) = attr.object().map(ast::Expr::unparen) else {
            return false;
        };
        if !matches!(
            self.bindings.get(&name.name_text()),
            Some(Value::List(_, false))
        ) || !matches!(attr.attr_text().as_str(), "append" | "extend" | "remove")
        {
            return false;
        }
        let Some(args) = call.arg_list() else {
            return false;
        };
        let args: Vec<_> = args.args().collect();
        matches!(args.as_slice(), [ast::AnyArg::Positional(_)])
    }
    fn inert_class(&mut self, class: &ast::ClassDef) -> bool {
        if class.decorators().next().is_some()
            || class
                .arguments()
                .is_some_and(|args| args.args().next().is_some())
        {
            return false;
        }
        let Some(body) = class.body() else {
            return false;
        };
        for stmt in body.statements() {
            if !self.step() {
                return false;
            }
            match stmt {
                ast::Stmt::Pass(_) => {}
                ast::Stmt::Expr(expr)
                    if matches!(
                        expr.expr(),
                        Some(ast::Expr::String(_) | ast::Expr::ConcatString(_))
                    ) => {}
                ast::Stmt::Assign(assign) => {
                    if !assign
                        .targets()
                        .into_iter()
                        .all(|target| matches!(target.unparen(), ast::Expr::Name(_)))
                        || !matches!(
                            assign.value().map(ast::Expr::unparen),
                            Some(
                                ast::Expr::Literal(_)
                                    | ast::Expr::String(_)
                                    | ast::Expr::ConcatString(_)
                            )
                        )
                    {
                        return false;
                    }
                }
                ast::Stmt::FuncDef(function) => {
                    if function.decorators().next().is_some() {
                        return false;
                    }
                    // Defaults/annotations execute now; method bodies do not.
                    let mut pending = vec![function.syntax().clone()];
                    while let Some(node) = pending.pop() {
                        if !self.step() {
                            return false;
                        }
                        if ast::Block::cast(node.clone()).is_some() {
                            continue;
                        }
                        if ast::Expr::cast(node.clone()).is_some_and(|expr| {
                            !matches!(
                                expr,
                                ast::Expr::Name(_)
                                    | ast::Expr::Literal(_)
                                    | ast::Expr::String(_)
                                    | ast::Expr::ConcatString(_)
                                    | ast::Expr::Paren(_)
                                    | ast::Expr::Tuple(_)
                                    | ast::Expr::List(_)
                            )
                        }) {
                            return false;
                        }
                        pending.extend(node.children());
                    }
                }
                _ => return false,
            }
        }
        true
    }
    // Only statement headers execute now; nested suites are handled separately.
    // Function bodies are inert; class construction executes code and hooks.
    fn check_runtime_calls(&mut self, stmt: &ast::Stmt) {
        // Only bare builtin-object classes with inert literal/definition bodies
        // are proven safe. Custom bases/metaclasses/decorators remain unknown.
        if let ast::Stmt::ClassDef(class) = stmt {
            if !self.inert_class(class) {
                self.invalidate_runtime_authority();
            }
            return;
        }
        let safe_root = if let ast::Stmt::Expr(e) = stmt {
            e.expr()
                .filter(|e| self.proven_mutation(e))
                .map(|e| e.syntax().clone())
        } else {
            None
        };
        let mut pending = vec![stmt.syntax().clone()];
        while let Some(node) = pending.pop() {
            if !self.step() {
                self.invalidate_runtime_authority();
                return;
            }
            // Every decorator expression is invoked implicitly, including @name.
            if ast::Decorator::cast(node.clone()).is_some() {
                self.invalidate_runtime_authority();
                return;
            }
            if ast::Block::cast(node.clone()).is_some() {
                continue;
            }
            if matches!(ast::Expr::cast(node.clone()), Some(ast::Expr::Call(_)))
                && safe_root.as_ref() != Some(&node)
            {
                self.invalidate_runtime_authority();
                return;
            }
            pending.extend(node.children());
        }
    }
    fn mutate(&mut self, expr: ast::Expr, uncertain: bool) {
        let ast::Expr::Call(call) = expr.unparen() else {
            return;
        };
        let Some(ast::Expr::Attribute(attr)) = call.callee() else {
            if let Some(args) = call.arg_list() {
                for arg in args.positional() {
                    if let ast::Expr::Name(n) = arg.unparen() {
                        if let Some(Value::List(value, _)) = self.bindings.get(&n.name_text()) {
                            value.borrow_mut().exact = false;
                        }
                    }
                }
            }
            return;
        };
        let Some(ast::Expr::Name(n)) = attr.object().map(ast::Expr::unparen) else {
            return;
        };
        let name = n.name_text();
        self.saw_all |= name == "__all__";
        if name == "__all__" && !self.bindings.contains_key(&name) {
            self.bindings.insert(
                name.clone(),
                Value::List(Rc::new(RefCell::new(Exports::default())), false),
            );
        }
        let Some(Value::List(value, tuple)) = self.bindings.get(&name).cloned() else {
            return;
        };
        if tuple {
            value.borrow_mut().exact = false;
            return;
        }
        let args = call
            .arg_list()
            .map(|a| a.args().collect::<Vec<_>>())
            .unwrap_or_default();
        let [ast::AnyArg::Positional(arg)] = args.as_slice() else {
            value.borrow_mut().exact = false;
            return;
        };
        let Some(arg) = arg.expr() else {
            value.borrow_mut().exact = false;
            return;
        };
        let method = attr.attr_text();
        let additions = if method == "extend" {
            Some(self.sequence(arg.clone()).0)
        } else {
            None
        };
        let scalar = if matches!(method.as_str(), "append" | "remove") {
            self.scalar(arg)
        } else {
            None
        };
        self.resolver_tainted = true;
        let added_bytes = additions.as_ref().map_or_else(
            || scalar.as_ref().map_or(0, String::len),
            |a| a.names.iter().map(String::len).sum(),
        );
        if self.bytes.saturating_add(added_bytes) > MAX_TEXT {
            self.exhausted = true;
            value.borrow_mut().exact = false;
            return;
        }
        self.bytes += added_bytes;
        self.retire_external_peers(&value);
        let mut value = value.borrow_mut();
        value.exact &= !uncertain;
        let mut bytes = value.names.iter().map(String::len).sum();
        match method.as_str() {
            "extend" => {
                let addition = additions.unwrap();
                value.exact &= addition.exact;
                for s in addition.names {
                    append(&mut value, &mut bytes, s)
                }
            }
            "append" => {
                if let Some(s) = scalar {
                    append(&mut value, &mut bytes, s)
                } else {
                    value.exact = false
                }
            }
            "remove" => {
                if let Some(s) = scalar {
                    if let Some(i) = value.names.iter().position(|n| n == &s) {
                        value.names.remove(i);
                    } else {
                        value.exact = false
                    }
                } else {
                    value.exact = false
                }
            }
            _ => value.exact = false,
        }
    }
}
fn merge_kind(kind: &mut Option<bool>, next: bool, out: &mut Exports) {
    if let Some(old) = kind {
        out.exact &= *old == next;
    } else {
        *kind = Some(next)
    }
}
fn append(out: &mut Exports, bytes: &mut usize, s: String) {
    if out.names.len() >= MAX_NAMES || bytes.saturating_add(s.len()) > MAX_TEXT {
        out.exact = false;
    } else {
        *bytes += s.len();
        out.names.push(s);
    }
}
fn plus(b: &ast::BinaryExpr) -> bool {
    b.syntax()
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .any(|t| t.kind() == SyntaxKind::PLUS)
}
/// None means no __all__ binding was seen. Partial names never authorize exclusion.
/// Functions/classes are not entered. Conditional/loop/try/with writes are nonexact.
pub fn evaluate(root: &SyntaxNode) -> Option<Exports> {
    evaluate_with(root, &mut |_| None)
}
/// Resolve only otherwise-unbound names/attributes. The caller owns dependency
/// identity, current import binding, cycle/depth/module budgets and exactness.
/// Callback values never execute code and are charged to this evaluator budget.
pub fn evaluate_with(
    root: &SyntaxNode,
    resolver: &mut impl FnMut(&ast::Expr) -> Option<ResolvedExportValue>,
) -> Option<Exports> {
    match evaluate_binding_with(root, "__all__", resolver)? {
        ResolvedExportValue::Sequence { exports, .. } => Some(exports),
        ResolvedExportValue::String { .. } => Some(Exports::default()),
    }
}
/// Read a named final tracked value without losing Python list/tuple kind.
/// Resolver identity and recursion budgets remain caller obligations.
pub fn evaluate_binding_with(
    root: &SyntaxNode,
    binding_name: &str,
    resolver: &mut impl FnMut(&ast::Expr) -> Option<ResolvedExportValue>,
) -> Option<ResolvedExportValue> {
    let module = ast::Module::cast(root.clone())?;
    let mut eval = Eval {
        resolver,
        resolved: HashMap::new(),
        resolver_tainted: false,
        external_sequences: Vec::new(),
        external_uses: 0,
        bindings: HashMap::new(),
        remaining: MAX_STEPS,
        bytes: 0,
        exhausted: false,
        saw_all: false,
    };
    let mut pending: Vec<_> = module
        .statements()
        .take(MAX_STEPS + 1)
        .map(|s| (s, false))
        .collect();
    pending.reverse();
    while let Some((stmt, uncertain)) = pending.pop() {
        if !eval.step() {
            break;
        }
        eval.check_runtime_calls(&stmt);
        if let ast::Stmt::For(f) = &stmt {
            if let Some(target) = f.target() {
                // Include tuple/list destructuring names, not only a bare target.
                for node in target.syntax().descendants() {
                    if let Some(ast::Expr::Name(name)) = ast::Expr::cast(node) {
                        eval.invalidate(&ast::Expr::Name(name));
                    }
                }
            }
        }
        match stmt {
            ast::Stmt::Assign(a) => {
                if let Some(expr) = a.value() {
                    let value = eval.value(expr);
                    for target in a.targets() {
                        if let ast::Expr::Name(n) = target.clone().unparen() {
                            eval.assign(n.name_text(), value.clone(), uncertain);
                        } else {
                            eval.invalidate(&target);
                        }
                    }
                }
            }
            ast::Stmt::AnnAssign(a) => {
                if let (Some(target), Some(expr)) = (a.target(), a.value()) {
                    if let ast::Expr::Name(n) = target.clone().unparen() {
                        let value = eval.value(expr);
                        eval.assign(n.name_text(), value, uncertain);
                    } else {
                        eval.invalidate(&target);
                    }
                }
            }
            ast::Stmt::AugAssign(a) => {
                if let Some(target) = a.target() {
                    if let ast::Expr::Name(n) = target.clone().unparen() {
                        let name = n.name_text();
                        if a.op().is_some_and(|t| t.text() == "+=") {
                            if let Some(expr) = a.value() {
                                let (additions, tuple) = eval.sequence(expr);
                                eval.resolver_tainted = true;
                                let size = additions.names.iter().map(String::len).sum::<usize>();
                                if let Some(Value::List(value, old_tuple)) =
                                    eval.bindings.get(&name).cloned()
                                {
                                    let retained = size
                                        + if old_tuple {
                                            value
                                                .borrow()
                                                .names
                                                .iter()
                                                .map(String::len)
                                                .sum::<usize>()
                                        } else {
                                            0
                                        };
                                    if eval.bytes.saturating_add(retained) > MAX_TEXT {
                                        eval.exhausted = true;
                                        value.borrow_mut().exact = false;
                                        continue;
                                    }
                                    eval.bytes += retained;
                                    if old_tuple {
                                        let mut combined = value.borrow().clone();
                                        combined.exact &= additions.exact && !uncertain && tuple;
                                        let mut bytes =
                                            combined.names.iter().map(String::len).sum();
                                        for s in additions.names {
                                            append(&mut combined, &mut bytes, s)
                                        }
                                        eval.bindings.insert(
                                            name,
                                            Value::List(Rc::new(RefCell::new(combined)), true),
                                        );
                                    } else {
                                        eval.retire_external_peers(&value);
                                        let mut value = value.borrow_mut();
                                        value.exact &= additions.exact && !uncertain;
                                        let mut bytes = value.names.iter().map(String::len).sum();
                                        for s in additions.names {
                                            append(&mut value, &mut bytes, s)
                                        }
                                    }
                                } else if name == "__all__" {
                                    eval.assign(
                                        name,
                                        Value::List(
                                            Rc::new(RefCell::new(Exports {
                                                names: additions.names,
                                                exact: false,
                                            })),
                                            tuple,
                                        ),
                                        true,
                                    );
                                } else {
                                    eval.bindings.remove(&name);
                                }
                            }
                        } else {
                            eval.invalidate(&target);
                        }
                    } else {
                        eval.invalidate(&target);
                    }
                }
            }
            ast::Stmt::Del(d) => {
                for target in d.targets() {
                    eval.invalidate(&target);
                }
            }
            ast::Stmt::Expr(e) => {
                if let Some(expr) = e.expr() {
                    eval.mutate(expr, uncertain)
                }
            }
            ast::Stmt::FuncDef(f) => {
                if let Some(n) = f.name() {
                    eval.bindings.remove(n.text());
                }
            }
            ast::Stmt::ClassDef(c) => {
                if let Some(n) = c.name() {
                    eval.bindings.remove(n.text());
                }
            }
            ast::Stmt::Import(i) => {
                for a in i.aliases() {
                    if let Some(n) = a.bound_name() {
                        eval.bindings.remove(&n);
                    }
                }
            }
            ast::Stmt::ImportFrom(i) => {
                for a in i.aliases() {
                    let n = a
                        .alias()
                        .map(|n| n.text().to_string())
                        .unwrap_or_else(|| a.name());
                    eval.bindings.remove(&n);
                }
            }
            other => {
                let mut nested = Vec::new();
                for block in ast::children::<ast::Block>(other.syntax()) {
                    let available = eval.remaining.saturating_sub(pending.len() + nested.len());
                    let items: Vec<_> = block.statements().take(available + 1).collect();
                    if items.len() > available {
                        eval.exhausted = true;
                    }
                    nested.extend(items.into_iter().take(available));
                }
                for child in other.syntax().children() {
                    if let Some(block) = ast::child::<ast::Block>(&child) {
                        let available = eval.remaining.saturating_sub(pending.len() + nested.len());
                        let items: Vec<_> = block.statements().take(available + 1).collect();
                        if items.len() > available {
                            eval.exhausted = true;
                        }
                        nested.extend(items.into_iter().take(available));
                    }
                }
                pending.extend(nested.into_iter().rev().map(|s| (s, true)));
            }
        }
    }
    match eval.bindings.get(binding_name) {
        Some(Value::List(value, tuple)) => {
            let mut exports = value.borrow().clone();
            exports.exact &= !eval.exhausted;
            Some(ResolvedExportValue::Sequence {
                exports,
                kind: if *tuple {
                    SequenceKind::Tuple
                } else {
                    SequenceKind::List
                },
            })
        }
        Some(Value::String(value, exact)) => Some(ResolvedExportValue::String {
            value: value.clone(),
            exact: *exact && !eval.exhausted,
        }),
        None if binding_name == "__all__" && eval.saw_all => Some(ResolvedExportValue::Sequence {
            exports: Exports::default(),
            kind: SequenceKind::List,
        }),
        None => None,
    }
}
