//! Shared Python diagnostic composition. Clients own scheduling, dictionary IO,
//! library-index enrichment, inlays, presentation ordering and protocol mapping.
use crate::inspections::{self, InspectionOptions};
pub use crate::inspections::{Problem, QuickFix};
use rustpy_core::{CancellationToken, Cancelled, Diagnostic, Severity, TextRange, TextSize};
use rustpy_semantic::{Analysis, FileInfo};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StubSupplementPolicy {
    InspectionOnly,
    Include,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DictionaryActionPolicy {
    ReplacementsOnly,
    IncludeSaveWord,
}

pub struct FileAnalysisOptions<'a> {
    pub inspections: &'a InspectionOptions,
    pub grammar: bool,
    pub dictionary: Option<&'a rustpy_spell::Dictionary>,
    pub stub_supplements: StubSupplementPolicy,
    pub dictionary_actions: DictionaryActionPolicy,
}

pub fn grammar_problems(issues: &[rustpy_grammar::Issue]) -> Vec<Problem> {
    issues
        .iter()
        .map(|issue| {
            let range = TextRange::new(
                TextSize::new(issue.range.start as u32),
                TextSize::new(issue.range.end as u32),
            );
            Problem {
                diagnostic: Diagnostic {
                    range,
                    severity: Severity::Typo,
                    message: format!("Grammar: {}", issue.message),
                    code: "Grammar".into(),
                },
                fixes: issue
                    .replacements
                    .iter()
                    .map(|replacement| QuickFix {
                        title: format!("Replace with '{replacement}'"),
                        edits: vec![(range, replacement.clone())],
                    })
                    .collect(),
            }
        })
        .collect()
}

pub fn spelling_problems(
    typos: &[rustpy_spell::Typo],
    actions: DictionaryActionPolicy,
) -> Vec<Problem> {
    typos
        .iter()
        .map(|typo| {
            let mut fixes: Vec<_> = typo
                .suggestions
                .iter()
                .take(5)
                .map(|replacement| QuickFix {
                    title: format!("Change to '{replacement}'"),
                    edits: vec![(typo.range, replacement.clone())],
                })
                .collect();
            if actions == DictionaryActionPolicy::IncludeSaveWord {
                fixes.push(QuickFix {
                    title: format!("Save '{}' to dictionary", typo.word.to_lowercase()),
                    edits: Vec::new(),
                });
            }
            Problem {
                diagnostic: Diagnostic {
                    range: typo.range,
                    severity: Severity::Typo,
                    message: format!("Typo: In word '{}'", typo.word),
                    code: "SpellCheckingInspection".into(),
                },
                fixes,
            }
        })
        .collect()
}

/// Returns stage order: inspections, grammar, spelling, legacy docstrings, regex.
/// The legacy docstring supplement remains a separate producer, with shared
/// final suppression and profile filtering. Cancellation never returns partial data.
/// Leaf scans currently checkpoint between stages, rather than within a scan.
pub fn analyze_python(
    analysis: &Analysis,
    file: &Arc<FileInfo>,
    options: &FileAnalysisOptions<'_>,
    cancel: &CancellationToken,
) -> Result<Vec<Problem>, Cancelled> {
    analyze_with_checkpoint(analysis, file, options, cancel, |_| {})
}

/// Applies an explicit validated grammar policy without changing legacy callers.
pub fn analyze_python_with_grammar_policy(
    analysis: &Analysis,
    file: &Arc<FileInfo>,
    options: &FileAnalysisOptions<'_>,
    cancel: &CancellationToken,
    policy: rustpy_grammar::policy::Policy,
) -> Result<Vec<Problem>, Cancelled> {
    analyze_with_policy_checkpoint(analysis, file, options, cancel, Some(policy), |_| {})
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Inspections,
    Grammar,
    Spelling,
    Docstrings,
    Regex,
    Complete,
}

fn analyze_with_checkpoint(
    analysis: &Analysis,
    file: &Arc<FileInfo>,
    options: &FileAnalysisOptions<'_>,
    cancel: &CancellationToken,
    checkpoint: impl FnMut(Stage),
) -> Result<Vec<Problem>, Cancelled> {
    analyze_with_policy_checkpoint(analysis, file, options, cancel, None, checkpoint)
}

fn analyze_with_policy_checkpoint(
    analysis: &Analysis,
    file: &Arc<FileInfo>,
    options: &FileAnalysisOptions<'_>,
    cancel: &CancellationToken,
    grammar_policy: Option<rustpy_grammar::policy::Policy>,
    mut checkpoint: impl FnMut(Stage),
) -> Result<Vec<Problem>, Cancelled> {
    let mut check = |stage| {
        checkpoint(stage);
        cancel.check()
    };
    check(Stage::Inspections)?;
    let mut problems =
        inspections::inspect_cancellable(analysis, file, options.inspections, cancel)?;
    cancel.check()?;
    if file.is_stub && options.stub_supplements == StubSupplementPolicy::InspectionOnly {
        return Ok(problems);
    }
    let text = file.text.as_ref();
    let configured_language = if options.inspections.editorconfig_enabled {
        options
            .inspections
            .editorconfig
            .as_ref()
            .map(|props| props.spelling_language.clone())
            .unwrap_or_else(|| {
                rustpy_settings::editorconfig::resolve_cached(&file.path).spelling_language
            })
    } else {
        None
    };
    let language = configured_language
        .as_deref()
        .and_then(|language| rustpy_grammar::policy::Language::from_key(language).ok());
    let supported_language = configured_language.is_none() || language.is_some();
    let grammar_policy = language
        .map(|language| {
            let mut policy = grammar_policy.unwrap_or_default();
            policy.language = language;
            policy
        })
        .or(grammar_policy);
    check(Stage::Grammar)?;
    if options.grammar && supported_language {
        let issues = match grammar_policy {
            Some(policy) => {
                rustpy_grammar::policy::check(text, rustpy_grammar::policy::Context::Python, policy)
            }
            None => rustpy_grammar::check_python(text),
        };
        problems.extend(grammar_problems(&issues));
    }
    check(Stage::Spelling)?;
    if let Some(dictionary) = options.dictionary.filter(|_| supported_language) {
        problems.extend(spelling_problems(
            &rustpy_spell::check_text(text, dictionary, rustpy_spell::CheckOptions::default()),
            options.dictionary_actions,
        ));
    }
    check(Stage::Docstrings)?;
    let docstring_problems = if analysis.analyze_doc_comments() {
        rustpy_semantic::docstrings::docstring_problems(analysis, file)
    } else {
        Vec::new()
    };
    for (range, message) in docstring_problems {
        cancel.check()?;
        problems.push(Problem {
            diagnostic: Diagnostic {
                range,
                severity: Severity::WeakWarning,
                message,
                code: "PyDocstring".into(),
            },
            fixes: Vec::new(),
        });
    }
    check(Stage::Regex)?;
    for injection in rustpy_regex::injections(text, &file.parse) {
        cancel.check()?;
        for (error, severity) in rustpy_regex::check(&injection.pattern, injection.verbose)
            .into_iter()
            .map(|e| (e, Severity::Error))
            .chain(
                rustpy_regex::inspection_checks::redundant_escapes(
                    &injection.pattern,
                    injection.verbose,
                )
                .into_iter()
                .map(|e| (e, Severity::WeakWarning)),
            )
        {
            cancel.check()?;
            let raw = text
                .get(usize::from(injection.string_range.start())..)
                .is_some_and(|s| s.starts_with(['r', 'R']));
            let range = if raw {
                TextRange::new(
                    injection.value_range.start() + error.range.start(),
                    (injection.value_range.start() + error.range.end())
                        .min(injection.value_range.end()),
                )
            } else {
                injection.value_range
            };
            problems.push(Problem {
                diagnostic: Diagnostic {
                    range,
                    severity,
                    message: format!("Regular expression: {}", error.message),
                    code: "PyRegex".into(),
                },
                fixes: if raw && severity == Severity::WeakWarning {
                    vec![QuickFix {
                        title: "Remove redundant regular-expression escape".into(),
                        edits: vec![(
                            range,
                            injection.pattern
                                [usize::from(error.range.start())..usize::from(error.range.end())]
                                .trim_start_matches('\\')
                                .to_string(),
                        )],
                    }]
                } else {
                    Vec::new()
                },
            });
        }
    }
    check(Stage::Complete)?;
    // Supplemental producers share the same comment policy as core inspections.
    // Suppressions deliberately retain syntax errors.
    let suppressions = inspections::Suppressions::new(file);
    problems.retain(|problem| !suppressions.is_suppressed(&problem.diagnostic));
    if let Some(policy) = &options.inspections.policy {
        policy.apply(&file.path, &mut problems);
    }
    Ok(problems)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const SOURCE: &str = "import re\n# a apple recieve\nlabel = 'é'\nre.compile(r'[')\nre.compile('[')\ndef f(x):\n    \"\"\"Args:\n        ghost: value\n    \"\"\"\n    return x\n";
    fn run(
        path: &str,
        source: &str,
        policy: StubSupplementPolicy,
        actions: DictionaryActionPolicy,
        grammar: bool,
        spelling: bool,
    ) -> Vec<Problem> {
        let db = Analysis::for_project(Path::new("/analysis-service-fixture"));
        let id = db.set_file_text(Path::new(path), source);
        let file = db.file(id).unwrap();
        let inspections = InspectionOptions::default();
        let dictionary = rustpy_spell::Dictionary::bundled();
        analyze_python(
            &db,
            &file,
            &FileAnalysisOptions {
                inspections: &inspections,
                grammar,
                dictionary: spelling.then_some(&dictionary),
                stub_supplements: policy,
                dictionary_actions: actions,
            },
            &CancellationToken::new(),
        )
        .unwrap()
    }

    // Frozen legacy LSP composition, independent of the shared converters.
    fn legacy_problems(
        db: &Analysis,
        file: &Arc<FileInfo>,
        opts: &InspectionOptions,
        dictionary: &rustpy_spell::Dictionary,
    ) -> Vec<Problem> {
        let mut out = inspections::inspect(db, file, opts);
        let text: &str = &file.text;
        for issue in rustpy_grammar::check_python(text) {
            let range = TextRange::new(
                (issue.range.start as u32).into(),
                (issue.range.end as u32).into(),
            );
            let fixes = issue
                .replacements
                .iter()
                .map(|r| QuickFix {
                    title: format!("Replace with '{r}'"),
                    edits: vec![(range, r.clone())],
                })
                .collect();
            out.push(Problem {
                diagnostic: Diagnostic {
                    range,
                    severity: Severity::Typo,
                    message: format!("Grammar: {}", issue.message),
                    code: "Grammar".into(),
                },
                fixes,
            });
        }
        for typo in
            rustpy_spell::check_text(text, dictionary, rustpy_spell::CheckOptions::default())
        {
            let fixes = typo
                .suggestions
                .iter()
                .take(5)
                .map(|s| QuickFix {
                    title: format!("Change to '{s}'"),
                    edits: vec![(typo.range, s.clone())],
                })
                .collect();
            out.push(Problem {
                diagnostic: Diagnostic {
                    range: typo.range,
                    severity: Severity::Typo,
                    message: format!("Typo: In word '{}'", typo.word),
                    code: "SpellCheckingInspection".into(),
                },
                fixes,
            });
        }
        for (range, message) in rustpy_semantic::docstrings::docstring_problems(db, file) {
            out.push(Problem {
                diagnostic: Diagnostic {
                    range,
                    severity: Severity::WeakWarning,
                    message,
                    code: "PyDocstring".into(),
                },
                fixes: Vec::new(),
            });
        }
        for inj in rustpy_regex::injections(text, &file.parse) {
            for (e, severity) in rustpy_regex::check(&inj.pattern, inj.verbose)
                .into_iter()
                .map(|e| (e, Severity::Error))
                .chain(
                    rustpy_regex::inspection_checks::redundant_escapes(&inj.pattern, inj.verbose)
                        .into_iter()
                        .map(|e| (e, Severity::WeakWarning)),
                )
            {
                let raw = text
                    .get(usize::from(inj.string_range.start())..)
                    .is_some_and(|s| s.starts_with(['r', 'R']));
                let range = if raw {
                    TextRange::new(
                        inj.value_range.start() + e.range.start(),
                        (inj.value_range.start() + e.range.end()).min(inj.value_range.end()),
                    )
                } else {
                    inj.value_range
                };
                out.push(Problem {
                    diagnostic: Diagnostic {
                        range,
                        severity,
                        message: format!("Regular expression: {}", e.message),
                        code: "PyRegex".into(),
                    },
                    fixes: if raw && severity == Severity::WeakWarning {
                        vec![QuickFix {
                            title: "Remove redundant regular-expression escape".into(),
                            edits: vec![(
                                range,
                                inj.pattern
                                    [usize::from(e.range.start())..usize::from(e.range.end())]
                                    .trim_start_matches('\\')
                                    .to_string(),
                            )],
                        }]
                    } else {
                        Vec::new()
                    },
                });
            }
        }
        out
    }

    #[test]
    fn legacy_client_composition_matches_exact_problems_before_and_after_sort() {
        let db = Analysis::for_project(Path::new("/analysis-service-fixture"));
        let id = db.set_file_text(Path::new("/analysis-service-fixture/m.py"), SOURCE);
        let file = db.file(id).unwrap();
        let inspections = InspectionOptions::default();
        let dictionary = rustpy_spell::Dictionary::bundled();
        let mut before = legacy_problems(&db, &file, &inspections, &dictionary);
        let mut after = analyze_python(
            &db,
            &file,
            &FileAnalysisOptions {
                inspections: &inspections,
                grammar: true,
                dictionary: Some(&dictionary),
                stub_supplements: StubSupplementPolicy::Include,
                dictionary_actions: DictionaryActionPolicy::ReplacementsOnly,
            },
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(after, before);
        before.sort_by_key(|p| p.diagnostic.range.start());
        after.sort_by_key(|p| p.diagnostic.range.start());
        assert_eq!(after, before);
    }

    #[test]
    fn composition_preserves_stages_ranges_and_client_dictionary_actions() {
        let replacements = run(
            "/analysis-service-fixture/m.py",
            SOURCE,
            StubSupplementPolicy::Include,
            DictionaryActionPolicy::ReplacementsOnly,
            true,
            true,
        );
        let ui = run(
            "/analysis-service-fixture/m.py",
            SOURCE,
            StubSupplementPolicy::Include,
            DictionaryActionPolicy::IncludeSaveWord,
            true,
            true,
        );
        assert_eq!(
            replacements
                .iter()
                .map(|p| &p.diagnostic)
                .collect::<Vec<_>>(),
            ui.iter().map(|p| &p.diagnostic).collect::<Vec<_>>()
        );
        let stages: Vec<_> = replacements
            .iter()
            .filter_map(|p| match p.diagnostic.code.as_str() {
                "Grammar" => Some(1),
                "SpellCheckingInspection" => Some(2),
                "PyDocstring" => Some(3),
                "PyRegex" => Some(4),
                _ => None,
            })
            .collect();
        assert!(stages.windows(2).all(|w| w[0] <= w[1]));
        assert!(stages.contains(&1) && stages.contains(&2) && stages.contains(&4));
        for (plain, ui) in replacements.iter().zip(&ui) {
            if plain.diagnostic.code == "SpellCheckingInspection" {
                assert_eq!(&ui.fixes[..ui.fixes.len() - 1], plain.fixes.as_slice());
                assert!(ui.fixes.last().unwrap().edits.is_empty());
            } else {
                assert_eq!(plain.fixes, ui.fixes);
            }
        }
        let regex: Vec<_> = replacements
            .iter()
            .filter(|p| p.diagnostic.code == "PyRegex")
            .collect();
        assert_eq!(regex.len(), 2);
        for problem in regex {
            assert_eq!(
                &SOURCE[usize::from(problem.diagnostic.range.start())
                    ..usize::from(problem.diagnostic.range.end())],
                "["
            );
        }
        let mut sorted = replacements.clone();
        sorted.sort_by_key(|p| p.diagnostic.range.start());
        assert_ne!(sorted, replacements); // LSP's client sort is a real presentation policy.
    }

    #[test]
    fn stub_and_disabled_supplement_policies_are_explicit() {
        let stub = run(
            "/analysis-service-fixture/m.pyi",
            SOURCE,
            StubSupplementPolicy::InspectionOnly,
            DictionaryActionPolicy::ReplacementsOnly,
            true,
            true,
        );
        assert!(stub.iter().all(|p| !matches!(
            p.diagnostic.code.as_str(),
            "Grammar" | "SpellCheckingInspection" | "PyDocstring" | "PyRegex"
        )));
        let included = run(
            "/analysis-service-fixture/m.pyi",
            SOURCE,
            StubSupplementPolicy::Include,
            DictionaryActionPolicy::ReplacementsOnly,
            true,
            true,
        );
        assert!(included.iter().any(|p| p.diagnostic.code == "PyRegex"));
        let disabled = run(
            "/analysis-service-fixture/m.py",
            SOURCE,
            StubSupplementPolicy::Include,
            DictionaryActionPolicy::ReplacementsOnly,
            false,
            false,
        );
        assert!(disabled.iter().all(|p| !matches!(
            p.diagnostic.code.as_str(),
            "Grammar" | "SpellCheckingInspection"
        )));
        assert!(disabled.iter().any(|p| p.diagnostic.code == "PyRegex"));
    }

    #[test]
    fn cancellation_at_every_stage_never_returns_partial_success() {
        let db = Analysis::for_project(Path::new("/analysis-service-fixture"));
        let id = db.set_file_text(Path::new("/analysis-service-fixture/m.py"), SOURCE);
        let file = db.file(id).unwrap();
        let inspections = InspectionOptions::default();
        let dictionary = rustpy_spell::Dictionary::bundled();
        let options = FileAnalysisOptions {
            inspections: &inspections,
            grammar: true,
            dictionary: Some(&dictionary),
            stub_supplements: StubSupplementPolicy::Include,
            dictionary_actions: DictionaryActionPolicy::IncludeSaveWord,
        };
        for target in [
            Stage::Inspections,
            Stage::Grammar,
            Stage::Spelling,
            Stage::Docstrings,
            Stage::Regex,
            Stage::Complete,
        ] {
            let cancel = CancellationToken::new();
            let mut reached = false;
            let result = analyze_with_checkpoint(&db, &file, &options, &cancel, |stage| {
                if stage == target {
                    reached = true;
                    cancel.cancel();
                }
            });
            assert!(reached);
            assert_eq!(result, Err(Cancelled));
        }
    }

    #[test]
    fn legacy_docstring_supplement_survives_disabled_inspection() {
        let db = Analysis::for_project(Path::new("/analysis-service-fixture"));
        let id = db.set_file_text(
            Path::new("/analysis-service-fixture/m.py"),
            "def f(x):\n    \"\"\":param ghost: value\"\"\"\n    return x\n",
        );
        let file = db.file(id).unwrap();
        let mut inspections = InspectionOptions::default();
        let run = |inspections: &InspectionOptions| {
            analyze_python(
                &db,
                &file,
                &FileAnalysisOptions {
                    inspections,
                    grammar: false,
                    dictionary: None,
                    stub_supplements: StubSupplementPolicy::Include,
                    dictionary_actions: DictionaryActionPolicy::ReplacementsOnly,
                },
                &CancellationToken::new(),
            )
            .unwrap()
        };
        let enabled = run(&inspections);
        assert!(enabled
            .iter()
            .any(|p| p.diagnostic.code == "PyIncorrectDocstring"));
        assert!(enabled.iter().any(|p| p.diagnostic.code == "PyDocstring"));
        inspections.incorrect_docstrings = false;
        let disabled = run(&inspections);
        assert!(!disabled
            .iter()
            .any(|p| p.diagnostic.code == "PyIncorrectDocstring"));
        assert!(disabled.iter().any(|p| p.diagnostic.code == "PyDocstring"));
    }

    #[test]
    fn spelling_converter_caps_replacements_and_preserves_dictionary_command() {
        let typo = rustpy_spell::Typo {
            range: TextRange::new(2.into(), 9.into()),
            word: "Recieve".into(),
            suggestions: (0..8).map(|n| format!("word{n}")).collect(),
            kind: rustpy_spell::TypoKind::Comment,
        };
        let plain = spelling_problems(
            std::slice::from_ref(&typo),
            DictionaryActionPolicy::ReplacementsOnly,
        );
        let ui = spelling_problems(&[typo], DictionaryActionPolicy::IncludeSaveWord);
        assert_eq!(plain[0].fixes.len(), 5);
        assert_eq!(ui[0].fixes.len(), 6);
        assert_eq!(ui[0].fixes[5].title, "Save 'recieve' to dictionary");
        assert!(ui[0].fixes[5].edits.is_empty());
        assert_eq!(plain[0].fixes, ui[0].fixes[..5]);
    }

    #[test]
    fn converters_preserve_equal_position_order_and_exact_fix_edits() {
        let issues = vec![
            rustpy_grammar::Issue {
                range: 2..4,
                rule: "first",
                message: "one".into(),
                replacements: vec!["x".into(), "y".into()],
            },
            rustpy_grammar::Issue {
                range: 2..4,
                rule: "second",
                message: "two".into(),
                replacements: vec![],
            },
        ];
        let mut problems = grammar_problems(&issues);
        problems.sort_by_key(|p| p.diagnostic.range.start());
        assert_eq!(problems[0].diagnostic.message, "Grammar: one");
        assert_eq!(problems[1].diagnostic.message, "Grammar: two");
        assert_eq!(problems[0].fixes[0].title, "Replace with 'x'");
        assert_eq!(
            problems[0].fixes[0].edits,
            vec![(TextRange::new(2.into(), 4.into()), "x".into())]
        );
        assert!(problems[1].fixes.is_empty());
    }
    #[test]
    fn injected_forward_and_open_references_have_precise_source_diagnostics() {
        let source = "import re\nre.compile(r'π🙂\\1(a)')\nre.compile(r'(?P<x>(?P=x))')\nre.compile(r'(a)\\1')\n";
        let problems = run(
            "/analysis-service-fixture/refs.py",
            source,
            StubSupplementPolicy::Include,
            DictionaryActionPolicy::ReplacementsOnly,
            false,
            false,
        );
        let regex: Vec<_> = problems
            .iter()
            .filter(|p| p.diagnostic.code == "PyRegex")
            .collect();
        assert_eq!(regex.len(), 2);
        let slices: Vec<_> = regex
            .iter()
            .map(|p| {
                &source
                    [usize::from(p.diagnostic.range.start())..usize::from(p.diagnostic.range.end())]
            })
            .collect();
        assert_eq!(slices, vec![r"\1", "x"]);
        assert!(regex[0]
            .diagnostic
            .message
            .contains("invalid group reference"));
        assert!(regex[1].diagnostic.message.contains("open group"));
    }
}
