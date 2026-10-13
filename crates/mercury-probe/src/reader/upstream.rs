//! The reader's one call to its one upstream, an OpenAI-compatible chat endpoint. It mirrors what
//! `mercury-judge`'s chat client does with a reply (the cost, `finish_reason`), without taking that
//! crate in: the image that reads code carries none of the judge's code.

use std::fmt;
use std::time::Duration;

use reqwest::Url;
use serde::Deserialize;
use serde_json::{Value, json};

use super::wire::{ErrorCode, ReadRequest};

/// How long the reader waits for its upstream: under `serve`'s 40 s and the core's 45 s, so each
/// layer fails with its own error before its caller's clock ends.
pub const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(35);

/// The largest reply the reader takes from its upstream, in bytes.
const MAX_REPLY_BYTES: usize = 1024 * 1024;

/// The key, read once from its file. `Debug` never prints it.
#[derive(Clone)]
pub struct Key(String);

impl Key {
    pub fn new(key: String) -> Self {
        Self(key)
    }

    fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Key(<set>)")
    }
}

/// What the model said and what the call cost.
#[derive(Debug, PartialEq)]
pub struct Answer {
    pub content: String,
    pub cost_usd: f64,
}

/// Why the upstream gave no answer: the code the reader returns, and a few words for its log. The
/// words never hold a body, a header or an error message of the upstream's.
#[derive(Debug, PartialEq, Eq)]
pub struct Failure {
    pub code: ErrorCode,
    pub kind: String,
}

impl Failure {
    fn new(code: ErrorCode, kind: impl Into<String>) -> Self {
        Self {
            code,
            kind: kind.into(),
        }
    }

    fn failed(kind: impl Into<String>) -> Self {
        Self::new(ErrorCode::UpstreamFailed, kind)
    }
}

/// The body sent upstream. An outside upstream is only ever one that neither collects nor retains
/// what it is sent (an endpoint that stops meeting that fails the request, so a change of policy
/// fails closed), and reports what the call cost.
pub fn body(request: &ReadRequest, outside: bool) -> Value {
    let mut body = json!({
        "model": request.model,
        "messages": request.messages,
        "max_tokens": request.max_tokens,
        "temperature": 0,
    });
    if outside {
        body["provider"] = json!({ "data_collection": "deny", "zdr": true });
        body["usage"] = json!({ "include": true });
    }
    body
}

pub struct Upstream {
    client: reqwest::Client,
    url: Url,
    outside: bool,
    key: Option<Key>,
}

impl Upstream {
    /// A private upstream is reached directly, an outside one through `HTTPS_PROXY`, which the
    /// start has checked is set. A redirect is never followed: the allowlist names one host.
    pub fn new(url: Url, outside: bool, key: Option<Key>, timeout: Duration) -> Result<Self, String> {
        let mut builder = reqwest::Client::builder()
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none());
        if !outside {
            builder = builder.no_proxy();
        }
        Ok(Self {
            client: builder
                .build()
                .map_err(|_| "the HTTP client for the upstream can't be built".to_owned())?,
            url,
            outside,
            key,
        })
    }

    pub async fn ask(&self, request: &ReadRequest) -> Result<Answer, Failure> {
        let mut post = self
            .client
            .post(self.url.clone())
            .json(&body(request, self.outside));
        if let Some(key) = &self.key {
            // `bearer_auth` marks the header sensitive, so no log of the client prints it
            post = post.bearer_auth(key.expose());
        }
        let response = post.send().await.map_err(|err| transport(&err))?;
        let status = response.status();
        if !status.is_success() {
            return Err(Failure::failed(format!("status {}", status.as_u16())));
        }
        let bytes = response.bytes().await.map_err(|err| transport(&err))?;
        if bytes.len() > MAX_REPLY_BYTES {
            return Err(Failure::failed("a reply over the size cap"));
        }
        let reply: Response =
            serde_json::from_slice(&bytes).map_err(|_| Failure::failed("a reply of another shape"))?;
        let cost = reply.usage.and_then(|usage| usage.cost);
        if cost.is_some_and(|cost| !cost.is_finite() || cost < 0.0) {
            return Err(Failure::failed("a negative cost"));
        }
        let choice = reply
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| Failure::failed("no choice"))?;
        match choice.finish_reason.as_deref() {
            None | Some("stop") => {}
            Some("length") => return Err(Failure::failed("it ran out of tokens")),
            Some("content_filter") => return Err(Failure::failed("a content filter stopped it")),
            Some(_) => return Err(Failure::failed("it stopped for another reason")),
        }
        let content = choice
            .message
            .content
            .filter(|content| !content.trim().is_empty())
            .ok_or_else(|| Failure::failed("an empty reply"))?;
        // an upstream that bills and says nothing of the cost leaves the day's spend unknown
        let cost_usd = match cost {
            Some(cost) => cost,
            None if self.outside => return Err(Failure::new(ErrorCode::NoCost, "no cost reported")),
            None => 0.0,
        };
        Ok(Answer { content, cost_usd })
    }
}

/// A client error reduced to its kind: the error's own text can name the URL.
fn transport(err: &reqwest::Error) -> Failure {
    if err.is_timeout() {
        Failure::new(ErrorCode::UpstreamTimeout, "timed out")
    } else if err.is_connect() {
        Failure::failed("couldn't connect")
    } else {
        Failure::failed("transport error")
    }
}

#[derive(Deserialize)]
struct Response {
    choices: Vec<Choice>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Choice {
    message: Content,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct Content {
    content: Option<String>,
}

#[derive(Deserialize)]
struct Usage {
    cost: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::wire::{Message, Role};

    fn request() -> ReadRequest {
        ReadRequest {
            model: "demo/model".into(),
            messages: vec![
                Message {
                    role: Role::System,
                    content: "s".into(),
                },
                Message {
                    role: Role::User,
                    content: "u".into(),
                },
            ],
            max_tokens: 1_500,
        }
    }

    #[test]
    fn the_outside_upstream_gets_zero_retention_and_usage_accounting() {
        let outside = body(&request(), true);
        assert_eq!(
            outside["provider"],
            json!({ "data_collection": "deny", "zdr": true })
        );
        assert_eq!(outside["usage"], json!({ "include": true }));
        assert_eq!(outside["temperature"], 0);
        assert_eq!(outside["model"], "demo/model");
        assert_eq!(outside["max_tokens"], 1_500);
        assert_eq!(outside["messages"][0]["role"], "system");
        assert_eq!(outside["messages"][1]["content"], "u");
        // a private upstream is sent the request and nothing about a provider
        let private = body(&request(), false);
        assert!(private.get("provider").is_none() && private.get("usage").is_none());
        assert_eq!(private["temperature"], 0);
    }

    #[test]
    fn the_key_is_never_printed() {
        assert!(!format!("{:?}", Key::new("sk-secret-value".into())).contains("secret"));
    }
}
