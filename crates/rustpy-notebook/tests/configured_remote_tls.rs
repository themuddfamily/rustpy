//! Real owned Jupyter Server HTTPS/WSS acceptance; explicitly run alone on Linux.
#![cfg(target_os = "linux")]
use rustpy_notebook::{Cell, CellRunner, Connection, KernelState, Notebook, Output};
use std::os::unix::process::CommandExt;
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const LIMIT: Duration = Duration::from_secs(45);
const SERVER: &str = r#"import json, os, pathlib, sys
from traitlets.config import Config
from jupyter_server.serverapp import ServerApp
options=json.load(sys.stdin)
c=Config()
c.ServerApp.ip='127.0.0.1'
c.ServerApp.port=0
c.ServerApp.port_retries=0
c.ServerApp.open_browser=False
c.ServerApp.allow_root=True
c.ServerApp.root_dir=options['root']
c.ServerApp.certfile=options['cert']
c.ServerApp.keyfile=options['key']
c.IdentityProvider.token=options['token']
app=ServerApp(config=c)
app.initialize([])
def ready():
    sockets=list(app.http_server._sockets.values())
    assert sockets and all(sock.getsockname()[0]=='127.0.0.1' for sock in sockets)
    target=pathlib.Path(options['ready'])
    temporary=target.with_suffix('.tmp')
    temporary.write_text(json.dumps({'pid':os.getpid(),'port':sockets[0].getsockname()[1]}))
    temporary.replace(target)
app.io_loop.add_callback(ready)
app.start()
"#;

#[derive(Clone, Debug)]
struct Identity {
    pid: u32,
    start: String,
}
fn identity(pid: u32) -> Option<Identity> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    Some(Identity {
        pid,
        start: stat.rsplit_once(") ")?.1.split_whitespace().nth(19)?.into(),
    })
}
fn present(owner: &Identity) -> bool {
    identity(owner.pid).is_some_and(|now| now.start == owner.start)
}
fn required(name: &str) -> PathBuf {
    let path =
        PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("explicit {name} required")));
    assert!(
        path.is_absolute() && path.is_file(),
        "{name}: admitted absolute file required"
    );
    path
}
struct Provider {
    child: Child,
    owner: Identity,
    stopped: bool,
}
impl Provider {
    fn stop(&mut self) {
        if self.stopped {
            return;
        }
        // This unreaped Child is owned by this fixture; its PID cannot be reused.
        assert!(
            unsafe { libc::kill(self.child.id() as i32, libc::SIGTERM) } == 0
                || !present(&self.owner)
        );
        let deadline = Instant::now() + LIMIT;
        loop {
            if self.child.try_wait().unwrap().is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "owned Jupyter Server failed graceful retirement"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        self.stopped = true;
        assert!(!present(&self.owner));
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        if !self.stopped && !matches!(self.child.try_wait(), Ok(Some(_))) {
            // Kill only the fresh process group created by Command::process_group.
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
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
    fn close(&mut self) {
        self.runner.shutdown();
        let deadline = Instant::now() + LIMIT;
        while self.runner.owns_process() {
            self.poll();
            assert!(
                Instant::now() < deadline,
                "remote client bridge must physically retire"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
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
#[ignore = "requires explicit admitted SDK/kernelspec and loopback TLS cert/key/CA/wrong CA; real owned Jupyter Server, run alone"]
fn configured_owned_remote_jupyter_tls_token_refusal_cell_and_retirement() {
    assert_eq!(
        std::env::var("RUSTPY_JUPYTER_REMOTE_ADMITTED").as_deref(),
        Ok("1")
    );
    let python = required("RUSTPY_JUPYTER_TEST_PYTHON");
    let cert = required("RUSTPY_JUPYTER_REMOTE_CERT");
    let key = required("RUSTPY_JUPYTER_REMOTE_KEY");
    let ca = required("RUSTPY_JUPYTER_REMOTE_CA");
    let wrong_ca = required("RUSTPY_JUPYTER_REMOTE_WRONG_CA");
    assert_ne!(
        std::fs::read(&ca).unwrap(),
        std::fs::read(&wrong_ca).unwrap()
    );
    let kernel =
        std::env::var("RUSTPY_JUPYTER_TEST_KERNEL").expect("explicit registered kernelspec");
    assert!(!kernel.is_empty() && kernel != "project");
    let sdk = python.parent().unwrap().parent().unwrap();
    assert!(
        (sdk.join("pyvenv.cfg")).is_file(),
        "admitted isolated SDK required"
    );
    // Keep the private provider logs/runtime as root-reviewable evidence even
    // if an actual provider fails. Never include token-bearing logs in panic.
    let root = tempfile::tempdir().unwrap().keep().canonicalize().unwrap();
    eprintln!("owned remote Jupyter TLS evidence: {}", root.display());
    let mut environment = Vec::new();
    for (name, folder) in [
        ("JUPYTER_CONFIG_DIR", "config"),
        ("JUPYTER_DATA_DIR", "data"),
        ("JUPYTER_RUNTIME_DIR", "runtime"),
        ("IPYTHONDIR", "ipython"),
        ("MPLCONFIGDIR", "matplotlib"),
    ] {
        let path = root.join(folder);
        std::fs::create_dir(&path).unwrap();
        environment.push((name.to_string(), path.to_string_lossy().into_owned()));
    }
    environment.extend([
        (
            "JUPYTER_PATH".into(),
            sdk.join("share/jupyter").to_string_lossy().into_owned(),
        ),
        ("PYTHONNOUSERSITE".into(), "1".into()),
        ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
        ("MPLBACKEND".into(), "Agg".into()),
        ("NO_PROXY".into(), "127.0.0.1,localhost".into()),
        ("no_proxy".into(), "127.0.0.1,localhost".into()),
    ]);
    let token = format!(
        "owned-notebook-fixture-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let ready = root.join("ready.json");
    let mut command = Command::new(&python);
    command
        .args(["-I", "-u", "-c", SERVER])
        .current_dir(&root)
        .envs(environment.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(std::fs::File::create(root.join("server.stdout.log")).unwrap())
        .stderr(std::fs::File::create(root.join("server.stderr.log")).unwrap())
        .process_group(0);
    let child = command
        .spawn()
        .expect("real admitted Jupyter Server must start");
    let owner = identity(child.id()).expect("Linux server process identity");
    let mut provider = Provider {
        child,
        owner,
        stopped: false,
    };
    serde_json::to_writer(
        provider.child.stdin.as_mut().unwrap(),
        &serde_json::json!({"root":root,"cert":cert,"key":key,"token":token,"ready":ready}),
    )
    .unwrap();
    provider.child.stdin.take().unwrap().flush().unwrap();
    let deadline = Instant::now() + LIMIT;
    while !ready.is_file() {
        assert!(
            provider.child.try_wait().unwrap().is_none(),
            "real TLS server exited; inspect private provider logs"
        );
        assert!(Instant::now() < deadline, "real TLS server not ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    let receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&ready).unwrap()).unwrap();
    assert_eq!(receipt["pid"].as_u64(), Some(u64::from(provider.owner.pid)));
    let port: u16 = receipt["port"].as_u64().unwrap().try_into().unwrap();
    assert_ne!(port, 0);
    let endpoint = format!("https://127.0.0.1:{port}/");
    for (case, selected_ca, selected_token, success) in [
        ("wrong-ca", &wrong_ca, token.as_str(), false),
        ("wrong-token", &ca, "wrong-owned-fixture-token", false),
        ("accepted", &ca, token.as_str(), true),
    ] {
        let marker = root.join(format!("{case}-cell.json"));
        let source = format!(
            "import json,os,pathlib,sys\nr={{'pid':os.getpid(),'python':sys.executable,'value':6*7}}\npathlib.Path({}).write_text(json.dumps(r))\nprint('ACTUAL_REMOTE_TLS_CELL',json.dumps(r))",
            serde_json::to_string(&marker.to_string_lossy()).unwrap()
        );
        let runner = CellRunner::start_with_connection(
            &python,
            &root,
            &environment,
            "",
            Connection {
                kind: "remote".into(),
                kernel: kernel.clone(),
                url: endpoint.clone(),
                token: selected_token.into(),
                verify_ssl: true,
                ca_file: selected_ca.to_string_lossy().into_owned(),
                ..Default::default()
            },
        )
        .expect("public remote bridge startup");
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
            if success
                && client.text().contains("ACTUAL_REMOTE_TLS_CELL")
                && !client.runner.is_busy()
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
                "configured remote case {case} did not reach its actual terminal oracle"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let messages = client.messages.join("\n");
        assert!(
            !messages.contains(&token) && !client.text().contains(&token),
            "credentials must not enter client output"
        );
        if success {
            let result: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
            assert_eq!(result["value"], 42);
            assert_eq!(
                Path::new(result["python"].as_str().unwrap()),
                python.as_path(),
                "kernel must retain admitted venv executable spelling"
            );
            let kernel_owner = identity(result["pid"].as_u64().unwrap().try_into().unwrap())
                .expect("actual remote kernel alive");
            client.close();
            let deadline = Instant::now() + LIMIT;
            while present(&kernel_owner) {
                assert!(
                    Instant::now() < deadline,
                    "owned remote kernel survived session deletion"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                present(&provider.owner),
                "closing client must preserve separately owned server"
            );
        } else {
            assert!(
                !marker.exists() && !client.text().contains("ACTUAL_REMOTE_TLS_CELL"),
                "refused TLS/auth must never execute queued cell"
            );
            if case == "wrong-ca" {
                assert!(
                    messages.contains("CERTIFICATE_VERIFY_FAILED")
                        || messages
                            .to_lowercase()
                            .contains("certificate verify failed")
                );
            } else {
                assert!(
                    messages.contains("403") || messages.contains("401"),
                    "actual credential refusal required"
                );
            }
            client.close();
        }
    }
    provider.stop();
}

fn password_audit(path: &Path, after: u64, sessions: usize, kernels: usize) -> u64 {
    let deadline = Instant::now() + LIMIT;
    loop {
        if let Ok(bytes) = std::fs::read(path) {
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let revision = value["revision"].as_u64().unwrap();
            if revision > after
                && value["sessions"].as_u64() == Some(sessions as u64)
                && value["kernels"].as_u64() == Some(kernels as u64)
            {
                return revision;
            }
        }
        assert!(
            Instant::now() < deadline,
            "owned password server did not reach required session/kernel state"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "requires explicitly admitted real password-only Jupyter HTTPS/WSS SDK/kernelspec and TLS material; Linux, run alone"]
fn configured_owned_remote_jupyter_password_tls_login_refusal_cell_and_retirement() {
    assert_eq!(
        std::env::var("RUSTPY_JUPYTER_REMOTE_ADMITTED").as_deref(),
        Ok("1")
    );
    let python = required("RUSTPY_JUPYTER_TEST_PYTHON");
    let cert = required("RUSTPY_JUPYTER_REMOTE_CERT");
    let key = required("RUSTPY_JUPYTER_REMOTE_KEY");
    let ca = required("RUSTPY_JUPYTER_REMOTE_CA");
    let kernel =
        std::env::var("RUSTPY_JUPYTER_TEST_KERNEL").expect("explicit registered kernelspec");
    assert!(!kernel.is_empty() && kernel != "project");
    let sdk = python.parent().unwrap().parent().unwrap();
    assert!(
        sdk.join("pyvenv.cfg").is_file(),
        "admitted isolated SDK required"
    );
    let root = tempfile::tempdir().unwrap().keep().canonicalize().unwrap();
    eprintln!(
        "owned password-only Jupyter TLS evidence: {}",
        root.display()
    );
    let mut environment = Vec::new();
    for (name, folder) in [
        ("JUPYTER_CONFIG_DIR", "config"),
        ("JUPYTER_DATA_DIR", "data"),
        ("JUPYTER_RUNTIME_DIR", "runtime"),
        ("IPYTHONDIR", "ipython"),
        ("MPLCONFIGDIR", "matplotlib"),
    ] {
        let path = root.join(folder);
        std::fs::create_dir(&path).unwrap();
        environment.push((name.to_string(), path.to_string_lossy().into_owned()));
    }
    environment.extend([
        (
            "JUPYTER_PATH".into(),
            sdk.join("share/jupyter").to_string_lossy().into_owned(),
        ),
        ("PYTHONNOUSERSITE".into(), "1".into()),
        ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
        ("MPLBACKEND".into(), "Agg".into()),
        ("NO_PROXY".into(), "127.0.0.1,localhost".into()),
        ("no_proxy".into(), "127.0.0.1,localhost".into()),
    ]);
    let password = format!(
        "owned-password-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let wrong_password = format!("{password}-incorrect");
    // Installed Jupyter provides both the actual password hash and its login
    // implementation. Leave XSRF protection enabled: the public bridge must
    // retrieve _xsrf, submit the form, retain login cookies, and send XSRF headers
    // for session creation/deletion before opening the authenticated WSS stream.
    let server = SERVER.replace("c.IdentityProvider.token=options['token']", r#"from jupyter_server.auth import passwd
from jupyter_server.auth.identity import PasswordIdentityProvider
c.ServerApp.identity_provider_class=PasswordIdentityProvider
c.IdentityProvider.token=''
c.PasswordIdentityProvider.token=''
c.PasswordIdentityProvider.hashed_password=passwd(options['password'])
c.PasswordIdentityProvider.password_required=True
c.PasswordIdentityProvider.allow_password_change=False"#)
        .replace("def ready():", r#"assert isinstance(app.identity_provider, PasswordIdentityProvider)
assert app.identity_provider.token == '' and app.identity_provider.auth_enabled
assert app.identity_provider.hashed_password != options['password']
assert app.identity_provider.passwd_check(options['password'])
revision=0
async def audit():
    global revision
    sessions=await app.session_manager.list_sessions()
    kernels=app.kernel_manager.list_kernel_ids()
    revision += 1
    target=pathlib.Path(options['audit'])
    temporary=target.with_suffix('.tmp')
    temporary.write_text(json.dumps({'revision':revision,'sessions':len(sessions),'kernels':len(kernels)}))
    temporary.replace(target)
def ready():"#)
        .replace("app.io_loop.add_callback(ready)", "from tornado.ioloop import PeriodicCallback\nmonitor=PeriodicCallback(audit,50)\nmonitor.start()\napp.io_loop.add_callback(ready)");
    assert!(!server.contains("options['token']"));
    let ready = root.join("ready.json");
    let audit = root.join("live-audit.json");
    let mut command = Command::new(&python);
    command
        .args(["-I", "-u", "-c", &server])
        .current_dir(&root)
        .envs(environment.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(std::fs::File::create(root.join("server.stdout.log")).unwrap())
        .stderr(std::fs::File::create(root.join("server.stderr.log")).unwrap())
        .process_group(0);
    let child = command
        .spawn()
        .expect("actual password-only Jupyter provider must start");
    let owner = identity(child.id()).expect("actual Linux provider identity");
    let mut provider = Provider {
        child,
        owner,
        stopped: false,
    };
    serde_json::to_writer(
        provider.child.stdin.as_mut().unwrap(),
        &serde_json::json!({
            "root":root, "cert":cert, "key":key, "password":password, "ready":ready, "audit":audit
        }),
    )
    .unwrap();
    provider.child.stdin.take().unwrap().flush().unwrap();
    let deadline = Instant::now() + LIMIT;
    while !ready.is_file() {
        assert!(
            provider.child.try_wait().unwrap().is_none(),
            "password provider exited; inspect private logs"
        );
        assert!(
            Instant::now() < deadline,
            "password-only HTTPS provider readiness deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&ready).unwrap()).unwrap();
    assert_eq!(receipt["pid"].as_u64(), Some(u64::from(provider.owner.pid)));
    let port: u16 = receipt["port"].as_u64().unwrap().try_into().unwrap();
    assert_ne!(port, 0);
    let endpoint = format!("https://127.0.0.1:{port}/");
    let mut revision = password_audit(&audit, 0, 0, 0);
    for (case, selected_password, success) in [
        ("wrong-password", &wrong_password, false),
        ("accepted-password", &password, true),
    ] {
        let marker = root.join(format!("{case}-cell.json"));
        let source = format!(
            "import json,os,pathlib,sys\nr={{'pid':os.getpid(),'python':sys.executable,'value':6*7}}\npathlib.Path({}).write_text(json.dumps(r))\nprint('ACTUAL_PASSWORD_TLS_CELL',json.dumps(r))",
            serde_json::to_string(&marker.to_string_lossy()).unwrap()
        );
        let runner = CellRunner::start_with_connection(
            &python,
            &root,
            &environment,
            "",
            Connection {
                kind: "remote".into(),
                kernel: kernel.clone(),
                url: endpoint.clone(),
                token: String::new(),
                password: selected_password.clone(),
                verify_ssl: true,
                ca_file: ca.to_string_lossy().into_owned(),
                ..Default::default()
            },
        )
        .expect("public password-only remote bridge startup");
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
            if success
                && client.text().contains("ACTUAL_PASSWORD_TLS_CELL")
                && !client.runner.is_busy()
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
                "password case {case} did not reach actual terminal oracle"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let messages = client.messages.join("\n");
        for credential in [&password, &wrong_password] {
            assert!(
                !messages.contains(credential) && !client.text().contains(credential),
                "passwords must never enter client output"
            );
        }
        if success {
            assert!(!client.notebook.cells[0]
                .outputs
                .iter()
                .any(|output| matches!(output, Output::Error { .. })));
            let result: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
            assert_eq!(result["value"], 42);
            assert_eq!(
                Path::new(result["python"].as_str().unwrap()),
                python.as_path()
            );
            let owner = identity(result["pid"].as_u64().unwrap().try_into().unwrap())
                .expect("actual password kernel alive");
            revision = password_audit(&audit, revision, 1, 1);
            client.close();
            let deadline = Instant::now() + LIMIT;
            while present(&owner) {
                assert!(
                    Instant::now() < deadline,
                    "password kernel survived authenticated session deletion"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        } else {
            assert!(
                !marker.exists() && !client.text().contains("ACTUAL_PASSWORD_TLS_CELL"),
                "wrong password cannot execute a queued cell"
            );
            assert!(
                messages.contains("401") || messages.contains("403"),
                "real password login must be refused by the provider"
            );
            client.close();
        }
        // A fresh post-close audit proves refusal never created a session or
        // kernel, and successful authenticated DELETE removed both resources.
        revision = password_audit(&audit, revision, 0, 0);
        assert!(
            present(&provider.owner),
            "client close must preserve separately owned server"
        );
    }
    provider.stop();
}
