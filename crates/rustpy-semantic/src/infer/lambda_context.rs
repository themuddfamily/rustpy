//! Lambda parameters typed by their call-site context when queried directly
//! (hover, completion, attribute checks), outside the isolated expected-type
//! session: `sorted(xs, key=lambda p: p.name)`, `map(lambda s: s.upper(), strs)`,
//! `f: Callable[[int], str] = lambda x: ...`. The result is cached with the
//! binding like any other parameter type.
use super::*;
impl Inferer<'_> {
    pub(in crate::infer) fn contextual_lambda_parameter(
        &self,
        file: &Arc<FileInfo>,
        binding: &crate::scope::Binding,
    ) -> Option<Type> {
        // Inside an isolated contextual session the session's overrides own
        // lambda parameters; never reenter parent adaptation from there.
        if self.expected_context.active.get() || binding.kind != BindingKind::Param {
            return None;
        }
        let scope = file.scopes.scope(binding.scope);
        if scope.kind != ScopeKind::Lambda {
            return None;
        }
        let lambda = file
            .parse
            .syntax_node()
            .covering_element(scope.range)
            .into_node()?
            .ancestors()
            .take(4)
            .find_map(ast::LambdaExpr::cast)
            .filter(|lambda| lambda.text_range() == scope.range)?;
        let parameters = lambda
            .params()
            .map(|p| self.params_from_list(file, &p, None, false))
            .unwrap_or_default();
        if parameters.len() > 32 {
            return None;
        }
        let parameter = parameters.iter().find(|p| p.name == binding.name)?;
        let expected = self.lambda_expected_type(file, &lambda)?;
        let signature = callable_member(&expected)?;
        if signature.params.len() > 32 {
            return None;
        }
        let target = match parameter.kind {
            ParamKind::Normal | ParamKind::PositionalOnly => {
                let index = parameters
                    .iter()
                    .filter(|p| matches!(p.kind, ParamKind::Normal | ParamKind::PositionalOnly))
                    .position(|p| p.name == parameter.name)?;
                super::parameter_at_position(&signature, index)?
            }
            ParamKind::KeywordOnly => signature
                .params
                .iter()
                .find(|p| p.kind != ParamKind::PositionalOnly && p.name == parameter.name)?,
            _ => return None,
        };
        // Unsolved type variables carry no evidence about the argument.
        let ty = self
            .normalize_class_objects(target.ty.clone())
            .substitute(&|_| Some(Type::Unknown));
        (!ty.is_unknown()).then_some(ty)
    }

    /// The callable type `lambda` is expected to have: an annotated
    /// assignment or return, or the (solved) parameter it is passed to.
    fn lambda_expected_type(&self, file: &Arc<FileInfo>, lambda: &ast::LambdaExpr) -> Option<Type> {
        let mut range = lambda.text_range();
        for node in lambda.syntax().ancestors().skip(1).take(8) {
            if node.kind() == SyntaxKind::PAREN_EXPR {
                range = node.text_range();
                continue;
            }
            if let Some(assign) = ast::AnnAssignStmt::cast(node.clone()) {
                if assign.value()?.text_range() != range {
                    return None;
                }
                return Some(self.annotation_type(file, &assign.annotation()?));
            }
            if let Some(statement) = ast::ReturnStmt::cast(node.clone()) {
                if statement.value()?.text_range() != range {
                    return None;
                }
                let function = node.ancestors().skip(1).find_map(ast::FuncDef::cast)?;
                return Some(self.annotation_type(file, &function.return_annotation()?));
            }
            if let Some(call) = ast::CallExpr::cast(node.clone()) {
                return self.lambda_argument_expected_type(file, &call, range);
            }
            // Only the argument list may separate a lambda from its call.
            if node.kind() != SyntaxKind::ARG_LIST && ast::AnyArg::cast(node).is_none() {
                return None;
            }
        }
        None
    }

    fn lambda_argument_expected_type(
        &self,
        file: &Arc<FileInfo>,
        call: &ast::CallExpr,
        range: TextRange,
    ) -> Option<Type> {
        let arguments: Vec<_> = call.arg_list()?.args().take(33).collect();
        if arguments.len() > 32 {
            return None;
        }
        // Lambda arguments (this one included) are not evidence for overload
        // selection or type-variable solving: inferring them would reenter
        // their own parameters. The remaining arguments decide.
        let mut pos = Vec::new();
        let mut kw = Vec::new();
        let mut target = None;
        for argument in &arguments {
            let value = match argument {
                ast::AnyArg::Positional(p) => p.expr(),
                ast::AnyArg::Keyword(k) => k.value(),
                _ => return None,
            }?;
            let ty = if matches!(value.clone().unparen(), ast::Expr::Lambda(_)) {
                Type::Unknown
            } else {
                self.type_of_expr(file, &value)
            };
            let hit = value.text_range() == range;
            match argument {
                ast::AnyArg::Positional(_) => {
                    if hit {
                        target = Some((Some(pos.len()), None));
                    }
                    pos.push(ty);
                }
                ast::AnyArg::Keyword(k) => {
                    let name = SmolStr::new(k.name_text());
                    if hit {
                        target = Some((None, Some(name.clone())));
                    }
                    kw.push((name, ty));
                }
                _ => return None,
            }
        }
        let (position, keyword) = target?;
        let callee = self.type_of_expr(file, &call.callee()?);
        let callee = match callee {
            Type::Class(class, _) => self.constructor(&class)?,
            other => other,
        };
        let signatures: Vec<Arc<Signature>> = match &callee {
            Type::Function(sig) | Type::Callable(sig) | Type::BoundMethod(sig, _) => {
                vec![sig.clone()]
            }
            Type::Overloaded(sigs) => sigs.clone(),
            _ => return None,
        };
        if signatures.len() > 32 {
            return None;
        }
        let parameter_of = |signature: &Arc<Signature>| -> Option<Type> {
            let parameter = match (&position, &keyword) {
                (Some(index), _) => super::parameter_at_position(signature, *index),
                (None, Some(name)) => signature
                    .params
                    .iter()
                    .find(|p| p.kind != ParamKind::PositionalOnly && p.name == *name),
                _ => None,
            }?;
            callable_member(&parameter.ty).map(|_| parameter.ty.clone())
        };
        // The first applicable overload whose matching parameter takes a
        // callable (`sorted(xs, key=...)` skips the `key: None` overload).
        let (signature, parameter) = signatures.iter().find_map(|signature| {
            if signatures.len() > 1 && !self.arguments_fit(signature, &pos, &kw) {
                return None;
            }
            parameter_of(signature).map(|parameter| (signature, parameter))
        })?;
        let mut substitutions = FxHashMap::default();
        self.solve_typevars(&signature.params, &pos, &kw, &mut substitutions);
        self.solve_signature_defaults(signature, &mut substitutions);
        Some(parameter.substitute(&|key| substitutions.get(key).cloned()))
    }
}

/// The callable signature of `ty`, or of the first callable member of a union
/// (`Callable[[T], R] | None`).
fn callable_member(ty: &Type) -> Option<Arc<Signature>> {
    match ty {
        Type::Function(sig) | Type::Callable(sig) => Some(sig.clone()),
        Type::Union(members) => members.iter().take(32).find_map(callable_member),
        _ => None,
    }
}
