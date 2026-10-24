//! Shared class-call result/signature authority; never treat __init__'s None as the result.
use super::*;

impl Inferer<'_> {
    pub(super) fn construction_metaclass(
        &self,
        class: &ClassRef,
        depth: usize,
    ) -> Result<Option<ClassRef>, ()> {
        if depth >= 32 {
            return Err(());
        }
        let info = self.class_info(class).ok_or(())?;
        let mut winner = None;
        if let Some((_, range)) = info.keywords.iter().find(|(name, _)| name == "metaclass") {
            let file = self.db.file(class.file).ok_or(())?;
            let expr = node_with_range(&file.parse.syntax_node(), *range, |n| {
                ast::Expr::can_cast(n.kind())
            })
            .and_then(ast::Expr::cast)
            .ok_or(())?;
            let Type::Class(meta, _) = self.type_of_expr(&file, &expr) else {
                return Err(());
            };
            winner = Some(meta);
        }
        for (base, _) in self.direct_bases(class) {
            let Some(candidate) = self.construction_metaclass(&base, depth + 1)? else {
                continue;
            };
            match &winner {
                None => winner = Some(candidate),
                Some(current) if self.mro(current).contains(&candidate) => {}
                Some(current) if self.mro(&candidate).contains(current) => winner = Some(candidate),
                Some(_) => return Err(()),
            }
        }
        Ok(winner)
    }

    fn custom_construction_member(
        &self,
        class: &ClassRef,
        args: &[Type],
        name: &str,
        receiver: &Type,
        instance: bool,
        builtin: &str,
    ) -> Option<Type> {
        let builtin = self.builtin_class(builtin);
        for (owner, owner_args) in self.mro_with_args(class, args) {
            if builtin.as_ref() == Some(&owner) {
                break;
            }
            let Some(info) = self.class_info(&owner) else {
                continue;
            };
            let Some(member) = info.members.get(name) else {
                continue;
            };
            let result = self.member_type(&owner, member, instance, receiver)?;
            return Some(
                result.substitute(&|name| self.class_substitution(&owner, &owner_args, name)),
            );
        }
        None
    }

    fn class_dispatch(
        &self,
        class: &ClassRef,
        args: &[Type],
        pos: &[Type],
        kw: &[(SmolStr, Type)],
    ) -> (Type, Option<Type>) {
        let receiver = Type::Class(class.clone(), args.to_vec());
        let meta = match self.construction_metaclass(class, 0) {
            Ok(meta) => meta,
            Err(()) => return (Type::Unknown, None),
        };
        if let Some(meta) = meta {
            if let Some(call) =
                self.custom_construction_member(&meta, &[], "__call__", &receiver, true, "type")
            {
                return (self.call_result(&call, pos, kw), Some(call));
            }
        }
        let default = Type::Instance(class.clone(), args.to_vec());
        if let Some(new) =
            self.custom_construction_member(class, args, "__new__", &receiver, false, "object")
        {
            let result = self.call_result(&new, pos, kw);
            let is_instance = |ty: &Type| match ty {
                Type::Instance(actual, _) | Type::SelfType(actual) => {
                    self.mro(actual).contains(class)
                }
                _ => false,
            };
            // Foreign/unknown/union returns never justify checking an unrelated initializer.
            // Union alternatives retain exact inferred result, using __new__'s shared signature.
            let signature = if is_instance(&result) {
                self.custom_construction_member(class, args, "__init__", &default, true, "object")
                    .or_else(|| Some(new.clone()))
            } else {
                Some(new)
            };
            return (result, signature);
        }
        (
            default,
            self.find_constructor(class).map(|(signature, _)| signature),
        )
    }

    pub(super) fn class_call_result(
        &self,
        class: &ClassRef,
        arguments: &[Type],
        pos: &[Type],
        kw: &[(SmolStr, Type)],
    ) -> Type {
        let key = (class.file, class.range, GuardKind::Constructor);
        if self.frames.borrow().len() >= 64 || !self.enter(key) {
            return Type::Unknown;
        }
        let mut args = arguments.to_vec();
        if args.is_empty() {
            if let Some(info) = self.class_info(class) {
                if !info.type_params.is_empty() {
                    let mut subst = FxHashMap::default();
                    if let Some((constructor, _)) = self.find_constructor(class) {
                        let signatures = match &constructor {
                            Type::Overloaded(sigs) => sigs.clone(),
                            other => other.signature().cloned().into_iter().collect(),
                        };
                        if !signatures.is_empty() {
                            let signature = self.select_overload(&signatures, pos, kw);
                            self.solve_typevars(&signature.params, pos, kw, &mut subst);
                        }
                    }
                    let parameters = self.class_parameter_types(class);
                    self.solve_generic_defaults(&parameters, &mut subst);
                    args = parameters
                        .iter()
                        .map(|parameter| {
                            Self::typevar_substitution_key(parameter)
                                .and_then(|key| subst.get(key))
                                .cloned()
                                .unwrap_or(Type::Unknown)
                        })
                        .collect();
                }
            }
        }
        args = self.complete_class_defaults(class, &args);
        let result = self.class_dispatch(class, &args, pos, kw).0;
        self.leave(key);
        result
    }

    /// Applicable ordinary construction argument checks in runtime order.
    /// A custom metaclass owns dispatch; foreign/unknown/union new returns skip init.
    pub fn construction_checks(
        &self,
        class: &ClassRef,
        args: &[Type],
        pos: &[Type],
        kw: &[(SmolStr, Type)],
    ) -> Vec<Type> {
        let key = (class.file, class.range, GuardKind::Constructor);
        if self.frames.borrow().len() >= 64 || !self.enter(key) {
            return Vec::new();
        }
        let checks = self.construction_checks_inner(class, args, pos, kw);
        self.leave(key);
        checks
    }
    fn construction_checks_inner(
        &self,
        class: &ClassRef,
        args: &[Type],
        pos: &[Type],
        kw: &[(SmolStr, Type)],
    ) -> Vec<Type> {
        let receiver = Type::Class(class.clone(), args.to_vec());
        let meta = match self.construction_metaclass(class, 0) {
            Ok(meta) => meta,
            Err(()) => return Vec::new(),
        };
        if let Some(meta) = meta {
            if let Some(call) =
                self.custom_construction_member(&meta, &[], "__call__", &receiver, true, "type")
            {
                return vec![call];
            }
        }
        // find_constructor walks each class's explicit methods before its
        // synthesized initializer. A selected generated signature therefore
        // precedes inherited factory methods such as NamedTuple.__new__.
        // An explicit custom __new__ before synthesis yields None here and
        // continues through the independent new/init stages below.
        if let Some((signature, _)) = self.generated_constructor(class) {
            return vec![Type::BoundMethod(
                signature,
                Box::new(Type::Instance(class.clone(), args.to_vec())),
            )];
        }
        if let Some(new) =
            self.custom_construction_member(class, args, "__new__", &receiver, false, "object")
        {
            let result = self.call_result(&new, pos, kw);
            let mut checks = vec![new];
            let instance = match &result {
                Type::Instance(actual, _) | Type::SelfType(actual) => {
                    self.mro(actual).contains(class)
                }
                _ => false,
            };
            if instance {
                if let Some(init) = self
                    .custom_construction_member(class, args, "__init__", &result, true, "object")
                {
                    checks.push(init);
                }
            }
            return checks;
        }
        self.find_constructor(class)
            .map(|(signature, _)| vec![signature])
            .unwrap_or_default()
    }

    /// Checkable dispatch signature for these actual arguments. None means unresolved authority.
    pub fn construction_signature(
        &self,
        class: &ClassRef,
        args: &[Type],
        pos: &[Type],
        kw: &[(SmolStr, Type)],
    ) -> Option<Type> {
        let key = (class.file, class.range, GuardKind::Constructor);
        if self.frames.borrow().len() >= 64 || !self.enter(key) {
            return None;
        }
        let signature = self.class_dispatch(class, args, pos, kw).1;
        self.leave(key);
        signature
    }
}
