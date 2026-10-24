//! [`AiConfig`]: model, output-token limit, endpoint and API-key resolution.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Default model (Claude Opus 5.5).
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
/// Default endpoint: the first-party Anthropic API.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
/// Default `max_tokens`. Generous on purpose: the current models think before
/// answering and thinking counts toward the limit, so a low value cuts answers
/// short. (Non-streaming [`complete`](crate::complete) calls are capped lower,
/// see [`COMPLETE_MAX_TOKENS`](crate::COMPLETE_MAX_TOKENS).)
pub const DEFAULT_MAX_TOKENS: u32 = 64_000;
/// Environment variable consulted when the config has no API key.
pub const API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// A model offered by the AI Assistant model picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KnownModel {
    /// Model id sent to the API.
    pub id: &'static str,
    /// Human-readable name for the picker.
    pub label: &'static str,
}

/// Models for the picker; the first entry is the default.
pub const KNOWN_MODELS: &[KnownModel] = &[
    KnownModel {
        id: "claude-opus-5-5",
        label: "Claude Opus 5.5",
    },
    KnownModel {
        id: "claude-sonnet-5-5",
        label: "Claude Sonnet 5.5",
    },
    KnownModel {
        id: "claude-haiku-4-5-20251001",
        label: "Claude Haiku 4.5",
    },
];

/// Picker label for a model id (the id itself when the model is not in [`KNOWN_MODELS`]).
pub fn model_label(id: &str) -> &str {
    KNOWN_MODELS
        .iter()
        .find(|m| m.id == id)
        .map_or(id, |m| m.label)
}

/// Wire protocol; existing configurations keep Anthropic.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    #[default]
    Anthropic,
    OpenAiCompatible,
}

/// AI Assistant settings.
///
/// Deserializes with defaults for missing fields, so it can be embedded in a
/// settings file. Note that a configured `api_key` is serialized in plain text;
/// leave it `None` to rely on `ANTHROPIC_API_KEY` instead. `Debug` output redacts
/// the key.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiConfig {
    pub provider: Provider,
    /// Runtime application/window owner; never persisted or sent to providers.
    #[serde(skip)]
    pub worker_owner: u64,
    /// API key; `None` (or blank) falls back to the `ANTHROPIC_API_KEY` environment variable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Model id, e.g. one of [`KNOWN_MODELS`]. Blank means [`DEFAULT_MODEL`].
    pub model: String,
    /// Maximum output tokens per reply (thinking included). `0` means [`DEFAULT_MAX_TOKENS`].
    pub max_tokens: u32,
    /// API origin; requests go to `{base_url}/v1/messages`. Blank means [`DEFAULT_BASE_URL`].
    pub base_url: String,
    /// Opt into the API's server-side refusal fallback (sent as a beta header on
    /// supported first-party models). Off by default.
    pub refusal_fallback: bool,
}

impl Default for AiConfig {
    fn default() -> Self {
        AiConfig {
            provider: Provider::Anthropic,
            worker_owner: 0,
            api_key: None,
            model: DEFAULT_MODEL.to_string(),
            max_tokens: DEFAULT_MAX_TOKENS,
            base_url: DEFAULT_BASE_URL.to_string(),
            refusal_fallback: false,
        }
    }
}

impl fmt::Debug for AiConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let key = self.api_key.as_ref().map(|_| "<redacted>");
        f.debug_struct("AiConfig")
            .field("provider", &self.provider)
            .field("api_key", &key)
            .field("model", &self.model)
            .field("max_tokens", &self.max_tokens)
            .field("base_url", &self.base_url)
            .field("refusal_fallback", &self.refusal_fallback)
            .finish()
    }
}

impl AiConfig {
    /// The API key to use: the configured key if it is not blank, otherwise the
    /// `ANTHROPIC_API_KEY` environment variable. Surrounding whitespace is removed
    /// (pasted keys often carry a trailing newline).
    pub fn resolved_key(&self) -> Option<String> {
        if self.provider == Provider::Anthropic {
            resolve_key(self.api_key.as_deref(), |name| std::env::var(name).ok())
        } else {
            self.api_key
                .as_deref()
                .map(str::trim)
                .filter(|key| !key.is_empty())
                .map(str::to_owned)
                .or_else(|| {
                    std::env::var("OPENAI_API_KEY")
                        .ok()
                        .map(|key| key.trim().to_owned())
                        .filter(|key| !key.is_empty())
                })
        }
    }

    /// `true` when [`resolved_key`](Self::resolved_key) finds a key.
    pub fn has_key(&self) -> bool {
        self.resolved_key().is_some()
    }

    /// Full Messages API URL, `{base_url}/v1/messages`. Tolerates a trailing `/`
    /// and a base URL that already ends in `/v1` or `/v1/messages`.
    pub fn messages_url(&self) -> String {
        let base = self.base_url.trim();
        let base = if base.is_empty() {
            DEFAULT_BASE_URL
        } else {
            base
        };
        let base = base.trim_end_matches('/');
        let base = base
            .strip_suffix("/v1/messages")
            .or_else(|| base.strip_suffix("/v1"))
            .unwrap_or(base);
        format!("{base}/v1/messages")
    }

    /// The model id sent to the API.
    pub(crate) fn effective_model(&self) -> &str {
        let model = self.model.trim();
        if model.is_empty() {
            DEFAULT_MODEL
        } else {
            model
        }
    }

    /// The `max_tokens` sent to the API.
    pub(crate) fn effective_max_tokens(&self) -> u32 {
        if self.max_tokens == 0 {
            DEFAULT_MAX_TOKENS
        } else {
            self.max_tokens
        }
    }

    /// Whether requests go to the first-party Anthropic API (not a proxy or gateway).
    pub(crate) fn is_first_party(&self) -> bool {
        self.messages_url()
            .eq_ignore_ascii_case("https://api.anthropic.com/v1/messages")
    }
}

/// Key resolution with an injectable environment lookup (testable without
/// touching the process environment).
pub(crate) fn resolve_key(
    configured: Option<&str>,
    env: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let non_blank = |s: &str| {
        let s = s.trim();
        (!s.is_empty()).then(|| s.to_string())
    };
    configured
        .and_then(non_blank)
        .or_else(|| env(API_KEY_ENV).as_deref().and_then(non_blank))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let cfg = AiConfig::default();
        assert_eq!(cfg.model, "claude-opus-5-5");
        assert_eq!(cfg.base_url, "https://api.anthropic.com");
        assert_eq!(cfg.max_tokens, DEFAULT_MAX_TOKENS);
        assert_eq!(cfg.api_key, None);
        assert_eq!(cfg.messages_url(), "https://api.anthropic.com/v1/messages");
        assert!(cfg.is_first_party());
        assert_eq!(KNOWN_MODELS[0].id, DEFAULT_MODEL);
        let ids: Vec<&str> = KNOWN_MODELS.iter().map(|m| m.id).collect();
        assert_eq!(
            ids,
            [
                "claude-opus-5-5",
                "claude-sonnet-5-5",
                "claude-haiku-4-5-20251001"
            ]
        );
        assert_eq!(model_label("claude-sonnet-5-5"), "Claude Sonnet 5.5");
        assert_eq!(model_label("my-custom-model"), "my-custom-model");
    }

    #[test]
    fn configured_key_wins_over_environment() {
        let env = |name: &str| (name == API_KEY_ENV).then(|| "env-key".to_string());
        assert_eq!(
            resolve_key(Some("cfg-key"), env).as_deref(),
            Some("cfg-key")
        );
        assert_eq!(
            resolve_key(Some("  cfg-key\n"), env).as_deref(),
            Some("cfg-key")
        );
        assert_eq!(resolve_key(None, env).as_deref(), Some("env-key"));
        assert_eq!(resolve_key(Some("   "), env).as_deref(), Some("env-key"));
        assert_eq!(resolve_key(None, |_| None), None);
        assert_eq!(resolve_key(None, |_| Some(" \n".to_string())), None);
        let cfg = AiConfig {
            api_key: Some(" sk-test ".into()),
            ..AiConfig::default()
        };
        assert_eq!(cfg.resolved_key().as_deref(), Some("sk-test"));
        assert!(cfg.has_key());
    }

    #[test]
    fn messages_url_normalization() {
        let url = |base: &str| {
            AiConfig {
                base_url: base.into(),
                ..AiConfig::default()
            }
            .messages_url()
        };
        assert_eq!(
            url("http://127.0.0.1:8080"),
            "http://127.0.0.1:8080/v1/messages"
        );
        assert_eq!(
            url("http://127.0.0.1:8080/"),
            "http://127.0.0.1:8080/v1/messages"
        );
        assert_eq!(
            url("https://gateway.example.com/anthropic/v1"),
            "https://gateway.example.com/anthropic/v1/messages"
        );
        assert_eq!(
            url("https://api.anthropic.com/v1/messages"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(url("  "), "https://api.anthropic.com/v1/messages");
        assert!(!AiConfig {
            base_url: "http://127.0.0.1:9".into(),
            ..AiConfig::default()
        }
        .is_first_party());
        assert!(AiConfig {
            base_url: "https://api.anthropic.com/".into(),
            ..AiConfig::default()
        }
        .is_first_party());
    }

    #[test]
    fn effective_values_fill_blanks() {
        let cfg = AiConfig {
            model: " ".into(),
            max_tokens: 0,
            ..AiConfig::default()
        };
        assert_eq!(cfg.effective_model(), DEFAULT_MODEL);
        assert_eq!(cfg.effective_max_tokens(), DEFAULT_MAX_TOKENS);
        let cfg = AiConfig {
            model: "claude-haiku-4-5-20251001".into(),
            max_tokens: 2048,
            ..AiConfig::default()
        };
        assert_eq!(cfg.effective_model(), "claude-haiku-4-5-20251001");
        assert_eq!(cfg.effective_max_tokens(), 2048);
    }

    #[test]
    fn debug_redacts_the_key() {
        let cfg = AiConfig {
            api_key: Some("sk-ant-secret".into()),
            ..AiConfig::default()
        };
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("sk-ant-secret"), "{dbg}");
        assert!(dbg.contains("<redacted>"));
        assert!(dbg.contains("claude-opus-5-5"));
    }

    #[test]
    fn serde_fills_missing_fields_with_defaults() {
        let cfg: AiConfig = serde_json::from_str(r#"{"model":"claude-sonnet-5-5"}"#).unwrap();
        assert_eq!(cfg.model, "claude-sonnet-5-5");
        assert_eq!(cfg.base_url, DEFAULT_BASE_URL);
        assert_eq!(cfg.max_tokens, DEFAULT_MAX_TOKENS);
        let json = serde_json::to_string(&AiConfig::default()).unwrap();
        assert!(!json.contains("api_key"), "{json}");
        let back: AiConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, AiConfig::default());
    }
}
