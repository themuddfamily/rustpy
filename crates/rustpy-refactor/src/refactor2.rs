//! Refactorings, batch 2 (roadmap §2.9): class hierarchy refactorings
//! (pull members up / push down, extract superclass, make abstract /
//! implement abstract methods), module-level file operations (convert to
//! package / module, move symbol, rename module), change-signature extras
//! (keyword-only / positional-only, rename parameter, set default) and type
//! related transformations (type aliases, dataclass conversion, type hints).
//!
//! Single-file operations return [`Refactoring`] (edits over the current file
//! text); multi-file ones return [`MultiFileRefactoring`] or a list of
//! [`FileOp`]s that the UI applies (edits before moves/deletes).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustpy_core::{TextRange, TextSize};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::{SyntaxKind, SyntaxNode, SyntaxToken};

use crate::refactor::{expr_at, Refactoring};
use rustpy_semantic::db::{Analysis, FileInfo};
use rustpy_semantic::edit_helpers::{
    after_last_line, ancestor_at, deletion_range, element_with_comma, indent_at, is_identifier,
    line_end, line_start, next_line_start, node_with_range, normalize, reindent, rng, slice,
    stmt_lines, token_at, Edit,
};
use rustpy_semantic::hierarchy;
use rustpy_semantic::infer::Inferer;
use rustpy_semantic::scope::{BindingKind, ImportInfo};
use rustpy_semantic::types::{ClassRef, Type};

/// A refactoring touching several files: edits per path (non-overlapping within a file).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultiFileRefactoring {
    pub title: String,
    pub changes: Vec<(PathBuf, Vec<Edit>)>,
}

/// A planned file-system / text operation. The UI applies `Edit`s first
/// (they refer to the current file contents), then `Create`, `Move` and
/// `Delete` in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileOp {
    Create(PathBuf, String),
    Delete(PathBuf),
    Move(PathBuf, PathBuf),
    Edit(PathBuf, Vec<Edit>),
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// `(offset, len)` of an introduced name inside an edit's replacement text.
type Mark = (usize, usize);

/// Edit accumulator remembering where introduced names end up in the result.
#[derive(Default)]
struct Edits {
    items: Vec<(TextRange, String, Vec<Mark>)>,
}

impl Edits {
    fn add(&mut self, range: TextRange, text: impl Into<String>) {
        self.items.push((range, text.into(), Vec::new()));
    }
    fn add_marked(&mut self, range: TextRange, text: impl Into<String>, marks: Vec<Mark>) {
        self.items.push((range, text.into(), marks));
    }
    fn finish(self) -> (Vec<Edit>, Vec<TextRange>) {
        let mut items = self.items;
        items.sort_by_key(|(r, ..)| (r.start(), r.end()));
        let mut delta: i64 = 0;
        let mut marks = Vec::new();
        for (r, t, ms) in &items {
            let start = usize::from(r.start()) as i64 + delta;
            for (rel, len) in ms {
                let s = (start + *rel as i64).max(0) as usize;
                marks.push(rng(s, s + len));
            }
            delta += t.len() as i64 - u32::from(r.len()) as i64;
        }
        (items.into_iter().map(|(r, t, _)| (r, t)).collect(), marks)
    }
}

fn push_change(changes: &mut Vec<(PathBuf, Vec<Edit>)>, path: &Path, edits: Vec<Edit>) {
    if edits.is_empty() {
        return;
    }
    match changes.iter_mut().find(|(p, _)| p == path) {
        Some((_, v)) => v.extend(edits),
        None => changes.push((path.to_path_buf(), edits)),
    }
}

fn class_at(file: &FileInfo, offset: TextSize) -> Result<ast::ClassDef, String> {
    ancestor_at::<ast::ClassDef>(&file.parse.syntax_node(), offset)
        .ok_or_else(|| "Place the caret on a class".to_string())
}

fn func_at(file: &FileInfo, offset: TextSize) -> Result<ast::FuncDef, String> {
    ancestor_at::<ast::FuncDef>(&file.parse.syntax_node(), offset)
        .ok_or_else(|| "Place the caret on a function".to_string())
}

fn class_ref_of(file: &FileInfo, class: &ast::ClassDef) -> ClassRef {
    ClassRef {
        file: file.id,
        range: class.syntax().text_range(),
        name: class.name_text().into(),
    }
}

fn class_def_of(db: &Analysis, c: &ClassRef) -> Option<(Arc<FileInfo>, ast::ClassDef)> {
    let f = db.file(c.file)?;
    let def = node_with_range(&f.parse.syntax_node(), c.range).and_then(ast::ClassDef::cast)?;
    Some((f, def))
}

/// Files of the project plus open files (loading project files on demand).
pub(crate) fn load_files(db: &Analysis, project_files: &[PathBuf]) -> Vec<Arc<FileInfo>> {
    let mut files: Vec<Arc<FileInfo>> = Vec::new();
    for p in project_files {
        if let Some(f) = db.file_for_path(p) {
            if !files.iter().any(|x| x.id == f.id) {
                files.push(f);
            }
        }
    }
    for f in db.open_files() {
        if !files.iter().any(|x| x.id == f.id) && !f.path.starts_with("<typeshed>") {
            files.push(f);
        }
    }
    files
}

pub(crate) fn body_statements(class: &ast::ClassDef) -> Vec<ast::Stmt> {
    class
        .body()
        .map(|b| b.statements().collect())
        .unwrap_or_default()
}

fn is_docstring_stmt(s: &ast::Stmt) -> bool {
    matches!(s, ast::Stmt::Expr(e) if matches!(e.expr(), Some(ast::Expr::String(_)) | Some(ast::Expr::ConcatString(_))))
}

fn stmt_defines(s: &ast::Stmt, name: &str) -> bool {
    match s {
        ast::Stmt::FuncDef(f) => f.name_text() == name,
        ast::Stmt::ClassDef(c) => c.name_text() == name,
        ast::Stmt::Assign(a) => a
            .targets()
            .iter()
            .any(|t| matches!(t, ast::Expr::Name(n) if n.name_text() == name)),
        ast::Stmt::AnnAssign(a) => {
            matches!(a.target(), Some(ast::Expr::Name(n)) if n.name_text() == name)
        }
        _ => false,
    }
}

pub(crate) fn defined_names(class: &ast::ClassDef) -> HashSet<String> {
    let mut out = HashSet::new();
    for s in body_statements(class) {
        match &s {
            ast::Stmt::FuncDef(f) => {
                out.insert(f.name_text());
            }
            ast::Stmt::ClassDef(c) => {
                out.insert(c.name_text());
            }
            ast::Stmt::Assign(a) => {
                for t in a.targets() {
                    if let ast::Expr::Name(n) = t {
                        out.insert(n.name_text());
                    }
                }
            }
            ast::Stmt::AnnAssign(a) => {
                if let Some(ast::Expr::Name(n)) = a.target() {
                    out.insert(n.name_text());
                }
            }
            _ => {}
        }
    }
    out
}

/// The statements defining `names` in the class body, in body order.
pub(crate) fn member_statements(
    class: &ast::ClassDef,
    names: &[String],
) -> Result<Vec<ast::Stmt>, String> {
    let body = body_statements(class);
    let mut out = Vec::new();
    for name in names {
        let found = body
            .iter()
            .filter(|s| stmt_defines(s, name))
            .cloned()
            .collect::<Vec<_>>();
        if found.is_empty() {
            return Err(format!("'{}' has no member '{name}'", class.name_text()));
        }
        out.extend(found);
    }
    out.sort_by_key(|s| s.syntax().text_range().start());
    out.dedup_by_key(|s| s.syntax().text_range().start());
    Ok(out)
}

fn class_indent(text: &str, class: &ast::ClassDef) -> String {
    indent_at(text, class.syntax().text_range().start().into())
}

pub(crate) fn class_body_indent(text: &str, class: &ast::ClassDef) -> String {
    match body_statements(class).first() {
        Some(s) => indent_at(text, s.syntax().text_range().start().into()),
        None => format!("{}    ", class_indent(text, class)),
    }
}

/// Source lines of a member statement (decorators, docstring and body
/// included), re-indented from `from` to `to`.
fn member_text(text: &str, stmt: &ast::Stmt, from: &str, to: &str) -> String {
    let lines = stmt_lines(text, stmt.syntax().text_range());
    reindent(slice(text, lines), from, to)
}

fn is_def(stmt: &ast::Stmt) -> bool {
    matches!(stmt, ast::Stmt::FuncDef(_) | ast::Stmt::ClassDef(_))
}

/// Edits inserting `members` (already at the class' body indentation) into
/// `class`: attributes before the first method, definitions at the end
/// (a `pass`-only body is replaced).
pub(crate) fn insert_members(
    text: &str,
    class: &ast::ClassDef,
    members: &[(bool, String)],
) -> Vec<Edit> {
    let mut edits = Vec::new();
    if members.is_empty() {
        return edits;
    }
    let body = body_statements(class);
    let real: Vec<&ast::Stmt> = body.iter().filter(|s| !is_docstring_stmt(s)).collect();
    let only_pass = real.len() == 1 && matches!(real[0], ast::Stmt::Pass(_));
    let attrs: String = members
        .iter()
        .filter(|(d, _)| !d)
        .map(|(_, t)| t.as_str())
        .collect();
    let defs: Vec<&str> = members
        .iter()
        .filter(|(d, _)| *d)
        .map(|(_, t)| t.as_str())
        .collect();
    if only_pass || real.is_empty() {
        let mut snippet = attrs.clone();
        for d in &defs {
            if !snippet.is_empty() {
                snippet.push('\n');
            }
            snippet.push_str(d);
        }
        match real.first() {
            Some(p) => edits.push((stmt_lines(text, p.syntax().text_range()), snippet)),
            None => {
                let at = after_last_line(text, class.syntax().text_range().end().into());
                edits.push((rng(at, at), snippet));
            }
        }
        return edits;
    }
    if !attrs.is_empty() {
        match real.iter().find(|s| is_def(s)) {
            Some(first_def) => {
                let at = line_start(text, first_def.syntax().text_range().start().into());
                edits.push((rng(at, at), format!("{attrs}\n")));
            }
            None => {
                let at = after_last_line(text, class.syntax().text_range().end().into());
                edits.push((rng(at, at), attrs));
            }
        }
    }
    if !defs.is_empty() {
        let at = after_last_line(text, class.syntax().text_range().end().into());
        let mut snippet = String::new();
        for d in &defs {
            snippet.push('\n');
            snippet.push_str(d);
        }
        edits.push((rng(at, at), snippet));
    }
    edits
}

/// Lines to delete for a class member: the statement itself plus the
/// blank lines that would otherwise be left dangling (after the header or
/// at the end of the body).
fn member_deletion_range(text: &str, class: &ast::ClassDef, stmt: &ast::Stmt) -> TextRange {
    let lines = stmt_lines(text, stmt.syntax().text_range());
    let mut start: usize = lines.start().into();
    let mut end: usize = lines.end().into();
    let class_end = after_last_line(text, class.syntax().text_range().end().into());
    let followed_by_code = !text[end..class_end.max(end)].trim().is_empty();
    if followed_by_code {
        let before = text[..start].trim_end();
        let after_header = before.ends_with(':');
        let preceded_by_blank = start >= 2
            && text.as_bytes()[start - 1] == b'\n'
            && text.as_bytes()[start - 2] == b'\n';
        if after_header || preceded_by_blank {
            while end < text.len() && text.as_bytes()[end] == b'\n' {
                end += 1;
            }
        }
    } else {
        // Last statement of the body: drop the blank lines before it.
        while start >= 2
            && text.as_bytes()[start - 1] == b'\n'
            && text.as_bytes()[start - 2] == b'\n'
        {
            start -= 1;
        }
    }
    rng(start, end)
}

/// Edits removing `stmts` from `class`; a body left empty gets `pass`.
pub(crate) fn remove_members(text: &str, class: &ast::ClassDef, stmts: &[ast::Stmt]) -> Vec<Edit> {
    let body = body_statements(class);
    let removed: HashSet<TextSize> = stmts
        .iter()
        .map(|s| s.syntax().text_range().start())
        .collect();
    let remaining = body
        .iter()
        .filter(|s| !removed.contains(&s.syntax().text_range().start()))
        .count();
    let mut edits: Vec<Edit> = Vec::new();
    for (i, s) in stmts.iter().enumerate() {
        let mut r = member_deletion_range(text, class, s);
        // Keep ranges disjoint when consecutive members are removed.
        if let Some((prev, _)) = edits.last() {
            if prev.end() > r.start() {
                r = TextRange::new(prev.end(), r.end().max(prev.end()));
            }
        }
        if i == 0 && remaining == 0 {
            edits.push((r, format!("{}pass\n", class_body_indent(text, class))));
        } else {
            edits.push((r, String::new()));
        }
    }
    edits
}

fn class_header_colon(class: &ast::ClassDef) -> Option<SyntaxToken> {
    class
        .syntax()
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == SyntaxKind::COLON)
}

/// Edit adding `base` to the class header (as first base when `first`).
pub(crate) fn add_base_edit(class: &ast::ClassDef, base: &str, first: bool) -> Option<Edit> {
    match class.arguments() {
        Some(args) => {
            let open = args
                .syntax()
                .first_token()
                .filter(|t| t.kind() == SyntaxKind::L_PAREN)?;
            let close = args
                .syntax()
                .last_token()
                .filter(|t| t.kind() == SyntaxKind::R_PAREN)?;
            let has_any = args.args().next().is_some();
            if first {
                let at: usize = open.text_range().end().into();
                Some((
                    rng(at, at),
                    if has_any {
                        format!("{base}, ")
                    } else {
                        base.to_string()
                    },
                ))
            } else {
                let at: usize = close.text_range().start().into();
                Some((
                    rng(at, at),
                    if has_any {
                        format!(", {base}")
                    } else {
                        base.to_string()
                    },
                ))
            }
        }
        None => {
            let colon = class_header_colon(class)?;
            let at: usize = colon.text_range().start().into();
            Some((rng(at, at), format!("({base})")))
        }
    }
}

pub(crate) fn top_level_statements(file: &FileInfo) -> Vec<ast::Stmt> {
    ast::Module::cast(file.parse.syntax_node())
        .map(|m| m.statements().collect())
        .unwrap_or_default()
}

/// Offset where a new import statement goes: after the last top-level
/// import, else after the module docstring, else at the start.
pub(crate) fn import_insert_offset(file: &FileInfo) -> usize {
    let text: &str = &file.text;
    let mut after_imports: Option<usize> = None;
    let mut after_doc: Option<usize> = None;
    for s in top_level_statements(file) {
        match &s {
            ast::Stmt::Import(_) | ast::Stmt::ImportFrom(_) => {
                after_imports = Some(next_line_start(
                    text,
                    line_end(text, s.syntax().text_range().end().into())
                        .saturating_sub(1)
                        .max(s.syntax().text_range().start().into()),
                ));
            }
            _ if is_docstring_stmt(&s) && after_doc.is_none() && after_imports.is_none() => {
                after_doc = Some(after_last_line(text, s.syntax().text_range().end().into()));
            }
            _ => {}
        }
    }
    after_imports.or(after_doc).unwrap_or(0)
}

/// Edit that makes `names` importable from `module` (merging into an
/// existing `from module import ...`), or `None` when they already are.
fn ensure_from_import(file: &FileInfo, module: &str, names: &[&str]) -> Option<Edit> {
    let text: &str = &file.text;
    for s in top_level_statements(file) {
        if let ast::Stmt::ImportFrom(imp) = &s {
            if imp.level() == 0 && imp.module_path() == module && !imp.is_star() {
                let present: HashSet<String> = imp.aliases().map(|a| a.name()).collect();
                let missing: Vec<&str> = names
                    .iter()
                    .copied()
                    .filter(|n| !present.contains(*n))
                    .collect();
                if missing.is_empty() {
                    return None;
                }
                let last = imp.aliases().last()?;
                let at: usize = last.syntax().text_range().end().into();
                return Some((rng(at, at), format!(", {}", missing.join(", "))));
            }
        }
    }
    let at = import_insert_offset(file);
    let prefix = if at > 0 && !text[..at].ends_with('\n') {
        "\n"
    } else {
        ""
    };
    let had_imports = top_level_statements(file)
        .iter()
        .any(|s| matches!(s, ast::Stmt::Import(_) | ast::Stmt::ImportFrom(_)));
    let rest = &text[at..];
    let next = rest.trim_start_matches('\n');
    let suffix = if had_imports || rest.starts_with('\n') || rest.is_empty() {
        ""
    } else if next.starts_with('@')
        || next.starts_with("class ")
        || next.starts_with("def ")
        || next.starts_with("async def ")
    {
        "\n\n"
    } else {
        "\n"
    };
    Some((
        rng(at, at),
        format!(
            "{prefix}from {module} import {}\n{suffix}",
            names.join(", ")
        ),
    ))
}

/// Dotted module name of `path` relative to the analysis roots or `root`.
pub(crate) fn module_name_for(db: &Analysis, root: &Path, path: &Path) -> Option<String> {
    if let Some(m) = db.resolver.module_name_of(path) {
        return Some(m);
    }
    let rel = path.strip_prefix(root).ok()?;
    let mut parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if let Some(last) = parts.last_mut() {
        if let Some(stem) = Path::new(last)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
        {
            *last = stem;
        }
    }
    if parts.last().map(String::as_str) == Some("__init__") {
        parts.pop();
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("."))
    }
}

/// Absolute module name of a relative import written in module `from_module`
/// (`is_package`: the importing file is an `__init__.py`).
pub(crate) fn absolute_module(
    from_module: &str,
    is_package: bool,
    level: usize,
    module: &str,
) -> String {
    let mut parts: Vec<&str> = from_module.split('.').collect();
    if !is_package {
        parts.pop();
    }
    for _ in 1..level {
        parts.pop();
    }
    let mut out: Vec<String> = parts.iter().map(|s| s.to_string()).collect();
    if !module.is_empty() {
        out.extend(module.split('.').map(|s| s.to_string()));
    }
    out.join(".")
}

fn rewrite_dotted(path: &str, old: &str, new: &str) -> Option<String> {
    if path == old {
        Some(new.to_string())
    } else {
        path.strip_prefix(old)
            .filter(|rest| rest.starts_with('.'))
            .map(|rest| format!("{new}{rest}"))
    }
}

pub(crate) fn file_text(db: &Analysis, path: &Path) -> Option<Arc<FileInfo>> {
    db.file_for_path(path)
}

// ---------------------------------------------------------------------------
// 1-3. Pull up / push down / extract superclass
// ---------------------------------------------------------------------------

/// Resolves a base of `class` by (dotted) name to its definition.
fn resolve_base(
    db: &Analysis,
    file: &Arc<FileInfo>,
    class: &ast::ClassDef,
    target_base: &str,
) -> Result<(Arc<FileInfo>, ast::ClassDef), String> {
    let inf = Inferer::new(db);
    let wanted = target_base.rsplit('.').next().unwrap_or(target_base);
    for base in class.bases() {
        let text = base.syntax().text().to_string();
        let last = text
            .trim_end_matches(']')
            .rsplit(['.', '['])
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        if last != wanted && text != target_base {
            continue;
        }
        if let Type::Class(c, _) = inf.type_of_expr(file, &base) {
            if let Some(found) = class_def_of(db, &c) {
                if found.0.path.starts_with("<typeshed>") || found.0.is_stub {
                    return Err(format!(
                        "'{wanted}' is a library class and cannot be modified"
                    ));
                }
                return Ok(found);
            }
        }
        // Fallback: a class of that name in this file.
        for s in top_level_statements(file) {
            if let ast::Stmt::ClassDef(c) = s {
                if c.name_text() == wanted {
                    return Ok((file.clone(), c));
                }
            }
        }
    }
    Err(format!(
        "'{}' has no base class '{target_base}'",
        class.name_text()
    ))
}

/// Moves `members` of the class at `class_offset` into its base `target_base`.
pub fn pull_members_up(
    db: &Analysis,
    file: &Arc<FileInfo>,
    class_offset: TextSize,
    members: &[String],
    target_base: &str,
) -> Result<MultiFileRefactoring, String> {
    let class_offset = rustpy_semantic::clamp_offset(file, class_offset);
    let text: &str = &file.text;
    let class = class_at(file, class_offset)?;
    if members.is_empty() {
        return Err("Select at least one member".into());
    }
    let (base_file, base) = resolve_base(db, file, &class, target_base)?;
    let stmts = member_statements(&class, members)?;
    let base_names = defined_names(&base);
    for m in members {
        if base_names.contains(m) {
            return Err(format!("'{}' already defines '{m}'", base.name_text()));
        }
    }
    let from = class_body_indent(text, &class);
    let to = class_body_indent(&base_file.text, &base);
    let snippets: Vec<(bool, String)> = stmts
        .iter()
        .map(|s| (is_def(s), member_text(text, s, &from, &to)))
        .collect();
    let mut changes = Vec::new();
    push_change(
        &mut changes,
        &base_file.path,
        insert_members(&base_file.text, &base, &snippets),
    );
    push_change(
        &mut changes,
        &file.path,
        remove_members(text, &class, &stmts),
    );
    Ok(MultiFileRefactoring {
        title: format!("Pull members up to '{}'", base.name_text()),
        changes,
    })
}

/// Copies `members` of the class at `class_offset` into every direct subclass
/// and removes them from the class.
pub fn push_members_down(
    db: &Analysis,
    file: &Arc<FileInfo>,
    class_offset: TextSize,
    members: &[String],
    project_files: &[PathBuf],
) -> Result<MultiFileRefactoring, String> {
    let class_offset = rustpy_semantic::clamp_offset(file, class_offset);
    let text: &str = &file.text;
    let class = class_at(file, class_offset)?;
    if members.is_empty() {
        return Err("Select at least one member".into());
    }
    let stmts = member_statements(&class, members)?;
    let files = load_files(db, project_files);
    let cref = class_ref_of(file, &class);
    let subs = hierarchy::direct_subclasses(db, &cref, &files);
    if subs.is_empty() {
        return Err(format!(
            "'{}' has no subclasses in the project",
            class.name_text()
        ));
    }
    let from = class_body_indent(text, &class);
    let mut changes = Vec::new();
    for sub in &subs {
        let Some((sub_file, sub_def)) = class_def_of(db, sub) else {
            continue;
        };
        let names = defined_names(&sub_def);
        for m in members {
            if names.contains(m) {
                return Err(format!("'{}' already defines '{m}'", sub_def.name_text()));
            }
        }
        let to = class_body_indent(&sub_file.text, &sub_def);
        let snippets: Vec<(bool, String)> = stmts
            .iter()
            .map(|s| (is_def(s), member_text(text, s, &from, &to)))
            .collect();
        push_change(
            &mut changes,
            &sub_file.path,
            insert_members(&sub_file.text, &sub_def, &snippets),
        );
    }
    push_change(
        &mut changes,
        &file.path,
        remove_members(text, &class, &stmts),
    );
    Ok(MultiFileRefactoring {
        title: format!("Push members down from '{}'", class.name_text()),
        changes,
    })
}

/// Creates `class new_name` above the class at `class_offset` holding
/// `members`, and makes the class inherit from it.
pub fn extract_superclass(
    db: &Analysis,
    file: &Arc<FileInfo>,
    class_offset: TextSize,
    new_name: &str,
    members: &[String],
) -> Result<Refactoring, String> {
    let class_offset = rustpy_semantic::clamp_offset(file, class_offset);
    let _ = db;
    let text: &str = &file.text;
    if !is_identifier(new_name) {
        return Err(format!("'{new_name}' is not a valid class name"));
    }
    let class = class_at(file, class_offset)?;
    if class.name_text() == new_name
        || file
            .scopes
            .resolve(file.scopes.module_scope(), new_name)
            .is_some()
    {
        return Err(format!("'{new_name}' is already defined"));
    }
    let stmts = member_statements(&class, members)?;
    let indent = class_indent(text, &class);
    let from = class_body_indent(text, &class);
    let to = format!("{indent}    ");
    let mut body = String::new();
    for (i, s) in stmts.iter().enumerate() {
        if i > 0 && is_def(s) {
            body.push('\n');
        }
        body.push_str(&member_text(text, s, &from, &to));
    }
    if body.is_empty() {
        body = format!("{to}pass\n");
    }
    let sep = if indent.is_empty() { "\n\n" } else { "\n" };
    let header = format!("{indent}class {new_name}:\n");
    let mut ed = Edits::default();
    let at = line_start(text, class.syntax().text_range().start().into());
    ed.add_marked(
        rng(at, at),
        format!("{header}{body}{sep}"),
        vec![(indent.len() + "class ".len(), new_name.len())],
    );
    let (brange, btext) = add_base_edit(&class, new_name, true).ok_or("Malformed class header")?;
    let mark_at = if btext.starts_with('(') { 1 } else { 0 };
    ed.add_marked(brange, btext, vec![(mark_at, new_name.len())]);
    for e in remove_members(text, &class, &stmts) {
        ed.add(e.0, e.1);
    }
    let (edits, rename_ranges) = ed.finish();
    Ok(Refactoring {
        title: format!("Extract superclass '{new_name}'"),
        edits,
        rename_ranges,
        name: new_name.to_string(),
    })
}

// ---------------------------------------------------------------------------
// 4. Abstract methods
// ---------------------------------------------------------------------------

/// A method definition in a base class: its file and `def` node.
type BaseDef = (Arc<FileInfo>, ast::FuncDef);

fn is_abstract(func: &ast::FuncDef) -> bool {
    func.has_decorator("abstractmethod") || func.has_decorator("abc.abstractmethod")
}

/// Marks the method at `method_offset` abstract (`@abstractmethod`, `ABC`
/// base, `abc` import).
pub fn make_abstract(
    db: &Analysis,
    file: &Arc<FileInfo>,
    method_offset: TextSize,
) -> Result<Refactoring, String> {
    let method_offset = rustpy_semantic::clamp_offset(file, method_offset);
    let _ = db;
    let text: &str = &file.text;
    let func = func_at(file, method_offset)?;
    if !func.is_method() {
        return Err("Only methods can be made abstract".into());
    }
    if is_abstract(&func) {
        return Err(format!("'{}' is already abstract", func.name_text()));
    }
    let class = func
        .syntax()
        .ancestors()
        .skip(1)
        .find_map(ast::ClassDef::cast)
        .ok_or("Not inside a class")?;
    let mut ed = Edits::default();
    let def_tok = func.def_token().ok_or("Malformed function")?;
    let at = line_start(text, def_tok.text_range().start().into());
    let ind = indent_at(text, def_tok.text_range().start().into());
    ed.add(rng(at, at), format!("{ind}@abstractmethod\n"));
    let has_abc_base = class.bases().iter().any(|b| {
        let t = b.syntax().text().to_string();
        t == "ABC" || t.ends_with(".ABC") || t == "ABCMeta"
    });
    let has_metaclass = class
        .arguments()
        .is_some_and(|a| a.keywords().any(|k| k.name_text() == "metaclass"));
    if !has_abc_base && !has_metaclass {
        if let Some((r, t)) = add_base_edit(&class, "ABC", false) {
            ed.add(r, t);
        }
    }
    if let Some((r, t)) = ensure_from_import(file, "abc", &["ABC", "abstractmethod"]) {
        ed.add(r, t);
    }
    let (edits, _) = ed.finish();
    Ok(Refactoring {
        title: format!("Make '{}' abstract", func.name_text()),
        edits,
        rename_ranges: Vec::new(),
        name: func.name_text(),
    })
}

/// Adds stubs for abstract methods of the bases that the class at
/// `class_offset` does not implement.
pub fn implement_abstract_methods(
    db: &Analysis,
    file: &Arc<FileInfo>,
    class_offset: TextSize,
) -> Result<Refactoring, String> {
    let class_offset = rustpy_semantic::clamp_offset(file, class_offset);
    let text: &str = &file.text;
    let class = class_at(file, class_offset)?;
    let inf = Inferer::new(db);
    let cref = class_ref_of(file, &class);
    let mro = inf.mro(&cref);
    // First definition of every member along the MRO (own class included).
    let mut first: BTreeMap<String, (bool, Option<BaseDef>)> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    for (i, c) in mro.iter().enumerate() {
        let Some(info) = inf.class_info(c) else {
            continue;
        };
        let Some(cfile) = db.file(c.file) else {
            continue;
        };
        let croot = cfile.parse.syntax_node();
        for (name, m) in info.members.iter() {
            if first.contains_key(name.as_str()) {
                continue;
            }
            let def = token_at(&croot, m.range.start())
                .and_then(|t| t.parent_ancestors().find_map(ast::FuncDef::cast))
                .filter(|f| f.name_text() == name.as_str());
            let abstract_ = def.as_ref().is_some_and(is_abstract);
            if i == 0 || def.is_none() {
                first.insert(name.to_string(), (false, None));
            } else {
                first.insert(
                    name.to_string(),
                    (abstract_, def.map(|d| (cfile.clone(), d))),
                );
                order.push(name.to_string());
            }
        }
    }
    let own = defined_names(&class);
    let mut snippets: Vec<(bool, String)> = Vec::new();
    let ind = class_body_indent(text, &class);
    for name in order {
        let Some((true, Some((bfile, def)))) = first.get(&name) else {
            continue;
        };
        if own.contains(&name) {
            continue;
        }
        let btext: &str = &bfile.text;
        let mut s = String::new();
        for d in def.decorators() {
            let dname = d.name_text().unwrap_or_default();
            if dname.ends_with("abstractmethod") {
                continue;
            }
            s.push_str(&format!(
                "{ind}@{}\n",
                slice(btext, d.syntax().text_range()).trim_start_matches('@')
            ));
        }
        let params = def
            .params()
            .map(|p| p.syntax().text().to_string())
            .unwrap_or_else(|| "(self)".into());
        let ret = def
            .return_annotation()
            .map(|r| format!(" -> {}", r.syntax().text()))
            .unwrap_or_default();
        let asyn = if def.is_async() { "async " } else { "" };
        s.push_str(&format!(
            "{ind}{asyn}def {name}{params}{ret}:\n{ind}    raise NotImplementedError\n"
        ));
        snippets.push((true, s));
    }
    if snippets.is_empty() {
        return Err(format!(
            "'{}' implements every abstract method of its bases",
            class.name_text()
        ));
    }
    let n = snippets.len();
    let edits = insert_members(text, &class, &snippets);
    Ok(Refactoring {
        title: format!("Implement {n} abstract method(s)"),
        edits,
        rename_ranges: Vec::new(),
        name: class.name_text(),
    })
}

// ---------------------------------------------------------------------------
// 5-7. File operations: convert, move symbol, rename module
// ---------------------------------------------------------------------------

/// Plans turning `module.py` into `module/__init__.py`. Imports keep working
/// (`import pkg.module` / `from pkg import module` resolve to the package).
pub fn convert_to_package(file_path: &Path) -> Result<Vec<FileOp>, String> {
    let name = file_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("Not a file")?;
    if file_path.extension().and_then(|e| e.to_str()) != Some("py") {
        return Err("Only .py modules can be converted to packages".into());
    }
    if name == "__init__.py" {
        return Err("This file already is a package".into());
    }
    let dir = file_path.with_extension("");
    if dir.exists() {
        return Err(format!("{} already exists", dir.display()));
    }
    Ok(vec![FileOp::Move(
        file_path.to_path_buf(),
        dir.join("__init__.py"),
    )])
}

/// Plans turning `package/__init__.py` (a package with no other modules)
/// back into `package.py`.
pub fn convert_to_module(package_dir: &Path) -> Result<Vec<FileOp>, String> {
    let init = package_dir.join("__init__.py");
    if !init.is_file() {
        return Err(format!("{} is not a package", package_dir.display()));
    }
    let others: Vec<PathBuf> = std::fs::read_dir(package_dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()) != Some("__init__.py")
                && p.file_name().and_then(|n| n.to_str()) != Some("__pycache__")
        })
        .collect();
    if !others.is_empty() {
        return Err(format!(
            "The package contains other modules ({}); move them first",
            others
                .iter()
                .filter_map(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let target = package_dir.with_extension("py");
    if target.exists() {
        return Err(format!("{} already exists", target.display()));
    }
    Ok(vec![
        FileOp::Move(init, target),
        FileOp::Delete(package_dir.to_path_buf()),
    ])
}

pub(crate) fn import_statement_for(
    info: &ImportInfo,
    from_module: &str,
    is_package: bool,
) -> String {
    let module = if info.level > 0 {
        absolute_module(from_module, is_package, info.level, &info.module)
    } else {
        info.module.to_string()
    };
    let alias = info
        .alias
        .as_ref()
        .map(|a| format!(" as {a}"))
        .unwrap_or_default();
    match &info.name {
        Some(name) => format!("from {module} import {name}{alias}"),
        None => format!("import {module}{alias}"),
    }
}

pub(crate) fn top_level_def_at(
    file: &FileInfo,
    offset: TextSize,
) -> Result<(SyntaxNode, String), String> {
    let root = file.parse.syntax_node();
    let tok = token_at(&root, offset).ok_or("Nothing here")?;
    let def = tok
        .parent_ancestors()
        .filter(|node| {
            matches!(
                node.kind(),
                SyntaxKind::FUNC_DEF
                    | SyntaxKind::CLASS_DEF
                    | SyntaxKind::ASSIGN_STMT
                    | SyntaxKind::ANN_ASSIGN_STMT
            )
        })
        .last()
        .ok_or("Place the caret on a top-level function, class or variable")?;
    if def.parent().map(|parent| parent.kind()) != Some(SyntaxKind::MODULE) {
        return Err("Only top-level declarations can move to another module".into());
    }
    let name=match def.kind() {
        SyntaxKind::FUNC_DEF=>ast::FuncDef::cast(def.clone()).map(|function|function.name_text()),
        SyntaxKind::CLASS_DEF=>ast::ClassDef::cast(def.clone()).map(|class|class.name_text()),
        SyntaxKind::ASSIGN_STMT=>ast::AssignStmt::cast(def.clone()).and_then(|assignment|{let targets=assignment.targets();if targets.len()==1 {if let ast::Expr::Name(name)=&targets[0]{Some(name.name_text())}else{None}}else{None}}),
        SyntaxKind::ANN_ASSIGN_STMT=>ast::AnnAssignStmt::cast(def.clone()).and_then(|assignment|assignment.target()).and_then(|target|if let ast::Expr::Name(name)=target{Some(name.name_text())}else{None}),
        _=>None,
    }.filter(|name|!name.is_empty()).ok_or("Select one named top-level declaration; destructuring/chained assignments must be split first")?;
    Ok((def, name))
}

/// Moves the top-level function/class at `offset` to `target_module_path`,
/// carrying the imports it needs and updating `from source import name`
/// statements across `project_files`.
pub fn move_symbol(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    target_module_path: &Path,
    root: &Path,
    project_files: &[PathBuf],
) -> Result<Vec<FileOp>, String> {
    move_symbol_with_import_style(
        db,
        file,
        offset,
        target_module_path,
        root,
        project_files,
        true,
    )
}

/// The preference changes newly generated imports. Authored imports carried
/// with a definition, and existing import statements being retargeted, retain
/// their form. Qualified imports use fresh aliases and resolved references.
pub fn move_symbol_with_import_style(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
    target_module_path: &Path,
    root: &Path,
    project_files: &[PathBuf],
    prefer_from_imports: bool,
) -> Result<Vec<FileOp>, String> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let text: &str = &file.text;
    let (def, name) = top_level_def_at(file, offset)?;
    if target_module_path == file.path {
        return Err("The target is the current module".into());
    }
    let source_module = module_name_for(db, root, &file.path)
        .ok_or("Cannot determine the module name of the current file")?;
    let target_module = module_name_for(db, root, target_module_path)
        .ok_or("Cannot determine the module name of the target file")?;
    let def_range = def.text_range();
    let module_scope = file.scopes.module_scope();
    let own_binding = file
        .scopes
        .binding_at(
            def.children_with_tokens()
                .filter_map(|e| e.into_token())
                .find(|t| t.kind() == SyntaxKind::IDENT)
                .map(|t| t.text_range().start())
                .unwrap_or(def_range.start()),
        )
        .map(|(id, _)| id);
    // Imports / module-level names the symbol depends on.
    let is_package = file.is_package_init();
    let mut needed: BTreeSet<String> = BTreeSet::new();
    let mut reserved = move_reserved_names(file);
    if let Some(target) = file_text(db, target_module_path) {
        reserved.extend(move_reserved_names(&target));
    }
    let mut module_aliases = BTreeMap::new();
    let mut symbol_edits = Vec::new();
    for r in file
        .scopes
        .references
        .iter()
        .filter(|r| def_range.contains_range(r.range))
    {
        let Some(id) = r.resolved else { continue };
        let b = file.scopes.binding(id);
        if b.scope != module_scope || b.name.as_str() == name || def_range.contains_range(b.range) {
            continue;
        }
        match &b.kind {
            BindingKind::Import(info) => {
                needed.insert(import_statement_for(info, &source_module, is_package));
            }
            BindingKind::Function
            | BindingKind::Class
            | BindingKind::Assignment
            | BindingKind::Annotation
            | BindingKind::TypeAlias => {
                if prefer_from_imports
                    || file.scopes.references_to(id).iter().any(|reference| {
                        reference.is_write && def_range.contains_range(reference.range)
                    })
                {
                    needed.insert(format!("from {source_module} import {}", b.name));
                } else {
                    let qualifier = move_module_import(
                        &source_module,
                        &mut reserved,
                        &mut needed,
                        &mut module_aliases,
                    );
                    symbol_edits.push((r.range, format!("{qualifier}.{}", b.name)));
                }
            }
            _ => {}
        }
    }
    let mut ops = Vec::new();
    // Source file: delete the definition; re-import it when still used here.
    let mut src_edits: Vec<Edit> = vec![(deletion_range(text, def_range), String::new())];
    let still_used = own_binding.is_some_and(|id| {
        file.scopes
            .references_to(id)
            .iter()
            .any(|r| !def_range.contains_range(r.range))
    });
    if still_used {
        let at = import_insert_offset(file);
        let references = own_binding
            .map(|id| file.scopes.references_to(id))
            .unwrap_or_default();
        if prefer_from_imports
            || references
                .iter()
                .any(|reference| reference.is_write && !def_range.contains_range(reference.range))
        {
            src_edits.push((rng(at, at), format!("from {target_module} import {name}\n")));
        } else {
            let mut imports = BTreeSet::new();
            let qualifier = move_module_import(
                &target_module,
                &mut reserved,
                &mut imports,
                &mut BTreeMap::new(),
            );
            src_edits.push((
                rng(at, at),
                imports
                    .into_iter()
                    .map(|import| format!("{import}\n"))
                    .collect(),
            ));
            src_edits.extend(
                references
                    .iter()
                    .filter(|reference| !def_range.contains_range(reference.range))
                    .map(|reference| (reference.range, format!("{qualifier}.{name}"))),
            );
        }
    }
    ops.push(FileOp::Edit(file.path.clone(), src_edits));
    // Target file.
    let symbol_range = stmt_lines(text, def_range);
    let local_edits: Vec<_> = symbol_edits
        .into_iter()
        .map(|(range, replacement)| {
            (
                TextRange::new(
                    range.start() - symbol_range.start(),
                    range.end() - symbol_range.start(),
                ),
                replacement,
            )
        })
        .collect();
    let symbol_text =
        rustpy_semantic::edit_helpers::try_apply_edits(slice(text, symbol_range), &local_edits)
            .map_err(|error| error.to_string())?;
    match file_text(db, target_module_path) {
        Some(target) => {
            let ttext: &str = &target.text;
            if target
                .scopes
                .resolve(target.scopes.module_scope(), &name)
                .is_some()
            {
                return Err(format!(
                    "'{name}' is already defined in {}",
                    target_module_path.display()
                ));
            }
            let mut tedits: Vec<Edit> = Vec::new();
            let existing: HashSet<String> = top_level_statements(&target)
                .iter()
                .filter(|s| matches!(s, ast::Stmt::Import(_) | ast::Stmt::ImportFrom(_)))
                .map(|s| normalize(&s.syntax().text().to_string()))
                .collect();
            let imports: Vec<&String> = needed
                .iter()
                .filter(|i| !existing.contains(&normalize(i)))
                .collect();
            if !imports.is_empty() {
                let at = import_insert_offset(&target);
                tedits.push((
                    rng(at, at),
                    imports.iter().map(|i| format!("{i}\n")).collect::<String>(),
                ));
            }
            let end = ttext.len();
            let mut tail = String::new();
            if !ttext.is_empty() && !ttext.ends_with('\n') {
                tail.push('\n');
            }
            if !ttext.trim().is_empty() {
                tail.push_str("\n\n");
            }
            tail.push_str(&symbol_text);
            tedits.push((rng(end, end), tail));
            ops.push(FileOp::Edit(target_module_path.to_path_buf(), tedits));
        }
        None => {
            let mut content = needed.iter().map(|i| format!("{i}\n")).collect::<String>();
            if !content.is_empty() {
                content.push_str("\n\n");
            }
            content.push_str(&symbol_text);
            ops.push(FileOp::Create(target_module_path.to_path_buf(), content));
        }
    }
    // Other files importing the symbol from the source module.
    for other in load_files(db, project_files) {
        if other.path == file.path || other.path == target_module_path {
            continue;
        }
        let otext: &str = &other.text;
        let mut edits: Vec<Edit> = Vec::new();
        for s in top_level_statements(&other) {
            let ast::Stmt::ImportFrom(imp) = s else {
                continue;
            };
            if imp.is_star() {
                continue;
            }
            let module = if imp.level() > 0 {
                module_name_for(db, root, &other.path)
                    .map(|m| {
                        absolute_module(
                            &m,
                            other.is_package_init(),
                            imp.level(),
                            &imp.module().map(|d| d.path()).unwrap_or_default(),
                        )
                    })
                    .unwrap_or_default()
            } else {
                imp.module_path()
            };
            if module != source_module {
                continue;
            }
            let aliases: Vec<ast::ImportAlias> = imp.aliases().collect();
            let Some(alias) = aliases.iter().find(|a| a.name() == name) else {
                continue;
            };
            if aliases.len() == 1 {
                let Some(m) = imp.module() else { continue };
                let r = if imp.level() > 0 {
                    TextRange::new(
                        imp.syntax()
                            .children_with_tokens()
                            .find(|e| e.kind() == SyntaxKind::RELATIVE_LEVEL)
                            .map(|e| e.text_range().start())
                            .unwrap_or(m.syntax().text_range().start()),
                        m.syntax().text_range().end(),
                    )
                } else {
                    m.syntax().text_range()
                };
                edits.push((r, target_module.clone()));
            } else {
                edits.push((element_with_comma(otext, alias.syntax()), String::new()));
                let at = next_line_start(otext, imp.syntax().text_range().end().into());
                let as_alias = alias
                    .alias()
                    .map(|a| format!(" as {}", a.text()))
                    .unwrap_or_default();
                let binding = other
                    .scopes
                    .bindings
                    .iter()
                    .enumerate()
                    .find(|(_, binding)| alias.syntax().text_range().contains_range(binding.range));
                let references = binding
                    .map(|(index, _)| {
                        other
                            .scopes
                            .references_to(rustpy_semantic::scope::BindingId(index as u32))
                    })
                    .unwrap_or_default();
                if !prefer_from_imports
                    && binding.is_some()
                    && !references.iter().any(|reference| reference.is_write)
                {
                    let mut used = move_reserved_names(&other);
                    let mut imports = BTreeSet::new();
                    let qualifier = move_module_import(
                        &target_module,
                        &mut used,
                        &mut imports,
                        &mut BTreeMap::new(),
                    );
                    edits.push((
                        rng(at, at),
                        imports
                            .into_iter()
                            .map(|import| format!("{import}\n"))
                            .collect(),
                    ));
                    edits.extend(
                        references
                            .into_iter()
                            .map(|reference| (reference.range, format!("{qualifier}.{name}"))),
                    );
                } else {
                    edits.push((
                        rng(at, at),
                        format!("from {target_module} import {name}{as_alias}\n"),
                    ));
                }
            }
        }
        if !edits.is_empty() {
            ops.push(FileOp::Edit(other.path.clone(), edits));
        }
    }
    Ok(ops)
}

fn move_reserved_names(file: &FileInfo) -> HashSet<String> {
    file.parse
        .syntax_node()
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| token.kind() == SyntaxKind::IDENT)
        .map(|token| token.text().to_string())
        .chain(
            file.scopes
                .bindings
                .iter()
                .map(|binding| binding.name.to_string()),
        )
        .collect()
}

fn move_module_import(
    module: &str,
    used: &mut HashSet<String>,
    imports: &mut BTreeSet<String>,
    aliases: &mut BTreeMap<String, String>,
) -> String {
    if let Some(alias) = aliases.get(module) {
        return alias.clone();
    }
    let base = module
        .rsplit('.')
        .next()
        .filter(|name| is_identifier(name))
        .unwrap_or("moved_module");
    let mut alias = base.to_string();
    let mut suffix = 2usize;
    while !used.insert(alias.clone()) {
        alias = format!("{base}_{suffix}");
        suffix += 1;
    }
    imports.insert(if alias == module {
        format!("import {module}")
    } else {
        format!("import {module} as {alias}")
    });
    aliases.insert(module.to_string(), alias.clone());
    alias
}

/// Renames the module or package at `old_path` to `new_name`, rewriting
/// `import` / `from ... import` statements across `project_files`.
pub fn rename_module(
    db: &Analysis,
    root: &Path,
    old_path: &Path,
    new_name: &str,
    project_files: &[PathBuf],
) -> Result<Vec<FileOp>, String> {
    if !is_identifier(new_name) {
        return Err(format!("'{new_name}' is not a valid module name"));
    }
    let is_init = old_path.file_name().and_then(|n| n.to_str()) == Some("__init__.py");
    let is_package = old_path.is_dir() || is_init;
    let package_dir = if is_init {
        old_path
            .parent()
            .map(Path::to_path_buf)
            .ok_or("Malformed path")?
    } else {
        old_path.to_path_buf()
    };
    let old_module = if is_package {
        module_name_for(db, root, &package_dir.join("__init__.py"))
    } else {
        module_name_for(db, root, old_path)
    }
    .ok_or("Cannot determine the module name")?;
    let parent_parts: Vec<&str> = old_module.split('.').collect();
    let new_module = {
        let mut p: Vec<&str> = parent_parts[..parent_parts.len() - 1].to_vec();
        p.push(new_name);
        p.join(".")
    };
    let old_last = parent_parts.last().copied().unwrap_or_default().to_string();
    if old_last == new_name {
        return Err("The module already has this name".into());
    }
    let new_path = if is_package {
        package_dir.with_file_name(new_name)
    } else {
        old_path.with_file_name(format!("{new_name}.py"))
    };
    if new_path.exists() {
        return Err(format!("{} already exists", new_path.display()));
    }
    let mut ops = Vec::new();
    for f in load_files(db, project_files) {
        let ftext: &str = &f.text;
        let mut edits: Vec<Edit> = Vec::new();
        let mut rename_bound: Vec<(SyntaxToken, String)> = Vec::new();
        for site in module_imports(db, root, &f, &old_module) {
            match site {
                ModuleImport::Import {
                    alias: a,
                    dotted,
                    path,
                } => {
                    let Some(new) = rewrite_dotted(&path, &old_module, &new_module) else {
                        continue;
                    };
                    edits.push((dotted.text_range(), new.clone()));
                    let old_first = path.split('.').next().unwrap_or_default();
                    let new_first = new.split('.').next().unwrap_or_default();
                    if a.alias().is_none() && old_first != new_first {
                        if let Some(tok) = a.name_token() {
                            rename_bound.push((tok, new_first.to_string()));
                        }
                    }
                }
                ModuleImport::From { imp, abs, written } => {
                    let Some(new_abs) = rewrite_dotted(&abs, &old_module, &new_module) else {
                        continue;
                    };
                    if let Some(d) = imp.module() {
                        let new_written = if imp.level() > 0 {
                            new_abs
                                .rsplit('.')
                                .take(written.split('.').count())
                                .collect::<Vec<_>>()
                                .into_iter()
                                .rev()
                                .collect::<Vec<_>>()
                                .join(".")
                        } else {
                            new_abs
                        };
                        edits.push((d.syntax().text_range(), new_written));
                    }
                }
                ModuleImport::FromAlias { alias: a } => {
                    if let Some(tok) = a.name_token() {
                        edits.push((tok.text_range(), new_name.to_string()));
                        if a.alias().is_none() {
                            rename_bound.push((tok, new_name.to_string()));
                        }
                    }
                }
            }
        }
        for (tok, new) in rename_bound {
            if let Some((id, _)) = f.scopes.binding_at(tok.text_range().start()) {
                for r in f.scopes.references_to(id) {
                    if !edits.iter().any(|(e, _)| e.contains_range(r.range)) {
                        edits.push((r.range, new.clone()));
                    }
                }
            }
        }
        edits.sort_by_key(|(r, _)| r.start());
        edits.dedup_by_key(|(r, _)| r.start());
        if !edits.is_empty() {
            let _ = ftext;
            ops.push(FileOp::Edit(f.path.clone(), edits));
        }
    }
    ops.push(FileOp::Move(
        if is_package {
            package_dir
        } else {
            old_path.to_path_buf()
        },
        new_path,
    ));
    Ok(ops)
}

/// Relocate a Python module/package, retaining import bindings and the meaning
/// of relative imports inside the moved tree. All edits precede the physical move.
pub fn move_module(
    db: &Analysis,
    root: &Path,
    source: &Path,
    destination: &Path,
    project_files: &[PathBuf],
) -> Result<Vec<FileOp>, String> {
    if destination.exists() || destination.starts_with(source) {
        return Err("Conflicting or recursive module destination".into());
    }
    let package = source.is_dir();
    let old_file = if package {
        source.join("__init__.py")
    } else {
        source.into()
    };
    let new_file = if package {
        destination.join("__init__.py")
    } else {
        destination.into()
    };
    let old = module_name_for(db, root, &old_file).ok_or("Source is not importable")?;
    let new = module_name_for(db, root, &new_file).ok_or("Destination is not importable")?;
    if !new.split('.').all(is_identifier)
        || !rustpy_syntax::parse(&format!("import {new}\n"))
            .errors()
            .is_empty()
    {
        return Err("Destination package components must be Python identifiers".into());
    }
    let new_leaf = new.rsplit('.').next().unwrap_or(&new);
    let new_parent = new.rsplit_once('.').map(|(parent, _)| parent).unwrap_or("");
    let mut result = Vec::new();
    for file in load_files(db, project_files) {
        let mut edits = Vec::new();
        let mut rewritten = HashSet::new();
        for site in module_imports(db, root, &file, &old) {
            match site {
                ModuleImport::Import {
                    alias,
                    dotted,
                    path,
                } => {
                    let Some(replacement) = rewrite_dotted(&path, &old, &new) else {
                        continue;
                    };
                    if alias.alias().is_some() {
                        edits.push((dotted.text_range(), replacement));
                    } else {
                        // Preserve the old lexical binding. For `import old.pkg`,
                        // its uses are rewritten through the actual bound reference IDs.
                        edits.push((dotted.text_range(), replacement.clone()));
                        if let Some(name) = alias.name_token() {
                            if let Some((binding, _)) =
                                file.scopes.binding_at(name.text_range().start())
                            {
                                let old_first = path.split('.').next().unwrap_or("");
                                let new_first = replacement.split('.').next().unwrap_or("");
                                for reference in file.scopes.references_to(binding) {
                                    let root = file.parse.syntax_node();
                                    let token = root
                                        .token_at_offset(reference.range.start())
                                        .right_biased();
                                    let access = token
                                        .into_iter()
                                        .flat_map(|token| token.parent_ancestors())
                                        .take_while(|node| {
                                            matches!(
                                                node.kind(),
                                                SyntaxKind::NAME_EXPR | SyntaxKind::ATTRIBUTE_EXPR
                                            )
                                        })
                                        .find(|node| {
                                            node.text()
                                                .to_string()
                                                .split_whitespace()
                                                .collect::<String>()
                                                == path
                                        });
                                    if let Some(access) = access {
                                        edits.push((access.text_range(), replacement.clone()));
                                    } else if old_first != new_first {
                                        edits.push((reference.range, new_first.into()));
                                    }
                                }
                            }
                        }
                    }
                }
                ModuleImport::From { imp, abs, .. } => {
                    if !rewritten.insert(imp.syntax().text_range()) {
                        continue;
                    }
                    let Some(replacement) = rewrite_dotted(&abs, &old, &new) else {
                        continue;
                    };
                    let aliases = if imp.is_star() {
                        "*".into()
                    } else {
                        imp.aliases()
                            .map(|alias| alias.syntax().text().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    edits.push((
                        imp.syntax().text_range(),
                        format!("from {replacement} import {aliases}"),
                    ));
                }
                ModuleImport::FromAlias { alias } => {
                    let Some(imp) = alias.syntax().parent().and_then(ast::ImportFromStmt::cast)
                    else {
                        continue;
                    };
                    if !rewritten.insert(imp.syntax().text_range()) {
                        continue;
                    }
                    let written = imp.module_path();
                    let prefix = &file.text[..usize::from(imp.syntax().text_range().start())];
                    let indent = prefix
                        .rsplit('\n')
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take_while(|character| character.is_whitespace())
                        .collect::<String>();
                    let mut statements = Vec::new();
                    for item in imp.aliases() {
                        if item.syntax().text_range() == alias.syntax().text_range() {
                            let binding = item
                                .alias()
                                .map(|token| token.text().to_string())
                                .unwrap_or_else(|| item.name());
                            if new_parent.is_empty() {
                                statements.push(format!("import {new_leaf} as {binding}"));
                            } else {
                                statements.push(format!(
                                    "from {new_parent} import {new_leaf} as {binding}"
                                ));
                            }
                        } else {
                            statements
                                .push(format!("from {written} import {}", item.syntax().text()));
                        }
                    }
                    edits.push((
                        imp.syntax().text_range(),
                        statements.join(&format!("\n{indent}")),
                    ));
                }
            }
        }
        if file.path == source || file.path.starts_with(source) {
            if let Some(module) = module_name_for(db, root, &file.path) {
                for imp in file
                    .parse
                    .syntax_node()
                    .descendants()
                    .filter_map(ast::ImportFromStmt::cast)
                    .filter(|import| import.level() > 0)
                {
                    if !rewritten.insert(imp.syntax().text_range()) {
                        continue;
                    }
                    let written = imp.module().map(|module| module.path()).unwrap_or_default();
                    let absolute =
                        absolute_module(&module, file.is_package_init(), imp.level(), &written);
                    let target = rewrite_dotted(&absolute, &old, &new).unwrap_or(absolute);
                    if target.is_empty() {
                        return Err(
                            "Cannot preserve a relative import above the source root".into()
                        );
                    }
                    let aliases = if imp.is_star() {
                        "*".into()
                    } else {
                        imp.aliases()
                            .map(|alias| alias.syntax().text().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    edits.push((
                        imp.syntax().text_range(),
                        format!("from {target} import {aliases}"),
                    ));
                }
            }
        }
        edits.sort_by_key(|(range, _)| range.start());
        edits.dedup_by_key(|(range, _)| *range);
        if edits
            .windows(2)
            .any(|pair| pair[0].0.end() > pair[1].0.start())
        {
            return Err("Overlapping module-move edits".into());
        }
        if !edits.is_empty() {
            result.push(FileOp::Edit(file.path.clone(), edits));
        }
    }
    result.push(FileOp::Move(source.into(), destination.into()));
    Ok(result)
}

/// One way an import statement names a module (or something inside it).
enum ModuleImport {
    /// `import module[.sub] [as x]`: the alias, its dotted name and the path written.
    Import {
        alias: ast::ImportAlias,
        dotted: SyntaxNode,
        path: String,
    },
    /// `from module[.sub] import x` (absolute or relative): the absolute and the written module.
    From {
        imp: ast::ImportFromStmt,
        abs: String,
        written: String,
    },
    /// `from parent import module [as x]`: the alias importing the module itself.
    FromAlias { alias: ast::ImportAlias },
}

impl ModuleImport {
    fn statement(&self) -> Option<SyntaxNode> {
        match self {
            ModuleImport::Import { alias, .. } | ModuleImport::FromAlias { alias } => {
                alias.syntax().parent()
            }
            ModuleImport::From { imp, .. } => Some(imp.syntax().clone()),
        }
    }
}

/// The imports in `f` (top-level and nested) that name `module` or a
/// submodule of it.
fn module_imports(db: &Analysis, root: &Path, f: &FileInfo, module: &str) -> Vec<ModuleImport> {
    let fmodule = module_name_for(db, root, &f.path);
    let mut out = Vec::new();
    for s in top_level_statements(f).into_iter().chain(nested_imports(f)) {
        match s {
            ast::Stmt::Import(imp) => {
                for a in imp.aliases() {
                    let path = a.name();
                    if rewrite_dotted(&path, module, module).is_none() {
                        continue;
                    }
                    if let Some(dotted) = a
                        .syntax()
                        .children()
                        .find(|c| c.kind() == SyntaxKind::DOTTED_NAME)
                    {
                        out.push(ModuleImport::Import {
                            alias: a,
                            dotted,
                            path,
                        });
                    }
                }
            }
            ast::Stmt::ImportFrom(imp) => {
                let level = imp.level();
                let written = imp.module().map(|d| d.path()).unwrap_or_default();
                let abs = if level > 0 {
                    fmodule
                        .as_ref()
                        .map(|m| absolute_module(m, f.is_package_init(), level, &written))
                        .unwrap_or_default()
                } else {
                    written.clone()
                };
                if rewrite_dotted(&abs, module, module).is_some() {
                    if imp.module().is_some() {
                        out.push(ModuleImport::From { imp, abs, written });
                    }
                } else if !imp.is_star() {
                    for a in imp.aliases() {
                        let full = if abs.is_empty() {
                            a.name()
                        } else {
                            format!("{abs}.{}", a.name())
                        };
                        if full == module {
                            out.push(ModuleImport::FromAlias { alias: a });
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Import statements in `project_files` that import the module or package
/// at `module_path` (or something inside it), as usages for the Find tool
/// window. Files inside the package itself do not count.
pub fn module_import_sites(
    db: &Analysis,
    root: &Path,
    module_path: &Path,
    project_files: &[PathBuf],
) -> Vec<rustpy_semantic::usages::Usage> {
    let is_init = module_path.file_name().and_then(|n| n.to_str()) == Some("__init__.py");
    let is_package = module_path.is_dir() || is_init;
    let package_dir = if is_init {
        module_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    } else {
        module_path.to_path_buf()
    };
    let module = if is_package {
        module_name_for(db, root, &package_dir.join("__init__.py"))
    } else {
        module_name_for(db, root, module_path)
    };
    let Some(module) = module else {
        return Vec::new();
    };
    let mut out: Vec<rustpy_semantic::usages::Usage> = Vec::new();
    for f in load_files(db, project_files) {
        if f.path == module_path || (is_package && f.path.starts_with(&package_dir)) {
            continue;
        }
        for site in module_imports(db, root, &f, &module) {
            let Some(stmt) = site.statement() else {
                continue;
            };
            if !out
                .iter()
                .any(|u| u.path == f.path && u.range == stmt.text_range())
            {
                out.push(rustpy_semantic::usages::usage_at(
                    &f,
                    stmt.text_range(),
                    false,
                    false,
                ));
            }
        }
    }
    out
}

/// Import statements nested in functions/classes/if-blocks (top-level ones excluded).
fn nested_imports(file: &FileInfo) -> Vec<ast::Stmt> {
    let root = file.parse.syntax_node();
    root.descendants()
        .filter(|n| {
            matches!(
                n.kind(),
                SyntaxKind::IMPORT_STMT | SyntaxKind::IMPORT_FROM_STMT
            ) && n.parent().map(|p| p.kind()) != Some(SyntaxKind::MODULE)
        })
        .filter_map(ast::Stmt::cast)
        .collect()
}

// ---------------------------------------------------------------------------
// 8. Change signature extras
// ---------------------------------------------------------------------------

/// Call sites of `func` in `file`: `(arg list, called through an attribute)`.
fn call_sites(
    db: &Analysis,
    file: &Arc<FileInfo>,
    func: &ast::FuncDef,
) -> Vec<(ast::ArgList, bool)> {
    let root = file.parse.syntax_node();
    let Some(name_tok) = func.name() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for u in rustpy_semantic::usages::usages_in_file(db, file, name_tok.text_range().start()) {
        if u.is_definition {
            continue;
        }
        let Some(tok) = token_at(&root, u.range.start()) else {
            continue;
        };
        let Some(parent) = tok.parent() else { continue };
        let (callee, via_attr) = match parent.kind() {
            SyntaxKind::NAME_EXPR => (parent, false),
            SyntaxKind::ATTRIBUTE_EXPR => (parent, true),
            _ => continue,
        };
        let Some(call) = callee.parent().and_then(ast::CallExpr::cast) else {
            continue;
        };
        if call.callee().map(|c| c.text_range()) != Some(callee.text_range()) {
            continue;
        }
        if let Some(args) = call.arg_list() {
            out.push((args, via_attr));
        }
    }
    out
}

fn line_no(text: &str, off: usize) -> usize {
    text[..off.min(text.len())].matches('\n').count() + 1
}

/// `(plain params, index of the `*` separator / *args, index of `/`)` in
/// parameter-list order (indices into the full `AnyParam` list).
fn param_layout(params: &ast::ParamList) -> (Vec<ast::AnyParam>, Option<usize>, Option<usize>) {
    let all: Vec<ast::AnyParam> = params.params().collect();
    let star = all
        .iter()
        .position(|p| matches!(p, ast::AnyParam::Star(_) | ast::AnyParam::StarSep(_)));
    let slash = all
        .iter()
        .position(|p| matches!(p, ast::AnyParam::Slash(_)));
    (all, star, slash)
}

fn plain_index_to_all(all: &[ast::AnyParam], index: usize) -> Option<usize> {
    all.iter()
        .enumerate()
        .filter(|(_, p)| matches!(p, ast::AnyParam::Plain(_)))
        .nth(index)
        .map(|(i, _)| i)
}

/// Makes the plain parameter `index` (and those after it) keyword-only by
/// inserting a `*` separator; positional arguments at call sites are
/// converted to keyword arguments.
pub fn make_keyword_only(
    db: &Analysis,
    file: &Arc<FileInfo>,
    func_offset: TextSize,
    index: usize,
) -> Result<Refactoring, String> {
    let func_offset = rustpy_semantic::clamp_offset(file, func_offset);
    let text: &str = &file.text;
    let func = func_at(file, func_offset)?;
    let params = func.params().ok_or("The function has no parameter list")?;
    let (all, star, _) = param_layout(&params);
    let plain: Vec<ast::Param> = params.plain_params().collect();
    let param = plain
        .get(index)
        .ok_or_else(|| format!("No parameter #{index}"))?;
    let pname = param.name_text();
    if func.is_method() && index == 0 && (pname == "self" || pname == "cls") {
        return Err(format!("'{pname}' cannot be keyword-only"));
    }
    let pos_all = plain_index_to_all(&all, index).ok_or("Malformed parameter list")?;
    if star.is_some_and(|s| s < pos_all) {
        return Err(format!("'{pname}' is already keyword-only"));
    }
    if star.is_some() {
        return Err("A function with *args cannot get keyword-only parameters before it".into());
    }
    let mut ed = Edits::default();
    let at: usize = param.syntax().text_range().start().into();
    ed.add(rng(at, at), "*, ");
    for (args, via_attr) in call_sites(db, file, &func) {
        let shift = usize::from(via_attr && func.is_method());
        if args.args().any(|a| matches!(a, ast::AnyArg::Star(_))) {
            return Err(format!(
                "Call at line {} uses *args and cannot be updated automatically",
                line_no(text, args.text_range().start().into())
            ));
        }
        let positional: Vec<ast::Arg> = args
            .syntax()
            .children()
            .filter_map(ast::Arg::cast)
            .collect();
        for (j, a) in positional.iter().enumerate() {
            let pidx = j + shift;
            if pidx < index {
                continue;
            }
            let Some(p) = plain.get(pidx) else {
                return Err(format!(
                    "Call at line {} passes too many positional arguments",
                    line_no(text, args.text_range().start().into())
                ));
            };
            ed.add(
                a.text_range(),
                format!("{}={}", p.name_text(), a.syntax().text()),
            );
        }
    }
    let (edits, _) = ed.finish();
    Ok(Refactoring {
        title: format!("Make '{pname}' keyword-only"),
        edits,
        rename_ranges: Vec::new(),
        name: pname,
    })
}

/// Makes the plain parameter `index` (and those before it) positional-only
/// by inserting a `/` separator; keyword arguments at call sites are
/// converted to positional ones.
pub fn make_positional_only(
    db: &Analysis,
    file: &Arc<FileInfo>,
    func_offset: TextSize,
    index: usize,
) -> Result<Refactoring, String> {
    let func_offset = rustpy_semantic::clamp_offset(file, func_offset);
    let text: &str = &file.text;
    let func = func_at(file, func_offset)?;
    let params = func.params().ok_or("The function has no parameter list")?;
    let (all, star, slash) = param_layout(&params);
    let plain: Vec<ast::Param> = params.plain_params().collect();
    let param = plain
        .get(index)
        .ok_or_else(|| format!("No parameter #{index}"))?;
    let pname = param.name_text();
    let pos_all = plain_index_to_all(&all, index).ok_or("Malformed parameter list")?;
    if slash.is_some_and(|s| s > pos_all) {
        return Err(format!("'{pname}' is already positional-only"));
    }
    if slash.is_some() {
        return Err(
            "The function already has positional-only parameters; move the '/' manually".into(),
        );
    }
    if star.is_some_and(|s| s < pos_all) {
        return Err(format!(
            "'{pname}' is keyword-only and cannot be positional-only"
        ));
    }
    let mut ed = Edits::default();
    let at: usize = param.syntax().text_range().end().into();
    ed.add(rng(at, at), ", /");
    for (args, via_attr) in call_sites(db, file, &func) {
        let shift = usize::from(via_attr && func.is_method());
        let positional: Vec<ast::Arg> = args
            .syntax()
            .children()
            .filter_map(ast::Arg::cast)
            .collect();
        let keywords: Vec<ast::KeywordArg> = args.keywords().collect();
        let affected: Vec<&ast::KeywordArg> = keywords
            .iter()
            .filter(|k| {
                plain
                    .iter()
                    .take(index + 1)
                    .skip(shift)
                    .any(|p| p.name_text() == k.name_text())
            })
            .collect();
        if affected.is_empty() {
            continue;
        }
        if args
            .args()
            .any(|a| matches!(a, ast::AnyArg::Star(_) | ast::AnyArg::StarStar(_)))
        {
            return Err(format!(
                "Call at line {} uses unpacking and cannot be updated automatically",
                line_no(text, args.text_range().start().into())
            ));
        }
        // Values for parameters shift..=index that are not passed positionally yet.
        let mut values: Vec<String> = Vec::new();
        for (pi, p) in plain.iter().enumerate().take(index + 1).skip(shift) {
            let pos = pi - shift;
            if pos < positional.len() {
                continue;
            }
            match keywords.iter().find(|k| k.name_text() == p.name_text()) {
                Some(k) => values.push(
                    k.value()
                        .map(|v| v.syntax().text().to_string())
                        .unwrap_or_default(),
                ),
                None if p.default().is_some() => values.push(
                    p.default()
                        .map(|d| d.syntax().text().to_string())
                        .unwrap_or_default(),
                ),
                None => {
                    return Err(format!(
                        "Call at line {} does not pass '{}'",
                        line_no(text, args.text_range().start().into()),
                        p.name_text()
                    ))
                }
            }
        }
        for k in &affected {
            ed.add(element_with_comma(text, k.syntax()), "");
        }
        if !values.is_empty() {
            match positional.last() {
                Some(last) => {
                    let end: usize = last.text_range().end().into();
                    ed.add(rng(end, end), format!(", {}", values.join(", ")));
                }
                None => {
                    let open = args
                        .syntax()
                        .first_token()
                        .filter(|t| t.kind() == SyntaxKind::L_PAREN)
                        .ok_or("Malformed call")?;
                    let at: usize = open.text_range().end().into();
                    let remaining = keywords.len() - affected.len();
                    ed.add(
                        rng(at, at),
                        format!(
                            "{}{}",
                            values.join(", "),
                            if remaining > 0 { ", " } else { "" }
                        ),
                    );
                }
            }
        }
    }
    let (edits, _) = ed.finish();
    Ok(Refactoring {
        title: format!("Make '{pname}' positional-only"),
        edits,
        rename_ranges: Vec::new(),
        name: pname,
    })
}

/// Renames the plain parameter `index`: definition, body references,
/// keyword arguments at call sites and the docstring entry.
pub fn rename_parameter(
    db: &Analysis,
    file: &Arc<FileInfo>,
    func_offset: TextSize,
    index: usize,
    new_name: &str,
) -> Result<Refactoring, String> {
    let func_offset = rustpy_semantic::clamp_offset(file, func_offset);
    if !is_identifier(new_name) {
        return Err(format!("'{new_name}' is not a valid parameter name"));
    }
    let func = func_at(file, func_offset)?;
    let params = func.params().ok_or("The function has no parameter list")?;
    let plain: Vec<ast::Param> = params.plain_params().collect();
    let param = plain
        .get(index)
        .ok_or_else(|| format!("No parameter #{index}"))?;
    let old = param.name_text();
    if params
        .params()
        .any(|p| p.name().is_some_and(|t| t.text() == new_name))
    {
        return Err(format!("Parameter '{new_name}' already exists"));
    }
    let name_tok = param.name().ok_or("Malformed parameter")?;
    let mut ed = Edits::default();
    let (bid, _) = file
        .scopes
        .binding_at(name_tok.text_range().start())
        .ok_or("Cannot resolve the parameter")?;
    ed.add_marked(name_tok.text_range(), new_name, vec![(0, new_name.len())]);
    let mut seen: HashSet<TextSize> = HashSet::new();
    for r in file.scopes.references_to(bid) {
        if seen.insert(r.range.start()) {
            ed.add_marked(r.range, new_name, vec![(0, new_name.len())]);
        }
    }
    for (args, _) in call_sites(db, file, &func) {
        for k in args.keywords() {
            if k.name_text() == old {
                if let Some(t) = k.name() {
                    ed.add_marked(t.text_range(), new_name, vec![(0, new_name.len())]);
                }
            }
        }
    }
    for r in rustpy_semantic::usages::docstring_param_ranges(file, func.syntax().text_range(), &old)
    {
        ed.add_marked(r, new_name, vec![(0, new_name.len())]);
    }
    let (edits, rename_ranges) = ed.finish();
    Ok(Refactoring {
        title: format!("Rename parameter '{old}' to '{new_name}'"),
        edits,
        rename_ranges,
        name: new_name.to_string(),
    })
}

/// Sets (or removes with `None`) the default value of the plain parameter `index`.
pub fn set_default(
    db: &Analysis,
    file: &Arc<FileInfo>,
    func_offset: TextSize,
    index: usize,
    default: Option<&str>,
) -> Result<Refactoring, String> {
    let func_offset = rustpy_semantic::clamp_offset(file, func_offset);
    let text: &str = &file.text;
    let func = func_at(file, func_offset)?;
    let params = func.params().ok_or("The function has no parameter list")?;
    let (all, star, _) = param_layout(&params);
    let plain: Vec<ast::Param> = params.plain_params().collect();
    let param = plain
        .get(index)
        .ok_or_else(|| format!("No parameter #{index}"))?;
    let pname = param.name_text();
    let pos_all = plain_index_to_all(&all, index).ok_or("Malformed parameter list")?;
    let keyword_only = star.is_some_and(|s| s < pos_all);
    let mut ed = Edits::default();
    match default {
        Some(d) => {
            if d.trim().is_empty() {
                return Err("Empty default value".into());
            }
            if !keyword_only {
                for (i, p) in plain.iter().enumerate().skip(index + 1) {
                    let i_all = plain_index_to_all(&all, i).unwrap_or(usize::MAX);
                    if star.is_some_and(|s| s < i_all) {
                        break;
                    }
                    if p.default().is_none() {
                        return Err(format!(
                            "'{}' has no default value and would follow a parameter with one",
                            p.name_text()
                        ));
                    }
                }
            }
            match param.default() {
                Some(old) => ed.add(old.syntax().text_range(), d.trim()),
                None => {
                    let at: usize = param.syntax().text_range().end().into();
                    ed.add(rng(at, at), format!("={}", d.trim()));
                }
            }
        }
        None => {
            let old = param
                .default()
                .ok_or_else(|| format!("'{pname}' has no default value"))?;
            if !keyword_only {
                for p in plain.iter().take(index) {
                    if p.default().is_some() {
                        return Err(format!(
                            "'{}' has a default value and would precede a required parameter",
                            p.name_text()
                        ));
                    }
                }
            }
            let end: usize = param.syntax().text_range().end().into();
            let mut start: usize = old.syntax().text_range().start().into();
            while start > 0 && text.as_bytes()[start - 1] != b'=' {
                start -= 1;
            }
            start -= 1;
            while start > 0 && text.as_bytes()[start - 1] == b' ' {
                start -= 1;
            }
            ed.add(rng(start, end), "");
            // Calls that relied on the default get a placeholder argument.
            for (args, via_attr) in call_sites(db, file, &func) {
                let shift = usize::from(via_attr && func.is_method());
                let positional: Vec<ast::Arg> = args
                    .syntax()
                    .children()
                    .filter_map(ast::Arg::cast)
                    .collect();
                let passed = index >= shift && index - shift < positional.len()
                    || args.keywords().any(|k| k.name_text() == pname)
                    || args
                        .args()
                        .any(|a| matches!(a, ast::AnyArg::Star(_) | ast::AnyArg::StarStar(_)));
                if passed {
                    continue;
                }
                let placeholder = if keyword_only || positional.len() + shift < index {
                    format!("{pname}={pname}")
                } else {
                    pname.clone()
                };
                match args.syntax().children().last() {
                    Some(last_arg) => {
                        let end: usize = last_arg.text_range().end().into();
                        ed.add(rng(end, end), format!(", {placeholder}"));
                    }
                    None => {
                        let open = args
                            .syntax()
                            .first_token()
                            .filter(|t| t.kind() == SyntaxKind::L_PAREN)
                            .ok_or("Malformed call")?;
                        let at: usize = open.text_range().end().into();
                        ed.add(rng(at, at), placeholder);
                    }
                }
            }
        }
    }
    let (edits, _) = ed.finish();
    let title = match default {
        Some(d) => format!("Set default of '{pname}' to {}", d.trim()),
        None => format!("Remove default of '{pname}'"),
    };
    Ok(Refactoring {
        title,
        edits,
        rename_ranges: Vec::new(),
        name: pname,
    })
}

// ---------------------------------------------------------------------------
// 9. Type aliases, dataclasses, type hints
// ---------------------------------------------------------------------------

/// Annotation-position expression nodes of the file (parameter/return/variable annotations).
fn annotation_exprs(file: &FileInfo) -> Vec<ast::Expr> {
    let root = file.parse.syntax_node();
    let mut out = Vec::new();
    for n in root.descendants() {
        let expr = match n.kind() {
            SyntaxKind::ANNOTATION | SyntaxKind::RETURN_ANNOTATION => {
                n.children().find_map(ast::Expr::cast)
            }
            SyntaxKind::ANN_ASSIGN_STMT => {
                ast::AnnAssignStmt::cast(n.clone()).and_then(|a| a.annotation())
            }
            _ => None,
        };
        if let Some(e) = expr {
            out.push(e);
        }
    }
    out
}

/// Every sub-expression (including the whole) of annotation expressions.
fn annotation_subexprs(file: &FileInfo) -> Vec<ast::Expr> {
    let mut out = Vec::new();
    for e in annotation_exprs(file) {
        for n in e.syntax().descendants() {
            if let Some(x) = ast::Expr::cast(n) {
                out.push(x);
            }
        }
    }
    out
}

/// Introduces `name = <selected type expression>` at module level and
/// replaces every identical annotation (sub)expression with `name`.
pub fn introduce_type_alias(
    file: &Arc<FileInfo>,
    range: TextRange,
    name: &str,
) -> Result<Refactoring, String> {
    let text: &str = &file.text;
    if !is_identifier(name) {
        return Err(format!("'{name}' is not a valid name"));
    }
    if file
        .scopes
        .resolve(file.scopes.module_scope(), name)
        .is_some()
    {
        return Err(format!("'{name}' is already defined"));
    }
    let expr = expr_at(file, range).ok_or("Select a type expression")?;
    let in_annotation = expr.syntax().ancestors().any(|a| {
        matches!(
            a.kind(),
            SyntaxKind::ANNOTATION | SyntaxKind::RETURN_ANNOTATION | SyntaxKind::ANN_ASSIGN_STMT
        )
    });
    if !in_annotation {
        return Err("Select a type expression inside an annotation".into());
    }
    let target = normalize(&expr.syntax().text().to_string());
    let mut occurrences: Vec<TextRange> = Vec::new();
    for e in annotation_subexprs(file) {
        if normalize(&e.syntax().text().to_string()) == target {
            let r = e.syntax().text_range();
            if !occurrences.iter().any(|o| o.contains_range(r)) {
                occurrences.push(r);
            }
        }
    }
    occurrences.sort_by_key(|r| r.start());
    occurrences.dedup();
    // Anchor: the top-level statement containing the first occurrence.
    let first = occurrences.first().copied().ok_or("No occurrences")?;
    let root = file.parse.syntax_node();
    let anchor = token_at(&root, first.start())
        .and_then(|t| {
            t.parent_ancestors()
                .filter(|n| n.parent().map(|p| p.kind()) == Some(SyntaxKind::MODULE))
                .last()
        })
        .ok_or("Cannot find a statement to anchor the alias")?;
    let at = line_start(text, anchor.text_range().start().into());
    let sep = if matches!(anchor.kind(), SyntaxKind::FUNC_DEF | SyntaxKind::CLASS_DEF) {
        "\n\n"
    } else {
        "\n"
    };
    let mut ed = Edits::default();
    ed.add_marked(
        rng(at, at),
        format!("{name} = {}\n{sep}", expr.syntax().text()),
        vec![(0, name.len())],
    );
    for r in occurrences {
        ed.add_marked(r, name, vec![(0, name.len())]);
    }
    let (edits, rename_ranges) = ed.finish();
    Ok(Refactoring {
        title: format!("Introduce type alias '{name}'"),
        edits,
        rename_ranges,
        name: name.to_string(),
    })
}

/// Inlines the module-level type alias (`Name = expr` or `type Name = expr`)
/// at `offset` into its usages and removes it.
pub fn inline_type_alias(
    db: &Analysis,
    file: &Arc<FileInfo>,
    offset: TextSize,
) -> Result<Refactoring, String> {
    let offset = rustpy_semantic::clamp_offset(file, offset);
    let _ = db;
    let text: &str = &file.text;
    let root = file.parse.syntax_node();
    let tok = token_at(&root, offset).ok_or("Nothing here")?;
    let name = tok.text().to_string();
    let module_scope = file.scopes.module_scope();
    let ids = file
        .scopes
        .resolve(file.scopes.scope_at(offset), &name)
        .ok_or_else(|| format!("'{name}' is not defined"))?;
    let (bid, binding) = ids
        .iter()
        .map(|id| (*id, file.scopes.binding(*id)))
        .find(|(_, b)| b.scope == module_scope && b.is_definition())
        .ok_or_else(|| format!("'{name}' is not a module-level alias"))?;
    let stmt = node_with_range(&root, binding.node_range)
        .and_then(|n| n.ancestors().find_map(ast::Stmt::cast))
        .ok_or("Cannot find the alias statement")?;
    let value = match &stmt {
        ast::Stmt::Assign(a) if a.targets().len() == 1 => a.value(),
        ast::Stmt::AnnAssign(a) => a.value(),
        ast::Stmt::TypeAlias(t) => t.value(),
        _ => None,
    }
    .ok_or_else(|| format!("'{name}' is not a simple alias"))?;
    let value_text = value.syntax().text().to_string();
    let mut ed = Edits::default();
    ed.add(deletion_range(text, stmt.syntax().text_range()), "");
    let mut n = 0;
    for r in file.scopes.references_to(bid) {
        if stmt.syntax().text_range().contains_range(r.range) {
            continue;
        }
        ed.add(r.range, value_text.clone());
        n += 1;
    }
    if n == 0 {
        return Err(format!("'{name}' is not used"));
    }
    let (edits, _) = ed.finish();
    Ok(Refactoring {
        title: format!("Inline type alias '{name}'"),
        edits,
        rename_ranges: Vec::new(),
        name,
    })
}

/// Converts a class with a trivial `__init__` (only `self.x = x` assignments)
/// into a `@dataclass` with annotated fields.
pub fn convert_to_dataclass(
    db: &Analysis,
    file: &Arc<FileInfo>,
    class_offset: TextSize,
) -> Result<Refactoring, String> {
    let class_offset = rustpy_semantic::clamp_offset(file, class_offset);
    let text: &str = &file.text;
    let class = class_at(file, class_offset)?;
    if class.decorators().any(|d| {
        d.name_text()
            .as_deref()
            .is_some_and(|n| n.ends_with("dataclass"))
    }) {
        return Err(format!("'{}' already is a dataclass", class.name_text()));
    }
    let init = class
        .methods()
        .find(|m| m.name_text() == "__init__")
        .ok_or_else(|| format!("'{}' has no __init__", class.name_text()))?;
    let params: Vec<ast::Param> = init
        .params()
        .map(|p| p.plain_params().collect())
        .unwrap_or_default();
    let params: Vec<&ast::Param> = params.iter().filter(|p| p.name_text() != "self").collect();
    let body: Vec<ast::Stmt> = init
        .body()
        .map(|b| b.statements().filter(|s| !is_docstring_stmt(s)).collect())
        .unwrap_or_default();
    let mut assigned: Vec<(String, String)> = Vec::new();
    for s in &body {
        let ast::Stmt::Assign(a) = s else {
            return Err(
                "__init__ is not trivial (only `self.x = x` assignments are supported)".into(),
            );
        };
        let targets = a.targets();
        let (Some(ast::Expr::Attribute(attr)), Some(ast::Expr::Name(v))) =
            (targets.first(), a.value())
        else {
            return Err(
                "__init__ is not trivial (only `self.x = x` assignments are supported)".into(),
            );
        };
        let is_self = matches!(attr.object(), Some(ast::Expr::Name(o)) if o.name_text() == "self");
        if !is_self || targets.len() != 1 {
            return Err(
                "__init__ is not trivial (only `self.x = x` assignments are supported)".into(),
            );
        }
        assigned.push((attr.attr_text(), v.name_text()));
    }
    let inf = Inferer::new(db);
    let mut fields = String::new();
    let mut needs_any = false;
    let mut needs_field = false;
    let ind = class_body_indent(text, &class);
    for p in &params {
        let pname = p.name_text();
        let attr_name = assigned
            .iter()
            .find(|(_, v)| *v == pname)
            .map(|(a, _)| a.clone())
            .unwrap_or_else(|| pname.clone());
        let annotation = match p.annotation() {
            Some(a) => a.syntax().text().to_string(),
            None => match p
                .default()
                .and_then(|d| annotation_for_expr(&inf, file, &d))
            {
                Some(t) => t,
                None => {
                    needs_any = true;
                    "Any".to_string()
                }
            },
        };
        let default = match p.default() {
            Some(d) => {
                let dt = d.syntax().text().to_string();
                let factory = match dt.chars().next() {
                    Some('[') => Some("list"),
                    Some('{') => Some(if dt.contains(':') { "dict" } else { "set" }),
                    _ => None,
                };
                match factory {
                    Some(f) => {
                        needs_field = true;
                        format!(" = field(default_factory={f})")
                    }
                    None => format!(" = {dt}"),
                }
            }
            None => String::new(),
        };
        fields.push_str(&format!("{ind}{attr_name}: {annotation}{default}\n"));
    }
    let mut ed = Edits::default();
    let at = line_start(text, class.syntax().text_range().start().into());
    let mut prefix_edits = vec![(
        rng(at, at),
        format!("{}@dataclass\n", class_indent(text, &class)),
    )];
    let init_stmt = ast::Stmt::FuncDef(init.clone());
    let remaining = body_statements(&class)
        .iter()
        .filter(|s| s.syntax().text_range() != init.syntax().text_range())
        .count();
    let replacement = if fields.is_empty() && remaining == 0 {
        format!("{ind}pass\n")
    } else {
        fields
    };
    let r = deletion_range(text, init_stmt.syntax().text_range());
    ed.add(
        r,
        if replacement.is_empty() {
            String::new()
        } else {
            format!("{replacement}{}", if remaining > 0 { "\n" } else { "" })
        },
    );
    let mut names: Vec<&str> = vec!["dataclass"];
    if needs_field {
        names.push("field");
    }
    let imports = [
        ensure_from_import(file, "dataclasses", &names),
        if needs_any {
            ensure_from_import(file, "typing", &["Any"])
        } else {
            None
        },
    ];
    for (range, text) in imports.into_iter().flatten() {
        // Import and decorator insertions can share an offset. Compose their
        // text explicitly: Document and string edit appliers order equal
        // ranges differently, and an import must precede the decorator.
        if let Some((_, existing)) = prefix_edits
            .iter_mut()
            .find(|(r, _)| r.is_empty() && *r == range)
        {
            existing.insert_str(0, &text);
        } else {
            prefix_edits.push((range, text));
        }
    }
    for (range, text) in prefix_edits {
        ed.add(range, text);
    }
    let (edits, _) = ed.finish();
    Ok(Refactoring {
        title: format!("Convert '{}' to a dataclass", class.name_text()),
        edits,
        rename_ranges: Vec::new(),
        name: class.name_text(),
    })
}

/// Annotation text for a value expression: literals are widened to their
/// builtin type, other expressions use the inferred type when displayable.
fn annotation_for_expr(inf: &Inferer, file: &Arc<FileInfo>, expr: &ast::Expr) -> Option<String> {
    match expr {
        ast::Expr::String(_) | ast::Expr::FString(_) | ast::Expr::ConcatString(_) => {
            return Some("str".into())
        }
        ast::Expr::Literal(_) => {
            let t = expr.syntax().text().to_string();
            return match t.as_str() {
                "True" | "False" => Some("bool".into()),
                "None" | "..." => None,
                _ if t.starts_with(|c: char| c.is_ascii_digit()) => {
                    if t.contains(['.', 'e', 'E']) && !t.starts_with("0x") && !t.starts_with("0X") {
                        Some("float".into())
                    } else if t.ends_with(['j', 'J']) {
                        Some("complex".into())
                    } else {
                        Some("int".into())
                    }
                }
                _ => None,
            };
        }
        _ => {}
    }
    displayable(&inf.type_of_expr(file, expr))
}

fn displayable(ty: &Type) -> Option<String> {
    if ty.is_unknown() {
        return None;
    }
    let s = ty.to_string();
    if s.contains("Unknown")
        || s.contains('<')
        || s.is_empty()
        || s.contains("Overloaded")
        || s.contains("def ")
    {
        return None;
    }
    if let Some(inner) = s.strip_prefix("Literal[").and_then(|r| r.strip_suffix(']')) {
        let first = inner.split(',').next().unwrap_or("").trim();
        return Some(match first {
            "True" | "False" => "bool".into(),
            f if f.starts_with(['"', '\'']) => "str".into(),
            f if f.starts_with(|c: char| c.is_ascii_digit() || c == '-') => {
                if f.contains('.') {
                    "float".into()
                } else {
                    "int".into()
                }
            }
            _ => return None,
        });
    }
    Some(s)
}

/// Annotates unannotated parameters (from their default values) and the
/// return type (inferred from the body) of the function at `func_offset`.
pub fn add_type_hints(
    db: &Analysis,
    file: &Arc<FileInfo>,
    func_offset: TextSize,
) -> Result<Refactoring, String> {
    let func_offset = rustpy_semantic::clamp_offset(file, func_offset);
    let text: &str = &file.text;
    let func = func_at(file, func_offset)?;
    let params = func.params().ok_or("The function has no parameter list")?;
    let inf = Inferer::new(db);
    let class = func
        .syntax()
        .ancestors()
        .skip(1)
        .find_map(ast::ClassDef::cast)
        .map(|c| class_ref_of(file, &c));
    let sig = inf.signature(file, &func, class.as_ref());
    let mut ed = Edits::default();
    let mut added = 0;
    for (i, p) in params.plain_params().enumerate() {
        if p.annotation().is_some() {
            continue;
        }
        let pname = p.name_text();
        if func.is_method() && i == 0 && (pname == "self" || pname == "cls") {
            continue;
        }
        let mut ty: Option<String> = p
            .default()
            .and_then(|d| annotation_for_expr(&inf, file, &d));
        if ty.is_none() {
            ty = sig
                .params
                .iter()
                .find(|sp| sp.name.as_str() == pname)
                .and_then(|sp| displayable(&sp.ty));
        }
        let Some(ty) = ty else { continue };
        let Some(name_tok) = p.name() else { continue };
        let at: usize = name_tok.text_range().end().into();
        match p.default() {
            // PEP 8: spaces around `=` once the parameter is annotated.
            Some(d) => ed.add(
                rng(at, d.syntax().text_range().start().into()),
                format!(": {ty} = "),
            ),
            None => ed.add(rng(at, at), format!(": {ty}")),
        }
        added += 1;
    }
    if func.return_annotation().is_none() {
        if let Some(ret) = displayable(&sig.ret) {
            let after_params: usize = params.syntax().text_range().end().into();
            if let Some(off) = text[after_params..].find(':') {
                let at = after_params + off;
                ed.add(rng(at, at), format!(" -> {ret}"));
                added += 1;
            }
        }
    }
    if added == 0 {
        return Err("No type information could be inferred".into());
    }
    let (edits, _) = ed.finish();
    Ok(Refactoring {
        title: format!("Add {added} type hint(s)"),
        edits,
        rename_ranges: Vec::new(),
        name: func.name_text(),
    })
}

/// Applies planned file operations to an in-memory file set (used by tests
/// and previews): `files` maps paths to contents.
pub fn apply_file_ops(files: &mut BTreeMap<PathBuf, String>, ops: &[FileOp]) {
    for op in ops {
        if let FileOp::Edit(p, edits) = op {
            if let Some(t) = files.get_mut(p) {
                *t = rustpy_semantic::edit_helpers::apply_edits(t, edits);
            }
        }
    }
    for op in ops {
        match op {
            FileOp::Create(p, content) => {
                files.insert(p.clone(), content.clone());
            }
            FileOp::Delete(p) => {
                files.remove(p);
            }
            FileOp::Move(from, to) => {
                let moved: Vec<(PathBuf, String)> = files
                    .iter()
                    .filter(|(p, _)| p.starts_with(from))
                    .map(|(p, t)| (to.join(p.strip_prefix(from).unwrap_or(p)), t.clone()))
                    .collect();
                let is_dir_move = !moved.is_empty() && !files.contains_key(from);
                if let Some(t) = files.remove(from) {
                    files.insert(to.clone(), t);
                } else if is_dir_move {
                    files.retain(|p, _| !p.starts_with(from));
                    for (p, t) in moved {
                        files.insert(p, t);
                    }
                }
            }
            FileOp::Edit(..) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_semantic::edit_helpers::{apply_edits, ts};

    fn setup(files: &[(&str, &str)]) -> (Analysis, Vec<Arc<FileInfo>>) {
        let db = Analysis::for_project(Path::new("/proj"));
        let mut out = Vec::new();
        for (p, src) in files {
            let id = db.set_file_text(Path::new(p), src);
            out.push(db.file(id).unwrap());
        }
        (db, out)
    }

    fn at(src: &str, needle: &str) -> TextSize {
        ts(src.find(needle).expect("needle"))
    }

    fn sel(src: &str, needle: &str) -> TextRange {
        let s = src.find(needle).expect("needle");
        rng(s, s + needle.len())
    }

    fn change_for<'a>(r: &'a MultiFileRefactoring, path: &str) -> &'a [Edit] {
        r.changes
            .iter()
            .find(|(p, _)| p == Path::new(path))
            .map(|(_, e)| e.as_slice())
            .unwrap_or(&[])
    }

    fn paths(files: &[(&str, &str)]) -> Vec<PathBuf> {
        files.iter().map(|(p, _)| PathBuf::from(p)).collect()
    }

    #[test]
    fn pull_members_up_same_file() {
        let src = "class Base:\n    def hello(self):\n        pass\n\n\nclass Child(Base):\n    LIMIT = 3\n\n    def run(self):\n        return self.LIMIT\n\n    def other(self):\n        pass\n";
        let (db, f) = setup(&[("/proj/m.py", src)]);
        let r = pull_members_up(
            &db,
            &f[0],
            at(src, "class Child"),
            &["run".into(), "LIMIT".into()],
            "Base",
        )
        .unwrap();
        let out = apply_edits(src, change_for(&r, "/proj/m.py"));
        assert_eq!(out, "class Base:\n    LIMIT = 3\n\n    def hello(self):\n        pass\n\n    def run(self):\n        return self.LIMIT\n\n\nclass Child(Base):\n    def other(self):\n        pass\n");
        assert!(pull_members_up(
            &db,
            &f[0],
            at(src, "class Child"),
            &["other".into()],
            "Nope"
        )
        .is_err());
        assert!(pull_members_up(
            &db,
            &f[0],
            at(src, "class Child"),
            &["missing".into()],
            "Base"
        )
        .is_err());
    }

    #[test]
    fn pull_members_up_into_other_file_and_leaves_pass() {
        let base = "class Base:\n    pass\n";
        let child = "from pkg.base import Base\n\n\nclass Child(Base):\n    def run(self):\n        return 1\n";
        let (db, f) = setup(&[("/proj/pkg/base.py", base), ("/proj/pkg/child.py", child)]);
        let r = pull_members_up(
            &db,
            &f[1],
            at(child, "class Child"),
            &["run".into()],
            "Base",
        )
        .unwrap();
        assert_eq!(
            apply_edits(base, change_for(&r, "/proj/pkg/base.py")),
            "class Base:\n    def run(self):\n        return 1\n"
        );
        assert_eq!(
            apply_edits(child, change_for(&r, "/proj/pkg/child.py")),
            "from pkg.base import Base\n\n\nclass Child(Base):\n    pass\n"
        );
        // Already defined in the base.
        let base2 = "class Base:\n    def run(self):\n        return 0\n";
        let (db, f) = setup(&[("/proj/pkg/base.py", base2), ("/proj/pkg/child.py", child)]);
        assert!(pull_members_up(
            &db,
            &f[1],
            at(child, "class Child"),
            &["run".into()],
            "Base"
        )
        .is_err());
    }

    #[test]
    fn push_members_down_copies_into_subclasses() {
        let src = "class Base:\n    def shared(self):\n        return 1\n\n    def keep(self):\n        pass\n\n\nclass A(Base):\n    pass\n\n\nclass B(Base):\n    def own(self):\n        pass\n";
        let files = [("/proj/m.py", src)];
        let (db, f) = setup(&files);
        let r = push_members_down(
            &db,
            &f[0],
            at(src, "class Base"),
            &["shared".into()],
            &paths(&files),
        )
        .unwrap();
        let out = apply_edits(src, change_for(&r, "/proj/m.py"));
        assert_eq!(out, "class Base:\n    def keep(self):\n        pass\n\n\nclass A(Base):\n    def shared(self):\n        return 1\n\n\nclass B(Base):\n    def own(self):\n        pass\n\n    def shared(self):\n        return 1\n");
        let lone = "class Lone:\n    def x(self):\n        pass\n";
        let files = [("/proj/n.py", lone)];
        let (db, f) = setup(&files);
        assert!(push_members_down(
            &db,
            &f[0],
            at(lone, "class Lone"),
            &["x".into()],
            &paths(&files)
        )
        .is_err());
    }

    #[test]
    fn extract_superclass_moves_members_and_adds_base() {
        let src = "class Dog(Animal):\n    legs = 4\n\n    def speak(self):\n        return 'woof'\n\n    def walk(self):\n        return 'walking'\n";
        let (db, f) = setup(&[("/proj/m.py", src)]);
        let r = extract_superclass(
            &db,
            &f[0],
            at(src, "class Dog"),
            "Walker",
            &["walk".into(), "legs".into()],
        )
        .unwrap();
        let out = apply_edits(src, &r.edits);
        assert_eq!(out, "class Walker:\n    legs = 4\n\n    def walk(self):\n        return 'walking'\n\n\nclass Dog(Walker, Animal):\n    def speak(self):\n        return 'woof'\n");
        for rr in &r.rename_ranges {
            assert_eq!(slice(&out, *rr), "Walker");
        }
        assert_eq!(r.rename_ranges.len(), 2);
        let src2 = "class Plain:\n    def a(self):\n        pass\n";
        let (db, f) = setup(&[("/proj/m.py", src2)]);
        let r =
            extract_superclass(&db, &f[0], at(src2, "class Plain"), "Base", &["a".into()]).unwrap();
        assert_eq!(
            apply_edits(src2, &r.edits),
            "class Base:\n    def a(self):\n        pass\n\n\nclass Plain(Base):\n    pass\n"
        );
    }

    #[test]
    fn make_abstract_adds_decorator_base_and_import() {
        let src = "import os\n\n\nclass Shape:\n    def area(self):\n        return 0\n";
        let (db, f) = setup(&[("/proj/m.py", src)]);
        let r = make_abstract(&db, &f[0], at(src, "def area")).unwrap();
        assert_eq!(apply_edits(src, &r.edits), "import os\nfrom abc import ABC, abstractmethod\n\n\nclass Shape(ABC):\n    @abstractmethod\n    def area(self):\n        return 0\n");
        let src2 = "from abc import ABC\n\n\nclass Shape(ABC):\n    @property\n    def area(self):\n        return 0\n";
        let (db, f) = setup(&[("/proj/m.py", src2)]);
        let r = make_abstract(&db, &f[0], at(src2, "def area")).unwrap();
        assert_eq!(apply_edits(src2, &r.edits), "from abc import ABC, abstractmethod\n\n\nclass Shape(ABC):\n    @property\n    @abstractmethod\n    def area(self):\n        return 0\n");
    }

    #[test]
    fn implement_abstract_methods_adds_stubs() {
        let src = "from abc import ABC, abstractmethod\n\n\nclass Shape(ABC):\n    @abstractmethod\n    def area(self) -> float:\n        ...\n\n    @abstractmethod\n    def name(self, prefix: str = '') -> str:\n        ...\n\n    def describe(self):\n        return self.name()\n\n\nclass Square(Shape):\n    def area(self) -> float:\n        return 1.0\n";
        let (db, f) = setup(&[("/proj/m.py", src)]);
        let r = implement_abstract_methods(&db, &f[0], at(src, "class Square")).unwrap();
        assert_eq!(apply_edits(src, &r.edits), "from abc import ABC, abstractmethod\n\n\nclass Shape(ABC):\n    @abstractmethod\n    def area(self) -> float:\n        ...\n\n    @abstractmethod\n    def name(self, prefix: str = '') -> str:\n        ...\n\n    def describe(self):\n        return self.name()\n\n\nclass Square(Shape):\n    def area(self) -> float:\n        return 1.0\n\n    def name(self, prefix: str = '') -> str:\n        raise NotImplementedError\n");
        let src2 = "from abc import ABC, abstractmethod\n\n\nclass Shape(ABC):\n    @abstractmethod\n    def area(self):\n        ...\n\n\nclass Empty(Shape):\n    pass\n";
        let (db, f) = setup(&[("/proj/m.py", src2)]);
        let r = implement_abstract_methods(&db, &f[0], at(src2, "class Empty")).unwrap();
        assert_eq!(apply_edits(src2, &r.edits), "from abc import ABC, abstractmethod\n\n\nclass Shape(ABC):\n    @abstractmethod\n    def area(self):\n        ...\n\n\nclass Empty(Shape):\n    def area(self):\n        raise NotImplementedError\n");
        assert!(implement_abstract_methods(&db, &f[0], at(src2, "class Shape")).is_err());
    }

    #[test]
    fn convert_package_and_module_plans() {
        let ops = convert_to_package(Path::new("/proj/pkg/util.py")).unwrap();
        assert_eq!(
            ops,
            vec![FileOp::Move(
                PathBuf::from("/proj/pkg/util.py"),
                PathBuf::from("/proj/pkg/util/__init__.py")
            )]
        );
        assert!(convert_to_package(Path::new("/proj/pkg/__init__.py")).is_err());
        let dir = tempfile::TempDir::with_prefix("rustpy-r2").unwrap();
        let pkg = dir.path().join("mypkg");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("__init__.py"), "X = 1\n").unwrap();
        let ops = convert_to_module(&pkg).unwrap();
        assert_eq!(
            ops,
            vec![
                FileOp::Move(pkg.join("__init__.py"), dir.path().join("mypkg.py")),
                FileOp::Delete(pkg.clone())
            ]
        );
        std::fs::write(pkg.join("extra.py"), "").unwrap();
        assert!(convert_to_module(&pkg).is_err());
        let mut files: BTreeMap<PathBuf, String> = BTreeMap::new();
        files.insert(PathBuf::from("/proj/pkg/util.py"), "X = 1\n".into());
        apply_file_ops(
            &mut files,
            &convert_to_package(Path::new("/proj/pkg/util.py")).unwrap(),
        );
        assert_eq!(
            files
                .get(Path::new("/proj/pkg/util/__init__.py"))
                .map(String::as_str),
            Some("X = 1\n")
        );
    }

    #[test]
    fn move_import_preference_qualifies_new_imports_without_capturing_names() {
        let source = "LIMIT = 3\n\ndef helper(value):\n    return value + LIMIT\n\ndef process():\n    return helper(LIMIT)\n\nprint(process())\n";
        let target = "a = 1\n\ndef untouched():\n    return a\n";
        let caller = "from a import process as go, helper\nprint(go(), helper(1))\n";
        let files = [
            ("/proj/a.py", source),
            ("/proj/b.py", target),
            ("/proj/c.py", caller),
        ];
        let (db, loaded) = setup(&files);
        let ops = move_symbol_with_import_style(
            &db,
            &loaded[0],
            at(source, "def process"),
            Path::new("/proj/b.py"),
            Path::new("/proj"),
            &paths(&files),
            false,
        )
        .unwrap();
        let mut result: BTreeMap<PathBuf, String> = files
            .iter()
            .map(|(path, text)| (PathBuf::from(path), text.to_string()))
            .collect();
        apply_file_ops(&mut result, &ops);
        let source = &result[Path::new("/proj/a.py")];
        assert!(source.contains("import b\n"), "{source}");
        assert!(source.contains("print(b.process())"), "{source}");
        let target = &result[Path::new("/proj/b.py")];
        assert!(target.contains("import a as a_2\n"), "{target}");
        assert!(target.contains("return a_2.helper(a_2.LIMIT)"), "{target}");
        assert!(
            target.contains("def untouched():\n    return a"),
            "{target}"
        );
        let caller = &result[Path::new("/proj/c.py")];
        assert!(caller.contains("from a import helper"), "{caller}");
        assert!(caller.contains("import b\n"), "{caller}");
        assert!(caller.contains("print(b.process(), helper(1))"), "{caller}");
    }

    #[test]
    fn explicit_from_import_preference_preserves_the_existing_move_plan() {
        let source = "VALUE = 1\n\ndef function():\n    return VALUE\n\nfunction()\n";
        let files = [("/proj/a.py", source), ("/proj/b.py", "")];
        let (db, loaded) = setup(&files);
        let original = move_symbol(
            &db,
            &loaded[0],
            at(source, "def function"),
            Path::new("/proj/b.py"),
            Path::new("/proj"),
            &paths(&files),
        )
        .unwrap();
        let explicit = move_symbol_with_import_style(
            &db,
            &loaded[0],
            at(source, "def function"),
            Path::new("/proj/b.py"),
            Path::new("/proj"),
            &paths(&files),
            true,
        )
        .unwrap();
        let mut first: BTreeMap<PathBuf, String> = files
            .iter()
            .map(|(path, text)| (PathBuf::from(path), text.to_string()))
            .collect();
        let mut second = first.clone();
        apply_file_ops(&mut first, &original);
        apply_file_ops(&mut second, &explicit);
        assert_eq!(first, second);
        assert!(first[Path::new("/proj/a.py")].contains("from b import function"));
        assert!(first[Path::new("/proj/b.py")].contains("from a import VALUE"));
    }

    #[test]
    fn move_symbol_carries_imports_and_updates_importers() {
        let a = "import os\nfrom typing import Optional\n\nLIMIT = 3\n\n\ndef helper(x):\n    return x * LIMIT\n\n\ndef process(path: Optional[str]):\n    return helper(os.path.basename(path))\n\n\nprint(process('x'))\n";
        let b = "import sys\n\n\ndef other():\n    pass\n";
        let c = "from a import process, helper\n\nprocess(helper(1))\n";
        let d = "from a import process\n";
        let files = [
            ("/proj/a.py", a),
            ("/proj/b.py", b),
            ("/proj/c.py", c),
            ("/proj/d.py", d),
        ];
        let (db, f) = setup(&files);
        let ops = move_symbol(
            &db,
            &f[0],
            at(a, "def process"),
            Path::new("/proj/b.py"),
            Path::new("/proj"),
            &paths(&files),
        )
        .unwrap();
        let mut fs: BTreeMap<PathBuf, String> = files
            .iter()
            .map(|(p, t)| (PathBuf::from(p), t.to_string()))
            .collect();
        apply_file_ops(&mut fs, &ops);
        assert_eq!(fs[Path::new("/proj/a.py")], "import os\nfrom typing import Optional\nfrom b import process\n\nLIMIT = 3\n\n\ndef helper(x):\n    return x * LIMIT\n\n\nprint(process('x'))\n");
        assert_eq!(fs[Path::new("/proj/b.py")], "import sys\nfrom a import helper\nfrom typing import Optional\nimport os\n\n\ndef other():\n    pass\n\n\ndef process(path: Optional[str]):\n    return helper(os.path.basename(path))\n");
        assert_eq!(
            fs[Path::new("/proj/c.py")],
            "from a import helper\nfrom b import process\n\nprocess(helper(1))\n"
        );
        assert_eq!(fs[Path::new("/proj/d.py")], "from b import process\n");
        // Moving into a new file creates it with the needed imports.
        let ops = move_symbol(
            &db,
            &f[0],
            at(a, "def process"),
            Path::new("/proj/new_mod.py"),
            Path::new("/proj"),
            &paths(&files),
        )
        .unwrap();
        let created = ops
            .iter()
            .find_map(|o| match o {
                FileOp::Create(p, t) if p == Path::new("/proj/new_mod.py") => Some(t.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(created, "from a import helper\nfrom typing import Optional\nimport os\n\n\ndef process(path: Optional[str]):\n    return helper(os.path.basename(path))\n");
        // Top-level variables are supported too: their clients must retain
        // the moved binding through a generated import, not a copied value.
        let ops = move_symbol(
            &db,
            &f[0],
            at(a, "LIMIT = 3"),
            Path::new("/proj/b.py"),
            Path::new("/proj"),
            &paths(&files),
        )
        .unwrap();
        let mut moved: BTreeMap<PathBuf, String> = files
            .iter()
            .map(|(path, text)| (PathBuf::from(path), text.to_string()))
            .collect();
        apply_file_ops(&mut moved, &ops);
        assert!(!moved[Path::new("/proj/a.py")].contains("LIMIT = 3"));
        assert!(moved[Path::new("/proj/a.py")].contains("from b import LIMIT"));
        assert!(moved[Path::new("/proj/a.py")].contains("return x * LIMIT"));
        assert!(moved[Path::new("/proj/b.py")].contains("LIMIT = 3"));
    }

    #[test]
    fn rename_module_rewrites_imports() {
        let util = "def f():\n    return 1\n";
        let main = "import util\nimport util as u\nfrom util import f\nfrom pkg import util as pu\n\nprint(util.f(), u.f(), f())\n";
        let pkg_init = "";
        let pkg_mod =
            "from . import util\nfrom .util import f\nfrom pkg.util import f as g\n\nutil.f()\n";
        let pkg_util = "def f():\n    return 2\n";
        let files = [
            ("/proj/util.py", util),
            ("/proj/main.py", main),
            ("/proj/pkg/__init__.py", pkg_init),
            ("/proj/pkg/mod.py", pkg_mod),
            ("/proj/pkg/util.py", pkg_util),
        ];
        let (db, _f) = setup(&files);
        let ops = rename_module(
            &db,
            Path::new("/proj"),
            Path::new("/proj/util.py"),
            "helpers",
            &paths(&files),
        )
        .unwrap();
        let mut fs: BTreeMap<PathBuf, String> = files
            .iter()
            .map(|(p, t)| (PathBuf::from(p), t.to_string()))
            .collect();
        apply_file_ops(&mut fs, &ops);
        assert_eq!(fs[Path::new("/proj/main.py")], "import helpers\nimport helpers as u\nfrom helpers import f\nfrom pkg import util as pu\n\nprint(helpers.f(), u.f(), f())\n");
        assert!(fs.contains_key(Path::new("/proj/helpers.py")));
        assert!(!fs.contains_key(Path::new("/proj/util.py")));
        // The package module keeps its relative imports (they point at pkg.util, not util).
        assert_eq!(fs[Path::new("/proj/pkg/mod.py")], pkg_mod);
        // Renaming the package-level module rewrites relative and absolute references.
        let ops = rename_module(
            &db,
            Path::new("/proj"),
            Path::new("/proj/pkg/util.py"),
            "tools",
            &paths(&files),
        )
        .unwrap();
        let mut fs: BTreeMap<PathBuf, String> = files
            .iter()
            .map(|(p, t)| (PathBuf::from(p), t.to_string()))
            .collect();
        apply_file_ops(&mut fs, &ops);
        assert_eq!(fs[Path::new("/proj/pkg/mod.py")], "from . import tools\nfrom .tools import f\nfrom pkg.tools import f as g\n\ntools.f()\n");
        assert_eq!(fs[Path::new("/proj/main.py")], "import util\nimport util as u\nfrom util import f\nfrom pkg import tools as pu\n\nprint(util.f(), u.f(), f())\n");
        assert!(fs.contains_key(Path::new("/proj/pkg/tools.py")));
    }

    #[test]
    fn module_import_sites_list_importers() {
        let util = "def f():\n    return 1\n";
        let main = "import util\nfrom util import f\nfrom pkg import util as pu\n\n\ndef late():\n    import util.sub\n";
        let pkg_mod = "from . import util\nfrom .util import f\n";
        let files = [
            ("/proj/util.py", util),
            ("/proj/main.py", main),
            ("/proj/pkg/__init__.py", ""),
            ("/proj/pkg/mod.py", pkg_mod),
            ("/proj/pkg/util.py", util),
        ];
        let (db, _f) = setup(&files);
        let lines = |sites: Vec<rustpy_semantic::usages::Usage>| {
            sites
                .iter()
                .map(|u| (u.path.display().to_string(), u.line + 1))
                .collect::<Vec<_>>()
        };
        let sites = module_import_sites(
            &db,
            Path::new("/proj"),
            Path::new("/proj/util.py"),
            &paths(&files),
        );
        assert_eq!(
            lines(sites),
            vec![
                ("/proj/main.py".to_string(), 1),
                ("/proj/main.py".to_string(), 2),
                ("/proj/main.py".to_string(), 7)
            ]
        );
        let sites = module_import_sites(
            &db,
            Path::new("/proj"),
            Path::new("/proj/pkg/util.py"),
            &paths(&files),
        );
        assert_eq!(
            lines(sites),
            vec![
                ("/proj/main.py".to_string(), 3),
                ("/proj/pkg/mod.py".to_string(), 1),
                ("/proj/pkg/mod.py".to_string(), 2)
            ]
        );
        // A package: only importers outside it.
        let sites = module_import_sites(
            &db,
            Path::new("/proj"),
            Path::new("/proj/pkg/__init__.py"),
            &paths(&files),
        );
        assert_eq!(lines(sites), vec![("/proj/main.py".to_string(), 3)]);
    }

    #[test]
    fn keyword_only_and_positional_only() {
        let src = "def connect(host, port, timeout=5):\n    pass\n\n\nconnect('h', 80, 3)\nconnect('h', port=81)\n";
        let (db, f) = setup(&[("/proj/m.py", src)]);
        let r = make_keyword_only(&db, &f[0], at(src, "def connect"), 1).unwrap();
        assert_eq!(apply_edits(src, &r.edits), "def connect(host, *, port, timeout=5):\n    pass\n\n\nconnect('h', port=80, timeout=3)\nconnect('h', port=81)\n");
        let r = make_positional_only(&db, &f[0], at(src, "def connect"), 1).unwrap();
        assert_eq!(apply_edits(src, &r.edits), "def connect(host, port, /, timeout=5):\n    pass\n\n\nconnect('h', 80, 3)\nconnect('h', 81)\n");
        let src2 = "class C:\n    def m(self, a, b):\n        pass\n\n\nC().m(1, b=2)\n";
        let (db, f) = setup(&[("/proj/m.py", src2)]);
        let r = make_positional_only(&db, &f[0], at(src2, "def m"), 2).unwrap();
        assert_eq!(
            apply_edits(src2, &r.edits),
            "class C:\n    def m(self, a, b, /):\n        pass\n\n\nC().m(1, 2)\n"
        );
        let r = make_keyword_only(&db, &f[0], at(src2, "def m"), 1).unwrap();
        assert_eq!(
            apply_edits(src2, &r.edits),
            "class C:\n    def m(self, *, a, b):\n        pass\n\n\nC().m(a=1, b=2)\n"
        );
        assert!(make_keyword_only(&db, &f[0], at(src2, "def m"), 0).is_err());
    }

    #[test]
    fn rename_parameter_updates_body_calls_and_docstring() {
        let src = "def greet(name, excited=False):\n    \"\"\"Say hi.\n\n    :param name: who\n    :param excited: loud\n    \"\"\"\n    msg = 'hi ' + name\n    return msg if not excited else msg.upper()\n\n\ngreet('bob', excited=True)\ngreet(name='ann')\n";
        let (db, f) = setup(&[("/proj/m.py", src)]);
        let r = rename_parameter(&db, &f[0], at(src, "def greet"), 0, "person").unwrap();
        let out = apply_edits(src, &r.edits);
        assert_eq!(out, "def greet(person, excited=False):\n    \"\"\"Say hi.\n\n    :param person: who\n    :param excited: loud\n    \"\"\"\n    msg = 'hi ' + person\n    return msg if not excited else msg.upper()\n\n\ngreet('bob', excited=True)\ngreet(person='ann')\n");
        for rr in &r.rename_ranges {
            assert_eq!(slice(&out, *rr), "person");
        }
        assert!(rename_parameter(&db, &f[0], at(src, "def greet"), 0, "excited").is_err());
    }

    #[test]
    fn set_default_adds_replaces_and_removes() {
        let src = "def f(a, b, c=1):\n    return a + b + c\n\n\nf(1, 2)\nf(1, 2, 3)\n";
        let (db, f) = setup(&[("/proj/m.py", src)]);
        let r = set_default(&db, &f[0], at(src, "def f"), 1, Some("0")).unwrap();
        assert_eq!(
            apply_edits(src, &r.edits),
            "def f(a, b=0, c=1):\n    return a + b + c\n\n\nf(1, 2)\nf(1, 2, 3)\n"
        );
        let r = set_default(&db, &f[0], at(src, "def f"), 2, Some("2")).unwrap();
        assert_eq!(
            apply_edits(src, &r.edits),
            "def f(a, b, c=2):\n    return a + b + c\n\n\nf(1, 2)\nf(1, 2, 3)\n"
        );
        assert!(set_default(&db, &f[0], at(src, "def f"), 0, Some("5")).is_err());
        let r = set_default(&db, &f[0], at(src, "def f"), 2, None).unwrap();
        assert_eq!(
            apply_edits(src, &r.edits),
            "def f(a, b, c):\n    return a + b + c\n\n\nf(1, 2, c)\nf(1, 2, 3)\n"
        );
        let src2 = "def g(a=1, b=2):\n    pass\n";
        let (db, f) = setup(&[("/proj/m.py", src2)]);
        assert!(set_default(&db, &f[0], at(src2, "def g"), 1, None).is_err());
        let r = set_default(&db, &f[0], at(src2, "def g"), 0, None).unwrap();
        assert_eq!(apply_edits(src2, &r.edits), "def g(a, b=2):\n    pass\n");
    }

    #[test]
    fn type_alias_introduce_and_inline() {
        let src = "import json\n\n\ndef load(path: str) -> dict[str, list[int]]:\n    data: dict[str, list[int]] = json.load(open(path))\n    return data\n";
        let (db, f) = setup(&[("/proj/m.py", src)]);
        let r = introduce_type_alias(&f[0], sel(src, "dict[str, list[int]]"), "Table").unwrap();
        let out = apply_edits(src, &r.edits);
        assert_eq!(out, "import json\n\n\nTable = dict[str, list[int]]\n\n\ndef load(path: str) -> Table:\n    data: Table = json.load(open(path))\n    return data\n");
        assert_eq!(r.rename_ranges.len(), 3);
        for rr in &r.rename_ranges {
            assert_eq!(slice(&out, *rr), "Table");
        }
        let (db2, f2) = setup(&[("/proj/n.py", &out)]);
        let r = inline_type_alias(&db2, &f2[0], at(&out, "Table:")).unwrap();
        assert_eq!(apply_edits(&out, &r.edits), "import json\n\n\ndef load(path: str) -> dict[str, list[int]]:\n    data: dict[str, list[int]] = json.load(open(path))\n    return data\n");
        let _ = db;
        assert!(introduce_type_alias(&f[0], sel(src, "json.load(open(path))"), "X").is_err());
    }

    #[test]
    fn convert_to_dataclass_from_trivial_init() {
        let src = "class Point:\n    \"\"\"A point.\"\"\"\n\n    def __init__(self, x: float, y: float = 0.0, tags=[], name='p'):\n        self.x = x\n        self.y = y\n        self.tags = tags\n        self.label = name\n\n    def norm(self):\n        return self.x\n";
        let (db, f) = setup(&[("/proj/m.py", src)]);
        let r = convert_to_dataclass(&db, &f[0], at(src, "class Point")).unwrap();
        assert_eq!(apply_edits(src, &r.edits), "from dataclasses import dataclass, field\n\n\n@dataclass\nclass Point:\n    \"\"\"A point.\"\"\"\n\n    x: float\n    y: float = 0.0\n    tags: list = field(default_factory=list)\n    label: str = 'p'\n\n    def norm(self):\n        return self.x\n");
        let src2 = "class C:\n    def __init__(self, a):\n        self.a = a\n        print(a)\n";
        let (db, f) = setup(&[("/proj/m.py", src2)]);
        assert!(convert_to_dataclass(&db, &f[0], at(src2, "class C")).is_err());
    }

    #[test]
    fn convert_to_dataclass_document_edits() {
        use rustpy_text::{Document, Edit as DocumentEdit, EditGroup};

        let cases = [
            ("no_imports", "class Person:\n    def __init__(self, name: str):\n        self.name = name\n", "from dataclasses import dataclass\n\n\n@dataclass\nclass Person:\n    name: str\n"),
            ("existing_import", "import os\n\nclass Person:\n    def __init__(self, name: str):\n        self.name = name\n", "import os\nfrom dataclasses import dataclass\n\n@dataclass\nclass Person:\n    name: str\n"),
            ("existing_dataclass", "from dataclasses import dataclass\n\nclass Person:\n    def __init__(self, name: str):\n        self.name = name\n", "from dataclasses import dataclass\n\n@dataclass\nclass Person:\n    name: str\n"),
            ("nested", "class Outer:\n    class Person:\n        def __init__(self, name: str):\n            self.name = name\n", "from dataclasses import dataclass\n\n\nclass Outer:\n    @dataclass\n    class Person:\n        name: str\n"),
            ("needs_any", "class Person:\n    def __init__(self, name):\n        self.name = name\n", "from typing import Any\n\n\nfrom dataclasses import dataclass\n\n\n@dataclass\nclass Person:\n    name: Any\n"),
            ("needs_any_and_field", "class Person:\n    def __init__(self, name, tags=[]):\n        self.name = name\n        self.tags = tags\n", "from typing import Any\n\n\nfrom dataclasses import dataclass, field\n\n\n@dataclass\nclass Person:\n    name: Any\n    tags: list = field(default_factory=list)\n"),
            ("merge_existing_field_import", "from dataclasses import dataclass\n\nclass Person:\n    def __init__(self, tags=[]):\n        self.tags = tags\n", "from dataclasses import dataclass, field\n\n@dataclass\nclass Person:\n    tags: list = field(default_factory=list)\n"),
        ];
        for (name, src, expected) in cases {
            let (db, files) = setup(&[("/proj/m.py", src)]);
            let result = convert_to_dataclass(&db, &files[0], at(src, "class Person")).unwrap();
            let mut document = Document::new(src);
            let edits = result
                .edits
                .iter()
                .map(|(range, text)| DocumentEdit::replace(*range, text.clone()))
                .collect();
            document
                .edit(edits, &[], &[], EditGroup::Separate, Some(&result.title))
                .unwrap();
            assert_eq!(document.text(), expected, "{name}");
            assert_eq!(apply_edits(src, &result.edits), expected, "helper: {name}");
            document.undo().unwrap();
            assert_eq!(document.text(), src, "undo: {name}");
            document.redo().unwrap();
            assert_eq!(document.text(), expected, "redo: {name}");
        }
    }

    #[test]
    fn add_type_hints_from_defaults_and_returns() {
        let src = "def scale(value, factor=2, label='x'):\n    return value * factor\n\n\ndef greet(name):\n    return 'hi ' + name\n";
        let (db, f) = setup(&[("/proj/m.py", src)]);
        let r = add_type_hints(&db, &f[0], at(src, "def scale")).unwrap();
        let out = apply_edits(src, &r.edits);
        assert!(
            out.starts_with("def scale(value, factor: int = 2, label: str = 'x')"),
            "{out}"
        );
        let r = add_type_hints(&db, &f[0], at(src, "def greet")).unwrap();
        assert_eq!(apply_edits(src, &r.edits), "def scale(value, factor=2, label='x'):\n    return value * factor\n\n\ndef greet(name) -> str:\n    return 'hi ' + name\n");
        let src2 = "def done(a: int) -> int:\n    return a\n";
        let (db, f) = setup(&[("/proj/m.py", src2)]);
        assert!(add_type_hints(&db, &f[0], at(src2, "def done")).is_err());
    }
}
