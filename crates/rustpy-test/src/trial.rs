//! Twisted Trial support: the `python -m twisted.trial` command line and a
//! streaming parser for trial's text reporters — `timing` (what the IDE
//! runs: `module.Class.test ... [OK]` followed by `(0.001 secs)`),
//! `bwverbose` (the same without timings) and the default `verbose` tree —
//! turning their output into [`TestEvent`]s.
//!
//! Trial names tests `module.Class.method`; results use the pytest-style ids
//! of the rest of the crate (`tests/test_x.py::Class::method`), mapping module
//! names back to files with [`TrialIds`]. Failure details (tracebacks, skip
//! and todo reasons) only appear in the summary trial prints at the end of a
//! run: a live parser reports each test as soon as its status is printed and,
//! when it did not pass, again with the details from the summary (the second
//! result replaces the first). A non-live parser reports each test once.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::behave::error_summary;
use crate::discover::{discover_file_in, discover_project, TestItem, TestKind, SKIP_DIRS};
use crate::runner::{Outcome, Summary, TestEvent, TestResult};

/// Reporter the IDE passes to trial (`--reporter=timing`).
pub const TRIAL_REPORTER: &str = "timing";

const DOUBLE_SEPARATOR: &str =
    "===============================================================================";
const SEPARATOR: &str =
    "-------------------------------------------------------------------------------";

/// The module name trial gives a Python file (Twisted's
/// `filenameToModuleName`): the file stem prefixed by every enclosing package
/// (directory with `__init__.py`). Also returns the directory that must be
/// on `sys.path` to import the module under that name.
pub fn trial_module_name(file: &Path) -> (String, PathBuf) {
    let mut name = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
    while dir.join("__init__.py").is_file() {
        let Some(pkg) = dir.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            break;
        };
        name = format!("{pkg}.{name}");
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => break,
        }
    }
    (name, dir)
}

/// True when Python source uses Twisted Trial (`twisted.trial` imports).
pub fn file_uses_trial(text: &str) -> bool {
    text.contains("twisted.trial")
}

fn rel_string(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Maps trial's dotted test names to node ids and source locations.
#[derive(Clone, Debug, Default)]
pub struct TrialIds {
    root: PathBuf,
    /// Module name → absolute file.
    modules: HashMap<String, PathBuf>,
    /// Module names known not to be files under the root.
    missing: HashSet<String>,
    /// Node id → (file, line) of discovered tests, classes and modules.
    locations: HashMap<String, (PathBuf, usize)>,
    files: HashSet<PathBuf>,
}

impl TrialIds {
    /// An empty mapping; module names are resolved against `root` on demand
    /// (`pkg.mod` → `pkg/mod.py`), files added with [`TrialIds::add_file`]
    /// are found under the name trial gives them.
    pub fn new(root: &Path) -> TrialIds {
        TrialIds {
            root: root.to_path_buf(),
            ..TrialIds::default()
        }
    }

    /// A mapping that knows every Python test module of the project (as
    /// [`discover_project`] finds them).
    pub fn for_project(root: &Path) -> TrialIds {
        let mut ids = TrialIds::new(root);
        for m in discover_project(root) {
            ids.add_discovered(&m.path, &m.children);
        }
        ids
    }

    /// Registers a Python file (absolute, or relative to the root) under its
    /// trial module name and its root-relative dotted name, with the
    /// locations of the tests it defines.
    pub fn add_file(&mut self, file: &Path) {
        let abs = if file.is_absolute() {
            file.to_path_buf()
        } else {
            self.root.join(file)
        };
        if self.files.contains(&abs) {
            return;
        }
        let items = std::fs::read_to_string(&abs)
            .map(|text| discover_file_in(&self.root, &abs, &text))
            .unwrap_or_default();
        self.add_discovered(&abs, &items);
    }

    /// Registers the absolute file `abs` with its statically discovered items.
    fn add_discovered(&mut self, abs: &Path, items: &[TestItem]) {
        if !self.files.insert(abs.to_path_buf()) {
            return;
        }
        let (name, _) = trial_module_name(abs);
        self.modules
            .entry(name)
            .or_insert_with(|| abs.to_path_buf());
        let rel = rel_string(&self.root, abs);
        if let Some(dotted) = rel
            .strip_suffix(".py")
            .filter(|_| !Path::new(&rel).is_absolute())
        {
            let dotted = dotted
                .strip_suffix("/__init__")
                .unwrap_or(dotted)
                .replace('/', ".");
            self.modules
                .entry(dotted)
                .or_insert_with(|| abs.to_path_buf());
        }
        self.locations.insert(rel, (abs.to_path_buf(), 1));
        for it in items.iter().flat_map(|i| i.walk()) {
            self.locations
                .insert(it.id.clone(), (abs.to_path_buf(), it.line));
        }
    }

    fn module_file(&mut self, module: &str) -> Option<PathBuf> {
        if let Some(f) = self.modules.get(module) {
            return Some(f.clone());
        }
        if self.missing.contains(module)
            || module.is_empty()
            || module
                .split('.')
                .any(|s| s.is_empty() || s.contains(['/', '\\']))
        {
            return None;
        }
        let base = self.root.join(module.replace('.', "/"));
        let candidates = [base.with_extension("py"), base.join("__init__.py")];
        match candidates.into_iter().find(|c| c.is_file()) {
            Some(f) => {
                self.modules.insert(module.to_string(), f.clone());
                self.add_file(&f);
                Some(f)
            }
            None => {
                self.missing.insert(module.to_string());
                None
            }
        }
    }

    /// The node id for a trial test name: `test_x.Case.test_a` →
    /// `tests/test_x.py::Case::test_a` (the longest dotted prefix that is a
    /// known module selects the file). File paths (trial's id for a module
    /// that failed to import) become root-relative paths; unknown names are
    /// returned unchanged.
    pub fn node_id(&mut self, trial_id: &str) -> String {
        let t = trial_id.trim();
        if t.ends_with(".py") || t.contains(['/', '\\']) {
            let p = Path::new(t);
            let abs = if p.is_absolute() {
                p.to_path_buf()
            } else {
                self.root.join(p)
            };
            return rel_string(&self.root, &abs);
        }
        match self.split_dotted(t) {
            Some((file, None)) => rel_string(&self.root, &file),
            Some((file, Some(member))) => format!("{}::{member}", rel_string(&self.root, &file)),
            None => t.to_string(),
        }
    }

    /// Splits a dotted name into the file of its longest known module prefix
    /// and the `::`-joined rest (`Class::method`).
    fn split_dotted(&mut self, dotted: &str) -> Option<(PathBuf, Option<String>)> {
        let segs: Vec<&str> = dotted.split('.').collect();
        for k in (1..=segs.len()).rev() {
            if let Some(file) = self.module_file(&segs[..k].join(".")) {
                let member = (k < segs.len()).then(|| segs[k..].join("::"));
                return Some((file, member));
            }
        }
        None
    }

    /// File and line of a node id (the file alone for unknown members).
    pub fn location(&self, node_id: &str) -> (Option<PathBuf>, Option<usize>) {
        if let Some((p, l)) = self.locations.get(node_id) {
            return (Some(p.clone()), Some(*l));
        }
        let file = node_id.split("::").next().unwrap_or(node_id);
        match self.locations.get(file) {
            Some((p, _)) => (Some(p.clone()), None),
            None => (None, None),
        }
    }
}

/// One status token of a progress line.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Status {
    Ok,
    Failure,
    Error,
    Skipped,
    Todo,
    UnexpectedSuccess,
}

const STATUS_TOKENS: &[(&str, Status)] = &[
    ("[OK]", Status::Ok),
    ("[FAILURE]", Status::Failure),
    ("[FAIL]", Status::Failure),
    ("[ERROR]", Status::Error),
    ("[SKIPPED]", Status::Skipped),
    ("[TODO]", Status::Todo),
    ("[SUCCESS!?!]", Status::UnexpectedSuccess),
];

impl Status {
    /// `(outcome, xfail)`: trial's todo is pytest's xfail, an unexpected
    /// success (which does not fail a trial run) an xpass.
    fn outcome(self) -> (Outcome, bool) {
        match self {
            Status::Ok => (Outcome::Passed, false),
            Status::Failure => (Outcome::Failed, false),
            Status::Error => (Outcome::Error, false),
            Status::Skipped => (Outcome::Skipped, false),
            Status::Todo => (Outcome::Skipped, true),
            Status::UnexpectedSuccess => (Outcome::Passed, true),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Status::Ok => "passed",
            Status::Failure => "failed",
            Status::Error => "error",
            Status::Skipped => "skipped",
            Status::Todo => "expected failure",
            Status::UnexpectedSuccess => "unexpected success",
        }
    }
}

fn rank(o: Outcome) -> u8 {
    match o {
        Outcome::Passed => 0,
        Outcome::Skipped => 1,
        Outcome::Failed => 2,
        Outcome::Error => 3,
    }
}

/// Splits the status tokens off the end of a progress line:
/// `hello[OK]` → `("hello", [Ok])`, `  [FAILURE][ERROR]` → `("", [Failure, Error])`.
fn split_statuses(s: &str) -> Option<(&str, Vec<Status>)> {
    let mut rest = s.trim_end();
    let mut found = Vec::new();
    while let Some((tok, st)) = STATUS_TOKENS.iter().find(|(t, _)| rest.ends_with(t)) {
        found.push(*st);
        rest = &rest[..rest.len() - tok.len()];
    }
    if found.is_empty() {
        return None;
    }
    found.reverse();
    Some((rest.trim_end(), found))
}

/// `(indent, name, rest)` of a `name ... rest` progress line.
fn progress_start(line: &str) -> Option<(usize, &str, &str)> {
    let (head, rest) = match line.split_once(" ... ") {
        Some(x) => x,
        None => (line.strip_suffix(" ...")?, ""),
    };
    let name = head.trim();
    let indent = head.len() - head.trim_start().len();
    if name.is_empty() || head.trim_end().len() != head.len() {
        return None;
    }
    let ok_chars = !name.contains(char::is_whitespace) || name.ends_with(".py");
    ok_chars.then_some((indent, name, rest))
}

/// `(0.001 secs)` → seconds.
fn timing(line: &str) -> Option<f64> {
    line.trim()
        .strip_prefix('(')?
        .strip_suffix(" secs)")?
        .parse()
        .ok()
}

/// Removes ANSI color sequences (trial colors the tree reporter on terminals).
fn strip_ansi(s: &str) -> String {
    if !s.contains('\u{1b}') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// `twisted.trial.unittest.FailTest: x` → `FailTest: x` (the exception
/// line of a traceback with its class name unqualified).
fn exception_line(traceback: &str) -> String {
    let line = error_summary(traceback);
    let (class, rest) = match line.split_once(": ") {
        Some((c, r)) => (c, Some(r)),
        None => (line.strip_suffix(':').unwrap_or(&line), None),
    };
    if class.contains(char::is_whitespace) || !class.contains('.') {
        return line;
    }
    let short = class.rsplit('.').next().unwrap_or(class);
    match rest {
        Some(r) => format!("{short}: {r}"),
        None => short.to_string(),
    }
}

/// `Reason: 'known bug'` → `known bug`.
fn todo_reason(line: &str) -> Option<String> {
    let r = line.strip_prefix("Reason: ")?.trim();
    let unquoted = r
        .strip_prefix('\'')
        .and_then(|x| x.strip_suffix('\''))
        .or_else(|| r.strip_prefix('"').and_then(|x| x.strip_suffix('"')));
    Some(unquoted.unwrap_or(r).to_string())
}

#[derive(Clone, Debug)]
struct TrialTest {
    id: String,
    outcome: Option<Outcome>,
    xfail: bool,
    duration: f64,
    output: Vec<String>,
    message: String,
    traceback: String,
    /// Details from the summary were applied.
    detailed: bool,
    /// A result with everything known so far was emitted.
    final_emitted: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Section {
    Tests,
    Summary,
    Footer,
}

/// Streaming parser of trial's console output (see the module docs).
#[derive(Debug)]
pub struct TrialParser {
    ids: TrialIds,
    live: bool,
    section: Section,
    /// Prelude segments (module, class) of the tree reporter.
    tree: Vec<String>,
    current: Option<usize>,
    awaiting_time: Option<usize>,
    tests: Vec<TrialTest>,
    by_id: HashMap<String, usize>,
    collected: HashSet<String>,
    block: Vec<String>,
    ran: Option<(usize, f64)>,
    verdict: Option<bool>,
    /// `(passed, failed, skipped, errors)` from the verdict line.
    verdict_counts: Option<(usize, usize, usize, usize)>,
    /// Per-test progress lines were seen (not so for the `text` reporter).
    progress_seen: bool,
}

impl TrialParser {
    /// A parser resolving ids with `ids`. A `live` parser reports problems
    /// as soon as their status is printed (and again with details).
    pub fn new(ids: TrialIds, live: bool) -> TrialParser {
        TrialParser {
            ids,
            live,
            section: Section::Tests,
            tree: Vec::new(),
            current: None,
            awaiting_time: None,
            tests: Vec::new(),
            by_id: HashMap::new(),
            collected: HashSet::new(),
            block: Vec::new(),
            ran: None,
            verdict: None,
            verdict_counts: None,
            progress_seen: false,
        }
    }

    /// Marks ids as already announced (no `Collected` event for them).
    pub fn mark_collected(&mut self, ids: impl IntoIterator<Item = String>) {
        self.collected.extend(ids);
    }

    /// True once trial's final `PASSED` / `FAILED` line was seen.
    pub fn saw_verdict(&self) -> bool {
        self.verdict.is_some()
    }

    /// Number of tests seen so far.
    pub fn test_count(&self) -> usize {
        self.tests.len()
    }

    fn test_index(&mut self, trial_id: &str, events: &mut Vec<TestEvent>) -> usize {
        let id = self.ids.node_id(trial_id);
        if let Some(&i) = self.by_id.get(&id) {
            return i;
        }
        if self.collected.insert(id.clone()) {
            let (path, line) = self.ids.location(&id);
            let name = id.rsplit("::").next().unwrap_or(&id).to_string();
            events.push(TestEvent::Collected {
                id: id.clone(),
                name,
                path,
                line,
            });
        }
        self.tests.push(TrialTest {
            id: id.clone(),
            outcome: None,
            xfail: false,
            duration: 0.0,
            output: Vec::new(),
            message: String::new(),
            traceback: String::new(),
            detailed: false,
            final_emitted: false,
        });
        self.by_id.insert(id, self.tests.len() - 1);
        self.tests.len() - 1
    }

    /// Feeds one line of trial's stdout.
    pub fn feed(&mut self, raw: &str) -> Vec<TestEvent> {
        let mut events = Vec::new();
        let clean = strip_ansi(raw);
        let line = clean.trim_end_matches(['\r', '\n']);
        match self.section {
            Section::Summary => {
                self.summary_line(line, &mut events);
                return events;
            }
            Section::Footer => {
                self.footer_line(line);
                return events;
            }
            Section::Tests => {}
        }
        if let Some(i) = self.awaiting_time.take() {
            if let Some(secs) = timing(line) {
                self.tests[i].duration = secs;
                self.progress_done(i, &mut events);
                return events;
            }
            self.progress_done(i, &mut events);
        }
        if let Some(i) = self.current {
            match split_statuses(line) {
                Some((before, statuses)) => {
                    self.push_output(i, before);
                    self.complete(i, &statuses);
                }
                None => self.tests[i].output.push(line.to_string()),
            }
            return events;
        }
        if line == DOUBLE_SEPARATOR {
            self.section = Section::Summary;
            self.block.clear();
            return events;
        }
        if line == SEPARATOR {
            self.section = Section::Footer;
            self.end_summary(&mut events);
            return events;
        }
        if let Some((indent, name, rest)) = progress_start(line) {
            let level = indent / 2;
            let trial_id = if indent == 0 {
                (name.contains(['.', '/', '\\'])).then(|| name.to_string())
            } else if indent.is_multiple_of(2) && level <= self.tree.len() {
                Some(format!("{}.{name}", self.tree[..level].join(".")))
            } else {
                None
            };
            if let Some(trial_id) = trial_id {
                self.progress_seen = true;
                let i = self.test_index(&trial_id, &mut events);
                events.push(TestEvent::Started(self.tests[i].id.clone()));
                self.current = Some(i);
                match split_statuses(rest) {
                    Some((before, statuses)) => {
                        self.push_output(i, before);
                        self.complete(i, &statuses);
                    }
                    None => self.push_output(i, rest),
                }
                return events;
            }
        }
        // Tree reporter prelude: `module` / `  Class` lines.
        let name = line.trim();
        let indent = line.len() - line.trim_start().len();
        if !name.is_empty()
            && !name.contains(char::is_whitespace)
            && indent.is_multiple_of(2)
            && indent / 2 <= self.tree.len()
        {
            self.tree.truncate(indent / 2);
            self.tree.push(name.to_string());
        }
        events
    }

    fn push_output(&mut self, i: usize, text: &str) {
        if !text.trim().is_empty() {
            self.tests[i].output.push(text.to_string());
        }
    }

    fn set_outcome(&mut self, i: usize, status: Status) {
        let (o, xfail) = status.outcome();
        let t = &mut self.tests[i];
        if t.outcome
            .is_none_or(|prev| rank(o) > rank(prev) || (rank(o) == rank(prev) && xfail))
        {
            t.outcome = Some(o);
            t.xfail = xfail;
        }
    }

    /// The status of the running test was printed.
    fn complete(&mut self, i: usize, statuses: &[Status]) {
        for s in statuses {
            self.set_outcome(i, *s);
        }
        let worst = statuses
            .iter()
            .copied()
            .max_by_key(|s| rank(s.outcome().0))
            .unwrap_or(Status::Ok);
        if self.tests[i].message.is_empty() && worst != Status::Ok {
            self.tests[i].message = worst.label().to_string();
        }
        self.current = None;
        self.awaiting_time = Some(i);
    }

    /// A test's progress line is complete (status and timing).
    fn progress_done(&mut self, i: usize, events: &mut Vec<TestEvent>) {
        let t = &self.tests[i];
        let clean_pass = t.outcome == Some(Outcome::Passed) && !t.xfail;
        if clean_pass {
            self.tests[i].final_emitted = true;
            events.push(TestEvent::Result(self.result(i)));
        } else if self.live {
            events.push(TestEvent::Result(self.result(i)));
        }
    }

    fn result(&self, i: usize) -> TestResult {
        let t = &self.tests[i];
        TestResult {
            id: t.id.clone(),
            outcome: t.outcome.unwrap_or(Outcome::Error),
            duration: t.duration,
            message: t.message.clone(),
            traceback: t.traceback.clone(),
            stdout: t.output.join("\n"),
            stderr: String::new(),
            xfail: t.xfail,
            expected: None,
            actual: None,
        }
    }

    fn summary_line(&mut self, line: &str, events: &mut Vec<TestEvent>) {
        if line == DOUBLE_SEPARATOR {
            self.end_block(events);
        } else if line == SEPARATOR {
            self.end_block(events);
            self.section = Section::Footer;
            self.end_summary(events);
        } else {
            self.block.push(line.to_string());
        }
    }

    /// One `[FLAVOR]` block of the summary: reason text, an empty line, and
    /// the ids of every test with that same reason.
    fn end_block(&mut self, events: &mut Vec<TestEvent>) {
        let lines = std::mem::take(&mut self.block);
        let Some(flavor) = lines.first() else { return };
        let Some(status) = STATUS_TOKENS
            .iter()
            .find(|(t, _)| flavor.trim() == *t)
            .map(|(_, s)| *s)
        else {
            return;
        };
        let body = &lines[1..];
        let (reason_lines, id_lines) = match body.iter().rposition(|l| l.trim().is_empty()) {
            Some(i) => (&body[..i], &body[i + 1..]),
            None => body.split_at(body.len().saturating_sub(1)),
        };
        let reason = reason_lines.join("\n").trim_end().to_string();
        let ids: Vec<String> = id_lines
            .iter()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        for trial_id in ids {
            let i = self.test_index(&trial_id, events);
            self.apply_detail(i, status, &reason);
        }
    }

    fn apply_detail(&mut self, i: usize, status: Status, reason: &str) {
        self.set_outcome(i, status);
        let (message, traceback) = match status {
            Status::Ok => return,
            Status::Skipped => (reason.trim().to_string(), String::new()),
            Status::Todo | Status::UnexpectedSuccess => {
                let mut lines = reason.lines();
                let first = lines.next().unwrap_or("");
                let (todo, rest) = match todo_reason(first) {
                    Some(t) => (t, lines.collect::<Vec<_>>().join("\n")),
                    None => (String::new(), reason.to_string()),
                };
                let todo_note = if todo.is_empty() {
                    String::new()
                } else {
                    format!(" (todo: {todo})")
                };
                if status == Status::Todo {
                    let exc = exception_line(&rest);
                    let msg = if exc.is_empty() {
                        format!("expected failure{todo_note}")
                    } else {
                        format!("expected failure: {exc}{todo_note}")
                    };
                    (msg, rest.trim_end().to_string())
                } else {
                    (
                        format!("unexpected success{todo_note}"),
                        rest.trim_end().to_string(),
                    )
                }
            }
            Status::Failure | Status::Error => (exception_line(reason), reason.to_string()),
        };
        let t = &mut self.tests[i];
        if !t.detailed || t.message.is_empty() {
            t.message = message;
        }
        if !traceback.is_empty() {
            if t.detailed && !t.traceback.is_empty() {
                t.traceback.push_str("\n\n");
                t.traceback.push_str(&traceback);
            } else {
                t.traceback = traceback;
            }
        }
        t.detailed = true;
        t.final_emitted = false;
    }

    /// The summary is complete: report every test whose details arrived (or
    /// that was not reported yet).
    fn end_summary(&mut self, events: &mut Vec<TestEvent>) {
        if let Some(i) = self.awaiting_time.take() {
            self.progress_done(i, events);
        }
        for i in 0..self.tests.len() {
            if !self.tests[i].final_emitted
                && (self.tests[i].detailed || self.tests[i].outcome.is_some())
            {
                self.tests[i].final_emitted = true;
                events.push(TestEvent::Result(self.result(i)));
            }
        }
    }

    fn footer_line(&mut self, line: &str) {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("Ran ") {
            let mut words = rest.split_whitespace();
            let n = words.next().and_then(|w| w.parse::<usize>().ok());
            let secs = rest
                .rsplit_once(" in ")
                .and_then(|(_, s)| s.strip_suffix('s'))
                .and_then(|s| s.parse::<f64>().ok());
            if let Some(n) = n {
                self.ran = Some((n, secs.unwrap_or(0.0)));
            }
        } else if let Some(rest) = l
            .strip_prefix("PASSED")
            .or_else(|| l.strip_prefix("FAILED"))
        {
            if !(rest.is_empty() || rest.starts_with(" (")) {
                return;
            }
            self.verdict = Some(l.starts_with("PASSED"));
            // `FAILED (skips=2, expectedFailures=1, failures=2, errors=2, unexpectedSuccesses=1, successes=2)`
            let counts = rest.trim().trim_start_matches('(').trim_end_matches(')');
            let get = |key: &str| {
                counts
                    .split(", ")
                    .find_map(|kv| {
                        kv.strip_prefix(key)?
                            .strip_prefix('=')?
                            .parse::<usize>()
                            .ok()
                    })
                    .unwrap_or(0)
            };
            self.verdict_counts = Some((
                get("successes") + get("unexpectedSuccesses"),
                get("failures"),
                get("skips") + get("expectedFailures"),
                get("errors"),
            ));
        }
    }

    /// Flushes everything and returns the remaining results plus `Done`.
    /// `exit_code` defaults to what trial's verdict implies (0 / 1).
    pub fn finish(&mut self, exit_code: Option<i32>) -> Vec<TestEvent> {
        let mut events = Vec::new();
        if self.section == Section::Summary {
            self.end_block(&mut events);
        }
        self.end_summary(&mut events);
        let mut summary = Summary::default();
        match self.verdict_counts.filter(|_| !self.progress_seen) {
            // Reporters without per-test lines (`text`): passing tests are only counted.
            Some((passed, failed, skipped, errors)) => {
                summary.passed = passed;
                summary.failed = failed;
                summary.skipped = skipped;
                summary.errors = errors;
            }
            None => {
                for t in &self.tests {
                    match t.outcome {
                        Some(Outcome::Passed) => summary.passed += 1,
                        Some(Outcome::Failed) => summary.failed += 1,
                        Some(Outcome::Skipped) => summary.skipped += 1,
                        Some(Outcome::Error) => summary.errors += 1,
                        None => {}
                    }
                }
            }
        }
        summary.duration = self
            .ran
            .map(|(_, s)| s)
            .unwrap_or_else(|| self.tests.iter().map(|t| t.duration).sum());
        summary.exit_code = exit_code.or_else(|| self.verdict.map(|ok| if ok { 0 } else { 1 }));
        events.push(TestEvent::Done(summary));
        events
    }
}

/// Parses the complete console output of a trial run (`timing`,
/// `bwverbose` or the default `verbose` reporter) into events: `Collected`,
/// `Started` and one `Result` per test (with skip / todo reasons and
/// tracebacks from the summary), then `Done`. Ids are resolved against the
/// project's Python test modules.
pub fn parse_trial_output(output: &str, project_root: &Path) -> Vec<TestEvent> {
    let mut p = TrialParser::new(TrialIds::for_project(project_root), false);
    let mut events: Vec<TestEvent> = output.lines().flat_map(|l| p.feed(l)).collect();
    events.extend(p.finish(None));
    events
}

/// Arguments after the interpreter: `-m twisted.trial --reporter=timing
/// [extra] [targets]` (the reporter is left alone when `extra_args` choose one).
pub fn trial_args(trial_targets: &[String], extra_args: &[String]) -> Vec<String> {
    let mut args: Vec<String> = vec!["-m".into(), "twisted.trial".into()];
    if !extra_args
        .iter()
        .any(|a| a == "--reporter" || a.starts_with("--reporter="))
    {
        args.push(format!("--reporter={TRIAL_REPORTER}"));
    }
    args.extend(extra_args.iter().cloned());
    args.extend(trial_targets.iter().cloned());
    args
}

/// A trial run: arguments after the interpreter, directories to put on
/// `PYTHONPATH` (so dotted names import the intended files), the id
/// mapping, and the statically known tests.
#[derive(Debug)]
pub(crate) struct TrialPlan {
    pub args: Vec<String>,
    pub pythonpath: Vec<PathBuf>,
    pub ids: TrialIds,
    pub tests: Vec<TestItem>,
}

fn leaves(items: &[TestItem]) -> Vec<TestItem> {
    items
        .iter()
        .flat_map(|i| i.walk())
        .filter(|i| i.kind == TestKind::Function)
        .cloned()
        .collect()
}

/// `test_*.py` files under `dir` (trial's package recursion).
fn trial_files_under(dir: &Path) -> Vec<PathBuf> {
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
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("test_") && n.ends_with(".py"))
        })
        .collect();
    files.sort();
    files
}

/// Test modules of the project trial can run (they define `TestCase`s).
fn project_trial_files(root: &Path) -> Vec<PathBuf> {
    discover_project(root)
        .into_iter()
        .filter(|m| {
            m.children.iter().any(|c| c.kind == TestKind::Class)
                && std::fs::read_to_string(&m.path).is_ok_and(|t| t.contains("TestCase"))
        })
        .map(|m| m.path)
        .collect()
}

enum Target {
    /// A test module, optionally narrowed to `Class` / `Class::method`.
    File(PathBuf, Option<String>),
    /// A dotted name trial resolves itself.
    Dotted(String),
}

pub(crate) fn plan(root: &Path, targets: &[String], extra_args: &[String]) -> TrialPlan {
    // pytest-style `-k EXPR` (trial has no name filter): select tests statically.
    let mut keyword: Option<String> = None;
    let mut extra: Vec<String> = Vec::new();
    let mut i = 0;
    while i < extra_args.len() {
        let a = &extra_args[i];
        if a == "-k" && i + 1 < extra_args.len() {
            keyword = Some(extra_args[i + 1].clone());
            i += 2;
            continue;
        }
        match a.strip_prefix("-k").filter(|k| !k.is_empty()) {
            Some(k) => keyword = Some(k.to_string()),
            None => extra.push(a.clone()),
        }
        i += 1;
    }

    let mut ids = TrialIds::new(root);
    let mut resolved: Vec<Target> = Vec::new();
    if targets.is_empty() {
        resolved.extend(
            project_trial_files(root)
                .into_iter()
                .map(|f| Target::File(f, None)),
        );
    }
    for t in targets {
        let (file_part, rest) = match t.split_once("::") {
            Some((f, r)) => (f, Some(r.to_string())),
            None => (t.as_str(), None),
        };
        let p = Path::new(file_part);
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(p)
        };
        if abs.is_dir() {
            resolved.extend(
                trial_files_under(&abs)
                    .into_iter()
                    .map(|f| Target::File(f, None)),
            );
        } else if abs.is_file() {
            resolved.push(Target::File(abs, rest.filter(|r| !r.is_empty())));
        } else if let Some((file, member)) = (rest.is_none() && !t.contains(['/', '\\']))
            .then(|| ids.split_dotted(t))
            .flatten()
        {
            // `pkg.tests.test_x.Case.test` of a module under the root.
            resolved.push(Target::File(file, member));
        } else {
            resolved.push(Target::Dotted(t.clone()));
        }
    }

    let mut tests: Vec<TestItem> = Vec::new();
    let mut args_targets: Vec<String> = Vec::new();
    let mut pythonpath: Vec<PathBuf> = Vec::new();
    for target in resolved {
        let (file, member) = match target {
            Target::Dotted(d) => {
                // Not a module under the root: trial resolves it (unfiltered by `-k`).
                args_targets.push(d);
                continue;
            }
            Target::File(f, m) => (f, m),
        };
        let (module, sys_path) = trial_module_name(&file);
        if sys_path != root && !pythonpath.contains(&sys_path) {
            pythonpath.push(sys_path);
        }
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let rel = rel_string(root, &file);
        let discovered = discover_file_in(root, &file, &text);
        ids.add_discovered(&file, &discovered);
        let all = leaves(&discovered);
        let selected: Vec<TestItem> = match &member {
            Some(m) => {
                let wanted = format!("{rel}::{m}");
                all.into_iter()
                    .filter(|it| it.id == wanted || it.id.starts_with(&format!("{wanted}::")))
                    .collect()
            }
            None => all,
        };
        match &keyword {
            Some(k) => {
                let k = k.to_lowercase();
                for it in selected {
                    let dotted = format!(
                        "{module}.{}",
                        it.id.split("::").skip(1).collect::<Vec<_>>().join(".")
                    );
                    if dotted.to_lowercase().contains(&k) || it.id.to_lowercase().contains(&k) {
                        args_targets.push(dotted);
                        tests.push(it);
                    }
                }
            }
            None => {
                args_targets.push(match &member {
                    Some(m) => format!("{module}.{}", m.replace("::", ".")),
                    None => rel,
                });
                tests.extend(selected);
            }
        }
    }
    let mut seen = HashSet::new();
    args_targets.retain(|a| seen.insert(a.clone()));
    let mut seen = HashSet::new();
    tests.retain(|t| seen.insert(t.id.clone()));
    TrialPlan {
        args: trial_args(&args_targets, &extra),
        pythonpath,
        ids,
        tests,
    }
}

/// State of a running trial process.
#[derive(Debug)]
pub(crate) struct TrialRun {
    parser: TrialParser,
    stderr_tail: VecDeque<String>,
}

impl TrialRun {
    /// Returns the run state and `Collected` events for the statically known tests.
    pub fn new(plan: TrialPlan) -> (TrialRun, Vec<TestEvent>) {
        let events: Vec<TestEvent> = plan
            .tests
            .iter()
            .map(|t| TestEvent::Collected {
                id: t.id.clone(),
                name: t.name.clone(),
                path: Some(t.path.clone()),
                line: Some(t.line),
            })
            .collect();
        let mut parser = TrialParser::new(plan.ids, true);
        parser.mark_collected(plan.tests.into_iter().map(|t| t.id));
        (
            TrialRun {
                parser,
                stderr_tail: VecDeque::new(),
            },
            events,
        )
    }

    pub fn stdout_line(&mut self, line: &str) -> Vec<TestEvent> {
        self.parser.feed(line)
    }

    pub fn stderr_line(&mut self, line: &str) {
        self.stderr_tail.push_back(line.to_string());
        if self.stderr_tail.len() > 40 {
            self.stderr_tail.pop_front();
        }
    }

    /// Final events once the process exited: remaining results and `Done`,
    /// or `Failed` when trial did not run (not installed, bad arguments).
    pub fn finish(&mut self, exit_code: i32, interpreter: &Path) -> Vec<TestEvent> {
        if !self.parser.saw_verdict() && self.parser.test_count() == 0 {
            let stderr: Vec<&str> = self
                .stderr_tail
                .iter()
                .map(String::as_str)
                .filter(|l| !l.trim().is_empty())
                .collect();
            let missing = stderr.iter().any(|l| {
                l.contains("No module named twisted") || l.contains("No module named 'twisted")
            });
            let msg = if missing {
                format!(
                    "Twisted is not installed for {0}. Install it with: {0} -m pip install twisted",
                    interpreter.display()
                )
            } else if stderr.is_empty() {
                format!("trial exited with code {exit_code} without running tests")
            } else {
                format!(
                    "trial exited with code {exit_code}: {}",
                    stderr[stderr.len().saturating_sub(5)..].join("\n")
                )
            };
            return vec![TestEvent::Failed(msg)];
        }
        self.parser.finish(Some(exit_code))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_tokens() {
        assert_eq!(split_statuses("[OK]"), Some(("", vec![Status::Ok])));
        assert_eq!(
            split_statuses("hello[OK]  "),
            Some(("hello", vec![Status::Ok]))
        );
        assert_eq!(
            split_statuses("        [FAILURE][ERROR]"),
            Some(("", vec![Status::Failure, Status::Error]))
        );
        assert_eq!(
            split_statuses("   [FAIL]"),
            Some(("", vec![Status::Failure]))
        );
        assert_eq!(
            split_statuses("[SUCCESS!?!]"),
            Some(("", vec![Status::UnexpectedSuccess]))
        );
        assert_eq!(split_statuses("plain output"), None);
        assert_eq!(timing("(0.051 secs)"), Some(0.051));
        assert_eq!(timing("(fast)"), None);
    }

    #[test]
    fn progress_lines() {
        assert_eq!(
            progress_start("test_a.T.test_x ... [OK]"),
            Some((0, "test_a.T.test_x", "[OK]"))
        );
        assert_eq!(
            progress_start("    test_x ...      [SKIPPED]"),
            Some((4, "test_x", "     [SKIPPED]"))
        );
        assert_eq!(progress_start("a.b.c ..."), Some((0, "a.b.c", "")));
        assert_eq!(progress_start("Loading things ... done"), None);
        assert_eq!(
            progress_start("my tests/test_x.py ... [ERROR]"),
            Some((0, "my tests/test_x.py", "[ERROR]"))
        );
        assert_eq!(strip_ansi("\u{1b}[32m[OK]\u{1b}[0m"), "[OK]");
    }

    #[test]
    fn exception_lines() {
        assert_eq!(exception_line("Traceback (most recent call last):\n  File \"t.py\", line 1, in f\n    x\ntwisted.trial.unittest.FailTest: 1 != 2 : one"), "FailTest: 1 != 2 : one");
        assert_eq!(exception_line("Traceback (most recent call last):\n  File \"t.py\", line 1, in f\n    x\nbuiltins.RuntimeError: boom"), "RuntimeError: boom");
        assert_eq!(exception_line("Traceback (most recent call last):\n  File \"t.py\", line 1, in f\n    x\ntwisted.internet.defer.CancelledError"), "CancelledError");
        assert_eq!(exception_line("not today"), "not today");
        assert_eq!(exception_line("Traceback (failure with no frames): twisted.internet.error.ReactorNotRestartable: \n"), "ReactorNotRestartable");
        assert_eq!(todo_reason("Reason: 'known bug'"), Some("known bug".into()));
        assert_eq!(todo_reason("Reason: \"it's\""), Some("it's".into()));
    }

    #[test]
    fn module_names_follow_packages() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("pkg/sub")).unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("pkg/__init__.py"), "").unwrap();
        std::fs::write(root.join("pkg/sub/__init__.py"), "").unwrap();
        std::fs::write(root.join("pkg/sub/test_a.py"), "from twisted.trial import unittest\nclass A(unittest.TestCase):\n    def test_x(self):\n        pass\n").unwrap();
        std::fs::write(
            root.join("tests/test_b.py"),
            "import unittest\nclass B(unittest.TestCase):\n    def test_y(self):\n        pass\n",
        )
        .unwrap();
        assert_eq!(
            trial_module_name(&root.join("pkg/sub/test_a.py")),
            ("pkg.sub.test_a".to_string(), root.to_path_buf())
        );
        assert_eq!(
            trial_module_name(&root.join("tests/test_b.py")),
            ("test_b".to_string(), root.join("tests"))
        );
        let mut ids = TrialIds::new(root);
        ids.add_file(&root.join("tests/test_b.py"));
        assert_eq!(ids.node_id("test_b.B.test_y"), "tests/test_b.py::B::test_y");
        assert_eq!(
            ids.location("tests/test_b.py::B::test_y"),
            (Some(root.join("tests/test_b.py")), Some(3))
        );
        // Packages resolve from the root without registration.
        assert_eq!(
            ids.node_id("pkg.sub.test_a.A.test_x"),
            "pkg/sub/test_a.py::A::test_x"
        );
        assert_eq!(ids.location("pkg/sub/test_a.py::A::test_x").1, Some(3));
        assert_eq!(ids.node_id("pkg.sub.test_a"), "pkg/sub/test_a.py");
        assert_eq!(ids.node_id("tests/test_b.py"), "tests/test_b.py");
        assert_eq!(
            ids.node_id(&root.join("tests/test_b.py").to_string_lossy()),
            "tests/test_b.py"
        );
        assert_eq!(ids.node_id("nosuch.Mod.test"), "nosuch.Mod.test");
        assert!(file_uses_trial("from twisted.trial import unittest"));
        assert!(!file_uses_trial("import unittest"));
    }

    #[test]
    fn plans_targets_and_keywords() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("tests/test_b.py"), "from twisted.trial import unittest\nclass B(unittest.TestCase):\n    def test_y(self):\n        pass\n    def test_z(self):\n        pass\n").unwrap();
        std::fs::write(
            root.join("tests/test_plain.py"),
            "def test_f():\n    pass\n",
        )
        .unwrap();
        std::fs::write(root.join("tests/helper.py"), "X = 1\n").unwrap();

        let whole = plan(&root, &[], &[]);
        assert_eq!(
            whole.args,
            vec![
                "-m",
                "twisted.trial",
                "--reporter=timing",
                "tests/test_b.py"
            ]
        );
        assert_eq!(whole.pythonpath, vec![root.join("tests")]);
        assert_eq!(
            whole
                .tests
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            vec!["tests/test_b.py::B::test_y", "tests/test_b.py::B::test_z"]
        );

        let node = plan(
            &root,
            &[
                "tests/test_b.py::B::test_z".into(),
                root.join("tests/test_b.py").to_string_lossy().into_owned() + "::B",
            ],
            &["-x".into()],
        );
        assert_eq!(
            node.args,
            vec![
                "-m",
                "twisted.trial",
                "--reporter=timing",
                "-x",
                "test_b.B.test_z",
                "test_b.B"
            ]
        );
        assert_eq!(node.tests.len(), 2);

        let dir_target = plan(
            &root,
            &["tests".into(), "pkg.mod.Case".into()],
            &["--reporter=bwverbose".into()],
        );
        assert_eq!(
            dir_target.args,
            vec![
                "-m",
                "twisted.trial",
                "--reporter=bwverbose",
                "tests/test_b.py",
                "tests/test_plain.py",
                "pkg.mod.Case"
            ]
        );

        let keyword = plan(&root, &[], &["-k".into(), "Z".into()]);
        assert_eq!(
            keyword.args,
            vec![
                "-m",
                "twisted.trial",
                "--reporter=timing",
                "test_b.B.test_z"
            ]
        );
        assert_eq!(
            keyword
                .tests
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            vec!["test_z"]
        );
        let none = plan(&root, &[], &["-knothing".into()]);
        assert_eq!(none.args, vec!["-m", "twisted.trial", "--reporter=timing"]);

        // Dotted names of modules under the root are resolved to their files
        // (trial itself cannot import `tests.test_b` without `__init__.py`).
        let dotted = plan(
            &root,
            &["tests.test_b.B.test_y".into(), "pkg.mod.Case".into()],
            &["-k".into(), "test".into()],
        );
        assert_eq!(
            dotted.args,
            vec![
                "-m",
                "twisted.trial",
                "--reporter=timing",
                "test_b.B.test_y",
                "pkg.mod.Case"
            ]
        );
        assert_eq!(dotted.pythonpath, vec![root.join("tests")]);
        assert_eq!(
            dotted
                .tests
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            vec!["tests/test_b.py::B::test_y"]
        );
    }

    #[test]
    fn parses_bwverbose_with_shared_summary_blocks() {
        let out = "\
a.T.test_one ... [SKIPPED]
a.T.test_two ... [SKIPPED]
a.T.test_three ... some output
more output
[FAILURE][ERROR]

===============================================================================
[SKIPPED]
same reason

a.T.test_one
a.T.test_two
===============================================================================
[FAIL]
Traceback (most recent call last):
  File \"a.py\", line 3, in test_three
twisted.trial.unittest.FailTest: nope

a.T.test_three
===============================================================================
[ERROR]
Traceback (failure with no frames): twisted.internet.error.ReactorNotRestartable:

a.T.test_three
-------------------------------------------------------------------------------
Ran 3 tests in 0.010s

FAILED (skips=2, failures=1, errors=1)
";
        let mut p = TrialParser::new(TrialIds::new(Path::new("/nonexistent-root")), false);
        let mut events: Vec<TestEvent> = out.lines().flat_map(|l| p.feed(l)).collect();
        events.extend(p.finish(None));
        let results: Vec<&TestResult> = events
            .iter()
            .filter_map(|e| {
                if let TestEvent::Result(r) = e {
                    Some(r)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(results.len(), 3, "{events:#?}");
        assert!(results
            .iter()
            .take(2)
            .all(|r| r.outcome == Outcome::Skipped && r.message == "same reason"));
        let three = results[2];
        assert_eq!(three.id, "a.T.test_three");
        assert_eq!(three.outcome, Outcome::Error);
        assert_eq!(three.message, "FailTest: nope");
        assert!(
            three.traceback.contains("FailTest: nope")
                && three.traceback.contains("ReactorNotRestartable"),
            "{three:?}"
        );
        assert_eq!(three.stdout, "some output\nmore output");
        let Some(TestEvent::Done(s)) = events.last() else {
            panic!()
        };
        assert_eq!(
            (s.passed, s.failed, s.skipped, s.errors, s.exit_code),
            (0, 0, 2, 1, Some(1))
        );
        assert!((s.duration - 0.010).abs() < 1e-9);
    }

    #[test]
    fn live_parser_reports_early_then_details() {
        let mut p = TrialParser::new(TrialIds::new(Path::new("/nonexistent-root")), true);
        let mut events = p.feed("a.T.test_x ... [FAILURE]");
        events.extend(p.feed("(0.002 secs)"));
        let early: Vec<&TestResult> = events
            .iter()
            .filter_map(|e| {
                if let TestEvent::Result(r) = e {
                    Some(r)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(early.len(), 1);
        assert_eq!(
            (
                early[0].outcome,
                early[0].message.as_str(),
                early[0].duration
            ),
            (Outcome::Failed, "failed", 0.002)
        );
        let mut later = Vec::new();
        for l in [
            "",
            DOUBLE_SEPARATOR,
            "[FAIL]",
            "Traceback (most recent call last):",
            "builtins.AssertionError: x",
            "",
            "a.T.test_x",
            SEPARATOR,
            "Ran 1 tests in 0.002s",
            "",
            "FAILED (failures=1)",
        ] {
            later.extend(p.feed(l));
        }
        let detailed: Vec<&TestResult> = later
            .iter()
            .filter_map(|e| {
                if let TestEvent::Result(r) = e {
                    Some(r)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(detailed.len(), 1);
        assert_eq!(detailed[0].message, "AssertionError: x");
        assert!(p.saw_verdict());
        assert!(
            matches!(p.finish(Some(1)).last(), Some(TestEvent::Done(s)) if s.failed == 1 && s.exit_code == Some(1))
        );
    }
}
