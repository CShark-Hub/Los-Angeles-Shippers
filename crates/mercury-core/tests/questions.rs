//! The questions API: asking with an idempotency key, the event stream and its exact replay after
//! a disconnect, the owner-only routes, who-am-I, the projects, feedback and the served contract.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE, RETRY_AFTER};
use axum::http::{Request, StatusCode};
use chrono::TimeDelta;
use http_body_util::BodyExt;
use mercury_core::db::{self, Db};
use mercury_core::orchestrator::budget;
use mercury_core::questions::{self, Asked, Emitter, Hub, NotBuilt, Questions, Runner};
use mercury_core::{admin, app};
use mercury_schema::api::{Accepted, Answer, ErrorCode, Refusal, RefusalCode, Usd};
use mercury_schema::events::{
    Classified, ComposerMode, Composing, Done, Event, EventError, ProjectPicked, ProjectSource, Refused,
};
use mercury_schema::facts::Ratio;
use mercury_schema::{JudgeMode, ProjectId, QuestionClass, QuestionId, Role};
use serde_json::{Value, json};
use sqlx::AssertSqlSafe;
use tokio::sync::Notify;
use tower::ServiceExt;

// ------------------------------------------------------------------------------------ the setup

/// Writes a scripted question: three events, then waits until it is let go, then the rest.
struct Scripted {
    go_on: Arc<Notify>,
}

#[async_trait]
impl Runner for Scripted {
    async fn run(&self, question: Asked, mut events: Emitter) {
        let project = question
            .project
            .clone()
            .unwrap_or_else(|| ProjectId::parse("delta").unwrap());
        let first = [
            Event::Project(ProjectPicked {
                project,
                p: Ratio::new(1.0).unwrap(),
                source: ProjectSource::Explicit,
            }),
            Event::Classified(Classified {
                class: QuestionClass::Progress,
                p: Ratio::new(0.94).unwrap(),
                checks: Vec::new(),
            }),
        ];
        for event in first {
            let _sent = events.emit(event).await.unwrap();
        }
        self.go_on.notified().await;
        let _sent = events.emit(Event::Composing(Composing {})).await.unwrap();
        let answer = events
            .emit(Event::Answer(Answer {
                text: "Two items in Export are done.".into(),
                facts: Vec::new(),
                confidence: Ratio::new(0.86).unwrap(),
            }))
            .await
            .unwrap();
        let _sent = events
            .emit(Event::Done(Done {
                duration_ms: 1200,
                cost_usd: Usd::new(0.004).unwrap(),
                judge_mode: JudgeMode::Jev,
                composer: ComposerMode::Model,
                audit_id: answer.seq() + 1,
            }))
            .await
            .unwrap();
    }
}

struct Core {
    _dir: tempfile::TempDir,
    db: Db,
    app: Router,
    go_on: Arc<Notify>,
    anna: String,
    ben: String,
}

async fn core_with(runner: Option<Arc<dyn Runner>>) -> Core {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("mercury.db")).await.unwrap();
    let now = db::now();
    let mut tokens = Vec::new();
    for email in ["anna@example.com", "ben@example.com"] {
        admin::add_user(&db, email, "Anna Example", Role::Pm, now)
            .await
            .unwrap();
        admin::grant(
            &db,
            email,
            &["delta".to_owned()],
            false,
            &admin::KnownProjects::new(["delta"]),
            now,
        )
        .await
        .unwrap();
        let token = admin::create_token(&db, email, "laptop", TimeDelta::days(30), now)
            .await
            .unwrap();
        tokens.push(token.token.reveal());
    }
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config-examples");
    let (mut config, _) = mercury_core::config::load(&examples).unwrap();
    // the token limit these tests count on, as the private and strict presets ship it; the examples,
    // the demo's, allow a person trying them far more
    config.policies.limits.questions_per_hour = 30;
    config.policies.limits.burst = 5;
    let go_on = Arc::new(Notify::new());
    let runner = runner.unwrap_or_else(|| Arc::new(Scripted { go_on: go_on.clone() }));
    let app = app(Questions::new(db.clone(), Arc::new(config), runner));
    let ben = tokens.pop().unwrap();
    let anna = tokens.pop().unwrap();
    Core {
        _dir: dir,
        db,
        app,
        go_on,
        anna,
        ben,
    }
}

async fn core() -> Core {
    core_with(None).await
}

fn request(method: &str, uri: &str, token: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(AUTHORIZATION, format!("Bearer {token}"))
}

async fn send(core: &Core, request: Request<Body>) -> (StatusCode, Value) {
    let response = core.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn ask_with(core: &Core, token: &str, key: Option<&str>, body: Value) -> (StatusCode, Value) {
    let mut builder = request("POST", "/v1/questions", token).header(CONTENT_TYPE, "application/json");
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    send(core, builder.body(Body::from(body.to_string())).unwrap()).await
}

async fn ask(core: &Core, key: &str) -> String {
    let (status, body) = ask_with(
        core,
        &core.anna,
        Some(key),
        json!({ "text": "What is done in export?", "project": "delta" }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    body["id"].as_str().unwrap().to_owned()
}

/// One event of the stream: id, name, data.
type Sent = (u64, String, Value);

fn parse(text: &str) -> Vec<Sent> {
    text.split("\n\n")
        .filter_map(|block| {
            let mut id = None;
            let mut name = None;
            let mut data = None;
            for line in block.lines() {
                if let Some(v) = line.strip_prefix("id:") {
                    id = v.trim().parse().ok();
                } else if let Some(v) = line.strip_prefix("event:") {
                    name = Some(v.trim().to_owned());
                } else if let Some(v) = line.strip_prefix("data:") {
                    data = serde_json::from_str(v.trim()).ok();
                }
            }
            Some((id?, name?, data?))
        })
        .collect()
}

fn events_request(token: &str, id: &str, last: Option<u64>) -> Request<Body> {
    let mut builder = request("GET", &format!("/v1/questions/{id}/events"), token);
    if let Some(last) = last {
        builder = builder.header("last-event-id", last.to_string());
    }
    builder.body(Body::empty()).unwrap()
}

/// The whole stream, read to its end.
async fn follow(core: &Core, id: &str, last: Option<u64>) -> Vec<Sent> {
    let response = core
        .app
        .clone()
        .oneshot(events_request(&core.anna, id, last))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-cache");
    assert_eq!(response.headers()["x-accel-buffering"], "no");
    let bytes = tokio::time::timeout(Duration::from_secs(10), response.into_body().collect())
        .await
        .expect("the stream ends at its terminal event")
        .unwrap()
        .to_bytes();
    parse(std::str::from_utf8(&bytes).unwrap())
}

// ------------------------------------------------------------------------------------ the stream

/// A client reads the first events, goes away mid-question, comes back with the last id it got,
/// and receives exactly the rest, once, in order.
#[tokio::test]
async fn after_a_disconnect_last_event_id_replays_the_rest_exactly_once() {
    let core = core().await;
    let id = ask(&core, "k-1").await;

    // the first connection: read until three events have come, then drop it mid-question
    let response = core
        .app
        .clone()
        .oneshot(events_request(&core.anna, &id, None))
        .await
        .unwrap();
    let mut body = response.into_body();
    let mut seen = String::new();
    while parse(&seen).len() < 3 {
        let frame = tokio::time::timeout(Duration::from_secs(10), body.frame())
            .await
            .expect("the first events arrive")
            .unwrap()
            .unwrap();
        if let Ok(data) = frame.into_data() {
            seen.push_str(std::str::from_utf8(&data).unwrap());
        }
    }
    drop(body);
    let first = parse(&seen);
    let names: Vec<&str> = first.iter().map(|(_, n, _)| n.as_str()).collect();
    assert_eq!(names, ["accepted", "project", "classified"]);
    assert_eq!(first[0].2, json!({ "id": id }));

    // the question goes on while nobody listens
    core.go_on.notify_one();
    let last = first.last().unwrap().0;
    let rest = follow(&core, &id, Some(last)).await;
    let names: Vec<&str> = rest.iter().map(|(_, n, _)| n.as_str()).collect();
    assert_eq!(names, ["composing", "answer", "done"]);
    assert!(rest.iter().all(|(seq, _, _)| *seq > last));

    // a full replay is the same events in the same order, and so is a second one
    let all = follow(&core, &id, None).await;
    assert_eq!(all, [first.clone(), rest.clone()].concat());
    assert_eq!(follow(&core, &id, None).await, all);
    // the ids are increasing audit seqs
    assert!(all.windows(2).all(|w| w[0].0 < w[1].0));
    // from the middle, exactly the tail
    assert_eq!(follow(&core, &id, Some(all[1].0)).await, all[2..].to_vec());
}

#[tokio::test]
async fn a_live_stream_sees_each_event_once_as_it_is_written() {
    let core = core().await;
    let id = ask(&core, "k-live").await;
    let reader = {
        let app = core.app.clone();
        let req = events_request(&core.anna, &id, None);
        tokio::spawn(async move {
            let response = app.oneshot(req).await.unwrap();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            parse(std::str::from_utf8(&bytes).unwrap())
        })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    core.go_on.notify_one();
    let live = tokio::time::timeout(Duration::from_secs(10), reader)
        .await
        .unwrap()
        .unwrap();
    let names: Vec<&str> = live.iter().map(|(_, n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        ["accepted", "project", "classified", "composing", "answer", "done"]
    );
}

/// A reconnect after the terminal event, as a browser makes after the stream closes, ends at once
/// instead of waiting on keep-alives for an event that will never come.
#[tokio::test]
async fn a_reconnect_after_the_terminal_event_ends_at_once() {
    let core = core().await;
    let id = ask(&core, "k-after-end").await;
    core.go_on.notify_one();
    let all = follow(&core, &id, None).await;
    let done = all.last().unwrap();
    assert_eq!(done.1, "done");
    let started = std::time::Instant::now();
    assert_eq!(follow(&core, &id, Some(done.0)).await, Vec::<Sent>::new());
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

/// Writes the project, then breaks off: by a panic, or by returning without a terminal event.
struct BreaksOff {
    panics: bool,
}

#[async_trait]
impl Runner for BreaksOff {
    async fn run(&self, _question: Asked, mut events: Emitter) {
        let _sent = events
            .emit(Event::Project(ProjectPicked {
                project: ProjectId::parse("delta").unwrap(),
                p: Ratio::new(1.0).unwrap(),
                source: ProjectSource::Explicit,
            }))
            .await
            .unwrap();
        assert!(!self.panics, "the run breaks off mid-question");
    }
}

async fn state(core: &Core, id: &str) -> Value {
    let (status, body) = send(
        core,
        request("GET", &format!("/v1/questions/{id}"), &core.anna)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    body
}

/// A run that panics, or ends without a terminal event, leaves no stream waiting: the question
/// ends with `error` `internal`, and its state says `failed`.
#[tokio::test]
async fn a_run_that_breaks_off_ends_the_question_with_an_error() {
    for panics in [true, false] {
        let core = core_with(Some(Arc::new(BreaksOff { panics }))).await;
        let id = ask(&core, "k-breaks-off").await;
        let events = follow(&core, &id, None).await;
        let names: Vec<&str> = events.iter().map(|(_, n, _)| n.as_str()).collect();
        assert_eq!(names, ["accepted", "project", "error"], "panics: {panics}");
        assert_eq!(events[2].2["code"], "internal");
        assert_eq!(state(&core, &id).await["status"], "failed");
    }
}

/// Even when the closing `error` can't be written, the question's row says `failed`, and that
/// ends every stream that waits on it.
#[tokio::test]
async fn a_terminal_event_that_cant_be_written_still_ends_the_stream() {
    let core = core_with(Some(Arc::new(BreaksOff { panics: false }))).await;
    sqlx::query(
        "CREATE TRIGGER no_error BEFORE INSERT ON audit_events WHEN NEW.type = 'error' \
         BEGIN SELECT RAISE(ABORT, 'the disk is full'); END",
    )
    .execute(core.db.pool())
    .await
    .unwrap();
    let id = ask(&core, "k-no-error").await;
    let events = follow(&core, &id, None).await;
    let names: Vec<&str> = events.iter().map(|(_, n, _)| n.as_str()).collect();
    assert_eq!(names, ["accepted", "project"]);
    assert_eq!(state(&core, &id).await["status"], "failed");
}

/// A question a previous run of the core left running is ended at start, and its stream ends.
#[tokio::test]
async fn a_question_left_running_is_ended_at_start() {
    let core = core().await;
    let id = ask(&core, "k-left-running").await;
    // the scripted run writes two events, then waits to be let go, as a run cut off by a restart
    // would wait forever
    for _ in 0..200 {
        let written: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE question_id = ?")
            .bind(&id)
            .fetch_one(core.db.pool())
            .await
            .unwrap();
        if written == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // what the run had counted before it was cut off, as the orchestrator keeps it in the row
    sqlx::query("UPDATE questions SET cost_usd = 0.0031, judge_mode = 'chat' WHERE id = ?")
        .bind(&id)
        .execute(core.db.pool())
        .await
        .unwrap();
    assert_eq!(questions::end_orphans(&core.db).await.unwrap(), 1);
    let events = follow(&core, &id, None).await;
    let names: Vec<&str> = events.iter().map(|(_, n, _)| n.as_str()).collect();
    assert_eq!(names, ["accepted", "project", "classified", "error"]);
    assert_eq!(events[3].2["code"], "internal");
    // the ending says what the question cost and which judge answered, as its row had them
    assert_eq!(events[3].2["cost_usd"], 0.0031);
    assert_eq!(events[3].2["judge_mode"], "chat");
    let state = state(&core, &id).await;
    assert_eq!(
        (&state["status"], &state["cost_usd"], &state["judge_mode"]),
        (&json!("failed"), &json!(0.0031), &json!("chat"))
    );
    assert_eq!(questions::end_orphans(&core.db).await.unwrap(), 0);
}

/// An ending other than `done` writes what it says the question cost, and which judge answered,
/// into the question's row in its own transaction: whoever reads the row after the event reads the
/// same, with nothing written after it.
#[tokio::test]
async fn an_ending_writes_its_cost_and_judge_into_the_questions_row_with_its_event() {
    let core = core().await;
    for (key, ending, status) in [
        (
            "k-ends-refused",
            Event::Refused(Refused::new(
                Refusal {
                    code: RefusalCode::AsksForCode,
                    message: "Mercury doesn't show code.".to_owned(),
                    hint: "Ask what is done instead.".to_owned(),
                },
                Usd::new(0.0042).unwrap(),
                Some(JudgeMode::Chat),
            )),
            "refused",
        ),
        (
            "k-ends-error",
            Event::Error(EventError {
                code: ErrorCode::ProbeUnavailable,
                message: "The project's probe can't answer right now.".to_owned(),
                cost_usd: Usd::new(0.0013).unwrap(),
                judge_mode: Some(JudgeMode::Rules),
            }),
            "failed",
        ),
    ] {
        let id = ask(&core, key).await;
        let mut events = Emitter::new(core.db.clone(), Hub::default(), QuestionId::parse(&id).unwrap());
        let _sent = events.emit(ending).await.unwrap();
        let row: (String, f64, Option<String>) =
            sqlx::query_as("SELECT status, cost_usd, judge_mode FROM questions WHERE id = ?")
                .bind(&id)
                .fetch_one(core.db.pool())
                .await
                .unwrap();
        let mode = if status == "refused" { "chat" } else { "rules" };
        let cost = if status == "refused" { 0.0042 } else { 0.0013 };
        assert_eq!(row, (status.to_owned(), cost, Some(mode.to_owned())), "{key}");
    }
}

/// A question cut off before any judge answered ends at start with nothing counted and no judge:
/// never a judge it didn't have.
#[tokio::test]
async fn a_question_left_running_before_any_judge_ends_with_no_judge() {
    let core = core().await;
    let id = ask(&core, "k-left-early").await;
    for _ in 0..200 {
        let written: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE question_id = ?")
            .bind(&id)
            .fetch_one(core.db.pool())
            .await
            .unwrap();
        if written == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(questions::end_orphans(&core.db).await.unwrap(), 1);
    let events = follow(&core, &id, None).await;
    let error = &events.last().unwrap().2;
    assert_eq!(
        (&error["cost_usd"], &error["judge_mode"]),
        (&json!(0.0), &Value::Null)
    );
}

/// When `accepted` can't be written, the question never started: the request fails, and a retry
/// with the same key asks afresh instead of returning a question with no events.
#[tokio::test]
async fn a_failed_accepted_releases_the_key_for_the_retry() {
    let core = core().await;
    sqlx::query(
        "CREATE TRIGGER no_accepted BEFORE INSERT ON audit_events WHEN NEW.type = 'accepted' \
         BEGIN SELECT RAISE(ABORT, 'the disk is full'); END",
    )
    .execute(core.db.pool())
    .await
    .unwrap();
    let body = json!({ "text": "What is done in export?", "project": "delta" });
    let (status, error) = ask_with(&core, &core.anna, Some("k-retry"), body.clone()).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{error}");
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM questions")
        .fetch_one(core.db.pool())
        .await
        .unwrap();
    assert_eq!(left, 0, "a question that never started leaves no row");

    sqlx::query("DROP TRIGGER no_accepted")
        .execute(core.db.pool())
        .await
        .unwrap();
    let (status, accepted) = ask_with(&core, &core.anna, Some("k-retry"), body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let id = accepted["id"].as_str().unwrap().to_owned();
    core.go_on.notify_one();
    let events = follow(&core, &id, None).await;
    assert_eq!(events.first().unwrap().1, "accepted");
    assert_eq!(events.last().unwrap().1, "done");
}

#[tokio::test]
async fn a_malformed_last_event_id_is_refused() {
    let core = core().await;
    let id = ask(&core, "k-bad-id").await;
    let request = request("GET", &format!("/v1/questions/{id}/events"), &core.anna)
        .header("last-event-id", "the end")
        .body(Body::empty())
        .unwrap();
    let (status, body) = send(&core, request).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_request");
}

// ------------------------------------------------------------------------------------ asking

#[tokio::test]
async fn the_same_key_returns_the_same_question_and_another_persons_doesnt_collide() {
    let core = core().await;
    let first = ask(&core, "same-key").await;
    let again = ask(&core, "same-key").await;
    assert_eq!(first, again);
    // a different body with the same key is still the first question
    let (_, body) = ask_with(
        &core,
        &core.anna,
        Some("same-key"),
        json!({ "text": "Something else" }),
    )
    .await;
    assert_eq!(body["id"], first);
    // another person's same key is another question
    let (status, body) = ask_with(
        &core,
        &core.ben,
        Some("same-key"),
        json!({ "text": "What is done?" }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_ne!(body["id"], first);
    // one question row and one `accepted` for the repeated key
    let questions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM questions WHERE user_id = (SELECT id FROM users WHERE email = 'anna@example.com')")
        .fetch_one(core.db.pool())
        .await
        .unwrap();
    assert_eq!(questions, 1);
    let accepted: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_events WHERE question_id = ? AND type = 'accepted'")
            .bind(&first)
            .fetch_one(core.db.pool())
            .await
            .unwrap();
    assert_eq!(accepted, 1);
}

#[tokio::test]
async fn a_question_needs_a_key_text_and_a_valid_body() {
    let text = json!({ "text": "What is done?" });
    let long_key = "k".repeat(129);
    let cases = [
        (None, text.clone()),
        (Some(""), text.clone()),
        (Some("has space"), text.clone()),
        (Some(long_key.as_str()), text.clone()),
        (Some("k-blank"), json!({ "text": "   " })),
        (Some("k-long"), json!({ "text": "x".repeat(2_001) })),
        (Some("k-field"), json!({ "question": "What is done?" })),
        (
            Some("k-clarify"),
            json!({ "text": "Pick delta", "clarify": { "of": "q_01JB2M3N4P5Q6R7S8T9V0W1X2Y", "option": "delta" } }),
        ),
    ];
    // a fresh core per case: refused requests still spend the token's rate limit
    for (key, body) in cases {
        let core = core().await;
        let (status, answer) = ask_with(&core, &core.anna, key, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{key:?} {answer}");
        assert_eq!(answer["error"]["code"], "invalid_request");
    }
    let core = core().await;
    let (status, body) = ask_with(&core, "mcy_nope", Some("k"), json!({ "text": "hi" })).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    // over the limit: five at once, then 429
    for i in 0..5 {
        let (status, _) = ask_with(&core, &core.anna, Some(&format!("k-{i}")), text.clone()).await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }
    let (status, _) = ask_with(&core, &core.anna, Some("k-6"), text).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn the_stored_text_is_redacted() {
    let core = core().await;
    let (status, body) = ask_with(
        &core,
        &core.anna,
        Some("k-redact"),
        json!({ "text": "Is the Acme Corp export done? Mail me at anna@example.com", "project": "delta" }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let stored: String = sqlx::query_scalar("SELECT text_redacted FROM questions WHERE id = ?")
        .bind(body["id"].as_str().unwrap())
        .fetch_one(core.db.pool())
        .await
        .unwrap();
    assert!(
        !stored.contains("Acme") && !stored.contains("anna@example.com"),
        "{stored}"
    );
    assert!(
        stored.contains("the client") && stored.contains("[email]"),
        "{stored}"
    );
}

// ------------------------------------------------------------------------------------ owners

#[tokio::test]
async fn only_the_owner_reads_a_question() {
    let core = core().await;
    let id = ask(&core, "k-own").await;
    let unknown = "q_01JB2M3N4P5Q6R7S8T9V0W1X2Y";
    for (token, qid) in [
        (&core.ben, id.as_str()),
        (&core.anna, unknown),
        (&core.anna, "not-an-id"),
    ] {
        for (method, uri, body) in [
            ("GET", format!("/v1/questions/{qid}"), Body::empty()),
            ("GET", format!("/v1/questions/{qid}/events"), Body::empty()),
            (
                "POST",
                format!("/v1/questions/{qid}/feedback"),
                Body::from(r#"{"helpful":true,"needed_dev":false}"#),
            ),
        ] {
            let request = request(method, &uri, token)
                .header(CONTENT_TYPE, "application/json")
                .body(body)
                .unwrap();
            let (status, body) = send(&core, request).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
            assert_eq!(body["error"]["message"], "There is no such question.");
        }
    }
    // without a token, nothing at all
    let (status, _) = send(
        &core,
        Request::builder()
            .uri(format!("/v1/questions/{id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ------------------------------------------------------------------------------------ state and feedback

#[tokio::test]
async fn the_state_follows_the_events() {
    let core = core().await;
    let id = ask(&core, "k-state").await;
    let get = |core: &Core| {
        let req = request("GET", &format!("/v1/questions/{id}"), &core.anna)
            .body(Body::empty())
            .unwrap();
        let app = core.app.clone();
        async move {
            let response = app.oneshot(req).await.unwrap();
            serde_json::from_slice::<Value>(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap()
        }
    };
    // running: every field present, null when there is nothing to say yet
    let running = get(&core).await;
    assert_eq!(running["status"], "running");
    for field in ["answer", "refusal", "judge_mode", "duration_ms", "audit_id"] {
        assert!(
            running.get(field).is_some_and(Value::is_null),
            "{field}: {running}"
        );
    }
    core.go_on.notify_one();
    let all = follow(&core, &id, None).await;
    let done = get(&core).await;
    assert_eq!(done["status"], "delivered");
    assert_eq!(done["project"], "delta");
    assert_eq!(done["class"], "progress");
    assert_eq!(done["judge_mode"], "jev");
    assert_eq!(done["duration_ms"], 1200);
    assert_eq!(done["answer"]["text"], "Two items in Export are done.");
    assert_eq!(done["audit_id"], all.last().unwrap().2["audit_id"]);
    assert!(done["refusal"].is_null());
    // it parses into the contract's type
    let _: mercury_schema::api::QuestionState = serde_json::from_value(done).unwrap();
}

/// Makes the stored events of `kind` as an older core wrote them: without these keys of their data.
async fn as_an_older_core_wrote_it(core: &Core, id: &str, kind: &str, keys: &[&str]) {
    let paths = keys
        .iter()
        .map(|key| format!("'$.{key}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let changed = sqlx::query(AssertSqlSafe(format!(
        "UPDATE audit_events SET payload_json = json_remove(payload_json, {paths}) \
         WHERE question_id = ? AND type = ?"
    )))
    .bind(id)
    .bind(kind)
    .execute(core.db.pool())
    .await
    .unwrap()
    .rows_affected();
    assert_eq!(changed, 1, "one {kind} row");
}

/// A question stored when `done` didn't name its composer: it opens, and its replay says who wrote
/// the answer from what the question's row holds, so a client reads it as it reads any other.
#[tokio::test]
async fn a_done_written_before_the_composer_was_named_opens_and_replays_with_the_row_s_composer() {
    for (prompt_version, composer) in [
        (Some("composer.v1"), "model"),
        (Some("template"), "template"),
        (None, "model"),
    ] {
        let core = core().await;
        let id = ask(&core, "k-old-done").await;
        core.go_on.notify_one();
        follow(&core, &id, None).await;
        sqlx::query("UPDATE questions SET prompt_version = ? WHERE id = ?")
            .bind(prompt_version)
            .bind(&id)
            .execute(core.db.pool())
            .await
            .unwrap();
        as_an_older_core_wrote_it(&core, &id, "done", &["composer"]).await;

        let opened = state(&core, &id).await;
        assert_eq!(opened["status"], "delivered", "{prompt_version:?}");
        assert!(opened["audit_id"].is_u64(), "{opened}");
        let _: mercury_schema::api::QuestionState = serde_json::from_value(opened).unwrap();

        let replay = follow(&core, &id, None).await;
        let done = &replay.last().unwrap().2;
        assert_eq!(done["composer"], composer, "{prompt_version:?}: {done}");
        let parsed: Done = serde_json::from_value(done.clone()).unwrap();
        assert_eq!(parsed.duration_ms, 1200, "what the row held is kept");
    }
}

/// An `error` stored when endings didn't carry their cost: its replay adds the cost and the judge
/// of the question's row; an ending that has them is sent as it was written.
#[tokio::test]
async fn an_ending_written_before_endings_carried_their_cost_replays_with_the_row_s_cost_and_judge() {
    let core = core_with(Some(Arc::new(BreaksOff { panics: false }))).await;
    let id = ask(&core, "k-old-error").await;
    follow(&core, &id, None).await;
    let current = follow(&core, &id, None).await;
    let written = current.last().unwrap().2.clone();
    assert_eq!(written["code"], "internal");

    sqlx::query("UPDATE questions SET cost_usd = 0.0123, judge_mode = 'chat' WHERE id = ?")
        .bind(&id)
        .execute(core.db.pool())
        .await
        .unwrap();
    // as it is: sent as it was written
    assert_eq!(follow(&core, &id, None).await.last().unwrap().2, written);

    as_an_older_core_wrote_it(&core, &id, "error", &["cost_usd", "judge_mode"]).await;
    let replay = follow(&core, &id, None).await;
    let error = &replay.last().unwrap().2;
    assert_eq!(
        (&error["cost_usd"], &error["judge_mode"]),
        (&json!(0.0123), &json!("chat")),
        "{error}"
    );
    let parsed: mercury_schema::events::EventError = serde_json::from_value(error.clone()).unwrap();
    assert_eq!(parsed.code, ErrorCode::Internal);
    assert_eq!(state(&core, &id).await["status"], "failed");
}

#[tokio::test]
async fn without_the_orchestrator_a_question_ends_with_an_error() {
    let core = core_with(Some(Arc::new(NotBuilt))).await;
    let id = ask(&core, "k-not-built").await;
    let all = follow(&core, &id, None).await;
    let names: Vec<&str> = all.iter().map(|(_, n, _)| n.as_str()).collect();
    assert_eq!(names, ["accepted", "error"]);
    let (_, state) = send(
        &core,
        request("GET", &format!("/v1/questions/{id}"), &core.anna)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(state["status"], "failed");
    let _ = (
        Accepted::new(mercury_schema::QuestionId::parse(&id).unwrap()),
        EventError {
            code: ErrorCode::Internal,
            message: String::new(),
            cost_usd: Usd::default(),
            judge_mode: None,
        },
    );
}

/// An event is never in the audit while the state it leaves isn't in the question's row. The two
/// used to be written one after the other, and a reader between them found `running` after the
/// end. The watcher here looks at both in one snapshot, again and again, while the question runs,
/// so it is in the gap whenever there is one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_event_is_never_in_the_audit_before_the_state_it_leaves() {
    for round in 0..30 {
        let core = core_with(Some(Arc::new(NotBuilt))).await;
        let stop = Arc::new(AtomicBool::new(false));
        let watcher = {
            let (db, stop) = (core.db.clone(), stop.clone());
            tokio::spawn(async move {
                let mut early = 0_i64;
                while !stop.load(Ordering::Relaxed) {
                    early += sqlx::query_scalar::<_, i64>(
                        "SELECT count(*) FROM audit_events a JOIN questions q ON q.id = a.question_id \
                         WHERE a.type = 'error' AND q.status = 'running'",
                    )
                    .fetch_one(db.pool())
                    .await
                    .unwrap();
                    tokio::task::yield_now().await;
                }
                early
            })
        };
        let id = ask(&core, &format!("k-ended-{round}")).await;
        let all = follow(&core, &id, None).await;
        stop.store(true, Ordering::Relaxed);
        assert_eq!(all.last().unwrap().1, "error", "round {round}");
        assert_eq!(
            watcher.await.unwrap(),
            0,
            "round {round}: an `error` was in the audit while its question still ran"
        );
        assert_eq!(state(&core, &id).await["status"], "failed", "round {round}");
    }
}

/// An event and the question's state are written together or not at all: when the question's row
/// can't be brought up to date, the event isn't in the audit either, so no stream sends an event
/// whose state a reader then doesn't find.
#[tokio::test]
async fn an_event_and_the_questions_state_are_written_together_or_not_at_all() {
    let core = core().await;
    let id = ask(&core, "k-together").await;
    let mut events = Emitter::new(core.db.clone(), Hub::default(), QuestionId::parse(&id).unwrap());
    sqlx::query(
        "CREATE TRIGGER no_failed BEFORE UPDATE ON questions WHEN NEW.status = 'failed' \
         BEGIN SELECT RAISE(ABORT, 'the disk is full'); END",
    )
    .execute(core.db.pool())
    .await
    .unwrap();
    let error = || {
        Event::Error(EventError {
            code: ErrorCode::Internal,
            message: "The question failed.".to_owned(),
            cost_usd: Usd::default(),
            judge_mode: None,
        })
    };
    let errors = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM audit_events WHERE question_id = ? AND type = 'error'",
        )
        .bind(&id)
        .fetch_one(core.db.pool())
        .await
        .unwrap()
    };

    assert!(events.emit(error()).await.is_err());
    assert_eq!(errors().await, 0, "no event without its state");
    assert_eq!(state(&core, &id).await["status"], "running");

    sqlx::query("DROP TRIGGER no_failed")
        .execute(core.db.pool())
        .await
        .unwrap();
    let sent = events.emit(error()).await.unwrap();
    assert_eq!(errors().await, 1);
    assert_eq!(state(&core, &id).await["status"], "failed");
    assert!(sent.seq() > 0);
    let verified = mercury_core::audit::verify(&core.db).await.unwrap();
    assert_eq!(
        verified.broken, None,
        "the rolled-back row left no gap in the chain"
    );
}

// ------------------------------------------------------------------------------- the budgets

async fn user_id(core: &Core, email: &str) -> String {
    sqlx::query_scalar("SELECT id FROM users WHERE email = ?")
        .bind(email)
        .fetch_one(core.db.pool())
        .await
        .unwrap()
}

async fn spend(core: &Core, who: &str, usd: f64) {
    sqlx::query("INSERT INTO spend (day, user_id, spent_usd) VALUES (?, ?, ?)")
        .bind(budget::day(db::now()))
        .bind(who)
        .bind(usd)
        .execute(core.db.pool())
        .await
        .unwrap();
}

/// Asks with a fresh key; the status, the body and `Retry-After`.
async fn ask_raw(core: &Core, token: &str, key: &str) -> (StatusCode, Value, Option<u64>) {
    let request = request("POST", "/v1/questions", token)
        .header(CONTENT_TYPE, "application/json")
        .header("idempotency-key", key)
        .body(Body::from(
            json!({ "text": "What is done in export?", "project": "delta" }).to_string(),
        ))
        .unwrap();
    let response = core.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let retry = response
        .headers()
        .get(RETRY_AFTER)
        .map(|v| v.to_str().unwrap().parse().unwrap());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        retry,
    )
}

async fn refusals(core: &Core) -> Vec<(Option<String>, Value)> {
    let rows: Vec<(Option<String>, String)> = sqlx::query_as(
        "SELECT question_id, payload_json FROM audit_events WHERE type = 'budget_exceeded' ORDER BY seq",
    )
    .fetch_all(core.db.pool())
    .await
    .unwrap();
    rows.into_iter()
        .map(|(q, p)| (q, serde_json::from_str(&p).unwrap()))
        .collect()
}

/// A person whose budget for the day is spent can't start a question, and the refusal is audited;
/// someone else still can, and the question already asked with a key is still that question.
#[tokio::test]
async fn over_the_daily_budget_a_person_cant_start_a_question() {
    let core = core().await;
    let earlier = ask(&core, "k-before").await;
    spend(&core, &user_id(&core, "anna@example.com").await, 1.0).await;

    let (status, body, retry) = ask_raw(&core, &core.anna, "k-after").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["error"]["code"], "budget_exceeded");
    assert!(
        body["error"]["hint"].as_str().unwrap().contains("tomorrow"),
        "{body}"
    );
    let retry = retry.unwrap();
    assert!((1..=86_400).contains(&retry), "{retry}");
    let not_asked: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_keys WHERE key = 'k-after'")
        .fetch_one(core.db.pool())
        .await
        .unwrap();
    assert_eq!(not_asked, 0, "no question, no key");

    let audited = refusals(&core).await;
    assert_eq!(audited.len(), 1);
    let (question, payload) = &audited[0];
    assert_eq!(question, &None);
    assert_eq!(payload["cap"], "user");
    assert_eq!(payload["day"], budget::day(db::now()));
    let anna_token: String = sqlx::query_scalar(
        "SELECT t.public_id FROM tokens t JOIN users u ON u.id = t.user_id WHERE u.email = ?",
    )
    .bind("anna@example.com")
    .fetch_one(core.db.pool())
    .await
    .unwrap();
    assert_eq!(payload["token"], anna_token.as_str());
    assert!(!payload.to_string().contains("1.0"), "no amounts: {payload}");

    assert_eq!(
        ask(&core, "k-before").await,
        earlier,
        "the same key is the same question"
    );
    let (status, _, _) = ask_raw(&core, &core.ben, "k-ben").await;
    assert_eq!(status, StatusCode::ACCEPTED, "the cap is per person");
}

/// Everyone's row counts everyone: once it is spent, nobody starts a question.
#[tokio::test]
async fn the_global_budget_stops_everyone() {
    let core = core().await;
    spend(&core, "*", 5.0).await;
    for (token, key) in [(&core.anna, "k-a"), (&core.ben, "k-b")] {
        let (status, body, _) = ask_raw(&core, token, key).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
        assert_eq!(body["error"]["code"], "budget_exceeded");
    }
    let caps: Vec<Value> = refusals(&core)
        .await
        .into_iter()
        .map(|(_, p)| p["cap"].clone())
        .collect();
    assert_eq!(caps, [json!("global"), json!("global")]);
}

/// A budget that can't be read lets no question start.
#[tokio::test]
async fn a_budget_that_cant_be_read_lets_no_question_start() {
    let core = core().await;
    sqlx::query("DROP TABLE spend")
        .execute(core.db.pool())
        .await
        .unwrap();
    let (status, body, _) = ask_raw(&core, &core.anna, "k-x").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body["error"]["code"], "internal");
    let questions: i64 = sqlx::query_scalar("SELECT count(*) FROM questions")
        .fetch_one(core.db.pool())
        .await
        .unwrap();
    assert_eq!(questions, 0);
}

#[tokio::test]
async fn feedback_is_recorded_and_a_second_one_replaces_it() {
    let core = core().await;
    let id = ask(&core, "k-feedback").await;
    for (helpful, needed_dev) in [(true, false), (false, true)] {
        let request = request("POST", &format!("/v1/questions/{id}/feedback"), &core.anna)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "helpful": helpful, "needed_dev": needed_dev }).to_string(),
            ))
            .unwrap();
        let (status, _) = send(&core, request).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    let row: (i64, i64) = sqlx::query_as("SELECT helpful, needed_dev FROM feedback WHERE question_id = ?")
        .bind(&id)
        .fetch_one(core.db.pool())
        .await
        .unwrap();
    assert_eq!(row, (0, 1));
    // each one is an audit row with the answer and the id, never the question text
    let audited: Vec<String> = sqlx::query_scalar(
        "SELECT payload_json FROM audit_events WHERE type = 'feedback' AND question_id IS NULL ORDER BY seq",
    )
    .fetch_all(core.db.pool())
    .await
    .unwrap();
    assert_eq!(audited.len(), 2);
    assert!(
        audited[1].contains(&id) && audited[1].contains("\"needed_dev\":true"),
        "{audited:?}"
    );
    assert!(audited.iter().all(|p| !p.contains("export")));
    let bad = request("POST", &format!("/v1/questions/{id}/feedback"), &core.anna)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"helpful":true}"#))
        .unwrap();
    assert_eq!(send(&core, bad).await.0, StatusCode::BAD_REQUEST);
}

/// The feedback and its audit row are one transaction: a row that can't be written is a `500` and
/// no feedback is kept, so sending it again is safe, and the first one is not replaced by a failure.
#[tokio::test]
async fn feedback_whose_audit_row_cant_be_written_is_not_kept() {
    let core = core().await;
    let id = ask(&core, "k-feedback-trail").await;
    let send_feedback = |helpful: bool, needed_dev: bool| {
        request("POST", &format!("/v1/questions/{id}/feedback"), &core.anna)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "helpful": helpful, "needed_dev": needed_dev }).to_string(),
            ))
            .unwrap()
    };
    let kept = || async {
        sqlx::query_as::<_, (i64, i64)>("SELECT helpful, needed_dev FROM feedback WHERE question_id = ?")
            .bind(&id)
            .fetch_optional(core.db.pool())
            .await
            .unwrap()
    };
    sqlx::query(
        "CREATE TRIGGER no_feedback BEFORE INSERT ON audit_events WHEN NEW.type = 'feedback' \n         BEGIN SELECT RAISE(ABORT, 'the disk is full'); END",
    )
    .execute(core.db.pool())
    .await
    .unwrap();

    // a first feedback: refused, and nothing is kept
    let (status, body) = send(&core, send_feedback(true, false)).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"]["code"], "internal");
    assert_eq!(kept().await, None, "no feedback without its row");

    // the disk is back: the same feedback is kept with its row
    sqlx::query("DROP TRIGGER no_feedback")
        .execute(core.db.pool())
        .await
        .unwrap();
    assert_eq!(
        send(&core, send_feedback(true, false)).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(kept().await, Some((1, 0)));

    // a second one that can't be written leaves the first as it was
    sqlx::query(
        "CREATE TRIGGER no_feedback BEFORE INSERT ON audit_events WHEN NEW.type = 'feedback' \n         BEGIN SELECT RAISE(ABORT, 'the disk is full'); END",
    )
    .execute(core.db.pool())
    .await
    .unwrap();
    assert_eq!(
        send(&core, send_feedback(false, true)).await.0,
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(kept().await, Some((1, 0)), "the first feedback stands");
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE type = 'feedback'")
        .fetch_one(core.db.pool())
        .await
        .unwrap();
    assert_eq!(rows, 1);
}

// ------------------------------------------------------------------------------------ people and projects

#[tokio::test]
async fn me_and_my_projects() {
    let core = core().await;
    let (status, me) = send(
        &core,
        request("GET", "/v1/me", &core.anna).body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(me["user"]["email"], "anna@example.com");
    assert_eq!(me["user"]["role"], "pm");
    assert_eq!(me["projects"], json!([{ "project": "delta", "approver": false }]));
    assert_eq!(me["token"]["public_id"].as_str().unwrap().len(), 12);
    let _: mercury_schema::api::Me = serde_json::from_value(me).unwrap();

    // delta has no manifest copy in the examples and no probe has registered: nothing to show
    let (status, projects) = send(
        &core,
        request("GET", "/v1/projects", &core.anna)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(projects, json!({ "projects": [] }));

    // once the fixture's probe registers, delta shows with its modules, classes and probe
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/repo-delta");
    let yaml = std::fs::read_to_string(repo.join(".mercury/probe.yaml")).unwrap();
    let manifest: mercury_schema::ProbeManifest = serde_norway::from_str(&yaml).unwrap();
    let now = db::time(db::now());
    sqlx::query(
        "INSERT INTO probes (project, url, access, status, schema, manifest_hash, manifest_json, tools_json,
                             registered_at, last_seen_at)
         VALUES ('delta', 'http://probe-delta:7000/mcp', 'code', 'healthy', 'mercury.facts/0.1', ?, ?, '[]', ?, ?)",
    )
    .bind("0".repeat(64))
    .bind(serde_json::to_string(&manifest).unwrap())
    .bind(&now)
    .bind(&now)
    .execute(core.db.pool())
    .await
    .unwrap();
    let (_, projects) = send(
        &core,
        request("GET", "/v1/projects", &core.anna)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    let delta = &projects["projects"][0];
    assert_eq!(delta["project"], "delta");
    assert_eq!(delta["display_name"], "Delta — invoicing");
    assert_eq!(delta["modules"], json!(["Invoicing", "Export", "Auth"]));
    assert_eq!(
        delta["classes"],
        json!(["progress", "tested", "blocked", "health", "changes", "open"])
    );
    assert_eq!(delta["probe"]["status"], "healthy");
    let _: mercury_schema::api::ProjectList = serde_json::from_value(projects).unwrap();
}

// ------------------------------------------------------------------------------------ the contract

#[tokio::test]
async fn the_served_openapi_is_the_committed_one_with_every_built_path() {
    let core = core().await;
    let response = core
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let served =
        String::from_utf8(response.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert_eq!(served, mercury_schema::openapi::document_json());
    let committed =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../openapi.json")).unwrap();
    assert_eq!(served.replace("\r\n", "\n"), committed.replace("\r\n", "\n"));
    let doc: Value = serde_json::from_str(&served).unwrap();
    for (path, method) in [
        ("/v1/health", "get"),
        ("/v1/openapi.json", "get"),
        ("/v1/session", "post"),
        ("/v1/session", "delete"),
        ("/v1/me", "get"),
        ("/v1/projects", "get"),
        ("/v1/questions", "post"),
        ("/v1/questions/{id}", "get"),
        ("/v1/questions/{id}/events", "get"),
        ("/v1/questions/{id}/feedback", "post"),
    ] {
        assert!(doc["paths"][path][method].is_object(), "{method} {path}");
    }
}
