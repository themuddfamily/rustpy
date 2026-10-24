//! Interpreter management plans and cancellable environment provisioning.
use crate::Interpreter;
use rustpy_core::CancellationToken;
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Manager {
    #[default]
    Virtualenv,
    Conda,
    Pipenv,
    Poetry,
    Uv,
    Hatch,
    Pdm,
    System,
}
impl Manager {
    pub const ALL: [Self; 8] = [
        Self::Virtualenv,
        Self::Conda,
        Self::Pipenv,
        Self::Poetry,
        Self::Uv,
        Self::Hatch,
        Self::Pdm,
        Self::System,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Virtualenv => "Virtualenv",
            Self::Conda => "Conda",
            Self::Pipenv => "Pipenv",
            Self::Poetry => "Poetry",
            Self::Uv => "uv",
            Self::Hatch => "Hatch",
            Self::Pdm => "PDM",
            Self::System => "System interpreter",
        }
    }
    pub fn executable(self) -> &'static str {
        match self {
            // Windows installs python.exe; python3.exe there is usually the
            // Microsoft Store alias, which opens the Store instead of running.
            Self::Virtualenv | Self::System if cfg!(windows) => "python",
            Self::Virtualenv | Self::System => "python3",
            Self::Conda => "conda",
            Self::Pipenv => "pipenv",
            Self::Poetry => "poetry",
            Self::Uv => "uv",
            Self::Hatch => "hatch",
            Self::Pdm => "pdm",
        }
    }
}
#[derive(Clone, Debug)]
pub struct Request {
    pub root: PathBuf,
    pub manager: Manager,
    pub existing: bool,
    pub executable: PathBuf,
    pub base: PathBuf,
    pub location: PathBuf,
    pub python_version: String,
    pub inherit_site_packages: bool,
    pub install: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandPlan {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}
#[derive(Clone, Debug)]
pub struct Plan {
    pub commands: Vec<CommandPlan>,
    pub interpreter: PathBuf,
    pub locate: Option<CommandPlan>,
}
fn command(program: &Path, args: Vec<String>) -> CommandPlan {
    CommandPlan {
        program: program.into(),
        args,
        env: Vec::new(),
    }
}
pub fn environment_python(location: &Path) -> PathBuf {
    if cfg!(windows) {
        location.join("Scripts").join("python.exe")
    } else {
        location.join("bin").join("python")
    }
}
/// Pure planning makes exact argv, import/sync opt-ins and paths reviewable.
pub fn plan(request: &Request) -> Result<Plan, String> {
    if !request.root.is_absolute() {
        return Err("Project directory must be absolute".into());
    }
    if request.executable.as_os_str().is_empty() {
        return Err("Select the manager executable".into());
    }
    let path = request.location.display().to_string();
    let base = request.base.display().to_string();
    let exe = &request.executable;
    let mut out = Plan {
        commands: Vec::new(),
        interpreter: environment_python(&request.location),
        locate: None,
    };
    if request.manager == Manager::System {
        out.interpreter = request.location.clone();
        return Ok(out);
    }
    if request.existing {
        return Ok(out);
    }
    if !request.location.is_absolute() {
        return Err("Environment location must be absolute".into());
    }
    if matches!(
        request.manager,
        Manager::Virtualenv | Manager::Uv | Manager::Conda
    ) && request.location.exists()
    {
        return Err(
            "Environment location already exists; choose Existing or another location".into(),
        );
    }
    match request.manager {
        Manager::Virtualenv => {
            let mut args = vec!["-m".into(), "venv".into()];
            if request.inherit_site_packages {
                args.push("--system-site-packages".into());
            }
            args.push(path);
            out.commands.push(command(&request.base, args));
        }
        Manager::Uv => {
            let mut args = vec![
                "venv".into(),
                "--seed".into(),
                "--python".into(),
                base.clone(),
            ];
            if request.inherit_site_packages {
                args.push("--system-site-packages".into());
            }
            args.push(path);
            out.commands.push(command(exe, args));
            if request.install && request.root.join("uv.lock").is_file() {
                let mut sync = command(
                    exe,
                    vec![
                        "sync".into(),
                        "--locked".into(),
                        "--python".into(),
                        out.interpreter.display().to_string(),
                    ],
                );
                sync.env.push((
                    "UV_PROJECT_ENVIRONMENT".into(),
                    request.location.display().to_string(),
                ));
                out.commands.push(sync);
            }
        }
        Manager::Conda => {
            if request.python_version.is_empty()
                || !request
                    .python_version
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '.')
            {
                return Err("Enter a Python version such as 3.12".into());
            }
            if request.install && request.root.join("environment.yml").is_file() {
                out.commands.push(command(
                    exe,
                    vec![
                        "env".into(),
                        "create".into(),
                        "--yes".into(),
                        "--prefix".into(),
                        path,
                        "--file".into(),
                        request.root.join("environment.yml").display().to_string(),
                    ],
                ));
            } else {
                out.commands.push(command(
                    exe,
                    vec![
                        "create".into(),
                        "--yes".into(),
                        "--prefix".into(),
                        path,
                        format!("python={}", request.python_version),
                    ],
                ));
            }
        }
        Manager::Pipenv => {
            if !request.root.join("Pipfile").is_file() {
                return Err("Pipenv requires a Pipfile in the project".into());
            }
            let mut create = command(exe, vec!["--python".into(), base.clone()]);
            create
                .env
                .push(("PIPENV_VENV_IN_PROJECT".into(), "1".into()));
            create
                .env
                .push(("PIPENV_IGNORE_VIRTUALENVS".into(), "1".into()));
            out.commands.push(create);
            if request.install {
                let mut install = command(
                    exe,
                    vec![if request.root.join("Pipfile.lock").is_file() {
                        "sync"
                    } else {
                        "install"
                    }
                    .into()],
                );
                install
                    .env
                    .push(("PIPENV_VENV_IN_PROJECT".into(), "1".into()));
                out.commands.push(install);
            }
            let mut locate = command(exe, vec!["--venv".into()]);
            locate
                .env
                .push(("PIPENV_VENV_IN_PROJECT".into(), "1".into()));
            out.locate = Some(locate);
        }
        Manager::Poetry => {
            if !request.root.join("pyproject.toml").is_file() {
                return Err("Poetry requires pyproject.toml".into());
            }
            out.commands
                .push(command(exe, vec!["env".into(), "use".into(), base.clone()]));
            if request.install {
                out.commands.push(command(
                    exe,
                    vec!["install".into(), "--no-interaction".into()],
                ));
            }
            out.locate = Some(command(
                exe,
                vec!["env".into(), "info".into(), "--path".into()],
            ));
        }
        Manager::Hatch => {
            if !request.root.join("pyproject.toml").is_file() {
                return Err("Hatch requires pyproject.toml".into());
            }
            let mut create = command(exe, vec!["env".into(), "create".into()]);
            create.env.push(("HATCH_PYTHON".into(), base.clone()));
            out.commands.push(create);
            out.locate = Some(command(exe, vec!["env".into(), "find".into()]));
        }
        Manager::Pdm => {
            if !request.root.join("pyproject.toml").is_file() {
                return Err("PDM requires pyproject.toml".into());
            }
            out.commands
                .push(command(exe, vec!["use".into(), "-f".into(), base]));
            if request.install {
                out.commands
                    .push(command(exe, vec!["install".into(), "--no-self".into()]));
            }
            out.locate = Some(command(exe, vec!["info".into(), "--python".into()]));
        }
        Manager::System => unreachable!(),
    }
    if request.install && matches!(request.manager, Manager::Virtualenv | Manager::Uv) {
        if request.root.join("requirements.txt").is_file() {
            out.commands.push(command(
                &out.interpreter,
                vec![
                    "-m".into(),
                    "pip".into(),
                    "install".into(),
                    "-r".into(),
                    request.root.join("requirements.txt").display().to_string(),
                ],
            ));
        } else if request.root.join("pyproject.toml").is_file()
            && !(request.manager == Manager::Uv && request.root.join("uv.lock").is_file())
        {
            out.commands.push(command(
                &out.interpreter,
                vec![
                    "-m".into(),
                    "pip".into(),
                    "install".into(),
                    "-e".into(),
                    request.root.display().to_string(),
                ],
            ));
        }
    }
    Ok(out)
}
/// A failed manager step reports the tool's own diagnostic (its last stderr
/// lines, else stdout) so the dialog shows e.g. Conda's "PackagesNotFound".
fn failure_message(
    program: &Path,
    status: std::process::ExitStatus,
    stdout: &[u8],
    stderr: &[u8],
) -> String {
    let name = program
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.display().to_string());
    let pick = |bytes: &[u8]| {
        let text = String::from_utf8_lossy(bytes);
        let lines: Vec<&str> = text
            .lines()
            .map(str::trim_end)
            .filter(|l| !l.trim().is_empty())
            .collect();
        lines[lines.len().saturating_sub(8)..].join("\n")
    };
    let detail = Some(pick(stderr))
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| pick(stdout));
    if detail.is_empty() {
        format!("{name} failed: {status}")
    } else {
        format!("{name} failed ({status}):\n{detail}")
    }
}
#[derive(Clone, Debug)]
pub enum Event {
    Progress(String),
    Finished(Result<Interpreter, String>),
}
pub struct Task {
    owner: u64,
    token: CancellationToken,
    rx: crossbeam_channel::Receiver<Event>,
}
impl Task {
    pub fn start(request: Request, trusted: bool) -> Result<Self, String> {
        if !trusted {
            return Err("Trust the project before configuring an environment".into());
        }
        let plan = plan(&request)?;
        let token = CancellationToken::new();
        let worker_token = token.clone();
        let (tx, rx) = crossbeam_channel::unbounded();
        std::thread::Builder::new()
            .name("rustpy-environment".into())
            .spawn(move || {
                let result = (|| {
                    let run = |step: &CommandPlan| -> Result<Vec<u8>, String> {
                        worker_token.check().map_err(|e| e.to_string())?;
                        let mut cmd = crate::interpreter::interpreter_command(&step.program);
                        cmd.args(&step.args)
                            // A parent terminal's active environment must not
                            // redirect Poetry/Pipenv/PDM into the IDE's own SDK.
                            .env_remove("VIRTUAL_ENV")
                            .env_remove("CONDA_PREFIX")
                            .env_remove("CONDA_DEFAULT_ENV")
                            .envs(step.env.iter().cloned())
                            .current_dir(&request.root);
                        let (status, stdout, stderr) =
                            crate::native_skeleton::supervise_tool_status(
                                &mut cmd,
                                &worker_token,
                                Duration::from_secs(600),
                                4 * 1024 * 1024,
                            )
                            .map_err(|e| e.to_string())?;
                        if !status.success() {
                            return Err(failure_message(&step.program, status, &stdout, &stderr));
                        }
                        Ok(stdout)
                    };
                    for (index, step) in plan.commands.iter().enumerate() {
                        let _ = tx.send(Event::Progress(format!(
                            "{}: step {} / {}",
                            request.manager.name(),
                            index + 1,
                            plan.commands.len()
                        )));
                        run(step)?;
                    }
                    let python = if let Some(locate) = plan.locate {
                        let output = run(&locate)?;
                        let value = String::from_utf8(output)
                            .map_err(|_| "Manager returned a non-UTF8 path")?;
                        // Managers may print notices before the path; the
                        // path is the last non-empty line.
                        let located = PathBuf::from(
                            value
                                .lines()
                                .map(str::trim)
                                .rfind(|line| !line.is_empty())
                                .unwrap_or_default(),
                        );
                        if !located.is_absolute() {
                            return Err(
                                "Manager did not return an absolute environment path".into()
                            );
                        }
                        if request.manager == Manager::Pdm {
                            located
                        } else {
                            environment_python(&located)
                        }
                    } else {
                        plan.interpreter
                    };
                    worker_token.check().map_err(|e| e.to_string())?;
                    let interpreter = crate::interpreter::probe(&python, request.manager.name())
                        .ok_or_else(|| {
                            format!("{} is not a working Python interpreter", python.display())
                        })?;
                    worker_token.check().map_err(|e| e.to_string())?;
                    Ok(interpreter)
                })();
                let _ = tx.send(Event::Finished(result));
            })
            .map_err(|e| e.to_string())?;
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Ok(Self {
            owner: NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            token,
            rx,
        })
    }
    pub fn poll(&self) -> Option<Event> {
        match self.rx.try_recv() {
            Ok(event) => Some(event),
            Err(crossbeam_channel::TryRecvError::Empty) => None,
            Err(crossbeam_channel::TryRecvError::Disconnected) => Some(Event::Finished(Err(
                "Environment worker stopped unexpectedly".into(),
            ))),
        }
    }
    pub fn quit_guard_state(&self) -> String {
        format!("environment:{}:{}", self.owner, self.token.is_cancelled())
    }
    pub fn cancel(&self) {
        self.token.cancel();
    }
}
impl Drop for Task {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub interpreter: Interpreter,
    pub name: String,
    pub shared: bool,
    pub projects: Vec<PathBuf>,
    pub paths: Vec<PathBuf>,
    pub removed_paths: Vec<PathBuf>,
}
pub fn registry_path() -> PathBuf {
    rustpy_settings::Settings::config_dir().join("interpreter-registry.json")
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Registry {
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub hidden_paths: Vec<PathBuf>,
}
impl Registry {
    pub fn load(path: &Path) -> Self {
        std::fs::metadata(path)
            .ok()
            .filter(|m| m.len() <= 4 * 1024 * 1024)
            .and_then(|_| std::fs::read(path).ok())
            .filter(|b| b.len() <= 4 * 1024 * 1024)
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let parent = path.parent().ok_or("Registry needs a parent directory")?;
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        use std::io::Write;
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let temporary = parent.join(format!(".interpreters-{}-{serial}.tmp", std::process::id()));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(&temporary, path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result.map_err(|e: std::io::Error| e.to_string())?;
        Ok(())
    }
    pub fn remember(&mut self, interpreter: Interpreter, root: &Path, name: &str, shared: bool) {
        self.hidden_paths.retain(|path| path != &interpreter.path);
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|e| e.interpreter.path == interpreter.path)
        {
            entry.interpreter = interpreter;
            if !name.trim().is_empty() {
                entry.name = name.trim().into();
            }
            entry.shared |= shared;
            if !entry.projects.iter().any(|p| p == root) {
                entry.projects.push(root.into());
            }
        } else {
            self.entries.push(Entry {
                name: if name.trim().is_empty() {
                    interpreter.label()
                } else {
                    name.trim().into()
                },
                interpreter,
                shared,
                projects: vec![root.into()],
                paths: Vec::new(),
                removed_paths: Vec::new(),
            });
        }
    }
    pub fn apply_paths(&self, interpreter: &mut Interpreter) {
        if let Some(entry) = self
            .entries
            .iter()
            .find(|e| e.interpreter.path == interpreter.path)
        {
            interpreter
                .sys_path
                .retain(|p| !entry.removed_paths.contains(p));
            for path in &entry.paths {
                if !interpreter.sys_path.contains(path) {
                    interpreter.sys_path.push(path.clone());
                }
            }
        }
    }
}
/// Choose a manifest without executing tools or importing project code.
pub fn suggested_manager(root: &Path) -> Option<(Manager, &'static str)> {
    [
        ("environment.yml", Manager::Conda),
        ("Pipfile", Manager::Pipenv),
        ("poetry.lock", Manager::Poetry),
        ("uv.lock", Manager::Uv),
        ("pyproject.toml", Manager::Virtualenv),
        ("requirements.txt", Manager::Virtualenv),
    ]
    .into_iter()
    .find(|(file, _)| root.join(file).is_file())
    .map(|(file, manager)| (manager, file))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(root: &Path, manager: Manager) -> Request {
        Request {
            root: root.into(),
            manager,
            existing: false,
            executable: "/tools/manager".into(),
            base: "/base/python".into(),
            location: root.join("custom env"),
            python_version: "3.12".into(),
            inherit_site_packages: true,
            install: false,
        }
    }
    #[test]
    #[cfg(unix)] // the fixture roots are Unix absolute paths
    fn virtualenv_and_uv_preserve_argument_boundaries() {
        let root = Path::new("/project space");
        let p = plan(&request(root, Manager::Virtualenv)).unwrap();
        assert_eq!(
            p.commands[0].args,
            vec![
                "-m",
                "venv",
                "--system-site-packages",
                "/project space/custom env"
            ]
        );
        let p = plan(&request(root, Manager::Uv)).unwrap();
        assert_eq!(
            p.commands[0].args.last().unwrap(),
            "/project space/custom env"
        );
        assert!(p.commands[0]
            .args
            .contains(&"--system-site-packages".into()));
    }
    #[cfg(unix)]
    #[test]
    fn failed_manager_step_reports_the_tools_own_diagnostic() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let tool = root.path().join("conda");
        std::fs::write(
            &tool,
            "#!/bin/sh\necho 'Collecting package metadata'\necho 'PackagesNotFoundError: python=9.9' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut r = request(root.path(), Manager::Conda);
        r.executable = tool;
        r.python_version = "9.9".into();
        let task = Task::start(r, true).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let error = loop {
            if let Some(Event::Finished(result)) = task.poll() {
                break result.unwrap_err();
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(error.starts_with("conda failed"), "{error}");
        assert!(
            error.contains("PackagesNotFoundError: python=9.9"),
            "{error}"
        );
    }
    #[test]
    #[cfg(unix)] // the fixture roots are Unix absolute paths
    fn existing_and_system_never_install_or_create() {
        let mut request = request(Path::new("/project"), Manager::Virtualenv);
        request.existing = true;
        request.install = true;
        assert!(plan(&request).unwrap().commands.is_empty());
        request.manager = Manager::System;
        request.location = "/custom/python".into();
        assert_eq!(
            plan(&request).unwrap().interpreter,
            Path::new("/custom/python")
        );
    }
    #[test]
    fn manifests_require_explicit_install_and_manager_metadata() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("pyproject.toml"),
            "[project]\nname='example'",
        )
        .unwrap();
        for manager in [Manager::Poetry, Manager::Hatch, Manager::Pdm] {
            let no_install = plan(&request(root.path(), manager)).unwrap();
            assert!(no_install.locate.is_some());
            assert_eq!(no_install.commands.len(), 1);
        }
        assert!(plan(&request(root.path(), Manager::Pipenv)).is_err());
        let mut r = request(root.path(), Manager::Virtualenv);
        r.install = true;
        assert_eq!(plan(&r).unwrap().commands.len(), 2);
    }
    #[test]
    fn registry_keeps_alias_associations_and_path_overrides_across_reload() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("registry.json");
        let interpreter = Interpreter {
            path: "/python".into(),
            version: "3.12.1".into(),
            prefix: "/prefix".into(),
            sys_path: vec!["/library".into()],
            is_venv: false,
            platform: "linux".into(),
            kind: "system".into(),
        };
        let mut registry = Registry::default();
        registry.remember(interpreter.clone(), root.path(), "Named SDK", true);
        registry.entries[0].paths.push("/custom".into());
        registry.entries[0].removed_paths.push("/library".into());
        registry.save(&path).unwrap();
        let mut loaded = Registry::load(&path);
        loaded.remember(interpreter.clone(), Path::new("/second-project"), "", false);
        assert_eq!(loaded.entries[0].name, "Named SDK");
        assert!(loaded.entries[0].shared);
        assert_eq!(loaded.entries[0].projects.len(), 2);
        let mut refreshed = interpreter;
        loaded.apply_paths(&mut refreshed);
        assert_eq!(refreshed.sys_path, vec![PathBuf::from("/custom")]);
    }
}
