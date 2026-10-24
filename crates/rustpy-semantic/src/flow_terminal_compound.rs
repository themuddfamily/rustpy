//! Captured compound receiver authority, without inference or executing callbacks.
use super::*;
impl Walker<'_> {
    pub(in super::super) fn compound_receiver_contract(
        &self,
        attribute: &ast::AttributeExpr,
        state: &State,
    ) -> Option<Contract> {
        let object = attribute.object()?.unparen();
        let member = attribute.attr_text();
        match object {
            ast::Expr::Call(call) => self.intrinsic_super_contract(&call, &member, state),
            ast::Expr::Attribute(field) => self.initialized_field_contract(&field, &member, state),
            _ => None,
        }
    }
    fn intrinsic_super_contract(
        &self,
        call: &ast::CallExpr,
        member: &str,
        state: &State,
    ) -> Option<Contract> {
        let ast::Expr::Name(name) = call.callee()?.unparen() else {
            return None;
        };
        if name.name_text() != "super"
            || self.file.scopes.resolve(self.scope, "super").is_some()
            || self.authority.retired_members.contains("super")
        {
            return None;
        }
        let mut cursor = Some(self.scope);
        while let Some(scope) = cursor {
            let info = self.file.scopes.scope(scope);
            if !info.star_imports.is_empty() {
                return None;
            }
            cursor = info.parent;
        }
        let function_scope = self.file.scopes.scope(self.scope);
        if function_scope.kind != crate::scope::ScopeKind::Function {
            return None;
        }
        let class_scope = function_scope.parent?;
        if self.file.scopes.scope(class_scope).kind != crate::scope::ScopeKind::Class {
            return None;
        }
        let class = node_at(
            &self.file.parse.syntax_node(),
            self.file.scopes.scope(class_scope).range,
            SyntaxKind::CLASS_DEF,
        )
        .and_then(ast::ClassDef::cast)?;
        let function = node_at(
            &self.file.parse.syntax_node(),
            function_scope.range,
            SyntaxKind::FUNC_DEF,
        )
        .and_then(ast::FuncDef::cast)?;
        if function.decorators().next().is_some() {
            return None;
        }
        let first = function.params()?.params().next()?;
        let receiver = first.name()?;
        // A reassigned first parameter or alternate runtime receiver cannot
        // provide zero-arg super's lexical class/instance authority.
        let ids = self.file.scopes.bindings_in(self.scope, receiver.text());
        if ids.len() != 1 || !matches!(self.file.scopes.binding(ids[0]).kind, BindingKind::Param) {
            return None;
        }
        if self
            .reaching_name(receiver.text(), state)
            .is_some_and(|values| {
                values
                    .iter()
                    .any(|value| value.binding != Some(ids[0]) || !value.preds.is_empty())
            })
        {
            return None;
        }
        let arguments: Vec<_> = call
            .arg_list()
            .into_iter()
            .flat_map(|args| args.args())
            .take(3)
            .collect();
        if !arguments.is_empty() {
            let [ast::AnyArg::Positional(first), ast::AnyArg::Positional(second)] =
                arguments.as_slice()
            else {
                return None;
            };
            let ast::Expr::Name(class_name) = first.expr()?.unparen() else {
                return None;
            };
            let ast::Expr::Name(instance_name) = second.expr()?.unparen() else {
                return None;
            };
            if instance_name.name_text() != receiver.text() {
                return None;
            }
            let class_ids = self.value_binding_ids(
                self.file,
                self.scope,
                &class_name,
                call.text_range().start(),
            )?;
            if class_ids.len() != 1 {
                return None;
            }
            let (file, actual_class) =
                self.class_from_binding(self.file, class_ids[0], None, &mut FxHashSet::default())?;
            if file.id != self.file.id || actual_class.text_range() != class.text_range() {
                return None;
            }
        }
        let mro = self.class_mro(self.file, class, &mut FxHashSet::default(), &mut 0)?;
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
        if self.authority.retired_members.contains(member) {
            return None;
        }
        self.mro_method_contract(mro.get(1..)?, member, &mut FxHashSet::default())
    }
    fn initialized_field_contract(
        &self,
        field: &ast::AttributeExpr,
        member: &str,
        state: &State,
    ) -> Option<Contract> {
        if self.authority.retired_members.contains(member) {
            return None;
        }
        let mro = self.initialized_field_mro(field, state, 0)?;
        if !self.inert_context_initializer(&mro) {
            return None;
        }
        self.mro_method_contract(&mro, member, &mut FxHashSet::default())
    }
    fn initialized_field_mro<'a>(
        &'a self,
        field: &ast::AttributeExpr,
        state: &State,
        depth: usize,
    ) -> Option<Vec<KnownClass<'a>>> {
        if depth >= 4 {
            return None;
        }
        let field_name = field.attr_text();
        let object = field.object()?.unparen();
        let mro = match object {
            ast::Expr::Attribute(ref previous) => {
                self.initialized_field_mro(previous, state, depth + 1)?
            }
            _ => self.context_mro(&object, state)?.0,
        };
        if ["__setattr__", "__delattr__"].iter().any(|name| {
            self.authority.retired_members.contains(*name)
                || mro.iter().any(|class| {
                    !class.intrinsic && !class.file.scopes.bindings_in(class.scope, name).is_empty()
                })
        }) {
            return None;
        }
        // Properties/class descriptors win over instance initialization proof.
        if mro.iter().any(|class| {
            !class
                .file
                .scopes
                .bindings_in(class.scope, &field_name)
                .is_empty()
        }) {
            return None;
        }
        let initializer = mro.iter().find(|class| {
            !class.intrinsic
                && !class
                    .file
                    .scopes
                    .bindings_in(class.scope, "__init__")
                    .is_empty()
        })?;
        let ids = initializer
            .file
            .scopes
            .bindings_in(initializer.scope, "__init__");
        if ids.len() != 1 {
            return None;
        }
        let binding = initializer.file.scopes.binding(ids[0]);
        let function = node_at(
            &initializer.file.parse.syntax_node(),
            binding.node_range,
            SyntaxKind::FUNC_DEF,
        )
        .and_then(ast::FuncDef::cast)?;
        if function.is_async() || function.decorators().next().is_some() {
            return None;
        }
        let params: Vec<_> = function.params()?.params().take(3).collect();
        if params.len() != 1 {
            return None;
        }
        let receiver = params[0].name()?.text().to_string();
        let statements: Vec<_> = function.body()?.statements().take(2).collect();
        let [ast::Stmt::Assign(assign)] = statements.as_slice() else {
            return None;
        };
        let targets = assign.targets();
        let [target] = targets.as_slice() else {
            return None;
        };
        let ast::Expr::Attribute(target) = target.clone().unparen() else {
            return None;
        };
        if target.attr_text() != field_name
            || !matches!(target.object().map(ast::Expr::unparen),Some(ast::Expr::Name(name)) if name.name_text()==receiver)
        {
            return None;
        }
        // Every other visible store/delete to this spelling retires authority,
        // including writes in callbacks; never infer mutable container contents.
        let mut scanned_files = FxHashSet::default();
        let mut scanned_nodes = 0usize;
        for file in std::iter::once(self.file)
            .chain(self.authority.project_imports.values().map(Arc::as_ref))
        {
            if !scanned_files.insert(file.id) {
                continue;
            }
            if scanned_files.len() > 16 {
                return None;
            }
            for node in file.parse.syntax_node().descendants() {
                scanned_nodes += 1;
                if scanned_nodes > 16_384 {
                    return None;
                }
                let targets = match ast::Stmt::cast(node) {
                    Some(ast::Stmt::Assign(statement)) => statement.targets(),
                    Some(ast::Stmt::AnnAssign(statement)) if statement.value().is_some() => {
                        statement.target().into_iter().collect()
                    }
                    Some(ast::Stmt::AugAssign(statement)) => {
                        statement.target().into_iter().collect()
                    }
                    Some(ast::Stmt::Del(statement)) => statement.targets(),
                    _ => Vec::new(),
                };
                if targets.len() > 16 {
                    return None;
                }
                for written in targets {
                    for raw_node in written.syntax().descendants() {
                        scanned_nodes += 1;
                        if scanned_nodes > 16_384 {
                            return None;
                        }
                        let Some(attribute) = ast::AttributeExpr::cast(raw_node) else {
                            continue;
                        };
                        if attribute.attr_text() == field_name
                            && !(file.id == initializer.file.id
                                && attribute.text_range() == target.text_range())
                        {
                            return None;
                        }
                    }
                }
            }
        }
        let ast::Expr::Call(construction) = assign.value()?.unparen() else {
            return None;
        };
        if construction
            .arg_list()
            .is_some_and(|args| args.args().next().is_some())
        {
            return None;
        }
        let ast::Expr::Name(name) = construction.callee()?.unparen() else {
            return None;
        };
        let ids = self.value_binding_ids(
            initializer.file,
            binding.scope,
            &name,
            assign.text_range().start(),
        )?;
        if ids.len() != 1 {
            return None;
        }
        let (file, class) =
            self.class_from_binding(initializer.file, ids[0], None, &mut FxHashSet::default())?;
        let inner_mro = self.class_mro(file, class.clone(), &mut FxHashSet::default(), &mut 0)?;
        if inner_mro.iter().any(|class| {
            !class.intrinsic
                && !class
                    .file
                    .scopes
                    .bindings_in(class.scope, "__new__")
                    .is_empty()
        }) {
            return None;
        }
        Some(inner_mro)
    }
    pub(super) fn builtin_property_decorator(
        &self,
        file: &FileInfo,
        scope: ScopeId,
        function: &ast::FuncDef,
    ) -> bool {
        let decorators: Vec<_> = function.decorators().take(2).collect();
        let [decorator] = decorators.as_slice() else {
            return false;
        };
        let Some(ast::Expr::Name(name)) = decorator.expr().map(ast::Expr::unparen) else {
            return false;
        };
        if name.name_text() != "property"
            || file.scopes.resolve(scope, "property").is_some()
            || self.authority.retired_members.contains("property")
        {
            return false;
        }
        let mut current = Some(scope);
        while let Some(id) = current {
            let info = file.scopes.scope(id);
            if !info.star_imports.is_empty() {
                return false;
            }
            current = info.parent;
        }
        true
    }
    pub(in super::super::super) fn property_getter_terminal(
        &self,
        attribute: &ast::AttributeExpr,
        state: &State,
    ) -> bool {
        self.property_result_contract(attribute, state, false)
            .is_some_and(|contract| matches!(contract, Contract::Sync))
    }
    pub(in super::super) fn property_callable_contract(
        &self,
        attribute: &ast::AttributeExpr,
        state: &State,
    ) -> Option<Contract> {
        self.property_result_contract(attribute, state, true)
    }
    fn property_result_contract(
        &self,
        attribute: &ast::AttributeExpr,
        state: &State,
        called: bool,
    ) -> Option<Contract> {
        let member = attribute.attr_text();
        if self.authority.retired_members.contains(member.as_str()) {
            return None;
        }
        let object = attribute.object()?.unparen();
        // No receiver evaluation callbacks or unknown descriptor chain in this proof.
        if !matches!(object, ast::Expr::Name(_)) {
            return None;
        }
        let (mro, _) = self.context_mro(&object, state)?;
        for class in mro {
            if class.intrinsic {
                continue;
            }
            let ids = class.file.scopes.bindings_in(class.scope, &member);
            if ids.is_empty() {
                continue;
            }
            if ids.len() != 1 {
                return None;
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
            if !self.builtin_property_decorator(class.file, class.scope, &function)
                || function.is_async()
                || function.syntax().descendants().any(|node| {
                    matches!(
                        node.kind(),
                        SyntaxKind::YIELD_EXPR | SyntaxKind::YIELD_FROM_EXPR
                    )
                })
            {
                return None;
            }
            let annotation = function.return_annotation()?;
            return if called {
                self.callable_annotation_contract(
                    class.file,
                    binding.scope,
                    annotation,
                    &mut FxHashSet::default(),
                    0,
                    binding.range.start(),
                )
            } else {
                self.canonical_type_symbol(
                    class.file,
                    binding.scope,
                    &annotation,
                    &["Never", "NoReturn"],
                )
                .then_some(Contract::Sync)
            };
        }
        None
    }
}
