//! Explicit real-daemon acceptance for the Services snapshot and lifecycle argv.
//! Requires an admitted Docker CLI with the Compose plugin and an already-local
//! pinned image; never pulls, prunes or touches resources it did not create.
#![cfg(unix)]
use rustpy_docker::{
    compose_args, container_args, run, Capture, ContainerAction, Snapshot, SnapshotTask,
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn required(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("explicit {key} admission required"))
}

struct Owned {
    cli: String,
    container: String,
    compose: Vec<String>,
    root: PathBuf,
}
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = run(
            &self.cli,
            &["rm", "-f", &self.container],
            Duration::from_secs(30),
        );
        let args: Vec<&str> = self.compose.iter().map(String::as_str).collect();
        let _ = run(&self.cli, &args, Duration::from_secs(60));
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn snapshot(cli: &str, cwd: &Path) -> Snapshot {
    let mut task = SnapshotTask::start(Capture::current(cli, cwd).unwrap());
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(result) = task.try_result() {
            return result.unwrap();
        }
        assert!(Instant::now() < deadline, "{}", task.drain_status());
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn state(snapshot: &Snapshot, name: &str) -> Option<String> {
    snapshot
        .containers
        .iter()
        .find(|c| c.names.split(',').any(|n| n == name))
        .map(|c| c.state.clone())
}

fn wait_state(cli: &str, cwd: &Path, name: &str, wanted: Option<&str>) -> Snapshot {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let snap = snapshot(cli, cwd);
        if state(&snap, name).as_deref() == wanted {
            return snap;
        }
        assert!(
            Instant::now() < deadline,
            "{name}: expected {wanted:?}, saw {:?}",
            state(&snap, name)
        );
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[test]
#[ignore = "requires explicit RUSTPY_DOCKER_ADMITTED=1, absolute CLI with Compose plugin, local pinned IMAGE and MOUNT_ROOT"]
fn configured_services_snapshot_lifecycle_and_compose_projects_use_real_daemon() {
    assert_eq!(required("RUSTPY_DOCKER_ADMITTED"), "1");
    let cli = required("RUSTPY_DOCKER_CLI");
    assert!(Path::new(&cli).is_absolute());
    let image = required("RUSTPY_DOCKER_IMAGE");
    assert!(image.contains("@sha256:"), "pinned local image required");
    run(&cli, &["image", "inspect", &image], Duration::from_secs(20)).unwrap();
    let mount = PathBuf::from(required("RUSTPY_DOCKER_MOUNT_ROOT"));
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = mount.join(format!("services-{unique}"));
    std::fs::create_dir_all(&root).unwrap();
    let root = dunce::canonicalize(&root).unwrap();
    let project = format!("rustpyservices{unique}");
    let container = format!("rustpy-services-{unique}");
    let compose_file = root.join("compose.yaml");
    std::fs::write(
        &compose_file,
        format!(
            "services:\n  app:\n    image: {image}\n    pull_policy: never\n    network_mode: none\n    command: [\"python3\", \"-u\", \"-c\", \"import time; print('compose service up', flush=True); time.sleep(600)\"]\n"
        ),
    )
    .unwrap();
    let compose_path = compose_file.to_string_lossy().into_owned();
    let _owned = Owned {
        cli: cli.clone(),
        container: container.clone(),
        compose: vec![
            "compose".into(),
            "-f".into(),
            compose_path.clone(),
            "-p".into(),
            project.clone(),
            "down".into(),
            "--timeout".into(),
            "1".into(),
        ],
        root: root.clone(),
    };

    run(
        &cli,
        &[
            "run",
            "--detach",
            "--name",
            &container,
            "--network",
            "none",
            "--label",
            "rustpy.acceptance=services",
            &image,
            "python3",
            "-u",
            "-c",
            "import time; print('services container up', flush=True); time.sleep(600)",
        ],
        Duration::from_secs(60),
    )
    .unwrap();
    run(
        &cli,
        &[
            "compose",
            "-f",
            &compose_path,
            "-p",
            &project,
            "up",
            "--detach",
        ],
        Duration::from_secs(120),
    )
    .unwrap();

    // Services snapshot through the supervised task (the UI path).
    let snap = wait_state(&cli, &root, &container, Some("running"));
    assert!(!snap.server_version.is_empty());
    assert!(snap.warnings.is_empty(), "{:?}", snap.warnings);
    let ours = snap
        .containers
        .iter()
        .find(|c| c.names == container)
        .unwrap();
    assert!(ours.is_running() && ours.labels.contains("rustpy.acceptance=services"));
    assert_eq!(ours.compose_project(), None);
    let service = snap
        .containers
        .iter()
        .find(|c| c.compose_project() == Some(project.as_str()))
        .expect("compose service container listed with its project label");
    assert!(service.is_running(), "{service:?}");
    let (repository, _) = image.split_once('@').unwrap();
    assert!(
        snap.images.iter().any(|i| {
            let reference = i.reference();
            reference == repository
                || reference.starts_with(&format!("{repository}:"))
                || i.repository == repository
        }),
        "{repository} missing from {:?}",
        snap.images
    );
    let listed = snap
        .compose
        .iter()
        .find(|p| p.name == project)
        .expect("compose ls lists the project")
        .clone();
    assert!(listed.status.starts_with("running"), "{listed:?}");
    assert!(
        listed.config_files.split(',').any(|f| f == compose_path),
        "{listed:?}"
    );

    // Logs and exec against the real container.
    let logs = run(&cli, &["logs", &container], Duration::from_secs(20)).unwrap();
    assert!(logs.contains("services container up"), "{logs}");
    let exec = run(
        &cli,
        &["exec", &container, "python3", "-c", "print(6*7)"],
        Duration::from_secs(20),
    )
    .unwrap();
    assert_eq!(exec.trim(), "42");

    // Lifecycle argv from the Services actions.
    let lifecycle = |action| {
        let args = container_args(action, &container);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        run(&cli, &args, Duration::from_secs(60)).unwrap();
    };
    lifecycle(ContainerAction::Stop);
    wait_state(&cli, &root, &container, Some("exited"));
    lifecycle(ContainerAction::Start);
    wait_state(&cli, &root, &container, Some("running"));
    lifecycle(ContainerAction::Restart);
    wait_state(&cli, &root, &container, Some("running"));
    lifecycle(ContainerAction::Remove);
    wait_state(&cli, &root, &container, None);

    // Compose project actions use the listed config files and project name.
    let logs = compose_args(&listed, "logs --no-color");
    let logs: Vec<&str> = logs.iter().map(String::as_str).collect();
    let logs = run(&cli, &logs, Duration::from_secs(30)).unwrap();
    assert!(logs.contains("compose service up"), "{logs}");
    let restart = compose_args(&listed, "restart");
    let restart: Vec<&str> = restart.iter().map(String::as_str).collect();
    run(&cli, &restart, Duration::from_secs(60)).unwrap();
    let down = compose_args(&listed, "down --timeout 1");
    let down: Vec<&str> = down.iter().map(String::as_str).collect();
    run(&cli, &down, Duration::from_secs(60)).unwrap();
    let snap = snapshot(&cli, &root);
    assert!(
        !snap.compose.iter().any(|p| p.name == project),
        "{:?}",
        snap.compose
    );
    assert!(!snap
        .containers
        .iter()
        .any(|c| c.compose_project() == Some(project.as_str())));
}
