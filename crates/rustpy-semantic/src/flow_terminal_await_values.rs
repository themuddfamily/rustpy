//! Captured declared await-result authority; no inference or runtime evaluation.
use super::*;
impl Walker<'_> {
    pub(in super::super::super) fn declared_await_operand_terminal(
        &self,
        expression: &ast::Expr,
        state: &State,
    ) -> bool {
        self.await_operand_expression(expression, state, 0)
    }
    fn await_operand_expression(
        &self,
        expression: &ast::Expr,
        state: &State,
        depth: usize,
    ) -> bool {
        if depth >= 16 {
            return false;
        }
        let expression = expression.clone().unparen();
        let name = match expression {
            ast::Expr::Name(name) => name,
            ast::Expr::Named(named) => {
                return named
                    .value()
                    .is_some_and(|value| self.await_operand_expression(&value, state, depth + 1));
            }
            _ => return false,
        };
        let Some(reaches) = self.reaching_name(&name.name_text(), state) else {
            return false;
        };
        !reaches.is_empty()
            && reaches.len() <= 16
            && reaches.iter().all(|reach| {
                reach.preds.is_empty()
                    && reach.binding.is_some_and(|id| {
                        self.await_operand_binding(self.file, id, &mut FxHashSet::default())
                    })
            })
    }
    fn await_operand_binding(
        &self,
        file: &FileInfo,
        id: BindingId,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
    ) -> bool {
        if seen.len() >= 16 || !seen.insert((file.id, id)) {
            return false;
        }
        let binding = file.scopes.binding(id);
        if !matches!(binding.kind, BindingKind::Param | BindingKind::Assignment) {
            return false;
        }
        if let Some(annotation) = binding
            .annotation
            .and_then(|range| self.expression_at(file, range))
        {
            let scope = if matches!(binding.kind, BindingKind::Param) {
                let Some(scope) = file.scopes.scope(binding.scope).parent else {
                    return false;
                };
                scope
            } else {
                binding.scope
            };
            if !matches!(annotation, ast::Expr::String(_))
                && !Self::annotations_deferred(file)
                && Self::unknown_annotation_class(file, scope)
            {
                return false;
            }
            return self
                .awaitable_return_contract(file, scope, &annotation)
                .is_some();
        }
        let Some(ast::Expr::Name(name)) = binding
            .value
            .and_then(|range| self.expression_at(file, range))
            .map(ast::Expr::unparen)
        else {
            return false;
        };
        let Some(ids) = self.value_binding_ids(file, binding.scope, &name, binding.range.start())
        else {
            return false;
        };
        !ids.is_empty()
            && ids.len() <= 16
            && ids
                .iter()
                .all(|id| self.await_operand_binding(file, *id, &mut seen.clone()))
    }
}
