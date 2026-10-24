//! Physical request ownership. Delivery cancellation is not worker retirement:
//! entries survive handle drop until the network closure has really returned.
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, MutexGuard, OnceLock,
    },
};
static NEXT: AtomicU64 = AtomicU64::new(1);
static WORKERS: OnceLock<Mutex<BTreeMap<u64, Entry>>> = OnceLock::new();
struct Entry {
    owner: u64,
    kind: String,
    cancel: Arc<AtomicBool>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerInfo {
    pub id: u64,
    pub owner: u64,
    pub kind: String,
    pub cancel_requested: bool,
}
fn entries() -> MutexGuard<'static, BTreeMap<u64, Entry>> {
    WORKERS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}
/// Allocate one stable owner per application/window, not per request.
pub fn new_worker_owner() -> u64 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}
pub fn active_workers() -> Vec<WorkerInfo> {
    entries()
        .iter()
        .map(|(id, entry)| WorkerInfo {
            id: *id,
            owner: entry.owner,
            kind: entry.kind.clone(),
            cancel_requested: entry.cancel.load(Ordering::Acquire),
        })
        .collect()
}
pub fn worker_ids_for_owner(owner: u64) -> Vec<u64> {
    entries()
        .iter()
        .filter_map(|(id, entry)| (entry.owner == owner).then_some(*id))
        .collect()
}
/// Cancel exactly a captured set; a newer request cannot be cancelled by an old
/// Stop or quit snapshot. This does not remove physically running entries.
pub fn cancel_workers(ids: &[u64]) {
    let workers = entries();
    for id in ids {
        if let Some(entry) = workers.get(id) {
            entry.cancel.store(true, Ordering::Release);
        }
    }
}
pub fn workers_running(ids: &[u64]) -> bool {
    let workers = entries();
    ids.iter().any(|id| workers.contains_key(id))
}
pub(crate) struct Guard {
    pub id: u64,
    pub cancel: Arc<AtomicBool>,
}
impl Guard {
    pub fn register(owner: u64, kind: &str, cancel: Arc<AtomicBool>) -> Self {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        entries().insert(
            id,
            Entry {
                owner,
                kind: kind.into(),
                cancel: cancel.clone(),
            },
        );
        Self { id, cancel }
    }
    pub fn check(&self) -> anyhow::Result<()> {
        if self.cancel.load(Ordering::Acquire) {
            anyhow::bail!("The request was cancelled.");
        }
        Ok(())
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        entries().remove(&self.id);
    }
}
pub(crate) struct Reader<R> {
    pub inner: R,
    pub cancel: Arc<AtomicBool>,
}
impl<R: std::io::Read> std::io::Read for Reader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let cancelled = || {
            std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "The request was cancelled.",
            )
        };
        if self.cancel.load(Ordering::Acquire) {
            return Err(cancelled());
        }
        let result = self.inner.read(buffer);
        if self.cancel.load(Ordering::Acquire) {
            return Err(cancelled());
        }
        result
    }
}
#[cfg(test)]
mod fixtures {
    use super::*;
    #[test]
    fn cancellation_is_not_retirement_and_does_not_cancel_replacement() {
        let owner = new_worker_owner();
        let first = Guard::register(owner, "stream", Arc::new(AtomicBool::new(false)));
        let captured = worker_ids_for_owner(owner);
        cancel_workers(&captured);
        assert!(first.cancel.load(Ordering::Acquire));
        assert!(workers_running(&captured));
        let replacement = Guard::register(owner, "replacement", Arc::new(AtomicBool::new(false)));
        assert!(!replacement.cancel.load(Ordering::Acquire));
        drop(first);
        assert!(!workers_running(&captured));
        assert!(workers_running(&[replacement.id]));
    }
    #[test]
    fn physical_owners_are_isolated() {
        let a = new_worker_owner();
        let b = new_worker_owner();
        let first = Guard::register(a, "request", Arc::new(AtomicBool::new(false)));
        let second = Guard::register(b, "request", Arc::new(AtomicBool::new(false)));
        cancel_workers(&worker_ids_for_owner(a));
        assert!(first.check().is_err());
        assert!(second.check().is_ok());
    }
}

#[cfg(test)]
mod blocked_reader_fixture {
    use super::*;
    use std::io::Read;
    struct BlockedRead(std::sync::mpsc::Receiver<()>);
    impl Read for BlockedRead {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            self.0.recv().unwrap();
            Ok(0)
        }
    }
    #[test]
    fn cancel_keeps_physical_worker_until_blocking_read_returns() {
        let owner = new_worker_owner();
        let guard = Guard::register(owner, "blocked request", Arc::new(AtomicBool::new(false)));
        let id = guard.id;
        let (release, wait) = std::sync::mpsc::channel();
        let (started, ready) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let guard = guard;
            let mut reader = Reader {
                inner: BlockedRead(wait),
                cancel: guard.cancel.clone(),
            };
            started.send(()).unwrap();
            assert!(reader.read(&mut [0; 1]).is_err());
        });
        ready.recv().unwrap();
        cancel_workers(&[id]);
        assert!(workers_running(&[id]));
        release.send(()).unwrap();
        worker.join().unwrap();
        assert!(!workers_running(&[id]));
    }
}
