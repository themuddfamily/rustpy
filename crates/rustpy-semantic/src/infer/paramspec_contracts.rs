//! Source-keyed parameter specifications. No inference callback or global cache.
use super::*;
use crate::types::ParameterSpecDefinition;
use crate::types::Variance;
use rustc_hash::FxHashSet;

impl Inferer<'_> {
    fn parameter_namespace_admitted(
        &self,
        file: &Arc<FileInfo>,
        expression: &ast::Expr,
        wanted: &str,
    ) -> bool {
        if file.is_stub {
            return true;
        }
        let mut nodes = file.parse.syntax_node().descendants();
        for _ in 0..4096 {
            let Some(node) = nodes.next() else {
                break;
            };
            let targets = match ast::Stmt::cast(node) {
                Some(ast::Stmt::Assign(assign)) => assign.targets(),
                Some(ast::Stmt::AnnAssign(assign)) if assign.value().is_some() => {
                    assign.target().into_iter().collect()
                }
                Some(ast::Stmt::AugAssign(assign)) => assign.target().into_iter().collect(),
                Some(ast::Stmt::Del(delete)) => delete.targets(),
                _ => Vec::new(),
            };
            for target in targets {
                let mut target_nodes = target.syntax().descendants();
                for _ in 0..64 {
                    let Some(node) = target_nodes.next() else {
                        break;
                    };
                    if ast::Expr::cast(node).is_some_and(|expression| matches!(expression.unparen(), ast::Expr::Attribute(attribute) if attribute.attr_text() == wanted)) { return false; }
                }
                if target_nodes.next().is_some() {
                    return false;
                }
            }
        }
        if nodes.next().is_some() {
            return false;
        }
        let mut scope = Some(self.scope_of(file, expression.syntax()));
        while let Some(id) = scope {
            let held = file.scopes.scope(id);
            if held.kind == ScopeKind::Function || held.kind == ScopeKind::Lambda {
                break;
            }
            if held.kind == ScopeKind::Class {
                let Some(class) = node_with_range(&file.parse.syntax_node(), held.range, |node| {
                    node.kind() == SyntaxKind::CLASS_DEF
                })
                .and_then(ast::ClassDef::cast) else {
                    return false;
                };
                if class
                    .arguments()
                    .is_some_and(|args| args.args().next().is_some())
                {
                    return false;
                }
            }
            scope = held.parent;
        }
        true
    }
    pub(super) fn canonical_parameter_symbol(
        &self,
        file: &Arc<FileInfo>,
        expression: &ast::Expr,
        wanted: &str,
        seen: &mut FxHashSet<(FileId, BindingId)>,
    ) -> bool {
        if seen.len() >= 16 {
            return false;
        }
        // The namespace scan visits up to 4096 nodes of the file. Answer the
        // (pure) symbol question first: almost every callee is not a symbol.
        let symbol = 'symbol: {
            match expression.clone().unparen() {
                ast::Expr::Name(name) => {
                    let scope = self.scope_of(file, name.syntax());
                    if let Some(flow_scope) = (!file.is_stub)
                        .then(|| crate::flow::flow_scope(file, scope))
                        .flatten()
                    {
                        let flow = self.scope_flow(file, flow_scope);
                        if let Some(reaches) = flow.refs.get(&name.text_range().start()) {
                            break 'symbol !reaches.is_empty()
                                && reaches.iter().all(|reach| {
                                    reach.binding.is_some_and(|id| {
                                        self.parameter_symbol_binding(file, id, wanted, seen)
                                    })
                                });
                        }
                    }
                    self.resolve_name(file, scope, &name.name_text())
                        .is_some_and(|definition| {
                            self.parameter_symbol_binding(
                                &definition.file,
                                definition.binding,
                                wanted,
                                seen,
                            )
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
                        self.parameter_symbol_binding(
                            &definition.file,
                            definition.binding,
                            wanted,
                            seen,
                        )
                    })
                }
                _ => false,
            }
        };
        symbol && self.parameter_namespace_admitted(file, expression, wanted)
    }
    fn parameter_symbol_binding(
        &self,
        file: &Arc<FileInfo>,
        id: BindingId,
        wanted: &str,
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
        let result = if definition.file.is_stub && binding.name == wanted {
            definition
                .file
                .module_name
                .as_deref()
                .is_some_and(|module| {
                    matches!(
                        module,
                        "typing" | "typing_extensions" | "collections.abc" | "_collections_abc"
                    ) && definition.file.path
                        == std::path::Path::new(&format!("<typeshed>/{module}.pyi"))
                })
        } else if binding.kind == BindingKind::Assignment {
            binding
                .value
                .and_then(|range| {
                    node_with_range(&definition.file.parse.syntax_node(), range, |node| {
                        ast::Expr::can_cast(node.kind())
                    })
                })
                .and_then(ast::Expr::cast)
                .is_some_and(|value| {
                    self.canonical_parameter_symbol(&definition.file, &value, wanted, seen)
                })
        } else {
            false
        };
        seen.remove(&(file.id, id));
        result
    }
    pub(super) fn declared_paramspec_call(
        &self,
        file: &Arc<FileInfo>,
        call: &ast::CallExpr,
    ) -> Option<Type> {
        let callee = call.callee()?;
        if !self.canonical_parameter_symbol(file, &callee, "ParamSpec", &mut FxHashSet::default()) {
            return None;
        }
        let Some(arguments) = call.arg_list() else {
            return Some(Type::Unknown);
        };
        let actual: Vec<_> = arguments.args().take(6).collect();
        if actual.is_empty() || actual.len() > 5 || !matches!(actual[0], ast::AnyArg::Positional(_))
        {
            return Some(Type::Unknown);
        }
        let mut seen = FxHashSet::default();
        let mut covariance = false;
        let mut contravariance = false;
        let mut infer_variance = false;
        for argument in &actual[1..] {
            let ast::AnyArg::Keyword(keyword) = argument else {
                return Some(Type::Unknown);
            };
            let name = keyword.name_text();
            if !seen.insert(name.clone()) {
                return Some(Type::Unknown);
            }
            let Some(value) = keyword.value() else {
                return Some(Type::Unknown);
            };
            match name.as_str() {
                "bound" => {
                    if !matches!(value.clone().unparen(), ast::Expr::Literal(literal) if literal.kind() == Some(SyntaxKind::NONE_KW))
                        && !matches!(self.type_of_expr(file, &value), Type::Class(_, _))
                    {
                        return Some(Type::Unknown);
                    }
                }
                "covariant" | "contravariant" | "infer_variance" => {
                    let ast::Expr::Literal(literal) = value.unparen() else {
                        return Some(Type::Unknown);
                    };
                    let truth = match literal.kind() {
                        Some(SyntaxKind::TRUE_KW) => true,
                        Some(SyntaxKind::FALSE_KW) => false,
                        _ => return Some(Type::Unknown),
                    };
                    if name == "covariant" {
                        covariance = truth;
                    } else if name == "contravariant" {
                        contravariance = truth;
                    } else {
                        infer_variance = truth;
                    }
                }
                _ => return Some(Type::Unknown),
            }
        }
        if (covariance as u8 + contravariance as u8 + infer_variance as u8) > 1 {
            return Some(Type::Unknown);
        }
        let Some(ast::Expr::String(name)) = arguments.positional().next() else {
            return Some(Type::Unknown);
        };
        let Ok(rustpy_syntax::string_literal::Decoded::Text(name, _)) =
            rustpy_syntax::string_literal::string(&name.raw_text())
        else {
            return Some(Type::Unknown);
        };
        let Some(binding) = file.scopes.bindings.iter().take(4096).find(|binding| {
            binding.kind == BindingKind::Assignment
                && binding
                    .value
                    .and_then(|range| {
                        node_with_range(&file.parse.syntax_node(), range, |node| {
                            ast::Expr::can_cast(node.kind())
                        })
                    })
                    .and_then(ast::Expr::cast)
                    .is_some_and(|value| value.unparen().text_range() == call.text_range())
        }) else {
            return Some(Type::Unknown);
        };
        if name.as_str() != binding.name.as_str() {
            return Some(Type::Unknown);
        }
        Some(Type::ParamSpec(Arc::new(ParameterSpecDefinition {
            key: format!(
                "\0paramspec:{}:{}",
                file.id.0,
                usize::from(binding.range.start())
            )
            .into(),
            name: binding.name.clone(),
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
    pub(super) fn declared_paramspec_parameter(
        &self,
        file: &Arc<FileInfo>,
        binding: &crate::scope::Binding,
        node: &SyntaxNode,
    ) -> Option<Type> {
        if !node
            .children_with_tokens()
            .filter_map(|item| item.into_token())
            .any(|token| token.kind() == SyntaxKind::STAR_STAR)
        {
            return None;
        }
        // ParamSpec defaults are a separate structured-default family.
        if node.children().any(|child| {
            matches!(
                child.kind(),
                SyntaxKind::DEFAULT_VALUE | SyntaxKind::ANNOTATION
            )
        }) {
            return Some(Type::Unknown);
        }
        Some(Type::ParamSpec(Arc::new(ParameterSpecDefinition {
            key: format!(
                "\0paramspec:{}:{}",
                file.id.0,
                usize::from(binding.range.start())
            )
            .into(),
            name: binding.name.clone(),
            variance: Variance::Inferred,
        })))
    }
    pub(super) fn parameter_specification_annotation(
        &self,
        file: &Arc<FileInfo>,
        expression: &ast::Expr,
    ) -> Option<Type> {
        let expression = expression.clone().unparen();
        if let ast::Expr::Subscript(concatenate) = &expression {
            if !self.canonical_parameter_symbol(
                file,
                &concatenate.object()?,
                "Concatenate",
                &mut FxHashSet::default(),
            ) {
                return None;
            }
            let index = concatenate.index()?;
            let args: Vec<_> = if index.kind() == SyntaxKind::SLICE_LIST {
                index
                    .children()
                    .filter_map(ast::Expr::cast)
                    .take(18)
                    .collect()
            } else {
                ast::Expr::cast(index).into_iter().collect()
            };
            if args.len() < 2 || args.len() > 17 {
                return None;
            }
            let Type::ParamSpec(tail) = self.type_of_expr(file, args.last()?) else {
                return None;
            };
            let prefix: Vec<_> = args[..args.len() - 1]
                .iter()
                .map(|arg| self.annotation_type(file, arg))
                .collect();
            if prefix.iter().any(Type::is_unknown) {
                return None;
            }
            return Some(Type::Concatenate(prefix, tail));
        }
        match self.type_of_expr(file, &expression) {
            Type::ParamSpec(definition) => Some(Type::ParamSpec(definition)),
            _ => None,
        }
    }
    pub(super) fn canonical_callable_annotation(
        &self,
        file: &Arc<FileInfo>,
        expression: &ast::Expr,
    ) -> bool {
        self.canonical_parameter_symbol(file, expression, "Callable", &mut FxHashSet::default())
    }
    pub(super) fn unify_parameter_specification(
        &self,
        template: &Signature,
        actual: &Signature,
        substitutions: &mut FxHashMap<SmolStr, Type>,
    ) -> bool {
        if template.params.len() != 1 {
            return false;
        }
        let (prefix, definition) = match &template.params[0].ty {
            Type::ParamSpec(definition) => (&[][..], definition),
            Type::Concatenate(prefix, definition) => (prefix.as_slice(), definition),
            _ => return false,
        };
        if actual.params.len() > 64 || actual.params.len() < prefix.len() {
            substitutions.insert(definition.key.clone(), Type::Unknown);
            return true;
        }
        for (expected, parameter) in prefix.iter().zip(&actual.params) {
            if !matches!(
                parameter.kind,
                ParamKind::PositionalOnly | ParamKind::Normal
            ) || !self.is_compatible(expected, &parameter.ty)
            {
                substitutions.insert(definition.key.clone(), Type::Unknown);
                return true;
            }
        }
        // Capture complete parameter metadata; P is not a union of argument types.
        let captured = Type::ParameterList(actual.params[prefix.len()..].to_vec());
        match substitutions.get(&definition.key) {
            Some(previous) if previous != &captured => {
                substitutions.insert(definition.key.clone(), Type::Unknown);
            }
            Some(_) => {}
            None => {
                substitutions.insert(definition.key.clone(), captured);
            }
        }
        true
    }
    pub(super) fn unresolved_parameter_specifications(
        &self,
        signature: &Signature,
        substitutions: &FxHashMap<SmolStr, Type>,
    ) -> bool {
        fn unresolved(ty: &Type, substitutions: &FxHashMap<SmolStr, Type>, depth: u8) -> bool {
            match ty {
                Type::ParamSpec(definition) | Type::Concatenate(_, definition) => !matches!(
                    substitutions.get(&definition.key),
                    Some(Type::ParameterList(_))
                ),
                // This check must not narrow old ordinary-type admission. New
                // directly encountered markers above still require a real list.
                _ if depth >= 16 => false,
                Type::Callable(signature) | Type::Function(signature) => {
                    signature
                        .params
                        .iter()
                        .any(|param| unresolved(&param.ty, substitutions, depth + 1))
                        || unresolved(&signature.ret, substitutions, depth + 1)
                }
                Type::Tuple(types)
                | Type::Union(types)
                | Type::Class(_, types)
                | Type::Instance(_, types) => types
                    .iter()
                    .any(|ty| unresolved(ty, substitutions, depth + 1)),
                _ => false,
            }
        }
        // Input-only opaque P does not determine an already concrete return.
        // Returned Callable/class/tuple structure still needs a real capture.
        unresolved(&signature.ret, substitutions, 0)
    }
}
