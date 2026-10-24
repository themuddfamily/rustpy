//! Type representation (roadmap §2.2 "Type system").

use std::fmt;
use std::sync::Arc;

use rustpy_core::{FileId, TextRange};
use smol_str::SmolStr;

/// Identity of a class: the file and range of its `class` statement.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ClassRef {
    pub file: FileId,
    pub range: TextRange,
    pub name: SmolStr,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FunctionRef {
    pub file: FileId,
    pub range: TextRange,
    pub name: SmolStr,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum ParamKind {
    PositionalOnly,
    Normal,
    KeywordOnly,
    VarArgs,
    KwArgs,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Param {
    pub name: SmolStr,
    pub kind: ParamKind,
    pub ty: Type,
    pub has_default: bool,
    /// Source text of the default value, for presentation.
    pub default_text: Option<SmolStr>,
}

/// A callable signature (function, method, lambda, or class constructor).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Signature {
    pub params: Vec<Param>,
    pub ret: Type,
    pub is_async: bool,
    pub def: Option<FunctionRef>,
    pub docstring: Option<Arc<str>>,
    /// True when `ret` was inferred from the body rather than annotated.
    pub ret_inferred: bool,
}

impl Signature {
    pub fn display_params(&self) -> String {
        let mut parts = Vec::new();
        let mut seen_kw_only = false;
        let mut seen_pos_only = false;
        for (i, p) in self.params.iter().enumerate() {
            if p.kind == ParamKind::PositionalOnly {
                seen_pos_only = true;
            } else if seen_pos_only {
                parts.push("/".to_string());
                seen_pos_only = false;
            }
            if p.kind == ParamKind::KeywordOnly
                && !seen_kw_only
                && !self.params[..i]
                    .iter()
                    .any(|q| q.kind == ParamKind::VarArgs)
            {
                parts.push("*".to_string());
                seen_kw_only = true;
            }
            let mut s = match p.kind {
                ParamKind::VarArgs => format!("*{}", p.name),
                ParamKind::KwArgs => format!("**{}", p.name),
                _ => p.name.to_string(),
            };
            if !matches!(p.ty, Type::Unknown) {
                s.push_str(&format!(": {}", p.ty));
            }
            if let Some(d) = &p.default_text {
                s.push_str(if matches!(p.ty, Type::Unknown) {
                    "="
                } else {
                    " = "
                });
                s.push_str(d);
            }
            parts.push(s);
        }
        if seen_pos_only {
            parts.push("/".to_string());
        }
        parts.join(", ")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum LiteralValue {
    Int(i64),
    Str(SmolStr),
    Bytes(SmolStr),
    Bool(bool),
    /// Canonical enum member identity; aliases share the first member's name.
    /// Primitive equality for IntEnum/StrEnum is supplied by enum authority,
    /// never by replacing this nominal identity with its initializer value.
    EnumMember {
        class: ClassRef,
        member: SmolStr,
    },
}

/// A declared type variable has source identity independently of its label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Variance {
    Invariant,
    Covariant,
    Contravariant,
    /// Automatic variance has not yet been proven by this classifier.
    Inferred,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TypeVarDefinition {
    pub key: SmolStr,
    pub name: SmolStr,
    pub bound: Option<Box<Type>>,
    pub constraints: Vec<Type>,
    pub variance: Variance,
    pub default: Option<Box<Type>>,
}

/// A ParamSpec is a declaration identity, not a scalar type variable.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ParameterSpecDefinition {
    pub key: SmolStr,
    pub name: SmolStr,
    pub variance: Variance,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TuplePackDefinition {
    pub key: SmolStr,
    pub name: SmolStr,
    pub variance: Variance,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Type {
    /// Inference failed or is not implemented.
    Unknown,
    /// Explicit `typing.Any`.
    Any,
    Never,
    /// A string statically restricted to literal-derived contents.
    LiteralString,
    /// Literal-derived inferred string; displays like ordinary inferred str.
    LiteralStringValue,
    None,
    Module(FileId),
    /// The class object itself (`type[C]`), with explicit type arguments if any.
    Class(ClassRef, Vec<Type>),
    /// An instance of a class with type arguments (`list[int]`).
    Instance(ClassRef, Vec<Type>),
    /// Fixed-length tuple.
    Tuple(Vec<Type>),
    Function(Arc<Signature>),
    /// A method bound to `self`.
    BoundMethod(Arc<Signature>, Box<Type>),
    /// `typing.Callable[[args], ret]` / a Protocol-like callable.
    Callable(Arc<Signature>),
    /// `@overload`ed function (already bound when accessed through an instance).
    Overloaded(Vec<Arc<Signature>>),
    Union(Vec<Type>),
    Literal(LiteralValue),
    TypeVar(SmolStr),
    DeclaredTypeVar(Arc<TypeVarDefinition>),
    ParamSpec(Arc<ParameterSpecDefinition>),
    TypeVarTuple(Arc<TuplePackDefinition>),
    /// Internal ordered substitution, including the empty pack.
    TypePack(Vec<Type>),
    Unpack(Box<Type>),
    /// Internal structured substitution value; never a scalar argument type.
    ParameterList(Vec<Param>),
    Concatenate(Vec<Type>, Arc<ParameterSpecDefinition>),
    /// `typing.Self` inside a class body.
    SelfType(ClassRef),
    /// The proxy returned by `super()` / `super(after, receiver)`: attribute
    /// lookups search the MRO of `receiver`'s class after `after` and bind
    /// methods to `receiver` (an instance, or a class in class methods).
    Super {
        after: ClassRef,
        receiver: Box<Type>,
    },
}

impl Type {
    pub fn union(types: Vec<Type>) -> Type {
        let mut flat: Vec<Type> = Vec::new();
        for t in types {
            match t {
                Type::Union(inner) => {
                    for i in inner {
                        if !flat.contains(&i) {
                            flat.push(i);
                        }
                    }
                }
                Type::Unknown => {
                    if !flat.contains(&Type::Unknown) {
                        flat.push(Type::Unknown);
                    }
                }
                other => {
                    if !flat.contains(&other) {
                        flat.push(other);
                    }
                }
            }
        }
        match flat.len() {
            0 => Type::Unknown,
            1 => flat.pop().unwrap(),
            _ => Type::Union(flat),
        }
    }

    pub fn is_unknown(&self) -> bool {
        matches!(self, Type::Unknown)
    }

    pub fn is_none(&self) -> bool {
        matches!(self, Type::None)
    }

    /// Removes `None` from a union (narrowing after `is not None`).
    pub fn without_none(&self) -> Type {
        match self {
            Type::Union(ts) => Type::union(ts.iter().filter(|t| !t.is_none()).cloned().collect()),
            t => t.clone(),
        }
    }

    /// Class of an instance-like type, if any (for a `super()` proxy: the
    /// class of the bound receiver).
    pub fn class_ref(&self) -> Option<&ClassRef> {
        match self {
            Type::Instance(c, _) | Type::Class(c, _) | Type::SelfType(c) => Some(c),
            Type::Literal(LiteralValue::EnumMember { class, .. }) => Some(class),
            Type::Super { receiver, .. } => receiver.class_ref(),
            _ => None,
        }
    }

    pub fn signature(&self) -> Option<&Arc<Signature>> {
        match self {
            Type::Function(s) | Type::BoundMethod(s, _) | Type::Callable(s) => Some(s),
            Type::Overloaded(v) => v.first(),
            _ => None,
        }
    }

    pub fn is_callable(&self) -> bool {
        matches!(
            self,
            Type::Function(_)
                | Type::BoundMethod(..)
                | Type::Callable(_)
                | Type::Overloaded(_)
                | Type::Class(..)
        )
    }

    /// Replaces type variables using `subst`.
    pub fn substitute(&self, subst: &dyn Fn(&str) -> Option<Type>) -> Type {
        match self {
            Type::TypeVar(n) => subst(n).unwrap_or_else(|| self.clone()),
            Type::DeclaredTypeVar(definition) => {
                subst(&definition.key).unwrap_or_else(|| self.clone())
            }
            Type::ParamSpec(definition) => subst(&definition.key).unwrap_or_else(|| self.clone()),
            Type::TypeVarTuple(definition) => {
                subst(&definition.key).unwrap_or_else(|| self.clone())
            }
            Type::TypePack(values) => {
                Type::TypePack(values.iter().map(|ty| ty.substitute(subst)).collect())
            }
            Type::Unpack(inner) => Type::Unpack(Box::new(inner.substitute(subst))),
            Type::ParameterList(params) => Type::ParameterList(
                params
                    .iter()
                    .map(|param| Param {
                        ty: param.ty.substitute(subst),
                        ..param.clone()
                    })
                    .collect(),
            ),
            Type::Concatenate(prefix, definition) => Type::Concatenate(
                prefix.iter().map(|ty| ty.substitute(subst)).collect(),
                definition.clone(),
            ),
            Type::Instance(c, args) => Type::Instance(
                c.clone(),
                args.iter().map(|a| a.substitute(subst)).collect(),
            ),
            Type::Class(c, args) => Type::Class(
                c.clone(),
                args.iter().map(|a| a.substitute(subst)).collect(),
            ),
            Type::Tuple(ts) => {
                let mut values = Vec::new();
                for ty in ts {
                    match ty.substitute(subst) {
                        Type::Unpack(inner) => match *inner {
                            Type::TypePack(pack) => values.extend(pack),
                            other => values.push(Type::Unpack(Box::new(other))),
                        },
                        other => values.push(other),
                    }
                }
                Type::Tuple(values)
            }
            Type::Union(ts) => Type::union(ts.iter().map(|a| a.substitute(subst)).collect()),
            Type::Function(sig) => Type::Function(Arc::new(sig.substitute(subst))),
            Type::Callable(sig) => Type::Callable(Arc::new(sig.substitute(subst))),
            Type::BoundMethod(sig, recv) => Type::BoundMethod(
                Arc::new(sig.substitute(subst)),
                Box::new(recv.substitute(subst)),
            ),
            Type::Overloaded(sigs) => {
                Type::Overloaded(sigs.iter().map(|s| Arc::new(s.substitute(subst))).collect())
            }
            Type::Super { after, receiver } => Type::Super {
                after: after.clone(),
                receiver: Box::new(receiver.substitute(subst)),
            },
            other => other.clone(),
        }
    }
}

impl Signature {
    pub fn substitute(&self, subst: &dyn Fn(&str) -> Option<Type>) -> Signature {
        let mut params = Vec::new();
        for parameter in &self.params {
            let expansion = match &parameter.ty {
                Type::ParamSpec(definition) => Some((&[][..], definition)),
                Type::Concatenate(prefix, definition) => Some((prefix.as_slice(), definition)),
                _ => None,
            };
            if let Some((prefix, definition)) = expansion {
                if let Some(Type::ParameterList(captured)) = subst(&definition.key) {
                    for (index, ty) in prefix.iter().enumerate() {
                        let mut name = format!("__prefix{index}");
                        while captured.iter().any(|param| param.name == name.as_str()) {
                            name.push('_');
                        }
                        params.push(Param {
                            name: name.into(),
                            kind: ParamKind::PositionalOnly,
                            ty: ty.substitute(subst),
                            has_default: false,
                            default_text: None,
                        });
                    }
                    params.extend(captured.iter().map(|param| Param {
                        ty: param.ty.substitute(subst),
                        ..param.clone()
                    }));
                    continue;
                }
            }
            params.push(Param {
                ty: parameter.ty.substitute(subst),
                ..parameter.clone()
            });
        }
        Signature {
            params,
            ret: self.ret.substitute(subst),
            is_async: self.is_async,
            def: self.def.clone(),
            docstring: self.docstring.clone(),
            ret_inferred: self.ret_inferred,
        }
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Unknown => write!(f, "Unknown"),
            Type::Any => write!(f, "Any"),
            Type::Never => write!(f, "Never"),
            Type::LiteralString => write!(f, "LiteralString"),
            Type::LiteralStringValue => write!(f, "str"),
            Type::None => write!(f, "None"),
            Type::Module(_) => write!(f, "module"),
            Type::Class(c, args) => {
                write!(f, "type[{}", c.name)?;
                fmt_args(f, args)?;
                write!(f, "]")
            }
            Type::Instance(c, args) => {
                write!(f, "{}", c.name)?;
                fmt_args(f, args)
            }
            Type::SelfType(c) => write!(f, "Self@{}", c.name),
            Type::Tuple(ts) => {
                write!(f, "tuple[")?;
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{t}")?;
                }
                if ts.is_empty() {
                    write!(f, "()")?;
                }
                write!(f, "]")
            }
            Type::Function(sig) | Type::BoundMethod(sig, _) | Type::Callable(sig) => {
                write!(f, "({}) -> {}", sig.display_params(), sig.ret)
            }
            Type::Overloaded(sigs) => match sigs.first() {
                Some(sig) => write!(
                    f,
                    "({}) -> {} [+{} overloads]",
                    sig.display_params(),
                    sig.ret,
                    sigs.len() - 1
                ),
                None => write!(f, "Overloaded[]"),
            },
            Type::Union(ts) => {
                for (i, t) in ts.iter().enumerate() {
                    if i > 0 {
                        write!(f, " | ")?;
                    }
                    write!(f, "{t}")?;
                }
                Ok(())
            }
            Type::Literal(v) => match v {
                LiteralValue::EnumMember { class, member } => {
                    write!(f, "Literal[{}.{member}]", class.name)
                }
                LiteralValue::Int(i) => write!(f, "Literal[{i}]"),
                LiteralValue::Str(s) => write!(f, "Literal[{s:?}]"),
                LiteralValue::Bytes(s) => write!(f, "Literal[b{s:?}]"),
                LiteralValue::Bool(b) => {
                    write!(f, "Literal[{}]", if *b { "True" } else { "False" })
                }
            },
            Type::TypeVar(n) => write!(f, "{n}"),
            Type::DeclaredTypeVar(definition) => write!(f, "{}", definition.name),
            Type::TypeVarTuple(definition) => write!(f, "{}", definition.name),
            Type::TypePack(values) => {
                write!(f, "Pack[")?;
                for (index, value) in values.iter().enumerate() {
                    if index != 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{value}")?;
                }
                write!(f, "]")
            }
            Type::Unpack(inner) => write!(f, "Unpack[{inner}]"),
            Type::ParamSpec(definition) => write!(f, "{}", definition.name),
            Type::ParameterList(params) => {
                write!(f, "Parameters[")?;
                for (index, param) in params.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: {}", param.name, param.ty)?;
                }
                write!(f, "]")
            }
            Type::Concatenate(prefix, definition) => {
                write!(f, "Concatenate[")?;
                for ty in prefix {
                    write!(f, "{ty}, ")?;
                }
                write!(f, "{}]", definition.name)
            }
            Type::Super { after, .. } => write!(f, "super[{}]", after.name),
        }
    }
}

fn fmt_args(f: &mut fmt::Formatter<'_>, args: &[Type]) -> fmt::Result {
    if args.is_empty() || args.iter().all(|a| matches!(a, Type::Unknown)) {
        return Ok(());
    }
    write!(f, "[")?;
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            write!(f, ", ")?;
        }
        write!(f, "{a}")?;
    }
    write!(f, "]")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cls(name: &str) -> ClassRef {
        ClassRef {
            file: FileId(1),
            range: TextRange::empty(rustpy_core::TextSize::new(0)),
            name: name.into(),
        }
    }

    #[test]
    fn union_flattening_and_display() {
        let int = Type::Instance(cls("int"), vec![]);
        let u = Type::union(vec![
            int.clone(),
            Type::None,
            Type::Union(vec![int.clone(), Type::Instance(cls("str"), vec![])]),
        ]);
        assert_eq!(u.to_string(), "int | None | str");
        assert_eq!(u.without_none().to_string(), "int | str");
        assert_eq!(Type::union(vec![int.clone()]), int);
        let list = Type::Instance(cls("list"), vec![Type::TypeVar("T".into())]);
        let sub = list.substitute(&|n| {
            if n == "T" {
                Some(Type::Instance(cls("int"), vec![]))
            } else {
                None
            }
        });
        assert_eq!(sub.to_string(), "list[int]");
    }

    #[test]
    fn signature_display() {
        let sig = Signature {
            params: vec![
                Param {
                    name: "self".into(),
                    kind: ParamKind::Normal,
                    ty: Type::Unknown,
                    has_default: false,
                    default_text: None,
                },
                Param {
                    name: "x".into(),
                    kind: ParamKind::Normal,
                    ty: Type::Instance(cls("int"), vec![]),
                    has_default: true,
                    default_text: Some("1".into()),
                },
                Param {
                    name: "args".into(),
                    kind: ParamKind::VarArgs,
                    ty: Type::Unknown,
                    has_default: false,
                    default_text: None,
                },
                Param {
                    name: "k".into(),
                    kind: ParamKind::KeywordOnly,
                    ty: Type::Unknown,
                    has_default: false,
                    default_text: None,
                },
            ],
            ret: Type::None,
            is_async: false,
            def: None,
            docstring: None,
            ret_inferred: false,
        };
        assert_eq!(sig.display_params(), "self, x: int = 1, *args, k");
    }
}
