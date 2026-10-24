//! Messages API client over HTTPS with `ureq`: streaming ([`start_stream`]) and
//! blocking ([`complete`]) requests to `POST {base_url}/v1/messages`. Retries,
//! proxy handling and the refusal-fallback opt-in are described in the crate
//! docs ("Requests").

use std::io::{ErrorKind, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use serde_json::{json, Map, Value};

use crate::api_error;
use crate::config::AiConfig;
use crate::sse::{SseParser, StreamDecoder};
use crate::types::{normalize_messages, AiEvent, ChatRequest};

#[cfg(test)]
mod tests;

/// Value sent in the `anthropic-version` header.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// `max_tokens` cap for [`complete`]: a non-streaming reply arrives only when it
/// is finished, so it is kept short enough to stay well within HTTP timeouts.
pub const COMPLETE_MAX_TOKENS: u32 = 16_000;

const REFUSAL_FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// Models that support `"fallbacks": "default"` on the first-party API.
const REFUSAL_FALLBACK_MODELS: &[&str] = &[
    "claude-opus-5-5",
    "claude-sonnet-5-5",
    "claude-opus-5",
    "claude-fable-5-1",
    "claude-fable-5",
];

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// Longest silence tolerated between two reads (the API sends pings while it works).
const READ_TIMEOUT: Duration = Duration::from_secs(600);
const WRITE_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_RETRIES: u32 = 2;
const MAX_RETRY_DELAY: Duration = Duration::from_secs(20);
const MAX_ERROR_BODY: u64 = 256 * 1024;
const MAX_MESSAGE_BODY: u64 = 32 * 1024 * 1024;
const READ_CHUNK: usize = 16 * 1024;

const NO_KEY: &str = "No Anthropic API key is configured. Add one in the AI Assistant settings or set the ANTHROPIC_API_KEY environment variable.";
const BAD_KEY: &str = "The API key contains characters that are not allowed (check for spaces, quotes or line breaks pasted with it).";
const NO_MESSAGES: &str = "Nothing to send: the request has no user message.";
const THREAD_GONE: &str = "The AI request stopped unexpectedly.";

/// Per-request switches derived from the config (separate so tests can force them).
#[derive(Clone, Copy, Debug)]
struct Options {
    refusal_fallback: bool,
}

impl Options {
    fn for_config(cfg: &AiConfig) -> Self {
        Options {
            // Opt-in only: the beta header is sent when the user enables it.
            refusal_fallback: cfg.refusal_fallback
                && cfg.is_first_party()
                && REFUSAL_FALLBACK_MODELS.contains(&cfg.effective_model()),
        }
    }
}

/// A reply being streamed on a background thread.
///
/// Call [`poll`](Self::poll) from the UI loop (it never blocks) and append the
/// [`AiEvent::Delta`] texts; the stream ends with one `Done` or `Error` event.
/// [`cancel`](Self::cancel) (or dropping the handle) stops it: no further events
/// are delivered, and the reader thread closes the connection as soon as the
/// next chunk arrives.
#[derive(Debug)]
#[must_use = "dropping a StreamHandle cancels the request"]
pub struct StreamHandle {
    pub(crate) rx: Receiver<AiEvent>,
    pub(crate) worker_id: Option<u64>,
    pub(crate) cancel: Arc<AtomicBool>,
    pub(crate) finished: AtomicBool,
}

impl StreamHandle {
    /// A handle that reports `message` without making a request.
    pub(crate) fn failed(message: &str) -> Self {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let _ = tx.send(AiEvent::Error(message.to_string()));
        StreamHandle {
            rx,
            worker_id: None,
            cancel: Arc::new(AtomicBool::new(false)),
            finished: AtomicBool::new(false),
        }
    }

    /// The events received since the last call, without blocking. Empty once the
    /// stream has finished or was cancelled.
    pub fn poll(&self) -> Vec<AiEvent> {
        let mut out = Vec::new();
        while !self.is_finished() {
            match self.rx.try_recv() {
                Ok(event) => self.accept(event, &mut out),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.accept(AiEvent::Error(THREAD_GONE.to_string()), &mut out)
                }
            }
        }
        out
    }

    /// Blocks until the stream finishes or `timeout` elapses, returning the
    /// events received meanwhile (for callers without a UI loop, and tests).
    pub fn wait(&self, timeout: Duration) -> Vec<AiEvent> {
        let deadline = Instant::now() + timeout.min(Duration::from_secs(365 * 24 * 3600));
        let mut out = Vec::new();
        while !self.is_finished() {
            match self
                .rx
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(event) => self.accept(event, &mut out),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    self.accept(AiEvent::Error(THREAD_GONE.to_string()), &mut out)
                }
            }
        }
        out
    }

    /// Stops the stream. Events not yet polled are discarded.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    /// Actual network worker, retained in the registry after delivery cancellation.
    pub fn worker_id(&self) -> Option<u64> {
        self.worker_id
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// `true` after the terminal event was delivered, or after [`cancel`](Self::cancel).
    pub fn is_finished(&self) -> bool {
        self.is_cancelled() || self.finished.load(Ordering::SeqCst)
    }

    fn accept(&self, event: AiEvent, out: &mut Vec<AiEvent>) {
        if event.is_terminal() {
            self.finished.store(true, Ordering::SeqCst);
        }
        out.push(event);
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Starts streaming a reply to `req`: a background thread POSTs to
/// `{base_url}/v1/messages` with `"stream": true` and parses the Server-Sent
/// Events as they arrive. Problems (no API key, HTTP or network errors, an
/// `error` event) are reported as an [`AiEvent::Error`] through the handle.
pub fn start_stream(cfg: &AiConfig, req: ChatRequest) -> StreamHandle {
    if cfg.provider == crate::Provider::OpenAiCompatible {
        return crate::compatible::start(cfg, req);
    }
    start_stream_inner(cfg, cfg.resolved_key(), req, Options::for_config(cfg))
}

fn start_stream_inner(
    cfg: &AiConfig,
    key: Option<String>,
    req: ChatRequest,
    opts: Options,
) -> StreamHandle {
    let key = match check_request(key, &req) {
        Ok(key) => key,
        Err(message) => return StreamHandle::failed(message),
    };
    let (tx, rx) = crossbeam_channel::unbounded();
    let cancel = Arc::new(AtomicBool::new(false));
    let thread_cancel = Arc::clone(&cancel);
    let worker =
        crate::workers::Guard::register(cfg.worker_owner, "Anthropic stream", cancel.clone());
    let worker_id = Some(worker.id);
    let cfg = cfg.clone();
    let spawned = std::thread::Builder::new()
        .name("rustpy-ai-stream".into())
        .spawn(move || {
            let _worker = worker;
            run_stream(&cfg, &key, &req, opts, &thread_cancel, &tx);
        });
    if let Err(err) = spawned {
        return StreamHandle::failed(&format!("Could not start the AI request: {err}"));
    }
    StreamHandle {
        rx,
        worker_id,
        cancel,
        finished: AtomicBool::new(false),
    }
}

/// Sends `req` without streaming and returns the reply's text, blocking until
/// it is complete; meant for short tasks such as commit messages. `max_tokens`
/// is capped at [`COMPLETE_MAX_TOKENS`]. A reply that the model declined
/// (`stop_reason: "refusal"`) is an error.
pub fn complete(cfg: &AiConfig, req: ChatRequest) -> anyhow::Result<String> {
    if cfg.provider == crate::Provider::OpenAiCompatible {
        return crate::compatible::complete(cfg, &req);
    }
    complete_inner(cfg, cfg.resolved_key(), &req, Options::for_config(cfg))
}

fn complete_inner(
    cfg: &AiConfig,
    key: Option<String>,
    req: &ChatRequest,
    opts: Options,
) -> anyhow::Result<String> {
    let key = check_request(key, req).map_err(anyhow::Error::msg)?;
    let max_tokens = cfg.effective_max_tokens().min(COMPLETE_MAX_TOKENS);
    let worker = crate::workers::Guard::register(
        cfg.worker_owner,
        "Anthropic complete",
        Arc::new(AtomicBool::new(false)),
    );
    worker.check()?;
    let response = match send(
        cfg,
        &key,
        req,
        false,
        max_tokens,
        opts.refusal_fallback,
        &worker.cancel,
    ) {
        Ok(response) => response,
        Err(Failure::Message(message)) => bail!(message),
        Err(Failure::Cancelled) => bail!("The request was cancelled."),
    };
    let body = read_body(
        crate::workers::Reader {
            inner: response.into_reader(),
            cancel: worker.cancel.clone(),
        },
        MAX_MESSAGE_BODY,
    )
    .map_err(|e| anyhow!("The connection was interrupted: {e}"))?;
    worker.check()?;
    let reply = parse_reply(&body).map_err(anyhow::Error::msg)?;
    let no_text = reply.text.trim().is_empty();
    match reply.stop_reason.as_deref() {
        Some("refusal") => {
            let category = reply
                .refusal_category
                .map(|c| format!(" (category: {c})"))
                .unwrap_or_default();
            bail!("The model declined this request{category}.")
        }
        Some("max_tokens") if no_text => bail!(
            "The reply reached the max_tokens limit ({max_tokens}) before any text was written."
        ),
        _ if no_text => bail!("The reply contained no text."),
        _ => Ok(reply.text),
    }
}

/// Validates the key and the conversation before anything is sent.
fn check_request(key: Option<String>, req: &ChatRequest) -> Result<String, &'static str> {
    let key = key.ok_or(NO_KEY)?;
    if !key.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(BAD_KEY);
    }
    if normalize_messages(&req.messages).is_empty() {
        return Err(NO_MESSAGES);
    }
    Ok(key)
}

fn run_stream(
    cfg: &AiConfig,
    key: &str,
    req: &ChatRequest,
    opts: Options,
    cancel: &AtomicBool,
    tx: &Sender<AiEvent>,
) {
    // Nothing is delivered after cancel(); a failed send means the handle is gone.
    let emit = |event: AiEvent| !cancel.load(Ordering::SeqCst) && tx.send(event).is_ok();
    let response = match send(
        cfg,
        key,
        req,
        true,
        cfg.effective_max_tokens(),
        opts.refusal_fallback,
        cancel,
    ) {
        Ok(response) => response,
        Err(Failure::Cancelled) => return,
        Err(Failure::Message(message)) => {
            emit(AiEvent::Error(message));
            return;
        }
    };
    let content_type = response.content_type().to_ascii_lowercase();
    if content_type.contains("json") {
        // The endpoint ignored `"stream": true` (some gateways do) and sent the whole message.
        let parsed = read_body(response.into_reader(), MAX_MESSAGE_BODY)
            .map_err(|e| format!("The connection was interrupted: {e}"))
            .and_then(|body| parse_reply(&body));
        let mut events = Vec::new();
        match parsed {
            Ok(reply) => {
                if !reply.text.is_empty() {
                    events.push(AiEvent::Delta(reply.text));
                }
                events.push(AiEvent::Done {
                    stop_reason: reply.stop_reason,
                    input_tokens: reply.input_tokens,
                    output_tokens: reply.output_tokens,
                });
            }
            Err(message) => events.push(AiEvent::Error(message)),
        }
        for event in events {
            if !emit(event) {
                return;
            }
        }
        return;
    }
    if content_type.contains("html") {
        emit(AiEvent::Error(format!(
            "The API endpoint {} answered with a web page instead of an event stream; check the API base URL and the network proxy.",
            cfg.messages_url()
        )));
        return;
    }

    let mut reader = response.into_reader();
    let mut parser = SseParser::new();
    let mut decoder = StreamDecoder::new();
    let mut buf = vec![0u8; READ_CHUNK];
    let mut read_error: Option<std::io::Error> = None;
    loop {
        // Checked between chunks: returning drops `reader`, which closes the connection.
        if cancel.load(Ordering::SeqCst) {
            return;
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            // A chunked body cut short (no terminating 0-size chunk) surfaces as a
            // read error; finish like an early EOF so a reply whose stop reason
            // already arrived still ends with `Done`.
            Err(err) => {
                read_error = Some(err);
                break;
            }
        };
        for sse in parser.feed(&buf[..n]) {
            if let Some(event) = decoder.handle(&sse) {
                let terminal = event.is_terminal();
                if !emit(event) || terminal {
                    return;
                }
            }
        }
    }
    let last = parser.finish().and_then(|sse| decoder.handle(&sse));
    for mut event in last.into_iter().chain(decoder.finish()) {
        if let (AiEvent::Error(message), Some(err)) = (&mut event, &read_error) {
            *message =
                format!("The connection was interrupted before the reply was complete ({err}).");
        }
        if !emit(event) {
            return;
        }
    }
}

enum Failure {
    /// User-facing error message.
    Message(String),
    Cancelled,
}

/// POSTs the request, retrying retryable failures; returns a 2xx response.
fn send(
    cfg: &AiConfig,
    key: &str,
    req: &ChatRequest,
    stream: bool,
    max_tokens: u32,
    mut refusal_fallback: bool,
    cancel: &AtomicBool,
) -> Result<ureq::Response, Failure> {
    let url = cfg.messages_url();
    let agent = agent_for(&url);
    let mut retries = 0;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(Failure::Cancelled);
        }
        let body = request_body(cfg, req, stream, max_tokens, refusal_fallback).to_string();
        let mut request = agent.post(&url);
        for (name, value) in request_headers(key, stream, refusal_fallback) {
            request = request.set(name, &value);
        }
        log::debug!(
            "rustpy-ai: POST {url} (model {}, stream {stream}, {} bytes)",
            cfg.effective_model(),
            body.len()
        );
        let (status, response) = match request.send_string(&body) {
            Ok(response) if (200..300).contains(&response.status()) => return Ok(response),
            Ok(response) => (response.status(), response),
            Err(ureq::Error::Status(status, response)) => (status, response),
            Err(ureq::Error::Transport(err)) => {
                // A bad-header message would echo the header line, i.e. the API key.
                let message = if err.kind() == ureq::ErrorKind::BadHeader {
                    BAD_KEY.to_string()
                } else {
                    // Proxy and certificate failures get explicit wording.
                    format!(
                        "Could not reach the API: {}",
                        rustpy_net::describe_transport_error(&err)
                    )
                };
                return Err(Failure::Message(message));
            }
        };
        let delay = retry_delay(&response, retries);
        let should_retry = response
            .header("x-should-retry")
            .map(|v| v.trim().eq_ignore_ascii_case("true"));
        let request_id = response.header("request-id").unwrap_or("-").to_string();
        let status_text = response.status_text().to_string();
        let text = read_body(response.into_reader(), MAX_ERROR_BODY).unwrap_or_default();
        log::warn!("rustpy-ai: HTTP {status} from {url} (request-id {request_id})");
        if refusal_fallback
            && status == 400
            && (text.contains("anthropic-beta") || text.contains("fallback"))
        {
            log::info!("rustpy-ai: the API rejected the refusal-fallback opt-in; sending the request without it");
            refusal_fallback = false;
            continue;
        }
        if retries < MAX_RETRIES && should_retry.unwrap_or_else(|| is_retryable(status)) {
            retries += 1;
            log::info!("rustpy-ai: retrying in {delay:?} ({retries}/{MAX_RETRIES})");
            if !sleep_unless_cancelled(delay, cancel) {
                return Err(Failure::Cancelled);
            }
            continue;
        }
        return Err(Failure::Message(api_error::http_error_message(
            status,
            &status_text,
            &text,
        )));
    }
}

/// The JSON body: `{model, max_tokens, system, messages, stream}` (plus
/// `fallbacks` when opting into refusal fallback). A blank system prompt is
/// left out and the conversation is normalized (see `normalize_messages`).
fn request_body(
    cfg: &AiConfig,
    req: &ChatRequest,
    stream: bool,
    max_tokens: u32,
    refusal_fallback: bool,
) -> Value {
    let messages: Vec<Value> = normalize_messages(&req.messages)
        .into_iter()
        .map(|m| json!({ "role": m.role.as_str(), "content": m.content }))
        .collect();
    let mut body = Map::new();
    body.insert("model".into(), json!(cfg.effective_model()));
    body.insert("max_tokens".into(), json!(max_tokens));
    if !req.system.trim().is_empty() {
        body.insert("system".into(), json!(req.system));
    }
    body.insert("messages".into(), Value::Array(messages));
    if stream {
        body.insert("stream".into(), Value::Bool(true));
    }
    if refusal_fallback {
        body.insert("fallbacks".into(), json!("default"));
    }
    Value::Object(body)
}

fn request_headers(key: &str, stream: bool, refusal_fallback: bool) -> Vec<(&'static str, String)> {
    let mut headers = vec![
        ("x-api-key", key.to_string()),
        ("anthropic-version", ANTHROPIC_VERSION.to_string()),
        ("content-type", "application/json".to_string()),
    ];
    if stream {
        // ureq would ask for gzip otherwise; a compressed event stream can hold chunks back.
        headers.push(("accept-encoding", "identity".to_string()));
    }
    if refusal_fallback {
        headers.push(("anthropic-beta", REFUSAL_FALLBACK_BETA.to_string()));
    }
    headers
}

/// The agent for `url`. Proxy and certificate trust come from the IDE's network
/// settings (`rustpy_net`: Settings › HTTP Proxy and Server Certificates; the
/// environment's `HTTPS_PROXY` / `NO_PROXY` in the default auto-detect mode).
fn agent_for(url: &str) -> ureq::Agent {
    rustpy_net::agent_builder(url)
        .timeout_connect(CONNECT_TIMEOUT)
        .timeout_read(READ_TIMEOUT)
        .timeout_write(WRITE_TIMEOUT)
        .redirects(0)
        .user_agent(&format!("rustpy-ai/{}", env!("CARGO_PKG_VERSION")))
        .build()
}

fn is_retryable(status: u16) -> bool {
    matches!(status, 408 | 409 | 429 | 500..=599)
}

/// `retry-after-ms` / `retry-after` (seconds) when present, capped at
/// [`MAX_RETRY_DELAY`]; otherwise exponential backoff from 0.5 s.
fn retry_delay(response: &ureq::Response, retries: u32) -> Duration {
    let header_secs = |name: &str, scale: f64| {
        let value = response.header(name)?.trim().parse::<f64>().ok()?;
        (value.is_finite() && value >= 0.0)
            .then(|| Duration::from_secs_f64((value * scale).min(MAX_RETRY_DELAY.as_secs_f64())))
    };
    header_secs("retry-after-ms", 0.001)
        .or_else(|| header_secs("retry-after", 1.0))
        .unwrap_or_else(|| Duration::from_millis(500u64 << retries.min(5)))
}

/// Sleeps for `delay`; returns `false` early if the request is cancelled meanwhile.
fn sleep_unless_cancelled(delay: Duration, cancel: &AtomicBool) -> bool {
    let deadline = Instant::now() + delay;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return false;
        }
        let now = Instant::now();
        if now >= deadline {
            return true;
        }
        std::thread::sleep((deadline - now).min(Duration::from_millis(50)));
    }
}

fn read_body(reader: impl Read, limit: u64) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    reader.take(limit).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The parts of a non-streaming Messages API reply the client uses.
#[derive(Debug, Default, PartialEq)]
struct ParsedReply {
    text: String,
    stop_reason: Option<String>,
    refusal_category: Option<String>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

/// Parses a non-streaming reply; its text is the concatenation of the `text`
/// content blocks (thinking and other blocks are skipped).
fn parse_reply(body: &str) -> Result<ParsedReply, String> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| format!("Unexpected response from the API (not JSON: {e})."))?;
    if v.get("type").and_then(Value::as_str) == Some("error") {
        return Err(api_error::stream_error_message(body));
    }
    let content = v
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| "Unexpected response from the API (no `content`).".to_string())?;
    Ok(ParsedReply {
        text: content
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect(),
        stop_reason: v["stop_reason"].as_str().map(str::to_owned),
        refusal_category: v
            .pointer("/stop_details/category")
            .and_then(Value::as_str)
            .map(str::to_owned),
        input_tokens: v.pointer("/usage/input_tokens").and_then(Value::as_u64),
        output_tokens: v.pointer("/usage/output_tokens").and_then(Value::as_u64),
    })
}
