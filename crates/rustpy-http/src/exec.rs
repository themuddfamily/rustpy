//! Request execution with `ureq` (blocking, or on a background thread).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use serde::{Deserialize, Serialize};

use crate::parser::HttpRequest;

/// Maximum response body kept in memory.
const MAX_BODY: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpResponse {
    /// `0` when the request could not be sent (see `error`).
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    /// Response body as text; JSON bodies are pretty-printed.
    pub body: String,
    pub content_type: String,
    pub elapsed: Duration,
    /// Body size in bytes (before pretty-printing).
    pub size: usize,
    /// Transport / setup error (connection refused, DNS, missing body file, …).
    pub error: Option<String>,
    pub bytes: Vec<u8>,
    pub tests: Vec<crate::scripts::ScriptTest>,
    pub logs: Vec<String>,
}

impl HttpResponse {
    pub fn is_success(&self) -> bool {
        self.error.is_none() && (200..300).contains(&self.status)
    }

    /// `HTTP/1.1 200 OK` style status line.
    pub fn status_line(&self) -> String {
        match &self.error {
            Some(e) => format!("Error: {e}"),
            None => format!("{} {}", self.status, self.status_text),
        }
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

fn error_response(error: String, elapsed: Duration) -> HttpResponse {
    HttpResponse {
        error: Some(error),
        elapsed,
        ..Default::default()
    }
}

fn normalize_url(url: &str) -> String {
    let u = url.trim();
    if u.contains("://") {
        u.to_string()
    } else {
        format!("http://{u}")
    }
}

/// Executes `req` (variables must already be resolved) and returns the response.
/// Body files are resolved relative to `base_dir`. Never panics; failures are
/// reported through [`HttpResponse::error`].
pub fn execute(req: &HttpRequest, base_dir: &Path, timeout: Duration) -> HttpResponse {
    let mut options = crate::runtime::Options::new(base_dir.into());
    options.timeout = timeout;
    crate::runtime::execute(req.clone(), options, &crate::runtime::Control::default()).response
}
type CachedAgent = (
    PathBuf,
    rustpy_net::NetConfig,
    u64,
    u64,
    String,
    ureq::Agent,
);
static AGENTS: std::sync::Mutex<Vec<CachedAgent>> = std::sync::Mutex::new(Vec::new());
fn agent(req: &HttpRequest, options: &crate::runtime::Options, url: &str) -> ureq::Agent {
    let timeout = req
        .timeout_ms
        .unwrap_or(options.timeout.as_millis() as u64)
        .clamp(1, 300_000);
    let connect = req.connection_timeout_ms.unwrap_or(5000).clamp(1, 60_000);
    let cfg = rustpy_net::config();
    let origin = url::Url::parse(url)
        .map(|url| url.origin().ascii_serialization())
        .unwrap_or_else(|_| url.into());
    let build = || {
        rustpy_net::agent_builder_with(&cfg, url)
            .timeout(Duration::from_millis(timeout))
            .timeout_connect(Duration::from_millis(connect))
            .redirects(0)
            .build()
    };
    if req.no_cookie_jar {
        return build();
    }
    let mut agents = AGENTS.lock().unwrap_or_else(|error| error.into_inner());
    if let Some((_, _, _, _, _, agent)) =
        agents
            .iter()
            .find(|(root, config, total, connection, origin_key, _)| {
                root == &options.project
                    && config == cfg.as_ref()
                    && *total == timeout
                    && *connection == connect
                    && origin_key == &origin
            })
    {
        return agent.clone();
    }
    let agent = build();
    if agents.len() >= 16 {
        agents.remove(0);
    }
    agents.push((
        options.project.clone(),
        cfg.as_ref().clone(),
        timeout,
        connect,
        origin,
        agent.clone(),
    ));
    agent
}
pub(crate) fn execute_transport(
    req: &HttpRequest,
    options: &crate::runtime::Options,
    control: &crate::runtime::Control,
) -> HttpResponse {
    let started = Instant::now();
    let url = normalize_url(&req.url);
    let parsed = match url::Url::parse(&url) {
        Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => parsed,
        _ => {
            return error_response(
                "Only HTTP(S) URLs are valid for HTTP transport".into(),
                started.elapsed(),
            )
        }
    };
    if parsed.host_str().is_none() {
        return error_response("Request has no host".into(), started.elapsed());
    }
    let body = req
        .binary_body
        .clone()
        .or_else(|| req.body.as_ref().map(|body| body.as_bytes().to_vec()));
    let mut current = parsed;
    let mut method = req.method.clone();
    let mut body = body;
    let mut headers = req.headers.clone();
    let mut redirects = 0;
    let budget = Duration::from_millis(
        req.timeout_ms
            .unwrap_or(options.timeout.as_millis() as u64)
            .clamp(1, 300_000),
    );
    let response = loop {
        let agent = agent(req, options, current.as_str());
        let Some(remaining) = budget
            .checked_sub(started.elapsed())
            .filter(|duration| !duration.is_zero())
        else {
            return error_response("Request timed out".into(), started.elapsed());
        };
        let mut request = agent.request(&method, current.as_str()).timeout(remaining);
        for (key, value) in &headers {
            request = request.set(key, value);
        }
        if control.cancelled() {
            return error_response("Request cancelled".into(), started.elapsed());
        }
        let response = match if let Some(bytes) = &body {
            request.send_bytes(bytes)
        } else {
            request.call()
        } {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(ureq::Error::Transport(error)) => {
                return error_response(
                    rustpy_net::describe_transport_error(&error),
                    started.elapsed(),
                )
            }
        };
        if req.no_redirect || !matches!(response.status(), 301 | 302 | 303 | 307 | 308) {
            break response;
        }
        let Some(location) = response.header("location") else {
            break response;
        };
        if redirects >= 10 {
            return error_response("Too many redirects".into(), started.elapsed());
        }
        let next = match current.join(location) {
            Ok(next)
                if matches!(next.scheme(), "http" | "https")
                    && next.username().is_empty()
                    && next.password().is_none() =>
            {
                next
            }
            _ => return error_response("Invalid redirect target".into(), started.elapsed()),
        };
        if next.origin() != current.origin() {
            headers.retain(|(name, _)| {
                ![
                    "authorization",
                    "proxy-authorization",
                    "cookie",
                    "host",
                    "x-amz-security-token",
                    "x-amz-date",
                    "x-amz-content-sha256",
                ]
                .iter()
                .any(|sensitive| name.eq_ignore_ascii_case(sensitive))
            });
        }
        if response.status() == 303 && method != "HEAD"
            || matches!(response.status(), 301 | 302) && method == "POST"
        {
            method = "GET".into();
            body = None;
            headers.retain(|(name, _)| {
                !name.eq_ignore_ascii_case("content-type")
                    && !name.eq_ignore_ascii_case("content-length")
            });
        }
        current = next;
        redirects += 1;
    };
    let status = response.status();
    let status_text = response.status_text().into();
    let content_type = response.content_type().to_string();
    let headers = response
        .headers_names()
        .into_iter()
        .map(|key| {
            let value = response.all(&key).join(", ");
            (key, value)
        })
        .collect();
    control.phase.store(3, std::sync::atomic::Ordering::Relaxed);
    let mut reader = response.into_reader();
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 16 * 1024];
    let mut error = None;
    loop {
        if control.cancelled() {
            error = Some("Request cancelled".into());
            break;
        }
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                if bytes.len() + count > MAX_BODY as usize {
                    error = Some("Response exceeds 16 MiB".into());
                    break;
                }
                bytes.extend_from_slice(&buffer[..count]);
                control
                    .received
                    .store(bytes.len(), std::sync::atomic::Ordering::Relaxed);
            }
            Err(cause) => {
                error = Some(format!("Error reading response: {cause}"));
                break;
            }
        }
    }
    let size = bytes.len();
    let mut body = String::from_utf8_lossy(&bytes).into_owned();
    if content_type.contains("json") {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
            body = serde_json::to_string_pretty(&value).unwrap_or(body);
        }
    } else if content_type.contains("xml") || content_type.contains("html") {
        body = crate::workspace::format_markup(&body);
    }
    HttpResponse {
        status,
        status_text,
        content_type,
        headers,
        body,
        bytes,
        size,
        elapsed: started.elapsed(),
        error,
        ..Default::default()
    }
}

/// Runs [`execute`] on a background thread; the receiver yields exactly one response.
pub fn execute_async(
    req: HttpRequest,
    base_dir: PathBuf,
    timeout: Duration,
) -> Receiver<HttpResponse> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    std::thread::spawn(move || {
        let resp = execute(&req, &base_dir, timeout);
        let _ = tx.send(resp);
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_body_file_is_an_error() {
        let req = HttpRequest {
            method: "POST".into(),
            url: "http://127.0.0.1:9/x".into(),
            body_file: Some(PathBuf::from("nope.json")),
            ..Default::default()
        };
        let resp = execute(&req, Path::new("/tmp"), Duration::from_secs(1));
        assert!(resp.error.as_deref().unwrap_or("").contains("nope.json"));
        assert_eq!(resp.status, 0);
    }

    #[test]
    fn connection_refused_is_reported_not_panicked() {
        let req = HttpRequest {
            method: "GET".into(),
            url: "127.0.0.1:9".into(),
            ..Default::default()
        };
        let resp = execute(&req, Path::new("/tmp"), Duration::from_secs(2));
        assert!(resp.error.is_some());
        assert!(resp.status_line().starts_with("Error:"));
    }

    /// Serializes the tests that change or depend on the process-wide network settings.
    static NET: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn requests_use_the_configured_proxy() {
        use std::io::Write as _;
        let _g = NET.lock().unwrap_or_else(|e| e.into_inner());
        // A fake HTTP proxy: answers the (absolute-form) request itself and records it.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut c, _) = listener.accept().unwrap();
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && c.read(&mut b).unwrap_or(0) == 1 {
                head.push(b[0]);
            }
            let _ = tx.send(
                String::from_utf8_lossy(&head)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string(),
            );
            let _ = c.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nproxy",
            );
        });
        rustpy_net::configure(rustpy_net::NetConfig {
            proxy: rustpy_net::ProxySettings {
                mode: rustpy_net::ProxyMode::Manual,
                host: "127.0.0.1".into(),
                port,
                ..Default::default()
            },
            ..Default::default()
        });
        // Only the proxy can answer for `*.test`.
        let req = HttpRequest {
            method: "GET".into(),
            url: "http://api.proxied.test/items".into(),
            ..Default::default()
        };
        let resp = execute(&req, Path::new("/tmp"), Duration::from_secs(10));
        rustpy_net::configure(rustpy_net::NetConfig::default());
        assert_eq!(resp.body, "proxy", "{:?}", resp.error);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            "GET http://api.proxied.test/items HTTP/1.1"
        );
    }

    #[test]
    fn network_example_com() {
        let _g = NET.lock().unwrap_or_else(|e| e.into_inner());
        let req = HttpRequest {
            method: "GET".into(),
            url: "https://example.com".into(),
            ..Default::default()
        };
        let rx = execute_async(req, PathBuf::from("/tmp"), Duration::from_secs(10));
        let resp = rx
            .recv_timeout(Duration::from_secs(15))
            .expect("response within timeout");
        if let Some(e) = &resp.error {
            eprintln!("network test skipped: {e}");
            return;
        }
        assert_eq!(resp.status, 200, "{}", resp.status_line());
        assert!(resp.size > 0);
        assert!(resp.header("content-type").is_some());
        assert!(resp.is_success());
    }
}
