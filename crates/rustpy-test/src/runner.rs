//! Framework detection, the test process, and failure-location parsing.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::behave::BehaveRun;
use crate::trial::TrialRun;

const REPORTER: &str = include_str!("reporter.py");
const PREFIX: &str = "##rustpy-test## ";

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Framework {
    Pytest,
    Unittest,
    Doctest,
    Nose,
    Nose2,
    Lettuce,
    Tox,
    /// behave (BDD): `python -m behave` with a JSON report; features are
    /// suites, scenarios (and outline rows) tests.
    Behave,
    /// Twisted Trial: `python -m twisted.trial --reporter=timing`.
    Trial,
}

impl Framework {
    pub fn label(self) -> &'static str {
        match self {
            Framework::Pytest => "pytest",
            Framework::Unittest => "unittest",
            Framework::Doctest => "doctest",
            Framework::Nose => "nose",
            Framework::Nose2 => "nose2",
            Framework::Lettuce => "lettuce",
            Framework::Tox => "tox",
            Framework::Behave => "behave",
            Framework::Trial => "trial",
        }
    }
}

/// True if `import pytest` succeeds in the interpreter (probe with a timeout).
pub fn pytest_available(interpreter: &Path) -> bool {
    run_ok(
        interpreter,
        &["-c", "import pytest"],
        Duration::from_secs(15),
    )
}

/// True if `import behave` succeeds in the interpreter (probe with a timeout).
pub fn behave_available(interpreter: &Path) -> bool {
    run_ok(
        interpreter,
        &["-c", "import behave"],
        Duration::from_secs(15),
    )
}

/// True if `import twisted.trial` succeeds in the interpreter (probe with a timeout).
pub fn trial_available(interpreter: &Path) -> bool {
    run_ok(
        interpreter,
        &["-c", "import twisted.trial.unittest"],
        Duration::from_secs(15),
    )
}

fn run_ok(program: &Path, args: &[&str], timeout: Duration) -> bool {
    let Ok(mut child) = rustpy_core::interpreter_launch::interpreter_command(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return false,
        }
    }
}

pub(crate) fn file_contains(path: &Path, needle: &str) -> bool {
    std::fs::read_to_string(path)
        .map(|s| s.contains(needle))
        .unwrap_or(false)
}

/// Structured INI header evidence: comments, values and continuations cannot
/// impersonate a configuration section. Option diagnostics do not erase a real
/// framework declaration; launching the framework owns configuration errors.
pub(crate) fn file_has_ini_section(path: &Path, expected: &str) -> bool {
    let Ok(source) = std::fs::read_to_string(path) else {
        return false;
    };
    let parsed = rustpy_syntax::ini::parse(&source, rustpy_syntax::ini::Dialect::ConfigParser);
    parsed
        .sections
        .iter()
        .any(|section| &source[section.name.clone()] == expected)
}

/// Config-file evidence that the project is set up for pytest.
pub fn project_uses_pytest(root: &Path) -> bool {
    crate::PytestConfiguration::load(root).path.is_some() || root.join("conftest.py").is_file()
}

impl Framework {
    pub const ALL: [Self; 9] = [
        Self::Pytest,
        Self::Unittest,
        Self::Doctest,
        Self::Nose,
        Self::Nose2,
        Self::Trial,
        Self::Tox,
        Self::Behave,
        Self::Lettuce,
    ];
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|framework| framework.label().eq_ignore_ascii_case(key))
    }
}
/// Pure configured preference; automatic probing remains the caller's background job.
pub fn configured_framework(root: &Path, preference: &str) -> Option<Framework> {
    Framework::from_key(preference).or_else(|| {
        if project_uses_pytest(root) {
            Some(Framework::Pytest)
        } else if root.join("nose2.cfg").is_file() || root.join("unittest.cfg").is_file() {
            Some(Framework::Nose2)
        } else if file_has_ini_section(&root.join("setup.cfg"), "nosetests") {
            Some(Framework::Nose)
        } else {
            None
        }
    })
}

/// pytest if the project is configured for it or the interpreter can import
/// it (falls back to `python3`/`python` on PATH when no interpreter is given);
/// otherwise unittest.
pub fn detect_framework(root: &Path, interpreter: Option<&Path>) -> Framework {
    if let Some(framework) = configured_framework(root, "auto") {
        return framework;
    }
    if project_uses_pytest(root) {
        return Framework::Pytest;
    }
    let python = interpreter
        .map(Path::to_path_buf)
        .or_else(|| which::which("python3").ok())
        .or_else(|| which::which("python").ok());
    match python {
        Some(p) if pytest_available(&p) => Framework::Pytest,
        _ => Framework::Unittest,
    }
}

/// True when the project has Python test modules and every one of them uses
/// Twisted Trial (`twisted.trial` imports).
pub fn project_uses_trial(root: &Path) -> bool {
    all_use_trial(&crate::discover_project(root))
}

fn all_use_trial(modules: &[crate::TestItem]) -> bool {
    !modules.is_empty()
        && modules
            .iter()
            .all(|m| file_contains(&m.path, "twisted.trial"))
}

/// The framework to run `targets` (as for [`TestRun::start`]) with:
///
/// * [`Framework::Behave`] for `.feature` files / feature ids / `file:line`
///   locations, for directories that contain feature files but no Python
///   tests, and for whole-project runs of behave projects without Python tests;
/// * [`Framework::Trial`] when the Python test files to run (all test
///   modules of the project for no targets) use `twisted.trial` and the
///   project is not configured for pytest;
/// * otherwise [`detect_framework`].
pub fn detect_framework_for(
    root: &Path,
    targets: &[String],
    interpreter: Option<&Path>,
) -> Framework {
    let file_of = |t: &str| -> PathBuf {
        let mut f = t.split("::").next().unwrap_or(t);
        if let Some((path, line)) = f.rsplit_once(':') {
            if !line.is_empty() && line.chars().all(|c| c.is_ascii_digit()) && path.len() > 1 {
                f = path;
            }
        }
        let p = Path::new(f);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(p)
        }
    };
    if targets.is_empty() {
        let modules = crate::discover_project(root);
        if modules.is_empty()
            && (crate::behave::project_uses_behave(root)
                || !crate::discover_features(root).is_empty())
        {
            return Framework::Behave;
        }
        if !project_uses_pytest(root) && all_use_trial(&modules) {
            return Framework::Trial;
        }
        return detect_framework(root, interpreter);
    }
    let files: Vec<PathBuf> = targets.iter().map(|t| file_of(t)).collect();
    let behave_target = |f: &PathBuf| {
        crate::behave::is_feature_file(f)
            || (f.is_dir()
                && crate::discover_project(f).is_empty()
                && !crate::discover_features(f).is_empty())
    };
    if files.iter().any(behave_target) {
        return Framework::Behave;
    }
    let python_files: Vec<PathBuf> = files
        .iter()
        .flat_map(|f| {
            if f.is_dir() {
                crate::discover_project(f)
                    .into_iter()
                    .map(|m| m.path)
                    .collect()
            } else {
                vec![f.clone()]
            }
        })
        .filter(|f| f.extension().is_some_and(|e| e == "py"))
        .collect();
    if !python_files.is_empty()
        && !project_uses_pytest(root)
        && python_files
            .iter()
            .all(|f| file_contains(f, "twisted.trial"))
    {
        return Framework::Trial;
    }
    detect_framework(root, interpreter)
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Passed,
    Failed,
    Skipped,
    Error,
}

impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Passed => "passed",
            Outcome::Failed => "failed",
            Outcome::Skipped => "skipped",
            Outcome::Error => "error",
        }
    }
    pub fn is_problem(self) -> bool {
        matches!(self, Outcome::Failed | Outcome::Error)
    }
}

/// The result of one test.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TestResult {
    pub id: String,
    pub outcome: Outcome,
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub traceback: String,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    /// pytest xfail/xpass marker.
    #[serde(default)]
    pub xfail: bool,
    /// For a failed equality assertion (`assert actual == expected`,
    /// `assertEqual(expected, actual)`): the expected value, pretty-printed
    /// (strings verbatim). Set together with [`TestResult::actual`].
    #[serde(default)]
    pub expected: Option<String>,
    /// The actual value of a failed equality assertion (see [`TestResult::expected`]).
    #[serde(default)]
    pub actual: Option<String>,
}

impl TestResult {
    /// `(expected, actual)` when the failure was an equality comparison
    /// whose values can be shown side by side.
    pub fn comparison(&self) -> Option<(&str, &str)> {
        Some((self.expected.as_deref()?, self.actual.as_deref()?))
    }
}

/// Puts the run-time type collector in front of a test runner command line
/// (`-u <runner> …` or `-u -m <module> …`): the collector becomes the script
/// and runs the reporter, debug bootstrap or runner module in-process, so the
/// reporter plugins see the same process and `sys.setprofile` chain.
fn runtime_types_args(args: &[String], collector: &Path, project_root: &Path) -> Vec<String> {
    let options = usize::from(args.first().is_some_and(|a| a == "-u"));
    rustpy_run::typecollect::wrap_args(
        args,
        options,
        collector,
        &rustpy_run::typecollect::output_path(project_root),
        project_root,
    )
}

/// Process options of a test run, usually taken from a run configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TestRunOptions {
    /// False starts from an empty process environment before explicit options.
    pub inherit_environment: bool,
    /// Environment variables applied over the defaults. A `PYTHONPATH` here
    /// replaces the default (project root + inherited `PYTHONPATH`); runner
    /// directories that Twisted Trial needs are still put in front of it.
    pub env: Vec<(String, String)>,
    /// Working directory of the test process (pytest and unittest; relative
    /// paths are resolved against the project root). Node ids stay relative
    /// to the project root. behave and Trial always run in the project root.
    pub cwd: Option<PathBuf>,
    /// Interpreter options placed before the runner (`-X dev`, `-W error`).
    pub interpreter_options: Vec<String>,
    /// Runs the test process under `typecollect.py` ("Collect run-time types
    /// information for code insight"), merging into the project's
    /// `.rustpy/runtime_types.json` like a collected script run.
    pub collect_runtime_types: bool,
}

impl Default for TestRunOptions {
    fn default() -> Self {
        Self {
            inherit_environment: true,
            env: Vec::new(),
            cwd: None,
            interpreter_options: Vec::new(),
            collect_runtime_types: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    #[serde(default)]
    pub passed: usize,
    #[serde(default)]
    pub failed: usize,
    #[serde(default)]
    pub skipped: usize,
    #[serde(default)]
    pub errors: usize,
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub exit_code: Option<i32>,
}

impl Summary {
    pub fn total(&self) -> usize {
        self.passed + self.failed + self.skipped + self.errors
    }
    /// Whether the run passed: no failures or errors, at least one test ran
    /// ("No tests were found" is not a pass) and the runner exited
    /// cleanly. pytest reports a target it cannot find ("ERROR: not found",
    /// exit code 4) or a run that collected nothing (exit code 5) with zero
    /// failures, so the counts alone would show such a run as passed.
    pub fn is_success(&self) -> bool {
        self.failed == 0
            && self.errors == 0
            && self.total() > 0
            && self.exit_code.is_none_or(|c| c == 0)
    }
    /// `3 passed, 1 failed, 1 skipped in 0.12s`
    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        if self.passed > 0 {
            parts.push(format!("{} passed", self.passed));
        }
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        if self.errors > 0 {
            parts.push(format!(
                "{} error{}",
                self.errors,
                if self.errors == 1 { "" } else { "s" }
            ));
        }
        if self.skipped > 0 {
            parts.push(format!("{} skipped", self.skipped));
        }
        if parts.is_empty() {
            parts.push("no tests ran".into());
        }
        format!("{} in {:.2}s", parts.join(", "), self.duration)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TestMetadata {
    pub id: String,
    pub markers: Vec<String>,
    pub fixtures: Vec<(String, PathBuf, usize)>,
    pub logs: String,
    pub parameter_id: Option<String>,
    pub worker: Option<String>,
    pub environment: Option<String>,
    pub hypothesis: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TestEvent {
    Metadata(TestMetadata),
    Debugger {
        host: String,
        port: u16,
    },
    /// A test was collected (before it runs). `path` is absolute when known.
    Collected {
        id: String,
        name: String,
        path: Option<PathBuf>,
        line: Option<usize>,
    },
    Started(String),
    Result(TestResult),
    /// A raw output line (`is_stderr`), without the trailing newline.
    Output(String, bool),
    Done(Summary),
    /// The run could not be performed (e.g. pytest missing); the process ends afterwards.
    Failed(String),
    /// The process has exited with this code; no more events follow.
    Exited(i32),
}

enum Raw {
    Line { text: String, is_stderr: bool },
    Eof,
    Exited(i32),
}

/// How the output of the test process is turned into events.
enum Mode {
    /// pytest / unittest through the embedded JSON-lines reporter.
    Reporter,
    /// behave: console lines for progress, the JSON report for results.
    Behave(Box<BehaveRun>),
    /// Twisted Trial: the `timing` reporter's console output.
    Trial(Box<TrialRun>),
}

/// A running (or finished) test process.
pub struct TestRun {
    pub id: u64,
    pub framework: Framework,
    pub command_line: String,
    pub project_root: PathBuf,
    pub started: Instant,
    interpreter: PathBuf,
    rx: Receiver<Raw>,
    child: Option<Arc<Mutex<Child>>>,
    tree: Arc<rustpy_run::ProcessTree>,
    mode: Mode,
    /// Events produced before the process printed anything (static collection).
    pending: Vec<TestEvent>,
    eofs: u8,
    exit_code: Option<i32>,
    finished: bool,
    done_seen: bool,
    failed_seen: bool,
    _reporter_dir: tempfile::TempDir,
    debug_events: Option<crate::debug_events::DebugEvents>,
}

fn relative_target(root: &Path, target: &str) -> String {
    let (file_part, rest) = match target.split_once("::") {
        Some((f, r)) => (f, Some(r)),
        None => (target, None),
    };
    let p = Path::new(file_part);
    let file_part = if p.is_absolute() {
        p.strip_prefix(root)
            .map(|r| r.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| file_part.to_string())
    } else {
        file_part.to_string()
    };
    match rest {
        Some(r) => format!("{file_part}::{r}"),
        None => file_part,
    }
}

/// The inverse of [`relative_target`] for targets whose file exists under `root`.
fn absolute_target(root: &Path, target: &str) -> String {
    let file_part = target.split("::").next().unwrap_or(target);
    let p = Path::new(file_part);
    if !file_part.is_empty() && !p.is_absolute() && root.join(p).exists() {
        root.join(target).to_string_lossy().into_owned()
    } else {
        target.to_string()
    }
}

/// Whether unittest arguments ask for unittest's own discovery, as
/// `python -m unittest discover` takes them: the word `discover` or a start
/// directory, file pattern or top-level directory option (`-s`, `-p`, `-t`).
fn unittest_discovery_requested(args: &[String]) -> bool {
    let mut args = args.iter();
    while let Some(a) = args.next() {
        let name = a.split('=').next().unwrap_or(a);
        if a == "discover"
            || matches!(
                name,
                "--start-directory" | "--pattern" | "--top-level-directory"
            )
            || ["-s", "-p", "-t"].iter().any(|o| a.starts_with(o))
        {
            return true;
        }
        if a == "-k" || a == "--durations" {
            args.next(); // the option's value
        }
    }
    false
}

impl TestRun {
    /// Starts a test run. `targets` are node ids (`tests/test_x.py::TestA::test_b`),
    /// file or directory paths (absolute or relative to `project_root`), or for
    /// unittest / trial dotted names; empty means the whole project. For
    /// behave, targets are `.feature` files, directories, feature / scenario
    /// ids (`features/x.feature::Feature::Scenario`) or `file:line` locations.
    /// `extra_args` are passed to pytest (any option), unittest (`-k pattern`,
    /// `-f`, and the discovery arguments of `python -m unittest discover`:
    /// `-s`/`-p`/`-t` or positional values after `discover`; with no targets
    /// they make unittest discover the tests itself, a pattern also filters
    /// the files of directory targets), behave (any option; `-k EXPR` becomes
    /// `--name EXPR`) or trial (any option; `-k EXPR` selects the matching
    /// tests statically).
    /// `env` is applied over the default environment (see [`TestRunOptions::env`]).
    pub fn start(
        id: u64,
        framework: Framework,
        interpreter: &Path,
        project_root: &Path,
        targets: &[String],
        extra_args: &[String],
        env: &[(String, String)],
    ) -> anyhow::Result<TestRun> {
        let options = TestRunOptions {
            env: env.to_vec(),
            ..Default::default()
        };
        Self::start_with(
            id,
            framework,
            interpreter,
            project_root,
            targets,
            extra_args,
            &options,
        )
    }

    /// [`TestRun::start`] with process options (environment, working
    /// directory, interpreter options) from a run configuration.
    pub fn start_with(
        id: u64,
        framework: Framework,
        interpreter: &Path,
        project_root: &Path,
        targets: &[String],
        extra_args: &[String],
        options: &TestRunOptions,
    ) -> anyhow::Result<TestRun> {
        let reporter_dir = tempfile::Builder::new()
            .prefix("rustpy-test-")
            .tempdir()
            .context("cannot create temp dir for the test reporter")?;
        let mut targets: Vec<String> = targets
            .iter()
            .map(|t| relative_target(project_root, t))
            .collect();
        let mut extra_pythonpath: Vec<PathBuf> = Vec::new();
        let reporter_cwd = match framework {
            Framework::Pytest
            | Framework::Unittest
            | Framework::Doctest
            | Framework::Nose
            | Framework::Nose2
            | Framework::Lettuce
            | Framework::Tox => options
                .cwd
                .as_ref()
                .filter(|d| !d.as_os_str().is_empty())
                .map(|d| {
                    if d.is_absolute() {
                        d.clone()
                    } else {
                        project_root.join(d)
                    }
                })
                .filter(|d| d != project_root),
            Framework::Behave | Framework::Trial => None,
        };
        let (args, mode, pending) = match framework {
            Framework::Pytest
            | Framework::Unittest
            | Framework::Doctest
            | Framework::Nose
            | Framework::Nose2
            | Framework::Lettuce
            | Framework::Tox => {
                let script = provision_reporter(reporter_dir.path())
                    .context("cannot provision the selected framework reporter")?;
                if targets.is_empty()
                    && framework == Framework::Unittest
                    && !unittest_discovery_requested(extra_args)
                {
                    // unittest's own discovery skips directories without __init__.py; use ours
                    // (unless the arguments ask for unittest's discovery: `-s tests -p "*_test.py"`).
                    targets = crate::discover_project(project_root)
                        .into_iter()
                        .map(|m| m.id)
                        .collect();
                }
                if reporter_cwd.is_some() && framework == Framework::Pytest {
                    // pytest resolves paths against the working directory; node ids stay root-relative.
                    targets = targets
                        .into_iter()
                        .map(|t| absolute_target(project_root, &t))
                        .collect();
                }
                let mut args: Vec<String> = vec![
                    "-u".into(),
                    script.to_string_lossy().into_owned(),
                    "--framework".into(),
                    framework.label().into(),
                    "--root".into(),
                    project_root.to_string_lossy().into_owned(),
                ];
                if let Some(cwd) = &reporter_cwd {
                    args.extend(["--cwd".to_string(), cwd.to_string_lossy().into_owned()]);
                }
                args.push("--".into());
                if framework == Framework::Unittest {
                    // Targets first: values after a `discover` word in the
                    // arguments are its start directory, pattern and top level.
                    args.extend(targets);
                    args.extend(extra_args.iter().cloned());
                } else {
                    args.extend(extra_args.iter().cloned());
                    if framework == Framework::Tox && !targets.is_empty() {
                        args.extend(["-e".into(), targets.join(",")]);
                    } else {
                        args.extend(targets);
                    }
                }
                (args, Mode::Reporter, Vec::new())
            }
            Framework::Behave => {
                let report = reporter_dir.path().join(crate::behave::BEHAVE_REPORT);
                let plan = crate::behave::plan(project_root, &targets, extra_args, &report);
                let (run, collected) =
                    BehaveRun::new(project_root, report, &plan.items, plan.narrowed);
                let mut args = vec!["-u".to_string()];
                args.extend(plan.args);
                (args, Mode::Behave(Box::new(run)), collected)
            }
            Framework::Trial => {
                let plan = crate::trial::plan(project_root, &targets, extra_args);
                extra_pythonpath = plan.pythonpath.clone();
                let mut args = vec!["-u".to_string()];
                args.extend(plan.args.iter().cloned());
                let (run, collected) = TrialRun::new(plan);
                (args, Mode::Trial(Box::new(run)), collected)
            }
        };

        if let Some(cwd) = reporter_cwd.as_ref().filter(|d| !d.is_dir()) {
            anyhow::bail!("The working directory {} does not exist", cwd.display());
        }
        let debug_enabled = options
            .env
            .iter()
            .rev()
            .find(|(name, _)| name == "RUSTPY_DEBUG_FAILED_TESTS")
            .is_some_and(|(_, value)| value == "1");
        let debug_events = if debug_enabled {
            Some(crate::debug_events::DebugEvents::new()?)
        } else {
            None
        };
        let args = if debug_enabled {
            provision_debugger_support(reporter_dir.path())?;
            let mut wrapped = vec![
                "-u".to_owned(),
                reporter_dir
                    .path()
                    .join("rustpy_debug_test_bootstrap.py")
                    .to_string_lossy()
                    .into_owned(),
                framework.label().into(),
            ];
            wrapped.extend(args.into_iter().skip(1));
            wrapped
        } else {
            args
        };
        let args = if options.collect_runtime_types {
            let collector = reporter_dir.path().join("rustpy_typecollect.py");
            std::fs::write(&collector, rustpy_run::typecollect::COLLECTOR_SOURCE)
                .context("cannot provision the run-time type collector")?;
            runtime_types_args(&args, &collector, project_root)
        } else {
            args
        };
        // Interpreter options go before `-u <runner>`.
        let args: Vec<String> = options
            .interpreter_options
            .iter()
            .cloned()
            .chain(args)
            .collect();

        let mut envs: BTreeMap<String, String> = BTreeMap::new();
        envs.insert("PYTHONUNBUFFERED".into(), "1".into());
        envs.insert("PYTHONIOENCODING".into(), "utf-8".into());
        let sep = if cfg!(windows) { ';' } else { ':' };
        let base_pythonpath = match options.env.iter().rev().find(|(k, _)| k == "PYTHONPATH") {
            Some((_, v)) => v.clone(),
            None => {
                let mut pp = project_root.to_string_lossy().into_owned();
                if let Some(existing) = options
                    .inherit_environment
                    .then(|| std::env::var("PYTHONPATH").ok())
                    .flatten()
                {
                    if !existing.is_empty() {
                        pp.push(sep);
                        pp.push_str(&existing);
                    }
                }
                pp
            }
        };
        let pp: Vec<String> = extra_pythonpath
            .iter()
            .map(|d| d.to_string_lossy().into_owned())
            .chain(std::iter::once(base_pythonpath))
            .filter(|p| !p.is_empty())
            .collect();
        envs.insert("PYTHONPATH".into(), pp.join(&sep.to_string()));
        for (k, v) in &options.env {
            if k != "PYTHONPATH" {
                envs.insert(k.clone(), v.clone());
            }
        }
        if let Some(debug) = &debug_events {
            envs.insert("RUSTPY_DEBUG_TEST_CALLBACK".into(), debug.address.clone());
            envs.insert("RUSTPY_DEBUG_TEST_TOKEN".into(), debug.token.clone());
        }
        let command_line = std::iter::once(interpreter.to_string_lossy().into_owned())
            .chain(args.iter().cloned())
            .map(|a| {
                if a.contains(' ') {
                    format!("\"{a}\"")
                } else {
                    a
                }
            })
            .collect::<Vec<_>>()
            .join(" ");

        let mut cmd = rustpy_core::interpreter_launch::interpreter_command(interpreter);
        if !options.inherit_environment {
            cmd.env_clear();
        }
        cmd.args(&args)
            .current_dir(reporter_cwd.as_deref().unwrap_or(project_root))
            .envs(&envs)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        rustpy_run::ProcessTree::prepare(&mut cmd);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("Cannot run program \"{}\"", interpreter.display()))?;
        let tree = match rustpy_run::ProcessTree::new(&mut child) {
            Ok(tree) => Arc::new(tree),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error).context("Cannot own test process tree");
            }
        };
        let (tx, rx) = channel();
        if let Some(out) = child.stdout.take() {
            spawn_reader(out, false, tx.clone());
        } else {
            let _ = tx.send(Raw::Eof);
        }
        if let Some(err) = child.stderr.take() {
            spawn_reader(err, true, tx.clone());
        } else {
            let _ = tx.send(Raw::Eof);
        }
        let child = Arc::new(Mutex::new(child));
        let watcher = child.clone();
        let watcher_tree = tree.clone();
        std::thread::spawn(move || {
            let status = loop {
                let r = watcher.lock().map(|mut c| watcher_tree.poll(&mut c));
                match r {
                    Ok(Ok(Some(s))) => break s,
                    Ok(Ok(None)) => std::thread::sleep(Duration::from_millis(30)),
                    Ok(Err(_)) => {
                        if let Ok(mut child) = watcher.lock() {
                            watcher_tree.force(&mut child);
                        }
                        let _ = tx.send(Raw::Exited(-1));
                        return;
                    }
                    Err(_) => return,
                }
            };
            let code = status.code().unwrap_or(-1);
            let _ = tx.send(Raw::Exited(code));
        });
        Ok(TestRun {
            id,
            framework,
            command_line,
            project_root: project_root.to_path_buf(),
            started: Instant::now(),
            interpreter: interpreter.to_path_buf(),
            rx,
            child: Some(child),
            tree,
            mode,
            pending,
            eofs: 0,
            exit_code: None,
            finished: false,
            done_seen: false,
            failed_seen: false,
            _reporter_dir: reporter_dir,
            debug_events,
        })
    }

    /// Drains pending events (non-blocking). After [`TestEvent::Exited`] has
    /// been returned nothing more follows.
    pub fn poll(&mut self) -> Vec<TestEvent> {
        let mut events = std::mem::take(&mut self.pending);
        if let Some(debug) = &self.debug_events {
            events.extend(
                debug
                    .poll()
                    .into_iter()
                    .map(|(host, port)| TestEvent::Debugger { host, port }),
            );
        }
        if self.finished {
            return events;
        }
        while let Ok(raw) = self.rx.try_recv() {
            match raw {
                Raw::Line { text, is_stderr } => match &mut self.mode {
                    Mode::Reporter => {
                        reporter_line(text, is_stderr, &self.project_root, &mut events)
                    }
                    Mode::Behave(run) => {
                        if is_stderr {
                            run.stderr_line(&text);
                            events.push(TestEvent::Output(text, true));
                        } else {
                            let derived = run.stdout_line(&text);
                            events.push(TestEvent::Output(text, false));
                            events.extend(derived);
                        }
                    }
                    Mode::Trial(run) => {
                        if is_stderr {
                            run.stderr_line(&text);
                            events.push(TestEvent::Output(text, true));
                        } else {
                            let derived = run.stdout_line(&text);
                            events.push(TestEvent::Output(text, false));
                            events.extend(derived);
                        }
                    }
                },
                Raw::Eof => self.eofs += 1,
                Raw::Exited(code) => self.exit_code = Some(code),
            }
        }
        if let Mode::Behave(run) = &mut self.mode {
            events.extend(run.poll_report(false));
        }
        if let Some(code) = self.exit_code {
            if self.eofs >= 2 {
                self.finished = true;
                match &mut self.mode {
                    Mode::Reporter => {}
                    Mode::Behave(run) => events.extend(run.finish(code, &self.interpreter)),
                    Mode::Trial(run) => events.extend(run.finish(code, &self.interpreter)),
                }
                self.note_terminal_events(&events);
                if !self.done_seen && !self.failed_seen {
                    events.push(TestEvent::Failed(format!(
                        "The test process exited with code {code} before reporting results"
                    )));
                }
                events.push(TestEvent::Exited(code));
                return events;
            }
        }
        self.note_terminal_events(&events);
        events
    }

    fn note_terminal_events(&mut self, events: &[TestEvent]) {
        for e in events {
            match e {
                TestEvent::Done(_) => self.done_seen = true,
                TestEvent::Failed(_) => self.failed_seen = true,
                _ => {}
            }
        }
    }

    /// True until the process has exited and all output was drained.
    pub fn is_running(&self) -> bool {
        !self.finished
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Terminates and reaps the owned group/job, including descendants. The
    /// reader threads still deliver buffered output and the terminal event.
    pub fn stop(&mut self) {
        if let Some(child) = &self.child {
            if let Ok(mut c) = child.lock() {
                let _ = self.tree.interrupt(&mut c);
                std::thread::sleep(Duration::from_millis(150));
                self.tree.force(&mut c);
            }
        }
    }
}

impl Drop for TestRun {
    fn drop(&mut self) {
        if !self.finished {
            self.stop();
        }
    }
}

/// One output line of the embedded reporter (pytest / unittest): JSON events
/// behind [`PREFIX`], anything else is program output.
fn reporter_line(text: String, is_stderr: bool, project_root: &Path, events: &mut Vec<TestEvent>) {
    if !is_stderr {
        if let Some(idx) = text.find(PREFIX) {
            let before = &text[..idx];
            if !before.trim().is_empty() {
                events.push(TestEvent::Output(before.to_string(), false));
            }
            match parse_reporter_event(&text[idx + PREFIX.len()..], project_root) {
                Some(ev) => events.push(ev),
                None => events.push(TestEvent::Output(text, false)),
            }
            return;
        }
    }
    events.push(TestEvent::Output(text, is_stderr));
}

fn parse_reporter_event(json: &str, project_root: &Path) -> Option<TestEvent> {
    let v: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    let kind = v.get("event")?.as_str()?;
    let id = || {
        v.get("id")
            .and_then(|i| i.as_str())
            .unwrap_or("")
            .to_string()
    };
    match kind {
        "debugger" => Some(TestEvent::Debugger {
            host: v["host"].as_str()?.into(),
            port: u16::try_from(v["port"].as_u64()?).ok()?,
        }),
        "collected" => {
            let name = v
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let path = v.get("file").and_then(|f| f.as_str()).map(|f| {
                let p = Path::new(f);
                if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    project_root.join(p)
                }
            });
            let line = v.get("line").and_then(|l| l.as_u64()).map(|l| l as usize);
            Some(TestEvent::Collected {
                id: id(),
                name,
                path,
                line,
            })
        }
        "metadata" => serde_json::from_value::<TestMetadata>(v)
            .ok()
            .map(|mut metadata| {
                for (_, path, _) in &mut metadata.fixtures {
                    if path.is_relative() {
                        *path = project_root.join(&*path);
                    }
                }
                TestEvent::Metadata(metadata)
            }),
        "start" => Some(TestEvent::Started(id())),
        "result" => serde_json::from_value::<TestResult>(v)
            .ok()
            .map(TestEvent::Result),
        "done" => serde_json::from_value::<Summary>(v)
            .ok()
            .map(TestEvent::Done),
        "error" => Some(TestEvent::Failed(
            v.get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("test run failed")
                .to_string(),
        )),
        _ => None,
    }
}

fn spawn_reader<R: std::io::Read + Send + 'static>(reader: R, is_stderr: bool, tx: Sender<Raw>) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let mut text = String::from_utf8_lossy(&buf).into_owned();
                    while text.ends_with('\n') || text.ends_with('\r') {
                        text.pop();
                    }
                    if tx.send(Raw::Line { text, is_stderr }).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = tx.send(Raw::Eof);
    });
}

const LIBRARY_MARKERS: &[&str] = &[
    "site-packages",
    "dist-packages",
    ".venv",
    "venv",
    "lib/python",
    "lib64/python",
];

fn project_path(path: &str, root: &Path) -> Option<PathBuf> {
    if path.starts_with('<') || path.is_empty() {
        return None;
    }
    let p = Path::new(path);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    };
    if !abs.starts_with(root) {
        return None;
    }
    let s = abs.to_string_lossy().replace('\\', "/");
    if LIBRARY_MARKERS
        .iter()
        .any(|m| s.contains(&format!("/{m}/")))
    {
        return None;
    }
    Some(abs)
}

/// Finds the deepest project-file frame in a Python traceback or a pytest
/// failure report. Returns the absolute path and 1-based line.
pub fn parse_failure_location(traceback: &str, project_root: &Path) -> Option<(PathBuf, usize)> {
    let mut best: Option<(PathBuf, usize)> = None;
    for raw in traceback.lines() {
        let line = raw.trim();
        let mut cand: Option<(String, usize)> = None;
        if let Some(rest) = line.strip_prefix("File \"") {
            // File "path", line N, in name
            if let Some((path, rest)) = rest.split_once('"') {
                if let Some(rest) = rest
                    .trim_start_matches(',')
                    .trim_start()
                    .strip_prefix("line ")
                {
                    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                    if let Ok(n) = digits.parse::<usize>() {
                        cand = Some((path.to_string(), n));
                    }
                }
            }
        } else if let Some(idx) = line.find(".py:") {
            // pytest: path.py:N: Error   /   path.py:N: in func
            let path = &line[..idx + 3];
            let rest = &line[idx + 4..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            let after = &rest[digits.len()..];
            if !digits.is_empty()
                && (after.is_empty() || after.starts_with(':'))
                && !path.contains(' ')
            {
                if let Ok(n) = digits.parse::<usize>() {
                    cand = Some((path.to_string(), n));
                }
            }
        }
        if let Some((path, n)) = cand {
            if let Some(abs) = project_path(&path, project_root) {
                best = Some((abs, n));
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"import unittest


def test_plain():
    print("plain output")
    assert True


class SampleCase(unittest.TestCase):
    def test_pass(self):
        print("hello from pass")
        self.assertEqual(1 + 1, 2)

    def test_fail(self):
        self.assertEqual(1, 2, "one is not two")

    @unittest.skip("not today")
    def test_skip(self):
        pass

    def test_error(self):
        raise RuntimeError("boom")
"#;

    fn required_providers() -> bool {
        std::env::var("RUSTPY_REQUIRE_PROVIDER_TESTS").is_ok_and(|value| value == "1")
    }

    fn python() -> Option<PathBuf> {
        if required_providers() {
            let path = PathBuf::from(
                std::env::var_os("RUSTPY_PROVIDER_PYTHON")
                    .expect("required-provider mode needs RUSTPY_PROVIDER_PYTHON"),
            );
            assert!(
                path.is_absolute() && path.is_file(),
                "RUSTPY_PROVIDER_PYTHON must be an installed absolute executable: {}",
                path.display()
            );
            return Some(path);
        }
        which::which("python3")
            .ok()
            .or_else(|| which::which("python").ok())
    }

    fn provider_available(available: bool, provider: &str, python: &Path) -> bool {
        assert!(
            !required_providers() || available,
            "required provider {provider} is unavailable for {}",
            python.display()
        );
        available
    }

    fn require_module(python: &Path, module: &str) {
        let output = std::process::Command::new(python)
            .args([
                "-c",
                &format!("import importlib; importlib.import_module({module:?})"),
            ])
            .output()
            .expect("configured Python must execute");
        assert!(
            output.status.success(),
            "required module {module} for {}: {}",
            python.display(),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn run_to_end(run: &mut TestRun) -> Vec<TestEvent> {
        let start = Instant::now();
        let mut events = Vec::new();
        while run.is_running() {
            events.extend(run.poll());
            if start.elapsed() > Duration::from_secs(90) {
                run.stop();
                panic!("test run timed out; events so far: {events:#?}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        events
    }

    fn result<'a>(events: &'a [TestEvent], id: &str) -> &'a TestResult {
        events
            .iter()
            .find_map(|e| match e {
                TestEvent::Result(r) if r.id == id => Some(r),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no result for {id}; events: {events:#?}"))
    }

    fn assert_started_before_result(events: &[TestEvent], id: &str) {
        let s = events
            .iter()
            .position(|e| matches!(e, TestEvent::Started(i) if i == id))
            .unwrap_or_else(|| panic!("no start for {id}"));
        let r = events
            .iter()
            .position(|e| matches!(e, TestEvent::Result(r) if r.id == id))
            .unwrap();
        assert!(s < r, "start of {id} must precede its result");
    }

    #[test]
    fn unittest_end_to_end() {
        let Some(py) = python() else {
            eprintln!("python3 not found; skipping");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("test_sample.py"), SAMPLE).unwrap();
        let mut run = TestRun::start(1, Framework::Unittest, &py, &root, &[], &[], &[]).unwrap();
        assert!(run.is_running());
        let events = run_to_end(&mut run);
        let collected: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                TestEvent::Collected { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            collected,
            vec![
                "test_sample.py::SampleCase::test_error",
                "test_sample.py::SampleCase::test_fail",
                "test_sample.py::SampleCase::test_pass",
                "test_sample.py::SampleCase::test_skip"
            ]
        );
        if let Some(TestEvent::Collected { path, line, .. }) = events
            .iter()
            .find(|e| matches!(e, TestEvent::Collected { id, .. } if id.ends_with("test_fail")))
        {
            assert_eq!(path.as_deref(), Some(root.join("test_sample.py").as_path()));
            assert_eq!(*line, Some(14));
        }
        for id in ["test_pass", "test_fail", "test_skip", "test_error"] {
            assert_started_before_result(&events, &format!("test_sample.py::SampleCase::{id}"));
        }
        let pass = result(&events, "test_sample.py::SampleCase::test_pass");
        assert_eq!(pass.outcome, Outcome::Passed);
        assert!(pass.stdout.contains("hello from pass"), "{pass:?}");
        let fail = result(&events, "test_sample.py::SampleCase::test_fail");
        assert_eq!(fail.outcome, Outcome::Failed);
        assert!(fail.message.contains("one is not two"), "{fail:?}");
        assert!(fail.traceback.contains("test_fail"));
        assert_eq!(
            parse_failure_location(&fail.traceback, &root),
            Some((root.join("test_sample.py"), 15))
        );
        let skip = result(&events, "test_sample.py::SampleCase::test_skip");
        assert_eq!(skip.outcome, Outcome::Skipped);
        assert_eq!(skip.message, "not today");
        let err = result(&events, "test_sample.py::SampleCase::test_error");
        assert_eq!(err.outcome, Outcome::Error);
        assert!(err.message.contains("RuntimeError: boom"), "{err:?}");
        let done = events
            .iter()
            .find_map(|e| match e {
                TestEvent::Done(s) => Some(s.clone()),
                _ => None,
            })
            .expect("done event");
        assert_eq!(
            (done.passed, done.failed, done.skipped, done.errors),
            (1, 1, 1, 1)
        );
        assert_eq!(done.exit_code, Some(1));
        assert!(!done.is_success());
        assert!(
            matches!(events.last(), Some(TestEvent::Exited(1))),
            "{:?}",
            events.last()
        );
        assert!(!events.iter().any(|e| matches!(e, TestEvent::Failed(_))));
        assert!(!run.is_running());
        assert!(run.poll().is_empty());
    }

    #[test]
    fn pytest_end_to_end() {
        let Some(py) = python() else {
            eprintln!("python3 not found; skipping");
            return;
        };
        if !provider_available(pytest_available(&py), "pytest", &py) {
            eprintln!("pytest not installed; skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("test_sample.py"), SAMPLE).unwrap();
        // Absolute file target: ids must still be relative to the root.
        let target = root.join("test_sample.py").to_string_lossy().into_owned();
        let mut run = TestRun::start(
            2,
            Framework::Pytest,
            &py,
            &root,
            &[target],
            &["-q".into()],
            &[],
        )
        .unwrap();
        let events = run_to_end(&mut run);
        let collected: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                TestEvent::Collected { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        // unittest.TestCase methods are collected in alphabetical order (unittest semantics).
        assert_eq!(
            collected,
            vec![
                "test_sample.py::test_plain",
                "test_sample.py::SampleCase::test_error",
                "test_sample.py::SampleCase::test_fail",
                "test_sample.py::SampleCase::test_pass",
                "test_sample.py::SampleCase::test_skip"
            ]
        );
        if let Some(TestEvent::Collected { path, line, .. }) = events
            .iter()
            .find(|e| matches!(e, TestEvent::Collected { id, .. } if id.ends_with("test_plain")))
        {
            assert_eq!(path.as_deref(), Some(root.join("test_sample.py").as_path()));
            assert_eq!(*line, Some(4));
        }
        for id in [
            "test_plain",
            "SampleCase::test_pass",
            "SampleCase::test_fail",
            "SampleCase::test_skip",
            "SampleCase::test_error",
        ] {
            assert_started_before_result(&events, &format!("test_sample.py::{id}"));
        }
        let plain = result(&events, "test_sample.py::test_plain");
        assert_eq!(plain.outcome, Outcome::Passed);
        assert!(plain.stdout.contains("plain output"), "{plain:?}");
        let fail = result(&events, "test_sample.py::SampleCase::test_fail");
        assert_eq!(fail.outcome, Outcome::Failed);
        assert!(fail.message.contains("one is not two"), "{fail:?}");
        assert_eq!(
            parse_failure_location(&fail.traceback, &root),
            Some((root.join("test_sample.py"), 15))
        );
        let skip = result(&events, "test_sample.py::SampleCase::test_skip");
        assert_eq!(skip.outcome, Outcome::Skipped);
        assert!(skip.message.contains("not today"), "{skip:?}");
        let err = result(&events, "test_sample.py::SampleCase::test_error");
        assert_eq!(err.outcome, Outcome::Failed);
        assert!(err.message.contains("boom"), "{err:?}");
        let done = events
            .iter()
            .find_map(|e| match e {
                TestEvent::Done(s) => Some(s.clone()),
                _ => None,
            })
            .expect("done event");
        assert_eq!(
            (done.passed, done.failed, done.skipped, done.errors),
            (2, 2, 1, 0)
        );
        assert_eq!(done.exit_code, Some(1));
        assert!(matches!(events.last(), Some(TestEvent::Exited(1))));
        // pytest's own summary line is forwarded as raw output.
        assert!(
            events
                .iter()
                .any(|e| matches!(e, TestEvent::Output(l, false) if l.contains("failed"))),
            "{events:#?}"
        );
    }

    #[test]
    fn pytest_selects_single_test_by_node_id() {
        let Some(py) = python() else { return };
        if !provider_available(pytest_available(&py), "pytest", &py) {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("test_sample.py"), SAMPLE).unwrap();
        let mut run = TestRun::start(
            3,
            Framework::Pytest,
            &py,
            &root,
            &["test_sample.py::SampleCase::test_pass".into()],
            &[],
            &[],
        )
        .unwrap();
        let events = run_to_end(&mut run);
        let results: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                TestEvent::Result(r) => Some(r.id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(results, vec!["test_sample.py::SampleCase::test_pass"]);
        assert!(matches!(events.last(), Some(TestEvent::Exited(0))));
    }

    #[test]
    fn unittest_selects_single_test_by_node_id() {
        let Some(py) = python() else { return };
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("tests/test_sample.py"), SAMPLE).unwrap();
        let mut run = TestRun::start(
            4,
            Framework::Unittest,
            &py,
            &root,
            &["tests/test_sample.py::SampleCase::test_skip".into()],
            &[],
            &[],
        )
        .unwrap();
        let events = run_to_end(&mut run);
        let results: Vec<(&str, Outcome)> = events
            .iter()
            .filter_map(|e| match e {
                TestEvent::Result(r) => Some((r.id.as_str(), r.outcome)),
                _ => None,
            })
            .collect();
        assert_eq!(
            results,
            vec![(
                "tests/test_sample.py::SampleCase::test_skip",
                Outcome::Skipped
            )]
        );
        assert!(matches!(events.last(), Some(TestEvent::Exited(0))));
    }

    /// A src-layout project whose tests read the environment, the working
    /// directory and interpreter flags.
    const OPTIONS_TEST: &str = r#"import os
import sys

from mypkg import VALUE


def test_imports_from_src():
    assert VALUE == 42


def test_env_cwd_and_flags():
    assert os.environ.get("RUSTPY_OPT") == "from-config"
    assert os.path.basename(os.getcwd()) == "workdir"
    assert sys.flags.dev_mode


def test_fails_first():
    assert False


def test_never_runs_with_x():
    pass
"#;

    fn options_project() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("src/mypkg")).unwrap();
        std::fs::write(root.join("src/mypkg/__init__.py"), "VALUE = 42\n").unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("tests/test_opts.py"), OPTIONS_TEST).unwrap();
        std::fs::create_dir_all(root.join("workdir")).unwrap();
        (dir, root)
    }

    #[test]
    fn pytest_honours_run_options() {
        let Some(py) = python() else { return };
        if !provider_available(pytest_available(&py), "pytest", &py) {
            return;
        }
        let (_dir, root) = options_project();
        let src = root.join("src").to_string_lossy().into_owned();
        let options = TestRunOptions {
            env: vec![
                ("RUSTPY_OPT".into(), "from-config".into()),
                ("PYTHONPATH".into(), format!("{}:{src}", root.display())),
            ],
            cwd: Some(PathBuf::from("workdir")),
            interpreter_options: vec!["-X".into(), "dev".into()],
            ..Default::default()
        };
        // `-x` stops at the first failure: the last test never runs.
        let mut run = TestRun::start_with(
            20,
            Framework::Pytest,
            &py,
            &root,
            &["tests/test_opts.py".into()],
            &["-x".into()],
            &options,
        )
        .unwrap();
        assert!(
            run.command_line
                .contains(&format!("{} -X dev -u", py.display())),
            "{}",
            run.command_line
        );
        assert!(run.command_line.contains("--cwd"), "{}", run.command_line);
        let events = run_to_end(&mut run);
        let results = results_of(&events);
        assert_eq!(
            results,
            vec![
                (
                    "tests/test_opts.py::test_imports_from_src".to_string(),
                    Outcome::Passed
                ),
                (
                    "tests/test_opts.py::test_env_cwd_and_flags".to_string(),
                    Outcome::Passed
                ),
                (
                    "tests/test_opts.py::test_fails_first".to_string(),
                    Outcome::Failed
                )
            ],
            "{events:#?}"
        );
        // Without the configuration's PYTHONPATH the src layout cannot be imported.
        let mut run = TestRun::start(
            21,
            Framework::Pytest,
            &py,
            &root,
            &["tests/test_opts.py::test_imports_from_src".into()],
            &[],
            &[],
        )
        .unwrap();
        let events = run_to_end(&mut run);
        assert!(
            results_of(&events).iter().all(|(_, o)| o.is_problem()),
            "{events:#?}"
        );
    }

    #[test]
    fn unittest_honours_run_options() {
        let Some(py) = python() else { return };
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("src/mypkg")).unwrap();
        std::fs::write(root.join("src/mypkg/__init__.py"), "VALUE = 42\n").unwrap();
        std::fs::create_dir_all(root.join("workdir")).unwrap();
        std::fs::write(root.join("test_case.py"), "import os, sys, unittest\nfrom mypkg import VALUE\n\nclass Case(unittest.TestCase):\n    def test_all(self):\n        self.assertEqual(VALUE, 42)\n        self.assertEqual(os.environ['RUSTPY_OPT'], 'x')\n        self.assertEqual(os.path.basename(os.getcwd()), 'workdir')\n        self.assertTrue(sys.flags.dev_mode)\n").unwrap();
        let src = root.join("src").to_string_lossy().into_owned();
        let options = TestRunOptions {
            env: vec![
                ("RUSTPY_OPT".into(), "x".into()),
                ("PYTHONPATH".into(), src),
            ],
            cwd: Some(root.join("workdir")),
            interpreter_options: vec!["-X".into(), "dev".into()],
            ..Default::default()
        };
        let mut run =
            TestRun::start_with(22, Framework::Unittest, &py, &root, &[], &[], &options).unwrap();
        let events = run_to_end(&mut run);
        assert_eq!(
            results_of(&events),
            vec![("test_case.py::Case::test_all".to_string(), Outcome::Passed)],
            "{events:#?}"
        );
        // A missing working directory is reported instead of a spawn error.
        let bad = TestRunOptions {
            cwd: Some(PathBuf::from("nope")),
            ..Default::default()
        };
        let err = TestRun::start_with(23, Framework::Unittest, &py, &root, &[], &[], &bad)
            .err()
            .expect("missing cwd");
        assert!(err.to_string().contains("does not exist"), "{err}");
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    #[cfg(unix)] // Unix fixture paths, tools or process semantics
    fn runtime_type_collector_wraps_each_runner_command() {
        let c = Path::new("/r/rustpy_typecollect.py");
        let root = Path::new("/p");
        let out = "/p/.rustpy/runtime_types.json";
        // pytest / unittest / doctest / nose / tox: the reporter script.
        assert_eq!(
            runtime_types_args(
                &strings(&[
                    "-u",
                    "/r/reporter.py",
                    "--framework",
                    "pytest",
                    "--",
                    "-k",
                    "x"
                ]),
                c,
                root
            ),
            strings(&[
                "-u",
                "/r/rustpy_typecollect.py",
                out,
                "/p",
                "/r/reporter.py",
                "--framework",
                "pytest",
                "--",
                "-k",
                "x"
            ])
        );
        // behave and Trial: runner modules.
        assert_eq!(
            runtime_types_args(
                &strings(&["-u", "-m", "behave", "--format", "plain"]),
                c,
                root
            ),
            strings(&[
                "-u",
                "/r/rustpy_typecollect.py",
                out,
                "/p",
                "-m",
                "behave",
                "--format",
                "plain"
            ])
        );
        assert_eq!(
            runtime_types_args(&strings(&["-u", "-m", "twisted.trial", "t.py"]), c, root),
            strings(&[
                "-u",
                "/r/rustpy_typecollect.py",
                out,
                "/p",
                "-m",
                "twisted.trial",
                "t.py"
            ])
        );
        // Debug-failed-tests bootstrap wraps the runner; the collector wraps the bootstrap.
        assert_eq!(
            runtime_types_args(
                &strings(&[
                    "-u",
                    "/r/rustpy_debug_test_bootstrap.py",
                    "unittest",
                    "/r/reporter.py"
                ]),
                c,
                root
            ),
            strings(&[
                "-u",
                "/r/rustpy_typecollect.py",
                out,
                "/p",
                "/r/rustpy_debug_test_bootstrap.py",
                "unittest",
                "/r/reporter.py"
            ])
        );
    }

    #[test]
    fn test_runs_collect_runtime_types_beside_the_reporter() {
        let Some(py) = python() else { return };
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        std::fs::write(
            root.join("calc.py"),
            "def double(value):\n    return value * 2\n",
        )
        .unwrap();
        std::fs::write(
            root.join("test_calc.py"),
            "import unittest\nfrom calc import double\n\nclass Case(unittest.TestCase):\n    def test_double(self):\n        self.assertEqual(double(21), 42)\n        self.assertEqual(double('a'), 'aa')\n",
        )
        .unwrap();
        let options = TestRunOptions {
            interpreter_options: vec!["-X".into(), "dev".into()],
            collect_runtime_types: true,
            ..Default::default()
        };
        let mut run =
            TestRun::start_with(24, Framework::Unittest, &py, &root, &[], &[], &options).unwrap();
        assert!(
            run.command_line.contains("-X dev -u ")
                && run.command_line.contains("rustpy_typecollect.py"),
            "{}",
            run.command_line
        );
        let events = run_to_end(&mut run);
        // The reporter still reports through the collector.
        assert_eq!(
            results_of(&events),
            vec![(
                "test_calc.py::Case::test_double".to_string(),
                Outcome::Passed
            )],
            "{events:#?}"
        );
        let text = std::fs::read_to_string(rustpy_run::typecollect::output_path(&root)).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        let key = format!("{}::double", root.join("calc.py").display());
        let entry = &json["functions"][&key];
        assert_eq!(
            entry["params"]["value"],
            serde_json::json!(["int", "str"]),
            "{text}"
        );
        assert_eq!(entry["return"], serde_json::json!(["int", "str"]), "{text}");
        // Off by default: no collector on the command line.
        let mut plain = TestRun::start_with(
            25,
            Framework::Unittest,
            &py,
            &root,
            &[],
            &[],
            &TestRunOptions::default(),
        )
        .unwrap();
        assert!(
            !plain.command_line.contains("typecollect"),
            "{}",
            plain.command_line
        );
        run_to_end(&mut plain);
    }

    const DIFF_TEST: &str = r#"import unittest


def test_dicts():
    actual = {"name": "rustpy", "tags": ["a", "b"], "n": 1}
    assert actual == {"name": "rustpy", "tags": ["a", "c"], "n": 1}


def test_strings():
    assert "line 1\nline 2\n" == "line 1\nline two\n"


def test_not_equality():
    assert 3 < 2


def test_caught_then_other_failure():
    try:
        assert [1] == [2]
    except AssertionError:
        pass
    raise ValueError("unrelated")


class Case(unittest.TestCase):
    def test_assert_equal(self):
        self.assertEqual({"k": [1, 2]}, {"k": [1, 3]})

    def test_multiline(self):
        self.assertMultiLineEqual("a\nb\n", "a\nc\n")

    def test_other_failure(self):
        self.assertTrue(False)
"#;

    fn comparison_of(events: &[TestEvent], id: &str) -> Option<(String, String)> {
        last_result(events, id)
            .comparison()
            .map(|(e, a)| (e.to_string(), a.to_string()))
    }

    #[test]
    fn pytest_reports_compared_values() {
        let Some(py) = python() else { return };
        if !provider_available(pytest_available(&py), "pytest", &py) {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("test_diff.py"), DIFF_TEST).unwrap();
        let mut run = TestRun::start(24, Framework::Pytest, &py, &root, &[], &[], &[]).unwrap();
        let events = run_to_end(&mut run);
        assert_eq!(
            comparison_of(&events, "test_diff.py::test_dicts"),
            Some((
                "{'n': 1, 'name': 'rustpy', 'tags': ['a', 'c']}".into(),
                "{'n': 1, 'name': 'rustpy', 'tags': ['a', 'b']}".into()
            ))
        );
        assert_eq!(
            comparison_of(&events, "test_diff.py::test_strings"),
            Some(("line 1\nline two\n".into(), "line 1\nline 2\n".into()))
        );
        assert_eq!(
            comparison_of(&events, "test_diff.py::test_not_equality"),
            None
        );
        assert_eq!(
            comparison_of(&events, "test_diff.py::test_caught_then_other_failure"),
            None
        );
        // unittest assertions inside pytest runs are captured too (expected first).
        assert_eq!(
            comparison_of(&events, "test_diff.py::Case::test_assert_equal"),
            Some(("{'k': [1, 2]}".into(), "{'k': [1, 3]}".into()))
        );
        assert_eq!(
            comparison_of(&events, "test_diff.py::Case::test_other_failure"),
            None
        );
        // pytest's own explanation is unchanged and the wrapper frame stays hidden.
        let dicts = last_result(&events, "test_diff.py::test_dicts");
        assert!(
            dicts.traceback.contains("Differing items"),
            "{}",
            dicts.traceback
        );
        let ut = last_result(&events, "test_diff.py::Case::test_assert_equal");
        assert!(
            !ut.traceback.contains("rustpy_test_reporter"),
            "{}",
            ut.traceback
        );
    }

    #[test]
    fn unittest_reports_compared_values() {
        let Some(py) = python() else { return };
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("test_diff.py"), DIFF_TEST).unwrap();
        let mut run = TestRun::start(
            25,
            Framework::Unittest,
            &py,
            &root,
            &["test_diff.py::Case".into()],
            &[],
            &[],
        )
        .unwrap();
        let events = run_to_end(&mut run);
        assert_eq!(
            comparison_of(&events, "test_diff.py::Case::test_assert_equal"),
            Some(("{'k': [1, 2]}".into(), "{'k': [1, 3]}".into()))
        );
        assert_eq!(
            comparison_of(&events, "test_diff.py::Case::test_multiline"),
            Some(("a\nb\n".into(), "a\nc\n".into()))
        );
        assert_eq!(
            comparison_of(&events, "test_diff.py::Case::test_other_failure"),
            None
        );
        let r = last_result(&events, "test_diff.py::Case::test_assert_equal");
        assert!(
            !r.traceback.contains("rustpy_test_reporter"),
            "the capturing wrapper must not show in tracebacks: {}",
            r.traceback
        );
        assert_eq!(
            parse_failure_location(&r.traceback, &root),
            Some((root.join("test_diff.py"), 27))
        );
    }

    #[test]
    fn broken_module_reports_an_error_result() {
        let Some(py) = python() else { return };
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        std::fs::write(
            root.join("test_broken.py"),
            "import does_not_exist\n\ndef test_x():\n    pass\n",
        )
        .unwrap();
        let mut run = TestRun::start(5, Framework::Unittest, &py, &root, &[], &[], &[]).unwrap();
        let events = run_to_end(&mut run);
        let err = events
            .iter()
            .find_map(|e| match e {
                TestEvent::Result(r) if r.outcome == Outcome::Error => Some(r),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{events:#?}"));
        assert!(
            err.message.contains("does_not_exist") || err.traceback.contains("does_not_exist"),
            "{err:?}"
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, TestEvent::Done(s) if s.errors == 1)));
    }

    const KEYWORD_ASSERTS: &str = r#"import unittest


class Keywords(unittest.TestCase):
    def test_keyword_calls(self):
        self.assertDictEqual(d1={"a": 1}, d2={"a": 1})
        self.assertListEqual(list1=[1], list2=[1])
        self.assertTupleEqual(tuple1=(1,), tuple2=(1,))
        self.assertSetEqual(set1={1}, set2={1})
        self.assertSequenceEqual(seq1=[1], seq2=[1])
        self.assertEqual(first=1, second=1)
        self.assertCountEqual(first=[1, 2], second=[2, 1])
        self.assertMultiLineEqual(first="a\\nb", second="a\\nb")
        import inspect
        for name in ("assertEqual", "assertDictEqual", "assertListEqual", "assertTupleEqual", "assertSetEqual", "assertSequenceEqual", "assertCountEqual", "assertMultiLineEqual"):
            wrapped = getattr(unittest.TestCase, name)
            self.assertEqual(inspect.signature(wrapped), inspect.signature(wrapped.__wrapped__))

    def test_failing_keyword_call(self):
        self.assertDictEqual(d2={"a": 2}, d1={"a": 1})

    def test_wrong_arguments_raise_type_error(self):
        with self.assertRaises(TypeError):
            self.assertListEqual([1])
"#;

    /// The equality-capturing wrappers accept every call the assertions
    /// accept (keyword arguments included), in unittest and pytest runs.
    #[test]
    fn equality_capture_keeps_keyword_arguments() {
        let Some(py) = python() else { return };
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("test_kw.py"), KEYWORD_ASSERTS).unwrap();
        let mut frameworks = vec![Framework::Unittest];
        if pytest_available(&py) {
            frameworks.push(Framework::Pytest);
        }
        for (n, framework) in frameworks.into_iter().enumerate() {
            let mut run = TestRun::start(
                30 + n as u64,
                framework,
                &py,
                &root,
                &["test_kw.py".into()],
                &[],
                &[],
            )
            .unwrap();
            let events = run_to_end(&mut run);
            let expected = vec![
                (
                    "test_kw.py::Keywords::test_failing_keyword_call".to_string(),
                    Outcome::Failed,
                ),
                (
                    "test_kw.py::Keywords::test_keyword_calls".to_string(),
                    Outcome::Passed,
                ),
                (
                    "test_kw.py::Keywords::test_wrong_arguments_raise_type_error".to_string(),
                    Outcome::Passed,
                ),
            ];
            let mut results = results_of(&events);
            results.sort_by(|a, b| a.0.cmp(&b.0));
            assert_eq!(results, expected, "{framework:?}: {events:#?}");
            // Values passed by keyword are captured by parameter, not by position.
            assert_eq!(
                comparison_of(&events, "test_kw.py::Keywords::test_failing_keyword_call"),
                Some(("{'a': 1}".into(), "{'a': 2}".into())),
                "{framework:?}"
            );
        }
    }

    const STALE_COMPARISONS: &str = r#"import pytest


def test_raises_then_message():
    with pytest.raises(AssertionError):
        assert {"a": 1} == {"a": 2}
    x = 0
    assert x, "x must be truthy"


def test_caught_then_other_assert():
    try:
        assert [1] == [2]
    except AssertionError:
        pass
    assert 3 < 2


def check(a, b):
    assert a == b


def test_helper_caught_then_other():
    try:
        check(1, 2)
    except AssertionError:
        pass
    assert not True


def test_helper_compare():
    check([1, 2], [1, 5])


def test_reraised_from_a_loop():
    for i in range(2):
        try:
            assert i == 5
        except AssertionError:
            if i == 1:
                raise


def test_loop_fails_again_before_the_compare():
    for c, a in [(True, 1), (False, 1)]:
        try:
            assert c and a == 2
        except AssertionError:
            if not c:
                raise


def test_later_compare_fails():
    try:
        assert "x" == "y"
    except AssertionError:
        pass
    assert "left" == (
        "right"
    )


@pytest.fixture
def broken_teardown():
    yield
    assert False, "teardown problem"


def test_teardown_error(broken_teardown):
    assert 1 == 2
"#;

    /// "Click to see difference" shows the comparison of the assert that
    /// failed: one that failed earlier (and was caught) or in another phase
    /// of the test is not offered.
    #[test]
    fn pytest_comparison_belongs_to_the_failing_assert() {
        let Some(py) = python() else { return };
        if !provider_available(pytest_available(&py), "pytest", &py) {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("test_stale.py"), STALE_COMPARISONS).unwrap();
        let mut run = TestRun::start(33, Framework::Pytest, &py, &root, &[], &[], &[]).unwrap();
        let events = run_to_end(&mut run);
        assert_eq!(
            comparison_of(&events, "test_stale.py::test_raises_then_message"),
            None
        );
        assert_eq!(
            comparison_of(&events, "test_stale.py::test_caught_then_other_assert"),
            None
        );
        assert_eq!(
            comparison_of(&events, "test_stale.py::test_helper_caught_then_other"),
            None
        );
        assert_eq!(
            comparison_of(&events, "test_stale.py::test_helper_compare"),
            Some(("[1, 5]".into(), "[1, 2]".into()))
        );
        assert_eq!(
            comparison_of(&events, "test_stale.py::test_reraised_from_a_loop"),
            Some(("5".into(), "1".into()))
        );
        // The same assert failed again without reaching its `==` (`c` is false):
        // the comparison of its first, caught failure is not this failure's diff.
        assert_eq!(
            comparison_of(
                &events,
                "test_stale.py::test_loop_fails_again_before_the_compare"
            ),
            None
        );
        assert_eq!(
            comparison_of(&events, "test_stale.py::test_later_compare_fails"),
            Some(("right".into(), "left".into()))
        );
        // The call keeps its diff; the teardown error that follows has none.
        let teardown: Vec<&TestResult> = events
            .iter()
            .filter_map(|e| match e {
                TestEvent::Result(r) if r.id == "test_stale.py::test_teardown_error" => Some(r),
                _ => None,
            })
            .collect();
        assert_eq!(
            teardown
                .iter()
                .map(|r| (r.outcome, r.comparison()))
                .collect::<Vec<_>>(),
            vec![(Outcome::Failed, Some(("2", "1"))), (Outcome::Error, None)],
            "{events:#?}"
        );
    }

    /// The discovery arguments of `python -m unittest discover` in a
    /// configuration's Parameters work in the Test Runner (they used to be
    /// taken as test names: "cannot import *_test.py").
    #[test]
    fn unittest_discovery_arguments() {
        let Some(py) = python() else { return };
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("tests/thing_test.py"), "import unittest\n\n\nclass Thing(unittest.TestCase):\n    def test_one(self):\n        pass\n").unwrap();
        std::fs::write(root.join("tests/test_other.py"), "import unittest\n\n\nclass Other(unittest.TestCase):\n    def test_other(self):\n        pass\n").unwrap();
        let args = |v: &[&str]| v.iter().map(|a| a.to_string()).collect::<Vec<String>>();
        let thing = vec![(
            "tests/thing_test.py::Thing::test_one".to_string(),
            Outcome::Passed,
        )];
        let cases: Vec<(Vec<String>, Vec<String>)> = vec![
            // Discovery with options, long options, or `discover` and positional values.
            (vec![], args(&["-s", "tests", "-p", "*_test.py"])),
            (
                vec![],
                args(&["--start-directory=tests", "--pattern", "*_test.py", "-v"]),
            ),
            (vec![], args(&["discover", "tests", "*_test.py"])),
            // Rerunning one test of such a run: the target, not the discovery.
            (
                vec!["tests/thing_test.py::Thing::test_one".into()],
                args(&["discover", "tests"]),
            ),
            // A pattern also selects the files of a directory target.
            (vec!["tests".into()], args(&["-p", "*_test.py"])),
        ];
        for (n, (targets, extra)) in cases.into_iter().enumerate() {
            let mut run = TestRun::start(
                34 + n as u64,
                Framework::Unittest,
                &py,
                &root,
                &targets,
                &extra,
                &[],
            )
            .unwrap();
            let events = run_to_end(&mut run);
            assert_eq!(
                results_of(&events),
                thing,
                "{targets:?} {extra:?}: {events:#?}"
            );
            assert_eq!(done_of(&events).exit_code, Some(0), "{extra:?}");
        }
        // `discover -s tests` alone uses unittest's default pattern `test*.py`.
        let mut run = TestRun::start(
            39,
            Framework::Unittest,
            &py,
            &root,
            &[],
            &args(&["discover", "-s", "tests"]),
            &[],
        )
        .unwrap();
        let events = run_to_end(&mut run);
        assert_eq!(
            results_of(&events),
            vec![(
                "tests/test_other.py::Other::test_other".to_string(),
                Outcome::Passed
            )],
            "{events:#?}"
        );
    }

    #[test]
    fn unittest_discovery_requests() {
        let args = |v: &[&str]| v.iter().map(|a| a.to_string()).collect::<Vec<String>>();
        for yes in [
            &["-s", "tests"][..],
            &["-p*_test.py"],
            &["discover"],
            &["--pattern=*_t.py"],
            &["-v", "--top-level-directory", "."],
        ] {
            assert!(unittest_discovery_requested(&args(yes)), "{yes:?}");
        }
        for no in [
            &[][..],
            &["-v", "-f"],
            &["-k", "-slow"],
            &["-k", "discover"],
            &["tests"],
            &["--durations", "5"],
        ] {
            assert!(!unittest_discovery_requested(&args(no)), "{no:?}");
        }
    }

    fn copy_dir(src: &Path, dst: &Path) {
        std::fs::create_dir_all(dst).unwrap();
        for entry in std::fs::read_dir(src).unwrap().flatten() {
            let to = dst.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                if entry.file_name() != "__pycache__" {
                    copy_dir(&entry.path(), &to);
                }
            } else {
                std::fs::copy(entry.path(), to).unwrap();
            }
        }
    }

    /// A copy of `tests/fixtures/<kind>/project` in a temp dir.
    fn fixture_project(kind: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        copy_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(kind)
                .join("project"),
            &root,
        );
        (dir, root)
    }

    /// `(id, outcome)` of the final result of every test, in first-result
    /// order (live trial runs report problems twice: early, then with details).
    fn results_of(events: &[TestEvent]) -> Vec<(String, Outcome)> {
        let mut out: Vec<(String, Outcome)> = Vec::new();
        for e in events {
            if let TestEvent::Result(r) = e {
                match out.iter_mut().find(|(id, _)| *id == r.id) {
                    Some(prev) => prev.1 = r.outcome,
                    None => out.push((r.id.clone(), r.outcome)),
                }
            }
        }
        out
    }

    fn last_result<'a>(events: &'a [TestEvent], id: &str) -> &'a TestResult {
        events
            .iter()
            .rev()
            .find_map(|e| match e {
                TestEvent::Result(r) if r.id == id => Some(r),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no result for {id}; events: {events:#?}"))
    }

    fn done_of(events: &[TestEvent]) -> Summary {
        events
            .iter()
            .find_map(|e| match e {
                TestEvent::Done(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no done event: {events:#?}"))
    }

    #[test]
    fn trial_end_to_end() {
        let Some(py) = python() else { return };
        if !provider_available(trial_available(&py), "twisted.trial", &py) {
            eprintln!("twisted not installed; skipping");
            return;
        }
        let (_dir, root) = fixture_project("trial");
        let mut run = TestRun::start(7, Framework::Trial, &py, &root, &[], &[], &[]).unwrap();
        assert!(
            run.command_line.contains(
                "-m twisted.trial --reporter=timing tests/test_broken.py tests/test_sample.py"
            ),
            "{}",
            run.command_line
        );
        let events = run_to_end(&mut run);
        // Statically collected first, with locations.
        assert!(
            matches!(&events[0], TestEvent::Collected { id, line: Some(6), .. } if id == "tests/test_broken.py::BrokenTest::test_x"),
            "{:?}",
            events[0]
        );
        let sample = "tests/test_sample.py::SampleTest";
        for t in [
            "test_pass",
            "test_fail",
            "test_error",
            "test_skip",
            "test_todo",
            "test_unexpected",
            "test_deferred_fail",
        ] {
            assert_started_before_result(&events, &format!("{sample}::{t}"));
        }
        let pass = last_result(&events, &format!("{sample}::test_pass"));
        assert_eq!(
            (pass.outcome, pass.stdout.as_str()),
            (Outcome::Passed, "hello from pass")
        );
        let fail = last_result(&events, &format!("{sample}::test_fail"));
        assert_eq!(
            (fail.outcome, fail.message.as_str()),
            (Outcome::Failed, "FailTest: 1 != 2 : one is not two")
        );
        assert_eq!(
            parse_failure_location(&fail.traceback, &root),
            Some((root.join("tests/test_sample.py"), 11))
        );
        assert_eq!(
            last_result(&events, &format!("{sample}::test_skip")).message,
            "not today"
        );
        assert!(
            last_result(&events, "tests/test_sample.py::SlowTest::test_sleep").duration >= 0.04
        );
        let broken = last_result(&events, "tests/test_broken.py");
        assert_eq!(
            (broken.outcome, broken.message.as_str()),
            (
                Outcome::Error,
                "ModuleNotFoundError: No module named 'does_not_exist'"
            )
        );
        let done = done_of(&events);
        assert_eq!(
            (
                done.passed,
                done.failed,
                done.skipped,
                done.errors,
                done.exit_code
            ),
            (3, 2, 3, 2, Some(1))
        );
        assert!(matches!(events.last(), Some(TestEvent::Exited(1))));
        assert!(!events.iter().any(|e| matches!(e, TestEvent::Failed(_))));
    }

    #[test]
    fn trial_runs_selected_tests() {
        let Some(py) = python() else { return };
        if !provider_available(trial_available(&py), "twisted.trial", &py) {
            return;
        }
        let (_dir, root) = fixture_project("trial");
        let target = root
            .join("tests/test_sample.py")
            .to_string_lossy()
            .into_owned()
            + "::SampleTest::test_pass";
        let mut run = TestRun::start(
            8,
            Framework::Trial,
            &py,
            &root,
            &[target, "tests/test_sample.py::OtherTest".into()],
            &[],
            &[],
        )
        .unwrap();
        assert!(
            run.command_line.ends_with(
                "--reporter=timing test_sample.SampleTest.test_pass test_sample.OtherTest"
            ),
            "{}",
            run.command_line
        );
        let events = run_to_end(&mut run);
        assert_eq!(
            results_of(&events),
            vec![
                (
                    "tests/test_sample.py::SampleTest::test_pass".to_string(),
                    Outcome::Passed
                ),
                (
                    "tests/test_sample.py::OtherTest::test_a".to_string(),
                    Outcome::Skipped
                )
            ]
        );
        assert_eq!(
            last_result(&events, "tests/test_sample.py::OtherTest::test_a").message,
            "whole class skipped"
        );
        assert!(matches!(events.last(), Some(TestEvent::Exited(0))));
        // `-k` selects by name (trial has no such option).
        let mut run = TestRun::start(
            9,
            Framework::Trial,
            &py,
            &root,
            &["tests".into()],
            &["-k".into(), "sleep".into()],
            &[],
        )
        .unwrap();
        let events = run_to_end(&mut run);
        assert_eq!(
            results_of(&events),
            vec![(
                "tests/test_sample.py::SlowTest::test_sleep".to_string(),
                Outcome::Passed
            )]
        );
    }

    /// The interpreter for behave tests: `RUSTPY_BEHAVE_PYTHON` or python3.
    fn behave_python() -> Option<PathBuf> {
        let py = if required_providers() {
            python().expect("required Python")
        } else {
            std::env::var_os("RUSTPY_BEHAVE_PYTHON")
                .map(PathBuf::from)
                .or_else(python)?
        };
        if provider_available(behave_available(&py), "behave", &py) {
            Some(py)
        } else {
            eprintln!(
                "behave not installed for {}; skipping (set RUSTPY_BEHAVE_PYTHON)",
                py.display()
            );
            None
        }
    }

    #[test]
    fn behave_end_to_end() {
        let Some(py) = behave_python() else { return };
        let (_dir, root) = fixture_project("behave");
        let mut run = TestRun::start(10, Framework::Behave, &py, &root, &[], &[], &[]).unwrap();
        assert!(
            run.command_line
                .contains("-m behave --format json.pretty --outfile"),
            "{}",
            run.command_line
        );
        let events = run_to_end(&mut run);
        let calc = "features/calculator.feature::Calculator";
        assert!(matches!(&events[0], TestEvent::Collected { id, line: Some(2), .. } if id == calc));
        for s in [
            "Add two numbers",
            "Failing addition",
            "Exploding step",
            "Undefined step",
            "Add 2 and 2 -- @1.2 Small numbers",
        ] {
            assert_started_before_result(&events, &format!("{calc}::{s}"));
        }
        assert_eq!(results_of(&events).len(), 9, "{events:#?}");
        assert_eq!(
            last_result(&events, &format!("{calc}::Add two numbers")).outcome,
            Outcome::Passed
        );
        let fail = last_result(&events, &format!("{calc}::Failing addition"));
        assert_eq!(fail.outcome, Outcome::Failed);
        assert!(fail.message.ends_with("expected 5 but got 4"), "{fail:?}");
        assert_eq!(fail.stdout, "adding 2 2");
        let boom = last_result(&events, &format!("{calc}::Exploding step"));
        assert!(
            boom.outcome.is_problem() && boom.message == "ZeroDivisionError: division by zero",
            "{boom:?}"
        );
        assert_eq!(
            parse_failure_location(&boom.traceback, &root),
            Some((root.join("features/steps/calc_steps.py"), 17))
        );
        assert_eq!(
            last_result(&events, &format!("{calc}::Undefined step")).outcome,
            Outcome::Error
        );
        assert_eq!(
            last_result(&events, &format!("{calc}::Skipped scenario")).outcome,
            Outcome::Skipped
        );
        assert_eq!(
            last_result(&events, "features/rules.feature::Rules::Positive").outcome,
            Outcome::Passed
        );
        let done = done_of(&events);
        assert_eq!((done.skipped, done.exit_code), (1, Some(1)));
        assert_eq!(done.total(), 9);
        assert!(matches!(events.last(), Some(TestEvent::Exited(1))));
        assert!(!events.iter().any(|e| matches!(e, TestEvent::Failed(_))));
    }

    #[test]
    fn behave_runs_selected_scenarios() {
        let Some(py) = behave_python() else { return };
        let (_dir, root) = fixture_project("behave");
        let calc = "features/calculator.feature::Calculator";
        let targets = vec![
            format!("{calc}::Add two numbers"),
            "features/calculator.feature:29".to_string(),
        ];
        let mut run =
            TestRun::start(11, Framework::Behave, &py, &root, &targets, &[], &[]).unwrap();
        let events = run_to_end(&mut run);
        let results = results_of(&events);
        assert_eq!(
            results,
            vec![
                (format!("{calc}::Add two numbers"), Outcome::Passed),
                (
                    format!("{calc}::Add 1 and 2 -- @1.1 Small numbers"),
                    Outcome::Passed
                ),
                (
                    format!("{calc}::Add 2 and 2 -- @1.2 Small numbers"),
                    Outcome::Failed
                )
            ]
        );
        assert!(matches!(events.last(), Some(TestEvent::Exited(1))));
        // A keyword becomes behave's --name filter.
        let mut run = TestRun::start(
            12,
            Framework::Behave,
            &py,
            &root,
            &[],
            &["-k".into(), "Positive".into()],
            &[],
        )
        .unwrap();
        let events = run_to_end(&mut run);
        assert_eq!(
            results_of(&events),
            vec![(
                "features/rules.feature::Rules::Positive".to_string(),
                Outcome::Passed
            )]
        );
        assert!(matches!(events.last(), Some(TestEvent::Exited(0))));
    }

    #[test]
    fn behave_edge_cases_end_to_end() {
        let Some(py) = behave_python() else { return };
        let (_dir, root) = fixture_project("behave/edge");
        let mut run = TestRun::start(14, Framework::Behave, &py, &root, &[], &[], &[]).unwrap();
        let events = run_to_end(&mut run);
        let f = "features/edge.feature::Edge cases";
        // Scenarios after a hook error are still followed live.
        for s in [
            "Hook explodes",
            "Pending step",
            "Noisy failure",
            "Empty scenario",
            "Fine",
        ] {
            assert_started_before_result(&events, &format!("{f}::{s}"));
        }
        assert_eq!(
            last_result(&events, &format!("{f}::Hook explodes")).message,
            "HOOK-ERROR in before_scenario: RuntimeError: hook boom"
        );
        let noisy = last_result(&events, &format!("{f}::Noisy failure"));
        assert_eq!(
            (noisy.outcome, noisy.stdout.as_str()),
            (Outcome::Failed, "to stdout")
        );
        assert!(noisy.stderr.starts_with("to stderr"), "{noisy:?}");
        let done = done_of(&events);
        assert_eq!(
            (done.passed, done.failed + done.errors, done.exit_code),
            (2, 3, Some(1))
        );
    }

    #[test]
    fn behave_missing_is_reported() {
        let Some(py) = python() else { return };
        if behave_available(&py) {
            return;
        }
        let (_dir, root) = fixture_project("behave");
        let mut run = TestRun::start(13, Framework::Behave, &py, &root, &[], &[], &[]).unwrap();
        let events = run_to_end(&mut run);
        assert!(
            events.iter().any(
                |e| matches!(e, TestEvent::Failed(m) if m.starts_with("behave is not installed"))
            ),
            "{events:#?}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, TestEvent::Failed(_)))
                .count(),
            1
        );
    }

    #[test]
    fn failure_location_from_python_traceback() {
        let root = Path::new("/proj");
        let tb = "Traceback (most recent call last):\n  File \"/proj/.venv/lib/python3.12/site-packages/_pytest/runner.py\", line 340, in from_call\n    result = func()\n  File \"/proj/tests/test_a.py\", line 12, in test_x\n    helper()\n  File \"/proj/src/helper.py\", line 4, in helper\n    raise ValueError(\"x\")\nValueError: x\n";
        assert_eq!(
            parse_failure_location(tb, root),
            Some((PathBuf::from("/proj/src/helper.py"), 4))
        );
        let pytest_repr = "    def test_x():\n>       helper()\n\ntests/test_a.py:12: \n_ _ _ _\n\n    def helper():\n>       raise ValueError(\"x\")\nE       ValueError: x\n\nsrc/helper.py:4: ValueError\n";
        assert_eq!(
            parse_failure_location(pytest_repr, root),
            Some((PathBuf::from("/proj/src/helper.py"), 4))
        );
        let lib_only = "  File \"/usr/lib/python3.12/unittest/case.py\", line 58, in testPartExecutor\n    yield\n";
        assert_eq!(parse_failure_location(lib_only, root), None);
        assert_eq!(parse_failure_location("", root), None);
    }

    #[test]
    fn summary_label() {
        let s = Summary {
            passed: 3,
            failed: 1,
            skipped: 1,
            errors: 0,
            duration: 0.123,
            exit_code: Some(1),
        };
        assert_eq!(s.label(), "3 passed, 1 failed, 1 skipped in 0.12s");
        assert_eq!(s.total(), 5);
        assert_eq!(Summary::default().label(), "no tests ran in 0.00s");
    }

    #[test]
    fn a_run_that_ran_nothing_is_not_a_success() {
        let passed = Summary {
            passed: 2,
            skipped: 1,
            exit_code: Some(0),
            ..Default::default()
        };
        assert!(passed.is_success());
        assert!(
            Summary {
                exit_code: None,
                ..passed.clone()
            }
            .is_success(),
            "runners that report no exit code"
        );
        assert!(!Summary {
            failed: 1,
            exit_code: Some(1),
            ..passed.clone()
        }
        .is_success());
        // pytest: "ERROR: not found" (usage error) and "no tests collected".
        assert!(!Summary {
            exit_code: Some(4),
            ..Default::default()
        }
        .is_success());
        assert!(!Summary {
            exit_code: Some(5),
            ..Default::default()
        }
        .is_success());
        assert!(
            !Summary {
                exit_code: Some(0),
                ..Default::default()
            }
            .is_success(),
            "unittest exits 0 when it ran nothing"
        );
        // Interrupted or crashed after some passes.
        assert!(!Summary {
            exit_code: Some(2),
            ..passed
        }
        .is_success());
    }

    /// QA: the gutter ▶ of a method ran `file::method` (no class); pytest
    /// answered "ERROR: not found" and the Tests window showed a green
    /// "no tests ran". The class-qualified id runs the test.
    #[test]
    #[ignore = "requires explicit RUSTPY_REQUIRE_PROVIDER_TESTS=1 and configured pytest/xdist/pytest-cov"]
    fn configured_pytest_xdist_and_cov_use_real_worker_protocol_and_coverage() {
        assert!(
            required_providers(),
            "admit providers with RUSTPY_REQUIRE_PROVIDER_TESTS=1"
        );
        let py = python().unwrap();
        for module in ["pytest", "xdist", "pytest_cov"] {
            require_module(&py, module);
        }
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        std::fs::write(
            root.join("target_module.py"),
            "def twice(value):\n    return value * 2\n",
        )
        .unwrap();
        std::fs::write(
            root.join("test_workers.py"),
            r#"import logging
import pytest
from target_module import twice
@pytest.fixture
def value(request):
    return request.param
@pytest.mark.parametrize('value', [0, 1], indirect=True, ids=['zero', 'one'])
def test_actual(value):
    logging.warning('ACTUAL_WORKER_%s', value)
    assert twice(value) == 0
"#,
        )
        .unwrap();
        let coverage = root.join("coverage.json");
        // Select the installed plugins explicitly, independent of the caller's
        // discovery policy. Disable entry-point discovery to avoid duplicate
        // registration when this configured fixture runs outside Root's harness.
        let options = TestRunOptions {
            env: vec![("PYTEST_DISABLE_PLUGIN_AUTOLOAD".into(), "1".into())],
            ..Default::default()
        };
        let mut run = TestRun::start_with(
            71,
            Framework::Pytest,
            &py,
            &root,
            &["test_workers.py".into()],
            &[
                "-p".into(),
                "xdist.plugin".into(),
                "-p".into(),
                "pytest_cov.plugin".into(),
                "-n".into(),
                "2".into(),
                "--cov=target_module".into(),
                format!("--cov-report=json:{}", coverage.display()),
            ],
            &options,
        )
        .unwrap();
        let events = run_to_end(&mut run);
        let summary = done_of(&events);
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
        for id in [
            "test_workers.py::test_actual[zero]",
            "test_workers.py::test_actual[one]",
        ] {
            assert_started_before_result(&events, id);
            assert!(events.iter().any(|event| matches!(event, TestEvent::Metadata(metadata)
                if metadata.id == id && metadata.worker.as_deref().is_some_and(|worker| worker.starts_with("gw"))
                && metadata.fixtures.iter().any(|(name, _, _)| name == "value"))), "real transported worker metadata missing for {id}: {events:#?}");
        }
        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(coverage).expect("pytest-cov must publish its real report"),
        )
        .unwrap();
        assert!(
            report["files"]
                .as_object()
                .unwrap()
                .iter()
                .any(|(name, file)| name.ends_with("target_module.py")
                    && file["executed_lines"]
                        .as_array()
                        .is_some_and(|lines| lines.contains(&serde_json::json!(2)))),
            "{report}"
        );
    }

    #[test]
    #[ignore = "requires explicit RUSTPY_REQUIRE_PROVIDER_TESTS=1 and configured real tox/virtualenv"]
    fn configured_tox_reports_real_environment_outcomes_and_metadata() {
        assert!(
            required_providers(),
            "admit providers with RUSTPY_REQUIRE_PROVIDER_TESTS=1"
        );
        let py = python().unwrap();
        require_module(&py, "tox");
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        std::fs::write(
            root.join("tox.ini"),
            format!(
                r#"[tox]
env_list = good,bad
skipsdist = true
[testenv]
basepython = {}
skip_install = true
package = skip
download = false
[testenv:good]
commands = python -c "print('ACTUAL_TOX_GOOD')"
[testenv:bad]
commands = python -c "import sys; print('ACTUAL_TOX_BAD'); sys.exit(3)"
"#,
                py.display()
            ),
        )
        .unwrap();
        let mut run = TestRun::start(
            72,
            Framework::Tox,
            &py,
            &root,
            &["good".into(), "bad".into()],
            &[],
            &[],
        )
        .unwrap();
        let events = run_to_end(&mut run);
        assert_eq!(
            result(&events, "tox::good").outcome,
            Outcome::Passed,
            "{events:#?}"
        );
        assert_eq!(
            result(&events, "tox::bad").outcome,
            Outcome::Failed,
            "{events:#?}"
        );
        assert!(result(&events, "tox::good")
            .stdout
            .contains("ACTUAL_TOX_GOOD"));
        assert!(result(&events, "tox::bad")
            .stdout
            .contains("ACTUAL_TOX_BAD"));
        for environment in ["good", "bad"] {
            let id = format!("tox::{environment}");
            assert_started_before_result(&events, &id);
            assert!(events.iter().any(|event| matches!(event, TestEvent::Metadata(metadata) if metadata.id == id && metadata.environment.as_deref() == Some(environment))), "{events:#?}");
        }
        let summary = done_of(&events);
        assert_eq!(
            (summary.passed, summary.failed, summary.exit_code),
            (1, 1, Some(1))
        );
    }

    #[test]
    fn pytest_target_that_is_not_found_fails_the_run() {
        let Some(py) = python() else { return };
        if !provider_available(pytest_available(&py), "pytest", &py) {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        std::fs::write(root.join("test_cls.py"), "import unittest\n\n\nclass TestExtra(unittest.TestCase):\n    def test_passes(self):\n        self.assertEqual(1 + 1, 2)\n").unwrap();
        let mut run = TestRun::start(
            30,
            Framework::Pytest,
            &py,
            &root,
            &["test_cls.py::test_passes".into()],
            &[],
            &[],
        )
        .unwrap();
        let events = run_to_end(&mut run);
        let done = done_of(&events);
        assert_eq!((done.total(), done.exit_code), (0, Some(4)), "{events:#?}");
        assert!(!done.is_success());
        let mut run = TestRun::start(
            31,
            Framework::Pytest,
            &py,
            &root,
            &["test_cls.py::TestExtra::test_passes".into()],
            &[],
            &[],
        )
        .unwrap();
        let events = run_to_end(&mut run);
        assert_eq!(
            results_of(&events),
            vec![(
                "test_cls.py::TestExtra::test_passes".to_string(),
                Outcome::Passed
            )]
        );
        assert!(done_of(&events).is_success(), "{events:#?}");
    }

    #[test]
    #[cfg(unix)] // Unix fixture paths, tools or process semantics
    fn relative_targets() {
        let root = Path::new("/proj");
        assert_eq!(
            relative_target(root, "/proj/tests/test_a.py::TestA::test_b"),
            "tests/test_a.py::TestA::test_b"
        );
        assert_eq!(relative_target(root, "tests/test_a.py"), "tests/test_a.py");
        assert_eq!(
            relative_target(root, "/other/test_a.py"),
            "/other/test_a.py"
        );
        assert_eq!(relative_target(root, "pkg.mod.Case"), "pkg.mod.Case");
    }
    #[test]
    fn doctest_stdlib_results_preserve_failure_diff_selection_and_shared_example_globals() {
        let Some(py) = python() else { return };
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("examples.py"),"def passing():\n    \"\"\"Examples.\n    >>> values = ['π']\n    >>> values[0].upper()\n    'Π'\n    \"\"\"\n\ndef failing():\n    \"\"\"Example.\n    >>> 1 + 1\n    3\n    \"\"\"\n").unwrap();
        let mut run = TestRun::start(
            501,
            Framework::Doctest,
            &py,
            root.path(),
            &["examples.py".into()],
            &[],
            &[],
        )
        .unwrap();
        let events = run_to_end(&mut run);
        assert_eq!(
            result(&events, "examples.py::passing").outcome,
            Outcome::Passed
        );
        let bad = result(&events, "examples.py::failing");
        assert_eq!(bad.outcome, Outcome::Failed);
        assert_eq!(bad.expected.as_deref(), Some("3\n"));
        assert_eq!(bad.actual.as_deref(), Some("2\n"));
        assert_started_before_result(&events, "examples.py::passing");
        assert!(events.iter().any(
            |event| matches!(event,TestEvent::Done(summary)if summary.passed==1&&summary.failed==1)
        ));
        let mut selected = TestRun::start(
            502,
            Framework::Doctest,
            &py,
            root.path(),
            &["examples.py::passing".into()],
            &[],
            &[],
        )
        .unwrap();
        let events = run_to_end(&mut selected);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, TestEvent::Result(_)))
                .count(),
            1
        );
        assert_eq!(
            result(&events, "examples.py::passing").outcome,
            Outcome::Passed
        );
    }
    #[test]
    fn doctest_option_flags_skips_errors_and_package_members_are_real_runner_nodes() {
        let Some(py) = python() else { return };
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("pkg")).unwrap();
        std::fs::write(root.path().join("pkg/__init__.py"), "").unwrap();
        std::fs::write(root.path().join("pkg/examples.py"),"class Sample:\n    def ellipsis(self):\n        \"\"\"Example.\n        >>> print('abcdef')\n        abc...\n        \"\"\"\n    def skipped(self):\n        \"\"\"Example.\n        >>> 1 / 0 # doctest: +SKIP\n        0\n        \"\"\"\n").unwrap();
        let mut run = TestRun::start(
            503,
            Framework::Doctest,
            &py,
            root.path(),
            &["pkg/examples.py".into()],
            &["-o".into(), "ELLIPSIS".into()],
            &[],
        )
        .unwrap();
        let events = run_to_end(&mut run);
        assert_eq!(
            result(&events, "pkg/examples.py::Sample.ellipsis").outcome,
            Outcome::Passed
        );
        assert_eq!(
            result(&events, "pkg/examples.py::Sample.skipped").outcome,
            Outcome::Skipped
        );
        let mut missing = TestRun::start(
            504,
            Framework::Doctest,
            &py,
            root.path(),
            &["pkg/examples.py::missing".into()],
            &[],
            &[],
        )
        .unwrap();
        let events = run_to_end(&mut missing);
        assert!(events
            .iter()
            .any(|event| matches!(event,TestEvent::Done(summary)if summary.errors==1)));
    }
}

/// Provision the same adapter for Debug/Coverage/Profile process launchers.
/// Call only after project trust admission; returns the executable script path.
pub fn provision_reporter(directory: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(directory)?;
    for (name, content) in [
        ("rustpy_test_entry.py", include_str!("test_entry.py")),
        (
            "rustpy_lettuce_reporter.py",
            include_str!("lettuce_reporter.py"),
        ),
        ("rustpy_test_reporter.py", REPORTER),
        (
            "rustpy_doctest_reporter.py",
            include_str!("doctest_reporter.py"),
        ),
        (
            "rustpy_legacy_reporter.py",
            include_str!("legacy_reporter.py"),
        ),
        ("rustpy_pytest_plugin.py", include_str!("pytest_plugin.py")),
    ] {
        let target = directory.join(name);
        if std::fs::read(&target).is_ok_and(|bytes| bytes == content.as_bytes()) {
            continue;
        }
        use std::io::Write;
        let mut staged = tempfile::NamedTempFile::new_in(directory)?;
        staged.write_all(content.as_bytes())?;
        staged.as_file().sync_all()?;
        staged.persist(&target).map_err(|error| error.error)?;
    }
    Ok(directory.join("rustpy_test_entry.py"))
}

#[cfg(test)]
mod framework_protocol_tests {
    use super::*;
    #[test]
    fn pytest_worker_metadata_keeps_param_markers_fixtures_logs_and_hypothesis() {
        let json = r#"{"event":"metadata","id":"tests/test_sample.py::test_value[custom id]","markers":["slow","parametrize"],"fixtures":[["database","tests/conftest.py",12]],"logs":"WARNING captured log","parameter_id":"custom id","worker":"gw1","hypothesis":"Falsifying example: value=0"}"#;
        let Some(TestEvent::Metadata(metadata)) = parse_reporter_event(json, Path::new("/project"))
        else {
            panic!("metadata not decoded")
        };
        assert_eq!(metadata.parameter_id.as_deref(), Some("custom id"));
        assert_eq!(
            metadata.fixtures[0],
            (
                "database".into(),
                PathBuf::from("/project/tests/conftest.py"),
                12
            )
        );
        assert_eq!(metadata.worker.as_deref(), Some("gw1"));
        assert!(metadata.hypothesis.contains("Falsifying example"));
    }
    #[test]
    fn subtest_child_and_tox_environment_records_share_live_tree_protocol() {
        let root = Path::new("/project");
        assert!(matches!(
            parse_reporter_event(
                r#"{"event":"collected","id":"test_a.py::Case::test_one::subTest(i=2)","name":"subTest i=2","line":9}"#,
                root
            ),
            Some(TestEvent::Collected { .. })
        ));
        assert!(
            matches!(parse_reporter_event(r#"{"event":"result","id":"tox::py311","outcome":"failed","message":"environment failed","stdout":"real tox output"}"#,root),Some(TestEvent::Result(result))if result.id=="tox::py311"&&result.outcome==Outcome::Failed)
        );
        assert!(
            matches!(parse_reporter_event(r#"{"event":"metadata","id":"tox::py311","environment":"py311"}"#,root),Some(TestEvent::Metadata(metadata))if metadata.environment.as_deref()==Some("py311"))
        );
    }
}

#[cfg(test)]
mod inheritance_tests {
    use super::*;
    #[test]
    fn historical_run_options_inherit_unless_explicitly_disabled() {
        let old: TestRunOptions =
            serde_json::from_str(r#"{"env":[],"cwd":null,"interpreter_options":[]}"#).unwrap();
        assert!(old.inherit_environment);
        let isolated: TestRunOptions =
            serde_json::from_str(r#"{"inherit_environment":false,"env":[["ONLY_THIS","yes"]]}"#)
                .unwrap();
        assert!(!isolated.inherit_environment);
        assert_eq!(isolated.env, vec![("ONLY_THIS".into(), "yes".into())]);
    }
}

/// Sidecar failure callbacks remain available to native runners and their workers.
pub fn provision_debugger_support(directory: &Path) -> anyhow::Result<()> {
    std::fs::write(
        directory.join("rustpy_debug_failures.py"),
        include_str!("debug_failures.py"),
    )?;
    std::fs::write(
        directory.join("rustpy_debug_test_bootstrap.py"),
        include_str!("debug_test_bootstrap.py"),
    )?;
    std::fs::write(directory.join("rustpy_debug_failure_plugin.py"),"import pytest\nfrom rustpy_debug_failures import debug_failure\n@pytest.hookimpl(tryfirst=True)\ndef pytest_runtest_makereport(item, call):\n    info = call.excinfo\n    if info is not None and call.when == 'call' and not list(item.iter_markers('xfail')) and not isinstance(info.value, (pytest.skip.Exception, pytest.xfail.Exception)):\n        debug_failure((info.type, info.value, info.tb))\n")?;
    std::fs::write(
        directory.join("sitecustomize.py"),
        include_str!("debug_sitecustomize.py"),
    )?;
    Ok(())
}
