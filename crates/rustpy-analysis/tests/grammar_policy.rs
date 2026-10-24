use rustpy_analysis::{
    file_analysis::{self, DictionaryActionPolicy, FileAnalysisOptions, StubSupplementPolicy},
    inspections::InspectionOptions,
};
use rustpy_core::cancel::CancellationToken;
use rustpy_grammar::policy::Policy;
use rustpy_semantic::db::Analysis;
use std::path::Path;

#[test]
fn actual_analyzer_filters_policy_and_limits_strings_to_real_docstrings() {
    let db = Analysis::for_project(Path::new("/grammar-policy-fixture"));
    let source =
        "'Module the the value.'\nlabel = '''Assigned the the value.'''\n# It happens alot\n";
    let id = db.set_file_text(Path::new("/grammar-policy-fixture/a.py"), source);
    let file = db.file(id).unwrap();
    let inspections = InspectionOptions::default();
    let options = FileAnalysisOptions {
        inspections: &inspections,
        grammar: true,
        dictionary: None,
        stub_supplements: StubSupplementPolicy::Include,
        dictionary_actions: DictionaryActionPolicy::ReplacementsOnly,
    };
    let cancel = CancellationToken::new();
    let policy = Policy::from_preferences("en-GB", ["style"]).unwrap();
    let problems =
        file_analysis::analyze_python_with_grammar_policy(&db, &file, &options, &cancel, policy)
            .unwrap();
    let grammar: Vec<_> = problems
        .iter()
        .filter(|p| p.diagnostic.code == "Grammar")
        .collect();
    assert_eq!(grammar.len(), 1);
    let range = grammar[0].diagnostic.range;
    assert_eq!(
        &source[usize::from(range.start())..usize::from(range.end())],
        "the the"
    );
    assert_eq!(grammar[0].fixes[0].edits[0].1, "the");
    let policy = Policy::from_preferences("en-US", ["grammar"]).unwrap();
    let problems =
        file_analysis::analyze_python_with_grammar_policy(&db, &file, &options, &cancel, policy)
            .unwrap();
    let grammar: Vec<_> = problems
        .iter()
        .filter(|p| p.diagnostic.code == "Grammar")
        .collect();
    assert_eq!(grammar.len(), 1);
    assert_eq!(grammar[0].fixes[0].edits[0].1, "a lot");
    cancel.cancel();
    assert!(file_analysis::analyze_python_with_grammar_policy(
        &db, &file, &options, &cancel, policy
    )
    .is_err());
}
