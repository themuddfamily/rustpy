//! Declared generic contracts retain declaration identity, not display-name identity.
use super::*;
#[path = "generic_variance.rs"]
mod generic_variance;
use crate::types::{TypeVarDefinition, Variance};
use rustc_hash::FxHashSet;

impl Inferer<'_> {
    pub(super) fn canonical_typevar_factory(
        &self,
        file: &Arc<FileInfo>,
        expression: &ast::Expr,
        seen: &mut FxHashSet<(FileId, BindingId)>,
    ) -> bool {
        if seen.len() >= 16 {
            return false;
        }
        match expression.clone().unparen() {
            ast::Expr::Name(name) => {
                let scope = self.scope_of(file, name.syntax());
                if let Some(flow_scope) = (!file.is_stub)
                    .then(|| crate::flow::flow_scope(file, scope))
                    .flatten()
                {
                    let info = self.scope_flow(file, flow_scope);
                    if let Some(reaches) = info.refs.get(&name.text_range().start()) {
                        return !reaches.is_empty()
                            && reaches.iter().all(|reach| {
                                reach.binding.is_some_and(|id| {
                                    self.canonical_typevar_binding(file, id, seen)
                                })
                            });
                    }
                }
                self.resolve_name(file, scope, &name.name_text())
                    .is_some_and(|definition| {
                        self.canonical_typevar_binding(&definition.file, definition.binding, seen)
                    })
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
                .is_some_and(|definition| {
                    self.canonical_typevar_binding(&definition.file, definition.binding, seen)
                })
            }
            _ => false,
        }
    }
    fn canonical_typevar_binding(
        &self,
        file: &Arc<FileInfo>,
        id: BindingId,
        seen: &mut FxHashSet<(FileId, BindingId)>,
    ) -> bool {
        if seen.len() >= 16 || !seen.insert((file.id, id)) {
            return false;
        }
        let definition = self.follow_imports(Definition {
            file: file.clone(),
            binding: id,
        });
        let binding = definition.binding();
        let result = if definition.file.is_stub
            && definition
                .file
                .path
                .to_string_lossy()
                .starts_with("<typeshed>")
            && matches!(
                definition.file.module_name.as_deref(),
                Some("typing" | "typing_extensions")
            )
            && binding.name == "TypeVar"
        {
            true
        } else if matches!(binding.kind, BindingKind::Assignment) {
            binding
                .value
                .and_then(|range| {
                    node_with_range(&definition.file.parse.syntax_node(), range, |node| {
                        ast::Expr::can_cast(node.kind())
                    })
                })
                .and_then(ast::Expr::cast)
                .is_some_and(|value| self.canonical_typevar_factory(&definition.file, &value, seen))
        } else {
            false
        };
        seen.remove(&(file.id, id));
        result
    }

    pub(super) fn canonical_typevar_class(&self, class: &ClassRef) -> bool {
        matches!(
            class.name.as_str(),
            "TypeVar" | "ParamSpec" | "TypeVarTuple"
        ) && self.db.file(class.file).is_some_and(|file| {
            file.is_stub
                && file.path.to_string_lossy().starts_with("<typeshed>")
                && matches!(
                    file.module_name.as_deref(),
                    Some("typing" | "typing_extensions")
                )
        })
    }

    pub(super) fn declared_typevar_call(
        &self,
        file: &Arc<FileInfo>,
        call: &ast::CallExpr,
    ) -> Option<Type> {
        let callee = call.callee()?;
        if !self.canonical_typevar_factory(file, &callee, &mut FxHashSet::default()) {
            return None;
        }
        let (index, binding) = file
            .scopes
            .bindings
            .iter()
            .enumerate()
            .find(|(_, binding)| {
                matches!(binding.kind, BindingKind::Assignment)
                    && binding
                        .value
                        .and_then(|range| {
                            node_with_range(&file.parse.syntax_node(), range, |node| {
                                ast::Expr::can_cast(node.kind())
                            })
                        })
                        .and_then(ast::Expr::cast)
                        .is_some_and(|value| value.unparen().text_range() == call.text_range())
            })?;
        let arguments = call.arg_list()?;
        if arguments.args().any(|argument| {
            matches!(
                argument,
                ast::AnyArg::Star(_) | ast::AnyArg::StarStar(_) | ast::AnyArg::Generator(_)
            )
        }) {
            return Some(Type::Unknown);
        }
        let positional: Vec<_> = arguments.positional().take(34).collect();
        if positional.is_empty() || positional.len() > 33 {
            return Some(Type::Unknown);
        }
        let ast::Expr::String(name) = &positional[0] else {
            return Some(Type::Unknown);
        };
        let Ok(rustpy_syntax::string_literal::Decoded::Text(name, _)) =
            rustpy_syntax::string_literal::string(&name.raw_text())
        else {
            return Some(Type::Unknown);
        };
        if name.as_str() != binding.name.as_str() {
            return Some(Type::Unknown);
        }
        let mut bound = None;
        let mut default = None;
        let mut infer_variance = false;
        let mut covariance = false;
        let mut contravariance = false;
        let mut keywords = FxHashSet::default();
        for keyword in arguments.keywords() {
            let key = keyword.name()?.text().to_owned();
            if !keywords.insert(key.clone()) {
                return Some(Type::Unknown);
            }
            let value = keyword.value()?;
            match key.as_str() {
                "bound" => {
                    if !matches!(value.clone().unparen(), ast::Expr::Literal(literal) if literal.kind() == Some(SyntaxKind::NONE_KW))
                    {
                        bound = Some(Box::new(self.annotation_type(file, &value)));
                    }
                }
                "default" => default = Some(Box::new(self.annotation_type(file, &value))),
                "covariant" | "contravariant" | "infer_variance" => {
                    let ast::Expr::Literal(literal) = value.unparen() else {
                        return Some(Type::Unknown);
                    };
                    let truth = match literal.kind()? {
                        SyntaxKind::TRUE_KW => true,
                        SyntaxKind::FALSE_KW => false,
                        _ => return Some(Type::Unknown),
                    };
                    if key == "covariant" {
                        covariance = truth;
                    } else if key == "contravariant" {
                        contravariance = truth;
                    } else {
                        infer_variance = truth;
                    }
                }
                _ => return Some(Type::Unknown),
            }
        }
        let constraints: Vec<_> = positional[1..]
            .iter()
            .map(|value| self.annotation_type(file, value))
            .collect();
        if constraints.len() == 1
            || !constraints.is_empty() && bound.is_some()
            || covariance && contravariance
            || infer_variance && (covariance || contravariance)
        {
            return Some(Type::Unknown);
        }
        Some(Type::DeclaredTypeVar(Arc::new(TypeVarDefinition {
            key: Self::typevar_key(file.id, file.scopes.binding(BindingId(index as u32)).range),
            name: name.into(),
            bound,
            constraints,
            default,
            variance: if infer_variance {
                Variance::Inferred
            } else if covariance {
                Variance::Covariant
            } else if contravariance {
                Variance::Contravariant
            } else {
                Variance::Invariant
            },
        })))
    }

    fn typevar_key(file: FileId, range: TextRange) -> SmolStr {
        format!("\0typevar:{}:{}", file.0, usize::from(range.start())).into()
    }

    pub(super) fn declared_type_parameter(
        &self,
        file: &Arc<FileInfo>,
        binding: &crate::scope::Binding,
    ) -> Type {
        let node = node_with_range(&file.parse.syntax_node(), binding.node_range, |_| true);
        if node.as_ref().is_none_or(|node| {
            !matches!(
                node.kind(),
                SyntaxKind::TYPE_PARAM
                    | SyntaxKind::PARAM_SPEC_PARAM
                    | SyntaxKind::TYPE_VAR_TUPLE_PARAM
            )
        }) {
            return Type::TypeVar(binding.name.clone());
        }
        if let Some(specification) = node
            .as_ref()
            .and_then(|node| self.declared_paramspec_parameter(file, binding, node))
        {
            return specification;
        }
        if let Some(pack) = node
            .as_ref()
            .and_then(|node| self.declared_tuple_pack_parameter(file, binding, node))
        {
            return pack;
        }
        let annotation = node
            .as_ref()
            .and_then(|node| {
                node.children()
                    .find(|child| child.kind() == SyntaxKind::ANNOTATION)
            })
            .and_then(|node| node.children().find_map(ast::Expr::cast));
        let (bound, constraints) = match annotation.map(ast::Expr::unparen) {
            Some(ast::Expr::Tuple(tuple)) => (
                None,
                tuple
                    .elements()
                    .map(|expression| self.annotation_type(file, &expression))
                    .collect(),
            ),
            Some(expression) => (
                Some(Box::new(self.annotation_type(file, &expression))),
                Vec::new(),
            ),
            None => (None, Vec::new()),
        };
        if constraints.len() == 1 {
            return Type::Unknown;
        }
        let default = node
            .as_ref()
            .and_then(|node| {
                node.children()
                    .find(|child| child.kind() == SyntaxKind::DEFAULT_VALUE)
            })
            .and_then(|node| node.children().find_map(ast::Expr::cast))
            .map(|expression| Box::new(self.annotation_type(file, &expression)));
        Type::DeclaredTypeVar(Arc::new(TypeVarDefinition {
            key: Self::typevar_key(file.id, binding.range),
            name: binding.name.clone(),
            bound,
            constraints,
            variance: Variance::Inferred,
            default,
        }))
    }

    pub(super) fn class_parameter_types(&self, class: &ClassRef) -> Vec<Type> {
        let Some(info) = self.class_info(class) else {
            return Vec::new();
        };
        let Some(file) = self.db.file(class.file) else {
            return Vec::new();
        };
        let Some(scope) = file.scopes.scope_for_node(class.range) else {
            return Vec::new();
        };
        let node = node_with_range(&file.parse.syntax_node(), class.range, |node| {
            node.kind() == SyntaxKind::CLASS_DEF
        })
        .and_then(ast::ClassDef::cast);
        info.type_params
            .iter()
            .map(|name| {
                if let Some(node) = &node {
                    if node.type_params().is_none() {
                        for base in node.bases() {
                            if let ast::Expr::Subscript(subscript) = base {
                                if let Some(expression) = subscript.index().and_then(|index| {
                                    index.descendants().find_map(|node| {
                                        let expression = ast::NameExpr::cast(node)?;
                                        (expression.name_text() == name.as_str())
                                            .then_some(expression)
                                    })
                                }) {
                                    return self.type_of_expr(&file, &ast::Expr::Name(expression));
                                }
                            }
                        }
                    }
                }
                // PEP695 declarations reside in the annotation scope enclosing
                // the class, not its subsequently executed namespace.
                let defining = file.scopes.scope(scope).parent.unwrap_or(scope);
                self.resolve_name(&file, defining, name)
                    .map(|definition| self.type_of_binding(&definition.file, definition.binding))
                    .unwrap_or_else(|| Type::TypeVar(name.clone()))
            })
            .collect()
    }
    pub(super) fn typevar_substitution_key(variable: &Type) -> Option<&str> {
        match variable {
            Type::DeclaredTypeVar(definition) => Some(&definition.key),
            Type::TypeVar(name) => Some(name),
            Type::ParamSpec(definition) => Some(&definition.key),
            Type::TypeVarTuple(definition) => Some(&definition.key),
            _ => None,
        }
    }
    pub(super) fn class_substitution(
        &self,
        class: &ClassRef,
        arguments: &[Type],
        name: &str,
    ) -> Option<Type> {
        let parameters = self.class_parameter_types(class);
        let index = parameters
            .iter()
            .position(|parameter| Self::typevar_substitution_key(parameter) == Some(name))?;
        let pack = parameters
            .iter()
            .position(|parameter| matches!(parameter, Type::TypeVarTuple(_)));
        if let Some(pack) = pack {
            let suffix = parameters.len() - pack - 1;
            let end = arguments.len().checked_sub(suffix)?;
            if end < pack {
                return None;
            }
            if index == pack {
                return Some(Type::TypePack(arguments.get(pack..end)?.to_vec()));
            }
            if index > pack {
                return arguments.get(end + index - pack - 1).cloned();
            }
        }
        arguments.get(index).cloned()
    }

    pub(super) fn accepts_typevar(&self, definition: &TypeVarDefinition, argument: &Type) -> bool {
        if matches!(argument, Type::Unknown | Type::Any) {
            return true;
        }
        if let Some(bound) = &definition.bound {
            return self.is_compatible(argument, bound);
        }
        definition.constraints.is_empty()
            || definition
                .constraints
                .iter()
                .any(|constraint| self.is_compatible(argument, constraint))
    }
    pub(super) fn solve_declared_typevar(
        &self,
        definition: &TypeVarDefinition,
        argument: &Type,
        substitutions: &mut FxHashMap<SmolStr, Type>,
    ) {
        if argument.is_unknown() {
            return;
        }
        if matches!(argument, Type::Any) {
            substitutions
                .entry(definition.key.clone())
                .or_insert(Type::Any);
            return;
        }
        // Unknown stored here denotes a conflicting/invalid solution, and is
        // sticky. It is not a terminal result type.
        if substitutions.get(&definition.key) == Some(&Type::Unknown) {
            return;
        }
        let candidate = if !definition.constraints.is_empty() {
            definition
                .constraints
                .iter()
                .filter(|constraint| self.is_compatible(argument, constraint))
                .max_by_key(|constraint| {
                    definition
                        .constraints
                        .iter()
                        .filter(|other| {
                            *other != *constraint && self.is_compatible(constraint, other)
                        })
                        .count()
                })
                .cloned()
        } else if self.accepts_typevar(definition, argument) {
            Some(argument.clone())
        } else {
            None
        };
        let Some(candidate) = candidate else {
            substitutions.insert(definition.key.clone(), Type::Unknown);
            return;
        };
        match substitutions.get(&definition.key).cloned() {
            None => {
                substitutions.insert(definition.key.clone(), candidate);
            }
            Some(Type::Any) => {
                substitutions.insert(definition.key.clone(), candidate);
            }
            Some(previous) if previous == candidate => {}
            Some(previous) if definition.constraints.is_empty() => {
                substitutions.insert(
                    definition.key.clone(),
                    Type::union(vec![previous, candidate]),
                );
            }
            Some(_) => {
                substitutions.insert(definition.key.clone(), Type::Unknown);
            }
        }
    }

    pub(super) fn generic_parameter_variance(parameter: &Type) -> Option<(&str, Variance)> {
        match parameter {
            Type::DeclaredTypeVar(value) => Some((&value.key, value.variance)),
            Type::ParamSpec(value) => Some((&value.key, value.variance)),
            Type::TypeVarTuple(value) => Some((&value.key, value.variance)),
            Type::Unpack(value) => Self::generic_parameter_variance(value),
            _ => None,
        }
    }
    pub(super) fn expanded_class_parameters<'p>(
        &self,
        parameters: &'p [Type],
        count: usize,
    ) -> Option<Vec<&'p Type>> {
        if parameters.len() > 128 || count > 128 {
            return None;
        }
        let packs: Vec<_> = parameters.iter().enumerate().filter(|(_,parameter)| matches!(parameter,Type::TypeVarTuple(_)) || matches!(parameter,Type::Unpack(value) if matches!(&**value,Type::TypeVarTuple(_)))).collect();
        if packs.len() > 1 {
            return None;
        }
        let Some((index, pack)) = packs.first().copied() else {
            return (parameters.len() == count).then(|| parameters.iter().collect());
        };
        let fixed = parameters.len() - 1;
        if count < fixed {
            return None;
        }
        let mut result: Vec<_> = parameters[..index].iter().collect();
        result.extend(std::iter::repeat_n(pack, count - fixed));
        result.extend(parameters[index + 1..].iter());
        Some(result)
    }
    pub(super) fn normalize_parameter_arguments(
        &self,
        parameters: &[Type],
        arguments: &[Type],
    ) -> Vec<Type> {
        let Some(parameters) = self.expanded_class_parameters(parameters, arguments.len()) else {
            return arguments.to_vec();
        };
        parameters
            .into_iter()
            .zip(arguments)
            .map(|(parameter, argument)| {
                if matches!(parameter, Type::ParamSpec(_)) {
                    if let Type::Tuple(types) = argument {
                        return Type::ParameterList(
                            types
                                .iter()
                                .enumerate()
                                .map(|(index, ty)| Param {
                                    name: format!("p{index}").into(),
                                    kind: ParamKind::PositionalOnly,
                                    ty: ty.clone(),
                                    has_default: false,
                                    default_text: None,
                                })
                                .collect(),
                        );
                    }
                }
                argument.clone()
            })
            .collect()
    }
    pub(super) fn generic_arguments_compatible(
        &self,
        class: &ClassRef,
        actual: &[Type],
        expected: &[Type],
    ) -> bool {
        if actual.is_empty() || expected.is_empty() {
            return true;
        }
        if actual.len() != expected.len() {
            return false;
        }
        let parameters = self.class_parameter_types(class);
        let Some(parameters) = self.expanded_class_parameters(&parameters, actual.len()) else {
            return false;
        };
        actual
            .iter()
            .zip(expected)
            .zip(parameters)
            .all(|((argument, parameter), definition)| {
                if matches!(parameter, Type::TypeVar(_) | Type::DeclaredTypeVar(_)) {
                    return self.is_compatible(argument, parameter);
                }
                let (key, variance) = Self::generic_parameter_variance(definition)
                    .unwrap_or(("", Variance::Invariant));
                let variance = if variance == Variance::Inferred {
                    self.inferred_generic_variance(class, key)
                } else {
                    variance
                };
                match variance {
                    Variance::Covariant => self.is_compatible(argument, parameter),
                    Variance::Contravariant => self.is_compatible(parameter, argument),
                    _ => {
                        self.is_compatible(argument, parameter)
                            && self.is_compatible(parameter, argument)
                    }
                }
            })
    }
}
