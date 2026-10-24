//! Application side of inlay code vision: real watcher/buffer generations, background
//! repository HEAD refresh, producer scheduling and exact-declaration clicks.
use super::inlay_code_vision::{
    Availability, Lenses, Options, ProjectSnapshot, RepoSnapshot, Request, Scheduler, Stamp,
};
use super::*;
use crate::editor::code_vision_view::{Display, FindUsagesTarget};
use rustpy_core::{TaskError, TaskHandle, TaskManager};
use rustpy_project::ProjectScope;
use rustpy_text::DocumentId;
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

type Buffers = Vec<(DocumentId, u64, Option<PathBuf>)>;
#[derive(Default)]
struct ContentClock {
    generation: u64,
    repository: u64,
    scope: Option<ProjectScope>,
    buffers: Buffers,
}
fn advance(generation: &mut u64) {
    *generation = generation
        .checked_add(1)
        .expect("Code Vision generation exhausted");
}
impl ContentClock {
    fn observe(&mut self, scope: Option<ProjectScope>, mut buffers: Buffers) {
        buffers.sort();
        if self.scope != scope {
            self.scope = scope;
            advance(&mut self.generation);
            advance(&mut self.repository);
        }
        if self.buffers != buffers {
            self.buffers = buffers;
            advance(&mut self.generation);
        }
    }
    fn event(&mut self, event: &notify::Event) {
        if matches!(event.kind, notify::EventKind::Access(_)) {
            return;
        }
        let git = event
            .paths
            .iter()
            .any(|path| path.components().any(|part| part.as_os_str() == ".git"));
        let source = event.need_rescan()
            || event.paths.iter().any(|path| {
                matches!(
                    path.extension().and_then(|extension| extension.to_str()),
                    Some("py" | "pyi")
                ) || matches!(
                    event.kind,
                    notify::EventKind::Create(_)
                        | notify::EventKind::Remove(_)
                        | notify::EventKind::Modify(notify::event::ModifyKind::Name(_))
                )
            });
        if source {
            advance(&mut self.generation);
        }
        if git {
            advance(&mut self.repository);
        }
    }
}
struct HeadJob {
    root: PathBuf,
    event_generation: u64,
    handle: TaskHandle<Option<String>>,
}
#[derive(Default)]
struct RepositoryCache {
    root: Option<PathBuf>,
    event_generation: u64,
    generation: u64,
    cached: Option<RepoSnapshot>,
    job: Option<HeadJob>,
    last_refresh: Option<Instant>,
}
impl RepositoryCache {
    fn maintain(&mut self, root: Option<PathBuf>, event_generation: u64) {
        if self.root != root || self.event_generation != event_generation {
            self.root = root;
            self.event_generation = event_generation;
            self.cached = None;
            self.last_refresh = None;
            advance(&mut self.generation);
            if let Some(job) = &self.job {
                job.handle.cancel();
            }
        }
        if let Some(result) = self.job.as_mut().and_then(|job| job.handle.try_result()) {
            let job = self.job.take().unwrap();
            if !job.handle.is_cancelled()
                && Some(&job.root) == self.root.as_ref()
                && job.event_generation == self.event_generation
            {
                match result {
                    Ok(head) => {
                        let changed = self
                            .cached
                            .as_ref()
                            .is_none_or(|cached| cached.head != head);
                        if changed {
                            advance(&mut self.generation);
                        }
                        self.cached = Some(RepoSnapshot {
                            root: job.root,
                            head,
                            generation: self.generation,
                        });
                    }
                    Err(_) => {
                        if self.cached.take().is_some() {
                            advance(&mut self.generation);
                        }
                    }
                }
            }
        }
        // Existing root watchers invalidate immediately. A periodic worker also
        // observes external shared-repository HEAD changes absent from that watcher.
        if self.job.is_none()
            && self
                .last_refresh
                .is_none_or(|last| last.elapsed() >= Duration::from_secs(1))
        {
            if let Some(root) = self.root.clone() {
                let expected = root.clone();
                let handle = TaskManager::global()
                    .task("Code Vision repository identity")
                    .hidden()
                    .spawn(move |progress| {
                        progress.check()?;
                        let repo = GitRepo::discover(&expected)
                            .ok_or_else(|| TaskError::from("Repository unavailable"))?;
                        if repo.root() != expected {
                            return Err(TaskError::from("Repository root changed"));
                        }
                        let head = repo.code_vision_head().map_err(TaskError::from)?;
                        progress.check()?;
                        Ok(head)
                    });
                self.last_refresh = Some(Instant::now());
                self.job = Some(HeadJob {
                    root,
                    event_generation: self.event_generation,
                    handle,
                });
            }
        }
    }
    fn cancel(&mut self) {
        if let Some(job) = &self.job {
            job.handle.cancel();
        }
        self.cached = None;
    }
}
struct FindJob {
    request: Request,
    target: FindUsagesTarget,
    handle: TaskHandle<Vec<Usage>>,
}
/// Root adds one Default Runtime field and temporarily takes it when calling
/// App methods. No source scans/blame/HEAD reads occur on the UI thread.
type CachedFiles = (u64, Arc<Vec<PathBuf>>, Arc<[PathBuf]>);

#[derive(Default)]
pub(super) struct Runtime {
    scheduler: Scheduler,
    clock: ContentClock,
    repo: RepositoryCache,
    request: Option<Request>,
    display: Option<Display>,
    find: Option<FindJob>,
    queued_find: Option<(Request, FindUsagesTarget)>,
    files: Option<CachedFiles>,
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.cancel();
    }
}
impl Runtime {
    pub(super) fn gutter_catalogue(&mut self, index: &rustpy_search::FileIndex) -> Arc<[PathBuf]> {
        self.catalogue(index)
    }
    /// Shared watcher/buffer authority remains usable with all Code Vision flags off.
    pub(super) fn gutter_content_generation(
        &mut self,
        scope: Option<ProjectScope>,
        buffers: Vec<(DocumentId, u64, Option<PathBuf>)>,
    ) -> u64 {
        self.clock.observe(scope, buffers);
        self.clock.generation
    }
    /// Root calls for every drained non-Access event BEFORE its .git/.rustpy
    /// filtering, and calls note_rescan for errors/overflow/manual refresh.
    pub fn note_fs_event(&mut self, event: &notify::Event) {
        self.clock.event(event);
    }
    pub fn note_rescan(&mut self) {
        advance(&mut self.clock.generation);
        advance(&mut self.clock.repository);
    }
    pub fn cancel(&mut self) {
        self.scheduler.cancel();
        self.repo.cancel();
        self.request = None;
        self.display = None;
        self.queued_find = None;
        if let Some(job) = &self.find {
            job.handle.cancel();
        }
    }
    pub fn display_for(&self, state: &EditorState) -> Option<&Display> {
        self.display
            .as_ref()
            .filter(|display| display.matches(state))
    }
    fn catalogue(&mut self, index: &rustpy_search::FileIndex) -> Arc<[PathBuf]> {
        let generation = index.generation();
        let Some(files) = index.files() else {
            return Arc::from([]);
        };
        if let Some((old_generation, old, filtered)) = &self.files {
            if *old_generation == generation && Arc::ptr_eq(old, &files) {
                return filtered.clone();
            }
        }
        let filtered: Arc<[PathBuf]> = files
            .iter()
            .filter(|path| {
                matches!(
                    path.extension().and_then(|extension| extension.to_str()),
                    Some("py" | "pyi")
                )
            })
            .cloned()
            .collect();
        self.files = Some((generation, files, filtered.clone()));
        filtered
    }
    fn adopt(&mut self, request: &Request, lenses: Lenses) {
        fn ready<T>(availability: Availability<Vec<T>>) -> Vec<T> {
            match availability {
                Availability::Ready(value) => value,
                _ => Vec::new(),
            }
        }
        let stamp = &request.stamp;
        self.display = Some(Display {
            document: stamp.document,
            version: stamp.version,
            path: stamp.path.clone(),
            source: stamp.source.clone(),
            returns: ready(lenses.returns),
            usages: ready(lenses.usages),
            authors: ready(lenses.authors),
            repo_head: stamp.repo.as_ref().and_then(|repo| repo.head.clone()),
        });
    }
}
impl IdeApp {
    /// Once per frame AFTER watcher drain/index maintenance/open-buffer sync.
    /// Runtime is passed explicitly so Root can integrate without borrow splitting.
    pub(super) fn code_vision_tick(&mut self, runtime: &mut Runtime, ctx: &egui::Context) {
        let options = Options {
            returns: self.settings.editor.inlay_return_hints,
            usages: self.settings.editor.code_vision_usages,
            authors: self.settings.editor.code_vision_author,
        };
        let Some(index) = self.active.filter(|index| self.tabs.get(*index).is_some()) else {
            runtime.cancel();
            return;
        };
        if !(options.returns || options.usages || options.authors)
            || self.safe_mode
            || self.settings.power_save_mode
            || self.tabs[index].state.language != Language::Python
            || self.tabs[index].state.is_large_file()
            || self.is_large_file_placeholder(index)
        {
            runtime.cancel();
            return;
        }
        let Some(project) = self.project.as_ref() else {
            runtime.cancel();
            return;
        };
        let scope = self.project_scope();
        let buffers = self
            .tabs
            .iter()
            .filter(|tab| tab.state.language == Language::Python)
            .map(|tab| {
                (
                    tab.state.doc.identity(),
                    tab.state.doc.version(),
                    tab.state.path.clone(),
                )
            })
            .collect();
        runtime.clock.observe(scope.clone(), buffers);
        let repo_root = project.git.as_ref().map(|repo| repo.root().to_path_buf());
        let analysis = project.analysis.clone();
        let project_root = project.root.clone();
        // The actual model's watcher-coverage/scope/build guard, made pub(super)
        // by Root's one visibility bridge; never call scoped_project_files here.
        let complete = scope
            .as_ref()
            .is_some_and(|scope| self.cached_list_is_current(scope));
        let Some(path) = self.tabs[index].state.path.clone() else {
            runtime.cancel();
            return;
        };
        let state = &self.tabs[index].state;
        let source = runtime
            .request
            .as_ref()
            .filter(|request| {
                request.stamp.document == state.doc.identity()
                    && request.stamp.version == state.doc.version()
                    && request.stamp.path == path
            })
            .map(|request| request.stamp.source.clone())
            .unwrap_or_else(|| Arc::from(state.doc.text()));
        if options.authors {
            runtime.repo.maintain(repo_root, runtime.clock.repository);
        } else {
            runtime.repo.cancel();
        }
        let before_generation = self.project_index.files.generation();
        let files = runtime.catalogue(&self.project_index.files);
        let after_generation = self.project_index.files.generation();
        let request = Request {
            analysis: analysis.clone(),
            stamp: Stamp {
                document: state.doc.identity(),
                version: state.doc.version(),
                path: path.clone(),
                source,
                semantic_revision: analysis.revision_excluding(&path),
                project: ProjectSnapshot {
                    root: project_root,
                    index_generation: after_generation,
                    content_generation: runtime.clock.generation,
                    complete: complete && before_generation == after_generation,
                    files,
                },
                repo: runtime.repo.cached.clone(),
                options,
                trusted: true,
                large_file: false,
            },
        };
        let old = runtime.display.clone();
        // Existing annotation/open-buffer lifecycle supplies the parse. This
        // frame must not parse or overwrite the database merely to show a lens.
        let parsed_current = analysis
            .cached_file_for_path(&path)
            .is_some_and(|file| file.is_open && file.text == request.stamp.source);
        let lenses = if parsed_current {
            runtime.scheduler.request(request.clone())
        } else {
            runtime.scheduler.cancel();
            fn pending<T>(enabled: bool) -> Availability<T> {
                if enabled {
                    Availability::Pending
                } else {
                    Availability::Disabled
                }
            }
            Lenses {
                returns: pending(options.returns),
                usages: pending(options.usages),
                authors: pending(options.authors),
            }
        };
        if matches!(lenses.returns, Availability::Pending)
            || matches!(lenses.usages, Availability::Pending)
            || matches!(lenses.authors, Availability::Pending)
        {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        runtime.adopt(&request, lenses);
        if let Some(lenses) = parsed_current
            .then(|| runtime.scheduler.poll(&request))
            .flatten()
        {
            runtime.adopt(&request, lenses);
        }
        if old != runtime.display {
            self.tabs[index].view.wrap_cache = None;
            ctx.request_repaint();
        }
        self.code_vision_poll_find(runtime, &request);
        runtime.request = Some(request);
        if runtime.repo.job.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        // Includes periodic repository identity maintenance without continuous redraw.
        if options.authors {
            ctx.request_repaint_after(Duration::from_secs(1));
        }
    }
    /// An actual header event chooses the definition offset, not the current
    /// caret. The ordinary Find tool receives resolver-backed search results.
    pub(super) fn code_vision_find_usages(
        &mut self,
        runtime: &mut Runtime,
        target: FindUsagesTarget,
    ) {
        if self.safe_mode || self.settings.power_save_mode {
            return;
        }
        let Some(request) = runtime.request.clone() else {
            return;
        };
        let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.state.doc.identity() == target.document)
        else {
            return;
        };
        let state = &self.tabs[index].state;
        if state.doc.version() != target.version
            || state.path.as_ref() != Some(&target.path)
            || target.range.end() > state.doc.len()
            || state.doc.slice(target.range) != target.name
            || !request.stamp.project.complete
            || request.stamp.document != target.document
            || request.stamp.version != target.version
        {
            return;
        }
        if request.stamp.project.content_generation != runtime.clock.generation
            || request.stamp.project.index_generation != self.project_index.files.generation()
            || request.stamp.repo != runtime.repo.cached
            || request.analysis.revision_excluding(&target.path) != request.stamp.semantic_revision
            || self
                .project
                .as_ref()
                .is_none_or(|project| !Arc::ptr_eq(&project.analysis, &request.analysis))
            || self
                .project_scope()
                .is_none_or(|scope| !self.cached_list_is_current(&scope))
        {
            return;
        }
        // Accept only a range that this current rendered declaration supplied.
        if runtime.display.as_ref().is_none_or(|display| {
            !display
                .usages
                .iter()
                .any(|usage| usage.range == target.range && usage.name == target.name)
        }) {
            return;
        }
        if let Some(job) = &runtime.find {
            job.handle.cancel();
        }
        runtime.queued_find = Some((request, target));
        self.code_vision_start_find(runtime);
    }
    fn code_vision_start_find(&mut self, runtime: &mut Runtime) {
        if runtime.find.is_some() {
            return;
        }
        let Some((request, target)) = runtime.queued_find.take() else {
            return;
        };
        let worker = request.clone();
        let offset = target.range.start();
        let handle = TaskManager::global()
            .task(format!("Finding usages of '{}'", target.name))
            .hidden()
            .spawn(move |progress| {
                progress.check()?;
                let snapshot = worker
                    .analysis
                    .snapshot(&worker.stamp.project.files, progress.token())
                    .map_err(|error| {
                        TaskError::Failed(format!("Code Vision snapshot unavailable: {error:?}"))
                    })?;
                let file = snapshot
                    .analysis()
                    .file_for_path(&worker.stamp.path)
                    .ok_or_else(|| TaskError::from("Declaration unavailable"))?;
                if file.text != worker.stamp.source {
                    return Err(TaskError::from("Declaration changed"));
                }
                let usages = rustpy_search::usages::find_usages_with_progress(
                    snapshot.analysis(),
                    &file,
                    offset,
                    &worker.stamp.project.files,
                    progress,
                )?;
                if !snapshot.complete() {
                    return Err(TaskError::from("Usage dependencies unavailable"));
                }
                Ok(usages)
            });
        self.set_status(format!("Finding usages of '{}'…", target.name));
        runtime.find = Some(FindJob {
            request,
            target,
            handle,
        });
    }
    fn code_vision_poll_find(&mut self, runtime: &mut Runtime, current: &Request) {
        let Some(job) = runtime.find.as_mut() else {
            return;
        };
        if job.request.stamp != current.stamp
            || !Arc::ptr_eq(&job.request.analysis, &current.analysis)
        {
            job.handle.cancel();
        }
        let Some(result) = job.handle.try_result() else {
            return;
        };
        let job = runtime.find.take().unwrap();
        if job.handle.is_cancelled() {
            if runtime.queued_find.as_ref().is_some_and(|(queued, _)| {
                queued.stamp != current.stamp || !Arc::ptr_eq(&queued.analysis, &current.analysis)
            }) {
                runtime.queued_find = None;
            }
            self.code_vision_start_find(runtime);
            return;
        }
        match result {
            Ok(usages) => {
                self.usages_title = format!("Usages of '{}' ({})", job.target.name, usages.len());
                self.set_status(format!(
                    "{} usage(s) of '{}'",
                    usages.len(),
                    job.target.name
                ));
                self.usages = usages;
                self.bottom = Some(BottomTool::Find);
            }
            Err(TaskError::Failed(error)) => {
                self.set_status(format!("Find Usages unavailable: {error}"))
            }
            Err(TaskError::Cancelled) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_core::{TextRange, TextSize};
    fn fixture() -> (tempfile::TempDir, IdeApp, Runtime, egui::Context) {
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let path = root.join("main.py");
        let source = "def π():\n    return 42\n\nπ()\n";
        std::fs::write(&path, source).unwrap();
        let mut settings = Settings::default();
        settings.editor.inlay_return_hints = true;
        settings.editor.code_vision_usages = true;
        let mut app = IdeApp::new(settings, None);
        app.project = Some(Project {
            root: root.clone(),
            tree: ProjectTree::new(root.clone()),
            analysis: Arc::new(Analysis::for_project(&root)),
            run_configs: RunConfigStore::default(),
            interpreters: vec![],
            interpreter: None,
            interpreter_rx: None,
            git: None,
            git_refreshed: -100.0,
            git_branch: None,
        });
        let mut state = EditorState::from_text(source, Language::Python);
        state.path = Some(path);
        app.push_tab(state);
        app.sync_open_tabs_now();
        app.start_fs_watcher(&root);
        app.project_index
            .files
            .rebuild_now(app.project_scope().unwrap());
        (directory, app, Runtime::default(), egui::Context::default())
    }
    fn ready(app: &mut IdeApp, runtime: &mut Runtime, ctx: &egui::Context) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            app.code_vision_tick(runtime, ctx);
            if runtime
                .display
                .as_ref()
                .is_some_and(|display| !display.returns.is_empty() && !display.usages.is_empty())
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "connected Code Vision did not publish"
            );
            std::thread::yield_now();
        }
    }
    #[test]
    fn actual_app_tick_publishes_flags_and_hides_on_safe_mode_edit_and_incomplete_index() {
        let (_directory, mut app, mut runtime, ctx) = fixture();
        ready(&mut app, &mut runtime, &ctx);
        let display = runtime.display_for(&app.tabs[0].state).unwrap();
        assert_eq!(display.returns[0].label, " -> int");
        assert_eq!(display.usages[0].usages, 1);
        let generation = runtime.clock.generation;
        app.tabs[0]
            .state
            .insert_text("# changed\n", &EditOptions::default());
        app.code_vision_tick(&mut runtime, &ctx);
        assert!(runtime.clock.generation > generation);
        assert!(runtime
            .display_for(&app.tabs[0].state)
            .unwrap()
            .usages
            .is_empty());
        app.safe_mode = true;
        app.code_vision_tick(&mut runtime, &ctx);
        assert!(runtime.display_for(&app.tabs[0].state).is_none());
        app.safe_mode = false;
        app.sync_open_tabs_now();
        app.project_index.structure_changed = true;
        app.code_vision_tick(&mut runtime, &ctx);
        assert!(!runtime.request.as_ref().unwrap().stamp.project.complete);
        assert!(runtime
            .display_for(&app.tabs[0].state)
            .unwrap()
            .usages
            .is_empty());
    }
    #[test]
    fn uncached_or_stale_parse_stays_pending_without_ui_parsing() {
        let (_directory, mut app, mut runtime, ctx) = fixture();
        let analysis = app.project.as_ref().unwrap().analysis.clone();
        let path = app.tabs[0].state.path.clone().unwrap();
        let missing = path.with_file_name("uncaptured.py");
        std::fs::write(&missing, "value = 42\n").unwrap();
        assert!(analysis.cached_file_for_path(&missing).is_none());
        app.tabs[0]
            .state
            .insert_text("# changed\n", &EditOptions::default());
        let old = analysis.cached_file_for_path(&path).unwrap();
        app.code_vision_tick(&mut runtime, &ctx);
        assert!(Arc::ptr_eq(
            &old,
            &analysis.cached_file_for_path(&path).unwrap()
        ));
        assert!(runtime.display.as_ref().unwrap().returns.is_empty());
        assert!(runtime.display.as_ref().unwrap().usages.is_empty());
        assert!(analysis.cached_file_for_path(&missing).is_none());
        // The existing lifecycle, not this lens tick, installs the current parse.
        app.sync_open_tabs_now();
        ready(&mut app, &mut runtime, &ctx);
    }
    #[test]
    fn real_owned_watcher_changes_content_authority_without_index_membership_change() {
        let (directory, mut app, mut runtime, ctx) = fixture();
        ready(&mut app, &mut runtime, &ctx);
        let index_generation = app.project_index.files.generation();
        let content_generation = runtime.clock.generation;
        let path = directory.path().join("main.py");
        std::fs::write(&path, "def π():\n    return 41\n\nπ()\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut observed = false;
        while !observed {
            if let Some(receiver) = &app.fs_events {
                while let Ok(Ok(event)) = receiver.try_recv() {
                    if !matches!(event.kind, notify::EventKind::Access(_))
                        && event.paths.iter().any(|event_path| event_path == &path)
                    {
                        runtime.note_fs_event(&event);
                        observed = true;
                    }
                }
            }
            assert!(
                Instant::now() < deadline,
                "owned watcher did not observe change"
            );
            std::thread::yield_now();
        }
        app.code_vision_tick(&mut runtime, &ctx);
        assert_eq!(app.project_index.files.generation(), index_generation);
        assert!(runtime.clock.generation > content_generation);
        assert!(runtime
            .display_for(&app.tabs[0].state)
            .unwrap()
            .usages
            .is_empty());
    }
    #[test]
    fn exact_declaration_click_runs_real_app_find_tool_without_replaying_caret() {
        let (_directory, mut app, mut runtime, ctx) = fixture();
        ready(&mut app, &mut runtime, &ctx);
        let usage = runtime.display.as_ref().unwrap().usages[0].clone();
        let end = app.tabs[0].state.doc.len();
        app.tabs[0]
            .state
            .set_carets(vec![crate::editor::state::Caret::at(end)]);
        let before = app.tabs[0].state.carets.clone();
        let source = app.tabs[0].state.doc.text();
        let target = FindUsagesTarget {
            document: app.tabs[0].state.doc.identity(),
            version: app.tabs[0].state.doc.version(),
            path: app.tabs[0].state.path.clone().unwrap(),
            range: usage.range,
            name: usage.name,
        };
        app.code_vision_find_usages(&mut runtime, target);
        let deadline = Instant::now() + Duration::from_secs(3);
        while runtime.find.is_some() {
            app.code_vision_tick(&mut runtime, &ctx);
            assert!(
                Instant::now() < deadline,
                "actual Find Usages did not complete"
            );
            std::thread::yield_now();
        }
        assert!(matches!(app.bottom, Some(BottomTool::Find)));
        let call = source.rfind("π()").unwrap() as u32;
        assert!(app.usages.iter().any(|usage| !usage.is_definition
            && usage.range == TextRange::new(TextSize::new(call), TextSize::new(call + 2))));
        assert_eq!(app.tabs[0].state.carets, before);
        assert_eq!(app.tabs[0].state.doc.text(), source);
    }
    #[test]
    fn actual_unborn_git_head_is_loaded_on_worker_and_rescan_invalidates_cached_identity() {
        let (directory, mut app, mut runtime, ctx) = fixture();
        app.project.as_mut().unwrap().git = Some(GitRepo::init(directory.path()).unwrap());
        app.settings.editor.code_vision_author = true;
        ready(&mut app, &mut runtime, &ctx);
        let deadline = Instant::now() + Duration::from_secs(3);
        while runtime.repo.cached.is_none() {
            app.code_vision_tick(&mut runtime, &ctx);
            assert!(
                Instant::now() < deadline,
                "background real Git HEAD did not finish"
            );
            std::thread::yield_now();
        }
        assert_eq!(runtime.repo.cached.as_ref().unwrap().head, None);
        let generation = runtime.repo.cached.as_ref().unwrap().generation;
        runtime.note_rescan();
        app.code_vision_tick(&mut runtime, &ctx);
        assert!(runtime.repo.cached.is_none());
        assert!(runtime.repo.generation > generation);
        assert!(runtime.request.as_ref().unwrap().stamp.repo.is_none());
    }
}
