//! Background tasks with progress and cancellation (roadmap §0.1): cancellable
//! background work that reports progress to the status bar.
//!
//! [`TaskManager::spawn`] runs a closure on its own thread and hands it a
//! [`ProgressReporter`] for progress text, a fraction (or indeterminate
//! progress) and cooperative cancellation. The manager keeps the list of
//! running tasks (the status bar shows the newest one with a progress bar and
//! a cancel button, the Background Tasks popup lists them all) and a short
//! history of finished ones, and publishes [`TaskEvent`]s on its
//! [`EventBus`]. The caller keeps the typed [`TaskHandle`] and polls it for
//! the result (the UI once per frame).
//!
//! Long operations take a `&ProgressReporter` (a progress indicator with a
//! cancellation flag); synchronous callers and tests pass a detached one
//! from [`ProgressReporter::new`].

use std::any::Any;
use std::collections::VecDeque;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, TryRecvError};
use parking_lot::Mutex;

use crate::cancel::{CancellationToken, Cancelled};
use crate::events::EventBus;

/// Identifier of a task, unique within its manager.
pub type TaskId = u64;

thread_local! {
    static WORKER_CPU: std::cell::RefCell<Option<(usize, cpu_time::ThreadTime)>> = const { std::cell::RefCell::new(None) };
}
fn sample_worker_cpu(shared: &TaskShared) {
    WORKER_CPU.with(|clock| {
        if let Some((owner, start)) = clock.borrow().as_ref() {
            if *owner == std::ptr::from_ref(shared) as usize {
                if let Ok(elapsed) = start.try_elapsed() {
                    shared.cpu_nanos.store(
                        elapsed.as_nanos().min(u128::from(u64::MAX - 1)) as u64,
                        Ordering::Relaxed,
                    );
                }
            }
        }
    });
}
/// CPU consumed by this process, including parallel workers, when supported by the OS.
pub fn process_cpu_time() -> Option<Duration> {
    cpu_time::ProcessTime::try_now()
        .ok()
        .map(|time| time.as_duration())
}

/// Why a task produced no value.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TaskError {
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Failed(String),
}

impl From<Cancelled> for TaskError {
    fn from(_: Cancelled) -> Self {
        TaskError::Cancelled
    }
}

impl From<String> for TaskError {
    fn from(message: String) -> Self {
        TaskError::Failed(message)
    }
}

impl From<&str> for TaskError {
    fn from(message: &str) -> Self {
        TaskError::Failed(message.to_string())
    }
}

impl From<std::io::Error> for TaskError {
    fn from(e: std::io::Error) -> Self {
        TaskError::Failed(e.to_string())
    }
}

impl From<anyhow::Error> for TaskError {
    fn from(e: anyhow::Error) -> Self {
        TaskError::Failed(format!("{e:#}"))
    }
}

/// What a task closure returns.
pub type TaskResult<T> = Result<T, TaskError>;

/// How a task ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskOutcome {
    Finished,
    Cancelled,
    Failed(String),
}

impl TaskOutcome {
    fn of<T>(result: &TaskResult<T>) -> Self {
        match result {
            Ok(_) => TaskOutcome::Finished,
            Err(TaskError::Cancelled) => TaskOutcome::Cancelled,
            Err(TaskError::Failed(m)) => TaskOutcome::Failed(m.clone()),
        }
    }
}

/// Snapshot of a task for display.
#[derive(Clone, Debug, PartialEq)]
pub struct TaskInfo {
    pub id: TaskId,
    pub title: String,
    /// Progress text ("Searching for 'foo'…") and details ("src/app.py").
    pub text: String,
    pub details: String,
    /// Fraction done in `0.0..=1.0`; `None` for indeterminate progress.
    pub fraction: Option<f32>,
    pub cancellable: bool,
    /// Cancellation was requested (the task may still be winding down).
    pub cancel_requested: bool,
    /// Not listed in the status bar or the Background Tasks popup.
    pub hidden: bool,
    /// Time since the start, or the run time of a finished task.
    pub elapsed: Duration,
    /// CPU consumed by this task's own worker (parallel helpers have their own accounting).
    pub cpu_time: Option<Duration>,
    /// How the task ended; `None` while it runs.
    pub outcome: Option<TaskOutcome>,
}

/// Task life-cycle notifications published on [`TaskManager::events`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskEvent {
    Started {
        id: TaskId,
        title: String,
    },
    Finished {
        id: TaskId,
        title: String,
        outcome: TaskOutcome,
        elapsed: Duration,
    },
}

/// Fraction bits meaning "indeterminate" (a NaN pattern `set_fraction` never stores).
const INDETERMINATE: u32 = u32::MAX;

/// Finished tasks remembered for the Background Tasks popup.
const MAX_FINISHED: usize = 20;

struct TaskShared {
    id: TaskId,
    title: String,
    token: CancellationToken,
    cancellable: bool,
    hidden: bool,
    key: Option<String>,
    started: Instant,
    fraction: AtomicU32,
    cpu_nanos: AtomicU64,
    text: Mutex<(String, String)>,
}

impl TaskShared {
    fn new(id: TaskId, title: String, token: CancellationToken) -> Self {
        Self {
            id,
            title,
            token,
            cancellable: true,
            hidden: false,
            key: None,
            started: Instant::now(),
            fraction: AtomicU32::new(INDETERMINATE),
            cpu_nanos: AtomicU64::new(u64::MAX),
            text: Mutex::new((String::new(), String::new())),
        }
    }

    fn fraction(&self) -> Option<f32> {
        let bits = self.fraction.load(Ordering::Relaxed);
        (bits != INDETERMINATE).then(|| f32::from_bits(bits))
    }

    fn info(&self, outcome: Option<TaskOutcome>, elapsed: Duration) -> TaskInfo {
        let (text, details) = self.text.lock().clone();
        TaskInfo {
            id: self.id,
            title: self.title.clone(),
            text,
            details,
            fraction: self.fraction(),
            cancellable: self.cancellable,
            cancel_requested: self.token.is_cancelled(),
            hidden: self.hidden,
            elapsed,
            cpu_time: match self.cpu_nanos.load(Ordering::Relaxed) {
                u64::MAX => None,
                nanos => Some(Duration::from_nanos(nanos)),
            },
            outcome,
        }
    }
}

/// Progress and cancellation for one piece of work: what a background task
/// closure receives, and what long operations take as a parameter.
#[derive(Clone)]
pub struct ProgressReporter {
    shared: Arc<TaskShared>,
}

impl Default for ProgressReporter {
    fn default() -> Self {
        Self::new()
    }
}

impl ProgressReporter {
    /// A reporter that belongs to no manager (synchronous callers, tests),
    /// with a fresh token.
    pub fn new() -> Self {
        Self::with_token(CancellationToken::new())
    }

    /// A detached reporter checking `token`.
    pub fn with_token(token: CancellationToken) -> Self {
        Self {
            shared: Arc::new(TaskShared::new(0, String::new(), token)),
        }
    }

    pub fn token(&self) -> &CancellationToken {
        &self.shared.token
    }

    pub fn is_cancelled(&self) -> bool {
        self.shared.token.is_cancelled()
    }

    /// `Err(Cancelled)` once cancellation was requested, for `?` in loops.
    pub fn check(&self) -> Result<(), Cancelled> {
        sample_worker_cpu(&self.shared);
        self.shared.token.check()
    }

    /// Main progress text ("Searching for 'foo'…").
    pub fn set_text(&self, text: impl Into<String>) {
        sample_worker_cpu(&self.shared);
        self.shared.text.lock().0 = text.into();
    }

    /// Secondary text, usually the item being processed.
    pub fn set_details(&self, details: impl Into<String>) {
        self.shared.text.lock().1 = details.into();
    }

    /// Fraction done, clamped to `0.0..=1.0` (NaN means indeterminate).
    pub fn set_fraction(&self, fraction: f64) {
        let bits = if fraction.is_nan() {
            INDETERMINATE
        } else {
            (fraction.clamp(0.0, 1.0) as f32).to_bits()
        };
        self.shared.fraction.store(bits, Ordering::Relaxed);
    }

    /// Progress of unknown length (an animated bar).
    pub fn set_indeterminate(&self) {
        self.shared.fraction.store(INDETERMINATE, Ordering::Relaxed);
    }

    /// `done` of `total` items (indeterminate when `total` is 0).
    pub fn set_step(&self, done: usize, total: usize) {
        if total == 0 {
            self.set_indeterminate();
        } else {
            self.set_fraction(done as f64 / total as f64);
        }
    }

    /// Current `(text, details, fraction)`.
    pub fn progress(&self) -> (String, String, Option<f32>) {
        let (text, details) = self.shared.text.lock().clone();
        (text, details, self.shared.fraction())
    }
}

impl std::fmt::Debug for ProgressReporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (text, details, fraction) = self.progress();
        f.debug_struct("ProgressReporter")
            .field("text", &text)
            .field("details", &details)
            .field("fraction", &fraction)
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

/// The caller's end of a spawned task: its result, and cancellation.
/// Dropping the handle leaves the task running.
pub struct TaskHandle<T> {
    shared: Arc<TaskShared>,
    rx: Receiver<TaskResult<T>>,
    taken: bool,
}

impl<T> TaskHandle<T> {
    pub fn id(&self) -> TaskId {
        self.shared.id
    }

    pub fn title(&self) -> &str {
        &self.shared.title
    }

    pub fn token(&self) -> &CancellationToken {
        &self.shared.token
    }

    /// Requests cancellation; the task stops at its next check.
    pub fn cancel(&self) {
        self.shared.token.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.shared.token.is_cancelled()
    }

    /// The task's current `(text, details, fraction)` (see [`ProgressReporter::progress`]).
    pub fn progress(&self) -> (String, String, Option<f32>) {
        ProgressReporter {
            shared: self.shared.clone(),
        }
        .progress()
    }

    /// Time since the task started.
    pub fn elapsed(&self) -> Duration {
        self.shared.started.elapsed()
    }

    /// The result once the task ended, `None` while it runs (and after the
    /// result was taken). A task that died without a result reports
    /// `Failed`.
    pub fn try_result(&mut self) -> Option<TaskResult<T>> {
        if self.taken {
            return None;
        }
        let result = match self.rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                Err(TaskError::Failed("the task stopped unexpectedly".into()))
            }
        };
        self.taken = true;
        Some(result)
    }

    /// Like [`TaskHandle::try_result`], waiting up to `timeout`.
    pub fn wait_timeout(&mut self, timeout: Duration) -> Option<TaskResult<T>> {
        if self.taken {
            return None;
        }
        let result = match self.rx.recv_timeout(timeout) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => return None,
            Err(RecvTimeoutError::Disconnected) => {
                Err(TaskError::Failed("the task stopped unexpectedly".into()))
            }
        };
        self.taken = true;
        Some(result)
    }

    /// Blocks until the task ends.
    pub fn wait(mut self) -> TaskResult<T> {
        if self.taken {
            return Err(TaskError::Failed("the result was already taken".into()));
        }
        self.taken = true;
        self.rx
            .recv()
            .unwrap_or_else(|_| Err(TaskError::Failed("the task stopped unexpectedly".into())))
    }
}

impl<T> std::fmt::Debug for TaskHandle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskHandle")
            .field("id", &self.shared.id)
            .field("title", &self.shared.title)
            .finish()
    }
}

type Waker = Arc<dyn Fn() + Send + Sync>;

struct Inner {
    next_id: AtomicU64,
    running: Mutex<Vec<Arc<TaskShared>>>,
    finished: Mutex<VecDeque<TaskInfo>>,
    events: EventBus,
    waker: Mutex<Option<Waker>>,
}

/// Runs and tracks background tasks; clones share the task list.
#[derive(Clone)]
pub struct TaskManager {
    inner: Arc<Inner>,
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for TaskManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskManager")
            .field("running", &self.inner.running.lock().len())
            .finish()
    }
}

impl TaskManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                next_id: AtomicU64::new(1),
                running: Mutex::new(Vec::new()),
                finished: Mutex::new(VecDeque::new()),
                events: EventBus::new(),
                waker: Mutex::new(None),
            }),
        }
    }

    /// The application-wide manager whose tasks the status bar shows.
    pub fn global() -> &'static TaskManager {
        static GLOBAL: OnceLock<TaskManager> = OnceLock::new();
        GLOBAL.get_or_init(TaskManager::new)
    }

    /// Where [`TaskEvent`]s are published.
    pub fn events(&self) -> &EventBus {
        &self.inner.events
    }

    /// Called (from the worker thread) whenever a task ends, e.g. to repaint the UI.
    pub fn set_waker(&self, waker: impl Fn() + Send + Sync + 'static) {
        *self.inner.waker.lock() = Some(Arc::new(waker));
    }

    /// Starts configuring a task titled `title`.
    pub fn task(&self, title: impl Into<String>) -> TaskBuilder<'_> {
        TaskBuilder {
            manager: self,
            title: title.into(),
            cancellable: true,
            hidden: false,
            key: None,
            parent: None,
        }
    }

    /// Spawns a visible, cancellable task.
    pub fn spawn<T, F>(&self, title: impl Into<String>, f: F) -> TaskHandle<T>
    where
        T: Send + 'static,
        F: FnOnce(&ProgressReporter) -> TaskResult<T> + Send + 'static,
    {
        self.task(title).spawn(f)
    }

    /// Visible running tasks, newest first.
    pub fn running(&self) -> Vec<TaskInfo> {
        self.snapshot(false)
    }

    /// Running tasks including hidden ones, newest first.
    pub fn running_all(&self) -> Vec<TaskInfo> {
        self.snapshot(true)
    }

    fn snapshot(&self, include_hidden: bool) -> Vec<TaskInfo> {
        let running: Vec<Arc<TaskShared>> = self
            .inner
            .running
            .lock()
            .iter()
            .filter(|t| include_hidden || !t.hidden)
            .cloned()
            .collect();
        running
            .iter()
            .rev()
            .map(|t| t.info(None, t.started.elapsed()))
            .collect()
    }

    /// Recently finished visible tasks, newest first.
    pub fn finished(&self) -> Vec<TaskInfo> {
        self.inner.finished.lock().iter().cloned().collect()
    }

    pub fn clear_finished(&self) {
        self.inner.finished.lock().clear();
    }

    pub fn is_running(&self, id: TaskId) -> bool {
        self.inner.running.lock().iter().any(|t| t.id == id)
    }

    /// Whether a task spawned with [`TaskBuilder::exclusive`]`(key)` runs.
    pub fn is_key_running(&self, key: &str) -> bool {
        self.inner
            .running
            .lock()
            .iter()
            .any(|t| t.key.as_deref() == Some(key))
    }

    /// Requests cancellation of task `id`; false when it is not running or
    /// cannot be cancelled.
    pub fn cancel(&self, id: TaskId) -> bool {
        let task = self
            .inner
            .running
            .lock()
            .iter()
            .find(|t| t.id == id)
            .cloned();
        match task {
            Some(t) if t.cancellable => {
                t.token.cancel();
                true
            }
            _ => false,
        }
    }

    /// Cancels the running tasks spawned with `key`; returns how many.
    pub fn cancel_key(&self, key: &str) -> usize {
        let tasks: Vec<Arc<TaskShared>> = self
            .inner
            .running
            .lock()
            .iter()
            .filter(|t| t.key.as_deref() == Some(key))
            .cloned()
            .collect();
        for t in &tasks {
            t.token.cancel();
        }
        tasks.len()
    }

    /// Cancels every cancellable running task (closing the project, exiting).
    pub fn cancel_all(&self) {
        let tasks: Vec<Arc<TaskShared>> = self.inner.running.lock().clone();
        for t in tasks.iter().filter(|t| t.cancellable) {
            t.token.cancel();
        }
    }

    fn start<T, F>(&self, b: TaskBuilder<'_>, f: F) -> TaskHandle<T>
    where
        T: Send + 'static,
        F: FnOnce(&ProgressReporter) -> TaskResult<T> + Send + 'static,
    {
        if let Some(key) = &b.key {
            self.cancel_key(key);
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let token = b
            .parent
            .as_ref()
            .map(CancellationToken::child)
            .unwrap_or_default();
        let mut shared = TaskShared::new(id, b.title, token);
        shared.cancellable = b.cancellable;
        shared.hidden = b.hidden;
        shared.key = b.key;
        let shared = Arc::new(shared);
        self.inner.running.lock().push(shared.clone());
        self.inner.events.publish(TaskEvent::Started {
            id,
            title: shared.title.clone(),
        });
        let (tx, rx) = crossbeam_channel::bounded(1);
        let manager = self.clone();
        let worker = shared.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("task-{id}"))
            .spawn(move || {
                WORKER_CPU.with(|clock| {
                    *clock.borrow_mut() = cpu_time::ThreadTime::try_now()
                        .ok()
                        .map(|start| (Arc::as_ptr(&worker) as usize, start))
                });
                let reporter = ProgressReporter {
                    shared: worker.clone(),
                };
                let result =
                    catch_unwind(AssertUnwindSafe(|| f(&reporter))).unwrap_or_else(|payload| {
                        Err(TaskError::Failed(format!(
                            "internal error: {}",
                            panic_message(payload.as_ref())
                        )))
                    });
                let outcome = TaskOutcome::of(&result);
                sample_worker_cpu(&worker);
                WORKER_CPU.with(|clock| *clock.borrow_mut() = None);
                // The result first: whoever is woken up by `finish` finds it.
                let _ = tx.send(result);
                manager.finish(&worker, outcome);
            });
        if let Err(e) = spawned {
            self.finish(
                &shared,
                TaskOutcome::Failed(format!("cannot start a thread: {e}")),
            );
        }
        TaskHandle {
            shared,
            rx,
            taken: false,
        }
    }

    fn finish(&self, task: &Arc<TaskShared>, outcome: TaskOutcome) {
        let elapsed = task.started.elapsed();
        if task.hidden {
            self.inner.running.lock().retain(|t| !Arc::ptr_eq(t, task));
        } else {
            // Record the outcome before leaving the running list, holding the
            // finished lock across both, so no observer sees the task in
            // neither list. Lock order is finished -> running; every other
            // path takes at most one of the two locks at a time.
            let mut finished = self.inner.finished.lock();
            finished.push_front(task.info(Some(outcome.clone()), elapsed));
            finished.truncate(MAX_FINISHED);
            self.inner.running.lock().retain(|t| !Arc::ptr_eq(t, task));
        }
        match &outcome {
            TaskOutcome::Failed(m) => log::warn!(
                "background task '{}' failed after {} ms: {m}",
                task.title,
                elapsed.as_millis()
            ),
            _ => log::debug!(
                "background task '{}' {:?} after {} ms",
                task.title,
                outcome,
                elapsed.as_millis()
            ),
        }
        self.inner.events.publish(TaskEvent::Finished {
            id: task.id,
            title: task.title.clone(),
            outcome,
            elapsed,
        });
        let waker = self.inner.waker.lock().clone();
        if let Some(w) = waker {
            w();
        }
    }
}

/// Options of a task about to be spawned ([`TaskManager::task`]).
#[must_use = "a task builder does nothing until `spawn` is called"]
pub struct TaskBuilder<'a> {
    manager: &'a TaskManager,
    title: String,
    cancellable: bool,
    hidden: bool,
    key: Option<String>,
    parent: Option<CancellationToken>,
}

impl TaskBuilder<'_> {
    /// Whether the UI offers a cancel button (default: yes).
    pub fn cancellable(mut self, cancellable: bool) -> Self {
        self.cancellable = cancellable;
        self
    }

    /// Keeps the task out of the status bar and the Background Tasks popup
    /// (frequent short work such as re-analysing the file being edited).
    pub fn hidden(mut self) -> Self {
        self.hidden = true;
        self
    }

    /// Cancels running tasks spawned with the same key first: a newer
    /// search replaces an older one.
    pub fn exclusive(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// Makes the task's token a child of `parent`.
    pub fn child_of(mut self, parent: &CancellationToken) -> Self {
        self.parent = Some(parent.clone());
        self
    }

    /// Starts the task on its own thread.
    pub fn spawn<T, F>(self, f: F) -> TaskHandle<T>
    where
        T: Send + 'static,
        F: FnOnce(&ProgressReporter) -> TaskResult<T> + Send + 'static,
    {
        let manager = self.manager;
        manager.start(self, f)
    }
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::bounded;

    const LONG: Duration = Duration::from_secs(10);

    #[test]
    fn a_foreign_reporter_cannot_attribute_another_managers_worker_cpu() {
        let first = TaskManager::new();
        let second = TaskManager::new();
        let (reporter_tx, reporter_rx) = bounded(1);
        let (release_tx, release_rx) = bounded(1);
        let worker = first.spawn("Held owner", move |reporter| {
            reporter_tx.send(reporter.clone()).unwrap();
            release_rx.recv().unwrap();
            Ok(())
        });
        let foreign = reporter_rx.recv_timeout(LONG).unwrap();
        let shared = foreign.shared.clone();
        // Independent managers reuse IDs; CPU ownership must use task identity.
        let other = second.spawn("Other owner", move |reporter| {
            assert_eq!(reporter.shared.id, foreign.shared.id);
            foreign.check()?;
            foreign.set_text("Updated from another worker");
            Ok(())
        });
        let result = other.wait();
        let attributed = shared.cpu_nanos.load(Ordering::Relaxed);
        release_tx.send(()).unwrap();
        worker.wait().unwrap();
        result.unwrap();
        assert_eq!(attributed, u64::MAX);
    }

    #[test]
    fn task_result_reaches_the_handle_and_the_history() {
        let tm = TaskManager::new();
        let events = tm.events().subscribe::<TaskEvent>();
        let h = tm.spawn("Adding", |p| {
            p.set_text("adding");
            Ok(2 + 2)
        });
        let id = h.id();
        assert_eq!(h.wait(), Ok(4));
        // `finish` runs right after the result is sent.
        let finished = events
            .recv_timeout(LONG)
            .and_then(|_| events.recv_timeout(LONG))
            .unwrap();
        assert!(
            matches!(finished, TaskEvent::Finished { id: i, outcome: TaskOutcome::Finished, .. } if i == id),
            "{finished:?}"
        );
        assert!(!tm.is_running(id));
        let hist = tm.finished();
        assert_eq!(hist.len(), 1);
        assert_eq!(
            (
                hist[0].title.as_str(),
                hist[0].text.as_str(),
                hist[0].outcome.clone()
            ),
            ("Adding", "adding", Some(TaskOutcome::Finished))
        );
    }

    #[test]
    fn progress_is_visible_while_the_task_runs() {
        let tm = TaskManager::new();
        let (go_tx, go_rx) = bounded::<()>(0);
        let (ready_tx, ready_rx) = bounded::<()>(0);
        let h = tm.spawn("Indexing", move |p| {
            p.set_text("Indexing project");
            p.set_details("a.py");
            p.set_step(1, 4);
            ready_tx.send(()).unwrap();
            go_rx.recv().unwrap();
            Ok(())
        });
        ready_rx.recv_timeout(LONG).unwrap();
        assert_eq!(
            h.progress(),
            (
                "Indexing project".to_string(),
                "a.py".to_string(),
                Some(0.25)
            ),
            "the handle sees the progress too"
        );
        let running = tm.running();
        assert_eq!(running.len(), 1);
        let t = &running[0];
        assert_eq!(
            (
                t.title.as_str(),
                t.text.as_str(),
                t.details.as_str(),
                t.fraction
            ),
            ("Indexing", "Indexing project", "a.py", Some(0.25))
        );
        assert!(t.cancellable && !t.cancel_requested && t.outcome.is_none());
        go_tx.send(()).unwrap();
        assert_eq!(h.wait(), Ok(()));
    }

    #[test]
    fn cancelling_stops_a_task_at_its_next_check() {
        let tm = TaskManager::new();
        let (ready_tx, ready_rx) = bounded::<()>(1);
        let mut h = tm.spawn("Looping", move |p| -> TaskResult<()> {
            let _ = ready_tx.send(());
            loop {
                p.check()?;
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        ready_rx.recv_timeout(LONG).unwrap();
        assert!(tm.cancel(h.id()));
        assert_eq!(h.wait_timeout(LONG), Some(Err(TaskError::Cancelled)));
        assert_eq!(h.try_result(), None, "a result is handed out once");
        // The outcome is recorded before the task leaves the running list, so
        // a task that is no longer running is always in the finished list.
        let deadline = Instant::now() + LONG;
        while tm.is_running(h.id()) && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(!tm.is_running(h.id()));
        assert_eq!(tm.finished()[0].outcome, Some(TaskOutcome::Cancelled));
    }

    #[test]
    fn a_task_that_stopped_running_is_already_in_the_finished_list() {
        let tm = TaskManager::new();
        for round in 0..16 {
            let h = tm.spawn(format!("Quick {round}"), |_| -> TaskResult<()> { Ok(()) });
            let id = h.id();
            // Poll without waiting on the handle so the check races `finish`.
            let deadline = Instant::now() + LONG;
            while tm.is_running(id) && Instant::now() < deadline {
                std::hint::spin_loop();
            }
            assert!(
                tm.finished().iter().any(|t| t.id == id),
                "task {id} left the running list before its outcome was recorded"
            );
        }
    }

    #[test]
    fn non_cancellable_tasks_ignore_cancel_requests_from_the_ui() {
        let tm = TaskManager::new();
        let (go_tx, go_rx) = bounded::<()>(0);
        let h = tm.task("Saving").cancellable(false).spawn(move |p| {
            go_rx.recv().unwrap();
            Ok(p.is_cancelled())
        });
        assert!(!tm.cancel(h.id()));
        go_tx.send(()).unwrap();
        assert_eq!(h.wait(), Ok(false));
    }

    #[test]
    fn exclusive_tasks_replace_each_other() {
        let tm = TaskManager::new();
        let (ready_tx, ready_rx) = bounded::<()>(1);
        let first = tm.task("Search 1").exclusive("search").spawn(move |p| {
            let _ = ready_tx.send(());
            while !p.is_cancelled() {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err::<u32, _>(TaskError::Cancelled)
        });
        ready_rx.recv_timeout(LONG).unwrap();
        assert!(tm.is_key_running("search"));
        let second = tm.task("Search 2").exclusive("search").spawn(|_| Ok(2u32));
        assert!(first.is_cancelled());
        assert_eq!(first.wait(), Err(TaskError::Cancelled));
        assert_eq!(second.wait(), Ok(2));
        // Other keys are unaffected.
        let other = tm
            .task("Other")
            .exclusive("other")
            .spawn(|p| Ok(p.is_cancelled()));
        assert_eq!(other.wait(), Ok(false));
    }

    #[test]
    fn panics_and_errors_become_failed_outcomes() {
        let tm = TaskManager::new();
        let events = tm.events().subscribe::<TaskEvent>();
        let h = tm.spawn("Boom", |_| -> TaskResult<()> { panic!("kaboom") });
        assert_eq!(
            h.wait(),
            Err(TaskError::Failed("internal error: kaboom".into()))
        );
        let h = tm.spawn("Err", |_| -> TaskResult<()> { Err("disk full".into()) });
        assert_eq!(h.wait(), Err(TaskError::Failed("disk full".into())));
        let mut outcomes = Vec::new();
        while outcomes.len() < 2 {
            if let TaskEvent::Finished { outcome, .. } = events.recv_timeout(LONG).unwrap() {
                outcomes.push(outcome);
            }
        }
        assert_eq!(
            outcomes,
            vec![
                TaskOutcome::Failed("internal error: kaboom".into()),
                TaskOutcome::Failed("disk full".into())
            ]
        );
    }

    #[test]
    fn hidden_tasks_are_not_listed() {
        let tm = TaskManager::new();
        let (go_tx, go_rx) = bounded::<()>(0);
        let (ready_tx, ready_rx) = bounded::<()>(0);
        let h = tm.task("Analysing").hidden().spawn(move |_| {
            ready_tx.send(()).unwrap();
            go_rx.recv().unwrap();
            Ok(())
        });
        ready_rx.recv_timeout(LONG).unwrap();
        assert!(tm.running().is_empty());
        assert_eq!(tm.running_all().len(), 1);
        go_tx.send(()).unwrap();
        h.wait().unwrap();
        assert!(tm.finished().is_empty());
    }

    #[test]
    fn child_tokens_follow_the_parent() {
        let tm = TaskManager::new();
        let project = CancellationToken::new();
        let (ready_tx, ready_rx) = bounded::<()>(1);
        let h = tm.task("Under project").child_of(&project).spawn(move |p| {
            let _ = ready_tx.send(());
            while p.check().is_ok() {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err::<(), _>(Cancelled.into())
        });
        ready_rx.recv_timeout(LONG).unwrap();
        assert!(!h.token().same_as(&project));
        project.cancel();
        assert_eq!(h.wait(), Err(TaskError::Cancelled));
    }

    #[test]
    fn waker_runs_when_a_task_ends_and_running_lists_newest_first() {
        let tm = TaskManager::new();
        let (wake_tx, wake_rx) = bounded::<()>(8);
        tm.set_waker(move || {
            let _ = wake_tx.try_send(());
        });
        let (go_tx, go_rx) = bounded::<()>(0);
        let (ready_tx, ready_rx) = bounded::<()>(2);
        let ready2 = ready_tx.clone();
        let go2 = go_rx.clone();
        let a = tm.spawn("A", move |_| {
            ready_tx.send(()).unwrap();
            go_rx.recv().unwrap();
            Ok(())
        });
        ready_rx.recv_timeout(LONG).unwrap();
        let b = tm.spawn("B", move |_| {
            ready2.send(()).unwrap();
            go2.recv().unwrap();
            Ok(())
        });
        ready_rx.recv_timeout(LONG).unwrap();
        assert_eq!(
            tm.running()
                .iter()
                .map(|t| t.title.as_str())
                .collect::<Vec<_>>(),
            vec!["B", "A"]
        );
        tm.cancel_all();
        assert!(tm.running().iter().all(|t| t.cancel_requested));
        go_tx.send(()).unwrap();
        go_tx.send(()).unwrap();
        a.wait().unwrap();
        b.wait().unwrap();
        wake_rx.recv_timeout(LONG).unwrap();
        wake_rx.recv_timeout(LONG).unwrap();
    }

    #[test]
    fn detached_reporters_track_progress_and_cancellation() {
        let p = ProgressReporter::new();
        p.set_fraction(1.7);
        assert_eq!(p.progress().2, Some(1.0));
        p.set_fraction(f64::NAN);
        assert_eq!(p.progress().2, None);
        p.set_step(3, 0);
        assert_eq!(p.progress().2, None);
        p.set_step(1, 2);
        assert_eq!(p.progress().2, Some(0.5));
        let token = CancellationToken::new();
        let q = ProgressReporter::with_token(token.child());
        assert!(q.check().is_ok());
        token.cancel();
        assert_eq!(q.check(), Err(Cancelled));
    }
}
