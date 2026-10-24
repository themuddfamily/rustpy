//! Python package management backend (the "Python Packages" tool window,
//! `requirements.txt` inspections): installed-package listing through pip (or
//! `importlib.metadata` when pip is missing), `pip show` details, background
//! install/uninstall/upgrade tasks with streamed output, PyPI JSON lookups,
//! PEP 440-ish version handling and known-vulnerability checks against OSV
//! ("Vulnerable Dependencies", see [`vuln`]).
//!
//! All functions that run the interpreter block until the command finishes
//! (typically well under a second for `list`, longer for `--outdated`, which
//! talks to the index); call them from a worker thread in the UI.

pub mod distributions;
pub mod pip;
pub mod project;
pub mod pypi;
pub mod requirements;
pub mod requirements_file;
pub mod version;
pub mod vuln;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

pub use distributions::{distribution_for_module, provider_for_module, Provider};
pub use pip::{
    install, install_requirements, install_requirements_with, is_externally_managed_error,
    uninstall, uninstall_with, upgrade, upgrade_with, PipTask, PipTaskKind, BREAK_SYSTEM_PACKAGES,
};
pub use pypi::{pypi_project, pypi_versions, search_pypi, PypiProject, PypiSummary};
pub use requirements::{normalize_name, parse_requirements, unsatisfied, Requirement};
pub use version::{parse_version, specifier_matches, Version};
pub use vuln::{
    check_requirements, check_vulnerabilities, parse_osv_batch, parse_osv_vuln, pinned_version,
    severity_rank, suggested_upgrade, Vulnerability, MAX_DETAIL_LOOKUPS,
};

/// One installed distribution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
    /// Site directory (or the project directory for editable installs).
    pub location: Option<PathBuf>,
    pub editable: bool,
    /// Only filled by the `importlib.metadata` fallback; `pip list` has no summaries.
    pub summary: Option<String>,
}

/// `pip show` details.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
    pub summary: String,
    pub home_page: String,
    pub author: String,
    pub license: String,
    pub requires: Vec<String>,
    pub required_by: Vec<String>,
    pub location: Option<PathBuf>,
}

/// Which tool installs packages into an interpreter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PackageManager {
    /// `python -m pip`.
    Pip,
    /// `uv pip … --python <interpreter>` (uv-created venvs ship without pip).
    Uv { exe: PathBuf },
}

impl PackageManager {
    pub fn label(&self) -> &'static str {
        match self {
            PackageManager::Pip => "pip",
            PackageManager::Uv { .. } => "uv",
        }
    }
}

fn find_uv() -> Option<PathBuf> {
    if let Ok(p) = which::which("uv") {
        return Some(p);
    }
    let home = dirs::home_dir()?;
    [home.join(".local/bin/uv"), home.join(".cargo/bin/uv")]
        .into_iter()
        .find(|p| p.is_file())
}

type ManagerCache = std::sync::Mutex<std::collections::HashMap<PathBuf, PackageManager>>;

fn manager_cache() -> &'static ManagerCache {
    static CACHE: std::sync::OnceLock<ManagerCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The manager detected earlier for `interpreter`, without probing (UI-safe).
pub fn package_manager_cached(interpreter: &Path) -> Option<PackageManager> {
    manager_cache()
        .lock()
        .ok()
        .and_then(|c| c.get(interpreter).cloned())
}

/// pip when `python -m pip` works, otherwise uv when installed (cached per interpreter).
pub fn package_manager(interpreter: &Path) -> PackageManager {
    let cache = manager_cache();
    if let Some(m) = cache.lock().ok().and_then(|c| c.get(interpreter).cloned()) {
        return m;
    }
    let m = if pip_available_raw(interpreter) {
        PackageManager::Pip
    } else {
        find_uv()
            .map(|exe| PackageManager::Uv { exe })
            .unwrap_or(PackageManager::Pip)
    };
    if let Ok(mut c) = cache.lock() {
        c.insert(interpreter.to_path_buf(), m.clone());
    }
    m
}

/// Translates pip arguments to `uv pip` arguments for `interpreter`.
pub fn uv_pip_args(interpreter: &Path, pip_args: &[String]) -> Vec<String> {
    let mut rest: Vec<String> = Vec::new();
    let mut it = pip_args.iter().peekable();
    let sub = it.next().cloned().unwrap_or_default();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--progress-bar" => {
                it.next();
            }
            "--no-input" | "-v" => {}
            "-y" if sub == "uninstall" => {}
            f if f.starts_with("--format=") => {
                rest.push("--format".into());
                rest.push(f.trim_start_matches("--format=").into());
            }
            _ => rest.push(a.clone()),
        }
    }
    let mut v = vec![
        "pip".to_string(),
        sub,
        "--python".into(),
        interpreter.display().to_string(),
    ];
    v.extend(rest);
    v
}

/// Builds `python -m pip <args>` (or the uv equivalent) with the environment needed for machine output.
fn pip_command(interpreter: &Path, args: &[&str]) -> Command {
    if let PackageManager::Uv { exe } = package_manager(interpreter) {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let mut cmd = Command::new(exe);
        cmd.args(uv_pip_args(interpreter, &owned));
        cmd.env("NO_COLOR", "1");
        cmd.envs(rustpy_net::proxy_env_vars());
        cmd.stdin(Stdio::null());
        return cmd;
    }
    pip_command_raw(interpreter, args)
}

fn pip_command_raw(interpreter: &Path, args: &[&str]) -> Command {
    let mut cmd = rustpy_core::interpreter_launch::interpreter_command(interpreter);
    cmd.arg("-m").arg("pip");
    cmd.args(args);
    cmd.env("PIP_DISABLE_PIP_VERSION_CHECK", "1");
    cmd.env("PIP_NO_INPUT", "1");
    cmd.env("PYTHONIOENCODING", "utf-8");
    cmd.env("PYTHONUNBUFFERED", "1");
    cmd.env("NO_COLOR", "1");
    // A manual proxy (Settings › HTTP Proxy) reaches pip as http(s)_proxy.
    cmd.envs(rustpy_net::proxy_env_vars());
    cmd.stdin(Stdio::null());
    cmd
}

/// Runs the command and returns stdout, or an error carrying stderr.
fn run_capture(mut cmd: Command, what: &str) -> Result<String> {
    let out = cmd.output().with_context(|| format!("cannot run {what}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let msg = if err.is_empty() {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        } else {
            err
        };
        bail!("{what} failed ({}): {}", out.status, last_lines(&msg, 6));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn last_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// True when packages can be managed for this interpreter (pip, or uv for pip-less venvs).
pub fn pip_available(interpreter: &Path) -> bool {
    match package_manager(interpreter) {
        PackageManager::Pip => pip_available_raw(interpreter),
        PackageManager::Uv { .. } => true,
    }
}

fn pip_available_raw(interpreter: &Path) -> bool {
    pip_command_raw(interpreter, &["--version"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `pip --version` text, e.g. `pip 24.0 from … (python 3.12)`.
pub fn pip_version(interpreter: &Path) -> Result<String> {
    Ok(
        run_capture(pip_command(interpreter, &["--version"]), "pip --version")?
            .trim()
            .to_string(),
    )
}

/// True for distro-managed interpreters (PEP 668 `EXTERNALLY-MANAGED` marker
/// in the stdlib directory): pip refuses to install into them unless
/// `--break-system-packages` is passed; the IDE should suggest a virtualenv.
pub fn is_externally_managed(interpreter: &Path) -> bool {
    let mut cmd = rustpy_core::interpreter_launch::interpreter_command(interpreter);
    cmd.arg("-c").arg("import os, sys, sysconfig; print(int(os.path.exists(os.path.join(sysconfig.get_path('stdlib'), 'EXTERNALLY-MANAGED')) and sys.prefix == sys.base_prefix))").stdin(Stdio::null());
    cmd.output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "1")
        .unwrap_or(false)
}

#[derive(Deserialize)]
struct PipListEntry {
    name: String,
    version: String,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    editable_project_location: Option<String>,
}

/// Installed packages of `interpreter`, via `pip list --format=json`; falls back
/// to `importlib.metadata` when pip is unavailable. Sorted by normalized name.
pub fn list_installed(interpreter: &Path) -> Result<Vec<InstalledPackage>> {
    let via_pip = run_capture(
        pip_command(interpreter, &["list", "-v", "--format=json"]),
        "pip list",
    )
    .and_then(|out| parse_pip_list(&out));
    let mut packages = match via_pip {
        Ok(p) => p,
        Err(e) => {
            log::warn!("pip list failed ({e:#}); falling back to importlib.metadata");
            metadata_list(interpreter)
                .map_err(|e2| anyhow!("{e:#}; fallback failed too: {e2:#}"))?
        }
    };
    packages.sort_by_key(|p| normalize_name(&p.name));
    packages.dedup_by_key(|p| normalize_name(&p.name));
    Ok(packages)
}

fn parse_pip_list(out: &str) -> Result<Vec<InstalledPackage>> {
    // pip may print warnings before the JSON; take the array itself.
    let start = out
        .find('[')
        .ok_or_else(|| anyhow!("no JSON in pip list output"))?;
    let entries: Vec<PipListEntry> =
        serde_json::from_str(out[start..].trim()).context("cannot parse pip list JSON")?;
    Ok(entries
        .into_iter()
        .map(|e| {
            let editable = e.editable_project_location.is_some();
            let location = e
                .editable_project_location
                .or(e.location)
                .map(PathBuf::from);
            InstalledPackage {
                name: e.name,
                version: e.version,
                location,
                editable,
                summary: None,
            }
        })
        .collect())
}

const METADATA_SCRIPT: &str = r#"
import json, sys
try:
    import importlib.metadata as m
except ImportError:
    import importlib_metadata as m
out = []
for d in m.distributions():
    try:
        md = d.metadata
        name = md["Name"]
    except Exception:
        continue
    if not name:
        continue
    editable = False
    try:
        du = d.read_text("direct_url.json")
        if du:
            editable = bool(json.loads(du).get("dir_info", {}).get("editable"))
    except Exception:
        pass
    loc = None
    try:
        loc = str(d.locate_file(""))
    except Exception:
        pass
    out.append({"name": name, "version": md["Version"] or "", "summary": md["Summary"], "location": loc, "editable": editable})
json.dump(out, sys.stdout)
"#;

#[derive(Deserialize)]
struct MetadataEntry {
    name: String,
    version: String,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    editable: bool,
}

fn metadata_list(interpreter: &Path) -> Result<Vec<InstalledPackage>> {
    let mut cmd = rustpy_core::interpreter_launch::interpreter_command(interpreter);
    cmd.arg("-c")
        .arg(METADATA_SCRIPT)
        .env("PYTHONIOENCODING", "utf-8")
        .stdin(Stdio::null());
    let out = run_capture(cmd, "importlib.metadata listing")?;
    let start = out
        .find('[')
        .ok_or_else(|| anyhow!("no JSON in metadata output"))?;
    let entries: Vec<MetadataEntry> =
        serde_json::from_str(out[start..].trim()).context("cannot parse metadata JSON")?;
    Ok(entries
        .into_iter()
        .map(|e| InstalledPackage {
            name: e.name,
            version: e.version,
            location: e.location.map(PathBuf::from),
            editable: e.editable,
            summary: e.summary.filter(|s| !s.is_empty()),
        })
        .collect())
}

#[derive(Deserialize)]
struct PipOutdatedEntry {
    name: String,
    version: String,
    latest_version: String,
}

/// Packages with a newer release on the index: `(name, installed, latest)`.
/// Talks to the package index, so it is slow and fails offline.
pub fn outdated(interpreter: &Path) -> Result<Vec<(String, String, String)>> {
    let out = run_capture(
        pip_command(interpreter, &["list", "--outdated", "--format=json"]),
        "pip list --outdated",
    )?;
    let start = out
        .find('[')
        .ok_or_else(|| anyhow!("no JSON in pip list --outdated output"))?;
    let entries: Vec<PipOutdatedEntry> = serde_json::from_str(out[start..].trim())
        .context("cannot parse pip list --outdated JSON")?;
    let mut v: Vec<(String, String, String)> = entries
        .into_iter()
        .map(|e| (e.name, e.version, e.latest_version))
        .collect();
    v.sort_by_key(|(n, _, _)| normalize_name(n));
    Ok(v)
}

/// `pip show <name>` details; `Err` when the package is not installed.
pub fn package_info(interpreter: &Path, name: &str) -> Result<PackageInfo> {
    let name = name.trim();
    if name.is_empty() {
        bail!("empty package name");
    }
    let out = run_capture(
        pip_command(interpreter, &["show", name]),
        &format!("pip show {name}"),
    )?;
    let info = parse_pip_show(&out);
    if info.name.is_empty() {
        bail!("package `{name}` is not installed");
    }
    Ok(info)
}

fn parse_pip_show(out: &str) -> PackageInfo {
    let mut info = PackageInfo::default();
    let split_list = |v: &str| -> Vec<String> {
        v.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };
    for line in out.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "Name" => info.name = value.to_string(),
            "Version" => info.version = value.to_string(),
            "Summary" => info.summary = value.to_string(),
            "Home-page" => info.home_page = value.to_string(),
            "Author" => info.author = value.to_string(),
            "License" | "License-Expression" if info.license.is_empty() => {
                info.license = value.to_string()
            }
            "Location" => {
                info.location = Some(PathBuf::from(value)).filter(|p| !p.as_os_str().is_empty())
            }
            "Requires" => info.requires = split_list(value),
            "Required-by" => info.required_by = split_list(value),
            _ => {}
        }
    }
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pip_list_json() {
        let out = r#"WARNING: something
[{"name": "Requests", "version": "2.31.0", "location": "/usr/lib/python3/dist-packages", "installer": "pip"},
 {"name": "mypkg", "version": "0.1", "location": "/x", "editable_project_location": "/home/me/mypkg"}]"#;
        let p = parse_pip_list(out).unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].name, "Requests");
        assert_eq!(
            p[0].location.as_deref(),
            Some(Path::new("/usr/lib/python3/dist-packages"))
        );
        assert!(!p[0].editable);
        assert!(p[1].editable);
        assert_eq!(p[1].location.as_deref(), Some(Path::new("/home/me/mypkg")));
    }

    #[test]
    fn parses_pip_show() {
        let out = "Name: requests\nVersion: 2.31.0\nSummary: Python HTTP for Humans.\nHome-page: https://requests.readthedocs.io\nAuthor: Kenneth Reitz\nAuthor-email: me@kennethreitz.org\nLicense: Apache 2.0\nLocation: /usr/lib/python3/dist-packages\nRequires: certifi, charset-normalizer, idna, urllib3\nRequired-by: \n";
        let info = parse_pip_show(out);
        assert_eq!(info.name, "requests");
        assert_eq!(info.version, "2.31.0");
        assert_eq!(info.home_page, "https://requests.readthedocs.io");
        assert_eq!(info.license, "Apache 2.0");
        assert_eq!(
            info.requires,
            vec!["certifi", "charset-normalizer", "idna", "urllib3"]
        );
        assert!(info.required_by.is_empty());
        assert_eq!(
            info.location.as_deref(),
            Some(Path::new("/usr/lib/python3/dist-packages"))
        );
    }
}

#[cfg(test)]
mod package_manager_tests {
    use super::*;

    #[test]
    fn translates_pip_arguments_for_uv() {
        let py = Path::new("/p/.venv/bin/python");
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            uv_pip_args(
                py,
                &a(&[
                    "install",
                    "--progress-bar",
                    "off",
                    "--break-system-packages",
                    "requests>=2"
                ])
            ),
            a(&[
                "pip",
                "install",
                "--python",
                "/p/.venv/bin/python",
                "--break-system-packages",
                "requests>=2"
            ])
        );
        assert_eq!(
            uv_pip_args(py, &a(&["uninstall", "-y", "flask"])),
            a(&[
                "pip",
                "uninstall",
                "--python",
                "/p/.venv/bin/python",
                "flask"
            ])
        );
        assert_eq!(
            uv_pip_args(py, &a(&["list", "-v", "--format=json"])),
            a(&[
                "pip",
                "list",
                "--python",
                "/p/.venv/bin/python",
                "--format",
                "json"
            ])
        );
        assert_eq!(
            uv_pip_args(py, &a(&["list", "--outdated", "--format=json"])),
            a(&[
                "pip",
                "list",
                "--python",
                "/p/.venv/bin/python",
                "--outdated",
                "--format",
                "json"
            ])
        );
        assert_eq!(
            uv_pip_args(
                py,
                &a(&["install", "--upgrade", "--progress-bar", "off", "x"])
            ),
            a(&[
                "pip",
                "install",
                "--python",
                "/p/.venv/bin/python",
                "--upgrade",
                "x"
            ])
        );
    }
}
