//! The limits as a caller meets them, route by route: the body cap of every route that reads a body,
//! and the rate limit of a token under a flood of questions. Each limit has its own unit test beside
//! its code; what is here is the whole public router, so a route added without its cap, or a limit
//! that stops counting, fails a test of its own.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE, ORIGIN, RETRY_AFTER};
use axum::http::{Request, StatusCode};
use chrono::TimeDelta;
use http_body_util::BodyExt;
use mercury_core::db::{self, Db};
use mercury_core::limits::MAX_BODY;
use mercury_core::questions::{Asked, Emitter, Questions, Runner};
use mercury_core::session::SessionConfig;
use mercury_core::{admin, app};
use mercury_schema::Role;
use serde_json::{Value, json};
use tower::ServiceExt;

/// The web client's own origin, as the core is set up with it.
const PAGE: &str = "https://mercury.example.com";

/// A question that is accepted and never run: these tests are about what happens before.
struct Idle;

#[async_trait]
impl Runner for Idle {
    async fn run(&self, _: Asked, _: Emitter) {}
}

struct Core {
    _dir: tempfile::TempDir,
    db: Db,
    app: Router,
    anna: String,
    ben: String,
    /// The questions a token may start at once, from the example configuration.
    burst: u32,
}

async fn person(db: &Db, email: &str) -> String {
    let now = db::now();
    admin::add_user(db, email, "Pat Example", Role::Pm, now)
        .await
        .unwrap();
    admin::grant(
        db,
        email,
        &["delta".to_owned()],
        false,
        &admin::KnownProjects::new(["delta"]),
        now,
    )
    .await
    .unwrap();
    admin::create_token(db, email, "laptop", TimeDelta::days(30), now)
        .await
        .unwrap()
        .token
        .reveal()
}

/// The public router over the example configuration, as `mercury-core` serves it, with the web
/// client's origin configured so that `/v1/session` is there.
async fn core() -> Core {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("mercury.db")).await.unwrap();
    let anna = person(&db, "anna@example.com").await;
    let ben = person(&db, "ben@example.com").await;
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config-examples");
    let (config, _) = mercury_core::config::load(&examples).unwrap();
    let burst = config.policies.limits.burst;
    let sessions = SessionConfig::parse(Some(PAGE), None, None).unwrap();
    let questions = Questions::new(db.clone(), Arc::new(config), Arc::new(Idle)).with_sessions(sessions);
    Core {
        _dir: dir,
        db,
        app: app(questions),
        anna,
        ben,
        burst,
    }
}

async fn send(core: &Core, request: Request<Body>) -> (StatusCode, axum::http::HeaderMap, Value) {
    let response = core.app.clone().oneshot(request).await.unwrap();
    let (parts, body) = response.into_parts();
    let bytes = body.collect().await.unwrap().to_bytes();
    (
        parts.status,
        parts.headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// A `POST` to `path` with `body`, the way the web client's page or an agent would send it, with a
/// token or without.
fn post(path: &str, token: Option<&str>, body: Vec<u8>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("host", "mercury.example.com")
        .header(CONTENT_TYPE, "application/json")
        .header("accept", "application/json, text/event-stream")
        .header(ORIGIN, PAGE)
        .header("x-mercury-client", "web")
        .header("idempotency-key", "limits-1");
    if let Some(token) = token {
        builder = builder.header(AUTHORIZATION, format!("Bearer {token}"));
    }
    builder.body(Body::from(body)).unwrap()
}

/// Every route that reads a body, as the contract writes its path (`/mcp` is MCP's, not the contract's).
const ROUTES_WITH_A_BODY: [&str; 5] = [
    "/v1/questions",
    "/v1/questions/{id}/feedback",
    "/v1/approvals/{id}",
    "/v1/session",
    "/mcp",
];

/// A path a caller sends to: the contract's, with an id filled in.
fn concrete(path: &str) -> String {
    path.replace("{id}", "q_01JB2M3N4P5Q6R7S8T9V0W1X2Y")
}

#[tokio::test]
async fn every_route_that_reads_a_body_refuses_one_byte_over_the_cap_before_anything_reads_it() {
    let core = core().await;
    assert_eq!(MAX_BODY, 64 * 1024, "the cap the contract names");
    for template in ROUTES_WITH_A_BODY {
        let path = concrete(template);
        let path = path.as_str();
        // with a token and without one: the cap comes before anything is read, so before the caller
        // is known, and it is the same answer
        for token in [Some(core.anna.as_str()), None] {
            let (status, _, body) = send(&core, post(path, token, vec![b'a'; MAX_BODY + 1])).await;
            assert_eq!(
                status,
                StatusCode::PAYLOAD_TOO_LARGE,
                "{path}, authenticated {}: {body}",
                token.is_some()
            );
            if path != "/mcp" {
                assert_eq!(body["error"]["code"], "invalid_request", "{path}: {body}");
            }
        }
        // the cap itself is a body that is allowed: it gets whatever answer its content earns (a
        // bad request, a refusal of the caller, a missing question), never a 413
        let (status, _, body) = send(&core, post(path, Some(&core.anna), vec![b'a'; MAX_BODY])).await;
        assert_ne!(status, StatusCode::PAYLOAD_TOO_LARGE, "{path} at the cap: {body}");
    }
}

#[tokio::test]
async fn a_refused_body_starts_nothing_and_spends_nothing() {
    let core = core().await;
    let (status, _, _) = send(
        &core,
        post("/v1/questions", Some(&core.anna), vec![b'a'; MAX_BODY + 1]),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    let questions: i64 = sqlx::query_scalar("SELECT count(*) FROM questions")
        .fetch_one(core.db.pool())
        .await
        .unwrap();
    assert_eq!(questions, 0, "no question was started");
    // the whole burst is still there for a real question
    for i in 0..core.burst {
        let body = json!({ "text": "What is done in export?", "project": "delta" });
        let request = Request::builder()
            .method("POST")
            .uri("/v1/questions")
            .header(AUTHORIZATION, format!("Bearer {}", core.anna))
            .header(CONTENT_TYPE, "application/json")
            .header("idempotency-key", format!("k-{i}"))
            .body(Body::from(body.to_string()))
            .unwrap();
        let (status, _, answer) = send(&core, request).await;
        assert_eq!(status, StatusCode::ACCEPTED, "question {i}: {answer}");
    }
}

#[tokio::test]
async fn one_token_sending_a_hundred_questions_in_a_minute_is_stopped_after_its_burst() {
    let core = core().await;
    let ask = |token: &str, i: usize| {
        let body = json!({ "text": "What is done in export?", "project": "delta" });
        Request::builder()
            .method("POST")
            .uri("/v1/questions")
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .header(CONTENT_TYPE, "application/json")
            .header("idempotency-key", format!("flood-{i}"))
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let (mut accepted, mut refused) = (0, 0);
    for i in 0..100 {
        let (status, headers, body) = send(&core, ask(&core.anna, i)).await;
        match status {
            StatusCode::ACCEPTED => {
                assert_eq!(
                    refused, 0,
                    "an accepted question after the first refusal (request {i})"
                );
                accepted += 1;
            }
            StatusCode::TOO_MANY_REQUESTS => {
                refused += 1;
                assert_eq!(body["error"]["code"], "rate_limited", "{body}");
                let wait: u64 = headers[RETRY_AFTER].to_str().unwrap().parse().unwrap();
                assert!(wait >= 1, "Retry-After {wait} says when to try again");
            }
            other => panic!("request {i}: {other} {body}"),
        }
    }
    assert_eq!(accepted, core.burst, "exactly the burst got through");
    assert_eq!(
        refused,
        100 - core.burst,
        "every other request of the minute was refused"
    );
    // a refused request starts no question
    let started: i64 = sqlx::query_scalar("SELECT count(*) FROM questions")
        .fetch_one(core.db.pool())
        .await
        .unwrap();
    assert_eq!(started, i64::from(core.burst));
    // the limit is the token's: another person's token has its own burst
    let (status, _, body) = send(&core, ask(&core.ben, 1000)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    // and the first token is still refused after it
    let (status, _, _) = send(&core, ask(&core.anna, 1001)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

/// A route added to the contract that takes a body must be added to the list above, which is the
/// cap test's: the served contract names every route with a request body, and this fails on one the
/// list doesn't hold.
#[tokio::test]
async fn the_cap_test_holds_every_route_with_a_body_the_contract_names() {
    let core = core().await;
    let request = Request::builder()
        .method("GET")
        .uri("/v1/openapi.json")
        .header("host", "mercury.example.com")
        .body(Body::empty())
        .unwrap();
    let (status, _, document) = send(&core, request).await;
    assert_eq!(status, StatusCode::OK);
    let mut with_a_body = Vec::new();
    for (path, operations) in document["paths"].as_object().unwrap() {
        for (method, operation) in operations.as_object().unwrap() {
            if operation.get("requestBody").is_some() {
                with_a_body.push(format!("{} {path}", method.to_uppercase()));
            }
        }
    }
    assert!(
        with_a_body.len() >= 4,
        "the contract names the routes with a body: {with_a_body:?}"
    );
    for route in &with_a_body {
        let path = route.split_once(' ').unwrap().1;
        assert!(
            ROUTES_WITH_A_BODY.contains(&path),
            "{route} takes a body and is not in the cap test's list"
        );
    }
}
