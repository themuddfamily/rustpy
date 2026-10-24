//! Live logical window ownership and a separate durable restart checkpoint.
//!
//! Loading, ordinary window updates and restore claims never erase a checkpoint.
//! Quit requires an unchanged accepted cohort; caller-owned UI/IPC save guards
//! must succeed before publication. OS locks coordinate cooperating processes,
//! not arbitrary external writers in the final check/replace interval.
use rustpy_core::vfs::{normalize_path, ContentRevision};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{hash_map::RandomState, BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    hash::BuildHasher,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// An opaque logical identity, never evidence that an OS process is alive.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WindowOwnerId(String);
impl WindowOwnerId {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let input = (
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        );
        let a = RandomState::new().hash_one(input);
        let b = RandomState::new().hash_one(input);
        Self(format!("{a:016x}{b:016x}"))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl Default for WindowOwnerId {
    fn default() -> Self {
        Self::new()
    }
}
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WindowGeneration(u64);
impl WindowGeneration {
    pub fn value(self) -> u64 {
        self.0
    }
    fn next(self) -> Result<Self, WindowSessionError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(WindowSessionError::Exhausted)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowToken {
    config: PathBuf,
    generation: WindowGeneration,
    owner: WindowOwnerId,
    revision: u64,
}
impl WindowToken {
    pub fn generation(&self) -> WindowGeneration {
        self.generation
    }
    pub fn owner(&self) -> &WindowOwnerId {
        &self.owner
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveWindow {
    pub token: WindowToken,
    pub project: Option<PathBuf>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointWindow {
    pub owner: WindowOwnerId,
    pub project: Option<PathBuf>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestartCheckpoint {
    pub generation: WindowGeneration,
    pub windows: Vec<CheckpointWindow>,
}
/// UI approval is opaque to the store. The caller must freeze actual documents,
/// notebooks, workspace choices and ownership before supplying this stamp.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuitUiGuard {
    pub nonce: String,
    pub state: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardedQuitWindow {
    pub owner: WindowOwnerId,
    pub generation: WindowGeneration,
    pub owner_revision: u64,
    pub window_id: String,
    pub project: Option<PathBuf>,
    pub guard: QuitUiGuard,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardedQuitBinding {
    pub transaction_id: String,
    /// Must be in the accepted logical cohort order, including Welcome windows.
    pub cohort: Vec<GuardedQuitWindow>,
}
/// This transportable value is not authorization until checked against the
/// configured store and the still-frozen local UI guard.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardedQuitCertificate {
    pub transaction_id: String,
    pub checkpoint_generation: WindowGeneration,
    pub checkpoint_revision: String,
    pub cohort: Vec<GuardedQuitWindow>,
}
/// Constructed only after reading an exact, currently effective persisted
/// binding. It proves store state at verification time, not future UI stability.
#[derive(Clone, Debug)]
pub struct VerifiedQuitCommit {
    config: PathBuf,
    certificate: GuardedQuitCertificate,
}
impl VerifiedQuitCommit {
    pub fn configuration(&self) -> &Path {
        &self.config
    }
    pub fn certificate(&self) -> &GuardedQuitCertificate {
        &self.certificate
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowProjectStatus {
    Directory,
    Missing,
    NotDirectory,
    Unreadable(String),
}
impl LiveWindow {
    pub fn project_status(&self) -> Option<WindowProjectStatus> {
        self.project.as_deref().map(project_status)
    }
}
impl CheckpointWindow {
    pub fn project_status(&self) -> Option<WindowProjectStatus> {
        self.project.as_deref().map(project_status)
    }
}
fn project_status(path: &Path) -> WindowProjectStatus {
    match std::fs::metadata(path) {
        Ok(m) if m.is_dir() => WindowProjectStatus::Directory,
        Ok(_) => WindowProjectStatus::NotDirectory,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => WindowProjectStatus::Missing,
        Err(e) => WindowProjectStatus::Unreadable(e.to_string()),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct WindowRecord {
    revision: u64,
    order: u64,
    project: Option<PathBuf>,
    closed: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct State {
    format_version: u32,
    generation: WindowGeneration,
    next_order: u64,
    live: BTreeMap<WindowOwnerId, WindowRecord>,
    checkpoint: Option<RestartCheckpoint>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            format_version: 1,
            generation: WindowGeneration::default(),
            next_order: 0,
            live: BTreeMap::new(),
            checkpoint: None,
        }
    }
}
#[derive(Clone, Debug)]
pub struct WindowSessionSnapshot {
    config: PathBuf,
    state: State,
    raw: Value,
    revision: Option<ContentRevision>,
    guarded_quit: Option<GuardedQuitCertificate>,
}
impl WindowSessionSnapshot {
    pub fn generation(&self) -> WindowGeneration {
        self.state.generation
    }
    pub fn revision(&self) -> Option<ContentRevision> {
        self.revision
    }
    pub fn checkpoint(&self) -> Option<&RestartCheckpoint> {
        self.state.checkpoint.as_ref()
    }
    /// Checked receipt for the persisted checkpoint, including historical
    /// checkpoints after a new runtime generation starts. This is NOT Commit
    /// authorization; use verify_guarded_quit and the actual frozen UI guard.
    pub fn guarded_quit_certificate(&self) -> Option<&GuardedQuitCertificate> {
        self.guarded_quit.as_ref()
    }
    pub fn live_windows(&self) -> Vec<LiveWindow> {
        let mut records: Vec<_> = self
            .state
            .live
            .iter()
            .filter(|(_, record)| !record.closed)
            .collect();
        records.sort_by(|(a, ra), (b, rb)| ra.order.cmp(&rb.order).then_with(|| a.cmp(b)));
        records
            .into_iter()
            .map(|(owner, record)| LiveWindow {
                token: WindowToken {
                    config: self.config.clone(),
                    generation: self.state.generation,
                    owner: owner.clone(),
                    revision: record.revision,
                },
                project: record.project.clone(),
            })
            .collect()
    }
}
#[derive(Debug)]
pub struct WindowMutation {
    pub snapshot: WindowSessionSnapshot,
    pub token: WindowToken,
}
#[derive(Debug)]
pub struct QuitPlan {
    config: PathBuf,
    generation: WindowGeneration,
    cohort: Vec<LiveWindow>,
    revision: Option<ContentRevision>,
}
impl QuitPlan {
    pub fn generation(&self) -> WindowGeneration {
        self.generation
    }
    pub fn cohort(&self) -> &[LiveWindow] {
        &self.cohort
    }
}
#[derive(Debug)]
pub struct RestoreClaim {
    config: PathBuf,
    owner: Arc<()>,
    checkpoint_generation: WindowGeneration,
    live_generation: WindowGeneration,
    revision: Option<ContentRevision>,
    cohort: Vec<LiveWindow>,
    begun: bool,
    _lock: OwnedLock,
}
impl RestoreClaim {
    pub fn generation(&self) -> WindowGeneration {
        self.checkpoint_generation
    }
}
#[derive(Debug)]
pub enum WindowSessionError {
    Io {
        path: PathBuf,
        error: std::io::Error,
    },
    Invalid {
        path: PathBuf,
        reason: String,
    },
    Busy,
    WrongConfiguration,
    StaleGeneration {
        expected: WindowGeneration,
        actual: WindowGeneration,
    },
    StaleOwner,
    ClosedOwner,
    DuplicateOwner,
    CohortChanged,
    MissingCheckpoint,
    RestoreAlreadyBegun,
    ClaimLost,
    Exhausted,
    ExternalChange,
    /// Replacement may already be visible. Never authorize Abort from this
    /// error: retain the guards and retry the SAME transaction to reconcile.
    QuitCommitUncertain {
        certificate: GuardedQuitCertificate,
        error: Box<WindowSessionError>,
    },
    /// Internal/publication diagnostic; guarded Quit wraps it with its receipt.
    PublicationUncertain {
        path: PathBuf,
        error: std::io::Error,
    },
}
impl std::fmt::Display for WindowSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, error } => write!(f, "cannot access {}: {error}", path.display()),
            Self::Invalid { path, reason } => write!(
                f,
                "{} is not valid: {reason}; left unchanged",
                path.display()
            ),
            Self::Busy => write!(f, "window session is busy in another process"),
            Self::WrongConfiguration => {
                write!(f, "window ownership belongs to another configuration")
            }
            Self::StaleGeneration { expected, actual } => write!(
                f,
                "window session generation changed (expected {}, actual {})",
                expected.0, actual.0
            ),
            Self::StaleOwner => write!(f, "window ownership revision changed or no longer exists"),
            Self::ClosedOwner => write!(f, "window was already closed"),
            Self::DuplicateOwner => write!(f, "window identity was already accepted"),
            Self::CohortChanged => write!(
                f,
                "accepted windows changed after the quit or restore plan was prepared"
            ),
            Self::MissingCheckpoint => write!(f, "there is no window restart checkpoint"),
            Self::RestoreAlreadyBegun => {
                write!(f, "this restore claim already began a startup round")
            }
            Self::ClaimLost => write!(f, "window restore claim was replaced"),
            Self::Exhausted => write!(f, "window session generation or revision is exhausted"),
            Self::ExternalChange => write!(f, "window session was changed by an external writer"),
            Self::PublicationUncertain { path, error } => write!(
                f,
                "{} was replaced but durable publication is uncertain: {error}",
                path.display()
            ),
            Self::QuitCommitUncertain { certificate, error } => write!(
                f,
                "Quit transaction {} may be committed; retry it without aborting: {error}",
                certificate.transaction_id
            ),
        }
    }
}
impl std::error::Error for WindowSessionError {}
fn io(path: &Path, error: std::io::Error) -> WindowSessionError {
    WindowSessionError::Io {
        path: path.into(),
        error,
    }
}

/// Clones share one coordinator identity; independently constructed stores cannot
/// borrow its restore claim. Merely constructing or loading a store never writes.
#[derive(Clone, Debug)]
pub struct WindowSessionStore {
    config: PathBuf,
    owner: Arc<()>,
}
impl WindowSessionStore {
    pub fn new(config: impl AsRef<Path>) -> Self {
        Self {
            config: normalize_path(config.as_ref()),
            owner: Arc::new(()),
        }
    }
    pub fn path(&self) -> PathBuf {
        self.config.join("window-session.json")
    }
    pub fn load(&self) -> Result<WindowSessionSnapshot, WindowSessionError> {
        let path = self.path();
        let bytes = read_checked(&path)?;
        let (raw, revision) = match bytes {
            None => (Value::Object(Default::default()), None),
            Some(bytes) => {
                let raw: Value =
                    serde_json::from_slice(&bytes).map_err(|e| WindowSessionError::Invalid {
                        path: path.clone(),
                        reason: e.to_string(),
                    })?;
                if !raw.is_object() {
                    return Err(WindowSessionError::Invalid {
                        path,
                        reason: "expected an object".into(),
                    });
                }
                (raw, Some(ContentRevision::of(&bytes)))
            }
        };
        let mut state: State = if revision.is_none() {
            State::default()
        } else {
            validate_required_keys(&raw, &path)?;
            serde_json::from_value(raw.clone()).map_err(|e| WindowSessionError::Invalid {
                path: path.clone(),
                reason: e.to_string(),
            })?
        };
        validate_state(&state, &path)?;
        let guarded_quit = stored_quit_certificate(&raw, &state, &path)?;
        for record in state.live.values_mut() {
            record.project = record.project.take().map(|path| self.project_path(&path));
        }
        if let Some(checkpoint) = &mut state.checkpoint {
            for window in &mut checkpoint.windows {
                window.project = window.project.take().map(|path| self.project_path(&path));
            }
        }
        Ok(WindowSessionSnapshot {
            config: self.config.clone(),
            state,
            raw,
            revision,
            guarded_quit,
        })
    }
    fn project_path(&self, path: &Path) -> PathBuf {
        normalize_path(&if path.is_absolute() {
            path.into()
        } else {
            self.config.join(path)
        })
    }
    pub fn accept_window(
        &self,
        baseline: &WindowSessionSnapshot,
        owner: WindowOwnerId,
        project: Option<&Path>,
    ) -> Result<WindowMutation, WindowSessionError> {
        if baseline.config != self.config {
            return Err(WindowSessionError::WrongConfiguration);
        }
        let project = project.map(|path| self.project_path(path));
        let snapshot = self.mutate(|current| {
            check_generation(baseline.generation(), current.generation())?;
            if baseline.revision.is_some() && current.revision.is_none() {
                return Err(WindowSessionError::ExternalChange);
            }
            if current.state.live.contains_key(&owner) {
                return Err(WindowSessionError::DuplicateOwner);
            }
            let order = current.state.next_order;
            current.state.next_order = order.checked_add(1).ok_or(WindowSessionError::Exhausted)?;
            current.state.live.insert(
                owner.clone(),
                WindowRecord {
                    revision: 0,
                    order,
                    project,
                    closed: false,
                },
            );
            Ok(())
        })?;
        let token = WindowToken {
            config: self.config.clone(),
            generation: snapshot.generation(),
            owner,
            revision: 0,
        };
        Ok(WindowMutation { snapshot, token })
    }
    /// Some(project) updates the accepted project; None is Close Project.
    /// Neither operation changes the durable restart checkpoint.
    pub fn update_project(
        &self,
        token: &WindowToken,
        project: Option<&Path>,
    ) -> Result<WindowMutation, WindowSessionError> {
        let project = project.map(|path| self.project_path(path));
        let snapshot = self.mutate(|current| {
            let record = checked_record(current, token, &self.config)?;
            record.revision = record
                .revision
                .checked_add(1)
                .ok_or(WindowSessionError::Exhausted)?;
            record.project = project;
            Ok(())
        })?;
        let token = WindowToken {
            revision: token
                .revision
                .checked_add(1)
                .ok_or(WindowSessionError::Exhausted)?,
            ..token.clone()
        };
        Ok(WindowMutation { snapshot, token })
    }
    /// A tombstone rejects late same-owner callbacks and duplicate acceptance.
    pub fn close_window(
        &self,
        token: &WindowToken,
    ) -> Result<WindowSessionSnapshot, WindowSessionError> {
        self.mutate(|current| {
            let record = checked_record(current, token, &self.config)?;
            record.revision = record
                .revision
                .checked_add(1)
                .ok_or(WindowSessionError::Exhausted)?;
            record.closed = true;
            Ok(())
        })
    }
    /// Tombstone accepted owners not kept by `keep`: windows whose process died
    /// (killed or crashed) without retiring its owner. `confirm` runs while
    /// the store lock excludes new acceptance; it must re-prove that `keep`
    /// still covers every live endpoint (acceptance always follows endpoint
    /// registration), otherwise nothing is retired. Returns retired owners.
    pub fn retire_unowned(
        &self,
        keep: impl Fn(&WindowOwnerId) -> bool,
        confirm: impl FnOnce() -> bool,
    ) -> Result<Vec<WindowOwnerId>, WindowSessionError> {
        let mut retired = Vec::new();
        self.mutate_mode(
            |current| {
                let stale: Vec<_> = current
                    .state
                    .live
                    .iter()
                    .filter(|(owner, record)| !record.closed && !keep(owner))
                    .map(|(owner, _)| owner.clone())
                    .collect();
                if stale.is_empty() || !confirm() {
                    return Ok(false);
                }
                for owner in &stale {
                    let record = current.state.live.get_mut(owner).unwrap();
                    record.revision = record
                        .revision
                        .checked_add(1)
                        .ok_or(WindowSessionError::Exhausted)?;
                    record.closed = true;
                }
                retired = stale;
                Ok(true)
            },
            |_| Ok(()),
            false,
            &|_| Ok(()),
        )?;
        Ok(retired)
    }
    pub fn prepare_quit(
        &self,
        snapshot: &WindowSessionSnapshot,
    ) -> Result<QuitPlan, WindowSessionError> {
        if snapshot.config != self.config {
            return Err(WindowSessionError::WrongConfiguration);
        }
        Ok(QuitPlan {
            config: self.config.clone(),
            generation: snapshot.generation(),
            cohort: snapshot.live_windows(),
            revision: snapshot.revision,
        })
    }
    /// Publish only after every external UI/IPC guard has accepted quitting.
    /// A changed membership, project or owner revision rejects the entire plan.
    pub fn quit_application(
        &self,
        plan: QuitPlan,
    ) -> Result<WindowSessionSnapshot, WindowSessionError> {
        if plan.config != self.config {
            return Err(WindowSessionError::WrongConfiguration);
        }
        self.mutate(|current| {
            check_generation(plan.generation, current.generation())?;
            if plan.revision.is_some() && current.revision.is_none() {
                return Err(WindowSessionError::ExternalChange);
            }
            if current.revision != plan.revision || current.live_windows() != plan.cohort {
                return Err(WindowSessionError::CohortChanged);
            }
            let generation = current.generation().next()?;
            if let Some(checkpoint) = current.raw["checkpoint"].as_object_mut() {
                checkpoint.remove("guarded_quit");
            }
            current.state.checkpoint = Some(RestartCheckpoint {
                generation,
                windows: plan
                    .cohort
                    .into_iter()
                    .map(|w| CheckpointWindow {
                        owner: w.token.owner,
                        project: w.project,
                    })
                    .collect(),
            });
            current.state.generation = generation;
            current.state.live.clear();
            current.state.next_order = 0;
            Ok(())
        })
    }
    /// Publish a transaction-bound checkpoint and retire its entire cohort in
    /// one locked replacement. This does not run UI guards or prove liveness.
    /// Exact retries are reconciled even after an uncertain post-rename result.
    pub fn commit_guarded_quit(
        &self,
        plan: QuitPlan,
        binding: GuardedQuitBinding,
    ) -> Result<GuardedQuitCertificate, WindowSessionError> {
        self.commit_guarded_quit_with_sync(plan, binding, sync_published)
    }
    fn commit_guarded_quit_with_sync(
        &self,
        plan: QuitPlan,
        binding: GuardedQuitBinding,
        sync: impl Fn(&Path) -> Result<(), WindowSessionError>,
    ) -> Result<GuardedQuitCertificate, WindowSessionError> {
        if plan.config != self.config {
            return Err(WindowSessionError::WrongConfiguration);
        }
        validate_quit_binding(&binding, &plan, &self.path())?;
        let generation = plan.generation.next()?;
        let certificate = make_quit_certificate(generation, binding)?;
        let result = self.mutate_inner_durable(
            |current| {
                if let Some(stored) =
                    stored_quit_certificate(&current.raw, &current.state, &self.path())?
                {
                    if stored.transaction_id == certificate.transaction_id {
                        if stored != certificate
                            || current.generation() != generation
                            || !current.state.live.is_empty()
                            || current.state.next_order != 0
                        {
                            return Err(WindowSessionError::CohortChanged);
                        }
                        // Idempotent retry: retain exact raw bytes (including future
                        // fields); only finish syncing the already published file.
                        return Ok(false);
                    }
                }
                check_generation(plan.generation, current.generation())?;
                if current.revision != plan.revision || current.live_windows() != plan.cohort {
                    return Err(WindowSessionError::CohortChanged);
                }
                current.state.checkpoint = Some(RestartCheckpoint {
                    generation,
                    windows: plan
                        .cohort
                        .iter()
                        .map(|w| CheckpointWindow {
                            owner: w.token.owner.clone(),
                            project: w.project.clone(),
                        })
                        .collect(),
                });
                if !current.raw["checkpoint"].is_object() {
                    current.raw["checkpoint"] = serde_json::json!({});
                }
                let encoded = serde_json::to_value(&certificate).map_err(|e| {
                    WindowSessionError::Invalid {
                        path: self.path(),
                        reason: e.to_string(),
                    }
                })?;
                overlay(&mut current.raw["checkpoint"]["guarded_quit"], &encoded);
                current.state.generation = generation;
                current.state.live.clear();
                current.state.next_order = 0;
                Ok(true)
            },
            &sync,
        );
        match result {
            Ok(_) => Ok(certificate),
            Err(error @ WindowSessionError::PublicationUncertain { .. }) => {
                Err(WindowSessionError::QuitCommitUncertain {
                    certificate,
                    error: Box::new(error),
                })
            }
            Err(error) => Err(error),
        }
    }
    /// Noncreating read. The complete certificate and local guard must match
    /// persisted state; restoration/new membership makes late Commit stale.
    pub fn verify_guarded_quit(
        &self,
        certificate: &GuardedQuitCertificate,
        local: &GuardedQuitWindow,
    ) -> Result<VerifiedQuitCommit, WindowSessionError> {
        self.verify_guarded_quit_with_sync(certificate, local, sync_published)
    }
    fn verify_guarded_quit_with_sync(
        &self,
        certificate: &GuardedQuitCertificate,
        local: &GuardedQuitWindow,
        sync: impl FnOnce(&Path) -> Result<(), WindowSessionError>,
    ) -> Result<VerifiedQuitCommit, WindowSessionError> {
        let snapshot = self.load()?;
        let stored = stored_quit_certificate(&snapshot.raw, &snapshot.state, &self.path())?
            .ok_or(WindowSessionError::MissingCheckpoint)?;
        if &stored != certificate
            || snapshot.generation() != stored.checkpoint_generation
            || !snapshot.state.live.is_empty()
            || snapshot.state.next_order != 0
            || !stored.cohort.contains(local)
        {
            return Err(WindowSessionError::CohortChanged);
        }
        sync(&self.path())?;
        if read_checked(&self.path())?
            .as_deref()
            .map(ContentRevision::of)
            != snapshot.revision
        {
            return Err(WindowSessionError::ExternalChange);
        }
        Ok(VerifiedQuitCommit {
            config: self.config.clone(),
            certificate: stored,
        })
    }

    /// Exclusive for this configuration, covering the requested checkpoint
    /// generation. Busy means another coordinator holds it; stale is an error.
    /// Acquiring a claim is read-only and does not imply that windows are dead.
    pub fn try_claim_restore(
        &self,
        requested: WindowGeneration,
    ) -> Result<Option<RestoreClaim>, WindowSessionError> {
        let initial = self.load()?;
        check_checkpoint(&initial, requested)?;
        let path = self.config.join("window-session.restore.lock");
        let Some(lock) = lock_file(&path)? else {
            return Ok(None);
        };
        let current = self.load()?;
        check_checkpoint(&current, requested)?;
        if !same_lock(&path, &lock)? {
            return Err(WindowSessionError::ClaimLost);
        }
        Ok(Some(RestoreClaim {
            config: self.config.clone(),
            owner: Arc::clone(&self.owner),
            checkpoint_generation: requested,
            live_generation: current.generation(),
            revision: current.revision,
            cohort: current.live_windows(),
            begun: false,
            _lock: lock,
        }))
    }
    /// Start a new runtime round without altering the restart checkpoint.
    /// PRECONDITION: the caller established through its authenticated registry
    /// that no live windows/startup cohort exists. The claim supplies exclusion,
    /// not PID liveness; this service cannot prove the caller's registry fact.
    /// Newly accepted membership since claim acquisition also rejects the round.
    pub fn begin_restore(
        &self,
        claim: &mut RestoreClaim,
    ) -> Result<WindowSessionSnapshot, WindowSessionError> {
        if claim.config != self.config || !Arc::ptr_eq(&claim.owner, &self.owner) {
            return Err(WindowSessionError::WrongConfiguration);
        }
        if claim.begun {
            return Err(WindowSessionError::RestoreAlreadyBegun);
        }
        let path = self.config.join("window-session.restore.lock");
        if !same_lock(&path, &claim._lock)? {
            return Err(WindowSessionError::ClaimLost);
        }
        let snapshot = self.mutate(|current| {
            check_checkpoint(current, claim.checkpoint_generation)?;
            check_generation(claim.live_generation, current.generation())?;
            if current.revision != claim.revision || current.live_windows() != claim.cohort {
                return Err(WindowSessionError::CohortChanged);
            }
            if !same_lock(&path, &claim._lock)? {
                return Err(WindowSessionError::ClaimLost);
            }
            current.state.generation = current.generation().next()?;
            current.state.live.clear();
            current.state.next_order = 0;
            Ok(())
        })?;
        claim.begun = true;
        Ok(snapshot)
    }
    fn mutate(
        &self,
        change: impl FnOnce(&mut WindowSessionSnapshot) -> Result<(), WindowSessionError>,
    ) -> Result<WindowSessionSnapshot, WindowSessionError> {
        self.mutate_inner(change, |_| Ok(()))
    }
    fn mutate_inner(
        &self,
        change: impl FnOnce(&mut WindowSessionSnapshot) -> Result<(), WindowSessionError>,
        before_publish: impl FnOnce(&Path) -> std::io::Result<()>,
    ) -> Result<WindowSessionSnapshot, WindowSessionError> {
        self.mutate_mode(
            |current| {
                change(current)?;
                Ok(true)
            },
            before_publish,
            false,
            &sync_published,
        )
    }
    fn mutate_inner_durable(
        &self,
        change: impl FnOnce(&mut WindowSessionSnapshot) -> Result<bool, WindowSessionError>,
        sync: &dyn Fn(&Path) -> Result<(), WindowSessionError>,
    ) -> Result<WindowSessionSnapshot, WindowSessionError> {
        self.mutate_mode(change, |_| Ok(()), true, sync)
    }
    fn mutate_mode(
        &self,
        change: impl FnOnce(&mut WindowSessionSnapshot) -> Result<bool, WindowSessionError>,
        before_publish: impl FnOnce(&Path) -> std::io::Result<()>,
        durable: bool,
        sync: &dyn Fn(&Path) -> Result<(), WindowSessionError>,
    ) -> Result<WindowSessionSnapshot, WindowSessionError> {
        let lock_path = self.config.join("window-session.lock");
        let deadline = Instant::now() + Duration::from_secs(2);
        let lock = loop {
            if let Some(lock) = lock_file(&lock_path)? {
                break lock;
            }
            if Instant::now() >= deadline {
                return Err(WindowSessionError::Busy);
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        if !same_lock(&lock_path, &lock)? {
            return Err(WindowSessionError::ExternalChange);
        }
        let mut current = self.load()?;
        let revision = current.revision;
        if !change(&mut current)? {
            sync(&self.path())?;
            return Ok(current);
        }
        validate_state(&current.state, &self.path())?;
        let encoded =
            serde_json::to_value(&current.state).map_err(|e| WindowSessionError::Invalid {
                path: self.path(),
                reason: e.to_string(),
            })?;
        // Root keys are known; live owner IDs and checkpoint IDs retain unknown
        // record metadata only while that logical record still exists.
        let mut raw = current.raw.clone();
        for key in ["format_version", "generation", "next_order"] {
            raw[key] = encoded[key].clone();
        }
        let previous = raw
            .get("live")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut live = serde_json::Map::new();
        for (key, value) in encoded["live"].as_object().unwrap() {
            let mut record = previous.get(key).cloned().unwrap_or(Value::Null);
            overlay(&mut record, value);
            live.insert(key.clone(), record);
        }
        raw["live"] = Value::Object(live);
        overlay(&mut raw["checkpoint"], &encoded["checkpoint"]);
        let bytes = serde_json::to_vec_pretty(&raw).map_err(|e| WindowSessionError::Invalid {
            path: self.path(),
            reason: e.to_string(),
        })?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(WindowSessionError::Invalid {
                path: self.path(),
                reason: "prepared window session exceeds 8 MiB".into(),
            });
        }
        let path = self.path();
        let guarded_quit = stored_quit_certificate(&raw, &current.state, &path)?;
        atomic_write(&path, &bytes, |temp| {
            before_publish(temp).map_err(|e| io(&path, e))?;
            if !same_lock(&lock_path, &lock)? {
                return Err(WindowSessionError::ExternalChange);
            }
            if read_checked(&path)?.as_deref().map(ContentRevision::of) != revision {
                return Err(WindowSessionError::ExternalChange);
            }
            Ok(())
        })?;
        if durable {
            sync(&path)?;
        }
        current.raw = raw;
        current.revision = Some(ContentRevision::of(&bytes));
        current.guarded_quit = guarded_quit;
        Ok(current)
    }
}
fn quit_invalid(path: &Path, reason: impl Into<String>) -> WindowSessionError {
    WindowSessionError::Invalid {
        path: path.into(),
        reason: reason.into(),
    }
}
fn compact_quit_value(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}
fn validate_quit_binding(
    binding: &GuardedQuitBinding,
    plan: &QuitPlan,
    path: &Path,
) -> Result<(), WindowSessionError> {
    if !compact_quit_value(&binding.transaction_id)
        || binding.cohort.is_empty()
        || binding.cohort.len() > 256
        || binding.cohort.len() != plan.cohort.len()
    {
        return Err(quit_invalid(
            path,
            "invalid guarded Quit transaction/cohort",
        ));
    }
    let mut ids = BTreeSet::new();
    for (guarded, live) in binding.cohort.iter().zip(&plan.cohort) {
        if guarded.owner != live.token.owner
            || guarded.generation != live.token.generation
            || guarded.owner_revision != live.token.revision
            || guarded.project != live.project
            || !compact_quit_value(&guarded.window_id)
            || !ids.insert(&guarded.window_id)
            || !compact_quit_value(&guarded.guard.nonce)
            || !compact_quit_value(&guarded.guard.state)
        {
            return Err(quit_invalid(
                path,
                "guarded Quit cohort does not exactly bind accepted windows",
            ));
        }
    }
    Ok(())
}
fn make_quit_certificate(
    generation: WindowGeneration,
    binding: GuardedQuitBinding,
) -> Result<GuardedQuitCertificate, WindowSessionError> {
    // Fixed field order, domain and typed compact serialization. No self-hash,
    // raw future metadata, filesystem canonicalization or mutable root revision.
    let bytes = serde_json::to_vec(&("rustpy.guarded-quit.v1", generation, &binding))
        .map_err(|e| quit_invalid(Path::new("window-session.json"), e.to_string()))?;
    let digest = ContentRevision::of(&bytes)
        .0
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(GuardedQuitCertificate {
        transaction_id: binding.transaction_id,
        checkpoint_generation: generation,
        checkpoint_revision: digest,
        cohort: binding.cohort,
    })
}
fn stored_quit_certificate(
    raw: &Value,
    state: &State,
    path: &Path,
) -> Result<Option<GuardedQuitCertificate>, WindowSessionError> {
    let Some(value) = raw.get("checkpoint").and_then(|c| c.get("guarded_quit")) else {
        return Ok(None);
    };
    let require = |object: &Value, keys: &[&str]| -> Result<(), WindowSessionError> {
        for key in keys {
            if object.get(*key).is_none() {
                return Err(quit_invalid(
                    path,
                    format!("missing guarded Quit field `{key}`"),
                ));
            }
        }
        Ok(())
    };
    require(
        value,
        &[
            "transaction_id",
            "checkpoint_generation",
            "checkpoint_revision",
            "cohort",
        ],
    )?;
    if let Some(cohort) = value["cohort"].as_array() {
        for window in cohort {
            require(
                window,
                &[
                    "owner",
                    "generation",
                    "owner_revision",
                    "window_id",
                    "project",
                    "guard",
                ],
            )?;
            require(&window["guard"], &["nonce", "state"])?;
        }
    }
    let certificate: GuardedQuitCertificate = serde_json::from_value(value.clone())
        .map_err(|e| quit_invalid(path, format!("invalid guarded Quit record: {e}")))?;
    let checkpoint = state
        .checkpoint
        .as_ref()
        .ok_or_else(|| quit_invalid(path, "guarded Quit without checkpoint"))?;
    let prior = certificate
        .cohort
        .first()
        .map(|w| w.generation)
        .ok_or_else(|| quit_invalid(path, "empty guarded Quit cohort"))?;
    let cohort: Vec<_> = certificate
        .cohort
        .iter()
        .map(|w| LiveWindow {
            token: WindowToken {
                config: PathBuf::new(),
                generation: w.generation,
                owner: w.owner.clone(),
                revision: w.owner_revision,
            },
            project: w.project.clone(),
        })
        .collect();
    let plan = QuitPlan {
        config: PathBuf::new(),
        generation: prior,
        cohort,
        revision: None,
    };
    let binding = GuardedQuitBinding {
        transaction_id: certificate.transaction_id.clone(),
        cohort: certificate.cohort.clone(),
    };
    validate_quit_binding(&binding, &plan, path)?;
    let mut owners = BTreeSet::new();
    if prior.next()? != checkpoint.generation
        || certificate.checkpoint_generation != checkpoint.generation
        || certificate
            .cohort
            .iter()
            .any(|w| w.generation != prior || !owners.insert(&w.owner))
        || checkpoint.windows
            != certificate
                .cohort
                .iter()
                .map(|w| CheckpointWindow {
                    owner: w.owner.clone(),
                    project: w.project.clone(),
                })
                .collect::<Vec<_>>()
        || make_quit_certificate(checkpoint.generation, binding)? != certificate
    {
        return Err(quit_invalid(
            path,
            "guarded Quit digest/checkpoint binding mismatch",
        ));
    }
    Ok(Some(certificate))
}
/// The file was replaced already. Failures here must never be reported as a
/// definitely uncommitted Quit. Retrying also repairs interrupted durability.
fn sync_published(path: &Path) -> Result<(), WindowSessionError> {
    let uncertain = |error| WindowSessionError::PublicationUncertain {
        path: path.into(),
        error,
    };
    let metadata = std::fs::symlink_metadata(path).map_err(uncertain)?;
    if !metadata.is_file() {
        return Err(uncertain(std::io::Error::other(
            "published checkpoint is not a regular file",
        )));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        };
        options
            .write(true)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT); // FlushFileBuffers needs GENERIC_WRITE.
    }
    let file = options.open(path).map_err(uncertain)?;
    if !file.metadata().map_err(uncertain)?.is_file() {
        return Err(uncertain(std::io::Error::other(
            "published checkpoint changed to a nonregular file",
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let owned = file.metadata().map_err(uncertain)?;
        if metadata.dev() != owned.dev() || metadata.ino() != owned.ino() {
            return Err(uncertain(std::io::Error::other(
                "published checkpoint identity changed before sync",
            )));
        }
    }
    file.sync_all().map_err(uncertain)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path.parent().unwrap())
            .map_err(uncertain)?;
        directory.sync_all().map_err(uncertain)?;
    }
    // Windows atomic replacement uses MOVEFILE_WRITE_THROUGH; file sync above
    // additionally checks completion. There is no portable directory fsync.
    Ok(())
}

/// Existing v1 files must contain their entire known state. In particular,
/// omitted Option fields are not silently interpreted as explicit JSON null.
fn validate_required_keys(raw: &Value, path: &Path) -> Result<(), WindowSessionError> {
    let require = |object: &Value, keys: &[&str], context: &str| {
        for key in keys {
            if object.get(*key).is_none() {
                return Err(WindowSessionError::Invalid {
                    path: path.into(),
                    reason: format!("missing required {context} field `{key}`"),
                });
            }
        }
        Ok(())
    };
    require(
        raw,
        &[
            "format_version",
            "generation",
            "next_order",
            "live",
            "checkpoint",
        ],
        "state",
    )?;
    if let Some(live) = raw["live"].as_object() {
        for record in live.values() {
            require(
                record,
                &["revision", "order", "project", "closed"],
                "live window",
            )?;
        }
    }
    if !raw["checkpoint"].is_null() {
        require(&raw["checkpoint"], &["generation", "windows"], "checkpoint")?;
        if let Some(windows) = raw["checkpoint"]["windows"].as_array() {
            for window in windows {
                require(window, &["owner", "project"], "checkpoint window")?;
            }
        }
    }
    Ok(())
}

fn validate_state(state: &State, path: &Path) -> Result<(), WindowSessionError> {
    let invalid = |reason: &str| WindowSessionError::Invalid {
        path: path.into(),
        reason: reason.into(),
    };
    if state.format_version != 1 {
        return Err(invalid("unsupported window-session format"));
    }
    if state
        .live
        .keys()
        .chain(
            state
                .checkpoint
                .iter()
                .flat_map(|c| c.windows.iter().map(|w| &w.owner)),
        )
        .any(|id| id.0.is_empty() || id.0.len() > 128)
    {
        return Err(invalid("invalid logical window identity"));
    }
    if state
        .live
        .values()
        .any(|record| record.order >= state.next_order)
    {
        return Err(invalid("invalid accepted window order"));
    }
    if state
        .live
        .values()
        .filter_map(|r| r.project.as_ref())
        .chain(
            state
                .checkpoint
                .iter()
                .flat_map(|c| c.windows.iter().filter_map(|w| w.project.as_ref())),
        )
        .any(|path| path.as_os_str().is_empty())
    {
        return Err(invalid("empty project path"));
    }
    if let Some(checkpoint) = &state.checkpoint {
        let mut ids = BTreeSet::new();
        if checkpoint
            .windows
            .iter()
            .any(|window| !ids.insert(&window.owner))
        {
            return Err(invalid("duplicate checkpoint owner"));
        }
        if checkpoint.generation.0 > state.generation.0 {
            return Err(invalid("checkpoint generation exceeds runtime generation"));
        }
    }
    Ok(())
}
fn check_generation(
    expected: WindowGeneration,
    actual: WindowGeneration,
) -> Result<(), WindowSessionError> {
    if expected == actual {
        Ok(())
    } else {
        Err(WindowSessionError::StaleGeneration { expected, actual })
    }
}
fn check_checkpoint(
    snapshot: &WindowSessionSnapshot,
    requested: WindowGeneration,
) -> Result<(), WindowSessionError> {
    let actual = snapshot
        .checkpoint()
        .ok_or(WindowSessionError::MissingCheckpoint)?
        .generation;
    check_generation(requested, actual)
}
fn checked_record<'a>(
    snapshot: &'a mut WindowSessionSnapshot,
    token: &WindowToken,
    config: &Path,
) -> Result<&'a mut WindowRecord, WindowSessionError> {
    if token.config != config {
        return Err(WindowSessionError::WrongConfiguration);
    }
    check_generation(token.generation, snapshot.generation())?;
    let record = snapshot
        .state
        .live
        .get_mut(&token.owner)
        .ok_or(WindowSessionError::StaleOwner)?;
    if record.closed {
        return Err(WindowSessionError::ClosedOwner);
    }
    if record.revision != token.revision {
        return Err(WindowSessionError::StaleOwner);
    }
    Ok(record)
}
fn overlay(old: &mut Value, new: &Value) {
    match (old, new) {
        (Value::Object(old), Value::Object(new)) => {
            for (key, value) in new {
                overlay(old.entry(key.clone()).or_insert(Value::Null), value);
            }
        }
        (Value::Array(old), Value::Array(new)) => {
            let previous = std::mem::take(old);
            *old = new
                .iter()
                .map(|value| {
                    let mut record = previous
                        .iter()
                        .find(|old| {
                            value.get("owner").is_some() && old.get("owner") == value.get("owner")
                        })
                        .cloned()
                        .unwrap_or(Value::Null);
                    overlay(&mut record, value);
                    record
                })
                .collect();
        }
        (old, new) => *old = new.clone(),
    }
}

fn read_checked(path: &Path) -> Result<Option<Vec<u8>>, WindowSessionError> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(path, e)),
        Ok(meta) if !meta.is_file() => {
            return Err(WindowSessionError::Invalid {
                path: path.into(),
                reason: "not a regular file (symlinks are not replaced)".into(),
            })
        }
        Ok(_) => {}
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|e| io(path, e))?;
    if !file.metadata().map_err(|e| io(path, e))?.is_file() {
        return Err(WindowSessionError::Invalid {
            path: path.into(),
            reason: "not a regular file".into(),
        });
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io(path, e))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(WindowSessionError::Invalid {
            path: path.into(),
            reason: "window session exceeds 8 MiB".into(),
        });
    }
    Ok(Some(bytes))
}
fn same_lock(path: &Path, file: &File) -> Result<bool, WindowSessionError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(io(path, e)),
    };
    if !metadata.is_file() {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let owned = file.metadata().map_err(|e| io(path, e))?;
        Ok(metadata.dev() == owned.dev() && metadata.ino() == owned.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Ok(true)
    } // Windows lock handles deny delete/replacement.
}
/// Unlock before close so an inherited/duplicated Unix description cannot
/// prolong this logical owner's lease after it releases ownership.
#[derive(Debug)]
struct OwnedLock(File);
impl std::ops::Deref for OwnedLock {
    type Target = File;
    fn deref(&self) -> &File {
        &self.0
    }
}
impl Drop for OwnedLock {
    fn drop(&mut self) {
        os_lock::unlock(&self.0);
    }
}

fn lock_file(path: &Path) -> Result<Option<OwnedLock>, WindowSessionError> {
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| io(path, e))?;
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.is_file() {
            return Err(WindowSessionError::Invalid {
                path: path.into(),
                reason: "lock is not a regular file".into(),
            });
        }
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        options
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path).map_err(|e| io(path, e))?;
    if !file.metadata().map_err(|e| io(path, e))?.is_file() {
        return Err(WindowSessionError::Invalid {
            path: path.into(),
            reason: "lock is not a regular file".into(),
        });
    }
    if os_lock::try_lock(&file).map_err(|e| io(path, e))? {
        Ok(Some(OwnedLock(file)))
    } else {
        Ok(None)
    }
}
fn atomic_write(
    path: &Path,
    bytes: &[u8],
    before_publish: impl FnOnce(&Path) -> Result<(), WindowSessionError>,
) -> Result<(), WindowSessionError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().unwrap();
    let permissions = match std::fs::metadata(path) {
        Ok(meta) if meta.permissions().readonly() => {
            return Err(io(
                path,
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "window session is read-only",
                ),
            ))
        }
        Ok(meta) => Some(meta.permissions()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(io(path, e)),
    };
    let mut temp = None;
    for _ in 0..128 {
        let candidate = parent.join(format!(
            ".window-session.{}.{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                temp = Some((candidate, file));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io(&candidate, e)),
        }
    }
    let (tmp, mut file) = temp.ok_or(WindowSessionError::Busy)?;
    let result = (|| {
        if let Some(permissions) = permissions {
            file.set_permissions(permissions).map_err(|e| io(path, e))?;
        }
        file.write_all(bytes).map_err(|e| io(path, e))?;
        file.sync_all().map_err(|e| io(path, e))?;
        drop(file);
        before_publish(&tmp)?;
        replace_file(&tmp, path).map_err(|e| io(path, e))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}
#[cfg(not(windows))]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::rename(from, to)
}
#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let from: Vec<u16> = from.as_os_str().encode_wide().chain([0]).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: both pointers are valid null-terminated strings for the call.
    if unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } != 0
    {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
mod os_lock {
    use std::os::unix::io::AsRawFd as _;

    /// Takes the exclusive lock without waiting: false when another holds it.
    pub fn try_lock(file: &std::fs::File) -> std::io::Result<bool> {
        for _ in 0..128 {
            // SAFETY: flock on a file descriptor this `File` owns.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(true);
            }
            let error = std::io::Error::last_os_error();
            match error.kind() {
                std::io::ErrorKind::Interrupted => continue,
                std::io::ErrorKind::WouldBlock => return Ok(false),
                _ => return Err(error),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "lock interrupted repeatedly",
        ))
    }

    #[allow(dead_code)]
    pub fn unlock(file: &std::fs::File) {
        // SAFETY: as above; closing the file would release the lock as well.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    }

    #[allow(dead_code)]
    pub fn unsupported(e: &std::io::Error) -> bool {
        matches!(e.raw_os_error(), Some(code) if code == libc::ENOLCK || code == libc::EOPNOTSUPP || code == libc::ENOTSUP || code == libc::ENOSYS)
    }
}

#[cfg(windows)]
mod os_lock {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        LockFileEx, UnlockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    /// `ERROR_LOCK_VIOLATION`: another handle holds the lock.
    const LOCK_VIOLATION: i32 = 33;

    pub fn try_lock(file: &std::fs::File) -> std::io::Result<bool> {
        // SAFETY: a zeroed OVERLAPPED locks from offset 0; the handle is owned by `file`.
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            LockFileEx(
                file.as_raw_handle(),
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                u32::MAX,
                u32::MAX,
                &mut overlapped,
            )
        };
        if ok != 0 {
            return Ok(true);
        }
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(LOCK_VIOLATION) {
            Ok(false)
        } else {
            Err(e)
        }
    }

    #[allow(dead_code)]
    pub fn unlock(file: &std::fs::File) {
        // SAFETY: as above.
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        unsafe { UnlockFileEx(file.as_raw_handle(), 0, u32::MAX, u32::MAX, &mut overlapped) };
    }

    #[allow(dead_code)]
    pub fn unsupported(_e: &std::io::Error) -> bool {
        false
    }
}

#[cfg(not(any(unix, windows)))]
mod os_lock {
    pub fn try_lock(_file: &std::fs::File) -> std::io::Result<bool> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "no file locks on this platform",
        ))
    }

    #[allow(dead_code)]
    pub fn unlock(_file: &std::fs::File) {}

    #[allow(dead_code)]
    pub fn unsupported(_e: &std::io::Error) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_atomic_publication_and_external_change_preserve_original() {
        let dir = tempfile::tempdir().unwrap();
        let store = WindowSessionStore::new(dir.path());
        let first = store
            .accept_window(&store.load().unwrap(), WindowOwnerId::new(), None)
            .unwrap();
        let before = std::fs::read(store.path()).unwrap();
        let error = store.mutate_inner(
            |snapshot| {
                snapshot.state.next_order += 1;
                Ok(())
            },
            |_| Err(std::io::Error::other("injected publication failure")),
        );
        assert!(matches!(error, Err(WindowSessionError::Io { .. })));
        assert_eq!(std::fs::read(store.path()).unwrap(), before);
        assert_eq!(store.load().unwrap().revision(), first.snapshot.revision());
        assert!(!std::fs::read_dir(dir.path()).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
        let external = b"{\"future_external\":true}";
        let error = store.mutate_inner(|_| Ok(()), |_| std::fs::write(store.path(), external));
        assert!(matches!(error, Err(WindowSessionError::ExternalChange)));
        assert_eq!(std::fs::read(store.path()).unwrap(), external);
    }
    #[test]
    fn oversized_output_refused_before_publish_for_missing_and_existing_files() {
        let dir = tempfile::tempdir().unwrap();
        let store = WindowSessionStore::new(dir.path());
        let oversize = |snapshot: &mut WindowSessionSnapshot| {
            snapshot.raw["future"] = Value::String("x".repeat(MAX_BYTES as usize));
            Ok(())
        };
        assert!(matches!(
            store.mutate(oversize),
            Err(WindowSessionError::Invalid { .. })
        ));
        assert!(!store.path().exists());
        let first = store
            .accept_window(&store.load().unwrap(), WindowOwnerId::new(), None)
            .unwrap();
        let before = std::fs::read(store.path()).unwrap();
        assert!(matches!(
            store.mutate(oversize),
            Err(WindowSessionError::Invalid { .. })
        ));
        assert_eq!(std::fs::read(store.path()).unwrap(), before);
        assert_eq!(store.load().unwrap().revision(), first.snapshot.revision());
    }
    #[cfg(unix)]
    #[test]
    fn replaced_sidecar_cannot_publish_under_detached_lock() {
        let dir = tempfile::tempdir().unwrap();
        let store = WindowSessionStore::new(dir.path());
        store
            .accept_window(&store.load().unwrap(), WindowOwnerId::new(), None)
            .unwrap();
        let before = std::fs::read(store.path()).unwrap();
        let error = store.mutate_inner(
            |_| Ok(()),
            |_| {
                std::fs::remove_file(dir.path().join("window-session.lock"))?;
                std::fs::write(dir.path().join("window-session.lock"), b"replacement")
            },
        );
        assert!(matches!(error, Err(WindowSessionError::ExternalChange)));
        assert_eq!(std::fs::read(store.path()).unwrap(), before);
    }
    #[cfg(windows)]
    #[test]
    fn held_sidecar_refuses_deletion_without_publishing() {
        let dir = tempfile::tempdir().unwrap();
        let store = WindowSessionStore::new(dir.path());
        store
            .accept_window(&store.load().unwrap(), WindowOwnerId::new(), None)
            .unwrap();
        let before = std::fs::read(store.path()).unwrap();
        let error = store.mutate_inner(
            |_| Ok(()),
            |_| std::fs::remove_file(dir.path().join("window-session.lock")),
        );
        assert!(matches!(error, Err(WindowSessionError::Io { .. })));
        assert_eq!(std::fs::read(store.path()).unwrap(), before);
    }
    #[cfg(unix)]
    #[test]
    fn drop_releases_owned_lock_even_when_an_inherited_description_remains_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owned.lock");
        let guard = lock_file(&path).unwrap().unwrap();
        let inherited = guard.try_clone().unwrap();
        assert!(lock_file(&path).unwrap().is_none());
        drop(guard);
        let next = lock_file(&path).unwrap();
        assert!(
            next.is_some(),
            "logical owner release must not wait for inherited descriptor close"
        );
        drop(inherited);
    }
}

#[cfg(test)]
mod guarded_quit_publication_tests {
    use super::*;

    #[test]
    fn post_rename_sync_failure_is_uncertain_and_same_transaction_repairs_durability() {
        let dir = tempfile::tempdir().unwrap();
        let store = WindowSessionStore::new(dir.path());
        let before = store
            .accept_window(&store.load().unwrap(), WindowOwnerId::new(), None)
            .unwrap()
            .snapshot;
        let live = &before.live_windows()[0];
        let binding = GuardedQuitBinding {
            transaction_id: "uncertain-quit".into(),
            cohort: vec![GuardedQuitWindow {
                owner: live.token.owner().clone(),
                generation: live.token.generation(),
                owner_revision: live.token.revision(),
                window_id: "actual-endpoint".into(),
                project: None,
                guard: QuitUiGuard {
                    nonce: "actual-ui-nonce".into(),
                    state: "actual-ui-state".into(),
                },
            }],
        };
        let result = store.commit_guarded_quit_with_sync(
            store.prepare_quit(&before).unwrap(),
            binding.clone(),
            |path| {
                // Inject at the real post-replacement boundary. The complete receipt
                // and ownership retirement already coexist on disk, never a sidecar.
                let published = store.load().unwrap();
                assert!(published.live_windows().is_empty());
                assert!(published.raw["checkpoint"]["guarded_quit"].is_object());
                Err(WindowSessionError::PublicationUncertain {
                    path: path.into(),
                    error: std::io::Error::other("injected directory sync failure"),
                })
            },
        );
        let Err(WindowSessionError::QuitCommitUncertain { certificate, .. }) = result else {
            panic!("post-replacement failure must not permit Abort");
        };
        let bytes = std::fs::read(store.path()).unwrap();
        let retry = store
            .commit_guarded_quit(store.prepare_quit(&before).unwrap(), binding.clone())
            .unwrap();
        assert_eq!(retry, certificate);
        assert_eq!(std::fs::read(store.path()).unwrap(), bytes);
        assert!(store
            .verify_guarded_quit(&retry, &binding.cohort[0])
            .is_ok());
    }

    #[test]
    fn publication_permission_failure_is_definitely_precommit_and_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let store = WindowSessionStore::new(dir.path());
        let before = store
            .accept_window(&store.load().unwrap(), WindowOwnerId::new(), None)
            .unwrap()
            .snapshot;
        let live = &before.live_windows()[0];
        let binding = GuardedQuitBinding {
            transaction_id: "precommit-quit".into(),
            cohort: vec![GuardedQuitWindow {
                owner: live.token.owner().clone(),
                generation: live.token.generation(),
                owner_revision: live.token.revision(),
                window_id: "actual-endpoint".into(),
                project: None,
                guard: QuitUiGuard {
                    nonce: "actual-ui-nonce".into(),
                    state: "actual-ui-state".into(),
                },
            }],
        };
        let bytes = std::fs::read(store.path()).unwrap();
        let mut permissions = std::fs::metadata(store.path()).unwrap().permissions();
        let original_permissions = permissions.clone();
        permissions.set_readonly(true);
        std::fs::set_permissions(store.path(), permissions).unwrap();
        let result = store.commit_guarded_quit(store.prepare_quit(&before).unwrap(), binding);
        std::fs::set_permissions(store.path(), original_permissions).unwrap();
        assert!(matches!(result, Err(WindowSessionError::Io { .. })));
        assert_eq!(std::fs::read(store.path()).unwrap(), bytes);
        assert_eq!(store.load().unwrap().live_windows(), before.live_windows());
    }
}

#[cfg(all(test, unix))]
mod guarded_quit_swapped_path_tests {
    use super::*;
    use std::{
        ffi::CString,
        os::unix::{
            ffi::OsStrExt,
            fs::{symlink, OpenOptionsExt},
        },
        sync::mpsc,
    };

    fn ready(store: &WindowSessionStore) -> (WindowSessionSnapshot, GuardedQuitBinding) {
        let before = store
            .accept_window(&store.load().unwrap(), WindowOwnerId::new(), None)
            .unwrap()
            .snapshot;
        let live = &before.live_windows()[0];
        let binding = GuardedQuitBinding {
            transaction_id: "swapped-path-quit".into(),
            cohort: vec![GuardedQuitWindow {
                owner: live.token.owner().clone(),
                generation: live.token.generation(),
                owner_revision: live.token.revision(),
                window_id: "actual-endpoint".into(),
                project: None,
                guard: QuitUiGuard {
                    nonce: "actual-ui-nonce".into(),
                    state: "actual-ui-state".into(),
                },
            }],
        };
        (before, binding)
    }
    // Rescue an old blocking open after a finite delay. This makes the test
    // diagnose the old FIFO hang without hanging its runner or leaking a thread.
    fn replace_with_fifo(path: &Path) -> (mpsc::Sender<()>, std::thread::JoinHandle<()>) {
        std::fs::remove_file(path).unwrap();
        let cpath = CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        let (stop, stopped) = mpsc::channel();
        let path = path.to_path_buf();
        let rescue = std::thread::spawn(move || {
            if stopped.recv_timeout(Duration::from_millis(400)).is_err() {
                let _rescue = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
                    .open(path)
                    .unwrap();
                let _ = stopped.recv_timeout(Duration::from_secs(2));
            }
        });
        (stop, rescue)
    }

    #[test]
    fn fifo_swapped_after_actual_publication_is_bounded_uncertain_then_retryable() {
        let dir = tempfile::tempdir().unwrap();
        let store = WindowSessionStore::new(dir.path());
        let (before, binding) = ready(&store);
        let backup = dir.path().join("installed-checkpoint-backup");
        let result = store.commit_guarded_quit_with_sync(
            store.prepare_quit(&before).unwrap(),
            binding.clone(),
            |path| {
                std::fs::copy(path, &backup).unwrap();
                let (stop, rescue) = replace_with_fifo(path);
                let started = Instant::now();
                let result = sync_published(path);
                let elapsed = started.elapsed();
                let _ = stop.send(());
                rescue.join().unwrap();
                assert!(
                    elapsed < Duration::from_millis(300),
                    "sync blocked on FIFO: {elapsed:?}"
                );
                result
            },
        );
        let Err(WindowSessionError::QuitCommitUncertain { certificate, .. }) = result else {
            panic!("a swapped published checkpoint must remain an uncertain commit");
        };
        std::fs::remove_file(store.path()).unwrap();
        std::fs::rename(backup, store.path()).unwrap();
        assert_eq!(
            store
                .commit_guarded_quit(store.prepare_quit(&before).unwrap(), binding)
                .unwrap(),
            certificate
        );
    }

    #[test]
    fn fifo_swapped_after_verification_read_is_bounded_and_never_verified() {
        let dir = tempfile::tempdir().unwrap();
        let store = WindowSessionStore::new(dir.path());
        let (before, binding) = ready(&store);
        let certificate = store
            .commit_guarded_quit(store.prepare_quit(&before).unwrap(), binding.clone())
            .unwrap();
        let result =
            store.verify_guarded_quit_with_sync(&certificate, &binding.cohort[0], |path| {
                let (stop, rescue) = replace_with_fifo(path);
                let started = Instant::now();
                let result = sync_published(path);
                let elapsed = started.elapsed();
                let _ = stop.send(());
                rescue.join().unwrap();
                assert!(
                    elapsed < Duration::from_millis(300),
                    "verification sync blocked on FIFO: {elapsed:?}"
                );
                result
            });
        assert!(matches!(
            result,
            Err(WindowSessionError::PublicationUncertain { .. })
        ));
    }

    #[test]
    fn symlink_swaps_at_publication_and_verification_do_not_sync_unowned_target() {
        for verifying in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let store = WindowSessionStore::new(dir.path());
            let (before, binding) = ready(&store);
            let outside_dir = tempfile::tempdir().unwrap();
            let outside = outside_dir.path().join("unowned");
            std::fs::write(&outside, "unowned content").unwrap();
            let swap = |path: &Path| {
                std::fs::remove_file(path).unwrap();
                symlink(&outside, path).unwrap();
                sync_published(path)
            };
            if verifying {
                let certificate = store
                    .commit_guarded_quit(store.prepare_quit(&before).unwrap(), binding.clone())
                    .unwrap();
                assert!(matches!(
                    store.verify_guarded_quit_with_sync(&certificate, &binding.cohort[0], swap),
                    Err(WindowSessionError::PublicationUncertain { .. })
                ));
            } else {
                assert!(matches!(
                    store.commit_guarded_quit_with_sync(
                        store.prepare_quit(&before).unwrap(),
                        binding,
                        swap
                    ),
                    Err(WindowSessionError::QuitCommitUncertain { .. })
                ));
            }
            assert!(std::fs::symlink_metadata(store.path())
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(std::fs::read_to_string(outside).unwrap(), "unowned content");
        }
    }
}
