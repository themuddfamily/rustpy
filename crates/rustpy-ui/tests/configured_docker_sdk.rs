//! Explicit real-daemon acceptance. No image pulls or unrelated mutations.
#![cfg(target_os = "linux")]
use rustpy_project::interpreter_remote::{
    DiscoveryJob, DockerResources, DockerTransport, RemoteCandidate, RemoteKind,
};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("explicit {name} admission required"))
}
fn docker(cli: &str, args: &[String]) -> String {
    rustpy_docker::run(
        cli,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
        Duration::from_secs(30),
    )
    .unwrap_or_else(|error| panic!("owned Docker operation failed for {args:?}: {error:#}"))
}
fn inherited_cpus() -> Vec<u32> {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let nice: i32 = stat
        .rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .nth(16)
        .unwrap()
        .parse()
        .unwrap();
    assert!(nice >= 10, "central actor must retain reduced priority");
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let list = status
        .lines()
        .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
        .unwrap()
        .trim();
    let mut cpus = Vec::new();
    for part in list.split(',') {
        let mut bounds = part.split('-').map(|value| value.parse::<u32>().unwrap());
        let first = bounds.next().unwrap();
        let last = bounds.next().unwrap_or(first);
        cpus.extend(first..=last);
    }
    assert_eq!(
        cpus.len(),
        2,
        "central actor must inherit exactly two admitted CPUs"
    );
    let physical = cpus
        .iter()
        .map(|cpu| {
            let root = format!("/sys/devices/system/cpu/cpu{cpu}/topology");
            (
                std::fs::read_to_string(format!("{root}/physical_package_id")).unwrap(),
                std::fs::read_to_string(format!("{root}/core_id")).unwrap(),
            )
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(physical.len(), 2, "admit two distinct physical cores");
    cpus
}
struct Owned {
    cli: String,
    label: String,
    containers: Vec<String>,
}
impl Drop for Owned {
    fn drop(&mut self) {
        // Labels identify this test's SDK --rm containers even if a probe failed
        // after daemon creation but before the local CLI returned its result.
        let listed = rustpy_docker::run(
            &self.cli,
            &[
                "ps",
                "-aq",
                "--filter",
                &format!("label=rustpy.acceptance={}", self.label),
            ],
            Duration::from_secs(15),
        )
        .unwrap_or_default();
        self.containers.extend(listed.lines().map(str::to_owned));
        self.containers.sort();
        self.containers.dedup();
        for id in &self.containers {
            if !id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit()) {
                let _ = rustpy_docker::run(&self.cli, &["rm", "-f", id], Duration::from_secs(15));
            }
        }
    }
}
fn assert_runtime(value: &serde_json::Value, cpus: &[u32]) {
    assert_eq!(value["cpus"], serde_json::json!(cpus));
    assert!(value["nice"].as_i64().unwrap() >= 10);
    assert!(value["threads"].as_u64().unwrap() <= 2);
    assert_eq!(value["omp"], "2");
    assert_eq!(value["blas"], "2");
    assert_eq!(value["cwd"], "/work");
    assert_eq!(value["mounted"], "owned input π\n");
    let quota = value["quota"].as_array().unwrap();
    assert!(
        quota[0].as_i64().unwrap() > 0
            && quota[0].as_i64().unwrap() <= 2 * quota[1].as_i64().unwrap(),
        "daemon-side CPU quota must be at most two cores: {value}"
    );
}
const RUNTIME: &str = "import os,json,pathlib\np=pathlib.Path('/sys/fs/cgroup/cpu.max')\nquota=list(map(int,p.read_text().split())) if p.exists() else [int(pathlib.Path('/sys/fs/cgroup/cpu/cpu.cfs_quota_us').read_text()),int(pathlib.Path('/sys/fs/cgroup/cpu/cpu.cfs_period_us').read_text())]\nprint(json.dumps({'cpus':sorted(os.sched_getaffinity(0)),'nice':os.getpriority(os.PRIO_PROCESS,0),'threads':len(list(pathlib.Path('/proc/self/task').iterdir())),'omp':os.environ['OMP_NUM_THREADS'],'blas':os.environ['OPENBLAS_NUM_THREADS'],'cwd':os.getcwd(),'quota':quota,'mounted':pathlib.Path('input π.txt').read_text()}))";

#[test]
#[ignore = "requires explicit RUSTPY_DOCKER_ADMITTED=1, absolute CLI, admitted daemon-visible MOUNT_ROOT, already-local real Python IMAGE/PYTHON/NICE and central two-core actor"]
fn configured_real_docker_lifecycle_and_both_sdk_consumers_enforce_daemon_side_limits() {
    assert_eq!(required("RUSTPY_DOCKER_ADMITTED"), "1");
    let cli = required("RUSTPY_DOCKER_CLI");
    assert!(Path::new(&cli).is_absolute() && Path::new(&cli).is_file());
    let image = required("RUSTPY_DOCKER_IMAGE");
    let python = required("RUSTPY_DOCKER_PYTHON");
    let nice = required("RUSTPY_DOCKER_NICE");
    let mount_root = PathBuf::from(required("RUSTPY_DOCKER_MOUNT_ROOT"));
    assert!(
        mount_root.is_absolute() && mount_root.is_dir(),
        "admit an existing host directory that the selected daemon sees at the same path"
    );
    let mount_root = mount_root.canonicalize().unwrap();
    assert!(python.starts_with('/') && nice.starts_with('/'));
    let cpus = inherited_cpus();
    // This check fails if the admitted image is absent. Nothing pulls it.
    let inspected: serde_json::Value = serde_json::from_str(&docker(
        &cli,
        &["image".into(), "inspect".into(), image.clone()],
    ))
    .unwrap();
    assert!(inspected[0]["Id"]
        .as_str()
        .is_some_and(|id| id.starts_with("sha256:")));
    let unique = format!(
        "rustpy-private-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let mut owned = Owned {
        cli: cli.clone(),
        label: unique.clone(),
        containers: vec![],
    };
    let root = tempfile::Builder::new()
        .prefix("docker mount π ")
        // A sandboxed daemon can have a different /tmp namespace. Its bind
        // source must come from explicitly admitted daemon-visible storage.
        .tempdir_in(mount_root)
        .unwrap();
    std::fs::write(root.path().join("input π.txt"), "owned input π\n").unwrap();
    let mut resources = DockerResources {
        cpuset_cpus: cpus
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(","),
        cpu_millis: 2000,
        threads: 2,
        nice: 10,
        nice_program: nice,
        name: format!("{unique}-service"),
        labels: [("rustpy.acceptance".into(), unique.clone())].into(),
    };
    let mut create = vec!["create".into()];
    create.extend(resources.container_args().unwrap());
    create.extend([
        "--volume".into(),
        format!("{}:/work", root.path().display()),
        "--workdir".into(),
        "/work".into(),
        image.clone(),
    ]);
    create.extend(resources.process_prefix().unwrap());
    create.extend([
        python.clone(),
        "-u".into(),
        "-c".into(),
        "import time; print('ACTUAL_DOCKER_STARTED',flush=True); time.sleep(120)".into(),
    ]);
    let id = docker(&cli, &create).trim().to_owned();
    assert!(id.len() >= 12 && id.chars().all(|c| c.is_ascii_hexdigit()));
    owned.containers.push(id.clone());
    let inspected: serde_json::Value =
        serde_json::from_str(&docker(&cli, &["inspect".into(), id.clone()])).unwrap();
    assert_eq!(
        inspected[0]["HostConfig"]["CpusetCpus"],
        resources.cpuset_cpus
    );
    assert_eq!(inspected[0]["HostConfig"]["CpuPeriod"], 100000);
    assert_eq!(inspected[0]["HostConfig"]["CpuQuota"], 200000);
    docker(
        &cli,
        &rustpy_docker::container_args(rustpy_docker::ContainerAction::Start, &id),
    );
    let snapshot = rustpy_docker::snapshot(&cli).unwrap();
    assert!(!snapshot.server_version.is_empty());
    assert!(snapshot
        .containers
        .iter()
        .any(|container| id.starts_with(&container.id) && container.is_running()));
    let mut exec = vec!["exec".into()];
    exec.extend(resources.environment_args());
    exec.push(id.clone());
    exec.extend(resources.process_prefix().unwrap());
    exec.extend([python.clone(), "-c".into(), RUNTIME.into()]);
    let runtime: serde_json::Value = serde_json::from_str(docker(&cli, &exec).trim()).unwrap();
    assert_runtime(&runtime, &cpus);
    let mut exec = vec!["exec".into()];
    exec.extend(resources.environment_args());
    exec.push(id.clone());
    exec.extend(resources.process_prefix().unwrap());
    exec.extend([python.clone(), "-c".into(), "from pathlib import Path; value=Path('input π.txt').read_text(); assert value=='owned input π\\n'; Path('output π.txt').write_text(value.upper()); print(value,end='')".into()]);
    assert_eq!(docker(&cli, &exec), "owned input π\n");
    assert_eq!(
        std::fs::read_to_string(root.path().join("output π.txt")).unwrap(),
        "OWNED INPUT Π\n"
    );
    docker(
        &cli,
        &rustpy_docker::container_args(rustpy_docker::ContainerAction::Stop, &id),
    );
    assert!(docker(&cli, &["logs".into(), id.clone()]).contains("ACTUAL_DOCKER_STARTED"));
    let state: serde_json::Value =
        serde_json::from_str(&docker(&cli, &["inspect".into(), id.clone()])).unwrap();
    assert_eq!(state[0]["State"]["Running"], false);
    docker(
        &cli,
        &rustpy_docker::container_args(rustpy_docker::ContainerAction::Remove, &id),
    );

    resources.name = format!("{unique}-sdk");
    let candidate = RemoteCandidate {
        kind: RemoteKind::Docker,
        target: image.clone(),
        python: python.clone(),
        project_root: root.path().into(),
        remote_root: None,
        docker: Some(DockerTransport {
            executable: PathBuf::from(&cli),
            resources: Some(resources.clone()),
        }),
    };
    let directory = root.path().join("adapters");
    assert!(
        DiscoveryJob::remote(candidate.clone(), directory.clone(), false).is_err(),
        "untrusted SDK target must refuse before Docker execution"
    );
    let job = DiscoveryJob::remote(candidate.clone(), directory.clone(), true).unwrap();
    let metadata = job
        .receiver
        .recv_timeout(Duration::from_secs(30))
        .expect("real Docker SDK probe must complete")
        .unwrap();
    assert_eq!(metadata.interpreter.platform, "linux");
    assert!(!metadata.interpreter.sys_path.is_empty());
    let wrapper =
        rustpy_project::interpreter_remote::install_wrapper(&candidate, &directory).unwrap();
    assert!(
        rustpy_project::interpreter_remote::configured_candidates(&directory, root.path())
            .contains(&candidate),
        "persisted transport retains exact CLI and resource authority"
    );
    let mut command = std::process::Command::new(wrapper);
    command.args(["-c", RUNTIME]);
    let (status, out, err) = rustpy_project::native_skeleton::supervise_tool_status(
        &mut command,
        &rustpy_core::CancellationToken::new(),
        Duration::from_secs(30),
        262144,
    )
    .unwrap();
    assert!(status.success(), "{}", String::from_utf8_lossy(&err));
    assert_runtime(&serde_json::from_slice(&out).unwrap(), &cpus);
    let mut missing_python = candidate.clone();
    missing_python.python = "/rustpy-missing-python".into();
    missing_python
        .docker
        .as_mut()
        .unwrap()
        .resources
        .as_mut()
        .unwrap()
        .name = format!("{unique}-missing-sdk");
    let missing = DiscoveryJob::remote(missing_python, directory, true).unwrap();
    assert!(
        missing
            .receiver
            .recv_timeout(Duration::from_secs(30))
            .unwrap()
            .is_err(),
        "actual daemon failure cannot become an empty successful SDK"
    );

    resources.name = format!("{unique}-image");
    let connection = rustpy_deploy::targets::Docker {
        executable: cli.clone(),
        resources: Some(resources),
        ..Default::default()
    };
    let spec = rustpy_deploy::targets::image_python_command(
        &connection,
        &image,
        &python,
        root.path(),
        "/work",
        &["-c".into(), RUNTIME.into()],
    )
    .unwrap();
    assert_eq!(spec.program, cli);
    let output = docker(&spec.program, &spec.args);
    assert_runtime(&serde_json::from_str(output.trim()).unwrap(), &cpus);
    assert!(
        docker(
            &cli,
            &[
                "ps".into(),
                "-aq".into(),
                "--filter".into(),
                format!("label=rustpy.acceptance={unique}")
            ]
        )
        .trim()
        .is_empty(),
        "all owned containers physically removed"
    );
}
