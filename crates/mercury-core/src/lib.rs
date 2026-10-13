//! The gateway. For now: the public health check, the internal listener's source check, the
//! request log, the container health check, the database, personal tokens with their limits,
//! the web client's sessions, the admin commands and the admin panel's read-only routes, the audit
//! chain with its routes and retention, the configuration and the policy, the probes' way to the
//! judge, MCP for AI agents, and the alerts the core raises.

pub mod admin;
pub mod admin_routes;
pub mod alerts;
pub mod approvals;
pub mod audit;
pub mod audit_routes;
pub mod backup;
pub mod cli;
pub mod config;
pub mod db;
pub mod error;
pub mod healthcheck;
pub mod identity;
pub mod internal;
pub mod limits;
pub mod line_editor;
pub mod logging;
pub mod mcp;
pub mod orchestrator;
pub mod policy;
pub mod probe_judge;
pub mod probes;
pub mod questions;
pub mod session;
pub mod shell;
pub mod status;
pub mod terminal;
pub mod welcome;

use std::net::SocketAddr;

use axum::{Json, Router, middleware, routing::get};
use serde::Serialize;

/// Where the public listener binds when `MERCURY_PUBLIC_ADDR` is not set.
pub const DEFAULT_PUBLIC_ADDR: &str = "127.0.0.1:8080";

/// Where the internal listener binds when `MERCURY_INTERNAL_ADDR` is not set.
pub const DEFAULT_INTERNAL_ADDR: &str = "127.0.0.1:8081";

/// The body of `GET /v1/health`.
#[derive(Debug, Serialize)]
struct Health {
    status: &'static str,
    version: &'static str,
}

async fn health() -> Json<Health> {
    Json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// The public listener's health check alone, for what runs without a database.
pub fn router() -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .layer(middleware::from_fn_with_state("public", logging::log_request))
}

/// The public API: the health check, the web client's sessions, the questions, the people's
/// routes, the approvals, the audit, the admin panel's reads, and `/mcp` for agents, which takes
/// Bearer tokens only. Serve
/// it with `into_make_service_with_connect_info::<SocketAddr>()`: the sign-in limit counts by the
/// peer's address.
pub fn app(questions: questions::Questions) -> Router {
    let sessions = questions.sessions.clone();
    Router::new()
        .route("/v1/health", get(health))
        .merge(session::routes(sessions))
        .merge(audit_routes::routes(questions.clone()))
        .merge(admin_routes::routes(questions.clone()))
        .merge(approvals::routes(questions.clone()))
        .merge(mcp::routes(questions.clone()))
        .merge(questions::routes(questions))
        .layer(middleware::from_fn_with_state("public", logging::log_request))
}

/// The public listener's address: `value` (from `MERCURY_PUBLIC_ADDR`) or the default.
pub fn public_addr(value: Option<&str>) -> Result<SocketAddr, String> {
    listen_addr("MERCURY_PUBLIC_ADDR", value, DEFAULT_PUBLIC_ADDR)
}

/// The internal listener's address: `value` (from `MERCURY_INTERNAL_ADDR`) or the default.
pub fn internal_addr(value: Option<&str>) -> Result<SocketAddr, String> {
    listen_addr("MERCURY_INTERNAL_ADDR", value, DEFAULT_INTERNAL_ADDR)
}

fn listen_addr(name: &str, value: Option<&str>, default: &str) -> Result<SocketAddr, String> {
    let value = value.unwrap_or(default);
    value
        .parse()
        .map_err(|_| format!("{name} must be an address like {default}, not {value:?}"))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    async fn call(method: Method, uri: &str) -> (StatusCode, String) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        let response = router().oneshot(request).await.unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn health_answers_ok_with_the_product_version() {
        let (status, body) = call(Method::GET, "/v1/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, r#"{"status":"ok","version":"0.0.0"}"#);
    }

    #[tokio::test]
    async fn health_answers_only_get() {
        let (status, _) = call(Method::POST, "/v1/health").await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn an_unknown_path_is_not_found() {
        let (status, _) = call(Method::GET, "/v1/nothing").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn the_public_address_defaults_to_the_loopback() {
        assert_eq!(public_addr(None).unwrap().to_string(), "127.0.0.1:8080");
        assert_eq!(
            public_addr(Some("0.0.0.0:9000")).unwrap().to_string(),
            "0.0.0.0:9000"
        );
    }

    #[test]
    fn a_public_address_without_a_port_is_refused() {
        assert!(public_addr(Some("localhost")).is_err());
    }

    #[test]
    fn the_internal_address_defaults_to_the_loopback() {
        assert_eq!(internal_addr(None).unwrap().to_string(), "127.0.0.1:8081");
        assert!(
            internal_addr(Some("8081"))
                .unwrap_err()
                .contains("MERCURY_INTERNAL_ADDR")
        );
    }
}
