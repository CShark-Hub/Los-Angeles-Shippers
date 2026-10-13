//! A full disk, then the space back, without a restart: the write that found the disk full fails
//! and leaves no row, and once there is room again every write through the same database handle
//! succeeds, so questions are asked and answered again.
//!
//! The disk fills the way SQLite sees it: `PRAGMA max_page_count` set to the pages the file has,
//! so a write that needs one more page fails with `SQLITE_FULL` ("database or disk is full"), and
//! SQLite rolls the whole transaction back, as it does when the disk itself is full. The limit
//! belongs to a connection, so every connection of the pool gets it; raising it again is the
//! space coming back.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{Request, StatusCode};
use chrono::TimeDelta;
use http_body_util::BodyExt;
use mercury_core::audit::{self, AuditError, Verified};
use mercury_core::db::{self, Db};
use mercury_core::questions::{Asked, Emitter, Hub, Questions, Runner};
use mercury_core::status::{self, Options};
use mercury_core::{admin, app};
use mercury_schema::api::{Answer, Usd};
use mercury_schema::events::{Classified, ComposerMode, Done, Event, ProjectPicked, ProjectSource};
use mercury_schema::facts::Ratio;
use mercury_schema::{JudgeMode, ProjectId, QuestionClass, QuestionId, Role};
use serde_json::{Value, json};
use sqlx::AssertSqlSafe;
use tower::ServiceExt;

// ------------------------------------------------------------------------------------ the setup

const ANSWER: &str = "Two items in Export are done.";

/// Answers every question at once: its project, its class, the answer, done.
struct Answers;

#[async_trait]
impl Runner for Answers {
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
        let answer = events
            .emit(Event::Answer(Answer {
                text: ANSWER.to_owned(),
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
    token: String,
}

/// The core over a fresh database, as the server opens it, with one person who asks.
async fn core() -> Core {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("mercury.db")).await.unwrap();
    let now = db::now();
    let email = "anna@example.com";
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
        .unwrap()
        .token
        .reveal();
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config-examples");
    let (config, _) = mercury_core::config::load(&examples).unwrap();
    let app = app(Questions::new(db.clone(), Arc::new(config), Arc::new(Answers)));
    Core {
        _dir: dir,
        db,
        app,
        token,
    }
}

/// One question asked through the API and its stream read to the end: the question's id and the
/// names of its events.
async fn asked(core: &Core, key: &str) -> (StatusCode, Option<String>, Vec<String>) {
    let request = Request::builder()
        .method("POST")
        .uri("/v1/questions")
        .header(AUTHORIZATION, format!("Bearer {}", core.token))
        .header(CONTENT_TYPE, "application/json")
        .header("idempotency-key", key)
        .body(Body::from(
            json!({ "text": "What is done in export?", "project": "delta" }).to_string(),
        ))
        .unwrap();
    let response = core.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let Some(id) = body["id"].as_str().map(str::to_owned) else {
        return (status, None, Vec::new());
    };
    let request = Request::builder()
        .method("GET")
        .uri(format!("/v1/questions/{id}/events"))
        .header(AUTHORIZATION, format!("Bearer {}", core.token))
        .body(Body::empty())
        .unwrap();
    let response = core.app.clone().oneshot(request).await.unwrap();
    let bytes = tokio::time::timeout(Duration::from_secs(10), response.into_body().collect())
        .await
        .expect("the stream ends at its terminal event")
        .unwrap()
        .to_bytes();
    let names = std::str::from_utf8(&bytes)
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("event:"))
        .map(|name| name.trim().to_owned())
        .collect();
    (status, Some(id), names)
}

/// Asks a question and checks it was answered: accepted, its events, the answer, done.
async fn delivered(core: &Core, key: &str) -> QuestionId {
    let (status, id, names) = asked(core, key).await;
    assert_eq!(status, StatusCode::ACCEPTED, "the question starts");
    assert_eq!(
        names,
        ["accepted", "project", "classified", "answer", "done"],
        "the question is answered"
    );
    QuestionId::parse(&id.unwrap()).unwrap()
}

async fn pragma(db: &Db, name: &str) -> i64 {
    sqlx::query_scalar(AssertSqlSafe(format!("PRAGMA {name}")))
        .fetch_one(db.pool())
        .await
        .unwrap()
}

/// Waits until every connection of the pool is back in it. A connection goes back by a task of
/// its own once it is let go, and one asked for before that would be a new connection.
async fn settled(db: &Db) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while db.pool().num_idle() < usize::try_from(db.pool().size()).unwrap() {
        assert!(Instant::now() < deadline, "the pool's connections come back");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Sets `PRAGMA max_page_count` on every connection the pool may hand out: on as many as it may
/// open, held all at once, so that none is left out (one in use is waited for) and the pool opens
/// no other until one of them closes.
async fn limit_pages(db: &Db, pages: i64) {
    let mut held = Vec::new();
    for _ in 0..db.pool().options().get_max_connections() {
        held.push(db.pool().acquire().await.unwrap());
    }
    for conn in &mut held {
        sqlx::query(AssertSqlSafe(format!("PRAGMA max_page_count = {pages}")))
            .execute(&mut **conn)
            .await
            .unwrap();
    }
    drop(held);
    settled(db).await;
}

/// The disk is full: no connection may add a page to the file.
async fn fill(db: &Db) {
    let pages = pragma(db, "page_count").await;
    limit_pages(db, pages).await;
}

/// Something to write that needs more pages than the file has free.
async fn filler(db: &Db) -> String {
    let free = pragma(db, "freelist_count").await;
    let page = pragma(db, "page_size").await;
    let bytes = usize::try_from((free + 16) * page).unwrap();
    "x".repeat(bytes)
}

/// The error a full disk gives: SQLite's `SQLITE_FULL`, 13.
fn is_full_disk<T>(result: &Result<T, AuditError>) -> bool {
    match result {
        Err(AuditError::Write(sqlx::Error::Database(error))) => error.code().as_deref() == Some("13"),
        _ => false,
    }
}

/// What a write came to, for a failed check: its error, or that it was written.
fn outcome<T>(result: &Result<T, AuditError>) -> String {
    match result {
        Ok(_) => "written".to_owned(),
        Err(err) => err.to_string(),
    }
}

/// Every connection the pool hands out takes a write: one write after another, each on the
/// connection that has waited longest, so the writes go round all of them, twice.
async fn every_connection_writes(db: &Db, what: &str) {
    for n in 0..2 * db.pool().size() {
        settled(db).await;
        let written = audit::action(db, "token_revoked", None, &json!({ "n": n }), db::now()).await;
        if let Err(err) = written {
            panic!("{what}: write {n} with the space back, the core not restarted: {err}");
        }
    }
}

// ------------------------------------------------------------------------------------ the tests

#[tokio::test]
async fn after_a_full_disk_the_next_writes_and_questions_succeed_without_a_restart() {
    let core = core().await;
    let unlimited = pragma(&core.db, "max_page_count").await;
    let asked_first = delivered(&core, "k-before").await;
    let before = audit::verify(&core.db).await.unwrap();
    assert_eq!(before.broken, None);

    // the disk fills: an admin's row is refused as a full disk refuses it, and nothing is written
    let big = json!({ "filler": filler(&core.db).await });
    fill(&core.db).await;
    let refused = audit::action(&core.db, "token_revoked", None, &big, db::now()).await;
    assert!(
        is_full_disk(&refused),
        "a full disk refuses the row: {}",
        outcome(&refused)
    );
    assert_eq!(
        audit::verify(&core.db).await.unwrap(),
        before,
        "nothing half-written"
    );

    // the space comes back
    limit_pages(&core.db, unlimited).await;
    every_connection_writes(&core.db, "after an admin's row found the disk full").await;
    if let Err(err) = audit::action(&core.db, "token_revoked", None, &big, db::now()).await {
        panic!("the row the full disk refused, with the space back: {err}");
    }

    // a question's own event finds the disk full the same way
    let before = audit::verify(&core.db).await.unwrap();
    let text = filler(&core.db).await;
    fill(&core.db).await;
    let mut events = Emitter::new(core.db.clone(), Hub::default(), asked_first);
    let refused = events
        .emit(Event::Answer(Answer {
            text,
            facts: Vec::new(),
            confidence: Ratio::new(0.86).unwrap(),
        }))
        .await;
    assert!(
        is_full_disk(&refused),
        "a full disk refuses the event: {}",
        outcome(&refused)
    );
    assert_eq!(
        audit::verify(&core.db).await.unwrap(),
        before,
        "nothing half-written"
    );

    // the space comes back: the writes, and questions asked and answered
    limit_pages(&core.db, unlimited).await;
    every_connection_writes(&core.db, "after a question's event found the disk full").await;
    delivered(&core, "k-after").await;
    delivered(&core, "k-again").await;
    let Verified { broken, .. } = audit::verify(&core.db).await.unwrap();
    assert_eq!(broken, None, "the chain holds");
}

#[tokio::test]
async fn status_says_the_disk_is_full_and_shows_nothing_until_the_space_is_back() {
    let core = core().await;
    let unlimited = pragma(&core.db, "max_page_count").await;
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config-examples");
    // a day's spend at 95% of the examples' $2.00, so the check has an alert to say
    let day = mercury_core::orchestrator::budget::day(db::now());
    sqlx::query("INSERT INTO spend (day, user_id, spent_usd) VALUES (?, '*', 1.9)")
        .bind(&day)
        .execute(core.db.pool())
        .await
        .unwrap();

    // the disk fills: small rows go in until one needs a page the file may not have
    fill(&core.db).await;
    let mut written = 0;
    let refused = loop {
        let row = audit::action(
            &core.db,
            "token_revoked",
            None,
            &json!({ "n": written }),
            db::now(),
        )
        .await;
        if row.is_err() {
            break row;
        }
        written += 1;
        assert!(written < 100_000, "the file fills");
    };
    assert!(
        is_full_disk(&refused),
        "a full disk refuses the row: {}",
        outcome(&refused)
    );

    for (what, options) in [
        ("the view", Options::default()),
        (
            "the check",
            Options {
                check: true,
                since: None,
            },
        ),
    ] {
        let mut out = Vec::new();
        let err = status::run(&core.db, &examples, &mut out, db::now(), options)
            .await
            .unwrap_err();
        assert!(
            err.to_string().starts_with("the disk is full"),
            "{what} says the disk is full: {err}"
        );
        assert!(out.is_empty(), "{what} shows nothing it couldn't audit");
    }

    // the space comes back, with no restart: both work, and the check says its alert
    limit_pages(&core.db, unlimited).await;
    let mut out = Vec::new();
    status::run(&core.db, &examples, &mut out, db::now(), Options::default())
        .await
        .unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(
        text.starts_with("Alerts\nspend: 95% of the daily budget"),
        "{text}"
    );
    let mut out = Vec::new();
    let check = Options {
        check: true,
        since: None,
    };
    let failed = status::run(&core.db, &examples, &mut out, db::now(), check)
        .await
        .unwrap_err();
    assert_eq!(failed.to_string(), "1 alert(s) hold");
    assert!(
        String::from_utf8(out)
            .unwrap()
            .starts_with("spend: 95% of the daily budget")
    );
}
