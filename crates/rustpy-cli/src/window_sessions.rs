//! Startup restoration of an accepted window cohort. Registry proofs establish
//! endpoint truth; durable attempts record uncertainty before a child is spawned.
//! An unknown attempt is never automatically resubmitted after a coordinator
//! crash. UI callers accept the plan before publishing a project/first frame and
//! confirm it only after the authenticated endpoint publishes that project.
use crate::{
    args::{GlobalOptions, OpenArgs},
    instance, ipc, window_registry,
};
use rustpy_core::vfs::ContentRevision;
use rustpy_settings::window_session_store::{
    RestoreClaim, WindowGeneration, WindowMutation, WindowOwnerId, WindowSessionError,
    WindowSessionStore, WindowToken,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::OpenOptions,
    io::{self, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::Child,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
const MAX_JOURNAL: usize = 1024 * 1024;
const MAX_COHORT: usize = 256;

#[derive(Debug)]
pub enum RestoreError {
    Io(io::Error),
    Store(WindowSessionError),
    InvalidJournal(String),
    InvalidChild(String),
    RegistryUncertain(Vec<window_registry::WindowUncertainty>),
    WrongStartupEndpoint,
    StaleGeneration,
    AlreadyStarted,
    PublicationChanged,
    Busy,
}
impl From<io::Error> for RestoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<WindowSessionError> for RestoreError {
    fn from(error: WindowSessionError) -> Self {
        Self::Store(error)
    }
}
impl std::fmt::Display for RestoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "window restore I/O failed: {error}"),
            Self::Store(error) => write!(f, "{error}"),
            Self::InvalidJournal(reason) => write!(
                f,
                "window restore attempts are invalid: {reason}; left unchanged"
            ),
            Self::InvalidChild(reason) => write!(f, "invalid restore child: {reason}"),
            Self::RegistryUncertain(entries) => write!(
                f,
                "window liveness is uncertain: {}",
                entries
                    .iter()
                    .map(|entry| entry.reason.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            Self::WrongStartupEndpoint => write!(
                f,
                "restore requires this startup's authenticated welcome endpoint"
            ),
            Self::StaleGeneration => write!(f, "window restore generation changed"),
            Self::AlreadyStarted => write!(
                f,
                "restore child launches were already submitted; poll unknown outcomes without respawning"
            ),
            Self::PublicationChanged => {
                write!(f, "window restore state changed before publication")
            }
            Self::Busy => write!(f, "window restore attempts are busy"),
        }
    }
}
impl std::error::Error for RestoreError {}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkipReason {
    ExplicitTarget,
    Welcome,
    DontReopen,
    NewWindow,
    Disabled,
    SmokeTest,
    ExistingWindows,
    ClaimedElsewhere,
    NoCheckpoint,
    EmptyCheckpoint,
}
#[derive(Clone, Debug)]
pub enum RestoreIssue {
    MissingProject(PathBuf),
    InvalidProject {
        path: PathBuf,
        reason: String,
    },
    DuplicateProject(Option<PathBuf>),
    SpawnFailed {
        project: Option<PathBuf>,
        reason: String,
    },
    ChildExited {
        project: Option<PathBuf>,
        status: String,
    },
    Unknown {
        owner: WindowOwnerId,
        project: Option<PathBuf>,
    },
    AlreadyOpen(Option<PathBuf>),
    Deferred(Option<PathBuf>),
    RegistryUncertain(Vec<window_registry::WindowUncertainty>),
}
#[derive(Debug)]
pub enum StartupRestore {
    Skipped(SkipReason),
    Child(PrimaryPlan),
    Restore(Box<RestoreRound>),
    Uncertain(PendingRestore),
}
#[derive(Debug)]
pub struct PendingRestore {
    pub attempts: Vec<RestoreIssue>,
    _claim: RestoreClaim,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ReceiptRoute {
    port: u16,
    nonce: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ChildMarker {
    checkpoint_generation: WindowGeneration,
    runtime_generation: WindowGeneration,
    owner: WindowOwnerId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    receipt: Option<ReceiptRoute>,
}
#[derive(Serialize, Deserialize)]
struct ChildReceipt {
    config: PathBuf,
    marker: ChildMarker,
    project: Option<PathBuf>,
    token: WindowToken,
    window: window_registry::WindowInfo,
}
/// A checked intended window. Its logical identity is not an endpoint identity.
#[derive(Clone, Debug)]
pub struct PrimaryPlan {
    store: WindowSessionStore,
    marker: ChildMarker,
    project: Option<PathBuf>,
    requires_attempt: bool,
    accepted_token: Arc<Mutex<Option<WindowToken>>>,
}
impl PrimaryPlan {
    pub fn owner(&self) -> &WindowOwnerId {
        &self.marker.owner
    }
    pub fn project(&self) -> Option<&Path> {
        self.project.as_deref()
    }
    pub fn generation(&self) -> WindowGeneration {
        self.marker.runtime_generation
    }
    pub fn store(&self) -> &WindowSessionStore {
        &self.store
    }
    /// Root must have completed its project-load preflight. Call before endpoint
    /// project publication or the first GUI frame. A replay/late child cannot
    /// acquire its logical owner after a generation or tombstone changed.
    pub fn accept_before_publish(
        &self,
        server: &ipc::Server,
        timeout: Duration,
    ) -> Result<WindowMutation, RestoreError> {
        fresh_endpoint(&self.store, server, Instant::now() + timeout)?;
        let lock = journal_lock(&self.store, Instant::now() + timeout)?;
        let journal = load_journal(&self.store)?;
        self.validate(&journal, false)?;
        if !same_journal_lock(&lock)? {
            return Err(RestoreError::PublicationChanged);
        }
        let snapshot = self.store.load()?;
        check_marker(&snapshot, &self.marker)?;
        let mutation = self.store.accept_window(
            &snapshot,
            self.marker.owner.clone(),
            self.project.as_deref(),
        )?;
        *self
            .accepted_token
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(mutation.token.clone());
        Ok(mutation)
    }
    /// Endpoint proof and the exact accepted logical token are both necessary.
    /// Confirmation clears uncertainty, without persisting endpoint IDs or PIDs.
    pub fn confirm_after_publish(
        &self,
        server: &ipc::Server,
        token: &WindowToken,
        timeout: Duration,
    ) -> Result<(), RestoreError> {
        if self
            .accepted_token
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            != Some(token)
        {
            return Err(RestoreError::StaleGeneration);
        }
        let deadline = Instant::now() + timeout;
        let presence = observed(&self.store, deadline)?;
        let window = presence
            .live
            .into_iter()
            .find(|window| {
                window.id == server.window_id()
                    && window.endpoint.port == server.port()
                    && window.project == self.project
            })
            .ok_or(RestoreError::WrongStartupEndpoint)?;
        let lock = journal_lock(&self.store, deadline)?;
        let mut journal = load_journal(&self.store)?;
        self.validate(&journal, true)?;
        let snapshot = self.store.load()?;
        check_marker(&snapshot, &self.marker)?;
        if !snapshot.live_windows().iter().any(|window| {
            &window.token == token
                && window.token.owner() == self.owner()
                && window.project == self.project
        }) {
            return Err(RestoreError::StaleGeneration);
        }
        journal
            .state
            .as_mut()
            .ok_or_else(|| RestoreError::InvalidJournal("missing startup round".into()))?
            .attempts
            .insert(
                self.marker.owner.clone(),
                Attempt {
                    project: self.project.clone(),
                    outcome: Outcome::Confirmed,
                },
            );
        publish_journal(&self.store, journal, &lock)?;
        drop(lock);
        if let Some(route) = &self.marker.receipt {
            let receipt = ChildReceipt {
                config: self.store.path().parent().unwrap().into(),
                marker: self.marker.clone(),
                project: self.project.clone(),
                token: token.clone(),
                window,
            };
            let mut stream = TcpStream::connect_timeout(
                &SocketAddr::from((Ipv4Addr::LOCALHOST, route.port)),
                remaining(deadline)?,
            )?;
            write_message(
                &mut stream,
                &serde_json::to_vec(&receipt).map_err(io::Error::other)?,
                deadline,
            )?;
            if read_message(&mut stream, deadline)? != b"accepted" {
                return Err(RestoreError::InvalidChild(
                    "parent rejected endpoint receipt".into(),
                ));
            }
        } else if self.requires_attempt {
            return Err(RestoreError::InvalidChild(
                "child lacks transient receipt route".into(),
            ));
        }
        Ok(())
    }
    fn validate(&self, journal: &Journal, allow_confirmed: bool) -> Result<(), RestoreError> {
        if self.requires_attempt
            && !self.marker.receipt.as_ref().is_some_and(|route| {
                route.port != 0
                    && route.nonce.len() == 64
                    && route.nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        {
            return Err(RestoreError::InvalidChild(
                "child lacks a valid transient receipt route".into(),
            ));
        }
        let snapshot = self.store.load()?;
        check_marker(&snapshot, &self.marker)?;
        let checkpoint_window = snapshot
            .checkpoint()
            .unwrap()
            .windows
            .iter()
            .find(|window| window.owner == self.marker.owner)
            .unwrap();
        let project = canonical_project(checkpoint_window.project.as_deref())
            .map_err(|_| RestoreError::PublicationChanged)?;
        if project != self.project {
            return Err(RestoreError::PublicationChanged);
        }
        let state = journal
            .state
            .as_ref()
            .ok_or_else(|| RestoreError::InvalidChild("missing attempt round".into()))?;
        if state.checkpoint_generation != self.marker.checkpoint_generation
            || state.runtime_generation != self.marker.runtime_generation
        {
            return Err(RestoreError::StaleGeneration);
        }
        if self.requires_attempt
            && !state.attempts.get(self.owner()).is_some_and(|attempt| {
                attempt.project == self.project
                    && (attempt.outcome == Outcome::Unknown
                        || (allow_confirmed && attempt.outcome == Outcome::Confirmed))
            })
        {
            return Err(RestoreError::InvalidChild(
                "child has no pending launch intent".into(),
            ));
        }
        Ok(())
    }
}
struct PendingChild {
    plan: PrimaryPlan,
    process: Child,
}
impl std::fmt::Debug for PendingChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingChild")
            .field("plan", &self.plan)
            .finish_non_exhaustive()
    }
}
#[derive(Debug)]
pub struct RestoreRound {
    store: WindowSessionStore,
    _claim: RestoreClaim,
    primary: PrimaryPlan,
    children: Vec<PrimaryPlan>,
    next_child: usize,
    pending: Vec<PendingChild>,
    finished_children: Vec<Child>,
    receipt_listener: TcpListener,
    confirmed_endpoints: BTreeMap<WindowOwnerId, (window_registry::WindowInfo, WindowToken)>,
    primary_endpoint: Option<window_registry::WindowInfo>,
    issues: Vec<RestoreIssue>,
    started: bool,
}
#[derive(Clone, Debug)]
pub struct RestoreProgress {
    pub issues: Vec<RestoreIssue>,
    pub confirmed: Vec<Option<PathBuf>>,
    pub pending: usize,
    /// Logical owners whose launch intent has not yet been recorded.
    pub unsubmitted: usize,
}
impl RestoreRound {
    pub fn primary(&self) -> &PrimaryPlan {
        &self.primary
    }
    pub fn issues(&self) -> &[RestoreIssue] {
        &self.issues
    }
    pub fn unsubmitted_count(&self) -> usize {
        self.children.len() - self.next_child
    }
    /// Submitted children still awaiting an authenticated outcome.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
    /// Submit each missing root at most once. This round retains the OS claim
    /// and unknown child handles; dropping a round never kills an unknown child.
    pub fn restore_children(
        &mut self,
        server: &ipc::Server,
        program: &Path,
        global: &GlobalOptions,
        cwd: &Path,
        timeout: Duration,
    ) -> Result<RestoreProgress, RestoreError> {
        self.restore_children_with(server, global, timeout, |arguments| {
            instance::spawn_detached(program, arguments, cwd)
        })
    }
    fn restore_children_with(
        &mut self,
        server: &ipc::Server,
        global: &GlobalOptions,
        timeout: Duration,
        spawn: impl FnMut(&[String]) -> io::Result<Child>,
    ) -> Result<RestoreProgress, RestoreError> {
        if self.started {
            return Err(RestoreError::AlreadyStarted);
        }
        self.started = true;
        let deadline = Instant::now() + timeout;
        let presence = observed(&self.store, deadline)?;
        let primary = presence
            .live
            .iter()
            .find(|window| {
                window.id == server.window_id()
                    && window.endpoint.port == server.port()
                    && window.project == self.primary.project
            })
            .cloned()
            .ok_or(RestoreError::WrongStartupEndpoint)?;
        self.primary_endpoint = Some(primary);
        self.require_known_presence(presence)?;
        let snapshot = self.store.load()?;
        check_marker(&snapshot, &self.primary.marker)?;
        if !snapshot.live_windows().iter().any(|window| {
            window.token.owner() == self.primary.owner() && window.project == self.primary.project
        }) {
            return Err(RestoreError::StaleGeneration);
        }
        self.drive_children(server, global, deadline, spawn)
    }
    /// Continue this exact owned round within a fresh budget. Existing Unknown
    /// submissions are observed first; only owners never submitted can launch.
    /// `restore_children` remains the one-time initial submission entry point.
    pub fn resume_children(
        &mut self,
        server: &ipc::Server,
        program: &Path,
        global: &GlobalOptions,
        cwd: &Path,
        timeout: Duration,
    ) -> Result<RestoreProgress, RestoreError> {
        self.resume_children_with(server, global, timeout, |arguments| {
            instance::spawn_detached(program, arguments, cwd)
        })
    }
    fn resume_children_with(
        &mut self,
        server: &ipc::Server,
        global: &GlobalOptions,
        timeout: Duration,
        spawn: impl FnMut(&[String]) -> io::Result<Child>,
    ) -> Result<RestoreProgress, RestoreError> {
        if !self.started {
            return Err(RestoreError::WrongStartupEndpoint);
        }
        self.drive_children(server, global, Instant::now() + timeout, spawn)
    }
    fn drive_children(
        &mut self,
        server: &ipc::Server,
        global: &GlobalOptions,
        deadline: Instant,
        mut spawn: impl FnMut(&[String]) -> io::Result<Child>,
    ) -> Result<RestoreProgress, RestoreError> {
        if !self.primary_endpoint.as_ref().is_some_and(|endpoint| {
            endpoint.id == server.window_id() && endpoint.endpoint.port == server.port()
        }) {
            return Err(RestoreError::WrongStartupEndpoint);
        }
        // A prior drive may have exhausted its budget awaiting a newborn. Do
        // not submit another owner until that exact child resolves.
        let snapshot = self.store.load()?;
        check_marker(&snapshot, &self.primary.marker)?;
        let accepted_primary = self
            .primary
            .accepted_token
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        if !accepted_primary.as_ref().is_some_and(|token| {
            snapshot.live_windows().iter().any(|member| {
                &member.token == token
                    && token.owner() == self.primary.owner()
                    && member.project == self.primary.project
            })
        }) {
            return Err(RestoreError::StaleGeneration);
        }
        let mut confirmed = self.poll_until(deadline)?.confirmed;
        if self.pending.is_empty() && Instant::now() < deadline {
            self.require_known_presence(observed(&self.store, deadline)?)?;
        }
        while self.next_child < self.children.len()
            && self.pending.is_empty()
            && Instant::now() < deadline
        {
            let plan = self.children[self.next_child].clone();
            let presence = self.require_known_presence(observed(&self.store, deadline)?)?;
            if presence
                .live
                .iter()
                .any(|window| plan.project.is_some() && window.project == plan.project)
            {
                self.record(&plan, Outcome::ExistingWindow, deadline)?;
                self.next_child += 1;
                self.issues.push(RestoreIssue::AlreadyOpen(plan.project));
                continue;
            }
            let journal = load_journal(&self.store)?;
            if journal
                .state
                .as_ref()
                .is_some_and(|state| state.attempts.contains_key(plan.owner()))
            {
                return Err(RestoreError::InvalidJournal(
                    "unsubmitted logical owner already has a durable attempt".into(),
                ));
            }
            let arguments = child_arguments(&self.store, &plan, global)?;
            // Advance immediately after durable intent, before fork. Even an
            // error in later bookkeeping must never resubmit this owner.
            self.record(&plan, Outcome::Unknown, deadline)?;
            self.next_child += 1;
            match spawn(&arguments) {
                Ok(process) => self.pending.push(PendingChild { plan, process }),
                Err(error) => {
                    self.record(&plan, Outcome::FailedBeforeLaunch, deadline)?;
                    self.issues.push(RestoreIssue::SpawnFailed {
                        project: plan.project,
                        reason: error.to_string(),
                    });
                }
            }
            confirmed.extend(self.poll_until(deadline)?.confirmed);
        }
        let mut progress = self.poll_until(deadline)?;
        confirmed.extend(progress.confirmed);
        progress.confirmed = confirmed;
        Ok(progress)
    }
    fn require_known_presence(
        &self,
        presence: window_registry::WindowPresence,
    ) -> Result<window_registry::WindowPresence, RestoreError> {
        let snapshot = self.store.load()?;
        check_marker(&snapshot, &self.primary.marker)?;
        let live = snapshot.live_windows();
        let checkpoint = snapshot.checkpoint().unwrap();
        let current_binding =
            |owner: &WindowOwnerId, token: &WindowToken, project: &Option<PathBuf>| {
                live.iter().any(|member| {
                    &member.token == token
                        && member.token.owner() == owner
                        && &member.project == project
                }) && checkpoint.windows.iter().any(|member| {
                    &member.owner == owner
                        && canonical_project(member.project.as_deref())
                            .is_ok_and(|current| &current == project)
                })
            };
        let accepted_primary = self
            .primary
            .accepted_token
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let mut unknown = presence.uncertain.clone();
        if let Some(known) = &self.primary_endpoint {
            if !accepted_primary.as_ref().is_some_and(|token| {
                current_binding(self.primary.owner(), token, &self.primary.project)
            }) {
                unknown.push(window_registry::WindowUncertainty {
                    id: Some(known.id.clone()),
                    reason: "accepted primary membership changed during restoration".into(),
                });
            }
        }
        for (owner, (known, token)) in &self.confirmed_endpoints {
            if !current_binding(owner, token, &known.project) {
                unknown.push(window_registry::WindowUncertainty {
                    id: Some(known.id.clone()),
                    reason: "accepted child membership changed during restoration".into(),
                });
            }
        }
        for window in &presence.live {
            let primary = self.primary_endpoint.as_ref().is_some_and(|known| {
                same_endpoint(known, window)
                    && accepted_primary.as_ref().is_some_and(|token| {
                        current_binding(self.primary.owner(), token, &self.primary.project)
                    })
            });
            let child = self
                .confirmed_endpoints
                .iter()
                .any(|(owner, (known, token))| {
                    same_endpoint(known, window) && current_binding(owner, token, &known.project)
                });
            if !primary && !child {
                unknown.push(window_registry::WindowUncertainty {
                    id: Some(window.id.clone()),
                    reason:
                        "endpoint is not an authenticated accepted member of this restore round"
                            .into(),
                });
            }
        }
        if unknown.is_empty() {
            Ok(presence)
        } else {
            Err(RestoreError::RegistryUncertain(unknown))
        }
    }
    fn accept_receipts(&mut self, deadline: Instant) -> Result<(), RestoreError> {
        while Instant::now() < deadline {
            let (mut stream, _) = match self.receipt_listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error.into()),
            };
            let receipt: ChildReceipt =
                serde_json::from_slice(&read_message(&mut stream, deadline)?)
                    .map_err(|error| RestoreError::InvalidChild(error.to_string()))?;
            let plan = self
                .children
                .iter()
                .find(|plan| plan.owner() == &receipt.marker.owner)
                .ok_or_else(|| {
                    RestoreError::InvalidChild("receipt owner is not a child in this round".into())
                })?;
            if receipt.config != self.store.path().parent().unwrap()
                || receipt.marker != plan.marker
                || receipt.project != plan.project
                || receipt.token.owner() != plan.owner()
            {
                return Err(RestoreError::InvalidChild(
                    "receipt does not match this restore round".into(),
                ));
            }
            let journal = load_journal(&self.store)?;
            plan.validate(&journal, true)?;
            if !journal
                .state
                .as_ref()
                .unwrap()
                .attempts
                .get(plan.owner())
                .is_some_and(|attempt| attempt.outcome == Outcome::Confirmed)
            {
                return Err(RestoreError::InvalidChild(
                    "receipt precedes child confirmation".into(),
                ));
            }
            let snapshot = self.store.load()?;
            if !snapshot
                .live_windows()
                .iter()
                .any(|window| window.token == receipt.token && window.project == plan.project)
            {
                return Err(RestoreError::StaleGeneration);
            }
            let exact = window_registry::verify_endpoint_until(&receipt.window, deadline)?;
            if exact.project != plan.project {
                return Err(RestoreError::WrongStartupEndpoint);
            }
            // IDs/ports/tokens remain exclusively in this live parent object.
            write_message(&mut stream, b"accepted", deadline)?;
            self.confirmed_endpoints
                .insert(plan.owner().clone(), (exact, receipt.token));
        }
        Ok(())
    }
    /// Observe existing submissions only. Never creates another child.
    pub fn poll_children(&mut self, timeout: Duration) -> Result<RestoreProgress, RestoreError> {
        self.poll_until(Instant::now() + timeout)
    }
    fn poll_until(&mut self, deadline: Instant) -> Result<RestoreProgress, RestoreError> {
        let mut confirmed = Vec::new();
        let mut uncertainty = Vec::new();
        while !self.pending.is_empty() && Instant::now() < deadline {
            self.accept_receipts(deadline)?;
            match observed(&self.store, deadline)
                .and_then(|presence| self.require_known_presence(presence))
            {
                Ok(_) => uncertainty.clear(),
                Err(RestoreError::RegistryUncertain(entries)) => {
                    uncertainty = entries;
                    std::thread::sleep(
                        deadline
                            .saturating_duration_since(Instant::now())
                            .min(Duration::from_millis(10)),
                    );
                    continue;
                }
                Err(error) => return Err(error),
            }
            let journal = load_journal(&self.store)?;
            let snapshot = self.store.load()?;
            check_marker(&snapshot, &self.primary.marker)?;
            let mut index = 0;
            while index < self.pending.len() {
                let mut pending = self.pending.remove(index);
                let result = (|| -> Result<bool, RestoreError> {
                    let accepted = snapshot.live_windows().iter().any(|window| {
                        window.token.owner() == pending.plan.owner()
                            && window.project == pending.plan.project
                    });
                    // Only the child can acknowledge its exact authenticated
                    // Server/token pair. A different endpoint at this project
                    // cannot turn an unresolved launch into a confirmation.
                    let acknowledged = journal.state.as_ref().is_some_and(|state| {
                        state.checkpoint_generation == pending.plan.marker.checkpoint_generation
                            && state.runtime_generation == pending.plan.marker.runtime_generation
                            && state
                                .attempts
                                .get(pending.plan.owner())
                                .is_some_and(|attempt| {
                                    attempt.project == pending.plan.project
                                        && attempt.outcome == Outcome::Confirmed
                                })
                    });
                    if accepted
                        && acknowledged
                        && self.confirmed_endpoints.contains_key(pending.plan.owner())
                    {
                        confirmed.push(pending.plan.project.clone());
                        Ok(true)
                    } else if let Some(status) = pending.process.try_wait()? {
                        self.record(&pending.plan, Outcome::Exited, deadline)?;
                        self.issues.push(RestoreIssue::ChildExited {
                            project: pending.plan.project.clone(),
                            status: status.to_string(),
                        });
                        Ok(true)
                    } else {
                        Ok(false)
                    }
                })();
                match result {
                    Ok(true) => self.finished_children.push(pending.process),
                    Ok(false) => {
                        self.pending.insert(index, pending);
                        index += 1;
                    }
                    Err(error) => {
                        self.pending.insert(index, pending);
                        return Err(error);
                    }
                }
            }
            if !self.pending.is_empty() {
                std::thread::sleep(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_millis(10)),
                );
            }
        }
        let mut issues = self.issues.clone();
        issues.extend(
            self.children[self.next_child..]
                .iter()
                .map(|plan| RestoreIssue::Deferred(plan.project.clone())),
        );
        if !uncertainty.is_empty() {
            issues.push(RestoreIssue::RegistryUncertain(uncertainty));
        }
        issues.extend(self.pending.iter().map(|pending| RestoreIssue::Unknown {
            owner: pending.plan.owner().clone(),
            project: pending.plan.project.clone(),
        }));
        Ok(RestoreProgress {
            issues,
            confirmed,
            pending: self.pending.len(),
            unsubmitted: self.unsubmitted_count(),
        })
    }
    fn record(
        &self,
        plan: &PrimaryPlan,
        outcome: Outcome,
        deadline: Instant,
    ) -> Result<(), RestoreError> {
        let lock = journal_lock(&self.store, deadline)?;
        let mut journal = load_journal(&self.store)?;
        let snapshot = self.store.load()?;
        check_marker(&snapshot, &plan.marker)?;
        let state = journal
            .state
            .as_mut()
            .ok_or_else(|| RestoreError::InvalidJournal("missing round".into()))?;
        if state.checkpoint_generation != plan.marker.checkpoint_generation
            || state.runtime_generation != plan.marker.runtime_generation
        {
            return Err(RestoreError::StaleGeneration);
        }
        state.attempts.insert(
            plan.owner().clone(),
            Attempt {
                project: plan.project.clone(),
                outcome,
            },
        );
        publish_journal(&self.store, journal, &lock)?;
        Ok(())
    }
}

/// Run after Launch::Start and before a GUI frame/project publication. The
/// supplied Server must be this startup's fresh, authenticated welcome endpoint.
/// Only that endpoint is excluded; every other welcome window blocks restore.
pub fn prepare_startup(
    config: &Path,
    args: &OpenArgs,
    reopen: bool,
    server: &ipc::Server,
    timeout: Duration,
) -> Result<StartupRestore, RestoreError> {
    let store = WindowSessionStore::new(config);
    let deadline = Instant::now() + timeout;
    if let Some(encoded) = &args.restore_child {
        let marker: ChildMarker = serde_json::from_str(encoded)
            .map_err(|error| RestoreError::InvalidChild(error.to_string()))?;
        if !args.new_window {
            return Err(RestoreError::InvalidChild(
                "restore child requires --new-window".into(),
            ));
        }
        fresh_endpoint(&store, server, deadline)?;
        let snapshot = store.load()?;
        check_marker(&snapshot, &marker)?;
        let window = snapshot
            .checkpoint()
            .unwrap()
            .windows
            .iter()
            .find(|window| window.owner == marker.owner)
            .ok_or_else(|| RestoreError::InvalidChild("owner is not in the checkpoint".into()))?;
        let project = canonical_project(window.project.as_deref())
            .map_err(|issue| RestoreError::InvalidChild(format!("{issue:?}")))?;
        let targets: Vec<_> = args
            .targets
            .iter()
            .map(|target| dunce::canonicalize(&target.path))
            .collect::<io::Result<_>>()?;
        if targets != project.iter().cloned().collect::<Vec<_>>()
            || (project.is_none() && !args.welcome)
        {
            return Err(RestoreError::InvalidChild(
                "child target does not match its checkpoint owner".into(),
            ));
        }
        let plan = PrimaryPlan {
            store,
            marker,
            project,
            requires_attempt: true,
            accepted_token: Arc::new(Mutex::new(None)),
        };
        let journal = load_journal(&plan.store)?;
        plan.validate(&journal, false)?;
        return Ok(StartupRestore::Child(plan));
    }
    let skip = if !args.targets.is_empty() || args.open_file.is_some() {
        Some(SkipReason::ExplicitTarget)
    } else if args.welcome {
        Some(SkipReason::Welcome)
    } else if args.dont_reopen_projects {
        Some(SkipReason::DontReopen)
    } else if args.new_window {
        Some(SkipReason::NewWindow)
    } else if !reopen {
        Some(SkipReason::Disabled)
    } else if args.is_smoke_test() {
        Some(SkipReason::SmokeTest)
    } else {
        None
    };
    if let Some(reason) = skip {
        return Ok(StartupRestore::Skipped(reason));
    }
    let presence = fresh_endpoint(&store, server, deadline)?;
    if presence.live.len() != 1 {
        return Ok(StartupRestore::Skipped(SkipReason::ExistingWindows));
    }
    let snapshot = store.load()?;
    let Some(checkpoint) = snapshot.checkpoint().cloned() else {
        return Ok(StartupRestore::Skipped(SkipReason::NoCheckpoint));
    };
    if checkpoint.windows.len() > MAX_COHORT {
        return Err(RestoreError::InvalidJournal(
            "checkpoint exceeds bounded window cohort".into(),
        ));
    }
    let Some(mut claim) = store.try_claim_restore(checkpoint.generation)? else {
        return Ok(StartupRestore::Skipped(SkipReason::ClaimedElsewhere));
    };
    let presence = fresh_endpoint(&store, server, deadline)?;
    if presence.live.len() != 1 {
        return Ok(StartupRestore::Skipped(SkipReason::ExistingWindows));
    }
    let lock = journal_lock(&store, deadline)?;
    let journal = load_journal(&store)?;
    if let Some(state) = &journal.state {
        if state.checkpoint_generation.value() > checkpoint.generation.value()
            || state.runtime_generation.value() > snapshot.generation().value()
        {
            return Err(RestoreError::InvalidJournal(
                "attempt journal is newer than checked window state".into(),
            ));
        }
        if state.checkpoint_generation == checkpoint.generation {
            let attempts: Vec<_> = state
                .attempts
                .iter()
                .filter(|(_, attempt)| attempt.outcome == Outcome::Unknown)
                .map(|(owner, attempt)| RestoreIssue::Unknown {
                    owner: owner.clone(),
                    project: attempt.project.clone(),
                })
                .collect();
            if !attempts.is_empty() {
                return Ok(StartupRestore::Uncertain(PendingRestore {
                    attempts,
                    _claim: claim,
                }));
            }
        }
    }
    let mut plans = Vec::new();
    let mut issues = Vec::new();
    let mut seen = BTreeSet::new();
    for window in checkpoint.windows {
        match canonical_project(window.project.as_deref()) {
            // Welcome windows have separate logical owners despite sharing
            // the absence of a project. Only real project paths are deduped.
            Ok(project) if project.is_none() || seen.insert(project.clone()) => {
                plans.push((window.owner, project))
            }
            Ok(project) => issues.push(RestoreIssue::DuplicateProject(project)),
            Err(issue) => issues.push(issue),
        }
    }
    if plans.is_empty() {
        return if issues.is_empty() {
            Ok(StartupRestore::Skipped(SkipReason::EmptyCheckpoint))
        } else {
            Ok(StartupRestore::Uncertain(PendingRestore {
                attempts: issues,
                _claim: claim,
            }))
        };
    }
    let begun = store.begin_restore(&mut claim)?;
    let state = JournalState {
        format_version: 1,
        checkpoint_generation: checkpoint.generation,
        runtime_generation: begun.generation(),
        attempts: BTreeMap::new(),
    };
    publish_journal(
        &store,
        Journal {
            state: Some(state),
            ..journal
        },
        &lock,
    )?;
    let receipt_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    receipt_listener.set_nonblocking(true)?;
    let route = ReceiptRoute {
        port: receipt_listener.local_addr()?.port(),
        nonce: ipc::new_token()?,
    };
    let mut plans = plans.into_iter().map(|(owner, project)| PrimaryPlan {
        store: store.clone(),
        marker: ChildMarker {
            checkpoint_generation: checkpoint.generation,
            runtime_generation: begun.generation(),
            owner,
            receipt: None,
        },
        project,
        requires_attempt: false,
        accepted_token: Arc::new(Mutex::new(None)),
    });
    let primary = plans.next().unwrap();
    let children = plans
        .map(|mut plan| {
            plan.requires_attempt = true;
            plan.marker.receipt = Some(route.clone());
            plan
        })
        .collect();
    Ok(StartupRestore::Restore(Box::new(RestoreRound {
        store,
        _claim: claim,
        primary,
        children,
        next_child: 0,
        pending: Vec::new(),
        finished_children: Vec::new(),
        receipt_listener,
        confirmed_endpoints: BTreeMap::new(),
        primary_endpoint: None,
        issues,
        started: false,
    })))
}
fn observed(
    store: &WindowSessionStore,
    deadline: Instant,
) -> Result<window_registry::WindowPresence, RestoreError> {
    let presence = window_registry::presence_until(store.path().parent().unwrap(), deadline)?;
    if !presence.uncertain.is_empty() {
        return Err(RestoreError::RegistryUncertain(presence.uncertain));
    }
    Ok(presence)
}
fn fresh_endpoint(
    store: &WindowSessionStore,
    server: &ipc::Server,
    deadline: Instant,
) -> Result<window_registry::WindowPresence, RestoreError> {
    let presence = observed(store, deadline)?;
    if !presence.live.iter().any(|window| {
        window.id == server.window_id()
            && window.endpoint.port == server.port()
            && window.project.is_none()
    }) {
        return Err(RestoreError::WrongStartupEndpoint);
    }
    Ok(presence)
}
fn check_marker(
    snapshot: &rustpy_settings::window_session_store::WindowSessionSnapshot,
    marker: &ChildMarker,
) -> Result<(), RestoreError> {
    if snapshot.generation() != marker.runtime_generation
        || snapshot
            .checkpoint()
            .map(|checkpoint| checkpoint.generation)
            != Some(marker.checkpoint_generation)
    {
        return Err(RestoreError::StaleGeneration);
    }
    if !snapshot
        .checkpoint()
        .unwrap()
        .windows
        .iter()
        .any(|window| window.owner == marker.owner)
    {
        return Err(RestoreError::InvalidChild(
            "logical owner not in checkpoint".into(),
        ));
    }
    Ok(())
}
fn canonical_project(project: Option<&Path>) -> Result<Option<PathBuf>, RestoreIssue> {
    project
        .map(|path| match dunce::canonicalize(path) {
            Ok(root) if root.is_dir() => Ok(root),
            Ok(_) => Err(RestoreIssue::InvalidProject {
                path: path.into(),
                reason: "not a directory".into(),
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Err(RestoreIssue::MissingProject(path.into()))
            }
            Err(error) => Err(RestoreIssue::InvalidProject {
                path: path.into(),
                reason: error.to_string(),
            }),
        })
        .transpose()
}
fn same_endpoint(a: &window_registry::WindowInfo, b: &window_registry::WindowInfo) -> bool {
    a.id == b.id
        && a.endpoint.port == b.endpoint.port
        && ipc::same_secret(&a.endpoint.token, &b.endpoint.token)
        && a.project == b.project
}
fn remaining(deadline: Instant) -> io::Result<Duration> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "restore receipt deadline expired",
        ))
    } else {
        Ok(remaining)
    }
}
fn write_message(stream: &mut TcpStream, bytes: &[u8], deadline: Instant) -> io::Result<()> {
    let mut message = bytes.to_vec();
    message.push(b'\n');
    let mut rest = message.as_slice();
    while !rest.is_empty() {
        stream.set_write_timeout(Some(remaining(deadline)?))?;
        let written = stream.write(rest)?;
        if written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "restore receipt stopped",
            ));
        }
        rest = &rest[written..];
    }
    remaining(deadline)?;
    Ok(())
}
fn read_message(stream: &mut TcpStream, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        stream.set_read_timeout(Some(remaining(deadline)?))?;
        let mut chunk = [0u8; 1024];
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "restore receipt ended",
            ));
        }
        let end = chunk[..count].iter().position(|byte| *byte == b'\n');
        bytes.extend_from_slice(&chunk[..end.unwrap_or(count)]);
        if bytes.len() > 65536 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "restore receipt exceeds limit",
            ));
        }
        if end.is_some() {
            remaining(deadline)?;
            return Ok(bytes);
        }
    }
}
fn child_arguments(
    store: &WindowSessionStore,
    plan: &PrimaryPlan,
    global: &GlobalOptions,
) -> Result<Vec<String>, RestoreError> {
    let text = |path: &Path| {
        path.to_str().map(str::to_owned).ok_or_else(|| {
            RestoreError::InvalidChild(
                "child path cannot be represented by launcher arguments".into(),
            )
        })
    };
    let mut arguments = vec![
        "--config-dir".into(),
        text(store.path().parent().unwrap())?,
        "--new-window".into(),
        "dontReopenProjects".into(),
        "--restore-child".into(),
        serde_json::to_string(&plan.marker).map_err(io::Error::other)?,
    ];
    for (flag, path) in [
        ("--system-dir", &global.system_dir),
        ("--log-dir", &global.log_dir),
    ] {
        if let Some(path) = path {
            arguments.extend([flag.into(), text(path)?]);
        }
    }
    if global.verbose {
        arguments.push("--verbose".into());
    }
    if global.disable_plugins {
        arguments.push("--disable-plugins".into());
    }
    if let Some(project) = &plan.project {
        arguments.push(text(project)?);
    } else {
        arguments.push("--welcome".into());
    }
    Ok(arguments)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Unknown,
    Confirmed,
    FailedBeforeLaunch,
    Exited,
    ExistingWindow,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Attempt {
    project: Option<PathBuf>,
    outcome: Outcome,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct JournalState {
    format_version: u32,
    checkpoint_generation: WindowGeneration,
    runtime_generation: WindowGeneration,
    attempts: BTreeMap<WindowOwnerId, Attempt>,
}
struct Journal {
    state: Option<JournalState>,
    raw: Value,
    revision: Option<ContentRevision>,
}
fn journal_path(store: &WindowSessionStore) -> PathBuf {
    store.path().with_file_name("window-restore-attempts.json")
}
fn read_journal_bytes(path: &Path) -> Result<Option<Vec<u8>>, RestoreError> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(metadata) if !metadata.is_file() => {
            return Err(RestoreError::InvalidJournal("not a regular file".into()));
        }
        Ok(_) => {}
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(RestoreError::InvalidJournal("not a regular file".into()));
    }
    let mut bytes = Vec::new();
    file.take(MAX_JOURNAL as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_JOURNAL {
        return Err(RestoreError::InvalidJournal(
            "attempt journal exceeds 1 MiB".into(),
        ));
    }
    Ok(Some(bytes))
}
fn load_journal(store: &WindowSessionStore) -> Result<Journal, RestoreError> {
    let Some(bytes) = read_journal_bytes(&journal_path(store))? else {
        return Ok(Journal {
            state: None,
            raw: Value::Object(Default::default()),
            revision: None,
        });
    };
    let raw: Value = serde_json::from_slice(&bytes)
        .map_err(|error| RestoreError::InvalidJournal(error.to_string()))?;
    for key in [
        "format_version",
        "checkpoint_generation",
        "runtime_generation",
        "attempts",
    ] {
        if raw.get(key).is_none() {
            return Err(RestoreError::InvalidJournal(format!(
                "missing required {key}"
            )));
        }
    }
    if let Some(attempts) = raw["attempts"].as_object() {
        for attempt in attempts.values() {
            if attempt.get("project").is_none() || attempt.get("outcome").is_none() {
                return Err(RestoreError::InvalidJournal(
                    "incomplete attempt record".into(),
                ));
            }
        }
    }
    let state: JournalState = serde_json::from_value(raw.clone())
        .map_err(|error| RestoreError::InvalidJournal(error.to_string()))?;
    if state.format_version != 1
        || state.attempts.len() > MAX_COHORT
        || state.runtime_generation.value() <= state.checkpoint_generation.value()
        || state
            .attempts
            .keys()
            .any(|owner| owner.as_str().is_empty() || owner.as_str().len() > 128)
    {
        return Err(RestoreError::InvalidJournal(
            "unsupported or oversized attempt cohort".into(),
        ));
    }
    Ok(Journal {
        state: Some(state),
        raw,
        revision: Some(ContentRevision::of(&bytes)),
    })
}
struct JournalLock {
    /// Held for its lifetime; only Unix reads its metadata back.
    #[cfg_attr(not(unix), allow(dead_code))]
    file: ipc::FileLease,
    path: PathBuf,
}
fn journal_lock(
    store: &WindowSessionStore,
    deadline: Instant,
) -> Result<JournalLock, RestoreError> {
    let path = journal_path(store).with_extension("lock");
    loop {
        if let Some(file) = ipc::try_lock_file(&path)? {
            return Ok(JournalLock { file, path });
        }
        if Instant::now() >= deadline {
            return Err(RestoreError::Busy);
        }
        std::thread::sleep(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(10)),
        );
    }
}
fn same_journal_lock(lock: &JournalLock) -> Result<bool, RestoreError> {
    let metadata = std::fs::symlink_metadata(&lock.path)?;
    if !metadata.is_file() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let held = lock.file.metadata()?;
        Ok(metadata.ino() == held.ino() && metadata.dev() == held.dev())
    }
    #[cfg(not(unix))]
    {
        Ok(true)
    }
}
fn publish_journal(
    store: &WindowSessionStore,
    mut journal: Journal,
    lock: &JournalLock,
) -> Result<(), RestoreError> {
    if !same_journal_lock(lock)? {
        return Err(RestoreError::PublicationChanged);
    }
    let state = journal
        .state
        .as_ref()
        .ok_or_else(|| RestoreError::InvalidJournal("no prepared state".into()))?;
    let current = store.load()?;
    if current.generation() != state.runtime_generation
        || current.checkpoint().map(|checkpoint| checkpoint.generation)
            != Some(state.checkpoint_generation)
    {
        return Err(RestoreError::StaleGeneration);
    }
    let encoded = serde_json::to_value(state).map_err(io::Error::other)?;
    for key in [
        "format_version",
        "checkpoint_generation",
        "runtime_generation",
    ] {
        journal.raw[key] = encoded[key].clone();
    }
    let previous = journal.raw.get("attempts").and_then(Value::as_object);
    let mut attempts = serde_json::Map::new();
    for (key, value) in encoded["attempts"].as_object().unwrap() {
        let mut record = previous
            .and_then(|previous| previous.get(key))
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default()));
        for field in ["project", "outcome"] {
            record[field] = value[field].clone();
        }
        attempts.insert(key.clone(), record);
    }
    journal.raw["attempts"] = Value::Object(attempts);
    let bytes = serde_json::to_vec_pretty(&journal.raw).map_err(io::Error::other)?;
    if bytes.len() > MAX_JOURNAL {
        return Err(RestoreError::InvalidJournal(
            "prepared attempt journal exceeds 1 MiB".into(),
        ));
    }
    let path = journal_path(store);
    let permissions = match std::fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(RestoreError::InvalidJournal("not a regular file".into()));
        }
        Ok(metadata) if metadata.permissions().readonly() => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "attempt journal is read-only",
            )
            .into());
        }
        Ok(metadata) => Some(metadata.permissions()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let temporary = path.with_file_name(format!(".window-restore-{}.tmp", ipc::new_token()?));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        if read_journal_bytes(&path)?
            .as_deref()
            .map(ContentRevision::of)
            != journal.revision
        {
            return Err(RestoreError::PublicationChanged);
        }
        let current = store.load()?;
        if current.generation() != state.runtime_generation
            || current.checkpoint().map(|checkpoint| checkpoint.generation)
                != Some(state.checkpoint_generation)
        {
            return Err(RestoreError::StaleGeneration);
        }
        if !same_journal_lock(lock)? {
            return Err(RestoreError::PublicationChanged);
        }
        window_registry::replace_file(&temporary, &path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::OpenTarget;

    fn checkpoint(config: &Path, projects: &[Option<PathBuf>]) -> WindowSessionStore {
        let store = WindowSessionStore::new(config);
        for project in projects {
            store
                .accept_window(
                    &store.load().unwrap(),
                    WindowOwnerId::new(),
                    project.as_deref(),
                )
                .unwrap();
        }
        store
            .quit_application(store.prepare_quit(&store.load().unwrap()).unwrap())
            .unwrap();
        store
    }
    fn round(config: &Path, server: &ipc::Server) -> RestoreRound {
        match prepare_startup(
            config,
            &OpenArgs::default(),
            true,
            server,
            Duration::from_secs(3),
        )
        .unwrap()
        {
            StartupRestore::Restore(round) => *round,
            _ => panic!("expected exclusive restore round"),
        }
    }
    fn publish_primary(round: &RestoreRound, server: &ipc::Server) {
        let mutation = round
            .primary()
            .accept_before_publish(server, Duration::from_secs(3))
            .unwrap();
        server.set_project(round.primary().project()).unwrap();
        round
            .primary()
            .confirm_after_publish(server, &mutation.token, Duration::from_secs(3))
            .unwrap();
    }

    #[test]
    fn explicit_skips_preserve_checkpoint_and_do_not_create_attempts() {
        let dir = tempfile::tempdir().unwrap();
        let store = checkpoint(dir.path(), &[None]);
        let bytes = std::fs::read(store.path()).unwrap();
        let server = ipc::Server::start_secondary(dir.path()).unwrap();
        let cases = [
            OpenArgs {
                welcome: true,
                ..Default::default()
            },
            OpenArgs {
                new_window: true,
                ..Default::default()
            },
            OpenArgs {
                dont_reopen_projects: true,
                ..Default::default()
            },
            OpenArgs {
                targets: vec![OpenTarget::new(dir.path())],
                ..Default::default()
            },
        ];
        for args in cases {
            assert!(matches!(
                prepare_startup(dir.path(), &args, true, &server, Duration::from_secs(2)).unwrap(),
                StartupRestore::Skipped(_)
            ));
            assert_eq!(std::fs::read(store.path()).unwrap(), bytes);
            assert!(!journal_path(&store).exists());
        }
        assert!(matches!(
            prepare_startup(
                dir.path(),
                &OpenArgs::default(),
                false,
                &server,
                Duration::from_secs(2)
            )
            .unwrap(),
            StartupRestore::Skipped(SkipReason::Disabled)
        ));
        assert_eq!(std::fs::read(store.path()).unwrap(), bytes);
    }

    #[test]
    fn another_authenticated_welcome_blocks_own_welcome_exclusion() {
        let dir = tempfile::tempdir().unwrap();
        let store = checkpoint(dir.path(), &[None]);
        let bytes = std::fs::read(store.path()).unwrap();
        let own = ipc::Server::start_secondary(dir.path()).unwrap();
        let other = ipc::Server::start_secondary(dir.path()).unwrap();
        assert_ne!(own.window_id(), other.window_id());
        assert!(matches!(
            prepare_startup(
                dir.path(),
                &OpenArgs::default(),
                true,
                &own,
                Duration::from_secs(3)
            )
            .unwrap(),
            StartupRestore::Skipped(SkipReason::ExistingWindows)
        ));
        assert_eq!(std::fs::read(store.path()).unwrap(), bytes);
    }

    #[test]
    fn corrupt_attempt_journal_preserves_checkpoint_and_runtime_generation() {
        let dir = tempfile::tempdir().unwrap();
        let store = checkpoint(dir.path(), &[None]);
        let bytes = std::fs::read(store.path()).unwrap();
        let own = ipc::Server::start_secondary(dir.path()).unwrap();
        for bad in [b"{".as_slice(), b"{}", br#"{"format_version":1}"#] {
            std::fs::write(journal_path(&store), bad).unwrap();
            assert!(matches!(
                prepare_startup(
                    dir.path(),
                    &OpenArgs::default(),
                    true,
                    &own,
                    Duration::from_secs(3)
                ),
                Err(RestoreError::InvalidJournal(_))
            ));
            assert_eq!(std::fs::read(store.path()).unwrap(), bytes);
            assert_eq!(std::fs::read(journal_path(&store)).unwrap(), bad);
        }
    }

    #[test]
    fn unknown_is_durable_before_spawn_and_failed_submission_is_not_repeated() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let store = checkpoint(dir.path(), &[Some(a), Some(b)]);
        let own = ipc::Server::start_secondary(dir.path()).unwrap();
        let mut round = round(dir.path(), &own);
        publish_primary(&round, &own);
        let child_owner = round.children[0].owner().clone();
        let mut calls = 0;
        let progress = round
            .restore_children_with(
                &own,
                &GlobalOptions::default(),
                Duration::from_secs(3),
                |_| {
                    calls += 1;
                    let journal = load_journal(&store).unwrap();
                    assert_eq!(
                        journal.state.unwrap().attempts[&child_owner].outcome,
                        Outcome::Unknown
                    );
                    Err(io::Error::other("deliberate failure before launch"))
                },
            )
            .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(progress.pending, 0);
        assert!(progress
            .issues
            .iter()
            .any(|issue| matches!(issue, RestoreIssue::SpawnFailed { .. })));
        assert_eq!(
            load_journal(&store).unwrap().state.unwrap().attempts[&child_owner].outcome,
            Outcome::FailedBeforeLaunch
        );
        assert!(matches!(
            round.restore_children_with(
                &own,
                &GlobalOptions::default(),
                Duration::from_secs(1),
                |_| panic!("must not resubmit")
            ),
            Err(RestoreError::AlreadyStarted)
        ));
    }
    #[test]
    fn resumed_round_never_retries_consumed_owner_and_rejects_changed_primary() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let store = checkpoint(dir.path(), &[Some(a), Some(b)]);
        let own = ipc::Server::start_secondary(dir.path()).unwrap();
        let mut round = round(dir.path(), &own);
        publish_primary(&round, &own);
        let first = round
            .restore_children_with(
                &own,
                &GlobalOptions::default(),
                Duration::from_secs(3),
                |_| Err(io::Error::other("fixture launch failure")),
            )
            .unwrap();
        assert_eq!(first.unsubmitted, 0);
        assert_eq!(round.unsubmitted_count(), 0);
        let journal_before = std::fs::read(journal_path(&store)).unwrap();
        let resumed = round
            .resume_children_with(
                &own,
                &GlobalOptions::default(),
                Duration::from_secs(3),
                |_| panic!("a consumed owner must never launch again"),
            )
            .unwrap();
        assert_eq!(resumed.pending, 0);
        assert_eq!(resumed.unsubmitted, 0);
        assert_eq!(std::fs::read(journal_path(&store)).unwrap(), journal_before);
        let snapshot = store.load().unwrap();
        let token = snapshot
            .live_windows()
            .iter()
            .find(|window| window.token.owner() == round.primary().owner())
            .unwrap()
            .token
            .clone();
        store.update_project(&token, None).unwrap();
        assert_eq!(store.load().unwrap().generation(), snapshot.generation());
        assert!(matches!(
            round.resume_children_with(
                &own,
                &GlobalOptions::default(),
                Duration::from_secs(3),
                |_| panic!("stale primary cannot authorize a launch"),
            ),
            Err(RestoreError::StaleGeneration)
        ));
        assert_eq!(std::fs::read(journal_path(&store)).unwrap(), journal_before);
    }

    #[test]
    fn resume_rejects_a_different_actual_parent_endpoint_before_submission() {
        let dir = tempfile::tempdir().unwrap();
        let store = checkpoint(dir.path(), &[None, None]);
        let own = ipc::Server::start_secondary(dir.path()).unwrap();
        let mut round = round(dir.path(), &own);
        publish_primary(&round, &own);
        // Establish the original endpoint binding while leaving one owner
        // unsubmitted. This is the exact state a bounded drive can retain.
        round.started = true;
        round.primary_endpoint = observed(&store, Instant::now() + Duration::from_secs(3))
            .unwrap()
            .live
            .into_iter()
            .find(|window| window.id == own.window_id());
        let other = ipc::Server::start_secondary(dir.path()).unwrap();
        let before = std::fs::read(journal_path(&store)).unwrap();
        assert!(matches!(
            round.resume_children_with(
                &other,
                &GlobalOptions::default(),
                Duration::from_secs(3),
                |_| panic!("another endpoint must never submit this round"),
            ),
            Err(RestoreError::WrongStartupEndpoint)
        ));
        assert_eq!(round.unsubmitted_count(), 1);
        assert_eq!(std::fs::read(journal_path(&store)).unwrap(), before);
    }

    #[test]
    fn held_restore_claim_prevents_a_second_startup_without_disk_changes() {
        let dir = tempfile::tempdir().unwrap();
        let store = checkpoint(dir.path(), &[None]);
        let snapshot = store.load().unwrap();
        let _claim = store
            .try_claim_restore(snapshot.checkpoint().unwrap().generation)
            .unwrap()
            .unwrap();
        let bytes = std::fs::read(store.path()).unwrap();
        let own = ipc::Server::start_secondary(dir.path()).unwrap();
        assert!(matches!(
            prepare_startup(
                dir.path(),
                &OpenArgs::default(),
                true,
                &own,
                Duration::from_secs(3)
            )
            .unwrap(),
            StartupRestore::Skipped(SkipReason::ClaimedElsewhere)
        ));
        assert_eq!(std::fs::read(store.path()).unwrap(), bytes);
    }

    #[test]
    fn missing_checkpoint_roots_remain_observable_and_unmodified() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let store = checkpoint(dir.path(), &[Some(missing.clone())]);
        let bytes = std::fs::read(store.path()).unwrap();
        let own = ipc::Server::start_secondary(dir.path()).unwrap();
        match prepare_startup(
            dir.path(),
            &OpenArgs::default(),
            true,
            &own,
            Duration::from_secs(3),
        )
        .unwrap()
        {
            StartupRestore::Uncertain(pending) => assert!(pending.attempts.iter().any(
                |issue| matches!(issue, RestoreIssue::MissingProject(path) if path == &missing)
            )),
            _ => panic!("missing root must remain observable"),
        }
        assert_eq!(std::fs::read(store.path()).unwrap(), bytes);
        assert!(!journal_path(&store).exists());
    }

    #[cfg(unix)]
    #[test]
    fn canonical_duplicate_checkpoint_roots_restore_only_one_logical_window() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&project, &alias).unwrap();
        let store = checkpoint(dir.path(), &[Some(project.clone()), Some(alias)]);
        let original = store.load().unwrap().checkpoint().unwrap().windows.clone();
        let own = ipc::Server::start_secondary(dir.path()).unwrap();
        let round = round(dir.path(), &own);
        assert_eq!(round.primary.project(), Some(project.as_path()));
        assert!(round.children.is_empty());
        assert!(round
            .issues()
            .iter()
            .any(|issue| matches!(issue, RestoreIssue::DuplicateProject(_))));
        assert_eq!(
            store.load().unwrap().checkpoint().unwrap().windows,
            original
        );
    }

    #[test]
    fn marker_requires_exact_pending_intent_and_rejects_target_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let store = checkpoint(dir.path(), &[Some(a), Some(b.clone())]);
        let own = ipc::Server::start_secondary(dir.path()).unwrap();
        let mut round = round(dir.path(), &own);
        publish_primary(&round, &own);
        let plan = &round.children[0];
        let cli =
            crate::args::parse(child_arguments(&store, plan, &GlobalOptions::default()).unwrap())
                .unwrap();
        let crate::args::Command::Open(args) = cli.command else {
            panic!("child must parse as Open")
        };
        let child = ipc::Server::start_secondary(dir.path()).unwrap();
        assert!(matches!(
            prepare_startup(dir.path(), &args, true, &child, Duration::from_secs(3)),
            Err(RestoreError::InvalidChild(_))
        ));
        round
            .record(
                plan,
                Outcome::Unknown,
                Instant::now() + Duration::from_secs(3),
            )
            .unwrap();
        let mut wrong = args.clone();
        wrong.targets = vec![OpenTarget::new(dir.path())];
        assert!(matches!(
            prepare_startup(dir.path(), &wrong, true, &child, Duration::from_secs(3)),
            Err(RestoreError::InvalidChild(_))
        ));
        let StartupRestore::Child(checked) =
            prepare_startup(dir.path(), &args, true, &child, Duration::from_secs(3)).unwrap()
        else {
            panic!("expected validated child")
        };
        assert_eq!(checked.project(), Some(b.as_path()));
        let accepted = checked
            .accept_before_publish(&child, Duration::from_secs(3))
            .unwrap();
        assert!(checked
            .accept_before_publish(&child, Duration::from_secs(3))
            .is_err());
        child.set_project(checked.project()).unwrap();
        let confirmer = checked.clone();
        let child = std::thread::scope(|scope| {
            let acknowledgment = scope.spawn(move || {
                let result = confirmer.confirm_after_publish(
                    &child,
                    &accepted.token,
                    Duration::from_secs(3),
                );
                (child, result)
            });
            let deadline = Instant::now() + Duration::from_secs(3);
            while round.confirmed_endpoints.is_empty() && Instant::now() < deadline {
                round.accept_receipts(deadline).unwrap();
                std::thread::sleep(Duration::from_millis(5));
            }
            let (child, result) = acknowledgment.join().unwrap();
            result.unwrap();
            child
        });
        assert_eq!(
            round.confirmed_endpoints[checked.owner()].0.id,
            child.window_id()
        );
        assert_eq!(
            load_journal(&store).unwrap().state.unwrap().attempts[checked.owner()].outcome,
            Outcome::Confirmed
        );
    }
    #[test]
    fn durable_unknown_blocks_a_new_round_after_claim_release() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let store = checkpoint(dir.path(), &[Some(a), Some(b)]);
        let own = ipc::Server::start_secondary(dir.path()).unwrap();
        let round = round(dir.path(), &own);
        // A coordinator crash after durable intent but before a child proof must
        // remain uncertain even after both endpoint and OS claim are released.
        round
            .record(
                &round.children[0],
                Outcome::Unknown,
                Instant::now() + Duration::from_secs(3),
            )
            .unwrap();
        let bytes = std::fs::read(store.path()).unwrap();
        let attempts = std::fs::read(journal_path(&store)).unwrap();
        drop(round);
        drop(own);
        let restarted = ipc::Server::start_secondary(dir.path()).unwrap();
        match prepare_startup(
            dir.path(),
            &OpenArgs::default(),
            true,
            &restarted,
            Duration::from_secs(3),
        )
        .unwrap()
        {
            StartupRestore::Uncertain(pending) => assert_eq!(pending.attempts.len(), 1),
            _ => panic!("unknown child must not be blindly resubmitted"),
        }
        assert_eq!(std::fs::read(store.path()).unwrap(), bytes);
        assert_eq!(std::fs::read(journal_path(&store)).unwrap(), attempts);
    }
    #[cfg(unix)]
    #[test]
    fn authenticated_child_process_helper() {
        let Some(arguments_file) = std::env::var_os("RUSTPY_RESTORE_TEST_ARGUMENTS") else {
            return;
        };
        let arguments_file = PathBuf::from(arguments_file);
        let arguments: Vec<String> =
            serde_json::from_slice(&std::fs::read(&arguments_file).unwrap()).unwrap();
        let cli = crate::args::parse(arguments).unwrap();
        let crate::args::Command::Open(args) = cli.command else {
            panic!("child must parse Open")
        };
        let config = cli.global.config_dir.unwrap();
        // Deterministic newborn interval: an actual process owns an OS lease
        // before publishing any endpoint, exactly the registry's uncertain case.
        let starting = config.join("windows").join(ipc::new_token().unwrap());
        std::fs::create_dir_all(&starting).unwrap();
        let lease = ipc::try_lock_file(&starting.join("lease"))
            .unwrap()
            .unwrap();
        std::fs::write(
            arguments_file.with_extension("starting"),
            b"lease held, endpoint unpublished",
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(800));
        drop(lease);
        std::fs::remove_file(starting.join("lease")).unwrap();
        std::fs::remove_dir(starting).unwrap();
        let server = ipc::Server::start_secondary(&config).unwrap();
        let StartupRestore::Child(plan) =
            prepare_startup(&config, &args, true, &server, Duration::from_secs(5)).unwrap()
        else {
            panic!("child must not initiate fan-out")
        };
        let mutation = plan
            .accept_before_publish(&server, Duration::from_secs(5))
            .unwrap();
        server.set_project(plan.project()).unwrap();
        plan.confirm_after_publish(&server, &mutation.token, Duration::from_secs(5))
            .unwrap();
        std::fs::write(
            arguments_file.with_extension("ready"),
            b"exact server and token confirmed",
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let release = PathBuf::from(std::env::var_os("RUSTPY_RESTORE_TEST_RELEASE").unwrap());
        while !release.exists() {
            assert!(
                Instant::now() < deadline,
                "parent did not release child fixture"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(server);
        std::fs::write(arguments_file.with_extension("done"), b"endpoint released").unwrap();
    }

    #[cfg(unix)]
    struct TransportFixture {
        round: RestoreRound,
        release: PathBuf,
    }
    #[cfg(unix)]
    impl TransportFixture {
        fn finish(&mut self) -> io::Result<Vec<std::process::ExitStatus>> {
            std::fs::write(&self.release, b"release")?;
            let mut processes = std::mem::take(&mut self.round.finished_children);
            processes.extend(self.round.pending.drain(..).map(|pending| pending.process));
            let mut statuses = Vec::new();
            let mut failure = None;
            for mut process in processes {
                match process.wait() {
                    Ok(status) => statuses.push(status),
                    Err(error) => {
                        if failure.is_none() {
                            failure = Some(error);
                        }
                    }
                }
            }
            match failure {
                Some(error) => Err(error),
                None => Ok(statuses),
            }
        }
    }
    #[cfg(unix)]
    impl Drop for TransportFixture {
        fn drop(&mut self) {
            let _ = self.finish();
        }
    }
    #[cfg(unix)]
    fn actual_transport(child_count: usize, short_first_round: bool) -> Vec<Option<PathBuf>> {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let projects: Vec<_> = (0..=child_count)
            .map(|index| dir.path().join(format!("project-{index}")))
            .collect();
        for project in &projects {
            std::fs::create_dir(project).unwrap();
        }
        let store = checkpoint(
            dir.path(),
            &projects.iter().cloned().map(Some).collect::<Vec<_>>(),
        );
        let own = ipc::Server::start_secondary(dir.path()).unwrap();
        let round = round(dir.path(), &own);
        publish_primary(&round, &own);
        let release = dir.path().join("release");
        let mut fixture = TransportFixture {
            round,
            release: release.clone(),
        };
        let arguments_base = dir.path().join("child-arguments");
        let script = dir.path().join("child-launcher");
        let exe = std::env::current_exe().unwrap();
        let script_text = format!(
            "#!/usr/bin/python3\nimport json,os,sys\nowner=json.loads(sys.argv[sys.argv.index('--restore-child')+1])['owner']\npath={}+'-'+owner+'.json'\nwith open(path,'w') as f: json.dump(sys.argv[1:],f)\nos.environ['RUSTPY_RESTORE_TEST_ARGUMENTS']=path\nos.environ['RUSTPY_RESTORE_TEST_RELEASE']={}\nos.environ.pop('DISPLAY',None)\nos.environ.pop('WAYLAND_DISPLAY',None)\nos.execv({},[{},'--exact','window_sessions::tests::authenticated_child_process_helper','--nocapture'])\n",
            serde_json::to_string(&arguments_base).unwrap(),
            serde_json::to_string(&release).unwrap(),
            serde_json::to_string(&exe).unwrap(),
            serde_json::to_string(&exe).unwrap()
        );
        std::fs::write(&script, script_text).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let first_deadline = if short_first_round {
            Duration::from_millis(300)
        } else {
            Duration::from_secs(10)
        };
        let mut result = fixture.round.restore_children(
            &own,
            &script,
            &GlobalOptions::default(),
            dir.path(),
            first_deadline,
        );
        if short_first_round {
            let unresolved = result.as_ref().unwrap();
            assert_eq!(unresolved.pending, 1);
            assert!(unresolved.confirmed.is_empty());
            assert!(unresolved
                .issues
                .iter()
                .any(|issue| matches!(issue, RestoreIssue::Unknown { .. })));
            assert!(matches!(
                fixture.round.restore_children(
                    &own,
                    &script,
                    &GlobalOptions::default(),
                    dir.path(),
                    Duration::from_secs(1)
                ),
                Err(RestoreError::AlreadyStarted)
            ));
            // Poll the same owned process; there is no second launch.
            result = fixture.round.poll_children(Duration::from_secs(10));
        }
        // RAII repeats release/reaping on any assertion or error path. Successful
        // and pending owned processes are reaped before result is unwrapped.
        let statuses = fixture.finish().unwrap();
        let progress = result.unwrap();
        assert_eq!(statuses.len(), child_count);
        assert!(statuses.iter().all(|status| status.success()));
        assert_eq!(progress.pending, 0);
        assert_eq!(
            progress.confirmed,
            projects[1..].iter().cloned().map(Some).collect::<Vec<_>>()
        );
        assert_eq!(fixture.round.confirmed_endpoints.len(), child_count);
        let distinct: BTreeSet<_> = fixture
            .round
            .confirmed_endpoints
            .values()
            .map(|(window, _)| window.id.clone())
            .collect();
        assert_eq!(distinct.len(), child_count);
        for plan in &fixture.round.children {
            let path = dir
                .path()
                .join(format!("child-arguments-{}.json", plan.owner().as_str()));
            assert!(path.with_extension("ready").exists());
            assert!(path.with_extension("done").exists());
        }
        assert_eq!(store.load().unwrap().live_windows().len(), child_count + 1);
        // Production durable data has no endpoint identity or nonce.
        let raw = std::fs::read_to_string(journal_path(&store)).unwrap();
        for (window, _) in fixture.round.confirmed_endpoints.values() {
            assert!(!raw.contains(&window.id));
            assert!(!raw.contains(&window.endpoint.token));
        }
        progress.confirmed
    }
    #[cfg(unix)]
    #[test]
    fn actual_detached_transport_requires_child_authored_endpoint_confirmation() {
        assert_eq!(actual_transport(1, false).len(), 1);
    }
    #[cfg(unix)]
    #[test]
    fn actual_three_window_restore_waits_for_exact_children_once() {
        assert_eq!(actual_transport(2, false).len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn actual_newborn_unknown_is_polled_without_resubmission() {
        assert_eq!(actual_transport(1, true).len(), 1);
    }
    #[test]
    fn restore_checkpoint_preserves_distinct_welcome_logical_owners() {
        let dir = tempfile::tempdir().unwrap();
        let projects = [dir.path().join("project-0"), dir.path().join("project-1")];
        for project in &projects {
            std::fs::create_dir(project).unwrap();
        }
        let expected_projects = vec![
            Some(projects[0].clone()),
            Some(projects[1].clone()),
            None,
            None,
        ];
        let store = checkpoint(dir.path(), &expected_projects);
        let checkpoint = store.load().unwrap().checkpoint().unwrap().clone();
        assert_eq!(checkpoint.windows.len(), 4);
        let welcome_owners: Vec<_> = checkpoint
            .windows
            .iter()
            .filter(|window| window.project.is_none())
            .map(|window| window.owner.clone())
            .collect();
        assert_eq!(welcome_owners.len(), 2);
        assert_ne!(welcome_owners[0], welcome_owners[1]);
        // The real TCP Server and Store feed production prepare_startup; no
        // restoration planner is substituted by the fixture.
        let own = ipc::Server::start_secondary(dir.path()).unwrap();
        let round = round(dir.path(), &own);
        let actual: Vec<_> = std::iter::once(round.primary())
            .chain(round.children.iter())
            .map(|plan| (plan.owner().clone(), plan.project.clone()))
            .collect();
        let expected: Vec<_> = checkpoint
            .windows
            .iter()
            .map(|window| (window.owner.clone(), window.project.clone()))
            .collect();
        assert_eq!(actual, expected);
        assert!(!round
            .issues()
            .iter()
            .any(|issue| matches!(issue, RestoreIssue::DuplicateProject(None))));
    }

    // A short drive must not strand the remaining logical owners after the
    // first real OS child spends the budget in unpublished registry presence.
    #[cfg(unix)]
    #[test]
    fn actual_bounded_restore_resumes_deferred_logical_owners_once() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let projects: Vec<_> = (0..4)
            .map(|index| dir.path().join(format!("project-{index}")))
            .collect();
        for project in &projects {
            std::fs::create_dir(project).unwrap();
        }
        let expected: Vec<_> = projects.iter().cloned().map(Some).collect();
        let store = checkpoint(dir.path(), &expected);
        let own = ipc::Server::start_secondary(dir.path()).unwrap();
        let round = round(dir.path(), &own);
        publish_primary(&round, &own);
        let owners: Vec<_> = round
            .children
            .iter()
            .map(|plan| plan.owner().clone())
            .collect();
        assert_eq!(owners.len(), 3);
        let release = dir.path().join("release");
        let mut fixture = TransportFixture {
            round,
            release: release.clone(),
        };
        let arguments_base = dir.path().join("child-arguments");
        let launches = dir.path().join("launches");
        let script = dir.path().join("child-launcher");
        let exe = std::env::current_exe().unwrap();
        let script_text = format!(
            "#!/usr/bin/python3\nimport json,os,sys\nowner=json.loads(sys.argv[sys.argv.index('--restore-child')+1])['owner']\npath={}+'-'+owner+'.json'\nwith open({},'a') as f: f.write(owner+'\\n')\nwith open(path,'w') as f: json.dump(sys.argv[1:],f)\nos.environ['RUSTPY_RESTORE_TEST_ARGUMENTS']=path\nos.environ['RUSTPY_RESTORE_TEST_RELEASE']={}\nos.environ.pop('DISPLAY',None)\nos.environ.pop('WAYLAND_DISPLAY',None)\nos.execv({},[{},'--exact','window_sessions::tests::authenticated_child_process_helper','--nocapture'])\n",
            serde_json::to_string(&arguments_base).unwrap(),
            serde_json::to_string(&launches).unwrap(),
            serde_json::to_string(&release).unwrap(),
            serde_json::to_string(&exe).unwrap(),
            serde_json::to_string(&exe).unwrap()
        );
        std::fs::write(&script, script_text).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let first = fixture
            .round
            .restore_children(
                &own,
                &script,
                &GlobalOptions::default(),
                dir.path(),
                Duration::from_millis(300),
            )
            .unwrap();
        assert_eq!(first.pending, 1);
        assert!(first.confirmed.is_empty());
        assert_eq!(
            first
                .issues
                .iter()
                .filter(|issue| matches!(issue, RestoreIssue::Deferred(_)))
                .count(),
            2
        );
        let journal = load_journal(&store).unwrap();
        let attempts = &journal.state.as_ref().unwrap().attempts;
        assert_eq!(attempts.len(), 2);
        assert_eq!(
            attempts
                .get(fixture.round.primary().owner())
                .unwrap()
                .outcome,
            Outcome::Confirmed
        );
        assert_eq!(attempts.get(&owners[0]).unwrap().outcome, Outcome::Unknown);
        assert!(owners[1..]
            .iter()
            .all(|owner| !attempts.contains_key(owner)));

        // Explicit continuation authenticates the existing Unknown child,
        // then submits the two previously Deferred owners in the same round.
        let later = fixture.round.resume_children(
            &own,
            &script,
            &GlobalOptions::default(),
            dir.path(),
            Duration::from_secs(10),
        );
        let journal = load_journal(&store).unwrap();
        let live = store.load().unwrap().live_windows().to_vec();
        let submitted: Vec<_> = std::fs::read_to_string(&launches)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        let statuses = fixture.finish().unwrap();
        assert_eq!(later.unwrap().pending, 0);
        assert_eq!(
            submitted,
            owners
                .iter()
                .map(|owner| owner.as_str().to_owned())
                .collect::<Vec<_>>()
        );
        assert_eq!(statuses.len(), 3);
        assert!(statuses.iter().all(|status| status.success()));
        assert_eq!(fixture.round.confirmed_endpoints.len(), 3);
        assert_eq!(live.len(), 4);
        for owner in &owners {
            assert_eq!(
                journal
                    .state
                    .as_ref()
                    .unwrap()
                    .attempts
                    .get(owner)
                    .unwrap()
                    .outcome,
                Outcome::Confirmed
            );
            let member = live
                .iter()
                .find(|window| window.token.owner() == owner)
                .unwrap();
            let checkpoint_member = store
                .load()
                .unwrap()
                .checkpoint()
                .unwrap()
                .windows
                .iter()
                .find(|window| &window.owner == owner)
                .unwrap()
                .clone();
            assert_eq!(member.project, checkpoint_member.project);
        }
    }

    #[test]
    fn checked_attempt_publication_retains_unknown_fields_and_rejects_stale_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let store = checkpoint(dir.path(), &[None]);
        let own = ipc::Server::start_secondary(dir.path()).unwrap();
        let round = round(dir.path(), &own);
        let path = journal_path(&store);
        let mut raw: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        raw["future_round"] = serde_json::json!({"retain": [1,2,3]});
        std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();
        round
            .record(
                round.primary(),
                Outcome::Unknown,
                Instant::now() + Duration::from_secs(3),
            )
            .unwrap();
        let raw: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(raw["future_round"], serde_json::json!({"retain": [1,2,3]}));
        let lock = journal_lock(&store, Instant::now() + Duration::from_secs(3)).unwrap();
        let stale = load_journal(&store).unwrap();
        let mut changed = raw;
        changed["future_round"]["retain"] = serde_json::json!([4]);
        let external_bytes = serde_json::to_vec(&changed).unwrap();
        std::fs::write(&path, &external_bytes).unwrap();
        let checkpoint_bytes = std::fs::read(store.path()).unwrap();
        assert!(matches!(
            publish_journal(&store, stale, &lock),
            Err(RestoreError::PublicationChanged)
        ));
        assert_eq!(std::fs::read(&path).unwrap(), external_bytes);
        assert_eq!(std::fs::read(store.path()).unwrap(), checkpoint_bytes);
    }
    #[test]
    fn receipt_nonce_cannot_be_replayed_into_this_restore_round() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let store = checkpoint(dir.path(), &[Some(a), Some(b)]);
        let own = ipc::Server::start_secondary(dir.path()).unwrap();
        let mut round = round(dir.path(), &own);
        publish_primary(&round, &own);
        let plan = round.children[0].clone();
        round
            .record(
                &plan,
                Outcome::Unknown,
                Instant::now() + Duration::from_secs(3),
            )
            .unwrap();
        let child = ipc::Server::start_secondary(dir.path()).unwrap();
        let accepted = plan
            .accept_before_publish(&child, Duration::from_secs(3))
            .unwrap();
        child.set_project(plan.project()).unwrap();
        // A durable confirmation alone cannot supply the live parent nonce.
        round
            .record(
                &plan,
                Outcome::Confirmed,
                Instant::now() + Duration::from_secs(3),
            )
            .unwrap();
        let window = observed(&store, Instant::now() + Duration::from_secs(3))
            .unwrap()
            .live
            .into_iter()
            .find(|window| window.id == child.window_id())
            .unwrap();
        let mut marker = plan.marker.clone();
        marker.receipt.as_mut().unwrap().nonce = ipc::new_token().unwrap();
        let receipt = ChildReceipt {
            config: dir.path().into(),
            marker,
            project: plan.project.clone(),
            token: accepted.token,
            window,
        };
        let port = round.receipt_listener.local_addr().unwrap().port();
        std::thread::scope(|scope| {
            let sender = scope.spawn(move || {
                let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
                write_message(
                    &mut stream,
                    &serde_json::to_vec(&receipt).unwrap(),
                    Instant::now() + Duration::from_secs(3),
                )
                .unwrap();
            });
            let deadline = Instant::now() + Duration::from_secs(3);
            let error = loop {
                match round.accept_receipts(deadline) {
                    Err(error) => break error,
                    Ok(()) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Ok(()) => panic!("forged receipt was not rejected"),
                }
            };
            assert!(matches!(error, RestoreError::InvalidChild(_)));
            sender.join().unwrap();
        });
        assert!(round.confirmed_endpoints.is_empty());
    }

    #[test]
    fn another_endpoint_at_confirmed_project_does_not_share_runtime_identity() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let store = checkpoint(dir.path(), &[Some(a), Some(b)]);
        let own = ipc::Server::start_secondary(dir.path()).unwrap();
        let mut round = round(dir.path(), &own);
        publish_primary(&round, &own);
        let primary = observed(&store, Instant::now() + Duration::from_secs(3))
            .unwrap()
            .live
            .into_iter()
            .find(|window| window.id == own.window_id())
            .unwrap();
        round.primary_endpoint = Some(primary);
        let plan = round.children[0].clone();
        round
            .record(
                &plan,
                Outcome::Unknown,
                Instant::now() + Duration::from_secs(3),
            )
            .unwrap();
        let child = ipc::Server::start_secondary(dir.path()).unwrap();
        let accepted = plan
            .accept_before_publish(&child, Duration::from_secs(3))
            .unwrap();
        child.set_project(plan.project()).unwrap();
        let confirmer = plan.clone();
        let _child = std::thread::scope(|scope| {
            let sender = scope.spawn(move || {
                let result = confirmer.confirm_after_publish(
                    &child,
                    &accepted.token,
                    Duration::from_secs(3),
                );
                (child, result)
            });
            let deadline = Instant::now() + Duration::from_secs(3);
            while round.confirmed_endpoints.is_empty() && Instant::now() < deadline {
                round.accept_receipts(deadline).unwrap();
                std::thread::sleep(Duration::from_millis(5));
            }
            let (child, result) = sender.join().unwrap();
            result.unwrap();
            child
        });
        let foreign = ipc::Server::start_secondary(dir.path()).unwrap();
        foreign.set_project(plan.project()).unwrap();
        let presence = observed(&store, Instant::now() + Duration::from_secs(3)).unwrap();
        match round.require_known_presence(presence) {
            Err(RestoreError::RegistryUncertain(entries)) => assert!(entries
                .iter()
                .any(|entry| entry.id.as_deref() == Some(foreign.window_id().as_str()))),
            _ => panic!("same project cannot substitute another authenticated endpoint"),
        }
    }
    fn accepted_pair(
        config: &Path,
    ) -> (WindowSessionStore, RestoreRound, ipc::Server, ipc::Server) {
        let a = config.join("a");
        let b = config.join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let store = checkpoint(config, &[Some(a), Some(b)]);
        let own = ipc::Server::start_secondary(config).unwrap();
        let mut round = round(config, &own);
        publish_primary(&round, &own);
        round.primary_endpoint = observed(&store, Instant::now() + Duration::from_secs(3))
            .unwrap()
            .live
            .into_iter()
            .find(|window| window.id == own.window_id());
        let plan = round.children[0].clone();
        round
            .record(
                &plan,
                Outcome::Unknown,
                Instant::now() + Duration::from_secs(3),
            )
            .unwrap();
        let child = ipc::Server::start_secondary(config).unwrap();
        let accepted = plan
            .accept_before_publish(&child, Duration::from_secs(3))
            .unwrap();
        child.set_project(plan.project()).unwrap();
        let child = std::thread::scope(|scope| {
            let sender = scope.spawn(move || {
                let result =
                    plan.confirm_after_publish(&child, &accepted.token, Duration::from_secs(3));
                (child, result)
            });
            let deadline = Instant::now() + Duration::from_secs(3);
            while round.confirmed_endpoints.is_empty() && Instant::now() < deadline {
                round.accept_receipts(deadline).unwrap();
                std::thread::sleep(Duration::from_millis(5));
            }
            let (child, result) = sender.join().unwrap();
            result.unwrap();
            child
        });
        (store, round, own, child)
    }
    #[test]
    fn current_membership_red_retired_child_with_live_authenticated_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let (store, round, _own, child) = accepted_pair(dir.path());
        let generation = store.load().unwrap().generation();
        let token = round.confirmed_endpoints.values().next().unwrap().1.clone();
        store.close_window(&token).unwrap();
        assert_eq!(store.load().unwrap().generation(), generation);
        let presence = observed(&store, Instant::now() + Duration::from_secs(3)).unwrap();
        assert!(presence
            .live
            .iter()
            .any(|window| window.id == child.window_id()));
        assert!(
            matches!(
                round.require_known_presence(presence),
                Err(RestoreError::RegistryUncertain(_))
            ),
            "retired owner cannot remain an allowed endpoint"
        );
    }
    #[test]
    fn current_membership_red_changed_primary_with_live_authenticated_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let (store, round, own, _child) = accepted_pair(dir.path());
        let snapshot = store.load().unwrap();
        let generation = snapshot.generation();
        let token = snapshot
            .live_windows()
            .into_iter()
            .find(|window| window.token.owner() == round.primary.owner())
            .unwrap()
            .token;
        store.update_project(&token, None).unwrap();
        assert_eq!(store.load().unwrap().generation(), generation);
        let presence = observed(&store, Instant::now() + Duration::from_secs(3)).unwrap();
        assert!(presence
            .live
            .iter()
            .any(|window| window.id == own.window_id()));
        assert!(
            matches!(
                round.require_known_presence(presence),
                Err(RestoreError::RegistryUncertain(_))
            ),
            "changed accepted primary token cannot remain allowed"
        );
    }
}
