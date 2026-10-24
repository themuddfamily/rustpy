use rustpy_core::CancellationToken;
use rustpy_settings::github::{Account, Provider};
use serde_json::Value;
use std::{io::Read, time::Duration};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    pub web: String,
    pub api: String,
    pub provider: Provider,
}
impl Server {
    pub fn new(input: &str, provider: Provider) -> Result<Self, String> {
        let parsed = url::Url::parse(input.trim()).map_err(|error| error.to_string())?;
        if parsed.scheme() != "https"
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || !matches!(parsed.path(), "" | "/")
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err("Enter an HTTPS server origin without credentials, path or query".into());
        }
        let web = parsed.origin().ascii_serialization();
        let authority = web.strip_prefix("https://").unwrap_or_default();
        let api = match provider {
            Provider::GitHub if authority.eq_ignore_ascii_case("github.com") => {
                "https://api.github.com".into()
            }
            Provider::GitHub => format!("{web}/api/v3"),
            Provider::GitLab => format!("{web}/api/v4"),
        };
        Ok(Self { web, api, provider })
    }
    pub fn api_url(&self, path: &str) -> Result<String, String> {
        if !path.starts_with('/')
            || path.starts_with("//")
            || path.contains(['\\', '\r', '\n'])
            || path
                .split('?')
                .next()
                .unwrap_or_default()
                .split('/')
                .any(|part| part == ".." || part == ".")
        {
            return Err("Invalid API path".into());
        }
        Ok(format!("{}{path}", self.api))
    }
    pub fn remote_repository(&self, url: &str) -> Option<String> {
        let authority = self.web.strip_prefix("https://")?;
        let relative = url
            .strip_prefix(&format!("{}/", self.web))
            .or_else(|| url.strip_prefix(&format!("git@{authority}:")))
            .or_else(|| url.strip_prefix(&format!("ssh://git@{authority}/")))?
            .trim_end_matches('/')
            .trim_end_matches(".git");
        if relative.is_empty()
            || relative.split('/').count() < 2
            || relative
                .split('/')
                .any(|part| part.is_empty() || part == ".." || part == ".")
            || relative.contains(['?', '#', '\\'])
        {
            return None;
        }
        Some(relative.into())
    }
    pub fn file_url(
        &self,
        repository: &str,
        revision: &str,
        path: &str,
        line: Option<usize>,
        last: Option<usize>,
    ) -> String {
        let path = path.split('/').map(encode).collect::<Vec<_>>().join("/");
        let mut url = format!(
            "{}/{}/{}blob/{}/{path}",
            self.web,
            repository,
            if self.provider == Provider::GitLab {
                "-/"
            } else {
                ""
            },
            encode(revision)
        );
        if let Some(line) = line {
            url.push_str(&format!("#L{line}"));
            if let Some(last) = last.filter(|last| *last > line) {
                url.push_str(&format!("-L{last}"));
            }
        }
        url
    }
}
pub fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}
#[derive(Clone, Debug)]
pub struct Request {
    pub method: &'static str,
    pub path: String,
    pub body: Option<Value>,
}
impl Request {
    pub fn read(path: impl Into<String>) -> Self {
        Self {
            method: "GET",
            path: path.into(),
            body: None,
        }
    }
    pub fn write(method: &'static str, path: impl Into<String>, body: Value) -> Self {
        Self {
            method,
            path: path.into(),
            body: Some(body),
        }
    }
    pub fn is_write(&self) -> bool {
        self.method != "GET"
    }
}
#[derive(Clone, Debug, Default)]
pub struct Response {
    pub value: Value,
    pub next_page: Option<usize>,
    pub scopes: Vec<String>,
}
pub trait Transport {
    fn send(
        &self,
        server: &Server,
        token: &str,
        request: &Request,
        cancel: &CancellationToken,
    ) -> Result<Response, String>;
}
pub struct HttpTransport;
impl Transport for HttpTransport {
    fn send(
        &self,
        server: &Server,
        token: &str,
        request: &Request,
        cancel: &CancellationToken,
    ) -> Result<Response, String> {
        cancel.check().map_err(|error| error.to_string())?;
        let url = server.api_url(&request.path)?;
        let agent = rustpy_net::agent_builder(&url)
            .redirects(0)
            .timeout_connect(Duration::from_secs(10))
            .timeout(Duration::from_secs(45))
            .build();
        let mut http = agent
            .request(request.method, &url)
            .set("User-Agent", rustpy_net::USER_AGENT)
            .set("Accept", "application/vnd.github+json");
        if !token.is_empty() {
            http = if server.provider == Provider::GitLab {
                http.set("PRIVATE-TOKEN", token)
            } else {
                http.set("Authorization", &format!("Bearer {token}"))
                    .set("X-GitHub-Api-Version", "2022-11-28")
            };
        }
        let response = match &request.body {
            Some(body) => http
                .set("Content-Type", "application/json")
                .send_string(&body.to_string()),
            None => http.call(),
        };
        let response = match response {
            Ok(response) => response,
            Err(ureq::Error::Status(code, response)) => {
                let mut text = String::new();
                let _ = response.into_reader().take(65536).read_to_string(&mut text);
                let message = serde_json::from_str::<Value>(&text)
                    .ok()
                    .and_then(|value| value.get("message").cloned())
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "Request rejected".into());
                return Err(format!(
                    "Server returned HTTP {code}: {}",
                    redact(&message, token)
                ));
            }
            Err(error) => return Err(redact(&error.to_string(), token)),
        };
        let next_page = response.header("Link").and_then(next_page).or_else(|| {
            response
                .header("X-Next-Page")
                .and_then(|page| page.parse().ok())
        });
        let scopes = response
            .header("X-OAuth-Scopes")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|scope| !scope.is_empty())
            .map(str::to_owned)
            .collect();
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if !request.is_write() {
            cancel.check().map_err(|error| error.to_string())?;
        }
        if bytes.len() > 8 * 1024 * 1024 {
            return Err("API response exceeds 8 MiB".into());
        }
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())?
        };
        Ok(Response {
            value,
            next_page,
            scopes,
        })
    }
}
fn redact(text: &str, token: &str) -> String {
    if token.is_empty() {
        text.into()
    } else {
        text.replace(token, "[redacted]")
    }
}
fn next_page(link: &str) -> Option<usize> {
    for part in link.split(',') {
        if !part.contains("rel=\"next\"") {
            continue;
        }
        let url = part.split('>').next()?.trim().trim_start_matches('<');
        for query in url.split('?').nth(1)?.split('&') {
            if let Some(page) = query
                .strip_prefix("page=")
                .and_then(|page| page.parse().ok())
            {
                return Some(page);
            }
        }
    }
    None
}
pub struct Client<T = HttpTransport> {
    pub server: Server,
    token: String,
    transport: T,
}
impl Client<HttpTransport> {
    pub fn account(account: &Account) -> Result<Self, String> {
        let server = Server::new(&account.server, account.provider)?;
        let token = rustpy_secrets::get(&account.secret_key())
            .map_err(|error| error.to_string())?
            .ok_or("Sign in to this account again")?;
        Ok(Self::new(server, token, HttpTransport))
    }
}
impl<T: Transport> Client<T> {
    pub fn new(server: Server, token: String, transport: T) -> Self {
        Self {
            server,
            token,
            transport,
        }
    }
    pub fn send(&self, request: &Request, cancel: &CancellationToken) -> Result<Response, String> {
        self.transport
            .send(&self.server, &self.token, request, cancel)
    }
    pub fn pages(
        &self,
        path: &str,
        key: Option<&str>,
        cancel: &CancellationToken,
    ) -> Result<Vec<Value>, String> {
        let mut page = 1;
        let mut rows = Vec::new();
        for _ in 0..100 {
            let separator = if path.contains('?') { '&' } else { '?' };
            let response = self.send(
                &Request::read(format!("{path}{separator}per_page=100&page={page}")),
                cancel,
            )?;
            let values = key
                .and_then(|key| response.value.get(key))
                .unwrap_or(&response.value)
                .as_array()
                .ok_or("Expected a paginated list")?;
            rows.extend(values.iter().cloned());
            if rows.len() > 10000 {
                return Err("API list exceeds 10,000 items; narrow the filter".into());
            }
            match response.next_page {
                Some(next) if next > page => page = next,
                _ => return Ok(rows),
            };
        }
        Err("Pagination limit exceeded".into())
    }
}
