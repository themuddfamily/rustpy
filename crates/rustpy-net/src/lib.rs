//! RustPy IDE — networking shared by every feature that talks HTTP(S)
//! (PyPI/OSV lookups, AI Assistant, HTTP client, issue trackers):
//! Appearance & Behavior › System Settings › HTTP Proxy and Tools › Server
//! Certificates.
//!
//! The UI calls [`configure`] at startup and whenever the settings change;
//! other crates build their `ureq` agents with [`agent_builder`], which applies:
//!
//! * the proxy for the request URL ([`proxy`]): none, the environment
//!   (`HTTPS_PROXY`, `NO_PROXY`, …), or the manual HTTP / SOCKS proxy with its
//!   "No proxy for" list and credentials. SOCKS goes through [`socks`].
//! * certificate trust ([`certs`], [`verifier`]): the bundled Mozilla roots, the
//!   OS CA bundle (optional), certificates the user accepted, and — when a
//!   certificate cannot be verified — a prompt answered in the UI (see
//!   [`pending_prompts`] / [`answer_prompt`]) or automatic acceptance.
//!
//! [`check_connection`] backs the settings page's "Check connection" button.

pub mod certs;
pub mod http_bridge;
mod own_sockets;
pub mod pac;
pub mod proxy;
pub mod socks;
mod system_store;
pub mod verifier;
pub mod x509;

#[cfg(test)]
mod test_certs;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use ureq::rustls;

pub use certs::{AcceptedCertificate, CertStore};
pub use proxy::{ProxyChoice, ProxyServer, SocksVersion};
pub use rustpy_settings::{CertificateSettings, ProxyKind, ProxyMode, ProxySettings};
pub use verifier::{
    answer_prompt, answer_prompt_with, clear_prompt_handler, pending_prompts, set_prompt_handler,
    CertificatePrompt, TrustDecision,
};
pub use x509::CertificateInfo;

/// User agent for requests that have no better one.
pub const USER_AGENT: &str = concat!("rustpy-ide/", env!("CARGO_PKG_VERSION"));

/// Proxy and certificate settings for connections.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetConfig {
    /// The proxy password is the session value (`ProxySettings::password`).
    pub proxy: ProxySettings,
    pub certificates: CertificateSettings,
    /// Folder of accepted certificates (`<config dir>/certs`); `None` keeps
    /// certificates accepted in this process in memory only.
    pub certs_dir: Option<PathBuf>,
}

impl NetConfig {
    pub fn cert_store(&self) -> CertStore {
        CertStore::new(self.certs_dir.clone())
    }
}

static CONFIG: RwLock<Option<Arc<NetConfig>>> = RwLock::new(None);
/// Bumped whenever trust inputs change (configuration, accepted certificates).
static GENERATION: AtomicU64 = AtomicU64::new(1);

pub(crate) fn bump_generation() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

/// Sets the configuration used by [`agent_builder`].
pub fn configure(cfg: NetConfig) {
    certs::refresh_accepted(&cfg.cert_store());
    *CONFIG.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(cfg));
    bump_generation();
}

/// The current configuration (environment proxies and default trust until
/// [`configure`] is called).
pub fn config() -> Arc<NetConfig> {
    CONFIG
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

/// The proxy a request to `url` would use under the current configuration.
pub fn proxy_for(url: &str) -> ProxyChoice {
    proxy::choose(&config().proxy, url, &proxy::process_env)
}

/// Environment variables that pass the manual proxy on to child processes
/// (pip, uv); empty unless a manual proxy is configured.
pub fn proxy_env_vars() -> Vec<(String, String)> {
    proxy::proxy_env_vars(&config().proxy)
}

#[derive(Clone, PartialEq, Eq)]
struct TlsKey {
    generation: u64,
    use_system_store: bool,
    accept_automatically: bool,
    certs_dir: Option<PathBuf>,
}

/// The rustls client configuration for `cfg` (cached until trust inputs change).
pub fn tls_config(cfg: &NetConfig) -> Arc<rustls::ClientConfig> {
    static CACHE: Mutex<Option<(TlsKey, Arc<rustls::ClientConfig>)>> = Mutex::new(None);
    let key = TlsKey {
        generation: GENERATION.load(Ordering::SeqCst),
        use_system_store: cfg.certificates.use_system_store,
        accept_automatically: cfg.certificates.accept_automatically,
        certs_dir: cfg.certs_dir.clone(),
    };
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((k, c)) = cache.as_ref() {
        if *k == key {
            return c.clone();
        }
    }
    let built = build_tls_config(cfg);
    *cache = Some((key, built.clone()));
    built
}

fn build_tls_config(cfg: &NetConfig) -> Arc<rustls::ClientConfig> {
    tls_config_with_roots(cfg, &[]).expect("the bundled TLS configuration is valid")
}

/// Source-private roots use the same shared host-pinned verifier without
/// mutating global trust or the cached ordinary HTTP client configuration.
pub fn tls_config_with_roots(
    cfg: &NetConfig,
    extra_roots: &[Vec<u8>],
) -> Result<Arc<rustls::ClientConfig>, rustls::Error> {
    let started = Instant::now();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let mut extra = 0usize;
    for root in extra_roots {
        roots.add(rustls::pki_types::CertificateDer::from(root.clone()))?;
        extra += 1;
    }
    if cfg.certificates.use_system_store {
        let (added, _) = roots.add_parsable_certificates(
            certs::system_certificates()
                .iter()
                .map(|d| rustls::pki_types::CertificateDer::from(d.as_slice())),
        );
        extra += added;
    }
    // Imported certificates are trust anchors, so an imported (corporate) CA
    // covers every server certificate it issues. Certificates accepted from a
    // prompt are not: they are pinned to their host (see `certs`).
    let store = cfg.cert_store();
    let (added, _) = roots.add_parsable_certificates(
        store
            .anchors()
            .into_iter()
            .map(rustls::pki_types::CertificateDer::from),
    );
    extra += added;
    let inner = rustls::client::WebPkiServerVerifier::builder_with_provider(
        Arc::new(roots),
        provider.clone(),
    )
    .build()
    .map_err(|error| rustls::Error::General(error.to_string()))?;
    let verifier = verifier::ConfirmingVerifier {
        inner,
        store,
        accept_automatically: cfg.certificates.accept_automatically,
    };
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS12, &rustls::version::TLS13])
        .map_err(|error| rustls::Error::General(error.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    log::debug!(
        "TLS configuration with {extra} system/accepted certificates built in {:?}",
        started.elapsed()
    );
    Ok(Arc::new(config))
}

/// A `ureq` agent builder for requests to `url` with the configured proxy and
/// certificate trust; callers add timeouts, user agent and redirects.
pub fn agent_builder(url: &str) -> ureq::AgentBuilder {
    agent_builder_with(&config(), url)
}

/// [`agent_builder`] for an explicit configuration (e.g. unsaved settings).
pub fn agent_builder_with(cfg: &NetConfig, url: &str) -> ureq::AgentBuilder {
    let builder = ureq::AgentBuilder::new().tls_config(tls_config(cfg));
    if cfg.proxy.mode == ProxyMode::Pac {
        return match pac::choose_with(cfg, url) {
            ProxyChoice::Unavailable(error) => builder.resolver(pac::UnavailableResolver(error)),
            _ => builder.resolver(pac::PacResolver {
                config: Arc::new(cfg.clone()),
            }),
        };
    }
    let route = proxy::choose(&cfg.proxy, url, &proxy::process_env);
    match route {
        ProxyChoice::Direct => builder,
        ProxyChoice::Pac(_) => builder.resolver(pac::PacResolver {
            config: Arc::new(cfg.clone()),
        }),
        ProxyChoice::Unavailable(error) => builder.resolver(pac::UnavailableResolver(error)),
        ProxyChoice::Http(server) => {
            // ureq sends proxy credentials only with CONNECT: plain-HTTP requests
            // through an authenticating proxy go through the bridge.
            let plain_http = proxy::url_target(url)
                .is_some_and(|(scheme, _, _)| scheme == "http" || scheme == "ws");
            if plain_http && server.user.as_deref().is_some_and(|u| !u.is_empty()) {
                return builder.resolver(http_bridge::HttpProxyResolver::new(server));
            }
            match server.to_ureq() {
                Ok(p) => builder.proxy(p),
                // A proxy ureq cannot express (IPv6 address): the bridge drives it.
                Err(e) => {
                    log::debug!("HTTP proxy {} through the bridge: {e}", server.address());
                    builder.resolver(http_bridge::HttpProxyResolver::new(server))
                }
            }
        }
        ProxyChoice::Socks(server, version) => {
            builder.resolver(socks::SocksResolver::new(server, version))
        }
    }
}

/// A user-facing description of a transport error (proxy and certificate
/// problems get explicit wording).
pub fn describe_transport_error(t: &ureq::Transport) -> String {
    describe_transport_error_with(&config(), t)
}

fn describe_transport_error_with(cfg: &NetConfig, t: &ureq::Transport) -> String {
    let text = t.to_string();
    match t.kind() {
        ureq::ErrorKind::ProxyUnauthorized => proxy_auth_problem(cfg, t.url().map(|u| u.as_str())),
        ureq::ErrorKind::ProxyConnect => "The proxy refused to open a tunnel (CONNECT failed)".into(),
        ureq::ErrorKind::InvalidProxyUrl => "Invalid proxy settings".into(),
        _ if text.contains("invalid peer certificate") => format!("The server's certificate is not trusted ({text}). Accept it when asked, or add it under Settings › Tools › Server Certificates."),
        // No credentials were offered because the password is unknown (see `ProxySettings::credentials`).
        _ if manual_password_missing(cfg) && cfg.proxy.kind == ProxyKind::Socks && (text.contains(socks::AUTH_REQUIRED) || text.contains(socks::NO_ACCEPTABLE_METHOD)) => format!("The SOCKS proxy requires authentication, but {NO_PASSWORD}"),
        _ => text,
    }
}

/// Why no credentials went to a proxy that asks for them.
const NO_PASSWORD: &str = "no proxy password is known in this session: enter it under Settings › Appearance & Behavior › System Settings › HTTP Proxy, or unlock the password database";

/// The manual proxy authenticates, but its password is not known in this session.
fn manual_password_missing(cfg: &NetConfig) -> bool {
    matches!(cfg.proxy.mode, ProxyMode::Manual | ProxyMode::Pac) && cfg.proxy.password_missing()
}

/// Why the proxy answered 407 for a request to `url`.
fn proxy_auth_problem(cfg: &NetConfig, url: Option<&str>) -> String {
    if manual_password_missing(cfg) {
        // No credentials were sent (see `ProxySettings::credentials`).
        return format!("The proxy requires authentication (HTTP 407), but {NO_PASSWORD}");
    }
    let sent = (cfg.proxy.mode == ProxyMode::Pac && cfg.proxy.credentials().is_some())
        || (cfg.proxy.mode != ProxyMode::Pac
            && url.is_some_and(|u| {
                matches!(
                    proxy::choose(&cfg.proxy, u, &proxy::process_env),
                    ProxyChoice::Http(ProxyServer { user: Some(_), .. })
                )
            }));
    if sent {
        "The proxy rejected the credentials (HTTP 407 Proxy Authentication Required)".into()
    } else {
        "The proxy requires authentication (HTTP 407 Proxy Authentication Required)".into()
    }
}

/// Requests `url` with `cfg` and describes the outcome; `Ok` when the server
/// answered (any HTTP status but 407).
pub fn check_connection(cfg: &NetConfig, url: &str) -> Result<String, String> {
    let url = url.trim();
    let url = if url.contains("://") {
        url.to_string()
    } else {
        format!("https://{url}")
    };
    if proxy::url_target(&url).is_none() {
        return Err(format!("Invalid URL: {url}"));
    }
    let route = if cfg.proxy.mode == ProxyMode::Pac {
        pac::choose_with(cfg, &url)
    } else {
        proxy::choose(&cfg.proxy, &url, &proxy::process_env)
    }
    .describe();
    let agent = agent_builder_with(cfg, &url)
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirects(0)
        .user_agent(USER_AGENT)
        .build();
    let started = Instant::now();
    let ms = || started.elapsed().as_millis();
    match agent.get(&url).call() {
        Ok(resp) => Ok(format!(
            "Connection successful via {route}: HTTP {} {} in {} ms",
            resp.status(),
            resp.status_text(),
            ms()
        )),
        Err(ureq::Error::Status(407, _)) => Err(format!(
            "{} — via {route}",
            proxy_auth_problem(cfg, Some(&url))
        )),
        Err(ureq::Error::Status(code, resp)) => Ok(format!(
            "Connection successful via {route}: the server answered HTTP {code} {} in {} ms",
            resp.status_text(),
            ms()
        )),
        Err(ureq::Error::Transport(t)) => Err(format!(
            "{} — via {route}",
            describe_transport_error_with(cfg, &t)
        )),
    }
}

/// Serializes tests that touch the process-wide configuration.
#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
