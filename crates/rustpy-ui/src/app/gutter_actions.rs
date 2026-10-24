//! Background semantic gutter destinations. App must capture complete current authority.
use rustpy_core::{ProgressReporter, TaskError, TaskHandle, TaskManager, TextSize};
use rustpy_semantic::{features::Location, hierarchy, Analysis};
use rustpy_syntax::ast::{AstNode, ClassDef, FuncDef};
use rustpy_text::DocumentId;
use std::{path::PathBuf, sync::Arc};
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Stamp {
    pub document: DocumentId,
    pub version: u64,
    pub path: PathBuf,
    pub source: Arc<str>,
    pub semantic_revision: u64,
    pub project: PathBuf,
    pub index_generation: u64,
    pub content_generation: u64,
    pub files: Arc<[PathBuf]>,
    pub complete: bool,
    pub trusted: bool,
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Marker {
    pub offset: TextSize,
    pub supers: Vec<Location>,
    pub implementations: Vec<Location>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Markers {
    Pending,
    Unavailable,
    Ready(Vec<Marker>),
}
struct Job {
    request: Request,
    handle: TaskHandle<Markers>,
}
/// At most one running worker, including a cancelled semantic hierarchy computation. Newest
/// request replaces the queue while cancellation drains; no abandoned workers
/// accumulate during typing. No persistent cache across project generations.
#[derive(Default)]
pub(super) struct Scheduler {
    job: Option<Job>,
    queued: Option<Request>,
    published: Option<(Request, Markers)>,
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
    pub fn request(&mut self, request: Request) -> Markers {
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
            return Markers::Pending;
        }
        if let Some(job) = &self.job {
            job.handle.cancel();
        }
        let pending = Markers::Pending;
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
            .task("Gutter navigation")
            .hidden()
            .spawn(move |progress| compute(&worker, progress));
        self.job = Some(Job { request, handle });
    }
    /// Invoke each frame with a freshly captured current stamp, including index
    /// content/repository generations even if the editor itself has not changed.
    pub fn poll(&mut self, current: &Request) -> Option<Markers> {
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
            let lenses = result.unwrap_or(Markers::Unavailable);
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

fn compute(request: &Request, progress: &ProgressReporter) -> Result<Markers, TaskError> {
    progress.check()?;
    let s = &request.stamp;
    if !s.trusted
        || !s.complete
        || s.source.len() > 1024 * 1024
        || s.files.len() > 256
        || !request.current()
    {
        return Ok(Markers::Unavailable);
    }
    let mut paths = s.files.to_vec();
    if !paths.contains(&s.path) {
        paths.push(s.path.clone());
    }
    let snapshot = match request.analysis.snapshot(&paths, progress.token()) {
        Ok(v) => v,
        Err(_) => {
            progress.check()?;
            return Ok(Markers::Unavailable);
        }
    };
    let db = snapshot.analysis();
    let Some(file) = db.file_for_path(&s.path) else {
        return Ok(Markers::Unavailable);
    };
    if file.text != s.source || !request.current() {
        return Ok(Markers::Unavailable);
    }
    let files: Vec<_> = paths.iter().filter_map(|p| db.file_for_path(p)).collect();
    if files.len() != paths.len() {
        return Ok(Markers::Unavailable);
    }
    let mut markers = Vec::new();
    let mut declarations = 0;
    for node in file.parse.syntax_node().descendants() {
        progress.check()?;
        let function = FuncDef::cast(node.clone());
        let class = ClassDef::cast(node);
        let name = function
            .as_ref()
            .and_then(|v| v.name())
            .or_else(|| class.as_ref().and_then(|v| v.name()));
        let Some(name) = name else { continue };
        declarations += 1;
        if declarations > 128 {
            return Ok(Markers::Unavailable);
        }
        let offset = name.text_range().start();
        let supers = if function.is_some() {
            hierarchy::super_methods(db, &file, offset)
        } else {
            Vec::new()
        };
        let implementations = if function.is_some() {
            hierarchy::implementations(db, &file, offset, &paths)
        } else {
            // Existing class implementations traversal has no visited set. Use its actual
            // resolver-backed direct-subclass primitive with bounded, cycle-safe traversal.
            let Some(origin) = hierarchy::class_at(db, &file, offset) else {
                continue;
            };
            let mut seen = vec![origin.clone()];
            let mut queue = vec![origin];
            let mut result = Vec::new();
            while let Some(base) = queue.pop() {
                progress.check()?;
                for child in hierarchy::direct_subclasses(db, &base, &files) {
                    if seen.contains(&child) {
                        continue;
                    }
                    if seen.len() >= 256 {
                        return Ok(Markers::Unavailable);
                    }
                    seen.push(child.clone());
                    if let Some(owner) = db.file(child.file) {
                        if let Some(def) = owner
                            .parse
                            .syntax_node()
                            .descendants()
                            .filter_map(ClassDef::cast)
                            .find(|d| d.syntax().text_range() == child.range)
                        {
                            if let Some(name) = def.name() {
                                result.push(Location {
                                    path: owner.path.clone(),
                                    range: name.text_range(),
                                    is_stub: owner.is_stub,
                                });
                            }
                        }
                    }
                    queue.push(child);
                }
            }
            result
        };
        if !supers.is_empty() || !implementations.is_empty() {
            markers.push(Marker {
                offset,
                supers,
                implementations,
            });
        }
    }
    progress.check()?;
    if !snapshot.complete() || !request.current() {
        return Ok(Markers::Unavailable);
    }
    Ok(Markers::Ready(markers))
}
/// Availability only, never a duplicate fix application route. Root passes only
/// current-version real quick fixes after its usual Reader/inspection policy.
pub(super) fn quick_fix_offsets(
    doc: &rustpy_text::Document,
    fixes: &[(
        rustpy_core::TextRange,
        Vec<rustpy_analysis::inspections::QuickFix>,
    )],
) -> Vec<TextSize> {
    let mut lines = std::collections::BTreeMap::new();
    for (range, items) in fixes {
        if !items.is_empty() && range.end() <= doc.len() {
            lines
                .entry(doc.line_of(range.start()))
                .or_insert(range.start());
        }
    }
    lines.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(text: &str) -> Request {
        let path = PathBuf::from("/proj/main.py");
        let analysis = Arc::new(Analysis::for_project(std::path::Path::new("/proj")));
        analysis.set_file_text(&path, text);
        let doc = rustpy_text::Document::new(text);
        Request {
            stamp: Stamp {
                document: doc.identity(),
                version: doc.version(),
                path: path.clone(),
                source: Arc::from(text),
                semantic_revision: analysis.revision_excluding(&path),
                project: PathBuf::from("/proj"),
                index_generation: 1,
                content_generation: 1,
                files: vec![path].into(),
                complete: true,
                trusted: true,
            },
            analysis,
        }
    }
    #[test]
    fn actual_override_and_implementation_destinations_use_semantic_identity() {
        let source="class Base:\n    def work(self):\n        return 1\nclass Child(Base):\n    def work(self):\n        return 2\nclass Unrelated:\n    def work(self):\n        return 3\n";
        let req = request(source);
        let Markers::Ready(markers) = compute(&req, &ProgressReporter::new()).unwrap() else {
            panic!("expected complete semantic destinations")
        };
        let base = TextSize::new(source.find("work").unwrap() as u32);
        let child = TextSize::new(source.find("work(self):\n        return 2").unwrap() as u32);
        let b = markers.iter().find(|m| m.offset == base).unwrap();
        assert_eq!(b.implementations.len(), 1);
        assert_eq!(b.implementations[0].range.start(), child);
        let c = markers.iter().find(|m| m.offset == child).unwrap();
        assert_eq!(c.supers.len(), 1);
        assert_eq!(c.supers[0].range.start(), base);
        assert!(!markers.iter().any(|m| m.offset
            == TextSize::new(source.find("work(self):\n        return 3").unwrap() as u32)));
    }
    #[test]
    fn unavailable_and_stale_authority_never_publishes_destinations() {
        let mut req = request("class A:\n    pass\n");
        req.stamp.complete = false;
        assert_eq!(
            compute(&req, &ProgressReporter::new()).unwrap(),
            Markers::Unavailable
        );
        req.stamp.complete = true;
        req.stamp.source = Arc::from("changed");
        assert_eq!(
            compute(&req, &ProgressReporter::new()).unwrap(),
            Markers::Unavailable
        );
    }
    #[test]
    fn cancelled_query_has_no_partial_result() {
        let req = request("class A:\n    pass\n");
        let progress = ProgressReporter::new();
        progress.token().cancel();
        assert!(compute(&req, &progress).is_err());
    }
    #[test]
    fn bulb_requires_actual_fix_and_deduplicates_source_line() {
        let doc = rustpy_text::Document::new("π😀\nx");
        let fix = rustpy_analysis::inspections::QuickFix {
            title: "Actual fix".into(),
            edits: vec![],
        };
        let ranges = vec![
            (rustpy_core::TextRange::empty(TextSize::new(0)), vec![]),
            (
                rustpy_core::TextRange::empty(TextSize::new(2)),
                vec![fix.clone()],
            ),
            (rustpy_core::TextRange::empty(TextSize::new(6)), vec![fix]),
        ];
        assert_eq!(quick_fix_offsets(&doc, &ranges), vec![TextSize::new(2)]);
        assert_eq!(doc.text(), "π😀\nx");
        assert!(!doc.can_undo());
    }
}
