//! Windows diagnostic for "Open in browser" with Chromium browsers (Edge,
//! Chrome). Launches the same headless command in several ways and records,
//! for each, whether the local HTTP listener received the page request and the
//! page's JavaScript beacon, the leader's exit status and time, the processes
//! the launch started (Toolhelp snapshots by parent and by profile folder),
//! their job membership, and the tail of Chromium's own log
//! (`--enable-logging --v=1` writes `chrome_debug.log` into the profile).
//! Every run uses its own throw-away profile under `--work` and is ended
//! afterwards; anything still alive is killed and reported as a leftover.
//!
//! Usage: `windows_chromium_diagnostic --work <empty dir> [--browsers edge,chrome]
//! [--modes plain,run_session,...] [--timeout <seconds, default 20>]`
//!
//! Run through `qa/windows/run-chromium-diagnostic.ps1`. Prints JSON.

#[cfg(windows)]
mod win {
    use rustpy_run::process_owner::ProcessOwner;
    use rustpy_settings::web_browsers::{Kind, Profile};
    use rustpy_ui::web_browser_workspace::{detect_program, redirect_message, redirected_by};
    use serde_json::{json, Value};
    use std::{
        collections::BTreeMap,
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        os::windows::{
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
            process::CommandExt,
        },
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };
    use windows_sys::Wdk::System::Threading::{
        NtQueryInformationProcess, ProcessCommandLineInformation,
    };
    use windows_sys::Win32::{
        Foundation::{FILETIME, HANDLE, INVALID_HANDLE_VALUE, UNICODE_STRING, WAIT_OBJECT_0},
        Security::{
            GetTokenInformation, TokenElevation, TokenElevationType, TOKEN_ELEVATION, TOKEN_QUERY,
        },
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, Thread32First,
                Thread32Next, PROCESSENTRY32W, TH32CS_SNAPPROCESS, TH32CS_SNAPTHREAD,
                THREADENTRY32,
            },
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
                JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
                QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
                JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
            },
            Threading::{
                GetCurrentProcess, GetExitCodeProcess, GetProcessTimes, OpenProcess,
                OpenProcessToken, OpenThread, ResumeThread, TerminateProcess, WaitForSingleObject,
                CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_SUSPENDED,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
                THREAD_SUSPEND_RESUME,
            },
        },
    };

    /// Requests seen by the listener: (ms since start, request target).
    type Seen = Arc<Mutex<Vec<(u128, String)>>>;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mode {
        Plain,
        PlainNoDeElevate,
        PlainTermEnv,
        OwnerJob,
        RunSession,
        RunSessionConpty,
        JobKeep,
        JobBreakaway,
        Launcher,
        LauncherNoDeElevate,
    }

    const MODES: &[(Mode, &str, &str)] = &[
        (
            Mode::Plain,
            "plain",
            "(a) std::process::Command, no job, stdin/stdout/stderr null, CREATE_NO_WINDOW",
        ),
        (
            Mode::PlainNoDeElevate,
            "plain_no_de_elevate",
            "(a') as plain plus --do-not-de-elevate (Chromium relaunches itself \
             de-elevated through Explorer when started elevated)",
        ),
        (
            Mode::PlainTermEnv,
            "plain_term_env",
            "(f') as plain plus TERM=xterm-256color / COLORTERM=truecolor, the variables \
             the ConPTY path adds; (f) itself (ConPTY without them) is not reachable: \
             the ConPTY spawner always injects them",
        ),
        (
            Mode::OwnerJob,
            "owner_job",
            "(b) ProcessOwner as used for external tools: suspended, kill-on-close job, \
             the tree is ended as soon as the leader exits; stdio null",
        ),
        (
            Mode::RunSession,
            "run_session",
            "(c) RunSession::spawn of the Program run configuration the IDE used before \
             this fix (emulate_terminal is off by default, so this is NOT ConPTY: piped \
             stdio + kill-on-close job, tree ended when the leader exits)",
        ),
        (
            Mode::RunSessionConpty,
            "run_session_conpty",
            "(c') the same configuration with Emulate terminal on: ConPTY + job; \
             (d) ConPTY without a job is not reachable: the ConPTY child starts \
             suspended and only the job attach resumes it",
        ),
        (
            Mode::JobKeep,
            "job_keep",
            "(e1) own kill-on-close job without breakaway; the leader's exit does not end \
             the job (only the cleanup does)",
        ),
        (
            Mode::JobBreakaway,
            "job_breakaway",
            "(e2) own kill-on-close job with JOB_OBJECT_LIMIT_BREAKAWAY_OK; the leader's \
             exit does not end the job",
        ),
        (
            Mode::Launcher,
            "launcher",
            "(fix) RunSession::spawn_launcher, what Open in browser / Open URL now use: \
             stdin null, piped output, job with breakaway allowed, hand-off kept after \
             the leader exits, Stop ends the job",
        ),
        (
            Mode::LauncherNoDeElevate,
            "launcher_no_de_elevate",
            "(fix') as launcher plus --do-not-de-elevate",
        ),
    ];

    pub fn main() {
        let mut work = None;
        let mut browsers = vec![Kind::Edge, Kind::Chrome];
        let mut modes: Vec<Mode> = MODES.iter().map(|m| m.0).collect();
        let mut timeout = 20u64;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--work" => work = args.next().map(PathBuf::from),
                "--timeout" => {
                    timeout = args
                        .next()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or_else(|| usage("--timeout needs seconds"))
                }
                "--browsers" => {
                    let list = args.next().unwrap_or_default();
                    browsers = list
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(|name| match name.to_ascii_lowercase().as_str() {
                            "edge" => Kind::Edge,
                            "chrome" => Kind::Chrome,
                            other => usage(&format!("unknown browser {other}")),
                        })
                        .collect();
                }
                "--modes" => {
                    let list = args.next().unwrap_or_default();
                    modes = list
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(|name| {
                            MODES
                                .iter()
                                .find(|m| m.1 == name)
                                .map(|m| m.0)
                                .unwrap_or_else(|| usage(&format!("unknown mode {name}")))
                        })
                        .collect();
                }
                other => usage(&format!("unknown argument {other}")),
            }
        }
        let Some(work) = work else {
            usage("--work <dir> is required")
        };
        std::fs::create_dir_all(&work).expect("work directory");
        let work = std::path::absolute(&work).expect("work directory");

        let started = Instant::now();
        let seen: Seen = Arc::default();
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        let port = listener.local_addr().unwrap().port();
        {
            let seen = seen.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let seen = seen.clone();
                    std::thread::spawn(move || answer(stream, &seen, started, port));
                }
            });
        }

        let mut report = serde_json::Map::new();
        report.insert("work".into(), json!(work));
        report.insert("port".into(), json!(port));
        report.insert("timeout_s".into(), json!(timeout));
        report.insert("elevation".into(), elevation());
        report.insert("helper_job".into(), own_job());
        report.insert(
            "modes".into(),
            json!(MODES
                .iter()
                .filter(|m| modes.contains(&m.0))
                .map(|m| json!({"mode": m.1, "how": m.2}))
                .collect::<Vec<_>>()),
        );
        let mut runs = Vec::new();
        let mut index = 0;
        for kind in &browsers {
            let template = Profile::template(*kind);
            let program = detect_program(&template);
            let browser = template.name.to_ascii_lowercase();
            let Some(program) = program.filter(|p| p.is_file()) else {
                runs.push(json!({"browser": browser, "skipped": "not installed",
                                 "configured": template.program}));
                continue;
            };
            // An Image File Execution Options Debugger (MSEdgeRedirect) runs
            // instead of the browser in every mode: nothing to diagnose.
            if let Some(debugger) = redirected_by(&program) {
                runs.push(json!({"browser": browser, "program": program,
                                 "redirected_by": debugger,
                                 "skipped": redirect_message(&template.name, &debugger)}));
                continue;
            }
            for (mode, name, _) in MODES.iter().filter(|m| modes.contains(&m.0)) {
                index += 1;
                eprintln!("[{index}] {browser} {name}");
                let run = run_one(&Run {
                    mode: *mode,
                    mode_name: name,
                    browser: &browser,
                    program: &program,
                    work: &work,
                    index,
                    port,
                    seen: &seen,
                    started,
                    timeout: Duration::from_secs(timeout),
                });
                runs.push(run);
            }
        }
        report.insert("summary".into(), summary(&runs));
        report.insert("runs".into(), json!(runs));
        report.insert(
            "requests".into(),
            json!(seen
                .lock()
                .unwrap()
                .iter()
                .map(|(ms, target)| format!("{ms} {target}"))
                .collect::<Vec<_>>()),
        );
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
    }

    fn usage(message: &str) -> ! {
        eprintln!("{message}");
        eprintln!(
            "usage: windows_chromium_diagnostic --work <dir> [--browsers edge,chrome] \
             [--modes {}] [--timeout <s>]",
            MODES.iter().map(|m| m.1).collect::<Vec<_>>().join(",")
        );
        std::process::exit(2);
    }

    struct Run<'a> {
        mode: Mode,
        mode_name: &'a str,
        browser: &'a str,
        program: &'a Path,
        work: &'a Path,
        index: usize,
        port: u16,
        seen: &'a Seen,
        started: Instant,
        timeout: Duration,
    }

    enum Running {
        Child(Child),
        Owner(ProcessOwner, Child),
        Job(Job, Child),
        Session(Box<rustpy_run::RunSession>),
    }

    fn run_one(run: &Run) -> Value {
        let tag = format!("{}-{}", run.browser, run.mode_name);
        let profile = run.work.join(format!("p{:02}-{tag}", run.index));
        let _ = std::fs::create_dir_all(&profile);
        let url = format!("http://127.0.0.1:{}/diag/{tag}/index.html", run.port);
        let mut args: Vec<String> = [
            "--headless=new",
            "--disable-gpu",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-extensions",
            "--disable-component-update",
            "--disable-sync",
            "--disable-background-networking",
            "--enable-logging",
            "--v=1",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        if matches!(run.mode, Mode::PlainNoDeElevate | Mode::LauncherNoDeElevate) {
            args.push("--do-not-de-elevate".into());
        }
        args.push(format!("--user-data-dir={}", profile.display()));
        args.push(url.clone());
        let mut out = json!({
            "browser": run.browser,
            "mode": run.mode_name,
            "program": run.program,
            "profile": profile,
            "args": args,
        });
        let marker = (
            profile
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_ascii_lowercase(),
            run.work
                .file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default(),
        );
        let exe_name = run
            .program
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_ascii_lowercase();
        let before = run.seen.lock().unwrap().len();
        let t0 = Instant::now();
        let mut running = match spawn(run, &args) {
            Ok(running) => running,
            Err(error) => {
                out["error"] = json!(error);
                return out;
            }
        };
        let pid = match &running {
            Running::Child(c) | Running::Owner(_, c) | Running::Job(_, c) => Some(c.id()),
            Running::Session(s) => s.process_id(),
        };
        out["spawn_ms"] = json!(t0.elapsed().as_millis());
        out["pid"] = json!(pid);
        let leader = pid.and_then(|pid| open(pid, false));
        let leader_created = leader.as_ref().map(|h| created(h.as_raw_handle()));
        let mut leader_exit: Option<(u128, u32)> = None;
        let mut seen_processes: BTreeMap<u32, Value> = BTreeMap::new();
        let mut after_exit_snapshot: Option<Value> = None;
        let mut page_ms = None;
        let mut beacon_ms = None;
        let mut last_scan = Instant::now() - Duration::from_secs(1);
        let job_handle = match &running {
            Running::Job(job, _) => Some(job.0.as_raw_handle() as usize),
            _ => None,
        };
        loop {
            let elapsed = t0.elapsed();
            match &mut running {
                Running::Owner(owner, child) => {
                    let _ = owner.poll(child);
                }
                Running::Session(session) => {
                    session.poll();
                }
                _ => {}
            }
            if leader_exit.is_none() {
                if let Some(h) = &leader {
                    if let Some(code) = exit_code(h.as_raw_handle()) {
                        leader_exit = Some((elapsed.as_millis(), code));
                    }
                }
            }
            for (ms, target) in run.seen.lock().unwrap()[before..].iter() {
                let since = ms.saturating_sub(t0.duration_since(run.started).as_millis());
                if page_ms.is_none() && target.starts_with(&format!("/diag/{tag}/")) {
                    page_ms = Some(since);
                }
                if beacon_ms.is_none() && target.starts_with(&format!("/beacon?tag={tag}&")) {
                    beacon_ms = Some(since);
                }
            }
            if last_scan.elapsed() >= Duration::from_millis(500) {
                last_scan = Instant::now();
                let found = scan(pid, leader_created, &exe_name, &marker, job_handle);
                for p in &found {
                    let id = p.pid;
                    seen_processes.entry(id).or_insert_with(|| {
                        let mut v = p.to_json();
                        v["first_seen_ms"] = json!(elapsed.as_millis());
                        v
                    });
                }
                if after_exit_snapshot.is_none() {
                    if let Some((exit_ms, _)) = leader_exit {
                        if elapsed.as_millis() >= exit_ms + 1500 {
                            after_exit_snapshot =
                                Some(json!(found.iter().map(Proc::to_json).collect::<Vec<_>>()));
                        }
                    }
                }
            }
            if (page_ms.is_some() && beacon_ms.is_some()) || elapsed > run.timeout {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        out["page_requested"] = json!(page_ms.is_some());
        out["page_ms"] = json!(page_ms);
        out["javascript_beacon"] = json!(beacon_ms.is_some());
        out["beacon_ms"] = json!(beacon_ms);
        out["loaded"] = json!(page_ms.is_some() && beacon_ms.is_some());
        out["leader_exit_ms"] = json!(leader_exit.map(|e| e.0));
        out["leader_exit_code"] = json!(leader_exit.map(|e| e.1));
        out["after_leader_exit_plus_1500ms"] = after_exit_snapshot.unwrap_or(Value::Null);
        let alive = scan(pid, leader_created, &exe_name, &marker, job_handle);
        out["alive_at_end"] = json!(alive.iter().map(Proc::to_json).collect::<Vec<_>>());
        let seen_list: Vec<Value> = seen_processes.into_values().collect();
        out["browser_processes_besides_leader"] = json!(seen_list
            .iter()
            .filter(|p| p["pid"] != json!(pid) && p["type"].is_null())
            .count());
        out["processes_seen"] = json!(seen_list);
        if let Running::Session(session) = &mut running {
            session.poll();
            out["session_running_before_stop"] = json!(session.is_running());
            out["session_status_before_stop"] = json!(format!("{:?}", session.status));
        }

        // Cleanup: the mode's own stop first, then every process found.
        let stop_started = Instant::now();
        match &mut running {
            Running::Child(child) => {
                let _ = child.kill();
                let _ = child.wait();
            }
            Running::Owner(owner, child) => owner.stop(child),
            Running::Job(job, child) => {
                job.terminate();
                let _ = child.wait();
            }
            Running::Session(session) => {
                session.stop();
                let deadline = Instant::now() + Duration::from_secs(15);
                while session.is_running() && Instant::now() < deadline {
                    session.poll();
                    std::thread::sleep(Duration::from_millis(50));
                }
                out["session_stopped"] = json!(!session.is_running());
                out["session_status"] = json!(format!("{:?}", session.status));
                out["session_output_tail"] = json!(tail(&session.text(), 12));
            }
        }
        out["mode_stop_ms"] = json!(stop_started.elapsed().as_millis());
        std::thread::sleep(Duration::from_millis(300));
        let survivors = scan(pid, leader_created, &exe_name, &marker, job_handle);
        out["survived_mode_stop"] = json!(survivors.iter().map(Proc::to_json).collect::<Vec<_>>());
        for p in &survivors {
            if let Some(h) = &p.handle {
                unsafe { TerminateProcess(h.as_raw_handle(), 1) };
            }
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut left = survivors;
        while !left.is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(200));
            left = scan(pid, leader_created, &exe_name, &marker, job_handle);
            for p in &left {
                if let Some(h) = &p.handle {
                    unsafe { TerminateProcess(h.as_raw_handle(), 1) };
                }
            }
        }
        out["leftovers"] = json!(left.iter().map(Proc::to_json).collect::<Vec<_>>());
        drop(running);
        out["chrome_debug_log_tail"] = json!(std::fs::read(profile.join("chrome_debug.log"))
            .map(|b| tail(&String::from_utf8_lossy(&b), 60))
            .unwrap_or_else(|e| vec![format!("<no chrome_debug.log: {e}>")]));
        out
    }

    fn spawn(run: &Run, args: &[String]) -> Result<Running, String> {
        let plain = |extra: &[(&str, &str)]| -> Command {
            let mut command = Command::new(run.program);
            command
                .args(args)
                .current_dir(run.work)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            for (k, v) in extra {
                command.env(k, v);
            }
            command
        };
        match run.mode {
            Mode::Plain | Mode::PlainNoDeElevate | Mode::PlainTermEnv => {
                let extra: &[(&str, &str)] = if run.mode == Mode::PlainTermEnv {
                    &[("TERM", "xterm-256color"), ("COLORTERM", "truecolor")]
                } else {
                    &[]
                };
                let mut command = plain(extra);
                command.creation_flags(CREATE_NO_WINDOW);
                command
                    .spawn()
                    .map(Running::Child)
                    .map_err(|e| e.to_string())
            }
            Mode::OwnerJob => {
                let mut command = plain(&[]);
                ProcessOwner::prepare(&mut command);
                let mut child = command.spawn().map_err(|e| e.to_string())?;
                match ProcessOwner::attach(&mut child) {
                    Ok(owner) => Ok(Running::Owner(owner, child)),
                    Err(e) => {
                        let _ = child.kill();
                        Err(format!("attach: {e}"))
                    }
                }
            }
            Mode::JobKeep | Mode::JobBreakaway => {
                let mut command = plain(&[]);
                command
                    .creation_flags(CREATE_SUSPENDED | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
                let mut child = command.spawn().map_err(|e| e.to_string())?;
                match Job::attach(&child, run.mode == Mode::JobBreakaway) {
                    Ok(job) => Ok(Running::Job(job, child)),
                    Err(e) => {
                        let _ = child.kill();
                        Err(format!("job: {e}"))
                    }
                }
            }
            Mode::RunSession
            | Mode::RunSessionConpty
            | Mode::Launcher
            | Mode::LauncherNoDeElevate => {
                let cfg = rustpy_project::RunConfig {
                    name: format!("Browser launcher: {} ({})", run.browser, run.mode_name),
                    kind: rustpy_project::RunConfigKind::Program {
                        program: run.program.to_path_buf(),
                        args: args.to_vec(),
                    },
                    working_dir: Some(run.work.to_path_buf()),
                    temporary: true,
                    add_content_roots: false,
                    add_source_roots: false,
                    emulate_terminal: run.mode == Mode::RunSessionConpty,
                    ..Default::default()
                };
                let session = if matches!(run.mode, Mode::Launcher | Mode::LauncherNoDeElevate) {
                    rustpy_run::RunSession::spawn_launcher(1, &cfg, run.work)
                } else {
                    rustpy_run::RunSession::spawn(1, &cfg, None, run.work)
                };
                if let rustpy_run::SessionStatus::Failed(e) = &session.status {
                    return Err(e.clone());
                }
                Ok(Running::Session(Box::new(session)))
            }
        }
    }

    struct Job(OwnedHandle);
    impl Job {
        fn attach(child: &Child, breakaway: bool) -> std::io::Result<Self> {
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return Err(std::io::Error::last_os_error());
                }
                let job = Self(OwnedHandle::from_raw_handle(handle));
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
                    || AssignProcessToJobObject(handle, child.as_raw_handle()) == 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                resume(child.id())?;
                Ok(job)
            }
        }
        fn terminate(&self) {
            unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) };
        }
    }

    fn resume(pid: u32) -> std::io::Result<()> {
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return Err(std::io::Error::last_os_error());
            }
            let snapshot = OwnedHandle::from_raw_handle(snapshot);
            let mut thread: THREADENTRY32 = std::mem::zeroed();
            thread.dwSize = std::mem::size_of_val(&thread) as u32;
            let mut present = Thread32First(snapshot.as_raw_handle(), &mut thread);
            while present != 0 {
                if thread.th32OwnerProcessID == pid {
                    let handle = OpenThread(THREAD_SUSPEND_RESUME, 0, thread.th32ThreadID);
                    if !handle.is_null() {
                        let handle = OwnedHandle::from_raw_handle(handle);
                        ResumeThread(handle.as_raw_handle());
                        return Ok(());
                    }
                }
                present = Thread32Next(snapshot.as_raw_handle(), &mut thread);
            }
        }
        Err(std::io::Error::other("suspended thread not found"))
    }

    struct Proc {
        pid: u32,
        ppid: u32,
        exe: String,
        how: &'static str,
        command_line: Option<String>,
        in_any_job: Option<bool>,
        in_mode_job: Option<bool>,
        handle: Option<OwnedHandle>,
    }
    impl Proc {
        fn to_json(&self) -> Value {
            let cl = self.command_line.as_deref().unwrap_or_default();
            let kind = cl
                .split_whitespace()
                .find_map(|a| a.trim_matches('"').strip_prefix("--type="))
                .map(str::to_string);
            json!({
                "pid": self.pid,
                "ppid": self.ppid,
                "exe": self.exe,
                "found_by": self.how,
                "type": kind,
                "in_any_job": self.in_any_job,
                "in_mode_job": self.in_mode_job,
                "command_line": self.command_line.as_ref().map(|c| c.chars().take(400).collect::<String>()),
            })
        }
    }

    /// Processes of this run: the leader's descendants by parent id (created no
    /// earlier than the leader) and every process of the browser's executable
    /// whose command line names this run's profile folder (a hand-off that
    /// escaped the tree, e.g. a relaunch through Explorer).
    fn scan(
        leader: Option<u32>,
        leader_created: Option<u64>,
        exe_name: &str,
        marker: &(String, String),
        job: Option<usize>,
    ) -> Vec<Proc> {
        let mut entries = Vec::new();
        unsafe {
            let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snapshot == INVALID_HANDLE_VALUE {
                return Vec::new();
            }
            let snapshot = OwnedHandle::from_raw_handle(snapshot);
            let mut entry: PROCESSENTRY32W = std::mem::zeroed();
            entry.dwSize = std::mem::size_of_val(&entry) as u32;
            let mut present = Process32FirstW(snapshot.as_raw_handle(), &mut entry);
            while present != 0 {
                let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(260);
                entries.push((
                    entry.th32ProcessID,
                    entry.th32ParentProcessID,
                    String::from_utf16_lossy(&entry.szExeFile[..len]),
                ));
                present = Process32NextW(snapshot.as_raw_handle(), &mut entry);
            }
        }
        let mut found: Vec<Proc> = Vec::new();
        let mut tree: Vec<u32> = Vec::new();
        if let Some(leader) = leader {
            // An exited leader is no longer listed; its children still name it.
            if let Some((pid, ppid, exe)) = entries.iter().find(|e| e.0 == leader) {
                if let Some(handle) = open(*pid, true) {
                    found.push(describe(*pid, *ppid, exe, "leader", handle, job));
                }
            }
            tree.push(leader);
        }
        let not_before = leader_created.unwrap_or(0);
        loop {
            let mut added = false;
            for (pid, ppid, exe) in &entries {
                if tree.contains(pid) || !tree.contains(ppid) {
                    continue;
                }
                let Some(handle) = open(*pid, true) else {
                    continue;
                };
                // A recycled parent id: older than the leader, not ours.
                if created(handle.as_raw_handle()) < not_before {
                    continue;
                }
                found.push(describe(*pid, *ppid, exe, "descendant", handle, job));
                tree.push(*pid);
                added = true;
            }
            if !added {
                break;
            }
        }
        for (pid, ppid, exe) in &entries {
            if found.iter().any(|p| p.pid == *pid) || !exe.eq_ignore_ascii_case(exe_name) {
                continue;
            }
            let Some(handle) = open(*pid, true) else {
                continue;
            };
            let Some(cl) = command_line(handle.as_raw_handle()) else {
                continue;
            };
            let lower = cl.to_ascii_lowercase();
            if lower.contains(&marker.0) && lower.contains(&marker.1) {
                found.push(describe(*pid, *ppid, exe, "profile_folder", handle, job));
            }
        }
        found
    }

    fn describe(
        pid: u32,
        ppid: u32,
        exe: &str,
        how: &'static str,
        handle: OwnedHandle,
        job: Option<usize>,
    ) -> Proc {
        let raw = handle.as_raw_handle();
        Proc {
            pid,
            ppid,
            exe: exe.to_string(),
            how,
            command_line: command_line(raw),
            in_any_job: in_job(raw, std::ptr::null_mut()),
            in_mode_job: job.and_then(|j| in_job(raw, j as HANDLE)),
            handle: Some(handle),
        }
    }

    fn open(pid: u32, terminate: bool) -> Option<OwnedHandle> {
        let mut access = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
        if terminate {
            access |= PROCESS_TERMINATE;
        }
        let mut handle = unsafe { OpenProcess(access, 0, pid) };
        if handle.is_null() && terminate {
            handle = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                    0,
                    pid,
                )
            };
        }
        (!handle.is_null()).then(|| unsafe { OwnedHandle::from_raw_handle(handle) })
    }

    fn created(handle: HANDLE) -> u64 {
        let mut times: [FILETIME; 4] = unsafe { std::mem::zeroed() };
        let [a, b, c, d] = &mut times;
        if unsafe { GetProcessTimes(handle, a, b, c, d) } == 0 {
            return 0;
        }
        (u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime)
    }

    fn exit_code(handle: HANDLE) -> Option<u32> {
        if unsafe { WaitForSingleObject(handle, 0) } != WAIT_OBJECT_0 {
            return None;
        }
        let mut code = 0;
        (unsafe { GetExitCodeProcess(handle, &mut code) } != 0).then_some(code)
    }

    fn in_job(handle: HANDLE, job: HANDLE) -> Option<bool> {
        let mut result = 0;
        (unsafe { IsProcessInJob(handle, job, &mut result) } != 0).then_some(result != 0)
    }

    fn command_line(handle: HANDLE) -> Option<String> {
        let mut buffer = vec![0u64; 8192];
        let mut length = 0u32;
        let status = unsafe {
            NtQueryInformationProcess(
                handle,
                ProcessCommandLineInformation,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * 8) as u32,
                &mut length,
            )
        };
        if status < 0 {
            return None;
        }
        let text = unsafe { &*(buffer.as_ptr() as *const UNICODE_STRING) };
        if text.Buffer.is_null() {
            return None;
        }
        let start = buffer.as_ptr() as usize;
        let end = start + buffer.len() * 8;
        let data = text.Buffer as usize;
        if data < start || data + text.Length as usize > end {
            return None;
        }
        let units = unsafe { std::slice::from_raw_parts(text.Buffer, text.Length as usize / 2) };
        Some(String::from_utf16_lossy(units))
    }

    fn elevation() -> Value {
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return json!({"error": std::io::Error::last_os_error().to_string()});
            }
            let token = OwnedHandle::from_raw_handle(token);
            let mut elevated: TOKEN_ELEVATION = std::mem::zeroed();
            let mut size = 0;
            let ok = GetTokenInformation(
                token.as_raw_handle(),
                TokenElevation,
                (&mut elevated as *mut TOKEN_ELEVATION).cast(),
                std::mem::size_of_val(&elevated) as u32,
                &mut size,
            ) != 0;
            let mut kind = 0i32;
            let kind_ok = GetTokenInformation(
                token.as_raw_handle(),
                TokenElevationType,
                (&mut kind as *mut i32).cast(),
                4,
                &mut size,
            ) != 0;
            json!({
                "elevated": ok.then_some(elevated.TokenIsElevated != 0),
                "elevation_type": kind_ok.then_some(match kind {
                    1 => "default (UAC off or standard user)",
                    2 => "full (elevated administrator)",
                    3 => "limited (filtered administrator)",
                    _ => "unknown",
                }),
            })
        }
    }

    fn own_job() -> Value {
        let Some(in_job) = in_job(unsafe { GetCurrentProcess() }, std::ptr::null_mut()) else {
            return json!({"error": std::io::Error::last_os_error().to_string()});
        };
        if !in_job {
            return json!({"in_job": false});
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            QueryInformationJobObject(
                std::ptr::null_mut(),
                JobObjectExtendedLimitInformation,
                (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
                std::ptr::null_mut(),
            )
        } != 0;
        let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
        let counted = unsafe {
            QueryInformationJobObject(
                std::ptr::null_mut(),
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                std::mem::size_of_val(&accounting) as u32,
                std::ptr::null_mut(),
            )
        } != 0;
        let flags = limits.BasicLimitInformation.LimitFlags;
        json!({
            "in_job": true,
            "limit_flags": ok.then(|| format!("{flags:#x}")),
            "breakaway_ok": ok.then_some(flags & JOB_OBJECT_LIMIT_BREAKAWAY_OK != 0),
            "silent_breakaway_ok": ok.then_some(flags & JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK != 0),
            "kill_on_job_close": ok.then_some(flags & JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE != 0),
            "active_processes": counted.then_some(accounting.ActiveProcesses),
        })
    }

    fn summary(runs: &[Value]) -> Value {
        let mut out = serde_json::Map::new();
        let mut by_browser: BTreeMap<String, Vec<(String, bool)>> = BTreeMap::new();
        for run in runs {
            let Some(mode) = run["mode"].as_str() else {
                if let Some(reason) = run["skipped"].as_str() {
                    out.insert(
                        run["browser"].as_str().unwrap_or_default().to_string(),
                        json!({"skipped": reason}),
                    );
                }
                continue;
            };
            by_browser
                .entry(run["browser"].as_str().unwrap_or_default().to_string())
                .or_default()
                .push((mode.to_string(), run["loaded"].as_bool().unwrap_or(false)));
        }
        for (browser, results) in by_browser {
            let ok = |name: &str| results.iter().find(|r| r.0 == name).map(|r| r.1);
            let mut notes = Vec::new();
            if ok("plain") == Some(false) && ok("plain_no_de_elevate") == Some(true) {
                notes.push("only --do-not-de-elevate loads: the browser relaunches itself de-elevated (the helper runs elevated)");
            }
            if ok("plain") == Some(false) && ok("plain_no_de_elevate") != Some(true) {
                notes.push("even a plain launch without any job fails: see chrome_debug_log_tail and elevation");
            }
            if ok("run_session") == Some(false) && ok("job_keep") == Some(true) {
                notes.push("the browser hands off to processes started by its first process; ending the tree when that leader exits (the old Program run) kills the real browser");
            }
            if ok("job_keep") == Some(false) && ok("job_breakaway") == Some(true) {
                notes.push(
                    "the browser needs to break away from the job (CREATE_BREAKAWAY_FROM_JOB)",
                );
            }
            if ok("launcher") == Some(true) {
                notes.push("the fixed launcher path (Open in browser / Open URL) loads the page");
            } else if ok("launcher") == Some(false) {
                notes.push("the fixed launcher path still fails");
            }
            out.insert(
                browser,
                json!({
                    "loaded": results.iter().map(|(m, l)| (m.clone(), json!(l))).collect::<serde_json::Map<_, _>>(),
                    "notes": notes,
                }),
            );
        }
        Value::Object(out)
    }

    fn tail(text: &str, n: usize) -> Vec<String> {
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(n)..]
            .iter()
            .map(|l| l.to_string())
            .collect()
    }

    fn answer(mut stream: TcpStream, seen: &Seen, started: Instant, port: u16) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        while !buffer.windows(4).any(|w| w == b"\r\n\r\n") && buffer.len() < 65536 {
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            }
        }
        let text = String::from_utf8_lossy(&buffer);
        let target = text
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or_default()
            .to_string();
        if target.is_empty() {
            return;
        }
        seen.lock()
            .unwrap()
            .push((started.elapsed().as_millis(), target.clone()));
        let (kind, body) = match target.strip_prefix("/diag/").and_then(|t| t.split_once('/')) {
            Some((tag, _)) => (
                "text/html; charset=utf-8",
                format!(
                    "<!doctype html><html><head><meta charset=\"utf-8\"><title>RustPy Chromium diagnostic</title></head>\
                     <body><p>RustPy Chromium diagnostic</p><script>\
                     new Image().src = 'http://127.0.0.1:{port}/beacon?tag={tag}&js=' + (6 * 7) + '&t=' + Date.now();\
                     </script></body></html>"
                ),
            ),
            None => ("text/plain", "ok".to_string()),
        };
        let _ = stream.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        );
    }
}

fn main() {
    #[cfg(windows)]
    win::main();
    #[cfg(not(windows))]
    {
        eprintln!("windows_chromium_diagnostic runs on Windows only");
        std::process::exit(2);
    }
}
