//! Explicit remote discovery: only configured, admitted targets are executed.
use super::interpreter::{InterpreterMetadata, InvalidInterpreter};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteKind {
    Wsl,
    Ssh,
    Docker,
    Compose,
    Vagrant,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteCandidate {
    pub kind: RemoteKind,
    pub target: String,
    pub python: String,
    /// Compose/Vagrant commands must run against this explicitly selected project.
    pub project_root: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker: Option<DockerTransport>,
    /// Path mapping: the remote directory in which `project_root` is visible
    /// (a Compose volume, a Vagrant synced folder, an SSH checkout). Docker image
    /// interpreters mount the project there, `/work` when unset. Arguments and the
    /// working directory under `project_root` are translated before launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_root: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DockerTransport {
    pub executable: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<DockerResources>,
}
/// Optional daemon-side limits. Host CLI affinity does not constrain containers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DockerResources {
    pub cpuset_cpus: String,
    pub cpu_millis: u32,
    pub threads: u16,
    pub nice: u8,
    pub nice_program: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub labels: std::collections::BTreeMap<String, String>,
}
impl DockerResources {
    pub fn validate(&self) -> Result<(), String> {
        let mut cpus = std::collections::BTreeSet::new();
        if self.cpuset_cpus.len() > 4096 {
            return Err("CPU set exceeds limit".into());
        }
        for part in self.cpuset_cpus.split(',') {
            let mut bounds = part.split('-');
            let start = bounds
                .next()
                .unwrap_or_default()
                .parse::<u32>()
                .map_err(|_| "Choose an explicit CPU set")?;
            let end = bounds
                .next()
                .map(str::parse::<u32>)
                .transpose()
                .map_err(|_| "Invalid CPU range")?
                .unwrap_or(start);
            if bounds.next().is_some() || end < start || end - start > 1023 {
                return Err("Invalid CPU range".into());
            }
            cpus.extend(start..=end);
            if cpus.len() > 1024 {
                return Err("CPU set exceeds limit".into());
            }
        }
        if self.cpu_millis == 0
            || u64::from(self.cpu_millis) > cpus.len() as u64 * 1000
            || self.threads == 0
            || self.nice > 19
        {
            return Err(
                "CPU quota, thread count or nice adjustment is outside configured limits".into(),
            );
        }
        if !self.nice_program.starts_with('/') || self.nice_program.chars().any(char::is_control) {
            return Err("Choose an absolute in-container nice executable".into());
        }
        if !self.name.is_empty()
            && (!self.name.starts_with(|c: char| c.is_ascii_alphanumeric())
                || !self
                    .name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')))
        {
            return Err("Invalid configured container name".into());
        }
        if self.labels.iter().any(|(key, value)| {
            key.is_empty()
                || key.contains('=')
                || key.chars().chain(value.chars()).any(char::is_control)
        }) {
            return Err("Invalid configured container label".into());
        }
        Ok(())
    }
    pub fn environment_args(&self) -> Vec<String> {
        [
            "OMP_NUM_THREADS",
            "OMP_THREAD_LIMIT",
            "OPENBLAS_NUM_THREADS",
            "MKL_NUM_THREADS",
            "NUMEXPR_NUM_THREADS",
            "VECLIB_MAXIMUM_THREADS",
            "BLIS_NUM_THREADS",
            "RAYON_NUM_THREADS",
            "RUST_TEST_THREADS",
            "PYTHON_CPU_COUNT",
        ]
        .into_iter()
        .flat_map(|key| ["--env".into(), format!("{key}={}", self.threads)])
        .collect()
    }
    pub fn container_args(&self) -> Result<Vec<String>, String> {
        self.validate()?;
        let mut args = vec![
            "--cpuset-cpus".into(),
            self.cpuset_cpus.clone(),
            "--cpu-period".into(),
            "100000".into(),
            "--cpu-quota".into(),
            (u64::from(self.cpu_millis) * 100).to_string(),
        ];
        args.extend(self.environment_args());
        if !self.name.is_empty() {
            args.extend(["--name".into(), self.name.clone()]);
        }
        for (key, value) in &self.labels {
            args.extend(["--label".into(), format!("{key}={value}")]);
        }
        Ok(args)
    }
    pub fn process_prefix(&self) -> Result<Vec<String>, String> {
        self.validate()?;
        Ok(vec![
            self.nice_program.clone(),
            "-n".into(),
            self.nice.to_string(),
            "--".into(),
        ])
    }
}
/// Stands for the mapped working directory computed by the adapter at launch.
/// NUL never survives candidate validation, so it cannot collide with a value.
const WORKDIR: &str = "\0workdir";
/// Interpreter environment that `docker run` / `docker exec` do not inherit.
/// Without it a run's output stays block-buffered until the process exits.
const FORWARDED_ENVIRONMENT: [&str; 4] = [
    "PYTHONUNBUFFERED",
    "PYTHONIOENCODING",
    "PYTHONUTF8",
    "PYTHONDONTWRITEBYTECODE",
];
fn docker_run_prefix(candidate: &RemoteCandidate) -> Result<Vec<String>, String> {
    let mut args = vec!["run".into(), "--rm".into(), "-i".into()];
    let resources = candidate
        .docker
        .as_ref()
        .and_then(|docker| docker.resources.as_ref());
    if let Some(resources) = resources {
        args.extend(resources.container_args()?);
    }
    for name in FORWARDED_ENVIRONMENT {
        args.extend(["--env".into(), name.into()]);
    }
    let remote = candidate.mapped_root().unwrap_or_else(|| "/work".into());
    args.extend([
        "-v".into(),
        format!("{}:{remote}", candidate.project_root.display()),
        "-w".into(),
        WORKDIR.into(),
        candidate.target.clone(),
    ]);
    if let Some(resources) = resources {
        args.extend(resources.process_prefix()?);
    }
    Ok(args)
}
// Compose owns container CPU limits in its captured service definition. These
// exec options constrain the actual interpreter process without creating a service.
fn compose_exec_prefix(candidate: &RemoteCandidate) -> Result<Vec<String>, String> {
    let mut args = vec!["compose".into(), "exec".into(), "-T".into()];
    let resources = candidate
        .docker
        .as_ref()
        .and_then(|transport| transport.resources.as_ref());
    if let Some(resources) = resources {
        resources.validate()?;
        args.extend(resources.environment_args());
    }
    for name in FORWARDED_ENVIRONMENT {
        args.extend(["--env".into(), name.into()]);
    }
    if candidate.mapped_root().is_some() {
        args.extend(["--workdir".into(), WORKDIR.into()]);
    }
    args.extend(["--".into(), candidate.target.clone()]);
    if let Some(resources) = resources {
        args.extend(resources.process_prefix()?);
    }
    Ok(args)
}
impl RemoteCandidate {
    pub fn label(&self) -> String {
        format!("{:?}: {}", self.kind, self.target)
    }
    /// The remote directory mapped to `project_root`, if any.
    pub fn mapped_root(&self) -> Option<String> {
        match &self.remote_root {
            Some(root) if !root.trim().is_empty() => {
                let trimmed = root.trim().trim_end_matches('/');
                Some(if trimmed.is_empty() {
                    "/".into()
                } else {
                    trimmed.into()
                })
            }
            _ => (self.kind == RemoteKind::Docker).then(|| "/work".into()),
        }
    }
    /// The remote spelling of a local path under `project_root` (path mapping).
    pub fn map_path(&self, local: &Path) -> Option<String> {
        let remote = self.mapped_root()?;
        let relative = local.strip_prefix(&self.project_root).ok()?;
        let mut mapped = remote.trim_end_matches('/').to_string();
        for part in relative.components() {
            mapped.push('/');
            mapped.push_str(&part.as_os_str().to_string_lossy());
        }
        Some(if mapped.is_empty() {
            "/".into()
        } else {
            mapped
        })
    }
    pub fn validate(&self) -> Result<(), String> {
        if let Some(remote) = &self.remote_root {
            if !remote.trim().is_empty()
                && (!remote.trim().starts_with('/')
                    || remote.chars().any(char::is_control)
                    || (matches!(self.kind, RemoteKind::Docker) && remote.contains([':', ','])))
            {
                return Err("The remote project path must be an absolute POSIX path".into());
            }
        }
        for value in [&self.target, &self.python] {
            if value.contains(['\0', '\n', '\r']) {
                return Err("Target and Python must be single-line values".into());
            }
        }
        if self.python.trim().is_empty() || self.python.starts_with('-') {
            return Err("Choose a Python executable".into());
        }
        if self.target.starts_with('-') || (self.target.is_empty() && self.kind != RemoteKind::Wsl)
        {
            return Err("Choose a target".into());
        }
        if !self.project_root.is_absolute() {
            return Err("Choose an absolute project directory".into());
        }
        if let Some(docker) = &self.docker {
            if !matches!(self.kind, RemoteKind::Docker | RemoteKind::Compose)
                || !docker.executable.is_absolute()
                || !docker.executable.is_file()
                || docker
                    .executable
                    .to_string_lossy()
                    .chars()
                    .any(char::is_control)
            {
                return Err(
                    "Docker transport requires its installed absolute CLI and a Docker or Compose target"
                        .into(),
                );
            }
            if let Some(resources) = &docker.resources {
                resources.validate()?;
            }
        }
        Ok(())
    }
}

/// The caller supplies the admitted transport. Enumeration never opens a host,
/// starts a container, reads SSH config, or guesses a Vagrant machine.
pub fn discover_configured(
    configured: &[RemoteCandidate],
    trusted: bool,
    mut admitted_transport: impl FnMut(&RemoteCandidate) -> Result<InterpreterMetadata, String>,
) -> Vec<Result<InterpreterMetadata, InvalidInterpreter>> {
    configured
        .iter()
        .take(64)
        .map(|candidate| {
            let result = candidate.validate().and_then(|_| {
                if !trusted {
                    return Err("Project trust is required".into());
                }
                admitted_transport(candidate)
            });
            result.map_err(|reason| InvalidInterpreter {
                path: PathBuf::from(candidate.label()),
                kind: "remote".into(),
                reason,
            })
        })
        .collect()
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

// Four literal backslashes survive shell double-quote parsing as two, so sed
// emits the single backslash required by the remote shell's apostrophe escape.
const REMOTE_QUOTE_FUNCTION: &str = r#"remote_quote() { printf "'"; printf '%s' "$1" | sed "s/'/'\\\\''/g"; printf "'"; }
"#;

/// POSIX shell fragment translating the launch directory and every argument
/// under `host` (a run's script, test targets, `--cov` paths) to `remote`.
/// It sets `workdir` and rewrites `"$@"` in place without word splitting.
pub fn path_mapping_script(host: &Path, remote: &str) -> String {
    let host = host.to_string_lossy();
    let host = host.trim_end_matches('/');
    let remote_root = if remote.is_empty() { "/" } else { remote };
    let remote_prefix = remote_root.trim_end_matches('/');
    format!(
        "host={}\nremote_root={}\nremote_prefix={}\nworkdir=$remote_root\ncase $caller in \"$host\"/*) workdir=$remote_prefix${{caller#\"$host\"}};; esac\nfor arg do\n  shift\n  case $arg in\n    \"$host\") arg=$remote_root;;\n    \"$host\"/*) arg=$remote_prefix${{arg#\"$host\"}};;\n  esac\n  set -- \"$@\" \"$arg\"\ndone\n",
        quote(host),
        quote(remote_root),
        quote(remote_prefix)
    )
}

/// A wrapper is the admitted transport adapter used by run, console and probes.
/// No remote connection is performed while constructing it. Arguments remain
/// arguments even when targets, project paths or Python paths contain spaces.
pub fn install_wrapper(candidate: &RemoteCandidate, directory: &Path) -> Result<PathBuf, String> {
    candidate.validate()?;
    if cfg!(windows) {
        return install_windows_wrapper(candidate, directory);
    }
    let (tool, prefix, shell_remote): (&str, Vec<String>, bool) = match candidate.kind {
        RemoteKind::Ssh => (
            "ssh",
            vec![
                "-o".into(),
                "BatchMode=yes".into(),
                "-o".into(),
                "ConnectTimeout=5".into(),
                "-o".into(),
                "StrictHostKeyChecking=yes".into(),
                "--".into(),
                candidate.target.clone(),
            ],
            true,
        ),
        RemoteKind::Docker => ("docker", docker_run_prefix(candidate)?, false),
        RemoteKind::Compose => ("docker", compose_exec_prefix(candidate)?, false),
        RemoteKind::Wsl => (
            "wsl.exe",
            if candidate.target.is_empty() {
                vec!["--exec".into()]
            } else {
                vec![
                    "--distribution".into(),
                    candidate.target.clone(),
                    "--exec".into(),
                ]
            },
            false,
        ),
        RemoteKind::Vagrant => (
            "vagrant",
            vec!["ssh".into(), candidate.target.clone(), "-c".into()],
            true,
        ),
    };
    let executable = if let Some(docker) = &candidate.docker {
        docker.executable.clone()
    } else {
        which::which(tool).map_err(|_| format!("Required tool {tool} is not installed"))?
    };
    let mut script = format!(
        "#!/bin/sh\ncaller=$PWD\ncd -- {} || exit 125\n",
        quote(&candidate.project_root.to_string_lossy())
    );
    let mapped = candidate.mapped_root();
    if let Some(remote) = &mapped {
        script.push_str(&path_mapping_script(&candidate.project_root, remote));
    }
    let program = std::iter::once(executable.to_string_lossy().into_owned())
        .chain(prefix)
        .map(|v| {
            if v == WORKDIR {
                "\"$workdir\"".into()
            } else {
                quote(&v)
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    if shell_remote {
        // sed quotes each argv word for the one remote shell command ssh accepts.
        script.push_str(REMOTE_QUOTE_FUNCTION);
        // The remote login shell neither starts in the mapped directory nor
        // receives the interpreter environment of the run configuration.
        script.push_str("launch='exec'\n");
        if mapped.is_some() {
            script.push_str("launch=\"cd -- $(remote_quote \"$workdir\") && exec\"\n");
        }
        script.push_str(&format!(
            "assign=\nfor name in {}; do eval \"value=\\${{$name-}}\"; if [ -n \"$value\" ]; then assign=\"$assign $name=$(remote_quote \"$value\")\"; fi; done\nif [ -n \"$assign\" ]; then launch=\"$launch env$assign\"; fi\n",
            FORWARDED_ENVIRONMENT.join(" ")
        ));
        script.push_str(&format!("remote=\"$launch $(remote_quote {})\"\nfor arg do remote=\"$remote $(remote_quote \"$arg\")\"; done\nexec {program} \"$remote\"\n", quote(&candidate.python)));
    } else {
        script.push_str(&format!(
            "exec {program} {} \"$@\"\n",
            quote(&candidate.python)
        ));
    }
    use sha2::{Digest, Sha256};
    let key = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(candidate).map_err(|e| e.to_string())?)
    );
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let wrapper = directory.join(format!("remote-{key}.sh"));
    std::fs::write(&wrapper, script).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    std::fs::write(
        directory.join(format!("remote-{key}.json")),
        serde_json::to_vec(candidate).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(wrapper)
}

/// Read only explicitly configured targets belonging to this project.
pub fn configured_candidates(directory: &Path, root: &Path) -> Vec<RemoteCandidate> {
    let mut files: Vec<_> = std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .take(4096)
        .map(|e| e.path())
        .collect();
    files.sort();
    files
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter_map(|p| {
            let bytes = std::fs::read(p).ok()?;
            if bytes.len() > 65536 {
                return None;
            }
            let candidate: RemoteCandidate = serde_json::from_slice(&bytes).ok()?;
            (candidate.project_root == root && candidate.validate().is_ok()).then_some(candidate)
        })
        .take(64)
        .collect()
}

/// One outstanding explicit probe per process. Dropping the dialog revokes
/// delivery; a timed subprocess already in flight is reaped at its probe timeout
/// (eight seconds locally, `REMOTE_PROBE_TIMEOUT` through a remote transport).
pub struct ProbeJob<T> {
    pub receiver: crossbeam_channel::Receiver<T>,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl<T> Drop for ProbeJob<T> {
    fn drop(&mut self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
    }
}
static EXPLICIT_PROBE_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
impl<T: Send + 'static> ProbeJob<T> {
    fn start(work: impl FnOnce() -> T + Send + 'static) -> Result<Self, String> {
        use std::sync::atomic::Ordering;
        EXPLICIT_PROBE_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "An interpreter probe is already running".to_string())?;
        let (tx, receiver) = crossbeam_channel::bounded(1);
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let token = cancelled.clone();
        let spawn = std::thread::Builder::new()
            .name("interpreter-probe".into())
            .spawn(move || {
                struct Active;
                impl Drop for Active {
                    fn drop(&mut self) {
                        EXPLICIT_PROBE_ACTIVE.store(false, Ordering::Release);
                    }
                }
                let _active = Active;
                if token.load(Ordering::Acquire) {
                    return;
                }
                let result = work();
                if !token.load(Ordering::Acquire) {
                    let _ = tx.send(result);
                }
            });
        if let Err(error) = spawn {
            EXPLICIT_PROBE_ACTIVE.store(false, Ordering::Release);
            return Err(error.to_string());
        }
        Ok(Self {
            receiver,
            cancelled,
        })
    }
}
pub type DiscoveryJob = ProbeJob<Result<InterpreterMetadata, String>>;
pub type WslRegistrationJob = ProbeJob<Result<Vec<String>, String>>;
impl ProbeJob<Result<Vec<String>, String>> {
    pub fn registrations() -> Result<Self, String> {
        Self::start(registered_wsl_distributions)
    }
}
impl ProbeJob<Result<InterpreterMetadata, String>> {
    pub fn local(path: PathBuf, trusted: bool) -> Result<Self, String> {
        if !trusted {
            return Err("Project trust is required to execute an interpreter".into());
        }
        Self::start(move || {
            super::interpreter::probe_detailed(&path, "custom").map_err(|e| e.label())
        })
    }
    pub fn remote(
        candidate: RemoteCandidate,
        directory: PathBuf,
        trusted: bool,
    ) -> Result<Self, String> {
        if !trusted {
            return Err("Project trust is required to execute a remote interpreter".into());
        }
        candidate.validate()?;
        Self::start(move || {
            discover_configured(std::slice::from_ref(&candidate), true, |admitted| {
                let path = install_wrapper(admitted, &directory)?;
                super::interpreter::probe_detailed_with_timeout(
                    &path,
                    &admitted.label(),
                    super::interpreter::REMOTE_PROBE_TIMEOUT,
                )
                .map_err(|e| e.label())
            })
            .pop()
            .ok_or("No configured interpreter")?
            .map_err(|e| e.label())
        })
    }
}

fn install_windows_wrapper(
    candidate: &RemoteCandidate,
    directory: &Path,
) -> Result<PathBuf, String> {
    let adapter_python = ["python.exe", "python3.exe", "py.exe"]
        .iter()
        .find_map(|p| which::which(p).ok())
        .ok_or("A local Python is required for the Windows remote transport adapter")?;
    if adapter_python.starts_with(&candidate.project_root) {
        return Err("The transport adapter must be installed outside the project".into());
    }
    let tool = match candidate.kind {
        RemoteKind::Wsl => "wsl.exe",
        RemoteKind::Ssh => "ssh.exe",
        RemoteKind::Docker | RemoteKind::Compose => "docker.exe",
        RemoteKind::Vagrant => "vagrant.exe",
    };
    let tool = if let Some(docker) = &candidate.docker {
        docker.executable.clone()
    } else {
        which::which(tool).map_err(|_| format!("Required tool {tool} is not installed"))?
    };
    use sha2::{Digest, Sha256};
    let bytes = serde_json::to_vec(candidate).map_err(|e| e.to_string())?;
    let key = format!("{:x}", Sha256::digest(&bytes));
    std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    let adapter = directory.join(format!("remote-{key}.py"));
    let config = serde_json::json!({"tool":tool, "candidate":candidate, "docker_prefix":docker_run_prefix(candidate)?, "compose_prefix":compose_exec_prefix(candidate)?, "mapped_root":candidate.mapped_root(), "workdir":WORKDIR, "forwarded":FORWARDED_ENVIRONMENT});
    let config_literal = serde_json::to_string(&config.to_string()).map_err(|e| e.to_string())?;
    let script = format!(
        r#"import json, ntpath, os, shlex, subprocess, sys
config = json.loads({config_literal})
c = config['candidate']
kind = c['kind']
target = c['target']
remote_root = config['mapped_root']
host = c['project_root'].rstrip('\\/')
def mapped(value):
    if remote_root is None:
        return value
    if ntpath.normcase(value) == ntpath.normcase(host):
        return remote_root
    head = value[:len(host) + 1]
    if ntpath.normcase(head[:-1]) == ntpath.normcase(host) and head[-1:] in ('\\', '/'):
        return remote_root.rstrip('/') + '/' + value[len(host) + 1:].replace('\\', '/')
    return value
workdir = mapped(os.getcwd()) if remote_root is not None else None
if workdir is not None and workdir == os.getcwd():
    workdir = remote_root
python = [c['python']] + [mapped(a) for a in sys.argv[1:]]
def remote_command():
    launch = ['cd', '--', workdir, '&&'] if workdir is not None else []
    assign = [name + '=' + os.environ[name] for name in config['forwarded'] if os.environ.get(name)]
    words = [shlex.quote(w) for w in python]
    prefix = (shlex.join(launch[:3]) + ' && ' if launch else '') + 'exec ' + ('env ' + shlex.join(assign) + ' ' if assign else '')
    return prefix + ' '.join(words)
def resolved(prefix):
    return [workdir if a == config['workdir'] else a for a in prefix]
if kind == 'wsl':
    args = (['--distribution', target] if target else []) + ['--exec'] + python
elif kind == 'ssh':
    args = ['-o', 'BatchMode=yes', '-o', 'ConnectTimeout=5', '-o', 'StrictHostKeyChecking=yes', '--', target, remote_command()]
elif kind == 'docker':
    args = resolved(config['docker_prefix']) + python
elif kind == 'compose':
    args = resolved(config['compose_prefix']) + python
elif kind == 'vagrant':
    args = ['ssh', target, '-c', remote_command()]
else:
    raise SystemExit('Unsupported interpreter transport')
raise SystemExit(subprocess.call([config['tool']] + args, cwd=c['project_root']))
"#
    );
    std::fs::write(&adapter, script).map_err(|e| e.to_string())?;
    // The .cmd stays for external consumers (a path users can paste into a
    // terminal). RustPy itself never spawns it: `interpreter_command`
    // recognises this exact format and starts the adapter Python directly.
    let wrapper = directory.join(format!("remote-{key}.cmd"));
    std::fs::write(
        &wrapper,
        windows_wrapper_contents(&adapter_python, &adapter)?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(directory.join(format!("remote-{key}.json")), bytes)
        .map_err(|e| e.to_string())?;
    Ok(wrapper)
}

/// The batch wrapper text for a Windows remote interpreter. cmd's percent
/// expansion applies to the generated command itself; all externally supplied
/// target values live in JSON, never batch syntax. Must stay in the format
/// `rustpy_core::interpreter_launch::parse_remote_wrapper` recognises.
fn windows_wrapper_contents(adapter_python: &Path, adapter: &Path) -> Result<String, String> {
    let batch_quote = |p: &Path| -> Result<String, String> {
        let text = p.to_str().ok_or("Unsupported adapter path")?;
        if text.contains(['"', '\n', '\r']) {
            return Err("Unsupported adapter path".into());
        }
        Ok(format!("\"{}\"", text.replace('%', "%%")))
    };
    Ok(format!(
        "@echo off\r\n{} -I {} %*\r\n",
        batch_quote(adapter_python)?,
        batch_quote(adapter)?
    ))
}

/// Windows emits distribution names as UTF-16LE; this lists registrations only,
/// never starts a distribution or probes a guessed target.
pub fn registered_wsl_distributions() -> Result<Vec<String>, String> {
    if !cfg!(windows) {
        return Err("WSL registration discovery is available on Windows".into());
    }
    let tool = which::which("wsl.exe").map_err(|_| "WSL is not installed")?;
    let bytes = crate::probe::probe_output(
        &tool,
        &["--list", "--quiet"],
        std::time::Duration::from_secs(3),
    )
    .ok_or("Could not list WSL distributions")?;
    let text = if bytes.contains(&0) {
        String::from_utf16_lossy(
            &bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect::<Vec<_>>(),
        )
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    };
    Ok(text
        .lines()
        .map(|l| l.trim().trim_matches('\u{feff}').to_string())
        .filter(|s| !s.is_empty())
        .take(64)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_windows_wrapper_is_recognised_and_bypasses_cmd() {
        use rustpy_core::interpreter_launch::parse_remote_wrapper;
        let key = "ab".repeat(32);
        let directory = r"C:\Users\A 100%\AppData\RustPy\adapters";
        let python = Path::new(r"C:\Program Files\Python 3.12 (x%PATH%)\python.exe");
        let adapter = format!(r"{directory}\remote-{key}.py");
        let text = windows_wrapper_contents(python, Path::new(&adapter)).unwrap();
        let launch = parse_remote_wrapper(&format!(r"{directory}\remote-{key}.cmd"), &text)
            .expect("the generated wrapper must be recognised");
        assert_eq!(launch.python, python);
        assert_eq!(launch.adapter, Path::new(&adapter));
        assert!(windows_wrapper_contents(Path::new("C:\\a\"b\\python.exe"), python).is_err());
    }
    #[test]
    fn legacy_remote_records_remain_unchanged_and_invalid_resource_limits_refuse() {
        let legacy = serde_json::json!({"kind":"docker","target":"local-python","python":"/usr/local/bin/python","project_root":"/work"});
        let parsed: RemoteCandidate = serde_json::from_value(legacy.clone()).unwrap();
        assert!(parsed.docker.is_none());
        assert_eq!(serde_json::to_value(parsed).unwrap(), legacy);
        let policy = DockerResources {
            cpuset_cpus: "0,1".into(),
            cpu_millis: 2000,
            threads: 2,
            nice: 10,
            nice_program: "/usr/bin/nice".into(),
            name: String::new(),
            labels: Default::default(),
        };
        assert!(policy.validate().is_ok());
        for cpuset in [
            "",
            "0,",
            "0-",
            "2-1",
            "0-1-2",
            "0;touch /tmp/unowned",
            "0-999999",
        ] {
            let mut invalid = policy.clone();
            invalid.cpuset_cpus = cpuset.into();
            assert!(
                invalid.container_args().is_err(),
                "reject {cpuset:?} before any launch"
            );
        }
        let mut invalid = policy.clone();
        invalid.cpu_millis = 2001;
        assert!(
            invalid.container_args().is_err(),
            "quota cannot exceed the selected CPU set"
        );
        let mut invalid = policy;
        invalid.nice = 20;
        assert!(
            invalid.process_prefix().is_err(),
            "invalid priority cannot silently become uncapped execution"
        );
    }
    fn candidate(kind: RemoteKind) -> RemoteCandidate {
        RemoteCandidate {
            kind,
            target: "configured-target".into(),
            python: "/opt/Python Space/bin/python".into(),
            project_root: std::env::current_dir().unwrap(),
            docker: None,
            remote_root: None,
        }
    }
    #[test]
    fn untrusted_and_malformed_candidates_never_reach_transport() {
        let calls = std::cell::Cell::new(0);
        let candidate = candidate(RemoteKind::Ssh);
        let results = discover_configured(std::slice::from_ref(&candidate), false, |_| {
            calls.set(calls.get() + 1);
            Err("should not execute".into())
        });
        assert_eq!(calls.get(), 0);
        assert!(results[0].as_ref().unwrap_err().reason.contains("trust"));
        let mut bad = candidate;
        bad.target = "-oProxyCommand=unexpected".into();
        assert!(discover_configured(&[bad], true, |_| {
            calls.set(1);
            Err("should not execute".into())
        })[0]
            .is_err());
        assert_eq!(calls.get(), 0);
    }
    #[test]
    fn explicit_transport_receives_each_named_provider_and_preserves_failures() {
        let kinds = [
            RemoteKind::Wsl,
            RemoteKind::Ssh,
            RemoteKind::Docker,
            RemoteKind::Compose,
            RemoteKind::Vagrant,
        ];
        let candidates: Vec<_> = kinds.iter().cloned().map(candidate).collect();
        let mut received = Vec::new();
        let results = discover_configured(&candidates, true, |candidate| {
            received.push(candidate.kind.clone());
            Err("tool unavailable".into())
        });
        assert_eq!(received, kinds);
        assert!(results
            .iter()
            .all(|r| r.as_ref().unwrap_err().reason == "tool unavailable"));
    }
    #[test]
    fn configuration_listing_is_project_scoped_and_does_not_execute() {
        let temp = tempfile::tempdir().unwrap();
        let candidate = candidate(RemoteKind::Compose);
        std::fs::write(
            temp.path().join("configured.json"),
            serde_json::to_vec(&candidate).unwrap(),
        )
        .unwrap();
        std::fs::write(temp.path().join("malformed.json"), "not json").unwrap();
        assert_eq!(
            configured_candidates(temp.path(), &candidate.project_root),
            vec![candidate]
        );
        assert!(
            configured_candidates(temp.path(), &temp.path().join("different-project")).is_empty()
        );
    }
    #[cfg(unix)]
    #[test]
    fn generated_remote_quote_preserves_shell_argv_and_blocks_injection() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("owned 'π' argv.sh");
        std::fs::write(&executable, "#!/bin/sh\nprintf '%s\\000' \"$@\"\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let values = [
            "",
            "'",
            "O'Reilly",
            "λ 😀",
            "a\\b\"c",
            "two\nlines\n",
            "$(touch INJECTED)",
            "`touch INJECTED`",
            "'; touch INJECTED; '",
            " $HOME ; * ? [x] | & < > ",
        ];
        // Exercise the exact helper emitted into SSH/Vagrant wrappers and the
        // same one-command argv construction, using only an actual local shell.
        let script = format!(
            "{REMOTE_QUOTE_FUNCTION}remote=$(remote_quote \"$1\")\nshift\nfor arg do remote=\"$remote $(remote_quote \"$arg\")\"; done\nexec /bin/sh -c \"$remote\"\n"
        );
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", &script, "owned-quote-regression"])
            .arg(&executable)
            .args(values)
            .current_dir(directory.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "actual shell failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected: Vec<u8> = values
            .iter()
            .flat_map(|value| value.bytes().chain(std::iter::once(0)))
            .collect();
        assert_eq!(
            output.stdout, expected,
            "remote shell must receive exact argv bytes"
        );
        assert!(!directory.path().join("INJECTED").exists());
    }

    #[cfg(unix)]
    #[test]
    fn docker_and_compose_wrappers_map_project_paths_and_working_directory() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let cli = directory.path().join("fake docker");
        std::fs::write(&cli, "#!/bin/sh\nprintf '%s\\000' \"$@\"\n").unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        let project = directory.path().join("proj 'π'");
        std::fs::create_dir_all(project.join("sub")).unwrap();
        let run = |candidate: &RemoteCandidate, cwd: &Path| -> Vec<String> {
            let wrapper = install_wrapper(candidate, &directory.path().join("adapters")).unwrap();
            let output = std::process::Command::new(wrapper)
                .args([
                    project.join("sub/main.py").to_str().unwrap(),
                    project.to_str().unwrap(),
                    &format!("{}-other/x", project.display()),
                    "two\nlines\n",
                ])
                .current_dir(cwd)
                .env("PYTHONUNBUFFERED", "1")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            String::from_utf8(output.stdout)
                .unwrap()
                .split_terminator('\0')
                .map(str::to_owned)
                .collect()
        };
        let mut candidate = RemoteCandidate {
            kind: RemoteKind::Docker,
            target: "image:tag".into(),
            python: "python3".into(),
            project_root: project.clone(),
            docker: Some(DockerTransport {
                executable: cli,
                resources: None,
            }),
            remote_root: None,
        };
        let argv = run(&candidate, &project.join("sub"));
        let tail = [
            "image:tag".to_string(),
            "python3".into(),
            "/work/sub/main.py".into(),
            "/work".into(),
            format!("{}-other/x", project.display()),
            "two\nlines\n".into(),
        ];
        assert!(argv.ends_with(&tail), "{argv:?}");
        assert!(argv.contains(&format!("{}:/work", project.display())));
        let workdir = argv.iter().position(|a| a == "-w").unwrap();
        assert_eq!(argv[workdir + 1], "/work/sub");
        assert!(argv.windows(2).any(|w| w == ["--env", "PYTHONUNBUFFERED"]));
        // Launched outside the project, the container starts at the mapped root.
        let argv = run(&candidate, directory.path());
        assert_eq!(
            argv[argv.iter().position(|a| a == "-w").unwrap() + 1],
            "/work"
        );

        candidate.kind = RemoteKind::Compose;
        candidate.target = "api".into();
        candidate.remote_root = Some("/srv/app/".into());
        let argv = run(&candidate, &project.join("sub"));
        assert!(
            argv.windows(2).any(|w| w == ["--workdir", "/srv/app/sub"]),
            "{argv:?}"
        );
        assert!(argv.ends_with(&[
            "api".to_string(),
            "python3".into(),
            "/srv/app/sub/main.py".into(),
            "/srv/app".into(),
            format!("{}-other/x", project.display()),
            "two\nlines\n".into(),
        ]));
        // Compose without a mapping keeps the service's own working directory.
        candidate.remote_root = None;
        let argv = run(&candidate, &project.join("sub"));
        assert!(!argv.iter().any(|a| a == "--workdir"));
        assert!(argv.contains(&project.join("sub/main.py").to_string_lossy().into_owned()));
        candidate.remote_root = Some("relative".into());
        assert!(candidate.validate().is_err());
        candidate.kind = RemoteKind::Docker;
        candidate.remote_root = Some("/a:/b".into());
        assert!(candidate.validate().is_err());
    }

    #[test]
    fn shell_quote_keeps_metacharacters_inside_one_argument() {
        assert_eq!(
            quote("a'b $HOME; $(touch marker)"),
            "'a'\\''b $HOME; $(touch marker)'"
        );
    }
}
