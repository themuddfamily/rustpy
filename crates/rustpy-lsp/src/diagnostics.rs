//! Diagnostics: syntax errors and the IDE's inspections (the same set the
//! editor shows for the active file: `inspections::inspect`, docstring /
//! signature mismatches, regex errors, grammar and spelling), with their quick
//! fixes kept for code actions.

use std::sync::Arc;

use lsp_types::{Diagnostic, DiagnosticSeverity, DiagnosticTag, NumberOrString};
pub use rustpy_analysis::file_analysis::Problem;
use rustpy_analysis::file_analysis::{
    self, DictionaryActionPolicy, FileAnalysisOptions, StubSupplementPolicy,
};
use rustpy_core::Severity;

use crate::snapshot::{DocView, HResult, LspError, Snapshot};

pub type FileProblems = Vec<Problem>;

/// All problems of `doc`, in text order.
pub fn problems(snap: &Snapshot, doc: &DocView) -> HResult<FileProblems> {
    snap.check_cancelled()?;
    let file = &doc.file;
    let settings = &snap.config.settings;
    // "Add import" fixes only concern names the file uses.
    let referenced: std::collections::HashSet<&str> = file
        .scopes
        .references
        .iter()
        .map(|r| r.name.as_str())
        .collect();
    let mut opts = snap.config.inspection_options(
        snap.index.project_symbols(|n| referenced.contains(n)),
        snap.python_version,
    );
    if snap.trusted {
        if opts.style.interpreter.is_none() {
            opts.style.interpreter = snap.interpreter.clone()
        }
    } else {
        opts.style.interpreter = None;
        opts.style.pep8_backend = "builtin".into()
    }
    let dictionary = (settings.inspections.spelling && !file.is_stub).then(spell_dictionary);
    let options = FileAnalysisOptions {
        inspections: &opts,
        grammar: settings.inspections.grammar,
        dictionary: dictionary.as_deref(),
        stub_supplements: StubSupplementPolicy::InspectionOnly,
        dictionary_actions: DictionaryActionPolicy::ReplacementsOnly,
    };
    let mut out = file_analysis::analyze_python(&snap.analysis, file, &options, &snap.cancel)
        .map_err(|_| LspError::cancelled())?;
    // Preserve the legacy stub early return and stable supplemental text order.
    if !file.is_stub {
        out.sort_by_key(|p| p.diagnostic.range.start());
    }
    snap.check_cancelled()?;
    Ok(out)
}

/// The dictionary with the user's words, and the dictionary file's modification time.
type CachedDictionary = Option<(Option<std::time::SystemTime>, Arc<rustpy_spell::Dictionary>)>;

/// The user dictionary the IDE's spell checker uses (`<config>/dictionary.txt`).
fn spell_dictionary() -> Arc<rustpy_spell::Dictionary> {
    static DICT: std::sync::OnceLock<parking_lot::Mutex<CachedDictionary>> =
        std::sync::OnceLock::new();
    let path = rustpy_settings::Settings::config_dir().join("dictionary.txt");
    let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    let mut cell = DICT.get_or_init(Default::default).lock();
    if let Some((stamp, d)) = &*cell {
        if *stamp == mtime {
            return d.clone();
        }
    }
    let words = rustpy_spell::load_user_words(&path).unwrap_or_default();
    let d = Arc::new(rustpy_spell::Dictionary::bundled().with_user_words(&words));
    *cell = Some((mtime, d.clone()));
    d
}

pub fn severity(s: Severity) -> DiagnosticSeverity {
    match s {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
        Severity::WeakWarning | Severity::Typo => DiagnosticSeverity::INFORMATION,
        Severity::Info => DiagnosticSeverity::HINT,
    }
}

/// The LSP form of an IDE diagnostic: severity, inspection id as the code,
/// and the "unnecessary" tag for code the editor greys out.
pub fn to_lsp(d: &rustpy_core::Diagnostic, doc: &DocView, snap: &Snapshot) -> Diagnostic {
    let unnecessary = matches!(
        d.code.as_str(),
        "PyUnusedImports" | "PyUnusedLocal" | "PyUnreachableCode" | "PyRedundantParentheses"
    );
    let tags = (unnecessary && snap.caps.diagnostic_tags).then(|| vec![DiagnosticTag::UNNECESSARY]);
    Diagnostic {
        range: doc.index.range(d.range, snap.enc),
        severity: Some(severity(d.severity)),
        code: Some(NumberOrString::String(d.code.to_string())),
        code_description: None,
        source: Some("RustPy".into()),
        message: d.message.clone(),
        related_information: None,
        tags,
        data: None,
    }
}

pub fn compute(snap: &Snapshot, doc: &DocView) -> HResult<(Vec<Diagnostic>, Arc<FileProblems>)> {
    let problems = problems(snap, doc)?;
    let diags = problems
        .iter()
        .map(|p| to_lsp(&p.diagnostic, doc, snap))
        .collect();
    snap.check_cancelled()?;
    Ok((diags, Arc::new(problems)))
}
