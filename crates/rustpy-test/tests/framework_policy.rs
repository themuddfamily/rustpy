//! Configuration, selectors and discovery fixtures: no interpreter is executed.
use rustpy_test::{
    configured_framework, discover_file_in, is_test_file_in, tox_environment_plans, Framework,
    PytestConfiguration,
};
#[test]
fn pytest_toml_evidence_and_custom_naming_feed_static_context_discovery() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("pyproject.toml"),
        r#"# [tool.pytest] in a comment is not evidence
[project]
name="demo"
"#,
    )
    .unwrap();
    assert_eq!(configured_framework(root.path(), "auto"), None);
    std::fs::write(
        root.path().join("pyproject.toml"),
        r#"[tool.pytest.ini_options]
python_files=["spec_*.py"]
python_classes=["*Spec"]
python_functions=["check_*"]
markers=["slow: slow checks"]
addopts="-ra --strict-markers"
"#,
    )
    .unwrap();
    assert_eq!(
        configured_framework(root.path(), "auto"),
        Some(Framework::Pytest)
    );
    assert_eq!(
        configured_framework(root.path(), "nose2"),
        Some(Framework::Nose2)
    );
    let path = root.path().join("spec_subject.py");
    assert!(is_test_file_in(root.path(), &path));
    assert!(!is_test_file_in(
        root.path(),
        &root.path().join("test_old.py")
    ));
    let tests = discover_file_in(
        root.path(),
        &path,
        "def check_one(): pass\nclass BehaviorSpec:\n    def check_two(self): pass\n",
    );
    assert_eq!(tests.len(), 2);
    assert_eq!(tests[1].children[0].name, "check_two");
    assert!(PytestConfiguration::load(root.path())
        .values
        .contains_key("markers"));
}
#[test]
fn toml_document_detection_rejects_strings_and_parse_errors_and_reads_tox_tables() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("pyproject.toml");
    for misleading in [
        "[project]\nname='demo'\ndescription='[tool.pytest.ini_options]'\n",
        "[tool.pytest.ini_options]\naddopts = [\n",
    ] {
        std::fs::write(&path, misleading).unwrap();
        assert_eq!(configured_framework(root.path(), "auto"), None);
        assert!(PytestConfiguration::load(root.path()).path.is_none());
    }
    std::fs::write(&path, "[tool.tox]\nenv_list=['py311', 'lint']\n").unwrap();
    let plans = tox_environment_plans(root.path(), &[], &[]);
    assert_eq!(
        plans
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["py311", "lint"]
    );
    std::fs::remove_file(path).unwrap();
    std::fs::write(
        root.path().join("tox.toml"),
        "env_list=['unit']\n[env.unit]\ndescription='unit checks'\n",
    )
    .unwrap();
    assert_eq!(tox_environment_plans(root.path(), &[], &[])[0].0, "unit");
}
#[test]
fn tox_brace_factors_produce_distinct_tab_plans_and_explicit_selection_wins() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("tox.ini"),
        "[tox]\nenvlist=py{310,311}-{unit,integration}, lint\n",
    )
    .unwrap();
    let plans = tox_environment_plans(root.path(), &[], &["--recreate".into()]);
    assert_eq!(
        plans
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        vec![
            "py310-unit",
            "py310-integration",
            "py311-unit",
            "py311-integration",
            "lint"
        ]
    );
    assert_eq!(plans[0].1, vec!["-e", "py310-unit", "--recreate"]);
    assert_eq!(
        tox_environment_plans(root.path(), &[], &["-e".into(), "lint".into()]).len(),
        1
    );
}
#[test]
fn all_nine_framework_choices_have_stable_roundtrip_keys() {
    for framework in Framework::ALL {
        assert_eq!(Framework::from_key(framework.label()), Some(framework));
    }
}
#[test]
fn pytest_naming_globs_support_classes_ranges_and_literal_brackets() {
    use rustpy_test::integration::glob_matches;
    assert!(glob_matches("spec_[a-c]?.py", "spec_b1.py"));
    assert!(!glob_matches("spec_[!a-c]*.py", "spec_b1.py"));
    assert!(glob_matches("spec_[[]*.py", "spec_[value.py"));
    assert!(glob_matches("*", "*literal"));
}
