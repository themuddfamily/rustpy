//! Genuine pinned local image through typed RunSession and public deployment plans.
//! No pulls, builds, Compose/WSL claims, or unrelated daemon mutations.
#![cfg(target_os = "linux")]
use rustpy_deploy::targets::{docker_args, Docker, DockerArguments};
use rustpy_project::configuration_types::{quote_arguments, ConfigurationSpec, ConfigurationType};
use rustpy_run::{DockerResources, OutputKind, RunConfig, RunSession, SessionStatus};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("explicit {name} admission required"))
}
fn raw(cli: &str, args: &[String]) -> String {
    rustpy_docker::run(
        cli,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
        Duration::from_secs(30),
    )
    .unwrap()
}
fn allowed_cpus() -> Vec<u32> {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let list = status
        .lines()
        .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
        .unwrap()
        .trim();
    let mut cpus = Vec::new();
    for part in list.split(',') {
        let (start, end) = part.split_once('-').unwrap_or((part, part));
        cpus.extend(start.parse::<u32>().unwrap()..=end.parse::<u32>().unwrap());
    }
    assert_eq!(
        cpus.len(),
        2,
        "central root actor must own exactly two CPUs"
    );
    let cores = cpus
        .iter()
        .map(|cpu| {
            let path = format!("/sys/devices/system/cpu/cpu{cpu}/topology");
            (
                std::fs::read_to_string(format!("{path}/physical_package_id")).unwrap(),
                std::fs::read_to_string(format!("{path}/core_id")).unwrap(),
            )
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        cores.len(),
        2,
        "central actor must use two distinct physical cores"
    );
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
    assert!(nice >= 10);
    cpus
}
struct Fixture {
    cli: String,
    image: String,
    python: String,
    endpoint: String,
    config: String,
    cpus: Vec<u32>,
    label: String,
    root: tempfile::TempDir,
    policy: DockerResources,
}
impl Fixture {
    fn new() -> Self {
        assert_eq!(required("RUSTPY_DOCKER_ADMITTED"), "1");
        let cli = required("RUSTPY_DOCKER_CLI");
        assert!(Path::new(&cli).is_absolute());
        let mut magic = [0; 4];
        std::fs::File::open(&cli)
            .unwrap()
            .read_exact(&mut magic)
            .unwrap();
        assert_eq!(&magic, b"\x7fELF", "must use genuine admitted Linux CLI");
        let image = required("RUSTPY_DOCKER_IMAGE");
        let (_, digest) = image
            .rsplit_once("@sha256:")
            .expect("explicit local RepoDigest required");
        assert_eq!(digest.len(), 64);
        assert!(digest.chars().all(|value| value.is_ascii_hexdigit()));
        let python = required("RUSTPY_DOCKER_PYTHON");
        let nice = required("RUSTPY_DOCKER_NICE");
        assert!(python.starts_with('/') && nice.starts_with('/'));
        let endpoint = required("DOCKER_HOST");
        assert!(endpoint.starts_with("unix:///"));
        let config = required("DOCKER_CONFIG");
        assert!(Path::new(&config).is_absolute());
        let mount = PathBuf::from(required("RUSTPY_DOCKER_MOUNT_ROOT"));
        assert!(mount.is_absolute() && mount.is_dir());
        let root = tempfile::Builder::new()
            .prefix("docker typed deploy π ")
            .tempdir_in(mount)
            .unwrap();
        std::fs::write(root.path().join("input π.txt"), "actual mapped input π\n").unwrap();
        let cpus = allowed_cpus();
        let label = format!(
            "rustpy-run-deploy-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let policy = DockerResources {
            cpuset_cpus: cpus
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(","),
            cpu_millis: 2000,
            threads: 2,
            nice: 10,
            nice_program: nice,
            name: format!("{label}-typed"),
            labels: BTreeMap::from([("rustpy.acceptance".into(), label.clone())]),
        };
        raw(&cli, &["image".into(), "inspect".into(), image.clone()]); // Missing image fails; never pull.
        Self {
            cli,
            image,
            python,
            endpoint,
            config,
            cpus,
            label,
            root,
            policy,
        }
    }
    fn names(&self) -> String {
        raw(
            &self.cli,
            &[
                "ps".into(),
                "-aq".into(),
                "--filter".into(),
                format!("label=rustpy.acceptance={}", self.label),
            ],
        )
    }
    fn configuration(&self, script: &str) -> RunConfig {
        let mut spec = ConfigurationSpec::new(ConfigurationType::Docker);
        spec.executable = self.cli.clone().into();
        spec.image = self.image.clone();
        spec.target = self.python.clone();
        spec.docker_resources = Some(self.policy.clone());
        spec.arguments = vec![
            "--pull=never".into(),
            "--network=none".into(),
            "--volume".into(),
            format!("{}:/work:ro", self.root.path().display()),
            "--workdir".into(),
            "/work".into(),
        ];
        let mut configuration = RunConfig {
            name: "Actual typed Docker".into(),
            parameters: quote_arguments(&["-c".into(), script.into()]),
            working_dir: Some(self.root.path().into()),
            type_options: Some(spec.clone()),
            ..Default::default()
        };
        configuration
            .env
            .insert("DOCKER_HOST".into(), self.endpoint.clone());
        configuration
            .env
            .insert("DOCKER_CONFIG".into(), self.config.clone());
        spec.materialize(&configuration).unwrap()
    }
    fn connection(&self) -> Docker {
        let mut policy = self.policy.clone();
        policy.name = format!("{}-deployment", self.label);
        Docker {
            name: "Actual deployment".into(),
            executable: self.cli.clone(),
            endpoint: self.endpoint.clone(),
            resources: Some(policy),
            ..Default::default()
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Failure cleanup is restricted to this fixture's unique label; successful
        // assertions must prove retirement BEFORE this guard gets an opportunity.
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
        for id in listed
            .lines()
            .filter(|id| !id.is_empty() && id.chars().all(|value| value.is_ascii_hexdigit()))
        {
            let _ = rustpy_docker::run(&self.cli, &["rm", "-f", id], Duration::from_secs(15));
        }
    }
}
fn finish(run: &mut RunSession) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    while run.is_running() {
        run.poll();
        if Instant::now() >= deadline {
            run.stop();
            panic!("actual Docker run did not retire: {:?}", run.output);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    run.poll();
    run.output.iter().map(|chunk| chunk.text.as_str()).collect()
}
fn actual_stdout(run: &RunSession) -> String {
    run.output
        .iter()
        .filter(|chunk| chunk.kind == OutputKind::Stdout)
        .map(|chunk| chunk.text.as_str())
        .collect()
}
const PROBE: &str = r#"import json,os,pathlib
p=pathlib.Path('/sys/fs/cgroup/cpu.max')
quota=list(map(int,p.read_text().split())) if p.exists() else [int(pathlib.Path('/sys/fs/cgroup/cpu/cpu.cfs_quota_us').read_text()),int(pathlib.Path('/sys/fs/cgroup/cpu/cpu.cfs_period_us').read_text())]
print('RUSTPY_ACTUAL_DOCKER='+json.dumps(dict(cpus=sorted(os.sched_getaffinity(0)),nice=os.getpriority(os.PRIO_PROCESS,0),threads=len(list(pathlib.Path('/proc/self/task').iterdir())),quota=quota,omp=os.environ['OMP_NUM_THREADS'],blas=os.environ['OPENBLAS_NUM_THREADS'],cwd=os.getcwd(),input=pathlib.Path('input π.txt').read_text())), flush=True)
"#;
fn assert_runtime(output: &str, cpus: &[u32]) {
    let value: serde_json::Value = serde_json::from_str(
        output
            .lines()
            .find_map(|line| line.strip_prefix("RUSTPY_ACTUAL_DOCKER="))
            .unwrap_or_else(|| panic!("actual runtime record missing: {output}")),
    )
    .unwrap();
    assert_eq!(value["cpus"], serde_json::json!(cpus));
    assert!(value["nice"].as_i64().unwrap() >= 10);
    assert!(value["threads"].as_u64().unwrap() <= 2);
    let quota = value["quota"].as_array().unwrap();
    assert!(
        quota[0].as_i64().unwrap() > 0
            && quota[0].as_i64().unwrap() <= 2 * quota[1].as_i64().unwrap()
    );
    assert_eq!(value["omp"], "2");
    assert_eq!(value["blas"], "2");
    assert_eq!(value["cwd"], "/work");
    assert_eq!(value["input"], "actual mapped input π\n");
}
fn deployment(
    connection: &Docker,
    operation: &str,
    arguments: &DockerArguments,
    root: &Path,
) -> String {
    let args = docker_args(operation, arguments, root).unwrap();
    let command = connection.command(args).unwrap();
    rustpy_deploy::exec::run_checked(&command, Duration::from_secs(30), |_| None)
        .unwrap()
        .stdout_text()
}

#[test]
#[ignore = "explicit genuine CLI/local pinned Python image, daemon-visible mount and central two-core actor required"]
fn configured_typed_docker_run_and_public_deployment_lifecycle_apply_real_resource_policy() {
    let fixture = Fixture::new();
    let configuration = fixture.configuration(PROBE);
    let mut run = RunSession::spawn(902, &configuration, None, fixture.root.path());
    let output = finish(&mut run);
    assert_eq!(run.status, SessionStatus::Exited(0), "{output}");
    assert_runtime(&output, &fixture.cpus);
    assert!(
        fixture.names().trim().is_empty(),
        "typed --rm owner survives normal retirement"
    );
    let connection = fixture.connection();
    let arguments = DockerArguments {
        image: fixture.image.clone(),
        command: vec![fixture.python.clone(), "-c".into(), PROBE.into()],
        volumes: vec![format!("{}:/work:ro", fixture.root.path().display())],
        ..Default::default()
    };
    // The production deployment form has no workdir field; explicit command
    // changes cwd before calling the same actual capped runtime probe.
    let mut arguments = arguments;
    arguments.command[2] = format!("import os; os.chdir('/work')\n{PROBE}");
    let created = deployment(
        &connection,
        "Create container",
        &arguments,
        fixture.root.path(),
    );
    let id = created.trim();
    assert!(!id.is_empty() && id.chars().all(|value| value.is_ascii_hexdigit()));
    let selected = DockerArguments {
        target: id.into(),
        ..Default::default()
    };
    let inspected: serde_json::Value = serde_json::from_str(&deployment(
        &connection,
        "Inspect",
        &selected,
        fixture.root.path(),
    ))
    .unwrap();
    assert_eq!(
        inspected[0]["HostConfig"]["CpusetCpus"],
        fixture.policy.cpuset_cpus
    );
    assert_eq!(inspected[0]["HostConfig"]["CpuQuota"], 200000);
    deployment(&connection, "Start", &selected, fixture.root.path());
    raw(&fixture.cli, &["wait".into(), id.into()]);
    let logs = deployment(&connection, "Logs", &selected, fixture.root.path());
    // Docker's --timestamps logs prefix is presentation data, not probe JSON.
    let normalized = logs
        .lines()
        .filter_map(|line| {
            line.find("RUSTPY_ACTUAL_DOCKER=")
                .map(|offset| &line[offset..])
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert_runtime(&normalized, &fixture.cpus);
    deployment(
        &connection,
        "Delete container",
        &selected,
        fixture.root.path(),
    );
    assert!(
        fixture.names().trim().is_empty(),
        "public deployment deletion must retire its own daemon resource"
    );
    let mut waiting = arguments.clone();
    waiting.command = vec![
        fixture.python.clone(),
        "-c".into(),
        format!("{PROBE}\nimport time;time.sleep(60)"),
    ];
    waiting.command[2] = format!("import os;os.chdir('/work')\n{}", waiting.command[2]);
    let id = deployment(&connection, "Run image", &waiting, fixture.root.path())
        .trim()
        .to_owned();
    assert!(!id.is_empty());
    let selected = DockerArguments {
        target: id.clone(),
        ..Default::default()
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let logs = deployment(&connection, "Logs", &selected, fixture.root.path());
        if logs.contains("RUSTPY_ACTUAL_DOCKER=") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "actual deployment process never ready"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    deployment(&connection, "Stop", &selected, fixture.root.path());
    let inspected: serde_json::Value = serde_json::from_str(&deployment(
        &connection,
        "Inspect",
        &selected,
        fixture.root.path(),
    ))
    .unwrap();
    assert_eq!(inspected[0]["State"]["Running"], false);
    deployment(
        &connection,
        "Delete container",
        &selected,
        fixture.root.path(),
    );
    assert!(fixture.names().trim().is_empty());
}

#[test]
#[ignore = "explicit real Docker admission; physical daemon owner retirement after typed RunSession Stop"]
fn configured_typed_docker_stop_retires_actual_daemon_owned_container() {
    let fixture = Fixture::new();
    let configuration = fixture.configuration(&format!("{PROBE}\nimport time;time.sleep(60)"));
    let mut run = RunSession::spawn(903, &configuration, None, fixture.root.path());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        run.poll();
        if actual_stdout(&run)
            .lines()
            .any(|line| line.starts_with("RUSTPY_ACTUAL_DOCKER="))
        {
            break;
        }
        assert!(
            run.is_running() && Instant::now() < deadline,
            "actual Docker process readiness missing: {:?}",
            run.output
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    run.stop();
    finish(&mut run);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if fixture.names().trim().is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "typed Stop left an owned daemon container after local CLI retirement"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "explicit genuine admitted Docker daemon and pinned local image; no pulls"]
fn configured_typed_docker_pre_create_refusals_finish_without_claiming_a_container() {
    let fixture = Fixture::new();
    let configuration = fixture.configuration(PROBE);
    let mut spec = configuration.type_options.clone().unwrap();
    spec.image = format!("rustpy-absent-{}:never", fixture.label);
    let absent = spec.materialize(&configuration).unwrap();
    let mut run = RunSession::spawn(904, &absent, None, fixture.root.path());
    let output = finish(&mut run);
    assert_eq!(run.status, SessionStatus::Exited(125), "{output}");
    assert!(output.contains("No such image"), "{output}");
    assert!(fixture.names().trim().is_empty());

    let mut invalid = configuration;
    if let rustpy_run::RunConfigKind::Program { args, .. } = &mut invalid.kind {
        args.insert(3, "--rustpy-unknown-option".into());
    } else {
        panic!("typed Docker must materialize to the actual program route");
    }
    let mut run = RunSession::spawn(905, &invalid, None, fixture.root.path());
    let output = finish(&mut run);
    assert_eq!(run.status, SessionStatus::Exited(125), "{output}");
    assert!(output.contains("unknown flag"), "{output}");
    assert!(fixture.names().trim().is_empty());
}

#[test]
#[ignore = "explicit real Docker admission; quick completion and immediate Stop ownership barriers"]
fn configured_typed_docker_quick_completion_and_immediate_stop_retire_daemon_ownership() {
    let fixture = Fixture::new();
    for terminal in [false, true] {
        let mut configuration = fixture.configuration("print('quick actual π', flush=True)");
        configuration.emulate_terminal = terminal;
        let mut run = RunSession::spawn(906, &configuration, None, fixture.root.path());
        let output = finish(&mut run);
        assert_eq!(run.status, SessionStatus::Exited(0), "{output}");
        assert!(
            actual_stdout(&run)
                .lines()
                .any(|line| line == "quick actual π"),
            "{output}"
        );
        assert!(fixture.names().trim().is_empty());
        let mut configuration = fixture.configuration("import time; time.sleep(60)");
        configuration.emulate_terminal = terminal;
        let mut run = RunSession::spawn(907, &configuration, None, fixture.root.path());
        run.stop();
        let output = finish(&mut run);
        assert!(!matches!(run.status, SessionStatus::Failed(_)), "{output}");
        assert!(
            fixture.names().trim().is_empty(),
            "immediate Stop must reconcile the actual daemon before terminal publication"
        );
    }
}

#[test]
#[ignore = "explicit real Docker admission; RunPanel must retain failed-status daemon ownership"]
fn configured_failed_docker_console_cannot_close_or_replace_before_physical_retirement() {
    let fixture = Fixture::new();
    let configuration = fixture.configuration(&format!("{PROBE}\nimport time;time.sleep(60)"));
    let mut run = RunSession::spawn(908, &configuration, None, fixture.root.path());
    let deadline = Instant::now() + Duration::from_secs(10);
    while !actual_stdout(&run)
        .lines()
        .any(|line| line.starts_with("RUSTPY_ACTUAL_DOCKER="))
    {
        run.poll();
        assert!(
            run.is_running() && Instant::now() < deadline,
            "actual Docker owner never became ready: {}",
            run.text()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!fixture.names().trim().is_empty());
    // Exercise the presentation/physical-ownership distinction with an actual
    // daemon owner. The transport-failure classification is tested separately;
    // changing the displayed status must never detach this real container.
    run.status = SessionStatus::Failed("unresolved retirement presentation".into());
    assert!(!run.is_running());
    assert!(run.owns_process());
    let mut panel = rustpy_ui::run_panel::RunPanel::default();
    panel.settings.open_new_tab = false;
    panel.add_session(run);
    assert!(panel.has_running());
    assert_eq!(panel.process_stamps().len(), 1);
    assert!(!panel.can_replace(908));
    panel.request_close(908);
    assert_eq!(
        panel.sessions.len(),
        1,
        "close must wait for physical ownership"
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    while panel.has_running() {
        panel.poll();
        assert!(
            Instant::now() < deadline,
            "Docker ownership did not retire: {:?}",
            panel.process_stamps()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    panel.poll();
    assert!(fixture.names().trim().is_empty());
    assert!(
        panel.sessions.is_empty(),
        "deferred close completes only after physical retirement"
    );
}
