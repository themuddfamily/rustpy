//! Defaults for unsolved declared type variables only; no imported global cache.
use super::*;
use crate::types::TypeVarDefinition;
use rustc_hash::FxHashSet;
impl Inferer<'_> {
    pub(super) fn solve_signature_defaults(
        &self,
        signature: &Signature,
        substitutions: &mut FxHashMap<SmolStr, Type>,
    ) {
        let mut types: Vec<Type> = signature
            .params
            .iter()
            .map(|parameter| parameter.ty.clone())
            .collect();
        types.push(signature.ret.clone());
        if let Some(definition) = &signature.def {
            if let Some(file) = self.db.file(definition.file) {
                if let Some(function) =
                    node_with_range(&file.parse.syntax_node(), definition.range, |node| {
                        node.kind() == SyntaxKind::FUNC_DEF
                    })
                    .and_then(ast::FuncDef::cast)
                {
                    if let Some(parameters) = function
                        .syntax()
                        .children()
                        .find(|node| node.kind() == SyntaxKind::TYPE_PARAM_LIST)
                    {
                        let Some(scope) = file.scopes.scope_for_node(parameters.text_range())
                        else {
                            return;
                        };
                        let mut declared = Vec::new();
                        for parameter in parameters.children().take(33) {
                            if declared.len() >= 32 {
                                return;
                            }
                            let Some(name) = ast::token(&parameter, SyntaxKind::IDENT) else {
                                return;
                            };
                            let ids = file.scopes.bindings_in(scope, name.text());
                            let Some(id) = ids.iter().find(|id| {
                                file.scopes.binding(**id).node_range == parameter.text_range()
                            }) else {
                                return;
                            };
                            declared.push(self.type_of_binding(&file, *id));
                        }
                        // Reject a stale signature whose captured declaration metadata
                        // disagrees with the source used to recover declaration order.
                        let mut visible = Vec::new();
                        let mut budget = 512;
                        for ty in &types {
                            collect_defaults(ty, &mut visible, 0, &mut budget);
                        }
                        // Own defaults are solved only for this function's
                        // explicit parameter list. Projected class variables
                        // remain supplied by receiver/argument inference.
                        if visible.iter().any(|held| {
                            declared.iter().any(|ty| matches!(ty, Type::DeclaredTypeVar(current) if current.key == held.key && current.as_ref() != held.as_ref()))
                        }) { return; }
                        self.solve_generic_defaults(&declared, substitutions);
                        return;
                    }
                }
            }
        }
        self.solve_generic_defaults(&types, substitutions);
    }

    pub(super) fn solve_generic_defaults(
        &self,
        types: &[Type],
        substitutions: &mut FxHashMap<SmolStr, Type>,
    ) {
        let mut definitions = Vec::new();
        let mut budget = 512;
        for ty in types {
            collect_defaults(ty, &mut definitions, 0, &mut budget);
        }
        let mut earlier = FxHashSet::default();
        for definition in definitions {
            if !substitutions.contains_key(&definition.key) {
                if let Some(default) = &definition.default {
                    if !default_shape_admitted(default, 0, &mut 512) {
                        substitutions.insert(definition.key.clone(), Type::Unknown);
                        earlier.insert(definition.key.clone());
                        continue;
                    }
                    let invalid = std::cell::Cell::new(false);
                    let candidate = default.substitute(&|key| {
                        if !earlier.contains(key) {
                            invalid.set(true);
                            return Some(Type::Unknown);
                        }
                        Some(substitutions.get(key).cloned().unwrap_or(Type::Unknown))
                    });
                    let dependent_constraints_valid = match default.as_ref() {
                        Type::DeclaredTypeVar(dependency) if !definition.constraints.is_empty() => {
                            !dependency.constraints.is_empty()
                                && dependency
                                    .constraints
                                    .iter()
                                    .all(|constraint| definition.constraints.contains(constraint))
                        }
                        _ => true,
                    };
                    let declared_bound_valid = definition.bound.as_ref().is_none_or(|bound| {
                        !matches!(default.as_ref(), Type::DeclaredTypeVar(_))
                            || self.is_compatible(default, bound)
                    });
                    let valid = !invalid.get()
                        && !candidate.is_unknown()
                        && dependent_constraints_valid
                        && declared_bound_valid
                        && definition
                            .bound
                            .as_ref()
                            .is_none_or(|bound| self.is_compatible(&candidate, bound))
                        && (definition.constraints.is_empty()
                            || definition.constraints.contains(&candidate));
                    substitutions.insert(
                        definition.key.clone(),
                        if valid { candidate } else { Type::Unknown },
                    );
                }
            }
            earlier.insert(definition.key.clone());
        }
    }
    pub(super) fn complete_class_defaults(
        &self,
        class: &ClassRef,
        arguments: &[Type],
    ) -> Vec<Type> {
        let parameters = self.class_parameter_types(class);
        if parameters.is_empty() || arguments.len() >= parameters.len() {
            return self.normalize_parameter_arguments(&parameters, arguments);
        }
        let mut substitutions = FxHashMap::default();
        for (parameter, argument) in parameters.iter().zip(arguments) {
            if let Some(key) = Self::typevar_substitution_key(parameter) {
                substitutions.insert(SmolStr::new(key), argument.clone());
            }
        }
        self.solve_generic_defaults(&parameters, &mut substitutions);
        let completed: Vec<_> = parameters
            .iter()
            .enumerate()
            .map(|(index, parameter)| {
                arguments
                    .get(index)
                    .cloned()
                    .or_else(|| {
                        Self::typevar_substitution_key(parameter)
                            .and_then(|key| substitutions.get(key).cloned())
                    })
                    .unwrap_or(Type::Unknown)
            })
            .collect();
        self.normalize_parameter_arguments(&parameters, &completed)
    }
}
fn collect_defaults(
    ty: &Type,
    definitions: &mut Vec<Arc<TypeVarDefinition>>,
    depth: u8,
    budget: &mut usize,
) {
    if depth >= 16 || *budget == 0 || definitions.len() >= 32 {
        return;
    }
    *budget -= 1;
    match ty {
        Type::DeclaredTypeVar(definition) => {
            if !definitions.iter().any(|old| old.key == definition.key) {
                definitions.push(definition.clone());
            }
        }
        Type::Class(_, types)
        | Type::Instance(_, types)
        | Type::Tuple(types)
        | Type::Union(types) => {
            for ty in types {
                collect_defaults(ty, definitions, depth + 1, budget);
            }
        }
        Type::Function(signature) | Type::Callable(signature) | Type::BoundMethod(signature, _) => {
            for parameter in &signature.params {
                collect_defaults(&parameter.ty, definitions, depth + 1, budget);
            }
            collect_defaults(&signature.ret, definitions, depth + 1, budget);
        }
        Type::Overloaded(signatures) => {
            for signature in signatures {
                for parameter in &signature.params {
                    collect_defaults(&parameter.ty, definitions, depth + 1, budget);
                }
                collect_defaults(&signature.ret, definitions, depth + 1, budget);
            }
        }
        Type::Super { receiver, .. } => collect_defaults(receiver, definitions, depth + 1, budget),
        _ => {}
    }
}

fn default_shape_admitted(ty: &Type, depth: u8, budget: &mut usize) -> bool {
    if depth >= 16 || *budget == 0 {
        return false;
    }
    *budget -= 1;
    match ty {
        Type::Class(_, types)
        | Type::Instance(_, types)
        | Type::Tuple(types)
        | Type::Union(types) => types
            .iter()
            .all(|ty| default_shape_admitted(ty, depth + 1, budget)),
        Type::Function(signature) | Type::Callable(signature) | Type::BoundMethod(signature, _) => {
            signature
                .params
                .iter()
                .all(|parameter| default_shape_admitted(&parameter.ty, depth + 1, budget))
                && default_shape_admitted(&signature.ret, depth + 1, budget)
        }
        Type::Overloaded(signatures) => signatures.iter().all(|signature| {
            signature
                .params
                .iter()
                .all(|parameter| default_shape_admitted(&parameter.ty, depth + 1, budget))
                && default_shape_admitted(&signature.ret, depth + 1, budget)
        }),
        Type::Super { receiver, .. } => default_shape_admitted(receiver, depth + 1, budget),
        _ => true,
    }
}
