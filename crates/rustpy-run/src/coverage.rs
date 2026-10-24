//! "Run with Coverage": runs a configuration under `coverage.py` and parses
//! the JSON report into per-file line states (roadmap §3.4). Branch coverage
//! marks partially covered lines; existing `.coverage`, `coverage.json` and
//! Cobertura XML reports can be imported, and an HTML report generated.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::streamed::{run_quiet, PythonCommand, Streamed};
use rustpy_project::config::{RunConfig, RunConfigKind};
use rustpy_project::interpreter::Interpreter;

/// True when `import coverage` works for the interpreter.
pub fn coverage_available(interpreter: &Path) -> bool {
    run_quiet(
        interpreter,
        &["-c", "import coverage"],
        None,
        Duration::from_secs(15),
    )
    .is_some()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineCoverage {
    Covered,
    /// Executed, but at least one branch from the line never ran (branch coverage).
    Partial,
    Missed,
    Excluded,
}

/// Coverage of one source file (line numbers are 1-based and sorted).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct FileCoverage {
    pub path: PathBuf,
    pub executed: Vec<usize>,
    pub missing: Vec<usize>,
    pub excluded: Vec<usize>,
    /// Executed lines with a branch that never ran (subset of `executed`).
    pub partial: Vec<usize>,
    /// Branches (arcs between lines) measured; 0 without branch coverage.
    pub num_branches: usize,
    pub covered_branches: usize,
    /// coverage.py's figure: statements and branches together when branches are measured.
    pub percent: f32,
    pub executed_arcs: Vec<(i64, i64)>,
    pub missing_arcs: Vec<(i64, i64)>,
}

impl FileCoverage {
    /// State of a 1-based line; `None` for lines that are not statements.
    pub fn line_state(&self, line: usize) -> Option<LineCoverage> {
        if self.partial.binary_search(&line).is_ok() {
            Some(LineCoverage::Partial)
        } else if self.executed.binary_search(&line).is_ok() {
            Some(LineCoverage::Covered)
        } else if self.missing.binary_search(&line).is_ok() {
            Some(LineCoverage::Missed)
        } else if self.excluded.binary_search(&line).is_ok() {
            Some(LineCoverage::Excluded)
        } else {
            None
        }
    }

    /// Number of measured statements (covered + missed).
    pub fn statements(&self) -> usize {
        self.executed.len() + self.missing.len()
    }

    /// Covered statements in percent (100 for a file without statements).
    pub fn line_percent(&self) -> f32 {
        percent_of(self.executed.len(), self.statements())
    }

    /// Covered branches in percent; `None` when no branches were measured.
    pub fn branch_percent(&self) -> Option<f32> {
        (self.num_branches > 0).then(|| percent_of(self.covered_branches, self.num_branches))
    }
}

/// `part` of `total` in percent; 100 when there is nothing to cover.
pub fn percent_of(part: usize, total: usize) -> f32 {
    if total == 0 {
        100.0
    } else {
        part as f32 * 100.0 / total as f32
    }
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct CoverageReport {
    pub files: Vec<FileCoverage>,
    pub total_percent: f32,
    /// Branch coverage was measured.
    pub branch: bool,
    /// The coverage.py data file behind the report (for the HTML report), if kept.
    pub data_file: Option<PathBuf>,
    /// Configuration file passed to coverage.py (`--rcfile`) for the run.
    pub rcfile: Option<PathBuf>,
}

impl CoverageReport {
    /// Coverage for an absolute path.
    pub fn file(&self, path: &Path) -> Option<&FileCoverage> {
        self.files.iter().find(|f| f.path == path)
    }

    /// `(covered, statements)` over all files.
    pub fn totals(&self) -> (usize, usize) {
        self.files.iter().fold((0, 0), |(c, s), f| {
            (c + f.executed.len(), s + f.statements())
        })
    }

    /// `(covered, total)` branches over all files.
    pub fn branch_totals(&self) -> (usize, usize) {
        self.files.iter().fold((0, 0), |(c, t), f| {
            (c + f.covered_branches, t + f.num_branches)
        })
    }
}

fn sorted_lines(v: &serde_json::Value) -> Vec<usize> {
    let mut out: Vec<usize> = v
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|n| n.as_u64())
                .map(|n| n as usize)
                .collect()
        })
        .unwrap_or_default();
    out.sort_unstable();
    out.dedup();
    out
}

/// Source lines of the `[from, to]` arcs in a `missing_branches` list.
fn arc_sources(v: &serde_json::Value) -> Vec<usize> {
    let mut out: Vec<usize> = v
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|arc| arc.get(0).and_then(|n| n.as_i64()))
                .filter(|n| *n > 0)
                .map(|n| n as usize)
                .collect()
        })
        .unwrap_or_default();
    out.sort_unstable();
    out.dedup();
    out
}

/// Parses `coverage json` output; relative file names are resolved against `root`.
pub fn load_json(text: &str, root: &Path) -> anyhow::Result<CoverageReport> {
    let v: serde_json::Value = serde_json::from_str(text)?;
    if v["rustpy_coverage"].as_u64() == Some(1) {
        return Ok(serde_json::from_value(v["report"].clone())?);
    }
    let mut files = Vec::new();
    let count = |entry: &serde_json::Value, key: &str| {
        entry
            .pointer(&format!("/summary/{key}"))
            .and_then(|n| n.as_u64())
            .map(|n| n as usize)
    };
    if let Some(map) = v.get("files").and_then(|f| f.as_object()) {
        for (name, entry) in map {
            let p = PathBuf::from(name);
            let path = if p.is_absolute() { p } else { root.join(p) };
            let percent = entry
                .pointer("/summary/percent_covered")
                .and_then(|p| p.as_f64())
                .unwrap_or(0.0) as f32;
            let executed = sorted_lines(&entry["executed_lines"]);
            // A partially covered line ran, but one of its branches never did.
            let partial: Vec<usize> = arc_sources(&entry["missing_branches"])
                .into_iter()
                .filter(|l| executed.binary_search(l).is_ok())
                .collect();
            let arcs = |key: &str| {
                entry
                    .get(key)
                    .and_then(|a| a.as_array())
                    .map_or(0, Vec::len)
            };
            let num_branches = count(entry, "num_branches")
                .unwrap_or_else(|| arcs("executed_branches") + arcs("missing_branches"));
            let covered_branches =
                count(entry, "covered_branches").unwrap_or_else(|| arcs("executed_branches"));
            files.push(FileCoverage {
                path,
                executed,
                missing: sorted_lines(&entry["missing_lines"]),
                excluded: sorted_lines(&entry["excluded_lines"]),
                partial,
                num_branches,
                covered_branches,
                percent,
                executed_arcs: serde_json::from_value(entry["executed_branches"].clone())
                    .unwrap_or_default(),
                missing_arcs: serde_json::from_value(entry["missing_branches"].clone())
                    .unwrap_or_default(),
            });
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let total_percent = v
        .pointer("/totals/percent_covered")
        .and_then(|p| p.as_f64())
        .unwrap_or(0.0) as f32;
    let branch = v
        .pointer("/meta/branch_coverage")
        .and_then(|b| b.as_bool())
        .unwrap_or_else(|| files.iter().any(|f| f.num_branches > 0));
    Ok(CoverageReport {
        files,
        total_percent,
        branch,
        data_file: None,
        rcfile: None,
    })
}

/// Parses a Cobertura XML report (`coverage xml`, `pytest --cov-report=xml`).
/// File names are resolved against the report's `<source>` directories, then `root`.
pub fn load_cobertura_xml(text: &str, root: &Path) -> anyhow::Result<CoverageReport> {
    let attr_re = regex::Regex::new(r#"([\w:-]+)\s*=\s*"([^"]*)""#)?;
    let attr = |tag: &str, name: &str| -> Option<String> {
        attr_re
            .captures_iter(tag)
            .find(|c| &c[1] == name)
            .map(|c| unescape_xml(&c[2]))
    };
    if !text.contains("<coverage") {
        anyhow::bail!("not a Cobertura coverage report");
    }
    let sources: Vec<PathBuf> = regex::Regex::new(r"<source>([^<]*)</source>")?
        .captures_iter(text)
        .map(|c| PathBuf::from(unescape_xml(c[1].trim())))
        .collect();
    let class_re = regex::Regex::new(r"(?s)<class\b([^>]*)>(.*?)</class>")?;
    let line_re = regex::Regex::new(r"<line\b([^>]*)/?>")?;
    let cond_re = regex::Regex::new(r"\((\d+)/(\d+)\)")?;
    let mut by_path: std::collections::BTreeMap<PathBuf, FileCoverage> =
        std::collections::BTreeMap::new();
    for class in class_re.captures_iter(text) {
        let Some(name) = attr(&class[1], "filename") else {
            continue;
        };
        let rel = PathBuf::from(&name);
        let path = if rel.is_absolute() {
            rel
        } else {
            sources
                .iter()
                .map(|s| s.join(&rel))
                .find(|p| p.exists())
                .or_else(|| {
                    sources
                        .first()
                        .map(|s| s.join(&rel))
                        .filter(|p| p.is_absolute())
                })
                .unwrap_or_else(|| root.join(&rel))
        };
        let f = by_path.entry(path.clone()).or_insert_with(|| FileCoverage {
            path,
            ..Default::default()
        });
        for line in line_re.captures_iter(&class[2]) {
            let tag = &line[1];
            let Some(number) = attr(tag, "number").and_then(|n| n.parse::<usize>().ok()) else {
                continue;
            };
            let hits = attr(tag, "hits")
                .and_then(|h| h.parse::<u64>().ok())
                .unwrap_or(0);
            if hits > 0 {
                f.executed.push(number);
            } else {
                f.missing.push(number);
            }
            if attr(tag, "branch").as_deref() == Some("true") {
                if let Some(c) = attr(tag, "condition-coverage").and_then(|cc| {
                    cond_re.captures(&cc).map(|c| {
                        (
                            c[1].parse::<usize>().unwrap_or(0),
                            c[2].parse::<usize>().unwrap_or(0),
                        )
                    })
                }) {
                    f.covered_branches += c.0;
                    f.num_branches += c.1;
                    if hits > 0 && c.0 < c.1 {
                        f.partial.push(number);
                    }
                }
            }
        }
    }
    let mut files: Vec<FileCoverage> = by_path.into_values().collect();
    for f in &mut files {
        for v in [&mut f.executed, &mut f.missing, &mut f.partial] {
            v.sort_unstable();
            v.dedup();
        }
        let missing: Vec<usize> = f
            .missing
            .iter()
            .copied()
            .filter(|l| f.executed.binary_search(l).is_err())
            .collect();
        f.missing = missing;
        f.percent = percent_of(
            f.executed.len() + f.covered_branches,
            f.statements() + f.num_branches,
        );
    }
    let branch = files.iter().any(|f| f.num_branches > 0);
    let (covered, statements) = files.iter().fold((0, 0), |(c, s), f| {
        (
            c + f.executed.len() + f.covered_branches,
            s + f.statements() + f.num_branches,
        )
    });
    Ok(CoverageReport {
        total_percent: percent_of(covered, statements),
        files,
        branch,
        data_file: None,
        rcfile: None,
    })
}

fn unescape_xml(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Loads a coverage report from `path`: `coverage json` output (`.json`),
/// Cobertura XML (`.xml`) or a coverage.py data file (`.coverage`, any other
/// name), which `python -m coverage json` converts. Relative file names are
/// resolved against `root` (or the report's directory when that fits better).
pub fn import_report(path: &Path, root: &Path, python: &Path) -> anyhow::Result<CoverageReport> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let base_dir = path.parent().unwrap_or(root);
    let pick_base =
        |load: &dyn Fn(&Path) -> anyhow::Result<CoverageReport>| -> anyhow::Result<CoverageReport> {
            let in_root = load(root)?;
            if base_dir == root {
                return Ok(in_root);
            }
            let in_dir = load(base_dir)?;
            let existing = |r: &CoverageReport| r.files.iter().filter(|f| f.path.exists()).count();
            Ok(if existing(&in_dir) > existing(&in_root) {
                in_dir
            } else {
                in_root
            })
        };
    match ext.as_str() {
        "json" => {
            let text = std::fs::read_to_string(path)?;
            pick_base(&|base| load_json(&text, base))
        }
        "xml" => {
            let text = std::fs::read_to_string(path)?;
            pick_base(&|base| load_cobertura_xml(&text, base))
        }
        _ => {
            if !path.is_file() {
                anyhow::bail!("{} does not exist", path.display());
            }
            let json_file = std::env::temp_dir().join(format!(
                "rustpy-coverage-import-{}-{}.json",
                std::process::id(),
                IMPORT_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            let result = run_coverage(
                python,
                &[
                    "json".into(),
                    format!("--data-file={}", path.display()),
                    "-o".into(),
                    json_file.to_string_lossy().into_owned(),
                    "-q".into(),
                    "--ignore-errors".into(),
                ],
                root,
                Duration::from_secs(120),
            );
            let text = std::fs::read_to_string(&json_file);
            let _ = std::fs::remove_file(&json_file);
            result.map_err(|e| {
                anyhow::anyhow!(
                    "{} is not a readable coverage data file: {e}",
                    path.display()
                )
            })?;
            let mut report = load_json(&text?, root)?;
            report.data_file = Some(path.to_path_buf());
            Ok(report)
        }
    }
}

static IMPORT_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Writes coverage.py's HTML report for `data_file` into `out_dir`; returns `index.html`.
pub fn html_report(
    python: &Path,
    data_file: &Path,
    rcfile: Option<&Path>,
    out_dir: &Path,
    cwd: &Path,
) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(out_dir)?;
    let mut args = vec![
        "html".to_string(),
        format!("--data-file={}", data_file.display()),
        "-d".into(),
        out_dir.to_string_lossy().into_owned(),
        "-q".into(),
        "--ignore-errors".into(),
    ];
    if let Some(rc) = rcfile {
        args.push(format!("--rcfile={}", rc.display()));
    }
    run_coverage(python, &args, cwd, Duration::from_secs(300))
        .map_err(|e| anyhow::anyhow!("coverage html failed: {e}"))?;
    let index = out_dir.join("index.html");
    if !index.is_file() {
        anyhow::bail!("coverage html wrote no {}", index.display());
    }
    Ok(index)
}

/// Runs `python -m coverage <args>`; the error carries coverage's message.
fn run_coverage(
    python: &Path,
    args: &[String],
    cwd: &Path,
    timeout: Duration,
) -> Result<(), String> {
    run_coverage_with(python, args, cwd, timeout, &CoverageOptions::default())
}

fn write_bundle(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if std::fs::metadata(path).is_ok_and(|metadata| metadata.len() == bytes.len() as u64) {
        return Ok(());
    }
    let stage = path.with_extension(format!(
        "{}-{}.tmp",
        std::process::id(),
        IMPORT_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::write(&stage, bytes)?;
    if let Err(error) = std::fs::rename(&stage, path) {
        let _ = std::fs::remove_file(&stage);
        if !std::fs::metadata(path).is_ok_and(|metadata| metadata.len() == bytes.len() as u64) {
            return Err(error.into());
        }
    }
    Ok(())
}
fn coverage_prefix(
    python: &Path,
    cwd: &Path,
    options: &CoverageOptions,
) -> anyhow::Result<(PathBuf, Vec<String>)> {
    if let Some(executable) = &options.executable {
        if !executable.as_os_str().is_empty() {
            return Ok((executable.clone(), Vec::new()));
        }
    }
    if options.bundled {
        let directory = cwd.join(".rustpy/coverage/runner");
        std::fs::create_dir_all(&directory)?;
        let archive = directory.join("coverage-7.6.1.tar.gz");
        let tomli = directory.join("tomli-2.0.1.tar.gz");
        write_bundle(&tomli, include_bytes!("coverage_vendor/tomli-2.0.1.tar.gz"))?;
        let bytes = include_bytes!("coverage_vendor/coverage-7.6.1.tar.gz");
        write_bundle(&archive, bytes)?;
        return Ok((
            python.into(),
            vec![
                "-c".into(),
                include_str!("coverage_vendor/bootstrap.py").into(),
                archive.display().to_string(),
            ],
        ));
    }
    Ok((python.into(), vec!["-m".into(), "coverage".into()]))
}

fn run_coverage_with(
    python: &Path,
    args: &[String],
    cwd: &Path,
    timeout: Duration,
    options: &CoverageOptions,
) -> Result<(), String> {
    let (program, prefix) =
        coverage_prefix(python, cwd, options).map_err(|error| error.to_string())?;
    let mut child = rustpy_project::interpreter::interpreter_command(&program)
        .args(prefix)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", python.display()))?;
    let mut stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = stderr.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("timed out".into());
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    let err = reader.join().unwrap_or_default();
    if status.success() {
        Ok(())
    } else {
        let msg = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_string();
        Err(if msg.is_empty() {
            format!("exit code {}", status.code().unwrap_or(-1))
        } else {
            msg
        })
    }
}

/// The coverage.py configuration coverage would read in `dir`: `.coveragerc`,
/// or `setup.cfg` / `tox.ini` / `pyproject.toml` with coverage sections, and
/// whether it chooses what to measure (`source`, `source_pkgs` or `include`
/// in its run section).
pub fn find_config(dir: &Path) -> Option<(PathBuf, bool)> {
    for (name, run_sections, needs) in [
        (".coveragerc", &["run", "coverage:run"][..], None),
        ("setup.cfg", &["coverage:run"][..], Some("[coverage:")),
        ("tox.ini", &["coverage:run"][..], Some("[coverage:")),
        (
            "pyproject.toml",
            &["tool.coverage.run"][..],
            Some("[tool.coverage"),
        ),
    ] {
        let path = dir.join(name);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if needs.is_some_and(|marker| !text.lines().any(|l| l.trim_start().starts_with(marker))) {
            continue;
        }
        let toml = name.ends_with(".toml");
        return Some((path, config_selects_source(&text, run_sections, toml)));
    }
    None
}

fn config_selects_source(text: &str, run_sections: &[&str], toml: bool) -> bool {
    let mut in_run = false;
    for line in text.lines() {
        let t = line.trim();
        if let Some(section) = t.strip_prefix('[').and_then(|s| s.split(']').next()) {
            in_run = run_sections
                .iter()
                .any(|r| r.eq_ignore_ascii_case(section.trim()));
            continue;
        }
        // In INI files an indented line continues the previous value.
        if in_run && (toml || !line.starts_with([' ', '\t'])) {
            let key = t.split(['=', ':']).next().unwrap_or("").trim();
            if matches!(key, "source" | "source_pkgs" | "include") {
                return true;
            }
        }
    }
    false
}

/// Options of a coverage run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoverageOptions {
    /// Measure branch coverage (`--branch`): partially covered lines.
    pub branch: bool,
    pub bundled: bool,
    pub executable: Option<PathBuf>,
    pub pytest_cov: bool,
    pub include: Vec<String>,
    pub omit: Vec<String>,
    pub collect_tests: bool,
}

impl Default for CoverageOptions {
    fn default() -> Self {
        Self {
            branch: true,
            bundled: true,
            executable: None,
            pytest_cov: false,
            include: Vec::new(),
            omit: Vec::new(),
            collect_tests: true,
        }
    }
}

enum Outcome {
    Report(CoverageReport),
    Failed(String),
}

/// Turns the data file into a report once the program has exited.
struct Exporter {
    options: CoverageOptions,
    python: PathBuf,
    cwd: PathBuf,
    data_file: PathBuf,
    rcfile: Option<PathBuf>,
    /// Where the data file is kept after the export (for the HTML report).
    keep_as: Option<PathBuf>,
    tx: Sender<Outcome>,
}

/// A run under `coverage run`; the report becomes available after the process exits.
pub struct CoverageRun {
    pub id: u64,
    pub name: String,
    pub command_line: String,
    process: Streamed,
    /// Present until the export has started.
    exporter: Option<Exporter>,
    outcome_rx: Option<Receiver<Outcome>>,
    report: Option<CoverageReport>,
    error: Option<String>,
    exit_code: Option<i32>,
}

/// Starts `python -m coverage run … <target>` for the configuration (branch coverage on).
pub fn run_with_coverage(
    id: u64,
    cfg: &RunConfig,
    interpreter: Option<&Interpreter>,
    project_root: &Path,
) -> anyhow::Result<CoverageRun> {
    run_with_coverage_opts(
        id,
        cfg,
        interpreter,
        project_root,
        &CoverageOptions::default(),
    )
}

/// Where a run's coverage data is kept: `<project>/.rustpy/coverage/<name>.coverage`.
pub fn kept_data_file(project_root: &Path, config_name: &str) -> PathBuf {
    let safe: String = config_name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let safe = if safe.trim_matches('.').is_empty() {
        "coverage".to_string()
    } else {
        safe
    };
    project_root
        .join(".rustpy")
        .join("coverage")
        .join(format!("{safe}.coverage"))
}

/// [`run_with_coverage`] with explicit options. The project's coverage.py
/// configuration is honoured: when the one in the run's working directory
/// selects what to measure (`source`, `include`) the run does not force
/// `--source=<project>`, and a configuration in the project root is passed
/// with `--rcfile` when the run's working directory is elsewhere.
pub fn run_with_coverage_opts(
    id: u64,
    cfg: &RunConfig,
    interpreter: Option<&Interpreter>,
    project_root: &Path,
    options: &CoverageOptions,
) -> anyhow::Result<CoverageRun> {
    let cmd = PythonCommand::for_config(cfg, interpreter, project_root, "Run with Coverage")?;
    if !options.bundled && options.executable.is_none() && !coverage_available(&cmd.python) {
        anyhow::bail!("The 'coverage' package is not installed for {}. Install it with: {} -m pip install coverage", cmd.python.display(), cmd.python.display());
    }
    let data_file =
        std::env::temp_dir().join(format!("rustpy-coverage-{}-{}.dat", std::process::id(), id));
    let _ = std::fs::remove_file(&data_file);
    let (program, mut inserted) = coverage_prefix(&cmd.python, project_root, options)?;
    inserted.extend(["run".into(), format!("--data-file={}", data_file.display())]);
    // coverage.py reads its configuration from the working directory only; one in
    // the project root is passed explicitly. Its relative `source` paths would then
    // resolve against the other working directory, so it does not replace
    // `--source=<project>` below.
    let (config_selects, rcfile) = match find_config(&cmd.cwd) {
        Some((_, selects)) => (selects, None),
        None => (false, find_config(project_root).map(|(path, _)| path)),
    };
    if let Some(rc) = &rcfile {
        inserted.push(format!("--rcfile={}", rc.display()));
    }
    if options.branch {
        inserted.push("--branch".into());
    }
    if !options.include.is_empty() {
        inserted.push(format!("--include={}", options.include.join(",")));
    }
    let mut omit = options.omit.clone();
    if !options.collect_tests {
        omit.extend([
            "*/tests/*".into(),
            "*/test_*.py".into(),
            "*/conftest.py".into(),
        ]);
    }
    if !omit.is_empty() {
        inserted.push(format!("--omit={}", omit.join(",")));
    }
    // Restrict measurement to the project when the target lives inside it,
    // unless the configuration says what to measure.
    let in_project = match &cfg.kind {
        RunConfigKind::Python { script } => {
            script.starts_with(project_root) || !script.is_absolute()
        }
        _ => true,
    };
    if in_project && !config_selects && options.include.is_empty() {
        inserted.push(format!("--source={}", project_root.display()));
    }
    let mut args = if program == cmd.python {
        cmd.interpreter_options.clone()
    } else {
        Vec::new()
    };
    args.extend(inserted.iter().cloned());
    args.extend(cmd.target.iter().cloned());
    let mut env = cmd.env.clone();
    env.insert(
        "COVERAGE_FILE".into(),
        data_file.to_string_lossy().into_owned(),
    );
    let coverage_arguments = inserted
        .iter()
        .position(|argument| argument == "run")
        .map(|index| inserted[index..].join(" "))
        .unwrap_or_default();
    let mut command_line = format!(
        "{} {} {}",
        options.executable.as_ref().unwrap_or(&cmd.python).display(),
        if options.executable.is_some() {
            coverage_arguments.clone()
        } else {
            format!("-m coverage {coverage_arguments}")
        },
        cmd.target.join(" ")
    );
    if options.pytest_cov && matches!(cfg.kind, RunConfigKind::Pytest { .. }) {
        // pytest-cov owns tracing in its pytest process; both report export and
        // subsequent imports use the same pinned coverage implementation.
        args = cmd.interpreter_options.clone();
        let options_json = serde_json::json!({"include":options.include,"omit":omit}).to_string();
        let runner = include_str!("coverage_vendor/pytest_runner.py");
        if options.bundled {
            let prepare = include_str!("coverage_vendor/bootstrap.py")
                .replace("runpy.run_module('coverage', run_name='__main__')", runner);
            args.extend([
                "-c".into(),
                prepare,
                project_root
                    .join(".rustpy/coverage/runner/coverage-7.6.1.tar.gz")
                    .display()
                    .to_string(),
                options_json,
            ]);
        } else {
            args.extend([
                "-c".into(),
                format!("import sys, runpy\n{runner}"),
                options_json,
            ]);
        }
        args.extend(cmd.target.iter().skip(2).cloned());
        args.push(if config_selects || !options.include.is_empty() {
            "--cov".into()
        } else {
            format!("--cov={}", project_root.display())
        });
        args.push("--cov-report=".into());
        if options.branch {
            args.push("--cov-branch".into());
        }
        if let Some(rc) = &rcfile {
            args.push(format!("--cov-config={}", rc.display()));
        }
        command_line = format!(
            "{} {} --cov --cov-report= {}",
            cmd.python.display(),
            cmd.target.join(" "),
            if options.branch { "--cov-branch" } else { "" }
        );
    }
    let run_program = if options.pytest_cov && matches!(cfg.kind, RunConfigKind::Pytest { .. }) {
        &cmd.python
    } else {
        &program
    };
    let process = Streamed::spawn_with_environment(
        run_program,
        &args,
        &cmd.cwd,
        &env,
        cmd.stdin(),
        cmd.inherit_environment,
    )?;
    let (tx, rx) = unbounded();
    let exporter = Exporter {
        options: options.clone(),
        python: cmd.python.clone(),
        cwd: cmd.cwd.clone(),
        data_file,
        rcfile: rcfile.or_else(|| find_config(&cmd.cwd).map(|(path, _)| path)),
        keep_as: Some(kept_data_file(project_root, &cfg.name)),
        tx,
    };
    Ok(CoverageRun {
        id,
        name: cfg.name.clone(),
        command_line,
        process,
        exporter: Some(exporter),
        outcome_rx: Some(rx),
        report: None,
        error: None,
        exit_code: None,
    })
}

impl Exporter {
    /// `coverage json` on the data file, then keeps the data file for the HTML report.
    fn export(self) {
        let json_file = self.data_file.with_extension("json");
        let mut args = vec![
            "json".to_string(),
            format!("--data-file={}", self.data_file.display()),
            "-o".to_string(),
            json_file.to_string_lossy().into_owned(),
            "-q".to_string(),
            "--ignore-errors".to_string(),
        ];
        if let Some(rc) = &self.rcfile {
            args.push(format!("--rcfile={}", rc.display()));
        }
        let result = match run_coverage_with(
            &self.python,
            &args,
            &self.cwd,
            Duration::from_secs(60),
            &self.options,
        ) {
            Ok(()) => match std::fs::read_to_string(&json_file) {
                Ok(text) => load_json(&text, &self.cwd)
                    .map(Outcome::Report)
                    .unwrap_or_else(|e| {
                        Outcome::Failed(format!("Cannot parse coverage report: {e}"))
                    }),
                Err(e) => Outcome::Failed(format!("Cannot read coverage report: {e}")),
            },
            Err(e) => Outcome::Failed(format!(
                "coverage json failed (no data was collected?): {e}"
            )),
        };
        let result = match result {
            Outcome::Report(mut r) => {
                r.rcfile = self.rcfile.clone();
                r.data_file = self
                    .keep_as
                    .as_deref()
                    .and_then(|dest| keep_file(&self.data_file, dest));
                Outcome::Report(r)
            }
            failed => failed,
        };
        let _ = std::fs::remove_file(&self.data_file);
        let _ = std::fs::remove_file(&json_file);
        let _ = self.tx.send(result);
    }
}

/// Moves (or copies) `from` to `dest`; `None` when that is not possible.
fn keep_file(from: &Path, dest: &Path) -> Option<PathBuf> {
    std::fs::create_dir_all(dest.parent()?).ok()?;
    if std::fs::rename(from, dest).is_err() {
        std::fs::copy(from, dest).ok()?;
    }
    Some(dest.to_path_buf())
}

impl CoverageRun {
    /// New output lines as `(text, is_stderr)`; the last lines announce the report.
    pub fn poll(&mut self) -> Vec<(String, bool)> {
        let mut lines = self.process.poll_lines();
        if let Some(exit) = self.process.exit().cloned() {
            if let Some(exporter) = self.exporter.take() {
                self.exit_code = exit.code();
                if exit.code().is_none() {
                    let _ = std::fs::remove_file(&exporter.data_file);
                    let _ = exporter.tx.send(Outcome::Failed(
                        "Process was stopped before coverage data could be collected".into(),
                    ));
                } else {
                    std::thread::spawn(move || exporter.export());
                }
            }
        }
        if let Some(rx) = &self.outcome_rx {
            if let Ok(outcome) = rx.try_recv() {
                match outcome {
                    Outcome::Report(r) => {
                        let (covered, total) = r.branch_totals();
                        let branches = if r.branch && total > 0 {
                            format!(", {:.1}% of branches", percent_of(covered, total))
                        } else {
                            String::new()
                        };
                        let (lc, ls) = r.totals();
                        lines.push((
                            format!(
                                "Coverage: {:.1}% of statements{branches} in {} file(s)",
                                percent_of(lc, ls),
                                r.files.len()
                            ),
                            false,
                        ));
                        self.report = Some(r);
                    }
                    Outcome::Failed(e) => {
                        lines.push((e.clone(), true));
                        self.error = Some(e);
                    }
                }
                self.outcome_rx = None;
            }
        }
        lines
    }

    /// True while the program runs or the report is still being produced.
    pub fn is_running(&self) -> bool {
        self.process.is_running() || self.outcome_rx.is_some()
    }

    pub fn stop(&mut self) {
        self.process.stop();
    }

    pub fn report(&self) -> Option<&CoverageReport> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_lines_come_from_missing_branches_of_executed_lines() {
        let json = r#"{"meta": {"version": "7.16.2", "branch_coverage": true}, "files": {"prog.py": {"executed_lines": [1, 4, 5, 6, 12, 13, 14, 20], "missing_lines": [7, 21], "excluded_lines": [],
            "executed_branches": [[5, 6], [12, 13], [12, 15], [13, 14], [20, -1]], "missing_branches": [[5, 7], [13, 12], [20, 21], [30, 31]],
            "summary": {"percent_covered": 78.3, "num_branches": 8, "covered_branches": 5}}}, "totals": {"percent_covered": 78.3}}"#;
        let r = load_json(json, Path::new("/p")).unwrap();
        assert!(r.branch);
        let f = r.file(Path::new("/p/prog.py")).unwrap();
        // Line 30 is not executed, so its missing arc does not make it partial.
        assert_eq!(f.partial, vec![5, 13, 20]);
        assert_eq!(f.line_state(5), Some(LineCoverage::Partial));
        assert_eq!(f.line_state(6), Some(LineCoverage::Covered));
        assert_eq!(f.line_state(7), Some(LineCoverage::Missed));
        assert_eq!((f.num_branches, f.covered_branches), (8, 5));
        assert_eq!(f.branch_percent(), Some(62.5));
        assert!((f.line_percent() - 80.0).abs() < 1e-3);
        assert_eq!(r.branch_totals(), (5, 8));
        // Without a summary the arcs are counted.
        let r = load_json(r#"{"files": {"/a.py": {"executed_lines": [1, 2], "missing_lines": [], "executed_branches": [[1, 2]], "missing_branches": [[1, 3]]}}}"#, Path::new("/")).unwrap();
        assert_eq!(
            (r.files[0].num_branches, r.files[0].covered_branches),
            (2, 1)
        );
        assert!(r.branch);
        // Line coverage only: no branches, nothing partial.
        let r = load_json(r#"{"meta": {"branch_coverage": false}, "files": {"/b.py": {"executed_lines": [1], "missing_lines": [2]}}}"#, Path::new("/")).unwrap();
        assert!(
            !r.branch && r.files[0].partial.is_empty() && r.files[0].branch_percent().is_none()
        );
        assert_eq!(percent_of(0, 0), 100.0);
    }

    #[test]
    fn cobertura_xml_reports_parse() {
        let xml = r#"<?xml version="1.0" ?>
<coverage version="7.6" timestamp="1" lines-valid="5" lines-covered="3" line-rate="0.6" branches-covered="1" branches-valid="2" branch-rate="0.5">
  <sources><source>/proj/src</source></sources>
  <packages><package name="pkg"><classes>
    <class name="mod.py" filename="pkg/mod.py" complexity="0" line-rate="0.6" branch-rate="0.5">
      <methods/>
      <lines>
        <line number="1" hits="1"/>
        <line number="2" hits="3" branch="true" condition-coverage="50% (1/2)" missing-branches="4"/>
        <line number="3" hits="1"/>
        <line number="4" hits="0"/>
        <line number="6" hits="0"/>
      </lines>
    </class>
    <class name="b &amp; c.py" filename="/abs/b.py"><lines><line number="1" hits="2"/></lines></class>
  </classes></package></packages>
</coverage>"#;
        let r = load_cobertura_xml(xml, Path::new("/root")).unwrap();
        assert_eq!(r.files.len(), 2);
        let m = r
            .file(Path::new("/proj/src/pkg/mod.py"))
            .expect("resolved against <source>");
        assert_eq!(m.executed, vec![1, 2, 3]);
        assert_eq!(m.missing, vec![4, 6]);
        assert_eq!(m.partial, vec![2]);
        assert_eq!(m.line_state(2), Some(LineCoverage::Partial));
        assert_eq!((m.num_branches, m.covered_branches), (2, 1));
        assert!(r.branch);
        assert!(r.file(Path::new("/abs/b.py")).is_some());
        assert!(load_cobertura_xml("<html/>", Path::new("/")).is_err());
    }

    #[test]
    fn coverage_configuration_is_found_and_read() {
        let dir = std::env::temp_dir().join(format!("rustpy-covcfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(find_config(&dir), None);
        // setup.cfg without coverage sections is not a coverage configuration.
        std::fs::write(dir.join("setup.cfg"), "[metadata]\nname = x\n").unwrap();
        assert_eq!(find_config(&dir), None);
        std::fs::write(
            dir.join("pyproject.toml"),
            "[project]\nname = 'x'\n\n[tool.coverage.run]\nbranch = true\nsource = [\"src\"]\n",
        )
        .unwrap();
        assert_eq!(find_config(&dir), Some((dir.join("pyproject.toml"), true)));
        std::fs::write(
            dir.join("tox.ini"),
            "[coverage:run]\nomit = tests/*\n[coverage:report]\ninclude = x\n",
        )
        .unwrap();
        assert_eq!(
            find_config(&dir),
            Some((dir.join("tox.ini"), false)),
            "include outside [coverage:run] does not count"
        );
        std::fs::write(
            dir.join("setup.cfg"),
            "[coverage:run]\nsource_pkgs =\n    mypkg\n",
        )
        .unwrap();
        assert_eq!(find_config(&dir), Some((dir.join("setup.cfg"), true)));
        // .coveragerc wins, even when it only sets other options.
        std::fs::write(
            dir.join(".coveragerc"),
            "[run]\nbranch = True\n# source = commented\n",
        )
        .unwrap();
        assert_eq!(find_config(&dir), Some((dir.join(".coveragerc"), false)));
        std::fs::write(dir.join(".coveragerc"), "[run]\ninclude: app/*\n").unwrap();
        assert_eq!(find_config(&dir), Some((dir.join(".coveragerc"), true)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn kept_data_files_have_safe_names() {
        let p = kept_data_file(Path::new("/proj"), "pytest in test_x.py");
        assert_eq!(
            p,
            Path::new("/proj/.rustpy/coverage/pytest_in_test_x.py.coverage")
        );
        assert_eq!(
            kept_data_file(Path::new("/proj"), ".."),
            Path::new("/proj/.rustpy/coverage/coverage.coverage")
        );
        assert_eq!(
            kept_data_file(Path::new("/proj"), "a/../b"),
            Path::new("/proj/.rustpy/coverage/a_.._b.coverage")
        );
    }
}
