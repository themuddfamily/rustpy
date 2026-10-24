//! Running the external tools (`ssh`, `scp`, `rsync`, `curl`) with a timeout.
//!
//! Every command is first built as a [`CommandSpec`] by a pure function (so it can be unit
//! tested and shown in a log) and then executed by [`run`] / [`run_checked`].

use std::fmt;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// An external command: `(program, args)` plus optional data for its stdin.
#[derive(Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    /// Written to the child's stdin, e.g. a curl config carrying the FTP password (keeps secrets
    /// out of the process list). Never shown by [`CommandSpec::display`] or `Debug`.
    pub stdin: Option<String>,
    /// Captured per-command environment; values are omitted from logs.
    pub env: std::collections::BTreeMap<String, String>,
}

impl CommandSpec {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            stdin: None,
            env: Default::default(),
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn with_stdin(mut self, data: impl Into<String>) -> Self {
        self.stdin = Some(data.into());
        self
    }

    /// Replaces the program (e.g. a configured path to `ssh.exe`).
    pub fn with_program(mut self, program: impl Into<String>) -> Self {
        self.program = program.into();
        self
    }

    /// The `(program, args)` pair.
    pub fn to_pair(&self) -> (String, Vec<String>) {
        (self.program.clone(), self.args.clone())
    }

    /// Shell-like rendering for logs (arguments quoted when needed; stdin is not shown).
    pub fn display(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .map(quote_if_needed)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

impl fmt::Debug for CommandSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CommandSpec")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("environment_keys", &self.env.keys().collect::<Vec<_>>())
            .field("stdin", &self.stdin.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl fmt::Display for CommandSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display())
    }
}

/// Quotes `s` for a POSIX shell with single quotes (`'` becomes `'\''`). Always quotes.
pub fn sh_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// True when `s` can be passed to a POSIX shell unquoted.
pub(crate) fn is_shell_safe(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-./+=:,@%".contains(&b))
}

/// [`sh_quote`] only when needed.
pub(crate) fn quote_if_needed(s: &str) -> String {
    if is_shell_safe(s) {
        s.to_string()
    } else {
        sh_quote(s)
    }
}

/// Result of a finished process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    /// Exit code (`None` when killed by a signal).
    pub code: Option<i32>,
    pub success: bool,
    pub stdout: Vec<u8>,
    /// Lossily decoded, trailing whitespace removed.
    pub stderr: String,
}

impl Output {
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

/// Why running an external command failed. The `Display` text is meant for the user.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ExecError {
    #[error("Transfer output exceeded the 64 MiB limit")]
    OutputLimit,
    #[error("Transfer cancelled")]
    Cancelled,
    #[error("`{program}` was not found; install it or make sure it is on PATH")]
    NotFound { program: String },
    #[error("could not start `{program}`: {message}")]
    Spawn { program: String, message: String },
    #[error("`{command}` timed out after {}{}", fmt_duration(.timeout), stderr_suffix(.stderr))]
    Timeout {
        command: String,
        timeout: Duration,
        stderr: String,
    },
    #[error("`{command}` failed ({}){}{}", exit_label(.code), stderr_suffix(.stderr), hint_suffix(.hint))]
    Failed {
        command: String,
        code: Option<i32>,
        stderr: String,
        hint: Option<String>,
    },
}

impl ExecError {
    /// The exit code of a [`ExecError::Failed`] command.
    pub fn exit_code(&self) -> Option<i32> {
        match self {
            ExecError::Failed { code, .. } => *code,
            _ => None,
        }
    }

    pub fn stderr(&self) -> &str {
        match self {
            ExecError::Timeout { stderr, .. } | ExecError::Failed { stderr, .. } => stderr,
            _ => "",
        }
    }
}

fn fmt_duration(d: &Duration) -> String {
    if d.subsec_millis() == 0 && d.as_secs() > 0 {
        format!("{}s", d.as_secs())
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

fn exit_label(code: &Option<i32>) -> String {
    match *code {
        Some(c) => format!("exit code {c}"),
        None => "killed by a signal".to_string(),
    }
}

fn stderr_suffix(stderr: &str) -> String {
    let s = stderr.trim();
    if s.is_empty() {
        String::new()
    } else {
        format!(": {}", truncate(s, 2000))
    }
}

fn hint_suffix(hint: &Option<String>) -> String {
    hint.as_ref()
        .map(|h| format!("\nHint: {h}"))
        .unwrap_or_default()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max).collect();
        t.push('…');
        t
    }
}

/// Short command text used in error messages.
pub(crate) fn short_display(spec: &CommandSpec) -> String {
    truncate(&spec.display(), 300)
}

#[cfg(windows)]
fn no_console_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_console_window(_cmd: &mut Command) {}

fn spawn_reader<R: Read + Send + 'static>(
    pipe: Option<R>,
    is_stdout: bool,
    tx: mpsc::Sender<(bool, Vec<u8>, bool)>,
) {
    match pipe {
        Some(mut reader) => {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 8192];
                let mut truncated = false;
                loop {
                    match reader.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(count) => {
                            let retained =
                                count.min((64 * 1024 * 1024usize).saturating_sub(buf.len()));
                            buf.extend_from_slice(&chunk[..retained]);
                            truncated |= retained < count;
                        }
                    }
                }
                let _ = tx.send((is_stdout, buf, truncated));
            });
        }
        None => {
            let _ = tx.send((is_stdout, Vec::new(), false));
        }
    }
}

/// Runs `spec`, killing it after `timeout`. Output is collected on background threads, so
/// large outputs cannot dead-lock the child. A non-zero exit is **not** an error here.
pub fn run(spec: &CommandSpec, timeout: Duration) -> Result<Output, ExecError> {
    let context = crate::transfer::current();
    if context
        .owner
        .as_ref()
        .is_some_and(|owner| owner.cancelled())
    {
        return Err(ExecError::Cancelled);
    }
    let configured = configure(spec, &context.options);
    let spec = &configured;
    let timeout = timeout.min(Duration::from_secs(
        context.options.timeout_seconds.clamp(5, 3600),
    ));
    crate::transfer::note(&spec.display());
    log::debug!("deploy: running {}", spec.display());
    let mut cmd = Command::new(&spec.program);
    if let Some(owner) = &context.owner {
        if owner.root.is_dir() {
            cmd.current_dir(&owner.root);
        }
    }
    cmd.args(&spec.args)
        .stdin(if spec.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    no_console_window(&mut cmd);
    for (key, value) in &context.environment {
        cmd.env(key, value);
    }
    cmd.envs(&spec.env);
    rustpy_run::process_owner::ProcessOwner::prepare(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ExecError::NotFound {
                program: spec.program.clone(),
            }
        } else {
            ExecError::Spawn {
                program: spec.program.clone(),
                message: e.to_string(),
            }
        }
    })?;
    let owner = match rustpy_run::process_owner::ProcessOwner::attach(&mut child) {
        Ok(owner) => owner,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ExecError::Spawn {
                program: spec.program.clone(),
                message: error.to_string(),
            });
        }
    };
    crate::transfer::process_stamp(owner.stamp());
    if let (Some(data), Some(mut pipe)) = (spec.stdin.clone(), child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = pipe.write_all(data.as_bytes());
            // Dropping the pipe closes it (EOF for the child).
        });
    }
    let (tx, rx) = mpsc::channel();
    spawn_reader(child.stdout.take(), true, tx.clone());
    spawn_reader(child.stderr.take(), false, tx);

    let start = Instant::now();
    let mut delay = Duration::from_millis(2);
    let mut was_cancelled = false;
    let status = loop {
        if context
            .owner
            .as_ref()
            .is_some_and(|owner| owner.cancelled())
        {
            owner.stop(&mut child);
            crate::transfer::process_stamp(owner.stamp());
            was_cancelled = true;
            break None;
        }
        match owner.poll(&mut child) {
            Ok(Some(status)) => break Some(status),
            Ok(None) if start.elapsed() >= timeout => {
                owner.stop(&mut child);
                break None;
            }
            Ok(None) => {
                std::thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_millis(40));
            }
            Err(e) => {
                owner.stop(&mut child);
                return Err(ExecError::Spawn {
                    program: spec.program.clone(),
                    message: e.to_string(),
                });
            }
        }
    };
    crate::transfer::process_stamp(owner.stamp());
    // The pipes reach EOF once the process (and anything it spawned holding them) is gone;
    // never wait long for stragglers.
    let grace = if status.is_some() {
        Duration::from_secs(5)
    } else {
        Duration::from_millis(500)
    };
    let deadline = Instant::now() + grace;
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let mut truncated = false;
    for _ in 0..2 {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok((true, data, limit)) => {
                stdout = data;
                truncated |= limit;
            }
            Ok((false, data, limit)) => {
                stderr = data;
                truncated |= limit;
            }
            Err(_) => break,
        }
    }
    let stderr = String::from_utf8_lossy(&stderr).trim_end().to_string();
    if was_cancelled {
        return Err(ExecError::Cancelled);
    }
    if truncated {
        return Err(ExecError::OutputLimit);
    }
    match status {
        None => Err(ExecError::Timeout {
            command: short_display(spec),
            timeout,
            stderr,
        }),
        Some(st) => Ok(Output {
            code: st.code(),
            success: st.success(),
            stdout,
            stderr,
        }),
    }
}

/// [`run`], turning an unsuccessful exit into [`ExecError::Failed`] (with an optional hint
/// derived from the output, e.g. "check the SSH key").
pub fn run_checked(
    spec: &CommandSpec,
    timeout: Duration,
    hint: impl Fn(&Output) -> Option<String>,
) -> Result<Output, ExecError> {
    let out = run(spec, timeout)?;
    if out.success {
        return Ok(out);
    }
    Err(failed(spec, &out, hint(&out)))
}

pub(crate) fn failed(spec: &CommandSpec, out: &Output, hint: Option<String>) -> ExecError {
    let stderr = if out.stderr.trim().is_empty() {
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    } else {
        out.stderr.clone()
    };
    ExecError::Failed {
        command: short_display(spec),
        code: out.code,
        stderr,
        hint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("plain"), "'plain'");
        assert_eq!(sh_quote(""), "''");
        assert_eq!(sh_quote("a b"), "'a b'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
        assert_eq!(sh_quote("$HOME `x` \"q\""), "'$HOME `x` \"q\"'");
        assert_eq!(quote_if_needed("/var/www/app.py"), "/var/www/app.py");
        assert_eq!(quote_if_needed("a b"), "'a b'");
        assert_eq!(quote_if_needed(""), "''");
        assert!(!is_shell_safe("é"));
    }

    #[test]
    fn spec_display_and_debug() {
        let spec = CommandSpec::new("curl")
            .args(["-sS", "ftp://h/a b"])
            .arg("--config")
            .arg("-")
            .with_stdin("user = \"u:secret\"");
        assert_eq!(spec.display(), "curl -sS 'ftp://h/a b' --config -");
        assert_eq!(spec.to_string(), spec.display());
        assert_eq!(
            spec.to_pair(),
            (
                "curl".to_string(),
                vec![
                    "-sS".to_string(),
                    "ftp://h/a b".to_string(),
                    "--config".to_string(),
                    "-".to_string()
                ]
            )
        );
        assert!(!format!("{spec:?}").contains("secret"));
        assert_eq!(spec.clone().with_program("/opt/curl").program, "/opt/curl");
    }

    #[test]
    fn error_messages() {
        let e = ExecError::Failed {
            command: "ssh h".into(),
            code: Some(255),
            stderr: "Permission denied (publickey).".into(),
            hint: Some("use a key".into()),
        };
        assert_eq!(
            e.to_string(),
            "`ssh h` failed (exit code 255): Permission denied (publickey).\nHint: use a key"
        );
        assert_eq!(e.exit_code(), Some(255));
        let e = ExecError::Timeout {
            command: "scp".into(),
            timeout: Duration::from_millis(300),
            stderr: String::new(),
        };
        assert_eq!(e.to_string(), "`scp` timed out after 0.3s");
        let e = ExecError::Timeout {
            command: "scp".into(),
            timeout: Duration::from_secs(10),
            stderr: "stalled".into(),
        };
        assert_eq!(e.to_string(), "`scp` timed out after 10s: stalled");
        assert_eq!(
            ExecError::NotFound {
                program: "rsync".into()
            }
            .to_string(),
            "`rsync` was not found; install it or make sure it is on PATH"
        );
    }

    #[test]
    fn missing_program() {
        let err = run(
            &CommandSpec::new("rustpy-deploy-no-such-program-xyz"),
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert_eq!(
            err,
            ExecError::NotFound {
                program: "rustpy-deploy-no-such-program-xyz".into()
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn runs_with_output_stdin_and_errors() {
        let out = run(
            &CommandSpec::new("sh").args(["-c", "printf 'out'; printf 'err' >&2"]),
            Duration::from_secs(10),
        )
        .unwrap();
        assert!(out.success);
        assert_eq!(out.stdout, b"out");
        assert_eq!(out.stderr, "err");

        let out = run(
            &CommandSpec::new("cat").with_stdin("piped data"),
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(out.stdout_text(), "piped data");

        // Large output must not dead-lock.
        let out = run(&CommandSpec::new("sh").args(["-c", "i=0; while [ $i -lt 2000 ]; do echo 0123456789012345678901234567890123456789012345678901234567890123456789; i=$((i+1)); done"]), Duration::from_secs(30)).unwrap();
        assert_eq!(out.stdout.len(), 2000 * 71);

        let err = run_checked(
            &CommandSpec::new("sh").args(["-c", "echo 'No such file' >&2; exit 3"]),
            Duration::from_secs(10),
            |o| (o.code == Some(3)).then(|| "three".to_string()),
        )
        .unwrap_err();
        assert_eq!(err.exit_code(), Some(3));
        assert_eq!(err.stderr(), "No such file");
        let msg = err.to_string();
        assert!(
            msg.contains("exit code 3")
                && msg.contains("No such file")
                && msg.contains("Hint: three"),
            "{msg}"
        );

        // stdout is used when stderr is empty
        let err = run_checked(
            &CommandSpec::new("sh").args(["-c", "echo oops; exit 1"]),
            Duration::from_secs(10),
            |_| None,
        )
        .unwrap_err();
        assert_eq!(err.stderr(), "oops");
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_the_process() {
        let started = Instant::now();
        let err = run(
            &CommandSpec::new("sleep").arg("20"),
            Duration::from_millis(200),
        )
        .unwrap_err();
        assert!(matches!(err, ExecError::Timeout { .. }), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}

/// Transform only configured options; command builders retain their stable default API.
pub fn configure(spec: &CommandSpec, options: &crate::options::Options) -> CommandSpec {
    let mut result = spec.clone();
    let tool = std::path::Path::new(&spec.program)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    let executable: &str = match tool.as_str() {
        "curl" => &options.curl_executable,
        "ssh" => &options.ssh_executable,
        "scp" => &options.scp_executable,
        "rsync" => &options.rsync_executable,
        _ => "",
    };
    if !executable.is_empty() {
        result.program = executable.into();
    }
    if tool == "curl" {
        if options.ftp_tls == "explicit" {
            result.args.splice(0..0, ["--ssl-reqd".into()]);
        } else if options.ftp_tls == "implicit" {
            for arg in &mut result.args {
                if let Some(path) = arg.strip_prefix("ftp://") {
                    *arg = format!("ftps://{path}");
                }
            }
        }
        for (flag, value) in [
            ("--cacert", &options.tls_ca_file),
            ("--cert", &options.tls_client_certificate),
        ] {
            if !value.is_empty() {
                result.args.splice(0..0, [flag.into(), value.clone()]);
            }
        }
        if !options.passive_ftp {
            result.args.splice(0..0, ["--ftp-port".into(), "-".into()]);
        }
        if !options.preserve_timestamps {
            result.args.retain(|arg| arg != "-R");
        }
    }
    if !options.preserve_timestamps && tool == "scp" {
        result.args.retain(|arg| arg != "-p");
    }
    if !options.preserve_timestamps && tool == "rsync" {
        for arg in &mut result.args {
            if arg == "-az" {
                *arg = "-rlpz".into();
            }
        }
    }
    result
}
#[cfg(test)]
mod option_fixtures {
    use super::*;
    #[test]
    fn ftps_uses_tls_without_disabling_certificate_checks() {
        let command = CommandSpec::new("curl").arg("ftp://host:990/%2Ffile");
        let implicit = configure(
            &command,
            &crate::options::Options {
                ftp_tls: "implicit".into(),
                tls_ca_file: "/cert/ca.pem".into(),
                ..Default::default()
            },
        );
        assert!(implicit.args.contains(&"ftps://host:990/%2Ffile".into()));
        assert!(implicit.args.contains(&"--cacert".into()));
        assert!(!implicit.args.contains(&"--insecure".into()));
        let explicit = configure(
            &command,
            &crate::options::Options {
                ftp_tls: "explicit".into(),
                ..Default::default()
            },
        );
        assert!(explicit.args.contains(&"--ssl-reqd".into()));
    }
    #[test]
    fn timestamp_preference_changes_real_transfer_arguments() {
        let options = crate::options::Options {
            preserve_timestamps: false,
            ..Default::default()
        };
        assert!(
            !configure(&CommandSpec::new("scp").args(["-p", "a", "h:b"]), &options)
                .args
                .contains(&"-p".into())
        );
    }
}

#[cfg(test)]
mod captured_environment_tests {
    use super::*;
    #[test]
    fn captured_environment_survives_configuration_without_log_disclosure() {
        let command = CommandSpec::new("docker")
            .arg("build")
            .with_env("DOCKER_BUILDKIT", "0")
            .with_env("PRIVATE_TOKEN", "secret-provider-value");
        let copy = configure(&command, &Default::default());
        assert_eq!(copy.env, command.env);
        assert!(!format!("{copy:?}").contains("secret-provider-value"));
        assert!(!copy.display().contains("secret-provider-value"));
    }
}
