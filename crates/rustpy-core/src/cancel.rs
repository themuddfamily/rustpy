//! Cooperative cancellation (roadmap §0.1): tokens shared between the UI
//! thread and background work, with child tokens for sub-operations.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Cooperative cancellation token shared between the UI thread and background work.
///
/// Clones share one flag. [`CancellationToken::child`] makes a token that is
/// cancelled together with its parent (and every ancestor) but can also be
/// cancelled on its own without affecting the parent: a background task owns
/// a token, and each run of its inner loop gets a child that a newer run can
/// cancel.
#[derive(Clone, Default)]
pub struct CancellationToken {
    inner: Arc<Node>,
}

#[derive(Default)]
struct Node {
    cancelled: AtomicBool,
    parent: Option<Arc<Node>>,
}

/// Error returned by [`CancellationToken::check`] when work should stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("operation cancelled")]
pub struct Cancelled;

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// A token cancelled when `self` (or any of its ancestors) is, or when
    /// it is cancelled itself. Cancelling the child leaves `self` untouched.
    pub fn child(&self) -> Self {
        Self {
            inner: Arc::new(Node {
                cancelled: AtomicBool::new(false),
                parent: Some(self.inner.clone()),
            }),
        }
    }

    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        if self.inner.cancelled.load(Ordering::Acquire) {
            return true;
        }
        let mut node = self.inner.parent.as_deref();
        while let Some(n) = node {
            if n.cancelled.load(Ordering::Acquire) {
                // Remember it: later checks stop at this token.
                self.inner.cancelled.store(true, Ordering::Release);
                return true;
            }
            node = n.parent.as_deref();
        }
        false
    }

    /// `Err(Cancelled)` once the token is cancelled, for `?` in work loops.
    pub fn check(&self) -> Result<(), Cancelled> {
        if self.is_cancelled() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }

    /// Whether both handles share the same flag (clones of one token).
    pub fn same_as(&self, other: &CancellationToken) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl fmt::Debug for CancellationToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CancellationToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_roundtrip() {
        let t = CancellationToken::new();
        assert!(t.check().is_ok());
        t.cancel();
        assert_eq!(t.check(), Err(Cancelled));
    }

    #[test]
    fn clones_share_the_flag() {
        let t = CancellationToken::new();
        let c = t.clone();
        assert!(t.same_as(&c));
        c.cancel();
        assert!(t.is_cancelled());
    }

    #[test]
    fn parent_cancellation_reaches_every_descendant() {
        let root = CancellationToken::new();
        let child = root.child();
        let grandchild = child.child();
        let sibling = root.child();
        assert!(!grandchild.is_cancelled());
        root.cancel();
        assert!(child.is_cancelled() && grandchild.is_cancelled() && sibling.is_cancelled());
        // Children made after the parent was cancelled start cancelled.
        assert!(root.child().is_cancelled());
    }

    #[test]
    fn child_cancellation_does_not_propagate_up_or_sideways() {
        let root = CancellationToken::new();
        let a = root.child();
        let b = root.child();
        let a1 = a.child();
        a.cancel();
        assert!(a.is_cancelled() && a1.is_cancelled());
        assert!(!root.is_cancelled());
        assert!(!b.is_cancelled());
        assert!(!a.same_as(&b));
    }

    #[test]
    fn cancellation_is_seen_across_threads() {
        let t = CancellationToken::new();
        let child = t.child();
        let worker = std::thread::spawn(move || {
            let mut spins = 0u64;
            while child.check().is_ok() {
                spins += 1;
                std::thread::yield_now();
            }
            spins
        });
        std::thread::sleep(std::time::Duration::from_millis(5));
        t.cancel();
        worker.join().unwrap();
    }
}
