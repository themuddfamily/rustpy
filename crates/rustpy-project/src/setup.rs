//! Environment setup planning, commands, scaffolds and step progression.

use rustpy_core::CancellationToken;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

/// Tool that creates the project environment.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EnvTool {
    /// `python -m venv`.
    Venv,
    /// `uv venv --seed` (pip included, so requirements install the same way).
    Uv,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Command {
        label: String,
        program: PathBuf,
        args: Vec<String>,
        cwd: PathBuf,
    },
    CreateEnv {
        tool: EnvTool,
        inherit_site_packages: bool,
    },
    InstallRequirements,
    /// `python -m django startproject <name> .` (a minimal skeleton when that fails).
    DjangoProject {
        name: String,
    },
    SelectEnv,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ProjectKind {
    Pure,
    Flask,
    FastApi,
    Django,
    Empty,
    Pyramid,
    AppEngine,
    Web2py,
    Scientific,
    Angular,
    React,
    Vue,
    Html5,
}
impl ProjectKind {
    pub fn all() -> &'static [Self] {
        &[
            Self::Pure,
            Self::Empty,
            Self::Django,
            Self::Flask,
            Self::FastApi,
            Self::Pyramid,
            Self::AppEngine,
            Self::Web2py,
            Self::Scientific,
            Self::Angular,
            Self::React,
            Self::Vue,
            Self::Html5,
        ]
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Pure => "Pure Python",
            Self::Empty => "Empty Project",
            Self::Django => "Django",
            Self::Flask => "Flask",
            Self::FastApi => "FastAPI",
            Self::Pyramid => "Pyramid",
            Self::AppEngine => "Google App Engine",
            Self::Web2py => "web2py",
            Self::Scientific => "Jupyter / Scientific",
            Self::Angular => "Angular",
            Self::React => "React",
            Self::Vue => "Vue",
            Self::Html5 => "HTML5",
        }
    }
}

/// The setup steps for a new project.
pub fn plan(
    kind: ProjectKind,
    create_env: bool,
    tool: EnvTool,
    inherit_site_packages: bool,
    has_requirements: bool,
    django_name: &str,
) -> Vec<Step> {
    let mut steps = Vec::new();
    if create_env {
        steps.push(Step::CreateEnv {
            tool,
            inherit_site_packages,
        });
        if has_requirements {
            steps.push(Step::InstallRequirements);
        }
    }
    if kind == ProjectKind::Django {
        steps.push(Step::DjangoProject {
            name: django_name.to_string(),
        });
    }
    if create_env {
        steps.push(Step::SelectEnv);
    }
    steps
}

/// `<root>/.venv`'s interpreter.
pub fn venv_python(root: &Path) -> PathBuf {
    root.join(".venv").join(if cfg!(windows) {
        "Scripts/python.exe"
    } else {
        "bin/python"
    })
}

/// Program and arguments that create `target` from `base`.
pub fn env_command(
    tool: EnvTool,
    base: &Path,
    target: &Path,
    inherit_site_packages: bool,
    uv: Option<&Path>,
) -> (PathBuf, Vec<String>) {
    let mut args: Vec<String> = Vec::new();
    let program = match (tool, uv) {
        (EnvTool::Uv, Some(uv)) => {
            args.extend([
                "venv".to_string(),
                "--seed".into(),
                "--python".into(),
                base.display().to_string(),
            ]);
            uv.to_path_buf()
        }
        _ => {
            args.extend(["-m".to_string(), "venv".into()]);
            base.to_path_buf()
        }
    };
    if inherit_site_packages {
        args.push("--system-site-packages".into());
    }
    args.push(target.display().to_string());
    (program, args)
}

/// A valid Django project (package) name derived from the project name.
pub fn django_project_name(name: &str) -> String {
    let n: String = name
        .trim()
        .chars()
        .map(|c| {
            if c == '-' || c == ' ' || c == '.' {
                '_'
            } else {
                c
            }
        })
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if n.is_empty()
        || n.starts_with(|c: char| c.is_ascii_digit())
        || matches!(n.as_str(), "django" | "test" | "site")
    {
        "mysite".into()
    } else {
        n
    }
}

/// An executable on `PATH` (absolute entries only, so nothing project-relative).
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .filter(|d| d.is_absolute())
        .map(|d| d.join(&exe))
        .find(|p| p.is_file())
}

/// A runnable Django project without Django's own `startproject` (no Django yet).
pub fn write_django_skeleton(root: &Path, name: &str) -> std::io::Result<()> {
    let pkg = root.join(name);
    std::fs::create_dir_all(&pkg)?;
    std::fs::create_dir_all(root.join("templates"))?;
    let files: [(PathBuf, &str); 6] = [
        (root.join("manage.py"), DJANGO_MANAGE),
        (pkg.join("__init__.py"), ""),
        (pkg.join("settings.py"), DJANGO_SETTINGS),
        (pkg.join("urls.py"), DJANGO_URLS),
        (pkg.join("wsgi.py"), DJANGO_WSGI),
        (pkg.join("asgi.py"), DJANGO_ASGI),
    ];
    for (path, text) in files {
        if !path.exists() {
            std::fs::write(path, text.replace("{name}", name))?;
        }
    }
    Ok(())
}

const DJANGO_MANAGE: &str = r#"#!/usr/bin/env python
"""Django's command-line utility for administrative tasks."""
import os
import sys


def main():
    os.environ.setdefault("DJANGO_SETTINGS_MODULE", "{name}.settings")
    from django.core.management import execute_from_command_line

    execute_from_command_line(sys.argv)


if __name__ == "__main__":
    main()
"#;

const DJANGO_SETTINGS: &str = r#"from pathlib import Path

BASE_DIR = Path(__file__).resolve().parent.parent

SECRET_KEY = "django-insecure-change-me"
DEBUG = True
ALLOWED_HOSTS: list[str] = []

INSTALLED_APPS = [
    "django.contrib.admin",
    "django.contrib.auth",
    "django.contrib.contenttypes",
    "django.contrib.sessions",
    "django.contrib.messages",
    "django.contrib.staticfiles",
]

MIDDLEWARE = [
    "django.middleware.security.SecurityMiddleware",
    "django.contrib.sessions.middleware.SessionMiddleware",
    "django.middleware.common.CommonMiddleware",
    "django.middleware.csrf.CsrfViewMiddleware",
    "django.contrib.auth.middleware.AuthenticationMiddleware",
    "django.contrib.messages.middleware.MessageMiddleware",
    "django.middleware.clickjacking.XFrameOptionsMiddleware",
]

ROOT_URLCONF = "{name}.urls"

TEMPLATES = [
    {
        "BACKEND": "django.template.backends.django.DjangoTemplates",
        "DIRS": [BASE_DIR / "templates"],
        "APP_DIRS": True,
        "OPTIONS": {
            "context_processors": [
                "django.template.context_processors.request",
                "django.contrib.auth.context_processors.auth",
                "django.contrib.messages.context_processors.messages",
            ],
        },
    },
]

WSGI_APPLICATION = "{name}.wsgi.application"

DATABASES = {
    "default": {
        "ENGINE": "django.db.backends.sqlite3",
        "NAME": BASE_DIR / "db.sqlite3",
    }
}

LANGUAGE_CODE = "en-us"
TIME_ZONE = "UTC"
USE_I18N = True
USE_TZ = True

STATIC_URL = "static/"
DEFAULT_AUTO_FIELD = "django.db.models.BigAutoField"
"#;

const DJANGO_URLS: &str = r#"from django.contrib import admin
from django.http import HttpResponse
from django.urls import path


def index(request):
    return HttpResponse("Hello from Django!")


urlpatterns = [
    path("admin/", admin.site.urls),
    path("", index),
]
"#;

const DJANGO_WSGI: &str = r#"import os

from django.core.wsgi import get_wsgi_application

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "{name}.settings")

application = get_wsgi_application()
"#;

const DJANGO_ASGI: &str = r#"import os

from django.core.asgi import get_asgi_application

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "{name}.settings")

application = get_asgi_application()
"#;

/// Failure reported by a project setup operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetupError {
    Cancelled,
    Failed(String),
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("setup cancelled"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for SetupError {}

/// A command ready for execution; Django owns its preserved scaffold fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandSpec {
    Process {
        program: PathBuf,
        args: Vec<String>,
        cwd: PathBuf,
    },
    Django {
        python: PathBuf,
        name: String,
        root: PathBuf,
    },
}

impl CommandSpec {
    fn run(self, token: &CancellationToken) -> Result<(), SetupError> {
        match self {
            Self::Process { program, args, cwd } => run_command(&program, &args, &cwd, token),
            Self::Django { python, name, root } => {
                let args = vec![
                    "-m".to_string(),
                    "django".into(),
                    "startproject".into(),
                    name.clone(),
                    ".".into(),
                ];
                match run_command(&python, &args, &root, token) {
                    Ok(()) => Ok(()),
                    Err(SetupError::Cancelled) => Err(SetupError::Cancelled),
                    Err(SetupError::Failed(error)) => {
                        if token.is_cancelled() {
                            return Err(SetupError::Cancelled);
                        }
                        log::info!("django startproject failed ({error}); writing a skeleton");
                        write_django_skeleton(&root, &name).map_err(|error| {
                            SetupError::Failed(format!("Cannot create the Django project: {error}"))
                        })?;
                        Err(SetupError::Failed(format!(
                            "Django is not installed in {}: created a project skeleton (install Django, then run manage.py)",
                            python.display()
                        )))
                    }
                }
            }
        }
    }
}

fn run_command(
    program: &Path,
    args: &[String],
    cwd: &Path,
    token: &CancellationToken,
) -> Result<(), SetupError> {
    let mut command = crate::interpreter::interpreter_command(program);
    command
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped());
    match rustpy_core::process::output_cancellable(&mut command, token) {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => {
            let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
            Err(SetupError::Failed(if error.is_empty() {
                format!("{} exited with {}", program.display(), output.status)
            } else {
                error
            }))
        }
        Err(rustpy_core::process::ProcessError::Cancelled) => Err(SetupError::Cancelled),
        Err(error) => Err(SetupError::Failed(format!(
            "cannot run {}: {error}",
            program.display()
        ))),
    }
}

/// Background command ownership: polling never blocks the UI; cancellation
/// uses the same core process termination path as other cancellable tasks.
static NEXT_COMMAND_OWNER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub struct CommandTask {
    owner: u64,
    command_fingerprint: rustpy_core::vfs::ContentRevision,
    token: CancellationToken,
    receiver: crossbeam_channel::Receiver<Result<(), SetupError>>,
}

impl CommandTask {
    pub fn spawn(command: CommandSpec, token: CancellationToken) -> Self {
        let owner = NEXT_COMMAND_OWNER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let command_fingerprint =
            rustpy_core::vfs::ContentRevision::of(format!("{command:?}").as_bytes());
        let (sender, receiver) = crossbeam_channel::unbounded();
        let worker_token = token.clone();
        std::thread::spawn(move || {
            let _ = sender.send(command.run(&worker_token));
        });
        Self {
            owner,
            command_fingerprint,
            token,
            receiver,
        }
    }
    pub fn quit_guard_state(&self) -> String {
        format!(
            "{:?}",
            (
                self.owner,
                self.command_fingerprint,
                self.token.is_cancelled()
            )
        )
    }
    pub fn poll(&self) -> Option<Result<(), SetupError>> {
        match self.receiver.try_recv() {
            Ok(result) => Some(result),
            Err(crossbeam_channel::TryRecvError::Empty) => None,
            Err(crossbeam_channel::TryRecvError::Disconnected) => Some(Err(SetupError::Failed(
                "the setup step stopped unexpectedly".into(),
            ))),
        }
    }
    pub fn cancel(&self) {
        self.token.cancel();
    }
}
impl Drop for CommandTask {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Work the front end dispatches, while project owns sequencing and outcomes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetupAction {
    Command {
        step: Step,
        command: CommandSpec,
    },
    InstallRequirements {
        interpreter: PathBuf,
        requirements: PathBuf,
    },
    SelectEnvironment {
        interpreter: PathBuf,
    },
    Finished,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetupEvent {
    StepStarted(Step),
    StepFinished {
        step: Step,
        result: Result<(), SetupError>,
    },
    Finished,
}

/// Project setup state independent of egui, package panels and editor selection.
pub struct Setup {
    pub root: PathBuf,
    base: PathBuf,
    environment: Option<PathBuf>,
    steps: VecDeque<Step>,
    current: Option<Step>,
    token: CancellationToken,
    events: VecDeque<SetupEvent>,
    finished: bool,
    pub env_ok: bool,
    pub problems: Vec<String>,
    pub failed: Vec<Step>,
}

impl Setup {
    pub fn new(root: PathBuf, base: PathBuf, steps: Vec<Step>) -> Self {
        Self {
            root,
            base,
            environment: None,
            steps: steps.into(),
            current: None,
            token: CancellationToken::new(),
            events: VecDeque::new(),
            finished: false,
            env_ok: false,
            problems: Vec::new(),
            failed: Vec::new(),
        }
    }
    /// Observational state used to reject a staged window quit after setup changes.
    pub fn quit_guard_state(&self) -> String {
        format!(
            "{:?}",
            (
                &self.root,
                &self.base,
                &self.environment,
                &self.steps,
                &self.events,
                &self.current,
                self.token.is_cancelled(),
                self.finished,
                self.env_ok,
                &self.problems,
                &self.failed
            )
        )
    }
    pub fn command_token(&self) -> CancellationToken {
        self.token.child()
    }
    /// A managed or existing SDK may live outside .venv. Subsequent setup
    /// installation/selection uses this exact admitted interpreter.
    pub fn set_environment(&mut self, interpreter: PathBuf) {
        self.environment = Some(interpreter);
        self.env_ok = true;
    }
    fn environment_python(&self) -> PathBuf {
        self.environment
            .clone()
            .unwrap_or_else(|| venv_python(&self.root))
    }
    pub fn waits_for_pip(&self) -> bool {
        self.current == Some(Step::InstallRequirements)
    }
    pub fn take_events(&mut self) -> Vec<SetupEvent> {
        self.events.drain(..).collect()
    }
    pub fn cancel(&mut self) {
        self.token.cancel();
        self.steps.clear();
    }

    /// `packages_busy` prevents consuming the requirements step until its
    /// specialized package executor is available. `uv` is resolved by the caller.
    pub fn next_action(&mut self, packages_busy: bool, uv: Option<&Path>) -> Option<SetupAction> {
        if self.current.is_some() {
            return None;
        }
        if self.token.is_cancelled() {
            self.steps.clear();
        }
        loop {
            let Some(step) = self.steps.front().cloned() else {
                if !self.finished {
                    self.finished = true;
                    self.events.push_back(SetupEvent::Finished);
                }
                return Some(SetupAction::Finished);
            };
            if step == Step::InstallRequirements && packages_busy {
                return None;
            }
            self.steps.pop_front();
            if matches!(step, Step::InstallRequirements | Step::SelectEnv) && !self.env_ok {
                continue;
            }
            let action = match &step {
                Step::Command {
                    label: _,
                    program,
                    args,
                    cwd,
                } => SetupAction::Command {
                    step: step.clone(),
                    command: CommandSpec::Process {
                        program: program.clone(),
                        args: args.clone(),
                        cwd: cwd.clone(),
                    },
                },
                Step::CreateEnv {
                    tool,
                    inherit_site_packages,
                } => {
                    let (program, args) = env_command(
                        *tool,
                        &self.base,
                        &self.root.join(".venv"),
                        *inherit_site_packages,
                        uv,
                    );
                    SetupAction::Command {
                        step: step.clone(),
                        command: CommandSpec::Process {
                            program,
                            args,
                            cwd: self.root.clone(),
                        },
                    }
                }
                Step::DjangoProject { name } => {
                    let python = if self.env_ok {
                        self.environment_python()
                    } else {
                        self.base.clone()
                    };
                    SetupAction::Command {
                        step: step.clone(),
                        command: CommandSpec::Django {
                            python,
                            name: name.clone(),
                            root: self.root.clone(),
                        },
                    }
                }
                Step::InstallRequirements => SetupAction::InstallRequirements {
                    interpreter: self.environment_python(),
                    requirements: self.root.join("requirements.txt"),
                },
                Step::SelectEnv => SetupAction::SelectEnvironment {
                    interpreter: self.environment_python(),
                },
            };
            self.current = Some(step.clone());
            self.events.push_back(SetupEvent::StepStarted(step));
            return Some(action);
        }
    }

    /// Finish the dispatched step; failure preserves the wizard's pruning and
    /// Django fallback rules. Explicit cancellation never starts later steps.
    pub fn complete(&mut self, result: Result<(), SetupError>) {
        let Some(step) = self.current.take() else {
            return;
        };
        self.events.push_back(SetupEvent::StepFinished {
            step: step.clone(),
            result: result.clone(),
        });
        match (&step, result) {
            (Step::CreateEnv { .. }, Ok(())) => self.env_ok = true,
            (_, Ok(())) => {}
            (_, Err(SetupError::Cancelled)) => {
                self.token.cancel();
                self.steps.clear();
                self.problems.push("Setup cancelled".into());
                self.failed.push(step);
            }
            (Step::CreateEnv { .. }, Err(error)) => {
                self.problems
                    .push(format!("Creating the virtual environment failed: {error}"));
                self.failed.push(step);
                self.steps
                    .retain(|step| matches!(step, Step::DjangoProject { .. }));
            }
            (_, Err(error)) => {
                self.problems.push(error.to_string());
                self.failed.push(step);
            }
        }
    }
}
impl Drop for Setup {
    fn drop(&mut self) {
        self.token.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_interpreter_path_drives_installation_and_selection() {
        let root = PathBuf::from("/project");
        let python = PathBuf::from("/outside/project environment/bin/python");
        let mut setup = Setup::new(
            root.clone(),
            "/base/python".into(),
            vec![Step::InstallRequirements, Step::SelectEnv],
        );
        setup.set_environment(python.clone());
        assert_eq!(
            setup.next_action(false, None),
            Some(SetupAction::InstallRequirements {
                interpreter: python.clone(),
                requirements: root.join("requirements.txt")
            })
        );
        setup.complete(Ok(()));
        assert_eq!(
            setup.next_action(false, None),
            Some(SetupAction::SelectEnvironment {
                interpreter: python
            })
        );
    }

    #[test]
    fn plans_install_and_django_with_the_new_environment() {
        let flask = plan(ProjectKind::Flask, true, EnvTool::Venv, false, true, "x");
        assert_eq!(
            flask,
            vec![
                Step::CreateEnv {
                    tool: EnvTool::Venv,
                    inherit_site_packages: false
                },
                Step::InstallRequirements,
                Step::SelectEnv
            ]
        );
        let django = plan(ProjectKind::Django, true, EnvTool::Uv, true, true, "site1");
        assert_eq!(
            django,
            vec![
                Step::CreateEnv {
                    tool: EnvTool::Uv,
                    inherit_site_packages: true
                },
                Step::InstallRequirements,
                Step::DjangoProject {
                    name: "site1".into()
                },
                Step::SelectEnv
            ]
        );
        // Custom interpreter: nothing is installed into it, Django uses it as is.
        assert_eq!(
            plan(ProjectKind::Django, false, EnvTool::Venv, false, true, "s"),
            vec![Step::DjangoProject { name: "s".into() }]
        );
        assert_eq!(
            plan(ProjectKind::Pure, true, EnvTool::Venv, false, false, "p"),
            vec![
                Step::CreateEnv {
                    tool: EnvTool::Venv,
                    inherit_site_packages: false
                },
                Step::SelectEnv
            ]
        );
        assert!(plan(ProjectKind::Pure, false, EnvTool::Venv, false, false, "p").is_empty());
    }

    #[test]
    fn environment_commands() {
        let base = Path::new("/usr/bin/python3");
        let target = Path::new("/p/.venv");
        let (prog, args) = env_command(EnvTool::Venv, base, target, false, None);
        assert_eq!(prog, base);
        assert_eq!(args, vec!["-m", "venv", "/p/.venv"]);
        let (_, args) = env_command(EnvTool::Venv, base, target, true, None);
        assert_eq!(
            args,
            vec!["-m", "venv", "--system-site-packages", "/p/.venv"]
        );
        let (prog, args) = env_command(EnvTool::Uv, base, target, true, Some(Path::new("/bin/uv")));
        assert_eq!(prog, Path::new("/bin/uv"));
        assert_eq!(
            args,
            vec![
                "venv",
                "--seed",
                "--python",
                "/usr/bin/python3",
                "--system-site-packages",
                "/p/.venv"
            ]
        );
        // uv requested but not found: plain venv.
        assert_eq!(env_command(EnvTool::Uv, base, target, false, None).0, base);
    }

    #[test]
    fn django_names() {
        assert_eq!(django_project_name("my-site"), "my_site");
        assert_eq!(django_project_name("pythonProject"), "pythonProject");
        assert_eq!(django_project_name("1st"), "mysite");
        assert_eq!(django_project_name("django"), "mysite");
        assert_eq!(django_project_name("  "), "mysite");
    }

    #[test]
    fn django_skeleton_is_complete() {
        let root = std::env::temp_dir().join(format!("rustpy-django-skel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        write_django_skeleton(&root, "site1").unwrap();
        for f in [
            "manage.py",
            "site1/__init__.py",
            "site1/settings.py",
            "site1/urls.py",
            "site1/wsgi.py",
            "site1/asgi.py",
        ] {
            assert!(root.join(f).is_file(), "{f}");
        }
        assert!(root.join("templates").is_dir());
        let settings = std::fs::read_to_string(root.join("site1/settings.py")).unwrap();
        assert!(
            settings.contains("ROOT_URLCONF = \"site1.urls\"")
                && settings.contains("MIDDLEWARE")
                && settings.contains("WSGI_APPLICATION = \"site1.wsgi.application\"")
        );
        std::fs::remove_dir_all(root).ok();
    }
}
