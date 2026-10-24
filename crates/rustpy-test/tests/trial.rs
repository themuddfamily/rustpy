//! Twisted Trial output parsing against recorded reporter output.
//!
//! `fixtures/trial/{timing,bwverbose,verbose}.txt` are the stdout of
//! `python3 -m twisted.trial --reporter=<name> tests/test_sample.py
//! tests/test_broken.py` (Twisted 24.3, Python 3.12) run in
//! `fixtures/trial/project`.

use std::path::{Path, PathBuf};

use rustpy_test::*;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/trial")
}

fn project() -> PathBuf {
    fixtures().join("project")
}

fn output(name: &str) -> String {
    std::fs::read_to_string(fixtures().join(name)).unwrap()
}

fn results(events: &[TestEvent]) -> Vec<&TestResult> {
    events
        .iter()
        .filter_map(|e| {
            if let TestEvent::Result(r) = e {
                Some(r)
            } else {
                None
            }
        })
        .collect()
}

fn result<'a>(events: &'a [TestEvent], id: &str) -> &'a TestResult {
    let found: Vec<&TestResult> = results(events).into_iter().filter(|r| r.id == id).collect();
    assert_eq!(found.len(), 1, "exactly one result for {id}: {events:#?}");
    found[0]
}

fn done(events: &[TestEvent]) -> &Summary {
    match events.last() {
        Some(TestEvent::Done(s)) => s,
        other => panic!("last event must be Done: {other:?}"),
    }
}

const SAMPLE: &str = "tests/test_sample.py";

/// Outcomes common to all reporters.
fn check_outcomes(events: &[TestEvent]) {
    let root = project();
    let pass = result(events, &format!("{SAMPLE}::SampleTest::test_pass"));
    assert_eq!(pass.outcome, Outcome::Passed);
    assert_eq!(
        pass.stdout, "hello from pass",
        "test output printed between `...` and the status"
    );

    let fail = result(events, &format!("{SAMPLE}::SampleTest::test_fail"));
    assert_eq!(fail.outcome, Outcome::Failed);
    assert_eq!(fail.message, "FailTest: 1 != 2 : one is not two");
    assert!(
        fail.traceback
            .starts_with("Traceback (most recent call last):"),
        "{fail:?}"
    );
    assert_eq!(
        parse_failure_location(&fail.traceback, &root),
        Some((root.join(SAMPLE), 11))
    );

    let deferred = result(events, &format!("{SAMPLE}::SampleTest::test_deferred_fail"));
    assert_eq!(
        (deferred.outcome, deferred.message.as_str()),
        (Outcome::Failed, "FailTest: deferred failed")
    );
    assert_eq!(
        parse_failure_location(&deferred.traceback, &root),
        Some((root.join(SAMPLE), 31))
    );

    let error = result(events, &format!("{SAMPLE}::SampleTest::test_error"));
    assert_eq!(
        (error.outcome, error.message.as_str()),
        (Outcome::Error, "RuntimeError: boom")
    );

    let skip = result(events, &format!("{SAMPLE}::SampleTest::test_skip"));
    assert_eq!(
        (skip.outcome, skip.message.as_str()),
        (Outcome::Skipped, "not today")
    );
    let class_skip = result(events, &format!("{SAMPLE}::OtherTest::test_a"));
    assert_eq!(
        (class_skip.outcome, class_skip.message.as_str()),
        (Outcome::Skipped, "whole class skipped")
    );

    // `todo` = expected failure (xfail), an unexpected success = xpass.
    let todo = result(events, &format!("{SAMPLE}::SampleTest::test_todo"));
    assert_eq!((todo.outcome, todo.xfail), (Outcome::Skipped, true));
    assert_eq!(
        todo.message,
        "expected failure: FailTest: expected to fail (todo: known bug)"
    );
    assert!(todo.traceback.contains("line 20, in test_todo"), "{todo:?}");
    let xpass = result(events, &format!("{SAMPLE}::SampleTest::test_unexpected"));
    assert_eq!(
        (xpass.outcome, xpass.xfail, xpass.message.as_str()),
        (
            Outcome::Passed,
            true,
            "unexpected success (todo: should fail but passes)"
        )
    );

    // A module that cannot be imported is reported under its path.
    let broken = result(events, "tests/test_broken.py");
    assert_eq!(broken.outcome, Outcome::Error);
    assert_eq!(
        broken.message,
        "ModuleNotFoundError: No module named 'does_not_exist'"
    );
    assert_eq!(
        parse_failure_location(&broken.traceback, &root),
        Some((root.join("tests/test_broken.py"), 1))
    );

    assert_eq!(results(events).len(), 10);
    let s = done(events);
    assert_eq!((s.passed, s.failed, s.skipped, s.errors), (3, 2, 3, 2));
    assert_eq!(s.exit_code, Some(1), "trial's FAILED verdict");
    assert!(!s.is_success());

    // Every test is announced (with its location) and started before its result.
    for r in results(events) {
        let c = events
            .iter()
            .position(|e| matches!(e, TestEvent::Collected { id, .. } if *id == r.id))
            .unwrap_or_else(|| panic!("{} not collected", r.id));
        let st = events
            .iter()
            .position(|e| matches!(e, TestEvent::Started(id) if *id == r.id))
            .unwrap_or_else(|| panic!("{} not started", r.id));
        let res = events
            .iter()
            .position(|e| matches!(e, TestEvent::Result(x) if x.id == r.id))
            .unwrap();
        assert!(c < st && st < res, "{}", r.id);
    }
    let collected_fail = events.iter().find_map(|e| match e {
        TestEvent::Collected { id, path, line, .. } if id.ends_with("SampleTest::test_fail") => {
            Some((path.clone(), *line))
        }
        _ => None,
    });
    assert_eq!(collected_fail, Some((Some(root.join(SAMPLE)), Some(10))));
}

#[test]
fn timing_reporter() {
    let events = parse_trial_output(&output("timing.txt"), &project());
    check_outcomes(&events);
    let sleep = result(&events, &format!("{SAMPLE}::SlowTest::test_sleep"));
    assert_eq!((sleep.outcome, sleep.duration), (Outcome::Passed, 0.051));
    assert_eq!(
        result(
            &events,
            &format!("{SAMPLE}::SampleTest::test_deferred_fail")
        )
        .duration,
        0.007
    );
    assert!(
        (done(&events).duration - 0.064).abs() < 1e-9,
        "duration from `Ran 10 tests in 0.064s`"
    );
}

#[test]
fn bwverbose_reporter() {
    let events = parse_trial_output(&output("bwverbose.txt"), &project());
    check_outcomes(&events);
    assert!(results(&events).iter().all(|r| r.duration == 0.0));
    assert!((done(&events).duration - 0.063).abs() < 1e-9);
}

#[test]
fn default_tree_reporter() {
    let events = parse_trial_output(&output("verbose.txt"), &project());
    check_outcomes(&events);
    assert!((done(&events).duration - 0.073).abs() < 1e-9);
}

#[test]
fn text_reporter_without_progress_lines() {
    // `--reporter=text` prints `SFEF.ST!.E`: only the summary names tests.
    let events = parse_trial_output(&output("text.txt"), &project());
    assert_eq!(
        results(&events).len(),
        8,
        "every test that did not pass cleanly"
    );
    assert_eq!(
        result(&events, &format!("{SAMPLE}::SampleTest::test_fail")).message,
        "FailTest: 1 != 2 : one is not two"
    );
    assert_eq!(
        result(&events, &format!("{SAMPLE}::SampleTest::test_unexpected")).outcome,
        Outcome::Passed
    );
    assert!(!events.iter().any(|e| matches!(e, TestEvent::Started(_))));
    let s = done(&events);
    assert_eq!(
        (s.passed, s.failed, s.skipped, s.errors),
        (3, 2, 3, 2),
        "counts from the verdict line"
    );
}

#[test]
fn live_parser_streams_results() {
    let mut parser = TrialParser::new(TrialIds::for_project(&project()), true);
    let lines: Vec<String> = output("timing.txt").lines().map(str::to_string).collect();
    let summary_start = lines.iter().position(|l| l.starts_with("=====")).unwrap();
    // Progress part: each test gets a result as soon as its status is printed.
    let progress: Vec<TestEvent> = lines[..summary_start]
        .iter()
        .flat_map(|l| parser.feed(l))
        .collect();
    let early = results(&progress);
    assert_eq!(early.len(), 10);
    let fail = early
        .iter()
        .find(|r| r.id.ends_with("SampleTest::test_fail"))
        .unwrap();
    assert_eq!(
        (fail.outcome, fail.message.as_str()),
        (Outcome::Failed, "failed")
    );
    // The summary adds the details for everything that did not pass cleanly.
    let mut rest: Vec<TestEvent> = lines[summary_start..]
        .iter()
        .flat_map(|l| parser.feed(l))
        .collect();
    rest.extend(parser.finish(Some(1)));
    let detailed = results(&rest);
    assert_eq!(
        detailed.len(),
        8,
        "every test but the two clean passes: {detailed:#?}"
    );
    let fail = detailed
        .iter()
        .find(|r| r.id.ends_with("SampleTest::test_fail"))
        .unwrap();
    assert_eq!(fail.message, "FailTest: 1 != 2 : one is not two");
    assert!(parser.saw_verdict());
    assert_eq!(done(&rest).exit_code, Some(1));
}

#[test]
fn ids_and_command_line() {
    let root = project();
    let mut ids = TrialIds::for_project(&root);
    assert_eq!(
        ids.node_id("test_sample.SampleTest.test_pass"),
        "tests/test_sample.py::SampleTest::test_pass"
    );
    assert_eq!(ids.node_id("tests/test_broken.py"), "tests/test_broken.py");
    assert_eq!(
        ids.location("tests/test_sample.py::SlowTest::test_sleep"),
        (Some(root.join(SAMPLE)), Some(44))
    );
    assert_eq!(
        trial_module_name(&root.join(SAMPLE)),
        ("test_sample".to_string(), root.join("tests"))
    );
    assert_eq!(
        trial_args(&["test_sample.SampleTest".into()], &["-x".into()]),
        vec![
            "-m",
            "twisted.trial",
            "--reporter=timing",
            "-x",
            "test_sample.SampleTest"
        ]
    );
    assert_eq!(
        trial_args(&[], &["--reporter=bwverbose".into()]),
        vec!["-m", "twisted.trial", "--reporter=bwverbose"]
    );
    assert!(file_uses_trial(
        &std::fs::read_to_string(root.join(SAMPLE)).unwrap()
    ));
    assert_eq!(Framework::Trial.label(), "trial");
    assert_eq!(
        detect_framework_for(&root, &["tests/test_sample.py::SampleTest".into()], None),
        Framework::Trial
    );
    assert_eq!(
        detect_framework_for(&root, &[], None),
        Framework::Trial,
        "every test module uses twisted.trial"
    );
}

#[test]
fn truncated_output_still_reports() {
    // The process was killed during the summary: what is known is reported.
    let text = output("timing.txt");
    let cut = text.find("[FAIL]").unwrap();
    let events = parse_trial_output(&text[..cut], &project());
    assert_eq!(results(&events).len(), 10);
    assert_eq!(
        result(&events, &format!("{SAMPLE}::SampleTest::test_fail")).message,
        "failed"
    );
    assert_eq!(
        result(&events, &format!("{SAMPLE}::SampleTest::test_skip")).message,
        "not today"
    );
    assert_eq!(done(&events).exit_code, None);
}
