//! Request and event types shared by the client and the prompt builders.

use serde::{Deserialize, Serialize};

/// Author of a conversation turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    /// The role name used by the Messages API (`"user"` / `"assistant"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }
}

/// One conversation turn (plain text).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Message {
            role: Role::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Message {
            role: Role::Assistant,
            content: content.into(),
        }
    }
}

/// A request to the model: a system prompt plus the conversation so far, which
/// should end with a user turn.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub system: String,
    pub messages: Vec<Message>,
}

/// Progress of a streamed reply, delivered by [`StreamHandle::poll`](crate::StreamHandle::poll).
///
/// A stream yields any number of `Delta`s followed by exactly one terminal
/// event (`Done` or `Error`), unless it is cancelled first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AiEvent {
    /// The next piece of the reply's text; append it to what came before.
    Delta(String),
    /// The reply is complete. `stop_reason` is the API's value: `"end_turn"`,
    /// `"max_tokens"` (the reply was cut off at the token limit), `"refusal"`
    /// (the model declined; the text so far may be partial), and so on.
    Done {
        stop_reason: Option<String>,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
    },
    /// The request failed; the message is meant for the user (it includes the
    /// API's own error message when there is one).
    Error(String),
}

impl AiEvent {
    /// `true` for `Done` and `Error`, the events that end a stream.
    pub fn is_terminal(&self) -> bool {
        !matches!(self, AiEvent::Delta(_))
    }
}

/// Makes a conversation acceptable to the Messages API: drops blank turns and
/// leading assistant turns, and merges consecutive turns of the same role
/// (joined by a blank line).
pub(crate) fn normalize_messages(messages: &[Message]) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::with_capacity(messages.len());
    for m in messages {
        if m.content.trim().is_empty() || (out.is_empty() && m.role == Role::Assistant) {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.role == m.role => {
                last.content.push_str("\n\n");
                last.content.push_str(&m.content);
            }
            _ => out.push(m.clone()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_serialize_as_api_names() {
        assert_eq!(serde_json::to_string(&Role::User).unwrap(), "\"user\"");
        assert_eq!(
            serde_json::to_string(&Message::assistant("hi")).unwrap(),
            r#"{"role":"assistant","content":"hi"}"#
        );
        assert_eq!(Role::Assistant.as_str(), "assistant");
    }

    #[test]
    fn terminal_events() {
        assert!(!AiEvent::Delta("x".into()).is_terminal());
        assert!(AiEvent::Error("x".into()).is_terminal());
        assert!(AiEvent::Done {
            stop_reason: None,
            input_tokens: None,
            output_tokens: None
        }
        .is_terminal());
    }

    #[test]
    fn normalization_merges_and_drops() {
        let msgs = vec![
            Message::assistant("stale greeting"),
            Message::user("first"),
            Message::user("second"),
            Message::assistant("   "),
            Message::assistant("answer"),
            Message::user(""),
            Message::user("third"),
        ];
        assert_eq!(
            normalize_messages(&msgs),
            vec![
                Message::user("first\n\nsecond"),
                Message::assistant("answer"),
                Message::user("third")
            ]
        );
        assert!(normalize_messages(&[Message::assistant("only")]).is_empty());
        assert!(normalize_messages(&[]).is_empty());
    }
}
