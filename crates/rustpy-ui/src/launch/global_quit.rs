//! Managed UI participant and worker controller for application-wide Quit.
//! Root must retain this controller while active, service controls before app
//! logic, block OPEN/ownership changes, and defer physical close until ready.
//! UI callbacks privately verify full durable bindings; queue ACK is not exit.
use crate::app::{
    window_lifecycle::{QuitGuard, QuitProgress as AppProgress},
    IdeApp,
};
use rustpy_cli::{
    ipc,
    quit_coordinator::{
        AcceptedWindow, QuitCancellation, QuitCoordinator, QuitPhase, QuitProgress,
    },
    window_registry,
};
use rustpy_settings::window_session_store::{
    GuardedQuitCertificate, GuardedQuitWindow, WindowSessionStore, WindowToken,
};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Time each window has to answer one control. Windows answer from their UI
/// frame, which can take well over half a second while busy (indexing, software
/// rendering); a shorter budget aborted every Quit before its prompt showed.
const CONTROL_BUDGET: Duration = Duration::from_secs(10);
struct Participant {
    ledger: ipc::QuitControlSession,
}
enum WorkerEvent {
    Started {
        transaction: String,
        cancel: QuitCancellation,
    },
    Progress(QuitProgress),
    Failed(String),
}
struct Worker {
    events: mpsc::Receiver<WorkerEvent>,
    stop: Arc<AtomicBool>,
    cancel_requested: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    transaction: Option<String>,
    cancel: Option<QuitCancellation>,
    finished: bool,
}
impl Drop for Worker {
    fn drop(&mut self) {
        // Process teardown must not join a worker waiting for this GUI's TCP
        // response. Signal bounded shutdown; normal live UI must retain active
        // ownership, not discard an unresolved transaction. Never synthesize
        // Abort on teardown after an uncertain publication.
        self.stop.store(true, Ordering::Release);
        if self.join.as_ref().is_some_and(JoinHandle::is_finished) {
            let _ = self.join.take().unwrap().join();
        }
    }
}
pub(super) struct GlobalQuit {
    config: PathBuf,
    worker: Option<Worker>,
    participant: Option<Participant>,
    progress: Option<QuitProgress>,
    error: Option<String>,
    committed: bool,
    delivered: bool,
    deliveries: Vec<ipc::ControlDelivery>,
    /// A first Prepare waits one frame so the window captures its ordinary
    /// contents as the backdrop of the Quit prompt.
    deferred: Vec<ipc::IncomingControl>,
    deferred_frames: u8,
    backdrop_for: Option<String>,
}
impl Default for GlobalQuit {
    fn default() -> Self {
        Self {
            config: rustpy_settings::Settings::config_dir(),
            worker: None,
            participant: None,
            progress: None,
            error: None,
            committed: false,
            delivered: false,
            deliveries: Vec::new(),
            deferred: Vec::new(),
            deferred_frames: 0,
            backdrop_for: None,
        }
    }
}
impl GlobalQuit {
    pub(super) fn with_configuration(config: &Path) -> Self {
        Self {
            config: config.to_path_buf(),
            ..Self::default()
        }
    }

    pub(super) fn active(&self) -> bool {
        self.worker.as_ref().is_some_and(|w| !w.finished)
            || self
                .participant
                .as_ref()
                .is_some_and(|p| !matches!(p.ledger.status(), ipc::ControlStatus::Aborted))
    }
    pub(super) fn committed(&self) -> bool {
        self.committed
    }
    pub(super) fn ready_to_close(&self) -> bool {
        self.committed
            && if self.worker.is_some() {
                self.progress
                    .as_ref()
                    .is_some_and(|p| p.phase == QuitPhase::Complete)
            } else {
                self.delivered
            }
    }
    pub(super) fn progress(&self) -> Option<&QuitProgress> {
        self.progress.as_ref()
    }
    pub(super) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    pub(super) fn request(&mut self, config: &Path, own_id: &str) -> Result<(), String> {
        if self.active() || self.committed {
            return Err("A managed Quit is already active".into());
        }
        self.config = config.to_path_buf();
        self.progress = None;
        self.error = None;
        self.delivered = false;
        let config = config.to_path_buf();
        let own_id = own_id.to_owned();
        let (tx, events) = mpsc::sync_channel(4);
        let stop = Arc::new(AtomicBool::new(false));
        let cancel_requested = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_cancel = cancel_requested.clone();
        let join = std::thread::spawn(move || {
            let result = (|| -> Result<(), String> {
                let deadline = Instant::now() + Duration::from_secs(3);
                let bindings = discover(&config, deadline)?;
                let mut coordinator = QuitCoordinator::begin(&config, &own_id, bindings, deadline)?;
                tx.send(WorkerEvent::Started {
                    transaction: coordinator.transaction_id().into(),
                    cancel: coordinator.cancellation(),
                })
                .map_err(|e| e.to_string())?;
                while !worker_stop.load(Ordering::Acquire) {
                    if worker_cancel.load(Ordering::Acquire) {
                        coordinator.cancellation().request();
                    }
                    let progress = coordinator.advance(Instant::now() + CONTROL_BUDGET);
                    let terminal =
                        matches!(progress.phase, QuitPhase::Complete | QuitPhase::Aborted);
                    tx.send(WorkerEvent::Progress(progress))
                        .map_err(|e| e.to_string())?;
                    if terminal {
                        return Ok(());
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(())
            })();
            if let Err(error) = result {
                let _ = tx.send(WorkerEvent::Failed(error));
            }
        });
        self.worker = Some(Worker {
            events,
            stop,
            cancel_requested,
            join: Some(join),
            transaction: None,
            cancel: None,
            finished: false,
        });
        Ok(())
    }
    /// Request cancellation before publication; a late request retains the
    /// same roll-forward worker and never turns a published Quit into Abort.
    pub(super) fn cancel(&mut self) {
        if let Some(worker) = &self.worker {
            worker.cancel_requested.store(true, Ordering::Release);
            if let Some(cancel) = &worker.cancel {
                cancel.request();
            }
        }
    }

    pub(super) fn drive(
        &mut self,
        app: &mut IdeApp,
        server: &ipc::Server,
        own_token: &WindowToken,
    ) {
        if let Some(worker) = &mut self.worker {
            while let Ok(event) = worker.events.try_recv() {
                match event {
                    WorkerEvent::Started {
                        transaction,
                        cancel,
                    } => {
                        worker.transaction = Some(transaction);
                        worker.cancel = Some(cancel);
                    }
                    WorkerEvent::Progress(progress) => {
                        worker.finished =
                            matches!(progress.phase, QuitPhase::Complete | QuitPhase::Aborted);
                        self.progress = Some(progress);
                    }
                    WorkerEvent::Failed(error) => {
                        worker.finished = true;
                        self.error = Some(error);
                    }
                }
            }
            if worker
                .transaction
                .as_deref()
                .is_some_and(|tx| app.coordinated_quit_cancel_requested(tx))
            {
                worker.cancel_requested.store(true, Ordering::Release);
                if let Some(cancel) = &worker.cancel {
                    cancel.request();
                }
            }
            if worker.finished && worker.join.as_ref().is_some_and(JoinHandle::is_finished) {
                let _ = worker.join.take().unwrap().join();
            }
        }
        self.deliveries
            .retain(|delivery| match delivery.try_result() {
                None => true,
                Some(Ok(())) => {
                    self.delivered = true;
                    false
                }
                Some(Err(error)) => {
                    self.error = Some(error.to_string());
                    false
                }
            });
        // The capture shows the frame after the request; it arrives a frame
        // later still. Hold the Prepare until then (bounded).
        let mut queue = Vec::new();
        if !self.deferred.is_empty() {
            self.deferred_frames = self.deferred_frames.saturating_sub(1);
            if app.has_quit_backdrop() || self.deferred_frames == 0 {
                queue = std::mem::take(&mut self.deferred);
            }
        }
        while let Some(incoming) = server.try_recv_control() {
            let transaction = &incoming.request.transaction_id;
            if matches!(incoming.request.quit_control, ipc::QuitControl::Prepare)
                && !app.coordinated_quit_active()
                && !app.has_quit_backdrop()
                && self.backdrop_for.as_ref() != Some(transaction)
            {
                self.backdrop_for = Some(transaction.clone());
                app.request_quit_backdrop();
                self.deferred.push(incoming);
                self.deferred_frames = 3;
                continue;
            }
            queue.push(incoming);
        }
        for incoming in queue {
            let status = self.control(app, server, own_token, &incoming);
            let committed = matches!(status, ipc::ControlStatus::Committed { .. });
            if committed {
                match incoming.respond_tracked(status) {
                    Ok(delivery) => self.deliveries.push(delivery),
                    Err(error) => self.error = Some(error.to_string()),
                }
            } else if let Err(error) = incoming.respond(status) {
                self.error = Some(error.to_string());
            }
        }
    }
    fn control(
        &mut self,
        app: &mut IdeApp,
        server: &ipc::Server,
        token: &WindowToken,
        incoming: &ipc::IncomingControl,
    ) -> ipc::ControlStatus {
        let request = &incoming.request;
        let expected = window_registry::ExpectedWindow {
            id: server.window_id(),
            project: app.actual_current_project_root().map(Path::to_path_buf),
        };
        let state = ipc::QuitStateStamp {
            owner: token.owner().as_str().into(),
            generation: token.generation().value(),
            owner_revision: token.revision(),
        };
        if let Err(error) = incoming.validate_consumption(&expected, &state) {
            return reject(error.to_string());
        }
        let tx = &request.transaction_id;
        match &request.quit_control {
            ipc::QuitControl::Prepare => {
                if self.participant.as_ref().is_some_and(|p| {
                    p.ledger.matches(request)
                        && matches!(p.ledger.status(), ipc::ControlStatus::Aborted)
                }) {
                    return reject("An aborted transaction cannot be restarted".into());
                }
                if self.worker.as_ref().is_some_and(|w| w.finished)
                    && self
                        .participant
                        .as_ref()
                        .is_none_or(|p| matches!(p.ledger.status(), ipc::ControlStatus::Aborted))
                {
                    self.worker.take();
                    self.progress = None;
                }
                if self.participant.as_ref().is_some_and(|p| {
                    !p.ledger.matches(request)
                        && !matches!(p.ledger.status(), ipc::ControlStatus::Aborted)
                }) {
                    return reject("Another managed Quit owns this window".into());
                }
                if self
                    .participant
                    .as_ref()
                    .is_none_or(|p| !p.ledger.matches(request))
                {
                    match ipc::QuitControlSession::begin(request) {
                        Ok(ledger) => self.participant = Some(Participant { ledger }),
                        Err(error) => return reject(error.to_string()),
                    }
                }
                let progress = app.prepare_managed_coordinated_quit(tx);
                self.approval(app, tx, progress)
            }
            ipc::QuitControl::PollPrepared => {
                if !self
                    .participant
                    .as_ref()
                    .is_some_and(|p| p.ledger.matches(request))
                {
                    return reject("No matching managed Quit".into());
                }
                let progress = app.poll_coordinated_quit(tx);
                self.approval(app, tx, progress)
            }
            ipc::QuitControl::Abort => {
                let store = WindowSessionStore::new(&self.config);
                let snapshot = match store.load() {
                    Ok(snapshot) => snapshot,
                    Err(error) => return reject(error.to_string()),
                };
                if snapshot.generation() != token.generation()
                    || snapshot
                        .guarded_quit_certificate()
                        .is_some_and(|c| c.transaction_id == *tx)
                    || !snapshot
                        .live_windows()
                        .iter()
                        .any(|w| &w.token == token && w.project == expected.project)
                {
                    return reject(
                        "Cannot prove this Quit was not published; keep participant frozen".into(),
                    );
                }
                if self
                    .participant
                    .as_ref()
                    .is_some_and(|p| !p.ledger.matches(request))
                {
                    return reject("Abort transaction does not own participant".into());
                }
                match app.abort_coordinated_quit(tx) {
                    AppProgress::Aborted => {
                        if let Some(participant) = &mut self.participant {
                            if let Err(error) = participant.ledger.abort(request) {
                                return reject(error.to_string());
                            }
                        }
                        ipc::ControlStatus::Aborted
                    }
                    progress => map_progress(progress),
                }
            }
            ipc::QuitControl::Commit { guard, certificate } => {
                let Some(participant) = &mut self.participant else {
                    return reject("No prepared managed Quit".into());
                };
                if !participant.ledger.matches(request) {
                    return reject("Commit transaction does not own participant".into());
                }
                let store = WindowSessionStore::new(&self.config);
                let stored = match checked_certificate(&store, certificate, token, &expected) {
                    Ok(value) => value,
                    Err(error) => return reject(error),
                };
                let local = match stored.cohort.iter().find(|w| w.window_id == expected.id) {
                    Some(value) => value.clone(),
                    None => return reject("Commit omits this window".into()),
                };
                let ui_guard = QuitGuard {
                    nonce: guard.nonce.clone(),
                    state: guard.state.clone(),
                };
                if local.guard.nonce != ui_guard.nonce || local.guard.state != ui_guard.state {
                    return reject("Commit guard differs from stored binding".into());
                }
                if !self.committed {
                    if let Err(error) = app.seal_coordinated_quit(tx, &ui_guard, |_, actual| {
                        verify(&store, &stored, &local, actual)
                    }) {
                        return reject(error);
                    }
                    if let Err(error) = app.commit_coordinated_quit(tx, &ui_guard, |_, actual| {
                        verify(&store, &stored, &local, actual)
                    }) {
                        return reject(error);
                    }
                } else if let Err(error) = verify(&store, &stored, &local, &ui_guard) {
                    return reject(error);
                }
                if let Err(error) = participant.ledger.commit_after_checkpoint(request) {
                    return reject(error.to_string());
                }
                self.committed = true;
                participant.ledger.status()
            }
        }
    }
    fn approval(&mut self, app: &IdeApp, tx: &str, progress: AppProgress) -> ipc::ControlStatus {
        if app.coordinated_quit_cancel_requested(tx) {
            return reject("User cancelled managed Quit; waiting for coordinator Abort".into());
        }
        match progress {
            AppProgress::Prepared { guard } => {
                let participant = self.participant.as_mut().unwrap();
                if let Err(error) = participant.ledger.approve(ipc::QuitGuardStamp {
                    nonce: guard.nonce,
                    state: guard.state,
                }) {
                    return reject(error.to_string());
                }
                participant.ledger.status()
            }
            progress => map_progress(progress),
        }
    }
}
fn reject(message: String) -> ipc::ControlStatus {
    ipc::ControlStatus::Rejected { message }
}
fn map_progress(progress: AppProgress) -> ipc::ControlStatus {
    match progress {
        AppProgress::PendingPersistence { .. }
        | AppProgress::PendingProcesses { .. }
        | AppProgress::PendingDocument { .. } => ipc::ControlStatus::Pending,
        AppProgress::Rejected { message } => reject(message),
        AppProgress::Aborted => ipc::ControlStatus::Aborted,
        AppProgress::Prepared { guard } => ipc::ControlStatus::Prepared {
            guard: ipc::QuitGuardStamp {
                nonce: guard.nonce,
                state: guard.state,
            },
        },
        AppProgress::Committed => reject("Commit requires verified certificate".into()),
    }
}
/// Every registered endpoint with its authenticated owner stamp. Any
/// uncertain or unanswered endpoint fails the whole observation.
fn authenticated_presence(
    config: &Path,
    deadline: Instant,
) -> Result<Vec<(window_registry::WindowInfo, ipc::QuitStateStamp)>, String> {
    let presence = window_registry::presence_until(config, deadline).map_err(|e| e.to_string())?;
    if !presence.uncertain.is_empty() {
        return Err("Quit endpoint cohort is uncertain".into());
    }
    presence
        .live
        .into_iter()
        .map(|window| {
            let stamp = ipc::query_window_owner(
                &window,
                deadline.saturating_duration_since(Instant::now()),
            )
            .map_err(|e| e.to_string())?;
            Ok((window, stamp))
        })
        .collect()
}
/// Tombstone accepted owners left behind by killed or crashed windows. An
/// owner is accepted only after its endpoint holds a registry lease, so with
/// every occupied registry entry authenticated (re-checked under the store
/// lock) an owner no endpoint reports belongs to a dead process.
fn retire_dead_owners(
    config: &Path,
    observed: &[(window_registry::WindowInfo, ipc::QuitStateStamp)],
) -> Result<usize, String> {
    let ids: std::collections::BTreeSet<_> = observed.iter().map(|(w, _)| w.id.clone()).collect();
    let owners: std::collections::BTreeSet<_> =
        observed.iter().map(|(_, s)| s.owner.as_str()).collect();
    let store = WindowSessionStore::new(config);
    let live = store.load().map_err(|e| e.to_string())?.live_windows();
    // Only a coherent view retires anything: every endpoint must report a
    // distinct, exactly current owner (a stale stamp proves nothing).
    let coherent = owners.len() == observed.len()
        && observed.iter().all(|(_, stamp)| {
            live.iter().any(|w| {
                w.token.owner().as_str() == stamp.owner
                    && w.token.generation().value() == stamp.generation
                    && w.token.revision() == stamp.owner_revision
            })
        });
    if !coherent {
        return Ok(0);
    }
    store
        .retire_unowned(
            |owner| owners.contains(owner.as_str()),
            || window_registry::occupied_ids(config).is_ok_and(|now| now == ids),
        )
        .map(|retired| retired.len())
        .map_err(|e| e.to_string())
}
/// Startup and pre-Quit cleanup of owners whose process no longer exists.
pub(super) fn prune_dead_owners(config: &Path, deadline: Instant) -> Result<usize, String> {
    retire_dead_owners(config, &authenticated_presence(config, deadline)?)
}
fn discover(config: &Path, deadline: Instant) -> Result<Vec<AcceptedWindow>, String> {
    let observed = authenticated_presence(config, deadline)?;
    retire_dead_owners(config, &observed)?;
    let store = WindowSessionStore::new(config);
    let snapshot = store.load().map_err(|e| e.to_string())?;
    let live = snapshot.live_windows();
    if live.len() != observed.len() {
        return Err("Quit registry and accepted cohort differ".into());
    }
    let mut result = Vec::new();
    let mut owners = std::collections::BTreeSet::new();
    for (window, stamp) in observed {
        let member = live
            .iter()
            .find(|w| {
                w.token.owner().as_str() == stamp.owner
                    && w.token.generation().value() == stamp.generation
                    && w.token.revision() == stamp.owner_revision
                    && w.project == window.project
            })
            .ok_or("Authenticated window has no matching accepted owner")?;
        if !owners.insert(member.token.owner().clone()) {
            return Err("Quit endpoint owner is duplicated".into());
        }
        result.push(AcceptedWindow {
            token: member.token.clone(),
            window,
        });
    }
    Ok(result)
}
fn checked_certificate(
    store: &WindowSessionStore,
    wire: &ipc::QuitCommitCertificate,
    token: &WindowToken,
    expected: &window_registry::ExpectedWindow,
) -> Result<GuardedQuitCertificate, String> {
    let snapshot = store.load().map_err(|e| e.to_string())?;
    let stored = snapshot
        .guarded_quit_certificate()
        .ok_or("No durable guarded Quit certificate")?;
    if stored.transaction_id != wire.transaction_id
        || stored.checkpoint_generation.value() != wire.checkpoint_generation
        || stored.checkpoint_revision != wire.checkpoint_revision
        || stored.cohort.len() != wire.cohort.len()
    {
        return Err("Commit certificate differs from stored receipt".into());
    }
    for (typed, wire) in stored.cohort.iter().zip(&wire.cohort) {
        if typed.window_id != wire.window.id
            || typed.project != wire.window.project
            || typed.owner.as_str() != wire.state.owner
            || typed.generation.value() != wire.state.generation
            || typed.owner_revision != wire.state.owner_revision
            || typed.guard.nonce != wire.guard.nonce
            || typed.guard.state != wire.guard.state
        {
            return Err("Full Commit cohort differs from stored binding".into());
        }
    }
    let local = stored
        .cohort
        .iter()
        .find(|w| {
            w.window_id == expected.id
                && w.project == expected.project
                && &w.owner == token.owner()
                && w.generation == token.generation()
                && w.owner_revision == token.revision()
        })
        .ok_or("Commit does not bind current owner/window")?;
    store
        .verify_guarded_quit(stored, local)
        .map_err(|e| e.to_string())?;
    Ok(stored.clone())
}
fn verify(
    store: &WindowSessionStore,
    certificate: &GuardedQuitCertificate,
    local: &GuardedQuitWindow,
    guard: &QuitGuard,
) -> Result<(), String> {
    if local.guard.nonce != guard.nonce || local.guard.state != guard.state {
        return Err("Actual UI guard differs from durable binding".into());
    }
    store
        .verify_guarded_quit(certificate, local)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpy_settings::window_session_store::{GuardedQuitBinding, QuitUiGuard, WindowOwnerId};
    struct Fixture {
        config: tempfile::TempDir,
        app: IdeApp,
        server: ipc::Server,
        token: WindowToken,
        adapter: GlobalQuit,
        window: window_registry::WindowInfo,
    }
    impl Fixture {
        fn new() -> Self {
            let config = tempfile::tempdir().unwrap();
            let app = IdeApp::new_welcome(rustpy_settings::Settings::default());
            let server = ipc::Server::start_secondary(config.path()).unwrap();
            let store = WindowSessionStore::new(config.path());
            let token = store
                .accept_window(&store.load().unwrap(), WindowOwnerId::new(), None)
                .unwrap()
                .token;
            server.set_quit_state(Some(stamp(&token))).unwrap();
            let window = window_registry::presence_until(
                config.path(),
                Instant::now() + Duration::from_secs(3),
            )
            .unwrap()
            .live
            .pop()
            .unwrap();
            let adapter = GlobalQuit::with_configuration(config.path());
            Self {
                config,
                app,
                server,
                token,
                adapter,
                window,
            }
        }
        fn request(&self, phase: ipc::QuitControl) -> ipc::ControlRequest {
            ipc::ControlRequest {
                protocol: ipc::PROTOCOL,
                token: self.window.endpoint.token.clone(),
                timeout_ms: 2000,
                expected_window: window_registry::ExpectedWindow {
                    id: self.window.id.clone(),
                    project: None,
                },
                transaction_id: "actual-app-quit".into(),
                expected_state: stamp(&self.token),
                quit_control: phase,
            }
        }
        fn exchange(
            &mut self,
            request: ipc::ControlRequest,
        ) -> Result<ipc::ControlResponse, ipc::ForwardError> {
            let window = self.window.clone();
            let client = std::thread::spawn(move || {
                ipc::control_window(&window, &request, Duration::from_secs(3))
            });
            let deadline = Instant::now() + Duration::from_secs(4);
            while !client.is_finished() && Instant::now() < deadline {
                self.adapter.drive(&mut self.app, &self.server, &self.token);
                std::thread::sleep(Duration::from_millis(2));
            }
            let result = client.join().unwrap();
            self.adapter.drive(&mut self.app, &self.server, &self.token);
            if result.as_ref().is_ok_and(|response| {
                matches!(response.status, ipc::ControlStatus::Committed { .. })
            }) {
                while !self.adapter.ready_to_close() && Instant::now() < deadline {
                    self.adapter.drive(&mut self.app, &self.server, &self.token);
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            result
        }
        fn publish(&self, guard: &ipc::QuitGuardStamp) -> ipc::QuitCommitCertificate {
            let store = WindowSessionStore::new(self.config.path());
            let snapshot = store.load().unwrap();
            let receipt = store
                .commit_guarded_quit(
                    store.prepare_quit(&snapshot).unwrap(),
                    GuardedQuitBinding {
                        transaction_id: "actual-app-quit".into(),
                        cohort: vec![GuardedQuitWindow {
                            owner: self.token.owner().clone(),
                            generation: self.token.generation(),
                            owner_revision: self.token.revision(),
                            window_id: self.window.id.clone(),
                            project: None,
                            guard: QuitUiGuard {
                                nonce: guard.nonce.clone(),
                                state: guard.state.clone(),
                            },
                        }],
                    },
                )
                .unwrap();
            ipc::QuitCommitCertificate {
                transaction_id: receipt.transaction_id,
                checkpoint_generation: receipt.checkpoint_generation.value(),
                checkpoint_revision: receipt.checkpoint_revision,
                cohort: vec![ipc::QuitPreparedWindow {
                    window: window_registry::ExpectedWindow {
                        id: self.window.id.clone(),
                        project: None,
                    },
                    state: stamp(&self.token),
                    guard: guard.clone(),
                }],
            }
        }
    }
    fn stamp(token: &WindowToken) -> ipc::QuitStateStamp {
        ipc::QuitStateStamp {
            owner: token.owner().as_str().into(),
            generation: token.generation().value(),
            owner_revision: token.revision(),
        }
    }
    #[test]
    fn actual_control_rejects_stale_owner_before_app_prepare() {
        let mut fixture = Fixture::new();
        let mut request = fixture.request(ipc::QuitControl::Prepare);
        request.expected_state.owner_revision += 1;
        let response = fixture.exchange(request).unwrap();
        assert!(matches!(
            response.status,
            ipc::ControlStatus::Rejected { .. }
        ));
        assert!(!fixture.app.coordinated_quit_active());
        assert!(!fixture.adapter.active());
    }

    #[test]
    fn actual_dirty_editor_pending_cancel_and_abort_preserve_disk() {
        let mut fixture = Fixture::new();
        let path = fixture.config.path().join("pending.py");
        let original = b"answer = 1\n";
        std::fs::write(&path, original).unwrap();
        fixture.app.open_file(&path);
        fixture.app.testing_edit_active_document("# dirty draft\n");
        let response = fixture
            .exchange(fixture.request(ipc::QuitControl::Prepare))
            .unwrap();
        assert_eq!(response.status, ipc::ControlStatus::Pending);
        assert!(fixture.app.coordinated_quit_active());
        fixture.app.answer_coordinated_quit(
            "actual-app-quit",
            crate::app::window_lifecycle::QuitAnswer::Cancel,
        );
        let response = fixture
            .exchange(fixture.request(ipc::QuitControl::PollPrepared))
            .unwrap();
        assert!(matches!(
            response.status,
            ipc::ControlStatus::Rejected { .. }
        ));
        assert!(fixture.app.coordinated_quit_active());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(
            fixture
                .exchange(fixture.request(ipc::QuitControl::Abort))
                .unwrap()
                .status,
            ipc::ControlStatus::Aborted
        );
        assert!(!fixture.app.coordinated_quit_active());
        assert_eq!(std::fs::read(path).unwrap(), original);
    }

    #[test]
    fn actual_app_cancel_rejects_poll_and_retains_freeze_until_checked_abort() {
        let mut fixture = Fixture::new();
        let response = fixture
            .exchange(fixture.request(ipc::QuitControl::Prepare))
            .unwrap();
        assert!(matches!(
            response.status,
            ipc::ControlStatus::Prepared { .. }
        ));
        assert!(fixture.adapter.active());
        assert!(!fixture.adapter.ready_to_close());
        fixture.app.answer_coordinated_quit(
            "actual-app-quit",
            crate::app::window_lifecycle::QuitAnswer::Cancel,
        );
        assert!(fixture.app.coordinated_quit_active());
        let response = fixture
            .exchange(fixture.request(ipc::QuitControl::PollPrepared))
            .unwrap();
        assert!(matches!(
            response.status,
            ipc::ControlStatus::Rejected { .. }
        ));
        assert!(fixture.app.coordinated_quit_active());
        let response = fixture
            .exchange(fixture.request(ipc::QuitControl::Abort))
            .unwrap();
        assert_eq!(response.status, ipc::ControlStatus::Aborted);
        assert!(!fixture.app.coordinated_quit_active());
        assert!(!fixture.adapter.active());
    }
    #[test]
    fn actual_app_full_certificate_abort_refusal_and_verified_commit() {
        let mut fixture = Fixture::new();
        let response = fixture
            .exchange(fixture.request(ipc::QuitControl::Prepare))
            .unwrap();
        let ipc::ControlStatus::Prepared { guard } = response.status else {
            panic!("actual Welcome app must prepare")
        };
        let certificate = fixture.publish(&guard);
        let response = fixture
            .exchange(fixture.request(ipc::QuitControl::Abort))
            .unwrap();
        assert!(matches!(
            response.status,
            ipc::ControlStatus::Rejected { .. }
        ));
        assert!(fixture.app.coordinated_quit_active());
        assert!(!fixture.adapter.committed());
        let mut forged = certificate.clone();
        forged.cohort[0].guard.state = "different-complete-binding".into();
        let response = fixture
            .exchange(fixture.request(ipc::QuitControl::Commit {
                guard: forged.cohort[0].guard.clone(),
                certificate: forged,
            }))
            .unwrap();
        assert!(matches!(
            response.status,
            ipc::ControlStatus::Rejected { .. }
        ));
        assert!(!fixture.app.coordinated_quit_published("actual-app-quit"));
        let response = fixture
            .exchange(fixture.request(ipc::QuitControl::Commit {
                guard: guard.clone(),
                certificate: certificate.clone(),
            }))
            .unwrap();
        assert_eq!(
            response.status,
            ipc::ControlStatus::Committed { certificate }
        );
        assert!(fixture.app.coordinated_quit_published("actual-app-quit"));
        assert!(fixture.adapter.committed());
        assert!(fixture.adapter.ready_to_close());
    }
    #[test]
    fn corrupt_abort_evidence_preserves_actual_app_freeze_and_bytes() {
        let mut fixture = Fixture::new();
        fixture
            .exchange(fixture.request(ipc::QuitControl::Prepare))
            .unwrap();
        let path = WindowSessionStore::new(fixture.config.path()).path();
        std::fs::write(&path, b"{").unwrap();
        let response = fixture
            .exchange(fixture.request(ipc::QuitControl::Abort))
            .unwrap();
        assert!(matches!(
            response.status,
            ipc::ControlStatus::Rejected { .. }
        ));
        assert!(fixture.app.coordinated_quit_active());
        assert!(fixture.adapter.active());
        assert_eq!(std::fs::read(path).unwrap(), b"{");
    }
    #[test]
    fn actual_owner_probe_distinguishes_duplicate_welcome_projects_and_stale_state() {
        let first = Fixture::new();
        let server = ipc::Server::start_secondary(first.config.path()).unwrap();
        let store = WindowSessionStore::new(first.config.path());
        let other = store
            .accept_window(&store.load().unwrap(), WindowOwnerId::new(), None)
            .unwrap()
            .token;
        server.set_quit_state(Some(stamp(&other))).unwrap();
        let bindings =
            discover(first.config.path(), Instant::now() + Duration::from_secs(3)).unwrap();
        assert_eq!(bindings.len(), 2);
        assert_ne!(bindings[0].token.owner(), bindings[1].token.owner());
        server.set_quit_state(Some(stamp(&first.token))).unwrap();
        assert!(discover(first.config.path(), Instant::now() + Duration::from_secs(3)).is_err());
    }
    #[test]
    fn owners_of_killed_windows_are_pruned_before_quit_and_at_startup() {
        let fixture = Fixture::new();
        let store = WindowSessionStore::new(fixture.config.path());
        // A window process that was killed: its registry lease is gone, but its
        // accepted owner is still live in the window session.
        let dead = || {
            store
                .accept_window(&store.load().unwrap(), WindowOwnerId::new(), None)
                .unwrap()
                .token
        };
        let first = dead();
        assert_eq!(store.load().unwrap().live_windows().len(), 2);
        let bindings = discover(
            fixture.config.path(),
            Instant::now() + Duration::from_secs(3),
        )
        .unwrap();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].token, fixture.token);
        let live = store.load().unwrap().live_windows();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].token, fixture.token);
        assert!(
            store.close_window(&first).is_err(),
            "retired owner stays tombstoned"
        );
        dead();
        assert_eq!(
            prune_dead_owners(
                fixture.config.path(),
                Instant::now() + Duration::from_secs(3)
            ),
            Ok(1)
        );
        assert_eq!(store.load().unwrap().live_windows().len(), 1);
        assert_eq!(
            prune_dead_owners(
                fixture.config.path(),
                Instant::now() + Duration::from_secs(3)
            ),
            Ok(0)
        );
    }
    #[test]
    fn owners_are_not_pruned_while_any_endpoint_is_unproved() {
        let fixture = Fixture::new();
        let store = WindowSessionStore::new(fixture.config.path());
        // A second, live endpoint that has not published its owner yet (it is
        // between acceptance and publication): nothing may be retired.
        let starting = ipc::Server::start_secondary(fixture.config.path()).unwrap();
        store
            .accept_window(&store.load().unwrap(), WindowOwnerId::new(), None)
            .unwrap();
        assert!(prune_dead_owners(
            fixture.config.path(),
            Instant::now() + Duration::from_secs(3)
        )
        .is_err());
        assert_eq!(store.load().unwrap().live_windows().len(), 2);
        drop(starting);
    }
    #[test]
    fn full_quit_succeeds_after_a_killed_window_left_its_owner() {
        let mut fixture = Fixture::new();
        let store = WindowSessionStore::new(fixture.config.path());
        store
            .accept_window(&store.load().unwrap(), WindowOwnerId::new(), None)
            .unwrap();
        fixture
            .adapter
            .request(fixture.config.path(), &fixture.window.id)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(6);
        while !fixture.adapter.ready_to_close() && Instant::now() < deadline {
            fixture
                .adapter
                .drive(&mut fixture.app, &fixture.server, &fixture.token);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(fixture.adapter.error(), None);
        assert!(fixture.adapter.ready_to_close());
    }
    #[test]
    fn actual_worker_cancel_before_gui_prepare_preserves_membership() {
        let mut fixture = Fixture::new();
        fixture
            .adapter
            .request(fixture.config.path(), &fixture.window.id)
            .unwrap();
        fixture.adapter.cancel();
        let deadline = Instant::now() + Duration::from_secs(6);
        while fixture.adapter.active() && Instant::now() < deadline {
            fixture
                .adapter
                .drive(&mut fixture.app, &fixture.server, &fixture.token);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            fixture.adapter.progress().unwrap().phase,
            QuitPhase::Aborted
        );
        assert!(!fixture.adapter.active());
        assert!(!fixture.adapter.committed());
        assert!(!fixture.adapter.ready_to_close());
        assert!(!fixture.app.coordinated_quit_published(
            fixture
                .adapter
                .worker
                .as_ref()
                .unwrap()
                .transaction
                .as_deref()
                .unwrap()
        ));
        let snapshot = WindowSessionStore::new(fixture.config.path())
            .load()
            .unwrap();
        assert!(snapshot.guarded_quit_certificate().is_none());
        assert_eq!(snapshot.live_windows().len(), 1);
        assert_eq!(snapshot.live_windows()[0].token, fixture.token);
    }

    #[test]
    fn actual_app_worker_requires_own_ack_before_ready() {
        let mut fixture = Fixture::new();
        fixture
            .adapter
            .request(fixture.config.path(), &fixture.window.id)
            .unwrap();
        assert!(fixture.adapter.active());
        let deadline = Instant::now() + Duration::from_secs(6);
        while !fixture.adapter.ready_to_close() && Instant::now() < deadline {
            fixture
                .adapter
                .drive(&mut fixture.app, &fixture.server, &fixture.token);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(fixture.adapter.committed());
        assert!(fixture.adapter.ready_to_close());
        assert_eq!(
            fixture.adapter.progress().unwrap().phase,
            QuitPhase::Complete
        );
        assert!(WindowSessionStore::new(fixture.config.path())
            .load()
            .unwrap()
            .guarded_quit_certificate()
            .is_some());
    }
}
