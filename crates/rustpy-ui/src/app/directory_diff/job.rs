//! App-owned task-domain bridge. Cancellation acknowledgement is polled, never inferred.
use super::inventory::{self, Limits, Report};
use rustpy_core::{CancellationToken, TaskHandle, TaskManager};
use std::path::PathBuf;

pub(crate) struct DirectoryJob {
    manager: TaskManager,
    handle: TaskHandle<Report>,
    result: Option<Result<Report, String>>,
}
impl DirectoryJob {
    /// Retain ownership until both result acknowledgement and manager retirement.
    /// Observation only: consent/stamp checks must not consume the task result.
    pub(crate) fn pending(&self) -> bool {
        self.result.is_none() || self.manager.is_running(self.handle.id())
    }

    pub(crate) fn spawn(
        manager: &TaskManager,
        owner: &CancellationToken,
        left: PathBuf,
        right: PathBuf,
    ) -> Self {
        let handle = manager
            .task("Compare directories")
            .child_of(owner)
            .spawn(move |progress| {
                Ok(inventory::scan(
                    left,
                    right,
                    Limits::default(),
                    progress.token(),
                ))
            });
        Self {
            manager: manager.clone(),
            handle,
            result: None,
        }
    }
    pub(crate) fn cancel(&self) {
        self.handle.cancel();
    }
    pub(crate) fn poll(&mut self) {
        if self.result.is_none() {
            if let Some(result) = self.handle.try_result() {
                self.result = Some(result.map_err(|error| error.to_string()));
            }
        }
    }
    /// Worker result AND manager retirement must both be observed before draining.
    pub(crate) fn drained(&mut self) -> bool {
        self.poll();
        self.result.is_some() && !self.manager.is_running(self.handle.id())
    }
    pub(super) fn take_result(&mut self) -> Option<Result<Report, String>> {
        self.poll();
        // Keep the result acknowledgement independently of UI delivery.
        self.result.clone()
    }
    pub(crate) fn quit_stamp(&self) -> String {
        format!(
            "directory-task:{}:{}:{}",
            self.handle.id(),
            self.manager.is_running(self.handle.id()),
            self.handle.is_cancelled()
        )
    }
}
impl Drop for DirectoryJob {
    fn drop(&mut self) {
        self.cancel();
    }
}
