//! Pure request conversion, examples and endpoint import APIs.
use crate::HttpRequest;
use std::path::Path;
pub fn words(text: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escape = false;
    for character in text.chars() {
        if escape {
            word.push(character);
            escape = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escape = true;
            continue;
        }
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            } else {
                word.push(character);
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            character if character.is_whitespace() => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            _ => word.push(character),
        }
    }
    if quote.is_some() || escape {
        return Err("Unclosed quote or escape in cURL input".into());
    }
    if !word.is_empty() {
        words.push(word);
    }
    Ok(words)
}
pub fn from_curl(text: &str) -> Result<HttpRequest, String> {
    let words = words(text)?;
    if words.first().map(String::as_str) != Some("curl") {
        return Err("Expected a cURL command".into());
    }
    let mut request = HttpRequest {
        method: "GET".into(),
        ..Default::default()
    };
    let mut index = 1;
    while index < words.len() {
        let word = &words[index];
        let value = |index: &mut usize| -> Result<String, String> {
            *index += 1;
            words
                .get(*index)
                .cloned()
                .ok_or_else(|| format!("Missing value for {word}"))
        };
        match word.as_str() {
            "-X" | "--request" => request.method = value(&mut index)?,
            "-H" | "--header" => {
                let header = value(&mut index)?;
                let (key, value) = header.split_once(':').ok_or("Invalid cURL header")?;
                request
                    .headers
                    .push((key.trim().into(), value.trim().into()));
            }
            "-d" | "--data" | "--data-raw" | "--data-binary" | "--json" => {
                let body = value(&mut index)?;
                if let Some(file) = body.strip_prefix('@') {
                    request.body_file = Some(file.into());
                } else {
                    request.body = Some(body);
                }
                if request.method == "GET" {
                    request.method = "POST".into();
                }
                if word == "--json" {
                    request
                        .headers
                        .push(("Content-Type".into(), "application/json".into()));
                }
            }
            "-u" | "--user" => {
                use base64::Engine;
                request.headers.push((
                    "Authorization".into(),
                    format!(
                        "Basic {}",
                        base64::engine::general_purpose::STANDARD.encode(value(&mut index)?)
                    ),
                ));
            }
            "-I" | "--head" => request.method = "HEAD".into(),
            "--url" => request.url = value(&mut index)?,
            "-L" | "--location" | "--compressed" | "-s" | "--silent" | "--show-error" => {}
            "--max-time" => {
                request.timeout_ms = Some(
                    (value(&mut index)?
                        .parse::<f64>()
                        .map_err(|_| "Invalid timeout")?
                        * 1000.0) as u64,
                )
            }
            "--connect-timeout" => {
                request.connection_timeout_ms = Some(
                    (value(&mut index)?
                        .parse::<f64>()
                        .map_err(|_| "Invalid timeout")?
                        * 1000.0) as u64,
                )
            }
            word if !word.starts_with('-') && request.url.is_empty() => request.url = word.into(),
            unknown => {
                return Err(format!(
                    "Unsupported cURL option {unknown}; nothing was executed"
                ))
            }
        }
        index += 1;
    }
    if request.url.is_empty() {
        return Err("cURL input has no URL".into());
    }
    Ok(request)
}
pub fn to_http(request: &HttpRequest) -> String {
    let mut text = format!("### {}\n", request.name.as_deref().unwrap_or("Request"));
    for (key, value) in &request.directives {
        text.push_str(&format!("# @{key} {value}\n"));
    }
    for (key, enabled) in [
        ("no-redirect", request.no_redirect),
        ("no-cookie-jar", request.no_cookie_jar),
        ("no-log", request.no_log),
    ] {
        if enabled && !request.directives.contains_key(key) {
            text.push_str(&format!("# @{key}\n"));
        }
    }
    for (key, value) in [
        ("timeout", request.timeout_ms),
        ("connection-timeout", request.connection_timeout_ms),
    ] {
        if let Some(value) = value.filter(|_| !request.directives.contains_key(key)) {
            text.push_str(&format!("# @{key} {value}\n"));
        }
    }
    if let Some(source) = &request.pre_script {
        text.push_str(&format!("< {{%\n{source}\n%}}\n"));
    }
    text.push_str(&format!("{} {}\n", request.method, request.url));
    for (key, value) in &request.headers {
        text.push_str(&format!("{key}: {value}\n"));
    }
    text.push('\n');
    if let Some(path) = &request.body_file {
        text.push_str(&format!("< {}\n", path.display()));
    } else if let Some(body) = &request.body {
        text.push_str(body);
        text.push('\n');
    }
    if let Some(source) = &request.handler {
        text.push_str(&format!("> {{%\n{source}\n%}}\n"));
    }
    if let Some(path) = &request.save_response {
        text.push_str(&format!(
            ">>{} {path}\n",
            if request.overwrite_response { "!" } else { "" }
        ));
    }
    text
}
pub fn to_curl(request: &HttpRequest) -> String {
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
    let mut text = format!("curl -X {} {}", quote(&request.method), quote(&request.url));
    for (key, value) in &request.headers {
        text.push_str(&format!(" -H {}", quote(&format!("{key}: {value}"))));
    }
    if let Some(path) = &request.body_file {
        text.push_str(&format!(
            " --data-binary {}",
            quote(&format!("@{}", path.display()))
        ));
    } else if let Some(body) = &request.body {
        text.push_str(&format!(" --data-raw {}", quote(body)));
    }
    text
}
pub fn multipart_body(body: &str, base: &Path) -> Result<Vec<u8>, String> {
    let mut result = Vec::new();
    for line in body.split_inclusive('\n') {
        if let Some(file) = line.trim().strip_prefix("< ") {
            let snapshot = rustpy_core::vfs::Vfs::new()
                .read(
                    &base.join(file),
                    rustpy_core::vfs::ReadLimits {
                        max_file_bytes: 16 * 1024 * 1024,
                        max_cached_bytes: 0,
                        max_cached_files: 0,
                    },
                )
                .map_err(|error| error.to_string())?;
            result.extend_from_slice(&snapshot.bytes);
            result.extend_from_slice(b"\r\n");
        } else {
            result.extend_from_slice(line.trim_end_matches(['\r', '\n']).as_bytes());
            result.extend_from_slice(b"\r\n");
        }
        if result.len() > 16 * 1024 * 1024 {
            return Err("Multipart body exceeds 16 MiB".into());
        }
    }
    Ok(result)
}
pub fn format_markup(body: &str) -> String {
    let mut output = String::new();
    let mut depth = 0usize;
    for segment in body.replace("><", ">\n<").lines() {
        let segment = segment.trim();
        if segment.starts_with("</") {
            depth = depth.saturating_sub(1);
        }
        output.push_str(&"  ".repeat(depth.min(40)));
        output.push_str(segment);
        output.push('\n');
        if segment.starts_with('<')
            && !segment.starts_with("</")
            && !segment.starts_with("<?")
            && !segment.starts_with("<!")
            && !segment.ends_with("/>")
            && !segment.contains("</")
        {
            depth += 1;
        }
    }
    output
}
pub fn template(name: &str, url: &str) -> Result<String, String> {
    let request=match name{"gtr"|"GET"=>format!("### GET request\nGET {url}\nAccept: application/json\n\n"),"ptr"|"POST"=>format!("### POST JSON\nPOST {url}\nContent-Type: application/json\n\n{{\n  \"key\": \"value\"\n}}\n"),"mptr"|"Multipart"=>format!("### Multipart form\nPOST {url}\nContent-Type: multipart/form-data; boundary=RustPyBoundary\n\n--RustPyBoundary\nContent-Disposition: form-data; name=\"file\"; filename=\"upload.bin\"\nContent-Type: application/octet-stream\n\n< ./upload.bin\n--RustPyBoundary--\n"),"GraphQL"=>format!("### GraphQL\nGRAPHQL {url}\n\nquery {{ __typename }}\n"),"WebSocket"=>format!("### WebSocket\n# @timeout 30000\nWEBSOCKET {}\n\nhello\n",url.replace("https://","wss://").replace("http://","ws://")),"gRPC"=>"### gRPC (server reflection or # @proto path.proto)\nGRPC grpc://localhost:50051/package.Service/Method\n\n{}\n".to_string(),_=>return Err("Unknown request template".into())};
    Ok(request)
}
pub fn openapi(text: &str) -> Result<Vec<HttpRequest>, String> {
    let document: serde_json::Value = serde_json::from_str(text)
        .or_else(|_| serde_saphyr::from_str(text))
        .map_err(|error| error.to_string())?;
    let base = document
        .pointer("/servers/0/url")
        .and_then(|value| value.as_str())
        .unwrap_or("{{host}}");
    let paths = document
        .get("paths")
        .and_then(|value| value.as_object())
        .ok_or("OpenAPI spec has no paths object")?;
    let mut result = Vec::new();
    for (path, operations) in paths {
        let Some(operations) = operations.as_object() else {
            continue;
        };
        for (method, operation) in operations {
            if ![
                "get", "post", "put", "patch", "delete", "head", "options", "trace",
            ]
            .contains(&method.as_str())
            {
                continue;
            }
            let mut url = format!("{base}{path}");
            if let Some(parameters) = operation
                .get("parameters")
                .and_then(|value| value.as_array())
            {
                for parameter in parameters {
                    if let Some(name) = parameter.get("name").and_then(|value| value.as_str()) {
                        if parameter.get("in").and_then(|value| value.as_str()) == Some("path") {
                            url = url.replace(&format!("{{{name}}}"), &format!("{{{{{name}}}}}"));
                        } else if parameter.get("in").and_then(|value| value.as_str())
                            == Some("query")
                        {
                            url.push(if url.contains('?') { '&' } else { '?' });
                            url.push_str(&format!("{name}={{{{{name}}}}}"));
                        }
                    }
                }
            }
            let body = operation
                .pointer("/requestBody/content/application~1json/example")
                .map(|value| serde_json::to_string_pretty(value).unwrap());
            result.push(HttpRequest {
                name: operation
                    .get("summary")
                    .or_else(|| operation.get("operationId"))
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
                method: method.to_ascii_uppercase(),
                url,
                headers: if body.is_some() {
                    vec![("Content-Type".into(), "application/json".into())]
                } else {
                    Vec::new()
                },
                body,
                ..Default::default()
            });
            if result.len() > 4096 {
                return Err("OpenAPI import exceeds 4096 operations".into());
            }
        }
    }
    Ok(result)
}
/// Public Postman v2 collection import (request folders are traversed, never executed).
pub fn postman(text: &str) -> Result<Vec<HttpRequest>, String> {
    fn visit(
        value: &serde_json::Value,
        result: &mut Vec<HttpRequest>,
        depth: usize,
    ) -> Result<(), String> {
        if depth > 32 {
            return Err("Postman folders exceed 32 levels".into());
        }
        if let Some(items) = value.get("item").and_then(|value| value.as_array()) {
            for item in items {
                visit(item, result, depth + 1)?;
            }
        }
        if let Some(request) = value.get("request") {
            let url = request
                .get("url")
                .and_then(|url| {
                    url.as_str()
                        .or_else(|| url.get("raw").and_then(|value| value.as_str()))
                })
                .ok_or("Postman request has no URL")?;
            result.push(HttpRequest {
                name: value
                    .get("name")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
                method: request
                    .get("method")
                    .and_then(|value| value.as_str())
                    .unwrap_or("GET")
                    .into(),
                url: url.into(),
                headers: request
                    .get("header")
                    .and_then(|value| value.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|header| {
                        Some((
                            header.get("key")?.as_str()?.into(),
                            header.get("value")?.as_str()?.into(),
                        ))
                    })
                    .collect(),
                body: request
                    .pointer("/body/raw")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
                ..Default::default()
            });
        }
        if result.len() > 4096 {
            return Err("Postman import exceeds 4096 requests".into());
        }
        Ok(())
    }
    let value = serde_json::from_str(text).map_err(|error| error.to_string())?;
    let mut result = Vec::new();
    visit(&value, &mut result, 0)?;
    Ok(result)
}
