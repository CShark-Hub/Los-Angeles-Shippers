//! The chat judge: a chat model answering a batch with probabilities in the shapes of Jev's
//! answers, parsed as strictly. Its probabilities aren't calibrated, so its outcomes carry
//! `JudgeMode::Chat`, and the verdicts tighten every threshold for them.

use std::io;
use std::path::Path;

use async_trait::async_trait;
use indexmap::IndexMap;
use mercury_schema::JudgeMode;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::answers::check_answers;
use crate::chat::{ChatClient, Format, Message};
use crate::jev::malformed;
use crate::{Batch, Judge, JudgeError, Outcome, Question};

/// A prompt with its version, which the audit records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// Such as `judge_fallback.v1`.
    pub version: String,
    pub text: String,
}

impl Prompt {
    pub fn new(version: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            text: text.into(),
        }
    }

    /// A prompt file, `<role>.v<N>.md`, as `config/prompts/` keeps them: its name without `.md` is
    /// the version. Line ends become LF, so the text is the same on every machine.
    pub fn read(path: &Path) -> io::Result<Self> {
        let version = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".md"))
            .filter(|version| is_version(version))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "a prompt file is named <role>.v<N>.md",
                )
            })?;
        let text = std::fs::read_to_string(path)?.replace("\r\n", "\n");
        Ok(Self::new(version, text.trim_end()))
    }
}

/// `judge_fallback.v1`: a role in lower case, then `.v` and a number.
fn is_version(version: &str) -> bool {
    version.split_once(".v").is_some_and(|(role, number)| {
        !role.is_empty()
            && role.bytes().all(|byte| byte.is_ascii_lowercase() || byte == b'_')
            && !number.is_empty()
            && number.bytes().all(|byte| byte.is_ascii_digit())
    })
}

/// A chat model as a judge.
#[derive(Debug)]
pub struct ChatJudge {
    client: ChatClient,
    prompt: Prompt,
}

impl ChatJudge {
    /// `prompt` tells the model the answer shapes; the batch goes in the user message.
    pub fn new(client: ChatClient, prompt: Prompt) -> Self {
        Self { client, prompt }
    }
}

/// What the model is asked: the batch as the Decisions endpoint would get it.
#[derive(Serialize)]
struct Asked<'a> {
    state: &'a Value,
    questions: &'a IndexMap<String, Question>,
}

/// What the model must answer, and nothing else.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Answered {
    answers: IndexMap<String, Value>,
}

#[async_trait]
impl Judge for ChatJudge {
    async fn decide(&self, batch: &Batch) -> Result<Outcome, JudgeError> {
        batch.validate()?;
        let asked = serde_json::to_string(&Asked {
            state: &batch.state,
            questions: &batch.questions,
        })
        .map_err(|_| JudgeError::InvalidBatch("it can't be written as JSON".to_owned()))?;
        let messages = [Message::system(self.prompt.text.clone()), Message::user(asked)];
        let completion = self.client.complete(&messages, Format::Json).await?;
        let answered: Answered = serde_json::from_str(&completion.content).map_err(|err| malformed(&err))?;
        Ok(Outcome {
            answers: check_answers(batch, answered.answers)?,
            mode: JudgeMode::Chat,
            cost_usd: completion.cost_usd,
            latency_ms: completion.latency_ms,
            request_id: completion.request_id,
            model: completion.model,
            prompt_version: Some(self.prompt.version.clone()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_is_a_role_and_a_number() {
        for good in ["judge_fallback.v1", "composer.v12"] {
            assert!(is_version(good), "{good}");
        }
        for bad in [
            "judge_fallback",
            "judge_fallback.v",
            "Judge.v1",
            "judge.v1b",
            ".v1",
            "a.b.v1",
        ] {
            assert!(!is_version(bad), "{bad}");
        }
    }

    #[test]
    fn a_prompt_file_gives_its_version_and_lf_text() {
        let dir = std::env::temp_dir().join(format!("mercury-judge-prompt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("judge_fallback.v3.md");
        std::fs::write(&file, "Answer in JSON.\r\nNothing else.\r\n").unwrap();
        let prompt = Prompt::read(&file).unwrap();
        assert_eq!(prompt.version, "judge_fallback.v3");
        assert_eq!(prompt.text, "Answer in JSON.\nNothing else.");
        let unversioned = dir.join("judge_fallback.md");
        std::fs::write(&unversioned, "x").unwrap();
        assert_eq!(
            Prompt::read(&unversioned).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
