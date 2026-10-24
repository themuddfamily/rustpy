//! Root-admitted real providers only. Run serially with the repository's CPU,
//! priority and thread caps, a private TMPDIR, and an absolute selected SDK.
//! This covers the public manager/registry backend, not App selection widgets.

#![cfg(target_os = "linux")]

use rustpy_core::CancellationToken;
use rustpy_project::environment_manager::{Event, Manager, Registry, Request, Task};
use rustpy_project::interpreter::Interpreter;
use rustpy_project::native_skeleton::supervised_tool;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn python(python: &Path, root: &Path, script: &str, args: &[&Path]) -> Vec<u8> {
    let mut command = Command::new(python);
    command
        .args(["-I", "-c", script])
        .args(args)
        .current_dir(root);
    supervised_tool(
        &mut command,
        &CancellationToken::new(),
        Duration::from_secs(60),
        1024 * 1024,
    )
    .expect("actual selected Python operation failed")
    .0
}

struct OwnedTask {
    task: Task,
    terminal: bool,
}

impl OwnedTask {
    fn start(request: Request) -> Self {
        Self {
            task: Task::start(request, true).unwrap(),
            terminal: false,
        }
    }
}

impl Drop for OwnedTask {
    fn drop(&mut self) {
        if self.terminal {
            return;
        }
        self.task.cancel();
        // Cancellation is a request, not physical retirement. Keep the private
        // project alive until the worker has returned from supervised tooling
        // and probing, including when an assertion unwinds the fixture.
        loop {
            if matches!(self.task.poll(), Some(Event::Finished(_))) {
                self.terminal = true;
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn complete(task: &mut OwnedTask) -> (Interpreter, Vec<String>) {
    let deadline = Instant::now() + Duration::from_secs(180);
    let mut progress = Vec::new();
    loop {
        if let Some(event) = task.task.poll() {
            match event {
                Event::Progress(message) => progress.push(message),
                Event::Finished(result) => {
                    task.terminal = true;
                    return (result.expect("actual environment manager failed"), progress);
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "environment manager timed out: {progress:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "requires RUSTPY_ENVIRONMENT_MANAGERS_ADMITTED=1 and absolute RUSTPY_ENVIRONMENT_PYTHON; actual offline venv/pip provider"]
fn configured_virtualenv_task_installs_probes_reuses_and_persists_registry() {
    assert_eq!(
        std::env::var("RUSTPY_ENVIRONMENT_MANAGERS_ADMITTED").as_deref(),
        Ok("1"),
        "explicit real-provider admission is required"
    );
    let base = PathBuf::from(
        std::env::var_os("RUSTPY_ENVIRONMENT_PYTHON").expect("select the absolute real Python SDK"),
    );
    assert!(
        base.is_absolute() && base.is_file(),
        "missing admitted SDK: {base:?}"
    );
    let temporary_root =
        PathBuf::from(std::env::var_os("TMPDIR").expect("private TMPDIR required"));
    assert!(temporary_root.is_absolute() && temporary_root.is_dir());
    let private = tempfile::Builder::new()
        .prefix("configured-environment-")
        .tempdir_in(temporary_root)
        .unwrap();
    let root = private.path().join("project π");
    let other = private.path().join("second project");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&other).unwrap();
    let wheel = root.join("rustpy_acceptance_payload-1.0-py3-none-any.whl");
    // Build only an owned, dependency-free wheel with stdlib zipfile. The real
    // manager still creates the environment, invokes pip and probes the SDK.
    python(
        &base,
        &root,
        r#"import sys, zipfile
files = {
 'rustpy_acceptance_payload.py': 'VALUE = 42\nTEXT = "π"\n',
 'rustpy_acceptance_payload-1.0.dist-info/METADATA': 'Metadata-Version: 2.1\nName: rustpy-acceptance-payload\nVersion: 1.0\n',
 'rustpy_acceptance_payload-1.0.dist-info/WHEEL': 'Wheel-Version: 1.0\nGenerator: RustPy acceptance\nRoot-Is-Purelib: true\nTag: py3-none-any\n',
}
files['rustpy_acceptance_payload-1.0.dist-info/RECORD'] = ''.join(name + ',,\n' for name in files) + 'rustpy_acceptance_payload-1.0.dist-info/RECORD,,\n'
with zipfile.ZipFile(sys.argv[1], 'w') as archive:
 for name, content in files.items(): archive.writestr(name, content)
"#,
        &[&wheel],
    );
    std::fs::write(
        root.join("requirements.txt"),
        "--no-index\n./rustpy_acceptance_payload-1.0-py3-none-any.whl\n",
    )
    .unwrap();
    let location = root.join("SDK π");
    let request = Request {
        root: root.clone(),
        manager: Manager::Virtualenv,
        existing: false,
        executable: base.clone(),
        base: base.clone(),
        location: location.clone(),
        python_version: String::new(),
        inherit_site_packages: false,
        install: true,
    };
    assert!(Task::start(request.clone(), false).is_err());
    assert!(
        !location.exists(),
        "untrusted admission must not create an SDK"
    );
    let mut task = OwnedTask::start(request.clone());
    let (created, progress) = complete(&mut task);
    assert_eq!(
        progress.len(),
        2,
        "create and actual pip install must both run"
    );
    assert!(created.is_venv);
    assert_eq!(created.prefix, location);
    assert_eq!(created.path, location.join("bin/python"));
    let output = python(
        &created.path,
        &root,
        "import json, os, sys, rustpy_acceptance_payload as p; print(json.dumps([p.VALUE, p.TEXT, sys.executable, sys.prefix, os.getcwd()]))",
        &[],
    );
    let observed: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        observed,
        serde_json::json!([42, "π", created.path, location, root])
    );
    assert!(
        Task::start(request.clone(), true).is_err(),
        "new must not overwrite an existing SDK"
    );
    let mut existing = OwnedTask::start(Request {
        existing: true,
        root: other.clone(),
        ..request
    });
    let (reused, progress) = complete(&mut existing);
    assert!(
        progress.is_empty(),
        "existing SDK reuse must not run creation/install"
    );
    assert_eq!(reused.path, created.path);
    assert_eq!(reused.version, created.version);

    let extra = other.join("extra π");
    std::fs::create_dir(&extra).unwrap();
    let removed = reused
        .sys_path
        .first()
        .expect("probe must publish search paths")
        .clone();
    let mut registry = Registry::default();
    registry.remember(created, &root, "First SDK", false);
    registry.entries[0].paths.push(extra.clone());
    registry.entries[0].removed_paths.push(removed.clone());
    registry.remember(reused.clone(), &other, " Renamed SDK π ", true);
    let path = private.path().join("private-registry.json");
    registry.save(&path).unwrap();
    let registry = Registry::load(&path);
    assert_eq!(registry.entries.len(), 1);
    assert_eq!(registry.entries[0].name, "Renamed SDK π");
    assert!(registry.entries[0].shared);
    assert_eq!(registry.entries[0].projects, vec![root, other]);
    let mut selected = reused;
    registry.apply_paths(&mut selected);
    assert!(selected.sys_path.contains(&extra));
    assert!(!selected.sys_path.contains(&removed));
    registry.apply_paths(&mut selected);
    assert_eq!(selected.sys_path.iter().filter(|p| *p == &extra).count(), 1);
}

#[test]
#[ignore = "requires explicit real Poetry CLI and selected Python; creates private Poetry config/cache and installs an owned wheel"]
fn configured_poetry_task_locks_installs_probes_and_reuses_private_sdk() {
    const SELECTOR: &str = "configured_poetry_task_locks_installs_probes_and_reuses_private_sdk";
    assert_eq!(
        std::env::var("RUSTPY_ENVIRONMENT_MANAGERS_ADMITTED").as_deref(),
        Ok("1")
    );
    let base = PathBuf::from(
        std::env::var_os("RUSTPY_ENVIRONMENT_PYTHON").expect("absolute selected SDK required"),
    );
    let poetry = PathBuf::from(
        std::env::var_os("RUSTPY_POETRY_CLI").expect("absolute real Poetry CLI required"),
    );
    for provider in [&base, &poetry] {
        assert!(
            provider.is_absolute() && provider.is_file(),
            "missing admitted provider: {provider:?}"
        );
    }
    if let Some(private) = std::env::var_os("RUSTPY_POETRY_FIXTURE_CHILD") {
        let private = PathBuf::from(private);
        assert!(private.is_absolute() && private.is_dir());
        for (key, leaf) in [
            ("POETRY_CONFIG_DIR", "config"),
            ("POETRY_CACHE_DIR", "cache"),
            ("VIRTUALENV_OVERRIDE_APP_DATA", "virtualenv"),
        ] {
            assert_eq!(
                std::env::var_os(key).map(PathBuf::from),
                Some(private.join(leaf))
            );
        }
        assert_eq!(
            std::env::var("POETRY_VIRTUALENVS_IN_PROJECT").as_deref(),
            Ok("true")
        );
        assert_eq!(
            std::env::var("POETRY_VIRTUALENVS_OPTIONS_NO_PIP").as_deref(),
            Ok("true")
        );
        let root = private.join("Poetry project π");
        std::fs::create_dir(&root).unwrap();
        let wheel = root.join("rustpy_poetry_payload-1.0-py3-none-any.whl");
        python(
            &base,
            &root,
            r#"import sys, zipfile
files = {
 'rustpy_poetry_payload.py': 'VALUE = 42\nTEXT = "π"\n',
 'rustpy_poetry_payload-1.0.dist-info/METADATA': 'Metadata-Version: 2.1\nName: rustpy-poetry-payload\nVersion: 1.0\n',
 'rustpy_poetry_payload-1.0.dist-info/WHEEL': 'Wheel-Version: 1.0\nGenerator: RustPy acceptance\nRoot-Is-Purelib: true\nTag: py3-none-any\n',
}
files['rustpy_poetry_payload-1.0.dist-info/RECORD'] = ''.join(name + ',,\n' for name in files) + 'rustpy_poetry_payload-1.0.dist-info/RECORD,,\n'
with zipfile.ZipFile(sys.argv[1], 'w') as archive:
 for name, content in files.items(): archive.writestr(name, content)
"#,
            &[&wheel],
        );
        std::fs::write(
            root.join("pyproject.toml"),
            r#"[tool.poetry]
name = "rustpy-poetry-project"
version = "1.0"
description = "Private real-provider acceptance"
authors = ["RustPy acceptance"]
package-mode = false
[tool.poetry.dependencies]
python = ">=3.10,<4.0"
rustpy-poetry-payload = { path = "rustpy_poetry_payload-1.0-py3-none-any.whl" }
"#,
        )
        .unwrap();
        let location = root.join(".venv");
        let request = Request {
            root: root.clone(),
            manager: Manager::Poetry,
            existing: false,
            executable: poetry.clone(),
            base,
            location: location.clone(),
            python_version: String::new(),
            inherit_site_packages: false,
            install: true,
        };
        assert!(Task::start(request.clone(), false).is_err());
        assert!(!location.exists());
        // Produce the genuine dependency lock without creating an environment:
        // the public Task, not preparation, must create the selected SDK.
        let mut command = Command::new(&poetry);
        command
            .args(["lock", "--no-interaction"])
            .env("POETRY_VIRTUALENVS_CREATE", "false")
            .current_dir(&root);
        supervised_tool(
            &mut command,
            &CancellationToken::new(),
            Duration::from_secs(120),
            1024 * 1024,
        )
        .expect("real offline Poetry lock preparation failed");
        assert!(
            !location.exists(),
            "lock preparation must not create the SDK"
        );
        let lock_before =
            std::fs::read(root.join("poetry.lock")).expect("real Poetry must produce a lockfile");
        let mut task = OwnedTask::start(request.clone());
        let (created, progress) = complete(&mut task);
        assert_eq!(
            progress.len(),
            2,
            "public Task must execute env use and install"
        );
        assert!(created.is_venv);
        assert_eq!(created.path, location.join("bin/python"));
        assert_eq!(created.prefix, location);
        assert_eq!(
            std::fs::read(root.join("poetry.lock")).unwrap(),
            lock_before
        );
        let observed: serde_json::Value = serde_json::from_slice(&python(
            &created.path, &root,
            "import json, os, sys, rustpy_poetry_payload as p; print(json.dumps([p.VALUE, p.TEXT, sys.executable, sys.prefix, os.getcwd()]))",
            &[],
        )).unwrap();
        assert_eq!(
            observed,
            serde_json::json!([42, "π", created.path, location, root])
        );
        let other = private.join("second project π");
        std::fs::create_dir(&other).unwrap();
        let mut reuse = OwnedTask::start(Request {
            existing: true,
            root: other.clone(),
            ..request
        });
        let (reused, progress) = complete(&mut reuse);
        assert!(progress.is_empty());
        assert_eq!(reused.path, created.path);
        let mut registry = Registry::default();
        registry.remember(created, &root, "Poetry SDK", false);
        registry.remember(reused, &other, "Shared Poetry π", true);
        let registry_path = private.join("registry.json");
        registry.save(&registry_path).unwrap();
        let loaded = Registry::load(&registry_path);
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].name, "Shared Poetry π");
        assert!(loaded.entries[0].shared);
        assert_eq!(loaded.entries[0].projects, vec![root, other]);
        return;
    }
    // Isolate manager variables in a genuine test-binary child: never mutate
    // process-global environment while Rust's test harness is multithreaded.
    let temporary_root =
        PathBuf::from(std::env::var_os("TMPDIR").expect("private TMPDIR required"));
    assert!(temporary_root.is_absolute() && temporary_root.is_dir());
    let private = tempfile::Builder::new()
        .prefix("configured-poetry-")
        .tempdir_in(temporary_root)
        .unwrap();
    for leaf in ["config", "cache", "virtualenv", "tmp"] {
        std::fs::create_dir(private.path().join(leaf)).unwrap();
    }
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args([
            SELECTOR,
            "--ignored",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("RUSTPY_POETRY_FIXTURE_CHILD", private.path())
        .env("POETRY_CONFIG_DIR", private.path().join("config"))
        .env("POETRY_CACHE_DIR", private.path().join("cache"))
        .env(
            "VIRTUALENV_OVERRIDE_APP_DATA",
            private.path().join("virtualenv"),
        )
        .env("TMPDIR", private.path().join("tmp"))
        .env("POETRY_VIRTUALENVS_IN_PROJECT", "true")
        .env("POETRY_VIRTUALENVS_CREATE", "true")
        .env("POETRY_VIRTUALENVS_OPTIONS_NO_PIP", "true")
        .env("POETRY_VIRTUALENVS_OPTIONS_SYSTEM_SITE_PACKAGES", "false")
        .env("POETRY_KEYRING_ENABLED", "false")
        .env("POETRY_NO_INTERACTION", "1")
        .env("POETRY_INSTALLER_PARALLEL", "false")
        .env("POETRY_INSTALLER_MAX_WORKERS", "2")
        .env_remove("VIRTUAL_ENV")
        .env_remove("CONDA_PREFIX")
        .env_remove("CONDA_DEFAULT_ENV");
    // Nested supervised manager processes own separate process groups. Never
    // hard-kill this controller at an outer deadline while its OwnedTask is
    // draining those owners. Retain the controller and its directory until it
    // exits; the centrally admitted outer runner owns emergency interruption.
    struct OwnedController {
        child: std::process::Child,
        reaped: bool,
    }
    impl Drop for OwnedController {
        fn drop(&mut self) {
            while !self.reaped {
                match self.child.wait() {
                    Ok(_) => self.reaped = true,
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        }
    }
    let mut controller = OwnedController {
        child: child
            .spawn()
            .expect("start isolated actual Poetry acceptance"),
        reaped: false,
    };
    let status = controller
        .child
        .wait()
        .expect("reap actual Poetry controller");
    controller.reaped = true;
    assert!(
        status.success(),
        "isolated actual Poetry acceptance failed: {status}"
    );
}

#[test]
#[ignore = "requires admitted actual uv, Python >=3.12, and genuine offline pip seed wheel"]
fn configured_uv_task_seeds_and_syncs_locked_offline_sdk() {
    const SELECTOR: &str = "configured_uv_task_seeds_and_syncs_locked_offline_sdk";
    assert_eq!(
        std::env::var("RUSTPY_ENVIRONMENT_MANAGERS_ADMITTED").as_deref(),
        Ok("1")
    );
    let base = PathBuf::from(
        std::env::var_os("RUSTPY_ENVIRONMENT_PYTHON").expect("absolute selected SDK required"),
    );
    let uv =
        PathBuf::from(std::env::var_os("RUSTPY_UV_CLI").expect("absolute actual uv CLI required"));
    let seed = PathBuf::from(
        std::env::var_os("RUSTPY_UV_SEED_WHEEL")
            .expect("absolute genuine offline pip wheel required"),
    );
    for provider in [&base, &uv, &seed] {
        assert!(
            provider.is_absolute() && provider.is_file(),
            "missing admitted provider: {provider:?}"
        );
    }
    if let Some(private) = std::env::var_os("RUSTPY_UV_FIXTURE_CHILD") {
        let private = PathBuf::from(private);
        assert!(private.is_absolute() && private.is_dir());
        for (key, leaf) in [("UV_CACHE_DIR", "cache"), ("UV_FIND_LINKS", "seed-wheels")] {
            assert_eq!(
                std::env::var_os(key).map(PathBuf::from),
                Some(private.join(leaf))
            );
        }
        for key in ["UV_NO_INDEX", "UV_OFFLINE", "UV_NO_CONFIG", "UV_NO_BUILD"] {
            assert_eq!(std::env::var(key).as_deref(), Ok("true"));
        }
        assert_eq!(std::env::var("UV_PYTHON_DOWNLOADS").as_deref(), Ok("never"));
        let selected = rustpy_project::interpreter::probe(&base, "admitted uv base")
            .expect("selected SDK must work");
        assert!(
            selected.major_minor() >= (3, 12),
            "fixture admits pip-only uv seeding on Python >=3.12"
        );
        let root = private.join("uv project π");
        std::fs::create_dir(&root).unwrap();
        let wheel = root.join("rustpy_uv_payload-1.0-py3-none-any.whl");
        python(
            &base,
            &root,
            r#"import sys, zipfile
files = {
 'rustpy_uv_payload.py': 'VALUE = 42\nTEXT = "π"\n',
 'rustpy_uv_payload-1.0.dist-info/METADATA': 'Metadata-Version: 2.1\nName: rustpy-uv-payload\nVersion: 1.0\n',
 'rustpy_uv_payload-1.0.dist-info/WHEEL': 'Wheel-Version: 1.0\nGenerator: RustPy acceptance\nRoot-Is-Purelib: true\nTag: py3-none-any\n',
}
files['rustpy_uv_payload-1.0.dist-info/RECORD'] = ''.join(name + ',,\n' for name in files) + 'rustpy_uv_payload-1.0.dist-info/RECORD,,\n'
with zipfile.ZipFile(sys.argv[1], 'w') as archive:
 for name, content in files.items(): archive.writestr(name, content)
"#,
            &[&wheel],
        );
        let wheel_url = url::Url::from_file_path(&wheel).expect("owned absolute wheel URL");
        std::fs::write(
            root.join("pyproject.toml"),
            format!(
                r#"[project]
name = "rustpy-uv-project"
version = "1.0"
requires-python = ">=3.12"
dependencies = ["rustpy-uv-payload @ {wheel_url}"]
"#
            ),
        )
        .unwrap();
        let location = root.join("SDK λ");
        let request = Request {
            root: root.clone(),
            manager: Manager::Uv,
            existing: false,
            executable: uv.clone(),
            base: base.clone(),
            location: location.clone(),
            python_version: String::new(),
            inherit_site_packages: false,
            install: true,
        };
        assert!(Task::start(request.clone(), false).is_err());
        assert!(!location.exists());
        let mut lock = Command::new(&uv);
        lock.args(["lock", "--offline", "--python"])
            .arg(&base)
            .current_dir(&root);
        supervised_tool(
            &mut lock,
            &CancellationToken::new(),
            Duration::from_secs(120),
            1024 * 1024,
        )
        .expect("actual uv must produce the offline lock from the owned wheel");
        assert!(
            !location.exists() && !root.join(".venv").exists(),
            "lock preparation must not create an SDK"
        );
        let lock_before = std::fs::read(root.join("uv.lock")).expect("real uv lock required");
        let mut task = OwnedTask::start(request.clone());
        let (created, progress) = complete(&mut task);
        assert_eq!(
            progress.len(),
            2,
            "public Task must run uv venv seeding and locked sync"
        );
        assert!(created.is_venv);
        assert_eq!(created.path, location.join("bin/python"));
        assert_eq!(created.prefix, location);
        assert!(
            !root.join(".venv").exists(),
            "sync must use the captured Unicode SDK location"
        );
        assert_eq!(std::fs::read(root.join("uv.lock")).unwrap(), lock_before);
        let observed: serde_json::Value = serde_json::from_slice(&python(
            &created.path, &root,
            "import json, os, sys, rustpy_uv_payload as p; print(json.dumps([p.VALUE, p.TEXT, sys.executable, sys.prefix, os.getcwd()]))",
            &[],
        )).unwrap();
        assert_eq!(
            observed,
            serde_json::json!([42, "π", created.path, location, root])
        );
        assert!(
            Task::start(request.clone(), true).is_err(),
            "new uv SDK must not overwrite an existing one"
        );
        let other = private.join("other project λ");
        std::fs::create_dir(&other).unwrap();
        let mut reuse = OwnedTask::start(Request {
            existing: true,
            root: other.clone(),
            ..request
        });
        let (reused, progress) = complete(&mut reuse);
        assert!(progress.is_empty());
        assert_eq!(reused.path, created.path);
        assert_eq!(reused.version, selected.version);
        let mut registry = Registry::default();
        registry.remember(created, &root, "uv SDK", false);
        registry.remember(reused, &other, "Shared uv π", true);
        let path = private.join("registry.json");
        registry.save(&path).unwrap();
        let loaded = Registry::load(&path);
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].name, "Shared uv π");
        assert!(loaded.entries[0].shared);
        assert_eq!(loaded.entries[0].projects, vec![root, other]);
        return;
    }
    let temporary_root =
        PathBuf::from(std::env::var_os("TMPDIR").expect("private TMPDIR required"));
    assert!(temporary_root.is_absolute() && temporary_root.is_dir());
    let private = tempfile::Builder::new()
        .prefix("configured-uv-")
        .tempdir_in(temporary_root)
        .unwrap();
    for leaf in ["cache", "seed-wheels", "tmp"] {
        std::fs::create_dir(private.path().join(leaf)).unwrap();
    }
    let filename = seed
        .file_name()
        .unwrap()
        .to_str()
        .expect("seed wheel name must be UTF8");
    assert!(
        filename.starts_with("pip-") && filename.ends_with(".whl"),
        "admit a genuine pip seed wheel"
    );
    std::fs::copy(&seed, private.path().join("seed-wheels").join(filename)).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args([
            SELECTOR,
            "--ignored",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("RUSTPY_UV_FIXTURE_CHILD", private.path())
        .env("UV_CACHE_DIR", private.path().join("cache"))
        .env("UV_FIND_LINKS", private.path().join("seed-wheels"))
        .env("TMPDIR", private.path().join("tmp"))
        .env("UV_NO_INDEX", "true")
        .env("UV_OFFLINE", "true")
        .env("UV_NO_CONFIG", "true")
        .env("UV_NO_BUILD", "true")
        .env("UV_PYTHON_DOWNLOADS", "never")
        .env("UV_CONCURRENT_DOWNLOADS", "2")
        .env("UV_CONCURRENT_BUILDS", "2")
        .env("UV_CONCURRENT_INSTALLS", "2")
        .env_remove("UV_PROJECT_ENVIRONMENT")
        .env_remove("VIRTUAL_ENV")
        .env_remove("CONDA_PREFIX")
        .env_remove("CONDA_DEFAULT_ENV");
    // As in the Poetry case, nested managers have independent process groups.
    // Retain this controller through child unwind and OwnedTask's terminal drain.
    struct OwnedController {
        child: std::process::Child,
        reaped: bool,
    }
    impl Drop for OwnedController {
        fn drop(&mut self) {
            while !self.reaped {
                match self.child.wait() {
                    Ok(_) => self.reaped = true,
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        }
    }
    let mut controller = OwnedController {
        child: child.spawn().expect("start real uv controller"),
        reaped: false,
    };
    let status = controller.child.wait().expect("reap real uv controller");
    controller.reaped = true;
    assert!(status.success(), "actual uv controller failed: {status}");
}
