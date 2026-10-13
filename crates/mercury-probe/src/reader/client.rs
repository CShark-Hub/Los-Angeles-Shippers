//! `serve`'s side of the code reader: where it is, and one request to it. `serve` has no route out
//! and no key; the reader's URL is a host on its own network (the table in [`super::hosts`]).

use std::fmt;
use std::time::Duration;

use reqwest::Url;

use super::hosts;
use super::wire::{MAX_BODY_BYTES, Message, ReadReply, ReadRequest, Role, SERVE_MAX_TOKENS};

/// How long `serve` waits for the reader: under the core's 45 s for `ask_code`, over the reader's
/// 35 s for its upstream, so each layer fails with its own error.
pub const READER_TIMEOUT: Duration = Duration::from_secs(40);

/// What `PROBE_CODE_READER_MAX_KB` is when it isn't set: about 40 k tokens.
pub const DEFAULT_MAX_KB: usize = 150;

/// The range `PROBE_CODE_READER_MAX_KB` accepts.
pub const MAX_KB_RANGE: std::ops::RangeInclusive<usize> = 1..=900;

/// `PROBE_CODE_READER_URL` when it is set: `None` when unset or empty after a trim. The start stops,
/// with a line naming the variable and never its value, when it isn't an `http(s)` URL to a private
/// host.
pub fn configured_url(var: impl Fn(&str) -> Option<String>) -> Result<Option<Url>, String> {
    let Some(value) = var("PROBE_CODE_READER_URL")
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
    else {
        return Ok(None);
    };
    let url = hosts::parse_url(&value)
        .map_err(|problem| format!("PROBE_CODE_READER_URL isn't usable: {problem}"))?;
    if !hosts::is_private(&url) {
        return Err(
            "PROBE_CODE_READER_URL must name a host on the probe's own network: serve never reaches outside it"
                .to_owned(),
        );
    }
    Ok(Some(url))
}

/// Why the reader gave no answer. Each is a kind to log, never a body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadFailure {
    Unreachable,
    Timeout,
    Status(u16),
    Malformed,
}

impl fmt::Display for ReadFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadFailure::Unreachable => f.write_str("the reader can't be reached"),
            ReadFailure::Timeout => f.write_str("the reader timed out"),
            ReadFailure::Status(status) => write!(f, "the reader answered {status}"),
            ReadFailure::Malformed => f.write_str("the reader's reply was malformed"),
        }
    }
}

/// The code reader as `serve` calls it.
#[derive(Debug, Clone)]
pub struct ReaderClient {
    http: reqwest::Client,
    url: String,
    host: String,
    model: String,
    max_bytes: usize,
}

impl ReaderClient {
    /// The reader named by the environment: `PROBE_CODE_READER_URL`, with `PROBE_CODE_READER_MODEL`
    /// (required with it) and `PROBE_CODE_READER_MAX_KB`. `None` when no URL is set.
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, String> {
        let Some(url) = configured_url(&var)? else {
            return Ok(None);
        };
        let set = |name: &str| var(name).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
        let model = set("PROBE_CODE_READER_MODEL")
            .ok_or("PROBE_CODE_READER_MODEL is not set: it is required with PROBE_CODE_READER_URL")?;
        let max_kb = match set("PROBE_CODE_READER_MAX_KB") {
            None => DEFAULT_MAX_KB,
            Some(value) => value
                .parse::<usize>()
                .ok()
                .filter(|kb| MAX_KB_RANGE.contains(kb))
                .ok_or("PROBE_CODE_READER_MAX_KB must be a whole number of kilobytes from 1 to 900")?,
        };
        Ok(Some(Self::new(&url, model, max_kb, READER_TIMEOUT)?))
    }

    pub fn new(url: &Url, model: String, max_kb: usize, timeout: Duration) -> Result<Self, String> {
        // the probes network has no proxy, and nothing here may take one
        let http = reqwest::Client::builder()
            .timeout(timeout)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "the HTTP client for the code reader can't be built".to_owned())?;
        Ok(Self {
            http,
            url: format!("{}/read", url.as_str().trim_end_matches('/')),
            host: url.host_str().unwrap_or("?").to_owned(),
            model,
            max_bytes: max_kb.saturating_mul(1024),
        })
    }

    /// The host the reader is at, for the log: never a path or a query.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The most the snapshot's files may come to, in bytes.
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// One request: the system prompt and the user message.
    pub async fn read(&self, system: String, user: String) -> Result<ReadReply, ReadFailure> {
        let request = ReadRequest {
            model: self.model.clone(),
            messages: vec![
                Message {
                    role: Role::System,
                    content: system,
                },
                Message {
                    role: Role::User,
                    content: user,
                },
            ],
            max_tokens: SERVE_MAX_TOKENS,
        };
        let response = self
            .http
            .post(&self.url)
            .json(&request)
            .send()
            .await
            .map_err(|err| {
                if err.is_timeout() {
                    ReadFailure::Timeout
                } else {
                    ReadFailure::Unreachable
                }
            })?;
        let status = response.status();
        if status.as_u16() != 200 {
            return Err(ReadFailure::Status(status.as_u16()));
        }
        let bytes = response.bytes().await.map_err(|err| {
            if err.is_timeout() {
                ReadFailure::Timeout
            } else {
                ReadFailure::Malformed
            }
        })?;
        if bytes.len() > MAX_BODY_BYTES {
            return Err(ReadFailure::Malformed);
        }
        let reply: ReadReply = serde_json::from_slice(&bytes).map_err(|_| ReadFailure::Malformed)?;
        if !reply.cost_usd.is_finite() || reply.cost_usd < 0.0 {
            return Err(ReadFailure::Malformed);
        }
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    #[test]
    fn no_url_or_an_empty_one_is_no_reader() {
        assert!(configured_url(env(&[])).unwrap().is_none());
        assert!(
            configured_url(env(&[("PROBE_CODE_READER_URL", "  ")]))
                .unwrap()
                .is_none()
        );
        assert!(
            ReaderClient::from_env(env(&[("PROBE_CODE_READER_URL", "")]))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn the_url_must_be_a_private_http_url_and_the_error_names_the_variable() {
        for url in [
            "https://openrouter.ai/api/v1",
            "http://8.8.8.8/",
            "ftp://code-reader/",
            "code-reader",
            "http://u:p@code-reader/",
        ] {
            let err = configured_url(env(&[("PROBE_CODE_READER_URL", url)])).unwrap_err();
            assert!(err.starts_with("PROBE_CODE_READER_URL"), "{err}");
            assert!(!err.contains(url), "the error quotes the value: {err}");
        }
        for url in [
            "http://code-reader:7100",
            "http://10.0.0.5:7100/",
            "http://llm.internal/",
        ] {
            assert!(
                configured_url(env(&[("PROBE_CODE_READER_URL", url)]))
                    .unwrap()
                    .is_some(),
                "{url}"
            );
        }
    }

    #[test]
    fn the_model_is_required_with_the_url_and_the_size_cap_is_bounded() {
        let url = ("PROBE_CODE_READER_URL", "http://code-reader:7100");
        let err = ReaderClient::from_env(env(&[url])).unwrap_err();
        assert!(err.starts_with("PROBE_CODE_READER_MODEL"), "{err}");
        let model = ("PROBE_CODE_READER_MODEL", "demo/model");
        let client = ReaderClient::from_env(env(&[url, model])).unwrap().unwrap();
        assert_eq!(client.max_bytes(), 150 * 1024);
        assert_eq!(client.host(), "code-reader");
        for (kb, ok) in [
            ("1", true),
            ("900", true),
            ("0", false),
            ("901", false),
            ("lots", false),
            ("-5", false),
        ] {
            let kb = ("PROBE_CODE_READER_MAX_KB", kb);
            assert_eq!(
                ReaderClient::from_env(env(&[url, model, kb])).is_ok(),
                ok,
                "{kb:?}"
            );
        }
        let kb = ("PROBE_CODE_READER_MAX_KB", "40");
        assert_eq!(
            ReaderClient::from_env(env(&[url, model, kb]))
                .unwrap()
                .unwrap()
                .max_bytes(),
            40 * 1024
        );
    }
}
