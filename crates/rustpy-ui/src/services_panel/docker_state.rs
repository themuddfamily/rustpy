//! Docker admission and retained snapshot ownership, independent of tree visibility.
use rustpy_docker::{Capture, Snapshot, SnapshotTask};
use rustpy_settings::trust::DirectoryIdentity;
use std::path::{Path, PathBuf};
#[derive(Clone, Debug, PartialEq, Eq)]
struct Scope {
    generation: u64,
    program: String,
    root: PathBuf,
    identity: DirectoryIdentity,
}
#[derive(Default)]
pub struct DockerState {
    generation: u64,
    pub snapshot: Option<Result<Snapshot, String>>,
    pub program: String,
    pub force_refresh: bool,
    admission: Option<Scope>,
    task: Option<(Scope, SnapshotTask)>,
    last_refresh: f64,
    paused: bool,
    explicit_refresh: bool,
}
impl DockerState {
    /// Called by the host on every normal/hidden retirement tick. Changing or
    /// revoking authority cancels, but never drops, the previously captured owner.
    pub fn set_admission(&mut self, allowed: bool, root: &Path) {
        let mut scope = if allowed {
            root.canonicalize().ok().and_then(|canonical| {
                DirectoryIdentity::capture(&canonical)
                    .ok()
                    .map(|identity| Scope {
                        generation: self.generation,
                        program: if self.program.trim().is_empty() {
                            "docker".into()
                        } else {
                            self.program.clone()
                        },
                        root: canonical,
                        identity,
                    })
            })
        } else {
            None
        };
        if self.admission != scope {
            self.generation = self.generation.wrapping_add(1);
            if let Some(scope) = &mut scope {
                scope.generation = self.generation;
            }
            if let Some((_, task)) = &self.task {
                task.cancel();
            }
            self.admission = scope;
            self.snapshot = None;
            self.last_refresh = f64::NEG_INFINITY;
            self.force_refresh = true;
            self.paused = false;
            self.explicit_refresh = false;
        }
    }
    pub fn admitted(&self) -> bool {
        self.admission
            .as_ref()
            .is_some_and(|scope| scope.identity.matches(&scope.root))
    }
    pub fn paused(&self) -> bool {
        self.paused
    }
    pub fn matches_admission(&self, root: &Path, program: &str) -> bool {
        let program = if program.trim().is_empty() {
            "docker"
        } else {
            program
        };
        self.admission.as_ref().is_some_and(|scope| {
            root.canonicalize().is_ok_and(|root| {
                scope.root == root && scope.program == program && scope.identity.matches(&root)
            })
        })
    }
    pub fn drain_status(&self) -> Option<String> {
        self.task.as_ref().map(|(_, task)| task.drain_status())
    }
    pub fn pending(&self) -> Option<(u64, String, String)> {
        self.task.as_ref().map(|(scope, task)| {
            (
                task.id(),
                task.label(),
                format!("{:?}:{}", scope.identity, task.stamp()),
            )
        })
    }
    /// Stop suppresses timed and incidental refreshes until explicit Refresh or a
    /// genuinely different admitted project/executable is selected.
    pub fn cancel(&mut self) {
        self.paused = true;
        self.force_refresh = false;
        self.explicit_refresh = false;
        if let Some((_, task)) = &self.task {
            task.cancel();
        }
    }
    pub fn stop(&mut self, id: u64, stamp: &str) -> bool {
        if self
            .pending()
            .is_some_and(|(current, _, state)| current == id && state == stamp)
        {
            self.cancel();
            true
        } else {
            false
        }
    }
    pub fn request_refresh(&mut self) {
        self.paused = false;
        self.force_refresh = true;
        self.explicit_refresh = true;
    }
    /// Polling is safe while hidden or during frozen quit: no new worker starts.
    pub fn poll_retirement(&mut self, now: f64, ctx: &egui::Context) {
        if !self.admitted() {
            if let Some((_, task)) = &self.task {
                task.cancel();
            }
            self.snapshot = None;
        }
        let result = self.task.as_mut().and_then(|(_, task)| task.try_result());
        if let Some(result) = result {
            let (scope, task) = self.task.take().expect("completed Docker owner");
            if self.admission.as_ref() == Some(&scope)
                && task.is_cancelled()
                && !self.explicit_refresh
            {
                self.paused = true;
                self.force_refresh = false;
            }
            if self.admission.as_ref() == Some(&scope)
                && scope.identity.matches(&scope.root)
                && !task.is_cancelled()
                && !self.paused
            {
                self.snapshot = Some(result);
                self.last_refresh = now;
            }
        }
        if self.task.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
    /// Only the visible, admitted Docker node requests periodic new snapshots.
    pub fn refresh_if_needed(&mut self, now: f64, ctx: &egui::Context) {
        self.poll_retirement(now, ctx);
        if self.task.is_some() || self.paused || !self.admitted() {
            return;
        }
        let stale = self.snapshot.is_none() || now - self.last_refresh > 10. || self.force_refresh;
        if stale {
            self.force_refresh = false;
            self.explicit_refresh = false;
            self.last_refresh = now;
            let scope = self.admission.as_ref().unwrap().clone();
            match Capture::current(scope.program.clone(), &scope.root) {
                Ok(capture) if capture.identity() == &scope.identity => {
                    self.task = Some((scope, SnapshotTask::start(capture)));
                }
                Ok(_) => {
                    self.snapshot = Some(Err(
                        "Docker project identity changed before admission".into()
                    ));
                }
                Err(error) => self.snapshot = Some(Err(format!("{error:#}"))),
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_secs(10));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_state_cannot_admit_or_start_a_snapshot() {
        let mut state = DockerState {
            program: "never-start-this".into(),
            ..Default::default()
        };
        state.set_admission(false, Path::new("/not-a-project"));
        assert!(!state.admitted());
        assert!(state.pending().is_none());
        assert!(state.snapshot.is_none());
    }
    #[test]
    fn stop_does_not_make_incidental_refreshes_restart_a_snapshot() {
        let mut state = DockerState::default();
        state.cancel();
        state.force_refresh = true;
        assert!(state.paused());
        state.request_refresh();
        assert!(!state.paused());
        assert!(state.force_refresh);
    }
    #[test]
    fn unknown_or_changed_stop_identity_is_rejected() {
        let mut state = DockerState::default();
        assert!(!state.stop(37, "old-owner"));
        assert!(!state.paused());
    }
    #[test]
    fn regrant_after_revocation_is_a_new_admission_even_for_the_same_root() {
        let root = std::env::current_dir().unwrap().canonicalize().unwrap();
        let mut state = DockerState::default();
        state.set_admission(true, &root);
        let original = state.admission.clone().unwrap();
        state.set_admission(false, &root);
        assert!(!state.admitted());
        state.set_admission(true, &root);
        assert_ne!(state.admission.as_ref(), Some(&original));
        assert!(state.pending().is_none());
    }
}
