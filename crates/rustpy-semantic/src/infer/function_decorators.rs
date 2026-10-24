//! Function values after actual bottom-up decorator application.
use super::*;

#[derive(Clone, Copy)]
enum FunctionDecorator {
    Identity,
    Property,
    Context(bool),
    Replacement,
}

impl Inferer<'_> {
    /// Each declaration describes one discriminated callable alternative. Apply
    /// its stack independently; implementation-only decorators do not rewrite
    /// the explicit overload promises.
    pub(super) fn decorated_overload_signatures(
        &self,
        file: &Arc<FileInfo>,
        functions: &[&ast::FuncDef],
        class: Option<&ClassRef>,
        require_function_descriptor: bool,
    ) -> Option<Vec<Arc<Signature>>> {
        if functions.is_empty() || functions.len() > 128 {
            return None;
        }
        let mut signatures = Vec::with_capacity(functions.len());
        for function in functions {
            let value = self.decorated_function_value(file, function, class);
            let value = self.functools_callable(&value).unwrap_or(value);
            let signature = match value {
                Type::Function(signature) => signature,
                Type::Callable(signature) if !require_function_descriptor => signature,
                // Callable shape alone does not prove a function __get__.
                // A non-callable, unknown or union replacement is not a proven
                // overload alternative. Do not retain the physical signature.
                _ => return None,
            };
            signatures.push(signature);
        }
        Some(signatures)
    }
    pub(super) fn decorated_function_value(
        &self,
        file: &Arc<FileInfo>,
        function: &ast::FuncDef,
        class: Option<&ClassRef>,
    ) -> Type {
        let key = (file.id, function.text_range(), GuardKind::FunctionDecorator);
        if !self.enter(key) {
            return Type::Unknown;
        }
        let result = self.decorated_function_value_inner(file, function, class);
        self.leave(key);
        result
    }

    fn decorated_function_value_inner(
        &self,
        file: &Arc<FileInfo>,
        function: &ast::FuncDef,
        class: Option<&ClassRef>,
    ) -> Type {
        let decorators: Vec<_> = function.decorators().take(33).collect();
        if decorators.len() > 32 {
            return Type::Unknown;
        }
        // Capture every decorator expression in source order before application.
        let mut captured = Vec::with_capacity(decorators.len());
        for decorator in decorators {
            let Some(expression) = decorator.expr() else {
                return Type::Unknown;
            };
            let kind = self.function_decorator_kind(file, &expression);
            let value = if matches!(kind, FunctionDecorator::Replacement) {
                self.type_of_expr(file, &expression)
            } else {
                Type::Unknown
            };
            captured.push((kind, value));
        }
        let mut result = Type::Function(self.signature(file, function, class));
        for (kind, decorator) in captured.into_iter().rev() {
            result = match kind {
                FunctionDecorator::Identity => result,
                FunctionDecorator::Replacement => self.call_result(&decorator, &[result], &[]),
                FunctionDecorator::Property => match result {
                    Type::Function(signature) | Type::Callable(signature) => signature.ret.clone(),
                    _ => Type::Unknown,
                },
                FunctionDecorator::Context(asynchronous) => {
                    self.function_context_decorator(result, asynchronous)
                }
            };
        }
        result
    }

    fn function_decorator_kind(
        &self,
        file: &Arc<FileInfo>,
        expression: &ast::Expr,
    ) -> FunctionDecorator {
        let expression = expression.clone().unparen();
        let (target, factory) = match &expression {
            ast::Expr::Call(call) => (call.callee(), true),
            _ => (Some(expression.clone()), false),
        };
        let definition = match target {
            Some(ast::Expr::Name(name)) => {
                self.resolve_name(file, self.scope_of(file, name.syntax()), &name.name_text())
            }
            Some(ast::Expr::Attribute(attribute)) => attribute.object().and_then(|object| {
                let Type::Module(id) = self.type_of_expr(file, &object) else {
                    return None;
                };
                let module = self.db.file(id)?;
                self.resolve_in_module(&module, &attribute.attr_text(), 0)
            }),
            _ => None,
        };
        let Some(definition) = definition.map(|definition| self.follow_imports(definition)) else {
            return FunctionDecorator::Replacement;
        };
        let owner = &definition.file;
        let name = definition.binding().name.as_str();
        let Some(module) = owner.module_name.as_deref() else {
            return FunctionDecorator::Replacement;
        };
        if !owner.is_stub
            || owner.path != std::path::Path::new(&format!("<typeshed>/{module}.pyi"))
            || Self::namespace_member_retired(file, name, expression.text_range().start())
        {
            return FunctionDecorator::Replacement;
        }
        match (module, name, factory) {
            ("builtins", "property", false) | ("functools", "cached_property", false) => {
                FunctionDecorator::Property
            }
            ("contextlib", "contextmanager", false) => FunctionDecorator::Context(false),
            ("contextlib", "asynccontextmanager", false) => FunctionDecorator::Context(true),
            ("builtins", "staticmethod" | "classmethod", false)
            | (
                "typing" | "typing_extensions",
                "overload" | "final" | "override" | "type_check_only" | "no_type_check",
                false,
            )
            | ("abc", "abstractmethod", false)
            | ("typing" | "typing_extensions" | "warnings", "deprecated", true)
            | ("functools", "cache", false)
            | ("functools", "lru_cache", _) => FunctionDecorator::Identity,
            ("typing" | "typing_extensions", "dataclass_transform", true) => {
                FunctionDecorator::Identity
            }
            _ => FunctionDecorator::Replacement,
        }
    }

    /// Metadata-only members retain the established descriptor/overload route.
    pub(super) fn function_has_canonical_marker(
        &self,
        file: &Arc<FileInfo>,
        function: &ast::FuncDef,
        marker: &str,
    ) -> bool {
        function
            .decorators()
            .take(32)
            .filter_map(|decorator| decorator.expr())
            .any(|expression| {
                self.symbol_origin(file, &expression)
                    .is_some_and(|(_, name)| name == marker)
                    && matches!(
                        self.function_decorator_kind(file, &expression),
                        FunctionDecorator::Identity
                    )
            })
    }
    pub(super) fn has_replacement_function_decorator(
        &self,
        file: &Arc<FileInfo>,
        function: &ast::FuncDef,
    ) -> bool {
        let decorators: Vec<_> = function.decorators().take(33).collect();
        decorators.len() > 32
            || decorators.into_iter().any(|decorator| {
                decorator.expr().is_none_or(|expression| {
                    matches!(
                        self.function_decorator_kind(file, &expression),
                        FunctionDecorator::Replacement
                    )
                })
            })
    }

    pub(super) fn bind_replacement_function_value(
        &self,
        value: Type,
        receiver: &Type,
        bind: bool,
    ) -> Type {
        if !bind {
            return value;
        }
        if let Some(bound) = self.functools_bound_member(&value, receiver) {
            return bound;
        }
        // Function descriptors bind; arbitrary callable objects do not acquire
        // an invented self parameter. Callable annotations describe the returned
        // callable shape and are retained as such.
        match value {
            Type::Function(signature) => {
                let signature = if signature.params.is_empty() {
                    signature
                } else {
                    Arc::new(Signature {
                        params: signature.params[1..].to_vec(),
                        ..(*signature).clone()
                    })
                };
                Type::BoundMethod(signature, Box::new(receiver.clone()))
            }
            other => other,
        }
    }

    fn function_context_decorator(&self, value: Type, asynchronous: bool) -> Type {
        let Type::Function(signature) = value else {
            return Type::Unknown;
        };
        let name = if asynchronous {
            "_AsyncGeneratorContextManager"
        } else {
            "_GeneratorContextManager"
        };
        let Some(module) = self.db.module("contextlib") else {
            return Type::Unknown;
        };
        if !module.is_stub
            || module.path != std::path::Path::new("<typeshed>/contextlib.pyi")
            || module.module_name.as_deref() != Some("contextlib")
        {
            return Type::Unknown;
        }
        let Some(id) = module
            .scopes
            .bindings_in(module.scopes.module_scope(), name)
            .iter()
            .find(|id| matches!(module.scopes.binding(**id).kind, BindingKind::Class))
        else {
            return Type::Unknown;
        };
        let binding = module.scopes.binding(*id);
        let class = ClassRef {
            file: module.id,
            range: binding.node_range,
            name: binding.name.clone(),
        };
        let yielded = match &signature.ret {
            Type::Instance(generator, arguments)
                if self.db.file(generator.file).is_some_and(|owner| {
                    owner.is_stub
                        && owner.path == std::path::Path::new("<typeshed>/typing.pyi")
                        && owner.module_name.as_deref() == Some("typing")
                        && if asynchronous {
                            matches!(
                                generator.name.as_str(),
                                "AsyncGenerator" | "AsyncIterator" | "AsyncIterable"
                            )
                        } else {
                            matches!(
                                generator.name.as_str(),
                                "Generator" | "Iterator" | "Iterable"
                            )
                        }
                }) =>
            {
                arguments.first().cloned().unwrap_or(Type::Unknown)
            }
            _ => Type::Unknown,
        };
        let count = self
            .class_info(&class)
            .map(|info| info.type_params.len())
            .unwrap_or(1)
            .max(1);
        let mut arguments = vec![yielded];
        arguments.resize(count, Type::None);
        Type::Function(Arc::new(Signature {
            ret: Type::Instance(class, arguments),
            ret_inferred: false,
            ..(*signature).clone()
        }))
    }
}
