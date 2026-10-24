//! Real public SSH notebook bridge against a root-owned private loopback sshd.
//! The serial root provider wrapper owns daemon provisioning and final reaping.
#![cfg(target_os = "linux")]
use rustpy_notebook::{Cell, CellRunner, Connection, KernelState, Notebook, Output};
use std::os::unix::fs::MetadataExt;
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const LIMIT: Duration = Duration::from_secs(45);
#[derive(Clone, Debug)]
struct Identity {
    pid: u32,
    start: String,
    parent: u32,
}
fn identity(pid: u32) -> Option<Identity> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<_> = stat.rsplit_once(") ")?.1.split_whitespace().collect();
    Some(Identity {
        pid,
        parent: fields.get(1)?.parse().ok()?,
        start: fields.get(19)?.to_string(),
    })
}
fn present(owner: &Identity) -> bool {
    identity(owner.pid).is_some_and(|now| now.start == owner.start)
}
fn descendant(mut child: Identity, daemon: &Identity) -> bool {
    for _ in 0..64 {
        if child.pid == daemon.pid {
            return child.start == daemon.start;
        }
        let Some(parent) = identity(child.parent) else {
            return false;
        };
        if parent.pid == child.pid {
            return false;
        }
        child = parent;
    }
    false
}
fn selected(name: &str) -> PathBuf {
    let path =
        PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("explicit {name} required")));
    assert!(
        path.is_absolute() && path.is_file(),
        "{name} must be an admitted absolute file"
    );
    path
}
struct Client {
    runner: CellRunner,
    notebook: Notebook,
    messages: Vec<String>,
}
impl Client {
    fn poll(&mut self) {
        self.runner.poll(&mut self.notebook);
        self.messages.extend(
            self.runner
                .take_events()
                .into_iter()
                .map(|event| format!("{event:?}")),
        );
    }
    fn text(&self) -> String {
        self.notebook.cells[0]
            .outputs
            .iter()
            .filter_map(|output| match output {
                Output::Stream { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
    fn close(&mut self) {
        self.runner.shutdown();
        let deadline = Instant::now() + LIMIT;
        while self.runner.owns_process() {
            self.poll();
            assert!(
                Instant::now() < deadline,
                "SSH client/bridge retirement deadline"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.runner.shutdown();
        let deadline = Instant::now() + LIMIT;
        while self.runner.owns_process() && Instant::now() < deadline {
            self.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
#[ignore = "requires explicit root-owned private sshd receipt/config and selected Jupyter SDK; run alone under provider wrapper"]
fn configured_private_ssh_notebook_cell_host_refusal_and_physical_retirement() {
    assert_eq!(
        std::env::var("RUSTPY_JUPYTER_SSH_ADMITTED").as_deref(),
        Ok("1")
    );
    let receipt_path = selected("RUSTPY_JUPYTER_SSH_PROVIDER_RECEIPT");
    let receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
    assert_eq!(receipt["private_rustpy_loopback_provider"], true);
    let root = PathBuf::from(receipt["root"].as_str().unwrap());
    assert!(root.is_absolute());
    assert_eq!(receipt_path.parent(), Some(root.as_path()));
    let metadata = std::fs::symlink_metadata(&root).unwrap();
    assert!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0,
        "provider root must be private and owned by this actor"
    );
    let daemon = identity(receipt["daemon_pid"].as_u64().unwrap().try_into().unwrap())
        .expect("admitted private daemon must be live");
    assert_eq!(daemon.start, receipt["daemon_start"].as_str().unwrap());
    let daemon_config = root.join("sshd_config");
    let daemon_program = PathBuf::from(receipt["daemon_program"].as_str().unwrap());
    assert!(daemon_program.is_absolute() && daemon_program.is_file());
    assert_eq!(
        receipt["daemon_argv"],
        serde_json::json!([daemon_program, "-D", "-e", "-f", daemon_config]),
        "root-owned provider receipt must bind the exact Popen invocation"
    );
    assert_eq!(
        std::fs::read_to_string(&daemon_config).unwrap(),
        receipt["daemon_config_snapshot"].as_str().unwrap(),
        "private configuration must remain the captured provider input"
    );
    let daemon_executable = std::fs::read_link(format!("/proc/{}/exe", daemon.pid)).unwrap();
    assert_eq!(
        daemon_executable,
        dunce::canonicalize(&daemon_program).unwrap(),
        "live executable must match the admitted daemon"
    );
    let command = std::fs::read(format!("/proc/{}/cmdline", daemon.pid)).unwrap();
    assert!(
        command.len() <= 8192,
        "unexpected daemon command/title size"
    );
    let arguments: Vec<_> = command
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .map(|argument| String::from_utf8_lossy(argument).into_owned())
        .collect();
    // OpenSSH replaces argv storage with its listener title after startup.
    // The root Popen receipt plus live PID/start/executable bind authority;
    // /proc title is corroborating context, not an immutable argv array.
    let title = arguments.join(" ");
    assert!(
        arguments
            .iter()
            .any(|argument| Path::new(argument) == daemon_config)
            || (title.contains("[listener]") && title.contains(daemon_config.to_str().unwrap())),
        "daemon title must corroborate captured private configuration; bounded actual /proc layout={arguments:?}"
    );
    assert!(present(&daemon), "daemon identity changed during admission");
    let ssh = selected("RUSTPY_TEST_SSH");
    let python = selected("RUSTPY_JUPYTER_TEST_PYTHON");
    assert_eq!(receipt["python"].as_str(), python.to_str());
    let config = root.join("client_config");
    assert!(config.is_file());
    let host = receipt["host_alias"].as_str().unwrap();
    assert_eq!(host, "rustpy-owned-notebook");
    let kernel =
        std::env::var("RUSTPY_JUPYTER_TEST_KERNEL").expect("explicit registered kernelspec");
    assert_eq!(receipt["kernel"], kernel);
    let wrong_hosts = root.join("wrong_known_hosts");
    let wrong_host_bytes = std::fs::read(&wrong_hosts)
        .expect("provider must supply wrong host key for this exact private endpoint");
    assert!(!wrong_host_bytes.is_empty());
    let refusal = root.join("refusal-client-config");
    let quoted = |path: &Path| serde_json::to_string(path.to_str().unwrap()).unwrap();
    std::fs::write(&refusal,format!("Host *\n  UserKnownHostsFile {}\n  GlobalKnownHostsFile /dev/null\n  StrictHostKeyChecking no\nInclude {}\n",quoted(&wrong_hosts),quoted(&config))).unwrap();
    for (selected_config, success) in [(&refusal, false), (&config, true)] {
        let marker = root.join(if success {
            "accepted-notebook-cell.json"
        } else {
            "refused-notebook-cell.json"
        });
        assert!(!marker.exists(), "private provider case must be fresh");
        let source = format!(
            "import os,sys,json,pathlib\nr={{'pid':os.getpid(),'bridge':os.getppid(),'python':sys.executable,'value':6*7,'runtime':os.environ.get('JUPYTER_RUNTIME_DIR'),'config':os.environ.get('JUPYTER_CONFIG_DIR')}}\npathlib.Path({}).write_text(json.dumps(r))\nprint('ACTUAL_SSH_NOTEBOOK',json.dumps(r))",
            serde_json::to_string(&marker.to_string_lossy()).unwrap()
        );
        let runner = CellRunner::start_with_connection(
            &python,
            &root,
            &[],
            "",
            Connection {
                kind: "ssh".into(),
                host: host.into(),
                kernel: kernel.clone(),
                python: python.to_string_lossy().into_owned(),
                cwd: root.to_string_lossy().into_owned(),
                ssh_program: Some(ssh.clone()),
                ssh_config_file: Some(selected_config.clone()),
                ..Default::default()
            },
        )
        .expect("actual public SSH notebook transport must start");
        let mut client = Client {
            runner,
            notebook: Notebook::empty(),
            messages: Vec::new(),
        };
        client.notebook.cells = vec![Cell::code(source)];
        assert!(client.runner.run_cell(&mut client.notebook, 0));
        let deadline = Instant::now() + LIMIT;
        loop {
            client.poll();
            if success && client.text().contains("ACTUAL_SSH_NOTEBOOK") && !client.runner.is_busy()
            {
                break;
            }
            if !success
                && client.runner.state() == KernelState::Dead
                && !client.runner.owns_process()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "real SSH notebook did not reach case terminal state: {:?}",
                client.messages
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        if success {
            let result: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
            assert_eq!(result["value"], 42);
            assert_eq!(
                Path::new(result["python"].as_str().unwrap()),
                python.as_path()
            );
            assert_eq!(
                Path::new(result["runtime"].as_str().unwrap()),
                root.join("jupyter-runtime")
            );
            assert_eq!(
                Path::new(result["config"].as_str().unwrap()),
                root.join("jupyter-config")
            );
            let kernel_owner =
                identity(result["pid"].as_u64().unwrap().try_into().unwrap()).unwrap();
            let bridge_owner =
                identity(result["bridge"].as_u64().unwrap().try_into().unwrap()).unwrap();
            assert!(
                descendant(kernel_owner.clone(), &daemon)
                    && descendant(bridge_owner.clone(), &daemon),
                "only the admitted daemon's real remote kernel/bridge are acceptance subjects"
            );
            client.close();
            let deadline = Instant::now() + LIMIT;
            while present(&kernel_owner) || present(&bridge_owner) {
                assert!(
                    Instant::now() < deadline,
                    "remote kernel/bridge must physically retire while daemon reaps them"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        } else {
            assert!(!marker.exists() && !client.text().contains("ACTUAL_SSH_NOTEBOOK"));
            assert!(
                client
                    .messages
                    .join("\n")
                    .contains("REMOTE HOST IDENTIFICATION HAS CHANGED"),
                "actual changed-host-key refusal required, not arbitrary startup failure"
            );
            assert_eq!(
                std::fs::read(&wrong_hosts).unwrap(),
                wrong_host_bytes,
                "CLI strict policy must override permissive private config without replacing trusted keys"
            );
            client.close();
        }
        assert!(
            present(&daemon),
            "provider wrapper must retain daemon until its sessions retire"
        );
    }
}
