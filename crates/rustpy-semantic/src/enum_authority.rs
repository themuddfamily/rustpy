//! Pure enum identities over a bounded, caller-owned source world.
use crate::{
    db::FileInfo,
    scope::{BindingId, BindingKind},
    types::{ClassRef, LiteralValue},
};
use rustpy_core::{TextRange, TextSize};
use rustpy_syntax::{
    ast::{self, AstNode},
    SyntaxKind,
};
use smol_str::SmolStr;
use std::sync::Arc;

/// Data type member values are converted to; decides literal equality.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Data {
    /// `Enum` / `Flag`: members never equal plain values.
    Plain,
    /// `IntEnum`, `IntFlag` or an `int` mixin.
    Int,
    /// `StrEnum`: `auto()` is the lower-cased member name.
    Str,
    /// A `str` mixin on a plain `Enum`.
    MixStr,
    /// Another data type mixin: equality with plain values is unknown.
    Opaque,
}

/// The value a member was declared with.
#[derive(Clone, Debug, PartialEq)]
pub enum MemberValue {
    Literal(LiteralValue),
    /// A non-literal initializer expression in the declaring file.
    Expression(TextRange),
    /// `auto()` producing an untracked `int` / `str`.
    Int,
    Str,
    Unknown,
}

#[derive(Clone)]
struct Entry {
    name: SmolStr,
    canonical: SmolStr,
    value: MemberValue,
    /// Structural identity of literal tuples / floats, for value aliases.
    key: Option<String>,
}

#[derive(Clone)]
pub struct EnumModel {
    pub class: ClassRef,
    data: Data,
    flag: bool,
    custom_new: bool,
    custom_auto: bool,
    exact: bool,
    entries: Vec<Entry>,
    nonmembers: Vec<(SmolStr, Option<TextRange>)>,
}
impl EnumModel {
    pub fn is_flag(&self) -> bool {
        self.flag
    }
    pub fn is_int_flag(&self) -> bool {
        self.flag && self.data == Data::Int
    }
    /// Whether every statement of the declaration was understood.
    pub fn is_exact(&self) -> bool {
        self.exact
    }
    pub fn values(&self) -> Vec<LiteralValue> {
        self.entries
            .iter()
            .filter(|entry| entry.name == entry.canonical)
            .map(|entry| LiteralValue::EnumMember {
                class: self.class.clone(),
                member: entry.canonical.clone(),
            })
            .collect()
    }
    fn entry(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.name == name)
    }
    pub fn member(&self, name: &str) -> Option<LiteralValue> {
        Some(LiteralValue::EnumMember {
            class: self.class.clone(),
            member: self.entry(name)?.canonical.clone(),
        })
    }
    /// Every member name, aliases included, in declaration order.
    pub fn names(&self) -> Vec<SmolStr> {
        self.entries
            .iter()
            .map(|entry| entry.name.clone())
            .collect()
    }
    pub fn is_member(&self, name: &str) -> bool {
        self.entry(name).is_some()
    }
    /// `nonmember(value)` attributes with their wrapped value expression.
    pub fn nonmember(&self, name: &str) -> Option<Option<TextRange>> {
        self.nonmembers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, range)| *range)
    }
    pub fn underlying(&self, name: &str) -> Option<LiteralValue> {
        match self.value(name)? {
            MemberValue::Literal(value) => Some(value),
            _ => None,
        }
    }
    /// The member's `.value`; a custom `__new__` decides it at runtime.
    pub fn value(&self, name: &str) -> Option<MemberValue> {
        let entry = self.entry(name)?;
        Some(if self.custom_new {
            MemberValue::Unknown
        } else {
            entry.value.clone()
        })
    }
    /// Builtin class every `.value` is converted to, for data type mixins.
    pub fn value_class(&self) -> Option<&'static str> {
        match self.data {
            Data::Int => Some("int"),
            Data::Str | Data::MixStr => Some("str"),
            Data::Plain | Data::Opaque => None,
        }
    }
    pub fn primitive(&self, name: &str) -> Option<LiteralValue> {
        if !matches!(self.data, Data::Int | Data::Str | Data::MixStr) {
            return None;
        }
        self.underlying(name)
    }
    /// Whether `primitive` is authoritative: `None` then means "never equal".
    pub fn primitive_known(&self, name: &str) -> bool {
        match self.data {
            Data::Plain => true,
            Data::Opaque => false,
            _ => self.primitive(name).is_some(),
        }
    }
    fn auto_value(&self, name: &str, start: i64) -> MemberValue {
        if self.custom_auto {
            return MemberValue::Unknown;
        }
        match self.data {
            Data::Str => {
                return MemberValue::Literal(LiteralValue::Str(name.to_lowercase().into()))
            }
            Data::MixStr => return MemberValue::Str,
            Data::Opaque => return MemberValue::Unknown,
            Data::Plain | Data::Int => {}
        }
        let mut maximum: Option<i64> = None;
        for entry in &self.entries {
            let number = match &entry.value {
                MemberValue::Literal(LiteralValue::Int(value)) => *value,
                MemberValue::Literal(LiteralValue::Bool(value)) => i64::from(*value),
                _ => return MemberValue::Int,
            };
            maximum = Some(maximum.map_or(number, |old| old.max(number)));
        }
        let next = if self.flag {
            maximum.map_or(Some(start), |maximum| {
                let maximum = u64::try_from(maximum).ok()?;
                let power = maximum.checked_next_power_of_two()?;
                let power = if power <= maximum {
                    power.checked_mul(2)?
                } else {
                    power
                };
                i64::try_from(power).ok()
            })
        } else {
            maximum.map_or(Some(start), |maximum| maximum.checked_add(1))
        };
        next.map_or(MemberValue::Int, |next| {
            MemberValue::Literal(LiteralValue::Int(next))
        })
    }
    fn push(&mut self, name: SmolStr, value: MemberValue, key: Option<String>) {
        if self.entries.len() >= 128 || self.entry(&name).is_some() {
            self.exact = false;
            return;
        }
        if let MemberValue::Literal(literal) = &value {
            let mismatch = match self.data {
                Data::Int => !matches!(literal, LiteralValue::Int(_) | LiteralValue::Bool(_)),
                Data::Str | Data::MixStr => !matches!(literal, LiteralValue::Str(_)),
                Data::Plain if self.flag => {
                    !matches!(literal, LiteralValue::Int(_) | LiteralValue::Bool(_))
                }
                _ => false,
            };
            if mismatch {
                self.exact = false;
            }
        }
        let canonical = self
            .entries
            .iter()
            .find(|old| match (&old.value, &value) {
                (MemberValue::Literal(a), MemberValue::Literal(b)) => primitive_equal(a, b),
                _ => old.key.is_some() && old.key == key,
            })
            .map(|old| old.canonical.clone())
            .unwrap_or_else(|| name.clone());
        self.entries.push(Entry {
            name,
            canonical,
            value,
            key,
        });
    }
    /// One `name = value` class-body binding.
    fn declare(
        &mut self,
        file: &FileInfo,
        name: String,
        value: ast::Expr,
        ignored: &mut Vec<String>,
        imports: &impl Fn(&FileInfo, BindingId) -> Option<Arc<FileInfo>>,
    ) {
        // Dunder and private (mangled) names are never members.
        if name.starts_with("__") {
            return;
        }
        if name == "_ignore_" {
            match names_of(&value) {
                Some(names) => ignored.extend(names),
                None => self.exact = false,
            }
            return;
        }
        if ignored.contains(&name) {
            return;
        }
        if name.len() > 2 && name.starts_with('_') && name.ends_with('_') {
            if !matches!(name.as_str(), "_order_" | "_boundary_") {
                self.exact = false;
            }
            return;
        }
        let mut value = value.unparen();
        if matches!(value, ast::Expr::Lambda(_)) {
            return;
        }
        if let ast::Expr::Call(call) = &value {
            let Some(callee) = call.callee() else {
                self.exact = false;
                return;
            };
            let first = call
                .arg_list()
                .and_then(|arguments| arguments.positional().next());
            match enum_symbol(file, &callee, imports).as_deref() {
                Some("nonmember") => {
                    self.nonmembers
                        .push((name.into(), first.map(|first| first.text_range())));
                    return;
                }
                Some("member") => {
                    let Some(first) = first else {
                        self.exact = false;
                        return;
                    };
                    value = first.unparen();
                }
                Some("property") => return,
                _ if descriptor_call(file, &callee, imports) => return,
                _ => {}
            }
        }
        if let ast::Expr::Name(alias) = &value {
            if let Some(old) = self.entry(&alias.name_text()).cloned() {
                if self.entries.len() >= 128 || self.entry(&name).is_some() {
                    self.exact = false;
                    return;
                }
                self.entries.push(Entry {
                    name: name.into(),
                    canonical: old.canonical,
                    value: old.value,
                    key: old.key,
                });
                return;
            }
        }
        if let ast::Expr::Call(call) = &value {
            if call.callee().is_some_and(|callee| {
                enum_symbol(file, &callee, imports).as_deref() == Some("auto")
            }) {
                if call
                    .arg_list()
                    .is_some_and(|arguments| arguments.args().next().is_some())
                {
                    self.exact = false;
                }
                let value = self.auto_value(&name, 1);
                self.push(name.into(), value, None);
                return;
            }
        }
        if file.is_stub && value.syntax().text() == "..." {
            self.push(name.into(), MemberValue::Unknown, None);
            return;
        }
        match initializer(&value, self, 0, &mut 512) {
            Some(literal) => self.push(name.into(), MemberValue::Literal(literal), None),
            None => {
                let key = structural_key(&value, 0);
                self.push(
                    name.into(),
                    MemberValue::Expression(value.text_range()),
                    key,
                );
            }
        }
    }
}

fn is_enum_stub(file: &FileInfo) -> bool {
    file.is_stub && file.path == std::path::Path::new("<typeshed>/enum.pyi")
}

/// Name of a class or function the expression refers to in `enum.pyi`.
fn enum_symbol(
    file: &FileInfo,
    expression: &ast::Expr,
    imports: &impl Fn(&FileInfo, BindingId) -> Option<Arc<FileInfo>>,
) -> Option<SmolStr> {
    if let Some((owner, class)) = resolve_class(file, expression, imports) {
        return is_enum_stub(owner.as_deref().unwrap_or(file)).then_some(class.name);
    }
    let lookup = |name: &ast::NameExpr| {
        let ids = file.scopes.resolve(
            file.scopes.scope_at(name.syntax().text_range().start()),
            &name.name_text(),
        )?;
        (ids.len() == 1).then(|| ids[0])
    };
    let (id, name) = match expression.clone().unparen() {
        ast::Expr::Name(name) => {
            let id = lookup(&name)?;
            let BindingKind::Import(import) = &file.scopes.binding(id).kind else {
                return None;
            };
            (id, SmolStr::new(import.name.as_deref()?))
        }
        ast::Expr::Attribute(attribute) => {
            let ast::Expr::Name(module) = attribute.object()?.unparen() else {
                return None;
            };
            let id = lookup(&module)?;
            let BindingKind::Import(import) = &file.scopes.binding(id).kind else {
                return None;
            };
            if import.name.is_some() {
                return None;
            }
            (id, SmolStr::new(attribute.attr_text()))
        }
        _ => return None,
    };
    is_enum_stub(&*imports(file, id)?).then_some(name)
}

fn class_declaration(file: &FileInfo, class: &ClassRef) -> Option<ast::ClassDef> {
    let declaration = file
        .parse
        .syntax_node()
        .token_at_offset(class.range.start())
        .right_biased()?
        .parent_ancestors()
        .take(64)
        .find_map(ast::ClassDef::cast)?;
    (declaration.text_range() == class.range && declaration.name_text() == class.name.as_str())
        .then_some(declaration)
}

fn defines(declaration: &ast::ClassDef, names: &[&str]) -> bool {
    declaration.body().is_some_and(|body| {
        body.statements().take(256).any(|statement| {
            matches!(statement, ast::Stmt::FuncDef(function) if names.contains(&function.name_text().as_str()))
        })
    })
}

/// Instances of classes defining `__get__` are descriptors, not members.
fn descriptor_call(
    file: &FileInfo,
    callee: &ast::Expr,
    imports: &impl Fn(&FileInfo, BindingId) -> Option<Arc<FileInfo>>,
) -> bool {
    if let Some((owner, class)) = resolve_class(file, callee, imports) {
        let owner = owner.as_deref().unwrap_or(file);
        return class_declaration(owner, &class)
            .is_some_and(|declaration| defines(&declaration, &["__get__"]));
    }
    matches!(callee.clone().unparen(), ast::Expr::Name(name) if matches!(name.name_text().as_str(), "property" | "classmethod" | "staticmethod"))
}

/// `metaclass=EnumMeta` / `EnumType`, or a class deriving from them.
fn enum_metaclass(
    file: &FileInfo,
    expression: &ast::Expr,
    imports: &impl Fn(&FileInfo, BindingId) -> Option<Arc<FileInfo>>,
    depth: usize,
) -> bool {
    if depth >= 8 {
        return false;
    }
    let Some((owner, class)) = resolve_class(file, expression, imports) else {
        return false;
    };
    let owner = owner.as_deref().unwrap_or(file);
    if is_enum_stub(owner) {
        return matches!(class.name.as_str(), "EnumMeta" | "EnumType");
    }
    if owner.is_stub {
        return false;
    }
    class_declaration(owner, &class).is_some_and(|declaration| {
        declaration
            .bases()
            .iter()
            .take(8)
            .any(|base| enum_metaclass(owner, base, imports, depth + 1))
    })
}

/// `_ignore_` / functional names: a string of names or a sequence of strings.
fn names_of(expression: &ast::Expr) -> Option<Vec<String>> {
    let expression = expression.clone().unparen();
    if let Some(text) = string_of(&expression) {
        return Some(
            text.split(|character: char| character == ',' || character.is_whitespace())
                .filter(|name| !name.is_empty())
                .take(129)
                .map(ToOwned::to_owned)
                .collect(),
        );
    }
    let elements: Vec<_> = match expression {
        ast::Expr::List(list) => list.elements().take(129).collect(),
        ast::Expr::Tuple(tuple) => tuple.elements().take(129).collect(),
        _ => return None,
    };
    elements.iter().map(string_of).collect()
}
fn string_of(expression: &ast::Expr) -> Option<String> {
    match primitive(&expression.clone().unparen())? {
        LiteralValue::Str(text) => Some(text.to_string()),
        _ => None,
    }
}

/// Equality key for non-literal constant values (tuples, floats, `None`).
fn structural_key(expression: &ast::Expr, depth: usize) -> Option<String> {
    if depth >= 8 || expression.text_range().len() > TextSize::new(4096) {
        return None;
    }
    match expression.clone().unparen() {
        ast::Expr::Tuple(tuple) => {
            let mut key = String::from("(");
            for (index, element) in tuple.elements().enumerate() {
                if index >= 32 {
                    return None;
                }
                key.push_str(&structural_key(&element, depth + 1)?);
                key.push(',');
            }
            key.push(')');
            Some(key)
        }
        ast::Expr::Literal(literal) if literal.kind() == Some(SyntaxKind::FLOAT_NUMBER) => {
            let text = literal.syntax().text().to_string().replace('_', "");
            Some(format!("{:?}", text.parse::<f64>().ok()?))
        }
        ast::Expr::Literal(literal) if literal.kind() == Some(SyntaxKind::NONE_KW) => {
            Some("None".into())
        }
        other => primitive(&other).map(|value| match value {
            LiteralValue::Bool(value) => i64::from(value).to_string(),
            LiteralValue::Int(value) => value.to_string(),
            other => format!("{other:?}"),
        }),
    }
}

/// The closed enum authority: an exact, intact, non-empty member layout.
pub fn model(
    file: &FileInfo,
    class: &ClassRef,
    imports: &impl Fn(&FileInfo, BindingId) -> Option<Arc<FileInfo>>,
) -> Option<EnumModel> {
    let model = scan(file, class, imports)?;
    (model.exact && !model.entries.is_empty() && namespace_intact(file, &model)).then_some(model)
}

/// Best-effort member layout of an enum class or functional enum call; check
/// `is_exact` before treating it as a closed domain.
pub fn scan(
    file: &FileInfo,
    class: &ClassRef,
    imports: &impl Fn(&FileInfo, BindingId) -> Option<Arc<FileInfo>>,
) -> Option<EnumModel> {
    if file.id != class.file || file.text.len() > 4 * 1024 * 1024 || is_enum_stub(file) {
        return None;
    }
    match class_declaration(file, class) {
        Some(declaration) => scan_class(file, &declaration, class.clone(), imports, 0, &mut 64),
        None => functional(file, class, imports),
    }
}

fn scan_class(
    file: &FileInfo,
    declaration: &ast::ClassDef,
    class: ClassRef,
    imports: &impl Fn(&FileInfo, BindingId) -> Option<Arc<FileInfo>>,
    depth: usize,
    budget: &mut usize,
) -> Option<EnumModel> {
    if depth >= 8 || *budget == 0 {
        return None;
    }
    *budget -= 1;
    let mut exact = declaration.text_range().len() <= TextSize::new(65_536);
    for decorator in declaration.decorators().take(16) {
        let callee = match decorator.expr() {
            Some(ast::Expr::Call(call)) => call.callee(),
            other => other,
        };
        if !callee.is_some_and(|callee| {
            matches!(
                enum_symbol(file, &callee, imports).as_deref(),
                Some("unique" | "verify")
            )
        }) {
            exact = false;
        }
    }
    // (data, flag, custom __new__, custom auto) of the enum base.
    let mut root: Option<(Data, bool, bool, bool)> = None;
    let mut metaclass = false;
    let mut mixins = Vec::new();
    let mut custom_new = false;
    for argument in declaration
        .arguments()
        .into_iter()
        .flat_map(|arguments| arguments.args().take(17).collect::<Vec<_>>())
    {
        match argument {
            ast::AnyArg::Positional(argument) => {
                let base = argument.expr()?;
                let Some((owner, base_class)) = resolve_class(file, &base, imports) else {
                    // Builtins are not bindings of the declaring file.
                    match base.clone().unparen() {
                        ast::Expr::Name(name) => match name.name_text().as_str() {
                            "object" => {}
                            "str" => mixins.push(Data::MixStr),
                            "int" => mixins.push(Data::Int),
                            "float" | "bytes" | "complex" | "tuple" | "frozenset" => {
                                mixins.push(Data::Opaque)
                            }
                            _ => {
                                exact = false;
                                mixins.push(Data::Opaque);
                            }
                        },
                        _ => {
                            exact = false;
                            mixins.push(Data::Opaque);
                        }
                    }
                    continue;
                };
                let owner = owner.as_deref().unwrap_or(file);
                if is_enum_stub(owner) {
                    let found = match base_class.name.as_str() {
                        "Enum" | "ReprEnum" => (Data::Plain, false),
                        "IntEnum" => (Data::Int, false),
                        "StrEnum" => (Data::Str, false),
                        "Flag" => (Data::Plain, true),
                        "IntFlag" => (Data::Int, true),
                        _ => return None,
                    };
                    if root.replace((found.0, found.1, false, false)).is_some() {
                        return None;
                    }
                } else if owner.is_stub {
                    let builtin = owner.path == std::path::Path::new("<typeshed>/builtins.pyi");
                    match base_class.name.as_str() {
                        "object" if builtin => {}
                        "str" if builtin => mixins.push(Data::MixStr),
                        "int" if builtin => mixins.push(Data::Int),
                        _ => mixins.push(Data::Opaque),
                    }
                } else {
                    let base_declaration = class_declaration(owner, &base_class)?;
                    match scan_class(
                        owner,
                        &base_declaration,
                        base_class,
                        imports,
                        depth + 1,
                        budget,
                    ) {
                        Some(base) => {
                            // Enums with members cannot be extended.
                            if !base.entries.is_empty() {
                                return None;
                            }
                            exact &= base.exact;
                            if root
                                .replace((base.data, base.flag, base.custom_new, base.custom_auto))
                                .is_some()
                            {
                                return None;
                            }
                        }
                        None => {
                            // A plain mixin: only a data type (`__new__`) or
                            // custom equality changes the member semantics.
                            if defines(&base_declaration, &["__eq__", "__ne__", "__hash__"]) {
                                exact = false;
                            }
                            if defines(&base_declaration, &["__new__"])
                                || !base_declaration.bases().is_empty()
                            {
                                custom_new = true;
                                mixins.push(Data::Opaque);
                            }
                        }
                    }
                }
            }
            ast::AnyArg::Keyword(keyword) => match keyword.name_text().as_str() {
                "metaclass" => {
                    metaclass = keyword
                        .value()
                        .is_some_and(|value| enum_metaclass(file, &value, imports, depth));
                    if !metaclass {
                        exact = false;
                    }
                }
                "boundary" => {}
                _ => exact = false,
            },
            _ => return None,
        }
    }
    let (mut data, flag, base_new, custom_auto) = match root {
        Some(root) => root,
        None if metaclass => (Data::Plain, false, false, false),
        None => return None,
    };
    for mixin in mixins {
        data = match (data, mixin) {
            (Data::Plain, Data::MixStr) if !flag => Data::MixStr,
            (Data::Plain, Data::Int) => Data::Int,
            (Data::Int, Data::Int) | (Data::Str | Data::MixStr, Data::MixStr) => data,
            _ => Data::Opaque,
        };
    }
    let mut model = EnumModel {
        class,
        data,
        flag,
        custom_new: custom_new || base_new,
        custom_auto,
        exact,
        entries: Vec::new(),
        nonmembers: Vec::new(),
    };
    let body = declaration.body()?;
    let statements: Vec<_> = body.statements().take(257).collect();
    if statements.len() > 256 {
        model.exact = false;
    }
    // Hooks that change every member's value or equality, wherever declared.
    for statement in &statements {
        let ast::Stmt::FuncDef(function) = statement else {
            continue;
        };
        match function.name_text().as_str() {
            "__new__" => model.custom_new = true,
            "_generate_next_value_" => model.custom_auto = true,
            "__eq__" | "__ne__" | "__hash__" | "__getattribute__" | "__getattr__"
            | "__setattr__" | "__index__" | "__or__" | "__and__" | "__xor__" | "__invert__"
            | "name" | "value" => model.exact = false,
            _ => {}
        }
    }
    let mut ignored = Vec::new();
    for statement in statements {
        match statement {
            ast::Stmt::Assign(assign) => {
                let targets = assign.targets();
                let (Some(ast::Expr::Name(name)), Some(value), 1) = (
                    targets.first().cloned().map(ast::Expr::unparen),
                    assign.value(),
                    targets.len(),
                ) else {
                    model.exact = false;
                    continue;
                };
                model.declare(file, name.name_text(), value, &mut ignored, imports);
            }
            ast::Stmt::AnnAssign(assign) => {
                // A bare annotation (`_value_: int`) declares no member.
                if let (Some(ast::Expr::Name(name)), Some(value)) =
                    (assign.target().map(ast::Expr::unparen), assign.value())
                {
                    model.declare(file, name.name_text(), value, &mut ignored, imports);
                }
            }
            ast::Stmt::FuncDef(function) => {
                for decorator in function.decorators().take(16) {
                    let Some(expression) = decorator.expr() else {
                        continue;
                    };
                    let callee = match &expression {
                        ast::Expr::Call(call) => call.callee(),
                        other => Some(other.clone()),
                    };
                    let Some(callee) = callee else {
                        continue;
                    };
                    match enum_symbol(file, &callee, imports).as_deref() {
                        Some("member") => {
                            let name = function.name_text();
                            if !name.starts_with("__") {
                                model.push(name.into(), MemberValue::Unknown, None);
                            }
                        }
                        Some("nonmember" | "property" | "unique") => {}
                        _ => {
                            let text = callee.syntax().text().to_string();
                            if !matches!(
                                text.rsplit('.').next(),
                                Some(
                                    "property"
                                        | "classmethod"
                                        | "staticmethod"
                                        | "cached_property"
                                        | "DynamicClassAttribute"
                                        | "abstractmethod"
                                        | "overload"
                                        | "override"
                                        | "cache"
                                        | "lru_cache"
                                        | "deprecated"
                                        | "setter"
                                        | "getter"
                                        | "deleter"
                                )
                            ) {
                                model.exact = false;
                            }
                        }
                    }
                }
            }
            ast::Stmt::ClassDef(_) | ast::Stmt::Pass(_) => {}
            ast::Stmt::Expr(expr) if matches!(expr.expr(), Some(ast::Expr::String(_))) => {}
            _ => model.exact = false,
        }
    }
    Some(model)
}

/// `Enum("Color", "RED GREEN")` and the other functional API spellings.
fn functional(
    file: &FileInfo,
    class: &ClassRef,
    imports: &impl Fn(&FileInfo, BindingId) -> Option<Arc<FileInfo>>,
) -> Option<EnumModel> {
    if class.range.len() > TextSize::new(65_536) {
        return None;
    }
    let element = file.parse.syntax_node().covering_element(class.range);
    let call = element
        .as_node()
        .cloned()
        .or_else(|| element.parent())?
        .ancestors()
        .take(32)
        .find(|node| node.text_range() == class.range)
        .and_then(ast::CallExpr::cast)?;
    let (owner, base) = resolve_class(file, &call.callee()?, imports)?;
    if !is_enum_stub(owner.as_deref().unwrap_or(file)) {
        return None;
    }
    let (data, flag) = match base.name.as_str() {
        "Enum" => (Data::Plain, false),
        "IntEnum" => (Data::Int, false),
        "StrEnum" => (Data::Str, false),
        "Flag" => (Data::Plain, true),
        "IntFlag" => (Data::Int, true),
        _ => return None,
    };
    let arguments = call.arg_list()?;
    if arguments.args().take(64).any(|argument| {
        !matches!(
            argument,
            ast::AnyArg::Positional(_) | ast::AnyArg::Keyword(_)
        )
    }) {
        return None;
    }
    let positional: Vec<_> = arguments.positional().take(3).collect();
    if positional.len() > 2 || string_of(positional.first()?)? != class.name.as_str() {
        return None;
    }
    let mut model = EnumModel {
        class: class.clone(),
        data,
        flag,
        custom_new: false,
        custom_auto: false,
        exact: true,
        entries: Vec::new(),
        nonmembers: Vec::new(),
    };
    let mut names = positional.get(1).cloned();
    let mut start = 1;
    for keyword in arguments.keywords().take(16) {
        let value = keyword.value()?;
        match keyword.name_text().as_str() {
            "names" if names.is_none() => names = Some(value),
            "module" | "qualname" | "boundary" => {}
            "start" => match primitive(&value.unparen()) {
                Some(LiteralValue::Int(value)) => start = value,
                _ => model.exact = false,
            },
            "type" => {
                model.data = Data::Opaque;
                model.exact = false;
            }
            _ => return None,
        }
    }
    let names = names?.unparen();
    let elements: Vec<_> = match &names {
        ast::Expr::List(list) => list.elements().take(129).collect(),
        ast::Expr::Tuple(tuple) => tuple.elements().take(129).collect(),
        ast::Expr::Dict(dict) => {
            if dict.unpacks().next().is_some() {
                return None;
            }
            for item in dict.items().take(129) {
                let name = string_of(&item.key()?)?;
                let value = item.value()?;
                model.declare(file, name, value, &mut Vec::new(), imports);
            }
            return Some(model);
        }
        _ => Vec::new(),
    };
    if elements.iter().any(|element| {
        matches!(
            element.clone().unparen(),
            ast::Expr::Tuple(_) | ast::Expr::List(_)
        )
    }) {
        for pair in elements {
            let pair: Vec<_> = match pair.unparen() {
                ast::Expr::Tuple(tuple) => tuple.elements().take(3).collect(),
                ast::Expr::List(list) => list.elements().take(3).collect(),
                _ => return None,
            };
            if pair.len() != 2 {
                return None;
            }
            model.declare(
                file,
                string_of(&pair[0])?,
                pair[1].clone(),
                &mut Vec::new(),
                imports,
            );
        }
        return Some(model);
    }
    for name in names_of(&names)? {
        let value = model.auto_value(&name, start);
        model.push(name.into(), value, None);
    }
    Some(model)
}

/// Resolves only physical class declarations and captured import/assignment aliases.
pub fn resolve_class(
    file: &FileInfo,
    expression: &ast::Expr,
    imports: &impl Fn(&FileInfo, BindingId) -> Option<Arc<FileInfo>>,
) -> Option<(Option<Arc<FileInfo>>, ClassRef)> {
    resolve(file, expression, imports, 0, &mut 256)
}
fn resolve(
    file: &FileInfo,
    expression: &ast::Expr,
    imports: &impl Fn(&FileInfo, BindingId) -> Option<Arc<FileInfo>>,
    depth: usize,
    budget: &mut usize,
) -> Option<(Option<Arc<FileInfo>>, ClassRef)> {
    if depth >= 16 || *budget == 0 {
        return None;
    }
    *budget -= 1;
    let expression = expression.clone().unparen();
    if let ast::Expr::Call(call) = &expression {
        // `Color = Enum("Color", ...)`: the factory call is the class identity.
        let (owner, base) = resolve(file, &call.callee()?, imports, depth + 1, budget)?;
        if !is_enum_stub(owner.as_deref().unwrap_or(file))
            || !matches!(
                base.name.as_str(),
                "Enum" | "IntEnum" | "StrEnum" | "Flag" | "IntFlag"
            )
        {
            return None;
        }
        let name = string_of(&call.arg_list()?.positional().next()?)?;
        return Some((
            None,
            ClassRef {
                file: file.id,
                range: call.syntax().text_range(),
                name: name.into(),
            },
        ));
    }
    let (from, name, cutoff) = match &expression {
        ast::Expr::Name(name) => (None, name.name_text(), expression.text_range().start()),
        ast::Expr::Attribute(attribute) => {
            let ast::Expr::Name(module) = attribute.object()?.unparen() else {
                return None;
            };
            let ids = file.scopes.resolve(
                file.scopes.scope_at(module.text_range().start()),
                &module.name_text(),
            )?;
            if ids.len() != 1 {
                return None;
            }
            let binding = file.scopes.binding(ids[0]);
            let BindingKind::Import(import) = &binding.kind else {
                return None;
            };
            if import.name.is_some() || binding.range.start() >= expression.text_range().start() {
                return None;
            }
            (
                Some(imports(file, ids[0])?),
                attribute.attr_text(),
                TextSize::new(u32::MAX),
            )
        }
        _ => return None,
    };
    let from_owner = from;
    let from = from_owner.as_deref().unwrap_or(file);
    let scope = if from.id == file.id {
        let mut scope = from.scopes.scope_at(expression.text_range().start());
        if let Some(class) = expression
            .syntax()
            .ancestors()
            .take(32)
            .find_map(ast::ClassDef::cast)
        {
            if class
                .body()
                .is_some_and(|body| expression.text_range().end() <= body.text_range().start())
                && from.scopes.scope(scope).kind == crate::scope::ScopeKind::Class
            {
                scope = from.scopes.scope(scope).parent?;
            }
        }
        scope
    } else {
        from.scopes.module_scope()
    };
    let ids = from.scopes.resolve(scope, &name)?;
    if ids.len() != 1 {
        return None;
    }
    let binding = from.scopes.binding(ids[0]);
    if binding.range.start() >= cutoff {
        return None;
    }
    match &binding.kind {
        BindingKind::Class => Some((
            from_owner.clone(),
            ClassRef {
                file: from.id,
                range: binding.node_range,
                name: binding.name.clone(),
            },
        )),
        BindingKind::Import(import) => imported_class(
            imports(from, ids[0])?,
            import.name.as_deref()?,
            imports,
            depth + 1,
            budget,
        ),
        BindingKind::Assignment => {
            let range = binding.value?;
            let node = from.parse.syntax_node().covering_element(range);
            let expr = node
                .as_node()
                .cloned()
                .or_else(|| node.parent())?
                .ancestors()
                .take(32)
                .find(|node| node.text_range() == range)
                .and_then(ast::Expr::cast)?;
            let (owner, class) = resolve(from, &expr, imports, depth + 1, budget)?;
            Some((owner.or(from_owner), class))
        }
        _ => None,
    }
}

/// Applies the same captured namespace-retirement policy to the querying file.
pub fn namespace_intact(file: &FileInfo, model: &EnumModel) -> bool {
    if file.text.len() > 4 * 1024 * 1024 {
        return false;
    }
    // Any visible namespace store with a relevant slot retires the model. This
    // intentionally over-approximates receivers rather than guessing aliases.
    let mut nodes = 0;
    for node in file.parse.syntax_node().descendants() {
        nodes += 1;
        if nodes > 16_384 {
            return false;
        }
        let targets = match ast::Stmt::cast(node.clone()) {
            Some(ast::Stmt::Assign(assign)) => assign.targets(),
            Some(ast::Stmt::AnnAssign(assign)) => assign.target().into_iter().collect(),
            Some(ast::Stmt::AugAssign(assign)) => assign.target().into_iter().collect(),
            Some(ast::Stmt::Del(delete)) => delete.targets(),
            _ => Vec::new(),
        };
        for target in targets {
            for node in target.syntax().descendants().take(258) {
                nodes += 1;
                if nodes > 16_384 {
                    return false;
                }
                if let Some(attribute) = ast::AttributeExpr::cast(node) {
                    let member = attribute.attr_text();
                    if matches!(
                        member.as_str(),
                        "Enum"
                            | "IntEnum"
                            | "StrEnum"
                            | "Flag"
                            | "IntFlag"
                            | "auto"
                            | "__new__"
                            | "__eq__"
                            | "__ne__"
                    ) || model
                        .entries
                        .iter()
                        .any(|entry| entry.name == member.as_str())
                    {
                        return false;
                    }
                }
            }
            if target.syntax().descendants().take(258).count() > 257 {
                return false;
            }
        }
    }
    true
}

fn primitive(expression: &ast::Expr) -> Option<LiteralValue> {
    if expression.text_range().len() > TextSize::new(4096) {
        return None;
    }
    match expression {
        ast::Expr::Literal(value) => match value.kind()? {
            SyntaxKind::TRUE_KW => Some(LiteralValue::Bool(true)),
            SyntaxKind::FALSE_KW => Some(LiteralValue::Bool(false)),
            SyntaxKind::INT_NUMBER => {
                let text = value.syntax().text().to_string().replace('_', "");
                let (radix, digits) = if let Some(x) =
                    text.strip_prefix("0x").or_else(|| text.strip_prefix("0X"))
                {
                    (16, x)
                } else if let Some(x) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B"))
                {
                    (2, x)
                } else if let Some(x) = text.strip_prefix("0o").or_else(|| text.strip_prefix("0O"))
                {
                    (8, x)
                } else {
                    (10, text.as_str())
                };
                Some(LiteralValue::Int(i64::from_str_radix(digits, radix).ok()?))
            }
            _ => None,
        },
        ast::Expr::Unary(value) => {
            let operand = primitive(&value.operand()?.unparen())?;
            match (value.op()?.kind(), operand) {
                (SyntaxKind::MINUS, LiteralValue::Int(value)) => {
                    Some(LiteralValue::Int(value.checked_neg()?))
                }
                (SyntaxKind::PLUS, LiteralValue::Int(value)) => Some(LiteralValue::Int(value)),
                _ => None,
            }
        }
        ast::Expr::String(value) => {
            match rustpy_syntax::string_literal::string(&value.syntax().text().to_string()).ok()? {
                rustpy_syntax::string_literal::Decoded::Text(text, _) => {
                    Some(LiteralValue::Str(text.into()))
                }
                _ => None,
            }
        }
        _ => None,
    }
}
fn primitive_equal(a: &LiteralValue, b: &LiteralValue) -> bool {
    match (a, b) {
        (LiteralValue::Int(a), LiteralValue::Bool(b))
        | (LiteralValue::Bool(b), LiteralValue::Int(a)) => *a == i64::from(*b),
        _ => a == b,
    }
}

/// Class prefix from a dotted match pattern; no synthetic expressions or inference.
pub fn resolve_class_names(
    file: &FileInfo,
    scope: crate::scope::ScopeId,
    names: &[String],
    imports: &impl Fn(&FileInfo, BindingId) -> Option<Arc<FileInfo>>,
) -> Option<(Option<Arc<FileInfo>>, ClassRef)> {
    if names.is_empty() || names.len() > 2 || names.iter().any(|name| name.len() > 4096) {
        return None;
    }
    let ids = file.scopes.resolve(scope, &names[0])?;
    if ids.len() != 1 {
        return None;
    }
    let binding = file.scopes.binding(ids[0]);
    if names.len() == 2 {
        let BindingKind::Import(import) = &binding.kind else {
            return None;
        };
        if import.name.is_some() {
            return None;
        }
        return imported_class(imports(file, ids[0])?, &names[1], imports, 0, &mut 256);
    }
    match &binding.kind {
        BindingKind::Class => Some((
            None,
            ClassRef {
                file: file.id,
                range: binding.node_range,
                name: binding.name.clone(),
            },
        )),
        BindingKind::Import(import) => imported_class(
            imports(file, ids[0])?,
            import.name.as_deref()?,
            imports,
            0,
            &mut 256,
        ),
        BindingKind::Assignment => {
            let range = binding.value?;
            let element = file.parse.syntax_node().covering_element(range);
            let expression = element
                .as_node()
                .cloned()
                .or_else(|| element.parent())?
                .ancestors()
                .take(32)
                .find(|node| node.text_range() == range)
                .and_then(ast::Expr::cast)?;
            resolve_class(file, &expression, imports)
        }
        _ => None,
    }
}

/// Equality of supported enum atoms in this captured world. Unknown models do
/// not turn into inequality, which would incorrectly remove residual cases.
pub fn pattern_equal(
    left: &LiteralValue,
    right: &LiteralValue,
    singleton: bool,
    models: &impl Fn(&ClassRef) -> Option<EnumModel>,
) -> Option<bool> {
    for value in [left, right] {
        if let LiteralValue::EnumMember { class, member } = value {
            let model = models(class)?;
            if model.member(member).as_ref() != Some(value) {
                return None;
            }
        }
    }
    if left == right {
        return Some(true);
    }
    if singleton {
        return Some(false);
    }
    // `None` inside: never equal; outer `None`: equality is unknown.
    let primitive = |value: &LiteralValue| -> Option<Option<LiteralValue>> {
        match value {
            LiteralValue::EnumMember { class, member } => {
                let model = models(class)?;
                model
                    .primitive_known(member)
                    .then(|| model.primitive(member))
            }
            _ => Some(Some(value.clone())),
        }
    };
    match (primitive(left)?, primitive(right)?) {
        (Some(a), Some(b)) => Some(primitive_equal(&a, &b)),
        _ => Some(false),
    }
}

fn imported_class(
    target: Arc<FileInfo>,
    name: &str,
    imports: &impl Fn(&FileInfo, BindingId) -> Option<Arc<FileInfo>>,
    depth: usize,
    budget: &mut usize,
) -> Option<(Option<Arc<FileInfo>>, ClassRef)> {
    if depth >= 16 || *budget == 0 {
        return None;
    }
    *budget -= 1;
    let ids = target
        .scopes
        .bindings_in(target.scopes.module_scope(), name);
    if ids.len() != 1 {
        return None;
    }
    let id = ids[0];
    let definition = target.scopes.binding(id);
    match &definition.kind {
        BindingKind::Class => Some((
            Some(target.clone()),
            ClassRef {
                file: target.id,
                range: definition.node_range,
                name: definition.name.clone(),
            },
        )),
        BindingKind::Import(import) => imported_class(
            imports(&target, id)?,
            import.name.as_deref()?,
            imports,
            depth + 1,
            budget,
        ),
        BindingKind::Assignment => {
            let range = definition.value?;
            let element = target.parse.syntax_node().covering_element(range);
            let expression = element
                .as_node()
                .cloned()
                .or_else(|| element.parent())?
                .ancestors()
                .take(32)
                .find(|node| node.text_range() == range)
                .and_then(ast::Expr::cast)?;
            let (owner, class) = resolve(&target, &expression, imports, depth + 1, budget)?;
            Some((owner.or(Some(target)), class))
        }
        _ => None,
    }
}

fn initializer(
    expression: &ast::Expr,
    model: &EnumModel,
    depth: usize,
    budget: &mut usize,
) -> Option<LiteralValue> {
    if depth >= 16 || *budget == 0 {
        return None;
    }
    *budget -= 1;
    let expression = expression.clone().unparen();
    if let ast::Expr::Name(name) = &expression {
        return model.underlying(&name.name_text());
    }
    if let ast::Expr::Binary(binary) = &expression {
        let left = initializer(&binary.lhs()?, model, depth + 1, budget)?;
        let right = initializer(&binary.rhs()?, model, depth + 1, budget)?;
        let integer = |value| match value {
            LiteralValue::Int(value) => Some(value),
            LiteralValue::Bool(value) => Some(i64::from(value)),
            _ => None,
        };
        let a = integer(left)?;
        let b = integer(right)?;
        let value = match binary.op()?.kind() {
            SyntaxKind::PIPE => a | b,
            SyntaxKind::AMP => a & b,
            SyntaxKind::CARET => a ^ b,
            SyntaxKind::PLUS => a.checked_add(b)?,
            SyntaxKind::MINUS => a.checked_sub(b)?,
            SyntaxKind::STAR => a.checked_mul(b)?,
            SyntaxKind::SHL => a.checked_shl(u32::try_from(b).ok()?)?,
            SyntaxKind::SHR => a.checked_shr(u32::try_from(b).ok()?)?,
            _ => return None,
        };
        return Some(LiteralValue::Int(value));
    }
    primitive(&expression)
}
