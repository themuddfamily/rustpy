//! Captured operator-produced Callable contracts; no inference or VM.
use super::*;
impl Walker<'_> {
    pub(in super::super) fn operator_callable_terminal(
        &self,
        binary: &ast::BinaryExpr,
        state: &State,
        awaited: bool,
    ) -> bool {
        self.operator_contract(binary, state, true)
            .is_some_and(|contract| contract.terminates(awaited))
    }
    pub(in crate::flow) fn operator_invocation_terminal(
        &self,
        binary: &ast::BinaryExpr,
        state: &State,
    ) -> bool {
        self.operator_contract(binary, state, false)
            .is_some_and(|contract| contract.terminates(false))
    }
    fn operator_contract(
        &self,
        binary: &ast::BinaryExpr,
        state: &State,
        returned_callable: bool,
    ) -> Option<Contract> {
        let operator = binary.op()?.kind();
        let (direct, reflected) = match operator {
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
            _ => return None,
        };
        let lhs = binary.lhs()?.unparen();
        let rhs = binary.rhs()?.unparen();
        if let (ast::Expr::Name(left), ast::Expr::Name(right)) = (&lhs, &rhs) {
            if let (Some((left_mro, left_exact)), Some((right_mro, right_exact))) =
                (self.context_mro(&lhs, state), self.context_mro(&rhs, state))
            {
                if (left.name_text() == right.name_text() || (left_exact && right_exact))
                    && left_mro
                        .iter()
                        .map(KnownClass::identity)
                        .eq(right_mro.iter().map(KnownClass::identity))
                {
                    return self.operator_named_result(
                        &lhs,
                        direct,
                        state,
                        &left_mro,
                        returned_callable,
                    );
                }
            }
        }
        if operator == SyntaxKind::PLUS
            && matches!(rhs, ast::Expr::Name(_))
            && self.captured_exact_integer(&lhs, state)
        {
            let (mro, _) = self.context_mro(&rhs, state)?;
            return self.operator_named_result(&rhs, reflected, state, &mro, returned_callable);
        }
        None
    }
    fn captured_exact_integer(&self, expression: &ast::Expr, state: &State) -> bool {
        match expression.clone().unparen() {
            ast::Expr::Literal(literal) => literal.kind() == Some(SyntaxKind::INT_NUMBER),
            ast::Expr::Name(name) => {
                if self
                    .shadowed
                    .iter()
                    .any(|names| names.contains(name.name_text().as_str()))
                {
                    return false;
                }
                let Some(reaches) = self.reaching_name(&name.name_text(), state) else {
                    return false;
                };
                !reaches.is_empty() && reaches.len() <= 16 && reaches.iter().all(|reach| {
                    let Some(id) = reach.binding else { return false; };
                    if !reach.preds.is_empty() { return false; }
                    let binding = self.file.scopes.binding(id);
                    matches!(binding.kind, BindingKind::Assignment)
                        && binding.value.and_then(|range| self.expression_at(self.file, range))
                            .is_some_and(|value| matches!(value.unparen(), ast::Expr::Literal(literal) if literal.kind() == Some(SyntaxKind::INT_NUMBER)))
                })
            }
            _ => false,
        }
    }

    fn operator_named_result(
        &self,
        receiver: &ast::Expr,
        member: &str,
        state: &State,
        mro: &[KnownClass<'_>],
        returned_callable: bool,
    ) -> Option<Contract> {
        if self.authority.retired_members.contains(member) {
            return None;
        }
        let class = mro.iter().find(|class| {
            !class.intrinsic
                && !class
                    .file
                    .scopes
                    .bindings_in(class.scope, member)
                    .is_empty()
        })?;
        let methods = class.file.scopes.bindings_in(class.scope, member);
        if methods.len() != 1 {
            return None;
        }
        let binding = class.file.scopes.binding(methods[0]);
        let function = node_at(
            &class.file.parse.syntax_node(),
            binding.node_range,
            SyntaxKind::FUNC_DEF,
        )
        .and_then(ast::FuncDef::cast)?;
        // Special operator binding receives receiver and one operand exactly.
        // Descriptors/defaults/varargs are not guessed by this proof.
        let parameters: Vec<_> = function.params()?.params().take(4).collect();
        if parameters.len() != 2
            || !parameters
                .iter()
                .all(|parameter| matches!(parameter, ast::AnyParam::Plain(_)))
            || function.is_async()
            || function.decorators().next().is_some()
        {
            return None;
        }
        let ast::Expr::Name(name) = receiver else {
            return None;
        };
        if self
            .shadowed
            .iter()
            .any(|names| names.contains(name.name_text().as_str()))
        {
            return None;
        }
        let ids = if let Some(reaches) = self.reaching_name(&name.name_text(), state) {
            if reaches.is_empty()
                || reaches.len() > 16
                || reaches
                    .iter()
                    .any(|reach| reach.binding.is_none() || !reach.preds.is_empty())
            {
                return None;
            }
            reaches
                .iter()
                .filter_map(|reach| reach.binding)
                .collect::<Vec<_>>()
        } else {
            self.file
                .scopes
                .resolve(self.scope, &name.name_text())?
                .to_vec()
        };
        if ids.is_empty() || ids.len() > 16 {
            return None;
        }
        let mut result = None;
        for id in ids {
            let current = if returned_callable {
                self.declared_member_factory_result_contract(self.file, id, member, false)?
            } else {
                self.declared_member_contract(self.file, id, member)?
            };
            if result.is_some_and(|previous| {
                !matches!(
                    (previous, current),
                    (Contract::Sync, Contract::Sync) | (Contract::Async, Contract::Async)
                )
            }) {
                return None;
            }
            result = Some(current);
        }
        result
    }
}
