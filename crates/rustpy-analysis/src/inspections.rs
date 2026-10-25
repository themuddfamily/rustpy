//! Inspections with quick fixes (roadmap §2.5, §2.6): unused imports and
//! variables, a PEP 8 subset, import helpers (Optimize Imports, Add Import),
//! create-from-usage fixes, call/type checking, version compatibility, code
//! smells, security and naming checks.

#[cfg(test)]
mod batch2_tests;
mod compat;
mod core_names;
mod create_from_usage;
mod deprecation;
pub mod framework_tools;
#[cfg(test)]
mod install_tests;
mod naming;
mod references_names;
mod security;
mod smells;
mod structural_search;
mod style;
mod suppress;
mod type_capability_checks;
mod type_check;

pub use suppress::Suppressions;
pub mod infrastructure;

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use rayon::prelude::*;
use rustpy_core::{
    CancellationToken, Cancelled, Diagnostic, ProgressReporter, Severity, TextRange, TextSize,
};
use rustpy_syntax::ast::{self, AstNode};
use rustpy_syntax::{SyntaxKind, SyntaxNode};

use rustpy_semantic::db::{Analysis, FileInfo};
use rustpy_semantic::scope::{BindingKind, ScopeKind};

pub use core_names::RENAME_ELEMENT_FIX;

/// A text edit inside the inspected file.
pub use rustpy_semantic::edit_helpers::Edit;
pub use rustpy_semantic::import_placement::import_insert_offset;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuickFix {
    pub title: String,
    pub edits: Vec<Edit>,
}

impl QuickFix {
    /// "Suppress … for statement/function/class" and `# noqa` fixes.
    pub fn is_suppression(&self) -> bool {
        self.title.starts_with("Suppress ")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub diagnostic: Diagnostic,
    pub fixes: Vec<QuickFix>,
}

impl Problem {
    /// The fix that resolves the problem (never a suppression comment).
    pub fn primary_fix(&self) -> Option<&QuickFix> {
        self.fixes.iter().find(|fix| !fix.is_suppression())
    }
}

#[derive(Clone, Debug)]
pub struct InspectionOptions {
    pub policy: Option<infrastructure::Policy>,
    pub max_line_length: usize,
    pub editorconfig_enabled: bool,
    /// Immutable per-file settings captured by an asynchronous caller.
    pub editorconfig: Option<Arc<rustpy_settings::editorconfig::EditorConfigProps>>,
    pub style: rustpy_settings::style_inspections::StyleInspections,
    /// User-saved structural templates explicitly enabled as inspections.
    pub structural_rules: Arc<Vec<rustpy_search::structural::Template>>,
    pub pep8: bool,
    pub unused_imports: bool,
    pub unused_variables: bool,
    /// Project symbol index `(name, module)` for "Add import" fixes (shared:
    /// it can hold every symbol of a large project).
    pub project_symbols: Arc<Vec<(String, String)>>,
    /// Call argument / type compatibility checks (`PyArgumentList`, `PyTypeChecker`, …).
    pub type_checker: bool,
    /// `PySecurity*` weak warnings.
    pub security: bool,
    /// Captured project/interpreter/package state; no processes run during inspection.
    pub framework: Arc<framework_tools::Inputs>,
    /// Python version compatibility and Python 2 syntax (`PyCompatibility`).
    pub compatibility: bool,
    /// Code smells: redeclarations, unreachable code, mutable defaults, … .
    pub code_smells: bool,
    /// PEP 8 naming conventions (`PyPep8Naming`).
    pub naming: bool,
    /// Missing docstrings (`PyMissingOrEmptyDocstring`); off by default.
    pub missing_docstrings: bool,
    /// Docstrings that disagree with the signature (`PyIncorrectDocstring`).
    pub incorrect_docstrings: bool,
    /// Uses of `@deprecated` functions, classes and overloads (`PyDeprecation`).
    pub deprecation: bool,
    /// Probable bugs: unbound locals, shadowed outer names, protected member
    /// access, attributes defined outside `__init__`, abstract classes,
    /// coroutines that are not awaited.
    pub probable_bugs: bool,
    /// Target interpreter version for compatibility checks.
    pub python_version: (u32, u32),
}

impl Default for InspectionOptions {
    fn default() -> Self {
        Self {
            policy: None,
            max_line_length: 120,
            editorconfig_enabled: true,
            editorconfig: None,
            style: Default::default(),
            structural_rules: Arc::default(),
            pep8: true,
            unused_imports: true,
            unused_variables: true,
            project_symbols: Arc::default(),
            type_checker: true,
            security: true,
            framework: Arc::default(),
            compatibility: true,
            code_smells: true,
            naming: true,
            missing_docstrings: false,
            incorrect_docstrings: true,
            deprecation: true,
            python_version: (3, 12),
            probable_bugs: true,
        }
    }
}

pub(crate) fn ts(n: usize) -> TextSize {
    TextSize::new(n as u32)
}

/// Byte offset of the start of the line containing `off`.
pub(crate) fn line_start(text: &str, off: usize) -> usize {
    // Callers pass `end - 1` style offsets that can fall inside a multi-byte
    // character; a line break is never part of one.
    text[..rustpy_core::floor_char_boundary(text, off)]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(0)
}

/// Byte offset just after the newline ending the line containing `off`
/// (or the text length).
pub(crate) fn line_end_incl(text: &str, off: usize) -> usize {
    let off = rustpy_core::floor_char_boundary(text, off);
    text[off..]
        .find('\n')
        .map(|i| off + i + 1)
        .unwrap_or(text.len())
}

/// Leading whitespace of the line containing `off`.
pub(crate) fn indent_at(text: &str, off: usize) -> String {
    let ls = line_start(text, off);
    text[ls..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect()
}

/// Range of the whole line(s) spanned by `range`, including the final newline.
pub(crate) fn full_lines(text: &str, range: TextRange) -> TextRange {
    let s = line_start(text, usize::from(range.start()));
    let last = usize::from(range.end()).max(usize::from(range.start()));
    let e = line_end_incl(text, last.saturating_sub(1).max(s));
    TextRange::new(ts(s), ts(e.max(s)))
}

/// The smallest statement node containing `node` (including itself).
pub(crate) fn enclosing_stmt(node: &SyntaxNode) -> Option<SyntaxNode> {
    node.ancestors().find(|n| ast::Stmt::can_cast(n.kind()))
}

/// Scope owning `node`, found through the syntax tree (robust at block
/// boundaries where offsets are ambiguous).
pub(crate) fn scope_of_node(file: &FileInfo, node: &SyntaxNode) -> rustpy_semantic::scope::ScopeId {
    for anc in node.ancestors().skip(1) {
        if matches!(
            anc.kind(),
            SyntaxKind::FUNC_DEF
                | SyntaxKind::LAMBDA_EXPR
                | SyntaxKind::CLASS_DEF
                | SyntaxKind::LIST_COMP
                | SyntaxKind::SET_COMP
                | SyntaxKind::DICT_COMP
                | SyntaxKind::GENERATOR_EXPR
                | SyntaxKind::MODULE
        ) {
            if let Some(s) = file.scopes.scope_for_node(anc.text_range()) {
                return s;
            }
        }
    }
    file.scopes.module_scope()
}

/// Token exactly at `range`, if any.
pub(crate) fn token_at(root: &SyntaxNode, range: TextRange) -> Option<rustpy_syntax::SyntaxToken> {
    let at = rustpy_syntax::token_at_offset(root, range.start());
    at.clone()
        .right_biased()
        .filter(|t| t.text_range() == range)
        .or_else(|| at.left_biased().filter(|t| t.text_range() == range))
}

/// Where a statement can be inserted at the start of a block (after the
/// docstring): byte offset of a line start plus the block indentation.
/// `None` for single-line bodies (`def f(): pass`).
pub(crate) fn body_insert_point(text: &str, block: &ast::Block) -> Option<(usize, String)> {
    let mut stmts = block.statements();
    let first = stmts.next()?;
    let first_start: usize = first.text_range().start().into();
    let ls = line_start(text, first_start);
    if !text[ls..first_start].trim().is_empty() {
        // Statement shares a line with the header (`def f(): pass`).
        return None;
    }
    let indent = indent_at(text, first_start);
    if block.docstring().is_some() {
        if let Some(second) = stmts.next() {
            let s: usize = second.text_range().start().into();
            return Some((line_start(text, s), indent));
        }
        let end = line_end_incl(
            text,
            usize::from(first.text_range().end())
                .saturating_sub(1)
                .max(first_start),
        );
        return Some((end, indent));
    }
    Some((ls, indent))
}

/// Collects problems (suppression comments are applied by [`inspect`]).
pub(crate) struct Sink {
    pub out: Vec<Problem>,
}

impl Sink {
    pub(crate) fn new() -> Self {
        Self { out: Vec::new() }
    }
    pub(crate) fn push(
        &mut self,
        range: TextRange,
        severity: Severity,
        code: &str,
        message: impl Into<String>,
        fixes: Vec<QuickFix>,
    ) {
        self.out.push(Problem {
            diagnostic: Diagnostic::new(range, severity, code, message),
            fixes,
        });
    }
}

/// Runs all inspections on `file`.
pub fn inspect(db: &Analysis, file: &Arc<FileInfo>, opts: &InspectionOptions) -> Vec<Problem> {
    inspect_cancellable(db, file, opts, &CancellationToken::new()).unwrap_or_default()
}

/// [`inspect`] for background analysis: `token` is checked between the
/// inspection passes, so an edit that makes the run obsolete stops it early.
pub fn inspect_cancellable(
    db: &Analysis,
    file: &Arc<FileInfo>,
    opts: &InspectionOptions,
    token: &CancellationToken,
) -> Result<Vec<Problem>, Cancelled> {
    let module_resolver = db.resolver.for_path(&file.path);
    let mut module_options;
    let opts = if !std::ptr::eq(module_resolver, &db.resolver) {
        module_options = opts.clone();
        module_options.python_version = module_resolver.python_version;
        &module_options
    } else {
        opts
    };
    let text: &str = &file.text;
    let mut out: Vec<Problem> = Vec::new();
    token.check()?;
    // Semantic + syntax diagnostics with import fixes for unresolved names.
    for d in rustpy_semantic::features::diagnostics(db, file) {
        let mut fixes = Vec::new();
        if d.code == "PyUnresolvedReferences" && d.message.starts_with("Unresolved reference '") {
            let name = d
                .message
                .trim_start_matches("Unresolved reference '")
                .trim_end_matches('\'');
            fixes.extend(import_fixes(db, file, name, &opts.project_symbols));
        }
        if !file.is_stub && d.code == "PyUnresolvedReferences" {
            fixes.extend(install_package_fix(db, file, &d));
            fixes.extend(create_from_usage::fixes_for(file, &d));
        }
        if d.code == "syntax" && d.message == "use '!=' instead of '<>'" {
            fixes.push(QuickFix {
                title: "Replace with '!='".into(),
                edits: vec![(d.range, "!=".into())],
            });
        }
        out.push(Problem {
            diagnostic: d,
            fixes,
        });
    }
    if file.is_stub {
        structural_search::run(db, file, &opts.structural_rules, token, &mut out)?;
        return Ok(out);
    }
    let mut sink = Sink::new();
    // One inferer for every inspection: class, MRO and signature caches are shared.
    let inf = rustpy_semantic::infer::Inferer::new(db);
    if opts.type_checker {
        token.check()?;
        type_check::run(&inf, file, opts.python_version, &mut sink);
        token.check()?;
        type_capability_checks::run(&inf, file, &mut sink);
    }
    if opts.compatibility {
        token.check()?;
        compat::run(file, opts.python_version, &mut sink);
    }
    if opts.code_smells {
        token.check()?;
        smells::run(&inf, file, &mut sink);
    }
    if opts.probable_bugs {
        core_names::run(&inf, file, &mut sink);
    }
    if opts.security {
        token.check()?;
        security::run(&inf, file, opts, &mut sink);
    }
    framework_tools::run(&inf, file, opts, &mut sink);
    if opts.naming {
        token.check()?;
        naming::run(&inf, file, &style::settings(opts), &mut sink);
    }
    if opts.missing_docstrings && db.analyze_doc_comments() {
        naming::docstrings(file, &style::settings(opts), &mut sink);
    }
    if opts.deprecation {
        deprecation::run(db, file, opts.python_version, &mut sink);
    }
    style::run(&inf, file, opts, &mut sink);
    references_names::run(&inf, file, opts, &mut sink);
    out.extend(sink.out);
    token.check()?;
    if opts.unused_imports && !file.is_package_init() {
        out.extend(unused_imports_with(db, file));
    }
    if opts.unused_variables {
        out.extend(unused_variables(file));
    }
    if opts.pep8 {
        token.check()?;
        out.extend(style::pep8(file, opts, token));
    }
    if opts.incorrect_docstrings && db.analyze_doc_comments() {
        for (range, message) in rustpy_semantic::docstrings::docstring_problems(db, file) {
            out.push(Problem {
                diagnostic: Diagnostic::new(
                    range,
                    Severity::WeakWarning,
                    "PyIncorrectDocstring",
                    message,
                ),
                fixes: Vec::new(),
            });
        }
    }
    token.check()?;
    structural_search::run(db, file, &opts.structural_rules, token, &mut out)?;
    let _ = text;
    // `# noinspection`, `# noqa` and `# type: ignore` comments.
    let suppressions = Suppressions::new(file);
    out.retain(|p| !suppressions.is_suppressed(&p.diagnostic));
    deprecation::drop_removed_module_duplicates(&mut out);
    for (index, p) in out.iter_mut().enumerate() {
        // A file can have thousands of problems: stay responsive to edits.
        if index % 64 == 0 {
            token.check()?;
        }
        p.fixes
            .extend(suppress::fixes(&suppressions, file, &p.diagnostic));
    }
    if let Some(policy) = &opts.policy {
        policy.apply(&file.path, &mut out);
    }
    out.sort_by_key(|p| p.diagnostic.range.start());
    Ok(out)
}

/// Inspects `paths` in parallel across the cores of the background scan
/// pool (Problems › Project Errors, batch inspection; see
/// [`rustpy_core::parallel`]): `(path, problems)` for the files with
/// problems, in the order of `paths`. Reports the files done to `progress`
/// and stops with `Err(Cancelled)` once it is cancelled (the token is also
/// checked between the inspection passes of each file).
pub fn inspect_files(
    db: &Analysis,
    paths: &[PathBuf],
    opts: &InspectionOptions,
    progress: &ProgressReporter,
) -> Result<Vec<(PathBuf, Vec<Problem>)>, Cancelled> {
    rustpy_core::parallel::in_background_pool(|| inspect_files_here(db, paths, opts, progress))
}

fn inspect_files_here(
    db: &Analysis,
    paths: &[PathBuf],
    opts: &InspectionOptions,
    progress: &ProgressReporter,
) -> Result<Vec<(PathBuf, Vec<Problem>)>, Cancelled> {
    // Every file is queried many times during the pass; verify disk content
    // at most once per recheck interval rather than on every access.
    let _batch = db.batch_reads();
    let total = paths.len();
    let done = AtomicUsize::new(0);
    let per_file: Vec<Option<(PathBuf, Vec<Problem>)>> = paths
        .par_iter()
        .map(|path| {
            if progress.is_cancelled() {
                return None;
            }
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            if n % 16 == 1 || n == total {
                progress.set_step(n, total);
                progress.set_details(path.display().to_string());
            }
            let file = db.file_for_path(path)?;
            let problems = inspect_cancellable(db, &file, opts, progress.token()).ok()?;
            (!problems.is_empty()).then(|| (path.clone(), problems))
        })
        .collect();
    progress.check()?;
    Ok(per_file.into_iter().flatten().collect())
}

/// "Install package 'PyYAML'" for `No module named 'yaml'`, when neither the
/// stdlib nor the project or interpreter has the package the distribution
/// installs (a missing submodule of an installed package is not a missing
/// package; for a namespace package such as `google.cloud.firestore` that is
/// the distribution's own subpackage, not the shared `google`).
fn install_package_fix(db: &Analysis, file: &FileInfo, d: &Diagnostic) -> Option<QuickFix> {
    let module = d
        .message
        .strip_prefix("No module named '")?
        .strip_suffix('\'')?;
    let top = module.split('.').next().filter(|t| !t.is_empty())?;
    if rustpy_semantic::typeshed::is_stdlib(top) {
        return None;
    }
    let provider = rustpy_packages::provider_for_module(module).or_else(|| {
        // `from google.cloud import firestore`: the namespace's subpackage is the imported name.
        let imp = file
            .parse
            .syntax_node()
            .covering_element(d.range)
            .parent()?
            .ancestors()
            .find_map(ast::ImportFromStmt::cast)?;
        let first = imp.aliases().next()?.name();
        rustpy_packages::provider_for_module(&format!("{module}.{first}"))
    })?;
    if db.import_module(&provider.package).is_some() {
        return None;
    }
    Some(QuickFix {
        title: install_package_title(&provider.distribution),
        edits: Vec::new(),
    })
}

/// Title of the quick fix that installs the distribution `dist`; the UI runs
/// pip for it (see [`install_package_target`]).
pub fn install_package_title(dist: &str) -> String {
    format!("Install package '{dist}'")
}

/// The distribution an "Install package" quick fix installs.
pub fn install_package_target(title: &str) -> Option<&str> {
    title.strip_prefix("Install package '")?.strip_suffix('\'')
}

/// Unused import bindings after inspection suppression policy.
fn unused_import_bindings(file: &Arc<FileInfo>) -> Vec<(TextRange, TextRange, String, String)> {
    unused_import_bindings_from(
        file,
        rustpy_semantic::imports::unused_import_candidates(file),
    )
}

fn unused_import_bindings_with(
    db: &Analysis,
    file: &Arc<FileInfo>,
) -> Vec<(TextRange, TextRange, String, String)> {
    unused_import_bindings_from(
        file,
        rustpy_semantic::imports::unused_import_candidates_with(db, file),
    )
}

fn unused_import_bindings_from(
    file: &Arc<FileInfo>,
    candidates: Vec<rustpy_semantic::imports::UnusedImport>,
) -> Vec<(TextRange, TextRange, String, String)> {
    let suppressions = Suppressions::new(file);
    candidates
        .into_iter()
        .filter(|b| {
            !suppressions.is_suppressed(&Diagnostic::new(
                b.range,
                Severity::Warning,
                "PyUnusedImports",
                "",
            ))
        })
        .map(|b| {
            let what = if b.is_from {
                format!("Unused import '{}'", b.name)
            } else {
                format!("Unused import statement 'import {}'", b.module)
            };
            (b.range, b.node_range, what, b.name)
        })
        .collect()
}

/// Import binding ranges that may be removed, respecting inspection suppressions.
pub fn unused_import_ranges(file: &Arc<FileInfo>) -> Vec<TextRange> {
    unused_import_bindings(file)
        .into_iter()
        .map(|(range, _, _, _)| range)
        .collect()
}

/// Current imported export facts, retaining the same suppression/optimizer policy.
pub fn unused_import_ranges_with(db: &Analysis, file: &Arc<FileInfo>) -> Vec<TextRange> {
    unused_import_bindings_with(db, file)
        .into_iter()
        .map(|(range, _, _, _)| range)
        .collect()
}

#[cfg(test)]
fn unused_imports(file: &Arc<FileInfo>) -> Vec<Problem> {
    unused_import_problems(file, unused_import_bindings(file))
}

fn unused_imports_with(db: &Analysis, file: &Arc<FileInfo>) -> Vec<Problem> {
    unused_import_problems(file, unused_import_bindings_with(db, file))
}

fn unused_import_problems(
    file: &Arc<FileInfo>,
    bindings: Vec<(TextRange, TextRange, String, String)>,
) -> Vec<Problem> {
    let mut out = Vec::new();
    let ranges: Vec<TextRange> = bindings.iter().map(|(range, _, _, _)| *range).collect();
    let optimize = rustpy_format::imports::optimize_imports(file, &ranges);
    for (range, node_range, what, name) in bindings {
        let fix = remove_import_fix(file, &node_range);
        let mut fixes = vec![QuickFix {
            title: format!("Remove unused import '{name}'"),
            edits: fix,
        }];
        if !optimize.is_empty() {
            fixes.push(QuickFix {
                title: "Optimize imports".into(),
                edits: optimize.clone(),
            });
        }
        out.push(Problem {
            diagnostic: Diagnostic::new(range, Severity::Warning, "PyUnusedImports", what),
            fixes,
        });
    }
    out
}

/// Edits removing one import alias (or the whole statement if it is the only one).
fn remove_import_fix(file: &FileInfo, alias_range: &TextRange) -> Vec<Edit> {
    let root = file.parse.syntax_node();
    let start_node = match root.covering_element(*alias_range) {
        rustpy_syntax::rowan::NodeOrToken::Node(n) => Some(n),
        rustpy_syntax::rowan::NodeOrToken::Token(t) => t.parent(),
    };
    let Some(alias) =
        start_node.and_then(|n| n.ancestors().find(|a| a.kind() == SyntaxKind::IMPORT_ALIAS))
    else {
        return Vec::new();
    };
    let Some(stmt) = alias.parent() else {
        return Vec::new();
    };
    let aliases: Vec<_> = stmt
        .children()
        .filter(|c| c.kind() == SyntaxKind::IMPORT_ALIAS)
        .collect();
    let text: &str = &file.text;
    if aliases.len() <= 1 {
        // Remove the whole line (including its newline).
        let start: usize = stmt.text_range().start().into();
        let end: usize = stmt.text_range().end().into();
        let line_start = text[..start].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line_end = text[end..]
            .find('\n')
            .map(|i| end + i + 1)
            .unwrap_or(text.len());
        return vec![(TextRange::new(ts(line_start), ts(line_end)), String::new())];
    }
    // Remove `alias,` or `, alias`.
    let idx = aliases.iter().position(|a| a == &alias).unwrap_or(0);
    let (start, end) = if idx + 1 < aliases.len() {
        (
            alias.text_range().start(),
            aliases[idx + 1].text_range().start(),
        )
    } else {
        (
            aliases[idx - 1].text_range().end(),
            alias.text_range().end(),
        )
    };
    vec![(TextRange::new(start, end), String::new())]
}

fn unused_variables(file: &Arc<FileInfo>) -> Vec<Problem> {
    let mut out = Vec::new();
    for (i, b) in file.scopes.bindings.iter().enumerate() {
        if !matches!(
            b.kind,
            BindingKind::Assignment | BindingKind::Walrus | BindingKind::AugAssignment
        ) {
            continue;
        }
        let scope = file.scopes.scope(b.scope);
        if scope.kind != ScopeKind::Function || b.name.starts_with('_') {
            continue;
        }
        if scope.globals.iter().any(|g| g == &b.name)
            || scope.nonlocals.iter().any(|g| g == &b.name)
        {
            continue;
        }
        let id = rustpy_semantic::scope::BindingId(i as u32);
        // Used if any reference in the scope resolves to any binding of this name in the scope
        // (flow-insensitive: a later read counts for all assignments).
        let ids = file.scopes.bindings_in(b.scope, &b.name);
        let used = file.scopes.references.iter().any(|r| {
            r.resolved.is_some_and(|rid| ids.contains(&rid)) && r.range.start() > b.range.end()
        });
        let used_anywhere = file
            .scopes
            .references
            .iter()
            .any(|r| r.resolved.is_some_and(|rid| ids.contains(&rid)));
        if used || (used_anywhere && b.kind == BindingKind::AugAssignment) {
            continue;
        }
        // Loop-carried / later reads before a reassignment are covered by `used_anywhere` for the first binding.
        if used_anywhere && ids.first() == Some(&id) {
            continue;
        }
        out.push(Problem {
            diagnostic: Diagnostic::new(
                b.range,
                Severity::WeakWarning,
                "PyUnusedLocal",
                format!("Local variable '{}' value is not used", b.name),
            ),
            fixes: Vec::new(),
        });
    }
    out
}

fn pep8(file: &Arc<FileInfo>, max_len: usize) -> Vec<Problem> {
    let text: &str = &file.text;
    let mut out = Vec::new();
    let mut push = |range: TextRange, code: &str, msg: String, fixes: Vec<QuickFix>| {
        out.push(Problem {
            diagnostic: Diagnostic::new(range, Severity::WeakWarning, format!("PEP8:{code}"), msg),
            fixes,
        });
    };
    let mut offset = 0usize;
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut blank_run = 0usize;
    let mut prev_nonblank: Option<&str> = None;
    for (i, raw) in lines.iter().enumerate() {
        let line = raw.trim_end_matches(['\n', '\r']);
        let start = offset;
        offset += raw.len();
        let trimmed = line.trim_end();
        // W291/W293 trailing whitespace.
        if trimmed.len() < line.len() {
            let r = TextRange::new(ts(start + trimmed.len()), ts(start + line.len()));
            let code = if trimmed.is_empty() { "W293" } else { "W291" };
            push(
                r,
                code,
                if trimmed.is_empty() {
                    "PEP 8: W293 whitespace on blank line".into()
                } else {
                    "PEP 8: W291 trailing whitespace".into()
                },
                vec![QuickFix {
                    title: "Remove trailing whitespace".into(),
                    edits: vec![(r, String::new())],
                }],
            );
        }
        // E501 line too long.
        let cols = line.chars().count();
        if cols > max_len {
            let byte = line
                .char_indices()
                .nth(max_len)
                .map(|(b, _)| b)
                .unwrap_or(line.len());
            push(
                TextRange::new(ts(start + byte), ts(start + line.len())),
                "E501",
                format!("PEP 8: E501 line too long ({cols} > {max_len} characters)"),
                Vec::new(),
            );
        }
        let t = line.trim_start();
        if trimmed.is_empty() {
            blank_run += 1;
            continue;
        }
        // E302/E305/E303 blank lines around top-level definitions.
        let indent = line.len() - t.len();
        let is_def = (t.starts_with("def ")
            || t.starts_with("async def ")
            || t.starts_with("class ")
            || t.starts_with('@'))
            && indent == 0;
        let prev_is_decorator = prev_nonblank
            .map(|p| p.trim_start().starts_with('@'))
            .unwrap_or(false);
        if is_def
            && i > 0
            && !prev_is_decorator
            && prev_nonblank.is_some()
            && blank_run < 2
            && !prev_nonblank
                .map(|p| p.trim_start().starts_with('#'))
                .unwrap_or(false)
        {
            let r = TextRange::new(
                ts(start),
                ts(start + t.split_whitespace().next().map(|w| w.len()).unwrap_or(1)),
            );
            let insert = "\n".repeat(2 - blank_run);
            push(
                r,
                "E302",
                format!("PEP 8: E302 expected 2 blank lines, got {blank_run}"),
                vec![QuickFix {
                    title: "Insert blank lines".into(),
                    edits: vec![(TextRange::empty(ts(start)), insert)],
                }],
            );
        }
        if blank_run > 2 && indent == 0 {
            push(
                TextRange::new(ts(start), ts(start + 1)),
                "E303",
                format!("PEP 8: E303 too many blank lines ({blank_run})"),
                Vec::new(),
            );
        }
        blank_run = 0;
        prev_nonblank = Some(line);
        // E261 inline comment needs two spaces; E262 inline comment should start with '# '.
        if let Some(pos) = find_inline_comment(line) {
            let before = &line[..pos];
            if !before.is_empty() && !before.ends_with("  ") {
                let r = TextRange::new(ts(start + before.trim_end().len()), ts(start + pos));
                push(
                    r,
                    "E261",
                    "PEP 8: E261 at least two spaces before inline comment".into(),
                    vec![QuickFix {
                        title: "Fix spacing before comment".into(),
                        edits: vec![(r, "  ".into())],
                    }],
                );
            }
        }
        // E711/E712 comparisons to None/True/False.
        for (pat, repl, code, what) in [
            ("== None", "is None", "E711", "None"),
            ("!= None", "is not None", "E711", "None"),
            ("== True", "is True", "E712", "True"),
            ("== False", "is False", "E712", "False"),
        ] {
            let code_part = strip_strings_and_comments(line);
            let mut from = 0;
            while let Some(p) = code_part[from..].find(pat) {
                let abs = from + p;
                let r = TextRange::new(ts(start + abs), ts(start + abs + pat.len()));
                push(
                    r,
                    code,
                    format!("PEP 8: {code} comparison to {what} should be '{repl}'"),
                    vec![QuickFix {
                        title: format!("Replace with '{repl}'"),
                        edits: vec![(r, repl.into())],
                    }],
                );
                from = abs + pat.len();
            }
        }
        // E401 multiple imports on one line.
        if t.starts_with("import ") && t.contains(',') && !t.contains('(') {
            let names: Vec<&str> = t["import ".len()..].split(',').map(str::trim).collect();
            let repl = names
                .iter()
                .map(|n| format!("{}import {n}", " ".repeat(indent)))
                .collect::<Vec<_>>()
                .join("\n");
            let r = TextRange::new(ts(start), ts(start + line.len()));
            push(
                TextRange::new(ts(start + indent), ts(start + indent + 6)),
                "E401",
                "PEP 8: E401 multiple imports on one line".into(),
                vec![QuickFix {
                    title: "Split imports".into(),
                    edits: vec![(r, repl)],
                }],
            );
        }
        // E231 missing whitespace after ','.
        let code_part = strip_strings_and_comments(line);
        for (idx, ch) in code_part.char_indices() {
            if ch == ',' {
                let next = code_part[idx + 1..].chars().next();
                if let Some(n) = next {
                    if !n.is_whitespace() && n != ')' && n != ']' && n != '}' {
                        let r = TextRange::new(ts(start + idx), ts(start + idx + 1));
                        push(
                            r,
                            "E231",
                            "PEP 8: E231 missing whitespace after ','".into(),
                            vec![QuickFix {
                                title: "Insert space".into(),
                                edits: vec![(TextRange::empty(ts(start + idx + 1)), " ".into())],
                            }],
                        );
                    }
                }
            }
        }
        // E225 missing whitespace around comparison/assignment operators (simple heuristic).
        for op in ["==", "!=", "<=", ">=", "+=", "-=", "*=", "/="] {
            let mut from = 0;
            while let Some(p) = code_part[from..].find(op) {
                let abs = from + p;
                let before = code_part[..abs].chars().last();
                let after = code_part[abs + op.len()..].chars().next();
                if before.is_some_and(|c| !c.is_whitespace())
                    || after.is_some_and(|c| !c.is_whitespace())
                {
                    let r = TextRange::new(ts(start + abs), ts(start + abs + op.len()));
                    push(
                        r,
                        "E225",
                        format!("PEP 8: E225 missing whitespace around operator '{op}'"),
                        vec![QuickFix {
                            title: "Add spaces around operator".into(),
                            edits: vec![(r, format!(" {op} "))],
                        }],
                    );
                }
                from = abs + op.len();
            }
        }
    }
    if !text.is_empty() && !text.ends_with('\n') {
        push(
            TextRange::empty(ts(text.len())),
            "W292",
            "PEP 8: W292 no newline at end of file".into(),
            vec![QuickFix {
                title: "Add newline".into(),
                edits: vec![(TextRange::empty(ts(text.len())), "\n".into())],
            }],
        );
    }
    out
}

/// Position of an inline `#` comment (outside strings), if any.
pub(crate) fn find_inline_comment(line: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    let mut prev = '\0';
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) => {
                if c == q && prev != '\\' {
                    quote = None;
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    quote = Some(c);
                } else if c == '#' {
                    return if i == 0 || line[..i].trim().is_empty() {
                        None
                    } else {
                        Some(i)
                    };
                }
            }
        }
        prev = c;
    }
    None
}

/// Replaces string contents and comments with spaces (keeps byte offsets).
fn strip_strings_and_comments(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut quote: Option<char> = None;
    let mut prev = '\0';
    for c in line.chars() {
        match quote {
            Some(q) => {
                out.push(if c == q && prev != '\\' { c } else { ' ' });
                if c == q && prev != '\\' {
                    quote = None;
                }
            }
            None => {
                if c == '#' {
                    out.extend(std::iter::repeat_n(' ', line.len() - out.len()));
                    return out;
                }
                if c == '"' || c == '\'' {
                    quote = Some(c);
                }
                out.push(c);
            }
        }
        prev = c;
    }
    out
}

// ---------------------------------------------------------------------------
// Import helpers

/// Quick fixes that import an unresolved `name`.
pub fn import_fixes(
    db: &Analysis,
    file: &Arc<FileInfo>,
    name: &str,
    project_symbols: &[(String, String)],
) -> Vec<QuickFix> {
    let mut fixes = Vec::new();
    let insert_at = import_insert_offset(file);
    // A module with that name?
    if db.module(name).is_some() {
        fixes.push(QuickFix {
            title: format!("Import '{name}'"),
            edits: vec![(TextRange::empty(insert_at), format!("import {name}\n"))],
        });
    }
    // A symbol defined in a project module?
    let mut seen = std::collections::HashSet::new();
    for (sym, module) in project_symbols {
        if sym == name
            && seen.insert(module.clone())
            && file.module_name.as_deref() != Some(module.as_str())
        {
            fixes.push(QuickFix {
                title: format!("Import 'from {module} import {name}'"),
                edits: vec![(
                    TextRange::empty(insert_at),
                    format!("from {module} import {name}\n"),
                )],
            });
        }
    }
    // Common stdlib symbols.
    for (module, names) in STDLIB_SYMBOLS {
        if names.contains(&name) {
            fixes.push(QuickFix {
                title: format!("Import 'from {module} import {name}'"),
                edits: vec![(
                    TextRange::empty(insert_at),
                    format!("from {module} import {name}\n"),
                )],
            });
        }
    }
    fixes.truncate(6);
    fixes
}

const STDLIB_SYMBOLS: &[(&str, &[&str])] = &[
    (
        "typing",
        &[
            "Any",
            "Optional",
            "Union",
            "List",
            "Dict",
            "Set",
            "Tuple",
            "Callable",
            "Iterable",
            "Iterator",
            "Sequence",
            "Mapping",
            "TypeVar",
            "Generic",
            "Protocol",
            "Literal",
            "Final",
            "ClassVar",
            "TYPE_CHECKING",
            "cast",
            "overload",
            "NamedTuple",
            "TypedDict",
            "Awaitable",
            "Coroutine",
            "Self",
        ],
    ),
    (
        "dataclasses",
        &["dataclass", "field", "asdict", "astuple", "replace"],
    ),
    (
        "collections",
        &[
            "defaultdict",
            "OrderedDict",
            "Counter",
            "deque",
            "namedtuple",
            "ChainMap",
        ],
    ),
    ("pathlib", &["Path", "PurePath"]),
    (
        "functools",
        &[
            "partial",
            "reduce",
            "wraps",
            "lru_cache",
            "cache",
            "cached_property",
            "total_ordering",
        ],
    ),
    (
        "itertools",
        &[
            "chain",
            "groupby",
            "islice",
            "product",
            "permutations",
            "combinations",
            "count",
            "cycle",
            "repeat",
            "zip_longest",
        ],
    ),
    (
        "datetime",
        &["datetime", "date", "time", "timedelta", "timezone"],
    ),
    ("enum", &["Enum", "IntEnum", "StrEnum", "Flag", "auto"]),
    ("abc", &["ABC", "abstractmethod"]),
    (
        "contextlib",
        &[
            "contextmanager",
            "suppress",
            "closing",
            "asynccontextmanager",
            "ExitStack",
        ],
    ),
    ("os", &["getcwd", "listdir", "environ", "makedirs"]),
    ("decimal", &["Decimal"]),
    ("fractions", &["Fraction"]),
    ("logging", &["getLogger", "Logger"]),
    ("unittest", &["TestCase", "mock"]),
    (
        "asyncio",
        &[
            "run",
            "gather",
            "sleep",
            "create_task",
            "Queue",
            "Lock",
            "Event",
        ],
    ),
    (
        "re",
        &["compile", "match", "search", "sub", "Pattern", "Match"],
    ),
    ("json", &["loads", "dumps", "load", "dump"]),
    ("random", &["randint", "choice", "shuffle", "random"]),
    ("math", &["sqrt", "pi", "floor", "ceil", "inf", "isclose"]),
    ("sys", &["argv", "exit", "stderr", "stdout", "path"]),
    (
        "subprocess",
        &["run", "Popen", "PIPE", "CalledProcessError"],
    ),
    ("time", &["sleep", "perf_counter", "monotonic"]),
    ("copy", &["deepcopy"]),
    ("io", &["StringIO", "BytesIO"]),
    ("shutil", &["copyfile", "rmtree", "which"]),
    (
        "tempfile",
        &["TemporaryDirectory", "NamedTemporaryFile", "mkdtemp"],
    ),
    ("uuid", &["uuid4", "UUID"]),
    ("pprint", &["pprint", "pformat"]),
    ("textwrap", &["dedent", "indent", "wrap"]),
    ("string", &["ascii_letters", "digits", "Template"]),
    ("operator", &["itemgetter", "attrgetter", "methodcaller"]),
    ("threading", &["Thread", "Lock", "Event", "RLock"]),
    ("queue", &["Queue", "Empty", "Full"]),
    ("statistics", &["mean", "median", "stdev"]),
];

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

    fn apply(text: &str, edits: &[Edit]) -> String {
        let mut edits = edits.to_vec();
        edits.sort_by_key(|(r, _)| std::cmp::Reverse(r.start()));
        let mut out = text.to_string();
        for (r, rep) in edits {
            out.replace_range(usize::from(r.start())..usize::from(r.end()), &rep);
        }
        out
    }

    #[test]
    fn unused_imports_and_variables() {
        let src = "import os\nimport sys, json\nfrom typing import List, Dict\n\ndef f():\n    unused = 1\n    used = 2\n    return used + len(sys.argv)\n\nx: List[int] = []\n";
        let (db, f) = setup(src);
        let problems = inspect(&db, &f, &InspectionOptions::default());
        let codes: Vec<(String, String)> = problems
            .iter()
            .map(|p| (p.diagnostic.code.to_string(), p.diagnostic.message.clone()))
            .collect();
        assert!(
            codes.contains(&(
                "PyUnusedImports".into(),
                "Unused import statement 'import os'".into()
            )),
            "{codes:?}"
        );
        assert!(
            codes.contains(&(
                "PyUnusedImports".into(),
                "Unused import statement 'import json'".into()
            )),
            "{codes:?}"
        );
        assert!(
            codes.contains(&("PyUnusedImports".into(), "Unused import 'Dict'".into())),
            "{codes:?}"
        );
        assert!(
            codes.contains(&(
                "PyUnusedLocal".into(),
                "Local variable 'unused' value is not used".into()
            )),
            "{codes:?}"
        );
        assert!(!codes.iter().any(|(_, m)| m.contains("'used'")));
        assert!(!codes
            .iter()
            .any(|(_, m)| m.contains("'sys'") || m.contains("'List'")));
        // Remove-unused-import fix for `json` keeps `sys`.
        let p = problems
            .iter()
            .find(|p| p.diagnostic.message == "Unused import statement 'import json'")
            .unwrap();
        let fixed = apply(src, &p.fixes[0].edits);
        assert!(fixed.contains("import sys\n"), "{fixed}");
        let p = problems
            .iter()
            .find(|p| p.diagnostic.message.contains("'import os'"))
            .unwrap();
        let fixed = apply(src, &p.fixes[0].edits);
        assert!(fixed.starts_with("import sys, json\n"), "{fixed}");
        // Optimize imports: removes unused, sorts, groups.
        let edits = rustpy_format::imports::optimize_imports(&f, &unused_import_ranges(&f));
        let fixed = apply(src, &edits);
        assert!(
            fixed.starts_with("import sys\nfrom typing import List\n"),
            "{fixed}"
        );
    }

    #[test]
    fn unused_import_policy_filters_facts_before_formatting() {
        let src = "import os  # noqa: F401\nimport sys  # noqa\n# noinspection PyUnusedImports\nimport json\n# noinspection PyUnresolvedReferences\nimport decimal\nimport pathlib\n";
        let (_, file) = setup(src);
        assert_eq!(
            rustpy_semantic::imports::unused_import_candidates(&file)
                .iter()
                .map(|b| b.name.as_str())
                .collect::<Vec<_>>(),
            vec!["os", "sys", "json", "decimal", "pathlib"]
        );
        let ranges = unused_import_ranges(&file);
        assert_eq!(ranges.len(), 1);
        assert_eq!(
            &src[usize::from(ranges[0].start())..usize::from(ranges[0].end())],
            "pathlib"
        );
        let problems = unused_imports(&file);
        assert_eq!(problems.len(), 1);
        assert_eq!(
            problems[0].diagnostic.message,
            "Unused import statement 'import pathlib'"
        );
        assert_eq!(problems[0].fixes[0].title, "Remove unused import 'pathlib'");
        let optimized = rustpy_format::imports::optimize_imports(&file, &ranges);
        assert_eq!(problems[0].fixes[1].title, "Optimize imports");
        assert_eq!(problems[0].fixes[1].edits, optimized);
        let output = apply(src, &optimized);
        assert!(
            output.contains("import os  # noqa: F401\n") && output.contains("import sys  # noqa\n"),
            "{output}"
        );
        assert!(
            output.contains("# noinspection PyUnusedImports\nimport json\n"),
            "{output}"
        );
        assert!(
            output.contains("# noinspection PyUnresolvedReferences\nimport decimal\n"),
            "{output}"
        );
        assert!(!output.contains("pathlib"), "{output}");
    }

    #[test]
    fn pep8_checks_and_fixes() {
        let src = "import os,sys\nx=1 \nif x == None:  # c\n    y = [1,2]\ndef g():\n    return x # comment\n";
        let (db, f) = setup(src);
        let problems = inspect(
            &db,
            &f,
            &InspectionOptions {
                unused_imports: false,
                unused_variables: false,
                ..Default::default()
            },
        );
        let codes: Vec<&str> = problems
            .iter()
            .map(|p| p.diagnostic.code.as_str())
            .collect();
        for c in [
            "PEP8:E401",
            "PEP8:W291",
            "PEP8:E711",
            "PEP8:E231",
            "PEP8:E302",
            "PEP8:E261",
        ] {
            assert!(codes.contains(&c), "missing {c} in {codes:?}");
        }
        let p = problems
            .iter()
            .find(|p| p.diagnostic.code == "PEP8:E711")
            .unwrap();
        assert_eq!(
            apply(src, &p.fixes[0].edits).lines().nth(2).unwrap(),
            "if x is None:  # c"
        );
        let p = problems
            .iter()
            .find(|p| p.diagnostic.code == "PEP8:E401")
            .unwrap();
        assert!(apply(src, &p.fixes[0].edits).starts_with("import os\nimport sys\n"));
    }

    #[test]
    fn cancellable_inspection_matches_inspect_and_stops_when_cancelled() {
        let src = "import os,sys\nx=1 \nif x == None:  # c\n    y = undefined_name\ndef g():\n    return x # comment\n";
        let (db, f) = setup(src);
        let opts = InspectionOptions {
            project_symbols: Arc::new(vec![("undefined_name".into(), "pkg.mod".into())]),
            ..Default::default()
        };
        let token = CancellationToken::new();
        let all = inspect_cancellable(&db, &f, &opts, &token).unwrap();
        assert_eq!(all, inspect(&db, &f, &opts));
        assert!(
            all.iter().any(|p| p
                .fixes
                .iter()
                .any(|q| q.title == "Import 'from pkg.mod import undefined_name'")),
            "shared project symbols feed import fixes"
        );
        token.cancel();
        assert_eq!(inspect_cancellable(&db, &f, &opts, &token), Err(Cancelled));
        let child = CancellationToken::new();
        let run = child.child();
        child.cancel();
        assert_eq!(
            inspect_cancellable(&db, &f, &opts, &run),
            Err(Cancelled),
            "a run's token follows its parent"
        );
    }

    #[test]
    fn parallel_project_inspection_matches_file_by_file_inspection() {
        let dir = std::env::temp_dir().join(format!("rustpy-inspect-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut paths = Vec::new();
        for i in 0..40 {
            let p = dir.join(format!("m{i:02}.py"));
            let src = match i % 3 {
                0 => format!("import os\n\nVALUE_{i} = undefined_{i}\n"),
                1 => format!("def f{i}(x):\n    return x + {i}\n"),
                _ => "from m00 import VALUE_0\nprint(VALUE_0 == None)\n".to_string(),
            };
            std::fs::write(&p, src).unwrap();
            paths.push(p);
        }
        let db = Analysis::for_project(&dir);
        let opts = InspectionOptions::default();
        let progress = ProgressReporter::new();
        let all = inspect_files(&db, &paths, &opts, &progress).unwrap();
        let sequential: Vec<(PathBuf, Vec<Problem>)> = paths
            .iter()
            .filter_map(|p| {
                let f = db.file_for_path(p).unwrap();
                let problems = inspect(&db, &f, &opts);
                (!problems.is_empty()).then(|| (p.clone(), problems))
            })
            .collect();
        assert_eq!(all, sequential, "same problems, same order");
        assert!(all.iter().any(|(_, ps)| ps
            .iter()
            .any(|p| p.diagnostic.message.contains("undefined_0"))));
        assert!(
            !all.iter().any(|(p, _)| p.ends_with("m01.py")),
            "files without problems are left out"
        );
        assert_eq!(progress.progress().2, Some(1.0));
        let cancelled = ProgressReporter::new();
        cancelled.token().cancel();
        assert_eq!(
            inspect_files(&db, &paths, &opts, &cancelled),
            Err(Cancelled)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_fix_candidates() {
        let src = "x = Path('a')\nos.getcwd()\n";
        let (db, f) = setup(src);
        let fixes = import_fixes(&db, &f, "Path", &[("Path".into(), "mypkg.paths".into())]);
        let titles: Vec<&str> = fixes.iter().map(|f| f.title.as_str()).collect();
        assert!(
            titles.contains(&"Import 'from pathlib import Path'"),
            "{titles:?}"
        );
        assert!(
            titles.contains(&"Import 'from mypkg.paths import Path'"),
            "{titles:?}"
        );
        let fixes = import_fixes(&db, &f, "os", &[]);
        assert_eq!(fixes[0].title, "Import 'os'");
        assert_eq!(fixes[0].edits[0].0.start(), TextSize::new(0));
    }
}
