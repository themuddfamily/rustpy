//! Atomic replacement of small state files that other processes may be
//! reading at the same moment.
//!
//! On Unix `rename(2)` replaces the destination atomically regardless of open
//! readers. On Windows `std::fs::rename` asks NTFS for POSIX rename semantics,
//! which also replaces a destination that readers hold open, provided they
//! opened it with `FILE_SHARE_DELETE` (the standard library's default share
//! mode). A reader that opened the file without delete sharing (an indexer,
//! a virus scanner) or a file system without POSIX semantics still makes the
//! replacement fail with a sharing violation for a moment; such failures are
//! retried briefly before they are reported.
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

/// How long a transient sharing conflict is retried before it is reported.
const SHARING_RETRY: Duration = Duration::from_millis(750);

/// Whether `error` is a Windows sharing conflict that resolves once another
/// handle closes: `ERROR_ACCESS_DENIED` (a replaced or deleted file whose
/// last handle is still open), `ERROR_SHARING_VIOLATION` or
/// `ERROR_LOCK_VIOLATION`. Always false elsewhere.
pub fn is_sharing_conflict(error: &io::Error) -> bool {
    cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32 | 33))
}

/// Runs `attempt` again while it fails with a transient sharing conflict,
/// for a bounded time. Other errors and the last conflict are returned.
pub fn retry_sharing<T>(mut attempt: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let deadline = Instant::now() + SHARING_RETRY;
    let mut pause = Duration::from_millis(1);
    loop {
        match attempt() {
            Err(error) if is_sharing_conflict(&error) && Instant::now() < deadline => {
                std::thread::sleep(pause);
                pause = (pause * 2).min(Duration::from_millis(25));
            }
            result => return result,
        }
    }
}

/// Atomically replaces `to` with `from`; readers see the old or the new file.
pub fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    retry_sharing(|| std::fs::rename(from, to))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn replaces_a_destination_that_a_reader_holds_open() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("state.json");
        std::fs::write(&destination, b"old").unwrap();
        let mut reader = std::fs::File::open(&destination).unwrap();
        let source = directory.path().join("state.tmp");
        std::fs::write(&source, b"new").unwrap();
        replace_file(&source, &destination).unwrap();
        let mut old = String::new();
        reader.read_to_string(&mut old).unwrap();
        assert_eq!(old, "old", "an open reader keeps the file it opened");
        assert_eq!(std::fs::read(&destination).unwrap(), b"new");
        assert!(!source.exists());
    }

    #[test]
    fn retry_returns_other_errors_at_once() {
        let mut calls = 0;
        let error = retry_sharing::<()>(|| {
            calls += 1;
            Err(io::Error::from(io::ErrorKind::NotFound))
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(calls, 1);
    }
}
