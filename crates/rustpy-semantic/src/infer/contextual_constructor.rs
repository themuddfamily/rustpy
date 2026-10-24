//! Context fills only unsolved ordinary-constructor parameters before defaults.
use super::*;

impl Inferer<'_> {
    pub(super) fn contextual_constructor(
        &self,
        file: &Arc<FileInfo>,
        call: &ast::CallExpr,
        class: &ClassRef,
        explicit: &[Type],
        expected: &Type,
    ) -> Option<Type> {
        let Type::Instance(expected_class, expected_arguments) = expected else {
            return None;
        };
        if expected_class != class || !explicit.is_empty() {
            return None;
        }
        let parameters = self.class_parameter_types(class);
        if parameters.is_empty()
            || parameters.len() > 32
            || parameters
                .iter()
                .any(|parameter| !matches!(parameter, Type::DeclaredTypeVar(_) | Type::TypeVar(_)))
        {
            return None;
        }
        if !bounded_context_type(&Type::Tuple(parameters.clone()), 0, &mut 512, &mut false) {
            return None;
        }
        // A custom metaclass, new method, or decorator owns the return. Decline
        // expected construction for the complete nominal ancestry, not just C.
        if self.has_unknown_base(class) {
            return None;
        }
        let owners = self.mro(class);
        if owners.is_empty() || owners.len() > 32 {
            return None;
        }
        let object = self.builtin_class("object");
        for owner in owners.iter() {
            if object.as_ref() == Some(owner) {
                continue;
            }
            let info = self.class_info(owner)?;
            let owner_file = self.db.file(owner.file)?;
            for range in &info.base_exprs {
                let expression = node_with_range(&owner_file.parse.syntax_node(), *range, |node| {
                    ast::Expr::can_cast(node.kind())
                })
                .and_then(ast::Expr::cast)?;
                if !self.context_constructor_base_known(&owner_file, &expression) {
                    return None;
                }
            }
            if !info.keywords.is_empty()
                || !info.decorators.is_empty()
                || info.members.contains_key("__new__")
            {
                return None;
            }
        }
        let arguments: Vec<_> = call.arg_list()?.args().take(33).collect();
        if arguments.len() > 32
            || arguments
                .iter()
                .any(|argument| matches!(argument, ast::AnyArg::Star(_) | ast::AnyArg::StarStar(_)))
        {
            return None;
        }
        let (positional, keywords) = self.call_args(file, call);
        let mut substitutions = FxHashMap::default();
        if let Some((constructor, _)) = self.find_constructor(class) {
            let signatures = match constructor {
                Type::Overloaded(signatures) => signatures,
                other => other.signature().cloned().into_iter().collect(),
            };
            if !signatures.is_empty() {
                if signatures.len() > 32 {
                    return None;
                }
                let signature = self.select_overload(&signatures, &positional, &keywords);
                if !bounded_context_type(
                    &Type::Function(signature.clone()),
                    0,
                    &mut 512,
                    &mut false,
                ) || signature.params.len() > 32
                {
                    return None;
                }
                self.solve_typevars(
                    &signature.params,
                    &positional,
                    &keywords,
                    &mut substitutions,
                );
            }
        }
        // An unresolved expected variable is no evidence for a constructor
        // argument. Keep its slot vacant so the actual class default can solve it.
        for (parameter, hint) in parameters.iter().zip(expected_arguments) {
            let mut unresolved = FxHashSet::default();
            collect_scalar_context_keys(hint, &mut unresolved, 0, &mut 512);
            if unresolved.is_empty() && !matches!(hint, Type::Unknown | Type::Any) {
                self.contextual_return_solutions(parameter, hint, &mut substitutions);
            }
        }
        self.solve_generic_defaults(&parameters, &mut substitutions);
        let solved = parameters
            .iter()
            .map(|parameter| {
                Self::typevar_substitution_key(parameter)
                    .and_then(|key| substitutions.get(key))
                    .cloned()
                    .unwrap_or(Type::Unknown)
            })
            .collect::<Vec<_>>();
        // Reuse actual dispatch and checks; __init__'s return is never the result.
        Some(self.class_call_result(class, &solved, &positional, &keywords))
    }
    fn context_constructor_base_known(&self, file: &Arc<FileInfo>, expression: &ast::Expr) -> bool {
        let expression = expression.clone().unparen();
        if matches!(expression, ast::Expr::Call(_)) {
            return false;
        }
        if let ast::Expr::Subscript(subscript) = &expression {
            if let Some(head) = subscript.object() {
                if let Type::Class(class, _) = self.type_of_expr(file, &head) {
                    if self.mro(&class).iter().any(|owner| {
                        self.db.file(owner.file).is_some_and(|source| {
                            !(source.is_stub
                                && source.path.to_string_lossy().starts_with("<typeshed>"))
                        }) && self
                            .class_info(owner)
                            .is_some_and(|info| info.members.contains_key("__class_getitem__"))
                    }) {
                        return false;
                    }
                }
            }
        }
        if matches!(
            self.annotation_type(file, &expression),
            Type::Instance(..) | Type::Class(..)
        ) {
            return true;
        }
        let head = match expression {
            ast::Expr::Subscript(subscript) => subscript.object(),
            other => Some(other),
        };
        let Some(head) = head else {
            return false;
        };
        let definition = match head.unparen() {
            ast::Expr::Name(name) => {
                self.resolve_name(file, self.scope_of(file, name.syntax()), &name.name_text())
            }
            ast::Expr::Attribute(attribute) => {
                let Some(object) = attribute.object() else {
                    return false;
                };
                let Type::Module(id) = self.type_of_expr(file, &object) else {
                    return false;
                };
                let Some(module) = self.db.file(id) else {
                    return false;
                };
                self.resolve_name(
                    &module,
                    module.scopes.module_scope(),
                    &attribute.attr_text(),
                )
            }
            _ => None,
        };
        let Some(definition) = definition else {
            return false;
        };
        let definition = self.follow_imports(definition);
        definition.file.is_stub
            && definition
                .file
                .path
                .to_string_lossy()
                .starts_with("<typeshed>")
            && matches!(
                definition.file.module_name.as_deref(),
                Some("typing" | "typing_extensions")
            )
            && matches!(definition.binding().name.as_str(), "Generic" | "Protocol")
    }
}
