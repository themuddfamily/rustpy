//! Client tests: request construction, retry and proxy logic, and end-to-end
//! runs against the local mock server (the real API is never contacted).

use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::json;

use super::*;
use crate::mock_server::{split_at, MockServer, Reply};
use crate::sse::tests::{RECORDED, RECORDED_ERROR, RECORDED_TEXT};
use crate::types::Message;

const WAIT: Duration = Duration::from_secs(20);

fn config(server: &MockServer) -> AiConfig {
    AiConfig {
        provider: crate::Provider::Anthropic,
        worker_owner: 0,
        api_key: Some("test-key".into()),
        model: "claude-opus-5-5".into(),
        max_tokens: 1024,
        base_url: server.base_url.clone(),
        refusal_fallback: false,
    }
}

fn request() -> ChatRequest {
    ChatRequest {
        system: "You are terse.".into(),
        messages: vec![Message::user("Say hello")],
    }
}

fn text_of(events: &[AiEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            AiEvent::Delta(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

fn recorded_done() -> AiEvent {
    AiEvent::Done {
        stop_reason: Some("end_turn".into()),
        input_tokens: Some(25),
        output_tokens: Some(15),
    }
}

fn json_reply(status: u16, headers: Vec<(&'static str, &'static str)>, body: &str) -> Reply {
    Reply::Full {
        status,
        content_type: "application/json",
        headers,
        body: body.to_string(),
    }
}

/// A port nothing listens on. (A privileged port: an ephemeral one could be
/// handed to a mock server of a test running in parallel.)
fn closed_port_url() -> String {
    "http://127.0.0.1:1".to_string()
}

const OVERLOADED: &str =
    r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
const INVALID_KEY: &str = r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"},"request_id":"req_011CSHoEeqs5C35K2UUqR7Fy"}"#;

// ---- request construction ------------------------------------------------

#[test]
fn refusal_fallback_only_on_the_first_party_api_for_supported_models() {
    // Off unless the user opts in.
    assert!(!Options::for_config(&AiConfig::default()).refusal_fallback);
    let on = AiConfig {
        refusal_fallback: true,
        ..AiConfig::default()
    };
    let opus = on.clone();
    assert!(Options::for_config(&opus).refusal_fallback);
    let sonnet = AiConfig {
        model: "claude-sonnet-5-5".into(),
        ..on.clone()
    };
    assert!(Options::for_config(&sonnet).refusal_fallback);
    let haiku = AiConfig {
        model: "claude-haiku-4-5-20251001".into(),
        ..on.clone()
    };
    assert!(!Options::for_config(&haiku).refusal_fallback);
    let gateway = AiConfig {
        base_url: "https://llm-gateway.example.com".into(),
        ..on
    };
    assert!(!Options::for_config(&gateway).refusal_fallback);
}

#[test]
#[cfg(unix)] // Windows file-sharing and socket semantics differ; see known issues
fn chunked_stream_cut_after_the_stop_reason_still_finishes() {
    use std::io::{Read as _, Write as _};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut req = [0u8; 8192];
        let _ = sock.read(&mut req);
        let events = concat!(
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":25,\"output_tokens\":1}}}\n\n",
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n",
            "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":15}}\n\n",
        );
        let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
        let chunk = format!("{:x}\r\n{}\r\n", events.len(), events);
        let _ = sock.write_all(head.as_bytes());
        let _ = sock.write_all(chunk.as_bytes());
        let _ = sock.flush();
        // Close without the terminating 0-size chunk (and without message_stop).
        let _ = sock.shutdown(std::net::Shutdown::Both);
    });
    let cfg = AiConfig {
        provider: crate::Provider::Anthropic,
        worker_owner: 0,
        api_key: Some("k".into()),
        model: "claude-opus-5-5".into(),
        max_tokens: 64,
        base_url: format!("http://{addr}"),
        refusal_fallback: false,
    };
    let handle = start_stream(&cfg, request());
    let mut events = Vec::new();
    let deadline = std::time::Instant::now() + WAIT;
    while std::time::Instant::now() < deadline {
        events.extend(handle.poll());
        if events.iter().any(|e: &AiEvent| e.is_terminal()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(events.contains(&AiEvent::Delta("Hi".into())), "{events:?}");
    assert!(
        matches!(events.last(), Some(AiEvent::Done { stop_reason: Some(r), .. }) if r == "end_turn"),
        "{events:?}"
    );
}

#[test]
fn request_body_and_headers() {
    let cfg = AiConfig {
        model: "claude-sonnet-5-5".into(),
        max_tokens: 2048,
        ..AiConfig::default()
    };
    let req = ChatRequest {
        system: "Be brief.".into(),
        messages: vec![
            Message::assistant("dropped"),
            Message::user("a"),
            Message::user("b"),
            Message::assistant("c"),
            Message::user("d"),
        ],
    };
    let body = request_body(&cfg, &req, true, 2048, false);
    assert_eq!(
        body,
        json!({
            "model": "claude-sonnet-5-5",
            "max_tokens": 2048,
            "system": "Be brief.",
            "messages": [
                {"role": "user", "content": "a\n\nb"},
                {"role": "assistant", "content": "c"},
                {"role": "user", "content": "d"}
            ],
            "stream": true
        })
    );
    let body = request_body(
        &cfg,
        &ChatRequest {
            system: "  ".into(),
            messages: vec![Message::user("x")],
        },
        false,
        100,
        true,
    );
    assert_eq!(
        body,
        json!({"model": "claude-sonnet-5-5", "max_tokens": 100, "messages": [{"role": "user", "content": "x"}], "fallbacks": "default"})
    );

    let headers = request_headers("k", true, false);
    assert_eq!(
        headers,
        vec![
            ("x-api-key", "k".to_string()),
            ("anthropic-version", "2023-06-01".to_string()),
            ("content-type", "application/json".to_string()),
            ("accept-encoding", "identity".to_string()),
        ]
    );
    let headers = request_headers("k", false, true);
    assert!(headers.contains(&(
        "anthropic-beta",
        "server-side-fallback-2026-07-01".to_string()
    )));
    assert!(!headers.iter().any(|(name, _)| *name == "accept-encoding"));
}

#[test]
fn retry_delays_and_retryable_statuses() {
    let response = |headers: &str| -> ureq::Response {
        format!("HTTP/1.1 529 Overloaded\r\n{headers}\r\n")
            .parse()
            .unwrap()
    };
    assert_eq!(retry_delay(&response(""), 0), Duration::from_millis(500));
    assert_eq!(retry_delay(&response(""), 1), Duration::from_millis(1000));
    assert_eq!(
        retry_delay(&response("retry-after: 3\r\n"), 0),
        Duration::from_secs(3)
    );
    assert_eq!(
        retry_delay(&response("retry-after: 0\r\n"), 1),
        Duration::ZERO
    );
    assert_eq!(
        retry_delay(&response("retry-after-ms: 250\r\nretry-after: 9\r\n"), 0),
        Duration::from_millis(250)
    );
    assert_eq!(
        retry_delay(&response("retry-after: 1e300\r\n"), 0),
        MAX_RETRY_DELAY
    );
    assert_eq!(
        retry_delay(
            &response("retry-after: Wed, 21 Oct 2026 07:28:00 GMT\r\n"),
            0
        ),
        Duration::from_millis(500)
    );
    for status in [408, 409, 429, 500, 502, 503, 529] {
        assert!(is_retryable(status), "{status}");
    }
    for status in [301, 400, 401, 403, 404, 413] {
        assert!(!is_retryable(status), "{status}");
    }
}

/// Requests follow the IDE's proxy settings (`rustpy_net`); proxy selection
/// itself is tested in `rustpy-net`.
#[test]
fn requests_go_through_the_configured_proxy() {
    let reply = json!({"type": "message", "content": [{"type": "text", "text": "via proxy"}], "stop_reason": "end_turn"});
    // The mock server plays the HTTP proxy: only it can answer for `*.test`.
    let proxy = MockServer::start(vec![json_reply(200, vec![], &reply.to_string())]);
    let port: u16 = proxy.base_url.rsplit(':').next().unwrap().parse().unwrap();
    rustpy_net::configure(rustpy_net::NetConfig {
        proxy: rustpy_net::ProxySettings {
            mode: rustpy_net::ProxyMode::Manual,
            host: "127.0.0.1".into(),
            port,
            ..Default::default()
        },
        ..Default::default()
    });
    let cfg = AiConfig {
        base_url: "http://llm-gateway.proxied.test".into(),
        ..config(&proxy)
    };
    let result = complete(&cfg, request());
    // Other tests only talk to loopback servers, which never use a proxy.
    rustpy_net::configure(rustpy_net::NetConfig::default());
    assert_eq!(result.unwrap(), "via proxy");
    let sent = proxy.next_request();
    assert_eq!(
        sent.path, "http://llm-gateway.proxied.test/v1/messages",
        "absolute-form request to the proxy"
    );
    assert_eq!(sent.header("x-api-key"), Some("test-key"));
}

#[test]
fn parse_reply_collects_text_blocks() {
    let body = json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-opus-5-5",
        "content": [{"type": "thinking", "thinking": "", "signature": "c2ln"}, {"type": "text", "text": "Hello"}, {"type": "text", "text": ", world"}],
        "stop_reason": "end_turn", "usage": {"input_tokens": 7, "output_tokens": 3}
    });
    assert_eq!(
        parse_reply(&body.to_string()),
        Ok(ParsedReply {
            text: "Hello, world".into(),
            stop_reason: Some("end_turn".into()),
            refusal_category: None,
            input_tokens: Some(7),
            output_tokens: Some(3)
        })
    );
    let refusal = json!({"type": "message", "content": [], "stop_reason": "refusal", "stop_details": {"type": "refusal", "category": "cyber"}});
    assert_eq!(
        parse_reply(&refusal.to_string())
            .unwrap()
            .refusal_category
            .as_deref(),
        Some("cyber")
    );
    assert_eq!(
        parse_reply(OVERLOADED),
        Err("API error (overloaded_error): Overloaded".to_string())
    );
    assert!(parse_reply("<html>").unwrap_err().contains("not JSON"));
    assert!(parse_reply("{}").unwrap_err().contains("no `content`"));
}

#[test]
fn requests_are_checked_before_sending() {
    // Nothing listens on this port, so a request that went out would fail differently.
    let cfg = AiConfig {
        base_url: closed_port_url(),
        ..AiConfig::default()
    };
    let opts = Options {
        refusal_fallback: false,
    };

    let events = start_stream_inner(&cfg, None, request(), opts).wait(WAIT);
    assert_eq!(events, vec![AiEvent::Error(NO_KEY.into())]);
    assert_eq!(
        complete_inner(&cfg, None, &request(), opts)
            .unwrap_err()
            .to_string(),
        NO_KEY
    );

    let events = start_stream_inner(&cfg, Some("sk-ant\nxyz".into()), request(), opts).wait(WAIT);
    assert_eq!(events, vec![AiEvent::Error(BAD_KEY.into())]);

    let empty = ChatRequest {
        system: "s".into(),
        messages: vec![Message::assistant("only me")],
    };
    let handle = start_stream_inner(&cfg, Some("k".into()), empty.clone(), opts);
    assert_eq!(handle.poll(), vec![AiEvent::Error(NO_MESSAGES.into())]);
    assert!(handle.is_finished());
    assert!(handle.poll().is_empty());
    assert_eq!(
        complete_inner(&cfg, Some("k".into()), &empty, opts)
            .unwrap_err()
            .to_string(),
        NO_MESSAGES
    );
}

// ---- end to end against the mock server ------------------------------------

#[test]
fn streams_a_chunked_sse_response() {
    let data = RECORDED.as_bytes();
    let e_acute = RECORDED.find('é').unwrap();
    let crab = RECORDED.find('🦀').unwrap();
    let ni_hao = RECORDED.find('你').unwrap();
    let first_delta = RECORDED.find("event: content_block_delta").unwrap();
    let first_ping = RECORDED.find("event: ping").unwrap();
    let first_blank = RECORDED.find("\n\n").unwrap();
    // HTTP chunk boundaries (one read each) inside "é", "🦀" and "你", inside a
    // field name, inside a ping, and between the two newlines that end an event.
    let mut cuts = vec![
        first_blank + 1,
        first_ping + 3,
        first_delta + 9,
        e_acute + 1,
        crab + 1,
        crab + 3,
        ni_hao + 2,
    ];
    cuts.sort_unstable();
    let server = MockServer::start(vec![Reply::Sse(split_at(data, &cuts))]);

    let handle = start_stream(&config(&server), request());
    let events = handle.wait(WAIT);
    assert_eq!(text_of(&events), RECORDED_TEXT);
    assert_eq!(events.last(), Some(&recorded_done()));
    assert_eq!(events.iter().filter(|e| e.is_terminal()).count(), 1);
    assert!(handle.is_finished());
    assert!(handle.poll().is_empty());

    let sent = server.next_request();
    assert_eq!(sent.method, "POST");
    assert_eq!(sent.path, "/v1/messages");
    assert_eq!(sent.header("x-api-key"), Some("test-key"));
    assert_eq!(sent.header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(sent.header("content-type"), Some("application/json"));
    assert_eq!(sent.header("accept-encoding"), Some("identity"));
    assert_eq!(sent.header("anthropic-beta"), None);
    assert_eq!(
        sent.json(),
        json!({
            "model": "claude-opus-5-5",
            "max_tokens": 1024,
            "system": "You are terse.",
            "messages": [{"role": "user", "content": "Say hello"}],
            "stream": true
        })
    );
}

#[test]
fn http_401_surfaces_the_api_error_message() {
    let server = MockServer::start(vec![json_reply(
        401,
        vec![("request-id", "req_011CSHoEeqs5C35K2UUqR7Fy")],
        INVALID_KEY,
    )]);
    let events = start_stream(&config(&server), request()).wait(WAIT);
    let [AiEvent::Error(message)] = events.as_slice() else {
        panic!("{events:?}")
    };
    assert!(message.contains("invalid x-api-key"), "{message}");
    assert!(message.contains("401"), "{message}");
    assert!(message.contains("authentication_error"), "{message}");
    assert!(!message.contains("test-key"), "{message}");
    server.next_request();
    assert_eq!(server.extra_requests(), 0, "a 401 is not retried");
}

#[test]
fn complete_reports_http_errors() {
    let server = MockServer::start(vec![json_reply(401, vec![], INVALID_KEY)]);
    let err = complete(&config(&server), request())
        .unwrap_err()
        .to_string();
    assert!(err.contains("invalid x-api-key"), "{err}");
}

#[test]
fn error_event_mid_stream_is_reported() {
    let server = MockServer::start(vec![Reply::Sse(split_at(
        RECORDED_ERROR.as_bytes(),
        &[17, 300],
    ))]);
    let events = start_stream(&config(&server), request()).wait(WAIT);
    assert_eq!(
        events,
        vec![
            AiEvent::Delta("Partial ünswer".into()),
            AiEvent::Error("API error (overloaded_error): Overloaded".into())
        ]
    );
}

#[test]
fn overloaded_and_rate_limited_responses_are_retried() {
    let rate_limited = r#"{"type":"error","error":{"type":"rate_limit_error","message":"Number of request tokens has exceeded your per-minute rate limit"}}"#;
    let server = MockServer::start(vec![
        json_reply(529, vec![("retry-after", "0")], OVERLOADED),
        json_reply(429, vec![("retry-after-ms", "10")], rate_limited),
        Reply::Sse(vec![RECORDED.as_bytes().to_vec()]),
    ]);
    let events = start_stream(&config(&server), request()).wait(WAIT);
    assert_eq!(text_of(&events), RECORDED_TEXT);
    assert_eq!(events.last(), Some(&recorded_done()));
    for _ in 0..3 {
        assert_eq!(server.next_request().json(), server_body_of_request());
    }
}

fn server_body_of_request() -> serde_json::Value {
    json!({
        "model": "claude-opus-5-5",
        "max_tokens": 1024,
        "system": "You are terse.",
        "messages": [{"role": "user", "content": "Say hello"}],
        "stream": true
    })
}

#[test]
fn retries_are_bounded_and_honor_x_should_retry() {
    let server = MockServer::start(vec![
        json_reply(529, vec![("retry-after", "0")], OVERLOADED),
        json_reply(529, vec![("retry-after", "0")], OVERLOADED),
        json_reply(529, vec![("retry-after", "0")], OVERLOADED),
    ]);
    let events = start_stream(&config(&server), request()).wait(WAIT);
    assert_eq!(
        events,
        vec![AiEvent::Error(
            "API error 529 (overloaded_error): Overloaded".into()
        )]
    );
    for _ in 0..3 {
        server.next_request();
    }

    let server = MockServer::start(vec![json_reply(
        500,
        vec![("x-should-retry", "false")],
        r#"{"type":"error","error":{"type":"api_error","message":"Internal server error"}}"#,
    )]);
    let err = complete(&config(&server), request())
        .unwrap_err()
        .to_string();
    assert_eq!(err, "API error 500 (api_error): Internal server error");
    server.next_request();
    assert_eq!(server.extra_requests(), 0);
}

#[test]
fn cancel_stops_delivery_and_drops_the_connection() {
    // Everything up to and including the first text delta, then pings forever.
    let first_text = RECORDED.find("Héllo").unwrap();
    let cut = first_text + RECORDED[first_text..].find("\n\n").unwrap() + 2;
    let (hung_up_tx, hung_up) = mpsc::channel();
    let server = MockServer::start(vec![Reply::SseUntilHangup {
        chunks: vec![RECORDED.as_bytes()[..cut].to_vec()],
        hung_up: hung_up_tx,
    }]);

    let handle = start_stream(&config(&server), request());
    let deadline = Instant::now() + WAIT;
    let mut events = Vec::new();
    while events.is_empty() {
        assert!(Instant::now() < deadline, "no event arrived");
        events.extend(handle.poll());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(events, vec![AiEvent::Delta("Héllo, ".into())]);
    assert!(!handle.is_finished());

    handle.cancel();
    assert!(handle.is_cancelled());
    assert!(handle.is_finished());
    hung_up
        .recv_timeout(Duration::from_secs(5))
        .expect("the client closed the connection after cancel()");
    assert!(handle.poll().is_empty());
    assert!(handle.wait(Duration::from_millis(50)).is_empty());
}

#[test]
fn dropping_the_handle_drops_the_connection() {
    let (hung_up_tx, hung_up) = mpsc::channel();
    let start = RECORDED.find("event: content_block_start").unwrap();
    let server = MockServer::start(vec![Reply::SseUntilHangup {
        chunks: vec![RECORDED.as_bytes()[..start].to_vec()],
        hung_up: hung_up_tx,
    }]);
    let handle = start_stream(&config(&server), request());
    server.next_request();
    drop(handle);
    hung_up
        .recv_timeout(Duration::from_secs(5))
        .expect("the client closed the connection after the handle was dropped");
}

#[test]
fn complete_returns_the_text_and_caps_max_tokens() {
    let reply = json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-opus-5-5",
        "content": [{"type": "thinking", "thinking": "", "signature": "c2ln"}, {"type": "text", "text": "feat(ai): add "}, {"type": "text", "text": "a client"}],
        "stop_reason": "end_turn", "usage": {"input_tokens": 10, "output_tokens": 5}
    });
    let server = MockServer::start(vec![json_reply(200, vec![], &reply.to_string())]);
    let cfg = AiConfig {
        max_tokens: 64_000,
        ..config(&server)
    };
    assert_eq!(complete(&cfg, request()).unwrap(), "feat(ai): add a client");

    let sent = server.next_request();
    let body = sent.json();
    assert_eq!(body["max_tokens"], 16_000);
    assert_eq!(body.get("stream"), None);
    assert_eq!(
        body["messages"],
        json!([{"role": "user", "content": "Say hello"}])
    );
    assert_eq!(sent.header("x-api-key"), Some("test-key"));
    assert_eq!(sent.header("anthropic-version"), Some("2023-06-01"));
}

#[test]
fn complete_reports_refusals_and_empty_replies() {
    let refusal = json!({"type": "message", "content": [], "stop_reason": "refusal", "stop_details": {"type": "refusal", "category": "cyber", "explanation": null}});
    let truncated = json!({"type": "message", "content": [{"type": "thinking", "thinking": "", "signature": "c2ln"}], "stop_reason": "max_tokens"});
    let server = MockServer::start(vec![
        json_reply(200, vec![], &refusal.to_string()),
        json_reply(200, vec![], &truncated.to_string()),
    ]);
    let err = complete(&config(&server), request())
        .unwrap_err()
        .to_string();
    assert_eq!(err, "The model declined this request (category: cyber).");
    let err = complete(&config(&server), request())
        .unwrap_err()
        .to_string();
    assert!(err.contains("max_tokens"), "{err}");
}

#[test]
fn a_json_reply_to_a_stream_request_is_still_delivered() {
    let reply = json!({"type": "message", "content": [{"type": "text", "text": "Whole answer"}], "stop_reason": "end_turn", "usage": {"input_tokens": 4, "output_tokens": 2}});
    let server = MockServer::start(vec![json_reply(200, vec![], &reply.to_string())]);
    let events = start_stream(&config(&server), request()).wait(WAIT);
    assert_eq!(
        events,
        vec![
            AiEvent::Delta("Whole answer".into()),
            AiEvent::Done {
                stop_reason: Some("end_turn".into()),
                input_tokens: Some(4),
                output_tokens: Some(2)
            },
        ]
    );
}

#[test]
fn rejected_refusal_fallback_opt_in_is_dropped() {
    let rejected = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Unexpected value(s) `server-side-fallback-2026-07-01` for the `anthropic-beta` header."}}"#;
    let server = MockServer::start(vec![
        json_reply(400, vec![], rejected),
        Reply::Sse(vec![RECORDED.as_bytes().to_vec()]),
    ]);
    let cfg = config(&server);
    let events = start_stream_inner(
        &cfg,
        cfg.resolved_key(),
        request(),
        Options {
            refusal_fallback: true,
        },
    )
    .wait(WAIT);
    assert_eq!(text_of(&events), RECORDED_TEXT);

    let first = server.next_request();
    assert_eq!(
        first.header("anthropic-beta"),
        Some("server-side-fallback-2026-07-01")
    );
    assert_eq!(first.json()["fallbacks"], "default");
    let second = server.next_request();
    assert_eq!(second.header("anthropic-beta"), None);
    assert_eq!(second.json().get("fallbacks"), None);
}

#[test]
fn an_unreachable_server_is_reported() {
    let cfg = AiConfig {
        api_key: Some("test-key".into()),
        base_url: closed_port_url(),
        ..AiConfig::default()
    };
    let events = start_stream(&cfg, request()).wait(WAIT);
    let [AiEvent::Error(message)] = events.as_slice() else {
        panic!("{events:?}")
    };
    assert!(message.starts_with("Could not reach the API"), "{message}");
    let err = complete(&cfg, request()).unwrap_err().to_string();
    assert!(err.starts_with("Could not reach the API"), "{err}");
}
