//! Immutable formatting candidates. External commands are bounded and owned.
//!
//! External commands run in an owned Unix process group or Windows Job Object.
//! Output, input and run time have explicit limits. At most four invocations can
//! own pipe workers concurrently; excess calls return [`CandidateError::Busy`].
//! On Windows, if synchronous pipe cancellation cannot finish within two seconds,
//! cleanup returns an I/O timeout after terminating the Job. Such a worker retains
//! its invocation permit until it exits, so failed cleanup cannot accumulate an
//! unbounded number of detached workers. No live document is mutated by this API.
//! Project trust must be checked by the caller before selecting an external tool.
use crate::{FormatError, FormatOptions};
use rustpy_core::cancel::CancellationToken;
use rustpy_syntax::{TextRange, TextSize};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};

pub enum Formatter<'a> {
    Builtin {
        range: TextRange,
        options: &'a FormatOptions,
    },
    External(ExternalFormatter<'a>),
}
pub struct ExternalFormatter<'a> {
    pub python: &'a Path,
    pub formatter: &'a str,
    pub extra_args: &'a str,
    pub cwd: Option<&'a Path>,
    pub env: &'a HashMap<String, String>,
    pub limits: FormatterLimits,
}
#[derive(Clone, Copy, Debug)]
pub struct FormatterLimits {
    pub max_input_bytes: usize,
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
    pub timeout: Duration,
}
impl Default for FormatterLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 16 * 1024 * 1024,
            max_stdout_bytes: 16 * 1024 * 1024,
            max_stderr_bytes: 1024 * 1024,
            timeout: Duration::from_secs(30),
        }
    }
}
#[derive(Debug)]
pub struct FormatCandidate {
    pub text: String,
    pub edits: Vec<(TextRange, String)>,
}
#[derive(Debug, thiserror::Error)]
pub enum CandidateError {
    #[error(transparent)]
    Builtin(#[from] FormatError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("invalid formatter configuration: {0}")]
    Configuration(String),
    #[error("formatting cancelled")]
    Cancelled,
    #[error("formatter timed out")]
    TimedOut,
    #[error("formatter process capacity is busy")]
    Busy,
    #[error("formatter input exceeds its byte limit")]
    InputTooLarge,
    #[error("formatter {0} exceeds its byte limit")]
    OutputTooLarge(&'static str),
    #[error("formatter output is not UTF-8: {0}")]
    InvalidUtf8(#[from] std::string::FromUtf8Error),
    #[error("formatter returned empty output for nonempty input")]
    EmptyOutput,
    #[error("formatter failed ({status}): {stderr}")]
    Failed {
        status: std::process::ExitStatus,
        stderr: String,
    },
}

pub fn format_candidate(
    text: &str,
    formatter: Formatter<'_>,
    token: &CancellationToken,
) -> Result<FormatCandidate, CandidateError> {
    if token.is_cancelled() {
        return Err(CandidateError::Cancelled);
    }
    match formatter {
        Formatter::Builtin { range, options } => {
            let edits = crate::format_range(text, range, options)?;
            let mut result = text.to_owned();
            for (range, replacement) in edits.iter().rev() {
                result.replace_range(
                    usize::from(range.start())..usize::from(range.end()),
                    replacement,
                );
            }
            if token.is_cancelled() {
                return Err(CandidateError::Cancelled);
            }
            Ok(FormatCandidate {
                text: result,
                edits,
            })
        }
        Formatter::External(config) => {
            let output = run(text, &config, token)?;
            let output = String::from_utf8(output)?;
            if !text.is_empty() && output.is_empty() {
                return Err(CandidateError::EmptyOutput);
            }
            let result = rustpy_text::LineEnding::normalize_to_lf(&output).into_owned();
            let edits = if result == text {
                Vec::new()
            } else {
                vec![(
                    TextRange::new(TextSize::from(0), TextSize::of(text)),
                    result.clone(),
                )]
            };
            if token.is_cancelled() {
                return Err(CandidateError::Cancelled);
            }
            Ok(FormatCandidate {
                text: result,
                edits,
            })
        }
    }
}

const POLL: Duration = Duration::from_millis(15);

// Workers retain their invocation permit until their pipe handles are closed.
// Even an OS-level synchronous-I/O cancellation failure cannot accumulate
// unbounded detached workers across subsequent formatter invocations.
struct InvocationLimiter(AtomicUsize);
static INVOCATIONS: InvocationLimiter = InvocationLimiter(AtomicUsize::new(0));
struct InvocationLease(&'static InvocationLimiter);
impl InvocationLimiter {
    fn acquire(&'static self) -> Result<Arc<InvocationLease>, CandidateError> {
        let mut count = self.0.load(Ordering::Acquire);
        loop {
            if count >= 4 {
                return Err(CandidateError::Busy);
            }
            match self.0.compare_exchange_weak(
                count,
                count + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => count = current,
            }
        }
        Ok(Arc::new(InvocationLease(self)))
    }
}
impl Drop for InvocationLease {
    fn drop(&mut self) {
        self.0 .0.fetch_sub(1, Ordering::AcqRel);
    }
}

struct OwnedChild {
    child: std::process::Child,
    owner: Option<platform::Owner>,
    stop: Arc<AtomicBool>,
}
impl std::ops::Deref for OwnedChild {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.child
    }
}
impl std::ops::DerefMut for OwnedChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.child
    }
}
impl OwnedChild {
    fn stop(&mut self) -> io::Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(owner) = self.owner.take() {
            match owner.terminate() {
                Ok(()) => {
                    rustpy_core::process::terminate_owned(&mut self.child, Duration::ZERO);
                }
                Err(error) => {
                    // A kernel-blocked process may not complete SIGKILL. Do
                    // not turn a cleanup timeout into an unbounded wait(), or
                    // retry this consumed owner when Drop runs.
                    let _ = self.child.kill();
                    let _ = self.child.try_wait();
                    return Err(error);
                }
            }
        }
        Ok(())
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(unix)]
fn ready(pipe: &impl std::os::fd::AsRawFd, write: bool, stop: &AtomicBool) -> io::Result<bool> {
    while !stop.load(Ordering::Acquire) {
        let mut p = libc::pollfd {
            fd: pipe.as_raw_fd(),
            events: if write { libc::POLLOUT } else { libc::POLLIN },
            revents: 0,
        };
        // The borrowed pipe owns this descriptor throughout the poll.
        let n = unsafe { libc::poll(&mut p, 1, 15) };
        if n > 0 {
            return Ok(true);
        }
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }
    Ok(false)
}

fn run(
    text: &str,
    config: &ExternalFormatter<'_>,
    token: &CancellationToken,
) -> Result<Vec<u8>, CandidateError> {
    let mut command = rustpy_core::interpreter_launch::interpreter_command(config.python);
    command.args(["-m", config.formatter]);
    command.args(formatter_arguments(config.formatter, None)?);
    command.args(parse_arguments(config.extra_args)?);
    command.envs(config.env);
    if let Some(cwd) = config.cwd {
        command.current_dir(cwd);
    }
    run_command(text, command, config.limits, false, token)
}

/// Executes an argv command with the same owned process, pipe, timeout and
/// cancellation policy as formatting. Ruff check may return 1 for diagnostics.
pub fn external_command_output(
    text: &str,
    command: Command,
    limits: FormatterLimits,
    diagnostic_exit: bool,
    token: &CancellationToken,
) -> Result<Vec<u8>, CandidateError> {
    run_command(text, command, limits, diagnostic_exit, token)
}

fn run_command(
    text: &str,
    mut command: Command,
    limits: FormatterLimits,
    diagnostic_exit: bool,
    token: &CancellationToken,
) -> Result<Vec<u8>, CandidateError> {
    if token.is_cancelled() {
        return Err(CandidateError::Cancelled);
    }
    if text.len() > limits.max_input_bytes {
        return Err(CandidateError::InputTooLarge);
    }
    let lease = INVOCATIONS.acquire()?;
    let deadline = Instant::now()
        .checked_add(limits.timeout)
        .unwrap_or_else(Instant::now);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    platform::prepare(&mut command);
    let mut child = command.spawn()?;
    let owner = match platform::Owner::new(&mut child) {
        Ok(owner) => owner,
        Err(error) => {
            rustpy_core::process::terminate_owned(&mut child, Duration::ZERO);
            return Err(error.into());
        }
    };
    let stop = Arc::new(AtomicBool::new(false));
    let mut child = OwnedChild {
        child,
        owner: Some(owner),
        stop: stop.clone(),
    };
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (out_tx, out_rx) = mpsc::sync_channel(1);
    let (err_tx, err_rx) = mpsc::sync_channel(1);
    let (in_tx, in_rx) = mpsc::sync_channel(1);
    let out_stop = stop.clone();
    let err_stop = stop.clone();
    let in_stop = stop.clone();
    let (out_lease, err_lease, in_lease) = (lease.clone(), lease.clone(), lease.clone());
    let out_limit = limits.max_stdout_bytes;
    let err_limit = limits.max_stderr_bytes;
    let input_bytes = text.as_bytes().to_vec();
    let tasks: Vec<Box<dyn FnOnce() + Send>> = vec![
        Box::new(move || {
            let _lease = out_lease;
            let _ = out_tx.send(drain(stdout, out_limit, "stdout", &out_stop));
        }),
        Box::new(move || {
            let _lease = err_lease;
            let _ = err_tx.send(drain(stderr, err_limit, "stderr", &err_stop));
        }),
        Box::new(move || {
            let _lease = in_lease;
            let _ = in_tx.send(feed(stdin, &input_bytes, &in_stop));
        }),
    ];
    let mut workers = Vec::new();
    for task in tasks {
        match std::thread::Builder::new().spawn(task) {
            Ok(worker) => workers.push(worker),
            Err(error) => {
                let retired = child.stop();
                let drained = platform::finish_workers(workers);
                retired?;
                drained?;
                return Err(error.into());
            }
        }
    }
    let (mut out, mut err, mut input, mut status) = (None, None, None, None);
    let result = (|| loop {
        if token.is_cancelled() {
            return Err(CandidateError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(CandidateError::TimedOut);
        }
        if out.is_none() {
            if let Ok(value) = out_rx.try_recv() {
                out = Some(value?);
            }
        }
        if err.is_none() {
            if let Ok(value) = err_rx.try_recv() {
                err = Some(value?);
            }
        }
        if input.is_none() {
            if let Ok(value) = in_rx.try_recv() {
                value?;
                input = Some(());
            }
        }
        if status.is_none() {
            status = platform::poll_status(&mut child)?;
        }
        if let (Some(status), Some(_), Some(_), Some(())) = (status, &out, &err, input) {
            if !status.success() && !(diagnostic_exit && status.code() == Some(1)) {
                return Err(CandidateError::Failed {
                    status,
                    stderr: String::from_utf8_lossy(err.as_ref().unwrap()).into_owned(),
                });
            }
            return Ok(out.take().unwrap());
        }
        std::thread::sleep(POLL);
    })();
    let retired = child.stop();
    let drained = platform::finish_workers(workers);
    retired?;
    drained?;
    result
}

fn drain<P: platform::PipeRead>(
    mut pipe: P,
    limit: usize,
    stream: &'static str,
    stop: &AtomicBool,
) -> Result<Vec<u8>, CandidateError> {
    let mut output = Vec::new();
    let mut buffer = [0; 8192];
    while platform::read_ready(&pipe, stop)? {
        let n = pipe.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        if n > limit.saturating_sub(output.len()) {
            return Err(CandidateError::OutputTooLarge(stream));
        }
        output.extend_from_slice(&buffer[..n]);
    }
    Ok(output)
}
fn feed<P: platform::PipeWrite>(mut pipe: P, input: &[u8], stop: &AtomicBool) -> io::Result<()> {
    let mut at = 0;
    while at < input.len() && platform::write_ready(&pipe, stop)? {
        // POSIX guarantees at least 512 bytes of atomic pipe capacity.
        let end = (at + 512).min(input.len());
        let n = pipe.write(&input[at..end])?;
        if n == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        at += n;
    }
    Ok(())
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::os::{fd::AsRawFd, unix::process::CommandExt};
    pub trait PipeRead: Read + AsRawFd {}
    impl<T: Read + AsRawFd> PipeRead for T {}
    pub trait PipeWrite: Write + AsRawFd {}
    impl<T: Write + AsRawFd> PipeWrite for T {}
    pub fn read_ready(pipe: &impl AsRawFd, stop: &AtomicBool) -> io::Result<bool> {
        ready(pipe, false, stop)
    }
    pub fn write_ready(pipe: &impl AsRawFd, stop: &AtomicBool) -> io::Result<bool> {
        ready(pipe, true, stop)
    }
    pub fn prepare(command: &mut Command) {
        command.process_group(0);
    }
    pub fn poll_status(
        child: &mut std::process::Child,
    ) -> io::Result<Option<std::process::ExitStatus>> {
        use std::os::unix::process::ExitStatusExt;
        // Never reap before group cleanup: the reserved leader PID pins the
        // numeric process-group identity even when every member has exited.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                child.id(),
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::Interrupted {
                Ok(None)
            } else {
                Err(error)
            };
        }
        if unsafe { info.si_pid() } == 0 {
            return Ok(None);
        }
        let code = unsafe { info.si_status() };
        let raw = match info.si_code {
            libc::CLD_EXITED => code << 8,
            libc::CLD_DUMPED => code | 0x80,
            _ => code,
        };
        Ok(Some(std::process::ExitStatus::from_raw(raw)))
    }
    pub struct Owner(u32);
    impl Owner {
        pub fn new(child: &mut std::process::Child) -> io::Result<Self> {
            Ok(Self(child.id()))
        }
        pub fn terminate(&self) -> io::Result<()> {
            // The group was created exclusively for this invocation, before exec.
            let result = unsafe { libc::kill(-(self.0 as i32), libc::SIGKILL) };
            if result < 0 {
                let error = io::Error::last_os_error();
                // ESRCH: the group is gone. EPERM on macOS: a member could not be
                // signalled, which is the already-exited (zombie) leader held by
                // WNOWAIT; nothing else of ours can refuse a signal.
                let benign = error.raw_os_error() == Some(libc::ESRCH)
                    || (cfg!(target_os = "macos") && error.raw_os_error() == Some(libc::EPERM));
                if !benign {
                    return Err(error);
                }
            }
            // Signal delivery is not process retirement. Keep the waitable
            // leader (and therefore the group identity) reserved until every
            // Linux member has actually stopped executing. Pipe workers alone
            // cannot prove this: cancellation lets them leave before EOF, and a
            // descendant can close its inherited pipes while still running.
            #[cfg(target_os = "linux")]
            {
                let deadline = Instant::now() + Duration::from_secs(1);
                while retirement_pending(
                    group_is_running(self.0, deadline)?,
                    Instant::now(),
                    deadline,
                )? {
                    std::thread::sleep(POLL);
                }
            }
            Ok(())
        }
    }
    #[cfg(target_os = "linux")]
    fn group_is_running(group: u32, deadline: Instant) -> io::Result<bool> {
        // This confirms ordinary descendants visible in this proc namespace;
        // privileged descendants hidden by proc policy are outside this check.
        for entry in std::fs::read_dir("/proc")? {
            retirement_pending(true, Instant::now(), deadline)?;
            let entry = entry?;
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
                continue;
            };
            // getpgid does not require reading another user's proc stat. With
            // hidepid, unrelated visible entries can deny that read; only an
            // identified member of our reserved group needs inspecting.
            let observed_group = unsafe { libc::getpgid(pid) };
            if observed_group < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    continue;
                }
                return Err(error);
            }
            if observed_group as u32 != group {
                continue;
            }
            let stat = match std::fs::read(entry.path().join("stat")) {
                Ok(stat) => stat,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            if stat_group_is_running(&String::from_utf8_lossy(&stat), group)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    #[cfg(target_os = "linux")]
    fn retirement_pending(running: bool, now: Instant, deadline: Instant) -> io::Result<bool> {
        if running && now >= deadline {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "formatter process group retirement could not be confirmed",
            ))
        } else {
            Ok(running)
        }
    }
    #[cfg(target_os = "linux")]
    fn stat_group_is_running(stat: &str, group: u32) -> io::Result<bool> {
        // comm may itself contain spaces and ')'; fields after its final ')'
        // begin with state, ppid and pgrp. Zombies need reaping by their actual
        // parent/init, but cannot execute or retain an inherited pipe.
        let fields = stat.rsplit_once(") ").map(|(_, fields)| fields);
        let mut fields = fields.unwrap_or_default().split_whitespace();
        let state = fields.next();
        let _parent = fields.next();
        let parsed_group = fields.next().and_then(|field| field.parse::<u32>().ok());
        match (state, parsed_group) {
            (Some(state), Some(parsed_group)) => {
                Ok(parsed_group == group && !matches!(state, "Z" | "X" | "x"))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid process stat",
            )),
        }
    }
    pub fn finish_workers(workers: Vec<std::thread::JoinHandle<()>>) -> io::Result<()> {
        for worker in workers {
            let _ = worker.join();
        }
        Ok(())
    }
    #[cfg(all(test, target_os = "linux"))]
    mod retirement_tests {
        use super::*;

        #[test]
        fn stat_comm_delimiters_do_not_hide_a_live_group_member() {
            assert!(stat_group_is_running("42 (worker ) with spaces) D 1 42 0", 42).unwrap());
            assert!(!stat_group_is_running("43 (worker )) Z 1 42 0", 42).unwrap());
            assert!(!stat_group_is_running("44 (other) R 1 99 0", 42).unwrap());
            assert_eq!(
                stat_group_is_running("42 (broken)", 42).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }

        #[test]
        fn cleanup_deadline_refuses_live_members_without_refusing_retired_members() {
            let start = Instant::now();
            let deadline = start + Duration::from_secs(1);
            assert!(retirement_pending(true, start, deadline).unwrap());
            assert_eq!(
                retirement_pending(true, deadline, deadline)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::TimedOut
            );
            assert!(!retirement_pending(false, deadline, deadline).unwrap());
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::os::windows::{io::AsRawHandle, process::CommandExt};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD,
                THREADENTRY32,
            },
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            },
            Threading::{OpenThread, ResumeThread, CREATE_SUSPENDED, THREAD_SUSPEND_RESUME},
            IO::CancelSynchronousIo,
        },
    };
    pub trait PipeRead: Read {}
    impl<T: Read> PipeRead for T {}
    pub trait PipeWrite: Write {}
    impl<T: Write> PipeWrite for T {}
    pub fn read_ready(_: &impl Read, stop: &AtomicBool) -> io::Result<bool> {
        Ok(!stop.load(Ordering::Acquire))
    }
    pub fn write_ready(_: &impl Write, stop: &AtomicBool) -> io::Result<bool> {
        Ok(!stop.load(Ordering::Acquire))
    }
    pub fn prepare(command: &mut Command) {
        command.creation_flags(CREATE_SUSPENDED);
    }
    pub fn poll_status(
        child: &mut std::process::Child,
    ) -> io::Result<Option<std::process::ExitStatus>> {
        child.try_wait()
    }
    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    pub struct Owner(Handle);
    impl Owner {
        pub fn new(child: &mut std::process::Child) -> io::Result<Self> {
            // Suspended creation prevents a descendant from escaping before assignment.
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let owner = Self(Handle(job));
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                if SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of_val(&limits) as u32,
                ) == 0
                    || AssignProcessToJobObject(job, child.as_raw_handle()) == 0
                {
                    return Err(io::Error::last_os_error());
                }
                let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
                if snapshot == INVALID_HANDLE_VALUE {
                    return Err(io::Error::last_os_error());
                }
                let snapshot = Handle(snapshot);
                let mut entry: THREADENTRY32 = std::mem::zeroed();
                entry.dwSize = std::mem::size_of_val(&entry) as u32;
                let mut present = Thread32First(snapshot.0, &mut entry);
                while present != 0 {
                    if entry.th32OwnerProcessID == child.id() {
                        let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                        if thread.is_null() {
                            return Err(io::Error::last_os_error());
                        }
                        let thread = Handle(thread);
                        if ResumeThread(thread.0) == u32::MAX {
                            return Err(io::Error::last_os_error());
                        }
                        return Ok(owner);
                    }
                    present = Thread32Next(snapshot.0, &mut entry);
                }
                Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "suspended formatter thread not found",
                ))
            }
        }
        pub fn terminate(&self) -> io::Result<()> {
            if unsafe { TerminateJobObject(self.0 .0, 1) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
    }
    pub fn finish_workers(workers: Vec<std::thread::JoinHandle<()>>) -> io::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(2);
        // Cancellation is tied to each live JoinHandle's actual thread handle.
        // Repeat to cover a worker entering I/O after its last stop-flag check.
        while workers.iter().any(|worker| !worker.is_finished()) {
            if Instant::now() >= deadline {
                // The job has already been terminated. Do not block the caller if
                // the OS cannot complete cancellation of a synchronous pipe I/O.
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "formatter pipe cleanup timed out",
                ));
            }
            for worker in &workers {
                if !worker.is_finished() {
                    unsafe {
                        CancelSynchronousIo(worker.as_raw_handle());
                    }
                }
            }
            std::thread::sleep(POLL);
        }
        for worker in workers {
            let _ = worker.join();
        }
        Ok(())
    }
}

/// Fixed stdin protocol: no formatter is allowed to edit the on-disk source.
pub fn formatter_arguments(
    tool: &str,
    filename: Option<&Path>,
) -> Result<Vec<String>, CandidateError> {
    let mut args: Vec<String> = match tool {
        "black" => vec!["-q", "-"],
        "ruff" => vec!["format", "-"],
        "isort" => vec!["--stdout", "-"],
        "autopep8" => vec!["-"],
        "yapf" => vec![],
        _ => vec![],
    }
    .into_iter()
    .map(str::to_owned)
    .collect();
    if let Some(path) = filename {
        match tool {
            "ruff" => args.extend([
                "--stdin-filename".into(),
                path.to_string_lossy().into_owned(),
            ]),
            "isort" => args.extend(["--filename".into(), path.to_string_lossy().into_owned()]),
            _ => {}
        }
    }
    Ok(args)
}

/// Quoted argv, never a shell command. Backslashes in Windows paths survive.
pub fn parse_arguments(text: &str) -> Result<Vec<String>, CandidateError> {
    if text.len() > 64 * 1024 {
        return Err(CandidateError::Configuration(
            "arguments exceed 64 KiB".into(),
        ));
    }
    let mut result = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut started = false;
    for c in text.chars() {
        if let Some(delimiter) = quote {
            if c == delimiter {
                quote = None;
            } else {
                word.push(c);
            }
            started = true;
        } else if c == '\'' || c == '"' {
            quote = Some(c);
            started = true;
        } else if c.is_whitespace() {
            if started {
                result.push(std::mem::take(&mut word));
                started = false;
            }
        } else {
            word.push(c);
            started = true;
        }
    }
    if quote.is_some() {
        return Err(CandidateError::Configuration(
            "unterminated argument quote".into(),
        ));
    }
    if started {
        result.push(word);
    }
    if result.len() > 256 {
        return Err(CandidateError::Configuration("too many arguments".into()));
    }
    Ok(result)
}

/// Diagnostic for the macOS EPERM seen in CI: prints how each spawn variant
/// behaves. Run with `cargo test -p rustpy-format --lib -- --ignored spawn_probe --nocapture`.
#[cfg(all(test, unix))]
mod spawn_probe {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    fn report(label: &str, command: &mut Command) {
        match command.stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
            Ok(mut child) => {
                let status = child.wait();
                eprintln!(
                    "probe {label}: spawned pid {}, status {status:?}",
                    child.id()
                );
            }
            Err(error) => eprintln!(
                "probe {label}: spawn failed: {error} ({:?})",
                error.raw_os_error()
            ),
        }
    }

    #[test]
    #[ignore = "diagnostic only"]
    fn spawn_probe() {
        eprintln!(
            "probe identity: pid {} pgid {} sid {}",
            std::process::id(),
            unsafe { libc::getpgid(0) },
            unsafe { libc::getsid(0) }
        );
        report("plain sh", Command::new("/bin/sh").args(["-c", "exit 0"]));
        report(
            "sh + process_group(0)",
            Command::new("/bin/sh")
                .args(["-c", "exit 0"])
                .process_group(0),
        );
        let mut pre = Command::new("/bin/sh");
        pre.args(["-c", "exit 0"]);
        unsafe {
            pre.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        report("sh + pre_exec setpgid", &mut pre);
        let dir = std::env::temp_dir().join(format!("rustpy-spawn-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fmt.sh");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        std::fs::set_permissions(&script, permissions).unwrap();
        report(
            "script + process_group(0)",
            Command::new(&script).process_group(0),
        );
        report(
            "python3 + process_group(0)",
            Command::new("python3")
                .args(["-c", "pass"])
                .process_group(0),
        );
    }
}

// macOS runners fail these with EPERM at process-group operations; see the
// known-issues list until that is understood on a Mac.
#[cfg(all(test, target_os = "linux"))]
mod ownership_tests {
    use super::*;
    #[test]
    fn exited_leader_remains_unreaped_until_owned_group_cleanup() {
        let mut command = Command::new("python3");
        command
            .args(["-c", "raise SystemExit(7)"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        platform::prepare(&mut command);
        let mut child = command.spawn().unwrap();
        let owner = platform::Owner::new(&mut child).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let status = loop {
            if let Some(status) = platform::poll_status(&mut child).unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(POLL);
        };
        assert_eq!(status.code(), Some(7));
        assert_eq!(
            platform::poll_status(&mut child).unwrap().unwrap().code(),
            Some(7)
        );
        #[cfg(target_os = "linux")]
        {
            let stat = std::fs::read_to_string(format!("/proc/{}/stat", child.id())).unwrap();
            assert!(
                stat.split_once(") ").unwrap().1.starts_with('Z'),
                "leader must remain reserved until cleanup: {stat}"
            );
        }
        owner.terminate().unwrap();
        rustpy_core::process::terminate_owned(&mut child, Duration::ZERO);
        assert_eq!(child.wait().unwrap().code(), Some(7));
    }
}

#[cfg(test)]
mod lease_tests {
    use super::*;
    #[test]
    fn detached_worker_retains_capacity_until_it_releases_ownership() {
        static LIMITER: InvocationLimiter = InvocationLimiter(AtomicUsize::new(0));
        let first = LIMITER.acquire().unwrap();
        let worker = first.clone();
        let remaining: Vec<_> = (0..3).map(|_| LIMITER.acquire().unwrap()).collect();
        drop(first);
        assert!(matches!(LIMITER.acquire(), Err(CandidateError::Busy)));
        drop(worker);
        let replacement = LIMITER.acquire().unwrap();
        assert!(matches!(LIMITER.acquire(), Err(CandidateError::Busy)));
        drop(remaining);
        drop(replacement);
        assert!(LIMITER.acquire().is_ok());
    }
}
