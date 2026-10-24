//! "Profile": runs a configuration under `cProfile` through an embedded runner
//! script and parses the statistics (roadmap §3.4).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::streamed::{PythonCommand, Streamed};
use rustpy_project::config::RunConfig;
use rustpy_project::interpreter::Interpreter;

/// The embedded runner (`python profile_runner.py [-m] target args…`).
pub const RUNNER_SOURCE: &str = include_str!("profile_runner.py");

/// Prefix of the JSON lines the runner prints after the program finished.
pub const PROFILE_PREFIX: &str = "##rustpy-profile## ";

/// One `caller → callee` edge as recorded by cProfile.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CallerRef {
    pub file: String,
    pub line: usize,
    pub name: String,
    pub ncalls: u64,
    pub primitive_calls: u64,
    /// Time spent in the callee when called from this caller.
    pub tottime: f64,
    pub cumtime: f64,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FunctionStats {
    /// Source file, or `~` for built-in functions.
    pub file: String,
    /// 1-based line of the `def` (0 for built-ins).
    pub line: usize,
    pub name: String,
    pub ncalls: u64,
    pub primitive_calls: u64,
    /// Time inside the function excluding sub-calls (seconds).
    pub tottime: f64,
    /// Time including sub-calls (seconds).
    pub cumtime: f64,
    pub callers: Vec<CallerRef>,
}

impl FunctionStats {
    pub fn is_builtin(&self) -> bool {
        self.file == "~" || self.file.starts_with('<') && self.file != "<stdin>"
    }

    /// `name  (file:line)` for tables.
    pub fn display_name(&self) -> String {
        if self.is_builtin() {
            self.name.clone()
        } else {
            let file = Path::new(&self.file)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.file.clone());
            format!("{}  ({file}:{})", self.name, self.line)
        }
    }

    /// Average `tottime` per call.
    pub fn per_call(&self) -> f64 {
        if self.ncalls == 0 {
            0.0
        } else {
            self.tottime / self.ncalls as f64
        }
    }

    /// Average `cumtime` per primitive call.
    pub fn cum_per_call(&self) -> f64 {
        if self.primitive_calls == 0 {
            0.0
        } else {
            self.cumtime / self.primitive_calls as f64
        }
    }

    fn key(&self) -> (&str, usize, &str) {
        (self.file.as_str(), self.line, self.name.as_str())
    }
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ProfileReport {
    pub sampling: bool,
    pub raw_file: Option<PathBuf>,
    pub pstat_file: Option<PathBuf>,
    pub engine: String,
    pub threads: Vec<ThreadStats>,
    pub functions: Vec<FunctionStats>,
    /// Sum of `tottime` over all functions (seconds).
    pub total_time: f64,
    pub total_calls: u64,
    /// Wall-clock time of the profiled program (seconds).
    pub elapsed: f64,
    pub exit_code: Option<i32>,
}
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadStats {
    pub id: u64,
    pub name: String,
    pub total_time: f64,
    pub schedules: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileOptions {
    pub engine: String,
    pub clock: String,
    pub threads: bool,
    pub builtins: bool,
}
impl Default for ProfileOptions {
    fn default() -> Self {
        Self {
            engine: "cprofile".into(),
            clock: "wall".into(),
            threads: true,
            builtins: true,
        }
    }
}

impl ProfileReport {
    /// Function indices sorted by own time, largest first.
    pub fn sorted_by_tottime(&self) -> Vec<usize> {
        let mut idx: Vec<usize> = (0..self.functions.len()).collect();
        idx.sort_by(|a, b| {
            self.functions[*b]
                .tottime
                .partial_cmp(&self.functions[*a].tottime)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        idx
    }

    /// Function indices sorted by cumulative time, largest first.
    pub fn sorted_by_cumtime(&self) -> Vec<usize> {
        let mut idx: Vec<usize> = (0..self.functions.len()).collect();
        idx.sort_by(|a, b| {
            self.functions[*b]
                .cumtime
                .partial_cmp(&self.functions[*a].cumtime)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        idx
    }

    /// Index of the function with this cProfile key.
    pub fn find(&self, file: &str, line: usize, name: &str) -> Option<usize> {
        self.functions
            .iter()
            .position(|f| f.key() == (file, line, name))
    }

    /// `(caller index, calls)` for the function at `index`, most calls first.
    pub fn callers_of(&self, index: usize) -> Vec<(usize, u64)> {
        let Some(f) = self.functions.get(index) else {
            return Vec::new();
        };
        let mut out: Vec<(usize, u64)> = f
            .callers
            .iter()
            .filter_map(|c| self.find(&c.file, c.line, &c.name).map(|i| (i, c.ncalls)))
            .collect();
        out.sort_by_key(|a| std::cmp::Reverse(a.1));
        out
    }

    /// `(callee index, calls)` for the function at `index` (derived from the callers table).
    pub fn callees_of(&self, index: usize) -> Vec<(usize, u64)> {
        let Some(f) = self.functions.get(index) else {
            return Vec::new();
        };
        let key = f.key();
        let mut out: Vec<(usize, u64)> = self
            .functions
            .iter()
            .enumerate()
            .filter_map(|(i, g)| {
                g.callers
                    .iter()
                    .find(|c| (c.file.as_str(), c.line, c.name.as_str()) == key)
                    .map(|c| (i, c.ncalls))
            })
            .collect();
        out.sort_by_key(|a| std::cmp::Reverse(a.1));
        out
    }

    /// Indices of functions defined under `root` (project code), by own time.
    pub fn project_functions(&self, root: &Path) -> Vec<usize> {
        self.sorted_by_tottime()
            .into_iter()
            .filter(|i| Path::new(&self.functions[*i].file).starts_with(root))
            .collect()
    }
}

/// Parses the runner's `##rustpy-profile##` JSON lines (other lines are ignored).
pub fn load_json_lines(text: &str) -> anyhow::Result<ProfileReport> {
    let mut report = ProfileReport::default();
    let mut seen: HashMap<(String, usize, String), usize> = HashMap::new();
    let mut saw_summary = false;
    for line in text.lines() {
        let Some(json) = line.trim_start().strip_prefix(PROFILE_PREFIX.trim_end()) else {
            continue;
        };
        let v: serde_json::Value = serde_json::from_str(json.trim())?;
        if v.get("event").and_then(|e| e.as_str()) == Some("summary") {
            report.engine = v["engine"].as_str().unwrap_or("cprofile").into();
            report.sampling = v["sampling"].as_bool().unwrap_or(false);
            report.threads = serde_json::from_value(v["threads"].clone()).unwrap_or_default();
            report.total_time = v["total_time"].as_f64().unwrap_or(0.0);
            report.total_calls = v["total_calls"].as_u64().unwrap_or(0);
            report.elapsed = v["elapsed"].as_f64().unwrap_or(0.0);
            report.exit_code = v["exit_code"].as_i64().map(|c| c as i32);
            saw_summary = true;
            continue;
        }
        let callers = v["callers"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|c| CallerRef {
                        file: c["file"].as_str().unwrap_or("").to_string(),
                        line: c["line"].as_u64().unwrap_or(0) as usize,
                        name: c["name"].as_str().unwrap_or("").to_string(),
                        ncalls: c["ncalls"].as_u64().unwrap_or(0),
                        primitive_calls: c["primitive_calls"].as_u64().unwrap_or(0),
                        tottime: c["tottime"].as_f64().unwrap_or(0.0),
                        cumtime: c["cumtime"].as_f64().unwrap_or(0.0),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let f = FunctionStats {
            file: v["file"].as_str().unwrap_or("").to_string(),
            line: v["line"].as_u64().unwrap_or(0) as usize,
            name: v["name"].as_str().unwrap_or("").to_string(),
            ncalls: v["ncalls"].as_u64().unwrap_or(0),
            primitive_calls: v["primitive_calls"].as_u64().unwrap_or(0),
            tottime: v["tottime"].as_f64().unwrap_or(0.0),
            cumtime: v["cumtime"].as_f64().unwrap_or(0.0),
            callers,
        };
        let key = (f.file.clone(), f.line, f.name.clone());
        if let Some(&i) = seen.get(&key) {
            report.functions[i] = f;
        } else {
            seen.insert(key, report.functions.len());
            report.functions.push(f);
        }
    }
    if report.functions.is_empty() {
        anyhow::bail!("no profile data found in the output");
    }
    if !saw_summary {
        report.total_time = report.functions.iter().map(|f| f.tottime).sum();
        report.total_calls = report.functions.iter().map(|f| f.ncalls).sum();
    }
    Ok(report)
}

/// A run under the cProfile runner; the report becomes available after the process exits.
pub struct ProfileRun {
    pstat_file: PathBuf,
    pub id: u64,
    pub name: String,
    pub command_line: String,
    process: Streamed,
    runner_file: PathBuf,
    profile_lines: String,
    collecting_profile_line: bool,
    stdout_line_start: bool,
    report: Option<ProfileReport>,
    error: Option<String>,
    exit_code: Option<i32>,
    finished: bool,
}

/// Starts `python <runner> [-m] <target> args…` for the configuration.
pub fn run_with_profiler(
    id: u64,
    cfg: &RunConfig,
    interpreter: Option<&Interpreter>,
    project_root: &Path,
) -> anyhow::Result<ProfileRun> {
    run_with_profiler_opts(
        id,
        cfg,
        interpreter,
        project_root,
        &ProfileOptions::default(),
    )
}
pub fn run_with_profiler_opts(
    id: u64,
    cfg: &RunConfig,
    interpreter: Option<&Interpreter>,
    project_root: &Path,
    options: &ProfileOptions,
) -> anyhow::Result<ProfileRun> {
    anyhow::ensure!(
        matches!(options.engine.as_str(), "cprofile" | "yappi" | "vmprof"),
        "Choose cProfile, yappi or vmprof"
    );
    let cmd = PythonCommand::for_config(cfg, interpreter, project_root, "Profile")?;
    let directory = project_root.join(".rustpy/profiles");
    std::fs::create_dir_all(&directory)?;
    let runner_file = std::env::temp_dir().join(format!(
        "rustpy-profile-runner-{}-{}.py",
        std::process::id(),
        id
    ));
    std::fs::write(&runner_file, RUNNER_SOURCE)
        .map_err(|e| anyhow::anyhow!("Cannot write the profiler runner: {e}"))?;
    let inserted = vec![runner_file.to_string_lossy().into_owned()];
    let mut args = cmd.interpreter_options.clone();
    args.extend(inserted.iter().cloned());
    args.extend(cmd.target.iter().cloned());
    let command_line = cmd.command_line(&inserted);
    let mut pstat_file = directory.join(format!(
        "session-{id}-{}.pstat",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    if options.engine == "vmprof" {
        pstat_file.set_extension("vmprof");
    }
    let mut env = cmd.env.clone();
    env.extend([
        ("RUSTPY_PROFILE_ENGINE".into(), options.engine.clone()),
        ("RUSTPY_PROFILE_CLOCK".into(), options.clock.clone()),
        (
            "RUSTPY_PROFILE_THREADS".into(),
            if options.threads { "1" } else { "0" }.into(),
        ),
        (
            "RUSTPY_PROFILE_BUILTINS".into(),
            if options.builtins { "1" } else { "0" }.into(),
        ),
        (
            "RUSTPY_PROFILE_PSTAT".into(),
            pstat_file.display().to_string(),
        ),
    ]);
    let process = match Streamed::spawn_with_environment(
        &cmd.python,
        &args,
        &cmd.cwd,
        &env,
        cmd.stdin(),
        cmd.inherit_environment,
    ) {
        Ok(p) => p,
        Err(e) => {
            let _ = std::fs::remove_file(&runner_file);
            return Err(e);
        }
    };
    Ok(ProfileRun {
        pstat_file,
        id,
        name: cfg.name.clone(),
        command_line,
        process,
        runner_file,
        profile_lines: String::new(),
        collecting_profile_line: false,
        stdout_line_start: true,
        report: None,
        error: None,
        exit_code: None,
        finished: false,
    })
}

impl ProfileRun {
    /// New program output lines as `(text, is_stderr)`; profile lines are consumed.
    pub fn poll(&mut self) -> Vec<(String, bool)> {
        let mut out = Vec::new();
        for chunk in self.process.poll_chunks() {
            let profile = !chunk.is_stderr
                && (self.collecting_profile_line
                    || (self.stdout_line_start
                        && chunk.text.starts_with(PROFILE_PREFIX.trim_end())));
            if !chunk.is_stderr {
                // Streamed may split an ordinary physical line at 64 KiB.
                // A prefix at the next chunk boundary is still program text.
                // Stderr is a separate stream and cannot advance stdout state.
                self.stdout_line_start = chunk.line_end;
                self.collecting_profile_line = profile && !chunk.line_end;
            }
            if profile {
                self.profile_lines.push_str(&chunk.text);
                if chunk.line_end {
                    self.profile_lines.push('\n');
                }
            } else {
                out.push((chunk.text, chunk.is_stderr));
            }
        }
        if !self.finished {
            if let Some(exit) = self.process.exit().cloned() {
                self.finished = true;
                self.exit_code = exit.code();
                let _ = std::fs::remove_file(&self.runner_file);
                match load_json_lines(&self.profile_lines) {
                    Ok(mut r) => {
                        r.raw_file = self.pstat_file.is_file().then(|| self.pstat_file.clone());
                        if !r.sampling {
                            r.pstat_file = r.raw_file.clone();
                        }
                        out.push((
                            format!(
                                "Profile: {} function(s), {} counts, {:.3}s total",
                                r.functions.len(),
                                r.total_calls,
                                r.total_time
                            ),
                            false,
                        ));
                        self.report = Some(r);
                    }
                    Err(e) => {
                        let msg = if exit.code().is_none() {
                            "Process was stopped before profile data could be collected".to_string()
                        } else {
                            format!("No profile data: {e}")
                        };
                        out.push((msg.clone(), true));
                        self.error = Some(msg);
                    }
                }
            }
        }
        out
    }

    pub fn is_running(&self) -> bool {
        !self.finished
    }

    pub fn stop(&mut self) {
        self.process.stop();
    }
    pub fn finish_and_collect(&mut self) {
        self.process.interrupt();
    }

    pub fn report(&self) -> Option<&ProfileReport> {
        self.report.as_ref()
    }

    /// Exit code of the program (`None` while running or when killed).
    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn elapsed(&self) -> Duration {
        self.process.elapsed()
    }
}

impl Drop for ProfileRun {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.runner_file);
    }
}

#[cfg(test)]
mod protocol_framing_tests {
    use super::*;
    use std::io::Write;
    use std::process::Stdio;
    use std::time::Instant;

    #[test]
    fn profile_protocol_child_fixture() {
        if std::env::var("RUSTPY_PROFILE_FRAMING_CHILD").as_deref() != Ok("1") {
            return;
        }
        let callers: Vec<_> = (0..1024)
            .map(|index| {
                serde_json::json!({
                    "file": format!("/profile/callerπ{index}.py"),
                    "line": index + 1,
                    "name": format!("callerπ{index}"),
                    "ncalls": 1, "primitive_calls": 1, "tottime": 0.0001, "cumtime": 0.0001,
                })
            })
            .collect();
        let frame = format!(
            "{PROFILE_PREFIX}{}",
            serde_json::json!({
                "file": "/profile/hot.py", "line": 11, "name": "hotπ",
                "ncalls": 1024, "primitive_calls": 1024,
                "tottime": 0.2, "cumtime": 0.3, "callers": callers,
            })
        );
        assert!(frame.len() > 128 * 1024);
        // Separate the test harness prefix from program stdout, then physically
        // split one protocol line around stderr while retaining its newline.
        println!("\nPROFILE_PROGRAM_BEFORE");
        let mut split = 80 * 1024;
        while !frame.is_char_boundary(split) {
            split -= 1;
        }
        let mut stdout = std::io::stdout().lock();
        let ordinary_head = "A".repeat(64 * 1024);
        let ordinary_tail = format!("{PROFILE_PREFIX}ordinary continuation π");
        stdout.write_all(ordinary_head.as_bytes()).unwrap();
        stdout.write_all(ordinary_tail.as_bytes()).unwrap();
        stdout.flush().unwrap();
        // Wait for the parent to observe the exact first bounded chunk, so
        // scheduling cannot coalesce the continuation into a newline frame.
        let acknowledgement = std::path::Path::new("ordinary-chunk-ack");
        let deadline = Instant::now() + Duration::from_secs(3);
        while !acknowledgement.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            acknowledgement.exists(),
            "parent must observe ordinary chunk"
        );
        eprintln!("PROFILE_STDERR_WITHIN_ORDINARY_LINE");
        writeln!(stdout).unwrap();
        stdout.write_all(&frame.as_bytes()[..split]).unwrap();
        stdout.flush().unwrap();
        eprintln!("PROFILE_PROGRAM_STDERR_BETWEEN_CHUNKS");
        std::thread::sleep(Duration::from_millis(30));
        stdout.write_all(&frame.as_bytes()[split..]).unwrap();
        writeln!(stdout).unwrap();
        writeln!(
            stdout,
            "{PROFILE_PREFIX}{}",
            serde_json::json!({
                "event": "summary", "engine": "cprofile", "exit_code": 0,
                "total_time": 0.2, "total_calls": 1024,
            })
        )
        .unwrap();
        writeln!(stdout, "{{\"ordinary_stdout\":true}}").unwrap();
        writeln!(stdout, "PROFILE_PROGRAM_AFTER").unwrap();
        stdout.flush().unwrap();
    }

    #[test]
    fn long_caller_frame_keeps_physical_lines_and_stderr_without_console_tails() {
        let root = tempfile::tempdir().unwrap();
        let process = Streamed::spawn_with_environment(
            &std::env::current_exe().unwrap(),
            &[
                "--exact".into(),
                "profile::protocol_framing_tests::profile_protocol_child_fixture".into(),
                "--nocapture".into(),
                "--test-threads=1".into(),
            ],
            root.path(),
            &std::collections::BTreeMap::from([(
                "RUSTPY_PROFILE_FRAMING_CHILD".into(),
                "1".into(),
            )]),
            Stdio::null(),
            true,
        )
        .unwrap();
        let mut run = ProfileRun {
            id: 73900,
            name: "long protocol frame".into(),
            command_line: "current test executable".into(),
            process,
            runner_file: root.path().join("unused-runner.py"),
            pstat_file: root.path().join("unused.pstat"),
            profile_lines: String::new(),
            collecting_profile_line: false,
            stdout_line_start: true,
            report: None,
            error: None,
            exit_code: None,
            finished: false,
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut output = Vec::new();
        let ordinary_head = "A".repeat(64 * 1024);
        let ordinary_tail = format!("{PROFILE_PREFIX}ordinary continuation π");
        while run.is_running() && Instant::now() < deadline {
            output.extend(run.poll());
            if output
                .iter()
                .any(|(line, stderr)| !stderr && line == &ordinary_head)
            {
                std::fs::write(root.path().join("ordinary-chunk-ack"), "observed").unwrap();
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if run.is_running() {
            run.stop();
            panic!("framing fixture did not retire: {output:?}");
        }
        output.extend(run.poll());
        assert_eq!(run.exit_code(), Some(0), "{output:?}");
        assert!(run.error().is_none(), "{:?}: {output:?}", run.error());
        let report = run.report().expect("complete long-frame report");
        assert_eq!(report.exit_code, Some(0));
        assert_eq!(report.engine, "cprofile");
        assert_eq!(report.functions.len(), 1);
        let hot = &report.functions[0];
        assert_eq!(hot.name, "hotπ");
        assert_eq!(hot.ncalls, 1024);
        assert_eq!(hot.callers.len(), 1024);
        assert_eq!(hot.callers[0].name, "callerπ0");
        assert_eq!(hot.callers[1023].name, "callerπ1023");
        assert_eq!(
            output
                .iter()
                .filter(|(line, stderr)| !stderr && line == &ordinary_head)
                .count(),
            1,
            "the ordinary first bounded chunk is preserved exactly once"
        );
        assert_eq!(
            output
                .iter()
                .filter(|(line, stderr)| !stderr && line == &ordinary_tail)
                .count(),
            1,
            "a profile-looking continuation remains exact program stdout"
        );
        for (text, stderr) in [
            ("PROFILE_STDERR_WITHIN_ORDINARY_LINE", true),
            ("PROFILE_PROGRAM_BEFORE", false),
            ("PROFILE_PROGRAM_STDERR_BETWEEN_CHUNKS", true),
            ("{\"ordinary_stdout\":true}", false),
            ("PROFILE_PROGRAM_AFTER", false),
        ] {
            assert!(
                output.iter().any(|line| line == &(text.into(), stderr)),
                "missing forwarded program output {text}: {output:?}"
            );
        }
        assert!(
            !output.iter().any(|(line, _)| line != &ordinary_tail
                && (line.contains("callerπ") || line.contains(PROFILE_PREFIX.trim_end()))),
            "no protocol chunk or continuation may leak to the console: {output:?}"
        );
    }
}
