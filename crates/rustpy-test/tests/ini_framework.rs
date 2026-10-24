//! Actual public framework selection evidence, without interpreter probing.
use rustpy_test::project_uses_pytest;

#[test]
fn setup_cfg_comments_and_literal_values_do_not_select_pytest_but_real_section_does() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("setup.cfg");
    let misleading =
        "# [tool:pytest]\n[metadata]\nname = package-π\ndescription = text [tool:pytest] literal\n";
    std::fs::write(&path, misleading).unwrap();
    assert!(!project_uses_pytest(root.path()));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), misleading);
    let valid = "[metadata]\nname = package-π\n[tool:pytest]\naddopts = -q\n";
    std::fs::write(&path, valid).unwrap();
    assert!(project_uses_pytest(root.path()));
    assert_eq!(std::fs::read_to_string(path).unwrap(), valid);
}

#[test]
fn tox_value_continuation_is_not_a_section_even_when_it_looks_like_one() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("tox.ini");
    std::fs::write(&path, "[tox]\ncommands = first\n    [pytest]\n    second\n").unwrap();
    assert!(!project_uses_pytest(root.path()));
    std::fs::write(&path, "[tox]\ncommands = first\n[pytest]\naddopts = -q\n").unwrap();
    assert!(project_uses_pytest(root.path()));
}

#[test]
fn only_exact_valid_setup_section_is_positive_framework_evidence() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("setup.cfg");
    for invalid in [
        "[tool:pytest_extra]\naddopts=-q\n",
        "[tool:pytest\naddopts=-q\n",
    ] {
        std::fs::write(&path, invalid).unwrap();
        assert!(!project_uses_pytest(root.path()), "{invalid:?}");
    }
    for declared in [
        "[tool:pytest] trailing junk\naddopts=-q\n",
        "\u{feff}[tool:pytest] # actual section\r\naddopts = -q\r\n",
        "[tool:pytest]\noption without delimiter\n",
        "[tool:pytest]\naddopts=-q\naddopts=-v\n",
    ] {
        std::fs::write(&path, declared).unwrap();
        assert!(
            project_uses_pytest(root.path()),
            "genuine section remains evidence despite option errors: {declared:?}"
        );
    }
}

#[test]
fn existing_pytest_ini_conftest_and_pyproject_evidence_stays_unchanged() {
    let root = tempfile::tempdir().unwrap();
    assert!(!project_uses_pytest(root.path()));
    for name in ["pytest.ini", "conftest.py"] {
        let path = root.path().join(name);
        std::fs::write(&path, "").unwrap();
        assert!(
            project_uses_pytest(root.path()),
            "existing file-presence policy: {name}"
        );
        std::fs::remove_file(path).unwrap();
    }
    let project = root.path().join("pyproject.toml");
    std::fs::write(&project, "[tool.pytest.ini_options]\naddopts='-q'\n").unwrap();
    assert!(project_uses_pytest(root.path()));
}
