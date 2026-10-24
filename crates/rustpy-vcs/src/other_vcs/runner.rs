//! Owned, cancellable provider processes. Passwords use stdin or a short-lived
//! private Mercurial config file, never command arguments or persisted settings.
use super::{CommandPlan, Credentials, Operation};
use crate::{provider::Provider, GitEnv};
use std::{
    io::{self, Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc, Mutex,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
const OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
const DRAIN_CHUNKS: usize = 64;
const DRAIN_BYTES: usize = 256 * 1024;
static NEXT_AUTH: AtomicU64 = AtomicU64::new(1);
struct Retiring {
    label: String,
    worker: JoinHandle<()>,
}
static RETIRING: Mutex<Vec<Retiring>> = Mutex::new(Vec::new());
/// Poll dropped jobs even when their project/tool window has closed. Labels
/// remain until the actual worker, owned child tree and pipe readers retire.
pub fn poll_retiring() -> Vec<String> {
    let mut owners = RETIRING.lock().unwrap_or_else(|error| error.into_inner());
    let mut labels = Vec::new();
    let mut index = 0;
    while index < owners.len() {
        if owners[index].worker.is_finished() {
            let _ = owners.swap_remove(index).worker.join();
        } else {
            labels.push(owners[index].label.clone());
            index += 1;
        }
    }
    labels
}
pub enum Event {
    Output { stderr: bool, text: String },
    Done(Completed),
}
pub struct Completed {
    pub root: PathBuf,
    pub provider: Provider,
    pub operation: Operation,
    pub success: bool,
    pub cancelled: bool,
    pub exit: i32,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub elapsed: Duration,
}
pub struct ProviderJob {
    pub root: PathBuf,
    pub provider: Provider,
    pub operation: Operation,
    pub started: Instant,
    rx: Receiver<Event>,
    cancel: Arc<AtomicBool>,
    finished: bool,
    worker: Option<JoinHandle<()>>,
    pending: Option<Completed>,
}
impl ProviderJob {
    pub fn start(
        plan: CommandPlan,
        credentials: Credentials,
        env: GitEnv,
        endpoint: &str,
    ) -> anyhow::Result<Self> {
        Self::start_with_history(plan, credentials, env, endpoint, None)
    }
    pub fn start_with_history(
        plan: CommandPlan,
        credentials: Credentials,
        env: GitEnv,
        endpoint: &str,
        history: Option<(PathBuf, Vec<PathBuf>)>,
    ) -> anyhow::Result<Self> {
        let reservation = crate::RepositoryReservation::try_acquire(
            &plan.root,
            format!("{} {}", plan.provider.label(), plan.operation.label()),
        )?;
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let (tx, rx) = mpsc::sync_channel(256);
        let (root, provider, operation) = (plan.root.clone(), plan.provider, plan.operation);
        let started = Instant::now();
        let endpoint = endpoint.to_string();
        let worker = std::thread::Builder::new()
            .name("vcs-provider-owner".into())
            .spawn(move || {
                reservation.run(|| {
                    if let Some((project, paths)) = history {
                        crate::local_history::snapshot_paths(
                            &crate::local_history::base_dir(),
                            Some(&project),
                            &paths,
                            &format!(
                                "Before {} {}",
                                plan.provider.label(),
                                plan.operation.label()
                            ),
                            true,
                        );
                    }
                    let result = run(
                        &plan,
                        credentials,
                        &env,
                        &endpoint,
                        &worker_cancel,
                        &tx,
                        started,
                    );
                    let completed = match result {
                        Ok(completed) => completed,
                        Err(error) => Completed {
                            root: plan.root,
                            provider: plan.provider,
                            operation: plan.operation,
                            success: false,
                            cancelled: worker_cancel.load(Ordering::Relaxed),
                            exit: -1,
                            stdout: String::new(),
                            stderr: error.to_string(),
                            truncated: false,
                            elapsed: started.elapsed(),
                        },
                    };
                    let _ = tx.send(Event::Done(completed));
                });
            })?;
        Ok(Self {
            root,
            provider,
            operation,
            started,
            rx,
            cancel,
            finished: false,
            worker: Some(worker),
            pending: None,
        })
    }
    pub fn poll(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        let mut disconnected = false;
        for _ in 0..256 {
            let event = match self.rx.try_recv() {
                Ok(event) => event,
                Err(mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
            };
            match event {
                Event::Done(done) => self.pending = Some(done),
                output => events.push(output),
            }
        }
        if (self.pending.is_some() || disconnected)
            && self.worker.as_ref().is_some_and(JoinHandle::is_finished)
        {
            let joined = self.worker.take().unwrap().join();
            let done = self.pending.take().unwrap_or_else(|| Completed {
                root: self.root.clone(),
                provider: self.provider,
                operation: self.operation,
                success: false,
                cancelled: self.cancel.load(Ordering::Acquire),
                exit: -1,
                stdout: String::new(),
                stderr: if joined.is_err() {
                    "Provider worker panicked"
                } else {
                    "Provider worker returned without completion"
                }
                .into(),
                truncated: false,
                elapsed: self.started.elapsed(),
            });
            self.finished = true;
            events.push(Event::Done(done));
        }
        events
    }
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
    pub fn finished(&self) -> bool {
        self.finished
    }
}
impl Drop for ProviderJob {
    fn drop(&mut self) {
        if !self.finished {
            self.stop();
        }
        if let Some(worker) = self.worker.take() {
            RETIRING
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(Retiring {
                    label: format!(
                        "{} {} at {} ({:?})",
                        self.provider.label(),
                        self.operation.label(),
                        self.root.display(),
                        worker.thread().id()
                    ),
                    worker,
                });
        }
    }
}
struct AuthFile(PathBuf);
impl Drop for AuthFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn hg_auth(credentials: &Credentials, endpoint: &str) -> anyhow::Result<AuthFile> {
    let url = url::Url::parse(endpoint).map_err(|_| {
        anyhow::anyhow!(
            "Enter an explicit HTTPS Mercurial remote before supplying a password or token"
        )
    })?;
    anyhow::ensure!(url.scheme()=="https" && url.host_str().is_some() && url.password().is_none(),"Mercurial password/token authentication requires an explicit HTTPS remote without a password in its URL");
    for value in [&credentials.username, &credentials.password] {
        anyhow::ensure!(
            !value.contains(['\r', '\n', '\0']),
            "Credentials must contain a single line"
        );
    }
    let file = std::env::temp_dir().join(format!(
        "rustpy-vcs-auth-{}-{}.rc",
        std::process::id(),
        NEXT_AUTH.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut handle = options.open(&file)?;
    let guard = AuthFile(file);
    // Preserve normal user/system configuration; the session auth group wins.
    let mut text = String::new();
    if let Some(path) = std::env::var_os("HGRCPATH") {
        for path in std::env::split_paths(&path) {
            text.push_str(&format!("%include {}\n", path.display()));
        }
    } else {
        if cfg!(unix) {
            text.push_str("%include /etc/mercurial/hgrc\n%include /etc/mercurial/hgrc.d/*.rc\n");
        }
        if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            let home = PathBuf::from(home);
            for path in [
                home.join(".hgrc"),
                home.join(".config/hg/hgrc"),
                home.join("Mercurial.ini"),
            ] {
                text.push_str(&format!("%include {}\n", path.display()));
            }
        }
    }
    text.push_str(&format!("\n[auth]\nrustpy.prefix = {}\nrustpy.schemes = https\nrustpy.username = {}\nrustpy.password = {}\n",url.as_str(),credentials.username,credentials.password));
    handle.write_all(text.as_bytes())?;
    handle.flush()?;
    Ok(guard)
}
fn pump(
    mut stream: impl Read + Send + 'static,
    stderr: bool,
    tx: SyncSender<(bool, Vec<u8>)>,
    discard: Arc<AtomicBool>,
) -> io::Result<JoinHandle<io::Result<()>>> {
    std::thread::Builder::new()
        .name("vcs-provider-output".into())
        .spawn(move || {
            let mut bytes = [0; 8192];
            loop {
                match stream.read(&mut bytes) {
                    Ok(0) => return Ok(()),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error),
                    Ok(count) => {
                        if discard.load(Ordering::Acquire) {
                            continue;
                        }
                        let mut chunk = (stderr, bytes[..count].to_vec());
                        loop {
                            if discard.load(Ordering::Acquire) {
                                break;
                            }
                            match tx.try_send(chunk) {
                                Ok(()) => break,
                                Err(mpsc::TrySendError::Full(value)) => {
                                    chunk = value;
                                    std::thread::sleep(Duration::from_millis(2));
                                }
                                Err(mpsc::TrySendError::Disconnected(_)) => {
                                    discard.store(true, Ordering::Release);
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        })
}
struct RunningProcess {
    child: Child,
    tree: rustpy_core::process_tree::ProcessTree,
    readers: Vec<JoinHandle<io::Result<()>>>,
    input: Option<JoinHandle<io::Result<()>>>,
    discard: Arc<AtomicBool>,
}
impl RunningProcess {
    fn join_io(&mut self) -> io::Result<()> {
        let mut error = None;
        for thread in self.readers.drain(..).chain(self.input.take()) {
            match thread.join() {
                Ok(Ok(())) => {}
                Ok(Err(value)) => {
                    error.get_or_insert(value);
                }
                Err(_) => {
                    error.get_or_insert_with(|| io::Error::other("Provider pipe worker panicked"));
                }
            }
        }
        error.map_or(Ok(()), Err)
    }
}
impl Drop for RunningProcess {
    fn drop(&mut self) {
        // This guard lives only on the retained background worker. Even an IO
        // error/panic must stop the actual group and join every owned pipe.
        self.discard.store(true, Ordering::Release);
        self.tree.force(&mut self.child);
        let _ = self.join_io();
    }
}
fn run(
    plan: &CommandPlan,
    credentials: Credentials,
    env: &GitEnv,
    endpoint: &str,
    cancel: &AtomicBool,
    tx: &SyncSender<Event>,
    started: Instant,
) -> anyhow::Result<Completed> {
    if plan.provider == Provider::Tfs {
        anyhow::ensure!(credentials.password.is_empty(), "TFVC uses the vendor's existing authenticated credential cache; passwords in command arguments/stdin are unsupported");
        let authority = plan
            .tfs_authority
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("TFVC request has no captured workspace authority"))?;
        if !matches!(
            plan.operation,
            Operation::Initialize | Operation::MapWorkspace | Operation::Info
        ) {
            let (silent, receiver) = mpsc::sync_channel(1);
            drop(receiver); // preflight output is private until authority is established
            let result = run_process(
                &super::tfs::preflight(plan),
                Credentials::default(),
                env,
                endpoint,
                cancel,
                &silent,
                started,
            )?;
            anyhow::ensure!(
                result.success && !result.cancelled && !result.truncated,
                "Cannot establish the native TFVC workspace authority; no operation was executed"
            );
            super::tfs::validate_workspace(&result.stdout, authority)?;
        }
    }
    run_process(plan, credentials, env, endpoint, cancel, tx, started)
}
fn run_process(
    plan: &CommandPlan,
    credentials: Credentials,
    env: &GitEnv,
    endpoint: &str,
    cancel: &AtomicBool,
    tx: &SyncSender<Event>,
    started: Instant,
) -> anyhow::Result<Completed> {
    anyhow::ensure!(plan.provider != Provider::Tfs || credentials.password.is_empty(), "TFVC uses the vendor's existing authenticated credential cache; passwords in command arguments/stdin are unsupported");
    let mut command = Command::new(&plan.executable);
    command
        .current_dir(&plan.root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut arguments = plan.args.clone();
    // Preserve the existing Unix no-controlling-terminal/session semantics;
    // setsid also reserves the child's own PGID for the shared tree authority.
    #[cfg(unix)]
    crate::job::detach(&mut command);
    #[cfg(not(unix))]
    rustpy_core::process_tree::ProcessTree::prepare(&mut command);
    for (key, value) in &env.vars {
        command.env(key, value);
    }
    let mut input = plan.stdin.clone();
    let mut _auth = None;
    if !credentials.password.is_empty() {
        anyhow::ensure!(
            !credentials.password.contains(['\r', '\n', '\0']),
            "A password or token must contain a single line"
        );
        match plan.provider {
            Provider::Mercurial => {
                let auth = hg_auth(&credentials, endpoint)?;
                command.env("HGRCPATH", &auth.0);
                _auth = Some(auth);
            }
            Provider::Subversion => {
                anyhow::ensure!(
                    input.is_none(),
                    "SVN form input and password input cannot be combined"
                );
                arguments.insert(0, "--password-from-stdin".into());
                input = Some(format!("{}\n", credentials.password));
            }
            Provider::Perforce if plan.operation == Operation::Login => {
                input = Some(format!("{}\n", credentials.password))
            }
            Provider::Perforce => {
                command.env("P4PASSWD", &credentials.password);
            }
            Provider::Git => unreachable!(),
            Provider::Tfs => unreachable!(),
        }
    }
    command.args(arguments).stdin(if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    if cancel.load(Ordering::Relaxed) {
        anyhow::bail!("Operation cancelled before starting");
    }
    let mut child = command.spawn()?;
    let tree = match rustpy_core::process_tree::ProcessTree::new(&mut child) {
        Ok(tree) => tree,
        Err(error) => {
            crate::job::kill_hard(&mut child);
            let _ = child.wait();
            return Err(error.into());
        }
    };
    let mut owned = RunningProcess {
        child,
        tree,
        readers: Vec::new(),
        input: None,
        discard: Arc::new(AtomicBool::new(false)),
    };
    let (output_tx, output_rx) = mpsc::sync_channel(64);
    if let Some(stream) = owned.child.stdout.take() {
        owned.readers.push(pump(
            stream,
            false,
            output_tx.clone(),
            owned.discard.clone(),
        )?);
    }
    if let Some(stream) = owned.child.stderr.take() {
        owned.readers.push(pump(
            stream,
            true,
            output_tx.clone(),
            owned.discard.clone(),
        )?);
    }
    drop(output_tx);
    if let (Some(input), Some(mut stdin)) = (input, owned.child.stdin.take()) {
        // A provider that never reads a large form cannot prevent cancellation
        // polling. Killing its owned tree breaks the retained writer's pipe.
        owned.input = Some(
            std::thread::Builder::new()
                .name("vcs-provider-input".into())
                .spawn(move || stdin.write_all(input.as_bytes()))?,
        );
    }
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut truncated = false;
    let mut timed_out = false;
    let mut drain = |is_error: bool, bytes: Vec<u8>| {
        let output = if is_error { &mut stderr } else { &mut stdout };
        let available = OUTPUT_LIMIT.saturating_sub(output.len());
        if available < bytes.len() {
            truncated = true;
        }
        output.extend_from_slice(&bytes[..available.min(bytes.len())]);
        let _ = tx.try_send(Event::Output {
            stderr: is_error,
            text: String::from_utf8_lossy(&bytes).into_owned(),
        });
    };
    let mut status = None;
    let mut readers_done = false;
    let mut pipe_error = None;
    loop {
        let mut bytes = 0;
        for _ in 0..DRAIN_CHUNKS {
            if bytes >= DRAIN_BYTES {
                break;
            }
            match output_rx.try_recv() {
                Ok((stderr, output)) => {
                    bytes += output.len();
                    drain(stderr, output);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    readers_done = true;
                    break;
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        if cancel.load(Ordering::Relaxed) || started.elapsed() > plan.timeout {
            timed_out = started.elapsed() > plan.timeout;
            owned.tree.request_force();
        }
        if owned.input.as_ref().is_some_and(JoinHandle::is_finished) {
            match owned.input.take().unwrap().join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    pipe_error.get_or_insert(error.to_string());
                    owned.tree.request_force();
                }
                Err(_) => {
                    pipe_error.get_or_insert("Provider input worker panicked".into());
                    owned.tree.request_force();
                }
            }
        }
        if status.is_none() {
            match owned.tree.poll(&mut owned.child) {
                Ok(value) => status = value,
                Err(error) => {
                    pipe_error.get_or_insert(error.to_string());
                    owned.tree.request_force();
                }
            }
        }
        // No deadline substitutes for actual pipe EOF/reader retirement. In
        // particular an escaped setsid pipe holder stays visibly owned here;
        // the shared PGID authority does not pretend it can kill that helper.
        if status.is_some() && readers_done {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(drain);
    if let Err(error) = owned.join_io() {
        pipe_error.get_or_insert(error.to_string());
    }
    let status = status.expect("owned process was physically reaped");
    if let Some(error) = &pipe_error {
        stderr.extend_from_slice(format!("\nProvider IO failed: {error}\n").as_bytes());
    }
    if timed_out {
        stderr.extend_from_slice(
            b"\nProvider operation timed out; inspect its output before retrying.\n",
        );
    }
    let exit = status.code().unwrap_or(-1);
    let cancelled = cancel.load(Ordering::Relaxed);
    Ok(Completed {
        root: plan.root.clone(),
        provider: plan.provider,
        operation: plan.operation,
        success: plan.success_codes.contains(&exit)
            && !cancelled
            && !timed_out
            && pipe_error.is_none(),
        cancelled,
        exit,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        truncated,
        elapsed: started.elapsed(),
    })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::path::Path;
    const FIXTURE: &str = "other_vcs::runner::tests::owned_process_fixture";

    // Genuine OS children exercise ownership only; these do not impersonate an
    // Hg/SVN/P4 protocol or count as provider-operation acceptance.
    #[test]
    fn owned_process_fixture() {
        let Ok(mode) = std::env::var("RUSTPY_PROVIDER_OWNER_FIXTURE") else {
            return;
        };
        let marker = PathBuf::from(std::env::var_os("RUSTPY_PROVIDER_OWNER_MARKER").unwrap());
        if mode == "leader" {
            let mut helper = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", FIXTURE, "--nocapture"])
                .env("RUSTPY_PROVIDER_OWNER_FIXTURE", "helper")
                .stdin(Stdio::null())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !marker.exists() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(
                helper.try_wait().unwrap().is_none(),
                "pipe holder must still be alive when its leader exits"
            );
            std::io::stdout()
                .write_all(b"leader-final-output\n")
                .unwrap();
            std::io::stdout().flush().unwrap();
            std::process::exit(23);
        }
        if mode == "helper" {
            std::io::stdout().write_all(b"helper-ready\n").unwrap();
            std::io::stdout().flush().unwrap();
        }
        let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
        let start = stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .nth(19)
            .unwrap();
        std::fs::write(marker, format!("{} {start}", std::process::id())).unwrap();
        if mode == "flood" {
            let bytes = [b'x'; 8192];
            loop {
                if std::io::stdout().write_all(&bytes).is_err() {
                    break;
                }
                if std::io::stderr().write_all(&bytes).is_err() {
                    break;
                }
            }
        } else {
            std::thread::sleep(Duration::from_secs(30));
        }
    }

    fn start(root: &Path, mode: &str) -> ProviderJob {
        let plan = CommandPlan {
            tfs_authority: None,
            root: root.to_path_buf(),
            provider: Provider::Mercurial,
            operation: Operation::Status,
            executable: std::env::current_exe().unwrap().to_str().unwrap().into(),
            args: vec!["--exact".into(), FIXTURE.into(), "--nocapture".into()],
            stdin: None,
            format: super::super::Format::Text,
            timeout: Duration::from_secs(30),
            success_codes: vec![23],
        };
        ProviderJob::start(
            plan,
            Credentials::default(),
            GitEnv {
                vars: vec![
                    ("RUSTPY_PROVIDER_OWNER_FIXTURE".into(), mode.into()),
                    (
                        "RUSTPY_PROVIDER_OWNER_MARKER".into(),
                        root.join("child.identity").to_str().unwrap().into(),
                    ),
                ],
            },
            "",
        )
        .unwrap()
    }
    struct Owned(ProviderJob);
    impl Drop for Owned {
        fn drop(&mut self) {
            self.0.stop();
            while !self.0.finished() {
                self.0.poll();
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    fn completed(job: &mut ProviderJob) -> Completed {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            for event in job.poll() {
                if let Event::Done(done) = event {
                    return done;
                }
            }
            assert!(
                Instant::now() < deadline,
                "owned provider did not physically retire"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn physically_running(root: &Path) -> bool {
        let identity = std::fs::read_to_string(root.join("child.identity")).unwrap();
        let mut identity = identity.split_whitespace();
        let pid = identity.next().unwrap();
        let start = identity.next().unwrap();
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        let fields: Vec<_> = stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .collect();
        fields[19] == start && !matches!(fields[0], "Z" | "X")
    }
    #[test]
    fn leader_exit_waits_for_pipe_holding_group_helper_and_preserves_status_and_tail() {
        let root = tempfile::tempdir().unwrap();
        let mut owner = Owned(start(root.path(), "leader"));
        let done = completed(&mut owner.0);
        assert!(done.success && !done.cancelled && !done.truncated);
        assert_eq!(done.exit, 23);
        assert!(done.stdout.contains("helper-ready\n"));
        assert!(done.stdout.contains("leader-final-output\n"));
        assert!(
            !physically_running(root.path()),
            "Done must not leave an executing pipe holder"
        );
        assert!(owner.0.finished() && owner.0.worker.is_none());
        assert!(crate::running_operation(root.path()).is_none());
    }
    #[test]
    fn sustained_stdout_and_stderr_do_not_starve_stop_or_retirement() {
        let root = tempfile::tempdir().unwrap();
        let mut owner = Owned(start(root.path(), "flood"));
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut output = 0;
        while output < 256 * 1024 || !root.path().join("child.identity").exists() {
            for event in owner.0.poll() {
                if let Event::Output { text, .. } = event {
                    output += text.len();
                }
            }
            assert!(
                Instant::now() < deadline,
                "actual continuous writer was not ready"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        owner.0.stop();
        let done = completed(&mut owner.0);
        assert!(done.cancelled && !done.success);
        assert!(!done.stdout.is_empty() && !done.stderr.is_empty());
        assert!(!physically_running(root.path()));
        assert!(owner.0.worker.is_none());
        assert!(crate::running_operation(root.path()).is_none());
    }
    #[test]
    fn dropping_an_active_job_retains_its_worker_until_physical_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let mut job = start(root.path(), "flood");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !root.path().join("child.identity").exists() {
            job.poll();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(job);
        loop {
            let labels = poll_retiring();
            if !labels
                .iter()
                .any(|label| label.contains(root.path().to_str().unwrap()))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "dropped provider owner did not retire"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!physically_running(root.path()));
        assert!(crate::running_operation(root.path()).is_none());
    }
}
