//! Real kernel ownership checks for the shared instance/window lease guard.
use super::{try_existing_lock_file, try_lock_file};
use std::os::unix::io::AsRawFd;

#[test]
fn failed_contender_does_not_unlock_primary_lease() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lease");
    let primary = try_lock_file(&path).unwrap().unwrap();
    assert!(try_lock_file(&path).unwrap().is_none());
    assert!(try_existing_lock_file(&path).unwrap().is_none());
    // Both failed acquisition paths have closed their temporary descriptors.
    assert!(try_existing_lock_file(&path).unwrap().is_none());
    drop(primary);
    assert!(try_existing_lock_file(&path).unwrap().is_some());
}

#[test]
fn old_guard_does_not_unlock_independently_acquired_successor() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("lease");
    let old = try_lock_file(&path).unwrap().unwrap();
    // Release only for this setup, retaining the old descriptor/guard. A fresh
    // open of the same inode can now acquire an independent file description.
    // SAFETY: old owns the live descriptor throughout this call.
    assert_eq!(
        unsafe { libc::flock(old._file.as_raw_fd(), libc::LOCK_UN) },
        0
    );
    let successor = try_existing_lock_file(&path).unwrap().unwrap();
    drop(old);
    assert!(try_lock_file(&path).unwrap().is_none());
    assert!(try_existing_lock_file(&path).unwrap().is_none());
    drop(successor);
    assert!(try_existing_lock_file(&path).unwrap().is_some());
}
