//! behave report parsing against recorded output.
//!
//! `fixtures/behave/report-*.json` / `plain-*.txt` are the `json.pretty`
//! report and the `plain` console output of
//! `python -m behave --format json.pretty --outfile report.json --format plain`
//! run in `fixtures/behave/project` with behave 1.3.3 and 1.2.6 (Python 3.12);
//! virtualenv paths in tracebacks were shortened to `/home/user/...`.

use std::path::{Path, PathBuf};

use rustpy_test::*;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/behave")
}

fn project() -> PathBuf {
    fixtures().join("project")
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(fixtures().join(name)).unwrap()
}

const CALC: &str = "features/calculator.feature::Calculator";

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

fn result<'a>(events: &'a [TestEvent], scenario: &str) -> &'a TestResult {
    let id = if scenario.contains("::") {
        scenario.to_string()
    } else {
        format!("{CALC}::{scenario}")
    };
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

#[test]
fn behave_1_3_report_with_console_output() {
    let root = project();
    let index = BehaveIndex::from_items(&discover_features(&root));
    let events = parse_behave_report(
        &fixture("report-1.3.3.json"),
        &root,
        Some(&index),
        Some(&fixture("plain-1.3.3.txt")),
    )
    .unwrap();

    // Features are suites, announced before their scenarios.
    let collected: Vec<(&str, Option<usize>)> = events
        .iter()
        .filter_map(|e| match e {
            TestEvent::Collected { id, line, .. } => Some((id.as_str(), *line)),
            _ => None,
        })
        .collect();
    assert_eq!(collected[0], (CALC, Some(2)));
    assert_eq!(
        collected[1],
        (
            "features/calculator.feature::Calculator::Add two numbers",
            Some(8)
        )
    );
    assert!(collected.contains(&("features/rules.feature::Rules", Some(1))));
    assert!(collected.contains(&("features/rules.feature::Rules::Positive", Some(3))));
    assert!(
        collected.contains(&(
            "features/calculator.feature::Calculator::Add 2 and 2 -- @1.2 Small numbers",
            Some(36)
        )),
        "outline rows are tests at their Examples row"
    );

    let pass = result(&events, "Add two numbers");
    assert_eq!(pass.outcome, Outcome::Passed);
    assert!(pass.duration > 0.0 && pass.message.is_empty());

    let fail = result(&events, "Failing addition");
    assert_eq!(fail.outcome, Outcome::Failed);
    assert_eq!(fail.message, "ASSERT FAILED: expected 5 but got 4");
    assert_eq!(
        fail.stdout, "adding 2 2",
        "captured output from the console"
    );
    assert_eq!(
        parse_failure_location(&fail.traceback, &root),
        Some((root.join("features/calculator.feature"), 14)),
        "the failing step"
    );
    assert!(
        fail.traceback.contains("in Then the result is 5"),
        "{}",
        fail.traceback
    );

    // behave 1.3 keeps the traceback of erroring steps out of the JSON report.
    let error = result(&events, "Exploding step");
    assert_eq!(error.outcome, Outcome::Error);
    assert_eq!(error.message, "ZeroDivisionError: division by zero");
    assert_eq!(
        parse_failure_location(&error.traceback, &root),
        Some((root.join("features/steps/calc_steps.py"), 17)),
        "the step implementation"
    );

    assert_eq!(
        result(&events, "Skipped scenario").outcome,
        Outcome::Skipped
    );
    let undefined = result(&events, "Undefined step");
    assert_eq!(
        (undefined.outcome, undefined.message.as_str()),
        (Outcome::Error, "Undefined step: When I multiply 2 by 3")
    );
    assert_eq!(
        result(&events, "Add 1 and 2 -- @1.1 Small numbers").outcome,
        Outcome::Passed
    );
    let row = result(&events, "Add 2 and 2 -- @1.2 Small numbers");
    assert_eq!(
        (row.outcome, row.stdout.as_str()),
        (Outcome::Failed, "adding 2 2")
    );
    assert_eq!(
        parse_failure_location(&row.traceback, &root),
        Some((root.join("features/calculator.feature"), 31)),
        "outline step line"
    );
    assert_eq!(
        result(&events, "Data table and doc string").outcome,
        Outcome::Passed
    );
    assert_eq!(
        result(&events, "features/rules.feature::Rules::Positive").outcome,
        Outcome::Passed
    );

    // behave: "4 scenarios passed, 2 failed, 2 error, 1 skipped".
    let s = done(&events);
    assert_eq!((s.passed, s.failed, s.errors, s.skipped), (4, 2, 2, 1));
    assert_eq!(s.exit_code, None);
    assert!(s.duration > 0.0);
    assert_eq!(results(&events).len(), 9);
}

#[test]
fn behave_1_2_6_report_alone() {
    let root = project();
    let events = parse_behave_json(&fixture("report-1.2.6.json"), &root).unwrap();
    let fail = result(&events, "Failing addition");
    assert_eq!(
        (fail.outcome, fail.message.as_str(), fail.stdout.as_str()),
        (
            Outcome::Failed,
            "Assertion Failed: expected 5 but got 4",
            "adding 2 2"
        ),
        "captured output split off the error message"
    );
    // behave 1.2.6 reports exceptions as failures, with the traceback in the report.
    let error = result(&events, "Exploding step");
    assert_eq!(
        (error.outcome, error.message.as_str()),
        (Outcome::Failed, "ZeroDivisionError: division by zero")
    );
    assert_eq!(
        parse_failure_location(&error.traceback, &root),
        Some((root.join("features/steps/calc_steps.py"), 17))
    );
    // 1.2.6 strips the trailing `:` of steps with a table, so this one is undefined there.
    let table = result(&events, "Data table and doc string");
    assert_eq!(
        (table.outcome, table.message.as_str()),
        (
            Outcome::Error,
            "Undefined step: Given the following numbers"
        )
    );
    let s = done(&events);
    assert_eq!((s.passed, s.failed, s.errors, s.skipped), (3, 3, 2, 1));
    // Without a static index ids are still built from the names.
    assert!(events.iter().any(|e| matches!(e, TestEvent::Collected { id, path: Some(p), .. } if id == "features/rules.feature::Rules" && *p == root.join("features/rules.feature"))));
}

/// `fixtures/behave/edge`: a hook error, a pending step, captured stdout /
/// stderr / logging, an empty scenario, recorded with behave 1.3.3 and 1.2.6.
#[test]
fn hook_errors_pending_steps_and_captured_streams() {
    let root = fixtures().join("edge/project");
    let index = BehaveIndex::from_items(&discover_features(&root));
    let f = "features/edge.feature::Edge cases";
    for version in ["1.3.3", "1.2.6"] {
        let events = parse_behave_report(
            &fixture(&format!("edge/report-{version}.json")),
            &root,
            Some(&index),
            Some(&fixture(&format!("edge/plain-{version}.txt"))),
        )
        .unwrap();
        let hook = result(&events, &format!("{f}::Hook explodes"));
        assert!(hook.outcome.is_problem(), "{version}: {hook:?}");
        assert_eq!(
            hook.message, "HOOK-ERROR in before_scenario: RuntimeError: hook boom",
            "{version}"
        );
        let pending = result(&events, &format!("{f}::Pending step"));
        assert!(
            pending.outcome.is_problem() && pending.message.ends_with("NotImplementedError: later"),
            "{version}: {pending:?}"
        );
        let noisy = result(&events, &format!("{f}::Noisy failure"));
        assert_eq!(noisy.outcome, Outcome::Failed);
        assert!(
            noisy.message.ends_with("noisy failed"),
            "{version}: {noisy:?}"
        );
        assert_eq!(noisy.stdout, "to stdout", "{version}");
        assert!(
            noisy.stderr.starts_with("to stderr"),
            "{version}: {noisy:?}"
        );
        assert_eq!(
            result(&events, &format!("{f}::Empty scenario")).outcome,
            Outcome::Passed
        );
        assert_eq!(
            result(&events, &format!("{f}::Fine")).outcome,
            Outcome::Passed
        );
        let s = done(&events);
        assert_eq!((s.passed, s.failed + s.errors), (2, 3), "{version}");
    }
}

#[test]
fn malformed_reports_are_errors() {
    assert!(parse_behave_json("", Path::new("/p")).is_err());
    assert!(parse_behave_json("{\"a\": 1}", Path::new("/p")).is_err());
    let empty = parse_behave_json("[\n\n]\n", Path::new("/p")).unwrap();
    assert!(matches!(empty.as_slice(), [TestEvent::Done(s)] if s.total() == 0));
}

#[test]
fn discovery_targets_and_command_line() {
    let root = project();
    let items = discover_features(&root);
    assert_eq!(
        items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
        vec!["features/calculator.feature", "features/rules.feature"]
    );
    let calc = &items[0].children[0];
    assert_eq!(
        (calc.id.as_str(), calc.kind, calc.line),
        (CALC, TestKind::Class, 2)
    );
    let scenarios: Vec<(&str, usize)> = calc
        .children
        .iter()
        .map(|s| (s.name.as_str(), s.line))
        .collect();
    assert_eq!(
        scenarios,
        vec![
            ("Add two numbers", 8),
            ("Failing addition", 12),
            ("Exploding step", 16),
            ("Skipped scenario", 21),
            ("Undefined step", 25),
            ("Add 1 and 2 -- @1.1 Small numbers", 35),
            ("Add 2 and 2 -- @1.2 Small numbers", 36),
            ("Data table and doc string", 38)
        ]
    );
    assert!(calc.children.iter().all(|s| s.kind == TestKind::Function));
    assert_eq!(items.iter().map(TestItem::test_count).sum::<usize>(), 9);

    let targets = behave_targets(
        &root,
        &[
            format!("{CALC}::Failing addition"),
            format!("{CALC}::Add 2 and 2 -- @1.2 Small numbers"),
            root.join("features/rules.feature")
                .to_string_lossy()
                .into_owned(),
            "features/calculator.feature:29".into(),
            "features/rules.feature::Rules".into(),
            format!("{CALC}::Renamed meanwhile"),
            "features".into(),
        ],
    );
    // The outline line `:29` is passed as its Examples rows (behave 1.2 would pick the scenario before it).
    assert_eq!(
        targets,
        vec![
            "features/calculator.feature:12",
            "features/calculator.feature:36",
            "features/rules.feature",
            "features/calculator.feature:35",
            "features/calculator.feature",
            "features"
        ]
    );
    assert_eq!(
        behave_targets(&root, &["features/calculator.feature:14".into()]),
        vec!["features/calculator.feature:12"],
        "a step line selects its scenario"
    );
    assert_eq!(
        behave_targets(&root, &["features/calculator.feature:2".into()]),
        vec!["features/calculator.feature"],
        "the Feature line selects everything"
    );
    assert_eq!(
        behave_targets(&root, &["features/missing.feature:7".into()]),
        vec!["features/missing.feature:7"]
    );

    let report = Path::new("/tmp/report.json");
    assert_eq!(
        behave_args(report, &targets[..2], &["-k".into(), "addition".into()]),
        vec![
            "-m",
            "behave",
            "--format",
            "json.pretty",
            "--outfile",
            "/tmp/report.json",
            "--format",
            "plain",
            "--no-skipped",
            "--name",
            "addition",
            "features/calculator.feature:12",
            "features/calculator.feature:36"
        ]
    );
    assert_eq!(
        behave_args(report, &[], &[]),
        vec![
            "-m",
            "behave",
            "--format",
            "json.pretty",
            "--outfile",
            "/tmp/report.json",
            "--format",
            "plain"
        ]
    );

    assert!(project_uses_behave(&root));
    assert!(is_feature_file(Path::new("x/Login.FEATURE")));
    assert_eq!(
        behave_feature_id("features/x.feature", "Log::in [beta]"),
        "features/x.feature::Log:in (beta)"
    );
    assert_eq!(Framework::Behave.label(), "behave");
    assert_eq!(
        detect_framework_for(&root, &[format!("{CALC}::Failing addition")], None),
        Framework::Behave
    );
    assert_eq!(
        detect_framework_for(&root, &["features/calculator.feature:12".into()], None),
        Framework::Behave
    );
    assert_eq!(
        detect_framework_for(&root, &[], None),
        Framework::Behave,
        "no Python tests, a features/ directory"
    );
}

#[test]
fn feature_files_without_feature_are_skipped() {
    let root = Path::new("/p");
    assert!(
        discover_feature_file_in(root, Path::new("/p/a.feature"), "# just a comment\n").is_empty()
    );
    let items = discover_feature_file_in(root, Path::new("/p/f/a.feature"), "Feature:\n  Scenario:\n    Given x\n  Scenario: same\n    Given y\n  Scenario: same\n    Given z\n");
    let ids: Vec<&str> = items[0].children.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        items[0].id, "f/a.feature::a",
        "an unnamed feature is named after its file"
    );
    assert_eq!(
        ids,
        vec![
            "f/a.feature::a::Scenario (line 2)",
            "f/a.feature::a::same",
            "f/a.feature::a::same (line 6)"
        ]
    );
}
