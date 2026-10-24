//! Generated constructors and result-changing decorators (roadmap §2.2
//! "dataclass/attrs/pydantic-style synthesized `__init__`/fields",
//! "decorators (incl. returning different callables)").
//!
//! `@dataclass`, `typing.NamedTuple`, `TypedDict`, attrs (`@attr.s`,
//! `@define`, …) and PEP 681 `dataclass_transform` classes (pydantic
//! `BaseModel`, SQLModel) get an `__init__` built from their fields, and
//! `@contextmanager` / `@asynccontextmanager` turn a generator function into
//! a context-manager factory.

use super::*;

/// The family of generated `__init__` a class belongs to.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SynthKind {
    /// `@dataclasses.dataclass` (also pydantic dataclasses).
    Dataclass,
    /// attrs: `classic` for `@attr.s` (fields are `attr.ib()` unless
    /// `auto_attribs`), otherwise `@define` / `@frozen` / `@mutable`.
    Attrs {
        classic: bool,
        auto_attribs: Option<bool>,
    },
    /// `class P(NamedTuple)`.
    NamedTuple,
    /// `class Movie(TypedDict)`: keyword-only keys.
    TypedDict,
    /// PEP 681 `dataclass_transform` (pydantic `BaseModel`, SQLModel, …).
    Transform,
}

/// How a class's `__init__` is generated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SynthSpec {
    pub kind: SynthKind,
    /// `False` for `@dataclass(init=False)`: no generated `__init__`, but the
    /// fields still count for subclasses.
    pub init: bool,
    /// Fields are keyword-only by default (`kw_only=True`, pydantic models).
    pub kw_only: bool,
    /// TypedDict totality (`total=False` makes every key optional).
    pub total: bool,
    pub frozen: bool,
    pub slots: bool,
    /// Subclasses are generated the same way (TypedDict, `dataclass_transform`
    /// through a base class or metaclass), unlike decorator-based forms.
    pub inheritable: bool,
    /// A generated `__match_args__` (dataclass/attrs `match_args=True`, the
    /// default, and class-form `NamedTuple`).
    pub match_args: bool,
    /// Generated `__lt__`/`__le__`/`__gt__`/`__ge__` (`@dataclass(order=True)`,
    /// attrs `order`).
    pub order: bool,
}

/// One generated `__init__` parameter.
struct Field {
    name: SmolStr,
    param: Param,
    kw_only: bool,
}

/// What a field specifier call (`field(...)`, `Field(...)`, `attr.ib(...)`) says.
struct FieldSpec {
    init: bool,
    kw_only: Option<bool>,
    default: Option<SmolStr>,
    alias: Option<SmolStr>,
}

fn bool_kwarg(args: Option<&ast::ArgList>, name: &str) -> Option<bool> {
    let k = args?.keywords().find(|k| k.name_text() == name)?;
    match k.value()? {
        ast::Expr::Literal(l) => match l.kind() {
            Some(SyntaxKind::TRUE_KW) => Some(true),
            Some(SyntaxKind::FALSE_KW) => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// `(callee, call arguments)` of a decorator or base expression.
fn split_call(e: ast::Expr) -> (ast::Expr, Option<ast::ArgList>) {
    match &e {
        ast::Expr::Call(c) => match c.callee() {
            Some(callee) => (callee, c.arg_list()),
            None => (e, None),
        },
        _ => (e, None),
    }
}

/// Source text for a default value shown in signatures.
fn default_text(e: &ast::Expr) -> SmolStr {
    let text: String = e
        .syntax()
        .text()
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.chars().count() > 40 {
        SmolStr::new("...")
    } else {
        SmolStr::new(text)
    }
}

impl<'a> Inferer<'a> {
    fn canonical_transform_marker<'d>(
        &self,
        file: &Arc<FileInfo>,
        decorators: impl Iterator<Item = &'d ast::Decorator>,
    ) -> Option<(bool, bool)> {
        for decorator in decorators.take(32) {
            let expression = decorator.expr()?;
            if self.canonical_typing_class_metadata(file, &expression) != Some(true) {
                continue;
            }
            let (callee, args) = split_call(expression);
            if self
                .symbol_origin(file, &callee)
                .is_some_and(|(_, name)| name == "dataclass_transform")
            {
                return Some((
                    bool_kwarg(args.as_ref(), "kw_only_default").unwrap_or(false),
                    bool_kwarg(args.as_ref(), "frozen_default").unwrap_or(false),
                ));
            }
        }
        None
    }
    /// `(defining module, name)` of the symbol an expression refers to,
    /// following imports; for an unresolved import, what the import names.
    pub fn symbol_origin(
        &self,
        file: &Arc<FileInfo>,
        expr: &ast::Expr,
    ) -> Option<(String, String)> {
        let from_def = |def: Definition| -> (String, String) {
            let def = self.follow_imports(def);
            let b = def.binding();
            match &b.kind {
                BindingKind::Import(info) => {
                    let module = info.module.trim_start_matches('.').to_string();
                    match &info.name {
                        Some(n) => (module, n.to_string()),
                        None => (module, String::new()),
                    }
                }
                _ => (
                    def.file.module_name.clone().unwrap_or_default(),
                    b.name.to_string(),
                ),
            }
        };
        match expr {
            ast::Expr::Name(n) => {
                let scope = self.scope_of(file, n.syntax());
                Some(from_def(self.resolve_name(file, scope, &n.name_text())?))
            }
            ast::Expr::Attribute(a) => {
                let obj = a.object()?;
                let attr = a.attr_text();
                if let Type::Module(fid) = self.type_of_expr(file, &obj) {
                    let m = self.db.file(fid)?;
                    return Some(match self.resolve_in_module(&m, &attr, 0) {
                        Some(def) => from_def(def),
                        None => (m.module_name.clone().unwrap_or_default(), attr),
                    });
                }
                // `import attr` that does not resolve: `attr.s`.
                let ast::Expr::Name(n) = &obj else {
                    return None;
                };
                let scope = self.scope_of(file, n.syntax());
                let def = self.resolve_name(file, scope, &n.name_text())?;
                match &def.binding().kind {
                    BindingKind::Import(info) if info.name.is_none() => {
                        Some((info.module.to_string(), attr))
                    }
                    _ => None,
                }
            }
            ast::Expr::Subscript(s) => self.symbol_origin(file, &s.object()?),
            _ => None,
        }
    }

    pub(super) fn class_node(&self, c: &ClassRef) -> Option<(Arc<FileInfo>, ast::ClassDef)> {
        let file = self.db.file(c.file)?;
        let node = node_with_range(&file.parse.syntax_node(), c.range, |n| {
            n.kind() == SyntaxKind::CLASS_DEF
        })
        .and_then(ast::ClassDef::cast)?;
        Some((file, node))
    }

    /// Whether `c` (not counting its subclasses) is marked `@dataclass_transform`
    /// or uses a metaclass that is; `Some(kw_only_default)`.
    fn transform_root(&self, c: &ClassRef) -> Option<(bool, bool)> {
        let (file, class) = self.class_node(c)?;
        let decorators: Vec<ast::Decorator> = class.decorators().collect();
        if let Some(kw) = self.canonical_transform_marker(&file, decorators.iter()) {
            return Some(kw);
        }
        let meta = class
            .arguments()?
            .keywords()
            .find(|k| k.name_text() == "metaclass")?
            .value()?;
        let Type::Class(meta_class, _) = self.type_of_expr(&file, &meta) else {
            return None;
        };
        self.mro(&meta_class).iter().find_map(|m| {
            let (source, node) = self.class_node(m)?;
            let decorators: Vec<ast::Decorator> = node.decorators().collect();
            self.canonical_transform_marker(&source, decorators.iter())
        })
    }

    /// How `c`'s `__init__` is generated, if it is a dataclass-like class.
    pub fn synth_spec(&self, c: &ClassRef) -> Option<SynthSpec> {
        if let Some(hit) = self.synth_cache.borrow().get(c) {
            return hit.clone();
        }
        // Placeholder while computing (guards against cyclic hierarchies).
        self.synth_cache.borrow_mut().insert(c.clone(), None);
        let spec = self.compute_synth_spec(c);
        self.synth_cache
            .borrow_mut()
            .insert(c.clone(), spec.clone());
        spec
    }

    fn compute_synth_spec(&self, c: &ClassRef) -> Option<SynthSpec> {
        if let Some(spec) = self.type_factory_synth_spec(c) {
            return Some(spec);
        }
        let (file, class) = self.class_node(c)?;
        let class_args = class.arguments();
        let spec = |kind, init, kw_only| SynthSpec {
            kind,
            init,
            kw_only,
            total: true,
            frozen: kind == SynthKind::NamedTuple
                || bool_kwarg(class_args.as_ref(), "frozen").unwrap_or(false),
            slots: kind == SynthKind::NamedTuple
                || bool_kwarg(class_args.as_ref(), "slots").unwrap_or(false),
            inheritable: kind == SynthKind::Transform,
            // `dataclass_transform` classes get a generated `__match_args__`
            // unless their class arguments say `match_args=False`.
            match_args: kind == SynthKind::NamedTuple
                || (kind == SynthKind::Transform
                    && bool_kwarg(class_args.as_ref(), "match_args").unwrap_or(true)),
            order: false,
        };
        // Decorators: @dataclass, attrs, or a dataclass_transform decorator function.
        for d in class.decorators() {
            let Some(expr) = d.expr() else { continue };
            let (callee, args) = split_call(expr);
            let init = bool_kwarg(args.as_ref(), "init").unwrap_or(true);
            let kw_only = bool_kwarg(args.as_ref(), "kw_only").unwrap_or(false);
            let origin = self.symbol_origin(&file, &callee);
            let (module, name) = origin
                .clone()
                .unwrap_or_else(|| (String::new(), d.name_text().unwrap_or_default()));
            let is_attrs_module = module == "attr"
                || module == "attrs"
                || module.starts_with("attr.")
                || module.starts_with("attrs.");
            let base_spec = &spec;
            let spec = |kind, init, kw_only| {
                let mut result = base_spec(kind, init, kw_only);
                result.frozen = bool_kwarg(args.as_ref(), "frozen").unwrap_or(name == "frozen");
                result.slots = bool_kwarg(args.as_ref(), "slots").unwrap_or(
                    is_attrs_module && matches!(name.as_str(), "define" | "frozen" | "mutable"),
                );
                result.match_args = matches!(
                    kind,
                    SynthKind::Dataclass | SynthKind::Attrs { .. } | SynthKind::Transform
                ) && bool_kwarg(args.as_ref(), "match_args").unwrap_or(true);
                // dataclasses and `attrs.define` default to `order=False`; classic
                // `@attr.s` orders whenever it compares (`order=None` follows `eq`).
                let eq = bool_kwarg(args.as_ref(), "eq").unwrap_or(true);
                let order_default = matches!(kind, SynthKind::Attrs { classic: true, .. }) && eq;
                result.order = eq && bool_kwarg(args.as_ref(), "order").unwrap_or(order_default);
                result
            };
            match (module.as_str(), name.as_str()) {
                ("dataclasses" | "pydantic.dataclasses", "dataclass") | ("", "dataclass") => {
                    return Some(spec(SynthKind::Dataclass, init, kw_only))
                }
                (_, "s" | "attrs" | "attributes") if is_attrs_module => {
                    let auto_attribs =
                        Some(bool_kwarg(args.as_ref(), "auto_attribs").unwrap_or(false));
                    return Some(spec(
                        SynthKind::Attrs {
                            classic: true,
                            auto_attribs,
                        },
                        init,
                        kw_only,
                    ));
                }
                (_, "dataclass") if is_attrs_module => {
                    return Some(spec(
                        SynthKind::Attrs {
                            classic: true,
                            auto_attribs: Some(true),
                        },
                        init,
                        kw_only,
                    ))
                }
                (_, "define" | "frozen" | "mutable") if is_attrs_module => {
                    let auto_attribs = bool_kwarg(args.as_ref(), "auto_attribs");
                    return Some(spec(
                        SynthKind::Attrs {
                            classic: false,
                            auto_attribs,
                        },
                        init,
                        kw_only,
                    ));
                }
                _ => {}
            }
            // A decorator function marked `@dataclass_transform(...)`.
            let decorator_def = match self.type_of_expr(&file, &callee) {
                Type::Function(s) => s.def.clone(),
                Type::Overloaded(v) => v.first().and_then(|s| s.def.clone()),
                _ => None,
            };
            if let Some(def) = decorator_def {
                let marker = self.db.file(def.file).and_then(|source| {
                    let function =
                        node_with_range(&source.parse.syntax_node(), def.range, |node| {
                            node.kind() == SyntaxKind::FUNC_DEF
                        })
                        .and_then(ast::FuncDef::cast)?;
                    let decorators: Vec<_> = function.decorators().collect();
                    self.canonical_transform_marker(&source, decorators.iter())
                });
                if let Some((kw_default, frozen_default)) = marker {
                    // The decorator form transforms only the decorated class.
                    return Some(SynthSpec {
                        inheritable: false,
                        frozen: bool_kwarg(args.as_ref(), "frozen").unwrap_or(frozen_default),
                        ..spec(
                            SynthKind::Transform,
                            init,
                            bool_kwarg(args.as_ref(), "kw_only").unwrap_or(kw_default),
                        )
                    });
                }
            }
        }
        // NamedTuple / TypedDict bases (special forms, so not part of the MRO).
        let total = bool_kwarg(class_args.as_ref(), "total").unwrap_or(true);
        for base in class.bases() {
            match self.symbol_origin(&file, &base) {
                Some((m, n))
                    if matches!(m.as_str(), "typing" | "typing_extensions")
                        && n == "NamedTuple" =>
                {
                    return Some(spec(SynthKind::NamedTuple, true, false))
                }
                Some((m, n))
                    if matches!(
                        m.as_str(),
                        "typing" | "typing_extensions" | "mypy_extensions"
                    ) && n == "TypedDict" =>
                {
                    return Some(SynthSpec {
                        kind: SynthKind::TypedDict,
                        init: true,
                        kw_only: true,
                        total,
                        frozen: false,
                        slots: false,
                        inheritable: true,
                        match_args: false,
                        order: false,
                    })
                }
                _ => {}
            }
        }
        let mro = self.mro(c);
        for cls in mro.iter().skip(1) {
            if self
                .synth_spec(cls)
                .is_some_and(|s| s.kind == SynthKind::TypedDict)
            {
                return Some(SynthSpec {
                    kind: SynthKind::TypedDict,
                    init: true,
                    kw_only: true,
                    total,
                    frozen: false,
                    slots: false,
                    inheritable: true,
                    match_args: false,
                    order: false,
                });
            }
        }
        // dataclass_transform through a base class or a metaclass (pydantic, SQLModel).
        let init = bool_kwarg(class_args.as_ref(), "init").unwrap_or(true);
        let kw_override = bool_kwarg(class_args.as_ref(), "kw_only");
        for (i, cls) in mro.iter().enumerate() {
            let marker = if i == 0 {
                // The class itself counts only through its metaclass.
                let (source, node) = self.class_node(cls)?;
                let decorators: Vec<ast::Decorator> = node.decorators().collect();
                if self
                    .canonical_transform_marker(&source, decorators.iter())
                    .is_some()
                {
                    None
                } else {
                    self.transform_root(cls)
                }
            } else {
                self.transform_root(cls)
            };
            if let Some((kw_default, frozen_default)) = marker {
                return Some(SynthSpec {
                    frozen: bool_kwarg(class_args.as_ref(), "frozen").unwrap_or(frozen_default),
                    ..spec(
                        SynthKind::Transform,
                        init,
                        kw_override.unwrap_or(kw_default),
                    )
                });
            }
        }
        // Well-known model bases when their package is not installed.
        for base in class.bases() {
            if let Some((m, n)) = self.symbol_origin(&file, &base) {
                let root = m.split('.').next().unwrap_or("");
                if ((root == "pydantic" && matches!(n.as_str(), "BaseModel" | "BaseSettings"))
                    || (root == "pydantic_settings" && n == "BaseSettings"))
                    || (root == "sqlmodel" && n == "SQLModel")
                {
                    return Some(spec(
                        SynthKind::Transform,
                        init,
                        kw_override.unwrap_or(true),
                    ));
                }
            }
        }
        // Subclasses of such models.
        for cls in mro.iter().skip(1) {
            if let Some(parent) = self
                .synth_spec(cls)
                .filter(|s| s.kind == SynthKind::Transform && s.inheritable)
            {
                return Some(spec(
                    SynthKind::Transform,
                    init,
                    kw_override.unwrap_or(parent.kw_only),
                ));
            }
        }
        None
    }

    /// The constructor found by walking the MRO: an explicit `__init__` /
    /// `__new__`, or a generated `__init__` (with its family).
    pub(super) fn find_constructor(&self, c: &ClassRef) -> Option<(Type, Option<SynthKind>)> {
        let receiver = Type::Instance(c.clone(), vec![]);
        let object_file = self
            .builtin_class("object")
            .map(|o| o.file)
            .unwrap_or(FileId(u32::MAX));
        for (cls, cls_args) in self.mro_with_args(c, &[]) {
            if cls.name == "object" && cls.file == object_file {
                break;
            }
            let Some(info) = self.class_info(&cls) else {
                continue;
            };
            let subst = |name: &str| self.class_substitution(&cls, &cls_args, name);
            for name in ["__init__", "__new__"] {
                if let Some(m) = info.members.get(name) {
                    let is_instance = name == "__init__";
                    if let Some(t) = self.member_type(&cls, m, is_instance, &receiver) {
                        return Some((t.substitute(&subst), None));
                    }
                }
            }
            if let Some(spec) = self.synth_spec(&cls) {
                if spec.init {
                    let sig = self.synthesized_init(&cls, &spec);
                    return Some((
                        Type::BoundMethod(sig, Box::new(receiver.clone())).substitute(&subst),
                        Some(spec.kind),
                    ));
                }
            }
        }
        None
    }

    /// The generated `__init__` used to construct `c` (signature without
    /// `self`) and its family, when `c` has no explicit constructor of its own
    /// before the generated one in the MRO.
    pub fn generated_constructor(&self, c: &ClassRef) -> Option<(Arc<Signature>, SynthKind)> {
        match self.find_constructor(c)? {
            (t, Some(kind)) => Some((t.signature()?.clone(), kind)),
            _ => None,
        }
    }

    /// The `__init__` generated for `c` from its fields (and its bases' fields).
    pub fn synthesized_init(&self, c: &ClassRef, spec: &SynthSpec) -> Arc<Signature> {
        let fields = self.synthesized_fields(c, spec);
        let mut params: Vec<Param> = fields
            .iter()
            .filter(|f| !f.kw_only)
            .map(|f| f.param.clone())
            .collect();
        params.extend(fields.into_iter().filter(|f| f.kw_only).map(|f| Param {
            kind: ParamKind::KeywordOnly,
            ..f.param
        }));
        let docstring = self.class_info(c).and_then(|i| i.docstring.clone());
        Arc::new(Signature {
            params,
            ret: Type::None,
            is_async: false,
            def: None,
            docstring,
            ret_inferred: false,
        })
    }

    /// The `__init__` fields of `c` (with its same-family bases' fields), in
    /// declaration order.
    fn synthesized_fields(&self, c: &ClassRef, spec: &SynthSpec) -> Vec<Field> {
        let same_family =
            |a: SynthKind, b: SynthKind| std::mem::discriminant(&a) == std::mem::discriminant(&b);
        let mut fields: Vec<Field> = Vec::new();
        if spec.kind == SynthKind::NamedTuple {
            fields = self.own_fields(c, spec);
        } else {
            // Base classes first; a redefined field keeps its original position
            // (attrs moves it to the end).
            for cls in self.mro(c).iter().rev() {
                let cspec = if cls == c {
                    Some(spec.clone())
                } else {
                    self.synth_spec(cls)
                };
                let Some(cspec) = cspec.filter(|s| same_family(s.kind, spec.kind)) else {
                    continue;
                };
                for f in self.own_fields(cls, &cspec) {
                    match (spec.kind, fields.iter().position(|x| x.name == f.name)) {
                        (SynthKind::Attrs { .. }, Some(i)) => {
                            fields.remove(i);
                            fields.push(f);
                        }
                        (_, Some(i)) => fields[i] = f,
                        (_, None) => fields.push(f),
                    }
                }
            }
        }
        fields
    }

    /// The generated `__match_args__` of `c` itself: positional `__init__`
    /// fields of a dataclass/attrs/`dataclass_transform` class (keyword-only
    /// fields and `KW_ONLY` sections excluded, so pydantic models, which are
    /// keyword-only by default, get `()`), or every class-form `NamedTuple` field.
    pub fn synthesized_match_args(&self, c: &ClassRef) -> Option<Vec<SmolStr>> {
        let spec = self.synth_spec(c).filter(|s| s.match_args)?;
        if !matches!(
            spec.kind,
            SynthKind::Dataclass
                | SynthKind::Attrs { .. }
                | SynthKind::NamedTuple
                | SynthKind::Transform
        ) {
            return None;
        }
        self.class_node(c)?;
        Some(
            self.synthesized_fields(c, &spec)
                .into_iter()
                .filter(|f| !f.kw_only)
                .map(|f| f.name)
                .collect(),
        )
    }

    /// Whether `class` is (a subclass of) a class-form `NamedTuple`.
    pub(crate) fn is_class_namedtuple(&self, class: &ClassRef) -> bool {
        self.mro(class).iter().take(32).any(|owner| {
            self.synth_spec(owner)
                .is_some_and(|s| s.kind == SynthKind::NamedTuple)
                && self.class_node(owner).is_some()
        })
    }

    /// Whether `info`'s only decorator is the dataclass/attrs decorator that
    /// generates its members, which keeps class patterns over it predictable.
    pub(crate) fn synthesized_match_authority(&self, class: &ClassRef, info: &ClassInfo) -> bool {
        info.decorators.len() == 1
            && (self.synth_spec(class).is_some_and(|s| {
                matches!(
                    s.kind,
                    SynthKind::Dataclass | SynthKind::Attrs { .. } | SynthKind::Transform
                )
            }) || self.class_node(class).is_some_and(|(file, node)| {
                // A `@dataclass_transform()` base: the marker returns the class.
                let decorators: Vec<ast::Decorator> = node.decorators().collect();
                self.canonical_transform_marker(&file, decorators.iter())
                    .is_some()
            }))
    }

    /// Field types of a class-form `NamedTuple` instance (or of a subclass of
    /// one), in tuple order, with the class's type arguments substituted.
    pub(super) fn class_namedtuple_fields(&self, value: &Type) -> Option<Vec<Type>> {
        let Type::Instance(class, args) = value else {
            return None;
        };
        let (owner, spec) = self
            .mro(class)
            .iter()
            .take(32)
            .find_map(|owner| Some((owner.clone(), self.synth_spec(owner)?)))?;
        // Functional forms (no class statement) have their own model.
        if spec.kind != SynthKind::NamedTuple || self.class_node(&owner).is_none() {
            return None;
        }
        let owner_args = self
            .mro_with_args(class, args)
            .into_iter()
            .find(|(cls, _)| *cls == owner)
            .map(|(_, args)| args)
            .unwrap_or_default();
        let subst = |name: &str| self.class_substitution(&owner, &owner_args, name);
        Some(
            self.own_fields(&owner, &spec)
                .into_iter()
                .map(|f| f.param.ty.substitute(&subst))
                .collect(),
        )
    }

    /// Ordering dunders a class gets without defining them:
    /// `@dataclass(order=True)` / attrs `order`, and `functools.total_ordering`
    /// (which derives the rest from one defined ordering method). The value is
    /// bound like a method: a function on the class, a bound method on an
    /// instance.
    pub fn synthesized_ordering_member(&self, ty: &Type, name: &str) -> Option<Type> {
        const ORDERING: [&str; 4] = ["__lt__", "__le__", "__gt__", "__ge__"];
        if !ORDERING.contains(&name) {
            return None;
        }
        let (class, is_instance) = match ty {
            Type::Instance(c, _) | Type::SelfType(c) => (c, true),
            Type::Class(c, _) => (c, false),
            _ => return None,
        };
        let object_file = self.builtin_class("object").map(|o| o.file);
        let owners = self.mro(class);
        let defines = |owner: &ClassRef, member: &str| {
            self.class_info(owner)
                .is_some_and(|info| info.members.contains_key(member))
        };
        for owner in owners.iter().take(32) {
            if owner.name == "object" && Some(owner.file) == object_file {
                break;
            }
            if defines(owner, name) {
                return None;
            }
            if self.synth_spec(owner).is_some_and(|s| s.order) {
                let other = Type::Instance(owner.clone(), vec![]);
                let receiver = Type::Instance(owner.clone(), vec![]);
                let param = |name: &str, ty: Type| Param {
                    name: name.into(),
                    kind: ParamKind::Normal,
                    ty,
                    has_default: false,
                    default_text: None,
                };
                let mut params = vec![param("other", other)];
                if !is_instance {
                    params.insert(0, param("self", receiver));
                }
                let sig = Arc::new(Signature {
                    params,
                    ret: self.instance_of("bool"),
                    is_async: false,
                    def: None,
                    docstring: None,
                    ret_inferred: false,
                });
                return Some(if is_instance {
                    Type::BoundMethod(sig, Box::new(ty.clone()))
                } else {
                    Type::Function(sig)
                });
            }
            if self.has_total_ordering(owner) {
                // The root is any ordering method the decorated class has
                // (inherited ones count, `object`'s do not).
                let inherited = |member: &str| {
                    owners
                        .iter()
                        .skip_while(|o| *o != owner)
                        .take_while(|o| !(o.name == "object" && Some(o.file) == object_file))
                        .any(|o| defines(o, member))
                };
                if inherited(name) {
                    return None;
                }
                let root = ORDERING.iter().find(|root| inherited(root))?;
                return self.lookup_member(ty, root);
            }
        }
        None
    }

    /// Whether `class` is decorated with `functools.total_ordering`.
    fn has_total_ordering(&self, class: &ClassRef) -> bool {
        let Some(info) = self.class_info(class) else {
            return false;
        };
        if !info
            .decorators
            .iter()
            .any(|d| d.rsplit('.').next() == Some("total_ordering"))
        {
            return false;
        }
        let Some((file, node)) = self.class_node(class) else {
            return false;
        };
        node.decorators().any(|d| {
            d.expr()
                .is_some_and(|e| self.total_ordering_decorator(&file, &e))
        })
    }

    /// `@functools.total_ordering`: returns the class it decorates, adding
    /// the missing ordering methods.
    pub(super) fn total_ordering_decorator(&self, file: &Arc<FileInfo>, e: &ast::Expr) -> bool {
        matches!(self.symbol_origin(file, e), Some((m, n)) if (m == "functools" || m.is_empty()) && n == "total_ordering")
    }

    /// What `value` (the right-hand side of a field declaration) says about the field.
    fn field_spec(&self, value: &ast::Expr) -> FieldSpec {
        let mut spec = FieldSpec {
            init: true,
            kw_only: None,
            default: Some(default_text(value)),
            alias: None,
        };
        let ast::Expr::Call(call) = value else {
            return spec;
        };
        let callee_name = call
            .callee()
            .map(|c| c.syntax().text().to_string())
            .unwrap_or_default();
        if !matches!(
            callee_name.rsplit('.').next(),
            Some("field" | "Field" | "ib" | "attrib")
        ) {
            return spec;
        }
        spec.default = None;
        let args = call.arg_list();
        for k in args.iter().flat_map(|a| a.keywords()) {
            let Some(v) = k.value() else { continue };
            match k.name_text().as_str() {
                "default" => spec.default = Some(default_text(&v)),
                "default_factory" | "factory" => spec.default = Some(SmolStr::new("<factory>")),
                "alias" => {
                    if let ast::Expr::String(s) = &v {
                        spec.alias = Some(SmolStr::new(s.value()));
                    }
                }
                _ => {}
            }
        }
        spec.init = bool_kwarg(args.as_ref(), "init").unwrap_or(true);
        spec.kw_only = bool_kwarg(args.as_ref(), "kw_only");
        // `Field(default)` / `attr.ib(default)`; `Field(...)` is required.
        if spec.default.is_none() {
            if let Some(first) = args.as_ref().and_then(|a| a.positional().next()) {
                if first.syntax().text() != "..." {
                    spec.default = Some(default_text(&first));
                }
            }
        }
        spec
    }

    /// Fields declared in `c`'s own body, in order.
    fn own_fields(&self, c: &ClassRef, spec: &SynthSpec) -> Vec<Field> {
        self.own_fields_with_init(c, spec, false)
    }
    fn own_fields_with_init(
        &self,
        c: &ClassRef,
        spec: &SynthSpec,
        include_non_init: bool,
    ) -> Vec<Field> {
        if let Some(params) = self.type_factory_init_params(c) {
            return params
                .into_iter()
                .map(|param| Field {
                    name: param.name.clone(),
                    kw_only: param.kind == ParamKind::KeywordOnly,
                    param,
                })
                .collect();
        }
        let Some((file, class)) = self.class_node(c) else {
            return Vec::new();
        };
        let Some(body) = class.body() else {
            return Vec::new();
        };
        let is_field_call = |v: &ast::Expr| matches!(v, ast::Expr::Call(call) if call.callee().is_some_and(|c| matches!(c.syntax().text().to_string().rsplit('.').next(), Some("ib" | "attrib" | "field"))));
        let stmts: Vec<ast::Stmt> = body.statements().collect();
        // attrs `@define` without auto_attribs: annotated attributes win when present.
        let auto_attribs = match spec.kind {
            SynthKind::Attrs { classic, auto_attribs } => auto_attribs.unwrap_or_else(|| {
                !classic && stmts.iter().any(|s| matches!(s, ast::Stmt::AnnAssign(a) if !a.annotation().is_some_and(|x| x.syntax().text().to_string().contains("ClassVar"))))
            }),
            _ => true,
        };
        let mut out = Vec::new();
        let mut kw_only_rest = spec.kw_only;
        for stmt in stmts {
            let (target, annotation, value) = match &stmt {
                ast::Stmt::AnnAssign(a) => (a.target(), a.annotation(), a.value()),
                ast::Stmt::Assign(a) if matches!(spec.kind, SynthKind::Attrs { .. }) => {
                    let targets = a.targets();
                    if targets.len() != 1 {
                        continue;
                    }
                    (targets.into_iter().next(), None, a.value())
                }
                _ => continue,
            };
            let Some(ast::Expr::Name(target)) = target else {
                continue;
            };
            let name = SmolStr::new(target.name_text());
            let ann_text = annotation
                .as_ref()
                .map(|x| x.syntax().text().to_string())
                .unwrap_or_default();
            let ann_head = match &annotation {
                Some(ast::Expr::Subscript(s)) => s
                    .object()
                    .map(|o| o.syntax().text().to_string())
                    .unwrap_or_default(),
                Some(other) => other.syntax().text().to_string(),
                None => String::new(),
            };
            // `typing` special forms are recognised through aliases too (`from typing import ClassVar as CV`).
            let ann_object = match &annotation {
                Some(ast::Expr::Subscript(s)) => s.object(),
                other => other.clone(),
            };
            let head = ann_object
                .as_ref()
                .and_then(|o| self.special_form_name(&file, o))
                .unwrap_or_else(|| ann_head.rsplit('.').next().unwrap_or("").to_string());
            if head == "ClassVar" {
                continue;
            }
            if spec.kind == SynthKind::Dataclass && head == "KW_ONLY" {
                kw_only_rest = true;
                continue;
            }
            match spec.kind {
                SynthKind::Attrs { .. } => {
                    // Fields: `attr.ib()` assignments, or annotations in auto_attribs mode.
                    let explicit = value.as_ref().is_some_and(is_field_call);
                    if annotation.is_none() && !explicit
                        || annotation.is_some() && !auto_attribs && !explicit
                    {
                        continue;
                    }
                }
                SynthKind::Transform if name.starts_with('_') => continue,
                _ => {}
            }
            let fs = match (&value, spec.kind) {
                (_, SynthKind::TypedDict) => FieldSpec {
                    init: true,
                    kw_only: Some(true),
                    default: None,
                    alias: None,
                },
                (Some(v), _) => self.field_spec(v),
                (None, _) => FieldSpec {
                    init: true,
                    kw_only: None,
                    default: None,
                    alias: None,
                },
            };
            if !fs.init && !include_non_init {
                continue;
            }
            let mut ty = annotation
                .as_ref()
                .map(|a| self.annotation_type(&file, a))
                .unwrap_or(Type::Unknown);
            if head == "InitVar" {
                // `InitVar[T]` is an `__init__` parameter of type `T`.
                ty = match &annotation {
                    Some(ast::Expr::Subscript(s)) => self
                        .subscript_args(&file, s)
                        .into_iter()
                        .next()
                        .unwrap_or(Type::Unknown),
                    _ => Type::Unknown,
                };
            }
            let (has_default, default_text) = match spec.kind {
                SynthKind::TypedDict => {
                    let required = self.typed_dict_required_field(c, &name).unwrap_or_else(|| {
                        match head.as_str() {
                            "Required" => true,
                            "NotRequired" => false,
                            _ => spec.total && !ann_text.contains("NotRequired["),
                        }
                    });
                    (!required, (!required).then(|| SmolStr::new("...")))
                }
                _ => (fs.default.is_some(), fs.default.clone()),
            };
            let param_name = match spec.kind {
                SynthKind::Attrs { .. } => fs
                    .alias
                    .clone()
                    .unwrap_or_else(|| SmolStr::new(name.trim_start_matches('_'))),
                _ => fs.alias.clone().unwrap_or_else(|| name.clone()),
            };
            let kw_only = fs.kw_only.unwrap_or(kw_only_rest);
            out.push(Field {
                name,
                param: Param {
                    name: param_name,
                    kind: ParamKind::Normal,
                    ty,
                    has_default,
                    default_text,
                },
                kw_only,
            });
        }
        out
    }

    // ----- decorators -----------------------------------------------------------

    /// `sig` after the decorators of `f` that change what calling it returns
    /// (`@contextmanager`, `@asynccontextmanager`); other decorators keep it.
    pub fn decorated_signature(
        &self,
        file: &Arc<FileInfo>,
        f: &ast::FuncDef,
        sig: Arc<Signature>,
    ) -> Arc<Signature> {
        let mut sig = sig;
        // Decorators apply bottom-up.
        let decorators: Vec<ast::Decorator> = f.decorators().collect();
        for d in decorators.iter().rev() {
            let Some(expr) = d.expr() else { continue };
            let (module, name) = self
                .symbol_origin(file, &expr)
                .unwrap_or_else(|| (String::new(), d.name_text().unwrap_or_default()));
            let class_name = match (module.as_str(), name.as_str()) {
                ("contextlib" | "", "contextmanager") => "_GeneratorContextManager",
                ("contextlib" | "", "asynccontextmanager") => "_AsyncGeneratorContextManager",
                _ => continue,
            };
            let Some(cm) = self.db.module("contextlib").and_then(|m| {
                let scope = m.scopes.module_scope();
                let id = *m
                    .scopes
                    .bindings_in(scope, class_name)
                    .iter()
                    .find(|id| matches!(m.scopes.binding(**id).kind, BindingKind::Class))?;
                Some(ClassRef {
                    file: m.id,
                    range: m.scopes.binding(id).node_range,
                    name: SmolStr::new(class_name),
                })
            }) else {
                continue;
            };
            let yielded = match &sig.ret {
                Type::Instance(c, args)
                    if matches!(
                        c.name.as_str(),
                        "Generator"
                            | "Iterator"
                            | "Iterable"
                            | "AsyncGenerator"
                            | "AsyncIterator"
                            | "AsyncIterable"
                    ) =>
                {
                    args.first().cloned().unwrap_or(Type::Unknown)
                }
                _ => Type::Unknown,
            };
            let n_params = self
                .class_info(&cm)
                .map(|i| i.type_params.len())
                .unwrap_or(1)
                .max(1);
            let mut args = vec![yielded];
            args.resize(n_params, Type::None);
            sig = Arc::new(Signature {
                ret: Type::Instance(cm, args),
                ret_inferred: false,
                ..(*sig).clone()
            });
        }
        sig
    }

    pub(super) fn synthesized_slot_names(&self, class: &ClassRef) -> Option<Vec<SmolStr>> {
        let mut result = Vec::new();
        for owner in self.mro(class).iter().take(32) {
            let Some(spec) = self.synth_spec(owner) else {
                continue;
            };
            let file = self.db.file(owner.file)?;
            if owner.range.len() > TextSize::new(65_536) {
                return None;
            }
            let fields = self.own_fields_with_init(owner, &spec, true);
            if fields.len() > 128 || result.len() + fields.len() > 256 {
                return None;
            }
            for field in fields {
                let annotation = self
                    .class_info(owner)?
                    .members
                    .get(&field.name)
                    .and_then(|member| member.annotation);
                if let Some(annotation) = annotation {
                    let expression =
                        node_with_range(&file.parse.syntax_node(), annotation, |node| {
                            ast::Expr::can_cast(node.kind())
                        })
                        .and_then(ast::Expr::cast)?;
                    if matches!(expression,ast::Expr::Subscript(subscript) if subscript.object().and_then(|head|self.symbol_origin(&file,&head)).is_some_and(|(_,name)|name=="InitVar"))
                    {
                        continue;
                    }
                }
                if !result.contains(&field.name) {
                    result.push(field.name);
                }
            }
        }
        Some(result)
    }

    /// Extra attributes of a function value created by a wrapping decorator
    /// (`functools.cache` / `lru_cache`: `cache_clear()`, `cache_info()`).
    pub fn function_wrapper(&self, sig: &Signature) -> Option<Type> {
        let def = sig.def.as_ref()?;
        let file = self.db.file(def.file)?;
        let f = node_with_range(&file.parse.syntax_node(), def.range, |n| {
            n.kind() == SyntaxKind::FUNC_DEF
        })
        .and_then(ast::FuncDef::cast)?;
        let cached = f.decorators().any(|d| {
            let Some(expr) = d.expr() else { return false };
            let (callee, _) = split_call(expr);
            matches!(self.symbol_origin(&file, &callee), Some((m, n)) if (m == "functools" || m.is_empty()) && (n == "cache" || n == "lru_cache"))
        });
        if !cached {
            return None;
        }
        let m = self.db.module("functools")?;
        let def = self.resolve_in_module(&m, "_lru_cache_wrapper", 0)?;
        match self.type_of_binding(&def.file, def.binding) {
            Type::Class(c, _) => Some(Type::Instance(c, vec![sig.ret.clone()])),
            _ => None,
        }
    }
}
