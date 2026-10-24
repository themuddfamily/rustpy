//! IDE-facing queries built on the inference engine: go to definition,
//! hover/quick documentation, diagnostics and code completion
//! (roadmap §2.4, §2.5 references, §2.7 navigation).

use std::path::PathBuf;
use std::sync::Arc;

use rustpy_core::{Diagnostic, Severity, TextRange, TextSize};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::{SyntaxKind, SyntaxToken};
use smol_str::SmolStr;

use crate::classes::MemberKind;
use crate::db::{Analysis, FileInfo};
use crate::infer::Inferer;
use crate::scope::{BindingKind, ScopeKind};
use crate::types::Type;

pub use crate::completion_ctx::{completion_context, insert_text_for, CompletionContext};

/// A navigation target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub path: PathBuf,
    /// Range of the name token to select.
    pub range: TextRange,
    pub is_stub: bool,
}

/// Information shown in the hover / Quick Documentation popup.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HoverInfo {
    /// Short header such as `def greet(name: str) -> str` or `x: int`.
    pub signature: String,
    pub docstring: Option<String>,
    /// Module or class owning the symbol.
    pub origin: Option<String>,
    pub kind: Option<&'static str>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum CompletionKind {
    Keyword,
    Variable,
    Parameter,
    Function,
    Method,
    Class,
    Module,
    Attribute,
    Property,
    Constant,
    /// Live template abbreviation.
    Template,
    /// Postfix completion key (`expr.if`).
    Postfix,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: SmolStr,
    pub kind: CompletionKind,
    /// Right-aligned type / signature text.
    pub detail: String,
    /// Text inserted (may differ from label, e.g. adds `()`).
    pub insert: String,
    /// Caret offset relative to the start of `insert` after insertion.
    pub caret_offset: Option<usize>,
    /// Lower sorts first.
    pub priority: u8,
}

pub const PYTHON_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield", "match", "case", "type",
];

/// Finds the identifier token at `offset`.
pub fn ident_at(file: &FileInfo, offset: TextSize) -> Option<SyntaxToken> {
    let offset = crate::clamp_offset(file, offset);
    let root = file.parse.syntax_node();
    let tok = root.token_at_offset(offset);
    let pick = |t: SyntaxToken| {
        if t.kind() == SyntaxKind::IDENT || t.kind().is_keyword() {
            Some(t)
        } else {
            None
        }
    };
    match tok {
        rustpy_syntax::rowan::TokenAtOffset::Single(t) => pick(t),
        rustpy_syntax::rowan::TokenAtOffset::Between(l, r) => pick(r).or_else(|| pick(l)),
        rustpy_syntax::rowan::TokenAtOffset::None => None,
    }
}

/// What the keyword argument `name=` of a call refers to.
#[derive(Clone, Debug)]
pub enum KeywordTarget {
    /// The parameter of the called function.
    Param(crate::infer::Definition),
    /// The callee is known but has no parameter that can be passed by this
    /// keyword (it takes `**kwargs`, or the parameter is positional-only).
    NoParam,
    /// The callee's type is unknown.
    Unresolved,
}

/// Resolves the keyword argument whose name token is `tok` through the
/// callee's inferred signature (constructors resolve to `__init__`).
pub fn keyword_target(
    db: &Analysis,
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    tok: &SyntaxToken,
) -> KeywordTarget {
    let Some(kw) = tok.parent().filter(|p| p.kind() == SyntaxKind::KEYWORD_ARG) else {
        return KeywordTarget::Unresolved;
    };
    // Keywords of a class header (`metaclass=`) are not call arguments.
    let Some(call) = kw
        .parent()
        .filter(|p| p.kind() == SyntaxKind::ARG_LIST)
        .and_then(|l| l.parent())
        .and_then(ast::CallExpr::cast)
    else {
        return KeywordTarget::NoParam;
    };
    let Some(callee) = call.callee() else {
        return KeywordTarget::Unresolved;
    };
    // The overload (or conditional definition) the arguments select.
    let sigs = crate::call_help::signatures_of(inf, &inf.type_of_expr(file, &callee));
    let sig = if sigs.len() > 1 {
        let (pos, kws) = inf.call_args(file, &call);
        crate::call_help::select_for_call(inf, &sigs, &pos, &kws)
    } else {
        sigs.into_iter().next()
    };
    let Some(def_ref) = sig.as_ref().and_then(|s| s.def.clone()) else {
        return KeywordTarget::Unresolved;
    };
    let name = tok.text();
    let by_keyword = sig
        .as_ref()
        .and_then(|s| s.params.iter().find(|p| p.name == name))
        .is_some_and(|p| {
            !matches!(
                p.kind,
                crate::types::ParamKind::PositionalOnly
                    | crate::types::ParamKind::VarArgs
                    | crate::types::ParamKind::KwArgs
            )
        });
    if !by_keyword {
        return KeywordTarget::NoParam;
    }
    let Some(f) = db.file(def_ref.file) else {
        return KeywordTarget::Unresolved;
    };
    let Some(scope) = f.scopes.scope_for_node(def_ref.range) else {
        return KeywordTarget::Unresolved;
    };
    match f
        .scopes
        .bindings_in(scope, name)
        .iter()
        .copied()
        .find(|id| f.scopes.binding(*id).kind == BindingKind::Param)
    {
        Some(binding) => KeywordTarget::Param(crate::infer::Definition { file: f, binding }),
        None => KeywordTarget::NoParam,
    }
}

/// The parameter a keyword argument names (`f(key=1)` → `key` of `f`), or
/// `None` when the callee cannot be resolved or has no such parameter.
pub fn keyword_arg_param(
    db: &Analysis,
    file: &Arc<FileInfo>,
    tok: &SyntaxToken,
) -> Option<crate::infer::Definition> {
    match keyword_target(db, &Inferer::new(db), file, tok) {
        KeywordTarget::Param(d) => Some(d),
        _ => None,
    }
}

#[path = "features/declaration_navigation.rs"]
mod declaration_navigation;

/// Go to Declaration with the user's source preference. Declaration identity
/// remains available through `definition_at` for Show Usages dispatch.
pub fn definition_at_with_options(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    prefer_source: bool,
) -> Vec<Location> {
    let cython = crate::cython::definition(db, file, offset);
    if !cython.is_empty() {
        return cython;
    }
    let framework = crate::frameworks::definition(db, file, offset);
    if !framework.is_empty() {
        return framework;
    }
    if let Some(definition) = crate::pytest_fixtures::resolve(db, file, offset) {
        return vec![Location {
            path: definition.file.path.clone(),
            range: definition.binding().range,
            is_stub: definition.file.is_stub,
        }];
    }
    declaration_navigation::definition_at(db, file, offset, prefer_source)
}
/// Explicit .pyi/.py counterpart navigation at a declaration or reference.
pub fn source_stub_counterpart_at(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
) -> Vec<Location> {
    let targets = definition_at(db, file, offset);
    targets
        .iter()
        .flat_map(|target| declaration_navigation::counterpart(db, target, target.is_stub))
        .collect()
}
/// Go to Declaration: definition(s) of the symbol at `offset`.
pub fn definition_at(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Vec<Location> {
    definition_at_with_options(db, file, offset, false)
}
fn definition_at_ordinary(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Vec<Location> {
    let offset = crate::clamp_offset(file, offset);
    let inf = Inferer::new(db);
    let Some(tok) = ident_at(file, offset) else {
        return Vec::new();
    };
    let Some(parent) = tok.parent() else {
        return Vec::new();
    };
    // Import module paths: `import os.path` / `from x import y`.
    if let Some(loc) = import_target(db, &inf, file, &tok, &parent) {
        return loc;
    }
    match parent.kind() {
        SyntaxKind::NAME_EXPR => {
            let scope = file.scopes.scope_at(offset);
            let name = tok.text();
            // A binding at this exact position?
            if let Some((_, b)) = file.scopes.binding_at(offset) {
                if b.range == tok.text_range() && !matches!(b.kind, BindingKind::Import(_)) {
                    // Already at the definition: return it; the editor then shows its usages (Show Usages).
                    return vec![Location {
                        path: file.path.clone(),
                        range: b.range,
                        is_stub: file.is_stub,
                    }];
                }
            }
            match inf.resolve_name(file, scope, name) {
                Some(def) => {
                    let def = inf.follow_imports(def);
                    let b = def.binding();
                    if let BindingKind::Import(_) = &b.kind {
                        if let Some(m) = inf.import_target_module(&def) {
                            return vec![Location {
                                path: m.path.clone(),
                                range: TextRange::empty(TextSize::new(0)),
                                is_stub: m.is_stub,
                            }];
                        }
                    }
                    vec![Location {
                        path: def.file.path.clone(),
                        range: b.range,
                        is_stub: def.file.is_stub,
                    }]
                }
                None => inf
                    .star_imported_module(file, scope, name)
                    .map(|module| {
                        vec![Location {
                            path: module.path.clone(),
                            range: TextRange::empty(TextSize::new(0)),
                            is_stub: module.is_stub,
                        }]
                    })
                    .unwrap_or_default(),
            }
        }
        SyntaxKind::ATTRIBUTE_EXPR => {
            let Some(attr) = ast::AttributeExpr::cast(parent) else {
                return Vec::new();
            };
            let Some(obj) = attr.object() else {
                return Vec::new();
            };
            let base = inf.type_of_expr(file, &obj);
            member_locations(db, &inf, &base, tok.text())
        }
        SyntaxKind::KEYWORD_ARG => match keyword_target(db, &inf, file, &tok) {
            // Keyword argument → parameter of the called function.
            KeywordTarget::Param(def) => vec![Location {
                path: def.file.path.clone(),
                range: def.binding().range,
                is_stub: def.file.is_stub,
            }],
            _ => {
                // Generated `__init__` (dataclass, NamedTuple, …): the field declaration.
                let callee = parent
                    .ancestors()
                    .find_map(ast::CallExpr::cast)
                    .and_then(|c| c.callee());
                match callee.map(|c| inf.type_of_expr(file, &c)) {
                    Some(Type::Class(c, args)) if inf.generated_constructor(&c).is_some() => {
                        member_locations(db, &inf, &Type::Instance(c, args), tok.text())
                    }
                    _ => Vec::new(),
                }
            }
        },
        SyntaxKind::FUNC_DEF
        | SyntaxKind::CLASS_DEF
        | SyntaxKind::PARAM
        | SyntaxKind::STAR_PARAM
        | SyntaxKind::STAR_STAR_PARAM
        | SyntaxKind::IMPORT_ALIAS
        | SyntaxKind::GLOBAL_STMT
        | SyntaxKind::NONLOCAL_STMT
        | SyntaxKind::MATCH_CAPTURE_PATTERN
        | SyntaxKind::TYPE_PARAM
        | SyntaxKind::PARAM_SPEC_PARAM
        | SyntaxKind::TYPE_VAR_TUPLE_PARAM => {
            vec![Location {
                path: file.path.clone(),
                range: tok.text_range(),
                is_stub: file.is_stub,
            }]
        }
        _ => Vec::new(),
    }
}

fn import_target(
    db: &Analysis,
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    tok: &SyntaxToken,
    parent: &rustpy_syntax::SyntaxNode,
) -> Option<Vec<Location>> {
    if parent.kind() == SyntaxKind::DOTTED_NAME {
        // Module path segment: resolve the prefix up to this token.
        let dotted = ast::DottedName::cast(parent.clone())?;
        let segs: Vec<SyntaxToken> = dotted.segments().collect();
        let idx = segs.iter().position(|s| s == tok)?;
        let prefix: Vec<&str> = segs[..=idx].iter().map(|s| s.text()).collect();
        let name = prefix.join(".");
        let from = parent.parent().and_then(ast::ImportFromStmt::cast);
        let m = match from {
            Some(f) => db.module_relative(file, f.level(), &name)?,
            None => db.module(&name)?,
        };
        return Some(vec![Location {
            path: m.path.clone(),
            range: TextRange::empty(TextSize::new(0)),
            is_stub: m.is_stub,
        }]);
    }
    if parent.kind() == SyntaxKind::IMPORT_ALIAS {
        let alias = ast::ImportAlias::cast(parent.clone())?;
        if alias.alias().as_ref() == Some(tok) {
            return Some(vec![Location {
                path: file.path.clone(),
                range: tok.text_range(),
                is_stub: file.is_stub,
            }]);
        }
        let from = parent.parent().and_then(ast::ImportFromStmt::cast)?;
        let m = db.module_relative(
            file,
            from.level(),
            &from.module().map(|m| m.path()).unwrap_or_default(),
        )?;
        if let Some(def) = inf.resolve_in_module(&m, tok.text(), 0) {
            let def = inf.follow_imports(def);
            let b = def.binding();
            if let BindingKind::Import(_) = b.kind {
                if let Some(mm) = inf.import_target_module(&def) {
                    return Some(vec![Location {
                        path: mm.path.clone(),
                        range: TextRange::empty(TextSize::new(0)),
                        is_stub: mm.is_stub,
                    }]);
                }
            }
            return Some(vec![Location {
                path: def.file.path.clone(),
                range: b.range,
                is_stub: def.file.is_stub,
            }]);
        }
        let sub = format!(
            "{}.{}",
            m.module_name.clone().unwrap_or_default(),
            tok.text()
        );
        let sm = db.module(&sub)?;
        return Some(vec![Location {
            path: sm.path.clone(),
            range: TextRange::empty(TextSize::new(0)),
            is_stub: sm.is_stub,
        }]);
    }
    None
}

fn member_locations(db: &Analysis, inf: &Inferer<'_>, base: &Type, name: &str) -> Vec<Location> {
    let mut out = Vec::new();
    let mut push = |file: &Arc<FileInfo>, range: TextRange| {
        let loc = Location {
            path: file.path.clone(),
            range,
            is_stub: file.is_stub,
        };
        if !out.contains(&loc) {
            out.push(loc);
        }
    };
    match base {
        Type::Module(fid) => {
            if let Some(m) = db.file(*fid) {
                if let Some(def) = inf.resolve_in_module(&m, name, 0) {
                    let def = inf.follow_imports(def);
                    push(&def.file, def.binding().range);
                } else if let Some(modname) = &m.module_name {
                    if let Some(sub) = db.module(&format!("{modname}.{name}")) {
                        push(&sub, TextRange::empty(TextSize::new(0)));
                    }
                }
            }
        }
        Type::Instance(c, args) | Type::Class(c, args) => {
            for (cls, _) in inf.mro_with_args(c, args) {
                if let Some(info) = inf.class_info(&cls) {
                    if let Some(m) = info.members.get(name) {
                        if let Some(f) = db.file(m.file) {
                            push(&f, m.range);
                        }
                        break;
                    }
                }
            }
        }
        Type::SelfType(c) => {
            return member_locations(db, inf, &Type::Instance(c.clone(), vec![]), name)
        }
        Type::Super { after, receiver } => {
            for (cls, _) in inf
                .super_mro(after, receiver)
                .map(|(classes, _)| classes)
                .unwrap_or_default()
            {
                if let Some(m) = inf
                    .class_info(&cls)
                    .and_then(|info| info.members.get(name).cloned())
                {
                    if let Some(f) = db.file(m.file) {
                        push(&f, m.range);
                    }
                    break;
                }
            }
        }
        Type::Union(ts) => {
            for t in ts {
                for l in member_locations(db, inf, t, name) {
                    if !out.contains(&l) {
                        out.push(l);
                    }
                }
            }
        }
        Type::Literal(_)
        | Type::LiteralString
        | Type::LiteralStringValue
        | Type::Tuple(_)
        | Type::None
        | Type::Function(_)
        | Type::BoundMethod(..) => {
            // Delegate through the builtin class of the value.
            if let Some(t) = inf.lookup_member(base, name) {
                let _ = t;
            }
            let cls = match base {
                Type::Literal(crate::types::LiteralValue::EnumMember { class, .. }) => {
                    Some(class.clone())
                }
                Type::Tuple(_) => inf.builtin_class("tuple"),
                Type::Literal(crate::types::LiteralValue::Str(_))
                | Type::LiteralString
                | Type::LiteralStringValue => inf.builtin_class("str"),
                Type::Literal(crate::types::LiteralValue::Int(_)) => inf.builtin_class("int"),
                Type::Literal(crate::types::LiteralValue::Bool(_)) => inf.builtin_class("bool"),
                Type::Literal(crate::types::LiteralValue::Bytes(_)) => inf.builtin_class("bytes"),
                _ => None,
            };
            if let Some(c) = cls {
                return member_locations(db, inf, &Type::Instance(c, vec![]), name);
            }
        }
        _ => {}
    }
    out
}

/// Go to Type Declaration (Ctrl+Shift+B): the class(es) of the value at
/// `offset` — a variable, attribute, parameter or other expression, or the
/// return type of a function name. Every member of a union is a target
/// (`None` is skipped); callables lead to their return type.
pub fn type_definition_at(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Vec<Location> {
    let offset = crate::clamp_offset(file, offset);
    let inf = Inferer::new(db);
    let Some(tok) = ident_at(file, offset) else {
        return Vec::new();
    };
    // Hard keywords (`def`, `class`, …) have no type of their own.
    if tok.kind() != SyntaxKind::IDENT && !matches!(tok.text(), "match" | "case" | "type" | "_") {
        return Vec::new();
    }
    let Some(parent) = tok.parent() else {
        return Vec::new();
    };
    let ty = match parent.kind() {
        SyntaxKind::NAME_EXPR | SyntaxKind::ATTRIBUTE_EXPR => ast::Expr::cast(parent.clone())
            .map(|e| inf.type_of_expr(file, &e))
            .unwrap_or(Type::Unknown),
        SyntaxKind::FUNC_DEF => {
            match ast::FuncDef::cast(parent.clone()).map(|f| inf.function_type(file, &f, None)) {
                Some(Type::Function(sig)) => sig.ret.clone(),
                // A property: already the value's type.
                Some(other) => other,
                None => Type::Unknown,
            }
        }
        SyntaxKind::CLASS_DEF => Type::Class(
            crate::types::ClassRef {
                file: file.id,
                range: parent.text_range(),
                name: SmolStr::new(tok.text()),
            },
            Vec::new(),
        ),
        SyntaxKind::PARAM
        | SyntaxKind::STAR_PARAM
        | SyntaxKind::STAR_STAR_PARAM
        | SyntaxKind::IMPORT_ALIAS
        | SyntaxKind::MATCH_CAPTURE_PATTERN => file
            .scopes
            .binding_at(offset)
            .map(|(id, _)| inf.type_of_binding(file, id))
            .unwrap_or(Type::Unknown),
        _ => Type::Unknown,
    };
    let mut out = Vec::new();
    type_locations(db, &inf, &ty, &mut out, 0);
    out
}

fn type_locations(
    db: &Analysis,
    inf: &Inferer<'_>,
    ty: &Type,
    out: &mut Vec<Location>,
    depth: usize,
) {
    if depth > 4 {
        return;
    }
    let mut push = |loc: Location| {
        if !out.contains(&loc) {
            out.push(loc);
        }
    };
    match ty {
        Type::Instance(c, _) | Type::Class(c, _) | Type::SelfType(c) => {
            if let Some(f) = db.file(c.file) {
                push(Location {
                    path: f.path.clone(),
                    range: crate::hierarchy::class_name_range(&f, c),
                    is_stub: f.is_stub,
                });
            }
        }
        Type::Module(fid) => {
            if let Some(m) = db.file(*fid) {
                push(Location {
                    path: m.path.clone(),
                    range: TextRange::empty(TextSize::new(0)),
                    is_stub: m.is_stub,
                });
            }
        }
        Type::Union(ts) => {
            for t in ts.iter().filter(|t| !t.is_none()) {
                type_locations(db, inf, t, out, depth + 1);
            }
        }
        Type::Function(sig) | Type::BoundMethod(sig, _) | Type::Callable(sig) => {
            type_locations(db, inf, &sig.ret, out, depth + 1)
        }
        Type::Overloaded(sigs) => {
            for sig in sigs.iter().take(128) {
                type_locations(db, inf, &sig.ret, out, depth + 1);
            }
        }
        Type::Literal(crate::types::LiteralValue::EnumMember { class, .. }) => {
            type_locations(
                db,
                inf,
                &Type::Instance(class.clone(), Vec::new()),
                out,
                depth + 1,
            );
        }
        Type::Literal(_) | Type::LiteralString | Type::LiteralStringValue | Type::Tuple(_) => {
            let name = match ty {
                Type::Literal(crate::types::LiteralValue::Str(_))
                | Type::LiteralString
                | Type::LiteralStringValue => "str",
                Type::Literal(crate::types::LiteralValue::Int(_)) => "int",
                Type::Literal(crate::types::LiteralValue::Bool(_)) => "bool",
                Type::Literal(crate::types::LiteralValue::Bytes(_)) => "bytes",
                _ => "tuple",
            };
            if let Some(c) = inf.builtin_class(name) {
                type_locations(db, inf, &Type::Instance(c, Vec::new()), out, depth + 1);
            }
        }
        _ => {}
    }
}

/// Hover / Quick Documentation for the symbol at `offset`.
pub fn hover_at(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Option<HoverInfo> {
    if let Some(hover) = crate::cython::hover(file, offset) {
        return Some(hover);
    }
    let offset = crate::clamp_offset(file, offset);
    let inf = Inferer::new(db);
    let tok = ident_at(file, offset)?;
    // Hard keywords (`def`, `class`, `return`, …) have no documentation of their own.
    if tok.kind() != SyntaxKind::IDENT && !matches!(tok.text(), "match" | "case" | "type" | "_") {
        return None;
    }
    let parent = tok.parent()?;
    // The imported name is wrapped in DOTTED_NAME; an explicit `as` name
    // belongs directly to IMPORT_ALIAS. Both refer to the import binding.
    let parent = if parent.kind() == SyntaxKind::DOTTED_NAME {
        parent
            .ancestors()
            .find(|node| node.kind() == SyntaxKind::IMPORT_ALIAS)
            .unwrap_or(parent)
    } else {
        parent
    };
    let name = tok.text().to_string();
    let (ty, origin) = match parent.kind() {
        SyntaxKind::NAME_EXPR => {
            let expr = ast::Expr::cast(parent.clone())?;
            let scope = file.scopes.scope_at(offset);
            let def = inf.resolve_name(file, scope, &name);
            let origin = def.as_ref().map(|d| {
                d.file.module_name.clone().unwrap_or_else(|| {
                    d.file
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                })
            });
            (inf.type_of_expr(file, &expr), origin)
        }
        SyntaxKind::ATTRIBUTE_EXPR => {
            let expr = ast::Expr::cast(parent.clone())?;
            let attr = ast::AttributeExpr::cast(parent.clone())?;
            let base = attr.object().map(|o| inf.type_of_expr(file, &o));
            // `math.sqrt`: the module's name rather than its type (`module`).
            let origin = base.as_ref().map(|b| match b {
                Type::Module(m) => db
                    .file(*m)
                    .and_then(|f| f.module_name.clone())
                    .unwrap_or_else(|| b.to_string()),
                other => other.to_string(),
            });
            (inf.type_of_expr(file, &expr), origin)
        }
        SyntaxKind::FUNC_DEF => {
            let f = ast::FuncDef::cast(parent.clone())?;
            let class = inf_enclosing_class_name(file, &parent);
            (inf.function_type(file, &f, None), class)
        }
        SyntaxKind::CLASS_DEF => {
            let c = ast::ClassDef::cast(parent.clone())?;
            let info = HoverInfo {
                signature: format!(
                    "class {}{}",
                    c.name_text(),
                    c.arguments()
                        .map(|a| a.syntax().text().to_string())
                        .unwrap_or_default()
                ),
                docstring: c.docstring().map(|d| d.value()),
                origin: file.module_name.clone(),
                kind: Some("class"),
            };
            return Some(info);
        }
        SyntaxKind::KEYWORD_ARG => return keyword_arg_hover(&inf, file, &parent, &name),
        SyntaxKind::PARAM
        | SyntaxKind::STAR_PARAM
        | SyntaxKind::STAR_STAR_PARAM
        | SyntaxKind::IMPORT_ALIAS
        | SyntaxKind::MATCH_CAPTURE_PATTERN => {
            let (id, _) = file.scopes.binding_at(offset)?;
            (inf.type_of_binding(file, id), file.module_name.clone())
        }
        _ => return None,
    };
    let (signature, kind, doc) = describe(&inf, &name, &ty);
    // Stubs carry no docstrings: use the interpreter's source of the same name.
    let doc = doc.or_else(|| crate::stub_docs::doc_for_type(db, &ty));
    Some(HoverInfo {
        signature: user_facing_types(&signature),
        docstring: doc,
        origin,
        kind: Some(kind),
    })
}

/// Hover on `name` in `f(name=…)`: the parameter of the overload the call selects.
fn keyword_arg_hover(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    kw: &rustpy_syntax::SyntaxNode,
    name: &str,
) -> Option<HoverInfo> {
    let call = kw.ancestors().find_map(ast::CallExpr::cast)?;
    let callee = call.callee()?;
    let sigs = crate::call_help::signatures_of(inf, &inf.type_of_expr(file, &callee));
    let (pos, kws) = inf.call_args(file, &call);
    let sig = crate::call_help::select_for_call(inf, &sigs, &pos, &kws)?;
    let p = sig
        .params
        .iter()
        .find(|p| p.name == name && p.kind != crate::types::ParamKind::PositionalOnly);
    let text = match p {
        Some(p) => crate::call_help::param_text(p),
        None => {
            let kwargs = sig
                .params
                .iter()
                .find(|p| p.kind == crate::types::ParamKind::KwArgs)?;
            if kwargs.ty.is_unknown() {
                name.to_string()
            } else {
                format!("{name}: {}", kwargs.ty)
            }
        }
    };
    let origin = match &callee {
        ast::Expr::Name(n) => n.name_text(),
        ast::Expr::Attribute(a) => a.attr_text(),
        other => other.syntax().text().to_string(),
    };
    Some(HoverInfo {
        signature: user_facing_types(&text),
        docstring: None,
        origin: Some(origin),
        kind: Some("parameter"),
    })
}

/// Shows types inference could not determine as `Any`.
fn user_facing_types(text: &str) -> String {
    static UNKNOWN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    UNKNOWN
        .get_or_init(|| regex::Regex::new(r"\bUnknown\b").expect("valid regex"))
        .replace_all(text, "Any")
        .into_owned()
}

fn inf_enclosing_class_name(file: &FileInfo, node: &rustpy_syntax::SyntaxNode) -> Option<String> {
    node.ancestors()
        .skip(1)
        .find_map(ast::ClassDef::cast)
        .map(|c| c.name_text())
        .or_else(|| file.module_name.clone())
}

/// Human readable one-line description of a symbol with the given type.
pub fn describe(
    inf: &Inferer<'_>,
    name: &str,
    ty: &Type,
) -> (String, &'static str, Option<String>) {
    match ty {
        Type::Function(sig) | Type::BoundMethod(sig, _) => {
            let asyn = if sig.is_async { "async " } else { "" };
            (
                format!("{asyn}def {name}({}) -> {}", sig.display_params(), sig.ret),
                "function",
                sig.docstring.as_ref().map(|d| d.to_string()),
            )
        }
        Type::Overloaded(sigs) => {
            let first = &sigs[0];
            (
                format!(
                    "def {name}({}) -> {}  (+{} overloads)",
                    first.display_params(),
                    first.ret,
                    sigs.len() - 1
                ),
                "function",
                first.docstring.as_ref().map(|d| d.to_string()),
            )
        }
        Type::Class(c, _) => {
            let doc = inf
                .class_info(c)
                .and_then(|i| i.docstring.clone())
                .map(|d| d.to_string());
            let bases = inf.class_info(c).map(|i| i.base_exprs.len()).unwrap_or(0);
            let _ = bases;
            (format!("class {}", c.name), "class", doc)
        }
        Type::Module(fid) => {
            let m = inf.db.file(*fid);
            let doc = m
                .as_ref()
                .and_then(|m| ast::Module::cast(m.parse.syntax_node()))
                .and_then(|m| m.docstring())
                .map(|d| d.value());
            (
                format!(
                    "module {}",
                    m.and_then(|m| m.module_name.clone())
                        .unwrap_or_else(|| name.to_string())
                ),
                "module",
                doc,
            )
        }
        other => (format!("{name}: {other}"), "variable", None),
    }
}

/// Parameter Info (Ctrl+P): the signatures of the call enclosing `offset`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureHelp {
    /// Full text of the selected signature, e.g. `greet(name: str, excited: bool = False) -> str`.
    pub label: String,
    /// Byte ranges of each parameter inside `label`.
    pub params: Vec<(usize, usize)>,
    pub active: Option<usize>,
    pub docstring: Option<String>,
    /// Number of signatures (overloads).
    pub overloads: usize,
    /// Every signature in declaration order (overloads, conditional definitions).
    pub signatures: Vec<SignatureLabel>,
    /// The signature the arguments typed so far select (index into `signatures`).
    pub active_signature: usize,
}

/// One signature of [`SignatureHelp`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureLabel {
    pub label: String,
    /// Byte ranges of each parameter inside `label`.
    pub params: Vec<(usize, usize)>,
    /// Parameter the argument at the caret fills (keywords map by name).
    pub active: Option<usize>,
}

pub fn signature_help(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
) -> Option<SignatureHelp> {
    let offset = crate::clamp_offset(file, offset);
    let inf = Inferer::new(db);
    let ctx = crate::call_help::call_context(&inf, file, offset)?;
    let active_signature = ctx.active_signature(&inf);
    let signatures: Vec<SignatureLabel> = ctx
        .signatures
        .iter()
        .map(|sig| {
            let (label, params) = crate::call_help::signature_label(&ctx.name, sig);
            SignatureLabel {
                label,
                params,
                active: ctx.active_param(sig),
            }
        })
        .collect();
    let selected = signatures.get(active_signature)?.clone();
    // Stubs carry no docstrings: use the interpreter's source of the same name.
    let sig = &ctx.signatures[active_signature];
    let docstring = sig
        .docstring
        .as_ref()
        .map(|d| d.to_string())
        .or_else(|| crate::stub_docs::doc_for_type(db, &Type::Function(sig.clone())));
    let docstring = docstring.map(|d| d.lines().next().unwrap_or("").to_string());
    Some(SignatureHelp {
        label: selected.label,
        params: selected.params,
        active: selected.active,
        docstring,
        overloads: signatures.len(),
        signatures,
        active_signature,
    })
}

/// Semantic diagnostics: syntax errors plus unresolved references.
pub fn diagnostics(db: &Analysis, file: &Arc<FileInfo>) -> Vec<Diagnostic> {
    let inf = Inferer::new(db);
    let mut out: Vec<Diagnostic> = file.parse.errors().to_vec();
    if file.is_stub {
        return out;
    }
    // Code under `if sys.version_info < ...` / `sys.platform` guards that cannot run
    // on the target interpreter is not checked against its API (both branches
    // of `if TYPE_CHECKING:` are).
    let env = db.resolver.static_env().for_user_code();
    let root = file.parse.syntax_node();
    let dead = |range: TextRange| match root.covering_element(range) {
        rustpy_syntax::rowan::NodeOrToken::Node(n) => crate::static_cond::in_dead_branch(&n, &env),
        rustpy_syntax::rowan::NodeOrToken::Token(t) => t
            .parent()
            .is_some_and(|n| crate::static_cond::in_dead_branch(&n, &env)),
    };
    // Unresolved names.
    for r in &file.scopes.references {
        if r.resolved.is_some() || r.is_write || dead(r.range) {
            continue;
        }
        if inf.resolve_name(file, r.scope, &r.name).is_some()
            || inf.star_imported_module(file, r.scope, &r.name).is_some()
        {
            continue;
        }
        if matches!(
            r.name.as_str(),
            "__file__"
                | "__name__"
                | "__doc__"
                | "__spec__"
                | "__loader__"
                | "__package__"
                | "__builtins__"
                | "__debug__"
                | "__path__"
                | "__all__"
                | "reveal_type"
                | "__class__"
        ) {
            continue;
        }
        out.push(Diagnostic::new(
            r.range,
            Severity::Error,
            "PyUnresolvedReferences",
            format!("Unresolved reference '{}'", r.name),
        ));
    }
    // Unresolved attributes on known types and unresolved imports.
    // Classes whose hierarchy is fully known and has no `__getattr__` (memoized per class).
    let mut checkable_classes: std::collections::HashMap<crate::types::ClassRef, bool> =
        std::collections::HashMap::new();
    let mut checkable = |c: &crate::types::ClassRef| {
        *checkable_classes
            .entry(c.clone())
            .or_insert_with(|| !has_dynamic_getattr(&inf, c) && !inf.has_unknown_base(c))
    };
    for node in root.descendants() {
        if matches!(
            node.kind(),
            SyntaxKind::ATTRIBUTE_EXPR | SyntaxKind::IMPORT_STMT | SyntaxKind::IMPORT_FROM_STMT
        ) && crate::static_cond::in_dead_branch(&node, &env)
        {
            continue;
        }
        match node.kind() {
            SyntaxKind::ATTRIBUTE_EXPR => {
                let Some(attr) = ast::AttributeExpr::cast(node.clone()) else {
                    continue;
                };
                let (Some(obj), Some(name_tok)) = (attr.object(), attr.attr()) else {
                    continue;
                };
                let base = inf.type_of_expr(file, &obj);
                let check = match &base {
                    Type::Module(_) => true,
                    // An instance of `type` is some class: any class attribute may exist.
                    Type::Instance(c, _)
                        if c.name == "type" && inf.builtin_class("type").as_ref() == Some(c) =>
                    {
                        false
                    }
                    Type::Instance(c, _) | Type::Class(c, _) => checkable(c),
                    Type::Super { receiver, .. } => {
                        receiver.class_ref().is_some_and(&mut checkable)
                    }
                    _ => false,
                };
                if !check {
                    continue;
                }
                if inf.lookup_member(&base, name_tok.text()).is_none() {
                    // Attribute assignment on self defines it; skip write targets of assignments.
                    let is_assign_target = node.parent().is_some_and(|p| {
                        matches!(
                            p.kind(),
                            SyntaxKind::ASSIGN_STMT
                                | SyntaxKind::ANN_ASSIGN_STMT
                                | SyntaxKind::AUG_ASSIGN_STMT
                        ) && p.children().next().as_ref() == Some(&node)
                    });
                    if is_assign_target {
                        continue;
                    }
                    // Names a stub defines for another platform (`os.startfile` on Linux),
                    // or leaves out although the runtime module has them.
                    if let Type::Module(fid) = &base {
                        if db.file(*fid).is_some_and(|m| {
                            defined_on_other_platform(db, &m, name_tok.text(), 0)
                                || missing_from_stub(db, &m, name_tok.text())
                        }) {
                            continue;
                        }
                    }
                    let what = match &base {
                        Type::Module(fid) => format!(
                            "Cannot find reference '{}' in '{}'",
                            name_tok.text(),
                            db.file(*fid)
                                .and_then(|m| m.module_name.clone())
                                .unwrap_or_default()
                        ),
                        Type::Class(c, _) => format!(
                            "Unresolved attribute reference '{}' for class '{}'",
                            name_tok.text(),
                            c.name
                        ),
                        Type::Instance(c, _) => format!(
                            "Unresolved attribute reference '{}' for class '{}'",
                            name_tok.text(),
                            c.name
                        ),
                        // `super().x`: name the class the lookup starts from.
                        Type::Super { after, receiver } => {
                            let next = inf
                                .super_mro(after, receiver)
                                .and_then(|(classes, _)| {
                                    classes.first().map(|(c, _)| c.name.to_string())
                                })
                                .unwrap_or_else(|| "object".into());
                            format!(
                                "Unresolved attribute reference '{}' for class '{}'",
                                name_tok.text(),
                                next
                            )
                        }
                        _ => continue,
                    };
                    out.push(Diagnostic::new(
                        name_tok.text_range(),
                        Severity::Warning,
                        "PyUnresolvedReferences",
                        what,
                    ));
                }
            }
            SyntaxKind::IMPORT_STMT => {
                let Some(imp) = ast::ImportStmt::cast(node.clone()) else {
                    continue;
                };
                for alias in imp.aliases() {
                    let name = alias.name();
                    if db.import_module_from(file, &name).is_none() {
                        if let Some(d) = alias
                            .syntax()
                            .children()
                            .find(|c| c.kind() == SyntaxKind::DOTTED_NAME)
                        {
                            out.push(Diagnostic::new(
                                d.text_range(),
                                Severity::Error,
                                "PyUnresolvedReferences",
                                format!("No module named '{name}'"),
                            ));
                        }
                    }
                }
            }
            SyntaxKind::IMPORT_FROM_STMT => {
                let Some(imp) = ast::ImportFromStmt::cast(node.clone()) else {
                    continue;
                };
                let module = imp.module().map(|m| m.path()).unwrap_or_default();
                let target = if imp.level() == 0 {
                    db.import_module_from(file, &module)
                } else {
                    db.module_relative(file, imp.level(), &module)
                };
                match target {
                    None => {
                        let range = imp
                            .module()
                            .map(|m| m.text_range())
                            .unwrap_or_else(|| imp.text_range());
                        out.push(Diagnostic::new(
                            range,
                            Severity::Error,
                            "PyUnresolvedReferences",
                            format!("No module named '{}'", imp.module_path()),
                        ));
                    }
                    Some(m) => {
                        if imp.is_star() {
                            continue;
                        }
                        for alias in imp.aliases() {
                            let name = alias.name();
                            let found = inf.resolve_in_module(&m, &name, 0).is_some()
                                || m.module_name
                                    .as_ref()
                                    .is_some_and(|mn| db.module(&format!("{mn}.{name}")).is_some())
                                || (m.module_name.is_none()
                                    && db
                                        .resolver
                                        .resolve_relative(
                                            &file.path,
                                            imp.level(),
                                            &if module.is_empty() {
                                                name.clone()
                                            } else {
                                                format!("{module}.{name}")
                                            },
                                        )
                                        .is_some());
                            if !found
                                && !has_module_getattr(&m)
                                && !defined_on_other_platform(db, &m, &name, 0)
                                && !missing_from_stub(db, &m, &name)
                            {
                                if let Some(t) = alias.name_token() {
                                    out.push(Diagnostic::new(
                                        t.text_range(),
                                        Severity::Error,
                                        "PyUnresolvedReferences",
                                        format!(
                                            "Cannot find reference '{}' in '{}'",
                                            name,
                                            imp.module_path()
                                        ),
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out.sort_by_key(|d| d.range.start());
    out
}

/// Whether `m` is a stub that leaves out `name` although the interpreter's
/// source of the module defines it.
fn missing_from_stub(db: &Analysis, m: &Arc<FileInfo>, name: &str) -> bool {
    m.is_stub
        && m.module_name
            .as_deref()
            .is_some_and(|module| crate::stub_docs::source_defines(db, module, name))
}

/// Whether stub `m` defines `name` for some other platform than the target
/// one (directly or through a star import such as `asyncio.windows_events`).
fn defined_on_other_platform(db: &Analysis, m: &Arc<FileInfo>, name: &str, depth: usize) -> bool {
    let Some(env) = &m.static_env else {
        return false;
    };
    if depth > 2 {
        return false;
    }
    let any = env.any_platform();
    let scopes = crate::scope::build_with(&m.parse, Some(&any));
    let ms = scopes.module_scope();
    if !scopes.bindings_in(ms, name).is_empty() {
        return true;
    }
    scopes.scope(ms).star_imports.iter().any(|star| {
        db.module_relative(m, star.level, star.module.trim_start_matches('.'))
            .is_some_and(|sm| defined_on_other_platform(db, &sm, name, depth + 1))
    })
}

fn has_dynamic_getattr(inf: &Inferer<'_>, c: &crate::types::ClassRef) -> bool {
    for cls in inf.mro(c).iter() {
        if let Some(info) = inf.class_info(cls) {
            if info.members.contains_key("__getattr__")
                || info.members.contains_key("__getattribute__") && cls.name != "object"
            {
                return true;
            }
            if info.is_enum_like || info.decorators.iter().any(|d| d.contains("dataclass")) {
                // Enum members / dataclasses are fine; nothing dynamic.
            }
        }
    }
    false
}

fn has_module_getattr(m: &FileInfo) -> bool {
    m.scopes
        .scope(m.scopes.module_scope())
        .bindings
        .contains_key("__getattr__")
}

/// Code completion at `offset`. `prefix` is the identifier typed so far.
/// Comments, string literals, numbers and new-name positions get no items
/// (after `def` in a class body: the overridable base-class methods).
pub fn complete(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Vec<CompletionItem> {
    complete_with(db, file, offset, Inferer::new(db))
}

/// [`complete`] for interactive lookups: type inference for the candidates
/// stops after `budget` (or once `token` is cancelled), so one request cannot
/// run for seconds through library return types; items found after that keep
/// their names and binding kinds without inferred details.
pub fn complete_bounded(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    budget: std::time::Duration,
    token: Option<rustpy_core::CancellationToken>,
) -> Vec<CompletionItem> {
    let inf = Inferer::new(db).with_budget(std::time::Instant::now() + budget, token);
    complete_with(db, file, offset, inf)
}

fn complete_with(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    inf: Inferer<'_>,
) -> Vec<CompletionItem> {
    let offset = crate::clamp_offset(file, offset);
    if let Some(context) = crate::basic_completion::complete(db, file, offset) {
        return context.items;
    }
    match completion_context(&file.text, offset) {
        CompletionContext::Code => {}
        CompletionContext::DefinitionName => {
            return crate::completion_ctx::override_completions(db, file, offset)
        }
        _ => return Vec::new(),
    }
    let text = &file.text;
    let off = usize::from(offset).min(text.len());
    let before = &text[..off];
    let prefix_start = before
        .rfind(|c: char| !rustpy_core::is_ident_continue(c))
        .map(|i| {
            i + before[i..]
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(1)
        })
        .unwrap_or(0);
    let prefix = &before[prefix_start..];
    // Names `finish` drops anyway need no inferred type.
    let lower_prefix = prefix.to_lowercase();
    let line_start = before[..prefix_start]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    let pre = before[line_start..prefix_start].trim_end();
    let mut items: Vec<CompletionItem> = Vec::new();

    // Import contexts.
    let words: Vec<&str> = pre.split_whitespace().collect();
    let is_from = words.first() == Some(&"from");
    let has_import = words.contains(&"import");
    if (words.first() == Some(&"import") && !is_from && !pre.ends_with('.'))
        || (is_from && has_import && pre.ends_with(',') && false)
    {
        for m in db.resolver.top_level_modules() {
            items.push(CompletionItem {
                label: SmolStr::new(&m),
                kind: CompletionKind::Module,
                detail: String::new(),
                insert: m.clone(),
                caret_offset: None,
                priority: 50,
            });
        }
        return finish(items, prefix);
    }
    if is_from && has_import {
        let module = words.get(1).copied().unwrap_or("");
        let level = module.chars().take_while(|c| *c == '.').count();
        if let Some(m) = db.module_relative(file, level, module.trim_start_matches('.')) {
            let mut labels: std::collections::HashSet<SmolStr> = std::collections::HashSet::new();
            for (name, ty) in inf.all_members(&Type::Module(m.id)) {
                if name.starts_with('_') && !prefix.starts_with('_') {
                    continue;
                }
                labels.insert(name.clone());
                items.push(member_item(&inf, &Type::Module(m.id), &name, &ty));
            }
            // `from . import <caret>`: the package's modules, also for namespace
            // packages and packages without a dotted name.
            if m.is_package_init() && !m.path.starts_with("<typeshed>") {
                for sub in m
                    .path
                    .parent()
                    .map(crate::module::list_dir_modules)
                    .unwrap_or_default()
                {
                    if (!sub.starts_with('_') || prefix.starts_with('_'))
                        && labels.insert(SmolStr::new(&sub))
                    {
                        items.push(CompletionItem {
                            label: SmolStr::new(&sub),
                            kind: CompletionKind::Module,
                            detail: String::new(),
                            insert: sub.clone(),
                            caret_offset: None,
                            priority: 50,
                        });
                    }
                }
            }
        }
        return finish(items, prefix);
    }
    if is_from {
        // `from <prefix>` or `from pkg.<prefix>`: modules and submodules.
        let typed = words.get(1).copied().unwrap_or("");
        let dotted = if pre.ends_with('.') { typed } else { "" };
        let level = dotted.chars().take_while(|c| *c == '.').count();
        let mods = if level > 0 {
            // `from .<caret>` / `from ..pkg.<caret>`: modules of the package the dots
            // (and the dotted path after them) refer to.
            let rest = dotted[level..].trim_end_matches('.');
            relative_package_dir(db, file, level, rest)
                .map(|dir| crate::module::list_dir_modules(&dir))
                .unwrap_or_default()
        } else if dotted.is_empty() {
            db.resolver.top_level_modules()
        } else {
            db.resolver.submodules(dotted.trim_end_matches('.'))
        };
        for m in mods {
            items.push(CompletionItem {
                label: SmolStr::new(&m),
                kind: CompletionKind::Module,
                detail: String::new(),
                insert: m.clone(),
                caret_offset: None,
                priority: 50,
            });
        }
        if !dotted.is_empty() || words.len() <= 1 {
            return finish(items, prefix);
        }
        items.push(CompletionItem {
            label: "import".into(),
            kind: CompletionKind::Keyword,
            detail: String::new(),
            insert: "import ".into(),
            caret_offset: None,
            priority: 0,
        });
        return finish(items, prefix);
    }
    // `import os.<prefix>` → submodules
    if words.first() == Some(&"import") && pre.ends_with('.') {
        let typed = words
            .last()
            .copied()
            .unwrap_or("")
            .rsplit(',')
            .next()
            .unwrap_or("");
        for m in db.resolver.submodules(typed.trim_end_matches('.')) {
            items.push(CompletionItem {
                label: SmolStr::new(&m),
                kind: CompletionKind::Module,
                detail: String::new(),
                insert: m.clone(),
                caret_offset: None,
                priority: 50,
            });
        }
        return finish(items, prefix);
    }

    // Member access: `expr.<prefix>`
    if let Some(stripped) = pre.strip_suffix('.') {
        if !stripped.ends_with('.') {
            let root = file.parse.syntax_node();
            // Find the attribute expression ending right before the dot.
            let dot_off = TextSize::new((line_start + pre.len() - 1) as u32);
            let obj_expr = root
                .token_at_offset(dot_off)
                .left_biased()
                .and_then(|t| t.parent())
                .and_then(|n| {
                    // The object is the left operand of the ATTRIBUTE_EXPR containing the dot, or the node ending at the dot.
                    n.ancestors().find_map(|a| {
                        if a.kind() == SyntaxKind::ATTRIBUTE_EXPR
                            && a.children_with_tokens()
                                .filter_map(|e| e.into_token())
                                .any(|t| {
                                    t.kind() == SyntaxKind::DOT && t.text_range().start() == dot_off
                                })
                        {
                            return ast::AttributeExpr::cast(a).and_then(|x| x.object());
                        }
                        None
                    })
                })
                .or_else(|| {
                    root.token_at_offset(dot_off)
                        .left_biased()
                        .and_then(|t| t.parent())
                        .and_then(|n| {
                            n.ancestors()
                                .filter_map(ast::Expr::cast)
                                .find(|e| e.text_range().end() == dot_off)
                        })
                });
            if let Some(obj) = obj_expr {
                let base = inf.type_of_expr(file, &obj);
                for (name, ty) in inf.all_members(&base) {
                    let private = name.starts_with("__") && !name.ends_with("__");
                    if private && !prefix.starts_with("__") {
                        continue;
                    }
                    let dunder = name.starts_with("__") && name.ends_with("__");
                    if dunder && !prefix.starts_with('_') {
                        continue;
                    }
                    items.push(member_item(&inf, &base, &name, &ty));
                }
            }
            return finish(items, prefix);
        }
    }

    // `name=` for the parameters of the call when an argument starts here.
    items.extend(crate::call_help::keyword_argument_items(
        &inf,
        file,
        offset,
        prefix_start,
    ));

    // Names in scope (LEGB) + builtins + keywords. A function's scope runs to
    // the next statement, so on the blank lines after its body the caret's
    // indentation decides: dedented to the `def` (or further) is outside it.
    let mut scope = file.scopes.scope_at(offset);
    let indent = before[line_start..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .count();
    while let Some(parent) = file.scopes.scope(scope).parent {
        let sc = file.scopes.scope(scope);
        if !matches!(sc.kind, ScopeKind::Function | ScopeKind::Class) {
            break;
        }
        let def_start = usize::from(sc.range.start()).min(text.len());
        let def_line = text[..def_start].rfind('\n').map(|i| i + 1).unwrap_or(0);
        // On the header line itself (`def f(): return x`) the caret is inside.
        if line_start <= def_line || indent > def_start - def_line {
            break;
        }
        scope = parent;
    }
    let mut seen: std::collections::HashSet<SmolStr> = std::collections::HashSet::new();
    let mut cur = Some(scope);
    let mut depth = 0u8;
    let mut first = true;
    // PEP 695 annotation scopes see a directly enclosing class (as in `resolve`).
    let mut annotation_only = true;
    while let Some(s) = cur {
        let sc = file.scopes.scope(s);
        let skip_class = sc.kind == ScopeKind::Class && !first && !annotation_only;
        annotation_only &= sc.kind == ScopeKind::TypeParams;
        if !skip_class {
            for (name, ids) in &sc.bindings {
                if seen.contains(name) {
                    continue;
                }
                let Some(id) = ids
                    .iter()
                    .rev()
                    .find(|id| file.scopes.binding(**id).is_definition())
                else {
                    continue;
                };
                let b = file.scopes.binding(*id);
                // Don't offer the name being typed as its own completion.
                if b.range.start() == TextSize::new(prefix_start as u32) {
                    continue;
                }
                seen.insert(name.clone());
                if !matches_prefix(name, &lower_prefix) {
                    continue;
                }
                let ty = inf.type_of_binding(file, *id);
                let kind = match (&b.kind, &ty) {
                    (BindingKind::Class, _) | (_, Type::Class(..)) => CompletionKind::Class,
                    (BindingKind::Function, _) | (_, Type::Function(_) | Type::Overloaded(_)) => {
                        CompletionKind::Function
                    }
                    (BindingKind::Import(_), Type::Module(_)) => CompletionKind::Module,
                    (
                        BindingKind::Param | BindingKind::StarParam | BindingKind::StarStarParam,
                        _,
                    ) => CompletionKind::Parameter,
                    _ if name
                        .chars()
                        .all(|c| c.is_uppercase() || c == '_' || c.is_ascii_digit())
                        && name.len() > 1 =>
                    {
                        CompletionKind::Constant
                    }
                    _ => CompletionKind::Variable,
                };
                let (insert, caret) = insert_for(&kind, name, &ty);
                items.push(CompletionItem {
                    label: name.clone(),
                    kind,
                    detail: short_detail(&ty),
                    insert,
                    caret_offset: caret,
                    priority: depth,
                });
            }
        }
        if !skip_class {
            for star in &sc.star_imports {
                if let Some(module) =
                    db.module_relative(file, star.level, star.module.trim_start_matches('.'))
                {
                    for (name, ty) in inf.star_members(&module) {
                        if !seen.insert(name.clone()) {
                            continue;
                        }
                        let mut item = member_item(&inf, &Type::Module(module.id), &name, &ty);
                        item.priority = depth;
                        items.push(item);
                    }
                }
            }
        }
        first = false;
        depth = depth.saturating_add(1);
        cur = sc.parent;
    }
    if let Some(b) = db.builtins() {
        let scope = b.scopes.module_scope();
        for (name, ids) in &b.scopes.scope(scope).bindings {
            if seen.contains(name) || name.starts_with('_') {
                continue;
            }
            let Some(id) = ids
                .iter()
                .rev()
                .find(|id| b.scopes.binding(**id).is_definition())
            else {
                continue;
            };
            seen.insert(name.clone());
            if !matches_prefix(name, &lower_prefix) {
                continue;
            }
            let ty = inf.type_of_binding(&b, *id);
            let kind = match &ty {
                Type::Class(..) => CompletionKind::Class,
                Type::Function(_) | Type::Overloaded(_) => CompletionKind::Function,
                _ => CompletionKind::Variable,
            };
            let (insert, caret) = insert_for(&kind, name, &ty);
            items.push(CompletionItem {
                label: name.clone(),
                kind,
                detail: short_detail(&ty),
                insert,
                caret_offset: caret,
                priority: 40,
            });
        }
    }
    for kw in PYTHON_KEYWORDS {
        if !seen.contains(*kw) && crate::basic_completion::keyword_allowed(db, file, offset, kw) {
            items.push(CompletionItem {
                label: SmolStr::new(kw),
                kind: CompletionKind::Keyword,
                detail: String::new(),
                insert: kw.to_string(),
                caret_offset: None,
                priority: 60,
            });
        }
    }
    finish(items, prefix)
}

/// Directory of the package `from <level dots><rest>` refers to in `file`
/// (`None` when it is a plain module or does not exist).
fn relative_package_dir(
    db: &Analysis,
    file: &FileInfo,
    level: usize,
    rest: &str,
) -> Option<PathBuf> {
    if file.path.starts_with("<typeshed>") {
        return None;
    }
    if rest.is_empty() {
        return crate::module::ModuleResolver::relative_package_dir(&file.path, level)
            .filter(|d| d.is_dir());
    }
    match db.resolver.resolve_relative(&file.path, level, rest)? {
        crate::module::ModuleSource::File {
            path,
            is_package: true,
        } => path.parent().map(PathBuf::from),
        crate::module::ModuleSource::Namespace(dir) => Some(dir),
        _ => None,
    }
}

pub(crate) fn member_item(
    inf: &Inferer<'_>,
    base: &Type,
    name: &SmolStr,
    ty: &Type,
) -> CompletionItem {
    let kind = match ty {
        Type::Class(..) => CompletionKind::Class,
        Type::Module(_) => CompletionKind::Module,
        Type::Function(_) | Type::Overloaded(_) => CompletionKind::Function,
        Type::BoundMethod(..) => CompletionKind::Method,
        _ => match base {
            Type::Module(_) => CompletionKind::Variable,
            _ => {
                let is_prop = base
                    .class_ref()
                    .and_then(|c| {
                        inf.mro(c).iter().find_map(|cls| {
                            inf.class_info(cls).and_then(|i| {
                                i.members.get(name).map(|m| m.kind == MemberKind::Property)
                            })
                        })
                    })
                    .unwrap_or(false);
                if is_prop {
                    CompletionKind::Property
                } else {
                    CompletionKind::Attribute
                }
            }
        },
    };
    let (insert, caret) = insert_for(&kind, name, ty);
    CompletionItem {
        label: name.clone(),
        kind,
        detail: short_detail(ty),
        insert,
        caret_offset: caret,
        priority: 10,
    }
}

fn insert_for(kind: &CompletionKind, name: &str, ty: &Type) -> (String, Option<usize>) {
    match kind {
        CompletionKind::Function | CompletionKind::Method => {
            let no_params = ty.signature().map(|s| s.params.is_empty()).unwrap_or(false);
            if no_params {
                (format!("{name}()"), None)
            } else {
                (format!("{name}()"), Some(name.len() + 1))
            }
        }
        _ => (name.to_string(), None),
    }
}

fn short_detail(ty: &Type) -> String {
    match ty {
        Type::Function(sig) | Type::BoundMethod(sig, _) => {
            format!("({}) -> {}", sig.display_params(), sig.ret)
        }
        Type::Overloaded(sigs) => format!("({}) -> {}", sigs[0].display_params(), sigs[0].ret),
        Type::Class(c, _) => format!("class {}", c.name),
        Type::Module(_) => "module".into(),
        Type::Unknown => String::new(),
        other => other.to_string(),
    }
}

/// A `name=` keyword-argument item; it sorts above other items of the same
/// rank (a local `name` included).
pub fn is_keyword_argument(item: &CompletionItem) -> bool {
    item.kind == CompletionKind::Parameter && item.label.ends_with('=')
}

pub(crate) fn finish(mut items: Vec<CompletionItem>, prefix: &str) -> Vec<CompletionItem> {
    let lower = prefix.to_lowercase();
    items.retain(|i| matches_prefix(&i.label, &lower));
    items.sort_by(|a, b| {
        let a_exact = a.label.to_lowercase().starts_with(&lower);
        let b_exact = b.label.to_lowercase().starts_with(&lower);
        b_exact
            .cmp(&a_exact)
            .then(a.priority.cmp(&b.priority))
            .then_with(|| is_keyword_argument(b).cmp(&is_keyword_argument(a)))
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
    });
    items.dedup_by(|a, b| a.label == b.label && a.kind == b.kind);
    items
}

/// Prefix, camel-hump (`gV` → `getValue`) and subsequence matching.
pub fn matches_prefix(label: &str, lower_prefix: &str) -> bool {
    if lower_prefix.is_empty() {
        return true;
    }
    let ll = label.to_lowercase();
    if ll.starts_with(lower_prefix) || ll.contains(lower_prefix) {
        return true;
    }
    // Subsequence match on word starts (snake_case / camelCase humps).
    let mut it = lower_prefix.chars().peekable();
    let mut prev = '_';
    for c in label.chars() {
        let is_start = prev == '_' || (c.is_uppercase() && !prev.is_uppercase());
        if let Some(&want) = it.peek() {
            if c.to_lowercase().next() == Some(want)
                && (is_start
                    || prev.to_lowercase().next() == it.clone().next().and(None).or(Some(prev)))
            {
                it.next();
            }
        }
        prev = c;
    }
    if it.peek().is_none() {
        return true;
    }
    // A fuzzy subsequence can span the middle of an identifier as well as
    // its humps. The lookup ranks it after contiguous and hump matches.
    let mut wanted = lower_prefix.chars();
    let mut next = wanted.next();
    for ch in ll.chars() {
        if next == Some(ch) {
            next = wanted.next();
        }
    }
    next.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn setup(src: &str) -> (Analysis, Arc<FileInfo>) {
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let f = db.file(id).unwrap();
        (db, f)
    }

    #[test]
    fn go_to_definition() {
        let src = "import os\nfrom os import path\n\ndef helper(x):\n    return x\n\nclass C:\n    def m(self):\n        return helper(self)\n\nc = C()\nc.m()\nos.getcwd()\npath.join('a')\n";
        let (db, f) = setup(src);
        let at =
            |needle: &str, extra: usize| TextSize::new((src.find(needle).unwrap() + extra) as u32);
        let d = definition_at(&db, &f, at("helper(self)", 1));
        assert_eq!(d.len(), 1);
        assert_eq!(&src[d[0].range], "helper");
        let d = definition_at(&db, &f, at("c.m()", 2));
        assert_eq!(&src[d[0].range], "m");
        let d = definition_at(&db, &f, at("os.getcwd", 4));
        assert!(d[0].path.to_string_lossy().contains("os"));
        assert!(d[0].is_stub);
        let d = definition_at(&db, &f, at("import os", 7));
        assert!(d[0].path.to_string_lossy().contains("<typeshed>/os"));
        let d = definition_at(&db, &f, at("path.join", 0));
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn hover_and_diagnostics() {
        let src = "import os\nimport nonexistent_mod\nfrom os import nope\n\ndef greet(name: str, excited: bool = False) -> str:\n    '''Say hi.'''\n    return name\n\nv = greet('a')\nundefined_name\nos.not_a_thing\nv.upper()\n";
        let (db, f) = setup(src);
        let at = |needle: &str| TextSize::new(src.find(needle).unwrap() as u32);
        let h = hover_at(&db, &f, at("greet('a')")).unwrap();
        assert_eq!(
            h.signature,
            "def greet(name: str, excited: bool = False) -> str"
        );
        assert_eq!(h.docstring.as_deref(), Some("Say hi."));
        let h = hover_at(&db, &f, at("v = greet")).unwrap();
        assert_eq!(h.signature, "v: str");
        let diags = diagnostics(&db, &f);
        let msgs: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert!(
            msgs.contains(&"No module named 'nonexistent_mod'"),
            "{msgs:?}"
        );
        assert!(
            msgs.contains(&"Cannot find reference 'nope' in 'os'"),
            "{msgs:?}"
        );
        assert!(
            msgs.contains(&"Unresolved reference 'undefined_name'"),
            "{msgs:?}"
        );
        assert!(
            msgs.contains(&"Cannot find reference 'not_a_thing' in 'os'"),
            "{msgs:?}"
        );
        assert_eq!(msgs.iter().filter(|m| m.contains("upper")).count(), 0);
    }

    #[test]
    fn bounded_completion_keeps_candidates_when_inference_is_out_of_budget() {
        let src = "def helper():\n    return 1\n\nhandle = helper()\nhe";
        let (db, f) = setup(src);
        let end = TextSize::new(src.len() as u32);
        let full = complete(&db, &f, end);
        let token = rustpy_core::CancellationToken::new();
        token.cancel();
        let started = std::time::Instant::now();
        let bounded = complete_bounded(
            &db,
            &f,
            end,
            std::time::Duration::from_secs(60),
            Some(token),
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        let labels = |items: &[CompletionItem]| -> Vec<String> {
            items.iter().map(|i| i.label.to_string()).collect()
        };
        assert_eq!(labels(&bounded), labels(&full));
        let helper = bounded.iter().find(|i| i.label == "helper").unwrap();
        assert_eq!(helper.kind, CompletionKind::Function);
        // A spent budget leaves details out, not candidates, and an unbounded
        // request afterwards still infers them.
        let handle = full.iter().find(|i| i.label == "handle").unwrap();
        assert_eq!(handle.detail, "int");
        assert_eq!(complete(&db, &f, end), full);
        let generous = complete_bounded(&db, &f, end, std::time::Duration::from_secs(60), None);
        assert_eq!(generous, full);
    }

    #[test]
    fn completion_names_members_and_imports() {
        let src = "import os\n\nclass Dog:\n    def __init__(self):\n        self.name = 'rex'\n    def bark(self): ...\n\nd = Dog()\nd.\n";
        let (db, f) = setup(src);
        let off = TextSize::new(src.rfind("d.").unwrap() as u32 + 2);
        let items = complete(&db, &f, off);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"name"), "{labels:?}");
        assert!(labels.contains(&"bark"), "{labels:?}");
        assert!(!labels.contains(&"__init__"));
        let bark = items.iter().find(|i| i.label == "bark").unwrap();
        assert_eq!(bark.kind, CompletionKind::Method);
        assert_eq!(bark.insert, "bark()");
        // Names in scope.
        let src2 = "value = 1\ndef func(param):\n    va\n";
        let (db, f) = setup(src2);
        let off = TextSize::new(src2.rfind("va").unwrap() as u32 + 2);
        let items = complete(&db, &f, off);
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels[0], "value", "{labels:?}");
        assert!(labels.contains(&"ValueError"));
        // Module members after `os.`
        let src3 = "import os\nos.pa\n";
        let (db, f) = setup(src3);
        let items = complete(&db, &f, TextSize::new(src3.len() as u32 - 1));
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"path"), "{labels:?}");
        // import completion
        let src4 = "import js\n";
        let (db, f) = setup(src4);
        let items = complete(&db, &f, TextSize::new(9));
        assert!(items.iter().any(|i| i.label == "json"));
        let src5 = "from os import ge\n";
        let (db, f) = setup(src5);
        let items = complete(&db, &f, TextSize::new(src5.len() as u32 - 1));
        assert!(items.iter().any(|i| i.label == "getcwd"));
    }

    #[test]
    fn parameter_info() {
        let src = "def greet(name: str, excited: bool = False, *, loud=1) -> str: ...\ngreet('a', loud=2)\ngreet('a', ex\n";
        let (db, f) = setup(src);
        let off = TextSize::new(src.find("'a', ex").unwrap() as u32 + 5);
        let h = signature_help(&db, &f, off).unwrap();
        assert_eq!(
            h.label,
            "greet(name: str, excited: bool = False, *, loud: int = 1) -> str"
        );
        assert_eq!(h.active, Some(1));
        assert_eq!(h.params.len(), 3);
        let off2 = TextSize::new(src.find("loud=2").unwrap() as u32 + 3);
        let h2 = signature_help(&db, &f, off2).unwrap();
        assert_eq!(h2.active, Some(2));
        let src3 = "import os\nos.path.join('a', \n";
        let (db, f) = setup(src3);
        let h3 = signature_help(&db, &f, TextSize::new(src3.len() as u32 - 1)).unwrap();
        assert!(h3.label.starts_with("join("), "{}", h3.label);
        assert!(h3.overloads >= 2);
    }

    #[test]
    fn prefix_matching() {
        assert!(matches_prefix("getValue", "gv"));
        assert!(matches_prefix("get_value", "gv"));
        assert!(matches_prefix("ValueError", "err"));
        assert!(!matches_prefix("abc", "xyz"));
    }
}

// ---------------------------------------------------------------------------
// Inlay hints
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InlayKind {
    /// `name:` before a positional argument.
    Parameter,
    /// `: Type` after an assignment target.
    Type,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlayHint {
    pub offset: TextSize,
    pub label: String,
    pub kind: InlayKind,
}

const NO_PARAM_HINTS: &[&str] = &[
    "print",
    "len",
    "str",
    "int",
    "float",
    "bool",
    "list",
    "dict",
    "set",
    "tuple",
    "repr",
    "isinstance",
    "issubclass",
    "hasattr",
    "getattr",
    "setattr",
    "delattr",
    "range",
    "enumerate",
    "zip",
    "map",
    "filter",
    "sorted",
    "reversed",
    "min",
    "max",
    "sum",
    "abs",
    "round",
    "open",
    "super",
    "type",
    "id",
    "hash",
    "iter",
    "next",
    "format",
    "input",
    "any",
    "all",
    "ord",
    "chr",
    "divmod",
    "pow",
    "append",
    "add",
    "remove",
    "discard",
    "pop",
    "insert",
    "extend",
    "index",
    "count",
    "join",
    "split",
    "strip",
    "startswith",
    "endswith",
    "get",
    "update",
    "write",
    "read",
    "exit",
];

/// Parameter-name and variable-type inlay hints for a file.
pub fn inlay_hints(
    db: &Analysis,
    file: &Arc<FileInfo>,
    params: bool,
    types: bool,
) -> Vec<InlayHint> {
    let inf = Inferer::new(db);
    let root = file.parse.syntax_node();
    let mut out = Vec::new();
    for (index, node) in root.descendants().enumerate() {
        // Hints of a cancelled run are discarded (see `with_cancellation`).
        if index % 256 == 0 && crate::infer::ambient_cancelled() {
            break;
        }
        match node.kind() {
            SyntaxKind::CALL_EXPR if params => {
                let Some(call) = ast::CallExpr::cast(node.clone()) else {
                    continue;
                };
                let Some(callee) = call.callee() else {
                    continue;
                };
                let callee_name = match &callee {
                    ast::Expr::Name(n) => n.name_text(),
                    ast::Expr::Attribute(a) => a.attr_text(),
                    _ => String::new(),
                };
                if NO_PARAM_HINTS.contains(&callee_name.as_str()) {
                    continue;
                }
                let args: Vec<ast::AnyArg> = call
                    .arg_list()
                    .map(|l| l.args().collect())
                    .unwrap_or_default();
                let positional: Vec<ast::Expr> = args
                    .iter()
                    .filter_map(|a| match a {
                        ast::AnyArg::Positional(p) => p.expr(),
                        _ => None,
                    })
                    .collect();
                if positional.is_empty() || args.iter().any(|a| matches!(a, ast::AnyArg::Star(_))) {
                    continue;
                }
                // The overload the arguments select.
                let sigs = crate::call_help::signatures_of(&inf, &inf.type_of_expr(file, &callee));
                let sig = if sigs.len() == 1 {
                    sigs.into_iter().next()
                } else {
                    let (pos, kw) = inf.call_args(file, &call);
                    crate::call_help::select_for_call(&inf, &sigs, &pos, &kw)
                };
                let Some(sig) = sig else { continue };
                for (pi, arg) in positional.iter().enumerate() {
                    let Some(p) = sig.params.get(pi) else { break };
                    match p.kind {
                        crate::types::ParamKind::VarArgs => break,
                        crate::types::ParamKind::KeywordOnly | crate::types::ParamKind::KwArgs => {
                            break
                        }
                        _ => {}
                    }
                    let text = arg.syntax().text().to_string();
                    let simple = text.rsplit('.').next().unwrap_or(&text).trim().to_string();
                    let pname = p.name.to_string();
                    if simple == pname
                        || pname.len() <= 1
                        || pname.starts_with('_')
                        || text.contains(&format!("{pname}="))
                    {
                        continue;
                    }
                    // Skip when the argument is itself a call to something named like the parameter.
                    if text.starts_with(&format!("{pname}(")) {
                        continue;
                    }
                    out.push(InlayHint {
                        offset: arg.syntax().text_range().start(),
                        label: format!("{pname}:"),
                        kind: InlayKind::Parameter,
                    });
                }
            }
            SyntaxKind::ASSIGN_STMT if types => {
                let Some(assign) = ast::AssignStmt::cast(node.clone()) else {
                    continue;
                };
                let targets = assign.targets();
                if targets.len() != 1 {
                    continue;
                }
                let ast::Expr::Name(name) = &targets[0] else {
                    continue;
                };
                let Some(value) = assign.value() else {
                    continue;
                };
                // Obvious cases are not annotated (literals, displays, constructor calls, lambdas).
                let obvious = match &value {
                    ast::Expr::Literal(_)
                    | ast::Expr::List(_)
                    | ast::Expr::Dict(_)
                    | ast::Expr::Set(_)
                    | ast::Expr::Tuple(_)
                    | ast::Expr::Lambda(_)
                    | ast::Expr::String(_)
                    | ast::Expr::FString(_) => true,
                    ast::Expr::Call(c) => {
                        matches!(c.callee(), Some(ast::Expr::Name(n)) if n.name_text().chars().next().is_some_and(|ch| ch.is_uppercase()))
                    }
                    _ => false,
                };
                if obvious {
                    continue;
                }
                let ty = inf.type_of_expr(file, &value);
                if ty.is_unknown() {
                    continue;
                }
                let label = format!("{ty}");
                if label.is_empty()
                    || label == "None"
                    || label.len() > 40
                    || label.contains("Unknown")
                {
                    continue;
                }
                out.push(InlayHint {
                    offset: name.syntax().text_range().end(),
                    label: format!(": {label}"),
                    kind: InlayKind::Type,
                });
            }
            _ => {}
        }
    }
    out.sort_by_key(|h| h.offset);
    out
}

#[cfg(test)]
mod hover_keyword_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn keywords_have_no_hover_and_unknown_reads_any() {
        let db = Analysis::for_project(Path::new("/proj"));
        let src = "def calc(a, b):\n    return a * b\n";
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let file = db.file(id).unwrap();
        assert!(
            hover_at(&db, &file, TextSize::new(1)).is_none(),
            "hovering `def` shows nothing"
        );
        let h = hover_at(&db, &file, TextSize::new(5)).expect("hover on the name");
        assert!(h.signature.starts_with("def calc(a, b)"), "{}", h.signature);
        assert!(!h.signature.contains("Unknown"), "{}", h.signature);
    }
}

#[cfg(test)]
mod type_definition_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn goes_from_values_to_their_classes() {
        let db = Analysis::for_project(Path::new("/proj"));
        let src = "from typing import Optional\n\nclass Engine:\n    pass\n\nclass Car:\n    engine: Engine\n\n    def __init__(self, engine: Engine) -> None:\n        self.engine = engine\n\ndef build() -> Car:\n    return Car(Engine())\n\ndef maybe(flag: bool) -> Optional[Car]:\n    return None\n\ndef either(x: Car | Engine) -> None:\n    pass\n\ncar = build()\ncar.engine\nm = maybe(True)\nname = 'x'\n";
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let file = db.file(id).unwrap();
        let at =
            |needle: &str, extra: usize| TextSize::new((src.find(needle).unwrap() + extra) as u32);
        let names = |locs: Vec<Location>| -> Vec<String> {
            locs.iter()
                .map(|l| {
                    if l.is_stub {
                        format!("stub:{}", l.path.display())
                    } else {
                        src[l.range].to_string()
                    }
                })
                .collect()
        };
        // A variable assigned from a call: its inferred class.
        assert_eq!(
            names(type_definition_at(&db, &file, at("car = build", 0))),
            vec!["Car"]
        );
        // An attribute: the declared attribute type.
        assert_eq!(
            names(type_definition_at(&db, &file, at("car.engine", 5))),
            vec!["Engine"]
        );
        // A parameter (annotated): its class.
        assert_eq!(
            names(type_definition_at(&db, &file, at("engine: Engine) ->", 0))),
            vec!["Engine"]
        );
        // A function name: its return type; Optional skips None.
        assert_eq!(
            names(type_definition_at(&db, &file, at("def build", 4))),
            vec!["Car"]
        );
        assert_eq!(
            names(type_definition_at(&db, &file, at("m = maybe", 0))),
            vec!["Car"]
        );
        // A union: every member.
        assert_eq!(
            names(type_definition_at(&db, &file, at("x: Car", 0))),
            vec!["Car", "Engine"]
        );
        // Builtins resolve into the typeshed stubs.
        let n = names(type_definition_at(&db, &file, at("name = 'x'", 0)));
        assert_eq!(n.len(), 1);
        assert!(
            n[0].starts_with("stub:") && n[0].contains("builtins"),
            "{n:?}"
        );
        // Nothing to go to on a keyword.
        assert!(type_definition_at(&db, &file, at("def build", 0)).is_empty());
    }
}
