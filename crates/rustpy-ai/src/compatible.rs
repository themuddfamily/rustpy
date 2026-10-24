//! Configured OpenAI-compatible Chat Completions transport. No model discovery
//! or implicit provider/model migration; users supply the endpoint and model.
use crate::{AiConfig, AiEvent, ChatRequest, StreamHandle};
use anyhow::{bail, Context};
use serde_json::{json, Value};
use std::{
    io::Read,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

fn endpoint(cfg: &AiConfig) -> anyhow::Result<String> {
    let base = cfg.base_url.trim().trim_end_matches('/');
    if cfg.model.trim().is_empty() {
        bail!("Configure the compatible provider model ID");
    }
    if !(base.starts_with("https://")
        || base.starts_with("http://127.0.0.1:")
        || base.starts_with("http://[::1]:"))
    {
        bail!("Use an HTTPS API endpoint, or HTTP on loopback for a local provider");
    }
    if base.contains('@') || base.contains('?') || base.contains('#') {
        bail!("API endpoint must not contain credentials, query or fragment");
    }
    Ok(if base.ends_with("/chat/completions") {
        base.into()
    } else {
        format!("{base}/chat/completions")
    })
}
fn body(cfg: &AiConfig, req: &ChatRequest, stream: bool) -> Value {
    let mut messages = Vec::new();
    if !req.system.trim().is_empty() {
        messages.push(json!({"role":"system","content":req.system}));
    }
    messages.extend(
        req.messages
            .iter()
            .map(|m| json!({"role":m.role.as_str(),"content":m.content})),
    );
    let mut value = json!({"model":cfg.model.trim(),"messages":messages,"stream":stream});
    if cfg.max_tokens > 0 {
        value["max_completion_tokens"] = json!(cfg.max_tokens);
    }
    value
}
fn send(cfg: &AiConfig, req: &ChatRequest, stream: bool) -> anyhow::Result<ureq::Response> {
    let url = endpoint(cfg)?;
    let key = cfg
        .resolved_key()
        .context("Configure the compatible API key, or OPENAI_API_KEY")?;
    if !key.bytes().all(|b| b.is_ascii_graphic()) {
        bail!("Invalid API key characters");
    }
    if req.messages.is_empty() {
        bail!("Nothing to send");
    }
    let agent = rustpy_net::agent_builder(&url)
        .redirects(0)
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(45))
        .timeout_write(Duration::from_secs(30))
        .build();
    match agent.post(&url).set("Authorization",&format!("Bearer {key}")).set("Content-Type","application/json").set("Accept-Encoding","identity").send_string(&body(cfg,req,stream).to_string()){
        Ok(response)=>Ok(response),
        Err(ureq::Error::Status(status,_))=>bail!("Compatible provider returned HTTP {status}; check endpoint, model, key and provider quota"),
        Err(ureq::Error::Transport(_))=>bail!("Could not reach compatible provider; check network/proxy/certificate settings"),
    }
}
fn reply(value: &Value) -> anyhow::Result<String> {
    if let Some(refusal) = value
        .pointer("/choices/0/message/refusal")
        .and_then(Value::as_str)
    {
        bail!("Provider declined: {refusal}");
    }
    value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .context("Compatible response has no message content")
}
pub(crate) fn complete(cfg: &AiConfig, req: &ChatRequest) -> anyhow::Result<String> {
    let worker = crate::workers::Guard::register(
        cfg.worker_owner,
        "Compatible complete",
        Arc::new(AtomicBool::new(false)),
    );
    worker.check()?;
    let mut data = String::new();
    let response = send(cfg, req, false)?;
    worker.check()?;
    crate::workers::Reader {
        inner: response.into_reader(),
        cancel: worker.cancel.clone(),
    }
    .take(8 * 1024 * 1024 + 1)
    .read_to_string(&mut data)?;
    if data.len() > 8 * 1024 * 1024 {
        bail!("Provider response exceeds 8 MiB");
    }
    worker.check()?;
    reply(&serde_json::from_str::<Value>(&data)?)
}
pub(crate) fn start(cfg: &AiConfig, req: ChatRequest) -> StreamHandle {
    if let Err(err) = endpoint(cfg) {
        return StreamHandle::failed(&err.to_string());
    }
    let (tx, rx) = crossbeam_channel::bounded(256);
    let cancel = Arc::new(AtomicBool::new(false));
    let stopped = cancel.clone();
    let worker =
        crate::workers::Guard::register(cfg.worker_owner, "Compatible stream", cancel.clone());
    let worker_id = Some(worker.id);
    let cfg = cfg.clone();
    if let Err(err) = std::thread::Builder::new()
        .name("compatible-ai".into())
        .spawn(move || {
            let _worker = worker;
            let emit = |event| {
                let mut event = event;
                loop {
                    if stopped.load(Ordering::Acquire) {
                        return false;
                    }
                    match tx.send_timeout(event, Duration::from_millis(100)) {
                        Ok(()) => return true,
                        Err(crossbeam_channel::SendTimeoutError::Timeout(next)) => event = next,
                        Err(_) => return false,
                    }
                }
            };
            let result = (|| -> anyhow::Result<()> {
                _worker.check()?;
                let response = send(&cfg, &req, true)?;
                _worker.check()?;
                if response.content_type().contains("json") {
                    let mut data = String::new();
                    crate::workers::Reader {
                        inner: response.into_reader(),
                        cancel: stopped.clone(),
                    }
                    .take(8 * 1024 * 1024 + 1)
                    .read_to_string(&mut data)?;
                    if data.len() > 8 * 1024 * 1024 {
                        bail!("Provider response exceeds limit");
                    }
                    emit(AiEvent::Delta(reply(&serde_json::from_str::<Value>(
                        &data,
                    )?)?));
                } else {
                    let mut parser = crate::sse::SseParser::new();
                    let mut reader = crate::workers::Reader {
                        inner: response.into_reader(),
                        cancel: stopped.clone(),
                    };
                    let mut chunk = [0; 8192];
                    let mut bytes = 0usize;
                    let mut done = false;
                    while !stopped.load(Ordering::Acquire) {
                        let n = reader.read(&mut chunk)?;
                        if n == 0 {
                            break;
                        }
                        bytes += n;
                        if bytes > 32 * 1024 * 1024 {
                            bail!("Provider stream exceeds limit");
                        }
                        for event in parser.feed(&chunk[..n]) {
                            if event.data.trim() == "[DONE]" {
                                done = true;
                                break;
                            }
                            let value: Value = serde_json::from_str(&event.data)?;
                            if value.get("error").is_some() {
                                bail!("Provider returned a streaming error");
                            }
                            if let Some(refusal) = value
                                .pointer("/choices/0/delta/refusal")
                                .and_then(Value::as_str)
                            {
                                bail!("Provider declined: {refusal}");
                            }
                            if let Some(text) = value
                                .pointer("/choices/0/delta/content")
                                .and_then(Value::as_str)
                            {
                                if !emit(AiEvent::Delta(text.into())) {
                                    return Ok(());
                                }
                            }
                        }
                        if done {
                            break;
                        }
                    }
                    if !done && !stopped.load(Ordering::Acquire) {
                        bail!("Provider stream ended before [DONE]");
                    }
                }
                if !stopped.load(Ordering::Acquire) {
                    emit(AiEvent::Done {
                        stop_reason: Some("stop".into()),
                        input_tokens: None,
                        output_tokens: None,
                    });
                }
                Ok(())
            })();
            if let Err(err) = result {
                emit(AiEvent::Error(err.to_string()));
            }
        })
    {
        return StreamHandle::failed(&format!("Cannot start provider request: {err}"));
    }
    StreamHandle {
        rx,
        worker_id,
        cancel,
        finished: AtomicBool::new(false),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_model_and_secure_endpoint() {
        let mut cfg = AiConfig {
            provider: crate::Provider::OpenAiCompatible,
            model: String::new(),
            base_url: "https://api.openai.com/v1".into(),
            ..Default::default()
        };
        assert!(endpoint(&cfg).is_err());
        cfg.model = "configured-model".into();
        assert_eq!(
            endpoint(&cfg).unwrap(),
            "https://api.openai.com/v1/chat/completions"
        );
        cfg.base_url = "http://remote.example/v1".into();
        assert!(endpoint(&cfg).is_err());
    }
    #[test]
    fn actual_wire_roles_and_content() {
        let cfg = AiConfig {
            model: "configured".into(),
            ..Default::default()
        };
        let req = ChatRequest {
            system: "system".into(),
            messages: vec![crate::Message::user("question")],
        };
        let value = body(&cfg, &req, true);
        assert_eq!(value["messages"][0]["role"], "system");
        assert_eq!(
            reply(&json!({"choices":[{"message":{"content":"answer"}}]})).unwrap(),
            "answer"
        );
        assert!(reply(&json!({"choices":[{"message":{"refusal":"no"}}]})).is_err());
    }
}

#[cfg(test)]
mod transport_fixtures {
    use super::*;
    use crate::mock_server::{MockServer, Reply};
    #[test]
    fn real_blocking_and_sse_protocol_use_selected_provider() {
        let server = MockServer::start(vec![
            Reply::Full {
                status: 200,
                content_type: "application/json",
                headers: vec![],
                body: r#"{"choices":[{"message":{"content":"answer"}}]}"#.into(),
            },
            Reply::Sse(vec![
                b"data: {\"choices\":[{\"delta\":{\"content\":\"streamed\"}}]}\n\ndata: [DONE]\n\n"
                    .to_vec(),
            ]),
        ]);
        let cfg = AiConfig {
            provider: crate::Provider::OpenAiCompatible,
            base_url: format!("{}/v1", server.base_url),
            model: "explicit-configured-model".into(),
            api_key: Some("fixture-token".into()),
            ..Default::default()
        };
        let request = ChatRequest {
            system: "policy".into(),
            messages: vec![crate::Message::user("question")],
        };
        assert_eq!(crate::complete(&cfg, request.clone()).unwrap(), "answer");
        let sent = server.next_request();
        assert_eq!(sent.path, "/v1/chat/completions");
        assert_eq!(sent.header("authorization"), Some("Bearer fixture-token"));
        assert!(sent.header("x-api-key").is_none());
        assert_eq!(sent.json()["model"], "explicit-configured-model");
        let stream = crate::start_stream(&cfg, request);
        let events = stream.wait(Duration::from_secs(3));
        assert!(events.contains(&AiEvent::Delta("streamed".into())));
        assert!(events.iter().any(|e| matches!(e, AiEvent::Done { .. })));
    }
}
