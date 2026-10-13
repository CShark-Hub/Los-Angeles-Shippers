//! The core's HTTP API as the CLI uses it: the token as Bearer, the bodies of `mercury-schema`, a
//! question's event stream, and the API's error body as a [`CliError`]. Redirects aren't
//! followed, so the token goes to no other server than the one it was given for.

use std::collections::VecDeque;
use std::time::Duration;

use mercury_schema::api::{
    Accepted, ApprovalDecision, ApprovalList, AuditPage, ErrorBody, Me, NewQuestion, ProjectList,
    QuestionState, TokenSecret,
};
use mercury_schema::events::Event;
use mercury_schema::ids::{ApprovalId, QuestionId};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderValue};
use reqwest::{RequestBuilder, Response};
use serde::de::DeserializeOwned;

use crate::credentials::{Login, Server};
use crate::error::CliError;
use crate::sse::{Frame, Reader, SseError};

/// How long a request may take; the event stream has no end time, only the idle limit below.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the event stream may stay silent before the CLI reconnects.
pub const STREAM_IDLE: Duration = Duration::from_secs(120);
/// The largest JSON body the CLI reads.
pub const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
/// Audit rows per page the CLI asks for: the server's most.
pub const AUDIT_PAGE: &str = "500";

/// The key that makes a retried `POST /v1/questions` the same question: 128 random bits in hex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn new() -> Result<Self, CliError> {
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|_| CliError::NoRandom)?;
        Ok(Self(bytes.iter().map(|byte| format!("{byte:02x}")).collect()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub struct Client {
    http: reqwest::Client,
    server: Server,
    token: TokenSecret,
    /// The longest silence of a body that is streamed, an export's, before the CLI gives up on it.
    idle: Duration,
}

impl Client {
    pub fn new(login: &Login) -> Result<Self, CliError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("mercury/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| CliError::Unreachable {
                server: login.server.to_string(),
                reason: "the HTTP client can't start",
            })?;
        Ok(Self {
            http,
            server: login.server.clone(),
            token: login.token.clone(),
            idle: STREAM_IDLE,
        })
    }

    /// The same client with another longest silence for a streamed body, for tests that can't wait
    /// two minutes.
    #[must_use]
    pub fn with_idle(mut self, idle: Duration) -> Self {
        self.idle = idle;
        self
    }

    pub fn server(&self) -> &Server {
        &self.server
    }

    pub async fn me(&self) -> Result<Me, CliError> {
        self.get_json("/v1/me").await
    }

    pub async fn projects(&self) -> Result<ProjectList, CliError> {
        self.get_json("/v1/projects").await
    }

    /// A page of `GET /v1/audit` with `filters`, from the start or after `cursor`.
    pub async fn audit(
        &self,
        filters: &[(&str, String)],
        cursor: Option<&str>,
    ) -> Result<AuditPage, CliError> {
        let mut request = self
            .http
            .get(self.server.endpoint("/v1/audit"))
            .header(AUTHORIZATION, self.bearer()?)
            .timeout(REQUEST_TIMEOUT)
            .query(filters)
            .query(&[("limit", AUDIT_PAGE)]);
        if let Some(cursor) = cursor {
            request = request.query(&[("cursor", cursor)]);
        }
        read_json(self.send(request).await?).await
    }

    /// `GET /v1/audit/export` with `filters`, streamed into `file`; how many rows it wrote. It has no
    /// time limit, as an export can be long, only the connection's and a limit on the silence between
    /// two chunks: a server that stops sending ends the export with an error, as it ends the event
    /// stream.
    pub async fn audit_export(
        &self,
        filters: &[(&str, String)],
        file: &mut dyn std::io::Write,
    ) -> Result<u64, CliError> {
        let request = self
            .http
            .get(self.server.endpoint("/v1/audit/export"))
            .header(AUTHORIZATION, self.bearer()?)
            .query(&[("format", "jsonl")])
            .query(filters);
        let mut response = self.send(request).await?;
        let mut rows = 0;
        while let Some(chunk) = tokio::time::timeout(self.idle, response.chunk())
            .await
            .map_err(|_| CliError::Unreachable {
                server: self.server.to_string(),
                reason: "the export stopped sending",
            })?
            .map_err(|_| CliError::Protocol("the export broke off".to_owned()))?
        {
            file.write_all(&chunk)?;
            rows += chunk.iter().filter(|&&byte| byte == b'\n').count() as u64;
        }
        file.flush()?;
        Ok(rows)
    }

    /// `GET /v1/approvals?status=pending`: the answers waiting for the caller's sign-off.
    pub async fn approvals(&self) -> Result<ApprovalList, CliError> {
        self.get_json("/v1/approvals?status=pending").await
    }

    /// `POST /v1/approvals/{id}`: approve or reject one held answer.
    pub async fn decide(&self, id: &ApprovalId, decision: &ApprovalDecision) -> Result<(), CliError> {
        let request = self
            .http
            .post(self.server.endpoint(&format!("/v1/approvals/{id}")))
            .header(AUTHORIZATION, self.bearer()?)
            .timeout(REQUEST_TIMEOUT)
            .json(decision);
        self.send(request).await?;
        Ok(())
    }

    /// `POST /v1/questions`; a retry with the same key is the same question to the server.
    pub async fn ask(&self, question: &NewQuestion, key: &IdempotencyKey) -> Result<Accepted, CliError> {
        let request = self
            .http
            .post(self.server.endpoint("/v1/questions"))
            .header(AUTHORIZATION, self.bearer()?)
            .header("idempotency-key", key.as_str())
            .timeout(REQUEST_TIMEOUT)
            .json(question);
        read_json(self.send(request).await?).await
    }

    /// `GET /v1/questions/{id}`: where the question stands, whatever its events say.
    pub async fn question(&self, id: &QuestionId) -> Result<QuestionState, CliError> {
        self.get_json(&format!("/v1/questions/{id}")).await
    }

    /// `GET /v1/questions/{id}/events`, from the start or after `last_event_id`.
    pub async fn events(
        &self,
        id: &QuestionId,
        last_event_id: Option<&str>,
    ) -> Result<EventStream, CliError> {
        let mut request = self
            .http
            .get(self.server.endpoint(&format!("/v1/questions/{id}/events")))
            .header(AUTHORIZATION, self.bearer()?)
            .header(ACCEPT, "text/event-stream");
        if let Some(last) = last_event_id {
            request = request.header("last-event-id", last);
        }
        Ok(EventStream {
            response: self.send(request).await?,
            reader: Reader::new(),
            ready: VecDeque::new(),
        })
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, CliError> {
        let request = self
            .http
            .get(self.server.endpoint(path))
            .header(AUTHORIZATION, self.bearer()?)
            .timeout(REQUEST_TIMEOUT);
        read_json(self.send(request).await?).await
    }

    fn bearer(&self) -> Result<HeaderValue, CliError> {
        let mut value = HeaderValue::from_str(&format!("Bearer {}", self.token.expose()))
            .map_err(|_| CliError::BadToken)?;
        value.set_sensitive(true);
        Ok(value)
    }

    /// Sends; an answer other than 2xx becomes the API's error, or `Status` when it isn't one.
    async fn send(&self, request: RequestBuilder) -> Result<Response, CliError> {
        let response = request.send().await.map_err(|err| CliError::Unreachable {
            server: self.server.to_string(),
            reason: if err.is_timeout() {
                "the request timed out"
            } else if err.is_connect() {
                "the connection failed"
            } else {
                "the request failed"
            },
        })?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let body = read_body(response).await.unwrap_or_default();
        Err(match serde_json::from_slice::<ErrorBody>(&body) {
            Ok(body) => CliError::Api {
                status: status.as_u16(),
                code: body.error.code,
                message: body.error.message,
                hint: body.error.hint,
            },
            Err(_) => CliError::Status(status.as_u16()),
        })
    }
}

async fn read_body(mut response: Response) -> Result<Vec<u8>, CliError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| CliError::Protocol("the body broke off".to_owned()))?
    {
        if body.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(CliError::Protocol(format!(
                "a body larger than {MAX_BODY_BYTES} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn read_json<T: DeserializeOwned>(response: Response) -> Result<T, CliError> {
    let body = read_body(response).await?;
    serde_json::from_slice(&body).map_err(|err| CliError::Protocol(format!("an unexpected body ({err})")))
}

/// Why a stream ended before its last event.
#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    #[error("the stream was silent for {} s", STREAM_IDLE.as_secs())]
    Silent,
    #[error("the stream broke off")]
    Dropped,
    #[error(transparent)]
    Malformed(#[from] SseError),
}

/// A question's events as they arrive.
pub struct EventStream {
    response: Response,
    reader: Reader,
    ready: VecDeque<Frame>,
}

impl EventStream {
    /// The next event as sent; `None` when the server closed the stream.
    pub async fn next_frame(&mut self) -> Result<Option<Frame>, StreamError> {
        loop {
            if let Some(frame) = self.ready.pop_front() {
                return Ok(Some(frame));
            }
            let chunk = tokio::time::timeout(STREAM_IDLE, self.response.chunk())
                .await
                .map_err(|_| StreamError::Silent)?
                .map_err(|_| StreamError::Dropped)?;
            match chunk {
                Some(bytes) => self.ready.extend(self.reader.feed(&bytes)?),
                None => return Ok(None),
            }
        }
    }
}

/// The event a frame carries; `None` for an event this CLI doesn't know, which a newer server
/// may send (additive changes stay in `/v1`), and for a row whose data is `null`, which retention
/// empties: it has nothing to show, and the question's state says how it ended. A known event
/// that doesn't parse is an error.
pub fn event_of(frame: &Frame) -> Result<Option<Event>, CliError> {
    if !Event::NAMES.contains(&frame.event.as_str()) {
        return Ok(None);
    }
    let broken = |what: &str| CliError::Protocol(format!("the {} event {what}", frame.event));
    let data: serde_json::Value = serde_json::from_str(&frame.data).map_err(|_| broken("isn't JSON"))?;
    if data.is_null() {
        return Ok(None);
    }
    serde_json::from_value(serde_json::json!({ "event": frame.event, "data": data }))
        .map(Some)
        .map_err(|_| broken("doesn't match the API"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(event: &str, data: &str) -> Frame {
        Frame {
            event: event.to_owned(),
            data: data.to_owned(),
            last_id: None,
        }
    }

    #[test]
    fn a_frame_becomes_its_event() {
        let event = event_of(&frame("judged", r#"{"sentences":4,"stripped":1,"held":0}"#))
            .unwrap()
            .unwrap();
        assert_eq!(event.name(), "judged");
        assert!(event_of(&frame("composing", "{}")).unwrap().is_some());
    }

    #[test]
    fn an_unknown_event_is_skipped_and_a_broken_one_is_an_error() {
        assert!(event_of(&frame("progress_hint", r#"{"x":1}"#)).unwrap().is_none());
        assert!(event_of(&frame("message", "hello")).unwrap().is_none());
        assert!(matches!(
            event_of(&frame("judged", "{not json")),
            Err(CliError::Protocol(_))
        ));
        assert!(matches!(
            event_of(&frame("judged", r#"{"sentences":-1}"#)),
            Err(CliError::Protocol(_))
        ));
    }

    /// Retention empties a row's data to `null`; the row has nothing to show, and it must not
    /// fail the whole question as "doesn't match the API".
    #[test]
    fn an_event_whose_data_retention_emptied_is_skipped() {
        for name in Event::NAMES {
            assert!(event_of(&frame(name, "null")).unwrap().is_none(), "{name}");
        }
        // only null is skipped: a known event with data of another shape is still an error
        assert!(matches!(
            event_of(&frame("project", "[]")),
            Err(CliError::Protocol(_))
        ));
        assert!(matches!(
            event_of(&frame("project", "{}")),
            Err(CliError::Protocol(_))
        ));
    }

    #[test]
    fn idempotency_keys_are_fresh_128_bit_hex() {
        let (one, two) = (IdempotencyKey::new().unwrap(), IdempotencyKey::new().unwrap());
        assert_eq!(one.as_str().len(), 32);
        assert!(one.as_str().bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(one, two);
    }
}
