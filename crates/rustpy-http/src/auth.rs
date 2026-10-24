//! Authentication helpers consumed by request execution and the request generator.
use crate::{
    runtime::{Control, Options},
    HttpRequest,
};
use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::{Digest, Sha256};
fn header(request: &mut HttpRequest, key: &str, value: String) {
    request
        .headers
        .retain(|(name, _)| !name.eq_ignore_ascii_case(key));
    request.headers.push((key.into(), value));
}
fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("Authentication requires {key}"))
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn mac(key: &[u8], data: &str) -> Vec<u8> {
    let mut value = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts arbitrary keys");
    value.update(data.as_bytes());
    value.finalize().into_bytes().to_vec()
}
fn percent(value: &str) -> String {
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
pub fn sigv4(request: &mut HttpRequest, value: &Value, date: &str) -> Result<(), String> {
    let url = url::Url::parse(&request.url).map_err(|error| error.to_string())?;
    let region = field(value, "region")?;
    let service = field(value, "service")?;
    let key = field(value, "access_key")?;
    let secret = field(value, "secret_key")?;
    if date.len() < 8 {
        return Err("Invalid AWS signing timestamp".into());
    }
    let host = url.host_str().ok_or("AWS signing URL has no host")?;
    let authority = url
        .port()
        .map(|port| format!("{host}:{port}"))
        .unwrap_or_else(|| host.into());
    header(request, "Host", authority);
    header(request, "X-Amz-Date", date.into());
    let payload = hex(&Sha256::digest(
        request
            .binary_body
            .as_deref()
            .unwrap_or_else(|| request.body.as_deref().unwrap_or("").as_bytes()),
    ));
    header(request, "X-Amz-Content-Sha256", payload.clone());
    if let Some(token) = value.get("session_token").and_then(Value::as_str) {
        header(request, "X-Amz-Security-Token", token.into());
    }
    let mut headers: Vec<_> = request
        .headers
        .iter()
        .filter(|(key, _)| !key.eq_ignore_ascii_case("authorization"))
        .map(|(key, value)| {
            (
                key.to_ascii_lowercase(),
                value.split_whitespace().collect::<Vec<_>>().join(" "),
            )
        })
        .collect();
    headers.sort();
    let signed = headers
        .iter()
        .map(|(key, _)| key.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let canonical_headers = headers
        .iter()
        .map(|(key, value)| format!("{key}:{value}\n"))
        .collect::<String>();
    let mut query: Vec<_> = url
        .query_pairs()
        .map(|(key, value)| (percent(&key), percent(&value)))
        .collect();
    query.sort();
    let query = query
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&");
    let canonical = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        request.method,
        url.path(),
        query,
        canonical_headers,
        signed,
        payload
    );
    let scope = format!("{}/{region}/{service}/aws4_request", &date[..8]);
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{date}\n{scope}\n{}",
        hex(&Sha256::digest(canonical.as_bytes()))
    );
    let date_key = mac(format!("AWS4{secret}").as_bytes(), &date[..8]);
    let region_key = mac(&date_key, region);
    let service_key = mac(&region_key, service);
    let signing = mac(&service_key, "aws4_request");
    let signature = hex(&mac(&signing, &to_sign));
    header(request,"Authorization",format!("AWS4-HMAC-SHA256 Credential={key}/{scope}, SignedHeaders={signed}, Signature={signature}"));
    Ok(())
}
fn oauth(value: &Value, options: &Options, control: &Control) -> Result<String, String> {
    let endpoint = field(value, "token_url")?;
    let grant = value
        .get("grant_type")
        .and_then(Value::as_str)
        .unwrap_or("client_credentials");
    let mut fields = vec![("grant_type".to_string(), grant.to_string())];
    for key in [
        "client_id",
        "client_secret",
        "scope",
        "username",
        "password",
        "code",
        "redirect_uri",
        "code_verifier",
        "refresh_token",
    ] {
        if let Some(value) = value.get(key).and_then(Value::as_str) {
            fields.push((key.into(), value.into()));
        }
    }
    let mut form = url::form_urlencoded::Serializer::new(String::new());
    form.extend_pairs(fields);
    let token_request = HttpRequest {
        method: "POST".into(),
        url: endpoint.into(),
        headers: vec![(
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        )],
        body: Some(form.finish()),
        no_log: true,
        ..Default::default()
    };
    let response = crate::exec::execute_transport(&token_request, options, control);
    if !response.is_success() {
        return Err(format!(
            "OAuth2 token exchange failed: {}",
            response.status_line()
        ));
    }
    let token: Value = serde_json::from_str(&response.body).map_err(|error| error.to_string())?;
    Ok(field(&token, "access_token")?.into())
}
// Authentication parameters are comma-separated, but quoted values (notably
// realms and qop lists) may themselves contain commas and escaped quotes.
fn digest_fields(challenge: &str) -> Result<std::collections::HashMap<String, String>, String> {
    let malformed = || "Malformed Digest challenge".to_string();
    let mut chars = challenge.chars().peekable();
    let mut fields = std::collections::HashMap::new();
    while chars.peek().is_some() {
        while chars.peek().is_some_and(|c| c.is_ascii_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let mut name = String::new();
        while chars
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(*c))
        {
            name.push(chars.next().unwrap());
        }
        while chars.peek().is_some_and(|c| c.is_ascii_whitespace()) {
            chars.next();
        }
        if name.is_empty() || chars.next() != Some('=') {
            return Err(malformed());
        }
        while chars.peek().is_some_and(|c| c.is_ascii_whitespace()) {
            chars.next();
        }
        let mut value = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            loop {
                match chars.next() {
                    Some('"') => break,
                    Some('\\') => value.push(chars.next().ok_or_else(malformed)?),
                    Some(c) if c != '\r' && c != '\n' => value.push(c),
                    _ => return Err(malformed()),
                }
            }
            while chars.peek().is_some_and(|c| c.is_ascii_whitespace()) {
                chars.next();
            }
        } else {
            while chars.peek().is_some_and(|c| *c != ',') {
                value.push(chars.next().unwrap());
            }
            value = value.trim().to_string();
        }
        if value.contains(['\r', '\n']) || fields.insert(name.to_ascii_lowercase(), value).is_some()
        {
            return Err(malformed());
        }
        match chars.next() {
            None => break,
            Some(',') if chars.peek().is_some() => {}
            _ => return Err(malformed()),
        }
    }
    Ok(fields)
}
fn digest_quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}
fn digest(
    request: &mut HttpRequest,
    value: &Value,
    options: &Options,
    control: &Control,
) -> Result<(), String> {
    use md5::Md5;
    let response = crate::exec::execute_transport(request, options, control);
    let challenge = response
        .header("www-authenticate")
        .ok_or("Server did not provide a Digest challenge")?;
    let challenge = challenge
        .strip_prefix("Digest ")
        .ok_or("Unsupported authentication challenge")?;
    let fields = digest_fields(challenge)?;
    let realm = fields.get("realm").ok_or("Digest challenge has no realm")?;
    let nonce = fields.get("nonce").ok_or("Digest challenge has no nonce")?;
    let algorithm = fields.get("algorithm").map(String::as_str).unwrap_or("MD5");
    let hash = |text: &str| -> Result<String, String> {
        match algorithm {
            "MD5" | "MD5-sess" => Ok(hex(&Md5::digest(text.as_bytes()))),
            "SHA-256" | "SHA-256-sess" => Ok(hex(&Sha256::digest(text.as_bytes()))),
            _ => Err(format!("Unsupported Digest algorithm {algorithm}")),
        }
    };
    let user = field(value, "username")?;
    let password = field(value, "password")?;
    let url = url::Url::parse(&request.url).map_err(|error| error.to_string())?;
    let uri = format!(
        "{}{}",
        url.path(),
        url.query()
            .map(|query| format!("?{query}"))
            .unwrap_or_default()
    );
    let cnonce = crate::builtin_variable("$uuid").unwrap();
    let mut a1 = hash(&format!("{user}:{realm}:{password}"))?;
    if algorithm.ends_with("-sess") {
        a1 = hash(&format!("{a1}:{nonce}:{cnonce}"))?;
    }
    let a2 = hash(&format!("{}:{uri}", request.method))?;
    let qop = fields
        .get("qop")
        .is_some_and(|qop| qop.split(',').any(|qop| qop.trim() == "auth"));
    if fields.contains_key("qop") && !qop {
        return Err("Digest server does not support qop=auth".into());
    }
    let digest = hash(&if qop {
        format!("{a1}:{nonce}:00000001:{cnonce}:auth:{a2}")
    } else {
        format!("{a1}:{nonce}:{a2}")
    })?;
    let mut authorization = format!(
        "Digest username={}, realm={}, nonce={}, uri={}, response={}, algorithm={algorithm}",
        digest_quoted(user),
        digest_quoted(realm),
        digest_quoted(nonce),
        digest_quoted(&uri),
        digest_quoted(&digest),
    );
    if qop {
        authorization.push_str(&format!(", qop=auth, nc=00000001, cnonce=\"{cnonce}\""));
    }
    if let Some(opaque) = fields.get("opaque") {
        authorization.push_str(&format!(", opaque={}", digest_quoted(opaque)));
    }
    header(request, "Authorization", authorization);
    Ok(())
}
pub fn prepare(
    request: &mut HttpRequest,
    options: &Options,
    control: &Control,
) -> Result<(), String> {
    let Some(spec) = request.directives.get("auth").cloned() else {
        return Ok(());
    };
    let value: Value =
        serde_json::from_str(&spec).map_err(|error| format!("Invalid @auth JSON: {error}"))?;
    match field(&value, "type")? {
        "basic" => header(
            request,
            "Authorization",
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!(
                    "{}:{}",
                    field(&value, "username")?,
                    field(&value, "password")?
                ))
            ),
        ),
        "bearer" => header(
            request,
            "Authorization",
            format!("Bearer {}", field(&value, "token")?),
        ),
        "oauth2" => header(
            request,
            "Authorization",
            format!("Bearer {}", oauth(&value, options, control)?),
        ),
        "digest" => digest(request, &value, options, control)?,
        "aws" | "sigv4" => sigv4(
            request,
            &value,
            &chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string(),
        )?,
        kind => return Err(format!("Unknown authentication helper {kind}")),
    }
    Ok(())
}
