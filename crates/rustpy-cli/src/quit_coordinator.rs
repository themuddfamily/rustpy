//! Worker-side guarded application Quit. UI approval and durable verification
//! remain participant obligations; transport ACK/EOF never authorizes closing.
//! Root supplies exact accepted owner/endpoint bindings, including Welcome.
//! Project equality alone cannot identify duplicate-project or Welcome owners.
//! Network iterations share an absolute deadline; Store locking/fsync uses its
//! existing bounded-lock and filesystem semantics, not a cancellable disk I/O
//! deadline. Root must drive this on a worker and keep GUI control consumption
//! active, including its own Commit. Actual UI guard freezing and verified
//! exit permission are caller obligations.
use crate::{
    ipc,
    window_admission::{QuitAdmissionLease, WindowAdmission},
    window_registry,
};
use rustpy_settings::window_session_store::{
    GuardedQuitBinding, GuardedQuitCertificate, GuardedQuitWindow, QuitUiGuard, WindowSessionError,
    WindowSessionSnapshot, WindowSessionStore, WindowToken,
};
use std::path::Path;
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct AcceptedWindow {
    pub token: WindowToken,
    pub window: window_registry::WindowInfo,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuitPhase {
    Preparing,
    Publishing,
    RollForward,
    Committing,
    Aborting,
    Aborted,
    Complete,
}
#[derive(Clone, Debug)]
pub struct QuitProgress {
    pub phase: QuitPhase,
    pub prepared: usize,
    pub acknowledged: usize,
    pub detail: Option<String>,
    pub certificate: Option<GuardedQuitCertificate>,
}
/// Must live on a worker while the GUI services its own typed control queue.
/// Retain this value (and its exclusive lease) through retryable uncertainty.
/// Dropping before completion does not send Abort; crash recovery admission
/// checks the durable certificate and old authenticated/uncertain endpoints.
#[derive(Clone)]
pub struct QuitCancellation(Arc<AtomicU8>);
impl QuitCancellation {
    /// True means cancellation won BEFORE the publication operation began.
    /// False requires retaining coordinator and driving its existing outcome;
    /// a racing disk CAS may already have published. Never infer permission to
    /// Abort from a late cancellation request.
    pub fn request(&self) -> bool {
        match self
            .0
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => true,
            Err(state) => state == 1,
        }
    }
}

pub struct QuitCoordinator {
    store: WindowSessionStore,
    baseline: WindowSessionSnapshot,
    admission: Option<QuitAdmissionLease>,
    windows: Vec<AcceptedWindow>,
    own: usize,
    transaction: String,
    guards: Vec<Option<ipc::QuitGuardStamp>>,
    attempted: Vec<bool>,
    cursor: usize,
    acknowledged: Vec<bool>,
    abort_acknowledged: Vec<bool>,
    phase: QuitPhase,
    certificate: Option<GuardedQuitCertificate>,
    cancel: QuitCancellation,
    detail: Option<String>,
}
impl QuitCoordinator {
    /// Acquire exclusive admission before reading the complete store/cohort.
    /// Supplied bindings must be exact, complete, and freshly authenticated.
    pub fn begin(
        config: &Path,
        own_id: &str,
        supplied: Vec<AcceptedWindow>,
        deadline: Instant,
    ) -> Result<Self, String> {
        let admission = WindowAdmission::new(config)
            .map_err(|e| e.to_string())?
            .acquire_quit(deadline)
            .map_err(|e| e.to_string())?;
        let store = WindowSessionStore::new(admission.configuration());
        let baseline = store.load().map_err(|e| e.to_string())?;
        let live = baseline.live_windows();
        if live.is_empty() || supplied.len() != live.len() {
            return Err("incomplete accepted Quit cohort".into());
        }
        let mut windows = Vec::with_capacity(live.len());
        let mut ids = std::collections::HashSet::new();
        for accepted in live {
            let matches: Vec<_> = supplied
                .iter()
                .filter(|w| w.token == accepted.token && w.window.project == accepted.project)
                .collect();
            if matches.len() != 1 || !ids.insert(matches[0].window.id.clone()) {
                return Err("ambiguous owner/endpoint Quit binding".into());
            }
            windows.push(matches[0].clone());
        }
        let own = windows
            .iter()
            .position(|w| w.window.id == own_id)
            .ok_or("Quit root is not in accepted cohort")?;
        let count = windows.len();
        let value = Self {
            store,
            baseline,
            admission: Some(admission),
            windows,
            own,
            transaction: ipc::new_token().map_err(|e| e.to_string())?,
            guards: vec![None; count],
            attempted: vec![false; count],
            cursor: 0,
            acknowledged: vec![false; count],
            abort_acknowledged: vec![false; count],
            phase: QuitPhase::Preparing,
            certificate: None,
            cancel: QuitCancellation(Arc::new(AtomicU8::new(0))),
            detail: None,
        };
        value.validate_membership(deadline)?;
        Ok(value)
    }
    pub fn cancellation(&self) -> QuitCancellation {
        self.cancel.clone()
    }
    pub fn transaction_id(&self) -> &str {
        &self.transaction
    }
    pub fn progress(&self) -> QuitProgress {
        QuitProgress {
            phase: self.phase,
            prepared: self.guards.iter().filter(|g| g.is_some()).count(),
            acknowledged: self.acknowledged.iter().filter(|a| **a).count(),
            detail: self.detail.clone(),
            certificate: self.certificate.clone(),
        }
    }
    /// One bounded worker iteration. Pending is explicit and needs another
    /// iteration. Cancellation is honored only before publication; afterwards
    /// the exact same transaction must roll forward, never Abort or rebind.
    pub fn advance(&mut self, deadline: Instant) -> QuitProgress {
        self.advance_with(deadline, ipc::control_window)
    }
    fn request(
        &self,
        index: usize,
        control: ipc::QuitControl,
        deadline: Instant,
    ) -> ipc::ControlRequest {
        let member = &self.windows[index];
        ipc::ControlRequest {
            protocol: ipc::PROTOCOL,
            token: member.window.endpoint.token.clone(),
            timeout_ms: deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .clamp(1, 30000) as u64,
            expected_window: window_registry::ExpectedWindow {
                id: member.window.id.clone(),
                project: member.window.project.clone(),
            },
            transaction_id: self.transaction.clone(),
            expected_state: ipc::QuitStateStamp {
                owner: member.token.owner().as_str().into(),
                generation: member.token.generation().value(),
                owner_revision: member.token.revision(),
            },
            quit_control: control,
        }
    }
    fn validate_membership(&self, deadline: Instant) -> Result<(), String> {
        self.admission
            .as_ref()
            .ok_or("Quit admission released")?
            .validate_identity()
            .map_err(|e| e.to_string())?;
        let now = self.store.load().map_err(|e| e.to_string())?;
        if now.revision() != self.baseline.revision()
            || now.generation() != self.baseline.generation()
            || now.live_windows() != self.baseline.live_windows()
        {
            return Err("accepted Quit cohort changed".into());
        }
        let presence = window_registry::presence_until(
            self.admission.as_ref().unwrap().configuration(),
            deadline,
        )
        .map_err(|e| e.to_string())?;
        if !presence.uncertain.is_empty() || presence.live.len() != self.windows.len() {
            return Err("complete Quit endpoint cohort is uncertain or changed".into());
        }
        for member in &self.windows {
            if !presence.live.iter().any(|w| {
                w.id == member.window.id
                    && w.project == member.window.project
                    && w.endpoint.port == member.window.endpoint.port
                    && ipc::same_secret(&w.endpoint.token, &member.window.endpoint.token)
            }) {
                return Err("Quit endpoint binding changed".into());
            }
        }
        if Instant::now() >= deadline {
            return Err("Quit deadline expired".into());
        }
        Ok(())
    }
    fn binding(&self) -> GuardedQuitBinding {
        GuardedQuitBinding {
            transaction_id: self.transaction.clone(),
            cohort: self
                .windows
                .iter()
                .zip(&self.guards)
                .map(|(w, g)| {
                    let g = g
                        .as_ref()
                        .expect("publication requires all prepared guards");
                    GuardedQuitWindow {
                        owner: w.token.owner().clone(),
                        generation: w.token.generation(),
                        owner_revision: w.token.revision(),
                        window_id: w.window.id.clone(),
                        project: w.window.project.clone(),
                        guard: QuitUiGuard {
                            nonce: g.nonce.clone(),
                            state: g.state.clone(),
                        },
                    }
                })
                .collect(),
        }
    }
    fn wire_certificate(&self) -> ipc::QuitCommitCertificate {
        let c = self
            .certificate
            .as_ref()
            .expect("Commit requires published certificate");
        ipc::QuitCommitCertificate {
            transaction_id: c.transaction_id.clone(),
            checkpoint_generation: c.checkpoint_generation.value(),
            checkpoint_revision: c.checkpoint_revision.clone(),
            cohort: c
                .cohort
                .iter()
                .map(|w| ipc::QuitPreparedWindow {
                    window: window_registry::ExpectedWindow {
                        id: w.window_id.clone(),
                        project: w.project.clone(),
                    },
                    state: ipc::QuitStateStamp {
                        owner: w.owner.as_str().into(),
                        generation: w.generation.value(),
                        owner_revision: w.owner_revision,
                    },
                    guard: ipc::QuitGuardStamp {
                        nonce: w.guard.nonce.clone(),
                        state: w.guard.state.clone(),
                    },
                })
                .collect(),
        }
    }
    fn abort(&mut self, detail: String) {
        self.phase = QuitPhase::Aborting;
        self.detail = Some(detail);
    }
    fn advance_with<F>(&mut self, deadline: Instant, mut send: F) -> QuitProgress
    where
        F: FnMut(
            &window_registry::WindowInfo,
            &ipc::ControlRequest,
            Duration,
        ) -> Result<ipc::ControlResponse, ipc::ForwardError>,
    {
        if matches!(self.phase, QuitPhase::Aborted | QuitPhase::Complete) {
            return self.progress();
        }
        if self.cancel.0.load(Ordering::Acquire) == 1 && self.certificate.is_none() {
            self.abort("Quit cancelled before publication".into());
        }
        if Instant::now() >= deadline {
            self.detail =
                Some("Quit iteration deadline expired; retain coordinator and retry".into());
            return self.progress();
        }
        match self.phase {
            QuitPhase::Preparing => {
                if let Err(e) = self.validate_membership(deadline) {
                    self.abort(e);
                    return self.progress();
                }
                for offset in 0..self.windows.len() {
                    let index = (self.cursor + offset) % self.windows.len();
                    if self.guards[index].is_some() {
                        continue;
                    }
                    let control = if self.attempted[index] {
                        ipc::QuitControl::PollPrepared
                    } else {
                        ipc::QuitControl::Prepare
                    };
                    self.attempted[index] = true;
                    self.cursor = (index + 1) % self.windows.len();
                    let request = self.request(index, control, deadline);
                    match send(
                        &self.windows[index].window,
                        &request,
                        deadline.saturating_duration_since(Instant::now()),
                    ) {
                        Ok(response) if response.transaction_id == self.transaction => {
                            match response.status {
                                ipc::ControlStatus::Prepared { guard } => {
                                    self.guards[index] = Some(guard);
                                }
                                ipc::ControlStatus::Pending => {}
                                ipc::ControlStatus::Rejected { message } => self.abort(message),
                                _ => self.abort("invalid Prepare reply".into()),
                            }
                        }
                        Ok(_) => self.abort("wrong Prepare transaction".into()),
                        Err(e) => self.abort(e.to_string()),
                    }
                    break;
                }
                if self.phase == QuitPhase::Preparing && self.guards.iter().all(Option::is_some) {
                    self.phase = QuitPhase::Publishing;
                }
            }
            QuitPhase::Publishing => {
                if let Err(e) = self.validate_membership(deadline) {
                    self.abort(e);
                    return self.progress();
                }
                // Revalidate every frozen UI guard, including root, immediately
                // before CAS. Participant must reject any changed actual state.
                for index in 0..self.windows.len() {
                    let request = self.request(index, ipc::QuitControl::PollPrepared, deadline);
                    match send(
                        &self.windows[index].window,
                        &request,
                        deadline.saturating_duration_since(Instant::now()),
                    ) {
                        Ok(response)
                            if response.transaction_id == self.transaction
                                && matches!(response.status,
                            ipc::ControlStatus::Prepared{ref guard} if Some(guard)==self.guards[index].as_ref()) =>
                            {}
                        Ok(_) => {
                            self.abort("prepared UI guard changed or refused".into());
                            return self.progress();
                        }
                        Err(e) => {
                            self.abort(e.to_string());
                            return self.progress();
                        }
                    }
                }
                if self.cancel.0.load(Ordering::Acquire) == 1 {
                    self.abort("Quit cancelled before publication".into());
                    return self.progress();
                }
                if Instant::now() >= deadline {
                    self.detail = Some("publication deferred at deadline".into());
                    return self.progress();
                }
                if let Err(e) = self.validate_membership(deadline) {
                    self.abort(e);
                    return self.progress();
                }
                self.publish();
            }
            QuitPhase::RollForward => self.publish(),
            QuitPhase::Committing => {
                if let Err(e) = self.admission.as_ref().unwrap().validate_identity() {
                    self.detail = Some(e.to_string());
                    return self.progress();
                }
                let index = (0..self.windows.len())
                    .find(|&i| i != self.own && !self.acknowledged[i])
                    .or_else(|| (!self.acknowledged[self.own]).then_some(self.own));
                if let Some(index) = index {
                    let stored = self.certificate.as_ref().unwrap();
                    if let Err(error) = self
                        .store
                        .verify_guarded_quit(stored, &stored.cohort[index])
                    {
                        self.detail = Some(error.to_string());
                        return self.progress();
                    }
                    let certificate = self.wire_certificate();
                    let request = self.request(
                        index,
                        ipc::QuitControl::Commit {
                            guard: self.guards[index].clone().unwrap(),
                            certificate: certificate.clone(),
                        },
                        deadline,
                    );
                    match send(
                        &self.windows[index].window,
                        &request,
                        deadline.saturating_duration_since(Instant::now()),
                    ) {
                        Ok(response)
                            if response.transaction_id == self.transaction
                                && matches!(response.status,
                            ipc::ControlStatus::Committed{certificate:ref ack} if ack==&certificate) =>
                        {
                            self.acknowledged[index] = true;
                            self.detail = None;
                        }
                        Ok(_) => {
                            self.detail = Some(
                                "Commit not acknowledged; retain exact binding and retry".into(),
                            )
                        }
                        Err(e) => self.detail = Some(e.to_string()),
                    }
                }
                if self.acknowledged.iter().all(|a| *a) {
                    self.phase = QuitPhase::Complete;
                    self.admission.take();
                }
            }
            QuitPhase::Aborting => {
                // Abort every participant: a lost Prepare response may conceal
                // a queued or prepared guard. Failures retain lease for retry.
                for index in 0..self.windows.len() {
                    if self.abort_acknowledged[index] || !self.attempted[index] {
                        continue;
                    }
                    let request = self.request(index, ipc::QuitControl::Abort, deadline);
                    match send(
                        &self.windows[index].window,
                        &request,
                        deadline.saturating_duration_since(Instant::now()),
                    ) {
                        Ok(response)
                            if response.transaction_id == self.transaction
                                && matches!(response.status, ipc::ControlStatus::Aborted) =>
                        {
                            self.abort_acknowledged[index] = true
                        }
                        Ok(_) => {
                            self.detail = Some("Abort not acknowledged; retry required".into())
                        }
                        Err(e) => self.detail = Some(e.to_string()),
                    }
                    break;
                }
                if self
                    .abort_acknowledged
                    .iter()
                    .zip(&self.attempted)
                    .all(|(ack, attempted)| *ack || !*attempted)
                {
                    self.phase = QuitPhase::Aborted;
                    self.admission.take();
                }
            }
            QuitPhase::Aborted | QuitPhase::Complete => {}
        }
        self.progress()
    }
    fn publish(&mut self) {
        if let Err(error) = self.admission.as_ref().unwrap().validate_identity() {
            if self.certificate.is_some() {
                self.detail = Some(error.to_string());
            } else {
                self.abort(error.to_string());
            }
            return;
        }
        // Recreate the SAME immutable plan from captured baseline; QuitPlan is
        // consumed by Store API. Never use a newly loaded baseline for retry.
        let plan = match self.store.prepare_quit(&self.baseline) {
            Ok(plan) => plan,
            Err(e) => {
                if self.certificate.is_some() {
                    self.detail = Some(e.to_string());
                } else {
                    self.abort(e.to_string());
                }
                return;
            }
        };
        if self.certificate.is_none()
            && self
                .cancel
                .0
                .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            self.abort("Quit cancelled before publication".into());
            return;
        }
        match self.store.commit_guarded_quit(plan, self.binding()) {
            Ok(certificate) => {
                self.certificate = Some(certificate);
                self.phase = QuitPhase::Committing;
                self.detail = None;
            }
            Err(WindowSessionError::QuitCommitUncertain { certificate, error }) => {
                self.certificate = Some(certificate);
                self.phase = QuitPhase::RollForward;
                self.detail = Some(error.to_string());
            }
            Err(e) => {
                if self.certificate.is_some() {
                    self.phase = QuitPhase::RollForward;
                    self.detail = Some(e.to_string());
                } else {
                    self.abort(e.to_string());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Mutex};
    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(3)
    }
    fn guard() -> ipc::QuitGuardStamp {
        ipc::QuitGuardStamp {
            nonce: "fixture-nonce".into(),
            state: "fixture-frozen-state".into(),
        }
    }
    fn reply(
        request: &ipc::ControlRequest,
        status: ipc::ControlStatus,
    ) -> Result<ipc::ControlResponse, ipc::ForwardError> {
        Ok(ipc::ControlResponse {
            transaction_id: request.transaction_id.clone(),
            status,
        })
    }
    // Real registry endpoints and OS ownership; UI responses in advance_with
    // tests are explicitly protocol mocks, not actual document save approval.
    struct Fixture {
        config: tempfile::TempDir,
        windows: Vec<AcceptedWindow>,
        _servers: Vec<ipc::Server>,
    }
    impl Fixture {
        fn new(count: usize) -> Self {
            let config = tempfile::tempdir().unwrap();
            let mut servers = Vec::new();
            let mut windows = Vec::new();
            let store = WindowSessionStore::new(config.path());
            for _ in 0..count {
                let server = ipc::Server::start_secondary(config.path()).unwrap();
                let window = window_registry::presence_until(config.path(), deadline())
                    .unwrap()
                    .live
                    .into_iter()
                    .find(|w| w.id == server.window_id())
                    .unwrap();
                let token = store
                    .accept_window(
                        &store.load().unwrap(),
                        rustpy_settings::window_session_store::WindowOwnerId::new(),
                        None,
                    )
                    .unwrap()
                    .token;
                windows.push(AcceptedWindow { token, window });
                servers.push(server);
            }
            Self {
                config,
                windows,
                _servers: servers,
            }
        }
        fn coordinator(&self) -> QuitCoordinator {
            QuitCoordinator::begin(
                self.config.path(),
                &self.windows[0].window.id,
                self.windows.clone(),
                deadline(),
            )
            .unwrap()
        }
    }
    #[test]
    fn explicit_pending_then_refusal_aborts_without_checkpoint() {
        let fixture = Fixture::new(1);
        let mut quit = fixture.coordinator();
        assert_eq!(
            quit.advance_with(deadline(), |_, r, _| reply(r, ipc::ControlStatus::Pending))
                .phase,
            QuitPhase::Preparing
        );
        let progress = quit.advance_with(deadline(), |_, r, _| {
            assert_eq!(r.quit_control, ipc::QuitControl::PollPrepared);
            reply(
                r,
                ipc::ControlStatus::Rejected {
                    message: "user cancelled".into(),
                },
            )
        });
        assert_eq!(progress.phase, QuitPhase::Aborting);
        assert_eq!(
            quit.advance_with(deadline(), |_, r, _| {
                assert_eq!(r.quit_control, ipc::QuitControl::Abort);
                reply(r, ipc::ControlStatus::Aborted)
            })
            .phase,
            QuitPhase::Aborted
        );
        assert!(quit.store.load().unwrap().checkpoint().is_none());
    }
    #[test]
    fn cancellation_aborts_lost_prepare_reply_and_keeps_lease_until_ack() {
        let fixture = Fixture::new(1);
        let mut quit = fixture.coordinator();
        quit.advance_with(deadline(), |_, _, _| {
            Err(ipc::ForwardError::Unreachable("lost response".into()))
        });
        assert_eq!(quit.phase, QuitPhase::Aborting);
        assert!(quit.cancellation().request());
        assert!(WindowAdmission::new(fixture.config.path())
            .unwrap()
            .acquire_shared(Instant::now() + Duration::from_millis(15))
            .is_err());
        quit.advance_with(deadline(), |_, r, _| reply(r, ipc::ControlStatus::Pending));
        assert_eq!(quit.phase, QuitPhase::Aborting);
        quit.advance_with(deadline(), |_, r, _| reply(r, ipc::ControlStatus::Aborted));
        assert_eq!(quit.phase, QuitPhase::Aborted);
        WindowAdmission::new(fixture.config.path())
            .unwrap()
            .acquire_shared(deadline())
            .unwrap();
    }
    #[test]
    fn cancellation_winning_before_publication_aborts_prepared_guard() {
        let fixture = Fixture::new(1);
        let mut quit = fixture.coordinator();
        quit.advance_with(deadline(), |_, r, _| {
            reply(r, ipc::ControlStatus::Prepared { guard: guard() })
        });
        assert_eq!(quit.phase, QuitPhase::Publishing);
        assert!(quit.cancellation().request());
        quit.advance_with(deadline(), |_, r, _| {
            assert_eq!(r.quit_control, ipc::QuitControl::Abort);
            reply(r, ipc::ControlStatus::Aborted)
        });
        assert_eq!(quit.phase, QuitPhase::Aborted);
        assert!(quit.store.load().unwrap().checkpoint().is_none());
    }

    #[test]
    fn complete_cohort_and_exact_guard_are_required_before_cas() {
        let fixture = Fixture::new(2);
        assert!(QuitCoordinator::begin(
            fixture.config.path(),
            &fixture.windows[0].window.id,
            vec![fixture.windows[0].clone()],
            deadline()
        )
        .is_err());
        let mut quit = fixture.coordinator();
        for _ in 0..2 {
            quit.advance_with(deadline(), |_, r, _| {
                reply(r, ipc::ControlStatus::Prepared { guard: guard() })
            });
        }
        let mut changed = guard();
        changed.state = "edited-after-approval".into();
        quit.advance_with(deadline(), |_, r, _| {
            reply(
                r,
                ipc::ControlStatus::Prepared {
                    guard: changed.clone(),
                },
            )
        });
        assert_eq!(quit.phase, QuitPhase::Aborting);
        assert!(quit.store.load().unwrap().checkpoint().is_none());
    }
    #[test]
    fn stale_membership_cannot_publish_checkpoint() {
        let fixture = Fixture::new(1);
        let mut quit = fixture.coordinator();
        quit.advance_with(deadline(), |_, r, _| {
            reply(r, ipc::ControlStatus::Prepared { guard: guard() })
        });
        quit.store.close_window(&fixture.windows[0].token).unwrap();
        quit.advance_with(deadline(), |_, _, _| {
            panic!("stale cohort must fail before guard poll")
        });
        assert_eq!(quit.phase, QuitPhase::Aborting);
        assert!(quit.store.load().unwrap().checkpoint().is_none());
    }
    #[test]
    fn durable_reconciliation_uses_same_binding_never_abort_and_root_commits_last() {
        let fixture = Fixture::new(2);
        let mut quit = fixture.coordinator();
        for _ in 0..2 {
            quit.advance_with(deadline(), |_, r, _| {
                reply(r, ipc::ControlStatus::Prepared { guard: guard() })
            });
        }
        // Simulated post-publication uncertainty: actually publish exact Store
        // CAS, then retain its receipt in RollForward as if directory sync failed.
        let certificate = quit
            .store
            .commit_guarded_quit(
                quit.store.prepare_quit(&quit.baseline).unwrap(),
                quit.binding(),
            )
            .unwrap();
        let bytes = std::fs::read(quit.store.path()).unwrap();
        quit.certificate = Some(certificate.clone());
        quit.phase = QuitPhase::RollForward;

        // The artificial uncertain setup bypassed publication latch: establish
        // the real durable state. Production publish sets this before disk CAS.
        quit.cancel.0.store(2, Ordering::Release);
        assert!(!quit.cancellation().request());
        quit.advance_with(deadline(), |_, _, _| {
            panic!("reconcile does not send control")
        });
        assert_eq!(quit.phase, QuitPhase::Committing);
        assert_eq!(quit.certificate.as_ref(), Some(&certificate));
        assert_eq!(std::fs::read(quit.store.path()).unwrap(), bytes);
        let mut order = Vec::new();
        quit.advance_with(deadline(), |w, r, _| {
            order.push(w.id.clone());
            assert!(matches!(r.quit_control, ipc::QuitControl::Commit { .. }));
            Err(ipc::ForwardError::Unreachable("lost Commit ACK".into()))
        });
        assert_eq!(quit.phase, QuitPhase::Committing);
        assert!(!quit.cancellation().request());
        assert!(quit.admission.is_some());
        for _ in 0..2 {
            quit.advance_with(deadline(), |w, r, _| {
                order.push(w.id.clone());
                let ipc::QuitControl::Commit { certificate, .. } = &r.quit_control else {
                    panic!("durable state cannot Abort")
                };
                reply(
                    r,
                    ipc::ControlStatus::Committed {
                        certificate: certificate.clone(),
                    },
                )
            });
        }
        assert_eq!(
            order,
            vec![
                fixture.windows[1].window.id.clone(),
                fixture.windows[1].window.id.clone(),
                fixture.windows[0].window.id.clone()
            ]
        );
        assert_eq!(quit.phase, QuitPhase::Complete);
        assert!(quit.admission.is_none());
    }
    struct Workers {
        stop: Arc<AtomicU8>,
        joins: Vec<std::thread::JoinHandle<()>>,
    }
    impl Drop for Workers {
        fn drop(&mut self) {
            self.stop.store(1, Ordering::Release);
            for join in self.joins.drain(..) {
                let _ = join.join();
            }
        }
    }
    #[test]
    fn actual_tcp_prepared_verification_commit_and_root_last() {
        let config = tempfile::tempdir().unwrap();
        let stop = Arc::new(AtomicU8::new(0));
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut workers = Workers {
            stop: stop.clone(),
            joins: Vec::new(),
        };
        let mut windows = Vec::new();
        let store = WindowSessionStore::new(config.path());
        for _ in 0..2 {
            let path = config.path().to_path_buf();
            let stop = stop.clone();
            let events = events.clone();
            let (ready_rx_tx, ready_rx) = mpsc::channel();
            let (token_tx, token_rx) = mpsc::channel::<WindowToken>();
            workers.joins.push(std::thread::spawn(move || {
                let server = ipc::Server::start_secondary(&path).unwrap();
                ready_rx_tx.send(server.window_id()).unwrap();
                let token = token_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                let expected = window_registry::ExpectedWindow {
                    id: server.window_id(),
                    project: None,
                };
                let state = ipc::QuitStateStamp {
                    owner: token.owner().as_str().into(),
                    generation: token.generation().value(),
                    owner_revision: token.revision(),
                };
                let mut session: Option<ipc::QuitControlSession> = None;
                let worker_deadline = Instant::now() + Duration::from_secs(15);
                while stop.load(Ordering::Acquire) == 0 && Instant::now() < worker_deadline {
                    let Some(incoming) = server.recv_control_timeout(Duration::from_millis(10))
                    else {
                        continue;
                    };
                    incoming.validate_consumption(&expected, &state).unwrap();
                    // This is a simulated UI participant over real production
                    // TCP/authentication, not the real IdeApp save guard.
                    match &incoming.request.quit_control {
                        ipc::QuitControl::Prepare => {
                            let mut started =
                                ipc::QuitControlSession::begin(&incoming.request).unwrap();
                            started.approve(guard()).unwrap();
                            session = Some(started);
                        }
                        ipc::QuitControl::PollPrepared => {}
                        ipc::QuitControl::Abort => {
                            session.as_mut().unwrap().abort(&incoming.request).unwrap()
                        }
                        ipc::QuitControl::Commit { certificate, .. } => {
                            let stored = WindowSessionStore::new(&path).load().unwrap();
                            let receipt = stored.guarded_quit_certificate().unwrap();
                            assert_eq!(receipt.transaction_id, certificate.transaction_id);
                            assert_eq!(
                                receipt.checkpoint_revision,
                                certificate.checkpoint_revision
                            );
                            assert_eq!(
                                receipt.checkpoint_generation.value(),
                                certificate.checkpoint_generation
                            );
                            assert_eq!(receipt.cohort.len(), certificate.cohort.len());
                            for (typed, wire) in receipt.cohort.iter().zip(&certificate.cohort) {
                                assert_eq!(typed.window_id, wire.window.id);
                                assert_eq!(typed.project, wire.window.project);
                                assert_eq!(typed.owner.as_str(), wire.state.owner);
                                assert_eq!(typed.generation.value(), wire.state.generation);
                                assert_eq!(typed.owner_revision, wire.state.owner_revision);
                                assert_eq!(typed.guard.nonce, wire.guard.nonce);
                                assert_eq!(typed.guard.state, wire.guard.state);
                            }
                            let local = receipt
                                .cohort
                                .iter()
                                .find(|w| w.window_id == expected.id)
                                .unwrap();
                            WindowSessionStore::new(&path)
                                .verify_guarded_quit(receipt, local)
                                .unwrap();
                            session
                                .as_mut()
                                .unwrap()
                                .commit_after_checkpoint(&incoming.request)
                                .unwrap();
                            events.lock().unwrap().push(expected.id.clone());
                        }
                    }
                    incoming
                        .respond(session.as_ref().unwrap().status())
                        .unwrap();
                }
            }));
            let id = ready_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            let window = window_registry::presence_until(config.path(), deadline())
                .unwrap()
                .live
                .into_iter()
                .find(|w| w.id == id)
                .unwrap();
            let token = store
                .accept_window(
                    &store.load().unwrap(),
                    rustpy_settings::window_session_store::WindowOwnerId::new(),
                    None,
                )
                .unwrap()
                .token;
            token_tx.send(token.clone()).unwrap();
            windows.push(AcceptedWindow { token, window });
        }
        let result = (|| {
            let mut quit = QuitCoordinator::begin(
                config.path(),
                &windows[0].window.id,
                windows.clone(),
                deadline(),
            )?;
            for _ in 0..12 {
                if quit.advance(deadline()).phase == QuitPhase::Complete {
                    return Ok(());
                }
            }
            Err(format!("Quit did not complete: {:?}", quit.progress()))
        })();
        stop.store(1, Ordering::Release);
        for join in workers.joins.drain(..) {
            join.join().unwrap();
        }
        result.unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            vec![windows[1].window.id.clone(), windows[0].window.id.clone()]
        );
    }
}
