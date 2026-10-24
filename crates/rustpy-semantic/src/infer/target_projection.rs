//! Bounded structural projection of assignment/iteration destructuring targets.
use super::*;

#[derive(Clone, Copy)]
struct TargetStep {
    index: usize,
    count: usize,
    star: Option<usize>,
}

impl Inferer<'_> {
    pub(super) fn structural_target_type(
        &self,
        file: &Arc<FileInfo>,
        binding: &crate::scope::Binding,
    ) -> Type {
        if !self
            .db
            .file(file.id)
            .is_some_and(|current| Arc::ptr_eq(&current, file))
        {
            return Type::Unknown;
        }
        let root = file.parse.syntax_node();
        let Some(token) = root.token_at_offset(binding.range.start()).right_biased() else {
            return Type::Unknown;
        };
        let mut node = token.parent();
        let mut steps = Vec::new();
        let mut budget = 512usize;
        for _ in 0..256 {
            let Some(current) = node else {
                return Type::Unknown;
            };
            let Some(parent) = current.parent() else {
                return Type::Unknown;
            };
            if budget == 0 {
                return Type::Unknown;
            }
            budget -= 1;
            match parent.kind() {
                SyntaxKind::STAR_EXPR | SyntaxKind::PAREN_EXPR => {
                    node = Some(parent);
                    continue;
                }
                SyntaxKind::TUPLE_EXPR | SyntaxKind::LIST_EXPR => {
                    let children: Vec<_> = parent
                        .children()
                        .filter(|child| ast::Expr::can_cast(child.kind()))
                        .take(257)
                        .collect();
                    if children.len() > 256 || steps.len() >= 32 || budget < children.len() {
                        return Type::Unknown;
                    }
                    budget -= children.len();
                    let Some(index) = children.iter().position(|child| child == &current) else {
                        return Type::Unknown;
                    };
                    let stars: Vec<_> = children
                        .iter()
                        .enumerate()
                        .filter(|(_, child)| child.kind() == SyntaxKind::STAR_EXPR)
                        .map(|(index, _)| index)
                        .collect();
                    if stars.len() > 1 {
                        return Type::Unknown;
                    }
                    steps.push(TargetStep {
                        index,
                        count: children.len(),
                        star: stars.first().copied(),
                    });
                    node = Some(parent);
                    continue;
                }
                _ => {}
            }
            let source = match parent.kind() {
                SyntaxKind::WITH_ITEM => ast::WithItem::cast(parent).and_then(|item| {
                    let context = item.context_expr()?;
                    // The source is the value acquired by this exact item, not
                    // the context object and not another item in the same with.
                    let is_async = item
                        .syntax()
                        .parent()
                        .and_then(ast::WithStmt::cast)
                        .is_some_and(|statement| statement.is_async());
                    let receiver = self.type_of_expr(file, &context);
                    let entered = self
                        .implicit_slot(&receiver, if is_async { "__aenter__" } else { "__enter__" })
                        .map(|member| self.call_result(&member, &[], &[]))
                        .unwrap_or(Type::Unknown);
                    Some(if is_async {
                        self.await_type(&entered)
                    } else {
                        entered
                    })
                }),
                SyntaxKind::ASSIGN_STMT => ast::AssignStmt::cast(parent)
                    .and_then(|statement| statement.value())
                    .map(|value| self.type_of_expr(file, &value)),
                SyntaxKind::FOR_STMT => ast::ForStmt::cast(parent).and_then(|statement| {
                    statement.iter().map(|value| {
                        self.iteration_source_type(
                            file,
                            &self.type_of_expr(file, &value),
                            statement.is_async(),
                            value.text_range().start(),
                        )
                    })
                }),
                SyntaxKind::COMP_FOR => ast::CompFor::cast(parent).and_then(|statement| {
                    statement.iter().map(|value| {
                        self.iteration_source_type(
                            file,
                            &self.type_of_expr(file, &value),
                            statement.is_async(),
                            value.text_range().start(),
                        )
                    })
                }),
                _ => None,
            };
            let mut value = source.unwrap_or(Type::Unknown);
            for step in steps.iter().rev() {
                if binding.kind == BindingKind::With
                    && step.star.is_none()
                    && matches!(&value, Type::Tuple(values) if values.len() != step.count)
                {
                    return Type::Unknown;
                }
                let Some(projected) = self.project_target_step(&value, *step, 0, &mut budget)
                else {
                    return Type::Unknown;
                };
                value = projected;
            }
            // Keep the preexisting scalar With result presentation unchanged.
            return if binding.kind == BindingKind::With && steps.is_empty() {
                value
            } else {
                self.literal_string_assignment(file, binding, value)
            };
        }
        Type::Unknown
    }

    fn project_target_step(
        &self,
        source: &Type,
        step: TargetStep,
        depth: usize,
        budget: &mut usize,
    ) -> Option<Type> {
        if depth >= 32 || *budget == 0 {
            return None;
        }
        *budget -= 1;
        if let Some(tuple) = self.type_factory_tuple_value(source) {
            return self.project_target_step(&tuple, step, depth + 1, budget);
        }
        match source {
            Type::Union(arms) if arms.len() <= 32 => {
                if *budget < arms.len() {
                    return None;
                }
                *budget -= arms.len();
                let projected = arms
                    .iter()
                    .map(|arm| self.project_target_step(arm, step, depth + 1, budget))
                    .collect::<Option<Vec<_>>>()?;
                Some(Type::union(projected))
            }
            Type::Union(_) => None,
            Type::Tuple(values) => {
                if values.len() > 256 || *budget < values.len() {
                    return None;
                }
                *budget -= values.len();
                let Some(star) = step.star else {
                    return Some(self.index_unpack(source, step.index));
                };
                if values
                    .iter()
                    .any(|value| matches!(value, Type::Unpack(_) | Type::TypeVarTuple(_)))
                {
                    return Some(Type::Unknown);
                }
                if values.len() < step.count.saturating_sub(1) {
                    return Some(Type::Unknown);
                }
                if step.index == star {
                    let suffix = step.count - star - 1;
                    let elements = values[star..values.len() - suffix].iter().cloned();
                    Some(self.star_capture_list(Type::union(elements.collect())))
                } else {
                    let index = if step.index < star {
                        step.index
                    } else {
                        values.len() - (step.count - step.index)
                    };
                    Some(values.get(index).cloned().unwrap_or(Type::Unknown))
                }
            }
            other => {
                let element = self.iter_type(other);
                if step.star == Some(step.index) {
                    if matches!(element, Type::Unknown) {
                        Some(Type::Unknown)
                    } else {
                        Some(self.star_capture_list(element))
                    }
                } else {
                    Some(element)
                }
            }
        }
    }
    fn star_capture_list(&self, element: Type) -> Type {
        self.builtin_class("list")
            .map(|class| {
                Type::Instance(
                    class,
                    vec![if matches!(element, Type::Never) {
                        Type::Unknown
                    } else {
                        element.clone()
                    }],
                )
            })
            .unwrap_or(Type::Unknown)
    }
}
