//! Opt-in real pip acceptance against an owned authenticated loopback PEP503 index.
//! Requires RUSTPY_REQUIRE_PROVIDER_TESTS=1, an absolute disposable-venv
//! RUSTPY_PROVIDER_PYTHON, and RUSTPY_PRIVATE_INDEX_PASSWORD=rustpy-fixture-… .
use rustpy_packages::{
    project::{command_plan, CommandPlan, Manager, Operation, Preferences, Repository},
    PipTask,
};
use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const NAME: &str = "rustpy-private-repository-acceptance";
const SECRET_ENV: &str = "RUSTPY_PRIVATE_INDEX_PASSWORD";
const SERVER: &str = r#"import base64, csv, hashlib, http.server, io, json, os, pathlib, sys, zipfile
root = pathlib.Path(sys.argv[1])
name = 'rustpy_private_repository_acceptance'
wheel_name = name + '-1.0-py3-none-any.whl'
info = name + '-1.0.dist-info'
files = {
    name + '/__init__.py': b'VALUE = 73\n',
    info + '/METADATA': b'Metadata-Version: 2.2\nName: rustpy-private-repository-acceptance\nVersion: 1.0\n\n',
    info + '/WHEEL': b'Wheel-Version: 1.0\nGenerator: RustPy private index acceptance\nRoot-Is-Purelib: true\nTag: py3-none-any\n',
}
record = io.StringIO(newline='')
writer = csv.writer(record)
for path, content in files.items():
    digest = base64.urlsafe_b64encode(hashlib.sha256(content).digest()).rstrip(b'=').decode()
    writer.writerow([path, 'sha256=' + digest, len(content)])
writer.writerow([info + '/RECORD', '', ''])
files[info + '/RECORD'] = record.getvalue().encode()
archive = io.BytesIO()
with zipfile.ZipFile(archive, 'w') as wheel:
    for path, content in files.items():
        wheel.writestr(path, content)
wheel = archive.getvalue()
expected = 'Basic ' + base64.b64encode(('rustpy-user:' + os.environ['RUSTPY_PRIVATE_INDEX_PASSWORD']).encode()).decode()
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass
    def do_GET(self):
        path = self.path.split('?', 1)[0]
        authenticated = self.headers.get('Authorization') == expected
        private = path.startswith('/simple/') or path.startswith('/files/')
        if private and not authenticated:
            status, body = 401, b'Authentication required'
        elif path == '/simple/rustpy-private-repository-acceptance/':
            digest = hashlib.sha256(wheel).hexdigest()
            body = ('<!doctype html><a href="/files/' + wheel_name + '#sha256=' + digest + '">' + wheel_name + '</a>').encode()
            status = 200
        elif path == '/files/' + wheel_name:
            status, body = 200, wheel
        elif path.startswith('/empty/simple/'):
            status, body = 200, b'<!doctype html><html><body></body></html>'
        else:
            status, body = 404, b'Not found'
        with (root / 'requests.jsonl').open('a') as audit:
            audit.write(json.dumps(dict(path=path, authenticated=authenticated, status=status)) + '\n')
        self.send_response(status)
        if status == 401:
            self.send_header('WWW-Authenticate', 'Basic realm="RustPy owned fixture"')
        self.send_header('Content-Type', 'application/octet-stream' if path.startswith('/files/') else 'text/html')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)
server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
server.daemon_threads = True
print('http://127.0.0.1:' + str(server.server_address[1]), flush=True)
server.serve_forever()
"#;

struct OwnedIndex {
    child: Child,
    url: String,
}
impl Drop for OwnedIndex {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl OwnedIndex {
    fn start(python: &Path, root: &Path) -> Self {
        let script = root.join("private_index.py");
        std::fs::write(&script, SERVER).unwrap();
        let mut child = Command::new(python)
            .args(["-I", "-u"])
            .arg(script)
            .arg(root)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .spawn()
            .expect("owned PEP503 server starts");
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let result = BufReader::new(stdout).read_line(&mut line).map(|_| line);
            let _ = tx.send(result);
        });
        // Establish RAII ownership before a failed readiness assertion can unwind.
        let mut server = Self {
            child,
            url: String::new(),
        };
        server.url = rx
            .recv_timeout(Duration::from_secs(15))
            .expect("private index readiness deadline")
            .expect("private index readiness output")
            .trim()
            .to_owned();
        assert!(server.url.starts_with("http://127.0.0.1:"));
        assert!(server
            .url
            .rsplit_once(':')
            .unwrap()
            .1
            .parse::<u16>()
            .is_ok_and(|port| port > 0));
        server
    }
}

fn poll_to_exit(mut task: PipTask) -> (i32, Vec<String>) {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut lines = Vec::new();
    while task.is_running() {
        lines.extend(task.poll());
        if Instant::now() >= deadline {
            task.stop();
            panic!("owned pip task exceeded deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    lines.extend(task.poll());
    assert!(!task.was_stopped());
    (task.exit_code().expect("physical pip child exit"), lines)
}

fn isolate(plan: &mut CommandPlan, index: &OwnedIndex, authenticated: bool) {
    plan.env.extend([
        (
            "PIP_INDEX_URL".into(),
            format!("{}/empty/simple/", index.url),
        ),
        ("PIP_NO_INDEX".into(), "0".into()),
        (
            "PIP_CONFIG_FILE".into(),
            if cfg!(windows) { "NUL" } else { "/dev/null" }.into(),
        ),
        ("PIP_DISABLE_PIP_VERSION_CHECK".into(), "1".into()),
        ("PIP_NO_CACHE_DIR".into(), "1".into()),
        ("PIP_KEYRING_PROVIDER".into(), "disabled".into()),
        ("PIP_REQUIRE_VIRTUALENV".into(), "1".into()),
        ("PIP_USER".into(), "0".into()),
        ("NO_PROXY".into(), "127.0.0.1,localhost".into()),
        ("no_proxy".into(), "127.0.0.1,localhost".into()),
    ]);
    if !authenticated {
        plan.env.push((
            "PIP_EXTRA_INDEX_URL".into(),
            format!("{}/simple/", index.url),
        ));
    }
}

fn inventory_has(python: &Path) -> bool {
    rustpy_packages::list_installed(python)
        .unwrap()
        .iter()
        .any(|package| rustpy_packages::normalize_name(&package.name) == NAME)
}

struct InstalledCleanup<'a> {
    python: &'a Path,
    root: &'a Path,
    armed: bool,
}
impl Drop for InstalledCleanup<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let preferences = Preferences {
            manager: Manager::Pip,
            ..Default::default()
        };
        if let Ok(mut plan) = command_plan(
            self.root,
            self.python,
            &preferences,
            &Operation::Uninstall(vec![NAME.into()]),
        ) {
            plan.env
                .push(("PIP_DISABLE_PIP_VERSION_CHECK".into(), "1".into()));
            if let Ok(mut task) = plan.start() {
                let deadline = Instant::now() + Duration::from_secs(15);
                while task.is_running() && Instant::now() < deadline {
                    task.poll();
                    std::thread::sleep(Duration::from_millis(10));
                }
                if task.is_running() {
                    task.stop();
                }
            }
        }
    }
}

#[test]
#[ignore = "requires admitted disposable SDK and explicit synthetic private-index credential; loopback HTTP only"]
fn configured_authenticated_private_pep503_install_inventory_and_uninstall() {
    assert_eq!(
        std::env::var("RUSTPY_REQUIRE_PROVIDER_TESTS").as_deref(),
        Ok("1"),
        "explicit provider admission required"
    );
    let python = PathBuf::from(
        std::env::var_os("RUSTPY_PROVIDER_PYTHON")
            .expect("absolute selected disposable SDK required"),
    );
    assert!(python.is_absolute() && python.is_file());
    let secret = std::env::var(SECRET_ENV).expect("set synthetic RUSTPY_PRIVATE_INDEX_PASSWORD");
    assert!(
        secret.starts_with("rustpy-fixture-") && secret.len() > 15,
        "use a synthetic fixture credential, never an account password"
    );
    for key in [
        "PYTHONPATH",
        "PYTHONHOME",
        "PIP_TARGET",
        "PIP_PREFIX",
        "PIP_ROOT",
    ] {
        assert!(
            std::env::var_os(key).is_none_or(|value| value.is_empty()),
            "remove inherited {key} before SDK mutation"
        );
    }
    let probe = Command::new(&python).args(["-I", "-c", "import sys, pip; assert sys.prefix != sys.base_prefix, 'disposable venv required'; print('PRIVATE_INDEX_SDK_OK')"])
        .output().unwrap();
    assert!(
        probe.status.success(),
        "selected SDK must be a disposable venv with actual pip"
    );
    assert_eq!(
        String::from_utf8_lossy(&probe.stdout).trim(),
        "PRIVATE_INDEX_SDK_OK"
    );
    assert!(
        !inventory_has(&python),
        "refuse replacing a preexisting distribution"
    );
    let root = tempfile::tempdir().unwrap();
    let index = OwnedIndex::start(&python, root.path());
    let preferences = Preferences {
        manager: Manager::Pip,
        repositories: vec![Repository {
            url: format!("{}/simple/", index.url),
            username: "rustpy-user".into(),
            password_environment: SECRET_ENV.into(),
            ..Default::default()
        }],
        options: vec!["--no-deps".into(), "--only-binary=:all:".into()],
        ..Default::default()
    };
    preferences.save(root.path()).unwrap();
    let persisted = std::fs::read_to_string(root.path().join(".rustpy/packages.json")).unwrap();
    assert!(persisted.contains(SECRET_ENV));
    assert!(
        !persisted.contains(&secret),
        "persist references rather than repository secrets"
    );
    let install = Operation::Install(vec![format!("{NAME}==1.0")]);
    let mut missing = preferences.clone();
    let missing_env = "RUSTPY_PRIVATE_INDEX_INTENTIONALLY_UNSET";
    assert!(
        std::env::var_os(missing_env).is_none(),
        "missing-auth control must actually be unset"
    );
    missing.repositories[0].password_environment = missing_env.into();
    let error = command_plan(root.path(), &python, &missing, &install)
        .expect_err("missing credential must refuse before spawning pip");
    assert!(error.to_string().contains(missing_env));
    assert!(!error.to_string().contains(&secret));
    assert!(
        !root.path().join("requests.jsonl").exists(),
        "planning refusal must not contact the repository"
    );

    let mut cleanup = InstalledCleanup {
        python: &python,
        root: root.path(),
        armed: true,
    };
    let mut anonymous = preferences.clone();
    anonymous.repositories[0].username.clear();
    anonymous.repositories[0].password_environment.clear();
    let mut plan = command_plan(root.path(), &python, &anonymous, &install).unwrap();
    isolate(&mut plan, &index, false);
    let (exit, lines) = poll_to_exit(plan.start().unwrap());
    assert_ne!(exit, 0, "anonymous private installation must fail");
    assert!(!inventory_has(&python));
    assert!(
        lines.iter().all(|line| !line.contains(&secret)),
        "anonymous failure must not leak credentials"
    );

    let mut plan = command_plan(root.path(), &python, &preferences, &install).unwrap();
    assert_eq!(plan.program, python);
    assert!(plan
        .args
        .iter()
        .all(|arg| !arg.contains(&secret) && !arg.contains("rustpy-user")));
    assert!(
        plan.env
            .iter()
            .any(|(key, value)| key == "PIP_EXTRA_INDEX_URL" && value.contains("rustpy-user:")),
        "actual repository preferences must supply pip authentication"
    );
    isolate(&mut plan, &index, true);
    let task = plan.start().unwrap();
    assert!(!task.command_line.contains(&secret));
    assert!(!task.command_line.contains("rustpy-user"));
    let (exit, lines) = poll_to_exit(task);
    assert!(
        lines.iter().all(|line| !line.contains(&secret)),
        "actual pip output must redact repository password"
    );
    assert_eq!(exit, 0, "authenticated real pip install failed: {lines:?}");
    let inventory = rustpy_packages::list_installed(&python).unwrap();
    let package = inventory
        .iter()
        .find(|package| rustpy_packages::normalize_name(&package.name) == NAME)
        .unwrap();
    assert_eq!(package.version, "1.0");
    let readback = Command::new(&python).args(["-I", "-c", "import rustpy_private_repository_acceptance as owned; import importlib.metadata as metadata; assert owned.VALUE == 73; assert metadata.version('rustpy-private-repository-acceptance') == '1.0'; print('PRIVATE_INDEX_IMPORT_OK')"])
        .current_dir(root.path()).output().unwrap();
    assert!(readback.status.success());
    assert_eq!(
        String::from_utf8_lossy(&readback.stdout).trim(),
        "PRIVATE_INDEX_IMPORT_OK"
    );
    let audit = std::fs::read_to_string(root.path().join("requests.jsonl")).unwrap();
    let requests: Vec<serde_json::Value> = audit
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(requests
        .iter()
        .any(|request| request["status"] == 401 && request["authenticated"] == false));
    assert!(requests.iter().any(|request| request["path"]
        == "/simple/rustpy-private-repository-acceptance/"
        && request["authenticated"] == true
        && request["status"] == 200));
    assert!(requests.iter().any(|request| request["path"]
        .as_str()
        .unwrap()
        .starts_with("/files/")
        && request["authenticated"] == true
        && request["status"] == 200));
    assert!(!audit.contains(&secret));
    let mut uninstall = command_plan(
        root.path(),
        &python,
        &Preferences {
            manager: Manager::Pip,
            ..Default::default()
        },
        &Operation::Uninstall(vec![NAME.into()]),
    )
    .unwrap();
    isolate(&mut uninstall, &index, false);
    let (exit, _) = poll_to_exit(uninstall.start().unwrap());
    assert_eq!(exit, 0);
    assert!(!inventory_has(&python));
    let absent = Command::new(&python).args(["-I", "-c", "import importlib.util; assert importlib.util.find_spec('rustpy_private_repository_acceptance') is None; print('PRIVATE_INDEX_UNINSTALL_OK')"])
        .output().unwrap();
    assert!(absent.status.success());
    assert_eq!(
        String::from_utf8_lossy(&absent.stdout).trim(),
        "PRIVATE_INDEX_UNINSTALL_OK"
    );
    cleanup.armed = false;
    drop(cleanup);
    eprintln!("REQUIRED_PRIVATE_REPOSITORY python={} loopback_index={} authenticated_wheel=true missing_auth_refused=true credentials_redacted=true install_inventory_import_uninstall=true", python.display(), index.url);
}
