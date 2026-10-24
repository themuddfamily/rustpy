//! Parser corpus tests (roadmap §2 "Quality: parser corpus tests").
//!
//! The tests are `#[ignore]`d because they depend on the local Python
//! installation and take a few seconds. Run them with
//!
//! ```text
//! cargo test -p rustpy-syntax --release -- --ignored corpus
//! ```
//!
//! * [`corpus_stdlib_parses_without_errors`] walks every `/usr/lib/python3.*`
//!   directory (plus the directories listed in `RUSTPY_CORPUS_DIRS`, separated
//!   like `PATH`, e.g. a CPython checkout's `Lib` or a `site-packages`),
//!   parses each `*.py` file and fails if a file that CPython accepts produces
//!   a syntax error. Panics and lossless round-trip failures fail the test for
//!   every file, valid or not. Files we report errors for are classified by
//!   asking CPython itself
//!   (`python3 -c "import ast,sys; ast.parse(open(sys.argv[1],'rb').read())" FILE`;
//!   set `RUSTPY_CORPUS_PYTHON` to use another interpreter): files CPython
//!   rejects too are intentionally invalid and only listed. Files that are not
//!   UTF-8 (legacy encodings) are skipped.
//! * [`corpus_mutated_sources_never_panic`] parses randomly broken copies of
//!   the same files (truncated, spliced, with unbalanced brackets or quotes
//!   inserted...), as an editor sees them while code is typed: the parser
//!   must not panic and the tree must stay lossless. `RUSTPY_CORPUS_SEED`
//!   changes the mutations.
//! * [`corpus_perf_100k_lines`] concatenates stdlib modules into a ~100k-line
//!   file and asserts that it parses in under 2 s (in a release build).

use std::cell::Cell;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, Once};
use std::time::{Duration, Instant};

/// The tests of this file use all cores (or measure time), so they run one
/// at a time even though the test harness starts them in parallel.
fn exclusive() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `/usr/lib/python3.*` plus the extra directories from `RUSTPY_CORPUS_DIRS`.
fn corpus_roots(with_extra: bool) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = std::fs::read_dir("/usr/lib")
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.is_dir()
                        && p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with("python3."))
                })
                .collect()
        })
        .unwrap_or_default();
    roots.sort();
    if with_extra {
        if let Some(extra) = std::env::var_os("RUSTPY_CORPUS_DIRS") {
            // Explicit roots are declarations, not optional discovery hints: an
            // absent/unreadable path must fail collection rather than disappear.
            roots.extend(std::env::split_paths(&extra));
        }
    }
    roots
}

/// Recursively collects `*.py` files. Symlinked directories are not followed
/// (they would only produce duplicates or cycles).
fn collect_py_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let rd = std::fs::read_dir(dir).unwrap_or_else(|error| {
        panic!(
            "corpus: cannot enumerate declared input {}: {error}",
            dir.display()
        )
    });
    for entry in rd {
        let entry = entry.unwrap_or_else(|error| {
            panic!(
                "corpus: cannot enumerate entry in {}: {error}",
                dir.display()
            )
        });
        let path = entry.path();
        let ft = entry
            .file_type()
            .unwrap_or_else(|error| panic!("corpus: cannot inspect {}: {error}", path.display()));
        if ft.is_dir() {
            if entry.file_name() != "__pycache__" {
                collect_py_files(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "py") {
            // Keep the existing file-symlink behavior, but metadata failures
            // must not turn a declared fixture into an invisible omission.
            let is_file = if ft.is_symlink() {
                std::fs::metadata(&path)
                    .unwrap_or_else(|error| {
                        panic!("corpus: cannot inspect {}: {error}", path.display())
                    })
                    .is_file()
            } else {
                ft.is_file()
            };
            if is_file {
                out.push(path);
            }
        }
    }
}

fn corpus_files(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for root in roots {
        collect_py_files(root, &mut files);
    }
    files.sort();
    files.dedup();
    files
}

/// An ignored acceptance gate with no exercised input is unavailable, not a pass.
fn require_corpus_work(count: usize, kind: &str) {
    assert!(count > 0, "corpus: no actual {kind}; configure readable corpus roots with eligible UTF-8 Python sources");
}

#[test]
#[should_panic(expected = "no actual parsed mutants")]
fn empty_mutation_work_is_not_a_successful_gate() {
    require_corpus_work(0, "parsed mutants");
}
#[test]
#[should_panic(expected = "cannot enumerate declared input")]
fn missing_declared_root_is_not_silently_skipped() {
    collect_py_files(Path::new(""), &mut Vec::new());
}
#[test]
fn unreadable_declared_source_retains_its_error() {
    assert!(matches!(process(Path::new("")), Outcome::Unreadable(error) if !error.is_empty()));
}

/// Files that are intentionally invalid Python 3 (CPython's own test data):
/// `bad*` / `badsyntax_*` modules, Python 2 sources and lib2to3's fixtures
/// (`lib2to3/tests/data` up to 3.10, `test/test_lib2to3/data` since 3.11).
/// `test_grammar.py` and friends are *not* excluded: they must parse.
fn excluded_by_name(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let normalized = path.to_string_lossy().replace('\\', "/");
    name.starts_with("bad")
        || name.contains("_py2")
        || normalized.contains("/lib2to3/tests/data/")
        || normalized.contains("/test_lib2to3/data/")
}

/// 1-based line and column (in characters) of a byte offset.
fn line_col(src: &str, offset: usize) -> (usize, usize) {
    let mut offset = offset.min(src.len());
    while !src.is_char_boundary(offset) {
        offset -= 1;
    }
    let before = &src[..offset];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, col)
}

enum Verdict {
    Accepts,
    Rejects(String),
    Unknown(String),
}

/// Asks CPython whether it can parse the file.
fn cpython_verdict(python: &str, path: &Path) -> Verdict {
    let out = Command::new(python)
        .arg("-c")
        .arg("import ast,sys; ast.parse(open(sys.argv[1],'rb').read())")
        .arg(path)
        .output();
    match out {
        Ok(o) if o.status.success() => Verdict::Accepts,
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            let last = stderr
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim()
                .to_string();
            Verdict::Rejects(last)
        }
        Err(e) => Verdict::Unknown(format!("could not run {python}: {e}")),
    }
}

enum Problem {
    Panic(String),
    RoundTrip,
    Errors {
        line: usize,
        col: usize,
        message: String,
        count: usize,
    },
}

impl Problem {
    fn describe(&self, path: &Path) -> String {
        match self {
            Problem::Panic(msg) => format!("{}: parser panicked: {msg}", path.display()),
            Problem::RoundTrip => format!(
                "{}: syntax tree text differs from the source (lossless round-trip broken)",
                path.display()
            ),
            Problem::Errors {
                line,
                col,
                message,
                count,
            } => format!(
                "{}:{line}:{col}: {message} ({count} error{})",
                path.display(),
                if *count == 1 { "" } else { "s" }
            ),
        }
    }
}

thread_local! {
    /// Set while [`check_file`] runs the parser: its panics are caught and
    /// reported per file, so the panic hook stays quiet for them.
    static IN_PARSER: Cell<bool> = const { Cell::new(false) };
}

/// Installs (once) a panic hook that silences panics raised inside
/// [`check_file`] and reports all others as usual.
fn quiet_parser_panics() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !IN_PARSER.with(Cell::get) {
                default(info);
            }
        }));
    });
}

fn check_file(src: &str) -> Option<Problem> {
    quiet_parser_panics();
    IN_PARSER.with(|f| f.set(true));
    let result = std::panic::catch_unwind(|| {
        let parse = rustpy_syntax::parse(src);
        let lossless = parse.syntax_node().text() == src;
        (parse, lossless)
    });
    IN_PARSER.with(|f| f.set(false));
    match result {
        Err(payload) => {
            let msg = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            Some(Problem::Panic(msg))
        }
        Ok((_, false)) => Some(Problem::RoundTrip),
        Ok((parse, true)) => {
            let first = parse.errors().first()?;
            let (line, col) = line_col(src, u32::from(first.range.start()) as usize);
            Some(Problem::Errors {
                line,
                col,
                message: first.message.clone(),
                count: parse.errors().len(),
            })
        }
    }
}

/// What happened to one corpus file.
enum Outcome {
    Excluded,
    Unreadable(String),
    NotUtf8,
    Parsed {
        bytes: usize,
        lines: usize,
        problem: Option<Problem>,
    },
}

fn process(path: &Path) -> Outcome {
    if excluded_by_name(path) {
        return Outcome::Excluded;
    }
    let data = match std::fs::read(path) {
        Ok(data) => data,
        Err(error) => return Outcome::Unreadable(error.to_string()),
    };
    let Ok(src) = String::from_utf8(data) else {
        return Outcome::NotUtf8;
    };
    Outcome::Parsed {
        bytes: src.len(),
        lines: src.lines().count(),
        problem: check_file(&src),
    }
}

/// Runs `f` on every item on all cores; results keep the order of `items`.
fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(items.len().max(1));
    let next = AtomicUsize::new(0);
    let mut results: Vec<Option<R>> = std::iter::repeat_with(|| None).take(items.len()).collect();
    std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut done = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(i) else { break };
                        done.push((i, f(item)));
                    }
                    done
                })
            })
            .collect();
        for worker in workers {
            for (i, r) in worker.join().expect("corpus worker thread panicked") {
                results[i] = Some(r);
            }
        }
    });
    results
        .into_iter()
        .map(|r| r.expect("every item is processed"))
        .collect()
}

#[test]
#[ignore = "walks the local CPython installation; run with `cargo test -p rustpy-syntax --release -- --ignored corpus`"]
fn corpus_stdlib_parses_without_errors() {
    let _serial = exclusive();
    let roots = corpus_roots(true);
    require_corpus_work(roots.len(), "declared roots");
    let python = std::env::var("RUSTPY_CORPUS_PYTHON").unwrap_or_else(|_| "python3".to_string());
    let files = corpus_files(&roots);
    require_corpus_work(files.len(), "discovered Python files");

    let started = Instant::now();
    let outcomes = par_map(&files, |path| process(path));
    let wall = started.elapsed();

    let (mut excluded, mut non_utf8, mut unreadable, mut parsed, mut clean) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
    let (mut bytes, mut lines) = (0usize, 0usize);
    let mut failures: Vec<String> = Vec::new();
    let mut invalid: Vec<String> = Vec::new();
    for (path, outcome) in files.iter().zip(outcomes) {
        let (size, line_count, problem) = match outcome {
            Outcome::Excluded => {
                excluded += 1;
                continue;
            }
            Outcome::Unreadable(error) => {
                unreadable += 1;
                failures.push(format!(
                    "{}: unreadable declared input: {error}",
                    path.display()
                ));
                continue;
            }
            Outcome::NotUtf8 => {
                non_utf8 += 1;
                continue;
            }
            Outcome::Parsed {
                bytes,
                lines,
                problem,
            } => (bytes, lines, problem),
        };
        parsed += 1;
        bytes += size;
        lines += line_count;
        match problem {
            None => clean += 1,
            Some(p @ (Problem::Panic(_) | Problem::RoundTrip)) => failures.push(p.describe(path)),
            Some(p @ Problem::Errors { .. }) => match cpython_verdict(&python, path) {
                Verdict::Accepts => failures.push(p.describe(path)),
                Verdict::Rejects(why) => {
                    invalid.push(format!("{}  [CPython: {why}]", p.describe(path)))
                }
                Verdict::Unknown(why) => failures.push(format!("{}  [{why}]", p.describe(path))),
            },
        }
    }

    let mut report = String::new();
    let roots_list = roots
        .iter()
        .map(|r| r.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let secs = wall.as_secs_f64();
    let _ = writeln!(
        report,
        "\n=== rustpy-syntax parser corpus: {roots_list} ==="
    );
    let _ = writeln!(report, "files: {} found, {excluded} excluded by name, {non_utf8} not UTF-8 (skipped), {unreadable} unreadable", files.len());
    let _ = writeln!(
        report,
        "parsed: {parsed} files, {lines} lines, {:.1} MB in {secs:.2} s wall clock on available capped workers",
        bytes as f64 / 1e6
    );
    let _ = writeln!(report, "clean: {clean}; rejected by CPython too (intentionally invalid, not counted): {}; FAILURES: {}", invalid.len(), failures.len());
    if !invalid.is_empty() {
        let _ = writeln!(report, "-- files CPython ({python}) rejects as well:");
        for line in &invalid {
            let _ = writeln!(report, "   {line}");
        }
    }
    if !failures.is_empty() {
        let _ = writeln!(
            report,
            "-- FAILURES (CPython accepts these files, or the parser panicked / lost text):"
        );
        for line in &failures {
            let _ = writeln!(report, "   {line}");
        }
    }
    eprintln!("{report}");
    require_corpus_work(parsed, "parsed source files");
    assert!(
        failures.is_empty(),
        "{} corpus file(s) failed to parse cleanly:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// SplitMix64: a tiny deterministic PRNG for the mutation test.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Fragments that tend to confuse a Python lexer or parser when spliced
/// into otherwise valid code (unbalanced brackets and quotes, soft keywords,
/// f-string pieces, continuations, odd whitespace).
const FRAGMENTS: &[&str] = &[
    "(",
    ")",
    "[",
    "]",
    "{",
    "}",
    "'",
    "\"",
    "'''",
    "\"\"\"",
    "f'",
    "f\"",
    "f'{",
    "f\"{x!r:{",
    "rf'\\",
    "t'{",
    "\\",
    "\\\n",
    "\n",
    "\r",
    "\r\n",
    "\t",
    "    ",
    "\x0c",
    ":",
    ";",
    ",",
    "lambda",
    "lambda *, a:",
    "match",
    "match x:\n",
    "case",
    "case _:",
    "type",
    "type X = ",
    "if",
    "else",
    "elif",
    "def",
    "class",
    "@",
    "*",
    "**",
    ":=",
    "#",
    "async",
    "await",
    "yield",
    "->",
    "...",
    "0x",
    "1e",
    "1_",
    "0b2",
    "\\N{",
    "u'",
    "rb'",
    "except*",
    "with (",
    "|",
    "not in",
    "is not",
    "global",
    "nonlocal",
    "print",
    "from . import",
    "import",
    "return",
    "try:",
    "finally:",
    "=",
    "==",
    "<>",
    "!",
    "$",
    "?",
    "`",
    "é",
    "\u{feff}",
    "\u{200b}",
    "((((((((((",
    "))))))))))",
    "[[[[[",
    "{{{{{",
    "}}}}}",
    "f'{f'{f'{",
    "\"\"\"\n",
    "if x:\n",
    "    pass\n",
    "\n\t\t\t",
];

/// Applies one random edit to `src`.
fn mutate(src: &str, rng: &mut Rng) -> String {
    // The char boundary at or before byte `i`.
    let floor = |s: &str, i: usize| {
        let mut i = i.min(s.len());
        while !s.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let (a, b) = {
        let (x, y) = (
            floor(src, rng.below(src.len() + 1)),
            floor(src, rng.below(src.len() + 1)),
        );
        (x.min(y), x.max(y))
    };
    match rng.below(6) {
        // Truncate.
        0 => src[..a].to_string(),
        // Delete a short range.
        1 => {
            let end = floor(src, a + 1 + rng.below(40));
            format!("{}{}", &src[..a], &src[end..])
        }
        // Insert a few fragments.
        2 | 3 => {
            let mut s = src[..a].to_string();
            for _ in 0..1 + rng.below(3) {
                s.push_str(FRAGMENTS[rng.below(FRAGMENTS.len())]);
            }
            s.push_str(&src[a..]);
            s
        }
        // Move a range elsewhere (swaps lines, splits statements, ...).
        4 => {
            let piece = &src[a..floor(src, b.min(a + 200))];
            let rest = format!("{}{}", &src[..a], &src[a + piece.len()..]);
            let at = floor(&rest, rng.below(rest.len() + 1));
            format!("{}{piece}{}", &rest[..at], &rest[at..])
        }
        // Duplicate a range.
        _ => format!("{}{}", &src[..b], &src[a..]),
    }
}

#[test]
#[ignore = "mutation fuzzing of the local stdlib; run with `cargo test -p rustpy-syntax --release -- --ignored corpus`"]
fn corpus_mutated_sources_never_panic() {
    let _serial = exclusive();
    const MUTANTS_PER_FILE: usize = 8;
    let files: Vec<PathBuf> = corpus_files(&corpus_roots(true))
        .into_iter()
        .filter(|p| !excluded_by_name(p))
        .collect();
    require_corpus_work(files.len(), "eligible mutation source files");
    let seed: u64 = std::env::var("RUSTPY_CORPUS_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x5EED);
    let started = Instant::now();
    // Per file: (mutants parsed, failures, slowest parse/size, non-UTF8 skip).
    let results = par_map(&files, |path| {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) => {
                return (
                    0,
                    vec![format!(
                        "{}: unreadable declared mutation input: {error}",
                        path.display()
                    )],
                    (Duration::ZERO, 0),
                    0,
                )
            }
        };
        let src = match String::from_utf8(bytes) {
            Ok(source) => source,
            Err(_) => return (0, Vec::new(), (Duration::ZERO, 0), 1),
        };
        let mut rng = Rng(seed
            ^ path
                .to_string_lossy()
                .bytes()
                .fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b))));
        let mut failures = Vec::new();
        let mut slowest = (Duration::ZERO, 0);
        let mut current = src;
        for i in 0..MUTANTS_PER_FILE {
            // Mutations accumulate, so later mutants are more broken.
            current = mutate(&current, &mut rng);
            let t = Instant::now();
            let problem = match check_file(&current) {
                Some(p @ (Problem::Panic(_) | Problem::RoundTrip)) => Some(p),
                _ => None,
            };
            slowest = slowest.max((t.elapsed(), current.len()));
            if let Some(p) = problem {
                failures.push(format!(
                    "{} (mutant {i}, RUSTPY_CORPUS_SEED={seed})",
                    p.describe(path)
                ));
            }
        }
        (MUTANTS_PER_FILE, failures, slowest, 0)
    });
    let parsed_sources = results.iter().filter(|result| result.0 > 0).count();
    let non_utf8: usize = results.iter().map(|result| result.3).sum();
    let mutants: usize = results.iter().map(|r| r.0).sum();
    let (slowest, slowest_file) = files
        .iter()
        .zip(&results)
        .map(|(f, r)| (r.2, f))
        .max()
        .map_or(((Duration::ZERO, 0), None), |(s, f)| (s, Some(f)));
    let failures: Vec<String> = results.into_iter().flat_map(|r| r.1).collect();
    eprintln!(
        "\n=== rustpy-syntax mutation fuzzing: {mutants} mutants of {parsed_sources} parsed sources ({} eligible files; {non_utf8} not UTF-8, skipped) in {:.2} s; FAILURES: {} ===\nslowest parse: {:.1} ms for a {:.0} kB mutant of {}",
        files.len(),
        started.elapsed().as_secs_f64(),
        failures.len(),
        slowest.0.as_secs_f64() * 1e3,
        slowest.1 as f64 / 1e3,
        slowest_file.map_or_else(String::new, |f| f.display().to_string()),
    );
    for f in &failures {
        eprintln!("   {f}");
    }
    require_corpus_work(parsed_sources, "parsed mutation sources");
    require_corpus_work(mutants, "parsed mutants");
    assert!(
        failures.is_empty(),
        "{} mutation input/parser contract failure(s)",
        failures.len()
    );
}

/// Fallback when no stdlib is installed: a module exercising most constructs.
const SYNTHETIC_MODULE: &str = r#"import os, sys
from typing import Any as _Any

@decorator(arg=1)
class Point[T](Base, metaclass=Meta):
    """Docstring."""
    x: int = 0

    def __init__(self, x: T, /, y: int = 2, *args, z=3, **kw) -> None:
        self.x, self.y = x, y
        values = [i ** 2 for i in range(10) if i % 2 if i]
        mapping = {k: v for k, v in kw.items()}
        total = sum(v for v in values)
        text = f"{self.x!r:>{width}} {total=} {'nested'}"
        while (n := len(values)) > 3:
            values.pop()
        try:
            result = await_me(lambda a, *b, c=1, **d: (a, b, c, d))
        except* (ValueError, TypeError) as exc:
            raise RuntimeError("boom") from exc
        finally:
            pass
        match self.x:
            case Point(x=0, y=yy) | [1, 2, *rest] if yy:
                return yy
            case {"key": value, **others}:
                return value
            case _:
                return None

async def main(*, retries: int = 3) -> list[str]:
    async with session() as s, other() as o:
        async for item in s:
            yield item[1:2, ::3]
    return [x async for x in aiter()]

type Alias[K, V] = dict[K, V] | None
"#;

#[test]
#[ignore = "timing test; run with `cargo test -p rustpy-syntax --release -- --ignored corpus`"]
fn corpus_perf_100k_lines() {
    let _serial = exclusive();
    const TARGET_LINES: usize = 100_000;
    let mut big = String::new();
    let (mut lines, mut modules) = (0usize, 0usize);
    for path in corpus_files(&corpus_roots(false)) {
        if lines >= TARGET_LINES {
            break;
        }
        if excluded_by_name(&path) {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        // Only modules that parse cleanly on their own, so the whole file is valid.
        if !rustpy_syntax::parse(&src).errors().is_empty() {
            continue;
        }
        big.push_str(&src);
        if !src.ends_with('\n') {
            big.push('\n');
        }
        lines += src.lines().count();
        modules += 1;
    }
    let source = if modules == 0 {
        "synthetic module"
    } else {
        "stdlib modules"
    };
    if modules == 0 {
        modules = 1;
    }
    while lines < TARGET_LINES {
        big.push_str(SYNTHETIC_MODULE);
        lines += SYNTHETIC_MODULE.lines().count();
    }

    // Warm-up (page faults, allocator), then time a few runs and keep the median.
    let warm = rustpy_syntax::parse(&big);
    assert!(
        warm.errors().is_empty(),
        "concatenated file has errors: {:?}",
        &warm.errors()[..warm.errors().len().min(5)]
    );
    drop(warm);
    let t = Instant::now();
    let (tokens, _) = rustpy_syntax::tokenize(&big);
    let lex_time = t.elapsed();
    let mut runs: Vec<Duration> = (0..5)
        .map(|_| {
            let t = Instant::now();
            let parse = rustpy_syntax::parse(&big);
            let elapsed = t.elapsed();
            assert!(parse.errors().is_empty());
            elapsed
        })
        .collect();
    runs.sort();
    let median = runs[runs.len() / 2];
    let mb = big.len() as f64 / 1e6;
    eprintln!(
        "\n=== rustpy-syntax parse performance ===\n{lines} lines / {mb:.1} MB / {} tokens from {modules} {source}\nlex: {:.1} ms; parse (lex + parse + tree): median {:.1} ms, best {:.1} ms, worst {:.1} ms ({:.1} MB/s, {:.0} klines/s)",
        tokens.len(),
        lex_time.as_secs_f64() * 1e3,
        median.as_secs_f64() * 1e3,
        runs[0].as_secs_f64() * 1e3,
        runs[runs.len() - 1].as_secs_f64() * 1e3,
        mb / median.as_secs_f64(),
        lines as f64 / median.as_secs_f64() / 1e3,
    );
    // The 2 s budget applies to optimized builds; debug builds get more slack.
    let budget = if cfg!(debug_assertions) {
        Duration::from_secs(20)
    } else {
        Duration::from_secs(2)
    };
    assert!(
        median < budget,
        "parsing {lines} lines took {median:?} (budget {budget:?})"
    );
}
