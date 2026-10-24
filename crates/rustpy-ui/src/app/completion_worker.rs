//! Lookup candidates computed off the UI thread: Python basic, smart and
//! docstring prompt completion, and the render-context names and members of
//! Django/Jinja/Chameleon templates, so completion never blocks the event
//! thread. A request returns to the UI at once (after
//! a short grace in which fast lookups still open in the same frame); its
//! result is applied when ready and refiltered by whatever was typed in the
//! meantime, and a request the editor has moved away from is cancelled.
use super::*;
use rustpy_core::{CancellationToken, TaskHandle, TaskManager};
use std::time::Duration;

/// How long a large file's automatic lookup may hold the frame for its result.
const LARGE_FILE_GRACE: Duration = Duration::from_millis(8);

/// Inference time one Python lookup may spend on candidate types.
pub(super) const INFERENCE_BUDGET: Duration = Duration::from_millis(400);

/// Where a request was made: the candidates computed for it serve the editor
/// while the text before the identifier is unchanged and the identifier was
/// only extended (typed characters refilter them).
#[derive(Clone)]
struct Origin {
    document: rustpy_text::DocumentId,
    text: Arc<str>,
    start: usize,
    caret: usize,
    /// The analysis the candidates came from.
    world: Option<usize>,
    mode: Mode,
}

/// Which Python lookup a request computes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Basic,
    /// Smart (expected-type) completion.
    Smart,
    /// Code in a docstring prompt, completed in an isolated analysis world.
    Doctest,
    /// A large file's lookup while typing (see [`IdeApp::large_file_lookup`]).
    Large,
}

/// What a large file's lookup found off the UI thread.
pub(super) enum Found {
    /// The caret is in a template-name string argument starting here.
    TemplateArgument(TextSize),
    /// An exclusive context provider claims the caret.
    Basic(rustpy_semantic::basic_completion::BasicCompletion),
    /// Python candidates.
    Items(Vec<CompletionItem>),
}

/// A large file's lookup: what it found and the file it analysed (the
/// editor text of the request, which the database may not have yet).
pub(super) struct LargeLookup {
    pub found: Found,
    pub file: Option<Arc<rustpy_semantic::FileInfo>>,
}

impl Origin {
    fn serves(
        &self,
        document: rustpy_text::DocumentId,
        text: &str,
        caret: usize,
        world: Option<usize>,
    ) -> bool {
        self.document == document
            && self.world == world
            && extends(&self.text, self.start, self.caret, text, caret)
    }
}

/// Whether the identifier `old[start..old_caret]` grew into `new[start..new_caret]`
/// with the text before it unchanged.
pub(super) fn extends(
    old: &str,
    start: usize,
    old_caret: usize,
    new: &str,
    new_caret: usize,
) -> bool {
    let (Some(old_head), Some(new_head)) = (old.get(..start), new.get(..start)) else {
        return false;
    };
    let (Some(typed_before), Some(typed)) = (old.get(start..old_caret), new.get(start..new_caret))
    else {
        return false;
    };
    old_head == new_head
        && typed.starts_with(typed_before)
        && typed.chars().all(rustpy_core::is_ident_continue)
}

struct Job<T> {
    origin: Origin,
    explicit: bool,
    second_press: bool,
    handle: TaskHandle<T>,
}

impl<T> Drop for Job<T> {
    fn drop(&mut self) {
        self.handle.cancel();
    }
}

/// What a template request computes from the project's views.
pub(super) enum TemplateQuery {
    /// Render-context variable names.
    Names,
    /// Members of `root` followed through `steps` (`{{ root.a.<caret> }}`).
    Members(String, Vec<rustpy_semantic::template_context::Step>),
}

pub(super) struct CompletionWorker {
    /// How long a request waits for its result before handing control back
    /// to the UI (which then shows the lookup's loading row).
    pub grace: Duration,
    /// Test hook: a slow provider (the worker sleeps this long first).
    #[cfg(test)]
    pub delay: Duration,
    python: Option<Job<Vec<CompletionItem>>>,
    python_done: Option<(Origin, Vec<CompletionItem>)>,
    template: Option<(String, Job<Vec<CompletionItem>>)>,
    template_done: Option<(String, Vec<CompletionItem>)>,
    large: Option<Job<Arc<LargeLookup>>>,
    large_done: Option<(Origin, Arc<LargeLookup>)>,
    /// `second_press` of the request whose result is being applied.
    resume: Option<bool>,
}

impl Default for CompletionWorker {
    fn default() -> Self {
        Self {
            // Unit tests drive lookups synchronously unless they opt in.
            grace: if cfg!(test) {
                Duration::from_secs(60)
            } else {
                Duration::from_millis(30)
            },
            #[cfg(test)]
            delay: Duration::ZERO,
            python: None,
            python_done: None,
            template: None,
            template_done: None,
            large: None,
            large_done: None,
            resume: None,
        }
    }
}

impl CompletionWorker {
    /// Whether a request is still being computed.
    pub fn pending(&self) -> bool {
        self.python.is_some() || self.template.is_some() || self.large.is_some()
    }

    /// Whether a template request is still being computed.
    pub fn template_pending(&self) -> bool {
        self.template.is_some()
    }

    pub fn cancel(&mut self) {
        self.python = None;
        self.template = None;
        self.large = None;
    }

    /// The `second_press` recorded with the request whose result is being
    /// applied now, if any.
    pub fn take_resume(&mut self) -> Option<bool> {
        self.resume.take()
    }

    fn spawn<T: Send + 'static>(
        &self,
        title: &str,
        work: impl FnOnce(&CancellationToken) -> T + Send + 'static,
    ) -> TaskHandle<T> {
        #[cfg(test)]
        let delay = self.delay;
        TaskManager::global()
            .task(title)
            .hidden()
            .spawn(move |progress| {
                #[cfg(test)]
                {
                    let until = std::time::Instant::now() + delay;
                    while std::time::Instant::now() < until {
                        progress.check()?;
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
                progress.check()?;
                let result = work(progress.token());
                progress.check()?;
                Ok(result)
            })
    }
}

fn world(analysis: &Arc<Analysis>) -> Option<usize> {
    Some(Arc::as_ptr(analysis) as usize)
}

impl IdeApp {
    /// Python candidates (smart completion's with `smart`) for the
    /// identifier at `prefix_start..caret`: the latest result when it still
    /// serves the editor, else a background request. `None` while that
    /// request runs; its result reopens the lookup (see
    /// [`IdeApp::poll_completion_worker`]).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn python_completion_candidates(
        &mut self,
        analysis: &Arc<Analysis>,
        file: &Arc<rustpy_semantic::FileInfo>,
        caret: TextSize,
        prefix_start: usize,
        explicit: bool,
        second_press: bool,
        smart: bool,
    ) -> Option<Vec<CompletionItem>> {
        let i = self.active?;
        let origin = Origin {
            document: self.tabs[i].state.doc.identity(),
            text: file.text.clone(),
            start: prefix_start,
            caret: usize::from(caret).min(file.text.len()),
            world: world(analysis),
            mode: if smart { Mode::Smart } else { Mode::Basic },
        };
        if let Some(found) = self.python_lookup(&origin) {
            return found;
        }
        let (db, source) = (analysis.clone(), file.clone());
        self.python_request(origin, explicit, second_press, move |token| {
            let token = Some(token.clone());
            if smart {
                rustpy_semantic::completion2::smart_complete_bounded(
                    &db,
                    &source,
                    caret,
                    INFERENCE_BUDGET,
                    token,
                )
            } else {
                features::complete_bounded(&db, &source, caret, INFERENCE_BUDGET, token)
            }
        })
    }

    /// Candidates for docstring prompt code at `prefix_start..caret` of the
    /// active editor: `build` makes the isolated analysis world, the virtual
    /// file and caret only when no current result serves the editor. `None`
    /// while a request runs (or when `build` finds nothing to complete).
    pub(super) fn doctest_completion_candidates(
        &mut self,
        prefix_start: usize,
        explicit: bool,
        build: impl FnOnce(&Self) -> Option<(Analysis, Arc<rustpy_semantic::FileInfo>, TextSize)>,
    ) -> Option<Vec<CompletionItem>> {
        let i = self.active?;
        let tab = &self.tabs[i];
        let text: Arc<str> = tab.state.doc.text().into();
        let origin = Origin {
            document: tab.state.doc.identity(),
            caret: usize::from(tab.state.primary().head()).min(text.len()),
            text,
            start: prefix_start,
            world: self.project.as_ref().and_then(|p| world(&p.analysis)),
            mode: Mode::Doctest,
        };
        if let Some(found) = self.python_lookup(&origin) {
            return found;
        }
        let (query, target, at) = build(self)?;
        self.python_request(origin, explicit, false, move |token| {
            features::complete_bounded(&query, &target, at, INFERENCE_BUDGET, Some(token.clone()))
        })
    }

    /// Whether editor `i` completes in the background while typing: a large
    /// Python file of the project. Its automatic lookups neither push the
    /// text into the analysis nor run a provider on the UI thread: the
    /// worker analyses the editor text (see [`IdeApp::large_file_lookup`]).
    pub(super) fn completes_in_background(&self, i: usize) -> bool {
        self.project.is_some()
            && !self.remote_bound(i)
            && !self.is_large_file_placeholder(i)
            && self.tabs.get(i).is_some_and(|tab| {
                tab.state.language == Language::Python
                    && !tab.state.is_large_file()
                    && usize::from(tab.state.doc.len())
                        >= super::analysis_jobs::BACKGROUND_SYNC_BYTES
                    && tab
                        .state
                        .path
                        .as_ref()
                        .is_some_and(|path| !path.starts_with("<typeshed>"))
            })
    }

    /// An automatic lookup in large editor `i` for the identifier starting at
    /// `prefix_start`: the latest result while it serves the editor (the
    /// identifier was only extended), else a request computed off the UI
    /// thread. The request analyses the editor text in the file's semantic
    /// context (the analysis's own copy when it already has this text) and
    /// asks the providers in the order the UI does: template names, the
    /// exclusive context providers, then the Python candidates. `None` while
    /// it runs; its result reopens the lookup.
    pub(super) fn large_file_lookup(
        &mut self,
        i: usize,
        prefix_start: usize,
    ) -> Option<Arc<LargeLookup>> {
        let analysis = self.project.as_ref()?.analysis.clone();
        let tab = &self.tabs[i];
        let path = tab.state.path.clone()?;
        let text: Arc<str> = tab.state.doc.text().into();
        let caret = usize::from(tab.state.primary().head()).min(text.len());
        let origin = Origin {
            document: tab.state.doc.identity(),
            text: text.clone(),
            start: prefix_start,
            caret,
            world: world(&analysis),
            mode: Mode::Large,
        };
        let worker = &mut self.completion_worker;
        if let Some(job) = worker.large.as_mut() {
            if let Some(result) = job.handle.try_result() {
                let job = worker.large.take().unwrap();
                if let Ok(found) = result {
                    worker.large_done = Some((job.origin.clone(), found));
                }
            }
        }
        let serves = |known: &Origin| {
            known.start == origin.start
                && known.serves(origin.document, &origin.text, origin.caret, origin.world)
        };
        if let Some((known, found)) = &worker.large_done {
            if serves(known) {
                return Some(found.clone());
            }
        }
        if worker.large.as_ref().is_some_and(|job| serves(&job.origin)) {
            return None;
        }
        // The analysis's copy of the file (maybe a few keystrokes old: it is
        // pushed in the background) gives the semantic context.
        let base = analysis.file_for_path(&path);
        let offset = TextSize::new(caret as u32);
        let mut handle = worker.spawn("Completing", move |token| {
            let Some(base) = base else {
                return Arc::new(LargeLookup {
                    found: Found::Items(Vec::new()),
                    file: None,
                });
            };
            let file = if *base.text == *text {
                base
            } else {
                Arc::new(base.with_candidate_text(text.to_string()))
            };
            let found = rustpy_semantic::infer::with_cancellation(token, || {
                if let Some(start) =
                    rustpy_semantic::framework_names::template_argument(&file, offset)
                {
                    Found::TemplateArgument(start)
                } else if let Some(context) =
                    rustpy_semantic::basic_completion::complete(&analysis, &file, offset)
                {
                    Found::Basic(context)
                } else {
                    Found::Items(features::complete_bounded(
                        &analysis,
                        &file,
                        offset,
                        INFERENCE_BUDGET,
                        Some(token.clone()),
                    ))
                }
            });
            Arc::new(LargeLookup {
                found,
                file: Some(file),
            })
        });
        // A large file's lookup rarely finishes within the usual grace: wait
        // briefly, then show the loading row rather than hold the frame.
        if let Some(Ok(found)) = handle.wait_timeout(worker.grace.min(LARGE_FILE_GRACE)) {
            worker.large_done = Some((origin, found.clone()));
            return Some(found);
        }
        worker.large = Some(Job {
            origin,
            explicit: false,
            second_press: false,
            handle,
        });
        None
    }

    /// `Some(Some(items))` when the latest result serves `origin`,
    /// `Some(None)` while a request for it runs, `None` when neither does.
    fn python_lookup(&mut self, origin: &Origin) -> Option<Option<Vec<CompletionItem>>> {
        let worker = &mut self.completion_worker;
        if let Some(job) = worker.python.as_mut() {
            if let Some(result) = job.handle.try_result() {
                let job = worker.python.take().unwrap();
                if let Ok(items) = result {
                    worker.python_done = Some((job.origin.clone(), items));
                }
            }
        }
        let serves = |known: &Origin| {
            known.start == origin.start
                && known.mode == origin.mode
                && known.serves(origin.document, &origin.text, origin.caret, origin.world)
        };
        if let Some((known, items)) = &worker.python_done {
            if serves(known) {
                return Some(Some(items.clone()));
            }
        }
        if worker
            .python
            .as_ref()
            .is_some_and(|job| serves(&job.origin))
        {
            return Some(None);
        }
        None
    }

    /// Starts `work` for `origin` (replacing any running request) and waits
    /// the short grace for it.
    fn python_request(
        &mut self,
        origin: Origin,
        explicit: bool,
        second_press: bool,
        work: impl FnOnce(&CancellationToken) -> Vec<CompletionItem> + Send + 'static,
    ) -> Option<Vec<CompletionItem>> {
        let worker = &mut self.completion_worker;
        worker.python = None;
        let mut handle = worker.spawn("Completing", work);
        if let Some(Ok(items)) = handle.wait_timeout(worker.grace) {
            worker.python_done = Some((origin, items.clone()));
            return Some(items);
        }
        worker.python = Some(Job {
            origin,
            explicit,
            second_press,
            handle,
        });
        None
    }

    /// Template candidates computed from the project's views (render-context
    /// names or members): the latest result for the same query and Python
    /// sources, else a background request (`None` while it runs).
    pub(super) fn template_context_candidates(
        &mut self,
        path: &Path,
        start: usize,
        explicit: bool,
        query: TemplateQuery,
    ) -> Option<Vec<CompletionItem>> {
        let i = self.active?;
        let tab = &self.tabs[i];
        let document = tab.state.doc.identity();
        let text: Arc<str> = tab.state.doc.text().into();
        let caret = usize::from(tab.state.primary().head()).min(text.len());
        let key = {
            let revision = self
                .project
                .as_ref()
                .map_or(0, |project| project.analysis.revision_excluding(path));
            let buffers: Vec<_> = self
                .tabs
                .iter()
                .filter(|t| t.state.language == Language::Python)
                .map(|t| (t.state.path.clone(), t.state.doc.version()))
                .collect();
            let what = match &query {
                TemplateQuery::Names => String::new(),
                TemplateQuery::Members(root, steps) => format!("{root}{steps:?}"),
            };
            format!("{}|{what}|{revision}|{buffers:?}", path.display())
        };
        if let Some((done, items)) = &self.completion_worker.template_done {
            if *done == key {
                return Some(items.clone());
            }
        }
        if let Some((pending, job)) = self.completion_worker.template.as_mut() {
            if *pending == key {
                match job.handle.try_result() {
                    Some(Ok(items)) => {
                        self.completion_worker.template = None;
                        self.completion_worker.template_done = Some((key, items.clone()));
                        return Some(items);
                    }
                    Some(Err(_)) => self.completion_worker.template = None,
                    None => return None,
                }
            }
        }
        self.completion_worker.template = None;
        let Some((db, files)) = self.template_context_analysis() else {
            return Some(Vec::new());
        };
        let target = path.to_path_buf();
        let worker = &mut self.completion_worker;
        let mut handle = worker.spawn("Completing template", move |token| match query {
            TemplateQuery::Names => rustpy_semantic::template_context::names(&db, &files, &target)
                .into_iter()
                .map(|name| CompletionItem {
                    label: name.as_str().into(),
                    kind: CompletionKind::Variable,
                    detail: "view context".into(),
                    insert: name,
                    caret_offset: None,
                    priority: 0,
                })
                .collect(),
            TemplateQuery::Members(root, steps) => {
                rustpy_semantic::template_context::members_bounded(
                    &db,
                    &files,
                    &target,
                    &root,
                    &steps,
                    INFERENCE_BUDGET,
                    Some(token.clone()),
                )
            }
        });
        if let Some(Ok(items)) = handle.wait_timeout(worker.grace) {
            worker.template_done = Some((key, items.clone()));
            return Some(items);
        }
        let origin = Origin {
            document,
            text,
            start,
            caret,
            world: None,
            mode: Mode::Basic,
        };
        worker.template = Some((
            key,
            Job {
                origin,
                explicit,
                second_press: false,
                handle,
            },
        ));
        None
    }

    /// Opens the lookup with only its loading row while a request runs: it
    /// follows the caret like a lookup and commits nothing.
    pub(super) fn show_loading_lookup(&mut self, prefix_start: TextSize, explicit: bool) {
        let Some(tab) = self.active.and_then(|i| self.tabs.get(i)) else {
            return;
        };
        self.completion = CompletionPopup {
            visible: true,
            items: Vec::new(),
            imports: Vec::new(),
            selected: usize::MAX,
            prefix_start,
            doc_version: tab.state.doc.version(),
            caret: tab.state.primary().head(),
            explicit,
            rect: self.completion.rect,
        };
    }

    /// Whether the lookup shows its loading row.
    pub(super) fn completion_loading(&self) -> bool {
        self.completion.visible
            && self.completion.items.is_empty()
            && self.completion_worker.pending()
    }

    /// Every frame: applies finished background lookups to the editor they
    /// still serve and cancels the ones it moved away from (or whose lookup
    /// was closed). Returns whether a request is still running.
    pub(super) fn poll_completion_worker(&mut self) -> bool {
        if !self.completion_worker.pending() {
            return false;
        }
        let current = self
            .active
            .and_then(|i| self.tabs.get(i))
            .filter(|_| self.completion.visible)
            .map(|tab| {
                (
                    tab.state.doc.identity(),
                    tab.state.doc.text(),
                    usize::from(tab.state.primary().head()),
                )
            });
        let analysis = self.project.as_ref().map(|p| p.analysis.clone());
        let mut reopen = None;
        if let Some(job) = self.completion_worker.python.as_mut() {
            let world = analysis.as_ref().and_then(world);
            let serves = current.as_ref().is_some_and(|(document, text, caret)| {
                job.origin.serves(*document, text, *caret, world)
            });
            if !serves {
                self.completion_worker.python = None;
            } else if let Some(result) = job.handle.try_result() {
                let job = self.completion_worker.python.take().unwrap();
                if let Ok(items) = result {
                    reopen = Some((
                        job.explicit,
                        Some(job.second_press),
                        job.origin.mode == Mode::Smart,
                    ));
                    self.completion_worker.python_done = Some((job.origin.clone(), items));
                }
            }
        }
        if let Some(job) = self.completion_worker.large.as_mut() {
            let world = analysis.as_ref().and_then(world);
            let serves = current.as_ref().is_some_and(|(document, text, caret)| {
                job.origin.serves(*document, text, *caret, world)
            });
            if !serves {
                self.completion_worker.large = None;
            } else if let Some(result) = job.handle.try_result() {
                let job = self.completion_worker.large.take().unwrap();
                if let Ok(found) = result {
                    reopen = reopen.or(Some((false, None, false)));
                    self.completion_worker.large_done = Some((job.origin.clone(), found));
                }
            }
        }
        if let Some((key, job)) = self.completion_worker.template.as_mut() {
            let serves = current.as_ref().is_some_and(|(document, text, caret)| {
                job.origin.serves(*document, text, *caret, None)
            });
            if !serves {
                self.completion_worker.template = None;
            } else if let Some(result) = job.handle.try_result() {
                let key = key.clone();
                let explicit = job.explicit;
                self.completion_worker.template = None;
                if let Ok(items) = result {
                    self.completion_worker.template_done = Some((key, items));
                    reopen = reopen.or(Some((explicit, None, false)));
                }
            }
        }
        match reopen {
            Some((_, _, true)) => self.open_smart_completion(),
            Some((explicit, second_press, false)) => {
                self.completion_worker.resume = second_press;
                self.open_completion(explicit);
                self.completion_worker.resume = None;
            }
            None => {}
        }
        // A loading row whose request ended without a result closes.
        if !self.completion_worker.pending() && self.completion.items.is_empty() {
            self.completion.visible = false;
        }
        self.completion_worker.pending()
    }
}

#[cfg(test)]
#[path = "completion_worker_tests.rs"]
mod tests;
