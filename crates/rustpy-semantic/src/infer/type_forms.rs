//! `type[X]` class-object forms, generic type-alias specialisation and the
//! real `collections` classes behind the `typing` aliases (`Counter`, ...).
use super::*;

/// `typing` alias names and the `collections` classes they stand for.
const COLLECTION_ALIASES: &[(&str, &str)] = &[
    ("Counter", "Counter"),
    ("OrderedDict", "OrderedDict"),
    ("ChainMap", "ChainMap"),
    ("DefaultDict", "defaultdict"),
    ("Deque", "deque"),
];

impl Inferer<'_> {
    pub(super) fn module_class(&self, module: &str, name: &str) -> Option<ClassRef> {
        let m = self.db.module(module)?;
        let scope = m.scopes.module_scope();
        let id = *m
            .scopes
            .bindings_in(scope, name)
            .iter()
            .find(|id| matches!(m.scopes.binding(**id).kind, BindingKind::Class))?;
        Some(ClassRef {
            file: m.id,
            range: m.scopes.binding(id).node_range,
            name: SmolStr::new(name),
        })
    }

    /// `typing.Counter[str]` / `DefaultDict[K, V]` / `Deque[T]` (and their
    /// `collections` spellings): an instance of the real `collections` class.
    pub(super) fn collection_alias_instance(&self, special: &str, args: &[Type]) -> Option<Type> {
        let (_, class) = COLLECTION_ALIASES
            .iter()
            .find(|(alias, _)| *alias == special)?;
        let class = self.module_class("collections", class)?;
        let args = self.complete_class_defaults(&class, args);
        Some(Type::Instance(class, args))
    }

    fn is_typing_any_class(&self, class: &ClassRef) -> bool {
        class.name == "Any" && self.module_class("typing", "Any").as_ref() == Some(class)
    }

    pub(super) fn is_builtin_type_class(&self, class: &ClassRef) -> bool {
        class.name == "type" && self.builtin_class("type").as_ref() == Some(class)
    }

    /// `type[X]`: the class object of `X`, distributed over unions. Type
    /// variables and `Self` stay `type[T]` (an instance of `type` carrying the
    /// variable) until solved. `None` when `X` has no class-object form.
    pub(super) fn class_object_form(&self, argument: &Type) -> Option<Type> {
        match argument {
            Type::Instance(class, args) => Some(Type::Class(class.clone(), args.clone())),
            Type::None => self
                .module_class("types", "NoneType")
                .map(|class| Type::Class(class, Vec::new())),
            Type::TypeVar(_) | Type::DeclaredTypeVar(_) | Type::SelfType(_) => Some(
                Type::Instance(self.builtin_class("type")?, vec![argument.clone()]),
            ),
            Type::Union(members) => members
                .iter()
                .map(|member| self.class_object_form(member))
                .collect::<Option<Vec<_>>>()
                .map(Type::union),
            _ => None,
        }
    }

    /// The instances a class-object value constructs (`type[A] | type[B]` →
    /// `A | B`, `type[T]` → `T`).
    fn constructed_by_class_object(&self, value: &Type) -> Option<Type> {
        match value {
            // `typing.Any` is a class in typeshed but a special form as a
            // type: `cast(Any, x)` binds `type[_T]` to `Any`, not an instance.
            Type::Class(class, _) if self.is_typing_any_class(class) => Some(Type::Any),
            Type::Class(class, args) => Some(Type::Instance(class.clone(), args.clone())),
            Type::Instance(class, args) if args.len() == 1 && self.is_builtin_type_class(class) => {
                Some(args[0].clone())
            }
            Type::Union(members) => members
                .iter()
                .map(|member| self.constructed_by_class_object(member))
                .collect::<Option<Vec<_>>>()
                .map(Type::union),
            _ => None,
        }
    }

    /// Calling a `type[T]` value constructs a `T`.
    pub(super) fn type_variable_class_call(&self, callee: &Type) -> Option<Type> {
        match callee {
            Type::Instance(class, args)
                if args.len() == 1
                    && matches!(
                        args[0],
                        Type::TypeVar(_) | Type::DeclaredTypeVar(_) | Type::SelfType(_)
                    )
                    && self.is_builtin_type_class(class) =>
            {
                Some(args[0].clone())
            }
            _ => None,
        }
    }

    /// Unifies a `type[P]` parameter with a class-object argument (`A`,
    /// `type[A] | type[B]`). Returns false when the pair is not of that shape.
    pub(super) fn unify_class_object(
        &self,
        parameter: &Type,
        argument: &Type,
        subst: &mut FxHashMap<SmolStr, Type>,
    ) -> bool {
        let Type::Instance(class, args) = parameter else {
            return false;
        };
        if args.len() != 1
            || !matches!(argument, Type::Class(..) | Type::Union(_))
            || !self.is_builtin_type_class(class)
        {
            return false;
        }
        match self.constructed_by_class_object(argument) {
            Some(instance) => {
                self.unify(&args[0], &instance, subst);
                true
            }
            None => false,
        }
    }

    /// Re-forms `type[T]` once `T` was substituted by a class (or a union of
    /// classes): `type[A]` is the class object `A`.
    pub(super) fn normalize_class_objects(&self, ty: Type) -> Type {
        match ty {
            Type::Instance(class, args) if args.len() == 1 && class.name == "type" => {
                if self.is_builtin_type_class(&class) {
                    if let Some(form) = self.class_object_form(&args[0]) {
                        return form;
                    }
                }
                Type::Instance(class, args)
            }
            Type::Union(members)
                if members
                    .iter()
                    .any(|m| matches!(m, Type::Instance(c, _) if c.name == "type")) =>
            {
                Type::union(
                    members
                        .into_iter()
                        .map(|member| self.normalize_class_objects(member))
                        .collect(),
                )
            }
            other => other,
        }
    }

    /// A type alias's target together with the alias's own type parameters:
    /// the PEP 695 `type X[T] = ...` parameter list, or the free type
    /// variables of an old-style alias in order of first appearance.
    pub(super) fn generic_alias(
        &self,
        file: &Arc<FileInfo>,
        expr: &ast::Expr,
    ) -> Option<(Type, Vec<Type>)> {
        let def = match expr {
            ast::Expr::Name(n) => {
                let scope = self.scope_of(file, n.syntax());
                self.follow_imports(self.resolve_name(file, scope, &n.name_text())?)
            }
            ast::Expr::Attribute(a) => {
                let obj = a.object()?;
                let Type::Module(fid) = self.type_of_expr(file, &obj) else {
                    return None;
                };
                let m = self.db.file(fid)?;
                self.follow_imports(self.resolve_in_module(&m, &a.attr_text(), 0)?)
            }
            _ => return None,
        };
        let b = def.binding().clone();
        if !matches!(b.kind, BindingKind::Assignment | BindingKind::TypeAlias) {
            return None;
        }
        let v = b.value?;
        // Only treat the value as a type expression when it looks like one.
        let root = def.file.parse.syntax_node();
        let value = node_with_range(&root, v, |a| ast::Expr::can_cast(a.kind()))
            .and_then(ast::Expr::cast)?;
        let looks_like_type = matches!(b.kind, BindingKind::TypeAlias)
            || match &value {
                ast::Expr::Subscript(_) | ast::Expr::String(_) => true,
                ast::Expr::Binary(bin) => bin.op().map(|t| t.kind()) == Some(SyntaxKind::PIPE),
                ast::Expr::Name(_) | ast::Expr::Attribute(_) => {
                    matches!(self.type_of_expr(&def.file, &value), Type::Class(..))
                }
                ast::Expr::Literal(l) => l.kind() == Some(SyntaxKind::NONE_KW),
                _ => false,
            };
        if !looks_like_type {
            return None;
        }
        let target = self.annotation_type_range(&def.file, v);
        if target.is_unknown() {
            return None;
        }
        let parameters = if matches!(b.kind, BindingKind::TypeAlias) {
            self.alias_type_parameters(&def.file, b.node_range)
        } else {
            let mut free = Vec::new();
            collect_free_type_variables(&target, &mut free, 0);
            free
        };
        Some((target, parameters))
    }

    /// The declared parameters of `type X[T, U = int] = ...`.
    fn alias_type_parameters(&self, file: &Arc<FileInfo>, statement: TextRange) -> Vec<Type> {
        let root = file.parse.syntax_node();
        let Some(list) = node_with_range(&root, statement, |n| {
            n.children()
                .any(|c| c.kind() == SyntaxKind::TYPE_PARAM_LIST)
        })
        .and_then(|n| {
            n.children()
                .find(|c| c.kind() == SyntaxKind::TYPE_PARAM_LIST)
        }) else {
            return Vec::new();
        };
        let Some(scope) = file.scopes.scope_for_node(list.text_range()) else {
            return Vec::new();
        };
        list.children()
            .take(32)
            .filter_map(|parameter| {
                let name = ast::token(&parameter, SyntaxKind::IDENT)?;
                let ids = file.scopes.bindings_in(scope, name.text());
                let id = ids
                    .iter()
                    .find(|id| file.scopes.binding(**id).node_range == parameter.text_range())?;
                Some(self.type_of_binding(file, *id))
            })
            .collect()
    }

    /// Substitutes a generic alias's own parameters by `args`; parameters
    /// without an argument take their PEP 696 default, else `Any`.
    pub(super) fn specialize_alias(
        &self,
        target: &Type,
        parameters: &[Type],
        args: &[Type],
    ) -> Type {
        if parameters.is_empty() {
            return target.clone();
        }
        let mut subst: FxHashMap<SmolStr, Type> = FxHashMap::default();
        for (parameter, argument) in parameters.iter().zip(args) {
            if let Type::TypeVar(_) | Type::DeclaredTypeVar(_) = parameter {
                if let Some(key) = Self::typevar_substitution_key(parameter) {
                    subst.insert(SmolStr::new(key), argument.clone());
                }
            }
        }
        self.solve_generic_defaults(parameters, &mut subst);
        let own: Vec<&str> = parameters
            .iter()
            .filter_map(Self::typevar_substitution_key)
            .collect();
        let result = target.substitute(&|key| match subst.get(key) {
            Some(Type::Unknown) | None if own.contains(&key) => Some(Type::Any),
            found => found.cloned(),
        });
        self.normalize_class_objects(result)
    }
}

/// Free scalar type variables of an alias target, in order of appearance.
fn collect_free_type_variables(ty: &Type, out: &mut Vec<Type>, depth: u8) {
    if depth >= 16 || out.len() >= 32 {
        return;
    }
    match ty {
        Type::TypeVar(_) | Type::DeclaredTypeVar(_) => {
            if !out.contains(ty) {
                out.push(ty.clone());
            }
        }
        Type::Class(_, types)
        | Type::Instance(_, types)
        | Type::Tuple(types)
        | Type::Union(types) => {
            for ty in types {
                collect_free_type_variables(ty, out, depth + 1);
            }
        }
        Type::Function(signature) | Type::Callable(signature) => {
            for parameter in &signature.params {
                collect_free_type_variables(&parameter.ty, out, depth + 1);
            }
            collect_free_type_variables(&signature.ret, out, depth + 1);
        }
        Type::Unpack(inner) => collect_free_type_variables(inner, out, depth + 1),
        _ => {}
    }
}
