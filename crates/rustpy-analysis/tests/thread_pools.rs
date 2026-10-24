//! Background scans (Problems › Project Errors, the usage searches of Find
//! Usages and Rename) and the usage searches the UI thread waits for (Show
//! Usages, Go to Declaration on a definition, Call Hierarchy, Safe Delete,
//! Change Signature) run in different thread pools: neither waits for the
//! other. They used to share rayon's global pool, where a UI-thread search
//! queued behind a whole project inspection (a 30 s UI freeze).
//!
//! A test here occupies every worker of one pool and checks that the work
//! of the other side still finishes. (Its own test binary: a blocked pool
//! cannot stall the crate's unit tests.)

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use rustpy_analysis::inspections::{self, InspectionOptions};
use rustpy_core::ProgressReporter;
use rustpy_search::usages;
use rustpy_semantic::{Analysis, FileInfo, TextSize};

/// The tests block pools the other one needs: one at a time.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// How long work may take before it counts as waiting for the blocked pool.
const PATIENCE: Duration = Duration::from_secs(60);

/// Blocks every worker of a pool until dropped.
struct Saturated {
    gate: Arc<(Mutex<bool>, Condvar)>,
}

impl Saturated {
    /// Occupies the `threads` workers `spawn` feeds; returns once each is blocked.
    fn new(threads: usize, spawn: impl Fn(Box<dyn FnOnce() + Send>)) -> Self {
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
        let deadline = Instant::now() + PATIENCE;
        while started.load(Ordering::SeqCst) < threads {
            assert!(
                Instant::now() < deadline,
                "the pool's workers never all started"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        Self { gate }
    }

    fn background_pool() -> Self {
        let pool = rustpy_core::parallel::background_pool().expect("the background pool");
        Self::new(pool.current_num_threads(), |job| pool.spawn(job))
    }

    fn global_pool() -> Self {
        Self::new(rayon::current_num_threads(), rayon::spawn)
    }
}

impl Drop for Saturated {
    fn drop(&mut self) {
        let (lock, cv) = &*self.gate;
        *lock.lock().unwrap() = true;
        cv.notify_all();
    }
}

/// Runs `op` on a fresh thread (like the UI thread or a task's thread: not
/// a pool worker); panics when it does not finish within [`PATIENCE`].
fn finishes<R: Send + 'static>(what: &str, op: impl FnOnce() -> R + Send + 'static) -> R {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(op());
    });
    rx.recv_timeout(PATIENCE)
        .unwrap_or_else(|_| panic!("{what} waited for the blocked pool"))
}

/// `helper()` defined in one file and called from 24 others.
fn project() -> (Arc<Analysis>, Arc<FileInfo>, TextSize, Vec<PathBuf>) {
    let db = Arc::new(Analysis::for_project(std::path::Path::new("/pools")));
    let def = PathBuf::from("/pools/helpers.py");
    db.set_file_text(&def, "def helper():\n    return 1\n");
    let mut paths = vec![def.clone()];
    for i in 0..24 {
        let p = PathBuf::from(format!("/pools/use{i}.py"));
        db.set_file_text(
            &p,
            &format!("from helpers import helper\n\nvalue_{i} = helper()\n"),
        );
        paths.push(p);
    }
    let file = db.file_for_path(&def).unwrap();
    (db, file, TextSize::new(4), paths)
}

#[test]
fn ui_searches_do_not_wait_for_background_scans() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let (db, file, at, paths) = project();
    let expected = usages::find_usages(&db, &file, at, &paths).len();
    assert_eq!(
        expected,
        1 + 24 * 2,
        "the definition, and an import and a call per file"
    );
    // A long background scan holds every worker of the background pool.
    let _scan = Saturated::background_pool();
    let (d, f, p) = (db.clone(), file.clone(), paths.clone());
    assert_eq!(
        finishes("Show Usages", move || usages::find_usages(&d, &f, at, &p)
            .len()),
        expected
    );
    let (d, f, p) = (db.clone(), file.clone(), paths.clone());
    let edits = finishes("Rename", move || {
        rustpy_refactor::rename::rename(&d, &f, at, "assist", &p).unwrap()
    });
    assert_eq!(edits.len(), 25);
    let (d, f, p) = (db.clone(), file.clone(), paths.clone());
    let callers = finishes("Call Hierarchy", move || {
        rustpy_search::usages::callers(&d, &f, at, &p).len()
    });
    assert_eq!(callers, 24);
}

#[test]
fn background_scans_do_not_wait_for_the_global_pool() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let (db, file, at, paths) = project();
    // The global pool is busy (with the UI's parallel work, say).
    let _ui = Saturated::global_pool();
    let (d, f, p) = (db.clone(), file.clone(), paths.clone());
    let found = finishes("Find Usages in the background", move || {
        usages::find_usages_with_progress(&d, &f, at, &p, &ProgressReporter::new())
            .unwrap()
            .len()
    });
    assert_eq!(found, 1 + 24 * 2);
    let (d, f, p) = (db.clone(), file.clone(), paths.clone());
    let edits = finishes("Rename in the background", move || {
        rustpy_refactor::rename::rename_with_progress(
            &d,
            &f,
            at,
            "assist",
            &p,
            &ProgressReporter::new(),
        )
        .unwrap()
    });
    assert_eq!(edits.len(), 25);
    let (d, p) = (db.clone(), paths.clone());
    let opts = InspectionOptions {
        pep8: false,
        ..InspectionOptions::default()
    };
    let inspected = finishes("Project Errors", move || {
        inspections::inspect_files(&d, &p, &opts, &ProgressReporter::new()).unwrap()
    });
    assert!(inspected.len() <= paths.len());
}
