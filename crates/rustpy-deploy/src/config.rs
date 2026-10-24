//! Deployment configuration (`<project>/.rustpy/deployment.json`).
//!
//! A project has a list of [`Server`]s, an optional default server (target of automatic
//! uploads), an [`AutoUpload`] mode and a list of excluded glob patterns (see [`crate::glob`]).

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use crate::glob::ExcludeSet;
use crate::paths::{normalize_local, rel_string, remote_path};

/// File name of the configuration inside `<project>/.rustpy/`.
pub const CONFIG_FILE: &str = "deployment.json";

/// Patterns excluded by default: `.git`, `.venv`, `__pycache__`, `.rustpy`, `*.pyc`.
pub const DEFAULT_EXCLUDED: [&str; 5] = [".git", ".venv", "__pycache__", ".rustpy", "*.pyc"];

/// Shown by the UI next to the FTP password field.
pub const PLAINTEXT_PASSWORD_WARNING: &str = "The FTP password is stored in plain text in .rustpy/deployment.json. Prefer SFTP with a key, or leave the password empty and use ~/.netrc.";

pub fn default_excluded() -> Vec<String> {
    DEFAULT_EXCLUDED.iter().map(|s| s.to_string()).collect()
}

/// When files are uploaded to the default server automatically.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoUpload {
    #[default]
    Never,
    /// Only when the user saves explicitly (Ctrl+S / Save All), not on auto-save.
    OnExplicitSave,
    /// On every save, including auto-save and external changes.
    Always,
}

impl AutoUpload {
    pub const ALL: [AutoUpload; 3] = [
        AutoUpload::Never,
        AutoUpload::OnExplicitSave,
        AutoUpload::Always,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AutoUpload::Never => "Never",
            AutoUpload::OnExplicitSave => "On explicit save action",
            AutoUpload::Always => "Always",
        }
    }
}

fn default_ssh_port() -> u16 {
    22
}

fn default_ftp_port() -> u16 {
    21
}

/// How a server is reached. Serialized with a `"type"` tag: `local`, `sftp`, `rsync`, `ftp`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerKind {
    /// A local or mounted folder; remote paths are relative to `root` (`/` is `root` itself).
    Local { root: PathBuf },
    /// SSH server: `ssh` for commands, `scp` for transfers. Authentication must not need a
    /// prompt (key file, ssh-agent or `~/.ssh/config`), because BatchMode is used.
    Sftp {
        host: String,
        #[serde(default = "default_ssh_port")]
        port: u16,
        #[serde(default)]
        user: String,
        #[serde(default)]
        key_file: Option<PathBuf>,
    },
    /// rsync over SSH (`rsync -az -e "ssh -p PORT"`); listing/deleting use `ssh`.
    Rsync {
        host: String,
        #[serde(default)]
        user: String,
        #[serde(default = "default_ssh_port")]
        port: u16,
    },
    /// FTP through the `curl` CLI. **Warning:** a `password` here is stored in plain
    /// text (see [`PLAINTEXT_PASSWORD_WARNING`]); the IDE keeps passwords in its
    /// password safe instead and leaves this empty. With no password, `~/.netrc`
    /// is consulted.
    Ftp {
        host: String,
        #[serde(default = "default_ftp_port")]
        port: u16,
        #[serde(default)]
        user: String,
        #[serde(default)]
        password: Option<String>,
        /// "Save password": keep the password in the password safe across
        /// sessions; otherwise it is forgotten when the IDE exits.
        #[serde(default = "default_true", skip_serializing_if = "is_true")]
        save_password: bool,
    },
}

fn default_true() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

impl fmt::Debug for ServerKind {
    // Hand-written so that logging a server never prints the FTP password.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServerKind::Local { root } => f.debug_struct("Local").field("root", root).finish(),
            ServerKind::Sftp {
                host,
                port,
                user,
                key_file,
            } => f
                .debug_struct("Sftp")
                .field("host", host)
                .field("port", port)
                .field("user", user)
                .field("key_file", key_file)
                .finish(),
            ServerKind::Rsync { host, user, port } => f
                .debug_struct("Rsync")
                .field("host", host)
                .field("user", user)
                .field("port", port)
                .finish(),
            ServerKind::Ftp {
                host,
                port,
                user,
                password,
                save_password,
            } => f
                .debug_struct("Ftp")
                .field("host", host)
                .field("port", port)
                .field("user", user)
                .field("password", &password.as_ref().map(|_| "<redacted>"))
                .field("save_password", save_password)
                .finish(),
        }
    }
}

impl ServerKind {
    /// Human-readable type, e.g. for the server list.
    pub fn type_label(&self) -> &'static str {
        match self {
            ServerKind::Local { .. } => "Local or mounted folder",
            ServerKind::Sftp { .. } => "SFTP",
            ServerKind::Rsync { .. } => "rsync over SSH",
            ServerKind::Ftp { .. } => "FTP",
        }
    }

    pub fn default_port(&self) -> Option<u16> {
        match self {
            ServerKind::Local { .. } => None,
            ServerKind::Sftp { .. } | ServerKind::Rsync { .. } => Some(22),
            ServerKind::Ftp { .. } => Some(21),
        }
    }
}

/// Maps a project directory to a deployment directory.
///
/// * `local` is relative to the project root (`""` is the project itself); an absolute path is
///   used as is.
/// * `remote` is relative to the server's `root_path` (the "Deployment path"); a
///   leading `/` is ignored, so `""` and `"/"` both mean the root path itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Mapping {
    #[serde(default)]
    pub web_path: String,
    #[serde(default)]
    pub local: String,
    #[serde(default)]
    pub remote: String,
}

impl Mapping {
    pub fn new(local: impl Into<String>, remote: impl Into<String>) -> Self {
        Self {
            local: local.into(),
            web_path: String::new(),
            remote: remote.into(),
        }
    }
}

/// A deployment target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Server {
    #[serde(default)]
    pub options: crate::options::Options,
    pub name: String,
    pub kind: ServerKind,
    /// Base directory on the server. Absolute (`/var/www/app`) or relative to the login
    /// directory (`www`, `~/www`, `.`); for [`ServerKind::Local`] it is relative to the folder.
    #[serde(default)]
    pub root_path: String,
    /// Path mappings; when empty the whole project maps to `root_path`.
    #[serde(default)]
    pub mappings: Vec<Mapping>,
    /// Web server URL corresponding to `root_path` (for "Open in browser").
    #[serde(default)]
    pub web_url: Option<String>,
}

impl Server {
    pub fn new(name: impl Into<String>, kind: ServerKind, root_path: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            options: Default::default(),
            kind,
            root_path: root_path.into(),
            mappings: Vec::new(),
            web_url: None,
        }
    }

    /// A local or mounted folder; the project maps to the folder itself.
    pub fn local(name: impl Into<String>, folder: impl Into<PathBuf>) -> Self {
        Self::new(
            name,
            ServerKind::Local {
                root: folder.into(),
            },
            "/",
        )
    }

    /// SFTP on port 22 without an explicit key file.
    pub fn sftp(
        name: impl Into<String>,
        host: impl Into<String>,
        user: impl Into<String>,
        root_path: impl Into<String>,
    ) -> Self {
        Self::new(
            name,
            ServerKind::Sftp {
                host: host.into(),
                port: 22,
                user: user.into(),
                key_file: None,
            },
            root_path,
        )
    }

    /// rsync over SSH on port 22.
    pub fn rsync(
        name: impl Into<String>,
        host: impl Into<String>,
        user: impl Into<String>,
        root_path: impl Into<String>,
    ) -> Self {
        Self::new(
            name,
            ServerKind::Rsync {
                host: host.into(),
                user: user.into(),
                port: 22,
            },
            root_path,
        )
    }

    /// FTP on port 21. A password is stored in plain text.
    pub fn ftp(
        name: impl Into<String>,
        host: impl Into<String>,
        user: impl Into<String>,
        password: Option<String>,
        root_path: impl Into<String>,
    ) -> Self {
        Self::new(
            name,
            ServerKind::Ftp {
                host: host.into(),
                port: 21,
                user: user.into(),
                password,
                save_password: true,
            },
            root_path,
        )
    }

    pub fn with_mapping(mut self, local: impl Into<String>, remote: impl Into<String>) -> Self {
        self.mappings.push(Mapping::new(local, remote));
        self
    }

    pub fn with_web_url(mut self, url: impl Into<String>) -> Self {
        self.web_url = Some(url.into());
        self
    }

    /// The configured mappings, or the default `"" -> ""` (project root → `root_path`).
    pub fn effective_mappings(&self) -> Vec<Mapping> {
        if self.mappings.is_empty() {
            vec![Mapping::default()]
        } else {
            self.mappings.clone()
        }
    }

    /// `sftp://deploy@example.com:22/var/www`, `file:///mnt/www`, …
    pub fn connection_label(&self) -> String {
        let at = |user: &str| {
            if user.is_empty() {
                String::new()
            } else {
                format!("{user}@")
            }
        };
        let root = if self.root_path.starts_with('/') {
            self.root_path.clone()
        } else {
            format!("/{}", self.root_path)
        };
        match &self.kind {
            ServerKind::Local { root: folder } => format!("file://{}", folder.display()),
            ServerKind::Sftp {
                host, port, user, ..
            } => format!("sftp://{}{host}:{port}{root}", at(user)),
            ServerKind::Rsync { host, port, user } => {
                format!("rsync://{}{host}:{port}{root}", at(user))
            }
            ServerKind::Ftp {
                host, port, user, ..
            } => format!("ftp://{}{host}:{port}{root}", at(user)),
        }
    }

    /// True for an FTP server with a stored password.
    pub fn has_plaintext_password(&self) -> bool {
        matches!(&self.kind, ServerKind::Ftp { password: Some(p), .. } if !p.is_empty())
    }

    /// Problems that prevent (or will likely break) deployment; empty when the server looks usable.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = self.options.validate();
        if self.name.trim().is_empty() {
            problems.push("Server name is empty".to_string());
        }
        if has_control_chars(&self.root_path) {
            problems.push("Root path contains control characters".to_string());
        }
        match &self.kind {
            ServerKind::Local { root } => {
                if root.as_os_str().is_empty() {
                    problems.push("Folder is empty".to_string());
                } else if !root.is_absolute() {
                    problems.push(format!(
                        "Folder must be an absolute path: {}",
                        root.display()
                    ));
                }
            }
            ServerKind::Sftp {
                host,
                port,
                user,
                key_file,
            } => {
                problems.extend(crate::ssh::check_host(host).err());
                problems.extend(crate::ssh::check_user(user).err());
                if *port == 0 {
                    problems.push("Port must be between 1 and 65535".to_string());
                }
                if key_file.as_ref().is_some_and(|k| k.as_os_str().is_empty()) {
                    problems.push("Key file path is empty".to_string());
                }
            }
            ServerKind::Rsync { host, user, port } => {
                problems.extend(crate::ssh::check_host(host).err());
                problems.extend(crate::ssh::check_user(user).err());
                if *port == 0 {
                    problems.push("Port must be between 1 and 65535".to_string());
                }
            }
            ServerKind::Ftp {
                host,
                port,
                user,
                password,
                ..
            } => {
                problems.extend(crate::ftp::check_host(host).err());
                if has_control_chars(user) {
                    problems.push("User name contains control characters".to_string());
                }
                if user.contains(':') {
                    problems.push("User name must not contain ':'".to_string());
                }
                if password.as_deref().is_some_and(has_control_chars) {
                    problems.push("Password contains control characters".to_string());
                }
                if *port == 0 {
                    problems.push("Port must be between 1 and 65535".to_string());
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        for m in &self.mappings {
            let local = m.local.trim().replace('\\', "/");
            let local = local.trim_matches('/');
            if local.split('/').any(|s| s == "..") || m.remote.split('/').any(|s| s == "..") {
                problems.push(format!(
                    "Mapping '{}' -> '{}' must not contain '..'",
                    m.local, m.remote
                ));
            }
            if has_control_chars(&m.local) || has_control_chars(&m.remote) {
                problems.push(format!("Mapping '{}' contains control characters", m.local));
            }
            if !seen.insert(local.to_string()) {
                problems.push(format!("Local path '{}' is mapped more than once", m.local));
            }
        }
        if let Some(url) = &self.web_url {
            let u = url.trim();
            if !u.is_empty() && !(u.starts_with("http://") || u.starts_with("https://")) {
                problems.push(format!(
                    "Web server URL should start with http:// or https://: {u}"
                ));
            }
        }
        problems
    }
}

pub(crate) fn has_control_chars(s: &str) -> bool {
    s.chars().any(char::is_control)
}

/// Writes a freshly created file; with `private` it is created owner-only (mode 0600 on Unix),
/// so a password never sits in a world-readable file, not even briefly.
fn write_new_file(path: &Path, contents: &str, private: bool) -> std::io::Result<()> {
    use std::io::Write as _;
    // A leftover file would keep its old permissions; start from scratch.
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    let mut file = options.open(path)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()
}

/// The project's deployment settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeploymentConfig {
    pub servers: Vec<Server>,
    /// Name of the server used for automatic uploads and default actions.
    pub default_server: Option<String>,
    pub auto_upload: AutoUpload,
    /// Glob patterns (relative to the project / mapping root) never deployed or synced.
    pub excluded: Vec<String>,
}

impl Default for DeploymentConfig {
    fn default() -> Self {
        Self {
            servers: Vec::new(),
            default_server: None,
            auto_upload: AutoUpload::Never,
            excluded: default_excluded(),
        }
    }
}

impl DeploymentConfig {
    /// `<project>/.rustpy/deployment.json`
    pub fn path(project_root: &Path) -> PathBuf {
        project_root.join(".rustpy").join(CONFIG_FILE)
    }

    /// Loads the configuration; a missing file yields the default configuration.
    pub fn load(project_root: &Path) -> anyhow::Result<DeploymentConfig> {
        let path = Self::path(project_root);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DeploymentConfig::default())
            }
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        };
        serde_json::from_str(&text)
            .with_context(|| format!("invalid deployment configuration in {}", path.display()))
    }

    /// Like [`DeploymentConfig::load`] but logs errors and falls back to the default.
    pub fn load_or_default(project_root: &Path) -> DeploymentConfig {
        Self::load(project_root).unwrap_or_else(|e| {
            log::warn!("deployment: {e:#}");
            DeploymentConfig::default()
        })
    }

    /// Writes the configuration (pretty JSON, atomically). On Unix the file is made
    /// owner-readable only when it contains an FTP password.
    pub fn save(&self, project_root: &Path) -> anyhow::Result<()> {
        let path = Self::path(project_root);
        let dir = path.parent().expect("config path has a parent");
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        let mut json = serde_json::to_string_pretty(self)?;
        json.push('\n');
        let private = !self.plaintext_password_servers().is_empty();
        if private {
            log::warn!(
                "deployment: FTP passwords are stored in plain text in {}",
                path.display()
            );
        }
        let tmp = dir.join(format!(".{CONFIG_FILE}.tmp"));
        write_new_file(&tmp, &json, private)
            .with_context(|| format!("cannot write {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("cannot write {}", path.display()))?;
        Ok(())
    }

    pub fn server(&self, name: &str) -> Option<&Server> {
        self.servers.iter().find(|s| s.name == name)
    }

    pub fn server_mut(&mut self, name: &str) -> Option<&mut Server> {
        self.servers.iter_mut().find(|s| s.name == name)
    }

    /// The server named by `default_server` (if it exists).
    pub fn default_server_entry(&self) -> Option<&Server> {
        self.default_server.as_deref().and_then(|n| self.server(n))
    }

    /// Adds `server`, or replaces the server with the same name.
    pub fn upsert_server(&mut self, server: Server) {
        match self.servers.iter_mut().find(|s| s.name == server.name) {
            Some(existing) => *existing = server,
            None => self.servers.push(server),
        }
    }

    /// Removes a server (clearing `default_server` if it pointed to it).
    pub fn remove_server(&mut self, name: &str) -> Option<Server> {
        let idx = self.servers.iter().position(|s| s.name == name)?;
        if self.default_server.as_deref() == Some(name) {
            self.default_server = None;
        }
        Some(self.servers.remove(idx))
    }

    /// Whether `rel_path` (relative to the project, `/`-separated) matches an excluded pattern.
    pub fn is_excluded(&self, rel_path: &str) -> bool {
        ExcludeSet::new(&self.excluded).is_excluded(rel_path)
    }

    /// The server a just-saved `file` should be uploaded to, if any: the default server, when
    /// the auto-upload mode allows it (`explicit_save` = the user pressed Save), the file lies in
    /// the project, is not excluded and has a mapping on that server.
    pub fn auto_upload_target(
        &self,
        project_root: &Path,
        file: &Path,
        explicit_save: bool,
    ) -> Option<&Server> {
        match self.auto_upload {
            AutoUpload::Never => return None,
            AutoUpload::OnExplicitSave if !explicit_save => return None,
            _ => {}
        }
        let server = self.default_server_entry()?;
        let abs = normalize_local(&project_root.join(file));
        let rel = rel_string(&normalize_local(project_root), &abs)?;
        if rel.is_empty() || self.is_excluded(&rel) {
            return None;
        }
        if ExcludeSet::new(&server.options.excluded).is_excluded(&rel) {
            return None;
        }
        remote_path(server, project_root, &abs)?;
        Some(server)
    }

    /// Names of servers that store a plain-text password.
    pub fn plaintext_password_servers(&self) -> Vec<&str> {
        self.servers
            .iter()
            .filter(|s| s.has_plaintext_password())
            .map(|s| s.name.as_str())
            .collect()
    }

    /// Problems of the whole configuration (each prefixed with the server name).
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let mut names = std::collections::HashSet::new();
        for s in &self.servers {
            if !names.insert(s.name.as_str()) {
                problems.push(format!("Duplicate server name '{}'", s.name));
            }
            problems.extend(s.validate().into_iter().map(|p| format!("{}: {p}", s.name)));
        }
        if let Some(d) = &self.default_server {
            if self.server(d).is_none() {
                problems.push(format!("Default server '{d}' does not exist"));
            }
        }
        problems
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::TempDir;

    fn sample() -> DeploymentConfig {
        DeploymentConfig {
            servers: vec![
                Server::local("mount", "/mnt/www"),
                Server::new(
                    "prod",
                    ServerKind::Sftp {
                        host: "example.com".into(),
                        port: 2222,
                        user: "deploy".into(),
                        key_file: Some("/home/me/.ssh/id_ed25519".into()),
                    },
                    "/var/www/app",
                )
                .with_mapping("", "")
                .with_mapping("static", "/assets")
                .with_web_url("https://example.com"),
                Server::rsync("mirror", "10.0.0.5", "me", "~/site"),
                Server::ftp(
                    "legacy",
                    "ftp.example.com",
                    "bob",
                    Some("s3cr\"et".into()),
                    "htdocs",
                ),
            ],
            default_server: Some("prod".into()),
            auto_upload: AutoUpload::OnExplicitSave,
            excluded: vec![".git".into(), "*.log".into()],
        }
    }

    #[test]
    fn json_round_trip() {
        let cfg = sample();
        let json = serde_json::to_string_pretty(&cfg).unwrap();
        assert!(json.contains("\"type\": \"sftp\""), "{json}");
        assert!(
            json.contains("\"auto_upload\": \"on_explicit_save\""),
            "{json}"
        );
        let back: DeploymentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn missing_fields_take_defaults() {
        let cfg: DeploymentConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg, DeploymentConfig::default());
        assert_eq!(cfg.excluded, default_excluded());
        assert_eq!(cfg.auto_upload, AutoUpload::Never);

        let cfg: DeploymentConfig = serde_json::from_str(r#"{"servers":[{"name":"s","kind":{"type":"sftp","host":"h"}},{"name":"f","kind":{"type":"ftp","host":"h"}}],"excluded":[]}"#).unwrap();
        assert_eq!(
            cfg.servers[0].kind,
            ServerKind::Sftp {
                host: "h".into(),
                port: 22,
                user: String::new(),
                key_file: None
            }
        );
        assert_eq!(
            cfg.servers[1].kind,
            ServerKind::Ftp {
                host: "h".into(),
                port: 21,
                user: String::new(),
                password: None,
                save_password: true
            }
        );
        // "Save password" is written only when it is off, so older files stay as they are.
        let json = serde_json::to_string(&cfg.servers[1]).unwrap();
        assert!(!json.contains("save_password"), "{json}");
        let mut off = cfg.servers[1].clone();
        if let ServerKind::Ftp { save_password, .. } = &mut off.kind {
            *save_password = false;
        }
        let json = serde_json::to_string(&off).unwrap();
        assert!(json.contains("\"save_password\":false"), "{json}");
        assert_eq!(serde_json::from_str::<Server>(&json).unwrap(), off);
        assert_eq!(cfg.servers[0].root_path, "");
        assert!(cfg.servers[0].mappings.is_empty());
        assert!(
            cfg.excluded.is_empty(),
            "an explicitly empty list stays empty"
        );
    }

    #[test]
    fn save_and_load() {
        let dir = TempDir::new("config");
        assert_eq!(
            DeploymentConfig::load(dir.path()).unwrap(),
            DeploymentConfig::default()
        );
        let cfg = sample();
        cfg.save(dir.path()).unwrap();
        let path = DeploymentConfig::path(dir.path());
        assert!(path.ends_with(".rustpy/deployment.json"));
        assert_eq!(DeploymentConfig::load(dir.path()).unwrap(), cfg);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "file with a password is private");
        }
        // Saving again replaces the file and leaves no temporary file behind.
        let mut changed = cfg.clone();
        changed.auto_upload = AutoUpload::Always;
        changed.servers.retain(|s| !s.has_plaintext_password());
        changed.save(dir.path()).unwrap();
        assert_eq!(DeploymentConfig::load(dir.path()).unwrap(), changed);
        assert!(!dir.path().join(".rustpy/.deployment.json.tmp").exists());
        std::fs::write(&path, "{ not json").unwrap();
        let err = DeploymentConfig::load(dir.path()).unwrap_err();
        assert!(
            format!("{err:#}").contains("invalid deployment configuration"),
            "{err:#}"
        );
        assert_eq!(
            DeploymentConfig::load_or_default(dir.path()),
            DeploymentConfig::default()
        );
    }

    #[test]
    fn password_is_never_logged() {
        let cfg = sample();
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("s3cr"), "{dbg}");
        assert!(dbg.contains("<redacted>"));
        assert_eq!(cfg.plaintext_password_servers(), vec!["legacy"]);
        assert!(!PLAINTEXT_PASSWORD_WARNING.is_empty());
    }

    #[test]
    fn server_management() {
        let mut cfg = sample();
        assert_eq!(
            cfg.default_server_entry().map(|s| s.name.as_str()),
            Some("prod")
        );
        let mut changed = cfg.server("mount").unwrap().clone();
        changed.root_path = "/sub".into();
        cfg.upsert_server(changed);
        assert_eq!(cfg.server("mount").unwrap().root_path, "/sub");
        assert_eq!(cfg.servers.len(), 4);
        cfg.upsert_server(Server::local("new", "/tmp/x"));
        assert_eq!(cfg.servers.len(), 5);
        cfg.server_mut("new").unwrap().web_url = Some("http://localhost".into());
        assert!(cfg.remove_server("prod").is_some());
        assert_eq!(cfg.default_server, None);
        assert!(cfg.remove_server("prod").is_none());
    }

    #[test]
    #[cfg(unix)] // sample servers use names that are invalid on Windows
    fn validation() {
        assert!(sample().validate().is_empty(), "{:?}", sample().validate());
        let mut cfg = sample();
        cfg.default_server = Some("nope".into());
        cfg.servers
            .push(Server::sftp("prod", "-oProxyCommand=evil", "us er", "/x"));
        cfg.servers.push(
            Server::local("rel", "relative/dir")
                .with_mapping("a", "b")
                .with_mapping("a/", "c")
                .with_mapping("../up", "d"),
        );
        cfg.servers.push(
            Server::ftp("f", "ftp.example.com/x", "u", Some("a\nb".into()), "/")
                .with_web_url("example.com"),
        );
        let problems = cfg.validate();
        let text = problems.join("\n");
        for needle in [
            "Duplicate server name 'prod'",
            "Default server 'nope'",
            "must not start with '-'",
            "User name",
            "absolute path",
            "mapped more than once",
            "'..'",
            "Password contains control",
            "Web server URL",
            "Host name",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        }
    }

    #[test]
    fn labels() {
        let cfg = sample();
        assert_eq!(
            cfg.server("prod").unwrap().connection_label(),
            "sftp://deploy@example.com:2222/var/www/app"
        );
        assert_eq!(
            cfg.server("legacy").unwrap().connection_label(),
            "ftp://bob@ftp.example.com:21/htdocs"
        );
        assert_eq!(
            cfg.server("mount").unwrap().connection_label(),
            "file:///mnt/www"
        );
        assert_eq!(
            cfg.server("mirror").unwrap().kind.type_label(),
            "rsync over SSH"
        );
        assert_eq!(cfg.server("legacy").unwrap().kind.default_port(), Some(21));
        assert_eq!(
            AutoUpload::ALL.map(AutoUpload::label),
            ["Never", "On explicit save action", "Always"]
        );
    }

    #[test]
    fn auto_upload_target() {
        let root = Path::new("/home/me/proj");
        let mut cfg = DeploymentConfig {
            servers: vec![Server::sftp("prod", "h", "u", "/srv").with_mapping("src", "")],
            default_server: Some("prod".into()),
            ..Default::default()
        };
        let file = root.join("src/app.py");
        assert!(
            cfg.auto_upload_target(root, &file, true).is_none(),
            "mode Never"
        );
        cfg.auto_upload = AutoUpload::OnExplicitSave;
        assert_eq!(
            cfg.auto_upload_target(root, &file, true)
                .map(|s| s.name.as_str()),
            Some("prod")
        );
        assert!(
            cfg.auto_upload_target(root, &file, false).is_none(),
            "auto-save is not explicit"
        );
        cfg.auto_upload = AutoUpload::Always;
        assert!(cfg.auto_upload_target(root, &file, false).is_some());
        assert!(
            cfg.auto_upload_target(root, Path::new("src/app.py"), false)
                .is_some(),
            "relative paths are project-relative"
        );
        assert!(
            cfg.auto_upload_target(root, &root.join("src/__pycache__/app.pyc"), true)
                .is_none(),
            "excluded"
        );
        assert!(
            cfg.auto_upload_target(root, &root.join("tests/test_app.py"), true)
                .is_none(),
            "not mapped"
        );
        assert!(
            cfg.auto_upload_target(root, Path::new("/elsewhere/x.py"), true)
                .is_none(),
            "outside the project"
        );
        cfg.default_server = None;
        assert!(
            cfg.auto_upload_target(root, &file, true).is_none(),
            "no default server"
        );
    }
}
