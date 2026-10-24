//! Additional compiler/type contracts. Unknown and open-world values are not
//! evidence of a type error; each check requires a concrete static obligation.
use super::Sink;
use crate::inspections::QuickFix;
use rustpy_core::{Severity, TextRange};
use rustpy_semantic::{
    db::FileInfo,
    infer::Inferer,
    scope::ScopeId,
    types::{ClassRef, LiteralValue, Type},
};
use rustpy_syntax::{
    ast::{self, AstNode},
    SyntaxKind as K, SyntaxNode,
};
use std::sync::Arc;
mod correctness;
mod declarations;
mod formatting;
mod hints;
mod operations;

pub(super) fn run(inf: &Inferer<'_>, file: &Arc<FileInfo>, version: (u32, u32), sink: &mut Sink) {
    if file.text.len() > 2 * 1024 * 1024 {
        return;
    }
    let env = inf.db.resolver.static_env().for_user_code();
    let mut dead_branches = rustpy_semantic::static_cond::DeadBranchCache::new(&env);
    for node in file.parse.syntax_node().descendants().take(100_000) {
        if dead_branches.in_dead_branch(&node) {
            continue;
        }
        operations::node(inf, file, &node, version, sink);
        hints::node(inf, file, &node, version, sink);
        declarations::node(inf, file, &node, sink);
        formatting::node(inf, file, &node, sink);
        correctness::node(inf, file, &node, version, sink);
    }
}
fn warn(node: &SyntaxNode, code: &str, message: impl Into<String>, sink: &mut Sink) {
    sink.push(
        node.text_range(),
        Severity::Warning,
        code,
        message,
        Vec::new(),
    );
}
pub(super) fn known(ty: &Type) -> bool {
    fn inner(ty: &Type, depth: u8, budget: &mut usize) -> bool {
        if *budget == 0 || depth > 16 {
            return false;
        }
        *budget -= 1;
        match ty {
            Type::Unknown
            | Type::Any
            | Type::TypeVar(_)
            | Type::DeclaredTypeVar(_)
            | Type::ParamSpec(_)
            | Type::TypeVarTuple(_) => false,
            Type::Union(v) | Type::Tuple(v) => {
                v.len() <= 128 && v.iter().all(|t| inner(t, depth + 1, budget))
            }
            _ => true,
        }
    }
    inner(ty, 0, &mut 512)
}
fn class(inf: &Inferer<'_>, ty: &Type) -> Option<ClassRef> {
    match ty {
        Type::Class(c, _) | Type::Instance(c, _) | Type::SelfType(c) => Some(c.clone()),
        Type::Literal(LiteralValue::EnumMember { class, .. }) => Some(class.clone()),
        Type::Literal(value) => inf.builtin_class(match value {
            LiteralValue::Int(_) => "int",
            LiteralValue::Str(_) => "str",
            LiteralValue::Bytes(_) => "bytes",
            LiteralValue::Bool(_) => "bool",
            LiteralValue::EnumMember { .. } => return None,
        }),
        Type::LiteralString | Type::LiteralStringValue => inf.builtin_class("str"),
        Type::Tuple(_) => inf.builtin_class("tuple"),
        Type::None => inf
            .builtin_class("NoneType")
            .or_else(|| inf.builtin_class("object")),
        _ => None,
    }
}
fn closed(inf: &Inferer<'_>, ty: &Type) -> bool {
    // Synthetic functools values (`partial(f, ...)`) are callable wrappers without a
    // declared class namespace; their call contract is checked by inference.
    known(ty)
        && !inf.is_functools_value(ty)
        && class(inf, ty).is_some_and(|c| !inf.has_unknown_base(&c))
}
/// Type namespace lookup for implicit protocols; instance __getattribute__ is
/// deliberately irrelevant. Only presence is used when no public bound-slot API exists.
fn slot(inf: &Inferer<'_>, ty: &Type, name: &str) -> Option<bool> {
    let c = protocol_class(inf, ty)?;
    if inf.has_unknown_base(&c) {
        return None;
    }
    if inf
        .synth_spec(&c)
        .is_some_and(|s| s.kind == rustpy_semantic::infer::SynthKind::TypedDict)
    {
        return slot(inf, &inf.instance_of("dict"), name);
    }
    for owner in inf.mro(&c).iter() {
        if let Some(info) = inf.class_info(owner) {
            if let Some(member) = info.members.get(name) {
                if let (Some(file), Some(range)) = (inf.db.file(member.file), member.value) {
                    if let Some(value) = rustpy_semantic::edit_helpers::node_with_range(
                        &file.parse.syntax_node(),
                        range,
                    ) {
                        if value.text().to_string().trim() == "None" {
                            return Some(false);
                        }
                    }
                }
                return Some(true);
            }
        }
    }
    // `@dataclass(order=True)`, attrs ordering, `functools.total_ordering`.
    if inf
        .synthesized_ordering_member(&Type::Class(c.clone(), Vec::new()), name)
        .is_some()
    {
        return Some(true);
    }
    if inf
        .synth_spec(&c)
        .is_some_and(|s| s.kind == rustpy_semantic::infer::SynthKind::NamedTuple)
    {
        return slot(inf, &inf.instance_of("tuple"), name);
    }
    Some(false)
}
fn protocol_class(inf: &Inferer<'_>, receiver: &Type) -> Option<ClassRef> {
    let class = class(inf, receiver)?;
    if !matches!(receiver, Type::Class(..)) {
        return Some(class);
    }
    if inf.has_unknown_base(&class) {
        return None;
    }
    let mut selected = inf.builtin_class("type")?;
    for owner in inf.mro(&class).iter().take(32) {
        let info = inf.class_info(owner)?;
        for (name, range) in &info.keywords {
            if name != "metaclass" {
                continue;
            }
            let file = inf.db.file(owner.file)?;
            let expression =
                rustpy_semantic::edit_helpers::node_with_range(&file.parse.syntax_node(), *range)
                    .and_then(ast::Expr::cast)?;
            let Type::Class(candidate, _) = inf.type_of_expr(&file, &expression) else {
                return None;
            };
            if inf.mro(&candidate).contains(&selected) {
                selected = candidate;
            } else if !inf.mro(&selected).contains(&candidate) {
                return None;
            }
        }
    }
    Some(selected)
}
fn canonical(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    expr: &ast::Expr,
    scope: ScopeId,
) -> Option<(String, String)> {
    let definition = match expr.clone().unparen() {
        ast::Expr::Name(n) => inf.resolve_name(file, scope, &n.name_text()),
        ast::Expr::Attribute(a) => {
            let object = a.object()?;
            let Type::Module(id) = inf.type_of_expr(file, &object) else {
                return None;
            };
            let owner = inf.db.file(id)?;
            inf.resolve_in_module(&owner, &a.attr_text(), 0)
        }
        _ => None,
    }?;
    let definition = inf.follow_imports(definition);
    let owner = &definition.file;
    let module = owner.module_name.as_ref()?;
    if !owner.is_stub || !owner.path.starts_with("<typeshed>") {
        return None;
    }
    Some((module.clone(), definition.binding().name.to_string()))
}
fn builtin(inf: &Inferer<'_>, file: &Arc<FileInfo>, expr: &ast::Expr, name: &str) -> bool {
    canonical(
        inf,
        file,
        expr,
        file.scopes.scope_at(expr.text_range().start()),
    )
    .is_some_and(|(m, n)| m == "builtins" && n == name)
}
fn typing(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    expr: &ast::Expr,
    scope: ScopeId,
    name: &str,
) -> bool {
    canonical(inf, file, expr, scope)
        .is_some_and(|(m, n)| matches!(m.as_str(), "typing" | "typing_extensions") && n == name)
}
fn own(node: &SyntaxNode, owner: &SyntaxNode) -> bool {
    !node
        .ancestors()
        .take_while(|a| a != owner)
        .any(|a| matches!(a.kind(), K::FUNC_DEF | K::LAMBDA_EXPR | K::CLASS_DEF))
}
fn protocol(inf: &Inferer<'_>, class: &ClassRef) -> bool {
    let Some(info) = inf.class_info(class) else {
        return false;
    };
    if !info.is_protocol {
        return false;
    }
    let Some(file) = inf.db.file(class.file) else {
        return false;
    };
    info.base_exprs
        .iter()
        .filter_map(|range| {
            rustpy_semantic::edit_helpers::node_with_range(&file.parse.syntax_node(), *range)
        })
        .filter_map(ast::Expr::cast)
        .any(|expr| {
            let head = match expr {
                ast::Expr::Subscript(s) => s.object(),
                e => Some(e),
            };
            head.is_some_and(|head| {
                typing(
                    inf,
                    &file,
                    &head,
                    file.scopes.scope_at(head.text_range().start()),
                    "Protocol",
                )
            })
        })
}
fn slot_return(inf: &Inferer<'_>, receiver: &Type, name: &str) -> Option<Type> {
    let class = protocol_class(inf, receiver)?;
    if inf.has_unknown_base(&class) {
        return None;
    }
    for owner in inf.mro(&class).iter() {
        let info = inf.class_info(owner)?;
        if let Some(member) = info.members.get(name) {
            if !matches!(
                member.kind,
                rustpy_semantic::classes::MemberKind::Method
                    | rustpy_semantic::classes::MemberKind::StaticMethod
                    | rustpy_semantic::classes::MemberKind::ClassMethod
            ) {
                return None;
            }
            let source = inf.db.file(member.file)?;
            let function = source
                .parse
                .syntax_node()
                .descendants()
                .filter_map(ast::FuncDef::cast)
                .find(|f| f.name().is_some_and(|n| n.text_range() == member.range))?;
            if function.decorators().next().is_some() {
                return None;
            }
            let signature = inf.signature(&source, &function, Some(owner));
            return Some(signature.ret.clone());
        }
    }
    None
}
pub(super) fn typed_dict_error(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    value: &ast::Expr,
    expected: &Type,
) -> Option<String> {
    if inf.typed_dict_literal_compatible(file, value, expected) != Some(false) {
        return None;
    }
    let Type::Instance(class, args) = expected else {
        return None;
    };
    let fields = inf.typed_dict_fields(class, args)?;
    let ast::Expr::Dict(dict) = value.clone().unparen() else {
        return None;
    };
    let mut present = std::collections::HashSet::new();
    let mut details = Vec::new();
    for item in dict.items() {
        let key = inf.typed_dict_key(file, &item.key()?)?;
        present.insert(key.clone());
        if let Some((_, ty, _, _)) = fields.iter().find(|(name, _, _, _)| name == &key) {
            let actual = inf.type_of_expr(file, &item.value()?);
            if known(&actual) && !inf.is_compatible(&actual, ty) {
                details.push(format!("key '{key}' expects {ty}, got {actual}"));
            }
        } else {
            details.push(format!("unknown key '{key}'"));
        }
    }
    for (name, _, required, _) in fields {
        if required && !present.contains(&name) {
            details.push(format!("missing required key '{name}'"));
        }
    }
    Some(format!(
        "TypedDict '{}': {}",
        class.name,
        if details.is_empty() {
            "incompatible dictionary literal".into()
        } else {
            details.join("; ")
        }
    ))
}
pub(super) fn protocol_detail(inf: &Inferer<'_>, actual: &Type, expected: &Type) -> Option<String> {
    let Type::Instance(class, _) = expected else {
        return None;
    };
    if !protocol(inf, class) {
        return None;
    }
    let mut missing = Vec::new();
    for owner in inf.mro(class).iter().take(32) {
        if let Some(info) = inf.class_info(owner) {
            for name in info.members.keys().filter(|n| !n.starts_with('_')) {
                if inf.lookup_member(actual, name).is_none() && !missing.contains(name) {
                    missing.push(name.clone());
                    if missing.len() >= 8 {
                        break;
                    }
                }
            }
        }
    }
    if missing.is_empty() {
        None
    } else {
        Some(format!(
            "missing protocol member(s): {}",
            missing
                .iter()
                .map(|n| n.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}
