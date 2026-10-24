//! Actual offline Conda acceptance. Root supplies a genuine CLI and an exclusive,
//! populated package cache (Python runtime closure plus six=1.17.0) below TMPDIR.
//! No missing-provider skips, downloads, shell peers, or HOME changes.
#![cfg(target_os = "linux")]

use rustpy_core::CancellationToken;
use rustpy_project::environment_manager::{Event, Manager, Registry, Request, Task};
use rustpy_project::interpreter::Interpreter;
use rustpy_project::native_skeleton::supervised_tool;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const SELECTOR: &str =
    "configured_conda_creates_probes_reuses_registers_and_consumes_environment_yaml";

fn selected(key: &str) -> PathBuf {
    let path = PathBuf::from(std::env::var_os(key).unwrap_or_else(|| panic!("required {key}")));
    assert!(
        path.is_absolute() && path.exists(),
        "invalid {key}: {path:?}"
    );
    path
}

fn output(command: &mut Command) -> Vec<u8> {
    supervised_tool(
        command,
        &CancellationToken::new(),
        Duration::from_secs(60),
        4 * 1024 * 1024,
    )
    .expect("actual selected Conda/Python command failed")
    .0
}

struct OwnedTask {
    task: Task,
    terminal: bool,
}
impl OwnedTask {
    fn start(request: Request) -> Self {
        Self {
            task: Task::start(request, true).expect("public Conda Task admission"),
            terminal: false,
        }
    }
    fn finish(&mut self) -> (Interpreter, Vec<String>) {
        let deadline = Instant::now() + Duration::from_secs(600);
        let mut progress = Vec::new();
        loop {
            match self.task.poll() {
                Some(Event::Progress(message)) => progress.push(message),
                Some(Event::Finished(result)) => {
                    self.terminal = true;
                    return (result.expect("actual offline Conda Task failed"), progress);
                }
                None => {}
            }
            assert!(
                Instant::now() < deadline,
                "Conda Task deadline: {progress:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for OwnedTask {
    fn drop(&mut self) {
        if self.terminal {
            return;
        }
        self.task.cancel();
        // Keep the private project alive until all supervised manager/probe
        // work has retired, including assertion unwinding.
        while !matches!(self.task.poll(), Some(Event::Finished(_))) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn assert_python(
    interpreter: &Interpreter,
    root: &Path,
    location: &Path,
    version: &str,
    six: bool,
) {
    assert_eq!(interpreter.path, location.join("bin/python"));
    assert_eq!(interpreter.prefix, location);
    assert_eq!(interpreter.version, version);
    assert!(location.join("conda-meta/history").is_file());
    let script = if six {
        "import json,sys,six; print(json.dumps([sys.executable,sys.prefix,list(sys.version_info[:3]),six.__version__,'π']))"
    } else {
        "import json,sys; print(json.dumps([sys.executable,sys.prefix,list(sys.version_info[:3]),'π']))"
    };
    let observed: serde_json::Value = serde_json::from_slice(&output(
        Command::new(&interpreter.path)
            .args(["-I", "-c", script])
            .current_dir(root),
    ))
    .unwrap();
    assert_eq!(observed[0], serde_json::json!(interpreter.path));
    assert_eq!(observed[1], serde_json::json!(location));
    let parts: Vec<u32> = version.split('.').map(|p| p.parse().unwrap()).collect();
    assert_eq!(observed[2], serde_json::json!(parts));
    if six {
        assert_eq!(observed[3], "1.17.0");
        assert_eq!(observed[4], "π");
    } else {
        assert_eq!(observed[3], "π");
    }
    // Conda is not a venv; sys.prefix and genuine conda-meta are the authority.
}

#[test]
#[ignore = "requires admitted genuine absolute Conda CLI, full Python version, and exclusive populated offline Conda cache"]
fn configured_conda_creates_probes_reuses_registers_and_consumes_environment_yaml() {
    assert_eq!(
        std::env::var("RUSTPY_ENVIRONMENT_MANAGERS_ADMITTED").as_deref(),
        Ok("1")
    );
    let conda = selected("RUSTPY_CONDA_CLI");
    assert!(conda.is_file());
    let cache = selected("RUSTPY_CONDA_PKGS_DIR");
    assert!(cache.is_dir());
    let version =
        std::env::var("RUSTPY_CONDA_PYTHON_VERSION").expect("explicit full Python version");
    let parts: Vec<_> = version.split('.').collect();
    assert_eq!(parts.len(), 3, "select an exact major.minor.patch version");
    assert!(parts
        .iter()
        .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())));
    let temporary = selected("TMPDIR");
    if let Some(private) = std::env::var_os("RUSTPY_CONDA_FIXTURE_CHILD") {
        let private = PathBuf::from(private);
        assert!(private.is_absolute() && private.is_dir());
        assert_eq!(
            std::env::var_os("CONDARC").map(PathBuf::from),
            Some(private.join("condarc"))
        );
        // Verify effective provider settings, rather than assuming inherited
        // user/root Conda configuration cannot override this offline fixture.
        let config: serde_json::Value =
            serde_json::from_slice(&output(Command::new(&conda).args([
                "config",
                "--show",
                "offline",
                "register_envs",
                "pkgs_dirs",
                "envs_dirs",
                "solver",
                "--json",
            ])))
            .unwrap();
        assert_eq!(config["offline"], true);
        assert_eq!(config["register_envs"], false);
        assert_eq!(config["pkgs_dirs"], serde_json::json!([cache]));
        assert_eq!(
            config["envs_dirs"][0],
            serde_json::json!(private.join("envs"))
        );
        assert_eq!(config["solver"], "classic");
        let root = private.join("Conda project π");
        let other = private.join("other project λ");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&other).unwrap();
        // Conda validates the prefix leaf as an environment name (no spaces);
        // retain spaces and Unicode in the project parent to exercise argv paths.
        let location = root.join("SDKπ");
        let request = Request {
            root: root.clone(),
            manager: Manager::Conda,
            existing: false,
            executable: conda,
            base: PathBuf::new(),
            location: location.clone(),
            python_version: version.clone(),
            inherit_site_packages: false,
            install: false,
        };
        assert!(Task::start(request.clone(), false).is_err());
        assert!(
            !location.exists(),
            "untrusted admission must not create an environment"
        );
        let mut task = OwnedTask::start(request.clone());
        let (created, progress) = task.finish();
        assert_eq!(progress.len(), 1);
        assert_python(&created, &root, &location, &version, false);
        assert!(
            Task::start(request.clone(), true).is_err(),
            "new must not overwrite an existing SDK"
        );
        let history = std::fs::read(location.join("conda-meta/history")).unwrap();
        let mut reuse = OwnedTask::start(Request {
            existing: true,
            root: other.clone(),
            ..request.clone()
        });
        let (reused, progress) = reuse.finish();
        assert!(
            progress.is_empty(),
            "existing reuse must not create/install"
        );
        assert_python(&reused, &other, &location, &version, false);
        assert_eq!(
            std::fs::read(location.join("conda-meta/history")).unwrap(),
            history
        );
        let mut registry = Registry::default();
        registry.remember(created, &root, "Conda SDK", false);
        registry.remember(reused, &other, "Shared Conda π", true);
        let registry_path = private.join("registry.json");
        registry.save(&registry_path).unwrap();
        let loaded = Registry::load(&registry_path);
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].name, "Shared Conda π");
        assert!(loaded.entries[0].shared);
        assert_eq!(loaded.entries[0].projects, vec![root.clone(), other]);
        assert_eq!(
            loaded.entries[0].interpreter.path,
            location.join("bin/python")
        );
        std::fs::write(root.join("environment.yml"), format!(
            "dependencies:\n  - python={version}\n  - six=1.17.0\nvariables:\n  RUSTPY_CONDA_YAML_MARKER: 'π'\n"
        )).unwrap();
        let yaml_location = root.join("YAMLSDKλ");
        let mut yaml = OwnedTask::start(Request {
            install: true,
            location: yaml_location.clone(),
            ..request
        });
        let (installed, progress) = yaml.finish();
        assert_eq!(
            progress.len(),
            1,
            "actual env create consumes environment.yml"
        );
        assert_python(&installed, &root, &yaml_location, &version, true);
        let state: serde_json::Value =
            serde_json::from_slice(&std::fs::read(yaml_location.join("conda-meta/state")).unwrap())
                .unwrap();
        assert_eq!(state["env_vars"]["RUSTPY_CONDA_YAML_MARKER"], "π");
        return;
    }
    assert!(
        dunce::canonicalize(&cache)
            .unwrap()
            .starts_with(dunce::canonicalize(&temporary).unwrap()),
        "Root must admit an exclusive private offline cache below TMPDIR"
    );
    let private = tempfile::Builder::new()
        .prefix("configured-conda-")
        .tempdir_in(&temporary)
        .unwrap();
    for leaf in ["envs", "xdg-cache", "xdg-config", "xdg-data", "tmp"] {
        std::fs::create_dir(private.path().join(leaf)).unwrap();
    }
    let config = private.path().join("condarc");
    std::fs::write(&config, "channels: [conda-forge]\noffline: true\nregister_envs: false\nsolver: classic\nnotify_outdated_conda: false\nreport_errors: false\ncreate_default_packages: []\n").unwrap();
    // Per-child settings only; preserve HOME and never mutate global test env.
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args([
            SELECTOR,
            "--ignored",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("RUSTPY_CONDA_FIXTURE_CHILD", private.path())
        .env("CONDARC", config)
        .env("CONDA_PKGS_DIRS", &cache)
        .env("CONDA_ENVS_PATH", private.path().join("envs"))
        .env("CONDA_OFFLINE", "true")
        .env("CONDA_REGISTER_ENVS", "false")
        .env("CONDA_DEFAULT_THREADS", "2")
        .env("CONDA_SOLVER", "classic")
        .env("CONDA_NO_PLUGINS", "true")
        .env("CONDA_CREATE_DEFAULT_PACKAGES", "")
        .env("CONDA_AUTO_UPDATE_CONDA", "false")
        .env("XDG_CACHE_HOME", private.path().join("xdg-cache"))
        .env("XDG_CONFIG_HOME", private.path().join("xdg-config"))
        .env("XDG_DATA_HOME", private.path().join("xdg-data"))
        .env("TMPDIR", private.path().join("tmp"))
        .env_remove("VIRTUAL_ENV")
        .env_remove("CONDA_PREFIX")
        .env_remove("CONDA_DEFAULT_ENV");
    // Nested Tasks own process groups. Retain the controller through its own
    // cancellation/unwind drain instead of killing it at an outer deadline.
    struct Controller(std::process::Child, bool);
    impl Drop for Controller {
        fn drop(&mut self) {
            while !self.1 {
                match self.0.wait() {
                    Ok(_) => self.1 = true,
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        }
    }
    let mut controller = Controller(
        child.spawn().expect("actual Conda fixture controller"),
        false,
    );
    let status = controller
        .0
        .wait()
        .expect("reap actual Conda fixture controller");
    controller.1 = true;
    assert!(status.success(), "actual Conda controller failed: {status}");
}
