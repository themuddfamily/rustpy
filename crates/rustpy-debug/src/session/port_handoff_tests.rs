//! Deterministic external-process theft of a handed-off CLI adapter port.
//! This adds no production retry, startup hook or changed existing deadline.
use super::*;
use std::io::{BufRead, BufReader};
use std::process::Command;

struct ExternalListener(std::process::Child);

impl Drop for ExternalListener {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn steal_handed_off_port(python: &Path, port: u16) -> ExternalListener {
    // A separate process, not another DebugPortLease: the process-local registry
    // cannot protect this bind. Readiness is emitted only after bind + listen.
    let mut child = ExternalListener(
        Command::new(python)
            .args([
                "-u",
                "-c",
                "import os,socket,sys,time; s=socket.socket(); s.bind(('127.0.0.1',int(sys.argv[1]))); s.listen(); print(str(os.getpid())+' '+str(s.getsockname()[1]),flush=True); time.sleep(120)",
                &port.to_string(),
            ])
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn fixture-owned external listener"),
    );
    // No inherited terminal or input: close the actual parent pipe immediately.
    drop(child.0.stdin.take());
    let stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send((result, line));
    });
    let (read, line) = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("external listener must report actual bound endpoint");
    assert!(read.unwrap() > 0, "external bind failed before readiness");
    assert_eq!(line.trim(), format!("{} {port}", child.0.id()));
    assert!(child.0.try_wait().unwrap().is_none());
    child
}

fn wait_for(session: &mut DebugSession, predicate: impl Fn(&DebugSession) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        session.poll();
        if predicate(session) {
            return true;
        }
        if matches!(session.state, DebugState::Failed(_)) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn external_listener_after_handoff_cannot_break_owned_debug_session() {
    let Some(python) = which::which("python3").ok() else {
        eprintln!("python3 missing; skipping debugpy integration fixture");
        return;
    };
    let mut probe = Command::new(&python)
        .args(["-c", "import debugpy"])
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn debugpy interpreter probe");
    drop(probe.stdin.take());
    let available = probe.wait_with_output().expect("probe debugpy interpreter");
    if !available.status.success() {
        eprintln!("debugpy not installed; skipping integration fixture");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("owned.py");
    let executed = directory.path().join("executed.txt");
    std::fs::write(
        &script,
        format!(
            "from pathlib import Path\nmarker = 'owned-debuggee'\noutput = Path({:?}).open('a')\noutput.write(marker + '\\n')\noutput.close()\nprint(marker)\n",
            executed.to_string_lossy()
        ),
    )
    .unwrap();
    let mut config = RunConfig::for_script(&script);
    config.interpreter = Some(python.clone());
    let mut lease = DebugPortLease::acquire().unwrap();
    let candidate = lease.port();
    assert!(std::net::TcpListener::bind(("127.0.0.1", candidate)).is_err());
    lease.hand_off();
    let mut external = steal_handed_off_port(&python, candidate);
    assert!(crate::port::is_leased(candidate));
    let mut breakpoints = HashMap::new();
    breakpoints.insert(script.clone(), vec![Breakpoint::new(3)]);
    let mut session = DebugSession::launch_on_port(
        9201,
        &config,
        None,
        directory.path(),
        LaunchSetup {
            breakpoints,
            ..Default::default()
        },
        lease,
    )
    .expect("launch debugger with an externally occupied released candidate");
    assert!(
        wait_for(&mut session, |s| s.is_stopped()
            && s.frames.first().is_some_and(|f| f.line == 3)),
        "external PID={} port={candidate}; state={:?}; output={:?}",
        external.0.id(),
        session.state,
        session.output
    );
    assert!(
        !executed.exists(),
        "wait-for-client and breakpoint must precede script side effects"
    );
    assert!(
        external.0.try_wait().unwrap().is_none(),
        "unrelated owner must not be killed"
    );
    session.continue_();
    let terminated = wait_for(&mut session, |s| {
        matches!(s.state, DebugState::Terminated(Some(0)))
    });
    if !terminated {
        let child_status = session.child.as_mut().map(|child| child.try_wait());
        eprintln!(
            "post-Continue diagnostic: external_pid={} candidate_port={candidate} state={:?} frames={:?} pending={:?} errors={:?} output={:?} marker_bytes={:?} child_status={child_status:?}",
            external.0.id(),
            session.state,
            session.frames,
            session.pending,
            session.errors,
            session.output,
            std::fs::read(&executed),
        );
    }
    assert!(terminated);
    assert_eq!(
        std::fs::read_to_string(&executed).unwrap(),
        "owned-debuggee\n",
        "debuggee must run exactly once"
    );
    assert!(external.0.try_wait().unwrap().is_none());
    drop(session);
    assert!(!crate::port::is_leased(candidate));
}
