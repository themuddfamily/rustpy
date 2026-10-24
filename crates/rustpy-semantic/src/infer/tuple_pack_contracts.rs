//! Ordered variadic packs; scalar unions never represent pack arity.
use super::*;
use crate::types::TuplePackDefinition;
use crate::types::Variance;
use rustc_hash::FxHashSet;

impl Inferer<'_> {
    pub(super) fn declared_tuple_pack_call(
        &self,
        file: &Arc<FileInfo>,
        call: &ast::CallExpr,
    ) -> Option<Type> {
        let callee = call.callee()?;
        if !self.canonical_parameter_symbol(
            file,
            &callee,
            "TypeVarTuple",
            &mut FxHashSet::default(),
        ) {
            return None;
        }
        let Some(arguments) = call.arg_list() else {
            return Some(Type::Unknown);
        };
        let actual: Vec<_> = arguments.args().take(5).collect();
        if actual.is_empty() || actual.len() > 4 || !matches!(actual[0], ast::AnyArg::Positional(_))
        {
            return Some(Type::Unknown);
        }
        let mut flags = [false; 3];
        let mut seen = FxHashSet::default();
        for argument in &actual[1..] {
            let ast::AnyArg::Keyword(keyword) = argument else {
                return Some(Type::Unknown);
            };
            let name = keyword.name_text();
            let index = match name.as_str() {
                "covariant" => 0,
                "contravariant" => 1,
                "infer_variance" => 2,
                _ => return Some(Type::Unknown),
            };
            if !seen.insert(index) {
                return Some(Type::Unknown);
            }
            let Some(ast::Expr::Literal(value)) = keyword.value().map(ast::Expr::unparen) else {
                return Some(Type::Unknown);
            };
            flags[index] = match value.kind() {
                Some(SyntaxKind::TRUE_KW) => true,
                Some(SyntaxKind::FALSE_KW) => false,
                _ => return Some(Type::Unknown),
            };
        }
        if flags.iter().filter(|flag| **flag).count() > 1 {
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
        Some(Type::TypeVarTuple(Arc::new(TuplePackDefinition {
            key: format!(
                "\0tuplepack:{}:{}",
                file.id.0,
                usize::from(binding.range.start())
            )
            .into(),
            name: binding.name.clone(),
            variance: if flags[2] {
                Variance::Inferred
            } else if flags[0] {
                Variance::Covariant
            } else if flags[1] {
                Variance::Contravariant
            } else {
                Variance::Invariant
            },
        })))
    }
    pub(super) fn declared_tuple_pack_parameter(
        &self,
        file: &Arc<FileInfo>,
        binding: &crate::scope::Binding,
        node: &SyntaxNode,
    ) -> Option<Type> {
        if !node
            .children_with_tokens()
            .filter_map(|item| item.into_token())
            .any(|token| token.kind() == SyntaxKind::STAR)
        {
            return None;
        }
        if node.children().any(|child| {
            matches!(
                child.kind(),
                SyntaxKind::DEFAULT_VALUE | SyntaxKind::ANNOTATION
            )
        }) {
            return Some(Type::Unknown);
        }
        Some(Type::TypeVarTuple(Arc::new(TuplePackDefinition {
            key: format!(
                "\0tuplepack:{}:{}",
                file.id.0,
                usize::from(binding.range.start())
            )
            .into(),
            name: binding.name.clone(),
            variance: Variance::Inferred,
        })))
    }
    pub(super) fn unpacked_annotation(&self, file: &Arc<FileInfo>, expression: &ast::Expr) -> Type {
        match self.type_of_expr(file, expression) {
            Type::TypeVarTuple(definition) => {
                Type::Unpack(Box::new(Type::TypeVarTuple(definition)))
            }
            _ => Type::Unpack(Box::new(self.annotation_type(file, expression))),
        }
    }
    pub(super) fn unpack_symbol_is_canonical(
        &self,
        file: &Arc<FileInfo>,
        expression: &ast::Expr,
    ) -> bool {
        self.canonical_parameter_symbol(file, expression, "Unpack", &mut FxHashSet::default())
    }
    pub(super) fn bind_tuple_pack(
        &self,
        definition: &Arc<TuplePackDefinition>,
        values: Vec<Type>,
        substitutions: &mut FxHashMap<SmolStr, Type>,
    ) {
        if values.len() > 256 {
            substitutions.insert(definition.key.clone(), Type::Unknown);
            return;
        }
        let captured = Type::TypePack(values.into_iter().map(|ty| self.widen(ty)).collect());
        match substitutions.get(&definition.key) {
            Some(previous) if previous != &captured => {
                substitutions.insert(definition.key.clone(), Type::Unknown);
            }
            Some(_) => {}
            None => {
                substitutions.insert(definition.key.clone(), captured);
            }
        }
    }
    pub(super) fn unify_tuple_pack(
        &self,
        template: &[Type],
        actual: &[Type],
        substitutions: &mut FxHashMap<SmolStr, Type>,
    ) -> bool {
        let packs: Vec<_> = template
            .iter()
            .enumerate()
            .filter_map(|(index, ty)| match ty {
                Type::Unpack(inner) => match inner.as_ref() {
                    Type::TypeVarTuple(definition) => Some((index, definition)),
                    _ => None,
                },
                _ => None,
            })
            .take(2)
            .collect();
        if packs.is_empty() {
            return false;
        }
        if packs.len() != 1 {
            for (_, definition) in packs {
                substitutions.insert(definition.key.clone(), Type::Unknown);
            }
            return true;
        }
        let (index, definition) = packs[0];
        let fixed = template.len() - 1;
        if actual.len() < fixed
            || actual.len() > 256
            || actual
                .iter()
                .any(|ty| matches!(ty, Type::Unpack(_) | Type::TypePack(_)))
        {
            substitutions.insert(definition.key.clone(), Type::Unknown);
            return true;
        }
        let suffix = template.len() - index - 1;
        for (expected, value) in template[..index].iter().zip(actual) {
            self.unify(expected, value, substitutions);
            let expected = expected.substitute(&|key| substitutions.get(key).cloned());
            if !self.is_compatible(value, &expected) {
                substitutions.insert(definition.key.clone(), Type::Unknown);
                return true;
            }
        }
        for (expected, value) in template[index + 1..]
            .iter()
            .zip(&actual[actual.len() - suffix..])
        {
            self.unify(expected, value, substitutions);
            let expected = expected.substitute(&|key| substitutions.get(key).cloned());
            if !self.is_compatible(value, &expected) {
                substitutions.insert(definition.key.clone(), Type::Unknown);
                return true;
            }
        }
        self.bind_tuple_pack(
            definition,
            actual[index..actual.len() - suffix].to_vec(),
            substitutions,
        );
        true
    }
    pub(super) fn unresolved_tuple_packs(
        &self,
        signature: &Signature,
        substitutions: &FxHashMap<SmolStr, Type>,
    ) -> bool {
        fn unresolved(ty: &Type, substitutions: &FxHashMap<SmolStr, Type>, depth: u8) -> bool {
            match ty {
                Type::TypeVarTuple(definition) => {
                    !matches!(substitutions.get(&definition.key), Some(Type::TypePack(_)))
                }
                _ if depth >= 16 => false,
                Type::Unpack(inner) => unresolved(inner, substitutions, depth + 1),
                Type::Tuple(types)
                | Type::Union(types)
                | Type::Class(_, types)
                | Type::Instance(_, types) => types
                    .iter()
                    .any(|ty| unresolved(ty, substitutions, depth + 1)),
                Type::Callable(signature) | Type::Function(signature) => {
                    signature
                        .params
                        .iter()
                        .any(|param| unresolved(&param.ty, substitutions, depth + 1))
                        || unresolved(&signature.ret, substitutions, depth + 1)
                }
                _ => false,
            }
        }
        // An opaque input pack cannot erase a concrete declared return.
        unresolved(&signature.ret, substitutions, 0)
    }
    /// A pack call with starred input must capture an actual fixed sequence,
    /// not pretend that omitted unknown-length input was an empty pack.
    pub(super) fn tuple_pack_star_call(
        &self,
        file: &Arc<FileInfo>,
        call: &ast::CallExpr,
        callee: &Type,
    ) -> Option<Type> {
        let signature = callee.signature()?;
        let arguments = call.arg_list()?;
        if !arguments
            .args()
            .any(|arg| matches!(arg, ast::AnyArg::Star(_) | ast::AnyArg::StarStar(_)))
        {
            return None;
        }
        fn contains_pack(ty: &Type, remaining: &mut usize, depth: u8) -> Option<bool> {
            if *remaining == 0 || depth > 16 {
                return None;
            }
            *remaining -= 1;
            match ty {
                Type::TypeVarTuple(_) => Some(true),
                Type::Unpack(inner) => contains_pack(inner, remaining, depth + 1),
                Type::Tuple(types) => {
                    for ty in types {
                        if contains_pack(ty, remaining, depth + 1)? {
                            return Some(true);
                        }
                    }
                    Some(false)
                }
                _ => Some(false),
            }
        }
        let mut remaining = 4096;
        let mut has_pack = false;
        for parameter in &signature.params {
            match contains_pack(&parameter.ty, &mut remaining, 0) {
                Some(true) => {
                    has_pack = true;
                    break;
                }
                Some(false) => {}
                None => return Some(Type::Unknown),
            }
        }
        if !has_pack {
            return None;
        }
        let mut positional = Vec::new();
        let mut keywords = Vec::new();
        for (argument_index, argument) in arguments.args().take(257).enumerate() {
            if argument_index >= 256 {
                return Some(Type::Unknown);
            }
            if positional.len() + keywords.len() > 256 {
                return Some(Type::Unknown);
            }
            match argument {
                ast::AnyArg::Positional(argument) => {
                    let Some(value) = argument.expr() else {
                        return Some(Type::Unknown);
                    };
                    positional.push(self.type_of_expr(file, &value));
                }
                ast::AnyArg::Keyword(argument) => {
                    let Some(value) = argument.value() else {
                        return Some(Type::Unknown);
                    };
                    keywords.push((
                        SmolStr::new(argument.name_text()),
                        self.type_of_expr(file, &value),
                    ));
                }
                ast::AnyArg::Star(argument) => {
                    let Some(value) = ast::AnyArg::Star(argument).value() else {
                        return Some(Type::Unknown);
                    };
                    let Type::Tuple(values) = self.type_of_expr(file, &value) else {
                        return Some(Type::Unknown);
                    };
                    if positional.len() + values.len() > 256 {
                        return Some(Type::Unknown);
                    }
                    positional.extend(values);
                }
                _ => return Some(Type::Unknown),
            }
        }
        Some(self.call_result(callee, &positional, &keywords))
    }
}
