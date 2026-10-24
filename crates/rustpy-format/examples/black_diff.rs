//! Differential test harness: `FormatOptions::black()` versus the real Black.
//!
//! ```text
//! cargo run -p rustpy-format --example black_diff -- \
//!     --black /path/to/venv/bin/black [--root /usr/lib/python3.12] [--sample 300] \
//!     [--examples 2] [--list] [--diff] [-l 120] [-S] [-C] [--files-from list.txt] [files...]
//! ```
//!
//! Collects every `*.py` file under `--root` (sorted by path), takes a fixed
//! sample of `--sample` files (every k-th file, k = total / sample) and formats
//! each one with Black (`black -q -`, default line length 88, source on stdin)
//! and with our formatter. It prints:
//!
//! * `files` — sampled files that Black can format (Black errors are listed separately);
//! * `identical` — our output is byte-identical to Black's;
//! * `idempotent` — formatting our output again changes nothing;
//! * `ast-equal` — `python3` parses the input and our output to the same AST
//!   (compared like Black's own safety check: docstring indentation and
//!   trailing whitespace are normalized, `del (a, b)` equals `del a, b`);
//!   inputs `python3` itself cannot parse count as equal and are reported
//!   separately;
//! * the most frequent categories of the *first* differing hunk per file.
//!
//! Black's outputs are cached in `target/black-diff-cache` (keyed by a hash of
//! the source), so only the first run pays for spawning Black.
//! Positional paths (or `--files-from <file>`, one path per line) diff just
//! those files instead of a sample; `--diff` prints
//! every first hunk, `--examples N` prints N sample hunks per category and
//! `--list` prints each non-identical file with its category.
//!
//! `-l/--line-length N`, `-S/--skip-string-normalization` and
//! `-C/--skip-magic-trailing-comma` are passed to Black and mapped onto
//! `FormatOptions` (`max_line_length`, `QuoteStyle::Preserve`,
//! `magic_trailing_comma: false`).

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use rustpy_format::{format_source, FormatOptions, QuoteStyle};

struct Args {
    black: String,
    python: String,
    root: PathBuf,
    sample: usize,
    examples: usize,
    list: bool,
    diff: bool,
    jobs: usize,
    files: Vec<PathBuf>,
    cache: PathBuf,
    line_length: usize,
    skip_string_normalization: bool,
    skip_magic_trailing_comma: bool,
}

impl Args {
    /// Extra Black arguments for the non-default options.
    fn black_options(&self) -> Vec<String> {
        let mut v = Vec::new();
        if self.line_length != 88 {
            v.push(format!("--line-length={}", self.line_length));
        }
        if self.skip_string_normalization {
            v.push("--skip-string-normalization".into());
        }
        if self.skip_magic_trailing_comma {
            v.push("--skip-magic-trailing-comma".into());
        }
        v
    }

    fn format_options(&self) -> FormatOptions {
        FormatOptions {
            max_line_length: self.line_length,
            quote: if self.skip_string_normalization {
                QuoteStyle::Preserve
            } else {
                QuoteStyle::Double
            },
            magic_trailing_comma: !self.skip_magic_trailing_comma,
            ..FormatOptions::black()
        }
    }
}

fn parse_args() -> Args {
    let mut args = Args {
        black: std::env::var("BLACK").unwrap_or_else(|_| "black".into()),
        python: "python3".into(),
        root: PathBuf::from("/usr/lib/python3.12"),
        sample: 300,
        examples: 0,
        list: false,
        diff: false,
        jobs: std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .min(2),
        files: Vec::new(),
        cache: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/black-diff-cache"),
        line_length: 88,
        skip_string_normalization: false,
        skip_magic_trailing_comma: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--black" => args.black = it.next().expect("--black <path>"),
            "--python" => args.python = it.next().expect("--python <path>"),
            "--root" => args.root = it.next().expect("--root <dir>").into(),
            "--sample" => args.sample = it.next().expect("--sample <n>").parse().unwrap(),
            "--examples" => args.examples = it.next().expect("--examples <n>").parse().unwrap(),
            "--jobs" => args.jobs = it.next().expect("--jobs <n>").parse().unwrap(),
            "--cache" => args.cache = it.next().expect("--cache <dir>").into(),
            "--list" => args.list = true,
            "--files-from" => {
                let list = it.next().expect("--files-from <file>");
                let text = std::fs::read_to_string(&list).expect("read --files-from list");
                args.files.extend(
                    text.lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(PathBuf::from),
                );
            }
            "--line-length" | "-l" => {
                args.line_length = it.next().expect("--line-length <n>").parse().unwrap()
            }
            "--skip-string-normalization" | "-S" => args.skip_string_normalization = true,
            "--skip-magic-trailing-comma" | "-C" => args.skip_magic_trailing_comma = true,
            "--diff" => args.diff = true,
            _ if a.starts_with("--") => panic!("unknown option {a}"),
            _ => args.files.push(a.into()),
        }
    }
    args
}

fn collect_py(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            collect_py(&p, out);
        } else if ft.is_file() && p.extension().map(|x| x == "py").unwrap_or(false) {
            out.push(p);
        }
    }
}

fn hash_str(s: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    s.len().hash(&mut h);
    h.finish()
}

/// Black's output, or `Err(message)` when Black refuses the file.
fn run_black(args: &Args, src: &str) -> Result<String, String> {
    let options = args.black_options();
    let key = format!(
        "{:016x}",
        hash_str(&format!("{}\0{src}", options.join(" ")))
    );
    let ok_path = args.cache.join(format!("{key}.py"));
    let err_path = args.cache.join(format!("{key}.err"));
    if let Ok(s) = std::fs::read_to_string(&ok_path) {
        return Ok(s);
    }
    if let Ok(s) = std::fs::read_to_string(&err_path) {
        return Err(s);
    }
    let mut child = Command::new(&args.black)
        .args(&options)
        .args(["-q", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn black");
    let mut stdin = child.stdin.take().unwrap();
    let input = src.to_string();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let out = child.wait_with_output().expect("black output");
    let _ = writer.join();
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).into_owned();
        let _ = std::fs::write(&ok_path, &s);
        Ok(s)
    } else {
        let s = String::from_utf8_lossy(&out.stderr).into_owned();
        let _ = std::fs::write(&err_path, &s);
        Err(s)
    }
}

/// Compares ASTs like Black's `assert_equivalent` (docstring whitespace
/// normalized, nested tuples in `del` flattened). Reads `src\tdst` path pairs.
const AST_CHECK: &str = r#"
import ast, sys, warnings
warnings.simplefilter("ignore")
def norm(tree):
    for node in ast.walk(tree):
        if isinstance(node, ast.Expr) and isinstance(node.value, ast.Constant) and isinstance(node.value.value, str):
            lines = [l.strip() for l in node.value.value.splitlines()]
            node.value.value = "\n".join(lines).strip()
        if isinstance(node, ast.Constant) and getattr(node, "kind", None) == "u":
            node.kind = None
        if isinstance(node, ast.Delete):
            flat = []
            def unwrap(t):
                if isinstance(t, ast.Tuple):
                    for e in t.elts: unwrap(e)
                else:
                    flat.append(t)
            for t in node.targets: unwrap(t)
            node.targets = flat
    return ast.dump(tree)
for line in sys.stdin:
    src, dst = line.rstrip("\n").split("\t")
    try:
        a = norm(ast.parse(open(src, encoding="utf-8-sig").read()))
    except Exception as e:
        print("SRCERR"); sys.stdout.flush(); continue
    try:
        b = norm(ast.parse(open(dst, encoding="utf-8-sig").read()))
    except Exception as e:
        print("DSTERR " + str(e).replace("\n", " ")); sys.stdout.flush(); continue
    print("OK" if a == b else "DIFF"); sys.stdout.flush()
"#;

#[derive(Default)]
struct Outcome {
    path: PathBuf,
    black_err: bool,
    ours_err: Option<String>,
    identical: bool,
    idempotent: bool,
    category: Option<String>,
    hunk: String,
    ours: String,
}

/// The first differing hunk (Black's lines, our lines, 1-based line): the
/// smallest window after the common prefix past which both texts agree again
/// for three lines (or both end).
fn first_hunk(black: &str, ours: &str) -> (Vec<String>, Vec<String>, usize) {
    const WINDOW: usize = 300;
    let a: Vec<&str> = black.split('\n').collect();
    let b: Vec<&str> = ours.split('\n').collect();
    let mut i = 0;
    while i < a.len() && i < b.len() && a[i] == b[i] {
        i += 1;
    }
    let resync = |ja: usize, jb: usize| -> bool {
        let (ra, rb) = (a.len() - ja, b.len() - jb);
        if ra == 0 && rb == 0 {
            return true;
        }
        let k = 3.min(ra).min(rb);
        k > 0 && (0..k).all(|t| a[ja + t] == b[jb + t]) && (k == 3 || ra == rb)
    };
    let mut end = (a.len().min(i + WINDOW), b.len().min(i + WINDOW));
    'search: for total in 0..=2 * WINDOW {
        for da in 0..=total.min(WINDOW) {
            let db = total - da;
            if db > WINDOW || i + da > a.len() || i + db > b.len() {
                continue;
            }
            if resync(i + da, i + db) {
                end = (i + da, i + db);
                break 'search;
            }
        }
    }
    let old = a[i..end.0].iter().map(|s| s.to_string()).collect();
    let new = b[i..end.1].iter().map(|s| s.to_string()).collect();
    (old, new, i + 1)
}

fn strip_ws(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Heuristic category of the first differing hunk (Black lines vs ours).
fn categorize(old: &[String], new: &[String]) -> String {
    let o = old.join("\n");
    let n = new.join("\n");
    let o_nonblank: Vec<&String> = old.iter().filter(|l| !l.trim().is_empty()).collect();
    let n_nonblank: Vec<&String> = new.iter().filter(|l| !l.trim().is_empty()).collect();
    if o_nonblank.is_empty() && n_nonblank.is_empty() {
        return "blank lines".into();
    }
    if o_nonblank == n_nonblank {
        return "blank lines".into();
    }
    let has_comment = |s: &str| s.contains('#');
    let ow = strip_ws(&o);
    let nw = strip_ws(&n);
    if ow == nw {
        let per_line = |v: &[&String]| -> Vec<String> { v.iter().map(|l| strip_ws(l)).collect() };
        if o_nonblank.len() != n_nonblank.len() || per_line(&o_nonblank) != per_line(&n_nonblank) {
            if has_comment(&o) || has_comment(&n) {
                return "line split (with comments)".into();
            }
            return "line split".into();
        }
        if o.contains("\"\"\"") || n.contains("\"\"\"") || o.contains("'''") {
            return "docstring/multiline string whitespace".into();
        }
        if has_comment(&o) {
            return "comment spacing".into();
        }
        return "whitespace within line".into();
    }
    if ow.replace(',', "") == nw.replace(',', "") {
        return "trailing comma".into();
    }
    let no_paren = |s: &str| s.replace(['(', ')'], "");
    if no_paren(&ow) == no_paren(&nw) {
        return "parentheses".into();
    }
    if no_paren(&ow).replace(',', "") == no_paren(&nw).replace(',', "") {
        return "parentheses+comma".into();
    }
    if ow.replace('\'', "\"") == nw.replace('\'', "\"") {
        return "string quotes".into();
    }
    if n.contains('\\') && !o.contains('\\') {
        return "backslash continuation".into();
    }
    if o.contains("\"\"\"") || n.contains("\"\"\"") || o.contains("'''") || n.contains("'''") {
        return "docstring/multiline string".into();
    }
    if has_comment(&o) || has_comment(&n) {
        let oc: Vec<&str> = old
            .iter()
            .filter_map(|l| l.find('#').map(|i| &l[i..]))
            .collect();
        let nc: Vec<&str> = new
            .iter()
            .filter_map(|l| l.find('#').map(|i| &l[i..]))
            .collect();
        if oc != nc {
            return "comment text".into();
        }
        return "comment placement".into();
    }
    "other".into()
}

fn main() {
    let args = parse_args();
    std::fs::create_dir_all(&args.cache).expect("create cache dir");
    let explicit = !args.files.is_empty();
    let files: Vec<PathBuf> = if explicit {
        args.files.clone()
    } else {
        let mut all = Vec::new();
        collect_py(&args.root, &mut all);
        all.sort();
        let total = all.len();
        let n = args.sample.min(total);
        let picked: Vec<PathBuf> = (0..n).map(|i| all[i * total / n].clone()).collect();
        eprintln!(
            "{} .py files under {}, sampling {} (every {:.2}th)",
            total,
            args.root.display(),
            n,
            total as f64 / n.max(1) as f64
        );
        picked
    };
    let opts = args.format_options();
    let outcomes: Mutex<Vec<Outcome>> = Mutex::new(Vec::new());
    let next = AtomicUsize::new(0);
    // Per-process scratch directory, so concurrent runs cannot mix up outputs.
    let ours_dir = args.cache.join(format!("ours-{}", std::process::id()));
    std::fs::create_dir_all(&ours_dir).unwrap();
    std::thread::scope(|s| {
        for _ in 0..args.jobs.clamp(1, 2) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                if i >= files.len() {
                    break;
                }
                let path = &files[i];
                let mut oc = Outcome {
                    path: path.clone(),
                    ..Outcome::default()
                };
                let Ok(src) = std::fs::read_to_string(path) else {
                    oc.black_err = true;
                    outcomes.lock().unwrap().push(oc);
                    continue;
                };
                let black = match run_black(&args, &src) {
                    Ok(b) => b,
                    Err(_) => {
                        oc.black_err = true;
                        outcomes.lock().unwrap().push(oc);
                        continue;
                    }
                };
                let ours = match std::panic::catch_unwind(|| format_source(&src, &opts)) {
                    Ok(Ok(o)) => o,
                    Ok(Err(e)) => {
                        oc.ours_err = Some(e.to_string());
                        oc.category = Some("our error".into());
                        outcomes.lock().unwrap().push(oc);
                        continue;
                    }
                    Err(_) => {
                        oc.ours_err = Some("panic".into());
                        oc.category = Some("our panic".into());
                        outcomes.lock().unwrap().push(oc);
                        continue;
                    }
                };
                oc.idempotent = std::panic::catch_unwind(|| format_source(&ours, &opts))
                    .ok()
                    .and_then(|r| r.ok())
                    .map(|again| again == ours)
                    .unwrap_or(false);
                oc.identical = ours == black;
                if !oc.identical {
                    let (old, new, line) = first_hunk(&black, &ours);
                    oc.category = Some(categorize(&old, &new));
                    let mut h = format!("@@ line {line}\n");
                    for l in &old {
                        h.push_str(&format!("-{l}\n"));
                    }
                    for l in &new {
                        h.push_str(&format!("+{l}\n"));
                    }
                    oc.hunk = h;
                }
                oc.ours = ours;
                outcomes.lock().unwrap().push(oc);
            });
        }
    });
    let mut outcomes = outcomes.into_inner().unwrap();
    outcomes.sort_by(|a, b| a.path.cmp(&b.path));

    // AST equivalence through python3.
    let mut ast_ok: HashMap<PathBuf, bool> = HashMap::new();
    let script = ours_dir.join("ast_check.py");
    std::fs::write(&script, AST_CHECK).unwrap();
    let mut pairs = String::new();
    let mut order = Vec::new();
    for (i, oc) in outcomes.iter().enumerate() {
        if oc.black_err || oc.ours_err.is_some() {
            continue;
        }
        let dst = ours_dir.join(format!("{i}.py"));
        std::fs::write(&dst, &oc.ours).unwrap();
        pairs.push_str(&format!("{}\t{}\n", oc.path.display(), dst.display()));
        order.push(oc.path.clone());
    }
    let mut child = Command::new(&args.python)
        .arg(&script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn python3");
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(pairs.as_bytes());
    });
    let out = child.wait_with_output().expect("python3 output");
    let _ = writer.join();
    let text = String::from_utf8_lossy(&out.stdout);
    let _ = std::fs::remove_dir_all(&ours_dir);
    let mut ast_problems = Vec::new();
    let mut python_cannot_parse = 0;
    for (path, res) in order.iter().zip(text.lines()) {
        // A source Python itself cannot parse has no AST to preserve (only
        // the formatter's own semantic fingerprint check covers it).
        let ok = res == "OK" || res == "SRCERR";
        python_cannot_parse += usize::from(res == "SRCERR");
        if !ok {
            ast_problems.push(format!("{}: {res}", path.display()));
        }
        ast_ok.insert(path.clone(), ok);
    }
    if order.len() > text.lines().count() {
        ast_problems.push(format!(
            "python3 checked only {} of {} files",
            text.lines().count(),
            order.len()
        ));
    }

    let formatted: Vec<&Outcome> = outcomes.iter().filter(|o| !o.black_err).collect();
    let n = formatted.len();
    let identical = formatted.iter().filter(|o| o.identical).count();
    let idempotent = formatted.iter().filter(|o| o.idempotent).count();
    let ast_equal = formatted
        .iter()
        .filter(|o| ast_ok.get(&o.path).copied().unwrap_or(false))
        .count();
    let black_errors = outcomes.len() - n;
    let mut cats: BTreeMap<String, Vec<&Outcome>> = BTreeMap::new();
    for o in &formatted {
        if let Some(c) = &o.category {
            cats.entry(c.clone()).or_default().push(o);
        }
    }
    let mut cats: Vec<(String, Vec<&Outcome>)> = cats.into_iter().collect();
    cats.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(&b.0)));

    let pct = |k: usize| 100.0 * k as f64 / n.max(1) as f64;
    println!("files:       {n}  (black errors skipped: {black_errors})");
    println!("identical:   {identical}  ({:.1}%)", pct(identical));
    println!("idempotent:  {idempotent}  ({:.1}%)", pct(idempotent));
    println!("ast-equal:   {ast_equal}  ({:.1}%)", pct(ast_equal));
    if python_cannot_parse > 0 {
        println!(
            "             ({python_cannot_parse} of them are sources {} cannot parse)",
            args.python
        );
    }
    println!("first-difference categories:");
    for (c, v) in &cats {
        println!("  {:5}  {c}", v.len());
    }
    for p in &ast_problems {
        println!("AST: {p}");
    }
    for o in &formatted {
        if !o.idempotent && o.ours_err.is_none() {
            println!("NOT IDEMPOTENT: {}", o.path.display());
        }
        if let Some(e) = &o.ours_err {
            println!("ERROR: {}: {e}", o.path.display());
        }
    }
    if args.list {
        for (c, v) in &cats {
            for o in v {
                println!("[{c}] {}", o.path.display());
            }
        }
        for o in outcomes.iter().filter(|o| o.black_err) {
            println!("[black error] {}", o.path.display());
        }
    }
    if args.examples > 0 || args.diff {
        for (c, v) in &cats {
            let k = if args.diff { v.len() } else { args.examples };
            for o in v.iter().take(k) {
                println!("=== [{c}] {}\n{}", o.path.display(), o.hunk);
            }
        }
    }
}
