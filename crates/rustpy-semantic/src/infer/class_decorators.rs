//! Actual typed class-decorator application, without synthesizing constructor returns.
use super::*;

impl Inferer<'_> {
    pub(super) fn decorated_class_binding(&self, file: &Arc<FileInfo>, class: ClassRef) -> Type {
        let ordinary = Type::Class(class.clone(), Vec::new());
        let Some(definition) = node_with_range(&file.parse.syntax_node(), class.range, |node| {
            node.kind() == SyntaxKind::CLASS_DEF
        })
        .and_then(ast::ClassDef::cast) else {
            return ordinary;
        };
        let decorators: Vec<_> = definition.decorators().take(33).collect();
        if decorators.len() > 32 {
            return Type::Unknown;
        }
        // Python evaluates expressions top-to-bottom, then applies bottom-to-top.
        let mut values = Vec::with_capacity(decorators.len());
        for decorator in decorators {
            let Some(expression) = decorator.expr() else {
                return Type::Unknown;
            };
            let metadata = self.canonical_typing_class_metadata(file, &expression);
            if metadata == Some(false) {
                return Type::Unknown;
            }
            let dataclass = self.canonical_dataclass_decorator(file, &expression);
            if dataclass
                && Self::namespace_member_retired(
                    file,
                    "dataclass",
                    expression.text_range().start(),
                )
            {
                return Type::Unknown;
            }
            let identity = metadata == Some(true)
                || dataclass
                || self.unresolved_attrs_synthesis_decorator(file, &class, &expression)
                || self.total_ordering_decorator(file, &expression);
            // Identity metadata is handled before signature inference: intrinsic
            // builtins.type/object themselves carry @disjoint_base, and querying
            // its type-bound signature here would re-enter those same classes.
            let value = if identity {
                Type::Unknown
            } else {
                self.type_of_expr(file, &expression)
            };
            values.push((value, identity));
        }
        let mut result = ordinary;
        for (decorator, identity) in values.into_iter().rev() {
            if identity {
                // Existing synthesis owns the fields/init of this same class.
                // Applying a dataclass decorator to a replaced nonclass value
                // cannot manufacture the original class back into existence.
                if !matches!(&result, Type::Class(owner, _) if owner == &class) {
                    return Type::Unknown;
                }
                continue;
            }
            let replacement = self.call_result(&decorator, &[result.clone()], &[]);
            result = if replacement.is_unknown() {
                self.inferred_identity_decorator_result(&decorator, &result)
                    .unwrap_or(replacement)
            } else {
                replacement
            };
        }
        result
    }
    pub(super) fn canonical_dataclass_decorator(
        &self,
        file: &Arc<FileInfo>,
        expression: &ast::Expr,
    ) -> bool {
        let expression = expression.clone().unparen();
        let target = if let ast::Expr::Call(call) = expression {
            if !Self::keyword_only_decorator_factory(&call) {
                return false;
            }
            let Some(callee) = call.callee() else {
                return false;
            };
            callee
        } else {
            expression
        };
        let target = self.type_of_expr(file, &target);
        let canonical = |signature: &Signature| {
            let Some(definition) = &signature.def else {
                return false;
            };
            let Some(owner) = self.db.file(definition.file) else {
                return false;
            };
            owner.is_stub
                && owner.module_name.as_deref() == Some("dataclasses")
                && owner.path == std::path::Path::new("<typeshed>/dataclasses.pyi")
                && definition.name == "dataclass"
        };
        match target {
            Type::Function(signature) | Type::Callable(signature) => canonical(&signature),
            Type::Overloaded(signatures) => {
                !signatures.is_empty()
                    && signatures.len() <= 32
                    && signatures.iter().all(|signature| canonical(signature))
            }
            _ => false,
        }
    }
    fn inferred_identity_decorator_result(
        &self,
        decorator: &Type,
        incoming: &Type,
    ) -> Option<Type> {
        let Type::Function(signature) = decorator else {
            return None;
        };
        if !signature.ret_inferred
            || signature.is_async
            || signature.params.len() != 1
            || !matches!(
                signature.params[0].kind,
                ParamKind::Normal | ParamKind::PositionalOnly
            )
        {
            return None;
        }
        let definition = signature.def.as_ref()?;
        let file = self.db.file(definition.file)?;
        if file.is_stub {
            return None;
        }
        let function = node_with_range(&file.parse.syntax_node(), definition.range, |node| {
            node.kind() == SyntaxKind::FUNC_DEF
        })
        .and_then(ast::FuncDef::cast)?;
        if function.is_async()
            || function.decorators().any(|decorator| {
                decorator.expr().is_none_or(|expression| {
                    self.canonical_typing_class_metadata(&file, &expression) != Some(true)
                        || Self::decorator_target(&expression)
                            .and_then(|target| self.symbol_origin(&file, &target))
                            .is_none_or(|(_, name)| {
                                !matches!(
                                    name.as_str(),
                                    "final" | "type_check_only" | "dataclass_transform"
                                )
                            })
                })
            })
        {
            return None;
        }
        let statements: Vec<_> = function.body()?.statements().take(2).collect();
        let [ast::Stmt::Return(returned)] = statements.as_slice() else {
            return None;
        };
        let ast::Expr::Name(name) = returned.value()?.unparen() else {
            return None;
        };
        let scope = file.scopes.scope_for_node(function.text_range())?;
        let ids = file.scopes.bindings_in(scope, &signature.params[0].name);
        let [parameter] = ids else {
            return None;
        };
        if !matches!(file.scopes.binding(*parameter).kind, BindingKind::Param) {
            return None;
        }
        let actual = self.resolve_name(
            &file,
            self.scope_of(&file, name.syntax()),
            &name.name_text(),
        )?;
        if !Arc::ptr_eq(&actual.file, &file) || actual.binding != *parameter {
            return None;
        }
        // Existing return inference observes an exact argument binding in an
        // isolated session, never mutating ordinary signature/binding caches.
        let session = Inferer::new(self.db);
        session.expected_context.active.set(true);
        session
            .expected_context
            .overrides
            .borrow_mut()
            .insert((file.id, *parameter), incoming.clone());
        Some(session.infer_return_type_static(&file, &function))
    }
    fn decorator_target(expression: &ast::Expr) -> Option<ast::Expr> {
        match expression.clone().unparen() {
            ast::Expr::Call(call) => call.callee(),
            expression => Some(expression),
        }
    }

    pub(super) fn canonical_typing_class_metadata(
        &self,
        file: &Arc<FileInfo>,
        expression: &ast::Expr,
    ) -> Option<bool> {
        let target = Self::decorator_target(expression)?;
        let definition = match target {
            ast::Expr::Name(name) => {
                self.resolve_name(file, self.scope_of(file, name.syntax()), &name.name_text())
            }
            ast::Expr::Attribute(attribute) => attribute.object().and_then(|object| {
                let Type::Module(id) = self.type_of_expr(file, &object) else {
                    return None;
                };
                let module = self.db.file(id)?;
                self.resolve_in_module(&module, &attribute.attr_text(), 0)
            }),
            _ => None,
        };
        let definition = definition.map(|definition| self.follow_imports(definition))?;
        let owner = &definition.file;
        let module = owner.module_name.as_deref();
        let canonical = matches!(module, Some("typing" | "typing_extensions"))
            && owner.is_stub
            && (owner.path == std::path::Path::new("<typeshed>/typing.pyi")
                || owner.path == std::path::Path::new("<typeshed>/typing_extensions.pyi"));
        if !canonical
            || !matches!(definition.binding().kind, BindingKind::Function)
            || !matches!(
                definition.binding().name.as_str(),
                "final"
                    | "runtime_checkable"
                    | "disjoint_base"
                    | "type_check_only"
                    | "dataclass_transform"
            )
        {
            return None;
        }
        if Self::namespace_member_retired(
            file,
            definition.binding().name.as_str(),
            expression.text_range().start(),
        ) {
            return Some(false);
        }
        let factory = definition.binding().name == "dataclass_transform";
        match expression.clone().unparen() {
            ast::Expr::Call(call) if factory => Some(Self::keyword_only_decorator_factory(&call)),
            ast::Expr::Call(_) => None,
            _ if factory => Some(false),
            _ => Some(true),
        }
    }

    pub(super) fn unresolved_attrs_synthesis_decorator(
        &self,
        file: &Arc<FileInfo>,
        class: &ClassRef,
        expression: &ast::Expr,
    ) -> bool {
        if let ast::Expr::Call(call) = expression.clone().unparen() {
            if !Self::keyword_only_decorator_factory(&call) {
                return false;
            }
        }
        let Some(spec) = self.synth_spec(class) else {
            return false;
        };
        if !matches!(spec.kind, SynthKind::Attrs { .. }) {
            return false;
        }
        let Some(target) = Self::decorator_target(expression) else {
            return false;
        };
        let (definition, member) = match target {
            ast::Expr::Name(name) => {
                let Some(definition) =
                    self.resolve_name(file, self.scope_of(file, name.syntax()), &name.name_text())
                else {
                    return false;
                };
                (self.follow_imports(definition), None)
            }
            ast::Expr::Attribute(attribute) => {
                let Some(ast::Expr::Name(name)) = attribute.object() else {
                    return false;
                };
                let Some(definition) =
                    self.resolve_name(file, self.scope_of(file, name.syntax()), &name.name_text())
                else {
                    return false;
                };
                (self.follow_imports(definition), Some(attribute.attr_text()))
            }
            _ => return false,
        };
        let BindingKind::Import(import) = &definition.binding().kind else {
            return false;
        };
        if import.level != 0 || !matches!(import.module.as_str(), "attr" | "attrs") {
            return false;
        }
        let name = match (&import.name, member.as_deref()) {
            (Some(name), None) => name.as_str(),
            (None, Some(member)) => member,
            _ => return false,
        };
        if Self::namespace_member_retired(file, name, expression.text_range().start()) {
            return false;
        }
        matches!(
            name,
            "s" | "attrs" | "attributes" | "dataclass" | "define" | "frozen" | "mutable"
        )
    }
    pub(super) fn namespace_member_retired(
        file: &Arc<FileInfo>,
        member: &str,
        before: TextSize,
    ) -> bool {
        // Immutable embedded declarations are authority, not user mutation sites.
        if file.is_stub
            && matches!(
                file.path.to_str(),
                Some(
                    "<typeshed>/builtins.pyi"
                        | "<typeshed>/typing.pyi"
                        | "<typeshed>/typing_extensions.pyi"
                )
            )
        {
            return false;
        }
        // The bounded scan below answers `true` at the first node that
        // retires the member (or exhausts a budget), whatever `before` is:
        // remember that point once per source version instead of rescanning
        // the file prefix for every decorator/factory query.
        if let Some(point) = file.member_retirements.lock().get(member) {
            return point.is_some_and(|point| point < before);
        }
        let point = Self::member_retirement_point(file, member);
        file.member_retirements
            .lock()
            .insert(SmolStr::new(member), point);
        point.is_some_and(|point| point < before)
    }

    /// Start of the first node (in preorder) at which an earlier write may
    /// have replaced `member`, or at which the scan budget runs out.
    fn member_retirement_point(file: &Arc<FileInfo>, member: &str) -> Option<TextSize> {
        let mut nodes = 16_384usize;
        let mut targets = 4_096usize;
        for node in file.parse.syntax_node().descendants() {
            let point = node.text_range().start();
            if nodes == 0 {
                return Some(point);
            }
            nodes -= 1;
            let written = if let Some(assignment) = ast::AssignStmt::cast(node.clone()) {
                let mut expressions: Vec<_> = assignment
                    .syntax()
                    .children()
                    .filter_map(ast::Expr::cast)
                    .take(targets + 2)
                    .collect();
                if expressions.len() > targets + 1 {
                    return Some(point);
                }
                expressions.pop();
                expressions
            } else if let Some(assignment) = ast::AnnAssignStmt::cast(node.clone()) {
                assignment.target().into_iter().collect()
            } else if let Some(assignment) = ast::AugAssignStmt::cast(node.clone()) {
                assignment.target().into_iter().collect()
            } else if let Some(deletion) = ast::DelStmt::cast(node) {
                deletion
                    .syntax()
                    .children()
                    .find_map(ast::Expr::cast)
                    .into_iter()
                    .collect()
            } else {
                continue;
            };
            for target in written {
                if Self::target_retires_member(target, member, &mut targets, 0) {
                    return Some(point);
                }
            }
        }
        None
    }

    fn target_retires_member(
        target: ast::Expr,
        member: &str,
        budget: &mut usize,
        depth: usize,
    ) -> bool {
        if depth >= 32 || *budget == 0 {
            return true;
        }
        *budget -= 1;
        match target {
            ast::Expr::Paren(paren) => paren.inner().is_none_or(|target| {
                Self::target_retires_member(target, member, budget, depth + 1)
            }),
            ast::Expr::Attribute(attribute) => attribute.attr_text() == member,
            ast::Expr::Tuple(tuple) => tuple
                .elements()
                .any(|target| Self::target_retires_member(target, member, budget, depth + 1)),
            ast::Expr::List(list) => list
                .elements()
                .any(|target| Self::target_retires_member(target, member, budget, depth + 1)),
            ast::Expr::Star(star) => star.expr().is_none_or(|target| {
                Self::target_retires_member(target, member, budget, depth + 1)
            }),
            _ => false,
        }
    }
    fn keyword_only_decorator_factory(call: &ast::CallExpr) -> bool {
        let Some(arguments) = call.arg_list() else {
            return false;
        };
        let mut count = 0usize;
        arguments.args().all(|argument| {
            count += 1;
            count <= 32 && matches!(argument, ast::AnyArg::Keyword(_))
        })
    }
}
