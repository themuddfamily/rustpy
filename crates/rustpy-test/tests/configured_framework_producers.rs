//! Explicit installed producers through TestRun; no supplied protocol records.
use rustpy_test::{Framework, Outcome, Summary, TestEvent, TestResult, TestRun, TestRunOptions};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn python() -> PathBuf {
    assert_eq!(
        std::env::var("RUSTPY_REQUIRE_PROVIDER_TESTS").as_deref(),
        Ok("1")
    );
    let path = PathBuf::from(
        std::env::var_os("RUSTPY_PROVIDER_PYTHON")
            .expect("explicit installed provider SDK required"),
    );
    assert!(
        path.is_absolute() && path.is_file(),
        "provider Python must be an absolute installed executable"
    );
    path
}
fn execute(
    root: &Path,
    framework: Framework,
    targets: &[String],
    arguments: &[String],
) -> Vec<TestEvent> {
    let options = TestRunOptions {
        env: vec![("PYTEST_DISABLE_PLUGIN_AUTOLOAD".into(), "1".into())],
        ..Default::default()
    };
    let mut run =
        TestRun::start_with(91, framework, &python(), root, targets, arguments, &options).unwrap();
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut events = Vec::new();
    while run.is_running() {
        events.extend(run.poll());
        if Instant::now() >= deadline {
            run.stop();
            panic!("actual provider did not finish: {events:#?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    events.extend(run.poll());
    events
}
fn results(events: &[TestEvent]) -> Vec<&TestResult> {
    events
        .iter()
        .filter_map(|event| {
            if let TestEvent::Result(result) = event {
                Some(result)
            } else {
                None
            }
        })
        .collect()
}
fn done(events: &[TestEvent]) -> &Summary {
    events
        .iter()
        .find_map(|event| {
            if let TestEvent::Done(summary) = event {
                Some(summary)
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("actual provider must report completion: {events:#?}"))
}
fn started_before_result(events: &[TestEvent], id: &str) {
    let start = events
        .iter()
        .position(|event| matches!(event, TestEvent::Started(found) if found == id))
        .expect("actual test start");
    let result = events
        .iter()
        .position(|event| matches!(event, TestEvent::Result(result) if result.id == id))
        .expect("actual test result");
    assert!(start < result);
}

#[test]
#[ignore = "requires explicitly admitted real pytest/Hypothesis SDK"]
fn configured_hypothesis_real_success_failure_and_falsifying_example_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    std::fs::write(
        root.join("test_properties.py"),
        r#"from hypothesis import given, settings, strategies as st
@settings(max_examples=5, derandomize=True, database=None)
@given(value=st.integers(min_value=-10, max_value=10))
def test_actual_success(value):
    assert value + 1 > value
    from rustpy_pytest_plugin import _hypothesis_failure_text
    item = type("Item", (), {"obj": staticmethod(test_actual_success)})()
    ordinary = type("Item", (), {"obj": staticmethod(lambda: None)})()
    for heading in ("Falsifying example: test_actual_success(value=1)",
                    "Failing test case: test_actual_success(value=1)"):
        report = type("Report", (), {"longreprtext": heading})()
        assert _hypothesis_failure_text(item, report) == heading
        assert _hypothesis_failure_text(ordinary, report) == ""
    report = type("Report", (), {"longreprtext": "ordinary AssertionError: value=1"})()
    assert _hypothesis_failure_text(item, report) == ""
@settings(max_examples=5, derandomize=True, database=None)
@given(value=st.integers(min_value=1, max_value=100))
def test_actual_failure(value):
    assert value == 0
"#,
    )
    .unwrap();
    let events = execute(
        &root,
        Framework::Pytest,
        &["test_properties.py".into()],
        &["-p".into(), "_hypothesis_pytestplugin".into()],
    );
    let summary = done(&events);
    assert_eq!(
        (
            summary.passed,
            summary.failed,
            summary.errors,
            summary.exit_code
        ),
        (1, 1, 0, Some(1)),
        "{events:#?}"
    );
    let id = "test_properties.py::test_actual_failure";
    let records = results(&events);
    let failed = records.iter().find(|result| result.id == id).unwrap();
    assert_eq!(failed.outcome, Outcome::Failed);
    let has_example_heading =
        |text: &str| text.contains("Falsifying example") || text.contains("Failing test case:");
    assert!(
        has_example_heading(&failed.traceback) && failed.traceback.contains("value=1"),
        "{failed:#?}"
    );
    assert!(events.iter().any(|event| matches!(event, TestEvent::Metadata(metadata) if metadata.id == id && has_example_heading(&metadata.hypothesis) && metadata.hypothesis.contains("value=1"))), "actual minimized Hypothesis example must reach the common protocol: {events:#?}");
    started_before_result(&events, id);
    started_before_result(&events, "test_properties.py::test_actual_success");
}

#[test]
#[ignore = "requires explicit admitted Python; actual stdlib unittest subTest producer"]
fn configured_unittest_subtests_report_actual_children_and_failed_parent() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    std::fs::write(
        root.join("test_subcases.py"),
        r#"import unittest
class Cases(unittest.TestCase):
    def test_good(self):
        print('ACTUAL_UNITTEST_GOOD')
        self.assertEqual(1, 1)
    def test_mixed(self):
        for value in (0, 1):
            with self.subTest(value=value):
                self.assertEqual(value, 0)
"#,
    )
    .unwrap();
    let events = execute(
        &root,
        Framework::Unittest,
        &["test_subcases.py".into()],
        &[],
    );
    let records = results(&events);
    let parent = "test_subcases.py::Cases::test_mixed";
    for (value, outcome) in [(0, Outcome::Passed), (1, Outcome::Failed)] {
        let child = format!("{parent}::subTest(value={value})");
        let result = records
            .iter()
            .find(|result| result.id == child)
            .unwrap_or_else(|| panic!("actual subtest child missing: {events:#?}"));
        assert_eq!(result.outcome, outcome);
        assert!(events
            .iter()
            .any(|event| matches!(event, TestEvent::Collected { id, .. } if id == &child)));
        started_before_result(&events, &child);
    }
    assert_eq!(
        records
            .iter()
            .find(|result| result.id == parent)
            .unwrap()
            .outcome,
        Outcome::Failed
    );
    assert!(records
        .iter()
        .any(|result| result.id.ends_with("::test_good")
            && result.outcome == Outcome::Passed
            && result.stdout.contains("ACTUAL_UNITTEST_GOOD")));
    let summary = done(&events);
    assert_eq!(
        (summary.passed, summary.failed, summary.exit_code),
        (1, 1, Some(1)),
        "child records must not double-count parent summary: {events:#?}"
    );
}

fn actual_legacy_unittest(framework: Framework) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    std::fs::write(
        root.join("test_legacy.py"),
        r#"import unittest
class ActualLegacy(unittest.TestCase):
    def test_good(self):
        print('ACTUAL_LEGACY_SUCCESS')
        self.assertEqual(1, 1)
    def test_bad(self):
        self.assertEqual('ACTUAL_EXPECTED', 'ACTUAL_DIFFERENT')
"#,
    )
    .unwrap();
    let events = execute(&root, framework, &["test_legacy".into()], &[]);
    let summary = done(&events);
    assert_eq!(
        (
            summary.passed,
            summary.failed,
            summary.errors,
            summary.exit_code
        ),
        (1, 1, 0, Some(1)),
        "native installed legacy framework must really execute, never substitute unittest: {events:#?}"
    );
    let records = results(&events);
    let passed = records
        .iter()
        .find(|result| result.id.ends_with("::test_good"))
        .expect("native provider success record");
    assert_eq!(passed.outcome, Outcome::Passed);
    assert!(passed.stdout.contains("ACTUAL_LEGACY_SUCCESS"));
    let failed = records
        .iter()
        .find(|result| result.id.ends_with("::test_bad"))
        .expect("native provider failure record");
    assert_eq!(failed.outcome, Outcome::Failed);
    assert!(
        failed.traceback.contains("ACTUAL_EXPECTED")
            && failed.traceback.contains("ACTUAL_DIFFERENT")
    );
    for result in records {
        started_before_result(&events, &result.id);
    }
}

#[test]
#[ignore = "requires explicitly admitted nose2 SDK; actual native entrypoint"]
fn configured_nose2_native_entrypoint_reports_real_success_and_failure() {
    actual_legacy_unittest(Framework::Nose2);
}

#[test]
#[ignore = "requires explicitly admitted nose-compatible Python; old nose may not support modern Python"]
fn configured_nose_native_entrypoint_reports_real_success_and_failure() {
    actual_legacy_unittest(Framework::Nose);
}

#[test]
#[ignore = "requires explicitly admitted Lettuce-compatible Python; legacy distribution may not support modern Python"]
fn configured_lettuce_native_entrypoint_reports_real_scenarios() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("features/steps")).unwrap();
    std::fs::write(root.join("features/actual.feature"), "Feature: Actual Lettuce\n  Scenario: passes\n    Given the value is 1\n    Then the value equals 1\n  Scenario: fails\n    Given the value is 1\n    Then the value equals 2\n").unwrap();
    std::fs::write(
        root.join("features/steps/steps.py"),
        r#"from lettuce import step, world
@step(r'the value is (\d+)')
def actual_value(step, value):
    world.value = int(value)
@step(r'the value equals (\d+)')
def actual_check(step, value):
    assert world.value == int(value), 'ACTUAL_LETTUCE_FAILURE'
"#,
    )
    .unwrap();
    let events = execute(
        &root,
        Framework::Lettuce,
        &["features/actual.feature".into()],
        &[],
    );
    let summary = done(&events);
    assert_eq!(
        (
            summary.passed,
            summary.failed,
            summary.errors,
            summary.exit_code
        ),
        (1, 1, 0, Some(1)),
        "native installed Lettuce must execute both scenarios: {events:#?}"
    );
    let records = results(&events);
    assert!(records
        .iter()
        .any(|result| result.id.ends_with("::passes") && result.outcome == Outcome::Passed));
    assert!(records.iter().any(|result| result.id.ends_with("::fails")
        && result.outcome == Outcome::Failed
        && result.traceback.contains("ACTUAL_LETTUCE_FAILURE")));
    for result in records {
        started_before_result(&events, &result.id);
    }
}
