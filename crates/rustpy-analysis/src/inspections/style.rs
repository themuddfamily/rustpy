//! Configurable style checks; no user Python is imported or evaluated.
use super::*;
use rustpy_semantic::infer::Inferer;
use rustpy_settings::style_inspections::StyleInspections;
mod docstrings;
mod metrics;
mod patterns;
mod pep8_backend;
#[cfg(test)]
mod tests;
pub(super) fn settings(opts: &InspectionOptions) -> StyleInspections {
    let mut s = opts.style.clone();
    if s.quote_style == "code_style" {
        s.quote_style = "preserve".into()
    }
    let Some(policy) = &opts.policy else { return s };
    let option = |id: &str, key: &str| {
        policy
            .rule(id)
            .and_then(|rule| rule.options.get(key))
            .filter(|v| !v.trim().is_empty())
    };
    macro_rules! boolean{($id:expr,$($field:ident),+)=>{$(if let Some(v)=option($id,stringify!($field)){if let Ok(v)=v.parse(){s.$field=v}})+}}
    macro_rules! number{($id:expr,$($field:ident),+)=>{$(if let Some(v)=option($id,stringify!($field)){if let Ok(v)=v.parse(){s.$field=v}})+}}
    boolean!("PyPep8Naming", ignore_overrides, check_constants);
    boolean!(
        "PyMissingOrEmptyDocstring",
        doc_modules,
        doc_classes,
        doc_functions,
        doc_methods,
        doc_private
    );
    boolean!("PyDocstringStyle", single_quoted_docstrings);
    boolean!(
        "PySourceStyle",
        mandatory_encoding,
        missing_shebang,
        non_ascii_identifiers,
        non_ascii_comments,
        trailing_comma
    );
    boolean!("PyNonAscii", non_ascii_identifiers, non_ascii_comments);
    if policy
        .rule("PyDuplicatedCode")
        .is_some_and(|rule| rule.enabled && !rule.options.contains_key("duplicate_code"))
    {
        s.duplicate_code = true
    }
    boolean!("PyDuplicatedCode", duplicate_code);
    number!(
        "PyFunctionMetrics",
        max_arguments,
        max_branches,
        max_returns,
        max_statements,
        max_locals
    );
    number!(
        "PyDuplicatedCode",
        duplicate_min_lines,
        duplicate_min_tokens
    );
    for (id, key, target) in [
        ("PyPep8", "ignoredErrors", &mut s.pep8_ignore),
        ("PyPep8Naming", "ignoredNames", &mut s.naming_ignore),
        (
            "PyPep8Naming",
            "ignoredPatterns",
            &mut s.naming_ignore_patterns,
        ),
    ] {
        if let Some(v) = option(id, key) {
            *target = v
                .split([',', ';', '\n'])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        }
    }
    if let Some(v) = option("PyPep8", "backend") {
        s.pep8_backend = v.clone()
    }
    if let Some(v) = option("PySourceStyle", "quote_style") {
        if v != "code_style" {
            s.quote_style = v.clone()
        }
    }
    s
}
pub(super) fn ignored_name(s: &StyleInspections, name: &str) -> bool {
    name == "_"
        || s.naming_ignore.iter().any(|n| n == name)
        || s.naming_ignore_patterns
            .iter()
            .any(|pattern| glob(pattern, name))
}
fn glob(pattern: &str, name: &str) -> bool {
    if pattern.len() > 256 || name.len() > 1024 {
        return false;
    }
    let p: Vec<_> = pattern.chars().collect();
    let n: Vec<_> = name.chars().collect();
    let (mut i, mut j, mut star, mut retry) = (0, 0, None, 0);
    while j < n.len() {
        if i < p.len() && (p[i] == '?' || p[i] == n[j]) {
            i += 1;
            j += 1
        } else if i < p.len() && p[i] == '*' {
            star = Some(i);
            i += 1;
            retry = j
        } else if let Some(at) = star {
            retry += 1;
            j = retry;
            i = at + 1
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == '*' {
        i += 1
    }
    i == p.len()
}
pub(super) fn pep8(
    file: &Arc<FileInfo>,
    opts: &InspectionOptions,
    token: &CancellationToken,
) -> Vec<Problem> {
    let settings = settings(opts);
    let length = opts.policy.as_ref().map_or(opts.max_line_length, |p| {
        p.line_length(opts.max_line_length)
    });
    let mut out = if settings.pep8_backend != "builtin" && settings.interpreter.is_some() {
        // A cancelled run's result is discarded: skip the fix and fallback passes.
        match pep8_backend::run(file, &settings, length, token) {
            _ if token.is_cancelled() => return Vec::new(),
            Ok(mut problems) => {
                let fixes = super::pep8(file, length)
                    .into_iter()
                    .filter(|problem| !problem.fixes.is_empty())
                    .map(|problem| {
                        (
                            (
                                problem.diagnostic.code.to_string(),
                                problem.diagnostic.range.start(),
                            ),
                            problem.fixes,
                        )
                    })
                    .collect::<std::collections::HashMap<_, _>>();
                for problem in &mut problems {
                    if let Some(quick) = fixes.get(&(
                        problem.diagnostic.code.to_string(),
                        problem.diagnostic.range.start(),
                    )) {
                        problem.fixes = quick.clone();
                    }
                }
                problems
            }
            Err(message) => {
                let mut problems = super::pep8(file, length);
                if settings.pep8_backend == "external" {
                    problems.push(Problem {
                        diagnostic: Diagnostic::new(
                            TextRange::empty(ts(0)),
                            Severity::WeakWarning,
                            "PyStyleBackend",
                            message,
                        ),
                        fixes: Vec::new(),
                    })
                }
                problems
            }
        }
    } else {
        super::pep8(file, length)
    };
    if settings.pep8_backend == "external" && settings.interpreter.is_none() {
        out.push(Problem {
            diagnostic: Diagnostic::new(
                TextRange::empty(ts(0)),
                Severity::WeakWarning,
                "PyStyleBackend",
                "Select a trusted Python 3.9+ interpreter for complete pycodestyle checks",
            ),
            fixes: Vec::new(),
        });
    }
    out.retain(|problem| {
        !settings.pep8_ignore.iter().any(|ignore| {
            problem
                .diagnostic
                .code
                .strip_prefix("PEP8:")
                .is_some_and(|code| code.starts_with(ignore))
        })
    });
    out
}
pub(super) fn run(
    inf: &Inferer<'_>,
    file: &Arc<FileInfo>,
    opts: &InspectionOptions,
    sink: &mut Sink,
) {
    let config = settings(opts);
    patterns::run(inf, file, &config, sink);
    if opts.incorrect_docstrings && inf.db.analyze_doc_comments() {
        docstrings::run(inf, file, &config, sink)
    }
    metrics::run(inf.db, file, &config, sink);
}
