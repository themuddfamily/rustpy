//! Explicit real-tool acceptance for the Docker/Compose, Kubernetes and Vagrant
//! deployment targets. Every test is opt-in, never pulls unless a pull image is
//! named, and only removes resources it created itself.
#![cfg(unix)]
use rustpy_deploy::{
    exec,
    kubernetes::{self, Target},
    targets::{
        docker_args, install_python_adapter, python_command, vagrant_args, Docker, DockerArguments,
    },
    transfer::TransferOwner,
    CommandSpec,
};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn required(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("explicit {key} admission required"))
}
fn unique() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}
/// Runs like the deployment worker: owned by a transfer rooted at the project.
fn checked(root: &Path, command: &CommandSpec, seconds: u64) -> String {
    let owner = TransferOwner::new(root.into(), "acceptance".into());
    owner.run(|| {
        let out = exec::run_checked(command, Duration::from_secs(seconds), |_| None)
            .unwrap_or_else(|e| panic!("{}: {e}", command.display()));
        format!("{}{}", out.stdout_text(), out.stderr)
    })
}
fn attempt(root: &Path, command: &CommandSpec, seconds: u64) -> bool {
    let owner = TransferOwner::new(root.into(), "acceptance cleanup".into());
    owner.run(|| exec::run(command, Duration::from_secs(seconds)).is_ok_and(|out| out.success))
}
fn scratch(mount_key: &str, prefix: &str) -> PathBuf {
    let base = std::env::var(mount_key)
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    let root = base.join(format!("{prefix}-{}", unique()));
    std::fs::create_dir_all(&root).unwrap();
    dunce::canonicalize(&root).unwrap()
}

// ---------------------------------------------------------------- Docker / Compose

struct DockerOwned {
    root: PathBuf,
    connection: Docker,
    cleanup: Vec<Vec<String>>,
}
impl Drop for DockerOwned {
    fn drop(&mut self) {
        for args in self.cleanup.iter().rev() {
            if let Ok(command) = self.connection.command(args.clone()) {
                let _ = attempt(&self.root, &command, 60);
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
#[ignore = "requires explicit RUSTPY_DOCKER_ADMITTED=1, absolute CLI with Compose plugin, local pinned IMAGE and MOUNT_ROOT"]
fn configured_docker_target_operations_and_compose_use_real_daemon() {
    assert_eq!(required("RUSTPY_DOCKER_ADMITTED"), "1");
    let cli = required("RUSTPY_DOCKER_CLI");
    assert!(Path::new(&cli).is_absolute());
    let image = required("RUSTPY_DOCKER_IMAGE");
    assert!(image.contains("@sha256:"), "pinned local image required");
    let root = scratch("RUSTPY_DOCKER_MOUNT_ROOT", "docker-targets");
    let id = unique();
    let container = format!("rustpy-target-{id}");
    let built = format!("rustpy-target-build:{id}");
    let volume = format!("rustpy-target-volume-{id}");
    let network = format!("rustpy-target-network-{id}");
    let project = root
        .file_name()
        .unwrap()
        .to_string_lossy()
        .replace(|c: char| !c.is_ascii_alphanumeric(), "")
        .to_ascii_lowercase();
    let connection = Docker {
        executable: cli.clone(),
        endpoint: std::env::var("DOCKER_HOST").unwrap_or_default(),
        ..Default::default()
    };
    let mut owned = DockerOwned {
        root: root.clone(),
        connection: connection.clone(),
        cleanup: vec![
            vec!["rm".into(), "-f".into(), container.clone()],
            vec!["image".into(), "rm".into(), "-f".into(), built.clone()],
            vec!["volume".into(), "rm".into(), volume.clone()],
            vec!["network".into(), "rm".into(), network.clone()],
        ],
    };
    let op = |operation: &str, args: &DockerArguments| -> String {
        let argv = docker_args(operation, args, &root).unwrap();
        checked(&root, &connection.command_at(argv, &root).unwrap(), 300)
    };
    let with = |f: &dyn Fn(&mut DockerArguments)| {
        let mut args = DockerArguments::default();
        f(&mut args);
        args
    };

    let version = op("Test connection", &DockerArguments::default());
    assert!(version.contains("Server"), "{version}");
    let images = op("Images", &DockerArguments::default());
    let (repository, _) = image.split_once('@').unwrap();
    assert!(
        images
            .lines()
            .any(|l| l.contains(&format!("\"Repository\":\"{repository}\""))),
        "{images}"
    );

    // Optional registry round trip, only when explicitly named.
    if let Ok(pull) = std::env::var("RUSTPY_DOCKER_PULL_IMAGE") {
        owned
            .cleanup
            .push(vec!["image".into(), "rm".into(), pull.clone()]);
        op("Pull image", &with(&|a| a.image = pull.clone()));
        let listed = op("Images", &DockerArguments::default());
        let (name, tag) = pull.rsplit_once(':').unwrap_or((&pull, "latest"));
        assert!(
            listed
                .lines()
                .any(|l| l.contains(&format!("\"Repository\":\"{name}\""))
                    && l.contains(&format!("\"Tag\":\"{tag}\""))),
            "{listed}"
        );
        op("Delete image", &with(&|a| a.target = pull.clone()));
    }

    // Build a Dockerfile in the project directory.
    std::fs::write(
        root.join("Dockerfile"),
        format!("FROM {image}\nCOPY payload.txt /payload.txt\nCMD [\"python3\", \"-u\", \"-c\", \"import time; print('target container up', flush=True); time.sleep(600)\"]\n"),
    )
    .unwrap();
    std::fs::write(root.join("payload.txt"), "built payload π\n").unwrap();
    op(
        "Build Dockerfile",
        &with(&|a| {
            a.path = root.join("Dockerfile").to_string_lossy().into_owned();
            a.image = built.clone();
        }),
    );

    // Run, browse and manage the container.
    op(
        "Run image",
        &with(&|a| {
            a.image = built.clone();
            a.name = container.clone();
            a.ports = vec!["127.0.0.1::8000".into()];
            a.environment = vec!["RUSTPY_TARGET=actual π".into()];
        }),
    );
    let target = |a: &mut DockerArguments| a.target = container.clone();
    let listed = op("Containers", &DockerArguments::default());
    assert!(
        listed
            .lines()
            .any(|l| l.contains(&format!("\"Names\":\"{container}\""))
                && l.contains("\"State\":\"running\"")),
        "{listed}"
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let logs = op("Logs", &with(&target));
        if logs.contains("target container up") {
            break;
        }
        assert!(Instant::now() < deadline, "{logs}");
        std::thread::sleep(Duration::from_millis(200));
    }
    let inspect = op("Inspect", &with(&target));
    let inspect: serde_json::Value = serde_json::from_str(inspect.trim()).unwrap();
    assert_eq!(inspect[0]["Name"], format!("/{container}"));
    let environment = op("Environment", &with(&target));
    assert!(
        environment.contains("RUSTPY_TARGET=actual π"),
        "{environment}"
    );
    let ports = op("Ports", &with(&target));
    assert!(ports.contains("8000/tcp -> 127.0.0.1:"), "{ports}");
    let files = op("Files", &with(&target));
    assert!(files.lines().any(|l| l == "payload.txt"), "{files}");
    let upload = root.join("upload π.txt");
    std::fs::write(&upload, "uploaded π\n").unwrap();
    op(
        "Upload container file",
        &with(&|a| {
            a.target = container.clone();
            a.path = upload.to_string_lossy().into_owned();
            a.destination = "/tmp/uploaded.txt".into();
        }),
    );
    let download = root.join("downloaded.txt");
    op(
        "Download container file",
        &with(&|a| {
            a.target = container.clone();
            a.path = "/tmp/uploaded.txt".into();
            a.destination = download.to_string_lossy().into_owned();
        }),
    );
    assert_eq!(std::fs::read_to_string(&download).unwrap(), "uploaded π\n");
    let state = |expected: &str| {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let listed = op("Containers", &DockerArguments::default());
            let line = listed
                .lines()
                .find(|l| l.contains(&format!("\"Names\":\"{container}\"")))
                .unwrap_or_default()
                .to_string();
            if expected.is_empty() && line.is_empty()
                || !expected.is_empty() && line.contains(&format!("\"State\":\"{expected}\""))
            {
                return;
            }
            assert!(Instant::now() < deadline, "{expected}: {line}");
            std::thread::sleep(Duration::from_millis(200));
        }
    };
    op("Stop", &with(&target));
    state("exited");
    op("Start", &with(&target));
    state("running");
    op("Restart", &with(&target));
    state("running");
    op("Stop", &with(&target));
    op("Delete container", &with(&target));
    state("");
    op("Delete image", &with(&|a| a.target = built.clone()));

    // Volumes and networks.
    op("Create volume", &with(&|a| a.target = volume.clone()));
    assert!(op("Volumes", &DockerArguments::default()).contains(&format!("\"Name\":\"{volume}\"")));
    assert!(op("Inspect volume", &with(&|a| a.target = volume.clone())).contains(&volume));
    op("Delete volume", &with(&|a| a.target = volume.clone()));
    assert!(!op("Volumes", &DockerArguments::default()).contains(&format!("\"Name\":\"{volume}\"")));
    op("Create network", &with(&|a| a.target = network.clone()));
    assert!(
        op("Networks", &DockerArguments::default()).contains(&format!("\"Name\":\"{network}\""))
    );
    assert!(op("Inspect network", &with(&|a| a.target = network.clone())).contains(&network));
    op("Delete network", &with(&|a| a.target = network.clone()));
    assert!(
        !op("Networks", &DockerArguments::default()).contains(&format!("\"Name\":\"{network}\""))
    );

    // Compose project in the project directory: services, profiles, env file, scale.
    std::fs::write(
        root.join("compose.yaml"),
        format!(
            "name: {project}\nservices:\n  api:\n    image: {image}\n    pull_policy: never\n    network_mode: none\n    environment:\n      GREETING: ${{GREETING}}\n    command: [\"python3\", \"-u\", \"-c\", \"import os, time; print('compose api', os.environ['GREETING'], flush=True); time.sleep(600)\"]\n  worker:\n    image: {image}\n    pull_policy: never\n    network_mode: none\n    profiles: [\"jobs\"]\n    command: [\"python3\", \"-u\", \"-c\", \"import time; print('compose worker', flush=True); time.sleep(600)\"]\n"
        ),
    )
    .unwrap();
    std::fs::write(root.join("acceptance.env"), "GREETING=hello-π\n").unwrap();
    let compose = |f: &dyn Fn(&mut DockerArguments)| {
        with(&|a| {
            a.compose_file = "compose.yaml".into();
            a.env_file = "acceptance.env".into();
            a.profiles = vec!["jobs".into()];
            f(a);
        })
    };
    owned.cleanup.push(
        docker_args("Compose down", &compose(&|_| {}), &root)
            .unwrap()
            .into_iter()
            .chain(["--timeout".into(), "1".into()])
            .collect(),
    );
    let valid = op("Validate Compose", &compose(&|_| {}));
    let valid: serde_json::Value = serde_json::from_str(valid.trim()).unwrap();
    assert_eq!(
        valid["services"]["api"]["environment"]["GREETING"],
        "hello-π"
    );
    let services = op("Compose services", &compose(&|_| {}));
    let mut services: Vec<&str> = services
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    services.sort();
    assert_eq!(services, ["api", "worker"]);
    op("Compose up", &compose(&|_| {}));
    let running = |service: &str| {
        checked(
            &root,
            &connection
                .command(vec![
                    "ps".into(),
                    "--filter".into(),
                    format!("label=com.docker.compose.project={project}"),
                    "--filter".into(),
                    format!("label=com.docker.compose.service={service}"),
                    "--filter".into(),
                    "status=running".into(),
                    "--quiet".into(),
                ])
                .unwrap(),
            30,
        )
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count()
    };
    assert_eq!(running("api"), 1);
    assert_eq!(running("worker"), 1);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let logs = op("Compose logs", &compose(&|a| a.service = "api".into()));
        if logs.contains("compose api hello-π") {
            assert!(!logs.contains("compose worker"), "service filter: {logs}");
            break;
        }
        assert!(Instant::now() < deadline, "{logs}");
        std::thread::sleep(Duration::from_millis(200));
    }
    op(
        "Compose scale",
        &compose(&|a| {
            a.service = "api".into();
            a.scale = 2;
        }),
    );
    assert_eq!(running("api"), 2);
    op("Compose restart", &compose(&|a| a.service = "api".into()));
    assert_eq!(running("api"), 2);
    op("Compose down", &compose(&|_| {}));
    assert_eq!(running("api") + running("worker"), 0);
}

// ---------------------------------------------------------------- Kubernetes

struct Namespace {
    root: PathBuf,
    target: Target,
}
impl Drop for Namespace {
    fn drop(&mut self) {
        let mut target = self.target.clone();
        let namespace = std::mem::take(&mut target.namespace);
        if let Ok(command) = target.kubectl([
            "delete".to_string(),
            "namespace".into(),
            namespace,
            "--wait=false".into(),
        ]) {
            let _ = attempt(&self.root, &command, 60);
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
struct Forward(Child);
impl Drop for Forward {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "requires explicit RUSTPY_KUBE_ADMITTED=1, absolute RUSTPY_KUBECTL, RUSTPY_KUBE_CONTEXT of a disposable cluster and RUSTPY_KUBE_IMAGE available to it"]
fn configured_kubernetes_target_browses_logs_forwards_and_applies_on_real_cluster() {
    assert_eq!(required("RUSTPY_KUBE_ADMITTED"), "1");
    let kubectl = required("RUSTPY_KUBECTL");
    assert!(Path::new(&kubectl).is_absolute());
    let context = required("RUSTPY_KUBE_CONTEXT");
    let image = required("RUSTPY_KUBE_IMAGE");
    let root = scratch("RUSTPY_DOCKER_MOUNT_ROOT", "kubernetes-target");
    let namespace = format!("rustpy-accept-{}", unique() % 1_000_000_000);
    let mut target = Target {
        kubectl,
        kubeconfig: std::env::var("RUSTPY_KUBECONFIG").unwrap_or_default(),
        namespace: namespace.clone(),
        ..Default::default()
    };

    // Contexts are listed without a selected context; cluster calls need one.
    let contexts = checked(&root, &target.contexts().unwrap(), 30);
    assert!(contexts.lines().any(|l| l.trim() == context), "{contexts}");
    assert!(target.browse("pods", false).is_err());
    target.context = context;
    target.save(&root).unwrap();
    assert_eq!(Target::load(&root).context, target.context);

    let _owned = Namespace {
        root: root.clone(),
        target: target.clone(),
    };
    checked(
        &root,
        &target
            .kubectl(["create".to_string(), "namespace".into(), namespace.clone()])
            .unwrap(),
        60,
    );

    let manifest = root.join("app.yaml");
    std::fs::write(
        &manifest,
        format!(
            r#"apiVersion: apps/v1
kind: Deployment
metadata:
  name: web
spec:
  replicas: 1
  selector:
    matchLabels: {{app: web}}
  template:
    metadata:
      labels: {{app: web}}
    spec:
      containers:
        - name: web
          image: {image}
          imagePullPolicy: IfNotPresent
          command: ["python3", "-u", "-c"]
          args:
            - |
              import http.server
              print("kubernetes pod up", flush=True)
              class H(http.server.BaseHTTPRequestHandler):
                  def do_GET(self):
                      self.send_response(200); self.end_headers(); self.wfile.write(b"forwarded-ok")
              http.server.HTTPServer(("0.0.0.0", 8000), H).serve_forever()
          ports: [{{containerPort: 8000}}]
          readinessProbe:
            tcpSocket: {{port: 8000}}
            periodSeconds: 1
---
apiVersion: v1
kind: Service
metadata:
  name: web
spec:
  selector: {{app: web}}
  ports: [{{port: 80, targetPort: 8000}}]
---
apiVersion: v1
kind: Secret
metadata:
  name: credentials
  annotations: {{note: hidden-annotation}}
stringData:
  password: hunter2-secret
"#
        ),
    )
    .unwrap();

    // Server dry run creates nothing.
    let plan = checked(&root, &target.manifest_apply(&manifest, true).unwrap(), 60);
    assert!(plan.contains("kind: Deployment"), "{plan}");
    let deployments = kubernetes::resources(&checked(
        &root,
        &target.browse("deployments", false).unwrap(),
        30,
    ))
    .unwrap();
    assert!(deployments.is_empty(), "{deployments:?}");

    checked(&root, &target.manifest_apply(&manifest, false).unwrap(), 60);
    let deadline = Instant::now() + Duration::from_secs(180);
    let pod = loop {
        let pods =
            kubernetes::resources(&checked(&root, &target.browse("pods", false).unwrap(), 30))
                .unwrap();
        if let Some(pod) = pods
            .iter()
            .find(|p| p.name.starts_with("web-") && p.phase == "Running")
        {
            break pod.clone();
        }
        assert!(Instant::now() < deadline, "{pods:?}");
        std::thread::sleep(Duration::from_millis(500));
    };
    assert_eq!(pod.kind, "Pod");
    assert_eq!(pod.namespace, namespace);
    assert_eq!(pod.containers, ["web"]);
    let all =
        kubernetes::resources(&checked(&root, &target.browse("pods", true).unwrap(), 30)).unwrap();
    assert!(all.iter().any(|p| p.namespace == "kube-system"));
    for kind in [
        "services",
        "deployments",
        "namespaces",
        "nodes",
        "events",
        "configmaps",
    ] {
        let rows = kubernetes::resources(&checked(&root, &target.browse(kind, false).unwrap(), 30))
            .unwrap();
        assert!(!rows.is_empty(), "{kind}");
    }

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let logs = checked(&root, &target.logs(&pod.name, "web", false).unwrap(), 30);
        if logs.contains("kubernetes pod up") {
            break;
        }
        assert!(Instant::now() < deadline, "{logs}");
        std::thread::sleep(Duration::from_millis(250));
    }
    let described = checked(
        &root,
        &target.resource("describe", "pods", &pod.name).unwrap(),
        30,
    );
    assert!(
        described.contains("Namespace:") && described.contains(&namespace),
        "{described}"
    );
    let yaml = checked(
        &root,
        &target.resource("yaml", "services", "web").unwrap(),
        30,
    );
    assert!(yaml.contains("targetPort: 8000"), "{yaml}");
    // Secrets are fetched as JSON and redacted before display.
    let secret = checked(
        &root,
        &target.resource("yaml", "secrets", "credentials").unwrap(),
        30,
    );
    assert!(secret.contains("\"password\""));
    let shown = kubernetes::redact(&secret);
    assert!(shown.contains("credentials"), "{shown}");
    assert!(
        !shown.contains("aHVudGVyMi1zZWNyZXQ")
            && !shown.contains("hunter2")
            && !shown.contains("hidden-annotation"),
        "{shown}"
    );

    // Loopback port-forward to the service.
    let local = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let forward = target.port_forward("services", "web", local, 80).unwrap();
    assert!(forward.args.contains(&"--request-timeout=0".to_string()));
    let _forward = Forward(
        Command::new(&forward.program)
            .args(&forward.args)
            .current_dir(&root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let body = loop {
        let response = TcpStream::connect(("127.0.0.1", local)).and_then(|mut s| {
            s.set_read_timeout(Some(Duration::from_secs(5)))?;
            s.write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
            let mut body = String::new();
            s.read_to_string(&mut body)?;
            Ok(body)
        });
        match response {
            Ok(body) if body.contains("forwarded-ok") => break body,
            other => assert!(Instant::now() < deadline, "{other:?}"),
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    assert!(body.starts_with("HTTP/1.0 200"), "{body}");

    // Kustomize render, server dry run and apply.
    let overlay = root.join("overlay");
    std::fs::create_dir_all(&overlay).unwrap();
    std::fs::write(
        overlay.join("kustomization.yaml"),
        format!("namespace: {namespace}\nconfigMapGenerator:\n  - name: settings\n    literals: [\"MODE=accept\"]\n    options: {{disableNameSuffixHash: true}}\n"),
    )
    .unwrap();
    let rendered = checked(&root, &target.kustomize_plan(&overlay).unwrap(), 30);
    assert!(
        rendered.contains("kind: ConfigMap") && rendered.contains("MODE: accept"),
        "{rendered}"
    );
    let planned = checked(&root, &target.kustomize_apply(&overlay, true).unwrap(), 60);
    assert!(planned.contains("MODE: accept"), "{planned}");
    let names = |kind: &str| -> Vec<String> {
        kubernetes::resources(&checked(&root, &target.browse(kind, false).unwrap(), 30))
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect()
    };
    assert!(!names("configmaps").contains(&"settings".to_string()));
    checked(&root, &target.kustomize_apply(&overlay, false).unwrap(), 60);
    assert!(names("configmaps").contains(&"settings".to_string()));

    checked(
        &root,
        &target.resource("delete", "deployments", "web").unwrap(),
        120,
    );
    assert!(!names("deployments").contains(&"web".to_string()));
}

// ---------------------------------------------------------------- Vagrant

struct Machine {
    root: PathBuf,
    command: CommandSpec,
}
impl Drop for Machine {
    fn drop(&mut self) {
        let _ = attempt(&self.root, &self.command, 300);
    }
}

#[test]
#[ignore = "requires explicit RUSTPY_VAGRANT_ADMITTED=1, absolute RUSTPY_VAGRANT, a disposable RUSTPY_VAGRANT_PROJECT and RUSTPY_VAGRANT_MACHINE"]
fn configured_vagrant_machine_lifecycle_ssh_and_interpreter_adapter() {
    assert_eq!(required("RUSTPY_VAGRANT_ADMITTED"), "1");
    let vagrant = required("RUSTPY_VAGRANT");
    assert!(Path::new(&vagrant).is_absolute());
    let root = dunce::canonicalize(PathBuf::from(required("RUSTPY_VAGRANT_PROJECT"))).unwrap();
    assert!(root.join("Vagrantfile").is_file());
    let machine = required("RUSTPY_VAGRANT_MACHINE");
    let command = |operation: &str| {
        CommandSpec::new(&vagrant).args(vagrant_args(operation, &machine, "").unwrap())
    };
    let status = || {
        checked(&root, &command("Status"), 120)
            .lines()
            .filter_map(|l| {
                let mut fields = l.split(',');
                let _time = fields.next()?;
                let name = fields.next()?;
                (fields.next()? == "state" && name == machine)
                    .then(|| fields.next().map(str::to_owned))?
            })
            .next()
            .unwrap_or_default()
    };
    let _machine = Machine {
        root: root.clone(),
        command: command("Destroy"),
    };

    checked(&root, &command("Up"), 600);
    assert_eq!(status(), "running");
    let config = checked(&root, &command("SSH config"), 120);
    assert!(
        config.contains("HostName") && config.contains("User vagrant"),
        "{config}"
    );
    let boxes = checked(&root, &command("Boxes"), 120);
    assert!(!boxes.is_empty());

    // The interpreter probe and the generated adapter reach the machine's Python.
    let probe = python_command(
        None,
        &machine,
        "python3",
        &[
            "-I".into(),
            "-c".into(),
            "import sys, json; print(json.dumps([sys.version_info[0], sys.argv[1:]]))".into(),
            "it's a π".into(),
        ],
        Some(&vagrant),
    )
    .unwrap();
    let output = checked(&root, &probe, 120);
    assert!(output.contains("[3, [\"it's a \\u03c0\"]]"), "{output}");
    let adapter = install_python_adapter(None, &machine, "python3", Some(&vagrant), &root).unwrap();
    // Interpreters are launched from arbitrary working directories.
    let elsewhere = std::env::temp_dir();
    let out = Command::new(&adapter)
        .args([
            "-c",
            "import sys; print(sys.argv[1:])",
            "a b",
            "it's",
            "$HOME",
        ])
        .current_dir(&elsewhere)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("['a b', \"it's\", '$HOME']"), "{stdout}");

    checked(&root, &command("Halt"), 300);
    assert_ne!(status(), "running");
    checked(&root, &command("Up"), 600);
    checked(&root, &command("Reload"), 600);
    assert_eq!(status(), "running");
    checked(&root, &command("Destroy"), 300);
    assert_eq!(status(), "not_created");
}
