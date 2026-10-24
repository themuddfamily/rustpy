//! Startup failure integration fixtures; production console stdin is unchanged.
use super::*;

#[test]
fn missing_debugpy_keeps_original_startup_hint() {
    use rustpy_project::RunConfig;
    let Some(python) = which::which("python3").ok() else {
        return;
    };
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("never_run.py");
    std::fs::write(&script, "raise AssertionError('target must not run')\n").unwrap();
    let mut config = RunConfig::for_script(&script);
    config.interpreter = Some(python);
    // Disable site-packages deterministically, preserving ordinary CLI
    // interpreter-option behavior rather than corrupting the installation.
    config.interpreter_options = "-I -S".into();
    let mut session = DebugSession::launch(
        9202,
        &config,
        None,
        directory.path(),
        LaunchSetup::default(),
    )
    .unwrap();
    drop(session.child.as_mut().unwrap().stdin.take());
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && session.is_active() {
        session.poll();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        matches!(session.state, DebugState::Failed(_)),
        "{:?}",
        session.state
    );
    let output = session
        .output
        .iter()
        .map(|chunk| chunk.text.as_str())
        .collect::<String>();
    assert!(output.contains("No module named debugpy"), "{output}");
    assert!(output.contains("pip install debugpy"), "{output}");
    assert!(!output.contains("target must not run"), "{output}");
}
