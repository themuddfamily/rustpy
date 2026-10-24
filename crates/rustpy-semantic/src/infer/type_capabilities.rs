//! Literal-derived strings and TypedDict field capabilities retain semantic
//! information that an ordinary value read intentionally unwraps.
use super::*;

struct DictionaryField {
    name: SmolStr,
    value: Type,
    required: bool,
    readonly: bool,
}

fn expr_at(file: &FileInfo, range: TextRange) -> Option<ast::Expr> {
    node_with_range(&file.parse.syntax_node(), range, |node| {
        ast::Expr::can_cast(node.kind())
    })
    .and_then(ast::Expr::cast)
}

impl Inferer<'_> {
    // Runtime results carry literal-derived provenance without exposing an
    // annotation-only restriction as the ordinary inferred string type.
    pub(super) fn inferred_string_result(&self, value: Type) -> Type {
        fn convert(value: Type, remaining: &mut usize, depth: usize) -> Type {
            if *remaining == 0 || depth >= 32 {
                return Type::Unknown;
            }
            *remaining -= 1;
            match value {
                Type::LiteralString => Type::LiteralStringValue,
                Type::Union(values) => Type::union(
                    values
                        .into_iter()
                        .map(|value| convert(value, remaining, depth + 1))
                        .collect(),
                ),
                Type::Tuple(values) => Type::Tuple(
                    values
                        .into_iter()
                        .map(|value| convert(value, remaining, depth + 1))
                        .collect(),
                ),
                Type::Instance(class, values) => Type::Instance(
                    class,
                    values
                        .into_iter()
                        .map(|value| convert(value, remaining, depth + 1))
                        .collect(),
                ),
                // Returned callable signatures retain their declared contracts.
                other => other,
            }
        }
        convert(value, &mut 2048, 0)
    }
    pub(super) fn simplify_inferred_string_unions(
        &self,
        value: Type,
        remaining: &mut usize,
        depth: usize,
    ) -> Type {
        if *remaining == 0 || depth >= 32 {
            return value;
        }
        *remaining -= 1;
        match value {
            Type::Union(values) => {
                let ordinary = values.iter().any(|value| {
                    matches!(value,
                    Type::Instance(class, arguments) if arguments.is_empty()
                        && self.builtin_class("str").as_ref() == Some(class))
                });
                Type::union(
                    values
                        .into_iter()
                        .filter(|value| !ordinary || !matches!(value, Type::LiteralStringValue))
                        .map(|value| {
                            self.simplify_inferred_string_unions(value, remaining, depth + 1)
                        })
                        .collect(),
                )
            }
            Type::Instance(class, values) => Type::Instance(
                class,
                values
                    .into_iter()
                    .map(|value| self.simplify_inferred_string_unions(value, remaining, depth + 1))
                    .collect(),
            ),
            Type::Tuple(values) => Type::Tuple(
                values
                    .into_iter()
                    .map(|value| self.simplify_inferred_string_unions(value, remaining, depth + 1))
                    .collect(),
            ),
            other => other,
        }
    }
    pub fn typed_dict_key(&self, file: &Arc<FileInfo>, expression: &ast::Expr) -> Option<SmolStr> {
        if let Type::Literal(LiteralValue::Str(key)) = self.type_of_expr(file, expression) {
            return (key.len() <= 256).then_some(key);
        }
        let ast::Expr::Name(name) = expression.clone().unparen() else {
            return None;
        };
        let scope = self.scope_of(file, name.syntax());
        let ids = file.scopes.resolve(scope, &name.name_text())?;
        if ids.len() != 1 {
            return None;
        }
        let binding = file.scopes.binding(ids[0]);
        let annotation = expr_at(file, binding.annotation?)?;
        let head = match annotation.unparen() {
            ast::Expr::Subscript(value) => value.object()?,
            other => other,
        };
        if self.scoped_qualifier_name(file, scope, &head).as_deref() != Some("Final") {
            return None;
        }
        let value = expr_at(file, binding.value?)?;
        let Type::Literal(LiteralValue::Str(key)) = self.type_of_expr(file, &value) else {
            return None;
        };
        (key.len() <= 256).then_some(key)
    }
    pub fn typed_dict_unknown_write_error(&self, receiver: &Type, delete: bool) -> Option<String> {
        let Type::Instance(class, arguments) = receiver else {
            return None;
        };
        self.dictionary_fields(class, arguments)?
            .iter()
            .find(|field| field.readonly || (delete && field.required))
            .map(|_| "Unknown key may modify a read-only or required TypedDict item".to_owned())
    }
    pub fn typed_dict_mutating_call_error(
        &self,
        file: &Arc<FileInfo>,
        call: &ast::CallExpr,
    ) -> Option<String> {
        if call.text_range().len() > TextSize::new(4096) {
            return None;
        }
        let ast::Expr::Attribute(attribute) = call.callee()?.unparen() else {
            return None;
        };
        let method = attribute.attr_text();
        if !matches!(
            method.as_str(),
            "update" | "pop" | "setdefault" | "clear" | "popitem" | "__setitem__" | "__delitem__"
        ) {
            return None;
        }
        let receiver = self.type_of_expr(file, &attribute.object()?);
        let Type::Instance(class, arguments) = &receiver else {
            return None;
        };
        let fields = self.dictionary_fields(class, arguments)?;
        if matches!(method.as_str(), "clear" | "popitem") {
            return Some(format!(
                "'{method}' may remove required or read-only TypedDict items"
            ));
        }
        let args = call.arg_list()?;
        if method != "update" {
            let key = args.positional().next()?;
            let Type::Literal(LiteralValue::Str(key)) = self.type_of_expr(file, &key) else {
                return None;
            };
            return self.typed_dict_write_error(
                &receiver,
                &key,
                matches!(method.as_str(), "pop" | "__delitem__"),
            );
        }
        let mut keys = Vec::new();
        let mut unknown_keys = false;
        let arguments: Vec<_> = args.args().take(33).collect();
        if arguments.len() > 32 {
            return None;
        }
        for argument in arguments {
            match argument {
                ast::AnyArg::Keyword(keyword) => {
                    let key = keyword.name_text();
                    if key.len() > 256 {
                        return None;
                    }
                    keys.push(key);
                }
                ast::AnyArg::Positional(argument) => {
                    let value = argument.expr()?;
                    if let ast::Expr::Dict(dictionary) = value.clone().unparen() {
                        if dictionary.unpacks().next().is_some() {
                            unknown_keys = true;
                        }
                        let items: Vec<_> = dictionary.items().take(129).collect();
                        if items.len() > 128 {
                            return None;
                        }
                        for item in items {
                            let Type::Literal(LiteralValue::Str(key)) =
                                self.type_of_expr(file, &item.key()?)
                            else {
                                unknown_keys = true;
                                continue;
                            };
                            if key.len() > 256 {
                                return None;
                            }
                            keys.push(key.to_string());
                        }
                    } else {
                        let ty = self.type_of_expr(file, &value);
                        if matches!(ty, Type::Any | Type::Unknown) {
                            continue;
                        }
                        if let Type::Instance(class, arguments) = ty {
                            if let Some(source) = self.dictionary_fields(&class, &arguments) {
                                for field in source {
                                    if !matches!(field.value, Type::Never) {
                                        keys.push(field.name.to_string());
                                    }
                                }
                            } else {
                                unknown_keys = true;
                            }
                        } else {
                            unknown_keys = true;
                        }
                    }
                }
                _ => unknown_keys = true,
            }
            if keys.len() > 128 {
                return None;
            }
        }
        fields
            .iter()
            .find(|field| {
                field.readonly
                    && (unknown_keys || keys.iter().any(|key| key == field.name.as_str()))
            })
            .map(|field| format!("TypedDict item '{}' is read-only", field.name))
    }
    pub fn typed_dict_literal_compatible(
        &self,
        file: &Arc<FileInfo>,
        expression: &ast::Expr,
        expected: &Type,
    ) -> Option<bool> {
        let Type::Instance(class, arguments) = expected else {
            return None;
        };
        let fields = self.dictionary_fields(class, arguments)?;
        let ast::Expr::Dict(dictionary) = expression.clone().unparen() else {
            return None;
        };
        if dictionary.text_range().len() > TextSize::new(4096)
            || dictionary.unpacks().next().is_some()
        {
            return None;
        }
        let items: Vec<_> = dictionary.items().take(129).collect();
        if items.len() > 128 {
            return None;
        }
        let mut present = Vec::new();
        for item in items {
            let Type::Literal(LiteralValue::Str(key)) = self.type_of_expr(file, &item.key()?)
            else {
                return Some(false);
            };
            if key.len() > 256 {
                return None;
            }
            let Some(field) = fields.iter().find(|field| field.name == key) else {
                return Some(false);
            };
            let value = self.type_of_expr(file, &item.value()?);
            if !self.is_compatible(&value, &field.value) {
                return Some(false);
            }
            present.push(key);
        }
        Some(
            fields
                .iter()
                .all(|field| !field.required || present.contains(&field.name)),
        )
    }
    fn literal_text(&self, value: &Type, remaining: &mut usize) -> bool {
        if *remaining == 0 {
            return false;
        }
        *remaining -= 1;
        match value {
            Type::LiteralString
            | Type::LiteralStringValue
            | Type::Literal(LiteralValue::Str(_)) => true,
            Type::Union(arms) if arms.len() <= 32 => {
                arms.iter().all(|arm| self.literal_text(arm, remaining))
            }
            _ => false,
        }
    }
    pub(super) fn literal_string_call(
        &self,
        file: &Arc<FileInfo>,
        call: &ast::CallExpr,
    ) -> Option<Type> {
        if call.text_range().len() > TextSize::new(4096) {
            return None;
        }
        let ast::Expr::Attribute(attribute) = call.callee()?.unparen() else {
            return None;
        };
        let receiver = self.type_of_expr(file, &attribute.object()?);
        if !self.literal_text(&receiver, &mut 256) {
            return None;
        }
        let method = attribute.attr_text();
        let arguments = call.arg_list()?;
        let args: Vec<_> = arguments.args().take(33).collect();
        if args.len() > 32 {
            return None;
        }
        let mut remaining = 256;
        if method == "join" {
            if args.len() != 1 || !matches!(args[0], ast::AnyArg::Positional(_)) {
                return None;
            }
            let values = self.type_of_expr(file, &args[0].value()?);
            let safe = match &values {
                Type::Tuple(values) if values.len() <= 128 => values
                    .iter()
                    .all(|value| self.literal_text(value, &mut remaining)),
                Type::Instance(class, values)
                    if matches!(class.name.as_str(), "list" | "tuple") && values.len() == 1 =>
                {
                    self.db.file(class.file).is_some_and(|owner| {
                        owner.is_stub
                            && owner.path == std::path::Path::new("<typeshed>/builtins.pyi")
                    }) && self.literal_text(&values[0], &mut remaining)
                }
                _ => false,
            };
            return safe.then_some(Type::LiteralStringValue);
        }
        if !matches!(
            method.as_str(),
            "upper"
                | "lower"
                | "casefold"
                | "capitalize"
                | "title"
                | "swapcase"
                | "strip"
                | "lstrip"
                | "rstrip"
                | "replace"
                | "removeprefix"
                | "removesuffix"
                | "center"
                | "ljust"
                | "rjust"
                | "zfill"
                | "expandtabs"
                | "format"
        ) {
            return None;
        }
        for argument in args {
            if !matches!(
                argument,
                ast::AnyArg::Positional(_) | ast::AnyArg::Keyword(_)
            ) {
                return None;
            }
            let value = self.type_of_expr(file, &argument.value()?);
            if !self.literal_text(&value, &mut remaining)
                && !matches!(value, Type::Literal(LiteralValue::Int(_)))
            {
                return None;
            }
        }
        Some(Type::LiteralStringValue)
    }
    pub(super) fn literal_string_fstring(
        &self,
        file: &Arc<FileInfo>,
        value: &ast::FStringExpr,
    ) -> Option<Type> {
        if value.is_template() || value.text_range().len() > TextSize::new(4096) {
            return None;
        }
        let mut remaining = 256;
        let mut nodes = 0;
        for node in value.syntax().descendants() {
            nodes += 1;
            if nodes > 512 {
                return None;
            }
            if let Some(replacement) = ast::FStringReplacement::cast(node) {
                let ty = self.type_of_expr(file, &replacement.expr()?);
                if !self.literal_text(&ty, &mut remaining) {
                    return None;
                }
            }
        }
        Some(Type::LiteralStringValue)
    }
    pub(super) fn literal_string_assignment(
        &self,
        file: &Arc<FileInfo>,
        binding: &crate::scope::Binding,
        value: Type,
    ) -> Type {
        if file.text.len() > 1024 * 1024
            || file.scopes.bindings.len() > 4096
            || file.scopes.scopes.len() > 4096
            || file.scopes.scopes.iter().any(|scope| {
                scope.globals.iter().any(|name| name == &binding.name)
                    || scope.nonlocals.iter().any(|name| name == &binding.name)
            })
        {
            return self.widen(value);
        }
        match value {
            Type::Literal(LiteralValue::Str(_))
            | Type::LiteralStringValue
            | Type::LiteralString => Type::LiteralStringValue,
            other => self.widen(other),
        }
    }
    pub(super) fn literal_string_compatibility(&self, arg: &Type, param: &Type) -> Option<bool> {
        if matches!(
            arg,
            Type::Union(_) | Type::DeclaredTypeVar(_) | Type::TypeVar(_)
        ) || matches!(
            param,
            Type::Union(_) | Type::DeclaredTypeVar(_) | Type::TypeVar(_)
        ) {
            return None;
        }
        if matches!(param, Type::LiteralStringValue) {
            // A value's provenance is not an annotation restricting callers.
            return Some(self.is_compatible(arg, &self.instance_of("str")));
        }
        let target = matches!(param, Type::LiteralString);
        let source = matches!(arg, Type::LiteralString | Type::LiteralStringValue);
        if target {
            return Some(matches!(
                arg,
                Type::LiteralString
                    | Type::LiteralStringValue
                    | Type::Literal(LiteralValue::Str(_))
                    | Type::Never
                    | Type::Any
                    | Type::Unknown
            ));
        }
        if source {
            return Some(self.is_compatible(&self.instance_of("str"), param));
        }
        None
    }

    pub(super) fn literal_string_binary(
        &self,
        op: SyntaxKind,
        left: &Type,
        right: &Type,
    ) -> Option<Type> {
        let safe = |value: &Type| {
            matches!(
                value,
                Type::LiteralString
                    | Type::LiteralStringValue
                    | Type::Literal(LiteralValue::Str(_))
            )
        };
        if op == SyntaxKind::PLUS && safe(left) && safe(right) {
            return Some(Type::LiteralStringValue);
        }
        if op == SyntaxKind::STAR
            && ((safe(left) && matches!(right, Type::Literal(LiteralValue::Int(_))))
                || (safe(right) && matches!(left, Type::Literal(LiteralValue::Int(_)))))
        {
            return Some(Type::LiteralStringValue);
        }
        None
    }

    fn genuine_dictionary(&self, class: &ClassRef) -> bool {
        let owners = self.mro(class);
        if owners.len() > 32 {
            return false;
        }
        owners.iter().any(|owner| {
            if self.is_typed_dict_factory(owner) {
                return true;
            }
            let Some(file) = self.db.file(owner.file) else {
                return false;
            };
            let Some(info) = self.class_info(owner) else {
                return false;
            };
            info.base_exprs.len() <= 32
                && info.base_exprs.iter().any(|range| {
                    let Some(base) = expr_at(&file, *range) else {
                        return false;
                    };
                    let Some((module, name)) = self.symbol_origin(&file, &base) else {
                        return false;
                    };
                    name == "TypedDict"
                        && matches!(module.as_str(), "typing" | "typing_extensions")
                        && self.db.module(&module).is_some_and(|owner| {
                            owner.is_stub
                                && owner.path.to_string_lossy()
                                    == format!("<typeshed>/{module}.pyi")
                        })
                })
        })
    }
    /// Canonical declared fields for completion and other schema consumers.
    pub fn typed_dict_fields(
        &self,
        class: &ClassRef,
        arguments: &[Type],
    ) -> Option<Vec<(SmolStr, Type, bool, bool)>> {
        Some(
            self.dictionary_fields(class, arguments)?
                .into_iter()
                .map(|field| (field.name, field.value, field.required, field.readonly))
                .collect(),
        )
    }
    fn dictionary_fields(
        &self,
        class: &ClassRef,
        arguments: &[Type],
    ) -> Option<Vec<DictionaryField>> {
        if arguments.len() > 32
            || self.synth_spec(class)?.kind != SynthKind::TypedDict
            || !self.genuine_dictionary(class)
        {
            return None;
        }
        let owners = self.mro(class);
        if owners.is_empty() || owners.len() > 32 {
            return None;
        }
        let mut fields = Vec::new();
        for owner in owners.iter() {
            if !self
                .synth_spec(owner)
                .is_some_and(|spec| spec.kind == SynthKind::TypedDict)
            {
                continue;
            }
            let file = self.db.file(owner.file)?;
            if file.text.len() > 1024 * 1024 {
                return None;
            }
            // Incomplete call sites elsewhere must not erase a valid schema.
            // Only syntax errors intersecting this declaration retire it.
            let root = file.parse.syntax_node();
            let declaration = if self.is_typed_dict_factory(owner) {
                owner.range
            } else {
                root.token_at_offset(owner.range.start())
                    .right_biased()?
                    .parent_ancestors()
                    .take(64)
                    .filter_map(ast::ClassDef::cast)
                    .find(|declaration| {
                        declaration.name_text() == owner.name
                            && declaration.text_range().contains_range(owner.range)
                    })?
                    .text_range()
            };
            if file.parse.errors().iter().any(|error| {
                declaration.contains(error.range.start())
                    || declaration.intersect(error.range).is_some()
            }) {
                return None;
            }
            let info = self.class_info(owner)?;
            if info.members.len() > 128 {
                return None;
            }
            let total = self.synth_spec(owner)?.total;
            for member in info.members.values() {
                if fields
                    .iter()
                    .any(|field: &DictionaryField| field.name == member.name)
                {
                    continue;
                }
                if fields.len() == 128 || member.name.len() > 256 {
                    return None;
                }
                let Some(range) = member.annotation else {
                    continue;
                };
                if range.len() > TextSize::new(4096) {
                    return None;
                }
                let source = self.db.file(member.file)?;
                let expression = expr_at(&source, range)?;
                let mut current = expression.clone();
                let mut required = total;
                let mut readonly = false;
                let mut finished = false;
                for _ in 0..16 {
                    let ast::Expr::Subscript(subscript) = current.clone().unparen() else {
                        finished = true;
                        break;
                    };
                    let head = subscript.object()?;
                    let scope = self.scope_of(&source, head.syntax());
                    let Some(qualifier) = self.scoped_qualifier_name(&source, scope, &head) else {
                        finished = true;
                        break;
                    };
                    match qualifier.as_str() {
                        "ReadOnly" => readonly = true,
                        "Required" => required = true,
                        "NotRequired" => required = false,
                        "Annotated" => {}
                        _ => {
                            finished = true;
                            break;
                        }
                    }
                    let index = subscript.index()?;
                    current = if index.kind() == SyntaxKind::SLICE_LIST {
                        index.children().next().and_then(ast::Expr::cast)?
                    } else {
                        ast::Expr::cast(index)?
                    };
                }
                if !finished {
                    return None;
                }
                fields.push(DictionaryField {
                    name: member.name.clone(),
                    value: self.lookup_member(
                        &Type::Instance(class.clone(), arguments.to_vec()),
                        &member.name,
                    )?,
                    required,
                    readonly,
                });
            }
        }
        Some(fields)
    }

    /// `td["key"]` with a literal key: the declared item type.
    pub(super) fn typed_dict_item(
        &self,
        file: &Arc<FileInfo>,
        receiver: &Type,
        index: &ast::Expr,
    ) -> Option<Type> {
        let key = self.typed_dict_key(file, index)?;
        self.typed_dict_item_type(receiver, &key, 0)
    }
    /// `td.get("key"[, default])`: the item type, or the default when absent.
    pub(super) fn typed_dict_get(
        &self,
        file: &Arc<FileInfo>,
        call: &ast::CallExpr,
    ) -> Option<Type> {
        let ast::Expr::Attribute(attribute) = call.callee()?.unparen() else {
            return None;
        };
        if attribute.attr_text() != "get" {
            return None;
        }
        let arguments = call.arg_list()?;
        let positional: Vec<_> = arguments.positional().take(3).collect();
        if positional.is_empty()
            || positional.len() > 2
            || arguments.args().count() != positional.len()
        {
            return None;
        }
        let receiver = self.type_of_expr(file, &attribute.object()?);
        let item = self.typed_dict_item(file, &receiver, &positional[0])?;
        let missing = match positional.get(1) {
            Some(default) => self.type_of_expr(file, default),
            None => Type::None,
        };
        Some(Type::union(vec![item, missing]))
    }
    fn typed_dict_item_type(&self, receiver: &Type, key: &str, depth: usize) -> Option<Type> {
        match receiver {
            Type::Union(arms) if arms.len() <= 32 && depth < 4 => Some(Type::union(
                arms.iter()
                    .map(|arm| self.typed_dict_item_type(arm, key, depth + 1))
                    .collect::<Option<Vec<_>>>()?,
            )),
            Type::Instance(class, arguments) => self
                .dictionary_fields(class, arguments)?
                .into_iter()
                .find(|field| field.name == key)
                .map(|field| field.value),
            _ => None,
        }
    }
    pub(super) fn typed_dict_required_field(&self, class: &ClassRef, name: &str) -> Option<bool> {
        self.dictionary_fields(class, &[])?
            .iter()
            .find(|field| field.name == name)
            .map(|field| field.required)
    }
    pub fn typed_dict_write_error(
        &self,
        receiver: &Type,
        key: &str,
        delete: bool,
    ) -> Option<String> {
        self.dictionary_write_error(receiver, key, delete, &mut 256, 0)
    }
    fn dictionary_write_error(
        &self,
        receiver: &Type,
        key: &str,
        delete: bool,
        remaining: &mut usize,
        depth: usize,
    ) -> Option<String> {
        if key.len() > 256 || depth >= 16 || *remaining == 0 {
            return None;
        }
        *remaining -= 1;
        if let Type::Union(arms) = receiver {
            if arms.len() > 32 {
                return None;
            }
            return arms.iter().find_map(|arm| {
                self.dictionary_write_error(arm, key, delete, remaining, depth + 1)
            });
        }
        let Type::Instance(class, arguments) = receiver else {
            return None;
        };
        let fields = self.dictionary_fields(class, arguments)?;
        let field = fields.iter().find(|field| field.name == key)?;
        if field.readonly {
            Some(format!("TypedDict item '{key}' is read-only"))
        } else if delete && field.required {
            Some(format!("Required TypedDict item '{key}' cannot be deleted"))
        } else {
            None
        }
    }

    pub(super) fn typed_dict_capability_compatible(
        &self,
        arg: &Type,
        param: &Type,
    ) -> Option<bool> {
        self.dictionary_relation(arg, param, &mut Vec::new(), 0)
    }
    fn dictionary_relation(
        &self,
        arg: &Type,
        param: &Type,
        active: &mut Vec<(ClassRef, ClassRef)>,
        depth: usize,
    ) -> Option<bool> {
        let (
            Type::Instance(actual, actual_arguments),
            Type::Instance(expected, expected_arguments),
        ) = (arg, param)
        else {
            return None;
        };
        if !self
            .synth_spec(expected)
            .is_some_and(|spec| spec.kind == SynthKind::TypedDict)
        {
            return None;
        }
        if arg == param {
            return Some(true);
        }
        if depth >= 16
            || active.len() >= 32
            || active
                .iter()
                .any(|pair| pair == &(actual.clone(), expected.clone()))
        {
            return Some(false);
        }
        let Some(actual_fields) = self.dictionary_fields(actual, actual_arguments) else {
            return Some(false);
        };
        let Some(expected_fields) = self.dictionary_fields(expected, expected_arguments) else {
            return Some(false);
        };
        active.push((actual.clone(), expected.clone()));
        let mut valid = true;
        for target in &expected_fields {
            let Some(source) = actual_fields
                .iter()
                .find(|source| source.name == target.name)
            else {
                let optional_object = target.readonly
                    && !target.required
                    && matches!(&target.value,Type::Instance(class,args) if args.is_empty()
                        && class.name=="object" && self.db.file(class.file).is_some_and(|owner|
                        owner.is_stub&&owner.path==std::path::Path::new("<typeshed>/builtins.pyi")));
                if optional_object {
                    continue;
                }
                valid = false;
                break;
            };
            let forward = self
                .dictionary_relation(&source.value, &target.value, active, depth + 1)
                .unwrap_or_else(|| self.is_compatible(&source.value, &target.value));
            if target.readonly {
                if !forward || (target.required && !source.required) {
                    valid = false;
                    break;
                }
            } else {
                let reverse = self
                    .dictionary_relation(&target.value, &source.value, active, depth + 1)
                    .unwrap_or_else(|| self.is_compatible(&target.value, &source.value));
                if source.readonly || source.required != target.required || !forward || !reverse {
                    valid = false;
                    break;
                }
            }
        }
        active.pop();
        Some(valid)
    }
}
