//! Background work of the IDE (roadmap §0.1 "background task scheduler",
//! §8 performance).
//!
//! Long operations run as tasks of the shared [`TaskManager`]: the status bar
//! shows the newest one with a progress bar and a cancel button, the
//! Background Tasks popup lists them all, and cancelling one stops it at its
//! next check (a `git` process is killed). This module starts them and picks
//! up their results once per frame:
//!
//! - project indexing: the symbol index behind Go to Symbol and the project
//!   symbols of "Add import" quick fixes ("Indexing" in the status bar);
//! - Find Usages;
//! - `git push`, `fetch` and `pull`, one at a time per repository (the Git
//!   tool window and the Branches popup wait for them too);
//! - loading a file too large to open directly (large-file mode: a banner
//!   offers "Open in Large-File Mode", very large files open read-only),
//!   and reloading such a file when it changes on disk.
//!
//! It also holds Power Save Mode's gating of background features and shows
//! the UI freeze detector's notice.

use crate::a11y::DialogWindow;
use std::collections::HashMap;
use std::sync::Weak;
use std::time::Duration;

use rustpy_core::{ProgressReporter, TaskError, TaskHandle, TaskManager, TaskResult};
use rustpy_semantic::FileInfo;
use rustpy_text::Document;

use super::analysis_jobs::AnalysisScheduler;
use super::project_problems::ProjectProblems;
use super::*;
use crate::freeze::{Freeze, FreezeDetector};
use crate::search_ui::Symbol;

/// Files larger than this are not loaded when opened: a banner offers to
/// open them in large-file mode.
pub(super) const LARGE_FILE_GATE_BYTES: u64 = 20 * 1024 * 1024;
/// Files larger than this open read-only.
pub(super) const READ_ONLY_FILE_BYTES: u64 = 100 * 1024 * 1024;

/// Task keys: a newer task of the same kind replaces the running one.
const INDEX_TASK: &str = "project-index";

/// How long the freeze notice stays up (seconds).
const FREEZE_NOTICE_SECS: f64 = 20.0;

/// Background features Power Save Mode turns off (File › Power Save Mode).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct BackgroundFeatures {
    /// Inspections, grammar and spelling checks and inlay hints of the
    /// editor, and the analysis of Gherkin and prose files.
    pub code_analysis: bool,
    /// The completion lookup opening by itself while typing.
    pub completion_popup: bool,
    /// Rainbow (semantic) highlighting of locals and parameters.
    pub semantic_highlighting: bool,
    /// Highlighting the usages of the symbol under the caret.
    pub usage_highlighting: bool,
}

impl BackgroundFeatures {
    pub fn of(settings: &Settings) -> Self {
        let on = !settings.power_save_mode;
        Self {
            code_analysis: on,
            completion_popup: on && settings.completion.show_as_you_type,
            semantic_highlighting: on && settings.semantic_highlighting,
            usage_highlighting: on && settings.editor.highlight_usages_on_caret_move,
        }
    }
}

/// `git` operations run in the background.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GitOp {
    Fetch,
    Pull,
}

impl GitOp {
    fn title(self) -> &'static str {
        match self {
            GitOp::Fetch => "Fetch",
            GitOp::Pull => "Pull",
        }
    }

    fn running_text(self) -> &'static str {
        match self {
            GitOp::Fetch => "Fetching…",
            GitOp::Pull => "Pulling…",
        }
    }

    /// The status bar message for a finished operation.
    pub fn message(self, result: &TaskResult<String>) -> String {
        let last_line = |s: &str| {
            s.lines()
                .rfind(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim()
                .to_string()
        };
        match (self, result) {
            (_, Err(TaskError::Cancelled)) => format!("{} cancelled", self.title()),
            (GitOp::Fetch, Ok(_)) => "Fetched".into(),
            (GitOp::Pull, Ok(o)) => format!(
                "Pulled: {}",
                Some(last_line(o))
                    .filter(|l| !l.is_empty())
                    .unwrap_or_else(|| "done".into())
            ),
            (op, Err(TaskError::Failed(text))) => {
                format!("{} failed: {}", op.title(), last_line(text))
            }
        }
    }
}

struct GitJob {
    op: GitOp,
    /// The repository it runs in.
    root: PathBuf,
    handle: TaskHandle<String>,
}

struct UsagesJob {
    target: super::usage_results_ui::Target,
    options: rustpy_search::usages::results::Options,
    origin: Arc<FileInfo>,
    db: Weak<Analysis>,
    handle: TaskHandle<Vec<rustpy_search::usages::results::Hit>>,
}

/// The project index: Go to Symbol's symbols and `(name, module)` pairs.
type IndexResult = (Arc<Vec<Symbol>>, Vec<(String, String)>);

struct IndexJob {
    root: PathBuf,
    /// The analysis database the module names are computed with.
    db: Weak<Analysis>,
    handle: TaskHandle<IndexResult>,
}

/// Module names of the symbols for a new analysis database (another interpreter).
struct RemapJob {
    root: PathBuf,
    db: Weak<Analysis>,
    handle: TaskHandle<Vec<(String, String)>>,
}

/// A file opened in large-file mode: waiting for "Open", or loading.
pub(super) struct LargeFile {
    pub size: u64,
    load: Option<TaskHandle<(Document, rustpy_core::vfs::Snapshot)>>,
    tab_id: egui::Id,
    session: u64,
    version: u64,
    generation: u64,
    document: rustpy_text::DocumentId,
}

/// A loaded large file being read again after it changed on disk.
struct Reload {
    /// The editor's document version when the reload started: an edit
    /// since then wins over the disk.
    version: u64,
    handle: TaskHandle<(Document, rustpy_core::vfs::Snapshot)>,
    tab_id: egui::Id,
    session: u64,
    if_modified: IfModified,
    context: ReloadContext,
}

#[derive(Clone, Debug)]
struct ReloadContext {
    generation: u64,
    document: rustpy_text::DocumentId,
    selected: Option<rustpy_text::Encoding>,
    request: Option<super::encoding_ui::EncodingReloadRequest>,
}

/// What [`IdeApp::reload_large_file`] does when the editor has unsaved changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IfModified {
    /// Ask (the File Changed on Disk dialog).
    Ask,
    /// Keep the editor's text.
    Keep,
    /// Reload anyway: the user chose to lose the changes.
    Discard,
}

struct EncodingPreparation {
    request: super::encoding_ui::EncodingReloadRequest,
    handle: TaskHandle<rustpy_core::vfs::ContentRevision>,
}

/// Background state of the application (one field of `IdeApp`).
pub(super) struct BackgroundState {
    /// The active editor's code analysis.
    pub analysis: AnalysisScheduler,
    /// Editor text on its way into the analysis database.
    pub text_sync: super::analysis_jobs::TextSync,
    /// Usages of the identifier under the caret of a large file.
    pub usage_highlights: super::analysis_jobs::UsageHighlightJob,
    /// Run markers of a large file: `(editor, version, path, run)`.
    pub run_lines: Option<(egui::Id, u64, PathBuf, TaskHandle<Vec<usize>>)>,
    /// Project symbol index `(name, module)` for "Add import" fixes.
    pub project_symbols: Arc<Vec<(String, String)>>,
    /// The analysis database `project_symbols`' module names come from.
    symbols_db: Weak<Analysis>,
    /// Module-level project symbols `(name, detail, path, line)` the module names were computed from.
    index_symbols: Arc<Vec<(String, String, PathBuf, usize)>>,
    /// The Background Tasks popup is open.
    pub tasks_popup: bool,
    pub(super) external_formatter: Option<super::formatter_tools::Job>,
    pub(super) reformat: Option<super::reformat_workflows::Job>,
    index: Option<IndexJob>,
    remap: Option<RemapJob>,
    usages: Option<UsagesJob>,
    git: Vec<GitJob>,
    /// Tabs of files opened in large-file mode that are not loaded yet.
    pub large_files: HashMap<PathBuf, LargeFile>,
    /// Large files being reloaded after they changed on disk.
    reloads: HashMap<PathBuf, Reload>,
    encoding_preparations: HashMap<PathBuf, EncodingPreparation>,
    /// Alt+Enter pressed while the editor's analysis was running: the
    /// context actions open once it finishes, if the editor (view, document
    /// version, caret) is still where it was.
    pub intentions_after_analysis: Option<(egui::Id, u64, TextSize)>,
    /// Problems › Project Errors.
    pub problems: ProjectProblems,
    pub freeze: FreezeDetector,
    /// The last freeze, and when it was reported (`ctx` time).
    freeze_notice: Option<(Freeze, f64)>,
    waker_installed: bool,
}

impl Default for BackgroundState {
    fn default() -> Self {
        Self {
            analysis: AnalysisScheduler::default(),
            text_sync: Default::default(),
            usage_highlights: Default::default(),
            run_lines: None,
            project_symbols: Arc::default(),
            symbols_db: Weak::new(),
            index_symbols: Arc::default(),
            tasks_popup: false,
            external_formatter: None,
            reformat: None,
            index: None,
            remap: None,
            usages: None,
            git: Vec::new(),
            large_files: HashMap::new(),
            reloads: HashMap::new(),
            encoding_preparations: HashMap::new(),
            intentions_after_analysis: None,
            problems: ProjectProblems::default(),
            freeze: FreezeDetector::new(
                FreezeDetector::DEFAULT_THRESHOLD,
                FreezeDetector::STARTUP_GRACE,
            ),
            freeze_notice: None,
            waker_installed: false,
        }
    }
}

/// `(name, module)` for each symbol whose file has a module name in `db`.
fn module_symbols(
    db: &Analysis,
    symbols: &[Symbol],
    progress: &ProgressReporter,
) -> Result<Vec<(String, String)>, rustpy_core::Cancelled> {
    let mut modules: HashMap<&Path, Option<String>> = HashMap::new();
    let mut out = Vec::with_capacity(symbols.len());
    for (k, (name, _, path, _)) in symbols.iter().enumerate() {
        if k.is_multiple_of(4096) {
            progress.check()?;
        }
        // Qualified class members belong in navigation, never `from m import`.
        if name.contains('.') {
            continue;
        }
        if let Some(m) = modules
            .entry(path.as_path())
            .or_insert_with(|| db.resolver.module_name_of(path))
        {
            out.push((name.clone(), m.clone()));
        }
    }
    Ok(out)
}

/// Reads `path` (of `size` bytes) reporting the fraction read; stops when cancelled.
#[cfg(test)]
fn read_with_progress(path: &Path, size: u64, progress: &ProgressReporter) -> TaskResult<Vec<u8>> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    let mut chunk = vec![0u8; 1 << 20];
    loop {
        progress.check()?;
        let n = file.read(&mut chunk)?;
        if n == 0 {
            return Ok(bytes);
        }
        bytes.extend_from_slice(&chunk[..n]);
        progress.set_fraction(bytes.len() as f64 / size.max(1) as f64);
    }
}

/// The first `limit` bytes of `path` and its size: binary detection and hex
/// views read no more than they show.
pub(super) fn read_head(path: &Path, limit: usize) -> Option<(Vec<u8>, u64)> {
    use std::io::Read;
    let file = rustpy_core::vfs::open_regular_file(path).ok()?;
    let size = file.metadata().ok()?.len();
    let mut head = Vec::with_capacity(limit.min(usize::try_from(size).unwrap_or(limit)));
    file.take(limit as u64).read_to_end(&mut head).ok()?;
    Some((head, size))
}

/// `35.2 MB`.
pub(super) fn format_size(bytes: u64) -> String {
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else {
        format!("{mb:.1} MB")
    }
}

impl IdeApp {
    /// Picks up the results of background work (once per frame, early).
    pub(super) fn poll_background(&mut self, ctx: &egui::Context) {
        if !self.bg.waker_installed {
            self.bg.waker_installed = true;
            let ctx = ctx.clone();
            // A finished task repaints, so its result shows without input.
            TaskManager::global().set_waker(move || ctx.request_repaint());
            self.bg.freeze.start_watchdog();
        }
        if self.poll_analysis() {
            ctx.request_repaint();
        }
        self.poll_deferred_intentions();
        if self.poll_index() {
            // An open Search Everywhere popup grows to the new list next frame.
            ctx.request_repaint();
        }
        self.poll_external_formatter();
        self.poll_reformat_workflow();
        self.poll_usages();
        self.poll_git();
        self.poll_large_files();
        self.poll_encoding_preparations();
        self.poll_reloads();
        self.poll_project_problems();
        self.git_panel.busy = self.git_busy();
        if self.bg.index.is_some()
            || self.bg.usages.is_some()
            || !self.bg.git.is_empty()
            || self.bg.analysis.has_job()
            || !self.bg.reloads.is_empty()
            || !self.bg.encoding_preparations.is_empty()
        {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    /// Windows of the background machinery: the Background Tasks popup and
    /// the freeze notice (after the main panels).
    pub(super) fn background_ui(&mut self, ctx: &egui::Context) {
        let theme = self.theme.clone();
        crate::progress_ui::tasks_popup(
            ctx,
            &theme,
            TaskManager::global(),
            &mut self.bg.tasks_popup,
        );
        let now = ctx.input(|i| i.time);
        if let Some(f) = self.bg.freeze.take_freeze() {
            self.bg.freeze_notice = Some((f, now));
        }
        let Some((freeze, since)) = self.bg.freeze_notice.clone() else {
            return;
        };
        if now - since > FREEZE_NOTICE_SECS {
            self.bg.freeze_notice = None;
            return;
        }
        let (mut dismiss, mut show_log) = (false, false);
        egui::Window::new("UI Freeze")
            .id(egui::Id::new("freeze_notice"))
            .collapsible(false)
            .resizable(false)
            .default_width(360.0)
            .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-12.0, -36.0))
            .show_modal(ctx, |ui| {
                ui.label(
                    egui::RichText::new(crate::freeze::notice_text(&freeze)).color(theme.text),
                );
                ui.label(
                    egui::RichText::new("What the IDE was doing was written to the log.")
                        .color(theme.text_dim)
                        .small(),
                );
                ui.horizontal(|ui| {
                    show_log = ui.button("Show Log").clicked();
                    dismiss = ui.button("Dismiss").clicked();
                });
            });
        ctx.request_repaint_after(Duration::from_secs(1));
        if show_log {
            self.pending_action = Some(Action::ShowLogInFiles);
        }
        if dismiss || show_log {
            self.bg.freeze_notice = None;
        }
    }

    /// The status bar's background part (laid out right to left): the
    /// newest running task with its progress bar and cancel button, and the
    /// Power Save Mode indicator.
    pub(super) fn status_bar_tasks(&mut self, ui: &mut egui::Ui) {
        let theme = self.theme.clone();
        if crate::progress_ui::status_widget(ui, &theme, TaskManager::global()) {
            self.bg.tasks_popup = !self.bg.tasks_popup;
        }
        if self.settings.power_save_mode {
            let resp = ui.add(egui::Label::new(egui::RichText::new("Power Save").color(theme.warning)).sense(Sense::click())).on_hover_text("Power Save Mode is on: no background code analysis, completion popups or semantic highlighting. Click to turn it off.");
            if resp.clicked() {
                self.set_power_save_mode(false);
            }
        }
    }

    // ----- Power Save Mode -------------------------------------------------

    pub(super) fn background_features(&self) -> BackgroundFeatures {
        BackgroundFeatures::of(&self.settings)
    }

    pub(super) fn set_power_save_mode(&mut self, on: bool) {
        if self.settings.power_save_mode == on {
            return;
        }
        self.settings.power_save_mode = on;
        let _ = self.settings.save();
        self.bg.analysis.cancel();
        self.bg.intentions_after_analysis = None;
        for t in &mut self.tabs {
            // Diagnostics fall back to syntax errors; turning the mode off re-analyses.
            // Quick fixes go too: nothing recomputes them while the mode is
            // on, and their ranges would not follow later edits.
            t.state.semantic_version = None;
            t.state.quick_fixes.clear();
            t.state.extra_diagnostics.clear();
            t.state.inlay_hints.clear();
            t.state.usage_highlights.clear();
            t.state.usage_highlight_caret = None;
        }
        self.set_status(if on { "Power Save Mode is on: background code analysis, completion popups while typing and semantic highlighting are off" } else { "Power Save Mode is off" });
    }

    // ----- project index ---------------------------------------------------

    /// (Re)builds the project index in the background ("Indexing" in the
    /// status bar); a build already running for this project is kept.
    pub(super) fn start_indexing(&mut self) {
        let Some(p) = &self.project else { return };
        if self.bg.index.as_ref().is_some_and(|j| {
            j.root == p.root
                && Weak::ptr_eq(&j.db, &Arc::downgrade(&p.analysis))
                && !j.handle.is_cancelled()
        }) {
            return;
        }
        let (root, analysis) = (p.root.clone(), p.analysis.clone());
        let db = Arc::downgrade(&analysis);
        let scan_root = root.clone();
        let handle = TaskManager::global()
            .task("Indexing")
            .exclusive(INDEX_TASK)
            .spawn(move |progress| {
                progress.set_text("Indexing project files");
                let symbols = crate::search_ui::scan_symbols_with_progress(&scan_root, progress);
                progress.check()?;
                progress.set_indeterminate();
                progress.set_details("Resolving module names…");
                let modules = module_symbols(&analysis, &symbols, progress)?;
                Ok((Arc::new(symbols), modules))
            });
        self.bg.index = Some(IndexJob { root, db, handle });
    }

    /// Rebuilds the project index from scratch (Invalidate Caches).
    pub(super) fn reindex(&mut self) {
        if let Some(job) = self.bg.index.take() {
            job.handle.cancel();
        }
        if let Some(job) = self.bg.remap.take() {
            job.handle.cancel();
        }
        self.bg.project_symbols = Arc::default();
        self.bg.index_symbols = Arc::default();
        self.bg.symbols_db = Weak::new();
        self.start_indexing();
    }

    /// Folders marked *Excluded* in the Project view: project-wide searches skip them.
    pub(super) fn excluded_dirs(&self) -> Vec<PathBuf> {
        self.project
            .as_ref()
            .map(|p| p.tree.excluded.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Picks up a finished index; true when the symbols changed.
    fn poll_index(&mut self) -> bool {
        let mut updated = false;
        if let Some(result) = self.bg.index.as_mut().and_then(|j| j.handle.try_result()) {
            let job = self.bg.index.take().expect("polled job");
            match result {
                Ok((symbols, modules))
                    if self.project.as_ref().is_some_and(|p| {
                        p.root == job.root && Weak::ptr_eq(&job.db, &Arc::downgrade(&p.analysis))
                    }) =>
                {
                    log::info!(
                        "project index: {} symbols in {} ms",
                        symbols.len(),
                        job.handle.elapsed().as_millis()
                    );
                    self.bg.index_symbols = symbols;
                    self.bg.project_symbols = Arc::new(modules);
                    self.bg.symbols_db = job.db;
                    updated = true;
                }
                Ok(_) | Err(TaskError::Cancelled) => {}
                Err(TaskError::Failed(e))
                    if self.project.as_ref().is_some_and(|p| {
                        p.root == job.root && Weak::ptr_eq(&job.db, &Arc::downgrade(&p.analysis))
                    }) =>
                {
                    self.set_status(format!("Indexing failed: {e}"))
                }
                Err(TaskError::Failed(_)) => {}
            }
        }
        if let Some(result) = self.bg.remap.as_mut().and_then(|j| j.handle.try_result()) {
            let job = self.bg.remap.take().expect("polled job");
            if self.project.as_ref().is_some_and(|p| {
                p.root == job.root && Weak::ptr_eq(&job.db, &Arc::downgrade(&p.analysis))
            }) {
                if let Ok(modules) = result {
                    self.bg.project_symbols = Arc::new(modules);
                    self.bg.symbols_db = job.db;
                    updated = true;
                }
            }
        }
        // Another analysis database (interpreter or source roots changed):
        // module names may differ, recompute them from the index.
        let Some(analysis) = self.project.as_ref().map(|p| p.analysis.clone()) else {
            return updated;
        };
        let current = Arc::downgrade(&analysis);
        if self.bg.index.is_none()
            && self.bg.remap.is_none()
            && !self.bg.index_symbols.is_empty()
            && !Weak::ptr_eq(&self.bg.symbols_db, &current)
        {
            let symbols = self.bg.index_symbols.clone();
            let handle = TaskManager::global()
                .task("Updating symbol index")
                .hidden()
                .spawn(move |progress| Ok(module_symbols(&analysis, &symbols, progress)?));
            let root = self.project.as_ref().unwrap().root.clone();
            self.bg.remap = Some(RemapJob {
                root,
                db: current,
                handle,
            });
        }
        updated
    }

    /// Stops the work that belongs to the open project (closing it).
    pub(super) fn cancel_project_tasks(&mut self) {
        self.cancel_docker_services_project();
        self.cancel_native_ui();
        self.cancel_plugin_runtime();
        self.cancel_remote_development_project();
        self.stop_ai_workers();
        self.stop_web_languages();
        self.cancel_ai_inline();
        self.cancel_collaboration_project();
        self.db_panel.stop_all_database();
        self.diagram_panel.cancel_capture();
        self.deploy.cancel_all();
        self.cancel_data_files_project();
        self.cancel_web_preview_project();
        if let Some(project) = &self.project {
            self.http_panel.stop_project(&project.root);
        }
        self.cancel_file_watchers();
        self.stop_other_vcs_operations();
        self.git_panel.cancel_log_project();
        self.git_panel.cancel_commit_project();
        self.git_panel.cancel_operation_project();
        self.git_panel.branch_workspace.cancel_project();
        self.cancel_run_workflows();
        self.cancel_measurement_project();
        for (session, _) in &mut self.debug_sessions {
            session.stop();
        }
        self.cancel_task_workspace();
        self.debug_sessions.clear();
        self.debug_panel.setup_task = None;
        self.stop_svg_previews();
        self.cancel_directory_comparisons();
        self.cancel_native_skeletons();
        for job in [
            self.bg.index.take().map(|j| j.handle.token().clone()),
            self.bg.remap.take().map(|j| j.handle.token().clone()),
            {
                self.usage_ui.searching = false;
                self.bg.usages.take().map(|j| j.handle.token().clone())
            },
        ]
        .into_iter()
        .flatten()
        {
            job.cancel();
        }
        self.bg.analysis.cancel();
        for (_, lf) in self.bg.large_files.drain() {
            if let Some(h) = lf.load {
                h.cancel();
            }
        }
        for (_, r) in self.bg.reloads.drain() {
            r.handle.cancel();
        }
        for (_, preparation) in self.bg.encoding_preparations.drain() {
            preparation.handle.cancel();
        }
        self.bg.intentions_after_analysis = None;
        self.bg.problems.clear();
        self.bg.project_symbols = Arc::default();
        self.bg.symbols_db = Weak::new();
        self.bg.index_symbols = Arc::default();
    }

    // ----- Find Usages -----------------------------------------------------

    /// An owned App task; starting another search cancels only this window's job.
    pub(super) fn start_usage_search(
        &mut self,
        target: super::usage_results_ui::Target,
        options: rustpy_search::usages::results::Options,
        analysis: Arc<Analysis>,
        file: Arc<FileInfo>,
    ) {
        use rustpy_search::usages::results::Scope;
        if let Some(old) = self.bg.usages.take() {
            old.handle.cancel();
        }
        let project_scope = self.project_scope();
        let excluded = self.excluded_dirs();
        let origin = file.clone();
        let db = Arc::downgrade(&analysis);
        let worker_options = options.clone();
        let caret = target.offset;
        let title = format!(
            "Finding usages of '{}'",
            crate::progress_ui::ellipsize(&target.name, 40)
        );
        let handle = TaskManager::global().task(title).spawn(move |progress| {
            progress.set_text("Searching for usages");
            let mut files = match &worker_options.scope {
                Scope::File(path) => vec![path.clone()],
                Scope::OpenFiles(paths) => paths.clone(),
                Scope::Named(_, paths) => paths.iter().cloned().collect(),
                Scope::Directory(directory) => {
                    rustpy_search::project_files_with_progress(directory, &excluded, progress)
                }
                Scope::Custom(roots) => {
                    let mut files = Vec::new();
                    for path in roots {
                        progress.check()?;
                        if path.is_dir() {
                            files.extend(rustpy_search::project_files_with_progress(
                                path, &excluded, progress,
                            ));
                        } else {
                            files.push(path.clone());
                        }
                    }
                    files
                }
                Scope::Project => {
                    if !usages::searches_project(&analysis, &file, caret)
                        && !worker_options.strings
                        && !worker_options.comments
                        && !worker_options.text_occurrences
                        && !worker_options.implementing
                        && !worker_options.overriding
                    {
                        vec![file.path.clone()]
                    } else {
                        project_scope
                            .as_ref()
                            .map(|scope| {
                                rustpy_project::files::project_files_in_with_cancel(scope, &|| {
                                    progress.is_cancelled()
                                })
                            })
                            .unwrap_or_default()
                    }
                }
            };
            files.sort();
            files.dedup();
            rustpy_core::parallel::in_background_pool(|| {
                rustpy_search::usages::results::search(
                    &analysis,
                    &file,
                    caret,
                    &files,
                    &worker_options,
                    progress,
                )
            })
            .map_err(rustpy_core::TaskError::from)
        });
        self.usage_ui.begin_search();
        self.usage_ui.target = Some(target.clone());
        self.usage_ui.options = options.clone();
        self.bg.usages = Some(UsagesJob {
            target,
            options,
            origin,
            db,
            handle,
        });
        self.bottom = Some(BottomTool::Find);
    }

    /// Alt+F7: Find Usages into the Find tool window.
    pub(super) fn find_usages(&mut self) {
        self.sync_open_tabs_now();
        let Some(index) = self.active else { return };
        let caret = self.tabs[index].state.primary().head();
        let Some((analysis, file)) = self.analysis_file(index) else {
            return;
        };
        let Some(token) = features::ident_at(&file, caret) else {
            return;
        };
        let target = super::usage_results_ui::Target {
            path: file.path.clone(),
            offset: caret,
            name: token.text().to_owned(),
        };
        let options = self.usage_options(&target.path);
        self.set_status(format!("Finding usages of '{}'…", target.name));
        self.start_usage_search(target, options, analysis, file);
    }

    fn poll_usages(&mut self) {
        let Some(result) = self
            .bg
            .usages
            .as_mut()
            .and_then(|job| job.handle.try_result())
        else {
            return;
        };
        let job = self.bg.usages.take().expect("polled usage job");
        self.usage_ui.searching = false;
        let current = self.analysis_handle();
        if !current
            .as_ref()
            .zip(job.db.upgrade())
            .is_some_and(|(current, original)| {
                Arc::ptr_eq(current, &original)
                    && current
                        .current_text(&job.origin.path)
                        .is_some_and(|text| text == job.origin.text)
            })
        {
            self.set_status("Find Usages source changed; re-run the search");
            return;
        }
        let found = match result {
            Ok(found) => found,
            Err(TaskError::Cancelled) => {
                self.set_status(format!(
                    "Search for usages of '{}' cancelled",
                    job.target.name
                ));
                return;
            }
            Err(TaskError::Failed(error)) => {
                self.set_status(format!("Find Usages failed: {error}"));
                return;
            }
        };
        if job.options.skip_single && found.len() == 1 {
            self.navigate_to(&found[0].usage.path, found[0].usage.range);
            self.set_status(format!("The only usage of '{}'", job.target.name));
            return;
        }
        self.set_status(format!("{} usage(s) of '{}'", found.len(), job.target.name));
        self.usages_title = format!("Usages of '{}' ({})", job.target.name, found.len());
        self.usages = found.iter().map(|hit| hit.usage.clone()).collect();
        self.usage_ui.adopt(self.usages_title.clone(), found);
        self.usage_ui.target = Some(job.target);
        self.usage_ui.options = job.options;
        self.bottom = Some(BottomTool::Find);
    }

    // ----- Git -------------------------------------------------------------

    /// The running background Git operation ("Pulling…"), if any: one runs
    /// at a time, and the Git tool window and the Branches popup wait for it.
    pub(super) fn git_busy(&self) -> Option<&'static str> {
        self.project
            .as_ref()
            .and_then(|p| p.git.as_ref())
            .and_then(|g| g.running_operation())
            .map(|_| "Git operation is running…")
    }

    /// `git push` (setting the upstream on a first push), `fetch` or `pull`
    /// as a cancellable background task; cancelling kills the process. One
    /// Git operation runs at a time per repository: two would race for its
    /// lock files (`index.lock`, "cannot lock ref"), and a pull rewrites the
    /// working tree other operations read.
    pub(super) fn git_in_background(&mut self, op: GitOp) {
        if !self.builtin_plugin_enabled("git") {
            self.set_status("Git bundled capability is disabled in Plugins");
            return;
        }
        if self.project_safe_mode() {
            self.notify_safe_mode("Version control commands and hooks");
            return;
        }
        let Some(git_root) = self
            .project
            .as_ref()
            .and_then(|p| p.git.as_ref().map(|g| g.root().to_path_buf()))
        else {
            self.set_status("Not a Git repository");
            return;
        };
        if let Some(running) = self
            .bg
            .git
            .iter()
            .find(|j| j.root == git_root)
            .map(|j| j.op)
        {
            self.set_status(git_refusal(op, running));
            return;
        }
        let Some(operation) = self.reserve_git_operation(op.title()) else {
            return;
        };
        if op == GitOp::Pull {
            operation.run(|| self.save_for_vcs());
            if let Some(p) = &self.project {
                if let Some(g) = &p.git {
                    crate::git_panel::snapshot_working_tree(g, &p.root, "Before update", false);
                }
            }
        }
        let args: Vec<String> = match op {
            GitOp::Fetch => ["fetch", "--all", "--prune"].map(String::from).to_vec(),
            GitOp::Pull => ["pull", "--ff-only"].map(String::from).to_vec(),
        };
        let root = git_root.clone();
        let handle = TaskManager::global()
            .task(op.title())
            .spawn(move |progress| {
                progress.set_text(format!("git {}", args.join(" ")));
                operation.run(|| {
                    rustpy_vcs::GitRepo::run_git_cancellable(&git_root, &args, progress.token())
                })
            });
        self.bg.git.push(GitJob { op, root, handle });
        self.set_status(op.running_text());
    }

    fn poll_git(&mut self) {
        let mut done = Vec::new();
        self.bg.git.retain_mut(|j| match j.handle.try_result() {
            Some(r) => {
                done.push((j.op, r));
                false
            }
            None => true,
        });
        for (op, result) in done {
            self.set_status(op.message(&result));
            if op == GitOp::Pull {
                self.reload_changed_files();
                self.reload_analysis_from_disk();
            }
            if let Some(p) = &mut self.project {
                p.git_refreshed = -100.0;
            }
        }
    }

    // ----- large-file mode -------------------------------------------------

    /// Opening a file larger than [`LARGE_FILE_GATE_BYTES`]: instead of
    /// loading it, a tab shows a banner offering large-file mode. Returns
    /// false for smaller files and binary files (the hex view reads only the
    /// part it shows).
    pub(super) fn open_large_file(&mut self, path: &Path) -> bool {
        if std::fs::metadata(path)
            .map_or(true, |m| !m.is_file() || m.len() <= LARGE_FILE_GATE_BYTES)
        {
            return false;
        }
        let Some((head, size)) = read_head(path, 8192) else {
            return false;
        };
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        let selected = match self.encoding_override(path) {
            Ok(selected) => selected,
            Err(error) => {
                self.set_status(error);
                return true;
            }
        };
        if size <= LARGE_FILE_GATE_BYTES
            || (selected.is_none()
                && head.contains(&0)
                && !matches!(ext.as_str(), "py" | "txt" | "md"))
        {
            return false;
        }
        let path = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if let Some(i) = self.tab_of(&path) {
            self.active = Some(i);
            self.focus_editor = true;
            return true;
        }
        // A read-only placeholder: nothing is analysed, and Save never writes it.
        let mut state = EditorState::new(Document::new(""), Some(path.clone()));
        state.doc.set_read_only(true);
        self.push_tab(state);
        self.bg.large_files.insert(
            path,
            LargeFile {
                size,
                load: None,
                tab_id: self.tabs.last().unwrap().view.id,
                session: self.file_session,
                version: self.tabs.last().unwrap().state.doc.version(),
                generation: self.encoding_generation,
                document: self.tabs.last().unwrap().state.doc.identity(),
            },
        );
        true
    }

    /// The banner of a large-file tab not loaded yet; true when tab `i` is one.
    pub(super) fn large_file_banner(&mut self, ui: &mut egui::Ui, i: usize) -> bool {
        let Some(path) = self.tabs.get(i).and_then(|t| t.state.path.clone()) else {
            return false;
        };
        let Some(lf) = self.bg.large_files.get(&path) else {
            return false;
        };
        let (size, loading) = (lf.size, lf.load.as_ref().map(|h| h.progress().2));
        let theme = self.theme.clone();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (mut open, mut cancel, mut close) = (false, false, false);
        let rect = ui.available_rect_before_wrap();
        ui.painter()
            .rect_filled(rect, 0.0, rgb(self.scheme.editor.background));
        egui::Frame::new()
            .fill(theme.panel_bg)
            .inner_margin(egui::Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    egui::RichText::new(format!("⚠ {name} is {}", format_size(size)))
                        .strong()
                        .color(theme.warning),
                );
                ui.add_space(4.0);
                ui.label(egui::RichText::new(large_file_explanation(size)).color(theme.text));
                ui.add_space(6.0);
                ui.horizontal(|ui| match loading {
                    Some(fraction) => {
                        crate::a11y::spinner_sized(ui, 14.0);
                        let pct = fraction
                            .map(|f| format!(" {:.0}%", f * 100.0))
                            .unwrap_or_default();
                        ui.label(
                            egui::RichText::new(format!("Loading…{pct}")).color(theme.text_dim),
                        );
                        cancel = ui.button("Cancel").clicked();
                    }
                    None => {
                        open = ui.button("Open in Large-File Mode").clicked();
                        close = ui.button("Close Tab").clicked();
                    }
                });
            });
        if open {
            self.load_large_file(&path, size);
        }
        if cancel {
            if let Some(h) = self
                .bg
                .large_files
                .get_mut(&path)
                .and_then(|lf| lf.load.take())
            {
                h.cancel();
            }
        }
        if close {
            self.close_tab(i);
        }
        true
    }

    fn load_large_file(&mut self, path: &Path, _size: u64) {
        let selected = match self.encoding_override(path) {
            Ok(selected) => selected,
            Err(error) => {
                self.set_status(error);
                return;
            }
        };
        let file = path.to_path_buf();
        let title = format!(
            "Loading {}",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
        let vfs = self.file_vfs();
        let limits = self.editor_read_limits();
        let handle = TaskManager::global()
            .task(title)
            .spawn(move |progress| read_document_snapshot(&vfs, &file, limits, progress, selected));
        if let Some(lf) = self.bg.large_files.get_mut(path) {
            lf.load = Some(handle);
            lf.generation = self.encoding_generation;
            lf.document = self
                .tabs
                .iter()
                .find(|t| t.state.path.as_deref() == Some(path))
                .unwrap()
                .state
                .doc
                .identity();
            lf.version = self
                .tabs
                .iter()
                .find(|t| t.state.path.as_deref() == Some(path))
                .map(|t| t.state.doc.version())
                .unwrap_or(0);
        }
    }

    fn poll_large_files(&mut self) {
        if self.bg.large_files.is_empty() {
            return;
        }
        // Tabs closed meanwhile: forget them (and stop their loads).
        let tabs: std::collections::HashSet<PathBuf> = self
            .tabs
            .iter()
            .filter_map(|t| t.state.path.clone())
            .collect();
        self.bg.large_files.retain(|p, lf| {
            let keep = tabs.contains(p);
            if let (false, Some(h)) = (keep, &lf.load) {
                h.cancel();
            }
            keep
        });
        let mut loaded = Vec::new();
        for (path, lf) in &mut self.bg.large_files {
            if let Some(result) = lf.load.as_mut().and_then(|h| h.try_result()) {
                lf.load = None;
                loaded.push((
                    path.clone(),
                    lf.size,
                    lf.tab_id,
                    lf.session,
                    lf.version,
                    lf.generation,
                    lf.document,
                    result,
                ));
            }
        }
        for (path, _advertised_size, tab_id, session, version, generation, document, result) in
            loaded
        {
            let Some(i) = self.tab_of(&path) else {
                continue;
            };
            if session != self.file_session
                || !self
                    .tabs
                    .iter()
                    .any(|tab| tab.view.id == tab_id || tab.state.doc.identity() == document)
                || self.tabs[i].state.doc.version() != version
                || self.tabs[i].state.doc.identity() != document
                || self.encoding_generation != generation
            {
                continue;
            }
            match result {
                Ok((doc, snapshot)) => {
                    self.bg.large_files.remove(&path);
                    let Some(i) = self.tab_of(&path) else {
                        continue;
                    };
                    if session != self.file_session
                        || !self.tabs.iter().any(|tab| {
                            tab.view.id == tab_id || tab.state.doc.identity() == document
                        })
                        || self.tabs[i].state.doc.version() != version
                    {
                        continue;
                    }
                    let (size, read_only) = loaded_file_state(snapshot.bytes.len(), false);
                    // Complete the shared authority, not one replacement EditorState:
                    // peer placeholders may already own this pending document.
                    self.tabs[i].state.doc.reload_from(doc);
                    // reload_from deliberately preserves the destination policy;
                    // the temporary placeholder policy ends only after guarded load.
                    self.tabs[i].state.doc.set_read_only(read_only);
                    let authority = self.tabs[i].state.doc.share();
                    for tab in &mut self.tabs {
                        if tab.state.doc.shares_authority(&authority) {
                            tab.view.complete_session_load(&mut tab.state);
                        }
                    }
                    // Later changes on disk are told apart from this version.
                    self.disk_revisions
                        .insert(path.clone(), Some(snapshot.revision));
                    self.bind_file_permissions(i);
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.set_status(format!(
                        "{name} ({}) opened in large-file mode{}",
                        format_size(size),
                        if read_only { ", read-only" } else { "" }
                    ));
                }
                Err(TaskError::Cancelled) => {}
                Err(TaskError::Failed(e)) => {
                    self.set_status(format!("Cannot open {}: {e}", path.display()))
                }
            }
        }
    }

    /// Whether tab `i` shows a large-file placeholder (the banner; nothing loaded).
    pub(super) fn is_large_file_placeholder(&self, i: usize) -> bool {
        self.tabs
            .get(i)
            .and_then(|t| t.state.path.as_ref())
            .is_some_and(|p| self.bg.large_files.contains_key(p))
    }

    /// Reloading tab `i` after its file changed on disk, when the tab is in
    /// large-file mode: true then (false for other tabs, which the callers
    /// reload themselves). The UI thread never reads or decodes such a file.
    /// A placeholder has nothing loaded to reload (opening it reads the
    /// current file). A loaded file is read by a background task and swapped
    /// in when it ends, unless the editor changed meanwhile; with unsaved
    /// changes, `if_modified` decides.
    pub(super) fn reload_large_file(&mut self, i: usize, if_modified: IfModified) -> bool {
        let Some(path) = self.tabs.get(i).and_then(|t| t.state.path.clone()) else {
            return false;
        };
        if self.bg.large_files.contains_key(&path) {
            return true;
        }
        let Ok(meta) = std::fs::metadata(&path) else {
            return false;
        };
        let doc = &self.tabs[i].state.doc;
        if meta.len().max(u64::from(u32::from(doc.len()))) <= LARGE_FILE_GATE_BYTES {
            return false;
        }
        if doc.is_modified() && if_modified == IfModified::Keep {
            return true;
        }
        let version = doc.version();
        let document = doc.identity();
        let selected = match self.encoding_override(&path) {
            Ok(selected) => selected,
            Err(error) => {
                self.set_status(error);
                return true;
            }
        };
        let context = ReloadContext {
            generation: self.encoding_generation,
            document,
            selected,
            request: None,
        };
        let file = path.clone();
        let title = format!(
            "Reloading {}",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
        let vfs = self.file_vfs();
        let limits = self.editor_read_limits();
        let handle = TaskManager::global()
            .task(title)
            .spawn(move |progress| read_document_snapshot(&vfs, &file, limits, progress, selected));
        let tab_id = self.tabs[i].view.id;
        let session = self.file_session;
        if let Some(old) = self.bg.reloads.insert(
            path,
            Reload {
                version,
                handle,
                tab_id,
                session,
                if_modified,
                context,
            },
        ) {
            old.handle.cancel();
        }
        true
    }

    pub(super) fn prepare_large_encoding_reload(
        &mut self,
        request: super::encoding_ui::EncodingReloadRequest,
    ) {
        let path = request.path.clone();
        let file = path.clone();
        let vfs = self.file_vfs();
        let limits = self.editor_read_limits();
        let handle = TaskManager::global()
            .task(format!("Preparing encoding reload of {}", path.display()))
            .spawn(move |progress| {
                let read = || {
                    vfs.read_with(&file, limits, || {
                        progress.check().map_err(|_| {
                            std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled")
                        })
                    })
                    .map_err(|error| {
                        if progress.is_cancelled() {
                            TaskError::Cancelled
                        } else {
                            error.into()
                        }
                    })
                };
                let snapshot = read()?;
                progress.check()?;
                if read()?.revision != snapshot.revision {
                    return Err(TaskError::Failed(
                        "file changed while preparing encoding reload; choose again".into(),
                    ));
                }
                Ok(snapshot.revision)
            });
        if let Some(old) = self
            .bg
            .encoding_preparations
            .insert(path, EncodingPreparation { request, handle })
        {
            old.handle.cancel();
        }
    }
    fn poll_encoding_preparations(&mut self) {
        let live: std::collections::HashSet<_> = self
            .tabs
            .iter()
            .map(|tab| {
                (
                    tab.view.id,
                    tab.state.doc.identity(),
                    tab.state.doc.version(),
                )
            })
            .collect();
        let session = self.file_session;
        let generation = self.encoding_generation;
        let mut done = Vec::new();
        self.bg.encoding_preparations.retain(|_, preparation| {
            let request = &preparation.request;
            if request.session != session
                || request.generation != generation
                || !live.contains(&(request.tab_id, request.document, request.version))
            {
                preparation.handle.cancel();
                return false;
            }
            match preparation.handle.try_result() {
                Some(result) => {
                    done.push((preparation.request.clone(), result));
                    false
                }
                None => true,
            }
        });
        for (mut request, result) in done {
            if self.encoding_request_index(&request).is_none() {
                continue;
            }
            match result {
                Ok(revision) => {
                    request.revision = Some(revision);
                    self.finish_encoding_reload_request(request);
                }
                Err(TaskError::Cancelled) => {}
                Err(TaskError::Failed(error)) => {
                    self.set_status(format!("Cannot prepare encoding reload: {error}"))
                }
            }
        }
    }

    pub(super) fn start_encoding_reload(
        &mut self,
        request: super::encoding_ui::EncodingReloadRequest,
    ) {
        let path = request.path.clone();
        let file = path.clone();
        let selected = Some(request.encoding);
        let Some(expected) = request.revision else {
            self.set_status("Cannot reload encoding before its disk snapshot is prepared");
            return;
        };
        let vfs = self.file_vfs();
        let limits = self.editor_read_limits();
        let handle = TaskManager::global()
            .task(format!(
                "Reloading {} as {}",
                path.display(),
                request.encoding.label()
            ))
            .spawn(move |progress| {
                let result = read_document_snapshot(&vfs, &file, limits, progress, selected)?;
                if result.1.revision != expected {
                    return Err(TaskError::Failed(
                        "file changed after reload was requested; choose again".into(),
                    ));
                }
                Ok(result)
            });
        let context = ReloadContext {
            generation: request.generation,
            document: request.document,
            selected,
            request: Some(request.clone()),
        };
        if let Some(old) = self.bg.reloads.insert(
            path,
            Reload {
                version: request.version,
                handle,
                tab_id: request.tab_id,
                session: request.session,
                if_modified: IfModified::Discard,
                context,
            },
        ) {
            old.handle.cancel();
        }
    }
    pub(super) fn apply_encoding_reload(
        &mut self,
        request: super::encoding_ui::EncodingReloadRequest,
        result: TaskResult<(Document, rustpy_core::vfs::Snapshot)>,
    ) {
        let Some(index) = self.encoding_request_index(&request) else {
            return;
        };
        match result {
            Ok((document, snapshot)) => {
                if Some(snapshot.revision) != request.revision {
                    self.set_status("Encoding reload cancelled: the file changed on disk");
                    return;
                }
                self.snapshot(index, "Before encoding reload");
                let placeholder = self.is_large_file_placeholder(index);
                let tab = &mut self.tabs[index];
                let position = tab.view.session_position(&tab.state);
                let (_, read_only) = loaded_file_state(
                    snapshot.bytes.len(),
                    !placeholder && tab.state.doc.is_read_only(),
                );
                tab.state.reload_from_document(document);
                tab.state.doc.set_read_only(read_only);
                let carets = tab.state.carets.clone();
                tab.view
                    .restore_session_position(&mut tab.state, position, true);
                if !placeholder {
                    tab.state.set_carets(carets);
                }
                let encoding = tab.state.doc.encoding();
                self.bg.large_files.remove(&request.path);
                self.disk_revisions
                    .insert(request.path.clone(), Some(snapshot.revision));
                self.bind_file_permissions(index);
                self.publish_encoding_override(&request.path, encoding);
                self.save_workspace();
                self.set_status(format!(
                    "Reloaded {} as {}",
                    request.path.display(),
                    encoding.label()
                ));
            }
            Err(TaskError::Cancelled) => {}
            Err(TaskError::Failed(error)) => {
                self.set_status(format!("Cannot reload {}: {error}", request.path.display()))
            }
        }
    }

    /// Swaps in the large files reloaded in the background.
    fn poll_reloads(&mut self) {
        if self.bg.reloads.is_empty() {
            return;
        }
        let tabs: std::collections::HashSet<PathBuf> = self
            .tabs
            .iter()
            .filter_map(|t| t.state.path.clone())
            .collect();
        let mut done = Vec::new();
        self.bg.reloads.retain(|path, r| {
            if !tabs.contains(path) {
                // The tab was closed.
                r.handle.cancel();
                return false;
            }
            match r.handle.try_result() {
                Some(result) => {
                    done.push((
                        path.clone(),
                        r.version,
                        r.tab_id,
                        r.session,
                        r.if_modified,
                        r.context.clone(),
                        result,
                    ));
                    false
                }
                None => true,
            }
        });
        for (path, version, tab_id, session, if_modified, context, result) in done {
            let Some(i) = self.tab_of(&path) else {
                continue;
            };
            if context.generation != self.encoding_generation
                || context.document != self.tabs[i].state.doc.identity()
                || self.encoding_override(&path).ok().flatten() != context.selected
                    && context.request.is_none()
            {
                continue;
            }
            if let Some(request) = context.request {
                self.apply_encoding_reload(request, result);
            } else {
                self.apply_disk_reload(path, version, tab_id, session, if_modified, result);
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn apply_disk_reload(
        &mut self,
        path: PathBuf,
        version: u64,
        tab_id: egui::Id,
        session: u64,
        if_modified: IfModified,
        result: TaskResult<(Document, rustpy_core::vfs::Snapshot)>,
    ) {
        let Some(i) = self.tab_of(&path) else { return };
        if session != self.file_session || self.tabs[i].view.id != tab_id {
            return;
        }
        if result.is_err() && self.tabs[i].state.doc.version() != version {
            return;
        }
        match result {
            Ok((doc, snapshot)) => {
                let revision = Some(snapshot.revision);
                if if_modified != IfModified::Discard
                    && self.disk_revisions.get(&path) == Some(&revision)
                {
                    return;
                }
                if self.tabs[i].state.doc.version() != version
                    || (self.tabs[i].state.doc.is_modified() && if_modified != IfModified::Discard)
                {
                    if matches!(self.dialog, Dialog::None) {
                        self.observed_revisions.insert(path.clone(), revision);
                        self.dialog = Dialog::ExternalChange(path);
                    } else if !self.external_changes.contains(&path) {
                        self.external_changes.push(path);
                    }
                } else {
                    let (_, read_only) = loaded_file_state(
                        snapshot.bytes.len(),
                        self.tabs[i].state.doc.is_read_only(),
                    );
                    let position = self.tabs[i].view.session_position(&self.tabs[i].state);
                    self.tabs[i].state.reload_from_document(doc);
                    let tab = &mut self.tabs[i];
                    let carets = tab.state.carets.clone();
                    tab.view
                        .restore_session_position(&mut tab.state, position, true);
                    tab.state.set_carets(carets);
                    self.tabs[i].state.doc.set_read_only(read_only);
                    self.refresh_notebook_from_document(i);
                    self.disk_revisions.insert(path.clone(), revision);
                    self.bind_file_permissions(i);
                    self.set_status(format!("Reloaded {} (changed on disk)", path.display()));
                }
            }
            Err(TaskError::Cancelled) => {}
            Err(TaskError::Failed(e)) => {
                self.set_status(format!("Cannot reload {}: {e}", path.display()))
            }
        }
    }
}

/// Classify the bytes actually read, rather than a pre-read metadata size.
/// Reload preserves existing library/manual protection, including after a file
/// shrinks; explicitly making it writable remains a separate user decision.
fn loaded_file_state(actual_bytes: usize, was_read_only: bool) -> (u64, bool) {
    let size = actual_bytes as u64;
    (size, was_read_only || size > READ_ONLY_FILE_BYTES)
}

/// The revision belongs to the bytes decoded, with a second bounded read after
/// decoding to reject changes while the worker ran. Application also guards
/// project session, tab identity and document version.
fn read_document_snapshot(
    vfs: &rustpy_core::vfs::Vfs,
    path: &Path,
    limits: rustpy_core::vfs::ReadLimits,
    progress: &ProgressReporter,
    selected: Option<rustpy_text::Encoding>,
) -> TaskResult<(Document, rustpy_core::vfs::Snapshot)> {
    decode_verified_snapshot(vfs, path, limits, progress, |bytes| {
        Document::from_bytes_with_policy(bytes, rustpy_text::SourcePolicy::for_path(path), selected)
    })
}
fn decode_verified_snapshot(
    vfs: &rustpy_core::vfs::Vfs,
    path: &Path,
    limits: rustpy_core::vfs::ReadLimits,
    progress: &ProgressReporter,
    decode: impl FnOnce(&[u8]) -> Result<Document, rustpy_text::EncodingError>,
) -> TaskResult<(Document, rustpy_core::vfs::Snapshot)> {
    progress.check()?;
    let checkpoint = || {
        progress
            .check()
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled"))
    };
    let read = || {
        vfs.read_with(path, limits, checkpoint).map_err(|error| {
            if progress.is_cancelled() {
                TaskError::Cancelled
            } else {
                error.into()
            }
        })
    };
    let snapshot = read()?;
    progress.set_details("Decoding…");
    let document = decode(&snapshot.bytes).map_err(|error| TaskError::Failed(error.to_string()))?;
    progress.check()?;
    let verified = read()?;
    if verified.revision != snapshot.revision {
        return Err(TaskError::Failed(
            "file changed while being read; refresh again".into(),
        ));
    }
    Ok((document, snapshot))
}

/// Why `op` does not start while `running` runs in the same repository.
fn git_refusal(op: GitOp, running: GitOp) -> String {
    if op == running {
        format!("{} is already running", op.title())
    } else {
        format!(
            "{} is running: wait for it to finish before {}",
            running.title(),
            op.running_text().trim_end_matches('…').to_lowercase()
        )
    }
}

/// What large-file mode means for a file of `size` bytes.
pub(super) fn large_file_explanation(size: u64) -> String {
    let limit = format_size(LARGE_FILE_GATE_BYTES);
    let base = format!(
        "Files larger than {limit} are not opened automatically. In large-file mode syntax highlighting, folding and code insight are off"
    );
    if size > READ_ONLY_FILE_BYTES {
        format!(
            "{base}, and files larger than {} are read-only.",
            format_size(READ_ONLY_FILE_BYTES)
        )
    } else {
        format!("{base}.")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loaded_size_crossing_advertised_threshold_locks_initial_load_and_reload() {
        let advertised = (READ_ONLY_FILE_BYTES - 1) as usize;
        let actual = (READ_ONLY_FILE_BYTES + 1) as usize;
        assert!(!loaded_file_state(advertised, false).1);
        assert_eq!(loaded_file_state(actual, false), (actual as u64, true));
        assert!(!loaded_file_state(READ_ONLY_FILE_BYTES as usize, false).1);
        assert!(
            loaded_file_state(actual, false).1,
            "a formerly writable buffer becomes protected when disk bytes grow"
        );
        assert!(
            loaded_file_state(1, true).1,
            "reload and shrink preserve existing manual/library protection"
        );
    }

    fn wait_for_finished<T>(manager: &TaskManager, handle: &TaskHandle<T>) {
        let start = std::time::Instant::now();
        while !manager.finished().iter().any(|task| task.id == handle.id()) {
            assert!(start.elapsed() < std::time::Duration::from_secs(10));
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    #[test]
    fn reindex_drops_finished_legacy_jobs_and_cached_symbols() {
        let manager = TaskManager::new();
        let analysis = Arc::new(Analysis::for_project(Path::new("/old-project")));
        let mut app = IdeApp::new(Settings::default(), None);
        let remap = manager
            .task("old remap")
            .spawn(|_| Ok(vec![("old".into(), "old_module".into())]));
        wait_for_finished(&manager, &remap);
        let index = manager.task("old index").spawn(|_| {
            Ok((
                Arc::new(vec![(
                    "old".into(),
                    "detail".into(),
                    PathBuf::from("/old-project/m.py"),
                    1,
                )]),
                vec![("old".into(), "old_module".into())],
            ))
        });
        wait_for_finished(&manager, &index);
        let remap_token = remap.token().clone();
        let index_token = index.token().clone();
        app.bg.remap = Some(RemapJob {
            root: PathBuf::from("/old-project"),
            db: Arc::downgrade(&analysis),
            handle: remap,
        });
        app.bg.index = Some(IndexJob {
            root: PathBuf::from("/old-project"),
            db: Arc::downgrade(&analysis),
            handle: index,
        });
        app.bg.index_symbols = Arc::new(vec![(
            "old".into(),
            "detail".into(),
            PathBuf::from("/old-project/m.py"),
            1,
        )]);
        app.bg.project_symbols = Arc::new(vec![("old".into(), "old_module".into())]);
        app.bg.symbols_db = Arc::downgrade(&analysis);
        app.reindex();
        assert!(remap_token.is_cancelled() && index_token.is_cancelled());
        assert!(!app.poll_index());
        assert!(app.bg.index_symbols.is_empty() && app.bg.project_symbols.is_empty());
        assert!(app.bg.symbols_db.upgrade().is_none());
    }
    #[test]
    fn legacy_index_and_remap_reject_results_from_replaced_analysis_same_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(dir.path()).unwrap();
        let manager = TaskManager::new();
        let old = Arc::new(Analysis::for_project(&root));
        let current = Arc::new(Analysis::for_project(&root));
        let mut app = IdeApp::new(Settings::default(), None);
        app.project = Some(Project {
            root: root.clone(),
            tree: ProjectTree::new(root.clone()),
            analysis: current,
            run_configs: RunConfigStore::load(&root),
            interpreters: Vec::new(),
            interpreter: None,
            interpreter_rx: None,
            git: None,
            git_refreshed: -100.0,
            git_branch: None,
        });
        let remap = manager
            .task("obsolete remap")
            .spawn(|_| Ok(vec![("obsolete".into(), "old_module".into())]));
        wait_for_finished(&manager, &remap);
        let index = manager.task("obsolete index").spawn(|_| {
            Ok((
                Arc::new(vec![(
                    "obsolete".into(),
                    "detail".into(),
                    PathBuf::from("/old/m.py"),
                    1,
                )]),
                vec![("obsolete".into(), "old_module".into())],
            ))
        });
        wait_for_finished(&manager, &index);
        app.bg.remap = Some(RemapJob {
            root: root.clone(),
            db: Arc::downgrade(&old),
            handle: remap,
        });
        app.bg.index = Some(IndexJob {
            root,
            db: Arc::downgrade(&old),
            handle: index,
        });
        assert!(!app.poll_index());
        assert!(app.bg.index_symbols.is_empty() && app.bg.project_symbols.is_empty());
        assert!(app.bg.symbols_db.upgrade().is_none());
    }

    #[test]
    fn reload_results_guard_session_tab_and_document_incarnation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.py");
        std::fs::write(&path, b"old=1\n").unwrap();
        let mut app = IdeApp::new(Settings::default(), None);
        app.open_file(&path);
        let id = app.tabs[0].view.id;
        let version = app.tabs[0].state.doc.version();
        let session = app.file_session;
        std::fs::write(&path, b"new=2\n").unwrap();
        let snapshot = app
            .file_vfs()
            .read(&path, app.editor_read_limits())
            .unwrap();
        let result = || {
            Ok((
                Document::from_bytes(&snapshot.bytes).unwrap(),
                snapshot.clone(),
            ))
        };
        app.apply_disk_reload(
            path.clone(),
            version,
            id,
            session.wrapping_add(1),
            IfModified::Ask,
            result(),
        );
        assert_eq!(app.tabs[0].state.doc.text(), "old=1\n");
        app.close_tab(0);
        std::fs::write(&path, b"open=3\n").unwrap();
        app.open_file(&path);
        app.apply_disk_reload(
            path.clone(),
            version,
            id,
            session,
            IfModified::Ask,
            result(),
        );
        assert_eq!(app.tabs[0].state.doc.text(), "open=3\n");
        let id = app.tabs[0].view.id;
        let version = app.tabs[0].state.doc.version();
        app.tabs[0].state.apply_text_edits(
            vec![(
                rustpy_core::TextRange::empty(rustpy_core::TextSize::new(0)),
                "# dirty\n".into(),
            )],
            "edit",
        );
        let edited = app.tabs[0].state.doc.text();
        app.apply_disk_reload(path, version, id, session, IfModified::Discard, result());
        assert_eq!(app.tabs[0].state.doc.text(), edited);
        assert!(matches!(app.dialog, Dialog::ExternalChange(_)));
    }
    #[test]
    fn encoding_preparation_cancels_when_its_editor_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("m.txt");
        std::fs::write(&path, b"owned disk").unwrap();
        let mut settings = Settings::default();
        settings.trust_project(directory.path());
        let mut app = IdeApp::new(settings, Some(directory.path().to_path_buf()));
        assert_eq!(app.project.as_ref().unwrap().root, directory.path());
        app.open_file(&path);
        let tab = &app.tabs[0];
        let request = super::super::encoding_ui::EncodingReloadRequest {
            path: path.clone(),
            encoding: rustpy_text::Encoding::Latin1,
            tab_id: tab.view.id,
            document: tab.state.doc.identity(),
            version: tab.state.doc.version(),
            session: app.file_session,
            generation: app.encoding_generation,
            revision: None,
            convert: false,
        };
        app.prepare_large_encoding_reload(request);
        app.tabs[0].state.apply_text_edits(
            vec![(
                rustpy_core::TextRange::empty(rustpy_core::TextSize::new(0)),
                "current edit".into(),
            )],
            "Changed during preparation",
        );
        app.poll_encoding_preparations();
        assert!(app.bg.encoding_preparations.is_empty());
        assert!(matches!(app.dialog, Dialog::None));
        assert_eq!(app.tabs[0].state.doc.text(), "current editowned disk");
        assert!(app.encoding_overrides.is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), b"owned disk");
    }

    #[test]
    fn worker_rejects_same_metadata_rewrite_during_decode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("m.py");
        std::fs::write(&path, b"x=1\n").unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        let file = path.clone();
        let result = TaskManager::global()
            .task("test raced disk decode")
            .spawn(move |progress| {
                let vfs = rustpy_core::vfs::Vfs::new();
                decode_verified_snapshot(
                    &vfs,
                    &file,
                    rustpy_core::vfs::ReadLimits::default(),
                    progress,
                    |bytes| {
                        let doc = Document::from_bytes(bytes).unwrap();
                        std::fs::write(&file, b"x=2\n").unwrap();
                        std::fs::File::options()
                            .write(true)
                            .open(&file)
                            .unwrap()
                            .set_times(std::fs::FileTimes::new().set_modified(mtime))
                            .unwrap();
                        Ok(doc)
                    },
                )
            })
            .wait();
        assert!(
            matches!(result, Err(TaskError::Failed(ref message)) if message.contains("changed while being read"))
        );
        assert_eq!(std::fs::read(path).unwrap(), b"x=2\n");
    }

    #[test]
    fn power_save_mode_turns_background_features_off() {
        let mut s = Settings::default();
        s.completion.show_as_you_type = true;
        s.semantic_highlighting = true;
        s.editor.highlight_usages_on_caret_move = true;
        assert_eq!(
            BackgroundFeatures::of(&s),
            BackgroundFeatures {
                code_analysis: true,
                completion_popup: true,
                semantic_highlighting: true,
                usage_highlighting: true
            }
        );
        // Each feature still follows its own setting.
        s.semantic_highlighting = false;
        s.completion.show_as_you_type = false;
        let f = BackgroundFeatures::of(&s);
        assert!(
            f.code_analysis
                && f.usage_highlighting
                && !f.semantic_highlighting
                && !f.completion_popup
        );
        // Power Save Mode wins over every setting.
        s.semantic_highlighting = true;
        s.completion.show_as_you_type = true;
        s.power_save_mode = true;
        assert_eq!(
            BackgroundFeatures::of(&s),
            BackgroundFeatures {
                code_analysis: false,
                completion_popup: false,
                semantic_highlighting: false,
                usage_highlighting: false
            }
        );
    }

    #[test]
    fn git_messages_name_the_outcome() {
        let ok = |s: &str| -> TaskResult<String> { Ok(s.to_string()) };
        assert_eq!(GitOp::Fetch.message(&ok("")), "Fetched");
        assert_eq!(
            GitOp::Pull.message(&ok("Updating 1..2\nFast-forward\n")),
            "Pulled: Fast-forward"
        );
        assert_eq!(
            GitOp::Pull.message(&Err(TaskError::Cancelled)),
            "Pull cancelled"
        );
        assert_eq!(
            GitOp::Fetch.message(&Err(TaskError::Failed(
                "git fetch failed:\nfatal: no remote\n".into()
            ))),
            "Fetch failed: fatal: no remote"
        );
    }

    #[test]
    fn git_operations_wait_for_the_running_one() {
        assert_eq!(
            git_refusal(GitOp::Pull, GitOp::Fetch),
            "Fetch is running: wait for it to finish before pulling"
        );
        assert_eq!(
            git_refusal(GitOp::Fetch, GitOp::Pull),
            "Pull is running: wait for it to finish before fetching"
        );
    }

    #[test]
    fn heads_sizes_and_explanations() {
        let dir = std::env::temp_dir().join(format!("rustpy-bg-heads-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("big.bin");
        std::fs::write(&p, vec![7u8; 10_000]).unwrap();
        let (head, size) = read_head(&p, 4096).unwrap();
        assert_eq!((head.len(), size), (4096, 10_000));
        let (all, _) = read_head(&p, 1 << 20).unwrap();
        assert_eq!(all.len(), 10_000);
        assert!(read_head(&dir.join("missing"), 10).is_none());
        assert_eq!(format_size(35 * 1024 * 1024 + 200 * 1024), "35.2 MB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3.0 GB");
        assert!(!large_file_explanation(30 << 20).contains("read-only"));
        assert!(large_file_explanation(200 << 20).contains("read-only"));
        // A cancelled read stops.
        let progress = ProgressReporter::new();
        assert_eq!(
            read_with_progress(&p, size, &progress).unwrap().len(),
            10_000
        );
        assert_eq!(progress.progress().2, Some(1.0));
        progress.token().cancel();
        assert_eq!(
            read_with_progress(&p, size, &progress),
            Err(TaskError::Cancelled)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn module_symbols_follow_the_database() {
        let db = Analysis::for_project(Path::new("/proj"));
        let symbols: Vec<Symbol> = vec![
            (
                "Foo".into(),
                String::new(),
                PathBuf::from("/proj/pkg/mod.py"),
                0,
            ),
            (
                "bar".into(),
                "()".into(),
                PathBuf::from("/proj/pkg/__init__.py"),
                3,
            ),
            (
                "out".into(),
                String::new(),
                PathBuf::from("/elsewhere/x.py"),
                0,
            ),
        ];
        let progress = ProgressReporter::new();
        assert_eq!(
            module_symbols(&db, &symbols, &progress).unwrap(),
            vec![
                ("Foo".to_string(), "pkg.mod".to_string()),
                ("bar".to_string(), "pkg".to_string())
            ]
        );
        progress.token().cancel();
        assert!(module_symbols(&db, &symbols, &progress).is_err());
    }
    #[test]
    fn real_large_file_worker_completes_shared_placeholder_after_original_view_closes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("large.txt");
        let mut bytes = "line = 'π🙂'\n".repeat((LARGE_FILE_GATE_BYTES as usize / 18) + 1);
        while bytes.len() <= LARGE_FILE_GATE_BYTES as usize {
            bytes.push_str("last\n");
        }
        std::fs::write(&path, bytes.as_bytes()).unwrap();
        let mut app = IdeApp::new(rustpy_settings::Settings::default(), None);
        assert!(app.open_large_file(&path));
        app.split_shared_editor(super::super::SplitDir::Horizontal);
        assert_eq!(app.tabs.len(), 2);
        let original = app.tabs[0].state.doc.identity();
        assert!(app.tabs[0]
            .state
            .doc
            .shares_authority(&app.tabs[1].state.doc));
        app.load_large_file(&path, bytes.len() as u64);
        app.request_close_tab(0);
        assert_eq!(app.tabs.len(), 1);
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while app.is_large_file_placeholder(0) && std::time::Instant::now() < deadline {
            app.poll_large_files();
            std::thread::yield_now();
        }
        assert!(
            !app.is_large_file_placeholder(0),
            "owned background load did not complete"
        );
        assert_ne!(app.tabs[0].state.doc.identity(), original);
        assert_eq!(app.tabs[0].state.doc.text(), bytes);
        assert!(!app.tabs[0].state.doc.is_modified());
        assert!(!app.tabs[0].state.doc.is_read_only());
        app.split_shared_editor(super::super::SplitDir::Horizontal);
        assert!(app.tabs[0]
            .state
            .doc
            .shares_authority(&app.tabs[1].state.doc));
        assert_eq!(app.tabs[1].state.doc.len(), app.tabs[0].state.doc.len());
    }
}

#[cfg(test)]
#[path = "large_file_connected_tests.rs"]
mod large_file_connected_tests;
