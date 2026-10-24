//! CI entry point for the IDE's actual test reporter and owned process tree.
use crate::args::TestArgs;
use rustpy_settings::Settings;
use rustpy_test::{Framework, Outcome, Summary, TestEvent, TestResult, TestRun, TestRunOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

static CANCELLED: OnceLock<Result<Arc<AtomicBool>, String>> = OnceLock::new();

fn cancellation() -> Result<Arc<AtomicBool>, String> {
    CANCELLED
        .get_or_init(|| {
            let flag = Arc::new(AtomicBool::new(false));
            let signal = flag.clone();
            // termination also handles SIGTERM/SIGHUP on Unix and console Ctrl-Break
            // on Windows. The handler only requests cancellation; this thread drains
            // the protocol and TestRun owns termination and reaping.
            ctrlc::set_handler(move || {
                signal.store(true, Ordering::SeqCst);
            })
            .map_err(|error| format!("cannot install termination handler: {error}"))?;
            Ok(flag)
        })
        .clone()
}

fn resolve(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.into()
    } else {
        root.join(path)
    }
}

fn interpreter(args: &TestArgs, root: &Path) -> Result<PathBuf, String> {
    let snapshot = rustpy_project::workspace::WorkspaceSnapshot::try_load(vec![root.into()])?;
    let model = rustpy_project::ProjectModel::try_load(root)?;
    let module_sdk = args.targets.first().and_then(|target| {
        model.sdk_for(&resolve(
            root,
            Path::new(target.split("::").next().unwrap_or(target)),
        ))
    });
    let selected = args
        .interpreter
        .as_deref()
        .or(module_sdk)
        .or(snapshot.ide_interpreter.as_deref());
    if args.interpreter.is_none() {
        for target in &args.targets {
            let anchor = resolve(root, Path::new(target.split("::").next().unwrap_or(target)));
            let sdk = model
                .sdk_for(&anchor)
                .or(snapshot.ide_interpreter.as_deref());
            if sdk != selected {
                return Err(
                    "targets use different module SDKs; split the run or choose --interpreter"
                        .into(),
                );
            }
        }
    }
    if let Some(path) = selected {
        if path.components().count() == 1 {
            return which::which(path)
                .map_err(|_| format!("interpreter not found: {}", path.display()));
        }
        let path = resolve(root, path);
        return path
            .is_file()
            .then_some(path)
            .ok_or_else(|| "configured interpreter is missing; use --interpreter".into());
    }
    for local in [
        ".venv/bin/python",
        ".venv/Scripts/python.exe",
        "venv/bin/python",
        "venv/Scripts/python.exe",
    ] {
        let path = root.join(local);
        if path.is_file() {
            return Ok(path);
        }
    }
    which::which("python3")
        .or_else(|_| which::which("python"))
        .map_err(|_| "no Python interpreter found; use --interpreter".into())
}

/// Resolve auto detection in an owned helper process: the existing detector may
/// import pytest, so it must participate in the same timeout and cancellation
/// boundary as the test run rather than leaving a detached probing thread.
fn framework(
    args: &TestArgs,
    settings: &Settings,
    root: &Path,
    python: &Path,
    token: &rustpy_core::CancellationToken,
) -> Result<Framework, String> {
    let preference = if args.framework != "auto" {
        &args.framework
    } else if settings.testing.default_runner != "auto" {
        &settings.testing.default_runner
    } else {
        &settings.python_testing.runner
    };
    if let Some(selected) = Framework::from_key(preference).or_else(|| {
        args.targets
            .is_empty()
            .then(|| rustpy_test::configured_framework(root, preference))
            .flatten()
    }) {
        return Ok(selected);
    }
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut command = std::process::Command::new(executable);
    command
        .arg("--headless")
        .arg("test")
        .arg("--internal-framework-probe")
        .arg("--project")
        .arg(root)
        .arg("--interpreter")
        .arg(python);
    if !args.inherit_environment {
        command.env_clear();
    }
    command.envs(args.env.iter().cloned());
    // Global settings directories were already captured by apply_global_options;
    // keep those identities even for an otherwise empty test environment.
    for key in ["RUSTPY_CONFIG_DIR", "RUSTPY_SYSTEM_DIR", "RUSTPY_LOG_DIR"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
        .args(&args.targets)
        .stdin(std::process::Stdio::null());
    let result = probe_output(&mut command, token)?;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr).trim().into());
    }
    Framework::from_key(String::from_utf8_lossy(&result.stdout).trim())
        .ok_or_else(|| "framework detector returned an invalid response".into())
}

/// The framework helper and its imports share one reserved process group/job.
/// In particular a Python startup hook cannot orphan its children on SIGTERM.
fn probe_output(
    command: &mut std::process::Command,
    token: &rustpy_core::CancellationToken,
) -> Result<std::process::Output, String> {
    use std::io::Read;
    rustpy_run::ProcessTree::prepare(command);
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let tree = match rustpy_run::ProcessTree::new(&mut child) {
        Ok(tree) => tree,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.to_string());
        }
    };
    struct Owned {
        child: std::process::Child,
        tree: rustpy_run::ProcessTree,
    }
    impl Drop for Owned {
        fn drop(&mut self) {
            self.tree.force(&mut self.child);
        }
    }
    let drain = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut output = Vec::new();
            let mut bytes = [0u8; 4096];
            while let Ok(count) = pipe.read(&mut bytes) {
                if count == 0 {
                    break;
                }
                let keep = count.min(16_384usize.saturating_sub(output.len()));
                output.extend_from_slice(&bytes[..keep]);
            }
            output
        })
    };
    let stdout = child.stdout.take().map(|pipe| drain(Box::new(pipe)));
    let stderr = child.stderr.take().map(|pipe| drain(Box::new(pipe)));
    let mut owned = Owned { child, tree };
    let result = loop {
        if token.is_cancelled() {
            owned.tree.force(&mut owned.child);
            break Err("cancelled".into());
        }
        match owned.tree.poll(&mut owned.child) {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => std::thread::sleep(Duration::from_millis(15)),
            Err(error) => {
                owned.tree.force(&mut owned.child);
                break Err(error.to_string());
            }
        }
    };
    let collect = |thread: Option<std::thread::JoinHandle<Vec<u8>>>| {
        thread
            .map(|thread| thread.join().unwrap_or_default())
            .unwrap_or_default()
    };
    let stdout = collect(stdout);
    let stderr = collect(stderr);
    result.map(|status| std::process::Output {
        status,
        stdout,
        stderr,
    })
}

fn preferences(
    args: &TestArgs,
    settings: &Settings,
    root: &Path,
    framework: Framework,
) -> (Vec<String>, TestRunOptions) {
    let mut extra = Vec::new();
    let mut options = TestRunOptions {
        inherit_environment: args.inherit_environment,
        env: Vec::new(),
        cwd: args.cwd.clone(),
        interpreter_options: args.interpreter_options.clone(),
        collect_runtime_types: false,
    };
    if framework == Framework::Pytest {
        // Same argument lexer used by External Tools, including quoted spaces.
        extra.extend(rustpy_project::RunConfig::split_args(
            &settings.python_testing.pytest_arguments,
        ));
        if settings.testing.suppress_buffering {
            extra.push("-s".into());
        }
        let custom = settings.testing.pytest_path.clone().or_else(|| {
            (!settings.python_testing.pytest_path.is_empty())
                .then(|| PathBuf::from(&settings.python_testing.pytest_path))
        });
        if let Some(custom) = custom {
            options.env.push((
                "RUSTPY_PYTEST_PATH".into(),
                resolve(root, &custom).display().to_string(),
            ));
        }
    }
    if framework == Framework::Unittest
        && (args.targets.is_empty()
            || args
                .targets
                .iter()
                .all(|target| resolve(root, Path::new(target)).is_dir()))
    {
        let pattern = if settings.testing.unittest_pattern == "test*.py" {
            &settings.python_testing.unittest_pattern
        } else {
            &settings.testing.unittest_pattern
        };
        if !pattern.is_empty()
            && !args
                .extra
                .iter()
                .any(|arg| arg == "-p" || arg == "--pattern")
        {
            extra.extend(["-p".into(), pattern.clone()]);
        }
        if let Some(path) = &settings.testing.unittest_top_level {
            if !args
                .extra
                .iter()
                .any(|arg| arg == "-t" || arg == "--top-level-directory")
            {
                extra.extend(["-t".into(), resolve(root, path).display().to_string()]);
            }
        }
    }
    // Match project import roots while respecting an explicit PYTHONPATH and an
    // empty-environment request. TestRun itself supplies reporter paths.
    if !args.env.iter().any(|(name, _)| name == "PYTHONPATH") {
        if let Ok(workspace) = rustpy_project::WorkspaceSnapshot::try_load(vec![root.into()]) {
            let mut paths = vec![root.to_path_buf()];
            paths.extend(workspace.source_roots);
            paths.extend(workspace.test_roots);
            if args.inherit_environment {
                if let Some(inherited) = std::env::var_os("PYTHONPATH") {
                    paths.extend(std::env::split_paths(&inherited));
                }
            }
            if let Ok(value) = std::env::join_paths(paths) {
                options
                    .env
                    .push(("PYTHONPATH".into(), value.to_string_lossy().into_owned()));
            }
        }
    }
    options.env.extend(args.env.clone());
    extra.extend(args.extra.clone());
    (extra, options)
}

pub fn run(args: &TestArgs, settings: &Settings, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    match execute(args, settings, out, err) {
        Ok(code) => code,
        Err(error) => {
            let _ = writeln!(err, "rustpy test: {error}");
            if args.json {
                let _ = serde_json::to_writer(
                    &mut *out,
                    &serde_json::json!({"error":error,"exit_code":2}),
                );
                let _ = writeln!(out);
            }
            if let Some(destination) = &args.junit {
                if let Err(report_error) =
                    write_report(destination, &junit(&[], &Summary::default(), 2))
                {
                    let _ = writeln!(err, "rustpy test: JUnit report: {report_error}");
                }
            }
            2
        }
    }
}

fn execute(
    args: &TestArgs,
    settings: &Settings,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<i32, String> {
    let root = args
        .project
        .canonicalize()
        .map_err(|error| format!("project: {error}"))?;
    if !root.is_dir() {
        return Err("--project must be a directory".into());
    }
    let python = interpreter(args, &root)?;
    if args.probe_framework {
        let detected = rustpy_test::detect_framework_for(&root, &args.targets, Some(&python));
        writeln!(out, "{}", detected.label()).map_err(|error| error.to_string())?;
        return Ok(0);
    }
    let cancelled = cancellation()?;
    let token = rustpy_core::CancellationToken::new();
    let started = Instant::now();
    let timed_out = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    // Own and join the small cancellation monitor even when setup returns early.
    let monitor = Monitor::new(
        finished,
        cancelled.clone(),
        timed_out.clone(),
        token.clone(),
        started,
        args.timeout_seconds,
    );
    let framework = match framework(args, settings, &root, &python, &token) {
        Ok(framework) => framework,
        Err(_) if token.is_cancelled() => {
            return aborted(
                args,
                if timed_out.load(Ordering::SeqCst) {
                    124
                } else {
                    130
                },
                out,
            )
        }
        Err(error) => return Err(error),
    };
    if token.is_cancelled() {
        return aborted(
            args,
            if timed_out.load(Ordering::SeqCst) {
                124
            } else {
                130
            },
            out,
        );
    }
    let (extra, options) = preferences(args, settings, &root, framework);
    let mut run = TestRun::start_with(
        1,
        framework,
        &python,
        &root,
        &args.targets,
        &extra,
        &options,
    )
    .map_err(|error| error.to_string())?;
    let mut results = Vec::new();
    let mut summary = None;
    let mut failed = false;
    let mut process_exit = None;
    let mut stopped = None;
    loop {
        if token.is_cancelled() && stopped.is_none() {
            run.stop();
            stopped = Some(Instant::now());
        }
        for event in run.poll() {
            match event {
                TestEvent::Result(result) => {
                    if !args.json {
                        writeln!(out, "{} {}", result.outcome.label(), result.id)
                            .map_err(|e| e.to_string())?;
                        if result.outcome.is_problem() {
                            for detail in [
                                &result.message,
                                &result.traceback,
                                &result.stdout,
                                &result.stderr,
                            ] {
                                if !detail.is_empty() {
                                    writeln!(err, "{detail}").map_err(|error| error.to_string())?;
                                }
                            }
                        }
                    }
                    results.push(result);
                }
                TestEvent::Done(done) => summary = Some(done),
                TestEvent::Failed(message) => {
                    failed = true;
                    writeln!(err, "{message}").map_err(|e| e.to_string())?;
                }
                TestEvent::Exited(code) => process_exit = Some(code),
                // Keep stdout valid JSON; console chatter belongs to stderr in JSON mode.
                TestEvent::Output(line, is_stderr) => {
                    if args.json || is_stderr {
                        writeln!(err, "{line}")
                    } else {
                        writeln!(out, "{line}")
                    }
                    .map_err(|e| e.to_string())?;
                }
                _ => {}
            }
        }
        if !run.is_running() || stopped.is_some_and(|time| time.elapsed() > Duration::from_secs(5))
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    let mut summary = summary.unwrap_or_else(|| summarize(&results));
    summary.exit_code = process_exit.or(summary.exit_code);
    let code = if timed_out.load(Ordering::SeqCst) {
        124
    } else if cancelled.load(Ordering::SeqCst) {
        130
    } else if failed || process_exit.is_none() {
        2
    } else if summary.total() == 0 {
        5
    } else if summary.is_success() {
        0
    } else {
        1
    };
    if let Some(destination) = &args.junit {
        let report = junit(&results, &summary, code);
        write_report(destination, &report).map_err(|error| format!("JUnit report: {error}"))?;
    }
    if args.json {
        serde_json::to_writer(
            &mut *out,
            &serde_json::json!({"framework": framework.label(), "interpreter": python,
            "project":root, "results":results, "summary":summary, "exit_code":code,
            "cancelled":code == 130, "timed_out":code == 124}),
        )
        .map_err(|error| error.to_string())?;
        writeln!(out).map_err(|error| error.to_string())?;
    } else {
        writeln!(out, "{}", summary.label()).map_err(|error| error.to_string())?;
    }
    drop(monitor);
    Ok(code)
}

fn aborted(args: &TestArgs, code: i32, out: &mut dyn Write) -> Result<i32, String> {
    if let Some(destination) = &args.junit {
        write_report(destination, &junit(&[], &Summary::default(), code))
            .map_err(|error| error.to_string())?;
    }
    if args.json {
        serde_json::to_writer(
            &mut *out,
            &serde_json::json!({"results":[], "summary":Summary::default(),
            "exit_code":code,"cancelled":code == 130,"timed_out":code == 124}),
        )
        .map_err(|error| error.to_string())?;
        writeln!(out).map_err(|error| error.to_string())?;
    } else {
        writeln!(
            out,
            "test run {}",
            if code == 124 {
                "timed out"
            } else {
                "cancelled"
            }
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(code)
}

fn write_report(destination: &Path, report: &str) -> std::io::Result<()> {
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(report.as_bytes())?;
    file.as_file().sync_all()?;
    file.persist(destination).map_err(|error| error.error)?;
    Ok(())
}

struct Monitor {
    finished: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Monitor {
    fn new(
        finished: Arc<AtomicBool>,
        cancelled: Arc<AtomicBool>,
        timed_out: Arc<AtomicBool>,
        token: rustpy_core::CancellationToken,
        started: Instant,
        timeout: Option<u64>,
    ) -> Self {
        let done = finished.clone();
        let thread = std::thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                if timeout.is_some_and(|seconds| started.elapsed() >= Duration::from_secs(seconds))
                {
                    timed_out.store(true, Ordering::SeqCst);
                    token.cancel();
                    break;
                }
                if cancelled.load(Ordering::SeqCst) {
                    token.cancel();
                    break;
                }
                std::thread::sleep(Duration::from_millis(15));
            }
        });
        Self {
            finished,
            thread: Some(thread),
        }
    }
}
impl Drop for Monitor {
    fn drop(&mut self) {
        self.finished.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn summarize(results: &[TestResult]) -> Summary {
    let mut summary = Summary::default();
    for result in results {
        match result.outcome {
            Outcome::Passed => summary.passed += 1,
            Outcome::Failed => summary.failed += 1,
            Outcome::Skipped => summary.skipped += 1,
            Outcome::Error => summary.errors += 1,
        }
        summary.duration += result.duration;
    }
    summary
}
fn xml(value: &str) -> String {
    value
        .chars()
        .filter(|ch| matches!(ch, '\t' | '\n' | '\r') || *ch >= '\u{20}')
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn junit(results: &[TestResult], summary: &Summary, code: i32) -> String {
    let infrastructure_error = code != 0 && summary.failed + summary.errors == 0;
    let mut report = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuite name=\"RustPy\" tests=\"{}\" failures=\"{}\" errors=\"{}\" skipped=\"{}\" time=\"{}\">\n",
        results.len() + usize::from(infrastructure_error), summary.failed, summary.errors + usize::from(infrastructure_error), summary.skipped, summary.duration);
    for result in results {
        report.push_str(&format!(
            "<testcase name=\"{}\" time=\"{}\">",
            xml(&result.id),
            result.duration
        ));
        let element = match result.outcome {
            Outcome::Passed => None,
            Outcome::Failed => Some("failure"),
            Outcome::Error => Some("error"),
            Outcome::Skipped => Some("skipped"),
        };
        if let Some(element) = element {
            report.push_str(&format!(
                "<{element} message=\"{}\">{}</{element}>",
                xml(&result.message),
                xml(&result.traceback)
            ));
        }
        report.push_str(&format!(
            "<system-out>{}</system-out><system-err>{}</system-err></testcase>\n",
            xml(&result.stdout),
            xml(&result.stderr)
        ));
    }
    if infrastructure_error {
        report.push_str(&format!(
            "<testcase name=\"runner\"><error message=\"RustPy test exit {code}\"/></testcase>\n"
        ));
    }
    report.push_str("</testsuite>\n");
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn junit_preserves_failure_capture_and_escapes_xml() {
        // Runner protocol outcomes use lowercase names, including failures.
        let result: TestResult = serde_json::from_value(serde_json::json!({
            "id": "tests/a.py::test_<x>",
            "outcome": "failed",
            "message": "a & b",
            "traceback": "bad <value>",
            "stdout": "\u{0}hello",
            "stderr": "error & detail"
        }))
        .unwrap();
        assert_eq!(result.outcome, Outcome::Failed);
        let summary = summarize(std::slice::from_ref(&result));
        let report = junit(&[result], &summary, 1);
        assert!(report.contains("failures=\"1\""));
        assert!(report.contains("test_&lt;x&gt;"));
        assert!(report.contains("a &amp; b"));
        assert!(report.contains("bad &lt;value&gt;</failure>"));
        assert!(report.contains("<system-out>hello</system-out>"));
        assert!(report.contains("<system-err>error &amp; detail</system-err>"));
        assert!(!report.contains('\0'));
    }
    #[test]
    fn no_tests_timeout_and_cancellation_are_junit_failures() {
        for code in [2, 5, 124, 130] {
            assert!(junit(&[], &Summary::default(), code).contains("<error message="));
        }
        assert!(!junit(&[], &Summary::default(), 0).contains("<error message="));
    }
    #[test]
    fn unittest_ci_uses_real_protocol_environment_and_separate_json_console() {
        let Ok(python) = which::which("python3").or_else(|_| which::which("python")) else {
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join("test_ci.py"),
            r#"import os, unittest
class TestCI(unittest.TestCase):
    def test_pass(self):
        print('raw console output')
        self.assertEqual(os.environ.get('RUSTPY_CLI_FIXTURE'), 'ready')
    def test_failure(self):
        self.assertEqual('expected', 'actual')
"#,
        )
        .unwrap();
        let report = directory.path().join("results.xml");
        let cli = crate::parse([
            "--headless".to_owned(),
            "test".into(),
            "--framework=unittest".into(),
            "--project".into(),
            directory.path().display().to_string(),
            "--interpreter".into(),
            python.display().to_string(),
            "--env=RUSTPY_CLI_FIXTURE=ready".into(),
            "--json".into(),
            "--junit".into(),
            report.display().to_string(),
        ])
        .unwrap();
        let crate::Command::Test(args) = cli.command else {
            unreachable!()
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(run(&args, &Settings::default(), &mut out, &mut err), 1);
        let result: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(result["summary"]["passed"], 1);
        assert_eq!(result["summary"]["failed"], 1);
        assert_eq!(result["exit_code"], 1);
        assert!(
            String::from_utf8(err)
                .unwrap()
                .contains("raw console output")
                || result["results"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|value| value["stdout"]
                        .as_str()
                        .unwrap_or("")
                        .contains("raw console output"))
        );
        assert!(std::fs::read_to_string(report)
            .unwrap()
            .contains("<failure"));
    }
    #[test]
    fn timeout_stops_term_ignoring_descendant_and_writes_ci_results() {
        let Ok(python) = which::which("python3").or_else(|_| which::which("python")) else {
            return;
        };
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("surviving-child");
        let child = format!("import time, signal; signal.signal(signal.SIGINT, signal.SIG_IGN); signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(3); open({}, 'w').write('leaked')", serde_json::to_string(&marker.display().to_string()).unwrap());
        std::fs::write(
            directory.path().join("test_timeout.py"),
            format!(
                r#"import signal, subprocess, sys, time, unittest
class TestTimeout(unittest.TestCase):
    def test_wait(self):
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        subprocess.Popen([sys.executable, '-c', {child}])
        time.sleep(60)
"#,
                child = serde_json::to_string(&child).unwrap()
            ),
        )
        .unwrap();
        let cli = crate::parse([
            "test".to_owned(),
            "--framework=unittest".into(),
            "--project".into(),
            directory.path().display().to_string(),
            "--interpreter".into(),
            python.display().to_string(),
            "--timeout=1".into(),
            "--json".into(),
        ])
        .unwrap();
        let crate::Command::Test(args) = cli.command else {
            unreachable!()
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(run(&args, &Settings::default(), &mut out, &mut err), 124);
        let result: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(result["timed_out"], true);
        std::thread::sleep(Duration::from_secs(3));
        assert!(!marker.exists(), "test descendant survived timeout");
    }
    #[test]
    fn report_failure_is_nonzero_and_settings_preserve_exact_quoted_arguments() {
        let directory = tempfile::tempdir().unwrap();
        let cli = crate::parse([
            "test",
            "--framework=pytest",
            "--env=PYTHONPATH=explicit",
            "--",
            "--capture=tee-sys",
        ])
        .unwrap();
        let crate::Command::Test(args) = cli.command else {
            unreachable!()
        };
        let mut settings = Settings::default();
        settings.python_testing.pytest_arguments = "-k 'slow or fast'".into();
        let (arguments, options) =
            preferences(&args, &settings, directory.path(), Framework::Pytest);
        assert_eq!(arguments, ["-k", "slow or fast", "--capture=tee-sys"]);
        assert_eq!(
            options
                .env
                .iter()
                .filter(|(name, _)| name == "PYTHONPATH")
                .collect::<Vec<_>>(),
            vec![&("PYTHONPATH".into(), "explicit".into())]
        );
        assert!(write_report(&directory.path().join("missing").join("report.xml"), "xml").is_err());
    }
}
