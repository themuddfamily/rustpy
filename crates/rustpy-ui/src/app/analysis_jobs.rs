//! Background analysis of the active editor (roadmap §2.2 "Performance").
//!
//! Inspections, grammar and spelling checks, docstring and regex checks and
//! inlay hints used to run on the UI thread 0.3 s after the last keystroke;
//! they now run on a worker (a hidden background task). An edit cancels the
//! run it makes obsolete — the token is checked between inspections — and
//! finished results are cached by file, content hash, settings and the
//! analysis database's revision of everything else, so undoing an edit (or
//! coming back to any analysed state) shows its results at once. The cache
//! holds at most [`CACHE_CAPACITY`] results and [`CACHE_BYTES`] retained bytes
//! (least recently used first out).

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Weak;
use std::time::Instant;

use rustpy_core::{CancellationToken, Cancelled, TaskError, TaskHandle, TaskManager};
use rustpy_semantic::FileInfo;

use super::*;

/// `semantic_version` of a tab whose analysis runs in the background. It is
/// never a document version, so the editor shows syntax errors meanwhile
/// (as it always did between an edit and the analysis), and a reset to
/// `None` while the run is in flight marks its result stale.
pub(super) const PENDING: u64 = u64::MAX;

/// Analysis results kept by the cache.
const CACHE_CAPACITY: usize = 48;
/// Retained result payloads, including quick-fix replacement strings.
const CACHE_BYTES: usize = 32 * 1024 * 1024;

/// Quiet time after the last edit before the analysis starts (seconds).
pub(super) const DEBOUNCE: f64 = 0.3;

/// Everything the editor shows from one analysis run.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct FileAnalysis {
    pub diagnostics: Vec<rustpy_core::Diagnostic>,
    pub quick_fixes: QuickFixes,
    pub inlay_hints: Vec<features::InlayHint>,
}

/// What a run depends on besides the file and the analysis database.
#[derive(Clone)]
pub(super) struct AnalysisInputs {
    pub inspections: InspectionOptions,
    pub disabled_inspections: Vec<String>,
    pub grammar: bool,
    pub grammar_policy: Option<rustpy_grammar::policy::Policy>,
    /// The spelling dictionary, when spell checking is on.
    pub dictionary: Option<Arc<rustpy_spell::Dictionary>>,
    pub parameter_hints: bool,
    pub type_hints: bool,
    /// The symbol indexes: "Import" fixes from installed libraries.
    pub index: crate::symbol_index::IndexSnapshot,
}

impl AnalysisInputs {
    /// Hash of the settings that change the results; the shared symbol list
    /// and dictionary are compared by identity ([`SharedId`]).
    pub fn fingerprint(&self) -> u64 {
        // No `..`: a new option must be added here (or knowingly ignored).
        let InspectionOptions {
            policy,
            max_line_length,
            editorconfig_enabled,
            editorconfig,
            style,
            structural_rules,
            framework,
            pep8,
            unused_imports,
            unused_variables,
            project_symbols: _,
            type_checker,
            security,
            compatibility,
            code_smells,
            naming,
            missing_docstrings,
            incorrect_docstrings,
            deprecation,
            probable_bugs,
            python_version,
        } = &self.inspections;
        let mut h = DefaultHasher::new();
        policy.hash(&mut h);
        style.hash(&mut h);
        editorconfig_enabled.hash(&mut h);
        editorconfig.hash(&mut h);
        framework.hash(&mut h);
        rustpy_search::structural::library_key(structural_rules.as_ref()).hash(&mut h);
        (
            max_line_length,
            pep8,
            unused_imports,
            unused_variables,
            type_checker,
            security,
            compatibility,
        )
            .hash(&mut h);
        (
            code_smells,
            naming,
            missing_docstrings,
            incorrect_docstrings,
            deprecation,
            probable_bugs,
            python_version,
        )
            .hash(&mut h);
        let mut disabled = self
            .disabled_inspections
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        disabled.sort_unstable();
        disabled.dedup();
        disabled.hash(&mut h);
        self.index.generation.hash(&mut h);
        (
            self.grammar,
            self.dictionary.is_some(),
            self.parameter_hints,
            self.type_hints,
        )
            .hash(&mut h);
        if let Some(policy) = self.grammar_policy {
            policy.language.key().hash(&mut h);
            for category in rustpy_grammar::policy::Category::ALL {
                policy.enabled(category).hash(&mut h);
            }
        }
        self.grammar_policy.is_some().hash(&mut h);
        h.finish()
    }
}

/// Identity of a shared input (`Arc`) in a cache key. It holds a `Weak`, so
/// the allocation outlives the key and a later `Arc` cannot reuse its
/// address while the key exists (only the counters and the value's inline
/// part stay allocated; a `Vec`'s or a dictionary's heap data is freed).
#[derive(Clone, Debug)]
struct SharedId<T>(Weak<T>);

impl<T> SharedId<T> {
    fn of(arc: &Arc<T>) -> Self {
        Self(Arc::downgrade(arc))
    }
}

impl<T> PartialEq for SharedId<T> {
    fn eq(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.0, &other.0)
    }
}

impl<T> Eq for SharedId<T> {}

impl<T> Hash for SharedId<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self.0.as_ptr() as *const () as usize).hash(state);
    }
}

/// Runs every per-file check on `file`, stopping with `Err(Cancelled)` at
/// the next check of `token` once it is cancelled. Inference started by the
/// checks stops as soon as the token is cancelled too, so an edit ends the
/// obsolete run within milliseconds rather than after its current check.
pub(super) fn analyze_file(
    analysis: &Analysis,
    file: &Arc<FileInfo>,
    inputs: &AnalysisInputs,
    token: &CancellationToken,
) -> Result<FileAnalysis, Cancelled> {
    rustpy_semantic::infer::with_cancellation(token, || {
        analyze_file_inner(analysis, file, inputs, token)
    })
}

fn analyze_file_inner(
    analysis: &Analysis,
    file: &Arc<FileInfo>,
    inputs: &AnalysisInputs,
    token: &CancellationToken,
) -> Result<FileAnalysis, Cancelled> {
    use rustpy_analysis::file_analysis::{
        self, DictionaryActionPolicy, FileAnalysisOptions, StubSupplementPolicy,
    };
    let mut inspection_options = inputs.inspections.clone();
    if let (Some(root), Some(interpreter)) = (
        &inputs.inspections.framework.root,
        &inputs.inspections.framework.interpreter,
    ) {
        let catalog = rustpy_project::framework_catalog::capture(
            root,
            interpreter,
            analysis.revision_excluding(std::path::Path::new("<framework>")),
            &file.path,
            inputs.inspections.framework.trusted,
            token,
        );
        let mut framework = (*inputs.inspections.framework).clone();
        apply_framework_catalog(&mut framework, catalog);
        inspection_options.framework = Arc::new(framework);
    }
    token.check()?;
    let options = FileAnalysisOptions {
        inspections: &inspection_options,
        grammar: inputs.grammar,
        dictionary: inputs.dictionary.as_deref(),
        stub_supplements: StubSupplementPolicy::Include,
        dictionary_actions: DictionaryActionPolicy::IncludeSaveWord,
    };
    let mut problems = match inputs.grammar_policy {
        Some(policy) => file_analysis::analyze_python_with_grammar_policy(
            analysis, file, &options, token, policy,
        ),
        None => file_analysis::analyze_python(analysis, file, &options, token),
    }?;
    problems.retain(|problem| {
        problem.diagnostic.code == "syntax"
            || !inputs
                .disabled_inspections
                .iter()
                .any(|code| code == problem.diagnostic.code.as_str())
    });
    super::index_ui::add_library_import_fixes(&mut problems, file, &inputs.index);
    let quick_fixes: QuickFixes = problems
        .iter()
        .filter(|p| !p.fixes.is_empty())
        .map(|p| (p.diagnostic.range, p.fixes.clone()))
        .collect();
    let diagnostics: Vec<rustpy_core::Diagnostic> =
        problems.into_iter().map(|p| p.diagnostic).collect();
    token.check()?;
    let (ph, th) = (inputs.parameter_hints, inputs.type_hints);
    let inlay_hints = if ph || th {
        features::inlay_hints(analysis, file, ph, th)
    } else {
        Vec::new()
    };
    // Hints computed after a cancellation are imprecise: never publish them.
    token.check()?;
    Ok(FileAnalysis {
        diagnostics,
        quick_fixes,
        inlay_hints,
    })
}

/// The quick fixes of `state` when they were computed for its current text.
/// After an edit their ranges point at other text (applying one would
/// corrupt the file); in Power Save Mode nothing recomputes them at all.
pub(super) fn current_quick_fixes(
    state: &crate::editor::EditorState,
) -> &[(TextRange, Vec<inspections::QuickFix>)] {
    if state.semantic_version == Some(state.doc.version()) {
        &state.quick_fixes
    } else {
        &[]
    }
}

fn content_hash(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// An LRU with independent entry-count and retained-payload limits.
pub(super) struct LruCache<K, V> {
    map: HashMap<K, (V, u64, usize)>,
    tick: u64,
    capacity: usize,
    max_bytes: usize,
    bytes: usize,
}
impl<K: Hash + Eq, V: Clone> LruCache<K, V> {
    #[cfg(test)]
    pub fn new(capacity: usize) -> Self {
        Self::with_limits(capacity, usize::MAX)
    }
    pub fn with_limits(capacity: usize, max_bytes: usize) -> Self {
        Self {
            map: HashMap::new(),
            tick: 0,
            capacity: capacity.max(1),
            max_bytes,
            bytes: 0,
        }
    }
    fn touch(&mut self) -> u64 {
        if self.tick == u64::MAX {
            // Preserve ordering when renormalizing the bounded map.
            let mut ticks: Vec<_> = self.map.values().map(|(_, tick, _)| *tick).collect();
            ticks.sort_unstable();
            for (_, tick, _) in self.map.values_mut() {
                *tick = ticks.binary_search(tick).unwrap_or(0) as u64;
            }
            self.tick = self.map.len() as u64;
        }
        self.tick += 1;
        self.tick
    }
    pub fn get(&mut self, key: &K) -> Option<V> {
        let tick = self.touch();
        self.map.get_mut(key).map(|(value, used, _)| {
            *used = tick;
            value.clone()
        })
    }
    #[cfg(test)]
    pub fn insert(&mut self, key: K, value: V) {
        self.insert_weighted(key, value, 0);
    }
    pub fn insert_weighted(&mut self, key: K, value: V, bytes: usize) {
        if let Some((_, _, old)) = self.map.remove(&key) {
            self.bytes -= old;
        }
        // Oversized results still reach the active editor, but must not evict
        // useful small results merely to retain an entry larger than the cache.
        if bytes > self.max_bytes {
            return;
        }
        while self.map.len() >= self.capacity || self.bytes > self.max_bytes - bytes {
            let Some(oldest) = self.map.values().map(|(_, tick, _)| *tick).min() else {
                break;
            };
            self.map.retain(|_, (_, tick, weight)| {
                if *tick == oldest {
                    self.bytes -= *weight;
                    false
                } else {
                    true
                }
            });
        }
        let tick = self.touch();
        self.bytes += bytes;
        self.map.insert(key, (value, tick, bytes));
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn retained_bytes(&self) -> usize {
        self.bytes
    }
    pub fn clear(&mut self) {
        self.map.clear();
        self.bytes = 0;
    }
}

impl FileAnalysis {
    fn retained_bytes(&self) -> usize {
        let mut bytes =
            std::mem::size_of::<Self>().saturating_add(2 * std::mem::size_of::<usize>());
        bytes = bytes.saturating_add(
            self.diagnostics
                .capacity()
                .saturating_mul(std::mem::size_of::<rustpy_core::Diagnostic>()),
        );
        for diagnostic in &self.diagnostics {
            bytes = bytes
                .saturating_add(diagnostic.message.capacity())
                .saturating_add(diagnostic.code.len());
        }
        bytes = bytes.saturating_add(
            self.quick_fixes
                .capacity()
                .saturating_mul(std::mem::size_of::<(TextRange, Vec<inspections::QuickFix>)>()),
        );
        for (_, fixes) in &self.quick_fixes {
            bytes = bytes.saturating_add(
                fixes
                    .capacity()
                    .saturating_mul(std::mem::size_of::<inspections::QuickFix>()),
            );
            for fix in fixes {
                bytes = bytes.saturating_add(fix.title.capacity()).saturating_add(
                    fix.edits
                        .capacity()
                        .saturating_mul(std::mem::size_of::<(TextRange, String)>()),
                );
                for (_, replacement) in &fix.edits {
                    bytes = bytes.saturating_add(replacement.capacity());
                }
            }
        }
        bytes = bytes.saturating_add(
            self.inlay_hints
                .capacity()
                .saturating_mul(std::mem::size_of::<features::InlayHint>()),
        );
        for hint in &self.inlay_hints {
            bytes = bytes.saturating_add(hint.label.capacity());
        }
        bytes
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CacheKey {
    path: PathBuf,
    content: u64,
    /// [`AnalysisInputs::fingerprint`].
    inputs: u64,
    /// `None` for an empty list (they are all alike).
    symbols: Option<SharedId<Vec<(String, String)>>>,
    dictionary: Option<SharedId<rustpy_spell::Dictionary>>,
    /// [`Analysis::revision_excluding`] the file: other editors, disk files,
    /// module lookups.
    revision: u64,
    editorconfig: rustpy_settings::editorconfig::EditorConfigProps,
}

struct Job {
    measurement: u64,
    path: PathBuf,
    /// Document version the result is for (moved along when an edit is
    /// undone while the run for that content is still going).
    version: u64,
    key: CacheKey,
    handle: TaskHandle<FileAnalysis>,
    started: Instant,
}

/// Outcome of [`AnalysisScheduler::request`].
#[derive(Debug)]
pub(super) enum Request {
    /// A cached result for exactly this state.
    Cached(Arc<FileAnalysis>),
    /// A background run started (or one for the same state is running).
    Running,
    /// The last run for this content failed; it is not retried until the text changes.
    Failed,
}

/// Runs the active editor's analysis in the background and caches results.
pub(super) struct AnalysisScheduler {
    cache: LruCache<CacheKey, Arc<FileAnalysis>>,
    /// The analysis database the cache belongs to (a new one, after an
    /// interpreter change or Invalidate Caches, starts an empty cache).
    db: Weak<Analysis>,
    job: Option<Job>,
    /// A run that failed: `(path, content hash)`.
    failed: Option<(PathBuf, u64)>,
    pub runs: u64,
    pub cache_hits: u64,
}

impl Default for AnalysisScheduler {
    fn default() -> Self {
        Self {
            cache: LruCache::with_limits(CACHE_CAPACITY, CACHE_BYTES),
            db: Weak::new(),
            job: None,
            failed: None,
            runs: 0,
            cache_hits: 0,
        }
    }
}

impl AnalysisScheduler {
    /// Whether a run for `path` at `version` is in flight.
    pub fn is_running(&self, path: &Path, version: u64) -> bool {
        self.job
            .as_ref()
            .is_some_and(|j| j.path == path && j.version == version)
    }

    pub fn has_job(&self) -> bool {
        self.job.is_some()
    }

    /// Stops the run in flight.
    pub fn cancel(&mut self) {
        if let Some(job) = self.job.take() {
            crate::performance_trace::event(
                "analysis_cancel",
                serde_json::json!({"id":job.measurement}),
            );
            job.handle.cancel();
        }
    }

    /// `path` changed to `version`: a run for another version of it is obsolete.
    pub fn cancel_obsolete(&mut self, path: &Path, version: u64) {
        if self
            .job
            .as_ref()
            .is_some_and(|j| j.path == path && j.version != version)
        {
            self.cancel();
        }
    }

    /// The cached result for `file` in its current state, or a background
    /// run computing it (replacing any other run).
    pub fn request(
        &mut self,
        analysis: &Arc<Analysis>,
        file: Arc<FileInfo>,
        version: u64,
        mut inputs: AnalysisInputs,
    ) -> Request {
        if !Weak::ptr_eq(&self.db, &Arc::downgrade(analysis)) {
            // Holding the `Weak` keeps the old allocation, so a new database never reuses its address.
            // Results of the old database, cached or in flight, do not apply to the new one.
            self.cache.clear();
            self.cancel();
            self.failed = None;
            self.db = Arc::downgrade(analysis);
        }
        let content = content_hash(&file.text);
        let key = CacheKey {
            path: file.path.clone(),
            content,
            inputs: inputs.fingerprint(),
            symbols: Some(&inputs.inspections.project_symbols)
                .filter(|s| !s.is_empty())
                .map(SharedId::of),
            dictionary: inputs.dictionary.as_ref().map(SharedId::of),
            revision: analysis.revision_excluding(&file.path),
            editorconfig: if inputs.inspections.editorconfig_enabled {
                rustpy_settings::editorconfig::resolve_cached(&file.path)
            } else {
                Default::default()
            },
        };
        if let Some(hit) = self.cache.get(&key) {
            self.cache_hits += 1;
            crate::performance_trace::event(
                "analysis_cache_hit",
                serde_json::json!({"hits":self.cache_hits}),
            );
            self.cancel_obsolete(&file.path, version);
            return Request::Cached(hit);
        }
        if self
            .failed
            .as_ref()
            .is_some_and(|(p, c)| *p == file.path && *c == content)
        {
            return Request::Failed;
        }
        if let Some(job) = &mut self.job {
            if job.key == key {
                job.version = version;
                return Request::Running;
            }
        }
        self.cancel();
        self.runs += 1;
        let name = file
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        inputs.inspections.editorconfig = Some(Arc::new(key.editorconfig.clone()));
        let db = analysis.clone();
        let path = file.path.clone();
        let measurement = crate::performance_trace::next_id();
        let handle = TaskManager::global()
            .task(format!("Analyzing {name}"))
            .hidden()
            .spawn(move |progress| {
                crate::performance_trace::event("analysis_start", serde_json::json!({"id":measurement,"cancelled":progress.token().is_cancelled()}));
                let result = analyze_file(&db, &file, &inputs, progress.token()).map_err(TaskError::from);
                crate::performance_trace::event("analysis_end", serde_json::json!({"id":measurement,"cancelled":progress.token().is_cancelled()}));
                result
            });
        self.job = Some(Job {
            measurement,
            path,
            version,
            key,
            handle,
            started: Instant::now(),
        });
        Request::Running
    }

    /// A finished run, `(path, document version, result)`; its result is cached.
    pub fn poll(&mut self) -> Option<(PathBuf, u64, Arc<FileAnalysis>)> {
        let result = self.job.as_mut()?.handle.try_result()?;
        let job = self.job.take()?;
        let ms = job.started.elapsed().as_millis();
        match result {
            Ok(r) => {
                let r = Arc::new(r);
                let bytes = r
                    .retained_bytes()
                    .saturating_add(job.key.path.capacity())
                    .saturating_add(
                        2 * std::mem::size_of::<(CacheKey, Arc<FileAnalysis>, u64, usize)>(),
                    );
                self.cache.insert_weighted(job.key, r.clone(), bytes);
                log::debug!(
                    "analysis of {} took {ms} ms ({} problems, {} hints; {} runs, {} cache hits, {} cached, {} retained bytes)",
                    job.path.display(),
                    r.diagnostics.len(),
                    r.inlay_hints.len(),
                    self.runs,
                    self.cache_hits,
                    self.cached_results(),
                    self.cache.retained_bytes()
                );
                Some((job.path, job.version, r))
            }
            Err(TaskError::Cancelled) => {
                log::debug!("analysis of {} cancelled after {ms} ms", job.path.display());
                None
            }
            Err(TaskError::Failed(e)) => {
                log::warn!("analysis of {} failed: {e}", job.path.display());
                self.failed = Some((job.path, job.key.content));
                None
            }
        }
    }

    pub fn cached_results(&self) -> usize {
        self.cache.len()
    }
}

/// Editor documents at least this large reach the analysis database in the
/// background: reparsing one and rebuilding its scope model takes several
/// milliseconds per keystroke (tens in an unoptimised build), which the UI
/// thread cannot afford while typing. Smaller ones are pushed at once.
pub(super) const BACKGROUND_SYNC_BYTES: usize = 48 * 1024;

/// A push of editor text into the analysis database running off the UI thread.
struct TextPush {
    path: PathBuf,
    document: rustpy_text::DocumentId,
    version: u64,
    handle: TaskHandle<()>,
}

/// Pushes the text of edited editors into the analysis database, large ones
/// off the UI thread (see [`BACKGROUND_SYNC_BYTES`]). One push runs at a
/// time: keystrokes made meanwhile are coalesced into the next push, of the
/// newest text. Until a push lands, analysis consumers see the previous,
/// complete version of the file (stale but consistent), and features that
/// need the current text push it synchronously ([`TextSync::push_now`]).
#[derive(Default)]
pub(super) struct TextSync {
    job: std::sync::Mutex<Option<TextPush>>,
    /// The last version seen of each editor: an edit restarts the debounce.
    seen: HashMap<PathBuf, (rustpy_text::DocumentId, u64)>,
}

impl TextSync {
    /// Notes the document version of `path`: true when it is new (an edit).
    pub fn observe(
        &mut self,
        path: &Path,
        document: rustpy_text::DocumentId,
        version: u64,
    ) -> bool {
        let key = (document, version);
        if self.seen.get(path) == Some(&key) {
            return false;
        }
        self.seen.insert(path.to_path_buf(), key);
        true
    }

    /// A finished background push: `(path, document, version)` of the text
    /// the database now has.
    pub fn poll(&mut self) -> Option<(PathBuf, rustpy_text::DocumentId, u64)> {
        let mut job = self.job.lock().unwrap_or_else(|e| e.into_inner());
        let result = job.as_mut()?.handle.try_result()?;
        let push = job.take()?;
        // A failed push leaves the previous text: the next frame retries.
        result.ok()?;
        Some((push.path, push.document, push.version))
    }

    /// Waits for a push of `path` in flight, so a synchronous push made next
    /// cannot be overwritten by older text.
    pub fn finish(&self, path: &Path) {
        let push = {
            let mut job = self.job.lock().unwrap_or_else(|e| e.into_inner());
            if job.as_ref().is_some_and(|push| push.path == path) {
                job.take()
            } else {
                None
            }
        };
        if let Some(push) = push {
            let _ = push.handle.wait();
        }
    }

    /// Pushes `text` of `path` on the calling thread (after any push in flight).
    pub fn push_now(&self, analysis: &Analysis, path: &Path, text: &str) {
        self.finish(path);
        analysis.set_file_text(path, text);
    }

    /// Starts pushing the text `text()` returns for `version` of `path`,
    /// unless a push is in flight (the next frame starts the newest one).
    /// False when nothing started.
    pub fn push_in_background(
        &mut self,
        analysis: &Arc<Analysis>,
        path: &Path,
        document: rustpy_text::DocumentId,
        version: u64,
        text: impl FnOnce() -> String,
    ) -> bool {
        let mut job = self.job.lock().unwrap_or_else(|e| e.into_inner());
        if job.is_some() {
            return false;
        }
        let text = text();
        let (db, target) = (analysis.clone(), path.to_path_buf());
        let handle = TaskManager::global()
            .task("Updating the analysis")
            .hidden()
            .spawn(move |_| {
                // An undone edit or a finished synchronous push can leave the
                // database with this text already.
                if !db
                    .cached_file_for_path(&rustpy_core::vfs::normalize_path(&target))
                    .is_some_and(|file| file.is_open && *file.text == *text)
                {
                    db.set_file_text(&target, &text);
                }
                Ok(())
            });
        *job = Some(TextPush {
            path: path.to_path_buf(),
            document,
            version,
            handle,
        });
        true
    }
}

/// Usages of the identifier under the caret, `(range, is_write)`.
type UsageRanges = Vec<(TextRange, bool)>;

/// Finds the usages of the identifier under the caret of a large file off
/// the UI thread (resolving a name runs the flow analysis of its scope).
#[derive(Default)]
pub(super) struct UsageHighlightJob {
    /// `(path, (document version, caret), run)`.
    job: Option<(PathBuf, (u64, TextSize), TaskHandle<UsageRanges>)>,
}

impl UsageHighlightJob {
    /// Starts finding the usages at `caret` of `file` (replacing any run).
    pub fn start(&mut self, analysis: &Arc<Analysis>, file: Arc<FileInfo>, key: (u64, TextSize)) {
        self.cancel();
        let db = analysis.clone();
        let path = file.path.clone();
        let handle = TaskManager::global()
            .task("Highlighting usages")
            .hidden()
            .spawn(move |progress| {
                let token = progress.token().clone();
                let ranges = rustpy_semantic::infer::with_cancellation(&token, || {
                    usages::usages_in_file(&db, &file, key.1)
                        .into_iter()
                        .map(|u| (u.range, u.is_write))
                        .collect()
                });
                progress.check()?;
                Ok(ranges)
            });
        self.job = Some((path, key, handle));
    }

    pub fn cancel(&mut self) {
        if let Some((_, _, handle)) = self.job.take() {
            handle.cancel();
        }
    }

    /// A finished run: `(path, (version, caret), usages)`.
    pub fn poll(&mut self) -> Option<(PathBuf, (u64, TextSize), UsageRanges)> {
        let result = self.job.as_mut()?.2.try_result()?;
        let (path, key, _) = self.job.take()?;
        Some((path, key, result.ok()?))
    }
}

impl IdeApp {
    pub(super) fn analysis_inputs_for(&self, path: &Path) -> AnalysisInputs {
        let mut inputs = self.analysis_inputs();
        if let Some(project) = &self.project {
            inputs.inspections.python_version =
                project.analysis.resolver.for_path(path).python_version;
        }
        inputs
    }

    /// Inputs of the active editor's analysis from the settings.
    pub(super) fn analysis_inputs(&self) -> AnalysisInputs {
        let project_safe_mode = self.project_safe_mode();
        let (framework, policy) = self.framework_inputs();
        let s = &self.settings;
        let inspections = InspectionOptions {
            framework,
            policy,
            max_line_length: s.editor.right_margin,
            editorconfig_enabled: s.editor.editorconfig_enabled,
            style: {
                let mut style = s.inspections.style.clone();
                if style.quote_style == "code_style" {
                    style.quote_style = s.code_style.quote_style.clone()
                }
                if project_safe_mode {
                    style.interpreter = None;
                    style.pep8_backend = "builtin".into()
                } else if style.interpreter.is_none() {
                    style.interpreter = self
                        .project
                        .as_ref()
                        .and_then(|project| project.interpreter.as_ref())
                        .map(|interpreter| interpreter.path.clone())
                }
                style
            },
            structural_rules: Arc::new(
                rustpy_search::structural::load_library(
                    &super::structural_ui::structural_library_path(),
                )
                .unwrap_or_default(),
            ),
            pep8: s.inspections.pep8,
            unused_imports: s.inspections.unused_imports,
            unused_variables: s.inspections.unused_variables,
            type_checker: s.inspections.type_checker,
            security: s.inspections.security,
            compatibility: s.inspections.compatibility,
            code_smells: s.inspections.code_smells,
            naming: s.inspections.naming,
            missing_docstrings: s.inspections.missing_docstrings,
            python_version: self
                .project
                .as_ref()
                .map(|project| {
                    self.active
                        .and_then(|index| self.tabs.get(index))
                        .and_then(|tab| tab.state.path.as_deref())
                        .map(|path| project.analysis.resolver.for_path(path).python_version)
                        .unwrap_or(project.analysis.resolver.python_version)
                })
                .unwrap_or((3, 12)),
            project_symbols: self.bg.project_symbols.clone(),
            probable_bugs: s.inspections.probable_bugs,
            ..InspectionOptions::default()
        };
        AnalysisInputs {
            inspections,
            disabled_inspections: s.inspections.disabled_inspections.clone(),
            grammar: s.inspections.grammar
                && super::grammar_policy::configured(&s.inspections).is_ok(),
            grammar_policy: super::grammar_policy::configured(&s.inspections).ok(),
            dictionary: s.inspections.spelling.then(|| {
                spell_dictionary_for(self.project.as_ref().map(|project| project.root.as_path()))
            }),
            parameter_hints: s.editor.inlay_parameter_hints,
            type_hints: s.editor.inlay_type_hints,
            index: self.everywhere.index.clone(),
        }
    }

    /// Framework/tool inputs shared by Python analysis and document inspections.
    fn framework_inputs(
        &self,
    ) -> (
        Arc<rustpy_analysis::inspections::framework_tools::Inputs>,
        Option<rustpy_analysis::inspections::infrastructure::Policy>,
    ) {
        let project_safe_mode = self.project_safe_mode();
        self.apply_documentation_policy();
        let s = &self.settings;
        let interpreter = self
            .project
            .as_ref()
            .and_then(|p| p.interpreter.as_ref())
            .map(|i| i.path.clone());
        let (installed, vulnerabilities) = self
            .packages_panel
            .inspection_snapshot(interpreter.as_deref());
        let policy = rustpy_analysis::inspections::infrastructure::Policy::from_settings(
            &s.inspections.profiles,
            self.project.as_ref().map(|p| p.root.as_path()),
            false,
        )
        .ok();
        let (schema_generation, schema_values) = rustpy_project::schema_store::snapshot();
        let mut framework = rustpy_analysis::inspections::framework_tools::Inputs {
            root: self.project.as_ref().map(|p| p.root.clone()),
            namespace_roots: self
                .project
                .as_ref()
                .map(|p| {
                    let mut paths = p
                        .tree
                        .namespace_packages
                        .iter()
                        .cloned()
                        .collect::<Vec<_>>();
                    paths.sort();
                    paths
                })
                .unwrap_or_default(),
            check_unused_ignore_patterns: policy
                .as_ref()
                .and_then(|p| p.rule("GitIgnorePattern"))
                .and_then(|r| r.options.get("check_unused_patterns"))
                .is_some_and(|v| v == "true"),
            document_policy: policy.clone(),
            disabled: s.inspections.disabled_inspections.clone(),
            trusted: !project_safe_mode && !self.tabs.iter().any(|t| t.state.doc.is_modified()),
            interpreter_checked: self.project.is_some(),
            interpreter,
            installed,
            vulnerabilities,
            schema_mappings: s
                .data_files
                .schemas
                .iter()
                .filter(|mapping| mapping.enabled)
                .map(|mapping| (mapping.pattern.clone(), mapping.schema.clone()))
                .collect(),
            template_directories: self
                .project
                .as_ref()
                .map(|project| project.tree.template_dirs.iter().cloned().collect())
                .unwrap_or_default(),
            static_directories: self
                .project
                .as_ref()
                .map(|project| {
                    project
                        .tree
                        .model()
                        .frameworks
                        .static_directories
                        .iter()
                        .map(|path| {
                            if path.is_absolute() {
                                path.clone()
                            } else {
                                project.root.join(path)
                            }
                        })
                        .collect()
                })
                .unwrap_or_default(),
            template_engine: self
                .project
                .as_ref()
                .map(|project| project.tree.model().frameworks.template_engine)
                .unwrap_or_default(),
            template_data_language: self
                .project
                .as_ref()
                .map(|project| project.tree.model().frameworks.template_data_language)
                .unwrap_or_default(),
            schemas: rustpy_analysis::inspections::framework_tools::SchemaSnapshot {
                generation: schema_generation,
                values: schema_values,
            },
            document_tools_generation: rustpy_project::document_tools::generation(),
            project_revision: self
                .project
                .as_ref()
                .map(|project| {
                    project
                        .analysis
                        .revision_excluding(std::path::Path::new("<framework>"))
                })
                .unwrap_or_default(),
            sql_dialect: policy
                .as_ref()
                .and_then(|policy| {
                    policy
                        .rule("SqlDialect")
                        .and_then(|rule| rule.options.get("dialect"))
                        .map(String::as_str)
                })
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
                .or_else(|| {
                    self.db_panel
                        .selected_source
                        .as_ref()
                        .map(|_| "sqlite".into())
                }),
            copyright_template: (s.copyright.enabled && s.copyright_profiles.is_unconfigured())
                .then(|| s.copyright.text.clone()),
            copyright_profiles: (!s.copyright_profiles.is_unconfigured())
                .then(|| serde_json::to_string(&s.copyright_profiles).ok())
                .flatten(),
            copyright_date: chrono::Local::now().format("%Y-%m-%d").to_string(),
            copyright_year: chrono::Local::now().format("%Y").to_string(),
            copyright_user: std::env::var("USER").unwrap_or_default(),
            ..Default::default()
        };
        if framework.trusted {
            if let (Some(root), Some(interpreter), Some(project)) =
                (&framework.root, &framework.interpreter, &self.project)
            {
                rustpy_project::framework_catalog::request(
                    root,
                    interpreter,
                    project
                        .analysis
                        .revision_excluding(std::path::Path::new("<framework>")),
                    true,
                );
                let catalog = rustpy_project::framework_catalog::cached(
                    root,
                    interpreter,
                    project
                        .analysis
                        .revision_excluding(std::path::Path::new("<framework>")),
                );
                apply_framework_catalog(&mut framework, catalog);
            }
        }
        (Arc::new(framework), policy)
    }

    /// Whether editor `i` gets the background analysis: a Python file, not
    /// in large-file mode or read-only, and not in Power Save Mode.
    fn analyzed_in_background(&self, i: usize) -> bool {
        self.background_features().code_analysis
            && self.tabs.get(i).is_some_and(|t| {
                t.state.language == Language::Python
                    && t.state.inspection_level
                        == rustpy_settings::inspection_profiles::HighlightingLevel::All
                    && t.state
                        .path
                        .as_ref()
                        .is_some_and(|p| !p.starts_with("<typeshed>"))
                    && !t.state.is_large_file()
                    && !t.state.doc.is_read_only()
            })
    }

    /// Project inputs (root, template folders, engine, catalogue) of the
    /// document inspections run by a non-Python editor `i`, such as Jinja2
    /// and Django templates (called by `sync_analysis` every frame).
    pub(super) fn refresh_document_inputs(&mut self, i: usize) {
        if self.tabs.get(i).is_some_and(|t| t.state.has_data_syntax()) {
            let (inputs, _) = self.framework_inputs();
            if let Some(tab) = self.tabs.get_mut(i) {
                tab.state.framework_inputs = inputs;
            }
        }
    }

    /// Analysis of the active Python editor `i` (called by `sync_analysis`
    /// every frame): a cached result when the file is in an analysed state,
    /// else a background run once the typing pauses. Nothing runs in Power
    /// Save Mode or for a file in large-file mode.
    pub(super) fn schedule_analysis(&mut self, i: usize, analysis: &Arc<Analysis>, now: f64) {
        if !self.analyzed_in_background(i) {
            return;
        }
        let Some(path) = self.tabs.get(i).and_then(|t| t.state.path.clone()) else {
            return;
        };
        let v = self.tabs[i].state.doc.version();
        match self.tabs[i].state.semantic_version {
            Some(sv) if sv == v => return,
            Some(PENDING) if self.bg.analysis.is_running(&path, v) => return,
            _ => {}
        }
        if now - self.last_edit_time <= DEBOUNCE {
            return;
        }
        // The database still has older text: its push is in flight.
        if self.tabs[i].state.synced_version != Some(v) {
            return;
        }
        let Some(file) = analysis.file_for_path(&path) else {
            return;
        };
        let inputs = self.analysis_inputs_for(&path);
        match self.bg.analysis.request(analysis, file, v, inputs) {
            Request::Cached(r) => self.apply_analysis(i, v, &r),
            Request::Running => self.tabs[i].state.semantic_version = Some(PENDING),
            Request::Failed => {}
        }
    }

    /// Alt+Enter in editor `i`: its quick fixes come from the analysis of
    /// the current text. When that is not there yet (the run is in flight,
    /// or the typing pause is not over), it starts at once and the context
    /// actions open when it finishes: true then. False when they can open
    /// now (analysed, cached, or no background analysis for this editor).
    pub(super) fn defer_intentions_until_analyzed(&mut self, i: usize) -> bool {
        let Some(analysis) = self.project.as_ref().map(|p| p.analysis.clone()) else {
            return false;
        };
        if !self.analyzed_in_background(i)
            || self.tabs[i].state.semantic_version == Some(self.tabs[i].state.doc.version())
        {
            return false;
        }
        let Some((_, file)) = self.analysis_file(i) else {
            return false;
        };
        let v = self.tabs[i].state.doc.version();
        match self
            .bg
            .analysis
            .request(&analysis, file, v, self.analysis_inputs())
        {
            Request::Cached(r) => {
                self.apply_analysis(i, v, &r);
                false
            }
            Request::Running => {
                let tab = &mut self.tabs[i];
                tab.state.semantic_version = Some(PENDING);
                self.bg.intentions_after_analysis =
                    Some((tab.view.id, v, tab.state.primary().head()));
                self.set_status("Analyzing the file: context actions open when it finishes");
                true
            }
            Request::Failed => false,
        }
    }

    /// Opens the context actions deferred by
    /// [`IdeApp::defer_intentions_until_analyzed`] once the analysis is in,
    /// or forgets them when the editor moved on (another editor, an edit or
    /// a caret move).
    pub(super) fn poll_deferred_intentions(&mut self) {
        let Some(at) = self.bg.intentions_after_analysis else {
            return;
        };
        let active = self.active.and_then(|i| self.tabs.get(i));
        if active.map(|t| (t.view.id, t.state.doc.version(), t.state.primary().head())) != Some(at)
        {
            self.bg.intentions_after_analysis = None;
        } else if active.is_some_and(|t| t.state.semantic_version == Some(at.1)) {
            self.bg.intentions_after_analysis = None;
            self.open_intentions();
        }
    }

    /// Shows a finished background run in its editor, unless the editor
    /// changed or its analysis was invalidated since the run started.
    pub(super) fn poll_analysis(&mut self) -> bool {
        let Some((path, version, result)) = self.bg.analysis.poll() else {
            return false;
        };
        if let Some(i) = self
            .tabs
            .iter()
            .position(|t| t.state.path.as_deref() == Some(path.as_path()))
        {
            let st = &self.tabs[i].state;
            if st.doc.version() == version && st.semantic_version == Some(PENDING) {
                self.apply_analysis(i, version, &result);
            }
        }
        true
    }

    fn apply_analysis(&mut self, i: usize, version: u64, r: &FileAnalysis) {
        let st = &mut self.tabs[i].state;
        st.extra_diagnostics = r.diagnostics.clone();
        st.quick_fixes = r.quick_fixes.clone();
        st.inlay_hints = r.inlay_hints.clone();
        st.semantic_version = Some(version);
    }
}

impl IdeApp {
    /// Render-thread-safe framework catalogue (template libraries, tags,
    /// filters, URL names) for completion. Queues trusted discovery on a miss.
    pub(super) fn framework_catalog_snapshot(&self) -> rustpy_project::framework_catalog::Catalog {
        let Some(project) = &self.project else {
            return Default::default();
        };
        let Some(interpreter) = project.interpreter.as_ref() else {
            return Default::default();
        };
        let revision = project
            .analysis
            .revision_excluding(std::path::Path::new("<framework>"));
        let trusted =
            !self.project_safe_mode() && !self.tabs.iter().any(|t| t.state.doc.is_modified());
        rustpy_project::framework_catalog::request(
            &project.root,
            &interpreter.path,
            revision,
            trusted,
        );
        rustpy_project::framework_catalog::cached(&project.root, &interpreter.path, revision)
    }
}

fn apply_framework_catalog(
    inputs: &mut rustpy_analysis::inspections::framework_tools::Inputs,
    catalog: rustpy_project::framework_catalog::Catalog,
) {
    if inputs.installed.is_none() {
        inputs.installed = catalog.installed;
    }
    inputs.interpreter_valid = catalog.interpreter_valid;
    inputs.marker_environment = catalog.marker_environment;
    inputs.django_complete = catalog.django_complete;
    inputs.django_template_engine = catalog.django_templates;
    inputs.template_tags = catalog.tags;
    inputs.template_libraries = catalog.libraries;
    inputs.template_filters = catalog.filters;
    inputs.url_names = catalog.urls;
    inputs.settings_keys = catalog.settings;
    inputs.pending_migrations = catalog.migrations;
    inputs.model_issues = catalog.model_issues;
    inputs.web_issues = catalog.web_issues;
    inputs.plugin_fixtures.extend(catalog.fixtures);
    inputs.fixtures_complete |= catalog.fixtures_complete;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with(files: &[(&str, &str)]) -> (Arc<Analysis>, Vec<Arc<FileInfo>>) {
        let db = Arc::new(Analysis::for_project(Path::new("/analysis-jobs")));
        let infos = files
            .iter()
            .map(|(name, src)| {
                let p = PathBuf::from(format!("/analysis-jobs/{name}"));
                db.set_file_text(&p, src);
                db.file_for_path(&p).unwrap()
            })
            .collect();
        (db, infos)
    }

    fn inputs() -> AnalysisInputs {
        AnalysisInputs {
            inspections: InspectionOptions::default(),
            disabled_inspections: Vec::new(),
            grammar: false,
            grammar_policy: None,
            dictionary: None,
            parameter_hints: true,
            type_hints: true,
            index: Default::default(),
        }
    }

    fn wait(s: &mut AnalysisScheduler) -> (PathBuf, u64, Arc<FileAnalysis>) {
        let deadline = Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if let Some(r) = s.poll() {
                return r;
            }
            assert!(s.has_job(), "the run ended without a result");
            assert!(Instant::now() < deadline, "analysis did not finish");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// The review's case: `import os` is unused, a line is typed above it and
    /// Alt+Enter offers the old "Remove unused import" fix, whose range now
    /// covers part of the new line ("value = compute_total(1)" became
    /// "mpute_total(1)" and the import stayed).
    // Frozen pre-service UI composition, used only for independent output parity.
    fn legacy_analyze_file(
        analysis: &Analysis,
        file: &Arc<FileInfo>,
        inputs: &AnalysisInputs,
        token: &CancellationToken,
    ) -> Result<FileAnalysis, Cancelled> {
        let mut problems =
            inspections::inspect_cancellable(analysis, file, &inputs.inspections, token)?;
        super::super::index_ui::add_library_import_fixes(&mut problems, file, &inputs.index);
        let mut quick_fixes: QuickFixes = problems
            .iter()
            .filter(|p| !p.fixes.is_empty())
            .map(|p| (p.diagnostic.range, p.fixes.clone()))
            .collect();
        let mut diagnostics: Vec<rustpy_core::Diagnostic> =
            problems.into_iter().map(|p| p.diagnostic).collect();
        let text: &str = &file.text;
        if inputs.grammar {
            token.check()?;
            let (d, f) = legacy_grammar_diagnostics(&rustpy_grammar::check_python(text));
            diagnostics.extend(d);
            quick_fixes.extend(f);
        }
        if let Some(dictionary) = &inputs.dictionary {
            token.check()?;
            let typos =
                rustpy_spell::check_text(text, dictionary, rustpy_spell::CheckOptions::default());
            let (d, f) = legacy_spelling_diagnostics(&typos);
            diagnostics.extend(d);
            quick_fixes.extend(f);
        }
        token.check()?;
        // Docstring / signature mismatches (weak warnings).
        for (range, message) in rustpy_semantic::docstrings::docstring_problems(analysis, file) {
            diagnostics.push(rustpy_core::Diagnostic {
                range,
                severity: rustpy_core::Severity::WeakWarning,
                message,
                code: "PyDocstring".into(),
            });
        }
        // Regex injection errors.
        for inj in rustpy_regex::injections(text, &file.parse) {
            for e in rustpy_regex::check(&inj.pattern, inj.verbose) {
                let range = if text[usize::from(inj.string_range.start())..].starts_with(['r', 'R'])
                {
                    TextRange::new(
                        inj.value_range.start() + e.range.start(),
                        (inj.value_range.start() + e.range.end()).min(inj.value_range.end()),
                    )
                } else {
                    inj.value_range
                };
                diagnostics.push(rustpy_core::Diagnostic {
                    range,
                    severity: rustpy_core::Severity::Error,
                    message: format!("Regular expression: {}", e.message),
                    code: "PyRegex".into(),
                });
            }
        }
        token.check()?;
        let (ph, th) = (inputs.parameter_hints, inputs.type_hints);
        let inlay_hints = if ph || th {
            features::inlay_hints(analysis, file, ph, th)
        } else {
            Vec::new()
        };
        Ok(FileAnalysis {
            diagnostics,
            quick_fixes,
            inlay_hints,
        })
    }
    fn legacy_grammar_diagnostics(
        issues: &[rustpy_grammar::Issue],
    ) -> (Vec<rustpy_core::Diagnostic>, QuickFixes) {
        let mut diags = Vec::new();
        let mut fixes = Vec::new();
        for i in issues {
            let range = TextRange::new(
                TextSize::new(i.range.start as u32),
                TextSize::new(i.range.end as u32),
            );
            diags.push(rustpy_core::Diagnostic {
                range,
                severity: rustpy_core::Severity::Typo,
                message: format!("Grammar: {}", i.message),
                code: "Grammar".into(),
            });
            if !i.replacements.is_empty() {
                fixes.push((
                    range,
                    i.replacements
                        .iter()
                        .map(|r| rustpy_analysis::inspections::QuickFix {
                            title: format!("Replace with '{r}'"),
                            edits: vec![(range, r.clone())],
                        })
                        .collect(),
                ));
            }
        }
        (diags, fixes)
    }
    fn legacy_spelling_diagnostics(
        typos: &[rustpy_spell::Typo],
    ) -> (Vec<rustpy_core::Diagnostic>, QuickFixes) {
        let mut diags = Vec::new();
        let mut fixes = Vec::new();
        for t in typos {
            diags.push(rustpy_core::Diagnostic {
                range: t.range,
                severity: rustpy_core::Severity::Typo,
                message: format!("Typo: In word '{}'", t.word),
                code: "SpellCheckingInspection".into(),
            });
            let mut fx: Vec<rustpy_analysis::inspections::QuickFix> = t
                .suggestions
                .iter()
                .take(5)
                .map(|s| rustpy_analysis::inspections::QuickFix {
                    title: format!("Change to '{s}'"),
                    edits: vec![(t.range, s.clone())],
                })
                .collect();
            fx.push(rustpy_analysis::inspections::QuickFix {
                title: format!("Save '{}' to dictionary", t.word.to_lowercase()),
                edits: Vec::new(),
            });
            fixes.push((t.range, fx));
        }
        (diags, fixes)
    }

    #[test]
    fn shared_analysis_bridge_preserves_complete_legacy_ui_results() {
        const SOURCE: &str = "import re\n# a apple recieve\nlabel = 'é'\nre.compile(r'[')\nre.compile('[')\ndef f(x):\n    \"\"\"Args:\n        ghost: value\n    \"\"\"\n    return x\n";
        let dictionary = Arc::new(rustpy_spell::Dictionary::bundled());
        for source in [
            SOURCE,
            "# noinspection PyDocstring\ndef f(x):\n    \"\"\"Args:\n        ghost: value\n    \"\"\"\n    return x\n",
            "# noinspection ALL\n# a apple recieve\nimport re\nre.compile(r'[')\n",
        ] {
            let (db, files) = db_with(&[("fixture.py", source), ("fixture.pyi", source)]);
            for file in files {
                for grammar in [false, true] {
                    for spelling in [false, true] {
                        for incorrect_docstrings in [false, true] {
                            let mut options = inputs();
                            options.inspections.incorrect_docstrings = incorrect_docstrings;
                            options.grammar = grammar;
                            options.dictionary = spelling.then(|| Arc::clone(&dictionary));
                            let expected = legacy_analyze_file(
                                &db,
                                &file,
                                &options,
                                &CancellationToken::new(),
                            )
                            .unwrap();
                            let actual =
                                analyze_file(&db, &file, &options, &CancellationToken::new())
                                    .unwrap();
                            assert_eq!(
                                actual,
                                expected,
                                "{} grammar={grammar} spelling={spelling} incorrect_docstrings={incorrect_docstrings}",
                                file.path.display()
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn shared_prose_converters_preserve_empty_replacements_and_save_word_actions() {
        let issues = vec![
            rustpy_grammar::Issue {
                range: 2..5,
                rule: "empty",
                message: "No replacement".into(),
                replacements: vec![],
            },
            rustpy_grammar::Issue {
                range: 6..10,
                rule: "replace",
                message: "Replace".into(),
                replacements: vec!["first".into(), "second".into()],
            },
        ];
        assert_eq!(
            grammar_diagnostics(&issues),
            legacy_grammar_diagnostics(&issues)
        );
        let (diagnostics, fixes) = grammar_diagnostics(&issues);
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(fixes.len(), 1);
        let dictionary = rustpy_spell::Dictionary::bundled();
        let typos = rustpy_spell::check_plain_text("Recieve", &dictionary);
        assert!(!typos.is_empty());
        assert_eq!(
            spelling_diagnostics(&typos),
            legacy_spelling_diagnostics(&typos)
        );
        assert!(spelling_diagnostics(&typos).1.iter().all(|(_, f)| {
            f.last()
                .is_some_and(|q| q.title.starts_with("Save '") && q.edits.is_empty())
        }));
    }
    #[test]
    fn quick_fixes_apply_only_to_the_text_they_were_computed_for() {
        use crate::editor::{Caret, EditOptions, EditorState, Language};
        let src = "import os\nimport sys\nprint(sys.argv)\n";
        let (db, files) = db_with(&[("fixes.py", src)]);
        let result = analyze_file(&db, &files[0], &inputs(), &CancellationToken::new()).unwrap();
        let mut state = EditorState::from_text(src, Language::Python);
        state.quick_fixes = result.quick_fixes.clone();
        state.extra_diagnostics = result.diagnostics.clone();
        state.semantic_version = Some(state.doc.version());
        let unused_os = |fixes: &[(TextRange, Vec<inspections::QuickFix>)]| {
            fixes
                .iter()
                .flat_map(|(_, f)| f.iter())
                .find(|q| q.title == "Remove unused import 'os'")
                .cloned()
        };
        let fix = unused_os(current_quick_fixes(&state))
            .expect("the fix is offered for the analysed text");
        // Applied to the text it was computed for, it removes the import.
        let mut fresh = EditorState::from_text(src, Language::Python);
        fresh.apply_text_edits(fix.edits.clone(), &fix.title);
        assert_eq!(fresh.doc.text(), "import sys\nprint(sys.argv)\n");
        // A line typed above the import (as in Power Save Mode, or while the
        // analysis of the new text is still running): the fix is withheld.
        state.set_carets(vec![Caret::at(TextSize::new(0))]);
        state.insert_text("value = compute_total(1)\n", &EditOptions::default());
        assert!(
            unused_os(&state.quick_fixes).is_some(),
            "the old fixes are still stored"
        );
        assert!(
            current_quick_fixes(&state).is_empty(),
            "but none is offered for the new text"
        );
        // What applying it would have done.
        let mut stale = EditorState::from_text(&state.doc.text(), Language::Python);
        stale.apply_text_edits(fix.edits, &fix.title);
        assert!(
            !stale.doc.text().starts_with("value = compute_total(1)\n")
                && stale.doc.text().contains("import os"),
            "{:?}",
            stale.doc.text()
        );
        // Analysed again: offered again.
        state.semantic_version = Some(state.doc.version());
        assert!(!current_quick_fixes(&state).is_empty());
    }

    const SRC: &str = "import os\nimport re\n\n\ndef total(items, factor):\n    unused = 1\n    return sum(items) * factor + missing_name\n\n\npattern = re.compile(r\"(unclosed\")\nprint(total([1, 2], factor=2), os.sep)\n";

    #[test]
    fn background_results_match_the_inline_computation() {
        let (db, files) = db_with(&[("m.py", SRC)]);
        let direct = analyze_file(&db, &files[0], &inputs(), &CancellationToken::new()).unwrap();
        // The same checks the UI thread used to run, in the same order.
        let problems = inspections::inspect(&db, &files[0], &InspectionOptions::default());
        assert_eq!(
            direct.diagnostics[..problems.len()],
            problems
                .iter()
                .map(|p| p.diagnostic.clone())
                .collect::<Vec<_>>()[..]
        );
        assert!(
            direct.diagnostics.iter().any(|d| d.code == "PyRegex"),
            "{:?}",
            direct.diagnostics
        );
        assert!(direct
            .diagnostics
            .iter()
            .any(|d| d.message.contains("missing_name")));
        assert!(
            direct
                .quick_fixes
                .iter()
                .any(|(_, f)| f.iter().any(|q| q.title.contains("Remove unused import")))
                || direct.quick_fixes.iter().any(|(_, f)| !f.is_empty())
        );
        assert_eq!(
            direct.inlay_hints,
            features::inlay_hints(&db, &files[0], true, true)
        );
        let mut s = AnalysisScheduler::default();
        assert!(matches!(
            s.request(&db, files[0].clone(), 7, inputs()),
            Request::Running
        ));
        assert!(s.is_running(&files[0].path, 7));
        let (path, version, result) = wait(&mut s);
        assert_eq!((path, version), (files[0].path.clone(), 7));
        assert_eq!(*result, direct, "identical results");
    }

    #[test]
    fn results_are_cached_by_content_and_invalidated_by_other_changes() {
        let (db, files) = db_with(&[
            ("a.py", "from b import thing\n\nprint(thing)\n"),
            ("b.py", "thing = 1\n"),
        ]);
        let a = files[0].path.clone();
        let mut s = AnalysisScheduler::default();
        s.request(&db, files[0].clone(), 1, inputs());
        let first = wait(&mut s).2;
        assert!(first.diagnostics.is_empty(), "{:?}", first.diagnostics);
        // Edit and undo: back to analysed content, served from the cache.
        db.set_file_text(&a, "from b import thing\n\nprint(thing))\n");
        assert!(matches!(
            s.request(&db, db.file_for_path(&a).unwrap(), 2, inputs()),
            Request::Running
        ));
        db.set_file_text(&a, "from b import thing\n\nprint(thing)\n");
        match s.request(&db, db.file_for_path(&a).unwrap(), 3, inputs()) {
            Request::Cached(r) => assert!(Arc::ptr_eq(&r, &first)),
            other => panic!("expected a cache hit, got {other:?}"),
        }
        assert!(!s.has_job(), "the obsolete run was cancelled");
        assert_eq!(s.cache_hits, 1);
        // Another file changes: the cached result may be wrong now.
        db.set_file_text(&files[1].path, "other = 1\n");
        assert!(matches!(
            s.request(&db, db.file_for_path(&a).unwrap(), 3, inputs()),
            Request::Running
        ));
        let after = wait(&mut s).2;
        assert!(
            after
                .diagnostics
                .iter()
                .any(|d| d.message.contains("thing")),
            "{:?}",
            after.diagnostics
        );
        // Different settings: a different key.
        let mut no_hints = inputs();
        no_hints.type_hints = false;
        no_hints.parameter_hints = false;
        assert!(matches!(
            s.request(&db, db.file_for_path(&a).unwrap(), 3, no_hints),
            Request::Running
        ));
        assert!(wait(&mut s).2.inlay_hints.is_empty());
        // A new database starts with an empty cache, and a run in flight for
        // the old one is dropped rather than adopted.
        db.set_file_text(&a, "from b import thing\n\nprint(thing)  # old db\n");
        s.request(&db, db.file_for_path(&a).unwrap(), 4, inputs());
        assert!(s.has_job());
        let (db2, files2) = db_with(&[("a.py", "from b import thing\n\nprint(thing)\n")]);
        assert!(matches!(
            s.request(&db2, files2[0].clone(), 1, inputs()),
            Request::Running
        ));
        assert!(s.is_running(&files2[0].path, 1) && !s.is_running(&a, 4));
        wait(&mut s);
        assert_eq!(s.cached_results(), 1);
    }

    #[test]
    fn edits_cancel_obsolete_runs_and_requests_replace_them() {
        let big: String = (0..400)
            .map(|i| format!("def f{i}(x):\n    return undefined_{i} + x\n\n\n"))
            .collect();
        // Keep the replacement module clean under the enabled default naming
        // inspection; a lowercase literal binding is now a constant warning.
        let (db, files) = db_with(&[("big.py", &big), ("small.py", "VALUE = 1\n")]);
        let mut s = AnalysisScheduler::default();
        s.request(&db, files[0].clone(), 1, inputs());
        assert!(s.is_running(&files[0].path, 1));
        s.cancel_obsolete(&files[0].path, 1);
        assert!(
            s.is_running(&files[0].path, 1),
            "same version: still useful"
        );
        s.cancel_obsolete(&files[1].path, 5);
        assert!(
            s.is_running(&files[0].path, 1),
            "another file's edit does not cancel it"
        );
        s.cancel_obsolete(&files[0].path, 2);
        assert!(!s.has_job());
        // A request for another file replaces the run in flight.
        s.request(&db, files[0].clone(), 2, inputs());
        s.request(&db, files[1].clone(), 1, inputs());
        assert!(s.is_running(&files[1].path, 1) && !s.is_running(&files[0].path, 2));
        let (path, _, r) = wait(&mut s);
        assert_eq!(path, files[1].path);
        assert!(r.diagnostics.is_empty());
        // Cancelled runs stop early and report nothing.
        let token = CancellationToken::new();
        token.cancel();
        assert_eq!(
            analyze_file(&db, &files[0], &inputs(), &token),
            Err(Cancelled)
        );
    }

    /// An edit ends the run in flight promptly in a large project whose
    /// active module has thousands of problems (the performance driver's
    /// workload: 5k module-level bindings, each a naming problem). The
    /// per-problem suppression fixes once ran for a quarter of the run
    /// without a cancellation checkpoint (a linear token lookup per problem).
    #[test]
    fn cancellation_ends_runs_promptly_in_a_large_project() {
        use std::time::Duration;
        const FILES: usize = 5000;
        let root = Path::new("/large-project");
        let db = Arc::new(Analysis::for_project(root));
        let mut symbols = Vec::with_capacity(FILES);
        for n in 0..FILES {
            let package = format!("package_{:04}", n / 100);
            db.set_file_text(
                &root.join(&package).join(format!("module_{n:06}.py")),
                &format!("def compute_{n}(value: int) -> int:\n    return value + {n}\n"),
            );
            symbols.push((format!("compute_{n}"), format!("{package}.module_{n:06}")));
        }
        let text: String = (0..5000)
            .map(|i| format!("value_{i:04} = {i}\n"))
            .chain(["result = value + compute_1\n".to_string()])
            .collect();
        let path = root.join("active.py");
        db.set_file_text(&path, &text);
        let file = db.file_for_path(&path).unwrap();
        let mut inputs = inputs();
        inputs.inspections.project_symbols = Arc::new(symbols);
        // The module-constant naming rule is opt-in; it supplies the problems.
        inputs.inspections.style.check_constants = true;
        let started = Instant::now();
        let full = analyze_file(&db, &file, &inputs, &CancellationToken::new()).unwrap();
        let total = started.elapsed();
        assert!(full.diagnostics.len() > 5000, "{}", full.diagnostics.len());
        // Cancel at points spread over a run: each ends soon after.
        let bound = Duration::from_millis(100).max(total / 5);
        let mut worst = Duration::ZERO;
        for step in 0..10u32 {
            let token = CancellationToken::new();
            let worker = {
                let (db, file, inputs, token) =
                    (db.clone(), file.clone(), inputs.clone(), token.clone());
                std::thread::spawn(move || {
                    let result = analyze_file(&db, &file, &inputs, &token);
                    (result.is_err(), Instant::now())
                })
            };
            std::thread::sleep(total * step / 10);
            let cancelled = Instant::now();
            token.cancel();
            let (stopped, ended) = worker.join().unwrap();
            if stopped {
                worst = worst.max(ended.saturating_duration_since(cancelled));
            }
        }
        assert!(
            worst < bound,
            "cancellation took {worst:?} (bound {bound:?}, full run {total:?})"
        );
    }

    /// A realistic module of about `lines` lines: classes, methods, calls,
    /// comprehensions, f-strings and a few unresolved names.
    fn big_module(lines: usize) -> String {
        let mut out = String::from("import os\nimport re\nfrom typing import Optional\n\n\n");
        let mut i = 0;
        while out.lines().count() < lines {
            out.push_str(&format!(
                "class Model{i}:\n    \"\"\"Model number {i}.\"\"\"\n\n    def __init__(self, name: str, size: int = {i}) -> None:\n        self.name = name\n        self.size = size\n\n    def scaled(self, factor: float) -> float:\n        values = [v * factor for v in range(self.size) if v % 2]\n        return sum(values) + len(self.name)\n\n    def describe(self, prefix: Optional[str] = None) -> str:\n        label = prefix or os.path.basename(self.name)\n        return f\"{{label}}: {{self.size}} {{undefined_{i}}}\"\n\n\ndef build_{i}(count):\n    models = [Model{i}(str(n), n) for n in range(count)]\n    pattern = re.compile(r\"model_[0-9]+\")\n    return [m.scaled(1.5) for m in models if pattern.match(m.name)]\n\n\n"
            ));
            i += 1;
        }
        out
    }

    fn percentiles(mut ms: Vec<f64>) -> String {
        ms.sort_by(|a, b| a.total_cmp(b));
        let at = |p: usize| ms[(ms.len() - 1) * p / 100];
        format!(
            "p50 {:>7.2} ms   p95 {:>7.2} ms   max {:>7.2} ms",
            at(50),
            at(95),
            ms[ms.len() - 1]
        )
    }

    /// Typing latency in a 5,000-line file (roadmap §8: "typing latency
    /// < 16 ms"). Measures what the UI thread still does per keystroke —
    /// the edit, pushing the text to the analysis database (incremental
    /// reparse and scopes, `sync_analysis`) and what the editor view
    /// recomputes (highlighting, folds, syntax diagnostics) — against the
    /// full analysis that used to run on the UI thread after each pause and
    /// now runs in the background, and against a cache hit. Run with
    /// `cargo test -p rustpy-ui --release -- --ignored --nocapture typing_latency`.
    #[test]
    #[ignore = "benchmark: run explicitly in release mode"]
    fn typing_latency_in_a_5k_line_file() {
        use crate::editor::{Caret, EditOptions, EditorState, Language};
        let src = big_module(5000);
        let db = Arc::new(Analysis::for_project(Path::new("/bench")));
        let path = PathBuf::from("/bench/big.py");
        let mut state = EditorState::new(rustpy_text::Document::new(&src), Some(path.clone()));
        state.language = Language::Python;
        db.set_file_text(&path, &state.doc.text());
        state.highlight_spans();
        state.fold_regions();
        // Type a statement into a method in the middle of the file.
        let lines: Vec<&str> = src.lines().collect();
        let line = (lines.len() / 2..lines.len())
            .find(|&l| lines[l].trim_start().starts_with("return sum(values)"))
            .expect("a method in the second half");
        let caret = state
            .doc
            .position_to_offset(rustpy_core::Position::new(line as u32, 0));
        state.set_carets(vec![Caret::at(caret)]);
        let opts = EditOptions::default();
        let (mut edit, mut sync, mut parse, mut highlight, mut total) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let ms = |t: Instant| t.elapsed().as_secs_f64() * 1000.0;
        for c in "        extra = compute(alpha, beta) + undefined_name\n".chars() {
            let start = Instant::now();
            state.insert_text(&c.to_string(), &opts);
            edit.push(ms(start));
            let t = Instant::now();
            db.set_file_text(&path, &state.doc.text());
            sync.push(ms(t));
            let t = Instant::now();
            state.fold_regions();
            let _ = state.diagnostics();
            parse.push(ms(t));
            let t = Instant::now();
            state.highlight_spans();
            highlight.push(ms(t));
            total.push(ms(start));
        }
        let file = db.file_for_path(&path).unwrap();
        assert!(file.text.contains("extra = compute(alpha, beta)"));
        let t = Instant::now();
        let full = analyze_file(&db, &file, &inputs(), &CancellationToken::new()).unwrap();
        let full_ms = ms(t);
        let mut s = AnalysisScheduler::default();
        s.request(&db, file.clone(), 1, inputs());
        wait(&mut s);
        let t = Instant::now();
        let hit = s.request(&db, file.clone(), 1, inputs());
        let hit_ms = ms(t);
        assert!(matches!(hit, Request::Cached(_)));
        // An edit cancels the run in flight quickly.
        db.set_file_text(&path, &format!("{}\n# edited\n", file.text));
        s.request(&db, db.file_for_path(&path).unwrap(), 2, inputs());
        let t = Instant::now();
        s.cancel_obsolete(&path, 3);
        assert!(!s.has_job());
        let cancel_ms = ms(t);
        println!(
            "typing into a {}-line file ({} keystrokes, per keystroke):",
            src.lines().count(),
            total.len()
        );
        println!("  edit                      {}", percentiles(edit));
        println!("  analysis db sync          {}", percentiles(sync));
        println!("  editor reparse + folds    {}", percentiles(parse));
        println!("  editor highlighting       {}", percentiles(highlight));
        println!("  UI thread total           {}", percentiles(total));
        println!(
            "full analysis (background): {full_ms:.1} ms, {} problems, {} hints",
            full.diagnostics.len(),
            full.inlay_hints.len()
        );
        println!("cached result:              {hit_ms:.3} ms");
        println!("cancel obsolete run:        {cancel_ms:.3} ms");
        assert!(!full.diagnostics.is_empty());
    }

    #[test]
    fn lru_cache_drops_the_least_recently_used_entry() {
        let mut c = LruCache::new(2);
        c.insert("a", 1);
        c.insert("b", 2);
        assert_eq!(c.get(&"a"), Some(1));
        c.insert("c", 3);
        assert_eq!(
            (c.get(&"a"), c.get(&"b"), c.get(&"c")),
            (Some(1), None, Some(3))
        );
        assert_eq!(c.len(), 2);
        c.clear();
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn cached_payload_limit_counts_nested_fixes_and_preserves_recent_small_results() {
        let mut large = FileAnalysis::default();
        large.quick_fixes.push((
            TextRange::empty(TextSize::new(0)),
            vec![inspections::QuickFix {
                title: "Replace".into(),
                edits: vec![(TextRange::empty(TextSize::new(0)), "x".repeat(4096))],
            }],
        ));
        let cost = large.retained_bytes();
        assert!(cost >= 4096);
        let mut cache = LruCache::with_limits(48, cost + 1);
        cache.insert_weighted("first", Arc::new(large.clone()), cost);
        cache.insert_weighted("second", Arc::new(large.clone()), cost);
        assert!(cache.get(&"first").is_none());
        assert!(cache.get(&"second").is_some());
        assert!(cache.retained_bytes() <= cost + 1);
        cache.insert_weighted("oversized", Arc::new(large), cost + 2);
        assert!(cache.get(&"oversized").is_none());
        assert!(cache.get(&"second").is_some());
        cache.clear();
        assert_eq!(cache.retained_bytes(), 0);
    }

    #[test]
    fn fingerprint_follows_settings_and_shared_lists() {
        let a = inputs();
        let mut b = inputs();
        assert_eq!(a.fingerprint(), b.fingerprint());
        b.inspections.structural_rules = Arc::new(vec![rustpy_search::structural::Template {
            name: "Calls".into(),
            pattern: "print($x$)".into(),
            inspection: true,
            ..Default::default()
        }]);
        assert_ne!(a.fingerprint(), b.fingerprint());
        b = inputs();
        b.inspections.pep8 = false;
        assert_ne!(a.fingerprint(), b.fingerprint());
        b = inputs();
        b.inspections.python_version = (3, 8);
        assert_ne!(a.fingerprint(), b.fingerprint());
        // Shared lists count by identity: an equal new list is another input.
        let (db, files) = db_with(&[("s.py", "x = undefined_thing\n")]);
        let mut s = AnalysisScheduler::default();
        let mut first = inputs();
        first.inspections.project_symbols =
            Arc::new(vec![("undefined_thing".into(), "pkg.things".into())]);
        s.request(&db, files[0].clone(), 1, first.clone());
        wait(&mut s);
        assert!(matches!(
            s.request(&db, files[0].clone(), 1, first.clone()),
            Request::Cached(_)
        ));
        let mut rebuilt = first.clone();
        rebuilt.inspections.project_symbols = Arc::new(first.inspections.project_symbols.to_vec());
        assert!(
            matches!(
                s.request(&db, files[0].clone(), 1, rebuilt),
                Request::Running
            ),
            "a new symbol list is a cache miss"
        );
        wait(&mut s);
        assert!(
            matches!(
                s.request(&db, files[0].clone(), 1, first),
                Request::Cached(_)
            ),
            "the old one is still cached"
        );
        // Empty lists are all alike.
        assert!(matches!(
            s.request(&db, files[0].clone(), 1, inputs()),
            Request::Running
        ));
        wait(&mut s);
        assert!(matches!(
            s.request(&db, files[0].clone(), 1, inputs()),
            Request::Cached(_)
        ));
    }

    #[test]
    fn shared_ids_hold_their_allocation() {
        let a = Arc::new(vec![1u8]);
        let id = SharedId::of(&a);
        let address = Arc::as_ptr(&a) as usize;
        drop(a);
        // The allocation stays while the id exists: no new Arc can take its address.
        let others: Vec<Arc<Vec<u8>>> = (0..64).map(|i| Arc::new(vec![i])).collect();
        assert!(others.iter().all(|o| Arc::as_ptr(o) as usize != address));
        assert_eq!(id, id.clone());
        assert_ne!(id, SharedId::of(&others[0]));
    }
}

#[cfg(test)]
#[path = "typing_latency_tests.rs"]
mod typing_latency_tests;
