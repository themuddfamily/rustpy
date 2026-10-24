//! Explicitly admitted Linux acceptance of real local/managed Jupyter transport.
//! Run serially with --ignored --exact and a provisioned interpreter/kernelspec.
//! This checks captured child identities, not an unavailable reader-join API.
use rustpy_notebook::{Cell, CellRunner, Connection, ConsoleEvent, KernelState, Notebook, Output};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

const LIMIT: Duration = Duration::from_secs(45);

#[derive(Clone, Debug)]
struct Identity {
    pid: u32,
    start: String,
    parent: u32,
}
fn identity(pid: u32) -> Option<Identity> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<_> = stat.rsplit_once(") ")?.1.split_whitespace().collect();
    Some(Identity {
        pid,
        parent: fields.get(1)?.parse().ok()?,
        start: fields.get(19)?.to_string(),
    })
}
fn present(owner: &Identity) -> bool {
    identity(owner.pid).is_some_and(|current| current.start == owner.start)
}
fn ancestry(pid: u32, managed: bool) -> Vec<Identity> {
    let mut result = Vec::new();
    let mut next = pid;
    for _ in 0..6 {
        let owner = identity(next).expect("captured Jupyter ancestor must still exist");
        let command = std::fs::read(format!("/proc/{next}/cmdline")).unwrap();
        let command = String::from_utf8_lossy(&command);
        next = owner.parent;
        result.push(owner);
        if command.contains("Explicit Jupyter transport.") {
            assert!(result.len() >= if managed { 3 } else { 2 });
            if managed {
                let server = &result[1];
                let command = std::fs::read(format!("/proc/{}/cmdline", server.pid)).unwrap();
                assert!(String::from_utf8_lossy(&command).contains("jupyter_server"));
            }
            return result;
        }
    }
    panic!("kernel ancestry did not reach the owned RustPy Jupyter bridge: {result:?}");
}

struct Fixture {
    runner: CellRunner,
    notebook: Notebook,
    _directory: tempfile::TempDir,
    events: Vec<String>,
    debugger_endpoints: Vec<(String, u16)>,
    adopted_adapter: Option<Identity>,
    owners: Vec<Identity>,
    closing: bool,
}
impl Fixture {
    fn start(kind: &str) -> Self {
        Self::start_configured(kind, false)
    }
    fn start_configured(kind: &str, debug_logs: bool) -> Self {
        assert_eq!(
            std::env::var("RUSTPY_JUPYTER_TEST_ADMITTED").as_deref(),
            Ok("1"),
            "root must explicitly admit real configured-kernel execution"
        );
        assert!(
            identity(std::process::id()).is_some(),
            "this physical identity acceptance requires Linux /proc"
        );
        let python = PathBuf::from(
            std::env::var_os("RUSTPY_JUPYTER_TEST_PYTHON")
                .expect("set the absolute provisioned Jupyter Python path"),
        );
        assert!(python.is_absolute() && python.is_file());
        let kernel = std::env::var("RUSTPY_JUPYTER_TEST_KERNEL")
            .expect("set the explicitly provisioned kernelspec name");
        assert!(
            !kernel.is_empty() && kernel != "project",
            "use an actual registered kernelspec"
        );
        let directory = tempfile::tempdir().unwrap();
        for name in ["jupyter-config", "jupyter-runtime", "matplotlib"] {
            std::fs::create_dir_all(directory.path().join(name)).unwrap();
        }
        let mut env = vec![
            (
                "JUPYTER_CONFIG_DIR".into(),
                directory
                    .path()
                    .join("jupyter-config")
                    .to_string_lossy()
                    .into_owned(),
            ),
            (
                "JUPYTER_RUNTIME_DIR".into(),
                directory
                    .path()
                    .join("jupyter-runtime")
                    .to_string_lossy()
                    .into_owned(),
            ),
            (
                "MPLCONFIGDIR".into(),
                directory
                    .path()
                    .join("matplotlib")
                    .to_string_lossy()
                    .into_owned(),
            ),
            ("PYTHONNOUSERSITE".into(), "1".into()),
            ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
            ("MPLBACKEND".into(), "Agg".into()),
        ];
        if debug_logs {
            let logs = directory.path().join("debugpy-owned-logs");
            std::fs::create_dir(&logs).unwrap();
            env.push((
                "DEBUGPY_LOG_DIR".into(),
                logs.to_string_lossy().into_owned(),
            ));
        }
        let runner = CellRunner::start_with_connection(
            &python,
            directory.path(),
            &env,
            "",
            Connection {
                kind: kind.into(),
                kernel,
                ..Default::default()
            },
        )
        .expect("actual configured Jupyter transport must start; dependencies may not be skipped");
        let mut fixture = Self {
            runner,
            notebook: Notebook::empty(),
            _directory: directory,
            events: Vec::new(),
            debugger_endpoints: Vec::new(),
            adopted_adapter: None,
            owners: Vec::new(),
            closing: false,
        };
        fixture.until(|this| this.runner.state() == KernelState::Idle);
        let executable = fixture
            .runner
            .kernel_executable()
            .expect("kernel reports its interpreter");
        assert_eq!(
            std::fs::canonicalize(executable).unwrap(),
            std::fs::canonicalize(python).unwrap(),
            "the registered kernel must use the admitted interpreter"
        );
        if kind == "managed" {
            let (url, token) = fixture
                .runner
                .server_info
                .as_ref()
                .expect("managed server endpoint");
            assert!(url.starts_with("http://127.0.0.1:") && !token.is_empty());
        }
        fixture
    }
    fn poll(&mut self) {
        self.runner.poll(&mut self.notebook);
        for event in self.runner.take_events() {
            match event {
                ConsoleEvent::Debugger {
                    host, port, error, ..
                } => {
                    assert!(error.is_none(), "actual kernel debugger failed: {error:?}");
                    self.debugger_endpoints.push((host, port));
                }
                ConsoleEvent::Failed(error) => panic!("configured transport failed: {error}"),
                ConsoleEvent::Stderr(text) => self.events.push(text),
                other => self.events.push(format!("{other:?}")),
            }
        }
    }
    fn until(&mut self, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + LIMIT;
        while !done(self) {
            self.poll();
            assert!(
                self.runner.state() != KernelState::Dead,
                "kernel died: {:?}",
                self.events
            );
            assert!(
                Instant::now() < deadline,
                "configured kernel timeout: {:?}; {:?}",
                self.events,
                self.notebook.cells
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn execute(&mut self, index: usize) {
        assert!(self.runner.run_cell(&mut self.notebook, index));
        self.until(|this| !this.runner.is_busy());
        assert!(
            !self.notebook.cells[index]
                .outputs
                .iter()
                .any(|output| matches!(output, Output::Error { .. })),
            "cell failed: {:?}",
            self.notebook.cells[index].outputs
        );
    }
    fn text(&self, index: usize) -> String {
        self.notebook.cells[index]
            .outputs
            .iter()
            .filter_map(|output| match output {
                Output::Stream { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
    fn capture(&mut self, index: usize, managed: bool) -> String {
        let text = self.text(index);
        let record = text
            .lines()
            .find_map(|line| line.strip_prefix("RUSTPY_IDENTITY "))
            .expect("real kernel identity output");
        let record: serde_json::Value = serde_json::from_str(record).unwrap();
        let owners = ancestry(record["pid"].as_u64().unwrap().try_into().unwrap(), managed);
        self.owners.extend(owners);
        record["session"].as_str().unwrap().to_owned()
    }
    fn retire(&mut self) {
        self.closing = true;
        self.runner.shutdown();
        let deadline = Instant::now() + LIMIT;
        while self.runner.owns_process() || self.owners.iter().any(present) {
            self.runner.poll(&mut self.notebook);
            self.runner.take_events();
            self.reap_adapter()
                .expect("reap positively captured adapter");
            assert!(
                Instant::now() < deadline,
                "owned transport/children did not physically retire: {:?}",
                self.owners
                    .iter()
                    .filter(|owner| present(owner))
                    .collect::<Vec<_>>()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!self.runner.owns_process());
    }
    fn reap_adapter(&self) -> std::io::Result<()> {
        if let Some(adapter) = self.adopted_adapter.as_ref().filter(|owner| present(owner)) {
            // Only the exact logged adapter can be adopted by this fixture.
            // No wildcard waits or signals against guessed daemon PIDs.
            let result =
                unsafe { libc::waitpid(adapter.pid as i32, std::ptr::null_mut(), libc::WNOHANG) };
            if result < 0 {
                let error = std::io::Error::last_os_error();
                if !matches!(error.raw_os_error(), Some(libc::ECHILD | libc::EINTR)) {
                    return Err(error);
                }
            }
        }
        Ok(())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !self.closing {
            // Failure cleanup uses the same retained supervisor; never signal
            // captured PIDs directly, which could now belong to another owner.
            self.runner.shutdown();
            let deadline = Instant::now() + LIMIT;
            while (self.runner.owns_process() || self.adopted_adapter.as_ref().is_some_and(present))
                && Instant::now() < deadline
            {
                self.runner.poll(&mut self.notebook);
                self.runner.take_events();
                let _ = self.reap_adapter();
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
const IDENTITY: &str = "import json, os\nprint('RUSTPY_IDENTITY ' + json.dumps({'pid': os.getpid(), 'session': get_ipython().kernel.session.session}))\n";
fn acceptance(kind: &str) {
    let managed = kind == "managed";
    let mut fixture = Fixture::start(kind);
    fixture.notebook.cells = vec![
        Cell::code(format!(
            "{IDENTITY}from IPython.display import display\nowner_token = 'π-owned'\ndisplay({{'text/plain': 'first π'}}, raw=True, metadata={{'phase': 1}}, display_id='owned-display')"
        )),
        Cell::code(
            "from IPython.display import update_display\nupdate_display({'text/plain': 'updated 😀'}, raw=True, metadata={'phase': 2}, display_id='owned-display')",
        ),
        Cell::code("import time\nprint('interrupt-ready', flush=True)\ntime.sleep(120)"),
        Cell::code("print('QUEUED_CELL_MUST_NOT_RUN')"),
    ];
    fixture.execute(0);
    let session = fixture.capture(0, managed);
    let first_kernel = fixture.owners[0].clone();
    let cell_id = fixture.notebook.cells[0].id.clone().unwrap();
    assert!(fixture.notebook.cells[0]
        .outputs
        .iter()
        .any(|output| matches!(output,
        Output::DisplayData { data, metadata, .. }
        if data.get("text/plain").is_some_and(|text| text == "first π")
            && metadata.get("phase") == Some(&serde_json::json!(1))
            && metadata.get("rustpy_cell_id") == Some(&serde_json::json!(cell_id)))));
    fixture.execute(1);
    assert!(fixture.notebook.cells[0]
        .outputs
        .iter()
        .any(|output| matches!(output,
        Output::DisplayData { data, metadata, .. }
        if data.get("text/plain").is_some_and(|text| text == "updated 😀")
            && metadata.get("phase") == Some(&serde_json::json!(2)))));
    assert!(
        !fixture.notebook.cells[1]
            .outputs
            .iter()
            .any(|output| matches!(output, Output::DisplayData { .. })),
        "display_id update must mutate the original cell, not the current cell"
    );
    fixture.runner.run_cells(&mut fixture.notebook, [2, 3]);
    fixture.until(|this| this.text(2).contains("interrupt-ready"));
    fixture.runner.interrupt();
    fixture.until(|this| !this.runner.is_busy());
    assert!(fixture.notebook.cells[2]
        .outputs
        .iter()
        .any(|output| matches!(output,
        Output::Error { ename, .. } if ename == "KeyboardInterrupt")));
    assert!(fixture.notebook.cells[3].outputs.is_empty());
    fixture.runner.restart();
    fixture.until(|this| this.runner.state() == KernelState::Idle);
    fixture.notebook.cells[0].source =
        format!("{IDENTITY}print('OLD_NAMESPACE ' + str('owner_token' in globals()))");
    fixture.execute(0);
    let restarted = fixture.capture(0, managed);
    assert_ne!(
        session, restarted,
        "restart must replace the actual kernel session"
    );
    assert!(
        !present(&first_kernel),
        "old kernel identity must be physically retired"
    );
    assert!(fixture.text(0).contains("OLD_NAMESPACE False"));
    assert_eq!(fixture.notebook.cells[0].execution_count, Some(1));
    fixture.retire();
}

#[test]
#[ignore = "requires explicit admitted Python, registered kernelspec, Jupyter dependencies and Linux /proc"]
fn configured_local_kernel_session_roundtrip_and_retirement() {
    acceptance("local");
}
#[test]
#[ignore = "requires explicit admitted Python, registered kernelspec, managed-server dependencies and Linux /proc"]
fn configured_managed_kernel_session_roundtrip_and_retirement() {
    acceptance("managed");
}

#[test]
#[cfg(target_os = "linux")]
#[ignore = "requires explicit admitted real ipykernel/debugpy SDK and kernelspec; run alone on Linux"]
fn configured_local_debug_cell_hits_real_breakpoint_stack_continue_and_result() {
    use rustpy_debug::{Breakpoint, DebugOptions, DebugSession, DebugState};
    // debugpy daemonizes its adapter. Adopt only this fixture's descendants so
    // kernel shutdown can be followed by physical reaping even under PID 1
    // implementations that do not reap detached children.
    struct Subreaper(i32);
    impl Drop for Subreaper {
        fn drop(&mut self) {
            let result = unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, self.0) };
            if result != 0 && !std::thread::panicking() {
                panic!(
                    "cannot restore prior subreaper state: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
    }
    let mut previous = 0i32;
    assert_eq!(
        unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut previous as *mut i32) },
        0
    );
    assert_eq!(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) }, 0);
    let _subreaper = Subreaper(previous);
    let mut fixture = Fixture::start_configured("local", true);
    fixture.notebook.cells = vec![Cell::code(IDENTITY)];
    fixture.execute(0);
    fixture.capture(0, false);
    fixture.notebook.cells.push(Cell::code(
        "debug_cell_value = 41\ndebug_cell_value += 1\nprint('ACTUAL_DEBUG_CELL_RESULT', debug_cell_value)",
    ));
    fixture.notebook.ensure_cell_ids();
    let cell_id = fixture.notebook.cells[1].id.as_ref().unwrap().clone();
    let source = PathBuf::from(fixture.runner.debug_source_path(&cell_id));
    assert!(fixture.runner.debug_cell(&mut fixture.notebook, 1));
    fixture.until(|this| !this.debugger_endpoints.is_empty());
    assert_eq!(fixture.debugger_endpoints.len(), 1);
    let (host, port) = fixture.debugger_endpoints.pop().unwrap();
    let logs = fixture._directory.path().join("debugpy-owned-logs");
    let adapters: Vec<_> =
        std::fs::read_dir(&logs)
            .unwrap()
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name();
                let pid = name
                    .to_str()?
                    .strip_prefix("debugpy.adapter-")?
                    .strip_suffix(".log")?
                    .parse::<u32>()
                    .ok()?;
                let metadata = std::fs::symlink_metadata(entry.path()).unwrap();
                assert!(metadata.is_file() && metadata.len() <= 16 * 1024 * 1024);
                let text = std::fs::read_to_string(entry.path()).unwrap();
                assert!(
                    text.contains(&format!(
                        "Listening for incoming Client connections on {host}:{port}..."
                    )),
                    "private adapter log must identify the actual debugger endpoint"
                );
                let captured = identity(pid).expect("logged actual adapter must still be alive");
                let executable = std::fs::read_link(format!("/proc/{pid}/exe")).unwrap();
                assert_eq!(
                    executable.canonicalize().unwrap(),
                    std::fs::canonicalize(fixture.runner.kernel_executable().unwrap()).unwrap()
                );
                let command = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap();
                let arguments: Vec<_> = command
                    .split(|byte| *byte == 0)
                    .filter(|arg| !arg.is_empty())
                    .collect();
                assert!(arguments.iter().any(|arg| {
                    PathBuf::from(String::from_utf8_lossy(arg).into_owned())
                        .ends_with("debugpy/adapter")
                }));
                assert!(arguments.windows(2).any(|pair| pair[0] == b"--log-dir"
                    && pair[1] == logs.as_os_str().as_encoded_bytes()));
                assert!(
                    present(&captured),
                    "adapter identity changed during validation"
                );
                Some(captured)
            })
            .collect();
    assert_eq!(
        adapters.len(),
        1,
        "one actual private endpoint-owning adapter required"
    );
    let adapter = adapters.into_iter().next().unwrap();
    fixture.adopted_adapter = Some(adapter.clone());
    fixture.owners.push(adapter);
    let mut debug = DebugSession::attach_endpoint(
        &host,
        port,
        DebugOptions {
            just_my_code: false,
            ..Default::default()
        },
        [(source.clone(), vec![Breakpoint::new(2)])].into(),
    )
    .expect("actual public debugger attach to the notebook kernel");
    let deadline = Instant::now() + LIMIT;
    let mut continued_epoch = None;
    loop {
        debug.poll();
        fixture.poll();
        assert!(
            !matches!(
                debug.state,
                DebugState::Failed(_) | DebugState::Terminated(_)
            ),
            "debugger ended before real cell breakpoint: {:?}; {:?}",
            debug.state,
            fixture.events
        );
        if matches!(debug.state, DebugState::Stopped { .. }) && !debug.frames.is_empty() {
            if debug
                .frames
                .iter()
                .any(|frame| frame.path.as_ref() == Some(&source) && frame.line == 2)
            {
                break;
            }
            // debug_cell deliberately starts with debugpy.breakpoint() before
            // exec(compiled cell). Continue that real bootstrap suspension;
            // the cell-owned source/line breakpoint below remains mandatory.
            if continued_epoch != Some(debug.stop_epoch) {
                continued_epoch = Some(debug.stop_epoch);
                debug.continue_();
            }
        }
        assert!(
            Instant::now() < deadline,
            "no actual cell breakpoint/stack: {:?}; {:?}",
            debug.state,
            debug.frames
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(debug.breakpoints[&source]
        .iter()
        .any(|breakpoint| breakpoint.line == 2 && breakpoint.verified));
    assert!(
        !fixture.text(1).contains("ACTUAL_DEBUG_CELL_RESULT"),
        "cell result cannot precede Continue"
    );

    debug.continue_();
    let deadline = Instant::now() + LIMIT;
    while fixture.runner.is_busy() {
        debug.poll();
        fixture.poll();
        assert!(
            Instant::now() < deadline,
            "continued actual debug cell did not complete: {:?}; {:?}",
            debug.state,
            fixture.events
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(fixture.text(1).contains("ACTUAL_DEBUG_CELL_RESULT 42"));
    assert!(!fixture.notebook.cells[1]
        .outputs
        .iter()
        .any(|output| matches!(output, Output::Error { .. })));
    debug.stop();
    let deadline = Instant::now() + LIMIT;
    while !debug.disconnect_completed() {
        debug.poll();
        fixture.poll();
        assert!(
            Instant::now() < deadline,
            "actual debugger disconnect must finish"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(debug);
    fixture.retire();
}
