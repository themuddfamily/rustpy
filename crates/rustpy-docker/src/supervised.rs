//! Bounded Docker collection. TaskManager retains the worker; every command
//! owns its process tree and both reader joins until reaping and EOF complete.
use super::{parse_compose_ls, parse_containers, parse_images, Snapshot};
use anyhow::{bail, Context};
use rustpy_core::{CancellationToken, ProgressReporter, TaskError, TaskHandle, TaskManager};
use rustpy_run::process_owner::ProcessOwner;
use rustpy_settings::trust::DirectoryIdentity;
use std::{
    collections::hash_map::DefaultHasher,
    ffi::OsString,
    hash::{Hash, Hasher},
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
pub const STDOUT_LIMIT: usize = 8 * 1024 * 1024;
pub const STDERR_LIMIT: usize = 128 * 1024;
/// No Debug implementation: the captured environment can contain credentials.
#[derive(Clone)]
pub struct Capture {
    program: String,
    cwd: PathBuf,
    identity: DirectoryIdentity,
    environment: Vec<(OsString, OsString)>,
    fingerprint: u64,
}
impl Capture {
    pub fn current(program: impl Into<String>, cwd: &Path) -> anyhow::Result<Self> {
        let program = program.into();
        anyhow::ensure!(
            !program.trim().is_empty() && !program.chars().any(char::is_control),
            "Choose a Docker CLI executable"
        );
        let cwd = dunce::canonicalize(cwd).context("Docker working directory is unavailable")?;
        let identity = DirectoryIdentity::capture(&cwd)?;
        let mut environment = std::env::vars_os().collect::<Vec<_>>();
        environment.sort_by(|a, b| a.0.cmp(&b.0));
        anyhow::ensure!(
            environment.len() <= 4096
                && environment
                    .iter()
                    .map(|(key, value)| key.as_encoded_bytes().len()
                        + value.as_encoded_bytes().len())
                    .sum::<usize>()
                    <= 1024 * 1024,
            "Docker environment exceeds the capture limit"
        );
        let mut hash = DefaultHasher::new();
        program.hash(&mut hash);
        cwd.hash(&mut hash);
        environment.hash(&mut hash);
        format!("{identity:?}").hash(&mut hash);
        Ok(Self {
            program,
            cwd,
            identity,
            environment,
            fingerprint: hash.finish(),
        })
    }
    pub fn program(&self) -> &str {
        &self.program
    }
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }
    pub fn identity(&self) -> &DirectoryIdentity {
        &self.identity
    }
    pub fn stamp(&self) -> String {
        format!(
            "{}:{}:{:016x}",
            self.program,
            self.cwd.display(),
            self.fingerprint
        )
    }
    fn check(&self, cancel: &CancellationToken) -> anyhow::Result<()> {
        cancel.check()?;
        anyhow::ensure!(
            self.identity.matches(&self.cwd),
            "Docker project directory changed; refresh admission"
        );
        Ok(())
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(args)
            .current_dir(&self.cwd)
            .env_clear()
            .envs(self.environment.iter().cloned())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        ProcessOwner::prepare(&mut command);
        command
    }
}
#[derive(Default)]
struct Phase {
    step: usize,
    label: String,
    process: String,
    cancelling: bool,
    reaped: bool,
    stdout_done: bool,
    stderr_done: bool,
}
type PhaseState = Arc<Mutex<Phase>>;
fn phase(state: &PhaseState) -> std::sync::MutexGuard<'_, Phase> {
    state.lock().unwrap_or_else(|error| error.into_inner())
}
struct Output {
    bytes: Vec<u8>,
    error: Option<String>,
}
fn collect(mut input: impl Read, maximum: usize, overflow: &AtomicBool) -> Output {
    let mut bytes = Vec::with_capacity(maximum.min(65536));
    let mut chunk = [0; 16384];
    loop {
        match input.read(&mut chunk) {
            Ok(0) => return Output { bytes, error: None },
            Ok(count) => {
                let retained = count.min(maximum.saturating_sub(bytes.len()));
                bytes.extend_from_slice(&chunk[..retained]);
                if retained < count {
                    overflow.store(true, Ordering::Release);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => {
                return Output {
                    bytes,
                    error: Some(error.to_string()),
                };
            }
        }
    }
}
fn reader(
    input: impl Read + Send + 'static,
    maximum: usize,
    overflow: Arc<AtomicBool>,
    name: &str,
) -> std::io::Result<JoinHandle<Output>> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || collect(input, maximum, &overflow))
}
struct OwnedCommand {
    child: Child,
    owner: ProcessOwner,
    status: Option<ExitStatus>,
    out: Option<JoinHandle<Output>>,
    err: Option<JoinHandle<Output>>,
    state: PhaseState,
}
impl OwnedCommand {
    fn update(&self) {
        let mut state = phase(&self.state);
        state.process = self.owner.stamp();
        state.reaped = self.status.is_some();
        state.stdout_done = self.out.as_ref().is_none_or(JoinHandle::is_finished);
        state.stderr_done = self.err.as_ref().is_none_or(JoinHandle::is_finished);
    }
    fn stop(&mut self) {
        phase(&self.state).cancelling = true;
        self.owner.stop(&mut self.child);
    }
    fn poll(&mut self) -> std::io::Result<()> {
        if self.status.is_none() {
            self.status = self.owner.poll(&mut self.child)?;
        }
        self.update();
        Ok(())
    }
    fn drained(&self) -> bool {
        self.status.is_some()
            && self.out.as_ref().is_none_or(JoinHandle::is_finished)
            && self.err.as_ref().is_none_or(JoinHandle::is_finished)
    }
    fn take_output(&mut self) -> anyhow::Result<(Output, Output)> {
        let out = self
            .out
            .take()
            .ok_or_else(|| anyhow::anyhow!("Docker stdout reader missing"))?
            .join()
            .map_err(|_| anyhow::anyhow!("Docker stdout reader failed"))?;
        let err = self
            .err
            .take()
            .ok_or_else(|| anyhow::anyhow!("Docker stderr reader missing"))?
            .join()
            .map_err(|_| anyhow::anyhow!("Docker stderr reader failed"))?;
        Ok((out, err))
    }
}
impl Drop for OwnedCommand {
    fn drop(&mut self) {
        if !self.drained() {
            self.stop();
            while !self.drained() {
                if let Err(error) = self.poll() {
                    phase(&self.state).label = format!("Docker cleanup pending: {error}");
                    self.owner.stop(&mut self.child);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        if let Some(reader) = self.out.take() {
            let _ = reader.join();
        }
        if let Some(reader) = self.err.take() {
            let _ = reader.join();
        }
        self.update();
    }
}
fn run(
    capture: &Capture,
    args: &[&str],
    timeout: Duration,
    cancel: &CancellationToken,
    state: &PhaseState,
) -> anyhow::Result<String> {
    capture.check(cancel)?;
    let mut command = capture.command(args);
    capture.check(cancel)?;
    let mut child = command
        .spawn()
        .with_context(|| format!("cannot run {}", capture.program))?;
    let owner = match ProcessOwner::attach(&mut child) {
        Ok(owner) => owner,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error).context("Cannot attach Docker process ownership");
        }
    };
    let mut process = OwnedCommand {
        child,
        owner,
        status: None,
        out: None,
        err: None,
        state: state.clone(),
    };
    process.update();
    let overflow = Arc::new(AtomicBool::new(false));
    let out = process
        .child
        .stdout
        .take()
        .context("Docker stdout missing")?;
    let err = process
        .child
        .stderr
        .take()
        .context("Docker stderr missing")?;
    process.out = Some(reader(
        out,
        STDOUT_LIMIT,
        overflow.clone(),
        "docker-stdout",
    )?);
    process.err = Some(reader(
        err,
        STDERR_LIMIT,
        overflow.clone(),
        "docker-stderr",
    )?);
    let started = Instant::now();
    let mut failure = None;
    loop {
        if failure.is_none() {
            failure = if cancel.is_cancelled() {
                Some("Docker snapshot cancelled".to_owned())
            } else if !capture.identity.matches(&capture.cwd) {
                Some("Docker project directory changed".into())
            } else if started.elapsed() >= timeout {
                Some(format!("{} timed out", capture.program))
            } else if overflow.load(Ordering::Acquire) {
                Some("Docker output exceeded its bounded capture limit".into())
            } else {
                None
            };
        }
        if failure.is_some() {
            process.stop();
        }
        if let Err(error) = process.poll() {
            failure.get_or_insert_with(|| format!("Docker process cleanup: {error}"));
            process.stop();
        }
        if process.drained() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let status = process.status.context("Docker child was not reaped")?;
    let (out, err) = process.take_output()?;
    capture.check(cancel)?;
    if overflow.load(Ordering::Acquire) {
        bail!("Docker output exceeded its bounded capture limit");
    }
    if let Some(error) = failure {
        bail!("{error}");
    }
    if let Some(error) = out.error.or(err.error) {
        bail!("Cannot collect Docker output: {error}");
    }
    let out = String::from_utf8(out.bytes).context("Docker stdout is not UTF-8")?;
    let err = String::from_utf8_lossy(&err.bytes);
    if !status.success() {
        let message = if err.trim().is_empty() {
            out.trim()
        } else {
            err.trim()
        };
        let detail = message.chars().take(4096).collect::<String>();
        bail!("Docker command exited with {status}:\n{detail}");
    }
    Ok(out)
}
fn snapshot(
    capture: &Capture,
    cancel: &CancellationToken,
    state: &PhaseState,
    reporter: Option<&ProgressReporter>,
) -> anyhow::Result<Snapshot> {
    let step = |index, label: &str, args: &[&str]| -> anyhow::Result<String> {
        capture.check(cancel)?;
        {
            let mut state = phase(state);
            *state = Phase {
                step: index,
                label: label.into(),
                ..Default::default()
            };
        }
        if let Some(reporter) = reporter {
            reporter.set_text(label);
            reporter.set_step(index, 4);
        }
        run(capture, args, Duration::from_secs(10), cancel, state)
    };
    let server_version = step(
        0,
        "Docker server version",
        &["version", "--format", "{{.Server.Version}}"],
    )?;
    anyhow::ensure!(
        server_version.trim().len() <= 128,
        "Docker server version exceeds its limit"
    );
    let containers = parse_containers(&step(
        1,
        "Docker containers",
        &["ps", "-a", "--no-trunc", "--format", "{{json .}}"],
    )?);
    let images = step(2, "Docker images", &["images", "--format", "{{json .}}"]);
    capture.check(cancel)?;
    let mut warnings = Vec::new();
    if let Err(error) = &images {
        warnings.push(format!("Images could not be collected: {error:#}"));
    }
    let images = parse_images(&images.unwrap_or_default());
    let compose = step(
        3,
        "Docker Compose projects",
        &["compose", "ls", "--all", "--format", "json"],
    );
    capture.check(cancel)?;
    if let Err(error) = &compose {
        warnings.push(format!(
            "Compose projects could not be collected: {error:#}"
        ));
    }
    let compose = parse_compose_ls(&compose.unwrap_or_default());
    if let Some(reporter) = reporter {
        reporter.set_step(4, 4);
    }
    phase(state).label = "Docker snapshot reaped and drained".into();
    Ok(Snapshot {
        server_version: server_version.trim().into(),
        containers,
        images,
        compose,
        warnings,
    })
}
/// Retained Services owner. Dropping requests cancellation; TaskManager still
/// owns the worker/token until all owned commands/readers have actually drained.
pub struct SnapshotTask {
    task: TaskHandle<Snapshot>,
    capture: Capture,
    state: PhaseState,
    result_taken: bool,
}
impl SnapshotTask {
    pub fn start(capture: Capture) -> Self {
        let state = PhaseState::default();
        let worker_state = state.clone();
        let captured = capture.clone();
        let task = TaskManager::global()
            .task(format!("Docker snapshot · {}", capture.cwd.display()))
            .spawn(move |reporter| {
                let result = snapshot(&captured, reporter.token(), &worker_state, Some(reporter));
                if reporter.is_cancelled() {
                    Err(TaskError::Cancelled)
                } else {
                    result.map_err(|error| TaskError::Failed(format!("{error:#}")))
                }
            });
        Self {
            task,
            capture,
            state,
            result_taken: false,
        }
    }
    pub fn id(&self) -> u64 {
        self.task.id()
    }
    pub fn cancel(&self) {
        self.task.cancel();
    }
    pub fn is_cancelled(&self) -> bool {
        self.task.is_cancelled()
    }
    pub fn capture(&self) -> &Capture {
        &self.capture
    }
    pub fn label(&self) -> String {
        format!("Docker snapshot · {}", self.capture.cwd.display())
    }
    pub fn stamp(&self) -> String {
        format!("docker-snapshot:{}:{}", self.id(), self.capture.stamp())
    }
    pub fn drain_status(&self) -> String {
        let state = phase(&self.state);
        format!(
            "{}:{}:step={}:{}:{}:cancel={}:reaped={}:pipes={}/{}",
            self.id(),
            self.capture.stamp(),
            state.step,
            state.label,
            state.process,
            self.is_cancelled() || state.cancelling,
            state.reaped,
            state.stdout_done,
            state.stderr_done
        )
    }
    pub fn try_result(&mut self) -> Option<Result<Snapshot, String>> {
        let result = self
            .task
            .try_result()
            .map(|result| result.map_err(|error| format!("{error:?}")));
        if result.is_some() {
            self.result_taken = true;
        }
        result
    }
}
impl Drop for SnapshotTask {
    fn drop(&mut self) {
        if !self.result_taken {
            self.cancel();
        }
    }
}
pub(super) fn run_compat(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> anyhow::Result<String> {
    let capture = Capture::current(program, &std::env::current_dir()?)?;
    run(
        &capture,
        args,
        timeout,
        &CancellationToken::new(),
        &PhaseState::default(),
    )
}
pub(super) fn snapshot_compat(program: &str) -> anyhow::Result<Snapshot> {
    let capture = Capture::current(program, &std::env::current_dir()?)?;
    snapshot(
        &capture,
        &CancellationToken::new(),
        &PhaseState::default(),
        None,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    // Functional regression fixtures are deliberately not run during feature
    // implementation. They use this test binary rather than an installed CLI.
    const FIXTURE: &str = "RUSTPY_DOCKER_OWNERSHIP_FIXTURE";
    const FIXTURE_ARGS: &[&str] = &[
        "--exact",
        "supervised::tests::process_fixture",
        "--nocapture",
    ];
    fn fixture_capture(role: &str) -> Capture {
        let exe = std::env::current_exe().unwrap();
        let mut capture =
            Capture::current(exe.to_string_lossy(), &std::env::current_dir().unwrap()).unwrap();
        capture.environment.retain(|(key, _)| key != FIXTURE);
        capture.environment.push((FIXTURE.into(), role.into()));
        capture
    }
    #[test]
    fn process_fixture() {
        let Ok(role) = std::env::var(FIXTURE) else {
            return;
        };
        use std::io::Write;
        match role.as_str() {
            "failure" => {
                eprintln!(
                    "Traceback (most recent call last):\n  File provider.py, line 2\nValueError: actual provider failure\n{}",
                    "x".repeat(5000)
                );
                std::process::exit(7);
            }
            "flood" => {
                let writer = std::thread::spawn(|| {
                    let mut stdout = std::io::stdout().lock();
                    for _ in 0..32 {
                        stdout.write_all(&[b'a'; 8192]).unwrap();
                    }
                    stdout.flush().unwrap();
                });
                let mut stderr = std::io::stderr().lock();
                for _ in 0..9 {
                    stderr.write_all(&[b'e'; 8192]).unwrap();
                }
                stderr.flush().unwrap();
                writer.join().unwrap();
            }
            "hold" => {
                let _child = Command::new(std::env::current_exe().unwrap())
                    .args(FIXTURE_ARGS)
                    .env(FIXTURE, "grandchild")
                    .stdin(Stdio::null())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .unwrap();
                println!("descendant spawned");
                std::io::stdout().flush().unwrap();
                std::process::exit(0);
            }
            "grandchild" | "sleep" => {
                println!("fixture waiting");
                std::io::stdout().flush().unwrap();
                std::thread::sleep(Duration::from_secs(30));
            }
            _ => panic!("unknown process fixture"),
        }
    }
    #[test]
    fn simultaneous_pipe_pressure_does_not_deadlock() {
        let capture = fixture_capture("flood");
        let output = run(
            &capture,
            FIXTURE_ARGS,
            Duration::from_secs(5),
            &CancellationToken::new(),
            &PhaseState::default(),
        )
        .unwrap();
        assert!(output.bytes().filter(|byte| *byte == b'a').count() >= 32 * 8192);
    }
    #[test]
    fn timeout_returns_only_after_reaping_and_both_reader_drains() {
        let capture = fixture_capture("sleep");
        let state = PhaseState::default();
        let started = Instant::now();
        let error = run(
            &capture,
            FIXTURE_ARGS,
            Duration::from_millis(100),
            &CancellationToken::new(),
            &state,
        )
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        let state = phase(&state);
        assert!(state.reaped && state.stdout_done && state.stderr_done);
        assert!(started.elapsed() < Duration::from_secs(5));
    }
    #[test]
    fn leader_exit_cleans_descendants_before_pipe_retirement() {
        let capture = fixture_capture("hold");
        let started = Instant::now();
        let output = run(
            &capture,
            FIXTURE_ARGS,
            Duration::from_secs(3),
            &CancellationToken::new(),
            &PhaseState::default(),
        )
        .unwrap();
        assert!(output.contains("descendant spawned"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
    #[test]
    fn concurrent_capture_buffers_are_bounded_without_losing_overflow() {
        let overflow = AtomicBool::new(false);
        let output = collect(&b"0123456789abcdef"[..], 4, &overflow);
        assert_eq!(output.bytes, b"0123");
        assert!(overflow.load(Ordering::Acquire));
        assert!(output.error.is_none());
    }
    #[test]
    fn failed_provider_retains_bounded_multiline_cause_and_exit_status() {
        let error = run(
            &fixture_capture("failure"),
            FIXTURE_ARGS,
            Duration::from_secs(3),
            &CancellationToken::new(),
            &PhaseState::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("7") && error.contains("ValueError: actual provider failure"),
            "{error}"
        );
        assert!(error.contains("\n  File provider.py, line 2\n"));
        assert!(error.len() < 4200, "provider failure stays bounded");
    }
    #[test]
    fn independent_streams_keep_stderr_even_with_large_stdout() {
        let out_overflow = AtomicBool::new(false);
        let err_overflow = AtomicBool::new(false);
        let out = collect(&b"huge stdout"[..], 4, &out_overflow);
        let err = collect(&b"daemon denied"[..], 128, &err_overflow);
        assert_eq!(out.bytes, b"huge");
        assert_eq!(err.bytes, b"daemon denied");
    }
    #[test]
    fn cancelled_capture_never_reaches_command_spawn() {
        let root = std::env::current_dir().unwrap();
        let capture = Capture::current("never-spawn-this-docker", &root).unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = run(
            &capture,
            &["version"],
            Duration::ZERO,
            &cancel,
            &PhaseState::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
    }
}
