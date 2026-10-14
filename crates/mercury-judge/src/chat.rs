//! One client for the OpenAI chat API, shared by the chat judge, the planner and the composer.
//! It speaks to either backend. On `openrouter` that is HTTPS, a key and the provider settings with
//! every request. On `openai_compatible`, a model server on your organisation's own host, plain
//! HTTP is allowed and the key is optional: a server that wants none is sent none. Errors name what
//! went wrong, never the messages, the key or the body of a response.

use std::time::{Duration, Instant};

use mercury_schema::config::ProviderSettings;
use reqwest::redirect::Policy;
use serde::{Deserialize, Serialize};

use crate::{ApiKey, JudgeError, ZERO_RETENTION, require_https};

/// How long a chat request may take when its role sets no timeout: room for the composer's
/// longest answer. The fallback judge's role should set a shorter one.
pub const DEFAULT_CHAT_TIMEOUT: Duration = Duration::from_secs(10);

/// Where a chat model is and how to ask it.
#[derive(Debug, Clone)]
pub struct ChatConfig {
    /// The chat completions endpoint.
    pub url: String,
    /// The model, such as `openai/gpt-4.1-mini`.
    pub model: String,
    /// The key every request carries as a bearer token.
    pub key: Option<ApiKey>,
    /// Whether a request without a key is an error, failing before anything is sent. It is on
    /// `openrouter`, and on a model server whose key file is configured but missing or empty: the
    /// operator meant it to want a key. A server with no key file is sent none.
    pub key_required: bool,
    pub timeout: Duration,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    /// Sent as the request's `provider` with every request.
    pub provider: ProviderSettings,
    /// Refuse an address that isn't `https://`. Only a test's stand-in server turns it off.
    pub https_only: bool,
}

impl ChatConfig {
    /// A model on OpenRouter: the default timeout, the zero-retention provider settings, HTTPS only.
    pub fn openrouter(url: impl Into<String>, model: impl Into<String>, key: Option<ApiKey>) -> Self {
        Self {
            url: url.into(),
            model: model.into(),
            key,
            key_required: true,
            timeout: DEFAULT_CHAT_TIMEOUT,
            temperature: None,
            max_tokens: None,
            provider: ZERO_RETENTION,
            https_only: true,
        }
    }

    /// A model on your organisation's own host: plain HTTP is allowed and no key is needed. Set
    /// `key_required` when the backend names a key file, so a key that can't be read fails closed.
    pub fn openai_compatible(url: impl Into<String>, model: impl Into<String>, key: Option<ApiKey>) -> Self {
        Self {
            key_required: false,
            https_only: false,
            ..Self::openrouter(url, model, key)
        }
    }
}

/// Who says a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
}

/// One message of a conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: String,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: content.into(),
        }
    }
}

/// What the answer must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Text,
    /// One JSON object (the API's JSON mode).
    Json,
}

/// A finished answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
    pub content: String,
    /// The model that answered, as the response names it.
    pub model: Option<String>,
    pub request_id: Option<String>,
    /// What the request cost; `None` when the response reports no cost.
    pub cost_usd: Option<f64>,
    pub latency_ms: u32,
}

/// Why a chat request gave no answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChatError {
    #[error("no key is configured for the chat model")]
    NoKey,
    #[error("the chat model's address isn't HTTPS")]
    InsecureUrl,
    #[error("the chat client can't be built")]
    Setup,
    #[error("the chat model gave no answer within {0} ms")]
    Timeout(u128),
    #[error("the request to the chat model failed before an answer arrived")]
    Transport,
    #[error("the chat model answered with HTTP {0}")]
    Status(u16),
    #[error("the chat model's response is malformed: {0}")]
    Malformed(String),
    #[error("the chat model's response has no answer")]
    Empty,
    #[error("the chat model's answer is unfinished: {0}")]
    Unfinished(&'static str),
}

impl From<ChatError> for JudgeError {
    fn from(err: ChatError) -> Self {
        match err {
            ChatError::NoKey => JudgeError::NoKey,
            ChatError::InsecureUrl => JudgeError::InsecureUrl,
            ChatError::Setup => JudgeError::Setup,
            ChatError::Timeout(ms) => JudgeError::Timeout(ms),
            ChatError::Transport => JudgeError::Transport,
            ChatError::Status(status) => JudgeError::Status(status),
            ChatError::Malformed(reason) => JudgeError::Malformed(reason),
            ChatError::Empty => JudgeError::Malformed("the chat model gave no answer".to_owned()),
            ChatError::Unfinished(reason) => {
                JudgeError::Malformed(format!("the answer is unfinished: {reason}"))
            }
        }
    }
}

/// The OpenAI chat API, through one backend.
#[derive(Debug)]
pub struct ChatClient {
    config: ChatConfig,
    client: reqwest::Client,
}

impl ChatClient {
    /// Fails when the address isn't HTTPS and HTTPS is required, or when the HTTP client can't be
    /// built.
    pub fn new(config: ChatConfig) -> Result<Self, ChatError> {
        require_https(&config.url, config.https_only).map_err(|_| ChatError::InsecureUrl)?;
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .https_only(config.https_only)
            // a redirect is an answer we didn't ask for, and the key would follow it
            .redirect(Policy::none())
            .build()
            .map_err(|_| ChatError::Setup)?;
        Ok(Self { config, client })
    }

    pub fn model(&self) -> &str {
        &self.config.model
    }

    fn transport(&self, err: &reqwest::Error) -> ChatError {
        if err.is_timeout() {
            ChatError::Timeout(self.config.timeout.as_millis())
        } else {
            ChatError::Transport
        }
    }

    /// The model's answer to `messages`, finished and not empty, or an error.
    pub async fn complete(&self, messages: &[Message], format: Format) -> Result<Completion, ChatError> {
        if self.config.key_required && self.config.key.is_none() {
            return Err(ChatError::NoKey);
        }
        let request = Request {
            model: &self.config.model,
            messages,
            temperature: self.config.temperature,
            max_tokens: self.config.max_tokens,
            response_format: (format == Format::Json).then_some(ResponseFormat { kind: "json_object" }),
            provider: &self.config.provider,
        };
        let started = Instant::now();
        let mut post = self.client.post(&self.config.url).json(&request);
        if let Some(key) = &self.config.key {
            post = post.bearer_auth(key.expose());
        }
        let response = post.send().await.map_err(|err| self.transport(&err))?;
        let status = response.status();
        if !status.is_success() {
            return Err(ChatError::Status(status.as_u16()));
        }
        let body = response.bytes().await.map_err(|err| self.transport(&err))?;
        let latency_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);

        let response: Response = serde_json::from_slice(&body).map_err(|err| malformed(&err))?;
        let cost_usd = match response.usage.and_then(|usage| usage.cost) {
            Some(cost) if cost < 0.0 => return Err(ChatError::Malformed("the cost is negative".to_owned())),
            cost => cost,
        };
        let choice = response.choices.into_iter().next().ok_or(ChatError::Empty)?;
        match choice.finish_reason.as_deref() {
            None | Some("stop") => {}
            Some("length") => return Err(ChatError::Unfinished("it ran out of tokens")),
            Some("content_filter") => return Err(ChatError::Unfinished("a content filter stopped it")),
            Some(_) => return Err(ChatError::Unfinished("it stopped for another reason")),
        }
        let content = choice
            .message
            .content
            .filter(|content| !content.trim().is_empty());
        Ok(Completion {
            content: content.ok_or(ChatError::Empty)?,
            model: response.model,
            request_id: response.id,
            cost_usd,
            latency_ms,
        })
    }
}

#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    messages: &'a [Message],
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
    provider: &'a ProviderSettings,
}

#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Deserialize)]
struct Response {
    id: Option<String>,
    model: Option<String>,
    choices: Vec<ResponseChoice>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct ResponseChoice {
    message: ResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: Option<String>,
}

#[derive(Deserialize)]
struct Usage {
    cost: Option<f64>,
}

/// Where the parse failed, without quoting the body.
fn malformed(err: &serde_json::Error) -> ChatError {
    ChatError::Malformed(format!(
        "{:?} error at line {} column {}",
        err.classify(),
        err.line(),
        err.column()
    ))
}
