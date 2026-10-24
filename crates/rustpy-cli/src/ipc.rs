//! Single-instance support for the launcher (roadmap §8 "Command-line launcher").
//!
//! The first IDE process for a configuration directory holds an exclusive lock on
//! `<config>/instance.lock` for as long as it runs (`flock` on Unix, an unshared
//! handle on Windows; the OS drops it when the process ends, even after a crash)
//! and listens on a localhost TCP port. The port and a random token are published
//! in `<config>/instance.json`, readable by the user only.
//!
//! A later `rustpy` invocation that finds the lock taken reads that file,
//! connects and sends one JSON line ([`Request`]); the IDE opens the files and
//! answers with one JSON line ([`Response::Ok`]). For `--wait` the connection stays
//! open until the IDE sends [`Response::Closed`] (or exits).
//!
//! A crash leaves `instance.json` behind, but the lock is free again, so the next
//! IDE takes it and rewrites the file; until then a client that reads the stale
//! file cannot connect (or is rejected by an unrelated program on the reused
//! port) and retries until its timeout.

use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::args::OpenTarget;

pub const LOCK_FILE: &str = "instance.lock";
pub const INFO_FILE: &str = "instance.json";
/// Version of the request/response lines.
pub const PROTOCOL: u32 = 1;
/// Longest request line accepted.
const MAX_REQUEST: u64 = 1 << 20;
/// How long the server waits for the IDE to handle a request.
const UI_TIMEOUT: Duration = Duration::from_secs(30);

fn is_false(value: &bool) -> bool {
    !*value
}

/// What `instance.json` holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceInfo {
    /// Explicit capability advertised only after the native project host owns
    /// this primary endpoint. Legacy/ordinary endpoints never receive NEW.
    #[serde(default, skip_serializing_if = "is_false")]
    pub native_project_host: bool,
    pub pid: u32,
    pub port: u16,
    pub token: String,
    #[serde(default)]
    pub version: String,
}

/// One request line from a launcher to the running IDE.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_window: Option<crate::args::OpenArgs>,
    pub protocol: u32,
    pub token: String,
    /// Absolute paths (the IDE's working directory is not the launcher's).
    pub targets: Vec<OpenTarget>,
    #[serde(default)]
    pub wait: bool,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub expected_window: Option<crate::window_registry::ExpectedWindow>,
}

/// One response line from the IDE.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    /// The request was handled; `opened` lists the files and folders opened.
    Ok {
        opened: Vec<PathBuf>,
    },
    /// `--wait`: every opened file's tab (or folder's project) is closed now.
    Closed,
    Error {
        message: String,
    },
}

/// Logical workspace ownership, not evidence of process liveness. The UI must
/// compare this with its actual owner immediately before consuming a control.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuitStateStamp {
    pub owner: String,
    pub generation: u64,
    pub owner_revision: u64,
}

/// Authenticated transient discovery only; never authorizes Prepare or Quit.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerChallenge {
    quit_owner_nonce: String,
    #[serde(deserialize_with = "explicit_window")]
    expected_window: crate::window_registry::ExpectedWindow,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerProof {
    nonce: String,
    #[serde(deserialize_with = "explicit_window")]
    window: crate::window_registry::ExpectedWindow,
    state: QuitStateStamp,
    mac: String,
}
fn owner_mac(token: &str, proof: &OwnerProof) -> io::Result<String> {
    let key = blake3::derive_key("rustpy-quit-owner-token-v1", token.as_bytes());
    let message = serde_json::to_vec(&(
        "rustpy-quit-owner-proof-v1",
        &proof.nonce,
        &proof.window,
        &proof.state,
    ))
    .map_err(io::Error::other)?;
    Ok(blake3::keyed_hash(&key, &message).to_hex().to_string())
}
/// Queries the live endpoint, not registry metadata. The returned stamp remains
/// subject to actual owner validation by Prepare and the guarded workspace store.
pub fn query_window_owner(
    window: &crate::window_registry::WindowInfo,
    timeout: Duration,
) -> io::Result<QuitStateStamp> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| control_error("invalid owner probe deadline"))?;
    let expected = crate::window_registry::ExpectedWindow {
        id: window.id.clone(),
        project: window.project.clone(),
    };
    let challenge = OwnerChallenge {
        quit_owner_nonce: new_token()?,
        expected_window: expected,
    };
    let socket = TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, window.endpoint.port)),
        control_remaining(deadline)?,
    )?;
    let mut bytes = serde_json::to_vec(&challenge).map_err(io::Error::other)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_REQUEST {
        return Err(control_error("owner challenge too large"));
    }
    control_write(&socket, &bytes, deadline)?;
    let proof: OwnerProof = serde_json::from_slice(&control_read_frame(&socket, deadline)?)
        .map_err(io::Error::other)?;
    if proof.nonce != challenge.quit_owner_nonce
        || proof.window != challenge.expected_window
        || !bounded_identity(&proof.state.owner)
        || !same_secret(&owner_mac(&window.endpoint.token, &proof)?, &proof.mac)
    {
        return Err(control_error("invalid authenticated owner proof"));
    }
    Ok(proof.state)
}

/// Opaque UI guard approval. `state` covers document/notebook/workspace state;
/// its interpretation and validation belong to the UI, not this transport.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuitGuardStamp {
    pub nonce: String,
    pub state: String,
}

/// Identifies a published checkpoint. This is not, by itself, a durable binding
/// of a transaction to that checkpoint. A UI must verify the stored checkpoint,
/// its complete cohort and transaction binding before accepting Commit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuitCommitCertificate {
    pub transaction_id: String,
    pub checkpoint_generation: u64,
    pub checkpoint_revision: String,
    pub cohort: Vec<QuitPreparedWindow>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuitPreparedWindow {
    #[serde(deserialize_with = "explicit_window")]
    pub window: crate::window_registry::ExpectedWindow,
    pub state: QuitStateStamp,
    pub guard: QuitGuardStamp,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub enum QuitControl {
    Prepare,
    PollPrepared,
    Abort,
    Commit {
        guard: QuitGuardStamp,
        certificate: QuitCommitCertificate,
    },
}

/// Separate from OPEN requests: neither OPEN's ACK nor an enqueued control is
/// approval to exit. `quit_control` is the explicit wire discriminator.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlRequest {
    pub protocol: u32,
    pub token: String,
    /// Per-phase UI handoff budget, not a deadline for the user's entire Quit.
    pub timeout_ms: u64,
    #[serde(deserialize_with = "explicit_window")]
    pub expected_window: crate::window_registry::ExpectedWindow,
    pub transaction_id: String,
    pub expected_state: QuitStateStamp,
    pub quit_control: QuitControl,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlStatus {
    Pending,
    Prepared { guard: QuitGuardStamp },
    Aborted,
    Committed { certificate: QuitCommitCertificate },
    Rejected { message: String },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlResponse {
    pub transaction_id: String,
    pub status: ControlStatus,
}

fn control_error(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}
fn bounded_identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
fn explicit_window<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<crate::window_registry::ExpectedWindow, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    if value.get("project").is_none() {
        return Err(serde::de::Error::custom(
            "expected project is required (null for Welcome)",
        ));
    }
    serde_json::from_value(value).map_err(serde::de::Error::custom)
}
impl ControlRequest {
    fn validate(&self) -> io::Result<()> {
        if self.protocol != PROTOCOL {
            return Err(control_error("unsupported control protocol"));
        }
        if self.timeout_ms == 0 || self.timeout_ms > 30_000 {
            return Err(control_error("invalid control handoff timeout"));
        }
        if !bounded_identity(&self.transaction_id)
            || !bounded_identity(&self.expected_state.owner)
            || !bounded_identity(&self.expected_window.id)
        {
            return Err(control_error("invalid control identity"));
        }
        if let QuitControl::Commit { guard, certificate } = &self.quit_control {
            validate_guard(guard)?;
            validate_certificate(certificate)?;
            if certificate.transaction_id != self.transaction_id
                || !certificate.cohort.iter().any(|entry| {
                    entry.window == self.expected_window
                        && entry.state == self.expected_state
                        && &entry.guard == guard
                })
            {
                return Err(control_error(
                    "checkpoint certificate does not bind this prepared window",
                ));
            }
        }
        Ok(())
    }
}
fn validate_guard(guard: &QuitGuardStamp) -> io::Result<()> {
    if !bounded_identity(&guard.nonce) || !bounded_identity(&guard.state) {
        return Err(control_error("invalid quit guard stamp"));
    }
    Ok(())
}
fn validate_certificate(certificate: &QuitCommitCertificate) -> io::Result<()> {
    if !bounded_identity(&certificate.checkpoint_revision)
        || !bounded_identity(&certificate.transaction_id)
        || certificate.cohort.is_empty()
        || certificate.cohort.len() > 256
    {
        return Err(control_error("invalid checkpoint revision"));
    }
    let mut windows = std::collections::HashSet::new();
    let mut owners = std::collections::HashSet::new();
    for entry in &certificate.cohort {
        validate_guard(&entry.guard)?;
        if !bounded_identity(&entry.window.id)
            || !bounded_identity(&entry.state.owner)
            || !windows.insert(&entry.window.id)
            || !owners.insert(&entry.state.owner)
            || entry.state.generation.checked_add(1) != Some(certificate.checkpoint_generation)
        {
            return Err(control_error("invalid or duplicate checkpoint cohort"));
        }
    }
    Ok(())
}
fn valid_control_response(request: &ControlRequest, response: &ControlResponse) -> bool {
    if response.transaction_id != request.transaction_id {
        return false;
    }
    match (&request.quit_control, &response.status) {
        (_, ControlStatus::Rejected { .. }) => true,
        (QuitControl::Prepare | QuitControl::PollPrepared, ControlStatus::Pending) => true,
        (QuitControl::Prepare | QuitControl::PollPrepared, ControlStatus::Prepared { guard }) => {
            validate_guard(guard).is_ok()
        }
        (QuitControl::Abort, ControlStatus::Aborted) => true,
        (
            QuitControl::Commit { certificate, .. },
            ControlStatus::Committed {
                certificate: actual,
            },
        ) => certificate == actual,
        _ => false,
    }
}

/// A short-lived request to the UI, separate from OPEN's queue. A network
/// timeout invalidates this request, not an already-created UI transaction.
#[derive(Debug)]
pub struct IncomingControl {
    pub request: ControlRequest,
    deadline: Instant,
    expired: Arc<AtomicBool>,
    reply: mpsc::Sender<ControlReply>,
}
struct ControlReply {
    response: ControlResponse,
    deadline: Instant,
    delivered: Option<mpsc::Sender<io::Result<()>>>,
}
/// Completion of the server's actual response write and flush. This is not
/// evidence that the coordinator received or approved the response.
#[derive(Debug)]
pub struct ControlDelivery {
    result: mpsc::Receiver<io::Result<()>>,
}
impl ControlDelivery {
    pub fn try_result(&self) -> Option<io::Result<()>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "control delivery was not completed",
            ))),
        }
    }
}
impl IncomingControl {
    /// Check at the UI consumption boundary, including explicit Welcome None.
    /// Checking endpoint metadata on the listener thread alone is insufficient.
    pub fn validate_consumption(
        &self,
        window: &crate::window_registry::ExpectedWindow,
        state: &QuitStateStamp,
    ) -> io::Result<()> {
        if self.expired.load(Ordering::SeqCst) || Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "control request expired",
            ));
        }
        self.request.validate()?;
        if &self.request.expected_window != window || &self.request.expected_state != state {
            return Err(control_error(
                "the destination window or logical owner changed",
            ));
        }
        Ok(())
    }
    /// Answers this phase only. Approval must come from the actual UI guard;
    /// replying Pending never releases the window or approves a checkpoint.
    pub fn respond(self, status: ControlStatus) -> io::Result<()> {
        self.respond_inner(status, None)
    }
    /// Enqueue a UI answer and retain separate observation of actual delivery.
    /// The returned handle starts pending; queue acceptance is not delivery.
    pub fn respond_tracked(self, status: ControlStatus) -> io::Result<ControlDelivery> {
        let (sender, result) = mpsc::channel();
        self.respond_inner(status, Some(sender))?;
        Ok(ControlDelivery { result })
    }
    fn respond_inner(
        self,
        status: ControlStatus,
        delivered: Option<mpsc::Sender<io::Result<()>>>,
    ) -> io::Result<()> {
        if self.expired.load(Ordering::SeqCst) || Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "control request expired",
            ));
        }
        let response = ControlResponse {
            transaction_id: self.request.transaction_id.clone(),
            status,
        };
        if !valid_control_response(&self.request, &response) {
            return Err(control_error("response does not match the control phase"));
        }
        self.reply
            .send(ControlReply {
                response,
                deadline: self.deadline,
                delivered,
            })
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "control caller disconnected"))
    }
}

/// Small UI-owned phase ledger. It never runs guards, freezes editors, verifies
/// liveness or writes checkpoints. Those responsibilities remain with the UI.
#[derive(Debug)]
pub struct QuitControlSession {
    transaction_id: String,
    window: crate::window_registry::ExpectedWindow,
    state: QuitStateStamp,
    status: ControlStatus,
}
impl QuitControlSession {
    pub fn begin(request: &ControlRequest) -> io::Result<Self> {
        request.validate()?;
        if request.quit_control != QuitControl::Prepare {
            return Err(control_error("transaction must begin with Prepare"));
        }
        Ok(Self {
            transaction_id: request.transaction_id.clone(),
            window: request.expected_window.clone(),
            state: request.expected_state.clone(),
            status: ControlStatus::Pending,
        })
    }
    pub fn matches(&self, request: &ControlRequest) -> bool {
        self.transaction_id == request.transaction_id
            && self.window == request.expected_window
            && self.state == request.expected_state
    }
    pub fn status(&self) -> ControlStatus {
        self.status.clone()
    }
    /// Call only after every actual local guard accepted and the UI froze the
    /// stamped state. Repeating the exact approval is idempotent.
    pub fn approve(&mut self, guard: QuitGuardStamp) -> io::Result<()> {
        validate_guard(&guard)?;
        match &self.status {
            ControlStatus::Pending => self.status = ControlStatus::Prepared { guard },
            ControlStatus::Prepared { guard: current } if current == &guard => {}
            _ => return Err(control_error("quit transaction cannot be prepared")),
        }
        Ok(())
    }
    pub fn abort(&mut self, request: &ControlRequest) -> io::Result<()> {
        if !self.matches(request) || request.quit_control != QuitControl::Abort {
            return Err(control_error("wrong abort transaction"));
        }
        match self.status {
            ControlStatus::Pending | ControlStatus::Prepared { .. } | ControlStatus::Aborted => {
                self.status = ControlStatus::Aborted
            }
            _ => return Err(control_error("a published quit cannot be aborted")),
        }
        Ok(())
    }
    /// Call only AFTER verifying the actual durable checkpoint, exact cohort,
    /// transaction binding and still-frozen UI state. Transport cannot establish
    /// that fact. Repeated exact Commit is idempotent; conflicting ones fail.
    pub fn commit_after_checkpoint(&mut self, request: &ControlRequest) -> io::Result<()> {
        request.validate()?;
        if !self.matches(request) {
            return Err(control_error("wrong commit transaction"));
        }
        let QuitControl::Commit { guard, certificate } = &request.quit_control else {
            return Err(control_error("expected Commit"));
        };
        if self.state.generation.checked_add(1) != Some(certificate.checkpoint_generation) {
            return Err(control_error(
                "checkpoint generation does not retire this owner",
            ));
        }
        match &self.status {
            ControlStatus::Prepared { guard: approved } if approved == guard => {
                self.status = ControlStatus::Committed {
                    certificate: certificate.clone(),
                }
            }
            ControlStatus::Committed {
                certificate: current,
            } if current == certificate => {}
            _ => return Err(control_error("quit was not prepared or commit conflicts")),
        }
        Ok(())
    }
}

fn lock_path(config_dir: &Path) -> PathBuf {
    config_dir.join(LOCK_FILE)
}

fn info_path(config_dir: &Path) -> PathBuf {
    config_dir.join(INFO_FILE)
}

/// Owns a successfully acquired lease; failed contenders never construct this guard.
/// Deliberately non-cloneable: one owner releases this open file description.
#[derive(Debug)]
pub(crate) struct FileLease {
    _file: File,
}

impl FileLease {
    /// Inspect the retained descriptor without duplicating its owning guard.
    #[cfg(unix)]
    pub(crate) fn metadata(&self) -> io::Result<std::fs::Metadata> {
        self._file.metadata()
    }
    /// Fixture-only access for an explicit kernel unlock; never clones a guard.
    #[cfg(test)]
    pub(crate) fn borrowed_file_for_test(&self) -> &File {
        &self._file
    }
}

#[cfg(unix)]
impl Drop for FileLease {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        // Closing alone does not release flock while a forked child still holds
        // the same open file description. Unlock that description before close.
        // SAFETY: this guard owns the live descriptor until Drop returns.
        let _ = unsafe { libc::flock(self._file.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// The lock of the IDE instance for a configuration directory, held until dropped.
#[derive(Debug)]
pub struct InstanceLock {
    _file: FileLease,
    config_dir: PathBuf,
}

impl InstanceLock {
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }
}

/// The state of the instance lock of a configuration directory.
#[derive(Debug)]
pub enum Probe {
    /// No IDE runs for the directory; the caller holds the lock now.
    Free(InstanceLock),
    /// Another process holds the lock: an IDE runs (or is starting).
    Running,
}

/// Tries to take the instance lock of `config_dir` (created if needed).
pub fn probe(config_dir: &Path) -> io::Result<Probe> {
    std::fs::create_dir_all(config_dir)?;
    Ok(match try_lock_file(&lock_path(config_dir))? {
        Some(file) => Probe::Free(InstanceLock {
            _file: file,
            config_dir: config_dir.to_path_buf(),
        }),
        None => Probe::Running,
    })
}

pub(crate) fn try_lock_file(path: &Path) -> io::Result<Option<FileLease>> {
    lock_file(path, true)
}
/// Discovery never creates a lease on behalf of a starting window.
pub(crate) fn try_existing_lock_file(path: &Path) -> io::Result<Option<FileLease>> {
    lock_file(path, false)
}

#[cfg(unix)]
fn lock_file(path: &Path, create: bool) -> io::Result<Option<FileLease>> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "instance lease must be a regular file",
        ));
    }
    // SAFETY: `flock` only reads the descriptor, which `file` keeps open.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(Some(FileLease { _file: file }));
    }
    let err = io::Error::last_os_error();
    if err.kind() == io::ErrorKind::WouldBlock {
        Ok(None)
    } else {
        Err(err)
    }
}

#[cfg(windows)]
fn lock_file(path: &Path, create: bool) -> io::Result<Option<FileLease>> {
    use std::os::windows::fs::OpenOptionsExt;
    // An unshared handle: other opens fail until this process closes it or ends.
    match OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .share_mode(0)
        .open(path)
    {
        Ok(file) => Ok(Some(FileLease { _file: file })),
        // ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION
        Err(e) if matches!(e.raw_os_error(), Some(32) | Some(33)) => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(not(any(unix, windows)))]
fn lock_file(path: &Path, create: bool) -> io::Result<Option<FileLease>> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        .open(path)
        .map(|file| Some(FileLease { _file: file }))
}

/// A new random token (64 hex digits).
pub(crate) fn new_token() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| io::Error::other(format!("no random numbers: {e}")))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Compares secrets without stopping at the first difference.
pub(crate) fn same_secret(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

/// Writes `instance.json` (owner-only on Unix) through a temporary file.
fn write_info(config_dir: &Path, info: &InstanceInfo) -> io::Result<()> {
    let path = info_path(config_dir);
    let tmp = config_dir.join(format!("{INFO_FILE}.{}.tmp", new_token()?));
    {
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(
            serde_json::to_string(info)
                .map_err(io::Error::other)?
                .as_bytes(),
        )?;
        f.sync_all().ok();
    }
    crate::window_registry::replace_file(&tmp, &path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Reads `instance.json` of `config_dir`.
pub fn read_info(config_dir: &Path) -> io::Result<InstanceInfo> {
    let bytes = std::fs::read(info_path(config_dir))?;
    serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// A request received by the running IDE. Answer it with [`Incoming::accept`]
/// or [`Incoming::reject`] (dropping it also refuses it).
#[derive(Debug)]
pub struct Incoming {
    pub native_window: Option<crate::args::OpenArgs>,
    pub targets: Vec<OpenTarget>,
    pub wait: bool,
    pub cwd: Option<PathBuf>,
    /// Routed request precondition; validate again at the UI consumption boundary.
    pub expected_project: Option<PathBuf>,
    reply: mpsc::Sender<Result<Vec<PathBuf>, String>>,
    closed: Option<mpsc::Sender<()>>,
}

impl Incoming {
    /// Reports what was opened. For a `--wait` request the returned handle must
    /// be finished once those files' tabs are closed. If the server has stopped
    /// waiting for this queued reply, no wait handle is returned.
    pub fn accept(self, opened: Vec<PathBuf>) -> Option<WaitHandle> {
        self.reply.send(Ok(opened)).ok()?;
        self.closed.map(|tx| WaitHandle { tx })
    }

    /// Refuses this OPEN request explicitly, including a `--wait` request.
    /// No successful acknowledgement or wait handle is produced.
    pub fn reject(self, message: impl Into<String>) {
        let _ = self.reply.send(Err(message.into()));
    }
}

/// A launcher waiting (`--wait`) for its files to be closed.
#[derive(Debug)]
pub struct WaitHandle {
    tx: mpsc::Sender<()>,
}

impl WaitHandle {
    /// Tells the launcher that the files are closed; it then exits.
    pub fn finish(self) {
        let _ = self.tx.send(());
    }
}

type Waker = Arc<Mutex<Option<Box<dyn Fn() + Send>>>>;

/// The listening side, owned by the IDE that holds the instance lock.
type OwnerWindowState = (
    Arc<Mutex<crate::window_registry::Registration>>,
    Arc<Mutex<Option<QuitStateStamp>>>,
);

pub struct Server {
    rx: mpsc::Receiver<Incoming>,
    control_rx: mpsc::Receiver<IncomingControl>,
    waker: Waker,
    port: u16,
    token: String,
    shutdown: Arc<AtomicBool>,
    config_dir: PathBuf,
    _lock: Option<InstanceLock>,
    registration: Arc<Mutex<crate::window_registry::Registration>>,
    quit_state: Arc<Mutex<Option<QuitStateStamp>>>,
    #[cfg(test)]
    before_transport_shutdown: Option<(mpsc::Sender<bool>, mpsc::Receiver<()>)>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("port", &self.port)
            .field("config_dir", &self.config_dir)
            .finish()
    }
}

impl Server {
    /// Listens on a free localhost port and publishes it with a new token in
    /// `instance.json`. The lock is released again when this fails.
    pub fn start(lock: InstanceLock) -> io::Result<Server> {
        Self::start_window(lock.config_dir.clone(), Some(lock))
    }

    /// Every secondary window owns an endpoint and OS liveness lease too.
    pub fn start_secondary(config_dir: &Path) -> io::Result<Server> {
        Self::start_window(config_dir.to_path_buf(), None)
    }
    fn start_window(config_dir: PathBuf, lock: Option<InstanceLock>) -> io::Result<Server> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        let token = new_token()?;
        let info = InstanceInfo {
            native_project_host: false,
            pid: std::process::id(),
            port,
            token: token.clone(),
            version: crate::help::VERSION.to_string(),
        };
        let registration = Arc::new(Mutex::new(crate::window_registry::Registration::create(
            &config_dir,
            info.clone(),
        )?));
        if lock.is_some() {
            write_info(&config_dir, &info)?;
        }
        let quit_state = Arc::new(Mutex::new(None));
        let (tx, rx) = mpsc::channel();
        let (control_tx, control_rx) = mpsc::channel();
        let waker: Waker = Arc::new(Mutex::new(None));
        let shutdown = Arc::new(AtomicBool::new(false));
        {
            let registration = registration.clone();
            let quit_state = quit_state.clone();
            let (token, waker, shutdown) = (token.clone(), waker.clone(), shutdown.clone());
            std::thread::Builder::new()
                .name("rustpy-ipc".into())
                .spawn(move || {
                    accept_loop(
                        listener,
                        token,
                        tx,
                        control_tx,
                        waker,
                        shutdown,
                        (registration, quit_state),
                    )
                })?;
        }
        log::info!(
            "single instance: listening on 127.0.0.1:{port} for {}",
            config_dir.display()
        );
        Ok(Server {
            rx,
            control_rx,
            waker,
            port,
            token,
            shutdown,
            config_dir,
            _lock: lock,
            registration,
            quit_state,
            #[cfg(test)]
            before_transport_shutdown: None,
        })
    }

    /// Separate the process host from logical project windows. The router owns
    /// the primary instance lease but is not a Quit/save participant. Called
    /// under startup admission before creating any logical window owner.
    pub fn become_native_project_router(&self) -> io::Result<()> {
        let mut registration = self
            .registration
            .lock()
            .map_err(|_| io::Error::other("window registry poisoned"))?;
        if self
            .quit_state
            .lock()
            .map_err(|_| io::Error::other("owner state poisoned"))?
            .is_some()
        {
            return Err(io::Error::other(
                "A logical window owner cannot become a process router",
            ));
        }
        registration.info.endpoint.native_project_host = true;
        registration.detach_from_discovery()?;
        if self._lock.is_some() {
            write_info(&self.config_dir, &registration.info.endpoint)?;
        }
        Ok(())
    }
    /// Publishes only the actual accepted logical owner. No persistence or approval.
    pub fn set_quit_state(&self, state: Option<QuitStateStamp>) -> io::Result<()> {
        if state
            .as_ref()
            .is_some_and(|state| !bounded_identity(&state.owner))
        {
            return Err(control_error("invalid owner identity"));
        }
        let _registration = self
            .registration
            .lock()
            .map_err(|_| io::Error::other("window registry poisoned"))?;
        *self
            .quit_state
            .lock()
            .map_err(|_| io::Error::other("owner state poisoned"))? = state;
        Ok(())
    }
    pub fn set_project(&self, project: Option<&Path>) -> io::Result<()> {
        let mut registration = self
            .registration
            .lock()
            .map_err(|_| io::Error::other("window registry poisoned"))?;
        let old = registration.info.project.clone();
        let result = registration.set_project(project);
        if old != registration.info.project {
            *self
                .quit_state
                .lock()
                .map_err(|_| io::Error::other("owner state poisoned"))? = None;
        }
        result
    }
    pub fn window_id(&self) -> String {
        self.registration
            .lock()
            .expect("window registry poisoned")
            .info
            .id
            .clone()
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Called (from the listener thread) after each request is queued, so an
    /// idle UI wakes up and handles it.
    pub fn set_waker(&self, wake: impl Fn() + Send + 'static) {
        if let Ok(mut w) = self.waker.lock() {
            *w = Some(Box::new(wake));
        }
    }

    /// The next queued request, if any.
    pub fn try_recv(&self) -> Option<Incoming> {
        self.rx.try_recv().ok()
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Option<Incoming> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// Typed Quit phases never appear in the OPEN queue or acknowledge files.
    pub fn try_recv_control(&self) -> Option<IncomingControl> {
        self.control_rx.try_recv().ok()
    }
    pub fn recv_control_timeout(&self, timeout: Duration) -> Option<IncomingControl> {
        self.control_rx.recv_timeout(timeout).ok()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Ok(mut registration) = self.registration.lock() {
            // Client/accept threads may still retain this Arc after the logical
            // window closes. Release its discovery lease now, rather than leave
            // a busy, recordless registration until the last thread exits.
            let _ = registration.detach_from_discovery();
        }
        // Revoke discovery before the accept loop can reject a challenge. If
        // transport shutdown wins first, a concurrent observer can receive a
        // reset while both the published record and its OS lease still say live.
        #[cfg(test)]
        if let Some((paused, release)) = self.before_transport_shutdown.take() {
            let _ = paused.send(self.shutdown.load(Ordering::SeqCst));
            let _ = release.recv();
        }
        self.shutdown.store(true, Ordering::SeqCst);
        // Wake the accept loop so its thread ends.
        let _ = TcpStream::connect_timeout(
            &SocketAddr::from((Ipv4Addr::LOCALHOST, self.port)),
            Duration::from_millis(200),
        );
        // Remove instance.json unless another instance has replaced it.
        if self._lock.is_some()
            && read_info(&self.config_dir)
                .is_ok_and(|i| i.port == self.port && i.token == self.token)
        {
            let _ = std::fs::remove_file(info_path(&self.config_dir));
        }
    }
}

fn accept_loop(
    listener: TcpListener,
    token: String,
    tx: mpsc::Sender<Incoming>,
    control_tx: mpsc::Sender<IncomingControl>,
    waker: Waker,
    shutdown: Arc<AtomicBool>,
    window_state: OwnerWindowState,
) {
    let (registration, quit_state) = window_state;
    for stream in listener.incoming() {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = stream else { continue };
        let registration = registration.clone();
        let quit_state = quit_state.clone();
        let (token, tx, control_tx, waker) =
            (token.clone(), tx.clone(), control_tx.clone(), waker.clone());
        let spawned = std::thread::Builder::new()
            .name("rustpy-ipc-client".into())
            .spawn(move || {
                if let Err(e) = serve(
                    stream,
                    &token,
                    &tx,
                    &control_tx,
                    &waker,
                    &registration,
                    &quit_state,
                ) {
                    log::debug!("single instance: request failed: {e}");
                }
            });
        if let Err(e) = spawned {
            log::warn!("single instance: cannot serve a request: {e}");
        }
    }
}

fn write_line(stream: &mut TcpStream, response: &Response) -> io::Result<()> {
    let mut line = serde_json::to_string(response).map_err(io::Error::other)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()
}

fn write_control(stream: &mut TcpStream, response: &ControlResponse) -> io::Result<()> {
    let mut line = serde_json::to_vec(response).map_err(io::Error::other)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    stream.flush()
}

fn serve_control(
    stream: &mut TcpStream,
    value: serde_json::Value,
    token: &str,
    tx: &mpsc::Sender<IncomingControl>,
    waker: &Waker,
    registration: &Arc<Mutex<crate::window_registry::Registration>>,
) -> io::Result<()> {
    let transaction_id = value
        .get("transaction_id")
        .and_then(serde_json::Value::as_str)
        .filter(|id| bounded_identity(id))
        .unwrap_or_default()
        .to_owned();
    let reject = |stream: &mut TcpStream, message: String| {
        write_control(
            stream,
            &ControlResponse {
                transaction_id: transaction_id.clone(),
                status: ControlStatus::Rejected { message },
            },
        )
    };
    // Serde's Option accepts an omitted field. On controls, Welcome None must
    // be an explicit precondition, never confused with no precondition.
    if value
        .get("expected_window")
        .and_then(|window| window.get("project"))
        .is_none()
    {
        return reject(
            stream,
            "control requires an explicit expected project (null for Welcome)".into(),
        );
    }
    let request: ControlRequest = match serde_json::from_value(value) {
        Ok(request) => request,
        Err(error) => return reject(stream, format!("bad control request: {error}")),
    };
    if let Err(error) = request.validate() {
        return reject(stream, error.to_string());
    }
    if !same_secret(&request.token, token) {
        return reject(stream, "invalid token".into());
    }
    {
        let registration = registration
            .lock()
            .map_err(|_| io::Error::other("window registry poisoned"))?;
        if request.expected_window.id != registration.info.id
            || request.expected_window.project != registration.info.project
        {
            return reject(
                stream,
                "the destination window changed identity or project".into(),
            );
        }
    }
    let timeout = Duration::from_millis(request.timeout_ms);
    let deadline = Instant::now() + timeout;
    let (reply_tx, reply_rx) = mpsc::channel();
    let expired = Arc::new(AtomicBool::new(false));
    let incoming = IncomingControl {
        request: request.clone(),
        deadline,
        expired: Arc::clone(&expired),
        reply: reply_tx,
    };
    if tx.send(incoming).is_err() {
        return reject(stream, "the IDE is shutting down".into());
    }
    if let Ok(waker) = waker.lock() {
        if let Some(wake) = waker.as_ref() {
            wake();
        }
    }
    let response = match reply_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(reply) if valid_control_response(&request, &reply.response) => reply,
        Ok(_) => {
            expired.store(true, Ordering::SeqCst);
            return reject(stream, "invalid UI control response".into());
        }
        Err(_) => {
            expired.store(true, Ordering::SeqCst);
            return reject(stream, "the UI did not answer this control phase".into());
        }
    };
    deliver_control_reply(stream, response)
}

fn deliver_control_reply(stream: &mut TcpStream, reply: ControlReply) -> io::Result<()> {
    let result: io::Result<()> = (|| {
        control_remaining(reply.deadline)?;
        let mut line = serde_json::to_vec(&reply.response).map_err(io::Error::other)?;
        line.push(b'\n');
        control_write(stream, &line, reply.deadline)?;
        stream.set_write_timeout(Some(control_remaining(reply.deadline)?))?;
        stream.flush()?;
        control_remaining(reply.deadline)?;
        Ok(())
    })();
    if let Some(delivered) = reply.delivered {
        let observed = result
            .as_ref()
            .map(|_| ())
            .map_err(|error| io::Error::new(error.kind(), error.to_string()));
        let _ = delivered.send(observed);
    }
    result
}

fn serve(
    mut stream: TcpStream,
    token: &str,
    tx: &mpsc::Sender<Incoming>,
    control_tx: &mpsc::Sender<IncomingControl>,
    waker: &Waker,
    registration: &Arc<Mutex<crate::window_registry::Registration>>,
    quit_state: &Arc<Mutex<Option<QuitStateStamp>>>,
) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut line = String::new();
    BufReader::new((&stream).take(MAX_REQUEST)).read_line(&mut line)?;
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
        if value.get("quit_owner_nonce").is_some() {
            let challenge: OwnerChallenge =
                serde_json::from_value(value).map_err(io::Error::other)?;
            if !line.ends_with('\n')
                || line.len() as u64 > MAX_REQUEST
                || challenge.quit_owner_nonce.len() != 64
                || !challenge
                    .quit_owner_nonce
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(control_error("invalid owner challenge"));
            }
            let registration = registration
                .lock()
                .map_err(|_| io::Error::other("window registry poisoned"))?;
            let window = crate::window_registry::ExpectedWindow {
                id: registration.info.id.clone(),
                project: registration.info.project.clone(),
            };
            if window != challenge.expected_window {
                return Err(control_error("owner destination changed"));
            }
            let state = quit_state
                .lock()
                .map_err(|_| io::Error::other("owner state poisoned"))?
                .clone()
                .ok_or_else(|| control_error("owner state is unpublished"))?;
            let mut proof = OwnerProof {
                nonce: challenge.quit_owner_nonce,
                window,
                state,
                mac: String::new(),
            };
            proof.mac = owner_mac(token, &proof)?;
            let mut bytes = serde_json::to_vec(&proof).map_err(io::Error::other)?;
            bytes.push(b'\n');
            return control_write(&stream, &bytes, Instant::now() + Duration::from_secs(10));
        }
        if value.get("quit_control").is_some() {
            if !line.ends_with('\n') || line.len() as u64 > MAX_REQUEST {
                return write_control(
                    &mut stream,
                    &ControlResponse {
                        transaction_id: value
                            .get("transaction_id")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        status: ControlStatus::Rejected {
                            message: "invalid control request frame".into(),
                        },
                    },
                );
            }
            return serve_control(&mut stream, value, token, control_tx, waker, registration);
        }
        if let Some(nonce) = value
            .get("window_nonce")
            .and_then(|nonce| nonce.as_str())
            .filter(|nonce| nonce.len() == 64 && nonce.bytes().all(|c| c.is_ascii_hexdigit()))
        {
            let info = &registration
                .lock()
                .map_err(|_| io::Error::other("window registry poisoned"))?
                .info;
            let proof = crate::window_registry::proof(info, nonce.to_string())?;
            writeln!(
                stream,
                "{}",
                serde_json::to_string(&proof).map_err(io::Error::other)?
            )?;
            return stream.flush();
        }
    }
    let request: Request = match serde_json::from_str(line.trim_end()) {
        Ok(r) => r,
        Err(e) => {
            return write_line(
                &mut stream,
                &Response::Error {
                    message: format!("bad request: {e}"),
                },
            );
        }
    };
    if request.protocol != PROTOCOL {
        return write_line(
            &mut stream,
            &Response::Error {
                message: format!(
                    "protocol {} is not supported (this IDE speaks {PROTOCOL})",
                    request.protocol
                ),
            },
        );
    }
    if !same_secret(&request.token, token) {
        return write_line(
            &mut stream,
            &Response::Error {
                message: "invalid token".into(),
            },
        );
    }
    if request.native_window.as_ref().is_some_and(|args| {
        !args.new_window
            || args.targets != request.targets
            || args.wait != request.wait
            || request.expected_window.is_some()
            || args.targets.len() > 256
    }) {
        return write_line(
            &mut stream,
            &Response::Error {
                message: "Invalid native project window request".into(),
            },
        );
    }
    if let Some(expected) = &request.expected_window {
        let registration = registration
            .lock()
            .map_err(|_| io::Error::other("window registry poisoned"))?;
        if expected.id != registration.info.id || expected.project != registration.info.project {
            return write_line(
                &mut stream,
                &Response::Error {
                    message: "the destination window changed project".into(),
                },
            );
        }
    }
    let (reply_tx, reply_rx) = mpsc::channel();
    let (closed_tx, closed_rx) = mpsc::channel();
    let incoming = Incoming {
        native_window: request.native_window,
        targets: request.targets,
        wait: request.wait,
        expected_project: request.expected_window.and_then(|window| window.project),
        cwd: request.cwd,
        reply: reply_tx,
        closed: request.wait.then_some(closed_tx),
    };
    if tx.send(incoming).is_err() {
        return write_line(
            &mut stream,
            &Response::Error {
                message: "the IDE is shutting down".into(),
            },
        );
    }
    if let Ok(w) = waker.lock() {
        if let Some(wake) = w.as_ref() {
            wake();
        }
    }
    let Some(opened) = receive_open_reply(&mut stream, &reply_rx, UI_TIMEOUT)? else {
        return Ok(());
    };
    write_line(&mut stream, &Response::Ok { opened })?;
    if request.wait {
        // Until the files are closed; an error means the IDE dropped the handle (exit).
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        if closed_rx.recv().is_ok() {
            write_line(&mut stream, &Response::Closed)?;
        }
    }
    Ok(())
}

fn receive_open_reply(
    stream: &mut TcpStream,
    reply: &mpsc::Receiver<Result<Vec<PathBuf>, String>>,
    timeout: Duration,
) -> io::Result<Option<Vec<PathBuf>>> {
    let message = match reply.recv_timeout(timeout) {
        Ok(Ok(opened)) => return Ok(Some(opened)),
        Ok(Err(message)) => message,
        Err(_) => "the IDE did not handle the request".into(),
    };
    write_line(stream, &Response::Error { message })?;
    Ok(None)
}

/// Why a request could not be forwarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForwardError {
    /// Nothing answered before the timeout (no IDE, a stale `instance.json`, a
    /// hung IDE): the last problem seen.
    Unreachable(String),
    /// The IDE answered with an error.
    Rejected(String),
}

impl std::fmt::Display for ForwardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ForwardError::Unreachable(m) => write!(f, "the running IDE does not answer ({m})"),
            ForwardError::Rejected(m) => write!(f, "the running IDE refused the request: {m}"),
        }
    }
}

impl std::error::Error for ForwardError {}

/// A request the running IDE accepted.
#[derive(Debug)]
pub struct Forwarded {
    pub opened: Vec<PathBuf>,
    reader: BufReader<TcpStream>,
}

impl Forwarded {
    /// Blocks until the IDE reports the files closed, or the connection ends
    /// (the IDE exited).
    pub fn wait_closed(mut self) -> io::Result<()> {
        self.reader.get_ref().set_read_timeout(None)?;
        let mut line = String::new();
        loop {
            line.clear();
            if self.reader.read_line(&mut line)? == 0 {
                return Ok(());
            }
            match serde_json::from_str::<Response>(line.trim_end()) {
                Ok(Response::Closed) => return Ok(()),
                Ok(Response::Error { message }) => return Err(io::Error::other(message)),
                _ => {}
            }
        }
    }
}

enum SendError {
    Io(io::Error),
    Rejected(String),
}

fn send(info: &InstanceInfo, request: &Request) -> Result<Forwarded, SendError> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, info.port));
    let stream =
        TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(SendError::Io)?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(SendError::Io)?;
    // Longer than the server's own wait for the UI.
    stream
        .set_read_timeout(Some(UI_TIMEOUT + Duration::from_secs(10)))
        .map_err(SendError::Io)?;
    let mut line =
        serde_json::to_string(request).map_err(|e| SendError::Io(io::Error::other(e)))?;
    line.push('\n');
    (&stream)
        .write_all(line.as_bytes())
        .map_err(SendError::Io)?;
    let mut reader = BufReader::new(stream);
    let mut answer = String::new();
    if reader.read_line(&mut answer).map_err(SendError::Io)? == 0 {
        return Err(SendError::Io(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the connection was closed",
        )));
    }
    match serde_json::from_str::<Response>(answer.trim_end()) {
        Ok(Response::Ok { opened }) => Ok(Forwarded { opened, reader }),
        Ok(Response::Closed) => Ok(Forwarded {
            opened: Vec::new(),
            reader,
        }),
        Ok(Response::Error { message }) => Err(SendError::Rejected(message)),
        Err(_) => Err(SendError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a RustPy instance",
        ))),
    }
}

/// Create an independent native project window through the advertised primary
/// host. Do not fall back to opening targets in a different existing project.
pub fn forward_native_window(
    config: &Path,
    args: &crate::args::OpenArgs,
    cwd: &Path,
) -> Result<Forwarded, ForwardError> {
    let info = read_info(config).map_err(|error| ForwardError::Unreachable(error.to_string()))?;
    if !info.native_project_host {
        return Err(ForwardError::Rejected(
            "The running IDE does not advertise a native project host".into(),
        ));
    }
    let mut args = args.clone();
    args.targets = absolute_targets(&args.targets, cwd);
    if let Some(path) = &args.open_file {
        if path.is_relative() {
            args.open_file = Some(cwd.join(path));
        }
    }
    let request = Request {
        protocol: PROTOCOL,
        token: info.token.clone(),
        targets: args.targets.clone(),
        wait: args.wait,
        cwd: Some(cwd.into()),
        expected_window: None,
        native_window: Some(args),
    };
    send(&info, &request).map_err(|error| match error {
        SendError::Io(error) => ForwardError::Unreachable(error.to_string()),
        SendError::Rejected(message) => ForwardError::Rejected(message),
    })
}

/// Forwards only to a previously authenticated window, retaining normal ACK,
/// rejection and --wait behavior. Never falls back to a different project.
pub fn forward_window(
    info: &crate::window_registry::WindowInfo,
    targets: &[OpenTarget],
    wait: bool,
    cwd: Option<&Path>,
) -> Result<Forwarded, ForwardError> {
    let verified = crate::window_registry::verify_endpoint(info)
        .map_err(|error| ForwardError::Unreachable(error.to_string()))?;
    if verified.project != info.project {
        return Err(ForwardError::Rejected(
            "the destination window changed project".into(),
        ));
    }
    let request = Request {
        native_window: None,
        expected_window: Some(crate::window_registry::ExpectedWindow {
            id: info.id.clone(),
            project: info.project.clone(),
        }),
        protocol: PROTOCOL,
        token: info.endpoint.token.clone(),
        targets: targets.to_vec(),
        wait,
        cwd: cwd.map(Path::to_path_buf),
    };
    send(&info.endpoint, &request).map_err(|error| match error {
        SendError::Io(error) => ForwardError::Unreachable(error.to_string()),
        SendError::Rejected(message) => ForwardError::Rejected(message),
    })
}

/// Send exactly one Quit phase to an authenticated endpoint. No fallback to a
/// different window, automatic retry, OPEN ACK or EOF is treated as approval.
/// Call this on a background worker: a UI thread must keep servicing controls.
pub fn control_window(
    info: &crate::window_registry::WindowInfo,
    request: &ControlRequest,
    timeout: Duration,
) -> Result<ControlResponse, ForwardError> {
    request
        .validate()
        .map_err(|error| ForwardError::Rejected(error.to_string()))?;
    if request.expected_window.id != info.id
        || request.expected_window.project != info.project
        || !same_secret(&request.token, &info.endpoint.token)
    {
        return Err(ForwardError::Rejected(
            "control does not target this authenticated window".into(),
        ));
    }
    if timeout.is_zero() {
        return Err(ForwardError::Unreachable("control deadline expired".into()));
    }
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| ForwardError::Rejected("invalid control timeout".into()))?;
    // Shared authenticated proof uses the remaining deadline at every socket
    // boundary; caller timeouts include discovery, not just the phase reply.
    let verified = crate::window_registry::verify_endpoint_until(info, deadline)
        .map_err(|error| ForwardError::Unreachable(error.to_string()))?;
    if verified.project != info.project {
        return Err(ForwardError::Rejected(
            "the destination window changed project".into(),
        ));
    }
    let result = (|| -> io::Result<ControlResponse> {
        let stream = TcpStream::connect_timeout(
            &SocketAddr::from((Ipv4Addr::LOCALHOST, info.endpoint.port)),
            control_remaining(deadline)?,
        )?;
        let mut wire = request.clone();
        wire.timeout_ms = wire
            .timeout_ms
            .min(u64::try_from(control_remaining(deadline)?.as_millis()).unwrap_or(u64::MAX));
        if wire.timeout_ms == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "control deadline expired",
            ));
        }
        let mut bytes = serde_json::to_vec(&wire).map_err(io::Error::other)?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_REQUEST {
            return Err(control_error("control request is too large"));
        }
        control_write(&stream, &bytes, deadline)?;
        let line = control_read_frame(&stream, deadline)?;
        let response: ControlResponse = serde_json::from_slice(&line).map_err(io::Error::other)?;
        if !valid_control_response(request, &response) {
            return Err(control_error(
                "control response transaction or phase mismatch",
            ));
        }
        Ok(response)
    })();
    result.map_err(|error| ForwardError::Unreachable(error.to_string()))
}

fn control_remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "control deadline expired"))
}

/// Socket timeouts apply per syscall, not per frame. Recompute the remaining
/// absolute budget before EVERY write/read so a trickling peer cannot renew it.
fn control_write(mut stream: &TcpStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        stream.set_write_timeout(Some(control_remaining(deadline)?))?;
        match stream.write(&bytes[..bytes.len().min(8192)]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "control connection stopped accepting data",
                ));
            }
            Ok(written) => bytes = &bytes[written..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
        control_remaining(deadline)?;
    }
    Ok(())
}

fn control_read_frame(mut stream: &TcpStream, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut frame = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        stream.set_read_timeout(Some(control_remaining(deadline)?))?;
        let count = match stream.read(&mut buffer) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "control connection closed without a complete response",
                ));
            }
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        control_remaining(deadline)?;
        let count = buffer[..count]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(count, |at| at + 1);
        if frame.len() as u64 + count as u64 > MAX_REQUEST {
            return Err(control_error("control response frame is too large"));
        }
        frame.extend_from_slice(&buffer[..count]);
        if frame.last() == Some(&b'\n') {
            return Ok(frame);
        }
    }
}

/// Sends `targets` to the IDE running for `config_dir`, retrying for up to
/// `timeout` while it starts (or replaces a stale `instance.json`).
pub fn forward(
    config_dir: &Path,
    targets: &[OpenTarget],
    wait: bool,
    cwd: Option<&Path>,
    timeout: Duration,
) -> Result<Forwarded, ForwardError> {
    let deadline = Instant::now() + timeout;
    loop {
        let last;
        match read_info(config_dir) {
            Ok(info) => {
                let request = Request {
                    native_window: None,
                    expected_window: None,
                    protocol: PROTOCOL,
                    token: info.token.clone(),
                    targets: targets.to_vec(),
                    wait,
                    cwd: cwd.map(Path::to_path_buf),
                };
                match send(&info, &request) {
                    Ok(f) => return Ok(f),
                    // A token mismatch can be a stale file about to be rewritten.
                    // An explicit UI refusal must not retry and duplicate OPEN.
                    Err(SendError::Rejected(m)) if m == "invalid token" => last = m,
                    Err(SendError::Rejected(m)) => return Err(ForwardError::Rejected(m)),
                    Err(SendError::Io(e)) => last = e.to_string(),
                }
            }
            Err(e) => last = format!("{}: {e}", info_path(config_dir).display()),
        }
        if Instant::now() >= deadline {
            return Err(ForwardError::Unreachable(last));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Makes the targets' paths absolute against `cwd` (and canonical when they exist),
/// so the IDE, whose working directory differs, finds them.
pub fn absolute_targets(targets: &[OpenTarget], cwd: &Path) -> Vec<OpenTarget> {
    targets
        .iter()
        .map(|t| {
            let joined = if t.path.is_absolute() {
                t.path.clone()
            } else {
                cwd.join(&t.path)
            };
            OpenTarget {
                path: dunce::canonicalize(&joined).unwrap_or(joined),
                line: t.line,
                column: t.column,
            }
        })
        .collect()
}

#[cfg(test)]
mod control_tests {
    #[test]
    fn tracked_response_queued_before_deadline_cannot_emit_after_expiry() {
        use super::*;
        let (_dir, _server, window) = fixture();
        let request = request(&window, QuitControl::Prepare);
        let (sender, receiver) = mpsc::channel();
        let deadline = Instant::now() + Duration::from_millis(100);
        let incoming = IncomingControl {
            request,
            deadline,
            expired: Arc::new(AtomicBool::new(false)),
            reply: sender,
        };
        let delivery = incoming.respond_tracked(ControlStatus::Pending).unwrap();
        let reply = receiver.recv().unwrap();
        assert!(delivery.try_result().is_none());
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let mut peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut socket, _) = listener.accept().unwrap();
        while Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(deliver_control_reply(&mut socket, reply).is_err());
        assert_eq!(
            delivery.try_result().unwrap().unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        peer.set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let mut byte = [0];
        assert!(
            peer.read(&mut byte).is_err(),
            "expired response reached the peer"
        );
    }
    #[test]
    fn tracked_response_waits_for_real_write_and_reports_closed_socket_failure() {
        use super::*;
        let (_dir, _server, window) = fixture();
        let request = request(&window, QuitControl::Prepare);
        let (sender, receiver) = mpsc::channel();
        let incoming = IncomingControl {
            request,
            deadline: Instant::now() + Duration::from_secs(2),
            expired: Arc::new(AtomicBool::new(false)),
            reply: sender,
        };
        let delivery = incoming.respond_tracked(ControlStatus::Pending).unwrap();
        assert!(delivery.try_result().is_none());
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let _peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut socket, _) = listener.accept().unwrap();
        socket.shutdown(std::net::Shutdown::Both).unwrap();
        assert!(deliver_control_reply(&mut socket, receiver.recv().unwrap()).is_err());
        assert!(delivery.try_result().unwrap().is_err());
    }
    #[test]
    fn tracked_tcp_reply_reports_actual_emission_separately_from_ui_queue() {
        use super::*;
        let (_dir, server, window) = fixture();
        let request = request(&window, QuitControl::Prepare);
        let client = std::thread::spawn(move || {
            control_window(&window, &request, Duration::from_secs(2)).unwrap()
        });
        let incoming = server.recv_control_timeout(Duration::from_secs(2)).unwrap();
        let delivery = incoming.respond_tracked(ControlStatus::Pending).unwrap();
        assert!(matches!(
            client.join().unwrap().status,
            ControlStatus::Pending
        ));
        let end = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(result) = delivery.try_result() {
                result.unwrap();
                break;
            }
            assert!(Instant::now() < end);
            std::thread::yield_now();
        }
    }

    use super::*;

    /// A real authenticated TCP peer whose second connection is the control.
    /// The stop channel and finite accept/IO deadlines close every owned socket
    /// and join the peer even when the client rejects its response early.
    fn response_peer(
        payload: Vec<u8>,
        interval: Duration,
    ) -> (
        crate::window_registry::WindowInfo,
        mpsc::Sender<()>,
        std::thread::JoinHandle<io::Result<()>>,
    ) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let window = crate::window_registry::WindowInfo {
            id: new_token().unwrap(),
            project: None,
            endpoint: InstanceInfo {
                native_project_host: false,
                pid: std::process::id(),
                port: listener.local_addr().unwrap().port(),
                token: new_token().unwrap(),
                version: "test-peer".into(),
            },
        };
        let info = window.clone();
        let (stop, stopped) = mpsc::channel();
        let peer = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            let accept = || -> io::Result<TcpStream> {
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => return Ok(stream),
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            control_remaining(deadline)?;
                            if stopped.recv_timeout(Duration::from_millis(5)).is_ok() {
                                return Err(io::Error::new(
                                    io::ErrorKind::Interrupted,
                                    "peer stopped",
                                ));
                            }
                        }
                        Err(error) => return Err(error),
                    }
                }
            };
            let proof_socket = accept()?;
            let challenge: serde_json::Value =
                serde_json::from_slice(&control_read_frame(&proof_socket, deadline)?)
                    .map_err(io::Error::other)?;
            let nonce = challenge["window_nonce"]
                .as_str()
                .ok_or_else(|| control_error("missing nonce challenge"))?;
            let proof = crate::window_registry::proof(&info, nonce.to_owned())?;
            let mut bytes = serde_json::to_vec(&proof).map_err(io::Error::other)?;
            bytes.push(b'\n');
            control_write(&proof_socket, &bytes, deadline)?;
            drop(proof_socket);
            let phase_socket = accept()?;
            let actual: ControlRequest =
                serde_json::from_slice(&control_read_frame(&phase_socket, deadline)?)
                    .map_err(io::Error::other)?;
            if actual.token != info.endpoint.token || actual.transaction_id != "quit-transaction" {
                return Err(control_error("unexpected phase request"));
            }
            if interval.is_zero() {
                // Early rejection can close the client while a large response
                // is in flight; that is an expected peer-side outcome.
                let _ = control_write(&phase_socket, &payload, deadline);
            } else {
                for byte in payload {
                    if control_write(&phase_socket, &[byte], deadline).is_err() {
                        break;
                    }
                    if stopped.recv_timeout(interval).is_ok() {
                        break;
                    }
                }
            }
            Ok(())
        });
        (window, stop, peer)
    }

    fn state() -> QuitStateStamp {
        QuitStateStamp {
            owner: "logical-owner".into(),
            generation: 4,
            owner_revision: 9,
        }
    }
    fn guard() -> QuitGuardStamp {
        QuitGuardStamp {
            nonce: "guard-nonce".into(),
            state: "documents-notebook-workspace-fingerprint".into(),
        }
    }
    fn request(window: &crate::window_registry::WindowInfo, phase: QuitControl) -> ControlRequest {
        ControlRequest {
            protocol: PROTOCOL,
            token: window.endpoint.token.clone(),
            timeout_ms: 1000,
            expected_window: crate::window_registry::ExpectedWindow {
                id: window.id.clone(),
                project: window.project.clone(),
            },
            transaction_id: "quit-transaction".into(),
            expected_state: state(),
            quit_control: phase,
        }
    }
    fn fixture() -> (
        tempfile::TempDir,
        Server,
        crate::window_registry::WindowInfo,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start_secondary(dir.path()).unwrap();
        let window = server.registration.lock().unwrap().info.clone();
        (dir, server, window)
    }
    fn certificate(request: &ControlRequest) -> QuitCommitCertificate {
        QuitCommitCertificate {
            transaction_id: request.transaction_id.clone(),
            checkpoint_generation: 5,
            checkpoint_revision: "durable-exact-content-revision".into(),
            cohort: vec![QuitPreparedWindow {
                window: request.expected_window.clone(),
                state: request.expected_state.clone(),
                guard: guard(),
            }],
        }
    }
    fn raw(
        window: &crate::window_registry::WindowInfo,
        value: &serde_json::Value,
    ) -> ControlResponse {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, window.endpoint.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        writeln!(stream, "{value}").unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[test]
    fn control_round_trip_is_separate_from_open_ack_and_retains_welcome_expectation() {
        let (_dir, server, window) = fixture();
        let req = request(&window, QuitControl::Prepare);
        let remote_window = window.clone();
        let remote_req = req.clone();
        let client = std::thread::spawn(move || {
            control_window(&remote_window, &remote_req, Duration::from_secs(2)).unwrap()
        });
        let incoming = server.recv_control_timeout(Duration::from_secs(2)).unwrap();
        assert!(
            server.try_recv().is_none(),
            "controls must not enter OPEN's queue"
        );
        assert_eq!(incoming.request.expected_window.project, None);
        incoming
            .validate_consumption(&req.expected_window, &state())
            .unwrap();
        incoming.respond(ControlStatus::Pending).unwrap();
        assert_eq!(
            client.join().unwrap(),
            ControlResponse {
                transaction_id: req.transaction_id,
                status: ControlStatus::Pending
            }
        );
        // The pre-existing OPEN path still reports opened files and --wait's
        // final close independently, after a control used the same endpoint.
        let client = std::thread::spawn(move || {
            let opened = forward_window(
                &window,
                &[OpenTarget::new(PathBuf::from("/owned/file.py"))],
                true,
                None,
            )
            .unwrap();
            assert_eq!(opened.opened, vec![PathBuf::from("/owned/file.py")]);
            opened.wait_closed().unwrap();
        });
        let open = server.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(server.try_recv_control().is_none());
        open.accept(vec![PathBuf::from("/owned/file.py")])
            .unwrap()
            .finish();
        client.join().unwrap();
    }

    #[test]
    fn listener_rejects_wrong_token_identity_project_and_omitted_welcome_precondition() {
        let (_dir, server, window) = fixture();
        let valid = serde_json::to_value(request(&window, QuitControl::Prepare)).unwrap();
        let mut invalid = Vec::new();
        let mut wrong = valid.clone();
        wrong["token"] = "wrong".into();
        invalid.push(wrong);
        let mut wrong = valid.clone();
        wrong["expected_window"]["id"] = "other-window".into();
        invalid.push(wrong);
        let mut wrong = valid.clone();
        wrong["expected_window"]["project"] = "/other-project".into();
        invalid.push(wrong);
        let mut wrong = valid.clone();
        wrong["expected_window"]
            .as_object_mut()
            .unwrap()
            .remove("project");
        invalid.push(wrong);
        let mut wrong = valid;
        wrong["targets"] = serde_json::json!([]);
        invalid.push(wrong);
        for value in invalid {
            assert!(matches!(
                raw(&window, &value).status,
                ControlStatus::Rejected { .. }
            ));
            assert!(server.try_recv().is_none() && server.try_recv_control().is_none());
        }
    }

    #[test]
    fn consumption_rechecks_identity_project_welcome_and_logical_revision_after_enqueue() {
        let (_dir, server, window) = fixture();
        let req = request(&window, QuitControl::Prepare);
        let client_window = window.clone();
        let client_req = req.clone();
        let client = std::thread::spawn(move || {
            control_window(&client_window, &client_req, Duration::from_secs(2)).unwrap()
        });
        let incoming = server.recv_control_timeout(Duration::from_secs(2)).unwrap();
        let mut changed = req.expected_window.clone();
        changed.project = Some(PathBuf::from("/now-open"));
        assert!(incoming.validate_consumption(&changed, &state()).is_err());
        changed = req.expected_window.clone();
        changed.id = "changed".into();
        assert!(incoming.validate_consumption(&changed, &state()).is_err());
        let mut changed_state = state();
        changed_state.owner_revision += 1;
        assert!(incoming
            .validate_consumption(&req.expected_window, &changed_state)
            .is_err());
        changed_state = state();
        changed_state.generation += 1;
        assert!(incoming
            .validate_consumption(&req.expected_window, &changed_state)
            .is_err());
        incoming
            .respond(ControlStatus::Rejected {
                message: "actual UI state changed".into(),
            })
            .unwrap();
        assert!(matches!(
            client.join().unwrap().status,
            ControlStatus::Rejected { .. }
        ));
    }

    #[test]
    fn expired_control_cannot_later_be_consumed_or_answered_prepared() {
        let (_dir, server, window) = fixture();
        let mut req = request(&window, QuitControl::Prepare);
        req.timeout_ms = 20;
        let client_window = window.clone();
        let client_req = req.clone();
        let client = std::thread::spawn(move || {
            raw(&client_window, &serde_json::to_value(client_req).unwrap())
        });
        let incoming = server.recv_control_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(
            client.join().unwrap().status,
            ControlStatus::Rejected { .. }
        ));
        assert_eq!(
            incoming
                .validate_consumption(&req.expected_window, &state())
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(
            incoming
                .respond(ControlStatus::Prepared { guard: guard() })
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[test]
    fn phase_ledger_requires_explicit_approval_and_exact_certificate_and_is_idempotent() {
        let (_dir, _server, window) = fixture();
        let prepare = request(&window, QuitControl::Prepare);
        let mut session = QuitControlSession::begin(&prepare).unwrap();
        assert_eq!(
            session.status(),
            ControlStatus::Pending,
            "enqueue/begin is not guard approval"
        );
        let commit = request(
            &window,
            QuitControl::Commit {
                guard: guard(),
                certificate: certificate(&prepare),
            },
        );
        assert!(session.commit_after_checkpoint(&commit).is_err());
        session.approve(guard()).unwrap();
        session.approve(guard()).unwrap();
        assert!(session.matches(&request(&window, QuitControl::PollPrepared)));
        let mut wrong = commit.clone();
        wrong.transaction_id = "other-transaction".into();
        assert!(session.commit_after_checkpoint(&wrong).is_err());
        let mut wrong = commit.clone();
        wrong.expected_state.owner_revision += 1;
        assert!(session.commit_after_checkpoint(&wrong).is_err());
        let mut wrong = commit.clone();
        if let QuitControl::Commit { certificate, .. } = &mut wrong.quit_control {
            certificate.cohort[0].guard.state = "changed-notebook".into();
        }
        assert!(session.commit_after_checkpoint(&wrong).is_err());
        // This unit test supplies an assumed durable proof; the real caller must
        // verify publication and the complete frozen cohort before this hook.
        session.commit_after_checkpoint(&commit).unwrap();
        session.commit_after_checkpoint(&commit).unwrap();
        assert!(session
            .abort(&request(&window, QuitControl::Abort))
            .is_err());
        let mut aborted = QuitControlSession::begin(&prepare).unwrap();
        aborted
            .abort(&request(&window, QuitControl::Abort))
            .unwrap();
        aborted
            .abort(&request(&window, QuitControl::Abort))
            .unwrap();
        assert!(aborted.approve(guard()).is_err());
        assert!(aborted.commit_after_checkpoint(&commit).is_err());
    }

    #[test]
    fn phase_response_mismatch_and_foreign_or_duplicate_cohorts_are_rejected() {
        let (_dir, server, window) = fixture();
        let req = request(&window, QuitControl::Prepare);
        let other = window.clone();
        let client_req = req.clone();
        let client = std::thread::spawn(move || {
            control_window(&other, &client_req, Duration::from_secs(2)).unwrap()
        });
        let incoming = server.recv_control_timeout(Duration::from_secs(2)).unwrap();
        assert!(incoming
            .respond(ControlStatus::Committed {
                certificate: certificate(&req)
            })
            .is_err());
        assert!(matches!(
            client.join().unwrap().status,
            ControlStatus::Rejected { .. }
        ));
        let mut cert = certificate(&req);
        cert.cohort.push(cert.cohort[0].clone());
        assert!(validate_certificate(&cert).is_err());
        let mut cert = certificate(&req);
        cert.transaction_id = "foreign".into();
        assert!(request(
            &window,
            QuitControl::Commit {
                guard: guard(),
                certificate: cert
            }
        )
        .validate()
        .is_err());
        assert!(!valid_control_response(
            &req,
            &ControlResponse {
                transaction_id: "foreign".into(),
                status: ControlStatus::Pending
            }
        ));
    }

    #[test]
    fn authenticated_peer_trickle_cannot_extend_the_overarching_control_deadline() {
        let response = ControlResponse {
            transaction_id: "quit-transaction".into(),
            status: ControlStatus::Pending,
        };
        let mut payload = serde_json::to_vec(&response).unwrap();
        payload.push(b'\n');
        let (window, stop, peer) = response_peer(payload, Duration::from_millis(20));
        let started = Instant::now();
        let result = control_window(
            &window,
            &request(&window, QuitControl::Prepare),
            Duration::from_millis(120),
        );
        let elapsed = started.elapsed();
        let _ = stop.send(());
        peer.join().unwrap().unwrap();
        assert!(
            matches!(result, Err(ForwardError::Unreachable(_))),
            "partial response is never approval: {result:?}"
        );
        // The old read_line renewed its 120ms syscall timeout on every byte and
        // took over a second to finish this valid response. Allow scheduling
        // headroom while rejecting that specific trickle behavior.
        assert!(
            elapsed < Duration::from_millis(700),
            "overall deadline extended by trickle: {elapsed:?}"
        );
    }

    #[test]
    fn authenticated_peer_eof_malformed_oversize_and_foreign_response_never_approve() {
        let foreign = serde_json::to_vec(&ControlResponse {
            transaction_id: "other-transaction".into(),
            status: ControlStatus::Pending,
        })
        .unwrap();
        let mut foreign = foreign;
        foreign.push(b'\n');
        for payload in [
            Vec::new(),
            b"{\"status\":\"pending\"}".to_vec(),
            b"\xff\n".to_vec(),
            vec![b'x'; MAX_REQUEST as usize + 1],
            foreign,
        ] {
            let (window, stop, peer) = response_peer(payload, Duration::ZERO);
            let result = control_window(
                &window,
                &request(&window, QuitControl::Prepare),
                Duration::from_secs(2),
            );
            let _ = stop.send(());
            peer.join().unwrap().unwrap();
            assert!(
                matches!(result, Err(ForwardError::Unreachable(_))),
                "invalid peer frame approved: {result:?}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_revokes_discovery_before_stopping_its_endpoint() {
        let directory = tempfile::tempdir().unwrap();
        let mut server = Server::start_secondary(directory.path()).unwrap();
        let info = crate::window_registry::live_windows(directory.path())
            .unwrap()
            .pop()
            .unwrap();
        let (paused_tx, paused_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        server.before_transport_shutdown = Some((paused_tx, release_rx));
        struct PausedDrop {
            release: Option<mpsc::Sender<()>>,
            thread: Option<std::thread::JoinHandle<()>>,
        }
        impl Drop for PausedDrop {
            fn drop(&mut self) {
                // Assertion/error unwinding also releases and joins the owner.
                self.release.take();
                if let Some(thread) = self.thread.take() {
                    let _ = thread.join();
                }
            }
        }
        let mut dropping = PausedDrop {
            release: Some(release_tx),
            thread: Some(std::thread::spawn(move || drop(server))),
        };
        // The channel fixes the schedule at the real Drop boundary: discovery
        // has been revoked, but the accept loop must still answer an already
        // observed endpoint. No sleeps or repeated connection attempts.
        let already_shutdown = paused_rx.recv().unwrap();
        let discovery = crate::window_registry::live_windows(directory.path());
        let proof = crate::window_registry::verify_endpoint(&info);
        let record_absent = !directory
            .path()
            .join("windows")
            .join(&info.id)
            .join("window.json")
            .exists();
        dropping.release.take();
        dropping.thread.take().unwrap().join().unwrap();
        assert!(
            !already_shutdown,
            "discovery must precede transport shutdown"
        );
        assert!(record_absent, "the published hint must already be revoked");
        assert!(discovery.unwrap().is_empty());
        assert_eq!(
            proof.unwrap().id,
            info.id,
            "the endpoint still answers proof"
        );
        assert!(crate::window_registry::live_windows(directory.path())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn incoming_busy_is_an_explicit_rejection_without_primary_retry_or_wait_ack() {
        let dir = tempfile::tempdir().unwrap();
        let Probe::Free(lock) = probe(dir.path()).unwrap() else {
            panic!("fresh directory")
        };
        let server = Server::start(lock).unwrap();
        for wait in [false, true] {
            let config = dir.path().to_path_buf();
            let client = std::thread::spawn(move || {
                forward(
                    &config,
                    &[target("/p/a.py", None)],
                    wait,
                    None,
                    Duration::from_secs(5),
                )
            });
            let incoming = server.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(incoming.wait, wait);
            incoming.reject("Busy: quit is preparing");
            assert!(
                matches!(client.join().unwrap(), Err(ForwardError::Rejected(message)) if message == "Busy: quit is preparing")
            );
            assert!(
                server.recv_timeout(Duration::from_millis(50)).is_none(),
                "rejection must not enqueue a retry"
            );
        }
        let window = crate::window_registry::live_windows(dir.path())
            .unwrap()
            .pop()
            .unwrap();
        let client = std::thread::spawn(move || {
            forward_window(&window, &[target("/p/a.py", None)], true, None)
        });
        server
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .reject("Busy: quit is preparing");
        assert!(
            matches!(client.join().unwrap(), Err(ForwardError::Rejected(message)) if message == "Busy: quit is preparing")
        );
    }

    #[test]
    fn empty_acceptance_remains_success_and_wait_is_retained_until_close() {
        let dir = tempfile::tempdir().unwrap();
        let Probe::Free(lock) = probe(dir.path()).unwrap() else {
            panic!("fresh directory")
        };
        let server = Server::start(lock).unwrap();
        for wait in [false, true] {
            let config = dir.path().to_path_buf();
            let client = std::thread::spawn(move || {
                forward(
                    &config,
                    &[target("/p/a.py", None)],
                    wait,
                    None,
                    Duration::from_secs(2),
                )
                .unwrap()
            });
            let handle = server
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .accept(Vec::new());
            assert_eq!(handle.is_some(), wait);
            let forwarded = client.join().unwrap();
            assert!(forwarded.opened.is_empty());
            if let Some(handle) = handle {
                let (done, finished) = mpsc::channel();
                let waiter =
                    std::thread::spawn(move || done.send(forwarded.wait_closed()).unwrap());
                assert!(matches!(
                    finished.recv_timeout(Duration::from_millis(50)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ));
                handle.finish();
                finished
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap()
                    .unwrap();
                waiter.join().unwrap();
            }
        }
    }

    #[test]
    fn queued_open_expiry_writes_error_and_late_acceptance_has_no_wait_handle() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let (reply, receiver) = mpsc::channel();
        let (closed, _closed_receiver) = mpsc::channel();
        let incoming = Incoming {
            native_window: None,
            targets: vec![target("/p/a.py", None)],
            wait: true,
            cwd: None,
            expected_project: None,
            reply,
            closed: Some(closed),
        };
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            assert!(
                receive_open_reply(&mut stream, &receiver, Duration::from_millis(25))
                    .unwrap()
                    .is_none()
            );
        });
        let mut client = BufReader::new(TcpStream::connect(address).unwrap());
        client
            .get_mut()
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut response = String::new();
        assert!(
            client.read_line(&mut response).unwrap() > 0,
            "expiry must send an explicit response, not only EOF"
        );
        assert!(
            matches!(serde_json::from_str::<Response>(&response).unwrap(), Response::Error { message } if message == "the IDE did not handle the request")
        );
        server.join().unwrap();
        assert!(incoming.accept(vec![PathBuf::from("/p/a.py")]).is_none());
    }

    fn target(p: &str, line: Option<u32>) -> OpenTarget {
        OpenTarget {
            path: PathBuf::from(p),
            line,
            column: None,
        }
    }

    #[test]
    fn the_lock_is_exclusive_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let Probe::Free(lock) = probe(dir.path()).unwrap() else {
            panic!("fresh directory")
        };
        assert!(
            matches!(probe(dir.path()).unwrap(), Probe::Running),
            "a second endpoint sees the IDE"
        );
        drop(lock);
        assert!(
            matches!(probe(dir.path()).unwrap(), Probe::Free(_)),
            "free again, as after a crash"
        );
    }

    #[test]
    fn request_round_trip_with_wait() {
        let dir = tempfile::tempdir().unwrap();
        let Probe::Free(lock) = probe(dir.path()).unwrap() else {
            panic!()
        };
        let server = Server::start(lock).unwrap();
        let info = read_info(dir.path()).unwrap();
        assert_eq!(
            (info.port, info.pid, info.token.len()),
            (server.port(), std::process::id(), 64)
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(INFO_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "the token is private: {mode:o}");
        }
        let woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let w = woken.clone();
        server.set_waker(move || {
            w.fetch_add(1, Ordering::SeqCst);
        });
        // The "IDE": opens the files, keeps them open a moment, then closes them.
        let ide = std::thread::spawn(move || {
            let incoming = server
                .recv_timeout(Duration::from_secs(10))
                .expect("a request");
            assert_eq!(
                incoming.targets,
                vec![target("/p/a.py", Some(3)), target("/p", None)]
            );
            assert!(incoming.wait);
            assert_eq!(incoming.cwd.as_deref(), Some(Path::new("/work")));
            let handle = incoming
                .accept(vec![PathBuf::from("/p/a.py"), PathBuf::from("/p")])
                .expect("a wait handle");
            std::thread::sleep(Duration::from_millis(150));
            handle.finish();
            server
        });
        let started = Instant::now();
        let fw = forward(
            dir.path(),
            &[target("/p/a.py", Some(3)), target("/p", None)],
            true,
            Some(Path::new("/work")),
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(
            fw.opened,
            vec![PathBuf::from("/p/a.py"), PathBuf::from("/p")]
        );
        fw.wait_closed().unwrap();
        assert!(
            started.elapsed() >= Duration::from_millis(150),
            "returned only after the files were closed"
        );
        let server = ide.join().unwrap();
        assert_eq!(woken.load(Ordering::SeqCst), 1);
        // Without --wait the answer comes at once.
        let ide = std::thread::spawn(move || {
            let incoming = server
                .recv_timeout(Duration::from_secs(10))
                .expect("a request");
            assert!(!incoming.wait);
            assert!(incoming.accept(vec![]).is_none());
            server
        });
        let fw = forward(
            dir.path(),
            &[target("/p/b.py", None)],
            false,
            None,
            Duration::from_secs(10),
        )
        .unwrap();
        assert!(fw.opened.is_empty());
        let server = ide.join().unwrap();
        drop(server);
        assert!(
            !dir.path().join(INFO_FILE).exists(),
            "a clean exit removes instance.json"
        );
    }

    #[test]
    fn a_wait_ends_when_the_ide_exits() {
        let dir = tempfile::tempdir().unwrap();
        let Probe::Free(lock) = probe(dir.path()).unwrap() else {
            panic!()
        };
        let server = Server::start(lock).unwrap();
        let ide = std::thread::spawn(move || {
            let incoming = server.recv_timeout(Duration::from_secs(10)).unwrap();
            let handle = incoming.accept(vec![PathBuf::from("/p/a.py")]);
            drop(handle); // the IDE quits without closing the tab
            drop(server);
        });
        let fw = forward(
            dir.path(),
            &[target("/p/a.py", None)],
            true,
            None,
            Duration::from_secs(10),
        )
        .unwrap();
        fw.wait_closed().unwrap();
        ide.join().unwrap();
    }

    #[test]
    fn wrong_tokens_and_garbage_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let Probe::Free(lock) = probe(dir.path()).unwrap() else {
            panic!()
        };
        let server = Server::start(lock).unwrap();
        let info = read_info(dir.path()).unwrap();
        let bad = Request {
            native_window: None,
            expected_window: None,
            protocol: PROTOCOL,
            token: "0".repeat(64),
            targets: vec![target("/etc/passwd", None)],
            wait: false,
            cwd: None,
        };
        match send(&info, &bad) {
            Err(SendError::Rejected(m)) => assert_eq!(m, "invalid token"),
            _ => panic!("a wrong token must be refused"),
        }
        let mut s = TcpStream::connect(("127.0.0.1", info.port)).unwrap();
        s.write_all(b"GET / HTTP/1.0\r\n\r\n").unwrap();
        let mut answer = String::new();
        BufReader::new(&s).read_line(&mut answer).unwrap();
        assert!(answer.contains("bad request"), "{answer}");
        let old = Request {
            native_window: None,
            protocol: PROTOCOL + 1,
            ..bad
        };
        let old = Request {
            token: info.token.clone(),
            ..old
        };
        match send(&info, &old) {
            Err(SendError::Rejected(m)) => assert!(m.starts_with("protocol"), "{m}"),
            _ => panic!("another protocol version must be refused"),
        }
        assert!(server.try_recv().is_none(), "nothing reached the IDE");
    }

    #[test]
    fn a_stale_instance_file_after_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        // A crashed IDE: the lock is free but instance.json points at a closed port.
        let closed_port = {
            let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            l.local_addr().unwrap().port()
        };
        write_info(
            dir.path(),
            &InstanceInfo {
                native_project_host: false,
                pid: 999_999,
                port: closed_port,
                token: "f".repeat(64),
                version: String::new(),
            },
        )
        .unwrap();
        let started = Instant::now();
        let err = forward(
            dir.path(),
            &[target("/p/a.py", None)],
            false,
            None,
            Duration::from_millis(300),
        )
        .unwrap_err();
        assert!(matches!(err, ForwardError::Unreachable(_)), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
        // The next IDE takes the free lock and replaces the file.
        let Probe::Free(lock) = probe(dir.path()).unwrap() else {
            panic!("a crash leaves no lock behind")
        };
        let server = Server::start(lock).unwrap();
        assert_ne!(read_info(dir.path()).unwrap().token, "f".repeat(64));
        let ide = std::thread::spawn(move || {
            server
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .accept(vec![PathBuf::from("/p/a.py")]);
            server
        });
        let fw = forward(
            dir.path(),
            &[target("/p/a.py", None)],
            false,
            None,
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(fw.opened, vec![PathBuf::from("/p/a.py")]);
        drop(ide.join().unwrap());
    }

    #[test]
    fn a_starting_ide_is_waited_for() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let ide = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let Probe::Free(lock) = probe(&path).unwrap() else {
                panic!()
            };
            let server = Server::start(lock).unwrap();
            server
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .accept(vec![]);
            server
        });
        let fw = forward(
            dir.path(),
            &[target("/p/a.py", None)],
            false,
            None,
            Duration::from_secs(10),
        );
        assert!(fw.is_ok(), "{fw:?}");
        drop(ide.join().unwrap());
    }

    #[test]
    fn targets_become_absolute() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.py"), "").unwrap();
        let t = absolute_targets(
            &[
                target("a.py", Some(2)),
                target("missing.py", None),
                target("/abs/x.py", None),
            ],
            dir.path(),
        );
        assert_eq!(
            t[0],
            OpenTarget {
                path: dunce::canonicalize(dir.path().join("a.py")).unwrap(),
                line: Some(2),
                column: None
            }
        );
        assert_eq!(t[1].path, dir.path().join("missing.py"));
        assert_eq!(t[2].path, PathBuf::from("/abs/x.py"));
    }

    #[test]
    fn secrets_compare_exactly() {
        assert!(same_secret("abc", "abc"));
        assert!(!same_secret("abc", "abd"));
        assert!(!same_secret("abc", "ab"));
        let t = new_token().unwrap();
        assert_eq!(t.len(), 64);
        assert_ne!(t, new_token().unwrap());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod lock_fork_tests;

#[cfg(all(test, unix))]
mod file_lease_tests;

#[cfg(test)]
mod owner_probe_tests {
    use super::*;

    fn stamp(owner: &str, revision: u64) -> QuitStateStamp {
        QuitStateStamp {
            owner: owner.into(),
            generation: 3,
            owner_revision: revision,
        }
    }
    fn info(server: &Server) -> crate::window_registry::WindowInfo {
        server.registration.lock().unwrap().info.clone()
    }
    #[test]
    fn live_welcome_windows_have_distinct_transient_owners_and_update() {
        let directory = tempfile::tempdir().unwrap();
        let first = Server::start_secondary(directory.path()).unwrap();
        let second = Server::start_secondary(directory.path()).unwrap();
        let timeout = Duration::from_secs(2);
        assert!(query_window_owner(&info(&first), timeout).is_err());
        first.set_quit_state(Some(stamp("first", 1))).unwrap();
        second.set_quit_state(Some(stamp("second", 7))).unwrap();
        assert_eq!(
            query_window_owner(&info(&first), timeout).unwrap(),
            stamp("first", 1)
        );
        assert_eq!(
            query_window_owner(&info(&second), timeout).unwrap(),
            stamp("second", 7)
        );
        first.set_quit_state(Some(stamp("replacement", 8))).unwrap();
        assert_eq!(
            query_window_owner(&info(&first), timeout).unwrap(),
            stamp("replacement", 8)
        );
        first.set_quit_state(None).unwrap();
        assert!(query_window_owner(&info(&first), timeout).is_err());
        assert!(first.try_recv().is_none());
        assert!(first.try_recv_control().is_none());
    }
    #[test]
    fn changed_project_invalidates_owner_and_old_destination() {
        let directory = tempfile::tempdir().unwrap();
        let server = Server::start_secondary(directory.path()).unwrap();
        server.set_quit_state(Some(stamp("welcome", 1))).unwrap();
        let welcome = info(&server);
        server.set_project(Some(directory.path())).unwrap();
        assert!(query_window_owner(&welcome, Duration::from_secs(2)).is_err());
        let project = info(&server);
        assert!(query_window_owner(&project, Duration::from_secs(2)).is_err());
        server.set_quit_state(Some(stamp("project", 2))).unwrap();
        assert_eq!(
            query_window_owner(&project, Duration::from_secs(2)).unwrap(),
            stamp("project", 2)
        );
        server.set_project(Some(directory.path())).unwrap();
        assert_eq!(
            query_window_owner(&project, Duration::from_secs(2)).unwrap(),
            stamp("project", 2)
        );
    }
    #[test]
    fn failed_project_publication_invalidates_old_owner_in_live_endpoint() {
        let directory = tempfile::tempdir().unwrap();
        let server = Server::start_secondary(directory.path()).unwrap();
        server.set_quit_state(Some(stamp("welcome", 1))).unwrap();
        let old_window = info(&server);
        let record = directory
            .path()
            .join("windows")
            .join(server.window_id())
            .join("window.json");
        std::fs::remove_file(&record).unwrap();
        std::fs::create_dir(&record).unwrap();
        assert!(server.set_project(Some(directory.path())).is_err());
        let changed_window = info(&server);
        assert_ne!(changed_window.project, old_window.project);
        assert!(query_window_owner(&old_window, Duration::from_secs(2)).is_err());
        assert!(query_window_owner(&changed_window, Duration::from_secs(2)).is_err());
        std::fs::remove_dir(&record).unwrap();
        server.set_project(Some(directory.path())).unwrap();
        server
            .set_quit_state(Some(stamp("accepted-project", 2)))
            .unwrap();
        assert_eq!(
            query_window_owner(&info(&server), Duration::from_secs(2)).unwrap(),
            stamp("accepted-project", 2)
        );
    }
    #[test]
    fn tcp_owner_proof_rejects_replay_forgery_and_changed_fields() {
        for mutation in 0..6 {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let token = new_token().unwrap();
            let window = crate::window_registry::WindowInfo {
                id: new_token().unwrap(),
                project: None,
                endpoint: InstanceInfo {
                    native_project_host: false,
                    pid: 0,
                    port: listener.local_addr().unwrap().port(),
                    token: token.clone(),
                    version: String::new(),
                },
            };
            let expected = crate::window_registry::ExpectedWindow {
                id: window.id.clone(),
                project: None,
            };
            let peer = std::thread::spawn(move || {
                let (socket, _) = listener.accept().unwrap();
                let deadline = Instant::now() + Duration::from_secs(2);
                let bytes = control_read_frame(&socket, deadline).unwrap();
                assert!(!String::from_utf8_lossy(&bytes).contains(&token));
                let challenge: OwnerChallenge = serde_json::from_slice(&bytes).unwrap();
                let mut proof = OwnerProof {
                    nonce: challenge.quit_owner_nonce,
                    window: expected,
                    state: stamp("owner", 1),
                    mac: String::new(),
                };
                proof.mac = owner_mac(&token, &proof).unwrap();
                match mutation {
                    0 => proof.nonce = new_token().unwrap(), // replayed challenge
                    1 => proof.window.id = new_token().unwrap(),
                    2 => proof.window.project = Some(PathBuf::from("/changed")),
                    3 => proof.state.owner_revision += 1,
                    4 => proof.mac = "00".repeat(32),
                    5 => {
                        // authentic MAC for the wrong destination still fails
                        proof.window.id = new_token().unwrap();
                        proof.mac = owner_mac(&token, &proof).unwrap();
                    }
                    _ => unreachable!(),
                }
                let mut bytes = serde_json::to_vec(&proof).unwrap();
                bytes.push(b'\n');
                control_write(&socket, &bytes, deadline).unwrap();
            });
            assert!(query_window_owner(&window, Duration::from_secs(2)).is_err());
            peer.join().unwrap();
        }
    }
}

#[cfg(test)]
mod native_project_host_fixtures {
    use super::*;
    #[test]
    fn closing_window_releases_discovery_while_client_registration_is_retained() {
        let directory = tempfile::tempdir().unwrap();
        let window = Server::start_secondary(directory.path()).unwrap();
        let retained = window.registration.clone();
        let id = window.window_id();
        assert_eq!(
            crate::window_registry::live_windows(directory.path())
                .unwrap()
                .len(),
            1
        );
        drop(window);
        // Simulate a client thread which has not yet released its registration.
        // Its continued allocation must not retain logical-window authority.
        assert_eq!(retained.lock().unwrap().info.id, id);
        assert!(!directory.path().join("windows").join(id).exists());
        let presence = crate::window_registry::presence_until(
            directory.path(),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
        assert!(presence.live.is_empty() && presence.uncertain.is_empty());
    }
    #[test]
    fn router_transport_is_never_a_logical_window_or_uncertain_quit_member() {
        let directory = tempfile::tempdir().unwrap();
        let router = match probe(directory.path()).unwrap() {
            Probe::Free(lock) => Server::start(lock).unwrap(),
            _ => panic!("Fresh router lease"),
        };
        router.become_native_project_router().unwrap();
        assert!(read_info(directory.path()).unwrap().native_project_host);
        let presence = crate::window_registry::presence_until(
            directory.path(),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
        assert!(presence.live.is_empty() && presence.uncertain.is_empty());
        let first = Server::start_secondary(directory.path()).unwrap();
        let second = Server::start_secondary(directory.path()).unwrap();
        let live = crate::window_registry::live_windows(directory.path()).unwrap();
        assert_eq!(live.len(), 2);
        assert_ne!(first.window_id(), second.window_id());
        assert_eq!(live[0].endpoint.pid, live[1].endpoint.pid);
        assert!(!live.iter().any(|window| window.id == router.window_id()));
        drop(first);
        assert_eq!(
            crate::window_registry::live_windows(directory.path())
                .unwrap()
                .len(),
            1
        );
        drop(second);
        let presence = crate::window_registry::presence_until(
            directory.path(),
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
        assert!(presence.live.is_empty() && presence.uncertain.is_empty());
    }
    #[test]
    fn native_new_keeps_startup_options_and_waits_for_its_actual_window_paths() {
        let directory = tempfile::tempdir().unwrap();
        let router = match probe(directory.path()).unwrap() {
            Probe::Free(lock) => Server::start(lock).unwrap(),
            _ => panic!("Fresh router lease"),
        };
        router.become_native_project_router().unwrap();
        let config = dunce::canonicalize(directory.path()).unwrap();
        let args = crate::args::OpenArgs {
            new_window: true,
            wait: true,
            run: true,
            terminal: Some("literal command".into()),
            actions: vec!["MergeAllProjectWindows".into()],
            targets: vec![crate::args::OpenTarget::new(&config)],
            ..Default::default()
        };
        let expected = args.clone();
        let client = std::thread::spawn(move || {
            let reply = forward_native_window(&config, &args, &config).unwrap();
            reply.wait_closed().unwrap();
        });
        let request = router.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(request.native_window.as_ref(), Some(&expected));
        assert!(request.wait);
        let handle = request
            .accept(
                expected
                    .targets
                    .iter()
                    .map(|target| target.path.clone())
                    .collect(),
            )
            .unwrap();
        assert!(
            !client.is_finished(),
            "NEW acknowledgement cannot finish --wait"
        );
        handle.finish();
        client.join().unwrap();
    }
    #[test]
    fn ordinary_legacy_endpoint_is_not_advertised_as_a_native_project_host() {
        let old: InstanceInfo =
            serde_json::from_str(r#"{"pid":1,"port":42,"token":"x","version":"old"}"#).unwrap();
        assert!(!old.native_project_host);
        assert!(serde_json::to_value(&old)
            .unwrap()
            .get("native_project_host")
            .is_none());
    }
}
