//! Actual RustPy CLI server and the production Gateway client; no substitute peer.
use super::*;
use rustpy_lsp::gateway::Query;
use sha2::{Digest, Sha256};
use std::{fs, os::unix::fs::MetadataExt, process::Command, thread};
#[path = "../../tests/support/configured_openssh.rs"]
mod openssh_support;

struct OwnedSshSessions(Vec<openssh_support::PinnedSession>);
impl Drop for OwnedSshSessions {
    fn drop(&mut self) {
        for session in &self.0 {
            session.terminate();
        }
        // Already captured pidfds remain authoritative even if sshd reparents
        // a remote command to the scoped fixture subreaper during disconnect.
        while self.0.iter().any(|s| !openssh_support::retired(s.captured)) {
            for session in &self.0 {
                session.reap_if_adopted();
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn remote_policy(pid: u32) {
    // Use the kernel's actual inherited mask, not a guessed logical CPU count.
    let allowed = fs::read_to_string("/proc/self/status").unwrap();
    let allowed = allowed
        .lines()
        .find_map(|line| line.strip_prefix("Cpus_allowed_list:\t"))
        .unwrap();
    let mask = |text: &str| {
        text.trim()
            .split(',')
            .flat_map(|part| {
                let (first, last) = part.split_once('-').unwrap_or((part, part));
                first.parse::<u32>().unwrap()..=last.parse::<u32>().unwrap()
            })
            .collect::<std::collections::BTreeSet<_>>()
    };
    let expected = mask(allowed);
    assert_eq!(expected.len(), 2);
    assert_eq!(
        expected
            .iter()
            .map(|cpu| {
                let root = PathBuf::from(format!("/sys/devices/system/cpu/cpu{cpu}/topology"));
                (
                    fs::read_to_string(root.join("physical_package_id")).unwrap(),
                    fs::read_to_string(root.join("core_id")).unwrap(),
                )
            })
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        2
    );
    let before = identity(pid).expect("remote process before policy read");
    for task in fs::read_dir(format!("/proc/{pid}/task")).unwrap() {
        let task = task.unwrap();
        let status = match fs::read_to_string(task.path().join("status")) {
            Ok(status) => status,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => panic!("cannot observe actual remote thread: {error}"),
        };
        let actual = status
            .lines()
            .find_map(|line| line.strip_prefix("Cpus_allowed_list:\t"))
            .unwrap();
        assert!(mask(actual).is_subset(&expected));
        let stat = match fs::read_to_string(task.path().join("stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => panic!("cannot observe actual remote priority: {error}"),
        };
        assert!(
            stat.rsplit(") ")
                .next()
                .unwrap()
                .split_whitespace()
                .nth(16)
                .unwrap()
                .parse::<i32>()
                .unwrap()
                >= 10
        );
    }
    let environment = fs::read(format!("/proc/{pid}/environ")).unwrap();
    for name in [
        "RAYON_NUM_THREADS",
        "OMP_NUM_THREADS",
        "OPENBLAS_NUM_THREADS",
        "MKL_NUM_THREADS",
        "NUMEXPR_NUM_THREADS",
    ] {
        let value = format!("{name}=2");
        assert!(
            environment
                .split(|byte| *byte == 0)
                .any(|entry| entry == value.as_bytes()),
            "actual remote session lacks {value}"
        );
    }
    assert_eq!(identity(pid).as_deref(), Some(before.as_str()));
}

fn remote_identity(
    binary: &Path,
    root: &Path,
    daemon: openssh_support::Identity,
) -> Option<openssh_support::Identity> {
    let executable = fs::metadata(binary).unwrap();
    let mut found = Vec::new();
    for entry in fs::read_dir("/proc").unwrap().flatten() {
        let Some(member) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse().ok())
            .and_then(openssh_support::identity)
        else {
            continue;
        };
        if !openssh_support::is_owned_descendant(member, daemon) {
            continue;
        }
        let Ok(actual) = fs::metadata(entry.path().join("exe")) else {
            continue;
        };
        if (actual.dev(), actual.ino()) != (executable.dev(), executable.ino()) {
            continue;
        }
        if dunce::canonicalize(entry.path().join("cwd"))
            .ok()
            .as_deref()
            != Some(root)
        {
            continue;
        }
        let Ok(command) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let args = command
            .split(|byte| *byte == 0)
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        if args
            != [
                binary.as_os_str().as_encoded_bytes(),
                b"remote-server",
                b"--root",
                root.as_os_str().as_encoded_bytes(),
                b"--admit-client",
            ]
        {
            continue;
        }
        if openssh_support::identity(member.pid).is_some_and(|after| {
            after.start == member.start && openssh_support::is_owned_descendant(after, daemon)
        }) {
            found.push(member);
        }
    }
    assert!(found.len() <= 1, "ambiguous genuine remote Gateway process");
    found.pop()
}

fn digest(path: &Path) -> String {
    let mut file = fs::File::open(path).unwrap();
    let mut hash = Sha256::new();
    let mut bytes = [0; 65536];
    loop {
        let count = file.read(&mut bytes).unwrap();
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
    }
    format!("{:x}", hash.finalize())
}

fn identity(pid: u32) -> Option<String> {
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()?
        .rsplit(") ")
        .next()?
        .split_whitespace()
        .nth(19)
        .map(str::to_owned)
}

fn until(mut condition: impl FnMut() -> bool, message: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !condition() {
        assert!(Instant::now() < deadline, "{message}");
        thread::sleep(Duration::from_millis(10));
    }
}

// The public Client/Forward Drop methods request asynchronous retirement.
// Retain their actual completion flags through assertion unwind, before the
// private project or owned listener can disappear. A deadline never releases
// an owner; bounded assertions still fail and the guards drain afterward.
struct OwnedClient {
    client: Option<Client>,
    shared: Arc<Shared>,
}
impl OwnedClient {
    fn spawn(target: Target) -> (Self, Reply) {
        let (client, reply) = Client::spawn(target, &[]).expect("actual RustPy server spawn");
        let shared = client.shared.clone();
        (
            Self {
                client: Some(client),
                shared,
            },
            reply,
        )
    }
    fn get(&self) -> &Client {
        self.client.as_ref().unwrap()
    }
    fn request(&self, request: Request) -> Result<Value, String> {
        let (_, reply) = self.get().request(request)?;
        reply
            .recv_timeout(Duration::from_secs(15))
            .expect("actual Gateway reply deadline")
    }
    fn drop_client(&mut self) {
        drop(self.client.take());
    }
}
impl Drop for OwnedClient {
    fn drop(&mut self) {
        if let Some(client) = &self.client {
            client.stop();
        }
        self.drop_client();
        while !self.shared.done.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(10));
        }
    }
}

struct OwnedForward {
    forward: Option<Forward>,
    done: Arc<AtomicBool>,
    port: u16,
    inode: String,
}
impl OwnedForward {
    fn new(client: &Client, remote: u16) -> Self {
        let forward = client
            .forward(0, remote)
            .expect("actual loopback forwarding listener");
        let mut owned = Self {
            done: forward.done.clone(),
            port: forward.local,
            forward: Some(forward),
            inode: String::new(),
        };
        let address = format!("0100007F:{:04X}", owned.port);
        let table = fs::read_to_string("/proc/net/tcp").unwrap();
        let rows = table
            .lines()
            .skip(1)
            .map(|line| line.split_whitespace().collect::<Vec<_>>())
            .filter(|row| row[1] == address && row[3] == "0A")
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 1, "actual forwarding listener must be unique");
        owned.inode = rows[0][9].to_owned();
        assert!(
            owned.has_descriptor(),
            "this process must own the captured listener inode"
        );
        owned
    }
    fn has_descriptor(&self) -> bool {
        let expected = format!("socket:[{}]", self.inode);
        fs::read_dir("/proc/self/fd")
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| fs::read_link(entry.path()).ok())
            .any(|link| link.as_os_str() == std::ffi::OsStr::new(&expected))
    }
    fn assert_retired(&self) {
        assert!(!self.has_descriptor(), "owned listener descriptor remains");
        assert!(
            !fs::read_to_string("/proc/net/tcp")
                .unwrap()
                .lines()
                .skip(1)
                .any(|line| line.split_whitespace().nth(9) == Some(self.inode.as_str())),
            "exact owned listener inode remains; numeric port reuse is irrelevant"
        );
    }
    fn drop_forward(&mut self) {
        drop(self.forward.take());
    }
}
impl Drop for OwnedForward {
    fn drop(&mut self) {
        if let Some(forward) = &self.forward {
            forward.stop();
        }
        self.drop_forward();
        while !self.done.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(10));
        }
    }
}

struct Echo {
    port: u16,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<Result<Vec<u8>, String>>>,
}
impl Echo {
    fn start(request: &'static [u8], response: &'static [u8]) -> Self {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = stop.clone();
        let worker = thread::spawn(move || {
            while !cancelled.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut socket, peer)) => {
                        assert!(peer.ip().is_loopback());
                        socket
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .map_err(|e| e.to_string())?;
                        socket
                            .set_write_timeout(Some(Duration::from_secs(2)))
                            .map_err(|e| e.to_string())?;
                        let mut actual = vec![0; request.len()];
                        socket.read_exact(&mut actual).map_err(|e| e.to_string())?;
                        if actual != request {
                            return Err("real remote endpoint received different bytes".into());
                        }
                        socket.write_all(response).map_err(|e| e.to_string())?;
                        socket.shutdown(Shutdown::Both).map_err(|e| e.to_string())?;
                        return Ok(actual);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => return Err(error.to_string()),
                }
            }
            Err("owned endpoint cancelled".into())
        });
        Self {
            port,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Echo {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn server_identity(client: &Client, binary: &Path, root: &Path) -> (u32, String) {
    let stamp = client.shared.stamp.lock().unwrap().clone();
    let pid: u32 = stamp
        .split(':')
        .find_map(|part| part.strip_prefix("pid="))
        .unwrap()
        .parse()
        .unwrap();
    let start = identity(pid).expect("actual server identity");
    let executable = fs::metadata(format!("/proc/{pid}/exe")).unwrap();
    let admitted = fs::metadata(binary).unwrap();
    assert_eq!(
        (executable.dev(), executable.ino()),
        (admitted.dev(), admitted.ino())
    );
    assert_eq!(
        dunce::canonicalize(format!("/proc/{pid}/cwd")).unwrap(),
        root
    );
    let command = fs::read(format!("/proc/{pid}/cmdline")).unwrap();
    let args = command
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(
        args,
        [
            binary.as_os_str().as_encoded_bytes(),
            b"remote-server",
            b"--root",
            root.as_os_str().as_encoded_bytes(),
            b"--admit-client"
        ]
    );
    assert_eq!(identity(pid).as_deref(), Some(start.as_str()));
    (pid, start)
}

fn admitted_binary() -> (PathBuf, String) {
    assert_eq!(
        std::env::var("RUSTPY_GATEWAY_ADMITTED").as_deref(),
        Ok("yes")
    );
    let binary = PathBuf::from(
        std::env::var_os("RUSTPY_GATEWAY_BINARY").expect("absolute actual RustPy CLI ELF"),
    );
    let binary_sha =
        std::env::var("RUSTPY_GATEWAY_BINARY_SHA256").expect("Root-bound actual ELF SHA256");
    let head = std::env::var("RUSTPY_GATEWAY_SOURCE_HEAD").expect("Root-bound source HEAD");
    assert!(binary.is_absolute() && binary.is_file());
    assert_eq!(digest(&binary), binary_sha);
    assert_eq!(head.len(), 40);
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let mut command = Command::new("git");
    command.args(["rev-parse", "HEAD"]).current_dir(repository);
    let (stdout, _) = rustpy_project::native_skeleton::supervised_tool(
        &mut command,
        &rustpy_core::CancellationToken::new(),
        Duration::from_secs(10),
        4096,
    )
    .unwrap();
    assert_eq!(String::from_utf8(stdout).unwrap().trim(), head);
    (binary, binary_sha)
}

#[test]
#[ignore = "requires explicit admitted same-source RustPy CLI ELF/SHA/head; real Gateway server and owned loopback only"]
fn configured_local_gateway_server_checked_files_analysis_forward_stop_and_drop() {
    let (binary, binary_sha) = admitted_binary();
    let files = tempfile::Builder::new()
        .prefix("gateway-λ-")
        .tempdir()
        .unwrap();
    let root = dunce::canonicalize(files.path()).unwrap();
    let path = "main π.py";
    let initial = "value = 1\n";
    let current = "from model import RemoteWidget\nvalue = 42\nRemoteWidget\n";
    fs::write(root.join(path), initial).unwrap();
    fs::write(root.join("model.py"), "class RemoteWidget:\n    pass\n").unwrap();
    let target = Target {
        kind: Kind::Local,
        root: root.to_str().unwrap().into(),
        executable: binary.to_str().unwrap().into(),
        local_cwd: root.clone(),
        ..Default::default()
    };
    let (client, hello) = OwnedClient::spawn(target.clone());
    let hello = hello
        .recv_timeout(Duration::from_secs(15))
        .unwrap()
        .expect("real server Hello");
    assert_eq!(hello["root"], root.to_str().unwrap());
    assert_eq!(hello["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(hello["protocol"], gateway::PROTOCOL);
    assert!(client.get().ready());
    let server = server_identity(client.get(), &binary, &root);
    let read = client.request(Request::Read { path: path.into() }).unwrap();
    assert_eq!(read["text"], initial);
    let revision = read["revision"].as_str().unwrap().to_owned();
    client
        .request(Request::Write {
            path: path.into(),
            base: revision.clone(),
            text: current.into(),
        })
        .unwrap();
    assert_eq!(fs::read_to_string(root.join(path)).unwrap(), current);
    let error = client
        .request(Request::Write {
            path: path.into(),
            base: revision,
            text: "must not replace current π\n".into(),
        })
        .unwrap_err();
    assert!(error.contains("changed"));
    assert_eq!(fs::read_to_string(root.join(path)).unwrap(), current);
    assert!(client
        .request(Request::Read {
            path: "../outside.py".into()
        })
        .is_err());
    let stale = client
        .request(Request::Analyze {
            path: path.into(),
            text: "unknown_gateway_name\n".into(),
            version: 17,
            cursor: 0,
            query: Query::Diagnostics,
        })
        .unwrap();
    assert_eq!(stale["version"], 17);
    assert_eq!(stale["query"], "diagnostics");
    assert!(!stale["payload"]["diagnostics"]
        .as_array()
        .unwrap()
        .is_empty());
    let analysis = client
        .request(Request::Analyze {
            path: path.into(),
            text: current.into(),
            version: 18,
            cursor: current.len() as u32 - 2,
            query: Query::Definition,
        })
        .unwrap();
    assert_eq!(analysis["version"], 18);
    assert!(analysis["payload"]["locations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["path"] == "model.py"));
    assert_eq!(
        fs::read_to_string(root.join(path)).unwrap(),
        current,
        "unsaved old analysis must not overwrite checked-written files"
    );

    let request = "request π λ 42".as_bytes();
    let response = "real loopback reply λ π 42".as_bytes();
    let mut echo = Echo::start(request, response);
    let forward = OwnedForward::new(client.get(), echo.port);
    let mut socket = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, forward.port)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    socket.write_all(request).unwrap();
    let mut actual = vec![0; response.len()];
    socket.read_exact(&mut actual).unwrap();
    assert_eq!(actual, response);
    drop(socket);
    assert_eq!(
        echo.worker.take().unwrap().join().unwrap().unwrap(),
        request
    );
    forward.forward.as_ref().unwrap().stop();
    until(
        || forward.done.load(Ordering::Acquire),
        "forward Stop retirement",
    );
    forward.assert_retired();
    drop(forward);
    let mut dropped_forward = OwnedForward::new(client.get(), echo.port);
    dropped_forward.drop_forward();
    until(
        || dropped_forward.done.load(Ordering::Acquire),
        "forward Drop retirement",
    );
    dropped_forward.assert_retired();
    drop(dropped_forward);
    client.get().stop();
    until(|| client.get().finished(), "actual server Stop retirement");
    assert_ne!(identity(server.0).as_deref(), Some(server.1.as_str()));
    assert!(client
        .get()
        .request(Request::Read { path: path.into() })
        .is_err());
    drop(client);

    let (mut dropped, hello) = OwnedClient::spawn(target);
    hello
        .recv_timeout(Duration::from_secs(15))
        .unwrap()
        .unwrap();
    let server = server_identity(dropped.get(), &binary, &root);
    dropped.drop_client(); // exercise the public Drop, retaining only observer flags
    until(
        || dropped.shared.done.load(Ordering::Acquire),
        "actual server Drop retirement",
    );
    assert_ne!(identity(server.0).as_deref(), Some(server.1.as_str()));
    assert_eq!(digest(&binary), binary_sha);
    assert_eq!(fs::read_to_string(root.join(path)).unwrap(), current);
}

#[test]
#[ignore = "requires explicitly admitted matching RustPy CLI and genuine private OpenSSH tools"]
fn configured_ssh_gateway_checked_files_analysis_forward_and_remote_retirement() {
    assert_eq!(
        std::env::var("RUSTPY_GATEWAY_SSH_ADMITTED").as_deref(),
        Ok("yes")
    );
    let (binary, binary_sha) = admitted_binary();
    let sshd = openssh_support::executable("RUSTPY_TEST_SSHD");
    let ssh = openssh_support::executable("RUSTPY_TEST_SSH");
    let keygen = openssh_support::executable("RUSTPY_TEST_SSH_KEYGEN");
    let user = std::env::var("RUSTPY_TEST_SSH_USER").expect("explicit private SSH login");
    let mut fixture = openssh_support::Fixture::start(&sshd, &keygen, &user);
    assert_eq!(
        fs::read_to_string(&fixture.known).unwrap(),
        fixture.known_line("host.pub")
    );
    until(
        || {
            let daemon = fixture.daemon.as_mut().unwrap();
            assert!(
                daemon.owner.poll(&mut daemon.child).unwrap().is_none(),
                "private sshd exited: {}",
                fs::read_to_string(&fixture.log).unwrap_or_default()
            );
            TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, fixture.port)).is_ok()
        },
        "private SSH daemon readiness",
    );
    let root = fixture.directory.path().join("Gateway project π");
    fs::create_dir(&root).unwrap();
    let root = dunce::canonicalize(&root).unwrap();
    let path = "main π.py";
    fs::write(root.join(path), "value = 1\n").unwrap();
    fs::write(root.join("model.py"), "class RemoteWidget:\n    pass\n").unwrap();
    let target = Target {
        kind: Kind::Ssh,
        target: "127.0.0.1".into(),
        user,
        port: fixture.port,
        key: fixture
            .directory
            .path()
            .join("user")
            .to_str()
            .unwrap()
            .into(),
        ssh_config: fixture.client_config.to_str().unwrap().into(),
        transport: ssh.to_str().unwrap().into(),
        root: root.to_str().unwrap().into(),
        executable: binary.to_str().unwrap().into(),
        local_cwd: root.clone(),
        ..Default::default()
    };
    let (client, hello) = OwnedClient::spawn(target);
    let hello = hello
        .recv_timeout(Duration::from_secs(15))
        .unwrap()
        .expect("real SSH Gateway Hello");
    let daemon = fixture.daemon.as_ref().unwrap().identity;
    let sessions = OwnedSshSessions(openssh_support::PinnedSession::capture(daemon));
    let remote = remote_identity(&binary, &root, daemon).expect("actual remote RustPy process");
    assert!(sessions.0.iter().any(
        |session| session.captured.pid == remote.pid && session.captured.start == remote.start
    ));
    assert_eq!(hello["root"], root.to_str().unwrap());
    assert_eq!(hello["protocol"], gateway::PROTOCOL);
    assert_eq!(hello["version"], env!("CARGO_PKG_VERSION"));
    remote_policy(remote.pid);
    let read = client.request(Request::Read { path: path.into() }).unwrap();
    assert_eq!(read["text"], "value = 1\n");
    let base = read["revision"].as_str().unwrap().to_owned();
    let current = "from model import RemoteWidget\nvalue = 42\nRemoteWidget\n";
    client
        .request(Request::Write {
            path: path.into(),
            base: base.clone(),
            text: current.into(),
        })
        .unwrap();
    assert!(client
        .request(Request::Write {
            path: path.into(),
            base,
            text: "stale π\n".into()
        })
        .unwrap_err()
        .contains("changed"));
    let analysis = client
        .request(Request::Analyze {
            path: path.into(),
            text: current.into(),
            version: 18,
            cursor: current.len() as u32 - 2,
            query: Query::Definition,
        })
        .unwrap();
    assert_eq!(analysis["version"], 18);
    assert!(analysis["payload"]["locations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["path"] == "model.py"));
    remote_policy(remote.pid);
    let request = "actual SSH π λ 42".as_bytes();
    let response = "remote forwarded π 42".as_bytes();
    let mut echo = Echo::start(request, response);
    let forward = OwnedForward::new(client.get(), echo.port);
    let mut socket = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, forward.port)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    socket.write_all(request).unwrap();
    let mut actual = vec![0; response.len()];
    socket.read_exact(&mut actual).unwrap();
    assert_eq!(actual, response);
    drop(socket);
    assert_eq!(
        echo.worker.take().unwrap().join().unwrap().unwrap(),
        request
    );
    forward.forward.as_ref().unwrap().stop();
    until(
        || forward.done.load(Ordering::Acquire),
        "SSH forward retirement",
    );
    forward.assert_retired();
    client.get().stop();
    until(|| client.get().finished(), "SSH client physical retirement");
    until(
        || {
            for session in &sessions.0 {
                session.reap_if_adopted();
            }
            openssh_support::retired(remote)
        },
        "actual remote RustPy retirement",
    );
    assert_eq!(fs::read_to_string(root.join(path)).unwrap(), current);
    assert_eq!(digest(&binary), binary_sha);
    drop(forward);
    drop(sessions);
    drop(client);
    fixture.daemon.as_mut().unwrap().stop();
}
