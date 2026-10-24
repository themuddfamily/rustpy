//! Canonical functools values retain their bound arguments and real call target.
//! Metadata copying does not replace the wrapper implementation's call contract.
use super::*;
use std::path::Path;

#[derive(Clone, PartialEq, Eq)]
enum ValueKind {
    Partial {
        callable: Type,
        positional: Vec<Type>,
        keywords: Vec<(SmolStr, Type)>,
    },
    Decorator {
        wrapped: Type,
    },
    Wrapped {
        callable: Type,
        wrapped: Type,
    },
}
#[derive(Clone)]
pub(super) struct FunctoolsValue {
    kind: ValueKind,
    source: Arc<FileInfo>,
    owner: Arc<FileInfo>,
    captures: Vec<Arc<FileInfo>>,
}
impl FunctoolsValue {
    fn current(&self, inf: &Inferer<'_>) -> bool {
        std::iter::once(&self.source)
            .chain(std::iter::once(&self.owner))
            .chain(self.captures.iter())
            .all(|old| {
                inf.db
                    .file(old.id)
                    .is_some_and(|now| Arc::ptr_eq(old, &now))
            })
    }
}
impl Inferer<'_> {
    pub(super) fn functools_call(
        &self,
        file: &Arc<FileInfo>,
        call: &ast::CallExpr,
        _callee: &Type,
    ) -> Option<Type> {
        let expression = call.callee()?;
        let definition = match expression.clone().unparen() {
            ast::Expr::Name(name) => {
                self.resolve_name(file, self.scope_of(file, name.syntax()), &name.name_text())
            }
            ast::Expr::Attribute(attribute) => attribute.object().and_then(|object| {
                let Type::Module(id) = self.type_of_expr(file, &object) else {
                    return None;
                };
                self.resolve_in_module(&self.db.file(id)?, &attribute.attr_text(), 0)
            }),
            _ => None,
        }
        .map(|definition| self.follow_imports(definition))?;
        let name = definition.binding().name.as_str();
        let owner = &definition.file;
        if !matches!(name, "partial" | "wraps" | "update_wrapper")
            || !owner.is_stub
            || owner.module_name.as_deref() != Some("functools")
            || owner.path != Path::new("<typeshed>/functools.pyi")
            || Self::namespace_member_retired(file, name, expression.text_range().start())
        {
            return None;
        }
        let args: Vec<_> = call.arg_list()?.args().take(65).collect();
        if args.len() > 64
            || args
                .iter()
                .any(|argument| matches!(argument, ast::AnyArg::Star(_) | ast::AnyArg::StarStar(_)))
        {
            return Some(Type::Unknown);
        }
        let (mut positional, mut keywords) = self.call_args(file, call);
        if keywords
            .iter()
            .enumerate()
            .any(|(index, (name, _))| keywords[..index].iter().any(|(old, _)| old == name))
        {
            return Some(Type::Unknown);
        }
        if !self.admit_functools_arguments(&positional, &keywords) {
            return Some(Type::Unknown);
        }
        let take = |positional: &mut Vec<Type>,
                    keywords: &mut Vec<(SmolStr, Type)>,
                    name: &str|
         -> Option<Type> {
            if !positional.is_empty() {
                if keywords.iter().any(|(key, _)| key == name) {
                    return None;
                }
                Some(positional.remove(0))
            } else {
                let index = keywords.iter().position(|(key, _)| key == name)?;
                Some(keywords.remove(index).1)
            }
        };
        let kind = match name {
            "partial" => {
                // The function argument is positional-only in Python.
                if positional.is_empty() {
                    return Some(Type::Unknown);
                }
                let mut callable = positional.remove(0);
                if let Some(previous) = self.functools_value(&callable) {
                    if let ValueKind::Partial {
                        callable: target,
                        positional: old_pos,
                        keywords: old_kw,
                    } = previous.kind
                    {
                        let mut combined = old_pos;
                        combined.extend(positional);
                        positional = combined;
                        keywords = merge_keywords(old_kw, keywords);
                        callable = target;
                    }
                }
                if self
                    .partial_signatures(&callable, &positional, &keywords)
                    .is_empty()
                {
                    return Some(Type::Unknown);
                }
                ValueKind::Partial {
                    callable,
                    positional,
                    keywords,
                }
            }
            "wraps" => {
                let Some(wrapped) = take(&mut positional, &mut keywords, "wrapped") else {
                    return Some(Type::Unknown);
                };
                // Custom attribute sequences do not change call authority, but
                // their execution/attribute side effects are not fabricated.
                if positional.len() > 2
                    || (!positional.is_empty()
                        && keywords.iter().any(|(name, _)| name == "assigned"))
                    || (positional.len() >= 2 && keywords.iter().any(|(name, _)| name == "updated"))
                    || keywords
                        .iter()
                        .any(|(key, _)| !matches!(key.as_str(), "assigned" | "updated"))
                {
                    return Some(Type::Unknown);
                }
                ValueKind::Decorator { wrapped }
            }
            "update_wrapper" => {
                let Some(callable) = take(&mut positional, &mut keywords, "wrapper") else {
                    return Some(Type::Unknown);
                };
                let Some(wrapped) = take(&mut positional, &mut keywords, "wrapped") else {
                    return Some(Type::Unknown);
                };
                if positional.len() > 2
                    || (!positional.is_empty()
                        && keywords.iter().any(|(name, _)| name == "assigned"))
                    || (positional.len() >= 2 && keywords.iter().any(|(name, _)| name == "updated"))
                    || keywords
                        .iter()
                        .any(|(key, _)| !matches!(key.as_str(), "assigned" | "updated"))
                {
                    return Some(Type::Unknown);
                }
                if self.functools_signatures(&callable).is_empty() {
                    return Some(Type::Unknown);
                }
                ValueKind::Wrapped { callable, wrapped }
            }
            _ => return None,
        };
        let key = ClassRef {
            file: file.id,
            range: call.text_range(),
            name: SmolStr::new(match &kind {
                ValueKind::Partial { .. } => "partial",
                ValueKind::Decorator { .. } => "wraps",
                ValueKind::Wrapped { .. } => "wrapped",
            }),
        };
        Some(self.store_functools_value(key, kind, file.clone(), owner.clone()))
    }
    fn store_functools_value(
        &self,
        key: ClassRef,
        kind: ValueKind,
        source: Arc<FileInfo>,
        owner: Arc<FileInfo>,
    ) -> Type {
        if !functools_namespace_stable(&source) {
            return Type::Unknown;
        }
        let mut captures = Vec::new();
        let mut budget = 2048;
        let mut bytes = 65536;
        let mut admit =
            |ty: &Type| self.admit_functools_type(ty, &mut captures, &mut budget, &mut bytes, 0);
        let admitted = match &kind {
            ValueKind::Partial {
                callable,
                positional,
                keywords,
            } => {
                admit(callable)
                    && positional.iter().all(&mut admit)
                    && keywords.iter().all(|(_, ty)| admit(ty))
            }
            ValueKind::Decorator { wrapped } => admit(wrapped),
            ValueKind::Wrapped { callable, wrapped } => admit(callable) && admit(wrapped),
        };
        if !admitted {
            return Type::Unknown;
        }
        let value = FunctoolsValue {
            kind,
            source,
            owner,
            captures,
        };
        if !value.current(self) {
            return Type::Unknown;
        }
        let mut values = self.functools_values.borrow_mut();
        if let Some(old) = values.get(&key) {
            // A context-dependent second allocation must not change the meaning
            // of an already published synthetic value.
            return if old.current(self) && old.kind == value.kind {
                Type::Instance(key, Vec::new())
            } else {
                Type::Unknown
            };
        }
        if values.len() >= 128 {
            return Type::Unknown;
        }
        values.insert(key.clone(), value);
        Type::Instance(key, Vec::new())
    }
    /// Whether `ty` is a synthetic `functools.partial`/`wraps` value: its class key is the
    /// creating call, not a class declaration, so it has no declared member namespace.
    pub fn is_functools_value(&self, ty: &Type) -> bool {
        let Type::Instance(class, _) = ty else {
            return false;
        };
        if self.functools_values.borrow().contains_key(class) {
            return true;
        }
        matches!(class.name.as_str(), "partial" | "wraps" | "wrapped")
            && self.class_info(class).is_none()
    }
    fn functools_value(&self, ty: &Type) -> Option<FunctoolsValue> {
        let Type::Instance(class, _) = ty else {
            return None;
        };
        let value = self.functools_values.borrow().get(class)?.clone();
        value.current(self).then_some(value)
    }
    pub(super) fn functools_call_result(
        &self,
        callee: &Type,
        positional: &[Type],
        keywords: &[(SmolStr, Type)],
    ) -> Option<Type> {
        let value = match self.functools_value(callee) {
            Some(value) => value,
            None => {
                let Type::Instance(class, _) = callee else {
                    return None;
                };
                return self
                    .functools_values
                    .borrow()
                    .contains_key(class)
                    .then_some(Type::Unknown);
            }
        };
        if positional.len() > 64
            || keywords.len() > 64
            || keywords.iter().enumerate().any(|(index, (name, _))| {
                keywords[..index]
                    .iter()
                    .any(|(previous, _)| previous == name)
            })
        {
            return Some(Type::Unknown);
        }
        if !self.admit_functools_arguments(positional, keywords) {
            return Some(Type::Unknown);
        }
        Some(match value.kind {
            ValueKind::Partial {
                callable,
                positional: saved,
                keywords: bound,
            } => {
                let mut combined = saved;
                combined.extend_from_slice(positional);
                let keywords = merge_keywords(bound, keywords.to_vec());
                let signatures = self.functools_signatures(&callable);
                if combined.len() > 128
                    || !signatures
                        .iter()
                        .any(|signature| self.arguments_fit(signature, &combined, &keywords))
                {
                    Type::Unknown
                } else {
                    if matches!(&callable, Type::Class(..)) {
                        self.call_result(&callable, &combined, &keywords)
                    } else {
                        self.call_result(&signatures_type(signatures), &combined, &keywords)
                    }
                }
            }
            ValueKind::Wrapped { callable, .. } => {
                self.call_result(&callable, positional, keywords)
            }
            ValueKind::Decorator { wrapped } => {
                if positional.len() != 1
                    || !keywords.is_empty()
                    || self.functools_signatures(&positional[0]).is_empty()
                {
                    return Some(Type::Unknown);
                }
                let callable = positional[0].clone();
                // A decorator factory can be reused. Key the actual wrapper by
                // its physical function identity instead of overwriting the
                // factory's own retained value.
                let definition = match &callable {
                    Type::Function(signature) | Type::Callable(signature) => signature.def.as_ref(),
                    _ => None,
                };
                let Some(definition) = definition else {
                    return Some(Type::Unknown);
                };
                let key = ClassRef {
                    file: definition.file,
                    range: definition.range,
                    name: SmolStr::new("wrapped"),
                };
                self.store_functools_value(
                    key,
                    ValueKind::Wrapped { callable, wrapped },
                    value.source,
                    value.owner,
                )
            }
        })
    }
    pub(super) fn functools_members(&self, receiver: &Type) -> Option<IndexMap<SmolStr, Type>> {
        let value = self.functools_value(receiver)?;
        let names: &[&str] = match value.kind {
            ValueKind::Partial { .. } => &["func", "args", "keywords", "__call__"],
            ValueKind::Wrapped { .. } => &[
                "__wrapped__",
                "__name__",
                "__qualname__",
                "__module__",
                "__doc__",
                "__annotations__",
                "__call__",
            ],
            ValueKind::Decorator { .. } => &["__call__"],
        };
        Some(
            names
                .iter()
                .filter_map(|name| {
                    self.functools_member(receiver, name)
                        .map(|value| (SmolStr::new(name), value))
                })
                .collect(),
        )
    }
    pub(super) fn functools_member(&self, receiver: &Type, name: &str) -> Option<Type> {
        let value = match self.functools_value(receiver) {
            Some(value) => value,
            None => {
                let Type::Instance(class, _) = receiver else {
                    return None;
                };
                return self
                    .functools_values
                    .borrow()
                    .contains_key(class)
                    .then_some(Type::Unknown);
            }
        };
        Some(match (&value.kind, name) {
            (ValueKind::Partial { callable, .. }, "func") => callable.clone(),
            (ValueKind::Partial { positional, .. }, "args") => Type::Tuple(positional.clone()),
            (ValueKind::Partial { keywords, .. }, "keywords") => Type::Instance(
                self.builtin_class("dict")?,
                vec![
                    self.instance_of("str"),
                    Type::union(keywords.iter().map(|(_, ty)| ty.clone()).collect()),
                ],
            ),
            (ValueKind::Wrapped { wrapped, .. }, "__wrapped__") => wrapped.clone(),
            (ValueKind::Wrapped { .. }, "__name__" | "__qualname__" | "__module__") => {
                self.instance_of("str")
            }
            (ValueKind::Wrapped { .. }, "__doc__") => {
                Type::union(vec![self.instance_of("str"), Type::None])
            }
            (ValueKind::Wrapped { .. }, "__annotations__") => Type::Instance(
                self.builtin_class("dict")?,
                vec![self.instance_of("str"), Type::Any],
            ),
            (
                ValueKind::Partial {
                    callable,
                    positional,
                    keywords,
                },
                "__call__",
            ) => signatures_type(self.partial_signatures(callable, positional, keywords)),
            (ValueKind::Wrapped { callable, .. }, "__call__") => {
                signatures_type(self.functools_signatures(callable))
            }
            (ValueKind::Decorator { .. }, "__call__") => Type::Unknown,
            // Never send a synthetic ClassRef through physical class lookup.
            _ => Type::Unknown,
        })
    }
    fn functools_signatures(&self, callable: &Type) -> Vec<Arc<Signature>> {
        self.functools_signatures_inner(callable, 0)
    }
    fn functools_signatures_inner(&self, callable: &Type, depth: usize) -> Vec<Arc<Signature>> {
        if depth >= 16 {
            return Vec::new();
        }
        if let Some(value) = self.functools_value(callable) {
            return match value.kind {
                ValueKind::Wrapped { callable, .. } => {
                    self.functools_signatures_inner(&callable, depth + 1)
                }
                ValueKind::Partial {
                    callable,
                    positional,
                    keywords,
                } => self
                    .functools_signatures_inner(&callable, depth + 1)
                    .iter()
                    .filter_map(|signature| {
                        self.partial_signature(signature, &positional, &keywords)
                    })
                    .collect(),
                ValueKind::Decorator { .. } => Vec::new(),
            };
        }
        match callable {
            Type::Function(signature)
            | Type::Callable(signature)
            | Type::BoundMethod(signature, _) => vec![signature.clone()],
            Type::Overloaded(signatures) if signatures.len() <= 64 => signatures.clone(),
            Type::Instance(..) | Type::SelfType(_) => self
                .implicit_slot(callable, "__call__")
                .map(|value| self.functools_signatures_inner(&value, depth + 1))
                .unwrap_or_default(),
            Type::Class(class, args) => self
                .construction_signature(class, args, &[], &[])
                .map(|value| {
                    self.functools_signatures_inner(&value, depth + 1)
                        .into_iter()
                        .map(|signature| {
                            // __init__'s None is not the class-call return. The actual
                            // invocation uses class_call_result with all merged args;
                            // a shape-only residual signature cannot solve those args.
                            Arc::new(Signature {
                                ret: Type::Unknown,
                                ..(*signature).clone()
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }
    fn partial_signatures(
        &self,
        callable: &Type,
        positional: &[Type],
        keywords: &[(SmolStr, Type)],
    ) -> Vec<Arc<Signature>> {
        self.functools_signatures(callable)
            .iter()
            .filter_map(|signature| self.partial_signature(signature, positional, keywords))
            .collect()
    }
    fn partial_signature(
        &self,
        signature: &Signature,
        positional: &[Type],
        keywords: &[(SmolStr, Type)],
    ) -> Option<Arc<Signature>> {
        if signature.params.len() > 64 || positional.len() > 64 || keywords.len() > 64 {
            return None;
        }
        let mut remaining = Vec::new();
        let mut next = 0;
        let mut consumed = vec![false; keywords.len()];
        let mut keyword_barrier = false;
        let mut kwargs = false;
        for parameter in &signature.params {
            let mut parameter = parameter.clone();
            match parameter.kind {
                ParamKind::PositionalOnly | ParamKind::Normal => {
                    if let Some(actual) = positional.get(next) {
                        if keyword_barrier
                            || !self.is_compatible(actual, &parameter.ty)
                            || (parameter.kind == ParamKind::Normal
                                && keywords.iter().any(|(name, _)| name == &parameter.name))
                        {
                            return None;
                        }
                        next += 1;
                        continue;
                    }
                    if parameter.kind == ParamKind::Normal {
                        if let Some((index, (_, actual))) = keywords
                            .iter()
                            .enumerate()
                            .find(|(_, (name, _))| name == &parameter.name)
                        {
                            let _ = actual; // A later call may override this saved keyword.
                            consumed[index] = true;
                            parameter.has_default = true;
                            parameter.default_text = None;
                            keyword_barrier = true;
                        }
                        if keyword_barrier {
                            parameter.kind = ParamKind::KeywordOnly;
                        }
                    }
                }
                ParamKind::KeywordOnly => {
                    if let Some((index, (_, actual))) = keywords
                        .iter()
                        .enumerate()
                        .find(|(_, (name, _))| name == &parameter.name)
                    {
                        let _ = actual; // A later call may override this saved keyword.
                        consumed[index] = true;
                        parameter.has_default = true;
                        parameter.default_text = None;
                    }
                }
                ParamKind::VarArgs => {
                    while let Some(actual) = positional.get(next) {
                        if !self.is_compatible(actual, &parameter.ty) {
                            return None;
                        }
                        next += 1;
                    }
                    if keyword_barrier {
                        continue;
                    }
                }
                ParamKind::KwArgs => kwargs = true,
            }
            remaining.push(parameter);
        }
        if next != positional.len() {
            return None;
        }
        for (index, _) in keywords.iter().enumerate() {
            if !consumed[index] && !kwargs {
                return None;
            }
        }
        // Saved positional solutions cannot be overridden. Saved keyword
        // solutions deliberately remain free until the actual invocation.
        let mut substitutions = FxHashMap::default();
        self.solve_typevars(&signature.params, positional, &[], &mut substitutions);
        let result = Signature {
            params: remaining,
            ret: signature.ret.clone(),
            is_async: signature.is_async,
            def: signature.def.clone(),
            docstring: signature.docstring.clone(),
            ret_inferred: signature.ret_inferred,
        };
        Some(Arc::new(
            result.substitute(&|key| substitutions.get(key).cloned()),
        ))
    }
}
fn signatures_type(signatures: Vec<Arc<Signature>>) -> Type {
    match signatures.len() {
        0 => Type::Unknown,
        1 => Type::Callable(signatures[0].clone()),
        _ => Type::Overloaded(signatures),
    }
}
fn merge_keywords(
    mut saved: Vec<(SmolStr, Type)>,
    supplied: Vec<(SmolStr, Type)>,
) -> Vec<(SmolStr, Type)> {
    for (name, ty) in supplied {
        if let Some((_, old)) = saved.iter_mut().find(|(key, _)| key == &name) {
            *old = ty;
        } else {
            saved.push((name, ty));
        }
    }
    saved
}

impl Inferer<'_> {
    /// Descriptor/overload integration can inspect the real wrapper callable;
    /// copying metadata must never change its descriptor or return authority.
    pub(super) fn functools_bound_member(&self, value: &Type, receiver: &Type) -> Option<Type> {
        let callable = self.functools_callable(value)?;
        Some(self.bind_replacement_function_value(callable, receiver, true))
    }
    pub(super) fn functools_callable(&self, ty: &Type) -> Option<Type> {
        match self.functools_value(ty)?.kind {
            ValueKind::Wrapped { callable, .. } => Some(callable),
            _ => None,
        }
    }
    fn admit_functools_arguments(&self, positional: &[Type], keywords: &[(SmolStr, Type)]) -> bool {
        if positional.len() > 128
            || keywords.len() > 64
            || keywords.iter().any(|(name, _)| name.len() > 1024)
        {
            return false;
        }
        let mut captures = Vec::new();
        let mut budget = 2048;
        let mut bytes = 65536;
        positional
            .iter()
            .chain(keywords.iter().map(|(_, ty)| ty))
            .all(|ty| self.admit_functools_type(ty, &mut captures, &mut budget, &mut bytes, 0))
    }
    fn admit_functools_type(
        &self,
        ty: &Type,
        captures: &mut Vec<Arc<FileInfo>>,
        budget: &mut usize,
        bytes: &mut usize,
        depth: usize,
    ) -> bool {
        if depth >= 16 || *budget == 0 {
            return false;
        }
        *budget -= 1;
        let mut text = |value: &str| -> bool {
            if value.len() > *bytes {
                false
            } else {
                *bytes -= value.len();
                true
            }
        };
        let mut owners = Vec::new();
        let mut children: Vec<&Type> = Vec::new();
        let mut signatures: Vec<&Signature> = Vec::new();
        match ty {
            Type::Class(class, args) | Type::Instance(class, args) => {
                if !text(&class.name) {
                    return false;
                }
                owners.push(class.file);
                children.extend(args);
            }
            Type::Tuple(values) | Type::Union(values) | Type::TypePack(values) => {
                children.extend(values)
            }
            Type::Function(signature) | Type::Callable(signature) => signatures.push(signature),
            Type::BoundMethod(signature, receiver) => {
                signatures.push(signature);
                children.push(receiver);
            }
            Type::Overloaded(values) => {
                if values.len() > 64 {
                    return false;
                }
                signatures.extend(values.iter().map(AsRef::as_ref));
            }
            Type::DeclaredTypeVar(definition) => {
                if !text(&definition.key) || !text(&definition.name) {
                    return false;
                }
                children.extend(definition.constraints.iter());
                children.extend(definition.bound.as_deref());
                children.extend(definition.default.as_deref());
            }
            Type::TypeVar(name) => {
                if !text(name) {
                    return false;
                }
            }
            Type::ParamSpec(definition) => {
                if !text(&definition.key) || !text(&definition.name) {
                    return false;
                }
            }
            Type::TypeVarTuple(definition) => {
                if !text(&definition.key) || !text(&definition.name) {
                    return false;
                }
            }
            Type::Concatenate(values, definition) => {
                if !text(&definition.key) || !text(&definition.name) {
                    return false;
                }
                children.extend(values);
            }
            Type::Unpack(inner) => children.push(inner),
            Type::ParameterList(parameters) => {
                if parameters.len() > 64 {
                    return false;
                }
                for parameter in parameters {
                    if !text(&parameter.name)
                        || parameter
                            .default_text
                            .as_ref()
                            .is_some_and(|value| !text(value))
                    {
                        return false;
                    }
                    children.push(&parameter.ty);
                }
            }
            Type::Super { after, receiver } => {
                if !text(&after.name) {
                    return false;
                }
                owners.push(after.file);
                children.push(receiver);
            }
            Type::SelfType(class) => {
                if !text(&class.name) {
                    return false;
                }
                owners.push(class.file);
            }
            Type::Literal(LiteralValue::Str(value) | LiteralValue::Bytes(value)) => {
                if !text(value) {
                    return false;
                }
            }
            Type::Literal(LiteralValue::EnumMember { class, member }) => {
                if !text(&class.name) || !text(member) {
                    return false;
                }
                owners.push(class.file);
            }
            Type::Module(file) => owners.push(*file),
            Type::Unknown
            | Type::Any
            | Type::Never
            | Type::LiteralString
            | Type::LiteralStringValue
            | Type::None
            | Type::Literal(LiteralValue::Int(_) | LiteralValue::Bool(_)) => {}
        }
        for signature in signatures {
            if signature.params.len() > 64 {
                return false;
            }
            if let Some(definition) = &signature.def {
                if !text(&definition.name) {
                    return false;
                }
                owners.push(definition.file);
            }
            if signature
                .docstring
                .as_ref()
                .is_some_and(|value| !text(value))
            {
                return false;
            }
            for parameter in &signature.params {
                if !text(&parameter.name)
                    || parameter
                        .default_text
                        .as_ref()
                        .is_some_and(|value| !text(value))
                {
                    return false;
                }
                children.push(&parameter.ty);
            }
            children.push(&signature.ret);
        }
        if children.len() > 256 {
            return false;
        }
        for owner in owners {
            let Some(source) = self.db.file(owner) else {
                return false;
            };
            if !captures.iter().any(|old| Arc::ptr_eq(old, &source)) {
                if captures.len() >= 32
                    || source.text.len() > 4 * 1024 * 1024
                    || captures
                        .iter()
                        .map(|file| file.text.len())
                        .sum::<usize>()
                        .saturating_add(source.text.len())
                        > 8 * 1024 * 1024
                {
                    return false;
                }
                captures.push(source);
            }
        }
        children
            .into_iter()
            .all(|child| self.admit_functools_type(child, captures, budget, bytes, depth + 1))
    }
}

// Saved partial keyword dictionaries and copied wrapper metadata are mutable.
// Retire visible writes instead of treating a retained allocation as immutable.
fn functools_namespace_stable(file: &FileInfo) -> bool {
    if file.text.len() > 1024 * 1024 {
        return false;
    }
    let mut budget = 4096usize;
    for node in file.parse.syntax_node().descendants() {
        let Some(left) = budget.checked_sub(1) else {
            return false;
        };
        budget = left;
        if let Some(call) = ast::CallExpr::cast(node.clone()) {
            if let Some(ast::Expr::Attribute(method)) = call.callee() {
                if matches!(method.object(), Some(ast::Expr::Attribute(attribute)) if attribute.attr_text() == "keywords")
                {
                    return false;
                }
            }
        }
        let targets = if let Some(clause) = ast::CompFor::cast(node.clone()) {
            clause.target().into_iter().collect()
        } else if let Some(item) = ast::WithItem::cast(node.clone()) {
            item.target().into_iter().collect()
        } else {
            match ast::Stmt::cast(node) {
                Some(ast::Stmt::Assign(statement)) => statement.targets(),
                Some(ast::Stmt::AnnAssign(statement)) if statement.value().is_some() => {
                    statement.target().into_iter().collect()
                }
                Some(ast::Stmt::AugAssign(statement)) => statement.target().into_iter().collect(),
                Some(ast::Stmt::Del(statement)) => statement.targets(),
                Some(ast::Stmt::For(statement)) => statement.target().into_iter().collect(),
                _ => Vec::new(),
            }
        };
        for target in targets {
            for node in target.syntax().descendants() {
                let Some(left) = budget.checked_sub(1) else {
                    return false;
                };
                budget = left;
                if let Some(attribute) = ast::AttributeExpr::cast(node) {
                    if matches!(
                        attribute.attr_text().as_str(),
                        "keywords"
                            | "__wrapped__"
                            | "__annotations__"
                            | "__name__"
                            | "__qualname__"
                            | "__doc__"
                            | "__module__"
                    ) {
                        return false;
                    }
                }
            }
        }
    }
    true
}
