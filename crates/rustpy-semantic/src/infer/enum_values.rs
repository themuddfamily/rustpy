//! Enum providers share the pure captured model with control-flow matching.
use super::*;
use crate::enum_authority::{self, EnumModel, MemberValue};

impl Inferer<'_> {
    /// Cheap filter before any source scan: an `enum.Enum` MRO or an explicit
    /// metaclass (which may be `EnumMeta` without an enum base).
    fn enum_candidate(&self, class: &ClassRef) -> bool {
        self.mro(class).iter().take(64).any(|owner| {
            owner.name == "Enum"
                && self.db.file(owner.file).is_some_and(|file| {
                    file.is_stub && file.path == std::path::Path::new("<typeshed>/enum.pyi")
                })
        }) || self
            .class_info(class)
            .is_some_and(|info| info.keywords.iter().any(|(key, _)| key == "metaclass"))
    }
    /// A user enum class (class statement or functional call), whose
    /// iteration, `C["NAME"]` and `C(value)` produce members.
    pub(super) fn is_enum_class(&self, class: &ClassRef) -> bool {
        self.enum_candidate(class)
            && self.db.file(class.file).is_some_and(|file| {
                enum_authority::scan(&file, class, &|from, id| {
                    let BindingKind::Import(import) = &from.scopes.binding(id).kind else {
                        return None;
                    };
                    self.db.module_relative(
                        from,
                        import.level,
                        import.module.trim_start_matches('.'),
                    )
                })
                .is_some()
            })
    }
    fn captured_enum_model(&self, class: &ClassRef) -> Option<EnumModel> {
        if !self.enum_candidate(class) {
            return None;
        }
        let root = self.db.file(class.file)?;
        let captures = RefCell::new(vec![root.clone()]);
        let bytes = std::cell::Cell::new(root.text.len());
        let edges = std::cell::Cell::new(0usize);
        let import = |from: &FileInfo, id: BindingId| {
            if edges.get() >= 128 {
                return None;
            }
            edges.set(edges.get() + 1);
            let BindingKind::Import(import) = &from.scopes.binding(id).kind else {
                return None;
            };
            let target = self.db.module_relative(
                from,
                import.level,
                import.module.trim_start_matches('.'),
            )?;
            let mut captured = captures.borrow_mut();
            if !captured.iter().any(|file| file.id == target.id) {
                if captured.len() >= 32
                    || bytes.get().saturating_add(target.text.len()) > 4 * 1024 * 1024
                {
                    return None;
                }
                bytes.set(bytes.get() + target.text.len());
                captured.push(target.clone());
            }
            Some(target)
        };
        let model = enum_authority::model(&root, class, &import)?;
        let query = self.enum_query_file.borrow().clone();
        if let Some(query) = query {
            if !enum_authority::namespace_intact(&query, &model) {
                return None;
            }
            if !captures.borrow().iter().any(|old| old.id == query.id) {
                if captures.borrow().len() >= 32
                    || bytes.get().saturating_add(query.text.len()) > 4 * 1024 * 1024
                {
                    return None;
                }
                captures.borrow_mut().push(query);
            }
        }
        let current = captures.borrow().iter().all(|file| {
            self.db
                .file(file.id)
                .is_some_and(|current| Arc::ptr_eq(file, &current))
        });
        current.then_some(model)
    }
    pub fn enum_member_value(&self, class: &ClassRef, name: &str) -> Option<Type> {
        self.captured_enum_model(class)?
            .member(name)
            .map(Type::Literal)
    }
    pub(super) fn enum_value_attribute(
        &self,
        class: &ClassRef,
        member: &str,
        name: &str,
    ) -> Option<Type> {
        let model = self.captured_enum_model(class)?;
        match name {
            "value" | "_value_" => match model.value(member)? {
                MemberValue::Literal(value) => Some(Type::Literal(value)),
                MemberValue::Expression(range) => {
                    let file = self.db.file(class.file)?;
                    let expression = node_with_range(&file.parse.syntax_node(), range, |node| {
                        ast::Expr::can_cast(node.kind())
                    })
                    .and_then(ast::Expr::cast)?;
                    let value = self.type_of_expr(&file, &expression);
                    // A data type mixin converts the initializer.
                    Some(match model.value_class() {
                        Some(builtin)
                            if !self.is_compatible(&value, &self.instance_of(builtin)) =>
                        {
                            self.instance_of(builtin)
                        }
                        _ => value,
                    })
                }
                MemberValue::Int => Some(self.instance_of("int")),
                MemberValue::Str => Some(self.instance_of("str")),
                MemberValue::Unknown => None,
            },
            "name" | "_name_" => Some(Type::Literal(LiteralValue::Str(member.into()))),
            _ => None,
        }
    }
    pub(super) fn enum_primitive_value(
        &self,
        class: &ClassRef,
        member: &str,
    ) -> Option<LiteralValue> {
        self.captured_enum_model(class)?.primitive(member)
    }
    pub fn enum_member_values(&self, class: &ClassRef) -> Option<Vec<Type>> {
        let model = self.captured_enum_model(class)?;
        if model.is_flag() {
            return None;
        }
        Some(model.values().into_iter().map(Type::Literal).collect())
    }
    pub fn enum_pattern_equal(
        &self,
        left: &LiteralValue,
        right: &LiteralValue,
        singleton: bool,
    ) -> Option<bool> {
        enum_authority::pattern_equal(left, right, singleton, &|class| {
            self.captured_enum_model(class)
        })
    }
    /// Class attribute `name` of an enum class: members are the enum type
    /// (a literal member while the layout is authoritative), `nonmember()`
    /// values their wrapped type, and every other name a plain attribute.
    pub(super) fn enum_attribute(&self, class: &ClassRef, name: &str) -> Option<Type> {
        if !self.enum_candidate(class) {
            return None;
        }
        let file = self.db.file(class.file)?;
        let scan = enum_authority::scan(&file, class, &|from, id| {
            let BindingKind::Import(import) = &from.scopes.binding(id).kind else {
                return None;
            };
            self.db
                .module_relative(from, import.level, import.module.trim_start_matches('.'))
        })?;
        if scan.is_member(name) {
            return Some(
                self.enum_member_value(class, name)
                    .unwrap_or_else(|| Type::Instance(class.clone(), Vec::new())),
            );
        }
        let range = scan.nonmember(name)??;
        let expression = node_with_range(&file.parse.syntax_node(), range, |node| {
            ast::Expr::can_cast(node.kind())
        })
        .and_then(ast::Expr::cast)?;
        Some(self.type_of_expr(&file, &expression))
    }
    pub(super) fn flag_binary_result(
        &self,
        op: SyntaxKind,
        left: &Type,
        right: &Type,
    ) -> Option<Type> {
        if !matches!(op, SyntaxKind::PIPE | SyntaxKind::AMP | SyntaxKind::CARET) {
            return None;
        }
        let (class, model) = [left, right]
            .into_iter()
            .filter_map(|value| match value {
                Type::Literal(LiteralValue::EnumMember { class, .. })
                | Type::Instance(class, _) => Some(class),
                _ => None,
            })
            .find_map(|class| {
                self.captured_enum_model(class)
                    .filter(|model| model.is_flag())
                    .map(|model| (class, model))
            })?;
        let number = |value: &Type| -> Option<i64> {
            match value {
                Type::Literal(LiteralValue::EnumMember {
                    class: other,
                    member,
                }) if other == class => match model.underlying(member)? {
                    LiteralValue::Int(value) => Some(value),
                    LiteralValue::Bool(value) => Some(i64::from(value)),
                    _ => None,
                },
                Type::Literal(LiteralValue::Int(value)) if model.is_int_flag() => Some(*value),
                Type::Literal(LiteralValue::Bool(value)) if model.is_int_flag() => {
                    Some(i64::from(*value))
                }
                _ => None,
            }
        };
        let (Some(a), Some(b)) = (number(left), number(right)) else {
            let valid = |value: &Type| {
                matches!(value,Type::Instance(other,_) if other==class)
                    || matches!(value,Type::Literal(LiteralValue::EnumMember{class:other,..}) if other==class)
                    || (model.is_int_flag() && self.is_compatible(value, &self.instance_of("int")))
            };
            return Some(if valid(left) && valid(right) {
                Type::Instance(class.clone(), Vec::new())
            } else {
                Type::Unknown
            });
        };
        let value = match op {
            SyntaxKind::PIPE => a | b,
            SyntaxKind::AMP => a & b,
            _ => a ^ b,
        };
        for atom in model.values() {
            if let LiteralValue::EnumMember { member, .. } = &atom {
                if model.underlying(member) == Some(LiteralValue::Int(value)) {
                    return Some(Type::Literal(atom));
                }
            }
        }
        Some(Type::Instance(class.clone(), Vec::new()))
    }

    pub(super) fn flag_invert_result(&self, value: &Type) -> Option<Type> {
        let class = match value {
            Type::Instance(class, _) | Type::Literal(LiteralValue::EnumMember { class, .. }) => {
                class
            }
            _ => return None,
        };
        let model = self.captured_enum_model(class)?;
        model
            .is_flag()
            .then(|| Type::Instance(class.clone(), Vec::new()))
    }
}
