//! Protocol member assignability, isolated from loose overload compatibility.
use super::*;

// CPython 3.12 typing._get_protocol_attrs excludes implementation scaffolding,
// not comparison/iteration/operator methods that are actual protocol contracts.
fn protocol_interface_name(name: &str) -> bool {
    !name.starts_with("_abc_")
        && !matches!(
            name,
            "__parameters__"
                | "__orig_bases__"
                | "__orig_class__"
                | "_is_protocol"
                | "_is_runtime_protocol"
                | "__protocol_attrs__"
                | "__non_callable_proto_members__"
                | "__type_params__"
                | "__abstractmethods__"
                | "__annotations__"
                | "__dict__"
                | "__doc__"
                | "__init__"
                | "__module__"
                | "__new__"
                | "__slots__"
                | "__subclasshook__"
                | "__weakref__"
                | "__class_getitem__"
                | "_MutableMapping__marker"
        )
}

impl Inferer<'_> {
    /// Whether a class explicitly declares the canonical typing Protocol base,
    /// resolving aliases and checking its real bundled provider authority.
    pub fn canonical_protocol(&self, class: &ClassRef) -> bool {
        let Some(info) = self.class_info(class) else {
            return false;
        };
        let Some(file) = self.db.file(class.file) else {
            return false;
        };
        info.base_exprs.iter().any(|range| {
            let expression = node_with_range(&file.parse.syntax_node(), *range, |node| {
                ast::Expr::can_cast(node.kind())
            })
            .and_then(ast::Expr::cast);
            expression
                .and_then(|expression| self.symbol_origin(&file, &expression))
                .is_some_and(|(module, name)| {
                    name == "Protocol"
                        && matches!(module.as_str(), "typing" | "typing_extensions")
                        && self.db.module(&module).is_some_and(|file| {
                            file.is_stub
                                && file.path
                                    == std::path::Path::new(&format!("<typeshed>/{module}.pyi"))
                        })
                })
        })
    }

    /// Interface member names a canonical protocol declares across its
    /// protocol bases (`None` when one of them has no class information).
    pub(super) fn protocol_member_names(&self, protocol: &ClassRef) -> Option<Vec<SmolStr>> {
        let mut names: Vec<SmolStr> = Vec::new();
        for base in self.mro(protocol).iter() {
            if !self.canonical_protocol(base) {
                continue;
            }
            for (name, member) in &self.class_info(base)?.members {
                if protocol_interface_name(name)
                    && member.binding.is_some()
                    && !names.contains(name)
                {
                    names.push(name.clone());
                }
            }
        }
        Some(names)
    }

    pub(super) fn protocol_compatible(&self, actual: &Type, expected: &Type) -> Option<bool> {
        let Type::Instance(protocol, _) = expected else {
            return None;
        };
        if !self.canonical_protocol(protocol) {
            return None;
        }
        Some(self.protocol_members_compatible(actual, expected, &mut Vec::new()))
    }

    fn protocol_value_compatible(
        &self,
        actual: &Type,
        expected: &Type,
        active: &mut Vec<(Type, Type)>,
    ) -> bool {
        if matches!(actual, Type::Any | Type::Unknown)
            || matches!(expected, Type::Any | Type::Unknown | Type::TypeVar(_))
        {
            return true;
        }
        match (actual, expected) {
            (Type::Never, _) => true,
            (Type::Tuple(actual), Type::Tuple(expected)) => {
                actual.len() == expected.len()
                    && actual.iter().zip(expected).all(|(actual, expected)| {
                        self.protocol_value_compatible(actual, expected, active)
                    })
            }
            (
                Type::Function(_)
                | Type::BoundMethod(_, _)
                | Type::Callable(_)
                | Type::Overloaded(_),
                Type::Function(_)
                | Type::BoundMethod(_, _)
                | Type::Callable(_)
                | Type::Overloaded(_),
            ) => self.protocol_callable_compatible(actual, expected, active),
            (
                Type::Instance(actual_class, actual_args),
                Type::Instance(expected_class, expected_args),
            ) if actual_class == expected_class
                && !actual_args.is_empty()
                && !expected_args.is_empty() =>
            {
                actual_args.len() == expected_args.len()
                    && actual_args
                        .iter()
                        .zip(expected_args)
                        .all(|(actual, expected)| {
                            self.protocol_value_compatible(actual, expected, active)
                                && (matches!(expected, Type::TypeVar(_) | Type::DeclaredTypeVar(_))
                                    || self.protocol_value_compatible(expected, actual, active))
                        })
            }
            (Type::Union(types), _) => types
                .iter()
                .all(|actual| self.protocol_value_compatible(actual, expected, active)),
            (_, Type::Union(types)) => types
                .iter()
                .any(|expected| self.protocol_value_compatible(actual, expected, active)),
            (_, Type::Instance(protocol, _)) if self.canonical_protocol(protocol) => {
                self.protocol_members_compatible(actual, expected, active)
            }
            _ => self.is_compatible(actual, expected),
        }
    }

    fn protocol_members_compatible(
        &self,
        actual: &Type,
        expected: &Type,
        active: &mut Vec<(Type, Type)>,
    ) -> bool {
        if matches!(actual, Type::Any | Type::Unknown | Type::Never) {
            return true;
        }
        if let Type::Union(arms) = actual {
            return arms
                .iter()
                .all(|actual| self.protocol_members_compatible(actual, expected, active));
        }
        let pair = (actual.clone(), expected.clone());
        if active.contains(&pair) {
            return true;
        } // recursive structural obligation
        if active.len() >= 32 {
            return false;
        }
        let Type::Instance(protocol, _) = expected else {
            return false;
        };
        active.push(pair);
        let mut members = IndexMap::new();
        for base in self.mro(protocol).iter() {
            if !self.canonical_protocol(base) {
                continue;
            }
            let Some(info) = self.class_info(base) else {
                active.pop();
                return false;
            };
            for (name, member) in &info.members {
                if protocol_interface_name(name) && member.binding.is_some() {
                    members
                        .entry(name.clone())
                        .or_insert_with(|| member.clone());
                }
            }
        }
        let compatible = members.values().all(|member| {
            let (Some(actual_type), Some(expected_type)) = (
                self.lookup_member(actual, &member.name),
                self.lookup_member(expected, &member.name),
            ) else {
                return false;
            };
            match member.kind {
                MemberKind::Method | MemberKind::StaticMethod | MemberKind::ClassMethod => {
                    self.protocol_callable_compatible(&actual_type, &expected_type, active)
                }
                MemberKind::Property
                    if self
                        .protocol_member_write_type(expected, &member.name)
                        .is_none() =>
                {
                    self.protocol_value_compatible(&actual_type, &expected_type, active)
                }
                MemberKind::Property | MemberKind::InstanceVar | MemberKind::ClassVar => {
                    let (Some(actual_write), Some(expected_write)) = (
                        self.protocol_member_write_type(actual, &member.name),
                        self.protocol_member_write_type(expected, &member.name),
                    ) else {
                        return false;
                    };
                    self.protocol_value_compatible(&actual_type, &expected_type, active)
                        && self.protocol_value_compatible(&expected_write, &actual_write, active)
                }
                MemberKind::NestedClass => {
                    self.protocol_value_compatible(&actual_type, &expected_type, active)
                }
            }
        });
        active.pop();
        compatible
    }

    fn protocol_member_write_type(&self, receiver: &Type, name: &str) -> Option<Type> {
        let class = receiver.class_ref()?;
        for base in self.mro(class).iter() {
            let info = self.class_info(base)?;
            let Some(member) = info.members.get(name) else {
                continue;
            };
            return match member.kind {
                MemberKind::InstanceVar | MemberKind::ClassVar => {
                    self.lookup_member(receiver, name)
                }
                MemberKind::Property => {
                    let file = self.db.file(member.file)?;
                    let class_node =
                        node_with_range(&file.parse.syntax_node(), base.range, |node| {
                            node.kind() == SyntaxKind::CLASS_DEF
                        })
                        .and_then(ast::ClassDef::cast)?;
                    let body = class_node.body()?;
                    body.statements().find_map(|statement| {
                        let ast::Stmt::FuncDef(function) = statement else {
                            return None;
                        };
                        if function.name_text() != name
                            || !function.decorators().any(|decorator| {
                                decorator
                                    .name_text()
                                    .is_some_and(|name| name.ends_with(".setter"))
                            })
                        {
                            return None;
                        }
                        let signature = self.signature(&file, &function, Some(base));
                        (!signature.is_async && signature.params.len() == 2)
                            .then(|| signature.params[1].ty.clone())
                    })
                }
                _ => None,
            };
        }
        None
    }

    fn protocol_callable_compatible(
        &self,
        actual: &Type,
        expected: &Type,
        active: &mut Vec<(Type, Type)>,
    ) -> bool {
        let signatures = |ty: &Type| -> Vec<Arc<Signature>> {
            match ty {
                Type::Function(signature)
                | Type::Callable(signature)
                | Type::BoundMethod(signature, _) => vec![signature.clone()],
                Type::Overloaded(signatures) => signatures.clone(),
                _ => Vec::new(),
            }
        };
        if matches!(actual, Type::Any | Type::Unknown) {
            return true;
        }
        let actual = signatures(actual);
        let expected = signatures(expected);
        !expected.is_empty()
            && expected.iter().all(|expected| {
                actual
                    .iter()
                    .any(|actual| self.protocol_signature_compatible(actual, expected, active))
            })
    }

    pub(super) fn protocol_signature_compatible(
        &self,
        actual: &Signature,
        expected: &Signature,
        active: &mut Vec<(Type, Type)>,
    ) -> bool {
        if actual.is_async != expected.is_async
            || !self.protocol_value_compatible(&actual.ret, &expected.ret, active)
        {
            return false;
        }
        let positional = |signature: &Signature| {
            signature
                .params
                .iter()
                .filter(|parameter| {
                    matches!(
                        parameter.kind,
                        ParamKind::PositionalOnly | ParamKind::Normal
                    )
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        let actual_positional = positional(actual);
        let expected_positional = positional(expected);
        let varargs = actual
            .params
            .iter()
            .find(|parameter| parameter.kind == ParamKind::VarArgs);
        let kwargs = actual
            .params
            .iter()
            .find(|parameter| parameter.kind == ParamKind::KwArgs);
        for (index, parameter) in expected_positional.iter().enumerate() {
            let Some(accepted) = actual_positional.get(index).or(varargs) else {
                return false;
            };
            if parameter.has_default && !accepted.has_default && accepted.kind != ParamKind::VarArgs
            {
                return false;
            }
            if !self.protocol_value_compatible(&parameter.ty, &accepted.ty, active) {
                return false;
            }
            if parameter.kind == ParamKind::Normal {
                let keyword = actual
                    .params
                    .iter()
                    .find(|accepted| {
                        matches!(accepted.kind, ParamKind::Normal | ParamKind::KeywordOnly)
                            && accepted.name == parameter.name
                    })
                    .or(kwargs);
                let Some(keyword) = keyword else { return false };
                if !self.protocol_value_compatible(&parameter.ty, &keyword.ty, active) {
                    return false;
                }
                // A differently named required positional parameter cannot be
                // satisfied by an expected keyword-only invocation.
                if accepted.kind != ParamKind::VarArgs
                    && !accepted.has_default
                    && (accepted.kind == ParamKind::PositionalOnly
                        || accepted.name != parameter.name)
                {
                    return false;
                }
            }
        }
        for parameter in &expected.params {
            let accepted = match parameter.kind {
                ParamKind::KeywordOnly => actual
                    .params
                    .iter()
                    .find(|accepted| {
                        matches!(accepted.kind, ParamKind::Normal | ParamKind::KeywordOnly)
                            && accepted.name == parameter.name
                    })
                    .or(kwargs),
                ParamKind::VarArgs => varargs,
                ParamKind::KwArgs => kwargs,
                _ => continue,
            };
            let Some(accepted) = accepted else {
                return false;
            };
            if parameter.has_default
                && !accepted.has_default
                && !matches!(accepted.kind, ParamKind::VarArgs | ParamKind::KwArgs)
            {
                return false;
            }
            if !self.protocol_value_compatible(&parameter.ty, &accepted.ty, active) {
                return false;
            }
        }
        let expected_varargs = expected
            .params
            .iter()
            .find(|parameter| parameter.kind == ParamKind::VarArgs);
        let expected_kwargs = expected
            .params
            .iter()
            .find(|parameter| parameter.kind == ParamKind::KwArgs);
        for (index, parameter) in actual_positional.iter().enumerate() {
            let supplied_positionally = expected_positional
                .get(index)
                .is_some_and(|expected| !expected.has_default);
            let supplied_by_required_keyword = parameter.kind == ParamKind::Normal
                && expected.params.iter().any(|expected| {
                    expected.kind == ParamKind::KeywordOnly
                        && expected.name == parameter.name
                        && !expected.has_default
                });
            if !parameter.has_default && !supplied_positionally && !supplied_by_required_keyword {
                return false;
            }
            // Variadic arguments first fill any remaining fixed positional slots.
            if index >= expected_positional.len() {
                if expected_varargs.is_some()
                    && parameter.kind == ParamKind::Normal
                    && expected.params.iter().any(|expected| {
                        expected.kind == ParamKind::KeywordOnly && expected.name == parameter.name
                    })
                {
                    // Expected call may supply both a variadic positional and
                    // the named keyword; implementation would bind it twice.
                    return false;
                }
                if let Some(tail) = expected_varargs {
                    if !self.protocol_value_compatible(&tail.ty, &parameter.ty, active) {
                        return false;
                    }
                }
            }
            if expected_kwargs.is_some()
                && parameter.kind == ParamKind::Normal
                && expected_positional
                    .get(index)
                    .is_some_and(|expected| expected.kind == ParamKind::PositionalOnly)
            {
                // Protocol permits name in **kwargs even while this positional
                // argument is present; a normal implementation would duplicate it.
                return false;
            }
        }
        if let Some(tail) = expected_kwargs {
            for parameter in actual.params.iter().filter(|parameter| {
                matches!(parameter.kind, ParamKind::Normal | ParamKind::KeywordOnly)
            }) {
                let reserved = expected.params.iter().any(|expected| {
                    matches!(expected.kind, ParamKind::Normal | ParamKind::KeywordOnly)
                        && expected.name == parameter.name
                });
                if !reserved && !self.protocol_value_compatible(&tail.ty, &parameter.ty, active) {
                    return false;
                }
            }
        }
        actual
            .params
            .iter()
            .filter(|parameter| parameter.kind == ParamKind::KeywordOnly && !parameter.has_default)
            .all(|parameter| {
                expected.params.iter().any(|expected| {
                    expected.name == parameter.name
                        && expected.kind == ParamKind::KeywordOnly
                        && !expected.has_default
                })
            })
    }
}
