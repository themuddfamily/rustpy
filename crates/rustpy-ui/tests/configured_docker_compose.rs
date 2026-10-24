//! Actual Compose typed launch and independently selected remote SDK.
#![cfg(target_os = "linux")]
use rustpy_project::{
    configuration_types::{ConfigurationSpec, ConfigurationType},
    interpreter_remote::{
        install_wrapper, DiscoveryJob, DockerResources, DockerTransport, RemoteCandidate,
        RemoteKind,
    },
};
use rustpy_run::{RunConfig, RunSession, SessionStatus};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
fn required(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("explicit {key} required"))
}
struct RestoreFile(PathBuf, Vec<u8>);
impl Drop for RestoreFile {
    fn drop(&mut self) {
        std::fs::write(&self.0, &self.1).unwrap();
    }
}
struct Compose {
    cli: String,
    root: tempfile::TempDir,
    project: String,
}
impl Compose {
    fn command(&self, args: &[&str]) -> rustpy_deploy::CommandSpec {
        rustpy_deploy::CommandSpec::new(&self.cli)
            .args([
                "compose",
                "--project-directory",
                self.root.path().to_str().unwrap(),
                "--file",
                self.root.path().join("compose.yaml").to_str().unwrap(),
                "--project-name",
                &self.project,
            ])
            .args(args.iter().copied())
    }
    fn run(&self, args: &[&str]) -> String {
        rustpy_deploy::exec::run_checked(&self.command(args), Duration::from_secs(40), |_| None)
            .unwrap()
            .stdout_text()
    }
    fn assert_absent(&self) {
        let command = rustpy_deploy::CommandSpec::new(&self.cli).args([
            "ps",
            "-aq",
            "--filter",
            &format!("label=com.docker.compose.project={}", self.project),
        ]);
        assert!(
            rustpy_deploy::exec::run_checked(&command, Duration::from_secs(20), |_| None)
                .unwrap()
                .stdout_text()
                .trim()
                .is_empty(),
            "owned Compose container survives down"
        );
    }
}
impl Drop for Compose {
    fn drop(&mut self) {
        let _ = rustpy_deploy::exec::run(
            &self.command(&["down", "--remove-orphans", "--timeout", "2"]),
            Duration::from_secs(25),
        );
    }
}
fn assert_probe(output: &str, cpus: &[u32]) {
    let record = output
        .lines()
        .find_map(|line| line.split_once("RUSTPY_COMPOSE=").map(|(_, value)| value))
        .unwrap_or_else(|| panic!("actual Compose producer missing: {output}"));
    let value: serde_json::Value = serde_json::from_str(record).unwrap();
    assert_eq!(value["cpus"], serde_json::json!(cpus));
    assert_eq!(value["payload"], "actual Compose π");
    assert!(value["nice"].as_i64().unwrap() >= 10);
    assert!(value["threads"].as_u64().unwrap() <= 2);
    assert_eq!(value["omp"], "2");
    assert_eq!(value["literal"], "${RUSTPY_SHOULD_STAY_LITERAL}");
    let tail = "RUSTPY_COMPOSE_TAIL=".to_owned() + &"π".repeat(24 * 1024);
    assert_eq!(
        output.matches(&tail).count(),
        1,
        "complete final tail exactly once"
    );
    assert_eq!(output.matches("RUSTPY_COMPOSE_FINAL_MARKER").count(), 1);
    let quota = value["quota"].as_array().unwrap();
    assert_eq!(quota[0].as_i64().unwrap(), 2 * quota[1].as_i64().unwrap());
}
const PROBE: &str = r#"import os,json,pathlib
p=pathlib.Path('/sys/fs/cgroup/cpu.max')
q=list(map(int,p.read_text().split())) if p.exists() else [int(pathlib.Path('/sys/fs/cgroup/cpu/cpu.cfs_quota_us').read_text()),int(pathlib.Path('/sys/fs/cgroup/cpu/cpu.cfs_period_us').read_text())]
print('RUSTPY_COMPOSE='+json.dumps(dict(cpus=sorted(os.sched_getaffinity(0)),quota=q,nice=os.getpriority(os.PRIO_PROCESS,0),threads=len(list(pathlib.Path('/proc/self/task').iterdir())),omp=os.environ['OMP_NUM_THREADS'],literal=os.environ['RUSTPY_LITERAL'],payload=pathlib.Path('/work/input.txt').read_text())),flush=True)
print('RUSTPY_COMPOSE_TAIL='+'π'*(24*1024),flush=True)
print('RUSTPY_COMPOSE_FINAL_MARKER',flush=True)
"#;
#[test]
#[ignore = "requires genuine private Docker CLI/Compose plugin, local pinned image and central capped actor"]
fn configured_compose_typed_launch_and_selected_sdk_use_actual_service() {
    assert_eq!(required("RUSTPY_DOCKER_ADMITTED"), "1");
    let cli = required("RUSTPY_DOCKER_CLI");
    assert!(Path::new(&cli).is_absolute());
    assert_eq!(&std::fs::read(&cli).unwrap()[..4], b"\x7fELF");
    assert!(required("DOCKER_HOST").starts_with("unix://"));
    assert!(Path::new(&required("DOCKER_CONFIG")).is_absolute());
    let version = rustpy_deploy::exec::run_checked(
        &rustpy_deploy::CommandSpec::new(&cli).args(["compose", "version"]),
        Duration::from_secs(20),
        |_| None,
    )
    .unwrap()
    .stdout_text();
    assert!(version.contains("Compose"), "{version}");
    let image = required("RUSTPY_DOCKER_IMAGE");
    let digest = image
        .split_once("@sha256:")
        .expect("pinned image required")
        .1;
    assert_eq!(digest.len(), 64);
    assert!(digest.bytes().all(|c| c.is_ascii_hexdigit()));
    let inspect = rustpy_deploy::CommandSpec::new(&cli).args(["image", "inspect", &image]);
    rustpy_deploy::exec::run_checked(&inspect, Duration::from_secs(20), |_| None).unwrap();
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
    assert_eq!(cpus.len(), 2);
    let cores = cpus
        .iter()
        .map(|cpu| {
            let base = format!("/sys/devices/system/cpu/cpu{cpu}/topology");
            (
                std::fs::read_to_string(format!("{base}/physical_package_id")).unwrap(),
                std::fs::read_to_string(format!("{base}/core_id")).unwrap(),
            )
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(cores.len(), 2, "two distinct physical cores required");
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let actor_nice: i32 = stat
        .rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .nth(16)
        .unwrap()
        .parse()
        .unwrap();
    assert!(actor_nice >= 10, "reduced-priority central actor required");
    let project = format!(
        "rustpycompose{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let mount = required("RUSTPY_DOCKER_MOUNT_ROOT");
    assert!(Path::new(&mount).is_absolute());
    let root = tempfile::Builder::new()
        .prefix("compose-")
        .tempdir_in(mount)
        .unwrap();
    std::fs::write(root.path().join("input.txt"), "actual Compose π").unwrap();
    let limits = serde_json::json!({"image":image,"pull_policy":"never","network_mode":"none","cpuset":cpuset,"cpu_period":100000,"cpu_quota":200000,"environment":{"OMP_NUM_THREADS":"2","OPENBLAS_NUM_THREADS":"2","RUSTPY_LITERAL":"$${RUSTPY_SHOULD_STAY_LITERAL}"},"volumes":[{"type":"bind","source":root.path(),"target":"/work","read_only":true}],"working_dir":"/work","entrypoint":[]});
    let mut failure = limits.clone();
    failure["command"] =
        serde_json::json!([nice, "-n", "10", "--", python, "-c", "raise SystemExit(7)"]);
    let mut probe = limits.clone();
    probe["command"] = serde_json::json!([nice, "-n", "10", "--", python, "-c", PROBE]);
    let mut sdk = limits;
    sdk["command"] = serde_json::json!([
        nice,
        "-n",
        "10",
        "--",
        python,
        "-c",
        "import time; print('RUSTPY_COMPOSE_READY',flush=True); time.sleep(300)"
    ]);
    let mut volume_source = sdk.clone();
    volume_source["ipc"] = serde_json::json!("shareable");
    volume_source["volumes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"type":"volume","target":"/shared-anonymous"}));
    volume_source["command"] = serde_json::json!([
        nice,
        "-n",
        "10",
        "--",
        python,
        "-c",
        "import pathlib,time; pathlib.Path('/shared-anonymous/value').write_text('actual shared π'); print('RUSTPY_SHARED_SOURCE_READY',flush=True); time.sleep(300)"
    ]);
    let mut owned_sdk = sdk.clone();
    owned_sdk["network_mode"] = serde_json::json!("service:volume-source");
    owned_sdk["ipc"] = serde_json::json!("service:volume-source");
    owned_sdk["pid"] = serde_json::json!("service:volume-source");
    owned_sdk["volumes_from"] = serde_json::json!(["volume-source:ro"]);
    owned_sdk["command"] = serde_json::json!([
        nice,
        "-n",
        "10",
        "--",
        python,
        "-c",
        "import pathlib,time
p=pathlib.Path('/shared-anonymous/value')
deadline=time.monotonic()+5
while not p.exists():
 assert time.monotonic()<deadline
 time.sleep(.01)
assert p.read_text()=='actual shared π'
print('RUSTPY_COMPOSE_OWNED_READY',flush=True)
time.sleep(300)"
    ]);
    let mut disabled = probe.clone();
    disabled["profiles"] = serde_json::json!(["configured-profile"]);
    disabled["command"] = serde_json::json!([
        nice,
        "-n",
        "10",
        "--",
        python,
        "-c",
        "import os,time; print('RUSTPY_PROFILE_READY='+os.environ['RUSTPY_PROFILE_VALUE'],flush=True); time.sleep(300)"
    ]);
    disabled["environment"]["RUSTPY_PROFILE_VALUE"] =
        serde_json::json!("${RUSTPY_PROFILE_VALUE:-inactive}");
    std::fs::write(
        root.path().join("selected.env"),
        "RUSTPY_PROFILE_VALUE=actual-selected-env\n",
    )
    .unwrap();
    owned_sdk["volumes"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"type":"volume","target":"/owned-anonymous"}));
    std::fs::write(
        root.path().join("compose.yaml"),
        serde_json::to_vec_pretty(
            &serde_json::json!({"name":project,"services":{"probe":probe,"sdk":sdk,"failed":failure,"owned-sdk":owned_sdk,"volume-source":volume_source,"disabled-profile":disabled}}),
        )
        .unwrap(),
    )
    .unwrap();
    let fixture = Compose {
        cli: cli.clone(),
        root,
        project,
    };
    let mut spec = ConfigurationSpec::new(ConfigurationType::DockerCompose);
    spec.target = "compose.yaml".into();
    spec.service = "probe".into();
    spec.executable = PathBuf::from(&cli);
    spec.arguments = [
        "--pull",
        "never",
        "--no-build",
        "--abort-on-container-exit",
        "--exit-code-from",
        "probe",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let cfg = RunConfig {
        working_dir: Some(fixture.root.path().into()),
        type_options: Some(spec.clone()),
        env: [
            ("DOCKER_HOST".into(), required("DOCKER_HOST")),
            ("DOCKER_CONFIG".into(), required("DOCKER_CONFIG")),
        ]
        .into(),
        ..Default::default()
    };
    let lowered = spec.materialize(&cfg).unwrap();
    let mut session = RunSession::spawn(911, &lowered, None, fixture.root.path());
    let deadline = Instant::now() + Duration::from_secs(45);
    while session.is_running() {
        session.poll();
        if Instant::now() > deadline {
            session.stop();
            panic!("typed actual Compose up did not finish");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    session.poll();
    let output = session
        .output
        .iter()
        .map(|part| part.text.as_str())
        .collect::<String>();
    assert_eq!(session.status, SessionStatus::Exited(0), "{output}");
    assert_probe(&output, &cpus);
    fixture.run(&["down", "--timeout", "2"]);
    fixture.assert_absent();
    // SDK uses its own actual running service and the public discovery/wrapper route.
    fixture.run(&["up", "-d", "--pull", "never", "--no-build", "sdk"]);
    let candidate = RemoteCandidate {
        kind: RemoteKind::Compose,
        target: "sdk".into(),
        python: python.clone(),
        project_root: fixture.root.path().into(),
        remote_root: None,
        docker: Some(DockerTransport {
            executable: cli.into(),
            resources: Some(DockerResources {
                cpuset_cpus: cpuset,
                cpu_millis: 2000,
                threads: 2,
                nice: 10,
                nice_program: nice,
                name: String::new(),
                labels: Default::default(),
            }),
        }),
    };
    let adapters = fixture.root.path().join("adapters");
    assert!(DiscoveryJob::remote(candidate.clone(), adapters.clone(), false).is_err());
    let job = DiscoveryJob::remote(candidate.clone(), adapters.clone(), true).unwrap();
    let metadata = job
        .receiver
        .recv_timeout(Duration::from_secs(35))
        .unwrap()
        .unwrap();
    assert_eq!(metadata.interpreter.platform, "linux");
    assert!(!metadata.interpreter.sys_path.is_empty());
    let wrapper = install_wrapper(&candidate, &adapters).unwrap();
    let mut command = std::process::Command::new(wrapper);
    command.args(["-c", PROBE]);
    let (status, out, err) = rustpy_project::native_skeleton::supervise_tool_status(
        &mut command,
        &rustpy_core::CancellationToken::new(),
        Duration::from_secs(30),
        262144,
    )
    .unwrap();
    assert!(status.success(), "{}", String::from_utf8_lossy(&err));
    assert_probe(&String::from_utf8(out).unwrap(), &cpus);
    let mut missing = candidate;
    missing.python = "/rustpy-absent-python".into();
    let failed = DiscoveryJob::remote(missing, adapters, true).unwrap();
    assert!(failed
        .receiver
        .recv_timeout(Duration::from_secs(30))
        .unwrap()
        .is_err());
    // Keep the user's named SDK project alive: isolated cleanup must not touch it.
    // Explicit isolated ownership uses fresh projects, leaving the user's named
    // project semantics intact. The guard below does not know these project IDs.
    for mode in [
        "fast",
        "failure",
        "immediate",
        "running",
        "multi",
        "profile",
        "drop",
    ] {
        let mut owned_spec = spec.clone();
        owned_spec.compose_owned_project = true;
        owned_spec.service = match mode {
            "fast" => "probe",
            "failure" => "failed",
            "running" => "owned-sdk",
            "multi" | "profile" => "",
            _ => "sdk",
        }
        .into();
        owned_spec.arguments = vec!["--no-build".into(), "--pull".into(), "never".into()];
        if mode == "failure" {
            owned_spec
                .arguments
                .extend(["--exit-code-from".into(), "failed".into()]);
        }
        if mode == "multi" {
            owned_spec
                .arguments
                .extend(["--scale".into(), "sdk=2".into()]);
        }
        if mode == "profile" {
            owned_spec.arguments.extend([
                "--profile".into(),
                "configured-profile".into(),
                "--env-file".into(),
                fixture
                    .root
                    .path()
                    .join("selected.env")
                    .to_string_lossy()
                    .into_owned(),
            ]);
        }
        let cfg = RunConfig {
            type_options: Some(owned_spec.clone()),
            working_dir: Some(fixture.root.path().into()),
            env: [
                ("DOCKER_HOST".into(), required("DOCKER_HOST")),
                ("DOCKER_CONFIG".into(), required("DOCKER_CONFIG")),
            ]
            .into(),
            ..Default::default()
        };
        let cfg = owned_spec
            .materialize_at(&cfg, fixture.root.path())
            .unwrap();
        let mut session = RunSession::spawn(912, &cfg, None, fixture.root.path());
        assert!(
            session.owns_process(),
            "preflight must retain RunPanel ownership"
        );
        if mode == "immediate" {
            let start = Instant::now();
            session.stop();
            assert!(start.elapsed() < Duration::from_millis(100));
        }
        let deadline = Instant::now() + Duration::from_secs(75);
        let mut output = String::new();
        let mut stopped = mode == "immediate";
        while session.owns_process() {
            session.poll();
            output = session
                .output
                .iter()
                .map(|chunk| chunk.text.as_str())
                .collect();
            if (mode == "running" && output.contains("RUSTPY_COMPOSE_OWNED_READY"))
                || ((mode == "multi" || mode == "drop") && output.contains("RUSTPY_COMPOSE_READY"))
                || (mode == "profile"
                    && output.contains("RUSTPY_PROFILE_READY=actual-selected-env"))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "owned Compose {mode} did not retire: {output}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let owned_project = output
            .lines()
            .find_map(|line| line.strip_prefix("RUSTPY_COMPOSE_OWNED_PROJECT="))
            .unwrap()
            .to_owned();
        let anonymous = if mode == "running" {
            let command = rustpy_deploy::CommandSpec::new(&fixture.cli).args([
                "ps",
                "-aq",
                "--filter",
                &format!("label=com.docker.compose.project={owned_project}"),
                "--filter",
                "label=com.docker.compose.service=owned-sdk",
            ]);
            let id = rustpy_deploy::exec::run_checked(&command, Duration::from_secs(15), |_| None)
                .unwrap()
                .stdout_text();
            let inspect = rustpy_deploy::CommandSpec::new(&fixture.cli).args([
                "container",
                "inspect",
                "--format",
                "{{json .Mounts}}",
                id.trim(),
            ]);
            let out = rustpy_deploy::exec::run_checked(&inspect, Duration::from_secs(15), |_| None)
                .unwrap()
                .stdout_text();
            let mounts: Vec<serde_json::Value> = serde_json::from_str(out.trim()).unwrap();
            let anonymous = mounts
                .iter()
                .filter(|mount| {
                    ["/owned-anonymous", "/shared-anonymous"]
                        .iter()
                        .any(|target| mount["Destination"] == *target)
                })
                .map(|mount| mount["Name"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>();
            assert_eq!(
                anonymous.len(),
                2,
                "both direct and inherited anonymous mounts must be real"
            );
            let host = rustpy_deploy::CommandSpec::new(&fixture.cli).args([
                "container",
                "inspect",
                "--format",
                "{{json .HostConfig}}",
                id.trim(),
            ]);
            let host = rustpy_deploy::exec::run_checked(&host, Duration::from_secs(15), |_| None)
                .unwrap()
                .stdout_text();
            let host: serde_json::Value = serde_json::from_str(host.trim()).unwrap();
            for field in ["NetworkMode", "IpcMode", "PidMode"] {
                assert!(
                    host[field].as_str().unwrap().starts_with("container:"),
                    "real {field} sharing was not applied"
                );
            }
            anonymous
        } else {
            Vec::new()
        };
        let restore = if mode == "running" || mode == "drop" {
            let path = fixture.root.path().join("compose.yaml");
            let previous = std::fs::read(&path).unwrap();
            std::fs::write(&path, "invalid: [changed after captured launch").unwrap();
            Some(RestoreFile(path, previous))
        } else {
            None
        };
        if mode == "running" || mode == "multi" || mode == "profile" {
            assert!(session.owns_process());
            session.stop();
            stopped = true;
        }
        let project = output
            .lines()
            .find_map(|line| line.strip_prefix("RUSTPY_COMPOSE_OWNED_PROJECT="))
            .expect("captured owned project token")
            .to_owned();
        if mode == "drop" {
            drop(session);
        } else {
            while session.owns_process() {
                session.poll();
                assert!(
                    Instant::now() < deadline,
                    "owned Compose cleanup timeout: {:?}",
                    session.output
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            session.poll();
            if !stopped {
                assert_eq!(
                    session.status,
                    SessionStatus::Exited(if mode == "failure" { 7 } else { 0 }),
                    "{:?}",
                    session.output
                );
                if mode != "failure" {
                    assert_probe(
                        &session
                            .output
                            .iter()
                            .map(|chunk| chunk.text.as_str())
                            .collect::<String>(),
                        &cpus,
                    );
                }
            }
        }
        // Repeated observations AFTER terminal/Drop prove no later startup
        // creation follows the explicit create→start fence. No cleanup fallback.
        let mut absent_since = None;
        loop {
            let command = rustpy_deploy::CommandSpec::new(&fixture.cli).args([
                "ps",
                "-aq",
                "--filter",
                &format!("label=com.docker.compose.project={project}"),
            ]);
            let listed =
                rustpy_deploy::exec::run_checked(&command, Duration::from_secs(15), |_| None)
                    .unwrap()
                    .stdout_text();
            if mode == "drop" && !listed.trim().is_empty() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            assert!(
                listed.trim().is_empty(),
                "owned Compose project {project} survives {mode}"
            );
            if absent_since.get_or_insert_with(Instant::now).elapsed() >= Duration::from_secs(2) {
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        for name in anonymous {
            let command = rustpy_deploy::CommandSpec::new(&fixture.cli).args([
                "volume",
                "ls",
                "-q",
                "--filter",
                &format!("name={name}"),
            ]);
            let out = rustpy_deploy::exec::run_checked(&command, Duration::from_secs(15), |_| None)
                .unwrap()
                .stdout_text();
            assert!(
                !out.lines().any(|line| line == name),
                "owned anonymous volume survived physical retirement"
            );
        }
        drop(restore);
        let named = fixture.run(&["ps", "-q", "sdk"]);
        assert!(
            !named.trim().is_empty(),
            "isolated owner removed the user's named project"
        );
    }
    fixture.run(&["down", "--remove-orphans", "--timeout", "2"]);
    fixture.assert_absent();
}
