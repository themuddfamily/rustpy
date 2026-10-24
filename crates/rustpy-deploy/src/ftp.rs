//! FTP deployment through the `curl` command-line tool.
//!
//! * upload: `curl -sS --ftp-create-dirs -T <file> ftp://host:port/<path>` (then `MFMT`/`MDTM`
//!   to keep the modification time, ignored when the server does not support them),
//! * download: `curl -sS -R -o <file> <url>`,
//! * listing: `curl -X MLSD <dir-url>/`, falling back to plain `LIST` parsing,
//! * delete: `curl -Q "DELE <path>"` (`RMD` for directories), mkdir: `--ftp-create-dirs`.
//!
//! The password never appears on the command line: it is passed in a curl config read from
//! stdin (`--config -`). Without a password, `~/.netrc` is consulted (`--netrc-optional`).
//! FTP URLs address paths relative to the login directory; absolute paths start with `%2F`.

use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use anyhow::{bail, Context as _};

use crate::config::has_control_chars;
use crate::exec::{self, CommandSpec, ExecError, Output};
use crate::listing::{ftp_timestamp, parse_ls_output, parse_mlsd_output};
use crate::paths::{
    is_root_like, join_remote, normalize_remote, percent_encode_segment, remote_file_name,
    remote_parent,
};
use crate::ssh::{DEFAULT_COMMAND_TIMEOUT, DEFAULT_TRANSFER_TIMEOUT};
use crate::{download_via_temp, mtime_secs, unix_now, walk_with_root, RemoteEntry, Transport};

/// FTP server and credentials.
#[derive(Clone, PartialEq, Eq)]
pub struct FtpTarget {
    pub host: String,
    pub port: u16,
    /// Empty for anonymous FTP.
    pub user: String,
    /// Plain-text password; `None` uses `~/.netrc` (or an empty password).
    pub password: Option<String>,
}

impl fmt::Debug for FtpTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FtpTarget")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl FtpTarget {
    /// Brackets around an IPv6 address are removed.
    pub fn new(
        host: impl Into<String>,
        port: u16,
        user: impl Into<String>,
        password: Option<String>,
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
            password,
        }
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        check_host(&self.host).map_err(anyhow::Error::msg)?;
        if has_control_chars(&self.user) || self.password.as_deref().is_some_and(has_control_chars)
        {
            bail!("FTP user name and password must not contain control characters");
        }
        if self.user.contains(':') {
            bail!("FTP user name must not contain ':'");
        }
        if self.port == 0 {
            bail!("FTP port must be between 1 and 65535");
        }
        Ok(())
    }

    /// `user@host:port`
    pub fn label(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.user.is_empty() {
            format!("{host}:{}", self.port)
        } else {
            format!("{}@{host}:{}", self.user, self.port)
        }
    }
}

pub(crate) fn check_host(host: &str) -> Result<(), String> {
    crate::ssh::check_host(host)
}

/// `ftp://[user@]host:port/<path>` with percent-encoded segments; absolute paths start with
/// `%2F`; directories end with `/`. The user is only part of the URL when no password is
/// configured (so `--netrc-optional` can find it).
pub fn ftp_url(target: &FtpTarget, remote: &str, is_dir: bool) -> String {
    let host = if target.host.contains(':') {
        format!("[{}]", target.host)
    } else {
        target.host.clone()
    };
    let userinfo = if target.password.is_none() && !target.user.is_empty() {
        format!("{}@", percent_encode_userinfo(&target.user))
    } else {
        String::new()
    };
    let p = normalize_remote(remote);
    let encode = |s: &str| {
        s.split('/')
            .filter(|seg| !seg.is_empty())
            .map(percent_encode_segment)
            .collect::<Vec<_>>()
            .join("/")
    };
    let mut path = if let Some(rest) = p.strip_prefix('/') {
        let rest = encode(rest);
        if rest.is_empty() {
            "%2F".to_string()
        } else {
            format!("%2F{rest}")
        }
    } else if p == "." {
        String::new()
    } else {
        encode(&p)
    };
    if is_dir && !path.is_empty() {
        path.push('/');
    }
    format!("ftp://{userinfo}{host}:{}/{path}", target.port)
}

/// Percent-encodes the user name of a URL: only RFC 3986 unreserved characters are kept, so
/// `@`, `:` and `/` cannot end the user info early.
fn percent_encode_userinfo(user: &str) -> String {
    let mut out = String::with_capacity(user.len());
    for b in user.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Escapes a value for a double-quoted string in a curl config file.
fn curl_config_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

fn curl_base(target: &FtpTarget) -> CommandSpec {
    let spec = CommandSpec::new("curl").args(["-sS", "--globoff", "--connect-timeout", "10"]);
    match (&target.password, target.user.is_empty()) {
        (Some(pw), false) => spec.args(["--config", "-"]).with_stdin(format!(
            "user = \"{}:{}\"\n",
            curl_config_escape(&target.user),
            curl_config_escape(pw)
        )),
        (None, false) => spec.arg("--netrc-optional"),
        _ => spec,
    }
}

/// FTP command arguments are the rest of the line: CR/LF would inject commands.
fn ftp_arg(s: &str) -> String {
    s.chars().filter(|c| *c != '\r' && *c != '\n').collect()
}

/// `curl -sS --ftp-create-dirs -T <local> [-Q "-*MFMT <time> <name>" -Q "-*MDTM <time> <name>"] <url>`.
/// The post-transfer commands keep `mtime` (Unix seconds) on servers supporting them; the
/// `*` prefix makes curl ignore their failure.
pub fn curl_upload_command(
    target: &FtpTarget,
    local: &Path,
    remote: &str,
    mtime: Option<i64>,
) -> CommandSpec {
    let mut spec = curl_base(target)
        .args([
            "--speed-time",
            "60",
            "--speed-limit",
            "1",
            "--ftp-create-dirs",
            "-T",
        ])
        .arg(local.to_string_lossy());
    if let Some(t) = mtime {
        let (ts, name) = (ftp_timestamp(t), ftp_arg(&remote_file_name(remote)));
        spec = spec
            .arg("-Q")
            .arg(format!("-*MFMT {ts} {name}"))
            .arg("-Q")
            .arg(format!("-*MDTM {ts} {name}"));
    }
    spec.arg(ftp_url(target, remote, false))
}

/// `curl -sS -R -o <local> <url>` (`-R` keeps the server's modification time).
pub fn curl_download_command(target: &FtpTarget, remote: &str, local: &Path) -> CommandSpec {
    curl_base(target)
        .args(["--speed-time", "60", "--speed-limit", "1", "-R", "-o"])
        .arg(local.to_string_lossy())
        .arg(ftp_url(target, remote, false))
}

/// Directory listing: `MLSD` (machine-readable, UTC times) or the default `LIST`.
pub fn curl_list_command(target: &FtpTarget, dir: &str, mlsd: bool) -> CommandSpec {
    let spec = curl_base(target);
    let spec = if mlsd {
        spec.args(["-X", "MLSD"])
    } else {
        spec
    };
    spec.arg(ftp_url(target, dir, true))
}

/// Creates `dir` and its parents (curl creates missing directories while changing into them).
pub fn curl_mkdirs_command(target: &FtpTarget, dir: &str) -> CommandSpec {
    curl_base(target)
        .arg("--ftp-create-dirs")
        .arg(ftp_url(target, dir, true))
}

/// `DELE <path>` (or `RMD <path>` for an empty directory), sent right after login, followed by
/// a listing of the parent directory (discarded).
pub fn curl_delete_command(target: &FtpTarget, path: &str, is_dir: bool) -> CommandSpec {
    let p = normalize_remote(path);
    let parent = remote_parent(&p).unwrap_or_else(|| ".".to_string());
    let verb = if is_dir { "RMD" } else { "DELE" };
    curl_base(target)
        .arg("-Q")
        .arg(format!("{verb} {}", ftp_arg(&p)))
        .arg(ftp_url(target, &parent, true))
}

/// A user-facing hint for a curl exit code.
pub(crate) fn curl_hint(code: Option<i32>) -> Option<String> {
    let hint = match code? {
        6 => "Could not resolve the host name.",
        7 => "Could not connect; check the host, port and firewall.",
        9 => "The server denied access to the remote directory (it may not exist).",
        28 => "The operation timed out.",
        67 => "Login denied; check the user name and password (or ~/.netrc).",
        19 | 78 => "The remote file does not exist or cannot be read.",
        25 => "The upload was rejected; check write permissions on the server.",
        21 => "The server rejected an FTP command (DELE/RMD); check permissions.",
        _ => return None,
    };
    Some(hint.to_string())
}

/// Errors that a retry with another listing command cannot fix.
fn is_definitive(e: &ExecError) -> bool {
    match e {
        ExecError::Failed { code, .. } => matches!(code, Some(6 | 7 | 9 | 28 | 67)),
        _ => true,
    }
}

const MLSD_UNKNOWN: u8 = 0;
const MLSD_YES: u8 = 1;
const MLSD_NO: u8 = 2;

/// FTP deployment through `curl`.
#[derive(Debug)]
pub struct FtpTransport {
    target: FtpTarget,
    root_path: String,
    curl_program: String,
    timeout: Duration,
    transfer_timeout: Duration,
    mlsd: AtomicU8,
}

impl FtpTransport {
    pub fn new(target: FtpTarget, root_path: impl Into<String>) -> anyhow::Result<Self> {
        target.validate()?;
        let root_path = root_path.into();
        check_path(&root_path)?;
        Ok(Self {
            target,
            root_path,
            curl_program: "curl".into(),
            timeout: DEFAULT_COMMAND_TIMEOUT,
            transfer_timeout: DEFAULT_TRANSFER_TIMEOUT,
            mlsd: AtomicU8::new(MLSD_UNKNOWN),
        })
    }

    /// Uses another `curl` executable.
    pub fn with_program(mut self, curl: impl Into<String>) -> Self {
        self.curl_program = curl.into();
        self
    }

    pub fn with_timeouts(mut self, command: Duration, transfer: Duration) -> Self {
        self.timeout = command;
        self.transfer_timeout = transfer;
        self
    }

    pub fn target(&self) -> &FtpTarget {
        &self.target
    }

    fn run(&self, spec: CommandSpec, timeout: Duration) -> Result<Output, ExecError> {
        exec::run_checked(&spec.with_program(&self.curl_program), timeout, |o| {
            curl_hint(o.code)
        })
    }

    fn list_unsorted(&self, dir: &str) -> Result<Vec<RemoteEntry>, ExecError> {
        if self.mlsd.load(Ordering::Relaxed) != MLSD_NO {
            match self.run(curl_list_command(&self.target, dir, true), self.timeout) {
                Ok(out) => {
                    self.mlsd.store(MLSD_YES, Ordering::Relaxed);
                    return Ok(parse_mlsd_output(&out.stdout_text()));
                }
                Err(e) if is_definitive(&e) => return Err(e),
                Err(e) => {
                    log::debug!("deployment: MLSD failed, using LIST: {e}");
                    let _ = self.mlsd.compare_exchange(
                        MLSD_UNKNOWN,
                        MLSD_NO,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    );
                }
            }
        }
        let out = self.run(curl_list_command(&self.target, dir, false), self.timeout)?;
        Ok(parse_ls_output(&out.stdout_text(), unix_now()))
    }

    fn delete_dir_contents(&self, dir: &str, depth: usize) -> anyhow::Result<()> {
        if depth > 64 {
            bail!("{dir} is nested too deeply");
        }
        for e in self.list(dir)? {
            let child = join_remote(dir, &e.name);
            if e.is_dir {
                self.delete_dir_contents(&child, depth + 1)?;
                self.run(
                    curl_delete_command(&self.target, &child, true),
                    self.timeout,
                )?;
            } else {
                self.run(
                    curl_delete_command(&self.target, &child, false),
                    self.timeout,
                )?;
            }
        }
        Ok(())
    }
}

fn check_path(remote: &str) -> anyhow::Result<()> {
    if has_control_chars(remote) {
        bail!("FTP paths must not contain control characters: {remote:?}");
    }
    Ok(())
}

impl Transport for FtpTransport {
    fn rename(&self, source: &str, destination: &str) -> anyhow::Result<()> {
        check_path(source)?;
        check_path(destination)?;
        anyhow::ensure!(
            !is_root_like(source) && !is_root_like(destination),
            "Cannot rename a deployment root"
        );
        let spec = curl_base(&self.target).args([
            "-Q".into(),
            format!("RNFR {}", ftp_arg(source)),
            "-Q".into(),
            format!("RNTO {}", ftp_arg(destination)),
            ftp_url(
                &self.target,
                &remote_parent(source).unwrap_or_default(),
                true,
            ),
        ]);
        self.run(spec, self.timeout)?;
        Ok(())
    }
    fn permissions(&self, path: &str, mode: u32) -> anyhow::Result<()> {
        check_path(path)?;
        anyhow::ensure!(mode <= 0o7777, "Invalid permission bits");
        let spec = curl_base(&self.target).args([
            "-Q".into(),
            format!("SITE CHMOD {mode:o} {}", ftp_arg(path)),
            ftp_url(&self.target, &remote_parent(path).unwrap_or_default(), true),
        ]);
        self.run(spec, self.timeout)?;
        Ok(())
    }
    fn coarse_times(&self) -> bool {
        true
    }

    fn upload(&self, local: &Path, remote: &str) -> anyhow::Result<()> {
        check_path(remote)?;
        let meta =
            std::fs::metadata(local).with_context(|| format!("cannot read {}", local.display()))?;
        if meta.is_dir() {
            bail!("{} is a directory", local.display());
        }
        if is_root_like(remote) {
            bail!("{remote:?} is not a file path");
        }
        self.run(
            curl_upload_command(
                &self.target,
                local,
                remote,
                if crate::transfer::current().options.preserve_timestamps {
                    mtime_secs(&meta)
                } else {
                    None
                },
            ),
            self.transfer_timeout,
        )?;
        Ok(())
    }

    fn download(&self, remote: &str, local: &Path) -> anyhow::Result<()> {
        check_path(remote)?;
        if is_root_like(remote) {
            bail!("{remote:?} is not a file path");
        }
        download_via_temp(local, |tmp| {
            self.run(
                curl_download_command(&self.target, remote, tmp),
                self.transfer_timeout,
            )?;
            Ok(())
        })
    }

    fn list(&self, remote_dir: &str) -> anyhow::Result<Vec<RemoteEntry>> {
        check_path(remote_dir)?;
        let mut entries = self.list_unsorted(remote_dir)?;
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    fn delete(&self, remote: &str) -> anyhow::Result<()> {
        check_path(remote)?;
        if is_root_like(remote) {
            bail!("refusing to delete {remote:?}");
        }
        let path = normalize_remote(remote);
        let dele_err = match self.run(
            curl_delete_command(&self.target, &path, false),
            self.timeout,
        ) {
            Ok(_) => return Ok(()),
            Err(e) if is_definitive(&e) && e.exit_code() != Some(9) => return Err(e.into()),
            Err(e) => e,
        };
        // Not a deletable file: maybe a directory, or already gone (both answer 550).
        if self.list(&path).is_err() {
            // Gone if the nearest listable ancestor lacks the next path component
            // (the parent itself may be missing too).
            let mut child = path.clone();
            for _ in 0..64 {
                let Some(parent) = remote_parent(&child) else {
                    break;
                };
                match self.list(&parent) {
                    Ok(entries) => {
                        let name = remote_file_name(&child);
                        if !entries.iter().any(|e| e.name == name) {
                            return Ok(());
                        }
                        break;
                    }
                    Err(_) if parent != "/" && parent != "." => child = parent,
                    Err(_) => break,
                }
            }
            return Err(anyhow::Error::from(dele_err).context(format!("cannot delete {path}")));
        }
        self.delete_dir_contents(&path, 0)?;
        self.run(curl_delete_command(&self.target, &path, true), self.timeout)?;
        Ok(())
    }

    fn mkdirs(&self, remote_dir: &str) -> anyhow::Result<()> {
        check_path(remote_dir)?;
        let d = normalize_remote(remote_dir);
        if d == "/" || d == "." {
            return Ok(());
        }
        self.run(curl_mkdirs_command(&self.target, &d), self.timeout)?;
        Ok(())
    }

    fn test_connection(&self) -> anyhow::Result<String> {
        let entries = self
            .list(&self.root_path)
            .with_context(|| format!("FTP connection to {} failed", self.target.label()))?;
        Ok(format!(
            "Connected to ftp://{}; root path {} has {} entries",
            self.target.label(),
            normalize_remote(&self.root_path),
            entries.len()
        ))
    }

    fn list_recursive(
        &self,
        remote_dir: &str,
        excluded: &[String],
    ) -> anyhow::Result<Vec<RemoteEntry>> {
        check_path(remote_dir)?;
        let first = match self.list(remote_dir) {
            Ok(entries) => entries,
            // CWD failed: the directory does not exist (yet).
            Err(e) if e.downcast_ref::<ExecError>().and_then(ExecError::exit_code) == Some(9) => {
                return Ok(Vec::new())
            }
            Err(e) => return Err(e),
        };
        walk_with_root(self, remote_dir, excluded, first)
    }

    fn creates_parent_dirs(&self) -> bool {
        true
    }

    fn describe(&self) -> String {
        format!("ftp://{}", self.target.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_password() -> FtpTarget {
        FtpTarget::new("ftp.example.com", 21, "bob", Some("p\"w\\d".into()))
    }

    fn strs(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }

    #[test]
    fn urls() {
        let t = with_password();
        assert_eq!(
            ftp_url(&t, "/var/www/a b#1?.py", false),
            "ftp://ftp.example.com:21/%2Fvar/www/a%20b%231%3F.py"
        );
        assert_eq!(
            ftp_url(&t, "/var/www/", true),
            "ftp://ftp.example.com:21/%2Fvar/www/"
        );
        assert_eq!(ftp_url(&t, "/", true), "ftp://ftp.example.com:21/%2F/");
        assert_eq!(
            ftp_url(&t, "htdocs/it's [x]%.txt", false),
            "ftp://ftp.example.com:21/htdocs/it%27s%20%5Bx%5D%25.txt"
        );
        assert_eq!(ftp_url(&t, ".", true), "ftp://ftp.example.com:21/");
        assert_eq!(
            ftp_url(&t, "~/site", true),
            "ftp://ftp.example.com:21/site/"
        );
        assert_eq!(
            ftp_url(&t, "/café/ü.py", false),
            "ftp://ftp.example.com:21/%2Fcaf%C3%A9/%C3%BC.py"
        );
        let netrc = FtpTarget::new("[2001:db8::2]", 2121, "us er@x", None);
        assert_eq!(
            ftp_url(&netrc, "a", false),
            "ftp://us%20er%40x@[2001:db8::2]:2121/a"
        );
        let anon = FtpTarget::new("h", 21, "", None);
        assert_eq!(ftp_url(&anon, "pub/x", false), "ftp://h:21/pub/x");
    }

    #[test]
    fn credentials_stay_off_the_command_line() {
        let spec = curl_list_command(&with_password(), "/www", false);
        assert_eq!(spec.program, "curl");
        assert_eq!(
            strs(&spec.args),
            [
                "-sS",
                "--globoff",
                "--connect-timeout",
                "10",
                "--config",
                "-",
                "ftp://ftp.example.com:21/%2Fwww/"
            ]
        );
        assert_eq!(spec.stdin.as_deref(), Some("user = \"bob:p\\\"w\\\\d\"\n"));
        assert!(!spec.display().contains("p\"w"));

        let netrc = curl_list_command(&FtpTarget::new("h", 21, "bob", None), "www", true);
        assert_eq!(
            strs(&netrc.args),
            [
                "-sS",
                "--globoff",
                "--connect-timeout",
                "10",
                "--netrc-optional",
                "-X",
                "MLSD",
                "ftp://bob@h:21/www/"
            ]
        );
        assert_eq!(netrc.stdin, None);

        let anon = curl_list_command(
            &FtpTarget::new("h", 21, "", Some("ignored".into())),
            ".",
            false,
        );
        assert_eq!(
            strs(&anon.args),
            ["-sS", "--globoff", "--connect-timeout", "10", "ftp://h:21/"]
        );
        assert_eq!(anon.stdin, None);
        assert!(!format!("{:?}", with_password()).contains("p\"w"));
    }

    #[test]
    fn transfer_commands() {
        let t = with_password();
        let up = curl_upload_command(
            &t,
            Path::new("/home/me/proj/a b.py"),
            "/www/a b.py",
            Some(1_705_314_600),
        );
        assert_eq!(
            strs(&up.args[6..]),
            [
                "--speed-time",
                "60",
                "--speed-limit",
                "1",
                "--ftp-create-dirs",
                "-T",
                "/home/me/proj/a b.py",
                "-Q",
                "-*MFMT 20240115103000 a b.py",
                "-Q",
                "-*MDTM 20240115103000 a b.py",
                "ftp://ftp.example.com:21/%2Fwww/a%20b.py"
            ]
        );
        let up = curl_upload_command(&t, Path::new("/p/x"), "rel/x", None);
        assert!(!up.args.iter().any(|a| a == "-Q"));
        assert_eq!(up.args.last().unwrap(), "ftp://ftp.example.com:21/rel/x");
        let evil = curl_upload_command(&t, Path::new("/p/x"), "/www/a\r\nDELE b", Some(0));
        assert!(
            evil.args
                .iter()
                .all(|a| !a.contains('\n') && !a.contains('\r')),
            "{:?}",
            evil.args
        );

        let down = curl_download_command(&t, "/www/it's.py", Path::new("/tmp/x y.py"));
        assert_eq!(
            strs(&down.args[6..]),
            [
                "--speed-time",
                "60",
                "--speed-limit",
                "1",
                "-R",
                "-o",
                "/tmp/x y.py",
                "ftp://ftp.example.com:21/%2Fwww/it%27s.py"
            ]
        );

        let mk = curl_mkdirs_command(&t, "/www/new dir/sub");
        assert_eq!(
            strs(&mk.args[6..]),
            [
                "--ftp-create-dirs",
                "ftp://ftp.example.com:21/%2Fwww/new%20dir/sub/"
            ]
        );

        let rm = curl_delete_command(&t, "/www/old file.py", false);
        assert_eq!(
            strs(&rm.args[6..]),
            [
                "-Q",
                "DELE /www/old file.py",
                "ftp://ftp.example.com:21/%2Fwww/"
            ]
        );
        let rmd = curl_delete_command(&t, "old dir/", true);
        assert_eq!(
            strs(&rmd.args[6..]),
            ["-Q", "RMD old dir", "ftp://ftp.example.com:21/"]
        );
    }

    #[test]
    fn validation_and_hints() {
        assert!(FtpTarget::new("ftp.example.com", 21, "u", None)
            .validate()
            .is_ok());
        assert!(FtpTarget::new("ftp.example.com/x", 21, "u", None)
            .validate()
            .is_err());
        assert!(FtpTarget::new("-x", 21, "u", None).validate().is_err());
        assert!(FtpTarget::new("h", 21, "u", Some("a\nb".into()))
            .validate()
            .is_err());
        assert!(FtpTarget::new("h", 0, "u", None).validate().is_err());
        assert!(
            FtpTarget::new("h", 21, "a:b", Some("pw".into()))
                .validate()
                .is_err(),
            "curl splits user:password at the first ':'"
        );
        assert!(FtpTransport::new(FtpTarget::new("h", 21, "u", None), "/www\r\n").is_err());
        assert!(curl_hint(Some(67)).unwrap().contains("Login denied"));
        assert_eq!(curl_hint(Some(0)), None);
        assert_eq!(curl_hint(None), None);
        assert_eq!(FtpTarget::new("::1", 21, "", None).label(), "[::1]:21");
    }

    #[cfg(unix)]
    #[test]
    fn missing_curl_is_reported() {
        let t = FtpTransport::new(FtpTarget::new("h", 21, "u", None), "/")
            .unwrap()
            .with_program("rustpy-deploy-no-such-curl");
        let err = t.list("/").unwrap_err();
        assert!(format!("{err:#}").contains("was not found"), "{err:#}");
    }
}
