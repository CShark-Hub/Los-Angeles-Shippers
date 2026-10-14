//! The web client's sessions: signing in with a token, the cookie that stands for it, the check on
//! where a cookie request comes from, signing out, what ends a session, and the sign-in limit by
//! source address. Everything goes through the public router, as a browser's requests would.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::header::{
    AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, COOKIE, ORIGIN, RETRY_AFTER, SET_COOKIE,
};
use axum::http::{Method, Request, Response, StatusCode};
use axum::routing::get;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::TimeDelta;
use http_body_util::BodyExt;
use mercury_core::db::{self, Db};
use mercury_core::identity::{Caller, NewToken};
use mercury_core::internal::AllowList;
use mercury_core::questions::{NotBuilt, Questions};
use mercury_core::session::{self, SessionConfig};
use mercury_core::{admin, app};
use mercury_schema::Role;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

/// The web client's own origin, as the server is set up with it.
const ORIGIN_OF_THE_PAGE: &str = "https://mercury.example.com";
/// Where requests come from unless a test says otherwise.
const SOURCE: &str = "203.0.113.9:40000";

struct Web {
    _dir: tempfile::TempDir,
    db: Db,
    app: Router,
    anna: String,
    ben: String,
}

fn settings() -> SessionConfig {
    SessionConfig::parse(Some(ORIGIN_OF_THE_PAGE), None, None).unwrap()
}

async fn web_with(config: SessionConfig) -> Web {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("mercury.db")).await.unwrap();
    let now = db::now();
    let known = admin::KnownProjects::new(["delta"]);
    let mut tokens = Vec::new();
    for email in ["anna@example.com", "ben@example.com"] {
        admin::add_user(&db, email, "Pat Example", Role::Pm, now)
            .await
            .unwrap();
        admin::grant(&db, email, &["delta".to_owned()], false, &known, now)
            .await
            .unwrap();
        let created = admin::create_token(&db, email, "laptop", TimeDelta::days(30), now)
            .await
            .unwrap();
        tokens.push(created.token.reveal());
    }
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config-examples");
    let (mut examples, _) = mercury_core::config::load(&examples).unwrap();
    // the token limit these tests count on, as the private and strict presets ship it; the examples,
    // the demo's, allow a person trying them far more
    examples.policies.limits.questions_per_hour = 30;
    examples.policies.limits.burst = 5;
    let questions = Questions::new(db.clone(), Arc::new(examples), Arc::new(NotBuilt)).with_sessions(config);
    let ben = tokens.pop().unwrap();
    let anna = tokens.pop().unwrap();
    Web {
        _dir: dir,
        db,
        app: app(questions),
        anna,
        ben,
    }
}

async fn web() -> Web {
    web_with(settings()).await
}

/// A request as the web client's page sends it: its own origin and its header.
fn from_the_page(method: Method, uri: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(ORIGIN, ORIGIN_OF_THE_PAGE)
        .header("x-mercury-client", "web")
}

fn from(builder: axum::http::request::Builder, source: &str, body: Body) -> Request<Body> {
    let mut request = builder.body(body).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(source.parse::<SocketAddr>().unwrap()));
    request
}

fn sign_in_request(token: &str, source: &str) -> Request<Body> {
    from(
        from_the_page(Method::POST, "/v1/session").header(CONTENT_TYPE, "application/json"),
        source,
        Body::from(json!({ "token": token }).to_string()),
    )
}

async fn send(web: &Web, request: Request<Body>) -> Response<Body> {
    web.app.clone().oneshot(request).await.unwrap()
}

async fn json_of(response: Response<Body>) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

/// Signs `token` in and gives the cookie as a request sends it back: `name=value`.
async fn sign_in(web: &Web, token: &str) -> String {
    let response = send(web, sign_in_request(token, SOURCE)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    set_cookie(&response).split(';').next().unwrap().to_owned()
}

fn set_cookie(response: &Response<Body>) -> String {
    let values: Vec<_> = response.headers().get_all(SET_COOKIE).iter().collect();
    assert_eq!(values.len(), 1, "exactly one cookie is set");
    values[0].to_str().unwrap().to_owned()
}

fn error_code(body: &Value) -> &str {
    body["error"]["code"].as_str().unwrap_or_default()
}

/// The audit rows of one type, as JSON, oldest first.
async fn audited(db: &Db, event_type: &str) -> Vec<Value> {
    let rows: Vec<String> =
        sqlx::query_scalar("SELECT payload_json FROM audit_events WHERE type = ? ORDER BY seq")
            .bind(event_type)
            .fetch_all(db.pool())
            .await
            .unwrap();
    rows.iter()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect()
}

async fn count(db: &Db, sql: &'static str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(db.pool()).await.unwrap()
}

/// A request header as a name and a value.
type Header = (&'static str, &'static str);

fn public_id(token: &str) -> &str {
    &token[4..16]
}

// ------------------------------------------------------------------------------------ signing in

#[tokio::test]
async fn signing_in_sets_a_cookie_the_page_cant_read_and_keeps_only_a_digest() {
    let web = web().await;
    let response = send(&web, sign_in_request(&web.anna, SOURCE)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
    let cookie = set_cookie(&response);
    let (pair, attributes) = cookie.split_once("; ").unwrap();
    assert_eq!(
        attributes, "Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=43200",
        "twelve hours, and nothing that names a domain"
    );
    let value = pair.strip_prefix("__Host-mercury_session=").unwrap();
    let id = URL_SAFE_NO_PAD.decode(value).unwrap();
    assert_eq!(id.len(), 32, "256 random bits");
    assert!(!String::from_utf8_lossy(&json_bytes(response).await).contains(value));

    // the row holds the SHA-256 of the id, never the id
    let stored: Vec<u8> = sqlx::query_scalar("SELECT id_sha256 FROM sessions")
        .fetch_one(web.db.pool())
        .await
        .unwrap();
    assert_eq!(stored, Sha256::digest(&id).to_vec());
    assert_ne!(stored, id);
    let owner: String = sqlx::query_scalar("SELECT token_public_id FROM sessions")
        .fetch_one(web.db.pool())
        .await
        .unwrap();
    assert_eq!(owner, public_id(&web.anna));

    // the audit says who signed in from where, and holds neither the id nor the token
    let rows = audited(&web.db, "session_signin").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["outcome"], "created");
    assert_eq!(rows[0]["token"], public_id(&web.anna));
    assert_eq!(rows[0]["source"], "203.0.113.9");
    let everything = rows[0].to_string();
    assert!(!everything.contains(value) && !everything.contains(&web.anna[17..]));
}

async fn json_bytes(response: Response<Body>) -> Vec<u8> {
    response.into_body().collect().await.unwrap().to_bytes().to_vec()
}

#[tokio::test]
async fn a_session_ends_with_the_session_time_and_never_after_its_token() {
    // a session time shorter than the token's
    let short = SessionConfig::parse(Some(ORIGIN_OF_THE_PAGE), Some("1h"), None).unwrap();
    let web = web_with(short).await;
    let cookie = set_cookie(&send(&web, sign_in_request(&web.anna, SOURCE)).await);
    assert!(cookie.ends_with("Max-Age=3600"), "{cookie}");

    // a token that expires within the hour: its session ends with it
    let soon = admin::create_token(
        &web.db,
        "ben@example.com",
        "short",
        TimeDelta::minutes(20),
        db::now(),
    )
    .await
    .unwrap();
    let response = send(&web, sign_in_request(&soon.token.reveal(), SOURCE)).await;
    let cookie = set_cookie(&response);
    let max_age: i64 = cookie.rsplit("Max-Age=").next().unwrap().parse().unwrap();
    assert!((1190..=1200).contains(&max_age), "{cookie}");
    // the session's last moment is the token's
    let (session_ends, token_ends): (String, String) = sqlx::query_as(
        "SELECT s.expires_at, t.expires_at FROM sessions s JOIN tokens t ON t.public_id = s.token_public_id
         WHERE s.token_public_id = ?",
    )
    .bind(soon.token.public_id())
    .fetch_one(web.db.pool())
    .await
    .unwrap();
    assert_eq!(session_ends, token_ends);
}

#[tokio::test]
async fn a_token_that_isnt_good_gets_the_same_answers_as_anywhere_and_no_cookie() {
    let web = web().await;
    let (secret_less, wrong_secret) = (
        "mcy_nothing".to_owned(),
        format!("{}{}", &web.anna[..17], "A".repeat(43)),
    );
    for token in [secret_less, wrong_secret] {
        let response = send(&web, sign_in_request(&token, "203.0.113.21:40000")).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(response.headers().get(SET_COOKIE).is_none());
        assert_eq!(error_code(&json_of(response).await), "unauthenticated");
    }

    // an expired token says so, to the right secret only
    let expired = NewToken::generate().unwrap();
    let user: String = sqlx::query_scalar("SELECT id FROM users WHERE email = 'anna@example.com'")
        .fetch_one(web.db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tokens (public_id, user_id, secret_sha256, label, created_at, expires_at)
         VALUES (?, ?, ?, 'old', '2026-01-01T00:00:00.000Z', '2026-02-01T00:00:00.000Z')",
    )
    .bind(expired.public_id())
    .bind(&user)
    .bind(expired.secret_sha256().as_slice())
    .execute(web.db.pool())
    .await
    .unwrap();
    let response = send(&web, sign_in_request(&expired.reveal(), "203.0.113.22:40000")).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(&json_of(response).await), "token_expired");

    // a revoked token and a disabled person are refused like a wrong one
    let public = public_id(&web.ben).to_owned();
    admin::revoke_token(&web.db, &public, db::now()).await.unwrap();
    let response = send(&web, sign_in_request(&web.ben, "203.0.113.23:40000")).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    admin::disable_user(&web.db, "anna@example.com", db::now())
        .await
        .unwrap();
    let response = send(&web, sign_in_request(&web.anna, "203.0.113.24:40000")).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    assert_eq!(count(&web.db, "SELECT count(*) FROM sessions").await, 0);
    // every attempt is in the audit, with the outcome and the token that was tried when it had its shape
    let rows = audited(&web.db, "session_signin").await;
    let outcomes: Vec<&str> = rows.iter().map(|row| row["outcome"].as_str().unwrap()).collect();
    assert_eq!(
        outcomes,
        [
            "unauthenticated",
            "unauthenticated",
            "token_expired",
            "unauthenticated",
            "unauthenticated"
        ]
    );
    assert_eq!(rows[0]["token"], Value::Null, "no shape, no token named");
    assert_eq!(rows[1]["token"], public_id(&web.anna));
    assert!(!rows.iter().any(|row| row.to_string().contains(&web.anna[17..])));
}

#[tokio::test]
async fn a_body_that_isnt_a_sign_in_is_a_bad_request() {
    let web = web().await;
    for body in [
        json!({ "token": web.anna, "remember": true }).to_string(),
        json!({}).to_string(),
        "mcy_not_json".to_owned(),
    ] {
        let request = from(
            from_the_page(Method::POST, "/v1/session").header(CONTENT_TYPE, "application/json"),
            SOURCE,
            Body::from(body),
        );
        let response = send(&web, request).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(error_code(&json_of(response).await), "invalid_request");
    }
    assert_eq!(count(&web.db, "SELECT count(*) FROM sessions").await, 0);
    assert_eq!(audited(&web.db, "session_signin").await.len(), 3);
}

// ------------------------------------------------------------------------------------ the cookie

#[tokio::test]
async fn the_cookie_stands_for_the_token_and_questions_it_asks_are_on_the_web_channel() {
    let web = web().await;
    let cookie = sign_in(&web, &web.anna).await;

    let me = send(
        &web,
        from(
            Request::builder().uri("/v1/me").header(COOKIE, &cookie),
            SOURCE,
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(me.status(), StatusCode::OK);
    let me = json_of(me).await;
    assert_eq!(me["user"]["email"], "anna@example.com");
    assert_eq!(me["token"]["public_id"], public_id(&web.anna));

    let asked = |token_header: Option<&str>, cookie: Option<&str>, key: &str| {
        let mut builder = from_the_page(Method::POST, "/v1/questions")
            .header(CONTENT_TYPE, "application/json")
            .header("idempotency-key", key);
        if let Some(token) = token_header {
            builder = builder.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        if let Some(cookie) = cookie {
            builder = builder.header(COOKIE, cookie);
        }
        from(
            builder,
            SOURCE,
            Body::from(json!({ "text": "What is done?", "project": "delta" }).to_string()),
        )
    };
    let by_cookie = send(&web, asked(None, Some(&cookie), "k-web")).await;
    assert_eq!(by_cookie.status(), StatusCode::ACCEPTED);
    let by_token = send(&web, asked(Some(&web.anna), None, "k-cli")).await;
    assert_eq!(by_token.status(), StatusCode::ACCEPTED);

    let channels: Vec<(String, String)> =
        sqlx::query_as("SELECT channel, token_public_id FROM questions ORDER BY rowid")
            .fetch_all(web.db.pool())
            .await
            .unwrap();
    assert_eq!(
        channels,
        [
            ("web".to_owned(), public_id(&web.anna).to_owned()),
            ("cli".to_owned(), public_id(&web.anna).to_owned())
        ]
    );
    // the web client's use counts as the token's: `token list` shows when it was last used
    let used: Option<String> = sqlx::query_scalar("SELECT last_used_at FROM tokens WHERE public_id = ?")
        .bind(public_id(&web.anna))
        .fetch_one(web.db.pool())
        .await
        .unwrap();
    assert!(used.is_some());
    let seen: Option<String> = sqlx::query_scalar("SELECT last_seen_at FROM sessions")
        .fetch_one(web.db.pool())
        .await
        .unwrap();
    assert!(seen.is_some());
}

#[tokio::test]
async fn the_token_rate_limit_counts_a_session_and_a_bearer_call_together() {
    let web = web().await;
    let cookie = sign_in(&web, &web.anna).await;
    let ask = |n: usize, with_cookie: bool| {
        let mut builder = from_the_page(Method::POST, "/v1/questions")
            .header(CONTENT_TYPE, "application/json")
            .header("idempotency-key", format!("k-{n}"));
        builder = if with_cookie {
            builder.header(COOKIE, &cookie)
        } else {
            builder.header(AUTHORIZATION, format!("Bearer {}", web.anna))
        };
        from(
            builder,
            SOURCE,
            Body::from(json!({ "text": "What is done?" }).to_string()),
        )
    };
    for n in 0..5 {
        let response = send(&web, ask(n, n % 2 == 0)).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED, "request {n}");
    }
    assert_eq!(
        send(&web, ask(5, true)).await.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "the sixth, by cookie, finds the token's burst spent"
    );
    assert_eq!(
        send(&web, ask(6, false)).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn a_cookie_that_isnt_a_session_is_unauthenticated_not_an_error() {
    let web = web().await;
    let real = sign_in(&web, &web.anna).await;
    let id = real.strip_prefix("__Host-mercury_session=").unwrap();
    let unknown = URL_SAFE_NO_PAD.encode([7_u8; 32]);
    for cookie in [
        format!("__Host-mercury_session={unknown}"),
        "__Host-mercury_session=".to_owned(),
        "__Host-mercury_session=not base64!".to_owned(),
        format!("__Host-mercury_session={}", &id[..id.len() - 4]),
        format!("mercury_session={id}"),
        format!("__Secure-mercury_session={id}"),
    ] {
        let response = send(
            &web,
            from(
                Request::builder().uri("/v1/me").header(COOKIE, &cookie),
                SOURCE,
                Body::empty(),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{cookie}");
        assert_eq!(error_code(&json_of(response).await), "unauthenticated");
    }
    // a stale cookie doesn't spoil what is public: the contract is for anyone
    let response = send(
        &web,
        from(
            Request::builder()
                .uri("/v1/openapi.json")
                .header(COOKIE, format!("__Host-mercury_session={unknown}")),
            SOURCE,
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    // the right one still works among other cookies
    let among = format!("theme=dark; {real}; other=1");
    let response = send(
        &web,
        from(
            Request::builder().uri("/v1/me").header(COOKIE, &among),
            SOURCE,
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

/// What a route that only takes a Bearer token does with the cookie: nothing. `/mcp` will be one.
#[tokio::test]
async fn a_route_that_takes_only_bearer_never_reads_the_cookie() {
    let web = web().await;
    let cookie = sign_in(&web, &web.anna).await;
    async fn who(caller: Caller) -> String {
        caller.email
    }
    let bearer_only = Router::new()
        .route("/bearer-only", get(who))
        .with_state(web.db.clone());
    let with_cookie = Request::builder()
        .uri("/bearer-only")
        .header(COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    let response = bearer_only.clone().oneshot(with_cookie).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let with_token = Request::builder()
        .uri("/bearer-only")
        .header(AUTHORIZATION, format!("Bearer {}", web.anna))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        bearer_only.oneshot(with_token).await.unwrap().status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_request_with_a_bearer_header_is_a_bearer_request_whatever_cookie_it_carries() {
    let web = web().await;
    let cookie = sign_in(&web, &web.anna).await;
    // a bad token and a good cookie: refused, the cookie isn't tried
    let response = send(
        &web,
        from(
            Request::builder()
                .uri("/v1/me")
                .header(AUTHORIZATION, "Bearer mcy_nothing")
                .header(COOKIE, &cookie),
            SOURCE,
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    // a good token with someone else's cookie is the token's caller, and needs no Origin to write
    let other_cookie = sign_in(&web, &web.ben).await;
    let request = from(
        Request::builder()
            .method(Method::POST)
            .uri("/v1/questions")
            .header(CONTENT_TYPE, "application/json")
            .header("idempotency-key", "k-bearer")
            .header(AUTHORIZATION, format!("Bearer {}", web.anna))
            .header(COOKIE, &other_cookie),
        SOURCE,
        Body::from(json!({ "text": "What is done?" }).to_string()),
    );
    assert_eq!(send(&web, request).await.status(), StatusCode::ACCEPTED);
    let owner: String = sqlx::query_scalar("SELECT token_public_id FROM questions")
        .fetch_one(web.db.pool())
        .await
        .unwrap();
    assert_eq!(owner, public_id(&web.anna));
}

// ------------------------------------------------------------------------------------ where a request comes from

#[tokio::test]
async fn a_cookie_request_that_changes_state_must_come_from_the_web_clients_own_page() {
    let web = web().await;
    let cookie = sign_in(&web, &web.anna).await;
    let post = |headers: &[Header]| {
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri("/v1/questions")
            .header(CONTENT_TYPE, "application/json")
            .header("idempotency-key", "k-csrf")
            .header(COOKIE, &cookie);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        from(
            builder,
            SOURCE,
            Body::from(json!({ "text": "Transfer everything" }).to_string()),
        )
    };
    let cases: [(&str, &[Header], &str); 6] = [
        ("no Origin, no client header", &[], "origin"),
        (
            "another site's Origin",
            &[
                ("origin", "https://evil.example.org"),
                ("x-mercury-client", "web"),
            ],
            "origin",
        ),
        (
            "an Origin that only starts the same",
            &[
                ("origin", "https://mercury.example.com.evil.example.org"),
                ("x-mercury-client", "web"),
            ],
            "origin",
        ),
        (
            "the opaque Origin",
            &[("origin", "null"), ("x-mercury-client", "web")],
            "origin",
        ),
        (
            "the right Origin, no client header",
            &[("origin", ORIGIN_OF_THE_PAGE)],
            "client",
        ),
        (
            "the right Origin, another client",
            &[("origin", ORIGIN_OF_THE_PAGE), ("x-mercury-client", "curl")],
            "client",
        ),
    ];
    for (what, headers, missing) in cases {
        let response = send(&web, post(headers)).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{what}");
        assert_eq!(error_code(&json_of(response).await), "invalid_request", "{what}");
        let rows = audited(&web.db, "csrf_refused").await;
        let last = rows.last().unwrap();
        assert_eq!(last["missing"], missing, "{what}");
    }
    // nothing was asked, and the trail names the token, the method and the route, never the Origin
    assert_eq!(count(&web.db, "SELECT count(*) FROM questions").await, 0);
    let rows = audited(&web.db, "csrf_refused").await;
    assert_eq!(rows.len(), 6);
    assert!(rows.iter().all(|row| row["token"] == public_id(&web.anna)));
    assert!(
        rows.iter()
            .all(|row| row["method"] == "POST" && row["route"] == "/v1/questions")
    );
    assert!(
        !rows
            .iter()
            .any(|row| row.to_string().contains("evil.example.org"))
    );

    // from the page itself it goes through, and reading needs neither header
    let right = [("origin", ORIGIN_OF_THE_PAGE), ("x-mercury-client", "web")];
    assert_eq!(send(&web, post(&right)).await.status(), StatusCode::ACCEPTED);
    let read = send(
        &web,
        from(
            Request::builder().uri("/v1/me").header(COOKIE, &cookie),
            SOURCE,
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(read.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_other_methods_that_change_state_are_checked_too_and_the_core_sends_no_cors_headers() {
    let web = web().await;
    let cookie = sign_in(&web, &web.anna).await;
    for method in [Method::PUT, Method::PATCH, Method::DELETE, Method::POST] {
        let request = from(
            Request::builder()
                .method(method.clone())
                .uri("/v1/questions/q_01JB2M3N4P5Q6R7S8T9V0W1X2Y/feedback")
                .header(COOKIE, &cookie),
            SOURCE,
            Body::empty(),
        );
        let response = send(&web, request).await;
        // PUT, PATCH and DELETE have no route here, but the check comes first: a 403, not a 405
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{method}");
        assert!(
            response
                .headers()
                .keys()
                .all(|name| !name.as_str().starts_with("access-control-")),
            "{method}"
        );
    }
    // a preflight gets no permission either
    let preflight = from(
        Request::builder()
            .method(Method::OPTIONS)
            .uri("/v1/questions")
            .header(ORIGIN, "https://evil.example.org")
            .header("access-control-request-method", "POST"),
        SOURCE,
        Body::empty(),
    );
    let response = send(&web, preflight).await;
    assert!(response.headers().get("access-control-allow-origin").is_none());
}

#[tokio::test]
async fn without_a_configured_origin_nothing_can_sign_in_or_write_with_a_cookie() {
    let web = web_with(SessionConfig::default()).await;
    let response = send(&web, sign_in_request(&web.anna, SOURCE)).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(response.headers().get(SET_COOKIE).is_none());
    assert_eq!(count(&web.db, "SELECT count(*) FROM sessions").await, 0);
    let rows = audited(&web.db, "session_signin").await;
    assert_eq!(rows[0]["outcome"], "not_from_the_web_client");

    // Bearer needs no origin
    let request = from(
        Request::builder()
            .uri("/v1/me")
            .header(AUTHORIZATION, format!("Bearer {}", web.anna)),
        SOURCE,
        Body::empty(),
    );
    assert_eq!(send(&web, request).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn signing_in_from_another_page_is_refused_before_the_token_is_looked_at() {
    let web = web().await;
    for builder in [
        Request::builder().method(Method::POST).uri("/v1/session"),
        Request::builder()
            .method(Method::POST)
            .uri("/v1/session")
            .header(ORIGIN, "https://evil.example.org")
            .header("x-mercury-client", "web"),
        Request::builder()
            .method(Method::POST)
            .uri("/v1/session")
            .header(ORIGIN, ORIGIN_OF_THE_PAGE),
    ] {
        let request = from(
            builder.header(CONTENT_TYPE, "application/json"),
            SOURCE,
            Body::from(json!({ "token": web.anna }).to_string()),
        );
        let response = send(&web, request).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(response.headers().get(SET_COOKIE).is_none());
    }
    assert_eq!(count(&web.db, "SELECT count(*) FROM sessions").await, 0);
    let rows = audited(&web.db, "session_signin").await;
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|row| row["outcome"] == "not_from_the_web_client"));
    assert!(
        rows.iter().all(|row| row["token"] == Value::Null),
        "the token wasn't read"
    );
}

// ------------------------------------------------------------------------------------ ending a session

#[tokio::test]
async fn signing_out_ends_the_session_and_clears_the_cookie() {
    let web = web().await;
    let cookie = sign_in(&web, &web.anna).await;
    let other = sign_in(&web, &web.ben).await;

    // from another page, or with no header, it is refused and the session stands
    let unchecked = from(
        Request::builder()
            .method(Method::DELETE)
            .uri("/v1/session")
            .header(COOKIE, &cookie),
        SOURCE,
        Body::empty(),
    );
    assert_eq!(send(&web, unchecked).await.status(), StatusCode::FORBIDDEN);
    let me = || {
        from(
            Request::builder().uri("/v1/me").header(COOKIE, &cookie),
            SOURCE,
            Body::empty(),
        )
    };
    assert_eq!(send(&web, me()).await.status(), StatusCode::OK);
    assert_eq!(audited(&web.db, "csrf_refused").await.len(), 1);

    let out = send(
        &web,
        from(
            from_the_page(Method::DELETE, "/v1/session").header(COOKIE, &cookie),
            SOURCE,
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(out.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        set_cookie(&out),
        "__Host-mercury_session=; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=0"
    );
    // the old cookie is dead, whoever replays it; the other session is not
    let replay = send(&web, me()).await;
    assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
    let ended: Option<String> =
        sqlx::query_scalar("SELECT revoked_at FROM sessions WHERE token_public_id = ?")
            .bind(public_id(&web.anna))
            .fetch_one(web.db.pool())
            .await
            .unwrap();
    assert!(ended.is_some(), "the row is revoked, not just the cookie dropped");
    // one row says so: whose token, never the session's id
    let rows = audited(&web.db, "session_ended").await;
    assert_eq!(rows, [json!({ "token": public_id(&web.anna) })]);
    let session_id = cookie.split_once('=').unwrap().1;
    let text: String =
        sqlx::query_scalar("SELECT payload_json FROM audit_events WHERE type = 'session_ended'")
            .fetch_one(web.db.pool())
            .await
            .unwrap();
    assert!(!text.contains(session_id), "{text}");
    let question: Option<String> =
        sqlx::query_scalar("SELECT question_id FROM audit_events WHERE type = 'session_ended'")
            .fetch_one(web.db.pool())
            .await
            .unwrap();
    assert_eq!(question, None);
    let still = send(
        &web,
        from(
            Request::builder().uri("/v1/me").header(COOKIE, &other),
            SOURCE,
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(still.status(), StatusCode::OK);

    // with no session there is nothing to end
    let nothing = send(
        &web,
        from(
            from_the_page(Method::DELETE, "/v1/session"),
            SOURCE,
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(nothing.status(), StatusCode::UNAUTHORIZED);
}

/// The sign-out and its `session_ended` row are one transaction: when the row can't be written, the
/// session isn't ended, the request fails, and the cookie still works for a second try.
#[tokio::test]
async fn a_sign_out_that_cant_be_audited_ends_nothing() {
    let web = web().await;
    let cookie = sign_in(&web, &web.anna).await;
    sqlx::query(
        "CREATE TRIGGER no_sign_out BEFORE INSERT ON audit_events WHEN NEW.type = 'session_ended' \
         BEGIN SELECT RAISE(ABORT, 'the disk is full'); END",
    )
    .execute(web.db.pool())
    .await
    .unwrap();
    let sign_out = || {
        from(
            from_the_page(Method::DELETE, "/v1/session").header(COOKIE, &cookie),
            SOURCE,
            Body::empty(),
        )
    };
    let out = send(&web, sign_out()).await;
    assert_eq!(out.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        out.headers().get(SET_COOKIE).is_none(),
        "the cookie isn't cleared for a session that goes on"
    );
    let me = from(
        Request::builder().uri("/v1/me").header(COOKIE, &cookie),
        SOURCE,
        Body::empty(),
    );
    assert_eq!(send(&web, me).await.status(), StatusCode::OK);
    assert_eq!(
        count(
            &web.db,
            "SELECT count(*) FROM sessions WHERE revoked_at IS NOT NULL"
        )
        .await,
        0
    );

    sqlx::query("DROP TRIGGER no_sign_out")
        .execute(web.db.pool())
        .await
        .unwrap();
    assert_eq!(send(&web, sign_out()).await.status(), StatusCode::NO_CONTENT);
    assert_eq!(audited(&web.db, "session_ended").await.len(), 1);
}

#[tokio::test]
async fn revoking_the_token_or_disabling_the_person_ends_their_sessions_at_once() {
    let web = web().await;
    let (anna, ben) = (sign_in(&web, &web.anna).await, sign_in(&web, &web.ben).await);
    // two sessions of Anna's token
    let anna_again = sign_in(&web, &web.anna).await;
    let me = |cookie: &str| {
        from(
            Request::builder().uri("/v1/me").header(COOKIE, cookie),
            SOURCE,
            Body::empty(),
        )
    };
    for cookie in [&anna, &anna_again, &ben] {
        assert_eq!(send(&web, me(cookie)).await.status(), StatusCode::OK);
    }

    admin::revoke_token(&web.db, public_id(&web.anna), db::now())
        .await
        .unwrap();
    for cookie in [&anna, &anna_again] {
        let response = send(&web, me(cookie)).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "within one request");
        assert_eq!(error_code(&json_of(response).await), "unauthenticated");
    }
    assert_eq!(
        send(&web, me(&ben)).await.status(),
        StatusCode::OK,
        "nobody else's"
    );
    let revoked: i64 = count(
        &web.db,
        "SELECT count(*) FROM sessions WHERE revoked_at IS NOT NULL",
    )
    .await;
    assert_eq!(revoked, 2, "the rows say so, not only the join with the token");
    let rows = audited(&web.db, "token_revoked").await;
    assert_eq!(rows[0]["sessions_ended"], 2);

    admin::disable_user(&web.db, "ben@example.com", db::now())
        .await
        .unwrap();
    assert_eq!(send(&web, me(&ben)).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(audited(&web.db, "user_disabled").await[0]["sessions_ended"], 1);
}

/// The sessions end in the same transaction as the revocation: when the audit row can't be written,
/// the revocation isn't made, and the session still works.
#[tokio::test]
async fn a_revocation_that_cant_be_audited_leaves_the_sessions_alone() {
    let web = web().await;
    let cookie = sign_in(&web, &web.anna).await;
    sqlx::query(
        "CREATE TRIGGER no_revoke BEFORE INSERT ON audit_events WHEN NEW.type = 'token_revoked' \
         BEGIN SELECT RAISE(ABORT, 'the disk is full'); END",
    )
    .execute(web.db.pool())
    .await
    .unwrap();
    assert!(
        admin::revoke_token(&web.db, public_id(&web.anna), db::now())
            .await
            .is_err()
    );
    let response = send(
        &web,
        from(
            Request::builder().uri("/v1/me").header(COOKIE, &cookie),
            SOURCE,
            Body::empty(),
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        count(
            &web.db,
            "SELECT count(*) FROM sessions WHERE revoked_at IS NOT NULL"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn an_expired_session_is_refused_and_the_daily_purge_deletes_it() {
    let web = web().await;
    let cookie = sign_in(&web, &web.anna).await;
    let live = sign_in(&web, &web.ben).await;
    sqlx::query("UPDATE sessions SET expires_at = '2026-01-01T00:00:00.000Z' WHERE token_public_id = ?")
        .bind(public_id(&web.anna))
        .execute(web.db.pool())
        .await
        .unwrap();
    let me = |cookie: &str| {
        from(
            Request::builder().uri("/v1/me").header(COOKIE, cookie),
            SOURCE,
            Body::empty(),
        )
    };
    assert_eq!(send(&web, me(&cookie)).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(send(&web, me(&live)).await.status(), StatusCode::OK);

    assert_eq!(session::purge_expired(&web.db, db::now()).await.unwrap(), 1);
    assert_eq!(count(&web.db, "SELECT count(*) FROM sessions").await, 1);
    assert_eq!(session::purge_expired(&web.db, db::now()).await.unwrap(), 0);
    assert_eq!(send(&web, me(&live)).await.status(), StatusCode::OK);
}

// ------------------------------------------------------------------------------------ the sign-in limit

#[tokio::test]
async fn sign_in_is_limited_per_source_address_and_the_flood_is_audited_once() {
    let web = web().await;
    let attempt = |source: &str| sign_in_request("mcy_nothing", source);
    // a burst of three, then 429 with Retry-After
    for n in 1..=3 {
        let response = send(&web, attempt("203.0.113.9:1111")).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "attempt {n}");
    }
    let limited = send(&web, attempt("203.0.113.9:2222")).await;
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    let wait: u64 = limited.headers()[RETRY_AFTER].to_str().unwrap().parse().unwrap();
    assert!((1..=6).contains(&wait), "one every six seconds: {wait}");
    assert_eq!(error_code(&json_of(limited).await), "rate_limited");
    // the port is not part of the address, and a good token is limited too, so the limit can't be
    // used to find out whether a token is good
    for _ in 0..10 {
        let response = send(&web, sign_in_request(&web.anna, "203.0.113.9:3333")).await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }
    // another address is not affected
    let other = send(&web, sign_in_request(&web.anna, "203.0.113.10:1111")).await;
    assert_eq!(other.status(), StatusCode::NO_CONTENT);

    // the audit has the three attempts, one refusal for the whole flood and the sign-in
    let rows = audited(&web.db, "session_signin").await;
    let outcomes: Vec<&str> = rows.iter().map(|row| row["outcome"].as_str().unwrap()).collect();
    assert_eq!(
        outcomes,
        [
            "unauthenticated",
            "unauthenticated",
            "unauthenticated",
            "rate_limited",
            "created"
        ]
    );
    assert_eq!(rows[3]["source"], "203.0.113.9");
    assert_eq!(
        count(&web.db, "SELECT count(*) FROM sessions").await,
        1,
        "the limited ones made none"
    );
}

#[tokio::test]
async fn an_ipv6_source_counts_as_its_64() {
    let web = web().await;
    let attempt = |source: &str| sign_in_request("mcy_nothing", source);
    for host in ["1", "2", "3"] {
        let response = send(&web, attempt(&format!("[2001:db8:1:2::{host}]:1111"))).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let same_network = send(&web, attempt("[2001:db8:1:2:ffff::9]:1111")).await;
    assert_eq!(same_network.status(), StatusCode::TOO_MANY_REQUESTS);
    let other = send(&web, attempt("[2001:db8:1:3::1]:1111")).await;
    assert_eq!(other.status(), StatusCode::UNAUTHORIZED);
    let rows = audited(&web.db, "session_signin").await;
    assert_eq!(rows[0]["source"], "2001:db8:1:2::/64");
}

#[tokio::test]
async fn the_address_a_proxy_forwards_counts_only_from_a_trusted_proxy() {
    let config = SessionConfig {
        trusted_proxies: AllowList::parse(Some("172.30.9.0/24")).unwrap(),
        ..settings()
    };
    let web = web_with(config).await;
    let via = |peer: &str, forwarded: &[&str]| {
        let mut builder = from_the_page(Method::POST, "/v1/session").header(CONTENT_TYPE, "application/json");
        for value in forwarded {
            builder = builder.header("x-forwarded-for", *value);
        }
        from(
            builder,
            peer,
            Body::from(json!({ "token": "mcy_nothing" }).to_string()),
        )
    };
    let source_of_last = |web: &Web| {
        let db = web.db.clone();
        async move {
            audited(&db, "session_signin").await.last().unwrap()["source"]
                .as_str()
                .unwrap()
                .to_owned()
        }
    };

    // Caddy, on the edge network, forwards the visitor's address
    send(&web, via("172.30.9.2:5555", &["198.51.100.7"])).await;
    assert_eq!(source_of_last(&web).await, "198.51.100.7");
    // several hops: the walk goes from the right past every trusted proxy
    send(
        &web,
        via("172.30.9.2:5555", &["203.0.113.50, 198.51.100.8", "172.30.9.77"]),
    )
    .await;
    assert_eq!(source_of_last(&web).await, "198.51.100.8");
    // what a visitor put in front of it is never reached
    send(&web, via("172.30.9.2:5555", &["10.0.0.1, 198.51.100.9"])).await;
    assert_eq!(source_of_last(&web).await, "198.51.100.9");
    // an entry that isn't an address ends the walk: the proxy's own address counts
    send(
        &web,
        via("172.30.9.2:5555", &["198.51.100.10, not-an-address, 172.30.9.4"]),
    )
    .await;
    assert_eq!(source_of_last(&web).await, "172.30.9.2");
    // no header at all: the proxy's own address
    send(&web, via("172.30.9.2:5555", &[])).await;
    assert_eq!(source_of_last(&web).await, "172.30.9.2");
    // another peer's header means nothing: it can't pick its own bucket
    send(&web, via("203.0.113.9:5555", &["198.51.100.99"])).await;
    assert_eq!(source_of_last(&web).await, "203.0.113.9");
}

#[tokio::test]
async fn without_trusted_proxies_the_forwarded_header_is_ignored() {
    let web = web().await;
    let request = from(
        from_the_page(Method::POST, "/v1/session")
            .header(CONTENT_TYPE, "application/json")
            .header("x-forwarded-for", "198.51.100.7"),
        "172.30.9.2:5555",
        Body::from(json!({ "token": "mcy_nothing" }).to_string()),
    );
    send(&web, request).await;
    assert_eq!(
        audited(&web.db, "session_signin").await[0]["source"],
        "172.30.9.2"
    );
}

// ------------------------------------------------------------------------------------ the settings

#[test]
fn the_settings_are_read_once_and_a_wrong_one_stops_the_core() {
    let config = SessionConfig::parse(
        Some(" https://Mercury.Example.com/ "),
        Some("7d"),
        Some("172.30.9.0/24, 100.64.0.0/10"),
    )
    .unwrap();
    assert_eq!(
        config.public_origin.as_deref(),
        Some("https://mercury.example.com")
    );
    assert_eq!(config.ttl, TimeDelta::days(7));
    assert!(config.trusted_proxies.allows("100.100.1.2".parse().unwrap()));

    // the origin as a browser writes it: no default port, no path
    for (text, origin) in [
        ("http://localhost:8080", "http://localhost:8080"),
        ("https://mercury.example.com:443", "https://mercury.example.com"),
        ("http://127.0.0.1:18080/", "http://127.0.0.1:18080"),
    ] {
        let config = SessionConfig::parse(Some(text), None, None).unwrap();
        assert_eq!(config.public_origin.as_deref(), Some(origin), "{text}");
    }
    let defaults = SessionConfig::parse(None, Some(" "), Some("")).unwrap();
    assert_eq!(defaults.public_origin, None);
    assert_eq!(defaults.ttl, TimeDelta::hours(12));
    assert!(defaults.trusted_proxies.is_empty());

    for wrong in [
        "mercury.example.com",
        "https://mercury.example.com/app",
        "ftp://mercury.example.com",
        "https://u:p@example.com",
        "https://mercury.example.com?x=1",
        "https://",
    ] {
        let err = SessionConfig::parse(Some(wrong), None, None).unwrap_err();
        assert!(err.starts_with("MERCURY_PUBLIC_ORIGIN"), "{wrong}: {err}");
    }
    assert!(
        SessionConfig::parse(None, Some("soon"), None)
            .unwrap_err()
            .starts_with("MERCURY_SESSION_TTL")
    );
    assert!(SessionConfig::parse(None, Some("0h"), None).is_err());
    assert!(
        SessionConfig::parse(None, None, Some("edge"))
            .unwrap_err()
            .starts_with("MERCURY_TRUSTED_PROXY_CIDRS")
    );
}
