//! Final-stage launcher fixtures. Set RUSTPY_CLI_EXE to the built RustPy binary;
//! these are deliberately not executed during feature implementation.
#[cfg(unix)]
#[test]
fn sigterm_cleans_up_ci_descendants_and_returns_json_cancellation() {
    use std::{
        io::Read,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let Some(executable) = std::env::var_os("RUSTPY_CLI_EXE") else {
        return;
    };
    let Ok(python) = which::which("python3").or_else(|_| which::which("python")) else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let ready = directory.path().join("ready");
    let leaked = directory.path().join("leaked");
    let child_source = format!("import signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); signal.signal(signal.SIGINT,signal.SIG_IGN); open({},'w').write('ready'); time.sleep(3); open({},'w').write('leaked')",
        serde_json::to_string(&ready.display().to_string()).unwrap(), serde_json::to_string(&leaked.display().to_string()).unwrap());
    std::fs::write(
        directory.path().join("test_wait.py"),
        format!(
            r#"import subprocess,sys,time,unittest
class Wait(unittest.TestCase):
    def test_wait(self):
        subprocess.Popen([sys.executable, '-c', {source}])
        time.sleep(60)
"#,
            source = serde_json::to_string(&child_source).unwrap()
        ),
    )
    .unwrap();
    let mut command = Command::new(executable);
    command
        .args([
            "--headless",
            "test",
            "--framework=unittest",
            "--json",
            "--timeout=20",
            "--project",
        ])
        .arg(directory.path())
        .arg("--interpreter")
        .arg(python)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    rustpy_run::ProcessTree::prepare(&mut command);
    let mut child = command.spawn().unwrap();
    let tree = rustpy_run::ProcessTree::new(&mut child).unwrap();
    struct Guard {
        child: std::process::Child,
        tree: rustpy_run::ProcessTree,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            rustpy_core::process::terminate_owned(&mut self.child, Duration::from_millis(100));
            self.tree.force(&mut self.child);
        }
    }
    let mut guard = Guard { child, tree };
    let mut output = guard.child.stdout.take().unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !ready.exists() {
        assert!(Instant::now() < deadline, "test descendant did not start");
        assert!(guard.tree.poll(&mut guard.child).unwrap().is_none());
        std::thread::sleep(Duration::from_millis(20));
    }
    // Signal only the launcher: it must stop its separately owned test group.
    assert_eq!(
        unsafe { libc::kill(guard.child.id() as i32, libc::SIGTERM) },
        0
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = guard.tree.poll(&mut guard.child).unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "launcher failed to cancel");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(130));
    let mut json = String::new();
    output.read_to_string(&mut json).unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["cancelled"], true);
    std::thread::sleep(Duration::from_secs(3));
    assert!(!leaked.exists(), "descendant survived launcher SIGTERM");
}
