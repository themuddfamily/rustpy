//! Integration tests against the system `python3` (skipped when absent).

use std::path::PathBuf;

fn python() -> Option<PathBuf> {
    which::which("python3").ok()
}

#[test]
fn lists_installed_packages() {
    let Some(py) = python() else {
        eprintln!("python3 not found; skipping");
        return;
    };
    let packages = rustpy_packages::list_installed(&py).expect("list_installed");
    // Sorted by normalized name, no duplicates.
    let names: Vec<String> = packages
        .iter()
        .map(|p| rustpy_packages::normalize_name(&p.name))
        .collect();
    let mut sorted = names.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(names, sorted);
    for p in &packages {
        assert!(!p.name.is_empty());
        assert!(!p.version.is_empty(), "{} has no version", p.name);
    }
    if rustpy_packages::pip_available(&py) {
        assert!(rustpy_packages::pip_version(&py)
            .unwrap()
            .starts_with("pip "));
        if let Some(p) = packages.first() {
            let info = rustpy_packages::package_info(&py, &p.name).expect("pip show");
            assert_eq!(
                rustpy_packages::normalize_name(&info.name),
                rustpy_packages::normalize_name(&p.name)
            );
            assert_eq!(info.version, p.version);
        }
        assert!(rustpy_packages::package_info(&py, "definitely-not-installed-xyz-123").is_err());
    }
}

#[test]
fn uninstall_of_missing_package_streams_output_and_exits() {
    let Some(py) = python() else {
        return;
    };
    if !rustpy_packages::pip_available(&py) {
        return;
    }
    // Distro interpreters refuse pip changes (PEP 668); uninstalling a missing package is a safe no-op with the override.
    let extra: Vec<String> = if rustpy_packages::is_externally_managed(&py) {
        vec![rustpy_packages::BREAK_SYSTEM_PACKAGES.to_string()]
    } else {
        Vec::new()
    };
    let mut task = rustpy_packages::uninstall_with(&py, "definitely-not-installed-xyz-123", &extra)
        .expect("spawn");
    assert!(task.is_running());
    assert_eq!(
        task.kind,
        rustpy_packages::PipTaskKind::Uninstall {
            name: "definitely-not-installed-xyz-123".into()
        }
    );
    let mut lines = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while task.is_running() && std::time::Instant::now() < deadline {
        lines.extend(task.poll());
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    lines.extend(task.poll());
    assert!(!task.is_running(), "pip uninstall did not finish");
    assert!(task.exit_code().is_some());
    assert!(
        lines
            .iter()
            .any(|l| l.to_lowercase().contains("not installed")),
        "output: {lines:?}"
    );
    assert!(!rustpy_packages::is_externally_managed_error(&lines));
}

#[test]
fn detects_pep668_refusal_without_override() {
    let Some(py) = python() else {
        return;
    };
    if !rustpy_packages::pip_available(&py) || !rustpy_packages::is_externally_managed(&py) {
        return;
    }
    // Some environments (e.g. CI images) switch the protection off via
    // PIP_BREAK_SYSTEM_PACKAGES or pip.conf; pip then never refuses.
    let pip_config = std::process::Command::new(&py)
        .args(["-m", "pip", "config", "list"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase())
        .unwrap_or_default();
    if std::env::var_os("PIP_BREAK_SYSTEM_PACKAGES").is_some()
        || pip_config.contains("break-system-packages")
    {
        eprintln!("skipping: PEP 668 protection is overridden in this environment");
        return;
    }
    let mut task =
        rustpy_packages::uninstall(&py, "definitely-not-installed-xyz-123").expect("spawn");
    let mut lines = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while task.is_running() && std::time::Instant::now() < deadline {
        lines.extend(task.poll());
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    lines.extend(task.poll());
    assert_ne!(task.exit_code(), Some(0));
    assert!(
        rustpy_packages::is_externally_managed_error(&lines),
        "output: {lines:?}"
    );
}

#[test]
fn stop_terminates_a_task() {
    let Some(py) = python() else {
        return;
    };
    if !rustpy_packages::pip_available(&py) {
        return;
    }
    // A resolvable-but-slow command: install from a dead index with retries.
    let mut task = rustpy_packages::install(
        &py,
        "rustpy-nonexistent-package-xyz",
        &[
            "--index-url".into(),
            "http://127.0.0.1:9/simple".into(),
            "--retries".into(),
            "10".into(),
            "--timeout".into(),
            "1".into(),
        ],
    )
    .expect("spawn");
    std::thread::sleep(std::time::Duration::from_millis(300));
    task.stop();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while task.is_running() && std::time::Instant::now() < deadline {
        task.poll();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!task.is_running());
    assert!(task.was_stopped());
}

/// Needs `uv` and network access: `cargo test -p rustpy-packages --test interpreter -- --ignored uv_`.
#[test]
#[ignore]
fn uv_venv_without_pip_installs_and_lists_packages() {
    let Ok(uv) = which::which("uv").or_else(|_| dirs_home_uv()) else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let venv = dir.path().join(".venv");
    assert!(std::process::Command::new(&uv)
        .args(["venv", "-q"])
        .arg(&venv)
        .status()
        .unwrap()
        .success());
    let py = venv.join("bin/python");
    assert_eq!(rustpy_packages::package_manager(&py).label(), "uv");
    let mut task = rustpy_packages::install(&py, "six", &[]).expect("spawn");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let mut lines = Vec::new();
    while task.is_running() && std::time::Instant::now() < deadline {
        lines.extend(task.poll());
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    lines.extend(task.poll());
    assert_eq!(task.exit_code(), Some(0), "{lines:?}");
    let installed = rustpy_packages::list_installed(&py).unwrap();
    assert!(installed.iter().any(|p| p.name == "six"), "{installed:?}");
}

fn dirs_home_uv() -> Result<std::path::PathBuf, ()> {
    let p = std::path::PathBuf::from(std::env::var("HOME").map_err(|_| ())?).join(".local/bin/uv");
    if p.is_file() {
        Ok(p)
    } else {
        Err(())
    }
}
