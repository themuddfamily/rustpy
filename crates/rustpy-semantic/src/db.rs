//! The analysis database: files, parses and scope models shared between the
//! editor and language services (roadmap §2.2 "Project-wide index").
//!
//! Files come from two places: open editors push their text with
//! [`Analysis::set_file_text`], everything else is read from disk on demand.
//! Disk copies retain the VFS byte revision they were read with. Content
//! checks detect edits even when size and modification time are preserved.
//! A file changed outside the editor is re-read under its stable identity.
//!
//! Disk copies are a bounded cache ([`CacheLimits`]): beyond the limits the
//! least recently used ones are dropped. Their ids stay valid — the next
//! lookup by id, path or module name re-reads the file under the same id —
//! so a project-wide search over thousands of files does not keep every
//! parse tree in memory. The database is shared between the UI thread and
//! background analysis; concurrent loads of one file converge on one copy.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Instant, SystemTime};

use parking_lot::RwLock;
use rustc_hash::{FxHashMap, FxHashSet};
use rustpy_core::vfs::{normalize_path, ContentRevision, FileKey, ReadLimits, Vfs};
use rustpy_core::FileId;
use rustpy_syntax::Parse;

use crate::module::{ModuleResolver, ModuleSource};
use crate::scope::{self, ScopeModel};

/// Size and modification time of a file as it was read from disk; a
/// different stamp means the file changed since.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskStamp {
    pub len: u64,
    pub mtime: Option<SystemTime>,
}

impl DiskStamp {
    /// The current stamp of the regular file at `path` (`None` when missing).
    pub fn of(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        meta.is_file().then(|| Self {
            len: meta.len(),
            mtime: meta.modified().ok(),
        })
    }
}

/// Module lookups made during inference trust a disk file for this long
/// before comparing its stamp again (explicit path lookups always compare).
const RECHECK_MS: u64 = 1000;
/// Modification times this recent may hide a same-size write made within the
/// file system's timestamp granularity; such stamps are always confirmed by content.
const RACY_STAMP: std::time::Duration = std::time::Duration::from_secs(3);

/// Import edges are keyed by immutable source versions and resolution generation.
#[derive(Clone)]
struct ImportGraph {
    generation: u64,
    sources: Vec<std::sync::Weak<FileInfo>>,
    importers: FxHashMap<FileId, Vec<std::sync::Weak<FileInfo>>>,
}

/// Everything the analyzer knows about one file.
#[derive(Debug)]
pub struct FileInfo {
    /// Originating module for SDK-specific library imports. Navigation keeps
    /// using `path`; this identity only selects the resolver environment.
    pub module_origin: Option<PathBuf>,
    pub id: FileId,
    /// Real path, or `<typeshed>/<module>.pyi` for bundled stubs.
    pub path: PathBuf,
    pub module_name: Option<String>,
    pub text: Arc<str>,
    pub parse: Parse,
    pub scopes: ScopeModel,
    pub is_stub: bool,
    /// Whether the text comes from an open editor (vs. loaded from disk).
    pub is_open: bool,
    /// For stubs: the interpreter version/platform their `if sys.version_info`
    /// / `sys.platform` branches are evaluated for (dead branches bind nothing).
    pub static_env: Option<crate::static_cond::StaticEnv>,
    /// Stamp of the disk contents the text was read from (`None` for editor
    /// text and for virtual files such as bundled stubs).
    pub disk_stamp: Option<DiskStamp>,
    pub disk_revision: Option<ContentRevision>,
    /// When the stamp was last compared with the disk (database clock, ms).
    checked_ms: AtomicU64,
    /// A full content comparison matched while the disk stamp was already older
    /// than the racy window: later comparisons may trust an unchanged stamp.
    stable_stamp: AtomicBool,
    /// Flow analysis of this version's scopes, computed on demand.
    pub(crate) flow: crate::flow::FlowCache,
    /// Names of this version's `@deprecated` definitions, computed on demand
    /// (`PyDeprecation` consults every loaded file on each inspection).
    pub(crate) deprecated_names: std::sync::OnceLock<Vec<smol_str::SmolStr>>,
    /// Member name → first offset from which an earlier write may have
    /// replaced that namespace member (see `namespace_member_retired`).
    pub(crate) member_retirements:
        parking_lot::Mutex<FxHashMap<smol_str::SmolStr, Option<rustpy_core::TextSize>>>,
    /// `(member, excepted declaration)` → `match_support::member_source_stable_except`.
    pub(crate) member_stability:
        parking_lot::Mutex<FxHashMap<(smol_str::SmolStr, Option<rustpy_core::TextRange>), bool>>,
    /// Content-derived declarations, shared by hierarchy and navigation queries.
    /// Lives on the immutable source version, so edits cannot reuse stale ranges.
    class_declarations: std::sync::OnceLock<Vec<crate::types::ClassRef>>,
    /// When a disk copy was last looked up (database clock, ms): the cache
    /// drops the least recently used ones first.
    last_used: AtomicU64,
}

impl FileInfo {
    fn new(
        id: FileId,
        path: PathBuf,
        module_name: Option<String>,
        text: String,
        is_stub: bool,
        is_open: bool,
        env: &crate::static_cond::StaticEnv,
    ) -> Self {
        let parse = rustpy_syntax::parse(&text);
        Self::with_parse(id, path, module_name, text, parse, is_stub, is_open, env)
    }

    /// Like [`FileInfo::new`] with an existing parse of `text`.
    #[allow(clippy::too_many_arguments)]
    fn with_parse(
        id: FileId,
        path: PathBuf,
        module_name: Option<String>,
        text: String,
        parse: Parse,
        is_stub: bool,
        is_open: bool,
        env: &crate::static_cond::StaticEnv,
    ) -> Self {
        let static_env = is_stub.then(|| env.clone());
        let scopes = scope::build_with(&parse, static_env.as_ref());
        Self {
            module_origin: None,
            id,
            path,
            module_name,
            text: Arc::from(text),
            parse,
            scopes,
            is_stub,
            is_open,
            static_env,
            disk_stamp: None,
            disk_revision: None,
            checked_ms: AtomicU64::new(0),
            stable_stamp: AtomicBool::new(false),
            last_used: AtomicU64::new(0),
            flow: Default::default(),
            deprecated_names: std::sync::OnceLock::new(),
            member_retirements: Default::default(),
            member_stability: Default::default(),
            class_declarations: std::sync::OnceLock::new(),
        }
    }
    /// Class declarations of this exact source version, including nested classes.
    pub fn class_declarations(&self) -> &[crate::types::ClassRef] {
        self.class_declarations.get_or_init(|| {
            self.scopes
                .bindings
                .iter()
                .filter(|binding| matches!(binding.kind, scope::BindingKind::Class))
                .map(|binding| crate::types::ClassRef {
                    file: self.id,
                    range: binding.node_range,
                    name: binding.name.clone(),
                })
                .collect()
        })
    }

    /// An unpublished view of candidate text with this file's semantic context.
    /// Parsing and version-dependent caches are fresh; no live database is changed.
    pub fn with_candidate_text(&self, text: String) -> Self {
        let parse = rustpy_syntax::parse(&text);
        let scopes = scope::build_with(&parse, self.static_env.as_ref());
        Self {
            module_origin: self.module_origin.clone(),
            id: self.id,
            path: self.path.clone(),
            module_name: self.module_name.clone(),
            text: Arc::from(text),
            parse,
            scopes,
            is_stub: self.is_stub,
            is_open: self.is_open,
            static_env: self.static_env.clone(),
            disk_stamp: None,
            disk_revision: None,
            checked_ms: AtomicU64::new(0),
            stable_stamp: AtomicBool::new(false),
            last_used: AtomicU64::new(0),
            flow: Default::default(),
            deprecated_names: std::sync::OnceLock::new(),
            member_retirements: Default::default(),
            member_stability: Default::default(),
            class_declarations: std::sync::OnceLock::new(),
        }
    }

    pub fn is_package_init(&self) -> bool {
        self.path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s == "__init__")
    }
}

/// Path, disk stamp and content revision of a dropped disk copy, and the
/// copy itself while anything still holds it.
type EvictedFile = (
    PathBuf,
    Option<DiskStamp>,
    Option<ContentRevision>,
    Weak<FileInfo>,
);

#[derive(Default)]
struct Files {
    by_id: FxHashMap<FileId, Arc<FileInfo>>,
    by_path: FxHashMap<PathBuf, FileId>,
    /// Module name -> file. `None` caches a failed lookup until the next
    /// structural change (a file appearing or disappearing).
    by_module: FxHashMap<String, Option<FileId>>,
    /// Disk copies dropped to bound the cache: their ids (still in
    /// `by_path` / `by_module`) are re-read on the next lookup, unless an
    /// analysis still holds the dropped copy (the weak reference).
    evicted: FxHashMap<FileId, EvictedFile>,
    /// Number and total text size of the evictable entries of `by_id` (disk
    /// copies not owned by an editor), kept up to date by `put` / `take`.
    disk_count: usize,
    disk_bytes: usize,
}

/// Whether the cache may drop `f`: a copy read from disk that no editor owns
/// (open files and bundled stubs stay).
fn evictable(f: &FileInfo) -> bool {
    f.disk_stamp.is_some() && !f.is_open
}

impl Files {
    /// Inserts or replaces `id` (every `by_id` change goes through `put` / `take`).
    fn put(&mut self, id: FileId, info: Arc<FileInfo>) {
        if evictable(&info) {
            self.disk_count += 1;
            self.disk_bytes += info.text.len();
        }
        if let Some(old) = self.by_id.insert(id, info) {
            self.forget_size(&old);
        }
    }

    fn take(&mut self, id: &FileId) -> Option<Arc<FileInfo>> {
        let old = self.by_id.remove(id)?;
        self.forget_size(&old);
        Some(old)
    }

    fn forget_size(&mut self, old: &FileInfo) {
        if evictable(old) {
            self.disk_count -= 1;
            self.disk_bytes -= old.text.len();
        }
    }

    /// Drops the cached copy of `path` unless an editor owns it (module
    /// lookups pointing at it are left to the caller). Returns the copy, to
    /// be freed once the lock is released (`None` for an evicted copy).
    fn drop_disk_entry(&mut self, path: &Path) -> Option<(FileId, Option<Arc<FileInfo>>)> {
        let id = self.by_path.get(path).copied()?;
        if self.by_id.get(&id).is_some_and(|f| f.is_open) {
            return None;
        }
        self.by_path.remove(path);
        self.evicted.remove(&id);
        Some((id, self.take(&id)))
    }

    /// Drops least recently used disk copies until at most 90% of `limits`
    /// remain (so the next load does not evict again). Open files and
    /// bundled stubs are never dropped. Cheap while under the limits.
    fn enforce(&mut self, limits: CacheLimits) {
        if self.disk_count <= limits.max_files && self.disk_bytes <= limits.max_bytes {
            return;
        }
        let mut disk: Vec<(u64, FileId)> = self
            .by_id
            .values()
            .filter(|f| evictable(f))
            .map(|f| (f.last_used.load(Ordering::Relaxed), f.id))
            .collect();
        disk.sort_unstable();
        let (target_count, target_bytes) = (limits.max_files * 9 / 10, limits.max_bytes / 10 * 9);
        for (_, id) in disk {
            if self.disk_count <= target_count && self.disk_bytes <= target_bytes {
                break;
            }
            if let Some(f) = self.take(&id) {
                self.evicted.insert(
                    id,
                    (
                        f.path.clone(),
                        f.disk_stamp,
                        f.disk_revision,
                        Arc::downgrade(&f),
                    ),
                );
            }
        }
    }
}

/// Change counters behind [`Analysis::revision_excluding`].
#[derive(Default)]
struct Revisions {
    /// Counts every change.
    last: u64,
    /// Who made the latest change: an editor's file, or `None` for changes
    /// of the world (disk files, module lookups, run-time types).
    last_changer: Option<PathBuf>,
    /// `last` before `last_changer` started its current run of changes.
    before_last_changer: u64,
}

impl Revisions {
    fn changed_by(&mut self, changer: Option<&Path>) {
        if self.last_changer.as_deref() != changer {
            self.before_last_changer = self.last;
            self.last_changer = changer.map(Path::to_path_buf);
        }
        self.last += 1;
    }

    fn excluding(&self, path: &Path) -> u64 {
        if self.last_changer.as_deref() == Some(path) {
            self.before_last_changer
        } else {
            self.last
        }
    }
}

/// Bounds of the cache of files read from disk (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheLimits {
    pub max_files: usize,
    /// Total size of their texts (parse trees and scopes take a multiple).
    pub max_bytes: usize,
}

impl Default for CacheLimits {
    fn default() -> Self {
        Self {
            max_files: 5000,
            max_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Immutable source/resolution world for one background computation. Missing
/// dependencies are tracked so callers cannot present incomplete counts as zero.
pub struct AnalysisSnapshot {
    analysis: Analysis,
    pub revision: u64,
}
impl AnalysisSnapshot {
    pub fn analysis(&self) -> &Analysis {
        &self.analysis
    }
    pub fn complete(&self) -> bool {
        !self.analysis.missing_dependency.load(Ordering::Acquire)
            && self.analysis.resolver.snapshot_complete()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotError {
    Cancelled,
    SourceUnavailable,
    BudgetExceeded,
    Changed,
}

/// Thread-safe analysis world.
/// See [`Analysis::batch_reads`].
pub struct BatchReads<'a>(&'a Analysis);
impl Drop for BatchReads<'_> {
    fn drop(&mut self) {
        self.0.batch_reads.fetch_sub(1, Ordering::Relaxed);
    }
}

pub struct Analysis {
    pub resolver: ModuleResolver,
    files: RwLock<Files>,
    pub vfs: Arc<Vfs>,
    runtime_types: RwLock<Arc<crate::runtime_types::RuntimeTypes>>,
    native_state: RwLock<Arc<crate::native_state::NativeState>>,
    runtime_docs: RwLock<Arc<crate::runtime_docs::RuntimeDocs>>,
    analyze_doc_comments: AtomicBool,
    /// Live [`BatchReads`] guards.
    batch_reads: AtomicUsize,
    clock: Instant,
    max_files: AtomicUsize,
    max_bytes: AtomicUsize,
    max_file_bytes: AtomicUsize,
    revisions: parking_lot::Mutex<Revisions>,
    frozen: bool,
    missing_dependency: AtomicBool,
    snapshot_next_id: AtomicU32,
    import_graph: RwLock<Option<Arc<ImportGraph>>>,
    pub(crate) hierarchy_index: RwLock<Option<Arc<crate::hierarchy::HierarchyIndex>>>,
    pub(crate) persisted_bases: RwLock<Option<Arc<crate::hierarchy::PersistedBases>>>,
    /// Framework model discovery over project sources, per source generation.
    pub(crate) framework_models: parking_lot::Mutex<crate::frameworks::ModelCache>,
    /// Inferred library function return types shared by every inference session.
    pub(crate) library_returns: parking_lot::Mutex<crate::infer::LibraryReturns>,
}

impl Analysis {
    pub fn new(resolver: ModuleResolver) -> Self {
        let limits = CacheLimits::default();
        Self {
            resolver,
            files: RwLock::new(Files::default()),
            vfs: Arc::new(Vfs::new()),
            runtime_types: RwLock::new(Arc::default()),
            native_state: RwLock::new(Arc::default()),
            clock: Instant::now(),
            max_files: AtomicUsize::new(limits.max_files),
            max_bytes: AtomicUsize::new(limits.max_bytes),
            max_file_bytes: AtomicUsize::new(ReadLimits::default().max_file_bytes),
            revisions: parking_lot::Mutex::new(Revisions::default()),
            frozen: false,
            missing_dependency: AtomicBool::new(false),
            snapshot_next_id: AtomicU32::new(0),
            hierarchy_index: RwLock::new(None),
            persisted_bases: RwLock::new(None),
            framework_models: parking_lot::Mutex::new(Default::default()),
            library_returns: parking_lot::Mutex::new(Default::default()),
            import_graph: RwLock::new(None),
            runtime_docs: RwLock::new(Arc::default()),
            analyze_doc_comments: AtomicBool::new(true),
            batch_reads: AtomicUsize::new(0),
        }
    }

    /// While the returned guard lives, explicit accesses of a disk file
    /// ([`Analysis::file`], [`Analysis::file_for_path`]) trust a copy whose
    /// content was verified within the last recheck interval, as module
    /// lookups always do, instead of reading and hashing the whole file again.
    /// Batch passes (project inspection) query the same files thousands of
    /// times; re-reading each time made them quadratic in the file size.
    pub fn batch_reads(&self) -> BatchReads<'_> {
        self.batch_reads.fetch_add(1, Ordering::Relaxed);
        BatchReads(self)
    }

    /// Configure explicit user stub roots before sharing this database. Resolver
    /// resolution and frozen catalogues retain the same ordered roots.
    pub fn with_stub_roots(mut self, roots: impl IntoIterator<Item = PathBuf>) -> Self {
        self.resolver = self.resolver.with_stub_roots(roots);
        self.files.write().by_module.clear();
        self.world_changed();
        self
    }

    pub(crate) fn source_generation(&self) -> u64 {
        self.revisions.lock().last
    }

    /// Load a bounded, complete index catalogue on a background thread, then
    /// freeze its source and resolution answers. Cached dependencies are retained.
    /// The caller must also bind/recheck its project/index content generation.
    pub fn snapshot(
        &self,
        paths: &[PathBuf],
        token: &rustpy_core::CancellationToken,
    ) -> Result<AnalysisSnapshot, SnapshotError> {
        if paths.len() > 10_000 {
            return Err(SnapshotError::BudgetExceeded);
        }
        let revision = self.revisions.lock().last;
        let mut captured = FxHashMap::default();
        let mut aliases = Vec::new();
        let mut bytes = 0usize;
        for path in paths {
            token.check().map_err(|_| SnapshotError::Cancelled)?;
            let file = self
                .file_for_path(path)
                .ok_or(SnapshotError::SourceUnavailable)?;
            if file.text.len() > 1024 * 1024 {
                return Err(SnapshotError::BudgetExceeded);
            }
            if !captured.contains_key(&file.id) {
                bytes = bytes.saturating_add(file.text.len());
            }
            if bytes > 64 * 1024 * 1024 {
                return Err(SnapshotError::BudgetExceeded);
            }
            aliases.push((path.clone(), file.id));
            captured.insert(file.id, file);
        }
        for file in self.all_files() {
            token.check().map_err(|_| SnapshotError::Cancelled)?;
            captured.entry(file.id).or_insert(file);
        }
        // Contextual library views keep real navigation paths but have separate
        // virtual cache keys; retain those aliases as well as the source keys.
        aliases.extend(
            self.files
                .read()
                .by_path
                .iter()
                .filter(|(_, id)| captured.contains_key(id))
                .map(|(path, id)| (path.clone(), *id)),
        );
        if captured.values().map(|f| f.text.len()).sum::<usize>() > 64 * 1024 * 1024 {
            return Err(SnapshotError::BudgetExceeded);
        }
        let resolver = self
            .resolver
            .frozen_catalogue(
                captured
                    .values()
                    .filter_map(|f| f.module_name.clone().map(|name| (name, f.path.clone()))),
                token,
            )
            .map_err(|_| SnapshotError::Cancelled)?;
        let mut frozen = Self::new(resolver);
        let next_id = captured
            .keys()
            .map(|id| id.0)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(SnapshotError::BudgetExceeded)?;
        frozen.snapshot_next_id.store(next_id, Ordering::Relaxed);
        *frozen.runtime_types.write() = self.runtime_types();
        *frozen.native_state.write() = self.native_state.read().clone();
        *frozen.runtime_docs.write() = self.runtime_docs();
        *frozen.persisted_bases.write() = self.persisted_bases.read().clone();
        frozen
            .analyze_doc_comments
            .store(self.analyze_doc_comments(), Ordering::Relaxed);
        {
            let mut files = frozen.files.write();
            for file in captured.into_values() {
                files.by_path.insert(file.path.clone(), file.id);
                files.put(file.id, file);
            }
            for (path, id) in aliases {
                files.by_path.insert(path, id);
            }
            files.by_module = self
                .files
                .read()
                .by_module
                .iter()
                .filter(|(_, id)| id.is_none_or(|id| files.by_id.contains_key(&id)))
                .map(|(name, id)| (name.clone(), *id))
                .collect();
        }
        token.check().map_err(|_| SnapshotError::Cancelled)?;
        if self.revisions.lock().last != revision {
            return Err(SnapshotError::Changed);
        }
        frozen.frozen = true;
        Ok(AnalysisSnapshot {
            analysis: frozen,
            revision,
        })
    }

    /// A counter that changes whenever something other than `path`'s own
    /// editor text changes (another editor's text, files on disk, module
    /// lookups, run-time types). Results computed for `path` stay valid
    /// while its text and this revision are unchanged, so caches key them by
    /// both. It may change more often than strictly needed, never less.
    pub fn revision_excluding(&self, path: &Path) -> u64 {
        self.revisions.lock().excluding(&normalize_path(path))
    }

    fn world_changed(&self) {
        self.revisions.lock().changed_by(None);
        self.library_returns.lock().clear();
    }

    /// A world change confined to `paths`: shared library inference survives
    /// unless one of them is a library source.
    fn world_changed_at<'p>(&self, paths: impl IntoIterator<Item = &'p Path>) {
        self.revisions.lock().changed_by(None);
        if paths
            .into_iter()
            .any(|path| crate::infer::library_path(self, path))
        {
            self.library_returns.lock().clear();
        }
    }

    /// `(hits, stores, retained entries)` of the shared library return-type cache.
    pub fn library_return_cache_stats(&self) -> (u64, u64, usize) {
        self.library_returns.lock().stats()
    }

    /// Whether structured DOC comments supply types and inspection contracts.
    /// Type annotations and PEP 484 comments remain independent authorities.
    pub fn analyze_doc_comments(&self) -> bool {
        self.analyze_doc_comments.load(Ordering::Relaxed)
    }
    pub fn set_analyze_doc_comments(&self, enabled: bool) {
        assert!(!self.frozen, "cannot mutate an analysis snapshot");
        if self.analyze_doc_comments.swap(enabled, Ordering::Relaxed) != enabled {
            self.world_changed();
        }
    }

    /// Changes the bounds of the disk-file cache (applied on the next load).
    pub fn set_cache_limits(&self, limits: CacheLimits) {
        assert!(!self.frozen, "cannot mutate an analysis snapshot");
        self.max_files.store(limits.max_files, Ordering::Relaxed);
        self.max_bytes.store(limits.max_bytes, Ordering::Relaxed);
    }

    pub fn cache_limits(&self) -> CacheLimits {
        CacheLimits {
            max_files: self.max_files.load(Ordering::Relaxed),
            max_bytes: self.max_bytes.load(Ordering::Relaxed),
        }
    }

    /// `(files, text bytes)` of the disk copies currently held in memory.
    pub fn cached_disk_files(&self) -> (usize, usize) {
        let files = self.files.read();
        (files.disk_count, files.disk_bytes)
    }

    /// Convenience: a project rooted at `root` targeting Python 3.12.
    pub fn for_project(root: &Path) -> Self {
        Self::new(ModuleResolver::new(
            vec![root.to_path_buf()],
            Vec::new(),
            (3, 12),
        ))
    }

    /// Publishes the class bases recorded by the persisted project index
    /// (`None` withdraws them). Hierarchy queries (subclasses, Go to
    /// Implementation, overriding markers) then resolve only the bases of
    /// classes whose recorded base names can denote the class asked about;
    /// files whose text differs from the indexed content are read directly.
    /// Answers do not change, so no analysis cache is invalidated.
    pub fn set_project_index(&self, index: Option<Arc<rustpy_index::SymbolIndex>>) {
        assert!(!self.frozen, "cannot mutate an analysis snapshot");
        let same = match (&*self.persisted_bases.read(), &index) {
            (Some(current), Some(index)) => current.is_for(index),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        *self.persisted_bases.write() =
            index.map(|index| Arc::new(crate::hierarchy::PersistedBases::new(index)));
        *self.hierarchy_index.write() = None;
    }

    /// Replaces the collected run-time types (see `runtime_types`).
    pub fn set_runtime_types(&self, rt: crate::runtime_types::RuntimeTypes) {
        assert!(!self.frozen, "cannot mutate an analysis snapshot");
        *self.runtime_types.write() = Arc::new(rt);
        self.world_changed();
    }

    pub fn runtime_types(&self) -> Arc<crate::runtime_types::RuntimeTypes> {
        self.runtime_types.read().clone()
    }

    /// Application publishes only a completed, identity-verified trusted library index.
    /// Passing NativeState::default() revokes generated skeleton authority.
    pub fn set_native_skeletons(&self, state: crate::native_state::NativeState) {
        assert!(!self.frozen, "cannot mutate an analysis snapshot");
        *self.native_state.write() = Arc::new(state);
        self.resolver.invalidate();
        let mut files = self.files.write();
        let compiled: Vec<_> = files
            .by_path
            .iter()
            .filter(|(path, _)| path.starts_with("<typeshed>/@compiled"))
            .map(|(path, id)| (path.clone(), *id))
            .collect();
        for (path, id) in compiled {
            files.by_path.remove(&path);
            files.evicted.remove(&id);
            files.take(&id);
        }
        files.by_module.clear();
        drop(files);
        self.world_changed();
    }

    /// Replaces the interpreter's docstrings (see `runtime_docs`).
    pub fn set_runtime_docs(&self, docs: Arc<crate::runtime_docs::RuntimeDocs>) {
        assert!(!self.frozen, "cannot mutate an analysis snapshot");
        *self.runtime_docs.write() = docs;
    }

    pub fn runtime_docs(&self) -> Arc<crate::runtime_docs::RuntimeDocs> {
        self.runtime_docs.read().clone()
    }

    /// Milliseconds on the database clock (never 0, which means "never checked").
    fn now_ms(&self) -> u64 {
        self.clock.elapsed().as_millis() as u64 + 1
    }

    /// Registers or updates the text of a file (open editor content).
    pub fn set_file_text(&self, path: &Path, text: &str) -> FileId {
        assert!(!self.frozen, "cannot mutate an analysis snapshot");
        let path = normalize_path(path);
        // Reparse incrementally from the previous version, outside the lock.
        let previous = {
            let files = self.files.read();
            files
                .by_path
                .get(&path)
                .and_then(|id| files.by_id.get(id))
                .cloned()
        };
        let parse = match &previous {
            Some(old) if &*old.text == text => old.parse.clone(),
            Some(old) => rustpy_syntax::reparse::reparse_text(&old.parse, &old.text, text).0,
            None => rustpy_syntax::parse(text),
        };
        if previous
            .as_ref()
            .is_none_or(|old| &*old.text != text || !old.is_open)
        {
            self.revisions.lock().changed_by(Some(&path));
            if crate::infer::library_path(self, &path) {
                self.library_returns.lock().clear();
            }
        }
        let module_name = self.resolver.module_name_of(&path);
        // Browsing/editing a runtime source must not replace the selected
        // stub in import resolution. The open text remains available by path.
        let publish_module =
            module_name
                .as_deref()
                .is_none_or(|name| match self.resolver.resolve(name) {
                    Some(crate::module::ModuleSource::File { path: selected, .. }) => {
                        selected == path
                            || (self.project_buffer_has_import_authority(&path, name)
                                && !self.selected_path_has_import_priority(&selected, name))
                    }
                    Some(crate::module::ModuleSource::Stub(_)) => {
                        self.project_buffer_has_import_authority(&path, name)
                    }
                    _ => true,
                });
        let mut files = self.files.write();
        let id = match files.by_path.get(&path) {
            Some(id) => *id,
            None => {
                let id = self.vfs.id(Vfs::real_key(&path));
                files.by_path.insert(path.clone(), id);
                id
            }
        };
        let is_stub = path.extension().and_then(|e| e.to_str()) == Some("pyi");
        let static_env = self.resolver.for_path(&path).static_env();
        let info = Arc::new(FileInfo::with_parse(
            id,
            path,
            module_name.clone(),
            text.to_string(),
            parse,
            is_stub,
            true,
            &static_env,
        ));
        files.evicted.remove(&id);
        files.put(id, info);
        if let Some(m) = module_name {
            if publish_module {
                files.by_module.insert(m, Some(id));
            } else if files.by_module.get(&m) == Some(&Some(id)) {
                files.by_module.remove(&m);
            }
        }
        id
    }

    /// Removes an open file's content (falls back to disk on next access).
    pub fn close_file(&self, path: &Path) {
        assert!(!self.frozen, "cannot mutate an analysis snapshot");
        let path = normalize_path(path);
        let path = path.as_path();
        self.world_changed_at([path]);
        let mut files = self.files.write();
        if let Some(id) = files.by_path.get(path).copied() {
            files
                .evicted
                .insert(id, (path.to_path_buf(), None, None, Weak::new()));
            let module = files.take(&id).and_then(|f| f.module_name.clone());
            if let Some(m) = module {
                files.by_module.remove(&m);
            }
            files.by_module.retain(|_, v| *v != Some(id));
        }
    }

    /// Forgets what is cached for `path` (a file, or a directory and
    /// everything below it) after it changed, appeared or disappeared on
    /// disk; it is re-read on next access. Files open in an editor keep
    /// their editor text. When the set of files changed (rather than the
    /// contents of a known file), module lookups are recomputed too: a new
    /// file can make a failed import resolvable, a deleted one can unshadow
    /// another module.
    pub fn invalidate_path(&self, path: &Path) {
        self.invalidate_paths(&[path.to_path_buf()]);
    }

    /// [`Analysis::invalidate_path`] for a batch of paths (a burst of file
    /// watcher events after `git checkout` or a formatter run), in one pass
    /// over the cache rather than one per path. A path whose cached copy is a
    /// placeholder (a namespace package's `__init__.py`) changes the set of
    /// files when it appears.
    pub fn invalidate_paths(&self, paths: &[PathBuf]) {
        assert!(!self.frozen, "cannot mutate an analysis snapshot");
        let normalized: Vec<_> = paths.iter().map(|p| normalize_path(p)).collect();
        let paths = normalized.as_slice();
        // Ask the file system before taking the lock.
        let mut seen: FxHashSet<&Path> = FxHashSet::default();
        let touched: Vec<(&Path, bool)> = paths
            .iter()
            .map(PathBuf::as_path)
            .filter(|p| seen.insert(p))
            .map(|p| (p, p.is_file()))
            .collect();
        if touched.is_empty() {
            return;
        }
        self.world_changed_at(touched.iter().map(|(path, _)| *path));
        // Directories, and deleted paths that may have been directories.
        let dirs: FxHashSet<&Path> = touched
            .iter()
            .filter(|(_, is_file)| !is_file)
            .map(|(p, _)| *p)
            .collect();
        let mut files = self.files.write();
        let mut dropped: FxHashSet<FileId> = FxHashSet::default();
        let mut freed: Vec<Arc<FileInfo>> = Vec::new();
        let mut structural = false;
        for &(path, is_file) in &touched {
            let Some(id) = files.by_path.get(path).copied() else {
                // A file the analysis never read: a new file, or a directory.
                structural = true;
                continue;
            };
            let read_from_disk = files.by_id.get(&id).is_some_and(|f| f.disk_stamp.is_some());
            if !is_file || (!read_from_disk && files.by_id.get(&id).is_some_and(|f| !f.is_open)) {
                structural = true;
            }
            if let Some((id, f)) = files.drop_disk_entry(path) {
                dropped.insert(id);
                freed.extend(f);
            }
        }
        if !dirs.is_empty() {
            let below: Vec<PathBuf> = files
                .by_path
                .keys()
                .filter(|p| p.ancestors().skip(1).any(|a| dirs.contains(a)))
                .cloned()
                .collect();
            for p in &below {
                if let Some((id, f)) = files.drop_disk_entry(p) {
                    dropped.insert(id);
                    freed.extend(f);
                }
                structural = true;
            }
        }
        if structural {
            files.by_module.clear();
        } else if !dropped.is_empty() {
            files
                .by_module
                .retain(|_, v| v.is_none_or(|id| !dropped.contains(&id)));
        }
        drop(files);
        if structural {
            self.resolver.invalidate();
        }
        drop(freed);
    }

    /// Forgets every file read from disk (and every namespace-package
    /// placeholder) and every module lookup (File › Synchronize, bulk
    /// changes): everything is re-read on next access. Editor texts and
    /// bundled stubs stay.
    pub fn invalidate_disk_files(&self) {
        assert!(!self.frozen, "cannot mutate an analysis snapshot");
        self.world_changed();
        let mut files = self.files.write();
        let mut disk: Vec<PathBuf> = files
            .by_id
            .values()
            .filter(|f| !f.is_open && !f.path.starts_with("<typeshed>"))
            .map(|f| f.path.clone())
            .collect();
        disk.extend(files.evicted.values().map(|(p, ..)| p.clone()));
        for p in &disk {
            let _ = files.drop_disk_entry(p);
        }
        files.by_module.clear();
        drop(files);
        self.resolver.invalidate();
    }

    /// Forgets every module lookup (successful or failed) without dropping
    /// file contents: after packages were installed or removed outside the
    /// project (a virtual environment's `site-packages`), imports are
    /// resolved again on next access.
    pub fn invalidate_modules(&self) {
        assert!(!self.frozen, "cannot mutate an analysis snapshot");
        self.files.write().by_module.clear();
        self.resolver.invalidate();
        // Publish the completed resolution invalidation to dependency caches.
        self.world_changed();
    }

    /// Like [`Analysis::file`], but a disk file verified within the last
    /// recheck interval is returned without reading and hashing it again
    /// (the policy module lookups already use). Flow-cache currency checks
    /// run several times per query; forcing a full re-read for each one made
    /// inspection quadratic in the file size.
    pub(crate) fn file_recently_checked(&self, id: FileId) -> Option<Arc<FileInfo>> {
        let cached = self.files.read().by_id.get(&id).cloned();
        match cached {
            Some(file) => self.fresh(file, false),
            None => self.file(id),
        }
    }

    /// [`Analysis::revision_excluding`] for the path of a database-owned
    /// [`FileInfo`], which is already normalized: no filesystem calls.
    pub(crate) fn revision_excluding_file(&self, file: &FileInfo) -> u64 {
        self.revisions.lock().excluding(&file.path)
    }

    pub fn file(&self, id: FileId) -> Option<Arc<FileInfo>> {
        let (cached, evicted) = {
            let files = self.files.read();
            (
                files.by_id.get(&id).cloned(),
                files.evicted.get(&id).cloned(),
            )
        };
        if let Some(file) = cached {
            return self.fresh(file, true);
        }
        if self.frozen {
            self.missing_dependency.store(true, Ordering::Release);
            return None;
        }
        let (path, stamp, revision, alive) = evicted?;
        // A dropped copy that a running analysis still holds keeps its
        // identity: re-reading would build a new parse per lookup, and every
        // identity-keyed cache of that analysis would miss (a project pass
        // with more files in flight than the cache bound re-parsed and
        // re-walked each file thousands of times).
        if let Some(file) = alive.upgrade() {
            return self.fresh(file, true);
        }
        // Dropped from the cache: read it again under the same id.
        let f = self.load(&path, Some(id))?;
        if f.disk_stamp != stamp || f.disk_revision != revision {
            self.world_changed_at([path.as_path()]);
        }
        Some(f)
    }

    fn cached(&self, path: &Path) -> Option<Arc<FileInfo>> {
        let files = self.files.read();
        files
            .by_path
            .get(path)
            .and_then(|id| files.by_id.get(id))
            .cloned()
    }

    /// Cached parse for an exact canonical path. Never normalizes, loads,
    /// refreshes disk metadata, or parses; callers must validate source identity.
    pub fn cached_file_for_path(&self, path: &Path) -> Option<Arc<FileInfo>> {
        self.cached(path)
    }

    /// File for a path, loading it from disk if it is not open. A cached
    /// disk copy is re-read when the file changed on disk since it was read,
    /// and forgotten when the file was deleted.
    pub fn file_for_path(&self, path: &Path) -> Option<Arc<FileInfo>> {
        let normalized = if self.frozen || path.starts_with("<typeshed>") {
            path.to_path_buf()
        } else {
            normalize_path(path)
        };
        let path = normalized.as_path();
        match self.cached(path) {
            Some(f) => self.fresh(f, true),
            None if path.starts_with("<typeshed>") => {
                // Browse the bundled library independently of import resolution.
                // In particular, do not populate by_module: user stub roots and
                // project modules must retain their ordinary precedence.
                let relative = path.strip_prefix("<typeshed>").ok()?;
                if relative
                    .components()
                    .any(|part| !matches!(part, std::path::Component::Normal(_)))
                {
                    return None;
                }
                let source = relative.to_str()?.strip_suffix(".pyi")?;
                let module = source.replace(std::path::MAIN_SEPARATOR, ".");
                if module.is_empty() || module.split('.').any(str::is_empty) {
                    return None;
                }
                let text = crate::typeshed::stub_for_module(&module)?;
                self.insert_virtual(path.to_path_buf(), Some(module), text.to_owned(), true)
            }
            None => self.load(path, None),
        }
    }

    /// The text the analysis has (or would load) for `path`, without parsing
    /// a file that is not cached: project-wide searches test it before
    /// paying for a parse.
    pub fn current_text(&self, path: &Path) -> Option<Arc<str>> {
        let normalized = if self.frozen || path.starts_with("<typeshed>") {
            path.to_path_buf()
        } else {
            normalize_path(path)
        };
        let path = normalized.as_path();
        match self.cached(path) {
            Some(f) => self.fresh(f, true).map(|f| f.text.clone()),
            None if self.frozen => {
                self.missing_dependency.store(true, Ordering::Release);
                None
            }
            None => self.vfs.read(path, self.read_limits()).ok().and_then(|s| {
                rustpy_text::Encoding::decode_python_source(&s.bytes)
                    .ok()
                    .map(Arc::from)
            }),
        }
    }

    /// Explains a failed disk-source decode for callers whose lookup returned
    /// `None`. Open editor text is authoritative and has no disk decode error.
    pub fn file_encoding_error(&self, path: &Path) -> Option<rustpy_text::EncodingError> {
        if self.frozen {
            return None;
        }
        if self
            .cached(&normalize_path(path))
            .is_some_and(|file| file.disk_stamp.is_none())
        {
            return None;
        }
        let snapshot = self.vfs.read(path, self.read_limits()).ok()?;
        rustpy_text::Encoding::decode_python_source(&snapshot.bytes).err()
    }

    /// `f`, or its current contents when the disk copy changed since it was
    /// read (`force`: compare even if the stamp was compared moments ago).
    fn fresh(&self, f: Arc<FileInfo>, force: bool) -> Option<Arc<FileInfo>> {
        if self.frozen {
            return Some(f);
        }
        let Some(_) = f.disk_stamp else {
            return Some(f);
        };
        let now = self.now_ms();
        if f.last_used.load(Ordering::Relaxed) != now {
            f.last_used.store(now, Ordering::Relaxed);
        }
        let force = force && self.batch_reads.load(Ordering::Relaxed) == 0;
        if !force && now.saturating_sub(f.checked_ms.load(Ordering::Relaxed)) < RECHECK_MS {
            return Some(f);
        }
        // Installed library sources are treated as immutable between project
        // edits: a stamp (size, mtime) that matched a full comparison after
        // leaving the racy window answers without reading and hashing the whole
        // file again. Project files keep the content comparison, which also
        // detects rewrites that restore the previous metadata.
        let library = crate::infer::library_path(self, &f.path);
        let stamp = if library {
            DiskStamp::of(&f.path)
        } else {
            None
        };
        let settled = stamp
            .as_ref()
            .and_then(|stamp| stamp.mtime)
            .is_some_and(|mtime| {
                SystemTime::now()
                    .duration_since(mtime)
                    .is_ok_and(|age| age > RACY_STAMP)
            });
        if library && settled && stamp == f.disk_stamp && f.stable_stamp.load(Ordering::Relaxed) {
            f.checked_ms.store(now, Ordering::Relaxed);
            return Some(f);
        }
        match self.vfs.read(&f.path, self.read_limits()) {
            Ok(snapshot) if Some(snapshot.revision) == f.disk_revision => {
                f.checked_ms.store(now, Ordering::Relaxed);
                if library && settled && stamp == f.disk_stamp {
                    f.stable_stamp.store(true, Ordering::Relaxed);
                }
                Some(f)
            }
            Ok(_) => {
                self.world_changed_at([f.path.as_path()]);
                self.load(&f.path, Some(f.id))
            }
            Err(_) => {
                self.invalidate_path(&f.path);
                None
            }
        }
    }

    pub fn set_read_limits(&self, limits: ReadLimits) {
        assert!(!self.frozen, "cannot mutate an analysis snapshot");
        self.max_file_bytes
            .store(limits.max_file_bytes, Ordering::Relaxed);
        self.set_cache_limits(CacheLimits {
            max_files: limits.max_cached_files,
            max_bytes: limits.max_cached_bytes,
        });
    }
    pub fn read_limits(&self) -> ReadLimits {
        let limits = self.cache_limits();
        ReadLimits {
            max_file_bytes: self.max_file_bytes.load(Ordering::Relaxed),
            max_cached_bytes: limits.max_bytes,
            max_cached_files: limits.max_files,
        }
    }

    /// Reads `path` from disk (re-using `reuse` as its id when re-reading a
    /// cached file, so ids held elsewhere stay valid; a path known from
    /// before keeps its id too).
    fn load(&self, path: &Path, reuse: Option<FileId>) -> Option<Arc<FileInfo>> {
        if self.frozen {
            self.missing_dependency.store(true, Ordering::Release);
            return None;
        }
        let normalized = normalize_path(path);
        let path = normalized.as_path();
        let snapshot = match self.vfs.read(path, self.read_limits()) {
            Ok(snapshot) => snapshot,
            Err(_) => {
                if reuse.is_some() {
                    self.invalidate_path(path);
                }
                return None;
            }
        };
        let text = match rustpy_text::Encoding::decode_python_source(&snapshot.bytes) {
            Ok(text) => text,
            Err(error) => {
                log::warn!("Cannot decode {}: {error}", path.display());
                self.invalidate_path(path);
                return None;
            }
        };
        let stamp = DiskStamp::of(path);
        let id = snapshot.id;
        let module_name = self.resolver.module_name_of(path);
        let is_stub = path.extension().and_then(|e| e.to_str()) == Some("pyi");
        let mut info = FileInfo::new(
            id,
            path.to_path_buf(),
            module_name.clone(),
            text,
            is_stub,
            false,
            &self.resolver.for_path(path).static_env(),
        );
        info.disk_revision = Some(snapshot.revision);
        info.disk_stamp = Some(stamp.unwrap_or(DiskStamp {
            len: info.text.len() as u64,
            mtime: None,
        }));
        let now = self.now_ms();
        info.checked_ms = AtomicU64::new(now);
        info.last_used = AtomicU64::new(now);
        let info = Arc::new(info);
        let mut files = self.files.write();
        if let Some(current) = files.by_path.get(path).and_then(|id| files.by_id.get(id)) {
            // An editor opened the file meanwhile: its text wins. Another
            // thread loaded the same contents meanwhile: share its copy, so
            // every thread sees one id per file.
            if current.is_open
                || (current.disk_stamp == info.disk_stamp && current.text == info.text)
            {
                return Some(current.clone());
            }
        }
        if let Some(old) = files
            .by_path
            .insert(path.to_path_buf(), id)
            .filter(|old| *old != id)
        {
            files.take(&old);
            files.evicted.remove(&old);
        }
        files.evicted.remove(&id);
        files.put(id, info.clone());
        if let Some(m) = module_name {
            let entry = files.by_module.entry(m).or_insert(None);
            if entry.is_none() {
                *entry = Some(id);
            }
        }
        files.enforce(CacheLimits {
            max_files: self.max_files.load(Ordering::Relaxed),
            max_bytes: self.max_bytes.load(Ordering::Relaxed),
        });
        Some(info)
    }

    fn project_buffer_has_import_authority(&self, path: &Path, name: &str) -> bool {
        let resolver = self.resolver.for_path(path);
        if resolver.is_excluded(path)
            || resolver
                .stub_roots
                .iter()
                .any(|root| resolver.root_containing(path, root).is_some())
            || matches!(
                name.split('.').next(),
                Some(
                    "builtins"
                        | "typing"
                        | "typing_extensions"
                        | "_typeshed"
                        | "collections"
                        | "abc"
                )
            )
        {
            return false;
        }
        let project = resolver
            .roots
            .iter()
            .filter_map(|root| resolver.root_containing(path, root))
            .map(|root| root.components().count())
            .max();
        let library = resolver
            .search_paths
            .iter()
            .filter_map(|root| resolver.root_containing(path, root))
            .map(|root| root.components().count())
            .max();
        // A search path equal to the project root remains project source;
        // a more specific nested interpreter environment remains a library.
        project.is_some_and(|depth| library.is_none_or(|library| depth >= library))
    }

    fn selected_path_has_import_priority(&self, path: &Path, name: &str) -> bool {
        if path.starts_with("<typeshed>") {
            return false;
        }
        let resolver = self.resolver.for_path(path);
        path.extension().is_some_and(|extension| extension == "pyi")
            || resolver
                .stub_roots
                .iter()
                .any(|root| resolver.root_containing(path, root).is_some())
            || self.project_buffer_has_import_authority(path, name)
    }

    // This predicate only consults immutable resolver policy and FileInfo;
    // it must never acquire the files lock or resolve/load another module.
    fn module_cache_admits(&self, name: &str, file: &FileInfo, selected: Option<&Path>) -> bool {
        let selected_file = selected.is_some_and(|path| path == file.path.as_path());
        let project_buffer =
            file.is_open && self.project_buffer_has_import_authority(&file.path, name);
        let selected_priority =
            selected.is_some_and(|path| self.selected_path_has_import_priority(path, name));
        !self.resolver.is_excluded(&file.path)
            && (selected_file || (project_buffer && !selected_priority))
    }

    fn publish_module_result(
        &self,
        name: &str,
        selected: Option<&Path>,
        result: Option<Arc<FileInfo>>,
    ) -> Option<Arc<FileInfo>> {
        let mut files = self.files.write();
        // Loading/parsing happened outside this lock. An editor may have
        // published the authoritative provider or newer overlay meanwhile.
        // Recheck under the same lock that publishes the result so a late
        // library completion cannot replace that winner (including with None).
        let current = files
            .by_module
            .get(name)
            .and_then(|id| *id)
            .and_then(|id| files.by_id.get(&id));
        if let Some(current) = current.filter(|file| self.module_cache_admits(name, file, selected))
        {
            return Some(current.clone());
        }
        files
            .by_module
            .insert(name.to_string(), result.as_ref().map(|file| file.id));
        result
    }

    /// The file implementing a dotted module name, if resolvable.
    pub fn module(&self, name: &str) -> Option<Arc<FileInfo>> {
        // The name cache is populated by indexing and editor buffers as well as
        // import resolution. Only the resolver-selected provider owns imports.
        let src = if self.frozen {
            // A captured open buffer can supply a module that does not exist
            // on disk. Consult the captured provider without declaring it an
            // uncaptured dependency before checking that buffer's authority.
            self.resolver
                .cached_source(name)
                .unwrap_or_else(|| self.resolver.resolve(name))
        } else {
            self.resolver.resolve(name)
        };
        let selected_path = match &src {
            Some(ModuleSource::File { path, .. }) | Some(ModuleSource::Binary(path)) => {
                Some(path.clone())
            }
            Some(ModuleSource::Namespace(path)) => Some(path.join("__init__.py")),
            Some(ModuleSource::Stub(stub)) => Some(PathBuf::from(format!(
                "<typeshed>/{}.pyi",
                stub.replace('.', "/")
            ))),
            None => None,
        }
        .map(|path| {
            if self.frozen || path.starts_with("<typeshed>") {
                path
            } else {
                // Memoized until the resolver is invalidated (structural or
                // module-lookup changes), like the resolution itself.
                self.resolver.normalized(&path)
            }
        });
        let hit = self.files.read().by_module.get(name).copied();
        match hit {
            Some(None) => {
                if self.frozen {
                    self.missing_dependency.store(true, Ordering::Release);
                }
            }
            Some(Some(id)) => {
                let cached = self.files.read().by_id.get(&id).cloned();
                let observed = match cached {
                    Some(f) => self.fresh(f, false),
                    None => self.file(id),
                };
                if let Some(file) = observed
                    .as_ref()
                    .filter(|file| self.module_cache_admits(name, file, selected_path.as_deref()))
                {
                    return Some(file.clone());
                }
                // Gone or excluded from imports: resolve the name again. Explicit
                // file_for_path/editor ownership remains available. Never erase
                // a provider/overlay installed after the rejected observation.
                let mut files = self.files.write();
                let unchanged = match (observed.as_ref(), files.by_id.get(&id)) {
                    (Some(observed), Some(current)) => Arc::ptr_eq(observed, current),
                    (None, None) => true,
                    _ => false,
                };
                if unchanged && files.by_module.get(name) == Some(&Some(id)) {
                    files.by_module.remove(name);
                }
            }
            None => {}
        }
        if self.frozen && src.is_none() {
            self.missing_dependency.store(true, Ordering::Release);
        }
        let result = match src {
            Some(ModuleSource::File { path, .. }) => self.file_for_path(&path),
            Some(ModuleSource::Namespace(dir)) => {
                let path = dir.join("__init__.py");
                self.insert_virtual(path, Some(name.to_string()), String::new(), false)
            }
            Some(ModuleSource::Stub(stub)) => {
                let text = crate::typeshed::stub_for_module(&stub)
                    .unwrap_or("")
                    .to_string();
                let path = PathBuf::from(format!("<typeshed>/{}.pyi", stub.replace('.', "/")));
                self.insert_virtual(path, Some(name.to_string()), text, true)
            }
            Some(ModuleSource::Binary(path)) => self.binary_module(&path, Some(name.to_string())),
            None => None,
        };
        self.publish_module_result(name, selected_path.as_deref(), result)
    }

    /// Resolve from a module's SDK without allowing another module's cached
    /// dotted-name provider to substitute a different interpreter package.
    pub fn module_from(&self, from: &FileInfo, name: &str) -> Option<Arc<FileInfo>> {
        let origin = from
            .module_origin
            .as_deref()
            .or_else(|| self.resolver.module_root_for(&from.path))
            .unwrap_or(&from.path);
        let resolver = self.resolver.for_path(origin);
        if std::ptr::eq(resolver, &self.resolver) {
            return self.module(name);
        }
        match resolver.resolve(name)? {
            ModuleSource::File { path, .. } => {
                let original = self.file_for_path(&path)?;
                if resolver
                    .search_paths
                    .iter()
                    .any(|root| resolver.root_containing(&path, root).is_some())
                    || resolver
                        .stub_roots
                        .iter()
                        .any(|root| resolver.root_containing(&path, root).is_some())
                {
                    self.contextual_module(&original, origin, resolver, Some(name.into()))
                } else {
                    Some(original)
                }
            }
            ModuleSource::Namespace(path) => self.insert_virtual(
                path.join("__init__.py"),
                Some(name.into()),
                String::new(),
                false,
            ),
            ModuleSource::Binary(path) => self.binary_module(&path, Some(name.into())),
            ModuleSource::Stub(module) => {
                let original = self.module(&module)?;
                self.contextual_module(&original, origin, resolver, Some(module))
            }
        }
    }

    fn contextual_module(
        &self,
        original: &FileInfo,
        origin: &Path,
        resolver: &ModuleResolver,
        module_name: Option<String>,
    ) -> Option<Arc<FileInfo>> {
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        origin.hash(&mut hash);
        original.path.hash(&mut hash);
        original.text.hash(&mut hash);
        resolver.python_version.hash(&mut hash);
        resolver.platform.hash(&mut hash);
        let key = PathBuf::from(format!("<module-context>/{:x}.pyi", hash.finish()));
        self.insert_virtual_context(
            key,
            module_name,
            original.text.to_string(),
            original.is_stub,
            resolver.static_env(),
            Some((original.path.clone(), origin.to_path_buf())),
        )
    }

    pub fn import_module_from(&self, from: &FileInfo, name: &str) -> Option<Arc<FileInfo>> {
        self.module_from(from, name).filter(|module| {
            !module.path.starts_with("<typeshed>")
                || self
                    .resolver
                    .for_path(from.module_origin.as_deref().unwrap_or(&from.path))
                    .import_available(name)
        })
    }

    /// Resolves an import target while preserving missing-dependency diagnostics
    /// for bundled third-party type information. Open project buffers remain
    /// valid import targets even before they have been saved to disk.
    pub fn import_module(&self, name: &str) -> Option<Arc<FileInfo>> {
        self.module(name).filter(|module| {
            !module.path.starts_with("<typeshed>") || self.resolver.import_available(name)
        })
    }

    fn insert_virtual(
        &self,
        path: PathBuf,
        module_name: Option<String>,
        text: String,
        is_stub: bool,
    ) -> Option<Arc<FileInfo>> {
        let environment = self.resolver.for_path(&path).static_env();
        self.insert_virtual_with_env(path, module_name, text, is_stub, environment)
    }

    fn insert_virtual_with_env(
        &self,
        path: PathBuf,
        module_name: Option<String>,
        text: String,
        is_stub: bool,
        environment: crate::static_cond::StaticEnv,
    ) -> Option<Arc<FileInfo>> {
        self.insert_virtual_context(path, module_name, text, is_stub, environment, None)
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_virtual_context(
        &self,
        path: PathBuf,
        module_name: Option<String>,
        text: String,
        is_stub: bool,
        environment: crate::static_cond::StaticEnv,
        origin: Option<(PathBuf, PathBuf)>,
    ) -> Option<Arc<FileInfo>> {
        // Copy the id out before `file()` locks again: holding the read
        // guard across it deadlocks once a writer queues in between
        // (parking_lot makes new readers wait behind a waiting writer).
        let known = self.files.read().by_path.get(&path).copied();
        if let Some(f) = known.and_then(|id| self.file(id)) {
            return Some(f);
        }
        // Unknown, or dropped since: insert it.
        if self.frozen
            && (text.len() > 1024 * 1024
                || self
                    .files
                    .read()
                    .by_id
                    .values()
                    .map(|file| file.text.len())
                    .sum::<usize>()
                    .saturating_add(text.len())
                    > 64 * 1024 * 1024)
        {
            self.missing_dependency.store(true, Ordering::Release);
            return None;
        }
        let id = if self.frozen {
            // Retained IDs belong to the live source catalogue. New immutable
            // synthetic sources use only the snapshot's disjoint local range;
            // no publication touches either live or snapshot VFS.
            let mut next = self.snapshot_next_id.load(Ordering::Relaxed);
            loop {
                let incremented = next.checked_add(1)?;
                match self.snapshot_next_id.compare_exchange_weak(
                    next,
                    incremented,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break FileId(next),
                    Err(current) => next = current,
                }
            }
        } else {
            self.vfs
                .publish(
                    FileKey::Virtual(path.clone()),
                    Arc::from(text.as_bytes()),
                    self.read_limits(),
                )
                .ok()?
                .id
        };
        let mut info = FileInfo::new(
            id,
            origin
                .as_ref()
                .map(|(source, _)| source.clone())
                .unwrap_or_else(|| path.clone()),
            module_name,
            text,
            is_stub,
            false,
            &environment,
        );
        info.module_origin = origin.map(|(_, module)| module);
        let info = Arc::new(info);
        let mut files = self.files.write();
        // Another thread inserted it meanwhile: keep the first copy (one id per file).
        if let Some(existing) = files.by_path.get(&path).and_then(|id| files.by_id.get(id)) {
            return Some(existing.clone());
        }
        if info.module_origin.is_some() {
            let stale: Vec<_> = files
                .by_id
                .iter()
                .filter(|(_, old)| {
                    old.path == info.path
                        && old.module_origin == info.module_origin
                        && old.text != info.text
                })
                .map(|(id, _)| *id)
                .collect();
            for stale_id in stale {
                files.by_path.retain(|_, id| *id != stale_id);
                files.take(&stale_id);
            }
        }
        files.by_path.insert(path, id);
        files.put(id, info.clone());
        Some(info)
    }

    /// Resolves `from <level dots><module> import` relative to `from_file`.
    pub fn module_relative(
        &self,
        from_file: &FileInfo,
        level: usize,
        module: &str,
    ) -> Option<Arc<FileInfo>> {
        if level == 0 {
            return self.module_from(from_file, module);
        }
        if from_file.path.starts_with("<typeshed>") {
            // Relative imports inside stubs: compute from the module name.
            let base = from_file.module_name.as_deref()?;
            let mut parts: Vec<&str> = base.split('.').collect();
            if !from_file.is_package_init() && !crate::typeshed::is_package(base) {
                parts.pop();
            }
            for _ in 1..level {
                parts.pop();
            }
            let mut name = parts.join(".");
            if !module.is_empty() {
                if !name.is_empty() {
                    name.push('.');
                }
                name.push_str(module);
            }
            return self.module_from(from_file, &name);
        }
        let origin = from_file
            .module_origin
            .as_deref()
            .or_else(|| self.resolver.module_root_for(&from_file.path))
            .unwrap_or(&from_file.path);
        let resolver = self.resolver.for_path(origin);
        let source = resolver.resolve_relative(&from_file.path, level, module);
        if self.frozen && source.is_none() {
            self.missing_dependency.store(true, Ordering::Release);
        }
        match source? {
            ModuleSource::File { path, .. } => {
                let original = match self.cached(&path) {
                    Some(file) => self.fresh(file, false),
                    None => self.load(&path, None),
                }?;
                if !std::ptr::eq(resolver, &self.resolver)
                    && (resolver
                        .search_paths
                        .iter()
                        .any(|root| resolver.root_containing(&path, root).is_some())
                        || resolver
                            .stub_roots
                            .iter()
                            .any(|root| resolver.root_containing(&path, root).is_some()))
                {
                    self.contextual_module(
                        &original,
                        origin,
                        resolver,
                        resolver.module_name_of(&path),
                    )
                } else {
                    Some(original)
                }
            }
            ModuleSource::Namespace(dir) => {
                self.insert_virtual(dir.join("__init__.py"), None, String::new(), false)
            }
            ModuleSource::Stub(s) => self.module(&s),
            ModuleSource::Binary(path) => {
                let name = self.resolver.module_name_of(&path);
                self.binary_module(&path, name)
            }
        }
    }

    /// The placeholder stub of a compiled extension module (see
    /// [`crate::module::BINARY_MODULE_STUB`]): a virtual read-only file.
    fn binary_module(&self, path: &Path, module_name: Option<String>) -> Option<Arc<FileInfo>> {
        let file_name = path.file_name()?.to_string_lossy().into_owned();
        let shown = module_name.clone().unwrap_or_else(|| {
            crate::module::extension_module_name(&file_name)
                .unwrap_or("module")
                .to_string()
        });
        let native = self.native_state.read().clone();
        if let Some(source) = native.source(&shown, path) {
            let virtual_path = native.virtual_path(&shown, path, &source)?;
            return self.insert_virtual(virtual_path, module_name, source.text.to_string(), true);
        }
        let virtual_path = PathBuf::from(format!("<typeshed>/@compiled/{shown}.pyi"));
        let text = format!(
            "# {}\n{}",
            path.display(),
            crate::module::BINARY_MODULE_STUB
        );
        self.insert_virtual(virtual_path, module_name, text, true)
    }

    /// The `builtins` stub.
    pub fn builtins(&self) -> Option<Arc<FileInfo>> {
        self.module("builtins")
    }

    pub fn open_files(&self) -> Vec<Arc<FileInfo>> {
        self.files
            .read()
            .by_id
            .values()
            .filter(|f| f.is_open)
            .cloned()
            .collect()
    }

    /// Capture at most `limit` already-loaded files without allocating on refusal.
    pub fn files_with_limit(&self, limit: usize) -> Option<Vec<Arc<FileInfo>>> {
        let files = self.files.read();
        if files.by_id.len() > limit {
            return None;
        }
        Some(files.by_id.values().cloned().collect())
    }

    /// Known direct importers, used only as background scheduling hints.
    /// Broad candidate scans remain authoritative for ordering and fresh text.
    pub fn importers_of(&self, target: FileId) -> Vec<Arc<FileInfo>> {
        self.importers_of_cancellable(target, &|| false)
            .unwrap_or_default()
    }

    pub fn importers_of_cancellable(
        &self,
        target: FileId,
        cancelled: &dyn Fn() -> bool,
    ) -> Option<Vec<Arc<FileInfo>>> {
        if cancelled() {
            return None;
        }
        let mut sources = self.all_files();
        sources.sort_by_key(|file| file.id.0);
        let generation = self.source_generation();
        let cached = self
            .import_graph
            .read()
            .as_ref()
            .filter(|graph| {
                graph.generation == generation
                    && graph.sources.len() == sources.len()
                    && graph
                        .sources
                        .iter()
                        .zip(&sources)
                        .all(|(previous, current)| {
                            previous
                                .upgrade()
                                .is_some_and(|previous| Arc::ptr_eq(&previous, current))
                        })
            })
            .cloned();
        let graph = if let Some(graph) = cached {
            graph
        } else {
            let mut importers: FxHashMap<FileId, Vec<std::sync::Weak<FileInfo>>> =
                FxHashMap::default();
            let modules: FxHashMap<_, _> = sources
                .iter()
                .filter_map(|file| file.module_name.as_deref().map(|name| (name, file.id)))
                .collect();
            for source in &sources {
                if cancelled() {
                    return None;
                }
                let mut add = |module: &str, level: usize, imported: Option<&str>| {
                    let mut parts: Vec<_> = if level == 0 {
                        Vec::new()
                    } else {
                        let mut parts: Vec<_> = source
                            .module_name
                            .as_deref()
                            .unwrap_or("")
                            .split('.')
                            .filter(|part| !part.is_empty())
                            .collect();
                        let package = source
                            .path
                            .file_stem()
                            .is_some_and(|name| name == "__init__")
                            || source.path.starts_with("<typeshed>")
                                && source
                                    .module_name
                                    .as_deref()
                                    .is_some_and(crate::typeshed::is_package);
                        if !package {
                            parts.pop();
                        }
                        for _ in 1..level {
                            parts.pop();
                        }
                        parts
                    };
                    parts.extend(
                        module
                            .trim_start_matches('.')
                            .split('.')
                            .filter(|part| !part.is_empty()),
                    );
                    let module = parts.join(".");
                    let child = imported.filter(|name| *name != "*").map(|name| {
                        if module.is_empty() {
                            name.to_owned()
                        } else {
                            format!("{module}.{name}")
                        }
                    });
                    for name in std::iter::once(module.as_str()).chain(child.as_deref()) {
                        if let Some(target) = modules.get(name) {
                            let entry = importers.entry(*target).or_default();
                            if !entry.iter().any(|previous| {
                                previous
                                    .upgrade()
                                    .is_some_and(|previous| previous.id == source.id)
                            }) {
                                entry.push(Arc::downgrade(source));
                            }
                        }
                    }
                };
                for binding in &source.scopes.bindings {
                    if cancelled() {
                        return None;
                    }
                    if let scope::BindingKind::Import(import) = &binding.kind {
                        add(&import.module, import.level, import.name.as_deref());
                    }
                }
                for scope in &source.scopes.scopes {
                    if cancelled() {
                        return None;
                    }
                    for import in &scope.star_imports {
                        add(&import.module, import.level, None);
                    }
                }
            }
            let graph = Arc::new(ImportGraph {
                generation,
                sources: sources.iter().map(Arc::downgrade).collect(),
                importers,
            });
            if generation == self.source_generation() {
                *self.import_graph.write() = Some(graph.clone());
            }
            graph
        };
        if cancelled() {
            return None;
        }
        Some(
            graph
                .importers
                .get(&target)
                .into_iter()
                .flatten()
                .filter_map(std::sync::Weak::upgrade)
                .collect(),
        )
    }

    pub fn all_files(&self) -> Vec<Arc<FileInfo>> {
        self.files.read().by_id.values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_library_publication_keeps_the_intervening_project_overlay() {
        let root = Path::new("/module-publication-interleave");
        let db = Arc::new(Analysis::for_project(root));
        let loaded = Arc::new(std::sync::Barrier::new(2));
        let publish = Arc::new(std::sync::Barrier::new(2));
        let worker = {
            let db = db.clone();
            let loaded = loaded.clone();
            let publish = publish.clone();
            std::thread::spawn(move || {
                let path = PathBuf::from("<typeshed>/random.pyi");
                let library = db
                    .insert_virtual(
                        path.clone(),
                        Some("random".into()),
                        crate::typeshed::stub_for_module("random").unwrap().into(),
                        true,
                    )
                    .unwrap();
                // Exactly the module() interleaving: library load is complete,
                // but its name-map publication has not happened yet.
                loaded.wait();
                publish.wait();
                db.publish_module_result("random", Some(&path), Some(library))
                    .unwrap()
            })
        };
        loaded.wait();
        let path = root.join("random.py");
        db.set_file_text(&path, "local = 1\n");
        let id = db.set_file_text(&path, "local = 2\n");
        let latest = db.file(id).unwrap();
        publish.wait();
        let winner = worker.join().unwrap();
        assert!(Arc::ptr_eq(&winner, &latest));
        assert!(Arc::ptr_eq(&db.module("random").unwrap(), &latest));
        assert_eq!(&*winner.text, "local = 2\n");

        let unknown = root.join("new_project_module.py");
        let id = db.set_file_text(&unknown, "value = 3\n");
        let winner = db
            .publish_module_result("new_project_module", None, None)
            .unwrap();
        assert_eq!(
            winner.id, id,
            "a late negative lookup must not erase an open module"
        );
    }

    #[test]
    fn publication_recheck_preserves_explicit_stubs_and_protected_builtins() {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("project");
        let custom = directory.path().join("custom");
        std::fs::create_dir_all(&custom).unwrap();
        let stub = custom.join("random.pyi");
        std::fs::write(&stub, "custom_value: int\n").unwrap();
        let db = Analysis::new(
            ModuleResolver::new(vec![project.clone()], Vec::new(), (3, 12))
                .with_stub_roots([custom]),
        );
        let selected = db.module("random").unwrap();
        db.set_file_text(&project.join("random.py"), "project_value = 1\n");
        let winner = db
            .publish_module_result("random", Some(&stub), None)
            .unwrap();
        assert!(Arc::ptr_eq(&winner, &selected));
        assert_eq!(winner.path, stub);

        let selected = db.builtins().unwrap();
        db.set_file_text(&project.join("builtins.py"), "project_value = 2\n");
        let winner = db
            .publish_module_result("builtins", Some(&selected.path), None)
            .unwrap();
        assert!(Arc::ptr_eq(&winner, &selected));
    }

    #[test]
    fn scoped_sdk_imports_keep_distinct_providers_and_snapshot_environments() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        for name in ["one", "two", "sdk-one", "sdk-two"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        let one = root.join("one/main.py");
        let two = root.join("two/main.py");
        let first_library = root.join("sdk-one/library.pyi");
        let second_library = root.join("sdk-two/library.pyi");
        for (path, text) in [
            (&one, "import library\n"),
            (&two, "import library\n"),
            (&first_library, "first = 1\n"),
            (&second_library, "second = 2\n"),
        ] {
            std::fs::write(path, text).unwrap();
        }
        let resolver = ModuleResolver::new(vec![root.to_path_buf()], Vec::new(), (3, 12))
            .with_module(
                root.join("one"),
                ModuleResolver::new(vec![root.join("one")], vec![root.join("sdk-one")], (3, 10)),
            )
            .with_module(
                root.join("two"),
                ModuleResolver::new(vec![root.join("two")], vec![root.join("sdk-two")], (3, 13)),
            );
        let database = Analysis::new(resolver);
        let first = database.file_for_path(&one).unwrap();
        let second = database.file_for_path(&two).unwrap();
        assert_eq!(
            database.module_relative(&first, 0, "library").unwrap().path,
            first_library
        );
        assert_eq!(
            database
                .module_relative(&second, 0, "library")
                .unwrap()
                .path,
            second_library
        );
        let first_context = database.module_from(&first, "library").unwrap();
        let second_context = database.module_from(&second, "library").unwrap();
        assert_eq!(
            first_context.static_env.as_ref().unwrap().python_version,
            (3, 10)
        );
        assert_eq!(
            second_context.static_env.as_ref().unwrap().python_version,
            (3, 13)
        );
        assert_eq!(first_context.module_name.as_deref(), Some("library"));
        assert_eq!(
            first_context.module_origin.as_deref(),
            Some(root.join("one").as_path())
        );
        let snapshot = database
            .snapshot(
                &[
                    one.clone(),
                    two.clone(),
                    first_library.clone(),
                    second_library.clone(),
                ],
                &rustpy_core::CancellationToken::new(),
            )
            .unwrap();
        assert_eq!(
            snapshot.analysis().resolver.for_path(&one).python_version,
            (3, 10)
        );
        assert_eq!(
            snapshot.analysis().resolver.for_path(&two).python_version,
            (3, 13)
        );
        std::fs::remove_file(&first_library).unwrap();
        assert_eq!(
            snapshot
                .analysis()
                .module_from(&first, "library")
                .unwrap()
                .path,
            first_library
        );
    }

    #[test]
    fn frozen_runtime_documentation_and_synthetic_ids_do_not_touch_live_vfs() {
        let directory = tempfile::TempDir::with_prefix("rustpy-frozen-source-docs").unwrap();
        let path = directory.path().join("library.py");
        std::fs::write(
            &path,
            "def original():\n    \"original docs\"\n    return 1\n",
        )
        .unwrap();
        let resolver = ModuleResolver::new(Vec::new(), vec![directory.path().into()], (3, 12));
        let db = Analysis::new(resolver);
        let frozen = db
            .snapshot(
                std::slice::from_ref(&path),
                &rustpy_core::CancellationToken::new(),
            )
            .unwrap();
        let original = frozen.analysis().file_for_path(&path).unwrap();
        assert!(!Arc::ptr_eq(&db.vfs, &frozen.analysis().vfs));
        let live_usage = db.vfs.cached_usage();
        std::fs::remove_file(&path).unwrap();
        assert!(crate::stub_docs::source_defines(
            frozen.analysis(),
            "library",
            "original"
        ));
        assert_eq!(
            crate::stub_docs::source_docstring(frozen.analysis(), "library", &["original".into()])
                .as_deref(),
            Some("original docs")
        );
        let builtins = frozen.analysis().builtins().unwrap();
        assert_ne!(builtins.id, original.id);
        assert_eq!(
            frozen.analysis().file(original.id).unwrap().text,
            original.text
        );
        assert_eq!(db.vfs.cached_usage(), live_usage);
        assert_eq!(frozen.analysis().vfs.cached_usage(), (0, 0));
        assert!(frozen.complete());
        assert!(frozen
            .analysis()
            .resolver
            .resolve_source("uncaptured")
            .is_none());
        assert!(!frozen.complete());
    }

    #[test]
    fn immutable_snapshot_survives_changed_disk_and_live_index_catalogue() {
        let directory = tempfile::TempDir::with_prefix("rustpy-analysis-snapshot").unwrap();
        let path = directory.path().join("a.py");
        std::fs::write(&path, "def first():\n    return 1\n").unwrap();
        let db = Analysis::for_project(directory.path());
        let frozen = db
            .snapshot(
                std::slice::from_ref(&path),
                &rustpy_core::CancellationToken::new(),
            )
            .unwrap();
        let original = frozen.analysis().file_for_path(&path).unwrap();
        std::fs::write(&path, "def second():\n    return 'changed'\n").unwrap();
        db.invalidate_path(&path);
        assert!(db.file_for_path(&path).unwrap().text.contains("second"));
        assert!(frozen
            .analysis()
            .file_for_path(&path)
            .unwrap()
            .text
            .contains("first"));
        assert_eq!(frozen.analysis().module("a").unwrap().id, original.id);
        let added = directory.path().join("new.py");
        std::fs::write(&added, "new = 2\n").unwrap();
        db.invalidate_path(&added);
        assert!(db.module("new").is_some());
        assert!(frozen.analysis().module("new").is_none());
        assert!(!frozen.complete());
    }
    #[test]
    fn frozen_relative_import_and_unicode_sources_keep_original_authority() {
        let directory =
            tempfile::TempDir::with_prefix("rustpy-analysis-relative-snapshot").unwrap();
        let package = directory.path().join("pkg");
        std::fs::create_dir(&package).unwrap();
        let init = package.join("__init__.py");
        let module = package.join("π.py");
        std::fs::write(&init, "from .π import value\n").unwrap();
        std::fs::write(&module, "value = 1\n").unwrap();
        let db = Analysis::for_project(directory.path());
        let frozen = db
            .snapshot(
                &[init.clone(), module.clone()],
                &rustpy_core::CancellationToken::new(),
            )
            .unwrap();
        let from = frozen.analysis().file_for_path(&init).unwrap();
        std::fs::remove_file(&module).unwrap();
        let target = frozen.analysis().module_relative(&from, 1, "π").unwrap();
        assert_eq!(&*target.text, "value = 1\n");
        assert!(frozen.complete());
        assert!(frozen
            .analysis()
            .current_text(&package.join("uncaptured.py"))
            .is_none());
        assert!(!frozen.complete());
    }

    #[test]
    fn batch_reads_trust_a_recent_content_check_until_the_guard_drops() {
        let dir = tempfile::TempDir::with_prefix("rustpy-batch-reads").unwrap();
        let path = dir.path().join("m.py");
        std::fs::write(&path, "x=1\n").unwrap();
        let db = Analysis::for_project(dir.path());
        let id = db.file_for_path(&path).unwrap().id;
        let first = db.file(id).unwrap();
        std::fs::write(&path, "x=2\n").unwrap();
        {
            let _batch = db.batch_reads();
            // Verified moments ago: no re-read while the batch pass runs.
            assert!(Arc::ptr_eq(&first, &db.file(id).unwrap()));
            assert!(Arc::ptr_eq(&first, &db.file_for_path(&path).unwrap()));
        }
        assert_eq!(&*db.file(id).unwrap().text, "x=2\n");
    }

    #[test]
    fn ids_survive_close_and_content_revisions_ignore_restored_metadata() {
        let dir = tempfile::TempDir::with_prefix("rustpy-vfs-db").unwrap();
        let path = dir.path().join("m.py");
        std::fs::write(&path, "x=1\n").unwrap();
        let db = Analysis::for_project(dir.path());
        let id = db.set_file_text(&path, "editor=1\n");
        assert_eq!(db.vfs.read(&path, db.read_limits()).unwrap().id, id);
        db.close_file(&dir.path().join("./m.py"));
        let first = db.file(id).unwrap();
        assert_eq!(&*first.text, "x=1\n");
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, "x=2\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(mtime))
            .unwrap();
        let second = db.file(id).unwrap();
        assert_eq!(first.id, second.id);
        assert_ne!(first.disk_revision, second.disk_revision);
        assert_eq!(&*second.text, "x=2\n");
        assert_eq!(
            db.set_file_text(&dir.path().join("./m.py"), "editor=2\n"),
            id
        );
        assert_eq!(db.current_text(&path).as_deref(), Some("editor=2\n"));
    }

    #[test]
    fn closed_and_open_sources_share_decoding_and_report_errors() {
        let dir = tempfile::TempDir::with_prefix("rustpy-encoding-db").unwrap();
        let path = dir.path().join("m.py");
        let db = Analysis::for_project(dir.path());
        for bytes in [
            b"# coding: latin-1\n\xe9 = 1\n".as_slice(),
            b"# coding: latin-1\n\xc2\xaa = 1\n",
            b"\xef\xbb\xbf\xc3\xa9 = 1\n",
        ] {
            std::fs::write(&path, bytes).unwrap();
            let expected = rustpy_text::Encoding::decode_python_source(bytes).unwrap();
            let closed = db.file_for_path(&path).unwrap();
            assert_eq!(&*closed.text, expected);
            assert_eq!(db.current_text(&path).as_deref(), Some(expected.as_str()));
            let id = db.set_file_text(&path, &expected);
            assert_eq!(id, closed.id);
            let open = db.file(id).unwrap();
            assert_eq!(
                open.scopes
                    .scope(open.scopes.module_scope())
                    .bindings
                    .keys()
                    .collect::<Vec<_>>(),
                closed
                    .scopes
                    .scope(closed.scopes.module_scope())
                    .bindings
                    .keys()
                    .collect::<Vec<_>>()
            );
            db.close_file(&path);
            assert_eq!(&*db.file(id).unwrap().text, expected);
        }
        std::fs::write(&path, b"# coding: utf-8\n\xff").unwrap();
        assert!(db.file_for_path(&path).is_none());
        assert!(db.current_text(&path).is_none());
        assert!(db.file_encoding_error(&path).is_some());
        db.set_file_text(&path, "editor = 1\n");
        assert_eq!(db.current_text(&path).as_deref(), Some("editor = 1\n"));
        assert!(db.file_encoding_error(&path).is_none());
    }

    #[test]
    fn loads_builtins_and_open_files() {
        let db = Analysis::for_project(Path::new("/nonexistent-root"));
        let b = db.builtins().unwrap();
        assert!(b.is_stub);
        assert!(b
            .scopes
            .scope(b.scopes.module_scope())
            .bindings
            .contains_key("int"));
        assert!(b
            .scopes
            .scope(b.scopes.module_scope())
            .bindings
            .contains_key("len"));
        let id = db.set_file_text(Path::new("/nonexistent-root/m.py"), "x = 1\n");
        let f = db.file(id).unwrap();
        assert_eq!(f.module_name.as_deref(), Some("m"));
        assert!(db.module("m").is_some());
        let os_path = db.module("os.path").unwrap();
        assert!(os_path.path.starts_with("<typeshed>"));
        // relative import inside a stub package
        let rel = db.module_relative(&os_path, 1, "").unwrap();
        assert_eq!(rel.module_name.as_deref(), Some("os"));
    }

    #[test]
    fn evicted_same_metadata_edits_change_world_revision_and_limits_are_caller_controlled() {
        let dir = tempfile::TempDir::with_prefix("rustpy-vfs-evicted").unwrap();
        let path = dir.path().join("m.py");
        std::fs::write(&path, "x=1\n").unwrap();
        let db = Analysis::for_project(dir.path());
        db.set_read_limits(ReadLimits {
            max_file_bytes: 10,
            max_cached_bytes: 0,
            max_cached_files: 0,
        });
        let first = db.file_for_path(&path).unwrap();
        let revision = db.revision_excluding(Path::new("/other.py"));
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, "x=2\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(mtime))
            .unwrap();
        assert_eq!(&*db.file(first.id).unwrap().text, "x=2\n");
        assert_ne!(db.revision_excluding(Path::new("/other.py")), revision);
        db.set_read_limits(ReadLimits {
            max_file_bytes: 2,
            max_cached_bytes: 0,
            max_cached_files: 0,
        });
        assert!(db.current_text(&path).is_none());
    }

    #[test]
    fn closed_files_follow_the_disk() {
        let dir = tempfile::TempDir::with_prefix("rustpy-db").unwrap();
        let path = dir.path().join("util.py");
        std::fs::write(&path, "def f():\n    pass\n").unwrap();
        let db = Analysis::for_project(dir.path());
        let first = db.file_for_path(&path).unwrap();
        assert!(first.disk_stamp.is_some() && !first.is_open);
        // Unchanged on disk: the cached copy is served.
        assert!(Arc::ptr_eq(&first, &db.file_for_path(&path).unwrap()));
        // Rewritten outside the editor: re-read under the same id.
        std::fs::write(&path, "import os\n\n\ndef f():\n    return os.sep\n").unwrap();
        let second = db.file_for_path(&path).unwrap();
        assert_eq!(
            &*second.text,
            "import os\n\n\ndef f():\n    return os.sep\n"
        );
        assert_eq!(second.id, first.id);
        assert_eq!(db.file(first.id).unwrap().text, second.text);
        // Module lookups see the new text once the path is invalidated.
        std::fs::write(&path, "X = 1\n").unwrap();
        db.invalidate_path(&path);
        assert_eq!(&*db.module("util").unwrap().text, "X = 1\n");
        // Deleted: forgotten.
        std::fs::remove_file(&path).unwrap();
        assert!(db.file_for_path(&path).is_none());
        db.invalidate_path(&path);
        assert!(db.module("util").is_none());
    }

    #[test]
    fn failed_module_lookups_recover_when_the_file_appears() {
        let dir = tempfile::TempDir::with_prefix("rustpy-db").unwrap();
        let db = Analysis::for_project(dir.path());
        assert!(db.module("late").is_none());
        assert!(db.module("late").is_none(), "cached miss");
        let path = dir.path().join("late.py");
        std::fs::write(&path, "VALUE = 2\n").unwrap();
        db.invalidate_path(&path);
        assert_eq!(
            &*db.module("late")
                .expect("resolvable after the file appeared")
                .text,
            "VALUE = 2\n"
        );
        // A new package directory is picked up as well.
        assert!(db.module("pkg.mod").is_none());
        std::fs::create_dir_all(dir.path().join("pkg")).unwrap();
        std::fs::write(dir.path().join("pkg/__init__.py"), "").unwrap();
        std::fs::write(dir.path().join("pkg/mod.py"), "Y = 3\n").unwrap();
        assert!(db.module("pkg.mod").is_none(), "cached miss");
        db.invalidate_path(&dir.path().join("pkg"));
        assert_eq!(&*db.module("pkg.mod").unwrap().text, "Y = 3\n");
        // A namespace package that gains an `__init__.py` stops being a placeholder.
        std::fs::create_dir_all(dir.path().join("ns")).unwrap();
        std::fs::write(dir.path().join("ns/m.py"), "").unwrap();
        assert_eq!(&*db.module("ns").unwrap().text, "");
        std::fs::write(dir.path().join("ns/__init__.py"), "Z = 4\n").unwrap();
        db.invalidate_disk_files();
        assert_eq!(&*db.module("ns").unwrap().text, "Z = 4\n");
    }

    #[test]
    fn evicted_copies_still_held_keep_their_identity_until_the_disk_changes() {
        let dir = tempfile::TempDir::with_prefix("rustpy-db-held").unwrap();
        for i in 0..8 {
            std::fs::write(dir.path().join(format!("m{i}.py")), format!("V{i} = {i}\n")).unwrap();
        }
        let db = Analysis::for_project(dir.path());
        db.set_cache_limits(CacheLimits {
            max_files: 2,
            max_bytes: 1 << 20,
        });
        let held = db.file_for_path(&dir.path().join("m0.py")).unwrap();
        for i in 1..8 {
            db.file_for_path(&dir.path().join(format!("m{i}.py")))
                .unwrap();
        }
        let (count, _) = db.cached_disk_files();
        assert!(count <= 2, "{count}");
        // Dropped from the cache, but an analysis still holds it: no new parse.
        assert!(Arc::ptr_eq(&held, &db.file(held.id).unwrap()));
        std::fs::write(dir.path().join("m0.py"), "V0 = 'changed'\n").unwrap();
        let reread = db.file(held.id).unwrap();
        assert!(!Arc::ptr_eq(&held, &reread));
        assert_eq!(&*reread.text, "V0 = 'changed'\n");
    }

    #[test]
    fn closed_file_cache_is_bounded_and_ids_survive_eviction() {
        let dir = tempfile::TempDir::with_prefix("rustpy-db").unwrap();
        for i in 0..12 {
            std::fs::write(
                dir.path().join(format!("m{i}.py")),
                format!("VALUE_{i} = {i}\n"),
            )
            .unwrap();
        }
        let db = Analysis::for_project(dir.path());
        db.set_cache_limits(CacheLimits {
            max_files: 5,
            max_bytes: 1 << 20,
        });
        let open = dir.path().join("m0.py");
        db.set_file_text(&open, "OPEN = 0\n");
        let first = db.file_for_path(&dir.path().join("m1.py")).unwrap();
        let first_id = first.id;
        drop(first);
        let ids: Vec<FileId> = (1..12)
            .map(|i| {
                db.file_for_path(&dir.path().join(format!("m{i}.py")))
                    .unwrap()
                    .id
            })
            .collect();
        assert_eq!(ids[0], first_id);
        let (count, _) = db.cached_disk_files();
        assert!(count <= 5, "{count} disk copies cached");
        // The open file is never dropped.
        assert_eq!(&*db.file_for_path(&open).unwrap().text, "OPEN = 0\n");
        // m1 was the least recently used: dropped, yet its id still resolves
        // (re-read under the same id) by id, by path and by module name.
        let again = db.file(first_id).expect("evicted ids reload");
        assert_eq!((again.id, &*again.text), (first_id, "VALUE_1 = 1\n"));
        assert_eq!(
            db.file_for_path(&dir.path().join("m1.py")).unwrap().id,
            first_id
        );
        assert_eq!(db.module("m1").unwrap().id, first_id);
        // Every id handed out stays valid.
        for (i, id) in ids.iter().enumerate() {
            assert_eq!(
                &*db.file(*id).unwrap().text,
                format!("VALUE_{} = {}\n", i + 1, i + 1),
                "id of m{}",
                i + 1
            );
        }
        assert!(db.cached_disk_files().0 <= 5);
        // Invalidating everything forgets evicted entries too.
        db.invalidate_disk_files();
        assert_eq!(db.cached_disk_files(), (0, 0));
        assert!(db.file(first_id).is_none());
        // A byte budget bounds the cache as well.
        db.set_cache_limits(CacheLimits {
            max_files: 100,
            max_bytes: 40,
        });
        for i in 1..12 {
            db.file_for_path(&dir.path().join(format!("m{i}.py")))
                .unwrap();
        }
        assert!(
            db.cached_disk_files().1 <= 40,
            "{:?}",
            db.cached_disk_files()
        );
    }

    /// `cached_disk_files` from a full scan, to check the running totals.
    fn recount(db: &Analysis) -> (usize, usize) {
        let files = db.files.read();
        files
            .by_id
            .values()
            .filter(|f| evictable(f))
            .fold((0, 0), |(n, b), f| (n + 1, b + f.text.len()))
    }

    #[test]
    fn cache_totals_follow_every_change() {
        let dir = tempfile::TempDir::with_prefix("rustpy-db").unwrap();
        for i in 0..8 {
            std::fs::write(dir.path().join(format!("t{i}.py")), "X = 1\n".repeat(i + 1)).unwrap();
        }
        let db = Analysis::for_project(dir.path());
        db.set_cache_limits(CacheLimits {
            max_files: 4,
            max_bytes: 1 << 20,
        });
        let p = |i: usize| dir.path().join(format!("t{i}.py"));
        let check = |what: &str| assert_eq!(db.cached_disk_files(), recount(&db), "after {what}");
        for i in 0..8 {
            db.file_for_path(&p(i)).unwrap();
            check("a load");
        }
        assert!(db.cached_disk_files().0 <= 4);
        // An editor takes over a cached disk copy, then lets it go.
        db.file_for_path(&p(7)).unwrap();
        db.set_file_text(&p(7), "EDITED = 1\n");
        check("opening a cached file");
        db.close_file(&p(7));
        check("closing it");
        db.module("typing").unwrap();
        check("a bundled stub");
        std::fs::write(p(6), "CHANGED = 2\n").unwrap();
        db.file_for_path(&p(6)).unwrap();
        check("a re-read after a change on disk");
        db.invalidate_path(&p(5));
        check("invalidating a file");
        db.invalidate_disk_files();
        check("invalidating everything");
        assert_eq!(db.cached_disk_files(), (0, 0));
    }

    #[test]
    fn evicted_file_deleted_from_disk_is_forgotten() {
        let dir = tempfile::TempDir::with_prefix("rustpy-db").unwrap();
        for i in 0..4 {
            std::fs::write(dir.path().join(format!("d{i}.py")), "X = 1\n").unwrap();
        }
        let db = Analysis::for_project(dir.path());
        db.set_cache_limits(CacheLimits {
            max_files: 1,
            max_bytes: 1 << 20,
        });
        let id0 = db.file_for_path(&dir.path().join("d0.py")).unwrap().id;
        for i in 1..4 {
            db.file_for_path(&dir.path().join(format!("d{i}.py")))
                .unwrap();
        }
        std::fs::remove_file(dir.path().join("d0.py")).unwrap();
        assert!(db.file(id0).is_none());
        assert!(db.file_for_path(&dir.path().join("d0.py")).is_none());
        assert!(db.module("d0").is_none());
    }

    #[test]
    fn library_sources_skip_rereads_only_after_a_settled_content_match() {
        let dir = tempfile::TempDir::with_prefix("library-stamp").unwrap();
        let site = dir.path().join("site-packages");
        std::fs::create_dir_all(&site).unwrap();
        let path = site.join("lib.py");
        std::fs::write(&path, "x = 1\n").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
        let set_old = || {
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(old))
                .unwrap();
        };
        set_old();
        let db = Analysis::new(ModuleResolver::new(
            vec![dir.path().join("project")],
            vec![site.clone()],
            (3, 12),
        ));
        let file = db.file_for_path(&path).unwrap();
        // The first settled comparison reads the content; later ones trust the stamp.
        assert!(!file.stable_stamp.load(Ordering::Relaxed));
        assert!(Arc::ptr_eq(&db.file(file.id).unwrap(), &file));
        assert!(file.stable_stamp.load(Ordering::Relaxed));
        assert!(Arc::ptr_eq(&db.file(file.id).unwrap(), &file));
        // A changed size is still seen at once.
        std::fs::write(&path, "x = 22\n").unwrap();
        set_old();
        assert_eq!(&*db.file(file.id).unwrap().text, "x = 22\n");
    }

    #[test]
    fn revisions_track_changes_made_by_other_files() {
        let dir = tempfile::TempDir::with_prefix("rustpy-db").unwrap();
        let (a, b, c) = (
            dir.path().join("a.py"),
            dir.path().join("b.py"),
            dir.path().join("c.py"),
        );
        std::fs::write(&c, "C = 1\n").unwrap();
        let db = Analysis::for_project(dir.path());
        db.set_file_text(&a, "A = 1\n");
        let (a0, b0) = (db.revision_excluding(&a), db.revision_excluding(&b));
        // A's own edits leave A's revision alone but change everyone else's.
        db.set_file_text(&a, "A = 2\n");
        db.set_file_text(&a, "A = 3\n");
        assert_eq!(db.revision_excluding(&a), a0);
        assert_ne!(db.revision_excluding(&b), b0);
        // Pushing the same text again is not a change.
        let b1 = db.revision_excluding(&b);
        db.set_file_text(&a, "A = 3\n");
        assert_eq!(db.revision_excluding(&b), b1);
        // B's edit changes A's revision.
        db.set_file_text(&b, "B = 1\n");
        let a1 = db.revision_excluding(&a);
        assert_ne!(a1, a0);
        db.set_file_text(&a, "A = 4\n");
        assert_eq!(
            db.revision_excluding(&a),
            a1,
            "A's edits after B's still leave A's revision"
        );
        // Disk and lookup changes change every revision.
        for change in [0, 1, 2] {
            let (ra, rb) = (db.revision_excluding(&a), db.revision_excluding(&b));
            match change {
                0 => db.invalidate_path(&c),
                1 => db.invalidate_disk_files(),
                _ => db.close_file(&b),
            }
            assert!(
                db.revision_excluding(&a) != ra && db.revision_excluding(&b) != rb,
                "change {change}"
            );
        }
        // A disk file re-read after it changed is a change too.
        db.file_for_path(&c).unwrap();
        let ra = db.revision_excluding(&a);
        std::fs::write(&c, "C = 22\n").unwrap();
        assert_eq!(&*db.file_for_path(&c).unwrap().text, "C = 22\n");
        assert_ne!(db.revision_excluding(&a), ra);
    }

    #[test]
    fn current_text_does_not_parse_uncached_files() {
        let dir = tempfile::TempDir::with_prefix("rustpy-db").unwrap();
        let path = dir.path().join("big.py");
        std::fs::write(&path, "def f():\n    return 1\n").unwrap();
        let db = Analysis::for_project(dir.path());
        assert_eq!(
            db.current_text(&path).as_deref(),
            Some("def f():\n    return 1\n")
        );
        assert_eq!(db.cached_disk_files().0, 0, "nothing was loaded");
        db.set_file_text(&path, "edited = True\n");
        assert_eq!(db.current_text(&path).as_deref(), Some("edited = True\n"));
        assert!(db.current_text(&dir.path().join("missing.py")).is_none());
    }

    #[test]
    fn concurrent_loads_converge_on_one_copy_per_file() {
        let dir = tempfile::TempDir::with_prefix("rustpy-db").unwrap();
        for i in 0..20 {
            std::fs::write(
                dir.path().join(format!("c{i}.py")),
                format!(
                    "import os\nfrom c{} import *\nV{i} = os.sep\n",
                    (i + 1) % 20
                ),
            )
            .unwrap();
        }
        let db = Arc::new(Analysis::for_project(dir.path()));
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let db = db.clone();
                let barrier = barrier.clone();
                let root = dir.path().to_path_buf();
                std::thread::spawn(move || {
                    barrier.wait();
                    let mut seen = Vec::new();
                    for k in 0..20 {
                        let i = (k + t * 3) % 20;
                        let f = if k % 2 == 0 {
                            db.file_for_path(&root.join(format!("c{i}.py")))
                        } else {
                            db.module(&format!("c{i}"))
                        };
                        let f = f.unwrap();
                        seen.push((f.path.clone(), f.id));
                        seen.push((
                            db.module("typing").unwrap().path.clone(),
                            db.module("typing").unwrap().id,
                        ));
                    }
                    seen
                })
            })
            .collect();
        let mut ids: FxHashMap<PathBuf, FileId> = FxHashMap::default();
        for t in threads {
            for (path, id) in t.join().unwrap() {
                let first = *ids.entry(path.clone()).or_insert(id);
                assert_eq!(first, id, "two ids for {}", path.display());
            }
        }
        for (path, id) in &ids {
            assert_eq!(
                db.file_for_path(path)
                    .map(|f| f.id)
                    .or_else(|| db.file(*id).map(|f| f.id)),
                Some(*id)
            );
        }
    }

    /// Runs each of `workers` in a loop on its own thread (passing the round
    /// number) until `run_for` has passed, then stops them; panics when they
    /// do not all finish their round soon after (a deadlock).
    /// A worker of [`hammer`]: called with the round number.
    type Worker = Box<dyn Fn(&Analysis, usize) + Send>;

    fn hammer(db: &Arc<Analysis>, run_for: std::time::Duration, workers: Vec<Worker>) {
        use std::sync::atomic::AtomicBool;
        let stop = Arc::new(AtomicBool::new(false));
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let count = workers.len();
        for work in workers {
            let (db, stop, done_tx) = (db.clone(), stop.clone(), done_tx.clone());
            std::thread::spawn(move || {
                let mut round = 0;
                while !stop.load(Ordering::Relaxed) {
                    work(&db, round);
                    round += 1;
                }
                let _ = done_tx.send(round);
            });
        }
        std::thread::sleep(run_for);
        stop.store(true, Ordering::Relaxed);
        let grace = std::time::Duration::from_secs(30);
        for k in 0..count {
            let rounds = done_rx.recv_timeout(grace).unwrap_or_else(|_| {
                panic!(
                    "deadlock: {} of {count} workers still blocked after {grace:?}",
                    count - k
                )
            });
            assert!(rounds > 0, "a worker never finished a round");
        }
    }

    /// Namespace-package and stub lookups take the database lock twice
    /// (a cached virtual file is found by path, then fetched by id). Doing
    /// that while still holding the first read guard deadlocked as soon as
    /// a writer queued in between (parking_lot blocks new readers behind
    /// it): background inspection and usage searches hung the whole IDE.
    #[test]
    fn virtual_file_lookups_racing_writers_never_deadlock() {
        let dir = tempfile::TempDir::with_prefix("rustpy-db").unwrap();
        // `pkg` has no `__init__.py`: a namespace package of modules importing each other.
        let pkg = dir.path().join("pkg");
        std::fs::create_dir_all(&pkg).unwrap();
        const N: usize = 40;
        for k in 0..N {
            std::fs::write(
                pkg.join(format!("m{k}.py")),
                format!(
                    "from . import m{}\nfrom .m{} import VALUE\nimport os\nVALUE = {k}\n",
                    (k + 1) % N,
                    (k + 2) % N
                ),
            )
            .unwrap();
        }
        let db = Arc::new(Analysis::for_project(dir.path()));
        // A small cache: evicted modules are re-read by `file()` under the same id.
        db.set_cache_limits(CacheLimits {
            max_files: 8,
            max_bytes: 1 << 20,
        });
        let reader = |db: &Analysis, round: usize| {
            assert!(db.module("pkg").is_some(), "namespace package");
            assert!(db.module("os").is_some(), "stub");
            let m = db
                .module(&format!("pkg.m{}", round % N))
                .expect("module of the namespace package");
            assert!(db.module_relative(&m, 1, "").is_some(), "from . import");
            assert!(
                db.module_relative(&m, 1, &format!("m{}", (round + 2) % N))
                    .is_some(),
                "from .m import"
            );
        };
        let editor = dir.path().join("editor.py");
        let unrelated = dir.path().join("not_there.py");
        let writer = move |db: &Analysis, round: usize| {
            // Typing in an editor, and file-system events that drop the
            // module lookups (a file the analysis never read: structural).
            db.set_file_text(&editor, &format!("X = {round}\n"));
            if round.is_multiple_of(4) {
                db.invalidate_path(&unrelated);
            }
        };
        let mut workers: Vec<Worker> = Vec::new();
        for _ in 0..6 {
            workers.push(Box::new(reader));
        }
        for _ in 0..2 {
            workers.push(Box::new(writer.clone()));
        }
        hammer(&db, std::time::Duration::from_secs(3), workers);
        let (count, bytes) = db.cached_disk_files();
        assert_eq!((count, bytes), recount(&db));
    }

    #[test]
    fn editor_text_wins_until_closed() {
        let dir = tempfile::TempDir::with_prefix("rustpy-db").unwrap();
        let path = dir.path().join("m.py");
        std::fs::write(&path, "on_disk = 1\n").unwrap();
        let db = Analysis::for_project(dir.path());
        db.set_file_text(&path, "unsaved = 2\n");
        db.invalidate_path(&path);
        db.invalidate_disk_files();
        assert_eq!(&*db.file_for_path(&path).unwrap().text, "unsaved = 2\n");
        assert_eq!(&*db.module("m").unwrap().text, "unsaved = 2\n");
        // Closing the tab without saving falls back to the disk text.
        db.close_file(&path);
        assert_eq!(&*db.file_for_path(&path).unwrap().text, "on_disk = 1\n");
        assert_eq!(&*db.module("m").unwrap().text, "on_disk = 1\n");
    }
}

#[cfg(test)]
mod candidate_tests {
    use super::*;

    #[test]
    fn candidate_reparses_without_publishing_or_reusing_version_caches() {
        let dir = tempfile::TempDir::with_prefix("rustpy-candidate").unwrap();
        let path = dir.path().join("candidate.py");
        let db = Analysis::for_project(dir.path());
        let id = db.set_file_text(&path, "old = 1\nprint(old)\n");
        let live = db.file(id).unwrap();
        let revision = db.revision_excluding(&dir.path().join("other.py"));
        let old_flow = crate::flow::flow_of(&live, crate::scope::ScopeId(0));
        live.deprecated_names.set(vec!["old".into()]).unwrap();
        let candidate = live.with_candidate_text("new = 2\nprint(new)\n".into());
        assert_eq!(candidate.id, id);
        assert_eq!(candidate.path, live.path);
        assert_eq!(candidate.module_name, live.module_name);
        assert_eq!(candidate.is_open, live.is_open);
        assert_eq!(candidate.static_env, live.static_env);
        assert!(candidate.disk_stamp.is_none() && candidate.disk_revision.is_none());
        assert!(candidate.deprecated_names.get().is_none());
        assert_eq!(
            candidate.parse.syntax_node().text().to_string(),
            "new = 2\nprint(new)\n"
        );
        let flow = crate::flow::flow_of(&candidate, crate::scope::ScopeId(0));
        assert!(!Arc::ptr_eq(&old_flow, &flow));
        assert!(candidate
            .scopes
            .bindings
            .iter()
            .any(|binding| binding.name == "new"));
        assert!(!candidate
            .scopes
            .bindings
            .iter()
            .any(|binding| binding.name == "old"));
        assert!(Arc::ptr_eq(&live, &db.file(id).unwrap()));
        assert_eq!(
            db.current_text(&path).as_deref(),
            Some("old = 1\nprint(old)\n")
        );
        assert_eq!(
            db.revision_excluding(&dir.path().join("other.py")),
            revision
        );
    }

    #[test]
    fn candidate_retains_stub_interpreter_and_module_context() {
        let env = crate::static_cond::StaticEnv::new((3, 10), "win32");
        let live = FileInfo::new(
            FileId(17),
            PathBuf::from("package/__init__.pyi"),
            Some("package".into()),
            "OLD = 1\n".into(),
            true,
            false,
            &env,
        );
        let text =
            "import sys\nif sys.platform == 'win32':\n    WINDOWS = 1\nelse:\n    UNIX = 1\n";
        let candidate = live.with_candidate_text(text.into());
        assert_eq!(candidate.static_env.as_ref(), Some(&env));
        assert!(candidate.is_stub && candidate.is_package_init());
        assert!(!candidate.is_open);
        assert_eq!(candidate.module_name.as_deref(), Some("package"));
        assert!(candidate
            .scopes
            .bindings
            .iter()
            .any(|binding| binding.name == "WINDOWS"));
        assert!(!candidate
            .scopes
            .bindings
            .iter()
            .any(|binding| binding.name == "UNIX"));
        assert_eq!(&*live.text, "OLD = 1\n");
    }
    #[test]
    fn import_graph_follows_aliases_stars_and_retires_changed_sources() {
        let db = Analysis::for_project(Path::new("/import-index"));
        let base = db.set_file_text(Path::new("/import-index/base.py"), "value=1\n");
        let child = db.set_file_text(
            Path::new("/import-index/child.py"),
            "from base import value as renamed\n",
        );
        let star = db.set_file_text(Path::new("/import-index/star.py"), "from base import *\n");
        let mut importers: Vec<_> = db.importers_of(base).iter().map(|file| file.id.0).collect();
        importers.sort_unstable();
        let mut expected = vec![child.0, star.0];
        expected.sort_unstable();
        assert_eq!(importers, expected);
        db.set_file_text(Path::new("/import-index/child.py"), "renamed=2\n");
        assert_eq!(
            db.importers_of(base)
                .iter()
                .map(|file| file.id)
                .collect::<Vec<_>>(),
            [star]
        );
    }

    #[test]
    fn browsing_bundled_stub_does_not_replace_user_resolution() {
        let dir = tempfile::TempDir::with_prefix("rustpy-user-stubs").unwrap();
        std::fs::write(dir.path().join("typing.pyi"), "user_marker: int\n").unwrap();
        let db = Analysis::new(ModuleResolver::new(Vec::new(), Vec::new(), (3, 12)))
            .with_stub_roots([dir.path().to_path_buf()]);
        let user = db.module("typing").unwrap();
        let bundled = db
            .file_for_path(Path::new("<typeshed>/typing.pyi"))
            .unwrap();
        assert_ne!(user.id, bundled.id);
        assert!(bundled.text.contains("TypeVar"));
        assert_eq!(db.module("typing").unwrap().id, user.id);
        assert!(db
            .file_for_path(Path::new("<typeshed>/../typing.pyi"))
            .is_none());
    }
}

#[cfg(all(test, unix))] // binds a Unix interpreter and extension fixtures
#[path = "native_skeleton_connected_tests.rs"]
mod native_skeleton_connected_tests;
