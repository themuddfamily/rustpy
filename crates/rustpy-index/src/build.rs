//! Building and refreshing indexes.
//!
//! [`refresh`] brings an index up to date with the files on disk: a file
//! is read within the size budget and hashed, and only parsed when no
//! entry with that content hash exists (in the old index or in a shared
//! bundle). Files are processed on a few worker threads; progress and
//! cancellation go through [`Progress`].

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rustc_hash::{FxHashMap, FxHashSet};

use crate::extract::{extract, FileSymbols};
use crate::hash::ContentHash;
use crate::index::{
    FileStamp, IndexBuilder, IndexMeta, Root, RootKind, SymbolIndex, FILE_EXTENSION,
};

/// A directory to index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootSpec {
    pub path: PathBuf,
    pub kind: RootKind,
}

/// A file to index: below root number `root` of the [`RootSpec`] list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFile {
    pub root: u32,
    pub path: PathBuf,
}

/// Shared between a build and whoever watches it.
#[derive(Debug, Default)]
pub struct Progress {
    /// Files to process (0 while the roots are being listed).
    pub total: AtomicUsize,
    pub done: AtomicUsize,
    /// Set to stop the build: [`refresh`] then returns [`Cancelled`].
    pub cancelled: AtomicBool,
}

impl Progress {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// `(done, total)`.
    pub fn counts(&self) -> (usize, usize) {
        (
            self.done.load(Ordering::Relaxed),
            self.total.load(Ordering::Relaxed),
        )
    }

    /// Completed fraction, `None` while the total is unknown.
    pub fn fraction(&self) -> Option<f32> {
        let (done, total) = self.counts();
        (total > 0).then(|| (done as f32 / total as f32).min(1.0))
    }
}

/// The build was cancelled through [`Progress::cancel`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cancelled;

#[derive(Clone, Debug)]
pub struct BuildOptions {
    /// Worker threads.
    pub threads: usize,
    /// Larger files are recorded without symbols (generated tables and the like).
    pub max_file_size: u64,
}

impl Default for BuildOptions {
    fn default() -> Self {
        // Half the cores at most: indexing runs in the background while the IDE is used.
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2);
        BuildOptions {
            threads: (cores / 2).clamp(1, 6),
            max_file_size: 4 * 1024 * 1024,
        }
    }
}

/// What a refresh did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RefreshStats {
    /// Files in the new index.
    pub files: usize,
    /// Content, size and modification time unchanged: not parsed.
    pub unchanged: usize,
    /// Stamp changed but the content hash did not: not parsed.
    pub rehashed: usize,
    /// Content found in another entry of the old index (a copied file).
    pub reused: usize,
    /// Content found in a shared bundle.
    pub from_shared: usize,
    /// Parsed.
    pub parsed: usize,
    /// Not in the old index.
    pub added: usize,
    /// In the old index but gone.
    pub removed: usize,
    /// Binary extension modules (recorded by name only).
    pub extensions: usize,
    /// Changed or new files too large to parse (recorded by name only).
    pub large: usize,
    /// Unreadable files.
    pub skipped: usize,
    /// Files omitted because their declared source encoding is invalid.
    pub encoding_errors: Vec<(PathBuf, String)>,
    /// The list of roots differs from the old index's.
    pub roots_changed: bool,
    pub elapsed_ms: u64,
}

impl RefreshStats {
    /// Whether the new index differs from the old one (it needs saving).
    pub fn changed(&self) -> bool {
        self.roots_changed
            || self.rehashed
                + self.reused
                + self.from_shared
                + self.parsed
                + self.large
                + self.added
                + self.removed
                > 0
    }

    pub fn summary(&self) -> String {
        format!(
            "{} files ({} unchanged, {} parsed, {} from shared indexes, {} reused, {} added, {} removed, {} encoding errors) in {:.1} s",
            self.files,
            self.unchanged,
            self.parsed,
            self.from_shared,
            self.reused + self.rehashed,
            self.added,
            self.removed,
            self.encoding_errors.len(),
            self.elapsed_ms as f64 / 1000.0
        )
    }
}

/// Whether directory `name` holds a test suite rather than importable API,
/// so that its files are skipped: `tests` anywhere (`numpy/core/tests`,
/// `pandas/tests`) and, in the standard library, `test` too (CPython's
/// regression suite, `ctypes/test` in older versions). Elsewhere `test` is
/// often public API (`django.test`, `twisted.test.proto_helpers`).
fn is_test_suite(name: &str, kind: RootKind) -> bool {
    name == "tests" || (name == "test" && kind == RootKind::Stdlib)
}

/// Stack size of the threads that parse files: extraction is iterative or
/// depth-limited, this is a margin for pathological generated files.
const WORKER_STACK: usize = 16 << 20;

/// Deepest package nesting followed below a root.
const MAX_DEPTH: usize = 24;

/// At most this many files per root are listed.
pub const MAX_FILES_PER_ROOT: usize = 200_000;

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c == '_' || c.is_alphabetic())
        && chars.all(|c| c == '_' || c.is_alphanumeric())
}

/// The files of library roots that can be imported: `.py` and `.pyi`
/// modules, binary extension modules, in directories that are packages
/// (identifier names, `<name>-stubs` stub packages). Hidden directories,
/// `__pycache__`, test suites and roots nested in other roots are skipped.
pub fn walk_library_roots(roots: &[RootSpec], progress: &Progress) -> Vec<SourceFile> {
    let all_roots: FxHashSet<&Path> = roots.iter().map(|r| r.path.as_path()).collect();
    let mut out = Vec::new();
    for (i, r) in roots.iter().enumerate() {
        let mut files = Vec::new();
        walk_dir(&r.path, r.kind, 0, &all_roots, &mut files, progress);
        files.sort();
        files.truncate(MAX_FILES_PER_ROOT);
        out.extend(files.into_iter().map(|path| SourceFile {
            root: i as u32,
            path,
        }));
        if progress.is_cancelled() {
            break;
        }
    }
    out
}

fn walk_dir(
    dir: &Path,
    kind: RootKind,
    depth: usize,
    roots: &FxHashSet<&Path>,
    out: &mut Vec<PathBuf>,
    progress: &Progress,
) {
    if depth > MAX_DEPTH || out.len() >= MAX_FILES_PER_ROOT || progress.is_cancelled() {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        // Directory symlinks are not followed (cycles); file symlinks are.
        let is_dir = ft.is_dir();
        let is_file = ft.is_file() || (ft.is_symlink() && path.is_file());
        if is_dir {
            if name.starts_with('.') || name == "__pycache__" || is_test_suite(name, kind) {
                continue;
            }
            // PEP 561 stub-only packages (`requests-stubs`) sit directly in the root.
            let package_like = is_identifier(name)
                || (depth == 0 && name.strip_suffix("-stubs").is_some_and(is_identifier));
            if !package_like || roots.contains(path.as_path()) {
                continue;
            }
            walk_dir(&path, kind, depth + 1, roots, out, progress);
        } else if is_file {
            let source = name
                .strip_suffix(".py")
                .or_else(|| name.strip_suffix(".pyi"));
            if source.is_some_and(is_identifier) || extension_module_stem(name).is_some() {
                out.push(path);
            }
        }
    }
}

/// `_ssl` for `_ssl.cpython-312-x86_64-linux-gnu.so`, `mod` for `mod.pyd`.
fn extension_module_stem(file_name: &str) -> Option<&str> {
    if !(file_name.ends_with(".so") || file_name.ends_with(".pyd")) {
        return None;
    }
    let stem = file_name.split('.').next()?;
    is_identifier(stem).then_some(stem)
}

/// Dotted module name of a path relative to its root (`/`-separated):
/// `pkg/sub/mod.py` → `pkg.sub.mod`, `pkg/__init__.pyi` → `pkg`,
/// `_ssl.cpython-312-x86_64-linux-gnu.so` → `_ssl`, `foo-stubs/x.pyi` →
/// `foo.x`. Empty when some part is not an identifier.
pub fn module_name(rel: &str) -> String {
    let mut parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
    let Some(last) = parts.pop() else {
        return String::new();
    };
    let stem = if let Some(s) = last
        .strip_suffix(".pyi")
        .or_else(|| last.strip_suffix(".py"))
    {
        s
    } else if let Some(s) = extension_module_stem(last) {
        s
    } else {
        return String::new();
    };
    let mut out: Vec<&str> = Vec::with_capacity(parts.len() + 1);
    for (i, p) in parts.iter().enumerate() {
        let p = if i == 0 {
            p.strip_suffix("-stubs").unwrap_or(p)
        } else {
            p
        };
        if !is_identifier(p) {
            return String::new();
        }
        out.push(p);
    }
    if stem != "__init__" {
        if !is_identifier(stem) {
            return String::new();
        }
        out.push(stem);
    }
    out.join(".")
}

fn mtime_ns(meta: &std::fs::Metadata) -> i64 {
    match meta.modified() {
        Ok(t) => match t.duration_since(UNIX_EPOCH) {
            Ok(d) => d.as_nanos().min(i64::MAX as u128) as i64,
            Err(e) => -(e.duration().as_nanos().min(i64::MAX as u128) as i64),
        },
        Err(_) => 0,
    }
}

/// Unix seconds now.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

enum Payload {
    Old(u32),
    Shared(usize, u32),
    Fresh(FileSymbols),
    /// Recorded without symbols: an extension module or a file too large to parse.
    Bare {
        extension: bool,
    },
    Skip,
}

enum How {
    Unchanged,
    Rehashed,
    Reused,
    Shared,
    Parsed,
    Extension,
    Bare,
    Skipped,
    EncodingError(String),
}

struct Outcome {
    size: u64,
    mtime_ns: i64,
    hash: ContentHash,
    payload: Payload,
    how: How,
}

/// Brings `old` (if any) up to date with `files` below `roots`, reusing
/// entries by validated content hash (also from `shared` bundles), and
/// returns the new index, or `None` when `old` is still up to date. `files`
/// lists what to index (see [`walk_library_roots`]; a project passes its own
/// file list).
pub fn refresh(
    old: Option<&SymbolIndex>,
    meta: IndexMeta,
    roots: &[RootSpec],
    files: Vec<SourceFile>,
    shared: &[Arc<SymbolIndex>],
    opts: &BuildOptions,
    progress: &Progress,
) -> Result<(Option<SymbolIndex>, RefreshStats), Cancelled> {
    let started = Instant::now();
    progress.total.store(files.len().max(1), Ordering::Relaxed);
    progress.done.store(0, Ordering::Relaxed);
    // Relative paths, with `/` separators.
    let rels: Vec<Option<String>> = files
        .iter()
        .map(|f| {
            roots
                .get(f.root as usize)
                .and_then(|r| f.path.strip_prefix(&r.path).ok())
                .map(rel_string)
        })
        .collect();
    // Old entries by (new root, rel path) and by content.
    let mut old_by_path: FxHashMap<(u32, &str), u32> = FxHashMap::default();
    let old_by_hash = old.map(SymbolIndex::files_by_hash).unwrap_or_default();
    if let Some(o) = old {
        let root_map: Vec<Option<u32>> = o
            .roots
            .iter()
            .map(|r| {
                roots
                    .iter()
                    .position(|n| n.path == r.path)
                    .map(|p| p as u32)
            })
            .collect();
        for (i, f) in o.files.iter().enumerate() {
            if let Some(Some(root)) = root_map.get(f.root as usize) {
                old_by_path.insert((*root, o.strings.get(f.rel)), i as u32);
            }
        }
    }
    let shared_by_hash: Vec<FxHashMap<ContentHash, u32>> =
        shared.iter().map(|s| s.files_by_hash()).collect();
    let next = AtomicUsize::new(0);
    let threads = opts.threads.clamp(1, 64).min(files.len().max(1));
    let mut outcomes: Vec<(usize, Outcome)> = Vec::with_capacity(files.len());
    let work = || {
        let mut local = Vec::new();
        loop {
            if progress.is_cancelled() {
                break;
            }
            let i = next.fetch_add(1, Ordering::Relaxed);
            if i >= files.len() {
                break;
            }
            let Some(rel) = rels[i].as_deref() else {
                progress.done.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            let previous = old_by_path
                .get(&(files[i].root, rel))
                .map(|&j| (j, &old.expect("old index").files[j as usize]));
            let outcome = process(
                &files[i].path,
                rel,
                previous,
                &old_by_hash,
                &shared_by_hash,
                opts,
            );
            local.push((i, outcome));
            progress.done.fetch_add(1, Ordering::Relaxed);
        }
        local
    };
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..threads)
            .filter_map(|_| {
                std::thread::Builder::new()
                    .name("rustpy-index-worker".into())
                    .stack_size(WORKER_STACK)
                    .spawn_scoped(scope, work)
                    .ok()
            })
            .collect();
        if workers.is_empty() {
            // No thread could be started: do the work here.
            outcomes.extend(work());
        }
        for w in workers {
            if let Ok(local) = w.join() {
                outcomes.extend(local);
            }
        }
    });
    if progress.is_cancelled() {
        return Err(Cancelled);
    }
    outcomes.sort_by_key(|(i, _)| *i);
    let mut stats = RefreshStats::default();
    let mut present: FxHashSet<(u32, &str)> = FxHashSet::default();
    let mut keep: Vec<usize> = Vec::with_capacity(outcomes.len());
    for (k, (i, o)) in outcomes.iter().enumerate() {
        let Some(rel) = rels[*i].as_deref() else {
            continue;
        };
        let root = files[*i].root;
        match &o.how {
            How::Unchanged => stats.unchanged += 1,
            How::Rehashed => stats.rehashed += 1,
            How::Reused => stats.reused += 1,
            How::Shared => stats.from_shared += 1,
            How::Parsed => stats.parsed += 1,
            How::Extension => stats.extensions += 1,
            How::Bare => stats.large += 1,
            How::Skipped => stats.skipped += 1,
            How::EncodingError(error) => {
                stats.skipped += 1;
                stats
                    .encoding_errors
                    .push((files[*i].path.clone(), error.clone()));
            }
        }
        if matches!(o.payload, Payload::Skip) || !present.insert((root, rel)) {
            continue;
        }
        if !old_by_path.contains_key(&(root, rel)) {
            stats.added += 1;
        }
        keep.push(k);
    }
    stats.removed = old_by_path.keys().filter(|k| !present.contains(*k)).count();
    stats.files = keep.len();
    stats.roots_changed = old.is_some_and(|o| {
        o.roots.len() != roots.len()
            || o.roots
                .iter()
                .zip(roots)
                .any(|(a, b)| a.path != b.path || a.kind != b.kind)
    });
    if old.is_some() && !stats.changed() {
        stats.elapsed_ms = started.elapsed().as_millis() as u64;
        return Ok((None, stats));
    }
    let mut b = IndexBuilder::new(
        roots
            .iter()
            .map(|r| Root {
                path: r.path.clone(),
                kind: r.kind,
            })
            .collect(),
    );
    for k in keep {
        let (i, o) = &outcomes[k];
        let Some(rel) = rels[*i].as_deref() else {
            continue;
        };
        let module = module_name(rel);
        let st = FileStamp {
            root: files[*i].root,
            rel,
            module: &module,
            size: o.size,
            mtime_ns: o.mtime_ns,
            hash: o.hash,
        };
        match &o.payload {
            Payload::Old(j) => b.add_copy(&st, old.expect("old index"), *j),
            Payload::Shared(k, j) => b.add_copy(&st, &shared[*k], *j),
            Payload::Fresh(fs) => b.add_file(&st, Some(fs), false),
            Payload::Bare { extension } => b.add_file(&st, None, *extension),
            Payload::Skip => {}
        }
    }
    let now = now_unix();
    let created = old
        .map(|o| o.meta.created)
        .filter(|c| *c > 0)
        .unwrap_or(now);
    let index = b.finish(IndexMeta {
        created,
        updated: now,
        ..meta
    });
    stats.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok((Some(index), stats))
}

fn rel_string(p: &Path) -> String {
    let mut s = String::new();
    for c in p.components() {
        if !s.is_empty() {
            s.push('/');
        }
        s.push_str(&c.as_os_str().to_string_lossy());
    }
    s
}

fn process(
    path: &Path,
    rel: &str,
    previous: Option<(u32, &crate::index::FileRec)>,
    old_by_hash: &FxHashMap<ContentHash, u32>,
    shared: &[FxHashMap<ContentHash, u32>],
    opts: &BuildOptions,
) -> Outcome {
    let skip = || Outcome {
        size: 0,
        mtime_ns: 0,
        hash: ContentHash::default(),
        payload: Payload::Skip,
        how: How::Skipped,
    };
    let Ok(meta) = std::fs::metadata(path) else {
        return skip();
    };
    if !meta.is_file() {
        return skip();
    }
    let (size, mtime) = (meta.len(), mtime_ns(&meta));
    let name = rel.rsplit('/').next().unwrap_or(rel);
    if extension_module_stem(name).is_some() && !name.ends_with(".py") {
        return Outcome {
            size,
            mtime_ns: mtime,
            hash: ContentHash::default(),
            payload: Payload::Bare { extension: true },
            how: How::Extension,
        };
    }
    if size > opts.max_file_size {
        if let Some((j, f)) = previous {
            if f.size == size
                && f.mtime_ns == mtime
                && f.flags & FILE_EXTENSION == 0
                && f.hash.is_none()
            {
                return Outcome {
                    size,
                    mtime_ns: mtime,
                    hash: f.hash,
                    payload: Payload::Old(j),
                    how: How::Unchanged,
                };
            }
        }
        return Outcome {
            size,
            mtime_ns: mtime,
            hash: ContentHash::default(),
            payload: Payload::Bare { extension: false },
            how: How::Bare,
        };
    }
    let Ok(file) = crate::store::open_source(path) else {
        return skip();
    };
    let Ok(opened) = file.metadata() else {
        return skip();
    };
    if !opened.is_file() {
        return skip();
    }
    let mtime = mtime_ns(&opened);
    let mut bytes = Vec::new();
    if file
        .take(opts.max_file_size.saturating_add(1))
        .read_to_end(&mut bytes)
        .is_err()
    {
        return skip();
    }
    if bytes.len() as u64 > opts.max_file_size {
        return Outcome {
            size: bytes.len() as u64,
            mtime_ns: mtime,
            hash: ContentHash::default(),
            payload: Payload::Bare { extension: false },
            how: How::Bare,
        };
    }
    let hash = ContentHash::of(&bytes);
    // The size may have changed between the stat and the read.
    let size = bytes.len() as u64;
    let text = match rustpy_text::Encoding::decode_python_source(&bytes) {
        Ok(text) => text,
        Err(error) => {
            log::warn!("index: cannot decode {}: {error}", path.display());
            return Outcome {
                how: How::EncodingError(error.to_string()),
                ..skip()
            };
        }
    };
    // Older index entries were extracted as UTF-8. Re-extract transcoded
    // sources rather than reusing their raw-byte hash with different text.
    let previous = previous.filter(|_| text.as_bytes() == bytes);
    let old_by_hash = if text.as_bytes() == bytes {
        old_by_hash
    } else {
        &FxHashMap::default()
    };
    if let Some((j, f)) = previous {
        if f.hash == hash {
            return Outcome {
                size,
                mtime_ns: mtime,
                hash,
                payload: Payload::Old(j),
                how: if f.size == size && f.mtime_ns == mtime {
                    How::Unchanged
                } else {
                    How::Rehashed
                },
            };
        }
    }
    if let Some(&j) = old_by_hash.get(&hash) {
        return Outcome {
            size,
            mtime_ns: mtime,
            hash,
            payload: Payload::Old(j),
            how: How::Reused,
        };
    }
    for (k, map) in shared
        .iter()
        .enumerate()
        .filter(|_| text.as_bytes() == bytes)
    {
        if let Some(&j) = map.get(&hash) {
            return Outcome {
                size,
                mtime_ns: mtime,
                hash,
                payload: Payload::Shared(k, j),
                how: How::Shared,
            };
        }
    }
    // A parser bug must not take the IDE down: such a file gets no symbols.
    let symbols = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| extract(&text))) {
        Ok(s) => s,
        Err(_) => {
            log::warn!("index: extracting symbols from {} panicked", path.display());
            FileSymbols::default()
        }
    };
    Outcome {
        size,
        mtime_ns: mtime,
        hash,
        payload: Payload::Fresh(symbols),
        how: How::Parsed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{IndexKind, SearchOptions};

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn lib_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        write(r, "site/pkg/__init__.py", "from .core import Engine\n");
        write(
            r,
            "site/pkg/core.py",
            "class Engine:\n    def start(self):\n        pass\n",
        );
        write(
            r,
            "site/pkg/tests/test_core.py",
            "def test_engine():\n    pass\n",
        );
        write(r, "site/pkg/__pycache__/core.cpython-312.pyc", "junk");
        write(
            r,
            "site/pkg/_speedups.cpython-312-x86_64-linux-gnu.so",
            "\x7fELF",
        );
        write(r, "site/pkg-1.0.dist-info/METADATA", "Name: pkg");
        write(r, "site/.hidden/x.py", "X = 1\n");
        write(r, "site/single.py", "def helper():\n    pass\n");
        write(r, "site/types-stubs/__init__.pyi", "class T: ...\n");
        write(r, "site/not-a-package/x.py", "Y = 1\n");
        write(r, "site/vendored/inner-stubs/x.pyi", "Z = 1\n");
        write(r, "site/not-a-module.py", "Q = 1\n");
        write(r, "stdlib/os.py", "def getcwd():\n    pass\n");
        write(r, "stdlib/json/__init__.py", "def loads(s):\n    pass\n");
        dir
    }

    fn roots(dir: &Path) -> Vec<RootSpec> {
        vec![
            RootSpec {
                path: dir.join("stdlib"),
                kind: RootKind::Stdlib,
            },
            RootSpec {
                path: dir.join("site"),
                kind: RootKind::Site,
            },
        ]
    }

    fn opts() -> BuildOptions {
        BuildOptions {
            threads: 3,
            ..Default::default()
        }
    }

    fn build(
        old: Option<&SymbolIndex>,
        dir: &Path,
        shared: &[Arc<SymbolIndex>],
    ) -> (SymbolIndex, RefreshStats) {
        let roots = roots(dir);
        let progress = Progress::default();
        let files = walk_library_roots(&roots, &progress);
        let (new, stats) = refresh(
            old,
            IndexMeta {
                kind: IndexKind::Libraries,
                label: "test".into(),
                ..Default::default()
            },
            &roots,
            files,
            shared,
            &opts(),
            &progress,
        )
        .unwrap();
        assert_eq!(
            new.is_none(),
            old.is_some() && !stats.changed(),
            "an index is returned exactly when something changed"
        );
        (
            new.unwrap_or_else(|| old.expect("unchanged without an old index").clone()),
            stats,
        )
    }

    fn score(name: &str, q: &str) -> Option<i32> {
        name.to_lowercase().contains(&q.to_lowercase()).then_some(1)
    }

    #[test]
    fn source_encodings_are_decoded_and_errors_are_reported() {
        let dir = lib_tree();
        let path = dir.path().join("site/encoded.py");
        std::fs::write(
            &path,
            b"# coding: latin-1\r\ndef caf\xe9():\r\n    pass\r\n",
        )
        .unwrap();
        let (index, stats) = build(None, dir.path(), &[]);
        assert!(stats.encoding_errors.is_empty());
        assert!(!index
            .search("café", &crate::SearchOptions::default(), &score)
            .is_empty());
        std::fs::write(&path, b"# coding: utf-8\n\xff").unwrap();
        let (index, stats) = build(Some(&index), dir.path(), &[]);
        assert_eq!(stats.encoding_errors.len(), 1);
        assert_eq!(stats.encoding_errors[0].0, path);
        assert!(stats.encoding_errors[0].1.contains("UTF-8"));
        assert!(index
            .search("café", &crate::SearchOptions::default(), &score)
            .is_empty());
    }

    #[test]
    fn module_names() {
        assert_eq!(module_name("pkg/sub/mod.py"), "pkg.sub.mod");
        assert_eq!(module_name("pkg/__init__.pyi"), "pkg");
        assert_eq!(module_name("__init__.py"), "");
        assert_eq!(module_name("_ssl.cpython-312-x86_64-linux-gnu.so"), "_ssl");
        assert_eq!(module_name("pkg/_ext.abi3.so"), "pkg._ext");
        assert_eq!(module_name("win.pyd"), "win");
        assert_eq!(module_name("foo-stubs/x.pyi"), "foo.x");
        assert_eq!(module_name("not-a-package/x.py"), "");
        assert_eq!(module_name("README.txt"), "");
        assert_eq!(module_name("3.7/x.py"), "");
    }

    #[test]
    fn walks_importable_files_only() {
        let dir = lib_tree();
        let progress = Progress::default();
        let files = walk_library_roots(&roots(dir.path()), &progress);
        let rels: Vec<(u32, String)> = files
            .iter()
            .map(|f| {
                (
                    f.root,
                    rel_string(
                        f.path
                            .strip_prefix(&roots(dir.path())[f.root as usize].path)
                            .unwrap(),
                    ),
                )
            })
            .collect();
        assert_eq!(
            rels,
            vec![
                (0, "json/__init__.py".to_string()),
                (0, "os.py".to_string()),
                (1, "pkg/__init__.py".to_string()),
                (
                    1,
                    "pkg/_speedups.cpython-312-x86_64-linux-gnu.so".to_string()
                ),
                (1, "pkg/core.py".to_string()),
                (1, "single.py".to_string()),
                (1, "types-stubs/__init__.pyi".to_string()),
            ]
        );
    }

    #[test]
    fn incremental_refresh_reuses_unchanged_files() {
        let dir = lib_tree();
        let (first, stats) = build(None, dir.path(), &[]);
        assert_eq!(
            (
                stats.files,
                stats.parsed,
                stats.added,
                stats.extensions,
                stats.unchanged
            ),
            (7, 6, 7, 1, 0),
            "{stats:?}"
        );
        assert!(first.module_path("pkg._speedups").is_some());
        assert_eq!(
            first.module_path("types"),
            Some(dir.path().join("site/types-stubs/__init__.pyi"))
        );
        // Nothing changed: validated content is reused without parsing.
        let (second, stats) = build(Some(&first), dir.path(), &[]);
        assert_eq!(
            (stats.unchanged, stats.parsed, stats.added, stats.removed),
            (6, 0, 0, 0),
            "{stats:?}"
        );
        assert!(!stats.changed());
        assert!(second.same_content(&first));
        // Edit one file, add one, delete one, and touch one without changing it.
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(
            dir.path(),
            "site/single.py",
            "def helper():\n    pass\n\ndef helper_two():\n    pass\n",
        );
        write(dir.path(), "site/extra.py", "EXTRA = 1\n");
        std::fs::remove_file(dir.path().join("stdlib/os.py")).unwrap();
        let json = dir.path().join("stdlib/json/__init__.py");
        let text = std::fs::read(&json).unwrap();
        std::fs::write(&json, text).unwrap();
        let f = std::fs::File::options().write(true).open(&json).unwrap();
        f.set_modified(SystemTime::now() + std::time::Duration::from_secs(5))
            .unwrap();
        let (third, stats) = build(Some(&second), dir.path(), &[]);
        assert_eq!(
            (stats.parsed, stats.added, stats.removed, stats.rehashed),
            (2, 1, 1, 1),
            "{stats:?}"
        );
        assert!(stats.changed());
        let hits = third.search("helper", &SearchOptions::default(), &score);
        assert_eq!(
            hits.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
            vec!["helper", "helper_two"]
        );
        assert!(third
            .search("getcwd", &SearchOptions::default(), &score)
            .is_empty());
        assert_eq!(
            third
                .search("EXTRA", &SearchOptions::default(), &score)
                .len(),
            1
        );
        // A copy of a file elsewhere is reused by content.
        write(
            dir.path(),
            "site/copy_of_core.py",
            "class Engine:\n    def start(self):\n        pass\n",
        );
        let (fourth, stats) = build(Some(&third), dir.path(), &[]);
        assert_eq!(
            (stats.parsed, stats.reused, stats.added),
            (0, 1, 1),
            "{stats:?}"
        );
        assert_eq!(
            fourth
                .search(
                    "Engine",
                    &SearchOptions {
                        classes_only: true,
                        ..Default::default()
                    },
                    &score
                )
                .len(),
            2
        );
    }

    #[test]
    fn restored_mtime_edits_refresh_persisted_symbols_and_renames() {
        let dir = lib_tree();
        let path = dir.path().join("site/single.py");
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let (first, _) = build(None, dir.path(), &[]);
        let cache = dir.path().join("cached.rpidx");
        crate::IndexStore::save(&cache, &first).unwrap();
        let before_len = std::fs::metadata(&path).unwrap().len();
        write(dir.path(), "site/single.py", "def edited():\n    pass\n");
        assert_eq!(std::fs::metadata(&path).unwrap().len(), before_len);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        let first = crate::IndexStore::load(&cache).unwrap();
        let (fresh, stats) = build(Some(&first), dir.path(), &[]);
        assert_eq!(stats.parsed, 1);
        assert!(fresh
            .search("helper", &SearchOptions::default(), &score)
            .is_empty());
        assert_eq!(
            fresh
                .search("edited", &SearchOptions::default(), &score)
                .len(),
            1
        );
        crate::IndexStore::save(&cache, &fresh).unwrap();
        let fresh = crate::IndexStore::load(&cache).unwrap();
        std::fs::rename(&path, dir.path().join("site/renamed.py")).unwrap();
        let (renamed, stats) = build(Some(&fresh), dir.path(), &[]);
        assert_eq!((stats.added, stats.removed, stats.reused), (1, 1, 1));
        assert_eq!(
            renamed.search("edited", &SearchOptions::default(), &score)[0].module,
            "renamed"
        );
    }

    #[cfg(unix)]
    #[test]
    fn fifo_sources_and_index_loads_do_not_block() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe.py");
        assert!(std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        assert!(crate::IndexStore::load(&fifo).is_err());
        let roots = [RootSpec {
            path: dir.path().to_path_buf(),
            kind: RootKind::Project,
        }];
        let (_, stats) = refresh(
            None,
            IndexMeta::default(),
            &roots,
            vec![SourceFile {
                root: 0,
                path: fifo,
            }],
            &[],
            &opts(),
            &Progress::default(),
        )
        .unwrap();
        assert_eq!((stats.files, stats.skipped), (0, 1));
    }

    #[test]
    fn nonregular_source_is_skipped_without_reading() {
        let dir = tempfile::tempdir().unwrap();
        let roots = [RootSpec {
            path: dir.path().to_path_buf(),
            kind: RootKind::Project,
        }];
        let path = dir.path().join("directory.py");
        std::fs::create_dir(&path).unwrap();
        let (_, stats) = refresh(
            None,
            IndexMeta::default(),
            &roots,
            vec![SourceFile { root: 0, path }],
            &[],
            &opts(),
            &Progress::default(),
        )
        .unwrap();
        assert_eq!((stats.files, stats.skipped), (0, 1));
    }

    #[test]
    fn shared_bundles_replace_parsing_wherever_the_files_are() {
        let a = lib_tree();
        let (index_a, _) = build(None, a.path(), &[]);
        let bundle =
            Arc::new(index_a.to_bundle(&[RootKind::Stdlib, RootKind::Site], IndexMeta::default()));
        // The same files at another location: nothing needs parsing.
        let b = lib_tree();
        let (index_b, stats) = build(None, b.path(), &[bundle]);
        assert_eq!(
            (stats.parsed, stats.from_shared, stats.added),
            (0, 6, 7),
            "{stats:?}"
        );
        let hit = &index_b.search(
            "Engine",
            &SearchOptions {
                classes_only: true,
                ..Default::default()
            },
            &score,
        )[0];
        assert_eq!(hit.path, b.path().join("site/pkg/core.py"));
        assert_eq!(hit.module, "pkg.core");
    }

    #[test]
    fn cancelled_builds_return_nothing() {
        let dir = lib_tree();
        let roots = roots(dir.path());
        let progress = Progress::default();
        let files = walk_library_roots(&roots, &progress);
        progress.cancel();
        assert_eq!(
            refresh(
                None,
                IndexMeta::default(),
                &roots,
                files,
                &[],
                &opts(),
                &progress
            )
            .err(),
            Some(Cancelled)
        );
    }

    #[test]
    fn test_packages_outside_the_standard_library_are_indexed() {
        let dir = lib_tree();
        let r = dir.path();
        write(r, "site/django/__init__.py", "");
        write(
            r,
            "site/django/test/__init__.py",
            "from django.test.testcases import TestCase\nfrom django.test.client import Client\n",
        );
        write(
            r,
            "site/django/test/testcases.py",
            "class TestCase:\n    pass\n",
        );
        write(r, "site/django/test/client.py", "class Client:\n    pass\n");
        write(
            r,
            "site/django/tests/test_views.py",
            "class ViewTests:\n    pass\n",
        );
        write(r, "site/twisted/__init__.py", "");
        write(r, "site/twisted/test/__init__.py", "");
        write(
            r,
            "site/twisted/test/proto_helpers.py",
            "class StringTransport:\n    pass\n",
        );
        write(r, "site/tests/__init__.py", "class StrayTests:\n    pass\n");
        write(r, "stdlib/encodings/__init__.py", "");
        write(r, "stdlib/test/test_os.py", "class OSTests:\n    pass\n");
        write(
            r,
            "stdlib/ctypes/test/test_x.py",
            "class CtypesTests:\n    pass\n",
        );
        let (index, _) = build(None, r, &[]);
        let modules = |name: &str| {
            index
                .exact_import_candidates(name, 5)
                .into_iter()
                .map(|c| c.module)
                .collect::<Vec<_>>()
        };
        // Public API in packages named `test` (singular) outside the stdlib.
        assert_eq!(modules("TestCase"), vec!["django.test"]);
        assert_eq!(modules("Client"), vec!["django.test"]);
        assert_eq!(
            modules("StringTransport"),
            vec!["twisted.test.proto_helpers"]
        );
        // Test suites: `tests` anywhere, `test` in the standard library.
        for suite in ["ViewTests", "StrayTests", "OSTests", "CtypesTests"] {
            assert!(modules(suite).is_empty(), "{suite}");
            assert!(
                index
                    .search(suite, &SearchOptions::default(), &score)
                    .is_empty(),
                "{suite}"
            );
        }
    }

    #[test]
    fn refresh_survives_huge_dunder_all_sums() {
        // A library module with an 11,000-term `__all__` sum (valid Python):
        // indexing it overflowed the stack and aborted the IDE.
        let dir = lib_tree();
        write(
            dir.path(),
            "site/generated.py",
            &crate::extract::tests::dunder_all_sum(11_000),
        );
        let (index, stats) = build(None, dir.path(), &[]);
        assert_eq!(stats.parsed, 7, "{stats:?}");
        assert!(index.module_path("generated").is_some());
        assert_eq!(
            index.exact_import_candidates("after", 5).len(),
            0,
            "an exact __all__ without it"
        );
        assert_eq!(
            index
                .search("after", &SearchOptions::default(), &score)
                .len(),
            1
        );
    }

    #[test]
    fn large_and_unreadable_files() {
        let dir = lib_tree();
        let roots = roots(dir.path());
        let progress = Progress::default();
        let mut files = walk_library_roots(&roots, &progress);
        files.push(SourceFile {
            root: 1,
            path: dir.path().join("site/missing.py"),
        });
        let small = BuildOptions {
            threads: 1,
            max_file_size: 30,
        };
        let (index, stats) = refresh(
            None,
            IndexMeta::default(),
            &roots,
            files,
            &[],
            &small,
            &progress,
        )
        .unwrap();
        let index = index.unwrap();
        assert_eq!((stats.skipped, stats.large), (1, 1), "{stats:?}");
        // `pkg/core.py` (60 bytes) is recorded without symbols; its module is still known.
        assert!(index.module_path("pkg.core").is_some());
        assert!(index
            .search("Engine", &SearchOptions::default(), &score)
            .is_empty());
        // Unchanged large files are not re-read on the next refresh.
        let files = walk_library_roots(&roots, &progress);
        let (same, again) = refresh(
            Some(&index),
            IndexMeta::default(),
            &roots,
            files,
            &[],
            &small,
            &progress,
        )
        .unwrap();
        assert!(!again.changed() && same.is_none(), "{again:?}");
    }
}
