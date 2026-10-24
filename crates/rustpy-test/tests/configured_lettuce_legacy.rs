//! Genuine untouched Lettuce 0.2.23 on an explicitly admitted Python 2.7 SDK.
use rustpy_test::{Framework, Outcome, TestEvent, TestRun, TestRunOptions};
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
        std::env::var_os("RUSTPY_LETTUCE_PYTHON")
            .expect("explicit compatible Python2.7 SDK required"),
    );
    assert!(path.is_absolute() && path.is_file());
    path
}
fn project() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(directory.path().join("features/steps")).unwrap();
    std::fs::write(directory.path().join("features/actual.feature"), "Feature: Actual Legacy\n  @rejected\n  Scenario: fails\n    Given the actual legacy value is 1\n    Then the actual legacy value equals 2\n  @accepted\n  Scenario: succeeds\n    Given the actual legacy value is 1\n    Then the actual legacy value equals 1\n  @accepted\n  Scenario: another\n    Given the actual legacy value is 2\n    Then the actual legacy value equals 2\n").unwrap();
    std::fs::write(
        directory.path().join("features/steps/actual_steps.py"),
        r#"import os
import sys
from lettuce import step, world
@step(r'the actual legacy value is (\d+)')
def actual_value(step, value):
    assert sys.version_info[:2] == (2, 7), 'the fixture requires the real selected Python2.7 SDK'
    assert os.environ.get('RUSTPY_LEGACY_ONLY') == 'actual-configured-environment'
    world.value = int(value)
@step(r'the actual legacy value equals (\d+)')
def actual_check(step, value):
    print('ACTUAL_LETTUCE_STDOUT')
    assert world.value == int(value), 'ACTUAL_LETTUCE_ASSERTION'
"#,
    )
    .unwrap();
    directory
}
fn options() -> TestRunOptions {
    TestRunOptions {
        env: vec![(
            "RUSTPY_LEGACY_ONLY".into(),
            "actual-configured-environment".into(),
        )],
        ..Default::default()
    }
}
fn finish(run: &mut TestRun) -> Vec<TestEvent> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut events = Vec::new();
    while run.is_running() {
        events.extend(run.poll());
        if Instant::now() >= deadline {
            run.stop();
            panic!("legacy provider timed out: {events:#?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    events.extend(run.poll());
    events
}
fn execute(root: &Path, targets: &[String], arguments: &[String]) -> Vec<TestEvent> {
    let mut run = TestRun::start_with(
        95,
        Framework::Lettuce,
        &python(),
        root,
        targets,
        arguments,
        &options(),
    )
    .unwrap();
    assert!(
        run.command_line.contains("rustpy_test_entry.py"),
        "all public task routes use the interpreter-compatible dispatcher"
    );
    finish(&mut run)
}
fn verdict(events: &[TestEvent]) -> (usize, usize, usize, Option<i32>) {
    events
        .iter()
        .find_map(|event| {
            if let TestEvent::Done(done) = event {
                Some((done.passed, done.failed, done.errors, done.exit_code))
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("real native completion missing: {events:#?}"))
}

#[test]
#[ignore = "requires RUSTPY_REQUIRE_PROVIDER_TESTS=1 and real RUSTPY_LETTUCE_PYTHON pointing to Python2.7 SDK with untouched Lettuce0.2.23"]
fn configured_python2_lettuce_actual_failures_selection_tags_and_process_entry() {
    let directory = project();
    let root = dunce::canonicalize(directory.path()).unwrap();
    let events = execute(&root, &["features/actual.feature".into()], &[]);
    assert_eq!(verdict(&events), (2, 1, 0, Some(1)), "{events:#?}");
    let failed = events
        .iter()
        .find_map(|event| {
            if let TestEvent::Result(result) = event {
                (result.id.ends_with("::fails")).then_some(result)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(failed.outcome, Outcome::Failed);
    assert!(failed
        .id
        .starts_with("features/actual.feature::Actual Legacy::"));
    assert!(
        failed.traceback.contains("ACTUAL_LETTUCE_ASSERTION"),
        "real step.why traceback must survive: {failed:#?}"
    );
    assert!(failed.stdout.contains("ACTUAL_LETTUCE_STDOUT"));
    for event in &events {
        if let TestEvent::Result(result) = event {
            let start = events
                .iter()
                .position(|event| matches!(event, TestEvent::Started(id) if id == &result.id))
                .unwrap();
            let end = events
                .iter()
                .position(
                    |event| matches!(event, TestEvent::Result(found) if found.id == result.id),
                )
                .unwrap();
            assert!(start < end);
        }
    }
    let selected = "features/actual.feature::Actual Legacy::succeeds".to_string();
    let events = execute(&root, std::slice::from_ref(&selected), &[]);
    assert_eq!(verdict(&events), (1, 0, 0, Some(0)), "{events:#?}");
    let events = execute(
        &root,
        &["features/actual.feature".into()],
        &["--tags".into(), "accepted".into()],
    );
    assert_eq!(verdict(&events), (2, 0, 0, Some(0)), "{events:#?}");
    let events = execute(
        &root,
        &["features/actual.feature".into()],
        &["--failfast".into()],
    );
    assert_eq!(verdict(&events), (0, 1, 0, Some(1)), "{events:#?}");
    let events = execute(&root, &["features/actual.feature::missing".into()], &[]);
    assert!(events.iter().any(|event| matches!(event, TestEvent::Failed(message) if message.contains("scenario is unavailable"))), "{events:#?}");
    assert_eq!(verdict(&events).3, Some(2));

    // The exact returned process adapter is also used by UI Debug/Coverage/Profile.
    let scripts = root.join("provisioned");
    let entry = rustpy_test::provision_reporter(&scripts).unwrap();
    let mut command = std::process::Command::new(python());
    command
        .arg(entry)
        .args(["--framework", "lettuce", "--root"])
        .arg(&root)
        .args(["--", &selected])
        .env("RUSTPY_LEGACY_ONLY", "actual-configured-environment");
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records: Vec<serde_json::Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("##rustpy-test## "))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(records.iter().any(|record| record["event"] == "result"
        && record["id"] == selected
        && record["outcome"] == "passed"));
    assert!(records
        .iter()
        .any(|record| record["event"] == "done" && record["exit_code"] == 0));
}

#[test]
#[ignore = "requires actual Python2.7/Lettuce SDK; native failfast must preserve genuine callback errors"]
fn configured_python2_lettuce_failfast_preserves_scenario_and_feature_hook_errors() {
    for (hook, late) in [
        ("each_scenario", false),
        ("each_feature", false),
        ("each_feature", true),
    ] {
        let directory = project();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let registration = if late {
            format!(
                "@before.all\ndef register_late_hook():\n    after.{hook}(actual_broken_hook)\n"
            )
        } else {
            format!("after.{hook}(actual_broken_hook)\n")
        };
        std::fs::write(root.join("features/terrain.py"), format!(
            "from lettuce import before, after\ndef actual_broken_hook(value):\n    raise RuntimeError('ACTUAL_LETTUCE_HOOK_{hook}')\n{registration}"
        )).unwrap();
        let events = execute(
            &root,
            &["features/actual.feature".into()],
            &["--failfast".into()],
        );
        assert_eq!(verdict(&events), (0, 1, 1, Some(2)), "{hook}: {events:#?}");
        let marker = format!("ACTUAL_LETTUCE_HOOK_{hook}");
        assert!(events.iter().any(|event| matches!(event, TestEvent::Failed(message) if message.contains(&marker) && message.contains("RuntimeError") && message.contains("actual_broken_hook"))), "original escaping callback traceback must survive Runner's failfast sentinel: {hook}: {events:#?}");
        assert!(events.iter().any(|event| matches!(event, TestEvent::Result(result) if result.outcome == Outcome::Failed && result.traceback.contains("ACTUAL_LETTUCE_ASSERTION"))), "actual step failure remains independently reported: {hook}: {events:#?}");
    }
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires actual Python2.7/Lettuce SDK; stops a real running native scenario and descendant"]
fn configured_python2_lettuce_stop_retires_owned_scenario_and_descendant() {
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    std::fs::create_dir_all(root.join("features/steps")).unwrap();
    std::fs::write(
        root.join("features/wait.feature"),
        "Feature: Owned wait\n  Scenario: sleeps\n    Given an actual legacy child waits\n",
    )
    .unwrap();
    std::fs::write(
        root.join("features/steps/wait.py"),
        r#"import json, os, subprocess, sys, time
from lettuce import step
@step(r'an actual legacy child waits')
def actual_wait(step):
    assert sys.version_info[:2] == (2, 7)
    child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])
    with open('owned.tmp', 'w') as stream:
        json.dump(dict(parent=os.getpid(), child=child.pid), stream)
        stream.flush()
        os.fsync(stream.fileno())
    os.rename('owned.tmp', 'owned.json')
    time.sleep(60)
"#,
    )
    .unwrap();
    let mut run = TestRun::start_with(
        96,
        Framework::Lettuce,
        &python(),
        &root,
        &["features/wait.feature".into()],
        &[],
        &options(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !root.join("owned.json").is_file() && Instant::now() < deadline {
        let events = run.poll();
        assert!(
            run.is_running(),
            "real waiting scenario exited: {events:#?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let owners: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("owned.json"))
            .expect("actual legacy scenario must create its owned descendant"),
    )
    .unwrap();
    run.stop();
    let _events = finish(&mut run);
    assert!(!run.is_running());
    for owner in ["parent", "child"] {
        let path = PathBuf::from(format!("/proc/{}", owners[owner].as_u64().unwrap()));
        assert!(
            !path.exists(),
            "physical legacy owner {owner} must retire: {}",
            path.display()
        );
    }
}

#[test]
#[ignore = "requires actual admitted Python2.7 SDK; verifies missing provider under isolated -S execution"]
fn configured_python2_lettuce_missing_provider_is_an_explicit_failure() {
    let directory = tempfile::tempdir().unwrap();
    let root = dunce::canonicalize(directory.path()).unwrap();
    let options = TestRunOptions {
        inherit_environment: false,
        interpreter_options: vec!["-S".into()],
        ..Default::default()
    };
    let mut run =
        TestRun::start_with(97, Framework::Lettuce, &python(), &root, &[], &[], &options).unwrap();
    let events = finish(&mut run);
    assert!(events.iter().any(|event| matches!(event, TestEvent::Failed(message) if message.contains("lettuce is not installed"))), "{events:#?}");
    assert_eq!(verdict(&events), (0, 0, 1, Some(2)));
    assert!(!events
        .iter()
        .any(|event| matches!(event, TestEvent::Result(_))));
}
