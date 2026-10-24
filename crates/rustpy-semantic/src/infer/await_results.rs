//! Direct await uses the maintained captured async protocol provider.
use super::*;
impl Inferer<'_> {
    pub(super) fn direct_await_result(&self, value: &Type) -> Type {
        if matches!(value, Type::Never) {
            return Type::Never;
        }
        let mut captures = Vec::new();
        let mut remaining = 512;
        if !self.direct_await_owners(value, &mut captures, &mut remaining, 0) {
            return Type::Unknown;
        }
        let result = self.direct_await_protocol(value, &mut 512, 0);
        if captures.iter().all(|file| {
            self.db
                .file(file.id)
                .is_some_and(|current| Arc::ptr_eq(file, &current))
        }) {
            result
        } else {
            Type::Unknown
        }
    }
    fn direct_await_protocol(&self, value: &Type, remaining: &mut usize, depth: usize) -> Type {
        if depth >= 16 || *remaining == 0 {
            return Type::Unknown;
        }
        *remaining -= 1;
        if let Type::Union(values) = value {
            if values.len() > 32 {
                return Type::Unknown;
            }
            let mut results = Vec::new();
            for value in values {
                results.push(self.direct_await_protocol(value, remaining, depth + 1));
                if *remaining == 0 {
                    return Type::Unknown;
                }
            }
            return Type::union(results);
        }
        if matches!(value, Type::Never) {
            return Type::Never;
        }
        let result = self.iteration_await_result(value, remaining, depth);
        if matches!(result, Type::Unknown)
            && *remaining > 0
            && matches!(
                self.async_slot_result(value, "__await__", remaining),
                Some(Type::Never)
            )
        {
            Type::Never
        } else {
            result
        }
    }

    pub(super) fn direct_await_at(
        &self,
        file: &Arc<FileInfo>,
        value: &Type,
        before: TextSize,
    ) -> Type {
        if !self
            .db
            .file(file.id)
            .is_some_and(|current| Arc::ptr_eq(file, &current))
        {
            return Type::Unknown;
        }
        if matches!(value, Type::Never) {
            return Type::Never;
        }
        let mut budget = 16_384;
        if !async_iteration::namespace_slot_intact_with_budget(
            file,
            before,
            &["__await__"],
            &mut budget,
        ) {
            return Type::Unknown;
        }
        let result = self.direct_await_result(value);
        if self
            .db
            .file(file.id)
            .is_some_and(|current| Arc::ptr_eq(file, &current))
        {
            result
        } else {
            Type::Unknown
        }
    }
    fn direct_await_owners(
        &self,
        value: &Type,
        captures: &mut Vec<Arc<FileInfo>>,
        remaining: &mut usize,
        depth: usize,
    ) -> bool {
        if depth >= 16 || *remaining == 0 {
            return false;
        }
        *remaining -= 1;
        if let Type::Union(values) = value {
            return values.len() <= 32
                && values
                    .iter()
                    .all(|value| self.direct_await_owners(value, captures, remaining, depth + 1));
        }
        if matches!(value, Type::Any | Type::Unknown | Type::Never) {
            return true;
        }
        let Some(class) = value.class_ref() else {
            return false;
        };
        let owners = self.mro(class);
        if owners.len() > 32 {
            return false;
        }
        for owner in owners.iter() {
            if *remaining == 0 {
                return false;
            }
            *remaining -= 1;
            let Some(file) = self.db.file(owner.file) else {
                return false;
            };
            if !captures.iter().any(|old| old.id == file.id) {
                if captures.len() >= 32 {
                    return false;
                }
                captures.push(file.clone());
            }
            let Some(declaration) = file
                .parse
                .syntax_node()
                .token_at_offset(owner.range.start())
                .right_biased()
                .and_then(|token| {
                    token
                        .parent_ancestors()
                        .take(64)
                        .find_map(ast::ClassDef::cast)
                })
            else {
                return false;
            };
            if declaration.text_range() != owner.range
                || declaration.name_text() != owner.name.as_str()
            {
                return false;
            }
            if file.is_stub && file.path.starts_with("<typeshed>") {
                continue;
            }
            for decorator in declaration.decorators().take(33) {
                let Some(expression) = decorator.expr() else {
                    return false;
                };
                let ty = self.type_of_expr(&file, &expression);
                let Some(definition) = ty.signature().and_then(|signature| signature.def.as_ref())
                else {
                    return false;
                };
                let Some(metadata) = self.db.file(definition.file) else {
                    return false;
                };
                if definition.name != "final"
                    || !metadata.is_stub
                    || !matches!(
                        metadata.path.to_str(),
                        Some("<typeshed>/typing.pyi" | "<typeshed>/typing_extensions.pyi")
                    )
                {
                    return false;
                }
                let mut namespace_budget = 16_384;
                if !async_iteration::namespace_slot_intact_with_budget(
                    &file,
                    expression.text_range().start(),
                    &["final"],
                    &mut namespace_budget,
                ) {
                    return false;
                }
                if captures.len() >= 32 && !captures.iter().any(|old| old.id == metadata.id) {
                    return false;
                }
                if !captures.iter().any(|old| old.id == metadata.id) {
                    captures.push(metadata);
                }
            }
            if declaration.decorators().take(33).count() > 32 {
                return false;
            }
        }
        true
    }
}
