//! Bounded terminal-call proofs. Never invokes inference or reads source files.
use super::*;
use crate::db::Analysis;
use crate::scope::BindingKind;

#[path = "flow_terminal_types.rs"]
mod declared_types;

#[path = "flow_terminal_inferred.rs"]
mod inferred_values;

/// An import edge captured with its lexical relative-import context.
#[derive(Clone)]
struct ImportDependency {
    from: Arc<FileInfo>,
    level: usize,
    module: String,
    target: Option<Arc<FileInfo>>,
}

#[derive(Clone, Default)]
pub(super) struct Authority {
    modules: FxHashSet<String>,
    dependencies: Vec<(String, Option<Arc<FileInfo>>)>,
    retired_members: FxHashSet<String>,
    project_imports: FxHashMap<(rustpy_core::FileId, BindingId), Arc<FileInfo>>,
    project_dependencies: Vec<ImportDependency>,
}
impl Authority {
    pub(super) fn canonical_match_literal(&self, module: &str) -> bool {
        self.modules.contains(module) && !self.retired_members.contains("Literal")
    }

    pub(super) fn enum_match_coverage(
        &self,
        file: &FileInfo,
        statement: &ast::MatchStmt,
    ) -> Option<Vec<bool>> {
        crate::match_support::finite_enum_coverage(file, statement, |from, binding| {
            self.project_imports.get(&(from.id, binding)).cloned()
        })
    }

    /// Conservative retained-source weight for the per-Inferer cache.
    /// Shared Arcs are counted once within a capture and again across entries,
    /// so the aggregate cache limit cannot undercount shared dependencies.
    pub(super) fn retained_bytes(&self, root: &FileInfo) -> usize {
        let mut seen = FxHashSet::default();
        seen.insert(root.id);
        let mut bytes = root.text.len();
        for file in self
            .dependencies
            .iter()
            .filter_map(|(_, file)| file.as_ref())
            .chain(self.project_dependencies.iter().flat_map(|dependency| {
                std::iter::once(&dependency.from).chain(dependency.target.as_ref())
            }))
            .chain(self.project_imports.values())
        {
            if seen.insert(file.id) {
                bytes = bytes.saturating_add(file.text.len());
            }
        }
        bytes
    }

    pub(super) fn is_current(&self, db: &Analysis) -> bool {
        self.project_dependencies.iter().all(|dependency| {
            match (
                &dependency.target,
                db.module_relative(&dependency.from, dependency.level, &dependency.module),
            ) {
                (Some(old), Some(now)) => Arc::ptr_eq(old, &now),
                (None, None) => true,
                _ => false,
            }
        }) && self
            .dependencies
            .iter()
            .all(|(name, captured)| match (captured, db.module(name)) {
                (Some(captured), Some(current)) => Arc::ptr_eq(captured, &current),
                (None, None) => true,
                _ => false,
            })
    }

    pub(super) fn captured(db: &Analysis, file: &FileInfo) -> Self {
        let mut result = Self::default();
        let required: FxHashSet<_> = file
            .scopes
            .bindings
            .iter()
            .filter_map(|binding| {
                let BindingKind::Import(import) = &binding.kind else {
                    return None;
                };
                (import.level == 0
                    && matches!(
                        import.module.as_str(),
                        "sys" | "os" | "typing" | "typing_extensions" | "builtins"
                    ))
                .then_some(import.module.as_str())
            })
            .chain(std::iter::once("builtins"))
            .collect();
        for module in required {
            let captured = db.module(module);
            result
                .dependencies
                .push((module.to_owned(), captured.clone()));
            if captured.is_some_and(|file| {
                file.is_stub
                    && file.path == std::path::Path::new(&format!("<typeshed>/{module}.pyi"))
                    && file.module_name.as_deref() == Some(module)
            }) {
                result.modules.insert(module.to_owned());
            }
        }
        // A normal attribute store can replace a module member even through
        // an alias. Conservatively retire that spelling throughout this file;
        // do not try to execute or infer alias expressions during the proof.
        for statement in file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::Stmt::cast)
        {
            let targets: Vec<_> = match statement {
                ast::Stmt::Assign(assign) => assign.targets(),
                ast::Stmt::AugAssign(assign) => assign.target().into_iter().collect(),
                ast::Stmt::AnnAssign(assign) if assign.value().is_some() => {
                    assign.target().into_iter().collect()
                }
                ast::Stmt::Del(delete) => delete.targets(),
                _ => Vec::new(),
            };
            for target in targets {
                for node in target
                    .syntax()
                    .descendants()
                    .filter_map(ast::AttributeExpr::cast)
                {
                    result.retired_members.insert(node.attr_text());
                }
            }
        }
        // Bounded declaration dependency catalogue. Never infer a call or
        // traverse function bodies. Standard stub dependencies stop here;
        // project imports may lead to a declared function or a re-export.
        let mut pending = db.file(file.id).into_iter().collect::<Vec<_>>();
        let mut seen = FxHashSet::default();
        let mut admitted = FxHashSet::default();
        admitted.insert(file.id);
        let mut bytes = file.text.len();
        while let Some(from) = pending.pop() {
            if !seen.insert(from.id) || seen.len() > 32 {
                continue;
            }
            if bytes > 4 * 1024 * 1024 {
                break;
            }
            for statement in from
                .parse
                .syntax_node()
                .descendants()
                .filter_map(ast::Stmt::cast)
            {
                let targets = match statement {
                    ast::Stmt::Assign(assign) => assign.targets(),
                    ast::Stmt::AugAssign(assign) => assign.target().into_iter().collect(),
                    ast::Stmt::AnnAssign(assign) if assign.value().is_some() => {
                        assign.target().into_iter().collect()
                    }
                    ast::Stmt::Del(delete) => delete.targets(),
                    _ => Vec::new(),
                };
                for target in targets {
                    for attribute in target
                        .syntax()
                        .descendants()
                        .filter_map(ast::AttributeExpr::cast)
                    {
                        result.retired_members.insert(attribute.attr_text());
                    }
                }
            }
            for (index, binding) in from.scopes.bindings.iter().enumerate() {
                let BindingKind::Import(import) = &binding.kind else {
                    continue;
                };
                if result.project_dependencies.len() >= 128 {
                    break;
                }
                // Without an alias, import a.b binds a, not a.b.
                if import.name.is_none() && import.alias.is_none() && import.module.contains('.') {
                    continue;
                }
                let name = import.module.trim_start_matches('.').to_owned();
                let captured = db.module_relative(&from, import.level, &name);
                if let Some(target) = &captured {
                    if !admitted.contains(&target.id) {
                        if admitted.len() >= 32
                            || bytes.saturating_add(target.text.len()) > 4 * 1024 * 1024
                        {
                            continue;
                        }
                        admitted.insert(target.id);
                        bytes += target.text.len();
                    }
                }
                result.project_dependencies.push(ImportDependency {
                    from: from.clone(),
                    level: import.level,
                    module: name.clone(),
                    target: captured.clone(),
                });
                if let Some(target) = captured {
                    if target.is_stub
                        && target.path == std::path::Path::new(&format!("<typeshed>/{name}.pyi"))
                        && matches!(
                            name.as_str(),
                            "sys" | "os" | "typing" | "typing_extensions" | "builtins"
                        )
                    {
                        result.modules.insert(name);
                    }
                    result
                        .project_imports
                        .insert((from.id, BindingId(index as u32)), target.clone());
                    if !target.path.starts_with("<typeshed>") {
                        pending.push(target);
                    }
                }
            }
        }
        result
    }

    fn imported(
        &self,
        binding: &crate::scope::Binding,
        member: Option<&str>,
        annotation: bool,
    ) -> bool {
        let BindingKind::Import(import) = &binding.kind else {
            return false;
        };
        if import.level != 0 || !self.modules.contains(import.module.as_str()) {
            return false;
        }
        let name = match (member, import.name.as_deref()) {
            (Some(name), None) => name,
            (None, Some(name)) => name,
            _ => return false,
        };
        if self.retired_members.contains(name) {
            return false;
        }
        if annotation {
            matches!(import.module.as_str(), "typing" | "typing_extensions")
                && matches!(name, "NoReturn" | "Never")
        } else {
            match import.module.as_str() {
                "sys" => name == "exit",
                "os" => matches!(name, "_exit" | "abort"),
                "typing" | "typing_extensions" => name == "assert_never",
                _ => false,
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Contract {
    Sync,
    Async,
}
impl Contract {
    fn terminates(self, awaited: bool) -> bool {
        matches!(self, Self::Sync) || awaited
    }
}

impl Walker<'_> {
    fn terminal_name(&self, name: &str, st: &State, member: Option<&str>, awaited: bool) -> bool {
        if self.shadowed.iter().any(|names| names.contains(name)) {
            return false;
        }
        // Class LOAD_NAME can find the actual builtin before a later own
        // namespace store. Only the parent's role-aware proof admits this.
        if member.is_none() && self.known_builtin_name(name, st) {
            return true;
        }
        let proof = |id| {
            self.contract(self.file, id, member, &mut FxHashSet::default())
                .is_some_and(|contract| contract.terminates(awaited))
        };
        // Authoritative path lookup includes class namespaces and captured
        // global/nonlocal cells. Unknown or definitely unbound results must
        // never fall back to a lexical definition from another path/role.
        if let Some(reaches) = self.reaching_name(name, st) {
            return !reaches.is_empty()
                && reaches
                    .iter()
                    .all(|reach| reach.preds.is_empty() && reach.binding.is_some_and(proof));
        }
        if let Some(ids) = self.file.scopes.resolve(self.scope, name) {
            return !ids.is_empty() && ids.iter().all(|id| proof(*id));
        }
        let mut current = Some(self.scope);
        while let Some(scope) = current {
            let scope = self.file.scopes.scope(scope);
            if !scope.star_imports.is_empty() {
                return false;
            }
            current = scope.parent;
        }
        member.is_none() && matches!(name, "exit" | "quit")
    }

    fn contract(
        &self,
        file: &FileInfo,
        id: BindingId,
        member: Option<&str>,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
    ) -> Option<Contract> {
        if seen.len() >= 16 || !seen.insert((file.id, id)) {
            return None;
        }
        let result = self.contract_inner(file, id, member, seen);
        seen.remove(&(file.id, id));
        result
    }

    fn contract_inner(
        &self,
        file: &FileInfo,
        id: BindingId,
        member: Option<&str>,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
    ) -> Option<Contract> {
        let binding = file.scopes.binding(id);
        if !file.scopes.scope(binding.scope).star_imports.is_empty() {
            return None;
        }
        let root = file.parse.syntax_node();
        let node = match root.covering_element(binding.range) {
            rustpy_syntax::rowan::NodeOrToken::Node(node) => node,
            rustpy_syntax::rowan::NodeOrToken::Token(token) => token.parent()?,
        };
        // Conditional binding alone does not prove existence on every path.
        if node
            .ancestors()
            .take_while(|node| node.text_range() != file.scopes.scope(binding.scope).range)
            .any(|node| {
                matches!(
                    ast::Stmt::cast(node),
                    Some(
                        ast::Stmt::If(_)
                            | ast::Stmt::For(_)
                            | ast::Stmt::While(_)
                            | ast::Stmt::Try(_)
                            | ast::Stmt::Match(_)
                            | ast::Stmt::With(_)
                    )
                )
            })
        {
            return None;
        }
        if self.authority.imported(binding, member, false) {
            return Some(Contract::Sync);
        }
        if let Some(member) = member {
            if let Some(contract) = self.declared_member_contract(file, id, member) {
                return Some(contract);
            }
        }
        if let BindingKind::Import(import) = &binding.kind {
            let name = match (member, import.name.as_deref()) {
                (Some(name), None) | (None, Some(name)) => name,
                _ => return None,
            };
            if self.authority.retired_members.contains(name) {
                return None;
            }
            let target = self.authority.project_imports.get(&(file.id, id))?;
            let ids = target.scopes.resolve(target.scopes.module_scope(), name)?;
            return self.same_contract(target, ids, None, seen);
        }
        if member.is_none() {
            if let Some(contract) = self.declared_parameter_contract(file, id) {
                return Some(contract);
            }
            if matches!(binding.kind, BindingKind::Class) {
                return self.declared_constructor_contract(file, id, seen);
            }
        }
        if matches!(binding.kind, BindingKind::Assignment) {
            let expression = self.expression_at(file, binding.value?)?;
            if let ast::Expr::Call(call) = &expression {
                if member.is_none() {
                    return self.declared_instance_contract(file, id, call, seen);
                }
                return None;
            }
            let (name, attribute) = match expression {
                ast::Expr::Name(name) => (name, None),
                ast::Expr::Attribute(attribute) => {
                    let ast::Expr::Name(name) = attribute.object()?.unparen() else {
                        return None;
                    };
                    (name, Some(attribute.attr_text()))
                }
                _ => return None,
            };
            if member.is_some() && attribute.is_some() {
                return None;
            }
            let member = member.or(attribute.as_deref());
            let start = name.name_token()?.text_range().start();
            // Current-scope RHS references retain the binding at assignment
            // time, so rebinding the original function later cannot retarget
            // its already-captured callable alias.
            if file.id == self.file.id {
                // Captured builtin identity is a source-point fact, not a
                // spelling guess. A competing recorded lookup retires it.
                if member.is_none()
                    && self.info.builtin_refs.contains(&start)
                    && !self.info.refs.contains_key(&start)
                {
                    return Some(Contract::Sync);
                }
                if let Some(reaches) = self.info.refs.get(&start) {
                    if reaches.is_empty()
                        || reaches
                            .iter()
                            .any(|r| r.binding.is_none() || !r.preds.is_empty())
                    {
                        return None;
                    }
                    let ids: Vec<_> = reaches.iter().filter_map(|r| r.binding).collect();
                    return self.same_contract(file, &ids, member, seen);
                }
            }
            // Builtin aliases are a fallback only when no authoritative
            // captured RHS lookup exists (including custom class namespaces).
            if member.is_none()
                && matches!(name.name_text().as_str(), "exit" | "quit")
                && file
                    .scopes
                    .resolve(binding.scope, &name.name_text())
                    .is_none()
                && file
                    .scopes
                    .scopes
                    .iter()
                    .all(|scope| scope.star_imports.is_empty())
            {
                return Some(Contract::Sync);
            }
            // Outside this walk require a single lexical definition; no
            // execution-order or branch guess across a module boundary.
            let ids = file.scopes.resolve(binding.scope, &name.name_text())?;
            if ids.len() != 1 {
                return None;
            }
            let target = file.scopes.binding(ids[0]);
            if target.range.start() >= binding.range.start() {
                return None;
            }
            return self.same_contract(file, ids, member, seen);
        }
        if member.is_some() || !matches!(binding.kind, BindingKind::Function) {
            return None;
        }
        let function = node_at(&root, binding.node_range, SyntaxKind::FUNC_DEF)
            .and_then(ast::FuncDef::cast)?;
        if let Some(contract) = self.declared_overload_contract(file, id) {
            return contract;
        }
        if function.decorators().next().is_some()
            || function.syntax().descendants().any(|node| {
                node.kind() == SyntaxKind::YIELD_EXPR
                    && node
                        .ancestors()
                        .find(|owner| {
                            matches!(
                                owner.kind(),
                                SyntaxKind::FUNC_DEF
                                    | SyntaxKind::LAMBDA_EXPR
                                    | SyntaxKind::CLASS_DEF
                            )
                        })
                        .is_some_and(|owner| owner.text_range() == function.syntax().text_range())
            })
        {
            // Calling a generator creates an object; its body and declared
            // return annotation cannot prove invocation-time termination.
            return None;
        }
        let annotation = function.return_annotation()?.unparen();
        if !function.is_async() {
            if let Some(contract) = self.awaitable_return_contract(file, binding.scope, &annotation)
            {
                return Some(contract);
            }
        }
        let (name, member) = match annotation {
            ast::Expr::Name(name) => (name.name_text(), None),
            ast::Expr::Attribute(attr) => {
                let ast::Expr::Name(name) = attr.object()?.unparen() else {
                    return None;
                };
                (name.name_text(), Some(attr.attr_text()))
            }
            ast::Expr::String(string) => {
                let rustpy_syntax::string_literal::Decoded::Text(text, _) =
                    rustpy_syntax::string_literal::string(&string.syntax().text().to_string())
                        .ok()?
                else {
                    return None;
                };
                match text.split_once('.') {
                    Some((name, member)) if !member.contains('.') => {
                        (name.to_owned(), Some(member.to_owned()))
                    }
                    None => (text, None),
                    _ => return None,
                }
            }
            _ => return None,
        };
        let ids = file.scopes.resolve(binding.scope, &name)?;
        if ids.is_empty()
            || !ids.iter().all(|id| {
                self.authority
                    .imported(file.scopes.binding(*id), member.as_deref(), true)
            })
        {
            return None;
        }
        Some(if function.is_async() {
            Contract::Async
        } else {
            Contract::Sync
        })
    }

    fn same_contract(
        &self,
        file: &FileInfo,
        ids: &[BindingId],
        member: Option<&str>,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
    ) -> Option<Contract> {
        let mut answer = None;
        for id in ids {
            let contract = self.contract(file, *id, member, seen)?;
            if answer.is_some_and(|old: Contract| {
                matches!(old, Contract::Async) != matches!(contract, Contract::Async)
            }) {
                return None;
            }
            answer = Some(contract);
        }
        answer
    }

    /// Captured at context acquisition, before target stores/body mutations.
    /// No implicit Call node or terminal callpoint is fabricated.
    pub(super) fn context_contract(
        &self,
        context: &ast::Expr,
        state: &State,
        async_with: bool,
    ) -> ContextContract {
        self.context_declared_contract(context, state, async_with)
    }

    pub(super) fn is_terminal_call(&self, expr: &ast::Expr, st: &State) -> bool {
        let (expression, awaited) = match expr.clone().unparen() {
            ast::Expr::Await(expression) => {
                let Some(expression) = expression.expr() else {
                    return false;
                };
                (expression.unparen(), true)
            }
            expression => (expression, false),
        };
        let ast::Expr::Call(call) = expression else {
            return false;
        };
        let Some(callee) = call.callee().map(ast::Expr::unparen) else {
            return false;
        };
        self.terminal_callable(&callee, st, awaited)
    }

    /// Pure callable proof at the captured decorator-expression state.
    /// No synthetic Call AST or terminal-call range is created or published.
    pub(super) fn is_terminal_callable(&self, callee: &ast::Expr, st: &State) -> bool {
        self.terminal_callable(callee, st, false)
    }

    fn terminal_indexed_callable(
        &self,
        subscript: &ast::SubscriptExpr,
        st: &State,
        awaited: bool,
    ) -> bool {
        let Some(ast::Expr::Name(name)) = subscript.object().map(ast::Expr::unparen) else {
            return false;
        };
        let name = name.name_text();
        if self
            .shadowed
            .iter()
            .any(|names| names.contains(name.as_str()))
        {
            return false;
        }
        let Some(index) = subscript.index_expr().map(ast::Expr::unparen) else {
            return false;
        };
        let (literal, negative) = match index {
            ast::Expr::Literal(literal) => (literal, false),
            ast::Expr::Unary(unary)
                if unary.op().is_some_and(|operator| {
                    matches!(operator.kind(), SyntaxKind::MINUS | SyntaxKind::PLUS)
                }) =>
            {
                let Some(ast::Expr::Literal(literal)) = unary.operand().map(ast::Expr::unparen)
                else {
                    return false;
                };
                (
                    literal,
                    unary
                        .op()
                        .is_some_and(|operator| operator.kind() == SyntaxKind::MINUS),
                )
            }
            _ => return false,
        };
        if literal.kind() != Some(SyntaxKind::INT_NUMBER) {
            return false;
        }
        // Same bounded integer spellings as infer::parse_int, without live inference.
        let text = literal.syntax().text().to_string();
        if text.len() > 32 {
            return false;
        }
        let digits = text.replace('_', "").to_ascii_lowercase();
        let parsed = if let Some(value) = digits.strip_prefix("0x") {
            i64::from_str_radix(value, 16)
        } else if let Some(value) = digits.strip_prefix("0o") {
            i64::from_str_radix(value, 8)
        } else if let Some(value) = digits.strip_prefix("0b") {
            i64::from_str_radix(value, 2)
        } else {
            digits.parse::<i64>()
        };
        let Ok(mut index) = parsed else {
            return false;
        };
        if negative {
            let Some(value) = index.checked_neg() else {
                return false;
            };
            index = value;
        }
        let proof = |id| {
            self.declared_indexed_parameter_contract(self.file, id, index)
                .is_some_and(|contract| contract.terminates(awaited))
        };
        if let Some(reaches) = self.reaching_name(&name, st) {
            return !reaches.is_empty()
                && reaches.len() <= 16
                && reaches
                    .iter()
                    .all(|reach| reach.preds.is_empty() && reach.binding.is_some_and(proof));
        }
        self.file
            .scopes
            .resolve(self.scope, &name)
            .is_some_and(|ids| {
                !ids.is_empty() && ids.len() <= 16 && ids.iter().all(|id| proof(*id))
            })
    }

    pub(super) fn acquired_call_modes(&self, call: &ast::CallExpr, state: &State) -> [bool; 2] {
        let Some(callee) = call.callee() else {
            return [false; 2];
        };
        let mut modes = [
            self.is_terminal_call(&ast::Expr::Call(call.clone()), state),
            self.terminal_callable(&callee, state, true),
        ];
        if !modes[1] {
            let selected = self.selected_overload_modes(call, state);
            modes[0] |= selected[0];
            modes[1] |= selected[1];
        }
        modes
    }
    pub(super) fn capture_factory_result(&mut self, call: &ast::CallExpr, state: &State) {
        let mut modes = [[false; 2]; 2];
        let receiver = match call.callee().map(ast::Expr::unparen) {
            Some(ast::Expr::Name(name)) => Some((name, None)),
            Some(ast::Expr::Attribute(attribute)) => {
                match attribute.object().map(ast::Expr::unparen) {
                    Some(ast::Expr::Name(name)) => Some((name, Some(attribute.attr_text()))),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some((name, member)) = receiver {
            let name = name.name_text();
            if !self
                .shadowed
                .iter()
                .any(|names| names.contains(name.as_str()))
            {
                let ids = if let Some(reaches) = self.reaching_name(&name, state) {
                    if reaches.is_empty()
                        || reaches.len() > 16
                        || reaches
                            .iter()
                            .any(|reach| reach.binding.is_none() || !reach.preds.is_empty())
                    {
                        Vec::new()
                    } else {
                        reaches.iter().filter_map(|reach| reach.binding).collect()
                    }
                } else {
                    self.file
                        .scopes
                        .resolve(self.scope, &name)
                        .map(|ids| ids.to_vec())
                        .unwrap_or_default()
                };
                if !ids.is_empty() && ids.len() <= 16 {
                    for (factory_await, row) in modes.iter_mut().enumerate() {
                        for (callback_await, terminal) in row.iter_mut().enumerate() {
                            *terminal = ids.iter().all(|id| {
                                let contract = match member.as_deref() {
                                    Some(member) => self.declared_member_factory_result_contract(
                                        self.file,
                                        *id,
                                        member,
                                        factory_await != 0,
                                    ),
                                    None => self.declared_factory_result_contract(
                                        self.file,
                                        *id,
                                        factory_await != 0,
                                    ),
                                };
                                contract.is_some_and(|contract| {
                                    contract.terminates(callback_await != 0)
                                })
                            });
                        }
                    }
                }
            }
        }
        for (factory_await, row) in modes.iter_mut().enumerate() {
            for (callback_await, terminal) in row.iter_mut().enumerate() {
                if !*terminal {
                    *terminal = self.inferred_factory_terminal(
                        call,
                        state,
                        factory_await != 0,
                        callback_await != 0,
                    );
                }
            }
        }
        // Overwrite on every visit, including private quiet-finally evaluation;
        // this is a value capture for the immediate outer evaluation, not an
        // aggregate terminal fact retained from another incoming path.
        self.factory_results.insert(call.text_range(), modes);
    }
    fn captured_factory_terminal(&self, expression: &ast::Expr, callback_await: bool) -> bool {
        let (call, factory_await) = match expression.clone().unparen() {
            ast::Expr::Call(call) => (call, false),
            ast::Expr::Await(awaited) => {
                let Some(ast::Expr::Call(call)) = awaited.expr().map(ast::Expr::unparen) else {
                    return false;
                };
                (call, true)
            }
            _ => return false,
        };
        self.factory_results
            .get(&call.text_range())
            .is_some_and(|modes| modes[usize::from(factory_await)][usize::from(callback_await)])
    }

    // Conditional callable proof never evaluates a condition or invokes inference.
    // Only simple conditions and stable declaration/member branches are admitted;
    // arguments are still evaluated after acquired_call_modes captures this proof.
    fn terminal_conditional_callable(
        &self,
        conditional: &ast::ConditionalExpr,
        state: &State,
        awaited: bool,
        depth: usize,
    ) -> bool {
        if depth >= 16 {
            return false;
        }
        let Some(condition) = conditional.condition().map(ast::Expr::unparen) else {
            return false;
        };
        if !matches!(condition, ast::Expr::Name(_) | ast::Expr::Literal(_)) {
            return false;
        }
        let branch = |expression: ast::Expr| match expression.unparen() {
            ast::Expr::Conditional(nested) => {
                self.terminal_conditional_callable(&nested, state, awaited, depth + 1)
            }
            expression @ (ast::Expr::Name(_) | ast::Expr::Attribute(_)) => {
                // Attribute objects must themselves be names: no property/factory
                // evaluation can replace the captured callee between branches.
                if let ast::Expr::Attribute(attribute) = &expression {
                    if !matches!(
                        attribute.object().map(ast::Expr::unparen),
                        Some(ast::Expr::Name(_))
                    ) {
                        return false;
                    }
                }
                if !self.conditional_branch_declaration_stable(&expression) {
                    return false;
                }
                self.terminal_callable(&expression, state, awaited)
            }
            _ => false,
        };
        conditional.true_expr().is_some_and(branch) && conditional.false_expr().is_some_and(branch)
    }

    // Even a bare condition Name can invoke __bool__/__len__. Require source
    // authority that cannot be replaced by any visible assignment/nonlocal
    // writer, rather than treating the condition's spelling as side-effect free.
    fn conditional_branch_declaration_stable(&self, expression: &ast::Expr) -> bool {
        let (name, member) = match expression {
            ast::Expr::Name(name) => (name.name_text(), None),
            ast::Expr::Attribute(attribute) => {
                let Some(ast::Expr::Name(name)) = attribute.object().map(ast::Expr::unparen) else {
                    return false;
                };
                (name.name_text(), Some(attribute.attr_text()))
            }
            _ => return false,
        };
        if member
            .as_ref()
            .is_some_and(|member| self.authority.retired_members.contains(member))
        {
            return false;
        }
        let mut found = false;
        for binding in self
            .file
            .scopes
            .bindings
            .iter()
            .filter(|binding| binding.name == name.as_str())
        {
            found = true;
            if !matches!(binding.kind, BindingKind::Function | BindingKind::Import(_)) {
                return false;
            }
            // An object's class/function constructor is not a stable attribute
            // receiver; module imports retain the existing member retirement guard.
            if member.is_some() && !matches!(binding.kind, BindingKind::Import(_)) {
                return false;
            }
        }
        found
    }

    fn terminal_callable(&self, callee: &ast::Expr, st: &State, awaited: bool) -> bool {
        match callee.clone().unparen() {
            expression @ (ast::Expr::Call(_) | ast::Expr::Await(_)) => {
                self.captured_factory_terminal(&expression, awaited)
            }
            ast::Expr::Subscript(subscript) => {
                self.terminal_indexed_callable(&subscript, st, awaited)
                    || self.inferred_indexed_terminal(&subscript, st, awaited)
            }
            ast::Expr::Binary(binary) => self.operator_callable_terminal(&binary, st, awaited),
            ast::Expr::Lambda(lambda) => self.inferred_lambda_terminal(&lambda, st, awaited),
            ast::Expr::BoolOp(boolean) => self.inferred_boolean_terminal(&boolean, st, awaited, 0),
            ast::Expr::Conditional(conditional) => {
                self.terminal_conditional_callable(&conditional, st, awaited, 0)
            }
            ast::Expr::Named(named) => named.value().is_some_and(|value| {
                self.conditional_branch_declaration_stable(&value.clone().unparen())
                    && self.terminal_callable(&value, st, awaited)
            }),
            ast::Expr::Name(name) => self.terminal_name(&name.name_text(), st, None, awaited),
            ast::Expr::Attribute(attr) => match attr.object().map(ast::Expr::unparen) {
                Some(ast::Expr::Name(name)) => {
                    self.terminal_name(&name.name_text(), st, Some(&attr.attr_text()), awaited)
                        || self
                            .property_callable_contract(&attr, st)
                            .is_some_and(|contract| contract.terminates(awaited))
                }
                Some(ast::Expr::Call(call)) => self
                    .inline_receiver_member_contract(&call, &attr.attr_text(), st)
                    .or_else(|| self.compound_receiver_contract(&attr, st))
                    .is_some_and(|contract| contract.terminates(awaited)),
                Some(ast::Expr::Attribute(_)) => self
                    .compound_receiver_contract(&attr, st)
                    .is_some_and(|contract| contract.terminates(awaited)),
                _ => false,
            },
            _ => false,
        }
    }
}
