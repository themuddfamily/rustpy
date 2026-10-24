//! Appearance & Behavior › System Settings › HTTP Proxy and Passwords, and
//! Tools › Server Certificates.
//!
//! Only preferences live here. Secrets (the proxy password) are fields marked
//! `#[serde(skip)]`: they are held in memory for the session and persisted in
//! the password safe (`rustpy-secrets`), never in `settings.json`. The logic
//! that uses these settings is in `rustpy-net` and `rustpy-secrets`.

use serde::{Deserialize, Serialize};

/// How network connections find their proxy.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyMode {
    /// Always connect directly.
    NoProxy,
    /// Use the proxy from the environment (`HTTPS_PROXY`, `HTTP_PROXY`,
    /// `ALL_PROXY`, `NO_PROXY`, either case), as command-line tools do.
    #[default]
    AutoDetect,
    /// Use the proxy configured in [`ProxySettings`].
    Manual,
    /// Evaluate a configured Proxy Auto-Configuration JavaScript file.
    Pac,
}

/// Protocol of a manually configured proxy.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyKind {
    /// HTTP proxy (`CONNECT` tunnels for HTTPS).
    #[default]
    Http,
    /// SOCKS5 proxy (host names are resolved by the proxy).
    Socks,
}

impl ProxyKind {
    /// Port used when the configured port is 0.
    pub fn default_port(self) -> u16 {
        match self {
            ProxyKind::Http => 8080,
            ProxyKind::Socks => 1080,
        }
    }
}

/// Appearance & Behavior › System Settings › HTTP Proxy.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxySettings {
    pub mode: ProxyMode,
    pub kind: ProxyKind,
    pub host: String,
    /// 0 = the kind's default port.
    pub port: u16,
    /// "No proxy for": comma-separated host names, domain suffixes (`.corp`),
    /// `*` wildcards (`192.168.*`) and CIDR ranges (`10.0.0.0/8`).
    pub no_proxy: String,
    /// Proxy authentication (Basic for HTTP, username/password for SOCKS5).
    pub authenticate: bool,
    pub login: String,
    /// Password for this session only; persisted in the password safe when
    /// `remember_password` is set, never in settings.json.
    #[serde(skip)]
    pub password: String,
    pub remember_password: bool,
    /// URL last used by "Check connection".
    pub check_url: String,
    pub pac_url: String,
    pub pac_refresh_seconds: u64,
}

impl std::fmt::Debug for ProxySettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProxySettings")
            .field("mode", &self.mode)
            .field("kind", &self.kind)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("authenticate", &self.authenticate)
            .field(
                "password",
                &if self.password.is_empty() {
                    "<empty>"
                } else {
                    "<redacted>"
                },
            )
            .finish_non_exhaustive()
    }
}

impl Default for ProxySettings {
    fn default() -> Self {
        Self {
            mode: ProxyMode::AutoDetect,
            kind: ProxyKind::Http,
            host: String::new(),
            port: 0,
            no_proxy: String::new(),
            authenticate: false,
            login: String::new(),
            password: String::new(),
            remember_password: true,
            check_url: String::new(),
            pac_url: String::new(),
            pac_refresh_seconds: 300,
        }
    }
}

impl ProxySettings {
    /// The configured port, or the kind's default when it is 0.
    pub fn effective_port(&self) -> u16 {
        if self.port == 0 {
            self.kind.default_port()
        } else {
            self.port
        }
    }

    /// The login and password to send to the proxy: only with authentication on,
    /// a login and a password. An empty password is one that is not known in
    /// this session (not read from the password safe yet, the database is
    /// locked, or it was not remembered): sending `login:` would count as a
    /// failed login for every request, which can lock a directory account.
    pub fn credentials(&self) -> Option<(&str, &str)> {
        let login = self.login.trim();
        (self.authenticate && !login.is_empty() && !self.password.is_empty())
            .then_some((login, self.password.as_str()))
    }

    /// Authentication is configured but no password is known: requests go to
    /// the proxy without credentials (see [`ProxySettings::credentials`]).
    pub fn password_missing(&self) -> bool {
        self.authenticate && !self.login.trim().is_empty() && self.password.is_empty()
    }
}

/// Tools › Server Certificates. Accepted certificates themselves are PEM files in
/// `<config dir>/certs`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CertificateSettings {
    /// Accept (and remember) any certificate that fails verification, without asking.
    pub accept_automatically: bool,
    /// Also trust the operating system's CA bundle (Linux/BSD/macOS PEM files,
    /// `SSL_CERT_FILE`), on top of the bundled Mozilla roots.
    pub use_system_store: bool,
}

impl Default for CertificateSettings {
    fn default() -> Self {
        Self {
            accept_automatically: false,
            use_system_store: true,
        }
    }
}

/// Where the password safe keeps secrets.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PasswordStorage {
    /// The OS keyring (Secret Service on Linux, Windows Credential Manager,
    /// macOS Keychain); the encrypted file when the keyring is unavailable.
    #[default]
    Native,
    /// An AES-256-GCM encrypted file (key from a master password or a random
    /// per-user key file).
    EncryptedFile,
    /// Kept in memory only: forgotten when the IDE exits.
    Memory,
}

/// Appearance & Behavior › System Settings › Passwords.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PasswordSettings {
    pub storage: PasswordStorage,
    /// Encrypted database file; empty = `<config dir>/passwords.db`.
    pub database: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_password_is_never_serialized() {
        let p = ProxySettings {
            mode: ProxyMode::Manual,
            host: "proxy.corp".into(),
            port: 3128,
            authenticate: true,
            login: "me".into(),
            password: "s3cret".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("s3cret"), "{json}");
        let back: ProxySettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.password, "");
        assert_eq!(back.login, "me");
        assert_eq!(back.mode, ProxyMode::Manual);
        // A password in the file (hand-edited) is ignored too.
        let back: ProxySettings =
            serde_json::from_str(r#"{"password": "x", "mode": "no_proxy"}"#).unwrap();
        assert_eq!(back.password, "");
        assert_eq!(back.mode, ProxyMode::NoProxy);
    }

    #[test]
    fn credentials_need_a_password() {
        let mut p = ProxySettings {
            mode: ProxyMode::Manual,
            host: "proxy.corp".into(),
            authenticate: true,
            login: " CORP\\alice ".into(),
            ..Default::default()
        };
        // The password is not loaded (or not known): no credentials at all.
        assert_eq!(p.credentials(), None);
        assert!(p.password_missing());
        p.password = "pw".into();
        assert_eq!(p.credentials(), Some(("CORP\\alice", "pw")));
        assert!(!p.password_missing());
        p.authenticate = false;
        assert_eq!(p.credentials(), None);
        assert!(!p.password_missing(), "authentication is off");
        let no_login = ProxySettings {
            authenticate: true,
            login: "  ".into(),
            ..Default::default()
        };
        assert!(no_login.credentials().is_none() && !no_login.password_missing());
    }

    #[test]
    fn defaults_and_ports() {
        let p = ProxySettings::default();
        assert_eq!(
            p.mode,
            ProxyMode::AutoDetect,
            "environment proxies keep working out of the box"
        );
        assert!(p.remember_password);
        assert_eq!(p.effective_port(), 8080);
        let s = ProxySettings {
            kind: ProxyKind::Socks,
            ..Default::default()
        };
        assert_eq!(s.effective_port(), 1080);
        let c = CertificateSettings::default();
        assert!(c.use_system_store && !c.accept_automatically);
        let empty: PasswordSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.storage, PasswordStorage::Native);
        let mem: PasswordSettings = serde_json::from_str(r#"{"storage": "memory"}"#).unwrap();
        assert_eq!(mem.storage, PasswordStorage::Memory);
    }
}
