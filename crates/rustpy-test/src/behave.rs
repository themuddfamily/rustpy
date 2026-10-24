//! Behave (BDD) support: static discovery of `.feature` files, the behave
//! command line, and conversion of behave's `json.pretty` report (plus the
//! `plain` formatter's console output) into [`TestEvent`]s.
//!
//! Ids follow the pytest node-id form used by the rest of the crate, relative
//! to the project root: a feature file is a module (`features/x.feature`),
//! the feature a suite (`features/x.feature::Calculator`) and every scenario
//! a test (`features/x.feature::Calculator::Add two numbers`). Scenario
//! outline rows are separate tests named the way behave names them
//! (`...::Add 1 and 2 -- @1.1 Small numbers`). `::` and brackets inside names
//! are replaced so ids stay splittable, and a duplicated scenario name gets a
//! ` (line N)` suffix.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use rustpy_gherkin::{outline_steps_expanded, BlockKind, Feature, DIALECTS};
use serde_json::Value;

use crate::discover::{TestItem, TestKind, SKIP_DIRS};
use crate::runner::{file_contains, Outcome, Summary, TestEvent, TestResult};

/// File name of the JSON report inside the run's temporary directory.
pub(crate) const BEHAVE_REPORT: &str = "behave-report.json";

/// A name as one `::`-separated id segment.
fn id_segment(name: &str) -> String {
    let mut s = name.trim().replace('[', "(").replace(']', ")");
    while s.contains("::") {
        s = s.replace("::", ":");
    }
    let s = s.trim_matches(|c: char| c == ':' || c.is_whitespace());
    if s.is_empty() {
        "_".to_string()
    } else {
        s.to_string()
    }
}

fn rel_string(root: &Path, path: &Path) -> String {
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    rel.trim_start_matches("./").to_string()
}

fn file_stem(rel: &str) -> String {
    Path::new(rel)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| rel.to_string())
}

/// Display name of a feature (behave allows empty names).
fn feature_name(name: &str, rel: &str) -> String {
    let n = name.trim();
    if n.is_empty() {
        file_stem(rel)
    } else {
        n.to_string()
    }
}

/// Display name of a scenario (behave allows empty names).
fn scenario_name(name: &str, keyword: &str, line: usize) -> String {
    let n = name.trim();
    if n.is_empty() {
        format!("{} (line {line})", keyword.trim())
    } else {
        n.to_string()
    }
}

/// Id of the feature in the file `rel` (`features/x.feature::Calculator`).
pub fn behave_feature_id(rel: &str, feature: &str) -> String {
    format!("{rel}::{}", id_segment(&feature_name(feature, rel)))
}

/// True for `*.feature` files.
pub fn is_feature_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("feature"))
}

/// Scenario ids within one feature, disambiguating duplicate names with the
/// line (`Name (line 12)`).
#[derive(Default)]
struct IdAllocator {
    used: HashSet<String>,
}

impl IdAllocator {
    fn id(&mut self, feature_id: &str, name: &str, line: usize) -> String {
        let base = format!("{feature_id}::{}", id_segment(name));
        if self.used.insert(base.clone()) {
            return base;
        }
        let with_line = format!("{base} (line {line})");
        self.used.insert(with_line.clone());
        with_line
    }
}

/// `(name, line)` of every scenario behave runs for `f`, in document order:
/// plain scenarios at their line, outline rows (with behave's generated
/// names) at their Examples row line.
fn runnable_scenarios(f: &Feature) -> Vec<(String, String, usize)> {
    let mut out = Vec::new();
    for s in &f.scenarios {
        if s.is_outline() {
            out.extend(
                outline_steps_expanded(s)
                    .into_iter()
                    .map(|r| (s.keyword.clone(), r.name, r.line)),
            );
        } else {
            out.push((s.keyword.clone(), s.name.clone(), s.line));
        }
    }
    out
}

fn feature_item(root: &Path, path: &Path, f: &Feature) -> Option<TestItem> {
    if f.line == 0 {
        return None;
    }
    let rel = rel_string(root, path);
    let fid = behave_feature_id(&rel, &f.name);
    let mut ids = IdAllocator::default();
    let children = runnable_scenarios(f)
        .into_iter()
        .map(|(keyword, name, line)| {
            let display = scenario_name(&name, &keyword, line);
            let id = ids.id(&fid, &display, line);
            TestItem {
                id,
                name: display,
                kind: TestKind::Function,
                path: path.to_path_buf(),
                line,
                children: Vec::new(),
            }
        })
        .collect();
    Some(TestItem {
        id: fid,
        name: feature_name(&f.name, &rel),
        kind: TestKind::Class,
        path: path.to_path_buf(),
        line: f.line,
        children,
    })
}

/// Discovers one feature file: a [`TestKind::Class`] item for the feature
/// with one [`TestKind::Function`] item per scenario / outline row (outline
/// rows are located at their Examples row, as behave reports them). `path`
/// is used for [`TestItem::path`]; ids are relative to `root`. A file
/// without a `Feature:` yields nothing.
pub fn discover_feature_file_in(root: &Path, path: &Path, text: &str) -> Vec<TestItem> {
    feature_item(root, path, &rustpy_gherkin::parse(text))
        .into_iter()
        .collect()
}

/// The [`TestKind::Module`] item of one feature file (ids relative to `root`).
fn feature_module(root: &Path, file: &Path) -> Option<TestItem> {
    let text = std::fs::read_to_string(file).ok()?;
    let children = discover_feature_file_in(root, file, &text);
    if children.is_empty() {
        return None;
    }
    let rel = rel_string(root, file);
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| rel.clone());
    Some(TestItem {
        id: rel,
        name,
        kind: TestKind::Module,
        path: file.to_path_buf(),
        line: 1,
        children,
    })
}

fn feature_files_under(dir: &Path) -> Vec<PathBuf> {
    let walker = ignore::WalkBuilder::new(dir)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .follow_links(false)
        .max_depth(Some(24))
        .filter_entry(|e| {
            !e.path()
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| SKIP_DIRS.contains(&n))
        })
        .build();
    let mut files: Vec<PathBuf> = walker
        .flatten()
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(|e| e.into_path())
        .filter(|p| is_feature_file(p))
        .collect();
    files.sort();
    files
}

/// Feature files under `dir` as module items with ids relative to `root`.
fn discover_features_under(root: &Path, dir: &Path) -> Vec<TestItem> {
    feature_files_under(dir)
        .iter()
        .filter_map(|f| feature_module(root, f))
        .collect()
}

/// Walks the project (honoring `.gitignore`, skipping virtualenvs and caches)
/// and returns one [`TestKind::Module`] item per `.feature` file (sorted by
/// path) with the feature ([`TestKind::Class`]) and its scenarios
/// ([`TestKind::Function`]) as children; ids are relative to `root`.
pub fn discover_features(root: &Path) -> Vec<TestItem> {
    discover_features_under(root, root)
}

/// Config-file evidence that the project uses behave: a `features/`
/// directory with `steps/` or `environment.py`, `behave.ini` / `.behaverc`,
/// or a `[behave]` section in `setup.cfg` / `tox.ini` (`[tool.behave]` in
/// `pyproject.toml`).
pub fn project_uses_behave(root: &Path) -> bool {
    let features = root.join("features");
    features.join("steps").is_dir()
        || features.join("environment.py").is_file()
        || root.join("behave.ini").is_file()
        || root.join(".behaverc").is_file()
        || file_contains(&root.join("setup.cfg"), "[behave]")
        || file_contains(&root.join("tox.ini"), "[behave]")
        || file_contains(&root.join("pyproject.toml"), "[tool.behave]")
}

fn absolute(root: &Path, p: &str) -> PathBuf {
    let path = Path::new(p);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

/// Splits `file::rest` or `file:line` (the file part may be a Windows path).
fn split_target(t: &str) -> (&str, Option<&str>) {
    if let Some((f, r)) = t.split_once("::") {
        return (f, Some(r));
    }
    if let Some((f, l)) = t.rsplit_once(':') {
        if is_line(l) && f.len() > 1 {
            return (f, Some(l));
        }
    }
    (t, None)
}

fn is_line(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// Scenario lines (scenario line, or Examples row line for outline rows) that
/// `behave file:LINE` runs, with behave ≥ 1.3 semantics: line 0 or the
/// `Feature:` line select everything, a `Rule:` / outline line its
/// scenarios, a scenario / Examples row line that scenario, and any other
/// line the closest preceding one of these.
fn lines_selected_by(f: &Feature, line: usize) -> Vec<usize> {
    #[derive(Clone, Copy)]
    enum Entity {
        All,
        Rule(usize),
        Outline(usize),
        One(usize),
    }
    let mut entities: Vec<(usize, Entity)> = vec![(0, Entity::All), (f.line, Entity::All)];
    entities.extend(
        f.rules
            .iter()
            .enumerate()
            .map(|(i, r)| (r.line, Entity::Rule(i))),
    );
    for (i, s) in f.scenarios.iter().enumerate() {
        if s.is_outline() {
            entities.push((s.line, Entity::Outline(i)));
            entities.extend(
                outline_steps_expanded(s)
                    .into_iter()
                    .map(|r| (r.line, Entity::One(r.line))),
            );
        } else {
            entities.push((s.line, Entity::One(s.line)));
        }
    }
    entities.sort_by_key(|(l, _)| *l);
    let pos = entities
        .partition_point(|(l, _)| *l <= line)
        .saturating_sub(1);
    let lines_of = |s: &rustpy_gherkin::Scenario| -> Vec<usize> {
        if s.is_outline() {
            outline_steps_expanded(s)
                .into_iter()
                .map(|r| r.line)
                .collect()
        } else {
            vec![s.line]
        }
    };
    match entities[pos].1 {
        Entity::All => f.scenarios.iter().flat_map(lines_of).collect(),
        Entity::Rule(r) => f
            .scenarios
            .iter()
            .filter(|s| s.rule == Some(r))
            .flat_map(lines_of)
            .collect(),
        Entity::Outline(i) => lines_of(&f.scenarios[i]),
        Entity::One(l) => vec![l],
    }
}

/// Narrows a feature module item to what the target suffix `rest` selects
/// (`12` = a line, `Feature::Scenario` = an id). Unknown ids keep the whole
/// file, like [`behave_targets`] does.
fn narrow(module: &mut TestItem, rest: &str) {
    if is_line(rest) {
        let line: usize = rest.parse().unwrap_or(0);
        let parsed = std::fs::read_to_string(&module.path)
            .map(|t| rustpy_gherkin::parse(&t))
            .unwrap_or_default();
        let lines = lines_selected_by(&parsed, line);
        for feature in &mut module.children {
            feature.children.retain(|s| lines.contains(&s.line));
        }
        return;
    }
    let wanted = format!("{}::{rest}", module.id);
    let known = module
        .children
        .iter()
        .any(|f| f.id == wanted || f.children.iter().any(|s| s.id == wanted));
    if !known {
        return;
    }
    for feature in &mut module.children {
        if feature.id != wanted {
            feature.children.retain(|s| s.id == wanted);
        }
    }
}

fn merge(out: &mut Vec<TestItem>, module: TestItem) {
    let Some(existing) = out.iter_mut().find(|o| o.id == module.id) else {
        out.push(module);
        return;
    };
    for (feature, extra) in existing.children.iter_mut().zip(module.children) {
        for s in extra.children {
            if !feature.children.iter().any(|c| c.id == s.id) {
                feature.children.push(s);
            }
        }
        feature.children.sort_by_key(|c| c.line);
    }
}

/// Static items for a run: the default `features/` directory (or the whole
/// project) for no targets, otherwise the targeted directories / files /
/// scenarios. Also returns whether any target selects single scenarios.
pub(crate) fn items_for_targets(root: &Path, targets: &[String]) -> (Vec<TestItem>, bool) {
    if targets.is_empty() {
        let features = root.join("features");
        let base = if features.is_dir() {
            features
        } else {
            root.to_path_buf()
        };
        return (discover_features_under(root, &base), false);
    }
    let mut out: Vec<TestItem> = Vec::new();
    let mut narrowed = false;
    for t in targets {
        let (file_part, rest) = split_target(t);
        let abs = absolute(root, file_part);
        if abs.is_dir() {
            for m in discover_features_under(root, &abs) {
                merge(&mut out, m);
            }
            continue;
        }
        let Some(mut module) = feature_module(root, &abs) else {
            continue;
        };
        if let Some(rest) = rest {
            narrowed = true;
            narrow(&mut module, rest);
        }
        merge(&mut out, module);
    }
    (out, narrowed)
}

/// Directories to pass to behave for a whole-project run: none when the
/// project has the default `features/` directory, otherwise the outermost
/// directories that contain feature files.
fn default_targets(root: &Path) -> Vec<String> {
    if root.join("features").is_dir() {
        return Vec::new();
    }
    let mut dirs: Vec<PathBuf> = feature_files_under(root)
        .into_iter()
        .filter_map(|f| f.parent().map(Path::to_path_buf))
        .collect();
    dirs.sort();
    dirs.dedup();
    let outer: Vec<&PathBuf> = dirs
        .iter()
        .filter(|d| !dirs.iter().any(|o| o != *d && d.starts_with(o)))
        .collect();
    outer
        .into_iter()
        .map(|d| {
            let rel = rel_string(root, d);
            if rel.is_empty() {
                ".".to_string()
            } else {
                rel
            }
        })
        .collect()
}

/// Converts test ids (`features/x.feature::Feature::Scenario`), feature ids,
/// `.feature` files / directories and `file:line` locations into behave path
/// arguments relative to `root`. Scenario ids become `file:line`; an id that
/// no longer exists selects its whole file. A `file:line` location is
/// resolved to the scenarios it selects with behave ≥ 1.3 rules (the
/// `Feature:` line selects everything, a `Rule:` / outline line its
/// scenarios, any other line the closest preceding scenario or Examples row)
/// and passed as their exact lines (or as the whole file), so behave 1.2,
/// which snaps lines to the closest scenario, runs the same.
pub fn behave_targets(root: &Path, targets: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in targets {
        let (file_part, rest) = split_target(t);
        let abs = absolute(root, file_part);
        let mut rel = rel_string(root, &abs);
        if rel.is_empty() {
            rel = ".".into();
        }
        let args: Vec<String> = match rest {
            Some(r) if is_line(r) => {
                let line: usize = r.parse().unwrap_or(0);
                let parsed = std::fs::read_to_string(&abs)
                    .map(|text| rustpy_gherkin::parse(&text))
                    .unwrap_or_default();
                let selected = lines_selected_by(&parsed, line);
                let all = runnable_scenarios(&parsed).len();
                if parsed.line == 0 || selected.is_empty() {
                    vec![format!("{rel}:{r}")]
                } else if selected.len() == all {
                    vec![rel]
                } else {
                    selected.iter().map(|l| format!("{rel}:{l}")).collect()
                }
            }
            Some(r) if r.contains("::") => {
                let wanted = format!("{rel}::{r}");
                let line = std::fs::read_to_string(&abs).ok().and_then(|text| {
                    discover_feature_file_in(root, &abs, &text)
                        .into_iter()
                        .flat_map(|f| f.children)
                        .find(|s| s.id == wanted)
                        .map(|s| s.line)
                });
                match line {
                    Some(l) => vec![format!("{rel}:{l}")],
                    None => vec![rel],
                }
            }
            _ => vec![rel],
        };
        for arg in args {
            if !out.contains(&arg) {
                out.push(arg);
            }
        }
    }
    out
}

/// Arguments after the interpreter:
/// `-m behave --format json.pretty --outfile REPORT --format plain [--no-skipped] [extra] [targets]`.
/// `--no-skipped` is added when the run is narrowed (scenario locations, or
/// name / tag filters) so deselected scenarios are not reported as skipped.
/// A pytest-style `-k EXPR` becomes behave's `--name EXPR` (a lone `-k` is
/// behave's own short form of `--no-skipped` and is kept).
pub fn behave_args(
    json_report: &Path,
    behave_targets: &[String],
    extra_args: &[String],
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-m".into(),
        "behave".into(),
        "--format".into(),
        "json.pretty".into(),
        "--outfile".into(),
        json_report.to_string_lossy().into_owned(),
        "--format".into(),
        "plain".into(),
    ];
    let mut extra: Vec<String> = Vec::new();
    let mut i = 0;
    while i < extra_args.len() {
        let a = &extra_args[i];
        if a == "-k" && extra_args.get(i + 1).is_some_and(|v| !v.starts_with('-')) {
            extra.push("--name".into());
            extra.push(extra_args[i + 1].clone());
            i += 2;
            continue;
        }
        match a.strip_prefix("-k").filter(|v| !v.is_empty()) {
            Some(v) => {
                extra.push("--name".into());
                extra.push(v.to_string());
            }
            None => extra.push(a.clone()),
        }
        i += 1;
    }
    let filter_flags = [
        "-n",
        "--name",
        "-t",
        "--tags",
        "-i",
        "--include",
        "-e",
        "--exclude",
    ];
    let narrowed = behave_targets
        .iter()
        .any(|t| matches!(split_target(t), (_, Some(l)) if is_line(l)))
        || extra.iter().any(|a| {
            filter_flags.contains(&a.as_str())
                || filter_flags
                    .iter()
                    .any(|f| f.starts_with("--") && a.starts_with(&format!("{f}=")))
        });
    if narrowed
        && !extra
            .iter()
            .any(|a| a == "--no-skipped" || a == "--show-skipped" || a == "-k")
    {
        args.push("--no-skipped".into());
    }
    args.extend(extra);
    args.extend(behave_targets.iter().cloned());
    args
}

/// The command line of a behave run: arguments after the interpreter and the
/// statically known items (Module → Feature → Scenario) the run covers.
pub(crate) struct BehavePlan {
    pub args: Vec<String>,
    pub items: Vec<TestItem>,
    /// Some targets select single scenarios: only report those.
    pub narrowed: bool,
}

pub(crate) fn plan(
    root: &Path,
    targets: &[String],
    extra_args: &[String],
    json_report: &Path,
) -> BehavePlan {
    let (items, narrowed) = items_for_targets(root, targets);
    let behave_targets = if targets.is_empty() {
        default_targets(root)
    } else {
        behave_targets(root, targets)
    };
    BehavePlan {
        args: behave_args(json_report, &behave_targets, extra_args),
        items,
        narrowed,
    }
}

/// Scenario ids by location (`(file, line)`) and by `(feature name,
/// scenario name)`, built from static discovery.
#[derive(Clone, Debug, Default)]
pub struct BehaveIndex {
    by_location: HashMap<(String, usize), String>,
    by_name: HashMap<(String, String), Vec<String>>,
}

impl BehaveIndex {
    /// Builds the index from items of [`discover_features`] /
    /// [`discover_feature_file_in`] (Module or Class items).
    pub fn from_items(items: &[TestItem]) -> BehaveIndex {
        let mut idx = BehaveIndex::default();
        for it in items {
            idx.add(it);
        }
        idx
    }

    fn add(&mut self, item: &TestItem) {
        match item.kind {
            TestKind::Module => item.children.iter().for_each(|c| self.add(c)),
            TestKind::Class => {
                let rel = item.id.split("::").next().unwrap_or("").to_string();
                for s in &item.children {
                    self.by_location.insert((rel.clone(), s.line), s.id.clone());
                    self.by_name
                        .entry((item.name.clone(), s.name.clone()))
                        .or_default()
                        .push(s.id.clone());
                }
            }
            TestKind::Function => {}
        }
    }

    /// The scenario id at `rel:line` (`rel` relative to the project root).
    pub fn scenario_id(&self, rel: &str, line: usize) -> Option<&str> {
        self.by_location
            .get(&(rel.to_string(), line))
            .map(String::as_str)
    }

    fn by_name(&self, feature: &str, scenario: &str) -> &[String] {
        self.by_name
            .get(&(feature.to_string(), scenario.to_string()))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn contains_file(&self, rel: &str) -> bool {
        self.by_location.keys().any(|(r, _)| r == rel)
    }
}

/// Error text and captured output of a scenario from the `plain` formatter.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PlainDetails {
    pub error: String,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum Capture {
    Stdout,
    Stderr,
}

const STEP_STATUSES: &[&str] = &[
    "passed",
    "failed",
    "error",
    "skipped",
    "undefined",
    "untested",
    "pending",
    "pending_warn",
    "hook_error",
    "cleanup_error",
    "executing",
    "untested_pending",
    "untested_undefined",
];
const PROBLEM_STATUSES: &[&str] = &["failed", "error", "hook_error", "cleanup_error", "pending"];

/// Follows behave's `plain` console output: which scenario runs (for live
/// "started" events) and error text / captured output per scenario (behave
/// ≥ 1.3 does not put the traceback of erroring steps into the JSON report).
#[derive(Debug, Default)]
pub(crate) struct PlainTracker {
    feature: Option<String>,
    /// Number of `Feature:` headers seen.
    pub features_seen: usize,
    scenario: Option<String>,
    error: Option<String>,
    capture: Option<Capture>,
    /// behave ≥ 1.3 (it prints `USING RUNNER: ...` first).
    modern: bool,
    /// behave 1.2 prints `HOOK-ERROR in before_scenario` before the header
    /// of the scenario it belongs to.
    pending_hook: Option<String>,
    details: HashMap<(String, String), PlainDetails>,
}

fn match_block_any(trimmed: &str) -> Option<(BlockKind, usize)> {
    DIALECTS
        .iter()
        .find_map(|d| d.match_block(trimmed))
        .map(|(k, _, len)| (k, len))
}

fn capture_header(line: &str) -> Option<Capture> {
    if line.starts_with("CAPTURED STDOUT:") {
        Some(Capture::Stdout)
    } else if line.starts_with("CAPTURED STDERR:") || line.starts_with("CAPTURED LOG:") {
        Some(Capture::Stderr)
    } else {
        None
    }
}

/// The status of a `    Given x ... passed in 0.001s` line.
fn step_status(line: &str) -> Option<&str> {
    if !line.starts_with(char::is_whitespace) {
        return None;
    }
    let (_, right) = line.rsplit_once(" ... ")?;
    let mut words = right.split_whitespace();
    let status = words.next()?;
    let rest: Vec<&str> = words.collect();
    let timing_ok =
        rest.is_empty() || (rest.len() == 2 && rest[0] == "in" && rest[1].ends_with('s'));
    (STEP_STATUSES.contains(&status) && timing_ok).then_some(status)
}

impl PlainTracker {
    /// Feeds one stdout line; returns `(feature name, scenario name)` when a
    /// scenario starts.
    pub fn feed(&mut self, raw: &str) -> Option<(String, String)> {
        let line = raw.trim_end();
        if line.starts_with("USING RUNNER:") {
            self.modern = true;
            return None;
        }
        if line.starts_with("---- CAPTURED_SCENARIO_OUTPUT_END") {
            self.capture = None;
            return None;
        }
        if line.starts_with("HOOK-ERROR in ") || line.starts_with("CLEANUP-ERROR in ") {
            self.end_error();
            if !self.modern
                && self.capture.is_none()
                && line.starts_with("HOOK-ERROR in before_scenario")
            {
                self.pending_hook = Some(line.to_string());
            } else {
                // behave 1.3 prints it inside the hook's captured output.
                self.capture = None;
                self.error = Some(format!("{line}\n"));
            }
            return None;
        }
        if let Some(c) = capture_header(line) {
            self.end_error();
            self.capture = Some(c);
            return None;
        }
        let trimmed = line.trim_start();
        let indented = trimmed.len() < line.len();
        let block = match_block_any(trimmed);
        if let Some(c) = self.capture {
            // Hook output sections end with a bare `----`; a header ends any section.
            let header = matches!(block, Some((BlockKind::Feature, _)) if !indented)
                || matches!(block, Some((BlockKind::Scenario | BlockKind::ScenarioOutline, _)) if indented);
            if line == "----" {
                self.capture = None;
                return None;
            }
            if !header {
                if let Some(d) = self.current_details() {
                    let buf = if c == Capture::Stdout {
                        &mut d.stdout
                    } else {
                        &mut d.stderr
                    };
                    buf.push_str(line);
                    buf.push('\n');
                }
                return None;
            }
            self.capture = None;
        }
        if let Some((kind, kw_len)) = block {
            let name = trimmed[kw_len..].trim().to_string();
            match kind {
                BlockKind::Feature if !indented => {
                    self.end_error();
                    self.features_seen += 1;
                    self.feature = Some(name);
                    self.scenario = None;
                    return None;
                }
                BlockKind::Scenario | BlockKind::ScenarioOutline if indented => {
                    self.end_error();
                    self.scenario = Some(name.clone());
                    if let Some(hook) = self.pending_hook.take() {
                        if let Some(d) = self.current_details() {
                            d.error = hook;
                        }
                    }
                    return Some((self.feature.clone().unwrap_or_default(), name));
                }
                BlockKind::Rule | BlockKind::Background if indented => {
                    self.end_error();
                    return None;
                }
                _ => {}
            }
        }
        if let Some(status) = step_status(line) {
            self.end_error();
            if PROBLEM_STATUSES.contains(&status) {
                self.error = Some(String::new());
            }
            return None;
        }
        if !indented && (line == "Failing scenarios:" || line == "Errored scenarios:") {
            self.end_error();
            self.scenario = None;
            return None;
        }
        if let Some(buf) = &mut self.error {
            buf.push_str(line);
            buf.push('\n');
        }
        None
    }

    fn current_details(&mut self) -> Option<&mut PlainDetails> {
        let key = (self.feature.clone()?, self.scenario.clone()?);
        Some(self.details.entry(key).or_default())
    }

    /// Stores the collected error text in the current scenario.
    pub fn end_error(&mut self) {
        let Some(buf) = self.error.take() else { return };
        let mut lines: Vec<&str> = buf.lines().collect();
        while lines
            .last()
            .is_some_and(|l| l.trim().is_empty() || l.trim() == "----")
        {
            lines.pop();
        }
        if lines.is_empty() {
            return;
        }
        let text = lines.join("\n");
        if let Some(d) = self.current_details() {
            if d.error.is_empty() {
                d.error = text;
            }
        }
    }

    pub fn details(&self, feature: &str, scenario: &str) -> Option<&PlainDetails> {
        self.details
            .get(&(feature.to_string(), scenario.to_string()))
    }
}

fn text_of(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Array(a) => Some(
            a.iter()
                .map(|l| {
                    l.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| l.to_string())
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

/// `path:line` → (path, line).
fn split_location(loc: &str) -> (String, usize) {
    match loc.rsplit_once(':') {
        Some((p, l)) if is_line(l) => (p.to_string(), l.parse().unwrap_or(0)),
        _ => (loc.to_string(), 0),
    }
}

/// behave 1.2.6 appends captured output to error messages:
/// `(error, stdout, stderr + log)`.
fn split_captured(text: &str) -> (String, String, String) {
    let (mut err, mut out, mut errout) = (Vec::new(), Vec::new(), Vec::new());
    let mut section = 0;
    for line in text.lines() {
        match line.trim_end() {
            "Captured stdout:" => section = 1,
            "Captured stderr:" | "Captured logging:" => section = 2,
            _ => match section {
                0 => err.push(line),
                1 => out.push(line),
                _ => errout.push(line),
            },
        }
    }
    let join = |v: Vec<&str>| v.join("\n").trim_end().to_string();
    (join(err), join(out), join(errout))
}

/// One-line summary of an error text: the exception line of a Python
/// traceback (without a `builtins.` prefix), otherwise the first non-empty line.
pub(crate) fn error_summary(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let unprefixed = |l: &str| {
        l.trim()
            .strip_prefix("builtins.")
            .unwrap_or(l.trim())
            .to_string()
    };
    if let Some(last_frame) = lines
        .iter()
        .rposition(|l| l.trim_start().starts_with("File \""))
    {
        let after = lines[last_frame + 1..]
            .iter()
            .find(|l| !l.trim().is_empty() && !l.starts_with(char::is_whitespace));
        if let Some(l) = after {
            return unprefixed(l);
        }
    }
    // Twisted's `Traceback (failure with no frames): builtins.X: message`.
    if let Some(rest) = lines
        .iter()
        .find_map(|l| l.trim().strip_prefix("Traceback (failure with no frames):"))
    {
        return unprefixed(rest);
    }
    if lines
        .iter()
        .any(|l| l.starts_with("Traceback (most recent call last)"))
    {
        let last = lines.iter().rev().find(|l| {
            !l.trim().is_empty()
                && !l.starts_with(char::is_whitespace)
                && !l.starts_with("Traceback (")
        });
        if let Some(l) = last {
            return unprefixed(l);
        }
    }
    lines
        .iter()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

fn outcome_of(status: &str) -> Outcome {
    match status {
        "passed" | "pending_warn" => Outcome::Passed,
        "failed" => Outcome::Failed,
        "error" | "hook_error" | "cleanup_error" | "undefined" | "pending" => Outcome::Error,
        _ => Outcome::Skipped,
    }
}

/// Converts report features into events, remembering announced ids.
#[derive(Debug)]
pub(crate) struct ReportConverter {
    root: PathBuf,
    index: BehaveIndex,
    /// Only report scenarios of the index for files it knows (narrowed runs).
    only_indexed: bool,
    pub collected: HashSet<String>,
    pub summary: Summary,
}

impl ReportConverter {
    pub fn new(root: &Path, index: BehaveIndex, only_indexed: bool) -> ReportConverter {
        ReportConverter {
            root: root.to_path_buf(),
            index,
            only_indexed,
            collected: HashSet::new(),
            summary: Summary::default(),
        }
    }

    fn rel_path(&self, file: &str) -> String {
        let p = Path::new(file);
        if p.is_absolute() {
            rel_string(&self.root, p)
        } else {
            file.replace('\\', "/").trim_start_matches("./").to_string()
        }
    }

    /// Events for one feature object of the report: `Collected` for ids not
    /// announced yet, then a `Result` per scenario.
    pub fn feature(&mut self, feature: &Value, plain: Option<&PlainTracker>) -> Vec<TestEvent> {
        let mut events = Vec::new();
        let (file, fline) = split_location(
            feature
                .get("location")
                .and_then(Value::as_str)
                .unwrap_or(""),
        );
        let rel = self.rel_path(&file);
        let abs = self.root.join(&rel);
        let raw_name = feature.get("name").and_then(Value::as_str).unwrap_or("");
        let fname = feature_name(raw_name, &rel);
        let fid = behave_feature_id(&rel, raw_name);
        let filter = self.only_indexed && self.index.contains_file(&rel);
        let mut ids = IdAllocator::default();
        let elements = feature
            .get("elements")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for el in elements
            .iter()
            .filter(|e| e.get("type").and_then(Value::as_str) != Some("background"))
        {
            let keyword = el
                .get("keyword")
                .and_then(Value::as_str)
                .unwrap_or("Scenario");
            let (_, line) =
                split_location(el.get("location").and_then(Value::as_str).unwrap_or(""));
            let display = scenario_name(
                el.get("name").and_then(Value::as_str).unwrap_or(""),
                keyword,
                line,
            );
            let named = ids.id(&fid, &display, line);
            let indexed = self.index.scenario_id(&rel, line).map(str::to_string);
            if filter && indexed.is_none() {
                continue;
            }
            let id = indexed.unwrap_or(named);
            if self.collected.insert(fid.clone()) {
                events.push(TestEvent::Collected {
                    id: fid.clone(),
                    name: fname.clone(),
                    path: Some(abs.clone()),
                    line: (fline > 0).then_some(fline),
                });
            }
            if self.collected.insert(id.clone()) {
                events.push(TestEvent::Collected {
                    id: id.clone(),
                    name: display.clone(),
                    path: Some(abs.clone()),
                    line: (line > 0).then_some(line),
                });
            }
            let details = plain.and_then(|p| p.details(&fname, &display));
            let result = scenario_result(&id, &abs, el, details);
            match result.outcome {
                Outcome::Passed => self.summary.passed += 1,
                Outcome::Failed => self.summary.failed += 1,
                Outcome::Skipped => self.summary.skipped += 1,
                Outcome::Error => self.summary.errors += 1,
            }
            self.summary.duration += result.duration;
            events.push(TestEvent::Result(result));
        }
        events
    }
}

fn scenario_result(
    id: &str,
    feature_path: &Path,
    el: &Value,
    details: Option<&PlainDetails>,
) -> TestResult {
    let status = el
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("untested");
    let steps = el
        .get("steps")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let step_status = |s: &Value| {
        s.get("result")
            .and_then(|r| r.get("status"))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let duration: f64 = steps
        .iter()
        .filter_map(|s| {
            s.get("result")
                .and_then(|r| r.get("duration"))
                .and_then(Value::as_f64)
        })
        .sum();
    let mut outcome = outcome_of(status);
    if steps
        .iter()
        .any(|s| step_status(s).as_deref() == Some("undefined"))
    {
        outcome = Outcome::Error;
    }
    let mut result = TestResult {
        id: id.to_string(),
        outcome,
        duration,
        message: String::new(),
        traceback: String::new(),
        stdout: String::new(),
        stderr: String::new(),
        xfail: false,
        expected: None,
        actual: None,
    };
    if let Some(d) = details {
        result.stdout = d.stdout.trim_end().to_string();
        result.stderr = d.stderr.trim_end().to_string();
    }
    if !outcome.is_problem() {
        return result;
    }
    let problem = steps.iter().find(|s| {
        step_status(s)
            .is_some_and(|st| PROBLEM_STATUSES.contains(&st.as_str()) || st == "undefined")
    });
    let Some(step) = problem else {
        // Hook errors and the like: no failing step.
        let error = details.map(|d| d.error.as_str()).filter(|e| !e.is_empty());
        result.message = match error {
            Some(e) => error_summary(e),
            None => format!("scenario {status}"),
        };
        result.traceback = error.unwrap_or_default().to_string();
        return result;
    };
    let st = step_status(step).unwrap_or_default();
    let keyword = step.get("keyword").and_then(Value::as_str).unwrap_or("");
    let name = step.get("name").and_then(Value::as_str).unwrap_or("");
    let step_text = if keyword.ends_with('\'') {
        format!("{keyword}{name}")
    } else {
        format!("{keyword} {name}")
    };
    let (_, step_line) = split_location(step.get("location").and_then(Value::as_str).unwrap_or(""));
    let frame = format!(
        "  File \"{}\", line {step_line}, in {step_text}",
        feature_path.display()
    );
    let reported = text_of(step.get("result").and_then(|r| r.get("error_message")));
    let (text, out, err) = match reported {
        Some(t) => split_captured(&t),
        None => (
            details.map(|d| d.error.clone()).unwrap_or_default(),
            String::new(),
            String::new(),
        ),
    };
    if !out.is_empty() {
        result.stdout = out;
    }
    if !err.is_empty() {
        result.stderr = err;
    }
    result.message = if st == "undefined" {
        format!("Undefined step: {step_text}")
    } else if text.trim().is_empty() {
        format!("Step {st}: {step_text}")
    } else {
        error_summary(&text)
    };
    result.traceback = if text.trim().is_empty() {
        frame
    } else {
        format!("{frame}\n{text}")
    };
    result
}

/// Converts a complete behave `json` / `json.pretty` report into events:
/// for every feature a `Collected` event for the feature (the suite), then
/// `Collected` and `Result` for each scenario (the tests), and a final `Done`
/// with the counts (its `exit_code` is `None`; only the process knows it).
pub fn parse_behave_json(json: &str, project_root: &Path) -> Result<Vec<TestEvent>, String> {
    parse_behave_report(json, project_root, None, None)
}

/// Like [`parse_behave_json`], resolving ids through a static index and
/// filling in error text / captured output from the `plain` formatter's
/// console output (`plain_output`), as a live run does.
pub fn parse_behave_report(
    json: &str,
    project_root: &Path,
    index: Option<&BehaveIndex>,
    plain_output: Option<&str>,
) -> Result<Vec<TestEvent>, String> {
    let features: Vec<Value> = match serde_json::from_str::<Value>(json.trim()) {
        Ok(Value::Array(a)) => a,
        Ok(_) => return Err("the behave report is not a JSON array".into()),
        Err(e) => return Err(format!("invalid behave report: {e}")),
    };
    let mut tracker = PlainTracker::default();
    for line in plain_output.unwrap_or("").lines() {
        tracker.feed(line);
    }
    tracker.end_error();
    let mut conv = ReportConverter::new(project_root, index.cloned().unwrap_or_default(), false);
    let mut events = Vec::new();
    for f in &features {
        events.extend(conv.feature(f, plain_output.map(|_| &tracker)));
    }
    events.push(TestEvent::Done(conv.summary.clone()));
    Ok(events)
}

/// State of a running behave process: static index, console tracker and the
/// incrementally read JSON report (behave writes each feature when it ends).
#[derive(Debug)]
pub(crate) struct BehaveRun {
    report: PathBuf,
    conv: ReportConverter,
    tracker: PlainTracker,
    index: BehaveIndex,
    started: HashSet<String>,
    buf: String,
    raw: Vec<u8>,
    read: u64,
    consumed: usize,
    features: Vec<Value>,
    converted: usize,
    complete: bool,
    stderr_tail: VecDeque<String>,
    stdout_tail: VecDeque<String>,
}

fn push_tail(tail: &mut VecDeque<String>, line: &str) {
    if line.trim().is_empty() {
        return;
    }
    tail.push_back(line.to_string());
    if tail.len() > 40 {
        tail.pop_front();
    }
}

impl BehaveRun {
    /// Returns the run state and `Collected` events for the statically known
    /// features and scenarios.
    pub fn new(
        root: &Path,
        report: PathBuf,
        items: &[TestItem],
        narrowed: bool,
    ) -> (BehaveRun, Vec<TestEvent>) {
        let index = BehaveIndex::from_items(items);
        let mut conv = ReportConverter::new(root, index.clone(), narrowed);
        let mut events = Vec::new();
        for module in items {
            for feature in &module.children {
                conv.collected.insert(feature.id.clone());
                events.push(TestEvent::Collected {
                    id: feature.id.clone(),
                    name: feature.name.clone(),
                    path: Some(feature.path.clone()),
                    line: Some(feature.line),
                });
                for s in &feature.children {
                    conv.collected.insert(s.id.clone());
                    events.push(TestEvent::Collected {
                        id: s.id.clone(),
                        name: s.name.clone(),
                        path: Some(s.path.clone()),
                        line: Some(s.line),
                    });
                }
            }
        }
        let run = BehaveRun {
            report,
            conv,
            tracker: PlainTracker::default(),
            index,
            started: HashSet::new(),
            buf: String::new(),
            raw: Vec::new(),
            read: 0,
            consumed: 0,
            features: Vec::new(),
            converted: 0,
            complete: false,
            stderr_tail: VecDeque::new(),
            stdout_tail: VecDeque::new(),
        };
        (run, events)
    }

    /// Feeds a console line; returns `Started` when a known scenario begins.
    pub fn stdout_line(&mut self, line: &str) -> Vec<TestEvent> {
        push_tail(&mut self.stdout_tail, line);
        let Some((feature, scenario)) = self.tracker.feed(line) else {
            return Vec::new();
        };
        let id = self
            .index
            .by_name(&feature, &scenario)
            .iter()
            .find(|id| !self.started.contains(*id))
            .cloned();
        match id {
            Some(id) => {
                self.started.insert(id.clone());
                vec![TestEvent::Started(id)]
            }
            None => Vec::new(),
        }
    }

    pub fn stderr_line(&mut self, line: &str) {
        push_tail(&mut self.stderr_tail, line);
    }

    fn read_report(&mut self) {
        let Ok(mut f) = std::fs::File::open(&self.report) else {
            return;
        };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len <= self.read || f.seek(SeekFrom::Start(self.read)).is_err() {
            return;
        }
        let mut bytes = Vec::new();
        if f.read_to_end(&mut bytes).is_err() {
            return;
        }
        self.read += bytes.len() as u64;
        self.raw.extend(bytes);
        let valid = match std::str::from_utf8(&self.raw) {
            Ok(_) => self.raw.len(),
            Err(e) => e.valid_up_to(),
        };
        self.buf
            .push_str(&String::from_utf8_lossy(&self.raw[..valid]));
        self.raw.drain(..valid);
        loop {
            let rest = &self.buf[self.consumed..];
            let after =
                rest.trim_start_matches(|c: char| c.is_whitespace() || c == '[' || c == ',');
            let skip = rest.len() - after.len();
            if after.starts_with(']') {
                self.complete = true;
                self.consumed += skip + 1;
                break;
            }
            if after.is_empty() {
                break;
            }
            let mut stream = serde_json::Deserializer::from_str(after).into_iter::<Value>();
            match stream.next() {
                Some(Ok(v)) => {
                    let off = stream.byte_offset();
                    self.features.push(v);
                    self.consumed += skip + off;
                }
                _ => break,
            }
        }
    }

    /// Reads newly written features. A feature is converted once the console
    /// shows the next feature (its captured output is complete then), or
    /// when `final_`.
    pub fn poll_report(&mut self, final_: bool) -> Vec<TestEvent> {
        self.read_report();
        if final_ {
            self.tracker.end_error();
        }
        let mut events = Vec::new();
        while self.converted < self.features.len() {
            if !final_ && self.tracker.features_seen < self.converted + 2 {
                break;
            }
            events.extend(
                self.conv
                    .feature(&self.features[self.converted], Some(&self.tracker)),
            );
            self.converted += 1;
        }
        events
    }

    /// Final events once the process exited: remaining results and `Done`,
    /// or `Failed` when behave produced no report.
    pub fn finish(&mut self, exit_code: i32, interpreter: &Path) -> Vec<TestEvent> {
        let mut events = self.poll_report(true);
        if self.features.is_empty() && !self.complete {
            let missing = self.stderr_tail.iter().any(|l| {
                l.contains("No module named behave") || l.contains("No module named 'behave'")
            });
            let msg = if missing {
                format!(
                    "behave is not installed for {0}. Install it with: {0} -m pip install behave",
                    interpreter.display()
                )
            } else {
                // Errors go to stderr; configuration / parser errors to stdout.
                let source = if self.stderr_tail.is_empty() {
                    &self.stdout_tail
                } else {
                    &self.stderr_tail
                };
                let tail: Vec<&str> = source
                    .iter()
                    .skip(source.len().saturating_sub(5))
                    .map(String::as_str)
                    .collect();
                if tail.is_empty() {
                    format!("behave exited with code {exit_code} without writing a report")
                } else {
                    format!("behave exited with code {exit_code}: {}", tail.join("\n"))
                }
            };
            events.push(TestEvent::Failed(msg));
            return events;
        }
        let mut summary = self.conv.summary.clone();
        summary.exit_code = Some(exit_code);
        events.push(TestEvent::Done(summary));
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_targets() {
        assert_eq!(
            behave_feature_id("f/a.feature", " Login "),
            "f/a.feature::Login"
        );
        assert_eq!(behave_feature_id("f/a.feature", ""), "f/a.feature::a");
        assert_eq!(id_segment("a::b [x]"), "a:b (x)");
        assert_eq!(id_segment(":::odd:: "), "odd");
        assert_eq!(id_segment("::"), "_");
        assert_eq!(split_target("f/a.feature:12"), ("f/a.feature", Some("12")));
        assert_eq!(
            split_target("f/a.feature::F::S"),
            ("f/a.feature", Some("F::S"))
        );
        assert_eq!(split_target("C:\\p\\a.feature"), ("C:\\p\\a.feature", None));
        assert_eq!(split_target("features"), ("features", None));
    }

    #[test]
    fn args() {
        let a = behave_args(
            Path::new("/tmp/r.json"),
            &["features/a.feature:12".into()],
            &["-k".into(), "login".into(), "--no-capture".into()],
        );
        assert_eq!(
            a,
            vec![
                "-m",
                "behave",
                "--format",
                "json.pretty",
                "--outfile",
                "/tmp/r.json",
                "--format",
                "plain",
                "--no-skipped",
                "--name",
                "login",
                "--no-capture",
                "features/a.feature:12"
            ]
        );
        let b = behave_args(Path::new("r.json"), &[], &[]);
        assert_eq!(b.len(), 8);
        let c = behave_args(
            Path::new("r.json"),
            &["features".into()],
            &["--tags=@smoke".into()],
        );
        assert!(c.contains(&"--no-skipped".to_string()));
        let d = behave_args(Path::new("r.json"), &["features".into()], &["-k".into()]);
        assert!(
            d.ends_with(&["-k".to_string(), "features".to_string()]),
            "a lone -k is behave's own --no-skipped"
        );
        assert!(!d.contains(&"--no-skipped".to_string()));
        let e = behave_args(
            Path::new("r.json"),
            &[],
            &["-kfoo".into(), "--show-skipped".into()],
        );
        assert!(
            e.ends_with(&[
                "--name".to_string(),
                "foo".to_string(),
                "--show-skipped".to_string()
            ]) && !e.contains(&"--no-skipped".to_string())
        );
    }

    #[test]
    fn line_selection_follows_behave() {
        let f = rustpy_gherkin::parse("Feature: F\n\n  Background:\n    Given x\n\n  Scenario: a\n    Given a\n\n  Scenario Outline: o <n>\n    Given <n>\n    Examples:\n      | n |\n      | 1 |\n      | 2 |\n\n  Rule: r\n    Scenario: in rule\n      Given r\n");
        assert_eq!(lines_selected_by(&f, 0), vec![6, 13, 14, 17]);
        assert_eq!(lines_selected_by(&f, 1), vec![6, 13, 14, 17]);
        assert_eq!(
            lines_selected_by(&f, 4),
            vec![6, 13, 14, 17],
            "background lines belong to the feature"
        );
        assert_eq!(lines_selected_by(&f, 6), vec![6]);
        assert_eq!(
            lines_selected_by(&f, 7),
            vec![6],
            "a step line selects its scenario"
        );
        assert_eq!(
            lines_selected_by(&f, 9),
            vec![13, 14],
            "an outline line selects all rows"
        );
        assert_eq!(lines_selected_by(&f, 12), vec![13, 14]);
        assert_eq!(lines_selected_by(&f, 14), vec![14]);
        assert_eq!(
            lines_selected_by(&f, 16),
            vec![17],
            "a rule line selects its scenarios"
        );
        assert_eq!(lines_selected_by(&f, 18), vec![17]);
    }

    #[test]
    fn plain_tracker() {
        let mut t = PlainTracker::default();
        assert_eq!(t.feed("Feature: Calculator"), None);
        assert_eq!(t.feed("  Background: "), None);
        assert_eq!(t.feed(""), None);
        assert_eq!(
            t.feed("  Scenario: Exploding step"),
            Some(("Calculator".into(), "Exploding step".into()))
        );
        t.feed("    When I divide 1 by 0 ... error in 0.000s");
        t.feed("Traceback (most recent call last):");
        t.feed("  File \"features/steps/calc_steps.py\", line 17, in step_divide");
        t.feed("    context.result = a / b");
        t.feed("ZeroDivisionError: division by zero");
        t.feed("");
        t.feed("----");
        t.feed("CAPTURED STDOUT: scenario");
        t.feed("hello");
        t.feed("---- CAPTURED_SCENARIO_OUTPUT_END ----");
        assert_eq!(
            t.feed("  Scenario Outline: Add 1 and 2 -- @1.1 "),
            Some(("Calculator".into(), "Add 1 and 2 -- @1.1".into()))
        );
        let d = t.details("Calculator", "Exploding step").unwrap();
        assert!(
            d.error.starts_with("Traceback")
                && d.error.ends_with("ZeroDivisionError: division by zero"),
            "{d:?}"
        );
        assert_eq!(d.stdout, "hello\n");
        assert_eq!(
            error_summary(&d.error),
            "ZeroDivisionError: division by zero"
        );
        assert_eq!(
            step_status("    Given x ... passed in 0.001s"),
            Some("passed")
        );
        assert_eq!(step_status("    Given x ... undefined"), Some("undefined"));
        assert_eq!(step_status("Given x ... passed"), None);
        assert_eq!(step_status("    Given a ... b"), None);
        // Localized headers.
        assert_eq!(t.feed("Funktionalität: Rechner"), None);
        assert_eq!(
            t.feed("  Szenario: Addieren"),
            Some(("Rechner".into(), "Addieren".into()))
        );
        assert_eq!(t.features_seen, 2);
    }

    #[test]
    fn hook_errors_belong_to_their_scenario() {
        let no_error =
            |t: &PlainTracker, s: &str| t.details("F", s).is_none_or(|d| d.error.is_empty());
        // behave 1.3: printed after the header, inside the hook's captured output.
        let mut m = PlainTracker::default();
        for l in [
            "USING RUNNER: behave.runner:Runner",
            "Feature: F",
            "  Scenario: Hooked",
            "----",
            "CAPTURED STDOUT: before_scenario",
            "HOOK-ERROR in before_scenario: RuntimeError: nope",
            "----",
            "",
        ] {
            assert_eq!(
                m.feed(l),
                if l.contains("Hooked") {
                    Some(("F".into(), "Hooked".into()))
                } else {
                    None
                }
            );
        }
        assert_eq!(
            m.feed("  Scenario: Next"),
            Some(("F".into(), "Next".into())),
            "the capture section ended"
        );
        m.end_error();
        assert_eq!(
            m.details("F", "Hooked").unwrap().error,
            "HOOK-ERROR in before_scenario: RuntimeError: nope"
        );
        assert!(no_error(&m, "Next"));
        // behave 1.2: printed before the header of its scenario.
        let mut o = PlainTracker::default();
        for l in [
            "Feature: F",
            "  Scenario: Before",
            "    Given x ... passed in 0.000s",
            "HOOK-ERROR in before_scenario: RuntimeError: nope",
            "",
        ] {
            o.feed(l);
        }
        assert_eq!(
            o.feed("  Scenario: Hooked"),
            Some(("F".into(), "Hooked".into()))
        );
        o.end_error();
        assert_eq!(
            o.details("F", "Hooked").unwrap().error,
            "HOOK-ERROR in before_scenario: RuntimeError: nope"
        );
        assert!(no_error(&o, "Before"));
    }

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/behave")
    }

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(fixtures().join(name)).unwrap()
    }

    fn result_ids(events: &[TestEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| {
                if let TestEvent::Result(r) = e {
                    Some(r.id.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    fn append(path: &Path, text: &str) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(text.as_bytes()).unwrap();
    }

    #[test]
    fn live_run_reads_the_report_incrementally() {
        let root = fixtures().join("project");
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join(BEHAVE_REPORT);
        let (items, narrowed) = items_for_targets(&root, &[]);
        assert!(!narrowed);
        let (mut run, collected) = BehaveRun::new(&root, report.clone(), &items, narrowed);
        assert_eq!(collected.len(), 2 + 9, "two features and their scenarios");
        assert!(
            matches!(&collected[0], TestEvent::Collected { id, line: Some(2), .. } if id == "features/calculator.feature::Calculator")
        );

        let json = fixture("report-1.3.3.json");
        let sep = json.find(",\n\n{").unwrap();
        let (first, second) = json.split_at(sep);
        let console = fixture("plain-1.3.3.txt");
        let rules_at = console.find("Feature: Rules").unwrap();
        let (console1, console2) = console.split_at(rules_at);

        // Half a feature on disk: nothing to report yet.
        append(&report, &first[..first.len() / 2]);
        assert!(run.poll_report(false).is_empty());
        let mut started = Vec::new();
        for line in console1.lines() {
            for e in run.stdout_line(line) {
                if let TestEvent::Started(id) = e {
                    started.push(id);
                }
            }
        }
        assert_eq!(started.len(), 8);
        assert_eq!(
            started[5],
            "features/calculator.feature::Calculator::Add 1 and 2 -- @1.1 Small numbers"
        );
        append(&report, &first[first.len() / 2..]);
        // The feature is complete but its console output may not be: wait for the next feature.
        assert!(run.poll_report(false).is_empty());
        assert!(run.stdout_line("Feature: Rules").is_empty());
        let events = run.poll_report(false);
        assert_eq!(result_ids(&events).len(), 8);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, TestEvent::Collected { .. })),
            "already announced statically"
        );
        let failing = events
            .iter()
            .find_map(|e| {
                if let TestEvent::Result(r) = e {
                    (r.id.ends_with("Failing addition")).then_some(r)
                } else {
                    None
                }
            })
            .unwrap();
        assert_eq!(failing.stdout, "adding 2 2");

        append(&report, second);
        let rest: Vec<TestEvent> = console2
            .lines()
            .skip(1)
            .flat_map(|l| run.stdout_line(l))
            .collect();
        assert_eq!(
            rest,
            vec![TestEvent::Started(
                "features/rules.feature::Rules::Positive".into()
            )]
        );
        let fin = run.finish(1, Path::new("python3"));
        assert_eq!(
            result_ids(&fin),
            vec!["features/rules.feature::Rules::Positive"]
        );
        assert!(
            matches!(fin.last(), Some(TestEvent::Done(s)) if (s.passed, s.failed, s.errors, s.skipped, s.exit_code) == (4, 2, 2, 1, Some(1)))
        );
    }

    #[test]
    fn narrowed_run_reports_only_selected_scenarios() {
        let root = fixtures().join("project");
        // `:14` is a step line of "Failing addition", `:36` an Examples row.
        let targets = vec![
            "features/calculator.feature:14".to_string(),
            "features/calculator.feature::Calculator::Add 2 and 2 -- @1.2 Small numbers"
                .to_string(),
        ];
        let (items, narrowed) = items_for_targets(&root, &targets);
        assert!(narrowed);
        let names: Vec<&str> = items[0].children[0]
            .children
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["Failing addition", "Add 2 and 2 -- @1.2 Small numbers"]
        );
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join(BEHAVE_REPORT);
        std::fs::write(&report, fixture("report-1.3.3-selected.json")).unwrap();
        let (mut run, _) = BehaveRun::new(&root, report, &items, narrowed);
        for line in fixture("plain-1.3.3-selected.txt").lines() {
            run.stdout_line(line);
        }
        let events = run.finish(1, Path::new("python3"));
        // behave lists the deselected `@skip` scenario as skipped even with --no-skipped.
        assert_eq!(
            result_ids(&events),
            vec![
                "features/calculator.feature::Calculator::Failing addition",
                "features/calculator.feature::Calculator::Add 2 and 2 -- @1.2 Small numbers"
            ]
        );
        assert!(
            matches!(events.last(), Some(TestEvent::Done(s)) if s.failed == 2 && s.total() == 2)
        );

        // An outline line selects every row; unknown ids keep the whole file.
        let (outline, _) = items_for_targets(&root, &["features/calculator.feature:29".into()]);
        assert_eq!(
            outline[0].children[0]
                .children
                .iter()
                .map(|s| s.line)
                .collect::<Vec<_>>(),
            vec![35, 36]
        );
        let (unknown, _) = items_for_targets(
            &root,
            &[
                "features/calculator.feature::Calculator::Gone".into(),
                "features/rules.feature".into(),
            ],
        );
        assert_eq!(unknown.iter().map(TestItem::test_count).sum::<usize>(), 9);
    }

    #[test]
    fn missing_report_is_a_failed_run() {
        let dir = tempfile::tempdir().unwrap();
        let (mut run, _) = BehaveRun::new(dir.path(), dir.path().join(BEHAVE_REPORT), &[], false);
        run.stderr_line("/usr/bin/python3: No module named behave");
        let events = run.finish(1, Path::new("/usr/bin/python3"));
        assert!(
            matches!(events.as_slice(), [TestEvent::Failed(m)] if m.starts_with("behave is not installed for /usr/bin/python3")),
            "{events:?}"
        );
        let (mut other, _) = BehaveRun::new(dir.path(), dir.path().join("none.json"), &[], false);
        other.stderr_line("ConfigError: No steps directory in '/x/features'");
        assert!(
            matches!(other.finish(2, Path::new("py")).as_slice(), [TestEvent::Failed(m)] if m.contains("No steps directory"))
        );
        // Parser errors are printed to stdout.
        let (mut parse_error, _) =
            BehaveRun::new(dir.path(), dir.path().join("none.json"), &[], false);
        parse_error.stdout_line(
            "ParserError: Failed to parse \"features/x.feature\": Parser failure in state=init",
        );
        parse_error.stdout_line("");
        assert!(
            matches!(parse_error.finish(1, Path::new("py")).as_slice(), [TestEvent::Failed(m)] if m == "behave exited with code 1: ParserError: Failed to parse \"features/x.feature\": Parser failure in state=init")
        );
    }

    #[test]
    fn default_targets_without_a_features_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("tests/bdd/steps")).unwrap();
        std::fs::create_dir_all(root.join("tests/bdd/more")).unwrap();
        std::fs::write(
            root.join("tests/bdd/a.feature"),
            "Feature: A\n  Scenario: s\n    Given x\n",
        )
        .unwrap();
        std::fs::write(
            root.join("tests/bdd/more/b.feature"),
            "Feature: B\n  Scenario: s\n    Given x\n",
        )
        .unwrap();
        assert_eq!(default_targets(root), vec!["tests/bdd"]);
        std::fs::create_dir_all(root.join("features")).unwrap();
        assert!(default_targets(root).is_empty(), "behave's default");
    }

    #[test]
    fn summaries() {
        assert_eq!(
            error_summary("ASSERT FAILED: expected 5 but got 4"),
            "ASSERT FAILED: expected 5 but got 4"
        );
        assert_eq!(error_summary("Traceback (most recent call last):\n  File \"x.py\", line 1, in f\n    boom()\nbuiltins.RuntimeError: boom\nmore"), "RuntimeError: boom");
        assert_eq!(error_summary(""), "");
        assert_eq!(
            error_summary("Traceback (failure with no frames): builtins.RuntimeError: boom\n"),
            "RuntimeError: boom"
        );
        assert_eq!(error_summary("Traceback (most recent call last):\nFailure: twisted.internet.defer.CancelledError: \n"), "Failure: twisted.internet.defer.CancelledError:");
        assert_eq!(
            split_captured(
                "Assertion Failed: x\nCaptured stdout:\nadding 2 2\nCaptured logging:\nINFO:x"
            ),
            (
                "Assertion Failed: x".into(),
                "adding 2 2".into(),
                "INFO:x".into()
            )
        );
    }
}
