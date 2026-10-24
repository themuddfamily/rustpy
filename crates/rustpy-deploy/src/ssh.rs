//! SSH-based transports: [`SftpTransport`] (`ssh` + `scp`) and [`RsyncTransport`]
//! (`ssh` + `rsync`).
//!
//! Remote commands run as `ssh [options] -- user@host "sh -c '<script>'"`: the `--` stops
//! option parsing before the destination, the script is executed by `sh` whatever the login
//! shell is, and every path inside it is single-quoted with [`sh_quote`]. `BatchMode=yes`
//! makes authentication fail instead of prompting, `ConnectTimeout=10` bounds connecting and
//! `ServerAliveInterval` detects stalled connections.
//!
//! Remote path arguments of the transfer tools follow each tool's own rules:
//!
//! * `scp` with the SFTP protocol (OpenSSH ≥ 9.0): upload targets are literal; download
//!   sources are glob patterns, so their `\*?[]` are backslash-escaped. With the legacy SCP
//!   protocol (older clients, or `-O`) the remote shell parses the path: it is single-quoted.
//!   When the server has no SFTP subsystem, [`SftpTransport`] switches to `-O` by itself.
//! * `rsync -s` (protect-args): no remote shell is involved, but the remote rsync expands each
//!   path segment containing `*`, `?` or `[` against the existing names (and uses the segment
//!   as typed when nothing matches). Such segments are escaped ([`rsync_escape_path`]), and
//!   uploads target the (existing) parent directory, so the file name comes from the local file
//!   and is never expanded.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use anyhow::{anyhow, bail, Context as _};

use crate::exec::{self, is_shell_safe, sh_quote, CommandSpec, ExecError, Output};
use crate::glob::ExcludeSet;
use crate::listing::{parse_find_output, parse_ls_output};
use crate::paths::{is_root_like, normalize_remote, remote_file_name, remote_parent};
use crate::{download_via_temp, unix_now, walk_recursive, RemoteEntry, Transport};

/// Default timeout of listing / mkdir / delete commands.
pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
/// Default timeout of one file transfer (stalls are detected earlier by keep-alives).
pub const DEFAULT_TRANSFER_TIMEOUT: Duration = Duration::from_secs(3600);
const TEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Exit code of the recursive listing script when GNU `find` is missing.
const NO_GNU_FIND_EXIT: i32 = 97;
/// Exit code of the connection test script when the root path cannot be entered.
const TEST_NO_ROOT_EXIT: i32 = 90;
/// Exit code of the connection test script when a required tool is missing on the server.
const TEST_MISSING_TOOL_EXIT: i32 = 91;

/// Where and how to connect with OpenSSH.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshTarget {
    pub host: String,
    pub port: u16,
    /// Empty: let ssh decide (`~/.ssh/config` or the local user name).
    pub user: String,
    pub key_file: Option<PathBuf>,
    /// Legacy compatibility field. Unknown host keys now always require an
    /// owned confirmation prompt; this flag never bypasses verification.
    pub accept_new_host_keys: bool,
}

impl SshTarget {
    /// Brackets around an IPv6 address are removed.
    pub fn new(
        host: impl Into<String>,
        port: u16,
        user: impl Into<String>,
        key_file: Option<PathBuf>,
    ) -> Self {
        let host: String = host.into();
        let host = host.trim();
        let host = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(host)
            .to_string();
        Self {
            host,
            port,
            user: user.into().trim().to_string(),
            key_file,
            accept_new_host_keys: false,
        }
    }

    pub fn with_accept_new_host_keys(mut self, accept: bool) -> Self {
        self.accept_new_host_keys = accept;
        self
    }

    /// Rejects host/user values that are empty, contain spaces or could be read as options.
    pub fn validate(&self) -> anyhow::Result<()> {
        check_host(&self.host).map_err(anyhow::Error::msg)?;
        check_user(&self.user).map_err(anyhow::Error::msg)?;
        if self.port == 0 {
            bail!("SSH port must be between 1 and 65535");
        }
        Ok(())
    }

    /// `user@host` (ssh destination).
    pub fn destination(&self) -> String {
        if self.user.is_empty() {
            self.host.clone()
        } else {
            format!("{}@{}", self.user, self.host)
        }
    }

    /// `user@host` with IPv6 addresses bracketed, as used before `:path` by scp and rsync.
    pub fn remote_prefix(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.user.is_empty() {
            host
        } else {
            format!("{}@{host}", self.user)
        }
    }

    /// `user@host:port`
    pub fn label(&self) -> String {
        format!("{}:{}", self.remote_prefix(), self.port)
    }
}

pub(crate) fn check_host(host: &str) -> Result<(), String> {
    if host.is_empty() {
        return Err("Host name is empty".to_string());
    }
    if host.starts_with('-') {
        return Err(format!("Host name must not start with '-': {host}"));
    }
    if !host
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || ".-_:%".contains(c))
    {
        return Err(format!("Host name contains invalid characters: {host:?}"));
    }
    Ok(())
}

pub(crate) fn check_user(user: &str) -> Result<(), String> {
    if user.starts_with('-') {
        return Err(format!("User name must not start with '-': {user}"));
    }
    if user
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || "@:/".contains(c))
    {
        return Err(format!("User name contains invalid characters: {user:?}"));
    }
    Ok(())
}

fn ssh_options(target: &SshTarget, port_flag: &str) -> Vec<String> {
    let options = crate::transfer::current().options;
    let mut a: Vec<String> = [
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
    ]
    .map(String::from)
    .to_vec();
    if options.ssh_auth == "password" {
        a[1] = "BatchMode=no".into();
        a.extend([
            "-o".into(),
            "PreferredAuthentications=publickey,keyboard-interactive,password".into(),
        ]);
    }
    if !options.ssh_config_file.is_empty() {
        a.extend(["-F".into(), options.ssh_config_file]);
    }
    if !options.jump_host.is_empty() {
        a.extend(["-J".into(), options.jump_host]);
    }
    // Command-line policy overrides permissive user/project SSH config. Only
    // an IDE-owned askpass channel can ask about an unknown key; headless plans
    // fail closed. OpenSSH's `ask` refuses changed keys without offering trust.
    let can_prompt = crate::transfer::current()
        .environment
        .iter()
        .any(|(key, value)| key == "RUSTPY_ASKPASS_ADDR" && !value.is_empty());
    a.extend([
        "-o".into(),
        if can_prompt {
            "StrictHostKeyChecking=ask".into()
        } else {
            "StrictHostKeyChecking=yes".into()
        },
    ]);
    // BatchMode suppresses host-key questions as well as password prompts.
    // Keep authentication inside the same owned askpass bridge when enabled.
    if can_prompt {
        a[1] = "BatchMode=no".into();
    }
    a.push(port_flag.to_string());
    a.push(target.port.to_string());
    if let Some(key) = &target.key_file {
        a.push("-i".into());
        a.push(key.to_string_lossy().into_owned());
    }
    a
}

/// Normalized remote path that cannot be mistaken for an option, a `find` operator (`!`, `(`,
/// `)`, `,`) or a `host:` prefix.
fn shell_path(path: &str) -> String {
    let p = normalize_remote(path);
    if p.starts_with(['-', ':', '!', '(', ')', ',']) {
        format!("./{p}")
    } else {
        p
    }
}

/// Local path argument for scp/rsync (`a:b` would be taken for a remote path).
fn local_arg(local: &Path) -> String {
    let s = local.to_string_lossy().into_owned();
    if cfg!(not(windows)) && !local.is_absolute() && (s.contains(':') || s.starts_with('-')) {
        format!("./{s}")
    } else {
        s
    }
}

// ---------------------------------------------------------------------------
// Pure command builders
// ---------------------------------------------------------------------------

/// `ssh -o BatchMode=yes -o ConnectTimeout=10 … -p PORT [-i KEY] -- user@host <remote_command>`
pub fn ssh_command(target: &SshTarget, remote_command: &str) -> CommandSpec {
    CommandSpec::new("ssh")
        .args(ssh_options(target, "-p"))
        .arg("--")
        .arg(target.destination())
        .arg(remote_command)
}

/// `sh -c '<script>'`, so the script runs in a POSIX shell whatever the login shell is.
pub fn remote_sh(script: &str) -> String {
    format!("sh -c {}", sh_quote(script))
}

/// Script listing one directory: GNU `find -printf` (NUL-separated records), falling back
/// to `ls -la` on systems without GNU find (`TZ=UTC0`, so that its dates are UTC like ours).
pub fn list_script(dir: &str) -> String {
    format!(
        r#"d={}; if find "$d" -maxdepth 0 -printf '' >/dev/null 2>&1; then find -H "$d" -mindepth 1 -maxdepth 1 -printf '%Y %s %T@ %f\0'; else TZ=UTC0 LC_ALL=C QUOTING_STYLE=literal ls -la "$d/"; fi"#,
        sh_quote(&shell_path(dir))
    )
}

/// Script listing a tree with GNU find (`%P` = path relative to `dir`), pruning the given
/// name patterns; exits with 0 and no output when `dir` does not exist.
pub fn list_recursive_script(dir: &str, prune_names: &[String]) -> String {
    let prune = if prune_names.is_empty() {
        String::new()
    } else {
        let names: Vec<String> = prune_names
            .iter()
            .map(|p| format!("-name {}", sh_quote(p)))
            .collect();
        format!(r"\( {} \) -prune -o ", names.join(" -o "))
    };
    format!(
        r#"d={}; [ -e "$d" ] || exit 0; if find "$d" -maxdepth 0 -printf '' >/dev/null 2>&1; then find -H "$d" -mindepth 1 {prune}-printf '%Y %s %T@ %P\0'; else echo 'GNU find is not available on the server' >&2; exit {NO_GNU_FIND_EXIT}; fi"#,
        sh_quote(&shell_path(dir))
    )
}

pub fn ssh_list_command(target: &SshTarget, dir: &str) -> CommandSpec {
    ssh_command(target, &remote_sh(&list_script(dir)))
}

/// Recursive listing; name-only `excluded` patterns are pruned on the server.
pub fn ssh_list_recursive_command(
    target: &SshTarget,
    dir: &str,
    excluded: &[String],
) -> CommandSpec {
    let set = ExcludeSet::new(excluded);
    ssh_command(
        target,
        &remote_sh(&list_recursive_script(dir, set.name_patterns())),
    )
}

pub fn ssh_mkdirs_command(target: &SshTarget, dir: &str) -> CommandSpec {
    ssh_command(
        target,
        &remote_sh(&format!("mkdir -p -- {}", sh_quote(&shell_path(dir)))),
    )
}

/// `rm -rf` of a file or directory (callers refuse `/` and the login directory).
pub fn ssh_delete_command(target: &SshTarget, path: &str) -> CommandSpec {
    ssh_command(
        target,
        &remote_sh(&format!("rm -rf -- {}", sh_quote(&shell_path(path)))),
    )
}

/// Connection test: `cd <root>` and print it with `pwd`, then check that each of
/// `required_tools` is installed on the server (`command -v`). Exits with 90 when the root
/// path cannot be entered and with 91 when a tool is missing.
pub fn ssh_test_command(target: &SshTarget, root: &str, required_tools: &[&str]) -> CommandSpec {
    let mut script = format!(
        "cd {} || exit {TEST_NO_ROOT_EXIT}; pwd",
        sh_quote(&shell_path(root))
    );
    for tool in required_tools {
        let missing = sh_quote(&format!("{tool} is not installed on the server"));
        script.push_str(&format!("; command -v {} >/dev/null 2>&1 || {{ echo {missing} >&2; exit {TEST_MISSING_TOOL_EXIT}; }}", sh_quote(tool)));
    }
    ssh_command(target, &remote_sh(&script))
}

/// How `scp` treats remote paths (it changed in OpenSSH 9.0).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScpMode {
    /// OpenSSH ≥ 9.0 (SFTP protocol): remote paths are used literally; glob characters in
    /// download sources are backslash-escaped.
    Sftp,
    /// Legacy SCP protocol of OpenSSH < 9.0: the remote shell interprets the remote path, so
    /// it is single-quoted when needed. `strict_names` (OpenSSH 8.x) adds `-T` to quoted
    /// downloads, because the client's file name check would reject them.
    Legacy { strict_names: bool },
    /// OpenSSH ≥ 9.0 switched back to the legacy protocol with `-O` (for servers without an
    /// SFTP subsystem); paths are quoted as for [`ScpMode::Legacy`] with strict names.
    ForcedLegacy,
}

/// Picks the scp mode from `ssh -V` output (`OpenSSH_9.6p1 …`, `OpenSSH_for_Windows_8.1p1 …`).
pub fn scp_mode_from_version(version_output: &str) -> ScpMode {
    let Some(i) = version_output.find("OpenSSH_") else {
        return ScpMode::Legacy {
            strict_names: false,
        };
    };
    let rest = &version_output[i + "OpenSSH_".len()..];
    let rest = rest.strip_prefix("for_Windows_").unwrap_or(rest);
    let major: u32 = rest
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or(0);
    match major {
        9.. => ScpMode::Sftp,
        8 => ScpMode::Legacy { strict_names: true },
        _ => ScpMode::Legacy {
            strict_names: false,
        },
    }
}

fn glob_escape(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        if "\\*?[]".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The `path` part of `host:path` and whether `-T` is needed.
fn scp_remote_arg(path: &str, mode: ScpMode, is_source: bool) -> (String, bool) {
    let legacy = |strict_names: bool| {
        if is_shell_safe(path) {
            (path.to_string(), false)
        } else {
            (sh_quote(path), strict_names && is_source)
        }
    };
    match mode {
        ScpMode::Sftp => (
            if is_source {
                glob_escape(path)
            } else {
                path.to_string()
            },
            false,
        ),
        ScpMode::Legacy { strict_names } => legacy(strict_names),
        ScpMode::ForcedLegacy => legacy(true),
    }
}

/// `scp [-O] -o … -P PORT [-i KEY] [-S SSH] -p`
fn scp_base(target: &SshTarget, ssh_program: &str, mode: ScpMode) -> CommandSpec {
    let options = crate::transfer::current().options;
    let ssh_program = if options.ssh_executable.is_empty() {
        ssh_program
    } else {
        &options.ssh_executable
    };
    let spec = CommandSpec::new("scp");
    let spec = if mode == ScpMode::ForcedLegacy {
        spec.arg("-O")
    } else {
        spec
    };
    let spec = spec.args(ssh_options(target, "-P"));
    let spec = if ssh_program != "ssh" {
        spec.arg("-S").arg(ssh_program)
    } else {
        spec
    };
    spec.arg("-p")
}

/// `scp [-O] … -P PORT [-i KEY] -p -- <local> user@host:<remote>` (`-p` keeps the mtime).
pub fn scp_upload_command(
    target: &SshTarget,
    ssh_program: &str,
    local: &Path,
    remote: &str,
    mode: ScpMode,
) -> CommandSpec {
    let (arg, _) = scp_remote_arg(&shell_path(remote), mode, false);
    scp_base(target, ssh_program, mode)
        .arg("--")
        .arg(local_arg(local))
        .arg(format!("{}:{arg}", target.remote_prefix()))
}

/// `scp [-O] … -P PORT [-i KEY] -p [-T] -- user@host:<remote> <local>`
pub fn scp_download_command(
    target: &SshTarget,
    ssh_program: &str,
    remote: &str,
    local: &Path,
    mode: ScpMode,
) -> CommandSpec {
    let (arg, no_name_check) = scp_remote_arg(&shell_path(remote), mode, true);
    let spec = scp_base(target, ssh_program, mode);
    let spec = if no_name_check { spec.arg("-T") } else { spec };
    spec.arg("--")
        .arg(format!("{}:{arg}", target.remote_prefix()))
        .arg(local_arg(local))
}

/// Quotes one word of rsync's `-e` command (rsync splits it on spaces; `''` is a quote).
fn rsync_word(s: &str) -> String {
    if !s.is_empty()
        && !s
            .chars()
            .any(|c| c.is_whitespace() || c == '\'' || c == '"')
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "''"))
    }
}

/// The `-e` value: `ssh -o BatchMode=yes … -p PORT [-i KEY]`.
pub fn rsync_shell(target: &SshTarget, ssh_program: &str) -> String {
    let options = crate::transfer::current().options;
    let ssh_program = if options.ssh_executable.is_empty() {
        ssh_program
    } else {
        &options.ssh_executable
    };
    std::iter::once(ssh_program.to_string())
        .chain(ssh_options(target, "-p"))
        .map(|w| rsync_word(&w))
        .collect::<Vec<_>>()
        .join(" ")
}

fn has_wildcard(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

/// Escapes a remote path for `rsync -s`: the remote rsync expands every segment containing
/// `*`, `?` or `[` against the existing names (wildmatch, `\` escapes) and uses the other
/// segments literally, so only segments with a wildcard get their `\*?[]` backslash-escaped.
/// The escaped segments must exist on the server.
pub fn rsync_escape_path(path: &str) -> String {
    path.split('/')
        .map(|seg| {
            if has_wildcard(seg) {
                glob_escape(seg)
            } else {
                seg.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// `rsync -az -L -s -e "<ssh>" --`: `-L` sends the content of a symlinked file (like the other
/// transports) instead of recreating the link, `-s` see [`rsync_escape_path`].
fn rsync_base(target: &SshTarget, ssh_program: &str) -> CommandSpec {
    CommandSpec::new("rsync")
        .args(["-az", "-L", "-s", "-e"])
        .arg(rsync_shell(target, ssh_program))
        .arg("--")
}

/// `rsync -az -L -s -e "<ssh>" -- <local> user@host:<remote dir>/`. `-s` (protect-args) passes
/// remote paths without a remote shell, so they are not quoted. The file goes into the parent
/// directory of `remote` (which must exist; see [`rsync_escape_path`]) and keeps the local
/// file name, so the name is never expanded as a wildcard and an existing directory named
/// like the file is an error instead of a target. When `remote` names the file differently,
/// the full path is used, which is refused if the new name contains a wildcard character.
pub fn rsync_upload_command(
    target: &SshTarget,
    ssh_program: &str,
    local: &Path,
    remote: &str,
) -> anyhow::Result<CommandSpec> {
    let remote = shell_path(remote);
    let Some(parent) = remote_parent(&remote) else {
        bail!("{remote:?} is not a file path")
    };
    let dir = match parent.as_str() {
        "/" => "/".to_string(),
        "." => "./".to_string(),
        p => format!("{}/", rsync_escape_path(&shell_path(p))),
    };
    let name = remote_file_name(&remote);
    let dest = if local
        .file_name()
        .is_some_and(|n| n.to_string_lossy() == name.as_str())
    {
        dir
    } else if has_wildcard(&name) {
        bail!(
            "rsync cannot upload {} as {name:?}: a renamed target must not contain '*', '?' or '['",
            local.display()
        );
    } else {
        format!("{dir}{name}")
    };
    Ok(rsync_base(target, ssh_program)
        .arg(local_arg(local))
        .arg(format!("{}:{dest}", target.remote_prefix())))
}

/// `rsync -az -L -s -e "<ssh>" -- user@host:<remote> <local>` (wildcard segments of `remote`
/// escaped, see [`rsync_escape_path`]).
pub fn rsync_download_command(
    target: &SshTarget,
    ssh_program: &str,
    remote: &str,
    local: &Path,
) -> CommandSpec {
    rsync_base(target, ssh_program)
        .arg(format!(
            "{}:{}",
            target.remote_prefix(),
            rsync_escape_path(&shell_path(remote))
        ))
        .arg(local_arg(local))
}

/// A user-facing hint for common ssh/scp/rsync failures.
pub(crate) fn ssh_hint(stderr: &str) -> Option<String> {
    let s = stderr.to_ascii_lowercase();
    let hint = if s.contains("host key verification failed")
        || s.contains("remote host identification has changed")
    {
        "The server's host key is unknown or has changed. Connect once with ssh in a terminal to verify and accept it."
    } else if s.contains("permission denied (") || s.contains("too many authentication failures") {
        "Authentication failed. Password prompts are disabled (BatchMode=yes); use a key file, ssh-agent or ~/.ssh/config."
    } else if s.contains("could not resolve hostname") || s.contains("name or service not known") {
        "Check the host name."
    } else if s.contains("connection refused") {
        "Nothing accepts SSH connections on that host and port."
    } else if s.contains("timed out") {
        "The host did not answer; check the address, port, VPN and firewall."
    } else if is_missing_sftp_subsystem(&s) {
        "The server has no SFTP subsystem."
    } else if s.contains("rsync: command not found")
        || s.contains("rsync: not found")
        || s.contains("rsync error: remote command not found")
    {
        "rsync must also be installed on the server."
    } else if s.contains("filename does not match request") {
        "scp rejected the file name; use the SFTP protocol (OpenSSH 9+) or a simpler name."
    } else {
        return None;
    };
    Some(hint.to_string())
}

/// `subsystem request failed on channel 0` (older OpenSSH) or `Request for subsystem 'sftp'
/// failed on channel 0`: the server cannot speak SFTP.
pub(crate) fn is_missing_sftp_subsystem(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("subsystem") && s.contains("failed on channel")
}

// ---------------------------------------------------------------------------
// Shared SSH operations
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct SshShell {
    target: SshTarget,
    program: String,
    timeout: Duration,
}

impl SshShell {
    fn run(&self, spec: CommandSpec) -> anyhow::Result<Output> {
        Ok(exec::run_checked(
            &spec.with_program(&self.program),
            self.timeout,
            |o| ssh_hint(&o.stderr),
        )?)
    }

    fn list(&self, dir: &str) -> anyhow::Result<Vec<RemoteEntry>> {
        let out = self.run(ssh_list_command(&self.target, dir))?;
        let mut entries = if out.stdout.is_empty() || out.stdout.contains(&0) {
            parse_find_output(&out.stdout)
        } else {
            parse_ls_output(&out.stdout_text(), unix_now())
        };
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    fn list_recursive<T: Transport + ?Sized>(
        &self,
        outer: &T,
        dir: &str,
        excluded: &[String],
    ) -> anyhow::Result<Vec<RemoteEntry>> {
        let spec =
            ssh_list_recursive_command(&self.target, dir, excluded).with_program(&self.program);
        let out = exec::run(&spec, self.timeout * 5)?;
        if out.code == Some(NO_GNU_FIND_EXIT) {
            return walk_recursive(outer, dir, excluded);
        }
        // find exits with 1 when some subdirectories were unreadable; keep what it listed.
        if !out.success && !(out.code == Some(1) && !out.stdout.is_empty()) {
            return Err(exec::failed(&spec, &out, ssh_hint(&out.stderr)).into());
        }
        if !out.success {
            log::warn!("deployment: partial listing of {dir}: {}", out.stderr);
        }
        let set = ExcludeSet::new(excluded);
        let mut entries: Vec<RemoteEntry> = parse_find_output(&out.stdout)
            .into_iter()
            .filter(|e| !set.is_excluded(&e.name))
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    fn mkdirs(&self, dir: &str) -> anyhow::Result<()> {
        let d = normalize_remote(dir);
        if d == "/" || d == "." {
            return Ok(());
        }
        self.run(ssh_mkdirs_command(&self.target, &d))?;
        Ok(())
    }

    fn delete(&self, path: &str) -> anyhow::Result<()> {
        if is_root_like(path) {
            bail!("refusing to delete {path:?}");
        }
        self.run(ssh_delete_command(&self.target, path))?;
        Ok(())
    }

    fn test(&self, root: &str, required_tools: &[&str]) -> anyhow::Result<String> {
        let spec = ssh_test_command(&self.target, root, required_tools).with_program(&self.program);
        let label = self.target.label();
        match exec::run_checked(&spec, TEST_TIMEOUT, |o| ssh_hint(&o.stderr)) {
            Ok(out) => Ok(format!(
                "Connected to {label}; root path: {}",
                out.stdout_text().trim()
            )),
            Err(
                e @ ExecError::Failed {
                    code: Some(255), ..
                },
            ) => Err(anyhow!(e).context(format!("SSH connection to {label} failed"))),
            Err(
                e @ ExecError::Failed {
                    code: Some(TEST_MISSING_TOOL_EXIT),
                    ..
                },
            ) => Err(anyhow!(e).context(format!(
                "connected to {label}, but a required tool is missing on the server"
            ))),
            Err(e @ ExecError::Failed { .. }) => Err(anyhow!(e).context(format!(
                "connected to {label}, but the root path {root:?} is not accessible"
            ))),
            Err(e) => Err(e.into()),
        }
    }
}

fn check_file_path(remote: &str) -> anyhow::Result<String> {
    let r = normalize_remote(remote);
    if is_root_like(&r) {
        bail!("{remote:?} is not a file path");
    }
    Ok(r)
}

fn check_local_file(local: &Path) -> anyhow::Result<()> {
    let meta =
        std::fs::metadata(local).with_context(|| format!("cannot read {}", local.display()))?;
    if meta.is_dir() {
        bail!("{} is a directory", local.display());
    }
    Ok(())
}

fn check_root_path(root_path: &str) -> anyhow::Result<()> {
    if crate::config::has_control_chars(root_path) {
        bail!("the root path contains control characters");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// SFTP (ssh + scp)
// ---------------------------------------------------------------------------

/// SFTP deployment through the OpenSSH `ssh` and `scp` command-line tools.
#[derive(Debug)]
pub struct SftpTransport {
    shell: SshShell,
    scp_program: String,
    /// Configured or detected on first use; becomes [`ScpMode::ForcedLegacy`] when the server
    /// turns out to have no SFTP subsystem.
    scp_mode: Mutex<Option<ScpMode>>,
    root_path: String,
    transfer_timeout: Duration,
}

impl Clone for SftpTransport {
    fn clone(&self) -> Self {
        Self {
            shell: self.shell.clone(),
            scp_program: self.scp_program.clone(),
            scp_mode: Mutex::new(*self.scp_mode.lock().unwrap_or_else(PoisonError::into_inner)),
            root_path: self.root_path.clone(),
            transfer_timeout: self.transfer_timeout,
        }
    }
}

impl SftpTransport {
    pub fn new(target: SshTarget, root_path: impl Into<String>) -> anyhow::Result<Self> {
        target.validate()?;
        let root_path = root_path.into();
        check_root_path(&root_path)?;
        Ok(Self {
            shell: SshShell {
                target,
                program: "ssh".into(),
                timeout: DEFAULT_COMMAND_TIMEOUT,
            },
            scp_program: "scp".into(),
            scp_mode: Mutex::new(None),
            root_path,
            transfer_timeout: DEFAULT_TRANSFER_TIMEOUT,
        })
    }

    /// Uses other `ssh`/`scp` executables (scp is told about a custom ssh with `-S`).
    pub fn with_programs(mut self, ssh: impl Into<String>, scp: impl Into<String>) -> Self {
        self.shell.program = ssh.into();
        self.scp_program = scp.into();
        self
    }

    /// Overrides the detected [`ScpMode`]; it must match the installed scp client.
    pub fn with_scp_mode(mut self, mode: ScpMode) -> Self {
        self.scp_mode = Mutex::new(Some(mode));
        self
    }

    pub fn with_timeouts(mut self, command: Duration, transfer: Duration) -> Self {
        self.shell.timeout = command;
        self.transfer_timeout = transfer;
        self
    }

    pub fn target(&self) -> &SshTarget {
        &self.shell.target
    }

    /// The scp mode: as configured, else detected once from `ssh -V`.
    pub fn scp_mode(&self) -> ScpMode {
        let mut mode = self.scp_mode.lock().unwrap_or_else(PoisonError::into_inner);
        *mode.get_or_insert_with(|| {
            match exec::run(
                &CommandSpec::new(&self.shell.program).arg("-V"),
                Duration::from_secs(10),
            ) {
                Ok(out) => scp_mode_from_version(&format!("{} {}", out.stderr, out.stdout_text())),
                Err(_) => ScpMode::Sftp,
            }
        })
    }

    /// Runs the scp command `build` makes for the current mode. When the server has no SFTP
    /// subsystem, switches to the legacy protocol (`scp -O`) for good and retries once.
    fn scp(&self, build: impl Fn(ScpMode) -> CommandSpec) -> anyhow::Result<()> {
        let run = |mode| {
            exec::run_checked(
                &build(mode).with_program(&self.scp_program),
                self.transfer_timeout,
                |o| ssh_hint(&o.stderr),
            )
        };
        let mode = self.scp_mode();
        match run(mode) {
            Ok(_) => Ok(()),
            Err(e) if mode == ScpMode::Sftp && is_missing_sftp_subsystem(e.stderr()) => {
                log::info!(
                    "deployment: {} has no SFTP subsystem; using the legacy SCP protocol",
                    self.shell.target.label()
                );
                *self.scp_mode.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(ScpMode::ForcedLegacy);
                run(ScpMode::ForcedLegacy)?;
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }
}

impl Transport for SftpTransport {
    fn rename(&self, source: &str, destination: &str) -> anyhow::Result<()> {
        let source = check_file_path(source)?;
        let destination = check_file_path(destination)?;
        self.shell.run(ssh_command(
            &self.shell.target,
            &format!(
                "mv -- {} {}",
                sh_quote(&shell_path(&source)),
                sh_quote(&shell_path(&destination))
            ),
        ))?;
        Ok(())
    }
    fn permissions(&self, path: &str, mode: u32) -> anyhow::Result<()> {
        let path = check_file_path(path)?;
        anyhow::ensure!(mode <= 0o7777, "Invalid permission bits");
        self.shell.run(ssh_command(
            &self.shell.target,
            &format!("chmod {mode:o} -- {}", sh_quote(&shell_path(&path))),
        ))?;
        Ok(())
    }
    fn upload(&self, local: &Path, remote: &str) -> anyhow::Result<()> {
        check_local_file(local)?;
        let remote = check_file_path(remote)?;
        self.scp(|mode| {
            scp_upload_command(
                &self.shell.target,
                &self.shell.program,
                local,
                &remote,
                mode,
            )
        })
    }

    fn download(&self, remote: &str, local: &Path) -> anyhow::Result<()> {
        let remote = check_file_path(remote)?;
        download_via_temp(local, |tmp| {
            self.scp(|mode| {
                scp_download_command(&self.shell.target, &self.shell.program, &remote, tmp, mode)
            })
        })
    }

    fn list(&self, remote_dir: &str) -> anyhow::Result<Vec<RemoteEntry>> {
        self.shell.list(remote_dir)
    }

    fn delete(&self, remote: &str) -> anyhow::Result<()> {
        self.shell.delete(remote)
    }

    fn mkdirs(&self, remote_dir: &str) -> anyhow::Result<()> {
        self.shell.mkdirs(remote_dir)
    }

    fn test_connection(&self) -> anyhow::Result<String> {
        self.shell.test(&self.root_path, &[])
    }

    fn list_recursive(
        &self,
        remote_dir: &str,
        excluded: &[String],
    ) -> anyhow::Result<Vec<RemoteEntry>> {
        self.shell.list_recursive(self, remote_dir, excluded)
    }

    fn describe(&self) -> String {
        format!("sftp://{}", self.shell.target.label())
    }
}

// ---------------------------------------------------------------------------
// rsync over SSH
// ---------------------------------------------------------------------------

/// rsync over SSH for transfers; `ssh` for listing, mkdir and delete.
#[derive(Clone, Debug)]
pub struct RsyncTransport {
    shell: SshShell,
    rsync_program: String,
    root_path: String,
    transfer_timeout: Duration,
}

impl RsyncTransport {
    pub fn new(target: SshTarget, root_path: impl Into<String>) -> anyhow::Result<Self> {
        target.validate()?;
        let root_path = root_path.into();
        check_root_path(&root_path)?;
        Ok(Self {
            shell: SshShell {
                target,
                program: "ssh".into(),
                timeout: DEFAULT_COMMAND_TIMEOUT,
            },
            rsync_program: "rsync".into(),
            root_path,
            transfer_timeout: DEFAULT_TRANSFER_TIMEOUT,
        })
    }

    /// Uses other `ssh`/`rsync` executables (the ssh program is also used inside `-e`).
    pub fn with_programs(mut self, ssh: impl Into<String>, rsync: impl Into<String>) -> Self {
        self.shell.program = ssh.into();
        self.rsync_program = rsync.into();
        self
    }

    pub fn with_timeouts(mut self, command: Duration, transfer: Duration) -> Self {
        self.shell.timeout = command;
        self.transfer_timeout = transfer;
        self
    }

    pub fn target(&self) -> &SshTarget {
        &self.shell.target
    }

    fn transfer(&self, spec: CommandSpec) -> anyhow::Result<()> {
        exec::run_checked(
            &spec.with_program(&self.rsync_program),
            self.transfer_timeout,
            |o| ssh_hint(&o.stderr),
        )?;
        Ok(())
    }
}

impl Transport for RsyncTransport {
    fn rename(&self, source: &str, destination: &str) -> anyhow::Result<()> {
        let source = check_file_path(source)?;
        let destination = check_file_path(destination)?;
        self.shell.run(ssh_command(
            &self.shell.target,
            &format!(
                "mv -- {} {}",
                sh_quote(&shell_path(&source)),
                sh_quote(&shell_path(&destination))
            ),
        ))?;
        Ok(())
    }
    fn permissions(&self, path: &str, mode: u32) -> anyhow::Result<()> {
        let path = check_file_path(path)?;
        anyhow::ensure!(mode <= 0o7777, "Invalid permission bits");
        self.shell.run(ssh_command(
            &self.shell.target,
            &format!("chmod {mode:o} -- {}", sh_quote(&shell_path(&path))),
        ))?;
        Ok(())
    }
    fn upload(&self, local: &Path, remote: &str) -> anyhow::Result<()> {
        check_local_file(local)?;
        let remote = check_file_path(remote)?;
        self.transfer(rsync_upload_command(
            &self.shell.target,
            &self.shell.program,
            local,
            &remote,
        )?)
    }

    fn download(&self, remote: &str, local: &Path) -> anyhow::Result<()> {
        let remote = check_file_path(remote)?;
        download_via_temp(local, |tmp| {
            self.transfer(rsync_download_command(
                &self.shell.target,
                &self.shell.program,
                &remote,
                tmp,
            ))
        })
    }

    fn list(&self, remote_dir: &str) -> anyhow::Result<Vec<RemoteEntry>> {
        self.shell.list(remote_dir)
    }

    fn delete(&self, remote: &str) -> anyhow::Result<()> {
        self.shell.delete(remote)
    }

    fn mkdirs(&self, remote_dir: &str) -> anyhow::Result<()> {
        self.shell.mkdirs(remote_dir)
    }

    fn test_connection(&self) -> anyhow::Result<String> {
        self.shell.test(&self.root_path, &["rsync"])
    }

    fn list_recursive(
        &self,
        remote_dir: &str,
        excluded: &[String],
    ) -> anyhow::Result<Vec<RemoteEntry>> {
        self.shell.list_recursive(self, remote_dir, excluded)
    }

    fn describe(&self) -> String {
        format!("rsync://{}", self.shell.target.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Splits a POSIX shell command line into words (single/double quotes, backslashes).
    pub(crate) fn shell_words(s: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut cur = String::new();
        let mut in_word = false;
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\'' => {
                    in_word = true;
                    for c in chars.by_ref() {
                        if c == '\'' {
                            break;
                        }
                        cur.push(c);
                    }
                }
                '"' => {
                    in_word = true;
                    while let Some(c) = chars.next() {
                        match c {
                            '"' => break,
                            '\\' if matches!(chars.peek(), Some('"' | '\\' | '$' | '`')) => {
                                cur.push(chars.next().unwrap())
                            }
                            c => cur.push(c),
                        }
                    }
                }
                '\\' => {
                    in_word = true;
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                }
                c if c.is_whitespace() => {
                    if in_word {
                        words.push(std::mem::take(&mut cur));
                        in_word = false;
                    }
                }
                c => {
                    in_word = true;
                    cur.push(c);
                }
            }
        }
        if in_word {
            words.push(cur);
        }
        words
    }

    fn target() -> SshTarget {
        SshTarget::new(
            "example.com",
            2222,
            "deploy",
            Some(PathBuf::from("/home/me/.ssh/my key")),
        )
    }

    const OPTS: [&str; 10] = [
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
        "-o",
        "StrictHostKeyChecking=yes",
    ];

    fn strs(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }

    #[test]
    fn first_key_confirmation_requires_owned_bridge_and_never_auto_accepts() {
        let mut context = crate::transfer::Context::default();
        context.options.accept_new_host_keys = true;
        let target = target().with_accept_new_host_keys(true);
        let headless =
            crate::transfer::with_context(context.clone(), || ssh_command(&target, "true"));
        assert!(headless
            .args
            .iter()
            .any(|arg| arg == "StrictHostKeyChecking=yes"));
        assert!(!headless.args.iter().any(|arg| arg.contains("accept-new")));
        context
            .environment
            .push(("RUSTPY_ASKPASS_ADDR".into(), "127.0.0.1:1234".into()));
        let interactive = crate::transfer::with_context(context, || ssh_command(&target, "true"));
        assert!(interactive
            .args
            .iter()
            .any(|arg| arg == "StrictHostKeyChecking=ask"));
        assert!(interactive.args.iter().any(|arg| arg == "BatchMode=no"));
    }

    #[test]
    fn ssh_command_shape() {
        let spec = ssh_command(&target(), "uptime");
        assert_eq!(spec.program, "ssh");
        let mut expected: Vec<&str> = OPTS.to_vec();
        expected.extend([
            "-p",
            "2222",
            "-i",
            "/home/me/.ssh/my key",
            "--",
            "deploy@example.com",
            "uptime",
        ]);
        assert_eq!(strs(&spec.args), expected);

        let t = SshTarget::new("[2001:db8::1]", 22, "", None).with_accept_new_host_keys(true);
        assert_eq!(t.host, "2001:db8::1");
        let spec = ssh_command(&t, "true");
        assert_eq!(
            spec.args[OPTS.len()..],
            ["-p", "22", "--", "2001:db8::1", "true"]
        );
        assert_eq!(t.remote_prefix(), "[2001:db8::1]");
        assert_eq!(t.label(), "[2001:db8::1]:22");
        assert_eq!(target().label(), "deploy@example.com:2222");
    }

    #[test]
    fn remote_scripts_quote_paths() {
        let dir = "/var/www/my app/it's $HOME";
        let spec = ssh_list_command(&target(), dir);
        let remote = spec.args.last().unwrap();
        let words = shell_words(remote);
        assert_eq!(words.len(), 3, "{remote}");
        assert_eq!(words[..2], ["sh", "-c"]);
        assert_eq!(words[2], list_script(dir));
        let script = &words[2];
        let assignment = script
            .split("; ")
            .next()
            .unwrap()
            .strip_prefix("d=")
            .unwrap();
        assert_eq!(assignment, r"'/var/www/my app/it'\''s $HOME'");
        assert_eq!(shell_words(assignment), [dir]);
        assert!(script.contains(r#"-printf '%Y %s %T@ %f\0'"#), "{script}");

        let mk = ssh_mkdirs_command(&target(), "/srv/a \"b\"/c");
        assert_eq!(
            shell_words(mk.args.last().unwrap()),
            ["sh", "-c", "mkdir -p -- '/srv/a \"b\"/c'"]
        );
        let rm = ssh_delete_command(&target(), "/srv/old dir/");
        assert_eq!(
            shell_words(rm.args.last().unwrap()),
            ["sh", "-c", "rm -rf -- '/srv/old dir'"]
        );
        let test = ssh_test_command(&target(), "~/www", &[]);
        assert_eq!(
            shell_words(test.args.last().unwrap()),
            ["sh", "-c", "cd 'www' || exit 90; pwd"]
        );
        let test = ssh_test_command(&target(), "/srv/my app", &["rsync"]);
        assert_eq!(
            shell_words(test.args.last().unwrap()),
            ["sh", "-c", "cd '/srv/my app' || exit 90; pwd; command -v 'rsync' >/dev/null 2>&1 || { echo 'rsync is not installed on the server' >&2; exit 91; }"]
        );
        let dash = ssh_mkdirs_command(&target(), "-rf");
        assert_eq!(
            shell_words(dash.args.last().unwrap()),
            ["sh", "-c", "mkdir -p -- './-rf'"]
        );
        // Relative paths that `find` would parse as an operator get a `./` prefix too.
        for (dir, quoted) in [
            ("!x", "'./!x'"),
            ("(a b)", "'./(a b)'"),
            (",c", "'./,c'"),
            ("ok/(x)", "'ok/(x)'"),
        ] {
            let script = list_script(dir);
            assert!(script.starts_with(&format!("d={quoted};")), "{script}");
        }
    }

    #[test]
    fn recursive_script_prunes_names() {
        let spec = ssh_list_recursive_command(
            &target(),
            "/srv/app",
            &[
                ".git".to_string(),
                "*.pyc".to_string(),
                "docs/build".to_string(),
            ],
        );
        let words = shell_words(spec.args.last().unwrap());
        let script = &words[2];
        assert!(
            script.contains(
                r"\( -name '.git' -o -name '*.pyc' \) -prune -o -printf '%Y %s %T@ %P\0'"
            ),
            "{script}"
        );
        assert!(
            !script.contains("docs/build"),
            "anchored patterns are filtered locally"
        );
        assert!(script.contains("exit 97"));
        let plain = list_recursive_script("/srv", &[]);
        assert!(plain.contains("-mindepth 1 -printf"), "{plain}");
    }

    #[test]
    fn scp_commands() {
        let local = Path::new("/home/me/proj/a b/it's.py");
        let remote = "/var/www/a b/it's.py";
        let up = scp_upload_command(&target(), "ssh", local, remote, ScpMode::Sftp);
        assert_eq!(up.program, "scp");
        let mut expected: Vec<&str> = OPTS.to_vec();
        expected.extend([
            "-P",
            "2222",
            "-i",
            "/home/me/.ssh/my key",
            "-p",
            "--",
            "/home/me/proj/a b/it's.py",
            "deploy@example.com:/var/www/a b/it's.py",
        ]);
        assert_eq!(strs(&up.args), expected);

        let up = scp_upload_command(
            &target(),
            "ssh",
            local,
            remote,
            ScpMode::Legacy { strict_names: true },
        );
        assert_eq!(
            up.args.last().unwrap(),
            r"deploy@example.com:'/var/www/a b/it'\''s.py'"
        );
        assert!(
            !up.args.contains(&"-T".to_string()),
            "-T only matters for downloads"
        );
        let safe = scp_upload_command(
            &target(),
            "ssh",
            local,
            "/var/www/plain.py",
            ScpMode::Legacy { strict_names: true },
        );
        assert_eq!(
            safe.args.last().unwrap(),
            "deploy@example.com:/var/www/plain.py"
        );

        let down = scp_download_command(
            &target(),
            "/usr/local/bin/ssh",
            "/srv/x [1]*.py",
            Path::new("/tmp/x.py"),
            ScpMode::Sftp,
        );
        let n = down.args.len();
        assert_eq!(
            strs(&down.args[n - 6..]),
            [
                "-S",
                "/usr/local/bin/ssh",
                "-p",
                "--",
                r"deploy@example.com:/srv/x \[1\]\*.py",
                "/tmp/x.py"
            ]
        );
        let down = scp_download_command(
            &target(),
            "ssh",
            "/srv/a b.py",
            Path::new("/tmp/x.py"),
            ScpMode::Legacy { strict_names: true },
        );
        let n = down.args.len();
        assert_eq!(
            strs(&down.args[n - 4..]),
            ["-T", "--", "deploy@example.com:'/srv/a b.py'", "/tmp/x.py"]
        );
        let down = scp_download_command(
            &target(),
            "ssh",
            "/srv/a b.py",
            Path::new("/tmp/x.py"),
            ScpMode::Legacy {
                strict_names: false,
            },
        );
        assert!(!down.args.contains(&"-T".to_string()));
        assert!(
            !down.args.contains(&"-O".to_string()),
            "pre-9.0 clients do not know -O"
        );

        // OpenSSH 9+ forced back to the legacy protocol: `-O`, quoting and `-T` as for 8.x.
        let forced = scp_download_command(
            &target(),
            "ssh",
            "/srv/a b.py",
            Path::new("/tmp/x.py"),
            ScpMode::ForcedLegacy,
        );
        assert_eq!(forced.args[0], "-O");
        let n = forced.args.len();
        assert_eq!(
            strs(&forced.args[n - 4..]),
            ["-T", "--", "deploy@example.com:'/srv/a b.py'", "/tmp/x.py"]
        );
        let forced = scp_upload_command(&target(), "ssh", local, remote, ScpMode::ForcedLegacy);
        assert_eq!(forced.args[0], "-O");
        assert_eq!(
            forced.args.last().unwrap(),
            r"deploy@example.com:'/var/www/a b/it'\''s.py'"
        );
        assert!(
            !scp_upload_command(&target(), "ssh", local, remote, ScpMode::Sftp)
                .args
                .contains(&"-O".to_string())
        );

        let v6 = scp_upload_command(
            &SshTarget::new("::1", 22, "u", None),
            "ssh",
            Path::new("/a"),
            "b",
            ScpMode::Sftp,
        );
        assert_eq!(v6.args.last().unwrap(), "u@[::1]:b");
        let rel = scp_upload_command(
            &SshTarget::new("h", 22, "", None),
            "ssh",
            Path::new("c:d.py"),
            "-x",
            ScpMode::Sftp,
        );
        if cfg!(not(windows)) {
            assert_eq!(
                strs(&rel.args[rel.args.len() - 2..]),
                ["./c:d.py", "h:./-x"]
            );
        }
    }

    #[test]
    fn rsync_commands() {
        let t = target();
        let up = rsync_upload_command(&t, "ssh", Path::new("/p/a b.py"), "/srv/it's here/a b.py")
            .unwrap();
        assert_eq!(up.program, "rsync");
        assert_eq!(
            strs(&up.args),
            ["-az", "-L", "-s", "-e", "ssh -o BatchMode=yes -o ConnectTimeout=10 -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -o StrictHostKeyChecking=yes -p 2222 -i '/home/me/.ssh/my key'", "--", "/p/a b.py", "deploy@example.com:/srv/it's here/"],
            "the file goes into its parent directory and keeps its name"
        );
        let down = rsync_download_command(
            &SshTarget::new("h", 2200, "", Some("/k/o'brien".into())),
            "/opt/my ssh",
            "rel/x.py",
            Path::new("/p/x.py"),
        );
        assert_eq!(strs(&down.args[3..]), ["-e", "'/opt/my ssh' -o BatchMode=yes -o ConnectTimeout=10 -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -o StrictHostKeyChecking=yes -p 2200 -i '/k/o''brien'", "--", "h:rel/x.py", "/p/x.py"]);

        // Only segments containing `*?[` are escaped; backslashes elsewhere stay single.
        assert_eq!(
            rsync_escape_path("/srv/[app]/x[1]*.py"),
            r"/srv/\[app\]/x\[1\]\*.py"
        );
        assert_eq!(rsync_escape_path(r"/srv/a\b/c?.py"), r"/srv/a\b/c\?.py");
        assert_eq!(rsync_escape_path(r"/srv/a\b[1]"), r"/srv/a\\b\[1\]");
        assert_eq!(
            rsync_escape_path("/srv/plain dir/it's \"q\".py"),
            "/srv/plain dir/it's \"q\".py"
        );
        let up = rsync_upload_command(
            &t,
            "ssh",
            Path::new("/p/pages/[id].tsx"),
            "/srv/[app]/pages/[id].tsx",
        )
        .unwrap();
        assert_eq!(
            up.args.last().unwrap(),
            r"deploy@example.com:/srv/\[app\]/pages/",
            "the wildcard name itself is never sent"
        );
        let down =
            rsync_download_command(&t, "ssh", "/srv/[app]/pages/[id].tsx", Path::new("/p/x"));
        assert_eq!(
            down.args[down.args.len() - 2],
            r"deploy@example.com:/srv/\[app\]/pages/\[id\].tsx"
        );

        // The root, the login directory, option-like names and renamed targets.
        let dest = |local: &str, remote: &str| {
            rsync_upload_command(&t, "ssh", Path::new(local), remote)
                .map(|s| s.args.last().unwrap().clone())
        };
        assert_eq!(dest("/p/a.py", "/a.py").unwrap(), "deploy@example.com:/");
        assert_eq!(dest("/p/a.py", "~/a.py").unwrap(), "deploy@example.com:./");
        assert_eq!(dest("/p/-a.py", "-a.py").unwrap(), "deploy@example.com:./");
        assert_eq!(
            dest("/p/x.py", "-d/x.py").unwrap(),
            "deploy@example.com:./-d/"
        );
        assert_eq!(
            dest("/p/settings_prod.py", "/srv/app/settings.py").unwrap(),
            "deploy@example.com:/srv/app/settings.py"
        );
        assert!(
            dest("/p/a.py", "/srv/[a].py").is_err(),
            "a renamed target containing a wildcard is refused"
        );
        assert!(dest("/p/a.py", "/").is_err());
    }

    #[test]
    fn version_detection() {
        assert_eq!(
            scp_mode_from_version("OpenSSH_9.6p1 Ubuntu-3ubuntu13.5, OpenSSL 3.0.13 30 Jan 2024"),
            ScpMode::Sftp
        );
        assert_eq!(
            scp_mode_from_version("OpenSSH_10.0p2, OpenSSL 3.5.0"),
            ScpMode::Sftp
        );
        assert_eq!(
            scp_mode_from_version("OpenSSH_for_Windows_9.5p1, LibreSSL 3.8.2"),
            ScpMode::Sftp
        );
        assert_eq!(
            scp_mode_from_version("OpenSSH_for_Windows_8.1p1, LibreSSL 3.0.2"),
            ScpMode::Legacy { strict_names: true }
        );
        assert_eq!(
            scp_mode_from_version("OpenSSH_7.4p1, OpenSSL 1.0.2k-fips  26 Jan 2017"),
            ScpMode::Legacy {
                strict_names: false
            }
        );
        assert_eq!(
            scp_mode_from_version("Dropbear v2022.83"),
            ScpMode::Legacy {
                strict_names: false
            }
        );
    }

    #[test]
    fn validation_blocks_option_injection() {
        assert!(
            SshTarget::new("-oProxyCommand=touch /tmp/pwned", 22, "u", None)
                .validate()
                .is_err()
        );
        assert!(SshTarget::new("host name", 22, "u", None)
            .validate()
            .is_err());
        assert!(SshTarget::new("h", 22, "-l", None).validate().is_err());
        assert!(SshTarget::new("h", 22, "a@b", None).validate().is_err());
        assert!(SshTarget::new("h", 0, "u", None).validate().is_err());
        assert!(SshTarget::new("", 22, "u", None).validate().is_err());
        assert!(SshTarget::new("my-server.example.com", 22, "", None)
            .validate()
            .is_ok());
        assert!(SshTarget::new("fe80::1%eth0", 22, "deploy_user", None)
            .validate()
            .is_ok());
        assert!(SftpTransport::new(SshTarget::new("h", 22, "u", None), "/srv\n/x").is_err());
        // Even an invalid host could never be parsed as an option: it follows `--`.
        let spec = ssh_command(&SshTarget::new("-oX", 22, "", None), "true");
        let dash = spec.args.iter().position(|a| a == "--").unwrap();
        assert_eq!(spec.args[dash + 1], "-oX");
    }

    #[test]
    fn hints() {
        assert!(
            ssh_hint("deploy@h: Permission denied (publickey,password).")
                .unwrap()
                .contains("BatchMode")
        );
        assert!(ssh_hint("Host key verification failed.")
            .unwrap()
            .contains("host key"));
        assert!(
            ssh_hint("ssh: Could not resolve hostname nope: Name or service not known")
                .unwrap()
                .contains("host name")
        );
        assert!(ssh_hint("ssh: connect to host h port 22: Connection refused").is_some());
        assert!(ssh_hint("ssh: connect to host h port 22: Connection timed out").is_some());
        assert_eq!(ssh_hint("something else"), None);
        assert!(
            ssh_hint("subsystem request failed on channel 0\nscp: Connection closed")
                .unwrap()
                .contains("SFTP")
        );
        assert!(is_missing_sftp_subsystem(
            "Request for subsystem 'sftp' failed on channel 0\r\nscp: Connection closed"
        ));
        assert!(!is_missing_sftp_subsystem("scp: Connection closed"));
    }

    #[test]
    fn display() {
        let spec = ssh_mkdirs_command(&SshTarget::new("h", 22, "u", None), "/a b");
        let shown = spec.display();
        assert!(shown.starts_with("ssh -o BatchMode=yes"), "{shown}");
        assert_eq!(
            shell_words(&shown).last().unwrap(),
            spec.args.last().unwrap()
        );
    }

    /// The remote scripts are valid POSIX shell: run them locally with `sh -c`.
    #[cfg(unix)]
    #[test]
    fn scripts_run_in_a_local_shell() {
        // The listing script needs GNU find (`-printf`); BSD find skips the check.
        if !std::process::Command::new("find")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            eprintln!("GNU find not available; skipping");
            return;
        }
        use crate::test_util::TempDir;
        let tmp = TempDir::new("ssh-scripts");
        let base = tmp.path().join("my dir 'q'");
        for name in [
            "a b.txt",
            "it's.py",
            "$HOME.txt",
            "x\"y.md",
            "sub dir/inner.py",
            ".git/HEAD",
            "sub dir/m.pyc",
        ] {
            let p = base.join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "12345").unwrap();
        }
        let dir = base.to_string_lossy().into_owned();
        let sh = |remote: &str| {
            let words = shell_words(remote);
            assert_eq!(words[..2], ["sh", "-c"]);
            exec::run(
                &CommandSpec::new("sh").arg("-c").arg(&words[2]),
                Duration::from_secs(20),
            )
            .unwrap()
        };
        let out = sh(ssh_list_command(&target(), &dir).args.last().unwrap());
        assert!(out.success, "{}", out.stderr);
        let entries = if out.stdout.contains(&0) {
            parse_find_output(&out.stdout)
        } else {
            parse_ls_output(&out.stdout_text(), unix_now())
        };
        let mut names: Vec<(String, bool)> =
            entries.iter().map(|e| (e.name.clone(), e.is_dir)).collect();
        names.sort();
        let expected: Vec<(String, bool)> = [
            ("$HOME.txt", false),
            (".git", true),
            ("a b.txt", false),
            ("it's.py", false),
            ("sub dir", true),
            ("x\"y.md", false),
        ]
        .iter()
        .map(|(n, d)| (n.to_string(), *d))
        .collect();
        assert_eq!(names, expected);
        let a = entries.iter().find(|e| e.name == "a b.txt").unwrap();
        assert_eq!(a.size, 5);
        assert!(a.mtime.unwrap() > 1_600_000_000);

        let out = sh(
            ssh_list_recursive_command(&target(), &dir, &crate::default_excluded())
                .args
                .last()
                .unwrap(),
        );
        assert!(out.success, "{}", out.stderr);
        let mut names: Vec<String> = parse_find_output(&out.stdout)
            .into_iter()
            .map(|e| e.name)
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                "$HOME.txt",
                "a b.txt",
                "it's.py",
                "sub dir",
                "sub dir/inner.py",
                "x\"y.md"
            ]
        );

        let missing = sh(
            ssh_list_recursive_command(&target(), &format!("{dir}/nope"), &[])
                .args
                .last()
                .unwrap(),
        );
        assert!(missing.success && missing.stdout.is_empty());

        let new_dir = format!("{dir}/new 'dir'/deeper");
        assert!(sh(ssh_mkdirs_command(&target(), &new_dir).args.last().unwrap()).success);
        assert!(Path::new(&new_dir).is_dir());
        let out = sh(ssh_test_command(&target(), &new_dir, &["sh"])
            .args
            .last()
            .unwrap());
        assert!(out.success, "{}", out.stderr);
        assert_eq!(out.stdout_text().trim(), new_dir);
        let out = sh(
            ssh_test_command(&target(), &new_dir, &["sh", "rustpy-no-such-tool"])
                .args
                .last()
                .unwrap(),
        );
        assert_eq!(out.code, Some(TEST_MISSING_TOOL_EXIT));
        assert!(
            out.stderr
                .contains("rustpy-no-such-tool is not installed on the server"),
            "{}",
            out.stderr
        );
        let out = sh(
            ssh_test_command(&target(), &format!("{dir}/missing"), &["sh"])
                .args
                .last()
                .unwrap(),
        );
        assert_eq!(out.code, Some(TEST_NO_ROOT_EXIT));
        assert!(
            sh(ssh_delete_command(&target(), &format!("{dir}/new 'dir'"))
                .args
                .last()
                .unwrap())
            .success
        );
        assert!(!Path::new(&format!("{dir}/new 'dir'")).exists());
        assert!(base.join("a b.txt").exists(), "siblings are untouched");

        // The `ls -la` fallback of `list_script` parses on this system, and its dates are UTC.
        let ls = exec::run(
            &CommandSpec::new("sh")
                .arg("-c")
                .arg(format!("TZ=UTC0 LC_ALL=C ls -la {}/", sh_quote(&dir))),
            Duration::from_secs(20),
        )
        .unwrap();
        let ls_entries = parse_ls_output(&ls.stdout_text(), unix_now());
        let mut names: Vec<String> = ls_entries.iter().map(|e| e.name.clone()).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "$HOME.txt",
                ".git",
                "a b.txt",
                "it's.py",
                "sub dir",
                "x\"y.md"
            ]
        );
        let ls_a = ls_entries.iter().find(|e| e.name == "a b.txt").unwrap();
        assert_eq!(
            (ls_a.size, ls_a.mtime),
            (5, a.mtime.map(|t| t - t.rem_euclid(60))),
            "same time as find, to the minute"
        );
    }
}
