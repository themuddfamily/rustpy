//! Bounded declared callable contracts; no inference, VM or body execution.
use super::*;

#[path = "flow_terminal_protocols.rs"]
mod standard_protocols;

#[path = "flow_terminal_implicit.rs"]
mod implicit_slots;

#[path = "flow_terminal_await_values.rs"]
mod await_values;

#[path = "flow_terminal_operators.rs"]
mod operator_values;

#[path = "flow_terminal_compound.rs"]
mod compound_receivers;

#[path = "flow_terminal_overloads.rs"]
mod selected_overloads;

impl Walker<'_> {
    // Canonical registry decorator plus at most one intrinsic descriptor.
    // Both stack orders are safe; unknown/duplicate/mixed descriptors are not.
    fn overload_metadata(
        &self,
        file: &FileInfo,
        function: &ast::FuncDef,
        scope: ScopeId,
    ) -> Option<(bool, Option<String>)> {
        let decorators: Vec<_> = function.decorators().take(3).collect();
        if decorators.len() > 2 {
            return None;
        }
        let mut overloaded = false;
        let mut descriptor = None;
        for decorator in decorators {
            let expression = decorator.expr()?;
            if self.canonical_type_symbol(file, scope, &expression, &["overload"]) {
                if overloaded {
                    return None;
                }
                overloaded = true;
            } else {
                let current = self.builtin_method_descriptor(file, scope, expression)?;
                if descriptor.is_some() {
                    return None;
                }
                descriptor = Some(current);
            }
        }
        Some((overloaded, descriptor))
    }

    fn overload_declaration(
        &self,
        file: &FileInfo,
        function: &ast::FuncDef,
        scope: ScopeId,
    ) -> bool {
        self.overload_metadata(file, function, scope)
            .is_some_and(|(overloaded, _)| overloaded)
    }

    fn overloaded_class_descriptor(&self, file: &FileInfo, id: BindingId) -> bool {
        let binding = file.scopes.binding(id);
        node_at(
            &file.parse.syntax_node(),
            binding.node_range,
            SyntaxKind::FUNC_DEF,
        )
        .and_then(ast::FuncDef::cast)
        .and_then(|function| self.overload_metadata(file, &function, binding.scope))
        .is_some_and(|(overloaded, descriptor)| overloaded && descriptor.is_some())
    }

    // Outer None: not an overload group. Some(None): overload authority exists
    // but its complete contract is unknown; do not fall through to one declaration.
    pub(super) fn declared_overload_contract(
        &self,
        file: &FileInfo,
        id: BindingId,
    ) -> Option<Option<Contract>> {
        self.overload_group_contract(file, id, false)
    }

    fn overload_group_contract(
        &self,
        file: &FileInfo,
        id: BindingId,
        allow_instance_method: bool,
    ) -> Option<Option<Contract>> {
        let binding = file.scopes.binding(id);
        let ids = file.scopes.bindings_in(binding.scope, &binding.name);
        if ids.len() <= 1 {
            return None;
        }
        if ids.len() > 32 {
            return Some(None);
        }
        let root = file.parse.syntax_node();
        let functions: Vec<_> = ids
            .iter()
            .filter_map(|id| {
                let candidate = file.scopes.binding(*id);
                if !matches!(candidate.kind, BindingKind::Function) {
                    return None;
                }
                node_at(&root, candidate.node_range, SyntaxKind::FUNC_DEF)
                    .and_then(ast::FuncDef::cast)
                    .map(|function| (*id, function))
            })
            .collect();
        if !functions.iter().any(|(id, function)| {
            self.overload_declaration(file, function, file.scopes.binding(*id).scope)
        }) {
            return None;
        }
        Some((|| {
            if functions.len() != ids.len() {
                return None;
            }
            if file.scopes.scope(binding.scope).kind == crate::scope::ScopeKind::Class {
                if !allow_instance_method {
                    return None;
                }
                let class = node_at(
                    &root,
                    file.scopes.scope(binding.scope).range,
                    SyntaxKind::CLASS_DEF,
                )
                .and_then(ast::ClassDef::cast)?;
                self.class_shape(file, &class)?;
            }
            let mut answer = None;
            let descriptor = self
                .overload_metadata(file, &functions.first()?.1, binding.scope)?
                .1;
            if descriptor.is_some()
                && file.scopes.scope(binding.scope).kind != crate::scope::ScopeKind::Class
            {
                return None;
            }
            let asynchronous = functions.first()?.1.is_async();
            let mut declarations = 0;
            let mut implementation = false;
            let container = functions.first()?.1.syntax().parent()?.text_range();
            let mut previous_end = None;
            for (index, (id, function)) in functions.iter().enumerate() {
                if function.syntax().parent()?.text_range() != container
                    || previous_end.is_some_and(|end| end > function.text_range().start())
                {
                    return None;
                }
                previous_end = Some(function.text_range().end());
                // Generators return their generator object at invocation time,
                // even when a declaration promises Never.
                if function.syntax().descendants().any(|node| {
                    matches!(
                        node.kind(),
                        SyntaxKind::YIELD_EXPR | SyntaxKind::YIELD_FROM_EXPR
                    ) && node
                        .ancestors()
                        .find(|owner| {
                            matches!(
                                owner.kind(),
                                SyntaxKind::FUNC_DEF
                                    | SyntaxKind::LAMBDA_EXPR
                                    | SyntaxKind::CLASS_DEF
                            )
                        })
                        .is_some_and(|owner| owner.text_range() == function.text_range())
                }) {
                    return None;
                }
                if function.is_async() != asynchronous {
                    return None;
                }
                let scope = file.scopes.binding(*id).scope;
                let (overloaded, current_descriptor) =
                    self.overload_metadata(file, function, scope)?;
                if current_descriptor != descriptor {
                    return None;
                }
                if overloaded {
                    if implementation {
                        return None;
                    }
                    declarations += 1;
                    let annotation = function.return_annotation()?.unparen();
                    let contract = if self.canonical_type_symbol(
                        file,
                        scope,
                        &annotation,
                        &["Never", "NoReturn"],
                    ) {
                        if function.is_async() {
                            Contract::Async
                        } else {
                            Contract::Sync
                        }
                    } else if !function.is_async() {
                        self.awaitable_return_contract(file, scope, &annotation)?
                    } else {
                        return None;
                    };
                    if answer.is_some_and(|previous| {
                        !matches!(
                            (previous, contract),
                            (Contract::Sync, Contract::Sync) | (Contract::Async, Contract::Async)
                        )
                    }) {
                        return None;
                    }
                    answer = Some(contract);
                } else {
                    if implementation || index + 1 != functions.len() {
                        return None;
                    }
                    implementation = true;
                }
            }
            (declarations >= 2 && (file.is_stub || implementation)).then_some(answer?)
        })())
    }

    /// Contract of the callable returned by an invocation, never proof that
    /// the factory itself terminates. The leaf captures this before arguments.
    pub(super) fn declared_factory_result_contract(
        &self,
        file: &FileInfo,
        id: BindingId,
        awaited_factory: bool,
    ) -> Option<Contract> {
        self.factory_binding_result(file, id, None, awaited_factory, &mut FxHashSet::default())
    }

    fn factory_binding_result(
        &self,
        file: &FileInfo,
        id: BindingId,
        member: Option<&str>,
        awaited: bool,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
    ) -> Option<Contract> {
        if seen.len() >= 16 || !seen.insert((file.id, id)) {
            return None;
        }
        let binding = file.scopes.binding(id);
        if !file.scopes.scope(binding.scope).star_imports.is_empty() {
            return None;
        }
        let root = file.parse.syntax_node();
        let node = match root.covering_element(binding.range) {
            rustpy_syntax::rowan::NodeOrToken::Node(node) => node,
            rustpy_syntax::rowan::NodeOrToken::Token(token) => token.parent()?,
        };
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
        if let BindingKind::Import(import) = &binding.kind {
            let name = match (member, import.name.as_deref()) {
                (Some(name), None) | (None, Some(name)) => name,
                (Some(member), Some(_)) => {
                    return self.class_factory_result(file, id, member, awaited, seen)
                }
                _ => return None,
            };
            if self.authority.retired_members.contains(name) {
                return None;
            }
            let target = self.authority.project_imports.get(&(file.id, id))?;
            let ids = target.scopes.resolve(target.scopes.module_scope(), name)?;
            if ids.len() != 1 {
                return None;
            }
            return self.factory_binding_result(target, ids[0], None, awaited, seen);
        }
        if let Some(member) = member {
            let constructed = matches!(binding.kind, BindingKind::Assignment)
                && binding
                    .value
                    .and_then(|range| self.expression_at(file, range))
                    .is_some_and(|value| matches!(value, ast::Expr::Call(_)));
            if matches!(binding.kind, BindingKind::Class | BindingKind::Param) || constructed {
                return self.class_factory_result(file, id, member, awaited, seen);
            }
        }
        if matches!(binding.kind, BindingKind::Assignment) {
            let (name, attribute) = match self.expression_at(file, binding.value?)? {
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
            let ids = self.value_binding_ids(file, binding.scope, &name, binding.range.start())?;
            if ids.is_empty() || ids.len() > 16 {
                return None;
            }
            let mut result = None;
            for id in ids {
                let mut branch = seen.clone();
                let contract =
                    self.factory_binding_result(file, id, member, awaited, &mut branch)?;
                if result.is_some_and(|old| {
                    !matches!(
                        (old, contract),
                        (Contract::Sync, Contract::Sync) | (Contract::Async, Contract::Async)
                    )
                }) {
                    return None;
                }
                result = Some(contract);
            }
            return result;
        }
        if matches!(binding.kind, BindingKind::Param) {
            let annotation = self.expression_at(file, binding.annotation?)?;
            let scope = file.scopes.scope(binding.scope).parent?;
            if !matches!(annotation, ast::Expr::String(_))
                && !Self::annotations_deferred(file)
                && Self::unknown_annotation_class(file, scope)
            {
                return None;
            }
            return self.factory_callable_annotation(
                file,
                scope,
                annotation,
                FactoryAnnotationProjection {
                    awaited,
                    before: binding.range.start(),
                },
                seen,
                0,
            );
        }
        if !matches!(binding.kind, BindingKind::Function) {
            return None;
        }
        self.function_factory_result(file, id, awaited, false, seen)
    }

    pub(super) fn declared_member_factory_result_contract(
        &self,
        file: &FileInfo,
        id: BindingId,
        member: &str,
        awaited_factory: bool,
    ) -> Option<Contract> {
        if self.authority.retired_members.contains(member) {
            return None;
        }
        self.factory_binding_result(
            file,
            id,
            Some(member),
            awaited_factory,
            &mut FxHashSet::default(),
        )
    }

    fn class_factory_result(
        &self,
        file: &FileInfo,
        id: BindingId,
        member: &str,
        awaited: bool,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
    ) -> Option<Contract> {
        if self.authority.retired_members.contains(member) {
            return None;
        }
        let (class_file, class, instance) = if let Some((file, class, _)) =
            self.context_instance_class(file, id, &mut FxHashSet::default())
        {
            (file, class, true)
        } else {
            let (file, class) =
                self.class_from_binding(file, id, None, &mut FxHashSet::default())?;
            (file, class, false)
        };
        let mro = self.class_mro(class_file, class, &mut FxHashSet::default(), &mut 0)?;
        if instance
            && mro.iter().any(|class| {
                !class.intrinsic
                    && !class
                        .file
                        .scopes
                        .bindings_in(class.scope, "__new__")
                        .is_empty()
            })
        {
            return None;
        }
        for class in mro {
            if class.intrinsic {
                continue;
            }
            let ids = class.file.scopes.bindings_in(class.scope, member);
            if ids.is_empty() {
                continue;
            }
            if ids.len() != 1
                || !matches!(
                    class.file.scopes.binding(ids[0]).kind,
                    BindingKind::Function
                )
            {
                return None;
            }
            let mut branch = seen.clone();
            if !branch.insert((class.file.id, ids[0])) || branch.len() > 16 {
                return None;
            }
            return self.function_factory_result(class.file, ids[0], awaited, true, &mut branch);
        }
        None
    }

    fn function_factory_result(
        &self,
        file: &FileInfo,
        id: BindingId,
        awaited: bool,
        known_receiver: bool,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
    ) -> Option<Contract> {
        let binding = file.scopes.binding(id);
        let root = file.parse.syntax_node();
        let function = node_at(&root, binding.node_range, SyntaxKind::FUNC_DEF)
            .and_then(ast::FuncDef::cast)?;
        if (function.decorators().next().is_some()
            && !(known_receiver
                && self
                    .builtin_method_decorator(file, binding.scope, &function)
                    .is_some()))
            || (function.is_async() && !awaited)
            || function.syntax().descendants().any(|node| {
                matches!(
                    node.kind(),
                    SyntaxKind::YIELD_EXPR | SyntaxKind::YIELD_FROM_EXPR
                ) && node
                    .ancestors()
                    .find(|owner| {
                        matches!(
                            owner.kind(),
                            SyntaxKind::FUNC_DEF | SyntaxKind::LAMBDA_EXPR | SyntaxKind::CLASS_DEF
                        )
                    })
                    .is_some_and(|owner| owner.text_range() == function.text_range())
            })
        {
            return None;
        }
        let scope = file
            .scopes
            .scope_for_node(function.text_range())
            .and_then(|scope| file.scopes.scope(scope).parent)?;
        let annotation = function.return_annotation()?.unparen();
        if !known_receiver
            && !matches!(annotation, ast::Expr::String(_))
            && !Self::annotations_deferred(file)
            && Self::unknown_annotation_class(file, scope)
        {
            return None;
        }
        self.returned_callable_annotation(
            file,
            scope,
            annotation,
            FactoryAnnotationProjection {
                awaited: awaited && !function.is_async(),
                before: binding.range.start(),
            },
            seen,
            0,
        )
    }

    fn factory_callable_annotation(
        &self,
        file: &FileInfo,
        scope: ScopeId,
        annotation: ast::Expr,
        projection: FactoryAnnotationProjection,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
        depth: usize,
    ) -> Option<Contract> {
        if depth >= 16 {
            return None;
        }
        match annotation.unparen() {
            annotation @ (ast::Expr::Name(_) | ast::Expr::Attribute(_)) => {
                let (file, scope, value, id) =
                    self.annotation_alias(file, scope, &annotation, projection.before)?;
                if !seen.insert((file.id, id)) {
                    return None;
                }
                self.factory_callable_annotation(
                    file,
                    scope,
                    value,
                    FactoryAnnotationProjection {
                        before: file.scopes.binding(id).range.start(),
                        ..projection
                    },
                    seen,
                    depth + 1,
                )
            }
            ast::Expr::String(string) => {
                let expression = Self::parsed_factory_annotation(&string)?;
                self.factory_callable_annotation(
                    file,
                    scope,
                    expression,
                    projection,
                    seen,
                    depth + 1,
                )
            }
            ast::Expr::Subscript(subscript) => {
                let arguments = Self::type_arguments(&subscript)?;
                let head = subscript.object()?;
                if self.canonical_type_symbol(file, scope, &head, &["Annotated"])
                    && arguments.len() >= 2
                {
                    return self.factory_callable_annotation(
                        file,
                        scope,
                        arguments[0].clone(),
                        projection,
                        seen,
                        depth + 1,
                    );
                }
                if arguments.len() != 2
                    || !self.canonical_type_symbol(file, scope, &head, &["Callable"])
                    || !self.callable_parameters(
                        file,
                        scope,
                        &arguments[0],
                        &mut FxHashSet::default(),
                        depth + 1,
                        projection.before,
                    )
                {
                    return None;
                }
                self.returned_callable_annotation(
                    file,
                    scope,
                    arguments[1].clone(),
                    projection,
                    seen,
                    depth + 1,
                )
            }
            _ => None,
        }
    }

    fn returned_callable_annotation(
        &self,
        file: &FileInfo,
        scope: ScopeId,
        annotation: ast::Expr,
        projection: FactoryAnnotationProjection,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
        depth: usize,
    ) -> Option<Contract> {
        if depth >= 16 {
            return None;
        }
        if !projection.awaited {
            return self.callable_annotation_contract(
                file,
                scope,
                annotation,
                seen,
                depth + 1,
                projection.before,
            );
        }
        match annotation.unparen() {
            annotation @ (ast::Expr::Name(_) | ast::Expr::Attribute(_)) => {
                let (file, scope, value, id) =
                    self.annotation_alias(file, scope, &annotation, projection.before)?;
                if !seen.insert((file.id, id)) {
                    return None;
                }
                self.returned_callable_annotation(
                    file,
                    scope,
                    value,
                    FactoryAnnotationProjection {
                        before: file.scopes.binding(id).range.start(),
                        ..projection
                    },
                    seen,
                    depth + 1,
                )
            }
            ast::Expr::String(string) => self.returned_callable_annotation(
                file,
                scope,
                Self::parsed_factory_annotation(&string)?,
                projection,
                seen,
                depth + 1,
            ),
            ast::Expr::Subscript(subscript) => {
                let head = subscript.object()?;
                let arguments = Self::type_arguments(&subscript)
                    .or_else(|| subscript.index_expr().map(|value| vec![value]))?;
                if self.canonical_type_symbol(file, scope, &head, &["Annotated"])
                    && arguments.len() >= 2
                {
                    return self.returned_callable_annotation(
                        file,
                        scope,
                        arguments[0].clone(),
                        projection,
                        seen,
                        depth + 1,
                    );
                }
                if !(arguments.len() == 1
                    && self.canonical_type_symbol(file, scope, &head, &["Awaitable"])
                    || arguments.len() == 3
                        && self.canonical_type_symbol(file, scope, &head, &["Coroutine"]))
                {
                    return None;
                }
                self.callable_annotation_contract(
                    file,
                    scope,
                    arguments.last()?.clone(),
                    seen,
                    depth + 1,
                    projection.before,
                )
            }
            _ => None,
        }
    }

    fn parsed_factory_annotation(string: &ast::StringExpr) -> Option<ast::Expr> {
        if string.syntax().text_range().len() > rustpy_core::TextSize::new(4096) {
            return None;
        }
        let Ok(rustpy_syntax::string_literal::Decoded::Text(text, _)) =
            rustpy_syntax::string_literal::string(&string.syntax().text().to_string())
        else {
            return None;
        };
        if text.len() > 4096 {
            return None;
        }
        let parsed = rustpy_syntax::parse_expression(&text);
        if !parsed.errors().is_empty() {
            return None;
        }
        parsed.syntax_node().children().find_map(ast::Expr::cast)
    }

    /// The outermost expression with exactly `range` (what a preorder search
    /// finds first), located from the covering element: O(depth), not a
    /// whole-file walk per declaration lookup.
    pub(super) fn expression_at(
        &self,
        file: &FileInfo,
        range: rustpy_core::TextRange,
    ) -> Option<ast::Expr> {
        let root = file.parse.syntax_node();
        if !root.text_range().contains_range(range) {
            return None;
        }
        let start = match root.covering_element(range) {
            rustpy_syntax::rowan::NodeOrToken::Node(node) => node,
            rustpy_syntax::rowan::NodeOrToken::Token(token) => token.parent()?,
        };
        start
            .ancestors()
            .filter(|node| node.text_range() == range && ast::Expr::can_cast(node.kind()))
            .last()
            .and_then(ast::Expr::cast)
            .map(ast::Expr::unparen)
    }

    fn canonical_type_symbol(
        &self,
        file: &FileInfo,
        scope: ScopeId,
        expression: &ast::Expr,
        expected: &[&str],
    ) -> bool {
        let (name, member) = match expression.clone().unparen() {
            ast::Expr::Name(name) => (name.name_text(), None),
            ast::Expr::Attribute(attribute) => {
                let Some(ast::Expr::Name(name)) = attribute.object().map(ast::Expr::unparen) else {
                    return false;
                };
                (name.name_text(), Some(attribute.attr_text()))
            }
            ast::Expr::String(string)
                if expected
                    .iter()
                    .all(|name| matches!(*name, "NoReturn" | "Never")) =>
            {
                let Ok(rustpy_syntax::string_literal::Decoded::Text(text, _)) =
                    rustpy_syntax::string_literal::string(&string.syntax().text().to_string())
                else {
                    return false;
                };
                match text.split_once('.') {
                    Some((name, member)) if !member.contains('.') => {
                        (name.to_owned(), Some(member.to_owned()))
                    }
                    None => (text, None),
                    _ => return false,
                }
            }
            _ => return false,
        };
        let Some(ids) = file.scopes.resolve(scope, &name) else {
            return false;
        };
        !ids.is_empty()
            && ids.iter().all(|id| {
                let binding = file.scopes.binding(*id);
                let BindingKind::Import(import) = &binding.kind else {
                    return false;
                };
                let symbol = match (member.as_deref(), import.name.as_deref()) {
                    (Some(name), None) | (None, Some(name)) => name,
                    _ => return false,
                };
                import.level == 0
                    && matches!(import.module.as_str(), "typing" | "typing_extensions")
                    && self.authority.modules.contains(import.module.as_str())
                    && expected.contains(&symbol)
                    && !self.authority.retired_members.contains(symbol)
                    && file.scopes.scope(binding.scope).star_imports.is_empty()
            })
    }

    pub(super) fn declared_parameter_contract(
        &self,
        file: &FileInfo,
        id: BindingId,
    ) -> Option<Contract> {
        let binding = file.scopes.binding(id);
        if !matches!(binding.kind, BindingKind::Param | BindingKind::Assignment) {
            return None;
        }
        let annotation = self.expression_at(file, binding.annotation?)?;
        let annotation_scope = if matches!(binding.kind, BindingKind::Param) {
            file.scopes.scope(binding.scope).parent?
        } else {
            binding.scope
        };
        // An eager header is evaluated in the actual class mapping, which
        // arbitrary bases/metaclasses can supply. Lexical module imports do
        // not establish the value read from an unknown mapping. Quoted and
        // future-deferred annotations are not evaluated by __prepare__.
        if !matches!(annotation, ast::Expr::String(_))
            && !Self::annotations_deferred(file)
            && Self::unknown_annotation_class(file, annotation_scope)
        {
            return None;
        }
        self.callable_annotation_contract(
            file,
            annotation_scope,
            annotation,
            &mut FxHashSet::default(),
            0,
            binding.range.start(),
        )
    }

    fn annotations_deferred(file: &FileInfo) -> bool {
        ast::Module::cast(file.parse.syntax_node()).is_some_and(|module| {
            module.statements().any(|statement| match statement {
                ast::Stmt::ImportFrom(import) => {
                    import.level() == 0
                        && import.module_path() == "__future__"
                        && import.aliases().any(|alias| alias.name() == "annotations")
                }
                _ => false,
            })
        })
    }

    fn unknown_annotation_class(file: &FileInfo, mut scope: ScopeId) -> bool {
        while file.scopes.scope(scope).kind == crate::scope::ScopeKind::TypeParams {
            let Some(parent) = file.scopes.scope(scope).parent else {
                return true;
            };
            scope = parent;
        }
        let scope = file.scopes.scope(scope);
        if scope.kind != crate::scope::ScopeKind::Class {
            return false;
        }
        // Match the flow parent's conservative namespace policy: any class
        // arguments may select an unknown metaclass/prepared dictionary.
        let Some(class) = node_at(
            &file.parse.syntax_node(),
            scope.range,
            SyntaxKind::CLASS_DEF,
        )
        .and_then(ast::ClassDef::cast) else {
            return true;
        };
        class
            .arguments()
            .is_some_and(|arguments| arguments.args().next().is_some())
    }

    // Resolve only bounded, static annotation aliases. No inference or execution.
    fn annotation_alias<'a>(
        &'a self,
        mut file: &'a FileInfo,
        scope: ScopeId,
        annotation: &ast::Expr,
        mut before: rustpy_core::TextSize,
    ) -> Option<(&'a FileInfo, ScopeId, ast::Expr, BindingId)> {
        let (name, member) = match annotation.clone().unparen() {
            ast::Expr::Name(name) => (name.name_text(), None),
            ast::Expr::Attribute(attribute) => {
                let ast::Expr::Name(name) = attribute.object()?.unparen() else {
                    return None;
                };
                (name.name_text(), Some(attribute.attr_text()))
            }
            _ => return None,
        };
        let ids = file.scopes.resolve(scope, &name)?;
        if ids.len() != 1 {
            return None;
        }
        let mut id = ids[0];
        let mut member = member;
        let mut visited = FxHashSet::default();
        loop {
            if visited.len() >= 16 || !visited.insert((file.id, id)) {
                return None;
            }
            let binding = file.scopes.binding(id);
            if binding.range.start() >= before
                || !file.scopes.scope(binding.scope).star_imports.is_empty()
            {
                return None;
            }
            let root = file.parse.syntax_node();
            let node = match root.covering_element(binding.range) {
                rustpy_syntax::rowan::NodeOrToken::Node(node) => node,
                rustpy_syntax::rowan::NodeOrToken::Token(token) => token.parent()?,
            };
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
            if let BindingKind::Import(import) = &binding.kind {
                let name = match (member.take(), import.name.as_deref()) {
                    (Some(name), None) => name,
                    (None, Some(name)) => name.to_owned(),
                    _ => return None,
                };
                if self.authority.retired_members.contains(name.as_str()) {
                    return None;
                }
                file = self.authority.project_imports.get(&(file.id, id))?;
                let ids = file.scopes.resolve(file.scopes.module_scope(), &name)?;
                if ids.len() != 1 {
                    return None;
                }
                id = ids[0];
                before = file.parse.syntax_node().text_range().end();
                continue;
            }
            if member.is_some() || !matches!(binding.kind, BindingKind::Assignment) {
                return None;
            }
            let expression = self.expression_at(file, binding.value?)?;
            return Some((file, binding.scope, expression, id));
        }
    }

    fn callable_annotation_contract(
        &self,
        file: &FileInfo,
        scope: ScopeId,
        annotation: ast::Expr,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
        depth: usize,
        before: rustpy_core::TextSize,
    ) -> Option<Contract> {
        if depth >= 16 {
            return None;
        }
        match annotation.unparen() {
            annotation @ (ast::Expr::Name(_) | ast::Expr::Attribute(_)) => {
                let (file, scope, expression, id) =
                    self.annotation_alias(file, scope, &annotation, before)?;
                if !seen.insert((file.id, id)) {
                    return None;
                }
                self.callable_annotation_contract(
                    file,
                    scope,
                    expression,
                    seen,
                    depth + 1,
                    file.scopes.binding(id).range.start(),
                )
            }
            ast::Expr::String(string) => {
                if string.syntax().text_range().len() > rustpy_core::TextSize::new(4096) {
                    return None;
                }
                let Ok(rustpy_syntax::string_literal::Decoded::Text(text, _)) =
                    rustpy_syntax::string_literal::string(&string.syntax().text().to_string())
                else {
                    return None;
                };
                if text.len() > 4096 {
                    return None;
                }
                let parsed = rustpy_syntax::parse_expression(&text);
                if !parsed.errors().is_empty() {
                    return None;
                }
                let expression = parsed.syntax_node().children().find_map(ast::Expr::cast)?;
                self.callable_annotation_contract(file, scope, expression, seen, depth + 1, before)
            }
            ast::Expr::Subscript(subscript) => {
                let head = subscript.object()?;
                if self.canonical_type_symbol(file, scope, &head, &["Annotated"]) {
                    let arguments = Self::type_arguments(&subscript)?;
                    if arguments.len() < 2 {
                        return None;
                    }
                    return self.callable_annotation_contract(
                        file,
                        scope,
                        arguments[0].clone(),
                        seen,
                        depth + 1,
                        before,
                    );
                }
                if !self.canonical_type_symbol(file, scope, &head, &["Callable"]) {
                    return None;
                }
                let arguments = Self::type_arguments(&subscript)?;
                if arguments.len() != 2
                    || !self.callable_parameters(
                        file,
                        scope,
                        &arguments[0],
                        &mut FxHashSet::default(),
                        0,
                        before,
                    )
                {
                    return None;
                }
                if self.canonical_type_symbol(file, scope, &arguments[1], &["NoReturn", "Never"]) {
                    Some(Contract::Sync)
                } else {
                    self.awaitable_return_contract(file, scope, &arguments[1])
                }
            }
            _ => None,
        }
    }

    fn callable_parameters(
        &self,
        file: &FileInfo,
        scope: ScopeId,
        parameters: &ast::Expr,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
        depth: usize,
        before: rustpy_core::TextSize,
    ) -> bool {
        if depth >= 16 {
            return false;
        }
        match parameters.clone().unparen() {
            ast::Expr::List(_) => true,
            ast::Expr::Literal(literal) => literal.kind() == Some(SyntaxKind::ELLIPSIS),
            annotation @ (ast::Expr::Name(_) | ast::Expr::Attribute(_)) => {
                let Some((file, scope, expression, id)) =
                    self.annotation_alias(file, scope, &annotation, before)
                else {
                    return false;
                };
                if !seen.insert((file.id, id)) {
                    return false;
                }
                if let ast::Expr::Call(call) = expression {
                    let Some(callee) = call.callee() else {
                        return false;
                    };
                    self.canonical_type_symbol(file, scope, &callee, &["ParamSpec"])
                        && self.paramspec_arguments(file, scope, &call)
                } else {
                    self.callable_parameters(
                        file,
                        scope,
                        &expression,
                        seen,
                        depth + 1,
                        file.scopes.binding(id).range.start(),
                    )
                }
            }
            ast::Expr::Subscript(subscript) => {
                let Some(head) = subscript.object() else {
                    return false;
                };
                if !self.canonical_type_symbol(file, scope, &head, &["Concatenate"]) {
                    return false;
                }
                let Some(arguments) = Self::fixed_annotation_arguments(&subscript) else {
                    return false;
                };
                if arguments.len() < 2 {
                    return false;
                }
                // Prefix types do not determine return completion. Require a
                // canonical captured ParamSpec tail, not an arbitrary shape.
                let tail = arguments.last().unwrap();
                if matches!(tail.clone().unparen(),ast::Expr::Literal(value) if value.kind()==Some(SyntaxKind::ELLIPSIS))
                {
                    return true;
                }
                self.canonical_paramspec(file, scope, tail, seen, depth + 1, before)
            }
            _ => false,
        }
    }

    fn canonical_paramspec(
        &self,
        file: &FileInfo,
        scope: ScopeId,
        expression: &ast::Expr,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
        depth: usize,
        before: rustpy_core::TextSize,
    ) -> bool {
        if depth >= 16 {
            return false;
        }
        let Some((file, scope, value, id)) = self.annotation_alias(file, scope, expression, before)
        else {
            return false;
        };
        if !seen.insert((file.id, id)) {
            return false;
        }
        match value.unparen() {
            ast::Expr::Call(call) => {
                call.callee().is_some_and(|callee| {
                    self.canonical_type_symbol(file, scope, &callee, &["ParamSpec"])
                }) && self.paramspec_arguments(file, scope, &call)
            }
            expression @ (ast::Expr::Name(_) | ast::Expr::Attribute(_)) => self
                .canonical_paramspec(
                    file,
                    scope,
                    &expression,
                    seen,
                    depth + 1,
                    file.scopes.binding(id).range.start(),
                ),
            _ => false,
        }
    }

    /// Project only a known fixed or homogeneous tuple parameter; the caller owns actual
    /// reaching-binding/literal-index proof at the captured callee state.
    pub(super) fn declared_indexed_parameter_contract(
        &self,
        file: &FileInfo,
        id: BindingId,
        index: i64,
    ) -> Option<Contract> {
        let binding = file.scopes.binding(id);
        if !matches!(binding.kind, BindingKind::Param) {
            return None;
        }
        let scope = file.scopes.scope(binding.scope).parent?;
        let annotation = self.expression_at(file, binding.annotation?)?;
        if !matches!(annotation, ast::Expr::String(_))
            && !Self::annotations_deferred(file)
            && Self::unknown_annotation_class(file, scope)
        {
            return None;
        }
        self.indexed_annotation_contract(
            file,
            scope,
            annotation,
            IndexedAnnotationProjection {
                index,
                before: binding.range.start(),
            },
            &mut FxHashSet::default(),
            0,
        )
    }

    fn indexed_annotation_contract(
        &self,
        file: &FileInfo,
        scope: ScopeId,
        annotation: ast::Expr,
        projection: IndexedAnnotationProjection,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
        depth: usize,
    ) -> Option<Contract> {
        let IndexedAnnotationProjection { index, before } = projection;
        if depth >= 16 {
            return None;
        }
        match annotation.unparen() {
            annotation @ (ast::Expr::Name(_) | ast::Expr::Attribute(_)) => {
                let (file, scope, expression, id) =
                    self.annotation_alias(file, scope, &annotation, before)?;
                if !seen.insert((file.id, id)) {
                    return None;
                }
                self.indexed_annotation_contract(
                    file,
                    scope,
                    expression,
                    IndexedAnnotationProjection {
                        index,
                        before: file.scopes.binding(id).range.start(),
                    },
                    seen,
                    depth + 1,
                )
            }
            ast::Expr::String(string) => {
                if string.syntax().text_range().len() > rustpy_core::TextSize::new(4096) {
                    return None;
                }
                let Ok(rustpy_syntax::string_literal::Decoded::Text(text, _)) =
                    rustpy_syntax::string_literal::string(&string.syntax().text().to_string())
                else {
                    return None;
                };
                if text.len() > 4096 {
                    return None;
                }
                let parsed = rustpy_syntax::parse_expression(&text);
                if !parsed.errors().is_empty() {
                    return None;
                }
                let expression = parsed.syntax_node().children().find_map(ast::Expr::cast)?;
                self.indexed_annotation_contract(
                    file,
                    scope,
                    expression,
                    projection,
                    seen,
                    depth + 1,
                )
            }
            ast::Expr::Subscript(subscript) => {
                let head = subscript.object()?;
                if self.canonical_type_symbol(file, scope, &head, &["Annotated"]) {
                    let arguments = Self::type_arguments(&subscript)?;
                    if arguments.len() < 2 {
                        return None;
                    }
                    return self.indexed_annotation_contract(
                        file,
                        scope,
                        arguments[0].clone(),
                        projection,
                        seen,
                        depth + 1,
                    );
                }
                let builtin = matches!(head.clone().unparen(),ast::Expr::Name(name) if name.name_text()=="tuple" && file.scopes.resolve(scope,"tuple").is_none());
                if !builtin && !self.canonical_type_symbol(file, scope, &head, &["Tuple"]) {
                    return None;
                }
                if builtin {
                    let mut current = Some(scope);
                    while let Some(id) = current {
                        let scope = file.scopes.scope(id);
                        if !scope.star_imports.is_empty() {
                            return None;
                        }
                        current = scope.parent;
                    }
                }
                let arguments = Self::fixed_annotation_arguments(&subscript)?;
                if arguments.len() == 2
                    && matches!(arguments[1].clone().unparen(),ast::Expr::Literal(value) if value.kind()==Some(SyntaxKind::ELLIPSIS))
                {
                    return self.callable_annotation_contract(
                        file,
                        scope,
                        arguments[0].clone(),
                        seen,
                        depth + 1,
                        before,
                    );
                }
                if arguments.iter().any(|argument|matches!(argument.clone().unparen(),ast::Expr::Star(_))
                    || matches!(argument.clone().unparen(),ast::Expr::Literal(value) if value.kind()==Some(SyntaxKind::ELLIPSIS))) { return None; }
                let normalized = if index < 0 {
                    (arguments.len() as i64).checked_add(index)?
                } else {
                    index
                };
                let selected = arguments.get(usize::try_from(normalized).ok()?)?;
                self.callable_annotation_contract(
                    file,
                    scope,
                    selected.clone(),
                    seen,
                    depth + 1,
                    before,
                )
            }
            _ => None,
        }
    }

    fn fixed_annotation_arguments(subscript: &ast::SubscriptExpr) -> Option<Vec<ast::Expr>> {
        let raw = subscript.index()?;
        let arguments = if raw.kind() == SyntaxKind::SLICE_LIST {
            raw.children()
                .take(17)
                .map(ast::Expr::cast)
                .collect::<Option<Vec<_>>>()?
        } else {
            match ast::Expr::cast(raw)?.unparen() {
                ast::Expr::Tuple(tuple) => tuple
                    .syntax()
                    .children()
                    .take(17)
                    .map(ast::Expr::cast)
                    .collect::<Option<Vec<_>>>()?,
                expression => vec![expression],
            }
        };
        (!arguments.is_empty() && arguments.len() <= 16).then_some(arguments)
    }

    // CPython 3.12 ParamSpec(name, *, bound=None, covariant=False,
    // contravariant=False). Admit only inert literal option values here.
    fn paramspec_arguments(&self, file: &FileInfo, scope: ScopeId, call: &ast::CallExpr) -> bool {
        let arguments: Vec<_> = call
            .arg_list()
            .into_iter()
            .flat_map(|list| list.args())
            .take(5)
            .collect();
        let Some(ast::AnyArg::Positional(first)) = arguments.first() else {
            return false;
        };
        if arguments.len() > 4 {
            return false;
        }
        let Some(ast::Expr::String(name)) = first.expr().map(ast::Expr::unparen) else {
            return false;
        };
        if name.syntax().text_range().len() > rustpy_core::TextSize::new(4096) {
            return false;
        }
        if !matches!(
            rustpy_syntax::string_literal::string(&name.syntax().text().to_string()),
            Ok(rustpy_syntax::string_literal::Decoded::Text(_, _))
        ) {
            return false;
        }
        let mut seen = FxHashSet::default();
        let mut covariant = false;
        let mut contravariant = false;
        for argument in arguments.iter().skip(1) {
            let ast::AnyArg::Keyword(argument) = argument else {
                return false;
            };
            let name = argument.name_text();
            if !seen.insert(name.clone()) {
                return false;
            }
            let Some(value) = argument.value().map(ast::Expr::unparen) else {
                return false;
            };
            if name == "bound" && !matches!(&value, ast::Expr::Literal(_)) {
                if !self.paramspec_class_bound(
                    file,
                    scope,
                    &value,
                    call.syntax().text_range().start(),
                ) {
                    return false;
                }
                continue;
            }
            let ast::Expr::Literal(value) = value else {
                return false;
            };
            match name.as_str() {
                "bound" if value.kind() == Some(SyntaxKind::NONE_KW) => {}
                "covariant" | "contravariant"
                    if matches!(
                        value.kind(),
                        Some(SyntaxKind::TRUE_KW | SyntaxKind::FALSE_KW)
                    ) =>
                {
                    let enabled = value.kind() == Some(SyntaxKind::TRUE_KW);
                    if name == "covariant" {
                        covariant = enabled;
                    } else {
                        contravariant = enabled;
                    }
                }
                _ => return false,
            }
        }
        !(covariant && contravariant)
    }

    fn paramspec_class_bound(
        &self,
        file: &FileInfo,
        scope: ScopeId,
        value: &ast::Expr,
        before: rustpy_core::TextSize,
    ) -> bool {
        let (name, member) = match value.clone().unparen() {
            ast::Expr::Name(name) => (name, None),
            ast::Expr::Attribute(attribute) => {
                let Some(ast::Expr::Name(name)) = attribute.object().map(ast::Expr::unparen) else {
                    return false;
                };
                (name, Some(attribute.attr_text()))
            }
            _ => return false,
        };
        if member.is_none()
            && matches!(
                name.name_text().as_str(),
                "object"
                    | "str"
                    | "int"
                    | "bool"
                    | "float"
                    | "complex"
                    | "bytes"
                    | "bytearray"
                    | "list"
                    | "tuple"
                    | "dict"
                    | "set"
                    | "frozenset"
                    | "type"
            )
            && file.scopes.resolve(scope, &name.name_text()).is_none()
        {
            let mut current = Some(scope);
            while let Some(id) = current {
                let scope = file.scopes.scope(id);
                if !scope.star_imports.is_empty() {
                    return false;
                }
                current = scope.parent;
            }
            return true;
        }
        let Some(ids) = self.value_binding_ids(file, scope, &name, before) else {
            return false;
        };
        if ids.len() != 1 {
            return false;
        }
        let Some((file, class)) =
            self.class_from_binding(file, ids[0], member.as_deref(), &mut FxHashSet::default())
        else {
            return false;
        };
        self.class_mro(file, class, &mut FxHashSet::default(), &mut 0)
            .is_some()
    }

    fn type_arguments(subscript: &ast::SubscriptExpr) -> Option<Vec<ast::Expr>> {
        let index = subscript.index()?;
        if index.kind() == SyntaxKind::SLICE_LIST {
            Some(
                index
                    .children()
                    .filter_map(ast::Expr::cast)
                    .take(4)
                    .collect(),
            )
        } else {
            match ast::Expr::cast(index)?.unparen() {
                ast::Expr::Tuple(tuple) => Some(tuple.elements().take(4).collect()),
                _ => None,
            }
        }
    }

    pub(super) fn awaitable_return_contract(
        &self,
        file: &FileInfo,
        scope: ScopeId,
        expression: &ast::Expr,
    ) -> Option<Contract> {
        let ast::Expr::Subscript(subscript) = expression.clone().unparen() else {
            return None;
        };
        let head = subscript.object()?;
        let arguments = Self::type_arguments(&subscript)
            .or_else(|| subscript.index_expr().map(|argument| vec![argument]))?;
        let valid_head = arguments.len() == 1
            && self.canonical_type_symbol(file, scope, &head, &["Awaitable"])
            || arguments.len() == 3
                && self.canonical_type_symbol(file, scope, &head, &["Coroutine"]);
        (valid_head
            && self.canonical_type_symbol(file, scope, arguments.last()?, &["NoReturn", "Never"]))
        .then_some(Contract::Async)
    }

    fn value_binding_ids(
        &self,
        file: &FileInfo,
        scope: ScopeId,
        name: &ast::NameExpr,
        before: rustpy_core::TextSize,
    ) -> Option<Vec<BindingId>> {
        let start = name.name_token()?.text_range().start();
        if file.id == self.file.id {
            if let Some(reaches) = self.info.refs.get(&start) {
                if reaches.is_empty()
                    || reaches
                        .iter()
                        .any(|reach| reach.binding.is_none() || !reach.preds.is_empty())
                {
                    return None;
                }
                return Some(reaches.iter().filter_map(|reach| reach.binding).collect());
            }
        }
        let ids = file.scopes.resolve(scope, &name.name_text())?;
        if ids.len() != 1 || file.scopes.binding(ids[0]).range.start() >= before {
            return None;
        }
        Some(ids.to_vec())
    }

    fn class_from_binding<'a>(
        &'a self,
        file: &'a FileInfo,
        id: BindingId,
        member: Option<&str>,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
    ) -> Option<(&'a FileInfo, ast::ClassDef)> {
        if seen.len() >= 16 || !seen.insert((file.id, id)) {
            return None;
        }
        let result = (|| {
            let binding = file.scopes.binding(id);
            if !file.scopes.scope(binding.scope).star_imports.is_empty() {
                return None;
            }
            if let BindingKind::Import(import) = &binding.kind {
                let name = match (member, import.name.as_deref()) {
                    (Some(name), None) | (None, Some(name)) => name,
                    _ => return None,
                };
                if self.authority.retired_members.contains(name) {
                    return None;
                }
                let target = self.authority.project_imports.get(&(file.id, id))?.as_ref();
                let ids = target.scopes.resolve(target.scopes.module_scope(), name)?;
                if ids.len() != 1 {
                    return None;
                }
                return self.class_from_binding(target, ids[0], None, seen);
            }
            if matches!(binding.kind, BindingKind::Assignment) {
                let expression = self.expression_at(file, binding.value?)?;
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
                let ids =
                    self.value_binding_ids(file, binding.scope, &name, binding.range.start())?;
                if ids.len() != 1 {
                    return None;
                }
                return self.class_from_binding(
                    file,
                    ids[0],
                    member.or(attribute.as_deref()),
                    seen,
                );
            }
            if member.is_some() || !matches!(binding.kind, BindingKind::Class) {
                return None;
            }
            let root = file.parse.syntax_node();
            let class = node_at(&root, binding.node_range, SyntaxKind::CLASS_DEF)
                .and_then(ast::ClassDef::cast)?;
            if class
                .syntax()
                .ancestors()
                .skip(1)
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
            Some((file, class))
        })();
        seen.remove(&(file.id, id));
        result
    }

    fn class_shape(&self, file: &FileInfo, class: &ast::ClassDef) -> Option<ScopeId> {
        if class.decorators().next().is_some() {
            return None;
        }
        if [
            "__call__",
            "__new__",
            "__init__",
            "__class__",
            "__getattr__",
            "__getattribute__",
            "__bases__",
            "__mro__",
            "__dict__",
        ]
        .iter()
        .any(|name| self.authority.retired_members.contains(*name))
        {
            return None;
        }
        let scope = file.scopes.scope_for_node(class.syntax().text_range())?;
        if !file.scopes.scope(scope).star_imports.is_empty()
            || ["__getattr__", "__getattribute__"]
                .iter()
                .any(|name| !file.scopes.bindings_in(scope, name).is_empty())
        {
            return None;
        }
        // No arbitrary class-suite execution/namespace construction in this
        // small proof. Other methods remain inert until their normal call.
        for statement in class.body()?.statements() {
            match statement {
                ast::Stmt::FuncDef(function) => {
                    if function.decorators().next().is_some()
                        && self
                            .builtin_method_decorator(file, scope, &function)
                            .is_none()
                        && !self.builtin_property_decorator(file, scope, &function)
                        && !self.overload_declaration(file, &function, scope)
                    {
                        return None;
                    }
                    let mut headers: Vec<_> = function
                        .params()
                        .into_iter()
                        .flat_map(|parameters| parameters.params())
                        .flat_map(|parameter| {
                            parameter
                                .default()
                                .into_iter()
                                .chain(parameter.annotation())
                        })
                        .collect();
                    headers.extend(function.return_annotation());
                    if headers.iter().any(|expression| {
                        expression.syntax().descendants().any(|node| {
                            matches!(
                                ast::Expr::cast(node),
                                Some(
                                    ast::Expr::Call(_) | ast::Expr::Await(_) | ast::Expr::Named(_)
                                )
                            )
                        })
                    }) {
                        return None;
                    }
                }
                ast::Stmt::Pass(_) => {}
                ast::Stmt::Expr(expression)
                    if expression
                        .expr()
                        .is_some_and(|expression| matches!(expression, ast::Expr::String(_))) => {}
                _ => return None,
            }
        }
        Some(scope)
    }

    // Only the unchanged intrinsic decorators are transparent descriptor
    // construction. A same-spelled class/module/local binding is not one.
    fn builtin_method_decorator(
        &self,
        file: &FileInfo,
        scope: ScopeId,
        function: &ast::FuncDef,
    ) -> Option<String> {
        let decorators: Vec<_> = function.decorators().take(2).collect();
        let [decorator] = decorators.as_slice() else {
            return None;
        };
        self.builtin_method_descriptor(file, scope, decorator.expr()?)
    }

    fn builtin_method_descriptor(
        &self,
        file: &FileInfo,
        scope: ScopeId,
        expression: ast::Expr,
    ) -> Option<String> {
        let ast::Expr::Name(name) = expression.unparen() else {
            return None;
        };
        let name = name.name_text();
        if !matches!(name.as_str(), "staticmethod" | "classmethod")
            || file.scopes.resolve(scope, &name).is_some()
            || self.authority.retired_members.contains(name.as_str())
        {
            return None;
        }
        let mut current = Some(scope);
        while let Some(id) = current {
            let scope = file.scopes.scope(id);
            if !scope.star_imports.is_empty() {
                return None;
            }
            current = scope.parent;
        }
        Some(name)
    }

    pub(super) fn declared_member_contract(
        &self,
        file: &FileInfo,
        id: BindingId,
        member: &str,
    ) -> Option<Contract> {
        if self.authority.retired_members.contains(member) {
            return None;
        }
        let binding = file.scopes.binding(id);
        let mut instance = false;
        let (class_file, class) = if matches!(binding.kind, BindingKind::Param) {
            instance = true;
            let scope = file.scopes.scope(binding.scope).parent?;
            let annotation = self.expression_at(file, binding.annotation?)?;
            if !matches!(annotation, ast::Expr::String(_))
                && !Self::annotations_deferred(file)
                && Self::unknown_annotation_class(file, scope)
            {
                return None;
            }
            self.annotated_receiver_class(file, scope, annotation, 0)?
        } else if matches!(binding.kind, BindingKind::Assignment) {
            let expression = self.expression_at(file, binding.value?)?;
            if let ast::Expr::Call(call) = expression {
                instance = true;
                let (name, attribute) = match call.callee()?.unparen() {
                    ast::Expr::Name(name) => (name, None),
                    ast::Expr::Attribute(attribute) => {
                        let ast::Expr::Name(name) = attribute.object()?.unparen() else {
                            return None;
                        };
                        (name, Some(attribute.attr_text()))
                    }
                    _ => return None,
                };
                let ids =
                    self.value_binding_ids(file, binding.scope, &name, binding.range.start())?;
                if ids.len() != 1 {
                    return None;
                }
                self.class_from_binding(
                    file,
                    ids[0],
                    attribute.as_deref(),
                    &mut FxHashSet::default(),
                )?
            } else {
                self.class_from_binding(file, id, None, &mut FxHashSet::default())?
            }
        } else {
            self.class_from_binding(file, id, None, &mut FxHashSet::default())?
        };
        self.receiver_method_contract(class_file, class, member, instance)
    }

    fn annotated_receiver_class<'a>(
        &'a self,
        file: &'a FileInfo,
        scope: ScopeId,
        annotation: ast::Expr,
        depth: usize,
    ) -> Option<(&'a FileInfo, ast::ClassDef)> {
        if depth >= 16 {
            return None;
        }
        if let ast::Expr::String(string) = &annotation {
            if string.syntax().text_range().len() > rustpy_core::TextSize::new(4096) {
                return None;
            }
            let Ok(rustpy_syntax::string_literal::Decoded::Text(text, _)) =
                rustpy_syntax::string_literal::string(&string.syntax().text().to_string())
            else {
                return None;
            };
            if text.len() > 4096 {
                return None;
            }
            let parsed = rustpy_syntax::parse_expression(&text);
            if !parsed.errors().is_empty() {
                return None;
            }
            let expression = parsed.syntax_node().children().find_map(ast::Expr::cast)?;
            return self.annotated_receiver_class(file, scope, expression, depth + 1);
        }
        let (name, member) = match annotation.unparen() {
            ast::Expr::Name(name) => (name, None),
            ast::Expr::Attribute(attribute) => {
                let ast::Expr::Name(name) = attribute.object()?.unparen() else {
                    return None;
                };
                (name, Some(attribute.attr_text()))
            }
            _ => return None,
        };
        let ids = file.scopes.resolve(scope, &name.name_text())?;
        if ids.len() != 1 {
            return None;
        }
        self.class_from_binding(file, ids[0], member.as_deref(), &mut FxHashSet::default())
    }

    pub(super) fn inline_receiver_member_contract(
        &self,
        call: &ast::CallExpr,
        member: &str,
        state: &State,
    ) -> Option<Contract> {
        // Admit only the direct empty construction form. Evaluated arguments
        // can rebind the class after its callable was captured; resolving that
        // again here would not prove the constructed receiver identity.
        if call
            .arg_list()
            .is_some_and(|arguments| arguments.args().next().is_some())
        {
            return None;
        }
        let ast::Expr::Name(name) = call.callee()?.unparen() else {
            return None;
        };
        let ids = if let Some(reaches) = self.reaching_name(&name.name_text(), state) {
            if reaches.is_empty()
                || reaches
                    .iter()
                    .any(|reach| reach.binding.is_none() || !reach.preds.is_empty())
            {
                return None;
            }
            reaches
                .iter()
                .filter_map(|reach| reach.binding)
                .take(17)
                .collect::<Vec<_>>()
        } else {
            self.file
                .scopes
                .resolve(self.scope, &name.name_text())?
                .iter()
                .copied()
                .take(17)
                .collect()
        };
        if ids.is_empty() || ids.len() > 16 {
            return None;
        }
        let mut contract = None;
        for id in ids {
            let (file, class) =
                self.class_from_binding(self.file, id, None, &mut FxHashSet::default())?;
            let current = self.receiver_method_contract(file, class, member, true)?;
            if contract.is_some_and(|old| {
                !matches!(
                    (old, current),
                    (Contract::Sync, Contract::Sync) | (Contract::Async, Contract::Async)
                )
            }) {
                return None;
            }
            contract = Some(current);
        }
        contract
    }

    fn receiver_method_contract(
        &self,
        class_file: &FileInfo,
        class: ast::ClassDef,
        member: &str,
        instance: bool,
    ) -> Option<Contract> {
        if self.authority.retired_members.contains(member) {
            return None;
        }
        let mro = self.class_mro(class_file, class, &mut FxHashSet::default(), &mut 0)?;
        if instance
            && mro.iter().any(|class| {
                !class.intrinsic
                    && !class
                        .file
                        .scopes
                        .bindings_in(class.scope, "__new__")
                        .is_empty()
            })
        {
            return None;
        }
        for class in &mro {
            if class.intrinsic {
                continue;
            }
            let ids = class.file.scopes.bindings_in(class.scope, member);
            if ids.is_empty() {
                continue;
            }
            if ids.len() != 1 {
                return if instance || self.overloaded_class_descriptor(class.file, ids[0]) {
                    self.overload_group_contract(class.file, ids[0], true)
                        .flatten()
                } else {
                    None
                };
            }
            let binding = class.file.scopes.binding(ids[0]);
            if !matches!(binding.kind, BindingKind::Function) {
                return None;
            }
            let function = node_at(
                &class.file.parse.syntax_node(),
                binding.node_range,
                SyntaxKind::FUNC_DEF,
            )
            .and_then(ast::FuncDef::cast)?;
            let decorated = function.decorators().next().is_some();
            if decorated {
                self.builtin_method_decorator(class.file, class.scope, &function)?;
            }
            if function.syntax().descendants().any(|node| {
                matches!(
                    node.kind(),
                    SyntaxKind::YIELD_EXPR | SyntaxKind::YIELD_FROM_EXPR
                ) && node
                    .ancestors()
                    .find(|owner| {
                        matches!(
                            owner.kind(),
                            SyntaxKind::FUNC_DEF | SyntaxKind::LAMBDA_EXPR | SyntaxKind::CLASS_DEF
                        )
                    })
                    .is_some_and(|owner| owner.text_range() == function.syntax().text_range())
            }) {
                return None;
            }
            let annotation = function.return_annotation()?.unparen();
            if self.canonical_type_symbol(
                class.file,
                binding.scope,
                &annotation,
                &["Never", "NoReturn"],
            ) {
                return Some(if function.is_async() {
                    Contract::Async
                } else {
                    Contract::Sync
                });
            }
            return if function.is_async() {
                None
            } else {
                self.awaitable_return_contract(class.file, binding.scope, &annotation)
            };
        }
        None
    }

    fn context_instance_class<'a>(
        &'a self,
        file: &'a FileInfo,
        id: BindingId,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
    ) -> Option<(&'a FileInfo, ast::ClassDef, bool)> {
        if seen.len() >= 16 || !seen.insert((file.id, id)) {
            return None;
        }
        let binding = file.scopes.binding(id);
        if matches!(binding.kind, BindingKind::Param) {
            let scope = file.scopes.scope(binding.scope).parent?;
            let annotation = self.expression_at(file, binding.annotation?)?;
            if !matches!(annotation, ast::Expr::String(_))
                && !Self::annotations_deferred(file)
                && Self::unknown_annotation_class(file, scope)
            {
                return None;
            }
            return self
                .annotated_receiver_class(file, scope, annotation, 0)
                .map(|(file, class)| (file, class, false));
        }
        if !matches!(binding.kind, BindingKind::Assignment) {
            return None;
        }
        match self.expression_at(file, binding.value?)? {
            ast::Expr::Call(call) => {
                let ast::Expr::Name(name) = call.callee()?.unparen() else {
                    return None;
                };
                let ids =
                    self.value_binding_ids(file, binding.scope, &name, binding.range.start())?;
                if ids.len() != 1 {
                    return None;
                }
                let empty_arguments = call
                    .arg_list()
                    .is_none_or(|arguments| arguments.args().next().is_none());
                self.class_from_binding(file, ids[0], None, &mut FxHashSet::default())
                    .map(|(file, class)| (file, class, empty_arguments))
            }
            ast::Expr::Name(name) => {
                let ids =
                    self.value_binding_ids(file, binding.scope, &name, binding.range.start())?;
                if ids.len() != 1 {
                    return None;
                }
                self.context_instance_class(file, ids[0], seen)
            }
            _ => None,
        }
    }

    fn context_mro<'a>(
        &'a self,
        context: &ast::Expr,
        state: &State,
    ) -> Option<(Vec<KnownClass<'a>>, bool)> {
        let (name, inline) = match context.clone().unparen() {
            ast::Expr::Name(name) => (name, false),
            ast::Expr::Call(call) => {
                if call
                    .arg_list()
                    .is_some_and(|arguments| arguments.args().next().is_some())
                {
                    return None;
                }
                let ast::Expr::Name(name) = call.callee()?.unparen() else {
                    return None;
                };
                (name, true)
            }
            _ => return None,
        };
        let ids = if let Some(reaches) = self.reaching_name(&name.name_text(), state) {
            if reaches.is_empty()
                || reaches
                    .iter()
                    .any(|reach| reach.binding.is_none() || !reach.preds.is_empty())
            {
                return None;
            }
            reaches
                .iter()
                .filter_map(|reach| reach.binding)
                .take(17)
                .collect::<Vec<_>>()
        } else {
            self.file
                .scopes
                .resolve(self.scope, &name.name_text())?
                .iter()
                .copied()
                .take(17)
                .collect()
        };
        if ids.is_empty() || ids.len() > 16 {
            return None;
        }
        let mut result: Option<Vec<KnownClass<'a>>> = None;
        let mut exact = true;
        for id in ids {
            let (file, class, constructed) = if inline {
                let (file, class) =
                    self.class_from_binding(self.file, id, None, &mut FxHashSet::default())?;
                (file, class, true)
            } else {
                self.context_instance_class(self.file, id, &mut FxHashSet::default())?
            };
            let mro = self.class_mro(file, class, &mut FxHashSet::default(), &mut 0)?;
            if mro.iter().any(|class| {
                !class.intrinsic
                    && !class
                        .file
                        .scopes
                        .bindings_in(class.scope, "__new__")
                        .is_empty()
            }) {
                return None;
            }
            if result.as_ref().is_some_and(|previous| {
                previous
                    .iter()
                    .map(KnownClass::identity)
                    .ne(mro.iter().map(KnownClass::identity))
            }) {
                return None;
            }
            exact &= constructed && self.inert_context_initializer(&mro);
            result = Some(mro);
        }
        result.map(|mro| (mro, exact))
    }

    // Exact construction must not expose the newly allocated object to
    // opaque initializer code that can change __class__ or exit dispatch.
    // Known default object initialization and a sole pass body are inert.
    fn inert_context_initializer(&self, mro: &[KnownClass<'_>]) -> bool {
        for class in mro {
            if class.intrinsic {
                return true;
            }
            let ids = class.file.scopes.bindings_in(class.scope, "__init__");
            if ids.is_empty() {
                continue;
            }
            if ids.len() != 1 || self.authority.retired_members.contains("__init__") {
                return false;
            }
            let binding = class.file.scopes.binding(ids[0]);
            if !matches!(binding.kind, BindingKind::Function) {
                return false;
            }
            let Some(function) = node_at(
                &class.file.parse.syntax_node(),
                binding.node_range,
                SyntaxKind::FUNC_DEF,
            )
            .and_then(ast::FuncDef::cast) else {
                return false;
            };
            if function.is_async() || function.decorators().next().is_some() {
                return false;
            }
            let statements: Vec<_> = function
                .body()
                .into_iter()
                .flat_map(|body| body.statements())
                .take(2)
                .collect();
            return matches!(statements.as_slice(), [ast::Stmt::Pass(_)]);
        }
        false
    }

    // The implicit ordinary exit invocation supplies self and three
    // exception arguments. A constant body alone cannot rule out binding
    // TypeError from an incompatible signature.
    fn ordinary_exit_accepts_arguments(function: &ast::FuncDef) -> bool {
        let parameters: Vec<_> = function
            .params()
            .into_iter()
            .flat_map(|parameters| parameters.params())
            .take(17)
            .collect();
        if parameters.len() > 16 {
            return false;
        }
        let mut required = 0;
        let mut positional = 0;
        let mut keyword_only = false;
        let mut variadic = false;
        for parameter in parameters {
            match parameter {
                ast::AnyParam::Plain(parameter) => {
                    if keyword_only {
                        if parameter.default().is_none() {
                            return false;
                        }
                    } else {
                        positional += 1;
                        if parameter.default().is_none() {
                            required += 1;
                        }
                    }
                }
                ast::AnyParam::Star(_) => {
                    variadic = true;
                    keyword_only = true;
                }
                ast::AnyParam::StarSep(_) => keyword_only = true,
                ast::AnyParam::StarStar(_) | ast::AnyParam::Slash(_) => {}
            }
        }
        required <= 4 && (variadic || positional >= 4)
    }

    fn context_literal_suppression(
        &self,
        mro: &[KnownClass<'_>],
        member: &str,
        async_with: bool,
    ) -> ContextSuppression {
        for class in mro {
            if class.intrinsic {
                continue;
            }
            let ids = class.file.scopes.bindings_in(class.scope, member);
            if ids.is_empty() {
                continue;
            }
            if ids.len() != 1 {
                return ContextSuppression::Unknown;
            }
            let binding = class.file.scopes.binding(ids[0]);
            if !matches!(binding.kind, BindingKind::Function) {
                return ContextSuppression::Unknown;
            }
            let Some(function) = node_at(
                &class.file.parse.syntax_node(),
                binding.node_range,
                SyntaxKind::FUNC_DEF,
            )
            .and_then(ast::FuncDef::cast) else {
                return ContextSuppression::Unknown;
            };
            if function.decorators().next().is_some()
                || function.is_async() != async_with
                || !Self::ordinary_exit_accepts_arguments(&function)
            {
                return ContextSuppression::Unknown;
            }
            let statements: Vec<_> = function
                .body()
                .into_iter()
                .flat_map(|body| body.statements())
                .take(2)
                .collect();
            let [ast::Stmt::Return(statement)] = statements.as_slice() else {
                return ContextSuppression::Unknown;
            };
            return match statement.value().map(ast::Expr::unparen) {
                Some(ast::Expr::Literal(value)) if value.kind() == Some(SyntaxKind::FALSE_KW) => {
                    ContextSuppression::False
                }
                Some(ast::Expr::Literal(value)) if value.kind() == Some(SyntaxKind::TRUE_KW) => {
                    ContextSuppression::True
                }
                _ => ContextSuppression::Unknown,
            };
        }
        ContextSuppression::Unknown
    }

    fn suppress_call(
        &self,
        file: &FileInfo,
        scope: ScopeId,
        context: ast::Expr,
        before: rustpy_core::TextSize,
        seen: &mut FxHashSet<BindingId>,
    ) -> Option<(ast::CallExpr, ScopeId)> {
        match context.unparen() {
            ast::Expr::Call(call) => Some((call, scope)),
            ast::Expr::Name(name) => {
                let ids = self.value_binding_ids(file, scope, &name, before)?;
                if ids.len() != 1 || seen.len() >= 16 || !seen.insert(ids[0]) {
                    return None;
                }
                let binding = file.scopes.binding(ids[0]);
                if !matches!(binding.kind, BindingKind::Assignment) {
                    return None;
                }
                self.suppress_call(
                    file,
                    binding.scope,
                    self.expression_at(file, binding.value?)?,
                    binding.range.start(),
                    seen,
                )
            }
            _ => None,
        }
    }

    fn canonical_suppress_exceptions(&self, context: &ast::Expr) -> Option<Vec<SmolStr>> {
        let (call, scope) = self.suppress_call(
            self.file,
            self.scope,
            context.clone(),
            context.syntax().text_range().start(),
            &mut FxHashSet::default(),
        )?;
        let (name, member) = match call.callee()?.unparen() {
            ast::Expr::Name(name) => (name, None),
            ast::Expr::Attribute(attribute) => {
                let ast::Expr::Name(name) = attribute.object()?.unparen() else {
                    return None;
                };
                (name, Some(attribute.attr_text()))
            }
            _ => return None,
        };
        let ids =
            self.value_binding_ids(self.file, scope, &name, call.syntax().text_range().start())?;
        if ids.len() != 1 || self.authority.retired_members.contains("suppress") {
            return None;
        }
        let binding = self.file.scopes.binding(ids[0]);
        let BindingKind::Import(import) = &binding.kind else {
            return None;
        };
        if !matches!(
            (member.as_deref(), import.name.as_deref()),
            (Some("suppress"), None) | (None, Some("suppress"))
        ) {
            return None;
        }
        let target = self
            .authority
            .project_imports
            .get(&(self.file.id, ids[0]))?;
        if !target.is_stub
            || target.module_name.as_deref() != Some("contextlib")
            || target.path != std::path::Path::new("<typeshed>/contextlib.pyi")
        {
            return None;
        }
        let arguments: Vec<_> = call
            .arg_list()
            .into_iter()
            .flat_map(|arguments| arguments.args())
            .take(17)
            .collect();
        if arguments.len() > 16 {
            return None;
        }
        let mut names = Vec::new();
        for argument in arguments {
            let ast::AnyArg::Positional(argument) = argument else {
                return None;
            };
            let ast::Expr::Name(name) = argument.expr()?.unparen() else {
                return None;
            };
            if name
                .name_token()
                .is_some_and(|token| self.info.refs.contains_key(&token.text_range().start()))
            {
                // Captured unknown prepared mappings/lexical bindings cannot
                // be substituted with builtin exception spelling.
                return None;
            }
            let name = name.name_text();
            // Deliberately finite builtin exception identities; shadowing or
            // star imports removes the canonical proof, never name-only trust.
            if !matches!(
                name.as_str(),
                "BaseException"
                    | "SystemExit"
                    | "KeyboardInterrupt"
                    | "GeneratorExit"
                    | "Exception"
                    | "RuntimeError"
                    | "ValueError"
                    | "TypeError"
                    | "NameError"
                    | "UnboundLocalError"
                    | "KeyError"
                    | "IndexError"
                    | "LookupError"
                    | "OSError"
                    | "ZeroDivisionError"
                    | "ArithmeticError"
                    | "AssertionError"
                    | "StopIteration"
            ) || self.file.scopes.resolve(scope, &name).is_some()
                || self
                    .file
                    .scopes
                    .scopes
                    .iter()
                    .any(|scope| !scope.star_imports.is_empty())
            {
                return None;
            }
            names.push(SmolStr::new(name));
        }
        Some(names)
    }

    pub(super) fn context_declared_contract(
        &self,
        context: &ast::Expr,
        state: &State,
        async_with: bool,
    ) -> ContextContract {
        if !async_with {
            if let Some(exceptions) = self.canonical_suppress_exceptions(context) {
                return ContextContract {
                    entry_terminal: false,
                    exit_terminal: false,
                    suppression: ContextSuppression::Exceptions(exceptions),
                    exit_may_raise: false,
                };
            }
        }
        let Some((mro, exact_receiver)) = self.context_mro(context, state) else {
            return ContextContract::default();
        };
        let entry = if async_with {
            "__aenter__"
        } else {
            "__enter__"
        };
        let exit = if async_with { "__aexit__" } else { "__exit__" };
        let (file, class) = (mro[0].file, mro[0].scope);
        let class = node_at(
            &file.parse.syntax_node(),
            file.scopes.scope(class).range,
            SyntaxKind::CLASS_DEF,
        )
        .and_then(ast::ClassDef::cast);
        let Some(class) = class else {
            return ContextContract::default();
        };
        let suppression = if !exact_receiver || self.authority.retired_members.contains(exit) {
            ContextSuppression::Unknown
        } else {
            self.context_literal_suppression(&mro, exit, async_with)
        };
        let exit_may_raise = matches!(suppression, ContextSuppression::Unknown);
        ContextContract {
            entry_terminal: self
                .receiver_method_contract(file, class.clone(), entry, true)
                .is_some_and(|contract| contract.terminates(async_with)),
            exit_terminal: self
                .receiver_method_contract(file, class, exit, true)
                .is_some_and(|contract| contract.terminates(async_with)),
            suppression,
            exit_may_raise,
        }
    }

    fn method_contract(
        &self,
        file: &FileInfo,
        scope: ScopeId,
        name: &str,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
    ) -> Option<Contract> {
        let ids = file.scopes.bindings_in(scope, name);
        if ids.len() > 1 {
            return self.overload_group_contract(file, ids[0], true).flatten();
        }
        if ids.len() != 1 || !matches!(file.scopes.binding(ids[0]).kind, BindingKind::Function) {
            return None;
        }
        self.contract(file, ids[0], None, seen)
    }

    fn class_mro<'a>(
        &'a self,
        file: &'a FileInfo,
        class: ast::ClassDef,
        seen: &mut FxHashSet<(rustpy_core::FileId, ScopeId)>,
        budget: &mut usize,
    ) -> Option<Vec<KnownClass<'a>>> {
        // Admit only a bounded direct argument prefix before collecting
        // MRO input sequences; repeated intrinsic object bases charge this
        // bound too, rather than bypassing the declared-class node budget.
        let arguments: Vec<_> = class
            .arguments()
            .into_iter()
            .flat_map(|arguments| arguments.args())
            .take(17)
            .collect();
        if arguments.len() > 16
            || arguments
                .iter()
                .any(|argument| !matches!(argument, ast::AnyArg::Positional(_)))
        {
            return None;
        }
        let scope = self.class_shape(file, &class)?;
        if *budget >= 16 || !seen.insert((file.id, scope)) {
            return None;
        }
        *budget += 1;
        let result = (|| {
            let mut bases = Vec::new();
            let mut sequences = Vec::new();
            let defining = file.scopes.scope(scope).parent?;
            for argument in arguments {
                let ast::AnyArg::Positional(argument) = argument else {
                    return None;
                };
                let base = argument.expr()?;
                let (name, attribute) = match base.unparen() {
                    ast::Expr::Name(name) => (name, None),
                    ast::Expr::Attribute(attribute) => {
                        let ast::Expr::Name(name) = attribute.object()?.unparen() else {
                            return None;
                        };
                        (name, Some(attribute.attr_text()))
                    }
                    _ => return None,
                };
                // Include intrinsic object in C3 ordering, even though its
                // slots cannot supply a Never contract in this proof.
                if attribute.is_none()
                    && name.name_text() == "object"
                    && file.scopes.resolve(defining, "object").is_none()
                    && file
                        .scopes
                        .scopes
                        .iter()
                        .all(|scope| scope.star_imports.is_empty())
                {
                    let object = KnownClass {
                        file,
                        scope: ScopeId(0),
                        intrinsic: true,
                    };
                    bases.push(object.clone());
                    sequences.push(vec![object]);
                    continue;
                }
                let ids = self.value_binding_ids(
                    file,
                    defining,
                    &name,
                    class.syntax().text_range().start(),
                )?;
                if ids.len() != 1 {
                    return None;
                }
                let (base_file, base_class) = self.class_from_binding(
                    file,
                    ids[0],
                    attribute.as_deref(),
                    &mut FxHashSet::default(),
                )?;
                let base_mro = self.class_mro(base_file, base_class, seen, budget)?;
                let first = base_mro.first()?.clone();
                if bases
                    .iter()
                    .any(|base: &KnownClass<'_>| base.identity() == first.identity())
                {
                    return None;
                }
                bases.push(first);
                sequences.push(base_mro);
            }
            if bases.is_empty() {
                let object = KnownClass {
                    file,
                    scope: ScopeId(0),
                    intrinsic: true,
                };
                bases.push(object.clone());
                sequences.push(vec![object]);
            }
            sequences.push(bases);
            let mut mro = vec![KnownClass {
                file,
                scope,
                intrinsic: false,
            }];
            while sequences.iter().any(|sequence| !sequence.is_empty()) {
                let candidate = sequences
                    .iter()
                    .filter_map(|sequence| sequence.first())
                    .find(|candidate| {
                        sequences.iter().all(|sequence| {
                            !sequence
                                .iter()
                                .skip(1)
                                .any(|later| later.identity() == candidate.identity())
                        })
                    })?
                    .clone();
                mro.push(candidate.clone());
                if mro.len() > 16 {
                    return None;
                }
                for sequence in &mut sequences {
                    if sequence
                        .first()
                        .is_some_and(|first| first.identity() == candidate.identity())
                    {
                        sequence.remove(0);
                    }
                }
            }
            Some(mro)
        })();
        seen.remove(&(file.id, scope));
        result
    }

    fn mro_method_contract(
        &self,
        mro: &[KnownClass<'_>],
        name: &str,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
    ) -> Option<Contract> {
        for class in mro {
            if class.intrinsic {
                continue;
            }
            if !class.file.scopes.bindings_in(class.scope, name).is_empty() {
                return self.method_contract(class.file, class.scope, name, seen);
            }
        }
        None
    }

    // Constructor-only topology. With/instance dispatch keeps class_mro's
    // stricter no-metaclass rule; this traversal never loosens that API.
    fn constructor_selection_mro<'a>(
        &'a self,
        file: &'a FileInfo,
        class: ast::ClassDef,
        metaclass_role: bool,
        seen: &mut FxHashSet<(rustpy_core::FileId, ScopeId)>,
        budget: &mut usize,
    ) -> Option<Vec<KnownClass<'a>>> {
        // Admit only a bounded direct argument prefix before collecting
        // MRO input sequences; repeated intrinsic object bases charge this
        // bound too, rather than bypassing the declared-class node budget.
        let arguments: Vec<_> = class
            .arguments()
            .into_iter()
            .flat_map(|arguments| arguments.args())
            .take(17)
            .collect();
        if arguments.len() > 16
            || arguments
                .iter()
                .any(|argument| !matches!(argument, ast::AnyArg::Positional(_)) && !matches!(argument, ast::AnyArg::Keyword(keyword) if !metaclass_role && keyword.name_text() == "metaclass"))
        {
            return None;
        }
        let scope = self.class_shape(file, &class)?;
        if *budget >= 16 || !seen.insert((file.id, scope)) {
            return None;
        }
        *budget += 1;
        let result = (|| {
            let mut bases = Vec::new();
            let mut sequences = Vec::new();
            let defining = file.scopes.scope(scope).parent?;
            for argument in arguments {
                let ast::AnyArg::Positional(argument) = argument else {
                    continue; // Exact metaclass keyword is checked by selection.
                };
                let base = argument.expr()?;
                let (name, attribute) = match base.unparen() {
                    ast::Expr::Name(name) => (name, None),
                    ast::Expr::Attribute(attribute) => {
                        let ast::Expr::Name(name) = attribute.object()?.unparen() else {
                            return None;
                        };
                        (name, Some(attribute.attr_text()))
                    }
                    _ => return None,
                };
                // Include intrinsic object in C3 ordering, even though its
                // slots cannot supply a Never contract in this proof.
                if metaclass_role && self.authority.retired_members.contains("type") {
                    return None;
                }
                if attribute.is_none()
                    && name.name_text() == if metaclass_role { "type" } else { "object" }
                    && file
                        .scopes
                        .resolve(defining, if metaclass_role { "type" } else { "object" })
                        .is_none()
                    && file
                        .scopes
                        .scopes
                        .iter()
                        .all(|scope| scope.star_imports.is_empty())
                {
                    let object = KnownClass {
                        file,
                        scope: ScopeId(0),
                        intrinsic: true,
                    };
                    bases.push(object.clone());
                    sequences.push(vec![object]);
                    continue;
                }
                let ids = self.value_binding_ids(
                    file,
                    defining,
                    &name,
                    class.syntax().text_range().start(),
                )?;
                if ids.len() != 1 {
                    return None;
                }
                let (base_file, base_class) = self.class_from_binding(
                    file,
                    ids[0],
                    attribute.as_deref(),
                    &mut FxHashSet::default(),
                )?;
                let base_mro = self.constructor_selection_mro(
                    base_file,
                    base_class,
                    metaclass_role,
                    seen,
                    budget,
                )?;
                let first = base_mro.first()?.clone();
                if bases
                    .iter()
                    .any(|base: &KnownClass<'_>| base.identity() == first.identity())
                {
                    return None;
                }
                bases.push(first);
                sequences.push(base_mro);
            }
            if bases.is_empty() {
                if metaclass_role {
                    return None;
                }
                let object = KnownClass {
                    file,
                    scope: ScopeId(0),
                    intrinsic: true,
                };
                bases.push(object.clone());
                sequences.push(vec![object]);
            }
            sequences.push(bases);
            let mut mro = vec![KnownClass {
                file,
                scope,
                intrinsic: false,
            }];
            while sequences.iter().any(|sequence| !sequence.is_empty()) {
                let candidate = sequences
                    .iter()
                    .filter_map(|sequence| sequence.first())
                    .find(|candidate| {
                        sequences.iter().all(|sequence| {
                            !sequence
                                .iter()
                                .skip(1)
                                .any(|later| later.identity() == candidate.identity())
                        })
                    })?
                    .clone();
                mro.push(candidate.clone());
                if mro.len() > 16 {
                    return None;
                }
                for sequence in &mut sequences {
                    if sequence
                        .first()
                        .is_some_and(|first| first.identity() == candidate.identity())
                    {
                        sequence.remove(0);
                    }
                }
            }
            Some(mro)
        })();
        seen.remove(&(file.id, scope));
        result
    }

    // Select the actual most-derived compatible metaclass from captured
    // nominal bases. A returned None inside Some means intrinsic type;
    // outer None means unknown/conflicting authority, never a fallback.
    fn selected_constructor_metaclass<'a>(
        &'a self,
        file: &'a FileInfo,
        class: ast::ClassDef,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
    ) -> Option<Option<Vec<KnownClass<'a>>>> {
        let nominal =
            self.constructor_selection_mro(file, class, false, &mut FxHashSet::default(), &mut 0)?;
        let mut selected: Option<Vec<KnownClass<'a>>> = None;
        for base in nominal {
            if base.intrinsic {
                continue;
            }
            let class = node_at(
                &base.file.parse.syntax_node(),
                base.file.scopes.scope(base.scope).range,
                SyntaxKind::CLASS_DEF,
            )
            .and_then(ast::ClassDef::cast)?;
            let defining = base.file.scopes.scope(base.scope).parent?;
            // Base namespace hooks can mutate a constructed child or selection.
            if !base
                .file
                .scopes
                .bindings_in(base.scope, "__init_subclass__")
                .is_empty()
                || self.authority.retired_members.contains("__init_subclass__")
            {
                return None;
            }
            let keywords: Vec<_> = class
                .arguments()
                .into_iter()
                .flat_map(|arguments| arguments.keywords())
                .collect();
            if keywords.len() > 1 {
                return None;
            }
            let Some(meta) = keywords.first() else {
                continue;
            };
            if meta.name_text() != "metaclass" {
                return None;
            }
            if self.authority.retired_members.contains("type") {
                return None;
            }
            let (name, member) = match meta.value()?.unparen() {
                ast::Expr::Name(name) => (name, None),
                ast::Expr::Attribute(attribute) => {
                    let ast::Expr::Name(name) = attribute.object()?.unparen() else {
                        return None;
                    };
                    (name, Some(attribute.attr_text()))
                }
                _ => return None,
            };
            if member.is_none()
                && name.name_text() == "type"
                && base.file.scopes.resolve(defining, "type").is_none()
                && base
                    .file
                    .scopes
                    .scopes
                    .iter()
                    .all(|scope| scope.star_imports.is_empty())
            {
                continue;
            }
            let ids =
                self.value_binding_ids(base.file, defining, &name, class.text_range().start())?;
            if ids.len() != 1 {
                return None;
            }
            let (meta_file, meta_class) =
                self.class_from_binding(base.file, ids[0], member.as_deref(), seen)?;
            let candidate = self.constructor_selection_mro(
                meta_file,
                meta_class,
                true,
                &mut FxHashSet::default(),
                &mut 0,
            )?;
            for meta in candidate.iter().filter(|meta| !meta.intrinsic) {
                for member in ["__prepare__", "__new__", "__init__", "__init_subclass__"] {
                    if self.authority.retired_members.contains(member)
                        || !meta.file.scopes.bindings_in(meta.scope, member).is_empty()
                    {
                        return None;
                    }
                }
            }
            selected = Some(match selected {
                None => candidate,
                Some(previous) => {
                    if previous
                        .iter()
                        .any(|meta| meta.identity() == candidate[0].identity())
                    {
                        previous
                    } else if candidate
                        .iter()
                        .any(|meta| meta.identity() == previous[0].identity())
                    {
                        candidate
                    } else {
                        return None;
                    }
                }
            });
        }
        Some(selected)
    }

    fn direct_metaclass_constructor_contract(
        &self,
        file: &FileInfo,
        class: &ast::ClassDef,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
    ) -> Option<Contract> {
        let metaclass = self.selected_constructor_metaclass(file, class.clone(), seen)??;
        match self.mro_method_contract(&metaclass, "__call__", seen)? {
            Contract::Sync => Some(Contract::Sync),
            Contract::Async => None,
        }
    }

    pub(super) fn declared_constructor_contract(
        &self,
        file: &FileInfo,
        id: BindingId,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
    ) -> Option<Contract> {
        let class = node_at(
            &file.parse.syntax_node(),
            file.scopes.binding(id).node_range,
            SyntaxKind::CLASS_DEF,
        )
        .and_then(ast::ClassDef::cast)?;
        if self
            .selected_constructor_metaclass(file, class.clone(), seen)?
            .is_some()
        {
            return self.direct_metaclass_constructor_contract(file, &class, seen);
        }
        let mro = self.class_mro(file, class, &mut FxHashSet::default(), &mut 0)?;
        let custom_new = mro.iter().any(|class| {
            !class.intrinsic
                && !class
                    .file
                    .scopes
                    .bindings_in(class.scope, "__new__")
                    .is_empty()
        });
        // A custom __new__ may return a foreign object. Only its own
        // synchronous Never contract can terminate construction in this proof.
        let contract = if custom_new {
            self.mro_method_contract(&mro, "__new__", seen)?
        } else {
            self.mro_method_contract(&mro, "__init__", seen)?
        };
        match contract {
            Contract::Sync => Some(Contract::Sync),
            Contract::Async => None,
        }
    }

    pub(super) fn declared_instance_contract(
        &self,
        file: &FileInfo,
        id: BindingId,
        call: &ast::CallExpr,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
    ) -> Option<Contract> {
        let binding = file.scopes.binding(id);
        let (name, attribute) = match call.callee()?.unparen() {
            ast::Expr::Name(name) => (name, None),
            ast::Expr::Attribute(attribute) => {
                let ast::Expr::Name(name) = attribute.object()?.unparen() else {
                    return None;
                };
                (name, Some(attribute.attr_text()))
            }
            _ => return None,
        };
        let ids = self.value_binding_ids(file, binding.scope, &name, binding.range.start())?;
        if ids.len() != 1 {
            return None;
        }
        let (class_file, class) = self.class_from_binding(
            file,
            ids[0],
            attribute.as_deref(),
            &mut FxHashSet::default(),
        )?;
        let mro = self.class_mro(class_file, class, &mut FxHashSet::default(), &mut 0)?;
        if mro.iter().any(|class| {
            !class.intrinsic
                && !class
                    .file
                    .scopes
                    .bindings_in(class.scope, "__new__")
                    .is_empty()
        }) {
            return None;
        }
        self.mro_method_contract(&mro, "__call__", seen)
    }
}

#[derive(Clone)]
struct KnownClass<'a> {
    file: &'a FileInfo,
    scope: ScopeId,
    intrinsic: bool,
}
impl KnownClass<'_> {
    fn identity(&self) -> (bool, rustpy_core::FileId, ScopeId) {
        if self.intrinsic {
            (true, rustpy_core::FileId(0), ScopeId(0))
        } else {
            (false, self.file.id, self.scope)
        }
    }
}

#[derive(Clone, Copy)]
struct IndexedAnnotationProjection {
    index: i64,
    before: rustpy_core::TextSize,
}

#[derive(Clone, Copy)]
struct FactoryAnnotationProjection {
    awaited: bool,
    before: rustpy_core::TextSize,
}
