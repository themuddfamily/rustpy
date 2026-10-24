//! Python interpreter discovery and introspection (roadmap §2.12).

use std::path::{Path, PathBuf};

pub use rustpy_core::interpreter_launch::{
    interpreter_command, interpreter_invocation, try_interpreter_command,
};
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interpreter {
    pub path: PathBuf,
    /// e.g. `3.12.3`
    pub version: String,
    pub prefix: PathBuf,
    pub sys_path: Vec<PathBuf>,
    pub is_venv: bool,
    pub platform: String,
    /// Human readable origin: "venv", "conda", "pyenv", "system", "project".
    pub kind: String,
}

impl Interpreter {
    /// `Python 3.12 (venv)` style label.
    pub fn label(&self) -> String {
        let short: String = self
            .version
            .split('.')
            .take(2)
            .collect::<Vec<_>>()
            .join(".");
        let name = if self.is_venv {
            self.prefix
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.kind.clone())
        } else {
            self.kind.clone()
        };
        format!("Python {short} ({name})")
    }

    pub fn major_minor(&self) -> (u32, u32) {
        let mut it = self
            .version
            .split('.')
            .filter_map(|p| p.parse::<u32>().ok());
        (it.next().unwrap_or(3), it.next().unwrap_or(12))
    }

    /// Site-packages and other library directories (for module resolution).
    pub fn search_paths(&self) -> Vec<PathBuf> {
        self.sys_path
            .iter()
            .filter(|p| p.is_dir())
            .cloned()
            .collect()
    }
}

pub const PROBE: &str = r#"import sys, json, platform, struct, sysconfig
print(json.dumps({"version": "%d.%d.%d" % sys.version_info[:3], "full_version": sys.version, "prefix": sys.prefix, "base_prefix": getattr(sys, "base_prefix", sys.prefix), "path": [p for p in sys.path if p], "platform": sys.platform, "architecture": platform.machine(), "bits": struct.calcsize("P") * 8, "sysconfig": sysconfig.get_paths()}))"#;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterpreterMetadata {
    pub interpreter: Interpreter,
    pub full_version: String,
    pub architecture: String,
    pub bits: u64,
    pub sysconfig: std::collections::BTreeMap<String, PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidInterpreter {
    pub path: PathBuf,
    pub kind: String,
    pub reason: String,
}
impl InvalidInterpreter {
    pub fn label(&self) -> String {
        format!(
            "Invalid interpreter: {} ({})",
            self.path.display(),
            self.reason
        )
    }
}

/// Decode the same introspection payload for local and admitted remote probes.
pub fn decode_metadata(
    path: &Path,
    kind: &str,
    output: &[u8],
) -> Result<InterpreterMetadata, String> {
    let json: serde_json::Value =
        serde_json::from_slice(output).map_err(|e| format!("Invalid introspection JSON: {e}"))?;
    let required = |key: &str| {
        json[key]
            .as_str()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("Missing {key}"))
    };
    let version = required("version")?.to_owned();
    if version.split('.').count() != 3 || !version.split('.').all(|v| v.parse::<u32>().is_ok()) {
        return Err("Invalid Python version".into());
    }
    let prefix = PathBuf::from(required("prefix")?);
    let base_prefix = PathBuf::from(
        json["base_prefix"]
            .as_str()
            .unwrap_or_else(|| json["prefix"].as_str().unwrap()),
    );
    let paths = json["path"].as_array().ok_or("Missing sys.path")?;
    let sys_path = paths
        .iter()
        .map(|v| {
            v.as_str()
                .map(PathBuf::from)
                .ok_or("Invalid sys.path entry")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let sysconfig = json["sysconfig"]
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k.clone(), PathBuf::from(v.as_str()?))))
                .collect()
        })
        .unwrap_or_default();
    Ok(InterpreterMetadata {
        interpreter: Interpreter {
            path: path.to_path_buf(),
            version,
            is_venv: prefix != base_prefix,
            prefix,
            sys_path,
            platform: required("platform")?.to_owned(),
            kind: kind.to_owned(),
        },
        full_version: json["full_version"].as_str().unwrap_or("").to_owned(),
        architecture: json["architecture"]
            .as_str()
            .unwrap_or("unknown")
            .to_owned(),
        bits: json["bits"].as_u64().unwrap_or(0),
        sysconfig,
    })
}

/// Hard limit for one interpreter introspection; the child is killed and
/// reaped when it is exceeded.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
/// Remote transports pay for a connection per launch: `vagrant ssh` alone takes
/// several seconds before the interpreter starts, a cold `docker run` likewise.
pub const REMOTE_PROBE_TIMEOUT: Duration = Duration::from_secs(60);

/// True for a Microsoft Store app-execution alias (`.../WindowsApps/python.exe`)
/// seen from a Unix host such as WSL. These zero-byte reparse stubs often block
/// waiting for the Store instead of running Python, so discovery skips them.
pub fn is_foreign_app_execution_alias(path: &Path) -> bool {
    !cfg!(windows)
        && path
            .components()
            .any(|c| c.as_os_str().eq_ignore_ascii_case("WindowsApps"))
}

pub fn probe_detailed(path: &Path, kind: &str) -> Result<InterpreterMetadata, InvalidInterpreter> {
    probe_detailed_with_timeout(path, kind, PROBE_TIMEOUT)
}

pub(crate) fn probe_detailed_with_timeout(
    path: &Path,
    kind: &str,
    timeout: Duration,
) -> Result<InterpreterMetadata, InvalidInterpreter> {
    let result = try_interpreter_command(path)
        .map_err(|e| e.to_string())
        .and_then(|mut command| {
            command.args(["-I", "-c", PROBE]);
            crate::probe::command_output(command, timeout).map_err(|e| format!("{e:?}"))
        })
        .and_then(|out| decode_metadata(path, kind, &out))
        .map_err(|reason| InvalidInterpreter {
            path: path.into(),
            kind: kind.into(),
            reason,
        });
    if let Ok(mut cache) = metadata_cache().lock() {
        if cache.len() >= 256 && !cache.contains_key(path) {
            cache.pop_first();
        }
        cache.insert(path.to_path_buf(), result.clone());
    }
    result
}

// Metadata is retained separately to preserve the stable Interpreter wire format.
fn metadata_cache() -> &'static std::sync::Mutex<
    std::collections::BTreeMap<PathBuf, Result<InterpreterMetadata, InvalidInterpreter>>,
> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<
            std::collections::BTreeMap<PathBuf, Result<InterpreterMetadata, InvalidInterpreter>>,
        >,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}
pub fn cached_probe(path: &Path) -> Option<Result<InterpreterMetadata, InvalidInterpreter>> {
    metadata_cache().lock().ok()?.get(path).cloned()
}

/// Runs the interpreter at `path` to read its version and `sys.path`.
pub fn probe(path: &Path, kind: &str) -> Option<Interpreter> {
    probe_detailed(path, kind)
        .ok()
        .map(|metadata| metadata.interpreter)
}

/// Candidate interpreter paths with their origin kind, without probing.
pub fn candidate_paths(project_root: Option<&Path>) -> Vec<(PathBuf, String)> {
    candidate_paths_with(project_root, true)
}

/// Like [`candidate_paths`]; with `include_project_envs` false (an untrusted
/// project) nothing inside `project_root` is a candidate: neither its
/// `.venv`/`venv`/`env`/`.env` nor a `python` found there through `PATH` or
/// `VIRTUAL_ENV`. Probing runs the binary, so this keeps safe mode from
/// executing a file committed to the repository.
pub fn candidate_paths_with(
    project_root: Option<&Path>,
    include_project_envs: bool,
) -> Vec<(PathBuf, String)> {
    let mut out = all_candidates(
        if include_project_envs {
            project_root
        } else {
            None
        },
        include_project_envs,
    );
    if let (Some(root), false) = (project_root, include_project_envs) {
        let real_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        out.retain(|(p, _)| {
            let real = p.canonicalize().unwrap_or_else(|_| p.clone());
            !p.starts_with(root) && !real.starts_with(&real_root)
        });
    }
    out
}

fn all_candidates(project_root: Option<&Path>, allow_tools: bool) -> Vec<(PathBuf, String)> {
    let home = dirs::home_dir().unwrap_or_default();
    let mut out = project_tool_envs(project_root);
    if allow_tools {
        out.extend(platform_tool_candidates());
    }
    for c in candidate_paths_in(project_root, &home, &|k| std::env::var(k).ok()) {
        if !out.iter().any(|(p, _)| p == &c.0) {
            out.push(c);
        }
    }
    out.retain(|(p, _)| !is_foreign_app_execution_alias(p));
    dedup_candidates(out)
}

fn bin_exe() -> (&'static str, &'static str) {
    if cfg!(windows) {
        ("Scripts", "python.exe")
    } else {
        ("bin", "python")
    }
}

/// `pyproject.toml` / `Pipfile` based environments of the project itself,
/// asked from the tools (`poetry env info --path`, `pipenv --venv`, `hatch env find`).
fn project_tool_envs(project_root: Option<&Path>) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let Some(root) = project_root else { return out };
    let (bin, exe) = bin_exe();
    let pyproject = std::fs::read_to_string(root.join("pyproject.toml")).unwrap_or_default();
    let ask = |tool: &str, args: &[&str]| -> Option<PathBuf> {
        let prog = which::which(tool).ok()?;
        let mut cmd_args: Vec<&str> = Vec::new();
        cmd_args.extend_from_slice(args);
        let out = run_in_dir_with_timeout(&prog, &cmd_args, root, Duration::from_secs(5))?;
        let text = String::from_utf8_lossy(&out);
        let line = text
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty())?
            .to_string();
        Some(PathBuf::from(line))
    };
    if pyproject.contains("[tool.poetry") {
        if let Some(env) = ask("poetry", &["env", "info", "--path"]) {
            out.push((env.join(bin).join(exe), "poetry venv".to_string()));
        }
    }
    if root.join("Pipfile").is_file() {
        if let Some(env) = ask("pipenv", &["--venv"]) {
            out.push((env.join(bin).join(exe), "pipenv venv".to_string()));
        }
    }
    if crate::manager_layouts::is_hatch_project(root) {
        if let Some(env) = ask("hatch", &["env", "find"]) {
            out.push((env.join(bin).join(exe), "hatch venv".to_string()));
        }
    }
    if pyproject.contains("[tool.pdm") || root.join("pdm.lock").is_file() {
        if let Some(python) = ask("pdm", &["info", "--python"]) {
            let in_venv = python
                .parent()
                .and_then(Path::parent)
                .is_some_and(|env| env.join("pyvenv.cfg").is_file());
            let kind = if root.join("__pypackages__").is_dir() && !in_venv {
                crate::manager_layouts::PDM_PEP582_KIND
            } else {
                "pdm venv"
            };
            out.push((python, kind.to_string()));
        }
    }
    out.retain(|(p, _)| p.is_file());
    out
}

fn run_in_dir_with_timeout(
    path: &Path,
    args: &[&str],
    dir: &Path,
    timeout: Duration,
) -> Option<Vec<u8>> {
    let mut command = interpreter_command(path);
    command.args(args).current_dir(dir);
    crate::probe::command_output(command, timeout).ok()
}

/// Normalized project name as Poetry/Pipenv put it in env directory names.
fn env_name_prefix(project_root: &Path) -> String {
    let name = project_root
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// Pure part of the discovery (testable with a fake home and environment).
pub fn candidate_paths_in(
    project_root: Option<&Path>,
    home: &Path,
    env: &dyn Fn(&str) -> Option<String>,
) -> Vec<(PathBuf, String)> {
    let mut out: Vec<(PathBuf, String)> = Vec::new();
    let mut push = |p: PathBuf, kind: &str| {
        if p.is_file() && !is_foreign_app_execution_alias(&p) && !out.iter().any(|(q, _)| q == &p) {
            out.push((p, kind.to_string()));
        }
    };
    let (bin, exe) = bin_exe();
    let prefix = project_root.map(env_name_prefix).unwrap_or_default();
    if let Some(root) = project_root {
        // PDM's selected interpreter (`.pdm-python`): an in-project/central venv,
        // or a base interpreter in PEP 582 (`__pypackages__`) mode.
        if let Some((python, kind)) = crate::manager_layouts::pdm_selected_python(root) {
            push(python, kind);
        }
        for venv in [".venv", "venv", "env", ".env"] {
            push(root.join(venv).join(bin).join(exe), "project venv");
        }
    }
    // Tool-managed environment stores; envs named after this project come first.
    let xdg_cache = env("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cache"));
    let xdg_data = env("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    let mut stores: Vec<(PathBuf, &str)> = vec![
        (xdg_cache.join("pypoetry/virtualenvs"), "poetry"),
        (home.join("Library/Caches/pypoetry/virtualenvs"), "poetry"),
        (
            env("WORKON_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| xdg_data.join("virtualenvs")),
            "pipenv",
        ),
        (home.join(".virtualenvs"), "virtualenv"),
    ];
    if let Some(local) = env("LOCALAPPDATA") {
        stores.push((
            PathBuf::from(local).join("pypoetry/Cache/virtualenvs"),
            "poetry",
        ));
    }
    let mut others: Vec<(PathBuf, String)> = Vec::new();
    for (dir, kind) in &stores {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for e in entries {
            let py = e.join(bin).join(exe);
            let name = e
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if !prefix.is_empty() && name.starts_with(&format!("{prefix}-")) {
                push(py, &format!("{kind} venv"));
            } else {
                others.push((py, kind.to_string()));
            }
        }
    }
    // Hatch: configured `dirs.env.virtual`, explicit env `path`s and the data
    // store `<data>/hatch/env/virtual/<project>/<id>/<env>`; the project's own
    // folder is matched by its normalised `[project] name`.
    let (hatch_mine, hatch_others) =
        crate::manager_layouts::hatch_env_dirs(project_root, home, env);
    for dir in hatch_mine {
        push(dir.join(bin).join(exe), "hatch venv");
    }
    for dir in hatch_others {
        others.push((dir.join(bin).join(exe), "hatch".to_string()));
    }
    if let Some(v) = env("VIRTUAL_ENV") {
        push(PathBuf::from(v).join(bin).join(exe), "venv");
    }
    if let Some(c) = env("CONDA_PREFIX") {
        push(PathBuf::from(c).join(bin).join(exe), "conda");
    }
    if let Some(path) = env("PATH") {
        for dir in std::env::split_paths(&path) {
            if !dir.is_absolute() {
                continue;
            }
            for executable in python_files(&dir) {
                // A conda base/env activated through PATH is still a conda SDK.
                let kind = if conda_prefix_of(&executable).is_some() {
                    "conda"
                } else {
                    "system"
                };
                push(executable, kind);
            }
        }
    }
    for v in ["3.14", "3.13", "3.12", "3.11", "3.10", "3.9", "3.8"] {
        push(PathBuf::from(format!("/usr/bin/python{v}")), "system");
        push(PathBuf::from(format!("/usr/local/bin/python{v}")), "system");
    }
    if cfg!(windows) {
        if let Some(local) = env("LOCALAPPDATA") {
            if let Ok(rd) = std::fs::read_dir(PathBuf::from(local).join("Programs/Python")) {
                for e in rd.flatten() {
                    push(e.path().join("python.exe"), "system");
                }
            }
        }
    }
    let conda_bin = if cfg!(windows) { "" } else { "bin" };
    let conda_exe = if cfg!(windows) {
        "python.exe"
    } else {
        "python"
    };
    for conda in [
        ".miniconda3",
        "miniconda3",
        "anaconda3",
        ".anaconda3",
        "miniforge3",
        "mambaforge",
    ] {
        let base = home.join(conda);
        push(base.join(conda_bin).join(conda_exe), "conda");
        if let Ok(rd) = std::fs::read_dir(base.join("envs")) {
            for e in rd.flatten() {
                push(e.path().join(conda_bin).join(conda_exe), "conda");
            }
        }
    }
    // Conda installations outside HOME (`/opt/conda`, a custom Miniforge prefix)
    // are located from the activation variables and `conda`/`mamba` on PATH,
    // without running the tool; their `envs` and the configured envs dirs follow.
    for root in conda_roots(env) {
        push(root.join(conda_bin).join(conda_exe), "conda");
        for e in sorted_children(&root.join("envs")) {
            push(e.join(conda_bin).join(conda_exe), "conda");
        }
    }
    for dir in conda_envs_dirs(home, env) {
        for e in sorted_children(&dir) {
            push(e.join(conda_bin).join(conda_exe), "conda");
        }
    }
    // Every environment conda knows about.
    if let Ok(list) = std::fs::read_to_string(home.join(".conda/environments.txt")) {
        for line in list
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            push(PathBuf::from(line).join(conda_bin).join(conda_exe), "conda");
        }
    }
    if let Ok(rd) = std::fs::read_dir(home.join(".pyenv").join("versions")) {
        for e in rd.flatten() {
            push(e.path().join("bin").join("python"), "pyenv");
        }
    }
    if let Ok(rd) = std::fs::read_dir(xdg_data.join("uv/python")) {
        for e in rd.flatten() {
            push(e.path().join("bin").join("python3"), "uv");
        }
    }
    for (path, kind) in additional_local_candidates(project_root, home, env) {
        push(path, &kind);
    }
    for (p, kind) in others {
        push(p, &kind);
    }
    out
}

/// The conda prefix owning `python` (`<prefix>/bin/python` with `conda-meta`).
fn conda_prefix_of(python: &Path) -> Option<PathBuf> {
    let dir = python.parent()?;
    let prefix = if cfg!(windows) { dir } else { dir.parent()? };
    prefix
        .join("conda-meta")
        .is_dir()
        .then(|| prefix.to_path_buf())
}

/// Conda/mamba installation roots known from the environment, never by
/// running a tool: `CONDA_EXE`, `CONDA_PYTHON_EXE`, `MAMBA_EXE`,
/// `MAMBA_ROOT_PREFIX`, `CONDA_ROOT` and `conda`/`mamba` executables on PATH
/// (`<root>/bin/conda` or `<root>/condabin/conda`).
fn conda_roots(env: &dyn Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let mut tools: Vec<PathBuf> = ["CONDA_EXE", "CONDA_PYTHON_EXE", "MAMBA_EXE"]
        .iter()
        .filter_map(|k| env(k))
        .map(PathBuf::from)
        .collect();
    if let Some(path) = env("PATH") {
        for dir in std::env::split_paths(&path).filter(|d| d.is_absolute()) {
            for name in ["conda", "mamba", "conda.exe", "mamba.exe"] {
                let tool = dir.join(name);
                if tool.is_file() {
                    tools.push(tool);
                }
            }
        }
    }
    // An explicit root prefix (micromamba) need not hold a base environment.
    let mut out: Vec<PathBuf> = ["MAMBA_ROOT_PREFIX", "CONDA_ROOT"]
        .iter()
        .filter_map(|k| env(k))
        .map(PathBuf::from)
        .filter(|root| root.is_absolute() && root.is_dir())
        .collect();
    let mut roots = Vec::new();
    for tool in tools {
        // Follow a PATH symlink (e.g. ~/.local/bin/conda) to the real install.
        let tool = tool.canonicalize().unwrap_or(tool);
        let Some(dir) = tool.parent() else { continue };
        let leaf = dir.file_name().unwrap_or_default().to_string_lossy();
        let root = if matches!(leaf.as_ref(), "bin" | "condabin" | "Scripts") {
            dir.parent()
        } else {
            Some(dir)
        };
        roots.extend(root.map(Path::to_path_buf));
    }
    for root in roots {
        if root.is_absolute() && root.join("conda-meta").is_dir() && !out.contains(&root) {
            out.push(root);
        }
    }
    out
}

/// Conda env directories outside an installation root: `~/.conda/envs` and
/// the `CONDA_ENVS_PATH`/`CONDA_ENVS_DIRS` lists.
fn conda_envs_dirs(home: &Path, env: &dyn Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for key in ["CONDA_ENVS_PATH", "CONDA_ENVS_DIRS"] {
        if let Some(list) = env(key) {
            for dir in std::env::split_paths(&list).filter(|d| d.is_absolute()) {
                if !out.contains(&dir) {
                    out.push(dir);
                }
            }
        }
    }
    let user = home.join(".conda/envs");
    if !out.contains(&user) {
        out.push(user);
    }
    out
}

/// Collapses symlinked duplicates of base interpreters, but never merges two
/// virtual environments (their `python` links to the same base interpreter).
fn dedup_candidates(mut out: Vec<(PathBuf, String)>) -> Vec<(PathBuf, String)> {
    let mut seen: Vec<PathBuf> = Vec::new();
    out.retain(|(p, kind)| {
        let managed_env = [
            "venv",
            "conda",
            "poetry",
            "pipenv",
            "hatch",
            "pdm",
            "virtualenv",
        ]
        .iter()
        .any(|name| kind.contains(name));
        let in_venv = p
            .parent()
            .and_then(Path::parent)
            .is_some_and(|env| env.join("pyvenv.cfg").is_file());
        let key = if in_venv || managed_env {
            p.clone()
        } else {
            p.canonicalize().unwrap_or_else(|_| p.clone())
        };
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
    out
}

/// Discovers and probes interpreters (slow: runs each candidate once).
pub fn discover_interpreters(project_root: Option<&Path>) -> Vec<Interpreter> {
    discover_interpreters_with(project_root, true)
}

/// Like [`discover_interpreters`]; pass `include_project_envs = false` while the
/// project is not trusted, so no binary from inside it is run (see [`candidate_paths_with`]).
pub fn discover_interpreters_with(
    project_root: Option<&Path>,
    include_project_envs: bool,
) -> Vec<Interpreter> {
    discover_interpreters_with_preferred(project_root, include_project_envs, None)
}

/// Probes a trusted workspace's saved choice before automatic candidates. Keep
/// the recorded path intact: resolving a venv's Python symlink loses its identity.
/// Untrusted workspace choices are never executed, even outside the project.
pub fn discover_interpreters_with_preferred(
    project_root: Option<&Path>,
    trusted: bool,
    preferred: Option<&Path>,
) -> Vec<Interpreter> {
    // Even a "system" binary may be a project-supplied PATH shim, and Python
    // startup (.pth/sitecustomize) can execute code before the probe. Safe mode
    // discovers no interpreters by running them; explicit trust restarts this.
    if project_root.is_some_and(|root| !trusted || !rustpy_settings::trust::root_unchanged(root)) {
        return Vec::new();
    }
    let model = trusted
        .then(|| project_root.and_then(|root| crate::project::ProjectModel::try_load(root).ok()))
        .flatten();
    let project_sdk = model.as_ref().and_then(|model| model.sdk.as_deref());
    let mut candidates = Vec::new();
    if let Some(model) = &model {
        for module in &model.modules {
            if let Some(path) = &module.sdk {
                candidates.push((path.clone(), "module SDK".into()));
            }
        }
    }
    candidates.extend(candidate_paths_with(project_root, trusted));
    discover_candidates(
        candidates,
        trusted.then_some(project_sdk.or(preferred)).flatten(),
        |path, kind| {
            if project_root.is_some_and(|root| !rustpy_settings::trust::root_unchanged(root)) {
                return None;
            }
            let interpreter = probe(path, kind)?;
            Some(match project_root {
                Some(root) => crate::manager_layouts::with_project_packages(interpreter, root),
                None => interpreter,
            })
        },
    )
}

fn discover_candidates(
    candidates: Vec<(PathBuf, String)>,
    preferred: Option<&Path>,
    mut probe_candidate: impl FnMut(&Path, &str) -> Option<Interpreter>,
) -> Vec<Interpreter> {
    let mut out = Vec::new();
    let started = std::time::Instant::now();
    if let Some(path) = preferred {
        if let Some(interpreter) = probe_candidate(path, "workspace") {
            out.push(interpreter);
        }
    }
    for (path, kind) in candidates.into_iter().take(256) {
        if started.elapsed() >= Duration::from_secs(40) {
            break;
        }
        if out.iter().any(|i: &Interpreter| i.path == path) {
            continue;
        }
        if let Some(interpreter) = probe_candidate(&path, &kind) {
            if kind == "module SDK"
                || !out.iter().any(|i: &Interpreter| {
                    i.prefix == interpreter.prefix && i.version == interpreter.version
                })
            {
                out.push(interpreter);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project whose `.venv/bin/python` (or `Scripts/python.exe`) is a marker file.
    fn fake_project(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("rustpy-interp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bin = if cfg!(windows) { "Scripts" } else { "bin" };
        let exe = if cfg!(windows) {
            "python.exe"
        } else {
            "python"
        };
        for venv in [".venv", "venv"] {
            let dir = root.join(venv).join(bin);
            std::fs::create_dir_all(&dir).unwrap();
            let file = dir.join(exe);
            std::fs::write(&file, "#!/bin/sh\ntouch \"$0.ran\"\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        root
    }

    fn fake_python_ran(root: &Path) -> bool {
        let dir = root
            .join(".venv")
            .join(if cfg!(windows) { "Scripts" } else { "bin" });
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().ends_with(".ran"))
    }

    #[test]
    fn untrusted_project_envs_are_not_candidates() {
        let root = fake_project("untrusted");
        let trusted = candidate_paths_with(Some(&root), true);
        assert!(
            trusted
                .iter()
                .any(|(p, kind)| p.starts_with(&root) && kind == "project venv"),
            "{trusted:?}"
        );
        let untrusted = candidate_paths_with(Some(&root), false);
        assert!(
            untrusted.iter().all(|(p, _)| !p.starts_with(&root)),
            "{untrusted:?}"
        );
        // Discovery in safe mode never executes the committed "python"…
        let _ = discover_interpreters_with(Some(&root), false);
        assert!(!fake_python_ran(&root));
        // …while trusted discovery probes (runs) it.
        #[cfg(unix)]
        {
            let _ = discover_interpreters_with(Some(&root), true);
            assert!(fake_python_ran(&root));
        }
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn probes_system_python() {
        let Ok(p) = which::which("python3") else {
            return;
        };
        let i = probe(&p, "system").expect("probe python3");
        assert!(i.version.starts_with('3'));
        assert!(!i.sys_path.is_empty());
        assert!(i.label().starts_with("Python 3."));
        let found = discover_interpreters(None);
        assert!(!found.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn finds_tool_managed_envs_and_keeps_distinct_venvs() {
        let root = std::env::temp_dir().join(format!("rustpy-interp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let project = root.join("work/My_Shop");
        std::fs::create_dir_all(&project).unwrap();
        let mk_venv = |dir: PathBuf| {
            std::fs::create_dir_all(dir.join("bin")).unwrap();
            std::fs::write(dir.join("pyvenv.cfg"), "home = /usr/bin\n").unwrap();
            let py = dir.join("bin/python");
            std::os::unix::fs::symlink("/usr/bin/python3", &py).unwrap();
            py
        };
        let mine = mk_venv(home.join(".cache/pypoetry/virtualenvs/my-shop-AbCd1234-py3.12"));
        let other = mk_venv(home.join(".cache/pypoetry/virtualenvs/other-XyZ-py3.12"));
        let pipenv = mk_venv(home.join(".local/share/virtualenvs/my-shop-Q1w2E3r4"));
        let hatch = mk_venv(home.join(".local/share/hatch/env/virtual/my-shop/aB12/default"));
        let conda_env = root.join("opt/conda-envs/ml");
        std::fs::create_dir_all(conda_env.join("bin")).unwrap();
        std::fs::write(conda_env.join("bin/python"), "").unwrap();
        std::fs::create_dir_all(home.join(".conda")).unwrap();
        std::fs::write(
            home.join(".conda/environments.txt"),
            format!("{}\n", conda_env.display()),
        )
        .unwrap();

        let c = dedup_candidates(candidate_paths_in(Some(&project), &home, &|_| None));
        let kind_of = |p: &Path| c.iter().find(|(q, _)| q == p).map(|(_, k)| k.clone());
        assert_eq!(kind_of(&mine).as_deref(), Some("poetry venv"));
        assert_eq!(kind_of(&pipenv).as_deref(), Some("pipenv venv"));
        assert_eq!(kind_of(&hatch).as_deref(), Some("hatch venv"));
        assert_eq!(
            kind_of(&other).as_deref(),
            Some("poetry"),
            "other projects' envs are listed but not preferred"
        );
        assert_eq!(
            kind_of(&conda_env.join("bin/python")).as_deref(),
            Some("conda")
        );
        // Project envs come before system interpreters and other projects' envs.
        let pos = |p: &Path| c.iter().position(|(q, _)| q == p).unwrap();
        assert!(pos(&mine) < pos(&other));
        if let Some(sys) = c.iter().position(|(_, k)| k == "system") {
            assert!(pos(&mine) < sys);
        }
        let _ = std::fs::remove_dir_all(&root);
    }
    #[cfg(unix)]
    #[test]
    fn finds_conda_installs_outside_home_from_path_activation_and_envs_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let mk = |prefix: PathBuf| {
            std::fs::create_dir_all(prefix.join("bin")).unwrap();
            std::fs::create_dir_all(prefix.join("conda-meta")).unwrap();
            std::fs::write(prefix.join("bin/python"), "").unwrap();
            prefix.join("bin/python")
        };
        let base = dir.path().join("opt/forge");
        let base_py = mk(base.clone());
        std::fs::create_dir_all(base.join("condabin")).unwrap();
        std::fs::write(base.join("condabin/conda"), "").unwrap();
        let named = mk(base.join("envs/ml"));
        let mamba = dir.path().join("micromamba");
        let mamba_env = mk(mamba.join("envs/data"));
        let user_env = mk(home.join(".conda/envs/user"));
        let listed = dir.path().join("custom-envs");
        let listed_env = mk(listed.join("tools"));
        let not_conda = dir.path().join("plain");
        std::fs::create_dir_all(not_conda.join("envs/x/bin")).unwrap();
        std::fs::write(not_conda.join("envs/x/bin/python"), "").unwrap();
        std::fs::create_dir_all(not_conda.join("bin")).unwrap();
        std::fs::write(not_conda.join("bin/conda"), "").unwrap();
        let path = std::env::join_paths([base.join("condabin"), not_conda.join("bin")])
            .unwrap()
            .into_string()
            .unwrap();
        let env = |k: &str| match k {
            "PATH" => Some(path.clone()),
            "MAMBA_ROOT_PREFIX" => Some(mamba.display().to_string()),
            "CONDA_ENVS_PATH" => Some(listed.display().to_string()),
            _ => None,
        };
        let c = dedup_candidates(candidate_paths_in(None, &home, &env));
        let kind_of = |p: &Path| c.iter().find(|(q, _)| q == p).map(|(_, k)| k.clone());
        for python in [&base_py, &named, &mamba_env, &user_env, &listed_env] {
            assert_eq!(kind_of(python).as_deref(), Some("conda"), "{python:?}");
        }
        assert!(
            kind_of(&not_conda.join("envs/x/bin/python")).is_none(),
            "a `conda` file without conda-meta is not an installation root"
        );
        // An activated base found on PATH keeps its conda identity.
        let env = |k: &str| (k == "PATH").then(|| base.join("bin").display().to_string());
        let c = candidate_paths_in(None, &home, &env);
        assert!(c.iter().any(|(p, k)| p == &base_py && k == "conda"));
    }
    #[cfg(unix)]
    #[test]
    fn recorded_external_provider_keeps_symlink_identity_and_falls_back_when_invalid() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let provider = dir.path().join("provider");
        crate::interpreter_provider_fixture::provision(dir.path());
        let recorded = dir.path().join("venv-python");
        symlink(&provider, &recorded).unwrap();
        let candidates = vec![(provider.clone(), "system".to_string())];
        let found = discover_candidates(candidates.clone(), Some(&recorded), probe);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, recorded);
        assert!(found[0].is_venv);
        let actual = discover_interpreters_with_preferred(None, true, Some(&recorded));
        assert_eq!(
            actual[0].path, recorded,
            "saved external path wins outside automatic discovery"
        );
        let stale = dir.path().join("missing");
        assert_eq!(
            discover_candidates(candidates.clone(), Some(&stale), probe)[0].path,
            provider
        );
        let malformed = dir.path().join("malformed");
        assert_eq!(
            discover_candidates(candidates, Some(&malformed), probe)[0].path,
            provider
        );
    }

    #[cfg(unix)]
    #[test]
    fn untrusted_recorded_external_provider_is_never_executed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let provider = dir.path().join("python");
        let marker = dir.path().join("executed");
        std::fs::write(
            &provider,
            format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o755)).unwrap();
        discover_interpreters_with_preferred(Some(dir.path()), false, Some(&provider));
        assert!(!marker.exists());
    }
}

fn sorted_children(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .take(4096)
        .map(|e| e.path())
        .collect();
    paths.sort();
    paths
}
fn python_files(dir: &Path) -> Vec<PathBuf> {
    sorted_children(dir)
        .into_iter()
        .filter(|path| {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let name = name.strip_suffix(".exe").unwrap_or(&name);
            (name == "python"
                || name == "python3"
                || name
                    .strip_prefix("python3.")
                    .is_some_and(|v| !v.is_empty() && v.chars().all(|c| c.is_ascii_digit())))
                && path.is_file()
        })
        .collect()
}
fn additional_local_candidates(
    project: Option<&Path>,
    home: &Path,
    env: &dyn Fn(&str) -> Option<String>,
) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let (bin, exe) = bin_exe();
    let data = env("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    let mut version_stores = vec![
        (
            env("PYENV_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".pyenv"))
                .join("versions"),
            "pyenv",
        ),
        (
            env("ASDF_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".asdf"))
                .join("installs/python"),
            "asdf",
        ),
        (
            env("UV_PYTHON_INSTALL_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| data.join("uv/python")),
            "uv",
        ),
        (
            PathBuf::from("/Library/Frameworks/Python.framework/Versions"),
            "macOS framework",
        ),
        (home.join("Library/Python"), "macOS"),
    ];
    if let Some(local) = env("LOCALAPPDATA") {
        version_stores.push((PathBuf::from(local).join("uv/python"), "uv"));
    }
    for (store, kind) in version_stores {
        for version in sorted_children(&store) {
            for relative in [
                "bin/python",
                "bin/python3",
                "python.exe",
                "install/python.exe",
                "Scripts/python.exe",
            ] {
                out.push((version.join(relative), kind.into()));
            }
            // pyenv virtualenv plugin nests environments beneath each version.
            for venv in sorted_children(&version.join("envs")) {
                out.push((venv.join(bin).join(exe), kind.into()));
            }
        }
    }
    let mut brew = vec![
        PathBuf::from("/opt/homebrew"),
        PathBuf::from("/usr/local"),
        home.join(".linuxbrew"),
    ];
    if let Some(prefix) = env("HOMEBREW_PREFIX") {
        brew.insert(0, prefix.into());
    }
    for prefix in brew {
        for python in python_files(&prefix.join("bin")) {
            out.push((python, "Homebrew".into()));
        }
        for formula in sorted_children(&prefix.join("opt")) {
            if formula
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("python"))
            {
                for python in python_files(&formula.join("bin")) {
                    out.push((python, "Homebrew".into()));
                }
                for python in python_files(&formula.join("libexec/bin")) {
                    out.push((python, "Homebrew".into()));
                }
            }
        }
    }
    for dir in ["/usr/bin", "/usr/local/bin"] {
        for python in python_files(Path::new(dir)) {
            out.push((python, "system".into()));
        }
    }
    let pdm = env("PDM_VENV_IN_PROJECT")
        .filter(|v| v == "true")
        .and(project)
        .map(|p| p.join(".venv"));
    if let Some(dir) = pdm {
        out.push((dir.join(bin).join(exe), "pdm venv".into()));
    }
    let mut stores = vec![
        (home.join(".venvs"), "virtualenv"),
        (data.join("pdm/venvs"), "pdm"),
        (home.join("Library/Application Support/pdm/venvs"), "pdm"),
    ];
    if let Some(path) = env("PDM_VENV_LOCATION") {
        stores.insert(0, (path.into(), "pdm"));
    }
    if let Some(path) = env("POETRY_VIRTUALENVS_PATH") {
        stores.insert(0, (path.into(), "poetry"));
    }
    if let Some(path) = env("POETRY_CACHE_DIR") {
        stores.push((PathBuf::from(path).join("virtualenvs"), "poetry"));
    }
    if let Some(local) = env("LOCALAPPDATA") {
        stores.push((PathBuf::from(local).join("pdm/venvs"), "pdm"));
    }
    for (store, kind) in stores {
        for venv in sorted_children(&store) {
            out.push((venv.join(bin).join(exe), format!("{kind} venv")));
        }
    }
    if cfg!(windows) {
        if let Some(prefix) = env("CONDA_PREFIX") {
            out.push((PathBuf::from(prefix).join("python.exe"), "conda".into()));
        }
    }
    out.retain(|(path, _)| path.is_file());
    out
}

/// Pure parsers make platform discovery testable without platform tools.
pub fn windows_launcher_paths(output: &str) -> Vec<PathBuf> {
    output
        .lines()
        .filter_map(|line| {
            let index = line.find(":\\")?;
            let start = line[..index].char_indices().last()?.0;
            Some(PathBuf::from(line[start..].trim().trim_matches('"')))
        })
        .collect()
}
pub fn windows_registry_paths(output: &str) -> Vec<PathBuf> {
    output
        .lines()
        .filter_map(|line| {
            let (_, value) = line.split_once("REG_SZ")?;
            let path = PathBuf::from(value.trim().trim_matches('"'));
            Some(
                if path
                    .extension()
                    .is_some_and(|e| e.to_string_lossy().eq_ignore_ascii_case("exe"))
                {
                    path
                } else {
                    path.join("python.exe")
                },
            )
        })
        .collect()
}

fn platform_tool_candidates() -> Vec<(PathBuf, String)> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let mut out = Vec::new();
    if let Ok(tool) = which::which("py.exe") {
        if let Some(bytes) = crate::probe::probe_output(&tool, &["-0p"], Duration::from_secs(3)) {
            out.extend(
                windows_launcher_paths(&String::from_utf8_lossy(&bytes))
                    .into_iter()
                    .map(|p| (p, "Windows py launcher".into())),
            );
        }
    }
    if let Ok(tool) = which::which("reg.exe") {
        for hive in [
            "HKCU\\Software\\Python",
            "HKLM\\Software\\Python",
            "HKLM\\Software\\WOW6432Node\\Python",
        ] {
            if let Some(bytes) = crate::probe::probe_output(
                &tool,
                &["query", hive, "/s", "/ve"],
                Duration::from_secs(3),
            ) {
                out.extend(
                    windows_registry_paths(&String::from_utf8_lossy(&bytes))
                        .into_iter()
                        .map(|p| (p, "Windows registry".into())),
                );
            }
        }
    }
    out
}

#[cfg(test)]
mod discovery_provider_tests {
    use super::*;
    #[test]
    fn metadata_retains_architecture_sysconfig_and_reports_invalid_payloads() {
        let payload = br#"{"version":"3.14.1","full_version":"3.14.1 build","prefix":"/env","base_prefix":"/usr","path":["/env/lib"],"platform":"linux","architecture":"aarch64","bits":64,"sysconfig":{"purelib":"/env/lib/site-packages","include":"/env/include"}}"#;
        let metadata = decode_metadata(Path::new("/env/bin/python"), "venv", payload).unwrap();
        assert!(metadata.interpreter.is_venv);
        assert_eq!(metadata.architecture, "aarch64");
        assert_eq!(metadata.bits, 64);
        assert_eq!(
            metadata.sysconfig["purelib"],
            PathBuf::from("/env/lib/site-packages")
        );
        assert!(decode_metadata(Path::new("python"), "system", b"not-json").is_err());
        assert!(
            decode_metadata(Path::new("python"), "system", br#"{"version":"unknown"}"#).is_err()
        );
    }
    #[test]
    fn platform_parsers_preserve_paths_with_spaces() {
        assert_eq!(
            windows_launcher_paths(
                " -V:3.13 * C:\\Program Files\\Python313\\python.exe\r\n -V:3.12 C:\\Python312\\python.exe"
            ),
            vec![
                PathBuf::from("C:\\Program Files\\Python313\\python.exe"),
                PathBuf::from("C:\\Python312\\python.exe")
            ]
        );
        assert_eq!(
            windows_registry_paths("    (Default)    REG_SZ    C:\\Program Files\\Python313\r\n"),
            vec![PathBuf::from(r"C:\Program Files\Python313").join("python.exe")]
        );
    }
    #[test]
    fn configured_local_stores_and_all_path_versions_are_candidates() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let (bin, exe) = bin_exe();
        let named = [
            (
                temp.path().join("asdf/installs/python/3.15/bin/python"),
                "asdf",
            ),
            (temp.path().join("pyenv/versions/3.15/bin/python"), "pyenv"),
            (temp.path().join("uv/cpython-3.15/bin/python3"), "uv"),
            (
                temp.path().join("brew/opt/python@3.15/bin/python3.15"),
                "Homebrew",
            ),
            (
                temp.path().join("pdm/env-a").join(bin).join(exe),
                "pdm venv",
            ),
            (temp.path().join("path/python3.15"), "system"),
        ];
        for (path, _) in &named {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "marker").unwrap();
        }
        let env = |key: &str| {
            match key {
                "ASDF_DATA_DIR" => Some(temp.path().join("asdf")),
                "PYENV_ROOT" => Some(temp.path().join("pyenv")),
                "UV_PYTHON_INSTALL_DIR" => Some(temp.path().join("uv")),
                "HOMEBREW_PREFIX" => Some(temp.path().join("brew")),
                "PDM_VENV_LOCATION" => Some(temp.path().join("pdm")),
                "PATH" => Some(temp.path().join("path")),
                _ => None,
            }
            .map(|p| p.to_string_lossy().into_owned())
        };
        let found = candidate_paths_in(Some(&project), &home, &env);
        for (path, kind) in named {
            assert!(found.contains(&(path, kind.into())), "{found:?}");
        }
    }
}

/// Shared bounded background discovery. Project/window owners hold only their
/// result receiver, so replacing or closing a project cannot apply stale results.
/// Rapid rediscovery requests cannot create an unbounded number of probe threads.
pub fn discovery_receiver(
    root: Option<PathBuf>,
    trusted: bool,
    preferred: Option<PathBuf>,
) -> crossbeam_channel::Receiver<Vec<Interpreter>> {
    struct Request {
        root: Option<PathBuf>,
        trusted: bool,
        preferred: Option<PathBuf>,
        reply: crossbeam_channel::Sender<Vec<Interpreter>>,
    }
    static REQUESTS: std::sync::OnceLock<crossbeam_channel::Sender<Request>> =
        std::sync::OnceLock::new();
    let requests = REQUESTS.get_or_init(|| {
        let (sender, receiver) = crossbeam_channel::bounded::<Request>(8);
        for index in 0..2 {
            let receiver = receiver.clone();
            std::thread::Builder::new()
                .name(format!("interpreter-discovery-{index}"))
                .spawn(move || {
                    while let Ok(request) = receiver.recv() {
                        let found = discover_interpreters_with_preferred(
                            request.root.as_deref(),
                            request.trusted,
                            request.preferred.as_deref(),
                        );
                        let _ = request.reply.send(found);
                    }
                })
                .expect("start interpreter discovery worker");
        }
        sender
    });
    let (reply, receiver) = crossbeam_channel::bounded(1);
    if let Err(error) = requests.try_send(Request {
        root,
        trusted,
        preferred,
        reply,
    }) {
        log::warn!("Interpreter discovery queue is full or unavailable: {error}");
        // The dropped reply sender completes the owner's receive as disconnected.
    }
    receiver
}

#[cfg(all(test, unix))]
mod probe_timeout_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn executable(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Regression: a Windows Store alias run from WSL hung for hours holding an
    /// inherited build lock. A probe must be killed and reaped at its timeout.
    #[test]
    fn hung_interpreter_probe_times_out_and_child_is_reaped() {
        let temp = tempfile::tempdir().unwrap();
        let python = temp.path().join("bin/python");
        let pid_file = temp.path().join("python.pid");
        executable(
            &python,
            &format!(
                "#!/bin/sh\necho $$ > '{0}.tmp'\nmv '{0}.tmp' '{0}'\nexec sleep 600\n",
                pid_file.display()
            ),
        );
        let started = std::time::Instant::now();
        let error = probe_detailed_with_timeout(&python, "system", Duration::from_millis(1500))
            .unwrap_err();
        assert!(error.reason.contains("TimedOut"), "{error:?}");
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "{:?}",
            started.elapsed()
        );
        let pid: i32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // The probe reaped its child: the pid no longer exists at all.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        if alive {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        assert!(!alive, "hung probe child {pid} survived the probe");
    }

    #[test]
    fn windows_store_aliases_are_not_candidates_on_unix() {
        let temp = tempfile::tempdir().unwrap();
        let apps = temp
            .path()
            .join("mnt/c/Users/u/AppData/Local/Microsoft/WindowsApps");
        let alias = apps.join("python.exe");
        executable(&alias, "");
        let real = temp.path().join("usr/bin/python3");
        executable(&real, "#!/bin/sh\n");
        let path = std::env::join_paths([&apps, real.parent().unwrap()])
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let home = temp.path().join("home");
        let found = candidate_paths_in(None, &home, &|key| (key == "PATH").then(|| path.clone()));
        assert!(found.iter().any(|(p, _)| p == &real), "{found:?}");
        assert!(!found.iter().any(|(p, _)| p == &alias), "{found:?}");
        assert!(is_foreign_app_execution_alias(&alias));
        assert!(!is_foreign_app_execution_alias(&real));
    }
}
