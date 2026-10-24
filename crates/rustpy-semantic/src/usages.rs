//! Find Usages, Highlight Usages and symbol resolution (roadmap §2.7–2.8).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustpy_core::{TextRange, TextSize};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::{SyntaxKind, SyntaxNode};

use crate::db::{Analysis, FileInfo};
use crate::features::{ident_at, keyword_target, KeywordTarget};
use crate::infer::{Definition, Inferer};
use crate::scope::{BindingKind, ScopeKind};
use crate::types::Type;

/// One usage of a symbol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Usage {
    pub path: PathBuf,
    pub range: TextRange,
    pub line: usize,
    /// Byte offset of the usage in `line_text`.
    pub column: usize,
    pub line_text: String,
    pub is_write: bool,
    pub is_definition: bool,
    /// Matched by name only: an attribute of a receiver whose type is
    /// unknown (`worker.run()` with an unannotated `worker`) or may be
    /// another class with a member of that name (`x.run()` with `x: Job |
    /// Robot`), or a keyword argument of a call that does not resolve. It
    /// may belong to another symbol of the same name, so Rename leaves it
    /// out unless chosen.
    pub dynamic: bool,
    /// An occurrence in a comment or string literal, not code.
    pub non_code: bool,
}

/// The symbol under the caret, resolved to its definition(s).
#[derive(Clone, Debug)]
pub enum Symbol {
    /// A name bound in a scope (variable, function, class, parameter, import).
    /// Keyword arguments at call sites resolve to the parameter they name.
    Name { def: Definition, name: String },
    /// An attribute/member of a class or module.
    Member {
        owner: Type,
        name: String,
        decl: Option<(Arc<FileInfo>, TextRange)>,
    },
    /// A keyword argument by name only. [`symbol_at`] never produces it: a
    /// keyword whose callee cannot be resolved has no symbol, since matching
    /// every same-named keyword in the project would hit unrelated calls.
    Keyword { name: String },
}

impl Symbol {
    pub fn name(&self) -> &str {
        match self {
            Symbol::Name { name, .. } | Symbol::Member { name, .. } | Symbol::Keyword { name } => {
                name
            }
        }
    }

    /// What the symbol is, for dialogs ("Rename method 'run'…").
    pub fn kind(&self) -> &'static str {
        match self {
            Symbol::Name { def, .. } => {
                let b = def.binding();
                let in_class = def.file.scopes.scope(b.scope).kind == ScopeKind::Class;
                match &b.kind {
                    BindingKind::Function if in_class => "method",
                    BindingKind::Function => "function",
                    BindingKind::Class => "class",
                    BindingKind::Param | BindingKind::StarParam | BindingKind::StarStarParam => {
                        "parameter"
                    }
                    BindingKind::Import(_) => "import",
                    _ if in_class => "class attribute",
                    _ => "variable",
                }
            }
            Symbol::Member {
                decl: Some((f, r)), ..
            } if f
                .scopes
                .binding_at(r.start())
                .is_some_and(|(_, b)| b.range == *r && matches!(b.kind, BindingKind::Function)) =>
            {
                "method"
            }
            Symbol::Member { .. } => "attribute",
            Symbol::Keyword { .. } => "keyword argument",
        }
    }
}

/// Identifies the symbol at `offset`.
pub fn symbol_at(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Option<Symbol> {
    let offset = crate::clamp_offset(file, offset);
    if let Some(definition) = crate::pytest_fixtures::resolve(db, file, offset) {
        return Some(Symbol::Name {
            name: definition.binding().name.to_string(),
            def: definition,
        });
    }
    let inf = Inferer::new(db);
    let tok = ident_at(file, offset)?;
    let parent = tok.parent()?;
    let name = tok.text().to_string();
    match parent.kind() {
        SyntaxKind::NAME_EXPR => {
            let scope = file.scopes.scope_at(offset);
            let def = inf.resolve_name(file, scope, &name)?;
            // On a declaration (`size: float = 1.5`): that binding.
            let at = file
                .scopes
                .binding_at(offset)
                .map(|(_, b)| b.range)
                .filter(|r| *r == tok.text_range());
            Some(class_attribute(db, &inf, &def, &name, at).unwrap_or(Symbol::Name { def, name }))
        }
        SyntaxKind::ATTRIBUTE_EXPR => {
            let attr = ast::AttributeExpr::cast(parent)?;
            if attr.attr().as_ref() != Some(&tok) {
                return None;
            }
            let owner = member_owner(inf.type_of_expr(file, &attr.object()?));
            // `module.name` is the module's own `name`: its other usages are
            // plain names in that module and `from module import name` elsewhere.
            if let Type::Module(fid) = &owner {
                if let Some(def) = db
                    .file(*fid)
                    .and_then(|m| inf.resolve_in_module(&m, &name, 0))
                {
                    return Some(Symbol::Name { def, name });
                }
            }
            let decl = member_decl(db, &inf, &owner, &name);
            Some(Symbol::Member { owner, name, decl })
        }
        SyntaxKind::KEYWORD_ARG => match keyword_target(db, &inf, file, &tok) {
            KeywordTarget::Param(def) => Some(Symbol::Name { def, name }),
            // `Point(x=1)` of a dataclass: the field `x`.
            _ => {
                let (cls, decl) = generated_init_field(db, &inf, file, &parent, &name)?;
                Some(Symbol::Member {
                    owner: Type::Instance(cls, vec![]),
                    name,
                    decl: Some(decl),
                })
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
        | SyntaxKind::MATCH_AS_PATTERN
        | SyntaxKind::MATCH_STAR_PATTERN => {
            // A definition site: the binding at this position.
            let (id, b) = file.scopes.binding_at(offset)?;
            if b.range != tok.text_range() {
                return None;
            }
            let def = Definition {
                file: file.clone(),
                binding: id,
            };
            // Methods and nested classes declared in a class body are members.
            if matches!(parent.kind(), SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF) {
                if let Some(sym) = class_attribute(db, &inf, &def, &name, Some(tok.text_range())) {
                    return Some(sym);
                }
            }
            Some(Symbol::Name { def, name })
        }
        _ => None,
    }
}

/// A name bound in a class body is a class attribute (`size = 1`, the
/// dataclass field `x: float`, a method, a nested class): a member, so its
/// usages include `self.size` and `Unit.size` besides the bare name inside
/// the class body. `at` is the declaration under the caret, if any.
fn class_attribute(
    db: &Analysis,
    inf: &Inferer<'_>,
    def: &Definition,
    name: &str,
    at: Option<TextRange>,
) -> Option<Symbol> {
    let b = def.binding();
    if matches!(
        b.kind,
        BindingKind::Import(_)
            | BindingKind::Global
            | BindingKind::Nonlocal
            | BindingKind::Del
            | BindingKind::TypeParam
    ) {
        return None;
    }
    let cls = enclosing_class_of_binding(&def.file, b.scope)?;
    let owner = Type::Instance(cls, vec![]);
    let decl = match at {
        Some(r) => (def.file.clone(), r),
        None => member_decl(db, inf, &owner, name).unwrap_or_else(|| (def.file.clone(), b.range)),
    };
    Some(Symbol::Member {
        owner,
        name: name.to_string(),
        decl: Some(decl),
    })
}

/// The keyword argument `kw` (named `name`) of a call constructing a
/// dataclass-like class through its generated `__init__` (`Point(x=1)`): the
/// class and the declaration of the field it sets.
fn generated_init_field(
    db: &Analysis,
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    kw: &SyntaxNode,
    name: &str,
) -> Option<(crate::types::ClassRef, (Arc<FileInfo>, TextRange))> {
    let call = kw
        .parent()
        .filter(|p| p.kind() == SyntaxKind::ARG_LIST)
        .and_then(|l| l.parent())
        .and_then(ast::CallExpr::cast)?;
    let Type::Class(cls, _) = inf.type_of_expr(file, &call.callee()?) else {
        return None;
    };
    let (sig, _) = inf.generated_constructor(&cls)?;
    sig.params.iter().find(|p| p.name == name).filter(|p| {
        !matches!(
            p.kind,
            crate::types::ParamKind::PositionalOnly
                | crate::types::ParamKind::VarArgs
                | crate::types::ParamKind::KwArgs
        )
    })?;
    let decl = member_decl(db, inf, &Type::Instance(cls.clone(), vec![]), name)?;
    Some((cls, decl))
}

/// Whether `a` and `b` are the same class or one derives from the other.
fn related_classes(
    inf: &Inferer<'_>,
    a: &crate::types::ClassRef,
    b: &crate::types::ClassRef,
) -> bool {
    a == b || inf.mro(a).contains(b) || inf.mro(b).contains(a)
}

/// The receiver type a member is looked up on: `Job | None` (or with an
/// unknown part) is a `Job`, whose member the attribute must be.
fn member_owner(owner: Type) -> Type {
    if let Type::Union(parts) = &owner {
        let known: Vec<&Type> = parts
            .iter()
            .filter(|t| !matches!(t, Type::None | Type::Unknown | Type::Any))
            .collect();
        if let [only] = known.as_slice() {
            return (*only).clone();
        }
    }
    owner
}

fn enclosing_class_of_binding(
    file: &FileInfo,
    scope: crate::scope::ScopeId,
) -> Option<crate::types::ClassRef> {
    let s = file.scopes.scope(scope);
    if s.kind == crate::scope::ScopeKind::Class {
        return Some(crate::types::ClassRef {
            file: file.id,
            range: s.range,
            name: s.name.clone().unwrap_or_default(),
        });
    }
    None
}

fn member_decl(
    db: &Analysis,
    inf: &Inferer<'_>,
    owner: &Type,
    name: &str,
) -> Option<(Arc<FileInfo>, TextRange)> {
    match owner {
        Type::Super { after, receiver } => {
            let (classes, _) = inf.super_mro(after, receiver)?;
            let m = classes.iter().find_map(|(cls, _)| {
                inf.class_info(cls)
                    .and_then(|info| info.members.get(name).cloned())
            })?;
            Some((db.file(m.file)?, m.range))
        }
        Type::Instance(c, args) | Type::Class(c, args) => {
            for (cls, _) in inf.mro_with_args(c, args) {
                if let Some(info) = inf.class_info(&cls) {
                    if let Some(m) = info.members.get(name) {
                        let f = db.file(m.file)?;
                        return Some((f, m.range));
                    }
                }
            }
            None
        }
        Type::Module(fid) => {
            let m = db.file(*fid)?;
            let def = inf.resolve_in_module(&m, name, 0)?;
            Some((def.file.clone(), def.binding().range))
        }
        _ => None,
    }
}

/// The physical declaration selected by ordinary member lookup. Refactoring
/// string references must use this identity rather than a matching member type.
pub fn member_declaration(
    db: &Analysis,
    owner: &Type,
    name: &str,
) -> Option<(Arc<FileInfo>, TextRange)> {
    member_decl(db, &Inferer::new(db), owner, name)
}

/// `(line, column, line text)` of `range`'s start.
pub fn line_info(file: &FileInfo, range: TextRange) -> (usize, usize, String) {
    let text = &file.text;
    let mut start: usize = usize::from(range.start()).min(text.len());
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let line = text[..start].matches('\n').count();
    let line_start = text[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_end = text[line_start..]
        .find('\n')
        .map(|i| line_start + i)
        .unwrap_or(text.len());
    (
        line,
        start - line_start,
        text[line_start..line_end].to_string(),
    )
}

/// A usage of `range` in `file`, with its line for display.
pub fn usage_at(file: &FileInfo, range: TextRange, is_write: bool, is_definition: bool) -> Usage {
    let (line, column, line_text) = line_info(file, range);
    Usage {
        path: file.path.clone(),
        range,
        line,
        column,
        line_text,
        is_write,
        is_definition,
        dynamic: false,
        non_code: false,
    }
}

/// How an attribute access relates to a member being searched for.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum MemberMatch {
    No,
    /// The receiver's type is the member's class, a base or a subclass.
    Static,
    /// The receiver's type is unknown, or may be another class with a
    /// member of that name: same name, possibly another symbol.
    Dynamic,
}

/// Does `receiver.<name>` refer to the member `name` of `owner` (whose
/// class is `owner_class`)? A union matches for certain through one of its
/// parts only when no other part has a `name` of its own (`Job | None`):
/// with `x: Job | Robot` and a `Robot.run`, `x.run()` may be either.
fn member_match(
    inf: &Inferer<'_>,
    receiver: &Type,
    owner: &Type,
    owner_class: Option<&crate::types::ClassRef>,
    name: &str,
) -> MemberMatch {
    match (receiver, owner) {
        (Type::Module(a), Type::Module(b)) => {
            if a == b {
                MemberMatch::Static
            } else {
                MemberMatch::No
            }
        }
        (Type::Union(parts), _) => {
            let matches: Vec<MemberMatch> = parts
                .iter()
                .map(|p| member_match(inf, p, owner, owner_class, name))
                .collect();
            if matches.contains(&MemberMatch::Static) {
                let other_member = parts
                    .iter()
                    .zip(&matches)
                    .any(|(p, m)| *m == MemberMatch::No && inf.lookup_member(p, name).is_some());
                if other_member {
                    MemberMatch::Dynamic
                } else {
                    MemberMatch::Static
                }
            } else if matches.contains(&MemberMatch::Dynamic) {
                MemberMatch::Dynamic
            } else {
                MemberMatch::No
            }
        }
        _ => match (receiver.class_ref(), owner_class) {
            (Some(a), Some(b)) => {
                if a == b || inf.mro(a).iter().any(|c| c == b) || inf.mro(b).iter().any(|c| c == a)
                {
                    MemberMatch::Static
                } else {
                    MemberMatch::No
                }
            }
            _ if matches!(receiver, Type::Unknown | Type::Any) => MemberMatch::Dynamic,
            _ => MemberMatch::No,
        },
    }
}

/// A function parameter that keyword arguments at call sites can name.
struct KeywordParam {
    /// Callee names of calls to the function: its name, plus the class name
    /// for `__init__` / `__new__` (`Box(size=1)`).
    callee_names: Vec<String>,
    /// The `def` statement (for the docstring).
    func_range: TextRange,
    /// For a method: its name. Calls through a receiver of unknown type
    /// (`obj.grow(by=1)`) are matched by it, as method usages are; a plain
    /// function is only called through names that resolve to it.
    method_name: Option<String>,
    /// For `__init__` / `__new__`: the class and the method, whose
    /// subclasses inherit the constructor (`Big(size=1)`).
    constructor: Option<(ast::ClassDef, ast::FuncDef)>,
    /// For a method: its class (calls on receivers related to it through the
    /// MRO match, as method usages do: a mixin calling `self.greet(name=…)`).
    owner: Option<crate::types::ClassRef>,
}

/// `def` is a parameter that can be passed by keyword (not `self`/`cls`,
/// positional-only, `*args` or `**kwargs`).
fn keyword_param(def: &Definition) -> Option<KeywordParam> {
    let b = def.binding();
    if b.kind != BindingKind::Param {
        return None;
    }
    let scopes = &def.file.scopes;
    let scope = scopes.scope(b.scope);
    if scope.kind != ScopeKind::Function {
        return None;
    }
    let func = crate::edit_helpers::node_with_range(&def.file.parse.syntax_node(), scope.range)
        .and_then(ast::FuncDef::cast)?;
    let params = func.params()?;
    let (mut found, mut first, mut slash_after) = (false, false, false);
    let mut plain_seen = 0;
    for p in params.params() {
        match &p {
            ast::AnyParam::Slash(_) if found => slash_after = true,
            ast::AnyParam::Plain(pp) => {
                if pp.name().map(|t| t.text_range()) == Some(b.range) {
                    found = true;
                    first = plain_seen == 0;
                }
                plain_seen += 1;
            }
            _ => {}
        }
    }
    if !found || slash_after || (first && func.is_method() && !func.has_decorator("staticmethod")) {
        return None;
    }
    let mut callee_names = vec![func.name_text()];
    let mut constructor = None;
    if matches!(func.name_text().as_str(), "__init__" | "__new__") {
        if let Some(class_scope) = scope
            .parent
            .map(|p| scopes.scope(p))
            .filter(|s| s.kind == ScopeKind::Class)
        {
            callee_names.extend(class_scope.name.as_ref().map(|n| n.to_string()));
            constructor = crate::edit_helpers::node_with_range(
                &def.file.parse.syntax_node(),
                class_scope.range,
            )
            .and_then(ast::ClassDef::cast)
            .map(|c| (c, func.clone()));
        }
    }
    let method_name = func.is_method().then(|| func.name_text());
    let owner = scope
        .parent
        .map(|p| scopes.scope(p))
        .filter(|s| s.kind == ScopeKind::Class)
        .map(|s| crate::types::ClassRef {
            file: def.file.id,
            range: s.range,
            name: s.name.clone().unwrap_or_default(),
        });
    Some(KeywordParam {
        callee_names,
        func_range: func.syntax().text_range(),
        method_name,
        constructor,
        owner,
    })
}

/// Whether the keyword argument `kw` belongs to a call `obj.<method>(...)`
/// whose receiver's type is unknown.
/// How a call `obj.<method>(...)` that does not resolve relates to `method`
/// of the class `owner`: a receiver of unknown type matches by name only
/// (dynamic); one whose class is related to `owner` through the MRO matches
/// for certain (a mixin method calling `self.greet(name=...)` that a
/// subclass defines), as method usages do.
fn untyped_receiver_call(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    kw: &SyntaxNode,
    method: &str,
    owner: Option<&crate::types::ClassRef>,
) -> MemberMatch {
    let Some(call) = kw
        .parent()
        .filter(|p| p.kind() == SyntaxKind::ARG_LIST)
        .and_then(|l| l.parent())
        .and_then(ast::CallExpr::cast)
    else {
        return MemberMatch::No;
    };
    let Some(ast::Expr::Attribute(a)) = call.callee() else {
        return MemberMatch::No;
    };
    if a.attr_text() != method {
        return MemberMatch::No;
    }
    let Some(receiver) = a.object().map(|o| inf.type_of_expr(file, &o)) else {
        return MemberMatch::No;
    };
    match owner {
        Some(o) => member_match(
            inf,
            &receiver,
            &Type::Instance(o.clone(), Vec::new()),
            Some(o),
            method,
        ),
        None if matches!(receiver, Type::Unknown | Type::Any) => MemberMatch::Dynamic,
        None => MemberMatch::No,
    }
}

/// Ranges of the parameter `name` in the docstring of the function at
/// `func_range`: Sphinx `:param name:` / `:param type name:` / `:type
/// name:`, Google `name (type):` / `name:` and NumPy `name : type` entries.
pub fn docstring_param_ranges(
    file: &FileInfo,
    func_range: TextRange,
    name: &str,
) -> Vec<TextRange> {
    let Some(func) = crate::edit_helpers::node_with_range(&file.parse.syntax_node(), func_range)
        .and_then(ast::FuncDef::cast)
    else {
        return Vec::new();
    };
    let Some(doc) = func.docstring() else {
        return Vec::new();
    };
    let r = doc.syntax().text_range();
    let start = usize::from(r.start());
    let Some(text) = file.text.get(start..usize::from(r.end())) else {
        return Vec::new();
    };
    crate::docstrings::parameter_name_ranges(text, name)
        .into_iter()
        .map(|range| range + TextSize::new(start as u32))
        .collect()
}

/// The import alias node of an import binding (`helper as h` in `from m
/// import helper as h`).
fn import_alias_of(file: &FileInfo, b: &crate::scope::Binding) -> Option<ast::ImportAlias> {
    let node = crate::edit_helpers::node_with_range(&file.parse.syntax_node(), b.node_range)?;
    node.ancestors().find_map(ast::ImportAlias::cast)
}

/// Whether two definitions identify the same binding in one file.
pub fn same_binding(a: &Definition, b: &Definition) -> bool {
    a.binding == b.binding && a.file.path == b.file.path
}

/// One step through a `from m import name` binding: the binding of `name`
/// in `m` (which may be another import).
fn import_hop(inf: &Inferer<'_>, d: &Definition) -> Option<Definition> {
    let BindingKind::Import(info) = &d.binding().kind else {
        return None;
    };
    let name = info.name.as_ref()?;
    let module =
        inf.db
            .module_relative(&d.file, info.level, info.module.trim_start_matches('.'))?;
    inf.resolve_in_module(&module, name, 0)
}

/// Whether an import binding gives its target another local name (`from m
/// import helper as h`, `import json as j`); `helper as helper` does not.
pub fn is_renaming_alias(b: &crate::scope::Binding) -> bool {
    match &b.kind {
        BindingKind::Import(info) => {
            info.alias.is_some()
                && b.name.as_str() != info.name.as_deref().unwrap_or(info.module.as_str())
        }
        _ => false,
    }
}

/// The binding a name comes from: `def` itself, or what it imports when the
/// import keeps the name (`from m import helper`, `from m import helper as
/// helper`). An alias that renames (`as h`), a module import and an import
/// that does not resolve are their own origin.
pub fn name_origin(inf: &Inferer<'_>, def: &Definition) -> Definition {
    let mut cur = def.clone();
    for _ in 0..16 {
        let b = cur.binding();
        let BindingKind::Import(info) = &b.kind else {
            break;
        };
        if info.name.as_deref() != Some(b.name.as_str()) {
            break;
        }
        match import_hop(inf, &cur) {
            Some(next) => cur = next,
            None => break,
        }
    }
    cur
}

/// Whether the binding `start` is `anchor` or imports it under `name`
/// through imports that keep the name. An import of another name (`from a
/// import h as helper`) starts a different local name, even when it leads
/// to the same definition.
fn reaches(inf: &Inferer<'_>, start: Definition, anchor: &Definition, name: &str) -> bool {
    let mut cur = start;
    for _ in 0..16 {
        if same_binding(&cur, anchor) {
            return true;
        }
        let BindingKind::Import(info) = &cur.binding().kind else {
            return false;
        };
        if info.name.as_deref() != Some(name) {
            return false;
        }
        match import_hop(inf, &cur) {
            Some(next) => cur = next,
            None => return false,
        }
    }
    false
}

/// Usages in `file` of the binding `anchor` (named `name`): references that
/// resolve to it directly or through imports that keep the name,
/// `module.name` of modules that define or re-export it, and the imports
/// of it (the imported name, plus the alias of `name as name`).
fn reaching_usages(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    anchor: &Definition,
    name: &str,
) -> Vec<Usage> {
    let mut out = Vec::new();
    for r in &file.scopes.references {
        if r.name.as_str() == name
            && inf
                .resolve_name(file, r.scope, name)
                .is_some_and(|d| reaches(inf, d, anchor, name))
        {
            out.push(usage_at(file, r.range, r.is_write, false));
        }
    }
    if anchor.binding().scope == anchor.file.scopes.module_scope() {
        for node in file
            .parse
            .syntax_node()
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::ATTRIBUTE_EXPR)
        {
            let Some(attr) = ast::AttributeExpr::cast(node.clone()) else {
                continue;
            };
            let Some(tok) = attr.attr().filter(|t| t.text() == name) else {
                continue;
            };
            let Some(obj) = attr.object() else { continue };
            let Type::Module(m) = inf.type_of_expr(file, &obj) else {
                continue;
            };
            let Some(module) = inf.db.file(m) else {
                continue;
            };
            if inf
                .resolve_in_module(&module, name, 0)
                .is_some_and(|d| reaches(inf, d, anchor, name))
            {
                let is_write = node.parent().is_some_and(|p| {
                    matches!(
                        p.kind(),
                        SyntaxKind::ASSIGN_STMT
                            | SyntaxKind::ANN_ASSIGN_STMT
                            | SyntaxKind::AUG_ASSIGN_STMT
                    ) && p.children().next().as_ref() == Some(&node)
                });
                out.push(usage_at(file, tok.text_range(), is_write, false));
            }
        }
    }
    for (i, b) in file.scopes.bindings.iter().enumerate() {
        let BindingKind::Import(info) = &b.kind else {
            continue;
        };
        if info.name.as_deref() != Some(name)
            || !reaches(
                inf,
                Definition {
                    file: file.clone(),
                    binding: crate::scope::BindingId(i as u32),
                },
                anchor,
                name,
            )
        {
            continue;
        }
        match import_alias_of(file, b) {
            Some(alias) => {
                if let Some(t) = alias.name_token() {
                    out.push(usage_at(file, t.text_range(), false, false));
                }
                if let Some(a) = alias.alias().filter(|a| a.text() == name) {
                    out.push(usage_at(file, a.text_range(), false, false));
                }
            }
            None => out.push(usage_at(file, b.range, false, false)),
        }
    }
    out
}

/// Semantic matching context for a module-level re-export. Project candidate
/// loading, ordering and deduplication belong to rustpy-search.
pub struct ReexportQuery<'db> {
    inf: Inferer<'db>,
    def: Definition,
    name: String,
}

impl<'db> ReexportQuery<'db> {
    pub fn prepare(db: &'db Analysis, def: &Definition) -> Option<Self> {
        let BindingKind::Import(info) = &def.binding().kind else {
            return None;
        };
        if def.binding().scope != def.file.scopes.module_scope() {
            return None;
        }
        // `from . import core` / `import pkg.core` in a package: the name is a
        // submodule, and `from pkg import core` elsewhere loads it by itself, so
        // those importers do not depend on this line.
        if let Some(name) = &info.name {
            let module = info.module.trim_start_matches('.');
            let dotted = if module.is_empty() {
                name.to_string()
            } else {
                format!("{module}.{name}")
            };
            if db.module_relative(&def.file, info.level, &dotted).is_some() {
                return None;
            }
        } else {
            return None;
        }
        Some(Self {
            inf: Inferer::new(db),
            def: def.clone(),
            name: def.binding().name.to_string(),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn in_file(&self, file: &Arc<FileInfo>) -> Vec<Usage> {
        reaching_usages(&self.inf, file, &self.def, &self.name)
    }
}

/// Usages of the symbol at `offset` within the same file (Highlight Usages).
pub fn usages_in_file(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Vec<Usage> {
    let offset = crate::clamp_offset(file, offset);
    let Some(sym) = symbol_at(db, file, offset) else {
        return Vec::new();
    };
    usages_of_in_file(db, &sym, file)
}

fn usages_of_in_file(db: &Analysis, sym: &Symbol, file: &Arc<FileInfo>) -> Vec<Usage> {
    let inf = Inferer::new(db);
    let mut out = Vec::new();
    match sym {
        Symbol::Name { def, name } => {
            // The binding the name comes from: the definition an import of
            // it leads to, or the alias that introduces it (`as h`).
            let origin = name_origin(&inf, def);
            let mut defs = vec![def.clone()];
            if !same_binding(&origin, def) {
                defs.push(origin.clone());
            }
            for d in defs.iter().filter(|d| d.file.path == file.path) {
                let b = d.binding();
                for id in file.scopes.bindings_in(b.scope, &b.name) {
                    let bb = file.scopes.binding(*id);
                    if bb.is_definition() && !out.iter().any(|u: &Usage| u.range == bb.range) {
                        out.push(usage_at(file, bb.range, true, true));
                    }
                }
            }
            out.extend(reaching_usages(&inf, file, &origin, name));
            out.extend(crate::pytest_fixtures::usages_for(db, file, &origin));
            // A parameter: keyword arguments naming it at call sites, and its docstring entries.
            if let Some(kp) = keyword_param(def) {
                for node in file
                    .parse
                    .syntax_node()
                    .descendants()
                    .filter(|n| n.kind() == SyntaxKind::KEYWORD_ARG)
                {
                    let Some(tok) =
                        ast::token(&node, SyntaxKind::IDENT).filter(|t| t.text() == name)
                    else {
                        continue;
                    };
                    let hit =
                        match keyword_target(db, &inf, file, &tok) {
                            KeywordTarget::Param(d) => same_binding(&d, def).then_some(false),
                            // A method called on a receiver of unknown type
                            // (`obj.method(key=1)`) matches by the method name, like
                            // method usages, as a dynamic usage. A name that does not
                            // resolve (`greet(name=1)` of a missing package) is another function.
                            KeywordTarget::Unresolved => match kp.method_name.as_deref().map(|m| {
                                untyped_receiver_call(&inf, file, &node, m, kp.owner.as_ref())
                            }) {
                                Some(MemberMatch::Static) => Some(false),
                                Some(MemberMatch::Dynamic) => Some(true),
                                _ => None,
                            },
                            KeywordTarget::NoParam => None,
                        };
                    if let Some(dynamic) = hit {
                        out.push(Usage {
                            dynamic,
                            ..usage_at(file, tok.text_range(), false, false)
                        });
                    }
                }
                if def.file.path == file.path {
                    for r in docstring_param_ranges(file, kp.func_range, name) {
                        out.push(usage_at(file, r, false, false));
                    }
                }
            }
        }
        Symbol::Member { owner, name, decl } => {
            // Declaration in this file.
            if let Some((f, r)) = decl {
                if f.path == file.path {
                    out.push(usage_at(file, *r, true, true));
                }
            }
            let owner_class = owner.class_ref().cloned();
            for node in file
                .parse
                .syntax_node()
                .descendants()
                .filter(|n| n.kind() == SyntaxKind::ATTRIBUTE_EXPR)
            {
                let Some(attr) = ast::AttributeExpr::cast(node.clone()) else {
                    continue;
                };
                let Some(tok) = attr.attr() else { continue };
                if tok.text() != name {
                    continue;
                }
                let Some(obj) = attr.object() else { continue };
                let base = inf.type_of_expr(file, &obj);
                let matched = member_match(&inf, &base, owner, owner_class.as_ref(), name);
                if matched != MemberMatch::No {
                    let is_write = node.parent().is_some_and(|p| {
                        matches!(
                            p.kind(),
                            SyntaxKind::ASSIGN_STMT
                                | SyntaxKind::ANN_ASSIGN_STMT
                                | SyntaxKind::AUG_ASSIGN_STMT
                        ) && p.children().next().as_ref() == Some(&node)
                    });
                    out.push(Usage {
                        dynamic: matched == MemberMatch::Dynamic,
                        ..usage_at(file, tok.text_range(), is_write, false)
                    });
                }
            }
            if let Some(cls) = &owner_class {
                // The name bound in the body of a related class (`size = 1`,
                // the dataclass field `x: float`, a nested class, a property
                // and its setter) and its bare-name uses in that body
                // (`@size.setter`, `double = size * 2`).
                for (si, s) in file.scopes.scopes.iter().enumerate() {
                    let ids = file
                        .scopes
                        .bindings_in(crate::scope::ScopeId(si as u32), name);
                    if s.kind != ScopeKind::Class || ids.is_empty() {
                        continue;
                    }
                    let this = crate::types::ClassRef {
                        file: file.id,
                        range: s.range,
                        name: s.name.clone().unwrap_or_default(),
                    };
                    if !related_classes(&inf, &this, cls) {
                        continue;
                    }
                    for id in ids {
                        let b = file.scopes.binding(*id);
                        if b.is_definition() && !matches!(b.kind, BindingKind::Import(_)) {
                            out.push(usage_at(file, b.range, true, true));
                        }
                    }
                    for r in file
                        .scopes
                        .references
                        .iter()
                        .filter(|r| r.resolved.is_some_and(|id| ids.contains(&id)))
                    {
                        out.push(usage_at(file, r.range, r.is_write, false));
                    }
                }
                // A dataclass field: the keyword arguments setting it through the
                // generated `__init__` (`Point(x=1)`, also of subclasses).
                for node in file
                    .parse
                    .syntax_node()
                    .descendants()
                    .filter(|n| n.kind() == SyntaxKind::KEYWORD_ARG)
                {
                    let Some(tok) =
                        ast::token(&node, SyntaxKind::IDENT).filter(|t| t.text() == name)
                    else {
                        continue;
                    };
                    if let Some((called, _)) = generated_init_field(db, &inf, file, &node, name) {
                        if related_classes(&inf, &called, cls) {
                            out.push(usage_at(file, tok.text_range(), false, false));
                        }
                    }
                }
            }
            // Methods declared in subclasses/same class body with this name (overrides) are definitions.
            if let Some(cls) = owner_class {
                for node in file
                    .parse
                    .syntax_node()
                    .descendants()
                    .filter(|n| n.kind() == SyntaxKind::FUNC_DEF)
                {
                    let Some(f) = ast::FuncDef::cast(node.clone()) else {
                        continue;
                    };
                    if f.name_text() != *name || !f.is_method() {
                        continue;
                    }
                    let Some(class_node) = node.parent().and_then(|b| b.parent()) else {
                        continue;
                    };
                    let this = crate::types::ClassRef {
                        file: file.id,
                        range: class_node.text_range(),
                        name: Default::default(),
                    };
                    let related = this.range == cls.range && file.id == cls.file
                        || inf
                            .mro(&crate::types::ClassRef {
                                file: file.id,
                                range: class_node.text_range(),
                                name: ast::ClassDef::cast(class_node.clone())
                                    .map(|c| c.name_text().into())
                                    .unwrap_or_default(),
                            })
                            .iter()
                            .any(|c| c.file == cls.file && c.range == cls.range);
                    if related {
                        if let Some(t) = f.name() {
                            if !out.iter().any(|u| u.range == t.text_range()) {
                                out.push(usage_at(file, t.text_range(), true, true));
                            }
                        }
                    }
                }
            }
        }
        Symbol::Keyword { name } => {
            for node in file
                .parse
                .syntax_node()
                .descendants()
                .filter(|n| n.kind() == SyntaxKind::KEYWORD_ARG)
            {
                if let Some(t) = ast::token(&node, SyntaxKind::IDENT) {
                    if t.text() == name {
                        out.push(usage_at(file, t.text_range(), false, false));
                    }
                }
            }
        }
    }
    out.sort_by_key(|u| u.range.start());
    out.dedup_by_key(|u| u.range);
    out
}

/// Resolved semantic context for a usage search. It does not load or scan
/// project candidates; the search owner supplies one file at a time.
pub struct UsageQuery {
    symbol: Symbol,
    seed_files: Vec<Arc<FileInfo>>,
    searches_project: bool,
    callee_names: Vec<String>,
    importer_anchor: Option<rustpy_core::FileId>,
    constructor: Option<(Arc<FileInfo>, TextRange, TextRange)>,
    fixture_names: Vec<String>,
}

impl UsageQuery {
    pub fn prepare(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> Option<Self> {
        let sym = symbol_at(db, file, crate::clamp_offset(file, offset))?;
        let origin = match &sym {
            Symbol::Name { def, .. } => Some(name_origin(&Inferer::new(db), def)),
            _ => None,
        };
        // Definition, then non-library origin: the search owner deduplicates
        // these against the caller file without changing their order.
        let seed_files: Vec<Arc<FileInfo>> = match &sym {
            Symbol::Name { def, .. } => [
                Some(def.file.clone()),
                origin
                    .as_ref()
                    .map(|o| o.file.clone())
                    .filter(|f| !is_library_file(db, f)),
            ]
            .into_iter()
            .flatten()
            .collect(),
            Symbol::Member { decl, .. } => decl.iter().map(|(f, _)| f.clone()).collect(),
            Symbol::Keyword { .. } => Vec::new(),
        };
        let mut searches_project = true;
        let mut callee_names = Vec::new();
        let mut constructor = None;
        if let (Symbol::Name { def, .. }, Some(origin)) = (&sym, &origin) {
            let scope = origin.file.scopes.scope(origin.binding().scope);
            if !matches!(scope.kind, ScopeKind::Module | ScopeKind::Class) {
                match keyword_param(def) {
                    Some(kp) => {
                        callee_names = kp.callee_names;
                        constructor = kp.constructor.map(|(class, func)| {
                            (
                                def.file.clone(),
                                class.syntax().text_range(),
                                func.syntax().text_range(),
                            )
                        });
                    }
                    None => searches_project = false,
                }
            }
        }
        let fixture_names = origin
            .as_ref()
            .filter(|definition| {
                definition.binding().kind == BindingKind::Function
                    && definition.file.text.contains("fixture")
            })
            .map(|definition| {
                crate::pytest_fixtures::available(
                    db,
                    &definition.file,
                    Some(definition.binding().range.start()),
                )
                .into_iter()
                .filter(|fixture| {
                    fixture.definition.file.path == definition.file.path
                        && fixture.definition.binding == definition.binding
                })
                .map(|fixture| fixture.name)
                .collect()
            })
            .unwrap_or_default();
        Some(Self {
            fixture_names,
            symbol: sym,
            seed_files,
            searches_project,
            callee_names,
            importer_anchor: origin.as_ref().map(|definition| definition.file.id),
            constructor,
        })
    }

    pub fn name(&self) -> &str {
        self.symbol.name()
    }
    pub fn candidate_text_may_match(&self, text: &str) -> bool {
        text.contains(self.symbol.name())
            || self.fixture_names.iter().any(|name| text.contains(name))
    }
    pub fn seed_files(&self) -> &[Arc<FileInfo>] {
        &self.seed_files
    }
    pub fn searches_project(&self) -> bool {
        self.searches_project
    }
    /// Scheduling hints only: never remove candidates, serve cached text, or
    /// change the caller's definition-first/project-input result order.
    pub fn importer_paths(
        &self,
        db: &Analysis,
        cancelled: &dyn Fn() -> bool,
    ) -> Option<Vec<PathBuf>> {
        let Some(anchor) = self.importer_anchor else {
            return Some(Vec::new());
        };
        Some(
            db.importers_of_cancellable(anchor, cancelled)?
                .into_iter()
                .map(|file| file.path.clone())
                .collect(),
        )
    }

    pub fn callee_names(&self) -> &[String] {
        &self.callee_names
    }
    pub fn constructor(&self) -> Option<(Arc<FileInfo>, ast::ClassDef, ast::FuncDef)> {
        let (file, class, func) = self.constructor.as_ref()?;
        let root = file.parse.syntax_node();
        Some((
            file.clone(),
            crate::edit_helpers::node_with_range(&root, *class).and_then(ast::ClassDef::cast)?,
            crate::edit_helpers::node_with_range(&root, *func).and_then(ast::FuncDef::cast)?,
        ))
    }
    pub fn in_file(&self, db: &Analysis, file: &Arc<FileInfo>) -> Vec<Usage> {
        usages_of_in_file(db, &self.symbol, file)
    }
}

/// Whether the usages of `sym` can be in other files: module and class
/// members, and parameters (keyword arguments name them at call sites
/// anywhere); local variables never escape their function. The same test
/// as the project usage search makes.
fn escapes_file(db: &Analysis, sym: &Symbol) -> bool {
    match sym {
        Symbol::Name { def, .. } => {
            let origin = name_origin(&Inferer::new(db), def);
            let s = origin.file.scopes.scope(origin.binding().scope);
            matches!(s.kind, ScopeKind::Module | ScopeKind::Class) || keyword_param(def).is_some()
        }
        Symbol::Member { .. } | Symbol::Keyword { .. } => true,
    }
}

/// Whether Find Usages of the symbol at `offset` searches the project's other
/// files. When it does not (a local variable), callers can skip listing them
/// and run the search right away.
pub fn searches_project(db: &Analysis, file: &Arc<FileInfo>, offset: TextSize) -> bool {
    symbol_at(db, file, crate::clamp_offset(file, offset)).is_some_and(|sym| escapes_file(db, &sym))
}

/// Whether `f` belongs to a library rather than to the project: bundled
/// stubs, the interpreter's search paths outside the project, and installed
/// packages or virtual environments inside it (the usual `.venv`). Project
/// code on the interpreter path (an editable install's `src`) stays project code.
///
/// File paths are canonical while configured roots and interpreter paths may
/// be spelled through a symlink (`proj/.venv -> ../venv`), so both spellings
/// of each root count; a file outside every root and search path that lies in
/// an installed-packages directory or virtualenv is a library file as well.
pub fn is_library_file(db: &Analysis, f: &FileInfo) -> bool {
    if f.path.starts_with("<typeshed>") {
        return true;
    }
    let resolver = &db.resolver;
    match resolver
        .roots
        .iter()
        .filter_map(|r| resolver.root_containing(&f.path, r))
        .max_by_key(|r| r.components().count())
    {
        Some(root) => in_environment(&f.path, &root),
        None => {
            resolver
                .search_paths
                .iter()
                .any(|p| resolver.root_containing(&f.path, p).is_some())
                || (f.path.is_absolute() && in_environment(&f.path, Path::new("/")))
        }
    }
}

/// Whether `path` lies in an installed-packages directory or a virtual
/// environment below the project root `root` (the root itself never counts).
fn in_environment(path: &Path, root: &Path) -> bool {
    path.ancestors()
        .skip(1)
        .take_while(|d| *d != root && d.starts_with(root))
        .any(|d| {
            matches!(
                d.file_name().and_then(|n| n.to_str()),
                Some("site-packages" | "dist-packages")
            ) || d.join("pyvenv.cfg").is_file()
                || d.join("conda-meta").is_dir()
        })
}

/// Whole-word occurrences of `name` in the comments and string literals
/// of `file` (inside `within` when given).
pub fn text_occurrences(file: &FileInfo, name: &str, within: Option<TextRange>) -> Vec<Usage> {
    let mut out = Vec::new();
    if name.is_empty() || !file.text.contains(name) {
        return out;
    }
    for el in file.parse.syntax_node().descendants_with_tokens() {
        let Some(tok) = el.as_token() else { continue };
        if !matches!(
            tok.kind(),
            SyntaxKind::COMMENT
                | SyntaxKind::STRING
                | SyntaxKind::FSTRING_MIDDLE
                | SyntaxKind::TSTRING_MIDDLE
        ) {
            continue;
        }
        let range = tok.text_range();
        if within.is_some_and(|w| !w.contains_range(range)) {
            continue;
        }
        let text = tok.text();
        for (i, _) in text.match_indices(name) {
            let before = text[..i].chars().next_back();
            let after = text[i + name.len()..].chars().next();
            if before.is_some_and(rustpy_core::is_ident_continue)
                || after.is_some_and(rustpy_core::is_ident_continue)
            {
                continue;
            }
            let start = range.start() + TextSize::new(i as u32);
            let r = TextRange::new(start, start + TextSize::new(name.len() as u32));
            out.push(Usage {
                non_code: true,
                ..usage_at(file, r, false, false)
            });
        }
    }
    out
}
