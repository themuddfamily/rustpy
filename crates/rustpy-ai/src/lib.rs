//! RustPy IDE — AI Assistant backend (roadmap §5.1 "AI Assistant").
//!
//! Talks to the Anthropic Messages API over HTTPS (`ureq`):
//!
//! * [`AiConfig`]: model ([`KNOWN_MODELS`] for the picker), `max_tokens`, base
//!   URL, and the API key from the config or the `ANTHROPIC_API_KEY` variable.
//! * [`start_stream`] streams a reply on a background thread; the UI polls the
//!   returned [`StreamHandle`] for [`AiEvent`]s (text deltas, then `Done` with
//!   the stop reason and token usage, or `Error`) and can cancel it.
//!   [`complete`] is the blocking, non-streaming variant for short tasks.
//! * [`prompts`] builds the requests behind the IDE actions: explain code,
//!   generate a docstring, suggest refactorings, write tests, write a commit
//!   message, fix an error, and chat with editor context.
//! * [`extract_code_blocks`] finds the fenced code blocks of a reply for the
//!   "Insert at caret" / "Replace selection" actions; [`strip_code_fence`]
//!   unwraps a reply that is a single block.
//! * [`sse`] holds the incremental Server-Sent Events parser and the stream
//!   event decoder used by [`start_stream`].
//!
//! # Requests
//!
//! Requests are `POST {base_url}/v1/messages` with `x-api-key`,
//! `anthropic-version: 2023-06-01` and a `{model, max_tokens, system, messages}`
//! body (`"stream": true` for [`start_stream`]). Before sending, the conversation
//! is normalized: blank turns and leading assistant turns are dropped and
//! consecutive turns of the same role are merged.
//!
//! * Rate-limit (429), overload (529) and server (5xx) errors are retried up to
//!   twice, honoring `retry-after` / `retry-after-ms` and `x-should-retry`; other
//!   HTTP errors are reported with the API's own error message.
//! * Connections use the IDE's network settings through `rustpy_net`: the
//!   proxy of Settings › HTTP Proxy (by default `HTTPS_PROXY` / `HTTP_PROXY` /
//!   `ALL_PROXY` and `NO_PROXY`; loopback hosts are never proxied) and the
//!   certificates trusted under Settings › Tools › Server Certificates.
//! * Optional (`AiConfig::refusal_fallback`, off by default): on the first-party
//!   API, requests to models whose safety classifiers can decline a request
//!   (Opus 5.5, Sonnet 5.5, Opus 5, Fable 5 / 5.1) opt into server-side refusal
//!   fallback (`"fallbacks": "default"` with the
//!   `server-side-fallback-2026-07-01` beta header): a declined request is re-run
//!   on Anthropic's recommended fallback model instead of ending with
//!   `stop_reason: "refusal"`. If the API rejects the opt-in, the request is sent
//!   again without it. Custom base URLs (gateways, proxies, tests) never get it.
//!
//! ```no_run
//! use rustpy_ai::{prompts, start_stream, AiConfig, AiEvent};
//!
//! let cfg = AiConfig::default(); // key from ANTHROPIC_API_KEY
//! let handle = start_stream(&cfg, prompts::explain_code("print(1)", "python", "main.py"));
//! let mut answer = String::new();
//! while !handle.is_finished() {
//!     for event in handle.poll() {
//!         match event {
//!             AiEvent::Delta(text) => answer.push_str(&text),
//!             AiEvent::Done { output_tokens, .. } => println!("done, {output_tokens:?} tokens"),
//!             AiEvent::Error(message) => eprintln!("{message}"),
//!         }
//!     }
//!     std::thread::sleep(std::time::Duration::from_millis(50));
//! }
//! ```

mod api_error;
mod client;
mod compatible;
mod config;
#[cfg(test)]
mod configured_https_tests;
mod markdown;
#[cfg(test)]
mod mock_server;
pub mod prompts;
pub mod sse;
mod types;
mod workers;
pub use workers::{
    active_workers, cancel_workers, new_worker_owner, worker_ids_for_owner, workers_running,
    WorkerInfo,
};

pub use client::{complete, start_stream, StreamHandle, ANTHROPIC_VERSION, COMPLETE_MAX_TOKENS};
pub use config::{
    model_label, AiConfig, KnownModel, Provider, API_KEY_ENV, DEFAULT_BASE_URL, DEFAULT_MAX_TOKENS,
    DEFAULT_MODEL, KNOWN_MODELS,
};
pub use markdown::{extract_code_blocks, strip_code_fence};
pub use prompts::{
    chat, commit_message, explain_code, fix_error, generate_docstring, suggest_refactoring,
    truncate_middle, write_tests, ContextSnippet, MAX_INPUT_CHARS,
};
pub use types::{AiEvent, ChatRequest, Message, Role};
