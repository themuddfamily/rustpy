//! Captured transfer ownership, cooperative cancellation and bounded progress.
use crate::options::Options;
use std::{
    cell::RefCell,
    collections::VecDeque,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
};
static NEXT: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Debug)]
pub struct Progress {
    pub current: usize,
    pub total: usize,
    pub path: String,
    pub message: String,
}
#[derive(Clone)]
pub struct TransferOwner {
    pub id: u64,
    pub root: PathBuf,
    pub label: String,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    progress: Arc<Mutex<VecDeque<Progress>>>,
    stamp: Arc<Mutex<String>>,
}
impl TransferOwner {
    pub fn new(root: PathBuf, label: String) -> Self {
        Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            root,
            label,
            cancel: Arc::new(AtomicBool::new(false)),
            done: Arc::new(AtomicBool::new(false)),
            progress: Arc::new(Mutex::new(VecDeque::new())),
            stamp: Arc::new(Mutex::new(String::new())),
        }
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }
    pub fn finished(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }
    pub fn finish(&self) {
        self.done.store(true, Ordering::Release);
    }
    pub fn lifecycle_stamp(&self) -> String {
        format!(
            "transfer:{}:root={}:cancel={}:finished={}:{}",
            self.id,
            self.root.display(),
            self.cancelled(),
            self.finished(),
            self.stamp.lock().unwrap()
        )
    }
    pub fn drain_progress(&self) -> Vec<Progress> {
        self.progress.lock().unwrap().drain(..).collect()
    }
    pub fn progress(&self, current: usize, total: usize, path: &str, message: &str) {
        let mut queue = self.progress.lock().unwrap();
        if queue.len() >= 128 {
            queue.pop_front();
        }
        queue.push_back(Progress {
            current,
            total,
            path: path.into(),
            message: message.into(),
        });
    }
    pub fn run<T>(&self, f: impl FnOnce() -> T) -> T {
        with_context(
            Context {
                owner: Some(self.clone()),
                ..current()
            },
            f,
        )
    }
}
#[derive(Clone, Default)]
pub struct Context {
    pub owner: Option<TransferOwner>,
    pub options: Options,
    pub environment: Vec<(String, String)>,
}
thread_local! {static CONTEXT:RefCell<Context>=RefCell::new(Context::default());}
pub fn current() -> Context {
    CONTEXT.with(|c| c.borrow().clone())
}
pub fn with_context<T>(value: Context, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<Context>);
    impl Drop for Restore {
        fn drop(&mut self) {
            if let Some(old) = self.0.take() {
                CONTEXT.with(|c| *c.borrow_mut() = old);
            }
        }
    }
    let old = CONTEXT.with(|c| c.replace(value));
    let _restore = Restore(Some(old));
    f()
}
pub fn check() -> anyhow::Result<()> {
    anyhow::ensure!(
        !current().owner.is_some_and(|o| o.cancelled()),
        "Transfer cancelled"
    );
    Ok(())
}
pub fn note(message: &str) {
    if let Some(owner) = current().owner {
        owner.progress(0, 0, "", message);
    }
}
pub fn process_stamp(stamp: String) {
    if let Some(owner) = current().owner {
        *owner.stamp.lock().unwrap() = stamp;
    }
}
#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn owner_context_restores_after_nested_scope_and_pre_cancel_never_spawns() {
        let owner = TransferOwner::new("/project".into(), "Upload".into());
        owner.cancel();
        let result = owner.run(|| {
            crate::exec::run(
                &crate::CommandSpec::new("must-not-start"),
                std::time::Duration::from_secs(1),
            )
        });
        assert!(matches!(result, Err(crate::ExecError::Cancelled)));
        assert!(current().owner.is_none());
        assert!(owner.lifecycle_stamp().contains("cancel=true"));
    }
    #[test]
    fn progress_is_bounded_and_drain_is_consuming() {
        let owner = TransferOwner::new("/p".into(), "Copy".into());
        for index in 0..500 {
            owner.progress(index, 500, "a.py", "Copy");
        }
        let progress = owner.drain_progress();
        assert_eq!(progress.len(), 128);
        assert_eq!(progress.last().unwrap().current, 499);
        assert!(owner.drain_progress().is_empty());
    }
}
