//! End-to-end tests for the coverage and profiler runners (system `python3`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rustpy_project::{Interpreter, RunConfig, RunConfigKind};
use rustpy_run::coverage::{
    coverage_available as probe_coverage_available, html_report, import_report, kept_data_file,
    load_json, run_with_coverage, run_with_coverage_opts, CoverageOptions, LineCoverage,
};
use rustpy_run::profile::{load_json_lines, run_with_profiler, PROFILE_PREFIX};

fn required_providers() -> bool {
    std::env::var("RUSTPY_REQUIRE_PROVIDER_TESTS").as_deref() == Ok("1")
}

fn python() -> Option<PathBuf> {
    if required_providers() {
        let path = PathBuf::from(
            std::env::var_os("RUSTPY_PROVIDER_PYTHON")
                .expect("required provider acceptance needs RUSTPY_PROVIDER_PYTHON"),
        );
        assert!(
            path.is_absolute() && path.is_file(),
            "admit an absolute installed Python interpreter"
        );
        eprintln!("REQUIRED_PROVIDER Python={}", path.display());
        Some(path)
    } else {
        which::which("python3")
            .or_else(|_| which::which("python"))
            .ok()
    }
}

fn coverage_available(python: &Path) -> bool {
    let available = probe_coverage_available(python);
    if required_providers() {
        assert!(
            available,
            "required coverage.py provider missing for {}",
            python.display()
        );
        eprintln!("REQUIRED_PROVIDER coverage.py python={}", python.display());
    }
    available
}

fn interpreter(path: &Path) -> Interpreter {
    Interpreter {
        path: path.to_path_buf(),
        version: "3.12.0".into(),
        prefix: PathBuf::from("/usr"),
        sys_path: Vec::new(),
        is_venv: false,
        platform: "linux".into(),
        kind: "system".into(),
    }
}

const SCRIPT: &str = r#"import sys


def hot(n):
    total = 0
    for i in range(n):
        total += i * i
    return total


def cold():
    return "never called"


def main():
    print("args:", sys.argv[1:])
    for _ in range(3):
        hot(2000)
    if len(sys.argv) > 5:
        cold()
    print("done", file=sys.stderr)


if __name__ == "__main__":
    main()
"#;

fn wait_until<F: FnMut() -> bool>(mut done: F, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn profiler_runs_script_and_reports_hot_function() {
    let Some(py) = python() else {
        eprintln!("python3 not found; skipping");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("prog.py");
    std::fs::write(&script, SCRIPT).unwrap();
    let mut cfg = RunConfig::for_script(&script);
    cfg.parameters = "alpha \"two words\"".into();
    cfg.env.insert("RUSTPY_TEST_ENV".into(), "1".into());
    let interp = interpreter(&py);
    let mut run = run_with_profiler(1, &cfg, Some(&interp), dir.path()).expect("profiler starts");
    assert!(
        run.command_line.contains("rustpy-profile-runner"),
        "{}",
        run.command_line
    );
    let mut output: Vec<(String, bool)> = Vec::new();
    assert!(
        wait_until(
            || {
                output.extend(run.poll());
                !run.is_running()
            },
            Duration::from_secs(60)
        ),
        "profiler run did not finish"
    );
    output.extend(run.poll());
    assert_eq!(run.exit_code(), Some(0), "output: {output:?}");
    assert!(run.error().is_none(), "{:?}", run.error());
    // Program output is forwarded, profile lines are consumed.
    assert!(
        output
            .iter()
            .any(|(l, err)| !*err && l == "args: ['alpha', 'two words']"),
        "{output:?}"
    );
    assert!(
        output.iter().any(|(l, err)| *err && l == "done"),
        "{output:?}"
    );
    assert!(
        !output
            .iter()
            .any(|(l, _)| l.starts_with(PROFILE_PREFIX.trim_end())),
        "profile lines leaked: {output:?}"
    );
    let report = run.report().expect("profile report");
    if required_providers() {
        assert_eq!(report.engine, "cprofile");
        eprintln!(
            "REQUIRED_PROVIDER cprofile python={} functions={}",
            py.display(),
            report.functions.len()
        );
    }
    let hot = report
        .functions
        .iter()
        .find(|f| f.name == "hot" && f.file.ends_with("prog.py"))
        .expect("hot function in report");
    assert!(hot.ncalls >= 3, "ncalls = {}", hot.ncalls);
    assert_eq!(hot.line, 4);
    assert!(hot.tottime >= 0.0 && hot.cumtime >= hot.tottime);
    assert!(report.functions.iter().all(|f| f.name != "cold"));
    assert!(report.total_calls >= 3);
    assert_eq!(report.exit_code, Some(0));
    // Call graph: main → hot.
    let hot_idx = report.find(&hot.file, hot.line, "hot").unwrap();
    let callers = report.callers_of(hot_idx);
    assert!(
        callers
            .iter()
            .any(|(i, n)| report.functions[*i].name == "main" && *n >= 3),
        "{callers:?}"
    );
    let main_idx = report
        .functions
        .iter()
        .position(|f| f.name == "main" && f.file.ends_with("prog.py"))
        .unwrap();
    assert!(report
        .callees_of(main_idx)
        .iter()
        .any(|(i, _)| *i == hot_idx));
    assert_eq!(report.sorted_by_tottime().len(), report.functions.len());
    assert!(report.project_functions(dir.path()).contains(&hot_idx));
}

#[test]
fn profiler_rejects_shell_configs() {
    let cfg = RunConfig {
        kind: RunConfigKind::ShellScript {
            script: PathBuf::from("x.sh"),
        },
        ..Default::default()
    };
    let err = run_with_profiler(2, &cfg, None, Path::new("/tmp"))
        .err()
        .expect("error");
    assert!(
        err.to_string().contains("Python run configurations"),
        "{err}"
    );
}

#[test]
fn profile_json_lines_parse_and_link() {
    let text = format!(
        "noise line\n{p}{{\"file\":\"/p/a.py\",\"line\":3,\"name\":\"f\",\"ncalls\":4,\"primitive_calls\":4,\"tottime\":0.5,\"cumtime\":0.9,\"callers\":[{{\"file\":\"/p/a.py\",\"line\":10,\"name\":\"g\",\"ncalls\":4,\"primitive_calls\":4,\"tottime\":0.5,\"cumtime\":0.9}}]}}\n{p}{{\"file\":\"/p/a.py\",\"line\":10,\"name\":\"g\",\"ncalls\":1,\"primitive_calls\":1,\"tottime\":0.1,\"cumtime\":1.0,\"callers\":[]}}\n{p}{{\"event\":\"summary\",\"total_time\":0.6,\"total_calls\":5,\"elapsed\":1.2,\"exit_code\":0,\"functions\":2}}\n",
        p = PROFILE_PREFIX
    );
    let r = load_json_lines(&text).unwrap();
    assert_eq!(r.functions.len(), 2);
    assert_eq!(r.total_calls, 5);
    assert!((r.total_time - 0.6).abs() < 1e-9);
    assert_eq!(r.sorted_by_tottime()[0], 0);
    assert_eq!(r.sorted_by_cumtime()[0], 1);
    assert_eq!(r.callers_of(0), vec![(1, 4)]);
    assert_eq!(r.callees_of(1), vec![(0, 4)]);
    assert_eq!(r.functions[0].display_name(), "f  (a.py:3)");
    assert!(load_json_lines("nothing here").is_err());
}

#[test]
fn coverage_json_parses_lines_and_states() {
    let json = r#"{"meta": {"version": "7.6"}, "files": {"pkg/mod.py": {"executed_lines": [1, 2, 5], "summary": {"covered_lines": 3, "num_statements": 5, "percent_covered": 60.0, "missing_lines": 2, "excluded_lines": 1}, "missing_lines": [3, 7], "excluded_lines": [9]}, "/abs/other.py": {"executed_lines": [1], "summary": {"percent_covered": 100.0}, "missing_lines": [], "excluded_lines": []}}, "totals": {"percent_covered": 66.7}}"#;
    let r = load_json(json, Path::new("/root/proj")).unwrap();
    assert_eq!(r.files.len(), 2);
    let m = r
        .file(Path::new("/root/proj/pkg/mod.py"))
        .expect("relative path resolved");
    assert_eq!(m.executed, vec![1, 2, 5]);
    assert_eq!(m.missing, vec![3, 7]);
    assert_eq!(m.line_state(1), Some(LineCoverage::Covered));
    assert_eq!(m.line_state(3), Some(LineCoverage::Missed));
    assert_eq!(m.line_state(9), Some(LineCoverage::Excluded));
    assert_eq!(m.line_state(4), None);
    assert_eq!(m.statements(), 5);
    assert!((m.percent - 60.0).abs() < 1e-3);
    assert!(r.file(Path::new("/abs/other.py")).is_some());
    assert!((r.total_percent - 66.7).abs() < 1e-3);
    assert_eq!(r.totals(), (4, 6));
}

#[test]
fn coverage_runs_script_when_coverage_is_installed() {
    let Some(py) = python() else {
        eprintln!("python3 not found; skipping");
        return;
    };
    if !coverage_available(&py) {
        // Without an installed `coverage`, only a run that refuses the bundled
        // runtime fails; the default options below fall back to the bundle.
        eprintln!(
            "coverage not installed for {}; the end-to-end run uses the bundled runtime",
            py.display()
        );
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("prog.py");
        std::fs::write(&script, SCRIPT).unwrap();
        let options = CoverageOptions {
            bundled: false,
            ..CoverageOptions::default()
        };
        let err = run_with_coverage_opts(
            3,
            &RunConfig::for_script(&script),
            Some(&interpreter(&py)),
            dir.path(),
            &options,
        )
        .err()
        .expect("error without coverage");
        assert!(err.to_string().contains("pip install coverage"), "{err}");
    }
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("prog.py");
    std::fs::write(&script, SCRIPT).unwrap();
    let cfg = RunConfig::for_script(&script);
    let mut run =
        run_with_coverage(4, &cfg, Some(&interpreter(&py)), dir.path()).expect("coverage starts");
    assert!(
        run.command_line.contains("-m coverage run"),
        "{}",
        run.command_line
    );
    let mut output: Vec<(String, bool)> = Vec::new();
    assert!(
        wait_until(
            || {
                output.extend(run.poll());
                !run.is_running()
            },
            Duration::from_secs(90)
        ),
        "coverage run did not finish: {output:?}"
    );
    output.extend(run.poll());
    assert_eq!(run.exit_code(), Some(0), "output: {output:?}");
    assert!(run.error().is_none(), "{:?} / {output:?}", run.error());
    assert!(output.iter().any(|(l, _)| l == "args: []"), "{output:?}");
    let report = run.report().expect("coverage report");
    let canonical = dunce::canonicalize(&script).unwrap_or(script.clone());
    let f = report
        .files
        .iter()
        .find(|f| f.path == script || f.path == canonical)
        .unwrap_or_else(|| {
            panic!(
                "script missing from report: {:?}",
                report
                    .files
                    .iter()
                    .map(|f| f.path.clone())
                    .collect::<Vec<_>>()
            )
        });
    // `hot` body ran, `cold` body did not.
    assert_eq!(f.line_state(5), Some(LineCoverage::Covered));
    assert_eq!(f.line_state(12), Some(LineCoverage::Missed));
    assert!(f.percent > 50.0 && f.percent < 100.0, "{}", f.percent);
    assert!(report.total_percent > 0.0);
    // Temp data files are cleaned up.
    let leftovers: Vec<_> = std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(&format!("rustpy-coverage-{}-4", std::process::id()))
        })
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

/// Runs to the end and returns the report (panics with the output on failure).
fn finish(mut run: rustpy_run::CoverageRun) -> rustpy_run::CoverageReport {
    let mut output: Vec<(String, bool)> = Vec::new();
    assert!(
        wait_until(
            || {
                output.extend(run.poll());
                !run.is_running()
            },
            Duration::from_secs(90)
        ),
        "coverage run did not finish: {output:?}"
    );
    output.extend(run.poll());
    run.report()
        .cloned()
        .unwrap_or_else(|| panic!("no report: {:?} / {output:?}", run.error()))
}

fn file_of<'a>(
    report: &'a rustpy_run::CoverageReport,
    script: &Path,
) -> &'a rustpy_run::FileCoverage {
    let canonical = dunce::canonicalize(script).unwrap_or(script.to_path_buf());
    report
        .files
        .iter()
        .find(|f| f.path == script || f.path == canonical)
        .unwrap_or_else(|| {
            panic!(
                "{} missing from report: {:?}",
                script.display(),
                report
                    .files
                    .iter()
                    .map(|f| f.path.clone())
                    .collect::<Vec<_>>()
            )
        })
}

#[test]
fn coverage_partial_lines_kept_data_import_and_html_report() {
    let Some(py) = python() else {
        eprintln!("python3 not found; skipping");
        return;
    };
    if !coverage_available(&py) {
        eprintln!("coverage not installed for {}; skipping", py.display());
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("prog.py");
    std::fs::write(&script, SCRIPT).unwrap();
    let interp = interpreter(&py);
    let cfg = RunConfig::for_script(&script);
    let run = run_with_coverage(5, &cfg, Some(&interp), dir.path()).expect("coverage starts");
    assert!(
        run.command_line.contains("--branch"),
        "{}",
        run.command_line
    );
    let report = finish(run);
    assert!(report.branch);
    let f = file_of(&report, &script);
    // `if len(sys.argv) > 5:` only ever took one branch: partially covered.
    assert_eq!(f.line_state(19), Some(LineCoverage::Partial));
    assert_eq!(f.line_state(18), Some(LineCoverage::Covered));
    assert_eq!(f.line_state(20), Some(LineCoverage::Missed));
    // The `for` loop ran its body and left: fully covered.
    assert_eq!(f.line_state(6), Some(LineCoverage::Covered));
    assert!(
        f.num_branches > f.covered_branches && f.covered_branches > 0,
        "{} / {}",
        f.covered_branches,
        f.num_branches
    );
    assert!(f.branch_percent().unwrap() < 100.0);

    // The data file is kept in the project for the HTML report and re-import.
    let data = report.data_file.clone().expect("data file kept");
    assert_eq!(data, kept_data_file(dir.path(), "prog"));
    assert!(data.is_file());
    let imported = import_report(&data, dir.path(), &py).expect("import .coverage");
    let g = file_of(&imported, &script);
    assert_eq!(
        (&g.executed, &g.missing, &g.partial),
        (&f.executed, &f.missing, &f.partial)
    );
    assert_eq!(imported.data_file.as_deref(), Some(data.as_path()));
    let index = html_report(&py, &data, None, &dir.path().join("htmlcov"), dir.path())
        .expect("html report");
    assert!(index.ends_with("index.html") && index.is_file());
    assert!(std::fs::read_to_string(&index).unwrap().contains("prog.py"));

    // coverage.json written by coverage.py itself imports too.
    let json = dir.path().join("coverage.json");
    let status = std::process::Command::new(&py)
        .args(["-m", "coverage", "json", "-q", "-o"])
        .arg(&json)
        .arg(format!("--data-file={}", data.display()))
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    let from_json = import_report(&json, dir.path(), &py).expect("import json");
    assert_eq!(file_of(&from_json, &script).partial, f.partial);
    // Bad input is an error, not a crash.
    assert!(import_report(&dir.path().join("missing.coverage"), dir.path(), &py).is_err());
    std::fs::write(dir.path().join("junk.coverage"), "not a database").unwrap();
    assert!(import_report(&dir.path().join("junk.coverage"), dir.path(), &py).is_err());

    // Line coverage only.
    let run = run_with_coverage_opts(
        6,
        &cfg,
        Some(&interp),
        dir.path(),
        &CoverageOptions {
            branch: false,
            ..Default::default()
        },
    )
    .expect("coverage starts");
    assert!(
        !run.command_line.contains("--branch"),
        "{}",
        run.command_line
    );
    let report = finish(run);
    let f = file_of(&report, &script);
    assert!(!report.branch && f.partial.is_empty() && f.num_branches == 0);
    assert_eq!(f.line_state(19), Some(LineCoverage::Covered));
}

#[test]
fn coverage_honours_the_project_configuration() {
    let Some(py) = python() else {
        eprintln!("python3 not found; skipping");
        return;
    };
    if !coverage_available(&py) {
        eprintln!("coverage not installed for {}; skipping", py.display());
        return;
    }
    let interp = interpreter(&py);
    // A configuration in the working directory that selects the source: no --source.
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("prog.py");
    std::fs::write(&script, SCRIPT).unwrap();
    std::fs::write(dir.path().join("pyproject.toml"), "[tool.coverage.run]\nsource = [\".\"]\n\n[tool.coverage.report]\nexclude_lines = [\"never called\"]\n").unwrap();
    let run = run_with_coverage(
        7,
        &RunConfig::for_script(&script),
        Some(&interp),
        dir.path(),
    )
    .expect("coverage starts");
    assert!(
        !run.command_line.contains("--source="),
        "{}",
        run.command_line
    );
    assert!(
        !run.command_line.contains("--rcfile="),
        "{}",
        run.command_line
    );
    let report = finish(run);
    // exclude_lines from the configuration applied.
    assert_eq!(
        file_of(&report, &script).line_state(12),
        Some(LineCoverage::Excluded)
    );

    // A script run from a subdirectory: the root's .coveragerc is passed explicitly
    // and the project is still measured as a whole.
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("app");
    std::fs::create_dir_all(&app).unwrap();
    let script = app.join("prog.py");
    std::fs::write(&script, SCRIPT).unwrap();
    std::fs::write(
        dir.path().join(".coveragerc"),
        "[run]\nsource = app\n\n[report]\nexclude_lines =\n    never called\n",
    )
    .unwrap();
    let cfg = RunConfig::for_script(&script);
    assert_eq!(cfg.working_dir.as_deref(), Some(app.as_path()));
    let run = run_with_coverage(8, &cfg, Some(&interp), dir.path()).expect("coverage starts");
    assert!(
        run.command_line.contains(&format!(
            "--rcfile={}",
            dir.path().join(".coveragerc").display()
        )),
        "{}",
        run.command_line
    );
    assert!(
        run.command_line
            .contains(&format!("--source={}", dir.path().display())),
        "{}",
        run.command_line
    );
    let report = finish(run);
    assert_eq!(
        file_of(&report, &script).line_state(12),
        Some(LineCoverage::Excluded)
    );
    assert_eq!(
        report.rcfile.as_deref(),
        Some(dir.path().join(".coveragerc").as_path())
    );
}

/// "Redirect input from" applies to the profiler and coverage runs too.
#[test]
fn measured_runs_read_the_redirected_input() {
    let Some(py) = python() else {
        eprintln!("python3 not found; skipping");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("echo_input.py");
    std::fs::write(
        &script,
        "import sys\nprint('input:', sys.stdin.read().strip())\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("input.txt"), "from the file\n").unwrap();
    let mut cfg = RunConfig::for_script(&script);
    cfg.redirect_input = Some(PathBuf::from("input.txt"));
    let interp = interpreter(&py);
    type Poll = Box<dyn FnMut() -> (Vec<(String, bool)>, bool)>;
    let mut runs: Vec<(&str, Poll)> = Vec::new();
    let mut profile =
        run_with_profiler(21, &cfg, Some(&interp), dir.path()).expect("profiler starts");
    runs.push((
        "profiler",
        Box::new(move || (profile.poll(), profile.is_running())),
    ));
    if coverage_available(&py) {
        let mut coverage =
            run_with_coverage(22, &cfg, Some(&interp), dir.path()).expect("coverage starts");
        runs.push((
            "coverage",
            Box::new(move || (coverage.poll(), coverage.is_running())),
        ));
    }
    for (name, mut poll) in runs {
        let mut output: Vec<(String, bool)> = Vec::new();
        let finished = wait_until(
            || {
                let (lines, running) = poll();
                output.extend(lines);
                !running
            },
            Duration::from_secs(90),
        );
        assert!(finished, "{name} run did not finish: {output:?}");
        output.extend(poll().0);
        assert!(
            output
                .iter()
                .any(|(l, err)| !*err && l == "input: from the file"),
            "{name}: {output:?}"
        );
    }
}

/// Actual optional-provider acceptance, never a protocol replay or silent skip.
fn configured_yappi(clock: &str, id: u64) {
    assert!(
        required_providers(),
        "explicitly set RUSTPY_REQUIRE_PROVIDER_TESTS=1"
    );
    let py = python().expect("required provider Python");
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("thread_profile.py");
    std::fs::write(&script, "import threading, time\ndef hot(n):\n    total = 0\n    for i in range(n):\n        total += i * i\n    return total\ndef worker():\n    for _ in range(3):\n        hot(4000)\n    time.sleep(0.01)\ndef main():\n    thread = threading.Thread(target=worker, name='rustpy-owned-profile-worker')\n    thread.start()\n    for _ in range(3):\n        hot(4000)\n    thread.join()\n    print('YAPPI_THREADS_FINISHED')\nmain()\n").unwrap();
    let cfg = RunConfig::for_script(&script);
    let interp = interpreter(&py);
    let options = rustpy_run::profile::ProfileOptions {
        engine: "yappi".into(),
        clock: clock.into(),
        threads: true,
        builtins: true,
    };
    let mut run =
        rustpy_run::profile::run_with_profiler_opts(id, &cfg, Some(&interp), dir.path(), &options)
            .expect("actual yappi runner starts");
    let mut output = Vec::new();
    assert!(
        wait_until(
            || {
                output.extend(run.poll());
                !run.is_running()
            },
            Duration::from_secs(60)
        ),
        "owned yappi run did not retire: {output:?}"
    );
    output.extend(run.poll());
    assert_eq!(
        run.exit_code(),
        Some(0),
        "required yappi provider run failed: {output:?}"
    );
    assert!(run.error().is_none(), "{:?}: {output:?}", run.error());
    assert!(output
        .iter()
        .any(|(line, error)| !error && line == "YAPPI_THREADS_FINISHED"));
    assert!(!output
        .iter()
        .any(|(line, _)| line.starts_with(PROFILE_PREFIX.trim_end())));
    let report = run.report().expect("actual yappi provider report");
    assert_eq!(report.engine, "yappi");
    assert!(!report.sampling);
    assert_eq!(report.exit_code, Some(0));
    let hot = report
        .functions
        .iter()
        .find(|function| function.name == "hot" && function.file.ends_with("thread_profile.py"))
        .expect("threaded hot function collected");
    assert_eq!(
        hot.ncalls, 6,
        "calls from both actual Python threads must be collected"
    );
    assert!(hot.tottime >= 0.0 && hot.cumtime >= hot.tottime);
    let index = report.find(&hot.file, hot.line, &hot.name).unwrap();
    let callers = report.callers_of(index);
    for name in ["main", "worker"] {
        assert!(
            callers
                .iter()
                .any(|(caller, calls)| report.functions[*caller].name == name && *calls == 3),
            "real {name} -> hot call graph edge missing: {callers:?}"
        );
    }
    let mut thread_ids: Vec<_> = report.threads.iter().map(|thread| thread.id).collect();
    thread_ids.sort_unstable();
    thread_ids.dedup();
    assert!(
        thread_ids.len() >= 2,
        "main and worker thread statistics missing: {:?}",
        report.threads
    );
    assert!(report
        .threads
        .iter()
        .all(|thread| thread.total_time >= 0.0 && thread.schedules > 0));
    let pstat = report
        .pstat_file
        .as_ref()
        .expect("actual yappi pstat artifact");
    assert!(std::fs::metadata(pstat).unwrap().len() > 0);
    let imported = rustpy_run::profile_tools::import_report(pstat, &py, dir.path())
        .expect("public pstat import consumes the actual provider artifact");
    assert!(imported
        .functions
        .iter()
        .any(|function| function.name == "hot" && function.ncalls == 6));
    eprintln!(
        "REQUIRED_PROVIDER yappi clock={clock} python={} functions={} threads={} pstat={}",
        py.display(),
        report.functions.len(),
        thread_ids.len(),
        pstat.display()
    );
}

#[test]
#[ignore = "requires explicitly admitted Python with installed yappi; set required provider mode"]
fn configured_yappi_wall_clock_threads_and_pstat_roundtrip() {
    configured_yappi("wall", 71001);
}

#[test]
#[ignore = "requires explicitly admitted Python with installed yappi; set required provider mode"]
fn configured_yappi_cpu_clock_threads_and_pstat_roundtrip() {
    configured_yappi("cpu", 71002);
}
