//! In-flight operations shared by every Git job owner and local mutation.
//! Reservations can move to workers; nested operations are allowed only
//! inside the owner's explicitly entered thread-local scope.

use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock, Weak};

struct State {
    key: PathBuf,
    label: String,
}

fn operations() -> &'static Mutex<HashMap<PathBuf, Weak<State>>> {
    static OPERATIONS: OnceLock<Mutex<HashMap<PathBuf, Weak<State>>>> = OnceLock::new();
    OPERATIONS.get_or_init(Mutex::default)
}

thread_local! {
    static ENTERED: RefCell<HashMap<PathBuf, Weak<State>>> = RefCell::new(HashMap::new());
}

fn key(path: &Path) -> PathBuf {
    // New files and packages can have several missing parent directories.
    // Discover from the nearest existing ancestor so those writes use the
    // same repository key as jobs rooted at the project directory.
    let mut dir = path;
    while !dir.is_dir() {
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }
    let path = git2::Repository::discover(dir)
        .map(|r| r.commondir().to_path_buf())
        .unwrap_or_else(|_| dir.to_path_buf());
    path.canonicalize().unwrap_or(path)
}

/// The operation running in this repository, including linked worktrees.
/// Read-only repository views need not acquire a reservation.
pub fn running_operation(path: &Path) -> Option<String> {
    running_operation_key(&key(path))
}

pub(crate) fn running_operation_key(key: &Path) -> Option<String> {
    operations()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(key)
        .and_then(Weak::upgrade)
        .map(|s| s.label.clone())
}

/// A reservation acquired before saving or dispatching an operation. Keep it
/// until the process has exited and cancellation cleanup has finished.
pub struct RepositoryReservation {
    state: Arc<State>,
}

impl RepositoryReservation {
    /// Refuses rather than waiting for another operation, even on the same
    /// thread: reserving two workers must never count as nested work.
    pub fn try_acquire(path: &Path, label: impl Into<String>) -> anyhow::Result<Self> {
        let key = key(path);
        let mut operations = operations().lock().unwrap_or_else(|e| e.into_inner());
        operations.retain(|_, state| state.strong_count() > 0);
        if let Some(running) = operations.get(&key).and_then(Weak::upgrade) {
            anyhow::bail!(
                "{} is running: wait for it to finish before starting another Git operation",
                running.label
            );
        }
        let state = Arc::new(State {
            key: key.clone(),
            label: label.into(),
        });
        operations.insert(key, Arc::downgrade(&state));
        Ok(Self { state })
    }

    /// Runs the reserved operation, allowing its nested Git mutations on
    /// this thread. The scope is restored on return or unwinding.
    pub fn run<R>(&self, op: impl FnOnce() -> R) -> R {
        let _entered = RepositoryOperation::activate(self.state.clone());
        op()
    }
}

/// A synchronous mutation, reentrant within its owner's thread only.
/// It cannot be moved to another thread; use a reservation for workers.
pub struct RepositoryOperation {
    state: Arc<State>,
    previous: Option<Weak<State>>,
    _thread: PhantomData<Rc<()>>,
}

impl RepositoryOperation {
    pub fn enter(path: &Path, label: impl Into<String>) -> anyhow::Result<Self> {
        let key = key(path);
        let nested = ENTERED.with(|entered| entered.borrow().get(&key).and_then(Weak::upgrade));
        let state = match nested {
            Some(state) => state,
            None => RepositoryReservation::try_acquire(path, label)?.state,
        };
        Ok(Self::activate(state))
    }

    fn activate(state: Arc<State>) -> Self {
        let previous = ENTERED.with(|entered| {
            entered
                .borrow_mut()
                .insert(state.key.clone(), Arc::downgrade(&state))
        });
        Self {
            state,
            previous,
            _thread: PhantomData,
        }
    }
}

impl Drop for RepositoryOperation {
    fn drop(&mut self) {
        ENTERED.with(|entered| {
            let mut entered = entered.borrow_mut();
            match self.previous.take() {
                Some(previous) => {
                    entered.insert(self.state.key.clone(), previous);
                }
                None => {
                    entered.remove(&self.state.key);
                }
            }
        });
    }
}
