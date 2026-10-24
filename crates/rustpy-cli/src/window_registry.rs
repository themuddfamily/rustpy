//! Authenticated discovery of every IDE window, including secondary processes.
//! Metadata is only a hint: an OS lease and a fresh endpoint proof are required.
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::ipc::{self, InstanceInfo};
use serde::{Deserialize, Serialize};

const DIRECTORY: &str = "windows";
const RECORD: &str = "window.json";
const MAX_RECORD: u64 = 65536;
const MAX_WINDOWS: usize = 256;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WindowInfo {
    pub id: String,
    pub endpoint: InstanceInfo,
    pub project: Option<PathBuf>,
}
/// Project/window precondition for a routed request. Legacy primary requests
/// omit it; consumers must also check it immediately before applying a request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpectedWindow {
    pub id: String,
    pub project: Option<PathBuf>,
}

#[derive(Debug)]
pub(crate) struct Registration {
    directory: PathBuf,
    lease: Option<ipc::FileLease>,
    pub info: WindowInfo,
}
impl Registration {
    pub fn create(config: &Path, endpoint: InstanceInfo) -> io::Result<Self> {
        let id = ipc::new_token()?;
        let directory = config.join(DIRECTORY).join(&id);
        std::fs::create_dir_all(config.join(DIRECTORY))?;
        std::fs::create_dir(&directory)?;
        let lease = ipc::try_lock_file(&directory.join("lease"))?
            .ok_or_else(|| io::Error::other("new window lease occupied"))?;
        let registration = Self {
            directory,
            lease: Some(lease),
            info: WindowInfo {
                id,
                endpoint,
                project: None,
            },
        };
        registration.publish()?;
        Ok(registration)
    }
    pub fn set_project(&mut self, project: Option<&Path>) -> io::Result<()> {
        self.info.project = project.map(dunce::canonicalize).transpose()?;
        // Called after an accepted UI switch: endpoint truth must not revert
        // merely because its discovery hint could not be persisted.
        self.publish()
    }
    pub fn unregister(&self) {
        let _ = std::fs::remove_file(self.directory.join(RECORD));
    }
    /// Remove this endpoint from the logical-window cohort while retaining its
    /// transport. The process host has no documents or durable window token.
    pub(crate) fn detach_from_discovery(&mut self) -> io::Result<()> {
        self.unregister();
        self.lease.take();
        match std::fs::remove_file(self.directory.join("lease")) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        match std::fs::remove_dir(&self.directory) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
    fn publish(&self) -> io::Result<()> {
        let temporary = self.directory.join(format!("{}.tmp", ipc::new_token()?));
        let result = (|| {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(&serde_json::to_vec(&self.info).map_err(io::Error::other)?)?;
            file.sync_all()?;
            replace_file(&temporary, &self.directory.join(RECORD))
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.directory.join(RECORD));
        self.lease.take();
        let _ = std::fs::remove_file(self.directory.join("lease"));
        let _ = std::fs::remove_dir(&self.directory);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Proof {
    pub id: String,
    pub project: Option<PathBuf>,
    pub nonce: String,
    pub mac: String,
}
/// A domain-separated BLAKE3 keyed MAC. Challenges never send the token.
fn mac(token: &str, id: &str, project: &Option<PathBuf>, nonce: &str) -> io::Result<String> {
    let key = blake3::derive_key("rustpy-window-token-v1", token.as_bytes());
    let message = serde_json::to_vec(&("rustpy-window-proof-v1", id, project, nonce))
        .map_err(io::Error::other)?;
    Ok(blake3::keyed_hash(&key, &message).to_hex().to_string())
}
pub(crate) fn proof(info: &WindowInfo, nonce: String) -> io::Result<Proof> {
    Ok(Proof {
        id: info.id.clone(),
        project: info.project.clone(),
        mac: mac(&info.endpoint.token, &info.id, &info.project, &nonce)?,
        nonce,
    })
}
pub(crate) fn verify_endpoint(info: &WindowInfo) -> io::Result<WindowInfo> {
    let nonce = ipc::new_token()?;
    let mut stream = TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, info.endpoint.port)),
        Duration::from_millis(500),
    )?;
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    writeln!(stream, "{}", serde_json::json!({"window_nonce": nonce}))?;
    let mut line = String::new();
    BufReader::new(stream.take(MAX_RECORD)).read_line(&mut line)?;
    let answer: Proof = serde_json::from_str(&line).map_err(io::Error::other)?;
    let expected = mac(&info.endpoint.token, &answer.id, &answer.project, &nonce)?;
    if answer.id != info.id || answer.nonce != nonce || !ipc::same_secret(&answer.mac, &expected) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "window endpoint proof failed",
        ));
    }
    Ok(WindowInfo {
        project: answer.project,
        ..info.clone()
    })
}
fn read_record(path: &Path) -> io::Result<WindowInfo> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "window metadata is not regular",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECORD + 1).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_RECORD as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "window metadata too large",
        ));
    }
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}
#[cfg(not(windows))]
pub(crate) fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::rename(source, destination)
}
#[cfg(windows)]
pub(crate) fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(source: *const u16, destination: *const u16, flags: u32) -> i32;
    }
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: both paths are nul-terminated and remain alive for the call.
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), 1 | 8) } == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Returns only OS-live windows whose endpoint proves its identity and current
/// project. PID and on-disk project values never establish liveness or routing.
/// A registration removed concurrently vanishes between listing and reading;
/// on Windows it is briefly inaccessible instead.
fn transient(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
    )
}
pub fn live_windows(config: &Path) -> io::Result<Vec<WindowInfo>> {
    let entries = match std::fs::read_dir(config.join(DIRECTORY)) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut live = Vec::new();
    let mut published = 0;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if transient(&error) => continue,
            Err(error) => return Err(error),
        };
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => {}
            Ok(_) => continue,
            Err(error) if transient(&error) => continue,
            Err(error) => return Err(error),
        }
        let id = entry.file_name().to_string_lossy().to_string();
        if id.len() != 64 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        let directory = entry.path();
        // The record is atomically published only AFTER its owner holds the
        // lease. Never create/probe a lease in an unpublished starting directory.
        match directory.join(RECORD).try_exists() {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) if transient(&error) => continue,
            Err(error) => return Err(error),
        }
        published += 1;
        if published > MAX_WINDOWS {
            return Err(io::Error::other(
                "window registry exceeds discovery limit; retry after stale entries are pruned",
            ));
        }
        let lease = match ipc::try_existing_lock_file(&directory.join("lease")) {
            Ok(lease) => lease,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if directory.join(RECORD).try_exists()? {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "published window has no lease",
                    ));
                }
                continue;
            }
            Err(error) if transient(&error) => continue,
            Err(error) => return Err(error),
        };
        match lease {
            Some(dead_lease) => {
                let _ = std::fs::remove_file(directory.join(RECORD));
                drop(dead_lease);
                let _ = std::fs::remove_file(directory.join("lease"));
                let _ = std::fs::remove_dir(&directory);
            }
            None => {
                let info = match read_record(&directory.join(RECORD)) {
                    Ok(info) => info,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error),
                };
                if info.id != id {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "window metadata identity mismatch",
                    ));
                }
                match verify_endpoint(&info) {
                    Ok(verified) => live.push(verified),
                    Err(error) => {
                        // A window can disappear after its lease was inspected.
                        if !directory.join(RECORD).exists() {
                            continue;
                        }
                        match ipc::try_existing_lock_file(&directory.join("lease")) {
                            Ok(Some(_)) => continue,
                            Err(missing) if missing.kind() == io::ErrorKind::NotFound => continue,
                            _ => return Err(error),
                        }
                    }
                }
            }
        }
    }
    live.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(live)
}
/// Chooses the deepest canonical project containing a file or folder target.
/// A sibling prefix (e.g. `project-old`) cannot match `project`.
pub fn find_project(config: &Path, target: &Path) -> io::Result<Option<WindowInfo>> {
    let target = dunce::canonicalize(target)?;
    Ok(live_windows(config)?
        .into_iter()
        .filter(|window| {
            window
                .project
                .as_ref()
                .is_some_and(|root| target.starts_with(root))
        })
        .max_by_key(|window| {
            window
                .project
                .as_ref()
                .map(|root| root.components().count())
                .unwrap_or(0)
        }))
}

/// A verified registry observation plus every entry whose absence cannot be
/// established. An unproved busy lease is never an absent window.
#[derive(Clone, Debug)]
pub struct WindowPresence {
    pub live: Vec<WindowInfo>,
    pub uncertain: Vec<WindowUncertainty>,
}
#[derive(Clone, Debug)]
pub struct WindowUncertainty {
    pub id: Option<String>,
    pub reason: String,
}

/// Read-only discovery for startup coordination. Unlike routing discovery this
/// never prunes metadata, and includes unpublished starting directories as
/// uncertainty. Socket reads/writes recheck the overall deadline on each chunk.
pub fn presence_until(config: &Path, deadline: std::time::Instant) -> io::Result<WindowPresence> {
    let mut result = WindowPresence {
        live: Vec::new(),
        uncertain: Vec::new(),
    };
    let entries = match std::fs::read_dir(config.join(DIRECTORY)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(result),
        Err(error) => return Err(error),
    };
    let mut count = 0;
    for entry in entries {
        if std::time::Instant::now() >= deadline {
            result.uncertain.push(WindowUncertainty {
                id: None,
                reason: "registry deadline expired".into(),
            });
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if !kind.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().to_string();
        if id.len() != 64 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        count += 1;
        if count > MAX_WINDOWS {
            return Err(io::Error::other("window registry exceeds discovery limit"));
        }
        let directory = entry.path();
        let observed = (|| -> io::Result<Option<WindowInfo>> {
            match ipc::try_existing_lock_file(&directory.join("lease"))? {
                Some(_free_lease) => Ok(None), // OS proves no owner; leave metadata untouched.
                None => {
                    let info = read_record(&directory.join(RECORD))?;
                    if info.id != id {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "window metadata identity mismatch",
                        ));
                    }
                    verify_endpoint_until(&info, deadline).map(Some)
                }
            }
        })();
        match observed {
            Ok(Some(info)) => result.live.push(info),
            Ok(None) => {}
            Err(error) => {
                // A cleanly removed registration is absent. An existing starting
                // directory, busy lease or unproved endpoint remains uncertain.
                if directory.try_exists()? {
                    result.uncertain.push(WindowUncertainty {
                        id: Some(id),
                        reason: error.to_string(),
                    });
                }
            }
        }
    }
    result.live.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(result)
}
/// Registry directories not proved free by the OS: held leases plus starting
/// directories that have no lease yet. Filesystem-only (no endpoint I/O), so
/// it is cheap enough to recheck under another lock; never prunes anything.
pub fn occupied_ids(config: &Path) -> io::Result<std::collections::BTreeSet<String>> {
    let mut result = std::collections::BTreeSet::new();
    let entries = match std::fs::read_dir(config.join(DIRECTORY)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(result),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let id = entry.file_name().to_string_lossy().to_string();
        if id.len() != 64 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        match ipc::try_existing_lock_file(&entry.path().join("lease")) {
            Ok(Some(_free_lease)) => {}
            Err(error)
                if error.kind() == io::ErrorKind::NotFound && !entry.path().try_exists()? => {}
            _ => {
                result.insert(id);
            }
        }
    }
    Ok(result)
}
fn proof_budget(deadline: std::time::Instant) -> io::Result<Duration> {
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "registry proof deadline expired",
        ));
    }
    Ok(remaining.min(Duration::from_millis(500)))
}
pub(crate) fn verify_endpoint_until(
    info: &WindowInfo,
    deadline: std::time::Instant,
) -> io::Result<WindowInfo> {
    let nonce = ipc::new_token()?;
    let mut stream = TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, info.endpoint.port)),
        proof_budget(deadline)?,
    )?;
    let request = format!("{}\n", serde_json::json!({"window_nonce": nonce}));
    let mut outgoing = request.as_bytes();
    while !outgoing.is_empty() {
        stream.set_write_timeout(Some(proof_budget(deadline)?))?;
        let written = stream.write(outgoing)?;
        if written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "window proof write stopped",
            ));
        }
        outgoing = &outgoing[written..];
    }
    let mut answer = Vec::new();
    loop {
        stream.set_read_timeout(Some(proof_budget(deadline)?))?;
        let mut chunk = [0u8; 1024];
        let count = stream.read(&mut chunk)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "window proof ended before newline",
            ));
        }
        let end = chunk[..count]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|n| n + 1);
        answer.extend_from_slice(&chunk[..end.unwrap_or(count)]);
        if answer.len() > MAX_RECORD as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "window proof too large",
            ));
        }
        if end.is_some() {
            break;
        }
    }
    let answer: Proof = serde_json::from_slice(&answer).map_err(io::Error::other)?;
    let expected = mac(&info.endpoint.token, &answer.id, &answer.project, &nonce)?;
    if answer.id != info.id || answer.nonce != nonce || !ipc::same_secret(&answer.mac, &expected) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "window endpoint proof failed",
        ));
    }
    proof_budget(deadline)?;
    Ok(WindowInfo {
        project: answer.project,
        ..info.clone()
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::{GlobalOptions, OpenArgs, OpenTarget};
    use crate::instance::{launch, Launch};
    use std::process::{Child, Command, Stdio};
    use std::time::Instant;

    #[test]
    fn explicit_new_window_launch_registers_an_endpoint() {
        let directory = tempfile::tempdir().unwrap();
        let args = OpenArgs {
            new_window: true,
            ..Default::default()
        };
        let Launch::Start(Some(server)) =
            launch(&args, &GlobalOptions::default(), directory.path())
        else {
            panic!("secondary launch has no authenticated endpoint")
        };
        assert_eq!(live_windows(directory.path()).unwrap().len(), 1);
        assert_eq!(
            live_windows(directory.path()).unwrap()[0].id,
            server.window_id()
        );
        drop(server);
        assert!(live_windows(directory.path()).unwrap().is_empty());
    }

    #[test]
    fn authoritative_project_proof_rejects_tampering_and_selects_deepest_root() {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("project");
        let nested = project.join("nested");
        let sibling = directory.path().join("project-old");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir(&sibling).unwrap();
        let file = nested.join("a.py");
        std::fs::write(&file, b"pass\n").unwrap();
        let parent = ipc::Server::start_secondary(directory.path()).unwrap();
        parent.set_project(Some(&project)).unwrap();
        let child = ipc::Server::start_secondary(directory.path()).unwrap();
        child.set_project(Some(&nested)).unwrap();
        assert_eq!(
            find_project(directory.path(), &file).unwrap().unwrap().id,
            child.window_id()
        );
        assert!(find_project(directory.path(), &sibling).unwrap().is_none());
        let record = directory
            .path()
            .join(DIRECTORY)
            .join(child.window_id())
            .join(RECORD);
        let original = read_record(&record).unwrap();
        let mut corrupted = original.clone();
        corrupted.project = Some(sibling.clone());
        std::fs::write(&record, serde_json::to_vec(&corrupted).unwrap()).unwrap();
        assert_eq!(
            find_project(directory.path(), &file).unwrap().unwrap().id,
            child.window_id(),
            "project hints are not authoritative"
        );
        corrupted.endpoint.token = "0".repeat(64);
        std::fs::write(&record, serde_json::to_vec(&corrupted).unwrap()).unwrap();
        assert!(
            find_project(directory.path(), &file).is_err(),
            "invalid live endpoint cannot trigger fallback to a broader project"
        );
        child.set_project(Some(&sibling)).unwrap();
        assert_eq!(
            find_project(directory.path(), &sibling)
                .unwrap()
                .unwrap()
                .id,
            child.window_id()
        );
        let old = find_project(directory.path(), &sibling).unwrap().unwrap();
        assert!(child
            .set_project(Some(&directory.path().join("missing")))
            .is_err());
        assert_eq!(
            find_project(directory.path(), &sibling)
                .unwrap()
                .unwrap()
                .project,
            old.project
        );
        #[cfg(unix)]
        {
            let alias = directory.path().join("alias");
            std::os::unix::fs::symlink(&project, &alias).unwrap();
            assert_eq!(
                find_project(directory.path(), &alias.join("nested/a.py"))
                    .unwrap()
                    .unwrap()
                    .id,
                parent.window_id()
            );
        }
    }

    // A subprocess endpoint, not a GUI. Its stdin is a closed anonymous pipe.
    #[test]
    fn child_endpoint() {
        let Some(config) = std::env::var_os("RUSTPY_REGISTRY_CHILD_CONFIG") else {
            return;
        };
        let config = PathBuf::from(config);
        let project = PathBuf::from(std::env::var_os("RUSTPY_REGISTRY_CHILD_PROJECT").unwrap());
        let server = ipc::Server::start_secondary(&config).unwrap();
        server.set_project(Some(&project)).unwrap();
        std::fs::write(project.join("ready"), server.window_id()).unwrap();
        let exit_response_observed = project.join("exit-response-observed");
        assert!(
            !exit_response_observed.exists(),
            "fixture receipt must be fresh"
        );
        let mut waits = Vec::new();
        loop {
            let Some(incoming) = server.recv_timeout(Duration::from_secs(20)) else {
                panic!("parent stopped sending requests")
            };
            std::fs::write(
                project.join("received"),
                serde_json::to_vec(&incoming.targets).unwrap(),
            )
            .unwrap();
            let command = incoming
                .targets
                .first()
                .and_then(|target| target.path.file_name())
                .and_then(|name| name.to_str())
                .unwrap_or("")
                .to_string();
            if command == "reject" {
                drop(incoming);
                continue;
            }
            let opened = incoming
                .targets
                .iter()
                .map(|target| target.path.clone())
                .collect();
            if let Some(wait) = incoming.accept(opened) {
                waits.push(wait);
            }
            if command == "close" {
                for wait in waits.drain(..) {
                    wait.finish();
                }
            }
            if command == "exit" {
                // accept only queues the reply. Keep the actual process alive
                // until its parent has received the successful TCP response.
                // Reuse the endpoint fixture's existing 20-second bound.
                let deadline = Instant::now() + Duration::from_secs(20);
                while !exit_response_observed.exists() {
                    assert!(
                        Instant::now() < deadline,
                        "parent did not observe exit response"
                    );
                    std::thread::yield_now();
                }
                break;
            }
        }
    }
    struct EndpointChild(Child);
    impl Drop for EndpointChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn child(config: &Path, project: &Path) -> EndpointChild {
        let mut process = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "window_registry::tests::child_endpoint",
                "--nocapture",
            ])
            .env("RUSTPY_REGISTRY_CHILD_CONFIG", config)
            .env("RUSTPY_REGISTRY_CHILD_PROJECT", project)
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        drop(process.stdin.take());
        let mut child = EndpointChild(process);
        let deadline = Instant::now() + Duration::from_secs(15);
        while !project.join("ready").exists() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "endpoint child exited early"
            );
            assert!(Instant::now() < deadline, "endpoint child did not register");
            std::thread::sleep(Duration::from_millis(10));
        }
        child
    }
    fn request(
        window: &WindowInfo,
        project: &Path,
        command: &str,
        wait: bool,
    ) -> Result<ipc::Forwarded, ipc::ForwardError> {
        ipc::forward_window(
            window,
            &[OpenTarget::new(project.join(command))],
            wait,
            None,
        )
    }
    #[test]
    #[cfg(unix)] // Windows file-sharing and socket semantics differ; see known issues
    fn actual_two_process_routing_wait_disconnect_rejection_and_crash_pruning() {
        let directory = tempfile::tempdir().unwrap();
        let a = directory.path().join("a");
        let b = directory.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let config = directory.path().to_path_buf();
        let project = a.clone();
        let first_thread = std::thread::spawn(move || child(&config, &project));
        let mut second = child(directory.path(), &b);
        let mut first = first_thread.join().unwrap();
        let windows = live_windows(directory.path()).unwrap();
        assert_eq!(windows.len(), 2);
        let destination = find_project(directory.path(), &b).unwrap().unwrap();
        let waiting = request(&destination, &b, "file", true).unwrap();
        assert_eq!(waiting.opened, vec![b.join("file")]);
        let completed = std::thread::spawn(move || waiting.wait_closed());
        request(&destination, &b, "close", false).unwrap();
        assert!(completed.join().unwrap().is_ok());
        assert!(matches!(
            request(&destination, &b, "reject", false),
            Err(ipc::ForwardError::Rejected(_))
        ));
        let waiting = request(&destination, &b, "file", true).unwrap();
        second.0.kill().unwrap();
        second.0.wait().unwrap();
        assert!(waiting.wait_closed().is_ok());
        assert_eq!(live_windows(directory.path()).unwrap().len(), 1);
        assert!(find_project(directory.path(), &b).unwrap().is_none());
        assert!(!directory
            .path()
            .join(DIRECTORY)
            .join(destination.id)
            .join(RECORD)
            .exists());
        let remaining = find_project(directory.path(), &a).unwrap().unwrap();
        request(&remaining, &a, "exit", false).unwrap();
        // Publish this fresh child-specific receipt only after the unchanged
        // successful request proves its actual response reached the parent.
        std::fs::write(a.join("exit-response-observed"), b"").unwrap();
        assert!(first.0.wait().unwrap().success());
        assert!(live_windows(directory.path()).unwrap().is_empty());
    }
    #[test]
    fn actual_launcher_routes_child_and_does_not_fallback_after_rejection() {
        let directory = tempfile::tempdir().unwrap();
        let a = directory.path().join("a");
        let b = directory.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let ipc::Probe::Free(lock) = ipc::probe(directory.path()).unwrap() else {
            panic!()
        };
        let primary = ipc::Server::start(lock).unwrap();
        primary.set_project(Some(&a)).unwrap();
        let _child = child(directory.path(), &b);
        let file = b.join("file");
        std::fs::write(&file, b"pass\n").unwrap();
        let args = OpenArgs {
            targets: vec![OpenTarget::new(&file)],
            ..Default::default()
        };
        assert!(matches!(
            launch(&args, &GlobalOptions::default(), directory.path()),
            Launch::Done(0)
        ));
        assert!(
            primary.try_recv().is_none(),
            "primary must not receive a child project request"
        );
        let received: Vec<OpenTarget> =
            serde_json::from_slice(&std::fs::read(b.join("received")).unwrap()).unwrap();
        assert_eq!(received, args.targets);
        let rejected = b.join("reject");
        std::fs::write(&rejected, b"pass\n").unwrap();
        let args = OpenArgs {
            targets: vec![OpenTarget::new(&rejected)],
            wait: true,
            ..Default::default()
        };
        assert!(matches!(
            launch(&args, &GlobalOptions::default(), directory.path()),
            Launch::Done(1)
        ));
        assert!(primary.try_recv().is_none());
        let waited = b.join("waited");
        std::fs::write(&waited, b"pass\n").unwrap();
        let wait_target = OpenTarget::new(&waited);
        let expected = vec![wait_target.clone()];
        let args = OpenArgs {
            targets: vec![wait_target],
            wait: true,
            ..Default::default()
        };
        let config = directory.path().to_path_buf();
        let waiter = std::thread::spawn(move || launch(&args, &GlobalOptions::default(), &config));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let received = std::fs::read(b.join("received"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Vec<OpenTarget>>(&bytes).ok());
            if received.as_ref() == Some(&expected) {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !waiter.is_finished(),
            "actual launcher must wait after child ACK"
        );
        let destination = find_project(directory.path(), &b).unwrap().unwrap();
        request(&destination, &b, "close", false).unwrap();
        assert!(matches!(waiter.join().unwrap(), Launch::Done(0)));
        assert!(primary.try_recv().is_none());
        let mut stale = find_project(directory.path(), &file).unwrap().unwrap();
        stale.project = Some(a);
        assert!(matches!(
            ipc::forward_window(&stale, &[OpenTarget::new(&file)], false, None),
            Err(ipc::ForwardError::Rejected(_))
        ));
    }

    #[test]
    fn routed_request_checks_project_before_queue_and_publish_error_keeps_endpoint_truth() {
        let directory = tempfile::tempdir().unwrap();
        let a = directory.path().join("a");
        let b = directory.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        let server = ipc::Server::start_secondary(directory.path()).unwrap();
        server.set_project(Some(&a)).unwrap();
        let original = find_project(directory.path(), &a).unwrap().unwrap();
        let record = directory
            .path()
            .join(DIRECTORY)
            .join(server.window_id())
            .join(RECORD);
        let bytes = std::fs::read(&record).unwrap();
        std::fs::remove_file(&record).unwrap();
        std::fs::create_dir(&record).unwrap();
        assert!(server.set_project(Some(&b)).is_err());
        std::fs::remove_dir(&record).unwrap();
        std::fs::write(&record, bytes).unwrap();
        let current = find_project(directory.path(), &b).unwrap().unwrap();
        assert_eq!(
            current.project,
            Some(b.clone()),
            "accepted project truth survives failed hint write"
        );
        assert!(find_project(directory.path(), &a).unwrap().is_none());
        let request = ipc::Request {
            native_window: None,
            protocol: ipc::PROTOCOL,
            token: original.endpoint.token,
            targets: vec![OpenTarget::new(&a)],
            wait: true,
            cwd: None,
            expected_window: Some(ExpectedWindow {
                id: original.id,
                project: original.project,
            }),
        };
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, current.endpoint.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        writeln!(stream, "{}", serde_json::to_string(&request).unwrap()).unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        assert!(
            matches!(serde_json::from_str::<ipc::Response>(&line).unwrap(), ipc::Response::Error { message } if message.contains("changed project"))
        );
        assert!(
            server.try_recv().is_none(),
            "stale routed request must not reach UI"
        );
    }

    #[test]
    fn endpoint_challenge_rejects_replay_without_disclosing_token() {
        let directory = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let token = ipc::new_token().unwrap();
        let registration = Registration::create(
            directory.path(),
            InstanceInfo {
                native_project_host: false,
                pid: 0,
                port: listener.local_addr().unwrap().port(),
                token: token.clone(),
                version: String::new(),
            },
        )
        .unwrap();
        let old_proof = proof(&registration.info, "0".repeat(64)).unwrap();
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            assert!(
                !line.contains(&token),
                "challenge must not send endpoint secret"
            );
            writeln!(stream, "{}", serde_json::to_string(&old_proof).unwrap()).unwrap();
        });
        assert_eq!(
            live_windows(directory.path()).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        thread.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn nonregular_lease_is_rejected_without_fifo_blocking() {
        use std::os::unix::ffi::OsStrExt;
        let directory = tempfile::tempdir().unwrap();
        let lease = directory.path().join("fifo");
        let name = std::ffi::CString::new(lease.as_os_str().as_bytes()).unwrap();
        // SAFETY: nul-terminated name is valid until mkfifo returns.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert_eq!(
            ipc::try_lock_file(&lease).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn abandoned_unpublished_directories_do_not_exhaust_discovery() {
        let directory = tempfile::tempdir().unwrap();
        for _ in 0..MAX_WINDOWS + 1 {
            std::fs::create_dir_all(
                directory
                    .path()
                    .join(DIRECTORY)
                    .join(ipc::new_token().unwrap()),
            )
            .unwrap();
        }
        let server = ipc::Server::start_secondary(directory.path()).unwrap();
        server.set_project(Some(directory.path())).unwrap();
        assert_eq!(live_windows(directory.path()).unwrap().len(), 1);
        assert_eq!(
            find_project(directory.path(), directory.path())
                .unwrap()
                .unwrap()
                .id,
            server.window_id()
        );
    }

    #[test]
    fn published_windows_still_have_a_bounded_discovery_limit() {
        let directory = tempfile::tempdir().unwrap();
        let servers: Vec<_> = (0..MAX_WINDOWS + 1)
            .map(|_| ipc::Server::start_secondary(directory.path()).unwrap())
            .collect();
        let error = live_windows(directory.path()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(error.to_string().contains("discovery limit"));
        drop(servers);
        assert!(live_windows(directory.path()).unwrap().is_empty());
    }

    #[test]
    fn discovery_does_not_steal_a_starting_registration_lease() {
        let directory = tempfile::tempdir().unwrap();
        let id = ipc::new_token().unwrap();
        let pending = directory.path().join(DIRECTORY).join(&id);
        std::fs::create_dir_all(&pending).unwrap();
        assert!(live_windows(directory.path()).unwrap().is_empty());
        assert!(
            !pending.join("lease").exists(),
            "discovery cannot manufacture or claim an unpublished lease"
        );
        let lease = ipc::try_lock_file(&pending.join("lease")).unwrap().unwrap();
        assert!(live_windows(directory.path()).unwrap().is_empty());
        assert!(
            pending.exists(),
            "live unpublished owner must not be pruned"
        );
        drop(lease);
    }
    #[test]
    #[cfg(unix)] // Windows file-sharing and socket semantics differ; see known issues
    fn registration_and_discovery_concurrent_stress() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_path_buf();
        let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observer_finished = finished.clone();
        let observer_root = root.clone();
        let observer = std::thread::spawn(move || {
            while !observer_finished.load(std::sync::atomic::Ordering::SeqCst) {
                live_windows(&observer_root).unwrap();
                std::thread::yield_now();
            }
        });
        let creators: Vec<_> = (0..8)
            .map(|_| {
                let root = root.clone();
                std::thread::spawn(move || {
                    for _ in 0..16 {
                        let server = ipc::Server::start_secondary(&root).unwrap();
                        std::thread::yield_now();
                        drop(server);
                    }
                })
            })
            .collect();
        for creator in creators {
            creator.join().unwrap();
        }
        finished.store(true, std::sync::atomic::Ordering::SeqCst);
        observer.join().unwrap();
        assert!(live_windows(&root).unwrap().is_empty());
    }

    #[test]
    fn concurrent_registration_has_distinct_leases_and_identity() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().to_path_buf();
        let servers: Vec<_> = (0..8)
            .map(|_| {
                let root = root.clone();
                std::thread::spawn(move || ipc::Server::start_secondary(&root).unwrap())
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        let live = live_windows(directory.path()).unwrap();
        assert_eq!(live.len(), servers.len());
        let ids: std::collections::HashSet<_> = live.iter().map(|window| &window.id).collect();
        assert_eq!(ids.len(), 8);
        drop(servers);
        assert!(live_windows(directory.path()).unwrap().is_empty());
    }
}

#[cfg(test)]
mod restore_presence_tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn busy_unpublished_lease_is_uncertain_and_free_metadata_is_not_pruned() {
        let config = tempfile::tempdir().unwrap();
        let id = ipc::new_token().unwrap();
        let directory = config.path().join(DIRECTORY).join(&id);
        std::fs::create_dir_all(&directory).unwrap();
        let lease = ipc::try_lock_file(&directory.join("lease"))
            .unwrap()
            .unwrap();
        let presence =
            presence_until(config.path(), Instant::now() + Duration::from_secs(1)).unwrap();
        assert!(presence.live.is_empty());
        assert_eq!(presence.uncertain.len(), 1);
        assert_eq!(presence.uncertain[0].id.as_deref(), Some(id.as_str()));
        drop(lease);
        let presence =
            presence_until(config.path(), Instant::now() + Duration::from_secs(1)).unwrap();
        assert!(presence.live.is_empty());
        assert!(presence.uncertain.is_empty());
        assert!(directory.join("lease").exists());
    }

    #[test]
    fn authentic_endpoint_is_proved_and_stalled_endpoint_is_uncertain_within_deadline() {
        let config = tempfile::tempdir().unwrap();
        let real = ipc::Server::start_secondary(config.path()).unwrap();
        let presence =
            presence_until(config.path(), Instant::now() + Duration::from_secs(1)).unwrap();
        assert!(presence.uncertain.is_empty());
        assert_eq!(presence.live.len(), 1);
        assert_eq!(presence.live[0].id, real.window_id());
        drop(real);
        // Server teardown runs on its endpoint thread. The stalled-peer fixture
        // owns a distinct configuration and cannot assume synchronous removal.
        let config = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let registration = Registration::create(
            config.path(),
            InstanceInfo {
                native_project_host: false,
                pid: std::process::id(),
                port: listener.local_addr().unwrap().port(),
                token: ipc::new_token().unwrap(),
                version: String::new(),
            },
        )
        .unwrap();
        let start = Instant::now();
        let presence = presence_until(config.path(), start + Duration::from_millis(80)).unwrap();
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(presence.live.is_empty());
        assert_eq!(presence.uncertain.len(), 1);
        assert_eq!(
            presence.uncertain[0].id.as_deref(),
            Some(registration.info.id.as_str())
        );
    }
    #[test]
    fn nonce_proof_trickle_cannot_extend_the_absolute_deadline() {
        let config = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let registration = Registration::create(
            config.path(),
            InstanceInfo {
                native_project_host: false,
                pid: std::process::id(),
                port: listener.local_addr().unwrap().port(),
                token: ipc::new_token().unwrap(),
                version: String::new(),
            },
        )
        .unwrap();
        let info = registration.info.clone();
        let peer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let request: serde_json::Value = serde_json::from_str(&request).unwrap();
            let proof = proof(&info, request["window_nonce"].as_str().unwrap().to_owned()).unwrap();
            let mut bytes = serde_json::to_vec(&proof).unwrap();
            bytes.push(b'\n');
            for byte in bytes {
                if stream.write_all(&[byte]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        let start = Instant::now();
        let result = verify_endpoint_until(&registration.info, start + Duration::from_millis(80));
        let elapsed = start.elapsed();
        peer.join().unwrap();
        assert!(result.is_err());
        assert!(
            elapsed < Duration::from_millis(500),
            "trickle extended deadline: {elapsed:?}"
        );
    }
}
