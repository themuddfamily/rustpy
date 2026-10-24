//! File-level language selection reaches the shared UI/CLI/LSP diagnostic pipeline.
use rustpy_analysis::{
    file_analysis::{self, DictionaryActionPolicy, FileAnalysisOptions, StubSupplementPolicy},
    inspections::InspectionOptions,
};
use rustpy_semantic::Analysis;
use std::{path::Path, sync::Arc};

#[test]
fn captured_language_controls_prose_checks_and_doc_policy_controls_supplements() {
    let db = Analysis::for_project(Path::new("/editorconfig-analysis"));
    let path = Path::new("/editorconfig-analysis/module.py");
    let id = db.set_file_text(path, "# the the answer happens alot\ndef f(value):\n    \"\"\"Summary.\n\n    :param missing: wrong\n    \"\"\"\n    return value\n");
    let file = db.file(id).unwrap();
    let dictionary = rustpy_spell::Dictionary::bundled();
    let analyze = |language: &str| {
        let options = InspectionOptions {
            editorconfig: Some(Arc::new(rustpy_settings::editorconfig::EditorConfigProps {
                spelling_language: Some(language.into()),
                ..Default::default()
            })),
            ..Default::default()
        };
        file_analysis::analyze_python(
            &db,
            &file,
            &FileAnalysisOptions {
                inspections: &options,
                grammar: true,
                dictionary: Some(&dictionary),
                stub_supplements: StubSupplementPolicy::Include,
                dictionary_actions: DictionaryActionPolicy::ReplacementsOnly,
            },
            &rustpy_core::CancellationToken::new(),
        )
        .unwrap()
    };
    let english = analyze("en-GB");
    assert!(english
        .iter()
        .any(|problem| problem.diagnostic.code == "Grammar"));
    assert!(english
        .iter()
        .any(|problem| problem.diagnostic.code == "PyDocstring"));
    db.set_analyze_doc_comments(false);
    let german = analyze("de-DE");
    assert!(!german.iter().any(|problem| matches!(
        problem.diagnostic.code.as_str(),
        "Grammar" | "Spelling" | "PyDocstring" | "PyIncorrectDocstring" | "PyDocstringType"
    )));
}
