//! Project manifests, explicit manager command plans, repository preferences,
//! and lock-change detection. Parsing/planning performs no interpreter work.
use crate::{InstalledPackage, PipTask, PipTaskKind, Requirement};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Manager {
    #[default]
    Auto,
    Pip,
    Uv,
    Poetry,
    Pipenv,
    Conda,
    Hatch,
    Pdm,
}
impl Manager {
    pub const ALL: [Self; 8] = [
        Self::Auto,
        Self::Pip,
        Self::Uv,
        Self::Poetry,
        Self::Pipenv,
        Self::Conda,
        Self::Hatch,
        Self::Pdm,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Pip => "pip",
            Self::Uv => "uv",
            Self::Poetry => "Poetry",
            Self::Pipenv => "Pipenv",
            Self::Conda => "Conda",
            Self::Hatch => "Hatch",
            Self::Pdm => "PDM",
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Repository {
    pub url: String,
    pub trusted_host: String,
    pub username: String,
    pub password_environment: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub manager: Manager,
    pub executables: BTreeMap<String, PathBuf>,
    pub repositories: Vec<Repository>,
    pub conda_channels: Vec<String>,
    pub prereleases: bool,
    pub user: bool,
    pub options: Vec<String>,
}
impl Preferences {
    pub fn load(root: &Path) -> Self {
        std::fs::read(root.join(".rustpy/packages.json"))
            .ok()
            .and_then(|s| serde_json::from_slice(&s).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, root: &Path) -> Result<()> {
        std::fs::create_dir_all(root.join(".rustpy"))?;
        std::fs::write(
            root.join(".rustpy/packages.json"),
            serde_json::to_vec_pretty(self)?,
        )?;
        Ok(())
    }
}
#[derive(Clone, Debug, Default)]
pub struct Manifest {
    pub manager: Manager,
    pub dependencies: Vec<Requirement>,
    pub sources: Vec<PathBuf>,
    pub warnings: Vec<String>,
}
fn add_dependency(out: &mut Manifest, text: &str) {
    if let Some(req) = crate::requirements::parse_requirement(text, 1) {
        if !out.dependencies.iter().any(|r| r.raw == req.raw) {
            out.dependencies.push(req);
        }
    }
}
fn dependency_table(out: &mut Manifest, table: &toml::Table) {
    for (name, value) in table {
        if name == "python" {
            continue;
        }
        let version = value
            .as_str()
            .or_else(|| value.get("version").and_then(toml::Value::as_str))
            .unwrap_or("");
        let spec = if version == "*" {
            "".to_string()
        } else if let Some(v) = version.strip_prefix('^') {
            let parts: Vec<_> = v.split('.').collect();
            let major = parts
                .first()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            if major > 0 || parts.len() == 1 {
                format!(">={v},<{}", major + 1)
            } else {
                let minor = parts
                    .get(1)
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                if minor > 0 || parts.len() == 2 {
                    format!(">={v},<0.{}", minor + 1)
                } else {
                    let patch = parts
                        .get(2)
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(0);
                    format!(">={v},<0.0.{}", patch + 1)
                }
            }
        } else if version.starts_with('~') && !version.starts_with("~=") {
            let v = &version[1..];
            let parts: Vec<_> = v.split('.').collect();
            let major = parts[0].parse::<u64>().unwrap_or(0);
            if parts.len() == 1 {
                format!(">={v},<{}", major + 1)
            } else {
                let minor = parts[1].parse::<u64>().unwrap_or(0);
                format!(">={v},<{major}.{}", minor + 1)
            }
        } else if version.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            format!("=={version}")
        } else {
            version.into()
        };
        let extras = value
            .get("extras")
            .and_then(toml::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .filter(|s| !s.is_empty())
            .map(|s| format!("[{s}]"))
            .unwrap_or_default();
        let spec = value
            .get("url")
            .and_then(toml::Value::as_str)
            .map(|url| format!(" @ {url}"))
            .unwrap_or(spec);
        let marker = value
            .get("markers")
            .and_then(toml::Value::as_str)
            .map(|s| format!(" ; {s}"))
            .unwrap_or_default();
        add_dependency(out, &format!("{name}{extras}{spec}{marker}"));
    }
}
pub fn read_manifest(root: &Path) -> Manifest {
    let mut out = Manifest::default();
    let pyproject = root.join("pyproject.toml");
    if let Ok(source) = std::fs::read_to_string(&pyproject) {
        out.sources.push(pyproject);
        match toml::from_str::<toml::Value>(&source) {
            Ok(value) => {
                if let Some(array) = value
                    .get("project")
                    .and_then(|v| v.get("dependencies"))
                    .and_then(toml::Value::as_array)
                {
                    for item in array.iter().filter_map(toml::Value::as_str) {
                        add_dependency(&mut out, item);
                    }
                }
                if let Some(table) = value
                    .get("tool")
                    .and_then(|v| v.get("poetry"))
                    .and_then(|v| v.get("dependencies"))
                    .and_then(toml::Value::as_table)
                {
                    out.manager = Manager::Poetry;
                    dependency_table(&mut out, table);
                }
                if value.get("tool").and_then(|v| v.get("hatch")).is_some() {
                    out.manager = Manager::Hatch;
                }
                if value.get("tool").and_then(|v| v.get("pdm")).is_some() {
                    out.manager = Manager::Pdm;
                }
                if value.get("tool").and_then(|v| v.get("uv")).is_some() {
                    out.manager = Manager::Uv;
                }
            }
            Err(error) => out.warnings.push(format!("pyproject.toml: {error}")),
        }
    }
    let pipfile = root.join("Pipfile");
    if let Ok(source) = std::fs::read_to_string(&pipfile) {
        out.sources.push(pipfile);
        out.manager = Manager::Pipenv;
        match toml::from_str::<toml::Value>(&source) {
            Ok(value) => {
                if let Some(table) = value.get("packages").and_then(toml::Value::as_table) {
                    dependency_table(&mut out, table);
                }
            }
            Err(error) => out.warnings.push(format!("Pipfile: {error}")),
        }
    }
    for (name, manager) in [
        ("poetry.lock", Manager::Poetry),
        ("Pipfile.lock", Manager::Pipenv),
        ("pdm.lock", Manager::Pdm),
        ("uv.lock", Manager::Uv),
        ("environment.yml", Manager::Conda),
        ("environment.yaml", Manager::Conda),
    ] {
        let path = root.join(name);
        if path.is_file() {
            out.sources.push(path);
            out.manager = manager;
        }
    }
    let requirements = root.join("requirements.txt");
    if let Ok(text) = std::fs::read_to_string(&requirements) {
        out.sources.push(requirements);
        for req in crate::parse_requirements(&text) {
            if !out.dependencies.iter().any(|r| r.raw == req.raw) {
                out.dependencies.push(req);
            }
        }
    }
    out
}
#[derive(Clone, Debug)]
pub enum Operation {
    Install(Vec<String>),
    Uninstall(Vec<String>),
    Upgrade(Vec<String>),
    InstallFile(PathBuf),
    Sync,
    Build,
    Setup(String),
    CreateSetup,
}
#[derive(Clone, Debug)]
pub struct CommandPlan {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub label: String,
}
impl CommandPlan {
    pub fn start(self) -> Result<PipTask> {
        let mut command = rustpy_core::interpreter_launch::interpreter_command(&self.program);
        command
            .args(&self.args)
            .current_dir(&self.cwd)
            .envs(self.env);
        let line = format!("{} {}", self.program.display(), self.args.join(" "));
        PipTask::spawn_command(PipTaskKind::Command { label: self.label }, command, line)
    }
}
fn prefix(interpreter: &Path) -> PathBuf {
    let parent = interpreter.parent().unwrap_or(Path::new("."));
    if parent
        .file_name()
        .is_some_and(|n| n == "bin" || n == "Scripts")
    {
        parent.parent().unwrap_or(parent).into()
    } else {
        parent.into()
    }
}
fn encode_url_part(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
pub fn effective_manager(root: &Path, interpreter: &Path, prefs: &Preferences) -> Manager {
    if prefs.manager != Manager::Auto {
        return prefs.manager;
    }
    let manifest = read_manifest(root);
    if manifest.manager != Manager::Auto {
        return manifest.manager;
    }
    match crate::package_manager_cached(interpreter) {
        Some(crate::PackageManager::Uv { .. }) => Manager::Uv,
        _ => Manager::Pip,
    }
}
pub fn command_plan(
    root: &Path,
    interpreter: &Path,
    prefs: &Preferences,
    operation: &Operation,
) -> Result<CommandPlan> {
    let mut manager = effective_manager(root, interpreter, prefs);
    // A pip requirements file is not itself a Poetry/PDM dependency artifact.
    // Install it into the explicitly selected interpreter without rewriting the
    // project's pyproject dependencies as a path dependency on a text file.
    if let Operation::InstallFile(path) = operation {
        if path.extension().is_some_and(|e| e == "txt" || e == "in")
            && !matches!(
                manager,
                Manager::Pip | Manager::Uv | Manager::Conda | Manager::Pipenv
            )
        {
            manager = Manager::Pip;
        }
    }
    if matches!(operation, Operation::CreateSetup) {
        bail!("Create setup.py is a file creation operation")
    }
    let mut plan = CommandPlan {
        program: interpreter.into(),
        args: Vec::new(),
        cwd: root.into(),
        env: Vec::new(),
        label: format!("{} package operation", manager.label()),
    };
    let exe = |name: &str| {
        prefs
            .executables
            .get(name)
            .cloned()
            .unwrap_or_else(|| PathBuf::from(name))
    };
    if matches!(operation, Operation::Build) {
        plan.args = vec!["-u".into(), "-m".into(), "build".into()];
        plan.label = "Build Python distribution".into();
        return Ok(plan);
    }
    if let Operation::Setup(task) = operation {
        anyhow::ensure!(
            SETUP_COMMANDS.contains(&task.as_str()),
            "Unknown setup.py task"
        );
        anyhow::ensure!(root.join("setup.py").is_file(), "No setup.py in project");
        plan.args = vec!["-u".into(), "setup.py".into(), task.clone()];
        plan.label = format!("setup.py {task}");
        return Ok(plan);
    }
    let (verb, specs) = match operation {
        Operation::Install(s) => ("install", s.clone()),
        Operation::Uninstall(s) => ("uninstall", s.clone()),
        Operation::Upgrade(s) => ("upgrade", s.clone()),
        Operation::InstallFile(p) => ("file", vec![p.to_string_lossy().into_owned()]),
        Operation::Sync => ("sync", Vec::new()),
        _ => unreachable!(),
    };
    anyhow::ensure!(
        specs
            .iter()
            .all(|s| !s.trim().is_empty() && !s.starts_with('-') && !s.contains('\n')),
        "Invalid package specification"
    );
    match manager {
        Manager::Pip | Manager::Auto | Manager::Uv | Manager::Hatch => {
            if manager == Manager::Uv {
                plan.program = exe("uv");
                plan.args = vec!["pip".into()];
            } else if manager == Manager::Hatch {
                plan.program = exe("hatch");
                plan.args = vec!["run".into(), "python".into(), "-m".into(), "pip".into()];
            } else {
                plan.args = vec!["-u".into(), "-m".into(), "pip".into()];
            }
            plan.args.push(
                if verb == "uninstall" {
                    "uninstall"
                } else {
                    "install"
                }
                .into(),
            );
            if manager == Manager::Uv {
                plan.args
                    .extend(["--python".into(), interpreter.display().to_string()]);
            } else {
                plan.args.push("--no-input".into());
            }
            if verb == "uninstall" && manager != Manager::Uv {
                plan.args.push("-y".into());
            }
            if verb == "upgrade" {
                plan.args.push("--upgrade".into());
            }
            if prefs.prereleases && matches!(verb, "install" | "file" | "upgrade") {
                plan.args.push("--pre".into());
            }
            if prefs.user && verb != "uninstall" {
                anyhow::ensure!(
                    manager != Manager::Uv,
                    "uv does not support --user installs"
                );
                plan.args.push("--user".into());
            }
            if verb == "file" {
                let file = Path::new(&specs[0]);
                if file.extension().is_some_and(|e| e == "txt" || e == "in") {
                    plan.args.push("-r".into());
                }
                plan.args.extend(specs);
            } else if verb == "sync" {
                if manager == Manager::Uv && root.join("uv.lock").is_file() {
                    plan.args = vec![
                        "sync".into(),
                        "--active".into(),
                        "--frozen".into(),
                        "--python".into(),
                        interpreter.display().to_string(),
                    ];
                    plan.env.push((
                        "VIRTUAL_ENV".into(),
                        prefix(interpreter).display().to_string(),
                    ));
                } else if root.join("requirements.txt").is_file() {
                    plan.args.extend([
                        "-r".into(),
                        root.join("requirements.txt").display().to_string(),
                    ]);
                } else {
                    plan.args.push(".".into());
                }
            } else {
                plan.args.extend(specs);
            }
        }
        Manager::Poetry => {
            plan.program = exe("poetry");
            plan.args = vec![match verb {
                "install" | "file" => "add",
                "uninstall" => "remove",
                "upgrade" => "update",
                _ => "install",
            }
            .into()];
            if prefs.prereleases && matches!(verb, "install" | "file") {
                plan.args.push("--allow-prereleases".into());
            }
            plan.args.extend(specs);
            plan.env
                .push(("POETRY_VIRTUALENVS_CREATE".into(), "false".into()));
            plan.env.push((
                "VIRTUAL_ENV".into(),
                prefix(interpreter).display().to_string(),
            ));
        }
        Manager::Pipenv => {
            plan.program = exe("pipenv");
            plan.args = vec![match verb {
                "uninstall" => "uninstall",
                "upgrade" => "update",
                "sync" => "sync",
                _ => "install",
            }
            .into()];
            if verb == "file"
                && Path::new(&specs[0])
                    .extension()
                    .is_some_and(|e| e == "txt" || e == "in")
            {
                plan.args.push("-r".into());
            }
            plan.args.extend(specs);
            if prefs.prereleases && matches!(verb, "install" | "file" | "upgrade") {
                plan.args.push("--pre".into());
            }
            plan.env
                .push(("PIPENV_PYTHON".into(), interpreter.display().to_string()));
        }
        Manager::Conda => {
            plan.program = exe("conda");
            plan.args = vec![if verb == "uninstall" {
                "remove"
            } else if verb == "upgrade" {
                "update"
            } else if verb == "sync" {
                "env"
            } else {
                "install"
            }
            .into()];
            if verb == "sync" {
                plan.args.extend([
                    "update".into(),
                    "--file".into(),
                    if root.join("environment.yml").is_file() {
                        "environment.yml"
                    } else {
                        "environment.yaml"
                    }
                    .into(),
                ]);
            } else {
                plan.args.push("--yes".into());
            }
            plan.args
                .extend(["--prefix".into(), prefix(interpreter).display().to_string()]);
            if verb == "file" {
                plan.args.push("--file".into());
            }
            for channel in &prefs.conda_channels {
                plan.args.extend(["-c".into(), channel.clone()]);
            }
            plan.args.extend(specs);
        }
        Manager::Pdm => {
            plan.program = exe("pdm");
            plan.args = vec![match verb {
                "install" | "file" => "add",
                "uninstall" => "remove",
                "upgrade" => "update",
                _ => "sync",
            }
            .into()];
            if prefs.prereleases && matches!(verb, "install" | "file" | "upgrade") {
                plan.args.push("--pre".into());
            }
            plan.args.extend(specs);
            plan.env
                .push(("PDM_PYTHON".into(), interpreter.display().to_string()));
        }
    }
    if !prefs.options.is_empty() {
        plan.args.extend(prefs.options.iter().cloned());
    }
    let mut urls = Vec::new();
    let mut trusted = Vec::new();
    for repository in &prefs.repositories {
        anyhow::ensure!(
            (repository.url.starts_with("https://") || repository.url.starts_with("http://"))
                && !repository.url.chars().any(char::is_whitespace)
                && !repository.url.contains('@'),
            "Repository URL must be HTTP(S) without embedded credentials"
        );
        let mut url = repository.url.clone();
        if !repository.username.is_empty() {
            let secret = if repository.password_environment.is_empty() {
                String::new()
            } else {
                std::env::var(&repository.password_environment).with_context(|| {
                    format!(
                        "Credential environment variable {} is unset",
                        repository.password_environment
                    )
                })?
            };
            let (scheme, tail) = url.split_once("://").unwrap();
            url = format!(
                "{scheme}://{}:{}@{tail}",
                encode_url_part(&repository.username),
                encode_url_part(&secret)
            );
        }
        urls.push(url);
        if !repository.trusted_host.trim().is_empty() {
            trusted.push(repository.trusted_host.trim().to_string());
        }
    }
    if !urls.is_empty() {
        let joined = urls.join(" ");
        plan.env.extend([
            ("PIP_EXTRA_INDEX_URL".into(), joined.clone()),
            ("UV_EXTRA_INDEX_URL".into(), joined),
        ]);
    }
    if !trusted.is_empty() {
        plan.env
            .push(("PIP_TRUSTED_HOST".into(), trusted.join(" ")));
    }
    Ok(plan)
}
pub const SETUP_COMMANDS: &[&str] = &[
    "build",
    "build_ext",
    "build_py",
    "sdist",
    "bdist_wheel",
    "egg_info",
    "check",
    "clean",
    "install",
    "develop",
];
pub fn create_setup(root: &Path) -> Result<PathBuf> {
    let path = root.join("setup.py");
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    file.write_all(b"from setuptools import setup, find_packages\n\nsetup(name=\"my-project\", version=\"0.1.0\", packages=find_packages())\n")?;
    Ok(path)
}
#[derive(Default)]
pub struct LockWatcher {
    root: Option<PathBuf>,
    fingerprints: HashMap<PathBuf, u64>,
}
impl LockWatcher {
    pub fn poll(&mut self, root: &Path) -> Vec<PathBuf> {
        use std::hash::{Hash, Hasher};
        let mut now = HashMap::new();
        for name in [
            "pyproject.toml",
            "Pipfile",
            "Pipfile.lock",
            "poetry.lock",
            "uv.lock",
            "pdm.lock",
            "requirements.txt",
            "environment.yml",
            "environment.yaml",
        ] {
            let path = root.join(name);
            if let Ok(bytes) = std::fs::read(&path) {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                bytes.hash(&mut hasher);
                now.insert(path, hasher.finish());
            }
        }
        let changed = if self.root.as_deref() == Some(root) {
            now.iter()
                .filter(|(p, h)| self.fingerprints.get(*p) != Some(*h))
                .map(|(p, _)| p.clone())
                .collect()
        } else {
            Vec::new()
        };
        self.root = Some(root.into());
        self.fingerprints = now;
        changed
    }
}
pub fn missing_dependencies(
    root: &Path,
    installed: &[InstalledPackage],
) -> Vec<(Requirement, Option<String>)> {
    crate::unsatisfied(&read_manifest(root).dependencies, installed)
}

/// Conda's complete catalog, including packages without Python dist-info.
pub fn conda_installed(
    interpreter: &Path,
    preferences: &Preferences,
) -> Result<Vec<InstalledPackage>> {
    let executable = preferences
        .executables
        .get("conda")
        .cloned()
        .unwrap_or_else(|| "conda".into());
    let output = std::process::Command::new(executable)
        .args(["list", "--json", "--prefix"])
        .arg(prefix(interpreter))
        .envs(rustpy_net::proxy_env_vars())
        .stdin(std::process::Stdio::null())
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "Conda package listing failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    #[derive(Deserialize)]
    struct Entry {
        name: String,
        version: String,
    }
    let entries: Vec<Entry> = serde_json::from_slice(&output.stdout)?;
    let mut packages: Vec<_> = entries
        .into_iter()
        .map(|entry| InstalledPackage {
            name: entry.name,
            version: entry.version,
            location: Some(prefix(interpreter)),
            editable: false,
            summary: None,
        })
        .collect();
    packages.sort_by_key(|package| crate::normalize_name(&package.name));
    Ok(packages)
}
