//! Find in Files is a background scan: it runs in the background thread
//! pool of `rustpy_core::parallel`, so a busy global pool (the parallel work
//! the UI thread waits for) does not hold it up, and it never holds up that
//! work. (Its own test binary: the blocked pool cannot stall other tests.)

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use rustpy_search::{find_in_files_with, SearchControl, SearchOptions, SearchSources};

const PATIENCE: Duration = Duration::from_secs(60);

#[test]
fn find_in_files_does_not_use_the_global_pool() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..40 {
        std::fs::write(
            dir.path().join(format!("f{i}.py")),
            format!("needle_{i} = {i}\nother = 0\n"),
        )
        .unwrap();
    }
    // Every worker of the global pool blocked until the gate opens.
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let started = Arc::new(AtomicUsize::new(0));
    let threads = rayon::current_num_threads();
    for _ in 0..threads {
        let (gate, started) = (gate.clone(), started.clone());
        rayon::spawn(move || {
            started.fetch_add(1, Ordering::SeqCst);
            let (lock, cv) = &*gate;
            let mut open = lock.lock().unwrap();
            while !*open {
                open = cv.wait(open).unwrap();
            }
        });
    }
    let deadline = Instant::now() + PATIENCE;
    while started.load(Ordering::SeqCst) < threads {
        assert!(
            Instant::now() < deadline,
            "the global pool's workers never all started"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    let root = dir.path().to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let found = Mutex::new(0usize);
        let sink = |batch: Vec<rustpy_search::SearchMatch>| *found.lock().unwrap() += batch.len();
        let summary = find_in_files_with(
            &root,
            "needle_",
            &SearchOptions::default(),
            &SearchSources::default(),
            &SearchControl::default(),
            &sink,
        )
        .unwrap();
        let _ = tx.send((summary.files_searched, found.into_inner().unwrap()));
    });
    let result = rx.recv_timeout(PATIENCE);
    let (lock, cv) = &*gate;
    *lock.lock().unwrap() = true;
    cv.notify_all();
    assert_eq!(
        result.expect("Find in Files waited for the global pool"),
        (40, 40)
    );
}
