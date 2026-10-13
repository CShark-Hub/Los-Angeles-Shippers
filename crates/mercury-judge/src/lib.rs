//! The judge: one interface for every yes/no, pick-one and score decision in Mercury, the Jev
//! client behind it, the chat judge and the rules after it, and the chain that falls from one to
//! the next.
//!
//! A judge answers every question of a [`Batch`] or fails with a [`JudgeError`]. Nothing here
//! fills a gap with a default: a timeout, a missing key, a missing answer or a value out of range
//! is an error. The chain answers an error with the next judge down, whose mode is in the
//! [`Outcome`], and an error of the last one stays an error: never a pass.

mod answers;
mod batch;
pub mod catalogue;
mod chat;
mod chat_judge;
pub mod eval;
mod jev;
mod resilient;
mod rules;
pub mod stages;

use std::path::Path;

use mercury_schema::config::{DataCollection, ProviderSettings};

pub use batch::{
    Answer, Batch, MAX_LEVELS, MAX_OPTIONS, MIN_LEVELS, MIN_OPTIONS, NoulCriteria, Outcome, Question,
};
pub use catalogue::{Catalogue, CatalogueError, VerdictError, verdict};
pub use chat::{
    ChatClient, ChatConfig, ChatError, Completion, DEFAULT_CHAT_TIMEOUT, Format, Message, MessageRole,
};
pub use chat_judge::{ChatJudge, Prompt};
pub use jev::{DEFAULT_TIMEOUT, JevConfig, JevJudge};
pub use resilient::{BREAKER_FAILURES, BREAKER_OPEN, BREAKER_WINDOW, CircuitBreaker, Clock, ResilientJudge};
pub use rules::{Polarity, Rule, RulesJudge, Test};

/// The provider settings every OpenRouter request carries: only endpoints that neither collect
/// nor retain what they are sent. An endpoint that stops meeting them fails the request, so a
/// policy change fails closed.
pub const ZERO_RETENTION: ProviderSettings = ProviderSettings {
    data_collection: DataCollection::Deny,
    zdr: true,
};

/// Decides every question of a batch, about one state, at once.
#[async_trait::async_trait]
pub trait Judge: Send + Sync {
    /// One answer per question, each checked against its question, or an error.
    async fn decide(&self, batch: &Batch) -> Result<Outcome, JudgeError>;
}

/// Why a judge gave no outcome. Messages name question ids and HTTP statuses, never the state,
/// the instructions, the key or the body of a response.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JudgeError {
    #[error("the batch can't be judged: {0}")]
    InvalidBatch(String),
    #[error("no key is configured for the judge")]
    NoKey,
    #[error("the judge's address isn't HTTPS")]
    InsecureUrl,
    #[error("the judge's HTTP client can't be built")]
    Setup,
    #[error("the judge gave no answer within {0} ms")]
    Timeout(u128),
    #[error("the request to the judge failed before an answer arrived")]
    Transport,
    #[error("the judge answered with HTTP {0}")]
    Status(u16),
    #[error("the judge's response is malformed: {0}")]
    Malformed(String),
    #[error("the judge didn't answer `{0}`")]
    MissingAnswer(String),
    #[error("the judge answered a question that wasn't asked")]
    UnexpectedAnswer,
    #[error("the judge's answer to `{id}` is invalid: {reason}")]
    InvalidAnswer { id: String, reason: &'static str },
    #[error("no rule answers `{0}`")]
    NoRule(String),
    #[error("the rule for `{0}` is for another type of question")]
    RuleMismatch(String),
}

/// A key for a model API. It never prints.
#[derive(Clone)]
pub struct ApiKey(String);

impl ApiKey {
    /// The key without surrounding whitespace, or `None` when nothing is left, as from an empty
    /// key file.
    pub fn new(key: &str) -> Option<Self> {
        let key = key.trim();
        (!key.is_empty()).then(|| Self(key.to_owned()))
    }

    /// The key in a secret file; `None` when the file is empty, as it stays until a key is set.
    /// The error names the problem, never the file's content.
    pub fn read(path: &Path) -> std::io::Result<Option<Self>> {
        Ok(Self::new(&std::fs::read_to_string(path)?))
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(..)")
    }
}

/// `Err(InsecureUrl)` unless `url` is `https://…` or HTTPS isn't required.
pub(crate) fn require_https(url: &str, https_only: bool) -> Result<(), JudgeError> {
    if https_only && !url.starts_with("https://") {
        return Err(JudgeError::InsecureUrl);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_retention_denies_collection_and_asks_for_zdr() {
        assert_eq!(
            serde_json::to_value(ZERO_RETENTION).unwrap(),
            serde_json::json!({"data_collection": "deny", "zdr": true})
        );
    }

    #[test]
    fn a_key_file_holds_the_key_and_an_empty_one_holds_none() {
        let dir = std::env::temp_dir().join(format!("mercury-judge-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("openrouter_api_key");
        std::fs::write(&file, "  not-a-real-key\n").unwrap();
        assert_eq!(ApiKey::read(&file).unwrap().unwrap().expose(), "not-a-real-key");
        std::fs::write(&file, "\n").unwrap();
        assert!(ApiKey::read(&file).unwrap().is_none());
        let err = ApiKey::read(&dir.join("missing")).unwrap_err();
        assert!(!err.to_string().contains("not-a-real-key"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_https_passes_when_https_is_required() {
        assert_eq!(require_https("https://openrouter.ai/api/v1", true), Ok(()));
        assert_eq!(
            require_https("http://openrouter.ai/api/v1", true),
            Err(JudgeError::InsecureUrl)
        );
        assert_eq!(
            require_https("HTTPS://openrouter.ai", true),
            Err(JudgeError::InsecureUrl)
        );
        assert_eq!(require_https("http://127.0.0.1:9000/stub", false), Ok(()));
    }
}
