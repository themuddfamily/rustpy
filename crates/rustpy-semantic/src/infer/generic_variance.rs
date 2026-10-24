//! Automatic generic variance from maintained declaration/member contracts.
//! No program execution; opaque shapes and exhausted/cyclic proofs are invariant.
use super::*;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Polarity {
    positive: bool,
    negative: bool,
}
impl Polarity {
    fn unknown() -> Self {
        Self {
            positive: true,
            negative: true,
        }
    }
    fn merge(&mut self, other: Self) {
        self.positive |= other.positive;
        self.negative |= other.negative;
    }
    fn reverse(self) -> Self {
        Self {
            positive: self.negative,
            negative: self.positive,
        }
    }
    fn invariant(self) -> Self {
        Self {
            positive: self.positive || self.negative,
            negative: self.positive || self.negative,
        }
    }
    fn variance(self) -> Variance {
        match (self.positive, self.negative) {
            (true, false) | (false, false) => Variance::Covariant,
            (false, true) => Variance::Contravariant,
            _ => Variance::Invariant,
        }
    }
}
struct Budget {
    steps: usize,
    classes: FxHashSet<(ClassRef, SmolStr)>,
    estimates: FxHashMap<(ClassRef, SmolStr), Polarity>,
}
impl Budget {
    fn charge(&mut self) -> bool {
        if self.steps == 0 {
            false
        } else {
            self.steps -= 1;
            true
        }
    }
}
impl Inferer<'_> {
    pub(super) fn inferred_generic_variance(&self, class: &ClassRef, key: &str) -> Variance {
        // Solve a finite monotone usage lattice. A recursive edge contributes
        // no invented polarity before a reachable declaration establishes it.
        // Only the checked fixed point is published, never a tentative seed.
        let mut budget = Budget {
            steps: 4096,
            classes: FxHashSet::default(),
            estimates: FxHashMap::default(),
        };
        for _ in 0..65 {
            let before = budget.estimates.clone();
            let result = self.class_usage(class, key, &mut budget);
            if budget.steps == 0 {
                return Variance::Invariant;
            }
            if before == budget.estimates {
                return result.variance();
            }
        }
        Variance::Invariant
    }
    fn class_usage(&self, class: &ClassRef, key: &str, budget: &mut Budget) -> Polarity {
        let identity = (class.clone(), SmolStr::new(key));
        if !budget.charge() {
            return Polarity::unknown();
        }
        if budget.classes.contains(&identity) {
            return budget.estimates.get(&identity).copied().unwrap_or_default();
        }
        if !budget.estimates.contains_key(&identity) && budget.estimates.len() >= 32 {
            return Polarity::unknown();
        }
        budget.estimates.entry(identity.clone()).or_default();
        budget.classes.insert(identity.clone());
        let mut result = self
            .class_usage_inner(class, key, budget)
            .unwrap_or_else(Polarity::unknown);
        budget.classes.remove(&identity);
        result.merge(budget.estimates.get(&identity).copied().unwrap_or_default());
        budget.estimates.insert(identity, result);
        result
    }
    fn class_usage_inner(
        &self,
        class: &ClassRef,
        key: &str,
        budget: &mut Budget,
    ) -> Option<Polarity> {
        let file = self.db.file(class.file)?;
        let definition = node_with_range(&file.parse.syntax_node(), class.range, |node| {
            node.kind() == SyntaxKind::CLASS_DEF
        })
        .and_then(ast::ClassDef::cast)?;
        let info = self.class_info(class)?;
        // Decorators/metaclass construction can replace a declaration/interface.
        if !info.decorators.is_empty() || !info.keywords.is_empty() {
            return None;
        }
        let defining = file.scopes.scope(info.scope).parent.unwrap_or(info.scope);
        let mut result = Polarity::default();
        // Evaluate base type arguments in the maintained annotation namespace,
        // preserving PEP695 keys, then compose the base's actual parameter variance.
        for base in definition.bases() {
            if !budget.charge() {
                return None;
            }
            let head = match &base {
                ast::Expr::Subscript(s) => s.object()?,
                _ => base.clone(),
            };
            if self.variance_marker(&file, defining, &head) {
                continue;
            }
            let ty = self.annotation_type_in_scope(&file, &base, defining);
            let Type::Instance(base_class, _) = &ty else {
                return None;
            };
            if base_class == class {
                return None;
            }
            result.merge(self.type_usage(&ty, key, budget));
        }
        let own_args = self.class_parameter_types(class);
        let receiver = Type::Instance(class.clone(), own_args);
        for member in info.members.values() {
            if !budget.charge() {
                return None;
            }
            if !matches!(member.kind, MemberKind::ClassVar | MemberKind::InstanceVar) {
                continue;
            }
            let ty = self.member_type(class, member, true, &receiver)?;
            let polarity = self.type_usage(&ty, key, budget);
            // Python's public assignability contract treats private storage as
            // implementation detail. Public methods writing it contribute their
            // input polarity below; public writable fields are invariant.
            result.merge(
                if member.name.starts_with('_') || self.variance_final_member(member) {
                    polarity
                } else {
                    polarity.invariant()
                },
            );
        }
        let body = definition.body()?;
        let implementation_only =
            self.variance_overload_implementations(&file, &definition, info.scope, budget)?;
        let mut properties: FxHashSet<String> = FxHashSet::default();
        for statement in body.statements() {
            if !budget.charge() {
                return None;
            }
            let function = match statement {
                ast::Stmt::FuncDef(f) => f,
                ast::Stmt::Pass(_) => continue,
                ast::Stmt::Assign(assignment) => {
                    for target in assignment.targets() {
                        for name in target
                            .syntax()
                            .descendants()
                            .filter_map(ast::NameExpr::cast)
                        {
                            properties.remove(name.name_text().as_str());
                        }
                    }
                    // An arbitrary evaluated callback can replace descriptors.
                    if assignment.value().is_some_and(|value| {
                        value.syntax().descendants().any(|node| {
                            matches!(node.kind(), SyntaxKind::CALL_EXPR | SyntaxKind::NAMED_EXPR)
                        })
                    }) {
                        properties.clear();
                    }
                    continue;
                }
                ast::Stmt::AnnAssign(assignment) => {
                    if assignment.value().is_some() {
                        if let Some(target) = assignment.target() {
                            for name in target
                                .syntax()
                                .descendants()
                                .filter_map(ast::NameExpr::cast)
                            {
                                properties.remove(name.name_text().as_str());
                            }
                        }
                        properties.clear();
                    }
                    continue;
                }
                ast::Stmt::Expr(e) if matches!(e.expr(), Some(ast::Expr::String(_))) => continue,
                _ => return None,
            };
            if matches!(
                function.name_text().as_str(),
                "__getattr__" | "__getattribute__" | "__setattr__"
            ) {
                return None;
            }
            if matches!(function.name_text().as_str(), "__init__" | "__new__") {
                continue;
            }
            if implementation_only.contains(&function.text_range()) {
                continue;
            }
            let mut is_static = false;
            let mut retains_property = false;
            if function.decorators().count() > 2 {
                return None;
            }
            for decorator in function.decorators() {
                let expression = decorator.expr()?.unparen();
                if self.variance_typing_symbol(
                    &file,
                    info.scope,
                    &expression,
                    function.text_range().start(),
                    "overload",
                ) {
                    continue;
                }
                match expression {
                    ast::Expr::Name(name)
                        if matches!(
                            name.name_text().as_str(),
                            "property" | "staticmethod" | "classmethod"
                        ) =>
                    {
                        let binding = self.resolve_name(&file, defining, &name.name_text())?;
                        if !binding.file.is_stub
                            || binding.file.module_name.as_deref() != Some("builtins")
                        {
                            return None;
                        }
                        if name.name_text() == "staticmethod" {
                            is_static = true;
                        }
                        if name.name_text() == "property" {
                            retains_property = true;
                        }
                    }
                    ast::Expr::Attribute(a)
                        if matches!(a.attr_text().as_str(), "setter" | "deleter") =>
                    {
                        let ast::Expr::Name(name) = a.object()?.unparen() else {
                            return None;
                        };
                        if name.name_text() != function.name_text()
                            || !properties.contains(name.name_text().as_str())
                        {
                            return None;
                        }
                        retains_property = true;
                    }
                    // Unknown/overload/custom wrappers can alter signatures;
                    // they need their own declared authority before inference.
                    _ => return None,
                }
            }
            // Only a previously established own canonical property can supply
            // setter/deleter identity. Global/custom/inherited unknown descriptors
            // and intervening replacements never establish this authority.
            if retains_property {
                properties.insert(function.name_text());
            } else {
                properties.remove(function.name_text().as_str());
            }
            let signature = self.signature(&file, &function, Some(class));
            let skip = usize::from(!is_static);
            if skip == 1 {
                let receiver = signature.params.first()?;
                if !matches!(&receiver.ty,Type::Instance(c,_)|Type::Class(c,_)|Type::SelfType(c) if c==class)
                {
                    return None;
                }
            }
            for parameter in signature.params.iter().skip(skip) {
                result.merge(self.type_usage(&parameter.ty, key, budget).reverse());
            }
            result.merge(self.type_usage(&signature.ret, key, budget));
        }
        Some(result)
    }
    // Qualifier recognition stays tied to the original annotation namespace,
    // including when a quoted expression is reparsed at synthetic offsets.
    fn variance_final_member(&self, member: &Member) -> bool {
        let Some(file) = self.db.file(member.file) else {
            return false;
        };
        let Some(range) = member.annotation else {
            return false;
        };
        let Some(annotation) = node_with_range(&file.parse.syntax_node(), range, |node| {
            ast::Expr::can_cast(node.kind())
        })
        .and_then(ast::Expr::cast) else {
            return false;
        };
        self.variance_final_expression(
            &file,
            file.scopes.scope_at(range.start()),
            &annotation,
            range.start(),
            0,
        )
    }
    fn variance_final_expression(
        &self,
        file: &Arc<FileInfo>,
        scope: ScopeId,
        expression: &ast::Expr,
        before: TextSize,
        depth: usize,
    ) -> bool {
        if depth >= 8 {
            return false;
        }
        match expression.clone().unparen() {
            ast::Expr::String(string) => {
                let raw = string.syntax().text().to_string();
                if raw.len() > 4096 {
                    return false;
                }
                let Ok(rustpy_syntax::string_literal::Decoded::Text(text, _)) =
                    rustpy_syntax::string_literal::string(&raw)
                else {
                    return false;
                };
                let parsed = rustpy_syntax::parse_expression(&text);
                if !parsed.errors().is_empty() {
                    return false;
                }
                let Some(inner) = parsed.syntax_node().children().find_map(ast::Expr::cast) else {
                    return false;
                };
                self.variance_final_expression(file, scope, &inner, before, depth + 1)
            }
            ast::Expr::Subscript(subscript) => {
                let Some(head) = subscript.object().map(ast::Expr::unparen) else {
                    return false;
                };
                let Some(index) = subscript.index() else {
                    return false;
                };
                let arguments: Vec<ast::Expr> = if index.kind() == SyntaxKind::SLICE_LIST {
                    index
                        .children()
                        .filter_map(ast::Expr::cast)
                        .take(17)
                        .collect()
                } else if let Some(ast::Expr::Tuple(tuple)) = ast::Expr::cast(index.clone()) {
                    tuple.elements().take(17).collect()
                } else {
                    ast::Expr::cast(index).into_iter().collect()
                };
                if arguments.len() > 16 {
                    return false;
                }
                if self.variance_typing_symbol(file, scope, &head, before, "Final") {
                    return arguments.len() == 1;
                }
                self.variance_typing_symbol(file, scope, &head, before, "Annotated")
                    && arguments.len() >= 2
                    && self.variance_final_expression(file, scope, &arguments[0], before, depth + 1)
            }
            _ => false,
        }
    }
    fn variance_typing_symbol(
        &self,
        file: &Arc<FileInfo>,
        scope: ScopeId,
        head: &ast::Expr,
        before: TextSize,
        expected: &str,
    ) -> bool {
        let definition = match head.clone().unparen() {
            ast::Expr::Name(name) => {
                if !self.variance_single_prior_name(file, scope, &name.name_text(), before) {
                    return false;
                }
                self.resolve_name(file, scope, &name.name_text())
                    .map(|definition| self.follow_imports(definition))
            }
            ast::Expr::Attribute(attribute) => {
                let Some(ast::Expr::Name(name)) = attribute.object().map(ast::Expr::unparen) else {
                    return false;
                };
                if !self.variance_single_prior_name(file, scope, &name.name_text(), before) {
                    return false;
                }
                let Some(definition) = self.resolve_name(file, scope, &name.name_text()) else {
                    return false;
                };
                let BindingKind::Import(import) = &definition.binding().kind else {
                    return false;
                };
                if import.name.is_some() {
                    return false;
                }
                self.import_target_module(&definition)
                    .and_then(|module| self.resolve_in_module(&module, &attribute.attr_text(), 0))
                    .map(|definition| self.follow_imports(definition))
            }
            _ => None,
        };
        definition.is_some_and(|definition| {
            definition.file.is_stub
                && definition.file.path.to_string_lossy().contains("typeshed")
                && matches!(
                    definition.file.module_name.as_deref(),
                    Some("typing" | "typing_extensions")
                )
                && definition.binding().name == expected
        })
    }
    fn variance_overload_implementations(
        &self,
        file: &Arc<FileInfo>,
        class: &ast::ClassDef,
        scope: ScopeId,
        budget: &mut Budget,
    ) -> Option<FxHashSet<TextRange>> {
        let mut groups: FxHashMap<String, Vec<ast::FuncDef>> = FxHashMap::default();
        for statement in class.body()?.statements() {
            if !budget.charge() {
                return None;
            }
            if let ast::Stmt::FuncDef(function) = statement {
                groups
                    .entry(function.name_text())
                    .or_default()
                    .push(function);
            }
        }
        if groups.len() > 128 {
            return None;
        }
        let mut omitted = FxHashSet::default();
        for (name, functions) in groups {
            if !budget.charge() {
                return None;
            }
            if functions.len() > 32 {
                return None;
            }
            let is_overload = |function: &ast::FuncDef| {
                function.decorators().any(|decorator| {
                    decorator.expr().is_some_and(|expr| {
                        self.variance_typing_symbol(
                            file,
                            scope,
                            &expr,
                            function.text_range().start(),
                            "overload",
                        )
                    })
                })
            };
            if !functions.iter().any(&is_overload) {
                continue;
            }
            if file
                .scopes
                .bindings_in(scope, &name)
                .iter()
                .any(|id| !matches!(file.scopes.binding(*id).kind, BindingKind::Function))
            {
                return None;
            }
            let mut declarations = 0;
            let mut implementation = false;
            let mut descriptor = None;
            for (index, function) in functions.iter().enumerate() {
                if !budget.charge() {
                    return None;
                }
                let overload = is_overload(function);
                let mut current = None;
                let mut overload_count = 0;
                for decorator in function.decorators() {
                    if !budget.charge() {
                        return None;
                    }
                    let expression = decorator.expr()?.unparen();
                    if self.variance_typing_symbol(
                        file,
                        scope,
                        &expression,
                        function.text_range().start(),
                        "overload",
                    ) {
                        overload_count += 1;
                        continue;
                    }
                    let ast::Expr::Name(name) = expression else {
                        return None;
                    };
                    if !matches!(name.name_text().as_str(), "staticmethod" | "classmethod") {
                        return None;
                    }
                    let binding =
                        self.follow_imports(self.resolve_name(file, scope, &name.name_text())?);
                    if !binding.file.is_stub
                        || binding.file.module_name.as_deref() != Some("builtins")
                        || current.is_some()
                    {
                        return None;
                    }
                    current = Some(name.name_text());
                }
                if index == 0 {
                    descriptor = Some(current.clone());
                }
                if descriptor.as_ref() != Some(&current) || overload_count > 1 {
                    return None;
                }
                if overload {
                    if implementation {
                        return None;
                    }
                    declarations += 1;
                } else {
                    if implementation || index + 1 != functions.len() {
                        return None;
                    }
                    implementation = true;
                    omitted.insert(function.text_range());
                }
            }
            if declarations < 2 || (!file.is_stub && !implementation) {
                return None;
            }
        }
        Some(omitted)
    }
    fn variance_single_prior_name(
        &self,
        file: &FileInfo,
        scope: ScopeId,
        name: &str,
        before: TextSize,
    ) -> bool {
        file.scopes
            .resolve(scope, name)
            .is_some_and(|ids| ids.len() == 1 && file.scopes.binding(ids[0]).range.start() < before)
    }
    fn variance_marker(&self, file: &Arc<FileInfo>, scope: ScopeId, head: &ast::Expr) -> bool {
        let binding = match head.clone().unparen() {
            ast::Expr::Name(name) => self.resolve_name(file, scope, &name.name_text()),
            ast::Expr::Attribute(attribute) => {
                let Some(object) = attribute.object() else {
                    return false;
                };
                let Type::Module(id) = self.type_of_expr(file, &object) else {
                    return false;
                };
                self.db
                    .file(id)
                    .and_then(|module| self.resolve_in_module(&module, &attribute.attr_text(), 0))
            }
            _ => None,
        };
        binding
            .map(|binding| self.follow_imports(binding))
            .is_some_and(|binding| {
                binding.file.is_stub
                    && binding.file.path.to_string_lossy().contains("typeshed")
                    && matches!(
                        binding.file.module_name.as_deref(),
                        Some("typing" | "typing_extensions")
                    )
                    && matches!(binding.binding().name.as_str(), "Generic" | "Protocol")
            })
    }
    fn type_usage(&self, ty: &Type, key: &str, budget: &mut Budget) -> Polarity {
        if !budget.charge() {
            return Polarity::unknown();
        }
        match ty {
            Type::DeclaredTypeVar(definition) => Polarity {
                positive: definition.key == key,
                negative: false,
            },
            Type::ParamSpec(definition) => Polarity {
                positive: definition.key == key,
                negative: false,
            },
            Type::TypeVarTuple(definition) => Polarity {
                positive: definition.key == key,
                negative: false,
            },
            Type::Unpack(value) => self.type_usage(value, key, budget),
            Type::ParameterList(parameters) => {
                let mut result = Polarity::default();
                for parameter in parameters {
                    result.merge(self.type_usage(&parameter.ty, key, budget));
                }
                result
            }
            Type::Concatenate(prefix, specification) => {
                let mut result = Polarity {
                    positive: specification.key == key,
                    negative: false,
                };
                for value in prefix {
                    result.merge(self.type_usage(value, key, budget));
                }
                result
            }
            // Unknown metadata might conceal the target parameter. Invariance
            // prevents opaque declarations from granting subtype conversions.
            Type::Unknown | Type::TypeVar(_) => Polarity::unknown(),
            Type::Tuple(types) | Type::Union(types) | Type::TypePack(types) => {
                let mut result = Polarity::default();
                for ty in types {
                    result.merge(self.type_usage(ty, key, budget));
                }
                result
            }
            Type::Instance(class, args) | Type::Class(class, args) => {
                let parameters = self.class_parameter_types(class);
                let Some(expanded) = self.expanded_class_parameters(&parameters, args.len()) else {
                    return Polarity::unknown();
                };
                let mut result = Polarity::default();
                for (parameter, argument) in expanded.into_iter().zip(args) {
                    let polarity = self.type_usage(argument, key, budget);
                    if !polarity.positive && !polarity.negative {
                        continue;
                    }
                    let Some((parameter_key, variance)) =
                        Self::generic_parameter_variance(parameter)
                    else {
                        return Polarity::unknown();
                    };
                    let usage = match variance {
                        Variance::Inferred => self.class_usage(class, parameter_key, budget),
                        Variance::Covariant => Polarity {
                            positive: true,
                            negative: false,
                        },
                        Variance::Contravariant => Polarity {
                            positive: false,
                            negative: true,
                        },
                        Variance::Invariant => Polarity::unknown(),
                    };
                    if usage.positive {
                        result.merge(polarity);
                    }
                    if usage.negative {
                        result.merge(polarity.reverse());
                    }
                }
                result
            }
            Type::Function(signature)
            | Type::Callable(signature)
            | Type::BoundMethod(signature, _) => self.signature_usage(signature, key, budget),
            Type::Overloaded(signatures) => {
                let mut result = Polarity::default();
                for signature in signatures {
                    result.merge(self.signature_usage(signature, key, budget));
                }
                result
            }
            Type::SelfType(class) => self.type_usage(
                &Type::Instance(class.clone(), self.class_parameter_types(class)),
                key,
                budget,
            ),
            _ => Polarity::default(),
        }
    }
    fn signature_usage(&self, signature: &Signature, key: &str, budget: &mut Budget) -> Polarity {
        let mut result = self.type_usage(&signature.ret, key, budget);
        for parameter in &signature.params {
            result.merge(self.type_usage(&parameter.ty, key, budget).reverse());
        }
        result
    }
}
