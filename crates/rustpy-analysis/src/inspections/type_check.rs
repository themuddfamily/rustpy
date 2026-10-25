//! Call argument checks (`PyArgumentList`) and simple type compatibility
//! checks (`PyTypeChecker`): arguments, literal returns, annotated
//! assignments, and attribute access on variables that are only ever `None`.

use std::sync::Arc;

use rustpy_core::Severity;
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::{SyntaxKind, SyntaxNode};

use super::Sink;
use rustpy_semantic::db::FileInfo;
use rustpy_semantic::infer::{Inferer, SynthKind};
use rustpy_semantic::scope::BindingKind;
use rustpy_semantic::types::{LiteralValue, ParamKind, Signature, Type};

mod advanced;
mod keyword_unpack;

const ARGS: &str = "PyArgumentList";
const TYPES: &str = "PyTypeChecker";

/// Decorators that keep the decorated function's call signature.
const SIGNATURE_PRESERVING: &[&str] = &[
    "staticmethod",
    "classmethod",
    "abstractmethod",
    "abc.abstractmethod",
    "overload",
    "typing.overload",
    "lru_cache",
    "functools.lru_cache",
    "cache",
    "functools.cache",
    "wraps",
    "functools.wraps",
    "final",
    "typing.final",
    "override",
    "typing.override",
    "property",
    "contextmanager",
    "contextlib.contextmanager",
    "asynccontextmanager",
    "contextlib.asynccontextmanager",
];

pub(super) fn run(inf: &Inferer<'_>, file: &Arc<FileInfo>, version: (u32, u32), sink: &mut Sink) {
    let db = inf.db;
    let root = file.parse.syntax_node();
    let env = if file.is_stub {
        db.resolver.static_env()
    } else {
        db.resolver.static_env().for_user_code()
    };
    for (index, node) in root.descendants().enumerate() {
        // Inference observes cancellation, but this walk also performs syntax
        // and declaration checks. Do not finish the whole file after an edit.
        if index % 64 == 0 && rustpy_semantic::infer::ambient_cancelled() {
            return;
        }
        // Version/platform branches that cannot run on the target interpreter.
        if matches!(
            node.kind(),
            SyntaxKind::CALL_EXPR
                | SyntaxKind::FUNC_DEF
                | SyntaxKind::ANN_ASSIGN_STMT
                | SyntaxKind::ATTRIBUTE_EXPR
        ) && rustpy_semantic::static_cond::in_dead_branch(&node, &env)
        {
            continue;
        }
        match node.kind() {
            SyntaxKind::CALL_EXPR => check_call(inf, file, &node, sink),
            SyntaxKind::FUNC_DEF => check_returns(inf, file, &node, sink),
            SyntaxKind::ANN_ASSIGN_STMT => check_ann_assign(inf, file, &node, sink),
            SyntaxKind::ATTRIBUTE_EXPR => check_none_attribute(inf, file, &node, sink),
            _ => {}
        }
    }
    advanced::run(inf, file, version, sink);
}

/// A checkable signature plus the names of its explicitly annotated
/// parameters (types inferred from default values are hints only).
struct Checkable {
    sig: Arc<Signature>,
    annotated: Vec<String>,
    /// TypedDict constructors also accept a positional mapping (not modelled).
    mapping_arg: bool,
}

/// Signature to check a call against, if it is known precisely enough.
fn callable_signature(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    callee: &ast::Expr,
    callee_ty: &Type,
    pos: &[Type],
    kw: &[(smol_str::SmolStr, Type)],
    construction_override: Option<&Type>,
) -> Option<Checkable> {
    // `X.__new__(cls, ...)`, `Base.__init__(self, ...)` and friends bind
    // differently from ordinary attribute access.
    let attr_name = match callee {
        ast::Expr::Attribute(a) => Some(a.attr_text()),
        _ => None,
    };
    if attr_name
        .as_deref()
        .is_some_and(|n| n.starts_with("__") && n.ends_with("__"))
    {
        return None;
    }
    let sig = match callee_ty {
        Type::Function(s) | Type::BoundMethod(s, _) | Type::Callable(s) => s.clone(),
        Type::Class(c, args) => {
            let info = inf.class_info(c)?;
            if info.is_enum_like || info.is_protocol {
                return None;
            }
            let dispatch = construction_override
                .cloned()
                .or_else(|| inf.construction_signature(c, args, pos, kw))?;
            let dispatch_sig = match &dispatch {
                Type::BoundMethod(signature, _) | Type::Function(signature) => signature.clone(),
                Type::Overloaded(signatures) => inf.select_overload(signatures, pos, kw),
                _ => return None,
            };
            // Generated `__init__`: checked for the stdlib forms, whose fields
            // are fully known when every base class resolves.
            if let Some((sig, kind)) = inf
                .generated_constructor(c)
                .filter(|_| construction_override.is_none())
            {
                if !matches!(
                    kind,
                    SynthKind::Dataclass | SynthKind::NamedTuple | SynthKind::TypedDict
                ) || inf.has_unknown_base_ignoring(c, &["TypedDict"])
                {
                    return None;
                }
                let annotated = sig
                    .params
                    .iter()
                    .filter(|p| !p.ty.is_unknown())
                    .map(|p| p.name.to_string())
                    .collect();
                return Some(Checkable {
                    sig,
                    annotated,
                    mapping_arg: kind == SynthKind::TypedDict,
                });
            }
            if info.is_dataclass || info.keywords.iter().any(|(name, _)| name != "metaclass") {
                return None;
            }
            if inf.mro(c).iter().any(|k| {
                matches!(
                    k.name.as_str(),
                    "NamedTuple" | "TypedDict" | "Enum" | "type" | "Protocol"
                )
            }) {
                return None;
            }
            dispatch_sig
        }
        _ => return None,
    };
    // A declared/captured Callable has no source function to mine annotations
    // from. Its expanded parameter types are themselves the contract. Never
    // interpret internal unresolved pack markers as ordinary argument slots.
    let declared_callable = matches!(callee_ty, Type::Callable(_)) && sig.def.is_none();
    if declared_callable {
        // Callable[..., R] is represented as one unknown *args slot, not a
        // closed signature that forbids keyword arguments.
        if sig.params.len() == 1
            && sig.params[0].kind == ParamKind::VarArgs
            && sig.params[0].ty.is_unknown()
        {
            return None;
        }
        let mut remaining = 512;
        if sig
            .params
            .iter()
            .any(|parameter| unresolved_callable_parameter(&parameter.ty, 0, &mut remaining))
        {
            return None;
        }
    }
    // Decorated functions may have a different runtime signature.
    let mut annotated: Vec<String> = if declared_callable {
        sig.params
            .iter()
            .filter(|parameter| !parameter.ty.is_unknown())
            .map(|parameter| parameter.name.to_string())
            .collect()
    } else {
        Vec::new()
    };
    if let Some(def) = &sig.def {
        let f = inf.db.file(def.file)?;
        let node =
            rustpy_semantic::edit_helpers::node_with_range(&f.parse.syntax_node(), def.range)
                .and_then(ast::FuncDef::cast)?;
        for d in node.decorators() {
            let name = d.name_text().unwrap_or_default();
            if !SIGNATURE_PRESERVING.contains(&name.as_str()) {
                return None;
            }
        }
        if let Some(params) = node.params() {
            for p in params.params() {
                if let (Some(n), Some(_)) = (p.name(), p.annotation()) {
                    annotated.push(n.text().to_string());
                }
            }
        }
        // A function reached through an alias (`_randbelow = _randbelow_with_getrandbits`,
        // `rb = self._randbelow`) may bind differently; its signature is not reliable.
        let accessed = match callee {
            ast::Expr::Attribute(a) => Some(a.attr_text()),
            ast::Expr::Name(n) => Some(n.name_text()),
            _ => None,
        };
        if let Some(name) = accessed {
            if !matches!(callee_ty, Type::Class(..))
                && node.name_text() != name
                && !stable_imported_function_alias(inf, file, callee, callee_ty, &sig)
            {
                return None;
            }
        }
    }
    // Receivers whose type may be narrowed at runtime (`isinstance` branches)
    // make method signatures unreliable; only trust simple receivers.
    if let ast::Expr::Attribute(a) = callee {
        let obj = a.object().map(|o| o.unparen())?;
        let ok = match &obj {
            ast::Expr::Name(n) => {
                matches!(n.name_text().as_str(), "self" | "cls") || single_binding(inf, file, &obj)
            }
            ast::Expr::Call(_)
            | ast::Expr::Literal(_)
            | ast::Expr::String(_)
            | ast::Expr::List(_)
            | ast::Expr::Dict(_)
            | ast::Expr::Set(_)
            | ast::Expr::Tuple(_) => true,
            _ => false,
        };
        if !ok {
            return None;
        }
    }
    Some(Checkable {
        sig,
        annotated,
        mapping_arg: false,
    })
}

/// A plain imported function does not bind a new receiver when renamed.
/// Keep assignment/member aliases conservative; use the actual declaration
/// identity behind one stable import, never an alias spelling or signature alone.
fn stable_imported_function_alias(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    callee: &ast::Expr,
    callee_ty: &Type,
    signature: &Signature,
) -> bool {
    if !matches!(callee_ty, Type::Function(_))
        || file.text.len() > 1024 * 1024
        || file.scopes.bindings.len() > 4096
        || file.scopes.scopes.len() > 4096
    {
        return false;
    }
    let revision = inf.db.revision_excluding(&file.path);
    if !inf
        .db
        .file(file.id)
        .is_some_and(|current| Arc::ptr_eq(&current, file))
    {
        return false;
    }
    let ast::Expr::Name(name) = callee else {
        return false;
    };
    if u32::from(name.text_range().len()) > 256 || !single_binding(inf, file, callee) {
        return false;
    }
    let Some(reference) = file.scopes.reference_at(name.text_range().start()) else {
        return false;
    };
    if reference.is_write || reference.range != name.text_range() {
        return false;
    }
    let Some(id) = reference.resolved else {
        return false;
    };
    let binding = file.scopes.binding(id);
    let BindingKind::Import(import) = &binding.kind else {
        return false;
    };
    if import.name.is_none()
        || binding.name != reference.name
        || file.scopes.bindings_in(binding.scope, &binding.name).len() != 1
    {
        return false;
    }
    let Some(definition) = &signature.def else {
        return false;
    };
    let mut target = rustpy_semantic::infer::Definition {
        file: file.clone(),
        binding: id,
    };
    let mut members = Vec::new();
    for _ in 0..16 {
        let BindingKind::Import(import) = &target.binding().kind else {
            break;
        };
        let Some(member) = &import.name else {
            return false;
        };
        if member.len() > 256 {
            return false;
        }
        members.push(member.to_string());
        let Some(module) = inf.import_target_module(&target) else {
            return false;
        };
        let Some(next) = inf.resolve_in_module(&module, member, 0) else {
            return false;
        };
        target = next;
    }
    let actual = target.binding();
    imported_member_unwritten(inf, &members)
        && matches!(actual.kind, BindingKind::Function)
        && target.file.scopes.scope(actual.scope).kind == rustpy_semantic::scope::ScopeKind::Module
        && target.file.id == definition.file
        && actual.node_range == definition.range
        && actual.name == definition.name
        && inf.db.revision_excluding(&file.path) == revision
        && inf
            .db
            .file(file.id)
            .is_some_and(|current| Arc::ptr_eq(&current, file))
}

/// Retire a physical import signature when any captured executable source can
/// replace that member. Receiver alias analysis is deliberately unnecessary:
/// a same-spelled member write anywhere in the bounded snapshot is conservative.
fn imported_member_unwritten(inf: &Inferer<'_>, members: &[String]) -> bool {
    let Some(files) = inf.db.files_with_limit(4096) else {
        return false;
    };
    let mut files_left = 256usize;
    let mut bytes_left = 4 * 1024 * 1024usize;
    let mut nodes_left = 65_536usize;
    for file in files {
        // Stub declarations are not executed mutation sites.
        if file.is_stub {
            continue;
        }
        if files_left == 0 || file.text.len() > bytes_left {
            return false;
        }
        files_left -= 1;
        bytes_left -= file.text.len();
        for node in file.parse.syntax_node().descendants() {
            if nodes_left == 0 {
                return false;
            }
            nodes_left -= 1;
            if let Some(assignment) = ast::AssignStmt::cast(node.clone()) {
                // The final expression is the RHS, never a store target.
                let mut expressions = assignment
                    .syntax()
                    .children()
                    .filter_map(ast::Expr::cast)
                    .peekable();
                while let Some(target) = expressions.next() {
                    if expressions.peek().is_some()
                        && !imported_target_unwritten(&target, members, &mut nodes_left)
                    {
                        return false;
                    }
                }
                continue;
            }
            let target = if let Some(assignment) = ast::AnnAssignStmt::cast(node.clone()) {
                // Bare annotations evaluate but do not replace the member.
                if assignment.value().is_some() {
                    assignment.target()
                } else {
                    None
                }
            } else if let Some(assignment) = ast::AugAssignStmt::cast(node.clone()) {
                assignment.target()
            } else if let Some(deletion) = ast::DelStmt::cast(node.clone()) {
                for target in deletion.syntax().children().filter_map(ast::Expr::cast) {
                    if !imported_target_unwritten(&target, members, &mut nodes_left) {
                        return false;
                    }
                }
                continue;
            } else if let Some(iteration) = ast::ForStmt::cast(node.clone()) {
                iteration.target()
            } else if let Some(item) = ast::WithItem::cast(node.clone()) {
                item.target()
            } else if let Some(iteration) = ast::CompFor::cast(node) {
                iteration.target()
            } else {
                None
            };
            if let Some(target) = target {
                if !imported_target_unwritten(&target, members, &mut nodes_left) {
                    return false;
                }
            }
        }
    }
    true
}

fn imported_target_unwritten(
    target: &ast::Expr,
    members: &[String],
    remaining: &mut usize,
) -> bool {
    for node in target.syntax().descendants() {
        if *remaining == 0 {
            return false;
        }
        *remaining -= 1;
        if ast::AttributeExpr::cast(node).is_some_and(|attribute| {
            members
                .iter()
                .any(|member| attribute.attr_text() == *member)
        }) {
            return false;
        }
    }
    true
}

/// Internal variadic markers require a genuine structured capture before the
/// ordinary call binder can assign positional and keyword argument slots.
fn unresolved_callable_parameter(ty: &Type, depth: u8, remaining: &mut usize) -> bool {
    if depth >= 16 || *remaining == 0 {
        return true;
    }
    *remaining -= 1;
    match ty {
        Type::ParamSpec(_)
        | Type::Concatenate(..)
        | Type::TypeVarTuple(_)
        | Type::TypePack(_)
        | Type::ParameterList(_)
        | Type::Unpack(_) => true,
        Type::Tuple(values)
        | Type::Union(values)
        | Type::Class(_, values)
        | Type::Instance(_, values) => values
            .iter()
            .any(|value| unresolved_callable_parameter(value, depth + 1, remaining)),
        Type::Callable(signature) | Type::Function(signature) | Type::BoundMethod(signature, _) => {
            signature
                .params
                .iter()
                .any(|parameter| unresolved_callable_parameter(&parameter.ty, depth + 1, remaining))
                || unresolved_callable_parameter(&signature.ret, depth + 1, remaining)
        }
        Type::Overloaded(signatures) => {
            signatures.iter().any(|signature| {
                signature.params.iter().any(|parameter| {
                    unresolved_callable_parameter(&parameter.ty, depth + 1, remaining)
                }) || unresolved_callable_parameter(&signature.ret, depth + 1, remaining)
            })
        }
        _ => false,
    }
}

/// True when some scope declares `name` global (its module value may change at runtime).
fn declared_global(file: &FileInfo, name: &str) -> bool {
    file.scopes
        .scopes
        .iter()
        .any(|s| s.globals.iter().any(|g| g == name) || s.nonlocals.iter().any(|g| g == name))
}

/// True when exactly one binding of `expr` (a name) reaches it, so its
/// inferred type is precise (flow-sensitive; names bound once in their
/// scope where flow analysis does not apply).
fn single_binding(inf: &Inferer<'_>, file: &FileInfo, expr: &ast::Expr) -> bool {
    let ast::Expr::Name(n) = expr else {
        return false;
    };
    if std::env::var("RUSTPY_TC_GATE").as_deref() != Ok("base") {
        if let Some(ids) = inf.reaching_bindings(file, n) {
            return ids.len() == 1 && !declared_global(file, &n.name_text());
        }
    }
    let Some(r) = file.scopes.reference_at(n.text_range().start()) else {
        return false;
    };
    let Some(bid) = r.resolved else { return false };
    let b = file.scopes.binding(bid);
    file.scopes.bindings_in(b.scope, &b.name).len() == 1 && !declared_global(file, &b.name)
}

/// Argument expressions whose inferred type is trustworthy: literals and
/// displays, names bound exactly once in their scope, and constructor calls.
fn precise_arg(inf: &Inferer<'_>, file: &Arc<FileInfo>, arg: &ast::Expr) -> bool {
    let e = arg.clone().unparen();
    if is_literal_like(&e) {
        return true;
    }
    match &e {
        ast::Expr::Name(_) => single_binding(inf, file, &e),
        ast::Expr::Subscript(subscript) => {
            let Some(object) = subscript.object() else {
                return false;
            };
            let object = object.unparen();
            if !matches!(object, ast::Expr::Name(_)) || !single_binding(inf, file, &object) {
                return false;
            }
            let Type::Tuple(elements) = inf.type_of_expr(file, &object) else {
                return false;
            };
            if elements.len() > 256 {
                return false;
            }
            let Some(index) = subscript.index_expr() else {
                return false;
            };
            let index = index.unparen();
            let literal_integer = match &index {
                ast::Expr::Literal(literal) => literal.kind() == Some(SyntaxKind::INT_NUMBER),
                ast::Expr::Unary(unary) => {
                    matches!(unary.op().map(|token| token.kind()), Some(SyntaxKind::PLUS | SyntaxKind::MINUS))
                        && unary.operand().is_some_and(|operand| matches!(operand,
                            ast::Expr::Literal(literal) if literal.kind() == Some(SyntaxKind::INT_NUMBER)))
                }
                _ => false,
            };
            if !literal_integer {
                return false;
            }
            let Type::Literal(LiteralValue::Int(index)) = inf.type_of_expr(file, &index) else {
                return false;
            };
            let index = if index < 0 {
                (elements.len() as i64).checked_add(index)
            } else {
                Some(index)
            };
            index.is_some_and(|index| index >= 0 && (index as usize) < elements.len())
        }
        ast::Expr::Call(c) => {
            c.callee()
                .is_some_and(|callee| match inf.type_of_expr(file, &callee) {
                    Type::Class(..) => true,
                    Type::Function(signature)
                    | Type::Callable(signature)
                    | Type::BoundMethod(signature, _) => advanced::known(&signature.ret),
                    _ => false,
                })
        }
        _ => false,
    }
}

fn check_call(inf: &Inferer<'_>, file: &Arc<FileInfo>, node: &SyntaxNode, sink: &mut Sink) {
    if let Some(call) = ast::CallExpr::cast(node.clone()) {
        if let Some(callee) = call.callee() {
            if let Type::Class(class, args) = inf.type_of_expr(file, &callee) {
                let (pos, kw) = inf.call_args(file, &call);
                let checks = inf.construction_checks(&class, &args, &pos, &kw);
                if checks.len() > 1 {
                    let first = sink.out.len();
                    for signature in &checks {
                        check_call_stage(inf, file, node, sink, Some(signature));
                    }
                    // Equal diagnostics from the same call are one finding; different
                    // ranges/messages remain independent stage constraints.
                    let findings = sink.out.split_off(first);
                    for finding in findings {
                        if !sink.out[first..]
                            .iter()
                            .any(|existing| existing.diagnostic == finding.diagnostic)
                        {
                            sink.out.push(finding);
                        }
                    }
                    return;
                }
            }
        }
    }
    check_call_stage(inf, file, node, sink, None);
}
fn check_call_stage(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    node: &SyntaxNode,
    sink: &mut Sink,
    construction_override: Option<&Type>,
) {
    let Some(call) = ast::CallExpr::cast(node.clone()) else {
        return;
    };
    let (Some(callee), Some(arg_list)) = (call.callee(), call.arg_list()) else {
        return;
    };
    // `super().__init__(...)` and class bases are checked loosely by inference; skip decorators too.
    if node
        .parent()
        .is_some_and(|p| p.kind() == SyntaxKind::DECORATOR)
    {
        return;
    }
    let callee_ty = inf.type_of_expr(file, &callee);
    let (call_pos, call_kw) = inf.call_args(file, &call);
    let Some(mut checkable) = callable_signature(
        inf,
        file,
        &callee,
        &callee_ty,
        &call_pos,
        &call_kw,
        construction_override,
    ) else {
        return;
    };
    let expanded = match keyword_unpack::expand(inf, &mut checkable) {
        Ok(expanded) => expanded,
        Err(()) => return,
    };
    if expanded {
        keyword_unpack::check_stars(inf, file, &arg_list, &checkable, sink);
    }
    let Checkable {
        sig,
        annotated,
        mapping_arg,
    } = checkable;
    let typed = |p: &rustpy_semantic::types::Param| annotated.iter().any(|a| *a == p.name.as_str());
    let mut positional: Vec<ast::Expr> = Vec::new();
    let mut keywords: Vec<ast::KeywordArg> = Vec::new();
    let mut has_star = false;
    let mut has_star_star = false;
    for a in arg_list.args() {
        match a {
            ast::AnyArg::Positional(p) => {
                if !keywords.is_empty() || has_star_star {
                    // Misordered arguments are reported as a syntax-level problem.
                    return;
                }
                if let Some(e) = p.expr() {
                    positional.push(e);
                }
            }
            ast::AnyArg::Generator(g) => positional.push(ast::Expr::Generator(g)),
            ast::AnyArg::Keyword(k) => keywords.push(k),
            ast::AnyArg::Star(_) => has_star = true,
            ast::AnyArg::StarStar(_) => has_star_star = true,
        }
    }
    if mapping_arg && (!positional.is_empty() || has_star) {
        return;
    }
    let params = &sig.params;
    let positional_slots: Vec<usize> = params
        .iter()
        .enumerate()
        .filter(|(_, p)| matches!(p.kind, ParamKind::PositionalOnly | ParamKind::Normal))
        .map(|(i, _)| i)
        .collect();
    let varargs = params.iter().find(|p| p.kind == ParamKind::VarArgs);
    let kwargs = params.iter().find(|p| p.kind == ParamKind::KwArgs);
    let mut filled = vec![false; params.len()];
    for (i, arg) in positional.iter().enumerate() {
        if let Some(&slot) = positional_slots.get(i) {
            filled[slot] = true;
            if typed(&params[slot]) {
                check_arg_type(inf, file, arg, &params[slot].ty, sink);
            }
        } else if let Some(v) = varargs {
            if typed(v) {
                check_arg_type(inf, file, arg, &v.ty, sink);
            }
        } else if !has_star {
            sink.push(
                arg.text_range(),
                Severity::Error,
                ARGS,
                "Unexpected argument",
                Vec::new(),
            );
        }
    }
    let mut seen_kw: Vec<String> = Vec::new();
    for k in &keywords {
        let name = k.name_text();
        if seen_kw.contains(&name) {
            // Reported as a syntax-level problem by the argument-list check.
            continue;
        }
        seen_kw.push(name.clone());
        match params.iter().position(|p| {
            p.name == name.as_str() && matches!(p.kind, ParamKind::Normal | ParamKind::KeywordOnly)
        }) {
            Some(j) => {
                if filled[j] {
                    sink.push(
                        k.text_range(),
                        Severity::Error,
                        ARGS,
                        format!("Multiple values for parameter '{name}'"),
                        Vec::new(),
                    );
                } else {
                    filled[j] = true;
                    if let Some(v) = k.value() {
                        if typed(&params[j]) {
                            check_arg_type(inf, file, &v, &params[j].ty, sink);
                        }
                    }
                }
            }
            None => {
                if params
                    .iter()
                    .any(|p| p.name == name.as_str() && p.kind == ParamKind::PositionalOnly)
                {
                    sink.push(k.text_range(), Severity::Error, ARGS, format!("Positional-only parameter '{name}' cannot be passed as a keyword argument"), Vec::new());
                } else if let Some(kw) = kwargs {
                    if let Some(v) = k.value() {
                        if typed(kw) {
                            check_arg_type(inf, file, &v, &kw.ty, sink);
                        }
                    }
                } else if !has_star_star {
                    sink.push(
                        k.text_range(),
                        Severity::Error,
                        ARGS,
                        format!("Unexpected argument '{name}'"),
                        Vec::new(),
                    );
                }
            }
        }
    }
    if !has_star_star {
        let missing: Vec<&str> = params
            .iter()
            .enumerate()
            .filter(|(i, p)| !filled[*i] && !p.has_default && !p.name.is_empty())
            .filter(|(_, p)| match p.kind {
                ParamKind::KeywordOnly => true,
                ParamKind::PositionalOnly | ParamKind::Normal => !has_star,
                _ => false,
            })
            .map(|(_, p)| p.name.as_str())
            .collect();
        if !missing.is_empty() {
            let msg = if missing.len() == 1 {
                format!("Parameter '{}' unfilled", missing[0])
            } else {
                format!(
                    "Parameters {} unfilled",
                    missing
                        .iter()
                        .map(|m| format!("'{m}'"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            sink.push(
                arg_list.text_range(),
                Severity::Error,
                ARGS,
                msg,
                Vec::new(),
            );
        }
    }
}

fn check_arg_type(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    arg: &ast::Expr,
    param: &Type,
    sink: &mut Sink,
) {
    // Never is a precise empty argument domain when supplied by semantic
    // annotation resolution. Admit it here without expanding the independent
    // return/assignment inspection policy or trusting a source-spelled name.
    let checkable = matches!(param, Type::Never) || is_simple_param(inf, param);
    if !checkable || !precise_arg(inf, file, arg) {
        return;
    }
    if inf.typed_dict_literal_compatible(file, arg, param) == Some(true) {
        return;
    }
    if let Some(message) = advanced::typed_dict_error(inf, file, arg, param) {
        sink.push(
            arg.text_range(),
            Severity::Warning,
            "PyTypedDict",
            message,
            Vec::new(),
        );
        return;
    }
    let arg_ty = inf.type_of_expr(file, arg);
    if let Some(msg) = incompatible_message_checked(inf, &arg_ty, param, checkable) {
        let msg = if let Some(detail) = advanced::protocol_detail(inf, &arg_ty, param) {
            format!("{msg}; {detail}")
        } else {
            msg
        };
        sink.push(arg.text_range(), Severity::Warning, TYPES, msg, Vec::new());
    }
}

const CONCRETE_BUILTINS: &[&str] = &[
    "int",
    "float",
    "complex",
    "bool",
    "str",
    "bytes",
    "bytearray",
    "list",
    "dict",
    "set",
    "frozenset",
    "tuple",
];

// Project protocols are checkable only when the semantic target has a
// canonical Protocol base; a source-spelled lookalike remains conservative.
fn canonical_protocol_param(inf: &Inferer<'_>, class: &rustpy_semantic::types::ClassRef) -> bool {
    let (Some(info), Some(file)) = (inf.class_info(class), inf.db.file(class.file)) else {
        return false;
    };
    info.base_exprs.iter().any(|range| {
        let expression = file
            .parse
            .syntax_node()
            .descendants()
            .find(|node| node.text_range() == *range && ast::Expr::can_cast(node.kind()))
            .and_then(ast::Expr::cast);
        expression
            .and_then(|expression| inf.symbol_origin(&file, &expression))
            .is_some_and(|(module, name)| {
                name == "Protocol"
                    && matches!(module.as_str(), "typing" | "typing_extensions")
                    && inf.db.module(&module).is_some_and(|module_file| {
                        module_file.is_stub
                            && module_file.path
                                == std::path::Path::new(&format!("<typeshed>/{module}.pyi"))
                    })
            })
    })
}

/// Parameter types precise enough to report mismatches against: concrete
/// builtin classes, classes defined in project files, `None`, and unions of
/// those. Canonical project Protocols are checked through semantic member
/// compatibility; protocols and ABCs from stubs remain skipped.
fn is_simple_param(inf: &Inferer<'_>, t: &Type) -> bool {
    simple_param_bounded(inf, t, 0, &mut Vec::new())
}

fn simple_param_bounded(
    inf: &Inferer<'_>,
    t: &Type,
    depth: usize,
    active: &mut Vec<String>,
) -> bool {
    if depth >= 16 {
        return false;
    }
    match t {
        Type::LiteralString | Type::LiteralStringValue => true,
        Type::DeclaredTypeVar(definition) => {
            if depth >= 16 {
                return false;
            }
            if active.iter().any(|key| key == definition.key.as_str()) {
                return false;
            }
            active.push(definition.key.to_string());
            let precise = if !definition.constraints.is_empty() {
                definition.constraints.len() <= 16
                    && definition
                        .constraints
                        .iter()
                        .all(|constraint| simple_param_bounded(inf, constraint, depth + 1, active))
            } else {
                definition
                    .bound
                    .as_deref()
                    .is_some_and(|bound| simple_param_bounded(inf, bound, depth + 1, active))
            };
            active.pop();
            precise
        }
        Type::None | Type::Never | Type::Literal(_) => true,
        Type::Callable(signature) => {
            signature.params.len() <= 128
                && signature.params.iter().all(|p| advanced::known(&p.ty))
                && advanced::known(&signature.ret)
        }
        Type::Tuple(items) => {
            items.len() <= 128
                && items
                    .iter()
                    .all(|item| simple_param_bounded(inf, item, depth + 1, active))
        }
        Type::Instance(c, _) | Type::Class(c, _) => {
            let Some(info) = inf.class_info(c) else {
                return false;
            };
            let from_stub = inf.db.file(c.file).is_some_and(|f| f.is_stub);
            if info.is_protocol {
                return !from_stub && canonical_protocol_param(inf, c);
            }
            if from_stub {
                CONCRETE_BUILTINS.contains(&c.name.as_str())
            } else {
                true
            }
        }
        Type::Union(ps) => {
            ps.len() <= 128
                && !ps.is_empty()
                && ps
                    .iter()
                    .all(|p| simple_param_bounded(inf, p, depth + 1, active))
        }
        _ => false,
    }
}

fn is_concrete_arg(t: &Type) -> bool {
    match t {
        Type::LiteralString | Type::LiteralStringValue => true,
        Type::Instance(..)
        | Type::Literal(_)
        | Type::None
        | Type::Tuple(_)
        | Type::Class(..)
        | Type::Function(_)
        | Type::BoundMethod(..)
        | Type::Module(_) => true,
        Type::Union(ts) => !ts.is_empty() && ts.iter().all(is_concrete_arg),
        _ => false,
    }
}

fn compatible(inf: &Inferer<'_>, arg: &Type, param: &Type) -> bool {
    if inf.is_compatible(arg, param) {
        return true;
    }
    // Promotions accepted by the type system but not modelled by `is_compatible`.
    match (arg, param) {
        (Type::Instance(a, _), Type::Instance(p, _)) => matches!(
            (a.name.as_str(), p.name.as_str()),
            ("bytearray", "bytes")
                | ("memoryview", "bytes")
                | ("bool", "float")
                | ("bool", "complex")
                | ("int", "complex")
                | ("float", "complex")
        ),
        (Type::Literal(l), Type::Instance(p, _)) => matches!(
            (l, p.name.as_str()),
            (LiteralValue::Bool(_), "float")
                | (LiteralValue::Bool(_), "complex")
                | (LiteralValue::Int(_), "complex")
                | (LiteralValue::Int(_), "float")
        ),
        (Type::Union(ts), _) => ts.iter().all(|t| compatible(inf, t, param)),
        (_, Type::Union(ps)) => ps.iter().any(|p| compatible(inf, arg, p)),
        _ => false,
    }
}

fn display(t: &Type) -> String {
    match t {
        Type::Literal(LiteralValue::Int(_)) => "int".into(),
        Type::Literal(LiteralValue::Str(_)) => "str".into(),
        Type::Literal(LiteralValue::Bytes(_)) => "bytes".into(),
        Type::Literal(LiteralValue::Bool(_)) => "bool".into(),
        Type::Union(ts) => ts.iter().map(display).collect::<Vec<_>>().join(" | "),
        other => format!("{other}"),
    }
}

/// `Expected type 'P', got 'A' instead` when `arg` is concrete and not
/// assignable to the simple parameter type `param`.
fn incompatible_message(inf: &Inferer<'_>, arg: &Type, param: &Type) -> Option<String> {
    incompatible_message_checked(inf, arg, param, is_simple_param(inf, param))
}

fn incompatible_message_checked(
    inf: &Inferer<'_>,
    arg: &Type,
    param: &Type,
    checkable: bool,
) -> Option<String> {
    if !is_concrete_arg(arg) || !checkable {
        return None;
    }
    if compatible(inf, arg, param) {
        return None;
    }
    // Retain literal constraints on the expected side: Expected str / got
    // str would hide the actual rejected Literal value.
    let expected = match param {
        Type::Literal(_) => param.to_string(),
        _ => display(param),
    };
    Some(format!(
        "Expected type '{}', got '{}' instead",
        expected,
        display(arg)
    ))
}

/// Literals and displays whose type is obvious from the source.
fn is_literal_like(e: &ast::Expr) -> bool {
    match e {
        ast::Expr::Literal(l) => l.kind() != Some(SyntaxKind::ELLIPSIS),
        ast::Expr::String(_)
        | ast::Expr::FString(_)
        | ast::Expr::ConcatString(_)
        | ast::Expr::List(_)
        | ast::Expr::Dict(_)
        | ast::Expr::Set(_)
        | ast::Expr::Tuple(_) => true,
        ast::Expr::Unary(u) => u
            .operand()
            .is_some_and(|o| matches!(o, ast::Expr::Literal(_))),
        ast::Expr::Paren(p) => p.inner().is_some_and(|i| is_literal_like(&i)),
        _ => false,
    }
}

fn check_returns(inf: &Inferer<'_>, file: &Arc<FileInfo>, node: &SyntaxNode, sink: &mut Sink) {
    let Some(f) = ast::FuncDef::cast(node.clone()) else {
        return;
    };
    let Some(ann) = f.return_annotation() else {
        return;
    };
    let Some(body) = f.body() else { return };
    let own = |n: &SyntaxNode| {
        !n.ancestors().take_while(|a| a != node).any(|a| {
            matches!(
                a.kind(),
                SyntaxKind::FUNC_DEF | SyntaxKind::LAMBDA_EXPR | SyntaxKind::CLASS_DEF
            )
        })
    };
    if body.syntax().descendants().any(|n| {
        matches!(
            n.kind(),
            SyntaxKind::YIELD_EXPR | SyntaxKind::YIELD_FROM_EXPR
        ) && own(&n)
    }) {
        return;
    }
    let ann_ty = inf.annotation_type(file, &ann);
    if !is_simple_param(inf, &ann_ty) {
        return;
    }
    for ret in body
        .syntax()
        .descendants()
        .filter(|n| n.kind() == SyntaxKind::RETURN_STMT && own(n))
        .filter_map(ast::ReturnStmt::cast)
    {
        match ret.value() {
            Some(v) if is_literal_like(&v) || precise_arg(inf, file, &v) => {
                if inf.typed_dict_literal_compatible(file, &v, &ann_ty) == Some(true) {
                    continue;
                }
                if let Some(message) = advanced::typed_dict_error(inf, file, &v, &ann_ty) {
                    sink.push(
                        v.text_range(),
                        Severity::Warning,
                        "PyTypedDict",
                        message,
                        Vec::new(),
                    );
                    continue;
                }
                let t = inf.type_of_expr(file, &v);
                if let Some(msg) = incompatible_message(inf, &t, &ann_ty) {
                    sink.push(v.text_range(), Severity::Warning, TYPES, msg, Vec::new());
                }
            }
            None => {
                if let Some(msg) = incompatible_message(inf, &Type::None, &ann_ty) {
                    sink.push(ret.text_range(), Severity::Warning, TYPES, msg, Vec::new());
                }
            }
            _ => {}
        }
    }
}

fn check_ann_assign(inf: &Inferer<'_>, file: &Arc<FileInfo>, node: &SyntaxNode, sink: &mut Sink) {
    let Some(a) = ast::AnnAssignStmt::cast(node.clone()) else {
        return;
    };
    let (Some(ann), Some(value)) = (a.annotation(), a.value()) else {
        return;
    };
    let constructor_call =
        matches!(value.clone().unparen(), ast::Expr::Call(_)) && precise_arg(inf, file, &value);
    if !is_literal_like(&value) && !constructor_call && !precise_arg(inf, file, &value) {
        return;
    }
    let ann_ty = inf.annotation_type(file, &ann);
    if inf.typed_dict_literal_compatible(file, &value, &ann_ty) == Some(true) {
        return;
    }
    if let Some(message) = advanced::typed_dict_error(inf, file, &value, &ann_ty) {
        sink.push(
            value.text_range(),
            Severity::Warning,
            "PyTypedDict",
            message,
            Vec::new(),
        );
        return;
    }
    let t = inf.type_of_expr(file, &value);
    if let Some(msg) = incompatible_message(inf, &t, &ann_ty) {
        sink.push(
            value.text_range(),
            Severity::Warning,
            TYPES,
            msg,
            Vec::new(),
        );
    }
}

/// `x = None` … `x.attr` where `x` is never assigned anything else.
fn check_none_attribute(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    node: &SyntaxNode,
    sink: &mut Sink,
) {
    let Some(attr) = ast::AttributeExpr::cast(node.clone()) else {
        return;
    };
    let (Some(obj), Some(name_tok)) = (attr.object(), attr.attr()) else {
        return;
    };
    let ast::Expr::Name(n) = obj else { return };
    let Some(reference) = file.scopes.reference_at(n.text_range().start()) else {
        return;
    };
    let Some(bid) = reference.resolved else {
        return;
    };
    let b = file.scopes.binding(bid);
    let ids = file.scopes.bindings_in(b.scope, &b.name);
    if ids.is_empty() {
        return;
    }
    let text: &str = &file.text;
    let all_none = ids.iter().all(|id| {
        let b = file.scopes.binding(*id);
        b.kind == BindingKind::Assignment
            && b.value.is_some_and(|v| {
                text[usize::from(v.start())..usize::from(v.end())].trim() == "None"
            })
    });
    if !all_none
        || file.scopes.scopes.iter().any(|s| {
            s.globals.iter().any(|g| g == &b.name) || s.nonlocals.iter().any(|g| g == &b.name)
        })
    {
        return;
    }
    if inf.lookup_member(&Type::None, name_tok.text()).is_some() {
        return;
    }
    sink.push(
        name_tok.text_range(),
        Severity::Warning,
        "PyUnresolvedReferences",
        format!("Cannot find reference '{}' in 'None'", name_tok.text()),
        Vec::new(),
    );
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use rustpy_core::CancellationToken;
    use rustpy_semantic::db::Analysis;

    #[test]
    fn cancelled_type_walks_do_not_emit_annotation_diagnostics() {
        let db = Analysis::for_project(std::path::Path::new("/typecheck-cancellation"));
        let id = db.set_file_text(
            std::path::Path::new("/typecheck-cancellation/main.py"),
            "value: b'int' = None\n",
        );
        let file = db.file(id).unwrap();
        for advanced_only in [false, true] {
            let inspect = |sink: &mut Sink| {
                let inf = Inferer::new(&db);
                if advanced_only {
                    advanced::run(&inf, &file, (3, 12), sink);
                } else {
                    run(&inf, &file, (3, 12), sink);
                }
            };
            let mut complete = Sink::new();
            inspect(&mut complete);
            assert!(complete.out.iter().any(|problem| {
                problem.diagnostic.message == "A bytes literal is not a type annotation"
            }));

            let token = CancellationToken::new();
            token.cancel();
            let mut cancelled = Sink::new();
            rustpy_semantic::infer::with_cancellation(&token, || inspect(&mut cancelled));
            assert!(
                cancelled.out.is_empty(),
                "an obsolete type walk still produced diagnostics (advanced only: {advanced_only})"
            );
        }
    }
}

#[cfg(test)]
mod terminal_reaching_tests {
    use super::*;
    use rustpy_semantic::db::Analysis;

    #[test]
    fn argument_precision_uses_current_terminal_branch_authority() {
        let directory = tempfile::TempDir::with_prefix("rustpy-typecheck-terminal-flow").unwrap();
        let path = directory.path().join("main.py");
        let source = "import sys\ndef consume(value:int):\n    pass\ndef f(flag:bool):\n    value=1\n    if flag:\n        value='bad'\n    else:\n        sys.exit()\n    consume(value)\n";
        std::fs::write(&path, source).unwrap();
        let db = Analysis::for_project(directory.path());
        let file = db.file_for_path(&path).unwrap();
        let argument = file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::CallExpr::cast)
            .find(|call| {
                call.callee()
                    .is_some_and(|expr| expr.syntax().text() == "consume")
            })
            .unwrap()
            .syntax()
            .descendants()
            .filter_map(ast::NameExpr::cast)
            .find(|name| name.name_text() == "value")
            .unwrap();
        let inf = Inferer::new(&db);
        assert!(precise_arg(&inf, &file, &ast::Expr::Name(argument.clone())));
        let scope = rustpy_semantic::flow::flow_scope(
            &file,
            file.scopes.scope_at(argument.syntax().text_range().start()),
        )
        .unwrap();
        let flow = rustpy_semantic::flow::flow_of_with_db(&db, &file, scope);
        let bindings: Vec<_> = flow.refs[&argument.name_token().unwrap().text_range().start()]
            .iter()
            .filter_map(|reach| reach.binding)
            .collect();
        assert_eq!(bindings.len(), 1);
        let binding = file.scopes.binding(bindings[0]);
        let range = binding.value.unwrap();
        assert_eq!(
            &file.text[usize::from(range.start())..usize::from(range.end())],
            "'bad'"
        );
    }
}

#[cfg(test)]
mod protocol_consumer_tests {
    use super::*;
    use rustpy_semantic::db::Analysis;
    fn type_warnings(source: &str) -> usize {
        let db = Analysis::for_project(std::path::Path::new("/protocol-consumer-controls"));
        let id = db.set_file_text(
            std::path::Path::new("/protocol-consumer-controls/m.py"),
            source,
        );
        let file = db.file(id).unwrap();
        super::super::inspect(&db, &file, &super::super::InspectionOptions::default())
            .iter()
            .filter(|problem| problem.diagnostic.code == TYPES)
            .count()
    }
    #[test]
    fn compatible_protocol_method_does_not_warn() {
        assert_eq!(
            type_warnings(
                "from typing import Protocol\nclass P(Protocol):\n    def run(self,value:int)->object: ...\nclass C:\n    def run(self,value:object)->str: return 'ok'\ndef consume(value:P): pass\nconsume(C())\n"
            ),
            0
        );
    }
    #[test]
    fn unknown_argument_keeps_conservative_protocol_policy() {
        assert_eq!(
            type_warnings(
                "from typing import Protocol, Any\nclass P(Protocol):\n    def run(self,value:int)->str: ...\ndef consume(value:P): pass\ndef f(value:Any):\n    consume(value)\n"
            ),
            0
        );
    }
    #[test]
    fn fake_protocol_base_keeps_existing_conservative_inspection_policy() {
        assert_eq!(
            type_warnings(
                "class Protocol: pass\nclass P(Protocol):\n    def run(self,value:int)->str: ...\nclass C: pass\ndef consume(value:P): pass\nconsume(C())\n"
            ),
            0
        );
    }
}
