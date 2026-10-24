//! Incremental Server-Sent Events parsing ([`SseParser`]) and decoding of the
//! Messages API stream events into [`AiEvent`]s ([`StreamDecoder`]).
//!
//! The parser works on raw bytes and only decodes complete lines, so reads that
//! split a line, a `\r\n` pair or a multi-byte UTF-8 character are handled.

use std::mem;

use serde_json::Value;

use crate::api_error;
use crate::types::AiEvent;

/// One dispatched SSE event.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SseEvent {
    /// The `event:` field; `"message"` when the event had none.
    pub event: String,
    /// The `data:` lines joined with `\n`.
    pub data: String,
}

/// Incremental parser for a `text/event-stream` body (WHATWG HTML §9.2).
///
/// Feed it bytes as they arrive; it returns the events completed by each chunk.
/// Lines may end in `\n`, `\r\n` or `\r`; comments, `id:` and `retry:` lines are
/// ignored; a leading byte-order mark is skipped.
#[derive(Debug, Default)]
pub struct SseParser {
    /// Bytes of the current, not yet terminated line.
    line: Vec<u8>,
    /// The previous byte was a `\r`, so a following `\n` belongs to the same line ending.
    after_cr: bool,
    /// At least one line has been processed (the BOM check is done).
    started: bool,
    event: String,
    data: String,
    has_data: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Consumes the next chunk of the stream and returns the events it completes.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        let mut out = Vec::new();
        let mut start = 0;
        for (i, &b) in bytes.iter().enumerate() {
            if self.after_cr {
                self.after_cr = false;
                if b == b'\n' {
                    start = i + 1;
                    continue;
                }
            }
            if b == b'\n' || b == b'\r' {
                self.line.extend_from_slice(&bytes[start..i]);
                self.end_line(&mut out);
                self.after_cr = b == b'\r';
                start = i + 1;
            }
        }
        self.line.extend_from_slice(&bytes[start..]);
        out
    }

    /// Ends the stream: processes an unterminated last line and dispatches a
    /// pending event that lacks its closing blank line. (The SSE spec discards
    /// such an event; accepting it is friendlier to servers that omit the final
    /// blank line.)
    pub fn finish(&mut self) -> Option<SseEvent> {
        let mut out = Vec::new();
        if !self.line.is_empty() {
            self.end_line(&mut out);
        }
        self.after_cr = false;
        self.dispatch(&mut out);
        out.pop()
    }

    fn end_line(&mut self, out: &mut Vec<SseEvent>) {
        let raw = mem::take(&mut self.line);
        let mut line = raw.as_slice();
        if !self.started {
            self.started = true;
            line = line.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(line);
        }
        if line.is_empty() {
            self.dispatch(out);
            return;
        }
        if line[0] == b':' {
            return; // comment
        }
        // A complete line never ends inside a UTF-8 sequence (CR and LF are ASCII).
        let text = String::from_utf8_lossy(line);
        let (field, value) = match text.find(':') {
            Some(p) => {
                let value = &text[p + 1..];
                (&text[..p], value.strip_prefix(' ').unwrap_or(value))
            }
            None => (&text[..], ""),
        };
        match field {
            "event" => self.event = value.to_owned(),
            "data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            _ => {} // `id`, `retry` and unknown fields
        }
    }

    fn dispatch(&mut self, out: &mut Vec<SseEvent>) {
        let event = mem::take(&mut self.event);
        if !self.has_data {
            return;
        }
        self.has_data = false;
        let event = if event.is_empty() {
            "message".to_string()
        } else {
            event
        };
        out.push(SseEvent {
            event,
            data: mem::take(&mut self.data),
        });
    }
}

/// Turns Messages API stream events into [`AiEvent`]s.
///
/// * `content_block_delta` with `delta.type == "text_delta"` → [`AiEvent::Delta`]
///   (thinking, signature, tool-input and citation deltas are skipped);
/// * `message_start` / `message_delta` → usage and stop reason, reported by the
///   [`AiEvent::Done`] that `message_stop` produces;
/// * `error` → [`AiEvent::Error`] with the API's message;
/// * `ping`, `content_block_start` / `content_block_stop` and unknown event types
///   are ignored.
///
/// After the first terminal event every further event is ignored.
#[derive(Debug, Default)]
pub struct StreamDecoder {
    stop_reason: Option<String>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    finished: bool,
}

impl StreamDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a terminal event has been produced.
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Decodes one SSE event.
    pub fn handle(&mut self, ev: &SseEvent) -> Option<AiEvent> {
        if self.finished {
            return None;
        }
        let json: Value = match serde_json::from_str(&ev.data) {
            Ok(v) => v,
            Err(err) => {
                if ev.event == "error" {
                    self.finished = true;
                    return Some(AiEvent::Error(api_error::stream_error_message(&ev.data)));
                }
                log::warn!(
                    "rustpy-ai: ignoring malformed `{}` stream event: {err}",
                    ev.event
                );
                return None;
            }
        };
        // The JSON `type` is authoritative; the SSE event name is a fallback.
        let kind = json
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or(ev.event.as_str());
        match kind {
            "message_start" => {
                if let Some(usage) = json.pointer("/message/usage") {
                    self.record_usage(usage);
                }
                None
            }
            "content_block_start" => {
                // Text blocks start empty in practice, but honor initial text.
                let block = &json["content_block"];
                if block["type"] == "text" {
                    text_event(&block["text"])
                } else {
                    None
                }
            }
            "content_block_delta" => {
                let delta = &json["delta"];
                if delta["type"] == "text_delta" {
                    text_event(&delta["text"])
                } else {
                    None
                }
            }
            "message_delta" => {
                if let Some(reason) = json.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    self.stop_reason = Some(reason.to_owned());
                }
                if let Some(usage) = json.get("usage") {
                    self.record_usage(usage);
                }
                None
            }
            "message_stop" => {
                self.finished = true;
                Some(self.done())
            }
            "error" => {
                self.finished = true;
                Some(AiEvent::Error(api_error::stream_error_message(&ev.data)))
            }
            _ => None,
        }
    }

    /// Ends the stream: if no terminal event was seen, returns `Done` when a stop
    /// reason already arrived (only `message_stop` was lost) and an error otherwise.
    pub fn finish(&mut self) -> Option<AiEvent> {
        if self.finished {
            return None;
        }
        self.finished = true;
        Some(if self.stop_reason.is_some() {
            self.done()
        } else {
            AiEvent::Error("The connection closed before the response was complete.".to_string())
        })
    }

    fn record_usage(&mut self, usage: &Value) {
        if let Some(n) = usage.get("input_tokens").and_then(Value::as_u64) {
            self.input_tokens = Some(n);
        }
        if let Some(n) = usage.get("output_tokens").and_then(Value::as_u64) {
            self.output_tokens = Some(n);
        }
    }

    fn done(&self) -> AiEvent {
        AiEvent::Done {
            stop_reason: self.stop_reason.clone(),
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
        }
    }
}

fn text_event(text: &Value) -> Option<AiEvent> {
    text.as_str()
        .filter(|t| !t.is_empty())
        .map(|t| AiEvent::Delta(t.to_owned()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A recorded Messages API stream (Opus-style: a thinking block, pings, then
    /// text with multi-byte characters).
    pub(crate) const RECORDED: &str = concat!(
        "event: message_start\n",
        r#"data: {"type":"message_start","message":{"id":"msg_01XFDUDYJgAACzvnptvVoYEL","type":"message","role":"assistant","model":"claude-opus-5-5","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":25,"output_tokens":1}}}"#,
        "\n\n",
        "event: content_block_start\n",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#,
        "\n\n",
        "event: ping\n",
        r#"data: {"type": "ping"}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":""}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"EqQBCgIYAhIM1gbcDa9GJwZA2b3hGgxBdjrkzLoky3dl1pkiMOYds"}}"#,
        "\n\n",
        "event: content_block_stop\n",
        r#"data: {"type":"content_block_stop","index":0}"#,
        "\n\n",
        "event: content_block_start\n",
        r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Héllo, "}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"wörld 🦀 — 你好"}}"#,
        "\n\n",
        ": keep-alive comment\n",
        "event: ping\n",
        r#"data: {"type": "ping"}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"!\n```python\nprint(\"ok\")\n```"}}"#,
        "\n\n",
        "event: content_block_stop\n",
        r#"data: {"type":"content_block_stop","index":1}"#,
        "\n\n",
        "event: message_delta\n",
        r#"data: {"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":15}}"#,
        "\n\n",
        "event: message_stop\n",
        r#"data: {"type":"message_stop"}"#,
        "\n\n",
    );

    /// The text carried by [`RECORDED`].
    pub(crate) const RECORDED_TEXT: &str = "Héllo, wörld 🦀 — 你好!\n```python\nprint(\"ok\")\n```";

    /// A stream that fails mid-way with an `error` event.
    pub(crate) const RECORDED_ERROR: &str = concat!(
        "event: message_start\n",
        r#"data: {"type":"message_start","message":{"id":"msg_02","type":"message","role":"assistant","model":"claude-opus-5-5","content":[],"stop_reason":null,"usage":{"input_tokens":12,"output_tokens":1}}}"#,
        "\n\n",
        "event: content_block_start\n",
        r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        "\n\n",
        "event: content_block_delta\n",
        r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Partial ünswer"}}"#,
        "\n\n",
        "event: ping\n",
        r#"data: {"type": "ping"}"#,
        "\n\n",
        "event: error\n",
        r#"data: {"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}"#,
        "\n\n",
    );

    fn parse_in_pieces(input: &[u8], cuts: &[usize]) -> Vec<SseEvent> {
        let mut parser = SseParser::new();
        let mut events = Vec::new();
        let mut prev = 0;
        for &cut in cuts.iter().chain(std::iter::once(&input.len())) {
            events.extend(parser.feed(&input[prev..cut]));
            prev = cut;
        }
        events.extend(parser.finish());
        events
    }

    fn decode(events: &[SseEvent]) -> Vec<AiEvent> {
        let mut decoder = StreamDecoder::new();
        let mut out: Vec<AiEvent> = events.iter().filter_map(|e| decoder.handle(e)).collect();
        out.extend(decoder.finish());
        out
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

    #[test]
    fn parses_fields_comments_and_multiline_data() {
        let events = parse_in_pieces(
            b"\xEF\xBB\xBF: comment\nevent: a\ndata: one\ndata:two\nid: 7\nretry: 100\n\ndata\n\nevent: b\n\nevent:c\ndata:  x\n\n",
            &[],
        );
        assert_eq!(
            events,
            vec![
                SseEvent {
                    event: "a".into(),
                    data: "one\ntwo".into()
                },
                SseEvent {
                    event: "message".into(),
                    data: String::new()
                },
                // `event: b` had no data and was not dispatched; its name does not leak into `c`.
                SseEvent {
                    event: "c".into(),
                    data: " x".into()
                },
            ]
        );
    }

    #[test]
    fn handles_crlf_and_cr_line_endings() {
        let lf = parse_in_pieces(b"event: x\ndata: 1\n\nevent: y\ndata: 2\n\n", &[]);
        let crlf = parse_in_pieces(
            b"event: x\r\ndata: 1\r\n\r\nevent: y\r\ndata: 2\r\n\r\n",
            &[],
        );
        let cr = parse_in_pieces(b"event: x\rdata: 1\r\revent: y\rdata: 2\r\r", &[]);
        assert_eq!(lf.len(), 2);
        assert_eq!(crlf, lf);
        assert_eq!(cr, lf);
        // A `\r\n` split across two reads is one line ending, not two.
        let split = b"event: x\r\ndata: 1\r\n\r\nevent: y\r\ndata: 2\r\n\r\n";
        for cut in 0..=split.len() {
            assert_eq!(parse_in_pieces(split, &[cut]), lf, "cut at {cut}");
        }
    }

    #[test]
    fn finish_flushes_an_unterminated_event() {
        assert_eq!(
            parse_in_pieces(b"event: message_stop\ndata: {}", &[]),
            vec![SseEvent {
                event: "message_stop".into(),
                data: "{}".into()
            }]
        );
        assert_eq!(
            parse_in_pieces(b"event: message_stop\ndata: {}\n", &[]).len(),
            1
        );
        assert!(parse_in_pieces(b"event: lonely\n", &[]).is_empty());
    }

    #[test]
    fn recorded_stream_decodes_to_text_and_done() {
        let events = decode(&parse_in_pieces(RECORDED.as_bytes(), &[]));
        assert_eq!(text_of(&events), RECORDED_TEXT);
        assert_eq!(
            events.last(),
            Some(&AiEvent::Done {
                stop_reason: Some("end_turn".into()),
                input_tokens: Some(25),
                output_tokens: Some(15)
            })
        );
        assert_eq!(events.iter().filter(|e| e.is_terminal()).count(), 1);
        // Thinking/signature deltas and pings produce nothing: only the three text deltas.
        assert_eq!(events.len(), 4);
    }

    #[test]
    fn recorded_stream_survives_every_split_point() {
        let input = RECORDED.as_bytes();
        let whole = parse_in_pieces(input, &[]);
        // Every single cut, which includes cuts inside "é", "ö", "🦀", "—" and "你好".
        for cut in 0..=input.len() {
            assert_eq!(parse_in_pieces(input, &[cut]), whole, "cut at byte {cut}");
        }
        // Byte-by-byte reads.
        let every: Vec<usize> = (1..input.len()).collect();
        assert_eq!(parse_in_pieces(input, &every), whole);
        // Explicit cuts in the middle of the crab emoji and of "é".
        let crab = RECORDED.find('🦀').unwrap();
        let e_acute = RECORDED.find('é').unwrap();
        let cuts = [e_acute + 1, crab + 1, crab + 2, crab + 3];
        assert_eq!(decode(&parse_in_pieces(input, &cuts)), decode(&whole));
        assert_eq!(
            text_of(&decode(&parse_in_pieces(input, &cuts))),
            RECORDED_TEXT
        );
    }

    #[test]
    fn error_event_ends_the_stream() {
        let input = RECORDED_ERROR.as_bytes();
        for cut in 0..=input.len() {
            let events = decode(&parse_in_pieces(input, &[cut]));
            assert_eq!(
                events,
                vec![
                    AiEvent::Delta("Partial ünswer".into()),
                    AiEvent::Error("API error (overloaded_error): Overloaded".into()),
                ],
                "cut at byte {cut}"
            );
        }
    }

    #[test]
    fn truncated_stream_is_an_error_unless_the_stop_reason_arrived() {
        let cut = RECORDED.find("event: message_delta").unwrap();
        let events = decode(&parse_in_pieces(&RECORDED.as_bytes()[..cut], &[]));
        assert_eq!(text_of(&events), RECORDED_TEXT);
        assert!(
            matches!(events.last(), Some(AiEvent::Error(m)) if m.contains("closed before")),
            "{events:?}"
        );

        let cut = RECORDED.find("event: message_stop").unwrap();
        let events = decode(&parse_in_pieces(&RECORDED.as_bytes()[..cut], &[]));
        assert!(
            matches!(events.last(), Some(AiEvent::Done { stop_reason: Some(r), .. }) if r == "end_turn")
        );
    }

    #[test]
    fn decoder_skips_non_text_blocks_and_stops_after_terminal_events() {
        let ev = |event: &str, data: &str| SseEvent {
            event: event.into(),
            data: data.into(),
        };
        let mut d = StreamDecoder::new();
        assert_eq!(d.handle(&ev("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"fallback","from":{"model":"claude-opus-5-5"},"to":{"model":"claude-opus-4-8"}}}"#)), None);
        assert_eq!(d.handle(&ev("content_block_start", r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"x","input":{}}}"#)), None);
        assert_eq!(d.handle(&ev("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"a\":"}}"#)), None);
        assert_eq!(d.handle(&ev("content_block_start", r#"{"type":"content_block_start","index":2,"content_block":{"type":"text","text":"Hi"}}"#)), Some(AiEvent::Delta("Hi".into())));
        assert_eq!(d.handle(&ev("content_block_delta", r#"{"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":""}}"#)), None);
        assert_eq!(
            d.handle(&ev("brand_new_event", r#"{"type":"brand_new_event"}"#)),
            None
        );
        assert_eq!(d.handle(&ev("content_block_delta", "{not json")), None);
        assert_eq!(d.handle(&ev("message_delta", r#"{"type":"message_delta","delta":{"stop_reason":"refusal"},"usage":{"input_tokens":40,"output_tokens":3}}"#)), None);
        assert_eq!(
            d.handle(&ev("message_stop", r#"{"type":"message_stop"}"#)),
            Some(AiEvent::Done {
                stop_reason: Some("refusal".into()),
                input_tokens: Some(40),
                output_tokens: Some(3)
            })
        );
        assert!(d.is_finished());
        assert_eq!(d.handle(&ev("content_block_delta", r#"{"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"late"}}"#)), None);
        assert_eq!(d.finish(), None);

        let mut d = StreamDecoder::new();
        assert_eq!(
            d.handle(&ev("error", "upstream exploded")),
            Some(AiEvent::Error("API error: upstream exploded".into()))
        );
        assert_eq!(d.finish(), None);
    }
}
