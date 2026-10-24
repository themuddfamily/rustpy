//! One-argument builtin type reflection is based on the captured callee.
use super::*;

impl Inferer<'_> {
    pub(super) fn reflected_builtin_type_call(
        &self,
        file: &Arc<FileInfo>,
        call: &ast::CallExpr,
        callee: &Type,
    ) -> Option<Type> {
        let Type::Class(class, _) = callee else {
            return None;
        };
        let owner = self.db.file(class.file)?;
        if class.name != "type"
            || !owner.is_stub
            || owner.path != std::path::Path::new("<typeshed>/builtins.pyi")
        {
            return None;
        }
        let mut source_captures = vec![file.clone(), owner.clone()];
        if let Some(ast::Expr::Attribute(attribute)) = call.callee().map(ast::Expr::unparen) {
            let member = attribute.attr_text();
            let object = self.type_of_expr(file, &attribute.object()?);
            let mut captures = Vec::new();
            let mut nodes = 16_384;
            match &object {
                Type::Module(id) => captures.push(self.db.file(*id)?),
                Type::Class(class, _) | Type::Instance(class, _) => {
                    let owners = self.mro(class);
                    if owners.len() > 32 {
                        return Some(Type::Unknown);
                    }
                    for class in owners.iter() {
                        let owner = self.db.file(class.file)?;
                        let declaration = owner
                            .parse
                            .syntax_node()
                            .token_at_offset(class.range.start())
                            .right_biased()?
                            .parent_ancestors()
                            .take(64)
                            .find_map(ast::ClassDef::cast)?;
                        if declaration.text_range() != class.range
                            || declaration.name_text() != class.name.as_str()
                        {
                            return Some(Type::Unknown);
                        }
                        if !captures
                            .iter()
                            .any(|old: &Arc<FileInfo>| old.id == owner.id)
                        {
                            captures.push(owner);
                        }
                    }
                }
                _ => return Some(Type::Unknown),
            }
            for owner in &captures {
                if !(owner.is_stub && owner.path.starts_with("<typeshed>"))
                    && !super::async_iteration::namespace_slot_intact_with_budget(
                        owner,
                        owner.parse.syntax_node().text_range().end(),
                        &["type", member.as_str()],
                        &mut nodes,
                    )
                {
                    return Some(Type::Unknown);
                }
            }
            if !captures.iter().all(|old| {
                self.db
                    .file(old.id)
                    .is_some_and(|current| Arc::ptr_eq(old, &current))
            }) {
                return Some(Type::Unknown);
            }
            if !super::async_iteration::namespace_slot_intact(
                file,
                call.text_range().start(),
                &["type", member.as_str()],
            ) {
                return Some(Type::Unknown);
            }
            source_captures.extend(captures);
        }
        let arguments: Vec<_> = call.arg_list()?.args().take(3).collect();
        if arguments.len() != 1 {
            return None;
        }
        let ast::AnyArg::Positional(argument) = &arguments[0] else {
            return None;
        };
        let argument = argument.expr()?;
        let result = match self.type_of_expr(file, &argument) {
            Type::Instance(class, args) => Type::Class(class, args),
            Type::Literal(value) => {
                Type::Class(self.builtin_class(literal_class_name(&value))?, Vec::new())
            }
            Type::None => {
                let types = self.db.module("types")?;
                if !types.is_stub || types.path != std::path::Path::new("<typeshed>/types.pyi") {
                    return None;
                }
                source_captures.push(types.clone());
                let binding = types
                    .scopes
                    .bindings_in(types.scopes.module_scope(), "NoneType")
                    .iter()
                    .find(|binding| {
                        matches!(types.scopes.binding(**binding).kind, BindingKind::Class)
                    })
                    .copied()?;
                Type::Class(
                    ClassRef {
                        file: types.id,
                        range: types.scopes.binding(binding).node_range,
                        name: "NoneType".into(),
                    },
                    Vec::new(),
                )
            }
            Type::Tuple(_) => Type::Class(self.builtin_class("tuple")?, Vec::new()),
            _ => self.instance_of("type"),
        };
        if source_captures.iter().all(|captured| {
            self.db
                .file(captured.id)
                .is_some_and(|current| Arc::ptr_eq(captured, &current))
        }) {
            Some(result)
        } else {
            Some(Type::Unknown)
        }
    }
}
