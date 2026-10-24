//! The Marketplace: a JSON catalog of downloadable plugins, update detection
//! and SHA-256-verified download + install.
//!
//! ```json
//! {"plugins": [{"id": "com.example.solarized-theme", "name": "Solarized Theme", "version": "1.0.0",
//!               "description": "…", "vendor": "…", "download_url": "https://…/x.rpplugin",
//!               "sha256": "<64 hex digits>", "homepage": "https://…", "tags": ["theme"]}]}
//! ```
//!
//! `download_url` points to a plugin bundle (see [`pack_bundle`](crate::pack_bundle)).
//!
//! The catalog bundled with the IDE ([`Catalog::bundled`]) lists the example
//! plugins under `examples/`; their bundles ship with the IDE too
//! ([`shipped_bundle`]), so that catalog installs without a network.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::bundle::MAX_BUNDLE_BYTES;
use crate::error::PluginError;
use crate::manifest::{check_http_url, is_compatible, validate_plugin_id};
use crate::registry::{InstallOutcome, PluginRegistry};
use crate::sha256::sha256_hex;
use crate::version::Version;
use crate::IDE_VERSION;

/// Timeout for fetching a catalog (the whole request) and for connecting to
/// the server of a download.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Overall limit for downloading a plugin bundle, from connecting to the last
/// byte. ureq 2 has no per-read timeout once an overall one is set (each read
/// may wait for the rest of the overall time), so a server that sends slowly
/// or stalls holds a download for at most this long.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_CATALOG_BYTES: u64 = 8 * 1024 * 1024;
const USER_AGENT: &str = concat!("rustpy-ide/", env!("CARGO_PKG_VERSION"), " (plugins)");

/// The catalog bundled with the IDE (offline Marketplace).
const BUNDLED_CATALOG: &str = include_str!("../assets/catalog.json");

/// The bundles of the bundled catalog's plugins (packed from `examples/`;
/// `tests/catalog.rs` checks they match the examples and the catalog).
const SHIPPED_BUNDLES: &[&[u8]] = &[
    include_bytes!("../assets/bundles/com.example.solarized-theme-1.0.0.rpplugin"),
    include_bytes!("../assets/bundles/com.example.emacs-keymap-0.3.1.rpplugin"),
    include_bytes!("../assets/bundles/com.example.pytest-kit-2.1.0.rpplugin"),
    include_bytes!("../assets/bundles/org.rustpy.csv-table-1.0.0.rpplugin"),
];

/// One downloadable plugin version.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub vendor: String,
    /// The plugin bundle (`http(s)://…`).
    pub download_url: String,
    /// SHA-256 of the bundle, 64 hex digits (normalized to lowercase when parsed).
    pub sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    pub tags: Vec<String>,
    pub screenshots: Vec<String>,
    /// File types for advisory Marketplace suggestions; never auto-installs.
    pub file_extensions: Vec<String>,
    /// Oldest IDE version this plugin version works with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_ide_version: Option<String>,
}

impl CatalogEntry {
    /// Validation problems (empty when the entry is usable).
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Err(e) = validate_plugin_id(&self.id) {
            out.push(format!("id: {e}"));
        }
        if self.name.trim().is_empty() {
            out.push("name: is required".into());
        }
        if let Err(e) = Version::parse(&self.version) {
            out.push(format!("version: {e}"));
        }
        if let Err(e) = check_http_url(&self.download_url) {
            out.push(format!("download_url: {e}"));
        }
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            out.push("sha256: must be 64 hexadecimal digits".into());
        }
        if self.file_extensions.len() > 64
            || self
                .file_extensions
                .iter()
                .any(|extension| crate::manifest::normalize_extension(extension).is_err())
        {
            out.push("Invalid file type suggestion metadata".into());
        }
        if self.screenshots.len() > 8 {
            out.push("At most 8 screenshots are supported".into());
        }
        for screenshot in &self.screenshots {
            if !screenshot.starts_with("https://") || check_http_url(screenshot).is_err() {
                out.push("Screenshots must use HTTPS URLs".into());
            }
        }
        if let Some(url) = &self.homepage {
            if let Err(e) = check_http_url(url) {
                out.push(format!("homepage: {e}"));
            }
        }
        if let Some(v) = &self.min_ide_version {
            if let Err(e) = Version::parse(v) {
                out.push(format!("min_ide_version: {e}"));
            }
        }
        out
    }

    /// Whether this IDE satisfies the entry's `min_ide_version`.
    pub fn is_compatible(&self) -> bool {
        is_compatible(self.min_ide_version.as_deref(), IDE_VERSION)
    }

    fn matches(&self, needle: &str) -> bool {
        [&self.id, &self.name, &self.description, &self.vendor]
            .into_iter()
            .chain(&self.tags)
            .any(|field| field.to_lowercase().contains(needle))
    }
}

/// An installed plugin with a newer compatible version in the catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginUpdate {
    pub id: String,
    pub installed_version: String,
    /// The newest compatible catalog entry (`entry.version` is the available version).
    pub entry: CatalogEntry,
}

/// A Marketplace catalog.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Catalog {
    /// Valid entries, in catalog order. One id may appear with several versions.
    pub plugins: Vec<CatalogEntry>,
    /// Entries left out while parsing, with the reason.
    #[serde(skip)]
    pub problems: Vec<String>,
}

impl Catalog {
    /// Parses catalog JSON. Malformed JSON is an error; individual invalid or
    /// duplicate entries are left out and described in [`problems`](Self::problems).
    pub fn parse(json: &str) -> Result<Catalog, PluginError> {
        let json_err = |source| PluginError::Json {
            what: "plugin catalog".into(),
            source,
        };
        let Value::Object(mut root) = serde_json::from_str(json).map_err(json_err)? else {
            return Err(PluginError::Catalog(
                "a catalog is a JSON object with a `plugins` array".into(),
            ));
        };
        let entries = match root.remove("plugins") {
            Some(Value::Array(entries)) => entries,
            None | Some(Value::Null) => Vec::new(),
            Some(_) => return Err(PluginError::Catalog("`plugins` must be an array".into())),
        };
        let mut catalog = Catalog::default();
        // (id, version) pairs taken so far; `1.0` and `1.0.0` are the same version.
        let mut seen: Vec<(String, Version)> = Vec::new();
        for (i, value) in entries.into_iter().enumerate() {
            let mut entry: CatalogEntry = match serde_json::from_value(value) {
                Ok(entry) => entry,
                Err(e) => {
                    catalog.problems.push(format!("plugins[{i}]: {e}"));
                    continue;
                }
            };
            entry.sha256 = entry.sha256.trim().to_ascii_lowercase();
            let problems = entry.problems();
            if !problems.is_empty() {
                let label = if entry.id.is_empty() {
                    String::new()
                } else {
                    format!(" (`{}`)", entry.id)
                };
                catalog
                    .problems
                    .push(format!("plugins[{i}]{label}: {}", problems.join("; ")));
                continue;
            }
            let Some(version) = version_of(&entry) else {
                continue;
            };
            if seen.iter().any(|(id, v)| *id == entry.id && *v == version) {
                catalog.problems.push(format!(
                    "plugins[{i}]: `{}` {} is listed more than once",
                    entry.id, entry.version
                ));
                continue;
            }
            seen.push((entry.id.clone(), version));
            catalog.plugins.push(entry);
        }
        Ok(catalog)
    }

    /// The catalog shipped with the IDE (`assets/catalog.json`).
    pub fn bundled() -> Catalog {
        Catalog::parse(BUNDLED_CATALOG).unwrap_or_default()
    }

    /// Downloads and parses a catalog (10 s timeout). Run it off the UI thread.
    pub fn fetch(url: &str) -> Result<Catalog, PluginError> {
        Self::fetch_with(url, &rustpy_net::config())
    }

    /// Policy is captured by the caller before starting a background worker.
    pub fn fetch_with(url: &str, policy: &rustpy_net::NetConfig) -> Result<Catalog, PluginError> {
        let bytes = http_get(policy, url, MAX_CATALOG_BYTES, HTTP_TIMEOUT)?;
        let text = String::from_utf8(bytes).map_err(|_| PluginError::Download {
            url: url.to_string(),
            message: "the catalog is not UTF-8 text".into(),
        })?;
        Catalog::parse(&text)
    }

    /// The newest version of `id` that works with this IDE.
    pub fn latest(&self, id: &str) -> Option<&CatalogEntry> {
        self.plugins
            .iter()
            .filter(|e| e.id == id && e.is_compatible())
            .max_by(|a, b| version_of(a).cmp(&version_of(b)))
    }

    /// One entry per plugin — the newest compatible version, else the newest
    /// version — sorted by name: the Marketplace list.
    pub fn listing(&self) -> Vec<&CatalogEntry> {
        let ids: BTreeSet<&str> = self.plugins.iter().map(|e| e.id.as_str()).collect();
        let mut out: Vec<&CatalogEntry> = ids
            .into_iter()
            .filter_map(|id| {
                self.latest(id).or_else(|| {
                    self.plugins
                        .iter()
                        .filter(|e| e.id == id)
                        .max_by(|a, b| version_of(a).cmp(&version_of(b)))
                })
            })
            .collect();
        out.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.id.cmp(&b.id))
        });
        out
    }

    /// [`listing`](Self::listing) entries whose id, name, description, vendor
    /// or a tag contains `query` (case-insensitive); all of them for a blank query.
    pub fn search(&self, query: &str) -> Vec<&CatalogEntry> {
        let needle = query.trim().to_lowercase();
        self.listing()
            .into_iter()
            .filter(|e| e.matches(&needle))
            .collect()
    }

    /// Installed plugins for which the catalog has a newer compatible version.
    pub fn updates(&self, registry: &PluginRegistry) -> Vec<PluginUpdate> {
        self.updates_for(
            registry
                .plugins()
                .iter()
                .map(|p| (p.id.as_str(), p.manifest.version.as_str())),
        )
    }

    /// Like [`updates`](Self::updates) for explicit `(id, installed version)` pairs.
    pub fn updates_for<'a>(
        &self,
        installed: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Vec<PluginUpdate> {
        let mut out = Vec::new();
        for (id, version) in installed {
            let Ok(current) = Version::parse(version) else {
                continue;
            };
            if let Some(entry) = self.latest(id) {
                if version_of(entry) > Some(current) {
                    out.push(PluginUpdate {
                        id: id.to_string(),
                        installed_version: version.to_string(),
                        entry: entry.clone(),
                    });
                }
            }
        }
        out
    }
}

fn version_of(entry: &CatalogEntry) -> Option<Version> {
    Version::parse(&entry.version).ok()
}

/// The bundle shipped with the IDE for `entry` when `entry` is one of the
/// bundled catalog's entries (same id, version, download URL and SHA-256),
/// so installing it needs no network. Other catalogs always download.
pub fn shipped_bundle(entry: &CatalogEntry) -> Option<&'static [u8]> {
    let sha = entry.sha256.trim().to_ascii_lowercase();
    let listed = Catalog::bundled().plugins.iter().any(|e| {
        e.id == entry.id
            && e.version == entry.version
            && e.download_url == entry.download_url
            && e.sha256 == sha
    });
    if !listed {
        return None;
    }
    SHIPPED_BUNDLES
        .iter()
        .copied()
        .find(|bundle| sha256_hex(bundle) == sha)
}

/// A transport error as text without the URL ureq puts first (the
/// [`PluginError::Download`] names it already), unless a redirect led to
/// another URL: "Connection Failed: Connect error: Connection refused".
fn transport_text(t: &ureq::Transport, requested: &str) -> String {
    let mut text = match t.url() {
        Some(failed) if failed.as_str() != requested => format!("{failed}: {}", t.kind()),
        _ => t.kind().to_string(),
    };
    if let Some(message) = t.message() {
        text.push_str(": ");
        text.push_str(message);
    }
    if let Some(source) = std::error::Error::source(t) {
        text.push_str(&format!(": {source}"));
    }
    text
}

/// GETs `url` into memory, refusing bodies over `limit` bytes.
fn redirect_target(current: &url::Url, location: &str) -> Result<url::Url, String> {
    let target = current.join(location).map_err(|error| error.to_string())?;
    validate_target(&target)?;
    if current.scheme() == "https" && target.scheme() != "https" {
        return Err("Marketplace redirects cannot downgrade HTTPS".into());
    }
    Ok(target)
}
fn validate_target(target: &url::Url) -> Result<(), String> {
    if !matches!(target.scheme(), "http" | "https")
        || target.host_str().is_none()
        || !target.username().is_empty()
        || target.password().is_some()
    {
        return Err("Marketplace URLs require HTTP(S), a host and no embedded credentials".into());
    }
    Ok(())
}
fn http_get(
    policy: &rustpy_net::NetConfig,
    url: &str,
    limit: u64,
    timeout: Duration,
) -> Result<Vec<u8>, PluginError> {
    let fail = |message: String| PluginError::Download {
        url: url.to_string(),
        message,
    };
    check_http_url(url).map_err(fail)?;
    let mut target = url::Url::parse(url).map_err(|error| fail(error.to_string()))?;
    validate_target(&target).map_err(fail)?;
    let started = Instant::now();
    let mut visited = BTreeSet::new();
    let response = loop {
        if visited.len() >= 10 || !visited.insert(target.to_string()) {
            return Err(fail("Marketplace redirect limit or loop".into()));
        }
        if started.elapsed() >= timeout {
            return Err(fail("Marketplace request deadline exceeded".into()));
        }
        // Rebuild for each URL: proxy/PAC routing and certificate policy must
        // never be inherited from a previous redirect's origin.
        let builder = rustpy_net::agent_builder_with(policy, target.as_str());
        let remaining = timeout
            .checked_sub(started.elapsed())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| fail("Marketplace request deadline exceeded".into()))?;
        let agent = builder
            .timeout_connect(HTTP_TIMEOUT.min(remaining))
            .timeout(remaining)
            .redirects(0)
            .user_agent(USER_AGENT)
            .build();
        let response = agent
            .get(target.as_str())
            .call()
            .map_err(|error| match error {
                ureq::Error::Status(code, response) => fail(format!(
                    "the server answered HTTP {code} {}",
                    response.status_text()
                )),
                ureq::Error::Transport(error) => fail(transport_text(&error, target.as_str())),
            })?;
        if matches!(response.status(), 301 | 302 | 303 | 307 | 308) {
            let location = response
                .header("location")
                .ok_or_else(|| fail("Marketplace redirect has no Location".into()))?;
            target = redirect_target(&target, location).map_err(fail)?;
            continue;
        }
        if !(200..300).contains(&response.status()) {
            return Err(fail(format!("Unexpected HTTP {}", response.status())));
        }
        break response;
    };
    let too_large = || fail(format!("the response is larger than {} KiB", limit / 1024));
    if let Some(length) = response
        .header("content-length")
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        if length > limit {
            return Err(too_large());
        }
    }
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| fail(format!("reading the response failed: {e}")))?;
    if bytes.len() as u64 > limit {
        return Err(too_large());
    }
    Ok(bytes)
}

/// Downloads `entry`'s bundle and verifies its SHA-256. Run it off the UI thread.
pub fn download(entry: &CatalogEntry) -> Result<Vec<u8>, PluginError> {
    download_with(entry, &rustpy_net::config())
}

pub fn download_with(
    entry: &CatalogEntry,
    policy: &rustpy_net::NetConfig,
) -> Result<Vec<u8>, PluginError> {
    let problems = entry.problems();
    if !problems.is_empty() {
        return Err(PluginError::Catalog(format!(
            "`{}`: {}",
            entry.id,
            problems.join("; ")
        )));
    }
    let bytes = http_get(
        policy,
        &entry.download_url,
        MAX_BUNDLE_BYTES as u64,
        DOWNLOAD_TIMEOUT,
    )?;
    let actual = sha256_hex(&bytes);
    let expected = entry.sha256.trim().to_ascii_lowercase();
    if actual != expected {
        return Err(PluginError::ChecksumMismatch {
            url: entry.download_url.clone(),
            expected,
            actual,
        });
    }
    Ok(bytes)
}

/// Downloads `entry`, verifies its SHA-256 and installs (or upgrades) it into
/// `<config_dir>/plugins/`. The bundle must contain exactly the plugin and
/// version the entry names. Entries of the bundled catalog install from the
/// bundles shipped with the IDE ([`shipped_bundle`]) without downloading.
/// Run it off the UI thread, then [`PluginRegistry::reload`] the registry the
/// UI holds.
pub fn download_and_install(
    entry: &CatalogEntry,
    config_dir: &Path,
) -> Result<InstallOutcome, PluginError> {
    download_and_install_with(entry, config_dir, &rustpy_net::config())
}

pub fn download_and_install_with(
    entry: &CatalogEntry,
    config_dir: &Path,
    policy: &rustpy_net::NetConfig,
) -> Result<InstallOutcome, PluginError> {
    let mut registry = PluginRegistry::load(config_dir);
    // Refuse early (before downloading) what the install would refuse anyway.
    registry.check_installable(&entry.id, &entry.version, entry.min_ide_version.as_deref())?;
    let bytes = match shipped_bundle(entry) {
        Some(bytes) => bytes.to_vec(),
        None => download_with(entry, policy)?,
    };
    registry.install_bundle_expecting(&bytes, Some((&entry.id, &entry.version)))
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    #[test]
    fn redirect_validation_preserves_tls_and_never_accepts_url_credentials() {
        let origin = url::Url::parse("https://catalog.example/plugins/index.json").unwrap();
        assert_eq!(
            redirect_target(&origin, "../bundle.rpplugin")
                .unwrap()
                .as_str(),
            "https://catalog.example/bundle.rpplugin"
        );
        assert!(redirect_target(&origin, "http://mirror.example/bundle").is_err());
        assert!(redirect_target(&origin, "https://secret:token@mirror.example/bundle").is_err());
        assert!(redirect_target(&origin, "file:///local/bundle").is_err());
    }
    #[test]
    fn captured_network_policy_survives_later_configuration_edits() {
        let mut local = rustpy_net::NetConfig::default();
        local.proxy.mode = rustpy_net::ProxyMode::Manual;
        let captured = std::sync::Arc::new(local.clone());
        local.proxy.mode = rustpy_net::ProxyMode::NoProxy;
        assert_eq!(captured.proxy.mode, rustpy_net::ProxyMode::Manual);
        assert_eq!(local.proxy.mode, rustpy_net::ProxyMode::NoProxy);
    }
    #[test]
    fn redirected_origin_uses_its_own_captured_proxy_exceptions() {
        let mut policy = rustpy_net::NetConfig::default();
        policy.proxy.mode = rustpy_net::ProxyMode::Manual;
        policy.proxy.host = "proxy.corp".into();
        policy.proxy.port = 3128;
        policy.proxy.no_proxy = "mirror.corp".into();
        let origin = url::Url::parse("https://catalog.example/index").unwrap();
        let redirected = redirect_target(&origin, "https://mirror.corp/plugin.rpplugin").unwrap();
        assert!(matches!(
            rustpy_net::proxy::choose(&policy.proxy, origin.as_str(), &|_| None),
            rustpy_net::ProxyChoice::Http(_)
        ));
        assert_eq!(
            rustpy_net::proxy::choose(&policy.proxy, redirected.as_str(), &|_| None),
            rustpy_net::ProxyChoice::Direct
        );
    }
}
