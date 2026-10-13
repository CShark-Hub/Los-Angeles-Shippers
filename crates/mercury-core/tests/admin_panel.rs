//! The admin panel's reads, `GET /v1/admin/*`: an admin's alone, refused for every other role before
//! anything is read; a parameter a route doesn't take refused; each read one audit row, written
//! before the body is sent; and bodies of counts and metadata only, never a token's secret, a
//! question's text, an answer or a draft. Everything goes through the public router.

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
use axum::http::header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, COOKIE, ORIGIN, SET_COOKIE};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use chrono::{DateTime, TimeDelta, Utc};
use http_body_util::BodyExt;
use mercury_core::audit;
use mercury_core::config::Config;
use mercury_core::db::{self, Db};
use mercury_core::policy::EffectivePolicy;
use mercury_core::questions::{NotBuilt, Questions};
use mercury_core::session::SessionConfig;
use mercury_core::{admin, app};
use mercury_schema::admin::{AdminOverview, AdminPeople, AdminRepositories, GrantClass, TokenState};
use mercury_schema::api::{ProbeStatus, SignoffReason};
use mercury_schema::events::{Event, PendingSignoff};
use mercury_schema::ids::ApprovalId;
use mercury_schema::{
    AccessMode, Channel, ProbeManifest, ProjectId, QuestionClass, QuestionId, Role, ToolName,
};
use serde_json::{Value, json};
use tower::ServiceExt;

const ROUTES: [&str; 3] = ["/v1/admin/overview", "/v1/admin/people", "/v1/admin/repositories"];

/// The web client's own origin, as the server is set up with it.
const ORIGIN_OF_THE_PAGE: &str = "https://mercury.example.com";

/// Texts the database holds that no body of the panel may carry.
const QUESTION_TEXT: &str = "which invoices did the client export break this week?";
const ANSWER_TEXT: &str = "Three invoices failed in the export module on Tuesday.";
const DRAFT_TEXT: &str = "A held draft about the export module and its failures.";

const Q_DELIVERED: &str = "q_01JB2M3N4P5Q6R7S8T9V0W1X2A";
const Q_HELD: &str = "q_01JB2M3N4P5Q6R7S8T9V0W1X2B";

// ------------------------------------------------------------------------------------ the setup

/// A PM on delta and gamma with three tokens (one expired, one revoked), a tech lead who approves
/// delta, a board member who approves `portfolio`, a disabled PM on delta, and an admin. Delta's
/// probe has registered; `projects.yaml` also lists omega, whose probe hasn't; and a probe has
/// registered under zeta, which the file doesn't list, and is quarantined.
struct Core {
    _dir: tempfile::TempDir,
    db: Db,
    app: Router,
    config: Config,
    manifest: ProbeManifest,
    pm: Person,
    lead: Person,
    board: Person,
    admin: Person,
    /// The PM's other two tokens' texts: expired, then revoked.
    others: Vec<String>,
}

struct Person {
    id: String,
    token: String,
    public_id: String,
}

async fn person(
    db: &Db,
    email: &str,
    role: Role,
    projects: &[&str],
    approver: bool,
    now: DateTime<Utc>,
) -> Person {
    let id = admin::add_user(db, email, "Pat Example", role, now)
        .await
        .unwrap();
    if !projects.is_empty() {
        let projects: Vec<String> = projects.iter().map(|p| (*p).to_owned()).collect();
        let known = admin::KnownProjects::new(["delta", "gamma"]);
        admin::grant(db, email, &projects, approver, &known, now)
            .await
            .unwrap();
    }
    let created = admin::create_token(db, email, "laptop", TimeDelta::days(30), now)
        .await
        .unwrap();
    Person {
        id,
        token: created.token.reveal(),
        public_id: created.token.public_id().to_owned(),
    }
}

/// The examples, with omega listed besides delta.
fn config() -> Config {
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config-examples");
    let (mut config, _) = mercury_core::config::load(&examples).unwrap();
    let mut omega = config.projects.projects.values().next().unwrap().clone();
    omega.display_name = "Omega — mobile app".to_owned();
    omega.access_max = AccessMode::Metadata;
    config
        .projects
        .projects
        .insert(ProjectId::parse("omega").unwrap(), omega);
    config
}

fn fixture_manifest() -> ProbeManifest {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/repo-delta");
    let yaml = std::fs::read_to_string(repo.join(".mercury/probe.yaml")).unwrap();
    serde_norway::from_str(&yaml).unwrap()
}

async fn register(db: &Db, project: &str, manifest: &ProbeManifest, status: &str, reason: Option<&str>) {
    let mut manifest = manifest.clone();
    manifest.project = ProjectId::parse(project).unwrap();
    let now = db::time(db::now());
    sqlx::query(
        "INSERT INTO probes (project, url, access, access_limited_by, status, schema, manifest_hash, manifest_json,
                             tools_json, quarantine_reason, registered_at, last_seen_at)
         VALUES (?, 'http://probe:7000/mcp', 'code', NULL, ?, 'mercury.facts/0.1', ?, ?,
                 '[\"describe\",\"progress\",\"tested\",\"blocked\",\"health\",\"changes\"]', ?, ?, ?)",
    )
    .bind(project)
    .bind(status)
    .bind("0".repeat(64))
    .bind(serde_json::to_string(&manifest).unwrap())
    .bind(reason)
    .bind(&now)
    .bind(&now)
    .execute(db.pool())
    .await
    .unwrap();
}

/// A question of `who` on delta, asked a minute ago, with the texts no body may carry.
async fn question(db: &Db, id: &str, who: &Person, status: &str, judge: Option<&str>) {
    let asked = db::time(db::now() - TimeDelta::minutes(1));
    sqlx::query(
        "INSERT INTO questions (id, user_id, token_public_id, channel, project, class, status, text_redacted,
                                answer_redacted, judge_mode, cost_usd, duration_ms, created_at, finished_at)
         VALUES (?, ?, ?, 'web', 'delta', 'progress', ?, ?, ?, ?, 0.01, 4200, ?, ?)",
    )
    .bind(id)
    .bind(&who.id)
    .bind(&who.public_id)
    .bind(status)
    .bind(QUESTION_TEXT)
    .bind(ANSWER_TEXT)
    .bind(judge)
    .bind(&asked)
    .bind(&asked)
    .execute(db.pool())
    .await
    .unwrap();
}

async fn core() -> Core {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("mercury.db")).await.unwrap();
    let now = db::now();
    // the PM's two other tokens are older than the one `person` gives them
    let pm_id = admin::add_user(&db, "anna@example.com", "Anna Example", Role::Pm, now)
        .await
        .unwrap();
    let known = admin::KnownProjects::new(["delta", "gamma"]);
    admin::grant(
        &db,
        "anna@example.com",
        &["delta".into(), "gamma".into()],
        false,
        &known,
        now,
    )
    .await
    .unwrap();
    let expired = admin::create_token(
        &db,
        "anna@example.com",
        "old phone",
        TimeDelta::hours(1),
        now - TimeDelta::hours(2),
    )
    .await
    .unwrap();
    let revoked = admin::create_token(
        &db,
        "anna@example.com",
        "tablet",
        TimeDelta::days(30),
        now - TimeDelta::hours(1),
    )
    .await
    .unwrap();
    admin::revoke_token(&db, revoked.token.public_id(), now)
        .await
        .unwrap();
    let laptop = admin::create_token(&db, "anna@example.com", "laptop", TimeDelta::days(30), now)
        .await
        .unwrap();
    let pm = Person {
        id: pm_id,
        token: laptop.token.reveal(),
        public_id: laptop.token.public_id().to_owned(),
    };
    let lead = person(&db, "lee@example.com", Role::TechLead, &["delta"], true, now).await;
    // a board person approves nothing: an approver is a pm or a tech lead
    let board = person(&db, "bo@example.com", Role::Board, &["portfolio"], false, now).await;
    let admin = person(&db, "admin@example.com", Role::Admin, &[], false, now).await;
    let _sam = person(&db, "sam@example.com", Role::Pm, &["delta"], false, now).await;
    admin::disable_user(&db, "sam@example.com", now).await.unwrap();

    let manifest = fixture_manifest();
    register(&db, "delta", &manifest, "healthy", None).await;
    register(
        &db,
        "zeta",
        &manifest,
        "quarantined",
        Some("an admin: reads too much"),
    )
    .await;

    question(&db, Q_DELIVERED, &pm, "delivered", Some("jev")).await;
    question(&db, Q_HELD, &pm, "pending_signoff", None).await;
    let held = QuestionId::parse(Q_HELD).unwrap();
    let approval = "ap_01JB2M3N4P5Q6R7S8T9V0W1X2C";
    let event = Event::PendingSignoff(PendingSignoff {
        approval_id: ApprovalId::parse(approval).unwrap(),
        reason: SignoffReason::ReviewBand,
    });
    let _recorded = audit::record(&db, &held, None, event, now).await.unwrap();
    let expires = db::time(now + TimeDelta::days(1));
    sqlx::query(
        "INSERT INTO approvals (id, question_id, reason_json, draft_answer, status, created_at, expires_at)
         VALUES (?, ?, '{\"reason\":\"review_band\",\"flags\":[]}', ?, 'pending', ?, ?)",
    )
    .bind(approval)
    .bind(Q_HELD)
    .bind(json!({ "text": DRAFT_TEXT, "facts": [], "confidence": 0.8 }).to_string())
    .bind(db::time(now))
    .bind(&expires)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query("INSERT INTO spend (day, user_id, spent_usd) VALUES (?, ?, 0.3), (?, '*', 0.3)")
        .bind(now.format("%Y-%m-%d").to_string())
        .bind(&pm.id)
        .bind(now.format("%Y-%m-%d").to_string())
        .execute(db.pool())
        .await
        .unwrap();

    let config = config();
    let sessions = SessionConfig::parse(Some(ORIGIN_OF_THE_PAGE), None, None).unwrap();
    let questions =
        Questions::new(db.clone(), Arc::new(config.clone()), Arc::new(NotBuilt)).with_sessions(sessions);
    Core {
        _dir: dir,
        db,
        app: app(questions),
        config,
        manifest,
        pm,
        lead,
        board,
        admin,
        others: vec![expired.token.reveal(), revoked.token.reveal()],
    }
}

async fn send(core: &Core, request: Request<Body>) -> (StatusCode, HeaderMap, String) {
    let response = core.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, headers, String::from_utf8(body).unwrap())
}

async fn get(core: &Core, uri: &str, token: &str) -> (StatusCode, HeaderMap, String) {
    let request = Request::builder()
        .uri(uri)
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    send(core, request).await
}

/// The admin's read of `uri`, which must answer `200`.
async fn read<T: serde::de::DeserializeOwned>(core: &Core, uri: &str) -> (T, String) {
    let (status, headers, body) = get(core, uri, &core.admin.token).await;
    assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    assert_eq!(headers[CACHE_CONTROL], "no-store", "{uri}");
    (serde_json::from_str(&body).unwrap(), body)
}

/// The payloads of the `admin_viewed` rows, in order; each has no question and no project.
async fn viewed(db: &Db) -> Vec<Value> {
    let rows: Vec<(Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT question_id, project, payload_json FROM audit_events WHERE type = 'admin_viewed' ORDER BY seq",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    rows.into_iter()
        .map(|(question, project, payload)| {
            assert_eq!((question, project), (None, None));
            serde_json::from_str(&payload.unwrap()).unwrap()
        })
        .collect()
}

fn error_code(body: &str) -> String {
    let body: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    body["error"]["code"].as_str().unwrap_or_default().to_owned()
}

/// The texts no body of the panel may carry: every token's secret, its `mcy_` form, and what the
/// questions, the answers and the drafts said.
fn assert_nothing_secret(core: &Core, body: &str) {
    assert!(!body.contains("mcy_"), "a token in {body}");
    for token in [&core.pm, &core.lead, &core.board, &core.admin]
        .map(|p| p.token.as_str())
        .into_iter()
        .chain(core.others.iter().map(String::as_str))
    {
        // `mcy_<public_id>_<secret>`: the secret is base64url, so it may hold `_` itself
        let secret = token.splitn(3, '_').nth(2).unwrap();
        assert!(!body.contains(secret), "a token's secret in {body}");
    }
    for text in [QUESTION_TEXT, ANSWER_TEXT, DRAFT_TEXT] {
        assert!(!body.contains(text), "{text:?} in {body}");
    }
}

// ------------------------------------------------------------------------------------ who reads

#[tokio::test]
async fn only_an_admin_reads_the_admin_panel_and_each_read_is_one_audit_row() {
    let core = core().await;
    for uri in ROUTES {
        for who in [&core.pm, &core.lead, &core.board] {
            let (status, _, body) = get(&core, uri, &who.token).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{uri}: {body}");
            assert_eq!(error_code(&body), "invalid_request", "{uri}");
            let body: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(
                body.as_object().unwrap().keys().collect::<Vec<_>>(),
                ["error"],
                "the error and no data"
            );
        }
        let (status, _, body) = get(&core, uri, "mcy_nothing").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(error_code(&body), "unauthenticated");
    }
    assert!(
        viewed(&core.db).await.is_empty(),
        "a refused read is no admin_viewed row"
    );

    let (overview, body) = read::<AdminOverview>(&core, "/v1/admin/overview").await;
    assert_nothing_secret(&core, &body);
    assert_eq!(
        serde_json::to_value(overview.window).unwrap(),
        "7d",
        "the default"
    );
    let (_, body) = read::<AdminPeople>(&core, "/v1/admin/people").await;
    assert_nothing_secret(&core, &body);
    let (_, body) = read::<AdminRepositories>(&core, "/v1/admin/repositories").await;
    assert_nothing_secret(&core, &body);

    let token = core.admin.public_id.as_str();
    assert_eq!(
        viewed(&core.db).await,
        [
            json!({ "section": "overview", "window": "7d", "token": token }),
            json!({ "section": "people", "token": token }),
            json!({ "section": "repositories", "token": token }),
        ],
        "one row a read, without a question, naming the section and the token that read"
    );
    assert_eq!(audit::verify(&core.db).await.unwrap().broken, None);
}

/// A row the admin's read can't read fails that read with `internal`, never a guess. Every other
/// role gets its `403` all the same, so it was refused before any row was read.
#[tokio::test]
async fn another_role_is_refused_before_a_row_is_read() {
    let core = core().await;
    sqlx::query("UPDATE questions SET channel = 'fax'")
        .execute(core.db.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE probes SET manifest_json = '{' WHERE project = 'delta'")
        .execute(core.db.pool())
        .await
        .unwrap();
    for uri in ROUTES {
        let (status, _, body) = get(&core, uri, &core.admin.token).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{uri}: {body}");
        assert_eq!(error_code(&body), "internal");
        for who in [&core.pm, &core.lead, &core.board] {
            let (status, _, body) = get(&core, uri, &who.token).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{uri}: {body}");
        }
    }
    assert!(
        viewed(&core.db).await.is_empty(),
        "nothing was sent, so no read is audited"
    );
}

#[tokio::test]
async fn a_parameter_a_route_doesnt_take_or_another_window_is_refused() {
    let core = core().await;
    for uri in [
        "/v1/admin/overview?window=1y",
        "/v1/admin/overview?window=",
        "/v1/admin/overview?window=7D",
        "/v1/admin/overview?window=7d&window=30d",
        "/v1/admin/overview?since=2026-10-01",
        "/v1/admin/overview?window=7d&project=delta",
        "/v1/admin/people?window=7d",
        "/v1/admin/people?email=anna@example.com",
        "/v1/admin/repositories?project=delta",
    ] {
        let (status, _, body) = get(&core, uri, &core.admin.token).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {body}");
        assert_eq!(error_code(&body), "invalid_request", "{uri}");
        assert!(
            !body.contains("1y") && !body.contains("anna"),
            "never echoes a value: {body}"
        );
        // the role is checked first
        let (status, _, _) = get(&core, uri, &core.pm.token).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
    }
    assert!(viewed(&core.db).await.is_empty());

    for (window, bucket, buckets) in [("24h", "hour", 24), ("7d", "day", 7), ("30d", "day", 30)] {
        let (overview, body) =
            read::<AdminOverview>(&core, &format!("/v1/admin/overview?window={window}")).await;
        let raw: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            (raw["window"].as_str(), raw["bucket"].as_str()),
            (Some(window), Some(bucket))
        );
        assert_eq!(overview.series.len(), buckets, "{window}");
        assert_eq!(overview.series[0].start, overview.since, "{window}");
        assert!(overview.until >= overview.series[buckets - 1].start, "{window}");
    }
    let windows: Vec<Value> = viewed(&core.db)
        .await
        .into_iter()
        .map(|row| row["window"].clone())
        .collect();
    assert_eq!(windows, [json!("24h"), json!("7d"), json!("30d")]);
}

#[tokio::test]
async fn the_web_clients_session_reads_the_panel_for_an_admin_only() {
    let core = core().await;
    let sign_in = |token: &str| {
        let mut request = Request::builder()
            .method(Method::POST)
            .uri("/v1/session")
            .header(ORIGIN, ORIGIN_OF_THE_PAGE)
            .header("x-mercury-client", "web")
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(json!({ "token": token }).to_string()))
            .unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo("203.0.113.9:40000".parse::<SocketAddr>().unwrap()));
        request
    };
    let cookie_of = |headers: &HeaderMap| -> String {
        let set = headers[SET_COOKIE].to_str().unwrap();
        set.split(';').next().unwrap().to_owned()
    };
    let (status, headers, _) = send(&core, sign_in(&core.admin.token)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let admin = cookie_of(&headers);
    let (status, headers, _) = send(&core, sign_in(&core.pm.token)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let pm = cookie_of(&headers);

    // a read needs no Origin: it changes nothing
    let with = |cookie: &str, uri: &str| {
        Request::builder()
            .uri(uri)
            .header(COOKIE, cookie)
            .body(Body::empty())
            .unwrap()
    };
    for uri in ROUTES {
        let (status, _, body) = send(&core, with(&admin, uri)).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        let (status, _, _) = send(&core, with(&pm, uri)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
    }
    let rows = viewed(&core.db).await;
    assert_eq!(rows.len(), 3);
    assert!(
        rows.iter()
            .all(|row| row["token"] == core.admin.public_id.as_str())
    );
}

#[tokio::test]
async fn the_panels_routes_are_in_the_contract_and_change_nothing() {
    let core = core().await;
    let request = Request::builder()
        .uri("/v1/openapi.json")
        .body(Body::empty())
        .unwrap();
    let (status, _, document) = send(&core, request).await;
    assert_eq!(status, StatusCode::OK);
    let document: Value = serde_json::from_str(&document).unwrap();
    for uri in ROUTES {
        let operations = document["paths"][uri].as_object().unwrap();
        assert_eq!(operations.keys().collect::<Vec<_>>(), ["get"], "{uri}");
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            let request = Request::builder()
                .method(method.clone())
                .uri(uri)
                .header(AUTHORIZATION, format!("Bearer {}", core.admin.token))
                .header(CONTENT_TYPE, "application/json")
                .body(Body::from("{}"))
                .unwrap();
            let (status, _, _) = send(&core, request).await;
            assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method} {uri}");
        }
    }
    assert!(viewed(&core.db).await.is_empty());
}

// ------------------------------------------------------------------------------------ what it shows

#[tokio::test]
async fn the_overview_counts_the_windows_questions_without_their_text() {
    let core = core().await;
    // a held answer's duration runs to the approver's decision: it is no measure of the core
    sqlx::query("UPDATE questions SET duration_ms = 86000000 WHERE id = ?")
        .bind(Q_HELD)
        .execute(core.db.pool())
        .await
        .unwrap();
    let (overview, body) = read::<AdminOverview>(&core, "/v1/admin/overview?window=24h").await;
    assert_nothing_secret(&core, &body);
    assert_eq!((overview.totals.answered, overview.totals.held), (1, 1), "{body}");
    let channels: Vec<Channel> = overview.channels.iter().map(|c| c.channel).collect();
    assert_eq!(channels, [Channel::Cli, Channel::Web, Channel::Mcp]);
    assert_eq!(overview.channels[1].counts.answered, 1);
    assert_eq!(overview.projects.len(), 1);
    assert_eq!(
        overview.projects[0].project.as_ref().map(ProjectId::as_str),
        Some("delta")
    );
    assert_eq!(overview.classes[0].class, QuestionClass::Progress);
    assert_eq!(
        (overview.duration_ms.p50, overview.duration_ms.max),
        (Some(4_200), Some(4_200)),
        "the delivered one, not the held one"
    );
    assert_eq!(overview.askers, 1);
    assert_eq!(overview.signoffs.pending, 1);
    assert_eq!(overview.spend.everyone_usd.get(), 0.3);
    assert_eq!(overview.spend.people.len(), 1);
    assert_eq!(
        overview.spend.people[0]
            .email
            .as_ref()
            .map(|e| e.as_str().to_owned()),
        Some("anna@example.com".to_owned())
    );
    // zeta is quarantined, so it is no silent probe; delta was seen just now; omega's probe never
    // registered, but the audit began just now, and a fresh stack's probes still have time
    assert!(overview.alerts.is_empty(), "{:?}", overview.alerts);
    assert!(overview.settings.models.is_some() && overview.settings.checks.is_some());
}

#[tokio::test]
async fn people_show_their_grants_what_each_lets_them_ask_and_their_tokens() {
    let core = core().await;
    let (people, body) = read::<AdminPeople>(&core, "/v1/admin/people").await;
    assert_nothing_secret(&core, &body);
    let emails: Vec<&str> = people.people.iter().map(|p| p.email.as_str()).collect();
    assert_eq!(
        emails,
        [
            "admin@example.com",
            "anna@example.com",
            "bo@example.com",
            "lee@example.com",
            "sam@example.com"
        ],
        "everyone, the disabled one too, by e-mail address"
    );
    let projects: Vec<&str> = people.projects.iter().map(ProjectId::as_str).collect();
    assert_eq!(projects, ["delta", "omega"], "what projects.yaml lists");
    assert!(people.classes_known);

    let anna = &people.people[1];
    assert_eq!((anna.role, anna.disabled_at), (Role::Pm, None));
    // delta as a question there is judged: the PM's classes, the manifest and both deny lists
    let expected: Vec<GrantClass> =
        EffectivePolicy::for_project(&core.config.policies, Role::Pm, &core.manifest)
            .allowed
            .into_iter()
            .map(|class| GrantClass {
                class,
                signoff: false,
            })
            .collect();
    assert!(!expected.is_empty());
    let grants: Vec<(&str, bool, Option<&Vec<GrantClass>>)> = anna
        .grants
        .iter()
        .map(|g| (g.project.as_str(), g.approver, g.classes.as_ref()))
        .collect();
    assert_eq!(
        grants,
        [("delta", false, Some(&expected)), ("gamma", false, None)],
        "gamma has no manifest, from a probe or projects.yaml: it can't be asked"
    );
    // the tokens oldest first, each with its state, never a secret
    let tokens: Vec<(&str, TokenState)> = anna.tokens.iter().map(|t| (t.label.as_str(), t.state)).collect();
    assert_eq!(
        tokens,
        [
            ("old phone", TokenState::Expired),
            ("tablet", TokenState::Revoked),
            ("laptop", TokenState::Active)
        ]
    );
    assert!(anna.tokens[1].revoked_at.is_some() && anna.tokens[2].revoked_at.is_none());
    assert_eq!(anna.tokens[2].public_id.as_str(), core.pm.public_id);
    assert_eq!(anna.questions_30d, 2);
    assert!(anna.last_asked_at.is_some());
    assert_eq!(anna.spent_today_usd.get(), 0.3);

    let bo = &people.people[2];
    let portfolio = bo.grants[0].classes.as_ref().unwrap();
    assert_eq!(bo.grants[0].project.as_str(), "portfolio");
    assert!(!bo.grants[0].approver, "a board person approves nothing");
    assert!(
        portfolio.contains(&GrantClass {
            class: QuestionClass::Portfolio,
            signoff: true
        }),
        "the board's portfolio answers wait for a sign-off: {portfolio:?}"
    );
    let lee = &people.people[3];
    assert!(lee.grants[0].approver);
    assert_eq!(lee.questions_30d, 0);
    assert_eq!(lee.last_asked_at, None);
    let sam = &people.people[4];
    assert!(sam.disabled_at.is_some());
    assert_eq!(sam.tokens[0].state, TokenState::Revoked, "disabling revoked it");
    assert!(people.people[0].grants.is_empty(), "the admin asks nothing");
}

#[tokio::test]
async fn repositories_show_each_listed_or_registered_project_with_its_probe_and_people() {
    let core = core().await;
    let (repositories, body) = read::<AdminRepositories>(&core, "/v1/admin/repositories").await;
    assert_nothing_secret(&core, &body);
    let ids: Vec<&str> = repositories
        .repositories
        .iter()
        .map(|r| r.project.as_str())
        .collect();
    assert_eq!(
        ids,
        ["delta", "omega", "zeta"],
        "what projects.yaml lists and what registered; gamma is neither"
    );

    let delta = &repositories.repositories[0];
    assert_eq!(delta.display_name.as_deref(), Some("Delta — invoicing"));
    assert_eq!(delta.access_max, Some(AccessMode::Code));
    let probe = delta.probe.as_ref().unwrap();
    assert_eq!(probe.status, ProbeStatus::Healthy);
    assert_eq!((probe.access, probe.access_limited_by), (AccessMode::Code, None));
    let modules: Vec<&str> = probe.modules.iter().map(|m| m.as_str()).collect();
    assert_eq!(modules, ["Invoicing", "Export", "Auth"]);
    assert_eq!(probe.tools.len(), 6);
    assert!(probe.tools.contains(&ToolName::Changes));
    assert_eq!(probe.quarantine_reason, None);
    let people: Vec<(&str, Role, bool, bool)> = delta
        .people
        .iter()
        .map(|p| (p.email.as_str(), p.role, p.approver, p.disabled_at.is_some()))
        .collect();
    assert_eq!(
        people,
        [
            ("anna@example.com", Role::Pm, false, false),
            ("lee@example.com", Role::TechLead, true, false),
            ("sam@example.com", Role::Pm, false, true)
        ]
    );
    assert_eq!(delta.questions_30d, 2);
    assert!(delta.last_asked_at.is_some());

    let omega = &repositories.repositories[1];
    assert_eq!(omega.display_name.as_deref(), Some("Omega — mobile app"));
    assert_eq!(omega.access_max, Some(AccessMode::Metadata));
    assert!(omega.probe.is_none(), "its probe hasn't registered");
    assert!(omega.people.is_empty());
    assert_eq!((omega.questions_30d, omega.last_asked_at), (0, None));

    let zeta = &repositories.repositories[2];
    assert_eq!((zeta.display_name.as_deref(), zeta.access_max), (None, None));
    let probe = zeta.probe.as_ref().unwrap();
    assert_eq!(probe.status, ProbeStatus::Quarantined);
    assert_eq!(
        probe.quarantine_reason.as_deref(),
        Some("an admin: reads too much")
    );
}
