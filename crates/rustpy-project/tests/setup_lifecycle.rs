#[cfg(unix)]
use rustpy_core::CancellationToken;
use rustpy_project::setup::{
    self, CommandSpec, CommandTask, EnvTool, ProjectKind, Setup, SetupAction, SetupError,
    SetupEvent, Step,
};
use std::path::Path;
use std::time::{Duration, Instant};

fn wait(task: &CommandTask) -> Result<(), SetupError> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(result) = task.poll() {
            return result;
        }
        if Instant::now() >= deadline {
            task.cancel();
            panic!("setup process did not finish within the test deadline");
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

#[test]
fn real_stdlib_environment_progresses_to_selection_with_events() {
    let python =
        which::which("python3").expect("Python is needed for the actual venv lifecycle regression");
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let mut service = Setup::new(
        root.clone(),
        python,
        setup::plan(ProjectKind::Pure, true, EnvTool::Venv, false, false, ""),
    );
    let Some(SetupAction::Command { step, command }) = service.next_action(false, None) else {
        panic!("environment command");
    };
    assert!(matches!(step, Step::CreateEnv { .. }));
    let task = CommandTask::spawn(command, service.command_token());
    let result = wait(&task);
    assert_eq!(result, Ok(()));
    service.complete(result);
    assert!(service.env_ok);
    let interpreter = setup::venv_python(&root);
    assert!(interpreter.is_file());
    let probed = rustpy_project::interpreter::probe(&interpreter, "venv")
        .expect("created environment is a working interpreter");
    assert!(probed.is_venv);
    assert_eq!(
        service.next_action(false, None),
        Some(SetupAction::SelectEnvironment { interpreter })
    );
    service.complete(Ok(()));
    assert_eq!(
        service.next_action(false, None),
        Some(SetupAction::Finished)
    );
    let events = service.take_events();
    assert!(matches!(
        events.as_slice(),
        [
            SetupEvent::StepStarted(Step::CreateEnv { .. }),
            SetupEvent::StepFinished {
                step: Step::CreateEnv { .. },
                result: Ok(())
            },
            SetupEvent::StepStarted(Step::SelectEnv),
            SetupEvent::StepFinished {
                step: Step::SelectEnv,
                result: Ok(())
            },
            SetupEvent::Finished
        ]
    ));
    assert!(service.problems.is_empty());
}

#[test]
fn package_provider_waits_and_environment_failure_preserves_only_django_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let plan = setup::plan(ProjectKind::Django, true, EnvTool::Uv, true, true, "site1");
    let mut service = Setup::new(dir.path().into(), "python3".into(), plan.clone());
    assert!(matches!(
        service.next_action(false, None),
        Some(SetupAction::Command {
            step: Step::CreateEnv { .. },
            ..
        })
    ));
    service.complete(Ok(()));
    assert_eq!(
        service.next_action(true, None),
        None,
        "busy package provider does not consume the pending step"
    );
    assert!(matches!(
        service.next_action(false, None),
        Some(SetupAction::InstallRequirements { .. })
    ));
    assert!(service.waits_for_pip());
    service.complete(Err(SetupError::Failed("provider failed".into())));
    assert!(!service.waits_for_pip());
    assert!(matches!(
        service.next_action(false, None),
        Some(SetupAction::Command {
            command: CommandSpec::Django { .. },
            ..
        })
    ));
    service.complete(Ok(()));
    assert!(matches!(
        service.next_action(false, None),
        Some(SetupAction::SelectEnvironment { .. })
    ));
    assert_eq!(service.failed, [Step::InstallRequirements]);

    let mut failed = Setup::new(dir.path().into(), "base-python".into(), plan);
    failed.next_action(false, None).unwrap();
    failed.complete(Err(SetupError::Failed("cannot create environment".into())));
    let Some(SetupAction::Command {
        command: CommandSpec::Django { python, .. },
        ..
    }) = failed.next_action(false, None)
    else {
        panic!("Django fallback should remain");
    };
    assert_eq!(python, Path::new("base-python"));
    failed.complete(Ok(()));
    assert_eq!(failed.next_action(false, None), Some(SetupAction::Finished));
    assert!(!failed.env_ok);
    assert!(failed.problems[0].contains("Creating the virtual environment failed"));
}

#[cfg(unix)]
fn provider(dir: &Path, body: &str) -> std::path::PathBuf {
    provider_fixture::provision(dir, body)
}

#[cfg(unix)]
mod provider_fixture {
    use std::io::{Read, Write};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    const DIRECTORY: &str = "RUSTPY_SETUP_PROVIDER_CHILD_DIR";
    const BYTES: &str = "RUSTPY_SETUP_PROVIDER_CHILD_BYTES";
    const NONCE: &str = "RUSTPY_SETUP_PROVIDER_CHILD_NONCE";
    const ACK: &str = "RUSTPY_SETUP_PROVIDER_ACK ";

    struct OwnedChild(Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn provisioning_child() {
        let Some(directory) = std::env::var_os(DIRECTORY) else {
            return;
        };
        let bytes = std::env::var(BYTES).unwrap();
        let nonce = std::env::var(NONCE).unwrap();
        let file = Path::new(&directory).join("provider with spaces");
        let mut writer = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&file)
            .unwrap();
        writer.write_all(bytes.as_bytes()).unwrap();
        let held = writer.metadata().unwrap();
        drop(writer);
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        let published = std::fs::symlink_metadata(&file).unwrap();
        assert!(published.is_file());
        assert_eq!((held.dev(), held.ino()), (published.dev(), published.ino()));
        assert_eq!(std::fs::read(&file).unwrap(), bytes.as_bytes());
        eprintln!(
            "{ACK}{}",
            serde_json::json!({"nonce": nonce, "pid": std::process::id(),
                "dev": published.dev(), "ino": published.ino(), "size": published.len()})
        );
    }

    pub(super) fn provision(directory: &Path, body: &str) -> PathBuf {
        let directory = dunce::canonicalize(directory).unwrap();
        let bytes = format!("#!/bin/sh\n{body}\n");
        let nonce = directory
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let owned = directory.join("provider-child-environment");
        std::fs::create_dir(&owned).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "provider_fixture::provisioning_child",
                "--nocapture",
            ])
            .env_clear()
            .env(DIRECTORY, &directory)
            .env(BYTES, &bytes)
            .env(NONCE, &nonce)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(value) = std::env::var_os("LD_LIBRARY_PATH") {
            command.env("LD_LIBRARY_PATH", value);
        }
        for (key, name) in [
            ("RUSTPY_CONFIG_DIR", "config"),
            ("RUSTPY_SYSTEM_DIR", "system"),
            ("RUSTPY_LOG_DIR", "log"),
            ("XDG_CONFIG_HOME", "xdg-config"),
            ("XDG_CACHE_HOME", "xdg-cache"),
            ("XDG_DATA_HOME", "xdg-data"),
            ("XDG_STATE_HOME", "xdg-state"),
            ("XDG_RUNTIME_DIR", "xdg-runtime"),
        ] {
            let location = owned.join(name);
            std::fs::create_dir(&location).unwrap();
            command.env(key, location);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut child = OwnedChild(command.spawn().unwrap());
        let original_pid = child.0.id();
        drop(child.0.stdin.take());
        let status = loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "owned provider writer timed out");
            std::thread::sleep(Duration::from_millis(5));
        };
        let mut stdout = String::new();
        child
            .0
            .stdout
            .take()
            .unwrap()
            .take(4097)
            .read_to_string(&mut stdout)
            .unwrap();
        let mut stderr = String::new();
        child
            .0
            .stderr
            .take()
            .unwrap()
            .take(4097)
            .read_to_string(&mut stderr)
            .unwrap();
        assert!(stdout.len() <= 4096 && stderr.len() <= 4096);
        eprintln!(
            "RUSTPY_SETUP_PROVIDER_CHILD pid={original_pid} status={status}\n{stdout}{stderr}"
        );
        assert!(status.success(), "owned provider writer failed");
        let records: Vec<_> = stderr
            .lines()
            .filter_map(|line| line.strip_prefix(ACK))
            .collect();
        assert_eq!(records.len(), 1);
        let ack: serde_json::Value = serde_json::from_str(records[0]).unwrap();
        let file = directory.join("provider with spaces");
        let actual = std::fs::symlink_metadata(&file).unwrap();
        assert!(actual.is_file());
        assert_eq!(ack["nonce"].as_str(), Some(nonce.as_str()));
        assert_eq!(ack["pid"].as_u64(), Some(u64::from(original_pid)));
        assert_eq!(ack["dev"].as_u64(), Some(actual.dev()));
        assert_eq!(ack["ino"].as_u64(), Some(actual.ino()));
        assert_eq!(ack["size"].as_u64(), Some(actual.len()));
        assert_eq!(actual.permissions().mode() & 0o777, 0o755);
        assert_eq!(std::fs::read(&file).unwrap(), bytes.as_bytes());
        file
    }
}

#[cfg(unix)]
#[test]
fn actual_uv_provider_receives_planned_arguments_and_error_output() {
    let dir = tempfile::tempdir().unwrap();
    let executable = provider(
        dir.path(),
        "printf '%s\\n' \"$@\" > args.txt\nprintf 'provider failed\\n' >&2\nexit 7",
    );
    let mut service = Setup::new(
        dir.path().into(),
        "/base/python with spaces".into(),
        setup::plan(ProjectKind::Pure, true, EnvTool::Uv, true, false, ""),
    );
    let Some(SetupAction::Command { command, .. }) = service.next_action(false, Some(&executable))
    else {
        panic!("uv command");
    };
    let task = CommandTask::spawn(command, service.command_token());
    let result = wait(&task);
    assert_eq!(result, Err(SetupError::Failed("provider failed".into())));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("args.txt"))
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        vec![
            "venv",
            "--seed",
            "--python",
            "/base/python with spaces",
            "--system-site-packages",
            &dir.path().join(".venv").display().to_string()
        ]
    );
    service.complete(result);
    assert_eq!(
        service.next_action(false, None),
        Some(SetupAction::Finished)
    );
    assert!(!service.env_ok);
}

#[cfg(unix)]
#[test]
fn django_failure_writes_preserved_scaffold_without_overwriting_existing_files() {
    let dir = tempfile::tempdir().unwrap();
    let executable = provider(dir.path(), "printf 'Django unavailable\\n' >&2\nexit 3");
    std::fs::write(dir.path().join("manage.py"), "user content").unwrap();
    let task = CommandTask::spawn(
        CommandSpec::Django {
            python: executable,
            name: "site1".into(),
            root: dir.path().into(),
        },
        CancellationToken::new(),
    );
    let result = wait(&task);
    assert!(
        matches!(result, Err(SetupError::Failed(message)) if message.contains("created a project skeleton"))
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("manage.py")).unwrap(),
        "user content"
    );
    assert!(dir.path().join("site1/settings.py").is_file());
}

#[cfg(unix)]
#[test]
fn explicit_cancel_returns_cancelled_and_prevents_django_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let executable = provider(dir.path(), ": > started\nexec sleep 30");
    let mut service = Setup::new(
        dir.path().into(),
        executable,
        setup::plan(
            ProjectKind::Django,
            false,
            EnvTool::Venv,
            false,
            false,
            "site1",
        ),
    );
    let Some(SetupAction::Command { command, .. }) = service.next_action(false, None) else {
        panic!("Django command");
    };
    let task = CommandTask::spawn(command, service.command_token());
    let deadline = Instant::now() + Duration::from_secs(3);
    while !dir.path().join("started").exists() && Instant::now() < deadline {
        if let Some(result) = task.poll() {
            panic!("Django provider stopped before its readiness marker: {result:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(dir.path().join("started").exists());
    service.cancel();
    let result = wait(&task);
    assert_eq!(result, Err(SetupError::Cancelled));
    service.complete(result);
    assert_eq!(
        service.next_action(false, None),
        Some(SetupAction::Finished)
    );
    assert!(
        !dir.path().join("manage.py").exists(),
        "cancelled Django must not write a fallback skeleton"
    );
    assert!(service.take_events().iter().any(|event| matches!(
        event,
        SetupEvent::StepFinished {
            result: Err(SetupError::Cancelled),
            ..
        }
    )));
}

#[cfg(target_os = "linux")]
#[test]
fn dropping_active_command_cancels_and_reaps_its_process() {
    let dir = tempfile::tempdir().unwrap();
    let executable = provider(dir.path(), "echo $$ > process.pid\nexec sleep 30");
    let token = CancellationToken::new();
    let task = CommandTask::spawn(
        CommandSpec::Process {
            program: executable,
            args: Vec::new(),
            cwd: dir.path().into(),
        },
        token.clone(),
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    let pidfile = dir.path().join("process.pid");
    while !pidfile.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid = std::fs::read_to_string(pidfile).unwrap();
    let process = std::path::PathBuf::from(format!("/proc/{}", pid.trim()));
    assert!(process.exists());
    drop(task);
    assert!(token.is_cancelled());
    let deadline = Instant::now() + Duration::from_secs(5);
    while process.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !process.exists(),
        "dropping the task left the active provider alive"
    );
}
