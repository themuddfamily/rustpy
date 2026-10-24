//! Static typing factories. Synthetic class identity is the physical factory
//! call's file/range, not its display name or its runtime initializer value.
use super::*;
use unicode_normalization::UnicodeNormalization;

#[derive(Clone, Copy, PartialEq, Eq)]
enum FactoryKind {
    NewType,
    NamedTuple,
    CollectionsTuple,
    /// `Enum("Color", "RED GREEN")`; members come from enum authority.
    Enum,
    /// `TypedDict("TD", {"key": int}, total=False)`.
    TypedDict,
}
struct FactoryField {
    name: SmolStr,
    range: TextRange,
    annotation: Option<TextRange>,
    default: Option<SmolStr>,
}
struct FactoryModel {
    file: Arc<FileInfo>,
    class: ClassRef,
    kind: FactoryKind,
    /// The NewType supertype, or the enum base class.
    base: Option<Type>,
    fields: Vec<FactoryField>,
    scope: crate::scope::ScopeId,
    total: bool,
}

fn factory_string(expression: &ast::Expr) -> Option<String> {
    if expression.text_range().len() > TextSize::from(4096u32) {
        return None;
    }
    match expression.clone().unparen() {
        ast::Expr::String(value) if !value.is_bytes() => Some(value.value()),
        ast::Expr::ConcatString(value) => {
            let mut text = String::new();
            let mut parts = 0;
            for part in value.parts() {
                parts += 1;
                if parts > 128 {
                    return None;
                }
                let ast::Expr::String(part) = part else {
                    return None;
                };
                if part.is_bytes() {
                    return None;
                }
                text.push_str(&part.value());
                if text.len() > 4096 {
                    return None;
                }
            }
            Some(text)
        }
        _ => None,
    }
}
fn factory_identifier(name: &str) -> bool {
    if name.is_empty() || name.len() > 256 {
        return false;
    }
    let parse = rustpy_syntax::parse_expression(name);
    parse.errors().is_empty()
        && parse
            .syntax_node()
            .descendants()
            .any(|node| ast::NameExpr::cast(node).is_some_and(|node| node.name_text() == name))
}
fn factory_elements(expression: &ast::Expr) -> Option<Vec<ast::Expr>> {
    let values: Vec<_> = match expression.clone().unparen() {
        ast::Expr::Tuple(tuple) => tuple.elements().take(129).collect(),
        ast::Expr::List(list) => list.elements().take(129).collect(),
        _ => return None,
    };
    (values.len() <= 128).then_some(values)
}

impl Inferer<'_> {
    fn factory_kind(
        &self,
        file: &Arc<FileInfo>,
        call: &ast::CallExpr,
        callee: &Type,
    ) -> Option<FactoryKind> {
        if file.text.len() > 4 * 1024 * 1024 || file.scopes.bindings.len() > 16_384 {
            return None;
        }
        let typed_dict = call
            .callee()
            .and_then(|callee| self.symbol_origin(file, &callee))
            .is_some_and(|(module, name)| {
                name == "TypedDict" && matches!(module.as_str(), "typing" | "typing_extensions")
            });
        let (owner, name) = match callee {
            _ if typed_dict => (self.db.module("typing")?, "TypedDict"),
            Type::Class(class, _) => (self.db.file(class.file)?, class.name.as_str()),
            Type::Function(signature) | Type::Callable(signature) => {
                let definition = signature.def.as_ref()?;
                (self.db.file(definition.file)?, definition.name.as_str())
            }
            _ => return None,
        };
        let kind = match (owner.path.to_str()?, owner.module_name.as_deref(), name) {
            ("<typeshed>/typing.pyi", Some("typing"), "NewType")
            | ("<typeshed>/typing_extensions.pyi", Some("typing_extensions"), "NewType") => {
                FactoryKind::NewType
            }
            ("<typeshed>/typing.pyi", Some("typing"), "NamedTuple")
            | ("<typeshed>/typing_extensions.pyi", Some("typing_extensions"), "NamedTuple") => {
                FactoryKind::NamedTuple
            }
            ("<typeshed>/collections.pyi", Some("collections"), "namedtuple") => {
                FactoryKind::CollectionsTuple
            }
            (
                "<typeshed>/enum.pyi",
                Some("enum"),
                "Enum" | "IntEnum" | "StrEnum" | "Flag" | "IntFlag",
            ) => FactoryKind::Enum,
            ("<typeshed>/typing.pyi", Some("typing"), "TypedDict") if typed_dict => {
                FactoryKind::TypedDict
            }
            _ => return None,
        };
        if !owner.is_stub || Self::namespace_member_retired(file, name, call.text_range().start()) {
            return None;
        }
        if let Some(ast::Expr::Name(access)) = call.callee().map(ast::Expr::unparen) {
            if file.scopes.bindings.iter().any(|binding| {
                binding.name == access.name_text()
                    && matches!(
                        binding.kind,
                        BindingKind::Global | BindingKind::Nonlocal | BindingKind::Del
                    )
            }) {
                return None;
            }
        }
        Some(kind)
    }

    fn factory_model_at(
        &self,
        file: &Arc<FileInfo>,
        call: &ast::CallExpr,
        kind: FactoryKind,
    ) -> Option<FactoryModel> {
        if call.text_range().len() > TextSize::from(65_536u32) || file.text.len() > 4 * 1024 * 1024
        {
            return None;
        }
        let args = call.arg_list()?;
        let arguments: Vec<_> = args.args().take(132).collect();
        if arguments.len() > 131
            || arguments.iter().any(|argument| {
                matches!(
                    argument,
                    ast::AnyArg::Star(_) | ast::AnyArg::StarStar(_) | ast::AnyArg::Generator(_)
                )
            })
        {
            return None;
        }
        let positional: Vec<_> = args.positional().take(3).collect();
        let name = factory_string(positional.first()?)?;
        if !factory_identifier(&name) {
            return None;
        }
        let class = ClassRef {
            file: file.id,
            range: call.text_range(),
            name: name.into(),
        };
        let scope = self.scope_of(file, call.syntax());
        if kind == FactoryKind::NewType {
            if positional.len() != 2 || args.keywords().next().is_some() {
                return None;
            }
            let base = match self.annotation_type(file, &positional[1]) {
                Type::Instance(class, args) | Type::Class(class, args) => {
                    Type::Instance(class, args)
                }
                _ => return None,
            };
            if base.class_ref() == Some(&class) {
                return None;
            }
            return Some(FactoryModel {
                file: file.clone(),
                class,
                kind,
                base: Some(base),
                fields: Vec::new(),
                scope,
                total: true,
            });
        }
        if kind == FactoryKind::Enum {
            let imports = |from: &FileInfo, id: crate::scope::BindingId| {
                let BindingKind::Import(import) = &from.scopes.binding(id).kind else {
                    return None;
                };
                self.db
                    .module_relative(from, import.level, import.module.trim_start_matches('.'))
            };
            let model = crate::enum_authority::scan(file, &class, &imports)?;
            let base = match self.type_of_expr(file, &call.callee()?) {
                Type::Class(base, _) => Type::Instance(base, Vec::new()),
                _ => return None,
            };
            let range = positional
                .get(1)
                .map_or(call.text_range(), |names| names.text_range());
            return Some(FactoryModel {
                file: file.clone(),
                class,
                kind,
                base: Some(base),
                fields: model
                    .names()
                    .into_iter()
                    .map(|name| FactoryField {
                        name,
                        range,
                        annotation: None,
                        default: None,
                    })
                    .collect(),
                scope,
                total: true,
            });
        }
        let mut fields = Vec::new();
        let mut total = true;
        if kind == FactoryKind::TypedDict {
            let ast::Expr::Dict(schema) = positional.get(1)?.clone().unparen() else {
                return None;
            };
            if positional.len() != 2 || schema.unpacks().next().is_some() {
                return None;
            }
            for keyword in args.keywords() {
                let value = keyword.value()?.unparen();
                match (keyword.name_text().as_str(), &value) {
                    ("total", ast::Expr::Literal(literal))
                        if matches!(
                            literal.kind(),
                            Some(SyntaxKind::TRUE_KW | SyntaxKind::FALSE_KW)
                        ) =>
                    {
                        total = literal.kind() == Some(SyntaxKind::TRUE_KW);
                    }
                    _ => return None,
                }
            }
            for item in schema.items().take(129) {
                let key = item.key()?;
                fields.push(FactoryField {
                    name: factory_string(&key)?.into(),
                    range: key.text_range(),
                    annotation: Some(item.value()?.text_range()),
                    default: None,
                });
            }
        } else if kind == FactoryKind::NamedTuple {
            if positional.len() == 2 {
                if args.keywords().next().is_some() {
                    return None;
                }
                for pair in factory_elements(&positional[1])? {
                    let pair = factory_elements(&pair)?;
                    if pair.len() != 2 {
                        return None;
                    }
                    fields.push(FactoryField {
                        name: factory_string(&pair[0])?.into(),
                        range: pair[0].text_range(),
                        annotation: Some(pair[1].text_range()),
                        default: None,
                    });
                }
            } else if positional.len() == 1 {
                for keyword in args.keywords().take(129) {
                    let value = keyword.value()?;
                    fields.push(FactoryField {
                        name: keyword.name_text().into(),
                        range: keyword.text_range(),
                        annotation: Some(value.text_range()),
                        default: None,
                    });
                }
            } else {
                return None;
            }
        } else {
            if positional.len() != 2 {
                return None;
            }
            let names = if let Some(text) = factory_string(&positional[1]) {
                text.split(|character: char| character == ',' || character.is_whitespace())
                    .filter(|name| !name.is_empty())
                    .map(ToOwned::to_owned)
                    .take(129)
                    .collect::<Vec<_>>()
            } else {
                factory_elements(&positional[1])?
                    .iter()
                    .map(factory_string)
                    .collect::<Option<Vec<_>>>()?
            };
            let mut rename = false;
            let mut defaults = None;
            let mut options = rustc_hash::FxHashSet::default();
            for keyword in args.keywords() {
                let key = keyword.name_text();
                if !options.insert(key.clone()) {
                    return None;
                }
                let value = keyword.value()?;
                match key.as_str() {
                    "rename" => {
                        rename = match value.clone().unparen() {
                            ast::Expr::Literal(literal)
                                if literal.kind() == Some(SyntaxKind::TRUE_KW) =>
                            {
                                true
                            }
                            ast::Expr::Literal(literal)
                                if literal.kind() == Some(SyntaxKind::FALSE_KW) =>
                            {
                                false
                            }
                            _ => return None,
                        }
                    }
                    "defaults" => {
                        defaults = match value.clone().unparen() {
                            ast::Expr::Literal(literal)
                                if literal.kind() == Some(SyntaxKind::NONE_KW) =>
                            {
                                None
                            }
                            _ => Some(factory_elements(&value)?),
                        }
                    }
                    "module"
                        if factory_string(&value).is_some()
                            || matches!(value, ast::Expr::Literal(ref literal) if literal.kind() == Some(SyntaxKind::NONE_KW)) =>
                        {}
                    _ => return None,
                }
            }
            let mut seen = rustc_hash::FxHashSet::default();
            for (index, name) in names.into_iter().enumerate() {
                let normalized: String = name.nfkc().collect();
                let invalid = !factory_identifier(&name)
                    || name.starts_with('_')
                    || seen.contains(&normalized);
                let name = if invalid && rename {
                    format!("_{index}")
                } else if invalid {
                    return None;
                } else {
                    name
                };
                seen.insert(name.nfkc().collect::<String>());
                fields.push(FactoryField {
                    name: name.into(),
                    range: positional[1].text_range(),
                    annotation: None,
                    default: None,
                });
            }
            if let Some(defaults) = defaults {
                if defaults.len() > fields.len() {
                    return None;
                }
                let first = fields.len() - defaults.len();
                for (field, value) in fields[first..].iter_mut().zip(defaults) {
                    let text = value.syntax().text().to_string();
                    field.default = Some(if text.len() <= 160 {
                        text.into()
                    } else {
                        "...".into()
                    });
                }
            }
        }
        if fields.len() > 128 {
            return None;
        }
        let mut names = rustc_hash::FxHashSet::default();
        for field in &fields {
            if (!factory_identifier(&field.name) || field.name.starts_with('_'))
                && kind == FactoryKind::NamedTuple
            {
                return None;
            }
            if !names.insert(field.name.nfkc().collect::<String>()) {
                return None;
            }
        }
        Some(FactoryModel {
            file: file.clone(),
            class,
            kind,
            base: None,
            fields,
            scope,
            total,
        })
    }

    fn factory_field_type(&self, model: &FactoryModel, field: &FactoryField) -> Type {
        field
            .annotation
            .map(|range| self.annotation_type_range(&model.file, range))
            .unwrap_or(Type::Unknown)
    }

    pub(super) fn is_typed_dict_factory(&self, class: &ClassRef) -> bool {
        self.factory_model(class)
            .is_some_and(|model| model.kind == FactoryKind::TypedDict)
    }
    fn factory_model(&self, class: &ClassRef) -> Option<FactoryModel> {
        let file = self.db.file(class.file)?;
        let call = node_with_range(&file.parse.syntax_node(), class.range, |node| {
            node.kind() == SyntaxKind::CALL_EXPR
        })
        .and_then(ast::CallExpr::cast)?;
        let callee = self.type_of_expr(&file, &call.callee()?);
        let kind = self.factory_kind(&file, &call, &callee)?;
        let model = self.factory_model_at(&file, &call, kind)?;
        (model.class == *class).then_some(model)
    }

    pub(super) fn type_factory_binding(
        &self,
        file: &Arc<FileInfo>,
        binding: &crate::scope::Binding,
    ) -> Option<Type> {
        let call = node_with_range(&file.parse.syntax_node(), binding.value?, |node| {
            node.kind() == SyntaxKind::CALL_EXPR
        })
        .and_then(ast::CallExpr::cast)?;
        let callee = self.type_of_expr(file, &call.callee()?);
        self.type_factory_call(file, &call, &callee)
    }
    pub(super) fn type_factory_call(
        &self,
        file: &Arc<FileInfo>,
        call: &ast::CallExpr,
        callee: &Type,
    ) -> Option<Type> {
        if let Some(item) = self.typed_dict_get(file, call) {
            return Some(item);
        }
        let kind = self.factory_kind(file, call, callee)?;
        match self.factory_model_at(file, call, kind) {
            Some(model) => Some(Type::Class(model.class, vec![])),
            // Other enum calls (`Enum(value)`) keep their stub signature.
            None if matches!(kind, FactoryKind::Enum | FactoryKind::TypedDict) => None,
            None => Some(Type::Unknown),
        }
    }
    pub(super) fn type_factory_class_info(&self, class: &ClassRef) -> Option<Arc<ClassInfo>> {
        let model = self.factory_model(class)?;
        let members = model
            .fields
            .iter()
            .map(|field| {
                (
                    field.name.clone(),
                    Member {
                        name: field.name.clone(),
                        kind: if model.kind == FactoryKind::Enum {
                            MemberKind::ClassVar
                        } else {
                            MemberKind::InstanceVar
                        },
                        file: class.file,
                        range: field.range,
                        binding: None,
                        value: None,
                        annotation: field.annotation,
                        is_stub: model.file.is_stub,
                    },
                )
            })
            .collect();
        Some(Arc::new(ClassInfo {
            class_ref: class.clone(),
            scope: model.scope,
            base_exprs: Vec::new(),
            keywords: Vec::new(),
            type_params: Vec::new(),
            decorators: Vec::new(),
            docstring: None,
            members,
            is_protocol: false,
            is_enum_like: model.kind == FactoryKind::Enum,
            is_dataclass: false,
        }))
    }
    pub(super) fn type_factory_bases(
        &self,
        class: &ClassRef,
    ) -> Option<Vec<(ClassRef, Vec<Type>)>> {
        let model = self.factory_model(class)?;
        if let Some(Type::Instance(class, args)) = &model.base {
            Some(vec![(class.clone(), args.clone())])
        } else if model.kind == FactoryKind::TypedDict {
            // The typeshed fallback the class form's mapping methods come from.
            let fallback = self
                .db
                .module("typing")
                .and_then(|typing| self.resolve_in_module(&typing, "_TypedDict", 0))
                .map(|definition| self.type_of_binding(&definition.file, definition.binding));
            Some(match fallback {
                Some(Type::Class(base, _)) => vec![(base, Vec::new())],
                _ => Vec::new(),
            })
        } else {
            Some(vec![(
                self.builtin_class("tuple")?,
                vec![Type::union(
                    model
                        .fields
                        .iter()
                        .map(|field| self.factory_field_type(&model, field))
                        .collect(),
                )],
            )])
        }
    }
    pub(super) fn type_factory_constructor(&self, class: &ClassRef) -> Option<Type> {
        let model = self.factory_model(class)?;
        let params = if model.kind == FactoryKind::Enum {
            vec![Param {
                name: "value".into(),
                kind: ParamKind::PositionalOnly,
                ty: Type::Unknown,
                has_default: false,
                default_text: None,
            }]
        } else if model.kind == FactoryKind::TypedDict {
            model
                .fields
                .iter()
                .map(|field| Param {
                    name: field.name.clone(),
                    kind: ParamKind::KeywordOnly,
                    ty: self.factory_field_type(&model, field),
                    has_default: !model.total,
                    default_text: (!model.total).then(|| "...".into()),
                })
                .collect()
        } else if let Some(base) = &model.base {
            vec![Param {
                name: "x".into(),
                kind: ParamKind::PositionalOnly,
                ty: base.clone(),
                has_default: false,
                default_text: None,
            }]
        } else {
            model
                .fields
                .iter()
                .map(|field| Param {
                    name: field.name.clone(),
                    kind: ParamKind::Normal,
                    ty: self.factory_field_type(&model, field),
                    has_default: field.default.is_some(),
                    default_text: field.default.clone(),
                })
                .collect()
        };
        Some(Type::Callable(Arc::new(Signature {
            params,
            ret: Type::Instance(class.clone(), vec![]),
            is_async: false,
            def: None,
            docstring: None,
            ret_inferred: false,
        })))
    }
    /// Generated `__init__` parameters of NamedTuple / TypedDict factories.
    pub(super) fn type_factory_init_params(&self, class: &ClassRef) -> Option<Vec<Param>> {
        let model = self.factory_model(class)?;
        if matches!(model.kind, FactoryKind::NewType | FactoryKind::Enum) {
            return None;
        }
        Some(
            self.type_factory_constructor(class)?
                .signature()?
                .params
                .clone(),
        )
    }
    pub(super) fn type_factory_construct(&self, class: &ClassRef) -> Option<Type> {
        (self.factory_model(class).is_some() || self.is_enum_class(class))
            .then(|| Type::Instance(class.clone(), vec![]))
    }
    pub(super) fn type_factory_member(&self, value: &Type, name: &str) -> Option<Type> {
        if let Some(ordering) = self.synthesized_ordering_member(value, name) {
            return Some(ordering);
        }
        let class = value.class_ref()?;
        if matches!(name, "_fields" | "__match_args__") && self.class_node(class).is_some() {
            // Class-form NamedTuple / dataclass / attrs metadata, unless a class
            // in the MRO spells it out.
            for owner in self.mro(class).iter().take(32) {
                if self
                    .class_info(owner)
                    .is_some_and(|info| info.members.contains_key(name))
                {
                    return None;
                }
                let Some(spec) = self.synth_spec(owner) else {
                    continue;
                };
                if name == "_fields" && spec.kind != SynthKind::NamedTuple {
                    return None;
                }
                let names = self.synthesized_match_args(owner)?;
                return Some(Type::Tuple(
                    names
                        .into_iter()
                        .map(|name| Type::Literal(LiteralValue::Str(name)))
                        .collect(),
                ));
            }
            return None;
        }
        let model = self.factory_model(class)?;
        if matches!(model.kind, FactoryKind::Enum | FactoryKind::TypedDict) {
            // Enum members resolve through enum authority; TypedDict keys are
            // items, not attributes.
            return None;
        }
        if model.kind == FactoryKind::NewType {
            return (name == "__supertype__" && matches!(value, Type::Class(..)))
                .then(|| match model.base? {
                    Type::Instance(class, args) => Some(Type::Class(class, args)),
                    _ => None,
                })
                .flatten();
        }
        if let Some(field) = model.fields.iter().find(|field| field.name == name) {
            return Some(if matches!(value, Type::Class(..)) {
                Type::Unknown
            } else {
                self.factory_field_type(&model, field)
            });
        }
        let values: Vec<_> = model
            .fields
            .iter()
            .map(|field| self.factory_field_type(&model, field))
            .collect();
        let result = Type::Instance(class.clone(), vec![]);
        let callable = |mut params: Vec<Param>, ret, instance_method: bool| {
            if instance_method && matches!(value, Type::Class(..)) {
                params.insert(
                    0,
                    Param {
                        name: "self".into(),
                        kind: ParamKind::Normal,
                        ty: result.clone(),
                        has_default: false,
                        default_text: None,
                    },
                );
            }
            Type::Callable(Arc::new(Signature {
                params,
                ret,
                is_async: false,
                def: None,
                docstring: None,
                ret_inferred: false,
            }))
        };
        match name {
            "_replace" => Some(callable(
                model
                    .fields
                    .iter()
                    .map(|field| Param {
                        name: field.name.clone(),
                        kind: ParamKind::KeywordOnly,
                        ty: self.factory_field_type(&model, field),
                        has_default: true,
                        default_text: Some("...".into()),
                    })
                    .collect(),
                result.clone(),
                true,
            )),
            "_asdict" => Some(callable(
                Vec::new(),
                Type::Instance(
                    self.builtin_class("dict")?,
                    vec![self.instance_of("str"), Type::union(values)],
                ),
                true,
            )),
            "__getnewargs__" => Some(callable(Vec::new(), Type::Tuple(values), true)),
            "_make" => Some(callable(
                vec![Param {
                    name: "iterable".into(),
                    kind: ParamKind::Normal,
                    ty: Type::Unknown,
                    has_default: false,
                    default_text: None,
                }],
                result.clone(),
                false,
            )),
            "__new__" => {
                let constructor = self.type_factory_constructor(class)?;
                let mut signature = (**constructor.signature()?).clone();
                signature.params.insert(
                    0,
                    Param {
                        name: "_cls".into(),
                        kind: ParamKind::PositionalOnly,
                        ty: Type::Class(class.clone(), vec![]),
                        has_default: false,
                        default_text: None,
                    },
                );
                Some(Type::Callable(Arc::new(signature)))
            }
            "_fields" | "__match_args__" => Some(Type::Tuple(
                model
                    .fields
                    .into_iter()
                    .map(|field| Type::Literal(LiteralValue::Str(field.name)))
                    .collect(),
            )),
            "_field_defaults" => Some(Type::Instance(
                self.builtin_class("dict")?,
                vec![self.instance_of("str"), Type::Unknown],
            )),
            _ => None,
        }
    }
    pub(super) fn type_factory_synth_spec(&self, class: &ClassRef) -> Option<SynthSpec> {
        let model = self.factory_model(class)?;
        if model.kind == FactoryKind::TypedDict {
            return Some(SynthSpec {
                kind: SynthKind::TypedDict,
                init: true,
                kw_only: true,
                total: model.total,
                frozen: false,
                slots: false,
                inheritable: true,
                match_args: false,
                order: false,
            });
        }
        matches!(
            model.kind,
            FactoryKind::NamedTuple | FactoryKind::CollectionsTuple
        )
        .then_some(SynthSpec {
            kind: SynthKind::NamedTuple,
            init: true,
            kw_only: false,
            total: true,
            inheritable: false,
            frozen: true,
            slots: true,
            match_args: true,
            order: false,
        })
    }
    fn factory_tuple(&self, value: &Type) -> Option<Vec<Type>> {
        let Type::Instance(class, _) = value else {
            return None;
        };
        let Some(model) = self.factory_model(class) else {
            // `class P(NamedTuple)`: a tuple of its field types too.
            return self.class_namedtuple_fields(value);
        };
        matches!(
            model.kind,
            FactoryKind::NamedTuple | FactoryKind::CollectionsTuple
        )
        .then(|| {
            model
                .fields
                .iter()
                .map(|field| self.factory_field_type(&model, field))
                .collect()
        })
    }
    pub(super) fn type_factory_tuple_value(&self, value: &Type) -> Option<Type> {
        Some(Type::Tuple(self.factory_tuple(value)?))
    }
    pub(super) fn type_factory_iter(&self, value: &Type) -> Option<Type> {
        if let Type::Class(class, _) = value {
            // Iterating an enum class yields its members.
            return self
                .is_enum_class(class)
                .then(|| Type::Instance(class.clone(), Vec::new()));
        }
        Some(Type::union(self.factory_tuple(value)?))
    }
    pub(super) fn type_factory_compatible(&self, actual: &Type, expected: &Type) -> Option<bool> {
        if matches!(
            actual,
            Type::Union(_) | Type::Unknown | Type::Any | Type::Never
        ) || matches!(expected, Type::Union(_) | Type::Unknown | Type::Any)
        {
            return None;
        }
        if [actual, expected].into_iter().any(|value| {
            value
                .class_ref()
                .and_then(|class| self.factory_model(class))
                .is_some_and(|model| model.kind == FactoryKind::TypedDict)
        }) {
            // Structural TypedDict rules apply, not nominal factory identity.
            return None;
        }
        if let Type::Instance(class, expected_arguments) = expected {
            if self.factory_model(class).is_some() {
                return Some(
                    matches!(actual, Type::Instance(..) | Type::SelfType(_))
                        && actual.class_ref().is_some_and(|actual| {
                            self.mro_with_args(actual, &[])
                                .into_iter()
                                .find(|(base, _)| base == class)
                                .is_some_and(|(_, arguments)| {
                                    self.generic_arguments_compatible(
                                        class,
                                        &arguments,
                                        expected_arguments,
                                    )
                                })
                        }),
                );
            }
        }
        if !matches!(expected, Type::Instance(..) | Type::Tuple(_)) {
            return None;
        }
        if let Type::Instance(class, _) = actual {
            if let Some(model) = self
                .factory_model(class)
                .filter(|model| model.kind == FactoryKind::NewType)
            {
                return Some(self.is_compatible(&model.base?, expected));
            }
            if self.factory_model(class).is_some() {
                if let Type::Instance(expected_class, expected_arguments) = expected {
                    // Real tuple MRO authority, never display-name numeric tower
                    // shortcuts when the generated type happens to be called int.
                    if self
                        .class_info(expected_class)
                        .is_some_and(|info| info.is_protocol)
                    {
                        return self.protocol_compatible(actual, expected);
                    }
                    return Some(
                        self.mro_with_args(class, &[])
                            .into_iter()
                            .find(|(base, _)| base == expected_class)
                            .is_some_and(|(_, arguments)| {
                                self.generic_arguments_compatible(
                                    expected_class,
                                    &arguments,
                                    expected_arguments,
                                )
                            }),
                    );
                }
            }
        }
        if let Type::Tuple(expected) = expected {
            return self.factory_tuple(actual).map(|fields| {
                fields.len() == expected.len()
                    && fields
                        .iter()
                        .zip(expected)
                        .all(|(actual, expected)| self.is_compatible(actual, expected))
            });
        }
        None
    }
}
