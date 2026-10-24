//! Explicit real-tool acceptance for remote interpreters (Docker image, Docker
//! Compose service, Vagrant machine) and for FTPS deployment against a genuine
//! third-party server (vsftpd). Every test is opt-in, never pulls, and removes
//! only the containers, projects and machines it created itself.
#![cfg(unix)]
use rustpy_deploy::{
    execute_sync, plan_server_sync,
    targets::{image_python_command, install_argv_adapter, Docker},
    transfer::{self, Context},
    transport_for, Server, ServerKind, SyncAction, SyncDirection,
};
use rustpy_project::interpreter_remote::{
    install_wrapper, DiscoveryJob, DockerTransport, RemoteCandidate, RemoteKind,
};
use rustpy_run::{RunConfig, RunSession, SessionStatus};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
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
fn scratch(base_key: &str, prefix: &str) -> PathBuf {
    let base = PathBuf::from(required(base_key));
    assert!(base.is_absolute() && base.is_dir(), "{base_key} must exist");
    let root = base.join(format!("{prefix}-{}", unique()));
    std::fs::create_dir_all(&root).unwrap();
    dunce::canonicalize(&root).unwrap()
}
fn tool(cli: &str, args: &[&str], cwd: &Path) -> String {
    let out = Command::new(cli)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("{cli} {args:?}: {e}"));
    assert!(
        out.status.success(),
        "{cli} {args:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}
struct Cleanup(Vec<(String, Vec<String>, PathBuf)>, Vec<PathBuf>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        for (cli, args, cwd) in self.0.iter().rev() {
            let _ = Command::new(cli)
                .args(args)
                .current_dir(cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        for directory in &self.1 {
            let _ = std::fs::remove_dir_all(directory);
        }
    }
}

/// Reports where the script runs: the mapped file, working directory, the
/// argument (a mapped project path), unbuffered output and a cwd-relative read.
const SCRIPT: &str = r#"import json, os, sys
print(json.dumps({"file": __file__, "cwd": os.getcwd(), "arg": sys.argv[1:], "unbuffered": os.environ.get("PYTHONUNBUFFERED"), "data": open("data π.txt", encoding="utf-8").read(), "version": sys.version_info[0]}), flush=True)
"#;

/// A project with `app π/main.py` and its cwd-relative data file.
fn project_script(root: &Path) -> PathBuf {
    let directory = root.join("app π");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("data π.txt"), "mapped λ").unwrap();
    let script = directory.join("main.py");
    std::fs::write(&script, SCRIPT).unwrap();
    script
}

/// Runs the script exactly like a Run configuration with the given interpreter.
fn run_configuration(interpreter: &Path, root: &Path, script: &Path) -> serde_json::Value {
    let mut config = RunConfig::for_script(script);
    config.interpreter = Some(interpreter.to_path_buf());
    config.parameters = format!("\"{}\"", root.join("app π").join("data π.txt").display());
    let mut session = RunSession::spawn(1, &config, None, root);
    let deadline = Instant::now() + Duration::from_secs(120);
    while session.is_running() {
        session.poll();
        if Instant::now() >= deadline {
            session.stop();
            panic!("remote run did not finish: {:?}", session.output);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    session.poll();
    let output: String = session
        .output
        .iter()
        .map(|chunk| chunk.text.as_str())
        .collect();
    assert_eq!(session.status, SessionStatus::Exited(0), "{output}");
    let line = output
        .lines()
        .find(|line| line.starts_with('{'))
        .unwrap_or_else(|| panic!("no script output: {output}"));
    serde_json::from_str(line).unwrap()
}
fn assert_mapped_run(value: &serde_json::Value, remote_root: &str) {
    assert_eq!(
        value["file"],
        format!("{remote_root}/app π/main.py"),
        "{value}"
    );
    assert_eq!(value["cwd"], format!("{remote_root}/app π"), "{value}");
    assert_eq!(
        value["arg"],
        serde_json::json!([format!("{remote_root}/app π/data π.txt")])
    );
    assert_eq!(
        value["unbuffered"], "1",
        "run environment reaches the target"
    );
    assert_eq!(value["data"], "mapped λ");
    assert_eq!(value["version"], 3);
}
fn probe(
    candidate: &RemoteCandidate,
    adapters: &Path,
) -> rustpy_project::interpreter::InterpreterMetadata {
    assert!(
        DiscoveryJob::remote(candidate.clone(), adapters.into(), false).is_err(),
        "untrusted projects never reach the transport"
    );
    let job = DiscoveryJob::remote(candidate.clone(), adapters.into(), true).unwrap();
    let metadata = job
        .receiver
        .recv_timeout(Duration::from_secs(60))
        .expect("probe must finish")
        .unwrap_or_else(|e| panic!("real probe failed: {e}"));
    let interpreter = &metadata.interpreter;
    assert!(interpreter.version.starts_with("3."), "{interpreter:?}");
    assert_eq!(interpreter.platform, "linux");
    assert!(
        interpreter
            .sys_path
            .iter()
            .any(|p| p.to_string_lossy().contains("site-packages")),
        "remote sys.path: {:?}",
        interpreter.sys_path
    );
    assert!(
        metadata.sysconfig.contains_key("purelib"),
        "{:?}",
        metadata.sysconfig
    );
    metadata
}
/// A minimal pure wheel, written with the host's zip support only.
fn wheel(root: &Path) -> PathBuf {
    let dist = root.join("dist");
    std::fs::create_dir_all(&dist).unwrap();
    let path = dist.join("rustpy_accept-1.0-py3-none-any.whl");
    let files: [(&str, &str); 4] = [
        (
            "rustpy_accept.py",
            "VALUE = 'installed through the remote interpreter'\n",
        ),
        (
            "rustpy_accept-1.0.dist-info/METADATA",
            "Metadata-Version: 2.1\nName: rustpy-accept\nVersion: 1.0\n",
        ),
        (
            "rustpy_accept-1.0.dist-info/WHEEL",
            "Wheel-Version: 1.0\nGenerator: rustpy\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
        ),
        ("rustpy_accept-1.0.dist-info/RECORD", ""),
    ];
    write_stored_zip(&path, &files);
    path
}
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}
fn write_stored_zip(path: &Path, files: &[(&str, &str)]) {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, body) in files {
        let offset = out.len() as u32;
        let (crc, size) = (crc32(body.as_bytes()), body.len() as u32);
        let header = |signature: u32, central: bool| {
            let mut h = Vec::new();
            h.extend(signature.to_le_bytes());
            if central {
                h.extend(20u16.to_le_bytes());
            }
            h.extend(20u16.to_le_bytes());
            h.extend([0u8; 6]); // flags, stored, time
            h.extend(0x21u16.to_le_bytes()); // 1980-01-01
            h.extend(crc.to_le_bytes());
            h.extend(size.to_le_bytes());
            h.extend(size.to_le_bytes());
            h.extend((name.len() as u16).to_le_bytes());
            h.extend(0u16.to_le_bytes());
            if central {
                h.extend([0u8; 10]); // comment, disk, internal and external attributes
                h.extend(offset.to_le_bytes());
            }
            h.extend(name.as_bytes());
            h
        };
        out.extend(header(0x0403_4b50, false));
        out.extend(body.as_bytes());
        central.extend(header(0x0201_4b50, true));
    }
    let start = out.len() as u32;
    out.extend(&central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0u8; 4]);
    out.extend((files.len() as u16).to_le_bytes());
    out.extend((files.len() as u16).to_le_bytes());
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(start.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    std::fs::write(path, out).unwrap();
}
fn finish_pip(mut task: rustpy_packages::PipTask) -> (Option<i32>, String) {
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut lines = Vec::new();
    while task.is_running() {
        lines.extend(task.poll());
        assert!(Instant::now() < deadline, "pip did not finish: {lines:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
    lines.extend(task.poll());
    (task.exit_code(), lines.join("\n"))
}
/// Install (a mapped local wheel path), list and uninstall through the wrapper.
fn packages_round_trip(wrapper: &Path, root: &Path) {
    let installed = rustpy_packages::list_installed(wrapper).expect("remote pip list");
    assert!(installed.iter().any(|p| p.name == "pip"), "{installed:?}");
    let wheel = wheel(root);
    let (code, log) =
        finish_pip(rustpy_packages::install(wrapper, wheel.to_str().unwrap(), &[]).unwrap());
    assert_eq!(code, Some(0), "{log}");
    let installed = rustpy_packages::list_installed(wrapper).unwrap();
    assert!(
        installed
            .iter()
            .any(|p| p.name == "rustpy-accept" && p.version == "1.0"),
        "{installed:?}"
    );
    let imported = Command::new(wrapper)
        .args(["-c", "import rustpy_accept; print(rustpy_accept.VALUE)"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&imported.stdout).contains("installed through the remote"),
        "{imported:?}"
    );
    let (code, log) = finish_pip(rustpy_packages::uninstall(wrapper, "rustpy-accept").unwrap());
    assert_eq!(code, Some(0), "{log}");
    assert!(!rustpy_packages::list_installed(wrapper)
        .unwrap()
        .iter()
        .any(|p| p.name == "rustpy-accept"));
}

#[test]
#[ignore = "requires explicit RUSTPY_DOCKER_ADMITTED=1, absolute RUSTPY_DOCKER_CLI, local pinned RUSTPY_DOCKER_IMAGE/PYTHON and daemon-visible RUSTPY_DOCKER_MOUNT_ROOT"]
fn configured_docker_image_interpreter_probes_runs_mapped_script_and_lists_packages() {
    assert_eq!(required("RUSTPY_DOCKER_ADMITTED"), "1");
    let cli = PathBuf::from(required("RUSTPY_DOCKER_CLI"));
    assert!(cli.is_absolute());
    let image = required("RUSTPY_DOCKER_IMAGE");
    let python = required("RUSTPY_DOCKER_PYTHON");
    let root = scratch("RUSTPY_DOCKER_MOUNT_ROOT", "remote-image");
    let adapters = scratch("RUSTPY_DOCKER_MOUNT_ROOT", "remote-image-adapters");
    let _cleanup = Cleanup(vec![], vec![root.clone(), adapters.clone()]);
    let script = project_script(&root);

    // Add Interpreter › Docker: image, default /work mapping, then a custom one.
    for remote_root in [None, Some("/srv/project π")] {
        let candidate = RemoteCandidate {
            kind: RemoteKind::Docker,
            target: image.clone(),
            python: python.clone(),
            project_root: root.clone(),
            docker: Some(DockerTransport {
                executable: cli.clone(),
                resources: None,
            }),
            remote_root: remote_root.map(str::to_owned),
        };
        let metadata = probe(&candidate, &adapters);
        assert_eq!(metadata.interpreter.prefix, PathBuf::from("/usr/local"));
        let wrapper = install_wrapper(&candidate, &adapters).unwrap();
        assert_mapped_run(
            &run_configuration(&wrapper, &root, &script),
            remote_root.unwrap_or("/work"),
        );
        let installed = rustpy_packages::list_installed(&wrapper).expect("pip list in image");
        assert!(installed.iter().any(|p| p.name == "pip"), "{installed:?}");
    }

    // Docker tool window › "Use as interpreter" route (deployment SDK adapter).
    let connection = Docker {
        executable: cli.to_string_lossy().into_owned(),
        endpoint: std::env::var("DOCKER_HOST").unwrap_or_default(),
        ..Default::default()
    };
    let command = image_python_command(&connection, &image, &python, &root, "", &[]).unwrap();
    let adapter = install_argv_adapter(&command, &root, "Docker image", Some("/work")).unwrap();
    assert_mapped_run(&run_configuration(&adapter, &root, &script), "/work");
}

#[test]
#[ignore = "requires explicit RUSTPY_DOCKER_ADMITTED=1, absolute RUSTPY_DOCKER_CLI with Compose plugin, local pinned RUSTPY_DOCKER_IMAGE/PYTHON and RUSTPY_DOCKER_MOUNT_ROOT"]
fn configured_compose_service_interpreter_probes_runs_and_manages_packages() {
    assert_eq!(required("RUSTPY_DOCKER_ADMITTED"), "1");
    let cli = required("RUSTPY_DOCKER_CLI");
    let image = required("RUSTPY_DOCKER_IMAGE");
    let python = required("RUSTPY_DOCKER_PYTHON");
    let root = scratch("RUSTPY_DOCKER_MOUNT_ROOT", "remote-compose");
    let adapters = scratch("RUSTPY_DOCKER_MOUNT_ROOT", "remote-compose-adapters");
    let project = format!("rustpy-remote-{}", unique());
    std::fs::write(
        root.join("compose.yaml"),
        format!(
            "name: {project}\nservices:\n  app:\n    image: \"{image}\"\n    init: true\n    command: [\"sleep\", \"infinity\"]\n    volumes:\n      - ./:/srv/app\n"
        ),
    )
    .unwrap();
    let _cleanup = Cleanup(
        vec![(
            cli.clone(),
            vec![
                "compose".into(),
                "down".into(),
                "--volumes".into(),
                "--timeout".into(),
                "2".into(),
            ],
            root.clone(),
        )],
        vec![root.clone(), adapters.clone()],
    );
    tool(
        &cli,
        &["compose", "up", "-d", "--pull", "never", "--wait"],
        &root,
    );
    let script = project_script(&root);
    let candidate = RemoteCandidate {
        kind: RemoteKind::Compose,
        target: "app".into(),
        python,
        project_root: root.clone(),
        docker: Some(DockerTransport {
            executable: cli.clone().into(),
            resources: None,
        }),
        remote_root: Some("/srv/app".into()),
    };
    probe(&candidate, &adapters);
    let wrapper = install_wrapper(&candidate, &adapters).unwrap();
    assert_mapped_run(&run_configuration(&wrapper, &root, &script), "/srv/app");
    packages_round_trip(&wrapper, &root);
    tool(&cli, &["compose", "down", "--timeout", "2"], &root);
    assert!(tool(
        &cli,
        &[
            "ps",
            "-a",
            "--filter",
            &format!("label=com.docker.compose.project={project}"),
            "-q"
        ],
        &root
    )
    .trim()
    .is_empty());
}

#[test]
#[ignore = "requires explicit RUSTPY_VAGRANT_ADMITTED=1, absolute RUSTPY_VAGRANT, an SSH-capable docker-provider RUSTPY_VAGRANT_IMAGE and RUSTPY_VAGRANT_ROOT"]
fn configured_vagrant_machine_interpreter_probes_runs_mapped_script_and_manages_packages() {
    assert_eq!(required("RUSTPY_VAGRANT_ADMITTED"), "1");
    let vagrant = required("RUSTPY_VAGRANT");
    assert!(Path::new(&vagrant).is_absolute());
    let image = required("RUSTPY_VAGRANT_IMAGE");
    let root = scratch("RUSTPY_VAGRANT_ROOT", "remote-vagrant");
    let adapters = scratch("RUSTPY_VAGRANT_ROOT", "remote-vagrant-adapters");
    let machine = "interp";
    std::fs::write(
        root.join("Vagrantfile"),
        format!(
            // Vagrant's synced folders and `volumes` assume Docker Desktop (Windows
            // paths) under WSL; a verbatim bind mount gives the same /vagrant share.
            "Vagrant.configure(\"2\") do |config|\n  config.vm.define \"{machine}\" do |m|\n    m.vm.synced_folder \".\", \"/vagrant\", disabled: true\n    m.vm.provider \"docker\" do |d|\n      d.image = \"{image}\"\n      d.has_ssh = true\n      d.remains_running = true\n      d.create_args = [\"--volume\", \"{}:/vagrant\"]\n    end\n  end\nend\n",
            root.display()
        ),
    )
    .unwrap();
    let _cleanup = Cleanup(
        vec![(
            vagrant.clone(),
            vec!["destroy".into(), "--force".into(), machine.into()],
            root.clone(),
        )],
        vec![root.clone(), adapters.clone()],
    );
    tool(&vagrant, &["up", machine], &root);
    let script = project_script(&root);
    let candidate = RemoteCandidate {
        kind: RemoteKind::Vagrant,
        target: machine.into(),
        python: "python3".into(),
        project_root: root.clone(),
        docker: None,
        remote_root: Some("/vagrant".into()),
    };
    probe(&candidate, &adapters);
    let wrapper = install_wrapper(&candidate, &adapters).unwrap();
    assert_mapped_run(&run_configuration(&wrapper, &root, &script), "/vagrant");
    // Launched from anywhere (console, tests): arguments survive the guest shell.
    let out = Command::new(&wrapper)
        .args([
            "-c",
            "import sys; print(sys.argv[1:])",
            "a b",
            "it's",
            "$HOME",
        ])
        .current_dir(&adapters)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("['a b', \"it's\", '$HOME']"),
        "{out:?}"
    );
    packages_round_trip(&wrapper, &root);
    tool(&vagrant, &["destroy", "--force", machine], &root);
}

// ---------------------------------------------------------------- FTPS (vsftpd)

const VSFTPD: &str = "listen=YES
listen_ipv6=NO
background=NO
listen_address=127.0.0.1
anonymous_enable=NO
local_enable=YES
write_enable=YES
local_umask=022
chroot_local_user=YES
allow_writeable_chroot=YES
secure_chroot_dir=/var/run/vsftpd/empty
pam_service_name=vsftpd
seccomp_sandbox=NO
pasv_enable=YES
ssl_enable=YES
allow_anon_ssl=NO
force_local_data_ssl=YES
force_local_logins_ssl=YES
require_ssl_reuse=NO
ssl_ciphers=HIGH
rsa_cert_file=/cfg/server.pem
rsa_private_key_file=/cfg/server-key.pem
";

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}
fn wait_listening(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "vsftpd did not listen on {port}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
#[ignore = "requires explicit RUSTPY_DOCKER_ADMITTED=1, RUSTPY_DOCKER_CLI, a local vsftpd RUSTPY_FTPS_IMAGE, RUSTPY_TEST_FTPS_OPENSSL/CURL and RUSTPY_DOCKER_MOUNT_ROOT"]
fn configured_vsftpd_explicit_and_implicit_ftps_deploy_and_refuse_plaintext() {
    assert_eq!(required("RUSTPY_DOCKER_ADMITTED"), "1");
    let cli = required("RUSTPY_DOCKER_CLI");
    let image = required("RUSTPY_FTPS_IMAGE");
    let openssl = required("RUSTPY_TEST_FTPS_OPENSSL");
    let curl = required("RUSTPY_TEST_FTPS_CURL");
    let root = scratch("RUSTPY_DOCKER_MOUNT_ROOT", "remote-ftps");
    let config = root.join("cfg");
    std::fs::create_dir(&config).unwrap();
    tool(
        &openssl,
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=localhost",
            "-addext",
            "subjectAltName=IP:127.0.0.1",
            "-keyout",
            config.join("server-key.pem").to_str().unwrap(),
            "-out",
            config.join("server.pem").to_str().unwrap(),
        ],
        &root,
    );
    let (explicit_port, implicit_port) = (free_port(), free_port());
    std::fs::write(
        config.join("explicit.conf"),
        format!("{VSFTPD}listen_port={explicit_port}\n"),
    )
    .unwrap();
    std::fs::write(
        config.join("implicit.conf"),
        format!("{VSFTPD}listen_port={implicit_port}\nimplicit_ssl=YES\n"),
    )
    .unwrap();
    let password = format!("owned-{}-p w:x", unique());
    std::fs::write(config.join("credentials"), format!("owned:{password}\n")).unwrap();
    let name = format!("rustpy-ftps-{}", unique());
    let _cleanup = Cleanup(
        vec![(
            cli.clone(),
            vec!["rm".into(), "-f".into(), name.clone()],
            root.clone(),
        )],
        vec![root.clone()],
    );
    tool(
        &cli,
        &[
            "run",
            "-d",
            "--name",
            &name,
            "--network",
            "host",
            "--cpus",
            "1",
            "-v",
            &format!("{}:/cfg:ro", config.display()),
            &image,
            "sh",
            "-c",
            "useradd -m -s /bin/bash owned && chpasswd < /cfg/credentials && \
             (vsftpd /cfg/explicit.conf & vsftpd /cfg/implicit.conf & wait)",
        ],
        &root,
    );
    wait_listening(explicit_port);
    wait_listening(implicit_port);
    let local = root.join("local");
    std::fs::create_dir(&local).unwrap();
    std::fs::write(local.join("owned π.txt"), "vsftpd TLS bytes λ\n").unwrap();
    let context = Context {
        environment: vec![
            ("NO_PROXY".into(), "127.0.0.1,localhost".into()),
            ("no_proxy".into(), "127.0.0.1,localhost".into()),
        ],
        ..Default::default()
    };
    let remote_file = |path: &str| {
        Command::new(&cli)
            .args(["exec", &name, "cat", &format!("/home/owned{path}")])
            .output()
            .unwrap()
            .stdout
    };
    for (mode, port) in [("explicit", explicit_port), ("implicit", implicit_port)] {
        let mut deployment = Server::new(
            format!("vsftpd {mode}"),
            ServerKind::Ftp {
                host: "127.0.0.1".into(),
                port,
                user: "owned".into(),
                password: Some(password.clone()),
                save_password: false,
            },
            "/",
        );
        deployment.options.ftp_tls = mode.into();
        deployment.options.curl_executable = curl.clone();
        deployment.options.timeout_seconds = 15;
        transfer::with_context(context.clone(), || {
            let error = transport_for(&deployment)
                .unwrap()
                .test_connection()
                .expect_err("a self-signed server certificate is not trusted implicitly");
            assert!(!format!("{error:#}").contains(&password));
            deployment.options.tls_ca_file = config.join("server.pem").to_string_lossy().into();
            let transport = transport_for(&deployment).unwrap();
            transport
                .test_connection()
                .unwrap_or_else(|e| panic!("{mode} FTPS connection: {e:#}"));
            let target = format!("/{mode} π.txt");
            transport
                .upload(&local.join("owned π.txt"), &target)
                .unwrap_or_else(|e| panic!("{mode} upload: {e:#}"));
            assert_eq!(remote_file(&target), b"vsftpd TLS bytes \xce\xbb\n");
            assert!(transport
                .list("/")
                .unwrap_or_else(|e| panic!("{mode} list: {e:#}"))
                .iter()
                .any(|entry| entry.name == format!("{mode} π.txt")));
            let downloaded = root.join(format!("downloaded-{mode}.txt"));
            transport
                .download(&target, &downloaded)
                .unwrap_or_else(|e| panic!("{mode} download: {e:#}"));
            assert_eq!(std::fs::read(&downloaded).unwrap(), remote_file(&target));
            let sync_root = root.join(format!("sync-{mode}"));
            std::fs::create_dir_all(sync_root.join("pkg")).unwrap();
            std::fs::write(
                sync_root.join("pkg/module.py"),
                format!("MODE = '{mode}'\n"),
            )
            .unwrap();
            let mut synced = deployment.clone();
            synced.root_path = format!("/site-{mode}");
            let transport = transport_for(&synced).unwrap();
            let items = plan_server_sync(
                transport.as_ref(),
                &synced,
                &sync_root,
                &[],
                SyncDirection::Upload,
            )
            .unwrap_or_else(|e| panic!("{mode} sync plan: {e:#}"));
            assert!(
                items
                    .iter()
                    .any(|i| i.action == SyncAction::Upload && i.rel_path == "pkg/module.py"),
                "{items:?}"
            );
            for (_, result) in execute_sync(
                transport.as_ref(),
                &synced,
                &sync_root,
                &items,
                |_, _, _| {},
            ) {
                result.unwrap_or_else(|e| panic!("{mode} sync: {e:#}"));
            }
            assert_eq!(
                remote_file(&format!("/site-{mode}/pkg/module.py")),
                format!("MODE = '{mode}'\n").into_bytes()
            );
        });
    }
    // The server enforces TLS: a plaintext FTP login is refused.
    let mut plain = Server::new(
        "vsftpd plain",
        ServerKind::Ftp {
            host: "127.0.0.1".into(),
            port: explicit_port,
            user: "owned".into(),
            password: Some(password.clone()),
            save_password: false,
        },
        "/",
    );
    plain.options.curl_executable = curl.clone();
    plain.options.timeout_seconds = 15;
    transfer::with_context(context, || {
        assert!(transport_for(&plain).unwrap().test_connection().is_err());
    });
}
