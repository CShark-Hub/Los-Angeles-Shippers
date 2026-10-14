//! What `serve` and the code reader say to each other. Both ends are one binary, so the wire lives
//! here and not in `mercury-schema`: it carries a model's request and the model's words, never a
//! probe's reply.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The largest request body the reader reads, in bytes.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

/// The most output tokens a request may ask for.
pub const MAX_TOKENS: u32 = 4_000;

/// What `serve` asks for: a handful of findings fit well inside it.
pub const SERVE_MAX_TOKENS: u32 = 1_500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

/// `Debug` prints sizes only: a message holds code and a question.
impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let role = match self.role {
            Role::System => "system",
            Role::User => "user",
        };
        write!(f, "Message({role}, <{} bytes>)", self.content.len())
    }
}

/// `POST /read`: exactly two messages, the system's and the user's, in that order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub max_tokens: u32,
}

impl ReadRequest {
    /// The shape the reader accepts: two messages, system then user, and `max_tokens` from 1 to
    /// [`MAX_TOKENS`].
    pub fn is_well_formed(&self) -> bool {
        matches!(
            self.messages.as_slice(),
            [
                Message {
                    role: Role::System,
                    ..
                },
                Message { role: Role::User, .. }
            ]
        ) && (1..=MAX_TOKENS).contains(&self.max_tokens)
    }
}

/// The reader's answer: what the model said and what the call cost, in US dollars.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadReply {
    pub content: String,
    pub cost_usd: f64,
}

impl fmt::Debug for ReadReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ReadReply(<{} bytes>, ${})", self.content.len(), self.cost_usd)
    }
}

/// Why the reader gave no answer. The status and the code in `{"error": code}` go together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    TooLarge,
    DailyCap,
    UpstreamFailed,
    NoCost,
    UpstreamTimeout,
}

impl ErrorCode {
    pub fn status(self) -> u16 {
        match self {
            ErrorCode::InvalidRequest => 400,
            ErrorCode::TooLarge => 413,
            ErrorCode::DailyCap => 429,
            ErrorCode::UpstreamFailed | ErrorCode::NoCost => 502,
            ErrorCode::UpstreamTimeout => 504,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::InvalidRequest => "invalid_request",
            ErrorCode::TooLarge => "too_large",
            ErrorCode::DailyCap => "daily_cap",
            ErrorCode::UpstreamFailed => "upstream_failed",
            ErrorCode::NoCost => "no_cost",
            ErrorCode::UpstreamTimeout => "upstream_timeout",
        }
    }
}

/// The body of every failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorBody {
    pub error: ErrorCode,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(messages: &str, max_tokens: u32) -> Result<ReadRequest, serde_json::Error> {
        serde_json::from_str(&format!(
            r#"{{"model":"m","messages":{messages},"max_tokens":{max_tokens}}}"#
        ))
    }

    const TWO: &str = r#"[{"role":"system","content":"s"},{"role":"user","content":"u"}]"#;

    #[test]
    fn a_request_is_two_messages_in_order_and_a_bounded_token_count() {
        assert!(request(TWO, 1_500).unwrap().is_well_formed());
        assert!(request(TWO, 1).unwrap().is_well_formed());
        assert!(request(TWO, MAX_TOKENS).unwrap().is_well_formed());
        assert!(!request(TWO, 0).unwrap().is_well_formed());
        assert!(!request(TWO, MAX_TOKENS + 1).unwrap().is_well_formed());
        for messages in [
            r#"[{"role":"user","content":"u"},{"role":"system","content":"s"}]"#,
            r#"[{"role":"system","content":"s"}]"#,
            r#"[{"role":"system","content":"s"},{"role":"user","content":"u"},{"role":"user","content":"v"}]"#,
            r#"[{"role":"system","content":"s"},{"role":"system","content":"s"}]"#,
            "[]",
        ] {
            assert!(!request(messages, 100).unwrap().is_well_formed(), "{messages}");
        }
    }

    #[test]
    fn anything_beyond_the_shape_is_refused_when_parsed() {
        let extra = r#"{"model":"m","messages":[],"max_tokens":1,"temperature":1}"#;
        assert!(serde_json::from_str::<ReadRequest>(extra).is_err());
        let role = r#"[{"role":"assistant","content":"a"},{"role":"user","content":"u"}]"#;
        assert!(request(role, 1).is_err());
        let field = r#"[{"role":"system","content":"s","name":"x"},{"role":"user","content":"u"}]"#;
        assert!(request(field, 1).is_err());
        assert!(request(TWO, 1).is_ok());
        assert!(serde_json::from_str::<ReadRequest>("[]").is_err());
        assert!(serde_json::from_str::<ReadReply>(r#"{"content":"c","cost_usd":0,"extra":1}"#).is_err());
    }

    #[test]
    fn the_failures_carry_their_statuses() {
        let all = [
            (ErrorCode::InvalidRequest, 400, "invalid_request"),
            (ErrorCode::TooLarge, 413, "too_large"),
            (ErrorCode::DailyCap, 429, "daily_cap"),
            (ErrorCode::UpstreamFailed, 502, "upstream_failed"),
            (ErrorCode::NoCost, 502, "no_cost"),
            (ErrorCode::UpstreamTimeout, 504, "upstream_timeout"),
        ];
        for (code, status, name) in all {
            assert_eq!(code.status(), status);
            assert_eq!(code.as_str(), name);
            assert_eq!(
                serde_json::to_string(&ErrorBody { error: code }).unwrap(),
                format!(r#"{{"error":"{name}"}}"#)
            );
        }
    }

    #[test]
    fn debug_never_prints_a_message_or_a_reply() {
        let marker = "ZqMarker-Code-Line";
        let read = ReadRequest {
            model: "m".into(),
            messages: vec![Message {
                role: Role::User,
                content: marker.into(),
            }],
            max_tokens: 1,
        };
        let reply = ReadReply {
            content: marker.into(),
            cost_usd: 0.5,
        };
        assert!(!format!("{read:?} {reply:?}").contains(marker));
    }
}
