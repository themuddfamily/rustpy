//! Per-file scope and binding analysis (symbol table) with LEGB resolution.

use indexmap::IndexMap;
use rustc_hash::FxHashMap;
use rustpy_core::TextRange;
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::{Parse, SyntaxKind, SyntaxNode};
use smol_str::SmolStr;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopeId(pub u32);
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BindingId(pub u32);

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ScopeKind {
    Module,
    Class,
    Function,
    Lambda,
    Comprehension,
    /// PEP 695 annotation scope for `def f[T]`, `class C[T]`, `type X[T] = ...`.
    TypeParams,
}

/// Details of an import binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportInfo {
    /// Dotted module path as written (`..pkg.mod` for relative imports).
    pub module: SmolStr,
    /// For `from m import x`: the imported name; `None` for plain `import m`.
    pub name: Option<SmolStr>,
    pub alias: Option<SmolStr>,
    pub level: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindingKind {
    Assignment,
    AugAssignment,
    /// `x: int` without value.
    Annotation,
    Param,
    StarParam,
    StarStarParam,
    Function,
    Class,
    Import(ImportInfo),
    For,
    With,
    Except,
    Walrus,
    MatchCapture,
    TypeParam,
    TypeAlias,
    /// `global x` declaration (re-exports a module binding into a function).
    Global,
    Nonlocal,
    Del,
    CompTarget,
}

#[derive(Clone, Debug)]
pub struct Binding {
    pub name: SmolStr,
    pub kind: BindingKind,
    /// Range of the name token.
    pub range: TextRange,
    /// Range of the whole defining node (statement / parameter / import alias).
    pub node_range: TextRange,
    pub scope: ScopeId,
    /// Assigned value expression (for `x = value`), when available.
    pub value: Option<TextRange>,
    /// Annotation expression range, when available.
    pub annotation: Option<TextRange>,
}

impl Binding {
    pub fn is_definition(&self) -> bool {
        !matches!(
            self.kind,
            BindingKind::Global | BindingKind::Nonlocal | BindingKind::Del
        )
    }
}

#[derive(Clone, Debug)]
pub struct Scope {
    pub kind: ScopeKind,
    pub parent: Option<ScopeId>,
    /// The defining node (module / class / function / lambda / comprehension).
    pub range: TextRange,
    /// Name of the function/class owning the scope.
    pub name: Option<SmolStr>,
    pub bindings: IndexMap<SmolStr, Vec<BindingId>>,
    pub globals: Vec<SmolStr>,
    pub nonlocals: Vec<SmolStr>,
    pub star_imports: Vec<ImportInfo>,
    pub children: Vec<ScopeId>,
    pub is_async: bool,
}

#[derive(Clone, Debug)]
pub struct Reference {
    pub name: SmolStr,
    pub range: TextRange,
    pub scope: ScopeId,
    pub resolved: Option<BindingId>,
    /// True when the name is a `del` target or otherwise written (not read).
    pub is_write: bool,
}

/// Symbol table of one file.
#[derive(Clone, Debug, Default)]
pub struct ScopeModel {
    pub scopes: Vec<Scope>,
    pub bindings: Vec<Binding>,
    pub references: Vec<Reference>,
    scope_by_range: FxHashMap<TextRange, ScopeId>,
}

impl ScopeModel {
    pub fn module_scope(&self) -> ScopeId {
        ScopeId(0)
    }
    pub fn scope(&self, id: ScopeId) -> &Scope {
        &self.scopes[id.0 as usize]
    }
    pub fn binding(&self, id: BindingId) -> &Binding {
        &self.bindings[id.0 as usize]
    }
    /// Scope of the node with exactly this range, if it defines a scope.
    pub fn scope_for_node(&self, range: TextRange) -> Option<ScopeId> {
        self.scope_by_range.get(&range).copied()
    }
    /// Innermost scope containing `offset`.
    pub fn scope_at(&self, offset: rustpy_core::TextSize) -> ScopeId {
        let mut best = ScopeId(0);
        for (i, s) in self.scopes.iter().enumerate() {
            if s.range.contains_inclusive(offset)
                && s.range.len() <= self.scopes[best.0 as usize].range.len()
                && i > 0
            {
                best = ScopeId(i as u32);
            }
        }
        best
    }
    /// All bindings of `name` in `scope` (in source order).
    pub fn bindings_in(&self, scope: ScopeId, name: &str) -> &[BindingId] {
        self.scope(scope)
            .bindings
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
    /// Resolves `name` as seen from `scope` using LEGB rules. Returns the
    /// binding ids (all bindings of the resolved scope for that name).
    pub fn resolve(&self, scope: ScopeId, name: &str) -> Option<&[BindingId]> {
        let mut cur = Some(scope);
        let mut first = true;
        // PEP 695 annotation scopes (type parameter lists, and the bounds,
        // bases and annotations evaluated in them) see the names of a class
        // that directly encloses them; ordinary nested scopes do not.
        let mut annotation_only = true;
        while let Some(id) = cur {
            let s = self.scope(id);
            if s.globals.iter().any(|g| g == name) {
                let b = self.bindings_in(self.module_scope(), name);
                return if b.is_empty() { None } else { Some(b) };
            }
            let skip_class = s.kind == ScopeKind::Class && !first && !annotation_only;
            if !skip_class && !s.nonlocals.iter().any(|n| n == name) {
                let b = self.bindings_in(id, name);
                if b.iter().any(|b| self.binding(*b).is_definition()) {
                    return Some(b);
                }
            }
            annotation_only &= s.kind == ScopeKind::TypeParams;
            first = false;
            cur = s.parent;
        }
        None
    }
    pub fn reference_at(&self, offset: rustpy_core::TextSize) -> Option<&Reference> {
        self.references
            .iter()
            .find(|r| r.range.contains_inclusive(offset))
    }
    pub fn binding_at(&self, offset: rustpy_core::TextSize) -> Option<(BindingId, &Binding)> {
        self.bindings
            .iter()
            .enumerate()
            .find(|(_, b)| b.range.contains_inclusive(offset))
            .map(|(i, b)| (BindingId(i as u32), b))
    }
    /// References resolved to `binding` (or any binding of the same name in the same scope).
    pub fn references_to(&self, binding: BindingId) -> Vec<&Reference> {
        let b = self.binding(binding);
        let ids = self.bindings_in(b.scope, &b.name);
        self.references
            .iter()
            .filter(|r| r.resolved.is_some_and(|id| ids.contains(&id)))
            .collect()
    }
}

/// Builds the scope model of a parsed module.
pub fn build(parse: &Parse) -> ScopeModel {
    build_with(parse, None)
}

/// Builds the scope model, skipping `if` branches that `env` rules out
/// (`sys.version_info` / `sys.platform` guards in stubs): their statements
/// create no bindings.
pub fn build_with(parse: &Parse, env: Option<&crate::static_cond::StaticEnv>) -> ScopeModel {
    let root = parse.syntax_node();
    let mut b = Builder {
        model: ScopeModel::default(),
        deferred: Vec::new(),
        env: env.cloned(),
    };
    let module_scope = b.push_scope(ScopeKind::Module, None, root.text_range(), None, false);
    if let Some(module) = ast::Module::cast(root) {
        for stmt in module.statements() {
            b.stmt(&stmt, module_scope);
        }
    }
    // Function bodies are analyzed after the enclosing scope is complete so
    // that later definitions (e.g. functions defined below) resolve.
    while let Some((scope, body)) = b.deferred.pop() {
        b.function_body(scope, &body);
    }
    b.resolve_all();
    b.model
}

struct Builder {
    model: ScopeModel,
    deferred: Vec<(ScopeId, DeferredBody)>,
    env: Option<crate::static_cond::StaticEnv>,
}

enum DeferredBody {
    Block(ast::Block),
    Expr(ast::Expr),
}

impl Builder {
    fn push_scope(
        &mut self,
        kind: ScopeKind,
        parent: Option<ScopeId>,
        range: TextRange,
        name: Option<SmolStr>,
        is_async: bool,
    ) -> ScopeId {
        let id = ScopeId(self.model.scopes.len() as u32);
        self.model.scopes.push(Scope {
            kind,
            parent,
            range,
            name,
            bindings: IndexMap::new(),
            globals: Vec::new(),
            nonlocals: Vec::new(),
            star_imports: Vec::new(),
            children: Vec::new(),
            is_async,
        });
        if let Some(p) = parent {
            self.model.scopes[p.0 as usize].children.push(id);
        }
        self.model.scope_by_range.insert(range, id);
        id
    }

    #[allow(clippy::too_many_arguments)]
    fn bind(
        &mut self,
        scope: ScopeId,
        name: &str,
        kind: BindingKind,
        range: TextRange,
        node_range: TextRange,
        value: Option<TextRange>,
        annotation: Option<TextRange>,
    ) -> BindingId {
        let id = BindingId(self.model.bindings.len() as u32);
        let name = SmolStr::new(name);
        self.model.bindings.push(Binding {
            name: name.clone(),
            kind,
            range,
            node_range,
            scope,
            value,
            annotation,
        });
        self.model.scopes[scope.0 as usize]
            .bindings
            .entry(name)
            .or_default()
            .push(id);
        id
    }

    fn reference(&mut self, scope: ScopeId, name: &str, range: TextRange, is_write: bool) {
        self.model.references.push(Reference {
            name: SmolStr::new(name),
            range,
            scope,
            resolved: None,
            is_write,
        });
    }

    // ----- statements ------------------------------------------------------

    fn stmt(&mut self, stmt: &ast::Stmt, scope: ScopeId) {
        match stmt {
            ast::Stmt::Expr(e) => {
                if let Some(x) = e.expr() {
                    self.expr(&x, scope);
                }
            }
            ast::Stmt::Assign(a) => {
                if let Some(v) = a.value() {
                    self.expr(&v, scope);
                }
                let value = a.value().map(|v| v.text_range());
                for t in a.targets() {
                    self.target(
                        &t,
                        scope,
                        BindingKind::Assignment,
                        a.text_range(),
                        value,
                        None,
                    );
                }
            }
            ast::Stmt::AugAssign(a) => {
                if let Some(v) = a.value() {
                    self.expr(&v, scope);
                }
                if let Some(t) = a.target() {
                    // The target is read and written.
                    self.expr(&t, scope);
                    self.target(
                        &t,
                        scope,
                        BindingKind::AugAssignment,
                        a.text_range(),
                        a.value().map(|v| v.text_range()),
                        None,
                    );
                }
            }
            ast::Stmt::AnnAssign(a) => {
                if let Some(ann) = a.annotation() {
                    self.expr(&ann, scope);
                }
                if let Some(v) = a.value() {
                    self.expr(&v, scope);
                }
                if let Some(t) = a.target() {
                    let kind = if a.value().is_some() {
                        BindingKind::Assignment
                    } else {
                        BindingKind::Annotation
                    };
                    self.target(
                        &t,
                        scope,
                        kind,
                        a.text_range(),
                        a.value().map(|v| v.text_range()),
                        a.annotation().map(|x| x.text_range()),
                    );
                }
            }
            ast::Stmt::Return(r) => {
                if let Some(v) = r.value() {
                    self.expr(&v, scope);
                }
            }
            ast::Stmt::Pass(_) | ast::Stmt::Break(_) | ast::Stmt::Continue(_) => {}
            ast::Stmt::Del(d) => {
                for t in d.targets() {
                    match &t {
                        ast::Expr::Name(n) => {
                            if let Some(tok) = n.name_token() {
                                self.reference(scope, tok.text(), tok.text_range(), true);
                                self.bind(
                                    scope,
                                    tok.text(),
                                    BindingKind::Del,
                                    tok.text_range(),
                                    d.text_range(),
                                    None,
                                    None,
                                );
                            }
                        }
                        other => self.expr(other, scope),
                    }
                }
            }
            ast::Stmt::Assert(a) => {
                if let Some(t) = a.test() {
                    self.expr(&t, scope);
                }
                if let Some(m) = a.message() {
                    self.expr(&m, scope);
                }
            }
            ast::Stmt::Global(g) => {
                for n in g.names() {
                    self.model.scopes[scope.0 as usize]
                        .globals
                        .push(SmolStr::new(n.text()));
                    self.bind(
                        scope,
                        n.text(),
                        BindingKind::Global,
                        n.text_range(),
                        g.text_range(),
                        None,
                        None,
                    );
                }
            }
            ast::Stmt::Nonlocal(g) => {
                for n in g.names() {
                    self.model.scopes[scope.0 as usize]
                        .nonlocals
                        .push(SmolStr::new(n.text()));
                    self.bind(
                        scope,
                        n.text(),
                        BindingKind::Nonlocal,
                        n.text_range(),
                        g.text_range(),
                        None,
                        None,
                    );
                }
            }
            ast::Stmt::Raise(r) => {
                if let Some(e) = r.exception() {
                    self.expr(&e, scope);
                }
                if let Some(c) = r.cause() {
                    self.expr(&c, scope);
                }
            }
            ast::Stmt::Import(i) => {
                for alias in i.aliases() {
                    let name = alias.name();
                    let bound = alias.bound_name().unwrap_or_default();
                    let range = alias
                        .alias()
                        .or_else(|| alias.name_token())
                        .map(|t| t.text_range())
                        .unwrap_or_else(|| alias.text_range());
                    let info = ImportInfo {
                        module: SmolStr::new(&name),
                        name: None,
                        alias: alias.alias().map(|a| SmolStr::new(a.text())),
                        level: 0,
                    };
                    self.bind(
                        scope,
                        &bound,
                        BindingKind::Import(info),
                        range,
                        alias.text_range(),
                        None,
                        None,
                    );
                }
            }
            ast::Stmt::ImportFrom(f) => {
                let module = f.module_path();
                let level = f.level();
                if f.is_star() {
                    self.model.scopes[scope.0 as usize]
                        .star_imports
                        .push(ImportInfo {
                            module: SmolStr::new(&module),
                            name: None,
                            alias: None,
                            level,
                        });
                }
                for alias in f.aliases() {
                    let name = alias.name();
                    let bound = alias.bound_name().unwrap_or_default();
                    let range = alias
                        .alias()
                        .or_else(|| alias.name_token())
                        .map(|t| t.text_range())
                        .unwrap_or_else(|| alias.text_range());
                    let info = ImportInfo {
                        module: SmolStr::new(&module),
                        name: Some(SmolStr::new(&name)),
                        alias: alias.alias().map(|a| SmolStr::new(a.text())),
                        level,
                    };
                    self.bind(
                        scope,
                        &bound,
                        BindingKind::Import(info),
                        range,
                        alias.text_range(),
                        None,
                        None,
                    );
                }
            }
            ast::Stmt::If(i) => {
                let live = crate::static_cond::live_branches(i, self.env.as_ref());
                let is_live = |b: &Option<ast::Block>| {
                    b.as_ref()
                        .is_some_and(|b| live.iter().any(|l| l.syntax() == b.syntax()))
                };
                if let Some(c) = i.condition() {
                    self.expr(&c, scope);
                }
                if is_live(&i.body()) {
                    self.block(i.body(), scope);
                }
                for e in i.elif_clauses() {
                    if let Some(c) = e.condition() {
                        self.expr(&c, scope);
                    }
                    if is_live(&e.body()) {
                        self.block(e.body(), scope);
                    }
                }
                let else_body = i.else_clause().and_then(|e| e.body());
                if is_live(&else_body) {
                    self.block(else_body, scope);
                }
            }
            ast::Stmt::While(w) => {
                if let Some(c) = w.condition() {
                    self.expr(&c, scope);
                }
                self.block(w.body(), scope);
                self.block(w.else_clause().and_then(|e| e.body()), scope);
            }
            ast::Stmt::For(f) => {
                if let Some(it) = f.iter() {
                    self.expr(&it, scope);
                }
                if let Some(t) = f.target() {
                    self.target(
                        &t,
                        scope,
                        BindingKind::For,
                        f.text_range(),
                        f.iter().map(|i| i.text_range()),
                        None,
                    );
                }
                self.block(f.body(), scope);
                self.block(f.else_clause().and_then(|e| e.body()), scope);
            }
            ast::Stmt::Try(t) => {
                self.block(t.body(), scope);
                for ex in t.except_clauses() {
                    if let Some(ty) = ex.exception_type() {
                        self.expr(&ty, scope);
                    }
                    if let Some(n) = ex.name() {
                        self.bind(
                            scope,
                            n.text(),
                            BindingKind::Except,
                            n.text_range(),
                            ex.text_range(),
                            ex.exception_type().map(|t| t.text_range()),
                            None,
                        );
                    }
                    self.block(ex.body(), scope);
                }
                self.block(t.else_clause().and_then(|e| e.body()), scope);
                self.block(t.finally_clause().and_then(|f| f.body()), scope);
            }
            ast::Stmt::With(w) => {
                for item in w.items() {
                    if let Some(c) = item.context_expr() {
                        self.expr(&c, scope);
                    }
                    if let Some(t) = item.target() {
                        self.target(
                            &t,
                            scope,
                            BindingKind::With,
                            item.text_range(),
                            item.context_expr().map(|c| c.text_range()),
                            None,
                        );
                    }
                }
                self.block(w.body(), scope);
            }
            ast::Stmt::FuncDef(f) => self.func_def(f, scope),
            ast::Stmt::ClassDef(c) => self.class_def(c, scope),
            ast::Stmt::TypeAlias(t) => {
                if let Some(n) = t.name() {
                    self.bind(
                        scope,
                        n.text(),
                        BindingKind::TypeAlias,
                        n.text_range(),
                        t.text_range(),
                        t.value().map(|v| v.text_range()),
                        None,
                    );
                }
                let tp_scope = self.type_params_scope(t.syntax(), scope, None);
                if let Some(v) = t.value() {
                    self.expr(&v, tp_scope.unwrap_or(scope));
                }
            }
            ast::Stmt::Match(m) => {
                if let Some(s) = m.subject() {
                    self.expr(&s, scope);
                }
                for case in m.cases() {
                    if let Some(p) = case.pattern() {
                        self.pattern(&p, scope);
                    }
                    if let Some(g) = case.guard() {
                        self.expr(&g, scope);
                    }
                    self.block(case.body(), scope);
                }
            }
        }
    }

    fn block(&mut self, block: Option<ast::Block>, scope: ScopeId) {
        if let Some(b) = block {
            for s in b.statements() {
                self.stmt(&s, scope);
            }
        }
    }

    /// Creates a PEP 695 type-parameter scope if the node has type params.
    fn type_params_scope(
        &mut self,
        node: &SyntaxNode,
        parent: ScopeId,
        name: Option<SmolStr>,
    ) -> Option<ScopeId> {
        let tpl = node
            .children()
            .find(|c| c.kind() == SyntaxKind::TYPE_PARAM_LIST)?;
        let scope = self.push_scope(
            ScopeKind::TypeParams,
            Some(parent),
            tpl.text_range(),
            name,
            false,
        );
        for tp in tpl.children() {
            if let Some(n) = ast::token(&tp, SyntaxKind::IDENT) {
                self.bind(
                    scope,
                    n.text(),
                    BindingKind::TypeParam,
                    n.text_range(),
                    tp.text_range(),
                    None,
                    None,
                );
            }
            for e in tp.children().filter_map(ast::Expr::cast) {
                self.expr(&e, scope);
            }
        }
        Some(scope)
    }

    fn func_def(&mut self, f: &ast::FuncDef, scope: ScopeId) {
        for d in f.decorators() {
            if let Some(e) = d.expr() {
                self.expr(&e, scope);
            }
        }
        if let Some(n) = f.name() {
            self.bind(
                scope,
                n.text(),
                BindingKind::Function,
                n.text_range(),
                f.text_range(),
                None,
                f.return_annotation().map(|r| r.text_range()),
            );
        }
        let name = f.name().map(|t| SmolStr::new(t.text()));
        let outer = self
            .type_params_scope(f.syntax(), scope, name.clone())
            .unwrap_or(scope);
        // Defaults and annotations are evaluated in the enclosing scope.
        if let Some(params) = f.params() {
            for p in params.params() {
                if let Some(d) = p.default() {
                    self.expr(&d, outer);
                }
                if let Some(a) = p.annotation() {
                    self.expr(&a, outer);
                }
            }
        }
        if let Some(r) = f.return_annotation() {
            self.expr(&r, outer);
        }
        let fscope = self.push_scope(
            ScopeKind::Function,
            Some(outer),
            f.text_range(),
            name,
            f.is_async(),
        );
        if let Some(params) = f.params() {
            for p in params.params() {
                let kind = match &p {
                    ast::AnyParam::Star(_) => BindingKind::StarParam,
                    ast::AnyParam::StarStar(_) => BindingKind::StarStarParam,
                    _ => BindingKind::Param,
                };
                if let Some(n) = p.name() {
                    self.bind(
                        fscope,
                        n.text(),
                        kind,
                        n.text_range(),
                        p.text_range(),
                        p.default().map(|d| d.text_range()),
                        p.annotation().map(|a| a.text_range()),
                    );
                }
            }
        }
        if let Some(body) = f.body() {
            self.deferred.push((fscope, DeferredBody::Block(body)));
        }
    }

    fn function_body(&mut self, scope: ScopeId, body: &DeferredBody) {
        match body {
            DeferredBody::Block(b) => {
                for s in b.statements() {
                    self.stmt(&s, scope);
                }
            }
            DeferredBody::Expr(e) => self.expr(e, scope),
        }
    }

    fn class_def(&mut self, c: &ast::ClassDef, scope: ScopeId) {
        for d in c.decorators() {
            if let Some(e) = d.expr() {
                self.expr(&e, scope);
            }
        }
        if let Some(n) = c.name() {
            self.bind(
                scope,
                n.text(),
                BindingKind::Class,
                n.text_range(),
                c.text_range(),
                None,
                None,
            );
        }
        let name = c.name().map(|t| SmolStr::new(t.text()));
        let outer = self
            .type_params_scope(c.syntax(), scope, name.clone())
            .unwrap_or(scope);
        if let Some(args) = c.arguments() {
            for a in args.args() {
                if let Some(v) = a.value() {
                    self.expr(&v, outer);
                }
            }
        }
        let cscope = self.push_scope(ScopeKind::Class, Some(outer), c.text_range(), name, false);
        self.block(c.body(), cscope);
    }

    // ----- targets & patterns ------------------------------------------------

    fn target(
        &mut self,
        t: &ast::Expr,
        scope: ScopeId,
        kind: BindingKind,
        node_range: TextRange,
        value: Option<TextRange>,
        annotation: Option<TextRange>,
    ) {
        match t {
            ast::Expr::Name(n) => {
                if let Some(tok) = n.name_token() {
                    self.bind(
                        scope,
                        tok.text(),
                        kind,
                        tok.text_range(),
                        node_range,
                        value,
                        annotation,
                    );
                }
            }
            ast::Expr::Tuple(tu) => {
                for e in tu.elements() {
                    self.target(&e, scope, kind.clone(), node_range, None, None);
                }
            }
            ast::Expr::List(l) => {
                for e in l.elements() {
                    self.target(&e, scope, kind.clone(), node_range, None, None);
                }
            }
            ast::Expr::Paren(p) => {
                if let Some(inner) = p.inner() {
                    self.target(&inner, scope, kind, node_range, value, annotation);
                }
            }
            ast::Expr::Star(s) => {
                if let Some(inner) = s.expr() {
                    self.target(&inner, scope, kind, node_range, None, None);
                }
            }
            // Attribute / subscript targets: evaluate the object expression.
            other => self.expr(other, scope),
        }
    }

    fn pattern(&mut self, p: &SyntaxNode, scope: ScopeId) {
        match p.kind() {
            SyntaxKind::MATCH_CAPTURE_PATTERN => {
                if let Some(t) = ast::token(p, SyntaxKind::IDENT) {
                    self.bind(
                        scope,
                        t.text(),
                        BindingKind::MatchCapture,
                        t.text_range(),
                        p.text_range(),
                        None,
                        None,
                    );
                }
            }
            SyntaxKind::MATCH_STAR_PATTERN | SyntaxKind::MATCH_MAPPING_REST => {
                if let Some(t) = ast::token(p, SyntaxKind::IDENT) {
                    self.bind(
                        scope,
                        t.text(),
                        BindingKind::MatchCapture,
                        t.text_range(),
                        p.text_range(),
                        None,
                        None,
                    );
                }
            }
            SyntaxKind::MATCH_AS_PATTERN => {
                for c in p.children() {
                    self.pattern(&c, scope);
                }
                // The `as NAME` is the last IDENT token.
                if let Some(t) = p
                    .children_with_tokens()
                    .filter_map(|e| e.into_token())
                    .filter(|t| t.kind() == SyntaxKind::IDENT)
                    .last()
                {
                    self.bind(
                        scope,
                        t.text(),
                        BindingKind::MatchCapture,
                        t.text_range(),
                        p.text_range(),
                        None,
                        None,
                    );
                }
            }
            SyntaxKind::MATCH_VALUE_PATTERN | SyntaxKind::MATCH_CLASS_PATTERN => {
                // Dotted value / class name: first IDENT is a reference.
                if let Some(t) = ast::token(p, SyntaxKind::IDENT) {
                    self.reference(scope, t.text(), t.text_range(), false);
                }
                for c in p.children() {
                    self.pattern(&c, scope);
                }
            }
            SyntaxKind::MATCH_KEYWORD_PATTERN => {
                for c in p.children() {
                    self.pattern(&c, scope);
                }
            }
            _ => {
                for c in p.children() {
                    if c.kind().is_pattern()
                        || matches!(
                            c.kind(),
                            SyntaxKind::MATCH_MAPPING_ITEM
                                | SyntaxKind::MATCH_MAPPING_REST
                                | SyntaxKind::MATCH_CLASS_ARG
                                | SyntaxKind::MATCH_KEYWORD_PATTERN
                        )
                    {
                        self.pattern(&c, scope);
                    } else if let Some(e) = ast::Expr::cast(c) {
                        self.expr(&e, scope);
                    }
                }
            }
        }
    }

    // ----- expressions ------------------------------------------------------

    fn expr(&mut self, e: &ast::Expr, scope: ScopeId) {
        match e {
            ast::Expr::Name(n) => {
                if let Some(t) = n.name_token() {
                    self.reference(scope, t.text(), t.text_range(), false);
                }
            }
            ast::Expr::Lambda(l) => {
                if let Some(params) = l.params() {
                    for p in params.params() {
                        if let Some(d) = p.default() {
                            self.expr(&d, scope);
                        }
                    }
                }
                let ls =
                    self.push_scope(ScopeKind::Lambda, Some(scope), l.text_range(), None, false);
                if let Some(params) = l.params() {
                    for p in params.params() {
                        if let Some(n) = p.name() {
                            let kind = match &p {
                                ast::AnyParam::Star(_) => BindingKind::StarParam,
                                ast::AnyParam::StarStar(_) => BindingKind::StarStarParam,
                                _ => BindingKind::Param,
                            };
                            self.bind(
                                ls,
                                n.text(),
                                kind,
                                n.text_range(),
                                p.text_range(),
                                p.default().map(|d| d.text_range()),
                                None,
                            );
                        }
                    }
                }
                if let Some(body) = l.body() {
                    self.deferred.push((ls, DeferredBody::Expr(body)));
                }
            }
            ast::Expr::ListComp(c) => self.comprehension(
                e.syntax(),
                c.element().into_iter().collect(),
                c.clauses().collect(),
                scope,
            ),
            ast::Expr::SetComp(c) => self.comprehension(
                e.syntax(),
                c.element().into_iter().collect(),
                c.clauses().collect(),
                scope,
            ),
            ast::Expr::Generator(c) => self.comprehension(
                e.syntax(),
                c.element().into_iter().collect(),
                c.clauses().collect(),
                scope,
            ),
            ast::Expr::DictComp(c) => {
                let elems: Vec<ast::Expr> = c
                    .item()
                    .into_iter()
                    .flat_map(|i| [i.key(), i.value()])
                    .flatten()
                    .collect();
                self.comprehension(e.syntax(), elems, c.clauses().collect(), scope)
            }
            ast::Expr::Named(n) => {
                if let Some(v) = n.value() {
                    self.expr(&v, scope);
                }
                if let Some(t) = n.target().and_then(|t| t.name_token()) {
                    // Walrus inside a comprehension binds in the enclosing non-comprehension scope.
                    let mut target_scope = scope;
                    while self.model.scope(target_scope).kind == ScopeKind::Comprehension {
                        match self.model.scope(target_scope).parent {
                            Some(p) => target_scope = p,
                            None => break,
                        }
                    }
                    self.bind(
                        target_scope,
                        t.text(),
                        BindingKind::Walrus,
                        t.text_range(),
                        n.text_range(),
                        n.value().map(|v| v.text_range()),
                        None,
                    );
                }
            }
            ast::Expr::Attribute(a) => {
                if let Some(o) = a.object() {
                    self.expr(&o, scope);
                }
            }
            ast::Expr::Call(c) => {
                if let Some(f) = c.callee() {
                    self.expr(&f, scope);
                }
                if let Some(args) = c.arg_list() {
                    for a in args.args() {
                        if let Some(v) = a.value() {
                            self.expr(&v, scope);
                        }
                    }
                }
            }
            ast::Expr::FString(f) => {
                for r in f.replacements() {
                    for e in r
                        .syntax()
                        .descendants()
                        .filter_map(ast::Expr::cast)
                        .filter(|x| {
                            x.syntax()
                                .parent()
                                .is_some_and(|p| p.kind() == SyntaxKind::FSTRING_REPLACEMENT)
                        })
                    {
                        self.expr(&e, scope);
                    }
                }
            }
            ast::Expr::ConcatString(c) => {
                for p in c.parts() {
                    self.expr(&p, scope);
                }
            }
            ast::Expr::String(_) | ast::Expr::Literal(_) => {}
            other => {
                for c in other.syntax().children() {
                    if let Some(x) = ast::Expr::cast(c.clone()) {
                        self.expr(&x, scope);
                    } else if matches!(
                        c.kind(),
                        SyntaxKind::DICT_ITEM
                            | SyntaxKind::DICT_UNPACK
                            | SyntaxKind::SLICE_LIST
                            | SyntaxKind::ARG_LIST
                    ) {
                        for cc in c.descendants().filter_map(ast::Expr::cast).filter(|x| {
                            x.syntax().parent().as_ref() == Some(&c)
                                || x.syntax()
                                    .parent()
                                    .is_some_and(|p| p.parent().as_ref() == Some(&c))
                        }) {
                            self.expr(&cc, scope);
                        }
                    }
                }
            }
        }
    }

    fn comprehension(
        &mut self,
        node: &SyntaxNode,
        elements: Vec<ast::Expr>,
        clauses: Vec<ast::CompFor>,
        scope: ScopeId,
    ) {
        // The first iterable is evaluated in the enclosing scope.
        if let Some(first) = clauses.first().and_then(|c| c.iter()) {
            self.expr(&first, scope);
        }
        let cs = self.push_scope(
            ScopeKind::Comprehension,
            Some(scope),
            node.text_range(),
            None,
            clauses.iter().any(|c| c.is_async()),
        );
        for (i, clause) in clauses.iter().enumerate() {
            if i > 0 {
                if let Some(it) = clause.iter() {
                    self.expr(&it, cs);
                }
            }
            if let Some(t) = clause.target() {
                self.target(
                    &t,
                    cs,
                    BindingKind::CompTarget,
                    clause.text_range(),
                    clause.iter().map(|i| i.text_range()),
                    None,
                );
            }
            for cond in clause.conditions() {
                self.expr(&cond, cs);
            }
        }
        for e in elements {
            self.expr(&e, cs);
        }
    }

    // ----- resolution --------------------------------------------------------

    fn resolve_all(&mut self) {
        let mut resolved = Vec::with_capacity(self.model.references.len());
        for r in &self.model.references {
            let ids = self.model.resolve(r.scope, &r.name);
            // Prefer the last binding that precedes the reference in the same scope; else last.
            let pick = ids.and_then(|ids| {
                let before = ids.iter().rev().find(|id| {
                    let b = self.model.binding(**id);
                    b.scope == r.scope && b.range.start() <= r.range.start() && b.is_definition()
                });
                before
                    .or_else(|| {
                        ids.iter()
                            .rev()
                            .find(|id| self.model.binding(**id).is_definition())
                    })
                    .copied()
            });
            resolved.push(pick);
        }
        for (r, id) in self.model.references.iter_mut().zip(resolved) {
            r.resolved = id;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_syntax::parse;

    fn model(src: &str) -> ScopeModel {
        build(&parse(src))
    }

    #[test]
    fn module_and_function_scopes() {
        let m = model("import os\nx = 1\n\ndef f(a, b=x):\n    y = a + x\n    return y\n\nclass C(object):\n    attr = 2\n    def m(self):\n        return attr\n");
        assert_eq!(m.scopes.len(), 4);
        let module = m.module_scope();
        assert_eq!(
            m.scope(module)
                .bindings
                .keys()
                .map(|k| k.as_str())
                .collect::<Vec<_>>(),
            vec!["os", "x", "f", "C"]
        );
        let f = m
            .scopes
            .iter()
            .position(|s| s.kind == ScopeKind::Function && s.name.as_deref() == Some("f"))
            .unwrap();
        let f = ScopeId(f as u32);
        assert_eq!(
            m.scope(f)
                .bindings
                .keys()
                .map(|k| k.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "y"]
        );
        // `x` inside f resolves to the module binding.
        let xs: Vec<&Reference> = m.references.iter().filter(|r| r.name == "x").collect();
        assert_eq!(xs.len(), 2);
        assert!(xs
            .iter()
            .all(|r| r.resolved.map(|b| m.binding(b).scope) == Some(module)));
        // `attr` inside method m does NOT see the class scope.
        let attr_ref = m.references.iter().find(|r| r.name == "attr").unwrap();
        assert!(attr_ref.resolved.is_none());
        // `object` and `self` references.
        assert!(m
            .references
            .iter()
            .any(|r| r.name == "object" && r.resolved.is_none()));
        let method = m
            .scopes
            .iter()
            .find(|s| s.name.as_deref() == Some("m"))
            .unwrap();
        assert!(method.bindings.contains_key("self"));
    }

    #[test]
    fn imports_and_globals() {
        let m = model("from . import a as b\nfrom ..p.q import c, d as e\nimport x.y\n\ndef f():\n    global g\n    g = 1\n    nonlocal_missing = 2\n");
        let names: Vec<&str> = m
            .scope(ScopeId(0))
            .bindings
            .keys()
            .map(|k| k.as_str())
            .collect();
        assert_eq!(names, vec!["b", "c", "e", "x", "f"]);
        let b = m.binding(m.bindings_in(ScopeId(0), "b")[0]);
        match &b.kind {
            BindingKind::Import(i) => {
                assert_eq!(i.level, 1);
                assert_eq!(i.name.as_deref(), Some("a"));
                assert_eq!(i.alias.as_deref(), Some("b"));
            }
            k => panic!("{k:?}"),
        }
        let x = m.binding(m.bindings_in(ScopeId(0), "x")[0]);
        assert!(matches!(&x.kind, BindingKind::Import(i) if i.module == "x.y" && i.name.is_none()));
        let f = ScopeId(1);
        assert!(m.scope(f).globals.iter().any(|g| g == "g"));
    }

    #[test]
    fn comprehension_and_walrus_scopes() {
        let m = model("data = [1]\nout = [y for x in data if (y := x * 2)]\nprint(x)\n");
        let comp = m
            .scopes
            .iter()
            .find(|s| s.kind == ScopeKind::Comprehension)
            .unwrap();
        assert_eq!(
            comp.bindings.keys().map(|k| k.as_str()).collect::<Vec<_>>(),
            vec!["x"]
        );
        // walrus target escapes to the module scope
        assert!(m.bindings_in(ScopeId(0), "y").len() == 1);
        // `data` is referenced from the module scope (first iterable), `x` in print is unresolved
        let data_ref = m.references.iter().find(|r| r.name == "data").unwrap();
        assert_eq!(data_ref.scope, ScopeId(0));
        let x_refs: Vec<&Reference> = m.references.iter().filter(|r| r.name == "x").collect();
        assert_eq!(x_refs.len(), 2);
        assert!(x_refs[0].resolved.is_some());
        assert!(x_refs[1].resolved.is_none());
    }

    #[test]
    fn match_patterns_and_type_params() {
        let m = model("match p:\n    case Point(x=0, y=yy) | [first, *rest] as whole:\n        pass\n\ndef g[T: int](v: T) -> T:\n    return v\n");
        let names: Vec<&str> = m
            .scope(ScopeId(0))
            .bindings
            .keys()
            .map(|k| k.as_str())
            .collect();
        assert_eq!(names, vec!["yy", "first", "rest", "whole", "g"]);
        let tp = m
            .scopes
            .iter()
            .find(|s| s.kind == ScopeKind::TypeParams)
            .unwrap();
        assert!(tp.bindings.contains_key("T"));
        let t_refs: Vec<&Reference> = m.references.iter().filter(|r| r.name == "T").collect();
        assert_eq!(t_refs.len(), 2);
        assert!(t_refs.iter().all(|r| r.resolved.is_some()));
    }

    #[test]
    fn scope_at_and_references_to() {
        let src = "def f():\n    v = 1\n    return v\n";
        let m = model(src);
        let inner = m.scope_at(rustpy_core::TextSize::new(
            src.find("return").unwrap() as u32
        ));
        assert_eq!(m.scope(inner).kind, ScopeKind::Function);
        let (id, _) = m
            .binding_at(rustpy_core::TextSize::new(src.find("v = 1").unwrap() as u32))
            .unwrap();
        assert_eq!(m.references_to(id).len(), 1);
    }
}
