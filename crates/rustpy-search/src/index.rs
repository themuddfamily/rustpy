//! Background project file index: the file list of a [`ProjectScope`] is built
//! off the UI thread, so Go to File, Search Everywhere and Find in Files never
//! walk the disk while the user types (roadmap §5 performance targets).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rustpy_project::{files::project_files_in, ProjectScope};

/// The file list of a [`ProjectScope`], built on a background thread and
/// shared with popups and background jobs (cloning shares the same index).
#[derive(Clone, Default)]
pub struct FileIndex {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    state: Mutex<State>,
    /// Last request number handed out (rebuilds and clears).
    requests: AtomicU64,
    /// Bumped whenever the stored list changes.
    generation: AtomicU64,
    /// Builds still running.
    building: AtomicUsize,
}

#[derive(Default)]
struct State {
    /// Request the stored list answers (0 = none yet).
    request: u64,
    scope: Option<ProjectScope>,
    files: Option<Arc<Vec<PathBuf>>>,
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Stores the list built for `request` unless a newer request was stored already.
    fn store(&self, request: u64, scope: ProjectScope, files: Vec<PathBuf>) -> bool {
        let mut st = self.state();
        if request <= st.request {
            return false;
        }
        st.request = request;
        st.scope = Some(scope);
        st.files = Some(Arc::new(files));
        self.generation.fetch_add(1, Ordering::SeqCst);
        true
    }

    fn next_request(&self) -> u64 {
        self.requests.fetch_add(1, Ordering::SeqCst) + 1
    }
}

impl FileIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuilds the list for `scope` on a background thread. The result of an
    /// older request never replaces the list of a newer one, so the last
    /// rebuild wins even when builds finish out of order.
    pub fn rebuild(&self, scope: ProjectScope) {
        let request = self.inner.next_request();
        let inner = self.inner.clone();
        inner.building.fetch_add(1, Ordering::SeqCst);
        let worker = inner.clone();
        let spawned = std::thread::Builder::new()
            .name("rustpy-file-index".into())
            .spawn(move || {
                let files = project_files_in(&scope);
                worker.store(request, scope, files);
                worker.building.fetch_sub(1, Ordering::SeqCst);
            });
        if spawned.is_err() {
            inner.building.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Builds the list on the calling thread (tests, command-line tools).
    pub fn rebuild_now(&self, scope: ProjectScope) {
        let request = self.inner.next_request();
        let files = project_files_in(&scope);
        self.inner.store(request, scope, files);
    }

    /// Forgets the list (the project closed); builds still running are dropped.
    pub fn clear(&self) {
        let request = self.inner.next_request();
        let mut st = self.inner.state();
        *st = State {
            request,
            scope: None,
            files: None,
        };
        self.inner.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// The files of the last finished build (None before the first one).
    pub fn files(&self) -> Option<Arc<Vec<PathBuf>>> {
        self.inner.state().files.clone()
    }

    /// The scope the stored list was built for.
    pub fn scope(&self) -> Option<ProjectScope> {
        self.inner.state().scope.clone()
    }

    /// The stored files that belong to `scope`: the shared list when it was built
    /// for exactly this scope, else (a folder was excluded since) a filtered copy.
    /// None before the first build and while the list is for another project.
    pub fn files_for(&self, scope: &ProjectScope) -> Option<Arc<Vec<PathBuf>>> {
        let st = self.inner.state();
        let files = st.files.as_ref()?;
        let built = st.scope.as_ref()?;
        if built == scope {
            return Some(files.clone());
        }
        if built.root != scope.root {
            return None;
        }
        let roots = scope.walk_roots();
        Some(Arc::new(
            files
                .iter()
                .filter(|p| roots.iter().any(|r| p.starts_with(r)) && !scope.is_excluded(p))
                .cloned()
                .collect(),
        ))
    }

    /// Changes whenever the stored list changes (popups re-run their query then).
    pub fn generation(&self) -> u64 {
        self.inner.generation.load(Ordering::SeqCst)
    }

    pub fn is_building(&self) -> bool {
        self.inner.building.load(Ordering::SeqCst) > 0
    }

    /// Waits until no build is running; false on timeout.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let start = Instant::now();
        while self.is_building() {
            if start.elapsed() > timeout {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join("vendor/lib")).unwrap();
        std::fs::write(dir.path().join("src/app.py"), "x = 1\n").unwrap();
        std::fs::write(dir.path().join("vendor/lib/big.py"), "y = 2\n").unwrap();
        dir
    }

    #[test]
    fn background_rebuild_honors_exclusions() {
        let dir = project();
        let root = dir.path().to_path_buf();
        let index = FileIndex::new();
        assert!(index.files().is_none());
        index.rebuild(ProjectScope::new(&root));
        assert!(index.wait_idle(Duration::from_secs(10)));
        assert_eq!(index.files().unwrap().len(), 2);
        let g = index.generation();
        let scope = ProjectScope::new(&root).with_excluded([root.join("vendor")]);
        // Until the rebuild finishes the stored list is filtered with the new scope.
        let filtered = index.files_for(&scope).unwrap();
        assert_eq!(filtered.as_slice(), &[root.join("src/app.py")]);
        index.rebuild(scope.clone());
        assert!(index.wait_idle(Duration::from_secs(10)));
        assert!(index.generation() > g);
        assert_eq!(
            index.files().unwrap().as_slice(),
            &[root.join("src/app.py")]
        );
        assert!(
            Arc::ptr_eq(&index.files_for(&scope).unwrap(), &index.files().unwrap()),
            "same scope shares the list"
        );
        assert!(index.files_for(&ProjectScope::new("/elsewhere")).is_none());
    }

    #[test]
    fn older_results_never_replace_newer_ones() {
        let dir = project();
        let root = dir.path().to_path_buf();
        let index = FileIndex::new();
        let first = index.inner.next_request();
        let second = index.inner.next_request();
        assert!(index
            .inner
            .store(second, ProjectScope::new(&root), vec![root.join("new.py")]));
        assert!(!index
            .inner
            .store(first, ProjectScope::new(&root), vec![root.join("old.py")]));
        assert_eq!(index.files().unwrap().as_slice(), &[root.join("new.py")]);
    }

    #[test]
    fn clear_drops_builds_in_flight() {
        let dir = project();
        let index = FileIndex::new();
        let pending = index.inner.next_request();
        index.clear();
        assert!(!index
            .inner
            .store(pending, ProjectScope::new(dir.path()), vec![]));
        assert!(index.files().is_none());
        index.rebuild_now(ProjectScope::new(dir.path()));
        assert_eq!(index.files().unwrap().len(), 2);
    }
}
