//! `rustpy inspect`: an offline inspection run (`inspect.sh`-style arguments) on
//! RustPy's code analysis. It checks Python files as the editor does (semantic
//! inspections, PEP 8, docstrings, regular expressions, grammar and spelling)
//! and Markdown files for grammar and spelling, then writes the results (see
//! [`crate::report`]).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use rustpy_analysis::inspections::InspectionOptions;
use rustpy_core::{CancellationToken, Cancelled, Diagnostic, Severity, TextRange, TextSize};
use rustpy_project::{ProjectScope, WorkspaceSnapshot};
use rustpy_semantic::module::ModuleResolver;
use rustpy_semantic::{Analysis, FileInfo};
use rustpy_settings::{InspectionSettings, Settings};

use crate::args::InspectArgs;
use crate::profile::{self, Profile};
use crate::report;

/// One problem found, positioned for the reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// The inspection id (`PyUnresolvedReferences`, `PyPep8`, …).
    pub inspection: String,
    pub severity: Severity,
    pub path: PathBuf,
    /// `/`-separated path relative to the project.
    pub rel: String,
    /// 1-based line and column (in characters) of the start, and of the end.
    pub line: usize,
    pub column: usize,
    pub end_line: usize,
    pub end_column: usize,
    /// Length of the highlighted text in characters.
    pub length: usize,
    pub message: String,
    /// The highlighted text (shortened, first line only).
    pub snippet: String,
}

/// The id used in reports for a diagnostic code: PEP 8 checks are one
/// inspection (`PyPep8`, the code stays in the message), and
/// parser errors are `PySyntaxError`.
pub fn report_id(code: &str) -> String {
    if code.starts_with("PEP8:") {
        "PyPep8".into()
    } else if code == "syntax" {
        "PySyntaxError".into()
    } else {
        code.to_string()
    }
}

/// Inspection metadata: `(id, display name, group, default severity)`.
pub const CATALOG: &[(&str, &str, &str, Severity)] = &[
    ("PySyntaxError", "Syntax errors", "Python", Severity::Error),
    (
        "PyUnresolvedReferences",
        "Unresolved references",
        "Python",
        Severity::Error,
    ),
    (
        "PyUnusedImports",
        "Unused import",
        "Python",
        Severity::Warning,
    ),
    (
        "PyUnusedLocal",
        "Unused local symbols",
        "Python",
        Severity::Warning,
    ),
    (
        "PyPep8",
        "PEP 8 coding style violation",
        "Python",
        Severity::WeakWarning,
    ),
    (
        "PyPep8Naming",
        "PEP 8 naming convention violation",
        "Python",
        Severity::WeakWarning,
    ),
    (
        "PyArgumentList",
        "Incorrect call arguments",
        "Python",
        Severity::Warning,
    ),
    (
        "PyTypeChecker",
        "Incorrect type",
        "Python",
        Severity::Warning,
    ),
    (
        "PyParameterList",
        "Incorrect parameter list",
        "Python",
        Severity::Error,
    ),
    (
        "PyCompatibility",
        "Code is incompatible with specific Python versions",
        "Python",
        Severity::Warning,
    ),
    (
        "PyMissingOrEmptyDocstring",
        "Missing or empty docstring",
        "Python",
        Severity::WeakWarning,
    ),
    (
        "PyDocstring",
        "Incorrect docstring",
        "Python",
        Severity::WeakWarning,
    ),
    (
        "PyMethodParameters",
        "Methods having troublesome first parameters",
        "Python",
        Severity::Warning,
    ),
    (
        "PyRedeclaration",
        "Redeclared names without usages",
        "Python",
        Severity::Warning,
    ),
    (
        "PyUnreachableCode",
        "Unreachable code",
        "Python",
        Severity::Warning,
    ),
    (
        "PyDefaultArgument",
        "The default argument is mutable",
        "Python",
        Severity::Warning,
    ),
    (
        "PyShadowingBuiltins",
        "Shadowing built-in names",
        "Python",
        Severity::WeakWarning,
    ),
    (
        "PyStatementEffect",
        "Statement has no effect",
        "Python",
        Severity::Warning,
    ),
    (
        "PyBroadException",
        "Unclear exception clauses",
        "Python",
        Severity::WeakWarning,
    ),
    (
        "PyMethodMayBeStatic",
        "Method is not declared static",
        "Python",
        Severity::WeakWarning,
    ),
    (
        "PyMissingConstructor",
        "Missed call to '__init__' of the super class",
        "Python",
        Severity::Warning,
    ),
    (
        "PySecurityEval",
        "Use of eval or exec",
        "Python Security",
        Severity::WeakWarning,
    ),
    (
        "PySecurityShellInjection",
        "Shell command injection",
        "Python Security",
        Severity::WeakWarning,
    ),
    (
        "PySecurityUnsafeDeserialization",
        "Unsafe deserialization",
        "Python Security",
        Severity::WeakWarning,
    ),
    (
        "PySecurityHardcodedSecret",
        "Hardcoded secret",
        "Python Security",
        Severity::WeakWarning,
    ),
    (
        "PySecurityOsSystem",
        "Use of os.system",
        "Python Security",
        Severity::WeakWarning,
    ),
    (
        "PySecurityTempfile",
        "Insecure temporary file",
        "Python Security",
        Severity::WeakWarning,
    ),
    (
        "PyRegex",
        "Regular expression errors",
        "RegExp",
        Severity::Error,
    ),
    (
        "SpellCheckingInspection",
        "Typo",
        "Proofreading",
        Severity::Typo,
    ),
    ("Grammar", "Grammar", "Proofreading", Severity::Typo),
];

/// Display name and group of an inspection id; unknown ids get a name from
/// their words (`PyRaiseNotImplemented` → "Raise not implemented").
pub fn describe(id: &str) -> (String, &'static str) {
    if let Some((_, name, group, _)) = CATALOG.iter().find(|(i, ..)| *i == id) {
        return (name.to_string(), group);
    }
    let words = id.strip_prefix("Py").unwrap_or(id);
    let mut name = String::new();
    for (i, c) in words.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            name.push(' ');
            name.extend(c.to_lowercase());
        } else {
            name.push(c);
        }
    }
    (
        name,
        if id.starts_with("PySecurity") {
            "Python Security"
        } else {
            "Python"
        },
    )
}

/// Byte offset → (0-based line, 0-based character column).
struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(text: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Self { starts }
    }

    fn position(&self, text: &str, offset: usize) -> (usize, usize) {
        let offset = rustpy_core::floor_char_boundary(text, offset);
        let line = self
            .starts
            .partition_point(|s| *s <= offset)
            .saturating_sub(1);
        let column = text[self.starts[line]..offset].chars().count();
        (line, column)
    }
}

fn finding(
    project: &Path,
    path: &Path,
    text: &str,
    index: &LineIndex,
    d: &Diagnostic,
    profile: &Profile,
) -> Option<Finding> {
    let id = report_id(&d.code);
    if !profile.is_enabled(&id) || profile.ignores(&id, &d.message) {
        return None;
    }
    let start = usize::from(d.range.start()).min(text.len());
    let end = usize::from(d.range.end()).clamp(start, text.len());
    let (line, column) = index.position(text, start);
    let (end_line, end_column) = index.position(text, end);
    let highlighted = &text[rustpy_core::floor_char_boundary(text, start)
        ..rustpy_core::floor_char_boundary(text, end)];
    let first_line = highlighted.lines().next().unwrap_or("");
    let snippet: String = first_line.chars().take(200).collect();
    Some(Finding {
        severity: profile.severity_of(&id, d.severity),
        inspection: id,
        path: path.to_path_buf(),
        rel: path
            .strip_prefix(project)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/"),
        line: line + 1,
        column: column + 1,
        end_line: end_line + 1,
        end_column: end_column + 1,
        length: highlighted.chars().count(),
        message: d.message.clone(),
        snippet,
    })
}

fn grammar(issues: Vec<rustpy_grammar::Issue>) -> impl Iterator<Item = Diagnostic> {
    rustpy_analysis::file_analysis::grammar_problems(&issues)
        .into_iter()
        .map(|p| p.diagnostic)
}

fn spelling(typos: Vec<rustpy_spell::Typo>) -> impl Iterator<Item = Diagnostic> {
    use rustpy_analysis::file_analysis::{spelling_problems, DictionaryActionPolicy};
    spelling_problems(&typos, DictionaryActionPolicy::ReplacementsOnly)
        .into_iter()
        .map(|p| p.diagnostic)
}

/// What the editor reports for a Python file (`sync_analysis` in the IDE).
pub fn python_diagnostics(
    analysis: &Analysis,
    file: &Arc<FileInfo>,
    opts: &InspectionOptions,
    groups: &InspectionSettings,
    dictionary: &rustpy_spell::Dictionary,
    token: &CancellationToken,
) -> Result<Vec<Diagnostic>, Cancelled> {
    use rustpy_analysis::file_analysis::{
        self, DictionaryActionPolicy, FileAnalysisOptions, StubSupplementPolicy,
    };
    let problems = file_analysis::analyze_python(
        analysis,
        file,
        &FileAnalysisOptions {
            inspections: opts,
            grammar: groups.grammar,
            dictionary: groups.spelling.then_some(dictionary),
            stub_supplements: StubSupplementPolicy::Include,
            dictionary_actions: DictionaryActionPolicy::ReplacementsOnly,
        },
        token,
    )?;
    Ok(problems.into_iter().map(|p| p.diagnostic).collect())
}

/// What the editor reports for a Markdown file.
pub fn markdown_diagnostics(
    text: &str,
    groups: &InspectionSettings,
    dictionary: &rustpy_spell::Dictionary,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    if groups.grammar {
        out.extend(grammar(rustpy_grammar::check_markdown(text)));
    }
    if groups.spelling {
        out.extend(spelling(rustpy_spell::check_plain_text(text, dictionary)));
    }
    out
}

fn is_python(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("py" | "pyi")
    )
}

fn is_markdown(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("md" | "markdown")
    )
}

/// The files to inspect below `scope`: Python and Markdown files that are not
/// excluded, ignored by Git or in a virtual environment, or only those with
/// uncommitted changes (`-changes`).
pub fn collect_files(
    project: &Path,
    scope: &Path,
    excluded: &[PathBuf],
    changes: bool,
) -> Result<Vec<PathBuf>, String> {
    collect_files_in(
        project,
        &ProjectScope::new(scope).with_excluded(excluded.iter().cloned()),
        changes,
    )
}

fn collect_files_in(
    project: &Path,
    scope: &ProjectScope,
    changes: bool,
) -> Result<Vec<PathBuf>, String> {
    let mut files: Vec<PathBuf> = if changes {
        let repo = rustpy_vcs::GitRepo::discover(project).ok_or_else(|| {
            format!(
                "-changes needs a Git repository, and {} is not in one",
                project.display()
            )
        })?;
        let status = repo.status().map_err(|e| format!("git status: {e}"))?;
        status
            .into_iter()
            .filter(|e| {
                !matches!(
                    e.status,
                    rustpy_vcs::FileStatus::Deleted | rustpy_vcs::FileStatus::Ignored
                )
            })
            .map(|e| e.path.canonicalize().unwrap_or(e.path))
            .filter(|p| p.is_file() && scope.contains(p))
            .collect()
    } else {
        rustpy_project::files::project_files_in(scope)
    };
    files.retain(|p| is_python(p) || is_markdown(p));
    files.sort();
    files.dedup();
    Ok(files)
}

/// The interpreter the IDE would pick for the project (the one recorded in
/// `.rustpy/workspace.json`, else a virtual environment, else the first found).
/// The project's own environments are only run when the project is trusted.
pub fn pick_interpreter(
    project: &Path,
    settings: &Settings,
    recorded: Option<&Path>,
) -> Option<rustpy_project::Interpreter> {
    let trusted = rustpy_settings::trust_all_from_env() || settings.is_project_trusted(project);
    let found = rustpy_project::discover_interpreters_with(Some(project), trusted);
    recorded
        .and_then(|r| found.iter().find(|i| i.path == r).cloned())
        .or_else(|| found.iter().find(|i| i.kind.contains("venv")).cloned())
        .or_else(|| found.first().cloned())
}

/// The analysis database for the project, set up like the IDE's.
pub fn project_analysis(
    project: &Path,
    workspace: &WorkspaceSnapshot,
    interpreter: Option<&rustpy_project::Interpreter>,
) -> Analysis {
    let roots = workspace.roots();
    let analysis = match interpreter {
        Some(i) => Analysis::new(
            ModuleResolver::new(roots, i.search_paths(), i.major_minor())
                .with_platform(&i.platform),
        ),
        None => Analysis::new(ModuleResolver::new(roots, Vec::new(), (3, 12))),
    };
    let settings = Settings::load();
    analysis.set_analyze_doc_comments(
        settings
            .documentation
            .effective(Some(project), &settings.docstring_format)
            .analyze_doc_comments,
    );
    let analysis = analysis.with_stub_roots(settings.python_stub_roots);
    let rt = rustpy_semantic::runtime_types::RuntimeTypes::load(project);
    if !rt.is_empty() {
        analysis.set_runtime_types(rt);
    }
    analysis
}

/// The spelling dictionary: bundled words plus the user's `dictionary.txt`.
pub fn dictionary() -> rustpy_spell::Dictionary {
    let words = rustpy_spell::load_user_words(Settings::config_dir().join("dictionary.txt"))
        .unwrap_or_default();
    rustpy_spell::Dictionary::bundled().with_user_words(&words)
}

/// Inspects `files` (absolute paths in `project`), in parallel.
/// Analysis cancellation is a failure, never a partial successful report.
pub fn inspect_files(
    analysis: &Analysis,
    project: &Path,
    files: &[PathBuf],
    profile: &Profile,
    opts: &InspectionOptions,
    mut progress: impl FnMut(&Path) + Send,
) -> Result<Vec<Finding>, Cancelled> {
    let dictionary = dictionary();
    let token = CancellationToken::new();
    let errors: Mutex<Option<Cancelled>> = Mutex::new(None);
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Finding>> = Mutex::new(Vec::new());
    let progress = Mutex::new(&mut progress);
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 16)
        .min(files.len().max(1));
    std::thread::scope(|s| {
        for _ in 0..workers {
            std::thread::Builder::new()
                .name("rustpy-inspect".into())
                .stack_size(16 * 1024 * 1024)
                .spawn_scoped(s, || {
                    loop {
                        let i = next.fetch_add(1, Ordering::SeqCst);
                        let Some(path) = files.get(i) else { break };
                        if let Ok(mut p) = progress.lock() {
                            (*p)(path);
                        }
                        let (text, diags) = if is_python(path) {
                            match analysis.file_for_path(path) {
                                Some(file) => {
                                    let diags = match python_diagnostics(
                                        analysis,
                                        &file,
                                        opts,
                                        &profile.groups,
                                        &dictionary,
                                        &token,
                                    ) {
                                        Ok(diags) => diags,
                                        Err(error) => {
                                            *errors
                                                .lock()
                                                .expect("inspection errors lock poisoned") =
                                                Some(error);
                                            token.cancel();
                                            break;
                                        }
                                    };
                                    (file.text.to_string(), diags)
                                }
                                None => {
                                    let Some(error) = analysis.file_encoding_error(path) else {
                                        continue;
                                    };
                                    // Report a source failure without pretending a decoded
                                    // empty file was successfully analyzed.
                                    (
                                        String::new(),
                                        vec![Diagnostic::new(
                                            TextRange::empty(TextSize::new(0)),
                                            Severity::Error,
                                            "PySyntaxError",
                                            format!("Cannot decode source: {error}"),
                                        )],
                                    )
                                }
                            }
                        } else {
                            match std::fs::read_to_string(path) {
                                Ok(text) => {
                                    let diags =
                                        markdown_diagnostics(&text, &profile.groups, &dictionary);
                                    (text, diags)
                                }
                                Err(_) => continue,
                            }
                        };
                        let index = LineIndex::new(&text);
                        let found: Vec<Finding> = diags
                            .iter()
                            .filter_map(|d| finding(project, path, &text, &index, d, profile))
                            .collect();
                        if let Ok(mut r) = results.lock() {
                            r.extend(found);
                        }
                    }
                })
                .expect("cannot start an inspection worker");
        }
    });
    if let Some(error) = errors
        .into_inner()
        .expect("inspection errors lock poisoned")
    {
        return Err(error);
    }
    let mut all = results.into_inner().unwrap_or_default();
    all.sort_by(|a, b| {
        (&a.rel, a.line, a.column, &a.inspection, &a.message).cmp(&(
            &b.rel,
            b.line,
            b.column,
            &b.inspection,
            &b.message,
        ))
    });
    all.dedup();
    Ok(all)
}

/// The options of the semantic inspections for a profile.
pub fn inspection_options(
    groups: &InspectionSettings,
    max_line_length: usize,
    python_version: (u32, u32),
) -> InspectionOptions {
    InspectionOptions {
        max_line_length,
        pep8: groups.pep8,
        unused_imports: groups.unused_imports,
        unused_variables: groups.unused_variables,
        type_checker: groups.type_checker,
        security: groups.security,
        compatibility: groups.compatibility,
        code_smells: groups.code_smells,
        naming: groups.naming,
        missing_docstrings: groups.missing_docstrings,
        python_version,
        ..Default::default()
    }
}

/// Runs `rustpy inspect`; progress goes to `out`, problems to `err`. Returns
/// the exit status (0 no errors, 1 error-severity problems, 2 failure).
pub fn run(
    args: &InspectArgs,
    settings: &Settings,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    run_with(args, settings, None, out, err)
}

/// [`run`] with the interpreter given instead of discovered (tests).
pub fn run_with(
    args: &InspectArgs,
    settings: &Settings,
    interpreter: Option<Option<rustpy_project::Interpreter>>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let project = match args.project.canonicalize() {
        Ok(p) if p.is_dir() => p,
        Ok(p) => {
            let _ = writeln!(err, "rustpy inspect: {} is not a folder", p.display());
            return 2;
        }
        Err(e) => {
            let _ = writeln!(err, "rustpy inspect: {}: {e}", args.project.display());
            return 2;
        }
    };
    let profile = match profile::resolve(&args.profile, &project, settings) {
        Ok(p) => p,
        Err(e) => {
            let _ = writeln!(err, "rustpy inspect: {e}");
            return 2;
        }
    };
    let scope = match &args.dir {
        None => project.clone(),
        Some(d) => {
            let candidate = if d.is_absolute() || d.exists() {
                d.clone()
            } else {
                project.join(d)
            };
            match candidate.canonicalize() {
                Ok(c) if c.is_dir() && c.starts_with(&project) => c,
                Ok(c) => {
                    let _ = writeln!(
                        err,
                        "rustpy inspect: -d {}: not a folder inside {}",
                        c.display(),
                        project.display()
                    );
                    return 2;
                }
                Err(e) => {
                    let _ = writeln!(err, "rustpy inspect: -d {}: {e}", d.display());
                    return 2;
                }
            }
        }
    };
    let workspace = match WorkspaceSnapshot::try_load(vec![project.clone()]) {
        Ok(workspace) => workspace,
        Err(error) => {
            let _ = writeln!(err, "rustpy inspect: {error}");
            return 2;
        }
    };
    let scope = if args.dir.is_some() {
        ProjectScope::new(scope).with_excluded(workspace.excluded.iter().cloned())
    } else {
        workspace.scope()
    };
    let files = match collect_files_in(&project, &scope, args.changes) {
        Ok(f) => f,
        Err(e) => {
            let _ = writeln!(err, "rustpy inspect: {e}");
            return 2;
        }
    };
    if let Err(e) = std::fs::create_dir_all(&args.output) {
        let _ = writeln!(
            err,
            "rustpy inspect: cannot create {}: {e}",
            args.output.display()
        );
        return 2;
    }
    let verbose = args.verbosity;
    if verbose >= 1 {
        let _ = writeln!(
            out,
            "Inspecting {} with profile '{}' ({})",
            project.display(),
            profile.name,
            profile.source
        );
    }
    let interpreter = match interpreter {
        Some(i) => i,
        None => pick_interpreter(&project, settings, workspace.ide_interpreter.as_deref()),
    };
    if verbose >= 2 {
        match &interpreter {
            Some(i) => {
                let _ = writeln!(
                    out,
                    "Python interpreter: {} ({})",
                    i.path.display(),
                    i.label()
                );
            }
            None => {
                let _ = writeln!(
                    out,
                    "Python interpreter: none found (third-party imports will be unresolved)"
                );
            }
        }
    }
    let analysis = project_analysis(&project, &workspace, interpreter.as_ref());
    analysis.set_analyze_doc_comments(
        settings
            .documentation
            .effective(Some(&project), &settings.docstring_format)
            .analyze_doc_comments,
    );
    let mut opts = inspection_options(
        &profile.groups,
        settings.editor.right_margin,
        interpreter
            .as_ref()
            .map(|i| i.major_minor())
            .unwrap_or((3, 12)),
    );
    opts.editorconfig_enabled = settings.editor.editorconfig_enabled;
    opts.style = settings.inspections.style.clone();
    if opts.style.quote_style == "code_style" {
        opts.style.quote_style = settings.code_style.quote_style.clone()
    }
    if opts.style.interpreter.is_none() {
        opts.style.interpreter = interpreter
            .as_ref()
            .map(|interpreter| interpreter.path.clone())
    }
    opts.policy = match rustpy_analysis::inspections::infrastructure::Policy::from_settings(
        &profile.groups.profiles,
        Some(&project),
        true,
    ) {
        Ok(policy) => Some(policy),
        Err(error) => {
            let _ = writeln!(err, "{error}");
            return 2;
        }
    };
    let progress_out: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let findings = match inspect_files(&analysis, &project, &files, &profile, &opts, |p| {
        if verbose >= 2 {
            if let Ok(mut lines) = progress_out.lock() {
                lines.push(format!(
                    "Inspecting {}",
                    p.strip_prefix(&project).unwrap_or(p).display()
                ));
            }
        }
    }) {
        Ok(findings) => findings,
        Err(error) => {
            let _ = writeln!(err, "rustpy inspect: {error}");
            return 2;
        }
    };
    for line in progress_out.into_inner().unwrap_or_default() {
        let _ = writeln!(out, "{line}");
    }
    let written = match report::write(
        args.format,
        &args.output,
        &project,
        &profile,
        &findings,
        files.len(),
    ) {
        Ok(w) => w,
        Err(e) => {
            let _ = writeln!(
                err,
                "rustpy inspect: cannot write the results to {}: {e}",
                args.output.display()
            );
            return 2;
        }
    };
    if args.format == crate::args::ReportFormat::Plain && verbose >= 1 {
        let _ = out.write_all(report::plain_text(&findings).as_bytes());
    } else if verbose >= 2 {
        for f in &findings {
            let _ = writeln!(out, "{}", report::plain_line(f));
        }
    }
    let count = |s: Severity| findings.iter().filter(|f| f.severity == s).count();
    let errors = count(Severity::Error);
    if verbose >= 1 {
        let files_with = {
            let mut v: Vec<&str> = findings.iter().map(|f| f.rel.as_str()).collect();
            v.dedup();
            v.len()
        };
        let _ = writeln!(
            out,
            "{} file(s) inspected, {} problem(s) in {} file(s): {} error(s), {} warning(s), {} weak warning(s), {} typo(s), {} information.",
            files.len(),
            findings.len(),
            files_with,
            errors,
            count(Severity::Warning),
            count(Severity::WeakWarning),
            count(Severity::Typo),
            count(Severity::Info)
        );
        let _ = writeln!(
            out,
            "Results: {}",
            written
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if errors > 0 {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Frozen pre-service CLI composition, used only for independent output parity.
    fn legacy_python_diagnostics(
        analysis: &Analysis,
        file: &Arc<FileInfo>,
        opts: &InspectionOptions,
        groups: &InspectionSettings,
        dictionary: &rustpy_spell::Dictionary,
    ) -> Vec<Diagnostic> {
        let text: &str = &file.text;
        let mut out: Vec<Diagnostic> = rustpy_analysis::inspections::inspect(analysis, file, opts)
            .into_iter()
            .map(|p| p.diagnostic)
            .collect();
        if groups.grammar {
            out.extend(legacy_grammar(rustpy_grammar::check_python(text)));
        }
        if groups.spelling {
            out.extend(legacy_spelling(rustpy_spell::check_text(
                text,
                dictionary,
                rustpy_spell::CheckOptions::default(),
            )));
        }
        for (r, message) in rustpy_semantic::docstrings::docstring_problems(analysis, file) {
            out.push(Diagnostic::new(
                r,
                Severity::WeakWarning,
                "PyDocstring",
                message,
            ));
        }
        for inj in rustpy_regex::injections(text, &file.parse) {
            for e in rustpy_regex::check(&inj.pattern, inj.verbose) {
                let raw = text[usize::from(inj.string_range.start())..].starts_with(['r', 'R']);
                let r = if raw {
                    TextRange::new(
                        inj.value_range.start() + e.range.start(),
                        (inj.value_range.start() + e.range.end()).min(inj.value_range.end()),
                    )
                } else {
                    inj.value_range
                };
                out.push(Diagnostic::new(
                    r,
                    Severity::Error,
                    "PyRegex",
                    format!("Regular expression: {}", e.message),
                ));
            }
        }
        out
    }
    fn legacy_grammar(issues: Vec<rustpy_grammar::Issue>) -> impl Iterator<Item = Diagnostic> {
        issues.into_iter().map(|i| {
            Diagnostic::new(
                legacy_range(i.range.start, i.range.end),
                Severity::Typo,
                "Grammar",
                format!("Grammar: {}", i.message),
            )
        })
    }
    fn legacy_spelling(typos: Vec<rustpy_spell::Typo>) -> impl Iterator<Item = Diagnostic> {
        typos.into_iter().map(|t| {
            Diagnostic::new(
                t.range,
                Severity::Typo,
                "SpellCheckingInspection",
                format!("Typo: In word '{}'", t.word),
            )
        })
    }
    fn legacy_range(start: usize, end: usize) -> TextRange {
        TextRange::new(TextSize::new(start as u32), TextSize::new(end as u32))
    }

    #[test]
    fn shared_python_bridge_preserves_legacy_cli_stage_order_and_stub_supplements() {
        const SOURCE: &str = "import re\n# a apple recieve\nlabel = 'é'\nre.compile(r'[')\nre.compile('[')\ndef f(x):\n    \"\"\"Args:\n        ghost: value\n    \"\"\"\n    return x\n";
        let dictionary = rustpy_spell::Dictionary::bundled();
        let dir = tempfile::tempdir().unwrap();
        let analysis = Analysis::for_project(dir.path());
        for source in [
            SOURCE,
            "# noinspection ALL\n# a apple recieve\nimport re\nre.compile(r'[')\n",
            "# noinspection PyDocstring\ndef f(x):\n    \"\"\"Args:\n        ghost: value\n    \"\"\"\n    return x\n",
        ] {
            for name in ["fixture.py", "fixture.pyi"] {
                let id = analysis.set_file_text(&dir.path().join(name), source);
                let file = analysis.file(id).unwrap();
                for grammar in [false, true] {
                    for spelling in [false, true] {
                        for incorrect_docstrings in [false, true] {
                            let groups = InspectionSettings {
                                grammar,
                                spelling,
                                ..Default::default()
                            };
                            let opts = InspectionOptions {
                                incorrect_docstrings,
                                ..inspection_options(&groups, 120, (3, 12))
                            };
                            let expected = legacy_python_diagnostics(
                                &analysis,
                                &file,
                                &opts,
                                &groups,
                                &dictionary,
                            );
                            let actual = python_diagnostics(
                                &analysis,
                                &file,
                                &opts,
                                &groups,
                                &dictionary,
                                &CancellationToken::new(),
                            )
                            .unwrap();
                            assert_eq!(
                                actual, expected,
                                "{name} grammar={grammar} spelling={spelling} incorrect_docstrings={incorrect_docstrings}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn shared_python_bridge_propagates_cancellation_without_partial_diagnostics() {
        let analysis = Analysis::for_project(Path::new("/cli-cancel-fixture"));
        let id =
            analysis.set_file_text(Path::new("/cli-cancel-fixture/main.py"), "undefined_name\n");
        let file = analysis.file(id).unwrap();
        let token = CancellationToken::new();
        token.cancel();
        assert_eq!(
            python_diagnostics(
                &analysis,
                &file,
                &InspectionOptions::default(),
                &InspectionSettings::default(),
                &rustpy_spell::Dictionary::bundled(),
                &token
            ),
            Err(Cancelled)
        );
    }

    #[test]
    fn shared_markdown_converters_preserve_language_specific_scanning_and_flags() {
        let text = "# a apple recieve\n\n```python\nundefined_name\n```\n";
        let dictionary = rustpy_spell::Dictionary::bundled();
        for grammar_enabled in [false, true] {
            for spelling_enabled in [false, true] {
                let groups = InspectionSettings {
                    grammar: grammar_enabled,
                    spelling: spelling_enabled,
                    ..Default::default()
                };
                let mut expected = Vec::new();
                if grammar_enabled {
                    expected.extend(legacy_grammar(rustpy_grammar::check_markdown(text)));
                }
                if spelling_enabled {
                    expected.extend(legacy_spelling(rustpy_spell::check_plain_text(
                        text,
                        &dictionary,
                    )));
                }
                assert_eq!(markdown_diagnostics(text, &groups, &dictionary), expected);
            }
        }
    }
    #[test]
    fn malformed_source_encoding_is_reported_instead_of_skipped() {
        let dir = project_with(&[("bad.py", "# coding: unknown-codec\nx=1\n")]);
        let out = dir.path().join("report");
        let a = args(dir.path(), &out, ReportFormat::Json);
        run_inspect(&a, &quiet_settings());
        let report = std::fs::read_to_string(out.join("PySyntaxError.json")).unwrap();
        assert!(
            report.contains("Cannot decode source") && report.contains("unknown-codec"),
            "{report}"
        );
    }

    #[test]
    fn large_left_associative_expression_has_enough_worker_stack() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("generated.py");
        std::fs::write(
            &path,
            format!("__all__ = {}\n", vec!["['a']"; 9500].join(" + ")),
        )
        .unwrap();
        let analysis = Analysis::for_project(dir.path());
        let groups = rustpy_settings::InspectionSettings {
            spelling: false,
            grammar: false,
            ..Default::default()
        };
        let profile = Profile::from_groups("Default", "test", groups);
        let opts = inspection_options(&profile.groups, 120, (3, 12));
        let mut visited = Vec::new();
        inspect_files(
            &analysis,
            dir.path(),
            std::slice::from_ref(&path),
            &profile,
            &opts,
            |p| visited.push(p.to_path_buf()),
        )
        .unwrap();
        assert_eq!(visited, [path]);
    }
    use crate::args::ReportFormat;

    fn project_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let p = dir.path().join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        dir
    }

    fn args(project: &Path, out: &Path, format: ReportFormat) -> InspectArgs {
        InspectArgs {
            project: project.into(),
            profile: "Project Default".into(),
            output: out.into(),
            dir: None,
            changes: false,
            format,
            verbosity: 1,
        }
    }

    fn quiet_settings() -> Settings {
        let mut s = Settings::default();
        // Proofreading depends on the bundled dictionary; keep the tests about Python.
        s.inspections.spelling = false;
        s.inspections.grammar = false;
        s
    }

    fn run_inspect(a: &InspectArgs, s: &Settings) -> (i32, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_with(a, s, Some(None), &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    const MAIN: &str = "import os\n\n\ndef f():\n    return undefined_name\n";

    #[test]
    fn report_ids_and_names() {
        assert_eq!(report_id("PEP8:E501"), "PyPep8");
        assert_eq!(report_id("syntax"), "PySyntaxError");
        assert_eq!(report_id("PyUnusedLocal"), "PyUnusedLocal");
        assert_eq!(
            describe("PyUnresolvedReferences"),
            ("Unresolved references".to_string(), "Python")
        );
        assert_eq!(describe("PyRaiseNotImplemented").0, "Raise not implemented");
        assert_eq!(describe("PySecurityFoo").1, "Python Security");
    }

    #[test]
    fn line_index_counts_characters() {
        let text = "aé\nbc\n";
        let idx = LineIndex::new(text);
        assert_eq!(idx.position(text, 0), (0, 0));
        assert_eq!(idx.position(text, 3), (0, 2), "é is one column");
        assert_eq!(idx.position(text, 4), (1, 0));
        assert_eq!(
            idx.position(text, 2),
            (0, 1),
            "inside a character: its start"
        );
    }

    #[test]
    fn inspects_a_project_into_xml_reports() {
        let dir = project_with(&[
            ("main.py", MAIN),
            ("pkg/__init__.py", ""),
            ("pkg/util.py", "def g(x):\n    y = 1\n    return x\n"),
            ("README.md", "# Readme\n"),
            (".venv/lib/bad.py", "import nothing_here\n"),
        ]);
        let out_dir = dir.path().join("out");
        let (code, out, err) = run_inspect(
            &args(dir.path(), &out_dir, ReportFormat::Xml),
            &quiet_settings(),
        );
        assert_eq!(code, 1, "an unresolved reference is an error: {out}{err}");
        let unresolved =
            std::fs::read_to_string(out_dir.join("PyUnresolvedReferences.xml")).unwrap();
        assert!(unresolved.starts_with("<?xml"), "{unresolved}");
        assert!(
            unresolved.contains("<file>file://$PROJECT_DIR$/main.py</file>"),
            "{unresolved}"
        );
        assert!(unresolved.contains("<line>5</line>"), "{unresolved}");
        assert!(
            unresolved.contains("Unresolved reference &apos;undefined_name&apos;")
                || unresolved.contains("Unresolved reference 'undefined_name'"),
            "{unresolved}"
        );
        assert!(
            unresolved.contains(r#"<problem_class id="PyUnresolvedReferences" severity="ERROR""#),
            "{unresolved}"
        );
        assert!(
            !unresolved.contains("bad.py"),
            "virtual environments are not inspected"
        );
        let unused = std::fs::read_to_string(out_dir.join("PyUnusedImports.xml")).unwrap();
        assert!(
            unused.contains("<line>1</line>")
                && unused.contains("<highlighted_element>os</highlighted_element>"),
            "{unused}"
        );
        assert!(
            out_dir.join("PyUnusedLocal.xml").exists(),
            "y = 1 is unused"
        );
        let desc = std::fs::read_to_string(out_dir.join(".descriptions.xml")).unwrap();
        assert!(
            desc.contains(r#"<inspections profile="Project Default">"#)
                && desc.contains(r#"shortName="PyUnresolvedReferences""#),
            "{desc}"
        );
        assert!(out.contains("file(s) inspected"), "{out}");
    }

    #[test]
    fn profiles_switch_inspections_off() {
        let dir = project_with(&[("main.py", "import os\n")]);
        let out_dir = dir.path().join("out");
        let mut s = quiet_settings();
        s.inspections.unused_imports = false;
        let (code, out, _) = run_inspect(&args(dir.path(), &out_dir, ReportFormat::Json), &s);
        assert_eq!(code, 0, "{out}");
        assert!(!out_dir.join("PyUnusedImports.json").exists());
        let (code, _, _) = run_inspect(
            &InspectArgs {
                profile: "Default".into(),
                ..args(dir.path(), &out_dir, ReportFormat::Json)
            },
            &s,
        );
        assert_eq!(code, 0);
        let json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(out_dir.join("PyUnusedImports.json")).unwrap(),
        )
        .unwrap();
        let p = &json["problems"][0];
        assert_eq!(p["line"], 1);
        assert_eq!(p["problem_class"]["id"], "PyUnusedImports");
        assert_eq!(p["problem_class"]["severity"], "WARNING");
        assert_eq!(p["highlighted_element"], "os");
        let desc: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(out_dir.join(".descriptions.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(desc["profile"], "Default");
    }

    #[test]
    fn xml_profile_severity_and_ignores() {
        let profile = "<profile version=\"1.0\"><option name=\"myName\" value=\"CI\"/>\
            <inspection_tool class=\"PyUnresolvedReferencesInspection\" enabled=\"true\" level=\"WARNING\" enabled_by_default=\"true\"/>\
            <inspection_tool class=\"PyUnusedImportsInspection\" enabled=\"false\" level=\"WARNING\" enabled_by_default=\"false\"/></profile>";
        let dir = project_with(&[("main.py", MAIN), ("ci.xml", profile)]);
        let out_dir = dir.path().join("out");
        let (code, out, err) = run_inspect(
            &InspectArgs {
                profile: "ci.xml".into(),
                ..args(dir.path(), &out_dir, ReportFormat::Sarif)
            },
            &quiet_settings(),
        );
        assert_eq!(
            code, 0,
            "the profile lowers unresolved references to warnings: {out}{err}"
        );
        let sarif: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(out_dir.join("results.sarif")).unwrap())
                .unwrap();
        assert_eq!(sarif["version"], "2.1.0");
        let results = sarif["runs"][0]["results"].as_array().unwrap();
        let r = results
            .iter()
            .find(|r| r["ruleId"] == "PyUnresolvedReferences")
            .expect("unresolved reference");
        assert_eq!(r["level"], "warning");
        assert_eq!(
            r["locations"][0]["physicalLocation"]["artifactLocation"]["uri"],
            "main.py"
        );
        assert_eq!(
            r["locations"][0]["physicalLocation"]["region"]["startLine"],
            5
        );
        assert_eq!(
            r["locations"][0]["physicalLocation"]["region"]["startColumn"],
            12
        );
        assert!(
            !results.iter().any(|r| r["ruleId"] == "PyUnusedImports"),
            "switched off by the profile"
        );
        let rules = sarif["runs"][0]["tool"]["driver"]["rules"]
            .as_array()
            .unwrap();
        assert_eq!(
            rules[r["ruleIndex"].as_u64().unwrap() as usize]["id"],
            "PyUnresolvedReferences"
        );
    }

    #[test]
    fn plain_output_subdirectory_and_failures() {
        let dir = project_with(&[("a/x.py", "import os\n"), ("b/y.py", "import sys\n")]);
        let out_dir = dir.path().join("out");
        let (code, out, _) = run_inspect(
            &InspectArgs {
                dir: Some("a".into()),
                ..args(dir.path(), &out_dir, ReportFormat::Plain)
            },
            &quiet_settings(),
        );
        assert_eq!(code, 0, "{out}");
        assert!(
            out.contains(
                "a/x.py:1:8: WARNING: Unused import statement 'import os' [PyUnusedImports]"
            ),
            "{out}"
        );
        assert!(!out.contains("y.py"), "{out}");
        let txt = std::fs::read_to_string(out_dir.join("inspections.txt")).unwrap();
        assert!(txt.contains("a/x.py:1:8"), "{txt}");
        let (code, _, err) = run_inspect(
            &InspectArgs {
                profile: "Nope".into(),
                ..args(dir.path(), &out_dir, ReportFormat::Plain)
            },
            &quiet_settings(),
        );
        assert_eq!(code, 2);
        assert!(err.contains("not found"), "{err}");
        let (code, _, err) = run_inspect(
            &args(&dir.path().join("missing"), &out_dir, ReportFormat::Xml),
            &quiet_settings(),
        );
        assert_eq!(code, 2, "{err}");
        let (code, _, err) = run_inspect(
            &InspectArgs {
                dir: Some("/".into()),
                ..args(dir.path(), &out_dir, ReportFormat::Xml)
            },
            &quiet_settings(),
        );
        assert_eq!(code, 2, "{err}");
    }

    #[test]
    fn only_uncommitted_changes() {
        let dir = project_with(&[("old.py", "import os\n"), ("new.py", "import sys\n")]);
        let root = dir.path();
        let repo = rustpy_vcs::GitRepo::init(root).unwrap();
        // Commit old.py, leave new.py untracked.
        if repo.commit(&[root.join("old.py")], "init", false).is_err() {
            return; // no Git identity configured on this machine
        }
        let out_dir = root.join("out");
        let (code, out, _) = run_inspect(
            &InspectArgs {
                changes: true,
                verbosity: 2,
                ..args(root, &out_dir, ReportFormat::Plain)
            },
            &quiet_settings(),
        );
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("new.py") && !out.contains("old.py"), "{out}");
        let plain = project_with(&[("a.py", "")]);
        let (code, _, err) = run_inspect(
            &InspectArgs {
                changes: true,
                ..args(plain.path(), &out_dir, ReportFormat::Plain)
            },
            &quiet_settings(),
        );
        if rustpy_vcs::GitRepo::discover(plain.path()).is_none() {
            assert_eq!(code, 2);
            assert!(err.contains("Git repository"), "{err}");
        }
    }

    #[test]
    fn markdown_is_proofread() {
        let dir = project_with(&[("README.md", "This is the the test.\n")]);
        let out_dir = dir.path().join("out");
        let mut s = Settings::default();
        s.inspections.spelling = false;
        let (_, out, _) = run_inspect(
            &InspectArgs {
                verbosity: 2,
                ..args(dir.path(), &out_dir, ReportFormat::Plain)
            },
            &s,
        );
        assert!(
            out.contains("README.md:1:") && out.contains("[Grammar]"),
            "{out}"
        );
    }
    #[test]
    fn saved_project_structure_drives_actual_inspection_and_import_roots() {
        let dir = project_with(&[
            ("src/main.py", "import helper\n"),
            ("tests/check.py", "x = 1\n"),
            ("vendor/hidden.py", "undefined_hidden\n"),
            ("ignored.py", "undefined_ignored\n"),
            (".git/config", ""),
            (".gitignore", "ignored.py\n"),
        ]);
        let shared = tempfile::tempdir().unwrap();
        std::fs::write(shared.path().join("helper.py"), "value = 1\n").unwrap();
        let model = rustpy_project::ProjectModel {
            content_roots: vec![shared.path().into()],
            source_roots: vec![dir.path().join("src")],
            test_roots: vec![dir.path().join("tests")],
            excluded: vec![dir.path().join("vendor")],
            ..Default::default()
        };
        model.save(dir.path()).unwrap();
        let snapshot = WorkspaceSnapshot::try_load(vec![dir.path().into()]).unwrap();
        let analysis = project_analysis(dir.path(), &snapshot, None);
        assert_eq!(analysis.resolver.roots, snapshot.roots());
        assert!(analysis.resolver.roots.contains(&dir.path().join("src")));
        assert!(analysis.resolver.roots.contains(&dir.path().join("tests")));
        assert!(analysis
            .resolver
            .roots
            .contains(&shared.path().to_path_buf()));
        let outdir = tempfile::tempdir().unwrap();
        let mut a = args(dir.path(), outdir.path(), ReportFormat::Plain);
        a.verbosity = 2;
        let (_, output, error) = run_inspect(&a, &quiet_settings());
        assert!(error.is_empty(), "{error}");
        assert!(
            output.contains("Inspecting src/main.py")
                && output.contains("Inspecting tests/check.py"),
            "{output}"
        );
        assert!(
            output.contains(&format!(
                "Inspecting {}",
                shared.path().join("helper.py").display()
            )),
            "{output}"
        );
        assert!(
            !output.contains("Inspecting vendor/hidden.py")
                && !output.contains("Inspecting ignored.py"),
            "{output}"
        );
        a.dir = Some(dir.path().join("src"));
        let (_, scoped, error) = run_inspect(&a, &quiet_settings());
        assert!(error.is_empty(), "{error}");
        assert!(
            !scoped.contains("Inspecting tests/check.py")
                && !scoped.contains(&shared.path().join("helper.py").display().to_string()),
            "{scoped}"
        );
    }

    #[test]
    fn malformed_shared_structure_fails_inspection_without_writing_reports() {
        let dir = project_with(&[
            ("main.py", "undefined_name\n"),
            (".rustpy/project.json", "<<<<<<< conflict"),
        ]);
        let outdir = tempfile::tempdir().unwrap();
        let a = args(
            dir.path(),
            &outdir.path().join("reports"),
            ReportFormat::Plain,
        );
        let (code, _, error) = run_inspect(&a, &quiet_settings());
        assert_eq!(code, 2);
        assert!(
            error.contains("project.json") && error.contains("not valid"),
            "{error}"
        );
        assert!(!a.output.exists());
    }
}
