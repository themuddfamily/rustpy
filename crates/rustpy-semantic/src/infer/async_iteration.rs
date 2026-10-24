//! Async iteration follows the actual async slots, never the sync fallback.
use super::*;

impl Inferer<'_> {
    pub(super) fn iteration_target_type(
        &self,
        file: &Arc<FileInfo>,
        binding: &crate::scope::Binding,
    ) -> Type {
        let Some(value) = binding.value else {
            return self.unpacked_target_type(file, binding);
        };
        if !self
            .db
            .file(file.id)
            .is_some_and(|current| Arc::ptr_eq(&current, file))
        {
            return Type::Unknown;
        }
        let Some(token) = file
            .parse
            .syntax_node()
            .token_at_offset(binding.range.start())
            .right_biased()
        else {
            return Type::Unknown;
        };
        for node in token.parent_ancestors().take(256) {
            let mode = match node.kind() {
                SyntaxKind::FOR_STMT => ast::ForStmt::cast(node.clone()).and_then(|statement| {
                    statement
                        .iter()
                        .filter(|expression| expression.text_range() == value)
                        .map(|_| statement.is_async())
                }),
                SyntaxKind::COMP_FOR => ast::CompFor::cast(node.clone()).and_then(|statement| {
                    statement
                        .iter()
                        .filter(|expression| expression.text_range() == value)
                        .map(|_| statement.is_async())
                }),
                _ => None,
            };
            if let Some(is_async) = mode {
                return self.iteration_source_type(
                    file,
                    &self.type_of_range(file, value),
                    is_async,
                    binding.range.start(),
                );
            }
            if matches!(
                node.kind(),
                SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF | SyntaxKind::LAMBDA_EXPR
            ) {
                break;
            }
        }
        Type::Unknown
    }

    pub(super) fn iteration_source_type(
        &self,
        file: &Arc<FileInfo>,
        source: &Type,
        is_async: bool,
        before: TextSize,
    ) -> Type {
        if !is_async {
            return self.iter_type(source);
        }
        if !async_protocol_intact(file, before) {
            return Type::Unknown;
        }
        let mut budget = 512;
        if !charge_async_type(source, &mut budget, 0) {
            return Type::Unknown;
        }
        let result = self.async_iteration_item(source, &mut budget, 0);
        if budget == 0 {
            Type::Unknown
        } else {
            result
        }
    }

    fn async_iteration_item(&self, source: &Type, budget: &mut usize, depth: usize) -> Type {
        if *budget == 0 || depth >= 16 {
            *budget = 0;
            return Type::Unknown;
        }
        *budget -= 1;
        match source {
            Type::Any | Type::Unknown => source.clone(),
            Type::Union(arms) if arms.len() <= 32 => {
                let mut result = Vec::new();
                for arm in arms {
                    let item = self.async_iteration_item(arm, budget, depth + 1);
                    if *budget == 0 {
                        return Type::Unknown;
                    }
                    result.push(item);
                }
                Type::union(result)
            }
            Type::Union(_) => {
                *budget = 0;
                Type::Unknown
            }
            _ => {
                // __aiter__ is not awaited. An async function returning a
                // coroutine here is invalid unless the returned object itself
                // actually supplies __anext__.
                let Some(iterator) = self.async_slot_result(source, "__aiter__", budget) else {
                    return Type::Unknown;
                };
                if !charge_async_type(&iterator, budget, 0) {
                    return Type::Unknown;
                }
                let Some(next) = self.async_slot_result(&iterator, "__anext__", budget) else {
                    return Type::Unknown;
                };
                if !charge_async_type(&next, budget, 0) {
                    return Type::Unknown;
                }
                self.iteration_await_result(&next, budget, depth + 1)
            }
        }
    }

    pub(super) fn iteration_await_result(
        &self,
        value: &Type,
        budget: &mut usize,
        depth: usize,
    ) -> Type {
        if *budget == 0 || depth >= 16 {
            *budget = 0;
            return Type::Unknown;
        }
        *budget -= 1;
        match value {
            Type::Any | Type::Unknown => value.clone(),
            Type::Union(arms) if arms.len() <= 32 => {
                let mut result = Vec::new();
                for arm in arms {
                    let awaited = self.iteration_await_result(arm, budget, depth + 1);
                    if *budget == 0 {
                        return Type::Unknown;
                    }
                    result.push(awaited);
                }
                Type::union(result)
            }
            Type::Union(_) => {
                *budget = 0;
                Type::Unknown
            }
            Type::Instance(class, args) if self.canonical_async_wrapper(class) => {
                match (class.name.as_str(), args.as_slice()) {
                    ("Coroutine", [_, _, result])
                    | ("CoroutineType", [_, _, result])
                    | ("Awaitable", [result]) => {
                        if charge_async_type(result, budget, 0) {
                            result.clone()
                        } else {
                            Type::Unknown
                        }
                    }
                    _ => self.iteration_await_slot(value, budget),
                }
            }
            _ => self.iteration_await_slot(value, budget),
        }
    }

    fn iteration_await_slot(&self, value: &Type, budget: &mut usize) -> Type {
        let Some(Type::Instance(class, args)) = self.async_slot_result(value, "__await__", budget)
        else {
            return Type::Unknown;
        };
        if self.canonical_async_wrapper(&class) && class.name == "Generator" && args.len() == 3 {
            if charge_async_type(&args[2], budget, 0) {
                args[2].clone()
            } else {
                Type::Unknown
            }
        } else {
            Type::Unknown
        }
    }

    pub(super) fn async_slot_result(
        &self,
        receiver: &Type,
        name: &str,
        budget: &mut usize,
    ) -> Option<Type> {
        let class = receiver.class_ref()?;
        let owners = self.mro(class);
        if owners.len() > 32 {
            *budget = 0;
            return None;
        }
        let mut captures = Vec::new();
        let mut owner_nodes = 16_384;
        for owner in owners.iter() {
            if *budget == 0 {
                return None;
            }
            *budget -= 1;
            let file = self.db.file(owner.file)?;
            let declaration = file
                .parse
                .syntax_node()
                .token_at_offset(owner.range.start())
                .right_biased()?
                .parent_ancestors()
                .take(64)
                .find_map(ast::ClassDef::cast)?;
            if declaration.text_range() != owner.range
                || declaration.name_text() != owner.name.as_str()
            {
                return None;
            }
            if !captures
                .iter()
                .any(|captured: &Arc<FileInfo>| captured.id == file.id)
            {
                if !async_owner_file_intact(&file, &mut owner_nodes) {
                    return None;
                }
                if captures.len() >= 32 {
                    *budget = 0;
                    return None;
                }
                captures.push(file);
            }
        }
        let callee = self.implicit_slot(receiver, name)?;
        let signatures: Vec<_> = match &callee {
            Type::Function(signature)
            | Type::BoundMethod(signature, _)
            | Type::Callable(signature) => vec![signature.clone()],
            Type::Overloaded(signatures) if signatures.len() <= 32 => signatures.clone(),
            _ => return None,
        };
        for signature in &signatures {
            if *budget == 0 {
                return None;
            }
            *budget -= 1;
            let definition = signature.def.as_ref()?;
            let file = self.db.file(definition.file)?;
            let declaration = file
                .parse
                .syntax_node()
                .token_at_offset(definition.range.start())
                .right_biased()?
                .parent_ancestors()
                .take(64)
                .find_map(ast::FuncDef::cast)?;
            if declaration.text_range() != definition.range
                || declaration.name_text() != definition.name.as_str()
            {
                return None;
            }
            // Unknown decorators can replace an async method while the maintained
            // nominal signature still retains its original declaration.
            if !(file.is_stub && file.path.starts_with("<typeshed>")) {
                for decorator in declaration.decorators().take(33) {
                    if owner_nodes == 0 {
                        return None;
                    }
                    owner_nodes -= 1;
                    let expression = decorator.expr()?.unparen();
                    // Factory calls and descriptor wrappers require separate binding
                    // semantics; only captured, ordinary identity metadata is admitted.
                    if matches!(expression, ast::Expr::Call(_)) {
                        return None;
                    }
                    let ty = self.type_of_expr(&file, &expression);
                    let definition = ty.signature()?.def.as_ref()?;
                    let metadata = self.db.file(definition.file)?;
                    let canonical = metadata.is_stub
                        && ((metadata.path == std::path::Path::new("<typeshed>/abc.pyi")
                            && definition.name == "abstractmethod")
                            || (["<typeshed>/typing.pyi", "<typeshed>/typing_extensions.pyi"]
                                .iter()
                                .any(|path| metadata.path == std::path::Path::new(path))
                                && matches!(
                                    definition.name.as_str(),
                                    "final" | "override" | "overload"
                                )));
                    if !canonical {
                        return None;
                    }
                    if let ast::Expr::Attribute(attribute) = &expression {
                        let name = attribute.attr_text();
                        if !namespace_slot_intact_with_budget(
                            &file,
                            expression.text_range().start(),
                            &[name.as_str()],
                            &mut owner_nodes,
                        ) {
                            return None;
                        }
                    }
                    if captures.len() >= 32 {
                        return None;
                    }
                    if !captures.iter().any(|old| old.id == metadata.id) {
                        captures.push(metadata);
                    }
                }
                if declaration.decorators().take(33).count() > 32 {
                    return None;
                }
            }
            if !captures.iter().any(|captured| captured.id == file.id) {
                if !async_owner_file_intact(&file, &mut owner_nodes) {
                    return None;
                }
                if captures.len() >= 32 {
                    *budget = 0;
                    return None;
                }
                captures.push(file);
            }
        }
        if captures.len() > 32 {
            *budget = 0;
            return None;
        }
        let signature = signatures
            .iter()
            .find(|signature| self.arguments_fit(signature, &[], &[]))?;
        let result = self.apply_signature(signature, &[], &[]);
        // Owner files are captured from the maintained database world, never
        // recanonicalized or read from disk. Reject a source swap during proof.
        captures
            .iter()
            .all(|file| {
                self.db
                    .file(file.id)
                    .is_some_and(|current| Arc::ptr_eq(file, &current))
            })
            .then_some(result)
    }

    fn canonical_async_wrapper(&self, class: &ClassRef) -> bool {
        self.db.file(class.file).is_some_and(|file| {
            file.is_stub
                && [
                    "<typeshed>/typing.pyi",
                    "<typeshed>/typing_extensions.pyi",
                    "<typeshed>/collections/abc.pyi",
                    "<typeshed>/_collections_abc.pyi",
                    "<typeshed>/types.pyi",
                ]
                .iter()
                .any(|path| file.path == std::path::Path::new(path))
        })
    }
}

fn charge_async_type(ty: &Type, budget: &mut usize, depth: usize) -> bool {
    if *budget == 0 || depth >= 16 {
        *budget = 0;
        return false;
    }
    *budget -= 1;
    match ty {
        Type::Class(_, values)
        | Type::Instance(_, values)
        | Type::Tuple(values)
        | Type::Union(values)
        | Type::TypePack(values)
        | Type::Concatenate(values, _) => values
            .iter()
            .all(|value| charge_async_type(value, budget, depth + 1)),
        Type::BoundMethod(_, receiver) | Type::Unpack(receiver) | Type::Super { receiver, .. } => {
            charge_async_type(receiver, budget, depth + 1)
        }
        Type::ParameterList(parameters) => parameters
            .iter()
            .all(|parameter| charge_async_type(&parameter.ty, budget, depth + 1)),
        Type::Overloaded(signatures) => {
            if signatures.len() > *budget {
                *budget = 0;
                false
            } else {
                *budget -= signatures.len();
                true
            }
        }
        _ => true,
    }
}

fn async_protocol_intact(file: &FileInfo, before: TextSize) -> bool {
    namespace_slot_intact(file, before, &["__aiter__", "__anext__", "__await__"])
}

pub(super) fn namespace_slot_intact(file: &FileInfo, before: TextSize, names: &[&str]) -> bool {
    let mut budget = 16_384;
    namespace_slot_intact_with_budget(file, before, names, &mut budget)
}

pub(super) fn namespace_slot_intact_with_budget(
    file: &FileInfo,
    before: TextSize,
    names: &[&str],
    budget: &mut usize,
) -> bool {
    for node in file.parse.syntax_node().descendants() {
        if node.text_range().start() >= before {
            break;
        }
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        let targets: Vec<_> = match node.kind() {
            SyntaxKind::ASSIGN_STMT => {
                let Some(statement) = ast::AssignStmt::cast(node) else {
                    return false;
                };
                let value = statement.value().map(|value| value.text_range());
                statement
                    .syntax()
                    .children()
                    .filter_map(ast::Expr::cast)
                    .filter(|target| Some(target.text_range()) != value)
                    .take(65)
                    .collect()
            }
            SyntaxKind::ANN_ASSIGN_STMT => ast::AnnAssignStmt::cast(node)
                .and_then(|statement| statement.target())
                .into_iter()
                .collect(),
            SyntaxKind::AUG_ASSIGN_STMT => ast::AugAssignStmt::cast(node)
                .and_then(|statement| statement.target())
                .into_iter()
                .collect(),
            SyntaxKind::DEL_STMT => node
                .children()
                .filter_map(ast::Expr::cast)
                .take(65)
                .collect(),
            _ => Vec::new(),
        };
        if targets.len() > 64 {
            return false;
        }
        if targets
            .into_iter()
            .any(|target| async_protocol_target_write(target, names, budget, 0))
        {
            return false;
        }
    }
    true
}

fn async_protocol_target_write(
    target: ast::Expr,
    names: &[&str],
    budget: &mut usize,
    depth: usize,
) -> bool {
    if *budget == 0 || depth >= 32 {
        *budget = 0;
        return true;
    }
    *budget -= 1;
    match target {
        ast::Expr::Attribute(attribute) => names.contains(&attribute.attr_text().as_str()),
        ast::Expr::Tuple(tuple) => tuple
            .elements()
            .any(|target| async_protocol_target_write(target, names, budget, depth + 1)),
        ast::Expr::List(list) => list
            .elements()
            .any(|target| async_protocol_target_write(target, names, budget, depth + 1)),
        ast::Expr::Paren(paren) => paren
            .inner()
            .is_none_or(|target| async_protocol_target_write(target, names, budget, depth + 1)),
        ast::Expr::Star(star) => star
            .expr()
            .is_none_or(|target| async_protocol_target_write(target, names, budget, depth + 1)),
        _ => false,
    }
}

fn async_owner_file_intact(file: &FileInfo, budget: &mut usize) -> bool {
    if file.is_stub && file.path.starts_with(std::path::Path::new("<typeshed>")) {
        return true;
    }
    // Conservatively include visible owner-module writes even if they occur
    // after the class statement; no module initialization or callback VM.
    namespace_slot_intact_with_budget(
        file,
        file.parse.syntax_node().text_range().end(),
        &["__aiter__", "__anext__", "__await__"],
        budget,
    )
}
