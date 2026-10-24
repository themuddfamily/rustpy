//! Captured predicate declarations and bounded nominal owner projections.
use super::*;
use crate::classes::MemberKind;
use crate::infer::{Definition, SynthKind};
use crate::scope::BindingKind;
use crate::types::{ParamKind, Signature};
use rustc_hash::FxHashSet;

impl Inferer<'_> {
    pub(super) fn narrowing_builtin(
        &self,
        file: &Arc<FileInfo>,
        expression: &ast::Expr,
    ) -> Option<String> {
        let ty = self.type_of_expr(file, expression);
        let canonical = |signature: &Signature| {
            let definition = signature.def.as_ref()?;
            let owner = self.db.file(definition.file)?;
            (owner.is_stub && owner.path == std::path::Path::new("<typeshed>/builtins.pyi"))
                .then(|| definition.name.to_string())
        };
        let name = match ty {
            Type::Function(signature) | Type::Callable(signature) => canonical(&signature)?,
            Type::Overloaded(signatures) if !signatures.is_empty() && signatures.len() <= 32 => {
                let name = canonical(&signatures[0])?;
                if !signatures
                    .iter()
                    .all(|signature| canonical(signature).as_ref() == Some(&name))
                {
                    return None;
                }
                name
            }
            Type::Class(class, _) if class.name == "type" => {
                let owner = self.db.file(class.file)?;
                if !owner.is_stub || owner.path != std::path::Path::new("<typeshed>/builtins.pyi") {
                    return None;
                }
                "type".to_string()
            }
            _ => return None,
        };
        if !matches!(
            name.as_str(),
            "isinstance" | "issubclass" | "callable" | "hasattr" | "len" | "type"
        ) || !Self::predicate_namespace_intact(file, &name, expression.text_range().start())
        {
            return None;
        }
        Some(name)
    }

    pub(super) fn predicate_subject_stable(node: &SyntaxNode, name: &str) -> bool {
        let principal = match ast::Expr::cast(node.clone()) {
            Some(ast::Expr::Call(call)) => call.arg_list().and_then(|arguments| {
                arguments
                    .args()
                    .filter_map(|argument| argument.value())
                    .find_map(|argument| subject_capture(&argument, name))
            }),
            Some(ast::Expr::Compare(compare)) => compare
                .operands()
                .find_map(|operand| subject_capture(&operand, name)),
            _ => None,
        };
        let allowed = principal.and_then(|expression| {
            matches!(expression, ast::Expr::Named(_)).then(|| expression.text_range())
        });
        let mut root = node.clone();
        for ancestor in node.ancestors().skip(1) {
            if ast::Stmt::can_cast(ancestor.kind())
                || matches!(
                    ancestor.kind(),
                    SyntaxKind::LAMBDA_EXPR | SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF
                )
            {
                break;
            }
            if ast::Expr::can_cast(ancestor.kind()) {
                root = ancestor;
            }
        }
        let mut budget = 16_384usize;
        for descendant in root.descendants() {
            if budget == 0 {
                return false;
            }
            budget -= 1;
            if let Some(assignment) = ast::NamedExpr::cast(descendant) {
                if assignment
                    .target()
                    .is_some_and(|target| target.name_text() == name)
                    && !allowed.is_some_and(|range| range.contains_range(assignment.text_range()))
                {
                    return false;
                }
            }
        }
        true
    }

    pub(super) fn narrowing_positional_arguments(call: &ast::CallExpr) -> Option<Vec<ast::Expr>> {
        let arguments: Vec<_> = call.arg_list()?.args().take(33).collect();
        if arguments.len() > 32
            || arguments
                .iter()
                .any(|argument| !matches!(argument, ast::AnyArg::Positional(_)))
        {
            return None;
        }
        arguments
            .into_iter()
            .map(|argument| argument.value())
            .collect()
    }

    pub(super) fn narrow_declared_predicate(
        &self,
        file: &Arc<FileInfo>,
        ty: &Type,
        call: &ast::CallExpr,
        positive: bool,
        name: &str,
    ) -> Option<Type> {
        if !Self::predicate_subject_stable(call.syntax(), name) {
            return None;
        }
        let (pos, kw) = self.call_args(file, call);
        let signature = match self.type_of_expr(file, &call.callee()?) {
            Type::Function(signature) | Type::BoundMethod(signature, _) => signature,
            Type::Overloaded(signatures) if !signatures.is_empty() && signatures.len() <= 32 => {
                // Retain the maintained first-compatible overload ordering;
                // never grant the selector's historical last-signature fallback.
                let mut selector_budget = 512;
                if !pos
                    .iter()
                    .skip(1)
                    .chain(kw.iter().map(|(_, ty)| ty))
                    .all(|argument| predicate_selection_known(argument, &mut selector_budget, 0))
                {
                    return None;
                }
                signatures.into_iter().find(|signature| {
                    predicate_call_shape(signature, pos.len(), &kw)
                        && self.arguments_fit(signature, &pos, &kw)
                })?
            }
            _ => return None,
        };
        if signature.is_async || signature.params.is_empty() || signature.params.len() > 32 {
            return None;
        }
        let definition = signature.def.as_ref()?;
        let owner = self.db.file(definition.file)?;
        let function = owner
            .parse
            .syntax_node()
            .descendants()
            .take(16_384)
            .find_map(|node| {
                (node.text_range() == definition.range)
                    .then(|| ast::FuncDef::cast(node))
                    .flatten()
            })?;
        if function.name_text() != definition.name.as_str()
            || !Self::predicate_namespace_intact(file, &definition.name, call.text_range().start())
            || call.callee().is_some_and(|callee| matches!(callee.unparen(), ast::Expr::Attribute(attribute) if !Self::predicate_namespace_intact(file, &attribute.attr_text(), call.text_range().start())))
            || !self.predicate_decorators(&owner, &function) {
            return None;
        }
        let physical_annotation = function.return_annotation()?;
        let scope = self.scope_of(&owner, physical_annotation.syntax());
        let cutoff = physical_annotation.text_range().start();
        let quoted = matches!(physical_annotation.clone().unparen(), ast::Expr::String(_));
        let expression = match physical_annotation.clone().unparen() {
            ast::Expr::String(string) => {
                if string.syntax().text().len() > rustpy_core::TextSize::new(65_536) {
                    return None;
                }
                let rustpy_syntax::string_literal::Decoded::Text(text, _) =
                    rustpy_syntax::string_literal::string(&string.syntax().text().to_string())
                        .ok()?
                else {
                    return None;
                };
                if text.len() > 65_536 {
                    return None;
                }
                let parsed = rustpy_syntax::parse_expression(&text);
                if !parsed.errors().is_empty() {
                    return None;
                }
                parsed.syntax_node().children().find_map(ast::Expr::cast)?
            }
            expression => expression,
        };
        let ast::Expr::Subscript(annotation) = expression.unparen() else {
            return None;
        };
        let head = annotation.object()?;
        let type_is = if self.predicate_symbol_in_scope(&owner, scope, cutoff, &head, "TypeGuard") {
            false
        } else if self.predicate_symbol_in_scope(&owner, scope, cutoff, &head, "TypeIs") {
            true
        } else {
            return None;
        };
        let payload_expression = annotation.index_expr()?;
        if matches!(payload_expression, ast::Expr::Tuple(_)) {
            return None;
        }
        let payload = if quoted {
            self.annotation_type_in_scope(&owner, &payload_expression, scope)
        } else {
            self.annotation_type(&owner, &payload_expression)
        };
        if matches!(payload, Type::Unknown | Type::Any) {
            return None;
        }
        if type_is
            && !declared_predicate_subtype(self, &payload, &signature.params[0].ty, 0, &mut 512)
        {
            return None;
        }
        let arguments: Vec<_> = call.arg_list()?.args().take(33).collect();
        if arguments.len() > 32
            || arguments.iter().any(|argument| {
                !matches!(
                    argument,
                    ast::AnyArg::Positional(_) | ast::AnyArg::Keyword(_)
                )
            })
        {
            return None;
        }
        let positional: Vec<_> = arguments
            .iter()
            .filter_map(|argument| match argument {
                ast::AnyArg::Positional(argument) => argument.expr(),
                _ => None,
            })
            .collect();
        let keyword = arguments.iter().find_map(|argument| match argument {
            ast::AnyArg::Keyword(argument)
                if argument
                    .name()
                    .is_some_and(|name| name.text() == signature.params[0].name.as_str()) =>
            {
                argument.value()
            }
            _ => None,
        });
        if !positional.is_empty() && keyword.is_some() {
            return None;
        }
        let subject = positional.first().cloned().or(keyword)?;
        if !is_subject(&subject, name) {
            return None;
        }
        if !predicate_call_shape(&signature, pos.len(), &kw) {
            return None;
        }
        let specialized = self.call_result(
            &Type::Function(Arc::new(Signature {
                ret: payload,
                ..(*signature).clone()
            })),
            &pos,
            &kw,
        );
        if matches!(specialized, Type::Unknown | Type::Any) {
            return None;
        }
        if !type_is {
            return Some(if positive { specialized } else { ty.clone() });
        }
        if !self.is_compatible(&specialized, &signature.params[0].ty) {
            return None;
        }
        Some(self.narrow_type_is(ty, &specialized, positive))
    }

    fn predicate_decorators(&self, file: &Arc<FileInfo>, function: &ast::FuncDef) -> bool {
        let decorators: Vec<_> = function.decorators().take(5).collect();
        if decorators.len() > 4 {
            return false;
        }
        decorators.into_iter().all(|decorator| {
            let Some(expression) = decorator.expr() else {
                return false;
            };
            if self.predicate_symbol(file, &expression, "overload", &mut FxHashSet::default()) {
                return true;
            }
            match self.type_of_expr(file, &expression) {
                Type::Class(class, _) => self.db.file(class.file).is_some_and(|owner| {
                    owner.is_stub
                        && owner.path == std::path::Path::new("<typeshed>/builtins.pyi")
                        && matches!(class.name.as_str(), "staticmethod" | "classmethod")
                        && Self::predicate_namespace_intact(
                            file,
                            &class.name,
                            expression.text_range().start(),
                        )
                }),
                _ => false,
            }
        })
    }

    fn predicate_symbol_in_scope(
        &self,
        file: &Arc<FileInfo>,
        scope: ScopeId,
        cutoff: rustpy_core::TextSize,
        expression: &ast::Expr,
        wanted: &str,
    ) -> bool {
        if !Self::predicate_namespace_intact(file, wanted, cutoff) {
            return false;
        }
        let definition = match expression.clone().unparen() {
            ast::Expr::Name(name) => self.resolve_name(file, scope, &name.name_text()),
            ast::Expr::Attribute(attribute) => {
                let Some(ast::Expr::Name(module_name)) = attribute.object().map(ast::Expr::unparen)
                else {
                    return false;
                };
                let Some(module_binding) = self.resolve_name(file, scope, &module_name.name_text())
                else {
                    return false;
                };
                if module_binding.file.id == file.id
                    && module_binding
                        .file
                        .scopes
                        .binding(module_binding.binding)
                        .range
                        .start()
                        > cutoff
                {
                    return false;
                }
                let Type::Module(id) =
                    self.type_of_binding(&module_binding.file, module_binding.binding)
                else {
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
        definition.is_some_and(|definition| {
            (definition.file.id != file.id
                || definition
                    .file
                    .scopes
                    .binding(definition.binding)
                    .range
                    .start()
                    < cutoff)
                && self.predicate_binding(
                    &definition.file,
                    definition.binding,
                    wanted,
                    &mut FxHashSet::default(),
                )
        })
    }

    fn predicate_protocol(&self, class: &crate::types::ClassRef) -> Option<bool> {
        let owners = self.mro(class);
        if owners.is_empty() || owners.len() > 32 {
            return None;
        }
        let mut budget = 16_384;
        for owner in owners.iter() {
            let file = self.db.file(owner.file)?;
            if file.is_stub
                && (file.path == std::path::Path::new("<typeshed>/builtins.pyi")
                    || file.path == std::path::Path::new("<typeshed>/typing.pyi")
                    || file.path == std::path::Path::new("<typeshed>/typing_extensions.pyi"))
            {
                continue;
            }
            let info = self.class_info(owner)?;
            if info.base_exprs.len() > 32 || info.members.len() > 256 {
                return None;
            }
            if info.base_exprs.is_empty() {
                continue;
            }
            let ranges: FxHashSet<_> = info.base_exprs.iter().copied().collect();
            for node in file.parse.syntax_node().descendants() {
                if budget == 0 {
                    return None;
                }
                budget -= 1;
                if ranges.contains(&node.text_range()) {
                    if let Some(expression) = ast::Expr::cast(node.clone()) {
                        let head = match expression.unparen() {
                            ast::Expr::Subscript(subscript) => subscript.object()?,
                            expression => expression,
                        };
                        if self.predicate_symbol(
                            &file,
                            &head,
                            "Protocol",
                            &mut FxHashSet::default(),
                        ) {
                            return Some(true);
                        }
                    }
                }
                if node.text_range().start() > owner.range.end() {
                    break;
                }
            }
        }
        Some(false)
    }

    fn predicate_protocol_relation(
        &self,
        actual: &Type,
        expected: &Type,
        budget: &mut usize,
        depth: usize,
        active: &mut Vec<(Type, Type)>,
    ) -> Option<bool> {
        if *budget == 0
            || depth >= 16
            || active
                .iter()
                .any(|pair| pair == &(actual.clone(), expected.clone()))
        {
            return None;
        }
        *budget -= 1;
        if !self.nominal_read_model(actual) {
            return None;
        }
        let Type::Instance(protocol, _) = expected else {
            return None;
        };
        let owners = self.mro(protocol);
        if owners.is_empty() || owners.len() > 32 {
            return None;
        }
        let mut names = FxHashSet::default();
        for owner in owners.iter() {
            let file = self.db.file(owner.file)?;
            if file.is_stub
                && (file.path == std::path::Path::new("<typeshed>/builtins.pyi")
                    || file.path == std::path::Path::new("<typeshed>/typing.pyi")
                    || file.path == std::path::Path::new("<typeshed>/typing_extensions.pyi"))
            {
                continue;
            }
            let info = self.class_info(owner)?;
            for name in info.members.keys() {
                if *budget == 0 || names.len() >= 256 {
                    return None;
                }
                *budget -= 1;
                if predicate_protocol_member(name) {
                    names.insert(name.clone());
                }
            }
        }
        active.push((actual.clone(), expected.clone()));
        let result = (|| {
            let mut uncertain = false;
            for name in names {
                let expected_member = self.lookup_member(expected, &name)?;
                let Some(actual_member) = self.lookup_member(actual, &name) else {
                    return Some(false);
                };
                // Writable protocol fields require both read and write
                // assignability. Property/method mutability is not inferred
                // from a read type alone; decline that unresolved capability.
                let expected_kind = self.predicate_member_kind(expected, &name);
                let actual_kind = self.predicate_member_kind(actual, &name);
                if !matches!(
                    expected_kind,
                    Some(MemberKind::InstanceVar | MemberKind::ClassVar)
                ) || !matches!(
                    actual_kind,
                    Some(MemberKind::InstanceVar | MemberKind::ClassVar)
                ) {
                    uncertain = true;
                    continue;
                }
                if self.predicate_member_classvar(expected, &name, budget)?
                    && (!matches!(actual_kind, Some(MemberKind::ClassVar))
                        || self.predicate_member_classvar(actual, &name, budget) != Some(true))
                {
                    uncertain = true;
                    continue;
                }
                for (from, to) in [
                    (&actual_member, &expected_member),
                    (&expected_member, &actual_member),
                ] {
                    match self.predicate_value_relation(from, to, budget, depth + 1, active) {
                        Some(false) => return Some(false),
                        None => uncertain = true,
                        Some(true) => {}
                    }
                }
            }
            if uncertain {
                None
            } else {
                Some(true)
            }
        })();
        active.pop();
        result
    }

    fn predicate_member_classvar(
        &self,
        receiver: &Type,
        name: &str,
        budget: &mut usize,
    ) -> Option<bool> {
        let Type::Instance(class, _) = receiver else {
            return None;
        };
        let owners = self.mro(class);
        if owners.len() > 32 {
            return None;
        }
        for owner in owners.iter() {
            let info = self.class_info(owner)?;
            let Some(member) = info.members.get(name) else {
                continue;
            };
            let Some(range) = member.annotation else {
                return Some(false);
            };
            let file = self.db.file(member.file)?;
            let mut physical = None;
            for node in file.parse.syntax_node().descendants() {
                if *budget == 0 {
                    return None;
                }
                *budget -= 1;
                if node.text_range() == range {
                    if let Some(expression) = ast::Expr::cast(node) {
                        physical = Some(expression);
                        break;
                    }
                }
            }
            let physical = physical?;
            let scope = self.scope_of(&file, physical.syntax());
            let cutoff = physical.text_range().start();
            let mut expression = match physical.clone().unparen() {
                ast::Expr::String(string) => {
                    if string.syntax().text().len() > rustpy_core::TextSize::new(65_536) {
                        return None;
                    }
                    let rustpy_syntax::string_literal::Decoded::Text(text, _) =
                        rustpy_syntax::string_literal::string(&string.syntax().text().to_string())
                            .ok()?
                    else {
                        return None;
                    };
                    let parsed = rustpy_syntax::parse_expression(&text);
                    if !parsed.errors().is_empty() {
                        return None;
                    }
                    parsed.syntax_node().children().find_map(ast::Expr::cast)?
                }
                expression => expression,
            };
            for _ in 0..16 {
                if *budget == 0 {
                    return None;
                }
                *budget -= 1;
                let ast::Expr::Subscript(subscript) = expression.unparen() else {
                    return Some(false);
                };
                let head = subscript.object()?;
                if self.predicate_symbol_in_scope(&file, scope, cutoff, &head, "ClassVar") {
                    return Some(true);
                }
                if !self.predicate_symbol_in_scope(&file, scope, cutoff, &head, "Annotated") {
                    return Some(false);
                }
                expression = match subscript.index_expr()? {
                    ast::Expr::Tuple(tuple) => tuple.elements().next()?,
                    expression => expression,
                };
            }
            return None;
        }
        None
    }

    fn predicate_member_kind(&self, receiver: &Type, name: &str) -> Option<MemberKind> {
        let Type::Instance(class, _) = receiver else {
            return None;
        };
        let owners = self.mro(class);
        if owners.len() > 32 {
            return None;
        }
        for owner in owners.iter() {
            let info = self.class_info(owner)?;
            if let Some(member) = info.members.get(name) {
                return Some(member.kind);
            }
        }
        None
    }

    fn predicate_value_relation(
        &self,
        actual: &Type,
        expected: &Type,
        budget: &mut usize,
        depth: usize,
        active: &mut Vec<(Type, Type)>,
    ) -> Option<bool> {
        if *budget == 0 || depth >= 16 {
            return None;
        }
        *budget -= 1;
        // Unknown, Any and unresolved recursive values are not definite
        // structural membership (nor definite exclusion) proofs.
        if !predicate_selection_known(actual, budget, depth)
            || !predicate_selection_known(expected, budget, depth)
        {
            return None;
        }
        if actual == expected {
            return Some(true);
        }
        match (actual, expected) {
            (Type::Instance(left, _), Type::Instance(right, _)) => {
                if self.has_unknown_base(left) || self.has_unknown_base(right) {
                    return None;
                }
                if self.predicate_protocol(right)? {
                    return self.predicate_protocol_relation(
                        actual,
                        expected,
                        budget,
                        depth + 1,
                        active,
                    );
                }
                Some(self.mro(left).contains(right) && self.is_compatible(actual, expected))
            }
            (Type::Tuple(left), Type::Tuple(right)) => {
                if left.len() != right.len() {
                    return Some(false);
                }
                let mut uncertain = false;
                for (left, right) in left.iter().zip(right) {
                    match self.predicate_value_relation(left, right, budget, depth + 1, active) {
                        Some(false) => return Some(false),
                        None => uncertain = true,
                        Some(true) => {}
                    }
                }
                if uncertain {
                    None
                } else {
                    Some(true)
                }
            }
            (Type::Literal(_), _) => self.predicate_value_relation(
                &self.widen(actual.clone()),
                expected,
                budget,
                depth + 1,
                active,
            ),
            (Type::None, Type::None) => Some(true),
            _ => None,
        }
    }

    fn predicate_symbol(
        &self,
        file: &Arc<FileInfo>,
        expression: &ast::Expr,
        wanted: &str,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
    ) -> bool {
        if seen.len() >= 16
            || !Self::predicate_namespace_intact(file, wanted, expression.text_range().start())
        {
            return false;
        }
        let definitions = match expression.clone().unparen() {
            ast::Expr::Name(name) => {
                let scope = self.scope_of(file, name.syntax());
                if !file.is_stub {
                    if let Some(scope) = flow::flow_scope(file, scope) {
                        let facts = self.scope_flow(file, scope);
                        if let Some(reaches) = facts.refs.get(&name.text_range().start()) {
                            return !reaches.is_empty()
                                && reaches.iter().all(|reach| {
                                    reach.binding.is_some_and(|binding| {
                                        self.predicate_binding(file, binding, wanted, seen)
                                    })
                                });
                        }
                    }
                }
                self.resolve_name(file, scope, &name.name_text())
            }
            ast::Expr::Attribute(attribute) => attribute.object().and_then(|object| {
                let Type::Module(id) = self.type_of_expr(file, &object) else {
                    return None;
                };
                let module = self.db.file(id)?;
                self.resolve_name(
                    &module,
                    module.scopes.module_scope(),
                    &attribute.attr_text(),
                )
            }),
            _ => None,
        };
        definitions.is_some_and(|definition| {
            self.predicate_binding(&definition.file, definition.binding, wanted, seen)
        })
    }

    fn predicate_binding(
        &self,
        file: &Arc<FileInfo>,
        binding: BindingId,
        wanted: &str,
        seen: &mut FxHashSet<(rustpy_core::FileId, BindingId)>,
    ) -> bool {
        if seen.len() >= 16 || !seen.insert((file.id, binding)) {
            return false;
        }
        let declaration = file.scopes.binding(binding);
        if file.is_stub
            && matches!(
                file.module_name.as_deref(),
                Some("typing" | "typing_extensions")
            )
            && (file.path == std::path::Path::new("<typeshed>/typing.pyi")
                || file.path == std::path::Path::new("<typeshed>/typing_extensions.pyi"))
            && declaration.name == wanted
            && !matches!(
                declaration.kind,
                BindingKind::Assignment | BindingKind::Walrus
            )
        {
            return true;
        }
        match &declaration.kind {
            BindingKind::Import(_) => {
                let followed = self.follow_imports(Definition {
                    file: file.clone(),
                    binding,
                });
                (followed.file.id != file.id || followed.binding != binding)
                    && self.predicate_binding(&followed.file, followed.binding, wanted, seen)
            }
            BindingKind::Assignment if declaration.annotation.is_none() => declaration
                .value
                .and_then(|range| {
                    file.parse
                        .syntax_node()
                        .descendants()
                        .take(16_384)
                        .find_map(|node| {
                            (node.text_range() == range)
                                .then(|| ast::Expr::cast(node))
                                .flatten()
                        })
                })
                .is_some_and(|value| self.predicate_symbol(file, &value, wanted, seen)),
            _ => false,
        }
    }

    fn narrow_type_is(&self, ty: &Type, payload: &Type, positive: bool) -> Type {
        if payload.is_none() {
            return self.narrow_none(ty, positive);
        }
        if let Type::Literal(literal) = payload {
            return self.narrow_literal(ty, literal, positive);
        }
        let input_members = members(ty);
        if input_members.len() > 256 {
            return ty.clone();
        }
        let mut result = Vec::new();
        let mut relation_budget = 512;
        for member in input_members {
            let subtype = nominal_subtype(self, &member, payload, &mut relation_budget);
            if positive {
                if subtype == Some(true) {
                    result.push(member);
                } else if nominal_subtype(self, payload, &member, &mut relation_budget)
                    == Some(true)
                    || matches!(member, Type::Any | Type::Unknown)
                {
                    result.push(payload.clone());
                } else if subtype.is_none() {
                    result.push(member);
                }
            } else if subtype != Some(true) {
                result.push(member);
            }
        }
        union_live(result)
    }

    fn nominal_read_model(&self, ty: &Type) -> bool {
        let Type::Instance(class, arguments) = ty else {
            return false;
        };
        let bases = self.mro_with_args(class, arguments);
        if bases.is_empty() || bases.len() > 32 {
            return false;
        }
        bases.iter().all(|(base, _)| {
            self.class_info(base).is_some_and(|info| {
                info.keywords.is_empty()
                    && info.decorators.is_empty()
                    && !info.members.contains_key("__getattribute__")
                    && !info.members.contains_key("__getattr__")
            }) || self.builtin_class("object").as_ref() == Some(base)
        })
    }

    fn property_member(&self, ty: &Type, member: &str) -> bool {
        let Type::Instance(class, _) = ty else {
            return false;
        };
        self.mro(class).iter().any(|owner| {
            self.class_info(owner).is_some_and(|info| {
                info.members
                    .get(member)
                    .is_some_and(|member| member.kind == MemberKind::Property)
            })
        })
    }

    pub(super) fn narrow_known_attribute(&self, ty: &Type, member: &str, positive: bool) -> Type {
        union_live(
            members(ty)
                .into_iter()
                .filter(|candidate| {
                    if !self.nominal_read_model(candidate)
                        || self.property_member(candidate, member)
                    {
                        return true;
                    }
                    self.lookup_member(candidate, member).is_some() == positive
                })
                .collect(),
        )
    }

    pub(super) fn narrow_projected_comparison(
        &self,
        file: &Arc<FileInfo>,
        ty: &Type,
        comparison: &ast::CompareExpr,
        positive: bool,
        name: &str,
    ) -> Option<Type> {
        let operations: Vec<_> = comparison
            .ops()
            .map(|operation| operation.text())
            .take(2)
            .collect();
        let operands: Vec<_> = comparison.operands().take(3).collect();
        if operations.len() != 1 || operands.len() != 2 {
            return None;
        }
        for (projection, literal, reversed) in [
            (&operands[0], &operands[1], false),
            (&operands[1], &operands[0], true),
        ] {
            if let ast::Expr::Call(call) = projection.clone().unparen() {
                if call
                    .callee()
                    .and_then(|callee| self.narrowing_builtin(file, &callee))
                    .as_deref()
                    == Some("len")
                {
                    let args = Self::narrowing_positional_arguments(&call)?;
                    if args.len() != 1 || !is_subject(&args[0], name) {
                        continue;
                    }
                    let Type::Literal(LiteralValue::Int(length)) = self.type_of_expr(file, literal)
                    else {
                        continue;
                    };
                    let operator = operations[0].as_str();
                    if !matches!(operator, "==" | "!=" | "<" | "<=" | ">" | ">=") {
                        continue;
                    }
                    return Some(union_live(
                        members(ty)
                            .into_iter()
                            .filter(|candidate| {
                                let Type::Tuple(values) = candidate else {
                                    return true;
                                };
                                if values.iter().any(|value| {
                                    matches!(value, Type::Unpack(_) | Type::TypeVarTuple(_))
                                }) {
                                    return true;
                                }
                                let (left, right) = if reversed {
                                    (length, values.len() as i64)
                                } else {
                                    (values.len() as i64, length)
                                };
                                let satisfied = match operator {
                                    "==" => left == right,
                                    "!=" => left != right,
                                    "<" => left < right,
                                    "<=" => left <= right,
                                    ">" => left > right,
                                    _ => left >= right,
                                };
                                satisfied == positive
                            })
                            .collect(),
                    ));
                }
            }
            if !matches!(operations[0].as_str(), "==" | "!=") {
                continue;
            }
            let (field, dictionary) = match projection.clone().unparen() {
                ast::Expr::Attribute(attribute)
                    if attribute
                        .object()
                        .is_some_and(|object| is_subject(&object, name)) =>
                {
                    (attribute.attr_text(), false)
                }
                ast::Expr::Subscript(subscript)
                    if subscript
                        .object()
                        .is_some_and(|object| is_subject(&object, name)) =>
                {
                    let Type::Literal(LiteralValue::Str(key)) =
                        self.type_of_expr(file, &subscript.index_expr()?)
                    else {
                        continue;
                    };
                    (key.to_string(), true)
                }
                _ => continue,
            };
            let Type::Literal(literal) = self.type_of_expr(file, literal) else {
                continue;
            };
            if !Self::projection_writes_absent(file, &field, comparison.text_range()) {
                return Some(ty.clone());
            }
            let equal = (operations[0] == "==") == positive;
            return Some(union_live(
                members(ty)
                    .into_iter()
                    .filter(|candidate| {
                        let Type::Instance(class, _) = candidate else {
                            return true;
                        };
                        if dictionary {
                            if !self
                                .synth_spec(class)
                                .is_some_and(|spec| spec.kind == SynthKind::TypedDict)
                            {
                                return true;
                            }
                        } else if !self.nominal_read_model(candidate) {
                            return true;
                        }
                        let Some(value) = self.lookup_member(candidate, &field) else {
                            return true;
                        };
                        if !dictionary
                            && self.mro(class).iter().any(|base| {
                                self.class_info(base).is_some_and(|info| {
                                    info.members
                                        .get(field.as_str())
                                        .is_some_and(|member| member.kind == MemberKind::Property)
                                })
                            })
                        {
                            return true;
                        }
                        let possible = self.narrow_literal(&value, &literal, equal);
                        !matches!(possible, Type::Never)
                    })
                    .collect(),
            ));
        }
        None
    }

    fn projection_writes_absent(file: &Arc<FileInfo>, field: &str, condition: TextRange) -> bool {
        let scope = file.scopes.scope_at(condition.start());
        let end = file.scopes.scope(scope).range.end();
        let mut budget = 16_384usize;
        for node in file.parse.syntax_node().descendants() {
            if node.text_range().start() >= end {
                break;
            }
            if budget == 0 {
                return false;
            }
            budget -= 1;
            if node.text_range().start() < condition.end() {
                continue;
            }
            let Some(targets) = written_targets(node, budget) else {
                return false;
            };
            for target in targets {
                if projection_target_write(target, field, &mut budget, 0) {
                    return false;
                }
            }
        }
        true
    }

    fn predicate_namespace_intact(
        file: &Arc<FileInfo>,
        wanted: &str,
        before: rustpy_core::TextSize,
    ) -> bool {
        if file.is_stub {
            return true;
        }
        let mut budget = 16_384usize;
        for node in file.parse.syntax_node().descendants() {
            if node.text_range().start() >= before {
                break;
            }
            if budget == 0 {
                return false;
            }
            budget -= 1;
            let Some(targets) = written_targets(node, budget) else {
                return false;
            };
            for target in targets {
                if retired_target(target, wanted, &mut budget, 0) {
                    return false;
                }
            }
        }
        true
    }
}

fn predicate_protocol_member(name: &str) -> bool {
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

fn predicate_selection_known(ty: &Type, budget: &mut usize, depth: usize) -> bool {
    if *budget == 0 || depth >= 16 {
        return false;
    }
    *budget -= 1;
    match ty {
        Type::Instance(_, arguments)
        | Type::Class(_, arguments)
        | Type::Tuple(arguments)
        | Type::Union(arguments) => arguments
            .iter()
            .all(|argument| predicate_selection_known(argument, budget, depth + 1)),
        Type::Literal(_) | Type::None | Type::Never => true,
        _ => false,
    }
}

fn declared_predicate_subtype(
    inferer: &Inferer<'_>,
    payload: &Type,
    domain: &Type,
    depth: usize,
    budget: &mut usize,
) -> bool {
    if depth >= 16 || *budget == 0 {
        return false;
    }
    *budget -= 1;
    if payload == domain {
        return true;
    }
    match (payload, domain) {
        (Type::Union(members), _) => members
            .iter()
            .all(|member| declared_predicate_subtype(inferer, member, domain, depth + 1, budget)),
        (_, Type::Union(members)) => members
            .iter()
            .any(|member| declared_predicate_subtype(inferer, payload, member, depth + 1, budget)),
        (_, Type::Any | Type::Unknown) => false,
        _ => nominal_subtype(inferer, payload, domain, budget) == Some(true),
    }
}

fn nominal_subtype(
    inferer: &Inferer<'_>,
    argument: &Type,
    expected: &Type,
    budget: &mut usize,
) -> Option<bool> {
    if *budget == 0 {
        return None;
    }
    *budget -= 1;
    match (argument, expected) {
        (Type::Instance(actual, _), Type::Instance(target, _)) => {
            if !predicate_selection_known(argument, budget, 0)
                || !predicate_selection_known(expected, budget, 0)
            {
                return None;
            }
            if inferer.has_unknown_base(actual) || inferer.has_unknown_base(target) {
                return None;
            }
            if inferer.predicate_protocol(target)? {
                return inferer.predicate_protocol_relation(
                    argument,
                    expected,
                    budget,
                    0,
                    &mut Vec::new(),
                );
            }
            Some(inferer.mro(actual).contains(target) && inferer.is_compatible(argument, expected))
        }
        (Type::None, Type::None) => Some(true),
        (Type::None, Type::Instance(_, _)) | (Type::Instance(_, _), Type::None) => Some(false),
        (Type::Literal(left), Type::Literal(right)) => Some(left == right),
        _ => None,
    }
}

fn predicate_call_shape(
    signature: &Signature,
    positional: usize,
    keywords: &[(smol_str::SmolStr, Type)],
) -> bool {
    let ordinary: Vec<_> = signature
        .params
        .iter()
        .filter(|parameter| {
            matches!(
                parameter.kind,
                ParamKind::Normal | ParamKind::PositionalOnly
            )
        })
        .collect();
    if positional > ordinary.len()
        && !signature
            .params
            .iter()
            .any(|parameter| parameter.kind == ParamKind::VarArgs)
    {
        return false;
    }
    let mut supplied = FxHashSet::default();
    for parameter in ordinary.iter().take(positional) {
        supplied.insert(parameter.name.clone());
    }
    for (name, _) in keywords {
        if !supplied.insert(name.clone()) {
            return false;
        }
        if !signature.params.iter().any(|parameter| {
            parameter.name == *name
                && matches!(parameter.kind, ParamKind::Normal | ParamKind::KeywordOnly)
        }) && !signature
            .params
            .iter()
            .any(|parameter| parameter.kind == ParamKind::KwArgs)
        {
            return false;
        }
    }
    signature.params.iter().all(|parameter| {
        parameter.has_default
            || matches!(parameter.kind, ParamKind::VarArgs | ParamKind::KwArgs)
            || supplied.contains(&parameter.name)
    })
}

fn retired_target(target: ast::Expr, wanted: &str, budget: &mut usize, depth: usize) -> bool {
    if *budget == 0 || depth >= 32 {
        return true;
    }
    *budget -= 1;
    match target {
        ast::Expr::Attribute(attribute) => attribute.attr_text() == wanted,
        ast::Expr::Tuple(tuple) => tuple
            .elements()
            .any(|target| retired_target(target, wanted, budget, depth + 1)),
        ast::Expr::List(list) => list
            .elements()
            .any(|target| retired_target(target, wanted, budget, depth + 1)),
        ast::Expr::Paren(paren) => paren
            .inner()
            .is_none_or(|target| retired_target(target, wanted, budget, depth + 1)),
        ast::Expr::Star(star) => star
            .expr()
            .is_none_or(|target| retired_target(target, wanted, budget, depth + 1)),
        _ => false,
    }
}

fn written_targets(node: SyntaxNode, budget: usize) -> Option<Vec<ast::Expr>> {
    let targets = match ast::Stmt::cast(node) {
        Some(ast::Stmt::Assign(assignment)) => {
            let mut expressions: Vec<_> = assignment
                .syntax()
                .children()
                .filter_map(ast::Expr::cast)
                .take(budget.saturating_add(2))
                .collect();
            if expressions.len() > budget.saturating_add(1) {
                return None;
            }
            expressions.pop();
            expressions
        }
        Some(ast::Stmt::AnnAssign(assignment)) if assignment.value().is_some() => {
            assignment.target().into_iter().collect()
        }
        Some(ast::Stmt::AugAssign(assignment)) => assignment.target().into_iter().collect(),
        Some(ast::Stmt::Del(deletion)) => deletion
            .syntax()
            .children()
            .find_map(ast::Expr::cast)
            .into_iter()
            .collect(),
        _ => Vec::new(),
    };
    Some(targets)
}

fn projection_target_write(
    target: ast::Expr,
    field: &str,
    budget: &mut usize,
    depth: usize,
) -> bool {
    if *budget == 0 || depth >= 32 {
        return true;
    }
    *budget -= 1;
    match target {
        ast::Expr::Attribute(attribute) => attribute.attr_text() == field,
        ast::Expr::Subscript(_) => true,
        ast::Expr::Tuple(tuple) => tuple
            .elements()
            .any(|target| projection_target_write(target, field, budget, depth + 1)),
        ast::Expr::List(list) => list
            .elements()
            .any(|target| projection_target_write(target, field, budget, depth + 1)),
        ast::Expr::Paren(paren) => paren
            .inner()
            .is_none_or(|target| projection_target_write(target, field, budget, depth + 1)),
        ast::Expr::Star(star) => star
            .expr()
            .is_none_or(|target| projection_target_write(target, field, budget, depth + 1)),
        _ => false,
    }
}

fn subject_capture(expression: &ast::Expr, name: &str) -> Option<ast::Expr> {
    let expression = expression.clone().unparen();
    if is_subject(&expression, name) {
        return Some(expression);
    }
    match expression {
        ast::Expr::Call(call) => call
            .arg_list()?
            .positional()
            .next()
            .filter(|argument| is_subject(argument, name)),
        ast::Expr::Attribute(attribute) => {
            attribute.object().filter(|object| is_subject(object, name))
        }
        ast::Expr::Subscript(subscript) => {
            subscript.object().filter(|object| is_subject(object, name))
        }
        _ => None,
    }
}
