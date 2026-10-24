//! Yield expression values from captured generator metadata, without execution.
use super::*;

impl Inferer<'_> {
    pub(super) fn yield_expression_result(
        &self,
        file: &Arc<FileInfo>,
        expression: &ast::YieldExpr,
    ) -> Type {
        // The closest lexical body owns send metadata. Never borrow an outer
        // generator annotation through a nested function, lambda or class.
        for ancestor in expression.syntax().ancestors().skip(1).take(256) {
            match ancestor.kind() {
                SyntaxKind::FUNC_DEF => {
                    let Some(function) = ast::FuncDef::cast(ancestor) else {
                        return Type::Unknown;
                    };
                    let Some(annotation) = function.return_annotation() else {
                        return Type::Unknown;
                    };
                    let metadata = self.annotation_type(file, &annotation);
                    return self.generator_metadata_slot(&metadata, 1, function.is_async(), 0);
                }
                SyntaxKind::LAMBDA_EXPR | SyntaxKind::CLASS_DEF => return Type::Unknown,
                _ => {}
            }
        }
        Type::Unknown
    }

    pub(super) fn yield_from_expression_result(
        &self,
        file: &Arc<FileInfo>,
        expression: &ast::YieldFromExpr,
    ) -> Type {
        let Some(delegate) = expression.expr() else {
            return Type::Unknown;
        };
        let metadata = self.type_of_expr(file, &delegate);
        self.generator_metadata_slot(&metadata, 2, false, 0)
    }

    fn generator_metadata_slot(
        &self,
        metadata: &Type,
        slot: usize,
        asynchronous: bool,
        depth: usize,
    ) -> Type {
        self.generator_metadata_slot_bounded(metadata, slot, asynchronous, depth, &mut 512)
            .unwrap_or(Type::Unknown)
    }

    fn generator_metadata_slot_bounded(
        &self,
        metadata: &Type,
        slot: usize,
        asynchronous: bool,
        depth: usize,
        budget: &mut usize,
    ) -> Option<Type> {
        if depth >= 32 || *budget == 0 {
            return None;
        }
        *budget -= 1;
        match metadata {
            Type::Instance(class, arguments) => {
                let expected_name = if asynchronous {
                    "AsyncGenerator"
                } else {
                    "Generator"
                };
                let expected_arity = if asynchronous { 2 } else { 3 };
                if class.name != expected_name || arguments.len() != expected_arity {
                    return Some(Type::Unknown);
                }
                let Some(source) = self.db.file(class.file) else {
                    return Some(Type::Unknown);
                };
                // A project class with the same spelling has no generator
                // protocol authority. Recognize canonical bundled declarations.
                if !source.is_stub
                    || !source.path.to_string_lossy().starts_with("<typeshed>")
                    || !matches!(
                        source.module_name.as_deref(),
                        Some("typing" | "typing_extensions" | "collections.abc")
                    )
                {
                    return Some(Type::Unknown);
                }
                Some(arguments.get(slot).cloned().unwrap_or(Type::Unknown))
            }
            Type::Union(arms) if arms.len() <= 32 => {
                // Charge the join as well as each visited arm. One shared
                // counter bounds the entire tree, not each sibling separately.
                if *budget < arms.len() {
                    return None;
                }
                *budget -= arms.len();
                let values = arms
                    .iter()
                    .map(|arm| {
                        self.generator_metadata_slot_bounded(
                            arm,
                            slot,
                            asynchronous,
                            depth + 1,
                            budget,
                        )
                    })
                    .collect::<Option<Vec<_>>>()?;
                Some(Type::union(values))
            }
            Type::Any => Some(Type::Any),
            _ => Some(Type::Unknown),
        }
    }
}
