//! Background row-101 producers. App owns one scheduler and publishes only
//! `poll(current_request)` results; pending/unavailable lenses have no counts.
use std::{ops::Range, path::PathBuf, sync::Arc};

use rustpy_core::{TaskError, TaskHandle, TaskManager};
use rustpy_search::code_vision::{usage_lenses, Limits, UsageLens};
use rustpy_semantic::{db::AnalysisSnapshot, features::InlayHint, Analysis};
use rustpy_syntax::ast::{AstNode, ClassDef, FuncDef};
use rustpy_text::DocumentId;
use rustpy_vcs::{code_vision::AuthorLens, GitRepo};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Options {
    pub returns: bool,
    pub usages: bool,
    pub authors: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ProjectSnapshot {
    pub root: PathBuf,
    pub index_generation: u64,
    /// Must change on same-path content changes, not just index membership.
    pub content_generation: u64,
    pub complete: bool,
    pub files: Arc<[PathBuf]>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RepoSnapshot {
    pub root: PathBuf,
    pub head: Option<String>,
    /// App's repository/index/worktree watcher generation.
    pub generation: u64,
}
/// Capture on the UI thread without parsing, disk reads, or repository work.
/// `semantic_revision` is Analysis::revision_excluding(path). `source` is the
/// exact document text, not just a hash; incarnation/version are independent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Stamp {
    pub document: DocumentId,
    pub version: u64,
    pub path: PathBuf,
    pub source: Arc<str>,
    pub semantic_revision: u64,
    pub project: ProjectSnapshot,
    pub repo: Option<RepoSnapshot>,
    pub options: Options,
    pub trusted: bool,
    pub large_file: bool,
}
#[derive(Clone)]
pub(super) struct Request {
    pub analysis: Arc<Analysis>,
    pub stamp: Stamp,
}
impl Request {
    fn matches(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.analysis, &other.analysis) && self.stamp == other.stamp
    }
    fn current(&self) -> bool {
        self.analysis.revision_excluding(&self.stamp.path) == self.stamp.semantic_revision
    }
}
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Availability<T> {
    Disabled,
    Pending,
    Unavailable,
    Ready(T),
}
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Lenses {
    pub returns: Availability<Vec<InlayHint>>,
    pub usages: Availability<Vec<UsageLens>>,
    pub authors: Availability<Vec<AuthorLens>>,
}
impl Lenses {
    fn pending(options: Options) -> Self {
        fn state<T>(enabled: bool) -> Availability<T> {
            if enabled {
                Availability::Pending
            } else {
                Availability::Disabled
            }
        }
        Self {
            returns: state(options.returns),
            usages: state(options.usages),
            authors: state(options.authors),
        }
    }
    fn unavailable(options: Options) -> Self {
        let mut result = Self::pending(options);
        if options.returns {
            result.returns = Availability::Unavailable;
        }
        if options.usages {
            result.usages = Availability::Unavailable;
        }
        if options.authors {
            result.authors = Availability::Unavailable;
        }
        result
    }
}
struct Job {
    request: Request,
    handle: TaskHandle<Lenses>,
}
/// At most one running worker, including a cancelled libgit2 blame. Newest
/// request replaces the queue while cancellation drains; no abandoned workers
/// accumulate during typing. No persistent cache across project generations.
#[derive(Default)]
pub(super) struct Scheduler {
    job: Option<Job>,
    queued: Option<Request>,
    published: Option<(Request, Lenses)>,
}
impl Drop for Scheduler {
    fn drop(&mut self) {
        self.cancel();
    }
}
impl Scheduler {
    /// Clears published authority and requests cooperative cancellation.
    pub fn cancel(&mut self) {
        if let Some(job) = &self.job {
            job.handle.cancel();
        }
        self.queued = None;
        self.published = None;
    }
    pub fn request(&mut self, request: Request) -> Lenses {
        if let Some((old, lenses)) = &self.published {
            if old.matches(&request) && request.current() {
                return lenses.clone();
            }
        }
        self.published = None;
        if self
            .job
            .as_ref()
            .is_some_and(|job| job.request.matches(&request) && !job.handle.is_cancelled())
        {
            return Lenses::pending(request.stamp.options);
        }
        if let Some(job) = &self.job {
            job.handle.cancel();
        }
        let pending = Lenses::pending(request.stamp.options);
        self.queued = Some(request);
        self.start_queued();
        pending
    }
    fn start_queued(&mut self) {
        if self.job.is_some() {
            return;
        }
        let Some(request) = self.queued.take() else {
            return;
        };
        let worker = request.clone();
        let handle = TaskManager::global()
            .task("Inlay hints and Code Vision")
            .hidden()
            .spawn(move |progress| compute(&worker, progress));
        self.job = Some(Job { request, handle });
    }
    /// Invoke each frame with a freshly captured current stamp, including index
    /// content/repository generations even if the editor itself has not changed.
    pub fn poll(&mut self, current: &Request) -> Option<Lenses> {
        if self
            .published
            .as_ref()
            .is_some_and(|(old, _)| !old.matches(current) || !current.current())
        {
            self.published = None;
        }
        let job = self.job.as_mut()?;
        if !job.request.matches(current) || !current.current() {
            job.handle.cancel();
        }
        let result = job.handle.try_result()?;
        let job = self.job.take().unwrap();
        let accepted =
            !job.handle.is_cancelled() && job.request.matches(current) && current.current();
        let output = if accepted {
            let lenses = result.unwrap_or_else(|_| Lenses::unavailable(current.stamp.options));
            self.published = Some((current.clone(), lenses.clone()));
            Some(lenses)
        } else {
            None
        };
        // Never start an old queued stamp after a document/project/focus change.
        if self
            .queued
            .as_ref()
            .is_some_and(|queued| !queued.matches(current) || !current.current())
        {
            self.queued = None;
        }
        self.start_queued();
        output
    }
}

fn declaration_lines(snapshot: &AnalysisSnapshot, path: &std::path::Path) -> Vec<Range<usize>> {
    let Some(file) = snapshot.analysis().file_for_path(path) else {
        return Vec::new();
    };
    let text = &file.text;
    file.parse
        .syntax_node()
        .descendants()
        .filter_map(|node| {
            if FuncDef::cast(node.clone()).is_none() && ClassDef::cast(node.clone()).is_none() {
                return None;
            }
            let range = node.text_range();
            let start = usize::from(range.start());
            let end = usize::from(range.end());
            let first = text[..start].bytes().filter(|byte| *byte == b'\n').count();
            // Parser ranges retain trailing trivia. Author coverage ends at
            // the declaration's last non-whitespace byte, not following blanks.
            let end = start + text[start..end].trim_end().len();
            let end_line = text.as_bytes()[..end]
                .iter()
                .filter(|byte| **byte == b'\n')
                .count()
                + 1;
            Some(first..end_line)
        })
        .take(129)
        .collect()
}
fn compute(
    request: &Request,
    progress: &rustpy_core::ProgressReporter,
) -> Result<Lenses, TaskError> {
    progress.check()?;
    let stamp = &request.stamp;
    let options = stamp.options;
    let mut output = Lenses::unavailable(options);
    if !options.returns && !options.usages && !options.authors {
        return Ok(output);
    }
    if !matches!(
        stamp.path.extension().and_then(|ext| ext.to_str()),
        Some("py" | "pyi")
    ) || !stamp.trusted
        || stamp.large_file
        || stamp.source.len() > 1024 * 1024
        || !request.current()
    {
        return Ok(output);
    }
    // Return hints do not require a ready index, but any uncaptured semantic
    // dependency is unavailable. Usage counts require the complete catalogue.
    let mut paths = if stamp.project.complete {
        stamp.project.files.to_vec()
    } else {
        Vec::new()
    };
    paths.push(stamp.path.clone());
    let snapshot = match request.analysis.snapshot(&paths, progress.token()) {
        Ok(snapshot) => snapshot,
        Err(_) => {
            progress.check()?;
            return Ok(output);
        }
    };
    let Some(file) = snapshot.analysis().file_for_path(&stamp.path) else {
        return Ok(output);
    };
    if file.text != stamp.source || !request.current() {
        return Ok(output);
    }
    if options.returns {
        let hints = rustpy_semantic::return_hints::return_hints(
            snapshot.analysis(),
            &file,
            progress.token(),
        )?;
        if snapshot.complete() {
            output.returns = Availability::Ready(hints);
        }
    }
    if options.usages && stamp.project.complete {
        if let Ok(lenses) = usage_lenses(
            snapshot.analysis(),
            &file,
            &stamp.project.files,
            Limits::default(),
            progress,
        ) {
            if snapshot.complete() {
                output.usages = Availability::Ready(lenses);
            }
        }
    }
    if options.authors {
        if let Some(expected) = &stamp.repo {
            if let Some(repo) = GitRepo::discover(&expected.root) {
                let root_matches = repo.root() == expected.root;
                if root_matches && repo.code_vision_head().ok() == Some(expected.head.clone()) {
                    let ranges = declaration_lines(&snapshot, &stamp.path);
                    if let Ok(authors) =
                        repo.code_authors(&stamp.path, &stamp.source, &ranges, progress.token())
                    {
                        if repo.code_vision_head().ok() == Some(expected.head.clone()) {
                            output.authors = Availability::Ready(authors);
                        }
                    }
                }
            }
        }
    }
    if !snapshot.complete() {
        if options.returns {
            output.returns = Availability::Unavailable;
        }
        if options.usages {
            output.usages = Availability::Unavailable;
        }
    }
    progress.check()?;
    if !request.current() {
        return Ok(Lenses::unavailable(options));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_core::{CancellationToken, ProgressReporter};
    use std::time::{Duration, Instant};

    fn fixture() -> (tempfile::TempDir, Request) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("main.py");
        let source: Arc<str> = "def π():\n    return 42\n\nπ()\n".into();
        let analysis = Arc::new(Analysis::for_project(directory.path()));
        analysis.set_file_text(&path, &source);
        let document = rustpy_text::Document::new(&source);
        let stamp = Stamp {
            document: document.identity(),
            version: document.version(),
            path: path.clone(),
            source,
            semantic_revision: analysis.revision_excluding(&path),
            project: ProjectSnapshot {
                root: directory.path().into(),
                index_generation: 1,
                content_generation: 1,
                complete: true,
                files: vec![path].into(),
            },
            repo: None,
            options: Options {
                returns: true,
                usages: true,
                authors: false,
            },
            trusted: true,
            large_file: false,
        };
        (directory, Request { analysis, stamp })
    }
    fn wait(scheduler: &mut Scheduler, current: &Request) -> Lenses {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(result) = scheduler.poll(current) {
                return result;
            }
            assert!(
                Instant::now() < deadline,
                "Code Vision worker did not finish"
            );
            std::thread::yield_now();
        }
    }
    #[test]
    fn background_real_producers_publish_unicode_return_and_semantic_usage() {
        let (_directory, request) = fixture();
        let mut scheduler = Scheduler::default();
        assert_eq!(
            scheduler.request(request.clone()),
            Lenses::pending(request.stamp.options)
        );
        let result = wait(&mut scheduler, &request);
        let Availability::Ready(returns) = result.returns else {
            panic!("return hints unavailable");
        };
        assert_eq!(returns.len(), 1);
        assert_eq!(returns[0].label, " -> int");
        let Availability::Ready(usages) = result.usages else {
            panic!("usage hints unavailable");
        };
        assert_eq!(usages.len(), 1);
        assert_eq!(usages[0].name, "π");
        assert_eq!(usages[0].usages, 1);
    }
    #[test]
    fn incomplete_index_safe_mode_and_cancellation_never_publish_false_zero() {
        let (_directory, mut request) = fixture();
        request.stamp.project.complete = false;
        let result = compute(&request, &ProgressReporter::new()).unwrap();
        assert_eq!(result.usages, Availability::Unavailable);
        request.stamp.trusted = false;
        assert_eq!(
            compute(&request, &ProgressReporter::new()).unwrap(),
            Lenses::unavailable(request.stamp.options)
        );
        let token = CancellationToken::new();
        token.cancel();
        assert_eq!(
            compute(&request, &ProgressReporter::with_token(token)),
            Err(TaskError::Cancelled)
        );
    }
    #[test]
    fn exact_authority_includes_incarnation_source_index_content_repo_and_world() {
        let (_directory, request) = fixture();
        let mut changed = request.clone();
        changed.stamp.document = rustpy_text::Document::new(&request.stamp.source).identity();
        assert!(!request.matches(&changed));
        changed = request.clone();
        changed.stamp.source = "different same-version source".into();
        assert!(!request.matches(&changed));
        changed = request.clone();
        changed.stamp.project.content_generation += 1;
        assert!(!request.matches(&changed));
        changed = request.clone();
        changed.stamp.project.index_generation += 1;
        assert!(!request.matches(&changed));
        changed = request.clone();
        changed.stamp.repo = Some(RepoSnapshot {
            root: request.stamp.project.root.clone(),
            head: Some("actual HEAD identity".into()),
            generation: 1,
        });
        assert!(!request.matches(&changed));
        request
            .analysis
            .set_file_text(&request.stamp.project.root.join("other.py"), "value = 2\n");
        assert!(!request.current());
    }
    #[test]
    fn cancelled_worker_drains_before_latest_job_and_stale_result_cannot_publish() {
        let (_directory, request) = fixture();
        let (release, barrier) = std::sync::mpsc::channel();
        let handle = TaskManager::global()
            .task("Controlled Code Vision result")
            .hidden()
            .spawn(move |_| {
                barrier.recv_timeout(Duration::from_secs(3)).unwrap();
                Ok(Lenses {
                    returns: Availability::Ready(vec![]),
                    usages: Availability::Ready(vec![]),
                    authors: Availability::Disabled,
                })
            });
        let mut scheduler = Scheduler {
            job: Some(Job {
                request: request.clone(),
                handle,
            }),
            queued: None,
            published: None,
        };
        let original_id = scheduler.job.as_ref().unwrap().handle.id();
        let mut newest = request.clone();
        newest.stamp.project.content_generation += 1;
        scheduler.request(newest.clone());
        assert!(scheduler.job.as_ref().unwrap().handle.is_cancelled());
        assert_eq!(scheduler.job.as_ref().unwrap().handle.id(), original_id);
        assert!(scheduler.poll(&newest).is_none());
        release.send(()).unwrap();
        let result = wait(&mut scheduler, &newest);
        let Availability::Ready(usages) = result.usages else {
            panic!("new worker unavailable");
        };
        assert_eq!(usages[0].usages, 1);
        assert!(scheduler.published.as_ref().unwrap().0.matches(&newest));
    }
    #[test]
    fn declaration_author_ranges_handle_unicode_eof_and_multiple_trailing_blanks() {
        for text in [
            "def π():\n    return '🙂'",
            "def π():\n    return '🙂'\n\n\t \n",
            "def π():\n    return '🙂'\r\n\r\n",
        ] {
            let (_directory, request) = fixture();
            request.analysis.set_file_text(&request.stamp.path, text);
            let snapshot = request
                .analysis
                .snapshot(&request.stamp.project.files, &CancellationToken::new())
                .unwrap();
            let ranges = declaration_lines(&snapshot, &request.stamp.path);
            assert_eq!(ranges.len(), 1);
            assert_eq!(ranges[0], 0..2, "{text:?}");
        }
    }
    #[test]
    fn declaration_author_ranges_are_unicode_safe_and_exclude_following_empty_line() {
        let (_directory, request) = fixture();
        let snapshot = request
            .analysis
            .snapshot(&request.stamp.project.files, &CancellationToken::new())
            .unwrap();
        let ranges = declaration_lines(&snapshot, &request.stamp.path);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0], 0..2);
    }
}
