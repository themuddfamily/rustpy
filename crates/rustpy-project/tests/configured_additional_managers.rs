//! Genuine installed managers only; run this exact ignored case alone under Root caps.
//! Root supplies private cache/config paths; this fixture never changes HOME or
//! global environment variables, installs managers, or substitutes CLI programs.
#![cfg(target_os = "linux")]
use rustpy_project::environment_manager::{Event, Manager, Registry, Request, Task};
use rustpy_project::interpreter::Interpreter;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct Owned(Task, bool);
impl Drop for Owned {
    fn drop(&mut self) {
        if !self.1 {
            self.0.cancel();
            while !matches!(self.0.poll(), Some(Event::Finished(_))) {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
fn finish(owner: &mut Owned) -> (Interpreter, Vec<String>) {
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut output = Vec::new();
    loop {
        match owner.0.poll() {
            Some(Event::Progress(line)) => output.push(line),
            Some(Event::Finished(result)) => {
                owner.1 = true;
                return (
                    result.expect("genuine manager must create/probe the environment"),
                    output,
                );
            }
            None => {}
        }
        assert!(
            Instant::now() < deadline,
            "manager did not complete: {output:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn executable(name: &str) -> PathBuf {
    let path = PathBuf::from(std::env::var_os(name).expect(name));
    assert!(
        path.is_absolute() && path.is_file(),
        "admit installed absolute {name}"
    );
    path
}
fn private_env(name: &str, root: &Path) {
    let path = PathBuf::from(std::env::var_os(name).expect(name));
    assert!(path.is_absolute() && path.is_dir());
    assert!(
        dunce::canonicalize(&path).unwrap().starts_with(root),
        "{name} must be privately owned"
    );
}

#[test]
#[ignore = "explicit installed Pipenv/Hatch/PDM and Python; private offline manager caches/config required; run alone"]
fn configured_pipenv_hatch_pdm_create_probe_reuse_and_persist_private_environments() {
    assert_eq!(
        std::env::var("RUSTPY_ADDITIONAL_MANAGERS_ADMITTED").as_deref(),
        Ok("1")
    );
    let base = executable("RUSTPY_ENVIRONMENT_PYTHON");
    let parent =
        PathBuf::from(std::env::var_os("RUSTPY_ADDITIONAL_MANAGER_ROOT").expect("private root"));
    assert!(parent.is_absolute() && parent.is_dir());
    let parent = dunce::canonicalize(&parent).unwrap();
    for name in [
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "XDG_DATA_HOME",
        "PIPENV_CACHE_DIR",
        "WORKON_HOME",
        "VIRTUALENV_OVERRIDE_APP_DATA",
        "HATCH_DATA_DIR",
        "HATCH_CACHE_DIR",
        "PDM_CACHE_DIR",
    ] {
        private_env(name, &parent);
    }
    assert_eq!(std::env::var("PIP_NO_INDEX").as_deref(), Ok("1"));
    assert_eq!(std::env::var("PIP_CONFIG_FILE").as_deref(), Ok("/dev/null"));
    assert_eq!(std::env::var("PIPENV_DONT_LOAD_ENV").as_deref(), Ok("1"));
    assert_eq!(std::env::var("PDM_CHECK_UPDATE").as_deref(), Ok("false"));
    assert_eq!(
        std::env::var("VIRTUALENV_NO_PERIODIC_UPDATE").as_deref(),
        Ok("1")
    );
    let private = tempfile::Builder::new()
        .prefix("additional-managers-λ-")
        .tempdir_in(&parent)
        .unwrap();
    for (manager, variable) in [
        (Manager::Pipenv, "RUSTPY_PIPENV_EXECUTABLE"),
        (Manager::Hatch, "RUSTPY_HATCH_EXECUTABLE"),
        (Manager::Pdm, "RUSTPY_PDM_EXECUTABLE"),
    ] {
        let program = executable(variable);
        let root = private.path().join(manager.name());
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("Pipfile"), "[packages]\n[dev-packages]\n").unwrap();
        // Hatch's environment is deliberately detached from packaging/build
        // backends: no build frontend or network dependency resolution is needed.
        std::fs::write(root.join("pyproject.toml"), "[project]\nname='rustpy-owned-manager'\nversion='1.0'\ndependencies=[]\n[tool.hatch.envs.default]\nskip-install=true\n[tool.pdm]\ndistribution=false\n").unwrap();
        let request = Request {
            root: root.clone(),
            manager,
            existing: false,
            executable: program,
            base: base.clone(),
            location: root.join(".venv"),
            python_version: String::new(),
            inherit_site_packages: false,
            install: false,
        };
        assert!(
            Task::start(request.clone(), false).is_err(),
            "trust refusal before any provider"
        );
        assert!(!root.join(".venv").exists());
        let mut task = Owned(Task::start(request.clone(), true).unwrap(), false);
        let (created, progress) = finish(&mut task);
        assert!(created.is_venv && !created.version.is_empty() && !created.sys_path.is_empty());
        assert!(
            dunce::canonicalize(&created.prefix)
                .unwrap()
                .starts_with(&parent),
            "manager must not select/mutate an ambient SDK"
        );
        assert_ne!(created.path, base);
        assert!(progress.iter().any(|line| line.contains(manager.name())));
        let mut reused = Owned(
            Task::start(
                Request {
                    existing: true,
                    location: created.prefix.clone(),
                    ..request
                },
                true,
            )
            .unwrap(),
            false,
        );
        let (existing, output) = finish(&mut reused);
        assert!(
            output.is_empty(),
            "existing reuse must not execute manager commands"
        );
        assert_eq!(existing.path, created.path);
        assert_eq!(existing.version, created.version);
        let registry_path = root.join("registry.json");
        let mut registry = Registry::default();
        registry.remember(created, &root, manager.name(), false);
        registry.save(&registry_path).unwrap();
        let restored = Registry::load(&registry_path);
        assert_eq!(restored.entries.len(), 1);
        assert_eq!(restored.entries[0].interpreter.path, existing.path);
        assert!(restored.entries[0].projects.contains(&root));
        assert!(!restored.entries[0].shared);
    }
}
