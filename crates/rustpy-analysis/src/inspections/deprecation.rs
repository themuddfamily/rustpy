//! `PyDeprecation` (roadmap §2.5): uses of functions, methods, classes and
//! overloads marked `@deprecated` — PEP 702 (`warnings.deprecated`,
//! `typing_extensions.deprecated`), which is also how typeshed marks
//! deprecated stdlib APIs such as `datetime.utcnow()` or the keyword form of
//! `typing.NamedTuple`. A call is checked against the overload it selects;
//! an uncalled or imported overloaded function is deprecated only when
//! every overload is. Stdlib modules resolve to the typeshed stubs, whose
//! version and platform branches are evaluated for the target interpreter,
//! so a deprecation applies only where its branch is live.
//!
//! Stdlib deprecations typeshed cannot mark are table-driven (see
//! `rustpy_semantic::deprecation`): whole modules such as `imp`, `asyncore`
//! or PEP 594's, and the ABC aliases `collections` lost in 3.10. They follow
//! the target version: deprecated modules are weak warnings until their
//! removal and errors from then on.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use once_cell::sync::Lazy;
use rustpy_core::{FileId, Severity, TextRange};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::{SyntaxKind, SyntaxNode};
use smol_str::SmolStr;

use super::{QuickFix, Sink};
use rustpy_semantic::db::{Analysis, FileInfo};
use rustpy_semantic::deprecation::{
    deprecated_module, deprecated_names, deprecation_message, is_collections_abc_alias,
    DeprecatedModule, COLLECTIONS_ABC_REMOVED,
};
use rustpy_semantic::infer::{Definition, Inferer};
use rustpy_semantic::scope::BindingKind;
use rustpy_semantic::types::{Signature, Type};
use rustpy_semantic::typeshed::PyVersion;

const CODE: &str = "PyDeprecation";

pub(super) fn run(db: &Analysis, file: &Arc<FileInfo>, version: PyVersion, sink: &mut Sink) {
    // Only names some `@deprecated` definition carries are resolved (type
    // inference for every name and attribute would double inspection time).
    let candidates = Candidates::new(db, file);
    let mut check = Checker {
        db,
        inf: Inferer::new(db),
        file,
        cache: HashMap::new(),
    };
    let mut stdlib = Stdlib::new(db, file, version);
    let root = file.parse.syntax_node();
    for node in root.descendants() {
        match node.kind() {
            SyntaxKind::IMPORT_STMT => stdlib.import(&node, sink),
            SyntaxKind::ATTRIBUTE_EXPR if stdlib.attribute(&node, sink) => {}
            SyntaxKind::NAME_EXPR | SyntaxKind::ATTRIBUTE_EXPR => {
                let Some(expr) = ast::Expr::cast(node.clone()) else {
                    continue;
                };
                let Some((range, name)) = name_of(&expr) else {
                    continue;
                };
                if !candidates.contains(&name) || is_store(&node) {
                    continue;
                }
                if let Some(message) = check.expr(&expr) {
                    sink.push(
                        range,
                        Severity::WeakWarning,
                        CODE,
                        describe(&name, &message),
                        Vec::new(),
                    );
                }
            }
            SyntaxKind::IMPORT_FROM_STMT => {
                let Some(imp) = ast::ImportFromStmt::cast(node.clone()) else {
                    continue;
                };
                stdlib.import_from(&imp, sink);
                let Some(module) = db.module_relative(
                    file,
                    imp.level(),
                    &imp.module().map(|m| m.path()).unwrap_or_default(),
                ) else {
                    continue;
                };
                for alias in imp.aliases() {
                    let Some(tok) = alias.name_token() else {
                        continue;
                    };
                    if !candidates.contains(tok.text()) {
                        continue;
                    }
                    let Some(def) = check.inf.resolve_in_module(&module, tok.text(), 0) else {
                        continue;
                    };
                    let def = check.inf.follow_imports(def);
                    if let Some(message) = check.imported(&def) {
                        sink.push(
                            tok.text_range(),
                            Severity::WeakWarning,
                            CODE,
                            describe(tok.text(), &message),
                            Vec::new(),
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

/// An import of a stdlib module removed for the target version is reported
/// once, by the `PyDeprecation` error, not also as `No module named …`.
pub(super) fn drop_removed_module_duplicates(out: &mut Vec<super::Problem>) {
    let removed: HashSet<TextRange> = out
        .iter()
        .filter(|p| p.diagnostic.code == CODE && p.diagnostic.severity == Severity::Error)
        .map(|p| p.diagnostic.range)
        .collect();
    if removed.is_empty() {
        return;
    }
    out.retain(|p| {
        !(p.diagnostic.code == "PyUnresolvedReferences"
            && p.diagnostic.message.starts_with("No module named '")
            && removed.contains(&p.diagnostic.range))
    });
}

/// Table-driven stdlib deprecations for one file and target version.
struct Stdlib<'a> {
    db: &'a Analysis,
    file: &'a Arc<FileInfo>,
    version: PyVersion,
    /// Names bound by `import collections` / `import <deprecated module>`
    /// (only attribute uses of these are resolved).
    bound: HashSet<String>,
    /// Whether a top-level module name resolves to the stdlib (and not to a
    /// project module or an installed backport of the same name).
    is_stdlib: HashMap<String, bool>,
    /// Some statement imports `collections.abc`.
    imports_abc: bool,
}

impl<'a> Stdlib<'a> {
    fn new(db: &'a Analysis, file: &'a Arc<FileInfo>, version: PyVersion) -> Self {
        let mut bound = HashSet::new();
        let mut imports_abc = false;
        for imp in file
            .parse
            .syntax_node()
            .descendants()
            .filter_map(ast::ImportStmt::cast)
        {
            for alias in imp.aliases() {
                let name = alias.name();
                imports_abc |= name == "collections.abc";
                let top = name.split('.').next().unwrap_or_default();
                if top == "collections" || deprecated_module(top, version).is_some() {
                    bound.extend(alias.bound_name());
                }
            }
        }
        Stdlib {
            db,
            file,
            version,
            bound,
            is_stdlib: HashMap::new(),
            imports_abc,
        }
    }

    /// The module's deprecation, unless something else provides the name.
    fn module(&mut self, name: &str) -> Option<&'static DeprecatedModule> {
        let deprecated = deprecated_module(name, self.version)?;
        let db = self.db;
        let stdlib = *self
            .is_stdlib
            .entry(deprecated.name.to_string())
            .or_insert_with(|| {
                db.module(deprecated.name)
                    .is_none_or(|m| m.path.starts_with("<typeshed>"))
            });
        stdlib.then_some(deprecated)
    }

    fn report_module(&self, range: TextRange, name: &str, m: &DeprecatedModule, sink: &mut Sink) {
        let (severity, message) = match m.removed {
            Some(removed) if self.version >= removed => (
                Severity::Error,
                format!(
                    "'{name}' was removed in Python {}.{}: {}",
                    removed.0, removed.1, m.advice
                ),
            ),
            _ => (
                Severity::WeakWarning,
                format!(
                    "'{name}' is deprecated since Python {}.{}: {}",
                    m.since.0, m.since.1, m.advice
                ),
            ),
        };
        sink.push(range, severity, CODE, message, Vec::new());
    }

    /// `collections.Mapping`: the severity and message.
    fn abc_alias(&self, name: &str) -> (Severity, String) {
        let (major, minor) = COLLECTIONS_ABC_REMOVED;
        if self.version >= COLLECTIONS_ABC_REMOVED {
            (
                Severity::Error,
                format!("'{name}' was removed from 'collections' in Python {major}.{minor}: import it from 'collections.abc'"),
            )
        } else {
            (
                Severity::WeakWarning,
                format!("'collections.{name}' is deprecated (removed in Python {major}.{minor}): import it from 'collections.abc'"),
            )
        }
    }

    /// `import imp`, `import distutils.core`.
    fn import(&mut self, node: &SyntaxNode, sink: &mut Sink) {
        let Some(imp) = ast::ImportStmt::cast(node.clone()) else {
            return;
        };
        for alias in imp.aliases() {
            let name = alias.name();
            let Some(m) = self.module(&name) else {
                continue;
            };
            let range = alias
                .syntax()
                .children()
                .find_map(ast::DottedName::cast)
                .map(|d| d.syntax().text_range())
                .or_else(|| alias.name_token().map(|t| t.text_range()));
            if let Some(range) = range {
                self.report_module(range, &name, m, sink);
            }
        }
    }

    /// `from imp import reload`; `from collections import Mapping`.
    fn import_from(&mut self, imp: &ast::ImportFromStmt, sink: &mut Sink) {
        if imp.level() != 0 {
            return;
        }
        let Some(module) = imp.module() else { return };
        let path = module.path();
        if let Some(m) = self.module(&path) {
            self.report_module(module.syntax().text_range(), &path, m, sink);
            return;
        }
        if path != "collections" {
            return;
        }
        let aliases: Vec<ast::ImportAlias> = imp.aliases().collect();
        let (abcs, rest): (Vec<_>, Vec<_>) = aliases
            .iter()
            .partition(|a| is_collections_abc_alias(&a.name()));
        if abcs.is_empty() {
            return;
        }
        let fix = QuickFix {
            title: "Import from 'collections.abc'".to_string(),
            edits: if rest.is_empty() {
                vec![(module.syntax().text_range(), "collections.abc".to_string())]
            } else {
                self.split_abc_import(imp, &aliases, &abcs)
            },
        };
        for alias in abcs {
            let Some(tok) = alias.name_token() else {
                continue;
            };
            let (severity, message) = self.abc_alias(tok.text());
            sink.push(tok.text_range(), severity, CODE, message, vec![fix.clone()]);
        }
    }

    /// Edits moving the ABC aliases of a mixed `from collections import …`
    /// to a new `from collections.abc import …` after it. The aliases are cut
    /// out of the original statement (each with its separating comma), so a
    /// parenthesised multi-line import keeps its layout.
    fn split_abc_import(
        &self,
        imp: &ast::ImportFromStmt,
        aliases: &[ast::ImportAlias],
        abcs: &[&ast::ImportAlias],
    ) -> Vec<(TextRange, String)> {
        let is_abc = |a: &ast::ImportAlias| abcs.iter().any(|b| b.syntax() == a.syntax());
        let mut cuts: Vec<TextRange> = Vec::new();
        let mut last_kept: Option<usize> = None;
        for (i, alias) in aliases.iter().enumerate() {
            let range = alias.syntax().text_range();
            if !is_abc(alias) {
                last_kept = Some(i);
                continue;
            }
            cuts.push(match aliases.get(i + 1) {
                // Up to the next alias: the alias, its comma and the layout after it.
                Some(next) => TextRange::new(range.start(), next.syntax().text_range().start()),
                // The last alias: from the end of the last kept one (its comma).
                None => {
                    let Some(k) = last_kept else { continue };
                    TextRange::new(aliases[k].syntax().text_range().end(), range.end())
                }
            });
        }
        // Merge overlapping cuts (the trailing cut may cover earlier ones).
        cuts.sort_by_key(|r| r.start());
        let mut merged: Vec<TextRange> = Vec::new();
        for cut in cuts {
            match merged.last_mut() {
                Some(prev) if cut.start() <= prev.end() => *prev = prev.cover(cut),
                _ => merged.push(cut),
            }
        }
        let names = abcs
            .iter()
            .map(|a| {
                a.syntax()
                    .text()
                    .to_string()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join(", ");
        let range = imp.syntax().text_range();
        let text: &str = &self.file.text;
        let start = usize::from(range.start());
        let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
        let prefix = &text[line_start..start];
        let sep = if prefix.chars().all(|c| c == ' ' || c == '\t') {
            format!("\n{prefix}")
        } else {
            "; ".to_string()
        };
        let mut edits: Vec<(TextRange, String)> =
            merged.into_iter().map(|r| (r, String::new())).collect();
        edits.push((
            TextRange::empty(range.end()),
            format!("{sep}from collections.abc import {names}"),
        ));
        edits
    }

    /// `collections.Mapping` (reported on `Mapping`; returns true) and
    /// `imp.reload` (reported on `imp`).
    fn attribute(&mut self, node: &SyntaxNode, sink: &mut Sink) -> bool {
        if self.bound.is_empty() || is_store(node) {
            return false;
        }
        let Some(attr) = ast::AttributeExpr::cast(node.clone()) else {
            return false;
        };
        let Some(ast::Expr::Name(receiver)) = attr.object() else {
            return false;
        };
        let name = receiver.name_text();
        if !self.bound.contains(&name) {
            return false;
        }
        // Every binding of the receiver must be an import of the same module.
        let scopes = &self.file.scopes;
        let scope = scopes.scope_at(receiver.syntax().text_range().start());
        let Some(ids) = scopes.resolve(scope, &name) else {
            return false;
        };
        let modules: Vec<String> = ids
            .iter()
            .filter_map(|id| match &scopes.binding(*id).kind {
                BindingKind::Import(info) if info.name.is_none() && info.level == 0 => {
                    // `import a.b` binds `a`; `import a.b as c` binds `a.b`.
                    Some(if info.alias.is_some() {
                        info.module.to_string()
                    } else {
                        info.module
                            .split('.')
                            .next()
                            .unwrap_or_default()
                            .to_string()
                    })
                }
                _ => None,
            })
            .collect();
        if modules.len() != ids.len() || modules.windows(2).any(|w| w[0] != w[1]) {
            return false;
        }
        let Some(module) = modules.into_iter().next() else {
            return false;
        };
        if module == "collections" {
            let Some(tok) = attr.attr().filter(|t| is_collections_abc_alias(t.text())) else {
                return false;
            };
            let mut edits = vec![(
                TextRange::empty(receiver.syntax().text_range().end()),
                ".abc".to_string(),
            )];
            if !self.imports_abc {
                edits.push((
                    TextRange::empty(rustpy_semantic::import_placement::import_insert_offset(
                        self.file,
                    )),
                    "import collections.abc\n".to_string(),
                ));
            }
            let (severity, message) = self.abc_alias(tok.text());
            let fix = QuickFix {
                title: format!("Replace with '{name}.abc.{}'", tok.text()),
                edits,
            };
            sink.push(tok.text_range(), severity, CODE, message, vec![fix]);
            return true;
        }
        if let Some(m) = self.module(&module) {
            self.report_module(receiver.syntax().text_range(), &module, m, sink);
        }
        false
    }
}

/// Names of definitions marked `@deprecated` in the bundled stubs (a
/// deprecated `__init__`/`__new__` overload marks its class's name).
static STUB_NAMES: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    let mut out = HashSet::new();
    for (_, text) in rustpy_semantic::typeshed::TYPESHED_STDLIB {
        if text.contains("deprecated") {
            scan_stub(text, &mut out);
        }
    }
    out
});

/// Line-based scan of a stub for `@deprecated` definitions. It may name a
/// few extra classes (a superset is all the prefilter needs).
fn scan_stub<'t>(text: &'t str, out: &mut HashSet<&'t str>) {
    let mut classes: Vec<(usize, &'t str)> = Vec::new();
    let mut pending = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if let Some(decorator) = trimmed.strip_prefix('@') {
            let name = decorator
                .split(|c: char| c == '(' || c.is_whitespace())
                .next()
                .unwrap_or("");
            pending |= name == "deprecated" || name.ends_with(".deprecated");
            continue;
        }
        let rest = trimmed.strip_prefix("async ").unwrap_or(trimmed);
        let (is_class, rest) = match (rest.strip_prefix("class "), rest.strip_prefix("def ")) {
            (Some(r), _) => (true, r),
            (_, Some(r)) => (false, r),
            _ => continue,
        };
        let name = rest
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .next()
            .unwrap_or("");
        while classes.last().is_some_and(|(i, _)| *i >= indent) {
            classes.pop();
        }
        if pending && !name.is_empty() {
            out.insert(name);
            if matches!(name, "__init__" | "__new__") {
                if let Some((_, class)) = classes.last() {
                    out.insert(class);
                }
            }
        }
        pending = false;
        if is_class {
            classes.push((indent, name));
        }
    }
}

/// Names worth resolving: the stubs' deprecated names plus those of
/// `@deprecated` definitions in every other file the analysis holds (the
/// inspected file's imports are loaded first). Each file version's names
/// are collected once and kept with it.
struct Candidates {
    local: HashSet<SmolStr>,
}

impl Candidates {
    fn new(db: &Analysis, file: &Arc<FileInfo>) -> Self {
        for node in file.parse.syntax_node().descendants() {
            if let Some(imp) = ast::ImportStmt::cast(node.clone()) {
                for alias in imp.aliases() {
                    let _ = db.module(&alias.name());
                }
            } else if let Some(imp) = ast::ImportFromStmt::cast(node) {
                let module = imp.module().map(|m| m.path()).unwrap_or_default();
                if let Some(m) = db.module_relative(file, imp.level(), &module) {
                    // `from pkg import submodule`
                    if let Some(name) = &m.module_name {
                        for alias in imp.aliases() {
                            let _ = db.module(&format!("{name}.{}", alias.name()));
                        }
                    }
                }
            }
        }
        let mut local = HashSet::new();
        for f in db.all_files() {
            if !f.path.starts_with("<typeshed>") {
                local.extend(deprecated_names(&f).iter().cloned());
            }
        }
        Candidates { local }
    }

    fn contains(&self, name: &str) -> bool {
        STUB_NAMES.contains(name) || self.local.contains(name)
    }
}

/// `'utcnow' is deprecated: Use timezone-aware objects …`
fn describe(name: &str, message: &str) -> String {
    if message.is_empty() {
        format!("'{name}' is deprecated")
    } else {
        format!("'{name}' is deprecated: {message}")
    }
}

/// The reported range and name: a name, or the attribute name of `a.b`.
fn name_of(expr: &ast::Expr) -> Option<(TextRange, String)> {
    match expr {
        ast::Expr::Name(n) => Some((n.syntax().text_range(), n.name_text())),
        ast::Expr::Attribute(a) => a.attr().map(|t| (t.text_range(), t.text().to_string())),
        _ => None,
    }
}

/// Assignment and `del` targets are not uses.
fn is_store(node: &SyntaxNode) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        SyntaxKind::ASSIGN_STMT | SyntaxKind::ANN_ASSIGN_STMT | SyntaxKind::AUG_ASSIGN_STMT => {
            // Targets precede the `=`; the value follows it.
            let eq = parent
                .children_with_tokens()
                .filter_map(|e| e.into_token())
                .find(|t| {
                    matches!(t.kind(), SyntaxKind::EQ | SyntaxKind::COLON)
                        || t.kind().is_augmented_assign_op()
                });
            eq.is_none_or(|eq| node.text_range().end() <= eq.text_range().start())
        }
        SyntaxKind::DEL_STMT => true,
        _ => false,
    }
}

struct Checker<'a> {
    db: &'a Analysis,
    inf: Inferer<'a>,
    file: &'a Arc<FileInfo>,
    /// Deprecation message per definition node (`None`: not deprecated).
    cache: HashMap<(FileId, TextRange), Option<String>>,
}

impl Checker<'_> {
    /// The deprecation message for what `expr` refers to (for a callee, the
    /// overload or constructor the call selects).
    fn expr(&mut self, expr: &ast::Expr) -> Option<String> {
        self.expr_by_type(expr)
    }

    fn expr_by_type(&mut self, expr: &ast::Expr) -> Option<String> {
        let ty = self.inf.type_of_expr(self.file, expr);
        let call = expr
            .syntax()
            .parent()
            .and_then(ast::CallExpr::cast)
            .filter(|c| {
                c.callee()
                    .is_some_and(|callee| callee.syntax() == expr.syntax())
            });
        match &ty {
            Type::Class(c, _) => {
                if let Some(m) = self.node(c.file, c.range) {
                    return Some(m);
                }
            }
            Type::Function(s) | Type::BoundMethod(s, _) | Type::Callable(s) => {
                return self.signature(s)
            }
            // Uncalled: deprecated only when every overload is.
            Type::Overloaded(sigs) if call.is_none() => {
                let messages: Vec<Option<String>> =
                    sigs.iter().map(|s| self.signature(s)).collect();
                return messages
                    .iter()
                    .all(Option::is_some)
                    .then(|| messages.into_iter().flatten().next())
                    .flatten();
            }
            _ => {}
        }
        if let Some(call) = call {
            let sigs = rustpy_semantic::call_help::signatures_of(&self.inf, &ty);
            if sigs.len() > 1 {
                let (pos, kw) = self.inf.call_args(self.file, &call);
                let selected =
                    rustpy_semantic::call_help::select_for_call(&self.inf, &sigs, &pos, &kw)?;
                return self.signature(&selected);
            }
            if let Some(sig) = sigs.first() {
                if let Some(m) = self.signature(sig) {
                    return Some(m);
                }
            }
        }
        // Decorators that hide the function type (`@functools.cache`): the definition itself.
        if let ast::Expr::Name(n) = expr {
            if !matches!(
                ty,
                Type::Function(_) | Type::Class(..) | Type::Overloaded(_) | Type::BoundMethod(..)
            ) {
                let scope = self.file.scopes.scope_at(n.syntax().text_range().start());
                let def = self.inf.follow_imports(self.inf.resolve_name(
                    self.file,
                    scope,
                    &n.name_text(),
                )?);
                return self.definition(&def.file, def.binding().range, &def.binding().kind);
            }
        }
        None
    }

    fn signature(&mut self, sig: &Signature) -> Option<String> {
        let def = sig.def.as_ref()?;
        self.node(def.file, def.range)
    }

    /// What `from m import name` imports: a deprecated class or function; an
    /// overloaded function only when every overload is (the import resolves
    /// to the last overload, while calls are checked against the one they select).
    fn imported(&mut self, def: &Definition) -> Option<String> {
        let b = def.binding();
        if matches!(b.kind, BindingKind::Function) {
            let root = def.file.parse.syntax_node();
            let overloads: Vec<TextRange> = def
                .file
                .scopes
                .bindings_in(b.scope, &b.name)
                .iter()
                .map(|id| def.file.scopes.binding(*id))
                .filter(|x| matches!(x.kind, BindingKind::Function))
                .filter_map(|x| {
                    rustpy_semantic::edit_helpers::node_with_range(&root, x.node_range)
                        .and_then(ast::FuncDef::cast)
                })
                .filter(|f| {
                    f.has_decorator("overload")
                        || f.has_decorator("typing.overload")
                        || f.has_decorator("typing_extensions.overload")
                })
                .map(|f| f.syntax().text_range())
                .collect();
            if overloads.len() > 1 {
                let messages: Vec<Option<String>> = overloads
                    .iter()
                    .map(|r| self.node(def.file.id, *r))
                    .collect();
                return messages
                    .iter()
                    .all(Option::is_some)
                    .then(|| messages.into_iter().flatten().next())
                    .flatten();
            }
        }
        self.definition(&def.file, b.range, &b.kind)
    }

    /// A `def`/`class` binding's deprecation, located by its name range.
    fn definition(
        &mut self,
        file: &Arc<FileInfo>,
        name_range: TextRange,
        kind: &BindingKind,
    ) -> Option<String> {
        if !matches!(kind, BindingKind::Function | BindingKind::Class) {
            return None;
        }
        let tok = file
            .parse
            .syntax_node()
            .covering_element(name_range)
            .into_token()?;
        let def = tok
            .parent()
            .filter(|p| matches!(p.kind(), SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF))?;
        self.node(file.id, def.text_range())
    }

    /// The deprecation message of the `def`/`class` node at `range` in `file`.
    fn node(&mut self, file: FileId, range: TextRange) -> Option<String> {
        if let Some(cached) = self.cache.get(&(file, range)) {
            return cached.clone();
        }
        let found = self.db.file(file).and_then(|f| {
            let root = f.parse.syntax_node();
            let start = match root.covering_element(range) {
                rustpy_syntax::rowan::NodeOrToken::Node(n) => n,
                rustpy_syntax::rowan::NodeOrToken::Token(t) => t.parent()?,
            };
            let def = start.ancestors().find(|n| {
                matches!(n.kind(), SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF)
                    && n.text_range() == range
            })?;
            deprecation_message(&def)
        });
        self.cache.insert((file, range), found.clone());
        found
    }
}

#[cfg(test)]
mod tests {
    use rustpy_semantic::deprecation::deprecated_names;
    use std::path::{Path, PathBuf};

    use super::super::{inspect, InspectionOptions};
    use rustpy_semantic::db::Analysis;

    fn deprecations(src: &str) -> Vec<(String, String)> {
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let f = db.file(id).unwrap();
        inspect(&db, &f, &InspectionOptions::default())
            .into_iter()
            .filter(|p| p.diagnostic.code == "PyDeprecation")
            .map(|p| (src[p.diagnostic.range].to_string(), p.diagnostic.message))
            .collect()
    }

    #[test]
    fn stub_scan_finds_deprecated_names() {
        let mut names = std::collections::HashSet::new();
        super::scan_stub("class A:\n    @overload\n    def __init__(self) -> None: ...\n    @overload\n    @deprecated(\n        \"no\"\n    )\n    def __init__(self, x: int) -> None: ...\n    @classmethod\n    @typing_extensions.deprecated(\"x\")\n    def old(cls) -> None: ...\n    def new(self) -> None: ...\n@deprecated(\"y\")\nclass B: ...\ndef c(): ...\n", &mut names);
        let mut names: Vec<&str> = names.into_iter().collect();
        names.sort();
        assert_eq!(names, vec!["A", "B", "__init__", "old"]);
        // The bundled typeshed's markers.
        for name in ["utcnow", "NamedTuple", "no_type_check_decorator"] {
            assert!(super::STUB_NAMES.contains(name), "{name}");
        }
        assert!(!super::STUB_NAMES.contains("now"));
    }

    #[test]
    fn pep702_functions_and_classes() {
        let src = "from typing_extensions import deprecated\n\n\n@deprecated(\"Use g() instead\")\ndef f():\n    pass\n\n\n@deprecated(\"Old \" \"class\")\nclass Old:\n    pass\n\n\ndef g():\n    pass\n\n\nf()\ng()\nx = Old()\nisinstance(x, Old)\nh = f\n";
        let d = deprecations(src);
        assert_eq!(
            d,
            vec![
                (
                    "f".to_string(),
                    "'f' is deprecated: Use g() instead".to_string()
                ),
                (
                    "Old".to_string(),
                    "'Old' is deprecated: Old class".to_string()
                ),
                (
                    "Old".to_string(),
                    "'Old' is deprecated: Old class".to_string()
                ),
                (
                    "f".to_string(),
                    "'f' is deprecated: Use g() instead".to_string()
                )
            ]
        );
    }

    #[test]
    fn methods_and_typeshed_markers() {
        let src = "import datetime\nfrom datetime import datetime as dt\n\nnow = datetime.datetime.utcnow()\nlater = dt.utcnow()\nfine = dt.now()\n";
        let d = deprecations(src);
        assert_eq!(d.len(), 2, "{d:?}");
        assert!(
            d.iter().all(|(t, m)| t == "utcnow"
                && m.starts_with("'utcnow' is deprecated: Use timezone-aware objects")),
            "{d:?}"
        );
        let src = "class Api:\n    from warnings import deprecated\n\n    @deprecated(\"use run()\")\n    def start(self):\n        pass\n\n    def run(self):\n        self.start()\n";
        let d = deprecations(src);
        assert_eq!(
            d,
            vec![(
                "start".to_string(),
                "'start' is deprecated: use run()".to_string()
            )]
        );
    }

    #[test]
    fn calls_check_the_selected_overload() {
        let src = "from typing import overload\nfrom typing_extensions import deprecated\n\n\n@overload\n@deprecated(\"int support is deprecated\")\ndef conv(x: int) -> str: ...\n@overload\ndef conv(x: str) -> str: ...\ndef conv(x):\n    return str(x)\n\n\nconv(1)\nconv('a')\nc = conv\n";
        let d = deprecations(src);
        assert_eq!(
            d,
            vec![(
                "conv".to_string(),
                "'conv' is deprecated: int support is deprecated".to_string()
            )]
        );
        // The keyword form of typing.NamedTuple (typeshed marks that constructor overload).
        let src = "from typing import NamedTuple\n\nP = NamedTuple('P', x=int)\nQ = NamedTuple('Q', [('x', int)])\n";
        let d = deprecations(src);
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].1.contains("keyword arguments is deprecated"), "{d:?}");
    }

    #[test]
    fn interpreter_stdlib_sources_give_way_to_the_stub_markers() {
        // With an interpreter, the stdlib directory is on the search path, but
        // its modules resolve to the typeshed stubs, which carry `@deprecated`.
        let dir = std::env::temp_dir().join(format!("rustpy_dep_{}", std::process::id()));
        let lib = dir.join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        // Python 3.12's layout: `datetime` re-exports `_pydatetime`'s classes.
        std::fs::write(lib.join("datetime.py"), "from _pydatetime import *\n").unwrap();
        std::fs::write(lib.join("_pydatetime.py"), "class datetime:\n    @classmethod\n    def utcnow(cls):\n        return cls()\n\n    @classmethod\n    def now(cls):\n        return cls()\n").unwrap();
        std::fs::write(
            lib.join("threading.py"),
            "def currentThread():\n    return None\n\n\ndef current_thread():\n    return None\n",
        )
        .unwrap();
        let db = Analysis::new(rustpy_semantic::module::ModuleResolver::new(
            vec![dir.join("proj")],
            vec![lib.clone()],
            (3, 12),
        ));
        let src = "import datetime\nfrom datetime import datetime as dt\nfrom threading import currentThread\nimport threading\n\nstamp = datetime.datetime.utcnow()\nnow = datetime.datetime.now()\nlater = dt.utcnow()\ncurrentThread()\nthreading.current_thread()\n";
        let id = db.set_file_text(&dir.join("proj/m.py"), src);
        let f = db.file(id).unwrap();
        assert!(db.module("datetime").is_some_and(|m| m.is_stub));
        let d: Vec<String> = inspect(&db, &f, &InspectionOptions::default())
            .into_iter()
            .filter(|p| p.diagnostic.code == "PyDeprecation")
            .map(|p| src[p.diagnostic.range].to_string())
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            d,
            vec!["currentThread", "utcnow", "utcnow", "currentThread"]
        );
    }

    #[test]
    fn imports_of_overloaded_functions_need_every_overload_deprecated() {
        // typeshed deprecates one overload of each of these (the last one, which
        // is what the import resolves to); using them correctly is fine.
        let src = "from contextlib import contextmanager, asynccontextmanager\nfrom logging import getLevelName\nfrom urllib.request import urlopen\nfrom shlex import split\nfrom xml.etree.ElementTree import iterparse\nfrom typing import Generator\n\n\n@contextmanager\ndef cm() -> Generator[int, None, None]:\n    yield getLevelName(10)\n\n\nparts = split('a b')\n";
        assert_eq!(deprecations(src), vec![]);
        // Every overload deprecated: the import is reported, as are uncalled uses.
        let src = "from typing import overload\nfrom typing_extensions import deprecated\n\n\n@overload\n@deprecated(\"no ints\")\ndef conv(x: int) -> str: ...\n@overload\n@deprecated(\"no strs\")\ndef conv(x: str) -> str: ...\ndef conv(x):\n    return str(x)\n";
        let db = Analysis::for_project(Path::new("/proj"));
        db.set_file_text(Path::new("/proj/lib.py"), src);
        let user = "from lib import conv\nfrom contextlib import contextmanager\n\nf = conv\n";
        let id = db.set_file_text(Path::new("/proj/m.py"), user);
        let f = db.file(id).unwrap();
        let d: Vec<(String, String)> = inspect(&db, &f, &InspectionOptions::default())
            .into_iter()
            .filter(|p| p.diagnostic.code == "PyDeprecation")
            .map(|p| (user[p.diagnostic.range].to_string(), p.diagnostic.message))
            .collect();
        assert_eq!(
            d,
            vec![
                (
                    "conv".to_string(),
                    "'conv' is deprecated: no ints".to_string()
                ),
                (
                    "conv".to_string(),
                    "'conv' is deprecated: no ints".to_string()
                )
            ]
        );
    }

    #[test]
    fn version_gated_stub_deprecations_follow_the_target_version() {
        let stub = "import sys\nfrom typing_extensions import deprecated\n\nif sys.version_info >= (3, 15):\n    @deprecated(\"Deprecated since Python 3.15\")\n    class Old: ...\n    @deprecated(\"Deprecated since Python 3.15\")\n    def old() -> None: ...\nelse:\n    class Old: ...\n    def old() -> None: ...\n";
        let user = "from mylib import Old, old\n\nOld()\nold()\n";
        let run = |version: (u32, u32)| -> usize {
            let db = Analysis::new(rustpy_semantic::module::ModuleResolver::new(
                vec![PathBuf::from("/proj")],
                Vec::new(),
                version,
            ));
            db.set_file_text(Path::new("/proj/mylib.pyi"), stub);
            let id = db.set_file_text(Path::new("/proj/m.py"), user);
            let f = db.file(id).unwrap();
            inspect(&db, &f, &InspectionOptions::default())
                .iter()
                .filter(|p| p.diagnostic.code == "PyDeprecation")
                .count()
        };
        assert_eq!(run((3, 12)), 0);
        assert_eq!(run((3, 15)), 4);
        // typeshed: `webbrowser.MacOSXOSAScript` is deprecated from 3.15 on, on macOS only.
        let user = "import webbrowser\n\nwebbrowser.MacOSXOSAScript\n";
        let run = |version: (u32, u32), platform: &str| -> Vec<String> {
            let db = Analysis::new(
                rustpy_semantic::module::ModuleResolver::new(
                    vec![PathBuf::from("/proj")],
                    Vec::new(),
                    version,
                )
                .with_platform(platform),
            );
            let id = db.set_file_text(Path::new("/proj/m.py"), user);
            let f = db.file(id).unwrap();
            inspect(&db, &f, &InspectionOptions::default())
                .into_iter()
                .filter(|p| p.diagnostic.code == "PyDeprecation")
                .map(|p| p.diagnostic.message)
                .collect()
        };
        assert!(run((3, 12), "linux").is_empty());
        assert!(run((3, 12), "darwin").is_empty());
        assert!(run((3, 15), "darwin")
            .iter()
            .any(|m| m.contains("Deprecated since Python 3.15")));
    }

    #[test]
    fn deprecated_names_are_collected_once_per_file_version() {
        let db = Analysis::for_project(Path::new("/proj"));
        let lib = "from typing_extensions import deprecated\n\n\nclass Api:\n    @deprecated(\"use run()\")\n    def __init__(self):\n        pass\n\n    @deprecated(\"x\")\n    def start(self):\n        pass\n";
        let lib_id = db.set_file_text(Path::new("/proj/lib.py"), lib);
        let id = db.set_file_text(
            Path::new("/proj/m.py"),
            "from lib import Api\n\nApi().start()\n",
        );
        let f = db.file(id).unwrap();
        let lib_file = db.file(lib_id).unwrap();
        let first = inspect(&db, &f, &InspectionOptions::default());
        // The scan's result stays with the file version and is reused.
        assert_eq!(
            Some(
                deprecated_names(&lib_file)
                    .iter()
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
            ),
            Some(vec![
                "Api".to_string(),
                "__init__".to_string(),
                "start".to_string()
            ])
        );
        assert_eq!(inspect(&db, &f, &InspectionOptions::default()), first);
        assert_eq!(
            first
                .iter()
                .filter(|p| p.diagnostic.code == "PyDeprecation")
                .count(),
            2
        );
        // A new version starts empty.
        db.set_file_text(Path::new("/proj/lib.py"), &format!("{lib}\n"));
        assert_eq!(
            deprecated_names(&db.file(lib_id).unwrap()),
            deprecated_names(&lib_file)
        );
    }

    /// (text, severity, message, fixed source) per stdlib-table report.
    fn stdlib_reports(
        src: &str,
        version: (u32, u32),
    ) -> Vec<(String, rustpy_core::Severity, String, Option<String>)> {
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let f = db.file(id).unwrap();
        let opts = InspectionOptions {
            python_version: version,
            ..Default::default()
        };
        inspect(&db, &f, &opts)
            .into_iter()
            .filter(|p| p.diagnostic.code == "PyDeprecation")
            .map(|p| {
                let fixed = p.fixes.iter().find(|x| !x.is_suppression()).map(|fix| {
                    let mut edits = fix.edits.clone();
                    edits.sort_by_key(|(r, _)| std::cmp::Reverse(r.start()));
                    let mut out = src.to_string();
                    for (r, text) in edits {
                        out.replace_range(usize::from(r.start())..usize::from(r.end()), &text);
                    }
                    out
                });
                (
                    src[p.diagnostic.range].to_string(),
                    p.diagnostic.severity,
                    p.diagnostic.message,
                    fixed,
                )
            })
            .collect()
    }

    #[test]
    fn collections_abc_aliases() {
        use rustpy_core::Severity;
        let src = "from collections import Mapping, OrderedDict\n\n\ndef f(x: Mapping) -> OrderedDict:\n    return OrderedDict(x)\n";
        let d = stdlib_reports(src, (3, 9));
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].0, "Mapping");
        assert_eq!(d[0].1, Severity::WeakWarning);
        assert!(d[0].2.contains("import it from 'collections.abc'"), "{d:?}");
        assert_eq!(
            d[0].3.as_deref(),
            Some("from collections import OrderedDict\nfrom collections.abc import Mapping\n\n\ndef f(x: Mapping) -> OrderedDict:\n    return OrderedDict(x)\n")
        );
        // Removed from 3.10 on: an error; an all-ABC import just changes module.
        let src = "from collections import Iterable, Sized as S\n";
        let d = stdlib_reports(src, (3, 12));
        assert_eq!(d.len(), 2, "{d:?}");
        assert!(d
            .iter()
            .all(|x| x.1 == Severity::Error
                && x.2.contains("removed from 'collections' in Python 3.10")));
        assert_eq!(
            d[0].3.as_deref(),
            Some("from collections.abc import Iterable, Sized as S\n")
        );
        // A parenthesised multi-line import keeps its layout.
        let src = "from collections import (\n    Mapping,\n    OrderedDict,  # ordered\n    Iterable as It,\n    defaultdict,\n    Sized,\n)\n";
        let d = stdlib_reports(src, (3, 12));
        assert_eq!(d.len(), 3, "{d:?}");
        assert!(d.iter().all(|x| x.3 == d[0].3));
        assert_eq!(
            d[0].3.as_deref(),
            Some("from collections import (\n    OrderedDict,  # ordered\n    defaultdict,\n)\nfrom collections.abc import Mapping, Iterable as It, Sized\n")
        );
        let src = "def f():\n    from collections import (OrderedDict,\n                             Mapping)\n    return Mapping\n";
        let d = stdlib_reports(src, (3, 12));
        assert_eq!(
            d[0].3.as_deref(),
            Some("def f():\n    from collections import (OrderedDict)\n    from collections.abc import Mapping\n    return Mapping\n")
        );
        let src = "import os; from collections import Mapping, OrderedDict\n";
        let d = stdlib_reports(src, (3, 12));
        assert_eq!(
            d[0].3.as_deref(),
            Some("import os; from collections import OrderedDict; from collections.abc import Mapping\n")
        );
        // Attribute access through the module.
        let src = "import collections\n\nx = isinstance({}, collections.Mapping)\ny = collections.OrderedDict()\n";
        let d = stdlib_reports(src, (3, 12));
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].0, "Mapping");
        assert_eq!(
            d[0].3.as_deref(),
            Some("import collections\nimport collections.abc\n\nx = isinstance({}, collections.abc.Mapping)\ny = collections.OrderedDict()\n")
        );
        let src = "import collections\nimport collections.abc\n\nx = collections.Sequence\n";
        let d = stdlib_reports(src, (3, 8));
        assert_eq!(
            d[0].3.as_deref(),
            Some("import collections\nimport collections.abc\n\nx = collections.abc.Sequence\n")
        );
        // Not reported: the right module, other names, shadowing receivers.
        for src in [
            "import collections.abc\n\nx = collections.abc.Mapping\n",
            "from collections.abc import Mapping, Iterable\n",
            "from collections import OrderedDict, defaultdict, abc\n\nx = abc.Mapping\n",
            "import collections\n\nx = collections.OrderedDict()\ny = collections.abc.Iterable\n",
            "import collections.abc as c\n\nx = c.Mapping\n",
            "import collections\n\n\nclass collections:\n    Mapping = dict\n\n\nx = collections.Mapping\n",
            "import collections\n\n\ndef f(collections):\n    return collections.Mapping\n",
        ] {
            assert_eq!(stdlib_reports(src, (3, 12)), vec![], "{src}");
        }
    }

    #[test]
    fn deprecated_stdlib_modules() {
        use rustpy_core::Severity;
        let src = "import imp\nfrom imp import reload\nimport os\n\nimp.reload(os)\n";
        let d = stdlib_reports(src, (3, 8));
        assert_eq!(
            d.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(),
            vec!["imp", "imp", "imp"]
        );
        assert!(
            d.iter().all(|x| x.1 == Severity::WeakWarning
                && x.2 == "'imp' is deprecated since Python 3.4: use importlib"),
            "{d:?}"
        );
        // Removed modules are errors for targets without them.
        let d = stdlib_reports("import imp\nimport distutils.core as dc\n", (3, 12));
        assert_eq!(d.len(), 2, "{d:?}");
        assert_eq!(d[0].2, "'imp' was removed in Python 3.12: use importlib");
        assert_eq!(d[1].0, "distutils.core");
        assert!(d.iter().all(|x| x.1 == Severity::Error), "{d:?}");
        // The removal error is the single report on such an import.
        let src = "import imp\nimport distutils.core\nfrom asyncore import loop\nimport nonexistent_mod\n";
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let f = db.file(id).unwrap();
        let all: Vec<(String, String)> = inspect(&db, &f, &InspectionOptions::default())
            .into_iter()
            .filter(|p| {
                p.diagnostic.message.starts_with("No module named")
                    || p.diagnostic.code == "PyDeprecation"
            })
            .map(|p| {
                (
                    src[p.diagnostic.range].to_string(),
                    p.diagnostic.code.to_string(),
                )
            })
            .collect();
        let pairs = |v: &[(&str, &str)]| -> Vec<(String, String)> {
            v.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        };
        assert_eq!(
            all,
            pairs(&[
                ("imp", "PyDeprecation"),
                ("distutils.core", "PyDeprecation"),
                ("asyncore", "PyDeprecation"),
                ("nonexistent_mod", "PyUnresolvedReferences"),
            ])
        );
        // With the deprecation inspection off, the unresolved import remains.
        let opts = InspectionOptions {
            deprecation: false,
            ..Default::default()
        };
        assert!(inspect(&db, &f, &opts)
            .iter()
            .any(|p| p.diagnostic.message == "No module named 'imp'"));
        // Version-aware: distutils (3.10) and PEP 594 modules (3.11).
        let src = "import distutils\nimport cgi\nimport asyncore\n";
        let names = |v| {
            stdlib_reports(src, v)
                .into_iter()
                .map(|x| x.0)
                .collect::<Vec<_>>()
        };
        assert_eq!(names((3, 9)), vec!["asyncore"]);
        assert_eq!(names((3, 10)), vec!["distutils", "asyncore"]);
        assert_eq!(names((3, 11)), vec!["distutils", "cgi", "asyncore"]);
        // Neighbours, relative imports and project modules of the same name are fine.
        for src in [
            "import importlib\nimport impala_like\nfrom importlib import reload\n",
            "from . import imp\nfrom .imp import x\n",
        ] {
            assert_eq!(stdlib_reports(src, (3, 8)), vec![], "{src}");
        }
        let db = Analysis::for_project(Path::new("/proj"));
        db.set_file_text(Path::new("/proj/imp.py"), "def reload(m):\n    return m\n");
        let id = db.set_file_text(Path::new("/proj/m.py"), "import imp\n\nimp.reload(1)\n");
        let f = db.file(id).unwrap();
        assert!(!inspect(&db, &f, &InspectionOptions::default())
            .iter()
            .any(|p| p.diagnostic.code == "PyDeprecation"));
    }

    #[test]
    fn typeshed_markers_still_apply_alongside_the_tables() {
        // asyncio.coroutine is marked in typeshed for targets before 3.11.
        let src = "import asyncio\nimport collections\n\n\n@asyncio.coroutine\ndef f():\n    yield\n\n\nx = collections.Mapping\n";
        let db = Analysis::new(rustpy_semantic::module::ModuleResolver::new(
            vec![PathBuf::from("/proj")],
            Vec::new(),
            (3, 9),
        ));
        let id = db.set_file_text(Path::new("/proj/m.py"), src);
        let f = db.file(id).unwrap();
        let opts = InspectionOptions {
            python_version: (3, 9),
            ..Default::default()
        };
        let texts: Vec<String> = inspect(&db, &f, &opts)
            .into_iter()
            .filter(|p| p.diagnostic.code == "PyDeprecation")
            .map(|p| src[p.diagnostic.range].to_string())
            .collect();
        assert_eq!(texts, vec!["coroutine", "Mapping"]);
    }

    #[test]
    fn imports_and_suppression() {
        let src = "from typing import no_type_check_decorator\n";
        assert_eq!(deprecations(src).len(), 1, "{:?}", deprecations(src));
        let src = "from typing_extensions import deprecated\n\n\n@deprecated(\"x\")\ndef f():\n    pass\n\n\n# noinspection PyDeprecation\nf()\n";
        assert!(deprecations(src).is_empty());
        // Opt-out.
        let db = Analysis::for_project(Path::new("/proj"));
        let id = db.set_file_text(
            Path::new("/proj/m.py"),
            "import datetime\ndatetime.datetime.utcnow()\n",
        );
        let f = db.file(id).unwrap();
        let opts = InspectionOptions {
            deprecation: false,
            ..Default::default()
        };
        assert!(!inspect(&db, &f, &opts)
            .iter()
            .any(|p| p.diagnostic.code == "PyDeprecation"));
    }
}
