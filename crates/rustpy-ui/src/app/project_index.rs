//! Project file index and background scans (roadmap §2.7 search scopes, §5
//! performance targets). Project-wide features see the files of the project
//! scope: the project directory and extra content roots, without the folders
//! marked Excluded. The list is built on a worker thread when the project
//! opens and again when files are created or deleted or folders are marked;
//! the Search Everywhere symbol scan and the TODO scan run on workers too.

use super::*;
use rustpy_project::ProjectScope;
use rustpy_search::FileIndex;

/// `(path, 0-based line, text)` rows for the TODO tool window.
pub(super) type TodoList = Vec<(PathBuf, usize, String)>;
type ColoredTodoList = Vec<((PathBuf, usize, String), [u8; 3])>;

/// Results retain the exact TODO rules and project/scope snapshot that produced them.
type TodoScanResult = (
    ColoredTodoList,
    rustpy_settings::todo::TodoSettings,
    Option<ProjectScope>,
    rustpy_settings::scopes::ScopeSettings,
);

#[derive(Default)]
pub(super) struct ProjectIndex {
    pub files: FileIndex,
    todos_rx: Option<crossbeam_channel::Receiver<TodoScanResult>>,
    todo_settings: Option<rustpy_settings::todo::TodoSettings>,
    todo_scope_settings: Option<rustpy_settings::scopes::ScopeSettings>,
    todo_editors: Vec<(PathBuf, rustpy_text::DocumentId, u64)>,
    contents_changed: bool,
    /// A file or folder was created, removed or renamed since the last rebuild.
    pub structure_changed: bool,
    /// `.rustpy/project.json` changed on disk (a pull, an edit): reload the structure.
    pub project_file_changed: bool,
    /// Why `.rustpy/project.json` cannot be read: until it is fixed the
    /// structure is not changed (that would overwrite the file).
    pub structure_error: Option<String>,
    /// Content roots outside the project directory that the file watcher covers.
    pub(super) watched_roots: Vec<PathBuf>,
    /// Walked folders the watcher could not cover: changes there go unnoticed,
    /// so the cached list is not trusted and project-wide features walk the disk.
    unwatched: Vec<PathBuf>,
    /// Whether the window had the keyboard focus last frame.
    window_focused: Option<bool>,
}

impl ProjectIndex {
    /// Notes a file-system event; creations, deletions and renames change the file list.
    pub(super) fn note_fs_event(&mut self, kind: &notify::EventKind) {
        self.contents_changed = true;
        if matches!(
            kind,
            notify::EventKind::Create(_)
                | notify::EventKind::Remove(_)
                | notify::EventKind::Modify(notify::event::ModifyKind::Name(_))
        ) {
            self.structure_changed = true;
        }
    }

    /// Notes the paths of a file-system event; true when `.rustpy/project.json`
    /// is among them (the caller schedules the debounced refresh).
    pub(super) fn note_project_file(&mut self, paths: &[PathBuf]) -> bool {
        let hit = paths.iter().any(|p| {
            p.file_name()
                .is_some_and(|n| n == rustpy_project::project::PROJECT_FILE)
                && p.parent()
                    .and_then(Path::file_name)
                    .is_some_and(|d| d == rustpy_settings::PROJECT_DIR_NAME)
        });
        self.project_file_changed |= hit;
        hit
    }

    /// A new file watcher (another project): nothing is watched yet.
    pub(super) fn forget_watches(&mut self) {
        self.watched_roots.clear();
        self.unwatched.clear();
    }

    /// The window's focus this frame; true when it was just gained.
    fn note_window_focus(&mut self, focused: Option<bool>) -> bool {
        let Some(now) = focused else { return false };
        let gained = now && self.window_focused == Some(false);
        self.window_focused = Some(now);
        gained
    }
}

impl IdeApp {
    /// What project-wide walks cover: the project directory and the extra content
    /// roots, without excluded folders (None without a project).
    pub(super) fn project_scope(&self) -> Option<ProjectScope> {
        let p = self.project.as_ref()?;
        let mut scope = ProjectScope::from_model(&p.root, &p.tree.model());
        scope.ignored_patterns = self.settings.editor.preferences.ignored_patterns.clone();
        Some(scope)
    }

    /// Whether the cached list can stand in for a walk of `scope`: it was built
    /// for exactly this scope, no rebuild is running or due (files came or went,
    /// the window was switched back to) and the watcher covers every walked
    /// folder. Otherwise a change may be missing from it.
    pub(super) fn cached_list_is_current(&self, scope: &ProjectScope) -> bool {
        let idx = &self.project_index;
        self.fs_watcher.is_some()
            && idx.unwatched.is_empty()
            && !idx.structure_changed
            && !idx.files.is_building()
            && idx.files.scope().as_ref() == Some(scope)
    }

    /// The files of the project scope (Find Usages, refactorings, Structural
    /// Search, …): the cached list while it is current, else a walk of the disk
    /// now, so they never work from a list that misses a file.
    pub(super) fn scoped_project_files(&self) -> Vec<PathBuf> {
        let Some(scope) = self.project_scope() else {
            return Vec::new();
        };
        if self.cached_list_is_current(&scope) {
            if let Some(files) = self.project_index.files.files() {
                return files.as_ref().clone();
            }
        }
        rustpy_project::files::project_files_in(&scope)
    }

    /// After a burst of file-system events (debounced): reloads a changed
    /// `project.json` and rebuilds the file index when files came or went.
    pub(super) fn after_fs_changes(&mut self) {
        if std::mem::take(&mut self.project_index.contents_changed) {
            self.todo_refreshed = -100.0;
        }
        if std::mem::take(&mut self.project_index.project_file_changed) {
            self.reload_project_structure();
        }
        if self.project_index.structure_changed {
            self.rebuild_file_index();
        }
    }

    /// File › Reload All from Disk, Invalidate Caches and the Project view's
    /// Refresh: reads `project.json` again, retries watching the content roots
    /// and rebuilds the file list (for changes the watcher missed).
    pub(super) fn refresh_project_files(&mut self) {
        self.code_vision.note_rescan();
        self.queue_open_file_reconciliation();
        self.reconcile_external_changes();
        let root = self.project.as_ref().map(|p| p.root.clone());
        self.project_index
            .unwatched
            .retain(|d| Some(d) == root.as_ref());
        self.reload_project_structure();
        self.rebuild_file_index();
    }

    /// Rebuilds the file index in the background (project opened, folders marked,
    /// files created or deleted); the TODO list is rescanned on next use (symbols: `symbol_index`).
    pub(super) fn rebuild_file_index(&mut self) {
        self.project_index.structure_changed = false;
        self.sync_content_root_watches();
        match self.project_scope() {
            Some(scope) => self.project_index.files.rebuild(scope),
            None => self.project_index.files.clear(),
        }
        self.todo_refreshed = -100.0;
    }

    /// A popup that lists project files opens: when changes may have been missed
    /// (a folder the watcher cannot cover), the list is rebuilt in the background.
    pub(super) fn refresh_unwatched_file_index(&mut self) {
        if !self.project_index.unwatched.is_empty() && !self.project_index.files.is_building() {
            self.rebuild_file_index();
        }
    }

    /// Watches the content roots outside the project directory (which
    /// `start_fs_watcher` watches) so that files created, deleted or renamed
    /// there update the list; detached roots are no longer watched.
    fn sync_content_root_watches(&mut self) {
        use notify::Watcher;
        let Some(scope) = self.project_scope() else {
            return;
        };
        let wanted: Vec<PathBuf> = scope
            .walk_roots()
            .into_iter()
            .filter(|r| *r != scope.root)
            .collect();
        for gone in self
            .project_index
            .watched_roots
            .iter()
            .filter(|r| !wanted.contains(r))
        {
            if let Some(w) = &mut self.fs_watcher {
                let _ = w.unwatch(gone);
            }
        }
        self.project_index
            .watched_roots
            .retain(|r| wanted.contains(r));
        self.project_index
            .unwatched
            .retain(|r| *r == scope.root || wanted.contains(r));
        for dir in wanted {
            if self.project_index.watched_roots.contains(&dir)
                || self.project_index.unwatched.contains(&dir)
                || !dir.is_dir()
            {
                continue;
            }
            let watched = match &mut self.fs_watcher {
                Some(w) => w
                    .watch(&dir, notify::RecursiveMode::Recursive)
                    .map_err(|e| e.to_string()),
                None => Err("the file watcher is not running".to_string()),
            };
            match watched {
                Ok(()) => self.project_index.watched_roots.push(dir),
                Err(e) => self.note_unwatched(&dir, &e),
            }
        }
        self.sync_external_file_watches();
    }

    /// The file watcher cannot cover `dir` (the inotify watch limit, a file
    /// system without change events): changes there go unnoticed, so the cached
    /// list is not trusted for project-wide features. Reported once.
    pub(super) fn note_unwatched(&mut self, dir: &Path, error: &str) {
        crate::performance_trace::event(
            "watcher_unavailable",
            serde_json::json!({"path":dir,"error":error}),
        );
        log::warn!("cannot watch {}: {error}", dir.display());
        if self.project_index.unwatched.iter().any(|d| d == dir) {
            return;
        }
        let first = self.project_index.unwatched.is_empty();
        self.project_index.unwatched.push(dir.to_path_buf());
        if first && self.settings.platform.warn_inotify_limit {
            let body = format!(
                "{} cannot be watched ({error}). Search, navigation and refactorings read the project from disk each time; File › Reload All from Disk refreshes the Project view and open editors.",
                dir.display()
            );
            self.notify(
                crate::notifications::NotificationKind::Warning,
                "File Watcher",
                "Changes made outside RustPy may be missed",
                body,
                Self::inotify_limit_actions(),
            );
        }
    }

    /// The scope's files for a worker thread: the cached list while it is
    /// current, else a walk on that thread.
    fn files_for_worker(&self) -> Option<impl FnOnce() -> Vec<PathBuf> + Send + 'static> {
        let scope = self.project_scope()?;
        let cached = if self.cached_list_is_current(&scope) {
            self.project_index.files.files()
        } else {
            None
        };
        Some(move || {
            cached
                .map(|f| f.as_ref().clone())
                .unwrap_or_else(|| rustpy_project::files::project_files_in(&scope))
        })
    }

    /// TODO tool window: rescans on a worker thread at most every 15 s while it is shown.
    pub(super) fn request_todo_scan(&mut self, now: f64) {
        let editor_versions: Vec<_> = self
            .tabs
            .iter()
            .filter_map(|tab| {
                tab.state.path.as_ref().map(|path| {
                    (
                        path.clone(),
                        tab.state.doc.identity(),
                        tab.state.doc.version(),
                    )
                })
            })
            .collect();
        if self.project_index.todo_scope_settings.as_ref() != Some(&self.settings.scopes)
            || self.project_index.todo_settings.as_ref() != Some(&self.settings.todo)
            || self.project_index.todo_editors != editor_versions
        {
            self.todo_refreshed = -100.0;
            self.todo_items.clear();
            self.todo_colors.clear();
        }
        if self.project_index.todos_rx.is_some() || now - self.todo_refreshed <= 15.0 {
            return;
        }
        let files = self.files_for_worker();
        let (tx, rx) = crossbeam_channel::bounded(1);
        let settings = self.settings.todo.clone();
        let scope = self.project_scope();
        let scope_settings = self.settings.scopes.clone();
        let mut scope_context = self.capture_scope_context();
        let compiled_scope = match scope_settings.compile(&scope_settings.todo) {
            Ok(scope) => scope,
            Err(error) => {
                self.set_status(error);
                return;
            }
        };
        let library_paths: Vec<PathBuf> = if compiled_scope.includes_non_project() {
            self.everywhere
                .index
                .library
                .as_ref()
                .map(|index| index.files().map(|(path, _, _)| path).collect())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        self.project_index.todo_scope_settings = Some(scope_settings.clone());
        self.project_index.todo_settings = Some(settings.clone());
        self.project_index.todo_editors = editor_versions;
        let overrides: std::collections::HashMap<_, _> = self
            .tabs
            .iter()
            .filter_map(|tab| {
                tab.state
                    .path
                    .as_ref()
                    .map(|path| (path.clone(), tab.state.doc.text()))
            })
            .collect();
        std::thread::spawn(move || {
            let mut paths = files.map(|files| files()).unwrap_or_default();
            paths.extend(overrides.keys().cloned());
            if let Some(scope) = &scope {
                for path in overrides.keys() {
                    if scope.contains(path) && !paths.contains(path) {
                        paths.push(path.clone());
                    }
                }
            }
            if scope_context.project_files.is_empty() {
                let mut project_files = paths.clone();
                project_files.sort();
                project_files.dedup();
                scope_context.project_files = Arc::new(project_files);
            }
            if compiled_scope.includes_non_project() {
                paths.extend(library_paths);
                paths.extend(overrides.keys().cloned());
                paths.extend(scope_context.changed_files.iter().cloned());
                paths.extend(scope_context.recently_viewed.iter().cloned());
                paths.extend(scope_context.recently_changed.iter().cloned());
                for root in &scope_context.scratch_roots {
                    paths.extend(crate::scratches::list(root));
                }
            }
            paths.sort();
            paths.dedup();
            paths.retain(|path| {
                overrides.contains_key(path)
                    || scope.as_ref().is_some_and(|scope| scope.contains(path))
                    || compiled_scope.matches(path, &scope_context)
            });
            let items = scan_todos_with_overrides(&paths, &settings, &overrides);
            let _ = tx.send((items, settings, scope, scope_settings));
        });
        self.project_index.todos_rx = Some(rx);
        self.todo_refreshed = now;
    }

    /// Whether the TODO list is being rescanned.
    pub(super) fn todo_scan_running(&self) -> bool {
        self.project_index.todos_rx.is_some()
    }

    /// Per frame: hands finished scans to their views, gives the search popups
    /// the index and scope, and refreshes them when the window is switched back
    /// to: changes made outside RustPy are not always reported (network drives,
    /// Windows programs writing to WSL's /mnt), so the project structure and the
    /// file list are reread as after file-system events.
    pub(super) fn poll_project_index(&mut self, ctx: &egui::Context) {
        if self
            .project_index
            .note_window_focus(ctx.input(|i| i.viewport().focused))
        {
            self.queue_open_file_reconciliation();
            self.reconcile_external_changes();
            self.reload_analysis_from_disk();
            self.project_index.structure_changed = true;
            self.project_index.project_file_changed = true;
            self.fs_dirty_since.get_or_insert(ctx.input(|i| i.time));
        }
        if let Some(rx) = &self.project_index.todos_rx {
            match rx.try_recv() {
                Ok((items, settings, scope, scope_settings)) => {
                    let versions: Vec<_> = self
                        .tabs
                        .iter()
                        .filter_map(|tab| {
                            tab.state.path.as_ref().map(|path| {
                                (
                                    path.clone(),
                                    tab.state.doc.identity(),
                                    tab.state.doc.version(),
                                )
                            })
                        })
                        .collect();
                    if settings == self.settings.todo
                        && scope_settings == self.settings.scopes
                        && scope == self.project_scope()
                        && versions == self.project_index.todo_editors
                    {
                        (self.todo_items, self.todo_colors) = items.into_iter().unzip();
                    } else {
                        self.todo_refreshed = -100.0;
                    }
                    self.project_index.todos_rx = None;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100))
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.project_index.todos_rx = None
                }
            }
        }
        let scope = self.project_scope();
        self.everywhere
            .set_index(self.project_index.files.clone(), scope.clone());
        self.find_in_files.set_project_scope(scope);
        if self.project_index.files.is_building()
            && (self.everywhere.visible || self.find_in_files.visible)
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
    }
}

/// Legacy entry point retained for old fixtures; production captures actual settings.
#[cfg(test)]
pub(super) fn scan_todos(files: &[PathBuf]) -> TodoList {
    scan_todos_with_settings(files, &rustpy_settings::todo::TodoSettings::default())
}
#[cfg(test)]
fn scan_todos_with_settings(
    files: &[PathBuf],
    settings: &rustpy_settings::todo::TodoSettings,
) -> TodoList {
    scan_todos_with_colors(files, settings)
        .into_iter()
        .map(|(item, _)| item)
        .collect()
}
#[cfg(test)]
fn scan_todos_with_colors(
    files: &[PathBuf],
    settings: &rustpy_settings::todo::TodoSettings,
) -> ColoredTodoList {
    scan_todos_with_overrides(files, settings, &Default::default())
}

fn scan_todos_with_overrides(
    files: &[PathBuf],
    settings: &rustpy_settings::todo::TodoSettings,
    overrides: &std::collections::HashMap<PathBuf, String>,
) -> ColoredTodoList {
    let Ok(patterns) = crate::todo_patterns::Patterns::compile(settings) else {
        return Vec::new();
    };
    let vfs = rustpy_core::vfs::Vfs::new();
    let mut out = Vec::new();
    for path in files {
        let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
        if !matches!(
            ext,
            "py" | "pyi"
                | "md"
                | "txt"
                | "rst"
                | "toml"
                | "cfg"
                | "ini"
                | "yaml"
                | "yml"
                | "sh"
                | "js"
                | "ts"
                | "tsx"
                | "jsx"
                | "vue"
                | "svelte"
                | "scss"
                | "less"
                | "json"
                | "jsonc"
                | "html"
                | "css"
        ) {
            continue;
        }
        let snapshot;
        let text = if let Some(text) = overrides.get(path) {
            if text.len() > 1024 * 1024 {
                continue;
            }
            text.as_str()
        } else {
            let Ok(read) = vfs.read(
                path,
                rustpy_core::vfs::ReadLimits {
                    max_file_bytes: 1024 * 1024,
                    max_cached_bytes: 0,
                    max_cached_files: 0,
                },
            ) else {
                continue;
            };
            snapshot = read;
            let Ok(text) = std::str::from_utf8(&snapshot.bytes) else {
                continue;
            };
            text
        };
        if let Some(items) = cached_todo_items(path, text, settings) {
            out.extend(
                items
                    .into_iter()
                    .take(5000usize.saturating_sub(out.len()))
                    .map(|(line, text, color)| ((path.clone(), line, text), color)),
            );
            if out.len() >= 5000 {
                break;
            }
            continue;
        }
        let comments = crate::todo_patterns::contexts(path, text, settings);
        let items: Vec<_> = patterns
            .scan(text, &comments)
            .into_iter()
            .take(5000)
            .map(|item| (item.line, item.text, item.color))
            .collect();
        store_todo_items(path, text, settings, items.clone());
        out.extend(
            items
                .into_iter()
                .take(5000usize.saturating_sub(out.len()))
                .map(|(line, text, color)| ((path.clone(), line, text), color)),
        );
        if out.len() >= 5000 {
            break;
        }
    }
    out.sort();
    out
}

type TodoRows = Vec<(usize, String, [u8; 3])>;
struct TodoEntry {
    text: Arc<str>,
    settings: rustpy_settings::todo::TodoSettings,
    rows: TodoRows,
    bytes: usize,
}
#[derive(Default)]
struct TodoCache {
    entries: std::collections::HashMap<PathBuf, TodoEntry>,
    bytes: usize,
}
fn todo_cache() -> &'static std::sync::Mutex<TodoCache> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<TodoCache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}
fn cached_todo_items(
    path: &Path,
    text: &str,
    settings: &rustpy_settings::todo::TodoSettings,
) -> Option<TodoRows> {
    let cache = todo_cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let entry = cache.entries.get(path)?;
    (entry.text.as_ref() == text && entry.settings == *settings).then(|| entry.rows.clone())
}
fn store_todo_items(
    path: &Path,
    text: &str,
    settings: &rustpy_settings::todo::TodoSettings,
    rows: TodoRows,
) {
    let bytes = text.len()
        + rows
            .iter()
            .map(|(_, text, _)| text.len() + std::mem::size_of::<(usize, String, [u8; 3])>())
            .sum::<usize>();
    const LIMIT: usize = 16 * 1024 * 1024;
    if bytes > LIMIT {
        return;
    }
    let mut cache = todo_cache()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(previous) = cache.entries.remove(path) {
        cache.bytes = cache.bytes.saturating_sub(previous.bytes);
    }
    if cache.entries.len() >= 2048 || cache.bytes.saturating_add(bytes) > LIMIT {
        cache.entries.clear();
        cache.bytes = 0;
    }
    cache.bytes += bytes;
    cache.entries.insert(
        path.to_path_buf(),
        TodoEntry {
            text: Arc::from(text),
            settings: settings.clone(),
            rows,
            bytes,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todo_scan_reads_only_the_given_files() {
        let dir = std::env::temp_dir().join(format!("rustpy-todo-scan-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("vendor")).unwrap();
        std::fs::write(
            dir.join("a.py"),
            "x = 1  # TODO: fix\ns = 'TODO in a string'\n",
        )
        .unwrap();
        std::fs::write(dir.join("notes.md"), "FIXME later\n").unwrap();
        std::fs::write(dir.join("vendor/b.py"), "# TODO vendored\n").unwrap();
        let scope = ProjectScope::new(&dir).with_excluded([dir.join("vendor")]);
        let todos = scan_todos(&rustpy_project::files::project_files_in(&scope));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            todos,
            vec![
                (dir.join("a.py"), 0, "TODO: fix".to_string()),
                (dir.join("notes.md"), 0, "FIXME later".to_string())
            ]
        );
    }

    #[test]
    fn creations_and_deletions_change_the_file_list() {
        use notify::event::{CreateKind, DataChange, ModifyKind, RemoveKind, RenameMode};
        let mut idx = ProjectIndex::default();
        idx.note_fs_event(&notify::EventKind::Modify(ModifyKind::Data(
            DataChange::Content,
        )));
        assert!(!idx.structure_changed, "saving a file keeps the list");
        idx.note_fs_event(&notify::EventKind::Create(CreateKind::File));
        assert!(idx.structure_changed);
        for kind in [
            notify::EventKind::Remove(RemoveKind::Folder),
            notify::EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
        ] {
            let mut idx = ProjectIndex::default();
            idx.note_fs_event(&kind);
            assert!(idx.structure_changed, "{kind:?}");
        }
    }

    /// A scratch directory, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let dir =
                std::env::temp_dir().join(format!("rustpy-index-test-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir.canonicalize().unwrap())
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// An IDE window on the project `root`, without the parts of opening a
    /// project that run programs or save the IDE settings (interpreter
    /// discovery, Git, `settings.json`).
    fn app_with_project(root: &Path) -> IdeApp {
        let mut app = IdeApp::new(Settings::default(), None);
        app.project = Some(Project {
            root: root.to_path_buf(),
            tree: ProjectTree::new(root.to_path_buf()),
            analysis: Arc::new(Analysis::for_project(root)),
            run_configs: RunConfigStore::load(root),
            interpreters: Vec::new(),
            interpreter: None,
            interpreter_rx: None,
            git: None,
            git_refreshed: -100.0,
            git_branch: None,
        });
        app.start_fs_watcher(root);
        app.load_project_structure(root);
        app.rebuild_file_index();
        assert!(app
            .project_index
            .files
            .wait_idle(std::time::Duration::from_secs(20)));
        app
    }

    /// Runs the file-system part of the frames (debounced events, rebuilds)
    /// until `done` holds.
    fn pump_until(app: &mut IdeApp, what: &str, done: impl Fn(&IdeApp) -> bool) {
        let start = std::time::Instant::now();
        let mut now = 0.0;
        while !done(app) {
            assert!(start.elapsed().as_secs() < 20, "{what}");
            now += 0.1;
            app.poll_fs_events(now);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    fn indexed(app: &IdeApp, path: &Path) -> bool {
        !app.project_index.files.is_building()
            && app
                .project_index
                .files
                .files()
                .is_some_and(|f| f.iter().any(|p| p == path))
    }

    #[test]
    fn files_created_in_a_content_root_reach_the_file_list() {
        let dir = Scratch::new();
        let base = dir.path().canonicalize().unwrap();
        let (root, shared) = (base.join("app"), base.join("shared"));
        std::fs::create_dir_all(root.join(".rustpy")).unwrap();
        std::fs::create_dir_all(shared.join("pkg")).unwrap();
        std::fs::write(
            root.join("main.py"),
            "from pkg.needle_shared import needle_shared\n",
        )
        .unwrap();
        std::fs::write(
            shared.join("pkg/needle_shared.py"),
            "def needle_shared(): pass\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".rustpy/project.json"),
            r#"{"content_roots": ["../shared"]}"#,
        )
        .unwrap();
        let mut app = app_with_project(&root);
        assert_eq!(
            app.project_index.watched_roots,
            vec![shared.clone()],
            "the content root is watched"
        );
        assert!(app
            .scoped_project_files()
            .contains(&shared.join("pkg/needle_shared.py")));
        // A file added to the content root while the IDE runs: Go to File and
        // refactorings (rename would otherwise miss this importer) see it.
        let late = shared.join("pkg/late_user.py");
        std::fs::write(
            &late,
            "from pkg.needle_shared import needle_shared\nneedle_shared()\n",
        )
        .unwrap();
        pump_until(&mut app, "the new file never reached the list", |app| {
            indexed(app, &late)
        });
        assert!(app.scoped_project_files().contains(&late));
        // Detaching the content root stops watching it.
        let mut model = app.project.as_ref().unwrap().tree.model();
        model.remove_content_root(&shared);
        assert!(app.apply_project_model(model));
        assert!(app.project_index.watched_roots.is_empty());
        assert!(!app.scoped_project_files().contains(&late));
    }

    #[test]
    fn the_list_is_not_trusted_when_changes_may_be_missed() {
        let dir = Scratch::new();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("a.py"), "a = 1\n").unwrap();
        let mut app = app_with_project(&root);
        let scope = app.project_scope().unwrap();
        assert!(app.cached_list_is_current(&scope));
        // Files came or went and the rebuild is still due: walk the disk.
        app.project_index.structure_changed = true;
        assert!(!app.cached_list_is_current(&scope));
        app.project_index.structure_changed = false;
        // A folder the watcher cannot cover (the inotify limit, a network drive):
        // every project-wide feature reads the disk, and the user is told once.
        app.note_unwatched(&root, "inotify watch limit reached");
        app.note_unwatched(&root.join("sub"), "inotify watch limit reached");
        assert_eq!(
            app.notifications
                .items()
                .iter()
                .filter(|n| n.group == "File Watcher")
                .count(),
            1
        );
        let b = root.join("b.py");
        std::fs::write(&b, "b = 2\n").unwrap();
        assert!(!indexed(&app, &b));
        assert!(
            app.scoped_project_files().contains(&b),
            "read from disk, not from the stale list"
        );
        // Search Everywhere / Go to File rebuild the list when they open.
        app.refresh_unwatched_file_index();
        assert!(app
            .project_index
            .files
            .wait_idle(std::time::Duration::from_secs(20)));
        assert!(indexed(&app, &b));
        // Without a running watcher the list is never trusted either.
        app.project_index.forget_watches();
        app.fs_watcher = None;
        assert!(!app.cached_list_is_current(&scope));
    }

    #[test]
    fn switching_back_to_the_window_and_explicit_refreshes_rebuild_the_list() {
        let dir = Scratch::new();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("a.py"), "a = 1\n").unwrap();
        let mut app = app_with_project(&root);
        let ctx = egui::Context::default();
        let frame = |app: &mut IdeApp, focused: bool| {
            let mut viewports = egui::ViewportIdMap::default();
            viewports.insert(
                egui::ViewportId::ROOT,
                egui::ViewportInfo {
                    focused: Some(focused),
                    ..Default::default()
                },
            );
            ctx.run_ui(
                egui::RawInput {
                    viewports,
                    ..Default::default()
                },
                |ui| app.poll_project_index(ui.ctx()),
            )
            .drop_without_applying_deltas();
        };
        frame(&mut app, true);
        assert!(!app.project_index.structure_changed && app.fs_dirty_since.is_none());
        frame(&mut app, false);
        assert!(!app.project_index.structure_changed);
        // Back from another program (that may have changed files unreported).
        frame(&mut app, true);
        assert!(
            app.project_index.structure_changed
                && app.project_index.project_file_changed
                && app.fs_dirty_since.is_some()
        );
        // Changes the watcher missed: Reload All from Disk, Invalidate Caches and
        // the Project view's Refresh rebuild the list.
        app.fs_watcher = None;
        app.fs_events = None;
        for (i, action) in [Action::Synchronize, Action::InvalidateCaches]
            .into_iter()
            .enumerate()
        {
            let missed = root.join(format!("missed_{i}.py"));
            std::fs::write(&missed, "x = 1\n").unwrap();
            if action == Action::InvalidateCaches {
                // Synchronize has loaded the dialog workspace's persisted
                // preferences. Set consent afterwards so lazy loading cannot
                // replace the fixture's accepted non-destructive rebuild choice.
                app.dialog_workspace.prefs.remember("rebuild-caches", true);
                assert_eq!(
                    app.dialog_workspace.prefs.answer("rebuild-caches"),
                    Some(true)
                );
            }
            app.handle_action(action, &ctx);
            assert!(app
                .project_index
                .files
                .wait_idle(std::time::Duration::from_secs(20)));
            assert!(indexed(&app, &missed), "{action:?}");
        }
    }

    #[test]
    fn a_conflicted_project_json_is_not_overwritten_by_structure_changes() {
        use rustpy_project::project::{ProjectModel, RootKind};
        let dir = Scratch::new();
        let root = dir.path().to_path_buf();
        for d in ["src", "tests", "lib", ".rustpy"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let conflicted = "<<<<<<< HEAD\n{\"source_roots\": [\"src\"], \"excluded\": [\"vendor\"]}\n=======\n{\"test_roots\": [\"tests\"], \"excluded\": [\"vendor\"]}\n>>>>>>> feature\n";
        std::fs::write(ProjectModel::path_for(&root), conflicted).unwrap();
        let mut app = app_with_project(&root);
        assert!(app.project_index.structure_error.is_some());
        // Project view › src › Mark Directory as › Test Sources Root: refused.
        app.mark_directory(&root.join("src"), Some(RootKind::Tests));
        assert_eq!(
            std::fs::read_to_string(ProjectModel::path_for(&root)).unwrap(),
            conflicted,
            "both sides of the conflict are kept"
        );
        assert!(app
            .notifications
            .items()
            .iter()
            .any(|n| n.title == "The project structure was not changed"));
        assert!(
            app.project.as_ref().unwrap().tree.test_roots.is_empty(),
            "nothing changed in memory either"
        );
        assert!(
            !app.apply_project_model(ProjectModel::default()),
            "the Project Structure page is refused too"
        );
        // The user resolves the conflict, unnoticed by the watcher: the next mark
        // reads the fixed file first and adds to it.
        std::fs::write(
            ProjectModel::path_for(&root),
            r#"{"source_roots": ["lib"], "test_roots": ["tests"]}"#,
        )
        .unwrap();
        app.mark_directory(&root.join("src"), Some(RootKind::Tests));
        assert!(app.project_index.structure_error.is_none());
        let saved = ProjectModel::load(&root);
        assert_eq!(
            saved.source_roots,
            vec![root.join("lib")],
            "the resolved file's marks are kept"
        );
        assert_eq!(saved.test_roots, vec![root.join("src"), root.join("tests")]);
    }

    #[test]
    fn project_file_changes_are_noticed() {
        let mut idx = ProjectIndex::default();
        assert!(!idx.note_project_file(&[
            PathBuf::from("/p/.rustpy/workspace.json"),
            PathBuf::from("/p/project.json")
        ]));
        assert!(!idx.project_file_changed);
        assert!(idx.note_project_file(&[
            PathBuf::from("/p/.rustpy/project.json.tmp"),
            PathBuf::from("/p/.rustpy/project.json")
        ]));
        assert!(idx.project_file_changed);
    }
    #[test]
    fn configured_todo_scan_uses_case_and_continuation_without_string_false_positive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("custom.py");
        std::fs::write(
            &path,
            "x='# REVIEW fake'\n# REVIEW π\n#  more 😀\n# review lower\n",
        )
        .unwrap();
        let settings = rustpy_settings::todo::TodoSettings {
            patterns: vec![rustpy_settings::todo::TodoPattern {
                regex: r"\bREVIEW\b".into(),
                case_sensitive: true,
                color: [1, 2, 3],
            }],
            multiline: true,
            ..Default::default()
        };
        assert_eq!(
            scan_todos_with_settings(std::slice::from_ref(&path), &settings),
            vec![(path, 1, "REVIEW π\nmore 😀".into())]
        );
    }
    #[test]
    fn todo_index_tracks_unsaved_text_settings_and_source_reversion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("current.py");
        std::fs::write(&path, "# TODO disk\n").unwrap();
        let files = vec![path.clone()];
        let settings = rustpy_settings::todo::TodoSettings::default();
        let first = scan_todos_with_overrides(&files, &settings, &Default::default());
        assert_eq!(first.len(), 1);
        assert!(first[0].0 .2.contains("disk"));
        let overrides =
            std::collections::HashMap::from([(path.clone(), "# TODO editor\n".to_owned())]);
        let edited = scan_todos_with_overrides(&files, &settings, &overrides);
        assert_eq!(edited.len(), 1);
        assert!(edited[0].0 .2.contains("editor"));
        let reverted = scan_todos_with_overrides(&files, &settings, &Default::default());
        assert_eq!(reverted, first);
        std::fs::write(&path, "value = 'TODO not a comment'\n").unwrap();
        assert!(scan_todos_with_overrides(&files, &settings, &Default::default()).is_empty());
    }
    #[test]
    fn todo_scopes_filter_actual_sources_and_keep_unsaved_overrides() {
        use rustpy_settings::scopes::{ScopeContext, ScopeSelection, ScopeSettings};
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.py");
        let test = dir.path().join("test_source.py");
        std::fs::write(&source, "# TODO disk\n").unwrap();
        std::fs::write(&test, "# TODO excluded\n").unwrap();
        let paths = vec![source.clone(), test.clone()];
        let mut catalogue = paths.clone();
        catalogue.sort();
        let context = ScopeContext {
            root: dir.path().to_owned(),
            project_files: Arc::new(catalogue),
            ..Default::default()
        };
        let selected = ScopeSettings::default()
            .filter_paths(
                &ScopeSelection::Custom("file:*.py&&!file:test*".into()),
                &paths,
                &context,
            )
            .unwrap();
        let overrides =
            std::collections::HashMap::from([(source.clone(), "# TODO edited\n".to_owned())]);
        let items = scan_todos_with_overrides(&selected, &Default::default(), &overrides);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].0 .0, source);
        assert!(items[0].0 .2.contains("edited"));
        assert!(!items.iter().any(|item| item.0 .0 == test));
    }
}
