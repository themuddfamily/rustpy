//! Reading the release list from the GitHub Releases API
//! (`GET /repos/{owner}/{repo}/releases`).
//!
//! Every failure maps to a [`FetchError`] with a calm message: a private repository (404),
//! GitHub's anonymous rate limit (403/429) or no connection are normal situations for an
//! update check, not errors to alarm the user with.

use std::io::Read;
use std::time::{Duration, Instant};

use crate::release::{parse_releases, Release};

/// The repository RustPy releases are published in.
pub const DEFAULT_REPOSITORY: &str = "themuddfamily/rustpy";

/// Why the release list could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchError {
    /// The "Releases" setting is neither `owner/name` nor a usable URL.
    BadSetting(String),
    /// HTTP 404: no such repository, a private one, or no releases yet.
    NotFound,
    /// HTTP 401, 403 or 429. `rate_limited` when GitHub's request limit is used up;
    /// `reset_at` is the Unix time when it resets.
    Refused {
        status: u16,
        rate_limited: bool,
        reset_at: Option<i64>,
    },
    /// Another HTTP status.
    Status(u16),
    /// No answer: offline, DNS, proxy, TLS or a timeout.
    Offline(String),
    /// The answer is not a release list.
    Invalid(String),
}

impl FetchError {
    /// Whether the server answered, so that the check counts as done for today.
    pub fn reached_server(&self) -> bool {
        !matches!(self, FetchError::Offline(_) | FetchError::BadSetting(_))
    }

    /// A calm, user-facing explanation.
    pub fn message(&self) -> String {
        match self {
            FetchError::BadSetting(s) => format!("\"{s}\" is not a GitHub repository (owner/name) or a releases URL. Change it in Settings › Appearance & Behavior › System Settings › Updates."),
            FetchError::NotFound => "No published releases were found. The release repository may be private, or it has no releases yet.".into(),
            FetchError::Refused { rate_limited: true, .. } => "GitHub's limit for anonymous requests from this network has been reached for now. RustPy will check again later.".into(),
            FetchError::Refused { status, .. } => format!("The release server declined the request (HTTP {status}). The release repository may be private."),
            FetchError::Status(code) => format!("The release server is not available right now (HTTP {code}). Please try again later."),
            FetchError::Offline(_) => "Could not connect to the update server. Check your internet connection and proxy settings, then try again.".into(),
            FetchError::Invalid(_) => "The update server sent an answer that is not a release list.".into(),
        }
    }
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::Offline(detail) | FetchError::Invalid(detail) => {
                write!(f, "{} ({detail})", self.message())
            }
            _ => f.write_str(&self.message()),
        }
    }
}

/// `owner` and `name` from `owner/name[/…]`; `.git` is dropped from the name.
fn owner_and_name(path: &str, exact: bool) -> Option<(&str, &str)> {
    let mut parts = path.trim_matches('/').split('/');
    let owner = parts.next()?;
    let name = parts.next()?;
    if exact && parts.next().is_some() {
        return None;
    }
    let name = name.strip_suffix(".git").unwrap_or(name);
    let valid = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    (valid(owner) && valid(name)).then_some((owner, name))
}

/// The API URL of the release list for the "Releases" setting:
///
/// * empty: the default repository ([`DEFAULT_REPOSITORY`]);
/// * `owner/name`;
/// * a repository page, `https://github.com/owner/name` (also with `.git` or `/releases…`);
/// * an API URL, used as is: `https://api.github.com/repos/owner/name/releases`, or GitHub
///   Enterprise's `https://ghe.example.com/api/v3/repos/owner/name/releases`;
/// * a GitHub Enterprise repository page, `https://ghe.example.com/owner/name`.
pub fn releases_api_url(setting: &str) -> Result<String, FetchError> {
    let s = setting.trim();
    let bad = || FetchError::BadSetting(s.to_string());
    if s.is_empty() {
        return releases_api_url(DEFAULT_REPOSITORY);
    }
    let Some((scheme, rest)) = s.split_once("://") else {
        let (owner, name) = owner_and_name(s, true).ok_or_else(bad)?;
        return Ok(format!(
            "https://api.github.com/repos/{owner}/{name}/releases"
        ));
    };
    if !scheme.eq_ignore_ascii_case("https") && !scheme.eq_ignore_ascii_case("http") {
        return Err(bad());
    }
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let path = path.split(['?', '#']).next().unwrap_or("");
    if host.is_empty() || host.contains(char::is_whitespace) {
        return Err(bad());
    }
    if host.eq_ignore_ascii_case("github.com") || host.eq_ignore_ascii_case("www.github.com") {
        let (owner, name) = owner_and_name(path, false).ok_or_else(bad)?;
        return Ok(format!(
            "https://api.github.com/repos/{owner}/{name}/releases"
        ));
    }
    if let Some(idx) = path
        .find("repos/")
        .filter(|&i| i == 0 || path[..i].ends_with('/'))
    {
        // An API URL: keep the host and prefix, normalize the tail to `/releases`.
        let (owner, name) = owner_and_name(&path[idx + "repos/".len()..], false).ok_or_else(bad)?;
        return Ok(format!(
            "{scheme}://{host}/{}repos/{owner}/{name}/releases",
            &path[..idx]
        ));
    }
    let (owner, name) = owner_and_name(path, false).ok_or_else(bad)?;
    Ok(format!(
        "{scheme}://{host}/api/v3/repos/{owner}/{name}/releases"
    ))
}

/// The web page listing the releases ("Release Notes" links, and the download fallback).
pub fn releases_page_url(setting: &str) -> String {
    let api = releases_api_url(setting)
        .or_else(|_| releases_api_url(""))
        .unwrap_or_default();
    if let Some(rest) = api.strip_prefix("https://api.github.com/repos/") {
        return format!("https://github.com/{rest}");
    }
    match api.split_once("/api/v3/repos/") {
        Some((base, rest)) => format!("{base}/{rest}"),
        None => api,
    }
}

/// Maps an HTTP error status and GitHub's `x-ratelimit-remaining` / `x-ratelimit-reset`
/// headers to a [`FetchError`].
pub fn classify_status(
    status: u16,
    ratelimit_remaining: Option<&str>,
    ratelimit_reset: Option<&str>,
) -> FetchError {
    match status {
        404 | 410 => FetchError::NotFound,
        401 | 403 | 429 => FetchError::Refused {
            status,
            rate_limited: status == 429 || ratelimit_remaining.map(str::trim) == Some("0"),
            reset_at: ratelimit_reset.and_then(|r| r.trim().parse().ok()),
        },
        _ => FetchError::Status(status),
    }
}

fn agent(url: &str, timeout: Duration) -> ureq::Agent {
    rustpy_net::agent_builder(url)
        // Redirect hops must choose the proxy and TLS policy for their own URL.
        .redirects(0)
        .timeout_connect(timeout.min(Duration::from_secs(5)))
        .timeout(timeout)
        .user_agent(concat!(
            "RustPy/",
            env!("CARGO_PKG_VERSION"),
            " (update check; +https://github.com/themuddfamily/rustpy)"
        ))
        .build()
}

fn request_page(
    start: &url::Url,
    deadline: Instant,
) -> Result<(ureq::Response, url::Url), FetchError> {
    let mut url = start.clone();
    let mut visited = std::collections::HashSet::new();
    for _ in 0..=5 {
        if !visited.insert(url.clone()) {
            return Err(FetchError::Invalid("release redirect repeats a URL".into()));
        }
        let timeout = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or_else(|| FetchError::Offline("release pagination timed out".into()))?
            .min(Duration::from_secs(15));
        let resp = match agent(url.as_str(), timeout)
            .get(url.as_str())
            .set("Accept", "application/vnd.github+json")
            .set("X-GitHub-Api-Version", "2022-11-28")
            .call()
        {
            Ok(resp) => resp,
            Err(ureq::Error::Status(code, resp)) => {
                return Err(classify_status(
                    code,
                    resp.header("x-ratelimit-remaining"),
                    resp.header("x-ratelimit-reset"),
                ))
            }
            Err(ureq::Error::Transport(t)) => return Err(FetchError::Offline(t.to_string())),
        };
        if matches!(resp.status(), 301 | 302 | 303 | 307 | 308) {
            let target = resp
                .header("Location")
                .and_then(|location| url.join(location).ok())
                .ok_or_else(|| FetchError::Invalid("invalid release redirect".into()))?;
            if !matches!(target.scheme(), "http" | "https")
                || target.host_str().is_none()
                || !target.username().is_empty()
                || target.password().is_some()
                || (url.scheme() == "https" && target.scheme() == "http")
            {
                return Err(FetchError::Invalid("invalid release redirect".into()));
            }
            url = target;
            url.set_fragment(None);
        } else if (200..300).contains(&resp.status()) {
            return Ok((resp, url));
        } else {
            return Err(classify_status(
                resp.status(),
                resp.header("x-ratelimit-remaining"),
                resp.header("x-ratelimit-reset"),
            ));
        }
    }
    Err(FetchError::Invalid(
        "release redirects exceed the limit".into(),
    ))
}

/// GitHub's Link header can contain quoted commas and commas inside a URL.
/// Refuse malformed pagination rather than reporting a truncated release list.
fn link_parts(header: &str, separator: char) -> Result<Vec<&str>, FetchError> {
    let invalid = || FetchError::Invalid("invalid release pagination".into());
    let mut angle = false;
    let mut quote = false;
    let mut escaped = false;
    let mut start = 0;
    let mut parts = Vec::new();
    for (i, c) in header.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if quote && c == '\\' {
            escaped = true;
            continue;
        }
        match c {
            '"' if !angle => quote = !quote,
            '<' if !quote => angle = true,
            '>' if !quote => angle = false,
            c if c == separator && !angle && !quote => {
                parts.push(&header[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if angle || quote || escaped {
        return Err(invalid());
    }
    parts.push(&header[start..]);
    Ok(parts)
}

fn next_page(
    header: &str,
    current: &url::Url,
    origin: &url::Url,
) -> Result<Option<url::Url>, FetchError> {
    let invalid = || FetchError::Invalid("invalid release pagination".into());
    let mut next = None;
    for part in link_parts(header, ',')? {
        let (target, params) = part
            .trim()
            .strip_prefix('<')
            .and_then(|p| p.split_once('>'))
            .ok_or_else(invalid)?;
        if !params.trim().is_empty() && !params.trim().starts_with(';') {
            return Err(invalid());
        }
        let is_next = link_parts(params, ';')?.into_iter().skip(1).any(|param| {
            param.trim().split_once('=').is_some_and(|(key, value)| {
                key.trim().eq_ignore_ascii_case("rel")
                    && value
                        .trim()
                        .trim_matches('"')
                        .split_whitespace()
                        .any(|rel| rel.eq_ignore_ascii_case("next"))
            })
        });
        if !is_next {
            continue;
        }
        let mut target = current.join(target).map_err(|_| invalid())?;
        target.set_fragment(None);
        if target.origin() != origin.origin()
            || !target.username().is_empty()
            || target.password().is_some()
            || next.is_some()
        {
            return Err(invalid());
        }
        next = Some(target);
    }
    Ok(next)
}

/// Reads every release page from `api_url` (see [`releases_api_url`]). Call it
/// from a background thread. Requests share a one-minute timeout budget (DNS
/// and interactive certificate prompts may outlast it), at most 100 pages and
/// 64 MiB of release JSON; incomplete lists are never returned.
pub fn fetch_releases(api_url: &str) -> Result<Vec<Release>, FetchError> {
    let url = if api_url.contains('?') {
        api_url.to_string()
    } else {
        format!("{api_url}?per_page=50")
    };
    let mut origin = url::Url::parse(&url).map_err(|_| FetchError::BadSetting(api_url.into()))?;
    if !matches!(origin.scheme(), "http" | "https")
        || origin.host_str().is_none()
        || !origin.username().is_empty()
        || origin.password().is_some()
    {
        return Err(FetchError::BadSetting(api_url.into()));
    }
    let mut url = origin.clone();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut visited = std::collections::HashSet::new();
    let mut releases = Vec::new();
    let mut remaining = 64 * 1024 * 1024u64;
    for page in 0..100 {
        if !visited.insert(url.clone()) {
            return Err(FetchError::Invalid(
                "release pagination repeats a page".into(),
            ));
        }
        let (resp, current) = request_page(&url, deadline)?;
        // A repository can move, or an HTTP Enterprise endpoint can redirect
        // to HTTPS. Pagination stays on the first actual response's origin.
        if page == 0 {
            origin = current.clone();
        }
        let links = resp.all("Link");
        let next = if links.is_empty() {
            None
        } else {
            next_page(&links.join(","), &current, &origin)?
        };
        let mut text = String::new();
        let limit = remaining.min(16 * 1024 * 1024);
        resp.into_reader()
            .take(limit + 1)
            .read_to_string(&mut text)
            .map_err(|e| FetchError::Offline(e.to_string()))?;
        if text.len() as u64 > limit {
            return Err(FetchError::Invalid(
                "release list exceeds the size limit".into(),
            ));
        }
        remaining -= text.len() as u64;
        releases.extend(parse_releases(&text).map_err(FetchError::Invalid)?);
        match next {
            Some(next) => url = next,
            None => return Ok(releases),
        }
    }
    Err(FetchError::Invalid(
        "release pagination exceeds the page limit".into(),
    ))
}

/// A stand-in for [`fetch_releases`] in headless test runs: `spec` is the path of a
/// Releases API JSON file, or `offline`, `http:<status>` or `ratelimit` to act out a failure.
pub fn fetch_from_test_feed(spec: &str) -> Result<Vec<Release>, FetchError> {
    let spec = spec.trim();
    if spec == "offline" {
        return Err(FetchError::Offline("simulated".into()));
    }
    if spec == "ratelimit" {
        return Err(classify_status(403, Some("0"), None));
    }
    if let Some(code) = spec
        .strip_prefix("http:")
        .and_then(|c| c.parse::<u16>().ok())
    {
        return Err(classify_status(code, None, None));
    }
    let text =
        std::fs::read_to_string(spec).map_err(|e| FetchError::Offline(format!("{spec}: {e}")))?;
    parse_releases(&text).map_err(FetchError::Invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, Write};
    use std::net::TcpListener;

    // rustpy-net's configuration is process-wide; these local network tests
    // serialize changes and restore it even if an assertion fails.
    static NETWORK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    struct RestoreNetwork(std::sync::Arc<rustpy_net::NetConfig>);
    impl Drop for RestoreNetwork {
        fn drop(&mut self) {
            rustpy_net::configure((*self.0).clone());
        }
    }
    fn direct_network() -> (std::sync::MutexGuard<'static, ()>, RestoreNetwork) {
        let guard = NETWORK.lock().unwrap_or_else(|e| e.into_inner());
        let restore = RestoreNetwork(rustpy_net::config());
        rustpy_net::configure(rustpy_net::NetConfig {
            proxy: rustpy_net::ProxySettings {
                mode: rustpy_net::ProxyMode::NoProxy,
                ..Default::default()
            },
            ..Default::default()
        });
        (guard, restore)
    }
    fn local_feed(
        responses: Vec<(u16, String, String)>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let host = base.clone();
        listener.set_nonblocking(true).unwrap();
        let worker = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, headers, body) in responses {
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut connection = loop {
                    match listener.accept() {
                        Ok((connection, _)) => break connection,
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            std::thread::sleep(Duration::from_millis(5))
                        }
                        Err(e) => panic!("local release server did not receive a request: {e}"),
                    }
                };
                connection
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(connection.try_clone().unwrap());
                let mut request = String::new();
                loop {
                    let mut line = String::new();
                    let n = reader.read_line(&mut line).unwrap();
                    assert!(n > 0 && request.len() < 16 * 1024);
                    request.push_str(&line);
                    if line == "\r\n" {
                        break;
                    }
                }
                requests.push(request);
                write!(connection, "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{body}", body.len(), headers.replace("$BASE", &host)).unwrap();
            }
            requests
        });
        (base, worker)
    }

    #[test]
    fn reads_all_release_pages_through_the_configured_authenticated_proxy() {
        let (_guard, _restore) = direct_network();
        let early: Vec<_> = (1..=50).map(|n| serde_json::json!({ "tag_name": format!("v2.0.0-eap.{n}"), "prerelease": true })).collect();
        let (proxy, worker) = local_feed(vec![
            (200, "Link: </repos/acme/ide/releases?per_page=50>; rel=\"prev\"\r\nLink: </repos/acme/ide/releases?per_page=50&page=2>; rel=\"next\"\r\n".into(), serde_json::to_string(&early).unwrap()),
            (200, String::new(), r#"[{"tag_name":"v1.9.0"}]"#.into()),
        ]);
        let proxy = url::Url::parse(&proxy).unwrap();
        rustpy_net::configure(rustpy_net::NetConfig {
            proxy: rustpy_net::ProxySettings {
                mode: rustpy_net::ProxyMode::Manual,
                kind: rustpy_net::ProxyKind::Http,
                host: "127.0.0.1".into(),
                port: proxy.port().unwrap(),
                authenticate: true,
                login: "me".into(),
                password: "pw".into(),
                no_proxy: String::new(),
                ..Default::default()
            },
            ..Default::default()
        });
        // This name cannot resolve without the configured proxy. Page two
        // must use that same shared setup, including authentication.
        let releases = fetch_releases("http://releases.invalid/repos/acme/ide/releases").unwrap();
        assert_eq!(releases.len(), 51);
        assert_eq!(
            crate::newest(&releases, crate::Channel::Stable)
                .unwrap()
                .tag,
            "v1.9.0"
        );
        let requests = worker.join().unwrap();
        assert!(requests[0].starts_with(
            "GET http://releases.invalid/repos/acme/ide/releases?per_page=50 HTTP/1.1\r\n"
        ));
        assert!(requests[1].starts_with(
            "GET http://releases.invalid/repos/acme/ide/releases?per_page=50&page=2 HTTP/1.1\r\n"
        ));
        for request in requests {
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("proxy-authorization: basic bwu6chc=\r\n"),
                "{request}"
            );
        }
    }

    #[test]
    fn later_page_failures_and_pagination_loops_never_return_a_partial_list() {
        let (_guard, _restore) = direct_network();
        for (status, body, expected) in [(503, "[]", "status"), (200, "not JSON", "invalid")] {
            let (base, worker) = local_feed(vec![
                (
                    200,
                    "Link: <$BASE/repos/a/b/releases?page=2>; rel=\"next\"\r\n".into(),
                    r#"[{"tag_name":"v1.0.0"}]"#.into(),
                ),
                (status, String::new(), body.into()),
            ]);
            let error = fetch_releases(&format!("{base}/repos/a/b/releases")).unwrap_err();
            assert!(
                match expected {
                    "status" => error == FetchError::Status(503),
                    _ => matches!(error, FetchError::Invalid(_)),
                },
                "{error:?}"
            );
            assert_eq!(worker.join().unwrap().len(), 2);
        }
        let (base, worker) = local_feed(vec![(
            200,
            "Link: <$BASE/repos/a/b/releases?per_page=50>; rel=\"next\"\r\n".into(),
            "[]".into(),
        )]);
        assert!(matches!(
            fetch_releases(&format!("{base}/repos/a/b/releases")),
            Err(FetchError::Invalid(_))
        ));
        assert_eq!(worker.join().unwrap().len(), 1);
        let (base, worker) = local_feed(
            (1..=100)
                .map(|page| {
                    (
                        200,
                        format!(
                            "Link: </repos/a/b/releases?page={}>; rel=\"next\"\r\n",
                            page + 1
                        ),
                        "[]".into(),
                    )
                })
                .collect(),
        );
        assert!(matches!(
            fetch_releases(&format!("{base}/repos/a/b/releases")),
            Err(FetchError::Invalid(_))
        ));
        assert_eq!(worker.join().unwrap().len(), 100);
    }

    #[test]
    fn redirects_reapply_proxy_exclusions_and_paginate_from_the_final_endpoint() {
        let (_guard, _restore) = direct_network();
        let (origin, origin_worker) = local_feed(vec![
            (
                200,
                "Link: </renamed/releases?page=2>; rel=\"next\"\r\n".into(),
                r#"[{"tag_name":"v2.0.0-eap.1","prerelease":true}]"#.into(),
            ),
            (200, String::new(), r#"[{"tag_name":"v1.9.0"}]"#.into()),
        ]);
        let (proxy, proxy_worker) = local_feed(vec![(
            302,
            format!("Location: {origin}/renamed/releases\r\n"),
            String::new(),
        )]);
        let proxy = url::Url::parse(&proxy).unwrap();
        rustpy_net::configure(rustpy_net::NetConfig {
            proxy: rustpy_net::ProxySettings {
                mode: rustpy_net::ProxyMode::Manual,
                kind: rustpy_net::ProxyKind::Http,
                host: "127.0.0.1".into(),
                port: proxy.port().unwrap(),
                authenticate: true,
                login: "me".into(),
                password: "pw".into(),
                no_proxy: "127.0.0.1".into(),
                ..Default::default()
            },
            ..Default::default()
        });
        let releases = fetch_releases("http://releases.invalid/repos/acme/ide/releases").unwrap();
        assert_eq!(
            crate::newest(&releases, crate::Channel::Stable)
                .unwrap()
                .tag,
            "v1.9.0"
        );
        assert_eq!(proxy_worker.join().unwrap().len(), 1);
        let requests = origin_worker.join().unwrap();
        assert!(requests[0].starts_with("GET /renamed/releases HTTP/1.1\r\n"));
        assert!(requests[1].starts_with("GET /renamed/releases?page=2 HTTP/1.1\r\n"));
        assert!(requests
            .iter()
            .all(|r| !r.to_ascii_lowercase().contains("proxy-authorization")));
    }

    #[test]
    fn redirect_loops_and_invalid_locations_are_refused() {
        let (_guard, _restore) = direct_network();
        for headers in [
            "Location: $BASE/repos/a/b/releases?per_page=50\r\n",
            "Location: ftp://example.invalid/releases\r\n",
            "Location: http://me:pw@example.invalid/releases\r\n",
            "",
        ] {
            let (base, worker) = local_feed(vec![(302, headers.into(), String::new())]);
            assert!(matches!(
                fetch_releases(&format!("{base}/repos/a/b/releases")),
                Err(FetchError::Invalid(_))
            ));
            assert_eq!(worker.join().unwrap().len(), 1);
        }
        let (base, worker) = local_feed(
            (1..=6)
                .map(|page| {
                    (
                        302,
                        format!("Location: /repos/a/b/releases?redirect={}\r\n", page + 1),
                        String::new(),
                    )
                })
                .collect(),
        );
        assert!(matches!(
            fetch_releases(&format!("{base}/repos/a/b/releases")),
            Err(FetchError::Invalid(_))
        ));
        assert_eq!(worker.join().unwrap().len(), 6);
    }

    #[test]
    fn pagination_relations_are_parsed_without_following_another_origin() {
        let current = url::Url::parse("http://localhost:8080/repos/a/b/releases?page=1").unwrap();
        let next = next_page(r#"</repos/a/b/releases?page=2&q=a,b>; title="pages, more"; rel="prev next", </repos/a/b/releases?page=3>; rel="last""#, &current, &current).unwrap().unwrap();
        assert_eq!(
            next.as_str(),
            "http://localhost:8080/repos/a/b/releases?page=2&q=a,b"
        );
        assert!(next_page(
            r#"</repos/a/b/releases?page=1>; rel="prev""#,
            &current,
            &current
        )
        .unwrap()
        .is_none());
        assert!(next_page(
            r#"</repos/a/b/releases?page=1>; title="text; rel=next"; rel="prev""#,
            &current,
            &current
        )
        .unwrap()
        .is_none());
        for invalid in [
            r#"<http://elsewhere.invalid/releases>; rel="next""#,
            r#"<https://localhost:8080/releases>; rel="next""#,
            r#"<http://me:pw@localhost:8080/releases>; rel="next""#,
            r#"</releases?page=2>; rel="next", </releases?page=3>; rel="next""#,
            r#"</releases?page=2>; rel="next"#,
            "broken",
        ] {
            assert!(
                matches!(
                    next_page(invalid, &current, &current),
                    Err(FetchError::Invalid(_))
                ),
                "{invalid}"
            );
        }
    }

    #[test]
    fn release_urls_from_the_setting() {
        let api = |s: &str| releases_api_url(s).unwrap_or_else(|e| panic!("{s:?}: {e:?}"));
        assert_eq!(
            api(""),
            "https://api.github.com/repos/themuddfamily/rustpy/releases"
        );
        assert_eq!(
            api(" acme/ide "),
            "https://api.github.com/repos/acme/ide/releases"
        );
        assert_eq!(
            api("https://github.com/acme/ide"),
            "https://api.github.com/repos/acme/ide/releases"
        );
        assert_eq!(
            api("https://github.com/acme/ide.git"),
            "https://api.github.com/repos/acme/ide/releases"
        );
        assert_eq!(
            api("https://github.com/acme/ide/releases/tag/v1.0.0"),
            "https://api.github.com/repos/acme/ide/releases"
        );
        assert_eq!(
            api("https://api.github.com/repos/acme/ide/releases"),
            "https://api.github.com/repos/acme/ide/releases"
        );
        assert_eq!(
            api("https://api.github.com/repos/acme/ide"),
            "https://api.github.com/repos/acme/ide/releases"
        );
        assert_eq!(
            api("https://ghe.example.com/api/v3/repos/team/ide/releases?per_page=5"),
            "https://ghe.example.com/api/v3/repos/team/ide/releases"
        );
        assert_eq!(
            api("https://ghe.example.com/team/ide"),
            "https://ghe.example.com/api/v3/repos/team/ide/releases"
        );
        assert_eq!(
            api("http://localhost:8080/repos/a/b/releases"),
            "http://localhost:8080/repos/a/b/releases"
        );
        for bad in [
            "acme",
            "acme/ide/extra",
            "ftp://github.com/a/b",
            "https://github.com/acme",
            "https:///a/b",
            "a b/c",
            "../x",
        ] {
            assert!(
                matches!(releases_api_url(bad), Err(FetchError::BadSetting(_))),
                "{bad:?} accepted"
            );
        }
        assert_eq!(
            releases_page_url(""),
            "https://github.com/themuddfamily/rustpy/releases"
        );
        assert_eq!(
            releases_page_url("https://ghe.example.com/team/ide"),
            "https://ghe.example.com/team/ide/releases"
        );
        assert_eq!(
            releases_page_url("not a repo"),
            "https://github.com/themuddfamily/rustpy/releases"
        );
    }

    #[test]
    fn classifies_http_failures_calmly() {
        assert_eq!(classify_status(404, None, None), FetchError::NotFound);
        assert_eq!(
            classify_status(403, Some("0"), Some("1791623831")),
            FetchError::Refused {
                status: 403,
                rate_limited: true,
                reset_at: Some(1_791_623_831)
            }
        );
        assert_eq!(
            classify_status(403, Some("12"), None),
            FetchError::Refused {
                status: 403,
                rate_limited: false,
                reset_at: None
            }
        );
        assert_eq!(
            classify_status(429, None, None),
            FetchError::Refused {
                status: 429,
                rate_limited: true,
                reset_at: None
            }
        );
        assert_eq!(classify_status(502, None, None), FetchError::Status(502));
        assert!(FetchError::NotFound.reached_server());
        assert!(!FetchError::Offline("dns".into()).reached_server());
        for e in [
            FetchError::NotFound,
            classify_status(403, Some("0"), None),
            classify_status(401, None, None),
            FetchError::Status(500),
            FetchError::Offline("x".into()),
            FetchError::Invalid("x".into()),
            FetchError::BadSetting("x".into()),
        ] {
            let m = e.message();
            assert!(!m.is_empty() && !m.to_lowercase().contains("error"), "{m}");
        }
    }

    #[test]
    fn test_feed_acts_out_failures_and_reads_files() {
        assert!(!fetch_from_test_feed("offline")
            .unwrap_err()
            .reached_server());
        assert_eq!(fetch_from_test_feed("http:404"), Err(FetchError::NotFound));
        assert!(matches!(
            fetch_from_test_feed("ratelimit"),
            Err(FetchError::Refused {
                rate_limited: true,
                ..
            })
        ));
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/github-releases.json"
        );
        assert!(!fetch_from_test_feed(fixture).unwrap().is_empty());
        assert!(matches!(
            fetch_from_test_feed("/nonexistent/releases.json"),
            Err(FetchError::Offline(_))
        ));
    }

    /// Needs network access: `cargo test -p rustpy-updates -- --ignored`.
    #[test]
    #[ignore]
    fn reads_a_public_release_list() {
        match fetch_releases(&releases_api_url("BurntSushi/ripgrep").unwrap()) {
            Ok(list) => assert!(
                list.iter()
                    .any(|r| r.version >= crate::Version::parse("14.0.0").unwrap()
                        && !r.assets.is_empty()),
                "{list:?}"
            ),
            // The anonymous rate limit may be used up on shared networks.
            Err(e) => assert!(matches!(e, FetchError::Refused { .. }), "{e:?}"),
        }
    }
}
