//! Which proxy a connection uses.
//!
//! * [`ProxyMode::NoProxy`]: always direct.
//! * [`ProxyMode::AutoDetect`]: the environment, as curl/pip read it:
//!   `https_proxy` / `HTTPS_PROXY` for `https://` URLs, `http_proxy` /
//!   `HTTP_PROXY` for `http://` URLs, then `all_proxy` / `ALL_PROXY`; hosts in
//!   `no_proxy` / `NO_PROXY` connect directly. Proxy URLs may use `http://`,
//!   `socks5://`, `socks5h://`,
//!   `socks4://`, `socks4a://` or no scheme (HTTP); credentials in the URL are
//!   percent-decoded.
//! * [`ProxyMode::Manual`]: the host/port/kind/credentials of [`ProxySettings`]
//!   and its "No proxy for" list.
//!
//! Loopback hosts (`localhost`, `127.0.0.0/8`, `::1`) never use a proxy.
//!
//! No-proxy lists ([`no_proxy_matches`]) are comma- or space-separated entries:
//! `*` (everything), a host or domain (`example.com`, `.example.com` and
//! `*.example.com` all cover `example.com` and its subdomains), a pattern with
//! `*` wildcards (`192.168.*`, `build-*.corp`), an IP address, or a CIDR range
//! (`10.0.0.0/8`, `fd00::/8`); `:port` limits an entry to one port.

use std::fmt;
use std::net::IpAddr;

use rustpy_settings::{ProxyKind, ProxyMode, ProxySettings};

/// SOCKS protocol version.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SocksVersion {
    /// SOCKS4: IPv4 only, the client resolves host names.
    V4,
    /// SOCKS4a: host names are resolved by the proxy.
    V4a,
    /// SOCKS5 (host names are resolved by the proxy).
    V5,
}

/// A proxy server and its credentials.
#[derive(Clone, PartialEq, Eq)]
pub struct ProxyServer {
    pub host: String,
    pub port: u16,
    pub user: Option<String>,
    pub password: Option<String>,
}

impl fmt::Debug for ProxyServer {
    // Never prints the password.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyServer")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl ProxyServer {
    /// `host:port` (IPv6 hosts in brackets).
    pub fn address(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// The `ureq` proxy for an HTTP proxy server. ureq splits the user from the
    /// password at the first `:` and takes credentials verbatim (no percent
    /// decoding), so they are passed unencoded. Errors for what ureq cannot
    /// express (IPv6 addresses); [`crate::http_bridge`] handles those.
    pub fn to_ureq(&self) -> Result<ureq::Proxy, String> {
        if self.host.contains(':') {
            return Err(format!(
                "IPv6 proxy addresses are not supported: {}",
                self.host
            ));
        }
        let userinfo = match (&self.user, &self.password) {
            (Some(u), p) if !u.is_empty() => {
                if u.contains(':') {
                    return Err("The proxy login must not contain ':'".into());
                }
                format!("{u}:{}@", p.as_deref().unwrap_or(""))
            }
            _ => String::new(),
        };
        ureq::Proxy::new(format!("http://{userinfo}{}:{}", self.host, self.port))
            .map_err(|e| e.to_string())
    }
}

/// How a connection to one URL is made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProxyChoice {
    Direct,
    Http(ProxyServer),
    Socks(ProxyServer, SocksVersion),
    Pac(Vec<ProxyChoice>),
    Unavailable(String),
}

impl ProxyChoice {
    /// Human-readable description for connection checks ("direct", "HTTP proxy proxy:3128").
    pub fn describe(&self) -> String {
        match self {
            ProxyChoice::Direct => "direct connection".into(),
            ProxyChoice::Pac(routes) => routes
                .iter()
                .map(ProxyChoice::describe)
                .collect::<Vec<_>>()
                .join(" → "),
            ProxyChoice::Unavailable(error) => error.clone(),
            ProxyChoice::Http(p) => format!("HTTP proxy {}", p.address()),
            ProxyChoice::Socks(p, v) => format!(
                "{} proxy {}",
                match v {
                    SocksVersion::V4 => "SOCKS4",
                    SocksVersion::V4a => "SOCKS4a",
                    SocksVersion::V5 => "SOCKS5",
                },
                p.address()
            ),
        }
    }
}

/// Scheme, host (lower case, IPv6 without brackets) and port of `url`.
pub fn url_target(url: &str) -> Option<(String, String, u16)> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    let host = match parsed.host()? {
        url::Host::Domain(d) => d.trim_end_matches('.').to_ascii_lowercase(),
        url::Host::Ipv4(ip) => ip.to_string(),
        url::Host::Ipv6(ip) => ip.to_string(),
    };
    let port = parsed.port_or_known_default()?;
    Some((parsed.scheme().to_ascii_lowercase(), host, port))
}

pub fn is_loopback(host: &str) -> bool {
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Reads an environment variable of this process (empty values count as unset).
pub fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// The proxy for a connection to `url` under `settings`; `env` looks up
/// environment variables (for [`ProxyMode::AutoDetect`]).
pub fn choose(
    settings: &ProxySettings,
    url: &str,
    env: &dyn Fn(&str) -> Option<String>,
) -> ProxyChoice {
    let Some((scheme, host, port)) = url_target(url) else {
        return ProxyChoice::Direct;
    };
    if is_loopback(&host) {
        return ProxyChoice::Direct;
    }
    match settings.mode {
        ProxyMode::NoProxy => ProxyChoice::Direct,
        ProxyMode::Manual => {
            let host_setting = settings.host.trim();
            if host_setting.is_empty() || no_proxy_matches(&settings.no_proxy, &host, port) {
                return ProxyChoice::Direct;
            }
            // No credentials while the password is unknown (see `ProxySettings::credentials`):
            // the proxy then answers 407 instead of counting a failed login.
            let (user, password) = match settings.credentials() {
                Some((login, password)) => (Some(login.to_string()), Some(password.to_string())),
                None => (None, None),
            };
            let server = ProxyServer {
                host: host_setting
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .to_string(),
                port: settings.effective_port(),
                user,
                password,
            };
            match settings.kind {
                ProxyKind::Http => ProxyChoice::Http(server),
                ProxyKind::Socks => ProxyChoice::Socks(server, SocksVersion::V5),
            }
        }
        ProxyMode::Pac => {
            let mut cfg = (*crate::config()).clone();
            cfg.proxy = settings.clone();
            crate::pac::choose_with(&cfg, url)
        }
        ProxyMode::AutoDetect => {
            let no_proxy = env("no_proxy")
                .or_else(|| env("NO_PROXY"))
                .unwrap_or_default();
            if no_proxy_matches(&no_proxy, &host, port) {
                return ProxyChoice::Direct;
            }
            let names: [&str; 4] = if scheme == "https" || scheme == "wss" {
                ["https_proxy", "HTTPS_PROXY", "all_proxy", "ALL_PROXY"]
            } else {
                ["http_proxy", "HTTP_PROXY", "all_proxy", "ALL_PROXY"]
            };
            for name in names {
                let Some(value) = env(name)
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
                else {
                    continue;
                };
                match parse_proxy_url(&value) {
                    Ok(choice) => return choice,
                    // Do not silently bypass an explicitly configured proxy,
                    // and never include its potentially secret URL in diagnostics.
                    Err(error) => {
                        return ProxyChoice::Unavailable(format!(
                            "Invalid proxy in ${name}: {error}"
                        ))
                    }
                }
            }
            ProxyChoice::Direct
        }
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            // Two hex digits exactly (`from_str_radix` alone would accept "+5").
            let digits = &bytes[i + 1..i + 3];
            let hex = digits
                .iter()
                .all(u8::is_ascii_hexdigit)
                .then(|| {
                    std::str::from_utf8(digits)
                        .ok()
                        .and_then(|h| u8::from_str_radix(h, 16).ok())
                })
                .flatten();
            if let Some(b) = hex {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parses a proxy URL from the environment (`http://user:pw@proxy:3128`,
/// `socks5h://proxy:1080`, `proxy.corp:8080`).
pub fn parse_proxy_url(value: &str) -> Result<ProxyChoice, String> {
    let value = value.trim();
    let with_scheme = if value.contains("://") {
        value.to_string()
    } else {
        format!("http://{value}")
    };
    let (scheme, _) = with_scheme.split_once("://").unwrap_or(("http", ""));
    let scheme = scheme.to_ascii_lowercase();
    let parsed = url::Url::parse(&with_scheme).map_err(|e| format!("invalid proxy URL ({e})"))?;
    let host = match parsed.host() {
        Some(url::Host::Domain(d)) => d.to_ascii_lowercase(),
        Some(url::Host::Ipv4(ip)) => ip.to_string(),
        Some(url::Host::Ipv6(ip)) => ip.to_string(),
        None => return Err("the proxy URL has no host".into()),
    };
    if host.is_empty() {
        return Err("the proxy URL has no host".into());
    }
    let user = Some(percent_decode(parsed.username())).filter(|u| !u.is_empty());
    let password = parsed.password().map(percent_decode);
    let (default_port, socks) = match scheme.as_str() {
        "http" => (80, None),
        "https"=>return Err("TLS-encrypted proxy endpoints are unsupported; HTTPS origins use an HTTP CONNECT proxy. The connection was blocked rather than sending proxy credentials unencrypted".into()),
        "socks5" | "socks5h" | "socks" => (1080, Some(SocksVersion::V5)),
        "socks4a" => (1080, Some(SocksVersion::V4a)),
        "socks4" => (1080, Some(SocksVersion::V4)),
        other => return Err(format!("unsupported proxy scheme '{other}'")),
    };
    let server = ProxyServer {
        host,
        port: parsed.port().unwrap_or(default_port),
        user,
        password,
    };
    Ok(match socks {
        Some(v) => ProxyChoice::Socks(server, v),
        None => ProxyChoice::Http(server),
    })
}

/// Splits `host:port` / `[v6]:port` / `host` into host and optional port.
fn split_host_port(entry: &str) -> (&str, Option<u16>) {
    if let Some(rest) = entry.strip_prefix('[') {
        if let Some((host, after)) = rest.split_once(']') {
            return (host, after.strip_prefix(':').and_then(|p| p.parse().ok()));
        }
        return (rest, None);
    }
    match entry.rsplit_once(':') {
        // A bare IPv6 address has several colons: no port.
        Some((host, port))
            if !host.contains(':')
                && !port.is_empty()
                && port.bytes().all(|b| b.is_ascii_digit()) =>
        {
            (host, port.parse().ok())
        }
        _ => (entry, None),
    }
}

/// `*` wildcard match (case already folded).
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == t[ti] || p[pi] == '?') {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            pi += 1;
            mark = ti;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// `10.0.0.0/8` / `fd00::/8` as network and prefix length (None when invalid).
fn parse_cidr(cidr: &str) -> Option<(IpAddr, u32)> {
    let (net, bits) = cidr.split_once('/')?;
    let net: IpAddr = net
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .ok()?;
    let bits: u32 = bits.parse().ok()?;
    let max = if net.is_ipv4() { 32 } else { 128 };
    (bits <= max).then_some((net, bits))
}

fn cidr_contains(cidr: &str, ip: IpAddr) -> Option<bool> {
    let (net, bits) = parse_cidr(cidr)?;
    Some(match (net, ip) {
        (IpAddr::V4(n), IpAddr::V4(a)) if bits <= 32 => {
            let mask = if bits == 0 {
                0
            } else {
                u32::MAX << (32 - bits)
            };
            u32::from(n) & mask == u32::from(a) & mask
        }
        (IpAddr::V6(n), IpAddr::V6(a)) if bits <= 128 => {
            let mask = if bits == 0 {
                0
            } else {
                u128::MAX << (128 - bits)
            };
            u128::from(n) & mask == u128::from(a) & mask
        }
        // An IPv4-mapped IPv6 address against an IPv4 range.
        (IpAddr::V4(n), IpAddr::V6(a)) if bits <= 32 => match a.to_ipv4_mapped() {
            Some(a) => {
                let mask = if bits == 0 {
                    0
                } else {
                    u32::MAX << (32 - bits)
                };
                u32::from(n) & mask == u32::from(a) & mask
            }
            None => false,
        },
        _ => false,
    })
}

/// Whether `host` (lower case, IPv6 without brackets) at `port` is listed in a
/// no-proxy list (see the module documentation for the syntax).
pub fn no_proxy_matches(list: &str, host: &str, port: u16) -> bool {
    let host = host
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let host_ip = host.parse::<IpAddr>().ok();
    list.split([',', ' ', '\t', ';', '\n'])
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .any(|entry| {
            if entry == "*" {
                return true;
            }
            if entry.contains('/') {
                return host_ip
                    .and_then(|ip| cidr_contains(&entry, ip))
                    .unwrap_or(false);
            }
            let (pattern, entry_port) = split_host_port(&entry);
            if entry_port.is_some_and(|p| p != port) {
                return false;
            }
            let pattern = pattern.trim_end_matches('.');
            if let (Some(ip), Ok(entry_ip)) = (host_ip, pattern.parse::<IpAddr>()) {
                return ip == entry_ip;
            }
            // Domain rules: `.example.com`, `*.example.com` and `example.com` cover the
            // domain and its subdomains. They never apply to IP addresses.
            let domain = pattern
                .strip_prefix("*.")
                .or_else(|| pattern.strip_prefix('.'))
                .unwrap_or(pattern);
            if !domain.contains('*') && !domain.is_empty() {
                if host_ip.is_some() {
                    return host == domain;
                }
                return host == domain
                    || host.strip_suffix(domain).is_some_and(|p| p.ends_with('.'));
            }
            glob_match(pattern, &host)
        })
}

/// `fd00::1`, `[fd00::1]` or `fe80::1%eth0`: an IPv6 address, optionally in
/// brackets and with a zone.
fn is_ipv6_literal(host: &str) -> bool {
    let inner = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    let address = inner.split_once('%').map_or(inner, |(a, _)| a);
    address.parse::<std::net::Ipv6Addr>().is_ok()
}

/// Problems with manual proxy settings, for the settings page (empty when usable).
pub fn validate(settings: &ProxySettings) -> Vec<String> {
    let mut problems = Vec::new();
    if settings.mode == ProxyMode::Pac {
        if settings.pac_url.trim().is_empty() {
            problems.push("Enter a PAC URL or local file".into());
        }
        if !(30..=86400).contains(&settings.pac_refresh_seconds) {
            problems.push("PAC refresh must be between 30 and 86400 seconds".into());
        }
        if settings.authenticate && settings.login.trim().is_empty() {
            problems.push("Enter the PAC proxy login".into());
        }
        if settings.login.len() > 255 || settings.password.len() > 255 {
            problems.push("PAC SOCKS credentials cannot exceed 255 bytes".into());
        }
        return problems;
    }
    if settings.mode != ProxyMode::Manual {
        return problems;
    }
    let host = settings.host.trim();
    if host.is_empty() {
        problems.push("Enter the proxy host name".to_string());
    } else if host.contains("://") {
        problems.push("Enter the host name without a scheme (e.g. proxy.corp)".to_string());
    } else if host
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '/' | '@' | '?' | '#'))
    {
        problems.push(format!("Invalid proxy host name: {host}"));
    } else if host.contains(':') && !is_ipv6_literal(host) {
        // `proxy.corp:3128` would be taken for an IPv6 address and never resolve.
        problems.push(match split_host_port(host) {
            (name, Some(port)) if !name.is_empty() => format!(
                "Enter the port under Port number, not in the host name: host name {}, port {port}",
                name.trim_start_matches('[').trim_end_matches(']')
            ),
            _ => format!("Invalid proxy host name: {host} (only IPv6 addresses contain ':')"),
        });
    }
    if settings.authenticate {
        if settings.login.trim().is_empty() {
            problems.push("Enter the proxy login, or turn off proxy authentication".to_string());
        }
        if settings.login.contains(':') && settings.kind == ProxyKind::Http {
            problems.push("The proxy login must not contain ':'".to_string());
        }
        if settings.kind == ProxyKind::Socks
            && (settings.login.len() > 255 || settings.password.len() > 255)
        {
            problems.push("SOCKS5 logins and passwords are limited to 255 bytes".to_string());
        }
    }
    for entry in settings
        .no_proxy
        .split([',', ' ', '\t', ';', '\n'])
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        if entry.contains('/') && parse_cidr(entry).is_none() {
            problems.push(format!(
                "Invalid address range in \"No proxy for\": {entry}"
            ));
        }
    }
    problems
}

/// Environment variables that make child processes (pip, uv, git, curl) use the
/// manually configured proxy. Empty unless the mode is Manual with an HTTP
/// proxy (or SOCKS5, as `socks5h://`); in the other modes children keep the
/// environment they inherit.
pub fn proxy_env_vars(settings: &ProxySettings) -> Vec<(String, String)> {
    if settings.mode != ProxyMode::Manual || settings.host.trim().is_empty() {
        return Vec::new();
    }
    fn enc(s: &str) -> String {
        s.bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect()
    }
    let scheme = match settings.kind {
        ProxyKind::Http => "http",
        ProxyKind::Socks => "socks5h",
    };
    // Like `choose`: no `login:@` while the password is not known.
    let userinfo = match settings.credentials() {
        Some((login, password)) => format!("{}:{}@", enc(login), enc(password)),
        None => String::new(),
    };
    let host = settings.host.trim();
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let url = format!("{scheme}://{userinfo}{host}:{}", settings.effective_port());
    let no_proxy: Vec<&str> = settings
        .no_proxy
        .split([',', ' ', '\t', ';', '\n'])
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .collect();
    let no_proxy = ["localhost", "127.0.0.1", "::1"]
        .into_iter()
        .chain(no_proxy)
        .collect::<Vec<_>>()
        .join(",");
    let mut vars = Vec::new();
    for name in [
        "http_proxy",
        "HTTP_PROXY",
        "https_proxy",
        "HTTPS_PROXY",
        "all_proxy",
        "ALL_PROXY",
    ] {
        vars.push((name.to_string(), url.clone()));
    }
    vars.push(("no_proxy".to_string(), no_proxy.clone()));
    vars.push(("NO_PROXY".to_string(), no_proxy));
    vars
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    fn auto() -> ProxySettings {
        ProxySettings::default()
    }

    fn server(host: &str, port: u16) -> ProxyServer {
        ProxyServer {
            host: host.into(),
            port,
            user: None,
            password: None,
        }
    }

    #[test]
    fn environment_proxies_by_scheme() {
        let e = env(&[
            ("HTTPS_PROXY", "http://proxy.corp:3128"),
            ("http_proxy", "plain.corp:80"),
        ]);
        assert_eq!(
            choose(&auto(), "https://pypi.org/pypi/x/json", &e),
            ProxyChoice::Http(server("proxy.corp", 3128))
        );
        assert_eq!(
            choose(&auto(), "http://example.com/", &e),
            ProxyChoice::Http(server("plain.corp", 80))
        );
        // Lower case wins over upper case; ALL_PROXY is the fallback.
        let e = env(&[
            ("https_proxy", "http://lower:1"),
            ("HTTPS_PROXY", "http://upper:2"),
        ]);
        assert_eq!(
            choose(&auto(), "https://a.b/", &e),
            ProxyChoice::Http(server("lower", 1))
        );
        let e = env(&[("ALL_PROXY", "socks5h://sock.corp")]);
        assert_eq!(
            choose(&auto(), "https://a.b/", &e),
            ProxyChoice::Socks(server("sock.corp", 1080), SocksVersion::V5)
        );
        assert_eq!(
            choose(&auto(), "https://a.b/", &env(&[])),
            ProxyChoice::Direct
        );
        // Loopback never goes through a proxy.
        let e = env(&[("ALL_PROXY", "http://p:1")]);
        for url in [
            "http://localhost:8000/x",
            "http://127.0.0.5/",
            "http://[::1]:80/",
            "http://app.localhost/",
        ] {
            assert_eq!(choose(&auto(), url, &e), ProxyChoice::Direct, "{url}");
        }
    }

    #[test]
    fn environment_no_proxy() {
        let e = env(&[
            ("ALL_PROXY", "http://all.corp:1080"),
            ("NO_PROXY", "internal.corp, .example.com:443"),
        ]);
        assert_eq!(
            choose(&auto(), "https://api.anthropic.com/v1/messages", &e),
            ProxyChoice::Http(server("all.corp", 1080))
        );
        assert_eq!(
            choose(&auto(), "https://llm.internal.corp/v1/messages", &e),
            ProxyChoice::Direct
        );
        assert_eq!(
            choose(&auto(), "https://internal.corp/v1/messages", &e),
            ProxyChoice::Direct
        );
        assert_eq!(
            choose(&auto(), "https://api.example.com/v1/messages", &e),
            ProxyChoice::Direct
        );
        assert_eq!(
            choose(&auto(), "http://api.example.com/v1/messages", &e),
            ProxyChoice::Http(server("all.corp", 1080)),
            "the entry is for port 443 only"
        );
        assert_eq!(
            choose(&auto(), "https://notexample.com/", &e),
            ProxyChoice::Http(server("all.corp", 1080))
        );
        let e = env(&[("https_proxy", "http://p:1"), ("no_proxy", "*")]);
        assert_eq!(
            choose(&auto(), "https://api.anthropic.com/", &e),
            ProxyChoice::Direct
        );
    }

    #[test]
    fn proxy_urls() {
        let p = parse_proxy_url("http://us%40er:p%3Ass%40w@proxy.corp:3128/").unwrap();
        assert_eq!(
            p,
            ProxyChoice::Http(ProxyServer {
                host: "proxy.corp".into(),
                port: 3128,
                user: Some("us@er".into()),
                password: Some("p:ss@w".into())
            })
        );
        assert_eq!(
            parse_proxy_url("PROXY.corp:8080").unwrap(),
            ProxyChoice::Http(server("proxy.corp", 8080))
        );
        assert_eq!(
            parse_proxy_url("http://proxy").unwrap(),
            ProxyChoice::Http(server("proxy", 80))
        );
        assert!(parse_proxy_url("https://proxy").is_err());
        assert_eq!(
            parse_proxy_url("socks4a://s:9").unwrap(),
            ProxyChoice::Socks(server("s", 9), SocksVersion::V4a)
        );
        assert_eq!(
            parse_proxy_url("socks4://s").unwrap(),
            ProxyChoice::Socks(server("s", 1080), SocksVersion::V4)
        );
        assert_eq!(
            parse_proxy_url("socks5://[fe80::1]:1080").unwrap(),
            ProxyChoice::Socks(server("fe80::1", 1080), SocksVersion::V5)
        );
        assert!(parse_proxy_url("ftp://proxy:21").is_err());
        assert!(parse_proxy_url("http://:80").is_err());
        // Debug output never shows the password.
        let dbg = format!("{p:?}");
        assert!(!dbg.contains("p:ss@w") && dbg.contains("redacted"), "{dbg}");
    }

    #[test]
    fn manual_and_no_proxy_modes() {
        let manual = ProxySettings {
            mode: ProxyMode::Manual,
            host: "proxy.corp".into(),
            port: 3128,
            no_proxy: "*.internal, 10.0.0.0/8".into(),
            authenticate: true,
            login: "me".into(),
            password: "pw".into(),
            ..Default::default()
        };
        let with_auth = ProxyChoice::Http(ProxyServer {
            host: "proxy.corp".into(),
            port: 3128,
            user: Some("me".into()),
            password: Some("pw".into()),
        });
        let e = env(&[("ALL_PROXY", "http://env:1")]);
        assert_eq!(
            choose(&manual, "https://pypi.org/", &e),
            with_auth,
            "manual settings ignore the environment"
        );
        assert_eq!(
            choose(&manual, "https://git.internal/", &e),
            ProxyChoice::Direct
        );
        assert_eq!(
            choose(&manual, "http://10.1.2.3:8080/", &e),
            ProxyChoice::Direct
        );
        let socks = ProxySettings {
            kind: ProxyKind::Socks,
            port: 0,
            authenticate: false,
            ..manual.clone()
        };
        assert_eq!(
            choose(&socks, "https://pypi.org/", &e),
            ProxyChoice::Socks(server("proxy.corp", 1080), SocksVersion::V5)
        );
        let none = ProxySettings {
            mode: ProxyMode::NoProxy,
            ..manual.clone()
        };
        assert_eq!(choose(&none, "https://pypi.org/", &e), ProxyChoice::Direct);
        let empty_host = ProxySettings {
            host: "  ".into(),
            ..manual
        };
        assert_eq!(
            choose(&empty_host, "https://pypi.org/", &e),
            ProxyChoice::Direct
        );
        assert_eq!(choose(&auto(), "not a url", &e), ProxyChoice::Direct);
    }

    #[test]
    fn no_proxy_syntax() {
        let m = |list: &str, host: &str, port: u16| no_proxy_matches(list, host, port);
        assert!(m("*", "anything.com", 443));
        assert!(m("example.com", "example.com", 443) && m("example.com", "a.b.example.com", 80));
        assert!(m(".example.com", "example.com", 443) && m("*.example.com", "x.example.com", 443));
        assert!(!m("example.com", "badexample.com", 443));
        assert!(
            m("EXAMPLE.com.", "Api.Example.COM", 443),
            "case and trailing dots are ignored"
        );
        assert!(m("192.168.*", "192.168.1.20", 80) && !m("192.168.*", "192.169.1.20", 80));
        assert!(m("build-*.corp", "build-17.corp", 80) && !m("build-*.corp", "build.corp", 80));
        assert!(m("10.0.0.0/8", "10.20.30.40", 80) && !m("10.0.0.0/8", "11.0.0.1", 80));
        assert!(m("fd00::/8", "fd12::1", 80) && !m("fd00::/8", "fe80::1", 80));
        assert!(m("[::1]", "::1", 80) && m("::1", "::1", 80));
        assert!(m("[fd00::1]:8443", "fd00::1", 8443) && !m("[fd00::1]:8443", "fd00::1", 443));
        assert!(m("host:8080", "host", 8080) && !m("host:8080", "host", 80));
        assert!(
            !m("0.1", "10.0.0.1", 80),
            "domain suffix rules never match IP addresses"
        );
        assert!(m("1.2.3.4", "1.2.3.4", 80) && !m("1.2.3.4", "1.2.3.5", 80));
        assert!(
            m("a.com b.com;c.com", "c.com", 1),
            "space and semicolon separators"
        );
        assert!(!m("", "x.com", 1) && !m(" , ", "x.com", 1));
        assert!(!m("10.0.0.0/8", "ten.corp", 80) && !m("10.0.0.0/99", "10.0.0.1", 80));
        assert!(
            m("10.0.0.0/8", "::ffff:10.1.1.1", 80),
            "IPv4-mapped addresses"
        );
    }

    #[test]
    fn validation() {
        let mut s = ProxySettings {
            mode: ProxyMode::Manual,
            ..Default::default()
        };
        assert_eq!(validate(&s).len(), 1);
        s.host = "http://proxy".into();
        assert!(validate(&s)[0].contains("without a scheme"));
        s.host = "proxy.corp".into();
        assert!(validate(&s).is_empty());
        s.authenticate = true;
        assert!(validate(&s)[0].contains("login"));
        s.login = "a:b".into();
        assert!(validate(&s)[0].contains("':'"));
        s.login = "ab".into();
        s.no_proxy = "10.0.0.0/33, ok.com".into();
        assert!(validate(&s)[0].contains("10.0.0.0/33"));
        s.no_proxy = "10.0.0.0/8 fd00::/8".into();
        assert!(validate(&s).is_empty(), "{:?}", validate(&s));
        s.host = "fd00::3128".into();
        assert!(
            validate(&s).is_empty(),
            "IPv6 proxies go through the bridge: {:?}",
            validate(&s)
        );
        s.host = "proxy corp".into();
        assert!(validate(&s)[0].contains("Invalid proxy host name"));
        assert!(
            validate(&ProxySettings::default()).is_empty(),
            "auto-detect has nothing to validate"
        );
        // A port typed into the host name field is no IPv6 address.
        s.host = "proxy.corp:3128".into();
        assert_eq!(validate(&s), vec!["Enter the port under Port number, not in the host name: host name proxy.corp, port 3128".to_string()]);
        s.host = "[fd00::1]:3128".into();
        assert!(
            validate(&s)[0].contains("host name fd00::1, port 3128"),
            "{:?}",
            validate(&s)
        );
        s.host = "a:b:c".into();
        assert!(
            validate(&s)[0].contains("only IPv6 addresses"),
            "{:?}",
            validate(&s)
        );
        for v6 in [
            "fd00::1",
            "[fd00::1]",
            "::1",
            "fe80::1%eth0",
            "[fe80::1%25eth0]",
        ] {
            s.host = v6.into();
            assert!(validate(&s).is_empty(), "{v6}: {:?}", validate(&s));
        }
    }

    #[test]
    fn no_credentials_without_a_password() {
        // Authentication on, but the remembered password is not loaded yet (or the
        // database is locked, or it was not remembered): an empty password would be
        // a failed login for every request.
        let s = ProxySettings {
            mode: ProxyMode::Manual,
            host: "proxy.corp".into(),
            port: 3128,
            authenticate: true,
            login: "CORP\\alice".into(),
            ..Default::default()
        };
        let e = env(&[]);
        assert_eq!(
            choose(&s, "https://pypi.org/", &e),
            ProxyChoice::Http(server("proxy.corp", 3128))
        );
        let socks = ProxySettings {
            kind: ProxyKind::Socks,
            ..s.clone()
        };
        assert_eq!(
            choose(&socks, "https://pypi.org/", &e),
            ProxyChoice::Socks(server("proxy.corp", 3128), SocksVersion::V5)
        );
        // pip and uv get the proxy without `login:@`.
        let vars = proxy_env_vars(&s);
        assert!(
            vars.iter()
                .all(|(_, v)| !v.contains('@') && !v.contains("alice")),
            "{vars:?}"
        );
        assert_eq!(vars[0].1, "http://proxy.corp:3128");
        // Once the password is known it is sent.
        let known = ProxySettings {
            password: "pw".into(),
            ..s
        };
        assert_eq!(
            choose(&known, "https://pypi.org/", &e),
            ProxyChoice::Http(ProxyServer {
                host: "proxy.corp".into(),
                port: 3128,
                user: Some("CORP\\alice".into()),
                password: Some("pw".into())
            })
        );
        assert_eq!(
            proxy_env_vars(&known)[0].1,
            "http://CORP%5Calice:pw@proxy.corp:3128"
        );
    }

    #[test]
    fn child_process_environment() {
        assert!(proxy_env_vars(&ProxySettings::default()).is_empty());
        let s = ProxySettings {
            mode: ProxyMode::Manual,
            host: "proxy.corp".into(),
            port: 3128,
            no_proxy: "*.internal".into(),
            authenticate: true,
            login: "me@corp".into(),
            password: "p:w d".into(),
            ..Default::default()
        };
        let vars = proxy_env_vars(&s);
        let get = |k: &str| {
            vars.iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert_eq!(
            get("https_proxy"),
            "http://me%40corp:p%3Aw%20d@proxy.corp:3128"
        );
        assert_eq!(get("HTTP_PROXY"), get("https_proxy"));
        assert_eq!(get("NO_PROXY"), "localhost,127.0.0.1,::1,*.internal");
        // The URL round-trips through the environment parser.
        assert_eq!(
            parse_proxy_url(&get("ALL_PROXY")).unwrap(),
            ProxyChoice::Http(ProxyServer {
                host: "proxy.corp".into(),
                port: 3128,
                user: Some("me@corp".into()),
                password: Some("p:w d".into())
            })
        );
        let socks = ProxySettings {
            kind: ProxyKind::Socks,
            authenticate: false,
            port: 0,
            ..s
        };
        assert_eq!(proxy_env_vars(&socks)[0].1, "socks5h://proxy.corp:1080");
    }

    #[test]
    fn ureq_proxy_keeps_raw_credentials() {
        let p = ProxyServer {
            host: "proxy.corp".into(),
            port: 3128,
            user: Some("me".into()),
            password: Some("p@ss:w/rd".into()),
        };
        assert!(p.to_ureq().is_ok());
        let bad = ProxyServer {
            user: Some("a:b".into()),
            ..p.clone()
        };
        assert!(bad.to_ureq().is_err());
        let v6 = ProxyServer {
            host: "fe80::1".into(),
            ..p
        };
        assert!(v6.to_ureq().is_err());
        assert_eq!(v6.address(), "[fe80::1]:3128");
    }

    #[test]
    fn glob() {
        assert!(glob_match("*", ""));
        assert!(glob_match("a*b*c", "aXXbYYc"));
        assert!(!glob_match("a*b", "a"));
        assert!(glob_match("*.corp", "x.corp"));
        assert!(!glob_match("*.corp", "corp"));
    }

    #[test]
    fn percent_decoding_edge_cases() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%4"), "%4");
        assert_eq!(percent_decode("%C3%A9"), "é");
        assert_eq!(percent_decode("%+5x"), "%+5x", "a sign is no hex digit");
    }
}
