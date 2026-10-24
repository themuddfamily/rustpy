//! Symbol indexes in the IDE (roadmap §7 "Shared indexes"): the index of the
//! project interpreter's libraries and the index of the project, loaded from
//! disk and refreshed on background threads. A refresh validates bounded file
//! content and only parses files whose
//! content is in no index or shared bundle yet (see `rustpy_index`).
//!
//! Consumers take an [`IndexSnapshot`] (cheap to clone): Go to Class / Go to
//! Symbol and Search Everywhere ([`IndexSnapshot::search`]), second-press
//! completion ([`IndexSnapshot::import_names`]) and the "Import" quick fix
//! for unresolved references ([`IndexSnapshot::import_modules`]).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crossbeam_channel::{Receiver, TryRecvError};
use parking_lot::Mutex;
use rustc_hash::FxHashSet;
use rustpy_index::{
    refresh, walk_library_roots, BuildOptions, IndexError, IndexKind, IndexMeta, IndexStore,
    Progress, RefreshStats, RootKind, RootSpec, SearchOptions, SourceFile, StoredIndex, SymbolHit,
    SymbolIndex, SymbolKind,
};
use rustpy_project::Interpreter;
use rustpy_semantic::completion2::ImportableName;
use rustpy_semantic::features::CompletionKind;

/// Where index files are kept: `<configuration directory>/index` when
/// `RUSTPY_CONFIG_DIR` is set (smoke tests, portable installs keep everything
/// in one place), otherwise the platform cache directory (`~/.cache/rustpy/index`
/// on Linux, `~/Library/Caches/rustpy/index` on macOS, `%LOCALAPPDATA%\rustpy\index`
/// on Windows).
pub fn index_dir() -> PathBuf {
    if std::env::var_os("RUSTPY_CONFIG_DIR").is_some() {
        return rustpy_settings::Settings::config_dir().join("index");
    }
    match dirs::cache_dir() {
        Some(d) => d.join("rustpy").join("index"),
        None => rustpy_settings::Settings::config_dir().join("index"),
    }
}

/// Shared index bundles installed with RustPy: `shared-indexes/` next to the executable.
pub fn builtin_bundle_dirs() -> Vec<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("shared-indexes")))
        .filter(|d| d.is_dir())
        .into_iter()
        .collect()
}

/// A burst of file changes in the project is indexed this long after its last change.
const PROJECT_DEBOUNCE: Duration = Duration::from_millis(800);
/// How often the library directories are checked for added or removed packages.
const ROOT_CHECK_INTERVAL: Duration = Duration::from_secs(10);
/// Library files edited in place are picked up by a periodic refresh.
const LIBRARY_REFRESH_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// The interpreter a library index is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryTarget {
    pub interpreter: PathBuf,
    pub label: String,
    pub version: String,
    /// `sys.path` (directories that do not exist are skipped when indexing).
    pub sys_path: Vec<PathBuf>,
}

impl LibraryTarget {
    pub fn of(i: &Interpreter) -> LibraryTarget {
        LibraryTarget {
            interpreter: i.path.clone(),
            label: i.label(),
            version: i.version.clone(),
            sys_path: i.sys_path.clone(),
        }
    }

    fn is_for(&self, i: &Interpreter) -> bool {
        self.interpreter == i.path && self.version == i.version && self.sys_path == i.sys_path
    }
}

/// `(name, detail, path, line)` of a module-level declaration (see
/// [`project_symbol_list`]).
pub type ProjectSymbol = (String, String, PathBuf, usize);

/// What the IDE derives from a project index for its consumers. Computed on
/// the job thread: for a large project it takes a moment.
#[derive(Debug, Default)]
pub struct ProjectData {
    /// Absolute paths of the indexed files.
    pub files: Arc<FxHashSet<PathBuf>>,
    /// Ids, in the project index, of its module-level classes, functions
    /// and constants (see [`project_symbol_list`]). Their strings stay in the
    /// index: owned copies took about 80 MB for a 100k-file project.
    pub symbols: Arc<[u32]>,
}

impl ProjectData {
    pub fn of(index: &SymbolIndex) -> ProjectData {
        ProjectData {
            files: Arc::new(index.file_paths()),
            symbols: index.top_level_declaration_ids(PROJECT_SYMBOL_KINDS).into(),
        }
    }
}

/// Indexes and derived data that were replaced or dropped, freed on a
/// background thread once nothing else holds them (snapshots and analysis
/// inputs share them): freeing a large project's takes tens of milliseconds
/// (hundreds of thousands of allocations), which the UI thread would
/// otherwise spend after every refresh.
#[derive(Default)]
struct Retired(Vec<Arc<dyn std::any::Any + Send + Sync>>);

impl Retired {
    fn index(&mut self, index: Option<Arc<SymbolIndex>>) {
        if let Some(i) = index {
            self.0.push(i);
        }
    }

    /// The file set of `data` (snapshots share it, not `data` itself; its
    /// symbol ids are a single allocation).
    fn data(&mut self, data: Option<Arc<ProjectData>>) {
        if let Some(d) = data {
            self.0.push(d.files.clone());
        }
    }

    /// Frees, on a background thread, what nothing else holds any more.
    fn collect(&mut self) {
        if self.0.is_empty() {
            return;
        }
        let (free, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.0)
            .into_iter()
            .partition(|a| Arc::strong_count(a) == 1);
        self.0 = keep;
        if !free.is_empty() {
            // Should no thread start, they are freed here.
            let _ = std::thread::Builder::new()
                .name("rustpy-index-free".into())
                .spawn(move || {
                    drop(free);
                    rustpy_core::memory::release_free_memory();
                });
        }
    }
}

/// An index and what is derived from it.
struct Loaded {
    index: Arc<SymbolIndex>,
    /// Project indexes only.
    data: Option<Arc<ProjectData>>,
}

enum JobMsg {
    /// The index as stored on disk, usable while it is refreshed.
    Loaded(Loaded),
    /// The refresh finished: a new index, or `None` when the loaded one is current.
    Done(Result<(Option<Loaded>, RefreshStats, Option<String>), String>),
}

struct Job {
    progress: Arc<Progress>,
    rx: Receiver<JobMsg>,
    started: Instant,
    rebuild: bool,
}

/// One index the IDE keeps current.
struct Slot<T> {
    target: Option<T>,
    index: Option<Arc<SymbolIndex>>,
    /// Derived from `index` (project indexes).
    data: Option<Arc<ProjectData>>,
    job: Option<Job>,
    /// When the last refresh started.
    refreshed: Option<Instant>,
    /// A refresh asked for while one ran (`true`: rebuild from scratch).
    queued: Option<bool>,
    stats: Option<RefreshStats>,
    error: Option<String>,
    /// Deleted in Settings: not built again until asked for.
    suppressed: bool,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Slot {
            target: None,
            index: None,
            data: None,
            job: None,
            refreshed: None,
            queued: None,
            stats: None,
            error: None,
            suppressed: false,
        }
    }
}

impl<T> Slot<T> {
    fn cancel(&mut self) {
        if let Some(job) = self.job.take() {
            job.progress.cancel();
            // A completed result may already be queued and exclusively own
            // large tables. Disconnect/free the queue off the UI thread.
            let _ = std::thread::Builder::new()
                .name("rustpy-index-cancel".into())
                .spawn(move || drop(job));
        }
        self.queued = None;
    }

    fn reset(&mut self, target: Option<T>, retired: &mut Retired) {
        self.cancel();
        retired.index(self.index.take());
        retired.data(self.data.take());
        *self = Slot {
            target,
            ..Slot::default()
        };
    }

    /// Collects messages from the running job; true when `index` changed.
    /// What it replaces goes to `retired`.
    fn poll(&mut self, retired: &mut Retired) -> bool {
        let mut changed = false;
        while let Some(job) = &self.job {
            match job.rx.try_recv() {
                Ok(JobMsg::Loaded(loaded)) => {
                    if self.index.is_none() {
                        self.index = Some(loaded.index);
                        retired.data(std::mem::replace(&mut self.data, loaded.data));
                        changed = true;
                    } else {
                        retired.index(Some(loaded.index));
                        retired.data(loaded.data);
                    }
                }
                Ok(JobMsg::Done(result)) => {
                    match result {
                        Ok((Some(loaded), stats, warning)) => {
                            log::info!(
                                "index: {} — {}",
                                loaded.index.meta().label,
                                stats.summary()
                            );
                            retired.index(self.index.replace(loaded.index));
                            retired.data(std::mem::replace(&mut self.data, loaded.data));
                            self.stats = Some(stats);
                            self.error = warning;
                            changed = true;
                        }
                        Ok((None, stats, warning)) => {
                            self.stats = Some(stats);
                            // A no-change refresh did not save the index: retain
                            // any previous persistence failure until a save succeeds.
                            if warning.is_some() {
                                self.error = warning;
                            }
                        }
                        Err(e) => self.error = Some(e),
                    }
                    self.job = None;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.job = None;
                    self.error = Some("indexing stopped unexpectedly".into());
                }
            }
        }
        changed
    }

    fn view(&self, label: String, file: Option<PathBuf>) -> IndexView {
        IndexView {
            label,
            file,
            index: self.index.clone(),
            running: self.job.as_ref().map(|j| j.progress.counts()),
            rebuilding: self.job.as_ref().is_some_and(|j| j.rebuild),
            stats: self.stats.clone(),
            error: self.error.clone(),
            suppressed: self.suppressed,
        }
    }
}

/// What a running index job is about, for the status bar.
#[derive(Clone, Debug, PartialEq)]
pub struct IndexStatus {
    pub text: String,
    /// Completed fraction, `None` while files are being listed.
    pub fraction: Option<f32>,
}

/// The state of the library or the project index, for Settings.
#[derive(Clone, Debug)]
pub struct IndexView {
    /// `Python 3.12 (venv)`, the project name.
    pub label: String,
    /// The index file.
    pub file: Option<PathBuf>,
    pub index: Option<Arc<SymbolIndex>>,
    /// `(done, total)` of a running refresh (`total` is 0 while listing files).
    pub running: Option<(usize, usize)>,
    pub rebuilding: bool,
    pub stats: Option<RefreshStats>,
    pub error: Option<String>,
    /// Deleted: not rebuilt until asked for.
    pub suppressed: bool,
}

/// Settings › Tools › Shared Indexes: text fields and options.
#[derive(Clone, Debug, Default)]
pub struct SharedIndexesPage {
    pub export_path: String,
    /// The suggested export path last shown: the path follows the
    /// suggestion (Python version, packages option) until it is edited.
    pub export_default: String,
    pub include_packages: bool,
    pub import_path: String,
}

/// The indexes as consumers see them at one moment: cheap to clone, usable
/// from any thread.
#[derive(Clone, Debug, Default)]
pub struct IndexSnapshot {
    /// [`SymbolIndexes::generation`] it was taken at.
    pub generation: u64,
    pub project: Option<Arc<SymbolIndex>>,
    pub library: Option<Arc<SymbolIndex>>,
    /// Files of the project index: library hits in them are duplicates (an
    /// editable install puts project sources on `sys.path`).
    pub project_files: Option<Arc<FxHashSet<PathBuf>>>,
    /// Ids of the project index's module-level classes, functions and
    /// constants: what the "Import" quick fix offers from project modules
    /// (see [`IndexSnapshot::project_symbol_list`]).
    pub project_symbols: Option<Arc<[u32]>>,
    /// The interpreter's libraries are being indexed for the first time.
    pub library_pending: bool,
    /// The project is being indexed for the first time.
    pub project_pending: bool,
}

/// A symbol found by [`IndexSnapshot::search`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundSymbol {
    pub hit: SymbolHit,
    /// From the interpreter's libraries rather than the project.
    pub library: bool,
}

impl IndexSnapshot {
    /// The project index's module-level classes, functions and constants,
    /// `(name, detail, path, line)` (see [`project_symbol_list`]), built from
    /// the index as they are iterated; `None` without a project index.
    pub fn project_symbol_list(&self) -> Option<impl Iterator<Item = ProjectSymbol> + '_> {
        let (index, ids) = (self.project.as_ref()?, self.project_symbols.as_ref()?);
        Some(ids.iter().filter_map(|&id| {
            let d = index.declaration(id)?;
            Some((d.name, d.detail, d.path, d.line as usize))
        }))
    }

    /// Go to Class (`classes_only`) / Go to Symbol / Search Everywhere: project
    /// symbols and, with `include_libraries`, library symbols matching `query`
    /// by `score`, best first (project symbols first among equal scores).
    pub fn search(
        &self,
        query: &str,
        classes_only: bool,
        include_libraries: bool,
        limit: usize,
        score: &dyn Fn(&str, &str) -> Option<i32>,
    ) -> Vec<FoundSymbol> {
        let mut out: Vec<FoundSymbol> = Vec::new();
        if let Some(p) = &self.project {
            let opts = SearchOptions {
                classes_only,
                limit,
                ..Default::default()
            };
            out.extend(
                p.search(query, &opts, score)
                    .into_iter()
                    .map(|hit| FoundSymbol {
                        hit,
                        library: false,
                    }),
            );
        }
        if include_libraries {
            if let Some(l) = &self.library {
                let opts = SearchOptions {
                    classes_only,
                    limit,
                    exclude_files: self.project_files.clone(),
                    ..Default::default()
                };
                out.extend(
                    l.search(query, &opts, score)
                        .into_iter()
                        .map(|hit| FoundSymbol { hit, library: true }),
                );
            }
        }
        out.sort_by(|a, b| {
            b.hit
                .score
                .cmp(&a.hit.score)
                .then(a.library.cmp(&b.library))
        });
        out.truncate(limit);
        out
    }

    /// Second-press completion: names from the interpreter's libraries that
    /// match the typed `prefix` (as completion matches: prefix, substring,
    /// word starts), best first.
    pub fn import_names(&self, prefix: &str, limit: usize) -> Vec<ImportableName> {
        let Some(library) = &self.library else {
            return Vec::new();
        };
        let lower = prefix.to_lowercase();
        let matches = |name: &str| rustpy_semantic::features::matches_prefix(name, &lower);
        library
            .import_candidates(&lower, &matches, limit)
            .into_iter()
            .map(|c| ImportableName {
                module: c.module,
                name: c.name,
                kind: completion_kind(c.kind),
            })
            .collect()
    }

    /// Class-only import lookup, preserving public re-export preference and
    /// applying the kind filter before the index's bounded result selection.
    pub fn import_class_names(&self, prefix: &str, limit: usize) -> Vec<ImportableName> {
        let Some(library) = &self.library else {
            return Vec::new();
        };
        let lower = prefix.to_lowercase();
        let matches = |name: &str| rustpy_semantic::features::matches_prefix(name, &lower);
        library
            .class_import_candidates(&lower, &matches, limit)
            .into_iter()
            .map(|candidate| ImportableName {
                module: candidate.module,
                name: candidate.name,
                kind: CompletionKind::Class,
            })
            .collect()
    }
    /// Modules of the interpreter's libraries to import exactly `name` from.
    pub fn import_modules(&self, name: &str, limit: usize) -> Vec<String> {
        self.library
            .as_ref()
            .map(|l| {
                l.exact_import_candidates(name, limit)
                    .into_iter()
                    .map(|c| c.module)
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn completion_kind(kind: SymbolKind) -> CompletionKind {
    match kind {
        SymbolKind::Class => CompletionKind::Class,
        SymbolKind::Function => CompletionKind::Function,
        SymbolKind::Method => CompletionKind::Method,
        SymbolKind::Constant => CompletionKind::Constant,
        SymbolKind::Variable | SymbolKind::Import => CompletionKind::Variable,
    }
}

/// Modification times of `dirs` (a package installed or removed changes its directory's).
fn dir_stamps(dirs: &[PathBuf]) -> Vec<(PathBuf, Option<SystemTime>)> {
    dirs.iter()
        .map(|d| {
            (
                d.clone(),
                std::fs::metadata(d).and_then(|m| m.modified()).ok(),
            )
        })
        .collect()
}

/// Shared bundles loaded for jobs. The lock is only held to read or replace
/// the list, never while bundles are decoded (the UI thread takes it too).
#[derive(Default)]
struct BundleCache {
    /// Bumped whenever bundles are imported or deleted: a load that started
    /// before is not kept.
    epoch: u64,
    /// `None`: not loaded yet.
    loaded: Option<Vec<Arc<SymbolIndex>>>,
}

impl BundleCache {
    fn invalidate(cache: &Mutex<BundleCache>) {
        let retired = {
            let mut c = cache.lock();
            c.epoch += 1;
            c.loaded.take()
        };
        // The cache may own the last references to large bundle tables.
        // Invalidating from Settings must not free them on the UI thread.
        if let Some(retired) = retired {
            let _ = std::thread::Builder::new()
                .name("rustpy-bundle-free".into())
                .spawn(move || drop(retired));
        }
    }

    /// The bundles of `store`, loaded on first use (outside the lock).
    fn get(cache: &Mutex<BundleCache>, store: &IndexStore) -> Vec<Arc<SymbolIndex>> {
        Self::get_with(cache, || store.load_bundles())
    }

    /// [`Self::get`] with `load` reading the bundles.
    fn get_with(
        cache: &Mutex<BundleCache>,
        load: impl FnOnce() -> Vec<Arc<SymbolIndex>>,
    ) -> Vec<Arc<SymbolIndex>> {
        let epoch = {
            let c = cache.lock();
            if let Some(b) = &c.loaded {
                return b.clone();
            }
            c.epoch
        };
        let loaded = load();
        let mut c = cache.lock();
        if c.epoch == epoch && c.loaded.is_none() {
            c.loaded = Some(loaded.clone());
        }
        loaded
    }
}

/// The project's and its interpreter's symbol indexes.
pub struct SymbolIndexes {
    store: IndexStore,
    library: Slot<LibraryTarget>,
    project: Slot<PathBuf>,
    /// Folders marked *Excluded* in the Project view (sorted): not indexed.
    project_excluded: Vec<PathBuf>,
    project_excluded_changed: bool,
    /// Loaded shared bundles, shared with jobs.
    bundles: Arc<Mutex<BundleCache>>,
    /// Held by a job while it puts a refreshed index in place (after checking
    /// that it was not cancelled) and by [`Self::delete`] while it deletes
    /// one: a refresh finishing meanwhile never writes a deleted index back.
    save_lock: Arc<Mutex<()>>,
    generation: u64,
    listing: Option<(Instant, Vec<StoredIndex>)>,
    /// Exports and imports started from Settings: their result messages.
    tasks: Vec<Receiver<Result<String, String>>>,
    /// Replaced and dropped indexes, freed off the UI thread.
    retired: Retired,
    /// Messages for the status bar (drained by the IDE).
    pub messages: Vec<String>,
    /// The last export/import result, shown on the Settings page.
    pub last_message: Option<Result<String, String>>,
    /// Last change reported by the file watcher, not indexed yet.
    project_changed_at: Option<Instant>,
    /// Library directories and their modification times when the library index was last refreshed.
    library_roots: Vec<(PathBuf, Option<SystemTime>)>,
    roots_checked: Option<Instant>,
    snapshot: IndexSnapshot,
    /// Settings page state.
    pub page: SharedIndexesPage,
}

impl Default for SymbolIndexes {
    fn default() -> Self {
        SymbolIndexes::new(IndexStore::new(index_dir()).with_builtin_dirs(builtin_bundle_dirs()))
    }
}

impl SymbolIndexes {
    pub fn new(store: IndexStore) -> SymbolIndexes {
        SymbolIndexes {
            store,
            library: Slot::default(),
            project: Slot::default(),
            project_excluded: Vec::new(),
            project_excluded_changed: false,
            bundles: Arc::new(Mutex::new(BundleCache::default())),
            save_lock: Arc::new(Mutex::new(())),
            generation: 0,
            listing: None,
            tasks: Vec::new(),
            retired: Retired::default(),
            messages: Vec::new(),
            last_message: None,
            project_changed_at: None,
            library_roots: Vec::new(),
            roots_checked: None,
            snapshot: IndexSnapshot::default(),
            page: SharedIndexesPage::default(),
        }
    }

    pub fn store(&self) -> &IndexStore {
        &self.store
    }
    /// Publish an independently staged download to the same bundle cache authority.
    pub(crate) fn shared_bundles_changed(&mut self) {
        self.listing = None;
        BundleCache::invalidate(&self.bundles);
        self.refresh_library(Duration::ZERO);
    }

    /// The library index of the project interpreter (possibly still being refreshed).
    pub fn library(&self) -> Option<Arc<SymbolIndex>> {
        self.library.index.clone()
    }

    /// The project index (possibly still being refreshed).
    pub fn project(&self) -> Option<Arc<SymbolIndex>> {
        self.project.index.clone()
    }

    /// Bumped whenever [`Self::library`] or [`Self::project`] changes.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The indexes for consumers (rebuilt only when they changed). Cheap:
    /// what is derived from the indexes was computed by their jobs.
    pub fn snapshot(&mut self) -> IndexSnapshot {
        let pending = self.library.index.is_none() && self.library.job.is_some();
        let project_pending = self.project.index.is_none() && self.project.job.is_some();
        if self.snapshot.generation != self.generation
            || self.snapshot.library_pending != pending
            || self.snapshot.project_pending != project_pending
        {
            let data = self.project.index.as_ref().and(self.project.data.as_ref());
            self.snapshot = IndexSnapshot {
                generation: self.generation,
                project: self.project.index.clone(),
                library: self.library.index.clone(),
                project_files: data.map(|d| d.files.clone()),
                project_symbols: data.map(|d| d.symbols.clone()),
                library_pending: pending,
                project_pending,
            };
        }
        self.snapshot.clone()
    }

    pub fn library_target(&self) -> Option<&LibraryTarget> {
        self.library.target.as_ref()
    }

    pub fn project_root(&self) -> Option<&Path> {
        self.project.target.as_deref()
    }

    pub fn library_file(&self) -> Option<PathBuf> {
        self.library
            .target
            .as_ref()
            .map(|t| self.store.library_path(&t.interpreter))
    }

    pub fn project_file(&self) -> Option<PathBuf> {
        self.project
            .target
            .as_ref()
            .map(|r| self.store.project_path(r))
    }

    /// The library index's state for Settings.
    pub fn library_view(&self) -> Option<IndexView> {
        let t = self.library.target.as_ref()?;
        Some(self.library.view(
            format!("{} — {}", t.label, t.interpreter.display()),
            self.library_file(),
        ))
    }

    /// The project index's state for Settings.
    pub fn project_view(&self) -> Option<IndexView> {
        let root = self.project.target.as_ref()?;
        Some(
            self.project
                .view(root.display().to_string(), self.project_file()),
        )
    }

    /// Follows the open project and its interpreter; cheap when neither changed
    /// (call it every frame).
    pub fn sync(&mut self, root: Option<&Path>, interpreter: Option<&Interpreter>) {
        self.set_project_root(root);
        self.set_library_interpreter(interpreter);
    }

    /// Follows the project interpreter (`None`: no interpreter): a new
    /// interpreter loads its index and refreshes it.
    pub fn set_library_interpreter(&mut self, interpreter: Option<&Interpreter>) {
        let same = match (&self.library.target, interpreter) {
            (Some(t), Some(i)) => t.is_for(i),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.library
            .reset(interpreter.map(LibraryTarget::of), &mut self.retired);
        self.library_roots.clear();
        self.generation += 1;
        if self.library.target.is_some() {
            self.start_library(false);
        }
    }

    /// Follows the open project.
    pub fn set_project_root(&mut self, root: Option<&Path>) {
        if self.project.target.as_deref() == root {
            return;
        }
        self.project
            .reset(root.map(Path::to_path_buf), &mut self.retired);
        self.project_changed_at = None;
        self.project_excluded_changed = false;
        self.generation += 1;
        if self.project.target.is_some() {
            self.start_project(false);
        }
    }

    /// Follows the folders marked *Excluded* in the Project view (call it
    /// before [`Self::sync`]; cheap when they did not change): their files are
    /// not indexed, and a change refreshes the project index.
    pub fn set_project_excluded<'a>(
        &mut self,
        excluded: impl ExactSizeIterator<Item = &'a PathBuf> + Clone,
    ) {
        let same = excluded.len() == self.project_excluded.len()
            && excluded
                .clone()
                .all(|p| self.project_excluded.binary_search(p).is_ok());
        if same {
            return;
        }
        let mut dirs: Vec<PathBuf> = excluded.cloned().collect();
        dirs.sort();
        self.project_excluded = dirs;
        self.project_excluded_changed = true;
    }

    /// Files changed in the project (the file watcher saw them): the project
    /// index is refreshed once the changes stop for a moment.
    pub fn files_changed(&mut self) {
        self.project.cancel();
        self.project_changed_at = Some(Instant::now());
    }

    /// Starts the refreshes that are due: the project index after file
    /// changes, the library index when a library directory changed (a package
    /// was installed or removed) and periodically. Cheap; call it every frame.
    pub fn maintain(&mut self) {
        if std::mem::take(&mut self.project_excluded_changed) {
            self.refresh_project(Duration::ZERO);
        }
        if self
            .project_changed_at
            .is_some_and(|t| t.elapsed() >= PROJECT_DEBOUNCE)
        {
            self.project_changed_at = None;
            self.refresh_project(Duration::ZERO);
        }
        if self
            .roots_checked
            .is_some_and(|t| t.elapsed() < ROOT_CHECK_INTERVAL)
        {
            return;
        }
        self.roots_checked = Some(Instant::now());
        if self.library.job.is_some() || self.library_roots.is_empty() {
            return;
        }
        let dirs: Vec<PathBuf> = self.library_roots.iter().map(|(d, _)| d.clone()).collect();
        if dir_stamps(&dirs) != self.library_roots {
            log::debug!("index: a library directory changed; refreshing");
            self.refresh_library(Duration::ZERO);
        } else {
            self.refresh_library(LIBRARY_REFRESH_INTERVAL);
        }
    }

    /// Refreshes the library index unless that happened less than `min_age` ago.
    pub fn refresh_library(&mut self, min_age: Duration) {
        if self.library.target.is_none()
            || self.library.suppressed
            || self.library.job.is_some()
            || self
                .library
                .refreshed
                .is_some_and(|t| t.elapsed() < min_age)
        {
            return;
        }
        self.start_library(false);
    }

    /// Refreshes the project index unless that happened less than `min_age`
    /// ago (queued behind a running refresh).
    pub fn refresh_project(&mut self, min_age: Duration) {
        if self.project.target.is_none()
            || self.project.suppressed
            || self
                .project
                .refreshed
                .is_some_and(|t| t.elapsed() < min_age)
        {
            return;
        }
        if self.project.job.is_some() {
            self.project.queued.get_or_insert(false);
            return;
        }
        self.start_project(false);
    }

    /// Rebuilds the library index from scratch (Settings, Invalidate Caches).
    pub fn rebuild_library(&mut self) {
        if self.library.target.is_some() {
            self.library.suppressed = false;
            self.library.cancel();
            let error = self.evict_generated(self.library_file());
            self.retired.index(self.library.index.take());
            self.library.stats = None;
            self.library.error = error;
            self.generation += 1;
            self.start_library(true);
        }
    }

    pub fn rebuild_project(&mut self) {
        if self.project.target.is_some() {
            self.project.suppressed = false;
            self.project.cancel();
            let error = self.evict_generated(self.project_file());
            self.retired.index(self.project.index.take());
            self.retired.data(self.project.data.take());
            self.project.stats = None;
            self.project.error = error;
            self.project_changed_at = None;
            self.generation += 1;
            self.start_project(true);
        }
    }

    /// Cancel the slot before calling: under this lock no cancelled worker
    /// can republish the disk artifact after its removal.
    fn evict_generated(&mut self, path: Option<PathBuf>) -> Option<String> {
        let path = path?;
        let result = {
            let _saving = self.save_lock.lock();
            self.store.delete(&path)
        };
        match result {
            Ok(()) => None,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                let message = format!("Cannot invalidate {}: {e}", path.display());
                self.messages.push(message.clone());
                self.last_message = Some(Err(message.clone()));
                Some(message)
            }
        }
    }

    /// Deletes an index file. Deleting the library or project index in use
    /// also drops it from memory; it is not built again in this session until
    /// Rebuild is chosen (it is rebuilt when RustPy starts next time).
    pub fn delete(&mut self, path: &Path) -> std::io::Result<()> {
        let library = self.library_file().as_deref() == Some(path);
        let project = self.project_file().as_deref() == Some(path);
        // A refresh of this index is cancelled first; under the save lock it
        // can no longer put its result in place after the file is gone.
        if library {
            self.library.cancel();
        }
        if project {
            self.project.cancel();
        }
        {
            let _saving = self.save_lock.lock();
            self.store.delete(path)?;
        }
        if library {
            self.retired.index(self.library.index.take());
            self.library.suppressed = true;
            self.generation += 1;
        }
        if project {
            self.retired.index(self.project.index.take());
            self.retired.data(self.project.data.take());
            self.project.suppressed = true;
            self.generation += 1;
        }
        if path.starts_with(self.store.shared_dir()) {
            BundleCache::invalidate(&self.bundles);
        }
        self.listing = None;
        Ok(())
    }

    fn start_library(&mut self, rebuild: bool) {
        let Some(target) = self.library.target.clone() else {
            return;
        };
        let dirs: Vec<PathBuf> = target
            .sys_path
            .iter()
            .filter(|p| p.is_dir())
            .cloned()
            .collect();
        self.library_roots = dir_stamps(&dirs);
        let roots: Vec<RootSpec> = dirs
            .into_iter()
            .map(|p| RootSpec {
                kind: RootKind::classify(&p),
                path: p,
            })
            .collect();
        let (store, bundles, list_roots) =
            (self.store.clone(), self.bundles.clone(), roots.clone());
        let spec = JobSpec {
            name: format!("{} libraries", target.label),
            path: self.store.library_path(&target.interpreter),
            meta: IndexMeta {
                kind: IndexKind::Libraries,
                label: target.label.clone(),
                owner: target.interpreter.display().to_string(),
                python_version: target.version.clone(),
                ..Default::default()
            },
            roots,
            list: Box::new(move |progress: &Progress| walk_library_roots(&list_roots, progress)),
            shared: Box::new(move || BundleCache::get(&bundles, &store)),
            derive: false,
            save_lock: self.save_lock.clone(),
        };
        let old = if rebuild {
            None
        } else {
            self.library.index.clone()
        };
        self.library.job = Some(spawn_job(spec, rebuild, old));
        self.library.refreshed = Some(Instant::now());
        self.listing = None;
    }

    fn start_project(&mut self, rebuild: bool) {
        let Some(root) = self.project.target.clone() else {
            return;
        };
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());
        let list_root = root.clone();
        let excluded = self.project_excluded.clone();
        let spec = JobSpec {
            name: format!("project {name}"),
            path: self.store.project_path(&root),
            meta: IndexMeta {
                kind: IndexKind::Project,
                label: name,
                owner: root.display().to_string(),
                ..Default::default()
            },
            roots: vec![RootSpec {
                path: root,
                kind: RootKind::Project,
            }],
            list: Box::new(move |_: &Progress| project_sources(&list_root, &excluded)),
            shared: Box::new(Vec::new),
            derive: true,
            save_lock: self.save_lock.clone(),
        };
        let old = if rebuild {
            None
        } else {
            self.project.index.clone()
        };
        self.project.job = Some(spawn_job(spec, rebuild, old));
        self.project.refreshed = Some(Instant::now());
        self.listing = None;
    }

    /// Collects finished jobs; true when an index changed (consumers should
    /// re-run their queries). Starts queued refreshes, and frees replaced
    /// indexes nothing holds any more (in the background).
    pub fn poll(&mut self) -> bool {
        self.retired.collect();
        let mut changed =
            self.library.poll(&mut self.retired) | self.project.poll(&mut self.retired);
        if self.library.job.is_none() {
            if let Some(rebuild) = self.library.queued.take() {
                self.start_library(rebuild);
            }
        }
        if self.project.job.is_none() {
            if let Some(rebuild) = self.project.queued.take() {
                self.start_project(rebuild);
            }
        }
        let mut finished = false;
        let mut results = Vec::new();
        self.tasks.retain(|rx| match rx.try_recv() {
            Ok(result) => {
                results.push(result);
                false
            }
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => false,
        });
        for result in results {
            finished = true;
            match &result {
                Ok(m) | Err(m) => self.messages.push(m.clone()),
            }
            self.last_message = Some(result);
        }
        if finished {
            self.listing = None;
            BundleCache::invalidate(&self.bundles);
        }
        if changed {
            self.generation += 1;
            self.listing = None;
        }
        changed |= finished;
        changed
    }

    /// Whether an index job or a bundle export/import is running.
    pub fn busy(&self) -> bool {
        self.library.job.is_some() || self.project.job.is_some() || !self.tasks.is_empty()
    }

    /// Dumb mode: an index is being built and the status bar shows it (a
    /// job running for more than a moment, or a rebuild). Features needing
    /// complete indexes wait or flag partial results meanwhile
    /// ([`crate::dumb_mode`]); a quick incremental refresh does not count.
    pub fn is_dumb(&self) -> bool {
        self.status().is_some()
    }

    /// The running job worth showing (one running for more than a moment).
    pub fn status(&self) -> Option<IndexStatus> {
        let describe = |job: &Job, what: String| {
            let (done, total) = job.progress.counts();
            let fraction = job.progress.fraction();
            let text = if fraction.is_some() {
                format!("Indexing {what}… {done}/{total}")
            } else {
                format!("Scanning {what}…")
            };
            IndexStatus { text, fraction }
        };
        let quiet = Duration::from_millis(400);
        if let (Some(job), Some(t)) = (&self.library.job, &self.library.target) {
            if job.started.elapsed() > quiet || job.rebuild {
                return Some(describe(job, format!("{} libraries", t.label)));
            }
        }
        if let (Some(job), Some(root)) = (&self.project.job, &self.project.target) {
            if job.started.elapsed() > quiet || job.rebuild {
                let name = root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                return Some(describe(job, format!("project {name}")));
            }
        }
        None
    }

    /// Last refresh results: `(library, project)`.
    pub fn last_stats(&self) -> (Option<&RefreshStats>, Option<&RefreshStats>) {
        (self.library.stats.as_ref(), self.project.stats.as_ref())
    }

    /// Last errors: `(library, project)`.
    pub fn errors(&self) -> (Option<&str>, Option<&str>) {
        (self.library.error.as_deref(), self.project.error.as_deref())
    }

    pub fn library_suppressed(&self) -> bool {
        self.library.suppressed
    }

    /// Index files on disk (read at most every two seconds).
    pub fn listing(&mut self) -> &[StoredIndex] {
        let stale = self
            .listing
            .as_ref()
            .is_none_or(|(t, _)| t.elapsed() > Duration::from_secs(2));
        if stale {
            self.listing = Some((Instant::now(), self.store.list()));
        }
        self.listing
            .as_ref()
            .map(|(_, l)| l.as_slice())
            .unwrap_or_default()
    }

    /// Exports the standard library part of the library index (with
    /// `include_packages`, all of it) as a shared bundle at `dest`.
    pub fn export_bundle(&mut self, dest: PathBuf, include_packages: bool) {
        let Some(index) = self.library.index.clone() else {
            let m = "Nothing to export: the interpreter libraries are not indexed yet".to_string();
            self.messages.push(m.clone());
            self.last_message = Some(Err(m));
            return;
        };
        let (tx, rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            // Never other `sys.path` entries: they hold the sources of
            // editable installs (the user's own projects) and PYTHONPATH.
            let kinds = bundle_root_kinds(include_packages);
            let meta = index.meta();
            let what = if include_packages {
                "libraries"
            } else {
                "stdlib"
            };
            let label = format!("Python {} {what}", meta.python_version);
            let now = rustpy_index::build::now_unix();
            let bundle = index.to_bundle(
                kinds,
                IndexMeta {
                    label,
                    python_version: meta.python_version.clone(),
                    created: now,
                    updated: now,
                    ..Default::default()
                },
            );
            let result = if bundle.file_count() == 0 {
                Err("Nothing to export: no standard library directory was indexed".to_string())
            } else {
                IndexStore::export_bundle(&bundle, &dest)
                    .map(|size| {
                        format!(
                            "Exported {} files ({}) to {}",
                            bundle.file_count(),
                            format_size(size),
                            dest.display()
                        )
                    })
                    .map_err(|e| format!("Cannot export to {}: {e}", dest.display()))
            };
            let _ = tx.send(result);
        });
        self.tasks.push(rx);
    }

    /// Imports a shared bundle file: later refreshes take the symbols of
    /// files with the same content from it instead of parsing them.
    pub fn import_bundle(&mut self, src: PathBuf) {
        let store = self.store.clone();
        let (tx, rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            let result = match store.import_bundle(&src) {
                Ok(s) => Ok(format!(
                    "Imported shared index “{}” ({} files)",
                    s.meta.label, s.meta.files
                )),
                Err(e) => Err(format!("Cannot import {}: {e}", src.display())),
            };
            let _ = tx.send(result);
        });
        self.tasks.push(rx);
    }
}

/// The kinds of library roots a shared bundle is exported from: the standard
/// library, and with `include_packages` site-packages.
pub fn bundle_root_kinds(include_packages: bool) -> &'static [RootKind] {
    if include_packages {
        &[RootKind::Stdlib, RootKind::Site]
    } else {
        &[RootKind::Stdlib]
    }
}

/// Lists the files to index (runs on the job thread).
type FileLister = Box<dyn FnOnce(&Progress) -> Vec<SourceFile> + Send>;
/// Loads the shared bundles to take symbols from (runs on the job thread, when needed).
type BundleLoader = Box<dyn FnOnce() -> Vec<Arc<SymbolIndex>> + Send>;

/// What a background job indexes.
struct JobSpec {
    /// For logs and the thread name.
    name: String,
    /// The index file.
    path: PathBuf,
    meta: IndexMeta,
    roots: Vec<RootSpec>,
    list: FileLister,
    shared: BundleLoader,
    /// Compute [`ProjectData`] for the index (project indexes).
    derive: bool,
    save_lock: Arc<Mutex<()>>,
}

/// Starts a background job. It loads the stored index (unless rebuilding or
/// `old` is given) and sends it, refreshes it and saves the result.
fn spawn_job(spec: JobSpec, rebuild: bool, old: Option<Arc<SymbolIndex>>) -> Job {
    let progress = Arc::new(Progress::default());
    let (tx, rx) = crossbeam_channel::unbounded();
    let job_progress = progress.clone();
    let thread_name = format!("rustpy-index {}", spec.name);
    let spawned = std::thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            let JobSpec {
                name,
                path,
                meta,
                roots,
                list,
                shared,
                derive,
                save_lock,
            } = spec;
            let progress = job_progress;
            let loaded = |index: Arc<SymbolIndex>| Loaded {
                data: derive.then(|| Arc::new(ProjectData::of(&index))),
                index,
            };
            let mut old = if rebuild { None } else { old };
            if old.is_none() && !rebuild {
                match IndexStore::load(&path) {
                    Ok(idx) if idx.meta().kind == meta.kind => {
                        let idx = Arc::new(idx);
                        let _ = tx.send(JobMsg::Loaded(loaded(idx.clone())));
                        old = Some(idx);
                    }
                    Ok(_) => log::warn!(
                        "index: {} is not a {:?} index; rebuilding",
                        path.display(),
                        meta.kind
                    ),
                    Err(IndexError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => log::warn!("index: cannot load {} ({e}); rebuilding", path.display()),
                }
            }
            let files = list(&progress);
            if progress.is_cancelled() {
                let _ = tx.send(JobMsg::Done(Err("cancelled".into())));
                return;
            }
            // Bundles only help when something has to be read.
            let bundles = if old.is_none()
                || files.len() != old.as_ref().map_or(0, |o| o.file_count())
                || rebuild
            {
                shared()
            } else {
                Vec::new()
            };
            let result = match refresh(
                old.as_deref(),
                meta,
                &roots,
                files,
                &bundles,
                &BuildOptions::default(),
                &progress,
            ) {
                Ok((Some(index), stats)) => {
                    log::debug!("index: {name}: {}", stats.summary());
                    // Written aside, then put in place unless the job was
                    // cancelled meanwhile (the index deleted in Settings).
                    let warning = match IndexStore::stage(&path, &index) {
                        Ok(staged) => {
                            let _saving = save_lock.lock();
                            if progress.is_cancelled() {
                                staged.discard();
                                None
                            } else {
                                staged
                                    .finish()
                                    .err()
                                    .map(|e| format!("Cannot save {}: {e}", path.display()))
                            }
                        }
                        Err(e) => Some(format!("Cannot save {}: {e}", path.display())),
                    };
                    if progress.is_cancelled() {
                        Err("cancelled".to_string())
                    } else {
                        Ok((Some(loaded(Arc::new(index))), stats, warning))
                    }
                }
                Ok((None, stats)) => Ok((None, stats, None)),
                Err(_) => Err("cancelled".to_string()),
            };
            let _ = tx.send(JobMsg::Done(result));
            // The build's temporary tables are freed: give their pages back.
            drop((old, bundles));
            rustpy_core::memory::release_free_memory();
        });
    if let Err(e) = spawned {
        log::warn!("index: cannot start a background thread: {e}");
    }
    Job {
        progress,
        rx,
        started: Instant::now(),
        rebuild,
    }
}

/// The project's Python files, as Find in Files and Go to File see them
/// (`.gitignore`d files, virtual environments, build output and folders
/// marked *Excluded* left out).
fn project_sources(root: &Path, excluded: &[PathBuf]) -> Vec<SourceFile> {
    let mut files: Vec<PathBuf> = rustpy_search::project_files_excluding(root, excluded)
        .into_iter()
        .filter(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("py" | "pyi")))
        .collect();
    // A stable order keeps results (and the index file) the same between refreshes.
    files.sort();
    files
        .into_iter()
        .map(|path| SourceFile { root: 0, path })
        .collect()
}

/// `(name, detail, path, line)` of the project's module-level classes,
/// functions and constants: what the "Import" quick fix for an unresolved
/// reference offers from project modules (the list `search_ui::scan_symbols`
/// produces when there is no index).
pub fn project_symbol_list(index: &SymbolIndex) -> Vec<ProjectSymbol> {
    index
        .top_level_declarations(PROJECT_SYMBOL_KINDS)
        .into_iter()
        .map(|d| (d.name, d.detail, d.path, d.line as usize))
        .collect()
}

/// The kinds [`project_symbol_list`] lists.
const PROJECT_SYMBOL_KINDS: &[SymbolKind] = &[
    SymbolKind::Class,
    SymbolKind::Function,
    SymbolKind::Constant,
];

/// `1.2 MB`-style sizes.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["bytes", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1000.0 && unit + 1 < UNITS.len() {
        v /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} bytes")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

/// `5 min ago`-style ages of Unix times.
pub fn format_age(unix_secs: u64, now_secs: u64) -> String {
    if unix_secs == 0 {
        return "never".into();
    }
    let d = now_secs.saturating_sub(unix_secs);
    match d {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86_399 => format!("{} h ago", d / 3600),
        _ => format!("{} days ago", d / 86_400),
    }
}

/// `rustpy-python-3.12-stdlib.rpidx` in the home directory: where Export suggests writing a bundle.
pub fn default_bundle_path(python_version: &str, include_packages: bool) -> PathBuf {
    let short: String = python_version
        .split('.')
        .take(2)
        .collect::<Vec<_>>()
        .join(".");
    let what = if include_packages {
        "libraries"
    } else {
        "stdlib"
    };
    let name = if short.is_empty() {
        format!("rustpy-python-{what}.rpidx")
    } else {
        format!("rustpy-python-{short}-{what}.rpidx")
    };
    dirs::home_dir().unwrap_or_default().join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait(idx: &mut SymbolIndexes) {
        let start = Instant::now();
        while idx.busy() && start.elapsed() < Duration::from_secs(60) {
            idx.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
        idx.poll();
    }

    fn interpreter(sys_path: Vec<PathBuf>) -> Interpreter {
        Interpreter {
            path: PathBuf::from("/fake/bin/python3"),
            version: "3.12.3".into(),
            prefix: PathBuf::from("/fake"),
            sys_path,
            is_venv: false,
            platform: "linux".into(),
            kind: "system".into(),
        }
    }

    fn contains_score(name: &str, q: &str) -> Option<i32> {
        let (n, q) = (name.to_lowercase(), q.to_lowercase());
        if n == q {
            Some(100)
        } else {
            n.contains(&q).then_some(50)
        }
    }

    #[test]
    fn rebuilding_an_index_is_dumb_mode() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("main.py"), "class Main:\n    pass\n").unwrap();
        let mut idx = SymbolIndexes::new(IndexStore::new(dir.path().join("index")));
        idx.sync(Some(&proj), None);
        wait(&mut idx);
        assert!(!idx.is_dumb());
        idx.rebuild_project();
        assert!(idx.is_dumb(), "a rebuild shows in the status bar at once");
        assert!(idx.snapshot().project_pending);
        wait(&mut idx);
        assert!(!idx.is_dumb() && !idx.snapshot().project_pending);
    }

    #[test]
    fn indexes_follow_the_project_and_interpreter_and_persist() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("proj");
        std::fs::create_dir_all(proj.join("pkg")).unwrap();
        std::fs::write(proj.join("main.py"), "def main():\n    pass\n").unwrap();
        std::fs::write(
            proj.join("pkg/models.py"),
            "class Order:\n    def total(self):\n        return 0\n\nLIMIT = 3\nvalue = 1\n",
        )
        .unwrap();
        let site = dir.path().join("site-packages");
        std::fs::create_dir_all(site.join("lib")).unwrap();
        std::fs::write(site.join("lib/__init__.py"), "from .core import Engine\n").unwrap();
        std::fs::write(site.join("lib/core.py"), "class Engine:\n    pass\n").unwrap();
        let store = IndexStore::new(dir.path().join("index"));
        let mut idx = SymbolIndexes::new(store.clone());
        let interp = interpreter(vec![site.clone(), dir.path().join("missing")]);
        idx.sync(Some(&proj), Some(&interp));
        let first = idx.snapshot();
        assert!(first.library_pending, "the library is being indexed");
        assert!(first.project_pending, "the project is being indexed");
        wait(&mut idx);
        assert!(!idx.is_dumb(), "dumb mode ends with indexing");
        let project = idx.project().expect("project index");
        assert_eq!(project.file_count(), 2);
        // Module-level classes, functions and constants (importable), not methods or plain variables.
        let symbols: Vec<String> = project_symbol_list(&project)
            .into_iter()
            .map(|(n, ..)| n)
            .collect();
        assert_eq!(symbols, vec!["main", "Order", "LIMIT"]);
        let library = idx.library().expect("library index");
        let found =
            library.import_candidates("eng", &|n: &str| n.to_lowercase().contains("eng"), 10);
        assert_eq!(
            found
                .iter()
                .map(|c| (c.module.as_str(), c.name.as_str()))
                .collect::<Vec<_>>(),
            vec![("lib", "Engine")]
        );
        assert!(store.project_path(&proj).is_file() && store.library_path(&interp.path).is_file());
        assert_eq!(idx.listing().len(), 2);
        let snap = idx.snapshot();
        assert!(!snap.library_pending && snap.project.is_some() && snap.library.is_some());
        assert!(!snap.project_pending);
        assert_eq!(snap.project_files.as_ref().map(|f| f.len()), Some(2));
        // Following the same project and interpreter again changes nothing.
        let generation = idx.generation();
        idx.sync(Some(&proj), Some(&interp));
        assert!(!idx.busy() && idx.generation() == generation);
        // A new session loads both from disk, and an unchanged refresh keeps them.
        let mut again = SymbolIndexes::new(store.clone());
        again.sync(Some(&proj), Some(&interp));
        wait(&mut again);
        assert_eq!(again.last_stats().0.map(|s| s.parsed), Some(0));
        assert_eq!(
            again.last_stats().1.map(|s| (s.parsed, s.unchanged)),
            Some((0, 2))
        );
        assert!(again.library().is_some() && again.project().is_some());
        // Edits reported by the file watcher are picked up once they settle.
        std::fs::write(
            proj.join("main.py"),
            "def main():\n    pass\n\ndef helper():\n    pass\n",
        )
        .unwrap();
        again.files_changed();
        again.maintain();
        assert!(!again.busy(), "debounced");
        std::thread::sleep(PROJECT_DEBOUNCE + Duration::from_millis(50));
        again.maintain();
        assert!(again.busy());
        wait(&mut again);
        assert_eq!(again.last_stats().1.map(|s| s.parsed), Some(1));
        assert!(project_symbol_list(&again.project().unwrap())
            .iter()
            .any(|(n, ..)| n == "helper"));
        // Deleting the library index drops it until rebuilt.
        let lib_file = again.library_file().unwrap();
        again.delete(&lib_file).unwrap();
        assert!(again.library().is_none() && !lib_file.exists());
        again.refresh_library(Duration::ZERO);
        assert!(!again.busy(), "suppressed after delete");
        assert!(again.library_view().unwrap().suppressed);
        again.rebuild_library();
        wait(&mut again);
        assert!(again.library().is_some() && lib_file.is_file());
        // No interpreter: no library index.
        again.set_library_interpreter(None);
        assert!(again.library().is_none() && again.library_view().is_none());
    }

    #[test]
    fn installing_a_package_refreshes_the_library_index() {
        let dir = tempfile::tempdir().unwrap();
        let site = dir.path().join("site-packages");
        std::fs::create_dir_all(site.join("first")).unwrap();
        std::fs::write(site.join("first/__init__.py"), "def one():\n    pass\n").unwrap();
        let mut idx = SymbolIndexes::new(IndexStore::new(dir.path().join("index")));
        idx.set_library_interpreter(Some(&interpreter(vec![site.clone()])));
        wait(&mut idx);
        idx.maintain();
        assert!(!idx.busy(), "nothing changed");
        // `pip install second` adds a directory to site-packages.
        std::thread::sleep(Duration::from_millis(20));
        std::fs::create_dir_all(site.join("second")).unwrap();
        std::fs::write(site.join("second/__init__.py"), "class Two:\n    pass\n").unwrap();
        idx.roots_checked = None;
        idx.maintain();
        assert!(idx.busy(), "the changed directory is noticed");
        wait(&mut idx);
        assert_eq!(
            idx.snapshot().import_names("two", 10),
            vec![ImportableName {
                module: "second".into(),
                name: "Two".into(),
                kind: CompletionKind::Class
            }]
        );
    }

    #[test]
    fn snapshot_search_merges_project_and_library_symbols() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("proj");
        std::fs::create_dir_all(proj.join("src/app")).unwrap();
        std::fs::write(proj.join("src/app/__init__.py"), "").unwrap();
        std::fs::write(
            proj.join("src/app/engine.py"),
            "class Engine:\n    def start(self):\n        pass\n",
        )
        .unwrap();
        let site = dir.path().join("site-packages");
        std::fs::create_dir_all(site.join("motor")).unwrap();
        std::fs::write(site.join("motor/__init__.py"), "class Engine:\n    pass\n\nclass EngineError(Exception):\n    pass\n\ndef engine_version():\n    pass\n").unwrap();
        // An editable install of the project puts `src` on sys.path.
        let interp = interpreter(vec![site.clone(), proj.join("src")]);
        let mut idx = SymbolIndexes::new(IndexStore::new(dir.path().join("index")));
        idx.sync(Some(&proj), Some(&interp));
        wait(&mut idx);
        let snap = idx.snapshot();
        let names = |found: &[FoundSymbol]| {
            found
                .iter()
                .map(|f| {
                    format!(
                        "{}{}:{}",
                        if f.library { "lib " } else { "" },
                        f.hit.module,
                        f.hit.name
                    )
                })
                .collect::<Vec<_>>()
        };
        // Project only.
        assert_eq!(
            names(&snap.search("engine", true, false, 10, &contains_score)),
            vec!["src.app.engine:Engine"]
        );
        // With non-project items: the project's own class is not listed again from `src` on sys.path.
        let all = snap.search("engine", true, true, 10, &contains_score);
        assert_eq!(
            names(&all),
            vec![
                "src.app.engine:Engine",
                "lib motor:Engine",
                "lib motor:EngineError"
            ]
        );
        let symbols = snap.search("engine", false, true, 10, &contains_score);
        assert!(
            names(&symbols).contains(&"lib motor:engine_version".to_string()),
            "{:?}",
            names(&symbols)
        );
        assert_eq!(
            snap.search("engine", true, true, 2, &contains_score).len(),
            2
        );
        // Import candidates and exact modules for quick fixes come from the libraries.
        let names: Vec<String> = snap
            .import_names("engine", 10)
            .into_iter()
            .map(|c| format!("{}.{}", c.module, c.name))
            .collect();
        assert!(
            names.contains(&"motor.Engine".to_string())
                && names.contains(&"motor.engine_version".to_string()),
            "{names:?}"
        );
        assert_eq!(
            snap.import_modules("EngineError", 5),
            vec!["motor".to_string()]
        );
        assert!(IndexSnapshot::default()
            .search("engine", false, true, 10, &contains_score)
            .is_empty());
    }

    #[test]
    fn bundles_export_and_import() {
        let dir = tempfile::tempdir().unwrap();
        let stdlib = dir.path().join("lib/python3.12");
        std::fs::create_dir_all(stdlib.join("json")).unwrap();
        std::fs::create_dir_all(stdlib.join("encodings")).unwrap();
        std::fs::write(stdlib.join("os.py"), "def getcwd():\n    pass\n").unwrap();
        std::fs::write(stdlib.join("encodings/__init__.py"), "").unwrap();
        std::fs::write(stdlib.join("json/__init__.py"), "def loads(s):\n    pass\n").unwrap();
        let mut idx = SymbolIndexes::new(IndexStore::new(dir.path().join("index")));
        idx.set_library_interpreter(Some(&interpreter(vec![stdlib.clone()])));
        wait(&mut idx);
        let dest = dir.path().join("python-3.12-stdlib.rpidx");
        idx.export_bundle(dest.clone(), false);
        wait(&mut idx);
        assert!(dest.is_file(), "{:?}", idx.messages);
        assert!(
            idx.messages.last().unwrap().starts_with("Exported 3 files"),
            "{:?}",
            idx.messages
        );
        assert!(matches!(&idx.last_message, Some(Ok(m)) if m.starts_with("Exported")));
        // A fresh install imports it and builds without parsing.
        let mut fresh = SymbolIndexes::new(IndexStore::new(dir.path().join("index2")));
        fresh.export_bundle(dir.path().join("nothing.rpidx"), false);
        assert!(
            matches!(&fresh.last_message, Some(Err(_))),
            "nothing indexed yet"
        );
        fresh.import_bundle(dest);
        wait(&mut fresh);
        assert!(
            fresh
                .messages
                .last()
                .unwrap()
                .starts_with("Imported shared index"),
            "{:?}",
            fresh.messages
        );
        fresh.set_library_interpreter(Some(&interpreter(vec![stdlib])));
        wait(&mut fresh);
        assert_eq!(
            fresh.last_stats().0.map(|s| (s.parsed, s.from_shared)),
            Some((0, 3))
        );
        assert_eq!(
            fresh
                .listing()
                .iter()
                .filter(|s| s.meta.kind == IndexKind::Bundle)
                .count(),
            1
        );
        // A damaged file is refused.
        let junk = dir.path().join("junk.rpidx");
        std::fs::write(&junk, b"junk").unwrap();
        fresh.import_bundle(junk);
        wait(&mut fresh);
        assert!(
            matches!(&fresh.last_message, Some(Err(m)) if m.contains("not a RustPy index")),
            "{:?}",
            fresh.last_message
        );
    }

    /// Waits (at most 30 s) until `done` holds.
    fn wait_until(mut done: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while !done() {
            if start.elapsed() > Duration::from_secs(30) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }

    /// Files of a save that has not been put in place (or discarded) yet.
    fn staged_files(dir: &Path) -> usize {
        std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.file_name().to_string_lossy().contains(".rpidx.tmp-"))
                    .count()
            })
            .unwrap_or(0)
    }

    #[test]
    fn a_delete_while_a_refresh_saves_is_not_undone() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("main.py"), "def main():\n    pass\n").unwrap();
        let mut idx = SymbolIndexes::new(IndexStore::new(dir.path().join("index")));
        idx.set_project_root(Some(&proj));
        wait(&mut idx);
        let file = idx.project_file().unwrap();
        let saves = file.parent().unwrap().to_path_buf();
        assert!(file.is_file());
        // A refresh with something to save, held where it puts the new index
        // in place (the test holds the lock that Delete takes).
        std::fs::write(proj.join("other.py"), "class Other:\n    pass\n").unwrap();
        let save_lock = idx.save_lock.clone();
        let held = save_lock.lock();
        idx.refresh_project(Duration::ZERO);
        let progress = idx
            .project
            .job
            .as_ref()
            .expect("a refresh")
            .progress
            .clone();
        assert!(
            wait_until(|| staged_files(&saves) == 1),
            "the refresh wrote its index aside and waits"
        );
        // Delete in Settings lands now: it cancels the refresh and deletes
        // the file under the lock (held here, so done by hand).
        idx.project.cancel();
        idx.store.delete(&file).unwrap();
        drop(held);
        assert!(
            wait_until(|| staged_files(&saves) == 0),
            "the cancelled refresh dropped what it wrote"
        );
        assert!(progress.is_cancelled());
        assert!(!file.exists(), "the deleted index was written back");
        // `delete` itself cancels a running refresh before it deletes.
        idx.rebuild_project();
        wait(&mut idx);
        assert!(file.is_file());
        std::fs::write(proj.join("third.py"), "THIRD = 3\n").unwrap();
        idx.refresh_project(Duration::ZERO);
        let progress = idx
            .project
            .job
            .as_ref()
            .expect("a refresh")
            .progress
            .clone();
        idx.delete(&file).unwrap();
        assert!(progress.is_cancelled() && idx.project.job.is_none());
        assert!(wait_until(|| staged_files(&saves) == 0));
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !file.exists() && idx.project().is_none() && idx.project_view().unwrap().suppressed
        );
    }

    #[test]
    fn rebuilding_discards_published_and_queued_results() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let path = project.join("models.py");
        std::fs::write(&path, "class OldModel: pass\n").unwrap();
        let mut idx = SymbolIndexes::new(IndexStore::new(dir.path().join("index")));
        idx.set_project_root(Some(&project));
        wait(&mut idx);
        let old = idx.project().unwrap();
        let (tx, rx) = crossbeam_channel::unbounded();
        let progress = Arc::new(Progress::default());
        tx.send(JobMsg::Done(Ok((
            Some(Loaded {
                data: Some(Arc::new(ProjectData::of(&old))),
                index: old,
            }),
            RefreshStats::default(),
            None,
        ))))
        .unwrap();
        idx.project.job = Some(Job {
            progress: progress.clone(),
            rx,
            started: Instant::now(),
            rebuild: false,
        });
        idx.project.queued = Some(false);
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, "class NewModel: pass\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        let generation = idx.snapshot().generation;
        idx.rebuild_project();
        assert!(progress.is_cancelled());
        assert!(idx.project.queued.is_none());
        assert!(idx.snapshot().project.is_none());
        assert!(idx.snapshot().generation > generation);
        wait(&mut idx);
        assert!(idx
            .snapshot()
            .search("OldModel", true, false, 10, &contains_score)
            .is_empty());
        assert_eq!(
            idx.snapshot()
                .search("NewModel", true, false, 10, &contains_score)
                .len(),
            1
        );
        let mut restarted = SymbolIndexes::new(idx.store.clone());
        restarted.set_project_root(Some(&project));
        wait(&mut restarted);
        assert_eq!(
            restarted
                .snapshot()
                .search("NewModel", true, false, 10, &contains_score)
                .len(),
            1
        );
    }

    #[test]
    fn rebuilding_removes_the_old_disk_artifact_before_worker_publication() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(project.join("old.py"), "class Old: pass\n").unwrap();
        let mut idx = SymbolIndexes::new(IndexStore::new(dir.path().join("index")));
        idx.set_project_root(Some(&project));
        wait(&mut idx);
        let path = idx.project_file().unwrap();
        std::fs::write(project.join("old.py"), "class New: pass\n").unwrap();
        idx.rebuild_project();
        // Restart before completion sees no artifact, or the newly rebuilt
        // one if the worker has already finished, never the invalidated one.
        match IndexStore::load(&path) {
            Err(IndexError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
            Ok(index) => assert!(index
                .search("Old", &SearchOptions::default(), &contains_score)
                .is_empty()),
            other => panic!("unexpected load: {other:?}"),
        }
        wait(&mut idx);
        assert!(path.is_file());
        // A directory occupying the generated cache path cannot be removed
        // as a file: that failure is immediately visible to the UI.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        idx.rebuild_project();
        assert!(idx
            .errors()
            .1
            .is_some_and(|e| e.starts_with("Cannot invalidate")));
        assert!(matches!(idx.last_message, Some(Err(_))));
        wait(&mut idx);
        assert!(idx.errors().1.is_some_and(|e| e.starts_with("Cannot save")));
    }

    #[test]
    fn save_failures_are_visible_without_losing_fresh_memory_symbols() {
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, "not a directory").unwrap();
        std::fs::write(dir.path().join("fresh.py"), "class Fresh: pass\n").unwrap();
        let mut idx = SymbolIndexes::new(IndexStore::new(&blocked));
        idx.set_project_root(Some(dir.path()));
        wait(&mut idx);
        assert!(idx.errors().1.is_some_and(|e| e.starts_with("Cannot save")));
        assert_eq!(
            idx.snapshot()
                .search("Fresh", true, false, 10, &contains_score)
                .len(),
            1
        );
    }

    #[test]
    fn bundles_load_outside_the_lock() {
        let cache = Mutex::new(BundleCache::default());
        let bundle = Arc::new(SymbolIndex::empty(IndexMeta::default()));
        let got = BundleCache::get_with(&cache, || {
            // Decoding takes seconds for large bundles: meanwhile the UI
            // thread finishing an import or a delete takes the lock at once.
            assert!(
                cache.try_lock().is_some(),
                "the lock is free while bundles load"
            );
            BundleCache::invalidate(&cache);
            vec![bundle.clone()]
        });
        assert_eq!(got.len(), 1, "the job uses what it loaded");
        assert!(
            cache.lock().loaded.is_none(),
            "a load overtaken by an import or delete is not kept"
        );
        // A load nothing overtook is kept: later jobs do not decode again.
        let first = BundleCache::get_with(&cache, || vec![bundle.clone()]);
        let again = BundleCache::get_with(&cache, || panic!("loaded twice"));
        assert!(Arc::ptr_eq(&first[0], &again[0]));
    }

    #[test]
    fn exported_bundles_leave_out_editable_installs() {
        let dir = tempfile::tempdir().unwrap();
        let stdlib = dir.path().join("lib/python3.12");
        std::fs::create_dir_all(stdlib.join("encodings")).unwrap();
        std::fs::write(stdlib.join("os.py"), "def getcwd():\n    pass\n").unwrap();
        std::fs::write(stdlib.join("encodings/__init__.py"), "").unwrap();
        let site = dir.path().join("site-packages");
        std::fs::create_dir_all(site.join("lib")).unwrap();
        std::fs::write(site.join("lib/__init__.py"), "X = 1\n").unwrap();
        std::fs::write(site.join("__editable___acme_billing_0_1_finder.py"), "MAPPING: dict[str, str] = {'acme_billing': '/home/me/secretproj/src/acme_billing'}\n\ndef install():\n    pass\n").unwrap();
        // `pip install -e secretproj` put the project's sources on sys.path.
        let src = dir.path().join("secretproj/src");
        std::fs::create_dir_all(src.join("acme_billing")).unwrap();
        std::fs::write(
            src.join("acme_billing/__init__.py"),
            "def connect(host='db.internal.acme.example', password='hunter2'):\n    pass\n",
        )
        .unwrap();
        let mut idx = SymbolIndexes::new(IndexStore::new(dir.path().join("index")));
        idx.set_library_interpreter(Some(&interpreter(vec![stdlib, site, src])));
        wait(&mut idx);
        assert_eq!(
            idx.snapshot().import_modules("connect", 5),
            vec!["acme_billing".to_string()],
            "found in the IDE"
        );
        let dest = dir.path().join("libraries.rpidx");
        idx.export_bundle(dest.clone(), true);
        wait(&mut idx);
        assert!(
            matches!(&idx.last_message, Some(Ok(m)) if m.starts_with("Exported 3 files")),
            "{:?}",
            idx.last_message
        );
        let bundle = IndexStore::load(&dest).unwrap();
        let modules: Vec<String> = bundle.files().map(|(_, m, _)| m.to_string()).collect();
        assert_eq!(modules, vec!["encodings", "os", "lib"]);
        assert!(
            bundle.exact_import_candidates("connect", 5).is_empty()
                && bundle.exact_import_candidates("install", 5).is_empty()
        );
        assert!(
            bundle
                .roots()
                .iter()
                .all(|r| r.path.to_string_lossy().starts_with('<')),
            "{:?}",
            bundle.roots()
        );
        let header = std::fs::read(&dest).unwrap();
        assert!(!String::from_utf8_lossy(&header).contains("secretproj"));
    }

    #[test]
    fn excluded_folders_are_not_indexed() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("proj");
        std::fs::create_dir_all(proj.join("gen")).unwrap();
        std::fs::write(proj.join("main.py"), "def main():\n    pass\n").unwrap();
        std::fs::write(proj.join("gen/out.py"), "def generated():\n    pass\n").unwrap();
        let excluded = [proj.join("gen")];
        let mut idx = SymbolIndexes::new(IndexStore::new(dir.path().join("index")));
        idx.set_project_excluded(excluded.iter());
        idx.sync(Some(&proj), None);
        wait(&mut idx);
        let names = |idx: &SymbolIndexes| {
            project_symbol_list(&idx.project().unwrap())
                .into_iter()
                .map(|(n, ..)| n)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&idx), vec!["main"]);
        // Marked as not excluded in the Project view: indexed again.
        idx.set_project_excluded([].iter());
        idx.maintain();
        wait(&mut idx);
        assert_eq!(names(&idx), vec!["generated", "main"]);
        // And excluded again: dropped from the index.
        idx.set_project_excluded(excluded.iter());
        idx.maintain();
        wait(&mut idx);
        assert_eq!(names(&idx), vec!["main"]);
        assert_eq!(idx.snapshot().project_files.map(|f| f.len()), Some(1));
    }

    #[test]
    fn project_data_comes_from_the_job_not_the_ui_thread() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("models.py"),
            "LIMIT = 3\n\nclass Order(Base):\n    def total(self):\n        pass\n\nvalue = 1\n",
        )
        .unwrap();
        let store = IndexStore::new(dir.path().join("index"));
        let mut idx = SymbolIndexes::new(store.clone());
        idx.sync(Some(&proj), None);
        wait(&mut idx);
        // The job sent the index together with what consumers derive from it.
        let data = idx.project.data.clone().expect("derived by the job");
        assert_eq!(data.symbols.len(), 2);
        // Kept as ids: the strings are read from the index on demand, and
        // match the list built from the index directly.
        let symbols: Vec<ProjectSymbol> = idx.snapshot().project_symbol_list().unwrap().collect();
        assert_eq!(symbols, project_symbol_list(&idx.project().unwrap()));
        let symbols: Vec<(String, String)> = symbols.into_iter().map(|(n, d, ..)| (n, d)).collect();
        assert_eq!(
            symbols,
            vec![
                ("LIMIT".to_string(), String::new()),
                ("Order".to_string(), "(Base)".to_string())
            ]
        );
        // Snapshots share it: nothing is rebuilt when they are taken, also
        // after other changes (here the interpreter) bump the generation.
        let first = idx.snapshot();
        idx.set_library_interpreter(Some(&interpreter(Vec::new())));
        wait(&mut idx);
        let second = idx.snapshot();
        assert_ne!(first.generation, second.generation);
        for snap in [&first, &second] {
            assert!(Arc::ptr_eq(
                snap.project_symbols.as_ref().unwrap(),
                &data.symbols
            ));
            assert!(Arc::ptr_eq(
                snap.project_files.as_ref().unwrap(),
                &data.files
            ));
        }
        // An index loaded from disk comes with its data too (before its refresh ends).
        let mut again = SymbolIndexes::new(store);
        again.sync(Some(&proj), None);
        assert!(wait_until(|| {
            again.poll();
            again.project().is_some()
        }));
        assert_eq!(again.snapshot().project_symbols.map(|s| s.len()), Some(2));
    }

    /// Records the thread that drops it.
    struct DropProbe(Arc<Mutex<Option<String>>>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            *self.0.lock() = Some(std::thread::current().name().unwrap_or("?").to_string());
        }
    }

    #[test]
    fn retired_data_is_freed_off_the_calling_thread_once_nothing_holds_it() {
        let dropped_on = Arc::new(Mutex::new(None));
        let mut retired = Retired::default();
        let probe = Arc::new(DropProbe(dropped_on.clone()));
        // A snapshot elsewhere still holds it: kept.
        let held = probe.clone();
        retired.0.push(probe);
        retired.collect();
        assert_eq!(retired.0.len(), 1);
        drop(held);
        assert!(
            dropped_on.lock().is_none(),
            "releasing the last other holder does not free it there"
        );
        retired.collect();
        assert!(retired.0.is_empty());
        assert!(wait_until(|| dropped_on.lock().is_some()));
        assert_eq!(dropped_on.lock().as_deref(), Some("rustpy-index-free"));
    }

    #[test]
    fn a_refreshed_project_index_frees_the_old_one_in_the_background() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("main.py"), "def main():\n    pass\n").unwrap();
        let mut idx = SymbolIndexes::new(IndexStore::new(dir.path().join("index")));
        idx.sync(Some(&proj), None);
        wait(&mut idx);
        // Search Everywhere and the analysis inputs hold snapshots.
        let held = idx.snapshot();
        let old_index = Arc::downgrade(held.project.as_ref().unwrap());
        let old_files = Arc::downgrade(held.project_files.as_ref().unwrap());
        std::fs::write(proj.join("other.py"), "class Other:\n    pass\n").unwrap();
        idx.refresh_project(Duration::ZERO);
        wait(&mut idx);
        assert!(
            idx.project()
                .unwrap()
                .symbols_of(&proj.join("other.py"))
                .len()
                == 1,
            "refreshed"
        );
        let fresh = idx.snapshot();
        assert!(!Arc::ptr_eq(
            fresh.project.as_ref().unwrap(),
            held.project.as_ref().unwrap()
        ));
        // The UI thread lets go of the last snapshot with the old index: it
        // is not freed there, but by a later poll in the background.
        drop(held);
        assert!(old_index.upgrade().is_some() && old_files.upgrade().is_some());
        idx.poll();
        assert!(
            wait_until(|| old_index.upgrade().is_none() && old_files.upgrade().is_none()),
            "freed"
        );
        assert!(idx.retired.0.is_empty());
        // Closing the project: the same (each frame polls, then takes a snapshot).
        let current = Arc::downgrade(&idx.project().unwrap());
        idx.sync(None, None);
        idx.poll();
        assert!(idx.snapshot().project.is_none());
        drop(fresh);
        assert!(current.upgrade().is_some());
        idx.poll();
        assert!(wait_until(|| current.upgrade().is_none()));
    }

    #[test]
    fn formatting() {
        assert_eq!(format_size(512), "512 bytes");
        assert_eq!(format_size(6_400_000), "6.4 MB");
        assert_eq!(format_age(0, 100), "never");
        assert_eq!(format_age(1000, 1030), "just now");
        assert_eq!(format_age(1000, 1000 + 300), "5 min ago");
        assert_eq!(format_age(1000, 1000 + 7200), "2 h ago");
        assert_eq!(format_age(1000, 1000 + 3 * 86_400), "3 days ago");
        assert!(default_bundle_path("3.12.3", false).ends_with("rustpy-python-3.12-stdlib.rpidx"));
        assert!(default_bundle_path("", true).ends_with("rustpy-python-libraries.rpidx"));
        assert_eq!(
            completion_kind(SymbolKind::Constant),
            CompletionKind::Constant
        );
    }
}
