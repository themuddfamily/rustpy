//! Actual desktop host exercised only through its early headless entrypoint.
use rustpy_run::{RunSession, SessionStatus};
use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, Instant},
};

fn python() -> std::path::PathBuf {
    which::which("python3")
        .or_else(|_| which::which("python"))
        .expect("Python fixture interpreter")
}
fn finish(session: &mut RunSession) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while session.is_running() && Instant::now() < deadline {
        session.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    session.poll();
    if session.is_running() {
        session.stop();
        panic!("redirected process did not finish: {}", session.text());
    }
}
#[test]
fn pty_keeps_output_terminal_and_reads_binary_file_to_eof_without_shell_argument_parsing() {
    rustpy_run::redirect_input::register_host(env!("CARGO_BIN_EXE_rustpy").into());
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input with spaces.txt");
    std::fs::write(&input, b"first\0last\n").unwrap();
    let argument = "a quote\" ; $(literal) %NAME% & spaces";
    let script = "import sys,os; assert sys.stdin.buffer.read() == b'first\\0last\\n'; assert not sys.stdin.isatty(); assert sys.stdout.isatty(); assert os.environ.get('ONLY_EXPLICIT') == 'yes'; assert 'PATH' not in os.environ; print('ARG=' + sys.argv[1]); print('redirected EOF reached')";
    let args = vec!["-c".into(), script.into(), argument.into()];
    let env = BTreeMap::from([
        ("ONLY_EXPLICIT".into(), "yes".into()),
        ("PYTHONIOENCODING".into(), "utf-8".into()),
    ]);
    let mut session = RunSession::spawn_pty_command_with_input(
        1,
        "redirected PTY",
        &python(),
        &args,
        dir.path(),
        &env,
        rustpy_run::process::PtyInput {
            inherit_environment: false,
            path: Some(&input),
        },
    );
    assert!(!session.accepts_stdin());
    // Console input cannot replace or append to the selected file.
    session.write_stdin("unexpected console input\n");
    finish(&mut session);
    assert_eq!(
        session.status,
        SessionStatus::Exited(0),
        "{}",
        session.text()
    );
    assert!(
        session.text().contains(&format!("ARG={argument}")),
        "{}",
        session.text()
    );
    assert!(session.text().contains("redirected EOF reached"));
}
#[test]
fn disappearing_input_fails_before_target_runs_and_never_falls_back_to_console_input() {
    rustpy_run::redirect_input::register_host(env!("CARGO_BIN_EXE_rustpy").into());
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("target-ran");
    let args = vec![
        "-c".into(),
        "import pathlib; pathlib.Path('target-ran').touch()".into(),
    ];
    let mut session = RunSession::spawn_pty_command_with_input(
        2,
        "missing input",
        &python(),
        &args,
        dir.path(),
        &BTreeMap::new(),
        rustpy_run::process::PtyInput {
            inherit_environment: true,
            path: Some(Path::new("missing input.txt")),
        },
    );
    finish(&mut session);
    assert_eq!(
        session.status,
        SessionStatus::Exited(126),
        "{}",
        session.text()
    );
    assert!(session.text().contains("RustPy input redirection"));
    assert!(!marker.exists());
}
