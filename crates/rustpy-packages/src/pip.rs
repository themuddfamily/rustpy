//! Background pip operations with streamed output.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PipTaskKind {
    Command { label: String },
    Install { spec: String },
    Uninstall { name: String },
    Upgrade { name: String },
    InstallRequirements { path: PathBuf },
}

impl PipTaskKind {
    /// Short description for the UI, e.g. `Installing requests`.
    pub fn label(&self) -> String {
        match self {
            PipTaskKind::Command { label } => label.clone(),
            PipTaskKind::Install { spec } => format!("Installing {spec}"),
            PipTaskKind::Uninstall { name } => format!("Uninstalling {name}"),
            PipTaskKind::Upgrade { name } => format!("Upgrading {name}"),
            PipTaskKind::InstallRequirements { path } => {
                format!("Installing requirements from {}", path.display())
            }
        }
    }
}

/// A running (or finished) `python -m pip …` process whose output is read in
/// the background. Poll it from the UI thread; drop or `stop` to terminate.
pub struct PipTask {
    pub kind: PipTaskKind,
    pub command_line: String,
    pub started: Instant,
    child: Option<Child>,
    rx: Receiver<String>,
    readers_done: bool,
    exit: Option<i32>,
    killed: bool,
}

impl PipTask {
    fn spawn(kind: PipTaskKind, interpreter: &Path, args: &[String]) -> Result<PipTask> {
        let manager = crate::package_manager(interpreter);
        let (cmd, command_line) = match &manager {
            crate::PackageManager::Uv { exe } => {
                let uv_args = crate::uv_pip_args(interpreter, args);
                let line = std::iter::once(exe.display().to_string())
                    .chain(uv_args.iter().cloned())
                    .collect::<Vec<_>>()
                    .join(" ");
                let mut c = Command::new(exe);
                c.args(&uv_args);
                (c, line)
            }
            crate::PackageManager::Pip => {
                let mut c = rustpy_core::interpreter_launch::interpreter_command(interpreter);
                c.arg("-u").arg("-m").arg("pip");
                c.args(args);
                c.arg("--no-input");
                let line = std::iter::once(interpreter.display().to_string())
                    .chain(["-m".to_string(), "pip".to_string()])
                    .chain(args.iter().cloned())
                    .chain(["--no-input".to_string()])
                    .collect::<Vec<_>>()
                    .join(" ");
                (c, line)
            }
        };
        Self::spawn_command(kind, cmd, command_line)
    }

    /// Runs a manager/task plan through the same streamed-output and process
    /// ownership path as ordinary pip installs. No shell is involved.
    pub fn spawn_command(
        kind: PipTaskKind,
        mut cmd: Command,
        command_line: String,
    ) -> Result<Self> {
        cmd.env("PIP_DISABLE_PIP_VERSION_CHECK", "1");
        cmd.env("PYTHONIOENCODING", "utf-8");
        cmd.env("PYTHONUNBUFFERED", "1");
        cmd.env("NO_COLOR", "1");
        cmd.env("TERM", "dumb");
        // A manual proxy (Settings › HTTP Proxy) reaches pip / uv as http(s)_proxy.
        cmd.envs(rustpy_net::proxy_env_vars());
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("cannot start {command_line}"))?;
        let (tx, rx) = channel::<String>();
        let streams: Vec<Box<dyn std::io::Read + Send>> = [
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn std::io::Read + Send>),
        ]
        .into_iter()
        .flatten()
        .collect();
        for stream in streams {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream);
                let mut buf = Vec::new();
                loop {
                    buf.clear();
                    match reader.read_until(b'\n', &mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            let text = String::from_utf8_lossy(&buf);
                            // Progress output uses carriage returns; keep the last segment.
                            let line = text
                                .trim_end_matches(['\n', '\r'])
                                .rsplit('\r')
                                .next()
                                .unwrap_or("")
                                .to_string();
                            if tx.send(line).is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
        drop(tx);
        Ok(PipTask {
            kind,
            command_line,
            started: Instant::now(),
            child: Some(child),
            rx,
            readers_done: false,
            exit: None,
            killed: false,
        })
    }

    /// New output lines since the last poll (non-blocking).
    pub fn poll(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(l) => lines.push(l),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.readers_done = true;
                    break;
                }
            }
        }
        if self.exit.is_none() && self.readers_done {
            if let Some(child) = &mut self.child {
                if let Ok(Some(status)) = child.try_wait() {
                    self.exit = Some(status.code().unwrap_or(if self.killed { 130 } else { -1 }));
                    self.child = None;
                }
            }
        }
        lines
    }

    pub fn is_running(&self) -> bool {
        self.exit.is_none()
    }

    /// Exit code once the process has finished and all output was delivered.
    pub fn exit_code(&self) -> Option<i32> {
        self.exit
    }

    /// True when `stop` was used.
    pub fn was_stopped(&self) -> bool {
        self.killed
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Terminates the process (and its process group on Unix).
    pub fn stop(&mut self) {
        if let Some(child) = &mut self.child {
            self.killed = true;
            #[cfg(unix)]
            {
                let pid = child.id() as i32;
                // SAFETY: plain libc call with a valid pid and signal number.
                unsafe { kill(-pid, 15) };
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = child.kill();
        }
    }
}

impl Drop for PipTask {
    fn drop(&mut self) {
        if self.child.is_some() {
            self.stop();
            if let Some(mut c) = self.child.take() {
                let _ = c.wait();
            }
        }
    }
}

#[cfg(unix)]
extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

fn non_empty(s: &str, what: &str) -> Result<String> {
    let s = s.trim();
    if s.is_empty() {
        bail!("empty {what}");
    }
    Ok(s.to_string())
}

/// pip's PEP 668 refusal ("externally-managed-environment"): the interpreter
/// is a distro-managed one; retry with `--break-system-packages` or use a venv.
pub fn is_externally_managed_error(output_lines: &[String]) -> bool {
    output_lines
        .iter()
        .any(|l| l.contains("externally-managed-environment"))
}

/// The pip flag that overrides the PEP 668 refusal.
pub const BREAK_SYSTEM_PACKAGES: &str = "--break-system-packages";

/// `pip install <spec> [extra args]`, e.g. spec `requests>=2` or `./path`.
pub fn install(interpreter: &Path, spec: &str, extra_args: &[String]) -> Result<PipTask> {
    let spec = non_empty(spec, "package specification")?;
    let mut args = vec!["install".to_string(), "--progress-bar".into(), "off".into()];
    args.extend(extra_args.iter().cloned());
    args.push(spec.clone());
    PipTask::spawn(PipTaskKind::Install { spec }, interpreter, &args)
}

/// `pip uninstall -y <name>`.
pub fn uninstall(interpreter: &Path, name: &str) -> Result<PipTask> {
    uninstall_with(interpreter, name, &[])
}

/// `pip uninstall -y <name> [extra args]` (e.g. `--break-system-packages`).
pub fn uninstall_with(interpreter: &Path, name: &str, extra_args: &[String]) -> Result<PipTask> {
    let name = non_empty(name, "package name")?;
    let mut args = vec!["uninstall".to_string(), "-y".into()];
    args.extend(extra_args.iter().cloned());
    args.push(name.clone());
    PipTask::spawn(PipTaskKind::Uninstall { name }, interpreter, &args)
}

/// `pip install --upgrade <name>`.
pub fn upgrade(interpreter: &Path, name: &str) -> Result<PipTask> {
    upgrade_with(interpreter, name, &[])
}

/// `pip install --upgrade <name> [extra args]`.
pub fn upgrade_with(interpreter: &Path, name: &str, extra_args: &[String]) -> Result<PipTask> {
    let name = non_empty(name, "package name")?;
    let mut args = vec![
        "install".to_string(),
        "--upgrade".into(),
        "--progress-bar".into(),
        "off".into(),
    ];
    args.extend(extra_args.iter().cloned());
    args.push(name.clone());
    PipTask::spawn(PipTaskKind::Upgrade { name }, interpreter, &args)
}

/// `pip install -r <path>`.
pub fn install_requirements(interpreter: &Path, path: &Path) -> Result<PipTask> {
    install_requirements_with(interpreter, path, &[])
}

/// `pip install -r <path> [extra args]`.
pub fn install_requirements_with(
    interpreter: &Path,
    path: &Path,
    extra_args: &[String],
) -> Result<PipTask> {
    if !path.is_file() {
        bail!("{} does not exist", path.display());
    }
    let mut args = vec!["install".to_string(), "--progress-bar".into(), "off".into()];
    args.extend(extra_args.iter().cloned());
    args.push("-r".into());
    args.push(path.display().to_string());
    PipTask::spawn(
        PipTaskKind::InstallRequirements {
            path: path.to_path_buf(),
        },
        interpreter,
        &args,
    )
}
