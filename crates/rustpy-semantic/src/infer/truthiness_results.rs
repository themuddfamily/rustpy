//! Value types of `and` / `or`, filtered by the operands' known truthiness.
use super::*;

impl Inferer<'_> {
    /// `a or b`: the truthy part of every operand but the last, plus the last
    /// one; `a and b` keeps the falsy parts instead (as pyright does). Operands
    /// after one that always decides the result are unreachable.
    pub(super) fn bool_op_result(&self, file: &Arc<FileInfo>, op: &ast::BoolOpExpr) -> Type {
        let keep_truthy = op.is_or();
        let operands: Vec<ast::Expr> = op.operands().collect();
        let mut parts = Vec::new();
        for (index, operand) in operands.iter().enumerate() {
            let ty = self.type_of_expr(file, operand);
            let last = index + 1 == operands.len();
            let decided = if last {
                ty.clone()
            } else {
                self.truthiness_part(&ty, keep_truthy)
            };
            parts.extend(
                union_members(decided)
                    .into_iter()
                    .filter(|member| !matches!(member, Type::Never))
                    .map(|member| self.widen(member)),
            );
            // `None and x` / `1 or x`: later operands are never evaluated.
            if !last && matches!(self.truthiness_part(&ty, !keep_truthy), Type::Never) {
                break;
            }
        }
        if parts.is_empty() {
            Type::Never
        } else {
            Type::union(parts)
        }
    }

    /// The members of `ty` that may evaluate as `truthy` (`Never` when none).
    fn truthiness_part(&self, ty: &Type, truthy: bool) -> Type {
        let live: Vec<Type> = union_members(ty.clone())
            .into_iter()
            .filter_map(|member| match self.value_truthiness(&member) {
                Some(known) if known != truthy => None,
                _ if self.is_bool_instance(&member) => {
                    Some(Type::Literal(LiteralValue::Bool(truthy)))
                }
                _ => Some(member),
            })
            .collect();
        if live.is_empty() {
            Type::Never
        } else {
            Type::union(live)
        }
    }

    /// Known truthiness of a value: literals and `None` by value, and instances
    /// of classes defining neither `__bool__` nor `__len__` are always truthy
    /// (`object` and protocols excepted: subclasses may still be falsy).
    pub(super) fn value_truthiness(&self, ty: &Type) -> Option<bool> {
        if let Some(known) = crate::narrow::truthiness(ty) {
            return Some(known);
        }
        let Type::Instance(class, _) = ty else {
            return None;
        };
        if self.builtin_class("object").as_ref() == Some(class)
            || self.canonical_protocol(class)
            || self.has_unknown_base(class)
        {
            return None;
        }
        (self.implicit_slot(ty, "__bool__").is_none()
            && self.implicit_slot(ty, "__len__").is_none())
        .then_some(true)
    }

    fn is_bool_instance(&self, ty: &Type) -> bool {
        matches!(ty, Type::Instance(class, _) if self.builtin_class("bool").as_ref() == Some(class))
    }
}

fn union_members(ty: Type) -> Vec<Type> {
    match ty {
        Type::Union(members) => members,
        other => vec![other],
    }
}
