//! Parser for the `.http` request file format.
//!
//! ```text
//! ### Get a user
//! # @name getUser
//! GET https://{{host}}/users/1
//!     ?expand=profile
//! Accept: application/json
//!
//! ###
//! POST https://{{host}}/users HTTP/1.1
//! Content-Type: application/json
//!
//! {"name": "x"}
//!
//! > {% client.test("created", function() { client.assert(response.status === 201); }); %}
//! ```

use std::collections::HashMap;
use std::path::PathBuf;

use rustpy_core::{TextRange, TextSize};

/// One request of a `.http` file. Text parts are kept verbatim (variables are
/// substituted by [`resolve_request`] at execution time).
#[derive(Clone, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct HttpRequest {
    /// From `### name` or `# @name name`.
    pub name: Option<String>,
    pub method: String,
    pub url: String,
    /// `HTTP/1.1` when written on the request line.
    pub version: Option<String>,
    pub headers: Vec<(String, String)>,
    /// Inline body text (trailing blank lines removed).
    pub body: Option<String>,
    /// `< ./path` body: file relative to the request file's directory.
    pub body_file: Option<PathBuf>,
    /// Byte range of the request block inside the file (without the `###` separator).
    pub range: TextRange,
    /// 1-based line of the request line.
    pub line: usize,
    /// `# @no-redirect`
    pub no_redirect: bool,
    /// `# @no-log`: do not store the request in the history.
    pub no_log: bool,
    pub no_cookie_jar: bool,
    pub timeout_ms: Option<u64>,
    pub connection_timeout_ms: Option<u64>,
    pub pre_script: Option<String>,
    pub directives: HashMap<String, String>,
    pub overwrite_response: bool,
    pub binary_body: Option<Vec<u8>>,
    /// Response handler script text (`> {% … %}` content) or `> path` reference.
    pub handler: Option<String>,
    /// `>> file` / `>>! file`: where the response body should be saved.
    pub save_response: Option<String>,
}

const METHODS: &[&str] = &[
    "GET",
    "POST",
    "PUT",
    "PATCH",
    "DELETE",
    "HEAD",
    "OPTIONS",
    "TRACE",
    "CONNECT",
    "LOCK",
    "UNLOCK",
    "PROPFIND",
    "PROPPATCH",
    "MKCOL",
    "COPY",
    "MOVE",
    "GRAPHQL",
    "GRPC",
    "WEBSOCKET",
];

#[derive(PartialEq, Eq, Clone, Copy)]
enum Stage {
    /// Comments / directives before the request line.
    Preamble,
    /// Right after the request line: indented lines continue the URL.
    RequestLine,
    Headers,
    Body,
    /// Inside a `> {% … %}` script.
    Handler,
    PreScript,
}

struct Line<'a> {
    text: &'a str,
    start: usize,
    end: usize,
    number: usize,
}

fn lines(text: &str) -> Vec<Line<'_>> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (n, raw) in text.split('\n').enumerate() {
        let end_with_nl = start + raw.len();
        let trimmed = raw.strip_suffix('\r').unwrap_or(raw);
        out.push(Line {
            text: trimmed,
            start,
            end: start + trimmed.len(),
            number: n + 1,
        });
        start = end_with_nl + 1;
    }
    out
}

fn is_comment(line: &str) -> bool {
    let t = line.trim_start();
    (t.starts_with('#') && !t.starts_with("###")) || t.starts_with("//")
}

/// `# @name foo` / `// @no-redirect` → `Some(("name", "foo"))`.
fn directive(line: &str) -> Option<(String, String)> {
    let t = line.trim_start();
    let rest = t
        .strip_prefix('#')
        .or_else(|| t.strip_prefix("//"))?
        .trim_start();
    let rest = rest.strip_prefix('@')?;
    let mut parts = rest.splitn(2, char::is_whitespace);
    let key = parts.next()?.trim().to_string();
    let value = parts.next().unwrap_or("").trim().to_string();
    Some((key, value))
}

fn parse_request_line(line: &str) -> (String, String, Option<String>) {
    let t = line.trim();
    let mut tokens: Vec<&str> = t.split_whitespace().collect();
    let mut version = None;
    if tokens.len() > 1 && tokens.last().is_some_and(|v| v.starts_with("HTTP/")) {
        version = tokens.pop().map(|v| v.to_string());
    }
    if tokens.is_empty() {
        return ("GET".into(), String::new(), version);
    }
    let first = tokens[0];
    let is_method = METHODS.contains(&first.to_ascii_uppercase().as_str())
        || (first.len() <= 10 && first.chars().all(|c| c.is_ascii_uppercase()) && tokens.len() > 1);
    if is_method {
        (first.to_ascii_uppercase(), tokens[1..].join(" "), version)
    } else {
        ("GET".into(), tokens.join(" "), version)
    }
}

fn is_header_line(line: &str) -> bool {
    let Some((name, _)) = line.split_once(':') else {
        return false;
    };
    let name = name.trim();
    !name.is_empty()
        && !name.contains(' ')
        && !name.starts_with('{')
        && !name.starts_with('<')
        && !name.starts_with('>')
        && !name.starts_with('/')
}

struct Builder {
    req: HttpRequest,
    block_start: usize,
    content_end: usize,
    stage: Stage,
    body_lines: Vec<String>,
    handler_lines: Vec<String>,
    pre_lines: Vec<String>,
    has_request_line: bool,
}

impl Builder {
    fn new(block_start: usize, name: Option<String>) -> Self {
        Self {
            req: HttpRequest {
                name,
                ..Default::default()
            },
            block_start,
            content_end: block_start,
            stage: Stage::Preamble,
            body_lines: Vec::new(),
            handler_lines: Vec::new(),
            pre_lines: Vec::new(),
            has_request_line: false,
        }
    }

    fn finish(mut self) -> Option<HttpRequest> {
        if !self.has_request_line {
            return None;
        }
        // Trim trailing blank lines of the body.
        while self.body_lines.last().is_some_and(|l| l.trim().is_empty()) {
            self.body_lines.pop();
        }
        if !self.body_lines.is_empty() {
            let joined = self.body_lines.join("\n");
            let trimmed = joined.trim();
            if self.body_lines.len() == 1 && trimmed.starts_with('<') && !trimmed.starts_with("<<")
            {
                let path = trimmed[1..].trim();
                if !path.is_empty() && !path.contains(' ')
                    || path.ends_with(".json")
                    || path.ends_with(".txt")
                    || path.ends_with(".xml")
                {
                    self.req.body_file = Some(PathBuf::from(path));
                } else {
                    self.req.body = Some(joined);
                }
            } else {
                self.req.body = Some(joined);
            }
        }
        if !self.handler_lines.is_empty() {
            self.req.handler = Some(self.handler_lines.join("\n").trim().to_string());
        }
        if !self.pre_lines.is_empty() {
            self.req.pre_script = Some(self.pre_lines.join("\n"));
        }
        let start = TextSize::new(self.block_start as u32);
        let end = TextSize::new(self.content_end.max(self.block_start) as u32);
        self.req.range = TextRange::new(start, end);
        Some(self.req)
    }
}

/// Parses a request file into its requests (in file order).
pub fn parse(text: &str) -> Vec<HttpRequest> {
    let mut out = Vec::new();
    let lines = lines(text);
    let mut builder = Builder::new(0, None);
    for line in &lines {
        let t = line.text;
        let trimmed = t.trim();
        // Separator: finish the current request and start a new block.
        if trimmed.starts_with("###") && !matches!(builder.stage, Stage::Handler | Stage::PreScript)
        {
            if let Some(r) = builder.finish() {
                out.push(r);
            }
            let name = trimmed.trim_start_matches('#').trim();
            builder = Builder::new(
                line.end + 1,
                if name.is_empty() {
                    None
                } else {
                    Some(name.to_string())
                },
            );
            builder.block_start = (line.end + 1).min(text.len());
            builder.content_end = builder.block_start;
            continue;
        }
        match builder.stage {
            Stage::PreScript => {
                if let Some(index) = t.find("%}") {
                    builder.pre_lines.push(t[..index].into());
                    builder.stage = Stage::Preamble;
                } else {
                    builder.pre_lines.push(t.into());
                }
                builder.content_end = line.end;
            }
            Stage::Handler => {
                if let Some(idx) = t.find("%}") {
                    builder.handler_lines.push(t[..idx].to_string());
                    builder.stage = Stage::Body;
                } else {
                    builder.handler_lines.push(t.to_string());
                }
                builder.content_end = line.end;
            }
            Stage::Preamble => {
                if trimmed.is_empty() {
                    continue;
                }
                if let Some(script) = trimmed.strip_prefix("< {%") {
                    if let Some(index) = script.find("%}") {
                        builder.pre_lines.push(script[..index].into());
                    } else {
                        builder.pre_lines.push(script.into());
                        builder.stage = Stage::PreScript;
                    }
                    continue;
                }
                if is_comment(t) {
                    if let Some((k, v)) = directive(t) {
                        builder.req.directives.insert(k.clone(), v.clone());
                        match k.as_str() {
                            "name" if !v.is_empty() => builder.req.name = Some(v),
                            "no-redirect" => builder.req.no_redirect = true,
                            "no-log" => builder.req.no_log = true,
                            "no-cookie-jar" => builder.req.no_cookie_jar = true,
                            "timeout" => builder.req.timeout_ms = v.parse().ok(),
                            "connection-timeout" => {
                                builder.req.connection_timeout_ms = v.parse().ok()
                            }
                            _ => {}
                        }
                    }
                    builder.content_end = line.end;
                    continue;
                }
                if trimmed.starts_with('@') && trimmed.contains('=') {
                    // File-level variable definition; not part of a request.
                    continue;
                }
                let (method, url, version) = parse_request_line(t);
                builder.req.method = method;
                builder.req.url = url;
                builder.req.version = version;
                builder.req.line = line.number;
                builder.has_request_line = true;
                if builder.block_start > line.start || builder.content_end == builder.block_start {
                    // The block effectively starts at the first content line.
                }
                builder.content_end = line.end;
                builder.stage = Stage::RequestLine;
            }
            Stage::RequestLine => {
                if trimmed.is_empty() {
                    builder.stage = Stage::Body;
                    continue;
                }
                let indented = t.starts_with(' ') || t.starts_with('\t');
                if indented && !is_header_line(trimmed) {
                    builder.req.url.push_str(trimmed);
                    builder.content_end = line.end;
                    continue;
                }
                if indented && (trimmed.starts_with('?') || trimmed.starts_with('&')) {
                    builder.req.url.push_str(trimmed);
                    builder.content_end = line.end;
                    continue;
                }
                builder.stage = Stage::Headers;
                push_header_or_body(&mut builder, t, trimmed, line.end);
            }
            Stage::Headers => {
                if trimmed.is_empty() {
                    builder.stage = Stage::Body;
                    continue;
                }
                push_header_or_body(&mut builder, t, trimmed, line.end);
            }
            Stage::Body => {
                if let Some(rest) = trimmed.strip_prefix(">>") {
                    builder.req.overwrite_response = rest.starts_with('!');
                    builder.req.save_response =
                        Some(rest.trim_start_matches('!').trim().to_string());
                    builder.content_end = line.end;
                    continue;
                }
                if let Some(rest) = trimmed.strip_prefix('>') {
                    let rest = rest.trim();
                    if let Some(script) = rest.strip_prefix("{%") {
                        if let Some(idx) = script.find("%}") {
                            builder.handler_lines.push(script[..idx].to_string());
                        } else {
                            builder.handler_lines.push(script.to_string());
                            builder.stage = Stage::Handler;
                        }
                    } else if !rest.is_empty() {
                        builder.handler_lines.push(format!("> {rest}"));
                    }
                    builder.content_end = line.end;
                    continue;
                }
                if is_comment(t) && builder.body_lines.is_empty() {
                    builder.content_end = line.end;
                    continue;
                }
                builder.body_lines.push(t.to_string());
                if !trimmed.is_empty() {
                    builder.content_end = line.end;
                }
            }
        }
    }
    if let Some(r) = builder.finish() {
        out.push(r);
    }
    out
}

fn push_header_or_body(builder: &mut Builder, raw: &str, trimmed: &str, end: usize) {
    if is_comment(raw) {
        builder.content_end = end;
        return;
    }
    if is_header_line(trimmed) {
        let (name, value) = trimmed.split_once(':').unwrap();
        builder
            .req
            .headers
            .push((name.trim().to_string(), value.trim().to_string()));
    } else {
        // Not a header: the body started without a blank line.
        builder.stage = Stage::Body;
        builder.body_lines.push(raw.to_string());
    }
    builder.content_end = end;
}

/// File-level variables (`@host = example.com`), later definitions override earlier ones.
pub fn file_variables(text: &str) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    for line in text.lines() {
        let t = line.trim();
        let Some(rest) = t.strip_prefix('@') else {
            continue;
        };
        let Some((name, value)) = rest.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() || name.contains(char::is_whitespace) {
            continue;
        }
        let value = crate::env::substitute(value.trim(), &vars);
        vars.insert(name.to_string(), value);
    }
    vars
}

/// Substitutes `{{variables}}` in the URL, headers and inline body.
pub fn resolve_request(req: &HttpRequest, vars: &HashMap<String, String>) -> HttpRequest {
    let mut r = req.clone();
    r.url = crate::env::substitute(&req.url, vars);
    r.headers = req
        .headers
        .iter()
        .map(|(k, v)| (k.clone(), crate::env::substitute(v, vars)))
        .collect();
    r.body = req.body.as_ref().map(|b| crate::env::substitute(b, vars));
    r.directives = req
        .directives
        .iter()
        .map(|(key, value)| (key.clone(), crate::env::substitute(value, vars)))
        .collect();
    r.save_response = req
        .save_response
        .as_ref()
        .map(|path| crate::env::substitute(path, vars));
    r.body_file = req
        .body_file
        .as_ref()
        .map(|p| PathBuf::from(crate::env::substitute(&p.to_string_lossy(), vars)));
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"@host = api.example.com
@token = abc

### List users
GET https://{{host}}/users
    ?page=2
    &limit=10
Accept: application/json
Authorization: Bearer {{token}}

###
# @name createUser
# @no-redirect
POST https://{{host}}/users HTTP/1.1
Content-Type: application/json

{
  "name": "Ada"
}

> {%
client.test("created", function() {
  client.assert(response.status === 201);
});
%}

### Upload
PUT https://{{host}}/upload
Content-Type: application/octet-stream

< ./data.bin

### Plain url
https://example.com/health
"#;

    #[test]
    fn parses_multiple_requests() {
        let reqs = parse(SAMPLE);
        assert_eq!(reqs.len(), 4, "{reqs:#?}");
        let r0 = &reqs[0];
        assert_eq!(r0.name.as_deref(), Some("List users"));
        assert_eq!(r0.method, "GET");
        assert_eq!(r0.url, "https://{{host}}/users?page=2&limit=10");
        assert_eq!(r0.headers.len(), 2);
        assert_eq!(
            r0.headers[1],
            ("Authorization".into(), "Bearer {{token}}".into())
        );
        assert!(r0.body.is_none());
        assert_eq!(r0.line, 5);
        assert!(!r0.no_redirect);

        let r1 = &reqs[1];
        assert_eq!(r1.name.as_deref(), Some("createUser"));
        assert!(r1.no_redirect);
        assert_eq!(r1.method, "POST");
        assert_eq!(r1.version.as_deref(), Some("HTTP/1.1"));
        assert_eq!(r1.body.as_deref(), Some("{\n  \"name\": \"Ada\"\n}"));
        let handler = r1.handler.as_deref().unwrap();
        assert!(handler.starts_with("client.test(\"created\""), "{handler}");
        assert!(handler.contains("response.status === 201"));

        let r2 = &reqs[2];
        assert_eq!(r2.method, "PUT");
        assert_eq!(
            r2.body_file.as_deref(),
            Some(std::path::Path::new("./data.bin"))
        );
        assert!(r2.body.is_none());

        let r3 = &reqs[3];
        assert_eq!(r3.method, "GET");
        assert_eq!(r3.url, "https://example.com/health");
        assert_eq!(r3.name.as_deref(), Some("Plain url"));
    }

    #[test]
    fn ranges_cover_blocks() {
        let reqs = parse(SAMPLE);
        for r in &reqs {
            let slice = &SAMPLE[usize::from(r.range.start())..usize::from(r.range.end())];
            assert!(
                slice.contains(r.url.split('?').next().unwrap()),
                "range for {:?} = {slice:?}",
                r.name
            );
            assert!(!slice.starts_with("###"));
        }
        assert!(
            SAMPLE[usize::from(reqs[1].range.start())..usize::from(reqs[1].range.end())]
                .ends_with("%}")
        );
    }

    #[test]
    fn file_variables_and_resolution() {
        let vars = file_variables(SAMPLE);
        assert_eq!(
            vars.get("host").map(String::as_str),
            Some("api.example.com")
        );
        assert_eq!(vars.get("token").map(String::as_str), Some("abc"));
        let reqs = parse(SAMPLE);
        let r = resolve_request(&reqs[0], &vars);
        assert_eq!(r.url, "https://api.example.com/users?page=2&limit=10");
        assert_eq!(r.headers[1].1, "Bearer abc");
    }

    #[test]
    fn single_request_without_separator_and_crlf() {
        let text = "GET https://a.b/c\r\nX-Test: 1\r\n\r\nbody line\r\n";
        let reqs = parse(text);
        assert_eq!(reqs.len(), 1);
        assert_eq!(
            reqs[0].headers,
            vec![("X-Test".to_string(), "1".to_string())]
        );
        assert_eq!(reqs[0].body.as_deref(), Some("body line"));
    }

    #[test]
    fn response_redirect_and_inline_handler() {
        let text = "GET https://a.b/c\n\n>> ./out.json\n> {% client.log(1) %}\n";
        let reqs = parse(text);
        assert_eq!(reqs[0].save_response.as_deref(), Some("./out.json"));
        assert_eq!(reqs[0].handler.as_deref(), Some("client.log(1)"));
        assert!(reqs[0].body.is_none());
    }
}
