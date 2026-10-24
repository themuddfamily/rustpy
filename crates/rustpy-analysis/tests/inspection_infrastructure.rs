use rustpy_analysis::inspections::{
    self,
    infrastructure::{self, Finding, GroupBy, Policy},
    InspectionOptions, Problem, QuickFix,
};
use rustpy_core::{Diagnostic, Severity, TextRange, TextSize};
use rustpy_semantic::Analysis;
use rustpy_settings::inspection_profiles::{
    CustomSeverity, HighlightingLevel, Profile, Profiles, Rule,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
fn run(source: &str, path: &str, policy: Policy) -> Vec<Problem> {
    let db = Analysis::for_project(Path::new("/profiles"));
    let id = db.set_file_text(Path::new(path), source);
    let file = db.file(id).unwrap();
    inspections::inspect(
        &db,
        &file,
        &InspectionOptions {
            policy: Some(policy),
            ..Default::default()
        },
    )
}
fn settings() -> Profiles {
    let mut settings = Profiles::default();
    settings.ide[0].inspections.insert(
        "PyUnusedImports".into(),
        Rule {
            severity: Some("Build failure".into()),
            include: vec!["src/**/*.py".into()],
            exclude: vec!["src/generated/**".into()],
            ..Rule::default()
        },
    );
    settings.custom_severities.push(CustomSeverity {
        name: "Build failure".into(),
        base: Severity::Error,
    });
    settings
}
#[test]
fn inherited_profiles_scopes_custom_severity_and_xml_drive_real_inspections() {
    let mut settings = settings();
    settings.project.insert(
        "/profiles".into(),
        vec![Profile {
            name: "Team".into(),
            parent: Some("Default".into()),
            inspections: BTreeMap::new(),
        }],
    );
    settings
        .project_active
        .insert("/profiles".into(), "Team".into());
    let policy = Policy::from_settings(&settings, Some(Path::new("/profiles")), false).unwrap();
    let problems = run("import os\n", "/profiles/src/main.py", policy.clone());
    let unused = problems
        .iter()
        .find(|problem| problem.diagnostic.code == "PyUnusedImports")
        .unwrap();
    assert_eq!(unused.diagnostic.severity, Severity::Error);
    assert_eq!(
        policy.severity_name("PyUnusedImports").as_deref(),
        Some("Build failure")
    );
    assert!(!run(
        "import os\n",
        "/profiles/src/generated/main.py",
        policy.clone()
    )
    .iter()
    .any(|problem| problem.diagnostic.code == "PyUnusedImports"));
    assert!(!run(
        "# noinspection PyUnusedImports\nimport os\n",
        "/profiles/src/main.py",
        policy.clone()
    )
    .iter()
    .any(|problem| problem.diagnostic.code == "PyUnusedImports"));
    let xml = settings.export_profile(&settings.resolved(Some(Path::new("/profiles"))).unwrap());
    let mut imported = Profiles::default();
    let profile = imported.import_profile(&xml).unwrap();
    imported.active = profile.name.clone();
    imported.ide.push(profile);
    assert_eq!(imported.severity("Build failure"), Some(Severity::Error));
    let policy = Policy::from_settings(&imported, Some(Path::new("/profiles")), false).unwrap();
    assert!(run("import os\n", "/profiles/src/main.py", policy)
        .iter()
        .any(|problem| problem.diagnostic.code == "PyUnusedImports"
            && problem.diagnostic.severity == Severity::Error));
}
#[test]
fn highlighting_levels_only_limit_on_the_fly_and_batch_remains_explicit() {
    let mut settings = Profiles::default();
    let path = "/profiles/main.py";
    settings
        .file_levels
        .insert(path.into(), HighlightingLevel::None);
    let policy = Policy::from_settings(&settings, Some(Path::new("/profiles")), false).unwrap();
    assert!(run("import os\nmissing\n", path, policy).is_empty());
    settings
        .file_levels
        .insert(path.into(), HighlightingLevel::Syntax);
    let policy = Policy::from_settings(&settings, Some(Path::new("/profiles")), false).unwrap();
    let problems = run("def\n", path, policy);
    assert!(!problems.is_empty());
    assert!(problems
        .iter()
        .all(|problem| problem.diagnostic.code == "syntax"));
    let policy = Policy::from_settings(&settings, Some(Path::new("/profiles")), true).unwrap();
    assert!(run("missing_name\n", path, policy)
        .iter()
        .any(|problem| problem.diagnostic.code == "PyUnresolvedReferences"));
}
#[test]
fn compiler_context_errors_survive_suppression_and_syntax_only_profiles() {
    let mut settings = Profiles::default();
    let path = "/profiles/main.py";
    settings
        .file_levels
        .insert(path.into(), HighlightingLevel::Syntax);
    settings.ide[0].inspections.insert(
        "PySyntaxError".into(),
        Rule {
            enabled: false,
            ..Rule::default()
        },
    );
    let policy = Policy::from_settings(&settings, Some(Path::new("/profiles")), false).unwrap();
    let problems = run(
        "# noinspection file: PySyntaxError\n__debug__ = False\n",
        path,
        policy,
    );
    let compiler_errors: Vec<_> = problems
        .iter()
        .filter(|problem| problem.diagnostic.code == "PySyntaxError")
        .collect();
    assert!(!compiler_errors.is_empty());
    assert!(compiler_errors
        .iter()
        .all(|problem| problem.diagnostic.severity == Severity::Error));
    assert!(compiler_errors
        .iter()
        .all(|problem| problem.fixes.iter().all(|fix| !fix.is_suppression())));
}
#[test]
fn registry_cleanup_and_selected_fix_batching_never_apply_suppressions_or_overlaps() {
    let range = |start, end| TextRange::new(TextSize::new(start), TextSize::new(end));
    let problem = |range, title: &str, replacement: &str| Problem {
        diagnostic: Diagnostic::new(
            range,
            Severity::Warning,
            "PyRedundantParentheses",
            "Redundant parentheses",
        ),
        fixes: vec![QuickFix {
            title: title.into(),
            edits: vec![(range, replacement.into())],
        }],
    };
    let problems = vec![
        problem(range(0, 3), "Remove parentheses", "a"),
        problem(range(1, 2), "Remove parentheses", "b"),
        problem(range(5, 8), "Suppress inspection", "# marker"),
    ];
    assert_eq!(
        infrastructure::batch_edits(&problems, None, None, true),
        vec![(range(0, 3), "a".into())]
    );
    assert!(infrastructure::batch_edits(
        &problems,
        Some("PyTypeChecker"),
        Some("Remove parentheses"),
        false
    )
    .is_empty());
    let ids: std::collections::BTreeSet<_> = infrastructure::REGISTRY
        .iter()
        .map(|entry| entry.id)
        .collect();
    assert_eq!(ids.len(), infrastructure::REGISTRY.len());
    assert!(
        infrastructure::inspection("PyUnusedImports")
            .unwrap()
            .cleanup
    );
    assert!(!infrastructure::inspection("PyTypeChecker").unwrap().cleanup);
    assert!(infrastructure::inspection("PEP8:E501")
        .unwrap()
        .options
        .contains(&"max_line_length"));
}
#[test]
fn all_report_formats_escape_source_and_preserve_actual_locations_and_groups() {
    let findings = vec![Finding {
        path: PathBuf::from("/project/a π.py"),
        line: 2,
        column: 4,
        diagnostic: Diagnostic::new(
            TextRange::empty(TextSize::new(0)),
            Severity::Error,
            "PyTypeChecker",
            "<script>&\"",
        ),
        severity_name: Some("Build failure".into()),
    }];
    assert_eq!(findings[0].group(GroupBy::Inspection), "PyTypeChecker");
    assert_eq!(findings[0].group(GroupBy::Severity), "Build failure");
    let html = infrastructure::export(&findings, "HTML", "<Team>").unwrap();
    assert!(!html.contains("<script>"));
    assert!(html.contains("&lt;script&gt;"));
    let xml = infrastructure::export(&findings, "XML", "Team").unwrap();
    assert!(xml.contains("line=\"3\" column=\"5\""));
    assert!(xml.contains("Build failure"));
    let sarif: serde_json::Value =
        serde_json::from_str(&infrastructure::export(&findings, "SARIF", "Team").unwrap()).unwrap();
    let result = &sarif["runs"][0]["results"][0];
    assert_eq!(result["level"], "error");
    assert_eq!(
        result["locations"][0]["physicalLocation"]["region"]["startLine"],
        3
    );
    assert!(
        result["locations"][0]["physicalLocation"]["artifactLocation"]["uri"]
            .as_str()
            .unwrap()
            .contains("%20")
    );
}

#[test]
fn registered_line_length_option_changes_the_real_style_pass() {
    let source = "word = 'abcdefghijklmnopqrstuvwxyz'\n";
    let mut profiles = Profiles::default();
    let policy = Policy::from_settings(&profiles, Some(Path::new("/profiles")), false).unwrap();
    assert!(!run(source, "/profiles/main.py", policy)
        .iter()
        .any(|problem| problem.diagnostic.code == "PEP8:E501"));
    let mut rule = Rule::default();
    rule.options.insert("max_line_length".into(), "20".into());
    profiles.ide[0].inspections.insert("PyPep8".into(), rule);
    let policy = Policy::from_settings(&profiles, Some(Path::new("/profiles")), false).unwrap();
    assert!(run(source, "/profiles/main.py", policy)
        .iter()
        .any(|problem| problem.diagnostic.code == "PEP8:E501"));
    assert!(infrastructure::matches_inspection("PyPep8", "PEP8:E501"));
    assert!(!infrastructure::matches_inspection(
        "PEP8:E225",
        "PEP8:E501"
    ));
}

#[test]
fn shared_composition_suppresses_supplements_and_preserves_syntax_errors() {
    use rustpy_analysis::file_analysis::{
        self, DictionaryActionPolicy, FileAnalysisOptions, StubSupplementPolicy,
    };
    let source = "import re\nre.compile(r'[')\ndef f(a, b):\n    \"\"\"Do.\n\n    :param a: first\n    :param zzz: ghost\n    :return: value\n    \"\"\"\n    print(a, b)\n";
    let db = Analysis::for_project(Path::new("/supplement-policy"));
    let path = Path::new("/supplement-policy/main.py");
    let options = InspectionOptions::default();
    let compose = |text: &str| {
        let file = db.file(db.set_file_text(path, text)).unwrap();
        file_analysis::analyze_python(
            &db,
            &file,
            &FileAnalysisOptions {
                inspections: &options,
                grammar: false,
                dictionary: None,
                stub_supplements: StubSupplementPolicy::Include,
                dictionary_actions: DictionaryActionPolicy::ReplacementsOnly,
            },
            &rustpy_core::CancellationToken::new(),
        )
        .unwrap()
    };
    let original = compose(source);
    for code in ["PyRegex", "PyDocstring"] {
        assert!(
            original
                .iter()
                .any(|problem| problem.diagnostic.code == code),
            "{code} producer must execute"
        );
    }
    let suppressed = compose(&format!(
        "# noinspection file: PyRegex,PyDocstring\n{source}"
    ));
    assert!(!suppressed
        .iter()
        .any(|problem| matches!(problem.diagnostic.code.as_str(), "PyRegex" | "PyDocstring")));
    let malformed = compose("# noinspection file: ALL\ndef broken(:\n");
    assert!(malformed
        .iter()
        .any(|problem| problem.diagnostic.code == "syntax"));
}
