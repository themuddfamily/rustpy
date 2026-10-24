//! A live command owns its process group/job until descendants are killed and the
//! leader is reaped. Numeric Unix identities are never signalled after reaping.
use std::{
    io,
    process::{Child, Command, ExitStatus},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(1);
/// Cross-platform ownership of a prepared command and its descendants.
///
/// Call [`Self::prepare`] before spawning, then [`Self::new`] immediately after
/// spawn. Attach failure requires killing and waiting for the child. Use
/// [`Self::poll`] rather than `Child::try_wait`: Unix PID/PGID identity stays
/// reserved until descendant cleanup, while Windows uses a kill-on-close Job.
pub struct ProcessTree {
    owner: u64,
    pid: u32,
    pty: bool,
    /// A launcher (an external GUI program such as a web browser) may hand
    /// off to processes it started and exit: they keep running until they
    /// exit or the owner is stopped instead of being ended with the leader.
    launcher: bool,
    reaped: AtomicBool,
    stopping: AtomicBool,
    interrupted: AtomicBool,
    #[cfg(unix)]
    retirement: std::sync::Mutex<unix::Retirement>,
    #[cfg(windows)]
    job: windows::Job,
}
impl ProcessTree {
    /// Reserve a Unix group or prepare a hidden, suspended Windows background process/group.
    /// A suspended Windows child must be attached via [`Self::new`] to resume.
    pub fn prepare(command: &mut Command) {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
            crate::orphan_guard::set_parent_death_signal(command);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            use windows_sys::Win32::System::Threading::{
                CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_SUSPENDED,
            };
            // Captured/background commands must not allocate a native console.
            // PTY launchers prepare their own STARTUPINFO/ConPTY flags instead.
            command.creation_flags(CREATE_SUSPENDED | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        }
    }
    /// Attach the owned group/job, resuming a Windows child only after attach.
    pub fn new(child: &mut Child) -> io::Result<Self> {
        Self::attach(child, false)
    }
    /// [`Self::new`] for an external GUI program launcher such as a web
    /// browser. Its descendants outlive the leader's exit (Chromium hands off
    /// to the processes it starts) and the Windows job lets a process that
    /// explicitly asks to break away do so; Stop/Drop still ends the job.
    pub fn new_launcher(child: &mut Child) -> io::Result<Self> {
        Self::attach(child, true)
    }
    fn attach(child: &mut Child, launcher: bool) -> io::Result<Self> {
        #[cfg(unix)]
        unix::verify_group(child.id())?;
        crate::orphan_guard::register(child.id());
        #[cfg(windows)]
        let job = {
            use std::os::windows::io::AsRawHandle;
            windows::Job::attach_suspended(child.id(), child.as_raw_handle(), launcher)?
        };
        Ok(Self {
            owner: NEXT.fetch_add(1, Ordering::Relaxed),
            pid: child.id(),
            pty: false,
            launcher,
            reaped: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            interrupted: AtomicBool::new(false),
            #[cfg(unix)]
            retirement: std::sync::Mutex::new(unix::Retirement::default()),
            #[cfg(windows)]
            job,
        })
    }
    pub fn for_pty(child: &mut (dyn portable_pty::Child + Send + Sync)) -> io::Result<Self> {
        let pid = child
            .process_id()
            .ok_or_else(|| io::Error::other("PTY did not expose its process identity"))?;
        #[cfg(unix)]
        unix::verify_group(pid)?;
        crate::orphan_guard::register(pid);
        #[cfg(windows)]
        let job = windows::Job::attach_suspended(
            pid,
            child
                .as_raw_handle()
                .ok_or_else(|| io::Error::other("PTY did not expose its owned process handle"))?,
            false,
        )?;
        Ok(Self {
            owner: NEXT.fetch_add(1, Ordering::Relaxed),
            pid,
            pty: true,
            launcher: false,
            reaped: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            interrupted: AtomicBool::new(false),
            #[cfg(unix)]
            retirement: std::sync::Mutex::new(unix::Retirement::default()),
            #[cfg(windows)]
            job,
        })
    }
    pub fn stamp(&self) -> String {
        format!(
            "owned-process:{}:pid={}:group={}:pty={}:job={}:interrupt={}:stop={}:reaped={}",
            self.owner,
            self.pid,
            self.pid,
            self.pty,
            self.job_identity(),
            self.interrupted.load(Ordering::Acquire),
            self.stopping.load(Ordering::Acquire),
            self.reaped.load(Ordering::Acquire)
        )
    }
    fn job_identity(&self) -> usize {
        #[cfg(windows)]
        {
            self.job.identity()
        }
        #[cfg(not(windows))]
        {
            0
        }
    }
    pub fn pid(&self) -> u32 {
        self.pid
    }
    /// The leader no longer pins the group: withdraw it from orphan cleanup.
    fn mark_reaped(&self) {
        if !self.reaped.swap(true, Ordering::AcqRel) {
            crate::orphan_guard::unregister(self.pid);
        }
    }
    fn terminate_tree(&self) {
        if self.reaped.load(Ordering::Acquire) {
            return;
        }
        #[cfg(unix)]
        unix::signal(self.pid, libc::SIGKILL);
        #[cfg(windows)]
        self.job.terminate();
    }
    /// Poll without releasing Unix identity before descendant cleanup.
    pub fn poll(&self, child: &mut Child) -> io::Result<Option<ExitStatus>> {
        if self.reaped.load(Ordering::Acquire) {
            return child.try_wait();
        }
        #[cfg(unix)]
        match unix::exited_without_reap(self.pid) {
            Ok(false) => return Ok(None),
            Ok(true) => {}
            Err(error) => {
                if error.raw_os_error() == Some(libc::ECHILD) {
                    self.mark_reaped();
                }
                return Err(error);
            }
        }
        #[cfg(windows)]
        {
            if child.try_wait()?.is_none() {
                return Ok(None);
            }
        }
        // The Unix leader is still waitable here. Its reserved PID pins PGID
        // until group cleanup, including helpers whose parent has just exited.
        // A launcher's hand-off keeps running until it exits or is stopped.
        if !self.launcher || self.stopping.load(Ordering::Acquire) {
            self.terminate_tree();
        }
        if !self.members_retired()? {
            return Ok(None);
        }
        let status = child.try_wait()?;
        if status.is_some() {
            self.mark_reaped();
        }
        Ok(status)
    }
    pub fn poll_pty(
        &self,
        child: &mut (dyn portable_pty::Child + Send + Sync),
    ) -> io::Result<Option<portable_pty::ExitStatus>> {
        if self.reaped.load(Ordering::Acquire) {
            return child.try_wait();
        }
        #[cfg(unix)]
        match unix::exited_without_reap(self.pid) {
            Ok(false) => return Ok(None),
            Ok(true) => {}
            Err(error) => {
                if error.raw_os_error() == Some(libc::ECHILD) {
                    self.mark_reaped();
                }
                return Err(error);
            }
        }
        #[cfg(windows)]
        {
            if child.try_wait()?.is_none() {
                return Ok(None);
            }
        }
        self.terminate_tree();
        if !self.members_retired()? {
            return Ok(None);
        }
        let status = child.try_wait()?;
        if status.is_some() {
            self.mark_reaped();
        }
        Ok(status)
    }
    /// Request SIGINT / console Ctrl-Break while the child is still owned.
    pub fn interrupt(&self, child: &mut Child) -> io::Result<()> {
        if self.poll(child)?.is_some() {
            return Ok(());
        }
        self.mark_interrupt();
        #[cfg(unix)]
        unix::signal(self.pid, libc::SIGINT);
        #[cfg(windows)]
        unsafe {
            if windows_sys::Win32::System::Console::GenerateConsoleCtrlEvent(1, self.pid) == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
    pub fn mark_interrupt(&self) {
        self.interrupted.store(true, Ordering::Release);
    }
    /// Signal the owned group/job without waiting for kernel retirement.
    /// A caller must retain this owner and child, and keep polling to completion.
    pub fn request_force(&self) {
        if self.reaped.load(Ordering::Acquire) {
            return;
        }
        self.stopping.store(true, Ordering::Release);
        self.terminate_tree();
    }
    fn members_retired(&self) -> io::Result<bool> {
        #[cfg(unix)]
        {
            self.retirement
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .poll(self.pid)
        }
        #[cfg(windows)]
        {
            self.job.retired()
        }
        #[cfg(not(any(unix, windows)))]
        {
            Ok(true)
        }
    }
    /// Blocking background cleanup. UI owners use request_force plus poll.
    pub fn force(&self, child: &mut Child) {
        self.request_force();
        while !self.reaped.load(Ordering::Acquire) {
            if matches!(self.poll(child), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    /// Blocking background PTY cleanup; retains the group/job through retirement.
    pub fn force_pty(&self, child: &mut (dyn portable_pty::Child + Send + Sync)) {
        self.request_force();
        while !self.reaped.load(Ordering::Acquire) {
            if matches!(self.poll_pty(child), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
impl Drop for ProcessTree {
    fn drop(&mut self) {
        self.terminate_tree();
        if !self.reaped.load(Ordering::Acquire) {
            crate::orphan_guard::unregister(self.pid);
        }
    }
}
#[cfg(all(test, windows))]
mod ownership_tests {
    use super::*;
    use std::os::windows::io::AsRawHandle;

    #[test]
    fn suspended_windows_child_joins_job_before_execution_and_is_reaped() {
        let mut command = Command::new("cmd.exe");
        command.args(["/D", "/C", "ping -n 30 127.0.0.1 >NUL"]);
        ProcessTree::prepare(&mut command);
        let mut child = command.spawn().unwrap();
        let owner = ProcessTree::new(&mut child).unwrap();
        let mut assigned = 0;
        let result = unsafe {
            windows_sys::Win32::System::JobObjects::IsProcessInJob(
                child.as_raw_handle(),
                std::ptr::null_mut(),
                &mut assigned,
            )
        };
        assert_ne!(result, 0);
        assert_ne!(assigned, 0);
        owner.force(&mut child);
        assert!(child.try_wait().unwrap().is_some());
        assert!(owner.stamp().contains("stop=true:reaped=true"));
    }
    use std::os::windows::{
        io::{FromRawHandle, OwnedHandle},
        process::CommandExt,
    };
    use std::{
        path::{Path, PathBuf},
        time::{Duration, Instant},
    };
    use windows_sys::Win32::{
        Foundation::{FILETIME, WAIT_OBJECT_0},
        System::Threading::{
            GetCurrentProcess, GetProcessTimes, OpenProcess, WaitForSingleObject, CREATE_NO_WINDOW,
            PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        },
    };

    fn created(handle: std::os::windows::io::RawHandle) -> u64 {
        let mut values: [FILETIME; 4] = unsafe { std::mem::zeroed() };
        let [created, exited, kernel, user] = &mut values;
        assert_ne!(
            unsafe { GetProcessTimes(handle, created, exited, kernel, user) },
            0
        );
        (u64::from(values[0].dwHighDateTime) << 32) | u64::from(values[0].dwLowDateTime)
    }
    fn publish(path: &Path) {
        let temporary = path.with_extension("pending");
        std::fs::write(
            &temporary,
            format!(
                "{} {}",
                std::process::id(),
                created(unsafe { GetCurrentProcess() })
            ),
        )
        .unwrap();
        std::fs::rename(temporary, path).unwrap();
    }
    fn wait_file(path: &Path) -> String {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Ok(value) = std::fs::read_to_string(path) {
                return value;
            }
            assert!(
                Instant::now() < deadline,
                "native child readiness missing: {}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn pin(path: &Path) -> OwnedHandle {
        let value = wait_file(path);
        let fields = value.split_whitespace().collect::<Vec<_>>();
        let pid = fields[0].parse::<u32>().unwrap();
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        assert!(!handle.is_null());
        let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
        assert_eq!(
            created(handle.as_raw_handle()),
            fields[1].parse::<u64>().unwrap(),
            "native PID reused before pinning"
        );
        handle
    }
    struct FixtureOwner {
        child: Child,
        tree: Option<ProcessTree>,
    }
    impl Drop for FixtureOwner {
        fn drop(&mut self) {
            if let Some(tree) = &self.tree {
                tree.force(&mut self.child);
            } else {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }
    #[test]
    fn windows_owned_process_fixture() {
        let Some(directory) = std::env::var_os("RUSTPY_WINDOWS_JOB_CHILD") else {
            return;
        };
        let directory = PathBuf::from(directory);
        assert!(
            unsafe { windows_sys::Win32::System::Console::GetConsoleWindow() }.is_null(),
            "captured Job helpers must not allocate a console window"
        );
        if std::env::var_os("RUSTPY_WINDOWS_JOB_GRANDCHILD").is_some() {
            publish(&directory.join("grandchild"));
        } else {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "process_tree::ownership_tests::windows_owned_process_fixture",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("RUSTPY_WINDOWS_JOB_GRANDCHILD", "1")
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .unwrap();
            wait_file(&directory.join("grandchild"));
            publish(&directory.join("leader"));
            // The actual production Job must retire this descendant; exiting
            // its parent or dropping std::process::Child is not cleanup.
            let _ = child.wait();
            panic!("owned descendant exited before its Job was stopped");
        }
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    #[test]
    #[ignore = "requires admitted actual Windows console supervisor and private case"]
    fn windows_job_retires_owned_descendants_on_force_and_drop() {
        assert_eq!(
            std::env::var("RUSTPY_WINDOWS_ACCEPTANCE").as_deref(),
            Ok("1")
        );
        let root = PathBuf::from(
            std::env::var_os("RUSTPY_WINDOWS_CASE").expect("private Windows case required"),
        );
        assert!(root.is_absolute() && root.is_dir());
        for drop_owner in [false, true] {
            let directory = root.join(format!(
                "job-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&directory).unwrap();
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "process_tree::ownership_tests::windows_owned_process_fixture",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("RUSTPY_WINDOWS_JOB_CHILD", &directory)
                .env_remove("RUSTPY_WINDOWS_JOB_GRANDCHILD");
            ProcessTree::prepare(&mut command);
            let mut child = command.spawn().unwrap();
            let tree = match ProcessTree::new(&mut child) {
                Ok(tree) => tree,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("native Job attach failed: {error}");
                }
            };
            let mut fixture = FixtureOwner {
                child,
                tree: Some(tree),
            };
            let leader = pin(&directory.join("leader"));
            let descendant = pin(&directory.join("grandchild"));
            assert_eq!(
                created(leader.as_raw_handle()),
                created(fixture.child.as_raw_handle())
            );
            for handle in [&leader, &descendant] {
                let mut assigned = 0;
                assert_ne!(
                    unsafe {
                        windows_sys::Win32::System::JobObjects::IsProcessInJob(
                            handle.as_raw_handle(),
                            fixture.tree.as_ref().unwrap().job_identity() as _,
                            &mut assigned,
                        )
                    },
                    0
                );
                assert_ne!(
                    assigned, 0,
                    "both native owners must belong to the production Job"
                );
                assert_ne!(
                    unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) },
                    WAIT_OBJECT_0
                );
            }
            let deadline = Instant::now() + Duration::from_secs(10);
            if drop_owner {
                drop(fixture.tree.take());
            } else {
                let tree = fixture.tree.as_ref().unwrap();
                let requested = Instant::now();
                tree.request_force();
                assert!(requested.elapsed() < Duration::from_millis(100));
                while tree.poll(&mut fixture.child).unwrap().is_none() {
                    assert!(Instant::now() < deadline, "native Job retirement timed out");
                    std::thread::sleep(Duration::from_millis(5));
                }
                assert!(
                    tree.members_retired().unwrap(),
                    "published exit requires an empty production Job"
                );
            }
            for handle in [&leader, &descendant] {
                assert_eq!(
                    unsafe { WaitForSingleObject(handle.as_raw_handle(), 10_000) },
                    WAIT_OBJECT_0,
                    "native owned process survived Force/Drop"
                );
            }
            assert!(Instant::now() < deadline);
            fixture.child.wait().unwrap();
            std::fs::remove_dir_all(&directory).unwrap();
        }
    }
}
#[cfg(unix)]
mod unix {
    use super::*;
    #[derive(Default)]
    pub(super) struct Retirement {
        #[cfg(target_os = "linux")]
        entries: Option<std::fs::ReadDir>,
        #[cfg(target_os = "macos")]
        members: Option<std::vec::IntoIter<u32>>,
    }
    #[cfg(target_os = "linux")]
    pub(super) fn running_member(stat: &str, group: u32) -> io::Result<bool> {
        // comm may contain spaces and parentheses. After its final ')':
        // state(field3), ppid(4), process-group(5).
        let fields = stat
            .get(
                stat.rfind(')')
                    .ok_or_else(|| io::Error::other("Invalid process stat"))?
                    + 1..,
            )
            .ok_or_else(|| io::Error::other("Invalid process stat"))?;
        let mut fields = fields.split_whitespace();
        let state = fields
            .next()
            .ok_or_else(|| io::Error::other("Missing process state"))?;
        let _parent = fields.next();
        let actual = fields
            .next()
            .and_then(|value| value.parse::<u32>().ok())
            .ok_or_else(|| io::Error::other("Missing process group"))?;
        Ok(actual == group && !matches!(state, "Z" | "X" | "x"))
    }
    impl Retirement {
        #[cfg(target_os = "linux")]
        pub(super) fn poll(&mut self, group: u32) -> io::Result<bool> {
            use std::io::Read;
            if self.entries.is_none() {
                self.entries = Some(std::fs::read_dir("/proc")?);
            }
            // Bound each caller/UI pass. The waitable leader pins group identity
            // across the scan; no recycled numeric process IDs are signalled.
            for _ in 0..128 {
                let Some(entry) = self.entries.as_mut().unwrap().next() else {
                    self.entries = None;
                    return Ok(true);
                };
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        self.entries = None;
                        return Err(error);
                    }
                };
                let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
                    continue;
                };
                let actual_group = unsafe { libc::getpgid(pid as i32) };
                if actual_group < 0 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() == Some(libc::ESRCH) {
                        continue;
                    }
                    self.entries = None;
                    return Err(error);
                }
                if actual_group as u32 != group {
                    continue;
                }
                let result = (|| {
                    let mut stat = String::new();
                    std::fs::File::open(entry.path().join("stat"))?
                        .take(16385)
                        .read_to_string(&mut stat)?;
                    if stat.len() > 16384 {
                        return Err(io::Error::other("Process stat exceeds limit"));
                    }
                    running_member(&stat, group)
                })();
                match result {
                    Ok(false) => {}
                    Ok(true) => {
                        self.entries = None;
                        return Ok(false);
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => {
                        self.entries = None;
                        return Err(error);
                    }
                }
            }
            Ok(false)
        }
        #[cfg(target_os = "macos")]
        pub(super) fn poll(&mut self, group: u32) -> io::Result<bool> {
            if self.members.is_none() {
                // PROC_PGRP_ONLY asks libproc for this pinned group alone.
                const PROC_PGRP_ONLY: u32 = 2;
                let mut members = vec![0u32; 65536];
                let bytes = unsafe {
                    libc::proc_listpids(
                        PROC_PGRP_ONLY,
                        group,
                        members.as_mut_ptr().cast(),
                        (members.len() * 4) as i32,
                    )
                };
                if bytes < 0 {
                    return Err(io::Error::last_os_error());
                }
                if bytes as usize >= members.len() * 4 {
                    return Err(io::Error::other(
                        "Owned process group exceeds observation limit",
                    ));
                }
                members.truncate(bytes as usize / 4);
                self.members = Some(members.into_iter());
            }
            for _ in 0..128 {
                let Some(pid) = self.members.as_mut().unwrap().next() else {
                    self.members = None;
                    return Ok(true);
                };
                let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
                let bytes = unsafe {
                    libc::proc_pidinfo(
                        pid as i32,
                        libc::PROC_PIDTBSDINFO,
                        0,
                        (&mut info as *mut libc::proc_bsdinfo).cast(),
                        std::mem::size_of_val(&info) as i32,
                    )
                };
                if bytes == 0 {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() == Some(libc::ESRCH) {
                        continue;
                    }
                    self.members = None;
                    return Err(error);
                }
                if bytes as usize != std::mem::size_of_val(&info) {
                    self.members = None;
                    return Err(io::Error::other("Incomplete owned process observation"));
                }
                if info.pbi_pgid == group && info.pbi_status != libc::SZOMB {
                    self.members = None;
                    return Ok(false);
                }
            }
            Ok(false)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        pub(super) fn poll(&mut self, _group: u32) -> io::Result<bool> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Owned group retirement observation unavailable on this Unix target",
            ))
        }
    }
    pub(super) fn verify_group(pid: u32) -> io::Result<()> {
        let group = unsafe { libc::getpgid(pid as i32) };
        if group == pid as i32 {
            Ok(())
        } else {
            Err(io::Error::other(
                "Child is not the leader of its owned process group",
            ))
        }
    }
    pub(super) fn exited_without_reap(pid: u32) -> io::Result<bool> {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                return Ok(false);
            }
            return Err(error);
        }
        Ok(unsafe { info.si_pid() } != 0)
    }
    pub(super) fn signal(pid: u32, signal: i32) {
        unsafe {
            libc::kill(-(pid as i32), signal);
        }
    }
}
#[cfg(windows)]
mod windows {
    use super::*;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD,
                THREADENTRY32,
            },
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            },
            Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
        },
    };
    pub(super) struct Job {
        handle: OwnedHandle,
    }
    impl Job {
        pub(super) fn attach_suspended(
            pid: u32,
            process: RawHandle,
            breakaway: bool,
        ) -> io::Result<Self> {
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let job = Self {
                    handle: OwnedHandle::from_raw_handle(handle),
                };
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                    | if breakaway {
                        JOB_OBJECT_LIMIT_BREAKAWAY_OK
                    } else {
                        0
                    };
                if SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of_val(&limits) as u32,
                ) == 0
                    || AssignProcessToJobObject(handle, process) == 0
                {
                    return Err(io::Error::last_os_error());
                }
                let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
                if snapshot == INVALID_HANDLE_VALUE {
                    return Err(io::Error::last_os_error());
                }
                let snapshot = OwnedHandle::from_raw_handle(snapshot);
                let mut thread: THREADENTRY32 = std::mem::zeroed();
                thread.dwSize = std::mem::size_of_val(&thread) as u32;
                let mut present = Thread32First(snapshot.as_raw_handle(), &mut thread);
                while present != 0 {
                    if thread.th32OwnerProcessID == pid {
                        let handle = OpenThread(THREAD_SUSPEND_RESUME, 0, thread.th32ThreadID);
                        if handle.is_null() {
                            return Err(io::Error::last_os_error());
                        }
                        let handle = OwnedHandle::from_raw_handle(handle);
                        if ResumeThread(handle.as_raw_handle()) == u32::MAX {
                            return Err(io::Error::last_os_error());
                        }
                        return Ok(job);
                    }
                    present = Thread32Next(snapshot.as_raw_handle(), &mut thread);
                }
                Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "Suspended child thread was not found",
                ))
            }
        }
        pub(super) fn retired(&self) -> io::Result<bool> {
            use windows_sys::Win32::System::JobObjects::{
                JobObjectBasicAccountingInformation, QueryInformationJobObject,
                JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
            };
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
            let result = unsafe {
                QueryInformationJobObject(
                    self.handle.as_raw_handle(),
                    JobObjectBasicAccountingInformation,
                    (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                    std::mem::size_of_val(&info) as u32,
                    std::ptr::null_mut(),
                )
            };
            if result == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(info.ActiveProcesses == 0)
        }
        pub(super) fn terminate(&self) {
            unsafe {
                TerminateJobObject(self.handle.as_raw_handle(), 137);
            }
        }
        pub(super) fn identity(&self) -> usize {
            self.handle.as_raw_handle() as usize
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod retirement_tests {
    use super::*;
    use std::{
        path::Path,
        process::Stdio,
        time::{Duration, Instant},
    };
    const FIXTURE: &str = "process_tree::retirement_tests::closed_pipe_fixture";
    #[test]
    fn closed_pipe_fixture() {
        let Ok(mode) = std::env::var("RUSTPY_TREE_RETIRE_FIXTURE") else {
            return;
        };
        let marker = std::env::var_os("RUSTPY_TREE_RETIRE_PID").unwrap();
        if mode == "child" {
            std::fs::write(marker, std::process::id().to_string()).unwrap();
            std::thread::sleep(Duration::from_secs(30));
        } else {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", FIXTURE, "--nocapture"])
                .env("RUSTPY_TREE_RETIRE_FIXTURE", "child")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let expires = Instant::now() + Duration::from_secs(5);
            while !Path::new(&marker).exists() {
                assert!(Instant::now() < expires);
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = child.try_wait();
        }
    }
    fn live(group: u32) -> bool {
        std::fs::read_dir("/proc")
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| {
                let stat = std::fs::read_to_string(entry.path().join("stat"));
                stat.is_ok_and(|stat| super::unix::running_member(&stat, group).unwrap_or(false))
            })
    }
    #[test]
    fn closed_pipe_descendants_retire_before_leader_reap_and_request_force_does_not_wait() {
        let marker = std::env::temp_dir().join(format!(
            "rustpy-retirement-{}-{}.pid",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let unrelated_marker = marker.with_extension("unrelated.pid");
        let mut unrelated_command = Command::new(std::env::current_exe().unwrap());
        unrelated_command
            .args(["--exact", FIXTURE, "--nocapture"])
            .env("RUSTPY_TREE_RETIRE_FIXTURE", "child")
            .env("RUSTPY_TREE_RETIRE_PID", &unrelated_marker)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        ProcessTree::prepare(&mut unrelated_command);
        let mut unrelated = unrelated_command.spawn().unwrap();
        let unrelated_owner = ProcessTree::new(&mut unrelated).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", FIXTURE, "--nocapture"])
            .env("RUSTPY_TREE_RETIRE_FIXTURE", "parent")
            .env("RUSTPY_TREE_RETIRE_PID", &marker)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        ProcessTree::prepare(&mut command);
        let mut child = command.spawn().unwrap();
        let owner = ProcessTree::new(&mut child).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !marker.exists() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        while !super::unix::exited_without_reap(owner.pid()).unwrap() {
            assert!(Instant::now() < deadline, "fixture leader did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        let before = Instant::now();
        owner.request_force();
        assert!(before.elapsed() < Duration::from_secs(1));
        loop {
            if owner.poll(&mut child).unwrap().is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "retirement still pending");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !live(owner.pid()),
            "completion requires physical group retirement even with closed pipes"
        );
        assert!(owner.stamp().contains("reaped=true"));
        // A retired owner must never signal its numeric group again.
        owner.request_force();
        assert!(owner.poll(&mut child).unwrap().is_some());
        let unrelated_alive = !super::unix::exited_without_reap(unrelated_owner.pid()).unwrap();
        unrelated_owner.force(&mut unrelated);
        let _ = std::fs::remove_file(marker);
        let _ = std::fs::remove_file(unrelated_marker);
        assert!(
            unrelated_alive,
            "retirement must not signal another owned group"
        );
    }
    #[test]
    fn launcher_hand_off_outlives_its_leader_until_stopped() {
        let marker = std::env::temp_dir().join(format!(
            "rustpy-launcher-{}-{}.pid",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", FIXTURE, "--nocapture"])
            .env("RUSTPY_TREE_RETIRE_FIXTURE", "parent")
            .env("RUSTPY_TREE_RETIRE_PID", &marker)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        ProcessTree::prepare(&mut command);
        let mut child = command.spawn().unwrap();
        let owner = ProcessTree::new_launcher(&mut child).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !super::unix::exited_without_reap(owner.pid()).unwrap() {
            assert!(Instant::now() < deadline, "fixture leader did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
        // The leader exited after its hand-off started: the hand-off lives on.
        for _ in 0..20 {
            assert!(owner.poll(&mut child).unwrap().is_none());
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(live(owner.pid()), "launcher hand-off ended with its leader");
        owner.request_force();
        while owner.poll(&mut child).unwrap().is_none() {
            assert!(Instant::now() < deadline, "stopped launcher did not retire");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!live(owner.pid()));
        let _ = std::fs::remove_file(marker);
    }
    #[test]
    fn killed_but_running_and_uninterruptible_members_are_not_retired() {
        for state in ["R", "S", "D", "T"] {
            assert!(
                super::unix::running_member(&format!("7 (worker (name)) {state} 1 7 0"), 7)
                    .unwrap()
            );
        }
        for state in ["Z", "X", "x"] {
            assert!(!super::unix::running_member(&format!("7 (worker) {state} 1 7 0"), 7).unwrap());
        }
        assert!(!super::unix::running_member("8 (other) R 1 8 0", 7).unwrap());
        assert!(super::unix::running_member("malformed", 7).is_err());
    }
}
