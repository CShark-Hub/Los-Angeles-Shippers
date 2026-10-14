//! No token, secret, e-mail address, session cookie or question text reaches a log line: every
//! request is logged by its listener, method, route pattern, status and duration only, and the
//! admin commands don't log. One test in a binary of its own, because it installs the process's
//! subscriber: a thread-local one would miss what other threads log, the database's among them.

#![allow(
    clippy::unwrap_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

use std::io::Write;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};

use http_body_util::BodyExt;

use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request};
use axum::http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, COOKIE, HOST, ORIGIN, SET_COOKIE};
use axum::middleware;
use axum::routing::post;
use chrono::TimeDelta;
use clap::Parser;
use mercury_core::cli::{Cli, Command};
use mercury_core::db::{self, Db};
use mercury_core::identity::Caller;
use mercury_core::internal::{self, AllowList};
use mercury_core::limits::{self, PerToken, Rate, RateLimiter};
use mercury_core::logging::{log_request, subscriber};
use mercury_core::probes::{self, McpClient, ProbeConfig, ProbeSecret, Probes};
use mercury_core::questions::{NotBuilt, Questions};
use mercury_core::session::SessionConfig;
use mercury_core::{admin, router};
use mercury_probe::probe::Probe;
use mercury_probe::registration;
use mercury_schema::{AccessMode, Role};
use tower::ServiceExt;
use tracing_subscriber::fmt::MakeWriter;

/// Collects everything the subscriber writes.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

const TOKEN: &str = "mcy_7fQ2kLp9_s3cr3tS3cr3tS3cr3tS3cr3tS3cr3t";
const SESSION: &str = "mercury_session=Zm9vYmFyc2Vzc2lvbmlk";
const QUESTION: &str = "Is the invoice export for the big customer finished?";
const EMAIL: &str = "dana.reyes@example.com";
const NAME: &str = "Dana Reyes";
const PROBE_TOKEN: &str = "probe-token-for-the-log-test";

/// A request that carries `token`, a session cookie and a question in its body.
fn with_secrets(method: &str, uri: &str, token: &str) -> Request {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .header(COOKIE, SESSION)
        .body(Body::from(format!(
            r#"{{"project":"delta","question":"{QUESTION}"}}"#
        )))
        .unwrap()
}

async fn ask(caller: Caller) -> &'static str {
    match caller.role {
        Role::Pm => "pm",
        _ => "other",
    }
}

/// A question route the way the API mounts one: the body cap, the token's limit, the request log.
fn question_route(db: &Db) -> Router {
    let limit = PerToken {
        db: db.clone(),
        limiter: RateLimiter::new(Rate::QUESTIONS),
    };
    Router::new()
        .route("/v1/ask", post(ask))
        .route_layer(middleware::from_fn_with_state(limit, limits::per_token))
        .layer(middleware::from_fn(limits::body_cap))
        .layer(middleware::from_fn_with_state("public", log_request))
        .with_state(db.clone())
}

/// Runs an admin command line and returns what it printed.
async fn admin_cli(db: &Db, args: &[&str]) -> String {
    let cli = Cli::try_parse_from(["mercury-core", "admin"].iter().chain(args)).unwrap();
    let Some(Command::Admin(command)) = cli.command else {
        panic!("not an admin command");
    };
    let (mut out, mut note) = (Vec::new(), Vec::new());
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config-examples");
    admin::run(command, db, &examples, &mut out, &mut note)
        .await
        .unwrap();
    String::from_utf8(out).unwrap() + &String::from_utf8(note).unwrap()
}

#[tokio::test]
async fn no_token_e_mail_or_question_text_reaches_a_log_line() {
    let capture = Capture::default();
    tracing::subscriber::set_global_default(subscriber(capture.clone())).unwrap();

    // the listeners, with a token in the header, the query and the path
    let query = format!("/v1/questions?q={}", QUESTION.replace(' ', "+"));
    let token_path = format!("/v1/{TOKEN}");
    let requests = [
        ("POST", "/v1/questions"),
        ("GET", query.as_str()),
        ("GET", token_path.as_str()),
        ("POST", "/internal/probes/register"),
        ("GET", "/v1/health"),
    ];
    for (method, uri) in requests {
        router().oneshot(with_secrets(method, uri, TOKEN)).await.unwrap();
        internal::router(AllowList::default())
            .oneshot(with_secrets(method, uri, TOKEN))
            .await
            .unwrap();
    }
    let logged = capture.text();
    let lines: Vec<&str> = logged.lines().collect();
    assert_eq!(
        lines.len(),
        10,
        "one line per request on each listener:\n{logged}"
    );
    assert!(lines.iter().all(|line| line.contains(r#""level":"INFO""#)));
    assert!(
        logged.contains(r#""route":"/v1/health""#) && logged.contains(r#""status":200"#),
        "{logged}"
    );
    assert!(logged.contains(r#""route":"unmatched""#), "{logged}");

    // the admin commands, on a fresh database
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("mercury.db")).await.unwrap();
    let email = EMAIL.to_uppercase();
    admin_cli(
        &db,
        &["user", "add", "--email", &email, "--name", NAME, "--role", "pm"],
    )
    .await;
    admin_cli(
        &db,
        &[
            "user",
            "grant",
            "--email",
            EMAIL,
            "--projects",
            "delta",
            "--approver",
        ],
    )
    .await;
    let created = admin_cli(&db, &["token", "create", "--email", EMAIL, "--label", "laptop"]).await;
    let token = created.lines().next().unwrap().to_owned();
    let secret = token[17..].to_owned();
    let public_id = token[4..16].to_owned();
    for args in [
        &["token", "list"][..],
        &["token", "list", "--email", EMAIL],
        &["access", "list"],
        &["access", "list", "--project", "delta"],
        &["access", "list", "--email", EMAIL],
    ] {
        admin_cli(&db, args).await;
    }
    let old = admin::create_token(
        &db,
        EMAIL,
        "old",
        TimeDelta::hours(1),
        db::now() - TimeDelta::days(1),
    )
    .await
    .unwrap()
    .token
    .reveal();

    // the token's route: allowed, over the limit, an expired token, an oversized body, a revoked
    // token, and the database gone
    let route = question_route(&db);
    for _ in 0..6 {
        route
            .clone()
            .oneshot(with_secrets("POST", "/v1/ask", &token))
            .await
            .unwrap();
    }
    route
        .clone()
        .oneshot(with_secrets("POST", "/v1/ask", &old))
        .await
        .unwrap();
    let oversized = Request::builder()
        .method("POST")
        .uri("/v1/ask")
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::from(format!("{QUESTION} {}", "x".repeat(limits::MAX_BODY))))
        .unwrap();
    route.clone().oneshot(oversized).await.unwrap();

    // the questions API itself: a question asked, followed and read back, and who-am-I
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config-examples");
    let (config, _) = mercury_core::config::load(&examples).unwrap();
    let sessions = SessionConfig::parse(Some("https://mercury.example.com"), None, None).unwrap();
    let api = mercury_core::app(
        Questions::new(db.clone(), Arc::new(config), Arc::new(NotBuilt)).with_sessions(sessions),
    );
    let asked = api
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/questions")
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .header(CONTENT_TYPE, "application/json")
                .header("idempotency-key", "log-test-1")
                .body(Body::from(format!(
                    r#"{{"text":"{QUESTION} Mail {EMAIL}","project":"delta"}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(asked.status(), 202);
    let id: serde_json::Value =
        serde_json::from_slice(&asked.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let id = id["id"].as_str().unwrap().to_owned();
    for uri in [
        format!("/v1/questions/{id}/events"),
        format!("/v1/questions/{id}"),
        "/v1/me".to_owned(),
        "/v1/projects".to_owned(),
    ] {
        let response = api
            .clone()
            .oneshot(with_secrets("GET", &uri, &token))
            .await
            .unwrap();
        // read the stream to its end, so every log line of the question is in
        response.into_body().collect().await.unwrap();
    }
    // the web client's session: the token goes in once, then the cookie is read, refused from
    // another page and cleared; the session id is in no log line either
    let page = |method: &str, uri: &str, origin: &str| {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(ORIGIN, origin)
            .header("x-mercury-client", "web")
    };
    let mut sign_in = page("POST", "/v1/session", "https://mercury.example.com")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(format!(r#"{{"token":"{token}"}}"#)))
        .unwrap();
    sign_in
        .extensions_mut()
        .insert(ConnectInfo("203.0.113.9:40000".parse::<SocketAddr>().unwrap()));
    let signed_in = api.clone().oneshot(sign_in).await.unwrap();
    assert_eq!(signed_in.status(), 204);
    let cookie = signed_in.headers()[SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let session_id = cookie.split_once('=').unwrap().1.to_owned();
    let read = Request::builder()
        .uri("/v1/me")
        .header(COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    assert_eq!(api.clone().oneshot(read).await.unwrap().status(), 200);
    let forged = page("POST", "/v1/questions", "https://evil.example.org")
        .header(COOKIE, &cookie)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(format!(r#"{{"text":"{QUESTION}"}}"#)))
        .unwrap();
    assert_eq!(api.clone().oneshot(forged).await.unwrap().status(), 403);
    let sign_out = page("DELETE", "/v1/session", "https://mercury.example.com")
        .header(COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    assert_eq!(api.clone().oneshot(sign_out).await.unwrap().status(), 204);

    // an agent over MCP: the handshake, the projects, a question followed to its end with progress,
    // the cookie alone, and a page from elsewhere
    let mcp = |token: &str, body: String| {
        Request::builder()
            .method("POST")
            .uri("/mcp")
            .header(HOST, "mercury.example.com")
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream")
            .body(Body::from(body))
            .unwrap()
    };
    let calls = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"agent","version":"1"}}}"#.to_owned(),
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_projects","arguments":{}}}"#.to_owned(),
        format!(
            r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"ask","arguments":{{"question":"{QUESTION} Mail {EMAIL}","project":"delta"}},"_meta":{{"progressToken":"p"}}}}}}"#
        ),
        format!(
            r#"{{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{{"name":"ask","arguments":{{"question":"{QUESTION}","trusted":true}}}}}}"#
        ),
    ];
    for body in calls {
        let response = api.clone().oneshot(mcp(&token, body)).await.unwrap();
        assert_eq!(response.status(), 200);
        response.into_body().collect().await.unwrap();
    }
    let mut cookie_only = mcp("", String::new());
    cookie_only.headers_mut().remove(AUTHORIZATION);
    cookie_only.headers_mut().insert(COOKIE, cookie.parse().unwrap());
    assert_eq!(api.clone().oneshot(cookie_only).await.unwrap().status(), 401);
    let mut elsewhere = mcp(&token, format!(r#"{{"question":"{QUESTION}"}}"#));
    elsewhere
        .headers_mut()
        .insert(ORIGIN, "https://evil.example.org".parse().unwrap());
    assert_eq!(api.clone().oneshot(elsewhere).await.unwrap().status(), 403);

    admin_cli(&db, &["token", "revoke", &public_id]).await;
    route
        .clone()
        .oneshot(with_secrets("POST", "/v1/ask", &token))
        .await
        .unwrap();
    admin_cli(&db, &["user", "disable", "--email", EMAIL]).await;
    db.pool().close().await;
    route
        .clone()
        .oneshot(with_secrets("POST", "/v1/ask", &token))
        .await
        .unwrap();

    // a probe registering on the internal listener: a body that isn't a registration, another
    // token, then its registration and a check that can't reach it
    let probe_dir = tempfile::tempdir().unwrap();
    let probe_db = Db::open(&probe_dir.path().join("mercury.db")).await.unwrap();
    let probes = Probes::new(
        probe_db,
        McpClient::http().unwrap(),
        vec![ProbeConfig {
            project: "delta".into(),
            url: "http://127.0.0.1:9/mcp".into(),
            token: ProbeSecret::new(PROBE_TOKEN).unwrap(),
            access_max: AccessMode::Code,
        }],
    );
    let internal = internal::router_with(
        AllowList::parse(Some("127.0.0.0/8")).unwrap(),
        probes::routes(probes.clone()),
    );
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/repo-delta");
    let probe = Probe::dev(&repo, &repo.join("meta"), None).unwrap();
    let body = serde_json::to_vec(&registration::body(&probe)).unwrap();
    for (token, body) in [
        (PROBE_TOKEN, b"{\"project\": \"delta\"}".to_vec()),
        ("another-probe-token", body.clone()),
        (PROBE_TOKEN, body),
    ] {
        let mut request = Request::builder()
            .method("POST")
            .uri("/internal/probes/register")
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo("127.0.0.1:40000".parse::<SocketAddr>().unwrap()));
        internal.clone().oneshot(request).await.unwrap();
    }
    probes.check("delta").await.unwrap();

    let logged = capture.text();
    let new = &logged[logged.lines().take(10).map(|line| line.len() + 1).sum::<usize>()..];
    for (route, status) in [
        ("/v1/questions", 202),
        ("/v1/questions/{id}/events", 200),
        ("/v1/me", 200),
        ("/mcp", 200),
        ("/mcp", 401),
        ("/mcp", 403),
    ] {
        let line = format!(r#""route":"{route}","status":{status}"#);
        assert!(
            new.contains(&line),
            "no {status} for {route}:
{new}"
        );
    }
    for status in [200, 429, 401, 413, 500] {
        let line = format!(r#""route":"/v1/ask","status":{status}"#);
        assert!(new.contains(&line), "no {status} for the question route:\n{new}");
    }
    assert!(
        new.contains("cannot check a token"),
        "the database failure is logged:\n{new}"
    );
    // the MCP library's own lines about each request stay out: one line per request
    assert!(!new.contains(r#""target":"rmcp"#), "{new}");
    for secret in [
        TOKEN,
        PROBE_TOKEN,
        "s3cr3t",
        SESSION,
        "Zm9vYmFy",
        QUESTION,
        "invoice",
        "customer",
        &token,
        &secret,
        &cookie,
        &session_id,
        "evil.example.org",
        &old,
        &old[17..],
        EMAIL,
        &email,
        "dana.reyes",
        NAME,
        "Reyes",
    ] {
        assert!(!logged.contains(secret), "{secret:?} reached the log:\n{logged}");
    }
}
