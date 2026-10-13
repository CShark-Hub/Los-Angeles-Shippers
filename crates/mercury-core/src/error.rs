//! The API's errors, for requests that can't start: a status, the shared error body
//! `{"error": {"code", "message", "hint"}}`, and `Retry-After` or `WWW-Authenticate` where they
//! belong.

use std::time::Duration;

use axum::Json;
use axum::http::header::{RETRY_AFTER, WWW_AUTHENTICATE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
pub use mercury_schema::api::ErrorCode;
use mercury_schema::api::{ErrorBody, ErrorDetail};

/// An error the API answers with. It never carries the request's token or text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: ErrorCode,
    pub message: String,
    pub hint: String,
    /// Whole seconds for `Retry-After`.
    pub retry_after: Option<u64>,
}

impl ApiError {
    fn new(status: StatusCode, code: ErrorCode, message: &str, hint: &str) -> Self {
        Self {
            status,
            code,
            message: message.to_owned(),
            hint: hint.to_owned(),
            retry_after: None,
        }
    }

    /// No token, a malformed or unknown one, a wrong secret, a revoked token or a disabled user:
    /// one answer for all of them.
    pub fn unauthenticated() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            ErrorCode::Unauthenticated,
            "The request has no valid token.",
            "Sign in again with your token, or ask the admin for a new one.",
        )
    }

    pub fn token_expired() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            ErrorCode::TokenExpired,
            "The token has expired.",
            "Ask the admin for a new token.",
        )
    }

    /// `wait` is how long until the token may ask again; `Retry-After` rounds it up.
    pub fn rate_limited(wait: Duration) -> Self {
        let seconds = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
        let seconds = seconds.max(1);
        Self {
            retry_after: Some(seconds),
            ..Self::new(
                StatusCode::TOO_MANY_REQUESTS,
                ErrorCode::RateLimited,
                "Too many requests with this token.",
                &format!("Wait {seconds} seconds, then try again."),
            )
        }
    }

    /// A request that changes state with the session cookie, or signs in, without the web client's
    /// own `Origin` and `X-Mercury-Client` header.
    pub fn not_from_the_web_client() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            ErrorCode::InvalidRequest,
            "This request doesn't come from Mercury's own web page.",
            "Use Mercury at its own web address. A script or an agent sends its token as Authorization: Bearer instead.",
        )
    }

    /// A request to `/mcp` whose `Origin` isn't Mercury's own page: MCP's rule against DNS
    /// rebinding. An agent sends no `Origin` at all.
    pub fn foreign_origin() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            ErrorCode::InvalidRequest,
            "The request comes from a page that isn't Mercury's own.",
            "An agent calls /mcp without an Origin header, with its token as Authorization: Bearer.",
        )
    }

    pub fn body_too_large() -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::InvalidRequest,
            "The request body is larger than 64 KB.",
            "Send a shorter request.",
        )
    }

    pub fn unreadable_body() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidRequest,
            "The request body could not be read.",
            "Send the request again.",
        )
    }

    /// The project's probe can't answer: down, slow, or quarantined for breaking the contract.
    pub fn probe_unavailable() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::ProbeUnavailable,
            "The project's probe can't answer right now.",
            "Try again in a few minutes; if it keeps failing, tell the admin.",
        )
    }

    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
            "The server could not handle the request.",
            "Try again; if it keeps failing, tell the admin.",
        )
    }

    /// The error's body, `{"error": {"code", "message", "hint"}}`, for an answer that isn't an
    /// HTTP response of its own: a tool result at `/mcp`.
    pub fn body(&self) -> ErrorBody {
        ErrorBody {
            error: ErrorDetail {
                code: self.code,
                message: self.message.clone(),
                hint: Some(self.hint.clone()),
            },
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (self.status, Json(self.body())).into_response();
        let headers = response.headers_mut();
        if let Some(seconds) = self.retry_after {
            headers.insert(RETRY_AFTER, HeaderValue::from(seconds));
        }
        if self.status == StatusCode::UNAUTHORIZED {
            headers.insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt;

    use super::*;

    async fn body(error: ApiError) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
        let response = error.into_response();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, headers, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn the_body_has_the_contract_shape() {
        let (status, headers, json) = body(ApiError::unauthenticated()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(json["error"]["code"], "unauthenticated");
        assert!(json["error"]["message"].is_string() && json["error"]["hint"].is_string());
        assert_eq!(json.as_object().unwrap().len(), 1);
        assert_eq!(headers[WWW_AUTHENTICATE], "Bearer");
        assert!(headers.get(RETRY_AFTER).is_none());
    }

    #[tokio::test]
    async fn rate_limited_rounds_retry_after_up_to_whole_seconds() {
        let (status, headers, json) = body(ApiError::rate_limited(Duration::from_millis(119_001))).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(json["error"]["code"], "rate_limited");
        assert_eq!(headers[RETRY_AFTER], "120");
        assert!(headers.get(WWW_AUTHENTICATE).is_none());
        assert_eq!(
            ApiError::rate_limited(Duration::ZERO).retry_after,
            Some(1),
            "never 0"
        );
    }

    #[tokio::test]
    async fn an_oversized_body_is_an_invalid_request() {
        let (status, _, json) = body(ApiError::body_too_large()).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(json["error"]["code"], "invalid_request");
    }

    #[test]
    fn the_codes_are_the_contracts_names() {
        let names: Vec<String> = [
            ErrorCode::Unauthenticated,
            ErrorCode::TokenExpired,
            ErrorCode::RateLimited,
            ErrorCode::InvalidRequest,
            ErrorCode::Internal,
        ]
        .iter()
        .map(|code| serde_json::to_value(code).unwrap().as_str().unwrap().to_owned())
        .collect();
        assert_eq!(
            names,
            [
                "unauthenticated",
                "token_expired",
                "rate_limited",
                "invalid_request",
                "internal"
            ]
        );
    }
}
