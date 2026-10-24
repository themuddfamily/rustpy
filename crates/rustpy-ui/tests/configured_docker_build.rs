//! Explicit legacy-builder acceptance. No pulls, provider installation, or daemon changes.
#![cfg(target_os = "linux")]
use rustpy_deploy::{
    targets::{docker_args, Docker, DockerArguments},
    CommandSpec,
};
use rustpy_project::configuration_types::{ConfigurationSpec, ConfigurationType};
use rustpy_run::{LegacyDockerBuildResources, RunConfig, RunSession, SessionStatus};
use std::{
    path::Path,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
fn required(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("required {key}"))
}
fn execute(command: &CommandSpec) -> String {
    rustpy_deploy::exec::run_checked(command, Duration::from_secs(90), |_| None)
        .unwrap()
        .stdout_text()
}
struct Images {
    connection: Docker,
    tags: Vec<String>,
}
impl Drop for Images {
    fn drop(&mut self) {
        for tag in &self.tags {
            let command = self
                .connection
                .command(vec!["image".into(), "rm".into(), tag.clone()])
                .unwrap();
            let _ = rustpy_deploy::exec::run(&command, Duration::from_secs(20));
        }
    }
}
#[test]
#[ignore = "requires admitted genuine Docker CLI, local pinned Python image and legacy-builder support"]
fn configured_legacy_typed_and_deployment_builds_enforce_actual_run_cpu_limits() {
    assert_eq!(required("RUSTPY_DOCKER_ADMITTED"), "1");
    let cli = required("RUSTPY_DOCKER_CLI");
    assert!(Path::new(&cli).is_absolute());
    assert_eq!(&std::fs::read(&cli).unwrap()[..4], b"\x7fELF");
    assert!(required("DOCKER_HOST").starts_with("unix://"));
    assert!(Path::new(&required("DOCKER_CONFIG")).is_absolute());
    let base = required("RUSTPY_DOCKER_IMAGE");
    let digest = base
        .split_once("@sha256:")
        .expect("local pinned RepoDigest required")
        .1;
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|c| c.is_ascii_hexdigit()));
    let python = required("RUSTPY_DOCKER_PYTHON");
    let nice = required("RUSTPY_DOCKER_NICE");
    assert!(Path::new(&python).is_absolute() && Path::new(&nice).is_absolute());
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let cpuset = status
        .lines()
        .find_map(|s| s.strip_prefix("Cpus_allowed_list:"))
        .unwrap()
        .trim()
        .to_string();
    let mut cpus = Vec::<u32>::new();
    for part in cpuset.split(',') {
        let (a, b) = part.split_once('-').unwrap_or((part, part));
        cpus.extend(a.parse::<u32>().unwrap()..=b.parse::<u32>().unwrap());
    }
    assert_eq!(cpus.len(), 2, "central capped actor required");
    let policy = LegacyDockerBuildResources {
        cpuset_cpus: cpuset,
        cpu_millis: 2000,
    };
    let root = tempfile::tempdir().unwrap();
    let probe = format!(
        r#"import os,json,pathlib
assert sorted(os.sched_getaffinity(0))=={cpus:?}
p=pathlib.Path('/sys/fs/cgroup/cpu.max')
q=p.read_text().split() if p.exists() else [pathlib.Path('/sys/fs/cgroup/cpu/cpu.cfs_quota_us').read_text(),pathlib.Path('/sys/fs/cgroup/cpu/cpu.cfs_period_us').read_text()]
assert int(q[0])==2*int(q[1]),q
assert os.getpriority(os.PRIO_PROCESS,0)>=10
assert len(list(pathlib.Path('/proc/self/task').iterdir()))<=2
assert os.environ['OMP_NUM_THREADS']=='2'
assert pathlib.Path('/input').read_text()=='actual build π'
pathlib.Path('/build-proof.json').write_text(json.dumps(dict(cpus=sorted(os.sched_getaffinity(0)),quota=q,payload='actual build π')))
"#
    );
    let run = serde_json::to_string(&vec![
        nice.clone(),
        "-n".into(),
        "10".into(),
        "--".into(),
        python.clone(),
        "-c".into(),
        probe,
    ])
    .unwrap();
    std::fs::write(root.path().join("Dockerfile"), format!("FROM {base}\nENV OMP_NUM_THREADS=2 OPENBLAS_NUM_THREADS=2\nCOPY input /input\nRUN {run}\n")).unwrap();
    std::fs::write(root.path().join("input"), "actual build π").unwrap();
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let tags = vec![
        format!("rustpy-build-proof:{unique}-typed"),
        format!("rustpy-build-proof:{unique}-deploy"),
    ];
    let connection = Docker {
        executable: cli.clone(),
        endpoint: required("DOCKER_HOST"),
        legacy_build_resources: Some(policy.clone()),
        resources: Some(rustpy_run::DockerResources {
            cpuset_cpus: policy.cpuset_cpus.clone(),
            cpu_millis: 2000,
            threads: 2,
            nice: 10,
            nice_program: nice.clone(),
            name: String::new(),
            labels: Default::default(),
        }),
        ..Default::default()
    };
    let mut owned = Images {
        connection: connection.clone(),
        tags: tags.clone(),
    };
    // Inspect first: --pull=false alone would still fetch a missing FROM image.
    execute(
        &connection
            .command(vec!["image".into(), "inspect".into(), base.clone()])
            .unwrap(),
    );
    // Typed configuration reaches the real public process owner.
    let mut spec = ConfigurationSpec::new(ConfigurationType::DockerImage);
    spec.target = "Dockerfile".into();
    spec.image = tags[0].clone();
    spec.executable = cli.into();
    spec.legacy_docker_build_resources = Some(policy);
    let mut cfg = RunConfig {
        type_options: Some(spec.clone()),
        working_dir: Some(root.path().into()),
        ..Default::default()
    };
    cfg.env
        .insert("DOCKER_HOST".into(), required("DOCKER_HOST"));
    cfg.env
        .insert("DOCKER_CONFIG".into(), required("DOCKER_CONFIG"));
    let cfg = spec.materialize(&cfg).unwrap();
    let mut session = RunSession::spawn(910, &cfg, None, root.path());
    let deadline = Instant::now() + Duration::from_secs(90);
    while session.is_running() {
        session.poll();
        if Instant::now() >= deadline {
            session.stop();
            panic!("legacy build timeout");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    session.poll();
    assert_eq!(
        session.status,
        SessionStatus::Exited(0),
        "{:?}",
        session.output
    );
    // Public deployment planner and executor must preserve the selected legacy environment.
    let args = docker_args(
        "Build Dockerfile",
        &DockerArguments {
            path: root.path().join("Dockerfile").to_string_lossy().into(),
            image: tags[1].clone(),
            ..Default::default()
        },
        root.path(),
    )
    .unwrap();
    let command = connection.command(args).unwrap();
    assert_eq!(
        command.env.get("DOCKER_BUILDKIT").map(String::as_str),
        Some("0")
    );
    execute(&command);
    for tag in &tags {
        let out = execute(
            &connection
                .command(vec![
                    "run".into(),
                    "--rm".into(),
                    "--pull=never".into(),
                    "--network=none".into(),
                    tag.clone(),
                    python.clone(),
                    "-c".into(),
                    "print(open('/build-proof.json').read())".into(),
                ])
                .unwrap(),
        );
        let value: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
        assert_eq!(value["cpus"], serde_json::json!(cpus));
        assert_eq!(value["payload"], "actual build π");
        execute(
            &connection
                .command(vec!["image".into(), "rm".into(), tag.clone()])
                .unwrap(),
        );
        let missing = rustpy_deploy::exec::run(
            &connection
                .command(vec!["image".into(), "inspect".into(), tag.clone()])
                .unwrap(),
            Duration::from_secs(15),
        )
        .unwrap();
        assert!(!missing.success, "owned image survived removal");
    }
    owned.tags.clear(); // Both exact owned tags already verified absent.
}
