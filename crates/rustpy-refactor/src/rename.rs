//! Rename validation, conflicts, project edits and in-place planning (roadmap §2.8).
//!
//! Resolution and usage searches belong to `rustpy-semantic`; this module
//! decides which occurrences to edit and whether those edits preserve meaning.

use std::path::PathBuf;
use std::sync::Arc;

use rustpy_core::{Cancelled, ProgressReporter, TextRange, TextSize};
use rustpy_search::usages::{find_usages, find_usages_with_progress, search_usages};
use rustpy_semantic::db::{Analysis, FileInfo};
use rustpy_semantic::infer::Inferer;
use rustpy_semantic::scope::{BindingKind, ScopeKind};
use rustpy_semantic::types::Type;
use rustpy_semantic::usages::{
    is_library_file, is_renaming_alias, line_info, name_origin, same_binding, symbol_at,
    text_occurrences, Symbol, Usage,
};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::SyntaxKind;

/// Rename edits grouped by file: `(path, [(range, new text)])`.
pub type RenameEdits = Vec<(PathBuf, Vec<(TextRange, String)>)>;

/// Refuses symbols that renaming at their usages would break: definitions
/// in libraries (renaming `len` or `list.append` would only rewrite the
/// project's calls), module imports (`import os`: use Rename Module) and
/// imports that do not resolve, and attributes of objects of unknown type
/// (`worker.run` with an untyped `worker`: no declaration to rename, and
/// every same-named attribute in the project would be a guess). Local
/// import aliases (`as h`) can be renamed.
pub fn check_rename_target(db: &Analysis, sym: &Symbol) -> Result<(), String> {
    match sym {
        Symbol::Name { def, name } => {
            let origin = name_origin(&Inferer::new(db), def);
            match &origin.binding().kind {
                // A local alias (`as h`) is just a name.
                BindingKind::Import(_) if is_renaming_alias(origin.binding()) => Ok(()),
                BindingKind::Import(info) if info.name.is_none() => Err(format!("'{name}' names a module: use Refactor › Rename Module to rename it")),
                BindingKind::Import(_) => Err(format!("Cannot resolve the definition of '{name}'")),
                _ if is_library_file(db, &origin.file) => Err(format!("Cannot rename '{name}': it is defined in a library ({})", origin.file.path.display())),
                _ => Ok(()),
            }
        }
        Symbol::Member { name, decl: Some((f, _)), .. } if is_library_file(db, f) => Err(format!("Cannot rename '{name}': it is defined in a library ({})", f.path.display())),
        Symbol::Member { name, owner: Type::Unknown | Type::Any, decl: None } => Err(format!("Cannot find the declaration of '{name}': the type of the object it is accessed on is unknown. Rename it at its definition")),
        // `pkg.sub` with `sub` a submodule: renaming the attribute alone would break the import.
        Symbol::Member { name, owner: Type::Module(_), decl: None } => Err(format!("The module does not define '{name}': to rename a submodule, use Refactor › Rename Module")),
        _ => Ok(()),
    }
}

/// [`check_rename_target`] for the symbol at `offset` (UIs call it before
/// starting an in-place rename).
pub fn check_rename_at(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
) -> Result<(), String> {
    rename_check(db, file, offset).map(|_| ())
}

/// `name` is a Python identifier and not a keyword.
pub fn check_identifier(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    if !chars.next().is_some_and(rustpy_core::is_ident_start)
        || !chars.all(rustpy_core::is_ident_continue)
    {
        return Err(format!("'{name}' is not a valid Python identifier"));
    }
    if rustpy_semantic::features::PYTHON_KEYWORDS.contains(&name) {
        return Err(format!("'{name}' is a keyword"));
    }
    Ok(())
}

/// What a new name for a symbol must satisfy, worked out from the code
/// before the rename: an in-place rename checks the typed name against it
/// when every occurrence already shows that name.
#[derive(Clone, Debug, Default)]
pub struct RenameCheck {
    /// The symbol's name before the rename.
    pub old_name: String,
    /// Names the symbol cannot take, with the reason.
    taken: Vec<(String, String)>,
}

impl RenameCheck {
    /// Only the identifier and keyword rules: a name a refactoring has just
    /// introduced (Extract Variable & co.) may reuse an existing name.
    pub fn identifier_only(old_name: &str) -> Self {
        Self {
            old_name: old_name.to_string(),
            taken: Vec::new(),
        }
    }

    /// Whether the symbol can be renamed to `new_name`.
    pub fn check(&self, new_name: &str) -> Result<(), String> {
        check_identifier(new_name)?;
        protected_name(&self.old_name, new_name)?;
        if new_name != self.old_name {
            if let Some((_, why)) = self.taken.iter().find(|(n, _)| n == new_name) {
                return Err(why.clone());
            }
        }
        Ok(())
    }
}

/// The checks a new name for the symbol at `offset` must pass (after
/// [`check_rename_target`] for the symbol itself): renaming a name onto
/// another name of its scope, or a member onto another member of its class
/// or a base class, would merge two symbols.
pub fn rename_check(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
) -> Result<RenameCheck, String> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let sym = symbol_at(db, file, offset).ok_or("Cannot find a symbol to rename")?;
    check_rename_target(db, &sym)?;
    let inf = Inferer::new(db);
    let old_name = sym.name().to_string();
    let mut taken: Vec<(String, String)> = Vec::new();
    let mut take = |n: &str, why: String| {
        if n != old_name && !taken.iter().any(|(t, _)| t == n) {
            taken.push((n.to_string(), why));
        }
    };
    match &sym {
        Symbol::Name { def, name } => {
            let origin = name_origin(&inf, def);
            for d in [def, &origin] {
                for n in d.file.scopes.scope(d.binding().scope).bindings.keys() {
                    take(
                        n.as_str(),
                        format!("'{n}' is already defined in the scope of '{name}'"),
                    );
                }
            }
        }
        Symbol::Member {
            owner: Type::Module(m),
            name,
            ..
        } => {
            // `module.name`: the other names of that module.
            if let Some(f) = db.file(*m) {
                for n in f.scopes.scope(f.scopes.module_scope()).bindings.keys() {
                    take(
                        n.as_str(),
                        format!("'{n}' is already defined in the scope of '{name}'"),
                    );
                }
            }
        }
        Symbol::Member { owner, .. } => {
            // Members of the class and of its bases in the project (overriding a
            // library method such as `__eq__` stays possible).
            let mro = owner.class_ref().map(|c| inf.mro(c)).unwrap_or_default();
            for cls in mro.iter() {
                let Some(info) = inf.class_info(cls) else {
                    continue;
                };
                if db.file(cls.file).is_some_and(|f| is_library_file(db, &f)) {
                    continue;
                }
                for n in info.members.keys() {
                    take(
                        n.as_str(),
                        format!("'{n}' is already defined in class '{}'", cls.name),
                    );
                }
            }
        }
        Symbol::Keyword { .. } => {}
    }
    Ok(RenameCheck { old_name, taken })
}

/// The edits renaming the symbol at `offset` to `new_name`, for callers
/// that wait for them (the usage search runs in the caller's rayon pool,
/// like [`find_usages`]).
pub fn rename(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    new_name: &str,
    project_files: &[PathBuf],
) -> Result<RenameEdits, String> {
    rename_using(
        db,
        file,
        offset,
        new_name,
        project_files,
        &ProgressReporter::new(),
        search_usages,
    )
}

/// [`rename`] as a background task: the usage search runs in the background
/// scan pool (like [`find_usages_with_progress`]), reported to (and
/// cancelled through) `progress`.
pub fn rename_with_progress(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    new_name: &str,
    project_files: &[PathBuf],
    progress: &ProgressReporter,
) -> Result<RenameEdits, String> {
    rename_using(
        db,
        file,
        offset,
        new_name,
        project_files,
        progress,
        find_usages_with_progress,
    )
}

/// Signature of [`search_usages`] and [`find_usages_with_progress`].
type UsageSearch = fn(
    &Analysis,
    &Arc<FileInfo>,
    TextSize,
    &[PathBuf],
    &ProgressReporter,
) -> Result<Vec<Usage>, Cancelled>;

fn rename_using(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    new_name: &str,
    project_files: &[PathBuf],
    progress: &ProgressReporter,
    search: UsageSearch,
) -> Result<RenameEdits, String> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    check_identifier(new_name)?;
    rename_check(db, file, offset)?.check(new_name)?;
    if let Some(sym) = symbol_at(db, file, offset) {
        check_rename_target(db, &sym)?;
    }
    let usages = search(db, file, offset, project_files, progress)
        .map_err(|_| "Rename was cancelled".to_string())?;
    if usages.is_empty() {
        return Err("Cannot find a symbol to rename".into());
    }
    // Code usages only: dynamic ones (receivers of unknown or ambiguous type,
    // keyword arguments of unresolved calls) are left for the preview.
    let code: Vec<Usage> = usages.into_iter().filter(|u| !u.dynamic).collect();
    if code.is_empty() {
        return Err("Cannot find the declaration of the symbol to rename".into());
    }
    if let Some(sym) = symbol_at(db, file, offset) {
        if let Some(c) = rename_conflicts(db, &sym, &code, new_name, project_files).first() {
            return Err(c.message.clone());
        }
    }
    Ok(rename_edits(code.iter(), new_name))
}

/// What Rename searches besides code.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct RenameOptions {
    /// Settings › Refactoring › "Search in comments and strings".
    pub search_in_comments: bool,
}

/// A reason renaming would change what the code means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenameConflict {
    pub path: PathBuf,
    pub range: TextRange,
    pub line: usize,
    /// Byte offset of `range` in `line_text`.
    pub column: usize,
    pub line_text: String,
    pub message: String,
}

/// Everything a rename would touch, for the Refactoring Preview.
#[derive(Clone, Debug, Default)]
pub struct RenamePlan {
    pub old_name: String,
    pub new_name: String,
    /// Definitions and references resolved to the symbol.
    pub code: Vec<Usage>,
    /// Same-named attributes of receivers of unknown type (or of a union
    /// with another class that has the member) and keyword arguments of
    /// unresolved calls: possibly other symbols.
    pub dynamic: Vec<Usage>,
    /// Whole-word occurrences in comments and strings.
    pub text: Vec<Usage>,
    pub conflicts: Vec<RenameConflict>,
}

impl RenamePlan {
    /// Whether the user should look before renaming: dynamic usages and
    /// text occurrences are not certain, conflicts change behaviour.
    pub fn needs_preview(&self) -> bool {
        !self.dynamic.is_empty() || !self.text.is_empty() || !self.conflicts.is_empty()
    }
}

/// Edits renaming `usages` to `new_name`, grouped by file, sorted, without duplicates.
pub fn rename_edits<'a>(
    usages: impl IntoIterator<Item = &'a Usage>,
    new_name: &str,
) -> RenameEdits {
    let mut by_file: RenameEdits = Vec::new();
    for u in usages {
        match by_file.iter_mut().find(|(p, _)| *p == u.path) {
            Some((_, edits)) => edits.push((u.range, new_name.to_string())),
            None => by_file.push((u.path.clone(), vec![(u.range, new_name.to_string())])),
        }
    }
    for (_, edits) in &mut by_file {
        edits.sort_by_key(|(r, _)| r.start());
        edits.dedup_by_key(|(r, _)| *r);
    }
    by_file
}

/// Special-method names participate in Python protocols, not ordinary calls.
pub fn protected_name(old: &str, new: &str) -> Result<(), String> {
    if old != new
        && ((old.starts_with("__") && old.ends_with("__"))
            || (new.starts_with("__") && new.ends_with("__")))
    {
        return Err("Python protocol dunder names are protected; change the implementation instead of renaming the protocol".into());
    }
    Ok(())
}

/// Whether `new_name` can name a Python symbol (an identifier, not a keyword).
pub fn check_new_name(new_name: &str) -> Result<(), String> {
    if !rustpy_core::is_ident_start(new_name.chars().next().unwrap_or(' '))
        || !new_name.chars().all(rustpy_core::is_ident_continue)
    {
        return Err(format!("'{new_name}' is not a valid Python identifier"));
    }
    if rustpy_semantic::features::PYTHON_KEYWORDS.contains(&new_name) {
        return Err(format!("'{new_name}' is a keyword"));
    }
    Ok(())
}

/// The rename of the symbol at `offset` to `new_name`: code usages,
/// dynamic usages, comment/string occurrences (with
/// `opts.search_in_comments`) and conflicts.
pub fn rename_plan(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    new_name: &str,
    project_files: &[PathBuf],
    opts: RenameOptions,
) -> Result<RenamePlan, String> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    check_new_name(new_name)?;
    let sym = symbol_at(db, file, offset).ok_or("Cannot find a symbol to rename")?;
    check_rename_target(db, &sym)?;
    let old_name = sym.name().to_string();
    protected_name(&old_name, new_name)?;
    if old_name == new_name {
        return Err(format!("The symbol is already named '{new_name}'"));
    }
    let mut usages = find_usages(db, file, offset, project_files);
    let paired = source_stub_counterparts(db, &sym, project_files);
    for (owner, range) in &paired {
        usages.extend(find_usages(db, owner, range.start(), project_files));
        usages.push(rustpy_semantic::usages::usage_at(owner, *range, true, true));
    }
    usages.sort_by_key(|usage| (usage.path.clone(), usage.range.start(), usage.range.end()));
    usages.dedup_by(|a, b| a.path == b.path && a.range == b.range);
    if usages.is_empty() {
        return Err("Cannot find a symbol to rename".into());
    }
    let (dynamic, code): (Vec<Usage>, Vec<Usage>) = usages.into_iter().partition(|u| u.dynamic);
    if code.is_empty() {
        // Only same-named attributes of unknown objects: nothing certain to rename.
        return Err(format!("Cannot find the declaration of '{old_name}'"));
    }
    let text = if opts.search_in_comments {
        text_occurrences_of(db, &sym, &code, &dynamic, project_files)
    } else {
        Vec::new()
    };
    let mut conflicts = rename_conflicts(db, &sym, &code, new_name, project_files);
    for (owner, range) in paired {
        if let Some(other) = symbol_at(db, &owner, range.start()) {
            conflicts.extend(rename_conflicts(db, &other, &code, new_name, project_files));
        }
    }
    Ok(RenamePlan {
        old_name,
        new_name: new_name.to_string(),
        code,
        dynamic,
        text,
        conflicts,
    })
}

/// Match paired declarations by their complete physical lexical path, never
/// by spelling across unrelated modules or classes. Each overload declaration
/// in the paired file participates. Imports and function-local temporaries do
/// not acquire a fictional paired declaration.
fn source_stub_counterparts(
    db: &Analysis,
    symbol: &Symbol,
    project_files: &[PathBuf],
) -> Vec<(Arc<FileInfo>, TextRange)> {
    let (owner, range) = match symbol {
        Symbol::Name { def, .. } => (def.file.clone(), def.range()),
        Symbol::Member {
            decl: Some((file, range)),
            ..
        } => (file.clone(), *range),
        _ => return Vec::new(),
    };
    let Some((_, binding)) = owner.scopes.binding_at(range.start()) else {
        return Vec::new();
    };
    if matches!(binding.kind, BindingKind::Import(_)) {
        return Vec::new();
    }
    let extension = match owner.path.extension().and_then(|s| s.to_str()) {
        Some("py") => "pyi",
        Some("pyi") => "py",
        _ => return Vec::new(),
    };
    if !project_files.contains(&owner.path) {
        return Vec::new();
    }
    let path = owner.path.with_extension(extension);
    let Some(other) = db.file_for_path(&path) else {
        return Vec::new();
    };
    let Some(key) = declaration_path(&owner, range) else {
        return Vec::new();
    };
    other
        .scopes
        .bindings
        .iter()
        .filter(|candidate| {
            candidate.name == binding.name
                && std::mem::discriminant(&candidate.kind) == std::mem::discriminant(&binding.kind)
                && declaration_path(&other, candidate.range).as_ref() == Some(&key)
        })
        .take(128)
        .map(|b| (other.clone(), b.range))
        .collect()
}
fn declaration_path(file: &FileInfo, range: TextRange) -> Option<Vec<String>> {
    let root = file.parse.syntax_node();
    let token = root
        .token_at_offset(range.start())
        .find(|token| token.text_range() == range)?;
    let mut path = Vec::new();
    for ancestor in token.parent_ancestors().take(128) {
        if let Some(function) = ast::FuncDef::cast(ancestor.clone()) {
            path.push(function.name()?.text().to_string())
        } else if let Some(class) = ast::ClassDef::cast(ancestor) {
            path.push(class.name()?.text().to_string())
        }
    }
    path.reverse();
    path.push(token.text().to_string());
    Some(path)
}

/// An in-place rename (multi-caret editing in one file).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InplaceRanges {
    /// The occurrences typed over together.
    pub ranges: Vec<TextRange>,
    /// String literals with the name that are left as they are (data such
    /// as `request.args["name"]`); the dialog's preview offers them.
    pub strings_left_out: usize,
}

/// [`inplace_rename`]'s ranges.
pub fn inplace_rename_ranges(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    project_files: &[PathBuf],
    search_in_comments: bool,
) -> Option<Vec<TextRange>> {
    inplace_rename(db, file, offset, project_files, search_in_comments).map(|r| r.ranges)
}

/// The in-place rename of the symbol at `offset` (multi-caret editing in
/// this file): its usages and, with `search_in_comments`, its occurrences
/// in comments and docstrings. Other string literals are data (a dict key,
/// `request.args["name"]`): a local's are left alone, as in-place
/// rename leaves strings alone; a module-level name or a member may be
/// named by one (`__all__`, `getattr(obj, "run")`), so it needs the dialog.
/// `None` when the rename needs the dialog: also for a usage or an
/// occurrence in another file, or a dynamic usage (it may belong to
/// another symbol).
pub fn inplace_rename(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    project_files: &[PathBuf],
    search_in_comments: bool,
) -> Option<InplaceRanges> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let sym = symbol_at(db, file, offset)?;
    check_rename_target(db, &sym).ok()?;
    if !source_stub_counterparts(db, &sym, project_files).is_empty() {
        return None;
    }
    let usages = find_usages(db, file, offset, project_files);
    if usages.is_empty() || usages.iter().any(|u| u.path != file.path || u.dynamic) {
        return None;
    }
    let mut ranges: Vec<TextRange> = usages.iter().map(|u| u.range).collect();
    let mut strings_left_out = 0;
    if search_in_comments {
        let text = text_occurrences_of(db, &sym, &usages, &[], project_files);
        if text.iter().any(|u| u.path != file.path) {
            return None;
        }
        let docstrings = docstring_ranges(file);
        let (data, prose): (Vec<&Usage>, Vec<&Usage>) = text
            .iter()
            .partition(|u| in_data_string(file, &docstrings, u.range));
        if !data.is_empty() && local_scope_of(&sym).is_none() {
            return None;
        }
        strings_left_out = data.len();
        ranges.extend(prose.iter().map(|u| u.range));
        ranges.sort_by_key(|r| r.start());
        ranges.dedup();
    }
    Some(InplaceRanges {
        ranges,
        strings_left_out,
    })
}

/// The docstrings of `file`: the module's and those of its classes and functions.
fn docstring_ranges(file: &FileInfo) -> Vec<TextRange> {
    let root = file.parse.syntax_node();
    let module = ast::Module::cast(root.clone()).and_then(|m| m.docstring());
    let defs = root.descendants().filter_map(|n| match n.kind() {
        SyntaxKind::FUNC_DEF => ast::FuncDef::cast(n).and_then(|f| f.docstring()),
        SyntaxKind::CLASS_DEF => ast::ClassDef::cast(n).and_then(|c| c.docstring()),
        _ => None,
    });
    module
        .into_iter()
        .chain(defs)
        .map(|d| d.syntax().text_range())
        .collect()
}

/// Whether a [`text_occurrences`] match at `range` is in a string literal
/// that is data, not in a comment or a docstring (`docstrings`).
fn in_data_string(file: &FileInfo, docstrings: &[TextRange], range: TextRange) -> bool {
    let comment = file
        .parse
        .syntax_node()
        .covering_element(range)
        .as_token()
        .is_some_and(|t| t.kind() == SyntaxKind::COMMENT);
    !comment && !docstrings.iter().any(|d| d.contains_range(range))
}

/// The function (lambda, comprehension) of a local variable or parameter;
/// `None` for module- and class-level names and for members.
fn local_scope_of(sym: &Symbol) -> Option<(Arc<FileInfo>, TextRange)> {
    match sym {
        Symbol::Name { def, .. } => {
            let s = def.file.scopes.scope(def.binding().scope);
            (!matches!(s.kind, ScopeKind::Module | ScopeKind::Class))
                .then(|| (def.file.clone(), s.range))
        }
        _ => None,
    }
}

/// Problems of an in-place rename the user has finished typing: `ranges`
/// are the occurrences in `file` (which now read `new_name`), all of them
/// usages of the one symbol that was called `old_name`. Reports names that
/// merged with an existing `new_name` of the scope, usages now captured by
/// another `new_name`, and references to an outer or builtin `new_name`
/// that the renamed symbol now shadows.
pub fn inplace_rename_conflicts(
    file: &FileInfo,
    ranges: &[TextRange],
    old_name: &str,
    new_name: &str,
) -> Vec<RenameConflict> {
    let scopes = &file.scopes;
    let renamed = |r: TextRange| ranges.contains(&r);
    let mut out: Vec<RenameConflict> = Vec::new();
    let mut push = |range: TextRange, message: String| {
        if !out.iter().any(|c| c.range == range) {
            let (line, column, line_text) = line_info(file, range);
            out.push(RenameConflict {
                path: file.path.clone(),
                range,
                line,
                column,
                line_text,
                message,
            });
        }
    };
    let defs: Vec<rustpy_semantic::scope::BindingId> = (0..scopes.bindings.len())
        .map(|i| rustpy_semantic::scope::BindingId(i as u32))
        .filter(|id| {
            let b = scopes.binding(*id);
            b.name.as_str() == new_name && b.is_definition() && renamed(b.range)
        })
        .collect();
    let mut merged = false;
    let mut def_scopes: Vec<rustpy_semantic::scope::ScopeId> =
        defs.iter().map(|d| scopes.binding(*d).scope).collect();
    def_scopes.dedup();
    for scope in &def_scopes {
        if let Some(other) = scopes
            .bindings_in(*scope, new_name)
            .iter()
            .map(|id| scopes.binding(*id))
            .find(|b| b.is_definition() && !renamed(b.range))
        {
            merged = true;
            let line = line_info(file, other.range).0 + 1;
            push(other.range, format!("'{new_name}' was already defined here (line {line}): '{old_name}' and '{new_name}' are now one variable"));
        }
    }
    for r in scopes
        .references
        .iter()
        .filter(|r| r.name.as_str() == new_name)
    {
        let resolved = scopes.resolve(r.scope, new_name);
        let to_renamed = resolved.is_some_and(|ids| ids.iter().any(|id| defs.contains(id)));
        if renamed(r.range) {
            // A usage of the renamed symbol must still reach it.
            if let Some(other) = resolved
                .filter(|_| !to_renamed && !defs.is_empty())
                .and_then(|ids| ids.first())
            {
                let line = line_info(file, scopes.binding(*other).range).0 + 1;
                push(r.range, format!("This usage of '{old_name}' now refers to the '{new_name}' defined on line {line}"));
            }
        } else if to_renamed && !merged {
            push(r.range, format!("'{new_name}' here now refers to the renamed '{old_name}' instead of an outer or builtin '{new_name}'"));
        }
    }
    out.sort_by_key(|c| c.range.start());
    out
}

/// Comment and string occurrences of `sym`'s name that are not already
/// usages: in the enclosing function for a local symbol, else project-wide.
fn text_occurrences_of(
    db: &Analysis,
    sym: &Symbol,
    code: &[Usage],
    dynamic: &[Usage],
    project_files: &[PathBuf],
) -> Vec<Usage> {
    let name = sym.name();
    let known = |u: &Usage| {
        code.iter().chain(dynamic).any(|c| {
            c.path == u.path && c.range.start() < u.range.end() && u.range.start() < c.range.end()
        })
    };
    // A local variable or parameter: only its function (docstring, comments).
    let mut out: Vec<Usage> = Vec::new();
    match local_scope_of(sym) {
        Some((f, range)) => out.extend(
            text_occurrences(&f, name, Some(range))
                .into_iter()
                .filter(|u| !known(u)),
        ),
        None => {
            let mut seen = std::collections::HashSet::new();
            let files = code
                .iter()
                .map(|u| u.path.clone())
                .chain(project_files.iter().cloned());
            for path in files {
                if path.extension().and_then(|e| e.to_str()) != Some("py")
                    || !seen.insert(path.clone())
                {
                    continue;
                }
                let Some(f) = db.file_for_path(&path) else {
                    continue;
                };
                if is_library_file(db, &f) {
                    continue;
                }
                out.extend(
                    text_occurrences(&f, name, None)
                        .into_iter()
                        .filter(|u| !known(u)),
                );
            }
        }
    }
    out
}

/// Where renaming `sym` to `new_name` would change behaviour:
/// - the new name is already bound in the symbol's scope (two symbols merge);
/// - a usage would be captured by an inner binding of the new name;
/// - the renamed symbol would shadow an outer or builtin `new_name` used
///   inside its scope;
/// - a module importing the symbol by name already binds the new name;
/// - the class (or a base or subclass) of a member already has `new_name`.
fn rename_conflicts(
    db: &Analysis,
    sym: &Symbol,
    code: &[Usage],
    new_name: &str,
    project_files: &[PathBuf],
) -> Vec<RenameConflict> {
    let inf = Inferer::new(db);
    let mut out: Vec<RenameConflict> = Vec::new();
    let mut push = |f: &FileInfo, range: TextRange, message: String| {
        if out
            .iter()
            .any(|c| c.path == f.path && c.range == range && c.message == message)
        {
            return;
        }
        let (line, column, line_text) = line_info(f, range);
        out.push(RenameConflict {
            path: f.path.clone(),
            range,
            line,
            column,
            line_text,
            message,
        });
    };
    match sym {
        Symbol::Name { def, name } => {
            // The symbol's own scope and, through imports that keep the name,
            // the scope of the binding the name comes from.
            let origin = name_origin(&inf, def);
            let mut scopes = vec![(def.file.clone(), def.binding().scope)];
            if !same_binding(&origin, def) {
                scopes.push((origin.file.clone(), origin.binding().scope));
            }
            for (f, scope) in &scopes {
                if let Some(id) = f
                    .scopes
                    .bindings_in(*scope, new_name)
                    .iter()
                    .find(|id| f.scopes.binding(**id).is_definition())
                {
                    let other = f.scopes.binding(*id);
                    push(
                        f,
                        other.range,
                        format!("'{new_name}' is already defined in the scope of '{name}'"),
                    );
                }
            }
            // Files where the name is bound by `from m import name` (no renaming alias).
            for u in code {
                let Some(f) = db.file_for_path(&u.path) else {
                    continue;
                };
                let module = f.scopes.module_scope();
                let imported_here = f.scopes.bindings_in(module, name).iter().map(|id| f.scopes.binding(*id)).any(|b| matches!(&b.kind, BindingKind::Import(info) if info.name.as_deref() == Some(name.as_str())) && !is_renaming_alias(b));
                if imported_here && !scopes.iter().any(|(sf, _)| sf.path == f.path) {
                    if let Some(id) = f
                        .scopes
                        .bindings_in(module, new_name)
                        .iter()
                        .find(|id| f.scopes.binding(**id).is_definition())
                    {
                        push(
                            &f,
                            f.scopes.binding(*id).range,
                            format!(
                                "'{new_name}' is already defined in {}, which imports '{name}'",
                                f.path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_default()
                            ),
                        );
                    }
                    scopes.push((f.clone(), module));
                }
            }
            for (f, scope) in &scopes {
                // Name references captured by an inner binding of the new name
                // (keyword arguments, imports and `module.attr` are not looked up
                // through scopes).
                for u in code.iter().filter(|u| u.path == f.path && !u.is_definition) {
                    let Some(r) = f
                        .scopes
                        .reference_at(u.range.start())
                        .filter(|r| r.range == u.range)
                    else {
                        continue;
                    };
                    if !reaches_scope(f, r.scope, *scope) {
                        continue;
                    }
                    if let Some(id) = binding_before(f, r.scope, *scope, new_name) {
                        let inner = f.scopes.binding(id);
                        push(f, u.range, format!("This usage of '{name}' would refer to the '{new_name}' defined on line {}", line_info(f, inner.range).0 + 1));
                    }
                }
                // Outer or builtin names the renamed symbol would shadow (a
                // `new_name` of the same scope is reported as a merge above).
                if f.scopes
                    .bindings_in(*scope, new_name)
                    .iter()
                    .any(|id| f.scopes.binding(*id).is_definition())
                {
                    continue;
                }
                for r in f
                    .scopes
                    .references
                    .iter()
                    .filter(|r| r.name.as_str() == new_name)
                {
                    if reaches_scope(f, r.scope, *scope)
                        && binding_before(f, r.scope, *scope, new_name).is_none()
                    {
                        push(f, r.range, format!("'{new_name}' here would refer to the renamed '{name}' instead of the current '{new_name}'"));
                    }
                }
            }
        }
        Symbol::Member { owner, name, .. } => {
            let Some(cls) = owner.class_ref() else {
                return out;
            };
            let mut classes: Vec<rustpy_semantic::types::ClassRef> =
                inf.mro(cls).iter().cloned().collect();
            if !classes.contains(cls) {
                classes.insert(0, cls.clone());
            }
            // Subclasses in files that use the member.
            let mut seen = std::collections::HashSet::new();
            for path in code
                .iter()
                .map(|u| u.path.clone())
                .chain(project_files.iter().cloned())
            {
                if !seen.insert(path.clone())
                    || path.extension().and_then(|e| e.to_str()) != Some("py")
                {
                    continue;
                }
                let Some(f) = db.file_for_path(&path) else {
                    continue;
                };
                if !f.text.contains(new_name) || is_library_file(db, &f) {
                    continue;
                }
                for s in f
                    .scopes
                    .scopes
                    .iter()
                    .filter(|s| s.kind == ScopeKind::Class)
                {
                    let c = rustpy_semantic::types::ClassRef {
                        file: f.id,
                        range: s.range,
                        name: s.name.clone().unwrap_or_default(),
                    };
                    if !classes.contains(&c) && inf.mro(&c).iter().any(|b| b == cls) {
                        classes.push(c);
                    }
                }
            }
            for c in &classes {
                let Some(info) = inf.class_info(c) else {
                    continue;
                };
                let Some(m) = info.members.get(new_name) else {
                    continue;
                };
                let Some(f) = db.file(m.file) else { continue };
                // Overriding a dunder method of `object` or another library base is intended.
                if new_name.starts_with("__") && *c != *cls && is_library_file(db, &f) {
                    continue;
                }
                push(&f, m.range, format!("Class '{}' already has a member '{new_name}' (renaming '{name}' would merge or override it)", c.name));
            }
        }
        Symbol::Keyword { .. } => {}
    }
    out
}

/// The first binding of `name` met walking from `from` towards `stop`
/// (exclusive), with Python's rule that class bodies are not enclosing
/// scopes for nested functions.
fn binding_before(
    f: &FileInfo,
    from: rustpy_semantic::scope::ScopeId,
    stop: rustpy_semantic::scope::ScopeId,
    name: &str,
) -> Option<rustpy_semantic::scope::BindingId> {
    let mut cur = Some(from);
    let mut first = true;
    while let Some(id) = cur {
        if id == stop {
            return None;
        }
        let s = f.scopes.scope(id);
        if !(s.kind == ScopeKind::Class && !first) {
            if let Some(b) = f
                .scopes
                .bindings_in(id, name)
                .iter()
                .find(|b| f.scopes.binding(**b).is_definition())
            {
                return Some(*b);
            }
        }
        first = false;
        cur = s.parent;
    }
    None
}

/// Whether name lookup from `from` passes through `target` (a class body
/// is only seen from directly inside it).
fn reaches_scope(
    f: &FileInfo,
    from: rustpy_semantic::scope::ScopeId,
    target: rustpy_semantic::scope::ScopeId,
) -> bool {
    let mut cur = Some(from);
    let mut first = true;
    while let Some(id) = cur {
        let s = f.scopes.scope(id);
        if id == target {
            return first || s.kind != ScopeKind::Class;
        }
        first = false;
        cur = s.parent;
    }
    false
}

/// Applies rename edits computed for `old_name` to `text` (the current
/// contents of a file that is not open), refusing when any edited range no
/// longer holds `old_name`: the file changed since it was analysed, and
/// applying the offsets anyway would corrupt it.
pub fn apply_rename_edits(
    text: &str,
    edits: &[(TextRange, String)],
    old_name: &str,
) -> Result<String, String> {
    for (r, _) in edits {
        if text.get(usize::from(r.start())..usize::from(r.end())) != Some(old_name) {
            return Err(format!(
                "the file changed since it was analysed (expected '{old_name}' at offset {})",
                usize::from(r.start())
            ));
        }
    }
    rustpy_semantic::edit_helpers::try_apply_edits(text, edits)
}

#[cfg(test)]
#[path = "rename_tests.rs"]
mod tests;
