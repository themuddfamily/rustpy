//! Typed run configuration factories and executable lowering. Type identity and
//! options survive persistence independently of the shared process transport.
use crate::{RunConfig, RunConfigKind};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationType {
    Python,
    PythonModule,
    Pytest,
    Unittest,
    Doctest,
    Nose,
    Nose2,
    Trial,
    Tox,
    Behave,
    Lettuce,
    DjangoServer,
    DjangoTests,
    DjangoTask,
    Flask,
    FastApi,
    Pyramid,
    Web2py,
    AppEngine,
    Compound,
    Shell,
    Docker,
    DockerCompose,
    DockerImage,
    Sphinx,
    Docutils,
    Jupyter,
    Npm,
    Node,
    JavaScriptDebug,
    PythonDebugServer,
    AttachProcess,
    Conda,
    ConsoleFile,
    HttpRequest,
}
impl ConfigurationType {
    pub const ALL: &'static [Self] = &[
        Self::Python,
        Self::PythonModule,
        Self::Pytest,
        Self::Unittest,
        Self::Doctest,
        Self::Nose,
        Self::Nose2,
        Self::Trial,
        Self::Tox,
        Self::Behave,
        Self::Lettuce,
        Self::DjangoServer,
        Self::DjangoTests,
        Self::DjangoTask,
        Self::Flask,
        Self::FastApi,
        Self::Pyramid,
        Self::Web2py,
        Self::AppEngine,
        Self::Compound,
        Self::Shell,
        Self::Docker,
        Self::DockerCompose,
        Self::DockerImage,
        Self::Sphinx,
        Self::Docutils,
        Self::Jupyter,
        Self::Npm,
        Self::Node,
        Self::JavaScriptDebug,
        Self::PythonDebugServer,
        Self::AttachProcess,
        Self::Conda,
        Self::ConsoleFile,
        Self::HttpRequest,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Python => "Python script",
            Self::PythonModule => "Python module",
            Self::Pytest => "pytest",
            Self::Unittest => "Unittests",
            Self::Doctest => "Doctests",
            Self::Nose => "Nose (legacy)",
            Self::Nose2 => "Nose2",
            Self::Trial => "Twisted Trial",
            Self::Tox => "Tox",
            Self::Behave => "Behave",
            Self::Lettuce => "Lettuce (legacy)",
            Self::DjangoServer => "Django server",
            Self::DjangoTests => "Django tests",
            Self::DjangoTask => "Django manage.py task",
            Self::Flask => "Flask server",
            Self::FastApi => "FastAPI (uvicorn)",
            Self::Pyramid => "Pyramid",
            Self::Web2py => "web2py",
            Self::AppEngine => "Google App Engine",
            Self::Compound => "Compound",
            Self::Shell => "Shell Script",
            Self::Docker => "Docker container",
            Self::DockerCompose => "Docker Compose",
            Self::DockerImage => "Docker image build",
            Self::Sphinx => "Python docs: Sphinx",
            Self::Docutils => "Python docs: Docutils",
            Self::Jupyter => "Jupyter",
            Self::Npm => "npm",
            Self::Node => "Node.js",
            Self::JavaScriptDebug => "JavaScript Debug (DAP)",
            Self::PythonDebugServer => "Python Debug Server",
            Self::AttachProcess => "Attach to Python Process",
            Self::Conda => "Conda environment task",
            Self::ConsoleFile => "Run file in Python Console",
            Self::HttpRequest => "HTTP Request",
        }
    }
    pub fn is_test(self) -> bool {
        matches!(
            self,
            Self::Pytest
                | Self::Unittest
                | Self::Doctest
                | Self::Nose
                | Self::Nose2
                | Self::Trial
                | Self::Tox
                | Self::Behave
                | Self::Lettuce
                | Self::DjangoTests
        )
    }
    pub fn special(self) -> bool {
        matches!(
            self,
            Self::Compound
                | Self::PythonDebugServer
                | Self::AttachProcess
                | Self::JavaScriptDebug
                | Self::HttpRequest
        )
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    #[default]
    Custom,
    Script,
    Module,
    Class,
    Method,
    Function,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ConfigurationSpec {
    pub kind: ConfigurationType,
    pub target: String,
    pub target_kind: TargetKind,
    pub member: String,
    pub keywords: String,
    pub arguments: Vec<String>,
    pub host: String,
    pub port: u16,
    pub listen: bool,
    pub pydevd: bool,
    pub pydevd_legacy: bool,
    pub path_mappings: BTreeMap<String, String>,
    pub process_id: u32,
    pub configurations: Vec<String>,
    pub environment: String,
    pub image: String,
    pub service: String,
    pub executable: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker_resources: Option<crate::interpreter_remote::DockerResources>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_docker_build_resources: Option<LegacyDockerBuildResources>,
    /// Run selected Compose services and dependencies in a fresh owned project.
    /// Ordinary named/external projects retain their existing lifecycle when false.
    #[serde(default)]
    pub compose_owned_project: bool,
    pub adapter_arguments: Vec<String>,
    pub debug_request: String,
    pub debug_options: serde_json::Value,
    pub request_line: usize,
}
/// Explicit opt-in to Docker's legacy builder. CPU limits apply to every RUN;
/// BuildKit is deliberately disabled for this command only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyDockerBuildResources {
    pub cpuset_cpus: String,
    pub cpu_millis: u32,
}
impl LegacyDockerBuildResources {
    pub fn validate(&self) -> Result<(), String> {
        crate::interpreter_remote::DockerResources {
            cpuset_cpus: self.cpuset_cpus.clone(),
            cpu_millis: self.cpu_millis,
            threads: 1,
            nice: 0,
            nice_program: "/usr/bin/nice".into(),
            name: String::new(),
            labels: BTreeMap::new(),
        }
        .validate()
    }
    pub fn apply(&self, args: &[String]) -> Result<Vec<String>, String> {
        self.validate()?;
        if args.len() < 2 || args.first().map(String::as_str) != Some("build") {
            return Err("Legacy build policy requires docker build".into());
        }
        // Accept only unambiguous options emitted by the public build planners.
        let mut index = 1;
        while index + 1 < args.len() {
            match args[index].as_str() {
                "-f" | "--file" | "-t" | "--tag" | "--label" | "--iidfile" => {
                    if args[index + 1].starts_with('-') {
                        return Err("Missing build option value".into());
                    }
                    index += 2;
                }
                _ => {
                    return Err(format!(
                        "Unsupported or policy-owned build option: {}",
                        args[index]
                    ));
                }
            }
        }
        if index != args.len() - 1 || args[index].starts_with('-') {
            return Err("Choose exactly one local Docker build context".into());
        }
        let context = Path::new(&args[index]);
        if !context.is_absolute() {
            return Err("Legacy build policy requires a resolved absolute local context".into());
        }
        let context = dunce::canonicalize(context)
            .map_err(|error| format!("Cannot resolve local build context: {error}"))?;
        if !context.is_dir() {
            return Err("Legacy build policy requires an existing local directory".into());
        }
        let mut result = vec![
            "build".into(),
            "--cpuset-cpus".into(),
            self.cpuset_cpus.clone(),
            "--cpu-period".into(),
            "100000".into(),
            "--cpu-quota".into(),
            (u64::from(self.cpu_millis) * 100).to_string(),
            "--pull=false".into(),
            "--network=none".into(),
            "--force-rm".into(),
            "--rm=true".into(),
            "--no-cache".into(),
        ];
        result.extend_from_slice(&args[1..index]);
        result.push(
            context
                .to_str()
                .ok_or("Build context must be UTF-8")?
                .into(),
        );
        Ok(result)
    }
    /// Resolve relative contexts only against an explicitly captured working directory.
    pub fn apply_at(&self, args: &[String], working_dir: &Path) -> Result<Vec<String>, String> {
        if !working_dir.is_absolute() {
            return Err("Build working directory must be absolute".into());
        }
        let mut resolved = args.to_vec();
        let context = resolved.last_mut().ok_or("Missing build context")?;
        let path = Path::new(context);
        if !path.is_absolute() {
            *context = working_dir
                .join(path)
                .to_str()
                .ok_or("Build context must be UTF-8")?
                .into();
        }
        self.apply(&resolved)
    }
}
impl Default for ConfigurationSpec {
    fn default() -> Self {
        Self::new(ConfigurationType::Python)
    }
}
impl ConfigurationSpec {
    pub fn new(kind: ConfigurationType) -> Self {
        let target = match kind {
            ConfigurationType::Python | ConfigurationType::ConsoleFile => "main.py",
            ConfigurationType::PythonModule => "main",
            ConfigurationType::DjangoServer
            | ConfigurationType::DjangoTests
            | ConfigurationType::DjangoTask => "manage.py",
            ConfigurationType::Flask => "app",
            ConfigurationType::FastApi => "main:app",
            ConfigurationType::Pyramid => "development.ini",
            ConfigurationType::Web2py => "run_web2py.py",
            ConfigurationType::AppEngine => "app.yaml",
            ConfigurationType::Shell => "script.sh",
            ConfigurationType::DockerCompose => "compose.yaml",
            ConfigurationType::DockerImage => "Dockerfile",
            ConfigurationType::Sphinx => "docs",
            ConfigurationType::Docutils => "README.rst",
            ConfigurationType::Jupyter => "",
            ConfigurationType::Npm => "dev",
            ConfigurationType::Node | ConfigurationType::JavaScriptDebug => "index.js",
            ConfigurationType::HttpRequest => "requests.http",
            _ => "",
        };
        Self {
            kind,
            target: target.into(),
            target_kind: TargetKind::Custom,
            member: String::new(),
            keywords: String::new(),
            arguments: Vec::new(),
            host: "127.0.0.1".into(),
            port: 5678,
            listen: kind == ConfigurationType::PythonDebugServer,
            pydevd: false,
            pydevd_legacy: false,
            path_mappings: BTreeMap::new(),
            process_id: 0,
            configurations: Vec::new(),
            environment: String::new(),
            image: String::new(),
            service: String::new(),
            executable: PathBuf::new(),
            compose_owned_project: false,
            docker_resources: None,
            legacy_docker_build_resources: None,
            adapter_arguments: Vec::new(),
            debug_request: "launch".into(),
            debug_options: if kind == ConfigurationType::JavaScriptDebug {
                serde_json::json!({"type":"pwa-node","console":"internalConsole","sourceMaps":true})
            } else {
                serde_json::json!({})
            },
            request_line: 0,
        }
    }
    pub fn label(&self) -> &'static str {
        self.kind.label()
    }
    pub fn validate(&self) -> Result<(), String> {
        if self.compose_owned_project && self.kind != ConfigurationType::DockerCompose {
            return Err("Isolated Compose ownership requires a Compose configuration".into());
        }
        if let Some(resources) = &self.docker_resources {
            resources.validate()?;
            if self.kind != ConfigurationType::Docker {
                return Err("Container resource policy currently supports Docker run only, not image builds or Compose".into());
            }
            if self.target.trim().is_empty() {
                return Err(
                    "Container resource policy requires an explicit in-container command".into(),
                );
            }
        }
        if let Some(resources) = &self.legacy_docker_build_resources {
            resources.validate()?;
            if self.kind != ConfigurationType::DockerImage {
                return Err("Legacy build policy requires a Docker image configuration".into());
            }
        }
        let optional_target = matches!(
            self.kind,
            ConfigurationType::Pytest
                | ConfigurationType::Unittest
                | ConfigurationType::Nose
                | ConfigurationType::Nose2
                | ConfigurationType::Trial
                | ConfigurationType::Tox
                | ConfigurationType::Behave
                | ConfigurationType::Lettuce
                | ConfigurationType::Jupyter
                | ConfigurationType::DjangoTests
                | ConfigurationType::Compound
                | ConfigurationType::Docker
                | ConfigurationType::Conda
                | ConfigurationType::PythonDebugServer
                | ConfigurationType::AttachProcess
                | ConfigurationType::JavaScriptDebug
        );
        if !optional_target && self.target.trim().is_empty() {
            return Err("Specify the configuration target".into());
        }
        if matches!(
            self.kind,
            ConfigurationType::Docker | ConfigurationType::DockerImage
        ) && self.image.trim().is_empty()
        {
            return Err("Specify a Docker image name/tag".into());
        }
        if self.kind == ConfigurationType::Conda
            && (self.environment.trim().is_empty() || self.target.trim().is_empty())
        {
            return Err("Specify a Conda environment and executable task".into());
        }
        if self.kind == ConfigurationType::Compound && self.configurations.is_empty() {
            return Err("Select configurations for the compound run".into());
        }
        if self.kind == ConfigurationType::AttachProcess && self.process_id == 0 {
            return Err("Specify a nonzero process ID".into());
        }
        if matches!(
            self.kind,
            ConfigurationType::PythonDebugServer | ConfigurationType::JavaScriptDebug
        ) && (self.host.trim().is_empty() || self.port == 0)
        {
            return Err("Specify a debugger host and nonzero port".into());
        }
        if self.kind == ConfigurationType::JavaScriptDebug && self.executable.as_os_str().is_empty()
        {
            return Err("Specify the JavaScript DAP adapter executable (for example a configured js-debug adapter)".into());
        }
        if self.pydevd && self.pydevd_legacy {
            return Err(
                "Choose either pydevd DAP or the legacy pydevd quoted-line protocol".into(),
            );
        }
        if self.kind == ConfigurationType::PythonDebugServer
            && (self.pydevd || self.pydevd_legacy)
            && !self.listen
        {
            return Err("pydevd DAP mode requires Listen for remote connection".into());
        }
        if self.kind == ConfigurationType::JavaScriptDebug
            && !self
                .adapter_arguments
                .iter()
                .any(|argument| argument.contains("$Port$"))
        {
            return Err("TCP JavaScript adapter arguments must include $Port$".into());
        }
        if !matches!(self.debug_request.as_str(), "launch" | "attach") {
            return Err("DAP request must be launch or attach".into());
        }
        if !self.debug_options.is_object() {
            return Err("DAP options must be a JSON object".into());
        }
        if self.target.contains('\0')
            || self
                .arguments
                .iter()
                .any(|argument| argument.contains('\0'))
        {
            return Err("Command arguments cannot contain NUL".into());
        }
        Ok(())
    }
    pub fn target_argument(&self) -> String {
        if self.member.is_empty() || self.kind == ConfigurationType::Lettuce {
            return self.target.clone();
        }
        if self.kind == ConfigurationType::Nose {
            return format!("{}:{}", self.target, self.member.replace("::", "."));
        }
        let separator = if self.kind == ConfigurationType::Pytest
            || (matches!(
                self.kind,
                ConfigurationType::Unittest | ConfigurationType::Nose2
            ) && (self.target.ends_with(".py")
                || self.target.contains('/')
                || self.target.contains('\\')))
        {
            "::"
        } else if matches!(
            self.kind,
            ConfigurationType::Behave | ConfigurationType::Lettuce
        ) {
            ":"
        } else {
            "."
        };
        format!(
            "{}{separator}{}",
            self.target,
            self.member.replace("::", separator)
        )
    }
    /// Compile type-specific options into the ordinary process/test transport.
    /// Additional configuration parameters stay quoted separately and are
    /// appended by the existing command builder.
    /// Lower with the same project-relative working-directory authority as RunSession.
    pub fn materialize_at(
        &self,
        cfg: &RunConfig,
        project_root: &Path,
    ) -> Result<RunConfig, String> {
        if self.legacy_docker_build_resources.is_none() {
            return self.materialize(cfg);
        }
        if !project_root.is_absolute() {
            return Err("Build project root must be absolute".into());
        }
        let mut bound = cfg.clone();
        bound.working_dir = Some(match cfg.working_dir.as_deref() {
            Some(path) if path.is_absolute() => path.to_path_buf(),
            Some(path) => project_root.join(path),
            None => project_root.to_path_buf(),
        });
        self.materialize(&bound)
    }
    pub fn materialize(&self, cfg: &RunConfig) -> Result<RunConfig, String> {
        self.validate()?;
        if self.legacy_docker_build_resources.is_some() && !cfg.parameters.trim().is_empty() {
            return Err("Put legacy build options in structured arguments; free-form parameters could override the policy".into());
        }
        let mut result = cfg.clone();
        let mut args = self.arguments.clone();
        let target = self.target_argument();
        let program = |default: &str| {
            if self.executable.as_os_str().is_empty() {
                PathBuf::from(default)
            } else {
                self.executable.clone()
            }
        };
        let module = |name: &str| RunConfigKind::PythonModule {
            module: name.into(),
        };
        use ConfigurationType::*;
        result.kind = match self.kind {
            Python | ConsoleFile => RunConfigKind::Python {
                script: self.target.clone().into(),
            },
            PythonModule => module(&self.target),
            Pytest => {
                if self.target_kind == TargetKind::Module {
                    args.insert(0, "--pyargs".into());
                }
                RunConfigKind::Pytest {
                    target,
                    keywords: self.keywords.clone(),
                }
            }
            Unittest => RunConfigKind::Unittest { target },
            Doctest => {
                if !target.is_empty() {
                    args.insert(0, target);
                }
                module("doctest")
            }
            Nose | Nose2 | Trial | Tox | Behave | Lettuce => {
                if !target.is_empty() {
                    if self.kind == Tox {
                        args.splice(0..0, ["-e".into(), target]);
                    } else {
                        args.insert(0, target);
                    }
                }
                if !self.keywords.is_empty() {
                    match self.kind {
                        Nose2 => args.extend(["-k".into(), self.keywords.clone()]),
                        Nose => args.extend(["-m".into(), self.keywords.clone()]),
                        Behave => args.extend(["--name".into(), self.keywords.clone()]),
                        Lettuce => args.extend(["--tag".into(), self.keywords.clone()]),
                        Tox => args.extend(["--".into(), "-k".into(), self.keywords.clone()]),
                        _ => {}
                    }
                }
                if self.kind == Lettuce && !self.member.is_empty() {
                    args.extend(["--scenarios".into(), self.member.clone()]);
                }
                module(match self.kind {
                    Nose => "nose",
                    Nose2 => "nose2",
                    Trial => "twisted.trial",
                    Tox => "tox",
                    Behave => "behave",
                    _ => "lettuce.bin",
                })
            }
            DjangoServer | DjangoTests | DjangoTask => {
                let task = match self.kind {
                    DjangoServer => "runserver",
                    DjangoTests => "test",
                    _ => {
                        if self.member.is_empty() {
                            "check"
                        } else {
                            &self.member
                        }
                    }
                };
                let mut prefix = vec![task.to_string()];
                if self.kind == DjangoTests && !self.member.is_empty() {
                    prefix.push(self.member.clone());
                }
                prefix.append(&mut args);
                args = prefix;
                RunConfigKind::Python {
                    script: self.target.clone().into(),
                }
            }
            Flask => {
                let mut prefix = vec!["--app".into(), self.target.clone(), "run".into()];
                prefix.append(&mut args);
                args = prefix;
                module("flask")
            }
            FastApi => {
                args.insert(0, self.target.clone());
                module("uvicorn")
            }
            Pyramid => {
                if self.target.ends_with(".py") {
                    RunConfigKind::Python {
                        script: self.target.clone().into(),
                    }
                } else {
                    args.insert(0, self.target.clone());
                    module("pyramid.scripts.pserve")
                }
            }
            Web2py => RunConfigKind::Python {
                script: self.target.clone().into(),
            },
            AppEngine => {
                let mut command = vec![self.target.clone()];
                command.append(&mut args);
                RunConfigKind::Program {
                    program: program("dev_appserver.py"),
                    args: command,
                }
            }
            Shell => RunConfigKind::ShellScript {
                script: self.target.clone().into(),
            },
            Docker => {
                let mut command = vec!["run".into(), "--rm".into(), "--init".into()];
                command.append(&mut args);
                command.push(self.image.clone());
                if !self.target.is_empty() {
                    command.push(self.target.clone());
                }
                if let Some(resources) = &self.docker_resources {
                    command = resources.apply_container_command(&command)?;
                }
                args = Vec::new();
                RunConfigKind::Program {
                    program: program("docker"),
                    args: command,
                }
            }
            DockerCompose => {
                let mut command = vec![
                    "compose".into(),
                    "-f".into(),
                    self.target.clone(),
                    "up".into(),
                ];
                command.append(&mut args);
                if !self.service.is_empty() {
                    command.push(self.service.clone());
                }
                args = Vec::new();
                RunConfigKind::Program {
                    program: program("docker"),
                    args: command,
                }
            }
            DockerImage => {
                let mut command = vec![
                    "build".into(),
                    "-f".into(),
                    self.target.clone(),
                    "-t".into(),
                    self.image.clone(),
                ];
                command.append(&mut args);
                command.push(".".into());
                if let Some(resources) = &self.legacy_docker_build_resources {
                    let working_dir = cfg.working_dir.as_deref().filter(|path| path.is_absolute())
                        .ok_or("Legacy build requires a captured project root or absolute working directory")?;
                    let working_dir =
                        dunce::canonicalize(working_dir).map_err(|error| error.to_string())?;
                    command = resources.apply_at(&command, &working_dir)?;
                    result.working_dir = Some(working_dir);
                    result.env.insert("DOCKER_BUILDKIT".into(), "0".into());
                }
                args = Vec::new();
                RunConfigKind::Program {
                    program: program("docker"),
                    args: command,
                }
            }
            Sphinx => {
                let mut prefix = vec![
                    "-b".into(),
                    if self.member.is_empty() {
                        "html".into()
                    } else {
                        self.member.clone()
                    },
                    self.target.clone(),
                    "_build/html".into(),
                ];
                prefix.append(&mut args);
                args = prefix;
                module("sphinx")
            }
            Docutils => {
                let mut prefix = vec![
                    "--writer=html5".into(),
                    self.target.clone(),
                    "documentation.html".into(),
                ];
                prefix.append(&mut args);
                args = prefix;
                module("docutils")
            }
            Jupyter => {
                if !self.target.is_empty() {
                    args.insert(0, self.target.clone());
                }
                module("jupyterlab")
            }
            Npm => {
                let mut command = vec!["run".into(), self.target.clone()];
                if !args.is_empty() {
                    command.push("--".into());
                    command.append(&mut args);
                }
                RunConfigKind::Program {
                    program: program(if cfg!(windows) { "npm.cmd" } else { "npm" }),
                    args: command,
                }
            }
            Node => {
                args.push(self.target.clone());
                RunConfigKind::Program {
                    program: program("node"),
                    args: std::mem::take(&mut args),
                }
            }
            Conda => {
                let mut command = vec![
                    "run".into(),
                    "--no-capture-output".into(),
                    "-n".into(),
                    self.environment.clone(),
                    "--".into(),
                    self.target.clone(),
                ];
                command.append(&mut args);
                RunConfigKind::Program {
                    program: program("conda"),
                    args: command,
                }
            }
            Compound | PythonDebugServer | AttachProcess | JavaScriptDebug | HttpRequest => {
                RunConfigKind::Program {
                    program: PathBuf::new(),
                    args: Vec::new(),
                }
            }
        };
        if self.kind == ConsoleFile {
            result.run_with_console = true;
        }
        if !args.is_empty() {
            let prefix = quote_arguments(&args);
            result.parameters = if result.parameters.is_empty() {
                prefix
            } else {
                format!("{prefix} {}", result.parameters)
            };
        }
        Ok(result)
    }
}
impl crate::interpreter_remote::DockerResources {
    /// Apply an explicit policy to a direct Docker run/create plan. Unknown
    /// options fail closed instead of guessing the image/command boundary.
    pub fn apply_container_command(&self, args: &[String]) -> Result<Vec<String>, String> {
        self.validate()?;
        if !args
            .first()
            .is_some_and(|verb| matches!(verb.as_str(), "run" | "create"))
        {
            return Err("Container resource policy needs a direct run/create command".into());
        }
        let mut index = 1;
        let mut options = Vec::new();
        while let Some(argument) = args.get(index) {
            if argument == "--" {
                index += 1;
                break;
            }
            if !argument.starts_with('-') {
                break;
            }
            let (flag, inline) = argument
                .split_once('=')
                .map_or((argument.as_str(), None), |(flag, value)| {
                    (flag, Some(value))
                });
            if matches!(
                flag,
                "--cpuset-cpus" | "--cpu-period" | "--cpu-quota" | "--cpus" | "--entrypoint"
            ) {
                return Err(format!(
                    "Explicit container policy owns {flag}; configure its policy fields instead"
                ));
            }
            if matches!(
                flag,
                "--rm"
                    | "--init"
                    | "-i"
                    | "--interactive"
                    | "-t"
                    | "--tty"
                    | "-it"
                    | "-ti"
                    | "-d"
                    | "--detach"
                    | "--read-only"
            ) {
                if inline.is_some_and(|value| !matches!(value, "true" | "false")) {
                    return Err(format!("Invalid boolean Docker option: {argument}"));
                }
                options.push(argument.clone());
                index += 1;
                continue;
            }
            if !matches!(
                flag,
                "--name"
                    | "--label"
                    | "-l"
                    | "--env"
                    | "-e"
                    | "--volume"
                    | "-v"
                    | "--publish"
                    | "-p"
                    | "--workdir"
                    | "-w"
                    | "--user"
                    | "-u"
                    | "--network"
                    | "--hostname"
                    | "--add-host"
                    | "--pull"
                    | "--platform"
                    | "--mount"
                    | "--memory"
                    | "-m"
                    | "--pids-limit"
                    | "--env-file"
                    | "--security-opt"
                    | "--cap-add"
                    | "--cap-drop"
            ) {
                return Err(format!(
                    "Unknown or ambiguous Docker option under explicit container policy: {argument}"
                ));
            }
            let value = inline
                .or_else(|| args.get(index + 1).map(String::as_str))
                .filter(|value| !value.is_empty() && !value.starts_with('-'))
                .ok_or_else(|| format!("Missing or ambiguous Docker option value: {argument}"))?;
            if flag == "--name" && !self.name.is_empty() {
                if value != self.name {
                    return Err("Container name conflicts with explicit policy owner".into());
                }
            } else {
                options.push(argument.clone());
                if inline.is_none() {
                    options.push(value.into());
                }
            }
            index += if inline.is_some() { 1 } else { 2 };
        }
        let image = args
            .get(index)
            .filter(|image| !image.is_empty() && !image.starts_with('-'))
            .ok_or("Explicit container policy requires an image")?;
        let command = args.get(index+1..).filter(|command| !command.is_empty() && !command[0].is_empty())
            .ok_or("Explicit container policy requires an in-container command; image default CMD is unknown")?;
        let process = self.process_prefix()?;
        let mut result = vec![args[0].clone()];
        result.extend(options);
        result.extend(self.container_args()?);
        // Override the image entrypoint, so an arbitrary image ENTRYPOINT
        // cannot turn the admitted nice command into inert argument text.
        result.extend([
            "--entrypoint".into(),
            process[0].clone(),
            "--".into(),
            image.clone(),
        ]);
        result.extend_from_slice(&process[1..]);
        result.extend_from_slice(command);
        Ok(result)
    }
}

pub fn quote_arguments(args: &[String]) -> String {
    args.iter()
        .map(|argument| format!("\"{}\"", argument.replace('"', "\"'\"'\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn new_config(kind: ConfigurationType, root: &Path, names: &[&str]) -> RunConfig {
    let spec = ConfigurationSpec::new(kind);
    let mut cfg = RunConfig {
        name: RunConfig::unique_name(kind.label(), names),
        working_dir: Some(root.to_path_buf()),
        type_options: Some(spec.clone()),
        ..Default::default()
    };
    // Incomplete templates remain editable; validation runs before launching.
    if let Ok(materialized) = spec.materialize(&cfg) {
        cfg.kind = materialized.kind;
        cfg.run_with_console = materialized.run_with_console;
    }
    cfg
}
/// Resolve the whole compound graph before any child starts. This rejects a
/// missing member or cycle atomically instead of partially launching a graph.
pub fn compound_members<'a>(
    name: &str,
    configs: &'a [RunConfig],
) -> Result<Vec<&'a RunConfig>, String> {
    fn visit<'a>(
        name: &str,
        configs: &'a [RunConfig],
        visiting: &mut HashSet<String>,
        result: &mut Vec<&'a RunConfig>,
    ) -> Result<(), String> {
        if !visiting.insert(name.into()) {
            return Err(format!("Compound configuration cycle at {name}"));
        }
        let cfg = configs
            .iter()
            .find(|cfg| cfg.name == name)
            .ok_or_else(|| format!("Missing compound configuration: {name}"))?;
        if let Some(spec) = cfg
            .type_options
            .as_ref()
            .filter(|spec| spec.kind == ConfigurationType::Compound)
        {
            spec.validate()?;
            for member in &spec.configurations {
                visit(member, configs, visiting, result)?;
            }
        } else if !result.iter().any(|previous| previous.name == cfg.name) {
            if let Some(spec) = &cfg.type_options {
                spec.validate()?;
            }
            result.push(cfg);
        }
        visiting.remove(name);
        Ok(())
    }
    let mut result = Vec::new();
    visit(name, configs, &mut HashSet::new(), &mut result)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn factories_retain_typed_identity_and_materialize_actual_commands() {
        for kind in ConfigurationType::ALL {
            let mut spec = ConfigurationSpec::new(*kind);
            spec.target = match kind {
                ConfigurationType::Tox => "py313",
                ConfigurationType::Docker => "python",
                ConfigurationType::Conda => "python",
                _ => {
                    if spec.target.is_empty() {
                        "tests"
                    } else {
                        &spec.target
                    }
                }
            }
            .to_string();
            spec.image = "project:test".into();
            spec.environment = "analysis".into();
            spec.configurations = vec!["child".into()];
            spec.process_id = 42;
            spec.executable = if *kind == ConfigurationType::JavaScriptDebug {
                "node".into()
            } else {
                PathBuf::new()
            };
            spec.adapter_arguments = vec!["js-debug/dapDebugServer.js".into(), "$Port$".into()];
            let cfg = RunConfig {
                type_options: Some(spec.clone()),
                ..Default::default()
            };
            let lowered = spec.materialize(&cfg).unwrap();
            assert_eq!(lowered.type_options, Some(spec.clone()));
            assert_eq!(lowered.kind_label(), kind.label());
            if !kind.special() {
                assert!(
                    !matches!(lowered.kind,RunConfigKind::Program{ref program,..}if program.as_os_str().is_empty()),
                    "{kind:?}"
                );
            }
        }
    }
    #[test]
    fn generated_argument_quoting_preserves_whitespace_both_quotes_and_windows_paths() {
        let args = vec![
            String::new(),
            "space here".into(),
            "both '\" quotes".into(),
            "C:\\path with spaces\\file.py".into(),
            "π".into(),
        ];
        assert_eq!(RunConfig::split_args(&quote_arguments(&args)), args);
    }
    #[test]
    fn targets_keywords_and_additional_arguments_are_not_lost_or_duplicated() {
        let mut spec = ConfigurationSpec::new(ConfigurationType::Pytest);
        spec.target = "package.tests".into();
        spec.target_kind = TargetKind::Module;
        spec.member = "TestCase::test_method".into();
        spec.keywords = "not slow".into();
        spec.arguments = vec!["-x".into()];
        let cfg = RunConfig {
            parameters: "--maxfail 2".into(),
            type_options: Some(spec.clone()),
            ..Default::default()
        };
        let lowered = spec.materialize(&cfg).unwrap();
        assert_eq!(
            RunConfig::split_args(&lowered.parameters),
            ["--pyargs", "-x", "--maxfail", "2"]
        );
        assert!(
            matches!(lowered.kind,RunConfigKind::Pytest{target,keywords}if target=="package.tests::TestCase::test_method"&&keywords=="not slow")
        );
        let cfg = new_config(ConfigurationType::Flask, Path::new("/project"), &[]);
        assert!(cfg.parameters.is_empty());
        let lowered = cfg
            .type_options
            .as_ref()
            .unwrap()
            .materialize(&cfg)
            .unwrap();
        assert_eq!(
            RunConfig::split_args(&lowered.parameters),
            ["--app", "app", "run"]
        );
    }
    #[test]
    fn tox_environment_and_container_context_have_correct_cli_placement() {
        let mut spec = ConfigurationSpec::new(ConfigurationType::Tox);
        spec.target = "py313,lint".into();
        let cfg = spec.materialize(&RunConfig::default()).unwrap();
        assert_eq!(RunConfig::split_args(&cfg.parameters), ["-e", "py313,lint"]);
        let mut spec = ConfigurationSpec::new(ConfigurationType::DockerImage);
        spec.target = "build/Dockerfile".into();
        spec.image = "myproject:test".into();
        spec.arguments = vec!["--build-arg".into(), "VALUE=a b".into()];
        let cfg = spec.materialize(&RunConfig::default()).unwrap();
        assert!(
            matches!(cfg.kind,RunConfigKind::Program{args,..}if args==["build","-f","build/Dockerfile","-t","myproject:test","--build-arg","VALUE=a b","."])
        );
    }
    #[test]
    fn appengine_server_does_not_deploy_and_console_file_uses_existing_console_pipeline() {
        let app = ConfigurationSpec::new(ConfigurationType::AppEngine)
            .materialize(&RunConfig::default())
            .unwrap();
        assert!(
            matches!(app.kind,RunConfigKind::Program{program,args}if program==Path::new("dev_appserver.py")&&args==["app.yaml"])
        );
        let console = ConfigurationSpec::new(ConfigurationType::ConsoleFile)
            .materialize(&RunConfig::default())
            .unwrap();
        assert!(console.run_with_console && matches!(console.kind, RunConfigKind::Python { .. }));
    }
    #[test]
    fn compound_graph_is_resolved_and_validated_before_any_member_launch() {
        let child = new_config(ConfigurationType::Python, Path::new("/project"), &[]);
        let mut group = new_config(ConfigurationType::Compound, Path::new("/project"), &[]);
        group.name = "group".into();
        group.type_options.as_mut().unwrap().configurations =
            vec![child.name.clone(), child.name.clone()];
        assert_eq!(
            compound_members("group", &[group.clone(), child.clone()])
                .unwrap()
                .len(),
            1
        );
        group
            .type_options
            .as_mut()
            .unwrap()
            .configurations
            .push("missing".into());
        assert!(compound_members("group", &[group.clone(), child]).is_err());
        group.type_options.as_mut().unwrap().configurations = vec!["group".into()];
        assert!(compound_members("group", &[group]).is_err());
    }
    #[test]
    fn typed_options_and_console_preferences_roundtrip_with_old_defaults() {
        let old: RunConfig = serde_json::from_str("{}").unwrap();
        assert!(old.type_options.is_none());
        assert!(old.show_console_on_stderr && !old.show_console_on_stdout);
        assert_eq!(old.console_encoding, "utf-8");
        let mut cfg = new_config(
            ConfigurationType::PythonDebugServer,
            Path::new("/project"),
            &[],
        );
        let spec = cfg.type_options.as_mut().unwrap();
        spec.pydevd = true;
        spec.path_mappings.insert("/local".into(), "/remote".into());
        let restored: RunConfig =
            serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(restored, cfg);
    }
}

#[cfg(test)]
mod docker_resource_tests {
    use super::*;
    fn policy() -> crate::interpreter_remote::DockerResources {
        crate::interpreter_remote::DockerResources {
            cpuset_cpus: "2,4".into(),
            cpu_millis: 2000,
            threads: 2,
            nice: 10,
            nice_program: "/usr/bin/nice".into(),
            name: "owned".into(),
            labels: Default::default(),
        }
    }
    #[test]
    fn old_docker_configuration_defaults_and_serde_keep_commands_unchanged() {
        let spec: ConfigurationSpec = serde_json::from_value(
            serde_json::json!({"kind":"docker","target":"","image":"python:3.12"}),
        )
        .unwrap();
        assert!(spec.docker_resources.is_none());
        assert!(serde_json::to_value(&spec)
            .unwrap()
            .get("docker_resources")
            .is_none());
        let materialized = spec.materialize(&RunConfig::default()).unwrap();
        assert_eq!(
            materialized.kind,
            RunConfigKind::Program {
                program: "docker".into(),
                args: vec![
                    "run".into(),
                    "--rm".into(),
                    "--init".into(),
                    "python:3.12".into()
                ]
            }
        );
    }
    #[test]
    fn typed_docker_policy_materializes_once_and_owns_real_command_entrypoint() {
        let mut spec = ConfigurationSpec::new(ConfigurationType::Docker);
        spec.image = "python@sha256:owned".into();
        spec.target = "/usr/local/bin/python3".into();
        spec.executable = "/private/docker".into();
        spec.docker_resources = Some(policy());
        spec.arguments = vec![
            "--env".into(),
            "OMP_NUM_THREADS=99".into(),
            "--volume".into(),
            "/host:/work:ro".into(),
        ];
        let lowered = spec.materialize(&RunConfig::default()).unwrap();
        let RunConfigKind::Program { program, args } = lowered.kind else {
            panic!("expected Docker program")
        };
        assert_eq!(program, PathBuf::from("/private/docker"));
        assert_eq!(
            args.iter()
                .filter(|value| value.as_str() == "--cpu-quota")
                .count(),
            1
        );
        assert_eq!(
            args.iter()
                .filter(|value| value.as_str() == "--entrypoint")
                .count(),
            1
        );
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--cpu-quota", "200000"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--entrypoint", "/usr/bin/nice"]));
        assert!(args.ends_with(&[
            spec.image.clone(),
            "-n".into(),
            "10".into(),
            "--".into(),
            spec.target.clone()
        ]));
        assert!(
            args.iter().position(|value| value == "OMP_NUM_THREADS=2")
                > args.iter().position(|value| value == "OMP_NUM_THREADS=99")
        );
        let roundtrip: ConfigurationSpec =
            serde_json::from_str(&serde_json::to_string(&spec).unwrap()).unwrap();
        assert_eq!(roundtrip, spec);
    }
    #[test]
    fn explicit_docker_policy_refuses_unknown_options_missing_commands_and_uncapped_builds() {
        for arguments in [
            vec!["run", "--unknown", "value", "image", "python"],
            vec!["create", "--volume", "--network", "image", "python"],
            vec!["run", "image"],
            vec!["run", "--entrypoint=/bin/sh", "image", "python"],
            vec!["run", "--cpus", "99", "image", "python"],
        ] {
            assert!(policy()
                .apply_container_command(
                    &arguments.into_iter().map(String::from).collect::<Vec<_>>()
                )
                .is_err());
        }
        let mut spec = ConfigurationSpec::new(ConfigurationType::DockerImage);
        spec.image = "owned".into();
        spec.docker_resources = Some(policy());
        assert!(spec
            .materialize(&RunConfig::default())
            .unwrap_err()
            .contains("image builds"));
    }
}

#[cfg(test)]
mod legacy_build_policy_tests {
    use super::*;
    fn policy() -> LegacyDockerBuildResources {
        LegacyDockerBuildResources {
            cpuset_cpus: "0,1".into(),
            cpu_millis: 2000,
        }
    }
    #[test]
    fn typed_build_uses_selected_project_for_default_and_relative_cwd() {
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir(project.path().join("subproject")).unwrap();
        let mut spec = ConfigurationSpec::new(ConfigurationType::DockerImage);
        spec.image = "owned:fixture".into();
        spec.legacy_docker_build_resources = Some(policy());
        for relative in [None, Some(PathBuf::from("subproject"))] {
            let cfg = RunConfig {
                working_dir: relative.clone(),
                ..Default::default()
            };
            assert!(
                spec.materialize(&cfg).is_err(),
                "rootless lowering must not guess IDE cwd"
            );
            let lowered = spec.materialize_at(&cfg, project.path()).unwrap();
            let expected = dunce::canonicalize(relative.as_ref().map_or_else(
                || project.path().to_path_buf(),
                |path| project.path().join(path),
            ))
            .unwrap();
            assert_eq!(lowered.working_dir.as_ref(), Some(&expected));
            let RunConfigKind::Program { args, .. } = lowered.kind else {
                panic!()
            };
            assert_eq!(args.last().unwrap(), expected.to_str().unwrap());
        }
    }
    #[test]
    fn local_context_is_verified_against_captured_directory() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("context")).unwrap();
        let expected = dunce::canonicalize(root.path().join("context")).unwrap();
        let args = vec!["build".into(), "context".into()];
        let relative = policy().apply_at(&args, root.path()).unwrap();
        assert_eq!(relative.last().unwrap(), expected.to_str().unwrap());
        let absolute = policy()
            .apply(&["build".into(), expected.to_str().unwrap().into()])
            .unwrap();
        assert_eq!(relative, absolute);
        for remote in [
            "https://example.invalid/repo.git",
            "github.com/owner/repo.git",
            "otheruser@host:path",
            "git@host:repo",
            "-",
            "missing",
        ] {
            assert!(
                policy()
                    .apply_at(&["build".into(), remote.into()], root.path())
                    .is_err(),
                "accepted {remote}"
            );
        }
        std::fs::write(root.path().join("file"), "not a directory").unwrap();
        assert!(policy()
            .apply_at(&["build".into(), "file".into()], root.path())
            .is_err());
        assert!(policy()
            .apply_at(&args, Path::new("relative-owner"))
            .is_err());
        assert!(
            policy().apply(&args).is_err(),
            "unbound relative context must not use ambient cwd"
        );
    }
    #[test]
    fn typed_build_captures_legacy_engine_and_rejects_overrides() {
        let mut spec = ConfigurationSpec::new(ConfigurationType::DockerImage);
        spec.target = "Dockerfile".into();
        spec.image = "owned:fixture".into();
        spec.legacy_docker_build_resources = Some(policy());
        let root = tempfile::tempdir().unwrap();
        let cfg = RunConfig {
            type_options: Some(spec.clone()),
            working_dir: Some(root.path().into()),
            ..Default::default()
        };
        let mut override_cfg = cfg.clone();
        override_cfg.parameters = "--network=host".into();
        assert!(spec.materialize(&override_cfg).is_err());
        let lowered = spec.materialize(&cfg).unwrap();
        assert_eq!(
            lowered.env.get("DOCKER_BUILDKIT").map(String::as_str),
            Some("0")
        );
        let RunConfigKind::Program { args, .. } = lowered.kind else {
            panic!()
        };
        assert!(args.windows(2).any(|a| a == ["--cpu-quota", "200000"]));
        assert!(
            policy().apply(&args).is_err(),
            "policy cannot be applied twice"
        );
        for option in ["--cpu-quota", "--network", "--pull", "--unknown"] {
            assert!(policy()
                .apply(&["build".into(), option.into(), "override".into(), ".".into()])
                .is_err());
        }
        assert!(policy().apply(&["build".into()]).is_err());
        assert!(policy()
            .apply(&["build".into(), "https://example.invalid/context".into()])
            .is_err());
    }
}
