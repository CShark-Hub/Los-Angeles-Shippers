//! The Jev client: one Decisions request per batch, and every answer checked against its question.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use indexmap::IndexMap;
use mercury_schema::JudgeMode;
use mercury_schema::config::ProviderSettings;
use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::answers::check_answers;
use crate::{ApiKey, Batch, Judge, JudgeError, Outcome, Question, ZERO_RETENTION, require_https};

/// How long a Decisions request may take, reading the answer included.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);

/// Where Jev is and how to ask it.
#[derive(Debug, Clone)]
pub struct JevConfig {
    /// The Decisions endpoint.
    pub url: String,
    /// The pinned model, such as `typesafe/jev-1.13`.
    pub model: String,
    /// Without a key every request fails before anything is sent.
    pub key: Option<ApiKey>,
    pub timeout: Duration,
    /// Sent as the request's `provider` with every request.
    pub provider: ProviderSettings,
    /// Refuse an address that isn't `https://`, so the key never travels in the clear. Only a
    /// test's stand-in server turns it off.
    pub https_only: bool,
}

impl JevConfig {
    /// A config with the default timeout, the zero-retention provider settings and HTTPS only.
    pub fn new(url: impl Into<String>, model: impl Into<String>, key: Option<ApiKey>) -> Self {
        Self {
            url: url.into(),
            model: model.into(),
            key,
            timeout: DEFAULT_TIMEOUT,
            provider: ZERO_RETENTION,
            https_only: true,
        }
    }
}

/// Jev through OpenRouter's Decisions endpoint.
#[derive(Debug)]
pub struct JevJudge {
    config: JevConfig,
    client: reqwest::Client,
}

impl JevJudge {
    /// Fails when the address isn't HTTPS and HTTPS is required, or when the HTTP client can't be
    /// built, as when the system has no usable root certificates.
    pub fn new(config: JevConfig) -> Result<Self, JudgeError> {
        require_https(&config.url, config.https_only)?;
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .https_only(config.https_only)
            // a redirect is an answer we didn't ask for, and the key would follow it
            .redirect(Policy::none())
            .build()
            .map_err(|_| JudgeError::Setup)?;
        Ok(Self { config, client })
    }

    fn transport(&self, err: &reqwest::Error) -> JudgeError {
        if err.is_timeout() {
            JudgeError::Timeout(self.config.timeout.as_millis())
        } else {
            JudgeError::Transport
        }
    }
}

#[async_trait]
impl Judge for JevJudge {
    async fn decide(&self, batch: &Batch) -> Result<Outcome, JudgeError> {
        batch.validate()?;
        let key = self.config.key.as_ref().ok_or(JudgeError::NoKey)?;
        let request = Request {
            model: &self.config.model,
            provider: &self.config.provider,
            state: &batch.state,
            questions: &batch.questions,
        };
        let started = Instant::now();
        let response = self
            .client
            .post(&self.config.url)
            .bearer_auth(key.expose())
            .json(&request)
            .send()
            .await
            .map_err(|err| self.transport(&err))?;
        let status = response.status();
        if !status.is_success() {
            return Err(JudgeError::Status(status.as_u16()));
        }
        let body = response.bytes().await.map_err(|err| self.transport(&err))?;
        let latency_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);

        let response: Response = serde_json::from_slice(&body).map_err(|err| malformed(&err))?;
        let cost_usd = match response.usage.cost {
            Some(cost) if cost < 0.0 => {
                return Err(JudgeError::Malformed("the cost is negative".to_owned()));
            }
            cost => cost,
        };
        Ok(Outcome {
            answers: check_answers(batch, response.answers)?,
            mode: JudgeMode::Jev,
            cost_usd,
            latency_ms,
            request_id: response.id,
            model: Some(response.model),
            prompt_version: None,
        })
    }
}

#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    provider: &'a ProviderSettings,
    state: &'a Value,
    questions: &'a IndexMap<String, Question>,
}

/// The response's required fields are `model`, `answers` and `usage`.
#[derive(Deserialize)]
struct Response {
    id: Option<String>,
    model: String,
    answers: IndexMap<String, Value>,
    usage: Usage,
}

#[derive(Deserialize)]
struct Usage {
    cost: Option<f64>,
}

/// Where the parse failed, without quoting the body.
pub(crate) fn malformed(err: &serde_json::Error) -> JudgeError {
    JudgeError::Malformed(format!(
        "{:?} error at line {} column {}",
        err.classify(),
        err.line(),
        err.column()
    ))
}
