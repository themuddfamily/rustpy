//! The thread pool of CPU-heavy background scans (roadmap §0.1, §8
//! performance): Problems › Project Errors, Find in Files and the usage
//! searches of Find Usages and Rename run their parallel loops here.
//!
//! Rayon's global pool is left to the parallel work the UI thread runs
//! synchronously (Show Usages, Go to Declaration on a definition, Call
//! Hierarchy, Safe Delete, Change Signature). Sharing one pool made those
//! wait for whole background scans: rayon workers finish their own queued
//! work before they take a job injected from outside the pool, so a 50 ms
//! usage search returned only when a 45 s project inspection had ended.
//!
//! The pool has one thread fewer than the machine has cores (at least
//! one), so the UI thread always has a core to itself. Measurements and
//! headless workloads can set `RUSTPY_BACKGROUND_THREADS` to an explicit
//! worker count instead.

use std::sync::OnceLock;

/// Name prefix of the pool's threads (thread dumps, the freeze detector).
pub const THREAD_NAME: &str = "bg-scan";

/// The background scan pool, created on first use. `None` when its threads
/// could not be started (then scans run in the caller's pool).
pub fn background_pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| {
        let available = std::thread::available_parallelism().map_or(2, |n| n.get());
        let threads = background_threads(std::env::var("RUSTPY_BACKGROUND_THREADS").ok().as_deref(), available);
        match rayon::ThreadPoolBuilder::new().num_threads(threads).thread_name(|i| format!("{THREAD_NAME}-{i}")).build() {
            Ok(pool) => Some(pool),
            Err(e) => {
                log::warn!("cannot start the background thread pool ({e}); background scans share the global pool");
                None
            }
        }
    })
    .as_ref()
}

/// Worker count: an explicit positive `RUSTPY_BACKGROUND_THREADS`, else one
/// fewer than the `available` cores (at least one).
fn background_threads(explicit: Option<&str>, available: usize) -> usize {
    explicit
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|threads| *threads > 0)
        .unwrap_or_else(|| available.saturating_sub(1).max(1))
}

/// Runs `op` in the [`background_pool`], so the parallel iterators it uses
/// run there; the calling thread blocks until `op` returns. Background
/// scans call this, never code the UI thread waits for.
pub fn in_background_pool<R: Send>(op: impl FnOnce() -> R + Send) -> R {
    match background_pool() {
        Some(pool) => pool.install(op),
        None => op(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rayon::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    #[test]
    fn worker_count_is_explicit_or_reserves_a_core_for_the_ui() {
        assert_eq!(background_threads(None, 10), 9);
        assert_eq!(background_threads(None, 1), 1);
        assert_eq!(background_threads(Some("10"), 2), 10);
        for invalid in ["0", "", "many"] {
            assert_eq!(background_threads(Some(invalid), 4), 3, "{invalid:?}");
        }
    }

    /// Blocks every worker of a pool until dropped (to show what does not need it).
    struct Saturated {
        gate: Arc<(Mutex<bool>, Condvar)>,
    }

    impl Saturated {
        /// Occupies all `threads` workers that `spawn` feeds; returns once each one is blocked.
        pub fn new(threads: usize, spawn: impl Fn(Box<dyn FnOnce() + Send>)) -> Self {
            let gate = Arc::new((Mutex::new(false), Condvar::new()));
            let started = Arc::new(AtomicUsize::new(0));
            for _ in 0..threads {
                let (gate, started) = (gate.clone(), started.clone());
                spawn(Box::new(move || {
                    started.fetch_add(1, Ordering::SeqCst);
                    let (lock, cv) = &*gate;
                    let mut open = lock.lock().unwrap();
                    while !*open {
                        open = cv.wait(open).unwrap();
                    }
                }));
            }
            let deadline = Instant::now() + Duration::from_secs(30);
            while started.load(Ordering::SeqCst) < threads {
                assert!(
                    Instant::now() < deadline,
                    "the pool's workers never all started"
                );
                std::thread::sleep(Duration::from_millis(2));
            }
            Self { gate }
        }
    }

    impl Drop for Saturated {
        fn drop(&mut self) {
            let (lock, cv) = &*self.gate;
            *lock.lock().unwrap() = true;
            cv.notify_all();
        }
    }

    /// Runs `op` on a fresh thread; `None` when it takes longer than `timeout`.
    fn finishes_within<R: Send + 'static>(
        timeout: Duration,
        op: impl FnOnce() -> R + Send + 'static,
    ) -> Option<R> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(op());
        });
        rx.recv_timeout(timeout).ok()
    }

    #[test]
    fn background_scans_and_the_global_pool_do_not_wait_for_each_other() {
        let pool = background_pool().expect("a pool");
        assert_eq!(
            pool.current_num_threads(),
            std::thread::available_parallelism()
                .map_or(2, |n| n.get())
                .saturating_sub(1)
                .max(1)
        );
        // Parallel work inside runs on the pool's named threads.
        let names: Vec<String> = in_background_pool(|| {
            (0..64)
                .into_par_iter()
                .map(|_| std::thread::current().name().unwrap_or("").to_string())
                .collect()
        });
        assert!(
            names.iter().all(|n| n.starts_with(THREAD_NAME)),
            "{names:?}"
        );
        let sum = |n: u64| (0..n).into_par_iter().sum::<u64>();
        // Every background worker busy (a long scan): the global pool still serves the UI.
        {
            let _busy = Saturated::new(pool.current_num_threads(), |job| pool.spawn(job));
            assert_eq!(
                finishes_within(Duration::from_secs(20), move || sum(10_000)),
                Some(49_995_000),
                "global-pool work waited for the background pool"
            );
        }
        // And a busy global pool does not hold up background scans.
        {
            let _busy = Saturated::new(rayon::current_num_threads(), rayon::spawn);
            assert_eq!(
                finishes_within(Duration::from_secs(20), move || in_background_pool(|| sum(
                    10_000
                ))),
                Some(49_995_000),
                "a background scan waited for the global pool"
            );
        }
    }
}
