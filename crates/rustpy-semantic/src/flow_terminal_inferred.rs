//! Bounded callable-body/value proofs using captured source only.
use super::*;
impl Walker<'_> {
    fn inferred_simple_callable(
        &self,
        expression: &ast::Expr,
        state: &State,
        awaited: bool,
    ) -> bool {
        let expression = expression.clone().unparen();
        if !self.conditional_branch_declaration_stable(&expression) {
            return false;
        }
        self.terminal_callable(&expression, state, awaited)
    }
    pub(super) fn inferred_lambda_terminal(
        &self,
        lambda: &ast::LambdaExpr,
        state: &State,
        awaited: bool,
    ) -> bool {
        if lambda
            .params()
            .is_some_and(|parameters| parameters.params().next().is_some())
        {
            return false;
        }
        let Some(ast::Expr::Call(call)) = lambda.body().map(ast::Expr::unparen) else {
            return false;
        };
        if call
            .arg_list()
            .is_some_and(|arguments| arguments.args().next().is_some())
        {
            return false;
        }
        call.callee()
            .is_some_and(|callee| self.inferred_simple_callable(&callee, state, awaited))
    }
    pub(super) fn inferred_boolean_terminal(
        &self,
        boolean: &ast::BoolOpExpr,
        state: &State,
        awaited: bool,
        depth: usize,
    ) -> bool {
        if depth >= 16 {
            return false;
        }
        let operands: Vec<_> = boolean.operands().take(17).collect();
        if operands.len() < 2 || operands.len() > 16 {
            return false;
        }
        // Each operand is (terminal callable, always-truthy function value).
        // Unknown operands keep the whole chain conservative.
        let mut proofs = Vec::with_capacity(operands.len());
        for operand in &operands {
            let proof = match operand.clone().unparen() {
                ast::Expr::BoolOp(nested) => (
                    self.inferred_boolean_terminal(&nested, state, awaited, depth + 1),
                    false,
                ),
                expression => (
                    self.inferred_simple_callable(&expression, state, awaited),
                    self.always_truthy_function(&expression, state),
                ),
            };
            if proof == (false, false) {
                return false;
            }
            proofs.push(proof);
        }
        // Mirrors Inferer::bool_op_result: `or` yields the first truthy operand,
        // `and` the first falsy one; functions and classes are always truthy.
        let is_or = boolean.is_or();
        let last = proofs.len() - 1;
        for (index, (terminal, truthy)) in proofs.into_iter().enumerate() {
            if index == last {
                return terminal;
            }
            if is_or {
                if !terminal {
                    return false;
                }
                if truthy {
                    return true;
                }
            } else if !truthy && !terminal {
                return false;
            }
        }
        false
    }
    /// A lambda, or a name whose every reaching binding is a `def`/`class`
    /// statement: such values are always truthy.
    fn always_truthy_function(&self, expression: &ast::Expr, state: &State) -> bool {
        let name = match expression {
            ast::Expr::Lambda(_) => return true,
            ast::Expr::Name(name) => name.name_text(),
            _ => return false,
        };
        if self
            .shadowed
            .iter()
            .any(|names| names.contains(name.as_str()))
        {
            return false;
        }
        let definition = |id: BindingId| {
            matches!(
                self.file.scopes.binding(id).kind,
                BindingKind::Function | BindingKind::Class
            )
        };
        if let Some(reaches) = self.reaching_name(&name, state) {
            return !reaches.is_empty()
                && reaches
                    .iter()
                    .all(|reach| reach.preds.is_empty() && reach.binding.is_some_and(definition));
        }
        self.file
            .scopes
            .resolve(self.scope, &name)
            .is_some_and(|ids| !ids.is_empty() && ids.iter().all(|id| definition(*id)))
    }
    pub(super) fn inferred_factory_terminal(
        &self,
        call: &ast::CallExpr,
        state: &State,
        factory_await: bool,
        callback_await: bool,
    ) -> bool {
        if factory_await
            || call
                .arg_list()
                .is_some_and(|arguments| arguments.args().next().is_some())
        {
            return false;
        }
        let Some(ast::Expr::Name(name)) = call.callee().map(ast::Expr::unparen) else {
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
        let ids = if let Some(reaches) = self.reaching_name(&name, state) {
            if reaches.is_empty()
                || reaches
                    .iter()
                    .any(|reach| reach.binding.is_none() || !reach.preds.is_empty())
            {
                return false;
            }
            reaches
                .iter()
                .filter_map(|reach| reach.binding)
                .collect::<Vec<_>>()
        } else {
            self.file
                .scopes
                .resolve(self.scope, &name)
                .map(|ids| ids.to_vec())
                .unwrap_or_default()
        };
        !ids.is_empty()
            && ids.len() <= 16
            && ids.iter().all(|id| {
                let binding = self.file.scopes.binding(*id);
                if !matches!(binding.kind, BindingKind::Function)
                    || self.file.scopes.scope(binding.scope).kind == crate::scope::ScopeKind::Class
                {
                    return false;
                }
                let Some(function) = node_at(
                    &self.file.parse.syntax_node(),
                    binding.node_range,
                    SyntaxKind::FUNC_DEF,
                )
                .and_then(ast::FuncDef::cast) else {
                    return false;
                };
                if function
                    .syntax()
                    .ancestors()
                    .skip(1)
                    .take_while(|node| {
                        node.text_range() != self.file.scopes.scope(binding.scope).range
                    })
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
                    return false;
                }
                if function.is_async()
                    || function.decorators().next().is_some()
                    || function.return_annotation().is_some()
                    || function
                        .params()
                        .is_some_and(|parameters| parameters.params().next().is_some())
                {
                    return false;
                }
                let Some(body) = function.body() else {
                    return false;
                };
                let statements: Vec<_> = body.statements().take(2).collect();
                let [ast::Stmt::Return(statement)] = statements.as_slice() else {
                    return false;
                };
                let Some(expression) = statement.value() else {
                    return false;
                };
                // Only stable source declarations: unknown aliases/nonlocal writers
                // cannot retarget the returned value during body execution.
                let Some(scope) = self.file.scopes.scope_for_node(function.text_range()) else {
                    return false;
                };
                self.inferred_returned_declaration(&expression, scope, callback_await)
            })
    }
    fn inferred_returned_declaration(
        &self,
        expression: &ast::Expr,
        scope: ScopeId,
        awaited: bool,
    ) -> bool {
        let expression = expression.clone().unparen();
        if !self.conditional_branch_declaration_stable(&expression) {
            return false;
        }
        let (name, member) = match expression {
            ast::Expr::Name(name) => (name, None),
            ast::Expr::Attribute(attribute) => {
                let Some(ast::Expr::Name(name)) = attribute.object().map(ast::Expr::unparen) else {
                    return false;
                };
                (name, Some(attribute.attr_text()))
            }
            _ => return false,
        };
        // This reference executes inside the factory, not its caller. A nested
        // same-spelled declaration in the caller cannot retarget the return.
        let Some(ids) = self.file.scopes.resolve(scope, &name.name_text()) else {
            return false;
        };
        !ids.is_empty()
            && ids.len() <= 16
            && ids.iter().all(|id| {
                self.contract(self.file, *id, member.as_deref(), &mut FxHashSet::default())
                    .is_some_and(|contract| contract.terminates(awaited))
            })
    }

    pub(super) fn inferred_indexed_terminal(
        &self,
        subscript: &ast::SubscriptExpr,
        state: &State,
        awaited: bool,
    ) -> bool {
        let Some(index) = subscript.index_expr().map(ast::Expr::unparen) else {
            return false;
        };
        let (literal, negative) = match index {
            ast::Expr::Literal(literal) => (literal, false),
            ast::Expr::Unary(unary) => {
                let Some(operator) = unary.op() else {
                    return false;
                };
                if !matches!(operator.kind(), SyntaxKind::MINUS | SyntaxKind::PLUS) {
                    return false;
                }
                let Some(ast::Expr::Literal(literal)) = unary.operand().map(ast::Expr::unparen)
                else {
                    return false;
                };
                (literal, operator.kind() == SyntaxKind::MINUS)
            }
            _ => return false,
        };
        if literal.kind() != Some(SyntaxKind::INT_NUMBER) {
            return false;
        }
        let text = literal.syntax().text().to_string();
        if text.len() > 32 {
            return false;
        }
        let digits = text.replace('_', "").to_ascii_lowercase();
        let parsed = if let Some(d) = digits.strip_prefix("0x") {
            i64::from_str_radix(d, 16)
        } else if let Some(d) = digits.strip_prefix("0o") {
            i64::from_str_radix(d, 8)
        } else if let Some(d) = digits.strip_prefix("0b") {
            i64::from_str_radix(d, 2)
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
        match subscript.object().map(ast::Expr::unparen) {
            Some(ast::Expr::List(list)) => {
                let elements: Vec<_> = list.elements().take(17).collect();
                if elements.is_empty()
                    || elements.len() > 16
                    || !elements.iter().all(|element| {
                        self.conditional_branch_declaration_stable(&element.clone().unparen())
                    })
                {
                    return false;
                }
                let index = if index < 0 {
                    elements.len() as i64 + index
                } else {
                    index
                };
                usize::try_from(index)
                    .ok()
                    .and_then(|index| elements.get(index))
                    .is_some_and(|expression| {
                        self.inferred_simple_callable(expression, state, awaited)
                    })
            }
            Some(ast::Expr::Name(name)) => {
                let name = name.name_text();
                if self
                    .shadowed
                    .iter()
                    .any(|names| names.contains(name.as_str()))
                {
                    return false;
                }
                let ids = if let Some(reaches) = self.reaching_name(&name, state) {
                    if reaches.is_empty()
                        || reaches
                            .iter()
                            .any(|reach| reach.binding.is_none() || !reach.preds.is_empty())
                    {
                        return false;
                    }
                    reaches
                        .iter()
                        .filter_map(|reach| reach.binding)
                        .collect::<Vec<_>>()
                } else {
                    self.file
                        .scopes
                        .resolve(self.scope, &name)
                        .map(|ids| ids.to_vec())
                        .unwrap_or_default()
                };
                !ids.is_empty()
                    && ids.len() <= 16
                    && ids.iter().all(|id| {
                        self.declared_member_factory_result_contract(
                            self.file,
                            *id,
                            "__getitem__",
                            false,
                        )
                        .is_some_and(|contract| contract.terminates(awaited))
                    })
            }
            _ => false,
        }
    }
}
