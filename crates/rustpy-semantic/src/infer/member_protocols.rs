//! Descriptor and dynamic attribute protocols use actual maintained callables.
use super::*;
impl Inferer<'_> {
    fn attribute_hook(
        &self,
        class: &ClassRef,
        args: &[Type],
        receiver: &Type,
        slot: &str,
        name: &str,
    ) -> Option<Type> {
        let callable = self.lookup_in_mro(class, args, slot, true, receiver)?;
        if matches!(slot, "__getattribute__" | "__setattr__" | "__delattr__")
            && callable
                .signature()
                .and_then(|signature| signature.def.as_ref())
                .is_some_and(|definition| {
                    self.db.file(definition.file).is_some_and(|file| {
                        file.path == std::path::Path::new("<typeshed>/builtins.pyi")
                            && definition.name == slot
                            && file
                                .parse
                                .syntax_node()
                                .token_at_offset(definition.range.start())
                                .right_biased()
                                .is_some_and(|token| {
                                    token
                                        .parent_ancestors()
                                        .take(64)
                                        .filter_map(ast::ClassDef::cast)
                                        .any(|class| {
                                            matches!(class.name_text().as_str(), "object" | "type")
                                        })
                                })
                    })
                })
        {
            return None;
        }
        Some(self.call_result(
            &callable,
            &[Type::Literal(LiteralValue::Str(name.into()))],
            &[],
        ))
    }
    fn descriptor_value(
        &self,
        value: Type,
        receiver: &Type,
        class: &ClassRef,
        args: &[Type],
    ) -> Type {
        let Type::Instance(descriptor, parameters) = &value else {
            return value;
        };
        let Some(get) = self.lookup_in_mro(descriptor, parameters, "__get__", true, &value) else {
            return value;
        };
        let owner = Type::Class(class.clone(), args.to_vec());
        self.call_result(&get, &[receiver.clone(), owner], &[])
    }
    pub(super) fn instance_attribute(
        &self,
        class: &ClassRef,
        args: &[Type],
        receiver: &Type,
        name: &str,
    ) -> Option<Type> {
        if !matches!(name, "__getattribute__" | "__getattr__") {
            if let Some(result) =
                self.attribute_hook(class, args, receiver, "__getattribute__", name)
            {
                return Some(result);
            }
        }
        if let Some(value) = self.lookup_in_mro(class, args, name, true, receiver) {
            let data = match &value {
                Type::Instance(descriptor, parameters) => {
                    self.lookup_in_mro(descriptor, parameters, "__set__", true, &value)
                        .is_some()
                        || self
                            .lookup_in_mro(descriptor, parameters, "__delete__", true, &value)
                            .is_some()
                }
                _ => false,
            };
            let declared = self
                .mro(class)
                .iter()
                .take(32)
                .filter_map(|owner| self.class_info(owner))
                .find_map(|info| info.members.get(name).cloned());
            let protected = declared.as_ref().is_some_and(|member| {
                member.annotation.is_some()
                    || matches!(member.kind, MemberKind::Property | MemberKind::InstanceVar)
            });
            if !data && !protected {
                if let Some(value) = self.direct_initialized_member(class, args, name) {
                    return Some(value);
                }
            }
            return Some(self.descriptor_value(value, receiver, class, args));
        }
        if !matches!(name, "__getattribute__" | "__getattr__") {
            self.attribute_hook(class, args, receiver, "__getattr__", name)
        } else {
            None
        }
    }
    fn direct_initialized_member(
        &self,
        class: &ClassRef,
        args: &[Type],
        name: &str,
    ) -> Option<Type> {
        for (owner, parameters) in self.mro_with_args(class, args).into_iter().take(32) {
            let file = self.db.file(owner.file)?;
            let Some(node) = node_with_range(&file.parse.syntax_node(), owner.range, |node| {
                node.kind() == SyntaxKind::CLASS_DEF
            })
            .and_then(ast::ClassDef::cast) else {
                continue;
            };
            let Some(function) = node
                .methods()
                .find(|function| function.name_text() == "__init__")
            else {
                continue;
            };
            if function.decorators().next().is_some() {
                return None;
            }
            let self_name = function
                .params()?
                .params()
                .next()?
                .name()?
                .text()
                .to_owned();
            let mut result = None;
            for statement in function.body()?.statements().take(256) {
                let (targets, value, annotation) = match statement {
                    ast::Stmt::Assign(statement) => (statement.targets(), statement.value(), None),
                    ast::Stmt::AnnAssign(statement) => (
                        statement.target().into_iter().collect(),
                        statement.value(),
                        statement.annotation(),
                    ),
                    ast::Stmt::Pass(_) => continue,
                    ast::Stmt::Expr(statement)
                        if matches!(statement.expr(), Some(ast::Expr::String(_))) =>
                    {
                        continue
                    }
                    _ => break,
                };
                if targets.iter().any(|target|matches!(target,ast::Expr::Attribute(attribute) if attribute.attr_text()==name
                    && matches!(attribute.object(),Some(ast::Expr::Name(object)) if object.name_text()==self_name))) {
                    let value=match annotation {Some(annotation)=>self.annotation_type(&file,&annotation),None=>value.map(|value|self.widen(self.type_of_expr(&file,&value))).unwrap_or(Type::Unknown)};
                    result=Some(value.substitute(&|name|self.class_substitution(&owner,&parameters,name)));
                }
            }
            return result;
        }
        None
    }
    pub(super) fn class_attribute(
        &self,
        class: &ClassRef,
        args: &[Type],
        receiver: &Type,
        name: &str,
    ) -> Option<Type> {
        let metaclass = match self.construction_metaclass(class, 0) {
            Ok(meta) => meta,
            Err(()) => return Some(Type::Unknown),
        };
        if let Some(meta) = &metaclass {
            if !matches!(name, "__getattribute__" | "__getattr__") {
                if let Some(result) =
                    self.attribute_hook(meta, &[], receiver, "__getattribute__", name)
                {
                    return Some(result);
                }
            }
            // Data descriptors on the metaclass precede the class namespace.
            if let Some(value) = self.lookup_in_mro(meta, &[], name, true, receiver) {
                if let Type::Instance(descriptor, parameters) = &value {
                    if self
                        .lookup_in_mro(descriptor, parameters, "__set__", true, &value)
                        .is_some()
                        || self
                            .lookup_in_mro(descriptor, parameters, "__delete__", true, &value)
                            .is_some()
                    {
                        return Some(self.descriptor_value(value, receiver, meta, &[]));
                    }
                }
            }
        }
        if let Some(value) = self
            .enum_attribute(class, name)
            .or_else(|| self.lookup_in_mro(class, args, name, false, receiver))
        {
            return Some(self.descriptor_value(value, &Type::None, class, args));
        }
        if let Some(meta) = metaclass {
            if let Some(value) = self.lookup_in_mro(&meta, &[], name, true, receiver) {
                return Some(self.descriptor_value(value, receiver, &meta, &[]));
            }
            if !matches!(name, "__getattribute__" | "__getattr__") {
                return self.attribute_hook(&meta, &[], receiver, "__getattr__", name);
            }
        }
        let meta = self.builtin_class("type")?;
        self.lookup_in_mro(
            &meta,
            &[],
            name,
            true,
            &Type::Instance(meta.clone(), Vec::new()),
        )
    }
}

impl Inferer<'_> {
    fn canonical_class_capabilities(&self, class: &ClassRef) -> bool {
        let Some(spec) = self.synth_spec(class) else {
            return false;
        };
        if spec.kind == SynthKind::NamedTuple {
            return true;
        }
        let Some(file) = self.db.file(class.file) else {
            return false;
        };
        let Some(node) = node_with_range(&file.parse.syntax_node(), class.range, |node| {
            node.kind() == SyntaxKind::CLASS_DEF
        })
        .and_then(ast::ClassDef::cast) else {
            return false;
        };
        node.decorators()
            .take(32)
            .filter_map(|decorator| decorator.expr())
            .any(|expression| {
                self.canonical_dataclass_decorator(&file, &expression)
                    || self.unresolved_attrs_synthesis_decorator(&file, class, &expression)
            })
            || (spec.kind == SynthKind::Transform && self.has_canonical_transform(class))
    }
    fn has_canonical_transform(&self, class: &ClassRef) -> bool {
        let mut owners = self.mro(class).as_ref().clone();
        if let Ok(Some(meta)) = self.construction_metaclass(class, 0) {
            owners.extend(self.mro(&meta).iter().take(32).cloned());
        }
        for owner in owners.iter().take(64) {
            let Some(file) = self.db.file(owner.file) else {
                continue;
            };
            let Some(node) = node_with_range(&file.parse.syntax_node(), owner.range, |node| {
                node.kind() == SyntaxKind::CLASS_DEF
            })
            .and_then(ast::ClassDef::cast) else {
                continue;
            };
            for decorator in node.decorators().take(32) {
                let Some(expression) = decorator.expr() else {
                    continue;
                };
                let target = match expression.clone().unparen() {
                    ast::Expr::Call(call) => call.callee(),
                    other => Some(other),
                };
                let Some(target) = target else {
                    continue;
                };
                if self
                    .symbol_origin(&file, &target)
                    .is_some_and(|(_, name)| name == "dataclass_transform")
                    && self.canonical_typing_class_metadata(&file, &expression) == Some(true)
                {
                    return true;
                }
                if let Some(definition) = self
                    .type_of_expr(&file, &target)
                    .signature()
                    .and_then(|signature| signature.def.as_ref())
                    .cloned()
                {
                    let Some(source) = self.db.file(definition.file) else {
                        continue;
                    };
                    let Some(function) =
                        node_with_range(&source.parse.syntax_node(), definition.range, |node| {
                            node.kind() == SyntaxKind::FUNC_DEF
                        })
                        .and_then(ast::FuncDef::cast)
                    else {
                        continue;
                    };
                    if function.decorators().take(32).filter_map(|decorator|decorator.expr()).any(|expression|self.canonical_typing_class_metadata(&source,&expression)==Some(true)
                        && matches!(expression.clone().unparen(),ast::Expr::Call(call) if call.callee().and_then(|target|self.symbol_origin(&source,&target)).is_some_and(|(_,name)|name=="dataclass_transform"))) {return true;}
                }
            }
        }
        false
    }
    /// Statically rejected writes; None retains ordinary/unknown dynamic policy.
    pub fn member_write_error(&self, receiver: &Type, name: &str, delete: bool) -> Option<String> {
        if let Type::Union(arms) = receiver {
            return arms
                .iter()
                .take(32)
                .find_map(|arm| self.member_write_error(arm, name, delete));
        }
        let class = match receiver {
            Type::Instance(class, _) | Type::Literal(LiteralValue::EnumMember { class, .. }) => {
                class
            }
            _ => return None,
        };
        if self.synth_spec(class).is_some_and(|spec| spec.frozen)
            && self.canonical_class_capabilities(class)
        {
            return Some(format!(
                "Cannot {} attribute '{name}' on a frozen instance",
                if delete { "delete" } else { "assign" }
            ));
        }
        for owner in self.mro(class).iter().take(32) {
            let info = self.class_info(owner)?;
            if let Some(member) = info.members.get(name) {
                if member.kind == MemberKind::Property {
                    let source = self.db.file(owner.file)?;
                    let node = node_with_range(&source.parse.syntax_node(), owner.range, |node| {
                        node.kind() == SyntaxKind::CLASS_DEF
                    })
                    .and_then(ast::ClassDef::cast)?;
                    let getter=node.methods().find(|function|function.name_text()==name && function.decorators().any(|decorator|decorator.expr().is_some_and(|expression| {
                        matches!(self.type_of_expr(&source,&expression),Type::Class(class,_) if self.builtin_class("property").as_ref()==Some(&class))
                    })))?;
                    let _ = getter;
                    let accessor = if delete { "deleter" } else { "setter" };
                    if !node.methods().any(|function|function.name_text()==name && function.decorators().any(|decorator|matches!(decorator.expr(),Some(ast::Expr::Attribute(attribute)) if attribute.attr_text()==accessor))) {
                        return Some(format!("Property '{name}' has no {accessor}"));
                    }
                    return None;
                }
                break;
            }
        }
        // A custom __setattr__ may intentionally use external storage.
        if self
            .attribute_hook(
                class,
                &[],
                receiver,
                if delete { "__delattr__" } else { "__setattr__" },
                name,
            )
            .is_some()
        {
            return None;
        }
        let mut allowed = std::collections::BTreeSet::new();
        for owner in self.mro(class).iter().take(32) {
            if self.builtin_class("object").as_ref() == Some(owner) {
                continue;
            }
            let info = self.class_info(owner)?;
            let generated = self.synth_spec(owner).is_some_and(|spec| spec.slots)
                && self.canonical_class_capabilities(owner);
            if generated {
                allowed.extend(self.synthesized_slot_names(owner)?);
                continue;
            }
            let slots = info.members.get("__slots__")?;
            let source = self.db.file(slots.file)?;
            let value = slots.value?;
            let expression = node_with_range(&source.parse.syntax_node(), value, |node| {
                ast::Expr::can_cast(node.kind())
            })
            .and_then(ast::Expr::cast)?
            .unparen();
            let values: Vec<_> = match expression {
                ast::Expr::Tuple(tuple) => tuple.elements().take(129).collect(),
                ast::Expr::List(list) => list.elements().take(129).collect(),
                value => vec![value],
            };
            if values.len() > 128 {
                return None;
            }
            for value in values {
                let ast::Expr::String(value) = value else {
                    return None;
                };
                let name = value.value();
                if name == "__dict__" {
                    return None;
                }
                allowed.insert(SmolStr::new(name));
            }
        }
        if !allowed.contains(name) {
            Some(format!("Attribute '{name}' is not declared in __slots__"))
        } else {
            None
        }
    }
    pub fn runtime_class_check_error(&self, target: &Type, subclass: bool) -> Option<String> {
        if let Type::Tuple(arms) | Type::Union(arms) = target {
            return arms
                .iter()
                .take(32)
                .find_map(|arm| self.runtime_class_check_error(arm, subclass));
        }
        let Type::Class(class, args) = target else {
            return None;
        };
        if !args.is_empty() {
            return Some("Parameterized generic cannot be used in a runtime class check".into());
        }
        let info = self.class_info(class)?;
        if !info.is_protocol {
            return None;
        }
        let runtime = self.mro(class).iter().take(32).any(|owner| {
            let Some(file) = self.db.file(owner.file) else {
                return false;
            };
            let Some(node) = node_with_range(&file.parse.syntax_node(), owner.range, |node| {
                node.kind() == SyntaxKind::CLASS_DEF
            })
            .and_then(ast::ClassDef::cast) else {
                return false;
            };
            node.decorators()
                .take(32)
                .filter_map(|decorator| decorator.expr())
                .any(|expression| {
                    self.symbol_origin(&file, &expression)
                        .is_some_and(|(_, name)| name == "runtime_checkable")
                        && self.canonical_typing_class_metadata(&file, &expression) == Some(true)
                })
        });
        if !runtime {
            return Some("Protocol must be @runtime_checkable for runtime class checks".into());
        }
        if subclass
            && self
                .mro(class)
                .iter()
                .take(32)
                .filter_map(|owner| self.class_info(owner))
                .any(|info| {
                    info.members.values().any(|member| {
                        !matches!(
                            member.name.as_str(),
                            "__module__"
                                | "__dict__"
                                | "__weakref__"
                                | "__doc__"
                                | "__annotations__"
                                | "__orig_bases__"
                                | "_is_protocol"
                                | "_is_runtime_protocol"
                        ) && !member.name.starts_with("_abc_")
                            && matches!(
                                member.kind,
                                MemberKind::Property
                                    | MemberKind::ClassVar
                                    | MemberKind::InstanceVar
                            )
                    })
                })
        {
            return Some("Protocol with data members does not support issubclass".into());
        }
        None
    }
}

impl Inferer<'_> {
    fn final_annotation(&self, file: &Arc<FileInfo>, range: TextRange) -> bool {
        let Some(mut expression) = node_with_range(&file.parse.syntax_node(), range, |node| {
            ast::Expr::can_cast(node.kind())
        })
        .and_then(ast::Expr::cast) else {
            return false;
        };
        for _ in 0..8 {
            let scope = self.scope_of(file, expression.syntax());
            if self
                .scoped_qualifier_name(file, scope, &expression)
                .as_deref()
                == Some("Final")
            {
                return true;
            }
            let ast::Expr::Subscript(subscript) = expression.unparen() else {
                return false;
            };
            let Some(head) = subscript.object() else {
                return false;
            };
            match self.scoped_qualifier_name(file, scope, &head).as_deref() {
                Some("Final") => return true,
                Some("Annotated" | "ClassVar" | "Required" | "NotRequired" | "ReadOnly") => {}
                _ => return false,
            }
            let Some(index) = subscript.index() else {
                return false;
            };
            let Some(value) = index
                .children()
                .filter_map(ast::Expr::cast)
                .next()
                .or_else(|| ast::Expr::cast(index))
            else {
                return false;
            };
            expression = value;
        }
        false
    }
    pub fn final_write_error(
        &self,
        file: &Arc<FileInfo>,
        target: &ast::Expr,
        delete: bool,
    ) -> Option<String> {
        let target = target.clone().unparen();
        match &target {
            ast::Expr::Name(name) => {
                let scope = self.scope_of(file, name.syntax());
                let ids = file.scopes.resolve(scope, &name.name_text())?;
                for id in ids.iter().take(128) {
                    let declaration = file.scopes.binding(*id);
                    let Some(annotation) = declaration.annotation else {
                        continue;
                    };
                    if declaration.range.start() >= target.text_range().start() {
                        continue;
                    }
                    if self.final_annotation(file, annotation) {
                        return Some(format!(
                            "Cannot {} final name '{}'",
                            if delete { "delete" } else { "reassign" },
                            name.name_text()
                        ));
                    }
                }
                None
            }
            ast::Expr::Attribute(attribute) => {
                let object = attribute.object()?;
                let receiver = self.type_of_expr(file, &object);
                let class = receiver.class_ref()?;
                for owner in self.mro(class).iter().take(32) {
                    let info = self.class_info(owner)?;
                    let Some(member) = info.members.get(attribute.attr_text().as_str()) else {
                        continue;
                    };
                    let source = self.db.file(member.file)?;
                    let annotation = member.annotation?;
                    if !self.final_annotation(&source, annotation) {
                        return None;
                    }
                    if !delete && member.value.is_none() && matches!(receiver, Type::Instance(..)) {
                        let function = target
                            .syntax()
                            .ancestors()
                            .take(64)
                            .find_map(ast::FuncDef::cast);
                        if let Some(function) =
                            function.filter(|function| function.name_text() == "__init__")
                        {
                            let self_name = function
                                .params()?
                                .params()
                                .next()?
                                .name()?
                                .text()
                                .to_owned();
                            if matches!(object.unparen(),ast::Expr::Name(name) if name.name_text()==self_name)
                            {
                                let prior=function.body()?.syntax().descendants().take(4096).filter(|node|node.text_range().start()<target.text_range().start())
                                    .filter_map(ast::Stmt::cast).any(|statement| {
                                        let targets=match statement{ast::Stmt::Assign(statement)=>statement.targets(),ast::Stmt::AnnAssign(statement)=>statement.target().into_iter().collect(),ast::Stmt::AugAssign(statement)=>statement.target().into_iter().collect(),_=>Vec::new()};
                                        targets.into_iter().any(|target|matches!(target,ast::Expr::Attribute(value) if value.attr_text()==attribute.attr_text() && matches!(value.object(),Some(ast::Expr::Name(name)) if name.name_text()==self_name)))
                                    });
                                if !prior {
                                    return None;
                                }
                            }
                        }
                    }
                    return Some(format!(
                        "Cannot {} final attribute '{}'",
                        if delete { "delete" } else { "reassign" },
                        attribute.attr_text()
                    ));
                }
                None
            }
            _ => None,
        }
    }
}

impl Inferer<'_> {
    pub fn class_declaration_errors(
        &self,
        file: &Arc<FileInfo>,
        class: &ast::ClassDef,
    ) -> Vec<(TextRange, String)> {
        if class.text_range().len() > TextSize::new(65_536) {
            return Vec::new();
        }
        let reference = ClassRef {
            file: file.id,
            range: class.text_range(),
            name: class.name_text().into(),
        };
        let mut errors = Vec::new();
        for base in class.bases().into_iter().take(32) {
            let Type::Class(base_class, _) = self.type_of_expr(file, &base) else {
                continue;
            };
            if let Some(source) = self.db.file(base_class.file) {
                if let Some(node) =
                    node_with_range(&source.parse.syntax_node(), base_class.range, |node| {
                        node.kind() == SyntaxKind::CLASS_DEF
                    })
                    .and_then(ast::ClassDef::cast)
                {
                    if node
                        .decorators()
                        .take(32)
                        .filter_map(|decorator| decorator.expr())
                        .any(|expression| {
                            self.symbol_origin(&source, &expression)
                                .is_some_and(|(_, name)| name == "final")
                                && self.canonical_typing_class_metadata(&source, &expression)
                                    == Some(true)
                        })
                    {
                        errors.push((
                            base.text_range(),
                            format!("Cannot inherit from final class '{}'", base_class.name),
                        ));
                    }
                }
            }
        }
        let ancestors: Vec<_> = self
            .mro(&reference)
            .iter()
            .skip(1)
            .take(32)
            .cloned()
            .collect();
        for method in class.methods().take(256) {
            let name = method.name_text();
            let overridden = ancestors.iter().find(|owner| {
                self.class_info(owner)
                    .is_some_and(|info| info.members.contains_key(name.as_str()))
            });
            for owner in &ancestors {
                let Some(source) = self.db.file(owner.file) else {
                    continue;
                };
                let Some(node) =
                    node_with_range(&source.parse.syntax_node(), owner.range, |node| {
                        node.kind() == SyntaxKind::CLASS_DEF
                    })
                    .and_then(ast::ClassDef::cast)
                else {
                    continue;
                };
                if node
                    .methods()
                    .filter(|method| method.name_text() == name)
                    .take(32)
                    .any(|method| self.function_has_canonical_marker(&source, &method, "final"))
                {
                    errors.push((
                        method
                            .name()
                            .map(|name| name.text_range())
                            .unwrap_or(method.text_range()),
                        format!("Cannot override final method '{name}'"),
                    ));
                    break;
                }
            }
            if self.function_has_canonical_marker(file, &method, "override") {
                let range = method
                    .name()
                    .map(|name| name.text_range())
                    .unwrap_or(method.text_range());
                let Some(owner) = overridden else {
                    errors.push((
                        range,
                        format!("Method '{name}' does not override a base member"),
                    ));
                    continue;
                };
                let actual =
                    self.lookup_member(&Type::Instance(reference.clone(), Vec::new()), &name);
                let expected =
                    self.lookup_member(&Type::Instance(owner.clone(), Vec::new()), &name);
                if let (Some(actual), Some(expected)) = (actual, expected) {
                    if actual.is_callable()
                        && expected.is_callable()
                        && !self.is_compatible(&actual, &expected)
                    {
                        errors.push((
                            range,
                            format!("Method '{name}' is incompatible with the overridden member"),
                        ));
                    }
                }
            }
        }
        errors
    }
}

impl Inferer<'_> {
    pub(super) fn callable_assignable(&self, actual: &Type, expected: &Type) -> bool {
        fn signatures(value: &Type) -> Option<Vec<Arc<Signature>>> {
            let values = match value {
                Type::Function(value) | Type::Callable(value) | Type::BoundMethod(value, _) => {
                    vec![value.clone()]
                }
                Type::Overloaded(values) if values.len() <= 32 => values.clone(),
                _ => return None,
            };
            if values.iter().any(|value| value.params.len() > 128) {
                return None;
            }
            Some(values)
        }
        let (Some(actual), Some(expected)) = (signatures(actual), signatures(expected)) else {
            return false;
        };
        expected.iter().all(|expected| {
            actual.iter().any(|actual| {
                // Callable compatibility concerns invocation results. Async metadata
                // has already been represented in the coroutine return type.
                let actual = Signature {
                    is_async: false,
                    ..(**actual).clone()
                };
                let expected = Signature {
                    is_async: false,
                    ..(**expected).clone()
                };
                self.protocol_signature_compatible(&actual, &expected, &mut Vec::new())
            })
        })
    }
}

impl Inferer<'_> {
    /// Implicit protocol dispatch performs type lookup, bypassing the value's
    /// __getattribute__/__getattr__ and instance dictionary. Descriptors found
    /// on that type still bind normally.
    pub(super) fn implicit_slot(&self, receiver: &Type, name: &str) -> Option<Type> {
        let depth = self.member_protocol_depth.get();
        if depth >= 32 {
            return Some(Type::Unknown);
        }
        self.member_protocol_depth.set(depth + 1);
        let result = self.implicit_slot_inner(receiver, name);
        self.member_protocol_depth.set(depth);
        result
    }
    fn implicit_slot_inner(&self, receiver: &Type, name: &str) -> Option<Type> {
        if name == "__call__" {
            if let Some(value) = self.functools_member(receiver, name) {
                return Some(value);
            }
        }
        if matches!(receiver, Type::Instance(..) | Type::SelfType(_)) {
            // `@dataclass(order=True)`, attrs ordering, `functools.total_ordering`.
            if let Some(value) = self.synthesized_ordering_member(receiver, name) {
                return Some(value);
            }
        }
        let (class, arguments) = match receiver {
            Type::Instance(class, arguments) => (class.clone(), arguments.clone()),
            Type::SelfType(class) | Type::Literal(LiteralValue::EnumMember { class, .. }) => {
                (class.clone(), Vec::new())
            }
            Type::Class(class, _) => (
                self.construction_metaclass(class, 0)
                    .ok()?
                    .or_else(|| self.builtin_class("type"))?,
                Vec::new(),
            ),
            Type::Tuple(_) => (self.builtin_class("tuple")?, Vec::new()),
            Type::Literal(value) => (self.builtin_class(literal_class_name(value))?, Vec::new()),
            Type::LiteralString | Type::LiteralStringValue => {
                (self.builtin_class("str")?, Vec::new())
            }
            Type::None => (self.builtin_class("NoneType")?, Vec::new()),
            Type::Any => return Some(Type::Any),
            Type::Unknown => return Some(Type::Unknown),
            _ => return None,
        };
        let value = self.lookup_in_mro(&class, &arguments, name, true, receiver)?;
        Some(self.descriptor_value(value, receiver, &class, &arguments))
    }
}
