//! User-facing messages for API errors (HTTP error responses and SSE `error` events).

use serde_json::Value;

/// Longest excerpt of a non-JSON error body included in a message.
const SNIPPET_CHARS: usize = 300;

/// `(error type, message)` from an error body. Understands the Anthropic shape
/// `{"type":"error","error":{"type":"…","message":"…"}}` and the common gateway
/// shapes `{"error":{"message":"…"}}`, `{"error":"…"}` and `{"message":"…"}`.
pub(crate) fn error_parts(v: &Value) -> Option<(Option<String>, String)> {
    let err = v.get("error").unwrap_or(v);
    let message = err
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| err.as_str())
        .or_else(|| v.get("message").and_then(Value::as_str))?
        .trim();
    if message.is_empty() {
        return None;
    }
    let kind = err
        .get("type")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty() && *t != "error");
    Some((kind.map(str::to_owned), message.to_owned()))
}

/// Message for an HTTP error response (`status` ≥ 300).
pub(crate) fn http_error_message(status: u16, status_text: &str, body: &str) -> String {
    let parsed = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| error_parts(&v));
    let message = match parsed {
        Some((Some(kind), message)) => format!("API error {status} ({kind}): {message}"),
        Some((None, message)) => format!("API error {status}: {message}"),
        None => {
            let mut s = format!("HTTP {status}");
            if !status_text.trim().is_empty() {
                s.push(' ');
                s.push_str(status_text.trim());
            }
            let snippet = snippet(body);
            if !snippet.is_empty() {
                s.push_str(": ");
                s.push_str(&snippet);
            }
            s
        }
    };
    match status {
        401 => add_sentence(
            message,
            "Check the API key in the AI Assistant settings or the ANTHROPIC_API_KEY environment variable.",
        ),
        300..=399 => add_sentence(message, "Unexpected redirect: check the API base URL."),
        _ => message,
    }
}

/// Message for the data of an SSE `error` event.
pub(crate) fn stream_error_message(data: &str) -> String {
    match serde_json::from_str::<Value>(data)
        .ok()
        .and_then(|v| error_parts(&v))
    {
        Some((Some(kind), message)) => format!("API error ({kind}): {message}"),
        Some((None, message)) => format!("API error: {message}"),
        None => {
            let snippet = snippet(data);
            if snippet.is_empty() {
                "API error (no details given)".to_string()
            } else {
                format!("API error: {snippet}")
            }
        }
    }
}

/// Whitespace-collapsed excerpt of a body; empty for HTML pages (proxy error pages).
fn snippet(body: &str) -> String {
    let body = body.trim();
    if body.starts_with('<') {
        return String::new();
    }
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= SNIPPET_CHARS {
        collapsed
    } else {
        let mut s: String = collapsed.chars().take(SNIPPET_CHARS).collect();
        s.push('…');
        s
    }
}

fn add_sentence(message: String, sentence: &str) -> String {
    let sep = if message.ends_with(['.', '!', '?']) {
        " "
    } else {
        ". "
    };
    format!("{message}{sep}{sentence}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_error_body() {
        let body = r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"},"request_id":"req_011CSHoEeqs5C35K2UUqR7Fy"}"#;
        let msg = http_error_message(401, "Unauthorized", body);
        assert!(
            msg.starts_with(
                "API error 401 (authentication_error): invalid x-api-key. Check the API key"
            ),
            "{msg}"
        );
        let msg = http_error_message(
            400,
            "Bad Request",
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"max_tokens: must be positive"}}"#,
        );
        assert_eq!(
            msg,
            "API error 400 (invalid_request_error): max_tokens: must be positive"
        );
    }

    #[test]
    fn gateway_and_plain_bodies() {
        assert_eq!(
            http_error_message(
                502,
                "Bad Gateway",
                r#"{"error":{"message":"upstream timed out"}}"#
            ),
            "API error 502: upstream timed out"
        );
        assert_eq!(
            http_error_message(429, "", r#"{"error":"slow down"}"#),
            "API error 429: slow down"
        );
        assert_eq!(
            http_error_message(503, "Service Unavailable", "  try\n again  later "),
            "HTTP 503 Service Unavailable: try again later"
        );
        assert_eq!(
            http_error_message(502, "Bad Gateway", "<html><body>nginx</body></html>"),
            "HTTP 502 Bad Gateway"
        );
        let long = "x".repeat(1000);
        let msg = http_error_message(500, "", &long);
        assert!(msg.ends_with('…') && msg.chars().count() < 400, "{msg}");
        assert!(http_error_message(301, "Moved Permanently", "").contains("base URL"));
    }

    #[test]
    fn stream_error_event_data() {
        assert_eq!(
            stream_error_message(
                r#"{"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}"#
            ),
            "API error (overloaded_error): Overloaded"
        );
        assert_eq!(stream_error_message("not json"), "API error: not json");
        assert_eq!(stream_error_message(""), "API error (no details given)");
    }
}
